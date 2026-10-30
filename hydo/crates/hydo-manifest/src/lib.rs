//! hydo's project format as types: `hydo.yaml` (or a JSON plan), a plan's `test:` section and the
//! `hydo/` folder next to the manifest.
//!
//! This crate is the Rust port of what `hydo/onboard/hydo-build` does before it builds: read the
//! file (its YAML subset, or JSON), apply `params:` and `--set`, expand the test composer's plan
//! (hydo/onboard/hydo_composer.py: `check_version`, `scan_folder`, `expand`), `validate`,
//! `check_ram_arch`, `service_order` and the image checks of `_oci_prepare` that need no image.
//! It accepts and refuses the same manifests with the same messages and gives the same normalised
//! data; `tests/equivalence.rs` proves that against hydo-build on every manifest in the repository.
//!
//! API contract other crates rely on (keep these names and signatures):
//! - `parse_text(text, name, by_content) -> Result<serde_json::Value, ManifestError>`
//! - `Guest::from_value(&Value)`: the `guest:` section with hydo-build's defaults
//! - `ManifestError`: `Display` gives hydo-build's message (the CLI prints `hydo-build: <msg>`)
//! - `load`, `from_embedded`, `check_summary` (below)
//!
//! Where hydo-build itself stops with a Python exception (a traceback, not a ManifestError), the
//! input is refused here too, with a message that starts with [`py::CRASH_PREFIX`].

pub mod composer;
mod oci;
pub mod py;
mod pyre;
mod rx;
mod types;
mod validate;
pub mod yaml;

use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};

pub use composer::{regression_status, Status};
pub use types::{
    CheckCmd, Command, Expect, Guest, HistoryCheck, Invariant, InvariantType, Knob, Manifest,
    Regression, Route, Run, Service, TestPlan, Workload, World,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError {
    pub message: String,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ManifestError {}

/// Python's text-mode reading: UTF-8, `\r\n` and `\r` read as `\n`.
fn read_text(path: &str) -> Result<String, ManifestError> {
    let bytes = std::fs::read(path).map_err(|e| py::crash(format!("{path}: {e}")))?;
    let t =
        String::from_utf8(bytes).map_err(|_| py::crash(format!("{path}: UnicodeDecodeError")))?;
    Ok(t.replace("\r\n", "\n").replace('\r', "\n"))
}

fn json_loads(text: &str) -> Result<Value, ManifestError> {
    serde_json::from_str(text).map_err(|e| py::crash(format!("json.decoder.JSONDecodeError: {e}")))
}

/// Read a manifest file's text as data. JSON when the name ends in `.json`, or when `by_content`
/// is set and the text starts with `{` (an image keeps every plan as /etc/hydo/hydo.yaml), else
/// hydo-build's YAML subset.
pub fn parse_text(text: &str, name: &str, by_content: bool) -> Result<Value, ManifestError> {
    if name.ends_with(".json") || (by_content && py::lstrip(text).starts_with('{')) {
        return json_loads(text);
    }
    yaml::parse_yaml(text, name)
}

/// What `load` needs besides the file.
#[derive(Clone, Debug)]
pub struct LoadOptions {
    /// `--arch`: "x86_64" (the default) or "arm64".
    pub arch: String,
    /// `--set KEY=VALUE`, in order.
    pub sets: Vec<(String, String)>,
    /// The product's `onboard/` directory: a plan's `hydo_version:` is checked against the
    /// VERSION file next to it (hydo/VERSION in the repository, hydo/VERSION in a release).
    pub onboard_dir: PathBuf,
}

impl Default for LoadOptions {
    /// x86_64, no `--set`, and the onboard/ directory of a release laid out around the running
    /// program (`<root>/bin/hydo` next to `<root>/hydo/onboard`), else of this source tree.
    fn default() -> Self {
        let release = std::env::current_exe()
            .ok()
            .and_then(|e| e.canonicalize().ok())
            .and_then(|e| e.parent()?.parent().map(|r| r.join("hydo").join("onboard")))
            .filter(|d| d.is_dir());
        LoadOptions {
            arch: "x86_64".into(),
            sets: Vec::new(),
            onboard_dir: release.unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join("onboard")
            }),
        }
    }
}

