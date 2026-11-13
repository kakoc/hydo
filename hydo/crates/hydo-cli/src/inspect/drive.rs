//! Bringing a recorded run back to one of its stops, and holding it there (docs/design/inspect.md
//! "Replay to a stop").
//!
//! A run on an agent image is a pure function of its image, its schedule and the host's commands,
//! which the control transcripts record stop by stop. So a fresh engine replays the recorded
//! conversations ([`ReplayController`], which ends the conversation with an ERROR at the first
//! stop whose virtual time or agent-line count differs from the record) and, at the chosen stop,
//! a [`Halt`] takes the stop from the replay before the recorded commands of that stop are typed.
//! The recorded run's directory is only read; the engine writes into the session's own directory.
//!
//! Two ways to hold the stop:
//!
//! - **snapshot**: the engine snapshots the stop (`SNAP`, retyping commands still in the serial
//!   queue on restore), and every call runs in a conversation resumed from it (`RESUME`), so each
//!   call sees the stop's state. Engines snapshot only in the boot and the workload phase: not
//!   after the workload's end (DISKOFF), not in a recovery boot, not for timing schedules
//!   (`tj`, `xt`, `pp`, `q`, `cf`, `pct`).
//! - **live**: the conversation stays at the stop and calls run one after another on that branch.

use super::work::Work;
use super::Problem;
use hydo_control::agent::engine_schedule;
use hydo_control::{
    Action, AgentPoint, Controller, Ending, EngineAction, Finished, Header, Link, LinkError,
    LinkNext, Out, Record, ReplayController, RunOpts, Seen, Start, StopInfo, Transcript,
};
use hydo_engine::lockstep::{Lockstep, LockstepStart, SnapAnswer};
use hydo_engine::{BootSpec, ControlledEngine, ControlledWorker};
use hydo_schedule::{HostToken, Schedule};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The name of the session's snapshot in the engine.
pub const SNAP: &str = "hydo-inspect";

/// A conversation of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Conv {
    Boot,
    Run,
    Recovery,
}

impl Conv {
    pub fn name(self) -> &'static str {
        match self {
            Conv::Boot => "boot",
            Conv::Run => "run",
            Conv::Recovery => "recovery",
        }
    }
    pub fn parse(s: &str) -> Option<Conv> {
        match s {
            "boot" => Some(Conv::Boot),
            "run" => Some(Conv::Run),
            "recovery" => Some(Conv::Recovery),
            _ => None,
        }
    }
    pub const ALL: [Conv; 3] = [Conv::Boot, Conv::Run, Conv::Recovery];
}

/// A stop of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub conv: Conv,
    pub stop: u64,
}

impl Target {
    pub fn point(&self) -> String {
        format!("{}:{}", self.conv.name(), self.stop)
    }
}

/// A recorded run as `hydo inspect` reads it (its directory is never written).
pub struct Recorded {
    pub dir: PathBuf,
    pub doc: hydo_results::Run,
    /// The image path the result document names (see [`Recorded::image`]).
    pub image_text: String,
    /// The schedule with its cadence (the transcripts' header).
    pub schedule: Schedule,
    pub eargs: Vec<String>,
    pub backend: String,
    pub transcripts: Vec<(Conv, Transcript)>,
    /// Where the run's files are in `dir`.
    pub files: RunFiles,
}

/// The names of a run's files in its directory: a `hydo run` directory's (`control.jsonl`,
/// `one.log`, ...), or those of one run of an exploration, `consoles/<tag>.*` in the
/// exploration's directory (`hydo report --html` reads those).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunFiles {
    /// None: a run directory; Some(tag): an exploration's run.
    pub tag: Option<String>,
}

impl RunFiles {
    /// An exploration's run `tag`.
    pub fn explorer(tag: &str) -> RunFiles {
        RunFiles {
            tag: Some(tag.to_string()),
        }
    }

    /// A conversation's control transcript.
    pub fn transcript(&self, c: Conv) -> String {
        let s = match c {
            Conv::Boot => ".boot",
            Conv::Run => "",
            Conv::Recovery => ".recovery",
        };
        match &self.tag {
            None => format!("control{s}.jsonl"),
            Some(t) => format!("consoles/{t}.control{s}.jsonl"),
        }
    }

