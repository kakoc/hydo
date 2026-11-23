//! Why a run failed (docs/design/inspect.md "Part A: the diagnosis").
//!
//! Two layers. [`host`] reads only what the run left (the transcripts, the engine's consoles with
//! the agent's own events, the result document), so it costs nothing and runs for every failed
//! `hydo run`. [`evidence`] replays the run to the stop where the verdict was given (live, on a
//! fresh engine, with the run's directory only read) and reads in the guest what the host never
//! sees: each service's log, the readiness check's own output, the kernel's messages, the
//! processes and the memory. Neither touches the recorded run: the fingerprints and transcripts
//! were written before, and the replay is a separate engine run.

use super::drive::{Conv, Recorded, Taker, Target};
use super::timeline::{agent_events, kernel_worth, sends, signal_name, AgentEv};
use super::work::{FileReq, Work};
use super::{env, netns_of, run_point, Args, Problem};
use crate::ctx::Ctx;
use hydo_control::{Ending, Out, Plan, Seen, StopInfo};
use hydo_results::{Diagnosis, DiagnosisDoc, Evidence, ProbeEnd, ProcessEnd, RunPoint};
use serde_json::Value;
use std::path::Path;

/// The service a verdict names: `ERROR service start (S)`, `ERROR S restart failed`,
/// `ERROR restart after the faults (S)`, and the violations of a service that did not start
/// again: `VIOLATION service cannot recover (committed data unrecoverable) (S)` (the recovery
/// boot after a power cut) and `VIOLATION service cannot recover after the faults (S)`, and a
/// service that stopped on its own: `VIOLATION service stopped in a run without faults (S)`, and a
/// service that did not start on its new version (`U=`, Milestone 238): `VIOLATION
/// [upgrade.started] S did not start on version V before step N (it ran W)`.
pub fn verdict_service(v: &str) -> Option<String> {
    let v = v.trim_start_matches("HYDO-ORACLE: ");
    if v.starts_with("VIOLATION service cannot recover")
        || v.starts_with("VIOLATION service stopped in a run without faults")
    {
        let r = v.strip_suffix(')')?;
        let (_, s) = r.rsplit_once(" (")?;
        return (!s.is_empty() && !s.contains(' ')).then(|| s.to_string());
    }
    if let Some(r) = v.strip_prefix("ERROR service start (") {
        return r.split(')').next().map(str::to_string);
    }
    // Milestone 238: `VIOLATION [upgrade.started] S did not start on version V before step N`
    if let Some(r) = v.strip_prefix("VIOLATION [upgrade.started] ") {
        return r
            .split_once(" did not start on version ")
            .map(|(s, _)| s.to_string());
    }
    if let Some(r) = v.strip_prefix("ERROR restart after the faults (") {
        return r.split(')').next().map(str::to_string);
    }
    if let Some(r) = v.strip_prefix("ERROR ") {
        if let Some(s) = r.strip_suffix(" restart failed") {
            return Some(s.to_string());
        }
    }
    None
}

fn ram_mib(rec: &Recorded) -> u64 {
    rec.doc
        .engine
        .as_ref()
        .and_then(|e| e.ram_mib)
        .unwrap_or(1024)
}

fn point(rec: &Recorded, a: &AgentEv) -> Option<RunPoint> {
    a.stop.map(|s| {
        run_point(
            rec,
            Target {
                conv: a.conv,
                stop: s,
            },
        )
    })
}

/// Process ends of the agent events, with whether hydo sent the signal itself.
fn process_ends(rec: &Recorded, evs: &[AgentEv]) -> Vec<ProcessEnd> {
    let mut out = Vec::new();
    for a in evs.iter().filter(|a| a.kind == "exit" || a.kind == "done") {
        let u = |k: &str| a.json.get(k).and_then(Value::as_u64);
        let status = u("status").unwrap_or(0);
        if status == 0 {
            continue;
        }
        let cmds = sends_seen(rec, a.conv);
        let id = u("id").unwrap_or(0);
        let sent = cmds.get(&id);
        let handle = a
            .json
            .get("h")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("command {id}"));
        let sig = u("sig");
        // did the host send that signal to this handle (a kill, the end of a check)?
        let by_hydo = sig.map(|s| {
            let name = signal_name(s);
            cmds.values().any(|c| {
                c.op == "signal"
                    && c.h.as_deref() == a.json.get("h").and_then(Value::as_str)
                    && c.json.get("sig").and_then(Value::as_str) == Some(name)
            }) || (a.kind == "done"
                && a.json.get("timed_out").and_then(Value::as_bool) == Some(true))
        });
        out.push(ProcessEnd {
            handle,
            command: sent.map(|s| s.text.clone()).filter(|t| !t.is_empty()),
            status,
            signal: sig,
            signal_name: sig.map(|s| signal_name(s).to_string()),
            timed_out: a.json.get("timed_out").and_then(Value::as_bool),
            signal_by_hydo: by_hydo,
            output: a
                .json
                .get("out")
                .and_then(Value::as_str)
                .filter(|o| !o.trim().is_empty())
                .map(str::to_string),
            at: point(rec, a),
        });
    }
    out
}

fn likely_of(p: &ProcessEnd, ram: u64) -> Option<String> {
    match (p.signal, p.status) {
        (Some(9), _) if p.signal_by_hydo != Some(true) => Some(format!(
            "{} was killed by SIGKILL, which hydo did not send: in a guest with {ram} MiB of RAM \
             that is almost always the kernel's out-of-memory killer (raise guest.ram_mib, or \
             look at `dmesg` and the processes' memory: hydo inspect diagnose --evidence)",
            p.handle
        )),
        (Some(11), _) => Some(format!("{} crashed (SIGSEGV, a segmentation fault)", p.handle)),
        (Some(6), _) => Some(format!("{} aborted itself (SIGABRT: an assertion or a fatal error of its own)", p.handle)),
        (Some(7), _) => Some(format!("{} died of SIGBUS (often a file it had mapped got shorter, or a full tmpfs)", p.handle)),
        (None, 127) => Some(format!(
            "{} exited with status 127: a program it runs was not found (not in the image, or not on PATH)",
            p.handle
        )),
        (None, 126) => Some(format!(
            "{} exited with status 126: a program it runs is not executable",
            p.handle
        )),
        _ => None,
    }
}

/// The diagnosis from the run's own files (no VM). None for a passing run.
pub fn host(rec: &Recorded) -> Option<Diagnosis> {
    host_from(rec, agent_events(rec))
}