/// A loaded manifest and what hydo-build printed or computed on the way.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub manifest: Manifest,
    /// The `hydo-build: warning:` lines, without that prefix, in order.
    pub warnings: Vec<String>,
    /// The test composer's plan meta (with `_folder`), when the manifest has a plan.
    pub compose: Option<Value>,
    /// The effective `params:` values.
    pub params: Vec<(String, String)>,
    /// Services in start order (depends_on).
    pub service_order: Vec<String>,
    /// Image services whose image exists but was not read (see `oci.rs`): (service, image path).
    pub oci_unchecked: Vec<(String, String)>,
}

/// hydo-build's `main()` up to the build: read, `--set`, the composer's plan, validate, the
/// architecture checks, the service order and the image checks.
pub fn load(path: &Path, opts: &LoadOptions) -> Result<Loaded, ManifestError> {
    let mut l = load_until_images(path, opts).0?;
    let p = path.to_string_lossy().into_owned();
    let arch = if opts.arch == "x86_64" || opts.arch == "arm64" {
        opts.arch.as_str()
    } else {
        "x86_64"
    };
    l.oci_unchecked = oci::prepare(&l.manifest.data, &p, arch)?;
    Ok(l)
}

/// [`load`] without the image checks (hydo-build crate reads the images itself, in hydo_oci's
/// order), with the warnings printed so far also when the manifest is refused: hydo-build prints
/// each `hydo-build: warning:` line before a refusal's message. `oci_unchecked` is empty.
pub fn load_until_images(
    path: &Path,
    opts: &LoadOptions,
) -> (Result<Loaded, ManifestError>, Vec<String>) {
    let mut warnings = Vec::new();
    let r = load_inner(path, opts, &mut warnings);
    let r = r.map(|mut l| {
        l.warnings = warnings.clone();
        l
    });
    (r, warnings)
}

fn load_inner(
    path: &Path,
    opts: &LoadOptions,
    warnings: &mut Vec<String>,
) -> Result<Loaded, ManifestError> {
    let p = path.to_string_lossy().into_owned();
    let text = read_text(&p)?;
    let mut m = if p.ends_with(".json") {
        json_loads(&text)?
    } else {
        yaml::parse_yaml(&text, &py::basename(&p))?
    };
    if !m.is_object() {
        return Err(ManifestError {
            message: "the manifest must be a mapping".into(),
        });
    }
    let params = validate::apply_params(&mut m, &opts.sets)?;
    if let Some(v) = m.get("hydo_version").cloned() {
        composer::check_version(&v, &composer::engine_version(&opts.onboard_dir), warnings)?;
    }
    let folder = composer::scan_folder(&py::dirname(&py::abspath(&p)))?;
    let mut comp = composer::expand(&mut m, folder.as_ref(), warnings)?;
    if let Some(c) = comp.as_mut() {
        c["_folder"] = folder.unwrap_or(Value::Null);
    }
    validate::validate(&mut m)?;
    if let Some(c) = &comp {
        m["_compose"] = c.clone();
    }
    validate::check_ram_arch(&m, &opts.arch)?;
    let order = validate::service_order(&m["services"])?;
    Ok(Loaded {
        manifest: Manifest::from_validated(m),
        warnings: Vec::new(),
        compose: comp,
        params,
        service_order: order,
        oci_unchecked: Vec::new(),
    })
}

/// hydo-build's `validate(m)` on data already parsed (hydo-compose checks the manifest it writes:
/// `parse_yaml`, then this, then [`service_order`]).
pub fn validate_data(m: &mut Value) -> Result<(), ManifestError> {
    validate::validate(m)
}

