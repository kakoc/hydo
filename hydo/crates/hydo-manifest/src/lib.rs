#![doc = include_str!("../README.md")]
//!
//! ## Notes for hydo's developers
//!
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
//! The project format itself (one document model for all three, its version rule, the schema and
//! the permanent example documents) is `model.rs`; `migrate.rs` has the canonical spelling,
//! `read_project` and `hydo migrate`.
//!
//! API contract other crates rely on (keep these names and signatures):
//! - `parse_text(text, name, by_content) -> Result<serde_json::Value, ManifestError>`
//! - `Guest::from_value(&Value)`: the `guest:` section with hydo-build's defaults
//! - `ManifestError`: `Display` gives hydo-build's message (the CLI prints `hydo-build: <msg>`)
//! - `load`, `from_embedded`, `check_summary` (below)
//!
//! Where hydo-build itself stops with a Python exception (a traceback, not a ManifestError), the
//! input is refused here too, with a message that starts with [`py::CRASH_PREFIX`].

pub mod ci;
pub mod composer;
pub mod known_findings;
pub mod messages;
pub mod migrate;
pub mod model;
mod oci;
pub mod py;
mod pyre;
pub mod restart;
pub mod rx;
pub mod storage;
mod types;
mod validate;
pub mod yaml;

use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};

pub use composer::{regression_status, Status};
pub use restart::Restart;
pub use types::{
    CheckCmd, Command, Expect, Guest, HistoryCheck, Invariant, InvariantType, Knob, Manifest,
    Regression, Route, Run, Service, TestPlan, Workload, World,
};
pub use validate::RAM_MIB_MAX;

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
    /// A plan whose `hydo_version:` this hydo does not satisfy loads with a warning instead of
    /// being refused (`HYDO_VERSION_CHECK=warn`, `hydo_config::build::Build`). Default off.
    pub version_check_warn: bool,
}

