//! The project format as types (M114): one document model for `hydo.yaml`, a JSON plan, a plan's
//! `test:` section and the `hydo/` folder. The JSON Schema in `hydo/schema/project-v1.json` is
//! generated from these types (`schema()`), and the permanent example documents in
//! `hydo/schema/examples/project/v1/` are read into them (`read_project`); the tests
//! `tests/format.rs` keep both fixed. The validator (`validate.rs`, `composer.rs`) stays the
//! authority for value rules (names, ranges, references between keys), which are checked after
//! `params:` are replaced; these types fix the keys, their nesting and their kinds of value.
//!
//! Rules of the format (MANIFEST.md "The project format"): a document says `format: 1` (a
//! document without `format:` is read as format 1); within a format version keys are only ever
//! added, never renamed, removed or given another meaning; unknown keys are errors; a breaking
//! change is a new format version, read beside the old one, and `hydo migrate` rewrites old files.
//!
//! Every string value may carry a `{{param}}` reference (`params:`), so a field with a fixed set
//! of values also takes a string that contains one ([`Param`]).

use indexmap::IndexMap;
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::borrow::Cow;

use crate::py::{repr, truthy};
use crate::ManifestError;

/// The project format this hydo reads and writes.
pub const FORMAT: u64 = 1;

/// Every project format there is, with the first hydo release that reads it.
pub const FORMATS: &[(u64, &str)] = &[(1, "0.1.0")];

/// The committed schema of [`FORMAT`], relative to the repository root.
pub const SCHEMA_PATH: &str = "hydo/schema/project-v1.json";

fn err(msg: impl Into<String>) -> ManifestError {
    ManifestError {
        message: msg.into(),
    }
}

// ------------------------------------------------------------------ leaf values

/// A field with a fixed set of values.
pub trait Choice: DeserializeOwned + Serialize + JsonSchema + Copy {
    const NAMES: &'static [&'static str];
}

macro_rules! choice {
    ($(#[$m:meta])* $name:ident { $($var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            $(#[serde(rename = $s)] $var),+
        }
        impl Choice for $name {
            const NAMES: &'static [&'static str] = &[$($s),+];
        }
    };
}

const PARAM_PATTERN: &str = r"\{\{[A-Za-z_][A-Za-z0-9_]*\}\}";

fn has_param_ref(s: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(PARAM_PATTERN).unwrap())
        .is_match(s)
}

/// One of a field's values, or a string with a `{{param}}` reference that `params:` (or `hydo
/// build --set`) replaces before the value is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Param<T> {
    Value(T),
    Ref(String),
}

impl<T: Choice> Param<T> {
    pub fn value(&self) -> Option<T> {
        match self {
            Param::Value(v) => Some(*v),
            Param::Ref(_) => None,
        }
    }
}

impl<'de, T: Choice> Deserialize<'de> for Param<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        if let Value::String(s) = &v {
            if has_param_ref(s) {
                return Ok(Param::Ref(s.clone()));
            }
        }
        T::deserialize(v.clone()).map(Param::Value).map_err(|_| {
            D::Error::custom(format!(
                "one of {} (or a {{{{param}}}} reference), not {}",
                T::NAMES.join(", "),
                repr(&v)
            ))
        })
    }
}

impl<T: Choice> Serialize for Param<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Param::Value(v) => v.serialize(s),
            Param::Ref(r) => r.serialize(s),
        }
    }
}

impl<T: Choice> JsonSchema for Param<T> {
    fn schema_name() -> Cow<'static, str> {
        format!("{}OrParam", T::schema_name()).into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "anyOf": [
                g.subschema_for::<T>(),
                g.subschema_for::<ParamRef>(),
            ]
        })
    }
}

/// A string with a `{{param}}` reference (only in the schema: [`Param`] keeps it as a string).
pub struct ParamRef;

impl JsonSchema for ParamRef {
    fn schema_name() -> Cow<'static, str> {
        "ParamRef".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A string with a {{param}} reference, replaced by params: before the value is checked.",
            "type": "string",
            "pattern": PARAM_PATTERN,
        })
    }
}

/// A string, or a whole number (the YAML subset reads bare digits as a number; a command or a
/// name gets its digits).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Text {
    Str(String),
    Int(serde_json::Number),
}

impl Default for Text {
    fn default() -> Self {
        Text::Str(String::new())
    }
}

