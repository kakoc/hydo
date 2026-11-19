//! The crate's copy, built and run: hydo never changes the project's own files. It copies the
//! package, writes a manifest for the copy from `cargo metadata` (so a workspace member's
//! inherited keys and relative paths resolve), keeps the workspace's lock file, profiles and
//! patches, adds the harness modules to the copied sources, and builds the test program with
//! `cargo test --no-run`.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How cargo is run.
#[derive(Clone, Debug, Default)]
pub struct Cargo {
    /// `cargo +<toolchain>`.
    pub toolchain: Option<String>,
    /// Build the harness with `--release` (no overflow checks, no debug assertions).
    pub release: bool,
}

impl Cargo {
    fn cmd(&self) -> Command {
        if let Some(t) = &self.toolchain {
            // rustup's proxy reads the toolchain from the first argument
            let mut c = Command::new(find_cargo());
            c.arg(format!("+{t}"));
            return c;
        }
        match std::env::var_os("CARGO") {
            Some(c) => Command::new(c),
            None => Command::new(find_cargo()),
        }
    }
}

/// `cargo` on PATH. A shell expands a `~` in PATH and the system's program lookup does not, so
/// such entries are expanded here.
fn find_cargo() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for d in std::env::var("PATH").unwrap_or_default().split(':') {
        let d = match (d.strip_prefix("~/"), &home) {
            (Some(r), Some(h)) => h.join(r),
            _ => PathBuf::from(d),
        };
        if d.join("cargo").is_file() {
            return d.join("cargo");
        }
    }
    PathBuf::from("cargo")
}

/// `rustfmt` beside the cargo that builds the harness, else on PATH (Milestone 326).
pub(crate) fn rustfmt() -> PathBuf {
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(find_cargo);
    match cargo.parent().map(|d| d.join("rustfmt")) {
        Some(r) if r.is_file() => r,
        _ => PathBuf::from("rustfmt"),
    }
}

/// A build target of the package.
#[derive(Clone, Debug)]
pub struct Target {
    pub kind: String,
    pub name: String,
    pub src_path: PathBuf,
    pub required_features: Vec<String>,
}

/// The package as `cargo metadata` describes it.
#[derive(Clone, Debug)]
pub struct Pkg {
    pub name: String,
    pub dir: PathBuf,
    pub workspace_root: PathBuf,
    pub targets: Vec<Target>,
    meta: Value,
}

/// Read the package whose manifest is `dir/Cargo.toml`.
pub fn metadata(cargo: &Cargo, dir: &Path) -> Result<Pkg, String> {
    let dir = &dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let manifest = dir.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(format!("{}: no Cargo.toml", dir.display()));
    }
    let out = cargo
        .cmd()
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(&manifest)
        .current_dir(dir)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let canon = manifest.canonicalize().map_err(|e| e.to_string())?;
    let p = v["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| {
            p["manifest_path"]
                .as_str()
                .and_then(|m| Path::new(m).canonicalize().ok())
                .as_deref()
                == Some(canon.as_path())
        })
        .ok_or("cargo metadata names no package for this Cargo.toml (a virtual workspace? name a member's folder)")?
        .clone();
    let targets = p["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| Target {
            kind: t["kind"][0].as_str().unwrap_or("").to_string(),
            name: t["name"].as_str().unwrap_or("").to_string(),
            src_path: PathBuf::from(t["src_path"].as_str().unwrap_or("")),
            required_features: t["required-features"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        })
        .collect();
    Ok(Pkg {
        name: p["name"].as_str().unwrap_or("").to_string(),
        dir: canon.parent().unwrap_or(dir).to_path_buf(),
        workspace_root: PathBuf::from(v["workspace_root"].as_str().unwrap_or("")),
        targets,
        meta: p,
    })
}