/// [`host`] from the given agent events.
fn host_from(rec: &Recorded, evs: Vec<AgentEv>) -> Option<Diagnosis> {
    let v = rec.doc.verdict.text.clone();
    let kind = rec.doc.verdict.kind.as_str();
    if kind == "ok" {
        return None;
    }
    let end = rec.end();
    let mut d = Diagnosis {
        verdict_at: end.map(|t| run_point(rec, t)),
        evidence_status: "not-collected".into(),
        ..Diagnosis::default()
    };
    let ram = ram_mib(rec);
    let exits = process_ends(rec, &evs);
    d.kernel = evs
        .iter()
        .filter(|a| a.kind == "kernel" && kernel_worth(&a.text))
        .map(|a| a.text.clone())
        .collect();
    let svc = verdict_service(&v);
    d.service = svc.clone();
    let conv = end.map(|t| t.conv).unwrap_or(Conv::Run);
    let core = v.trim_start_matches("HYDO-ORACLE: ").to_string();
    if let Some(s) = &svc {
        // the service's last start: its setup, its process, its readiness check
        let cmds = sends(rec, conv);
        let last_spawn = cmds
            .iter()
            .rev()
            .find(|(_, c)| c.op == "spawn" && c.h.as_deref() == Some(s))
            .map(|(id, _)| *id);
        let setup_fail = exits
            .iter()
            .rev()
            .find(|p| p.handle == format!("setup:{s}"))
            .cloned();
        let svc_exit = exits
            .iter()
            .rev()
            .find(|p| {
                p.handle == *s
                    && p.at.as_ref().map(|x| x.conversation.as_str()) == Some(conv.name())
            })
            .cloned();
        let probe = evs.iter().rev().find(|a| {
            a.kind == "probe"
                && a.conv == conv
                && cmds
                    .get(&a.json.get("id").and_then(Value::as_u64).unwrap_or(0))
                    .and_then(|c| c.json.get("alive").and_then(Value::as_str))
                    == Some(s.as_str())
        });
        let probe_end = probe.map(|a| {
            let id = a.json.get("id").and_then(Value::as_u64).unwrap_or(0);
            let c = cmds.get(&id);
            ProbeEnd {
                service: s.clone(),
                command: c.map(|c| c.text.clone()).unwrap_or_default(),
                why: a
                    .json
                    .get("why")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .into(),
                tries: a.json.get("tries").and_then(Value::as_u64).unwrap_or(0),
                last_status: a.json.get("status").and_then(Value::as_u64),
                deadline_s: c
                    .and_then(|c| c.json.get("deadline_ms").and_then(Value::as_u64))
                    .map(|ms| ms / 1000),
            }
        });
        let _ = last_spawn;
        let what = if conv == Conv::Recovery {
            "while it started in the recovery boot after the power cut"
        } else if core.contains("restart")
            || core.contains("after the faults")
            || core.contains("without faults")
        {
            "while it was restarted"
        } else {
            "while it started"
        };
        if let Some(p) = setup_fail {
            d.failure = "service-setup".into();
            d.summary = format!(
                "the setup command of {s} failed {what} ({})",
                exit_words(&p)
            );
            d.process = Some(p);
        } else if let Some(p) = svc_exit {
            d.failure = "service-exited".into();
            d.summary = format!(
                "{s} exited {what}, before its readiness check passed ({})",
                exit_words(&p)
            );
            d.process = Some(p);
            d.probe = probe_end;
        } else if let Some(pe) = probe_end {
            if pe.why == "deadline" {
                d.failure = "service-not-ready".into();
                d.summary = format!(
                    "{s} kept running but its readiness check `{}` did not pass in {} s ({} tries{})",
                    super::timeline::diff_cut(&pe.command, 80),
                    pe.deadline_s.unwrap_or(0),
                    pe.tries,
                    pe.last_status
                        .map(|x| format!(", last status {x}"))
                        .unwrap_or_default()
                );
                d.likely.push(format!(
                    "either {s} is not answering where the check asks (another port or address, still \
                     loading), or the check itself fails: the evidence runs the check once and shows \
                     its output"
                ));
            } else {
                d.failure = "service-exited".into();
                d.summary = format!("{s}'s process ended {what} (its readiness check saw it die)");
            }
            d.probe = Some(pe);
        } else {
            d.failure = if core.contains("restart") {
                "restart"
            } else {
                "other"
            }
            .into();
            d.summary = format!("{core}: no exit or readiness result of {s} was recorded");
            if !evs
                .iter()
                .any(|a| matches!(a.kind, "exit" | "done" | "probe"))
            {
                d.summary.push_str(&format!(
                    " (the run's consoles carry no agent lines: on hvx they go over the host-call \
                     port; `hydo inspect diagnose {} --evidence` replays the run and reads them)",
                    rec.dir.display()
                ));
            }
        }
    } else if core.starts_with("ERROR provision failed") {
        d.failure = "provision".into();
        match exits
            .iter()
            .rev()
            .find(|p| p.at.as_ref().map(|x| x.conversation.as_str()) == Some("boot"))
        {
            Some(p) => {
                d.summary = format!("the provisioning command failed ({})", exit_words(p));
                d.process = Some(p.clone());
            }
            None => d.summary = "the provisioning command failed".into(),
        }
    } else if core.starts_with("ERROR mount data") {
        d.failure = "mount".into();
        d.summary = format!("the data disk could not be mounted: {core}");
    } else if core.starts_with("TIMEOUT") {
        d.failure = "timeout".into();
        d.summary = format!(
            "{core}: the phase ran out of its time limit (a command that never ends, a stuck \
             service, or a workload slower than workload.timeout)"
        );
    } else if core.starts_with("ERROR agent lines full") {
        // Milestone 348: the hvx engine's verdict names the cause and what to change
        d.failure = "vm".into();
        d.summary = core.clone();
    } else if core.starts_with("ERROR vm") || core.starts_with("ERROR harness") {
        d.failure = "vm".into();
        d.summary = format!("{core}: the guest or the engine stopped (see engine.log)");
    } else if core.contains("ERROR control") || core.contains("diverged") {
        d.failure = "control".into();
        d.summary = core.clone();
    } else if kind == "violation" {
        d.failure = "violation".into();
        let has_agent = evs
            .iter()
            .any(|a| matches!(a.kind, "exit" | "done" | "probe"));
        if let Some(down) = service_down(rec, &evs, &exits, conv) {
            // a service that was down when the check ran (Milestone 192): a check that reads
            // it cannot tell its data from its absence, so "lost" may mean "not answering"
            d.service = Some(down.service.clone());
            let wh = conv_words(down.conv);
            match (down.process, down.probe) {
                (Some(p), probe) => {
                    d.failure = "service-exited".into();
                    d.summary = format!(
                        "a check failed: {core}. But {} exited {wh} before the verdict ({}), so \
                         the check may have read a service that was down rather than lost or \
                         wrong data; look at its log",
                        down.service,
                        exit_words(&p)
                    );
                    d.process = Some(p);
                    d.probe = probe;
                }
                (None, Some(pe)) => {
                    d.failure = "service-not-ready".into();
                    d.summary = format!(
                        "a check failed: {core}. But the readiness check of {} ended `{}` {wh} \
                         ({} tries{}), so the check may have read a service that was not \
                         answering rather than lost or wrong data; look at its log",
                        down.service,
                        pe.why,
                        pe.tries,
                        pe.last_status
                            .map(|x| format!(", last status {x}"))
                            .unwrap_or_default()
                    );
                    d.probe = Some(pe);
                }
                (None, None) => {}
            }
        } else if has_agent {
            d.summary = format!(
                "a check failed: {core}. No service exited and every readiness check passed; \
                 look at the state behind it with hydo inspect"
            );
        } else {
            d.summary = format!(
                "a check failed: {core}. The run's consoles carry no process ends (on hvx the \
                 agent's lines go over the host-call port), so whether a service was down is not \
                 known here: `hydo inspect diagnose {} --evidence` replays the run and reads \
                 them",
                rec.dir.display()
            );
        }
    } else {
        d.failure = "other".into();
        d.summary = core.clone();
    }
    // conclusions from every failed process (the main one first)
    if let Some(p) = &d.process {
        d.likely.extend(likely_of(p, ram));
    }
    for p in &exits {
        if Some(p) != d.process.as_ref() {
            if let Some(l) = likely_of(p, ram) {
                if !d.likely.contains(&l) {
                    d.likely.push(l);
                }
            }
        }
    }
    d.exits = exits.into_iter().rev().take(10).collect();
    d.exits.reverse();
    let dir = rec.dir.display();
    d.next = vec![
        format!("hydo inspect diagnose {dir} --evidence"),
        format!("hydo inspect start {dir} --at end"),
        format!("hydo inspect events {dir} --kind exit,probe,fault,verdict"),
    ];
    if let Some(s) = &d.service {
        d.next
            .insert(1, format!("hydo inspect logs {dir} {s} --lines 80"));
    }
    Some(d)
}

/// A service that was down in the verdict's conversation: its process ended (by itself, not by
/// a signal hydo sent) and was not started again, or its last readiness check did not pass.
struct Down {
    service: String,
    conv: Conv,
    process: Option<ProcessEnd>,
    probe: Option<ProbeEnd>,
}

/// The commands a conversation's agent events can name: its own, and for the workload the boot's
/// too (the services the boot started run on from snapshot S1 with the boot's command numbers).
fn sends_seen(
    rec: &Recorded,
    conv: Conv,
) -> std::collections::BTreeMap<u64, super::timeline::Sent> {
    let mut m = sends(rec, conv);
    if conv == Conv::Run {
        for (id, c) in sends(rec, Conv::Boot) {
            m.entry(id).or_insert(c);
        }
    }
    m
}

/// The services of the run: the handles of commands started with `HYDO_SVC=<handle>` and the
/// processes readiness checks watch.
fn service_names(rec: &Recorded) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for c in Conv::ALL {
        for s in sends(rec, c).values() {
            let name = if s.op == "spawn" {
                s.h.clone().filter(|h| {
                    s.json
                        .get("env")
                        .and_then(Value::as_array)
                        .is_some_and(|e| {
                            e.iter()
                                .any(|x| x.as_str() == Some(&format!("HYDO_SVC={h}")))
                        })
                })
            } else if s.op == "probe" {
                s.json
                    .get("alive")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            };
            if let Some(n) = name {
                if !v.contains(&n) {
                    v.push(n);
                }
            }
        }
    }
    v
}

