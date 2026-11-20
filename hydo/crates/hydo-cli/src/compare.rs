//! `hydo compare-versions` (Milestone 298, docs/design/differential.md): does a new version of
//! the system behave as the old one did?
//!
//! Two versions of a system (made as `hydo versions` makes them: a manifest parameter, two git
//! revisions with a build command, or two images) run the same schedules: the given ones, the
//! saved regressions, or schedules drawn from seeds as `hydo explore` draws them. hydo repeats a
//! run exactly, so the two runs of one schedule differ only where the versions differ, never by
//! chance. Each pair of runs is compared at three levels, each optional:
//!
//! 1. `verdicts`: the verdict's kind and check, and each finding of `result.json`;
//! 2. `observations`: what the workload's client saw: the lines of `$HYDO_OUTCOMES` (the agent's
//!    reads of the file, in the control transcripts and the agent logs), the acknowledged and
//!    failed lists (`hydo-explore-acked:`, `hydo-explore-failed:`), and the `HYDO-HIST` records
//!    of the history and messages checks, per history and process, without their guest times;
//! 3. `state`: what the system stored: each service's `compare:` dump (`HYDO-STATE` notes,
//!    `hydo_control::schedule::dump`) and what each check printed (`HYDO-ASSERT` records'
//!    output and `HYDO-CHECK-OUT` notes).
//!
//! Normalization rules (`--ignore`, `--ignore-json`, `hydo/compare.yaml`) blank the parts that
//! may differ (generated ids, times, version strings), so that the rest must be equal. The
//! answer is `compare.json` (`hydo_results::CompareDoc`), a plain report on stdout, and a
//! section of `hydo report` and `hydo report --html` of the directory.
//!
//! With `--seeds`, the schedules are drawn as `hydo explore` draws them ([`hydo_explore::Drawer`])
//! from version A's image, and each schedule whose runs differ is shrunk with the explorer's
//! candidates to a smallest schedule whose runs still differ in the same stream.
//!
//! Tests: the scripted engine (`HYDO_SCRIPTED_ENGINE`) answers the runs and writes their
//! consoles (tests/compare.rs); the reading and the comparison are unit-tested on the recorded
//! Redis runs of `tests/fixtures/report/redis` (below).

use crate::ctx::{abspath, die, take, Ctx, Platform, OUT};
use crate::diff::Op;
use crate::runs::RunOpts;
use crate::versions::{self, kv, slug, split_list, take_all, take_flag, Maker, Source, Version};
use hydo_results::{
    compare_level as lv, CompareDifference, CompareDoc, CompareSchedule, CompareSearch,
    CompareVersion,
};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const HELP: &str = r#"hydo compare-versions -- does a new version of the system behave as the old one? (hydo guide workflow)

    hydo compare-versions "<schedule>"... VERSIONS [options]
    hydo compare-versions --regressions VERSIONS [options]
    hydo compare-versions --seeds A-B VERSIONS [explorer options] [options]

  runs each schedule on version A and on version B and compares what the two runs did. hydo
  repeats a run exactly, so every difference comes from the versions, never from chance. It
  compares three levels:
    verdicts      the verdict and each finding of result.json
    observations  what the workload's client saw: $HYDO_OUTCOMES, the acknowledged and failed
                  lists, and the HYDO-HIST records of the history and messages checks
    state         what the system stored: each service's compare: command (MANIFEST.md, "State
                  for comparisons") and what each check printed
  For each schedule it prints the first difference: the step, the operation or the line, what A
  had and what B had, and the commands that run it again on each side.

  VERSIONS (two, A first, as for hydo versions):
    --param KEY=A,B          hydo build MANIFEST --set KEY=<version>
    --git REV_A,REV_B        a checkout of each revision, its --build command, then hydo build
    --images A=IMG,B=IMG     two ready images

  SCHEDULES:
    "<schedule>"...          the schedules given (repeatable)
    --regressions            the saved regressions that hydo build wrote beside version A's
                             image (IMG.regressions.json)
    --seeds A-B              schedules drawn from these seeds as hydo explore draws them, from
                             version A's image; the explorer's drawing options follow
                             (--strategy, --new-faults, --storage-faults, ...). Each schedule
                             whose runs differ is shrunk to a smallest one that still differs.

  Options:
    --levels L,L,...    verdicts, observations, state (default: all three)
    --ignore REGEX[=>TEXT]   replace what REGEX matches in every compared record (by <ignored>,
                        or TEXT) (repeatable)
    --ignore-json PATH  in a record that is JSON, replace the value at PATH (a.b.c; * is any
                        key or index) by "<ignored>" (repeatable)
    --normalize FILE    the rules from a file (default hydo/compare.yaml when it exists):
                        ignore: [{regex: ..., as: ...}, {json: ...}], levels: [...]
    --again DIR         compare the runs of an earlier comparison (its --out) once more, with the
                        rules and levels given now; no VM runs
    --no-shrink         --seeds: report the drawn schedules without shrinking them
    --shrink-budget N   --seeds: shrink candidates per difference, each run on both (default 20)
    --manifest FILE, --project DIR, --build CMD, --set KEY=VALUE, --cache DIR   as hydo versions
    --out DIR           where the runs and compare.json go (default hydo-out/compare)
    --vcpus N, --turn-margin N   as for hydo run

  `hydo report DIR` and `hydo report --html DIR` show the answer. The exit status is 0 when the
  two versions agree on every schedule, 1 when a schedule's runs differ, and 2 when a version
  could not be built or a schedule could not run on it.
"#;

// ------------------------------------------------------------------ the records of one run

/// One compared record of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rec {
    pub level: &'static str,
    /// The stream (`outcomes`, `history kv p1`, `state redis`, ...).
    pub stream: String,
    /// `run` or `recovery`; "" for the verdicts.
    pub conv: &'static str,
    /// The workload steps it belongs to (`0..11`), when known.
    pub step: Option<String>,
    pub text: String,
}

/// Streams compared as sets of lines, sorted by operation id: concurrent operations that finish
/// in another order agree.
fn is_set(stream: &str) -> bool {
    matches!(stream, "outcomes" | "acked" | "failed")
}

const CONVS: [(&str, &str, &str, &str); 2] = [
    ("run", "one.log", "control.jsonl", "one.agent.log"),
    (
        "recovery",
        "one.recovery.log",
        "control.recovery.jsonl",
        "one.recovery.agent.log",
    ),
];

/// `0..11` of a `hydo-ob: workload steps 0..11 rc=0` note.
fn steps_of(line: &str) -> Option<String> {
    let i = line.find("workload steps ")?;
    let rest = &line[i + "workload steps ".len()..];
    let w = rest.split_whitespace().next()?;
    w.contains("..").then(|| w.to_string())
}

/// The JSON after a record's tag (`HYDO-ASSERT {...}`).
fn json_after(line: &str, tag: &str) -> Option<Value> {
    let i = line.find(tag)?;
    serde_json::from_str(line[i + tag.len()..].trim()).ok()
}

