//! Replay the recorded lock-step transcripts (`tests/lockstep/*.txt`, written by
//! `examples/lockstep_check.rs` on the bare-metal test machine, M118, and `agent-*.txt` by
//! `examples/agent_boot_check.rs` on both engines, M121; never edited) through the
//! typed client: every engine line parses, every host line is what the client writes for the
//! same call, and the protocol's ordering rules hold (stop numbers count up by one, a resumed run
//! repeats its snapshot's stop number, every command but CONT/END gets exactly one answer, the
//! run ends with one RESULT).

use hydo_engine::lockstep::{
    ChanLink, HostCmd, Link, Lockstep, Next, SendAnswer, SnapAnswer, TextLink,
};
use hydo_engine::protocol::{EngineMsg, HostMsg};
use std::io::Cursor;
use std::path::PathBuf;

fn transcripts() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/lockstep");
    let mut v: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("tests/lockstep")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    assert!(v.len() >= 4, "the recorded transcripts are missing");
    v
}

/// Drive a client over the engine half of a transcript, issuing the host half's commands through
/// the typed API; the bytes it writes must be the host half exactly, and its own transcript must
/// be the recorded one. `chan`: over the in-process link (the engine half as values, M142),
/// else over the text link.
fn replay(name: &str, text: &str, chan: bool) {
    // serve: the first line is the CTL/RESUME request; `--control one` has none on stdin, nor
    // has an agent image's controlled boot (M121: `agent-boot-*`, `agent-one-*`). A CTL of an
    // agent image restores the host's S1 and starts like a RESUME (why=resume, S1's number).
    let agent = name.starts_with("agent-");
    let resume = text.starts_with("< RESUME ") || (agent && text.starts_with("< CTL "));
    let request = text.starts_with("< RESUME ") || text.starts_with("< CTL ");
    assert!(
        request
            || name.starts_with("one")
            || name.starts_with("agent-boot")
            || name.starts_with("agent-one"),
        "{name}: no request line"
    );
    let body: Vec<&str> = text.lines().skip(request as usize).collect();
    let engine: String = body
        .iter()
        .filter_map(|l| l.strip_prefix("> "))
        .map(|l| format!("{l}\n"))
        .collect();
    let host: Vec<HostCmd> = body
        .iter()
        .filter_map(|l| l.strip_prefix("< "))
        .map(|l| HostCmd::parse(l).unwrap_or_else(|| panic!("{name}: host line {l:?}")))
        .collect();
    // M142: every engine line of both engines is a protocol value that prints back byte for byte
    for l in body.iter().filter_map(|l| l.strip_prefix("> ")) {
        let m = EngineMsg::parse(l);
        assert!(
            !matches!(m, EngineMsg::Other(_)),
            "{name}: engine line {l:?}"
        );
        assert_eq!(m.line(), *l, "{name}: the value's line");
    }
    let (htx, hrx) = std::sync::mpsc::channel::<HostMsg>();
    let (etx, erx) = std::sync::mpsc::channel::<EngineMsg>();
    for l in engine.lines() {
        etx.send(EngineMsg::parse(l)).unwrap();
    }
    drop(etx);
    let mut cl = ChanLink::new(htx, erx);
    let mut tl = TextLink::new(Cursor::new(engine.into_bytes()), Vec::new());
    let link: &mut dyn Link = if chan { &mut cl } else { &mut tl };
    let mut log = Vec::new();
    let want_log: String = body.iter().map(|l| format!("{l}\n")).collect();
    let mut s = Lockstep::new(link).with_log(&mut log);
    let mut cmds = host.iter();
    let mut last_n = None;
    let mut first_stop = true;
    let result = loop {
        match s.next_stop().unwrap_or_else(|e| panic!("{name}: {e}")) {
            Next::Done(r) => break r,
            Next::Stop(rep) => {
                let n = rep.stop.n;
                if let Some(prev) = last_n {
                    assert_eq!(n, prev + 1, "{name}: stop numbers");
                } else if !resume {
                    assert_eq!(n, 1, "{name}: the first stop is number 1");
                }
                assert_eq!(
                    first_stop && resume,
                    rep.stop.why == hydo_engine::lockstep::StopWhy::Resume,
                    "{name}: a resumed run starts with why=resume"
                );
                first_stop = false;
                last_n = Some(n);
                for e in &rep.events {
                    assert!(e.at <= rep.stop.at, "{name}: an event after its stop");
                }
                // the commands at this stop, up to CONT or END
                loop {
                    match cmds
                        .next()
                        .unwrap_or_else(|| panic!("{name}: no command at stop {n}"))
                    {
                        HostCmd::Send { at, bytes } => match s.send(*at, bytes).unwrap() {
                            SendAnswer::Typed { at: t, .. } => {
                                assert!(*at <= t && t == rep.stop.at)
                            }
                            SendAnswer::Queued { at: q, .. } => {
                                assert!(q == *at && q > rep.stop.at)
                            }
                        },
                        HostCmd::Ack(id) => {
                            s.ack(*id).unwrap().unwrap();
                        }
                        HostCmd::Snap { name: sn, retype } => {
                            if let SnapAnswer::Taken { at, .. } = s.snap(sn, *retype).unwrap() {
                                assert_eq!(at, rep.stop.at);
                            }
                        }
                        HostCmd::DiskOff => {
                            s.diskoff().unwrap().unwrap();
                        }
                        HostCmd::Cut => {
                            s.cut().unwrap().unwrap();
                            break;
                        }
                        HostCmd::Cont => {
                            s.cont().unwrap();
                            break;
                        }
                        HostCmd::End(v) => {
                            let r = s.end(v.as_deref()).unwrap();
                            assert!(cmds.next().is_none());
                            let w = written(chan, &hrx, &tl);
                            check_host(name, &w, &host);
                            assert_eq!(String::from_utf8_lossy(&log), want_log, "{name}");
                            assert!(r.extra("stops").is_some());
                            return;
                        }
                    }
                }
            }
        }
    };
    assert!(cmds.next().is_none(), "{name}: commands after the RESULT");
    let w = written(chan, &hrx, &tl);
    check_host(name, &w, &host);
    assert_eq!(
        String::from_utf8_lossy(&log),
        want_log,
        "{name}: transcript"
    );
    assert_eq!(
        result.extra("stops").map(|v| v.parse::<u64>().unwrap()),
        last_n,
        "{name}: stops= is the last stop's number"
    );
    assert!(result.extra("typed").is_some() && result.extra("inh").is_some());
}

