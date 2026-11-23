//! `hydo rules` (Milestone 236, tentative number; docs/design/rule-suggestion.md): rules that
//! nobody wrote.
//!
//! - `hydo rules scan [DIR]` lists what a project promises, with the file and the line: schema
//!   constraints and state columns, API contracts, promise words in the documentation, and test
//!   assertions about refusals and limits. It knows no domain and writes no rule; the onboarding
//!   skill turns the evidence into candidate rules.
//! - `hydo rules check <candidates file>` validates candidate rules before a person sees them:
//!   it runs the image's workload without faults on several composition seeds, with every
//!   candidate as a final invariant added to the image's plan (the image is not rebuilt), and
//!   reports each rule as `holds`, `vacuous` (it judged no case), `fails-clean` (it failed
//!   without faults), `unevaluated` or `not-run`. `--mutate` makes a planted change of the state
//!   before a rule that holds, which the rule must then catch.
//!
//! Each candidate runs inside a wrapper that exits 0 unless the rule failed (exit 1), and whose
//! last line is `hydo-rule exit=S mutate=M cases=C :: LAST`. So every candidate gets one
//! `HYDO-ASSERT` record with its own exit status and case count, and a candidate that cannot be
//! evaluated does not turn the run into an error for the others.

use crate::ctx::{abspath, die, take, Ctx, OUT};
use crate::runs::{find_image, run_with_plan, RunOpts};
use hydo_control::plan::{Layout, PlanInvariant};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HELP: &str = "usage: hydo rules scan [DIR] [--kind K] [--json] [--max N] [--manifest FILE] [--all]
       hydo rules check <candidates.yaml> [IMG] [--image IMG] [--runs N] [--schedule S]
                        [--mutate] [--keep-checks] [--timeout S] [--out DIR]
                        [--write-checks DIR] [--vcpus N]
  scan: lists what the project in DIR (default .) promises, with the file and the line: schema
  constraints and state columns, API contracts, promise words in its documentation, and test
  assertions about refusals and limits, and the code that measures a duration with the wall
  clock, and the routes of its API that write. It writes no rule. --kind K shows one kind
  (schema, state, api, docs, tests, clock, writes).
  --kind clock is the clock audit: each place where a lease, a timeout, a throttle, a lag or a
  delay is measured on the wall clock instead of a monotonic clock, with the limit when the code
  states it, and three clock-step schedules aimed at it (a step back before and after the fault
  that makes the code run, and a step forward). The schedules name the services of hydo.yaml
  (--manifest FILE, default DIR/hydo/hydo.yaml or DIR/hydo.yaml) when they can. It shows one
  place per function, the most likely 5 first (--all shows every place; --json has them all,
  each with its rank).
  --kind writes lists every route of the API that writes: the HTTP routes with the methods POST,
  PUT, PATCH and DELETE of the router code (Rust, Go, JavaScript and TypeScript, Python, Java and
  Kotlin), the RPC methods of .proto files and gRPC servers whose names do not say that they
  read, and the operations of OpenAPI files. Each has its handler with the file and the line, and
  why it counts as a write. They are grouped by resource, and ranked by class: data (it changes
  the system's data), admin (a snapshot, a dump, a compaction), session (a sign-in or a token),
  runtime (health, metrics, a reload). A POST route whose name says that it reads is left out
  (--all lists them; --json has them all).
  check: validates candidate rules (format: hydo guide rules). It runs the image's workload
  without faults, `disk=none | run W=<seed>` for the seeds 00000001..N (--runs, default 3; one run of
  `disk=none | run` for an image without composed commands, `disk=none | run I=<seed>` for a walk
  through the pages, or --schedule S), with every
  candidate as a final check and without the project's own checks (--keep-checks keeps them).
  Each rule is reported as:
    holds        it held in every run and judged at least one case
    vacuous      it held, but its `cases` count was 0 in every run
    fails-clean  it failed in a run without faults: a wrong rule, or a bug on the normal path
    unevaluated  it could not be evaluated (exit 2, a missing program, a timeout)
    not-run      its kind needs the workload's records (messages, history)
  --mutate makes one more run: each rule that holds runs, then its `mutate` change of the state
  is made, then it runs again; it must fail then (caught), or it is blind to that change. A rule
  that an earlier rule's change already broke gets a run of its own. --write-checks DIR writes each rule that holds as
  a check file for hydo/checks/. Results: DIR/rules-check.json (default hydo-out/rules).";

pub fn cmd_rules(ctx: &Ctx, argv: &[String]) -> i32 {
    match argv.first().map(String::as_str) {
        Some("scan") => cmd_scan(&argv[1..]),
        Some("check") => cmd_check(ctx, &argv[1..]),
        Some("-h" | "--help") => {
            println!("{HELP}");
            0
        }
        _ => {
            eprintln!("{HELP}");
            2
        }
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    match args.iter().position(|a| a == flag) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------- the candidates file

/// The kinds of rule. `sql` and `command` run as final checks; `messages` and `history` need
/// the workload's records and are not run by `hydo rules check`.
pub const KINDS: &[&str] = &["sql", "command", "messages", "history"];
pub const CONFIDENCES: &[&str] = &["high", "medium", "low"];

/// One proposed rule.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    /// The rule in plain words.
    pub says: String,
    pub kind: String,
    pub confidence: String,
    /// Where the promise is stated (`file:line` or a link, with an optional quote).
    pub evidence: Vec<String>,
    /// `sql`: the endpoint of workload/helper_sql.conf (default: the file's `store`, else the
    /// image's first primary).
    pub store: Option<String>,
    /// `sql`: the query that must return no rows.
    pub query: Option<String>,
    /// `command`: the check command (exit 0 holds, 1 violated, 2 cannot evaluate).
    pub run: Option<String>,
    /// What the rule judges, as a count: a query (`sql`) or a command (`command`) whose last
    /// output line ends with the number.
    pub cases: Option<String>,
    /// A change of the state that must make the rule fail: a statement (`sql`) or a command.
    pub mutate: Option<String>,
    /// Seconds of guest time the rule may take to come true (an `eventually` rule: something
    /// the system does after the workload, such as an invoice or a settled transfer); None: it
    /// must hold at once.
    pub within: Option<u64>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim_end().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn evidence_of(v: &Value) -> Vec<String> {
    let one = |x: &Value| -> Option<String> {
        match x {
            Value::Object(m) => {
                let place = match (
                    m.get("file").and_then(text_of),
                    m.get("line").and_then(text_of),
                ) {
                    (Some(f), Some(l)) => format!("{f}:{l}"),
                    (Some(f), None) => f,
                    _ => m.get("doc").or(m.get("url")).and_then(text_of)?,
                };
                Some(match m.get("quote").and_then(text_of) {
                    Some(q) => format!("{place}: {q}"),
                    None => place,
                })
            }
            other => text_of(other),
        }
    };
    match v {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        Value::Null => Vec::new(),
        other => one(other).into_iter().collect(),
    }
}

/// Reads a candidates file (YAML, `format: 1`): the file's default store and its rules.
pub fn parse_candidates(
    text: &str,
    name: &str,
) -> Result<(Option<String>, Vec<Candidate>), String> {
    let v = hydo_manifest::yaml::parse_yaml(text, name).map_err(|e| e.message)?;
    let Some(top) = v.as_object() else {
        return Err(format!("{name}: a mapping with `format: 1` and `rules:`"));
    };
    match top.get("format").and_then(text_of).as_deref() {
        Some("1") => {}
        Some(f) => return Err(format!("{name}: format {f}; this hydo reads format 1")),
        None => return Err(format!("{name}: no `format: 1`")),
    }
    for k in top.keys() {
        if !["format", "store", "rules", "project", "sources"].contains(&k.as_str()) {
            return Err(format!(
                "{name}: unknown key {k} (known: format, project, sources, store, rules)"
            ));
        }
    }
    let store = top.get("store").and_then(text_of);
    let Some(rules) = top.get("rules").and_then(Value::as_array) else {
        return Err(format!("{name}: no `rules:` list"));
    };
    let known = [
        "id",
        "says",
        "kind",
        "confidence",
        "evidence",
        "store",
        "query",
        "run",
        "cases",
        "mutate",
        "within",
    ];
    let mut out: Vec<Candidate> = Vec::new();
    for (i, r) in rules.iter().enumerate() {
        let Some(m) = r.as_object() else {
            return Err(format!("{name}: rule {}: a mapping", i + 1));
        };
        let id = m.get("id").and_then(text_of).unwrap_or_default();
        let at = if id.is_empty() {
            format!("{name}: rule {}", i + 1)
        } else {
            format!("{name}: rule {id}")
        };
        if !valid_id(&id) {
            return Err(format!(
                "{at}: `id` is needed, of letters, digits, '.', '_' and '-'"
            ));
        }
        if out.iter().any(|c| c.id == id) {
            return Err(format!("{at}: a second rule with this id"));
        }
        if let Some(k) = m.keys().find(|k| !known.contains(&k.as_str())) {
            return Err(format!(
                "{at}: unknown key {k} (known: {})",
                known.join(", ")
            ));
        }
        let get = |k: &str| m.get(k).and_then(text_of).filter(|s| !s.trim().is_empty());
        let says = get("says")
            .ok_or_else(|| format!("{at}: `says` is needed (the rule in plain words)"))?;
        let kind =
            get("kind").ok_or_else(|| format!("{at}: `kind` is needed ({})", KINDS.join(", ")))?;
        if !KINDS.contains(&kind.as_str()) {
            return Err(format!("{at}: kind {kind} (known: {})", KINDS.join(", ")));
        }
        let confidence = get("confidence").unwrap_or_else(|| "medium".into());
        if !CONFIDENCES.contains(&confidence.as_str()) {
            return Err(format!(
                "{at}: confidence {confidence} (known: {})",
                CONFIDENCES.join(", ")
            ));
        }
        let c = Candidate {
            id: id.clone(),
            says,
            kind: kind.clone(),
            confidence,
            evidence: evidence_of(m.get("evidence").unwrap_or(&Value::Null)),
            store: get("store"),
            query: get("query"),
            run: get("run"),
            cases: get("cases"),
            mutate: get("mutate"),
            within: match get("within") {
                Some(w) => Some(
                    w.parse()
                        .ok()
                        .filter(|n| *n <= 3600)
                        .ok_or_else(|| format!("{at}: within {w}: seconds, at most 3600"))?,
                ),
                None => None,
            },
        };
        match kind.as_str() {
            "sql" if c.query.is_none() => {
                return Err(format!(
                    "{at}: a `sql` rule needs `query` (rows that must not exist)"
                ))
            }
            "command" if c.run.is_none() => {
                return Err(format!(
                    "{at}: a `command` rule needs `run` (the check command)"
                ))
            }
            _ => {}
        }
        out.push(c);
    }
    Ok((store, out))
}

// ---------------------------------------------------------------- the commands in the guest

/// A word for the shell: single-quoted.
pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The first primary endpoint of a `helper_sql.conf` (else its first endpoint).
pub fn primary_of_conf(conf: &str) -> Option<String> {
    let eps: Vec<Vec<&str>> = conf
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|w| w.first() == Some(&"endpoint") && w.len() >= 2)
        .collect();
    eps.iter()
        .find(|w| w.last() == Some(&"primary"))
        .or(eps.first())
        .map(|w| w[1].to_string())
}

/// The commands of one rule: (the check, its case count, its planted change).
pub fn commands_of(c: &Candidate, store: &str) -> (String, Option<String>, Option<String>) {
    match c.kind.as_str() {
        "sql" => {
            let q = c.query.as_deref().unwrap_or_default();
            let read = |s: &str| format!("hydo-sql read sql {} {}", sq(store), sq(s));
            (
                format!("hydo-sql none {} {}", sq(store), sq(q)),
                c.cases.as_deref().map(read),
                c.mutate.as_deref().map(read),
            )
        }
        _ => (
            c.run.clone().unwrap_or_default(),
            c.cases.clone(),
            c.mutate.clone(),
        ),
    }
}

/// The wrapper the guest runs for one rule: the rule (with `within`: again every 2 s of guest
/// time until it holds or the time is up), its case count, the rule's last 8 output lines, and
/// the line `hydo-rule exit=S mutate=M cases=C :: LAST`. With `mutate` the rule runs before the
/// planted change, then the change, then the rule again, and M is `<the change's exit
/// status>,<the first run's exit status>`. It exits 1 when the (last) rule failed, else 0.
pub fn wrapper(
    l: &Layout,
    check: &str,
    cases: Option<&str>,
    mutate: Option<&str>,
    within: Option<u64>,
    timeout: u64,
) -> String {
    let bb = &l.busybox;
    let inner = |cmd: &str| {
        format!(
            "{bb} timeout {timeout} {bb} sh -c {}",
            sq(&format!(". {}; {cmd}", l.prelude))
        )
    };
    let block = match within {
        Some(n) => format!(
            "t=0; while :; do o=$({} 2>&1); s=$?; [ \"$s\" = 0 ] && break; \
             [ $t -ge {n} ] && break; {bb} sleep 2; t=$((t + 2)); done; ",
            inner(check)
        ),
        None => format!("o=$({} 2>&1); s=$?; ", inner(check)),
    };
    let mut w = String::new();
    match mutate {
        // the rule before its change (it must hold), the change, the rule again
        Some(m) => w.push_str(&format!(
            "{block}ps=$s; {} >/dev/null 2>&1; m=\"$?,$ps\"; {block}",
            inner(m)
        )),
        None => w.push_str(&format!("m=-; {block}")),
    }
    match cases {
        Some(c) => w.push_str(&format!(
            "c=$({} 2>&1 | {bb} grep -v '^[[:space:]]*$' | {bb} tail -n 1 | {bb} cut -c 1-200); ",
            inner(c)
        )),
        None => w.push_str("c=-; "),
    }
    w.push_str(&format!(
        "printf '%s\\n' \"$o\" | {bb} tail -n 8 | {bb} cut -c 1-250; \
         last=$(printf '%s\\n' \"$o\" | {bb} grep -v '^[[:space:]]*$' | {bb} tail -n 1 | {bb} cut -c 1-250); \
         printf 'hydo-rule exit=%s mutate=%s cases=%s :: %s\\n' \"$s\" \"$m\" \"$c\" \"$last\"; \
         [ \"$s\" = 1 ] && exit 1; exit 0"
    ));
    w
}

/// The invariant id of a rule.
pub fn inv_id(id: &str) -> String {
    format!("rule.{id}")
}

/// Whether an invariant of the image's plan is one of the project's own checks (a composed
/// plan's `compose.sh check N`; every invariant of a plan without composed commands). The
/// composed commands' invariants (`compose.sh verdict`, the `eventually` and `finally` commands)
/// stay: they are the workload.
pub fn is_project_check(composed: bool, inv: &PlanInvariant) -> bool {
    if !composed {
        return true;
    }
    inv.cmd
        .strip_prefix(hydo_composer::SH)
        .is_some_and(|r| r.starts_with(" check "))
}

/// The rules as final invariants, after the invariants of the plan that stay.
pub fn edit_invariants(
    invs: &mut Vec<PlanInvariant>,
    composed: bool,
    keep_checks: bool,
    rules: &[(String, String, String)],
) {
    if !keep_checks {
        invs.retain(|i| !is_project_check(composed, i));
    }
    for (n, (id, msg, cmd)) in rules.iter().enumerate() {
        invs.push(PlanInvariant {
            key: format!("rule{n}"),
            id: inv_id(id),
            kind: "always".into(),
            msg: msg.clone(),
            cmd: cmd.clone(),
            every: String::new(),
            final_: true,
            live: None,
        });
    }
}

// ---------------------------------------------------------------- reading a run

/// One rule's result in one run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// The rule's exit status (`-1`: unreadable).
    pub exit: i64,
    /// The planted change's exit status, when there was one.
    pub mutate: Option<i64>,
    /// With a planted change: the rule's exit status just before it.
    pub before: Option<i64>,
    /// The case count's last line as printed (`-` when the rule has no `cases`).
    pub cases_raw: String,
    /// The rule's last output line.
    pub last: String,
}

impl Outcome {
    /// The number the case count ended with.
    pub fn cases(&self) -> Option<u64> {
        if self.cases_raw == "-" {
            return None;
        }
        let digits: Vec<&str> = self
            .cases_raw
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .collect();
        digits.last().and_then(|d| d.parse().ok())
    }
}

/// `hydo-rule exit=S mutate=M cases=C :: LAST`.
pub fn parse_summary(line: &str) -> Option<Outcome> {
    let rest = line.trim().strip_prefix("hydo-rule exit=")?;
    let (exit, rest) = rest.split_once(" mutate=")?;
    let (mutate, rest) = rest.split_once(" cases=")?;
    let (cases, last) = match rest.split_once(" :: ") {
        Some((c, l)) => (c, l),
        None => (rest.strip_suffix(" ::").unwrap_or(rest), ""),
    };
    let (mutate, before) = match mutate.trim().split_once(',') {
        Some((m, b)) => (m.parse().ok(), b.parse().ok()),
        None => (mutate.trim().parse().ok(), None),
    };
    Some(Outcome {
        exit: exit.trim().parse().unwrap_or(-1),
        mutate,
        before,
        cases_raw: cases.trim().to_string(),
        last: last.trim().to_string(),
    })
}

/// The rules' outcomes in a run's console text, by rule id (from the `HYDO-ASSERT` records of
/// the `rule.<id>` invariants).
pub fn outcomes(text: &str) -> BTreeMap<String, Outcome> {
    let mut out = BTreeMap::new();
    for l in text.lines() {
        let Some(i) = l.find("HYDO-ASSERT {") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&l[i + "HYDO-ASSERT ".len()..]) else {
            continue;
        };
        let Some(id) = v
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| s.strip_prefix("rule."))
        else {
            continue;
        };
        let line = v
            .pointer("/details/out")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(o) = parse_summary(line) {
            out.entry(id.to_string()).or_insert(o);
        }
    }
    out
}

