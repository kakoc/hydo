//! `hydo diff <run dir A> <run dir B>` (Milestone 165, docs/design/explain.md): where two runs
//! first differ, at the levels hydo records without engine changes.
//!
//! A run is read as a sequence of events per conversation (boot, run, recovery, and the run's
//! result): the commands the host typed and its engine actions (the control transcripts'
//! `send`, `engine`, `snap` and `end` records), the host controller's notes and reports, and the
//! lines the guest's own programs printed (the merged console `<tag>.log`, in which the
//! controller put each guest line before the report of the stop that saw it, so each guest line
//! gets that stop). Every event keeps the stop number and the guest time (the engine's virtual
//! clock) of the stop it belongs to.
//!
//! The two sequences are aligned per conversation with Myers' diff on a key that leaves out what
//! only moves with time (a command's id and nonce, the stop numbers and times): two runs of one
//! image are deterministic, so they share every event up to the point where they part, and two
//! images that do the same things still align by what they do. Reported:
//!
//! - the first difference (the first place where the alignment has an event only one run has);
//! - the guest-time divergence (the first shared event the runs reach at another stop or time);
//! - the first difference in what the checks judge (acknowledged lists, assertions, history and
//!   fake-world records, verdicts), skipping records of a kind the other run never writes;
//! - the records of a kind only one run writes (an assertion id the other never reports);
//! - every difference counted by conversation and level.
//!
//! The document is [`hydo_results::Diff`] (`diff.json`); [`render`] is its view (`diff.txt`).

use crate::ctx::{die, read_text, strftime, take, Ctx, OUT};
use hydo_results::{Diff, DiffCount, DiffLine, DiffPoint, DiffSide, Divergence, Run};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The conversations in the order they happen; `result` is the run's result document.
pub const CONVERSATIONS: [&str; 4] = ["boot", "run", "recovery", "result"];

/// The levels whose records the checks judge (the first outcome difference looks at these; an
/// assertion only when it failed or when the other run has the same assertion there).
const OUTCOME_LEVELS: [&str; 7] = [
    "verdict",
    "assertion",
    "history",
    "fake-world",
    "error-log",
    "outcome",
    "composer",
];

/// Shared events shown before a divergence.
const CONTEXT: usize = 4;
/// Differing events shown per side of a divergence.
const SHOWN: usize = 10;
/// The longest text shown for one event.
const WIDTH: usize = 200;

/// One event of a run.
#[derive(Clone, Debug, PartialEq)]
pub struct Ev {
    pub conv: &'static str,
    pub stop: Option<u64>,
    pub at: Option<u64>,
    pub level: &'static str,
    /// What is compared: the event without what only moves with time.
    pub key: String,
    /// What is shown.
    pub text: String,
    /// Events of one family are paired inside a difference to say what differs between them.
    pub family: String,
    /// The kind of record, for records only one run writes (an assertion's id).
    pub vocab: Option<String>,
    /// The event's JSON (a command, an assertion record), for field details.
    pub json: Option<Value>,
    /// The host controller's last note before it in its conversation (`hydo-ob: KILLED redis
    /// before step 11`): where in the workload it happened.
    pub after: Option<String>,
}

/// One run read for a diff.
pub struct Loaded {
    pub dir: PathBuf,
    pub run: Run,
    pub events: Vec<Ev>,
    /// The sources found (`control`, `console`, `result`).
    pub levels: Vec<String>,
}

// ---------------------------------------------------------------- reading a run

fn conv_name(s: &str) -> Option<&'static str> {
    CONVERSATIONS.iter().copied().find(|c| *c == s)
}

fn cut(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        return s;
    }
    let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// A command as a person reads it: `spawn [workload] /bin/hydostack place $HYDO_FROM $HYDO_TO`.
fn argv_text(argv: &[Value]) -> String {
    let mut parts: Vec<String> = argv
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    loop {
        match parts.first().map(String::as_str) {
            Some("/bin/busybox") => {
                parts.remove(0);
            }
            Some("timeout") if parts.len() > 2 => {
                parts.drain(..2);
            }
            Some("sh") if parts.get(1).map(String::as_str) == Some("-c") && parts.len() > 2 => {
                parts = vec![parts[2].clone()];
                break;
            }
            _ => break,
        }
    }
    let s = parts.join(" ");
    let s = s
        .strip_prefix(". /etc/hydo/prelude.sh;")
        .unwrap_or(&s)
        .trim()
        .to_string();
    cut(&s, 120)
}

fn send_text(v: &Value) -> String {
    let op = v.get("op").and_then(Value::as_str).unwrap_or("?");
    let mut s = op.to_string();
    if let Some(h) = v.get("h").and_then(Value::as_str) {
        s.push_str(&format!(" [{h}]"));
    }
    let path = v.get("path").and_then(Value::as_str).unwrap_or("");
    match op {
        "signal" => {
            let sig = v.get("sig").and_then(Value::as_str).unwrap_or("?");
            s.push_str(&format!(" {sig}"));
        }
        "write" => {
            let n = v.get("data").and_then(Value::as_str).map_or(0, str::len);
            s.push_str(&format!(" {path} ({n} bytes)"));
        }
        "read" => s.push_str(&format!(" {path}")),
        _ => {
            if let Some(a) = v.get("argv").and_then(Value::as_array) {
                s.push_str(&format!(" {}", argv_text(a)));
            } else if !path.is_empty() {
                s.push_str(&format!(" {path}"));
            }
        }
    }
    if v.get("alive").is_some() {
        s.push_str(" (until ready)");
    }
    s
}

/// The level of a line the controller or the guest wrote.
fn line_level(line: &str, guest: bool) -> &'static str {
    let p = |x: &str| line.starts_with(x);
    if p("HYDO-ORACLE") {
        "verdict"
    } else if p("HYDO-ASSERT-DECL") {
        "assertion-decl"
    } else if p("HYDO-ASSERT") {
        "assertion"
    } else if p("HYDO-HIST") || p("HYDO-CHECK") {
        "history"
    } else if p("HYDO-ERRLOG") {
        "error-log"
    } else if p("hydo-world:") {
        "fake-world"
    } else if p("hydo-explore-acked:") || p("hydo-explore-failed:") {
        "outcome"
    } else if p("HYDO-CMD") || p("HYDO-COMPOSE") {
        "composer"
    } else if guest {
        "guest"
    } else {
        "controller"
    }
}

