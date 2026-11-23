//! Agent images on the engine interface (docs/design/guest-control.md step C1; M126).
//!
//! [`AgentEngine`] wraps an engine (hvx or hvm) for an image whose `/etc/hydo/control` says
//! `agent <proto>` and implements the same [`Engine`]/[`Worker`] traits, so the explorer's pool,
//! re-runs, shrinking and reports and the `hydo` command drive agent images without knowing:
//!
//! - [`Engine::start`] boots under control ([`ControlledEngine::start_controlled`]) with the boot
//!   conversation of `ScheduleController::boot` (the host takes S0 and S1), then serves;
//! - [`Worker::run`] runs a schedule from S1 with [`ControlledWorker::run_controlled`] and the run
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
use hydo_engine::lockstep::{Driver, Lockstep};
use hydo_engine::{
    BootSpec, ControlledEngine, ControlledWorker, Engine, EngineError, EngineKind, OneRun,
    RunResult, Worker,
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
        EngineKind::Kvm => 64,
    }
}

/// The schedule with the cadence (`rb=`) in its host part, and the engine's form of it (the host
/// part and the guest part `run`: the controller types the guest tokens' commands itself).
pub fn engine_schedule(s: &Schedule, cadence: u64) -> Result<(Schedule, Schedule), String> {
    let mut full = s.clone();
    match s.host_token("rb") {
        None => {
            // before any branch point (`br=`): rb= is the run's own token
            let at = full.main_host().len();
            full.host.insert(at, HostToken::Rb(cadence))
        }
        Some(HostToken::Rb(n)) if *n == cadence => {}
        Some(t) => {
            return Err(format!(
                "an agent image runs at rb={cadence} (its worker boots at that cadence, and a \
                 snapshot and its continuations use one cadence); {t} is not supported yet"
            ))
        }
    }
    // the branch points (`br=`) are the host controller's: the runner sends TIME at each
    let eng = Schedule {
        host: full.main_host().to_vec(),
        guest: Vec::new(),
    };
    Ok((full, eng))
}

/// The end of an hvm verdict for a run that stopped in an exit-free loop the plain spin observer
/// cannot resolve (serve.c's `stop_name(STOP_SPIN)`, after "ERROR workload: " and the like).
const SPIN_VERDICT: &str = ": SPIN (guest loops without exits";

/// The schedule to run again in the engine's rewind mode (M130; the KVM engine's since M161), or
/// None: a run on hvm or kvm whose verdict is a SPIN and whose schedule has no `spin=rewind`. hvmx re-runs a shell image's SPIN run in the
/// rewind mode itself (`HVM_SPIN_REWIND=auto`, M57), but under lock-step control it cannot: the
/// host's conversation has happened. So the host does it: the same schedule with `spin=rewind`,
/// a new conversation from S1 (or a new fresh boot), whose stops, sends and verdict are a pure
/// function of that schedule; the plain run is not reported, as with RUN. A counter loop that
/// only the rewind mode resolves (ledger-spin) then runs to its verdict. The KVM engine ends a
/// run at once as a SPIN when it proves a counter loop outside the rewind mode, so its runs take
/// the same way (M161). `HVM_SPIN_REWIND=0` turns it off for both engines, as for hvmx
/// (`enabled`: `hydo_config::run::Control::spin_rewind`).
pub fn rewind_again(
    kind: EngineKind,
    r: &RunResult,
    s: &Schedule,
    enabled: bool,
) -> Option<Schedule> {
    if !enabled
        || !matches!(kind, EngineKind::Hvm | EngineKind::Kvm)
        || !r.verdict.starts_with("ERROR")
        || !r.verdict.contains(SPIN_VERDICT)
        || s.host_token("spin").is_some()
    {
        return None;
    }
    let mut again = s.clone();
    again.host.push(HostToken::SpinRewind);
    Some(again)
}

