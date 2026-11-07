//! The component list's completeness checks. Each returns one message per problem, each saying
//! what to add and where.

use crate::glob;
use crate::model::{asset_release_path, sums_paths, List, Platform};
use crate::rust::Crate;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Every component has an id, a name, a version, a license, files, license texts that exist, and
/// a source; a copyleft component's source archive is included and pinned; source files are
/// pinned by sha256 and named once; repository files exist.
pub fn components(repo: &Path, p: Platform, list: &List) -> Vec<String> {
    let lp = p.list_path();
    let mut errs = Vec::new();
    let mut ids = BTreeSet::new();
    let mut files: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for c in &list.components {
        let at = format!("{lp}: component {:?}", c.id);
        if c.id.is_empty() || !ids.insert(c.id.as_str()) {
            errs.push(format!("{at}: give every component a unique \"id\""));
        }
        for (k, v) in [
            ("name", &c.name),
            ("version", &c.version),
            ("license", &c.license),
        ] {
            if v.trim().is_empty() {
                errs.push(format!(
                    "{at}: add its \"{k}\" (the license as an SPDX expression)"
                ));
            }
        }
        if c.files.is_empty() {
            errs.push(format!("{at}: add the release paths it ships to \"files\""));
        }
        if c.texts.is_empty() {
            errs.push(format!(
                "{at}: add its full license texts to \"texts\" (repository paths; third-party texts live in hydo/dist/licenses/<source>/)"
            ));
        }
        for t in &c.texts {
            let ok = match t.strip_suffix('/') {
                Some(d) => !crate::notices::walk(&repo.join(d)).is_empty(),
                None => repo.join(t).is_file(),
            };
            if !ok {
                errs.push(format!(
                    "{at}: the license text {t} does not exist; copy it from the component's source into the repository at that path"
                ));
            }
        }
        if c.sources.is_empty() && c.repo.is_empty() {
            errs.push(format!(
                "{at}: add a \"sources\" entry (the upstream archive with \"file\", \"sha256\", \"url\" and \"from\"), or \"repo\" files for our own code"
            ));
        }
        let included = c.included_sources().count();
        if crate::copyleft(&c.license) && included == 0 && c.repo.is_empty() {
            errs.push(format!(
                "{at}: its license {:?} is copyleft, so its exact source archive must be included: add a \"sources\" entry with \"file\" and \"sha256\" (fetch it into the sources cache, docs/RELEASING.md)",
                c.license
            ));
        }
        for s in &c.sources {
            match (&s.file, &s.sha256) {
                (Some(f), Some(h)) => {
                    if !is_hex64(h) {
                        errs.push(format!(
                            "{at}: source {f}: \"sha256\" must be 64 lower-case hex digits"
                        ));
                    }
                    if f.contains('/') || f.is_empty() {
                        errs.push(format!("{at}: source {f:?}: \"file\" is a plain file name"));
                    }
                    if let Some((other, h2)) = files.insert(f, (&c.id, h)) {
                        if h2 != h {
                            errs.push(format!(
                                "{at}: source file {f} is also in component {other:?} with another sha256; give one of them another name"
                            ));
                        }
                    }
                    if s.from.is_none() {
                        errs.push(format!(
                            "{at}: source {f}: add \"from\" (how fetch-sources.sh obtains it)"
                        ));
                    }
                    if s.url.is_empty() && !s.from.as_deref().unwrap_or("").starts_with("nix") {
                        errs.push(format!("{at}: source {f}: add the upstream \"url\""));
                    }
                }
                (Some(f), None) => errs.push(format!("{at}: source {f}: add its \"sha256\"")),
                (None, Some(_)) => {
                    errs.push(format!("{at}: a source has a \"sha256\" but no \"file\""))
                }
                (None, None) => {
                    if s.url.is_empty() {
                        errs.push(format!("{at}: a source without \"file\" needs a \"url\""));
                    }
                }
            }
        }
        for r in &c.repo {
            if !repo.join(r).is_file() {
                errs.push(format!(
                    "{at}: the repository file {r} (in \"repo\") does not exist"
                ));
            }
        }
        for n in &c.nix {
            for o in &n.out {
                if !o.starts_with("/nix/store/") || o[11..].contains('/') {
                    errs.push(format!("{at}: nix out {o:?} is not a store path"));
                }
            }
        }
    }
    if !is_hex64(&list.nixpkgs.sha256) {
        errs.push(format!(
            "{lp}: nixpkgs.sha256 must be 64 lower-case hex digits"
        ));
    }
    errs
}

/// Every pinned file (assets.sha256 and guest-programs.sha256, at its release path) belongs to a
/// component.
pub fn pins(repo: &Path, p: Platform, list: &List) -> Vec<String> {
    let mut errs = Vec::new();
    let dist = repo.join(p.dist_dir());
    let assets = std::fs::read_to_string(dist.join("assets.sha256")).unwrap_or_default();
    let guest = std::fs::read_to_string(dist.join("guest-programs.sha256")).unwrap_or_default();
    for a in sums_paths(&assets) {
        match asset_release_path(p, &a) {
            None => errs.push(format!(
                "{}/assets.sha256: {a} has no release path (teach model::asset_release_path, and hydo-cli's setup.rs, where build-release.sh puts it)",
                p.dist_dir()
            )),
            Some(r) if !list.covers(&r) => errs.push(format!(
                "{}/assets.sha256: {a} (release path {r}) belongs to no component: add {r:?} (or a pattern) to the \"files\" of its component in {}",
                p.dist_dir(),
                p.list_path()
            )),
            _ => {}
        }
    }
    for r in sums_paths(&guest) {
        if !list.covers(&r) {
            errs.push(format!(
                "{}/guest-programs.sha256: {r} belongs to no component: add it to the \"files\" of a component in {}",
                p.dist_dir(),
                p.list_path()
            ));
        }
    }
    errs
}

