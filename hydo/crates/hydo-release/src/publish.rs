//! The crates `cargo install hydo` builds, and whether each can be published (Milestone 169,
//! docs/PUBLISHING.md "Publishing the hydo command").
//!
//! The package `hydo` (directory `hydo/crates/hydo-cli`) and every workspace crate it depends on
//! (not counting dev-dependencies) are published to crates.io under their own names: the
//! *closure*. [`check`] says what stops one of them from being published or from building from
//! crates.io alone, so `cargo test -p hydo-release --test it crates::` fails before `cargo publish`
//! would, or before a published crate would fail to build on a user's machine:
//!
//! - its manifest: not `publish = false`; a description, the license Apache-2.0, the repository
//!   and homepage; the version of `hydo/VERSION` (except the crates of [`OWN_VERSION`]); every
//!   dependency on a workspace crate with that crate's version (`version = "…"` next to its
//!   `path`, which `[workspace.dependencies]` gives), and
//!   every dev-dependency on a workspace crate by path only (cargo leaves those out of the
//!   published manifest, so the tests' dependency cycles are not published);
//! - its package, as `cargo package --list` gives it: the repository's `LICENSE` (a link in the
//!   crate's directory), no tests, examples or benchmarks (their recorded answers are large and
//!   stay in the repository), at most 10 MiB of files (crates.io's limit, which applies to the
//!   compressed `.crate`), and every file present;
//! - files read at compile time: every `include_str!`, `include_bytes!` and `include!` of a
//!   string literal in a packaged Rust file names a file inside the crate's directory that is in
//!   the package (a file of the repository is reached through a link in the crate, whose content
//!   cargo copies into the package), and a build script names no path outside the crate except
//!   the ones [`OUTSIDE_ALLOWED`] lists with the reason.
//!
//! - its pages: [`page_problems`] (a README that crates.io and docs.rs both show, with absolute
//!   links, and for an internal crate the sentence that it has no stable API).
//!
//! It also checks that every other workspace crate has `publish = false` (so that a
//! `cargo publish --workspace` cannot publish a development tool or a guest program by accident),
//! that the guest programs are not in the closure (they come from the release download, never
//! from cargo), and that docs/PUBLISHING.md lists the closure in an order that publishes every
//! crate after the crates it depends on.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// The package `cargo install` installs.
pub const ROOT: &str = "hydo";

/// The repository and homepage of every published crate.
pub const REPOSITORY: &str = "https://github.com/kakoc/hydo";

/// The license of every published crate.
pub const LICENSE: &str = "Apache-2.0";

/// crates.io's limit on a `.crate` file (applied here to the uncompressed files, which is
/// stricter).
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;

/// The guest programs: they are inside guest images, come from the release download and must
/// never be something `cargo install hydo` builds.
pub const GUEST_PROGRAMS: &[&str] = &[
    "hydo-agent",
    "hydo-world",
    "hydo-read",
    "hydo-grpc",
    "hydo-sql",
];

/// Published crates whose version is their own, not hydo's, with the reason.
pub const OWN_VERSION: &[(&str, &str)] = &[(
    "hydo-agent-proto",
    "the guest agent (hydo/crates/hydo-agent, its own workspace) compiles it and the agent's \
     bytes are inside guest images, so its version moves with the protocol, not with every release",
)];

/// Paths outside its directory that a published crate's build script may name, with the reason.
pub const OUTSIDE_ALLOWED: &[(&str, &str, &str)] = &[(
    "hydo-image",
    "../../COMMIT",
    "read only in the repository (a tree copied without .git); a packaged crate takes its commit \
     from .cargo_vcs_info.json and never reads it",
)];

/// The files cargo writes into every package itself.
const GENERATED: &[&str] = &[
    "Cargo.toml",
    "Cargo.toml.orig",
    "Cargo.lock",
    ".cargo_vcs_info.json",
];

/// One workspace crate, as `cargo metadata` describes it.
#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    /// None: may be published anywhere; Some(empty): `publish = false`.
    pub publish: Option<Vec<String>>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub readme: Option<String>,
    /// Dependencies on other workspace crates: (name, kind, version requirement).
    pub path_deps: Vec<(String, String, String)>,
    /// The names of its binaries.
    pub bins: Vec<String>,
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn s(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(str::to_string)
}

