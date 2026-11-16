//! `browser:`'s screenshots out of the guest (Milestone 254) on the scripted agent and engine:
//! after the verdict the controller reads the watcher's index and each file in hex, and sends
//! each as one `HYDO-SCREEN` note; `write_run_files` writes them into `screens/`. Without
//! `browser:` the run sends the same commands as before.

use crate::support;
use crate::tmpdir::TmpDir;

use hydo_agent_proto::msg::Command;
use hydo_control::transcript::Record;
use hydo_control::{Ending, Plan};
use hydo_schedule::Schedule;
use support::Item;

fn plan(screens: bool) -> Plan {
    let text = "name: web\nservices:\n  - name: kv\n    cmd: kv-server\n    ready: kv-ping\n\
                workload:\n  steps: 2\n  run: client HYDO_FAKE_SHOTS\ninvariants:\n  - id: all.done\n    final: true\n    cmd: final-check\n";
    let m = hydo_manifest::from_embedded(text, None).unwrap();
    let mut p = Plan::from_manifest(&m).unwrap();
    assert_eq!(p.screens, None);
    if screens {
        p.screens = Some("/tmp/hydo/web/shots".into());
    }
    p
}

fn reads(items: &[Item]) -> Vec<(String, bool)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Read(r),
                ..
            } => Some((r.path.clone(), r.hex == Some(true))),
            _ => None,
        })
        .collect()
}

