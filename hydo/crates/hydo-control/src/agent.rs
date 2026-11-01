//! Agent images on the engine interface (docs/design/guest-control.md step C1; M126).
//!
//! [`AgentEngine`] wraps an engine (hvx or hvm) for an image whose `/etc/hydo/control` says
//! `agent <proto>` and implements the same [`Engine`]/[`Worker`] traits, so the explorer's pool,
//! re-runs, shrinking and reports and the `hydo` command drive agent images without knowing:
//!
//! - [`Engine::start`] boots under control ([`Engine::start_controlled`]) with the boot
//!   conversation of [`ScheduleController::boot`] (the host takes S0 and S1), then serves;
//! - [`Worker::run`] runs a schedule from S1 with [`Worker::run_controlled`] and the run
//!   conversation (and after a power cut the recovery conversation, in the same engine run);
//! - [`Engine::one`] is the fresh-boot run (`--control boot one`).
//!
//! What a run leaves in the engine's output directory, next to the shell images' files:
//! `<tag>.log` and `<tag>.recovery.log` are the run's report (the lines the shell init prints on
//! the console: assertion records, the acknowledged ids, the verdict, and the `hydo-ob:` notes),
//! so every reader of a shell image's consoles reads an agent run the same way; the engine's own
//! consoles (the agent's framed lines) are kept as `<tag>.agent.log` and `<tag>.recovery.agent.log`;
//! the control transcripts are `<tag>.control.jsonl` and `<tag>.control.recovery.jsonl` (a
//! fresh-boot run: `control.boot.jsonl`, `control.jsonl`, `control.recovery.jsonl`). The RESULT
//! line gains `ctl=<hash16>` ([`crate::drive::Ran::ctl`]) before `verdict=`.
//!
//! The cadence: an agent image's run is under control from its first instruction, and the host
//! sees the guest only at stops, so the engine must stop often enough for the conversation (each
//! command waits for the next stop). A schedule without `rb=` runs at [`default_cadence`] (hvx
//! `rb=16`, hvm `rb=64`), and a worker boots at that cadence too, because a snapshot and its
//! continuations must use one cadence (M43, M118); another `rb=` is refused.

use crate::drive::{self, Booted, DriveError, Policy, Ran, RunFrom};
use crate::lockstep_link::LockstepLink;
use crate::plan::Plan;
use crate::runner::{Finished, RunError, RunOpts};
use crate::seams::Unsupported;
use crate::transcript::Transcript;
use hydo_engine::lockstep::{Driver, Lockstep, LockstepStart};
use hydo_engine::{
    BootSpec, BranchOutcome, BranchRequest, Engine, EngineError, EngineKind, OneRun, RunResult,
    Worker,
};
use hydo_schedule::{HostToken, Schedule};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// The cadence of an agent image's runs without `rb=`: hvx stops after at most 16 events
/// (M121's boot check), hvm every 64 exits.
pub fn default_cadence(kind: EngineKind) -> u64 {
    match kind {
        EngineKind::Hvx => 16,
        EngineKind::Hvm => 64,
    }
}

/// The schedule with the cadence (`rb=`) in its host part, and the engine's form of it (the host
/// part and the guest part `run`: the controller types the guest tokens' commands itself).
pub fn engine_schedule(s: &Schedule, cadence: u64) -> Result<(Schedule, Schedule), String> {
    let mut full = s.clone();
    match s.host_token("rb") {
        None => {
            let t = HostToken::parse(&format!("rb={cadence}")).map_err(|e| e.message)?;
            full.host.push(t);
        }
        Some(t) if t.to_string() == format!("rb={cadence}") => {}
        Some(t) => {
            return Err(format!(
                "an agent image runs at rb={cadence} (its worker boots at that cadence, and a \
                 snapshot and its continuations use one cadence); {t} is not supported yet"
            ))
        }
    }
    let eng = Schedule {
        host: full.host.clone(),
        guest: Vec::new(),
    };
    Ok((full, eng))
}

/// The end of an hvm verdict for a run that stopped in an exit-free loop the plain spin observer
/// cannot resolve (serve.c's `stop_name(STOP_SPIN)`, after "ERROR workload: " and the like).
const SPIN_VERDICT: &str = ": SPIN (guest loops without exits";