impl Text {
    pub fn as_string(&self) -> String {
        match self {
            Text::Str(s) => s.clone(),
            Text::Int(n) => n.to_string(),
        }
    }
}

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(s) => Ok(Text::Str(s)),
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok(Text::Int(n)),
            other => Err(D::Error::custom(format!(
                "a string (or a whole number), not {}",
                repr(&other)
            ))),
        }
    }
}

impl Serialize for Text {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Text::Str(x) => x.serialize(s),
            Text::Int(n) => n.serialize(s),
        }
    }
}

impl JsonSchema for Text {
    fn schema_name() -> Cow<'static, str> {
        "Text".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A string, or a whole number (written as its digits).",
            "type": ["string", "integer"],
        })
    }
}

/// A string, a number or true/false (free text and environment values).
#[derive(Clone, Debug, PartialEq)]
pub struct Scalar(pub Value);

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            v @ (Value::String(_) | Value::Number(_) | Value::Bool(_)) => Ok(Scalar(v)),
            other => Err(D::Error::custom(format!(
                "a string, a number or true/false, not {}",
                repr(&other)
            ))),
        }
    }
}

impl Serialize for Scalar {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl JsonSchema for Scalar {
    fn schema_name() -> Cow<'static, str> {
        "Scalar".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A string, a number or true/false.",
            "type": ["string", "number", "boolean"],
        })
    }
}

/// A service's restart policy: a string (Docker Compose's `restart:` words, checked by the
/// validator after `params:` are replaced: `crate::restart`), or false (a bare YAML `no`).
#[derive(Clone, Debug, PartialEq)]
pub struct RestartPolicy(pub Value);

impl<'de> Deserialize<'de> for RestartPolicy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            v @ (Value::String(_) | Value::Bool(false)) => Ok(RestartPolicy(v)),
            other => Err(D::Error::custom(format!(
                "no, always, on-failure, on-failure:<max> or unless-stopped (a string), not {}",
                repr(&other)
            ))),
        }
    }
}

impl Serialize for RestartPolicy {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl JsonSchema for RestartPolicy {
    fn schema_name() -> Cow<'static, str> {
        "RestartPolicy".into()
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Docker Compose's restart: words: no (the default), always, unless-stopped, on-failure or on-failure:<max> (0 = no limit); false (a bare YAML no) means no.",
            "anyOf": [
                {"type": "string", "pattern": "^(no|always|unless-stopped|on-failure(:[0-9]+)?)$"},
                {"const": false},
                g.subschema_for::<ParamRef>(),
            ]
        })
    }
}

/// A version pin: "0.1", "0.1.0" or ">=0.1" (a bare 0.1 also reads as that version).
#[derive(Clone, Debug, PartialEq)]
pub struct VersionPin(pub Value);

impl<'de> Deserialize<'de> for VersionPin {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            v @ (Value::String(_) | Value::Number(_)) => Ok(VersionPin(v)),
            other => Err(D::Error::custom(format!(
                "a version such as \"0.1\", \"0.1.0\" or \">=0.1\", not {}",
                repr(&other)
            ))),
        }
    }
}

impl Serialize for VersionPin {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

impl JsonSchema for VersionPin {
    fn schema_name() -> Cow<'static, str> {
        "VersionPin".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "The hydo the project was made for: \"0.1\" (same major.minor), \"0.1.0\", or \">=0.1\".",
            "type": ["string", "number"],
        })
    }
}

/// A command: an argv list (exec'd, no shell), or a shell command string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandLine {
    Shell(Text),
    Argv(Vec<Text>),
}

impl Default for CommandLine {
    fn default() -> Self {
        CommandLine::Shell(Text::default())
    }
}

impl<'de> Deserialize<'de> for CommandLine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bad = |v: &Value| {
            D::Error::custom(format!(
                "an argv list of strings (exec'd, no shell) or a shell command string, not {}",
                repr(v)
            ))
        };
        match Value::deserialize(d)? {
            Value::String(s) => Ok(CommandLine::Shell(Text::Str(s))),
            Value::Number(n) if n.is_i64() || n.is_u64() => Ok(CommandLine::Shell(Text::Int(n))),
            Value::Array(a) => {
                let mut out = Vec::new();
                for x in &a {
                    match x {
                        Value::String(s) => out.push(Text::Str(s.clone())),
                        Value::Number(n) if n.is_i64() || n.is_u64() => {
                            out.push(Text::Int(n.clone()))
                        }
                        _ => return Err(bad(&Value::Array(a.clone()))),
                    }
                }
                Ok(CommandLine::Argv(out))
            }
            other => Err(bad(&other)),
        }
    }
}