impl Pkg {
    /// The target whose sources hold `file` (a path under the package): a binary whose root it
    /// is, else the library, else the binary whose folder holds it.
    pub fn target_of(&self, file: &Path) -> Option<&Target> {
        let roots: Vec<&Target> = self
            .targets
            .iter()
            .filter(|t| t.kind == "lib" || t.kind == "bin")
            .collect();
        if let Some(t) = roots.iter().find(|t| t.kind == "bin" && t.src_path == file) {
            return Some(t);
        }
        let under = |t: &Target| t.src_path.parent().is_some_and(|d| file.starts_with(d));
        roots
            .iter()
            .find(|t| t.kind == "lib" && under(t))
            .or_else(|| roots.iter().find(|t| t.kind == "bin" && under(t)))
            .copied()
    }

    /// The source files of the package (`.rs` files outside `target/`).
    pub fn sources(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(&self.dir, &mut |p| {
            if p.extension().is_some_and(|e| e == "rs") {
                out.push(p.to_path_buf());
            }
        });
        out.sort();
        out
    }
}

fn skip_dir(name: &str) -> bool {
    matches!(name, "target" | ".git" | "node_modules" | "hydo-out")
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut es: Vec<_> = rd.flatten().collect();
    es.sort_by_key(|e| e.file_name());
    for e in es {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                if !skip_dir(&name) && !p.join("Cargo.toml").is_file() {
                    walk(&p, f);
                }
            }
            Ok(t) if t.is_file() => f(&p),
            _ => {}
        }
    }
}