/// The `guest:` section of the manifest an image carries (`guest.ram_mib`, `guest.cpu`,
/// `guest.cpu_cache`), read as the host controller reads the manifest: with the test composer's
/// plan meta (`/etc/hydo/compose.json`) and the image's parameter values. Every path that boots
/// an image takes its engine options from here or from [`guest_of_embedded`], so that a worker
/// pool and a fresh boot boot the same guest (M207: hydo-bench read a composed plan's manifest
/// without its meta, the reading failed, and its pools booted without the CPUID cache leaves).
/// An image without a manifest gives the defaults; a manifest that cannot be read is an error.
pub fn image_guest(image: &Path) -> Result<hydo_manifest::Guest, String> {
    let Ok(text) = hydo_engine::image::embedded_manifest(image) else {
        return Ok(hydo_manifest::Guest::from_value(&serde_json::Value::Null));
    };
    let compose = hydo_engine::image::embedded_compose(image);
    let params = hydo_engine::image::embedded_file(image, "etc/hydo/params")
        .map(|t| hydo_manifest::parse_params_file(&t))
        .unwrap_or_default();
    guest_of_embedded(&text, compose.as_deref(), &params)
        .map_err(|e| format!("{}: /etc/hydo/hydo.yaml: {e}", image.display()))
}

/// [`image_guest`] from the image's files: the manifest text, its plan meta and its parameter
/// values.
pub fn guest_of_embedded(
    text: &str,
    compose: Option<&str>,
    params: &[(String, String)],
) -> Result<hydo_manifest::Guest, String> {
    let meta: Option<serde_json::Value> = match compose {
        None => None,
        Some(c) => {
            Some(serde_json::from_str(c).map_err(|e| format!("etc/hydo/compose.json: {e}"))?)
        }
    };
    let m =
        hydo_manifest::from_embedded_params(text, meta.as_ref(), params).map_err(|e| e.message)?;
    Ok(hydo_manifest::Guest::from_value(&m.data))
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
    // From the fingerprint epoch 4 (M269) the compositions that a `W=@<key>` names are beside the
    // image, with the saved regressions, not in its /etc/hydo/compose.saved/.
    if let Some(cp) = plan.compose.as_mut() {
        if !hydo_engine::image::image_defaults(image)
            .1
            .regressions_in_image
        {
            let regs = hydo_image::read_regressions(image)
                .map_err(Unsupported::other)?
                .unwrap_or_default();
            cp.saved = crate::schedule::composer::saved_of(&regs);
        }
    }
    // Milestone 334: the filmstrip and the traces of a walk come from the image's own walker; an
    // image built before this milestone has the walker before them, and its runs copy what they
    // copied (its fingerprint epoch cannot say: an epoch-5 image may have either walker)
    if plan.walk {
        plan.film =
            hydo_engine::image::embedded_file(image, hydo_manifest::browser::FILM_MARK).is_ok();
    }
    // Milestone 283: without `guest.cgroup2` the image's epoch decides (the mount from epoch 4)
    if plan.cgroup2.is_none() {
        plan.cgroup2 = Some(hydo_engine::image::image_defaults(image).1.cgroup2);
    }
    // Milestone 338: from epoch 5 a timed pause of threads finds the workload as a reaction does
    plan.pause_largest_session = hydo_engine::image::image_defaults(image)
        .1
        .pause_largest_session;
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
    // A hydo/ folder plan's history checks (test.checks of kind linearizable / list_append) come
    // from its checks/ files, which hydo-build expanded into the manifest it compiled; the
    // embedded hydo.yaml does not carry them, so the image's manifest.sh (HYDO_CHECK_c<i>_*) is
    // the record of what the shell image's init declares and reads back (M127: on the composed
    // Redis history example the agent image printed no HYDO-CHECK line and ran no `final:`
    // read-back, so every history check was skipped).
    if plan.history.is_empty() {
        if let Ok(sh) = hydo_engine::image::embedded_file(image, "etc/hydo/manifest.sh") {
            plan.history = history_of_manifest_sh(&sh);
        }
    } else if plan
        .history
        .iter()
        .any(|c| c.json.contains("\"type\":\"messages\""))
    {
        // a messages check's include: bundles are not in the image: the compiled manifest has
        // the declaration hydo build resolved (docs/design/messaging.md "Bundles")
        if let Ok(sh) = hydo_engine::image::embedded_file(image, "etc/hydo/manifest.sh") {
            let built = history_of_manifest_sh(&sh);
            for c in plan.history.iter_mut() {
                if let Some(b) = built.iter().find(|b| b.id == c.id) {
                    c.json = b.json.clone();
                }
            }
        }
    }
    Ok(plan)
}

/// An image's saved regressions as the host replays them ([`regressions_of_image`]).
#[derive(Clone, Debug)]
pub struct ImageRegressions {
    /// The plan meta the replay checks each regression against (`/etc/hydo/compose.json`, or an
    /// empty object for an image without a plan), with `regressions` set to the entries.
    pub meta: serde_json::Value,
    /// The entries, in their order.
    pub regressions: Vec<serde_json::Value>,
    /// Where they are: `None` inside the image (built before the fingerprint epoch 4), else the
    /// file beside it (`<image>.regressions.json`, Milestone 269), which may be missing.
    pub file: Option<std::path::PathBuf>,
}