impl Serialize for CommandLine {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CommandLine::Shell(x) => x.serialize(s),
            CommandLine::Argv(a) => a.serialize(s),
        }
    }
}

impl JsonSchema for CommandLine {
    fn schema_name() -> Cow<'static, str> {
        "CommandLine".into()
    }
    fn json_schema(g: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "An argv list (exec'd, no shell), or a shell command string.",
            "anyOf": [
                g.subschema_for::<Text>(),
                {"type": "array", "items": g.subschema_for::<Text>()},
            ]
        })
    }
}

// ------------------------------------------------------------------ fixed sets of values

choice!(
    /// How the services reach each other.
    Network { Loopback = "loopback", Bridge = "bridge" }
);
choice!(
    /// The guest CPU level (x86_64 only).
    CpuLevel { V2 = "v2", V3 = "v3" }
);
choice!(
    /// A fault kind the explorer may use.
    FaultKind {
        Kill = "kill",
        Pause = "pause",
        Network = "network",
        Oneway = "oneway",
        Cut = "cut",
        Clock = "clock",
        Disk = "disk",
        Freeze = "freeze",
        Protocol = "protocol",
    }
);
choice!(
    /// What kind of property an invariant is.
    InvariantType {
        Always = "always",
        AlwaysOrUnreachable = "always_or_unreachable",
        Eventually = "eventually",
    }
);
choice!(
    /// Where an eventually property's deadline counts from.
    Anchor { Heal = "heal", Start = "start" }
);
choice!(
    /// A ready-made history checker.
    HistoryType { Linearizable = "linearizable", ListAppend = "list-append" }
);
choice!(
    /// The object a linearizable history is checked against.
    HistoryModel { Register = "register", Cas = "cas" }
);
choice!(
    /// What a key holds before the first write.
    HistoryInitial { Nil = "nil", Any = "any" }
);
choice!(
    /// The consistency level a list-append history is checked against.
    Consistency {
        StrictSerializable = "strict-serializable",
        StrongSessionSerializable = "strong-session-serializable",
        Serializable = "serializable",
        StrongSnapshotIsolation = "strong-snapshot-isolation",
        StrongSessionSnapshotIsolation = "strong-session-snapshot-isolation",
        SnapshotIsolation = "snapshot-isolation",
    }
);
choice!(
    /// A fake outside service's protocol.
    FakeProtocol { Http = "http", Grpc = "grpc" }
);
choice!(
    /// A fault proxy's protocol.
    ProxyProtocol { Http = "http", Grpc = "grpc", Tcp = "tcp" }
);
choice!(
    /// Which log lines are error lines.
    ErrorLevel { Error = "error", Warning = "warning" }
);
choice!(
    /// How a service's log is read.
    ErrorFormat { Auto = "auto", Redis = "redis", None = "none" }
);
choice!(
    /// When a composed command runs.
    CommandKind {
        First = "first",
        ParallelDriver = "parallel_driver",
        SerialDriver = "serial_driver",
        SingletonDriver = "singleton_driver",
        Anytime = "anytime",
        Eventually = "eventually",
        Finally = "finally",
    }
);
choice!(
    /// When a composed check runs. linearizable, list_append and list-append are retired here
    /// (they are history checks: top-level `checks:`); format 1 still reads them, and `hydo
    /// migrate` moves them.
    TestCheckKind {
        Final = "final",
        Always = "always",
        Eventually = "eventually",
        Linearizable = "linearizable",
        ListAppend = "list_append",
        ListAppendDash = "list-append",
    }
);

impl TestCheckKind {
    /// A kind that is a history check written in `test.checks` (retired in format 1).
    pub fn is_history(self) -> bool {
        matches!(
            self,
            TestCheckKind::Linearizable | TestCheckKind::ListAppend | TestCheckKind::ListAppendDash
        )
    }
}

// ------------------------------------------------------------------ the document

