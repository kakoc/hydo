//! The Rust side of the equivalence test (tests/equiv.py): reads cases as JSON lines on stdin and
//! writes one answer line per case on stdout (hydo_history::equiv::answer).
//!
//! With HYDO_HIST_TIMES=<file>, the time of each "checks" case (ns) goes there, one per line.

use std::io::{BufRead, Write};

fn main() {
    let mut tf = std::env::var("HYDO_HIST_TIMES")
        .ok()
        .map(|p| std::fs::File::create(p).expect("times file"));
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read");
        if line.trim().is_empty() {
            continue;
        }
        let (a, took) = hydo_history::equiv::answer(&line);
        if let (Some(f), Some(t)) = (tf.as_mut(), took) {
            let _ = writeln!(f, "{t}");
        }
        let _ = writeln!(out, "{a}");
    }
}