/// Copy the package's files (not `target/`, not nested packages) to `to`.
pub fn copy_package(pkg: &Pkg, to: &Path) -> Result<(), String> {
    let mut err = None;
    walk(&pkg.dir, &mut |p| {
        let rel = p.strip_prefix(&pkg.dir).unwrap_or(p);
        let dst = to.join(rel);
        if let Some(d) = dst.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Err(e) = std::fs::copy(p, &dst) {
            err.get_or_insert(format!("{}: {e}", p.display()));
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    for f in ["rust-toolchain.toml", "rust-toolchain"] {
        for d in [&pkg.dir, &pkg.workspace_root] {
            if d.join(f).is_file() {
                let _ = std::fs::copy(d.join(f), to.join(f));
            }
        }
    }
    let lock = pkg.workspace_root.join("Cargo.lock");
    if lock.is_file() {
        std::fs::copy(&lock, to.join("Cargo.lock")).map_err(|e| e.to_string())?;
    }
    for c in ["config.toml", "config"] {
        let src = pkg.workspace_root.join(".cargo").join(c);
        if src.is_file() {
            let _ = std::fs::create_dir_all(to.join(".cargo"));
            let _ = std::fs::copy(&src, to.join(".cargo").join(c));
        }
    }
    Ok(())
}

/// A TOML string.
fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// The manifest of the copy: the package's own keys from `cargo metadata`, the one target that
/// holds the fuzzed functions (and the library), and the workspace's profiles and patches.
pub fn manifest(pkg: &Pkg, target: &Target) -> String {
    let m = &pkg.meta;
    let mut t = String::new();
    t.push_str("# written by `hydo fuzz` for a copy of the package; the project's own files are unchanged\n");
    t.push_str("[package]\n");
    t.push_str(&format!("name = {}\n", q(&pkg.name)));
    t.push_str(&format!(
        "version = {}\n",
        q(m["version"].as_str().unwrap_or("0.0.0"))
    ));
    t.push_str(&format!(
        "edition = {}\n",
        q(m["edition"].as_str().unwrap_or("2021"))
    ));
    t.push_str("publish = false\nautobins = false\nautoexamples = false\nautotests = false\nautobenches = false\n");
    if let Some(l) = m["links"].as_str() {
        t.push_str(&format!("links = {}\n", q(l)));
    }
    let rel = |p: &Path| {
        p.strip_prefix(&pkg.dir)
            .unwrap_or(p)
            .to_string_lossy()
            .to_string()
    };
    match pkg.targets.iter().find(|t| t.kind == "custom-build") {
        Some(b) => t.push_str(&format!("build = {}\n", q(&rel(&b.src_path)))),
        None => t.push_str("build = false\n"),
    }
    t.push_str("\n[workspace]\n\n");
    if let Some(lib) = pkg
        .targets
        .iter()
        .find(|t| t.kind == "lib" || t.kind == "proc-macro")
    {
        t.push_str(&format!(
            "[lib]\nname = {}\npath = {}\n",
            q(&lib.name.replace('-', "_")),
            q(&rel(&lib.src_path))
        ));
        if lib.kind == "proc-macro" {
            t.push_str("proc-macro = true\n");
        }
        t.push('\n');
    }
    // every other target as the package declares it (a build script may name them, as with
    // `cargo:rustc-link-arg-benches`); `cargo test --lib` or `--bin` builds only the one
    for x in &pkg.targets {
        let table = match x.kind.as_str() {
            "bin" => "bin",
            "bench" => "bench",
            "test" => "test",
            "example" => "example",
            _ => continue,
        };
        t.push_str(&format!(
            "[[{table}]]\nname = {}\npath = {}\n",
            q(&x.name),
            q(&rel(&x.src_path))
        ));
        if !x.required_features.is_empty() {
            let f: Vec<String> = x.required_features.iter().map(|s| q(s)).collect();
            t.push_str(&format!("required-features = [{}]\n", f.join(", ")));
        }
        t.push('\n');
    }
    let _ = target;
    if let Some(f) = m["features"].as_object() {
        if !f.is_empty() {
            t.push_str("[features]\n");
            for (k, v) in f {
                let l: Vec<String> = v
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(q)
                    .collect();
                t.push_str(&format!("{} = [{}]\n", q(k), l.join(", ")));
            }
            t.push('\n');
        }
    }
    // the dependencies, by table
    let mut tables: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for d in m["dependencies"].as_array().into_iter().flatten() {
        let name = d["name"].as_str().unwrap_or("");
        let key = d["rename"].as_str().unwrap_or(name).to_string();
        let kind = match d["kind"].as_str() {
            Some("dev") => "dev-dependencies",
            Some("build") => "build-dependencies",
            _ => "dependencies",
        };
        let table = match d["target"].as_str() {
            Some(tg) => format!("target.{}.{kind}", q(tg)),
            None => kind.to_string(),
        };
        let mut e: Vec<String> = Vec::new();
        if d["rename"].is_string() {
            e.push(format!("package = {}", q(name)));
        }
        let req = d["req"].as_str().unwrap_or("*");
        let source = d["source"].as_str().unwrap_or("");
        if let Some(p) = d["path"].as_str() {
            let p = Path::new(p);
            let p = if p.canonicalize().ok().as_deref() == Some(pkg.dir.as_path()) {
                ".".to_string()
            } else {
                p.to_string_lossy().to_string()
            };
            e.push(format!("path = {}", q(&p)));
            if req != "*" {
                e.push(format!("version = {}", q(req)));
            }
        } else if let Some(g) = source.strip_prefix("git+") {
            let g = g.split('#').next().unwrap_or(g);
            let (url, query) = g.split_once('?').unwrap_or((g, ""));
            e.push(format!("git = {}", q(url)));
            for kv in query.split('&').filter(|s| !s.is_empty()) {
                if let Some((k, v)) = kv.split_once('=') {
                    if matches!(k, "branch" | "tag" | "rev") {
                        e.push(format!("{k} = {}", q(v)));
                    }
                }
            }
            if req != "*" {
                e.push(format!("version = {}", q(req)));
            }
        } else {
            e.push(format!("version = {}", q(req)));
            if let Some(r) = d["registry"].as_str() {
                if !r.contains("crates.io-index") {
                    e.push(format!("registry-index = {}", q(r)));
                }
            }
        }
        if d["uses_default_features"].as_bool() == Some(false) {
            e.push("default-features = false".into());
        }
        let feats: Vec<String> = d["features"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(q)
            .collect();
        if !feats.is_empty() {
            e.push(format!("features = [{}]", feats.join(", ")));
        }
        if d["optional"].as_bool() == Some(true) {
            e.push("optional = true".into());
        }
        let rows = tables.entry(table).or_default();
        if !rows.iter().any(|(k, _)| *k == key) {
            rows.push((key, format!("{{ {} }}", e.join(", "))));
        }
    }
    for (table, rows) in &tables {
        t.push_str(&format!("[{table}]\n"));
        for (k, v) in rows {
            t.push_str(&format!("{} = {v}\n", q(k)));
        }
        t.push('\n');
    }
    // the workspace's profiles and patches, as written there
    let root = std::fs::read_to_string(pkg.workspace_root.join("Cargo.toml")).ok();
    if let Some(root) = &root {
        t.push_str(&sections(root, &pkg.workspace_root));
    }
    // the package's lints (Milestone 289): `cargo metadata` does not give them, and a crate
    // that denies warnings in its tests needs its `check-cfg` list
    if let Ok(own) = std::fs::read_to_string(pkg.dir.join("Cargo.toml")) {
        t.push_str(&lints(&own, root.as_deref()));
    }
    t
}

/// Whether a manifest's line is a table header (`[lints.rust]`, `[[bin]]`), not a line of a
/// value such as a multi-line array.
fn is_header(line: &str) -> bool {
    let l = line.split('#').next().unwrap_or("").trim();
    l.starts_with('[')
        && l.ends_with(']')
        && l.trim_matches(|c| c == '[' || c == ']')
            .chars()
            .all(|c| c.is_alphanumeric() || "_-.\"' ".contains(c))
}

/// The tables of a manifest's text whose header starts with one of `prefixes`, as written.
fn tables(text: &str, prefixes: &[&str]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut keep = false;
    for line in text.lines() {
        if is_header(line) {
            let h = line.split('#').next().unwrap_or("").trim().to_string();
            let name = h.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            keep = prefixes
                .iter()
                .any(|p| name == *p || name.starts_with(&format!("{p}.")));
            if keep {
                out.push((name, Vec::new()));
            }
            continue;
        }
        if keep {
            if let Some(last) = out.last_mut() {
                last.1.push(line.to_string());
            }
        }
    }
    out
}

/// The package's `[lints]` tables for the copy's manifest: its own, or with `workspace = true`
/// the workspace's `[workspace.lints]` tables.
pub fn lints(own: &str, workspace: Option<&str>) -> String {
    let mine = tables(own, &["lints"]);
    let inherits = mine.iter().any(|(n, body)| {
        n == "lints"
            && body
                .iter()
                .any(|l| l.replace(' ', "").starts_with("workspace=true"))
    });
    let mut t = String::new();
    if inherits {
        for (n, body) in tables(workspace.unwrap_or(""), &["workspace.lints"]) {
            t.push_str(&format!("\n[{}]\n", n.trim_start_matches("workspace.")));
            for l in body {
                t.push_str(&l);
                t.push('\n');
            }
        }
        return t;
    }
    for (n, body) in mine {
        t.push_str(&format!("\n[{n}]\n"));
        for l in body {
            t.push_str(&l);
            t.push('\n');
        }
    }
    t
}

/// The `[profile.*]` and `[patch.*]` sections of a manifest's text, with relative `path`s made
/// absolute.
fn sections(text: &str, root: &Path) -> String {
    let mut out = String::new();
    let mut keep = false;
    for line in text.lines() {
        let l = line.trim_start();
        if l.starts_with('[') {
            keep = l.starts_with("[profile") || l.starts_with("[patch");
            if keep {
                out.push('\n');
            }
        }
        if keep {
            out.push_str(&abs_paths(line, root));
            out.push('\n');
        }
    }
    out
}

fn abs_paths(line: &str, root: &Path) -> String {
    let Some(i) = line.find("path = \"") else {
        return line.to_string();
    };
    let start = i + "path = \"".len();
    let Some(len) = line[start..].find('"') else {
        return line.to_string();
    };
    let p = &line[start..start + len];
    if Path::new(p).is_absolute() {
        return line.to_string();
    }
    format!(
        "{}{}{}",
        &line[..start],
        root.join(p).to_string_lossy(),
        &line[start + len..]
    )
}

/// One error of the compiler: its first line (`error[E0616]: field ... is private`) and the
/// places it names (the file as cargo gives it, the line, and the column in characters).
#[derive(Clone, Debug, Default)]
pub struct Diag {
    pub message: String,
    pub spans: Vec<(String, u32, u32)>,
    /// The primary place (Milestone 326).
    pub primary: Option<Span>,
    /// The labels of the error's own places (`expected `u16`, found `&u8``).
    pub labels: Vec<(Span, String)>,
    /// The compiler's `help:` and `note:` lines, each with its suggested change.
    pub notes: Vec<String>,
    /// The error as the compiler printed it.
    pub rendered: String,
}

/// A place in a file as the compiler names it (Milestone 326).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub file: String,
    pub line: u32,
    /// The first column and the column after the last, in characters.
    pub cols: (u32, u32),
}

impl Span {
    fn of(s: &Value) -> Option<Span> {
        Some(Span {
            file: s["file_name"].as_str()?.to_string(),
            line: s["line_start"].as_u64()? as u32,
            cols: (
                s["column_start"].as_u64()? as u32,
                s["column_end"].as_u64()? as u32,
            ),
        })
    }
}

/// The places, labels and notes of one compiler message (Milestone 326).
fn diag_of(m: &Value, rendered: &str) -> Diag {
    let code = m["code"]["code"]
        .as_str()
        .map(|c| format!("[{c}]"))
        .unwrap_or_default();
    let mut spans = Vec::new();
    spans_of(m, &mut spans);
    let own = m["spans"].as_array().cloned().unwrap_or_default();
    let primary = own
        .iter()
        .find(|s| s["is_primary"] == true)
        .or(own.first())
        .and_then(Span::of);
    let labels = own
        .iter()
        .filter_map(|s| Some((Span::of(s)?, s["label"].as_str()?.to_string())))
        .collect();
    let mut notes = Vec::new();
    for c in m["children"].as_array().into_iter().flatten() {
        let level = c["level"].as_str().unwrap_or("note");
        let text = c["message"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let fixes: Vec<String> = c["spans"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["suggested_replacement"].as_str())
            .filter(|r| !r.trim().is_empty() && r.len() <= 160)
            .map(|r| format!("`{}`", r.trim()))
            .collect();
        if fixes.is_empty() {
            notes.push(format!("{level}: {text}"));
        } else {
            notes.push(format!("{level}: {text}: {}", fixes.join(", ")));
        }
    }
    Diag {
        message: format!("error{code}: {}", m["message"].as_str().unwrap_or("")),
        spans,
        primary,
        labels,
        notes,
        rendered: rendered.to_string(),
    }
}

/// A compiler's rendered text with each very long source line cut to the part around its marks
/// (a harness module is one line), so that a log stays readable (Milestone 326).
pub fn narrow(rendered: &str) -> String {
    let lines: Vec<&str> = rendered.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.chars().count() <= 300 {
            out.push(l.to_string());
            i += 1;
            continue;
        }
        // the gutter (`12 | `) and the marks of the next line (`   |      ^^^`)
        let gutter = l.find('|').map_or(0, |p| p + 1);
        let marks = lines.get(i + 1).filter(|n| {
            gutter > 0
                && n.find('|') == Some(gutter - 1)
                && n[gutter..].trim_start().starts_with(['^', '-'])
        });
        let at = marks.map_or(0, |n| n[gutter..].chars().take_while(|c| *c == ' ').count());
        let from = at.saturating_sub(80);
        let cut = |x: &str| -> String {
            let head: String = x.chars().take(gutter).collect();
            let body: String = x.chars().skip(gutter + from).take(240).collect();
            let more = if x.chars().count() > gutter + from + 240 {
                " ..."
            } else {
                ""
            };
            let less = if from > 0 { "... " } else { "" };
            format!("{head}{less}{body}{more}")
        };
        out.push(cut(l));
        if let Some(n) = marks {
            out.push(cut(n));
            i += 1;
        }
        i += 1;
    }
    let mut s = out.join("\n");
    if rendered.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// A build that failed: the compiler's errors, and the text to show.
#[derive(Clone, Debug)]
pub struct BuildFail {
    pub errors: Vec<Diag>,
    pub text: String,
}

impl BuildFail {
    /// The first error's line, or the first line of the text.
    pub fn first_error(&self) -> String {
        match self.errors.first() {
            Some(d) => d.message.clone(),
            None => self.text.lines().next().unwrap_or("").to_string(),
        }
    }
}

/// Every place a diagnostic names: its spans, the macro expansions they come from, and its
/// children's spans.
fn spans_of(m: &Value, out: &mut Vec<(String, u32, u32)>) {
    fn span(s: &Value, out: &mut Vec<(String, u32, u32)>) {
        if let (Some(f), Some(l), Some(c)) = (
            s["file_name"].as_str(),
            s["line_start"].as_u64(),
            s["column_start"].as_u64(),
        ) {
            out.push((f.to_string(), l as u32, c as u32));
        }
        if s["expansion"].is_object() {
            span(&s["expansion"]["span"], out);
        }
    }
    for s in m["spans"].as_array().into_iter().flatten() {
        span(s, out);
    }
    for c in m["children"].as_array().into_iter().flatten() {
        spans_of(c, out);
    }
}

/// The builds of this process, for the log's headings.
static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Build the copy's test program and return its path. The compiler's output is added to `log`.
pub fn build(
    cargo: &Cargo,
    copy: &Path,
    target_dir: &Path,
    target: &Target,
    log: &Path,
) -> Result<PathBuf, BuildFail> {
    let fail = |text: String| BuildFail {
        errors: vec![],
        text,
    };
    let mut c = cargo.cmd();
    c.args(["test", "--no-run", "--message-format=json"]);
    if cargo.release {
        c.arg("--release");
    }
    if target.kind == "bin" {
        c.args(["--bin", &target.name]);
    } else {
        c.arg("--lib");
    }
    c.current_dir(copy)
        .env("CARGO_TARGET_DIR", target_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c
        .output()
        .map_err(|e| fail(format!("cargo test --no-run: {e}")))?;
    // the log: the compiler's rendered messages, then cargo's own output
    let mut rendered = String::new();
    let mut errors = Vec::new();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let msgs: Vec<Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    for v in &msgs {
        if v["reason"] != "compiler-message" {
            continue;
        }
        let m = &v["message"];
        let r = m["rendered"].as_str().unwrap_or("");
        rendered.push_str(r);
        if m["level"] == "error" {
            errors.push(diag_of(m, r));
        }
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // every build of the run stays in the log, after a line that numbers it, so that a later
    // build does not hide a failed build's output (Milestone 326)
    let n = BUILDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        use std::io::Write as _;
        let _ = writeln!(
            f,
            "==== hydo fuzz: build {n} of the copy: {}",
            if out.status.success() {
                "built"
            } else {
                "failed"
            }
        );
        let _ = f.write_all(narrow(&format!("{rendered}{stderr}")).as_bytes());
    }
    if !out.status.success() {
        let all = format!("{rendered}{stderr}");
        let lines: Vec<&str> = all.lines().collect();
        let first = lines
            .iter()
            .position(|l| l.starts_with("error"))
            .unwrap_or(lines.len().saturating_sub(30));
        // a generated line is long: show its start
        let shown: Vec<String> = lines[first..]
            .iter()
            .take(40)
            .map(|l| match l.char_indices().nth(240) {
                Some((i, _)) => format!("{}...", &l[..i]),
                None => l.to_string(),
            })
            .collect();
        return Err(BuildFail {
            errors,
            text: format!(
                "the harness did not build (all of cargo's output: {}):\n{}",
                log.display(),
                shown.join("\n")
            ),
        });
    }
    let want = target.name.replace('-', "_");
    for v in msgs {
        if v["reason"] == "compiler-artifact"
            && v["profile"]["test"] == true
            && v["target"]["name"].as_str().map(|n| n.replace('-', "_")) == Some(want.clone())
        {
            if let Some(e) = v["executable"].as_str() {
                return Ok(PathBuf::from(e));
            }
        }
    }
    Err(fail("cargo built no test program for the target".into()))
}

/// Whether the package itself builds its test program, with its own manifest (Milestone 289):
/// `Some(true)` when it does, so that a failure of hydo's copy is hydo's. A package of its own
/// is built in a plain copy at `scratch`, whose sources `prepare` writes; a workspace member in
/// place with `--locked`, so that
/// the project's files stay as they are. `None` when it cannot be told (a member without a
/// lock file).
pub fn original_builds(
    cargo: &Cargo,
    pkg: &Pkg,
    target: &Target,
    target_dir: &Path,
    scratch: &Path,
    prepare: &dyn Fn(&Path) -> Result<(), String>,
) -> Option<bool> {
    let standalone = pkg.dir == pkg.workspace_root;
    let dir = if standalone {
        if scratch.exists() {
            std::fs::remove_dir_all(scratch).ok()?;
        }
        std::fs::create_dir_all(scratch).ok()?;
        copy_package(pkg, scratch).ok()?;
        prepare(scratch).ok()?;
        scratch.to_path_buf()
    } else {
        if !pkg.workspace_root.join("Cargo.lock").is_file() {
            return None;
        }
        pkg.dir.clone()
    };
    let mut c = cargo.cmd();
    c.args(["test", "--no-run", "--quiet"]);
    if !standalone {
        c.arg("--locked");
    }
    if cargo.release {
        c.arg("--release");
    }
    if target.kind == "bin" {
        c.args(["--bin", &target.name]);
    } else {
        c.arg("--lib");
    }
    let out = c
        .arg("--manifest-path")
        .arg(dir.join("Cargo.toml"))
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(out.success())
}

/// How one run of the test program ended.
pub struct Ran {
    /// The answer lines, in order.
    pub lines: Vec<String>,
    /// The program went silent for longer than the timeout and was stopped.
    pub hung: bool,
}

/// Run the test program on a cases file. `filter` names the harness's test. `tick` hears the
/// number of answers so far about once a second.
pub fn run(
    exe: &Path,
    filter: &str,
    cases: &Path,
    answers: &Path,
    timeout: Duration,
    env: &[(String, String)],
    tick: &mut dyn FnMut(u64),
) -> Result<Ran, String> {
    let _ = std::fs::remove_file(answers);
    std::fs::write(answers, "").map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args([filter, "--test-threads=1", "-q"])
        .env("HYDO_FUZZ_CASES", cases)
        .env("HYDO_FUZZ_OUT", answers)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let mut last_len = 0u64;
    let mut last_change = Instant::now();
    let mut last_tick = Instant::now();
    let mut hung = false;
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if last_tick.elapsed() >= Duration::from_secs(1) {
            last_tick = Instant::now();
            let n = std::fs::read(answers)
                .map(|b| b.iter().filter(|&&c| c == b'\n').count() as u64)
                .unwrap_or(0);
            tick(n);
        }
        let len = std::fs::metadata(answers).map(|m| m.len()).unwrap_or(0);
        if len != last_len {
            last_len = len;
            last_change = Instant::now();
        } else if last_change.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            hung = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let text = std::fs::read_to_string(answers).unwrap_or_default();
    // a line without its newline is a partial write of a process that ended
    let lines = text
        .split_inclusive('\n')
        .filter(|l| l.ends_with('\n'))
        .map(|l| l.trim_end().to_string())
        .collect();
    Ok(Ran { lines, hung })
}
