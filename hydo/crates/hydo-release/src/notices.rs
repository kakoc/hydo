//! The release's `LICENSE`, `licenses/` and `THIRD-PARTY.md`, and the check that every file of a
//! release tree belongs to a component.

use crate::model::{text_release_path, List, Platform};
use crate::rust::Crate;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The files (and symbolic links) of a tree, relative and sorted.
pub fn walk(root: &Path) -> Vec<String> {
    fn go(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                go(root, &p, out);
            } else if let Ok(r) = p.strip_prefix(root) {
                out.push(r.to_string_lossy().into_owned());
            }
        }
    }
    let mut v = Vec::new();
    go(root, root, &mut v);
    v.sort();
    v
}

/// The store path (`/nix/store/<hash>-<name>`) a path inside a store tree lies in.
fn store_path_of(p: &str) -> Option<String> {
    let i = p.find("nix/store/")?;
    let rest = &p[i + 10..];
    let name = rest.split('/').next()?;
    (!name.is_empty()).then(|| format!("/nix/store/{name}"))
}

/// Every file of the release tree `rel` belongs to a component, and every Nix store path in it
/// (store trees, and the closure lists the release ships) to a component's `nix` entry.
pub fn check_tree(p: Platform, list: &List, rel: &Path) -> Vec<String> {
    let lp = p.list_path();
    let mut errs = Vec::new();
    let known = list.store_paths();
    let mut store: BTreeSet<String> = BTreeSet::new();
    for f in walk(rel) {
        if !list.covers(&f) {
            errs.push(format!(
                "release file {f} belongs to no component: add {f:?} (or a pattern) to the \"files\" of its component in {lp}, or add a component for it"
            ));
        }
        if let Some(s) = store_path_of(&f) {
            store.insert(s);
        }
    }
    for closure in [
        "guest/store-closures.txt",
        "guest/tc-closure.txt",
        "guest/netns-closure.txt",
    ] {
        if let Ok(text) = fs::read_to_string(rel.join(closure)) {
            for w in text.split_whitespace() {
                if let Some(s) = store_path_of(w) {
                    store.insert(s);
                }
            }
        }
    }
    for s in store {
        if !known.contains(s.as_str()) {
            errs.push(format!(
                "the release contains the store path {s}, which no component's \"nix\" entry lists: add it to the \"out\" of the component it is (or add a component) in {lp}"
            ));
        }
    }
    errs
}