/// The first service that was down in conversation `conv` when the verdict was given.
fn service_down(rec: &Recorded, evs: &[AgentEv], exits: &[ProcessEnd], conv: Conv) -> Option<Down> {
    let services = service_names(rec);
    let cmds = sends_seen(rec, conv);
    let probe_of = |a: &AgentEv| -> Option<ProbeEnd> {
        let id = a.json.get("id").and_then(Value::as_u64)?;
        let c = cmds.get(&id)?;
        let s = c.json.get("alive").and_then(Value::as_str)?;
        Some(ProbeEnd {
            service: s.to_string(),
            command: c.text.clone(),
            why: a
                .json
                .get("why")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .into(),
            tries: a.json.get("tries").and_then(Value::as_u64).unwrap_or(0),
            last_status: a.json.get("status").and_then(Value::as_u64),
            deadline_s: c
                .json
                .get("deadline_ms")
                .and_then(Value::as_u64)
                .map(|ms| ms / 1000),
        })
    };
    // the last readiness result of each service in the conversation
    let last_probe = |s: &str| {
        evs.iter()
            .rev()
            .filter(|a| a.conv == conv && a.kind == "probe")
            .filter_map(probe_of)
            .find(|p| p.service == s)
    };
    for s in &services {
        let last_spawn = cmds
            .iter()
            .rev()
            .find(|(_, c)| c.op == "spawn" && c.h.as_deref() == Some(s))
            .map(|(id, _)| *id);
        let exit = evs.iter().rev().find(|a| {
            a.conv == conv
                && a.kind == "exit"
                && a.json.get("h").and_then(Value::as_str) == Some(s)
                && a.json.get("id").and_then(Value::as_u64) == last_spawn
        });
        if let (Some(a), Some(spawn)) = (exit, last_spawn) {
            let signalled = cmds
                .range(spawn + 1..)
                .any(|(_, c)| c.op == "signal" && c.h.as_deref() == Some(s));
            let p = exits.iter().find(|p| {
                p.handle == *s
                    && p.at.as_ref().map(|x| x.conversation.as_str()) == Some(conv.name())
                    && p.at.as_ref().map(|x| x.stop) == a.stop
            });
            if let (false, Some(p)) = (signalled, p) {
                if p.signal_by_hydo != Some(true) {
                    return Some(Down {
                        service: s.clone(),
                        conv,
                        process: Some(p.clone()),
                        probe: last_probe(s).filter(|pe| pe.why != "ok"),
                    });
                }
            }
        }
        if let Some(pe) = last_probe(s).filter(|pe| pe.why != "ok") {
            return Some(Down {
                service: s.clone(),
                conv,
                process: None,
                probe: Some(pe),
            });
        }
    }
    None
}

fn conv_words(c: Conv) -> &'static str {
    match c {
        Conv::Boot => "while the image booted",
        Conv::Run => "during the workload",
        Conv::Recovery => "in the recovery boot after the power cut",
    }
}

fn exit_words(p: &ProcessEnd) -> String {
    match (&p.signal_name, p.timed_out) {
        (Some(n), _) => format!("killed by SIG{n}, status {}", p.status),
        (None, Some(true)) => format!("timed out, status {}", p.status),
        _ => format!("exit status {}", p.status),
    }
}

// ---------------------------------------------------------------- the evidence

/// Runs a list of works one after another at the stop, then ends the conversation.
struct Batch {
    works: Vec<(String, String, Work)>,
    i: usize,
    stops: u64,
}

impl Taker for Batch {
    fn step(&mut self, _stop: &StopInfo, seen: &[Seen], out: &mut Out) {
        let mut seen = seen;
        while self.i < self.works.len() {
            let w = &mut self.works[self.i].2;
            w.step(seen, out);
            seen = &[];
            if w.stops > 50_000 && !w.finished() {
                w.failed = Some("no answer".into());
            }
            if !w.finished() {
                return;
            }
            self.stops += w.stops;
            self.i += 1;
        }
        out.end(Ending::Verdict("HYDO-ORACLE: OK inspect".into()));
    }
}

const TAIL: u64 = 3000;
/// How many last lines of each file of a service's `logs:` the evidence reads.
const LOG_FILE_LINES: u64 = 30;
/// How much of the end of each service's log the evidence reads and writes into the run
/// directory as `logs/<service>.log` (Milestone 239); `result.json` keeps the last [`TAIL`] bytes.
const LOG_KEEP: u64 = 64 * 1024;
/// The run directory's folder of service logs.
pub const LOGS_DIR: &str = "logs";
/// At most this many last lines of the failing service's log go into the summary, whole.
const QUOTE_LINES: usize = 5;
/// The summary quotes whole lines from the end while they fit into this many characters (at
/// least one line, however long).
const QUOTE_CHARS: usize = 600;

/// The replayed agent events of a run (conversation, stop, the agent's line).
pub type Replayed = Vec<(Conv, StopInfo, Value)>;

/// The evidence, and when the run's consoles carry no agent lines (hvx's host-call port), the
/// diagnosis made again from the agent events the replay saw. The replayed events are returned.
pub fn with_evidence(ctx: &Ctx, rec: &Recorded, d: Diagnosis) -> (Diagnosis, Replayed) {
    let mut d = d;
    let ev = evidence(ctx, rec, &mut d);
    let console = agent_events(rec);
    let has_agent = console
        .iter()
        .any(|a| matches!(a.kind, "exit" | "done" | "probe" | "refused"));
    if has_agent || ev.is_empty() {
        return (d, ev);
    }
    let mut evs = console;
    evs.extend(
        ev.iter()
            .filter_map(|(c, s, v)| super::timeline::replayed(rec, *c, s.n, s.at, v.clone())),
    );
    let Some(mut d2) = host_from(rec, evs) else {
        return (d, ev);
    };
    for k in d.kernel.drain(..) {
        if !d2.kernel.contains(&k) {
            d2.kernel.push(k);
        }
    }
    d2.evidence_status = d.evidence_status;
    d2.evidence_note = d.evidence_note;
    d2.evidence = d.evidence;
    refine(&mut d2);
    (d2, ev)
}

