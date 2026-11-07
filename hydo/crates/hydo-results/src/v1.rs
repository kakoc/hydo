//! Version 1 of the results format: the types, which are the specification together with the
//! example documents (`hydo/schema/examples/results/v1/`).
//!
//! Open sets of values (a finding's `kind` and `status`, a run's `role`, a verdict's `kind`) are
//! strings, not schema enums: a later version-1 writer may add a value, and readers treat a value
//! they do not know like any other string. The values hydo writes today are in [`kind`],
//! [`status`] and [`role`].

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::borrow::Cow;

/// The version field of every version-1 record and document: always 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V1;

impl Serialize for V1 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(1)
    }
}

impl<'de> Deserialize<'de> for V1 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<V1, D::Error> {
        let n = u64::deserialize(d)?;
        if n == 1 {
            Ok(V1)
        } else {
            Err(serde::de::Error::custom(format!(
                "results version {n}, this type reads version 1"
            )))
        }
    }
}

impl JsonSchema for V1 {
    fn schema_name() -> Cow<'static, str> {
        "Version1".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({"type": "integer", "const": 1})
    }
}

/// A record's `type` tag: a string with one value.
macro_rules! tag {
    ($name:ident, $lit:literal) => {
        #[doc = concat!("The `type` of this record: always `\"", $lit, "\"`.")]
        #[derive(
            Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema,
        )]
        pub enum $name {
            #[default]
            #[serde(rename = $lit)]
            Tag,
        }
    };
}

tag!(RunType, "run");
tag!(StartType, "start");
tag!(ShrinkType, "shrink");
tag!(EndType, "end");
tag!(ExplorationType, "exploration");
tag!(DiffType, "diff");

/// The values of [`Finding::kind`] hydo writes.
pub mod kind {
    /// The guest's own oracle or the manifest's invariants failed (the guest's verdict).
    pub const INVARIANT: &str = "invariant";
    /// An assertion property (HYDO-ASSERT records, hydo/ASSERTIONS.md) failed in this run.
    pub const ASSERTION: &str = "assertion";
    /// A history check run on the host (hydo/HISTORIES.md).
    pub const HISTORY_CHECK: &str = "history-check";
    /// An error kind in a service's log (`errors:` in the manifest).
    pub const ERROR_LOG: &str = "error-log";
}

/// The values of [`Finding::status`] hydo writes.
pub mod status {
    /// Held (a history check that passed).
    pub const OK: &str = "ok";
    /// Failed: an invariant, an assertion or a history check.
    pub const VIOLATION: &str = "violation";
    /// The checker ran out of its step budget.
    pub const UNDECIDED: &str = "undecided";
    /// Could not be evaluated (an invariant's ERROR verdict, an unreadable history).
    pub const ERROR: &str = "error";
    /// An error kind the known list does not cover: a finding.
    pub const NEW: &str = "new";
    /// An error kind the known list covers.
    pub const KNOWN: &str = "known";
    /// A property with no passing evaluation (sometimes, reachable) in a property table.
    pub const NOT_EXERCISED: &str = "not-exercised";
    /// An eventually property with no resolved episode in a property table.
    pub const NOT_EVALUATED: &str = "not-evaluated";
}

/// The values of [`Run::role`] hydo writes: why the explorer made the run.
pub mod role {
    /// A schedule of the exploration (a seed or a sweep point).
    pub const FIRST: &str = "first";
    /// A failure run again to confirm its fingerprint.
    pub const RERUN: &str = "rerun";
    /// A shrink candidate.
    pub const SHRINK: &str = "shrink";
    /// A minimal schedule (or its stable form) run to confirm it.
    pub const MINIMAL: &str = "minimal";
    /// A shrink candidate of a new error kind.
    pub const ERROR_SHRINK: &str = "error-shrink";
    /// A new error kind's minimal schedule run to confirm it.
    pub const ERROR_MINIMAL: &str = "error-minimal";
    /// A freeze sweep's trace run (counts the packets of one stream).
    pub const TRACE: &str = "trace";
    /// The flush-count probe of `--more-faults`.
    pub const PROBE: &str = "probe";
    /// `--schedule`: one schedule given by hand.
    pub const MANUAL: &str = "manual";
    /// A run of a branch exploration (`--branch`).
    pub const BRANCH: &str = "branch";
}