    /// The console the host controller merged the guest's lines into (`one.log`).
    pub fn console(&self, c: Conv) -> String {
        let s = if c == Conv::Recovery { ".recovery" } else { "" };
        match &self.tag {
            None => format!("one{s}.log"),
            Some(t) => format!("consoles/{t}{s}.log"),
        }
    }

    /// The engine's console with the agent's own lines (`one.agent.log`).
    pub fn agent_log(&self, c: Conv) -> String {
        let s = if c == Conv::Recovery { ".recovery" } else { "" };
        match &self.tag {
            None => format!("one{s}.agent.log"),
            Some(t) => format!("consoles/{t}{s}.agent.log"),
        }
    }

    /// The forced preemption points' records (Milestone 208).
    pub fn preempt(&self) -> String {
        match &self.tag {
            None => crate::preempt::FILE.to_string(),
            Some(t) => format!("consoles/{t}.preempt.jsonl"),
        }
    }

    /// The agent events a replay saw (only `hydo run` directories keep them).
    pub fn agent_events(&self) -> Option<&'static str> {
        self.tag.is_none().then_some(super::timeline::AGENT_EVENTS)
    }
}

impl Recorded {
    pub fn load(dir: &Path) -> Result<Recorded, Problem> {
        let doc = hydo_results::reader::read_run(&dir.join(hydo_results::RUN_DOC)).map_err(|e| {
            Problem::new(
                "no-run",
                format!("{}: {e}", dir.display()),
                vec!["hydo run <image> \"<schedule>\" --out <run dir>   (then inspect that directory)".into()],
            )
        })?;
        Recorded::of(dir, doc, RunFiles::default())
    }

    /// The run whose document is `doc`, with the files `files` names in `dir`.
    pub fn of(dir: &Path, doc: hydo_results::Run, files: RunFiles) -> Result<Recorded, Problem> {
        let mut transcripts = Vec::new();
        for c in Conv::ALL {
            let f = files.transcript(c);
            if let Ok(t) = std::fs::read_to_string(dir.join(&f)) {
                let t = Transcript::parse(&t).map_err(|e| {
                    Problem::new("no-run", format!("{}: {e}", dir.join(&f).display()), vec![])
                })?;
                transcripts.push((c, t));
            }
        }
        if transcripts.is_empty() {
            return Err(Problem::new(
                "no-run",
                format!(
                    "{} has no control transcripts (control.boot.jsonl, control.jsonl): only runs on \
                     agent images made by `hydo run` can be inspected",
                    dir.display()
                ),
                vec![format!("hydo run <image> \"{}\" --out <new run dir>", doc.schedule)],
            ));
        }
        let header_sched = transcripts
            .iter()
            .find_map(|(_, t)| t.header.schedule.clone())
            .unwrap_or_else(|| doc.schedule.clone());
        let schedule = Schedule::parse(&header_sched).map_err(|e| {
            Problem::new(
                "no-run",
                format!("the recorded schedule {header_sched:?}: {e}"),
                vec![],
            )
        })?;
        let image_text = doc
            .image
            .as_ref()
            .map(|i| i.path.clone())
            .unwrap_or_default();
        let eargs = doc
            .engine
            .as_ref()
            .and_then(|e| e.args.clone())
            .unwrap_or_default();
        let backend = doc
            .engine
            .as_ref()
            .and_then(|e| e.backend.clone())
            .unwrap_or_default();
        Ok(Recorded {
            dir: dir.to_path_buf(),
            doc,
            image_text,
            schedule,
            eargs,
            backend,
            transcripts,
            files,
        })
    }

    /// The run's image, which a replay needs: the very file the run used (its sha256 checked when
    /// the result records one).
    pub fn image(&self) -> Result<PathBuf, Problem> {
        let image = resolve(&self.image_text, &self.dir).ok_or_else(|| {
            Problem::new(
                "no-run",
                format!(
                    "the run's image {:?} is not there any more (result.json image.path); \
                     inspection replays the run on the very same image",
                    self.image_text
                ),
                vec!["rebuild the image with the same hydo and manifest (hydo build), then run the schedule again".into()],
            )
        })?;
        if let Some(want) = self.doc.image.as_ref().and_then(|i| i.sha256.as_deref()) {
            if let Ok(have) = hydo_engine::image::sha256_file(&image) {
                if have != want {
                    return Err(Problem::new(
                        "no-run",
                        format!(
                            "{} has other bytes than the image the run used (sha256 {} there, {} \
                             recorded): a replay would diverge",
                            image.display(),
                            &have[..16.min(have.len())],
                            &want[..16.min(want.len())]
                        ),
                        vec![format!(
                            "hydo run {} \"{}\" --out <new run dir>   (inspect the new run)",
                            image.display(),
                            self.doc.schedule
                        )],
                    ));
                }
            }
        }
        Ok(image)
    }

