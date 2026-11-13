//! The engine on the scripted vCPU (`hydo_kvm::script`): the protocol, the clock, the PIC and the
//! timer, the interrupt rule, snapshots and fingerprints, with no KVM. The guest program
//! initializes the PIC as Linux does, arms the hydo timer, idles in HLT, counts ticks in its
//! interrupt handler (EOI, re-arm), reads the clock, prints its verdict and halts.

use hydo_engine_proto::{
    Answer, EngineMsg, HostCmd, HostMsg, Phase, Request, RunResult, SendAnswer, StopWhy,
};
use hydo_hvx::port::ChanPort;
use hydo_kvm::layout::*;
use hydo_kvm::script::{Op, Program, Script};
use hydo_kvm::serve::{run, KvmOptions, Mode, Start};
use hydo_schedule::Schedule;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const TIMER_VECTOR: u8 = 0x30;

fn program(ticks: u64) -> Program {
    use Op::*;
    let mut ops = vec![
        Out(0x21, 1, 0xff),
        Out(0xa1, 1, 0xff),
        Out(0x20, 1, 0x11),
        Out(0x21, 1, 0x30),
        Out(0x21, 1, 0x04),
        Out(0x21, 1, 0x01),
        Out(0xa0, 1, 0x11),
        Out(0xa1, 1, 0x38),
        Out(0xa1, 1, 0x02),
        Out(0xa1, 1, 0x01),
        Out(0x21, 1, 0xfe), // IRQ 0 only
        Print(b"boot\n".to_vec()),
        RdMsr(0x1a0), // MISC_ENABLE: the model's value
        Out(PORT_TIMER, 4, 1_000_000),
        Sti,
        Hlt, // 15
        JmpIfCountBelow(ticks, 15),
        InPair(PORT_CLOCK),
        Print(b"HYDO-ORACLE: OK ticks\n".to_vec()),
        Cli,
        Hlt, // halted with interrupts off
    ];
    let handler = ops.len();
    ops.extend([
        Inc,
        Print(b"tick\n".to_vec()),
        Out(0x20, 1, 0x60), // specific EOI, line 0
        Out(PORT_TIMER, 4, 1_000_000),
        Iret,
    ]);
    Program {
        ops,
        handlers: HashMap::from([(TIMER_VECTOR, handler)]),
    }
}

struct Host {
    tx: Sender<HostMsg>,
    rx: Receiver<EngineMsg>,
    join: Option<std::thread::JoinHandle<i32>>,
    log: Vec<String>,
}

impl Host {
    fn start(mode: Mode, ticks: u64) -> Host {
        Host::start_program(mode, program(ticks))
    }

    fn start_program(mode: Mode, prog: Program) -> Host {
        Host::start_with(mode, prog, KvmOptions::default()).0
    }

    /// An engine with options `o`, and its output directory.
    fn start_with(mode: Mode, prog: Program, o: KvmOptions) -> (Host, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "hydo-kvm-engine-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let k = dir.join("bzImage-kvm");
        std::fs::write(&k, hydo_kvm::script::bzimage()).unwrap();
        let i = dir.join("initrd");
        std::fs::write(&i, b"initramfs").unwrap();
        let start = Start {
            mode,
            kernel: k,
            initrd: i,
            disk: None,
            outdir: dir.join("out"),
            ram_mib: Some(256),
            image_epoch: None,
        };
        let (htx, hrx) = channel();
        let (etx, erx) = channel();
        let hv = Box::new(Script {
            program: Arc::new(prog),
        });
        let out = start.outdir.clone();
        let join = std::thread::spawn(move || {
            run(
                start,
                &o,
                Box::new(ChanPort::new(hrx, etx)),
                Box::new(std::io::sink()),
                None,
                hv,
            )
        });
        (
            Host {
                tx: htx,
                rx: erx,
                join: Some(join),
                log: Vec::new(),
            },
            out,
        )
    }

