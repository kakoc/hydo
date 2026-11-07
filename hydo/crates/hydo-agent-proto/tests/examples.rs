//! The permanent example transcripts (`hydo/schema/examples/agent/v1/*.txt`, recorded from real
//! runs of the agent and never edited) must be read by this version of the host forever:
//! - each one passes [`transcript::check`] (a hello the session accepts, framed and canonical agent
//!   lines numbered in order, every answer pairs with a typed command);
//! - every host line that is a command and every agent line is valid by the committed schema;
//! - `INDEX` lists exactly the files there, each with what it shows;
//! - together they show every op, every reply, every error code the agent can give for a host line
//!   and every event, apart from the ones named in [`NOT_YET`] (they need a guest; see there).

use crate::common;

use std::collections::BTreeSet;

use common::{schema_dir, validate};
use hydo_agent_proto::frame::{deframe_line, Deframed, CAN};
use hydo_agent_proto::json;
use hydo_agent_proto::msg::{Command, ErrCode, Event};
use hydo_agent_proto::transcript::{self, Entry};

/// Kinds no example shows yet (sorted): the packet trigger's events and replies (they need a
/// netfilter queue, which an unprivileged native run cannot bind; step A3 proved the trigger in a
/// guest through hydo_fc's command line), the warnings, and a successful clock_set (only a guest
/// may set the realtime clock; a native run as root would set the host's).
const NOT_YET: &[&str] = &[
    "event.froze",
    "event.resumed",
    "event.warn",
    "reply.clock_set",
    "reply.freeze",
    "reply.unfreeze",
];

fn dir() -> std::path::PathBuf {
    schema_dir().join("examples/agent/v1")
}

fn files() -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    v.sort();
    v
}

#[test]
fn every_example_reads_and_is_valid() {
    let schema: json::Value = json::parse(
        std::fs::read(schema_dir().join("agent-v1.json"))
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    let mut seen = BTreeSet::new();
    let files = files();
    assert!(!files.is_empty());
    for p in &files {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let entries = transcript::parse(&std::fs::read_to_string(p).unwrap())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let sum = transcript::check(&entries).unwrap_or_else(|e| panic!("{name}: {e}"));
        for r in &sum.replied {
            seen.insert(format!("reply.{r}"));
        }
        for e in &sum.events {
            seen.insert(format!("event.{e}"));
        }
        for e in &sum.refusals {
            seen.insert(format!("error.{e}"));
        }
        for e in &entries {
            match e {
                Entry::Agent(l) => {
                    let Deframed::Agent(j) = deframe_line(l.as_bytes()) else {
                        panic!("{name}: {l}")
                    };
                    let v = json::parse(j.as_bytes()).unwrap();
                    validate(&schema, &v).unwrap_or_else(|e| panic!("{name}: {e}: {j}"));
                }
                Entry::Host(b) => {
                    for line in b.split(|&c| c == b'\n').filter(|l| !l.is_empty()) {
                        let line = line.rsplit(|&c| c == CAN).next().unwrap();
                        let Ok(v) = json::parse(line) else { continue };
                        if let Ok(h) = hydo_agent_proto::HostLine::decode(line, &[1]) {
                            seen.insert(format!("command.{}", h.cmd.name()));
                            validate(&schema, &v).unwrap_or_else(|e| {
                                panic!("{name}: {e}: {}", String::from_utf8_lossy(line))
                            });
                        }
                    }
                }
                Entry::Comment(_) => {}
            }
        }
    }
    let mut want = BTreeSet::new();
    for op in Command::NAMES {
        want.insert(format!("command.{op}"));
        want.insert(format!("reply.{op}"));
    }
    for ev in Event::NAMES {
        want.insert(format!("event.{ev}"));
    }
    for c in ErrCode::ALL {
        want.insert(format!("error.{}", c.as_str()));
    }
    let missing: Vec<_> = want.difference(&seen).cloned().collect();
    let not_yet: Vec<String> = NOT_YET.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        missing, not_yet,
        "the examples should show every kind except NOT_YET (update NOT_YET when one is added)"
    );
}

#[test]
fn the_index_lists_every_example() {
    let index = std::fs::read_to_string(dir().join("INDEX")).unwrap();
    let listed: Vec<&str> = index
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    let present: Vec<String> = files()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        listed,
        present.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn a_broken_example_is_seen() {
    let p = &dir().join("native-hello-sync-refusals.txt");
    let text = std::fs::read_to_string(p).unwrap();
    let mut entries = transcript::parse(&text).unwrap();
    // drop the first answer after the hello: the numbers no longer follow each other
    let k = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Entry::Agent(_)))
        .nth(1)
        .unwrap()
        .0;
    entries.remove(k);
    assert!(transcript::check(&entries).is_err());
}
