//! `hydo compose` / `hydo-compose`: turn a docker-compose.yaml into a starting hydo.yaml
//! (MANIFEST.md, "From Docker Compose"). The Rust port of hydo/onboard/hydo_compose.py, which it
//! replaces: for the same Compose file, options and lock file it writes the same hydo.yaml,
//! fetch-images.sh and hydo-compose.lock.json and prints the same messages (docs/HISTORY.md, M106).
//!
//! Differences from the Python, on purpose:
//! - The Compose file is read with PyYAML's `safe_load` semantics (src/yaml.rs), which the Python
//!   used when PyYAML was installed; without PyYAML it used hydo-build's YAML subset instead.
//! - Images are resolved to digests by talking to the registry directly (src/registry.rs, the
//!   requests `skopeo inspect --raw` and `skopeo inspect --config --raw` make), so no skopeo is
//!   needed. `hydo-fetch-image` (the port of fetch_image.py) copies an image by digest.
//! - Where the Python stopped with a traceback (input it did not expect, such as a service that
//!   is not a mapping), this prints one `hydo-compose: <reason>` line (the exit status is 1 in
//!   both).

pub mod interp;
pub mod ocilocal;
pub mod pyval;
pub mod registry;
pub mod shlex;
pub mod yaml;

use interp::{parse_env_file, read_text, Interp};
use pyval::{as_list, dumps_str, pstr, repr, Y};
use regex::Regex;
use std::fmt;
use std::path::Path;
use std::sync::OnceLock;

pub const USAGE: &str = include_str!("usage.txt");

const ADDR_BASE: usize = 11; // 127.0.0.11, .12, ... ; 10.89.0.11, .12, ... (network: bridge)
const BRIDGE_GW: &str = "10.89.0.1"; // hydo-build's NET_GW: the bridge's own address
const DEFAULT_SVC_RAM: i128 = 256;

/// Known image families: (match on the repository's last path component, ram MiB, data MiB, cpu).
const FAMILIES: &[(&str, i128, i128, Option<&str>)] = &[
    (r"^(postgres|postgis|timescaledb)", 384, 1024, None),
    (r"^(mysql|mariadb|percona)", 640, 1024, None),
    (r"^(redis|valkey|keydb)", 96, 256, None),
    (r"^(mongo)", 640, 1024, None),
    (
        r"^(kafka|cp-kafka|zookeeper|cp-zookeeper|elasticsearch|opensearch|cassandra|keycloak)",
        1024,
        1024,
        None,
    ),
    (r"^(rabbitmq)", 384, 512, None),
    (r"^(memcached|nginx|haproxy|traefik|caddy)", 64, 128, None),
    (r"^restate$", 512, 1024, Some("v3")), // restatedev/restate: built with target-cpu=broadwell (M68)
];

const HANDLED: &[&str] = &[
    "image",
    "command",
    "entrypoint",
    "environment",
    "env_file",
    "working_dir",
    "user",
    "depends_on",
    "healthcheck",
    "ports",
    "expose",
    "volumes",
    "restart",
    "container_name",
    "hostname",
    "networks",
    "extra_hosts",
    "links",
    "tmpfs",
    "profiles",
    "build",
    "deploy",
    "mem_limit",
    "mem_reservation",
    "secrets",
    "platform",
];
const IGNORED: &[&str] = &[
    "labels",
    "logging",
    "stdin_open",
    "tty",
    "stop_signal",
    "stop_grace_period",
    "domainname",
    "pull_policy",
    "annotations",
    "attach",
    "cpu_count",
    "cpu_percent",
    "cpu_shares",
    "cpus",
    "cpu_quota",
    "cpu_period",
    "cpuset",
    "oom_score_adj",
    "oom_kill_disable",
    "x-*",
];
const UNSUPPORTED: &[(&str, &str)] = &[
    ("privileged", "hydo runs every service as a plain process (no capabilities beyond root's); remove it if the program does not need it"),
    ("cap_add", "capabilities are not modelled; programs run as root or as their user"),
    ("cap_drop", "capabilities are not modelled"),
    ("devices", "host devices are not available in the guest; drop the service or replace the device"),
    ("device_cgroup_rules", "host devices are not available in the guest"),
    ("security_opt", "seccomp/AppArmor profiles are not applied"),
    ("network_mode", "every service has its own network namespace on one bridge (or, with --network loopback, all share one loopback network); network_mode is ignored"),
    ("pid", "pid namespaces are not modelled; services see each other's processes"),
    ("ipc", "ipc namespaces are not modelled"),
    ("uts", "uts namespaces are not modelled (the guest has one hostname)"),
    ("userns_mode", "user namespaces are not modelled"),
    ("cgroup", "cgroups are not modelled"),
    ("cgroup_parent", "cgroups are not modelled"),
    ("sysctls", "set them in a service's setup: (sysctl -w ...) if the program needs them"),
    ("ulimits", "set them in a service's setup: (ulimit) if the program needs them"),
    ("dns", "there is no DNS in the guest; names resolve through /etc/hosts"),
    ("dns_search", "there is no DNS in the guest"),
    ("dns_opt", "there is no DNS in the guest"),
    ("init", "there is no init process per service (no tini); a program that must reap children should be started by its own init"),
    ("volumes_from", "list the volumes explicitly in each service"),
    ("extends", "inline the extended service (or run `docker compose config` and import its output)"),
    ("configs", "use a bind mount of the file instead"),
    ("scale", "one instance per service; copy the service to get more"),
    ("isolation", "not modelled"),
    ("shm_size", "every image service gets a 64 MiB /dev/shm"),
    ("group_add", "add the groups in the image's /etc/group"),
    ("runtime", "not modelled"),
    ("storage_opt", "not modelled"),
    ("mac_address", "not modelled"),
    ("post_start", "add the commands to the service's setup: or to provision:"),
    ("pre_stop", "not modelled"),
    ("develop", "not modelled"),
    ("gpus", "no GPUs in the guest"),
];
const SPECIAL_HOST: &[&str] = &[
    "/var/run/docker.sock",
    "/run/docker.sock",
    "/etc/localtime",
    "/etc/timezone",
    "/proc",
    "/sys",
    "/dev",
    "/var/lib/docker",
    "/lib/modules",
];

// ---------------------------------------------------------------- errors and warnings

/// A refusal: `hydo-compose: <message>`, exit status 1.
#[derive(Debug, Clone)]
pub struct ComposeError(pub String);

impl fmt::Display for ComposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl ComposeError {
    /// Python's OSError text: `[Errno 2] No such file or directory: '/x'`.
    pub fn os(e: std::io::Error, p: &str) -> ComposeError {
        let n = e.raw_os_error().unwrap_or(0);
        let msg = e.to_string();
        let msg = msg.split(" (os error").next().unwrap_or(&msg).to_string();
        ComposeError(format!(
            "[Errno {n}] {msg}: {}",
            hydo_manifest::py::repr_str(p)
        ))
    }
}

type R<T> = Result<T, ComposeError>;

fn err<T>(m: impl Into<String>) -> R<T> {
    Err(ComposeError(m.into()))
}

/// The warnings, in order, each once.
#[derive(Default, Debug)]
pub struct Warn {
    pub list: Vec<(String, String)>,
}

impl Warn {
    pub fn warn(&mut self, key: &str, text: String) {
        if !self.list.iter().any(|(k, t)| k == key && *t == text) {
            self.list.push((key.to_string(), text));
        }
    }
}

// ---------------------------------------------------------------- restart policies (M134)

/// A Compose `restart:` value in Compose's words, or None when it is not one.
fn compose_restart(v: &Y) -> Option<String> {
    let s = match v {
        Y::Bool(false) => return Some("no".into()),
        _ => pstr(v),
    };
    match s.as_str() {
        "no" | "False" | "false" => Some("no".into()),
        "always" | "unless-stopped" | "on-failure" => Some(s),
        _ => s
            .strip_prefix("on-failure:")
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u64>().ok())
            .map(|n| format!("on-failure:{n}")),
    }
}

// ---------------------------------------------------------------- small Python helpers

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

/// `re.match(pattern + "$")`: a full match, or one that leaves a single final newline.
fn match_dollar(r: &Regex, s: &str) -> bool {
    r.is_match(s) || (s.ends_with('\n') && r.is_match(&s[..s.len() - 1]))
}

fn plain_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"^[A-Za-z0-9_./][A-Za-z0-9_./@+=-]*$"))
}

fn path_ok_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| re(r"^/[A-Za-z0-9._@+=,:/-]*$"))
}

/// The named volume (`safe_name` of its source) a Compose `volumes:` entry puts on `/data`, if it
/// is one: the short form `name:/data[:opts]` or the long form `{type: volume, source, target}`.
fn named_volume_on_data(v: &Y) -> Option<String> {
    named_volume(v)
        .filter(|(_, tgt)| tgt == "/data")
        .map(|(name, _)| name)
}

/// A named volume entry (`name:/target`, or `type: volume` with a source): its safe name and its
/// normalised target.
fn named_volume(v: &Y) -> Option<(String, String)> {
    let (typ, src, tgt) = if v.is_map() {
        (
            pstr(v.get("type").unwrap_or(&Y::s("volume"))),
            v.getn("source").clone(),
            v.getn("target").clone(),
        )
    } else {
        let vs = pstr(v);
        let parts: Vec<&str> = vs.split(':').collect();
        if parts.len() < 2 || parts[0].starts_with(['.', '/', '~']) {
            return None;
        }
        ("volume".to_string(), Y::s(parts[0]), Y::s(parts[1]))
    };
    let Y::Str(s) = src else { return None };
    (typ == "volume" && !s.is_empty())
        .then(|| (safe_name(&s), hydo_manifest::py::normpath(&pstr(&tgt))))
}

/// `re.sub(r"[^A-Za-z0-9._-]", "_", s)`.
fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `os.path.isabs`.
fn isabs(p: &str) -> bool {
    p.starts_with('/')
}