    pub fn transcript(&self, c: Conv) -> Option<&Transcript> {
        self.transcripts
            .iter()
            .find(|(x, _)| *x == c)
            .map(|(_, t)| t)
    }

    /// The first and the last stop of a conversation's transcript.
    pub fn span(&self, c: Conv) -> Option<(u64, u64)> {
        let t = self.transcript(c)?;
        let first = t.records.first()?.stop();
        let last = t.records.last()?.stop();
        Some((first, last))
    }

    /// Where the run's verdict was given: the stop of the last conversation's end.
    pub fn end(&self) -> Option<Target> {
        let (c, t) = self.transcripts.last()?;
        let n = t.records.iter().rev().find_map(|r| match r {
            Record::End { n, .. } => Some(*n),
            _ => None,
        })?;
        // a boot that ended at S1 is not where a run ended
        if *c == Conv::Boot && matches!(t.ending(), Some(Ending::AtSnapshot(_))) {
            return None;
        }
        Some(Target { conv: *c, stop: n })
    }

    /// The engine's virtual time at a stop, when the transcript recorded it.
    pub fn at_of(&self, t: Target) -> Option<u64> {
        self.transcript(t.conv)?
            .records
            .iter()
            .find_map(|r| match r {
                Record::Stop { n, at, .. } if *n == t.stop => Some(*at),
                _ => None,
            })
    }

    /// Whether the engines keep a snapshot at `t` ("snapshot"), or only a live branch ("live"),
    /// with the reason.
    pub fn snapshot_ok(&self, t: Target) -> Result<(), String> {
        let timing: Vec<String> = self
            .schedule
            .host
            .iter()
            .filter(|h| {
                let k = h.to_string();
                ["tj", "xt", "pp", "q=", "qs", "cf", "pct"]
                    .iter()
                    .any(|p| k.starts_with(p))
            })
            .map(|h| h.to_string())
            .collect();
        match t.conv {
            Conv::Boot => Ok(()),
            Conv::Recovery => Err("the engines keep no snapshot in a recovery boot".into()),
            Conv::Run => {
                if !timing.is_empty() {
                    return Err(format!(
                        "the engines keep no snapshot under the timing tokens {}",
                        timing.join(" ")
                    ));
                }
                let off = self.transcript(Conv::Run).and_then(|tr| {
                    tr.records.iter().find_map(|r| match r {
                        Record::Engine {
                            n,
                            action: EngineAction::DiskFaultsOff,
                        } => Some(*n),
                        _ => None,
                    })
                });
                match off {
                    Some(n) if t.stop > n => Err(format!(
                        "stop {} is after the workload's end (stop {n}), where the engines keep no snapshot",
                        t.stop
                    )),
                    _ => Ok(()),
                }
            }
        }
    }

    /// The first stop of a conversation at which the agent had said hello (agent lines seen).
    pub fn first_agent_stop(&self, c: Conv) -> Option<u64> {
        self.transcript(c)?.records.iter().find_map(|r| match r {
            Record::Stop { n, seen, .. } if *seen > 0 => Some(*n),
            _ => None,
        })
    }
}

/// An image path from a result document: as written when it is there, else relative to the run
/// directory's parent directories.
fn resolve(p: &str, dir: &Path) -> Option<PathBuf> {
    if p.is_empty() {
        return None;
    }
    let pb = PathBuf::from(p);
    if pb.is_file() {
        return Some(pb);
    }
    if pb.is_relative() {
        let mut d = Some(dir);
        while let Some(x) = d {
            if x.join(&pb).is_file() {
                return Some(x.join(&pb));
            }
            d = x.parent();
        }
    }
    None
}

// ---------------------------------------------------------------- the controllers

/// What runs a stop once the replay has handed it over.
pub trait Taker {
    fn step(&mut self, stop: &StopInfo, seen: &[Seen], out: &mut Out);
}