impl Default for LoadOptions {
    /// x86_64, no `--set`, and the onboard/ directory of a release laid out around the running
    /// program (`<root>/bin/hydo` next to `<root>/hydo/onboard`), else of the source tree the
    /// program was built in (`<tree>/target/.../<program>`): the nearest directory above the
    /// program that has `hydo/onboard`. Until M154 the source tree's directory was compiled into
    /// the program (`CARGO_MANIFEST_DIR`), so every release's `hydo-build` named the building
    /// machine's checkout.
    fn default() -> Self {
        let onboard = std::env::current_exe()
            .ok()
            .and_then(|e| e.canonicalize().ok())
            .and_then(|e| {
                e.ancestors()
                    .skip(1)
                    .map(|a| a.join("hydo").join("onboard"))
                    .find(|d| d.is_dir())
            });
        LoadOptions {
            arch: "x86_64".into(),
            sets: Vec::new(),
            onboard_dir: onboard.unwrap_or_else(|| Path::new("hydo").join("onboard")),
            version_check_warn: false,
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
    /// The document as written (after the format check, before `params:` and the expansion):
    /// what [`check_document`] reads into the project model.
    pub document: Value,
}

/// hydo-build's `main()` up to the build: read, `--set`, the composer's plan, validate, the
/// architecture checks, the service order, the image checks and the project model.
pub fn load(path: &Path, opts: &LoadOptions) -> Result<Loaded, ManifestError> {
    let mut l = load_until_images(path, opts).0?;
    let p = path.to_string_lossy().into_owned();
    let arch = if opts.arch == "x86_64" || opts.arch == "arm64" {
        opts.arch.as_str()
    } else {
        "x86_64"
    };
    l.oci_unchecked = oci::prepare(&l.manifest.data, &p, arch)?;
    check_document(&l)?;
    Ok(l)
}

/// The loaded document read into the project model (model.rs): its keys, their nesting and their
/// kinds of value, also where the validator does not look (root.files entries, mounts, routes).
/// `load` calls it after the image checks, and hydo-build after reading the images, so that the
/// more specific messages of the validator and of the image reader come first.
pub fn check_document(l: &Loaded) -> Result<model::Project, ManifestError> {
    model::from_value(&l.document)
}

/// The keys the validator knows, per place (for the test that keeps the model and the validator
/// on the same keys): (the model's definition name, the keys).
#[doc(hidden)]
pub fn validator_keys() -> Vec<(&'static str, Vec<&'static str>)> {
    use validate as v;
    vec![
        ("Project", v::KNOWN_TOP.to_vec()),
        ("Service", v::KNOWN_SVC.to_vec()),
        ("Invariant", v::KNOWN_INV.to_vec()),
        ("Knob", v::KNOWN_KNOB.to_vec()),
        ("Workload", v::KNOWN_WORKLOAD.to_vec()),
        ("Data", v::KNOWN_DATA.to_vec()),
        ("Root", v::KNOWN_ROOT.to_vec()),
        ("Guest", v::KNOWN_GUEST.to_vec()),
        ("ErrorsConfig", v::KNOWN_ERRORS_TOP.to_vec()),
        ("ServiceErrors", v::KNOWN_ERRORS_SVC.to_vec()),
        ("Group", v::KNOWN_GROUP.to_vec()),
        ("Fake", v::KNOWN_FAKE.to_vec()),
        ("Proxy", v::KNOWN_PROXY.to_vec()),
        ("FakeRoute", v::KNOWN_ROUTE.to_vec()),
        ("ProxyRoute", v::KNOWN_PROXY_ROUTE.to_vec()),
        ("Webhook", v::KNOWN_HOOK.to_vec()),
        ("Tls", v::KNOWN_TLS.to_vec()),
        ("HistoryCheck", v::known_check_all()),
        ("Test", composer::known_test()),
        ("TestCommand", composer::KNOWN_CMD.to_vec()),
        ("TestCheck", composer::KNOWN_CHECK.to_vec()),
        ("TestRegression", composer::KNOWN_REG.to_vec()),
    ]
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
    // the project format first: a newer document is refused before any of its keys is read
    model::check_format(&m, Some(&composer::engine_version(&opts.onboard_dir)))?;
    let as_written = m.clone();
    let params = validate::apply_params(&mut m, &opts.sets)?;
    if let Some(v) = m.get("hydo_version").cloned() {
        composer::check_version(
            &v,
            &composer::engine_version(&opts.onboard_dir),
            opts.version_check_warn,
            warnings,
        )?;
    }
    let folder = composer::scan_folder(&py::dirname(&py::abspath(&p)))?;
    let mut comp = composer::expand(&mut m, folder.as_ref(), warnings)?;
    if let Some(c) = comp.as_mut() {
        c["_folder"] = folder.unwrap_or(Value::Null);
    }
    // a messages check's include: bundles, relative to the manifest (the image carries the
    // resolved declaration)
    messages::resolve_includes(&mut m, &py::dirname(&py::abspath(&p)))?;
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
        document: as_written,
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

/// The `--check` warning for `root.files` sources that do not exist (relative to the manifest's
/// folder, as the build resolves them), or None when every source is there.
fn missing_sources(manifest_path: &str, m: &Value) -> Option<String> {
    let files = m.get("root")?.get("files")?.as_array()?;
    let mdir = py::dirname(&py::abspath(manifest_path));
    let home = std::env::var("HOME").unwrap_or_default();
    let mut missing = Vec::new();
    let mut total = 0usize;
    for f in files {
        let Some(src) = f.get("src").and_then(Value::as_str) else {
            continue;
        };
        total += 1;
        let p = match src.strip_prefix("~/") {
            Some(r) if !home.is_empty() => format!("{home}/{r}"),
            _ if src.starts_with('/') => src.to_string(),
            _ => format!("{}/{src}", mdir.trim_end_matches('/')),
        };
        if !Path::new(&p).exists() {
            missing.push(src.to_string());
        }
    }
    if missing.is_empty() {
        return None;
    }
    let shown: Vec<String> = missing.iter().take(3).cloned().collect();
    Some(format!(
        "hydo-build: warning: {} of {total} source files of root.files are missing ({}{}); the \
         full build will fail until they exist (--check checks the format only)",
        missing.len(),
        shown.join(", "),
        if missing.len() > 3 { ", ..." } else { "" }
    ))
}

/// A service's `ready:` as the shell command the guest runs. `tcp:<port>` and
/// `tcp:<host>:<port>` (Milestone 239, a format-1 addition) are a connect to the port that must
/// succeed, run with the guest busybox's `nc` exactly as `hydo compose` writes a readiness check
/// without a healthcheck. The host defaults to the service's `addr:`, else 127.0.0.1. Every
/// other value is returned as written, so no existing manifest's command changes.
pub fn ready_command(ready: &str, addr: Option<&str>) -> String {
    match tcp_ready(ready) {
        Some(Ok((host, port))) => {
            let host = host
                .or(addr.filter(|a| !a.is_empty()))
                .unwrap_or("127.0.0.1");
            format!("nc -z -w 2 {host} {port}")
        }
        _ => ready.to_string(),
    }
}

/// `tcp:[<host>:]<port>`: None for any other `ready:`; Err with the reason for a malformed one.
pub fn tcp_ready(ready: &str) -> Option<Result<(Option<&str>, u16), String>> {
    let rest = ready.trim().strip_prefix("tcp:")?;
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (Some(h), p),
        None => (None, rest),
    };
    let bad = || {
        format!(
            "ready: {ready:?}: tcp:<port> or tcp:<host>:<port>, a port from 1 to 65535 and a \
             host of letters, digits, '.', '-' and '_'"
        )
    };
    let port: u16 = match port.parse() {
        Ok(p) if p > 0 => p,
        _ => return Some(Err(bad())),
    };
    if let Some(h) = host {
        if h.is_empty()
            || !h
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Some(Err(bad()));
        }
    }
    Some(Ok((host, port)))
}

/// The `guest.clock_start` a new project gets (`hydo init`, `hydo compose`): `given`
/// (`HYDO_INIT_CLOCK_START`), else today at 00:00 UTC. The format's default (2026-01-01) stays
/// as it is, because changing it would move every recorded image; a new project names its own
/// start, so that the guest's clock does not start before the software it runs was built.
pub fn new_clock_start(given: Option<i64>) -> i64 {
    given.unwrap_or_else(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        now - now.rem_euclid(86_400)
    })
}