/// The records of one console: its history records (with their steps), acknowledged and failed
/// lists, state dumps, and check outputs.
pub fn console_records(conv: &'static str, text: &str) -> Vec<Rec> {
    let lines: Vec<&str> = text.lines().collect();
    // the steps note at or after each line (a guest line is printed before the note of the
    // call that ran it), else the last before it
    let mut next_steps: Vec<Option<String>> = vec![None; lines.len()];
    let mut cur: Option<String> = None;
    for (i, l) in lines.iter().enumerate().rev() {
        if let Some(s) = steps_of(l) {
            cur = Some(s);
        }
        next_steps[i] = cur.clone();
    }
    let mut last: Option<String> = None;
    let mut seen_acked: Vec<String> = Vec::new();
    let mut seen_failed: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let rec = |level, stream: String, step: Option<String>, text: String| Rec {
        level,
        stream,
        conv,
        step,
        text,
    };
    for (i, l) in lines.iter().enumerate() {
        if let Some(s) = steps_of(l) {
            last = Some(s);
        }
        if let Some(k) = l.find(hydo_history::events::TAG) {
            let tok: Vec<&str> = l[k + hydo_history::events::TAG.len()..]
                .split_whitespace()
                .collect();
            if tok.len() >= 5 {
                // the guest time (tok[3]) moves with every change of the binaries: left out
                let mut t = vec![tok[2], tok[4]];
                t.extend(&tok[5..]);
                out.push(rec(
                    lv::OBSERVATIONS,
                    format!("history {} {}", tok[0], tok[1]),
                    next_steps[i].clone().or_else(|| last.clone()),
                    t.join(" "),
                ));
            }
            continue;
        }
        for (tag, stream, seen) in [
            ("hydo-explore-acked:", "acked", &mut seen_acked),
            ("hydo-explore-failed:", "failed", &mut seen_failed),
        ] {
            if let Some(k) = l.find(tag) {
                for id in l[k + tag.len()..].split_whitespace() {
                    if !seen.iter().any(|x| x == id) {
                        seen.push(id.to_string());
                        out.push(rec(
                            lv::OBSERVATIONS,
                            stream.into(),
                            last.clone(),
                            id.to_string(),
                        ));
                    }
                }
            }
        }
        if let Some(k) = l.find("HYDO-STATE-END ") {
            let rest = &l[k + "HYDO-STATE-END ".len()..];
            let mut w = rest.split_whitespace();
            if let Some(svc) = w.next() {
                let rc = w.find(|x| x.starts_with("rc=")).unwrap_or("rc=?");
                let cut = rest.contains("cut=1");
                out.push(rec(
                    lv::STATE,
                    format!("state {svc}"),
                    None,
                    format!(
                        "(the command ended with {}{})",
                        rc.replace('=', " "),
                        if cut {
                            "; its output was cut at 64 KiB"
                        } else {
                            ""
                        }
                    ),
                ));
            }
            continue;
        }
        if let Some(k) = l.find("HYDO-STATE ") {
            let rest = &l[k + "HYDO-STATE ".len()..];
            if let Some((svc, line)) = rest.split_once(": ") {
                out.push(rec(lv::STATE, format!("state {svc}"), None, line.into()));
            } else if let Some(svc) = rest.strip_suffix(':') {
                out.push(rec(lv::STATE, format!("state {svc}"), None, String::new()));
            }
            continue;
        }
        if let Some(v) = json_after(l, "HYDO-CHECK-OUT ") {
            let id = v["id"].as_str().unwrap_or_default().to_string();
            for x in v["lines"].as_array().into_iter().flatten() {
                out.push(rec(
                    lv::STATE,
                    format!("check {id}"),
                    None,
                    x.as_str().unwrap_or_default().to_string(),
                ));
            }
            continue;
        }
        if l.contains("HYDO-ASSERT {") {
            // a check's record: the controller's, with what the check printed
            if let Some(v) = json_after(l, "HYDO-ASSERT ") {
                if let (Some(id), Some(o)) = (v["id"].as_str(), v["details"]["out"].as_str()) {
                    let held = if v["ok"].as_bool() == Some(true) {
                        "held"
                    } else {
                        "failed"
                    };
                    out.push(rec(
                        lv::STATE,
                        format!("check {id}"),
                        None,
                        format!("{held}: {o}"),
                    ));
                }
            }
        }
    }
    out
}

/// One JSON line of a transcript or an agent log.
fn json_line(l: &str) -> Option<Value> {
    let t = l.trim_start_matches(|c: char| c.is_control()).trim();
    serde_json::from_str(t).ok()
}