fn code_list(v: &[String]) -> String {
    v.iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The name of the sources archive that accompanies a release.
pub fn sources_name(p: Platform, version: &str) -> String {
    format!("hydo-{}-{version}-sources.tar.gz", p.name())
}

/// THIRD-PARTY.md: one table of components, one of Rust crates, and where their sources are.
pub fn third_party_md(
    p: Platform,
    list: &List,
    crates: &BTreeMap<String, Crate>,
    version: &str,
) -> String {
    let src = sources_name(p, version);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# Third-party software in hydo {version} for {}\n",
        p.title()
    );
    let _ = writeln!(
        s,
        "hydo is licensed under the Apache License 2.0 (`LICENSE`). This release also contains the \
         third-party software listed below, each under its own license; the full license texts \
         are in `licenses/`, below the paths the tables name.\n"
    );
    let _ = writeln!(
        s,
        "**Corresponding source.** The archive `{src}` accompanies this release: it is published \
         next to this release's tarball, with the same version, and its sha256 is listed with \
         the tarball's. It contains the corresponding source of every component below that is \
         not hydo's own: the exact upstream source archives (under `upstream/`, each pinned by \
         sha256 in `third-party.json`) of every GPL-, LGPL- and other copyleft-licensed \
         component and of the other bundled C programs and libraries, the patches and \
         configurations applied to them, and the build recipes (the nixpkgs tree that built the \
         Nix-built components, and hydo's own scripts under `hydo/`). Rust crates are \
         distributed in source form by crates.io at the address the crate table gives.\n"
    );
    let _ = writeln!(s, "## Components\n");
    let _ = writeln!(
        s,
        "| Component | Version | License | Files in this release | Source |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|");
    for c in &list.components {
        let mut srcs: Vec<String> = c
            .sources
            .iter()
            .map(|x| match (&x.file, x.url.first()) {
                (Some(f), Some(u)) => format!("`upstream/{f}` ({u})"),
                (Some(f), None) => format!("`upstream/{f}`"),
                (None, Some(u)) => u.clone(),
                (None, None) => String::new(),
            })
            .collect();
        if !c.repo.is_empty() {
            srcs.push(format!(
                "{} (`hydo/` in the sources archive)",
                code_list(&c.repo)
            ));
        }
        if !c.nix.is_empty() {
            let r: Vec<String> = c
                .recipe_files(list)
                .iter()
                .map(|f| format!("`upstream/{f}`"))
                .collect();
            srcs.push(format!("recipe: {}", r.join(", ")));
        }
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} |",
            c.name,
            c.version_for(version),
            c.license,
            code_list(&c.files),
            srcs.join("; ")
        );
    }
    let _ = writeln!(s, "\n## Notes and license texts\n");
    for c in &list.components {
        let texts: Vec<String> = c.texts.iter().map(|t| text_release_path(t)).collect();
        let _ = writeln!(
            s,
            "- **{}** ({}): license texts {}.",
            c.name,
            c.license,
            code_list(&texts)
        );
        if let Some(n) = &c.note {
            let _ = writeln!(s, "  {n}");
        }
        for n in &c.nix {
            let mut from = String::new();
            if let Some(a) = &n.attr {
                from = match &n.nixpkgs {
                    Some(t) => format!(" (nixpkgs `{a}` of `upstream/{t}`)"),
                    None => format!(" (nixpkgs `{a}`)"),
                };
            }
            if let Some(d) = &n.drv {
                from.push_str(&format!(" by the derivation `{d}`"));
            }
            if let Some(r) = &n.recipe {
                from.push_str(&format!(
                    ", whose whole derivation closure is in `upstream/{r}`"
                ));
            }
            let _ = writeln!(
                s,
                "  Built by Nix for {}{from}: {}.{}",
                n.system,
                code_list(&n.out),
                n.note.as_ref().map(|x| format!(" {x}")).unwrap_or_default()
            );
        }
    }
    let _ = writeln!(
        s,
        "\nThe Nix-built components come from nixpkgs revision `{}` (`upstream/{}`, {}), whose \
         recipes (`pkgs/…`) and patches built them{}.\n",
        list.nixpkgs.rev,
        list.nixpkgs.archive,
        list.nixpkgs.url,
        if list
            .components
            .iter()
            .any(|c| c.nix.iter().any(|n| n.nixpkgs.is_some()))
        {
            ", except where a component's note names another nixpkgs tree"
        } else {
            ""
        }
    );
    let _ = writeln!(s, "## Rust crates\n");
    let _ = writeln!(
        s,
        "These crates are compiled into hydo's programs. Each crate's own license files are in \
         `licenses/rust-crates/<crate>-<version>/`.\n"
    );
    let _ = writeln!(s, "| Crate | Version | License | Compiled into | Source |");
    let _ = writeln!(s, "|---|---|---|---|---|");
    for c in crates.values() {
        let used: Vec<String> = c.used_by.iter().cloned().collect();
        let src = match &c.checksum {
            Some(h) => format!("{} (sha256 `{h}`)", c.crates_io_url()),
            None => c.crates_io_url(),
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} |",
            c.name,
            c.version,
            c.license.as_deref().unwrap_or(""),
            code_list(&used),
            src
        );
    }
    s
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Every license text file the release carries: (release path, source path).
pub fn text_files(
    repo: &Path,
    list: &List,
    crates: &BTreeMap<String, Crate>,
) -> BTreeMap<String, std::path::PathBuf> {
    let mut m = BTreeMap::new();
    for c in &list.components {
        for t in crate::model::expand_texts(repo, &c.texts) {
            m.insert(text_release_path(&t), repo.join(&t));
        }
    }
    for c in crates.values() {
        for t in &c.texts {
            if c.texts_from_repo {
                m.insert(text_release_path(t), repo.join(t));
            } else {
                m.insert(
                    format!("licenses/rust-crates/{}-{}/{t}", c.name, c.version),
                    c.dir.join(t),
                );
            }
        }
    }
    m
}

/// Write LICENSE, licenses/ and THIRD-PARTY.md into the release tree.
pub fn write(
    repo: &Path,
    p: Platform,
    list: &List,
    crates: &BTreeMap<String, Crate>,
    rel: &Path,
    version: &str,
) -> Result<usize, String> {
    let files = text_files(repo, list, crates);
    for (dst, src) in &files {
        let bytes = fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
        write_file(&rel.join(dst), &bytes)?;
    }
    let md = third_party_md(p, list, crates, version);
    write_file(&rel.join("THIRD-PARTY.md"), md.as_bytes())?;
    Ok(files.len() + 1)
}
