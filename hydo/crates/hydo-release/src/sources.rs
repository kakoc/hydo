//! The sources archive `hydo-<platform>-<version>-sources.tar.gz`: the pinned upstream source
//! archives from a sources cache (each checked by sha256), the repository files the components
//! name, the component list and the notices. One GNU tar, sorted, every entry owned by 0:0 with a
//! fixed time and mode 0644 (0755 for directories and executable repository files), compressed
//! by flate2's pure-Rust deflate with no name or time: the same bytes from the same inputs on any
//! machine.

use crate::model::{List, Platform};
use crate::notices::{sources_name, third_party_md};
use crate::rust::Crate;
use flate2::write::GzEncoder;
use flate2::{Compression, GzBuilder};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The entries' modification time (the Linux tarball's, 2020-09-13).
pub const MTIME: u64 = 1_600_000_000;

enum Entry {
    File(PathBuf, u32),
    Bytes(Vec<u8>),
}

/// Every included source file of the list (name -> sha256), the nixpkgs archive with them.
pub fn included(list: &List) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for c in &list.components {
        for (f, h) in c.included_sources() {
            m.insert(f.to_string(), h.to_string());
        }
    }
    m.insert(list.nixpkgs.archive.clone(), list.nixpkgs.sha256.clone());
    m
}

/// The lines fetch-sources.sh reads: `<file>\t<sha256>\t<from>\t<first url>`.
pub fn fetch_list(list: &List, cache: Option<&Path>) -> String {
    let have = |f: &str, h: &str| {
        cache.is_some_and(|c| crate::sha256_file(&c.join(f)).is_ok_and(|g| g == h))
    };
    let mut seen = BTreeSet::new();
    let mut s = String::new();
    for c in &list.components {
        for x in &c.sources {
            if let (Some(f), Some(h)) = (&x.file, &x.sha256) {
                if seen.insert(f.clone()) && !have(f, h) {
                    let _ = writeln!(
                        s,
                        "{f}\t{h}\t{}\t{}",
                        x.from.as_deref().unwrap_or("url"),
                        x.url.first().map(String::as_str).unwrap_or("-")
                    );
                }
            }
        }
    }
    let n = &list.nixpkgs;
    if !have(&n.archive, &n.sha256) {
        let _ = writeln!(
            s,
            "{}\t{}\tnix-pack:{}\t{}",
            n.archive, n.sha256, n.store_path, n.url
        );
    }
    s
}

/// Check a sources cache: every included file present with its sha256. Returns the problems.
pub fn check_cache(list: &List, cache: &Path) -> Vec<String> {
    let mut errs = Vec::new();
    for (f, h) in included(list) {
        let p = cache.join(&f);
        match crate::sha256_file(&p) {
            Ok(got) if got == h => {}
            Ok(got) => errs.push(format!(
                "sources cache {}: {f} has sha256 {got}, the component list pins {h}; fetch it again (docs/RELEASING.md)",
                cache.display()
            )),
            Err(_) => errs.push(format!(
                "sources cache {}: {f} is missing; fetch it with hydo/dist/fetch-sources.sh (docs/RELEASING.md)",
                cache.display()
            )),
        }
    }
    errs
}

fn readme(p: Platform, list: &List, version: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# Corresponding source of hydo {version} for {}\n",
        p.title()
    );
    let _ = writeln!(
        s,
        "This archive accompanies the release tarball `hydo-{}-{version}.tar.gz`. It holds the \
         corresponding source of the third-party software that release contains (THIRD-PARTY.md \
         lists every component, its license and the files it is in):\n",
        p.name()
    );
    let _ = writeln!(s, "- `upstream/`: the exact upstream source archives and patch files, each pinned by sha256 in `third-party.json`;");
    let _ = writeln!(
        s,
        "- `upstream/{}`: the nixpkgs tree (revision `{}`) whose recipes, patches and kernel configuration generator built every component that `third-party.json` says was built by Nix;",
        list.nixpkgs.archive, list.nixpkgs.rev
    );
    let _ = writeln!(s, "- `hydo/`: hydo's own files that built or configured them (our kernel patches and configurations, the build scripts, and the sources of the static helper programs);");
    let _ = writeln!(s, "- `third-party.json`: the component list these files and THIRD-PARTY.md are generated from.\n");
    let _ = writeln!(s, "## By component\n");
    for c in &list.components {
        let inc: Vec<String> = c
            .included_sources()
            .map(|(f, _)| format!("`upstream/{f}`"))
            .collect();
        let repo: Vec<String> = c.repo.iter().map(|r| format!("`hydo/{r}`")).collect();
        let refs: Vec<String> = c
            .sources
            .iter()
            .filter(|x| x.file.is_none())
            .flat_map(|x| x.url.iter().cloned())
            .collect();
        let mut parts = Vec::new();
        if !inc.is_empty() {
            parts.push(inc.join(", "));
        }
        if !repo.is_empty() {
            parts.push(repo.join(", "));
        }
        if !c.nix.is_empty() {
            parts.push(format!("recipe in `upstream/{}`", list.nixpkgs.archive));
        }
        if !refs.is_empty() {
            parts.push(format!("source published at {}", refs.join(", ")));
        }
        let _ = writeln!(
            s,
            "- {} {} ({}): {}",
            c.name,
            c.version_for(version),
            c.license,
            parts.join("; ")
        );
    }
    let _ = writeln!(
        s,
        "\nHow the release was built from these files: `hydo/docs/RELEASING.md` and \
         `hydo/docs/BUILD-PINS.md` (where they are included), and each component's note in \
         THIRD-PARTY.md."
    );
    s
}