/// A hydo project, format 1: what runs in the guest, the workload, the properties, the faults that
/// make sense, and the test composer's plan. Experimental until hydo 1.0; within format 1 keys are
/// only added. String values may contain {{param}} references (params:).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
#[schemars(title = "hydo project (format 1)")]
pub struct Project {
    /// The project format: 1. A document without format: is read as format 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("const" = 1))]
    pub format: Option<u64>,
    /// The hydo the project was made for (checked against hydo's own version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydo_version: Option<VersionPin>,
    /// The project's name: lowercase letters, digits and '-'.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<Guest>,
    /// loopback (default): one network for everything; bridge: each service in its own network
    /// namespace at its addr:.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Param<Network>>,
    /// What goes on the read-only root filesystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Root>,
    /// The read-write data disk, mounted on /data: the fault target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Data>,
    /// Exported to every command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, Scalar>>,
    /// Shell helpers, sourced before every command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Text>,
    /// Build parameters: {{name}} in any string value is replaced by the default here or by
    /// `hydo build --set name=value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<IndexMap<String, Option<Scalar>>>,
    /// The long-running processes, started in dependency order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<Service>>,
    /// Run once after every service is ready, before the snapshot every schedule starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision: Option<Text>,
    /// The workload in numbered steps (with test commands: only steps and timeout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload: Option<Workload>,
    /// Durability settings the explorer may relax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knobs: Option<Vec<Knob>>,
    /// Properties as shell commands, run by the init.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invariants: Option<Vec<Invariant>>,
    /// Ready-made checkers of recorded histories, run on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<HistoryCheck>>,
    /// The fault kinds that make sense for this project (default: all but freeze and protocol).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faults: Option<Vec<Param<FaultKind>>>,
    /// Payload strings that mark the packets a freeze sweep tries first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze_match: Option<FreezeMatch>,
    /// Fake outside services, served and recorded in the guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fakes: Option<Vec<Fake>>,
    /// Fault proxies in front of real services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxies: Option<Vec<Proxy>>,
    /// Error logs as findings: true, or a mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<ErrorsSetting>,
    /// The programs carry BUGGIFY fault points (the schedule token B= sets them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buggify: Option<bool>,
    /// The test composer's plan (the hydo/ folder fills it too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<Test>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Guest {
    /// Guest RAM in MiB: 256..3072 on x86_64, 256..4096 on arm64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 1024))]
    pub ram_mib: Option<i64>,
    /// Unix seconds the realtime clock starts at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 1767225600))]
    pub clock_start: Option<i64>,
    /// After a power cut the recovery boot's clock continues from the cut plus this many seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_after_cut: Option<i64>,
    /// After a power cut the recovery boot's $HYDO_OUTCOMES starts with the first boot's last
    /// outcome of every id (without it the file starts empty there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = false))]
    pub outcomes_after_cut: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "v2"))]
    pub cpu: Option<Param<CpuLevel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Root {
    /// Default: 1.3 x the content, rounded up to 64 MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_mib: Option<i64>,
    /// Nix store paths; each full closure is copied to the same path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nix: Option<Vec<Text>>,
    /// Host files or directories copied into the guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<RootFile>>,
    /// Small files written inline: guest path -> content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<IndexMap<String, Text>>,
    /// Empty directories to create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirs: Option<Vec<Text>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct RootFile {
    /// Relative to the manifest, or ~ / absolute.
    pub src: Text,
    /// The guest path.
    pub dst: Text,
    /// Octal permission bits, e.g. "0755".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Text>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Data {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 512))]
    pub size_mib: Option<i64>,
    /// ext4 mount options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "rw"))]
    pub mount: Option<Text>,
    /// Created under /data on every mount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirs: Option<Vec<Text>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Service {
    /// [a-z][a-z0-9_]*: also the name fault tokens use (K=<name>@...).
    pub name: String,
    /// Runs in the foreground (an image service: replaces the image's Cmd; a list = argv).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<CommandLine>,
    /// Run before every start (also restarts and crash recovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<Text>,
    /// Exit 0 = ready; polled once per second.
    pub ready: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 120))]
    pub ready_timeout: Option<i64>,
    /// The port network faults act on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i64>,
    /// The service's own address (loopback 127.x.y.z, or 10.89.x.y with network: bridge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<Text>,
    /// Start after these services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<Text>>,
    /// false: not started for the power-cut recovery check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = true))]
    pub recovery: Option<bool>,
    /// What hydo does when the service's process ends by itself while the test runs, in Docker
    /// Compose's words: no (the default: it stays down until the heal), always, unless-stopped,
    /// on-failure, on-failure:<max>. Followed by agent images (MANIFEST.md "Restart policies").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartPolicy>,
    /// true: the restart at the heal (after the faults, before workload.retry) may fail. If the
    /// service does not come back then, the run goes on without workload.retry; a power cut at
    /// the k-th flush (cf=) still reaches its recovery boot, which gives the verdict; without a
    /// cut the verdict is ERROR naming the service. Default false: a service that does not come
    /// back is at once ERROR (MANIFEST.md "A restart after the faults that may fail").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = false))]
    pub restart_may_fail: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
    /// This service's error-log settings (needs the top-level errors:).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<ServiceErrors>,
    /// A Docker/OCI image file the service's files come from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageSource>,
    /// Which image in the file: a tag or sha256:<digest>.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_ref: Option<Text>,
    /// Replaces the image's Entrypoint (and drops its Cmd).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<CommandLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<Text>,
    /// Added to or overriding the image's Env.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, Option<Scalar>>>,
    /// Bind, volume and tmpfs mounts of an image service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounts: Option<Vec<Mount>>,
}