/// The lines of `$HYDO_OUTCOMES` the controller read in one conversation (its transcript and
/// the agent's replies), each with the steps of the workload call before the read.
pub fn outcome_records(conv: &'static str, transcript: &str, agent: &str) -> Vec<Rec> {
    // the replies' data by request id
    let mut data: BTreeMap<u64, String> = BTreeMap::new();
    for l in agent.lines() {
        let Some(i) = l.find('{') else { continue };
        let Some(v) = json_line(&l[i..]) else {
            continue;
        };
        if v["op"] == "read" {
            if let (Some(re), Some(d)) = (v["re"].as_u64(), v["data"].as_str()) {
                data.insert(re, d.to_string());
            }
        }
    }
    let mut steps: Option<String> = None;
    // each read of the file from its start, with the chunks after it
    let mut snaps: Vec<(Option<String>, String)> = Vec::new();
    for l in transcript.lines() {
        let Some(r) = json_line(l) else { continue };
        match r["t"].as_str() {
            Some("note") => {
                if let Some(s) = r["line"].as_str().and_then(steps_of) {
                    steps = Some(s);
                }
            }
            Some("send") => {
                let Some(b) = r["bytes"].as_str().and_then(json_line) else {
                    continue;
                };
                let outcomes = b["path"].as_str().is_some_and(|p| p.ends_with("/outcomes"));
                if b["op"] != "read" || !outcomes {
                    continue;
                }
                let Some(d) = b["id"].as_u64().and_then(|id| data.get(&id)) else {
                    continue;
                };
                match (b["offset"].as_u64(), snaps.last_mut()) {
                    (Some(_), Some(last)) => last.1.push_str(d),
                    _ => snaps.push((steps.clone(), d.clone())),
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut prev = String::new();
    for (step, text) in snaps {
        // the lines added since the read before (a file the workload appends to)
        let new = text.strip_prefix(prev.as_str()).unwrap_or(&text);
        for line in new.lines().filter(|x| !x.trim().is_empty()) {
            out.push(Rec {
                level: lv::OBSERVATIONS,
                stream: "outcomes".into(),
                conv,
                step: step.clone(),
                text: line.trim().to_string(),
            });
        }
        prev = text;
    }
    out
}

/// The verdict's record: its kind and the check it names (`violation redis.acked_writes_survive`).
fn verdict_record(verdict: &str) -> String {
    let kind = hydo_engine::protocol::verdict_kind(verdict);
    let check = verdict
        .split_once('[')
        .and_then(|(_, r)| r.split_once(']'))
        .map(|(c, _)| format!(" {c}"))
        .unwrap_or_default();
    format!("{kind}{check}")
}

/// Everything one run directory gives: its records, verdict and fingerprint. A run of the
/// scripted engine (the tests) has no `result.json`; its verdict comes from the replay.
#[derive(Clone, Debug, Default)]
pub struct Observed {
    pub recs: Vec<Rec>,
    pub verdict: Option<String>,
    pub fp: Option<String>,
}

pub fn observe(dir: &Path, verdict: Option<&str>, fp: Option<&str>) -> Observed {
    let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap_or_default();
    let mut o = Observed {
        verdict: verdict.map(str::to_string),
        fp: fp.map(str::to_string),
        ..Observed::default()
    };
    let run = hydo_results::reader::read_run(&dir.join(hydo_results::RUN_DOC)).ok();
    if let Some(r) = &run {
        o.verdict = Some(r.verdict.text.clone());
        o.fp = Some(r.fingerprints.fp.clone());
    }
    if let Some(v) = &o.verdict {
        o.recs.push(Rec {
            level: lv::VERDICTS,
            stream: "verdict".into(),
            conv: "",
            step: None,
            text: verdict_record(v),
        });
    }
    if let Some(r) = &run {
        let mut fs: Vec<String> = r
            .findings
            .iter()
            .map(|f| format!("{} {}: {}", f.kind, f.id, f.status))
            .collect();
        fs.sort();
        for f in fs {
            o.recs.push(Rec {
                level: lv::VERDICTS,
                stream: "findings".into(),
                conv: "",
                step: None,
                text: f,
            });
        }
    }
    for (conv, console, transcript, agent) in CONVS {
        o.recs
            .extend(outcome_records(conv, &read(transcript), &read(agent)));
        o.recs.extend(console_records(conv, &read(console)));
    }
    o
}

// ------------------------------------------------------------------ normalization

/// One rule that blanks what may differ.
#[derive(Clone, Debug)]
pub enum Rule {
    Regex(Regex, String),
    Json(Vec<String>),
}

impl Rule {
    pub fn regex(spec: &str) -> Result<Rule, String> {
        let (pat, with) = match spec.split_once("=>") {
            Some((p, w)) => (p, w.to_string()),
            None => (spec, "<ignored>".to_string()),
        };
        Regex::new(pat)
            .map(|r| Rule::Regex(r, with))
            .map_err(|e| format!("--ignore {spec}: not a regular expression: {e}"))
    }

    pub fn json(path: &str) -> Result<Rule, String> {
        let p = path.trim_start_matches("$.").trim_start_matches('$');
        let parts: Vec<String> = p
            .split('.')
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect();
        if parts.is_empty() {
            return Err(format!("--ignore-json {path}: an empty path"));
        }
        Ok(Rule::Json(parts))
    }

    pub fn describe(&self) -> String {
        match self {
            Rule::Regex(r, w) => format!("regex {} => {w}", r.as_str()),
            Rule::Json(p) => format!("json {}", p.join(".")),
        }
    }
}

fn blank(v: &mut Value, path: &[String]) {
    let Some((k, rest)) = path.split_first() else {
        *v = Value::String("<ignored>".into());
        return;
    };
    match v {
        Value::Object(m) => {
            if k == "*" {
                for x in m.values_mut() {
                    blank(x, rest);
                }
            } else if let Some(x) = m.get_mut(k) {
                blank(x, rest);
            }
        }
        Value::Array(a) => {
            if k == "*" {
                for x in a.iter_mut() {
                    blank(x, rest);
                }
            } else if let Some(x) = k.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
                blank(x, rest);
            }
        }
        _ => {}
    }
}

/// A record's text after the rules: the JSON rules on the JSON after the first `{` or `[`, then
/// the regular expressions.
pub fn normalize(text: &str, rules: &[Rule]) -> String {
    let mut t = text.to_string();
    let paths: Vec<&Vec<String>> = rules
        .iter()
        .filter_map(|r| match r {
            Rule::Json(p) => Some(p),
            _ => None,
        })
        .collect();
    if !paths.is_empty() {
        if let Some(i) = t.find(['{', '[']) {
            if let Ok(mut v) = serde_json::from_str::<Value>(&t[i..]) {
                for p in &paths {
                    blank(&mut v, p);
                }
                t = format!("{}{}", &t[..i], v);
            }
        }
    }
    for r in rules {
        if let Rule::Regex(re, with) = r {
            t = re.replace_all(&t, with.as_str()).into_owned();
        }
    }
    t
}

/// The rules and levels of a `hydo/compare.yaml`.
pub fn rules_file(path: &Path) -> Result<(Vec<Rule>, Option<Vec<String>>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = hydo_manifest::yaml::parse_yaml(&text, &path.display().to_string())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut rules = Vec::new();
    for x in v["ignore"].as_array().into_iter().flatten() {
        if let Some(p) = x["regex"].as_str() {
            let spec = match x["as"].as_str() {
                Some(w) => format!("{p}=>{w}"),
                None => p.to_string(),
            };
            rules.push(Rule::regex(&spec).map_err(|e| format!("{}: {e}", path.display()))?);
        } else if let Some(p) = x["json"].as_str() {
            rules.push(Rule::json(p).map_err(|e| format!("{}: {e}", path.display()))?);
        } else {
            return Err(format!(
                "{}: ignore: each entry is {{regex: ..., as: ...}} or {{json: ...}}",
                path.display()
            ));
        }
    }
    let levels = v["levels"].as_array().map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    });
    Ok((rules, levels))
}

// ------------------------------------------------------------------ the comparison

/// The streams of a run in the order they first appear: (level, conversation, stream).
type Key = (&'static str, &'static str, String);

fn keys(a: &[Rec], b: &[Rec]) -> Vec<Key> {
    let mut v: Vec<Key> = Vec::new();
    for r in a.iter().chain(b) {
        let k = (r.level, r.conv, r.stream.clone());
        if !v.contains(&k) {
            v.push(k);
        }
    }
    // the run's order: what the client saw, what was stored, the verdict
    v.sort_by_key(|k| (level_rank(k.0), conv_rank(Some(k.1))));
    v
}

/// A numeric-aware order of lines: by their first word as a number, then by text.
fn sort_lines(v: &mut [Rec]) {
    v.sort_by(|x, y| {
        let n = |r: &Rec| {
            r.text
                .split_whitespace()
                .next()
                .and_then(|w| w.parse::<i64>().ok())
        };
        (n(x).is_none(), n(x), &x.text).cmp(&(n(y).is_none(), n(y), &y.text))
    });
}

fn level_rank(l: &str) -> u8 {
    match l {
        lv::OBSERVATIONS => 0,
        lv::STATE => 1,
        _ => 2,
    }
}

fn conv_rank(c: Option<&str>) -> u8 {
    match c {
        Some("run") => 0,
        Some("recovery") => 1,
        _ => 2,
    }
}

fn step_lo(s: Option<&str>) -> i64 {
    s.and_then(|s| s.split("..").next())
        .and_then(|x| x.parse().ok())
        .unwrap_or(i64::MAX)
}

/// What differs between two runs' records at the given levels: the first difference of each
/// stream, the index of the first in the run's order, and the records compared per level.
pub fn compare(
    a: &[Rec],
    b: &[Rec],
    levels: &[String],
    rules: &[Rule],
) -> (Vec<CompareDifference>, Option<u64>, BTreeMap<String, u64>) {
    let keep = |r: &&Rec| levels.iter().any(|l| l == r.level);
    let norm = |v: &[Rec]| -> Vec<Rec> {
        v.iter()
            .filter(keep)
            .map(|r| Rec {
                text: normalize(&r.text, rules),
                ..r.clone()
            })
            .collect()
    };
    let (a, b) = (norm(a), norm(b));
    let mut compared: BTreeMap<String, u64> = BTreeMap::new();
    for r in a.iter().chain(&b) {
        *compared.entry(r.level.to_string()).or_default() += 1;
    }
    let mut diffs = Vec::new();
    for (level, conv, stream) in keys(&a, &b) {
        let pick = |v: &[Rec]| -> Vec<Rec> {
            v.iter()
                .filter(|r| r.level == level && r.conv == conv && r.stream == stream)
                .cloned()
                .collect()
        };
        let (xa, xb) = (pick(&a), pick(&b));
        let ta: Vec<&str> = xa.iter().map(|r| r.text.as_str()).collect();
        let tb: Vec<&str> = xb.iter().map(|r| r.text.as_str()).collect();
        let same = if is_set(&stream) {
            let (mut sa, mut sb) = (xa.clone(), xb.clone());
            sort_lines(&mut sa);
            sort_lines(&mut sb);
            sa.iter().map(|r| &r.text).eq(sb.iter().map(|r| &r.text))
        } else {
            ta == tb
        };
        if same {
            continue;
        }
        // the first place the two sequences part, aligned as `hydo diff` aligns events: a
        // record only one side has, or a record replaced by another
        let (ia, ib, before) = if is_set(&stream) {
            // the records only one side has, each side in its own run's order; a pair of one
            // operation (the same first word) when there is one, the earliest in A's order
            let only = |x: &[&str], y: &[&str]| -> Vec<usize> {
                let mut left: BTreeMap<&str, i64> = BTreeMap::new();
                for t in y {
                    *left.entry(t).or_default() += 1;
                }
                let mut v = Vec::new();
                for (i, t) in x.iter().enumerate() {
                    match left.get_mut(t) {
                        Some(n) if *n > 0 => *n -= 1,
                        _ => v.push(i),
                    }
                }
                v
            };
            let (oa, ob) = (only(&ta, &tb), only(&tb, &ta));
            let word = |t: &str| t.split_whitespace().next().unwrap_or("").to_string();
            let pair = oa.iter().find_map(|&i| {
                ob.iter()
                    .find(|&&j| word(tb[j]) == word(ta[i]))
                    .map(|&j| (i, j))
            });
            match pair {
                Some((i, j)) => (Some(i), Some(j), None),
                None => (oa.first().copied(), ob.first().copied(), None),
            }
        } else {
            let ops = crate::diff::align(&ta, &tb);
            let p = ops
                .iter()
                .position(|o| !matches!(o, Op::Eq(..)))
                .unwrap_or(ops.len());
            let before = p.checked_sub(1).and_then(|k| match ops[k] {
                Op::Eq(i, _) => Some(ta[i]),
                _ => None,
            });
            // the hunk from there to the next shared record: its first record on each side
            let hunk = ops[p..].iter().take_while(|o| !matches!(o, Op::Eq(..)));
            let (mut ia, mut ib) = (None, None);
            for o in hunk {
                match o {
                    Op::Del(i) if ia.is_none() => ia = Some(*i),
                    Op::Ins(j) if ib.is_none() => ib = Some(*j),
                    _ => {}
                }
            }
            (ia, ib, before)
        };
        let count = |x: &[&str], y: &[&str]| -> u64 {
            let mut m: BTreeMap<&str, i64> = BTreeMap::new();
            for t in x {
                *m.entry(t).or_default() += 1;
            }
            for t in y {
                *m.entry(t).or_default() -= 1;
            }
            m.values().filter(|n| **n > 0).map(|n| *n as u64).sum()
        };
        diffs.push(CompareDifference {
            level: level.to_string(),
            stream: stream.clone(),
            conversation: (!conv.is_empty()).then(|| conv.to_string()),
            index: ia.or(ib).unwrap_or(0) as u64,
            step_a: ia.and_then(|i| xa[i].step.clone()),
            step_b: ib.and_then(|j| xb[j].step.clone()),
            a: ia.map(|i| ta[i].to_string()),
            b: ib.map(|j| tb[j].to_string()),
            before: before.map(str::to_string),
            only_a: count(&ta, &tb),
            only_b: count(&tb, &ta),
        });
    }
    let first = diffs
        .iter()
        .enumerate()
        .min_by_key(|(n, d)| {
            (
                level_rank(&d.level),
                conv_rank(d.conversation.as_deref()),
                step_lo(d.step_a.as_deref().or(d.step_b.as_deref())),
                *n,
            )
        })
        .map(|(n, _)| n as u64);
    (diffs, first, compared)
}

// ------------------------------------------------------------------ words

fn conv_words(c: Option<&str>) -> &'static str {
    match c {
        Some("recovery") => " in the recovery boot after the power cut",
        Some("run") => "",
        _ => "",
    }
}

