//! The inventory cannot drift (M144):
//!
//! - `INVENTORY.md` is `hydo_config::registry::inventory_markdown()` (`HYDO_BLESS=1` rewrites it);
//! - no crate reads a `HYDO_*`, `HVX_*` or `HVM_*` variable from the environment outside this
//!   crate (a grep of `env::var`, `var_os`, `set_var`, `remove_var` calls), and only programs'
//!   `main`s, tests and examples call `Env::process` / `Config::process`;
//! - every such name the crates use (any exact string literal in a crate's code, and the names
//!   tests and examples read or set) is in the inventory, and no name of the inventory is unused;
//! - the names removed in M144 are gone.

use hydo_config::registry::{inventory_markdown, Kind, REMOVED, VARIABLES};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

/// The workspace members (from the root Cargo.toml) and the guest programs with their own
/// workspaces (hydo-agent, hydo-read).
fn crates() -> Vec<PathBuf> {
    let r = root();
    let toml = std::fs::read_to_string(r.join("Cargo.toml")).unwrap();
    let mut v: Vec<PathBuf> = toml
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("\"hydo/crates/"))
        .filter_map(|l| l.strip_suffix("\","))
        .map(|n| r.join("hydo/crates").join(n))
        .collect();
    assert!(v.len() > 10, "members not found in Cargo.toml");
    v.push(r.join("hydo/crates/hydo-agent"));
    v.push(r.join("hydo/crates/hydo-read"));
    v
}

fn name_of(c: &Path) -> String {
    c.file_name().unwrap().to_string_lossy().into_owned()
}

/// Guest programs: their bytes are pinned inside images, so they read their environment
/// themselves; their names are the guest interface (kind Guest).
const GUEST_PROGRAMS: &[&str] = &["hydo-agent", "hydo-read", "hydo-world", "hydo-grpc"];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut es: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    es.sort();
    for p in es {
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "golden" || n == "target")
            {
                continue;
            }
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// A source file's code: everything before its test module.
fn code_of(text: &str) -> &str {
    let mut at = 0;
    for l in text.split_inclusive('\n') {
        if l.trim() == "#[cfg(test)]" {
            return &text[..at];
        }
        at += l.len();
    }
    text
}