/// Digit runs as `#`, a list of numbers as one `#`: lines that differ only in numbers are of one
/// family.
fn number_family(s: &str) -> String {
    let mut out = String::new();
    let mut in_num = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !in_num {
                out.push('#');
            }
            in_num = true;
        } else {
            in_num = false;
            out.push(c);
        }
    }
    while out.contains("# #") {
        out = out.replace("# #", "#");
    }
    out.trim_end().to_string()
}

/// The JSON object after a record's prefix (`HYDO-ASSERT {...}`).
fn record_json(line: &str) -> Option<Value> {
    let i = line.find('{')?;
    serde_json::from_str::<Value>(&line[i..])
        .ok()
        .filter(Value::is_object)
}

fn line_event(
    conv: &'static str,
    stop: Option<u64>,
    at: Option<u64>,
    line: &str,
    guest: bool,
) -> Ev {
    let level = line_level(line, guest);
    let json = match level {
        "assertion" | "assertion-decl" | "history" => record_json(line),
        _ => None,
    };
    let id = json
        .as_ref()
        .and_then(|j| j.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let (family, vocab, text) = match (level, &id, &json) {
        ("assertion", Some(id), Some(j)) => {
            let ok = j.get("ok").map(|v| v.to_string()).unwrap_or_default();
            let msg = j.get("msg").and_then(Value::as_str).unwrap_or("");
            let det = j
                .get("details")
                .map(|d| format!(" {d}"))
                .unwrap_or_default();
            (
                format!("assert:{id}"),
                Some(format!("assertion {id}")),
                format!("assert {id} ok={ok}: {msg}{det}"),
            )
        }
        ("assertion-decl", Some(id), _) => (format!("decl:{id}"), None, line.to_string()),
        ("verdict", _, _) => ("verdict".to_string(), None, line.to_string()),
        _ => (number_family(line), None, line.to_string()),
    };
    Ev {
        conv,
        stop,
        at,
        level,
        // the controller's echo of the schedule's guest part differs whenever the schedules
        // do: it is compared as one event, and the report names both schedules
        key: if !guest && line.starts_with("hydo-ob: command: run") {
            "line\thydo-ob: command: run".to_string()
        } else {
            format!("{}\t{line}", if guest { "guest" } else { "line" })
        },
        text: cut(&text, WIDTH),
        family,
        vocab,
        json,
        after: None,
    }
}

fn jsonl(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// One conversation's events: its transcript's records with the guest's console lines between
/// them (`console`: the merged `<tag>.log`, or nothing).
pub fn conversation_events(conv: &'static str, transcript: &str, console: &str) -> Vec<Ev> {
    let lines: Vec<&str> = console.lines().collect();
    let mut cursor = 0;
    let mut out = Vec::new();
    let (mut stop, mut at): (Option<u64>, Option<u64>) = (None, None);
    let u = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    for r in jsonl(transcript) {
        let Some(t) = r.get("t").and_then(Value::as_str) else {
            continue; // the header
        };
        let n = u(&r, "n");
        if n != stop {
            at = None;
        }
        stop = n.or(stop);
        match t {
            "stop" => at = u(&r, "at"),
            "ack" => {}
            "note" | "report" => {
                let line = s(&r, "line").unwrap_or_default();
                if let Some(j) = lines[cursor..].iter().position(|l| *l == line) {
                    for l in &lines[cursor..cursor + j] {
                        out.push(line_event(conv, stop, at, l, true));
                    }
                    cursor += j + 1;
                }
                out.push(line_event(conv, stop, at, &line, false));
            }
            "send" => {
                let bytes = s(&r, "bytes").unwrap_or_default();
                let mut v: Value = serde_json::from_str(bytes.trim_start_matches('\u{18}'))
                    .unwrap_or_else(|_| Value::String(bytes.clone()));
                if let Some(o) = v.as_object_mut() {
                    o.remove("id");
                    o.remove("nonce");
                }
                let op = v
                    .get("op")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string();
                let h = v.get("h").and_then(Value::as_str).unwrap_or("").to_string();
                out.push(Ev {
                    conv,
                    stop,
                    at: u(&r, "at").or(at),
                    level: "control",
                    key: format!("send\t{v}"),
                    text: cut(&send_text(&v), WIDTH),
                    family: format!("send:{op}:{h}"),
                    vocab: None,
                    json: Some(v),
                    after: None,
                });
            }
            "engine" | "snap" | "end" => {
                let mut v = r.clone();
                if let Some(o) = v.as_object_mut() {
                    o.remove("n");
                    o.remove("t");
                }
                let text = match t {
                    "engine" => format!("engine {}", s(&r, "action").unwrap_or_default()),
                    "snap" => format!("snapshot {}", s(&r, "name").unwrap_or_default()),
                    _ => end_text(r.get("end").unwrap_or(&Value::Null)),
                };
                out.push(Ev {
                    conv,
                    stop,
                    at,
                    level: if t == "end" { "outcome" } else { "control" },
                    key: format!("{t}\t{v}"),
                    text: cut(&text, WIDTH),
                    family: t.to_string(),
                    vocab: None,
                    json: if t == "end" {
                        r.get("end").cloned()
                    } else {
                        None
                    },
                    after: None,
                });
            }
            _ => {}
        }
    }
    for l in &lines[cursor..] {
        out.push(line_event(conv, stop, at, l, true));
    }
    let mut last: Option<String> = None;
    for e in &mut out {
        e.after = last.clone();
        if e.level == "controller" && e.text.starts_with("hydo-ob:") {
            last = Some(e.text.clone());
        }
    }
    out
}

fn end_text(e: &Value) -> String {
    if let Some(v) = e.get("verdict").and_then(Value::as_str) {
        return format!("end: {v}");
    }
    if let Some(s) = e.get("snapshot").and_then(Value::as_str) {
        return format!("end at snapshot {s}");
    }
    if let Some(c) = e.get("cut") {
        let list = |k: &str| {
            c.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        };
        return format!(
            "end: power cut, acknowledged [{}], failed [{}]",
            list("acked"),
            list("failed")
        );
    }
    format!("end {e}")
}

/// The run's result as events: its verdict and its findings.
fn result_events(run: &Run) -> Vec<Ev> {
    let mut out = vec![Ev {
        conv: "result",
        stop: None,
        at: None,
        level: "verdict",
        key: format!("verdict\t{}\t{}", run.verdict.kind, run.verdict.text),
        text: cut(
            &format!("verdict {}: {}", run.verdict.kind, run.verdict.text),
            WIDTH,
        ),
        family: "verdict".into(),
        vocab: None,
        json: None,
        after: None,
    }];
    for f in &run.findings {
        let level = match f.kind.as_str() {
            "history-check" => "history",
            "assertion" => "assertion",
            "error-log" => "error-log",
            _ => "verdict",
        };
        let text = format!("finding {} {} {}: {}", f.kind, f.id, f.status, f.message);
        out.push(Ev {
            conv: "result",
            stop: None,
            at: None,
            level,
            key: format!("finding\t{text}"),
            text: cut(&text, WIDTH),
            family: format!("finding:{}:{}", f.kind, f.id),
            vocab: None,
            json: None,
            after: None,
        });
    }
    out
}

/// A run directory (`hydo run`, `hydo repro`): its result document, transcripts and consoles.
pub fn load(dir: &Path) -> Result<Loaded, String> {
    let run = hydo_results::reader::read_run(&dir.join(hydo_results::RUN_DOC))
        .map_err(|e| format!("{e} (hydo diff compares run directories: hydo run --out DIR)"))?;
    // the transcripts the result names (the run's own files when it names none)
    let mut files: BTreeMap<usize, PathBuf> = BTreeMap::new();
    for c in &run.control {
        if let (Some(t), Some(i)) = (
            &c.transcript,
            CONVERSATIONS.iter().position(|x| *x == c.conversation),
        ) {
            files.insert(i, dir.join(t));
        }
    }
    for (i, f) in [
        "control.boot.jsonl",
        "control.jsonl",
        "control.recovery.jsonl",
    ]
    .iter()
    .enumerate()
    {
        files.entry(i).or_insert_with(|| dir.join(f));
    }
    let consoles: Vec<String> = run
        .artifacts
        .as_ref()
        .map(|a| a.consoles.clone())
        .unwrap_or_default();
    let console_of = |conv: &str| -> Option<PathBuf> {
        let want_rec = conv == "recovery";
        consoles
            .iter()
            .find(|c| c.ends_with(".recovery.log") == want_rec && !c.ends_with(".agent.log"))
            .map(|c| dir.join(c))
            .or_else(|| {
                Some(dir.join(if want_rec {
                    "one.recovery.log"
                } else {
                    "one.log"
                }))
            })
    };
    let mut events = Vec::new();
    let mut levels = Vec::new();
    for (i, f) in &files {
        let Ok(t) = read_text(f) else { continue };
        let conv = jsonl(&t)
            .first()
            .and_then(|h| {
                h.get("conversation")
                    .and_then(Value::as_str)
                    .and_then(conv_name)
            })
            .unwrap_or(CONVERSATIONS[*i]);
        let console = match conv {
            "boot" => None,
            c => console_of(c).and_then(|p| read_text(&p).ok()),
        };
        if !levels.iter().any(|l| l == "control") {
            levels.push("control".to_string());
        }
        if console.is_some() && !levels.iter().any(|l| l == "console") {
            levels.push("console".to_string());
        }
        events.extend(conversation_events(
            conv,
            &t,
            console.as_deref().unwrap_or(""),
        ));
    }
    if events.is_empty() {
        // no transcripts (not an agent image's run): the consoles alone
        for conv in ["run", "recovery"] {
            if let Some(t) = console_of(conv).and_then(|p| read_text(&p).ok()) {
                if !levels.iter().any(|l| l == "console") {
                    levels.push("console".to_string());
                }
                events.extend(conversation_events(conv, "", &t));
            }
        }
    }
    events.extend(result_events(&run));
    levels.push("result".into());
    Ok(Loaded {
        dir: dir.to_path_buf(),
        run,
        events,
        levels,
    })
}

// ---------------------------------------------------------------- the alignment

/// One step of an alignment: an event both have, or one only A or only B has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq(usize, usize),
    Del(usize),
    Ins(usize),
}

/// The largest edit distance followed exactly; beyond it the middle counts as replaced.
const MAX_D: usize = 2000;

/// Myers' O(ND) diff of two key sequences (with the common prefix and suffix taken first).
pub fn align(a: &[&str], b: &[&str]) -> Vec<Op> {
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let mut ops: Vec<Op> = (0..pre).map(|i| Op::Eq(i, i)).collect();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    for op in myers(am, bm) {
        ops.push(match op {
            Op::Eq(i, j) => Op::Eq(i + pre, j + pre),
            Op::Del(i) => Op::Del(i + pre),
            Op::Ins(j) => Op::Ins(j + pre),
        });
    }
    let (ea, eb) = (a.len() - suf, b.len() - suf);
    ops.extend((0..suf).map(|k| Op::Eq(ea + k, eb + k)));
    ops
}

fn myers(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n == 0 || m == 0 {
        return (0..a.len())
            .map(Op::Del)
            .chain((0..b.len()).map(Op::Ins))
            .collect();
    }
    let max = (n + m) as usize;
    let off = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    // trace[d]: v[k] for k in -(d+1)..=(d+1) at the start of round d
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let idx = |k: isize| (k + off) as usize;
    for d in 0..=(max.min(MAX_D) as isize) {
        trace.push(v[idx(-d - 1)..=idx(d + 1)].to_vec());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) {
                v[idx(k + 1)]
            } else {
                v[idx(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx(k)] = x;
            if x >= n && y >= m {
                return backtrack(&trace, n, m);
            }
            k += 2;
        }
    }
    // too far apart to follow: the middle is replaced
    (0..a.len())
        .map(Op::Del)
        .chain((0..b.len()).map(Op::Ins))
        .collect()
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let t = &trace[d as usize];
        let get = |k: isize| t[(k + d + 1) as usize];
        let k = x - y;
        let pk = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let px = get(pk);
        let py = px - pk;
        while x > px && y > py && x > 0 && y > 0 {
            ops.push(Op::Eq((x - 1) as usize, (y - 1) as usize));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == px {
                ops.push(Op::Ins((y - 1) as usize));
            } else {
                ops.push(Op::Del((x - 1) as usize));
            }
        }
        x = px;
        y = py;
    }
    ops.reverse();
    ops
}

