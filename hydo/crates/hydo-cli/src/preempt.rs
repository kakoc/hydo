//! A run's forced preemption points as they fired (Milestone 208): the records the KVM and Mac
//! engines write to `one.preempt.jsonl` when `HYDO_PREEMPT_RECORD=1` and the schedule has
//! `pct=`. Each line is one point (`hydo_hvm::machine::PctRec`; this one is the Qdrant
//! example's point that only `pctn=40` has, Milestone 208):
//!
//! ```text
//! {"v":1,"type":"preempt","i":38,"exit":13863,"result":"moved","vtime_ns":3093347500,
//!  "to_ns":3097120000,"pc":"0xffffffff81232bc0","mode":"kernel","page_table":"0x2d03000",
//!  "thread_pointer":"0x7f375a2737f0","sp":"0xffffffff8180bf00"}
//! ```
//!
//! `i` is the point's index in exit order, `exit` the engine's exit count where it fired (the
//! clock of `pctk=` and of the stops' `at`), `result` what it did (`moved`: the guest's clock
//! moved to its timer's deadline, so the timer interrupt preempts the running thread at once;
//! `due`: the timer was already due; `missed`: no timer was armed; `passed`: its exit had passed
//! when the points were set), the clock before and after, and the guest's own registers there:
//! the instruction pointer, kernel or user mode, the page-table root (the process), the user
//! thread pointer (the FS base on x86_64, TPIDR_EL0 on arm64: the thread) and the stack pointer.
//! `hydo inspect events` shows each point at the stop that follows it; `hydo diff` names the
//! first point where two runs' records differ.

use hydo_control::{Record, Transcript};
use serde_json::Value;
use std::path::Path;

/// The records file of a `hydo run` directory (its run conversation's tag is `one`).
pub const FILE: &str = "one.preempt.jsonl";

/// The records of a run directory, or None when the run wrote none (not recorded).
pub fn read(dir: &Path) -> Option<Vec<Value>> {
    read_file(&dir.join(FILE))
}

/// The records of one file (an exploration keeps a run's as `consoles/<tag>.preempt.jsonl`).
pub fn read_file(f: &Path) -> Option<Vec<Value>> {
    let t = std::fs::read_to_string(f).ok()?;
    Some(
        t.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("type").and_then(Value::as_str) == Some("preempt"))
            .collect(),
    )
}

fn num(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn word<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("?")
}

/// The record's exit count.
pub fn exit(v: &Value) -> u64 {
    num(v, "exit")
}

/// What two runs' records are compared on: everything but the index (a point both runs have
/// is at the same exit with the same guest state, whatever its index).
fn key(v: &Value) -> String {
    let mut m = v.as_object().cloned().unwrap_or_default();
    m.remove("i");
    serde_json::to_string(&m).unwrap_or_default()
}

/// Digits grouped by thousands.
fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// One record for people: `point 38 at exit 13,863 moved the clock from 3,093,347,500 to
/// 3,097,120,000 ns (the timer's deadline): kernel code at pc 0xffffffff81232bc0, thread pointer
/// 0x7f375a2737f0 (page table 0x2d03000, sp 0xffffffff8180bf00)`.
pub fn text(v: &Value) -> String {
    let what = match word(v, "result") {
        "moved" => format!(
            "moved the clock from {} to {} ns (the timer's deadline)",
            grouped(num(v, "vtime_ns")),
            grouped(num(v, "to_ns"))
        ),
        "due" => "found the timer already due".into(),
        "missed" => "found no timer armed".into(),
        "passed" => "had passed when the points were set".into(),
        other => other.into(),
    };
    format!(
        "point {} at exit {} {what}: {} code at pc {}, thread pointer {} (page table {}, sp {})",
        num(v, "i"),
        grouped(exit(v)),
        word(v, "mode"),
        word(v, "pc"),
        word(v, "thread_pointer"),
        word(v, "page_table"),
        word(v, "sp"),
    )
}