/// The saved regressions of `image`, by the epoch it was built at: before epoch 4 the image's own
/// `/etc/hydo/compose.json` carries them; from epoch 4 (Milestone 269) `hydo build` writes them
/// beside the image, so editing one does not change the image's bytes.
pub fn regressions_of_image(image: &Path) -> Result<ImageRegressions, String> {
    let mut meta = hydo_engine::image::embedded_compose(image)
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let in_image = hydo_engine::image::image_defaults(image)
        .1
        .regressions_in_image;
    let (regressions, file) = if in_image {
        let r = meta
            .get("regressions")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        (r, None)
    } else {
        let r = hydo_image::read_regressions(image)?.unwrap_or_default();
        (r, Some(hydo_image::regressions_path(image)))
    };
    meta["regressions"] = serde_json::Value::Array(regressions.clone());
    Ok(ImageRegressions {
        meta,
        regressions,
        file,
    })
}

/// The history checks a manifest.sh carries (hydo-build's compile_checks: `HYDO_CHECKS` names
/// them, `HYDO_CHECK_<k>_id`, `_json` and `_final` describe each), in their order.
pub fn history_of_manifest_sh(sh: &str) -> Vec<crate::schedule::history::HistCheck> {
    let names = shell_var(sh, "HYDO_CHECKS").unwrap_or_default();
    names
        .split_whitespace()
        .filter_map(|k| {
            Some(crate::schedule::history::HistCheck {
                id: shell_var(sh, &format!("HYDO_CHECK_{k}_id"))?,
                json: shell_var(sh, &format!("HYDO_CHECK_{k}_json"))?,
                final_: shell_var(sh, &format!("HYDO_CHECK_{k}_final")).unwrap_or_default(),
            })
        })
        .collect()
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

/// The RESULT line of a run's conversations: `ctl=<hash>` added before `verdict=`, and the
/// controller's verdict in place of the engine's when the engine ended the run by itself
/// (Milestone 328, [`crate::runner::Finished::engine_verdict`]).
pub fn run_result(r: &RunResult, ran: &crate::drive::Ran) -> RunResult {
    let c = with_ctl(r, &ran.ctl());
    match ran.engine_verdict() {
        Some(v) => {
            let line = match c.line.split_once(" verdict=") {
                Some((head, _)) => format!("{head} verdict={}", v.replace('\n', " ")),
                None => return c,
            };
            RunResult::parse(&line).unwrap_or(c)
        }
        None => c,
    }
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
    // `browser:` (Milestone 254): the screenshots go to `<prefix>screens/`, and their notes stay
    // in the console and the transcript file without the bytes
    let run_t = take_screens(&ran.run.transcript, &dir.join(format!("{prefix}screens")));
    // Milestone 263: the boots after reboots the guest asked for add their reports to the run's
    // (or the recovery's) report, and each has its own transcript `control.reboot<n>.jsonl`
    let more = |rec: bool| -> String {
        ran.reboots
            .iter()
            .filter(|(r, _)| *r == rec)
            .map(|(_, f)| console_text(&f.transcript, &f.console))
            .collect()
    };
    let _ = std::fs::write(
        dir.join(format!("{tag}.log")),
        console_text(&run_t, &ran.run.console) + &more(false),
    );
    let [b, r, rc] = transcript_files(prefix);
    let _ = std::fs::write(dir.join(&r.1), run_t.to_jsonl());
    if let Some(rec) = &ran.recovery {
        // Milestone 334: the recovery boot's files (the read-back check's pages) join the first
        // boot's, whose filmstrip the controller copied out before the power cut
        let rec_t = take_screens(&rec.transcript, &dir.join(format!("{prefix}screens")));
        let _ = std::fs::write(
            dir.join(format!("{tag}.recovery.log")),
            console_text(&rec_t, &rec.console) + &more(true),
        );
        let _ = std::fs::write(dir.join(&rc.1), rec_t.to_jsonl());
    }
    for (i, (_, f)) in ran.reboots.iter().enumerate() {
        let _ = std::fs::write(
            dir.join(format!("{prefix}control.reboot{}.jsonl", i + 1)),
            f.transcript.to_jsonl(),
        );
    }
    if let Some(bt) = boot {
        let _ = std::fs::write(dir.join(&b.1), bt.transcript.to_jsonl());
    }
}

/// The screenshot notes of a transcript (`HYDO-SCREEN`, schedule/screens.rs) written as files
/// into `out`, with `index.jsonl` (one line per file, without its bytes; appended to, so that a
/// recovery boot's files join the first boot's); the transcript comes back with each such note
/// cut to its short line. A transcript without such notes comes back as it is, and `out` is not
/// made.
pub fn take_screens(t: &Transcript, out: &Path) -> Transcript {
    use crate::transcript::Record;
    let mut t = t.clone();
    let mut index = String::new();
    for r in t.records.iter_mut() {
        if let Record::Note { line, .. } = r {
            if let Some((file, bytes, short)) = crate::schedule::screens::of_note(line) {
                let _ = std::fs::create_dir_all(out);
                if file == crate::schedule::screens::FILM {
                    // a recovery boot's filmstrip (the read-back check's pages) follows the
                    // first boot's, which the controller copied at the power cut
                    let mut all = std::fs::read(out.join(&file)).unwrap_or_default();
                    all.extend_from_slice(&bytes);
                    let _ = std::fs::write(out.join(&file), all);
                } else {
                    let _ = std::fs::write(out.join(&file), bytes);
                }
                index.push_str(&short.to_string());
                index.push('\n');
                *line = format!("{} {short}", crate::schedule::screens::NOTE);
            }
        }
    }
    if !index.is_empty() {
        let path = out.join("index.jsonl");
        let mut all = std::fs::read_to_string(&path).unwrap_or_default();
        for l in index.lines() {
            if !all.lines().any(|x| x == l) {
                all.push_str(l);
                all.push('\n');
            }
        }
        let _ = std::fs::write(&path, all);
    }
    t
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
    pub inner: Arc<dyn ControlledEngine + Send + Sync>,
    pub plan: Plan,
    pub cadence: u64,
    /// A SPIN on hvm or kvm runs again in the rewind mode (`hydo_config::run::Control::spin_rewind`,
    /// default on; `HVM_SPIN_REWIND=0` turns it off).
    pub spin_rewind: bool,
}

impl AgentEngine {
    pub fn new(inner: Arc<dyn ControlledEngine + Send + Sync>, plan: Plan) -> AgentEngine {
        let cadence = default_cadence(inner.kind());
        AgentEngine {
            inner,
            plan,
            cadence,
            spin_rewind: true,
        }
    }

    /// With the host controller's configuration (M144).
    pub fn with_control(mut self, c: &hydo_config::run::Control) -> AgentEngine {
        self.spin_rewind = c.spin_rewind;
        self.plan.cost = c.cost;
        self
    }

    /// The engine for `image`, its plan read from the image. A shell-init image (built before
    /// Milestone 136) is refused ([`hydo_engine::image::control`]), and so is an agent image whose
    /// protocol or plan this hydo cannot drive.
    pub fn for_image(
        inner: Arc<dyn ControlledEngine + Send + Sync>,
        image: &Path,
    ) -> Result<AgentEngine, String> {
        let proto = hydo_engine::image::control(image).map_err(|e| e.to_string())?;
        if !hydo_agent_proto::HOST_PROTOCOLS.contains(&proto) {
            return Err(format!(
                "{}: the image's agent speaks agent protocol {proto}; this hydo knows {:?}",
                image.display(),
                hydo_agent_proto::HOST_PROTOCOLS
            ));
        }
        let mut plan = plan_of_image(image).map_err(|e| {
            format!(
                "{}: this agent image cannot be run yet: {e}",
                image.display()
            )
        })?;
        // Milestone 328: what a root that does not fit into guest RAM is reported with
        let ram = image_guest(image).map(|g| g.ram_mib).unwrap_or(1024);
        plan.root_fit = crate::plan::RootFit::of_image(image, ram as u64);
        Ok(AgentEngine::new(inner, plan))
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
            (Policy::Schedule, Some(res)) => {
                rewind_again(self.inner.kind(), res, schedule, self.spin_rewind)
            }
            _ => None,
        };
        match again {
            Some(s) => {
                let mut r2 = self.one_once(spec, &s, policy)?;
                r2.output = format!(
                    "{}hydo: the run ended as SPIN; ran it again in the engine's rewind mode ({s})\n{}",
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
                result.map(|r| run_result(&r, &ran))
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
            host: vec![HostToken::Rb(self.cadence)],
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
                    spin_rewind: self.spin_rewind,
                    recs: std::collections::HashMap::new(),
                    prefix: crate::prefix::Store::new(crate::prefix::keep(self.inner.kind())),
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

    fn worker_memory(&self, spec: &BootSpec) -> hydo_engine::WorkerMemory {
        self.inner.worker_memory(spec)
    }

    fn max_workers(&self, spec: &BootSpec) -> usize {
        self.inner.max_workers(spec)
    }
}

/// A booted agent image (see the module documentation).
pub struct AgentWorker {
    inner: Box<dyn ControlledWorker>,
    kind: EngineKind,
    booted: Booted,
    cadence: u64,
    outdir: PathBuf,
    spin_rewind: bool,
    /// The state search's recordings: each snapshot's parent stops up to the snapshot
    /// (docs/design/state-search.md).
    recs: std::collections::HashMap<String, Arc<Vec<crate::search::RecStop>>>,
    /// The snapshots of shrinks' runs (Milestone 340, [`crate::prefix`]).
    prefix: crate::prefix::Store,
}

/// The driver of a state search's run: a recording run from S1, or a branch from a snapshot.
struct SearchDriver<'b> {
    booted: &'b Booted,
    schedule: &'b Schedule,
    policy: crate::search::SnapPolicy,
    parent: Option<&'b [crate::search::RecStop]>,
    ran: Option<Ran>,
    stops: Vec<crate::search::RecStop>,
    features: Vec<String>,
    taken: Vec<crate::search::Taken>,
    snap_secs: f64,
}

impl Driver for SearchDriver<'_> {
    fn drive(&mut self, ls: &mut Lockstep<'_>) -> Result<RunResult, EngineError> {
        use crate::search::{BranchLink, RecLink};
        let r = {
            let mut link = LockstepLink::new(ls);
            let r = {
                let mut br;
                let inner: &mut dyn crate::runner::Link = match self.parent {
                    Some(p) => {
                        br = BranchLink::new(p, Some(&mut link));
                        &mut br
                    }
                    None => &mut link,
                };
                let mut rec = RecLink::new(inner, self.policy.clone());
                let r = drive::run(
                    &mut rec,
                    self.booted,
                    self.schedule,
                    RunFrom::Restored,
                    &Policy::Schedule,
                );
                self.stops = std::mem::take(&mut rec.stops);
                self.features = std::mem::take(&mut rec.features);
                self.taken = std::mem::take(&mut rec.taken);
                self.snap_secs = rec.snap_secs;
                r
            };
            r.map(|ran| (ran, link.result.take()))
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

/// Whether a branch's schedule sends its parent's commands up to the parent's snapshot: its
/// controller over the parent's recorded stops, without a VM (docs/design/state-search.md).
pub fn check_branch(
    booted: &Booted,
    schedule: &Schedule,
    parent: &[crate::search::RecStop],
) -> Result<(), String> {
    let mut br = crate::search::BranchLink::new(parent, None);
    match drive::run(
        &mut br,
        booted,
        schedule,
        RunFrom::Restored,
        &Policy::Schedule,
    ) {
        Err(DriveError::Run(RunError::Link(e))) if e.0 == crate::search::REACHED => Ok(()),
        Err(e) => Err(e.to_string()),
        Ok(_) => Err("state search: the branch's run ended before its snapshot".into()),
    }
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
        match (
            policy,
            rewind_again(self.kind, &r, schedule, self.spin_rewind),
        ) {
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
                run_result(&r, &ran)
            }
            None => r,
        })
    }

    pub fn booted(&self) -> &Booted {
        &self.booted
    }

    /// [`Worker::run_search`]: see docs/design/state-search.md.
    fn search_once(
        &mut self,
        tag: &str,
        schedule: &Schedule,
        req: &hydo_engine::SearchReq,
    ) -> Result<(RunResult, hydo_engine::SearchRun), EngineError> {
        use hydo_engine::{SearchRun, SearchSnap};
        let (full, eng) = match engine_schedule(schedule, self.cadence) {
            Ok(x) => x,
            Err(e) => return Ok((refused(tag, &unsupported_verdict(&e)), SearchRun::default())),
        };
        if let Err(u) = drive::check(&self.booted.plan, &full) {
            return Ok((
                refused(tag, &unsupported_verdict(&u.to_string())),
                SearchRun::default(),
            ));
        }
        let parent = match &req.branch_from {
            None => None,
            Some(name) => match self.recs.get(name) {
                Some(r) => Some(r.clone()),
                None => {
                    let why = format!("state search: no recording of snapshot {name}");
                    return Ok((
                        refused(tag, &why),
                        SearchRun {
                            invalid: Some(why),
                            ..SearchRun::default()
                        },
                    ));
                }
            },
        };
        if let Some(p) = &parent {
            if let Err(why) = check_branch(&self.booted, &full, p) {
                return Ok((
                    refused(tag, &format!("state search: {why}")),
                    SearchRun {
                        invalid: Some(why),
                        ..SearchRun::default()
                    },
                ));
            }
        }
        let skip = parent.as_ref().map(|p| p.len() - 1).unwrap_or(0);
        // the names this run may take are free again: their old recordings go
        for n in &req.snap_names {
            self.recs.remove(n);
        }
        let mut d = SearchDriver {
            booted: &self.booted,
            schedule: &full,
            policy: crate::search::SnapPolicy {
                names: req.snap_names.clone(),
                counts: req.counts.clone(),
                rare: req.rare,
                any_new: req.any_new,
                skip,
                spread: req.spread.clone(),
                ranges: false,
                spacing: 0,
            },
            parent: parent.as_deref().map(|v| v.as_slice()),
            ran: None,
            stops: Vec::new(),
            features: Vec::new(),
            taken: Vec::new(),
            snap_secs: 0.0,
        };
        let r = match &req.branch_from {
            Some(name) => self.inner.resume(tag, name, &mut d, None)?,
            None => self.inner.run_controlled(tag, &eng, &mut d, None)?,
        };
        let (stops, features, taken, ran) = (d.stops, d.features, d.taken, d.ran);
        let (clock, clock0) = crate::search::clock(&stops, skip);
        let mut out = SearchRun {
            features,
            clock,
            clock0,
            ..SearchRun::default()
        };
        for t in &taken {
            self.recs
                .insert(t.name.clone(), Arc::new(stops[..=t.index].to_vec()));
            out.snaps.push(SearchSnap {
                name: t.name.clone(),
                stop: t.stop.n,
                at: t.stop.at,
                new: t.new.clone(),
            });
        }
        Ok(match ran {
            Some(ran) => {
                out.ranges = crate::search::ranges(&ran.run.transcript);
                let prefix = format!("{tag}.");
                write_run_files(&self.outdir, tag, &prefix, &ran, None);
                (run_result(&r, &ran), out)
            }
            None => (r, out),
        })
    }
}

/// The files a run left under `<tag>.` in `dir` (the engine's own console, `.agent.log`, left
/// out: a run continued from a snapshot has only the part after it), by name.
fn tag_files(dir: &Path, tag: &str) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let pre = format!("{tag}.");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&pre) && !n.ends_with(".agent.log") && !n.ends_with(".err") {
                if let Ok(b) = std::fs::read(e.path()) {
                    out.insert(n, b);
                }
            }
        }
    }
    out
}

