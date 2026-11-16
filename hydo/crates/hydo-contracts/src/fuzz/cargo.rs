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
    if let Ok(root) = std::fs::read_to_string(pkg.workspace_root.join("Cargo.toml")) {
        t.push_str(&sections(&root, &pkg.workspace_root));
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

/// Build the copy's test program and return its path.
pub fn build(
    cargo: &Cargo,
    copy: &Path,
    target_dir: &Path,
    target: &Target,
    log: &Path,
) -> Result<PathBuf, String> {
    let mut c = cargo.cmd();
    c.args([
        "test",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ]);
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
        .map_err(|e| format!("cargo test --no-run: {e}"))?;
    let _ = std::fs::write(log, &out.stderr);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<&str> = err.lines().collect();
        let first = lines
            .iter()
            .position(|l| l.starts_with("error"))
            .unwrap_or(lines.len().saturating_sub(30));
        let shown: Vec<&str> = lines[first..].iter().take(40).copied().collect();
        return Err(format!(
            "the harness did not build (all of cargo's output: {}):\n{}",
            log.display(),
            shown.join("\n")
        ));
    }
    let want = target.name.replace('-', "_");
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["reason"] == "compiler-artifact"
            && v["profile"]["test"] == true
            && v["target"]["name"].as_str().map(|n| n.replace('-', "_")) == Some(want.clone())
        {
            if let Some(e) = v["executable"].as_str() {
                return Ok(PathBuf::from(e));
            }
        }
    }
    Err("cargo built no test program for the target".into())
}

/// How one run of the test program ended.
pub struct Ran {
    /// The answer lines, in order.
    pub lines: Vec<String>,
    /// The program went silent for longer than the timeout and was stopped.
    pub hung: bool,
}

/// Run the test program on a cases file. `filter` names the harness's test.
pub fn run(
    exe: &Path,
    filter: &str,
    cases: &Path,
    answers: &Path,
    timeout: Duration,
    env: &[(String, String)],
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
    let mut hung = false;
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
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