// ---------------------------------------------------------------- the diff

fn point(e: &Ev) -> DiffPoint {
    DiffPoint {
        conversation: e.conv.to_string(),
        stop: e.stop,
        at: e.at,
    }
}

fn line_of(e: &Ev, a: Option<&Ev>, b: Option<&Ev>) -> DiffLine {
    DiffLine {
        level: e.level.to_string(),
        text: e.text.clone(),
        a: a.map(point),
        b: b.map(point),
        after: e.after.clone(),
    }
}

/// What differs between two events of one family.
fn details(x: &Ev, y: &Ev) -> Vec<String> {
    let label = match x.level {
        "control" => x.text.split(' ').next().unwrap_or("").to_string(),
        "assertion" => x.family.trim_start_matches("assert:").to_string(),
        _ => x
            .text
            .split_whitespace()
            .take(if x.text.starts_with("hydo-ob:") { 3 } else { 1 })
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches(':')
            .to_string(),
    };
    let mut out = Vec::new();
    match (&x.json, &y.json) {
        (Some(a), Some(b)) => json_details("", a, b, &mut out),
        _ => {
            if let Some(t) = token_details(&x.text, &y.text) {
                out.push(t);
            }
        }
    }
    out.into_iter().map(|d| format!("{label}: {d}")).collect()
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

fn only(a: &[String], b: &[String]) -> Option<String> {
    let oa: Vec<&String> = a.iter().filter(|x| !b.contains(x)).collect();
    let ob: Vec<&String> = b.iter().filter(|x| !a.contains(x)).collect();
    let j = |v: &[&String]| {
        cut(
            &v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "),
            160,
        )
    };
    match (oa.is_empty(), ob.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("only in A: {}", j(&oa))),
        (true, false) => Some(format!("only in B: {}", j(&ob))),
        (false, false) => Some(format!("only in A: {}; only in B: {}", j(&oa), j(&ob))),
    }
}