fn q(s: &str) -> String {
    let t: String = s.chars().take(200).collect();
    format!("`{t}`")
}

/// One difference in plain words, with the two versions' labels.
pub fn words(d: &CompareDifference, la: &str, lb: &str) -> String {
    let at = match (&d.step_a, &d.step_b) {
        (Some(x), Some(y)) if x != y => format!(" (workload steps {x} in A, {y} in B)"),
        (Some(x), _) | (None, Some(x)) => format!(" at workload steps {x}"),
        _ => String::new(),
    };
    let side = |x: &Option<String>| x.as_deref().map(q);
    let (a, b) = (side(&d.a), side(&d.b));
    let n = d.index + 1;
    let cw = conv_words(d.conversation.as_deref());
    match (d.level.as_str(), d.stream.as_str()) {
        (lv::VERDICTS, "verdict") => format!(
            "the verdicts differ: A ({la}) {}, B ({lb}) {}",
            a.unwrap_or_else(|| "has none".into()),
            b.unwrap_or_else(|| "has none".into())
        ),
        (lv::VERDICTS, _) => match (a, b) {
            (Some(a), Some(b)) => format!("finding {n} differs: A ({la}) has {a}, B ({lb}) has {b}"),
            (Some(a), None) => format!("only A ({la}) has the finding {a}"),
            (None, Some(b)) => format!("only B ({lb}) has the finding {b}"),
            _ => String::new(),
        },
        (_, "outcomes") => match (a, b) {
            (Some(a), Some(b)) => format!(
                "the client saw another outcome{at}{cw}: A ({la}) has {a} in $HYDO_OUTCOMES where B ({lb}) has {b}"
            ),
            (Some(a), None) => {
                format!("the client saw the outcome {a}{at}{cw} in A ({la}) only")
            }
            (None, Some(b)) => {
                format!("the client saw the outcome {b}{at}{cw} in B ({lb}) only")
            }
            _ => String::new(),
        },
        (_, s @ ("acked" | "failed")) => {
            let what = if s == "acked" { "acknowledged" } else { "failed" };
            match (a, b) {
                (Some(a), Some(b)) => format!(
                    "the {what} operations differ{at}{cw}: A ({la}) has {a} where B ({lb}) has {b}"
                ),
                (Some(a), None) => format!("the client saw {a} {what}{at}{cw} in A ({la}) only"),
                (None, Some(b)) => format!("the client saw {b} {what}{at}{cw} in B ({lb}) only"),
                _ => String::new(),
            }
        }
        (_, s) if s.starts_with("history ") => {
            let what = s.trim_start_matches("history ");
            match (a, b) {
                (Some(a), Some(b)) => format!(
                    "the history {what} parts at its record {n}{at}{cw}: A ({la}) recorded {a}, B ({lb}) recorded {b}"
                ),
                (Some(a), None) => format!("only A ({la}) recorded {a} in the history {what}{at}{cw}"),
                (None, Some(b)) => format!("only B ({lb}) recorded {b} in the history {what}{at}{cw}"),
                _ => String::new(),
            }
        }
        (_, s) if s.starts_with("state ") => {
            let svc = s.trim_start_matches("state ");
            match (a, b) {
                (Some(a), Some(b)) => format!(
                    "the state that {svc} stored differs{cw}: line {n} of its compare: output is {a} in A ({la}) and {b} in B ({lb})"
                ),
                (Some(a), None) => format!("only A ({la}) stored {a} ({svc}, line {n}){cw}"),
                (None, Some(b)) => format!("only B ({lb}) stored {b} ({svc}, line {n}){cw}"),
                _ => String::new(),
            }
        }
        (_, s) => {
            let id = s.trim_start_matches("check ");
            match (a, b) {
                (Some(a), Some(b)) => format!(
                    "the check {id} printed {a} in A ({la}) and {b} in B ({lb}){cw}"
                ),
                (Some(a), None) => format!("only A ({la}) has the check output {a} ({id}){cw}"),
                (None, Some(b)) => format!("only B ({lb}) has the check output {b} ({id}){cw}"),
                _ => String::new(),
            }
        }
    }
}

