//! Embed the Claude Code plugin hydo with the onboarding skill (plugin/, the process), the guide (hydo/guide/, this
//! hydo's facts: the topics of `hydo guide`, the examples and `hydo init`'s template) and
//! MANIFEST.md into the binary, so that `hydo guide`, `hydo skill install` and `hydo init` work
//! from the `hydo` binary alone. A release that ships the plugin folder (plugin/) still
//! reads the skill from disk first (src/lib.rs); the guide is always the built-in one.
//!
//! All three are read through the links in this crate's embed/ folder (embed/plugin ->
//! plugin, embed/guide -> hydo/guide, embed/MANIFEST.md -> hydo/onboard/MANIFEST.md),
//! never from outside the crate: the crate published to crates.io carries the linked files'
//! contents there (docs/PUBLISHING.md), and the tree reads the repository's files through the same
//! links.

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn skipped(n: &str) -> bool {
    n == "__pycache__" || n == ".DS_Store" || n.starts_with("._")
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf, u32)>, dirs: &mut Vec<PathBuf>) {
    dirs.push(dir.to_path_buf());
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !skipped(n))
        .collect();
    names.sort();
    for n in names {
        let p = dir.join(&n);
        let r = if rel.is_empty() {
            n.clone()
        } else {
            format!("{rel}/{n}")
        };
        // follow symbolic links, as shutil.copytree(symlinks=False) does
        let md = std::fs::metadata(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        if md.is_dir() {
            walk(&p, &r, out, dirs);
        } else {
            // the mode git records (0755 or 0644), not the checkout's: a tree unpacked under umask
            // 002 has 0664 and 0775 files, and those modes went into `hydo init`'s files and
            // made the binary depend on the umask of whoever unpacked the tree (Milestone 172)
            let mode = if md.permissions().mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            out.push((r, p.canonicalize().unwrap(), mode));
        }
    }
}

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let skill = here.join("embed/plugin");
    let guide = here.join("embed/guide");
    let manifest = here.join("embed/MANIFEST.md").canonicalize().unwrap();
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut src = String::new();
    for (name, top) in [("FILES", &skill), ("GUIDE", &guide)] {
        let mut these = Vec::new();
        walk(top, "", &mut these, &mut dirs);
        writeln!(src, "pub static {name}: &[(&str, u32, &[u8])] = &[").unwrap();
        for (rel, p, mode) in &these {
            writeln!(
                src,
                "    ({rel:?}, 0o{mode:o}, include_bytes!({:?})),",
                p.display()
            )
            .unwrap();
        }
        src.push_str("];\n");
        files.extend(these);
    }
    writeln!(
        src,
        "pub static MANIFEST_MD: &[u8] = include_bytes!({:?});",
        manifest.display()
    )
    .unwrap();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded.rs");
    std::fs::write(out, src).unwrap();
    for d in &dirs {
        println!("cargo:rerun-if-changed={}", d.display());
    }
    for (_, p, _) in &files {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    println!("cargo:rerun-if-changed={}", manifest.display());
}