fn token_details(a: &str, b: &str) -> Option<String> {
    let ta: Vec<String> = a.split_whitespace().map(str::to_string).collect();
    let tb: Vec<String> = b.split_whitespace().map(str::to_string).collect();
    only(&ta, &tb)
}

fn json_details(at: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    if a == b || out.len() >= 6 {
        return;
    }
    let name = |k: &str| {
        if at.is_empty() {
            k.to_string()
        } else {
            format!("{at}.{k}")
        }
    };
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().collect();
            keys.extend(y.keys().filter(|k| !x.contains_key(*k)));
            for k in keys {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => json_details(&name(k), p, q, out),
                    (Some(p), None) => {
                        out.push(format!("{} only in A: {}", name(k), cut(&scalar(p), 120)))
                    }
                    (None, Some(q)) => {
                        out.push(format!("{} only in B: {}", name(k), cut(&scalar(q), 120)))
                    }
                    (None, None) => {}
                }
            }
        }
        (Value::Array(x), Value::Array(y))
            if x.iter().chain(y).all(|v| !v.is_object() && !v.is_array()) =>
        {
            let sa: Vec<String> = x.iter().map(scalar).collect();
            let sb: Vec<String> = y.iter().map(scalar).collect();
            match only(&sa, &sb) {
                Some(d) => out.push(format!("{}: {d}", if at.is_empty() { "list" } else { at })),
                None => out.push(format!("{}: the same items in another order", at)),
            }
        }
        (Value::String(x), Value::String(y))
            if x.contains(char::is_whitespace) || y.contains(char::is_whitespace) =>
        {
            match token_details(x, y) {
                Some(d) => out.push(format!("{at}: {d}")),
                None => out.push(format!("{at}: A {} B {}", cut(x, 80), cut(y, 80))),
            }
        }
        _ => out.push(format!(
            "{}: A {} B {}",
            if at.is_empty() { "value" } else { at },
            cut(&scalar(a), 80),
            cut(&scalar(b), 80)
        )),
    }
}

/// One place where the alignment has events only one side has.
struct Hunk {
    conv: &'static str,
    /// The index into the alignment of its first op.
    start: usize,
    dels: Vec<usize>,
    ins: Vec<usize>,
}

fn hunks(ops: &[Op], ea: &[Ev], eb: &[Ev]) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::new();
    let mut open = false;
    for (i, op) in ops.iter().enumerate() {
        let conv = match op {
            Op::Eq(..) => {
                open = false;
                continue;
            }
            Op::Del(x) => ea[*x].conv,
            Op::Ins(y) => eb[*y].conv,
        };
        if !open || out.last().is_some_and(|h| h.conv != conv) {
            out.push(Hunk {
                conv,
                start: i,
                dels: Vec::new(),
                ins: Vec::new(),
            });
            open = true;
        }
        let h = out.last_mut().expect("a hunk was opened");
        match op {
            Op::Del(x) => h.dels.push(*x),
            Op::Ins(y) => h.ins.push(*y),
            Op::Eq(..) => {}
        }
    }
    out
}

/// Builds the divergence of the events `dels`/`ins` (a hunk, or the part of one that counts).
fn divergence(
    conv: &str,
    start: usize,
    dels: &[usize],
    ins: &[usize],
    ops: &[Op],
    ea: &[Ev],
    eb: &[Ev],
) -> Divergence {
    // the shared events before it, and the shared event after it (where the other side is)
    let mut context = Vec::new();
    for op in ops[..start].iter().rev() {
        if let Op::Eq(x, y) = op {
            if ea[*x].conv != conv || context.len() == CONTEXT {
                break;
            }
            context.push(line_of(&ea[*x], Some(&ea[*x]), Some(&eb[*y])));
        }
    }
    context.reverse();
    let next = ops[start..].iter().find_map(|op| match op {
        Op::Eq(x, y) => Some((*x, *y)),
        _ => None,
    });
    let a = dels
        .first()
        .map(|x| point(&ea[*x]))
        .or_else(|| next.map(|(x, _)| point(&ea[x])));
    let b = ins
        .first()
        .map(|y| point(&eb[*y]))
        .or_else(|| next.map(|(_, y)| point(&eb[y])));
    let first = match (dels.first(), ins.first()) {
        (Some(x), Some(y)) => {
            // the earlier of the two, by position in the alignment
            let px = ops.iter().position(|o| *o == Op::Del(*x));
            let py = ops.iter().position(|o| *o == Op::Ins(*y));
            if px <= py {
                &ea[*x]
            } else {
                &eb[*y]
            }
        }
        (Some(x), None) => &ea[*x],
        (None, Some(y)) => &eb[*y],
        (None, None) => unreachable!("a divergence has an event"),
    };
    let mut lines: Vec<DiffLine> = dels
        .iter()
        .take(SHOWN)
        .map(|x| line_of(&ea[*x], Some(&ea[*x]), None))
        .collect();
    lines.extend(
        ins.iter()
            .take(SHOWN)
            .map(|y| line_of(&eb[*y], None, Some(&eb[*y]))),
    );
    // pairs of one family: what differs inside them
    let mut used = vec![false; ins.len()];
    let mut det = Vec::new();
    for x in dels {
        if let Some(k) = (0..ins.len()).find(|k| !used[*k] && eb[ins[*k]].family == ea[*x].family) {
            used[k] = true;
            det.extend(details(&ea[*x], &eb[ins[k]]));
        }
        if det.len() >= 8 {
            break;
        }
    }
    if dels.len() > SHOWN {
        det.push(format!("and {} more events only in A", dels.len() - SHOWN));
    }
    if ins.len() > SHOWN {
        det.push(format!("and {} more events only in B", ins.len() - SHOWN));
    }
    Divergence {
        conversation: conv.to_string(),
        level: first.level.to_string(),
        a,
        b,
        context,
        lines,
        details: det,
    }
}