/// The stop of `t` a point at `exit` comes before (the first stop at or after it; the last
/// stop when the conversation ended before reaching it), with that stop's clock.
pub fn stop_for(t: Option<&Transcript>, exit: u64) -> Option<(u64, u64)> {
    let mut last = None;
    for r in &t?.records {
        if let Record::Stop { n, at, .. } = r {
            last = Some((*n, *at));
            if *at >= exit {
                return last;
            }
        }
    }
    last
}

/// The first place where two runs' records differ, in exit order: a point only A has, a point
/// only B has, or a point at the same exit with other guest state (both). None: the same
/// points at the same exits with the same guest state.
pub fn first_difference<'a>(
    a: &'a [Value],
    b: &'a [Value],
) -> Option<(Option<&'a Value>, Option<&'a Value>)> {
    let (mut i, mut j) = (0, 0);
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return None,
            (Some(x), None) => return Some((Some(x), None)),
            (None, Some(y)) => return Some((None, Some(y))),
            (Some(x), Some(y)) => {
                if exit(x) < exit(y) {
                    return Some((Some(x), None));
                }
                if exit(y) < exit(x) {
                    return Some((None, Some(y)));
                }
                if key(x) != key(y) {
                    return Some((Some(x), Some(y)));
                }
                i += 1;
                j += 1;
            }
        }
    }
}

/// What a run directory's points did (`hydo inspect events`), or None when it recorded none.
pub fn summary(dir: &Path) -> Option<hydo_results::PreemptSummary> {
    let recs = read(dir)?;
    let count = |r: &str| recs.iter().filter(|v| word(v, "result") == r).count() as u64;
    let mut threads: Vec<&str> = recs
        .iter()
        .filter(|v| word(v, "result") == "moved")
        .map(|v| word(v, "thread_pointer"))
        .collect();
    threads.sort_unstable();
    threads.dedup();
    Some(hydo_results::PreemptSummary {
        file: FILE.into(),
        points: recs.len() as u64,
        moved: count("moved"),
        due: count("due"),
        missed: count("missed"),
        passed: count("passed"),
        threads: threads.len() as u64,
    })
}

/// How many points both runs' records have, before the first difference.
pub fn shared_before(a: &[Value], b: &[Value]) -> usize {
    a.iter()
        .zip(b)
        .take_while(|(x, y)| key(x) == key(y))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(i: u64, exit: u64, pc: &str) -> Value {
        serde_json::json!({"v":1,"type":"preempt","i":i,"exit":exit,"result":"moved",
            "vtime_ns":1,"to_ns":2,"pc":pc,"mode":"user","page_table":"0x1000",
            "thread_pointer":"0x7f00","sp":"0x7ffc"})
    }

    #[test]
    fn the_first_difference_is_the_point_only_one_run_has() {
        // pctn=39 against pctn=40: the 40th draw lands between the others
        let a = vec![rec(0, 100, "0x1"), rec(1, 300, "0x3")];
        let b = vec![rec(0, 100, "0x1"), rec(1, 200, "0x2"), rec(2, 300, "0x3")];
        let (x, y) = first_difference(&a, &b).expect("a difference");
        assert!(x.is_none());
        assert_eq!(exit(y.unwrap()), 200);
        assert_eq!(shared_before(&a, &b), 1);
        assert!(first_difference(&a, &a).is_none());
        // the same exit, another thread: both sides
        let c = vec![rec(0, 100, "0x1"), rec(1, 300, "0x9")];
        let (x, y) = first_difference(&a, &c).unwrap();
        assert_eq!(exit(x.unwrap()), 300);
        assert_eq!(exit(y.unwrap()), 300);
        let t = text(&b[1]);
        assert!(t.starts_with("point 1 at exit 200 moved the clock"), "{t}");
        assert_eq!(grouped(2716011), "2,716,011");
    }
}