/// The image a run booted.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Image {
    /// The image as it was given (a path, or an explorer image name).
    pub path: String,
    /// The path the engine opened, when it differs from `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    /// SHA-256 of the image file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The fingerprint epoch of the hydo that built the image (its `/etc/hydo/image.json`, added
    /// in M155; absent for an image built before).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    /// The guest kernel the image names (its record), as `<file> (Linux <version>, sha256 <hex>)`
    /// (added in M155).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// The hydo that built the image, from the build record next to it (`<image>.json`, M155).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_by: Option<BuiltBy>,
    /// SHA-256 of the manifest the image was built from (the build record, M155).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
    /// The Docker/OCI images of its services with their digests (the build record, M155).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceImage>,
}

/// The hydo that built an image (added in M155).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuiltBy {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// A service built from a Docker/OCI image (added in M155).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceImage {
    pub service: String,
    /// The image as the manifest gives it.
    pub image: String,
    /// The image manifest's digest (`sha256:...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// The image configuration's digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_digest: Option<String>,
}

/// The engine and the machine it gave the guest.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Engine {
    /// `hvx` (the Linux kernel module), `hvm` (Hypervisor.framework on the Mac) or `kvm` (stock Linux KVM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// The options passed to the engine (`--ram-mib`, `--vcpus`, `--cpu`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u64>,
    /// The guest CPU level (`v2`, `v3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    /// hvm: the guest kernel and the first 16 hex digits of its SHA-256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_kernel: Option<String>,
    /// hvx: the loaded module's srcversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_srcversion: Option<String>,
    /// An exploration's worker count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<u64>,
}

/// A run's verdict.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Verdict {
    /// `ok`, `violation` or `error` (of `text`).
    pub kind: String,
    /// The verdict line (the guest's `HYDO-ORACLE: ...`, or what the host made of it).
    pub text: String,
    /// The guest's own verdict, when the host's history checks replaced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<String>,
    /// The verdict before the error logs failed the run (`unknown: fail`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_error_logs: Option<String>,
}

/// A run's fingerprints: equal fingerprints mean bit-identical runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fingerprints {
    /// The whole run (console, snapshot, guest RAM, disk).
    pub fp: String,
    /// The workload's part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wfp: Option<String>,
    /// An agent image's run: the hash of its control transcripts (`ctl=` on the RESULT line,
    /// M126): equal hashes mean the host typed the same commands at the same stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctl: Option<String>,
}

/// What a run checked, in one shape for every source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    /// The source: `invariant`, `assertion`, `history-check`, `error-log` ([`kind`]).
    pub kind: String,
    /// The property id, the check id, the invariant's id (or the verdict's signature), or the
    /// error kind's signature.
    pub id: String,
    /// The outcome ([`status`]).
    pub status: String,
    /// One line for people.
    #[serde(default)]
    pub message: String,
    /// What the source says beyond that (the examples show each source's fields).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub details: Map<String, Value>,
}

/// Engine counters a run reported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Counters {
    /// Tick jitter: the first and last delayed tick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tjk: Option<Vec<u64>>,
    /// Tick jitter: delayed ticks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tjn: Option<u64>,
    /// PCT points that fired, and the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pctf: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pctw: Option<u64>,
    /// Inserted ticks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xtn: Option<u64>,
    /// vCPU turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qn: Option<u64>,
}

/// A run's error-log analysis (`errors:` in the manifest).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorLogs {
    /// The known list used (absent: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_sha256: Option<String>,
    /// `report` or `fail`: what a new kind does to the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
    /// The workload's steps (the labels of where a line was written refer to them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<i64>,
    /// The guest's HYDO-ERRLOG-SUM record of every boot.
    #[serde(default)]
    pub boots: Vec<Value>,
    /// A boot printed only its first kinds (`errors.max`).
    #[serde(default)]
    pub truncated: bool,
}

/// A branch run's branch point.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Branch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tjlo: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<u64>,
}

/// A freeze sweep's trace run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Trace {
    /// `<service>><peer>`.
    pub pair: String,
    /// `<` or `>`.
    pub dir: String,
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
    pub step: i64,
    /// Packets of the stream in that step (absent: the freeze never fired).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
    /// Taken from an earlier sweep (`--fc-traces`), not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused: Option<bool>,
}

/// A shrink candidate's place in its shrink.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShrinkStep {
    /// The signature being shrunk (a failure's, or an error kind's).
    pub of: String,
    /// The round (1, 2, ...).
    pub round: u64,
    /// The candidate kept the failure and became the next round's schedule.
    pub kept: bool,
}