/// The text of one schedule's comparison, for people.
pub fn schedule_text(s: &CompareSchedule, la: &str, lb: &str) -> String {
    let mut t = format!("schedule: {}   ({})\n", s.schedule, s.from);
    if let Some(f) = &s.shrunk_from {
        t.push_str(&format!(
            "  shrunk from: {f} ({} candidate runs on each side)\n",
            s.shrink_runs.unwrap_or(0)
        ));
    }
    let side = |l: &str, v: &Option<String>, fp: &Option<String>, d: &Option<String>| {
        format!(
            "  {l}: {}{}{}\n",
            v.as_deref().unwrap_or("no run"),
            fp.as_deref()
                .map(|f| format!("  fp={f}"))
                .unwrap_or_default(),
            d.as_deref().map(|d| format!("  ({d})")).unwrap_or_default()
        )
    };
    t.push_str(&side(
        &format!("A ({la})"),
        &s.verdict_a,
        &s.fingerprint_a,
        &s.dir_a,
    ));
    t.push_str(&side(
        &format!("B ({lb})"),
        &s.verdict_b,
        &s.fingerprint_b,
        &s.dir_b,
    ));
    t.push_str(&format!("  {}\n", s.summary));
    if s.differences.len() > 1 {
        t.push_str("  every stream that differs, at its first difference:\n");
        for d in &s.differences {
            t.push_str(&format!(
                "    {}, {}{}: {} ({} only in A, {} only in B)\n",
                d.level,
                d.stream,
                d.conversation
                    .as_deref()
                    .map(|c| format!(" ({c})"))
                    .unwrap_or_default(),
                words(d, la, lb),
                d.only_a,
                d.only_b
            ));
        }
    }
    for (l, r) in [("A", &s.replay_a), ("B", &s.replay_b)] {
        if let Some(r) = r {
            t.push_str(&format!("  replay {l}: {r}\n"));
        }
    }
    t
}

/// The whole answer, for people.
pub fn text(doc: &CompareDoc) -> String {
    let (la, lb) = (&doc.a.version, &doc.b.version);
    let mut t = format!(
        "versions: A {la}, B {lb} ({}; engine {})\nlevels: {}\n",
        doc.source,
        doc.engine.backend.as_deref().unwrap_or("-"),
        doc.levels.join(", ")
    );
    if !doc.normalize.is_empty() {
        t.push_str(&format!("normalized: {}\n", doc.normalize.join("; ")));
    }
    if let Some(s) = &doc.search {
        t.push_str(&format!(
            "search: seeds {}: {} schedules drawn, {} differ, {} could not run\n",
            s.seeds, s.drawn, s.differ, s.cannot_run
        ));
    }
    t.push('\n');
    let shown: Vec<&CompareSchedule> = if doc.search.is_some() {
        // a search: the schedules that differ, and their shrinks
        doc.schedules
            .iter()
            .filter(|s| s.identical != Some(true))
            .collect()
    } else {
        doc.schedules.iter().collect()
    };
    for s in shown {
        t.push_str(&schedule_text(s, la, lb));
        t.push('\n');
    }
    t.push_str(&format!("{}\n", doc.summary));
    t
}

/// The lines `hydo report` prints for a comparison.
pub fn report_lines(doc: &CompareDoc) -> String {
    text(doc)
}

// ------------------------------------------------------------------ the command

/// One version's runs.
struct Side {
    label: String,
    image: Option<PathBuf>,
    why: Option<String>,
}

/// The command that runs a schedule again on an image.
fn replay_line(ctx: &Ctx, image: &Path, sched: &str) -> String {
    let backend = match ctx.platform {
        Platform::Linux => format!("--backend {} ", ctx.backend_name()),
        Platform::Mac => String::new(),
    };
    format!("hydo run {backend}{} \"{sched}\"", image.display())
}

struct Runner<'a> {
    ctx: &'a Ctx,
    opts: RunOpts,
    out: PathBuf,
    levels: Vec<String>,
    rules: Vec<Rule>,
    n: usize,
}

impl Runner<'_> {
    /// Run one schedule on both sides and compare.
    fn one(&mut self, a: &Side, b: &Side, sched: &str, from: &str) -> CompareSchedule {
        self.n += 1;
        let mut cs = CompareSchedule {
            schedule: sched.to_string(),
            from: from.to_string(),
            ..CompareSchedule::default()
        };
        let mut obs: Vec<Option<Observed>> = Vec::new();
        for (tag, side) in [("a", a), ("b", b)] {
            let Some(img) = &side.image else {
                cs.why = Some(format!(
                    "{} ({}) has no image: {}",
                    tag.to_uppercase(),
                    side.label,
                    side.why.clone().unwrap_or_default()
                ));
                obs.push(None);
                continue;
            };
            let d =
                self.out
                    .join("runs")
                    .join(format!("{:03}-{tag}-{}", self.n, slug(&side.label)));
            println!(
                "hydo compare-versions: {} ({}): {sched}",
                tag.to_uppercase(),
                side.label
            );
            let r = versions::replay(self.ctx, &self.opts, img, sched, &d);
            let rel = d
                .strip_prefix(&self.out)
                .unwrap_or(&d)
                .display()
                .to_string();
            let line = replay_line(self.ctx, img, sched);
            if tag == "a" {
                cs.dir_a = Some(rel);
                cs.replay_a = Some(line);
            } else {
                cs.dir_b = Some(rel);
                cs.replay_b = Some(line);
            }
            if r.verdict.is_none() {
                cs.why = Some(format!(
                    "{} ({}) did not run: {}",
                    tag.to_uppercase(),
                    side.label,
                    r.why.clone().unwrap_or_default()
                ));
                obs.push(None);
                continue;
            }
            obs.push(Some(observe(&d, r.verdict.as_deref(), r.fp.as_deref())));
        }
        let (Some(oa), Some(ob)) = (&obs[0], &obs[1]) else {
            cs.summary = format!(
                "not compared: {}",
                cs.why
                    .clone()
                    .unwrap_or_else(|| "a side did not run".into())
            );
            return cs;
        };
        cs.verdict_a = oa.verdict.clone();
        cs.verdict_b = ob.verdict.clone();
        cs.fingerprint_a = oa.fp.clone();
        cs.fingerprint_b = ob.fp.clone();
        let (diffs, first, compared) = compare(&oa.recs, &ob.recs, &self.levels, &self.rules);
        cs.identical = Some(diffs.is_empty());
        cs.compared = compared;
        cs.first = first;
        cs.summary = summary_of(&cs, &diffs, first, &a.label, &b.label);
        cs.differences = diffs;
        println!("hydo compare-versions: {}", cs.summary);
        cs
    }

    /// The key of a comparison's first difference: what a shrink must keep.
    fn key_of(s: &CompareSchedule) -> Option<(String, String)> {
        let d = s.differences.get(s.first? as usize)?;
        Some((d.level.clone(), d.stream.clone()))
    }
}