/// What the client sent: the values over the channel, or the text link's bytes.
fn written(
    chan: bool,
    hrx: &std::sync::mpsc::Receiver<HostMsg>,
    tl: &TextLink<Cursor<Vec<u8>>, Vec<u8>>,
) -> Vec<u8> {
    if chan {
        hrx.try_iter()
            .map(|m| format!("{}\n", m.line()))
            .collect::<String>()
            .into_bytes()
    } else {
        tl.writer().clone()
    }
}

fn check_host(name: &str, written: &[u8], host: &[HostCmd]) {
    let want: String = host.iter().map(|c| format!("{}\n", c.line())).collect();
    assert_eq!(
        String::from_utf8_lossy(written),
        want,
        "{name}: the client wrote other host lines"
    );
}

#[test]
fn recorded_transcripts_replay() {
    for (name, text) in transcripts() {
        replay(&name, &text, false);
        replay(&name, &text, true);
    }
}

#[test]
fn agent_boot_transcripts() {
    // M121: a controlled boot from the first instruction (icount or exit 0), the host's S0 and S1,
    // and a CTL from that S1 that continues the boot's stop numbers
    let all = transcripts();
    let mut seen = 0;
    for eng in ["hvx", "hvm"] {
        let Some(boot) = all
            .iter()
            .find(|(f, _)| *f == format!("agent-boot-{eng}.txt"))
        else {
            continue;
        };
        seen += 1;
        let boot = &boot.1;
        assert!(
            boot.starts_with("> STOP boot at=0 n=1 phase=boot why=start\n"),
            "{eng}"
        );
        let snap = |name: &str| {
            boot.lines()
                .find_map(|l| l.strip_prefix(&format!("> OK snap {name} at=")))
                .map(|r| r.split(' ').next().unwrap().to_string())
                .unwrap_or_else(|| panic!("{eng}: no SNAP {name}"))
        };
        let (s0, s1) = (snap("S0"), snap("S1"));
        // S0 is the stop where the agent's hello was seen
        let hello = boot
            .lines()
            .find(|l| l.contains("\"ev\":\"hello\""))
            .unwrap();
        assert!(
            hello.starts_with(&format!("> EVENT boot at={s0} ")),
            "{eng}: {hello}"
        );
        let ctl = &all
            .iter()
            .find(|(f, _)| *f == format!("agent-s1-{eng}.txt"))
            .unwrap()
            .1;
        let first = ctl.lines().find(|l| l.starts_with("> STOP ")).unwrap();
        let s1_stop = boot
            .lines()
            .take_while(|l| !l.starts_with("> OK snap S1 "))
            .filter(|l| l.starts_with("> STOP "))
            .last()
            .unwrap();
        let n = s1_stop
            .split(" n=")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        assert_eq!(
            first,
            format!("> STOP s1a at={s1} n={n} phase=workload why=resume"),
            "{eng}"
        );
    }
    assert!(seen > 0, "no agent-boot transcript");
}

#[test]
fn transcripts_show_the_rules() {
    let all = transcripts();
    let get = |n: &str| &all.iter().find(|(f, _)| f == n).unwrap().1;
    // a send typed at an exact point: the same stop in a run from S1 and from a fresh boot
    let sent = |t: &str| {
        t.lines()
            .find(|l| l.starts_with("> SENT ") || l.contains("OK send id=1 sent="))
            .map(|l| l.rsplit("at=").next().unwrap().to_string())
    };
    assert_eq!(sent(get("at20000-a.txt")), sent(get("one-at20000.txt")));
    // the snapshot rule: refused while typed input is unacknowledged, then taken with retype,
    // and the continuation types it again at the snapshot's icount
    let base = get("same-base.txt");
    assert!(base.contains("> ERR snap same unacked=1"));
    assert!(base.contains("> OK snap same at="));
    let cont = get("same-c1.txt");
    assert!(cont
        .lines()
        .any(|l| l.starts_with("> SENT same-c1 id=1 at=") && l.ends_with(" retype=1")));
}