/// Every workspace crate, by name.
pub fn members(repo: &Path) -> Result<BTreeMap<String, Member>, String> {
    let out = Command::new(cargo())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .arg("--manifest-path")
        .arg(repo.join("Cargo.toml"))
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata output: {e}"))?;
    let mut m = BTreeMap::new();
    for p in meta["packages"].as_array().into_iter().flatten() {
        let manifest = PathBuf::from(s(p, "manifest_path").unwrap_or_default());
        let path_deps = p["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["path"].is_string())
            .map(|d| {
                (
                    s(d, "name").unwrap_or_default(),
                    s(d, "kind").unwrap_or_else(|| "normal".into()),
                    s(d, "req").unwrap_or_default(),
                )
            })
            .collect();
        let bins = p["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| {
                t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k == "bin"))
            })
            .filter_map(|t| s(t, "name"))
            .collect();
        let publish = p["publish"].as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        });
        let name = s(p, "name").unwrap_or_default();
        m.insert(
            name.clone(),
            Member {
                name,
                version: s(p, "version").unwrap_or_default(),
                dir: manifest.parent().map(Path::to_path_buf).unwrap_or_default(),
                publish,
                description: s(p, "description"),
                license: s(p, "license"),
                repository: s(p, "repository"),
                homepage: s(p, "homepage"),
                readme: s(p, "readme"),
                path_deps,
                bins,
            },
        );
    }
    Ok(m)
}

/// The closure of [`ROOT`]: it and every workspace crate it needs to build (normal and build
/// dependencies).
pub fn closure(members: &BTreeMap<String, Member>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![ROOT.to_string()];
    while let Some(n) = todo.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        if let Some(m) = members.get(&n) {
            for (d, kind, _) in &m.path_deps {
                if kind != "dev" {
                    todo.push(d.clone());
                }
            }
        }
    }
    seen
}

/// A publishing order of the closure: every crate after the crates it depends on, ties broken by
/// name.
pub fn order(members: &BTreeMap<String, Member>) -> Vec<String> {
    let set = closure(members);
    let deps = |n: &str| -> BTreeSet<String> {
        members[n]
            .path_deps
            .iter()
            .filter(|(d, k, _)| k != "dev" && set.contains(d))
            .map(|(d, _, _)| d.clone())
            .collect()
    };
    let mut done: Vec<String> = Vec::new();
    let mut left: BTreeSet<String> = set.clone();
    while !left.is_empty() {
        let next = left
            .iter()
            .find(|n| deps(n).iter().all(|d| done.contains(d)))
            .cloned();
        match next {
            Some(n) => {
                left.remove(&n);
                done.push(n);
            }
            None => break, // a cycle: check() reports it through the documented order
        }
    }
    done
}

/// `a/b/../c` -> `a/c`, without touching the file system.
pub fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if out.file_name().is_some() {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A Rust source as code and string literals: comments are dropped, each plain string literal
/// (`"…"`, escapes kept as written) is one `Str`, everything else is `Code`. Raw strings and
/// byte strings count as code, which is enough for the paths this module looks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    Code(String),
    Str(String),
}

pub fn pieces(src: &str) -> Vec<Piece> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut code = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == '*' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == '"' {
            out.push(Piece::Code(std::mem::take(&mut code)));
            let mut lit = String::new();
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' && i + 1 < b.len() {
                    lit.push(b[i]);
                    i += 1;
                }
                lit.push(b[i]);
                i += 1;
            }
            i += 1;
            out.push(Piece::Str(lit));
        } else if c == '\'' {
            // a character literal ('x', '\n', '\'', '"'); otherwise a lifetime
            let len = if next == Some('\\') {
                b[i + 2..].iter().position(|&x| x == '\'').map(|p| p + 3)
            } else if b.get(i + 2) == Some(&'\'') {
                Some(3)
            } else {
                None
            };
            let len = len.unwrap_or(1);
            code.extend(&b[i..(i + len).min(b.len())]);
            i += len;
        } else {
            code.push(c);
            i += 1;
        }
    }
    out.push(Piece::Code(code));
    out
}