    fn recv(&mut self) -> EngineMsg {
        let m = self
            .rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the engine answers");
        self.log.push(m.line());
        m
    }

    fn cmd(&mut self, c: HostCmd) {
        self.log
            .push(format!("< {}", HostMsg::Cmd(c.clone()).line()));
        self.tx.send(HostMsg::Cmd(c)).unwrap();
    }

    fn answer(&mut self) -> Answer {
        loop {
            if let EngineMsg::Answer(a) = self.recv() {
                return a;
            }
        }
    }

    /// Serve one phase-tagged conversation: at every stop call `at_stop(n, phase, why)` for the
    /// commands, then CONT unless it ended; until a RESULT.
    fn drive(
        &mut self,
        mut at_stop: impl FnMut(&mut Host, u64, Phase, StopWhy) -> bool,
    ) -> RunResult {
        loop {
            match self.recv() {
                EngineMsg::Stop { stop, .. } => {
                    if !at_stop(self, stop.n, stop.phase, stop.why) {
                        self.cmd(HostCmd::Cont);
                    }
                }
                EngineMsg::Result(r) => return *r,
                _ => {}
            }
        }
    }

    fn snap(&mut self, name: &str) {
        self.cmd(HostCmd::Snap {
            name: name.into(),
            retype: false,
        });
        match self.answer() {
            Answer::Snap { name: n, .. } => assert_eq!(n, name),
            a => panic!("SNAP {name}: {a:?}"),
        }
    }

    fn quit(mut self) -> i32 {
        let _ = self.tx.send(HostMsg::Request(Request::Quit));
        self.join.take().unwrap().join().unwrap()
    }
}

fn sched(s: &str) -> Schedule {
    Schedule::parse(s).unwrap()
}