// ---------------------------------------------------------------- the verdict per rule

pub mod status {
    pub const HOLDS: &str = "holds";
    pub const VACUOUS: &str = "vacuous";
    pub const FAILS_CLEAN: &str = "fails-clean";
    pub const UNEVALUATED: &str = "unevaluated";
    pub const NOT_RUN: &str = "not-run";
}

/// A rule's status over the clean runs (None: no result in that run) and why.
pub fn classify(c: &Candidate, runs: &[Option<Outcome>]) -> (&'static str, String) {
    if !matches!(c.kind.as_str(), "sql" | "command") {
        return (
            status::NOT_RUN,
            format!(
                "a `{}` rule needs the workload's records: write it into hydo/ and run it",
                c.kind
            ),
        );
    }
    if let Some((n, o)) = runs
        .iter()
        .enumerate()
        .find_map(|(n, o)| o.as_ref().filter(|o| o.exit == 1).map(|o| (n, o)))
    {
        return (status::FAILS_CLEAN, format!("run {}: {}", n + 1, o.last));
    }
    if let Some(n) = runs.iter().position(Option::is_none) {
        return (
            status::UNEVALUATED,
            format!(
                "run {}: the rule gave no result (the run ended before its checks)",
                n + 1
            ),
        );
    }
    let done: Vec<&Outcome> = runs.iter().flatten().collect();
    if done.is_empty() {
        return (status::UNEVALUATED, "no run".into());
    }
    if let Some((n, o)) = done.iter().enumerate().find(|(_, o)| o.exit != 0) {
        let why = match o.exit {
            2 => "it could not evaluate (exit 2)".to_string(),
            127 => "a program is missing (exit 127)".to_string(),
            143 | 124 => "it timed out".to_string(),
            e => format!("exit {e}"),
        };
        return (
            status::UNEVALUATED,
            format!("run {}: {why}: {}", n + 1, o.last),
        );
    }
    if c.cases.is_some() {
        let counts: Vec<Option<u64>> = done.iter().map(|o| o.cases()).collect();
        if counts.iter().all(Option::is_none) {
            return (
                status::UNEVALUATED,
                format!(
                    "its `cases` printed no number: {}",
                    done.last().map(|o| o.cases_raw.as_str()).unwrap_or("")
                ),
            );
        }
        if counts.iter().all(|c| matches!(c, None | Some(0))) {
            return (
                status::VACUOUS,
                "it held, but judged no case in any run".into(),
            );
        }
        let shown: Vec<String> = counts
            .iter()
            .map(|c| c.map(|n| n.to_string()).unwrap_or_else(|| "?".into()))
            .collect();
        return (status::HOLDS, format!("cases {}", shown.join(", ")));
    }
    (status::HOLDS, "cases not counted (no `cases`)".into())
}