/// Files of a run, relative to the document's directory.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Artifacts {
    /// The guest consoles: the run's, then the recovery boot's after a power cut.
    #[serde(default)]
    pub consoles: Vec<String>,
    /// Everything the engine printed (`hydo run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_log: Option<String>,
}

/// The control transcript of one conversation of a run on an agent image (added in M120,
/// docs/design/guest-control.md "The control transcript"): which controller decided, the
/// engine's stop cadence, the transcript's hash and where the transcript file is. A run has one
/// per conversation (the run; after a power cut also the recovery boot).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Control {
    /// The conversation: `run`, `recovery`, `boot` or `solo`.
    pub conversation: String,
    /// The controller that decided (`schedule`), or `replay` for a transcript typed again.
    pub controller: String,
    /// The engine's stop cadence (`rb=<n>`, or `default`).
    pub cadence: String,
    /// The transcript's hash, 16 hex digits (`ctl=` on the RESULT line).
    pub hash: String,
    /// Agent commands typed.
    pub sends: u64,
    /// Stops at which the host acted.
    pub stops: u64,
    /// The transcript file (`control.jsonl`, `control.recovery.jsonl`), relative to the
    /// document's directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

/// One run of one schedule: `result.json` of `hydo run`, and every run of an exploration in
/// `events.jsonl`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: RunType,
    /// The hydo that wrote the document (documents only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydo_version: Option<String>,
    /// The fingerprint epoch of that hydo (documents only; added in M155): fingerprints of one
    /// epoch are comparable, across epochs only verdicts are. Absent: before M155, epoch 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    /// The explorer's name for the run (`seed-3`, `repro-3`, `shrink-1-0`, `min-a`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The exploration seed (or sweep point number) the run belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Why the explorer made the run ([`role`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The schedule, in the schedule language (`disk=cut | run C=12`).
    pub schedule: String,
    /// The explorer's finding class of the schedule (`durable-defaults`, `unsafe-knob`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// A sweep point's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<String>,
    /// Documents only: in a stream, the [`Start`] record names the image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    /// Documents only: in a stream, the [`Start`] record names the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<Engine>,
    pub verdict: Verdict,
    pub fingerprints: Fingerprints,
    /// What the run checked: failed invariants and assertions, every history check, every
    /// error kind.
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// Documents only: the run's property table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<PropertyTable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_logs: Option<ErrorLogs>,
    /// The run's wall time in seconds (engine side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters: Option<Counters>,
    /// The explorer worker that ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<u64>,
    /// Answered from `$HYDO_EXPLORE_CACHE` instead of run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<Branch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<Trace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shrink: Option<ShrinkStep>,
    /// The engine's RESULT line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Artifacts>,
    /// Runs on agent images: the control transcript of each conversation (added in M120).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control: Vec<Control>,
    /// Why the run failed, when it did not pass (added in M178): what the run's own files say
    /// and, when collected, what a replay of the run read in the guest at the stop where the
    /// verdict was given. `hydo run` writes it; the fingerprints never depend on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<Diagnosis>,
}

// ------------------------------------------------------------------ diagnosis and inspection

/// Where in a run something is (added in M178): its conversation (`boot`, `run`, `recovery`),
/// the number of the stop (the control transcript's `n`) and the engine's virtual clock there
/// (hvx's instruction clock, hvm's and kvm's exit count). `point` is the two in the form
/// `hydo inspect start --at` takes (`boot:172`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunPoint {
    pub point: String,
    pub conversation: String,
    pub stop: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

/// How a process the host started ended (added in M178): the agent's `exit` (a service, the
/// provisioning) or `done` (a command) event.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessEnd {
    /// The service's or the command's name as the host started it (`redis`, `provision`,
    /// `setup:redis`, `workload`).
    pub handle: String,
    /// The command, as text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The exit status as a shell reports it (the code, or 128 + the signal).
    pub status: u64,
    /// The signal that ended it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<u64>,
    /// The signal's name (`KILL`, `SEGV`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_name: Option<String>,
    /// Its time limit ended it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
    /// The host sent that signal itself (a kill of the schedule, the end of a check). False: a
    /// signal hydo did not send (for SIGKILL usually the kernel's out-of-memory killer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_by_hydo: Option<bool>,
    /// What the agent kept of its output (commands only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Where it ended (the first recorded stop by which the host had seen the event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<RunPoint>,
}

/// A readiness check's end (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProbeEnd {
    pub service: String,
    pub command: String,
    /// `ok`, `deadline` (it did not pass in time) or `died` (the service's process ended first).
    pub why: String,
    pub tries: u64,
    /// The last attempt's exit status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<u64>,
    /// The time limit, in seconds of guest time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_s: Option<u64>,
}