/// The schedule to run again in hvm's rewind mode (M130), or None: a run on hvm whose verdict is
/// a SPIN and whose schedule has no `spin=rewind`. hvmx re-runs a shell image's SPIN run in the
/// rewind mode itself (`HVM_SPIN_REWIND=auto`, M57), but under lock-step control it cannot: the
/// host's conversation has happened. So the host does it: the same schedule with `spin=rewind`,
/// a new conversation from S1 (or a new fresh boot), whose stops, sends and verdict are a pure
/// function of that schedule; the plain run is not reported, as with RUN. A counter loop that
/// only the rewind mode resolves (ledger-spin) then runs to its verdict. `HVM_SPIN_REWIND=0`
/// turns it off, as for hvmx.
pub fn rewind_again(kind: EngineKind, r: &RunResult, s: &Schedule) -> Option<Schedule> {
    if kind != EngineKind::Hvm
        || !r.verdict.starts_with("ERROR")
        || !r.verdict.contains(SPIN_VERDICT)
        || s.host_token("spin").is_some()
        || std::env::var("HVM_SPIN_REWIND").is_ok_and(|v| v == "0")
    {
        return None;
    }
    let mut again = s.clone();
    again.host.push(HostToken::SpinRewind);
    Some(again)
}

/// The core plan of an agent image's manifest (`/etc/hydo/hydo.yaml`); a manifest that needs a
/// wave-3 feature is refused with its seam.
pub fn plan_of_image(image: &Path) -> Result<Plan, Unsupported> {
    let text = hydo_engine::image::embedded_manifest(image)
        .map_err(|e| Unsupported::other(format!("{}: {e}", image.display())))?;
    let compose = hydo_engine::image::embedded_compose(image);
    // a manifest with params: is the image's only with the values it was built with (M128)
    let params = hydo_engine::image::embedded_file(image, "etc/hydo/params")
        .map(|t| hydo_manifest::parse_params_file(&t))
        .unwrap_or_default();
    let mut plan =
        plan_of_embedded_params(&text, compose.as_deref(), &params).map_err(|e| Unsupported {
            what: format!("{}: {}", image.display(), e.what),
            ..e
        })?;
    // The musl table of the image's busybox: the words of an unmountable data disk (M131).
    let bb = plan.layout.busybox.trim_start_matches('/').to_string();
    if let Ok(bytes) = hydo_engine::image::embedded_file(image, &bb) {
        plan.layout.strerror = crate::busybox::Strerror::of_busybox(bytes.as_bytes());
    }
    // The pinned tc, ip and iptables are image facts: Plan::from_manifest leaves hydo-build's
    // placeholders where the manifest's faults need a tool; the image's manifest.sh has the paths.
    let needed = [&plan.layout.tc, &plan.layout.ip, &plan.layout.iptables]
        .iter()
        .any(|p| !p.is_empty());
    if needed {
        let sh = hydo_engine::image::embedded_file(image, "etc/hydo/manifest.sh")
            .map_err(|e| Unsupported::other(format!("{}: {e}", image.display())))?;
        let l = &mut plan.layout;
        for (field, key, what) in [
            (&mut l.tc, "HYDO_TC", "tc"),
            (&mut l.ip, "HYDO_IP", "ip"),
            (&mut l.iptables, "HYDO_IPT", "iptables"),
        ] {
            if field.is_empty() {
                continue;
            }
            match shell_var(&sh, key).filter(|v| !v.is_empty()) {
                Some(v) => *field = v,
                None => {
                    return Err(Unsupported::other(format!(
                        "{}: the manifest's faults need {what}, and the image's manifest.sh has no {key}",
                        image.display()
                    )))
                }
            }
        }
    }
    Ok(plan)
}

/// The plan of an image's manifest text and its test composer plan (`/etc/hydo/compose.json`, the
/// meta hydo-build staged; M123): the manifest expanded as the explorer expands it, with that
/// meta as `_compose`, where the composer's controller reads its commands, checks and saved
/// compositions.
pub fn plan_of_embedded(text: &str, compose: Option<&str>) -> Result<Plan, Unsupported> {
    plan_of(text, compose, None)
}