/// Build the sources archive at `out`. Returns its sha256.
pub fn write(
    repo: &Path,
    p: Platform,
    list: &List,
    crates: &BTreeMap<String, Crate>,
    cache: &Path,
    version: &str,
    out: &Path,
) -> Result<String, String> {
    let errs = check_cache(list, cache);
    if !errs.is_empty() {
        return Err(errs.join("\n"));
    }
    let top = sources_name(p, version)
        .trim_end_matches(".tar.gz")
        .to_string();
    let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
    entries.insert(
        format!("{top}/README.md"),
        Entry::Bytes(readme(p, list, version).into_bytes()),
    );
    entries.insert(
        format!("{top}/THIRD-PARTY.md"),
        Entry::Bytes(third_party_md(p, list, crates, version).into_bytes()),
    );
    entries.insert(
        format!("{top}/third-party.json"),
        Entry::File(repo.join(p.list_path()), 0o644),
    );
    for f in included(list).keys() {
        entries.insert(
            format!("{top}/upstream/{f}"),
            Entry::File(cache.join(f), 0o644),
        );
    }
    for c in &list.components {
        for r in &c.repo {
            let src = repo.join(r);
            let exec = fs::metadata(&src)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .map_err(|e| format!("{}: {e}", src.display()))?;
            entries.insert(
                format!("{top}/hydo/{r}"),
                Entry::File(src, if exec { 0o755 } else { 0o644 }),
            );
        }
    }
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for k in entries.keys() {
        let mut d = k.as_str();
        while let Some(i) = d.rfind('/') {
            d = &d[..i];
            dirs.insert(format!("{d}/"));
        }
    }
    let mut all: Vec<(String, Option<&Entry>)> = dirs.into_iter().map(|d| (d, None)).collect();
    all.extend(entries.iter().map(|(k, v)| (k.clone(), Some(v))));
    all.sort_by(|a, b| a.0.cmp(&b.0));

    let tmp = out.with_extension("gz.part");
    let f = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let gz: GzEncoder<BufWriter<fs::File>> = GzBuilder::new()
        .mtime(0)
        .write(BufWriter::with_capacity(1 << 20, f), Compression::new(6));
    let mut tb = tar::Builder::new(gz);
    for (name, e) in all {
        let mut h = tar::Header::new_gnu();
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(MTIME);
        let _ = h.set_username("root");
        let _ = h.set_groupname("root");
        let res = match e {
            None => {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_mode(0o755);
                h.set_size(0);
                tb.append_data(&mut h, &name, std::io::empty())
            }
            Some(Entry::Bytes(b)) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(0o644);
                h.set_size(b.len() as u64);
                tb.append_data(&mut h, &name, b.as_slice())
            }
            Some(Entry::File(path, mode)) => {
                let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                let len = file
                    .metadata()
                    .map_err(|e| format!("{}: {e}", path.display()))?
                    .len();
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(*mode);
                h.set_size(len);
                tb.append_data(
                    &mut h,
                    &name,
                    std::io::BufReader::with_capacity(1 << 20, file),
                )
            }
        };
        res.map_err(|e| format!("{name}: {e}"))?;
    }
    let gz = tb.into_inner().map_err(|e| format!("tar: {e}"))?;
    let mut w = gz.finish().map_err(|e| format!("gzip: {e}"))?;
    w.flush().map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(w);
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644)).map_err(|e| e.to_string())?;
    fs::rename(&tmp, out).map_err(|e| format!("{}: {e}", out.display()))?;
    crate::sha256_file(out).map_err(|e| e.to_string())
}