/// The planted change's verdict: `caught` (the rule held before the change and failed after
/// it), `blind` (it held after it too) or `unevaluated`. All the changes are made in one run,
/// one rule after the other, so a rule that already fails before its own change (another
/// rule's change broke it) is `unevaluated`.
pub fn mutation_verdict(o: Option<&Outcome>) -> (&'static str, String) {
    match o {
        Some(o) if o.before.is_some_and(|b| b != 0) => (
            "unevaluated",
            format!(
                "the rule failed before its own change (exit {}): an earlier change broke it",
                o.before.unwrap_or(-1)
            ),
        ),
        Some(o) if o.mutate.is_some_and(|m| m != 0) => (
            "unevaluated",
            format!("the change itself failed (exit {})", o.mutate.unwrap_or(-1)),
        ),
        Some(o) if o.exit == 1 => ("caught", o.last.clone()),
        Some(o) if o.exit == 0 => ("blind", "the rule held after the change".into()),
        Some(o) => ("unevaluated", format!("exit {}: {}", o.exit, o.last)),
        None => ("unevaluated", "the rule gave no result".into()),
    }
}

// ---------------------------------------------------------------- check files

/// The check file a rule that holds becomes: (file name, text, executable). A rule with
/// `within` becomes an `eventually_` check that counts from the end of the `eventually`
/// commands (`after=commands`), as the rule ran in `hydo rules check`.
pub fn check_file(c: &Candidate, store: Option<&str>) -> Option<(String, String, bool)> {
    let (prefix, timing) = match c.within {
        Some(n) => ("eventually", format!("within={n} after=commands")),
        None => ("final", String::new()),
    };
    let mut head = Vec::new();
    head.push(format!("hydo-msg: {}", c.says.replace('\n', " ")));
    for e in &c.evidence {
        head.push(format!("evidence: {}", e.replace('\n', " ")));
    }
    head.push(format!(
        "proposed by rule suggestion (confidence {}); validated by hydo rules check",
        c.confidence
    ));
    match c.kind.as_str() {
        "sql" => {
            let mut t = String::new();
            let keys: Vec<String> = store
                .map(|s| format!("store={s}"))
                .into_iter()
                .chain((!timing.is_empty()).then(|| timing.clone()))
                .collect();
            if !keys.is_empty() {
                t.push_str(&format!("-- hydo: {}\n", keys.join(" ")));
            }
            for h in &head {
                t.push_str(&format!("-- {h}\n"));
            }
            t.push_str(c.query.as_deref()?);
            t.push('\n');
            Some((format!("{prefix}_{}.sql", c.id), t, false))
        }
        "command" => {
            let mut t = String::from("#!/bin/sh\n");
            if !timing.is_empty() {
                t.push_str(&format!("# hydo: {timing}\n"));
            }
            for h in &head {
                t.push_str(&format!("# {h}\n"));
            }
            t.push_str(c.run.as_deref()?);
            t.push('\n');
            Some((format!("{prefix}_{}", c.id), t, true))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- hydo rules check

struct Planned {
    cand: Candidate,
    store: Option<String>,
}

fn image_store(image: &Path) -> Option<String> {
    let conf = hydo_engine::image::embedded_file(image, "hydo/workload/helper_sql.conf").ok()?;
    primary_of_conf(&conf)
}

/// The consoles of a run directory as one text.
fn console_text(dir: &Path) -> String {
    let mut t = String::new();
    for f in ["one.log", "one.recovery.log"] {
        if let Ok(s) = std::fs::read_to_string(dir.join(f)) {
            t.push_str(&s);
            t.push('\n');
        }
    }
    t
}

/// One run with `rules` (id, message, wrapper) added to the plan; the console text.
fn run_rules(
    ctx: &Ctx,
    image: &Path,
    sched: &str,
    dir: &Path,
    eargs: &[String],
    keep_checks: bool,
    rules: &[(String, String, String)],
) -> Result<String, String> {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
    if let Some(p) = &ctx.config.pool.scripted_engine {
        // the tests: the scripted engine runs the schedule and writes one.log
        let list: Vec<Value> = rules
            .iter()
            .map(|(id, _, cmd)| json!({"id": id, "cmd": cmd}))
            .collect();
        let f = dir.join("rules.json");
        let _ = std::fs::write(&f, serde_json::to_string_pretty(&list).unwrap_or_default());
        let o = Command::new(p)
            .arg("one")
            .arg(dir)
            .arg(sched)
            .env("HYDO_SCRIPTED_IMAGE", image)
            .env("HYDO_SCRIPTED_RULES", &f)
            .output()
            .map_err(|e| format!("{}: {e}", p.display()))?;
        let _ = std::fs::write(dir.join("engine.log"), &o.stdout);
        return Ok(console_text(dir));
    }
    let edit = |plan: &mut hydo_control::Plan| {
        let composed = plan.composed();
        edit_invariants(&mut plan.invariants, composed, keep_checks, rules);
    };
    let (res, out) = run_with_plan(ctx, image, sched, dir, eargs, &edit)?;
    if res.is_none() {
        let _ = std::fs::write(dir.join("engine.log"), &out);
    }
    Ok(console_text(dir))
}

fn wrapped(
    p: &Planned,
    layout: &Layout,
    timeout: u64,
    with_mutation: bool,
) -> (String, String, String) {
    let (check, cases, mutate) = commands_of(&p.cand, p.store.as_deref().unwrap_or("db"));
    let w = wrapper(
        layout,
        &check,
        cases.as_deref(),
        if with_mutation {
            mutate.as_deref()
        } else {
            None
        },
        p.cand.within,
        timeout,
    );
    (p.cand.id.clone(), p.cand.says.clone(), w)
}

fn cmd_check(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return 0;
    }
    let runs: u64 = take(&mut args, "--runs")
        .map(|v| {
            v.parse()
                .ok()
                .filter(|n| (1..=50).contains(n))
                .unwrap_or_else(|| die(&format!("--runs {v}: a number from 1 to 50")))
        })
        .unwrap_or(3);
    let fixed = take(&mut args, "--schedule");
    let mutate = take_flag(&mut args, "--mutate");
    let keep_checks = take_flag(&mut args, "--keep-checks");
    let timeout: u64 = take(&mut args, "--timeout")
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| die(&format!("--timeout {v}: seconds")))
        })
        .unwrap_or(120);
    let out = take(&mut args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(OUT).join("rules"));
    let write_checks = take(&mut args, "--write-checks").map(PathBuf::from);
    let opts = RunOpts::take(ctx, &mut args);
    if args.is_empty() {
        die("usage: hydo rules check <candidates.yaml> [IMG] [options] (hydo rules --help)");
    }
    let file = args.remove(0);
    let image = find_image(ctx, &mut args);
    if let Some(a) = args.first() {
        die(&format!(
            "hydo rules check: unexpected argument {a} (hydo rules --help)"
        ));
    }
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| die(&format!("{file}: {e}")));
    let (default_store, cands) = parse_candidates(&text, &file).unwrap_or_else(|e| die(&e));
    if cands.is_empty() {
        die(&format!("{file}: no rules"));
    }
    let scripted = ctx.config.pool.scripted_engine.is_some();
    let plan = if scripted {
        None
    } else {
        Some(
            hydo_control::agent::plan_of_image(&image)
                .unwrap_or_else(|e| die(&format!("{}: {e}", image.display()))),
        )
    };
    let layout = plan.as_ref().map(|p| p.layout.clone()).unwrap_or_default();
    let composed = plan.as_ref().map(|p| p.composed()).unwrap_or(true);
    // browser.explore (Milestone 258): the runs without faults are walks, one seed each
    let walks = plan.as_ref().is_some_and(|p| p.walk);
    let needs_store = cands
        .iter()
        .any(|c| c.kind == "sql" && c.store.is_none() && default_store.is_none());
    let img_store = if needs_store {
        image_store(&image)
    } else {
        None
    };
    if needs_store && img_store.is_none() {
        die(&format!(
            "{file}: a `sql` rule without `store:`, and {} has no readable \
             /hydo/workload/helper_sql.conf: name the endpoint with `store:`",
            image.display()
        ));
    }
    let planned: Vec<Planned> = cands
        .into_iter()
        .map(|c| Planned {
            store: c
                .store
                .clone()
                .or_else(|| default_store.clone())
                .or_else(|| img_store.clone()),
            cand: c,
        })
        .collect();
    let schedules: Vec<String> = match &fixed {
        Some(s) => vec![s.clone()],
        None if composed => (1..=runs)
            .map(|n| format!("disk=none | run W={n:08x}"))
            .collect(),
        None if walks => (1..=runs)
            .map(|n| format!("disk=none | run I={n:x}"))
            .collect(),
        None => vec!["disk=none | run".to_string()],
    };
    let runnable: Vec<&Planned> = planned
        .iter()
        .filter(|p| matches!(p.cand.kind.as_str(), "sql" | "command"))
        .collect();
    let eargs = opts.eargs(&image);
    let _ = std::fs::create_dir_all(&out);
    println!(
        "hydo rules check: {} candidates ({} to run), {} clean run(s) on {}",
        planned.len(),
        runnable.len(),
        schedules.len(),
        image.display()
    );
    let rules: Vec<(String, String, String)> = runnable
        .iter()
        .map(|p| wrapped(p, &layout, timeout, false))
        .collect();
    let mut per_run: Vec<BTreeMap<String, Outcome>> = Vec::new();
    let mut checkouts: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (n, s) in schedules.iter().enumerate() {
        let dir = out.join(format!("run-{}", n + 1));
        println!(
            "hydo rules check: run {} of {}: {s}",
            n + 1,
            schedules.len()
        );
        let t = if runnable.is_empty() {
            String::new()
        } else {
            run_rules(ctx, &image, s, &dir, &eargs, keep_checks, &rules)
                .unwrap_or_else(|w| die(&format!("not run (no VM was started): {w}")))
        };
        for (id, (lines, _)) in hydo_explore::results::check_outputs(&t) {
            if let Some(r) = id.strip_prefix("rule.") {
                checkouts.entry(r.to_string()).or_insert(lines);
            }
        }
        let o = outcomes(&t);
        if !runnable.is_empty() && o.is_empty() {
            println!(
                "hydo rules check: run {}: no rule gave a result (see {})",
                n + 1,
                dir.join("engine.log").display()
            );
        }
        per_run.push(o);
    }
    let mut results: Vec<(String, String)> = Vec::new();
    let mut docs: Vec<Value> = Vec::new();
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for p in &planned {
        let id = &p.cand.id;
        let rs: Vec<Option<Outcome>> = per_run.iter().map(|m| m.get(id).cloned()).collect();
        let (st, why) = classify(&p.cand, &rs);
        *counts.entry(st).or_default() += 1;
        let mut d = Map::new();
        d.insert("id".into(), json!(id));
        d.insert("says".into(), json!(p.cand.says));
        d.insert("kind".into(), json!(p.cand.kind));
        d.insert("confidence".into(), json!(p.cand.confidence));
        d.insert("evidence".into(), json!(p.cand.evidence));
        d.insert("status".into(), json!(st));
        d.insert("why".into(), json!(why));
        d.insert(
            "runs".into(),
            Value::Array(
                rs.iter()
                    .zip(&schedules)
                    .map(|(o, s)| match o {
                        Some(o) => json!({"schedule": s, "exit": o.exit, "cases": o.cases(), "last": o.last}),
                        None => json!({"schedule": s, "exit": null}),
                    })
                    .collect(),
            ),
        );
        if let Some(l) = checkouts.get(id) {
            d.insert("output".into(), Value::Array(l.clone()));
        }
        docs.push(Value::Object(d));
        results.push((st.to_string(), why));
    }
    // the planted changes: one more run, in which each rule that holds and has a change runs,
    // then its change is made, then it runs again; a rule that an earlier rule's change already
    // broke gets a run of its own
    let changed: Vec<usize> = (0..planned.len())
        .filter(|&i| results[i].0 == status::HOLDS && planned[i].cand.mutate.is_some())
        .collect();
    if mutate && !changed.is_empty() {
        let dir = out.join("mutate");
        println!(
            "hydo rules check: planted changes for {} rule(s): {}",
            changed.len(),
            schedules[0]
        );
        let list: Vec<(String, String, String)> = changed
            .iter()
            .map(|&i| wrapped(&planned[i], &layout, timeout, true))
            .collect();
        let t = run_rules(ctx, &image, &schedules[0], &dir, &eargs, keep_checks, &list)
            .unwrap_or_else(|w| die(&format!("not run (no VM was started): {w}")));
        let o = outcomes(&t);
        for &i in &changed {
            let mut got = o.get(&planned[i].cand.id).cloned();
            if got
                .as_ref()
                .is_some_and(|x| x.before.is_some_and(|b| b != 0))
            {
                let id = &planned[i].cand.id;
                println!(
                    "hydo rules check: planted change for {id} alone (an earlier change broke it)"
                );
                let one = vec![wrapped(&planned[i], &layout, timeout, true)];
                let t = run_rules(
                    ctx,
                    &image,
                    &schedules[0],
                    &out.join(format!("mutate-{id}")),
                    &eargs,
                    keep_checks,
                    &one,
                )
                .unwrap_or_else(|w| die(&format!("not run (no VM was started): {w}")));
                got = outcomes(&t).get(id).cloned();
            }
            let (v, why) = mutation_verdict(got.as_ref());
            if let Some(d) = docs[i].as_object_mut() {
                d.insert("mutation".into(), json!({"verdict": v, "why": why}));
            }
        }
    }
    // the report
    let w = planned.iter().map(|p| p.cand.id.len()).max().unwrap_or(4);
    println!();
    for (i, p) in planned.iter().enumerate() {
        let m = docs[i]
            .get("mutation")
            .and_then(|m| m.get("verdict"))
            .and_then(Value::as_str)
            .map(|v| format!(" [change {v}]"))
            .unwrap_or_default();
        println!(
            "  {:<12} {:<w$}  {}{m}",
            results[i].0,
            p.cand.id,
            p.cand.says.lines().next().unwrap_or(""),
        );
        println!("  {:<12} {:<w$}  {}", "", "", results[i].1);
    }
    let n = |s: &str| counts.get(s).copied().unwrap_or(0);
    let summary = json!({
        "proposed": planned.len(),
        "holds": n(status::HOLDS),
        "vacuous": n(status::VACUOUS),
        "fails_clean": n(status::FAILS_CLEAN),
        "unevaluated": n(status::UNEVALUATED),
        "not_run": n(status::NOT_RUN),
    });
    println!(
        "\nhydo rules check: {} proposed: {} hold, {} vacuous, {} fail without faults, {} unevaluated, {} not run",
        planned.len(),
        n(status::HOLDS),
        n(status::VACUOUS),
        n(status::FAILS_CLEAN),
        n(status::UNEVALUATED),
        n(status::NOT_RUN)
    );
    let doc = json!({
        "format": 1,
        "candidates": abspath(Path::new(&file)).display().to_string(),
        "image": image.display().to_string(),
        "schedules": schedules,
        "keep_checks": keep_checks,
        "rules": docs,
        "summary": summary,
    });
    let jf = out.join("rules-check.json");
    let _ = std::fs::write(
        &jf,
        serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n",
    );
    println!("hydo rules check: results in {}", jf.display());
    if let Some(dir) = write_checks {
        let _ = std::fs::create_dir_all(&dir);
        let mut written = 0;
        for (i, p) in planned.iter().enumerate() {
            if results[i].0 != status::HOLDS {
                continue;
            }
            let Some((name, text, exec)) = check_file(&p.cand, p.store.as_deref()) else {
                continue;
            };
            let f = dir.join(&name);
            if let Err(e) = std::fs::write(&f, text) {
                die(&format!("{}: {e}", f.display()));
            }
            if exec {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755));
            }
            written += 1;
        }
        println!(
            "hydo rules check: wrote {written} check file(s) into {}",
            dir.display()
        );
    }
    0
}