/// [`plan_of_embedded`] with the image's parameter values (`/etc/hydo/params`): a manifest's
/// `params:` applied as hydo-build applied it, so `{{name}}` in a service's command or a step is
/// the built value (M128). Without `params:` in the manifest the same as [`plan_of_embedded`].
pub fn plan_of_embedded_params(
    text: &str,
    compose: Option<&str>,
    params: &[(String, String)],
) -> Result<Plan, Unsupported> {
    plan_of(text, compose, Some(params))
}

fn plan_of(
    text: &str,
    compose: Option<&str>,
    params: Option<&[(String, String)]>,
) -> Result<Plan, Unsupported> {
    let meta: Option<serde_json::Value> = match compose {
        None => None,
        Some(c) => Some(
            serde_json::from_str(c)
                .map_err(|e| Unsupported::other(format!("etc/hydo/compose.json: {e}")))?,
        ),
    };
    let m = match params {
        None => hydo_manifest::from_embedded(text, meta.as_ref()),
        Some(p) => hydo_manifest::from_embedded_params(text, meta.as_ref(), p),
    }
    .map_err(|e| Unsupported::other(e.message))?;
    match meta {
        None => Plan::from_manifest(&m),
        Some(meta) => {
            let mut d = m.data.clone();
            d["_compose"] = meta;
            Plan::from_manifest(&hydo_manifest::Manifest::from_validated(d))
        }
    }
}

/// The value of `KEY=value` (or `KEY='value'`, hydo-build's `shq`) in a manifest.sh text.
pub fn shell_var(sh: &str, key: &str) -> Option<String> {
    let v = sh
        .lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))?;
    Some(
        match v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
            // shq writes a ' inside as '\'' (close, escaped quote, reopen)
            Some(inner) => inner.replace("'\\''", "'"),
            None => v.to_string(),
        },
    )
}

/// A run's report as the shell init's console shows it: the report lines and the notes, in order.
pub fn report_text(t: &Transcript) -> String {
    use crate::transcript::Record;
    let mut s = String::new();
    for r in &t.records {
        if let Record::Report { line, .. } | Record::Note { line, .. } = r {
            s.push_str(line);
            s.push('\n');
        }
    }
    s
}