/// The string literals that are the first argument of `include_str!(`, `include_bytes!(` and
/// `include!(` in a Rust source (an include of anything else, such as
/// `concat!(env!("OUT_DIR"), …)`, is not a path in the package).
pub fn included_literals(src: &str) -> Vec<String> {
    let p = pieces(src);
    let mut v = Vec::new();
    for w in p.windows(2) {
        if let (Piece::Code(c), Piece::Str(s)) = (&w[0], &w[1]) {
            let c = c.trim_end();
            if ["include_str!(", "include_bytes!(", "include!("]
                .iter()
                .any(|m| c.ends_with(m))
            {
                v.push(s.clone());
            }
        }
    }
    v
}

/// Every string literal of a Rust source that contains `../`.
pub fn parent_literals(src: &str) -> Vec<String> {
    pieces(src)
        .into_iter()
        .filter_map(|p| match p {
            Piece::Str(s) if s.contains("../") => Some(s),
            _ => None,
        })
        .collect()
}

/// `cargo package --list` of one crate.
pub fn package_list(m: &Member) -> Result<Vec<String>, String> {
    let out = Command::new(cargo())
        .args(["package", "--list", "--allow-dirty", "--offline", "--quiet"])
        .arg("--manifest-path")
        .arg(m.dir.join("Cargo.toml"))
        .current_dir(&m.dir)
        .output()
        .map_err(|e| format!("cannot run cargo package --list: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{}: cargo package --list failed:\n{}",
            m.name,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect())
}

/// The crate names of `cargo publish … -p <name>` lines of docs/PUBLISHING.md, in order.
pub fn documented_order(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("cargo publish --registry crates-io")?;
            let mut w = rest.split_whitespace();
            while let Some(a) = w.next() {
                if a == "-p" {
                    return w.next().map(str::to_string);
                }
            }
            None
        })
        .collect()
}

/// What stops the closure from being published, one message per problem (empty: nothing).
pub fn check(repo: &Path) -> Result<Vec<String>, String> {
    let members = members(repo)?;
    let mut errs = Vec::new();
    if !members.contains_key(ROOT) {
        return Ok(vec![format!("no workspace package named {ROOT}")]);
    }
    let version = std::fs::read_to_string(repo.join("hydo/VERSION"))
        .map_err(|e| format!("hydo/VERSION: {e}"))?
        .trim()
        .to_string();
    let license = std::fs::read(repo.join("LICENSE")).map_err(|e| format!("LICENSE: {e}"))?;
    let set = closure(&members);

    let root = &members[ROOT];
    if !root.bins.iter().any(|b| b == ROOT) {
        errs.push(format!("{ROOT}: no binary named {ROOT}"));
    }
    if root.readme.is_none() {
        errs.push(format!("{ROOT}: no readme (the crates.io page shows it)"));
    }
    for g in GUEST_PROGRAMS {
        if set.contains(*g) {
            errs.push(format!(
                "{g} is a guest program but {ROOT} depends on it: guest programs come from the \
                 release download, cargo must not build them"
            ));
        }
    }
    for (name, m) in &members {
        if !set.contains(name) && m.publish.as_ref().is_none_or(|p| !p.is_empty()) {
            errs.push(format!(
                "{name}: not needed by {ROOT}, so it must have publish = false"
            ));
        }
    }
    for name in &set {
        let Some(m) = members.get(name) else {
            errs.push(format!("{name}: not a workspace package"));
            continue;
        };
        let own = OWN_VERSION.iter().any(|(c, _)| c == name);
        errs.extend(check_member(m, &members, own, &version, &license, &set));
    }

    // the documented order
    let doc = std::fs::read_to_string(repo.join("docs/PUBLISHING.md")).unwrap_or_default();
    let listed = documented_order(&doc);
    let listed_set: BTreeSet<String> = listed.iter().cloned().collect();
    if listed_set != set || listed.len() != set.len() {
        errs.push(format!(
            "docs/PUBLISHING.md: the `cargo publish --registry crates-io -p <crate>` lines name \
             {listed:?}; they must name each of {set:?} once"
        ));
    }
    for (i, n) in listed.iter().enumerate() {
        if let Some(m) = members.get(n) {
            for (d, k, _) in &m.path_deps {
                if k != "dev" && set.contains(d) && !listed[..i].contains(d) {
                    errs.push(format!(
                        "docs/PUBLISHING.md: {n} is published before {d}, which it depends on"
                    ));
                }
            }
        }
    }
    Ok(errs)
}