/// Text read in the guest (added in M178): a log's tail, a command's output.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// What it is, in words (`the log of redis`, `the kernel's messages`).
    pub what: String,
    /// The file read, or the command run.
    pub source: String,
    /// The text (invalid UTF-8 replaced by U+FFFD).
    pub text: String,
    /// The whole size in bytes (of the file, or of the command's output).
    pub bytes: u64,
    /// Only the end (a log) or the start (a command's output) of it is in `text`.
    pub truncated: bool,
    /// A command's exit status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u64>,
}

/// Why a run failed (added in M178; `result.json`'s `diagnosis`, `hydo inspect diagnose`).
///
/// `failure` names what failed (an open set): `service-exited` (a service's process ended
/// while it started), `service-not-ready` (its readiness check did not pass in time),
/// `service-setup`, `provision`, `restart` (a service did not come back after the faults),
/// `mount`, `timeout` (a phase ran out of time), `vm` (the guest or the engine stopped),
/// `control`, `violation` (a check failed; no process failed) or `other`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnosis {
    /// One or two sentences for a reader who sees nothing else.
    pub summary: String,
    pub failure: String,
    /// The service it concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The stop where the verdict was given (`hydo inspect start <run dir> --at end` looks there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_at: Option<RunPoint>,
    /// The process whose end caused the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessEnd>,
    /// The readiness check that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeEnd>,
    /// Every process the host started that ended before the verdict with a status other than 0.
    #[serde(default)]
    pub exits: Vec<ProcessEnd>,
    /// Conclusions drawn from the facts, each with its reason.
    #[serde(default)]
    pub likely: Vec<String>,
    /// The kernel's lines about crashes and memory (out of memory, segfault, panic), from the
    /// console and, when collected, from `dmesg`.
    #[serde(default)]
    pub kernel: Vec<String>,
    /// `collected` (a replay read `evidence` in the guest), `not-collected` (with the reason in
    /// `evidence_note`) or `failed`.
    pub evidence_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_note: Option<String>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// Commands that look further, exactly as they can be run.
    #[serde(default)]
    pub next: Vec<String>,
}

tag!(DiagnosisType, "diagnosis");
tag!(TimelineType, "timeline");
tag!(InspectType, "inspect");
tag!(InspectErrorType, "inspect-error");

/// `hydo inspect diagnose <run dir>`: a run's [`Diagnosis`] as a document (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosisDoc {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: DiagnosisType,
    pub run_dir: String,
    pub verdict: Verdict,
    pub diagnosis: Diagnosis,
}

/// One event of a run's timeline (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineEvent {
    /// `<conversation>:<stop>`, the form `hydo inspect start --at` takes; absent for the run's
    /// result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<String>,
    pub conversation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<u64>,
    /// The engine's virtual clock at the stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    /// An open set: `command` (the host typed an agent command), `exit` (a process the host
    /// started ended), `probe` (a readiness check ended), `done` (a command ended), `fault` (a
    /// fault applied or healed), `note` (the host controller's note), `report` (a line of the
    /// run's report), `assertion`, `guest` (a line the guest printed), `snapshot`, `engine`,
    /// `end` (the end of a conversation), `verdict`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The record behind it (an agent event, a command).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// One conversation of a run in its timeline (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineConversation {
    pub conversation: String,
    pub first_stop: u64,
    pub last_stop: u64,
    /// How it ended (`end at snapshot S1`, `end: HYDO-ORACLE: OK ...`, `power cut`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// Whether `hydo inspect` can keep a snapshot at its stops (boot and the workload; not after
    /// the workload's end, not in a recovery boot, not under timing schedules) or works on one
    /// live branch there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshots: Option<String>,
}

/// `hydo inspect events <run dir>`: a run's events with their stops, filtered and paged (added in
/// M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Timeline {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: TimelineType,
    pub run_dir: String,
    pub verdict: Verdict,
    #[serde(default)]
    pub conversations: Vec<TimelineConversation>,
    /// Events matching the filters (before paging).
    pub total: u64,
    /// The index of the first event shown among them.
    pub offset: u64,
    #[serde(default)]
    pub events: Vec<TimelineEvent>,
    /// More events match than are shown.
    pub truncated: bool,
    /// The command that shows the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// Text read from the guest by `hydo inspect` (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectText {
    /// The text (invalid UTF-8 replaced by U+FFFD), absent when `hex` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The bytes in lower-case hex, when they are not UTF-8 text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    /// The whole size in bytes.
    pub bytes: u64,
    /// The bytes shown start here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Fewer bytes are shown than there are (`--max-bytes`).
    pub truncated: bool,
}

