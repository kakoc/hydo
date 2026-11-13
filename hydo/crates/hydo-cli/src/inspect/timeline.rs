//! `hydo inspect events <run dir>` (docs/design/inspect.md "The timeline"): a run's events with
//! the stop and guest time of each, read from the run's own files only (no VM): the control
//! transcripts and the merged consoles as `hydo diff` reads them (M165: commands, the host
//! controller's notes and reports, the guest's console lines, ends), plus the agent's own events
//! from the engine's consoles (`one.agent.log`, and a failed boot's `one.log`): which process
//! exited with what status or signal, which readiness check ended how, which command ended, and
//! the kernel's lines. An agent event gets the first recorded stop by which the host had seen it
//! (the transcript's `seen` count).

use super::drive::{Conv, Recorded};
use super::{Args, Problem};
use crate::diff;
use hydo_agent_proto::frame::{deframe_line, Deframed};
use hydo_control::Record;
use hydo_results::{Timeline, TimelineConversation, TimelineEvent};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// A command the host typed, from a transcript's send record.
#[derive(Clone, Debug, Default)]
pub struct Sent {
    pub op: String,
    pub h: Option<String>,
    pub text: String,
    pub json: Value,
}

/// One event of the agent (or a kernel or guest line) from an engine console.
#[derive(Clone, Debug)]
pub struct AgentEv {
    pub conv: Conv,
    pub stop: Option<u64>,
    pub at: Option<u64>,
    /// `exit`, `done`, `probe`, `refused`, `kernel`, `guest`.
    pub kind: &'static str,
    pub json: Value,
    pub text: String,
}

/// The commands of a conversation by id.
pub fn sends(rec: &Recorded, c: Conv) -> BTreeMap<u64, Sent> {
    let mut m = BTreeMap::new();
    let Some(t) = rec.transcript(c) else {
        return m;
    };
    for r in &t.records {
        if let Record::Send { id, bytes, .. } = r {
            let v: Value = serde_json::from_str(bytes.trim_start_matches('\u{18}').trim_end())
                .unwrap_or(Value::Null);
            let op = v
                .get("op")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let h = v.get("h").and_then(Value::as_str).map(str::to_string);
            let text = command_text(&v);
            m.insert(
                *id,
                Sent {
                    op,
                    h,
                    text,
                    json: v,
                },
            );
        }
    }
    m
}

/// A command as a person reads it (`/bin/redis-server /etc/redis.conf`).
pub fn command_text(v: &Value) -> String {
    let argv: Vec<&str> = v
        .get("argv")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut s = match argv.as_slice() {
        [_, "sh", "-c", t] => t.to_string(),
        [_, "timeout", _, _, "sh", "-c", t] => t.to_string(),
        a => a.join(" "),
    };
    for p in [". /etc/hydo/prelude.sh; exec ", ". /etc/hydo/prelude.sh; "] {
        if let Some(x) = s.strip_prefix(p) {
            s = x.to_string();
            break;
        }
    }
    let s = s.trim().to_string();
    if s.is_empty() {
        v.get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default()
    } else {
        s
    }
}

/// The kernel's lines worth a reader's attention (crashes and memory).
pub fn kernel_worth(l: &str) -> bool {
    [
        "Out of memory",
        "oom-kill",
        "invoked oom-killer",
        "Killed process",
        "segfault",
        "general protection",
        "Kernel panic",
        "BUG:",
        "Oops",
        "blocked for more than",
        "hung_task",
        "page allocation failure",
    ]
    .iter()
    .any(|p| l.contains(p))
}

fn is_kernel(l: &str) -> bool {
    l.starts_with('[') && l.find(']').is_some_and(|i| i < 16)
}

/// The engine console of a conversation, as the run directory keeps it.
fn console_of(rec: &Recorded, c: Conv) -> Option<String> {
    let f = match c {
        // a boot's console is kept only when the boot ended the run (no run conversation); an
        // exploration's workers boot apart from its runs
        Conv::Boot if rec.transcript(Conv::Run).is_none() && rec.files.tag.is_none() => {
            rec.files.console(Conv::Run)
        }
        Conv::Boot => return None,
        c => rec.files.agent_log(c),
    };
    std::fs::read_to_string(rec.dir.join(f)).ok()
}