// ---------------------------------------------------------------- hydo rules scan

/// One piece of evidence: its kind, file, line number and text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub kind: &'static str,
    pub file: String,
    pub line: usize,
    pub text: String,
}

/// The kinds of evidence, in the order the scan prints them.
pub const EVIDENCE_KINDS: &[(&str, &str)] = &[
    (
        "schema",
        "constraints of the stored data: CHECK, UNIQUE, foreign keys, enumerations",
    ),
    (
        "state",
        "status and state columns: the values a record moves through",
    ),
    (
        "api",
        "API contracts: idempotency keys, conflict and validation answers, limits",
    ),
    ("docs", "promise words in the documentation"),
    (
        "tests",
        "the project's tests: assertions about refusals, duplicates and limits",
    ),
];

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    ".hydo",
    "hydo-out",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    "coverage",
    "third_party",
    ".cache",
];

const CODE_EXT: &[&str] = &[
    "go", "rb", "py", "ts", "tsx", "js", "jsx", "mjs", "java", "kt", "cs", "php", "rs", "ex",
    "exs", "scala", "swift",
];

fn has_any(l: &str, words: &[&str]) -> bool {
    words.iter().any(|w| l.contains(w))
}

/// Whether `l` (lower case) contains `w` as a whole word.
fn has_word(l: &str, w: &str) -> bool {
    l.match_indices(w).any(|(i, _)| {
        let before = l[..i].chars().next_back();
        let after = l[i + w.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric())
            && !after.is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// What kind of source a file is, by its path (lower case) and its first bytes.
fn source_kind(rel: &str, head: &str) -> Option<&'static str> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let ext = name.rsplit_once('.').map(|x| x.1).unwrap_or("");
    let parts: Vec<&str> = rel.split('/').collect();
    let dirs = &parts[..parts.len().saturating_sub(1)];
    let test_dir = dirs.iter().any(|d| {
        matches!(
            *d,
            "test" | "tests" | "spec" | "specs" | "__tests__" | "e2e" | "integration"
        )
    });
    let test_name = name.ends_with("_test.go")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_spec.rb")
        || name.ends_with("_test.rb")
        || name.ends_with("_test.py")
        || (name.starts_with("test_") && ext == "py")
        || name.ends_with("test.java")
        || name.ends_with("tests.cs");
    if matches!(ext, "md" | "mdx" | "rst" | "adoc") {
        return Some("docs");
    }
    if matches!(ext, "yaml" | "yml" | "json")
        && (head.contains("openapi") || head.contains("swagger"))
    {
        return Some("api");
    }
    if ext == "proto" || ext == "graphql" || ext == "graphqls" {
        return Some("api");
    }
    if CODE_EXT.contains(&ext) && (test_dir || test_name) {
        return Some("tests");
    }
    let schema_path = ext == "sql"
        || ext == "prisma"
        || name == "schema.rb"
        || dirs.iter().any(|d| {
            d.contains("migrat")
                || matches!(
                    *d,
                    "models" | "model" | "entities" | "entity" | "schema" | "schemas" | "db"
                )
        });
    if schema_path && (ext == "sql" || ext == "prisma" || CODE_EXT.contains(&ext)) {
        return Some("schema");
    }
    None
}

const SCHEMA_WORDS: &[&str] = &[
    "check (",
    "check(",
    "unique",
    "references ",
    "foreign key",
    "foreign_key",
    "foreignkey",
    "exclude using",
    " as enum",
    "check_constraint",
    "checkconstraint",
    "@check",
    "@relation",
    "on delete",
    "validates",
    "validate :",
    "exclusion_constraint",
    "limit",
    "atomic",
    "idempoten",
    "allow_",
    "maximum",
    "minimum",
];

const DOC_WORDS: &[&str] = &[
    "exactly once",
    "exactly-once",
    "at most once",
    "at-most-once",
    "at least once",
    "at-least-once",
    "idempoten",
    "guarantee",
    "atomic",
    "duplicate",
    "must not",
    "must never",
    "never ",
    "cannot exceed",
    "can not exceed",
    "not exceed",
    "not allowed",
    "only once",
    "is rejected",
    "are rejected",
    "will be rejected",
    "will fail",
    "no more than",
    "at most ",
    "consisten",
    "unique",
    "irreversible",
    "immutable",
];

const API_WORDS: &[&str] = &[
    "idempoten",
    "'409'",
    "\"409\"",
    "409:",
    "'422'",
    "\"422\"",
    "422:",
    "conflict",
    "duplicate",
    "maximum:",
    "minimum:",
    "exclusiveminimum",
    "maxlength",
    "already exists",
];

const TEST_ASSERT: &[&str] = &[
    "assert", "expect", "should", "require.", "t.error", "t.fatal", "raises", "tothrow",
    "to_raise", "throws",
];

const TEST_WORDS: &[&str] = &[
    "duplicate",
    "already",
    "exceed",
    "insufficient",
    "negative",
    "limit",
    "conflict",
    "409",
    "422",
    "twice",
    "once",
    "not allowed",
    "invalid",
    "reject",
    "unique",
    "idempoten",
    "forbidden",
];

/// The evidence one line gives, for a file of `kind`.
pub fn line_evidence(kind: &'static str, line: &str) -> Option<&'static str> {
    let l = line.to_lowercase();
    match kind {
        "schema" => {
            // a query's `LIMIT n ... OFFSET m` pages rows: it is no promise
            let paging = l.contains("offset") || (has_word(&l, "limit") && l.contains("select"));
            let words = if paging {
                l.replace("limit", "")
            } else {
                l.clone()
            };
            if has_any(&words, SCHEMA_WORDS) {
                Some("schema")
            } else if has_word(&l, "status")
                || has_word(&l, "state")
                || l.contains("_status")
                || l.contains("_state")
                || l.contains("enum ")
                || l.contains("enum(")
            {
                Some("state")
            } else {
                None
            }
        }
        "api" => has_any(&l, API_WORDS).then_some("api"),
        "docs" => has_any(&l, DOC_WORDS).then_some("docs"),
        "tests" => (has_any(&l, TEST_ASSERT) && has_any(&l, TEST_WORDS)).then_some("tests"),
        _ => None,
    }
}

pub(crate) fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut ents: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(&p, files);
            }
        } else if ft.is_file() {
            files.push(p);
        }
    }
}