/// A Docker/OCI image: a `docker save` tar or an OCI layout directory, or one per architecture.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "an image path, or a mapping x86_64: / arm64: of image paths"
)]
pub enum ImageSource {
    Path(String),
    PerArch(ImagePerArch),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct ImagePerArch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x86_64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm64: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Mount {
    /// A guest path (bind or volume); or give tmpfs: true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<Text>,
    /// The path in the image.
    pub dst: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmpfs: Option<bool>,
    /// Copy the image's files into src on first use (a volume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_up: Option<bool>,
    /// tmpfs size, e.g. 64m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Text>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Workload {
    /// Faults are placed at step numbers (1..1000; with test commands: rounds, default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<i64>,
    /// Runs steps [HYDO_FROM, HYDO_TO); not with test commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Text>,
    /// Runs after the faults heal (no power cut).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<Text>,
    /// Seconds per run/retry call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 300))]
    pub timeout: Option<i64>,
    /// A pgrep -f pattern telling whether a request is in flight (default: run's first word).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Knob {
    /// One capital letter, not a built-in schedule token.
    pub token: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Text>,
    /// The value the image starts with (the safe one).
    pub default: Text,
    /// Sets the value $HYDO_VALUE.
    pub set: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<Scalar>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe: Option<Scalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Invariant {
    /// [A-Za-z0-9_.-]+, unique among invariants and checks.
    pub id: Text,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "always"))]
    pub kind: Option<Param<InvariantType>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<Text>,
    /// Exit 0 = holds, 1 = violated, 2 = could not be evaluated now.
    pub cmd: Text,
    /// Also check every this many seconds of guest time during the workload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<i64>,
    /// Check at the end (default: true unless every: is given).
    #[serde(rename = "final", default, skip_serializing_if = "Option::is_none")]
    pub final_: Option<bool>,
    /// type: eventually: the deadline in seconds of guest time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<i64>,
    /// type: eventually: the anchor of the deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "heal"))]
    pub after: Option<Param<Anchor>>,
    /// type: eventually: seconds between checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 1))]
    pub poll: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

/// A history check: the workload records operations (hydo_hist), the host checks them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct HistoryCheck {
    pub id: Text,
    #[serde(rename = "type")]
    pub kind: Param<HistoryType>,
    /// The history the workload records under.
    pub history: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "register"))]
    pub model: Option<Param<HistoryModel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "nil"))]
    pub initial: Option<Param<HistoryInitial>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "strict-serializable"))]
    pub consistency: Option<Param<Consistency>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<Text>,
    /// A shell command that reads the final state back into the history.
    #[serde(rename = "final", default, skip_serializing_if = "Option::is_none")]
    pub final_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

impl Default for Param<HistoryType> {
    fn default() -> Self {
        Param::Value(HistoryType::Linearizable)
    }
}

impl Default for Param<CommandKind> {
    fn default() -> Self {
        Param::Value(CommandKind::ParallelDriver)
    }
}

impl Default for Param<TestCheckKind> {
    fn default() -> Self {
        Param::Value(TestCheckKind::Final)
    }
}

/// Payload strings for the freeze sweep: a list, or replies:/requests: lists.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "a list of payload strings, or a mapping with lists replies: and requests:"
)]
pub enum FreezeMatch {
    List(Vec<String>),
    Split(FreezeSplit),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct FreezeSplit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replies: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<Vec<String>>,
}