fn side(l: &Loaded) -> DiffSide {
    DiffSide {
        dir: l.dir.display().to_string(),
        schedule: l.run.schedule.clone(),
        image_sha256: l.run.image.as_ref().and_then(|i| i.sha256.clone()),
        verdict: l.run.verdict.clone(),
        fingerprints: l.run.fingerprints.clone(),
    }
}

/// The diff of two loaded runs.
pub fn diff(a: &Loaded, b: &Loaded) -> Diff {
    let (ea, eb) = (&a.events, &b.events);
    // align conversation by conversation, in their order
    let mut ops = Vec::new();
    for conv in CONVERSATIONS {
        let ia: Vec<usize> = (0..ea.len()).filter(|i| ea[*i].conv == conv).collect();
        let ib: Vec<usize> = (0..eb.len()).filter(|i| eb[*i].conv == conv).collect();
        let ka: Vec<&str> = ia.iter().map(|i| ea[*i].key.as_str()).collect();
        let kb: Vec<&str> = ib.iter().map(|i| eb[*i].key.as_str()).collect();
        for op in align(&ka, &kb) {
            ops.push(match op {
                Op::Eq(x, y) => Op::Eq(ia[x], ib[y]),
                Op::Del(x) => Op::Del(ia[x]),
                Op::Ins(y) => Op::Ins(ib[y]),
            });
        }
    }
    let hs = hunks(&ops, ea, eb);
    // records of a kind the other run never writes
    let vocab = |es: &[Ev]| -> Vec<String> { es.iter().filter_map(|e| e.vocab.clone()).collect() };
    let (va, vb) = (vocab(ea), vocab(eb));
    let one_sided_a = |e: &Ev| e.vocab.as_ref().is_some_and(|v| !vb.contains(v));
    let one_sided_b = |e: &Ev| e.vocab.as_ref().is_some_and(|v| !va.contains(v));
    let mut one_sided = Vec::new();
    let mut seen_kinds: Vec<String> = Vec::new();
    for op in &ops {
        let (e, l) = match op {
            Op::Del(x) if one_sided_a(&ea[*x]) => (&ea[*x], line_of(&ea[*x], Some(&ea[*x]), None)),
            Op::Ins(y) if one_sided_b(&eb[*y]) => (&eb[*y], line_of(&eb[*y], None, Some(&eb[*y]))),
            _ => continue,
        };
        let k = e.vocab.clone().unwrap_or_default();
        if !seen_kinds.contains(&k) {
            seen_kinds.push(k);
            one_sided.push(l);
        }
    }
    // the first difference: the first place with an event of a kind both runs write (the
    // records of a kind only one run writes are listed on their own)
    let first = hs.iter().find_map(|h| {
        let dels: Vec<usize> = h
            .dels
            .iter()
            .copied()
            .filter(|x| !one_sided_a(&ea[*x]))
            .collect();
        let ins: Vec<usize> = h
            .ins
            .iter()
            .copied()
            .filter(|y| !one_sided_b(&eb[*y]))
            .collect();
        if dels.is_empty() && ins.is_empty() {
            return None;
        }
        Some(divergence(h.conv, h.start, &dels, &ins, &ops, ea, eb))
    });
    // the first difference in a judged value: a record both runs write with another content
    // here (a pair of one family), a failing assertion, a verdict, the acknowledged lists, a
    // history, fake-world or error-log record; an extra passing evaluation of a check one run
    // made at another time is not one
    let first_outcome = hs.iter().find_map(|h| {
        let fam_b: Vec<&str> = h.ins.iter().map(|y| eb[*y].family.as_str()).collect();
        let fam_a: Vec<&str> = h.dels.iter().map(|x| ea[*x].family.as_str()).collect();
        let judged = |e: &Ev, others: &[&str]| {
            OUTCOME_LEVELS.contains(&e.level)
                && (e.level != "assertion"
                    || others.contains(&e.family.as_str())
                    || e.json.as_ref().and_then(|j| j.get("ok")) != Some(&Value::Bool(true)))
        };
        let dels: Vec<usize> = h
            .dels
            .iter()
            .copied()
            .filter(|x| !one_sided_a(&ea[*x]) && judged(&ea[*x], &fam_b))
            .collect();
        let ins: Vec<usize> = h
            .ins
            .iter()
            .copied()
            .filter(|y| !one_sided_b(&eb[*y]) && judged(&eb[*y], &fam_a))
            .collect();
        if dels.is_empty() && ins.is_empty() {
            return None;
        }
        Some(divergence(h.conv, h.start, &dels, &ins, &ops, ea, eb))
    });
    // the first shared event at another stop or guest time
    let mut timing = None;
    let mut prev: Option<(usize, usize)> = None;
    for op in &ops {
        let Op::Eq(x, y) = op else { continue };
        let (p, q) = (&ea[*x], &eb[*y]);
        if p.conv == "result" {
            break;
        }
        if (p.stop, p.at) != (q.stop, q.at) {
            timing = Some(Divergence {
                conversation: p.conv.to_string(),
                level: "timing".into(),
                a: Some(point(p)),
                b: Some(point(q)),
                context: prev
                    .map(|(i, j)| vec![line_of(&ea[i], Some(&ea[i]), Some(&eb[j]))])
                    .unwrap_or_default(),
                lines: vec![line_of(p, Some(p), Some(q))],
                details: Vec::new(),
            });
            break;
        }
        prev = Some((*x, *y));
    }
    // the counts
    let mut counts: Vec<DiffCount> = Vec::new();
    for op in &ops {
        let (e, is_a) = match op {
            Op::Del(x) => (&ea[*x], true),
            Op::Ins(y) => (&eb[*y], false),
            Op::Eq(..) => continue,
        };
        let i = match counts
            .iter()
            .position(|c| c.conversation == e.conv && c.level == e.level)
        {
            Some(i) => i,
            None => {
                counts.push(DiffCount {
                    conversation: e.conv.to_string(),
                    level: e.level.to_string(),
                    only_a: 0,
                    only_b: 0,
                });
                counts.len() - 1
            }
        };
        if is_a {
            counts[i].only_a += 1;
        } else {
            counts[i].only_b += 1;
        }
    }
    let mut levels = a.levels.clone();
    for l in &b.levels {
        if !levels.contains(l) {
            levels.push(l.clone());
        }
    }
    Diff {
        hydo_version: Some(env!("CARGO_PKG_VERSION").into()),
        a: side(a),
        b: side(b),
        neighbour: None,
        levels,
        identical: hs.is_empty(),
        first,
        timing,
        first_outcome,
        one_sided,
        counts,
        hunks: hs.len() as u64,
        ..Diff::default()
    }
}