/// `os.path.join(a, b)`.
fn join(a: &str, b: &str) -> String {
    if isabs(b) || a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

fn exists(p: &str) -> bool {
    Path::new(p).exists()
}

/// `os.path.relpath(path, start)`.
fn relpath(path: &str, start: &str) -> String {
    let a = hydo_manifest::py::abspath(path);
    let b = hydo_manifest::py::abspath(start);
    let a: Vec<&str> = a.split('/').filter(|x| !x.is_empty()).collect();
    let b: Vec<&str> = b.split('/').filter(|x| !x.is_empty()).collect();
    let n = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<&str> = vec![".."; b.len() - n];
    parts.extend(&a[n..]);
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// `os.path.expanduser` for `~` and `~/...`.
fn expanduser(p: &str) -> String {
    if p == "~" || p.starts_with("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let home = home.trim_end_matches('/');
        return format!("{home}{}", &p[1..]);
    }
    p.to_string()
}

/// Python's `int(s)` of a str (whitespace, a sign, digits with single underscores).
fn py_int(s: &str) -> Option<i128> {
    let t = hydo_manifest::py::strip(s);
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if body.is_empty()
        || body.starts_with('_')
        || body.ends_with('_')
        || body.contains("__")
        || !body.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let v: i128 = body.replace('_', "").parse().ok()?;
    Some(if neg { -v } else { v })
}

/// `int(x)` of a value.
fn int_of(v: &Y) -> Option<i128> {
    match v {
        Y::Int(i) => Some(*i),
        Y::Bool(b) => Some(i128::from(*b)),
        Y::Float(f) if f.is_finite() => Some(f.trunc() as i128),
        Y::Str(s) => py_int(s),
        _ => None,
    }
}

/// `"%s" % sorted(set(ints))` style: a Python list of ints.
fn int_list(v: &[i128]) -> String {
    format!(
        "[{}]",
        v.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

// ---------------------------------------------------------------- YAML output (hydo-build's subset)

/// The YAML form of a scalar: plain when it is safe, else JSON-quoted.
fn q_str(s: &str) -> String {
    let plain = match_dollar(plain_re(), s)
        && !matches!(
            s,
            "true" | "false" | "yes" | "no" | "null" | "True" | "False" | "~"
        )
        && !is_int_text(s);
    if plain {
        s.to_string()
    } else {
        dumps_str(s)
    }
}

/// `re.fullmatch(r"-?\d+", s)`.
fn is_int_text(s: &str) -> bool {
    let b = s.strip_prefix('-').unwrap_or(s);
    !b.is_empty() && b.chars().all(|c| c.is_ascii_digit())
}

fn q(v: &Y) -> String {
    match v {
        Y::Bool(b) => if *b { "true" } else { "false" }.into(),
        Y::Int(i) => i.to_string(),
        other => q_str(&pstr(other)),
    }
}

fn flow_list(xs: &[String]) -> String {
    format!(
        "[{}]",
        xs.iter().map(|x| q_str(x)).collect::<Vec<_>>().join(", ")
    )
}

fn flow_map(d: &[(&str, Y)]) -> String {
    format!(
        "{{{}}}",
        d.iter()
            .map(|(k, v)| format!("{k}: {}", q(v)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn block(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut o = String::from("|\n");
    for ln in text.trim_end_matches('\n').split('\n') {
        if !ln.is_empty() {
            o.push_str(&pad);
            o.push_str(ln);
        }
        o.push('\n');
    }
    o
}

fn comment(text: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let mut o = String::new();
    for ln in text.split('\n') {
        if ln.is_empty() {
            o.push_str(&format!("{pad}#\n"));
        } else {
            o.push_str(&format!("{pad}# {ln}\n"));
        }
    }
    o
}

// ---------------------------------------------------------------- images

/// 'redis:7.2.4', 'ghcr.io/x/y@sha256:..' -> (registry, repo, tag, digest).
pub fn parse_ref(r: &str) -> (String, String, Option<String>, Option<String>) {
    let mut r = hydo_manifest::py::strip(r).to_string();
    let mut digest = None;
    if let Some((a, d)) = r.clone().split_once('@') {
        r = a.to_string();
        digest = Some(d.to_string());
    }
    let mut tag = None;
    let last = r.rsplit('/').next().unwrap_or("").to_string();
    if last.contains(':') {
        let i = r.rfind(':').unwrap();
        tag = Some(r[i + 1..].to_string());
        r.truncate(i);
    }
    let parts: Vec<&str> = r.split('/').collect();
    let (reg, mut repo) = if parts.len() > 1
        && (parts[0].contains('.') || parts[0].contains(':') || parts[0] == "localhost")
    {
        (parts[0].to_string(), parts[1..].join("/"))
    } else {
        ("docker.io".to_string(), r.clone())
    };
    if reg == "docker.io" && !repo.contains('/') {
        repo = format!("library/{repo}");
    }
    if tag.is_none() && digest.is_none() {
        tag = Some("latest".into());
    }
    (reg, repo, tag, digest)
}

pub fn short_name(reg: &str, repo: &str) -> String {
    let r = if reg == "docker.io" {
        repo.strip_prefix("library/").unwrap_or(repo)
    } else {
        repo
    };
    if reg == "docker.io" {
        r.to_string()
    } else {
        format!("{reg}/{r}")
    }
}

fn file_stem(reg: &str, repo: &str, tag: Option<&str>, digest: Option<&str>) -> R<String> {
    let base = safe_dash(&short_name(reg, repo).replace('/', "-"));
    let t = match (tag, digest) {
        (Some(t), _) if !t.is_empty() => t.to_string(),
        (_, Some(d)) => d.split(':').nth(1).map(|h| h.chars().take(12).collect()).unwrap_or_default(),
        // the Python stopped here with a traceback (None.split)
        _ => {
            return err(format!(
                "image {}: the tag is empty (an unset variable in the Compose file?); give the image a tag",
                short_name(reg, repo)
            ))
        }
    };
    Ok(format!("{base}-{}", safe_dash(&t)))
}

/// `re.sub(r"[^A-Za-z0-9._-]", "-", s)`.
fn safe_dash(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn opt_y(v: Option<String>) -> Y {
    v.map(Y::Str).unwrap_or(Y::Null)
}

// ---------------------------------------------------------------- the conversion

/// `svc_name(n)`: a valid hydo service name for a Compose service name.
fn svc_name(n: &str) -> String {
    let s: String = n
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        format!("s_{s}")
    } else {
        s
    }
}

/// command/entrypoint: a list as is; a string split into words as Compose does (no shell).
fn words(v: Option<&Y>, wh: &str) -> R<Option<Vec<String>>> {
    match v {
        None | Some(Y::Null) => Ok(None),
        Some(Y::Seq(a)) => Ok(Some(a.iter().map(|x| unesc(&pstr(x))).collect())),
        Some(o) => match shlex::split(&pstr(o)) {
            Ok(w) => Ok(Some(w.iter().map(|x| unesc(x)).collect())),
            Err(e) => err(format!("{wh}: cannot split {} into words ({e})", repr(o))),
        },
    }
}

fn unesc(s: &str) -> String {
    s.replace("$$", "$")
}

/// Compose durations: 30s, 1m30s, 500ms, 1h -> seconds (an int, at least 1).
fn duration_s(v: Option<&Y>, default: i128) -> R<i128> {
    let v = match v {
        None | Some(Y::Null) => return Ok(default),
        Some(v) => v,
    };
    if let Y::Int(i) = v {
        return Ok(if *i > 100000 {
            (i / 1_000_000_000).max(1)
        } else {
            *i
        });
    }
    if let Y::Bool(b) = v {
        return Ok(i128::from(*b));
    }
    let s = pstr(v);
    let s = hydo_manifest::py::strip(&s);
    let cs: Vec<char> = s.chars().collect();
    let mut tot = 0.0f64;
    let mut i = 0;
    while i < cs.len() {
        if !(cs[i].is_ascii_digit() || cs[i] == '.') {
            i += 1;
            continue;
        }
        let st = i;
        while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
            i += 1;
        }
        let num: String = cs[st..i].iter().collect();
        let rest: String = cs[i..].iter().collect();
        let mut mult = 1.0;
        for (u, m) in [
            ("us", 1e-6),
            ("ms", 1e-3),
            ("s", 1.0),
            ("m", 60.0),
            ("h", 3600.0),
        ] {
            if rest.starts_with(u) {
                mult = m;
                i += u.len();
                break;
            }
        }
        let n: f64 = num
            .parse()
            .map_err(|_| ComposeError(format!("could not convert string to float: '{num}'")))?;
        tot += n * mult;
    }
    Ok((round_half_even(tot) as i128).max(1))
}

/// Python's `round()` of a float to an int (ties to even).
fn round_half_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

/// `mem_limit`-style sizes -> MiB.
fn mem_mib(v: Option<&Y>) -> R<Option<i128>> {
    let v = match v {
        None | Some(Y::Null) => return Ok(None),
        Some(v) => v,
    };
    match v {
        Y::Int(i) => return Ok(Some(i.div_euclid(1 << 20))),
        Y::Bool(b) => return Ok(Some(i128::from(*b) >> 20)),
        _ => {}
    }
    static R: OnceLock<Regex> = OnceLock::new();
    let r = R.get_or_init(|| re(r"^\s*([\d.]+)\s*([bkmg]?)i?b?\s*$"));
    let s = pstr(v).to_lowercase();
    let Some(c) = r.captures(&s) else {
        return Ok(None);
    };
    let n: f64 = c[1]
        .parse()
        .map_err(|_| ComposeError(format!("could not convert string to float: '{}'", &c[1])))?;
    let f = match &c[2] {
        "" | "b" => 1.0 / (1u64 << 20) as f64,
        "k" => 1.0 / 1024.0,
        "m" => 1.0,
        _ => 1024.0,
    };
    Ok(Some((n * f).trunc() as i128))
}

#[derive(Clone, Debug, Default)]
struct Mount {
    src: Option<String>,
    tmpfs: bool,
    dst: String,
    copy_up: bool,
    size: Option<String>,
}

impl Mount {
    fn src(src: &str, dst: &str, copy_up: bool) -> Mount {
        Mount {
            src: Some(src.into()),
            dst: dst.into(),
            copy_up,
            ..Default::default()
        }
    }

    fn tmpfs(dst: &str, size: Option<String>) -> Mount {
        Mount {
            tmpfs: true,
            dst: dst.into(),
            size,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug)]
enum Img {
    One(String),
    Two(String, String),
}

#[derive(Clone, Debug)]
struct Svc {
    name: String,
    compose: String,
    comments: Vec<String>,
    info: Y,
    image: Img,
    image_ref: String,
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
    job: Option<Vec<String>>,
    env: Vec<(String, String)>,
    workdir: Option<String>,
    user: Option<String>,
    port: Option<i128>,
    listen: Vec<i128>,
    addr: String,
    noaddr: bool,
    depends_on: Vec<String>,
    setup: Option<String>,
    ready: String,
    ready_timeout: Option<i128>,
    recovery_false: bool,
    /// The Compose restart policy, in its words (`restart:` or `deploy.restart_policy`), M134.
    restart: Option<String>,
    mounts: Vec<Mount>,
    /// Built locally (`build:` with no image file, lock entry or registry image yet): the
    /// arguments of `docker build` that make it, for fetch-images.sh.
    build: Option<String>,
}

/// The options of one import.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub env_files: Vec<String>,
    pub profiles: Vec<String>,
    pub use_env: bool,
    pub no_resolve: bool,
    pub no_addr: bool,
    pub network: Option<String>,
    pub name: Option<String>,
    pub images_dir: Option<String>,
    pub lock: Option<String>,
}

pub struct Importer<'a> {
    cpath: String,
    cdir: String,
    out: String,
    odir: String,
    o: Opts,
    bridge: bool,
    pub w: Warn,
    notes: Vec<String>,
    interp: Interp,
    doc: Y,
    lock_path: String,
    lock: Y,
    pub lock_dirty: bool,
    reg: &'a mut dyn registry::Registry,
    names: Vec<(String, String)>,
    addr: Vec<(String, String)>,
    hosts: Vec<(String, Vec<String>)>,
    bind_files: Vec<(String, String)>,
    data_dirs: Vec<String>,
    /// several services each have their own named volume on /data: mount each on its /data
    own_data: bool,
    /// why `own_data` is on, for the note
    own_data_why: &'static str,
    ram: i128,
    data_mib: i128,
    cpu: Option<String>,
    svcs: Vec<Svc>,
}

fn lookup<'a>(v: &'a [(String, String)], k: &str) -> Option<&'a String> {
    v.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

impl<'a> Importer<'a> {
    pub fn new(
        compose_path: &str,
        out_path: &str,
        o: Opts,
        reg: &'a mut dyn registry::Registry,
    ) -> Importer<'a> {
        let cpath = hydo_manifest::py::abspath(compose_path);
        let out = hydo_manifest::py::abspath(out_path);
        let bridge = o.network.as_deref().unwrap_or("bridge") == "bridge" && !o.no_addr;
        Importer {
            cdir: hydo_manifest::py::dirname(&cpath),
            odir: hydo_manifest::py::dirname(&out),
            cpath,
            out,
            o,
            bridge,
            w: Warn::default(),
            notes: Vec::new(),
            interp: Interp::new(Vec::new(), Vec::new(), false),
            doc: Y::Null,
            lock_path: String::new(),
            lock: Y::Map(Vec::new()),
            lock_dirty: false,
            reg,
            names: Vec::new(),
            addr: Vec::new(),
            hosts: Vec::new(),
            bind_files: Vec::new(),
            data_dirs: Vec::new(),
            own_data: false,
            own_data_why: "",
            ram: 256,
            data_mib: 256,
            cpu: None,
            svcs: Vec::new(),
        }
    }

    pub fn lock_path(&self) -> &str {
        &self.lock_path
    }

    pub fn services(&self) -> usize {
        self.svcs.len()
    }

    // ------------------------------------------------------------ top

    fn load(&mut self) -> R<()> {
        let text = read_text(&self.cpath)?;
        let doc = if self.cpath.ends_with(".json") {
            let v: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| ComposeError(format!("{}: {e}", self.cpath)))?;
            pyval::from_json(&v)
        } else {
            yaml::safe_load(&text).map_err(|e| ComposeError(format!("{}: {e}", self.cpath)))?
        };
        let ok = doc
            .get("services")
            .is_some_and(|s| s.is_map() && s.truthy());
        if !ok {
            return err(format!(
                "{}: no services: mapping (is this a Compose file?)",
                self.cpath
            ));
        }
        let mut envf: Vec<(String, String)> = Vec::new();
        let dot = join(&self.cdir, ".env");
        let files = if !self.o.env_files.is_empty() {
            self.o.env_files.clone()
        } else if Path::new(&dot).is_file() {
            vec![dot]
        } else {
            Vec::new()
        };
        for f in files {
            let (e, _) = parse_env_file(&f, true)?;
            for (k, v) in e {
                if let Some(x) = envf.iter_mut().find(|(a, _)| *a == k) {
                    x.1 = v;
                } else {
                    envf.push((k, v));
                }
            }
        }
        let shell: Vec<(String, String)> = std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect();
        self.interp = Interp::new(envf, shell, self.o.use_env);
        self.doc = self.interp.walk(&doc, "compose", &mut self.w)?;
        for k in self.doc.iter_keys() {
            let ks = pstr(&k);
            if matches!(
                ks.as_str(),
                "services" | "volumes" | "networks" | "name" | "version" | "secrets"
            ) || ks.starts_with("x-")
            {
                continue;
            }
            if ks == "configs" {
                self.w.warn(
                    "configs",
                    "top-level configs: not supported; use bind mounts of the files".into(),
                );
            } else if ks == "include" {
                self.w.warn(
                    "include",
                    "include: is not followed; merge the files first (`docker compose config`)"
                        .into(),
                );
            } else {
                self.w.warn(&ks, format!("top-level {ks}: ignored"));
            }
        }
        if let Some(v) = self.doc.get("version") {
            self.notes.push(format!(
                "version: {} ignored (the Compose specification has no versions)",
                pstr(v)
            ));
        }
        Ok(())
    }

    fn lock_load(&mut self) {
        self.lock_path = match &self.o.lock {
            Some(l) if !l.is_empty() => l.clone(),
            _ => join(&self.odir, "hydo-compose.lock.json"),
        };
        self.lock = std::fs::read(&self.lock_path)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| pyval::from_json(&v))
            .and_then(|v| v.get("images").cloned())
            .filter(Y::is_map)
            .unwrap_or(Y::Map(Vec::new()));
        self.lock_dirty = false;
    }

    /// `docker build`'s arguments for a Compose `build:` (a context, or a mapping with
    /// `context:`, `dockerfile:` and `target:`), with the paths relative to the output directory,
    /// where fetch-images.sh runs.
    fn build_args(&self, b: &Y) -> String {
        let (ctx, file, target) = if b.is_map() {
            let opt = |k: &str| b.get(k).filter(|v| v.truthy()).map(pstr);
            (
                opt("context").unwrap_or_else(|| ".".into()),
                opt("dockerfile"),
                opt("target"),
            )
        } else {
            (pstr(b), None, None)
        };
        let local = |p: &str| {
            if p.contains("://") || p.starts_with("git@") {
                p.to_string()
            } else {
                relpath(&join(&self.cdir, p), &self.odir)
            }
        };
        let cdir = local(&ctx);
        let mut a: Vec<String> = Vec::new();
        if let Some(f) = file {
            // dockerfile: is relative to the context
            let f = if cdir.contains("://") || f.starts_with('/') {
                f
            } else {
                relpath(&join(&join(&self.cdir, &ctx), &f), &self.odir)
            };
            a.push("-f".into());
            a.push(shlex::quote(&f));
        }
        if let Some(t) = target {
            a.push("--target".into());
            a.push(shlex::quote(&t));
        }
        a.push(shlex::quote(&cdir));
        a.join(" ")
    }

    /// The image of `r` is already pinned without a registry: by the lock file, or by an image
    /// file in the images directory (`<stem>-amd64.docker.tar`, read by its content).
    fn image_known(&self, r: &str) -> R<bool> {
        if self.lock.get(r).is_some() {
            return Ok(true);
        }
        let (reg, repo, tag, digest) = parse_ref(r);
        let stem = file_stem(&reg, &repo, tag.as_deref(), digest.as_deref())?;
        let idir = join(
            &self.odir,
            self.o
                .images_dir
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("images"),
        );
        Ok(exists(&join(&idir, &format!("{stem}-amd64.docker.tar"))))
    }

    /// The image of a service built locally whose file is not there yet: no platforms, no
    /// digest, never looked up in a registry (`local_build`).
    fn local_info(&self, r: &str) -> R<Y> {
        let (reg, repo, tag, digest) = parse_ref(r);
        file_stem(&reg, &repo, tag.as_deref(), digest.as_deref())?;
        Ok(Y::Map(vec![
            (Y::s("ref"), Y::s(r)),
            (Y::s("name"), Y::Str(short_name(&reg, &repo))),
            (Y::s("tag"), opt_y(tag)),
            (Y::s("index_digest"), opt_y(digest)),
            (Y::s("registry"), Y::s(&reg)),
            (Y::s("repo"), Y::s(&repo)),
            (Y::s("platforms"), Y::Map(Vec::new())),
            (Y::s("local_build"), Y::Bool(true)),
        ]))
    }

    /// Docker Compose's project name (`name:`, else the Compose file's directory), as it names
    /// the image of a `build:` without `image:` (`<project>-<service>`).
    fn compose_project(&self) -> String {
        let raw = match (&self.o.name, self.doc.get("name")) {
            (Some(n), _) if !n.is_empty() => n.clone(),
            (_, Some(n)) if n.truthy() => pstr(n),
            _ => hydo_manifest::py::basename(&self.cdir),
        };
        let p: String = raw
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let p = p.trim_matches('-').to_string();
        if p.is_empty() {
            "compose".into()
        } else {
            p
        }
    }

    fn image_info(&mut self, r: &str) -> R<Y> {
        if let Some(i) = self.lock.get(r) {
            return Ok(i.clone());
        }
        let (reg, repo, tag, digest) = parse_ref(r);
        let stem = file_stem(&reg, &repo, tag.as_deref(), digest.as_deref())?;
        let idir = join(
            &self.odir,
            self.o
                .images_dir
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("images"),
        );
        let amd = join(&idir, &format!("{stem}-amd64.docker.tar"));
        let base = |plats: Y| {
            Y::Map(vec![
                (Y::s("ref"), Y::s(r)),
                (Y::s("name"), Y::Str(short_name(&reg, &repo))),
                (Y::s("tag"), opt_y(tag.clone())),
                (Y::s("index_digest"), opt_y(digest.clone())),
                (Y::s("registry"), Y::s(&reg)),
                (Y::s("repo"), Y::s(&repo)),
                (Y::s("platforms"), plats),
            ])
        };
        if exists(&amd) {
            let mut plats = Y::Map(vec![(
                Y::s("amd64"),
                ocilocal::config_from_file(&amd, "amd64")?,
            )]);
            let arm = join(&idir, &format!("{stem}-arm64-oci"));
            if exists(&arm) {
                plats.set(Y::s("arm64"), ocilocal::config_from_file(&arm, "arm64")?);
            }
            let mut info = base(plats);
            info.set(Y::s("from"), Y::s("local file"));
            return Ok(info);
        }
        if self.o.no_resolve {
            let mut info = base(Y::Map(Vec::new()));
            info.set(Y::s("unresolved"), Y::Bool(true));
            return Ok(info);
        }
        eprintln!("hydo-compose: resolving {r}");
        let info = registry::resolve_image(self.reg, r)?;
        self.lock.set(Y::s(r), info.clone());
        self.lock_dirty = true;
        Ok(info)
    }

    // ------------------------------------------------------------ services

    pub fn run(&mut self) -> R<String> {
        self.load()?;
        self.lock_load();
        let raw = self.doc.getn("services").clone();
        let mut order: Vec<(Y, String)> = Vec::new();
        for (n, s) in raw.map_items() {
            let ns = pstr(n);
            let ps = as_list(if s.truthy() { s.get("profiles") } else { None });
            if !ps.is_empty() && !ps.iter().any(|p| self.o.profiles.contains(&pstr(p))) {
                self.w.warn(
                    "profiles",
                    format!(
                        "service {ns} is in profile(s) {} and was left out (as `docker compose up` does); pass --profile {} to include it",
                        ps.iter().map(pstr).collect::<Vec<_>>().join(", "),
                        pstr(&ps[0])
                    ),
                );
                continue;
            }
            order.push((n.clone(), ns));
        }
        if order.is_empty() {
            return err("no service left after profiles");
        }
        // Several services, each with its own named volume on /data (the nodes of a cluster):
        // each gets that volume as its /data, instead of all sharing the data disk's root.
        // The same when one service has its volume on /data and another service keeps its own
        // named volume on the data disk: the data disk's root holds that other service's data, and
        // an image that takes over its /data (Redis's entrypoint chowns everything under its
        // working directory /data to its own user) would take it over too.
        let mut on_data: Vec<String> = Vec::new();
        let mut elsewhere = false;
        for (n, s) in raw.map_items() {
            if !order.iter().any(|(o, _)| o == n) || !s.is_map() {
                continue;
            }
            for v in as_list(s.get("volumes")) {
                if let Some(k) = named_volume_on_data(&v) {
                    if !on_data.contains(&k) {
                        on_data.push(k);
                    }
                } else if named_volume(&v).is_some() {
                    elsewhere = true;
                }
            }
        }
        self.own_data = on_data.len() > 1 || (!on_data.is_empty() && elsewhere);
        self.own_data_why = if on_data.len() > 1 {
            "several services have their own volume on /data"
        } else {
            "the data disk also holds another service's volume"
        };
        for (n, ns) in &order {
            if let Y::Str(_) = n {
            } else {
                return err(format!(
                    "service name {} is not a string; quote it in the Compose file",
                    repr(n)
                ));
            }
            let hn = svc_name(ns);
            if let Some((other, _)) = self.names.iter().find(|(_, v)| *v == hn) {
                return err(format!(
                    "services {ns} and {other} map to the same hydo name {hn}; rename one"
                ));
            }
            self.names.push((ns.clone(), hn.clone()));
            if hn != *ns {
                self.notes.push(format!(
                    "Compose service {} is hydo service {} (fault tokens use {hn}; the name {ns} still resolves through /etc/hosts)",
                    hydo_manifest::py::repr_str(ns),
                    hydo_manifest::py::repr_str(&hn)
                ));
            }
        }
        if order.len() > 250 - ADDR_BASE {
            return err("too many services for one /24");
        }
        for (i, (_, ns)) in order.iter().enumerate() {
            let a = if self.o.no_addr {
                "127.0.0.1".to_string()
            } else if self.bridge {
                format!("10.89.0.{}", ADDR_BASE + i)
            } else {
                format!("127.0.0.{}", ADDR_BASE + i)
            };
            self.addr.push((ns.clone(), a));
        }
        let jobs = self.find_jobs(&order, &raw);
        for (n, ns) in &order {
            let s = raw.get(ns).cloned().unwrap_or(Y::Null);
            let s = if s.truthy() { s } else { Y::Map(Vec::new()) };
            if !s.is_map() {
                return err(format!("service {ns}: must be a mapping, not {}", repr(&s)));
            }
            let _ = n;
            let svc = self.service(ns, &s, &jobs)?;
            self.svcs.push(svc);
        }
        let vt = self.doc.getn("volumes").clone();
        for (v, d) in vt.map_items() {
            if d.is_map() && d.getn("external").truthy() {
                self.w.warn(
                    "volumes",
                    format!(
                        "volume {} is external: it starts empty in the guest (no data is imported)",
                        pstr(v)
                    ),
                );
            }
            if d.is_map() {
                let drv = d.getn("driver");
                if !drv.is_none() && *drv != Y::s("local") {
                    self.w.warn(
                        "volumes",
                        format!(
                            "volume {}: driver {} ignored; it is a directory on the data disk",
                            pstr(v),
                            pstr(drv)
                        ),
                    );
                }
            }
        }
        let nets = self.doc.getn("networks").clone();
        let nets = if nets.truthy() {
            nets
        } else {
            Y::Map(Vec::new())
        };
        let custom: Vec<String> = nets
            .iter_keys()
            .iter()
            .filter(|k| **k != Y::s("default"))
            .map(pstr)
            .collect();
        if !custom.is_empty() {
            self.w.warn(
                "networks",
                format!(
                    "networks {}: all services share one {} network in the guest, so the isolation between Compose networks is not modelled (every service can reach every other)",
                    custom.join(", "),
                    if self.bridge { "bridge" } else { "loopback" }
                ),
            );
        }
        if !self.bridge {
            self.port_collisions();
        }
        Ok(self.emit())
    }

    fn find_jobs(&self, order: &[(Y, String)], raw: &Y) -> Vec<String> {
        let mut jobs = Vec::new();
        for (_, ns) in order {
            let dep = raw.getn(ns).getn("depends_on");
            for (d, c) in dep.map_items() {
                if c.is_map() && *c.getn("condition") == Y::s("service_completed_successfully") {
                    let ds = pstr(d);
                    if !jobs.contains(&ds) {
                        jobs.push(ds);
                    }
                }
            }
        }
        jobs
    }

    fn name_of(&self, n: &str) -> Option<&String> {
        lookup(&self.names, n)
    }

    fn addr_of(&self, n: &str) -> String {
        lookup(&self.addr, n).cloned().unwrap_or_default()
    }

    fn service(&mut self, n: &str, s: &Y, jobs: &[String]) -> R<Svc> {
        let wh = format!("service {n}");
        let hn = self.name_of(n).cloned().unwrap_or_default();
        let mut comments: Vec<String> = Vec::new();
        for k in s.iter_keys() {
            let ks = pstr(&k);
            if HANDLED.contains(&ks.as_str())
                || IGNORED.contains(&ks.as_str())
                || ks.starts_with("x-")
            {
                continue;
            }
            if ks == "network_mode" && s.get("network_mode").map(pstr).as_deref() == Some("host") {
                // Docker's host network: every service sees every other on one localhost, so
                // the program's own arguments often name peers as 0.0.0.0:<port> or
                // localhost:<port>, which point at the service itself inside its own namespace.
                let how = if self.bridge {
                    "here each service has its own network namespace and addr:, so a peer named as 0.0.0.0:<port> or localhost:<port> in a command or configuration must become that peer's addr: (or import again with --network loopback: one shared loopback network)"
                } else {
                    "with --network loopback every service shares one loopback network, as with Docker's host network"
                };
                self.w.warn(&ks, format!("{wh}: network_mode: host: {how}"));
            } else if let Some((_, why)) = UNSUPPORTED.iter().find(|(u, _)| *u == ks) {
                self.w.warn(&ks, format!("{wh}: {ks}: {why}"));
            } else {
                self.w.warn(
                    &ks,
                    format!("{wh}: {ks}: not understood by hydo-compose; ignored"),
                );
            }
        }
        // A service built locally (`build:`): its image is a file the user makes with Docker (or
        // an OCI layout), never a pull of the tag, which usually names only the local build.
        // An image file already in the images directory, or a lock entry, pins it as before; a
        // tag the registry has (a published image the Compose file also builds) is pinned by
        // digest as before; otherwise the service is imported as a local build, and
        // fetch-images.sh says how to make the file.
        let mut local_build: Option<String> = None;
        let rf = if s.has("build") {
            let ctxt = self.build_args(s.getn("build"));
            match s.get("image") {
                Some(img) => {
                    let rf = pstr(img);
                    if !self.image_known(&rf)? {
                        local_build = Some(ctxt);
                    }
                    rf
                }
                None => {
                    // Docker Compose names the image of a build without image: <project>-<service>
                    local_build = Some(ctxt);
                    format!(
                        "{}-{}",
                        self.compose_project(),
                        safe_dash(&n.to_lowercase())
                    )
                }
            }
        } else {
            let Some(img) = s.get("image") else {
                return err(format!("{wh}: no image:"));
            };
            pstr(img)
        };
        let info = match &local_build {
            Some(_) if !self.o.no_resolve && s.get("image").is_some() => {
                // a tag the registry may have; a refusal (401, 404) leaves it a local build
                match self.image_info(&rf) {
                    Ok(i) => {
                        local_build = None;
                        i
                    }
                    Err(e) => {
                        self.w.warn(
                            "build",
                            format!("{wh}: image {rf} is not in its registry ({e}): imported as a local build"),
                        );
                        self.local_info(&rf)?
                    }
                }
            }
            Some(_) => self.local_info(&rf)?,
            None => self.image_info(&rf)?,
        };
        if s.has("build") && local_build.is_none() && pstr(info.getn("from")) != "local file" {
            self.w.warn("build", format!("{wh}: build: ignored, the image {rf} is used (build and push it, or `docker save` your local build and point image: at the file)"));
        }
        let (reg, repo, tag, digest) = parse_ref(&rf);
        let stem = file_stem(&reg, &repo, tag.as_deref(), digest.as_deref())?;
        let idir = self
            .o
            .images_dir
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "images".into());
        let plat = info.getn("platforms").clone();
        if let Some(p) = s.get("platform").filter(|p| p.truthy()) {
            let ps = pstr(p);
            let second = ps.split('/').nth(1);
            if !matches!(second, Some("amd64") | Some("arm64")) {
                self.w.warn("platform", format!("{wh}: platform {ps}: hydo runs linux/amd64 (x86_64) and linux/arm64 (Mac) only"));
            }
        }
        let x86 = format!("{idir}/{stem}-amd64.docker.tar");
        let unresolved = info.getn("unresolved").truthy() || local_build.is_some();
        let arm = format!("{idir}/{stem}-arm64-oci");
        if let Some(b) = &local_build {
            comments.push(format!("image {rf}: built locally (build:), NOT pinned yet: fetch-images.sh says how to make its file; run hydo compose again once it is there to pin it by the file's content"));
            self.w.warn(
                "build",
                format!("{wh}: image {rf} is built locally (build:): make its file with `docker build -t {rf} {b} && docker save -o {x86} {rf}` (on a Mac, or for arm64: an OCI layout of it at {arm}), then run hydo compose again to pin it by the file's content"),
            );
        }
        let image = if plat.has("arm64") || unresolved {
            Img::Two(x86, arm)
        } else {
            Img::One(x86)
        };
        let iname = pstr(info.getn("name"));
        let image_ref = match &tag {
            Some(t) if !t.is_empty() => format!("{iname}:{t}"),
            _ => iname.clone(),
        };
        if unresolved {
            if local_build.is_none() {
                comments.push(format!("image {rf}: NOT resolved to a digest (--no-resolve); fetch-images.sh fetches the tag, which can change"));
                self.w.warn(
                    "image",
                    format!("{wh}: image {rf} is not pinned by digest (--no-resolve)"),
                );
            }
        } else {
            comments.push(format!(
                "image {rf} = {}/{}@{}",
                pstr(info.getn("registry")),
                pstr(info.getn("repo")),
                pstr(info.getn("index_digest"))
            ));
            for a in ["amd64", "arm64"] {
                if let Some(p) = plat.get(a) {
                    let var = pstr(p.getn("variant"));
                    let var = if p.getn("variant").truthy() {
                        format!("/{var}")
                    } else {
                        String::new()
                    };
                    comments.push(format!("  linux/{a}{var}: {}", pstr(p.getn("digest"))));
                }
            }
        }
        let am = plat
            .get("amd64")
            .filter(|x| x.truthy())
            .cloned()
            .unwrap_or(Y::Map(vec![]));
        let cfg = am
            .get("config")
            .filter(|x| x.truthy())
            .cloned()
            .unwrap_or(Y::Map(vec![]));
        let av = am.getn("variant");
        if *av == Y::s("v3") || *av == Y::s("v4") {
            self.cpu = Some("v3".into());
            comments.push(format!(
                "linux/amd64 variant {}: needs AVX2 (guest.cpu: v3)",
                pstr(av)
            ));
            if *av == Y::s("v4") {
                self.w.warn(
                    "cpu",
                    format!(
                        "{wh}: the image is built for x86-64-v4 (AVX-512); hydo offers v3 at most"
                    ),
                );
            }
        }
        let base = repo.rsplit('/').next().unwrap_or("").to_string();
        let fam = FAMILIES.iter().find(|f| re(f.0).is_match(&base));
        if let Some(f) = fam {
            if f.3 == Some("v3") {
                self.cpu = Some("v3".into());
                comments.push(format!("{base} is known to need AVX2 (guest.cpu: v3)"));
            }
        }
        let ent = words(s.get("entrypoint"), &format!("{wh} entrypoint"))?;
        let cmd = words(s.get("command"), &format!("{wh} command"))?;
        let mut svc = Svc {
            name: hn.clone(),
            compose: n.to_string(),
            comments,
            info: info.clone(),
            image,
            image_ref,
            entrypoint: ent.clone(),
            cmd: cmd.clone(),
            job: None,
            env: Vec::new(),
            workdir: None,
            user: None,
            port: None,
            listen: Vec::new(),
            addr: String::new(),
            noaddr: false,
            depends_on: Vec::new(),
            setup: None,
            ready: String::new(),
            ready_timeout: None,
            recovery_false: false,
            restart: None,
            mounts: Vec::new(),
            build: local_build.clone(),
        };
        let is_job = jobs.iter().any(|j| j == n);
        if is_job {
            self.job(&mut svc, &cfg, ent.as_deref(), cmd.as_deref(), &wh);
        }
        // env: env_file first, environment overrides
        let mut env: Vec<(String, String)> = Vec::new();
        let put = |env: &mut Vec<(String, String)>, k: String, v: String| {
            if let Some(e) = env.iter_mut().find(|(x, _)| *x == k) {
                e.1 = v;
            } else {
                env.push((k, v));
            }
        };
        for ef in as_list(s.get("env_file")) {
            let (path, req) = if ef.is_map() {
                (
                    ef.getn("path").clone(),
                    ef.get("required").cloned().unwrap_or(Y::Bool(true)),
                )
            } else {
                (ef.clone(), Y::Bool(true))
            };
            let Y::Str(path) = path else {
                return err(format!(
                    "{wh}: env_file path {} is not a string",
                    repr(&path)
                ));
            };
            let p = if isabs(&path) {
                path.clone()
            } else {
                join(&self.cdir, &path)
            };
            let (e, bare) = parse_env_file(&p, req.truthy())?;
            for (k, v) in e {
                put(&mut env, k, unesc(&v));
            }
            for k in bare {
                self.w.warn(
                    "env_file",
                    format!("{wh}: {k} in {path} has no value; left out"),
                );
            }
        }
        for (k, v) in self.env_of(s.get("environment"), &wh)? {
            put(&mut env, k, unesc(&v));
        }
        svc.env = env;
        if let Some(wd) = s.get("working_dir").filter(|x| x.truthy()) {
            svc.workdir = Some(pstr(wd));
        }
        if let Some(u) = s.get("user").filter(|x| !x.is_none()) {
            svc.user = Some(pstr(u));
        }
        let user = match &svc.user {
            Some(u) => u.clone(),
            None => {
                let u = cfg.getn("User");
                if u.truthy() {
                    pstr(u)
                } else {
                    String::new()
                }
            }
        };
        // ports
        let ports = self.container_ports(s, &wh);
        let mut exposed: Vec<i128> = cfg
            .getn("ExposedPorts")
            .iter_keys()
            .iter()
            .filter_map(|p| {
                let p = pstr(p);
                let h = p.split('/').next().unwrap_or("").to_string();
                (!h.is_empty() && h.chars().all(|c| c.is_ascii_digit()))
                    .then(|| h.parse().ok())
                    .flatten()
            })
            .collect();
        exposed.sort();
        if let Some(&p0) = ports.first() {
            svc.port = Some(p0);
            let mut set: Vec<i128> = ports.clone();
            set.sort();
            set.dedup();
            if set.len() > 1 {
                svc.comments.push(format!(
                    "ports {}: network faults (N=/O=) act on the first, {p0}",
                    int_list(&set)
                ));
            }
        } else if let Some(&e0) = exposed.first() {
            svc.port = Some(e0);
            svc.comments.push(format!(
                "port from the image's EXPOSE {}",
                int_list(&exposed)
            ));
        }
        let mut listen: Vec<i128> = ports.iter().chain(&exposed).copied().collect();
        listen.sort();
        listen.dedup();
        svc.listen = listen;
        svc.addr = self.addr_of(n);
        if self.o.no_addr {
            svc.noaddr = true;
        }
        // names
        let mut aliases: Vec<String> = vec![n.to_string()];
        for k in ["container_name", "hostname"] {
            if let Some(v) = s.get(k).filter(|x| x.truthy()) {
                aliases.push(pstr(v));
            }
        }
        if let Some(nets) = s.get("networks").filter(|x| x.is_map()) {
            for (_, nd) in nets.map_items() {
                if nd.is_map() {
                    aliases.extend(as_list(nd.get("aliases")).iter().map(pstr));
                    if let Some(ip) = nd.get("ipv4_address").filter(|x| x.truthy()) {
                        self.w.warn(
                            "networks",
                            format!(
                                "{wh}: ipv4_address {} ignored; the service is at {}",
                                pstr(ip),
                                self.addr_of(n)
                            ),
                        );
                    }
                }
            }
        }
        let mut uniq: Vec<String> = Vec::new();
        for a in aliases {
            if !uniq.contains(&a) {
                uniq.push(a);
            }
        }
        self.hosts.push((self.addr_of(n), uniq));
        for ln in as_list(s.get("links")) {
            let l = pstr(&ln);
            let (tgt, al) = l.split_once(':').unwrap_or((&l, ""));
            if !al.is_empty() {
                if let Some(a) = lookup(&self.addr, tgt).cloned() {
                    self.hosts.push((a, vec![al.to_string()]));
                }
            }
        }
        let eh = s.getn("extra_hosts").clone();
        let pairs: Vec<(String, Y)> = if eh.is_map() {
            eh.map_items()
                .iter()
                .map(|(h, ip)| (pstr(h), ip.clone()))
                .collect()
        } else {
            as_list(Some(&eh))
                .iter()
                .map(|x| {
                    let t = pstr(x);
                    match t.find([':', '=']) {
                        Some(i) => (t[..i].to_string(), Y::Str(t[i + 1..].to_string())),
                        None => (t.clone(), Y::s("")),
                    }
                })
                .collect()
        };
        for (h, ip) in pairs {
            let ip = if ip == Y::s("host-gateway") || ip == Y::s("") {
                if self.bridge { BRIDGE_GW } else { "127.0.0.1" }.to_string()
            } else {
                pstr(&ip)
            };
            self.hosts.push((ip, vec![h]));
        }
        // depends_on
        let dep = s.getn("depends_on").clone();
        let mut deps: Vec<String> = Vec::new();
        match &dep {
            Y::Seq(a) => deps = a.iter().map(pstr).collect(),
            Y::Map(m) => {
                for (d, c) in m {
                    let ds = pstr(d);
                    deps.push(ds.clone());
                    if c.is_map() {
                        let cond = c
                            .get("condition")
                            .cloned()
                            .unwrap_or(Y::s("service_started"));
                        if cond == Y::s("service_started") {
                            svc.comments.push(format!(
                                "depends_on {ds} (service_started): hydo waits until {ds} is ready"
                            ));
                        }
                        if matches!(c.get("required"), Some(Y::Bool(false))) {
                            self.w.warn("depends_on", format!("{wh}: depends_on {ds} required: false -- hydo always starts it"));
                        }
                    }
                }
            }
            _ => {}
        }
        let mut deps2 = Vec::new();
        for d in deps {
            match self.name_of(&d) {
                Some(h) => deps2.push(h.clone()),
                None => {
                    if self.doc.getn("services").contains(&Y::Str(d.clone())) {
                        self.w.warn(
                            "depends_on",
                            format!("{wh}: depends on {d}, which was left out (profiles)"),
                        );
                        continue;
                    }
                    return err(format!("{wh}: depends_on names unknown service {d}"));
                }
            }
        }
        svc.depends_on = deps2;
        // healthcheck -> ready
        if is_job {
            svc.ready = "true".into();
        } else {
            self.readiness(&mut svc, s, &cfg, &user, &wh)?;
        }
        // restart (or deploy.restart_policy) -> restart: and recovery (M134)
        let (rs, from) = self.restart_of(s, &wh);
        if let Some(r) = rs {
            // where it came from, unless that is restart: with the same words
            let src = if from.starts_with("restart: ") {
                String::new()
            } else {
                format!("{from} -> ")
            };
            if r == "no" {
                svc.recovery_false = true;
                svc.comments.push(format!("{src}restart: \"no\": the service stays down when its process ends by itself, until the heal; and recovery: false (not started again after a power cut, as Docker does not restart it after a reboot; hydo still restarts it after a kill fault and at the heal)"));
            } else {
                svc.comments.push(format!("{src}restart: {r}: an agent image starts the service again when its process ends by itself while the test runs (MANIFEST.md \"Restart policies\"); hydo's kill faults keep their own restart points"));
            }
            svc.restart = Some(r);
        }
        // volumes, secrets, tmpfs, writable /run + /dev/shm
        self.mounts(&mut svc, s, &cfg, n, &wh)?;
        // RAM, data disk
        let deploy = s.getn("deploy").clone();
        let lim = match mem_mib(s.get("mem_limit"))? {
            Some(x) if x != 0 => Some(x),
            _ => {
                let res = deploy.getn("resources");
                let lims = res.get("limits").cloned().unwrap_or(Y::Map(vec![]));
                mem_mib(lims.get("memory"))?.filter(|x| *x != 0)
            }
        };
        let r = lim.unwrap_or(fam.map(|f| f.1).unwrap_or(DEFAULT_SVC_RAM));
        self.ram += r;
        if svc
            .mounts
            .iter()
            .any(|m| m.src.as_deref().unwrap_or("").starts_with("/data/"))
        {
            self.data_mib += fam.map(|f| f.2).unwrap_or(128);
        }
        for k in deploy.iter_keys() {
            let ks = pstr(&k);
            if ks == "resources" {
                continue;
            }
            if ks == "replicas" {
                let v = deploy.getn("replicas");
                let n = int_of(v).ok_or_else(|| {
                    ComposeError(format!("{wh}: deploy.replicas {}: not a number", pstr(v)))
                })?;
                if n > 1 {
                    self.w.warn("deploy", format!("{wh}: deploy.replicas {}: one instance is started; copy the service for more", pstr(v)));
                }
            } else if !matches!(ks.as_str(), "restart_policy" | "labels") {
                self.w.warn(
                    "deploy",
                    format!("{wh}: deploy.{ks} ignored (Swarm settings)"),
                );
            }
        }
        let resv = deploy
            .getn("resources")
            .get("reservations")
            .cloned()
            .unwrap_or(Y::Map(vec![]));
        if resv.getn("devices").truthy() {
            self.w.warn(
                "deploy",
                format!("{wh}: device reservations (GPUs) are not available"),
            );
        }
        Ok(svc)
    }

    /// environment: a list of KEY=VAL / KEY, or a mapping -> ordered (values as str).
    fn env_of(&mut self, v: Option<&Y>, wh: &str) -> R<Vec<(String, String)>> {
        let mut out: Vec<(String, String)> = Vec::new();
        let items: Vec<(String, Option<Y>)> = match v {
            None | Some(Y::Null) => return Ok(out),
            Some(Y::Map(m)) => m.iter().map(|(k, v)| (pstr(k), Some(v.clone()))).collect(),
            Some(Y::Seq(a)) => a
                .iter()
                .map(|e| {
                    let e = pstr(e);
                    match e.split_once('=') {
                        Some((k, v)) => (k.to_string(), Some(Y::s(v))),
                        None => (e, None),
                    }
                })
                .collect(),
            Some(_) => return err(format!("{wh}: environment must be a list or a mapping")),
        };
        for (k, val) in items {
            let val = match val {
                None | Some(Y::Null) => {
                    match self.interp.get(&k, &mut self.w) {
                        Some(v) => {
                            set_kv(&mut out, k, v);
                        }
                        None => self.w.warn("environment", format!("{wh}: {k} has no value and is not in .env (Compose would take it from the shell); it was left out -- give it a value in .env, or pass --use-env")),
                    }
                    continue;
                }
                Some(v) => v,
            };
            let s = match val {
                Y::Bool(b) => if b { "true" } else { "false" }.to_string(),
                other => pstr(&other),
            };
            set_kv(&mut out, k, s);
        }
        Ok(out)
    }

    fn container_ports(&mut self, s: &Y, wh: &str) -> Vec<i128> {
        let mut out = Vec::new();
        for p in as_list(s.get("ports")) {
            let (t, proto) = if p.is_map() {
                (
                    p.getn("target").clone(),
                    p.get("protocol").cloned().unwrap_or(Y::s("tcp")),
                )
            } else {
                let mut st = pstr(&p);
                let mut proto = "tcp".to_string();
                if let Some(i) = st.rfind('/') {
                    proto = st[i + 1..].to_string();
                    st.truncate(i);
                }
                let t = st.rsplit(':').next().unwrap_or("").to_string();
                (Y::Str(t), Y::Str(proto))
            };
            if proto != Y::s("tcp") {
                self.w.warn(
                    "ports",
                    format!(
                        "{wh}: {} port {}: network faults act on TCP/UDP to that port alike; fine",
                        pstr(&proto),
                        pstr(&t)
                    ),
                );
            }
            let ts = pstr(&t);
            let rng: Vec<&str> = ts.split('-').collect();
            match py_int(rng[0]) {
                Some(x) => out.push(x),
                None => {
                    self.w.warn(
                        "ports",
                        format!(
                            "{wh}: cannot read the container port of {}; ignored",
                            repr(&p)
                        ),
                    );
                    continue;
                }
            }
            if rng.len() > 1 {
                self.w.warn(
                    "ports",
                    format!(
                        "{wh}: port range {ts}: network faults act on the first port only ({})",
                        rng[0]
                    ),
                );
            }
        }
        for p in as_list(s.get("expose")) {
            let ps = pstr(&p);
            let h = ps.split('/').next().unwrap_or("");
            let h = h.split('-').next().unwrap_or("");
            match py_int(h) {
                Some(x) => out.push(x),
                None => self
                    .w
                    .warn("expose", format!("{wh}: cannot read {}; ignored", repr(&p))),
            }
        }
        out
    }

    /// A one-shot service other services wait for (service_completed_successfully): it idles in
    /// the guest, and its command runs in the setup of each dependent (once per boot).
    fn job(
        &mut self,
        svc: &mut Svc,
        cfg: &Y,
        ent: Option<&[String]>,
        cmd: Option<&[String]>,
        wh: &str,
    ) {
        let strs = |v: &Y| -> Vec<String> {
            match v {
                Y::Seq(a) => a.iter().map(pstr).collect(),
                Y::Str(s) => s.chars().map(|c| c.to_string()).collect(),
                _ => Vec::new(),
            }
        };
        let e: Vec<String> = match ent {
            Some(e) => e.to_vec(),
            None => strs(cfg.getn("Entrypoint")),
        };
        let c: Vec<String> = match (cmd, ent) {
            (Some(c), _) => c.to_vec(),
            (None, Some(_)) => Vec::new(),
            (None, None) => strs(cfg.getn("Cmd")),
        };
        let argv: Vec<String> = e.into_iter().chain(c).collect();
        if argv.is_empty() {
            self.w.warn("job", format!("{wh}: is a one-shot job but its command is unknown (image not resolved); fill in the job command in the dependents' setup:"));
        }
        svc.job = Some(argv);
        svc.entrypoint = Some(vec![
            "/.hydo/busybox".into(),
            "sleep".into(),
            "2147483647".into(),
        ]);
        svc.cmd = None;
        svc.comments.push("a one-shot job (others depend on it with service_completed_successfully): it idles here and its command runs in the setup: of each service that waits for it".into());
    }

    /// The service's restart policy in Compose's words and where it came from: `deploy.
    /// restart_policy` when it is set (Compose then ignores `restart:`), else `restart:`.
    fn restart_of(&mut self, s: &Y, wh: &str) -> (Option<String>, String) {
        let rp = s.getn("deploy").getn("restart_policy").clone();
        if let Some(c) = rp.get("condition").filter(|c| !c.is_none()) {
            let cond = pstr(c);
            let max = rp.get("max_attempts").and_then(int_of).filter(|n| *n > 0);
            let r = match cond.as_str() {
                "none" => Some("no".to_string()),
                "any" => Some("always".to_string()),
                "on-failure" => Some(match max {
                    Some(n) => format!("on-failure:{n}"),
                    None => "on-failure".to_string(),
                }),
                _ => None,
            };
            if r.is_none() {
                self.w.warn(
                    "deploy",
                    format!("{wh}: deploy.restart_policy.condition {cond}: not none, on-failure or any; no restart policy written"),
                );
            }
            for k in ["delay", "window"] {
                if rp.get(k).is_some_and(|v| !v.is_none()) {
                    self.w.warn(
                        "deploy",
                        format!("{wh}: deploy.restart_policy.{k} ignored: hydo waits Docker's own restart delays (100 ms, doubled after each restart, at most 60 s)"),
                    );
                }
            }
            return (r, format!("deploy.restart_policy.condition {cond}"));
        }
        match s.get("restart").filter(|v| !v.is_none()) {
            None => (None, String::new()),
            Some(v) => {
                let r = compose_restart(v);
                if r.is_none() {
                    self.w.warn(
                        "restart",
                        format!("{wh}: restart: {}: not a Compose restart policy (no, always, on-failure[:max], unless-stopped); no restart policy written", pstr(v)),
                    );
                }
                (r, format!("restart: {}", pstr(v)))
            }
        }
    }

    fn readiness(&mut self, svc: &mut Svc, s: &Y, cfg: &Y, user: &str, wh: &str) -> R<()> {
        let mut hc = s.get("healthcheck").cloned();
        let mut src = "Compose healthcheck";
        let use_image = match &hc {
            None | Some(Y::Null) => true,
            Some(h) if h.is_map() => !h.has("test") && !h.getn("disable").truthy(),
            _ => false,
        };
        if use_image {
            let ih = cfg.getn("Healthcheck");
            if ih.truthy() && ih.getn("Test").truthy() {
                let ns = |k: &str| -> Y {
                    let v = int_of(ih.getn(k)).unwrap_or(0).div_euclid(1_000_000_000);
                    if v == 0 {
                        Y::Null
                    } else {
                        Y::Int(v)
                    }
                };
                hc = Some(Y::Map(vec![
                    (Y::s("test"), ih.getn("Test").clone()),
                    (Y::s("timeout"), ns("Timeout")),
                    (Y::s("interval"), ns("Interval")),
                    (Y::s("retries"), ih.getn("Retries").clone()),
                    (Y::s("start_period"), ns("StartPeriod")),
                ]));
                src = "the image's HEALTHCHECK";
            }
        }
        let mut test: Option<Vec<String>> = None;
        if let Some(h) = hc
            .as_ref()
            .filter(|h| h.is_map() && !h.getn("disable").truthy())
        {
            match h.getn("test") {
                Y::Seq(t) if !t.is_empty() => {
                    let t0 = pstr(&t[0]);
                    if t[0] == Y::s("NONE") {
                        test = None;
                    } else if t[0] == Y::s("CMD") {
                        test = Some(t[1..].iter().map(|x| unesc(&pstr(x))).collect());
                    } else if t[0] == Y::s("CMD-SHELL") {
                        let j = t[1..].iter().map(pstr).collect::<Vec<_>>().join(" ");
                        test = Some(vec!["/bin/sh".into(), "-c".into(), unesc(&j)]);
                    } else {
                        let _ = t0;
                        test = Some(t.iter().map(|x| unesc(&pstr(x))).collect());
                    }
                }
                Y::Str(t) if !t.is_empty() => {
                    test = Some(vec!["/bin/sh".into(), "-c".into(), unesc(t)]);
                }
                _ => {}
            }
        }
        let hn = svc.name.clone();
        if let Some(test) = test.filter(|t| !t.is_empty()) {
            let h = hc.unwrap_or(Y::Null);
            let to = duration_s(h.get("timeout"), 30)?;
            let pre: Vec<String> = if !user.is_empty() && !matches!(user, "root" | "0" | "0:0") {
                vec![
                    "/.hydo/busybox".into(),
                    "chpst".into(),
                    "-u".into(),
                    user.into(),
                ]
            } else {
                Vec::new()
            };
            let words: Vec<String> = pre.iter().chain(&test).map(|x| shlex::quote(x)).collect();
            svc.ready = format!("timeout {} hydo-oci {hn} {}", to.min(30), words.join(" "));
            let retries = {
                let r = h.getn("retries");
                if r.truthy() {
                    int_of(r).ok_or_else(|| {
                        ComposeError(format!(
                            "{wh}: healthcheck retries {}: not a number",
                            pstr(r)
                        ))
                    })?
                } else {
                    3
                }
            };
            let budget = duration_s(h.get("start_period"), 0)?
                + duration_s(h.get("interval"), 30)? * retries;
            svc.ready_timeout = Some(120.max(600.min(budget + 60)));
            svc.comments.push(format!("ready: from {src}"));
        } else if let Some(p) = svc.port.filter(|p| *p != 0) {
            svc.ready = format!("nc -z -w 2 {} {p}", svc.addr);
            svc.comments.push("ready: no healthcheck; a TCP connect to its port (TODO: a real readiness check, e.g. a query, if the port opens before the service can serve)".into());
        } else {
            svc.ready = "true".into();
            svc.comments
                .push("ready: TODO no healthcheck and no port -- `true` means started".into());
            if svc.job.is_none() {
                self.w.warn("healthcheck", format!("{wh}: no healthcheck and no port: readiness is just `started`; write a ready: check"));
            }
        }
        Ok(())
    }

    fn add_mount(
        &mut self,
        mts: &mut Vec<Mount>,
        seen: &mut Vec<String>,
        mut m: Mount,
        what: &str,
        wh: &str,
    ) {
        let d = hydo_manifest::py::normpath(&m.dst);
        if !match_dollar(path_ok_re(), &d) {
            self.w.warn("volumes", format!("{wh}: {what} target {} has characters hydo cannot mount (spaces, quotes); skipped", hydo_manifest::py::repr_str(&d)));
            return;
        }
        let own_volume = self.own_data
            && what.starts_with("volume ")
            && what != "volume (anonymous)"
            && m.src
                .as_deref()
                .is_some_and(|s| s.starts_with("/data/vol/"));
        if d == "/data" && own_volume {
            // hydo-build mounts a data-disk directory over the image's /data (no copy_up: the
            // image's own /data is never visible, /data is the data disk)
            self.notes.push(format!(
                "{wh}: {what} on /data: {}, so it is mounted as this service's /data",
                self.own_data_why
            ));
            m.dst = d.clone();
            m.copy_up = false;
            seen.push(d);
            mts.push(m);
            return;
        }
        if matches!(
            d.as_str(),
            "/" | "/proc" | "/sys" | "/dev" | "/tmp" | "/data"
        ) {
            if d == "/data" {
                self.w.warn("volumes", format!("{wh}: {what} on /data: the image's /data is already the data disk's root (shared by every service); the mount was not added"));
            } else if d == "/tmp" {
                self.notes.push(format!(
                    "{wh}: {what} on /tmp: /tmp is shared with the guest (and every service)"
                ));
            } else {
                self.w.warn(
                    "volumes",
                    format!("{wh}: {what} on {d} cannot be mounted; skipped"),
                );
            }
            return;
        }
        if seen.contains(&d) {
            mts.retain(|x| x.dst != d);
        }
        seen.push(d.clone());
        m.dst = d;
        mts.push(m);
    }

    fn mounts(&mut self, svc: &mut Svc, s: &Y, cfg: &Y, n: &str, wh: &str) -> R<()> {
        let mut mts = vec![
            Mount::src("/etc/hosts", "/etc/hosts", false),
            Mount {
                tmpfs: true,
                dst: "/run".into(),
                copy_up: true,
                ..Default::default()
            },
            Mount::tmpfs("/dev/shm", Some("64m".into())),
        ];
        let mut seen: Vec<String> = vec!["/etc/hosts".into(), "/run".into(), "/dev/shm".into()];
        for v in as_list(s.get("volumes")) {
            self.volume(&v, &mut mts, &mut seen, n, wh)?;
        }
        static SZ: OnceLock<Regex> = OnceLock::new();
        let sz = SZ.get_or_init(|| re(r"size=(\d+[kmg]?)"));
        for v in as_list(s.get("tmpfs")) {
            let vs = pstr(&v);
            let (p, opt) = vs.split_once(':').unwrap_or((&vs, ""));
            let size = sz.captures(opt).map(|c| c[1].to_string());
            self.add_mount(&mut mts, &mut seen, Mount::tmpfs(p, size), "tmpfs", wh);
        }
        for sec in as_list(s.get("secrets")) {
            let (name, tgt) = if sec.is_map() {
                (sec.getn("source").clone(), sec.getn("target").clone())
            } else {
                (Y::Str(pstr(&sec)), Y::Null)
            };
            let secrets = self.doc.getn("secrets").clone();
            let d = secrets
                .map_items()
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
                .filter(Y::truthy)
                .unwrap_or(Y::Map(vec![]));
            let file = d.getn("file");
            if !file.truthy() {
                self.w.warn("secrets", format!("{wh}: secret {} has no file: (environment/external secrets are not supported); skipped", pstr(&name)));
                continue;
            }
            let f = pstr(file);
            let src = if isabs(&f) { f } else { join(&self.cdir, &f) };
            let tgt = match &tgt {
                Y::Str(t) if t.starts_with('/') => t.clone(),
                t => format!(
                    "/run/secrets/{}",
                    if t.truthy() { pstr(t) } else { pstr(&name) }
                ),
            };
            if let Some(gp) = self.copy_bind(&src) {
                self.add_mount(
                    &mut mts,
                    &mut seen,
                    Mount::src(&gp, &tgt, false),
                    &format!("secret {}", pstr(&name)),
                    wh,
                );
            }
        }
        let covered: Vec<String> = mts.iter().map(|m| m.dst.clone()).collect();
        let mut vols: Vec<String> = cfg.getn("Volumes").iter_keys().iter().map(pstr).collect();
        vols.sort();
        for v in vols {
            let vv = hydo_manifest::py::normpath(&v);
            if covered.contains(&vv)
                || covered
                    .iter()
                    .any(|c| c != "/" && vv.starts_with(&format!("{c}/")))
            {
                continue;
            }
            if vv == "/data" {
                continue; // the image's /data is the data disk anyway
            }
            let k = format!("anon-{n}-{}", vv.trim_matches('/').replace('/', "-"));
            self.data_dirs.push(format!("vol/{k}"));
            self.add_mount(
                &mut mts,
                &mut seen,
                Mount::src(&format!("/data/vol/{k}"), &vv, true),
                "image VOLUME",
                wh,
            );
            svc.comments.push(format!(
                "the image declares VOLUME {vv}: an anonymous volume on the data disk"
            ));
        }
        svc.mounts = mts;
        Ok(())
    }

    fn copy_bind(&mut self, src: &str) -> Option<String> {
        if !exists(src) {
            return None;
        }
        for sp in SPECIAL_HOST {
            if src == *sp || src.starts_with(&format!("{sp}/")) {
                return None;
            }
        }
        if let Some((_, g)) = self.bind_files.iter().find(|(s0, _)| s0 == src) {
            return Some(g.clone());
        }
        let k = self.bind_files.len();
        let b = hydo_manifest::py::basename(src.trim_end_matches('/'));
        let base = safe_name(if b.is_empty() { "root" } else { &b });
        let gp = format!("/etc/hydo/compose/bind/{k}-{base}");
        self.bind_files.push((src.to_string(), gp.clone()));
        Some(gp)
    }

    fn volume(
        &mut self,
        v: &Y,
        mts: &mut Vec<Mount>,
        seen: &mut Vec<String>,
        n: &str,
        wh: &str,
    ) -> R<()> {
        let mut ro = false;
        let (typ, src, tgt): (String, Y, Y);
        if v.is_map() {
            typ = pstr(v.get("type").unwrap_or(&Y::s("volume")));
            src = v.getn("source").clone();
            tgt = v.getn("target").clone();
            ro = v.getn("read_only").truthy();
            if typ == "tmpfs" {
                let sz = v.getn("tmpfs").getn("size").clone();
                let size = if sz.truthy() { Some(pstr(&sz)) } else { None };
                let Y::Str(t) = &tgt else {
                    return err(format!("{wh}: tmpfs volume {} has no target", repr(v)));
                };
                self.add_mount(mts, seen, Mount::tmpfs(t, size), "tmpfs", wh);
                return Ok(());
            }
            if typ != "volume" && typ != "bind" {
                self.w.warn(
                    "volumes",
                    format!("{wh}: volume type {typ} not supported; skipped"),
                );
                return Ok(());
            }
        } else {
            let vs = pstr(v);
            let parts: Vec<&str> = vs.split(':').collect();
            if parts.len() == 1 {
                typ = "volume".into();
                src = Y::Null;
                tgt = Y::s(parts[0]);
            } else {
                src = Y::s(parts[0]);
                tgt = Y::s(parts[1]);
                ro = parts.len() > 2 && parts[2].split(',').any(|x| x == "ro");
                typ = if parts[0].starts_with(['.', '/', '~']) {
                    "bind"
                } else {
                    "volume"
                }
                .into();
            }
        }
        let tgt_s = pstr(&tgt);
        if !tgt.truthy() || !tgt_s.starts_with('/') {
            self.w.warn(
                "volumes",
                format!("{wh}: volume {}: no absolute target; skipped", repr(v)),
            );
            return Ok(());
        }
        if typ == "volume" {
            let k = if src.truthy() {
                let Y::Str(ss) = &src else {
                    return err(format!(
                        "{wh}: volume source {} is not a string",
                        repr(&src)
                    ));
                };
                if !self.doc.getn("volumes").contains(&src) {
                    self.w.warn(
                        "volumes",
                        format!("{wh}: volume {ss} is not declared under the top-level volumes:"),
                    );
                }
                safe_name(ss)
            } else {
                format!("anon-{n}-{}", tgt_s.trim_matches('/').replace('/', "-"))
            };
            self.data_dirs.push(format!("vol/{k}"));
            if ro {
                self.notes.push(format!(
                    "{wh}: volume {} is mounted read-write in the guest (:ro is not enforced)",
                    pstr(&src)
                ));
            }
            let what = format!(
                "volume {}",
                if src.truthy() {
                    pstr(&src)
                } else {
                    "(anonymous)".into()
                }
            );
            self.add_mount(
                mts,
                seen,
                Mount::src(&format!("/data/vol/{k}"), &tgt_s, true),
                &what,
                wh,
            );
            return Ok(());
        }
        let Y::Str(srcs) = &src else {
            return err(format!("{wh}: bind mount {} has no source", repr(v)));
        };
        let mut p = expanduser(srcs);
        if !isabs(&p) {
            p = hydo_manifest::py::normpath(&join(&self.cdir, &p));
        }
        for sp in SPECIAL_HOST {
            if p == *sp || p.starts_with(&format!("{sp}/")) {
                let extra = if sp.contains("time") {
                    " (the guest's clock is UTC)"
                } else if sp.contains("docker") {
                    " (there is no Docker daemon inside)"
                } else {
                    ""
                };
                self.w.warn("volumes", format!("{wh}: bind mount of the host's {sp} skipped: the guest is a separate machine{extra}"));
                return Ok(());
            }
        }
        if !exists(&p) {
            let r = relpath(&p, &self.cdir);
            let r = r.trim_matches(['.', '/']);
            let k = format!("bind-{}", safe_name(if r.is_empty() { n } else { r }));
            self.data_dirs.push(format!("vol/{k}"));
            self.w.warn("volumes", format!("{wh}: bind mount source {srcs} does not exist (Docker would create an empty directory): it is an empty directory on the data disk, /data/vol/{k}"));
            self.add_mount(
                mts,
                seen,
                Mount::src(&format!("/data/vol/{k}"), &tgt_s, false),
                &format!("bind {srcs}"),
                wh,
            );
            return Ok(());
        }
        if Path::new(&p).is_dir() && !ro {
            self.w.warn("volumes", format!("{wh}: bind mount {srcs}:{tgt_s} is copied into the READ-ONLY root at build time; if the program writes there, make it a named volume (then it lives on the data disk)"));
        }
        if let Some(gp) = self.copy_bind(&p) {
            self.add_mount(
                mts,
                seen,
                Mount::src(&gp, &tgt_s, false),
                &format!("bind {srcs}"),
                wh,
            );
        }
        Ok(())
    }

    fn port_collisions(&mut self) {
        let mut by: Vec<(i128, Vec<String>)> = Vec::new();
        for s in &self.svcs {
            for p in &s.listen {
                match by.iter_mut().find(|(x, _)| x == p) {
                    Some(e) => e.1.push(s.name.clone()),
                    None => by.push((*p, vec![s.name.clone()])),
                }
            }
        }
        by.sort_by_key(|x| x.0);
        for (p, ss) in by {
            if ss.len() > 1 {
                let addrs: Vec<String> = ss
                    .iter()
                    .map(|x| {
                        let a = self
                            .svcs
                            .iter()
                            .find(|y| y.name == *x)
                            .map(|y| y.addr.clone())
                            .unwrap_or_default();
                        format!("{x}={a}")
                    })
                    .collect();
                self.w.warn("ports", format!("services {} all use container port {p}. In Compose each has its own network; in the guest they share one, so if they listen on 0.0.0.0:{p} only the first starts. Make each listen on its own addr: ({}) or move one to another port, e.g. with env:/cmd:", ss.join(", "), addrs.join(", ")));
            }
        }
    }

    // ------------------------------------------------------------ output

    fn emit(&mut self) -> String {
        let raw_name = match (&self.o.name, self.doc.get("name")) {
            (Some(n), _) if !n.is_empty() => n.clone(),
            (_, Some(n)) if n.truthy() => pstr(n),
            _ => {
                let b = hydo_manifest::py::basename(&self.cdir);
                if b.is_empty() {
                    "compose".into()
                } else {
                    b
                }
            }
        };
        let name: String = raw_name
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let mut name = name.trim_matches('-').to_string();
        if name.is_empty() || !name.starts_with(|c: char| c.is_alphabetic()) {
            name = format!("c-{name}");
        }
        let mut ram = 1024.max((self.ram + 255).div_euclid(256) * 256);
        if ram > 3072 {
            self.w.warn("ram", format!("the services need about {ram} MiB; hydo's x86_64 guest has at most 3072 MiB (guest.ram_mib was set to 3072)"));
            ram = 3072;
        }
        let data_mib = 512.max((self.data_mib + 255).div_euclid(256) * 256);
        // job commands go into the dependents' setup
        let jobs: Vec<Svc> = self
            .svcs
            .iter()
            .filter(|s| s.job.is_some())
            .cloned()
            .collect();
        for s in self.svcs.iter_mut() {
            let mut pre = Vec::new();
            for d in &s.depends_on {
                if let Some(j) = jobs.iter().find(|j| j.name == *d) {
                    let u = match &j.user {
                        Some(u) if !u.is_empty() => u.clone(),
                        _ => {
                            let u = j
                                .info
                                .getn("platforms")
                                .getn("amd64")
                                .getn("config")
                                .getn("User");
                            if u.truthy() {
                                pstr(u)
                            } else {
                                String::new()
                            }
                        }
                    };
                    let mut argv: Vec<String> =
                        if !u.is_empty() && !matches!(u.as_str(), "root" | "0" | "0:0") {
                            vec!["/.hydo/busybox".into(), "chpst".into(), "-u".into(), u]
                        } else {
                            Vec::new()
                        };
                    let jc = j.job.clone().unwrap_or_default();
                    if jc.is_empty() {
                        argv.push("TODO-job-command".into());
                    } else {
                        argv.extend(jc);
                    }
                    let q: Vec<String> = argv.iter().map(|x| shlex::quote(x)).collect();
                    pre.push(format!("[ -f /tmp/hydo/job.{d}.done ] || {{ hydo-oci {d} {} && : > /tmp/hydo/job.{d}.done; }}", q.join(" ")));
                }
            }
            if !pre.is_empty() {
                s.setup = Some(pre.join(" && "));
            }
        }
        let mut l = String::new();
        l.push_str(&comment(&format!(
            "Generated by hvx/onboard/hydo-compose from {}.\nA STARTING POINT: the services, images, readiness checks and volumes come from the Compose\nfile; the workload and the invariants are TODO (search for TODO). The hydo-onboard skill\n(`hydo guide`) fills them in. MANIFEST.md, \"From Docker Compose\", explains\nevery mapping.\n\n1. sh {}           # fetch the images by digest (needs network, once)\n2. write the workload and the invariants below\n3. HYDO_SEEDRNG=<pinned hvx_seedrng> hvx/onboard/hydo-build {}   (or: hydo build)",
            relpath(&self.cpath, &self.odir),
            "fetch-images.sh",
            hydo_manifest::py::basename(&self.out)
        ), 0));
        if !self.w.list.is_empty() {
            let body: Vec<String> = self.w.list.iter().map(|(_, t)| format!("- {t}")).collect();
            l.push_str("#\n");
            l.push_str(&comment(
                &format!(
                    "WARNINGS (also printed by hydo-compose):\n{}",
                    body.join("\n")
                ),
                0,
            ));
        }
        if !self.notes.is_empty() {
            let body: Vec<String> = self.notes.iter().map(|t| format!("- {t}")).collect();
            l.push_str("#\n");
            l.push_str(&comment(&format!("Notes:\n{}", body.join("\n")), 0));
        }
        // the project format (hydo-manifest model.rs): every document hydo writes names it
        l.push_str(&format!("format: {}\n", hydo_manifest::model::FORMAT));
        l.push_str(&format!("name: {name}\n"));
        l.push_str(&format!(
            "description: {}\n",
            q_str(&format!(
                "imported from {} ({} services); TODO: what is tested",
                hydo_manifest::py::basename(&self.cpath),
                self.svcs.len()
            ))
        ));
        if self.bridge {
            l.push_str("\nnetwork: bridge   # each service in its own network namespace at its addr: (MANIFEST.md \"Service networks\")\n");
        }
        l.push_str("\nguest:\n");
        l.push_str(&format!("  ram_mib: {ram}   # a guess: {} MiB for the services and the kernel; raise it if a service runs out\n", self.ram));
        if self.cpu.is_some() {
            l.push_str("  cpu: v3       # an image needs AVX/AVX2 (see the service's comment)\n");
        } else {
            l.push_str("  # cpu: v3     # uncomment if a service stops with \"invalid opcode\" (an image built for AVX2)\n");
        }
        l.push_str(&format!("\ndata:\n  size_mib: {data_mib}   # the data disk: every volume lives here (the fault target)\n"));
        let mut dd: Vec<String> = Vec::new();
        for d in &self.data_dirs {
            if !dd.contains(d) {
                dd.push(d.clone());
            }
        }
        if !dd.is_empty() {
            l.push_str(&format!("  dirs: {}\n", flow_list(&dd)));
        }
        l.push_str("\nroot:\n");
        if !self.bind_files.is_empty() {
            l.push_str(
                "  files:                 # bind-mount sources, copied into the read-only root\n",
            );
            for (src, gp) in &self.bind_files {
                let rs = relpath(src, &self.odir);
                l.push_str(&format!(
                    "    - {}\n",
                    flow_map(&[("src", Y::Str(rs)), ("dst", Y::s(gp))])
                ));
            }
        }
        let mut hosts =
            String::from("127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n");
        for (ip, al) in &self.hosts {
            hosts.push_str(&format!("{ip} {}\n", al.join(" ")));
        }
        l.push_str(&format!(
            "  text:\n    /etc/hosts: {}",
            block(&hosts, 6).replacen(
                "|\n",
                "|   # Compose service names -> each service's addr:\n",
                1
            )
        ));
        l.push_str("\nservices:\n");
        for s in &self.svcs {
            l.push_str(&comment(&format!("Compose service {}", s.compose), 2));
            for c in &s.comments {
                l.push_str(&comment(c, 2));
            }
            l.push_str(&format!("  - name: {}\n", s.name));
            match &s.image {
                Img::Two(a, b) => l.push_str(&format!(
                    "    image: {}\n",
                    flow_map(&[("x86_64", Y::s(a)), ("arm64", Y::s(b))])
                )),
                Img::One(a) => l.push_str(&format!("    image: {}\n", q_str(a))),
            }
            l.push_str(&format!("    image_ref: {}\n", q_str(&s.image_ref)));
            if let Some(e) = &s.entrypoint {
                l.push_str(&format!("    entrypoint: {}\n", flow_list(e)));
            }
            if let Some(c) = &s.cmd {
                l.push_str(&format!("    cmd: {}\n", flow_list(c)));
            }
            if !s.env.is_empty() {
                l.push_str("    env:\n");
                for (k, v) in &s.env {
                    l.push_str(&format!("      {k}: {}\n", dumps_str(v)));
                }
            }
            if let Some(w) = &s.workdir {
                l.push_str(&format!("    workdir: {}\n", dumps_str(w)));
            }
            if let Some(u) = &s.user {
                l.push_str(&format!("    user: {}\n", dumps_str(u)));
            }
            if let Some(p) = s.port {
                l.push_str(&format!("    port: {p}\n"));
            }
            if !s.noaddr {
                l.push_str(&format!("    addr: {}\n", s.addr));
            }
            if !s.depends_on.is_empty() {
                l.push_str(&format!("    depends_on: {}\n", flow_list(&s.depends_on)));
            }
            if let Some(st) = s.setup.as_ref().filter(|x| !x.is_empty()) {
                l.push_str(&format!("    setup: {}\n", dumps_str(st)));
            }
            l.push_str(&format!("    ready: {}\n", dumps_str(&s.ready)));
            if let Some(t) = s.ready_timeout.filter(|t| *t != 0) {
                l.push_str(&format!("    ready_timeout: {t}\n"));
            }
            if s.recovery_false {
                l.push_str("    recovery: false\n");
            }
            if let Some(r) = &s.restart {
                l.push_str(&format!("    restart: {}\n", dumps_str(r)));
            }
            l.push_str("    mounts:\n");
            for m in &s.mounts {
                let mut d: Vec<(&str, Y)> = Vec::new();
                if let Some(x) = &m.src {
                    d.push(("src", Y::s(x)));
                }
                if m.tmpfs {
                    d.push(("tmpfs", Y::Bool(true)));
                }
                d.push(("dst", Y::s(&m.dst)));
                if m.copy_up {
                    d.push(("copy_up", Y::Bool(true)));
                }
                if let Some(x) = &m.size {
                    d.push(("size", Y::s(x)));
                }
                l.push_str(&format!("      - {}\n", flow_map(&d)));
            }
        }
        let ex = self
            .svcs
            .iter()
            .find(|s| s.port.is_some_and(|p| p != 0))
            .unwrap_or(&self.svcs[0]);
        let ex_port = match ex.port {
            Some(p) => p.to_string(),
            None => "80".into(),
        };
        l.push('\n');
        l.push_str(&comment(&format!(
            "TODO(workload): one step = one client operation against the system (an HTTP request, a\nquery), appended to $HYDO_ACKED only when the system acknowledged it. Faults are placed\nat step numbers. Each Compose service name resolves (e.g. {} -> {}); run tools inside\nan image with `hydo-oci <service> <command>`. Ask the hydo-onboard skill to write it.",
            ex.compose, ex.addr
        ), 0));
        l.push_str("workload:\n  steps: 20\n  run: ");
        l.push_str(&block(&format!(
            "i=$HYDO_FROM\nwhile [ $i -lt $HYDO_TO ]; do\n  # TODO: one operation, e.g.\n  #   wget -q -T 5 -O /dev/null --post-data \"id=$i\" http://{}:{}/ && echo $i >> $HYDO_ACKED\n  echo \"hydo-compose: TODO write the workload (hydo.yaml)\" > /dev/console; exit 1\n  i=$((i + 1))\ndone",
            ex.compose, ex_port
        ), 4));
        l.push_str("  timeout: 300\n\n");
        l.push_str(&comment("TODO(invariants): shell commands, exit 0 = holds, 1 = violated, 2 = cannot check now.\nTypical: every id in $HYDO_ACKED can be read back after the run (and after a power cut);\ntype: eventually for \"processed within N s\". `hydo guide invariant-patterns`", 0));
        l.push_str("invariants:\n  - id: todo.acked_operations_survive\n    msg: TODO every acknowledged operation is still there at the end\n    cmd: ");
        l.push_str(&block(
            "echo \"TODO: write the invariants (hydo.yaml)\"; exit 2",
            6,
        ));
        l.push_str("\nfaults: [kill, pause, network, oneway, cut, clock, disk]\n");
        l
    }

    /// fetch-images.sh: copy the images by digest into the images directory with
    /// `hydo-fetch-image` (M107; before that, skopeo). Every copy is a `docker save` archive at the
    /// path the manifest names (the arm64 one keeps its `-arm64-oci` name, so the manifest and
    /// the image bytes stay the same; hydo-build reads either form by its content). An image that
    /// was not resolved to a digest (--no-resolve) is fetched by its tag, which can change.
    pub fn fetch_script(&self) -> String {
        let idir = self
            .o
            .images_dir
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "images".into());
        let mut l: Vec<String> = vec![
            "#!/bin/sh".into(),
            format!(
                "# Fetch the images of {} by digest into {}/ (generated by hydo compose; hydo-build",
                hydo_manifest::py::basename(&self.cpath),
                idir
            ),
            "# never pulls). Needs hydo-fetch-image (it comes with hydo) and network access, once.".into(),
            "set -eu".into(),
            "cd \"$(dirname \"$0\")\"".into(),
            "F=${HYDO_FETCH_IMAGE:-$(command -v hydo-fetch-image || true)}".into(),
            "[ -n \"$F\" ] || { echo \"fetch-images.sh: hydo-fetch-image not found (put hydo's bin directory on PATH, or set HYDO_FETCH_IMAGE)\" >&2; exit 1; }".into(),
            format!("mkdir -p {}", shlex::quote(&idir)),
        ];
        let mut done: Vec<String> = Vec::new();
        let mut local: Vec<String> = Vec::new();
        for s in &self.svcs {
            let info = &s.info;
            if let (Some(b), Img::Two(x86, arm)) = (&s.build, &s.image) {
                // built locally: nothing to fetch; say how to make the file, and stop at the end
                // when it is not there
                if local.contains(x86) {
                    continue;
                }
                local.push(x86.clone());
                let (x, a, t) = (
                    shlex::quote(x86),
                    shlex::quote(arm),
                    shlex::quote(&s.image_ref),
                );
                l.push(format!(
                    "# {} is built locally (build:): nothing is fetched. Make its file once with Docker:",
                    s.image_ref
                ));
                l.push(format!(
                    "#   docker build -t {t} {b} && docker save -o {x} {t}"
                ));
                l.push(format!("#   (arm64, for the Mac: docker buildx build --platform linux/arm64 -t {t} -o type=oci,tar=false,dest={a} {b})"));
                l.push(
                    "# then run hydo compose again: it pins the image by the file's content."
                        .into(),
                );
                l.push(format!(
                    "[ -f {x} ] || [ -e {a} ] || {{ echo {} >&2; LOCAL=1; }}",
                    shlex::quote(&format!(
                        "fetch-images.sh: {x86} is built locally (build:): docker build -t {} {b} && docker save -o {x86} {}",
                        s.image_ref, s.image_ref
                    ))
                ));
                continue;
            }
            let mut img: Vec<(&str, &String)> = match &s.image {
                Img::One(a) => vec![("x86_64", a)],
                Img::Two(a, b) => vec![("x86_64", a), ("arm64", b)],
            };
            img.sort();
            let plat = info.getn("platforms");
            for (arch, path) in img {
                if done.contains(path) {
                    continue;
                }
                done.push(path.clone());
                let a = if arch == "x86_64" { "amd64" } else { "arm64" };
                let pd = plat.get(a).map(|p| p.getn("digest"));
                let repo = format!(
                    "{}/{}",
                    pstr(info.getn("registry")),
                    pstr(info.getn("repo"))
                );
                let what = match pd.filter(|d| d.truthy()) {
                    Some(d) => pstr(d),
                    None => {
                        let t = info.getn("tag");
                        let t = if t.truthy() { pstr(t) } else { "latest".into() };
                        l.push(format!("# NOT pinned: docker://{repo}:{t}"));
                        format!("linux/{a}")
                    }
                };
                let test = if arch == "x86_64" { "-f" } else { "-e" };
                l.push(format!(
                    "[ {test} {} ] || \"$F\" {} {} {} {}",
                    shlex::quote(path),
                    shlex::quote(&repo),
                    shlex::quote(&what),
                    shlex::quote(&s.image_ref),
                    shlex::quote(path),
                ));
            }
        }
        done.sort();
        if !local.is_empty() {
            l.push("[ -z \"${LOCAL:-}\" ] || exit 1".into());
        }
        if done.is_empty() {
            return l.join("\n") + "\n";
        }
        l.push(format!(
            "ls -ld {}",
            done.iter()
                .map(|p| shlex::quote(p))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        l.join("\n") + "\n"
    }

    /// The lock file's text (`json.dump(..., indent=1, sort_keys=True)` and a newline).
    pub fn lock_text(&self) -> String {
        let v = Y::Map(vec![
            (Y::s("generated_by"), Y::s("hvx/onboard/hydo-compose")),
            (Y::s("images"), self.lock.clone()),
        ]);
        pyval::dumps_indent1_sorted(&v) + "\n"
    }
}

fn set_kv(out: &mut Vec<(String, String)>, k: String, v: String) {
    if let Some(e) = out.iter_mut().find(|(x, _)| *x == k) {
        e.1 = v;
    } else {
        out.push((k, v));
    }
}

// ---------------------------------------------------------------- the command

/// `hydo-compose <docker-compose.yaml> [options]` (and `hydo compose`): the exit status.
pub fn main(argv: &[String]) -> i32 {
    let mut reg = registry::Http::new();
    main_with(argv, &mut reg)
}

/// [`main`] with a given registry (the tests replay recorded registry answers).
pub fn main_with(a: &[String], reg: &mut dyn registry::Registry) -> i32 {
    if a.is_empty() || a[0] == "-h" || a[0] == "--help" {
        println!("{USAGE}");
        return if a.is_empty() { 2 } else { 0 };
    }
    let mut o = Opts::default();
    let mut out: Option<String> = None;
    let mut path: Option<String> = None;
    let mut i = 0;
    while i < a.len() {
        let x = a[i].as_str();
        if matches!(
            x,
            "--out"
                | "-o"
                | "--name"
                | "--images-dir"
                | "--lock"
                | "--env-file"
                | "--profile"
                | "--network"
        ) {
            if i + 1 >= a.len() {
                eprintln!("hydo-compose: {x} needs a value");
                return 2;
            }
            let v = a[i + 1].clone();
            match x {
                "--out" | "-o" => out = Some(v),
                "--env-file" => o.env_files.push(v),
                "--profile" => o.profiles.push(v),
                "--name" => o.name = Some(v),
                "--images-dir" => o.images_dir = Some(v),
                "--lock" => o.lock = Some(v),
                _ => o.network = Some(v),
            }
            i += 2;
        } else if x == "--use-env" {
            o.use_env = true;
            i += 1;
        } else if x == "--no-resolve" {
            o.no_resolve = true;
            i += 1;
        } else if x == "--no-addr" {
            o.no_addr = true;
            i += 1;
        } else if x.starts_with('-') {
            eprintln!("hydo-compose: unknown option {x}");
            return 2;
        } else if path.is_none() {
            path = Some(x.to_string());
            i += 1;
        } else {
            eprintln!("hydo-compose: one Compose file only");
            return 2;
        }
    }
    let Some(path) = path else {
        println!("{USAGE}");
        return 2;
    };
    if !matches!(
        o.network.as_deref().unwrap_or("bridge"),
        "bridge" | "loopback"
    ) {
        eprintln!("hydo-compose: --network bridge or loopback");
        return 2;
    }
    let out = match out.filter(|x| !x.is_empty()) {
        Some(x) => x,
        None => join(
            &hydo_manifest::py::dirname(&hydo_manifest::py::abspath(&path)),
            "hydo.yaml",
        ),
    };
    let mut imp = Importer::new(&path, &out, o, reg);
    let text = match imp.run() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("hydo-compose: {e}");
            return 1;
        }
    };
    // the output must be what hydo-build reads
    let check = hydo_manifest::yaml::parse_yaml(&text, "hydo.yaml").and_then(|mut m| {
        hydo_manifest::validate_data(&mut m)?;
        hydo_manifest::service_order(&m["services"])
    });
    if let Err(e) = check {
        eprintln!("hydo-compose: internal error, the generated manifest is not valid: {e}");
        return 3;
    }
    let oabs = hydo_manifest::py::abspath(&out);
    let odir = hydo_manifest::py::dirname(&oabs);
    if let Err(e) = std::fs::create_dir_all(&odir) {
        eprintln!("hydo-compose: {}", ComposeError::os(e, &odir));
        return 1;
    }
    if let Err(e) = std::fs::write(&out, &text) {
        eprintln!("hydo-compose: {}", ComposeError::os(e, &out));
        return 1;
    }
    let fs = join(&odir, "fetch-images.sh");
    if let Err(e) = std::fs::write(&fs, imp.fetch_script()) {
        eprintln!("hydo-compose: {}", ComposeError::os(e, &fs));
        return 1;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&fs, std::fs::Permissions::from_mode(0o755));
    }
    if imp.lock_dirty {
        if let Err(e) = std::fs::write(imp.lock_path(), imp.lock_text()) {
            eprintln!("hydo-compose: {}", ComposeError::os(e, imp.lock_path()));
            return 1;
        }
    }
    for (_, t) in &imp.w.list {
        eprintln!("hydo-compose: warning: {t}");
    }
    eprintln!(
        "hydo-compose: wrote {out} ({} services, {} warnings), {fs}{}",
        imp.services(),
        imp.w.list.len(),
        if imp.lock_dirty {
            format!(", {}", imp.lock_path())
        } else {
            String::new()
        }
    );
    eprintln!(
        "hydo-compose: next: sh {fs}; then write the workload and the invariants (TODO in {out})"
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        assert_eq!(
            parse_ref("redis:7.2.4"),
            (
                "docker.io".into(),
                "library/redis".into(),
                Some("7.2.4".into()),
                None
            )
        );
        assert_eq!(
            parse_ref("localhost:5000/x/y"),
            (
                "localhost:5000".into(),
                "x/y".into(),
                Some("latest".into()),
                None
            )
        );
        assert_eq!(short_name("ghcr.io", "a/b"), "ghcr.io/a/b");
        assert_eq!(
            file_stem("docker.io", "library/redis", Some("7.2.4"), None).unwrap(),
            "redis-7.2.4"
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(q_str("abc"), "abc");
        assert_eq!(q_str("yes"), "\"yes\"");
        assert_eq!(q_str("123"), "\"123\"");
        assert_eq!(q_str("a b"), "\"a b\"");
        assert_eq!(duration_s(Some(&Y::s("1m30s")), 0).unwrap(), 90);
        assert_eq!(duration_s(Some(&Y::s("500ms")), 0).unwrap(), 1);
        assert_eq!(duration_s(Some(&Y::s("2.5s")), 0).unwrap(), 2);
        assert_eq!(mem_mib(Some(&Y::s("1g"))).unwrap(), Some(1024));
        assert_eq!(mem_mib(Some(&Y::s("512MiB"))).unwrap(), Some(512));
    }

    #[test]
    fn named_volumes_on_data() {
        let m = |t: &str, s: &str, d: &str| {
            Y::Map(vec![
                (Y::s("type"), Y::s(t)),
                (Y::s("source"), Y::s(s)),
                (Y::s("target"), Y::s(d)),
            ])
        };
        assert_eq!(
            named_volume_on_data(&Y::s("n1-data:/data")),
            Some("n1-data".into())
        );
        assert_eq!(
            named_volume_on_data(&Y::s("n1 data:/data/:rw")),
            Some("n1_data".into())
        );
        assert_eq!(
            named_volume_on_data(&m("volume", "v", "/data")),
            Some("v".into())
        );
        assert_eq!(named_volume_on_data(&Y::s("/data")), None); // anonymous
        assert_eq!(named_volume_on_data(&Y::s("./d:/data")), None); // a bind mount
        assert_eq!(named_volume_on_data(&Y::s("v:/data/x")), None);
        assert_eq!(named_volume_on_data(&m("bind", "./d", "/data")), None);
        assert_eq!(named_volume_on_data(&m("volume", "", "/data")), None);
    }
}