/// A Unix time's UTC date, `YYYY-MM-DD` (for the comment beside a written `clock_start`).
pub fn utc_date(sec: i64) -> String {
    // days since 1970-01-01 to a civil date (Howard Hinnant's days_from_civil, inverted)
    let z = sec.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
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
    embedded(text, compose, None)
}

/// [`from_embedded`] with the image's parameter values (`/etc/hydo/params`, the `name=value`
/// lines hydo-build writes for a manifest with `params:`): `params:` is applied as hydo-build
/// applies it (its defaults, then these values; every `{{name}}` substituted), so the manifest
/// says what the image's compiled manifest.sh says (M128: an agent image's controller read
/// `/bin/orderd serve --mode {{mode}}` literally). Without `params:` this is [`from_embedded`].
pub fn from_embedded_params(
    text: &str,
    compose: Option<&Value>,
    params: &[(String, String)],
) -> Result<Manifest, ManifestError> {
    embedded(text, compose, Some(params)).map(|(m, _)| m)
}

/// The `name=value` lines of an image's `/etc/hydo/params`.
pub fn parse_params_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn embedded(
    text: &str,
    compose: Option<&Value>,
    params: Option<&[(String, String)]>,
) -> Result<(Manifest, Option<Value>), ManifestError> {
    let mut m = parse_text(text, "hydo.yaml", true)?;
    if let Some(p) = params {
        if m.get("params").is_some() {
            validate::apply_params(&mut m, p)?;
        }
    }
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
    if m.is_object() {
        model::check_format(&m, None)?;
    }
    let folder = composer::scan_folder(&py::dirname(&py::abspath(&p)))?;
    let mut warnings = Vec::new();
    let meta = composer::expand(&mut m, folder.as_ref(), &mut warnings)?;
    validate::validate(&mut m)?;
    Ok((m, meta))
}

/// The ids of the placeholder command and check that `hydo init` writes
/// (`hydo/guide/init/hydo/workload/parallel_driver_todo`, `checks/final_todo`).
pub const INIT_PLACEHOLDERS: [&str; 2] = ["parallel_driver_todo", "final_todo"];

/// The id of the placeholder invariant that `hydo compose` writes (hydo-compose's
/// `invariants:` of a new import).
pub const COMPOSE_PLACEHOLDER_INVARIANT: &str = "todo.acked_operations_survive";

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
        // `--check` checks the format only: name the placeholders `hydo init` wrote, which pass
        // it and fail every run (0.3.2, a reader of the quickstart took the OK for a finished
        // project)
        for (k, dir) in [("commands", "workload"), ("checks", "checks")] {
            for x in c.get(k).and_then(Value::as_array).into_iter().flatten() {
                let mut id = py::str_of(&x["id"]);
                if id.is_empty() || id == "None" {
                    id = format!("{}_{}", py::str_of(&x["kind"]), py::str_of(&x["name"]));
                }
                if INIT_PLACEHOLDERS.contains(&id.as_str()) {
                    out.push(format!(
                        "hydo-build: warning: hydo/{dir}/{id} is the placeholder `hydo init` wrote; \
                         every run fails until you replace it (--check checks the format only)"
                    ));
                }
            }
        }
    }
    // the same for the placeholders `hydo compose` writes (0.3.3, a reader of docs/ONBOARD.md)
    let run = m
        .get("workload")
        .and_then(|w| w.get("run"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if run.contains("hydo-compose: TODO write the workload") {
        out.push(
            "hydo-build: warning: workload.run is the placeholder `hydo compose` wrote; every run \
             fails until you replace it (--check checks the format only)"
                .to_string(),
        );
    }
    for x in m
        .get("invariants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if x.get("id").and_then(Value::as_str) == Some(COMPOSE_PLACEHOLDER_INVARIANT) {
            out.push(format!(
                "hydo-build: warning: the invariant {COMPOSE_PLACEHOLDER_INVARIANT} is the \
                 placeholder `hydo compose` wrote; it cannot pass until you replace it (--check \
                 checks the format only)"
            ));
        }
    }
    // `--check` does not stage the files: name the sources the full build will not find (an
    // onboarding of Milestone 239 wrote root.files before building the binaries)
    if let Some(w) = missing_sources(path_as_given, m) {
        out.push(w);
    }
    // each messages check's resolved declaration (what the image will claim)
    for c in m
        .get("checks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if c.get("type").and_then(Value::as_str) == Some(messages::TYPE) {
            let mut sp = serde_json::Map::new();
            messages::spec_fields(c, &mut sp);
            out.push(format!(
                "hydo-build: check {} (messages, history {}) declares {}",
                py::str_of(&c["id"]),
                py::str_of(&c["history"]),
                serde_json::to_string(&Value::Object(sp)).unwrap_or_default()
            ));
        }
    }
    out
}