// ---------------------------------------------------------------- the view

fn short(h: &Option<String>) -> String {
    h.as_deref()
        .map(|s| format!("{}…", &s[..s.len().min(12)]))
        .unwrap_or_else(|| "unknown".into())
}

fn pt(p: &Option<DiffPoint>) -> String {
    match p {
        Some(DiffPoint {
            stop: Some(s),
            at: Some(t),
            ..
        }) => format!("stop {s}, guest time {t}"),
        Some(DiffPoint {
            stop: Some(s),
            at: None,
            ..
        }) => format!("stop {s}"),
        _ => "the end of the run".into(),
    }
}

fn where_(d: &Divergence) -> String {
    if d.conversation == "result" {
        return "in the runs' results".into();
    }
    if d.a == d.b {
        format!(
            "in the {} conversation, at {} in both runs",
            d.conversation,
            pt(&d.a)
        )
    } else {
        format!(
            "in the {} conversation, at {} in A and {} in B",
            d.conversation,
            pt(&d.a),
            pt(&d.b)
        )
    }
}

fn stop_of(p: Option<&DiffPoint>) -> String {
    p.and_then(|p| p.stop)
        .map(|s| format!("stop {s}"))
        .unwrap_or_default()
}

/// One event as a row: `=` shared (with both stops when they differ), `-` only A, `+` only B.
fn row(sign: &str, l: &DiffLine) -> String {
    let p = if sign == "+" {
        l.b.as_ref()
    } else {
        l.a.as_ref()
    };
    let conv = p.map(|p| p.conversation.as_str()).unwrap_or("");
    let at = match (sign, l.a.as_ref(), l.b.as_ref()) {
        ("=", Some(x), Some(y)) if x.stop != y.stop => {
            format!("A {} | B {}", stop_of(Some(x)), stop_of(Some(y)))
        }
        _ => stop_of(p),
    };
    format!(
        "    {sign} {conv:<8} {at:>12}  {:<14} {}\n",
        l.level, l.text
    )
}

/// Rows of events one run has, each group under the controller's note it came after.
fn rows(s: &mut String, sign: &str, ls: &[&DiffLine]) {
    let mut last: Option<&Option<String>> = None;
    for l in ls {
        if last != Some(&l.after) {
            if let Some(a) = &l.after {
                s.push_str(&format!("      (after {a})\n"));
            }
            last = Some(&l.after);
        }
        s.push_str(&row(sign, l));
    }
}

fn render_divergence(s: &mut String, d: &Divergence) {
    s.push_str(&format!("  {}; level {}.\n", where_(d), d.level));
    if !d.context.is_empty() {
        s.push_str("  The events both runs share right before it:\n");
        for l in &d.context {
            s.push_str(&row("=", l));
        }
    }
    let (la, lb): (Vec<&DiffLine>, Vec<&DiffLine>) = d.lines.iter().partition(|l| l.a.is_some());
    if !la.is_empty() {
        s.push_str("  Only in A:\n");
        rows(s, "-", &la);
    }
    if !lb.is_empty() {
        s.push_str("  Only in B:\n");
        rows(s, "+", &lb);
    }
    for x in &d.details {
        s.push_str(&format!("  What differs: {x}\n"));
    }
}

/// `diff.txt`: the diff for people.
pub fn render(d: &Diff) -> String {
    let mut s = String::new();
    for (n, x) in [("A", &d.a), ("B", &d.b)] {
        s.push_str(&format!("{n}: {}\n", x.dir));
        s.push_str(&format!("   schedule  {}\n", x.schedule));
        s.push_str(&format!(
            "   verdict   {} (fp={}{})\n",
            x.verdict.text,
            x.fingerprints.fp,
            x.fingerprints
                .ctl
                .as_ref()
                .map(|c| format!(" ctl={c}"))
                .unwrap_or_default()
        ));
    }
    if d.a.image_sha256 == d.b.image_sha256 {
        s.push_str(&format!(
            "Both runs booted the same image (sha256 {}).\n",
            short(&d.a.image_sha256)
        ));
    } else {
        s.push_str(&format!(
            "The runs booted different images (A sha256 {}, B sha256 {}): lines one image's code never writes are expected.\n",
            short(&d.a.image_sha256),
            short(&d.b.image_sha256)
        ));
    }
    if let Some(nb) = &d.neighbour {
        s.push_str(&format!("B's schedule is A's {nb}.\n"));
    }
    s.push_str(&format!("Compared: {}.\n", d.levels.join(", ")));
    if d.identical {
        if d.a.fingerprints.fp == d.b.fingerprints.fp {
            s.push_str("\nThe two runs do not differ at any level compared, and their fingerprints are equal: they are the same run.\n");
        } else {
            s.push_str(&format!(
                "\nThe two runs do not differ at any level compared, but their fingerprints differ \
                 (A fp={}, B fp={}): the difference is in what these levels do not record (the \
                 disk's contents, guest memory, timing inside a stop).\n",
                d.a.fingerprints.fp, d.b.fingerprints.fp
            ));
        }
        return s;
    }
    if let Some(f) = &d.first {
        s.push_str("\nThe first difference:\n");
        render_divergence(&mut s, f);
    }
    s.push_str("\nGuest time:\n");
    match &d.timing {
        None => s.push_str(
            "  Every event both runs share happens at the same stop and guest time in both.\n",
        ),
        Some(t) => {
            match t.context.first() {
                None => s.push_str("  The runs are at different stops or guest times from their first shared event on.\n"),
                Some(c) => s.push_str(&format!(
                    "  The runs share their stops and guest times up to {} in the {} conversation ({}).\n",
                    pt(&c.a), c.a.as_ref().map(|p| p.conversation.as_str()).unwrap_or("?"), c.text
                )),
            }
            if let Some(l) = t.lines.first() {
                s.push_str(&format!(
                    "  The next shared event, {}, is at {} in A and at {} in B.\n",
                    l.text,
                    pt(&t.a),
                    pt(&t.b)
                ));
            }
        }
    }
    match (&d.first_outcome, &d.first) {
        (Some(o), Some(f)) if o == f => s.push_str(
            "\nThe first difference in what the checks judge is the first difference itself.\n",
        ),
        (Some(o), _) => {
            s.push_str("\nThe first difference in what the checks judge (acknowledged lists, assertions, history and fake-world records, verdicts):\n");
            render_divergence(&mut s, o);
        }
        (None, _) => s.push_str("\nThe runs do not differ in what the checks judge.\n"),
    }
    if !d.one_sided.is_empty() {
        s.push_str("\nRecords of a kind only one run writes (code only one image has, or a fault only one schedule has), first occurrence:\n");
        let (la, lb): (Vec<&DiffLine>, Vec<&DiffLine>) =
            d.one_sided.iter().partition(|l| l.a.is_some());
        rows(&mut s, "-", &la);
        rows(&mut s, "+", &lb);
    }
    s.push_str(&format!("\nAll differences: {} places.", d.hunks));
    for c in &d.counts {
        s.push_str(&format!(
            "\n  {:<9} {:<15} {:>4} only in A, {:>4} only in B",
            c.conversation, c.level, c.only_a, c.only_b
        ));
    }
    s.push('\n');
    s
}