/// The evidence of every file under `root`, in path order. A file of documentation or tests gives
/// at most `per_file` lines; a schema or API file gives all of its lines (a schema dump holds the
/// constraints of every table in one file).
pub fn scan(root: &Path, per_file: usize) -> (usize, Vec<Evidence>) {
    let mut files = Vec::new();
    walk(root, &mut files);
    let mut out = Vec::new();
    let mut read = 0;
    for f in files {
        let rel = f
            .strip_prefix(root)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(meta) = std::fs::metadata(&f) else {
            continue;
        };
        if meta.len() > 2_000_000 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let head: String = text.chars().take(2048).collect::<String>().to_lowercase();
        let Some(kind) = source_kind(&rel.to_lowercase(), &head) else {
            continue;
        };
        read += 1;
        let most = if matches!(kind, "docs" | "tests") {
            per_file
        } else {
            usize::MAX
        };
        let mut n = 0;
        for (i, line) in text.lines().enumerate() {
            if n >= most {
                break;
            }
            if let Some(k) = line_evidence(kind, line) {
                let t = line.trim();
                let t: String = t.chars().take(200).collect();
                out.push(Evidence {
                    kind: k,
                    file: rel.clone(),
                    line: i + 1,
                    text: t,
                });
                n += 1;
            }
        }
    }
    (read, out)
}