#[cfg(test)]
mod ready_tests {
    use super::{ready_command, tcp_ready};

    #[test]
    fn a_tcp_ready_is_the_connect_check_and_every_other_value_stays() {
        assert_eq!(ready_command("tcp:7000", None), "nc -z -w 2 127.0.0.1 7000");
        assert_eq!(
            ready_command("tcp:7000", Some("10.89.0.11")),
            "nc -z -w 2 10.89.0.11 7000"
        );
        assert_eq!(
            ready_command("tcp:node2:9000", Some("10.89.0.11")),
            "nc -z -w 2 node2 9000"
        );
        for r in ["redis-cli ping", "grep -qa started $HYDO_LOG", "", " tcp"] {
            assert_eq!(ready_command(r, Some("10.89.0.2")), r);
        }
        for bad in [
            "tcp:",
            "tcp:0",
            "tcp:70000",
            "tcp:x",
            "tcp::80",
            "tcp:a b:80",
        ] {
            assert!(matches!(tcp_ready(bad), Some(Err(_))), "{bad}");
        }
    }

    #[test]
    fn the_check_names_missing_sources_of_root_files() {
        let d = std::env::temp_dir().join(format!("hydo-missing-src-{}", std::process::id()));
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("bin/there"), b"x").unwrap();
        let mp = d.join("hydo.yaml").display().to_string();
        let m = serde_json::json!({"root": {"files": [
            {"src": "bin/there", "dst": "/bin/there"},
            {"src": "bin/a", "dst": "/bin/a"},
            {"src": "bin/b", "dst": "/bin/b"}
        ]}});
        let w = super::missing_sources(&mp, &m).expect("two are missing");
        assert!(
            w.starts_with("hydo-build: warning: 2 of 3 source files of root.files are missing (bin/a, bin/b);"),
            "{w}"
        );
        let all = serde_json::json!({"root": {"files": [{"src": "bin/there", "dst": "/x"}]}});
        assert_eq!(super::missing_sources(&mp, &all), None);
        assert_eq!(super::missing_sources(&mp, &serde_json::json!({})), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_malformed_tcp_ready_fails_the_check() {
        let mut m: serde_json::Value = serde_json::json!({
            "name": "x", "format": 1,
            "services": [{"name": "a", "cmd": "/bin/a", "ready": "tcp:99999"}]
        });
        let e = super::validate_data(&mut m).expect_err("a port above 65535");
        assert!(e.message.contains("tcp:<port>"), "{}", e.message);
        let mut ok: serde_json::Value = serde_json::json!({
            "name": "x", "format": 1,
            "services": [{"name": "a", "cmd": "/bin/a", "ready": "tcp:7000"}],
            "workload": {"steps": 1, "run": "true"},
            "invariants": [{"id": "i", "cmd": "true"}]
        });
        super::validate_data(&mut ok).expect("tcp:7000 is valid");
    }
}

#[cfg(test)]
mod clock_tests {
    #[test]
    fn dates() {
        assert_eq!(super::utc_date(1767225600), "2026-01-01");
        assert_eq!(super::utc_date(951782400), "2000-02-29");
        assert_eq!(super::utc_date(0), "1970-01-01");
        assert_eq!(super::utc_date(1794096000), "2026-11-08");
        assert_eq!(super::new_clock_start(Some(5)), 5);
        assert_eq!(super::new_clock_start(None) % 86_400, 0);
    }
}
