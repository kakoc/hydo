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
/// `ERROR restart after the faults (S)`.
pub fn verdict_service(v: &str) -> Option<String> {
    let v = v.trim_start_matches("HYDO-ORACLE: ");
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
        let cmds = sends(rec, a.conv);
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
    let v = rec.doc.verdict.text.clone();
    let kind = rec.doc.verdict.kind.as_str();
    if kind == "ok" {
        return None;
    }
    let evs = agent_events(rec);
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
        let what = if core.contains("restart") {
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
        d.summary = format!(
            "a check failed: {core}. No process failed; look at the state behind it with hydo inspect"
        );
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

/// Read in the guest at the verdict's stop, by one replay. Fills `d.evidence` and `d.kernel`.
pub fn evidence(ctx: &Ctx, rec: &Recorded, d: &mut Diagnosis) {
    let Some(target) = rec.end() else {
        d.evidence_status = "not-collected".into();
        d.evidence_note = Some("the run has no recorded end".into());
        return;
    };
    let tmp = super::session::scratch(ctx, "diagnose");
    let r = (|| -> Result<Batch, Problem> {
        let m = super::session::machine(ctx, rec, &tmp)?;
        let mut works = plan_works(&m.plan, d);
        let mut b = Batch {
            works: std::mem::take(&mut works),
            i: 0,
            stops: 0,
        };
        m.reach(rec, target, &mut b, false, &tmp.join("engine.err"))?;
        Ok(b)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    match r {
        Err(p) => {
            d.evidence_status = "failed".into();
            d.evidence_note = Some(p.message);
        }
        Ok(b) => {
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
    if let (Some(s), "service-not-ready") = (&d.service, d.failure.as_str()) {
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
        evidence(ctx, &rec, &mut d);
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
    if evidence_on && rec.doc.verdict.kind == "error" {
        evidence(ctx, &rec, &mut d);
        d.next.retain(|n| !n.contains("--evidence"));
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
