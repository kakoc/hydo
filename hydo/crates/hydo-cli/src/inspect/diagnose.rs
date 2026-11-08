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
/// boot after a power cut) and `VIOLATION service cannot recover after the faults (S)`.
pub fn verdict_service(v: &str) -> Option<String> {
    let v = v.trim_start_matches("HYDO-ORACLE: ");
    if v.starts_with("VIOLATION service cannot recover") {
        let r = v.strip_suffix(')')?;
        let (_, s) = r.rsplit_once(" (")?;
        return (!s.is_empty() && !s.contains(' ')).then(|| s.to_string());
    }
    if let Some(r) = v.strip_prefix("ERROR service start (") {
        return r.split(')').next().map(str::to_string);
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
        } else if core.contains("restart") || core.contains("after the faults") {
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
        let mut works = plan_works(&m.plan, d);
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
                if let Some(e) = to_evidence(&what, &source, &w) {
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

/// The reads of the evidence: the failing service's log first, then every other service's, the
/// provisioning's and setup logs, the readiness check run once, the kernel's messages, memory
/// and processes.
fn plan_works(plan: &Plan, d: &Diagnosis) -> Vec<(String, String, Work)> {
    let l = &plan.layout;
    let mut v: Vec<(String, String, Work)> = Vec::new();
    let tail = |p: String| {
        Work::read(vec![FileReq {
            path: p,
            offset: TAIL,
            max: TAIL,
            tail: true,
        }])
    };
    let mut names: Vec<String> = plan.services.iter().map(|s| s.name.clone()).collect();
    if let Some(s) = &d.service {
        names.retain(|n| n != s);
        names.insert(0, s.clone());
    }
    for s in &names {
        let p = l.log_of(s);
        v.push((format!("the log of {s}"), p.clone(), tail(p)));
    }
    if let Some(s) = &d.service {
        let p = format!("{}/{s}.setup.log", l.dir);
        v.push((format!("the setup log of {s}"), p.clone(), tail(p)));
    }
    if d.failure == "provision" {
        let p = format!("{}/provision.log", l.dir);
        v.push(("the provisioning's log".into(), p.clone(), tail(p)));
    }
    let bb = l.busybox.as_str();
    let ex = |script: String, ns: Option<String>, k: u64| {
        Work::exec(bb, script, env(plan), ns, super::TIMEOUT_MS, 100 + k, TAIL)
    };
    if let (Some(s), "service-not-ready" | "other") = (&d.service, d.failure.as_str()) {
        if let Some(sv) = plan.service(s) {
            let ns = netns_of(plan, Some(s)).ok().flatten();
            v.push((
                format!("the readiness check of {s}, run once"),
                sv.ready.clone(),
                ex(format!(". {} 2>/dev/null; {}", l.prelude, sv.ready), ns, 1),
            ));
        }
    }
    let dmesg = "dmesg | grep -E 'Out of memory|oom-kill|Killed process|segfault|general protection|Kernel panic|BUG:|Oops|blocked for more than|page allocation failure' | tail -n 20; echo '--- the last lines'; dmesg | tail -n 15";
    v.push((
        "the kernel's messages".into(),
        dmesg.into(),
        ex(dmesg.into(), None, 2),
    ));
    v.push((
        "memory".into(),
        "free -m".into(),
        ex("free -m".into(), None, 3),
    ));
    v.push((
        "processes".into(),
        "ps -o pid,ppid,stat,rss,args".into(),
        ex(
            "ps -o pid,ppid,stat,rss,args 2>/dev/null || ps".into(),
            None,
            4,
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
                let last: Vec<&str> = e
                    .text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                let tail: Vec<&str> = last.iter().rev().take(3).rev().copied().collect();
                if !tail.is_empty() {
                    d.summary.push_str(&format!(
                        "; its log ends: {}",
                        super::timeline::diff_cut(&tail.join(" | "), 300)
                    ));
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
    }
}

/// `hydo inspect diagnose <run dir> [--evidence]`.
pub fn cmd(ctx: &Ctx, rest: &[String]) -> Result<Value, Problem> {
    let mut a = Args::new(rest);
    let ev = a.flag("--evidence");
    let dir = a.one("a run directory")?;
    a.done()?;
    let rec = Recorded::load(Path::new(&dir))?;
    let mut d = match host(&rec) {
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
        d = with_evidence(ctx, &rec, d).0;
        d.next.retain(|n| !n.contains("--evidence"));
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
                "HYDO-ORACLE: VIOLATION service cannot recover (committed data unrecoverable)"
            ),
            None
        );
        assert_eq!(verdict_service("HYDO-ORACLE: ERROR provision failed"), None);
        assert_eq!(verdict_service("HYDO-ORACLE: VIOLATION [x] y"), None);
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
