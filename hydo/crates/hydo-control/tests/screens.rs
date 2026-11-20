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

/// A plan of a walk whose image keeps a filmstrip (Milestone 334), with `check` as its one final
/// check.
fn film_plan(check: &str) -> Plan {
    let text = format!(
        "name: web\nservices:\n  - name: kv\n    cmd: kv-server\n    ready: kv-ping\n\
         workload:\n  steps: 2\n  run: client HYDO_FAKE_SHOTS HYDO_FAKE_FILM\ninvariants:\n  - id: all.done\n    final: true\n    cmd: {check}\n"
    );
    let m = hydo_manifest::from_embedded(&text, None).unwrap();
    let mut p = Plan::from_manifest(&m).unwrap();
    assert!(
        !p.film,
        "only an image with the walker's mark has a filmstrip"
    );
    p.screens = Some("/tmp/hydo/web/shots".into());
    p.walk = true;
    p.film = true;
    p
}

fn shot_reads(items: &[Item]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (p, hex) in reads(items) {
        if let Some(f) = p.strip_prefix("/tmp/hydo/web/shots/") {
            let f = format!("{f}{}", if hex { "" } else { " (text)" });
            if out.last() != Some(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// Milestone 334: an image with a filmstrip copies the index's files, then `film.jsonl` and its
/// frames, after the verdict; the traces of the steps only when the run failed, the latest step
/// first. The notes come after the verdict, and a replay gives the same transcript.
#[test]
fn a_walk_s_filmstrip_comes_out_and_its_traces_only_when_it_fails() {
    let s = Schedule::parse("disk=none | run").unwrap();
    let ok = film_plan("final-check");
    let b = support::boot(&ok, "kv").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    assert!(
        matches!(&a.run.fin.ending, Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: OK")),
        "{:?}",
        a.run.fin.ending
    );
    assert_eq!(
        shot_reads(&a.run.items),
        [
            "index.jsonl (text)",
            "step-0-1.png",
            "step-1-1.png",
            "film.jsonl (text)",
            "film.jsonl",
            "film-0-000.jpg",
            "film-0-001.jpg",
            "film-1-000.jpg",
            "film-1-001.jpg",
        ]
    );
    let lines = hydo_control::agent::console_text(&a.run.fin.transcript, &[]);
    assert!(lines.find("HYDO-ORACLE: OK").unwrap() < lines.find("HYDO-SCREEN").unwrap());
    let d = TmpDir::fresh(std::env::temp_dir().join(format!("hydo-film-{}", std::process::id())));
    hydo_control::agent::take_screens(&a.run.fin.transcript, &d.join("screens"));
    let film = std::fs::read_to_string(d.join("screens/film.jsonl")).unwrap();
    assert_eq!(film.lines().count(), 4);
    assert_eq!(
        std::fs::read_to_string(d.join("screens/film-1-001.jpg")).unwrap(),
        "JPG 1.1"
    );
    // a second transcript's filmstrip follows the first one's (a recovery boot's read-back)
    hydo_control::agent::take_screens(&a.run.fin.transcript, &d.join("screens"));
    let film2 = std::fs::read_to_string(d.join("screens/film.jsonl")).unwrap();
    assert_eq!(film2.lines().count(), 8);
    let index = std::fs::read_to_string(d.join("screens/index.jsonl")).unwrap();
    assert_eq!(
        index.lines().filter(|l| l.contains("step-0-1.png")).count(),
        1
    );
    let _ = std::fs::remove_dir_all(&d);
    let again = support::run_schedule(&b, &s).unwrap();
    assert_eq!(a.run.fin.transcript, again.run.fin.transcript);
    // a check that the scripted agent fails: the traces follow, step 1 first
    let failing = (0..200)
        .map(|n| format!("final-check-{n}"))
        .find_map(|c| {
            let p = film_plan(&c);
            let b = support::boot(&p, "kv").ok()?;
            let r = support::run_schedule(&b, &s).ok()?;
            matches!(&r.run.fin.ending, Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: VIOLATION"))
                .then_some(r)
        })
        .expect("a check name that the scripted agent fails");
    let got = shot_reads(&failing.run.items);
    assert_eq!(
        got[got.len() - 2..],
        ["walk-step-1.trace.zip", "walk-step-0.trace.zip"]
    );
}

/// Milestone 334: a power cut copies the filmstrip and the last step's trace out of the first
/// boot before the cut, with every service frozen; an image without a filmstrip reads nothing
/// there.
#[test]
fn a_power_cut_takes_the_filmstrip_out_before_the_cut() {
    let s = Schedule::parse("disk=none | run C=1").unwrap();
    let p = film_plan("final-check");
    let b = support::boot(&p, "kv").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    assert!(
        matches!(&a.run.fin.ending, Some(Ending::PowerCut { .. })),
        "{:?}",
        a.run.fin.ending
    );
    assert_eq!(
        shot_reads(&a.run.items),
        [
            "index.jsonl (text)",
            "step-0-1.png",
            "film.jsonl (text)",
            "film.jsonl",
            "film-0-000.jpg",
            "film-0-001.jpg",
            "walk-step-0.trace.zip",
        ]
    );
    let lines = hydo_control::agent::console_text(&a.run.fin.transcript, &[]);
    assert!(
        lines.find("HYDO-SCREEN").unwrap() < lines.find("hydo-explore-workload-done cut").unwrap()
    );
    let mut old = film_plan("final-check");
    old.film = false;
    let b = support::boot(&old, "kv").unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    assert!(shot_reads(&a.run.items).is_empty());
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
