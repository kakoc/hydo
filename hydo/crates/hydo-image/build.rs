//! The commit this hydo is built from, for the build record (`<image>.json`) and every run's
//! `result.json`: the file `hydo/COMMIT` when it exists (a tree copied without its `.git`, as the
//! release scripts copy it to the bare-metal machine, gets the commit written there; the file is
//! git-ignored), else `git rev-parse HEAD` in this tree, else nothing (the records then leave the
//! commit out). Only files that exist are watched, so a tree without git does not rebuild the
//! crate every time.

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

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let file = dir.join("../../COMMIT");
    watch(&file);
    let commit = if let Ok(t) = std::fs::read_to_string(&file) {
        Some(t.trim().to_string()).filter(|s| !s.is_empty())
    } else {
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
    };
    println!(
        "cargo:rustc-env=BUILT_FROM_COMMIT={}",
        commit.unwrap_or_default()
    );
}
