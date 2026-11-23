//! `guest.path` (Milestone 348): every command the host controller starts gets `PATH` in its
//! environment, so a program that a workload command starts finds `hydo-oci` by name (open item
//! 170). The variable is in guest RAM, so an absent key means no `PATH` for every image built at
//! the fingerprint epochs so far, and the recorded transcripts stay the same.

use crate::support::{self, Item};
use hydo_agent_proto::msg::Command;
use hydo_control::Plan;
use hydo_schedule::Schedule;

const MANIFEST: &str = "name: path-test
services:
  - name: n1
    cmd: node 1
    ready: ping 1
workload:
  steps: 2
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
";

fn plan_of(key: Option<bool>) -> Plan {
    let text = match key {
        None => MANIFEST.to_string(),
        Some(b) => format!("{MANIFEST}guest:\n  path: {b}\n"),
    };
    let m = hydo_manifest::from_embedded(&text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

/// The `PATH` of each command a conversation started (None: the command had none).
fn paths(items: &[Item]) -> Vec<(String, Option<String>)> {
    items
        .iter()
        .filter_map(|it| match it {
            Item::Send { cmd, .. } => {
                let (argv, env) = match cmd {
                    Command::Run(r) => (&r.argv, &r.env),
                    Command::Spawn(s) => (&s.argv, &s.env),
                    _ => return None,
                };
                let p = env
                    .iter()
                    .flatten()
                    .find_map(|e| e.strip_prefix("PATH="))
                    .map(str::to_string);
                Some((argv.join(" "), p))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn with_the_key_every_command_gets_path() {
    let p = plan_of(Some(true));
    assert_eq!(p.path, Some(true));
    let b = support::boot(&p, "path-on").unwrap();
    let r = support::run_schedule(&b, &Schedule::parse("disk=none | run").unwrap()).unwrap();
    let got = paths(&r.run.items);
    let workload: Vec<_> = got.iter().filter(|(a, _)| a.contains("client")).collect();
    assert!(!workload.is_empty(), "the workload ran: {got:?}");
    for (argv, path) in &got {
        assert_eq!(path.as_deref(), Some(hydo_image::COMMAND_PATH), "{argv}");
    }
}

#[test]
fn without_the_key_or_with_false_no_command_gets_path() {
    for v in [None, Some(false)] {
        let p = plan_of(v);
        assert_eq!(p.path, v);
        let b = support::boot(&p, "path-off").unwrap();
        let r = support::run_schedule(&b, &Schedule::parse("disk=none | run").unwrap()).unwrap();
        let got = paths(&r.run.items);
        assert!(!got.is_empty());
        assert!(got.iter().all(|(_, p)| p.is_none()), "{v:?}: {got:?}");
    }
}