/// A run's console as the shell init's console shows it (M128): the report lines and the notes
/// of the transcript, with the guest's own console lines (`console`: what its programs print on
/// /dev/console, with the stop that reported each) put before the report lines of the stop that
/// saw them, since the controller writes those after it has seen that stop's lines. Readers of
/// records the guest's programs print themselves (the history checkers' `HYDO-HIST` lines, the
/// SDKs' `HYDO-ASSERT` records) find them in `<tag>.log` as they do on a shell image.
pub fn console_text(t: &Transcript, console: &[(u64, String)]) -> String {
    use crate::transcript::Record;
    let mut s = String::new();
    let mut c = console.iter().peekable();
    for r in &t.records {
        if let Record::Report { n, line, .. } | Record::Note { n, line, .. } = r {
            while let Some((_, l)) = c.next_if(|(cn, _)| cn <= n) {
                s.push_str(l);
                s.push('\n');
            }
            s.push_str(line);
            s.push('\n');
        }
    }
    for (_, l) in c {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// The RESULT line with `ctl=<hash>` added before `verdict=`.
pub fn with_ctl(r: &RunResult, ctl: &str) -> RunResult {
    let mut line = format!(
        "RESULT {} fp={} wfp={} secs={}",
        r.tag, r.fp, r.wfp, r.secs_text
    );
    for (k, v) in &r.extras {
        if k != "ctl" {
            line.push_str(&format!(" {k}={v}"));
        }
    }
    line.push_str(&format!(" ctl={ctl} verdict={}", r.verdict));
    RunResult::parse(&line).unwrap_or_else(|| r.clone())
}

/// The file names of a run's transcripts: `<prefix>control.jsonl` (the run),
/// `<prefix>control.recovery.jsonl`, `<prefix>control.boot.jsonl`.
pub fn transcript_files(prefix: &str) -> [(&'static str, String); 3] {
    [
        ("boot", format!("{prefix}control.boot.jsonl")),
        ("run", format!("{prefix}control.jsonl")),
        ("recovery", format!("{prefix}control.recovery.jsonl")),
    ]
}

/// The control summaries of the transcripts in `dir` (hydo-results `Run.control`), in the order
/// boot, run, recovery; `rel` is the directory's path relative to the result document.
pub fn controls_in(dir: &Path, prefix: &str, rel: &str) -> Vec<hydo_results::Control> {
    let mut out = Vec::new();
    for (_, f) in transcript_files(prefix) {
        if let Ok(text) = std::fs::read_to_string(dir.join(&f)) {
            if let Ok(t) = Transcript::parse(&text) {
                out.push(t.to_results(Some(&format!("{rel}{f}"))));
            }
        }
    }
    out
}

/// Write a run's files into `dir` (see the module documentation). `prefix` names the transcripts.
pub fn write_run_files(dir: &Path, tag: &str, prefix: &str, ran: &Ran, boot: Option<&Finished>) {
    for suffix in ["", ".recovery"] {
        let raw = dir.join(format!("{tag}{suffix}.log"));
        if raw.exists() {
            let _ = std::fs::rename(&raw, dir.join(format!("{tag}{suffix}.agent.log")));
        }
    }
    let _ = std::fs::write(
        dir.join(format!("{tag}.log")),
        console_text(&ran.run.transcript, &ran.run.console),
    );
    let [b, r, rc] = transcript_files(prefix);
    let _ = std::fs::write(dir.join(&r.1), ran.run.transcript.to_jsonl());
    if let Some(rec) = &ran.recovery {
        let _ = std::fs::write(
            dir.join(format!("{tag}.recovery.log")),
            console_text(&rec.transcript, &rec.console),
        );
        let _ = std::fs::write(dir.join(&rc.1), rec.transcript.to_jsonl());
    }
    if let Some(bt) = boot {
        let _ = std::fs::write(dir.join(&b.1), bt.transcript.to_jsonl());
    }
}

/// A RESULT for a run that did not happen (no VM ran).
fn refused(tag: &str, verdict: &str) -> RunResult {
    RunResult::parse(&format!(
        "RESULT {tag} fp=0 wfp=0 secs=0 verdict={}",
        verdict.replace('\n', " ")
    ))
    .expect("a RESULT line")
}

/// The verdict of a schedule the controller cannot drive.
pub fn unsupported_verdict(why: &str) -> String {
    format!("UNSUPPORTED on an agent image: {why}")
}

/// The driver of one run conversation over a worker's lock-step session.
struct RunDriver<'b> {
    booted: &'b Booted,
    schedule: &'b Schedule,
    policy: &'b Policy,
    from: RunFrom,
    ran: Option<Ran>,
}

/// End an engine run that the host cannot carry on (the engine waits at a stop) with an ERROR.
fn end_broken(ls: &mut Lockstep<'_>, e: &DriveError) -> Result<RunResult, EngineError> {
    if ls.finished() {
        return Err(EngineError::Protocol(format!("control: {e}")));
    }
    let v = format!("HYDO-ORACLE: ERROR control: {e}").replace('\n', " ");
    ls.end(Some(&v))
}

impl Driver for RunDriver<'_> {
    fn drive(&mut self, ls: &mut Lockstep<'_>) -> Result<RunResult, EngineError> {
        let r = {
            let mut link = LockstepLink::new(ls);
            drive::run(
                &mut link,
                self.booted,
                self.schedule,
                self.from,
                self.policy,
            )
            .map(|ran| (ran, link.result.take()))
        };
        match r {
            Ok((ran, result)) => {
                let result = result
                    .or_else(|| RunResult::parse(ran.result_line()))
                    .ok_or_else(|| EngineError::Protocol(ran.result_line().to_string()))?;
                self.ran = Some(ran);
                Ok(result)
            }
            Err(DriveError::Run(RunError::Link(e))) => Err(EngineError::Protocol(e.0)),
            Err(e) => end_broken(ls, &e),
        }
    }
}

/// An engine for one agent image (see the module documentation).
#[derive(Clone)]
pub struct AgentEngine {
    pub inner: Arc<dyn Engine + Send + Sync>,
    pub plan: Plan,
    pub cadence: u64,
}

impl AgentEngine {
    pub fn new(inner: Arc<dyn Engine + Send + Sync>, plan: Plan) -> AgentEngine {
        let cadence = default_cadence(inner.kind());
        AgentEngine {
            inner,
            plan,
            cadence,
        }
    }

    /// The engine for `image`: Some for an agent image (its plan read from the image), None for a
    /// shell-init image (run it on `inner` as before).
    pub fn for_image(
        inner: Arc<dyn Engine + Send + Sync>,
        image: &Path,
    ) -> Result<Option<AgentEngine>, String> {
        match hydo_engine::image::control(image).map_err(|e| e.to_string())? {
            hydo_engine::GuestControl::Shell => Ok(None),
            hydo_engine::GuestControl::Agent { proto } => {
                if !hydo_agent_proto::HOST_PROTOCOLS.contains(&proto) {
                    return Err(format!(
                        "{}: the image's agent speaks agent protocol {proto}; this hydo knows {:?}",
                        image.display(),
                        hydo_agent_proto::HOST_PROTOCOLS
                    ));
                }
                let plan = plan_of_image(image).map_err(|e| {
                    format!(
                        "{}: this agent image cannot be run yet: {e}",
                        image.display()
                    )
                })?;
                Ok(Some(AgentEngine::new(inner, plan)))
            }
        }
    }

    /// Whether the controller drives `schedule` on this image (the cadence and the seams); the
    /// refusal is a message for people.
    pub fn check(&self, schedule: &Schedule) -> Result<(Schedule, Schedule), String> {
        let (full, eng) = engine_schedule(schedule, self.cadence)?;
        drive::check(&self.plan, &full).map_err(|u| u.to_string())?;
        Ok((full, eng))
    }

    /// A fresh-boot run under `policy` (`hydo run`; `hydo repro --transcript`: a replay).
    /// Files go to `spec.outdir` with tag `one`, the transcripts as `control*.jsonl`.
    pub fn one_with(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        policy: &Policy,
    ) -> Result<OneRun, EngineError> {
        let r = self.one_once(spec, schedule, policy)?;
        let again = match (policy, &r.result) {
            (Policy::Schedule, Some(res)) => rewind_again(self.inner.kind(), res, schedule),
            _ => None,
        };
        match again {
            Some(s) => {
                let mut r2 = self.one_once(spec, &s, policy)?;
                r2.output = format!(
                    "{}hydo: the run ended as SPIN; ran it again in hvm's rewind mode ({s})\n{}",
                    r.output, r2.output
                );
                r2.wall += r.wall;
                Ok(r2)
            }
            None => Ok(r),
        }
    }

    fn one_once(
        &self,
        spec: &BootSpec,
        schedule: &Schedule,
        policy: &Policy,
    ) -> Result<OneRun, EngineError> {
        let t0 = Instant::now();
        let (full, eng) = self.check(schedule).map_err(EngineError::Unsupported)?;
        let _ = std::fs::create_dir_all(&spec.outdir);
        let errlog = spec.outdir.join("one.engine.err");
        let _ = std::fs::remove_file(&errlog);
        let mut cr = self.inner.one_controlled(spec, &eng, &errlog)?;
        let mut note = String::new();
        let got = {
            let mut ls = cr.session(None);
            let r = {
                let mut link = LockstepLink::new(&mut ls);
                drive::one(&mut link, &self.plan, &full, policy).map(|x| (x, link.result.take()))
            };
            match r {
                Ok(((b, ran), result)) => {
                    let result = result.or_else(|| RunResult::parse(ran.result_line()));
                    Ok((Some(b), Some(ran), result))
                }
                Err(DriveError::Boot(v, fin)) => {
                    note = format!("hydo: the agent image's boot did not reach S1: {v}\n");
                    let result = RunResult::parse(&fin.result);
                    let _ = std::fs::write(
                        spec.outdir.join("control.boot.jsonl"),
                        fin.transcript.to_jsonl(),
                    );
                    Ok((None, None, result))
                }
                Err(DriveError::Run(RunError::Link(e))) => Err(e.0),
                Err(e) => end_broken(&mut ls, &e)
                    .map(|r| (None, None, Some(r)))
                    .map_err(|x| x.to_string()),
            }
        };
        let code = cr.wait()?;
        let mut output = std::fs::read_to_string(&errlog).unwrap_or_default();
        output.push_str(&note);
        let result = match got {
            Ok((b, Some(ran), result)) => {
                write_run_files(&spec.outdir, "one", "", &ran, b.as_ref().map(|b| &b.boot));
                result.map(|r| with_ctl(&r, &ran.ctl()))
            }
            Ok((_, None, result)) => result,
            Err(e) => {
                output.push_str(&format!("hydo: the controlled run broke: {e}\n"));
                None
            }
        };
        if let Some(r) = &result {
            output.push_str(&r.line);
            output.push('\n');
        }
        Ok(OneRun {
            result,
            output,
            exit_code: code,
            wall: t0.elapsed(),
        })
    }
}

impl Engine for AgentEngine {
    fn kind(&self) -> EngineKind {
        self.inner.kind()
    }

    fn one(&self, spec: &BootSpec, schedule: &Schedule) -> Result<OneRun, EngineError> {
        self.one_with(spec, schedule, &Policy::Schedule)
    }

    fn start(&self, spec: &BootSpec, stderr_log: &Path) -> Result<Box<dyn Worker>, EngineError> {
        let boot_s = Schedule {
            host: vec![HostToken::parse(&format!("rb={}", self.cadence))
                .map_err(|e| EngineError::Unsupported(e.message))?],
            guest: Vec::new(),
        };
        let mut cb = self.inner.start_controlled(spec, &boot_s, stderr_log)?;
        let booted = {
            let mut ls = cb.session(None);
            let mut link = LockstepLink::new(&mut ls);
            drive::boot(&mut link, &self.plan, &boot_s, RunOpts::default())
        };
        let boot_file = stderr_log.with_extension("control.boot.jsonl");
        match booted {
            Ok(b) => {
                let _ = std::fs::write(&boot_file, b.boot.transcript.to_jsonl());
                let w = cb.ready()?;
                Ok(Box::new(AgentWorker {
                    inner: w,
                    kind: self.inner.kind(),
                    booted: b,
                    cadence: self.cadence,
                    outdir: spec.outdir.clone(),
                }))
            }
            Err(e) => {
                if let DriveError::Boot(_, fin) = &e {
                    let _ = std::fs::write(&boot_file, fin.transcript.to_jsonl());
                }
                if let Ok(w) = cb.ready() {
                    let _ = w.close();
                }
                Err(EngineError::NotReady(format!(
                    "the agent image's boot: {e}"
                )))
            }
        }
    }

    fn repro_line(&self, spec: &BootSpec, schedule: &Schedule) -> String {
        format!("hydo run --image {} \"{schedule}\"", spec.image.display())
    }

    fn fingerprint_scheme(&self) -> &'static str {
        self.inner.fingerprint_scheme()
    }

    fn max_workers(&self, spec: &BootSpec) -> usize {
        self.inner.max_workers(spec)
    }
}