/// A command run in the guest by `hydo inspect` (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectExec {
    pub argv: Vec<String>,
    /// The service whose network namespace it ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The exit status (the code, or 128 + the signal).
    pub status: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<u64>,
    /// Its time limit (`--timeout`, guest time) ended it.
    pub timed_out: bool,
    pub stdout: InspectText,
    pub stderr: InspectText,
}

/// A file or directory entry (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectEntry {
    pub name: String,
    /// `file`, `dir`, `link`, `socket`, `fifo`, `device` or `other`.
    pub kind: String,
    pub bytes: u64,
    /// The permission bits in octal.
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Seconds since 1970 (the guest's clock).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// A process in the guest (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectProcess {
    pub pid: u64,
    pub ppid: u64,
    /// The kernel's state letter (`R`, `S`, `D`, `T` stopped, `Z` zombie).
    pub state: String,
    /// Resident memory in KiB.
    pub rss_kib: u64,
    pub threads: u64,
    pub command: String,
}

/// A socket in the guest (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectSocket {
    /// `tcp`, `tcp6`, `udp`, `udp6` or `unix`.
    pub proto: String,
    pub local: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// TCP's state (`LISTEN`, `ESTABLISHED`, ...).
    pub state: String,
    pub send_queue: u64,
    pub recv_queue: u64,
}

/// An inspection session (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectSession {
    pub id: String,
    pub run_dir: String,
    /// The stop the session looks at, and how: `snapshot` (every call starts from the stop's
    /// snapshot, so each sees the same state) or `live` (calls run one after another on one
    /// branch from the stop; each sees what the earlier ones left).
    pub at: RunPoint,
    pub state: String,
    /// `starting`, `ready` or `ended`.
    pub status: String,
    /// It ends by itself after this many seconds without a call.
    pub idle_timeout_s: u64,
    /// Every process was stopped (SIGSTOP) at the stop (`--freeze`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen: Option<bool>,
    /// The engine (`hvx`, `hvm`, `kvm`).
    pub engine: String,
    /// Calls served so far.
    pub calls: u64,
}

/// What a call cost (added in M178).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectCost {
    /// Wall time of the call.
    pub wall_ms: u64,
    /// Engine stops the call took (a replay to the stop counts every stop of it).
    pub stops: u64,
}

/// One answer of `hydo inspect` (added in M178): `op` says which; the members of that op are set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Inspect {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: InspectType,
    /// `start`, `status`, `list`, `stop`, `exec`, `read`, `ls`, `logs`, `ps`, `sockets`,
    /// `advance`.
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<InspectSession>,
    /// `list`: every session of this user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<InspectSession>,
    /// The state the call saw: the session's stop (`snapshot`), or the branch after the earlier
    /// calls (`live`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<RunPoint>,
    /// `exec`, `logs`, `ps`, `sockets`: the command and its output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<InspectExec>,
    /// `read`: the file's bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<InspectText>,
    /// `ls`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<InspectEntry>,
    /// `ps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<InspectProcess>,
    /// `sockets`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sockets: Vec<InspectSocket>,
    /// Lists shown in part (`--limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<InspectCost>,
    /// Remarks for the caller (what the state is, what to try next).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A call of `hydo inspect` that failed (added in M178): what was wrong and what would work.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectError {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: InspectErrorType,
    pub op: String,
    /// An open set: `usage`, `no-run`, `no-session`, `bad-point`, `engine`, `agent`,
    /// `timeout`, `refused`.
    pub code: String,
    pub message: String,
    /// Commands that would work, exactly as they can be run.
    #[serde(default, rename = "try", skip_serializing_if = "Vec::is_empty")]
    pub try_: Vec<String>,
}

