//! The engine on the scripted vCPU (`hydo_kvm::script`): the protocol, the clock, the PIC and the
//! timer, the interrupt rule, snapshots and fingerprints, with no KVM. The guest program
//! initializes the PIC as Linux does, arms the hydo timer, idles in HLT, counts ticks in its
//! interrupt handler (EOI, re-arm), reads the clock, prints its verdict and halts.

use hydo_engine_proto::{Answer, EngineMsg, HostCmd, HostMsg, Phase, Request, RunResult, StopWhy};
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
        };
        let (htx, hrx) = channel();
        let (etx, erx) = channel();
        let hv = Box::new(Script {
            program: Arc::new(prog),
        });
        let join = std::thread::spawn(move || {
            run(
                start,
                &KvmOptions::default(),
                Box::new(ChanPort::new(hrx, etx)),
                Box::new(std::io::sink()),
                None,
                hv,
            )
        });
        Host {
            tx: htx,
            rx: erx,
            join: Some(join),
            log: Vec::new(),
        }
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