/// A booted agent image (see the module documentation).
pub struct AgentWorker {
    inner: Box<dyn Worker>,
    kind: EngineKind,
    booted: Booted,
    cadence: u64,
    outdir: PathBuf,
}

impl AgentWorker {
    /// [`Worker::run`] under `policy` (a replay of recorded transcripts, or the schedule's).
    pub fn run_with(
        &mut self,
        tag: &str,
        schedule: &Schedule,
        policy: &Policy,
    ) -> Result<RunResult, EngineError> {
        let r = self.run_once(tag, schedule, policy)?;
        match (policy, rewind_again(self.kind, &r, schedule)) {
            (Policy::Schedule, Some(again)) => self.run_once(tag, &again, policy),
            _ => Ok(r),
        }
    }

    fn run_once(
        &mut self,
        tag: &str,
        schedule: &Schedule,
        policy: &Policy,
    ) -> Result<RunResult, EngineError> {
        let (full, eng) = match engine_schedule(schedule, self.cadence) {
            Ok(x) => x,
            Err(e) => return Ok(refused(tag, &unsupported_verdict(&e))),
        };
        if let Err(u) = drive::check(&self.booted.plan, &full) {
            return Ok(refused(tag, &unsupported_verdict(&u.to_string())));
        }
        let mut d = RunDriver {
            booted: &self.booted,
            schedule: &full,
            policy,
            from: RunFrom::Restored,
            ran: None,
        };
        let r = self.inner.run_controlled(tag, &eng, &mut d, None)?;
        Ok(match d.ran {
            Some(ran) => {
                let prefix = format!("{tag}.");
                write_run_files(&self.outdir, tag, &prefix, &ran, None);
                with_ctl(&r, &ran.ctl())
            }
            None => r,
        })
    }