/// The replay until the target stop, then the taker (see the module documentation).
pub struct Halt<'t> {
    inner: &'t mut dyn Controller,
    target: Option<u64>,
    taker: &'t mut dyn Taker,
    pub taken: Option<StopInfo>,
    /// The agent's events the replay saw before the target (exits, ends of commands and probes,
    /// refusals), each with its stop, as the agent's JSON line: on hvx the agent's lines go over
    /// the host-call port, so the run's console has none of them.
    pub events: Vec<(StopInfo, serde_json::Value)>,
}

impl<'t> Halt<'t> {
    pub fn new(
        inner: &'t mut dyn Controller,
        target: Option<u64>,
        taker: &'t mut dyn Taker,
    ) -> Halt<'t> {
        Halt {
            inner,
            target,
            taker,
            taken: None,
            events: Vec::new(),
        }
    }
}

impl Controller for Halt<'_> {
    fn name(&self) -> &str {
        "inspect"
    }

    fn step(&mut self, stop: &StopInfo, seen: &[Seen], out: &mut Out) {
        if self.taken.is_none() {
            // what the stop saw happened before it (the taken stop included)
            for s in seen {
                if let Some(v) = event_json(s) {
                    self.events.push((*stop, v));
                }
            }
            if self.target.is_some_and(|t| stop.n >= t) {
                self.taken = Some(*stop);
            }
        }
        if self.taken.is_some() {
            self.taker.step(stop, seen, out)
        } else {
            self.inner.step(stop, seen, out)
        }
    }
}

/// An agent event or refusal as the agent's own JSON line (without its sequence number).
pub fn event_json(s: &Seen) -> Option<serde_json::Value> {
    use hydo_agent_proto::line::AgentLine;
    use hydo_agent_proto::msg::Event;
    let text = match s {
        Seen::Event(ev @ (Event::Exit(_) | Event::Done(_) | Event::Probe(_))) => AgentLine::Event {
            v: hydo_agent_proto::msg::PROTO,
            seq: 0,
            ev: ev.clone(),
        }
        .encode(),
        Seen::Refused { error, .. } => AgentLine::Error(error.clone()).encode(),
        _ => return None,
    };
    let mut v: serde_json::Value = serde_json::from_str(&text).ok()?;
    if let Some(o) = v.as_object_mut() {
        o.remove("seq");
    }
    Some(v)
}

/// Takes the stop's snapshot and ends the conversation there.
pub struct SnapTaker;

impl Taker for SnapTaker {
    fn step(&mut self, _stop: &StopInfo, _seen: &[Seen], out: &mut Out) {
        out.snapshot(SNAP);
        out.end(Ending::AtSnapshot(SNAP.into()));
    }
}

/// [`Link`] over the engines' lock-step session, as `hydo_control::lockstep_link` is, except
/// that a snapshot keeps the sends still in the serial queue (retyped on restore), and nothing
/// goes to a run directory.
pub struct InspectLink<'a, 'b> {
    inner: hydo_control::lockstep_link::LockstepLink<'a, 'b>,
}

impl<'a, 'b> InspectLink<'a, 'b> {
    pub fn new(ls: &'b mut Lockstep<'a>) -> InspectLink<'a, 'b> {
        InspectLink {
            inner: hydo_control::lockstep_link::LockstepLink::new(ls),
        }
    }
}

impl Link for InspectLink<'_, '_> {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        self.inner.next_stop()
    }
    fn send(&mut self, at: u64, bytes: &[u8]) -> Result<u64, LinkError> {
        self.inner.send(at, bytes)
    }
    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        self.inner.ack(send)
    }
    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        match self
            .inner
            .ls
            .snap(name, true)
            .map_err(|e| LinkError(e.to_string()))?
        {
            SnapAnswer::Taken { .. } => Ok(()),
            SnapAnswer::Refused(why) => Err(LinkError(format!("snapshot {name} refused: {why}"))),
        }
    }
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        self.inner.engine(a)
    }
    fn engine_cut(&self) -> bool {
        self.inner.engine_cut()
    }
    fn cont(&mut self) -> Result<(), LinkError> {
        self.inner.cont()
    }
    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.inner.end(e)
    }
    fn cut(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.inner.cut(e)
    }
}

/// The verdict a replay ends with when the run did not follow its recording.
fn diverged(f: &Finished) -> Option<String> {
    match &f.ending {
        Some(Ending::Verdict(v)) if v.contains("the replay diverged") => Some(v.clone()),
        _ => None,
    }
}

