//! The image input key (docs/design/ci.md section 7.2): an image is the same bytes when its inputs
//! are the same (`hydo build` is reproducible), so a key over the inputs names the image before it
//! is built, and an image cache keyed by it lets a change that touches no input reuse the image.
//!
//! `ik1-<sha256>` over one line per input, sorted:
//!
//! - `hydo <version> <commit>`: the builder itself, with the pins compiled into it;
//! - `arch <x86_64|arm64>` and every `set <KEY>=<VALUE>`;
//! - `file <path> <sha256>` for the manifest and every file of its `hydo/` folder (but `ci.yaml`
//!   and `known-findings.yaml`, which are not staged into the image: changing a budget or accepting
//!   a finding keeps the image);
//! - `src <path> <hash>` for every `src:` path the manifest stages, a file's or a directory's
//!   content (every file's relative path, executable bit and sha256): the content, not the git
//!   object, so a working tree with uncommitted changes gets the key of what it builds;
//! - `input <path> <hash>` for every path `hydo/ci.yaml` names in `inputs:`. When the CI settings
//!   have a `build:` command, a `src:` path that git does not track is taken to be that command's
//!   output, made from the declared inputs, and left out; so the key is known before the command
//!   runs, and an unchanged input skips both the command and the image build.
//!
//! OCI services are pinned by digest in the manifest, so the manifest's own line covers them.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The key's version prefix.
pub const VERSION: &str = "ik1";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn file_sha(p: &Path) -> std::io::Result<String> {
    Ok(hex(&Sha256::digest(std::fs::read(p)?)))
}

/// A file's or a directory's content hash; `missing` when the path does not exist.
pub fn content_hash(p: &Path) -> String {
    let Ok(md) = std::fs::metadata(p) else {
        return "missing".into();
    };
    if md.is_file() {
        return file_sha(p).unwrap_or_else(|_| "unreadable".into());
    }
    let mut files = Vec::new();
    walk(p, p, &mut files);
    files.sort();
    let mut h = Sha256::new();
    for (rel, exec, sha) in files {
        h.update(format!("{rel} {} {sha}\n", if exec { "x" } else { "-" }).as_bytes());
    }
    hex(&h.finalize())
}

fn walk(top: &Path, d: &Path, out: &mut Vec<(String, bool, String)>) {
    let Ok(rd) = std::fs::read_dir(d) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == ".DS_Store" {
            continue;
        }
        let Ok(md) = std::fs::metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            walk(top, &p, out);
        } else if md.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let rel = p
                .strip_prefix(top)
                .unwrap_or(&p)
                .to_string_lossy()
                .into_owned();
            let exec = md.permissions().mode() & 0o111 != 0;
            out.push((rel, exec, file_sha(&p).unwrap_or_default()));
        }
    }
}

/// The folders under a `hydo/` folder that hold what hydo made, not what it builds from: the build
/// folders (`.hydo`, `hydo-out`) and every run's or exploration's folder (one with a
/// `result.json` or an `exploration.json`). Their images, unpacked roots and disk images are no
/// input of an image. Before Milestone 374 the key read them all: `hydo explore --configs` on
/// the RabbitMQ example read 13 GB of its copies' builds for 23 minutes (open item 191).
fn is_output_dir(d: &Path) -> bool {
    let name = d.file_name().map(|n| n.to_string_lossy().into_owned());
    matches!(name.as_deref(), Some(".hydo" | "hydo-out"))
        || d.join("result.json").is_file()
        || d.join("exploration.json").is_file()
}

/// [`walk`] without the folders [`is_output_dir`] names.
fn walk_inputs(top: &Path, d: &Path, out: &mut Vec<(String, bool, String)>) {
    let Ok(rd) = std::fs::read_dir(d) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = std::fs::metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            if is_output_dir(&p) || e.file_name() == ".git" {
                continue;
            }
            walk_inputs(top, &p, out);
        } else {
            let mut one = Vec::new();
            walk_one(top, &p, &md, &mut one);
            out.extend(one);
        }
    }
}