/// Read in the guest at the verdict's stop, by one replay. Fills `d.evidence` and `d.kernel`;
/// returns the agent events the replay saw.
pub fn evidence(ctx: &Ctx, rec: &Recorded, d: &mut Diagnosis) -> Replayed {
    let Some(target) = rec.end() else {
        d.evidence_status = "not-collected".into();
        d.evidence_note = Some("the run has no recorded end".into());
        return Vec::new();
    };
    let tmp = super::session::scratch(ctx, "diagnose");
    let r = (|| -> Result<(Batch, Replayed), Problem> {
        let m = super::session::machine(ctx, rec, &tmp)?;
        let mut works = plan_works(&m.plan, d, &failed_commands(rec, target));
        let mut b = Batch {
            works: std::mem::take(&mut works),
            i: 0,
            stops: 0,
        };
        let (_, reach) = m.reach(rec, target, &mut b, false, &tmp.join("engine.err"))?;
        Ok((b, reach.events))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    match r {
        Err(p) => {
            d.evidence_status = "failed".into();
            d.evidence_note = Some(p.message);
            Vec::new()
        }
        Ok((b, events)) => {
            d.evidence_status = "collected".into();
            d.evidence_note = Some(format!(
                "read in the guest at {} by a replay of the run (the recorded run is unchanged)",
                target.point()
            ));
            for (what, source, w) in b.works {
                if let Some(mut e) = to_evidence(&what, &source, &w) {
                    if let Some(svc) = what.strip_prefix("the log of ") {
                        keep_log(&rec.dir, svc, &mut e);
                    }
                    // Milestone 348: a workload command's output, beside the services' logs
                    if let Some(id) = what.strip_prefix(COMMAND_OUTPUT) {
                        keep_log(&rec.dir, &format!("command.{id}"), &mut e);
                    }
                    if what == WORKLOAD_OUTPUT {
                        keep_log(&rec.dir, "workload", &mut e);
                    }
                    if what == "the kernel's messages" {
                        for l in e.text.lines().filter(|l| kernel_worth(l)) {
                            if !d.kernel.iter().any(|k| k == l) {
                                d.kernel.push(l.to_string());
                            }
                        }
                    }
                    d.evidence.push(e);
                }
            }
            refine(d);
            events
        }
    }
}

/// How many failed composed commands the evidence reads the output of (Milestone 348).
const COMMANDS_MAX: usize = 4;
/// The evidence of a failed composed command's output, before the command's id.
const COMMAND_OUTPUT: &str = "the output of the command ";
/// The evidence of a plain workload's output (`workload.run`, Milestone 348).
const WORKLOAD_OUTPUT: &str = "the workload's output";

/// The composed workload's commands that ended with an exit status other than 0 in the
/// conversation of `target`, from the controller's `HYDO-CMD end i=<entry> cmd=<id> rc=<rc>`
/// reports: (the command's id, its output's file name in the composer's folder), the first
/// failure of each id, at most [`COMMANDS_MAX`] (Milestone 348). The output stays in the guest,
/// so only the replay can read it.
fn failed_commands(rec: &Recorded, target: Target) -> Vec<(String, String)> {
    let Some(t) = rec.transcript(target.conv) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for r in &t.records {
        let hydo_control::Record::Report { line, .. } = r else {
            continue;
        };
        if let Some(c) = failed_command(line) {
            if out.len() < COMMANDS_MAX && !out.iter().any(|(id, _)| *id == c.0) {
                out.push(c);
            }
        }
    }
    out
}

/// (id, `log.<entry>`) of a `HYDO-CMD end` report whose exit status is not 0.
fn failed_command(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("HYDO-CMD end ")?;
    let word = |k: &str| {
        rest.split(' ')
            .find_map(|w| w.strip_prefix(k))
            .map(str::to_string)
    };
    let (entry, id, rc) = (word("i=")?, word("cmd=")?, word("rc=")?);
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    };
    (rc != "0" && plain(&entry) && plain(&id)).then(|| (id, format!("log.{entry}")))
}

/// The reads of the evidence: the failing service's log first, then every other service's, the
/// provisioning's and setup logs, the output of the workload's failed commands (`cmds`, from
/// [`failed_commands`]) or of a plain workload, the readiness check run once, the kernel's
/// messages, memory and processes.
fn plan_works(
    plan: &Plan,
    d: &Diagnosis,
    cmds: &[(String, String)],
) -> Vec<(String, String, Work)> {
    let l = &plan.layout;
    let mut v: Vec<(String, String, Work)> = Vec::new();
    let tail_of = |p: String, n: u64| {
        Work::read(vec![FileReq {
            path: p,
            offset: n,
            max: n,
            tail: true,
        }])
    };
    let tail = |p: String| tail_of(p, TAIL);
    let mut names: Vec<String> = plan.services.iter().map(|s| s.name.clone()).collect();
    if let Some(s) = &d.service {
        names.retain(|n| n != s);
        names.insert(0, s.clone());
    }
    let bb = l.busybox.as_str();
    let ex = |script: String, ns: Option<String>| {
        Work::exec(bb, script, env(plan), ns, super::TIMEOUT_MS, TAIL)
    };
    for s in &names {
        let p = l.log_of(s);
        v.push((format!("the log of {s}"), p.clone(), tail_of(p, LOG_KEEP)));
        // the service's own log files (`logs:`, `errors.files`): databases write theirs into
        // their data directories, not to their output (Milestone 202)
        if let Some(sv) = plan.service(s).filter(|sv| !sv.logs.is_empty()) {
            v.push((
                format!("the log files of {s}"),
                sv.logs.join(" "),
                Work::exec(
                    bb,
                    super::log_files_script(&sv.logs, LOG_FILE_LINES),
                    env(plan),
                    None,
                    super::TIMEOUT_MS,
                    4 * TAIL,
                ),
            ));
        }
    }
    if let Some(s) = &d.service {
        let p = format!("{}/{s}.setup.log", l.dir);
        v.push((format!("the setup log of {s}"), p.clone(), tail(p)));
    }
    // Milestone 348: the workload's own output stays in the guest (open items 157 and 170)
    if plan.compose.is_some() {
        for (id, file) in cmds {
            let p = format!("{}/compose/{file}", l.dir);
            v.push((
                format!("{COMMAND_OUTPUT}{id}"),
                p.clone(),
                tail_of(p, LOG_KEEP),
            ));
        }
    } else {
        let p = format!("{}/workload.log", l.dir);
        v.push((WORKLOAD_OUTPUT.into(), p.clone(), tail_of(p, LOG_KEEP)));
    }
    if d.failure == "provision" {
        let p = format!("{}/provision.log", l.dir);
        v.push(("the provisioning's log".into(), p.clone(), tail(p)));
    }
    if let (Some(s), "service-not-ready" | "other") = (&d.service, d.failure.as_str()) {
        if let Some(sv) = plan.service(s) {
            let ns = netns_of(plan, Some(s)).ok().flatten();
            v.push((
                format!("the readiness check of {s}, run once"),
                sv.ready.clone(),
                ex(format!(". {} 2>/dev/null; {}", l.prelude, sv.ready), ns),
            ));
        }
    }
    let dmesg = "dmesg | grep -E 'Out of memory|oom-kill|Killed process|segfault|general protection|Kernel panic|BUG:|Oops|blocked for more than|page allocation failure' | tail -n 20; echo '--- the last lines'; dmesg | tail -n 15";
    v.push((
        "the kernel's messages".into(),
        dmesg.into(),
        ex(dmesg.into(), None),
    ));
    v.push((
        "memory".into(),
        "free -m".into(),
        ex("free -m".into(), None),
    ));
    v.push((
        "processes".into(),
        "ps -o pid,ppid,stat,rss,args".into(),
        ex(
            "ps -o pid,ppid,stat,rss,args 2>/dev/null || ps".into(),
            None,
        ),
    ));
    v
}

fn to_evidence(what: &str, source: &str, w: &Work) -> Option<Evidence> {
    let g = w.got.first()?;
    if let Some(e) = &g.error {
        if w.ended.is_none() {
            // a file that is not there is no evidence (a service without a setup log)
            let _ = e;
            return None;
        }
    }
    let mut text = g.text();
    if let (Some(err), Some(_)) = (w.got.get(1), &w.ended) {
        let t = err.text();
        if !t.trim().is_empty() {
            text.push_str("\n--- stderr\n");
            text.push_str(&t);
        }
    }
    Some(Evidence {
        what: what.into(),
        source: source.into(),
        text,
        bytes: g.size,
        truncated: g.truncated(),
        status: w.ended.as_ref().map(|e| e.status as u64),
    })
}

/// One service's memory at the end of a run (`hydo run --memory`, Milestone 239).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryPeak {
    pub service: String,
    /// The sum of the peak resident sizes (VmHWM) of the service's processes, in KiB.
    pub peak_kib: u64,
    /// The sum of their resident sizes now (VmRSS), in KiB.
    pub now_kib: u64,
    pub processes: usize,
}

/// The guest's lines `<VmHWM kB> <VmRSS kB> <fd 1's target>` per process, read by
/// [`memory_peaks`]: a service's processes write to its log, so their standard output names it.
fn memory_script(bb: &str) -> String {
    format!(
        "for p in /proc/[0-9]*; do o=$({bb} readlink $p/fd/1 2>/dev/null) || continue; \
         h=$({bb} sed -n 's/^VmHWM:[^0-9]*\\([0-9]*\\).*/\\1/p' $p/status 2>/dev/null); \
         r=$({bb} sed -n 's/^VmRSS:[^0-9]*\\([0-9]*\\).*/\\1/p' $p/status 2>/dev/null); \
         echo \"${{h:-0}} ${{r:-0}} $o\"; done"
    )
}

/// The lines of [`memory_script`] summed per service, for (service, its log) in the plan's
/// order; a process whose output goes elsewhere (the workload, the agent) is left out.
fn memory_of(services: &[(String, String)], out: &str) -> Vec<MemoryPeak> {
    let mut v: Vec<MemoryPeak> = services
        .iter()
        .map(|(s, _)| MemoryPeak {
            service: s.clone(),
            peak_kib: 0,
            now_kib: 0,
            processes: 0,
        })
        .collect();
    for l in out.lines() {
        let mut w = l.splitn(3, ' ');
        let (Some(h), Some(r), Some(o)) = (w.next(), w.next(), w.next()) else {
            continue;
        };
        let Some(i) = services.iter().position(|(_, log)| log == o.trim()) else {
            continue;
        };
        let m = &mut v[i];
        m.peak_kib += h.parse::<u64>().unwrap_or(0);
        m.now_kib += r.parse::<u64>().unwrap_or(0);
        m.processes += 1;
    }
    v
}