/// One property over the runs of a table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Property {
    pub id: String,
    /// `always`, `always_or_unreachable`, `sometimes`, `reachable`, `unreachable`, `eventually`.
    pub assertion: String,
    #[serde(default)]
    pub message: String,
    /// Declared in a HYDO-ASSERT-DECL record (the catalog).
    #[serde(default)]
    pub declared: bool,
    /// Evaluations that held, and that did not.
    #[serde(default)]
    pub passes: u64,
    #[serde(default)]
    pub fails: u64,
    /// Runs with at least one such evaluation.
    #[serde(default)]
    pub runs_passed: u64,
    #[serde(default)]
    pub runs_failed: u64,
    /// The table's verdict (`PASS`, `FAIL`, `FAIL (never evaluated)`, `FAIL (reached)`,
    /// `FAIL (deadline missed)`, `NOT EXERCISED`, `NOT EVALUATED`).
    pub verdict: String,
    /// The verdict as a [`status`]: `ok`, `violation`, `not-exercised`, `not-evaluated`.
    pub status: String,
    /// The details of the first failing evaluation, as the record wrote them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_failure: Option<String>,
    /// Eventually properties: the slowest episode that held, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_ok_s: Option<f64>,
    /// Eventually properties: the deadline in seconds after the anchor (the records' `within_s`,
    /// the largest seen; added after M176).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within_s: Option<f64>,
}

/// The property table: every assertion property over a set of runs (hydo/ASSERTIONS.md).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PropertyTable {
    /// Runs whose consoles carried records.
    pub runs: u64,
    /// Of those, runs that printed HYDO-SETUP-COMPLETE.
    #[serde(default)]
    pub setup_complete_runs: u64,
    /// Properties whose verdict is a FAIL.
    #[serde(default)]
    pub violations: u64,
    #[serde(default)]
    pub properties: Vec<Property>,
}

/// One eventually property over an exploration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Liveness {
    pub id: String,
    /// Runs in which every episode held in time, and runs with a missed deadline.
    pub held_runs: u64,
    pub missed_runs: u64,
    /// The slowest run that held: its time and its run (tag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_run: Option<String>,
    /// The first run with a missed deadline, and that episode's details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_miss_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_miss: Option<Value>,
    /// The property's deadline in seconds after its anchor (the records' `within_s`; the largest
    /// when runs differ), so that `slowest_s` can be read against it (added after M176).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within_s: Option<f64>,
    /// The slowest episode that held only after its deadline (the host's `satisfied only <t>s
    /// after ...`): the guest time the property needed in that run, and the run (added after
    /// M176). A deadline of at least this would have held there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_late_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_late_run: Option<String>,
}

/// A run of a schedule again, to confirm it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reproduction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Absent: the run gave no result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// Error kinds: the kind was present in this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present: Option<bool>,
}

/// A failing run of an exploration and its re-runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Failure {
    /// The grouping key: the verdict with numbers as `#` (violations: up to ` order=`).
    pub signature: String,
    /// Every re-run gave the run's fingerprint.
    pub reproduced: bool,
    #[serde(default)]
    pub reproductions: Vec<Reproduction>,
    /// Branch explorations: the failure appears only with the timing change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_only: Option<bool>,
    /// The failing run.
    pub run: Run,
}

/// A failure shrunk to a minimal schedule (one per class and signature).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Shrink {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: ShrinkType,
    pub class: String,
    pub signature: String,
    /// The failure it started from.
    pub from_seed: i64,
    /// The minimal schedule.
    pub minimal: String,
    /// The verdict the minimal schedule kept during the shrink.
    pub verdict: String,
    /// Shrink candidates run (every candidate of a round counts).
    pub tries: u64,
    /// The minimal schedule's own finding class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_class: Option<String>,
    /// The verdict of the first confirmation run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_verdict: Option<String>,
    /// The minimal schedule run twice.
    #[serde(default)]
    pub confirmations: Vec<Reproduction>,
    /// The two confirmations gave the same fingerprint.
    #[serde(default)]
    pub identical: bool,
    /// The command line that runs the minimal schedule once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro_command: Option<String>,
    /// Test composer: the composition of the minimal `W=` in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<String>,
    /// Test composer: the command ids of the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<String>>,
    /// Test composer: each used command's content hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<Map<String, Value>>,
    /// Test composer: the resolved `W=:` form (commands by id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable: Option<String>,
    /// Test composer: the schedule with the resolved form (or `W=@<key>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_schedule: Option<String>,
    /// Test composer: why the minimal schedule keeps the seed form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_note: Option<String>,
}

/// An error kind's first run in which it was new.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FirstNew {
    pub seed: Value,
    pub schedule: String,
    #[serde(default)]
    pub at: Vec<String>,
    #[serde(default)]
    pub example: String,
}

/// A new error kind shrunk to the smallest schedule that still writes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorShrink {
    pub minimal: String,
    pub tries: u64,
    /// The minimal schedule run twice.
    #[serde(default)]
    pub confirmations: Vec<Reproduction>,
    #[serde(default)]
    pub identical: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro_command: Option<String>,
}