/// The schedules of `--regressions`: the saved regressions beside version A's image.
fn regressions(image: &Path) -> Vec<(String, String)> {
    let r = hydo_control::agent::regressions_of_image(image)
        .unwrap_or_else(|e| die(&format!("--regressions: {}: {e}", image.display())));
    r.regressions
        .iter()
        .filter_map(|x| {
            let s = x["schedule"].as_str()?.to_string();
            let name = x["name"]
                .as_str()
                .or_else(|| x["file"].as_str())
                .unwrap_or("?")
                .to_string();
            Some((format!("regression {name}"), s))
        })
        .collect()
}

/// The explorer's command line that draws schedules from `image` (as `hydo explore` builds it).
fn drawer(ctx: &Ctx, image: &Path, out: &Path, opts: &[String]) -> hydo_explore::Drawer {
    let mut full: Vec<String> = Vec::new();
    match (ctx.platform, ctx.kvm) {
        (Platform::Linux, true) => {
            full.push("--backend".into());
            full.push("kvm".into());
        }
        (Platform::Linux, false) => {}
        (Platform::Mac, _) => {
            let kernel = crate::runs::mac_kernel_for(ctx, image, true);
            full.push("--backend".into());
            full.push("hvm".into());
            full.push("--hvm-kernel".into());
            full.push(kernel.display().to_string());
        }
    }
    full.push("--image".into());
    full.push(image.display().to_string());
    full.push("--out".into());
    full.push(out.display().to_string());
    full.extend(opts.iter().cloned());
    let env = hydo_explore::Env {
        root: ctx.root.clone(),
        cli: true,
        plain_end: false,
        config: if ctx.platform == Platform::Linux {
            crate::hvxenv::explore_config(ctx)
        } else {
            ctx.config.clone()
        },
        memory_hint: None,
    };
    hydo_explore::Drawer::new(&full, &env)
        .unwrap_or_else(|e| die(&format!("hydo compare-versions --seeds: {e}")))
}

fn parse_seeds(s: &str) -> (u64, u64) {
    s.split_once('-')
        .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
        .filter(|(a, b)| a <= b)
        .unwrap_or_else(|| die(&format!("--seeds {s}: A-B, such as 1-20")))
}

pub fn cmd_compare(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return 0;
    }
    let param = take(&mut args, "--param");
    let git_spec = take(&mut args, "--git");
    let images: Vec<String> = take_all(&mut args, "--images");
    let manifest_flag = take(&mut args, "--manifest");
    let project_flag = take(&mut args, "--project");
    let build = take(&mut args, "--build");
    let sets: Vec<(String, String)> = take_all(&mut args, "--set")
        .iter()
        .map(|s| kv(s, "--set"))
        .collect();
    let levels_flag = take(&mut args, "--levels");
    let ignores = take_all(&mut args, "--ignore");
    let ignore_json = take_all(&mut args, "--ignore-json");
    let normalize_flag = take(&mut args, "--normalize");
    let seeds = take(&mut args, "--seeds");
    let use_regressions = take_flag(&mut args, "--regressions");
    let no_shrink = take_flag(&mut args, "--no-shrink");
    let budget: u64 = take(&mut args, "--shrink-budget")
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| die(&format!("--shrink-budget {v}: a whole number")))
        })
        .unwrap_or(20);
    let out_flag = take(&mut args, "--out");
    let cache_flag = take(&mut args, "--cache");
    let again_dir = take(&mut args, "--again");
    let opts = RunOpts::take(ctx, &mut args);
    // what is left: the schedules, or with --seeds the explorer's drawing options
    let (given, draw_opts): (Vec<String>, Vec<String>) = if seeds.is_some() {
        (Vec::new(), args.clone())
    } else {
        if let Some(a) = args.iter().find(|a| a.starts_with("--")) {
            die(&format!(
                "hydo compare-versions: unknown option {a} (hydo compare-versions --help)"
            ));
        }
        (args.clone(), Vec::new())
    };
    let modes = [!given.is_empty(), use_regressions, seeds.is_some()]
        .iter()
        .filter(|x| **x)
        .count();
    if modes != 1 && again_dir.is_none() {
        die("usage: hydo compare-versions <\"<schedule>\"... | --regressions | --seeds A-B> --param KEY=A,B | --git A,B | --images A=IMG,B=IMG [options] (hydo compare-versions --help)");
    }
    let sources = [param.is_some(), git_spec.is_some(), !images.is_empty()]
        .iter()
        .filter(|x| **x)
        .count();
    if sources != 1 && again_dir.is_none() {
        die("hydo compare-versions: name the two versions with one of --param, --git or --images");
    }

    // the rules
    let mut rules: Vec<Rule> = Vec::new();
    let mut file_levels = None;
    let nfile = normalize_flag
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from("hydo/compare.yaml")).filter(|p| p.is_file()));
    if let Some(f) = &nfile {
        let (r, l) = rules_file(f).unwrap_or_else(|e| die(&e));
        rules.extend(r);
        file_levels = l;
    }
    for i in &ignores {
        rules.push(Rule::regex(i).unwrap_or_else(|e| die(&e)));
    }
    for j in &ignore_json {
        rules.push(Rule::json(j).unwrap_or_else(|e| die(&e)));
    }
    let levels: Vec<String> = match levels_flag {
        Some(l) => split_list(&l),
        None => file_levels.unwrap_or_else(|| {
            [lv::VERDICTS, lv::OBSERVATIONS, lv::STATE]
                .map(String::from)
                .to_vec()
        }),
    };
    for l in &levels {
        if ![lv::VERDICTS, lv::OBSERVATIONS, lv::STATE].contains(&l.as_str()) {
            die(&format!(
                "--levels {l}: one of verdicts, observations, state"
            ));
        }
    }
    if let Some(d) = again_dir {
        if modes > 0 {
            die("--again compares the runs of an earlier comparison: give no schedules");
        }
        return again(&abspath(Path::new(&d)), levels, rules);
    }

    // the two versions (as hydo versions makes them)
    let out = abspath(
        &out_flag
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(OUT).join("compare")),
    );
    let _ = std::fs::create_dir_all(&out);
    let (source, vs): (Source, Vec<Version>) = if let Some(p) = &param {
        let (k, list) = kv(p, "--param");
        let manifest = abspath(Path::new(
            manifest_flag.as_deref().unwrap_or("hydo/hydo.yaml"),
        ));
        if !manifest.is_file() {
            die(&format!(
                "--param: no manifest {} (--manifest FILE)",
                manifest.display()
            ));
        }
        let v = split_list(&list)
            .into_iter()
            .map(|label| Version {
                label,
                commit: None,
                image: None,
                bad: None,
            })
            .collect();
        (Source::Param { key: k, manifest }, v)
    } else if let Some(spec) = &git_spec {
        let project = abspath(Path::new(project_flag.as_deref().unwrap_or(".")));
        let top = versions::git(&project, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                die(&format!(
                    "--git: {} is not in a git repository",
                    project.display()
                ))
            });
        let rel = abspath(&project)
            .strip_prefix(abspath(&top))
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let manifest = PathBuf::from(manifest_flag.as_deref().unwrap_or("hydo/hydo.yaml"));
        if spec.contains("..") {
            die("--git: two revisions, A,B (a range is for hydo versions)");
        }
        let v = versions::git_versions(&top, spec);
        (Source::Git { top, rel, manifest }, v)
    } else {
        let v = images
            .iter()
            .flat_map(|s| split_list(s))
            .map(|s| {
                let (label, path) = match s.split_once('=') {
                    Some((l, p)) => (l.to_string(), p.to_string()),
                    None => (
                        Path::new(&s)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| s.clone()),
                        s.clone(),
                    ),
                };
                Version {
                    label,
                    commit: None,
                    image: Some(abspath(Path::new(&path))),
                    bad: None,
                }
            })
            .collect();
        (Source::Images, v)
    };
    if vs.len() != 2 {
        die(&format!(
            "hydo compare-versions: two versions, A and B ({} given)",
            vs.len()
        ));
    }
    let cache = cache_flag
        .map(|c| abspath(Path::new(&c)))
        .unwrap_or_else(|| ctx.config.cli.cache.home.join("versions"));
    let mut maker = Maker {
        ctx,
        source,
        sets,
        build,
        cache,
        out: out.clone(),
        made: BTreeMap::new(),
        who: "hydo compare-versions",
    };
    let mut cv: Vec<CompareVersion> = Vec::new();
    let mut sides: Vec<Side> = Vec::new();
    for (i, v) in vs.iter().enumerate() {
        let m = maker.image(i, v);
        cv.push(CompareVersion {
            version: v.label.clone(),
            commit: v.commit.clone(),
            image_sha256: m.as_ref().ok().and_then(|m| m.sha.clone()),
            image_reused: m.as_ref().ok().map(|m| m.reused),
            why: m.as_ref().err().cloned(),
        });
        sides.push(Side {
            label: v.label.clone(),
            image: m.as_ref().ok().map(|m| m.path.clone()),
            why: m.err(),
        });
    }
    let mut doc = CompareDoc {
        hydo_version: Some(hydo_image::hydo_version().to_string()),
        epoch: Some(hydo_image::EPOCH),
        engine: hydo_results::Engine {
            backend: Some(ctx.backend_name().into()),
            vcpus: opts.vcpus.map(u64::from),
            ..Default::default()
        },
        source: maker.source.name(),
        a: cv[0].clone(),
        b: cv[1].clone(),
        levels: levels.clone(),
        normalize: rules.iter().map(Rule::describe).collect(),
        ..Default::default()
    };
    let mut runner = Runner {
        ctx,
        opts,
        out: out.clone(),
        levels,
        rules,
        n: 0,
    };
    let (a, b) = (&sides[0], &sides[1]);

    // the schedules
    let list: Vec<(String, String)> = if use_regressions {
        let img = a.image.clone().unwrap_or_else(|| {
            die(&format!(
                "--regressions: version A ({}) has no image: {}",
                a.label,
                a.why.clone().unwrap_or_default()
            ))
        });
        let r = regressions(&img);
        if r.is_empty() {
            die(&format!(
                "--regressions: {} has no saved regressions ({})",
                img.display(),
                hydo_image::regressions_path(&img).display()
            ));
        }
        r
    } else {
        given
            .iter()
            .map(|s| ("given".to_string(), s.clone()))
            .collect()
    };
    if let Some(sd) = &seeds {
        let img = a.image.clone().unwrap_or_else(|| {
            die(&format!(
                "--seeds: version A ({}) has no image to draw from: {}",
                a.label,
                a.why.clone().unwrap_or_default()
            ))
        });
        let (lo, hi) = parse_seeds(sd);
        let d = drawer(ctx, &img, &out.join("draw"), &draw_opts);
        let mut search = CompareSearch {
            seeds: sd.clone(),
            options: draw_opts.clone(),
            ..CompareSearch::default()
        };
        for seed in lo..=hi {
            let sched = d.schedule(seed);
            let cs = runner.one(a, b, &sched, &format!("seed-{seed}"));
            search.drawn += 1;
            match cs.identical {
                None => search.cannot_run += 1,
                Some(true) => {}
                Some(false) => search.differ += 1,
            }
            let differs = cs.identical == Some(false);
            let key = Runner::key_of(&cs);
            doc.schedules.push(cs);
            if !differs || no_shrink {
                continue;
            }
            // shrink: the explorer's candidates, simplest first; keep one whose runs still
            // differ in the same stream
            let mut cur = sched.clone();
            let mut best: Option<CompareSchedule> = None;
            let mut runs = 0u64;
            'rounds: while runs < budget {
                let cands = d.candidates(&cur);
                if cands.is_empty() {
                    break;
                }
                let mut kept = false;
                for c in cands {
                    if runs >= budget {
                        break 'rounds;
                    }
                    runs += 1;
                    let r = runner.one(a, b, &c, &format!("shrink of seed-{seed}"));
                    if r.identical == Some(false) && Runner::key_of(&r) == key {
                        cur = c;
                        best = Some(r);
                        kept = true;
                        break;
                    }
                }
                if !kept {
                    break;
                }
            }
            if let Some(mut m) = best {
                m.from = format!("shrunk from seed-{seed}");
                m.shrunk_from = Some(sched.clone());
                m.shrink_runs = Some(runs);
                println!("hydo compare-versions: seed-{seed}: minimal {}", m.schedule);
                doc.schedules.push(m);
            }
        }
        doc.search = Some(search);
    }
    for (from, s) in &list {
        let cs = runner.one(a, b, s, from);
        doc.schedules.push(cs);
    }

    conclude(&mut doc, &out)
}