/// What differs between a run that took or used snapshots and the same schedule's run from S1
/// (`--check-continued`): the RESULT line but for its wall seconds, and the run's files; None
/// when nothing does.
pub fn prefix_differs(
    a: &RunResult,
    b: &RunResult,
    fa: &std::collections::BTreeMap<String, Vec<u8>>,
    fb: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Option<String> {
    let strip = |r: &RunResult| -> String {
        r.line
            .split(' ')
            .filter(|w| !w.starts_with("secs="))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (la, lb) = (strip(a), strip(b));
    if la != lb {
        return Some(format!("RESULT {la:?} against {lb:?} from S1"));
    }
    let names: std::collections::BTreeSet<&String> = fa.keys().chain(fb.keys()).collect();
    let bad: Vec<String> = names
        .into_iter()
        .filter(|n| fa.get(*n) != fb.get(*n))
        .cloned()
        .collect();
    (!bad.is_empty()).then(|| format!("files {}", bad.join(", ")))
}

impl AgentWorker {
    /// [`Worker::run_prefix`] (Milestone 340, docs/design/shrink-snapshots.md).
    fn prefix_once(
        &mut self,
        tag: &str,
        schedule: &Schedule,
        req: &hydo_engine::PrefixReq,
    ) -> Result<(RunResult, hydo_engine::PrefixRun), EngineError> {
        use crate::prefix::{Store, PER_RUN};
        use hydo_engine::PrefixRun;
        let keep = match req.keep {
            0 => crate::prefix::keep(self.kind),
            n => n.min(crate::prefix::keep(self.kind)),
        };
        if self.prefix.len() != keep {
            self.prefix = Store::new(keep);
        }
        let per_run = if req.per_run == 0 {
            PER_RUN
        } else {
            req.per_run
        };
        let (full, eng) = match engine_schedule(schedule, self.cadence) {
            Ok(x) => x,
            Err(e) => return Ok((refused(tag, &unsupported_verdict(&e)), PrefixRun::default())),
        };
        if let Err(u) = drive::check(&self.booted.plan, &full) {
            return Ok((
                refused(tag, &unsupported_verdict(&u.to_string())),
                PrefixRun::default(),
            ));
        }
        let (full_text, eng_text) = (full.to_string(), eng.to_string());
        let parent = if req.continue_ {
            let booted = &self.booted;
            self.prefix.best(&full_text, &eng_text, |rec| {
                check_branch(booted, &full, rec).is_ok()
            })
        } else {
            None
        };
        let slots = if req.take {
            self.prefix.slots_for_run(parent, per_run)
        } else {
            Vec::new()
        };
        let rec = parent.and_then(|i| self.prefix.entry(i).map(|e| (e.rec.clone(), e.clock)));
        let skip = rec.as_ref().map(|(p, _)| p.len() - 1).unwrap_or(0);
        let mut d = SearchDriver {
            booted: &self.booted,
            schedule: &full,
            policy: crate::search::SnapPolicy {
                names: slots.iter().map(|i| Store::name(*i)).collect(),
                skip,
                ranges: true,
                // a composed workload has a point per round: spread the run's snapshots over them
                spacing: if self.booted.plan.composed() {
                    (self.booted.plan.steps.max(1) as u64).div_ceil(per_run.max(1) as u64 + 1)
                } else {
                    1
                },
                ..crate::search::SnapPolicy::default()
            },
            parent: rec.as_ref().map(|(p, _)| p.as_slice()),
            ran: None,
            stops: Vec::new(),
            features: Vec::new(),
            taken: Vec::new(),
            snap_secs: 0.0,
        };
        let r = match parent {
            Some(i) => self.inner.resume(tag, &Store::name(i), &mut d, None),
            None => self.inner.run_controlled(tag, &eng, &mut d, None),
        };
        let r = match (r, parent) {
            (Ok(r), _) => r,
            // a continuation that failed: that snapshot is no use, and the run goes from S1
            (Err(EngineError::Protocol(why)), Some(i)) if why != "the engine exited" => {
                eprintln!(
                    "hydo: {tag}: continuing from {} failed: {why}",
                    Store::name(i)
                );
                self.prefix.drop_slot(i);
                let r = self.run_with(tag, schedule, &Policy::Schedule)?;
                return Ok((r, PrefixRun::default()));
            }
            (Err(e), _) => return Err(e),
        };
        let (stops, taken, ran, snap_secs) = (d.stops, d.taken, d.ran, d.snap_secs);
        if let Some(i) = parent {
            self.prefix.touch(i);
        }
        for t in &taken {
            let Some(i) = slots.iter().copied().find(|i| Store::name(*i) == t.name) else {
                continue;
            };
            let upto = &stops[..=t.index];
            let clock = crate::search::clock(upto, 0).0;
            self.prefix
                .put(i, &eng_text, Arc::new(upto.to_vec()), clock);
        }
        let mut out = PrefixRun {
            from: parent.map(Store::name),
            skipped: rec.as_ref().map(|(_, c)| *c).unwrap_or(0),
            clock: crate::search::clock(&stops, 0).0,
            taken: taken.len() as u64,
            snap_secs,
            ..PrefixRun::default()
        };
        let result = match ran {
            Some(ran) => {
                let prefix = format!("{tag}.");
                write_run_files(&self.outdir, tag, &prefix, &ran, None);
                run_result(&r, &ran)
            }
            None => r,
        };
        // M130: a SPIN on the Mac engine runs again in the rewind mode, from S1
        if rewind_again(self.kind, &result, schedule, self.spin_rewind).is_some() {
            let r = self.run_with(tag, schedule, &Policy::Schedule)?;
            return Ok((r, PrefixRun::default()));
        }
        if req.check && (parent.is_some() || !taken.is_empty()) {
            let fa = tag_files(&self.outdir, tag);
            let plain = self.run_with(tag, schedule, &Policy::Schedule)?;
            let fb = tag_files(&self.outdir, tag);
            out.checked = true;
            out.differs = prefix_differs(&result, &plain, &fa, &fb);
            return Ok((plain, out));
        }
        Ok((result, out))
    }

    /// [`Worker::prefix_gain`].
    fn gain_of(&mut self, schedule: &Schedule) -> u64 {
        let Ok((full, eng)) = engine_schedule(schedule, self.cadence) else {
            return 0;
        };
        let booted = &self.booted;
        let (ft, et) = (full.to_string(), eng.to_string());
        let best = self
            .prefix
            .best(&ft, &et, |rec| check_branch(booted, &full, rec).is_ok());
        best.and_then(|i| self.prefix.entry(i).map(|e| e.clock))
            .unwrap_or(0)
    }
}

impl Worker for AgentWorker {
    fn boot_secs(&self) -> f64 {
        self.inner.boot_secs()
    }

    fn run(&mut self, tag: &str, schedule: &Schedule) -> Result<RunResult, EngineError> {
        self.run_with(tag, schedule, &Policy::Schedule)
    }

    fn run_search(
        &mut self,
        tag: &str,
        schedule: &Schedule,
        req: &hydo_engine::SearchReq,
    ) -> Result<(RunResult, hydo_engine::SearchRun), EngineError> {
        self.search_once(tag, schedule, req)
    }

    fn run_prefix(
        &mut self,
        tag: &str,
        schedule: &Schedule,
        req: &hydo_engine::PrefixReq,
    ) -> Result<(RunResult, hydo_engine::PrefixRun), EngineError> {
        self.prefix_once(tag, schedule, req)
    }

    fn prefix_gain(&mut self, schedule: &Schedule) -> u64 {
        self.gain_of(schedule)
    }

    fn close(self: Box<Self>) -> Result<(), EngineError> {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_checks_from_manifest_sh() {
        // the lines of examples/redis-composed-hist's x86_64 image (M127)
        let sh = concat!(
            "HYDO_CHECK_c0_id='redis.kv_linearizable'\n",
            "HYDO_CHECK_c0_json='{\"history\":\"kv\",\"id\":\"redis.kv_linearizable\",\"initial\":\"nil\",\"model\":\"register\",\"msg\":\"m\",\"type\":\"linearizable\"}'\n",
            "HYDO_CHECK_c0_final='exec '\\''/hydo/checks/linearizable_redis.kv_linearizable'\\'''\n",
            "HYDO_CHECK_c1_id='redis.lists_append'\n",
            "HYDO_CHECK_c1_json='{\"history\":\"log\",\"id\":\"redis.lists_append\",\"type\":\"list-append\"}'\n",
            "HYDO_CHECK_c1_final=''\n",
            "HYDO_CHECKS='c0 c1'\n",
        );
        let h = history_of_manifest_sh(sh);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].id, "redis.kv_linearizable");
        assert!(
            h[0].json.starts_with("{\"history\":\"kv\""),
            "{}",
            h[0].json
        );
        assert_eq!(
            h[0].final_,
            "exec '/hydo/checks/linearizable_redis.kv_linearizable'"
        );
        assert_eq!(h[1].id, "redis.lists_append");
        assert_eq!(h[1].final_, "");
        assert!(history_of_manifest_sh("HYDO_CHECKS=''\n").is_empty());
        assert!(history_of_manifest_sh("").is_empty());
    }

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
        let again = rewind_again(EngineKind::Hvm, &spin, &s, true).expect("a SPIN on hvm");
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
            assert!(
                rewind_again(EngineKind::Hvm, &r, &s, true).is_some(),
                "{phase}"
            );
        }
        assert!(rewind_again(EngineKind::Hvx, &spin, &s, true).is_none());
        // the KVM engine's rewind mode (M161)
        assert_eq!(
            rewind_again(EngineKind::Kvm, &spin, &s, true).map(|s| s.to_string()),
            Some("disk=none spin=rewind | run P=front@10+2".to_string())
        );
        assert!(rewind_again(EngineKind::Kvm, &spin, &s, false).is_none());
        assert!(rewind_again(EngineKind::Hvm, &spin, &again, true).is_none());
        for v in [
            "HYDO-ORACLE: OK invariants=2 held",
            "HYDO-ORACLE: VIOLATION [x] out: SPIN (guest loops without exits",
            "ERROR workload: exit budget exhausted",
        ] {
            let r = RunResult::parse(&format!("RESULT a fp=1 wfp=2 secs=0 verdict={v}")).unwrap();
            assert!(rewind_again(EngineKind::Hvm, &r, &s, true).is_none(), "{v}");
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
