//! Every topic of `hydo guide --list` prints text, and no topic is much shorter than its last
//! recorded size (Milestone 363). On 2026-11-25 a scripted merge emptied hydo/guide/strategies.md:
//! `hydo guide strategies` printed nothing on `main`, and every check passed, because the CLI's
//! golden answers record what the command prints, and they were recorded again with it empty.
//!
//! The sizes are in tests/guide-sizes.txt, one `<topic> <bytes>` line per topic. A topic fails
//! when it prints nothing, or less than three quarters of its recorded size. A topic that grows
//! passes. After a deliberate change that shortens a topic, or after a new topic, record the
//! sizes again with `HYDO_BLESS=1 cargo test -p hydo-agents --test it guide_topics::`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A topic fails below this share of its recorded size: three quarters.
const SHORTER: (usize, usize) = (3, 4);

/// Whether `n` bytes are too few for a topic recorded with `r`.
fn too_short(n: usize, r: usize) -> bool {
    n == 0 || n * SHORTER.1 < r * SHORTER.0
}

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root")
}

fn sizes_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guide-sizes.txt")
}

/// What `hydo guide <args>` prints, as the command prints it.
fn guide(args: &[&str]) -> String {
    let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let mut buf: Vec<u8> = Vec::new();
    let mut out = hydo_agents::Out::new(&mut buf);
    hydo_agents::guide(&argv, &top(), &mut out)
        .unwrap_or_else(|e| panic!("hydo guide {}: {e}", args.join(" ")));
    String::from_utf8(buf).expect("the guide prints UTF-8")
}

/// The topics of `hydo guide --list`: every `hydo guide <name>` line but the placeholders
/// (`examples/<file>`, `<file>`) and the line of `hydo guide` alone.
fn listed() -> Vec<String> {
    let list = guide(&["--list"]);
    let mut v: Vec<String> = list
        .lines()
        .filter_map(|l| l.strip_prefix("hydo guide "))
        // the line of `hydo guide` alone has its description after the padding
        .filter(|r| !r.starts_with(' '))
        .filter_map(|r| r.split_whitespace().next())
        .filter(|n| !n.contains('<'))
        .map(str::to_string)
        .collect();
    v.sort();
    v.dedup();
    v
}

fn recorded() -> BTreeMap<String, usize> {
    let text = std::fs::read_to_string(sizes_file()).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (k, v) = l.split_once(' ')?;
            Some((k.to_string(), v.trim().parse().ok()?))
        })
        .collect()
}

#[test]
fn every_topic_prints_text_of_its_recorded_size() {
    let topics = listed();
    assert!(
        topics.len() >= 20,
        "hydo guide --list names {} topics: {topics:?}",
        topics.len()
    );
    let now: BTreeMap<String, usize> = topics
        .iter()
        .map(|t| (t.clone(), guide(&[t]).len()))
        .collect();
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        let mut t = String::from(
            "# The size in bytes of what `hydo guide <topic>` prints, per topic of `hydo guide \
             --list`\n# (tests/guide_topics.rs). Written by HYDO_BLESS=1 cargo test -p \
             hydo-agents --test it guide_topics::\n",
        );
        for (k, v) in &now {
            t.push_str(&format!("{k} {v}\n"));
        }
        std::fs::write(sizes_file(), t).expect("write tests/guide-sizes.txt");
        return;
    }
    let rec = recorded();
    let mut bad: Vec<String> = Vec::new();
    for (k, &n) in &now {
        if n == 0 {
            bad.push(format!("`hydo guide {k}` prints nothing"));
            continue;
        }
        match rec.get(k) {
            None => bad.push(format!(
                "`hydo guide {k}` has no recorded size ({n} bytes now)"
            )),
            Some(&r) if too_short(n, r) => bad.push(format!(
                "`hydo guide {k}` prints {n} bytes, less than three quarters of its recorded {r}"
            )),
            Some(_) => {}
        }
    }
    for k in rec.keys().filter(|k| !now.contains_key(*k)) {
        bad.push(format!(
            "`hydo guide {k}` has a recorded size, but `hydo guide --list` does not name it"
        ));
    }
    assert!(
        bad.is_empty(),
        "{}\n(after a deliberate change, record the sizes again: HYDO_BLESS=1 cargo test -p \
         hydo-agents --test it guide_topics::)",
        bad.join("\n")
    );
}

/// The check fails on the case it was written for: an emptied topic, and a topic cut short.
#[test]
fn an_empty_or_cut_topic_fails() {
    let rec = recorded();
    let r = *rec
        .get("strategies")
        .expect("strategies has a recorded size");
    assert!(too_short(0, r));
    assert!(too_short(r / 2, r));
    assert!(!too_short(r, r));
    assert!(!too_short(r + r / 10, r));
}