/// hydo-build's `service_order(m["services"])`: service names in start order (depends_on).
pub fn service_order(services: &Value) -> Result<Vec<String>, ManifestError> {
    validate::service_order(services)
}

/// The manifest an image carries (/etc/hydo/hydo.yaml, any format) with the plan meta it carries
/// (/etc/hydo/compose.json): manifest_space.Space(text, "hydo.yaml", compose=meta) up to its
/// validation. The plan is in `Manifest::test`; `data` is the validated manifest as Space holds it.
pub fn from_embedded(text: &str, compose: Option<&Value>) -> Result<Manifest, ManifestError> {
    from_embedded_meta(text, compose).map(|(m, _)| m)
}

/// [`from_embedded`], with the plan meta that the expansion gave (as manifest_space.Space keeps
/// it in `Space.compose`).
pub fn from_embedded_meta(
    text: &str,
    compose: Option<&Value>,
) -> Result<(Manifest, Option<Value>), ManifestError> {
    let mut m = parse_text(text, "hydo.yaml", true)?;
    let mut meta = None;
    let has_test = match &m {
        Value::Object(o) => o.contains_key("test"),
        other => py::has(other, "test")?,
    };
    if compose.is_some() || has_test {
        if let Some(c) = compose {
            if !m.is_object() {
                return Err(py::crash("the manifest is not a mapping"));
            }
            m["test"] = composer::test_from_meta(c);
        }
        let mut warnings = Vec::new();
        meta = composer::expand(&mut m, None, &mut warnings)?;
    }
    validate::validate(&mut m)?;
    let mut man = Manifest::from_validated(m);
    man.test = meta.as_ref().map(TestPlan::from_meta);
    Ok((man, meta))
}

/// A manifest file as the explorer reads it (hydo/explore/manifest_space.py `load(manifest=path)`):
/// JSON by name or content, else the YAML subset; no `params:`/`--set` and no version check; the
/// test composer's expansion with the `hydo/` folder next to the file; then `validate`. Returns
/// the validated data and the plan meta (None without a plan).
pub fn for_explorer(path: &Path) -> Result<(Value, Option<Value>), ManifestError> {
    let p = path.to_string_lossy().into_owned();
    let text = read_text(&p)?;
    let mut m = parse_text(&text, &py::basename(&p), true)?;
    let folder = composer::scan_folder(&py::dirname(&py::abspath(&p)))?;
    let mut warnings = Vec::new();
    let meta = composer::expand(&mut m, folder.as_ref(), &mut warnings)?;
    validate::validate(&mut m)?;
    Ok((m, meta))
}

/// The lines `hydo-build <path> --check` prints for a loaded manifest.
pub fn check_summary(path_as_given: &str, l: &Loaded) -> Vec<String> {
    let m = &l.manifest.data;
    let count = |k: &str| match m.get(k) {
        Some(Value::Array(a)) => a.len(),
        Some(Value::String(s)) => s.chars().count(),
        Some(Value::Object(o)) => o.len(),
        _ => 0,
    };
    let steps = m
        .get("workload")
        .and_then(|w| w.get("steps"))
        .map(py::d)
        .unwrap_or_default();
    let mut line = format!(
        "hydo-build: {} OK ({} services, {} invariants, {} workload steps)",
        path_as_given,
        count("services"),
        count("invariants"),
        steps
    );
    if m.get("checks").is_some_and(py::truthy) {
        line.push_str(&format!(" + {} history checks", count("checks")));
    }
    let mut out = vec![line];
    if let Some(c) = m.get("_compose").filter(|c| py::truthy(c)) {
        let cmds = c
            .get("commands")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let n = |k: &str| {
            c.get(k)
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0)
        };
        out.push(format!(
            "hydo-build: test composer: {} commands ({}), {} checks, {} regressions",
            cmds.len(),
            cmds.iter()
                .map(|x| py::str_of(&x["id"]))
                .collect::<Vec<_>>()
                .join(", "),
            n("checks"),
            n("regressions")
        ));
    }
    out
}
