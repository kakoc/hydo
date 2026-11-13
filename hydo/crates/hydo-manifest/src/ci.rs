//! `hydo/ci.yaml`: a project's settings for continuous integration (`hydo ci run`;
//! docs/design/ci.md section 8.2), format 1.
//!
//! The file sits next to `hydo/hydo.yaml` and is **not** staged into the image: if CI settings
//! lived in `hydo.yaml`, changing a budget would change the image's bytes and every fingerprint.
//! Every key is optional; a flag of `hydo ci run` wins over the file. Within format 1 keys are only
//! added (the committed schema `hydo/schema/ci-v1.json` is checked against the released one by
//! `tests/ci_format.rs` with `hydo-schema-compat`); an unknown key is an error, so a misspelled
//! key is never silently ignored, and a newer format is refused first, naming the format it needs.
//!
//! ```yaml
//! format: 1
//! engine: kvm
//! build: ./ci/build-services.sh       # builds what hydo.yaml stages, from this commit
//! inputs:                             # for services built here: what their files depend on
//!   orders: [services/orders/]
//! pr:
//!   seeds: 1-200
//!   strategy: process
//!   options: [--more-faults]
//!   budget: 10m                       # stage 2: stop drawing schedules when the run would not fit
//! fail_on: [new, reappeared, regression]
//! ```

use crate::ManifestError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The newest format of `hydo/ci.yaml` this hydo reads.
pub const FORMAT: u64 = 1;

/// Where `hydo ci run` looks for the file, relative to the project's directory.
pub const DEFAULT_PATH: &str = "hydo/ci.yaml";

/// What fails a CI run (`fail_on`): a finding in one of these states, or a saved regression that
/// is not as expected.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum FailOn {
    /// A finding this change introduced (the base commit passes its minimal schedule).
    New,
    /// A finding that was fixed earlier and fails again (decided from history, stage 2).
    Reappeared,
    /// A saved regression (`hydo/regressions/`) that did not give its expected answer.
    Regression,
    /// A finding the base also has, which the base's own exploration missed.
    PreExisting,
    /// A finding of the base that is still there.
    Present,
    /// A finding of the base whose schedule now fails differently.
    Changed,
    /// A finding whose schedule could not be replayed.
    Unverified,
}

impl FailOn {
    /// The word as written in the file and in `ci-run.json`.
    pub fn word(self) -> &'static str {
        match self {
            FailOn::New => "new",
            FailOn::Reappeared => "reappeared",
            FailOn::Regression => "regression",
            FailOn::PreExisting => "pre-existing",
            FailOn::Present => "present",
            FailOn::Changed => "changed",
            FailOn::Unverified => "unverified",
        }
    }

    /// The word, read back (`--fail-on new,regression`).
    pub fn parse(w: &str) -> Option<FailOn> {
        [
            FailOn::New,
            FailOn::Reappeared,
            FailOn::Regression,
            FailOn::PreExisting,
            FailOn::Present,
            FailOn::Changed,
            FailOn::Unverified,
        ]
        .into_iter()
        .find(|f| f.word() == w)
    }

    /// The default policy: a pull request fails on what it caused, never on what was already
    /// there (docs/design/ci.md section 5.1).
    pub fn default_policy() -> Vec<FailOn> {
        vec![FailOn::New, FailOn::Reappeared, FailOn::Regression]
    }
}

/// What one kind of CI run explores.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// The explorer's seed range (`1-200`). The same range on every run of this kind, so that the
    /// comparison with the base explores the same schedules. Default `1-50`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeds: Option<String>,
    /// The explorer's strategy (`random`, `swarm`, `process`, ...). Default `process`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// More explorer options (`--more-faults`, `--protocol-faults`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The explorer's workers; default: sized from the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<u64>,
    /// The wall time the whole CI run may take (`10m`, `90s`, `1h30m`; added in stage 2). The
    /// exploration stops drawing new schedules when the rest of the run would not fit, at a seed
    /// boundary in seed order, and the run reports the seeds it explored. Default: no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
}

/// `hydo/ci.yaml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CiConfig {
    /// The format of this file: 1. A file without it is read as format 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<u64>,
    /// The manifest, relative to the project's directory. Default `hydo/hydo.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// The engine: `kvm` (stock Linux KVM: CI runners), `hvx` (the kernel module) or `hvm` (an
    /// Apple Silicon Mac). Default: `hvm` on a Mac, `kvm` on Linux.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// A shell command run in the project's directory before `hydo build`, at the head commit and
    /// again in the base commit's checkout: it builds what the manifest stages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// `hydo build --set KEY=VALUE` for the manifest's `params:`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    /// For files the `build` command makes: the repository paths (files or directories) they are
    /// made from, per service or any name. Their git tree hashes go into the image input key, so
    /// an unchanged input reuses the cached image without building it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, Vec<String>>,
    /// What a pull request's run explores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<Budget>,
    /// What a run on a push to a branch explores. Default: as `pr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<Budget>,
    /// What a scheduled run explores. Default: as `push`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nightly: Option<Budget>,
    /// What fails the run. Default `[new, reappeared, regression]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_on: Option<Vec<FailOn>>,
}