// ---------------------------------------------------------------- the command

/// Writes `diff.json` and `diff.txt` into `out`.
pub fn write(out: &Path, d: &Diff) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    hydo_results::write_document(&out.join(hydo_results::DIFF_DOC), d)?;
    std::fs::write(out.join("diff.txt"), render(d))
}

pub fn new_out_dir(kind: &str) -> PathBuf {
    Path::new(OUT).join(format!(
        "{kind}-{}-{}",
        strftime("%Y%m%d-%H%M%S"),
        std::process::id()
    ))
}

const USAGE: &str = "usage: hydo diff <run dir A> <run dir B> [--out DIR]
    where two runs first differ: their control transcripts (the host's commands, notes and
    reports, with stop numbers and guest time), the guest's console lines and their results
    (docs/design/explain.md). Writes diff.json (the results format) and diff.txt into DIR
    (default hydo-out/diff-<time>-<pid>). Give the failing run as A.";

/// `hydo diff <run dir A> <run dir B> [--out DIR]`.
pub fn cmd_diff(_ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let out = take(&mut args, "--out").map(PathBuf::from);
    if args.len() != 2 {
        eprintln!("{USAGE}");
        return 2;
    }
    let a = load(Path::new(&args[0])).unwrap_or_else(|e| die(&e));
    let b = load(Path::new(&args[1])).unwrap_or_else(|e| die(&e));
    let d = diff(&a, &b);
    print!("{}", render(&d));
    let out = out.unwrap_or_else(|| new_out_dir("diff"));
    if let Err(e) = write(&out, &d) {
        die(&format!("{}: {e}", out.display()));
    }
    println!(
        "hydo: the diff is in {}",
        out.join(hydo_results::DIFF_DOC).display()
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[&str], b: &[&str], ops: &[Op]) {
        // the alignment covers both sequences in order, and Eq pairs are equal
        let (mut i, mut j) = (0, 0);
        for op in ops {
            match op {
                Op::Eq(x, y) => {
                    assert_eq!((*x, *y), (i, j));
                    assert_eq!(a[*x], b[*y]);
                    i += 1;
                    j += 1;
                }
                Op::Del(x) => {
                    assert_eq!(*x, i);
                    i += 1;
                }
                Op::Ins(y) => {
                    assert_eq!(*y, j);
                    j += 1;
                }
            }
        }
        assert_eq!((i, j), (a.len(), b.len()));
    }

    fn edits(ops: &[Op]) -> usize {
        ops.iter().filter(|o| !matches!(o, Op::Eq(..))).count()
    }

    #[test]
    fn myers_finds_a_shortest_alignment() {
        let cases: [(&str, &str, usize); 7] = [
            ("abcabba", "cbabac", 5),
            ("", "", 0),
            ("abc", "", 3),
            ("", "xy", 2),
            ("abcdef", "abcdef", 0),
            ("abcdef", "abXdef", 2),
            ("aaaa", "aaba", 2),
        ];
        for (a, b, d) in cases {
            let va: Vec<String> = a.chars().map(String::from).collect();
            let vb: Vec<String> = b.chars().map(String::from).collect();
            let ka: Vec<&str> = va.iter().map(String::as_str).collect();
            let kb: Vec<&str> = vb.iter().map(String::as_str).collect();
            let ops = align(&ka, &kb);
            apply(&ka, &kb, &ops);
            assert_eq!(edits(&ops), d, "{a} / {b}");
        }
        // a pseudo-random pair: still a valid alignment
        let mut x = 7u64;
        let mut gen = |n: usize| -> Vec<String> {
            (0..n)
                .map(|_| {
                    x = x
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    ((x >> 60) % 4).to_string()
                })
                .collect()
        };
        let (va, vb) = (gen(300), gen(280));
        let ka: Vec<&str> = va.iter().map(String::as_str).collect();
        let kb: Vec<&str> = vb.iter().map(String::as_str).collect();
        apply(&ka, &kb, &align(&ka, &kb));
    }

    const HEADER: &str = r#"{"cadence":"rb=16","control":1,"controller":"schedule","conversation":"run","schedule":"x","start":"resume S1"}"#;

    fn transcript(sends: &[(u64, u64, &str)], notes: &[(u64, &str)]) -> String {
        // records sorted by stop: stop, then its sends, then its notes
        let mut stops: Vec<(u64, u64)> = sends.iter().map(|(n, at, _)| (*n, *at)).collect();
        stops.sort();
        stops.dedup();
        let mut s = format!("{HEADER}\n");
        for (n, at) in stops {
            s.push_str(&format!(
                "{{\"at\":{at},\"n\":{n},\"seen\":1,\"t\":\"stop\",\"why\":\"hostcall\"}}\n"
            ));
            for (sn, sat, b) in sends {
                if *sn == n {
                    let bytes = serde_json::to_string(b).unwrap();
                    s.push_str(&format!(
                        "{{\"at\":{sat},\"by\":\"main\",\"bytes\":{bytes},\"id\":9,\"n\":{n},\"t\":\"send\"}}\n"
                    ));
                }
            }
            for (nn, l) in notes {
                if *nn == n {
                    let line = serde_json::to_string(l).unwrap();
                    s.push_str(&format!(
                        "{{\"by\":\"main\",\"line\":{line},\"n\":{n},\"t\":\"note\"}}\n"
                    ));
                }
            }
        }
        s
    }

    fn loaded(dir: &str, events: Vec<Ev>, verdict: &str) -> Loaded {
        Loaded {
            dir: PathBuf::from(dir),
            run: Run {
                schedule: "disk=none | run".into(),
                verdict: hydo_results::Verdict {
                    kind: verdict.split(':').next().unwrap().to_lowercase(),
                    text: verdict.into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            events,
            levels: vec!["control".into(), "console".into(), "result".into()],
        }
    }

    #[test]
    fn the_first_difference_the_timing_and_the_outcome() {
        let sync = r#"{"id":1,"nonce":100,"op":"sync","v":1}"#;
        let work_a = r#"{"argv":["/bin/busybox","sh","-c",". /etc/hydo/prelude.sh; place"],"env":["HYDO_TO=11"],"id":2,"op":"spawn","v":1}"#;
        let work_b = r#"{"argv":["/bin/busybox","sh","-c",". /etc/hydo/prelude.sh; place"],"env":["HYDO_TO=24"],"id":2,"op":"spawn","v":1}"#;
        let kill = r#"{"h":"redis","id":3,"op":"signal","sig":"KILL","v":1}"#;
        let ta = transcript(
            &[(5, 100, sync), (5, 100, work_a), (9, 300, kill)],
            &[
                (5, "hydo-ob: command: run"),
                (9, "hydo-ob: workload steps 0..10 rc=0"),
                (9, "hydo-explore-acked: 0 1 2 10 12 "),
            ],
        );
        let tb = transcript(
            &[(5, 100, sync), (5, 100, work_b)],
            &[
                (5, "hydo-ob: command: run"),
                (12, "hydo-ob: workload steps 0..23 rc=0"),
            ],
        );
        // B's notes at stop 12 need their stop record
        let tb = tb
            + "{\"at\":400,\"n\":12,\"seen\":2,\"t\":\"stop\",\"why\":\"hostcall\"}\n"
            + "{\"by\":\"main\",\"line\":\"hydo-ob: workload steps 0..23 rc=0\",\"n\":12,\"t\":\"note\"}\n"
            + "{\"by\":\"main\",\"line\":\"hydo-explore-acked: 0 1 2 10 11 12 \",\"n\":12,\"t\":\"report\"}\n";
        let ca = "hydo-ob: command: run\nhydo-ob: workload steps 0..10 rc=0\nhydo-explore-acked: 0 1 2 10 12 \n";
        let cb = "hydo-ob: command: run\nHYDO-ASSERT {\"t\":\"sometimes\",\"id\":\"server.cache_write_skipped\",\"ok\":true,\"msg\":\"skipped\",\"details\":{\"order\":11}}\nhydo-ob: workload steps 0..23 rc=0\nhydo-explore-acked: 0 1 2 10 11 12 \n";
        let a = loaded(
            "a",
            conversation_events("run", &ta, ca),
            "HYDO-ORACLE: VIOLATION lost",
        );
        let b = loaded("b", conversation_events("run", &tb, cb), "HYDO-ORACLE: OK");
        // the guest's line got the stop of the report after it
        let g = b.events.iter().find(|e| e.level == "assertion").unwrap();
        assert_eq!((g.stop, g.at), (Some(12), Some(400)));
        let mut a = a;
        let mut b = b;
        a.events.extend(result_events(&a.run));
        b.events.extend(result_events(&b.run));
        let d = diff(&a, &b);
        assert!(!d.identical);
        let f = d.first.as_ref().unwrap();
        assert_eq!(f.conversation, "run");
        assert_eq!(f.level, "control");
        assert_eq!(f.a.as_ref().unwrap().stop, Some(5));
        assert!(
            f.details
                .iter()
                .any(|x| x.contains("only in A: HYDO_TO=11; only in B: HYDO_TO=24")),
            "{:?}",
            f.details
        );
        // the shared sync is the context: its nonce and id did not count
        assert_eq!(f.context.len(), 1);
        let o = d.first_outcome.as_ref().unwrap();
        assert_eq!(o.level, "outcome");
        assert!(
            o.details.iter().any(|x| x.ends_with("only in B: 11")),
            "{:?}",
            o.details
        );
        // the one-sided assertion is listed, and is not the first outcome difference
        assert_eq!(d.one_sided.len(), 1);
        assert!(d.one_sided[0].text.contains("server.cache_write_skipped"));
        assert!(d.timing.is_none(), "every shared event at the same stop");
        let text = render(&d);
        assert!(text.contains("The first difference:"), "{text}");
        assert!(text.contains("only in B: 11"), "{text}");
        // a run against itself
        let same = diff(&a, &a);
        assert!(same.identical && same.first.is_none());
        assert!(render(&same).contains("do not differ"));
    }

    #[test]
    fn number_families_pair_lines_that_differ_in_numbers() {
        assert_eq!(
            number_family("hydo-explore-acked: 0 1 2 10 12 "),
            "hydo-explore-acked: #"
        );
        assert_eq!(
            number_family("hydo-ob: redis ready after 1 polls"),
            number_family("hydo-ob: redis ready after 0 polls")
        );
        assert_eq!(
            argv_text(&serde_json::from_str::<Vec<Value>>(
                r#"["/bin/busybox","timeout","300","/bin/busybox","sh","-c",". /etc/hydo/prelude.sh; /bin/hydostack place $HYDO_FROM $HYDO_TO"]"#
            )
            .unwrap()),
            "/bin/hydostack place $HYDO_FROM $HYDO_TO"
        );
    }

    #[test]
    fn a_document_reads_back() {
        let d = Diff {
            identical: true,
            ..Diff::default()
        };
        let text = hydo_results::to_document(&d);
        match hydo_results::reader::document(&text).unwrap() {
            hydo_results::reader::Record::Diff(x) => assert_eq!(*x, d),
            r => panic!("{r:?}"),
        }
    }
}