fn check_member(
    m: &Member,
    members: &BTreeMap<String, Member>,
    own_version: bool,
    version: &str,
    license: &[u8],
    set: &BTreeSet<String>,
) -> Vec<String> {
    let n = &m.name;
    let mut errs = Vec::new();
    if m.publish.is_some() {
        errs.push(format!(
            "{n}: {ROOT} needs it, so it is published: remove its `publish` key"
        ));
    }
    if m.description.as_deref().unwrap_or("").trim().is_empty() {
        errs.push(format!("{n}: no description"));
    }
    if m.license.as_deref() != Some(LICENSE) {
        errs.push(format!("{n}: license {:?}, expected {LICENSE}", m.license));
    }
    if m.repository.as_deref() != Some(REPOSITORY) {
        errs.push(format!(
            "{n}: repository {:?}, expected {REPOSITORY} (repository.workspace = true)",
            m.repository
        ));
    }
    if m.homepage.as_deref() != Some(REPOSITORY) {
        errs.push(format!(
            "{n}: homepage {:?}, expected {REPOSITORY} (homepage.workspace = true)",
            m.homepage
        ));
    }
    if !own_version && m.version != version {
        errs.push(format!(
            "{n}: version {}, but hydo/VERSION is {version}",
            m.version
        ));
    }
    for (d, kind, req) in &m.path_deps {
        if kind == "dev" {
            if req != "*" {
                errs.push(format!(
                    "{n}: the dev-dependency {d} has the version {req}: give it by path only, so \
                     that it is left out of the published manifest"
                ));
            }
        } else if let Some(dm) = members.get(d).filter(|_| set.contains(d)) {
            let v = &dm.version;
            if req != &format!("^{v}") {
                errs.push(format!(
                    "{n}: the dependency {d} needs `version = \"{v}\"` next to its path (it has \
                     {req:?}; [workspace.dependencies] gives it)"
                ));
            }
        }
    }

    let files = match package_list(m) {
        Ok(f) => f,
        Err(e) => {
            errs.push(e);
            return errs;
        }
    };
    let listed: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    if !listed.contains("LICENSE") {
        errs.push(format!(
            "{n}: LICENSE is not in the package (a link LICENSE -> ../../../LICENSE in its \
             directory, and \"/LICENSE\" in `include`)"
        ));
    } else if std::fs::read(m.dir.join("LICENSE")).ok().as_deref() != Some(license) {
        errs.push(format!("{n}: its LICENSE is not the repository's LICENSE"));
    }
    if let Some(r) = &m.readme {
        if !listed.contains(r.as_str()) {
            errs.push(format!("{n}: its readme {r} is not in the package"));
        }
    }
    errs.extend(page_problems(m));
    let mut bytes = 0u64;
    for f in &files {
        if ["tests/", "examples/", "benches/"]
            .iter()
            .any(|p| f.starts_with(p))
        {
            errs.push(format!(
                "{n}: {f} is in the package: publish the sources only (`include` in its \
                 Cargo.toml)"
            ));
        }
        if GENERATED.contains(&f.as_str()) {
            continue;
        }
        match std::fs::metadata(m.dir.join(f)) {
            Ok(md) => bytes += md.len(),
            Err(e) => errs.push(format!(
                "{n}: {f} is in the package list but cannot be read: {e}"
            )),
        }
        if f.ends_with(".rs") {
            let Ok(src) = std::fs::read_to_string(m.dir.join(f)) else {
                continue;
            };
            let here = Path::new(f).parent().unwrap_or(Path::new(""));
            for lit in included_literals(&src) {
                let rel = lexical(&here.join(&lit));
                let inside = rel
                    .components()
                    .next()
                    .is_some_and(|c| c != Component::ParentDir)
                    && !rel.is_absolute();
                let r = rel.to_string_lossy().to_string();
                if !inside {
                    errs.push(format!(
                        "{n}: {f} includes {lit:?}, outside the crate's directory: link the file \
                         into the crate (the way hydo-cli/pins/ does) and include the link"
                    ));
                } else if !listed.contains(r.as_str()) {
                    errs.push(format!(
                        "{n}: {f} includes {lit:?} ({r}), which is not in the package"
                    ));
                }
            }
            if f == "build.rs" {
                for lit in parent_literals(&src) {
                    let allowed = OUTSIDE_ALLOWED.iter().any(|(c, p, _)| c == n && *p == lit);
                    if !allowed {
                        errs.push(format!(
                            "{n}: build.rs names {lit:?}, which may be outside the crate: a crate \
                             built from crates.io has nothing there (link what it needs into the \
                             crate, or list the path in hydo-release's OUTSIDE_ALLOWED with the \
                             reason)"
                        ));
                    }
                }
            }
        }
    }
    if bytes > MAX_BYTES {
        errs.push(format!(
            "{n}: the package's files are {bytes} bytes, over crates.io's {MAX_BYTES}"
        ));
    }
    errs
}