fn cmd_scan(argv: &[String]) -> i32 {
    let mut args = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return 0;
    }
    let as_json = take_flag(&mut args, "--json");
    let all = take_flag(&mut args, "--all");
    let only = take(&mut args, "--kind");
    let manifest = take(&mut args, "--manifest");
    if let Some(k) = &only {
        if k != "clock" && k != "writes" && !EVIDENCE_KINDS.iter().any(|(n, _)| n == k) {
            let names: Vec<&str> = EVIDENCE_KINDS.iter().map(|x| x.0).collect();
            die(&format!(
                "--kind {k}: one of {}, clock, writes",
                names.join(", ")
            ));
        }
    }
    let max: usize = take(&mut args, "--max")
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| die(&format!("--max {v}: a number")))
        })
        .unwrap_or(200);
    let dir = args.first().cloned().unwrap_or_else(|| ".".into());
    if args.len() > 1 {
        die("usage: hydo rules scan [DIR] [--kind K] [--json] [--max N]");
    }
    let root = Path::new(&dir);
    if !root.is_dir() {
        die(&format!("{dir}: not a directory"));
    }
    if only.as_deref() == Some("clock") {
        let svcs = crate::clockscan::services_near(root, manifest.as_deref());
        let a = crate::clockscan::audit(root);
        if as_json {
            println!("{}", crate::clockscan::json_report(&dir, &a, &svcs));
        } else {
            print!(
                "{}",
                crate::clockscan::text_report(&dir, &a, &svcs, max, all)
            );
        }
        return 0;
    }
    if only.as_deref() == Some("writes") {
        let w = crate::writescan::scan_writes(root);
        if as_json {
            println!("{}", crate::writescan::json_report(&dir, &w));
        } else {
            print!("{}", crate::writescan::text_report(&dir, &w, max, all));
        }
        return 0;
    }
    let (read, mut ev) = scan(root, 40);
    if let Some(k) = &only {
        ev.retain(|e| e.kind == k.as_str());
    }
    if as_json {
        let items: Vec<Value> = ev
            .iter()
            .map(|e| json!({"kind": e.kind, "file": e.file, "line": e.line, "text": e.text}))
            .collect();
        let mut doc = json!({"dir": dir, "files_read": read, "evidence": items});
        if only.is_none() {
            let matches = crate::clockscan::audit(root).matches;
            doc["clock"] = Value::Array(matches.iter().map(crate::clockscan::match_json).collect());
            let w = crate::writescan::scan_writes(root);
            doc["writes"] = Value::Array(
                w.routes
                    .iter()
                    .enumerate()
                    .map(|(i, r)| crate::writescan::route_json(r, i + 1))
                    .collect(),
            );
        }
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return 0;
    }
    println!(
        "hydo rules scan: {} lines of evidence in {read} files of {dir}",
        ev.len()
    );
    for (k, what) in EVIDENCE_KINDS {
        if only.as_deref().is_some_and(|o| o != *k) {
            continue;
        }
        let of: Vec<&Evidence> = ev.iter().filter(|e| e.kind == *k).collect();
        println!("\n{k} ({}): {what}", of.len());
        for e in of.iter().take(max) {
            println!("  {}:{}: {}", e.file, e.line, e.text);
        }
        if of.len() > max {
            println!("  ... {} more (--max N, or --json for all)", of.len() - max);
        }
    }
    if only.is_none() {
        // the clock audit's places, one line each; --kind clock gives the schedules
        let matches = crate::clockscan::audit(root).matches;
        println!(
            "\nclock ({}): durations measured with the wall clock (--kind clock proposes the schedules that test them)",
            matches.len()
        );
        for m in matches.iter().take(max) {
            println!("  {}:{}: [{}] {}", m.file, m.line, m.confidence, m.text);
        }
        if matches.len() > max {
            println!(
                "  ... {} more (--max N, or --json for all)",
                matches.len() - max
            );
        }
        // the routes that write, one line each; --kind writes gives the handlers
        let w = crate::writescan::scan_writes(root);
        println!(
            "\nwrites ({}): routes of the API that write (--kind writes names each handler and why)",
            w.routes.len()
        );
        for r in w.routes.iter().take(max) {
            println!(
                "  {}:{}: [{}] {} {}",
                r.file, r.line, r.class, r.method, r.path
            );
        }
        if w.routes.len() > max {
            println!(
                "  ... {} more (--max N, or --json for all)",
                w.routes.len() - max
            );
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"format: 1
store: db
rules:
  - id: no_overdraft
    says: A balance never goes below minus its overdraft limit.
    kind: sql
    confidence: high
    evidence:
      - {file: docs/overdrafts.md, line: 12, quote: "succeeds only if within the limit"}
      - db/migrations/0003.sql:40
    query: |
      SELECT id FROM balances WHERE balance < -overdraft_limit
    cases: SELECT count(*) FROM balances WHERE overdraft_limit > 0
    mutate: UPDATE balances SET balance = -overdraft_limit - 1 WHERE overdraft_limit > 0
  - id: acked_kept
    says: Every operation the client saw acknowledged is in the books.
    kind: command
    run: /bin/client rule acked
  - id: once
    says: Each message is delivered once.
    kind: messages
"#;

    #[test]
    fn a_candidates_file_is_read() {
        let (store, c) = parse_candidates(FILE, "rules.yaml").unwrap();
        assert_eq!(store.as_deref(), Some("db"));
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].kind, "sql");
        assert_eq!(c[0].confidence, "high");
        assert_eq!(
            c[0].evidence,
            vec![
                "docs/overdrafts.md:12: succeeds only if within the limit".to_string(),
                "db/migrations/0003.sql:40".to_string()
            ]
        );
        assert_eq!(
            c[0].query.as_deref(),
            Some("SELECT id FROM balances WHERE balance < -overdraft_limit")
        );
        assert_eq!(c[1].confidence, "medium");
        assert_eq!(c[2].kind, "messages");
    }

    #[test]
    fn a_bad_candidates_file_is_refused_with_its_place() {
        let e = |t: &str| parse_candidates(t, "f.yaml").unwrap_err();
        assert!(e("rules: []\n").contains("no `format: 1`"));
        assert!(e("format: 1\nrules:\n  - id: a b\n    says: x\n    kind: sql\n").contains("`id`"));
        assert!(
            e("format: 1\nrules:\n  - id: a\n    says: x\n    kind: sql\n")
                .contains("needs `query`")
        );
        assert!(
            e("format: 1\nrules:\n  - id: a\n    says: x\n    kind: guess\n")
                .contains("kind guess")
        );
        assert!(e("format: 1\nrules:\n  - id: a\n    says: x\n    kind: command\n    run: x\n    colour: red\n")
            .contains("unknown key colour"));
        assert!(e("format: 1\nrules:\n  - id: a\n    says: x\n    kind: command\n    run: x\n  - id: a\n    says: y\n    kind: command\n    run: y\n")
            .contains("a second rule"));
    }

    #[test]
    fn a_sql_rule_runs_through_hydo_sql_with_its_store() {
        let (_, c) = parse_candidates(FILE, "f").unwrap();
        let (check, cases, mutate) = commands_of(&c[0], "db");
        assert_eq!(
            check,
            "hydo-sql none 'db' 'SELECT id FROM balances WHERE balance < -overdraft_limit'"
        );
        assert_eq!(
            cases.unwrap(),
            "hydo-sql read sql 'db' 'SELECT count(*) FROM balances WHERE overdraft_limit > 0'"
        );
        assert!(mutate
            .unwrap()
            .starts_with("hydo-sql read sql 'db' 'UPDATE"));
        assert_eq!(sq("it's"), r"'it'\''s'");
    }

    #[test]
    fn the_wrapper_reports_and_fails_only_on_a_violation() {
        let l = Layout::default();
        let w = wrapper(&l, "true", Some("echo 3"), None, None, 60);
        assert!(w.starts_with("m=-; o=$(/bin/busybox timeout 60 /bin/busybox sh -c '. /etc/hydo/prelude.sh; true' 2>&1); s=$?;"));
        assert!(w.contains("printf 'hydo-rule exit=%s mutate=%s cases=%s :: %s\\n'"));
        assert!(w.ends_with("[ \"$s\" = 1 ] && exit 1; exit 0"));
        let m = wrapper(&l, "true", None, Some("rm x"), None, 60);
        assert!(m.starts_with("o=$(/bin/busybox timeout 60 /bin/busybox sh -c '. /etc/hydo/prelude.sh; true' 2>&1); s=$?; ps=$s; /bin/busybox timeout 60 /bin/busybox sh -c '. /etc/hydo/prelude.sh; rm x' >/dev/null 2>&1; m=\"$?,$ps\"; o=$("));
        assert!(m.contains("c=-;"));
    }

    /// The wrapper in a real shell: the summary line, the exit status, the case count.
    #[test]
    fn the_wrapper_runs_in_a_shell() {
        let dir = std::env::temp_dir().join(format!("hydo-rules-w-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let prelude = dir.join("prelude.sh");
        std::fs::write(&prelude, "X=1\n").unwrap();
        // busybox's applets as plain commands: `sh`, `timeout`, `grep`, `tail`, `cut`
        let bb = dir.join("bb");
        std::fs::write(
            &bb,
            "#!/bin/sh\ncase $1 in timeout) shift 2;; esac\nexec \"$@\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bb, std::fs::Permissions::from_mode(0o755)).unwrap();
        let l = Layout {
            busybox: bb.display().to_string(),
            prelude: prelude.display().to_string(),
            ..Layout::default()
        };
        let sh = |w: &str| {
            let o = Command::new("/bin/sh").arg("-c").arg(w).output().unwrap();
            (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).into_owned(),
            )
        };
        let (rc, out) = sh(&wrapper(
            &l,
            "echo two rows; echo 'db: 2 rows where none may be'; exit 1",
            Some("echo '{\"count\":\"7\"}'"),
            None,
            None,
            10,
        ));
        assert_eq!(rc, 1);
        let last = out.lines().last().unwrap();
        let o = parse_summary(last).unwrap();
        assert_eq!(
            (o.exit, o.cases(), o.last.as_str()),
            (1, Some(7), "db: 2 rows where none may be")
        );
        let (rc, out) = sh(&wrapper(
            &l,
            "echo 'not reachable' >&2; exit 2",
            None,
            Some("true"),
            None,
            10,
        ));
        assert_eq!(rc, 0);
        let o = parse_summary(out.lines().last().unwrap()).unwrap();
        assert_eq!(
            (o.exit, o.mutate, o.before, o.cases(), o.last.as_str()),
            (2, Some(0), Some(2), None, "not reachable")
        );
        // `within`: the rule is tried again until it holds (here on its second try)
        let flag = dir.join("flag");
        let check = format!(
            "if [ -e {f} ]; then echo invoiced; exit 0; fi; echo > {f}; echo 'not yet'; exit 1",
            f = flag.display()
        );
        let (rc, out) = sh(&wrapper(&l, &check, None, None, Some(4), 10));
        assert_eq!(rc, 0);
        let o = parse_summary(out.lines().last().unwrap()).unwrap();
        assert_eq!((o.exit, o.last.as_str()), (0, "invoiced"));
        // and it fails when the time is up
        let (rc, out) = sh(&wrapper(
            &l,
            "echo 'never'; exit 1",
            None,
            None,
            Some(2),
            10,
        ));
        assert_eq!(rc, 1);
        assert_eq!(
            parse_summary(out.lines().last().unwrap()).unwrap().last,
            "never"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rules_are_read_from_the_assert_records() {
        let text = concat!(
            "HYDO-ASSERT {\"t\":\"always\",\"id\":\"rule.a\",\"ok\":true,\"msg\":\"m\",\"details\":{\"out\":\"hydo-rule exit=0 mutate=- cases={\\\"count\\\":\\\"4\\\"} :: db: no rows\"}}\n",
            "noise\n",
            "HYDO-ASSERT {\"t\":\"always\",\"id\":\"rule.b\",\"ok\":false,\"msg\":\"m\",\"details\":{\"out\":\"hydo-rule exit=1 mutate=- cases=- :: db: 3 rows where none may be\"}}\n",
            "HYDO-ASSERT {\"t\":\"always\",\"id\":\"app.other\",\"ok\":true,\"msg\":\"m\",\"details\":{\"out\":\"x\"}}\n",
        );
        let o = outcomes(text);
        assert_eq!(o.len(), 2);
        assert_eq!(o["a"].cases(), Some(4));
        assert_eq!(o["b"].exit, 1);
        assert_eq!(o["b"].last, "db: 3 rows where none may be");
    }

    fn out(exit: i64, cases: &str) -> Option<Outcome> {
        Some(Outcome {
            exit,
            mutate: None,
            before: None,
            cases_raw: cases.into(),
            last: "l".into(),
        })
    }

    #[test]
    fn each_status_has_its_case() {
        let mut c = Candidate {
            id: "a".into(),
            says: "x".into(),
            kind: "sql".into(),
            cases: Some("q".into()),
            ..Candidate::default()
        };
        assert_eq!(classify(&c, &[out(0, "3"), out(0, "0")]).0, status::HOLDS);
        assert_eq!(
            classify(&c, &[out(0, "0"), out(0, "{\"n\":0}")]).0,
            status::VACUOUS
        );
        assert_eq!(
            classify(&c, &[out(0, "3"), out(1, "3")]).0,
            status::FAILS_CLEAN
        );
        assert_eq!(classify(&c, &[out(0, "3"), None]).0, status::UNEVALUATED);
        assert_eq!(classify(&c, &[out(2, "3")]).0, status::UNEVALUATED);
        assert_eq!(classify(&c, &[out(0, "none")]).0, status::UNEVALUATED);
        c.cases = None;
        assert_eq!(
            classify(&c, &[out(0, "-")]),
            (status::HOLDS, "cases not counted (no `cases`)".into())
        );
        c.kind = "history".into();
        assert_eq!(classify(&c, &[]).0, status::NOT_RUN);
        let m = |e: i64, mu: Option<i64>, b: i64| Outcome {
            exit: e,
            mutate: mu,
            before: Some(b),
            ..Outcome::default()
        };
        assert_eq!(mutation_verdict(Some(&m(1, Some(0), 0))).0, "caught");
        assert_eq!(mutation_verdict(Some(&m(0, Some(0), 0))).0, "blind");
        assert_eq!(mutation_verdict(Some(&m(1, Some(1), 0))).0, "unevaluated");
        assert_eq!(mutation_verdict(Some(&m(1, Some(0), 1))).0, "unevaluated");
        assert_eq!(mutation_verdict(None).0, "unevaluated");
        let o = parse_summary("hydo-rule exit=1 mutate=0,0 cases=- :: x").unwrap();
        assert_eq!((o.mutate, o.before), (Some(0), Some(0)));
    }

    #[test]
    fn the_projects_checks_leave_and_the_commands_stay() {
        let inv = |id: &str, cmd: &str| PlanInvariant {
            key: id.into(),
            id: id.into(),
            kind: "always".into(),
            msg: id.into(),
            cmd: cmd.into(),
            every: String::new(),
            final_: true,
            live: None,
        };
        let sh = hydo_composer::SH;
        let mut v = vec![
            inv("compose.verdict", &format!("{sh} verdict")),
            inv("compose.settle", &format!("{sh} end 3")),
            inv("app.books", &format!("{sh} check 0")),
        ];
        edit_invariants(
            &mut v,
            true,
            false,
            &[("r1".into(), "m".into(), "true".into())],
        );
        let ids: Vec<&str> = v.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["compose.verdict", "compose.settle", "rule.r1"]);
        assert!(v[2].final_);
        let mut w = vec![inv("app.books", "/bin/check")];
        edit_invariants(
            &mut w,
            false,
            true,
            &[("r1".into(), "m".into(), "true".into())],
        );
        assert_eq!(w.len(), 2);
        let mut w = vec![inv("app.books", "/bin/check")];
        edit_invariants(&mut w, false, false, &[]);
        assert!(w.is_empty());
    }

    #[test]
    fn a_rule_that_holds_becomes_a_check_file() {
        let (_, c) = parse_candidates(FILE, "f").unwrap();
        let (n, t, x) = check_file(&c[0], Some("db")).unwrap();
        assert_eq!(n, "final_no_overdraft.sql");
        assert!(!x);
        assert!(t.starts_with("-- hydo: store=db\n-- hydo-msg: A balance never goes below"));
        assert!(t.contains("-- evidence: db/migrations/0003.sql:40\n"));
        assert!(t.ends_with("WHERE balance < -overdraft_limit\n"));
        let (n, t, x) = check_file(&c[1], None).unwrap();
        assert_eq!(n, "final_acked_kept");
        assert!(x);
        assert!(t.starts_with("#!/bin/sh\n# hydo-msg: Every operation"));
        assert!(check_file(&c[2], None).is_none());
        let mut late = c[0].clone();
        late.within = Some(120);
        let (n, t, _) = check_file(&late, Some("db")).unwrap();
        assert_eq!(n, "eventually_no_overdraft.sql");
        assert!(t.starts_with("-- hydo: store=db within=120 after=commands\n"));
    }

    #[test]
    fn the_store_is_the_first_primary() {
        let conf = "dialect postgres\nendpoint r1 postgres://x replica\nendpoint db postgres://y primary\n";
        assert_eq!(primary_of_conf(conf).as_deref(), Some("db"));
        assert_eq!(
            primary_of_conf("endpoint a x replica\n").as_deref(),
            Some("a")
        );
        assert_eq!(primary_of_conf("dialect x\n"), None);
    }

    #[test]
    fn evidence_kinds_by_file_and_line() {
        assert_eq!(
            source_kind("db/migrations/001_init.sql", ""),
            Some("schema")
        );
        assert_eq!(source_kind("app/models/invoice.rb", ""), Some("schema"));
        assert_eq!(source_kind("docs/guide.md", ""), Some("docs"));
        assert_eq!(
            source_kind("api/openapi.yaml", "openapi: 3.0.0"),
            Some("api")
        );
        assert_eq!(source_kind("config.yaml", "name: x"), None);
        assert_eq!(source_kind("internal/ledger_test.go", ""), Some("tests"));
        assert_eq!(
            source_kind("spec/models/invoice_spec.rb", ""),
            Some("tests")
        );
        assert_eq!(source_kind("src/main.go", ""), None);
        assert_eq!(
            line_evidence("schema", "  amount BIGINT CHECK (amount >= 0),"),
            Some("schema")
        );
        assert_eq!(
            line_evidence("schema", "  status VARCHAR(20) NOT NULL,"),
            Some("state")
        );
        assert_eq!(line_evidence("schema", "  name TEXT,"), None);
        assert_eq!(
            line_evidence("schema", "  overdraft_limit NUMERIC,"),
            Some("schema")
        );
        assert_eq!(
            line_evidence("schema", "ORDER BY id OFFSET $2 LIMIT $3;"),
            None
        );
        assert_eq!(line_evidence("schema", "SELECT id FROM t LIMIT 1;"), None);
        assert_eq!(line_evidence("schema", "  statuses_seen int,"), None);
        assert_eq!(
            line_evidence("docs", "A transfer is never applied twice."),
            Some("docs")
        );
        assert_eq!(line_evidence("docs", "Install the package."), None);
        assert_eq!(line_evidence("api", "        '409':"), Some("api"));
        assert_eq!(
            line_evidence("tests", "assert.Equal(t, 409, resp.Code) // duplicate"),
            Some("tests")
        );
        assert_eq!(
            line_evidence("tests", "assert.Equal(t, 200, resp.Code)"),
            None
        );
    }

    #[test]
    fn a_scan_reads_a_tree_and_skips_the_rest() {
        let d = std::env::temp_dir().join(format!("hydo-rules-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (p, t) in [
            (
                "db/migrations/1.sql",
                "CREATE TABLE t (\n  id INT,\n  qty INT CHECK (qty >= 0),\n  state TEXT\n);\n",
            ),
            ("README.md", "Orders are processed exactly once.\nHello.\n"),
            ("node_modules/x/README.md", "never seen\n"),
            ("src/main.go", "// never read: not a source of promises\n"),
        ] {
            let f = d.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, t).unwrap();
        }
        let (read, ev) = scan(&d, 40);
        assert_eq!(read, 2);
        let got: Vec<(&str, &str, usize)> = ev
            .iter()
            .map(|e| (e.kind, e.file.as_str(), e.line))
            .collect();
        assert_eq!(
            got,
            [
                ("docs", "README.md", 1),
                ("schema", "db/migrations/1.sql", 3),
                ("state", "db/migrations/1.sql", 4)
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