/// Every literal destination the platform's build-release.sh writes in the release tree (`$REL/…`)
/// belongs to a component: a file path must match a pattern, a directory (ending in `/`, or a tree
/// copied there) must be the start of one. The release-time check of [`crate::notices`] walks the
/// real tree; this one catches a new `cp` before a release is built.
pub fn script(repo: &Path, p: Platform, list: &List) -> Vec<String> {
    let path = repo.join(p.dist_dir()).join("build-release.sh");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![format!("{}: cannot read", path.display())];
    };
    script_text(p, list, &text)
}

/// [`script`] on a script's text.
pub fn script_text(p: Platform, list: &List, text: &str) -> Vec<String> {
    let pats: Vec<&str> = list
        .components
        .iter()
        .flat_map(|c| c.files.iter())
        .chain(list.rust.iter().flat_map(|r| r.files.iter()))
        .map(|s| s.as_str())
        .collect();
    let mut errs = BTreeSet::new();
    for line in text.lines() {
        let l = line.trim_start();
        if l.starts_with('#') {
            continue;
        }
        let mut rest = line;
        while let Some(i) = rest.find("$REL/") {
            rest = &rest[i + 5..];
            let end = rest
                .find(|c: char| {
                    c == '"' || c == '$' || c == ' ' || c == '\'' || c == ')' || c == ';'
                })
                .unwrap_or(rest.len());
            let lit = &rest[..end];
            let is_var = rest[end..].starts_with('$');
            if lit.is_empty() || lit.starts_with('.') {
                continue;
            }
            let ok = if lit.ends_with('/') || is_var {
                // a directory (or a name built from a variable): some pattern must lie below it
                pats.iter().any(|q| {
                    let pre = glob::literal_prefix(q);
                    pre.starts_with(lit) || (q.contains('*') && lit.starts_with(pre))
                })
            } else {
                pats.iter().any(|q| glob::matches(q, lit))
                    || pats
                        .iter()
                        .any(|q| glob::literal_prefix(q).starts_with(&format!("{lit}/")))
            };
            if !ok {
                errs.insert(format!(
                    "{}/build-release.sh writes $REL/{lit}, which no component of {} lists: add it (or a pattern) to the \"files\" of the component it belongs to",
                    p.dist_dir(),
                    p.list_path()
                ));
            }
        }
    }
    errs.into_iter().collect()
}

/// Every crate has a license and license texts; an override names an existing crate and files.
pub fn crates(
    repo: &Path,
    p: Platform,
    list: &List,
    crates: &BTreeMap<String, Crate>,
) -> Vec<String> {
    let mut errs = Vec::new();
    for c in crates.values() {
        if c.license.as_deref().unwrap_or("").trim().is_empty() {
            errs.push(format!(
                "Rust crate {} {} has no license field: add {{\"name\": {:?}, \"version\": {:?}, \"license\": \"<SPDX>\", \"reason\": \"…\"}} to \"rust_overrides\" in {}",
                c.name, c.version, c.name, c.version, p.list_path()
            ));
        }
        if c.texts.is_empty() {
            errs.push(format!(
                "Rust crate {} {} ships no license text (in {}): put its license text into hydo/dist/licenses/rust-crates/{}-{}/ and add {{\"name\": {:?}, \"version\": {:?}, \"texts\": [\"hydo/dist/licenses/rust-crates/{}-{}/LICENSE\"], \"reason\": \"…\"}} to \"rust_overrides\" in {}",
                c.name, c.version, c.dir.display(), c.name, c.version, c.name, c.version, c.name, c.version, p.list_path()
            ));
        }
        if c.texts_from_repo {
            for t in &c.texts {
                if !repo.join(t).is_file() {
                    errs.push(format!(
                        "{}: rust_overrides text {t} for {} does not exist",
                        p.list_path(),
                        c.key()
                    ));
                }
            }
        }
        if c.source.starts_with("registry+") && c.checksum.is_none() {
            errs.push(format!(
                "Rust crate {}: no checksum in its Cargo.lock",
                c.key()
            ));
        }
    }
    for o in &list.rust_overrides {
        if !crates.contains_key(&format!("{} {}", o.name, o.version)) {
            errs.push(format!(
                "{}: rust_overrides names {} {}, which no shipped program uses any more: remove the entry",
                p.list_path(),
                o.name,
                o.version
            ));
        }
    }
    errs
}

/// All checks that need no release tree.
pub fn all(repo: &Path, p: Platform) -> Result<Vec<String>, String> {
    let list = List::load(repo, p)?;
    let mut e = components(repo, p, &list);
    e.extend(pins(repo, p, &list));
    e.extend(script(repo, p, &list));
    let cr = crate::rust::crates(repo, &list)?;
    e.extend(crates(repo, p, &list, &cr));
    Ok(e)
}