fn header(conv: Conv, s: &Schedule, start: &str) -> Header {
    hydo_control::drive::header(conv.name(), Some(s), start)
}

// ---------------------------------------------------------------- the engine

/// An engine booted for one recorded run.
pub struct Machine {
    pub eng: Arc<dyn ControlledEngine + Send + Sync>,
    pub spec: BootSpec,
    pub plan: hydo_control::Plan,
    pub cadence: u64,
}

/// How a stop is held.
pub enum Held {
    /// The engine keeps the stop's snapshot; every call resumes from `point`.
    Snapshot {
        worker: Box<dyn ControlledWorker>,
        point: AgentPoint,
    },
    /// The taker served every call inside the replay's conversation; the engine is gone.
    Live,
}

/// What reaching a stop cost.
#[derive(Clone, Debug, Default)]
pub struct Reach {
    /// The stop as the replay reached it.
    pub stop: Option<StopInfo>,
    /// Stops the replay took (every conversation up to the target).
    pub stops: u64,
    /// The agent's events the replay saw before the target ([`Halt::events`]).
    pub events: Vec<(Conv, StopInfo, serde_json::Value)>,
}

/// Counts the stops a conversation took (the last stop's number less the first's).
fn stops_of(f: &Finished, first: Option<u64>) -> u64 {
    let last = f.last_stop.map(|s| s.n).unwrap_or(0);
    last.saturating_sub(first.unwrap_or(0)) + 1
}

fn link_err(e: impl std::fmt::Display) -> Problem {
    Problem::new("engine", e.to_string(), vec![])
}

fn run_conv(
    link: &mut dyn Link,
    ctl: &mut dyn Controller,
    start: Start,
    h: Header,
) -> Result<Finished, Problem> {
    hydo_control::run_with(link, ctl, start, h, RunOpts::default()).map_err(link_err)
}