/// The answer: the summary, `compare.json`, the report on stdout, and the exit status.
fn conclude(doc: &mut CompareDoc, out: &Path) -> i32 {
    let (la, lb) = (doc.a.version.clone(), doc.b.version.clone());
    let firsts: Vec<&CompareSchedule> = doc
        .schedules
        .iter()
        .filter(|s| !s.from.starts_with("shrink of") && s.shrunk_from.is_none())
        .collect();
    let differ = firsts.iter().filter(|s| s.identical == Some(false)).count();
    let failed = firsts.iter().filter(|s| s.identical.is_none()).count();
    let total = firsts.len();
    let plural = if total == 1 { "" } else { "s" };
    doc.summary = if differ == 0 && failed == 0 {
        format!("no difference: {la} and {lb} agree on {total} of {total} schedule{plural} at the levels compared")
    } else {
        let mut s = format!("{la} and {lb} differ on {differ} of {total} schedule{plural}");
        if failed > 0 {
            s.push_str(&format!("; {failed} could not run on a version"));
        }
        s
    };
    // the shrink candidates that did not keep the difference stay in the runs folder only
    doc.schedules.retain(|s| !s.from.starts_with("shrink of"));
    let p = out.join(hydo_results::COMPARE_DOC);
    if let Err(e) = hydo_results::write_document(&p, &*doc) {
        die(&format!("{}: {e}", p.display()));
    }
    println!();
    print!("{}", text(doc));
    println!("hydo compare-versions: {}", p.display());
    if failed > 0 {
        2
    } else if differ > 0 {
        1
    } else {
        0
    }
}

/// `--again DIR`: compare the runs of an earlier comparison once more, with the rules and levels
/// given now, and no VM.
fn again(dir: &Path, levels: Vec<String>, rules: Vec<Rule>) -> i32 {
    let Some((_, mut doc)) = hydo_results::compare_of(dir) else {
        die(&format!(
            "--again {}: no compare.json there (a hydo compare-versions --out directory)",
            dir.display()
        ))
    };
    let (la, lb) = (doc.a.version.clone(), doc.b.version.clone());
    for s in doc.schedules.iter_mut() {
        let (Some(da), Some(db)) = (s.dir_a.clone(), s.dir_b.clone()) else {
            continue;
        };
        if s.identical.is_none() {
            continue;
        }
        let oa = observe(
            &dir.join(da),
            s.verdict_a.as_deref(),
            s.fingerprint_a.as_deref(),
        );
        let ob = observe(
            &dir.join(db),
            s.verdict_b.as_deref(),
            s.fingerprint_b.as_deref(),
        );
        let (diffs, first, compared) = compare(&oa.recs, &ob.recs, &levels, &rules);
        s.identical = Some(diffs.is_empty());
        s.compared = compared;
        s.first = first;
        s.summary = summary_of(s, &diffs, first, &la, &lb);
        s.differences = diffs;
    }
    doc.levels = levels;
    doc.normalize = rules.iter().map(Rule::describe).collect();
    conclude(&mut doc, dir)
}