/// One error kind over an exploration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorKind {
    /// The first 12 hex digits of sha256("<service>\n<template>").
    pub signature: String,
    pub service: String,
    /// `log`, `setup`, or a file.
    #[serde(default)]
    pub source: String,
    /// The line with its times, numbers, ids and client ports blanked.
    pub template: String,
    /// A line of this kind.
    #[serde(default)]
    pub example: String,
    /// New in at least one run (the known list did not cover it).
    pub new: bool,
    /// Runs that had it, and runs in which it was new.
    pub runs: u64,
    #[serde(default)]
    pub new_runs: u64,
    /// The first 12 seeds that had it.
    #[serde(default)]
    pub seeds: Vec<Value>,
    pub first_seed: Value,
    #[serde(default)]
    pub first_schedule: String,
    #[serde(default)]
    pub first_at: Vec<String>,
    /// Lines of this kind over every run.
    #[serde(default)]
    pub lines: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_new: Option<FirstNew>,
    /// The reason of the known-list entry that covered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shrink: Option<ErrorShrink>,
}

/// The error kinds of an exploration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorKinds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_sha256: Option<String>,
    /// `report` or `fail`.
    pub unknown: String,
    #[serde(default)]
    pub kinds: Vec<ErrorKind>,
}

/// How the exploration was asked for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Config {
    /// The strategy (`random`, `swarm`, `process`, ...), or `sweep:<kind>` for a sweep.
    pub strategy: String,
    /// `--seeds lo-hi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeds: Option<String>,
    /// `--sweep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep: Option<String>,
    /// `--branch`: the marker the runs branch at, and the variants per branch point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_marker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<u64>,
    /// `--schedule`: the one schedule given by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// The explorer's whole command line.
    #[serde(default)]
    pub argv: Vec<String>,
}

/// The numbers of an exploration. Absent fields do not apply to its mode.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    /// Schedules (seeds or sweep points) explored.
    pub schedules: u64,
    /// Branch explorations: branch points found, runs made, timing variants per point and the
    /// tick-jitter permille.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_points: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants_per_point: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tjpm: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep_wall_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_per_hour: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_run_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_boot_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_wall_s: Option<f64>,
    pub failures: u64,
    /// Failures whose every re-run gave the same fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduced: Option<u64>,
    /// Branch explorations: failures reproduced as plain schedules, and failures that need the
    /// timing change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduced_standalone: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing_only_failures: Option<u64>,
    /// Re-runs per failure (when not 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repro_runs: Option<u64>,
    /// Seconds the pool waited for `$HYDO_QUIET_FILE` to go away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_wait_s: Option<f64>,
    /// First runs answered from `$HYDO_EXPLORE_CACHE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hits: Option<u64>,
    /// Error kinds new in some run, and runs with a new kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kinds_new: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_with_new_error_kinds: Option<u64>,
}

/// Where an exploration's files are, relative to its directory.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExplorationArtifacts {
    /// The event stream.
    pub events: String,
    /// The report for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    /// The directory of the guest consoles (`<tag>.log`, `<tag>.recovery.log`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consoles: Option<String>,
}

/// The first record of an exploration's stream.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Start {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: StartType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydo_version: Option<String>,
    /// The fingerprint epoch of that hydo (added in M155; absent: before M155, epoch 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    /// `seeds`, `sweep`, `branch` or `schedule`.
    pub mode: String,
    pub config: Config,
    pub image: Image,
    pub engine: Engine,
}

/// The last record of an exploration's stream: the exploration finished.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct End {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: EndType,
    pub summary: Summary,
}

/// `exploration.json`: an exploration's result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Exploration {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: ExplorationType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydo_version: Option<String>,
    /// The fingerprint epoch of that hydo (added in M155; absent: before M155, epoch 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    /// `seeds`, `sweep`, `branch` or `schedule`.
    pub mode: String,
    pub config: Config,
    pub image: Image,
    pub engine: Engine,
    pub summary: Summary,
    /// `--sweep freeze`: the trace and adaptive stages (their counts, centres, windows, rounds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze_sweep: Option<Map<String, Value>>,
    /// Every failing first run, in seed order.
    #[serde(default)]
    pub failures: Vec<Failure>,
    /// One minimal schedule per class and signature.
    #[serde(default)]
    pub shrinks: Vec<Shrink>,
    /// Images built with `errors:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kinds: Option<ErrorKinds>,
    /// Images whose guests emit HYDO-ASSERT records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<PropertyTable>,
    #[serde(default)]
    pub liveness: Vec<Liveness>,
    pub artifacts: ExplorationArtifacts,
}