/// The agent's events a replay of the run saw (`hydo run` writes them when the run's consoles
/// have none: on hvx the agent's lines go over the host-call port, not the console). One JSON
/// object per line: `conversation`, `stop`, `at`, `line` (the agent's line without its number).
pub const AGENT_EVENTS: &str = "agent-events.jsonl";

/// One replayed agent event as a timeline event.
pub fn replayed(rec: &Recorded, c: Conv, stop: u64, at: u64, v: Value) -> Option<AgentEv> {
    let cmds = sends(rec, c);
    let (kind, text) = describe(&v, &cmds)?;
    Some(AgentEv {
        conv: c,
        stop: Some(stop),
        at: Some(at),
        kind,
        json: v,
        text,
    })
}

/// The lines of [`AGENT_EVENTS`] for the replayed events of a run.
pub fn agent_events_file(events: &[(Conv, hydo_control::StopInfo, Value)]) -> String {
    let mut s = String::new();
    for (c, st, v) in events {
        s.push_str(
            &serde_json::to_string(
                &json!({"conversation": c.name(), "stop": st.n, "at": st.at, "line": v}),
            )
            .unwrap_or_default(),
        );
        s.push('\n');
    }
    s
}

/// The agent's events (and kernel and guest lines) of every conversation's engine console.
pub fn agent_events(rec: &Recorded) -> Vec<AgentEv> {
    let mut out = Vec::new();
    let mut framed = Vec::new();
    for c in Conv::ALL {
        let Some(text) = console_of(rec, c) else {
            continue;
        };
        if text.contains("HYDO-AG ") {
            framed.push(c);
        }
        let stops: Vec<(u64, u64, u64)> = rec
            .transcript(c)
            .map(|t| {
                t.records
                    .iter()
                    .filter_map(|r| match r {
                        Record::Stop { n, at, seen, .. } => Some((*n, *at, *seen)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let stop_of = |seq: u64| stops.iter().find(|(_, _, seen)| *seen > seq).copied();
        let cmds = sends(rec, c);
        let mut last: Option<(u64, u64, u64)> = stops.first().copied();
        for line in text.lines() {
            match deframe_line(line.as_bytes()) {
                Deframed::Agent(j) => {
                    let Ok(v) = serde_json::from_str::<Value>(&j) else {
                        continue;
                    };
                    let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
                    let st = stop_of(seq);
                    if st.is_some() {
                        last = st;
                    }
                    let (kind, text) = match describe(&v, &cmds) {
                        Some(x) => x,
                        None => continue,
                    };
                    out.push(AgentEv {
                        conv: c,
                        stop: st.map(|s| s.0),
                        at: st.map(|s| s.1),
                        kind,
                        json: v,
                        text,
                    });
                }
                Deframed::Corrupt { .. } => {}
                Deframed::Other(l) => {
                    let l = l.trim_end().to_string();
                    if l.is_empty() {
                        continue;
                    }
                    let kernel = is_kernel(&l);
                    // the run's guest lines are in the merged console already
                    if !kernel && c != Conv::Boot {
                        continue;
                    }
                    out.push(AgentEv {
                        conv: c,
                        stop: last.map(|s| s.0),
                        at: last.map(|s| s.1),
                        kind: if kernel { "kernel" } else { "guest" },
                        json: Value::Null,
                        text: l,
                    });
                }
            }
        }
    }
    // conversations whose console has no agent lines: what a replay saw, when it was kept
    let kept = rec.files.agent_events().map(|f| rec.dir.join(f));
    if let Some(Ok(t)) = kept.map(std::fs::read_to_string) {
        for l in t.lines() {
            let Ok(v) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            let Some(c) = v["conversation"].as_str().and_then(Conv::parse) else {
                continue;
            };
            if framed.contains(&c) {
                continue;
            }
            let (n, at) = (
                v["stop"].as_u64().unwrap_or(0),
                v["at"].as_u64().unwrap_or(0),
            );
            out.extend(replayed(rec, c, n, at, v["line"].clone()));
        }
    }
    out
}

pub fn signal_name(n: u64) -> &'static str {
    match n {
        1 => "HUP",
        2 => "INT",
        3 => "QUIT",
        4 => "ILL",
        6 => "ABRT",
        7 => "BUS",
        8 => "FPE",
        9 => "KILL",
        11 => "SEGV",
        13 => "PIPE",
        14 => "ALRM",
        15 => "TERM",
        _ => "?",
    }
}

/// An agent line as an event: exits, ends of commands and probes, refusals.
fn describe(v: &Value, cmds: &BTreeMap<u64, Sent>) -> Option<(&'static str, String)> {
    let u = |k: &str| v.get(k).and_then(Value::as_u64);
    let how = || {
        let st = u("status").unwrap_or(0);
        match u("sig") {
            Some(s) => format!("killed by signal {s} ({}), status {st}", signal_name(s)),
            None if v.get("timed_out").and_then(Value::as_bool) == Some(true) => {
                format!("timed out, status {st}")
            }
            None => format!("status {st}"),
        }
    };
    match v.get("ev").and_then(Value::as_str) {
        Some("exit") => {
            let h = v.get("h").and_then(Value::as_str).unwrap_or("?");
            Some(("exit", format!("{h} exited: {}", how())))
        }
        Some("done") => {
            let id = u("id").unwrap_or(0);
            let c = cmds.get(&id).map(|s| s.text.as_str()).unwrap_or("?");
            let out = v.get("out").and_then(Value::as_str).unwrap_or("");
            let out = if out.trim().is_empty() {
                String::new()
            } else {
                format!("; output: {}", diff_cut(out.trim(), 160))
            };
            Some((
                "done",
                format!("command ended ({}): {}{out}", how(), diff_cut(c, 120)),
            ))
        }
        Some("probe") => {
            let id = u("id").unwrap_or(0);
            let s = cmds.get(&id);
            let who = s
                .and_then(|s| s.json.get("alive").and_then(Value::as_str))
                .map(|a| format!("readiness check of {a}"))
                .unwrap_or_else(|| "check".into());
            let why = v.get("why").and_then(Value::as_str).unwrap_or("?");
            let tries = u("tries").unwrap_or(0);
            let last = u("status")
                .map(|s| format!(", last status {s}"))
                .unwrap_or_default();
            Some((
                "probe",
                format!(
                    "{who} ended: {why} after {tries} tries{last} ({})",
                    s.map(|s| diff_cut(&s.text, 100)).unwrap_or_default()
                ),
            ))
        }
        Some(_) => None,
        None if v.get("ok").and_then(Value::as_bool) == Some(false) => {
            let re = u("re");
            let c = re
                .and_then(|r| cmds.get(&r))
                .map(|s| format!("{} {}", s.op, diff_cut(&s.text, 80)))
                .unwrap_or_default();
            Some((
                "refused",
                format!(
                    "the agent refused {c}: {} {}",
                    v.get("err").and_then(Value::as_str).unwrap_or(""),
                    v.get("msg").and_then(Value::as_str).unwrap_or("")
                ),
            ))
        }
        None => None,
    }
}

pub fn diff_cut(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        return s;
    }
    let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// The kind of a `hydo diff` event.
pub fn kind_of(e: &diff::Ev) -> &'static str {
    match e.level {
        "control" if e.key.starts_with("send") => "command",
        "control" if e.key.starts_with("snap") => "snapshot",
        "control" => "engine",
        "outcome" if e.key.starts_with("end") => "end",
        "outcome" => "report",
        "verdict" => "verdict",
        "assertion" | "assertion-decl" => "assertion",
        "guest" => "guest",
        "history" | "fake-world" | "error-log" | "composer" => "report",
        _ => {
            let t = e.text.strip_prefix("hydo-ob: ").unwrap_or("");
            let first = t.split_whitespace().next().unwrap_or("");
            if first.len() >= 3 && first.chars().all(|c| c.is_ascii_uppercase()) {
                "fault"
            } else if e.text.starts_with("hydo-ob:") {
                "note"
            } else {
                "report"
            }
        }
    }
}

pub fn service_of(text: &str, json: &Value) -> Option<String> {
    if let Some(h) = json.get("h").and_then(Value::as_str) {
        return Some(h.trim_start_matches("setup:").to_string());
    }
    if let Some(a) = json.get("alive").and_then(Value::as_str) {
        return Some(a.to_string());
    }
    let t = text.strip_prefix("hydo-ob: ")?;
    let mut w = t.split_whitespace();
    let first = w.next()?;
    if first.chars().all(|c| c.is_ascii_uppercase()) {
        // `KILLED redis ...`, not `POWER CUT ...` or `NET FAULT ...`
        return w
            .next()
            .filter(|s| !s.chars().all(|c| c.is_ascii_uppercase()))
            .map(str::to_string);
    }
    None
}

/// Every event of a run, in order.
pub fn events(rec: &Recorded) -> Vec<TimelineEvent> {
    let mut keyed: Vec<((usize, u64, u8, usize), TimelineEvent)> = Vec::new();
    let merged = |c: Conv| -> String {
        if c == Conv::Boot {
            return String::new();
        }
        std::fs::read_to_string(rec.dir.join(rec.files.console(c))).unwrap_or_default()
    };
    for (ci, c) in Conv::ALL.iter().enumerate() {
        let Some(t) = rec.transcript(*c) else {
            continue;
        };
        let console = merged(*c);
        let evs = diff::conversation_events(
            match c {
                Conv::Boot => "boot",
                Conv::Run => "run",
                Conv::Recovery => "recovery",
            },
            &t.to_jsonl(),
            &console,
        );
        for (i, e) in evs.into_iter().enumerate() {
            let kind = kind_of(&e);
            let details = e
                .json
                .clone()
                .filter(|_| kind == "command" || kind == "end");
            let service = service_of(&e.text, e.json.as_ref().unwrap_or(&Value::Null));
            keyed.push((
                (ci, e.stop.unwrap_or(0), 2, i),
                TimelineEvent {
                    point: e.stop.map(|s| format!("{}:{s}", c.name())),
                    conversation: c.name().into(),
                    stop: e.stop,
                    at: e.at,
                    kind: kind.into(),
                    text: e.text,
                    service,
                    details,
                },
            ));
        }
    }
    for (i, a) in agent_events(rec).into_iter().enumerate() {
        let ci = Conv::ALL.iter().position(|c| *c == a.conv).unwrap_or(0);
        let service = service_of("", &a.json);
        keyed.push((
            (ci, a.stop.unwrap_or(0), 1, i),
            TimelineEvent {
                point: a.stop.map(|s| format!("{}:{s}", a.conv.name())),
                conversation: a.conv.name().into(),
                stop: a.stop,
                at: a.at,
                kind: a.kind.into(),
                text: a.text,
                service,
                details: (!a.json.is_null()).then_some(a.json),
            },
        ));
    }
    // the forced preemption points (HYDO_PREEMPT_RECORD, M208), each before the stop that
    // follows it; `at` is the point's own exit count
    let ci = Conv::ALL.iter().position(|c| *c == Conv::Run).unwrap_or(0);
    for (i, p) in crate::preempt::read_file(&rec.dir.join(rec.files.preempt()))
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let stop = crate::preempt::stop_for(rec.transcript(Conv::Run), crate::preempt::exit(&p))
            .map(|(n, _)| n);
        keyed.push((
            (ci, stop.unwrap_or(0), 0, i),
            TimelineEvent {
                point: stop.map(|s| format!("{}:{s}", Conv::Run.name())),
                conversation: Conv::Run.name().into(),
                stop,
                at: Some(crate::preempt::exit(&p)),
                kind: "preempt".into(),
                text: crate::preempt::text(&p),
                service: None,
                details: Some(p),
            },
        ));
    }
    keyed.sort_by_key(|(k, _)| *k);
    let mut out: Vec<TimelineEvent> = keyed.into_iter().map(|(_, e)| e).collect();
    out.push(TimelineEvent {
        point: None,
        conversation: "result".into(),
        stop: None,
        at: None,
        kind: "verdict".into(),
        text: format!("{}: {}", rec.doc.verdict.kind, rec.doc.verdict.text),
        service: None,
        details: None,
    });
    out
}

/// The conversations of a run with their stops, how each ended and whether stops there can be
/// kept as snapshots.
pub fn conversations(rec: &Recorded) -> Vec<TimelineConversation> {
    rec.transcripts
        .iter()
        .filter_map(|(c, t)| {
            let (first, last) = rec.span(*c)?;
            let end = t.ending().map(|e| match e {
                hydo_control::Ending::Verdict(v) => format!("verdict: {v}"),
                hydo_control::Ending::AtSnapshot(s) => format!("at snapshot {s}"),
                hydo_control::Ending::PowerCut { .. } => "power cut".into(),
            });
            let snapshots = match c {
                Conv::Boot => "every stop".to_string(),
                Conv::Recovery => "none (live sessions only)".into(),
                Conv::Run => {
                    let probe = super::drive::Target {
                        conv: Conv::Run,
                        stop: last,
                    };
                    match rec.snapshot_ok(probe) {
                        Ok(()) => "every stop".into(),
                        Err(why) => {
                            let off = t.records.iter().find_map(|r| match r {
                                Record::Engine { n, .. } => Some(*n),
                                _ => None,
                            });
                            match off {
                                Some(n) if !why.contains("timing") => {
                                    format!("stops {first}..{n} (after {n}: live only)")
                                }
                                _ => format!("none: {why}"),
                            }
                        }
                    }
                }
            };
            Some(TimelineConversation {
                conversation: c.name().into(),
                first_stop: first,
                last_stop: last,
                end,
                snapshots: Some(snapshots),
            })
        })
        .collect()
}

fn order(conv: &str) -> usize {
    match conv {
        "boot" => 0,
        "run" => 1,
        "recovery" => 2,
        _ => 3,
    }
}

fn parse_point(p: &str) -> Result<(usize, u64), Problem> {
    let (c, n) = p.split_once(':').ok_or_else(|| {
        Problem::new(
            "usage",
            format!("{p:?}: a point is <conversation>:<stop> (run:1203)"),
            vec![],
        )
    })?;
    Ok((
        order(c),
        n.parse()
            .map_err(|_| Problem::new("usage", format!("{p:?}: the stop is a number"), vec![]))?,
    ))
}

/// `hydo inspect events <run dir> [filters]`.
pub fn cmd(rest: &[String]) -> Result<Value, Problem> {
    let mut a = Args::new(rest);
    let kinds: Vec<String> = a
        .val("--kind")?
        .map(|k| k.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_default();
    let grep = a.val("--grep")?.map(|g| g.to_lowercase());
    let conv = a.val("--conversation")?;
    let from = a.val("--from")?.map(|p| parse_point(&p)).transpose()?;
    let to = a.val("--to")?.map(|p| parse_point(&p)).transpose()?;
    let limit = a.num("--limit")?.unwrap_or(200);
    let offset = a.num("--offset")?.unwrap_or(0);
    let dir = a.one("a run directory")?;
    a.done()?;
    let rec = Recorded::load(Path::new(&dir))?;
    let all = events(&rec);
    let matching: Vec<TimelineEvent> = all
        .into_iter()
        .filter(|e| kinds.is_empty() || kinds.contains(&e.kind))
        .filter(|e| conv.as_ref().is_none_or(|c| *c == e.conversation))
        .filter(|e| {
            grep.as_ref().is_none_or(|g| {
                e.text.to_lowercase().contains(g)
                    || e.service
                        .as_ref()
                        .is_some_and(|s| s.to_lowercase().contains(g))
            })
        })
        .filter(|e| {
            let k = (order(&e.conversation), e.stop.unwrap_or(u64::MAX));
            from.is_none_or(|f| k >= f) && to.is_none_or(|t| k <= t)
        })
        .collect();
    let total = matching.len() as u64;
    let shown: Vec<TimelineEvent> = matching
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    let truncated = offset + (shown.len() as u64) < total;
    let next = truncated.then(|| {
        let mut w: Vec<String> = vec!["hydo inspect events".into(), dir.clone()];
        for (i, r) in rest.iter().enumerate() {
            if r == &dir || r == "--offset" || (i > 0 && rest[i - 1] == "--offset") {
                continue;
            }
            w.push(super::work::quote(r));
        }
        w.push(format!("--offset {}", offset + shown.len() as u64));
        w.join(" ")
    });
    let doc = Timeline {
        run_dir: rec.dir.display().to_string(),
        verdict: rec.doc.verdict.clone(),
        conversations: conversations(&rec),
        total,
        offset,
        events: shown,
        truncated,
        next,
        preempt: crate::preempt::summary(&rec.dir),
        ..Timeline::default()
    };
    Ok(serde_json::to_value(&doc).unwrap_or(json!(null)))
}