/// A fake outside service (HTTP, or cleartext HTTP/2 gRPC).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Fake {
    pub name: Text,
    pub port: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "http"))]
    pub protocol: Option<Param<FakeProtocol>>,
    /// Where a request's idempotency key is (an extractor such as header:Idempotency-Key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<Text>,
    /// Where a request's reference is (an extractor such as json:order).
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<FakeRoute>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct FakeRoute {
    pub name: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Text>,
    /// The HTTP status of the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    /// The gRPC status code of the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, Text>>,
    /// The answer body (a template).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Text>,
    /// gRPC: the answer message's protobuf bytes in hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Text>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Text>,
    /// The route has an effect in the outside world (a charge, an email).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<bool>,
    /// The same idempotency key again gets the first answer and no new effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<Webhook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Webhook {
    /// http://<address>:<port>/<path> of the system.
    pub url: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<IndexMap<String, Text>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

/// A fault proxy in front of a real service.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Proxy {
    pub name: Text,
    pub port: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "http"))]
    pub protocol: Option<Param<ProxyProtocol>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<Text>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routes: Option<Vec<ProxyRoute>>,
    /// <address>:<port> of the real service (or give service:).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Text>,
    /// The service (with a port:) the proxy stands in front of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct ProxyRoute {
    pub name: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Text>,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

/// Error logs as findings: true (every default), false (off), or a mapping.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "true, false, or a mapping (level, max, services)"
)]
pub enum ErrorsSetting {
    On(bool),
    Config(ErrorsConfig),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct ErrorsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "error"))]
    pub level: Option<Param<ErrorLevel>>,
    /// The most error kinds one boot prints, 1..200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 40))]
    pub max: Option<i64>,
    /// The services whose logs are scanned (default: every service).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct ServiceErrors {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = "auto"))]
    pub format: Option<Param<ErrorFormat>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<Param<ErrorLevel>>,
    /// Also an error line (POSIX ERE).
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub match_: Option<String>,
    /// Never an error line (POSIX ERE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore: Option<String>,
    /// More log files the service writes (absolute paths).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// More parts to blank out of the template.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalize: Option<Vec<Normalize>>,
    /// true: not this service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