/// Where an event of one run is in that run (added in M165): its conversation, the number of the
/// stop that saw it (the control transcript's `n`) and the engine's virtual clock at it (hvx's
/// instruction clock, hvm's exit count).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffPoint {
    /// `boot`, `run` or `recovery`.
    pub conversation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

/// One event of a diff (added in M165): its level, its text, and where it is in each run (absent
/// on the side that does not have it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffLine {
    /// `control` (a command the host typed, an engine action), `controller` (the host
    /// controller's notes and reports), `guest` (a line the guest printed), `assertion`,
    /// `assertion-decl`, `history`, `fake-world`, `error-log`, `composer`, `outcome` (the
    /// acknowledged and failed lists, the end of a conversation) or `verdict`.
    pub level: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<DiffPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<DiffPoint>,
    /// The host controller's last note before it in the run that has it (`hydo-ob: KILLED redis
    /// before step 11`): where in the workload it happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// A place where two runs part (added in M165).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Divergence {
    /// The conversation it is in.
    pub conversation: String,
    /// The level of its first differing event (`timing` for a guest-time divergence).
    pub level: String,
    /// Where it is in run A and in run B (the first differing event, or, on the side that has
    /// none, the next shared event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<DiffPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<DiffPoint>,
    /// The events both runs share right before it, oldest first.
    #[serde(default)]
    pub context: Vec<DiffLine>,
    /// The differing events: those only A has (`a` set) and those only B has (`b` set).
    #[serde(default)]
    pub lines: Vec<DiffLine>,
    /// What differs inside two events of the same kind (`only in B: 11`, `env: only in A: ...`).
    #[serde(default)]
    pub details: Vec<String>,
}

/// One of the two runs of a diff (added in M165).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffSide {
    /// The run directory.
    pub dir: String,
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_sha256: Option<String>,
    pub verdict: Verdict,
    pub fingerprints: Fingerprints,
}

/// How many events of one level one run has and the other does not (added in M165).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffCount {
    pub conversation: String,
    pub level: String,
    pub only_a: u64,
    pub only_b: u64,
}

/// `diff.json`: where two runs first differ (`hydo diff`, `hydo explain`; added in M165). The
/// runs are aligned event by event, per conversation (the control transcripts' commands, notes
/// and reports, and the guest's console lines); `first` is the first event that differs,
/// `timing` the first shared event the two runs reach at another stop or guest time, and
/// `first_outcome` the first difference in what the checks judge (acknowledged lists,
/// assertions, history records, verdicts) among records both runs write.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Diff {
    pub v: V1,
    #[serde(rename = "type")]
    pub ty: DiffType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydo_version: Option<String>,
    pub a: DiffSide,
    pub b: DiffSide,
    /// `hydo explain`: how run B's schedule was made from run A's (`without K=redis@11+1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighbour: Option<String>,
    /// The sources compared (`control`, `console`, `result`).
    #[serde(default)]
    pub levels: Vec<String>,
    /// No difference at the levels compared.
    pub identical: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<Divergence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<Divergence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_outcome: Option<Divergence>,
    /// Records of a kind the other run never writes (an assertion id it never reports), each at
    /// its first occurrence: often code only one of two images has.
    #[serde(default)]
    pub one_sided: Vec<DiffLine>,
    /// Every difference, counted by conversation and level.
    #[serde(default)]
    pub counts: Vec<DiffCount>,
    /// The number of separate places where the runs differ.
    #[serde(default)]
    pub hunks: u64,
}

/// Any version-1 record or document (the schema's root).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AnyV1 {
    Run(Box<Run>),
    Exploration(Box<Exploration>),
    Start(Box<Start>),
    Shrink(Box<Shrink>),
    End(Box<End>),
    Diff(Box<Diff>),
    /// Added in M178.
    Diagnosis(Box<DiagnosisDoc>),
    Timeline(Box<Timeline>),
    Inspect(Box<Inspect>),
    InspectError(Box<InspectError>),
}

impl Run {
    pub fn new(schedule: &str, verdict: Verdict, fp: &str) -> Run {
        Run {
            schedule: schedule.into(),
            verdict,
            fingerprints: Fingerprints {
                fp: fp.into(),
                wfp: None,
                ctl: None,
            },
            ..Run::default()
        }
    }

    /// The findings of one kind.
    pub fn findings_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Finding> + 'a {
        self.findings.iter().filter(move |f| f.kind == kind)
    }
}