    pub fn booted(&self) -> &Booted {
        &self.booted
    }
}

impl Worker for AgentWorker {
    fn boot_secs(&self) -> f64 {
        self.inner.boot_secs()
    }

    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError> {
        self.run_with(tag, schedule, &Policy::Schedule)
    }

    fn branch(&mut self, _req: &BranchRequest) -> Result<BranchOutcome, EngineError> {
        Err(EngineError::Unsupported(
            "BRANCH is not supported for an agent image yet (it runs under control: SNAP, RESUME)"
                .into(),
        ))
    }

    fn lockstep<'a>(
        &'a mut self,
        start: &LockstepStart,
        log: Option<&'a mut dyn std::io::Write>,
    ) -> Result<Lockstep<'a>, EngineError> {
        self.inner.lockstep(start, log)
    }

    fn close(self: Box<Self>) -> Result<(), EngineError> {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_sh_values() {
        let sh = "HYDO_X=1\nHYDO_TC='/nix/store/abc-iproute2/bin/tc'\nHYDO_IP=/bin/ip\nHYDO_Q='it'\\''s'\n";
        assert_eq!(
            shell_var(sh, "HYDO_TC").as_deref(),
            Some("/nix/store/abc-iproute2/bin/tc")
        );
        assert_eq!(shell_var(sh, "HYDO_IP").as_deref(), Some("/bin/ip"));
        assert_eq!(shell_var(sh, "HYDO_Q").as_deref(), Some("it's"));
        assert_eq!(shell_var(sh, "HYDO_IPT"), None);
        assert_eq!(shell_var(sh, "HYDO"), None);
    }

    #[test]
    fn cadence_and_ctl() {
        let s: Schedule = "disk=cut | run C=12".parse().unwrap();
        let (full, eng) = engine_schedule(&s, 16).unwrap();
        assert_eq!(full.to_string(), "disk=cut rb=16 | run C=12");
        assert_eq!(eng.to_string(), "disk=cut rb=16 | run");
        let s: Schedule = "rb=16 | run".parse().unwrap();
        assert_eq!(engine_schedule(&s, 16).unwrap().0, s);
        let s: Schedule = "rb=1 | run".parse().unwrap();
        assert!(engine_schedule(&s, 16).is_err());
        let r = RunResult::parse(
            "RESULT a fp=1 wfp=2 secs=0.5 stops=3 typed=1 inh=00aa verdict=HYDO-ORACLE: OK x",
        )
        .unwrap();
        let c = with_ctl(&r, "0123456789abcdef");
        assert_eq!(
            c.line,
            "RESULT a fp=1 wfp=2 secs=0.5 stops=3 typed=1 inh=00aa ctl=0123456789abcdef verdict=HYDO-ORACLE: OK x"
        );
        assert_eq!(c.extra("ctl"), Some("0123456789abcdef"));
    }

    /// M130: on hvm a run that ended as SPIN is run again with `spin=rewind` (hvmx cannot re-run a
    /// controlled run itself); nothing else is: another engine, another verdict, a schedule
    /// already in the rewind mode.
    #[test]
    fn a_spin_on_hvm_runs_again_in_rewind_mode() {
        let s: Schedule = "disk=none | run P=front@10+2".parse().unwrap();
        let spin = RunResult::parse(
            "RESULT one fp=0000000000000000 wfp=0000000000000000 secs=0.0 stops=208 typed=19 \
             inh=855549cd692f7247 verdict=ERROR workload: SPIN (guest loops without exits; run invalid)",
        )
        .unwrap();
        let again = rewind_again(EngineKind::Hvm, &spin, &s).expect("a SPIN on hvm");
        assert_eq!(
            again.to_string(),
            "disk=none spin=rewind | run P=front@10+2"
        );
        // the engine's form keeps the token (the cadence after it), the controller drives it
        let (full, eng) = engine_schedule(&again, 64).unwrap();
        assert_eq!(
            full.to_string(),
            "disk=none spin=rewind rb=64 | run P=front@10+2"
        );
        assert_eq!(eng.to_string(), "disk=none spin=rewind rb=64 | run");
        for phase in ["check", "recovery"] {
            let r = RunResult::parse(&format!(
                "RESULT a fp=1 wfp=2 secs=0 verdict=ERROR {phase}: SPIN (guest loops without exits; run invalid)"
            ))
            .unwrap();
            assert!(rewind_again(EngineKind::Hvm, &r, &s).is_some(), "{phase}");
        }
        assert!(rewind_again(EngineKind::Hvx, &spin, &s).is_none());
        assert!(rewind_again(EngineKind::Hvm, &spin, &again).is_none());
        for v in [
            "HYDO-ORACLE: OK invariants=2 held",
            "HYDO-ORACLE: VIOLATION [x] out: SPIN (guest loops without exits",
            "ERROR workload: exit budget exhausted",
        ] {
            let r = RunResult::parse(&format!("RESULT a fp=1 wfp=2 secs=0 verdict={v}")).unwrap();
            assert!(rewind_again(EngineKind::Hvm, &r, &s).is_none(), "{v}");
        }
    }

    /// M128: an agent image's plan has the parameter values the image was built with (its
    /// `/etc/hydo/params`), as the shell image's compiled manifest.sh has; before, the gRPC demo's
    /// `/bin/orderd serve --mode {{mode}}` was started literally and exited.
    #[test]
    fn params_are_the_images() {
        let text = "name: p\nparams:\n  mode: buggy\nservices:\n  - name: s\n    cmd: /bin/s --mode {{mode}}\n    ready: /bin/true\nworkload:\n  steps: 2\n  run: echo {{mode}}\ninvariants:\n  - id: i\n    cmd: /bin/true\n";
        let built = hydo_manifest::parse_params_file("mode=fixed\n");
        let p = plan_of_embedded_params(text, None, &built).unwrap();
        assert_eq!(p.services[0].cmd, "/bin/s --mode fixed");
        assert_eq!(p.workload_run.trim(), "echo fixed");
        // the defaults when the image says nothing (hydo-build substitutes them too)
        let p = plan_of_embedded_params(text, None, &[]).unwrap();
        assert_eq!(p.services[0].cmd, "/bin/s --mode buggy");
        // without params: the reading is plan_of_embedded's
        let plain = text.replace("params:\n  mode: buggy\n", "");
        assert_eq!(
            plan_of_embedded_params(&plain, None, &built).unwrap(),
            plan_of_embedded(&plain, None).unwrap()
        );
    }

    /// M128: the HYDO-HIST records a workload prints on /dev/console reach `<tag>.log`, before
    /// the report lines of the stop that saw them (on a shell image the history checkers read
    /// them from the console; on the agent image they were only in `<tag>.agent.log`).
    #[test]
    fn console_text_keeps_the_guests_own_lines() {
        use crate::transcript::{Header, Record};
        let mut t = Transcript::new(Header {
            controller: "schedule".into(),
            conversation: "run".into(),
            schedule: None,
            cadence: "rb=64".into(),
            start: "resume".into(),
        });
        let rep = |n: u64, line: &str| Record::Report {
            n,
            by: "schedule".into(),
            line: line.into(),
        };
        t.records.push(rep(3, "hydo-ob: command: run"));
        t.records.push(Record::Note {
            n: 3,
            by: "schedule".into(),
            line: "note".into(),
        });
        t.records.push(rep(9, "hydo-ob: workload steps 0..2 rc=0"));
        t.records.push(rep(12, "HYDO-ORACLE: OK"));
        let console = vec![
            (5, "HYDO-HIST {\"p\":0}".to_string()),
            (9, "HYDO-HIST {\"p\":1}".to_string()),
            (14, "late line".to_string()),
        ];
        assert_eq!(
            console_text(&t, &console),
            "hydo-ob: command: run\nnote\nHYDO-HIST {\"p\":0}\nHYDO-HIST {\"p\":1}\n\
             hydo-ob: workload steps 0..2 rc=0\nHYDO-ORACLE: OK\nlate line\n"
        );
        assert_eq!(console_text(&t, &[]), report_text(&t));
    }
}
