//! `browser:`'s screenshots out of the guest (Milestone 254) on the scripted agent and engine:
//! after the verdict the controller reads the watcher's index and each file in hex, and sends
//! each as one `HYDO-SCREEN` note; `write_run_files` writes them into `screens/`. Without
//! `browser:` the run sends the same commands as before.

use crate::support;

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
    let d = std::env::temp_dir().join(format!("hydo-screens-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
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