fn notes(t: &hydo_control::transcript::Transcript) -> Vec<String> {
    t.records
        .iter()
        .filter_map(|r| match r {
            Record::Note { line, .. } if line.starts_with("HYDO-SCREEN") => Some(line.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_screenshots_come_out_after_the_verdict() {
    let p = plan(true);
    let b = support::boot(&p, "kv").unwrap();
    let s = Schedule::parse("disk=none | run").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    assert!(
        matches!(&a.run.fin.ending, Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: OK")),
        "{:?}",
        a.run.fin.ending
    );
    let shots: Vec<(String, bool)> = reads(&a.run.items)
        .into_iter()
        .filter(|(p, _)| p.starts_with("/tmp/hydo/web/"))
        .collect();
    // the index as text, then each file in hex: step 1's file of 2,013 bytes takes two reads
    assert_eq!(
        shots,
        [
            ("/tmp/hydo/web/shots/index.jsonl".into(), false),
            ("/tmp/hydo/web/shots/step-0-1.png".into(), true),
            ("/tmp/hydo/web/shots/step-1-1.png".into(), true),
            ("/tmp/hydo/web/shots/step-1-1.png".into(), true),
        ]
    );
    let n = notes(&a.run.fin.transcript);
    assert_eq!(n.len(), 2, "{n:?}");
    let (file, bytes, _) = hydo_control::schedule::screens::of_note(&n[1]).unwrap();
    assert_eq!(file, "step-1-1.png");
    assert_eq!(bytes.len(), 2 + "PNG step 1 ".len() + 2000);
    // the verdict is reported before the reads, so they change nothing the run decided
    let lines = hydo_control::agent::console_text(&a.run.fin.transcript, &[]);
    assert!(lines.find("HYDO-ORACLE: OK").unwrap() < lines.find("HYDO-SCREEN").unwrap());
    // the run directory gets the files, and the transcript keeps the notes without the bytes
    let d =
        TmpDir::fresh(std::env::temp_dir().join(format!("hydo-screens-{}", std::process::id())));
    let t = hydo_control::agent::take_screens(&a.run.fin.transcript, &d.join("screens"));
    assert_eq!(
        std::fs::read(d.join("screens/step-1-1.png")).unwrap(),
        bytes
    );
    let index = std::fs::read_to_string(d.join("screens/index.jsonl")).unwrap();
    assert_eq!(index.lines().count(), 2);
    assert!(!index.contains("\"hex\""));
    assert!(notes(&t).iter().all(|l| !l.contains("\"hex\"")));
    let _ = std::fs::remove_dir_all(&d);
    // a replay of the run gives the same transcript
    let again = support::run_schedule(&b, &s).unwrap();
    assert_eq!(a.run.fin.transcript, again.run.fin.transcript);
}

/// Milestone 264: a run copies at most `screens::MAX_BYTES` out of the guest, so the copy fits
/// the engines' channels and never ends a run in `TIMEOUT check`; a file larger than what is
/// left costs one read, stays in the guest, and a note names it.
#[test]
fn a_file_larger_than_the_copy_budget_stays_in_the_guest() {
    let text = "name: web\nservices:\n  - name: kv\n    cmd: kv-server\n    ready: kv-ping\n\
                workload:\n  steps: 2\n  run: client HYDO_FAKE_SHOTS HYDO_FAKE_BIG_SHOT\ninvariants:\n  - id: all.done\n    final: true\n    cmd: final-check\n";
    let m = hydo_manifest::from_embedded(text, None).unwrap();
    let mut p = Plan::from_manifest(&m).unwrap();
    p.screens = Some("/tmp/hydo/web/shots".into());
    let b = support::boot(&p, "kv").unwrap();
    let s = Schedule::parse("disk=none | run").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    let shots: Vec<String> = reads(&a.run.items)
        .into_iter()
        .filter(|(p, _)| p.starts_with("/tmp/hydo/web/shots/walk"))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(shots, ["/tmp/hydo/web/shots/walk-step-0.webm"]);
    let n = notes(&a.run.fin.transcript);
    assert!(
        n[0].starts_with("HYDO-SCREEN-LEFT {\"file\":\"walk-step-0.webm\",\"bytes\":1048577,"),
        "{n:?}"
    );
    // the screenshots after it still come out
    assert_eq!(n.len(), 3, "{n:?}");
    assert!(hydo_control::schedule::screens::of_note(&n[0]).is_none());
    assert_eq!(
        hydo_control::schedule::screens::of_note(&n[2]).unwrap().0,
        "step-1-1.png"
    );
}

#[test]
fn without_browser_nothing_is_read() {
    let p = plan(false);
    let b = support::boot(&p, "kv").unwrap();
    let a = support::run_schedule(&b, &Schedule::parse("disk=none | run").unwrap()).unwrap();
    assert!(reads(&a.run.items)
        .iter()
        .all(|(p, _)| !p.starts_with("/tmp/hydo/web/")));
    assert!(notes(&a.run.fin.transcript).is_empty());
}

/// The writes of a run's items to one guest file: (the text, whether it appends).
fn writes_to(items: &[Item], file: &str) -> Vec<(String, bool)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Write(w),
                ..
            } if w.path == file => Some((w.data.clone(), w.append == Some(true))),
            _ => None,
        })
        .collect()
}

/// Milestone 270: a recording replay (`I=...,rec`) gets the script of the run it replays in the
/// guest before the walk's first step, in parts small enough for the agent's channel; a walk
/// without `rec`, or a plan without a script, sends the same commands as before.
#[test]
fn a_recording_replay_gets_its_script_before_the_walk() {
    let file = hydo_manifest::browser::WALK_SCRIPT;
    let line = |k: usize| {
        format!(
            "0.{k} {{\"do\":\"fill\",\"value\":\"{}\"}}\n",
            "a".repeat(300)
        )
    };
    let script: String = (0..9).map(line).collect();
    let mut p = plan(true);
    p.walk = true;
    p.walk_script = Some(script.clone());
    let b = support::boot(&p, "kv").unwrap();
    let run = |b: &support::Booted, s: &str| {
        support::run_schedule(b, &Schedule::parse(s).unwrap()).unwrap()
    };
    let rec = run(&b, "disk=none | run I=1,rec");
    let w = writes_to(&rec.run.items, file);
    assert!(w.len() > 1, "{w:?}");
    assert!(!w[0].1 && w[1..].iter().all(|(_, a)| *a));
    assert!(w.iter().all(|(t, _)| t.len() <= 1200));
    assert_eq!(
        w.iter().map(|(t, _)| t.as_str()).collect::<String>(),
        script
    );
    // the script comes before the workload's command
    let at = |pred: &dyn Fn(&Command) -> bool| {
        rec.run
            .items
            .iter()
            .position(|i| matches!(i, Item::Send { cmd, .. } if pred(cmd)))
            .unwrap()
    };
    let first_write = at(&|c| matches!(c, Command::Write(x) if x.path == file));
    let workload = at(&|c| {
        matches!(c, Command::Spawn(x)
            if x.env.iter().flatten().any(|e| e.starts_with("HYDO_WALK=1,rec")))
    });
    assert!(first_write < workload);
    // without rec, and without a script, nothing is written
    let plain = run(&b, "disk=none | run I=1");
    assert!(writes_to(&plain.run.items, file).is_empty());
    let mut q = plan(true);
    q.walk = true;
    let b2 = support::boot(&q, "kv").unwrap();
    let none = run(&b2, "disk=none | run I=1,rec");
    assert!(writes_to(&none.run.items, file).is_empty());
}