/// Exact string literals that are a hydo variable name, with the byte offset of their quote.
fn names(text: &str) -> Vec<(usize, String)> {
    let b = text.as_bytes();
    let mut v = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let rest = &text[i + 1..];
            if rest.starts_with("HYDO_") || rest.starts_with("HVX_") || rest.starts_with("HVM_") {
                let n = rest
                    .bytes()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
                    .count();
                // a whole name (not a prefix such as "HYDO_")
                if rest.as_bytes().get(n) == Some(&b'"') && !rest[..n].ends_with('_') {
                    v.push((i, rest[..n].to_string()));
                    i += n + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    v
}

/// The call a literal is the first argument of (`var`, `var_os`, `env`, ...), if any.
fn callee(text: &str, at: usize) -> Option<&str> {
    let before = text[..at].trim_end();
    let before = before.strip_suffix('(')?;
    let id: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let start = before.len() - id.len();
    Some(&before[start..])
}

const ENV_CALLS: &[&str] = &[
    "var",
    "var_os",
    "env",
    "env_remove",
    "set_var",
    "remove_var",
];

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

#[test]
fn inventory_md_is_current() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("INVENTORY.md");
    let want = inventory_markdown();
    if std::env::var("HYDO_BLESS").is_ok_and(|v| v == "1") {
        std::fs::write(&p, &want).unwrap();
    }
    let have = std::fs::read_to_string(&p).unwrap_or_default();
    assert!(
        have == want,
        "{} is not the inventory: HYDO_BLESS=1 cargo test -p hydo-config rewrites it",
        p.display()
    );
}

#[test]
fn every_entry_is_well_formed() {
    let mut seen = std::collections::BTreeSet::new();
    for v in VARIABLES {
        assert!(seen.insert(v.name), "{} twice", v.name);
        assert!(!v.doc.trim().is_empty(), "{} has no documentation", v.name);
        assert!(!v.ty.is_empty() && !v.readers.is_empty(), "{}", v.name);
    }
    for r in REMOVED {
        assert!(
            !seen.contains(r.name),
            "{} is both listed and removed",
            r.name
        );
    }
}

/// Reads that may name a variable by a non-literal (system variables only), with why.
const NON_LITERAL_READS: &[(&str, &str)] = &[
    (
        "hydo-cli/src/module.rs",
        "getpass.getuser(): LOGNAME, USER, LNAME, USERNAME",
    ),
    (
        "hydo-cli/src/ci/env.rs",
        "the CI systems' own variables (GITHUB_SHA, CI_COMMIT_SHA, CIRCLE_SHA1, ...): `hydo ci \
         run`'s table of what each CI system says about its job (docs/design/ci.md section 8.4)",
    ),
    (
        "hydo-engine/src/inproc.rs",
        "the module lock's default: SUDO_USER, SUDO_UID, SUDO_GID, HOME",
    ),
];

#[test]
fn only_hydo_config_reads_the_environment() {
    let r = root();
    let mut bad = Vec::new();
    for c in crates() {
        let n = name_of(&c);
        if n == "hydo-config" || GUEST_PROGRAMS.contains(&n.as_str()) {
            continue;
        }
        let mut files = Vec::new();
        rs_files(&c.join("src"), &mut files);
        for f in files {
            let rel = f
                .strip_prefix(r.join("hydo/crates"))
                .unwrap()
                .display()
                .to_string();
            let text = std::fs::read_to_string(&f).unwrap();
            let code = code_of(&text);
            for call in [
                "env::var(",
                "env::var_os(",
                "env::set_var(",
                "env::remove_var(",
            ] {
                for (at, _) in code.match_indices(call) {
                    let arg = code[at + call.len()..].trim_start();
                    let ok = if let Some(lit) = arg.strip_prefix('"') {
                        !(lit.starts_with("HYDO_")
                            || lit.starts_with("HVX_")
                            || lit.starts_with("HVM_"))
                    } else {
                        NON_LITERAL_READS.iter().any(|(p, _)| *p == rel)
                    };
                    if !ok {
                        bad.push(format!("{rel}:{}: {call}...", line_of(code, at)));
                    }
                }
            }
            // the environment is read once, at a program's start
            let entry = rel.contains("/src/bin/") || rel.ends_with("/src/main.rs");
            for call in ["Env::process()", "Config::process()"] {
                if !entry && code.contains(call) {
                    bad.push(format!("{rel}: {call} outside a program's main"));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "read hydo's variables through hydo_config (see its lib.rs, \"How to add an option\"):\n{}",
        bad.join("\n")
    );
}

#[test]
fn every_name_is_in_the_inventory_and_every_entry_is_used() {
    let r = root();
    let mut used: std::collections::BTreeSet<String> = Default::default();
    let mut bad = Vec::new();
    for c in crates() {
        let n = name_of(&c);
        if n == "hydo-config" {
            continue;
        }
        for (sub, all) in [
            ("src", true),
            ("tests", false),
            ("examples", false),
            ("benches", false),
        ] {
            let mut files = Vec::new();
            rs_files(&c.join(sub), &mut files);
            for f in files {
                let text = std::fs::read_to_string(&f).unwrap();
                let code = if all { code_of(&text) } else { &text };
                for (at, name) in names(code) {
                    used.insert(name.clone());
                    // in tests and examples only the names they read or set are variables (the
                    // others are manifest.sh lines, ABI constants and expected output)
                    let env_use = callee(code, at).is_some_and(|c| ENV_CALLS.contains(&c));
                    if (all || env_use) && hydo_config::registry::find(&name).is_none() {
                        bad.push(format!(
                            "{}:{}: {name} is not in hydo_config::registry",
                            f.strip_prefix(&r).unwrap().display(),
                            line_of(code, at)
                        ));
                    }
                }
            }
        }
    }
    // a host variable is used when a typed group of hydo-config reads it
    let mut cfg = String::new();
    let mut files = Vec::new();
    rs_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    for f in files {
        if !f.ends_with("registry.rs") {
            cfg += &std::fs::read_to_string(&f).unwrap();
        }
    }
    for v in VARIABLES {
        let read = if v.kind.is_host_config() {
            cfg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|w| w == v.name)
        } else {
            used.contains(v.name)
        };
        if !read {
            bad.push(format!(
                "{} ({:?}) is in the inventory but nothing uses it",
                v.name, v.kind
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn removed_names_are_gone() {
    let mut bad = Vec::new();
    for c in crates() {
        if name_of(&c) == "hydo-config" {
            continue;
        }
        for sub in ["src", "tests", "examples"] {
            let mut files = Vec::new();
            rs_files(&c.join(sub), &mut files);
            for f in files {
                let text = std::fs::read_to_string(&f).unwrap();
                for (at, name) in names(&text) {
                    if REMOVED.iter().any(|r| r.name == name) {
                        bad.push(format!("{}:{}: {name}", f.display(), line_of(&text, at)));
                    }
                }
                for r in REMOVED {
                    if text.contains(&format!("\"{}=", r.name)) {
                        bad.push(format!("{}: {}=", f.display(), r.name));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "removed in M144:\n{}", bad.join("\n"));
}

#[test]
fn kinds_are_counted() {
    // the counts the inventory and docs/HISTORY.md "Milestone 144" state
    let n = |k: Kind| VARIABLES.iter().filter(|v| v.kind == k).count();
    eprintln!(
        "user {} developer {} test {} guest {} image {} passed {} removed {}",
        n(Kind::User),
        n(Kind::Developer),
        n(Kind::Test),
        n(Kind::Guest),
        n(Kind::Image),
        n(Kind::Passed),
        REMOVED.len()
    );
}