/// Each service's memory at the end of the recorded run in `dir`, read in the guest by one
/// replay to the run's end (the recorded run is unchanged, so no fingerprint moves). Only the
/// processes alive at the end count: a process that was killed or restarted took its peak with
/// it.
pub fn memory_peaks(ctx: &Ctx, dir: &Path) -> Result<Vec<MemoryPeak>, String> {
    let rec = Recorded::load(dir).map_err(|p| p.message)?;
    let target = rec.end().ok_or("the run has no recorded end")?;
    let tmp = super::session::scratch(ctx, "memory");
    let r = (|| -> Result<(Plan, Batch), Problem> {
        let m = super::session::machine(ctx, &rec, &tmp)?;
        let bb = m.plan.layout.busybox.clone();
        let w = Work::exec(
            &bb,
            memory_script(&bb),
            env(&m.plan),
            None,
            super::TIMEOUT_MS,
            64 * 1024,
        );
        let mut b = Batch {
            works: vec![("memory".into(), String::new(), w)],
            i: 0,
            stops: 0,
        };
        m.reach(&rec, target, &mut b, false, &tmp.join("engine.err"))?;
        Ok((m.plan.clone(), b))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    let (plan, b) = r.map_err(|p| p.message)?;
    let w = &b.works[0].2;
    if let Some(f) = &w.failed {
        return Err(f.clone());
    }
    let out = w.got.first().map(|g| g.text()).unwrap_or_default();
    let services: Vec<(String, String)> = plan
        .services
        .iter()
        .map(|s| (s.name.clone(), plan.layout.log_of(&s.name)))
        .collect();
    Ok(memory_of(&services, &out))
}

/// A service log's text without terminal colour and cursor codes.
pub fn strip_ansi(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut it = t.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.next() {
            // CSI: parameters and intermediates up to a final byte @..~
            Some('[') => {
                for x in it.by_ref() {
                    if ('@'..='~').contains(&x) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC \
            Some(']') => {
                while let Some(x) = it.next() {
                    if x == '\u{7}' {
                        break;
                    }
                    if x == '\u{1b}' {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The log as read (its last [`LOG_KEEP`] bytes, colour codes removed) into
/// `<run dir>/logs/<service>.log`, and the evidence cut to its last [`TAIL`] bytes at a line
/// start, so that `result.json` keeps its size and every quoted line is whole.
fn keep_log(dir: &Path, svc: &str, e: &mut Evidence) {
    let clean = strip_ansi(&e.text);
    let clean = if e.truncated {
        // the first line of a tail read starts in the middle
        match clean.split_once('\n') {
            Some((_, rest)) if !rest.is_empty() => rest.to_string(),
            _ => clean,
        }
    } else {
        clean
    };
    let logs = dir.join(LOGS_DIR);
    if std::fs::create_dir_all(&logs).is_ok() {
        let _ = std::fs::write(logs.join(format!("{svc}.log")), &clean);
    }
    let keep = TAIL as usize;
    if clean.len() > keep {
        e.truncated = true;
    }
    e.text = if clean.len() > keep {
        let mut at = clean.len() - keep;
        while !clean.is_char_boundary(at) {
            at += 1;
        }
        let t = &clean[at..];
        match t.split_once('\n') {
            Some((_, rest)) if !rest.is_empty() => rest.to_string(),
            _ => t.to_string(),
        }
    } else {
        clean
    };
}

/// The last lines of a log for the summary: whole lines, the last [`QUOTE_LINES`] at most,
/// while they fit into [`QUOTE_CHARS`] characters, and at least the last one.
fn last_lines(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut used = 0usize;
    for l in lines.iter().rev().take(QUOTE_LINES) {
        let n = l.chars().count();
        if !out.is_empty() && used + n > QUOTE_CHARS {
            break;
        }
        used += n;
        out.push(l.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    out.reverse();
    out
}

/// Words of a service's last log lines that name a cause in its environment: a permission it
/// lacks, a read-only file system, a program or library that cannot run, memory or an address it
/// cannot get.
const ENVIRONMENT_WORDS: &[&str] = &[
    "permission denied",
    "operation not permitted",
    "read-only file system",
    "exec format error",
    "error while loading shared libraries",
    "cannot allocate memory",
    "out of memory",
    "address already in use",
];

/// Words of a service's last log lines that name damaged or lost data, from the file system
/// (`EUCLEAN`, `EIO`, `EBADMSG`) or from the service's own checks.
const DATA_WORDS: &[&str] = &[
    "structure needs cleaning",
    "input/output error",
    "bad message",
    "corrupt",
    "checksum",
    "invalid page",
    "invalid record",
    "invalid magic",
    "bad magic",
    "unexpected end",
    "truncated",
    "no such file or directory",
];

/// Milestone 316: the rule for a service that did not start in the recovery boot after a power
/// cut, whose verdict is always `VIOLATION service cannot recover (committed data
/// unrecoverable)`. When its last log lines name a cause in its environment
/// ([`ENVIRONMENT_WORDS`]) and none of them names damaged or missing data ([`DATA_WORDS`]; a
/// missing file counts as data, since a lying disk loses files), the diagnosis says so and does
/// not repeat the claim of lost data: the line with the cause. The verdict and the explorer's
/// class stay, so every recorded value stays. A user's PostgreSQL that found devtmpfs's
/// `/dev/shm` in its recovery boot stopped with `could not open shared memory segment ...:
/// Permission denied`, and the run claimed its committed data unrecoverable.
fn environment_cause(tail: &[String]) -> Option<String> {
    let has = |l: &str, words: &[&str]| {
        let l = l.to_lowercase();
        words.iter().any(|w| l.contains(w))
    };
    if tail.iter().any(|l| has(l, DATA_WORDS)) {
        return None;
    }
    tail.iter()
        .rev()
        .find(|l| has(l, ENVIRONMENT_WORDS))
        .map(|l| l.chars().take(200).collect())
}

/// Sharpen the summary with what the evidence showed.
fn refine(d: &mut Diagnosis) {
    if let Some(l) = d.kernel.iter().find(|l| l.contains("Killed process")) {
        let l = l.clone();
        d.likely
            .retain(|x| !x.contains("almost always the kernel's out-of-memory killer"));
        d.likely.insert(
            0,
            format!("the kernel's out-of-memory killer: {}", l.trim()),
        );
    }
    if matches!(
        d.failure.as_str(),
        "service-exited" | "service-setup" | "restart"
    ) {
        if let Some(s) = &d.service {
            let want = format!("the log of {s}");
            if let Some(e) = d.evidence.iter().find(|e| e.what == want) {
                let tail = last_lines(&e.text);
                if !tail.is_empty() {
                    d.summary.push_str(&format!(
                        "; its log ends: {} (the whole log: {LOGS_DIR}/{s}.log in the run directory)",
                        tail.join(" | ")
                    ));
                }
                let recovery = d.summary.contains(conv_words(Conv::Recovery));
                if let Some(cause) = recovery.then(|| environment_cause(&tail)).flatten() {
                    let why = format!(
                        "{s} did not start because of its environment, not because of lost data: its \
                         log ends with `{cause}` and names no damaged file, so this run does not show \
                         what the verdict's words \"committed data unrecoverable\" say; look at what \
                         {s} needs at its start (its mounts, owners and modes, programs and memory)"
                    );
                    d.summary.push_str(&format!("; {why}"));
                    d.likely.insert(0, why);
                }
            }
        }
    }
    if d.failure == "service-not-ready" {
        if let Some(e) = d
            .evidence
            .iter()
            .find(|e| e.what.starts_with("the readiness check"))
        {
            d.summary.push_str(&format!(
                "; run once at the end it gives status {} and prints: {}",
                e.status.unwrap_or(0),
                super::timeline::diff_cut(
                    if e.text.trim().is_empty() {
                        "(nothing)"
                    } else {
                        e.text.trim()
                    },
                    200
                )
            ));
        }
        if let Some(s) = &d.service {
            let want = format!("the log of {s}");
            if d.evidence.iter().any(|e| e.what == want) {
                d.summary.push_str(&format!(
                    "; its log: {LOGS_DIR}/{s}.log in the run directory"
                ));
            }
        }
    }
}

/// `hydo inspect diagnose <run dir> [--evidence]`.
pub fn cmd(ctx: &Ctx, rest: &[String]) -> Result<Value, Problem> {
    let mut a = Args::new(rest);
    let ev = a.flag("--evidence");
    let make = a.flag("--run");
    let problem = a.num("--problem")?;
    let dir = a.one("a run directory")?;
    a.done()?;
    if Path::new(&dir)
        .join(hydo_results::EXPLORATION_DOC)
        .is_file()
    {
        return explored(ctx, &dir, problem.unwrap_or(1), make, ev);
    }
    if problem.is_some() || make {
        return Err(Problem::new(
            "usage",
            format!(
                "{dir}: --problem and --run take an exploration's folder (with exploration.json)"
            ),
            vec![format!("hydo inspect diagnose {dir}")],
        ));
    }
    diagnose_dir(ctx, &dir, ev, Vec::new())
}

/// The diagnosis of the run directory `dir`, with `notes` before its own next steps.
fn diagnose_dir(ctx: &Ctx, dir: &str, ev: bool, notes: Vec<String>) -> Result<Value, Problem> {
    let rec = Recorded::load(Path::new(dir))?;
    diagnose_rec(ctx, &rec, dir, ev, notes)
}

/// `hydo inspect diagnose <exploration> [--problem N] [--run]` (Milestone 324): problem N as
/// `hydo report` numbers it (default 1). An exploration keeps no run directory, so the
/// diagnosis reads, in this order: the run directory that the problem's `hydo run ... --out
/// <exploration>/runs/seed-<n>` made; the explorer's fresh replay of its failing run
/// (`replays/<tag>/`, made for a run that stopped with an error); the exploration's own
/// transcripts of the run (`consoles/<tag>.control.jsonl`). When none is there, it names the
/// command that makes the run directory, and `--run` makes it (one VM run) and reads it.
fn explored(ctx: &Ctx, dir: &str, n: u64, make: bool, ev: bool) -> Result<Value, Problem> {
    let p = Path::new(dir);
    let (doc, events) = match hydo_results::read_dir(p) {
        Ok(hydo_results::Results::Exploration {
            doc: Some(doc),
            events,
            ..
        }) => (doc, events),
        Ok(_) => {
            return Err(Problem::new(
                "no-run",
                format!("{dir}: the exploration has not finished"),
                vec![format!("hydo report {dir}")],
            ))
        }
        Err(e) => return Err(Problem::new("no-run", format!("{dir}: {e}"), vec![])),
    };
    let st = crate::htmlreport::exploration_story(p, &doc, events.as_ref(), dir);
    // the problems as `hydo report` numbers them; the services' error lines have no run
    let all = &st.items;
    let failing = all.iter().filter(|i| i.run_dir.is_some()).count();
    if failing == 0 {
        return Err(Problem::new(
            "usage",
            format!("{dir}: no run of the exploration failed: nothing to diagnose"),
            vec![format!("hydo report {dir}")],
        ));
    }
    let Some(it) = n.checked_sub(1).and_then(|i| all.get(i as usize)) else {
        return Err(Problem::new(
            "usage",
            format!(
                "{dir}: problem {n}: the exploration has {} problem{} (hydo report numbers them)",
                all.len(),
                if all.len() == 1 { "" } else { "s" }
            ),
            vec![
                format!("hydo report {dir}"),
                format!("hydo inspect diagnose {dir} --problem 1"),
            ],
        ));
    };
    let (Some(run_dir), Some(command)) = (it.run_dir.clone(), it.run_command.clone()) else {
        return Err(Problem::new(
            "usage",
            format!(
                "{dir}: problem {n} has no failing run to diagnose: {}",
                it.problem.title
            ),
            vec![it.command.clone(), format!("hydo report {dir}")],
        ));
    };
    let title = format!("problem {n} ({})", it.problem.title);
    let others = if all.len() > 1 {
        vec![format!(
            "the exploration has {} problems: hydo inspect diagnose {dir} --problem <1-{}>",
            all.len(),
            all.len()
        )]
    } else {
        Vec::new()
    };
    // 1. the run directory that the problem's command made
    if Path::new(&run_dir).join(hydo_results::RUN_DOC).is_file() {
        let mut notes = vec![format!("{title}: its run directory {run_dir}")];
        notes.extend(others);
        return diagnose_dir(ctx, &run_dir, ev, notes);
    }
    // 2. the explorer's fresh replay of the failing run, with the failure it shows
    let tag = it.tag.clone().unwrap_or_default();
    let failure = doc
        .failures
        .iter()
        .find(|f| f.run.tag.as_deref() == Some(tag.as_str()));
    for t in doc
        .failures
        .iter()
        .filter(|f| f.signature == failure.map(|x| x.signature.clone()).unwrap_or_default())
        .filter_map(|f| f.run.tag.clone())
    {
        let r = p.join(crate::htmlreport::REPLAYS).join(&t);
        if let Ok(run) = hydo_results::reader::read_run(&r.join(hydo_results::RUN_DOC)) {
            let same = hydo_explore::report::sig_key(&run.verdict.text)
                == hydo_explore::report::sig_key(
                    &failure
                        .map(|f| f.run.verdict.text.clone())
                        .unwrap_or_default(),
                );
            if same && run.verdict.kind != "ok" {
                let rd = r.display().to_string();
                let mut notes = vec![format!(
                    "{title}: the explorer's replay of run {t} ({rd}), which failed the same way; \
                     a run directory of the problem's own schedule: {command}"
                )];
                notes.extend(others);
                return diagnose_dir(ctx, &rd, ev, notes);
            }
        }
    }
    // 3. the exploration's own transcripts of the run
    let run_doc = st
        .by_tag
        .get(&tag)
        .map(|r| (*r).clone())
        .or_else(|| failure.map(|f| f.run.clone()));
    let mut why = format!("{dir} keeps no run directory of {title}");
    let files = super::drive::RunFiles::explorer(&tag);
    let kept = p.join(files.transcript(Conv::Run)).is_file()
        || p.join(files.transcript(Conv::Recovery)).is_file();
    if !kept {
        why = format!(
            "{why}, and no control transcript of its run {tag} ({}; the scripted engine and \
             shell images keep none)",
            files.transcript(Conv::Run)
        );
    }
    if let Some(run) = run_doc.filter(|_| kept) {
        match Recorded::of(p, run, super::drive::RunFiles::explorer(&tag)) {
            Ok(rec) if !ev => {
                let mut notes = vec![format!(
                    "{title}: read from the exploration's own files of run {tag} \
                     (consoles/{tag}.*); a run directory of it, for hydo inspect start and \
                     --evidence: {command}"
                )];
                notes.extend(others.clone());
                // the other `hydo inspect` commands take a run directory, which this is not
                let v = diagnose_rec(ctx, &rec, dir, false, notes)?;
                let mut doc: DiagnosisDoc = serde_json::from_value(v).unwrap_or_default();
                doc.diagnosis
                    .next
                    .retain(|x| !x.starts_with("hydo inspect"));
                doc.diagnosis.next.push(format!(
                    "after that command: hydo inspect diagnose {run_dir} --evidence, hydo inspect \
                     start {run_dir} --at end"
                ));
                return Ok(serde_json::to_value(&doc).unwrap_or(Value::Null));
            }
            Ok(_) => {
                why = format!(
                    "{why}, and --evidence replays a run directory, which the exploration's own \
                     files of run {tag} are not"
                )
            }
            Err(e) => {
                why = format!(
                    "{why}, and its own files of run {tag} cannot be read: {}",
                    e.message
                )
            }
        }
    }
    // 4. make the run directory (one VM run), or say how
    if !make {
        return Err(Problem::new(
            "no-run",
            format!("{why}: make one with the command below, or run this again with --run"),
            vec![
                command,
                format!("hydo inspect diagnose {dir} --problem {n} --run"),
            ],
        ));
    }
    let image = doc
        .image
        .resolved
        .clone()
        .unwrap_or_else(|| doc.image.path.clone());
    let mut opts = crate::runs::RunOpts::of_eargs(doc.engine.args.as_deref().unwrap_or(&[]));
    if opts.vcpus.is_none() {
        opts.vcpus = doc.engine.vcpus.filter(|n| *n > 1).map(|n| n as u32);
    }
    let out = Path::new(&run_dir);
    let _ = std::fs::create_dir_all(out);
    if let Some(e) = &ctx.config.pool.scripted_engine {
        // the tests: the scripted engine's `one` mode leaves the run directory
        let ok = std::process::Command::new(e)
            .arg("one")
            .arg(out)
            .arg(&it.schedule)
            .env("HYDO_SCRIPTED_IMAGE", &image)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return Err(Problem::new(
                "no-run",
                format!("{}: the scripted engine failed", e.display()),
                vec![],
            ));
        }
        let mut notes = vec![format!("{title}: ran {command}")];
        notes.extend(others);
        return diagnose_dir(ctx, &run_dir, ev, notes);
    }
    match crate::runs::try_run_to_dir(
        ctx,
        Path::new(&image),
        doc.image.sha256.clone(),
        &it.schedule,
        out,
        &opts,
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err(Problem::new(
                "no-run",
                format!("{command}: the run gave no result (engine.log in {run_dir})"),
                vec![],
            ))
        }
        Err(e) => return Err(Problem::new("no-run", format!("{command}: {e}"), vec![])),
    }
    let mut notes = vec![format!("{title}: ran {command}")];
    notes.extend(others);
    diagnose_dir(ctx, &run_dir, ev, notes)
}

fn diagnose_rec(
    ctx: &Ctx,
    rec: &Recorded,
    dir: &str,
    ev: bool,
    notes: Vec<String>,
) -> Result<Value, Problem> {
    let mut d = match host(rec) {
        Some(d) => d,
        None => {
            return Err(Problem::new(
                "usage",
                format!(
                    "the run passed ({}): nothing to diagnose",
                    rec.doc.verdict.text
                ),
                vec![
                    format!("hydo inspect events {dir}"),
                    format!("hydo inspect start {dir} --at end"),
                ],
            ))
        }
    };
    if ev {
        d = with_evidence(ctx, rec, d).0;
        d.next.retain(|n| !n.contains("--evidence"));
    }
    // what an exploration's diagnosis read, first (Milestone 324)
    if !notes.is_empty() {
        let mut next = notes;
        next.append(&mut d.next);
        d.next = next;
    }
    let doc = DiagnosisDoc {
        run_dir: rec.dir.display().to_string(),
        verdict: rec.doc.verdict.clone(),
        diagnosis: d,
        ..DiagnosisDoc::default()
    };
    Ok(serde_json::to_value(&doc).unwrap_or(Value::Null))
}

/// `hydo run`'s diagnosis of a run it just made: the host layer always, the evidence for an
/// ERROR or TIMEOUT unless `evidence` is off. None for a passing run or a directory without
/// transcripts.
pub fn for_run(ctx: &Ctx, dir: &Path, evidence_on: bool) -> Option<Diagnosis> {
    let rec = Recorded::load(dir).ok()?;
    let mut d = host(&rec)?;
    // a violation in the recovery boot whose consoles carry no agent lines (hvx): only the
    // replay can tell a service that did not come back from lost data (Milestone 192)
    let blind_recovery = rec.doc.verdict.kind == "violation"
        && rec.end().map(|t| t.conv) == Some(Conv::Recovery)
        && !agent_events(&rec)
            .iter()
            .any(|a| a.conv == Conv::Recovery && matches!(a.kind, "exit" | "done" | "probe"));
    // a violation that names a service which did not start again (`service cannot recover ...
    // (S)`): its log says why, and only the replay reads it
    let named =
        rec.doc.verdict.kind == "violation" && verdict_service(&rec.doc.verdict.text).is_some();
    if evidence_on && (rec.doc.verdict.kind == "error" || blind_recovery || named) {
        let (d2, ev) = with_evidence(ctx, &rec, d);
        d = d2;
        d.next.retain(|n| !n.contains("--evidence"));
        // keep what the replay saw of the agent when the consoles do not have it (hvx), so that
        // `hydo inspect events` shows the exits and readiness checks without another replay
        let console = agent_events(&rec);
        if !ev.is_empty()
            && !console
                .iter()
                .any(|a| matches!(a.kind, "exit" | "done" | "probe"))
        {
            let _ = std::fs::write(
                dir.join(super::timeline::AGENT_EVENTS),
                super::timeline::agent_events_file(&ev),
            );
        }
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_in_verdicts() {
        assert_eq!(
            verdict_service("HYDO-ORACLE: ERROR service start (medusa)").as_deref(),
            Some("medusa")
        );
        assert_eq!(
            verdict_service("HYDO-ORACLE: ERROR tigerbeetle_0 restart failed").as_deref(),
            Some("tigerbeetle_0")
        );
        assert_eq!(
            verdict_service("HYDO-ORACLE: ERROR restart after the faults (redis)").as_deref(),
            Some("redis")
        );
        assert_eq!(
            verdict_service(
                "HYDO-ORACLE: VIOLATION service cannot recover (committed data unrecoverable) (tb_0)"
            )
            .as_deref(),
            Some("tb_0")
        );
        assert_eq!(
            verdict_service("HYDO-ORACLE: VIOLATION service cannot recover after the faults (db)")
                .as_deref(),
            Some("db")
        );
        assert_eq!(
            verdict_service(
                "HYDO-ORACLE: VIOLATION [upgrade.started] etcd1 did not start on version 3.6.0 \
                 before step 5 (it ran 3.5.5)"
            )
            .as_deref(),
            Some("etcd1")
        );
        assert_eq!(
            verdict_service(
                "HYDO-ORACLE: VIOLATION service cannot recover (committed data unrecoverable)"
            ),
            None
        );
        assert_eq!(
            verdict_service(
                "HYDO-ORACLE: VIOLATION service stopped in a run without faults (tigerbeetle_0)"
            )
            .as_deref(),
            Some("tigerbeetle_0")
        );
        assert_eq!(verdict_service("HYDO-ORACLE: ERROR provision failed"), None);
        assert_eq!(verdict_service("HYDO-ORACLE: VIOLATION [x] y"), None);
    }

    /// Milestone 348: the composed commands whose output the evidence reads.
    #[test]
    fn a_failed_command_is_named_by_its_end_report() {
        assert_eq!(
            failed_command("HYDO-CMD end i=12 cmd=parallel_driver_write rc=1 ms=40 status=failed"),
            Some(("parallel_driver_write".into(), "log.12".into()))
        );
        assert_eq!(
            failed_command("HYDO-CMD end i=e2 cmd=eventually_drain rc=143 ms=9000"),
            Some(("eventually_drain".into(), "log.e2".into()))
        );
        assert_eq!(
            failed_command("HYDO-CMD end i=3 cmd=anytime_read rc=0 ms=10 status=ok"),
            None
        );
        assert_eq!(failed_command("HYDO-CMD start i=3 kind=x cmd=y"), None);
        // a word that could leave the composer's folder is not read
        assert_eq!(failed_command("HYDO-CMD end i=../x cmd=y rc=1 ms=1"), None);
    }

    #[test]
    fn memory_is_summed_per_service_by_the_log_its_processes_write() {
        let svcs = vec![
            ("db".to_string(), "/tmp/hydo/db.log".to_string()),
            ("api".to_string(), "/tmp/hydo/api.log".to_string()),
            ("idle".to_string(), "/tmp/hydo/idle.log".to_string()),
        ];
        let out = "2048 1024 /tmp/hydo/db.log\n\
                   512 500 /tmp/hydo/db.log\n\
                   4096 4000 /tmp/hydo/api.log\n\
                   9999 9999 /dev/console\n\
                   77 77 /tmp/hydo-inspect/out\n";
        let m = memory_of(&svcs, out);
        assert_eq!(
            m.iter()
                .map(|x| (x.service.as_str(), x.peak_kib, x.now_kib, x.processes))
                .collect::<Vec<_>>(),
            [
                ("db", 2560, 1524, 2),
                ("api", 4096, 4000, 1),
                ("idle", 0, 0, 0)
            ]
        );
        assert!(memory_script("/bin/busybox").contains("/bin/busybox readlink $p/fd/1"));
    }

    /// Milestone 316: a service that does not start in the recovery boot because of its
    /// environment is not said to have lost its data; damaged data keeps the verdict's words.
    #[test]
    fn a_recovery_start_that_fails_on_its_environment_claims_no_lost_data() {
        let d = |log: &str, conv: Conv| {
            let mut d = Diagnosis {
                summary: format!(
                    "db exited while it started {}, before its readiness check passed (exit status 1)",
                    conv_words(conv)
                ),
                failure: "service-exited".into(),
                service: Some("db".into()),
                evidence: vec![Evidence {
                    what: "the log of db".into(),
                    source: "/tmp/hydo/db.log".into(),
                    text: log.into(),
                    bytes: log.len() as u64,
                    truncated: false,
                    status: None,
                }],
                ..Default::default()
            };
            refine(&mut d);
            d
        };
        let shm = "LOG:  database system was interrupted\n\
                   FATAL:  could not open shared memory segment \"/PostgreSQL.1\": Permission denied\n\
                   LOG:  database system is shut down\n";
        let got = d(shm, Conv::Recovery);
        assert!(
            got.summary.contains(
                "db did not start because of its environment, not because of lost data: its log \
                 ends with `FATAL: could not open shared memory segment \"/PostgreSQL.1\": \
                 Permission denied`"
            ),
            "{}",
            got.summary
        );
        assert!(got.likely[0].starts_with("db did not start because of its environment"));
        // a damaged file: the verdict's words stand
        let lying = "chown: cannot access '/var/lib/postgresql/data/base/5/2600': Structure needs cleaning\n\
                     chown: /var/lib/postgresql/data: Permission denied\n";
        let got = d(lying, Conv::Recovery);
        assert!(!got.summary.contains("environment"), "{}", got.summary);
        assert!(got.likely.is_empty());
        // a missing file counts as lost data (a lying disk loses files)
        let gone = "open /data/wal/0001: no such file or directory\npermission denied\n";
        assert!(!d(gone, Conv::Recovery).summary.contains("environment"));
        // the first boot's start has no claim about data to correct
        assert!(!d(shm, Conv::Boot).summary.contains("environment"));
        assert_eq!(environment_cause(&[]), None);
    }

    #[test]
    fn a_service_log_goes_to_the_run_directory_and_the_summary_quotes_whole_lines() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m x\u{1b}]0;t\u{7}y"),
            "2026 ERROR xy"
        );
        let dir = std::env::temp_dir().join(format!("hydo-keep-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // a tail read: the first line starts in the middle; the last line is long
        let long = format!("\u{1b}[31mnode: store full: {}\u{1b}[0m", "x".repeat(510));
        let mut body = String::from("ing the repl\n");
        for i in 0..200 {
            body.push_str(&format!("line {i} of the start\n"));
        }
        body.push_str("opening the replica\n");
        body.push_str(&long);
        body.push('\n');
        let mut e = Evidence {
            what: "the log of node0".into(),
            source: "/tmp/hydo/node0.log".into(),
            text: body.clone(),
            bytes: 100_000,
            truncated: true,
            status: None,
        };
        keep_log(&dir, "node0", &mut e);
        let file = std::fs::read_to_string(dir.join("logs/node0.log")).unwrap();
        assert!(file.starts_with("line 0 of the start\n"), "{}", &file[..40]);
        assert!(!file.contains('\u{1b}'));
        assert!(file.ends_with(&format!("store full: {}\n", "x".repeat(510))));
        assert!(e.truncated && e.text.len() <= TAIL as usize);
        assert!(e.text.starts_with("line "), "{}", &e.text[..20]);
        // whole lines from the end while they fit into 600 characters
        let q = last_lines(&e.text);
        assert_eq!(q.len(), 4, "{q:?}");
        assert_eq!(q[0], "line 198 of the start");
        assert_eq!(q[2], "opening the replica");
        assert!(q[3].ends_with(&"x".repeat(510)));
        // one line longer than that is quoted whole
        let one = format!("a\n{}\n", "y".repeat(900));
        assert_eq!(last_lines(&one), ["y".repeat(900)]);
        // short lines: the last five
        let q = last_lines("a\nb\n\nc\nd\ne\nf\ng\n");
        assert_eq!(q, ["c", "d", "e", "f", "g"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sigkill_hydo_did_not_send_points_at_memory() {
        let p = ProcessEnd {
            handle: "provision".into(),
            status: 137,
            signal: Some(9),
            signal_by_hydo: Some(false),
            ..ProcessEnd::default()
        };
        assert!(likely_of(&p, 256).unwrap().contains("out-of-memory killer"));
        let mut k = p.clone();
        k.signal_by_hydo = Some(true);
        assert_eq!(likely_of(&k, 256), None);
        let nf = ProcessEnd {
            handle: "s".into(),
            status: 127,
            ..ProcessEnd::default()
        };
        assert!(likely_of(&nf, 1024).unwrap().contains("not found"));
    }
}

/// The recorded runs of `tests/fixtures/inspect/` (real hvx runs of the Redis example and broken
/// copies of it): what the host layer and the timeline say, and that reading never writes.
#[cfg(test)]
mod fixtures {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/inspect")
            .join(name)
    }

    /// Every file's bytes, by name.
    fn files(d: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_service_that_exits_at_start_is_named_with_its_status() {
        let d = fixture("badconf");
        let before = files(&d);
        let rec = Recorded::load(&d).unwrap();
        let g = host(&rec).unwrap();
        assert_eq!(g.failure, "service-exited");
        assert_eq!(g.service.as_deref(), Some("redis"));
        let p = g.process.as_ref().unwrap();
        assert_eq!((p.handle.as_str(), p.status, p.signal), ("redis", 1, None));
        assert_eq!(g.probe.as_ref().unwrap().why, "died");
        assert_eq!(g.verdict_at.as_ref().unwrap().point, "boot:22257");
        // the timeline has the exit at its stop, from the replayed agent events (hvx)
        let ev = super::super::timeline::events(&rec);
        let exit = ev.iter().find(|e| e.kind == "exit").unwrap();
        assert_eq!(exit.point.as_deref(), Some("boot:22247"));
        assert_eq!(exit.service.as_deref(), Some("redis"));
        assert_eq!(files(&d), before, "inspecting changed the recorded run");
    }

    #[test]
    fn a_sigkill_nobody_sent_in_provisioning() {
        let d = fixture("oom");
        let before = files(&d);
        let rec = Recorded::load(&d).unwrap();
        let g = host(&rec).unwrap();
        assert_eq!(g.failure, "provision");
        let p = g.process.as_ref().unwrap();
        assert_eq!(
            (p.signal_name.as_deref(), p.signal_by_hydo),
            (Some("KILL"), Some(false))
        );
        assert!(
            g.likely[0].contains("out-of-memory killer"),
            "{:?}",
            g.likely
        );
        assert_eq!(files(&d), before);
    }

    #[test]
    fn a_power_cut_run_has_three_conversations_and_its_points() {
        use super::super::drive::Target;
        use super::super::{target_of, timeline};
        let d = fixture("cut");
        let before = files(&d);
        let rec = Recorded::load(&d).unwrap();
        let g = host(&rec).unwrap();
        assert_eq!(g.failure, "violation");
        let end = rec.end().unwrap();
        assert_eq!(end.conv, Conv::Recovery);
        // snapshots: the workload yes, the recovery boot no
        let before_cut = Target {
            conv: Conv::Run,
            stop: 23170,
        };
        assert!(rec.snapshot_ok(before_cut).is_ok());
        assert!(rec.snapshot_ok(end).is_err());
        assert_eq!(target_of(&rec, "run:23170").unwrap(), before_cut);
        assert_eq!(target_of(&rec, "end").unwrap(), end);
        for bad in ["run:1", "nowhere:3", "run:x", "recovery:99999999"] {
            let p = target_of(&rec, bad).unwrap_err();
            assert_eq!(p.code, "bad-point", "{bad}");
            assert!(!p.try_.is_empty(), "{bad}");
        }
        let ev = timeline::events(&rec);
        let cut = ev.iter().find(|e| e.kind == "fault").unwrap();
        assert_eq!(cut.point.as_deref(), Some("run:23170"));
        assert_eq!(cut.service, None, "POWER CUT names no service");
        // the documents are results documents, paged
        let dir = d.display().to_string();
        let doc = timeline::cmd(&[dir.clone(), "--kind".into(), "fault,verdict".into()]).unwrap();
        match hydo_results::reader::document(&doc.to_string()).unwrap() {
            hydo_results::reader::Record::Timeline(t) => {
                assert!(t
                    .events
                    .iter()
                    .all(|e| e.kind == "fault" || e.kind == "verdict"));
                assert!(!t.truncated);
            }
            r => panic!("{r:?}"),
        }
        let page = timeline::cmd(&[dir, "--limit".into(), "2".into()]).unwrap();
        assert_eq!(page["truncated"], true);
        assert!(page["next"].as_str().unwrap().contains("--offset 2"));
        assert_eq!(files(&d), before);
        // hvx: no agent line in the consoles, so the host layer cannot claim no process failed
        assert!(
            g.summary.contains("--evidence") && !g.summary.contains("No service exited"),
            "{}",
            g.summary
        );
    }

    /// The power-cut run with the agent events a replay would keep (`agent-events.jsonl`), in a
    /// copy of the fixture.
    fn cut_with_events(name: &str, events: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-diagnose-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for e in std::fs::read_dir(fixture("cut")).unwrap().flatten() {
            std::fs::copy(e.path(), d.join(e.file_name())).unwrap();
        }
        std::fs::write(d.join(super::super::timeline::AGENT_EVENTS), events).unwrap();
        d
    }

    /// Milestone 192: the recovery boot's start of a service fails (its process exits with
    /// status 2 and its readiness check sees it die): the host controller's verdict is
    /// `VIOLATION service cannot recover (committed data unrecoverable) (redis)`, and the
    /// diagnosis names redis, its exit and its readiness check instead of "No process failed".
    #[test]
    fn a_service_that_does_not_start_in_the_recovery_boot_is_named() {
        let d = cut_with_events(
            "nostart",
            concat!(
                r#"{"conversation":"recovery","stop":23300,"at":824500,"line":{"ev":"exit","h":"redis","id":12,"v":1,"status":2}}"#,
                "\n",
                r#"{"conversation":"recovery","stop":23306,"at":824608,"line":{"ev":"probe","id":13,"ok":false,"why":"died","status":1,"tries":1,"v":1}}"#,
                "\n"
            ),
        );
        let v =
            "HYDO-ORACLE: VIOLATION service cannot recover (committed data unrecoverable) (redis)";
        let p = d.join(hydo_results::RUN_DOC);
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        doc["verdict"]["text"] = Value::String(v.into());
        std::fs::write(&p, serde_json::to_string(&doc).unwrap()).unwrap();
        let rec = Recorded::load(&d).unwrap();
        let g = host(&rec).unwrap();
        assert_eq!(g.failure, "service-exited", "{}", g.summary);
        assert_eq!(g.service.as_deref(), Some("redis"));
        let pe = g.process.as_ref().unwrap();
        assert_eq!((pe.status, pe.signal), (2, None));
        assert_eq!(g.probe.as_ref().unwrap().why, "died");
        assert!(
            g.summary
                .contains("in the recovery boot after the power cut")
                && g.summary.contains("exit status 2"),
            "{}",
            g.summary
        );
        // without the replayed events (hvx), the summary says where to get them
        std::fs::remove_file(d.join(super::super::timeline::AGENT_EVENTS)).unwrap();
        let g2 = host(&Recorded::load(&d).unwrap()).unwrap();
        assert!(g2.summary.contains("--evidence"), "{}", g2.summary);
        let _ = std::fs::remove_dir_all(d);
    }

    /// Milestone 192: a service that exits by itself in the recovery boot is named, so that a
    /// "lost" verdict is not read as data loss (it said "No process failed" before).
    #[test]
    fn a_service_that_dies_in_the_recovery_boot_is_named() {
        // redis (spawned as command 12 in the recovery boot) passed its readiness check
        // (command 13), then exited with status 2 before the check (command 14) read it
        let d = cut_with_events(
            "died",
            concat!(
                r#"{"conversation":"recovery","stop":23999,"at":841857,"line":{"ev":"probe","id":13,"ok":true,"why":"ok","status":0,"tries":1,"v":1}}"#,
                "\n",
                r#"{"conversation":"recovery","stop":24019,"at":842175,"line":{"ev":"exit","h":"redis","id":12,"v":1,"status":2}}"#,
                "\n"
            ),
        );
        let rec = Recorded::load(&d).unwrap();
        let g = host(&rec).unwrap();
        assert_eq!(g.failure, "service-exited", "{}", g.summary);
        assert_eq!(g.service.as_deref(), Some("redis"));
        let p = g.process.as_ref().unwrap();
        assert_eq!((p.handle.as_str(), p.status, p.signal), ("redis", 2, None));
        assert_eq!(p.at.as_ref().unwrap().point, "recovery:24019");
        assert!(p.command.as_deref().unwrap().contains("redis-server"));
        assert!(
            g.summary.contains("in the recovery boot") && g.summary.contains("exit status 2"),
            "{}",
            g.summary
        );
        assert_eq!(g.probe, None, "its readiness check passed");
        // a readiness check that never passed in the recovery boot, and no exit
        let d2 = cut_with_events(
            "notready",
            concat!(
                r#"{"conversation":"recovery","stop":23999,"at":841857,"line":{"ev":"probe","id":13,"ok":false,"why":"deadline","status":1,"tries":120,"v":1}}"#,
                "\n"
            ),
        );
        let g2 = host(&Recorded::load(&d2).unwrap()).unwrap();
        assert_eq!(g2.failure, "service-not-ready", "{}", g2.summary);
        assert_eq!(g2.probe.as_ref().unwrap().why, "deadline");
        assert_eq!(g2.service.as_deref(), Some("redis"));
        // every service up: the old words, now saying what was checked
        let d3 = cut_with_events(
            "fine",
            concat!(
                r#"{"conversation":"recovery","stop":23999,"at":841857,"line":{"ev":"probe","id":13,"ok":true,"why":"ok","status":0,"tries":1,"v":1}}"#,
                "\n"
            ),
        );
        let g3 = host(&Recorded::load(&d3).unwrap()).unwrap();
        assert_eq!(g3.failure, "violation");
        assert!(g3.summary.contains("No service exited"), "{}", g3.summary);
        for x in [d, d2, d3] {
            let _ = std::fs::remove_dir_all(x);
        }
    }
}