impl Machine {
    /// Replay `rec` to `target` and hand the stop to `taker`. With `keep`, the taker ends the
    /// conversation at the snapshot [`SNAP`] ([`SnapTaker`]) and the worker that holds it is
    /// returned; without, the taker serves everything inside the conversation (live) and the
    /// engine is closed when it ends.
    pub fn reach(
        &self,
        rec: &Recorded,
        target: Target,
        taker: &mut dyn Taker,
        keep: bool,
        errlog: &Path,
    ) -> Result<(Held, Reach), Problem> {
        let mut reach = Reach::default();
        let boot_s = Schedule {
            host: vec![HostToken::Rb(self.cadence)],
            guest: Vec::new(),
        };
        let (_, eng_sched) = engine_schedule(&rec.schedule, self.cadence)
            .map_err(|e| Problem::new("refused", e, vec![]))?;
        let is_live = !keep;
        let boot_t = rec.transcript(Conv::Boot).ok_or_else(|| {
            Problem::new(
                "no-run",
                "the run has no boot transcript (control.boot.jsonl)",
                vec![],
            )
        })?;
        let mut cb = self
            .eng
            .start_controlled(&self.spec, &boot_s, errlog)
            .map_err(link_err)?;
        // the boot: replayed, and taken at the target when it is there
        let boot_fin = {
            let mut ls = cb.session(None);
            let mut link = InspectLink::new(&mut ls);
            let mut rp = ReplayController::new(boot_t);
            let target_n = (target.conv == Conv::Boot).then_some(target.stop);
            let mut tk_holder;
            let tk: &mut dyn Taker = if target.conv == Conv::Boot {
                &mut *taker
            } else {
                tk_holder = SnapTaker;
                &mut tk_holder
            };
            let mut halt = Halt::new(&mut rp, target_n, tk);
            let f = run_conv(
                &mut link,
                &mut halt,
                Start::Boot,
                header(Conv::Boot, &rec.schedule, "boot"),
            )?;
            reach.stop = halt.taken.filter(|_| target.conv == Conv::Boot);
            let ev = std::mem::take(&mut halt.events);
            reach
                .events
                .extend(ev.into_iter().map(|(s, v)| (Conv::Boot, s, v)));
            f
        };
        reach.stops += stops_of(&boot_fin, None);
        if let Some(v) = diverged(&boot_fin) {
            let _ = cb.ready().map(|w| w.close());
            return Err(divergence(rec, &v));
        }
        let worker = cb.ready().map_err(link_err)?;
        if target.conv == Conv::Boot {
            return self.held(worker, &boot_fin, reach, is_live, target);
        }
        let point = |name: &str| -> Result<AgentPoint, Problem> {
            boot_fin
                .snapshots
                .iter()
                .rev()
                .find(|p| p.snapshot == name)
                .cloned()
                .ok_or_else(|| {
                    Problem::new(
                        "engine",
                        format!("the replayed boot took no snapshot {name}"),
                        vec![],
                    )
                })
        };
        let (s0, s1) = (point("S0")?, point("S1")?);
        let mut worker = worker;
        let run_t = rec.transcript(Conv::Run).ok_or_else(|| {
            Problem::new(
                "bad-point",
                "the run has no run conversation (control.jsonl)",
                vec![],
            )
        })?;
        let start = LockstepStart::Run {
            tag: "inspect".into(),
            schedule: eng_sched,
        };
        let fin = {
            let mut ls = worker.lockstep(&start, None).map_err(link_err)?;
            let mut link = InspectLink::new(&mut ls);
            let mut rp = ReplayController::new(run_t);
            let mut holder = SnapTaker;
            let (tn, tk): (Option<u64>, &mut dyn Taker) = if target.conv == Conv::Run {
                (Some(target.stop), &mut *taker)
            } else {
                (None, &mut holder)
            };
            let mut halt = Halt::new(&mut rp, tn, tk);
            let f = run_conv(
                &mut link,
                &mut halt,
                Start::Resume(s1),
                header(Conv::Run, &rec.schedule, "resume S1"),
            )?;
            let taken = halt.taken;
            let ev = std::mem::take(&mut halt.events);
            reach
                .events
                .extend(ev.into_iter().map(|(s, v)| (Conv::Run, s, v)));
            reach.stops += stops_of(&f, Some(s_first(&f)));
            if let Some(v) = diverged(&f) {
                return Err(divergence(rec, &v));
            }
            if target.conv == Conv::Run {
                reach.stop = taken;
                f
            } else {
                // the recovery boot, on the same engine run
                if !matches!(f.ending, Some(Ending::PowerCut { .. })) {
                    return Err(Problem::new(
                        "bad-point",
                        "the replayed run did not end with a power cut, so it has no recovery boot",
                        vec![],
                    ));
                }
                let rec_t = rec.transcript(Conv::Recovery).ok_or_else(|| {
                    Problem::new("bad-point", "the run has no recovery transcript", vec![])
                })?;
                let mut rp = ReplayController::new(rec_t);
                let mut halt = Halt::new(&mut rp, Some(target.stop), &mut *taker);
                let f2 = run_conv(
                    &mut link,
                    &mut halt,
                    Start::Resume(s0),
                    header(Conv::Recovery, &rec.schedule, "resume S0"),
                )?;
                reach.stop = halt.taken;
                let ev = std::mem::take(&mut halt.events);
                reach
                    .events
                    .extend(ev.into_iter().map(|(s, v)| (Conv::Recovery, s, v)));
                reach.stops += stops_of(&f2, Some(s_first(&f2)));
                if let Some(v) = diverged(&f2) {
                    return Err(divergence(rec, &v));
                }
                f2
            }
        };
        self.held(worker, &fin, reach, is_live, target)
    }

    fn held(
        &self,
        worker: Box<dyn ControlledWorker>,
        fin: &Finished,
        reach: Reach,
        live: bool,
        target: Target,
    ) -> Result<(Held, Reach), Problem> {
        if reach.stop.is_none() {
            let _ = worker.close();
            return Err(Problem::new(
                "bad-point",
                format!(
                    "the replay of the {} conversation ended before stop {} ({})",
                    target.conv.name(),
                    target.stop,
                    fin.ending
                        .as_ref()
                        .map(|e| format!("{e:?}"))
                        .unwrap_or_else(|| fin.result.clone())
                ),
                vec![],
            ));
        }
        if live {
            let _ = worker.close();
            return Ok((Held::Live, reach));
        }
        let point = fin
            .snapshots
            .iter()
            .rev()
            .find(|p| p.snapshot == SNAP)
            .cloned()
            .ok_or_else(|| {
                Problem::new("engine", "the engine took no snapshot at the stop", vec![])
            })?;
        Ok((Held::Snapshot { worker, point }, reach))
    }