/// What the crates.io and docs.rs pages of a published crate need (docs/PUBLISHING.md, "The
/// crates' pages"): a `README.md` as its readme (crates.io shows it), every link in it absolute
/// (crates.io shows the README outside the repository, so a relative link leads nowhere), a
/// library's documentation starting with that README (`#![doc = include_str!("../README.md")]`
/// as the first line of `src/lib.rs`, so docs.rs shows the same page and its code blocks are
/// doctests), and, for every crate but the `hydo` command, the sentence that says it has no
/// stable API.
pub fn page_problems(m: &Member) -> Vec<String> {
    let n = &m.name;
    let mut errs = Vec::new();
    if m.readme.as_deref() != Some("README.md") {
        errs.push(format!(
            "{n}: readme is {:?}; set `readme = \"README.md\"` (the crates.io page)",
            m.readme
        ));
        return errs;
    }
    let Ok(text) = std::fs::read_to_string(m.dir.join("README.md")) else {
        errs.push(format!("{n}: README.md cannot be read"));
        return errs;
    };
    for target in relative_links(&text) {
        errs.push(format!(
            "{n}: README.md links to {target:?}; crates.io shows the README outside the \
             repository, so give an absolute https:// link"
        ));
    }
    let lib = m.dir.join("src/lib.rs");
    if let Ok(src) = std::fs::read_to_string(&lib) {
        if src.lines().next() != Some(README_DOC) {
            errs.push(format!(
                "{n}: the first line of src/lib.rs must be `{README_DOC}`, so that docs.rs shows \
                 the crates.io page and its examples run as doctests"
            ));
        }
    }
    if n != ROOT && !text.contains(NO_STABLE_API) {
        errs.push(format!(
            "{n}: README.md must say \"{NO_STABLE_API}\": the crate is published only because \
             {ROOT} is built from it"
        ));
    }
    errs
}

/// The first line of a published library's `src/lib.rs`.
pub const README_DOC: &str = "#![doc = include_str!(\"../README.md\")]";

/// What every internal crate's page says.
pub const NO_STABLE_API: &str = "This crate has no stable API.";

/// The targets of the Markdown links and images of `text` (`[text](target)`) that are neither
/// absolute (`https://`, `http://`, `mailto:`) nor an anchor on the page (`#...`). Code spans and
/// fenced blocks are skipped.
pub fn relative_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        // drop code spans
        let mut plain = String::new();
        let mut code = false;
        for c in line.chars() {
            if c == '`' {
                code = !code;
            } else if !code {
                plain.push(c);
            }
        }
        let mut rest = plain.as_str();
        while let Some(i) = rest.find("](") {
            let after = &rest[i + 2..];
            let end = after.find(')').unwrap_or(after.len());
            let target = after[..end].trim();
            let absolute = ["https://", "http://", "mailto:", "#"]
                .iter()
                .any(|p| target.starts_with(p));
            if !absolute {
                out.push(target.to_string());
            }
            rest = &after[end..];
        }
    }
    out
}