/// One file of [`walk`]: its relative path, its executable bit and its sha256.
fn walk_one(top: &Path, p: &Path, md: &std::fs::Metadata, out: &mut Vec<(String, bool, String)>) {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !md.is_file() || name == ".DS_Store" {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let rel = p
        .strip_prefix(top)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned();
    let exec = md.permissions().mode() & 0o111 != 0;
    out.push((rel, exec, file_sha(p).unwrap_or_default()));
}

/// Every `src:` value of the manifest's data (root files, directories, mounts, ...), as written.
pub fn src_paths(data: &serde_json::Value, out: &mut Vec<String>) {
    match data {
        serde_json::Value::Object(o) => {
            for (k, v) in o {
                match (k.as_str(), v) {
                    ("src", serde_json::Value::String(s)) => out.push(s.clone()),
                    _ => src_paths(v, out),
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| src_paths(v, out)),
        _ => {}
    }
}

/// Whether git tracks `p` (a file, or any file under a directory) in the repository around it.
fn tracked(p: &Path) -> bool {
    let (dir, name) = if p.is_dir() {
        (p.to_path_buf(), ".".to_string())
    } else {
        (
            p.parent().map(Path::to_path_buf).unwrap_or_default(),
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    };
    std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["ls-files", "--error-unmatch", "--", &name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// What the key is computed from.
pub struct Inputs<'a> {
    pub hydo: String,
    pub arch: &'a str,
    pub sets: &'a [(String, String)],
    /// The manifest file.
    pub manifest: &'a Path,
    /// The project's directory (`inputs:` paths are relative to it).
    pub project: &'a Path,
    /// The manifest's `src:` values, as written (relative to the manifest's directory).
    pub srcs: &'a [String],
    pub inputs: &'a std::collections::BTreeMap<String, Vec<String>>,
    /// The CI settings have a `build:` command.
    pub built_here: bool,
}

/// The key's lines (sorted) and the key.
pub fn input_key(i: &Inputs) -> (Vec<String>, String) {
    let mut lines = vec![format!("hydo {}", i.hydo), format!("arch {}", i.arch)];
    for (k, v) in i.sets {
        lines.push(format!("set {k}={v}"));
    }
    let mdir = i.manifest.parent().unwrap_or(Path::new("."));
    let rel = |p: &Path| -> String {
        p.strip_prefix(i.project)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    // the manifest, and the whole hydo/ folder when the manifest is in one, without hydo's own
    // outputs there (Milestone 374)
    let mut files = vec![i.manifest.to_path_buf()];
    if mdir.file_name().is_some_and(|n| n == "hydo") {
        let mut all = Vec::new();
        walk_inputs(mdir, mdir, &mut all);
        files = all
            .into_iter()
            .filter(|(r, _, _)| r != "ci.yaml" && r != "known-findings.yaml")
            .map(|(r, _, _)| mdir.join(r))
            .collect();
    }
    for f in files {
        lines.push(format!("file {} {}", rel(&f), content_hash(&f)));
    }
    for s in i.srcs {
        let p: PathBuf = mdir.join(s);
        if i.built_here && !i.inputs.is_empty() && !tracked(&p) {
            continue;
        }
        lines.push(format!("src {} {}", s, content_hash(&p)));
    }
    for paths in i.inputs.values() {
        for p in paths {
            lines.push(format!("input {} {}", p, content_hash(&i.project.join(p))));
        }
    }
    lines.sort();
    lines.dedup();
    let mut h = Sha256::new();
    for l in &lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    let key = format!("{VERSION}-{}", &hex(&h.finalize())[..32]);
    (lines, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-ci-ik-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("hydo")).unwrap();
        d
    }

    /// Milestone 374: hydo's own outputs in the hydo/ folder (the build folder, a run's or an
    /// exploration's folder) are no input: adding or changing them keeps the key.
    #[test]
    fn the_key_leaves_out_hydos_outputs() {
        let d = tmp("outputs");
        std::fs::write(d.join("hydo/hydo.yaml"), "name: x\n").unwrap();
        std::fs::create_dir_all(d.join("hydo/workload")).unwrap();
        std::fs::write(d.join("hydo/workload/serial_driver_a"), "#!/bin/sh\n").unwrap();
        let none = BTreeMap::new();
        let k = || {
            input_key(&Inputs {
                hydo: "0.7.0 abc".into(),
                arch: "x86_64",
                sets: &[],
                manifest: &d.join("hydo/hydo.yaml"),
                project: &d,
                srcs: &[],
                inputs: &none,
                built_here: false,
            })
        };
        let (lines, k1) = k();
        assert!(lines
            .iter()
            .any(|l| l.starts_with("file hydo/workload/serial_driver_a ")));
        for (dir, file) in [
            (".hydo", "explore-ob-x-disk.img"),
            ("hydo-out/runs/run-1", "one.log"),
            ("explore", "exploration.json"),
            ("e2/runs/seed-3", "result.json"),
        ] {
            std::fs::create_dir_all(d.join("hydo").join(dir)).unwrap();
            std::fs::write(d.join("hydo").join(dir).join(file), "made by hydo").unwrap();
            assert_eq!(k().1, k1, "{dir}/{file} is hydo's output");
        }
        // a file the folder plan reads is an input
        std::fs::write(d.join("hydo/workload/serial_driver_a"), "#!/bin/sh\necho\n").unwrap();
        assert_ne!(k().1, k1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_key_follows_the_inputs_and_not_the_ci_settings() {
        let d = tmp("a");
        std::fs::write(d.join("hydo/hydo.yaml"), "name: x\n").unwrap();
        std::fs::write(d.join("hydo/ci.yaml"), "format: 1\n").unwrap();
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("bin/app"), "v1").unwrap();
        let srcs = vec!["../bin/app".to_string()];
        let none = BTreeMap::new();
        let k = |sets: &[(String, String)]| {
            input_key(&Inputs {
                hydo: "0.2.0 abc".into(),
                arch: "arm64",
                sets,
                manifest: &d.join("hydo/hydo.yaml"),
                project: &d,
                srcs: &srcs,
                inputs: &none,
                built_here: false,
            })
            .1
        };
        let k1 = k(&[]);
        assert!(k1.starts_with("ik1-") && k1.len() == 4 + 32);
        std::fs::write(d.join("hydo/ci.yaml"), "format: 1\npr: {seeds: 1-9}\n").unwrap();
        assert_eq!(k(&[]), k1, "ci.yaml is not an input of the image");
        std::fs::write(
            d.join("hydo/known-findings.yaml"),
            "format: 1\naccepted: []\n",
        )
        .unwrap();
        assert_eq!(k(&[]), k1, "nor are the accepted findings");
        std::fs::write(d.join("bin/app"), "v2").unwrap();
        let k2 = k(&[]);
        assert_ne!(k2, k1, "a staged file changed");
        assert_ne!(k(&[("variant".into(), "buggy".into())]), k2);
        std::fs::write(d.join("hydo/check.sh"), "exit 0\n").unwrap();
        assert_ne!(k(&[]), k2, "a file of the hydo/ folder");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn src_paths_are_found_anywhere() {
        let v = serde_json::json!({"root": {"files": [{"src": "a", "dst": "/a"}], "dirs": [{"src": "d/"}]},
                                  "services": [{"mounts": [{"src": "m", "dst": "/m"}]}]});
        let mut s = Vec::new();
        src_paths(&v, &mut s);
        s.sort();
        assert_eq!(s, ["a", "d/", "m"]);
    }
}
