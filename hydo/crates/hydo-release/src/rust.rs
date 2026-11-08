//! The Rust crates compiled into the shipped programs: `cargo metadata` of each [`RustProgram`]
//! (for its target, so only that platform's dependencies), every normal and build dependency
//! reachable from its packages (build dependencies and proc macros run only while compiling, but
//! they are listed too), with the license texts their registry sources carry.

use crate::model::{List, RustOverride, RustProgram};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One third-party crate (a registry or git package; path packages are hydo's own).
#[derive(Clone, Debug)]
pub struct Crate {
    pub name: String,
    pub version: String,
    /// The `license` field, or the override's.
    pub license: Option<String>,
    pub license_file: Option<String>,
    /// The crate's unpacked source (in the Cargo home's registry).
    pub dir: PathBuf,
    /// Where it comes from: the registry URL, or a git URL.
    pub source: String,
    /// Cargo.lock's checksum of the `.crate` archive (registry crates).
    pub checksum: Option<String>,
    /// The release paths of the programs it is compiled into.
    pub used_by: BTreeSet<String>,
    /// Paths (relative to `dir`, or repository paths from an override) of its license texts.
    pub texts: Vec<String>,
    /// Whether `texts` are repository paths (from an override) rather than crate files.
    pub texts_from_repo: bool,
}

impl Crate {
    pub fn key(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
    pub fn crates_io_url(&self) -> String {
        if self.source.starts_with("registry+") {
            format!("https://crates.io/crates/{}/{}", self.name, self.version)
        } else {
            self.source.clone()
        }
    }
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn metadata(repo: &Path, prog: &RustProgram) -> Result<Value, String> {
    let manifest = repo.join(&prog.manifest);
    let out = Command::new(cargo())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
        ])
        .arg(&prog.target)
        .args(if prog.features.is_empty() {
            vec![]
        } else {
            vec!["--features".to_string(), prog.features.join(",")]
        })
        .arg("--manifest-path")
        .arg(&manifest)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata --manifest-path {} --filter-platform {} failed:\n{}",
            prog.manifest,
            prog.target,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))
}

/// `name version` -> checksum, from a Cargo.lock.
fn lock_checksums(lock: &Path) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(lock) else {
        return m;
    };
    let (mut name, mut version) = (String::new(), String::new());
    for l in text.lines() {
        let val = |k: &str| {
            l.strip_prefix(k)
                .and_then(|r| r.strip_prefix(" = \""))
                .and_then(|r| r.strip_suffix('"'))
                .map(str::to_string)
        };
        if l == "[[package]]" {
            name.clear();
            version.clear();
        } else if let Some(v) = val("name") {
            name = v;
        } else if let Some(v) = val("version") {
            version = v;
        } else if let Some(v) = val("checksum") {
            m.insert(format!("{name} {version}"), v);
        }
    }
    m
}

/// Whether a file or directory name in a crate's root is a license or notice text.
pub fn license_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "license",
        "licence",
        "copying",
        "copyright",
        "notice",
        "unlicense",
    ]
    .iter()
    .any(|p| n.starts_with(p))
}

/// The license texts in a crate's source: root files with a license-like name, the files of a
/// root directory with such a name, and the `license-file` the manifest names; sorted.
pub fn crate_texts(dir: &Path, license_file: Option<&str>) -> Vec<String> {
    let mut v = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !license_name(&name) {
                continue;
            }
            let p = e.path();
            if p.is_file() {
                v.insert(name);
            } else if p.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&p) {
                    for s in sub.flatten() {
                        if s.path().is_file() {
                            v.insert(format!("{name}/{}", s.file_name().to_string_lossy()));
                        }
                    }
                }
            }
        }
    }
    if let Some(f) = license_file {
        if dir.join(f).is_file() {
            v.insert(f.to_string());
        }
    }
    v.into_iter().collect()
}

/// Every third-party crate of every Rust program of the list, by `name version`.
pub fn crates(repo: &Path, list: &List) -> Result<BTreeMap<String, Crate>, String> {
    let overrides: BTreeMap<String, &RustOverride> = list
        .rust_overrides
        .iter()
        .map(|o| (format!("{} {}", o.name, o.version), o))
        .collect();
    let mut all: BTreeMap<String, Crate> = BTreeMap::new();
    for prog in &list.rust {
        let meta = metadata(repo, prog)?;
        let ws_root = meta["workspace_root"].as_str().unwrap_or("");
        let sums = lock_checksums(&Path::new(ws_root).join("Cargo.lock"));
        let empty = vec![];
        let pkgs = meta["packages"].as_array().unwrap_or(&empty);
        let by_id: BTreeMap<&str, &Value> = pkgs
            .iter()
            .filter_map(|p| Some((p["id"].as_str()?, p)))
            .collect();
        let nodes: BTreeMap<&str, &Value> = meta["resolve"]["nodes"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(|n| Some((n["id"].as_str()?, n)))
            .collect();
        let mut queue: VecDeque<&str> = VecDeque::new();
        for want in &prog.packages {
            let id = pkgs
                .iter()
                .find(|p| p["name"] == want.as_str() && p["source"].is_null())
                .and_then(|p| p["id"].as_str())
                .ok_or_else(|| format!("{}: no workspace package {want}", prog.manifest))?;
            queue.push_back(id);
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = nodes.get(id) else { continue };
            for d in node["deps"].as_array().unwrap_or(&empty) {
                let shipped = d["dep_kinds"]
                    .as_array()
                    .unwrap_or(&empty)
                    .iter()
                    .any(|k| k["kind"].is_null() || k["kind"] == "build");
                if shipped {
                    if let Some(p) = d["pkg"].as_str() {
                        queue.push_back(p);
                    }
                }
            }
        }
        for id in seen {
            let p = by_id[id];
            if p["source"].is_null() {
                continue; // hydo's own path packages
            }
            let name = p["name"].as_str().unwrap_or("").to_string();
            let version = p["version"].as_str().unwrap_or("").to_string();
            let key = format!("{name} {version}");
            let entry = all.entry(key.clone()).or_insert_with(|| {
                let dir = Path::new(p["manifest_path"].as_str().unwrap_or(""))
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_path_buf();
                let license_file = p["license_file"].as_str().map(str::to_string);
                let ov = overrides.get(&key);
                let mut texts = crate_texts(&dir, license_file.as_deref());
                let mut texts_from_repo = false;
                if texts.is_empty() {
                    if let Some(o) = ov {
                        texts = o.texts.clone();
                        texts_from_repo = true;
                    }
                }
                let license = p["license"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| ov.and_then(|o| o.license.clone()));
                Crate {
                    name: name.clone(),
                    version: version.clone(),
                    license,
                    license_file,
                    dir,
                    source: p["source"].as_str().unwrap_or("").to_string(),
                    checksum: sums.get(&key).cloned(),
                    used_by: BTreeSet::new(),
                    texts,
                    texts_from_repo,
                }
            });
            entry.used_by.extend(prog.files.iter().cloned());
        }
    }
    Ok(all)
}