    /// One call from the held snapshot: a conversation resumed from it that does `work`, then
    /// ends; with `keep`, the state after it becomes the new held snapshot.
    pub fn call(
        &self,
        worker: &mut Box<dyn ControlledWorker>,
        point: &mut AgentPoint,
        rec: &Recorded,
        work: &mut Work,
        keep: bool,
        stop_limit: u64,
    ) -> Result<Called, Problem> {
        let from = point.clone();
        let c = self.call_from(
            worker,
            (SNAP, &from),
            keep.then_some(SNAP),
            rec,
            work,
            stop_limit,
        )?;
        if let Some(p) = &c.point {
            *point = p.clone();
        }
        Ok(c)
    }

    /// One call from the engine's snapshot `from` (its name and the agent's position there): a
    /// conversation resumed from it that does `work`, then ends; with `keep_as`, the state after
    /// it is kept as the snapshot of that name (docs/design/agent-environments.md: a branch).
    pub fn call_from(
        &self,
        worker: &mut Box<dyn ControlledWorker>,
        from: (&str, &AgentPoint),
        keep_as: Option<&str>,
        rec: &Recorded,
        work: &mut Work,
        stop_limit: u64,
    ) -> Result<Called, Problem> {
        let start = LockstepStart::Resume {
            tag: "inspect".into(),
            snapshot: from.0.into(),
        };
        let mut ls = worker.lockstep(&start, None).map_err(link_err)?;
        let mut link = InspectLink::new(&mut ls);
        let mut ctl = WorkCtl {
            work,
            keep: keep_as,
            stop_limit,
        };
        let f = run_conv(
            &mut link,
            &mut ctl,
            Start::Resume(from.1.clone()),
            header(Conv::Run, &rec.schedule, &format!("resume {}", from.0)),
        )?;
        let point = keep_as.and_then(|name| {
            f.snapshots
                .iter()
                .rev()
                .find(|p| p.snapshot == name)
                .cloned()
        });
        if keep_as.is_some() && point.is_none() && work.failed.is_none() {
            return Err(Problem::new(
                "engine",
                format!(
                    "the engine kept no snapshot {} ({})",
                    keep_as.unwrap_or_default(),
                    f.result
                ),
                vec![],
            ));
        }
        Ok(Called {
            point,
            fp: result_fp(&f.result),
        })
    }
}

/// What one call left.
pub struct Called {
    /// The agent's position at the kept snapshot (`keep_as`).
    pub point: Option<AgentPoint>,
    /// The engine's fingerprint of the state at the call's end (the RESULT line's `fp=`).
    pub fp: Option<String>,
}

/// `fp=<hex>` of an engine's RESULT line.
pub fn result_fp(line: &str) -> Option<String> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix("fp="))
        .map(str::to_string)
}

fn s_first(f: &Finished) -> u64 {
    f.transcript.records.first().map(|r| r.stop()).unwrap_or(0)
}

fn divergence(rec: &Recorded, v: &str) -> Problem {
    Problem::new(
        "engine",
        format!(
            "{v}. The run cannot be brought back exactly here (another engine, module or hydo than \
             the one that recorded it?)"
        ),
        vec![format!(
            "hydo repro {}   (does the run itself reproduce?)",
            rec.dir.display()
        )],
    )
}

/// A conversation that does one call's work and ends (at the snapshot `keep`, when given).
struct WorkCtl<'w, 'k> {
    work: &'w mut Work,
    keep: Option<&'k str>,
    stop_limit: u64,
}

impl Controller for WorkCtl<'_, '_> {
    fn name(&self) -> &str {
        "inspect"
    }
    fn step(&mut self, _stop: &StopInfo, seen: &[Seen], out: &mut Out) {
        self.work.step(seen, out);
        if self.work.finished() || self.work.stops >= self.stop_limit {
            if self.work.stops >= self.stop_limit && !self.work.finished() {
                self.work.failed = Some(format!(
                    "no answer within {} engine stops (the guest may be stuck; try --timeout)",
                    self.stop_limit
                ));
            }
            if let (Some(name), None) = (self.keep, &self.work.failed) {
                out.snapshot(name);
                out.end(Ending::AtSnapshot(name.into()));
            } else {
                out.end(Ending::Verdict("HYDO-ORACLE: OK inspect".into()));
            }
        }
    }
}

/// Every action of an `Out` (for tests).
#[allow(dead_code)]
pub fn actions(out: Out) -> Vec<Action> {
    out.into_actions()
}
