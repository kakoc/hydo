//! The commit this hydo is built from, for the build record (`<image>.json`) and every run's
//! `result.json`: the file `hydo/COMMIT` when it exists (a tree copied without its `.git`, as the
//! release scripts copy it to the bare-metal machine, gets the commit written there; the file is
//! git-ignored), else `git rev-parse HEAD` in this tree, else nothing (the records then leave the
//! commit out). Only files that exist are watched, so a tree without git does not rebuild the
//! crate every time.
//!
//! A crate built from its published package (crates.io, or a `.crate` file: cargo writes
//! `Cargo.toml.orig` into every package) never asks git and never reads `hydo/COMMIT`, because the
//! directory it is unpacked in may sit inside some other repository (a home directory kept in
//! git): its commit is the `sha1` of the `.cargo_vcs_info.json` that `cargo package` wrote from the
//! repository it packaged, and nothing when that file is missing or says the packaged tree was
//! dirty (docs/PUBLISHING.md).

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn watch(p: &Path) {
    if p.exists() {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

/// The commit a packaged crate was made from: `git.sha1` of `.cargo_vcs_info.json`, unless the
/// packaged tree was dirty. A small reader, so that the build script needs no JSON crate.
fn packaged_commit(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".cargo_vcs_info.json")).ok()?;
    let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if flat.contains("\"dirty\":true") {
        return None;
    }
    let key = "\"sha1\":\"";
    let rest = &flat[flat.find(key)? + key.len()..];
    let sha1 = &rest[..rest.find('"')?];
    (sha1.len() == 40 && sha1.bytes().all(|b| b.is_ascii_hexdigit())).then(|| sha1.to_string())
}

fn tree_commit(dir: &Path) -> Option<String> {
    let file = dir.join("../../COMMIT");
    watch(&file);
    if let Ok(t) = std::fs::read_to_string(&file) {
        return Some(t.trim().to_string()).filter(|s| !s.is_empty());
    }
    let c = git(dir, &["rev-parse", "HEAD"]);
    if let Some(head) = git(dir, &["rev-parse", "--git-path", "HEAD"]) {
        let head = dir.join(head);
        watch(&head);
        if let Ok(t) = std::fs::read_to_string(&head) {
            if let Some(r) = t.trim().strip_prefix("ref: ") {
                if let Some(rp) = git(dir, &["rev-parse", "--git-path", r]) {
                    watch(&dir.join(rp));
                }
            }
        }
        if let Some(pr) = git(dir, &["rev-parse", "--git-path", "packed-refs"]) {
            watch(&dir.join(pr));
        }
    }
    c
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let commit = if dir.join("Cargo.toml.orig").exists() {
        packaged_commit(dir)
    } else {
        tree_commit(dir)
    };
    println!(
        "cargo:rustc-env=BUILT_FROM_COMMIT={}",
        commit.unwrap_or_default()
    );
}