/// The boot conversation: S0 at the start stop, S1 at the third stop, then END.
fn boot(h: &mut Host) -> RunResult {
    h.drive(|h, n, phase, _| {
        assert_eq!(phase, Phase::Boot);
        if n == 1 {
            h.snap("S0");
        }
        if n == 3 {
            h.snap("S1");
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    })
}

/// A run's conversation: CONT at every stop, END at the guest's exit stop.
fn to_the_end(h: &mut Host) -> RunResult {
    h.drive(|h, _, _, why| {
        if why == StopWhy::Exit {
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    })
}

#[test]
fn serve_runs_are_identical_and_equal_a_fresh_boot() {
    let mut h = Host::start(
        Mode::Serve {
            boot: Some(sched("rb=4 | run")),
        },
        5,
    );
    let b = boot(&mut h);
    assert_eq!(b.tag, "boot");
    match h.recv() {
        EngineMsg::Ready(_) => {}
        m => panic!("READY expected: {}", m.line()),
    }
    let mut fps = Vec::new();
    for tag in ["r1", "r2"] {
        h.tx.send(HostMsg::Request(Request::Ctl {
            tag: tag.into(),
            schedule: sched("rb=4 | run"),
        }))
        .unwrap();
        let r = to_the_end(&mut h);
        assert_eq!(r.tag, tag);
        assert_eq!(r.verdict, "HYDO-ORACLE: OK ticks", "{}", h.log.join("\n"));
        fps.push((r.fp.clone(), r.wfp.clone(), r.extras.clone()));
    }
    assert_eq!(fps[0], fps[1], "two runs from S1");
    assert_eq!(h.quit(), 0);

    // the same schedule from a fresh boot: the host's SNAP S1 starts the workload there
    let mut o = Host::start(Mode::One(sched("rb=4 | run")), 5);
    let r = o.drive(|h, n, phase, why| {
        if phase == Phase::Boot && n == 1 {
            h.snap("S0");
        }
        if phase == Phase::Boot && n == 3 {
            h.snap("S1");
        }
        if why == StopWhy::Exit {
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    });
    assert_eq!(r.verdict, "HYDO-ORACLE: OK ticks");
    assert_eq!(
        (r.fp.clone(), r.wfp.clone()),
        (fps[0].0.clone(), fps[0].1.clone()),
        "a fresh boot's run equals a run from S1"
    );
    o.join.take().unwrap().join().unwrap();
}

/// S1 taken at boot stop `k` (S0 at the first), then the schedule: from the worker's S1 in a
/// fresh vCPU (serve, CTL) and from a fresh boot that goes on in the vCPU that took S1 (one).
fn s1_and_fresh_at(k: u64, s: &str) -> (RunResult, RunResult, String) {
    let mut h = Host::start(
        Mode::Serve {
            boot: Some(sched(s)),
        },
        5,
    );
    h.drive(|h, n, _, _| {
        if n == 1 {
            h.snap("S0");
        }
        if n == k {
            h.snap("S1");
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    });
    h.recv(); // READY
    h.tx.send(HostMsg::Request(Request::Ctl {
        tag: "w".into(),
        schedule: sched(s),
    }))
    .unwrap();
    let w = to_the_end(&mut h);
    let wlog = h.log.join("\n");
    h.quit();
    let mut o = Host::start(Mode::One(sched(s)), 5);
    let f = o.drive(|h, n, phase, why| {
        if phase == Phase::Boot && n == 1 {
            h.snap("S0");
        }
        if phase == Phase::Boot && n == k {
            h.snap("S1");
        }
        if why == StopWhy::Exit {
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    });
    o.join.take().unwrap().join().unwrap();
    let logs = format!("from S1:\n{wlog}\nfresh boot:\n{}", o.log.join("\n"));
    (w, f, logs)
}

#[test]
fn a_run_from_s1_equals_a_fresh_boot_wherever_s1_is_taken() {
    // with a stop at every exit, S1 lands on every kind of exit of the boot, among them the HLT
    // whose clock jump makes the timer's interrupt pending at the stop: the run that took S1
    // injects it at once (KVM said "ready" at that exit), and a run restored from S1 must too,
    // not ask for the window first (a fresh vCPU's kvm_run says 0 until its first KVM_RUN)
    for k in 2..=40 {
        let (w, f, logs) = s1_and_fresh_at(k, "rb=1 | run");
        assert_eq!(f.verdict, "HYDO-ORACLE: OK ticks");
        assert_eq!(
            (w.fp, w.wfp, w.extras),
            (f.fp, f.wfp, f.extras),
            "S1 at boot stop {k}: the run from S1 differs from the fresh boot\n{logs}"
        );
    }
}

#[test]
fn snap_and_resume_continue_the_same_run() {
    // a run with a SNAP in the middle, then a RESUME from it: the same fingerprint
    let mut h = Host::start(
        Mode::Serve {
            boot: Some(sched("rb=4 | run")),
        },
        8,
    );
    boot(&mut h);
    h.recv(); // READY
    h.tx.send(HostMsg::Request(Request::Ctl {
        tag: "a".into(),
        schedule: sched("rb=4 | run"),
    }))
    .unwrap();
    let a = h.drive(|h, n, _, why| {
        if n == 6 {
            h.snap("mid");
        }
        if why == StopWhy::Exit {
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    });
    h.tx.send(HostMsg::Request(Request::Resume {
        tag: "b".into(),
        snapshot: "mid".into(),
    }))
    .unwrap();
    let b = to_the_end(&mut h);
    assert_eq!(a.verdict, "HYDO-ORACLE: OK ticks");
    assert_eq!((a.fp, a.wfp), (b.fp, b.wfp), "{}", h.log.join("\n"));
    h.quit();
}

#[test]
fn the_clock_counts_exits_and_ticks_arrive_at_exits() {
    // more ticks: more exits, a later clock; and the console has one "tick" per interrupt
    let mut h = Host::start(
        Mode::Serve {
            boot: Some(sched("rb=4 | run")),
        },
        3,
    );
    boot(&mut h);
    h.recv();
    h.tx.send(HostMsg::Request(Request::Ctl {
        tag: "t".into(),
        schedule: sched("rb=4 | run"),
    }))
    .unwrap();
    let mut events = Vec::new();
    let mut ats = Vec::new();
    loop {
        match h.recv() {
            EngineMsg::Event { event, .. } => events.push(String::from_utf8(event.text).unwrap()),
            EngineMsg::Stop { stop, .. } => {
                ats.push(stop.at);
                if stop.why == StopWhy::Exit {
                    h.cmd(HostCmd::End(None));
                } else {
                    h.cmd(HostCmd::Cont);
                }
            }
            EngineMsg::Result(_) => break,
            _ => {}
        }
    }
    assert_eq!(
        events.iter().filter(|e| *e == "tick").count(),
        3,
        "{events:?}"
    );
    assert!(ats.windows(2).all(|w| w[0] <= w[1]), "{ats:?}");
    // a cadence of 4 exits: consecutive cadence stops are 4 exits apart or more
    h.quit();
}

#[test]
fn typed_input_is_reported_and_hashed() {
    // a SEND at a stop is typed at once (at <= now) and counted in the RESULT's typed=/inh=
    let mut h = Host::start(
        Mode::Serve {
            boot: Some(sched("rb=4 | run")),
        },
        2,
    );
    boot(&mut h);
    h.recv();
    let mut inh = Vec::new();
    for tag in ["s1", "s2"] {
        h.tx.send(HostMsg::Request(Request::Ctl {
            tag: tag.into(),
            schedule: sched("rb=4 | run"),
        }))
        .unwrap();
        let mut sent = false;
        let r = h.drive(|h, _, _, why| {
            if !sent {
                sent = true;
                h.cmd(HostCmd::Send {
                    at: 0,
                    bytes: b"hello\n".to_vec(),
                });
                match h.answer() {
                    Answer::Send(_) => {}
                    a => panic!("{a:?}"),
                }
            }
            if why == StopWhy::Exit {
                h.cmd(HostCmd::End(None));
                return true;
            }
            false
        });
        let ex: HashMap<_, _> = r.extras.iter().cloned().collect();
        assert_eq!(ex.get("typed").map(String::as_str), Some("1"));
        inh.push(ex.get("inh").cloned().unwrap());
    }
    assert_eq!(inh[0], inh[1]);
    h.quit();
}

/// A guest that reads `n` bytes from the serial input (an empty receive register reads 0),
/// then prints its verdict and halts.
fn reader_program(n: u64) -> Program {
    use Op::*;
    Program {
        ops: vec![
            Print(b"boot\n".to_vec()),
            In(hydo_kvm::uart::BASE, 1), // 1
            JmpIfBelow(1, 1),            // nothing there yet
            Inc,
            JmpIfCountBelow(n, 1),
            Print(b"HYDO-ORACLE: OK read\n".to_vec()),
            Cli,
            Hlt,
        ],
        handlers: HashMap::new(),
    }
}

#[test]
fn sends_wait_for_room_in_the_receive_queue() {
    // open item 82: three sends of 3,000 bytes at one stop, more than the 4,096-byte receive
    // queue holds. The first is typed at once; the others wait until the guest has read enough,
    // and every byte reaches the guest (before, the bytes that did not fit were dropped and the
    // guest never read its 9,000).
    let mut h = Host::start_program(
        Mode::Serve {
            boot: Some(sched("rb=64 | run")),
        },
        reader_program(9000),
    );
    boot(&mut h);
    h.recv();
    h.tx.send(HostMsg::Request(Request::Ctl {
        tag: "big".into(),
        schedule: sched("rb=64 | run"),
    }))
    .unwrap();
    let mut answers = Vec::new();
    let r = h.drive(|h, _, _, why| {
        if answers.is_empty() {
            for c in [b'a', b'b', b'c'] {
                h.cmd(HostCmd::Send {
                    at: 0,
                    bytes: vec![c; 3000],
                });
                match h.answer() {
                    Answer::Send(a) => answers.push(a),
                    a => panic!("{a:?}"),
                }
            }
        }
        if why == StopWhy::Exit {
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    });
    assert!(
        matches!(answers[0], SendAnswer::Typed { id: 1, .. }),
        "{answers:?}"
    );
    assert!(
        answers[1..]
            .iter()
            .all(|a| matches!(a, SendAnswer::Queued { .. })),
        "{answers:?}"
    );
    assert_eq!(r.verdict, "HYDO-ORACLE: OK read", "{:?}", h.log);
    let ex: HashMap<_, _> = r.extras.iter().cloned().collect();
    assert_eq!(ex.get("typed").map(String::as_str), Some("3"));
    // each queued send was reported when it was typed, in order
    let sent: Vec<String> = h
        .log
        .iter()
        .filter(|l| l.starts_with("SENT big"))
        .cloned()
        .collect();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent[0].contains("id=2") && sent[1].contains("id=3"),
        "{sent:?}"
    );
    h.quit();
}

#[test]
fn a_run_holds_more_than_1024_sends() {
    // open item 83: a run held at most 1,024 sends; a slow workload sends more
    let n = 3000;
    let mut h = Host::start_program(
        Mode::Serve {
            boot: Some(sched("rb=64 | run")),
        },
        reader_program(n),
    );
    boot(&mut h);
    h.recv();
    h.tx.send(HostMsg::Request(Request::Ctl {
        tag: "many".into(),
        schedule: sched("rb=64 | run"),
    }))
    .unwrap();
    let mut sent = 0;
    let r = h.drive(|h, _, _, why| {
        // a few hundred sends of one byte at each stop, each acknowledged at the next stop
        let first = sent + 1;
        while sent < n && sent + 1 - first < 400 {
            h.cmd(HostCmd::Send {
                at: 0,
                bytes: vec![b'x'],
            });
            match h.answer() {
                Answer::Send(_) => sent += 1,
                a => panic!("send {}: {a:?}", sent + 1),
            }
        }
        for id in first..=sent {
            h.cmd(HostCmd::Ack(id));
            assert!(matches!(h.answer(), Answer::Ack(_)));
        }
        if why == StopWhy::Exit {
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    });
    assert_eq!(r.verdict, "HYDO-ORACLE: OK read");
    let ex: HashMap<_, _> = r.extras.iter().cloned().collect();
    assert_eq!(ex.get("typed").map(String::as_str), Some("3000"));
    assert!(
        hydo_engine_proto::too_many_sends(hydo_engine_proto::MAX_SENDS)
            .starts_with("send more than 65536 sends in one run")
    );
    h.quit();
}

#[test]
fn hvx_only_tokens_are_refused() {
    let mut h = Host::start(
        Mode::Serve {
            boot: Some(sched("rb=4 | run")),
        },
        1,
    );
    boot(&mut h);
    h.recv();
    for s in ["disk=cut cf=3 rb=4 | run", "pp=5 rb=4 | run"] {
        h.tx.send(HostMsg::Request(Request::Ctl {
            tag: "x".into(),
            schedule: sched(s),
        }))
        .unwrap();
        let r = h.drive(|_, _, _, _| false);
        assert!(
            r.verdict.starts_with("UNSUPPORTED on kvm"),
            "{s}: {}",
            r.verdict
        );
    }
    h.quit();
}

/// The guest of `program` with its idle HLT replaced by an exit-free spin on the tick counter
/// (the counter is the handler's: an exact-repeat loop that only an interrupt ends, as the
/// mysqld loop of docs/design/kvm-engine.md).
fn spin_program(ticks: u64) -> Program {
    let mut p = program(ticks);
    p.ops[15] = Op::JmpIfCountBelow(ticks, 15);
    p.ops[16] = Op::Jmp(17);
    p
}

#[test]
fn an_exit_free_spin_is_resolved_and_the_run_stays_deterministic() {
    let mut fps = Vec::new();
    for tag in ["p1", "p2"] {
        let mut h = Host::start_program(
            Mode::Serve {
                boot: Some(sched("rb=4 | run")),
            },
            spin_program(4),
        );
        boot(&mut h);
        h.recv();
        h.tx.send(HostMsg::Request(Request::Ctl {
            tag: tag.into(),
            schedule: sched("rb=4 | run"),
        }))
        .unwrap();
        let r = to_the_end(&mut h);
        assert_eq!(r.verdict, "HYDO-ORACLE: OK ticks", "{}", h.log.join("\n"));
        fps.push((r.fp, r.wfp));
        h.quit();
    }
    assert_eq!(fps[0], fps[1]);
}

/// A guest that waits for its timer in a counter loop (`while (rax < 0x10000) rbx++`, the
/// counter feeding nothing else): the register state never repeats, so only the rewind mode
/// resolves it. The handler sets rax (an unknown port reads all ones) and the loop ends.
fn counter_program() -> Program {
    use Op::*;
    let mut ops = program(1).ops[..11].to_vec();
    ops.extend([
        Print(b"boot\n".to_vec()),
        RdMsr(0x1a0), // rax: the model's MISC_ENABLE, below 0x10000
        Out(PORT_TIMER, 4, 1_000_000),
        Sti,
        Inc, // 15: the counter loop
        JmpIfBelow(0x10000, 15),
        InPair(PORT_CLOCK),
        Print(b"HYDO-ORACLE: OK counted\n".to_vec()),
        Cli,
        Hlt,
    ]);
    let handler = ops.len();
    ops.extend([
        In(0x5f8, 4),
        Print(b"tick\n".to_vec()),
        Out(0x20, 1, 0x60),
        Iret,
    ]);
    Program {
        ops,
        handlers: HashMap::from([(TIMER_VECTOR, handler)]),
    }
}

/// One run of `prog` from S1 with each schedule, each from its own engine: the results (the
/// host ends a run at the guest's exit stop; `cont`: it says CONT there, so a guest that stopped
/// gives the engine's own verdict).
fn runs_of(prog: &Program, schedules: &[&str]) -> Vec<RunResult> {
    runs_of_with(prog, schedules, false)
}

fn runs_of_with(prog: &Program, schedules: &[&str], cont: bool) -> Vec<RunResult> {
    schedules
        .iter()
        .map(|s| {
            let mut h = Host::start_program(
                Mode::Serve {
                    boot: Some(sched("rb=4 | run")),
                },
                prog.clone(),
            );
            boot(&mut h);
            h.recv();
            h.tx.send(HostMsg::Request(Request::Ctl {
                tag: "c".into(),
                schedule: sched(s),
            }))
            .unwrap();
            let r = if cont {
                h.drive(|_, _, _, _| false)
            } else {
                to_the_end(&mut h)
            };
            h.quit();
            r
        })
        .collect()
}

#[test]
fn a_counter_loop_is_a_spin_without_the_rewind_mode_and_resolved_with_it() {
    let p = counter_program();
    let spin = runs_of_with(&p, &["rb=4 | run"], true);
    assert!(
        spin[0]
            .verdict
            .starts_with("ERROR workload: SPIN (guest loops without exits"),
        "{}",
        spin[0].verdict
    );
    let r = runs_of(
        &p,
        &[
            "spin=rewind rb=4 | run",
            "spin=rewind rb=4 | run",
            "spin=rewind rb=64 | run",
            "spin=rewind rb=64 | run",
        ],
    );
    for x in &r {
        assert_eq!(x.verdict, "HYDO-ORACLE: OK counted");
    }
    // the delivery point is the guest's: the same run whatever the cadence (rb=64 replays the
    // exits since the run's first checkpoint, rb=4 checkpoints at every stop)
    let key = |x: &RunResult| (x.fp.clone(), x.wfp.clone());
    assert_eq!(key(&r[0]), key(&r[1]));
    assert_eq!(key(&r[2]), key(&r[3]));
    assert_eq!(
        key(&r[0]),
        key(&r[2]),
        "the cadence does not move the delivery point"
    );
}

#[test]
fn the_rewind_mode_changes_nothing_without_a_counter_loop() {
    // a run that idles in HLT and a run with an exact-repeat spin: the same fingerprints, stops
    // and inh= with the mode on
    for p in [program(5), spin_program(4)] {
        let r = runs_of(&p, &["rb=4 | run", "spin=rewind rb=4 | run"]);
        assert!(
            r[0].verdict.starts_with("HYDO-ORACLE: OK"),
            "{}",
            r[0].verdict
        );
        assert_eq!(
            (&r[0].fp, &r[0].wfp, &r[0].verdict, &r[0].extras),
            (&r[1].fp, &r[1].wfp, &r[1].verdict, &r[1].extras)
        );
    }
}

#[test]
fn a_snap_in_the_rewind_mode_resumes_in_it() {
    let mut h = Host::start_program(
        Mode::Serve {
            boot: Some(sched("rb=4 | run")),
        },
        counter_program(),
    );
    boot(&mut h);
    h.recv();
    h.tx.send(HostMsg::Request(Request::Ctl {
        tag: "a".into(),
        schedule: sched("spin=rewind rb=4 | run"),
    }))
    .unwrap();
    let a = h.drive(|h, n, _, why| {
        if n == 3 {
            h.snap("mid");
        }
        if why == StopWhy::Exit {
            h.cmd(HostCmd::End(None));
            return true;
        }
        false
    });
    h.tx.send(HostMsg::Request(Request::Resume {
        tag: "b".into(),
        snapshot: "mid".into(),
    }))
    .unwrap();
    let b = to_the_end(&mut h);
    assert_eq!(a.verdict, "HYDO-ORACLE: OK counted", "{}", h.log.join("\n"));
    assert_eq!((a.fp, a.wfp), (b.fp, b.wfp), "{}", h.log.join("\n"));
    h.quit();
}

/// One run of `prog` from S1 with `s`, with or without the preemption records: the result and
/// the records file (None: not written).
fn run_recorded(prog: &Program, s: &str, record: bool) -> (RunResult, Option<String>) {
    let o = KvmOptions {
        preempt_record: record,
        ..KvmOptions::default()
    };
    let (mut h, out) = Host::start_with(
        Mode::Serve {
            boot: Some(sched("rb=4 | run")),
        },
        prog.clone(),
        o,
    );
    boot(&mut h);
    h.recv();
    h.tx.send(HostMsg::Request(Request::Ctl {
        tag: "c".into(),
        schedule: sched(s),
    }))
    .unwrap();
    let r = h.drive(|_, _, _, _| false);
    h.quit();
    (r, std::fs::read_to_string(out.join("c.preempt.jsonl")).ok())
}

/// A number field of a record line.
fn field(line: &str, k: &str) -> u64 {
    let at = line.find(&format!("\"{k}\":")).expect("the field") + k.len() + 3;
    line[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("a number")
}

#[test]
fn preemption_records_name_every_point_and_move_nothing() {
    // M208: HYDO_PREEMPT_RECORD reads registers only: the same run with and without it
    for (p, s, n) in [
        (
            program(40),
            "pct=ec699251106e8e0c pctn=12 pctk=400 rb=4 | run",
            12,
        ),
        // the rewind mode replays exits after a checkpoint: no point is recorded twice
        (
            counter_program(),
            "spin=rewind pct=5 pctn=8 pctk=300 rb=4 | run",
            8,
        ),
    ] {
        let (off, none) = run_recorded(&p, s, false);
        let (on, file) = run_recorded(&p, s, true);
        assert!(none.is_none(), "nothing written without the option");
        assert_eq!(
            (&off.fp, &off.wfp, &off.verdict, &off.extras),
            (&on.fp, &on.wfp, &on.verdict, &on.extras),
            "{s}"
        );
        let file = file.expect("the records");
        let recs: Vec<&str> = file.lines().collect();
        // every point the run reached (a point after the guest's end never fires)
        assert!(!recs.is_empty() && recs.len() <= n, "{s}: {file}");
        for (k, r) in recs.iter().enumerate() {
            assert!(r.starts_with("{\"v\":1,\"type\":\"preempt\""), "{r}");
            assert_eq!(field(r, "i"), k as u64, "{r}");
            assert!(r.contains("\"pc\":\"0x"), "{r}");
        }
        let exits: Vec<u64> = recs.iter().map(|r| field(r, "exit")).collect();
        assert!(exits.windows(2).all(|w| w[0] <= w[1]), "{exits:?}");
        // the points that moved the clock or found the timer due are the engine's pctf=
        let fired = recs
            .iter()
            .filter(|r| r.contains("\"result\":\"moved\"") || r.contains("\"result\":\"due\""))
            .count();
        let pctf = on
            .extras
            .iter()
            .find(|(k, _)| k == "pctf")
            .map(|(_, v)| v.parse::<usize>().unwrap());
        assert_eq!(pctf, Some(fired), "{s}: {file}");
    }
}

/// The one-vCPU fingerprints of the scripted guests, pinned before the work on several vCPUs
/// (docs/design/kvm-vcpus.md, "One vCPU stays exactly as it is"): a change that moves the
/// one-vCPU path fails here on any machine. Each line is a run's `fp`, `wfp`, extras (`stops=`,
/// `typed=`, `inh=`, `ctl=`, the tick-jitter and preemption-point counts) and verdict.
#[test]
fn one_vcpu_fingerprints_are_pinned() {
    // (name, guest, schedule, the host says CONT at the guest's exit stop: the engine's verdict)
    let cases: [(&str, Program, &str, bool); 6] = [
        ("idle", program(5), "rb=4 | run", false),
        (
            "jitter",
            program(40),
            "tj=1234 tjpm=1000 tjmax=300 rb=4 | run",
            false,
        ),
        (
            "preempt",
            program(40),
            "pct=5 pctn=8 pctk=300 rb=4 | run",
            false,
        ),
        ("cadence", program(5), "rb=64 | run", false),
        ("spin", spin_program(4), "rb=4 | run", false),
        (
            "counter",
            counter_program(),
            "spin=rewind rb=4 | run",
            false,
        ),
    ];
    let mut got = Vec::new();
    for (name, p, s, cont) in cases {
        let r = &runs_of_with(&p, &[s], cont)[0];
        let extras: Vec<String> = r.extras.iter().map(|(k, v)| format!("{k}={v}")).collect();
        got.push(format!(
            "{name} fp={} wfp={} {} | {}",
            r.fp,
            r.wfp,
            extras.join(" "),
            r.verdict
        ));
    }
    assert_eq!(got, PINNED_ONE_VCPU, "got:\n{}", got.join("\n"));
}

const PINNED_ONE_VCPU: [&str; 6] = [
    "idle fp=1be91463ed7b93ef wfp=8f02392fd574a1ea stops=22 typed=0 inh=0000000000000000 | HYDO-ORACLE: OK ticks",
    "jitter fp=f205c09b49407002 wfp=c02648c73673a250 tjk=0-12 tjn=11 stops=92 typed=0 inh=0000000000000000 | HYDO-ORACLE: OK ticks",
    "preempt fp=0cf0417a1a2cf149 wfp=c7bdacf949b69bf6 pctf=0 pctw=354 stops=92 typed=0 inh=0000000000000000 | HYDO-ORACLE: OK ticks",
    "cadence fp=1be91463ed7b93ef wfp=8f02392fd574a1ea stops=5 typed=0 inh=0000000000000000 | HYDO-ORACLE: OK ticks",
    "spin fp=2e8b92015190b56e wfp=1119861f7b2e16f4 stops=19 typed=0 inh=0000000000000000 | HYDO-ORACLE: OK ticks",
    "counter fp=3c94fb7ed9aacd9c wfp=cab9588566b03486 stops=14 typed=0 inh=0000000000000000 | HYDO-ORACLE: OK counted",
];