/// A regular expression (replaced by *), or {pattern, replace}.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "a regular expression (replaced by *), or {pattern, replace}"
)]
pub enum Normalize {
    Pattern(String),
    Rule(NormalizeRule),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct NormalizeRule {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

/// The test composer's plan: small commands composed into seeded rounds (the schedule token W=),
/// properties, and saved regressions. The hydo/ folder fills commands, checks and regressions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct Test {
    /// At most this many commands at once (1..16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 4))]
    pub parallel: Option<i64>,
    /// Parallel drivers started per round (1..8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 2))]
    pub per_round: Option<i64>,
    /// A round every this many ms of guest time (10..60000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 200))]
    pub round_ms: Option<i64>,
    /// Seconds per command (1..3600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 60))]
    pub command_timeout: Option<i64>,
    /// After the last round, running commands get this long (1..3600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 60))]
    pub drain_timeout: Option<i64>,
    /// Default period of always checks (1..3600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 2))]
    pub check_every: Option<i64>,
    /// Default deadline of eventually checks (1..3600).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 30))]
    pub check_within: Option<i64>,
    /// 0 = no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 0))]
    pub max_commands: Option<i64>,
    /// 0 = none; else no command starts later than this after round 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("default" = 0))]
    pub budget_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<TestCommand>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<TestCheck>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regressions: Option<Vec<TestRegression>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

/// A composed command; its id is <kind>_<name>.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct TestCommand {
    /// Letters, digits, '.', '_', '-'.
    pub name: Text,
    pub kind: Param<CommandKind>,
    pub run: CommandLine,
    /// Seconds (default: test.command_timeout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i64>,
    /// 8 to 64 lowercase hex digits: a hash of the command's code (an SDK computes it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

/// A composed check; its name is its property id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct TestCheck {
    pub name: Text,
    pub kind: Param<TestCheckKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<CommandLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<Text>,
    /// kind always: seconds between checks (default test.check_every).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<i64>,
    /// kind eventually: the deadline (default test.check_within).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Param<Anchor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
    /// Retired (a history check written here; top-level checks: is its home).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("deprecated" = true))]
    pub history: Option<Text>,
    /// Retired (a history check written here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("deprecated" = true))]
    pub model: Option<Text>,
    /// Retired (a history check written here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("deprecated" = true))]
    pub initial: Option<Text>,
    /// Retired (a history check written here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("deprecated" = true))]
    pub consistency: Option<Text>,
}

/// A saved schedule replayed by `hydo repro --regressions`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, expecting = "a mapping")]
pub struct TestRegression {
    /// Default r<index>.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Text>,
    /// '<host tokens> | run <guest tokens>'.
    pub schedule: String,
    /// ok (default), violation, or fp=<16 hex digits>.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Text>,
    /// The command ids at save time (seed-form W= regressions written before M98).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<Text>>,
    /// The resolved composition a W=@<key> names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose: Option<Text>,
    /// Command id -> content hash at save time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<IndexMap<String, Text>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_schedule: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Scalar>,
}

// ------------------------------------------------------------------ reading

/// The format of a document (its `format:`; a document without one is format 1), or a refusal
/// that names the format the document needs. `have` is this hydo's version, for the message.
pub fn check_format(doc: &Value, have: Option<&str>) -> Result<u64, ManifestError> {
    let Some(f) = doc.as_object().and_then(|o| o.get("format")) else {
        return Ok(FORMAT);
    };
    let this = match have {
        Some(v) => format!("this hydo ({v})"),
        None => "this hydo".to_string(),
    };
    match f.as_u64() {
        Some(n) if n == FORMAT => Ok(n),
        Some(n) if n > FORMAT => {
            let pin = doc
                .get("hydo_version")
                .filter(|v| truthy(v))
                .map(|v| format!("; the file asks for hydo_version {}", crate::py::str_of(v)))
                .unwrap_or_default();
            Err(err(format!(
                "format: this file is written in project format {n}, and {this} reads project format {FORMAT} only; install a newer hydo, one that reads format {n}{pin}"
            )))
        }
        _ => Err(err(format!(
            "format: the project format, a whole number; {this} reads format {FORMAT} (got {})",
            repr(f)
        ))),
    }
}

/// The document read into the model: its keys, nesting and kinds of value (not the value rules,
/// which the validator checks). The message names the place: `services[0].mounts[1]: ...`.
pub fn from_value(doc: &Value) -> Result<Project, ManifestError> {
    serde_path_to_error::deserialize::<_, Project>(doc).map_err(|e| {
        let path = e.path().to_string();
        let wh = if path == "." || path.is_empty() {
            "top level".to_string()
        } else {
            path
        };
        err(format!(
            "{wh}: {} (project format {FORMAT}; the schema is {SCHEMA_PATH})",
            plain_words(&e.inner().to_string())
        ))
    })
}

/// serde's words for kinds of value, in the manifest's words.
fn plain_words(m: &str) -> String {
    let mut s = m.to_string();
    for (from, to) in [
        ("expected i64", "expected a whole number"),
        ("expected u64", "expected a whole number"),
        ("expected a sequence", "expected a list"),
        ("invalid type: sequence", "invalid type: list"),
        ("expected a boolean", "expected true or false"),
    ] {
        s = s.replace(from, to);
    }
    static MAP: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re =
        MAP.get_or_init(|| regex::Regex::new(r"\b(invalid type: map|expected a map)\b").unwrap());
    re.replace_all(&s, "${1}ping").into_owned()
}

/// The model as canonical JSON (keys in the model's order, absent keys left out).
pub fn to_json(p: &Project) -> Value {
    serde_json::to_value(p).unwrap_or(Value::Null)
}

/// The JSON Schema of the format, generated from the types.
pub fn schema() -> Value {
    let g = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let s = g.into_root_schema_for::<Project>();
    let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
    if let Value::Object(o) = &mut v {
        o.insert(
            "$comment".into(),
            Value::String(format!(
                "Generated from hydo-manifest's model (hydo/crates/hydo-manifest/src/model.rs); do not edit. Project format {FORMAT}, experimental until hydo 1.0: within format {FORMAT} keys are only added. HYDO_BLESS=1 cargo test -p hydo-manifest --test format rewrites it."
            )),
        );
    }
    v
}

/// The schema as the committed file holds it.
pub fn schema_text() -> String {
    serde_json::to_string_pretty(&schema()).unwrap_or_default() + "\n"
}