impl CiConfig {
    /// The budget of a mode (`pr`, `push`, `nightly`), with the fallbacks above.
    pub fn budget(&self, mode: &str) -> Budget {
        let pick = match mode {
            "nightly" => self
                .nightly
                .as_ref()
                .or(self.push.as_ref())
                .or(self.pr.as_ref()),
            "push" => self.push.as_ref().or(self.pr.as_ref()),
            _ => self.pr.as_ref(),
        };
        pick.cloned().unwrap_or_default()
    }

    /// The policy, with the default.
    pub fn fail_on(&self) -> Vec<FailOn> {
        self.fail_on.clone().unwrap_or_else(FailOn::default_policy)
    }
}

fn err(m: String) -> ManifestError {
    ManifestError { message: m }
}

/// Read `hydo/ci.yaml`'s text (`name` for messages).
pub fn parse(text: &str, name: &str) -> Result<CiConfig, ManifestError> {
    let v = crate::yaml::parse_yaml(text, name)?;
    from_value(v, name)
}

/// The settings from their data (YAML read as JSON values).
pub fn from_value(v: Value, name: &str) -> Result<CiConfig, ManifestError> {
    let v = if v.is_null() {
        Value::Object(Default::default())
    } else {
        v
    };
    if !v.is_object() {
        return Err(err(format!("{name}: CI settings must be a mapping")));
    }
    // a newer format first: its keys would otherwise be reported as unknown
    if let Some(f) = v.get("format") {
        match f.as_u64() {
            Some(n) if n <= FORMAT && n >= 1 => {}
            Some(n) => {
                return Err(err(format!(
                    "{name}: format {n}: this hydo reads hydo/ci.yaml format {FORMAT}; a newer hydo is needed"
                )))
            }
            None => return Err(err(format!("{name}: format must be a whole number"))),
        }
    }
    let c: CiConfig = serde_path_to_error::deserialize(v)
        .map_err(|e| err(format!("{name}: {}: {}", e.path(), e.inner())))?;
    if let Some(e) = c.engine.as_deref() {
        if !matches!(e, "kvm" | "hvx" | "hvm") {
            return Err(err(format!(
                "{name}: engine: {e:?} is not one of kvm, hvx, hvm"
            )));
        }
    }
    for (m, b) in [("pr", &c.pr), ("push", &c.push), ("nightly", &c.nightly)] {
        if let Some(s) = b.as_ref().and_then(|b| b.seeds.as_deref()) {
            if parse_seeds(s).is_none() {
                return Err(err(format!(
                    "{name}: {m}.seeds: {s:?} is not a range like 1-200"
                )));
            }
        }
        if let Some(s) = b.as_ref().and_then(|b| b.budget.as_deref()) {
            if parse_duration(s).is_none() {
                return Err(err(format!(
                    "{name}: {m}.budget: {s:?} is not a duration like 10m, 90s or 1h30m"
                )));
            }
        }
    }
    Ok(c)
}

/// A duration in seconds: `600`, `90s`, `10m`, `2h`, `1h30m`, `2m30s` (more than zero).
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<f64>() {
        return (n.is_finite() && n > 0.0).then_some(n);
    }
    let (mut total, mut num) = (0.0, String::new());
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let n: f64 = num.parse().ok()?;
        num.clear();
        total += n * match c {
            'h' => 3600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
    }
    (num.is_empty() && total > 0.0).then_some(total)
}

/// A seed range `lo-hi` (or one seed), as the explorer reads it.
pub fn parse_seeds(s: &str) -> Option<(u64, u64)> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (a >= 1 && a <= b).then_some((a, b))
}

/// The JSON Schema of `hydo/ci.yaml`, generated from the types (committed as
/// `hydo/schema/ci-v1.json`).
pub fn schema() -> Value {
    let g = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let s = g.into_root_schema_for::<CiConfig>();
    let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
    if let Value::Object(o) = &mut v {
        o.insert(
            "title".into(),
            Value::String("hydo/ci.yaml, format 1".into()),
        );
        o.insert(
            "$comment".into(),
            Value::String(
                "Generated from hydo-manifest's CI settings (hydo/crates/hydo-manifest/src/ci.rs); do not edit. Within format 1 keys are only added. HYDO_BLESS=1 cargo test -p hydo-manifest --test it ci_format:: rewrites it."
                    .into(),
            ),
        );
    }
    v
}

/// The schema as the committed file holds it.
pub fn schema_text() -> String {
    serde_json::to_string_pretty(&schema()).unwrap_or_default() + "\n"
}