/// A schedule's summary sentence from its differences.
fn summary_of(
    cs: &CompareSchedule,
    diffs: &[CompareDifference],
    first: Option<u64>,
    la: &str,
    lb: &str,
) -> String {
    match first.and_then(|i| diffs.get(i as usize)) {
        None => {
            let same_fp = cs.fingerprint_a.is_some() && cs.fingerprint_a == cs.fingerprint_b;
            format!(
                "no difference at the levels compared ({} records){}",
                cs.compared.values().sum::<u64>(),
                if same_fp {
                    "; the two runs also have the same fingerprint"
                } else {
                    ""
                }
            )
        }
        Some(d) => format!("the first difference: {}", words(d, la, lb)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/report/redis/hydo-out")
            .join(name)
    }

    fn all() -> Vec<String> {
        [lv::VERDICTS, lv::OBSERVATIONS, lv::STATE]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn a_recorded_run_gives_its_outcomes_lists_and_checks() {
        let o = observe(&fixture("run-always"), None, None);
        let outcomes: Vec<&Rec> = o.recs.iter().filter(|r| r.stream == "outcomes").collect();
        assert_eq!(outcomes.len(), 12, "{:?}", o.recs);
        assert_eq!(outcomes[0].text, "0 ack");
        assert_eq!(outcomes[0].step.as_deref(), Some("0..11"));
        assert_eq!(outcomes[0].conv, "run");
        let acked: Vec<&str> = o
            .recs
            .iter()
            .filter(|r| r.stream == "acked")
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(acked.len(), 12);
        let check: Vec<&Rec> = o
            .recs
            .iter()
            .filter(|r| r.stream == "check redis.acked_writes_survive")
            .collect();
        assert_eq!(check.len(), 1);
        assert_eq!(check[0].text, "held: acked=12 all present");
        assert_eq!(check[0].conv, "recovery");
        assert_eq!(
            o.recs.iter().find(|r| r.stream == "verdict").unwrap().text,
            "ok"
        );
    }

    #[test]
    fn two_settings_differ_in_what_the_recovery_read_back() {
        let a = observe(&fixture("run-always"), None, None);
        let b = observe(&fixture("run-cut"), None, None);
        let (d, first, compared) = compare(&a.recs, &b.recs, &all(), &[]);
        assert!(!d.is_empty());
        assert!(compared[lv::OBSERVATIONS] >= 48, "{compared:?}");
        // the client saw the same 12 acknowledged writes in both runs
        assert!(
            !d.iter()
                .any(|x| x.stream == "outcomes" || x.stream == "acked"),
            "{d:?}"
        );
        let f = &d[first.unwrap() as usize];
        assert_eq!(f.level, lv::STATE, "{f:?}");
        assert_eq!(f.conversation.as_deref(), Some("recovery"));
        let w = words(f, "always", "everysec");
        assert!(w.contains("redis.acked_writes_survive"), "{w}");
        assert!(d.iter().any(|x| x.stream == "verdict"), "{d:?}");
        // the same run against itself: nothing
        let (d, first, _) = compare(&a.recs, &a.recs, &all(), &[]);
        assert!(d.is_empty() && first.is_none());
        // at the observations alone the two runs agree
        let (d, _, _) = compare(&a.recs, &b.recs, &[lv::OBSERVATIONS.to_string()], &[]);
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn history_records_leave_out_the_guest_time_and_keep_their_steps() {
        let text = "HYDO-HIST kv p1 invoke 120 write k1 v1\nHYDO-HIST kv p1 ok 450 write k1 v1\n\
                    hydo-ob: workload steps 0..3 rc=0\nHYDO-HIST kv p1 invoke 900 read k1 -\n\
                    hydo-ob: workload steps 4..5 rc=0\n";
        let r = console_records("run", text);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].stream, "history kv p1");
        assert_eq!(r[0].text, "invoke write k1 v1");
        assert_eq!(r[0].step.as_deref(), Some("0..3"));
        assert_eq!(r[2].step.as_deref(), Some("4..5"));
        let other = text.replace(" 120 ", " 9999 ").replace(" 900 ", " 1 ");
        let (d, _, _) = compare(&r, &console_records("run", &other), &all(), &[]);
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn state_dumps_compare_line_by_line_after_the_rules() {
        let a = "HYDO-STATE db: {\"id\":\"7f1c\",\"total\":10,\"at\":\"2026-01-01\"}\n\
                 HYDO-STATE db: k:2 v2\nHYDO-STATE-END db rc=0 lines=2 cut=0\n";
        let b = "HYDO-STATE db: {\"id\":\"a990\",\"total\":10,\"at\":\"2026-01-02\"}\n\
                 HYDO-STATE db: k:2 v3\nHYDO-STATE-END db rc=0 lines=2 cut=0\n";
        let (ra, rb) = (console_records("run", a), console_records("run", b));
        assert_eq!(ra.len(), 3);
        let (d, first, _) = compare(&ra, &rb, &all(), &[]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].index, 0);
        assert_eq!(first, Some(0));
        let rules = vec![Rule::json("id").unwrap(), Rule::json("$.at").unwrap()];
        let (d, _, _) = compare(&ra, &rb, &all(), &rules);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].index, 1);
        assert_eq!(
            (d[0].a.as_deref(), d[0].b.as_deref()),
            (Some("k:2 v2"), Some("k:2 v3"))
        );
        let w = words(&d[0], "1.0", "1.1");
        assert!(w.contains("the state that db stored differs"), "{w}");
        let rules = vec![
            Rule::json("id").unwrap(),
            Rule::json("at").unwrap(),
            Rule::regex(r"v\d+=>v#").unwrap(),
        ];
        let (d, _, _) = compare(&ra, &rb, &all(), &rules);
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn lists_compare_as_sets_of_operations() {
        let a = "hydo-ob: workload steps 0..3 rc=0\nhydo-explore-acked: 3 1 2\n";
        let b = "hydo-ob: workload steps 0..3 rc=0\nhydo-explore-acked: 1 2 3\n";
        let (d, _, _) = compare(
            &console_records("run", a),
            &console_records("run", b),
            &all(),
            &[],
        );
        assert!(d.is_empty(), "{d:?}");
        let c = "hydo-ob: workload steps 0..3 rc=0\nhydo-explore-acked: 1 2\n";
        let (d, _, _) = compare(
            &console_records("run", a),
            &console_records("run", c),
            &all(),
            &[],
        );
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].only_a, d[0].only_b), (1, 0));
        assert_eq!(d[0].a.as_deref(), Some("3"));
        assert_eq!(d[0].step_a.as_deref(), Some("0..3"));
    }

    #[test]
    fn json_paths_reach_into_lists() {
        let rules = vec![Rule::json("items.*.id").unwrap()];
        assert_eq!(
            normalize(r#"row {"items":[{"id":1,"n":2},{"id":3,"n":4}]}"#, &rules),
            r#"row {"items":[{"id":"<ignored>","n":2},{"id":"<ignored>","n":4}]}"#
        );
        assert_eq!(normalize("not json", &rules), "not json");
        assert!(Rule::regex("(").is_err());
        assert!(Rule::json("$.").is_err());
    }
}
