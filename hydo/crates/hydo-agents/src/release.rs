//! The skill's own releases (Milestone 170): the onboarding skill is released separately from
//! hydo, as the GitHub release `skill-v<version>` with two assets, `hydo-skill-<version>.tar.gz`
//! (the Claude Code plugin folder as `hydo/`, with the skill in `hydo/skills/onboard/`; Milestone
//! 228) and `SHA256SUMS`. [`pack`] makes the archive reproducibly (the same bytes from the same
//! folder on any machine: sorted entries, every time 0, owner 0:0, modes 0644 or 0755, gzip
//! without a name or time); [`unpack`] takes one apart for `hydo skill update`, refusing anything
//! but plain files and directories below `hydo/`.

use crate::{is_hydo_skill_md, skill_version, Fail, PLUGIN, SKILL, SKILL_IN_PLUGIN};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

/// The tag of a skill release: `skill-v<version>`.
pub const TAG_PREFIX: &str = "skill-v";

/// The archive of a skill release.
pub fn archive_name(version: &str) -> String {
    format!("hydo-skill-{version}.tar.gz")
}

/// The most an archive may unpack to (bytes, entries): a skill is a few text files.
const MAX_BYTES: u64 = 16 << 20;
const MAX_ENTRIES: usize = 2000;

fn skipped(n: &str) -> bool {
    n == "__pycache__" || n == ".DS_Store" || n.starts_with("._")
}

/// Every file and directory below `dir` (sorted, symbolic links refused), relative paths.
fn tree(dir: &Path, rel: &str, out: &mut Vec<(String, bool, u32)>) -> Result<(), Fail> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| Fail(format!("{}: {e}", dir.display())))?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
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
        let md =
            std::fs::symlink_metadata(&p).map_err(|e| Fail(format!("{}: {e}", p.display())))?;
        if md.file_type().is_symlink() {
            return Err(Fail(format!(
                "{}: a symbolic link; a skill release holds plain files only",
                p.display()
            )));
        }
        if md.is_dir() {
            out.push((r.clone(), true, 0o755));
            tree(&p, &r, out)?;
        } else {
            let mode = if md.permissions().mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            out.push((r, false, mode));
        }
    }
    Ok(())
}

/// The release archive of the plugin folder `dir`, and the skill's version (its SKILL.md's
/// `metadata.version`).
pub fn pack(dir: &Path) -> Result<(Vec<u8>, String), Fail> {
    let md_path = dir.join(SKILL_IN_PLUGIN).join("SKILL.md");
    let md = std::fs::read_to_string(&md_path)
        .map_err(|e| Fail(format!("{}: {e}", md_path.display())))?;
    let version =
        skill_version(&md).ok_or_else(|| Fail("SKILL.md has no metadata.version".into()))?;
    let mut entries = Vec::new();
    tree(dir, "", &mut entries)?;
    let mut b = tar::Builder::new(Vec::new());
    b.mode(tar::HeaderMode::Deterministic);
    let mut add = |path: String, dir: bool, mode: u32, data: &[u8]| -> Result<(), Fail> {
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(if dir {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        h.set_mode(mode);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        h.set_size(data.len() as u64);
        b.append_data(&mut h, &path, data)
            .map_err(|e| Fail(format!("{path}: {e}")))
    };
    add(format!("{PLUGIN}/"), true, 0o755, &[])?;
    for (rel, is_dir, mode) in entries {
        if is_dir {
            add(format!("{PLUGIN}/{rel}/"), true, mode, &[])?;
        } else {
            let p = dir.join(&rel);
            let data = std::fs::read(&p).map_err(|e| Fail(format!("{}: {e}", p.display())))?;
            add(format!("{PLUGIN}/{rel}"), false, mode, &data)?;
        }
    }
    let tar = b
        .into_inner()
        .map_err(|e| Fail(format!("the archive: {e}")))?;
    let mut gz = flate2::GzBuilder::new()
        .mtime(0)
        .operating_system(3)
        .write(Vec::new(), flate2::Compression::best());
    gz.write_all(&tar)
        .and_then(|_| gz.try_finish())
        .map_err(|e| Fail(format!("the archive: {e}")))?;
    let out = gz.finish().map_err(|e| Fail(format!("the archive: {e}")))?;
    Ok((out, version))
}

/// The path of an entry below `hydo/` (None: the top directory itself). An entry that
/// is not a plain relative path below it is refused.
fn below_top(p: &Path) -> Result<Option<PathBuf>, Fail> {
    let mut parts = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_os_string()),
            _ => {
                return Err(Fail(format!(
                    "the archive holds {} (not a plain relative path); refused",
                    p.display()
                )))
            }
        }
    }
    if parts.first().map(|s| s.as_os_str()) != Some(std::ffi::OsStr::new(PLUGIN)) {
        return Err(Fail(format!(
            "the archive holds {} outside {PLUGIN}/; refused",
            p.display()
        )));
    }
    Ok((parts.len() > 1).then(|| parts[1..].iter().collect()))
}

/// Unpack a release archive into `dest` (made here; it must not exist): the folder's contents
/// without the `hydo/` top directory. Returns the skill's version. Only plain files and
/// directories are accepted; links, devices, absolute paths and `..` are refused before anything
/// is written outside `dest`, and on any refusal `dest` is deleted again.
pub fn unpack(archive: &[u8], dest: &Path) -> Result<String, Fail> {
    if dest.exists() {
        return Err(Fail(format!("{} exists", dest.display())));
    }
    std::fs::create_dir_all(dest).map_err(|e| Fail(format!("{}: {e}", dest.display())))?;
    let r = unpack_into(archive, dest);
    if r.is_err() {
        let _ = std::fs::remove_dir_all(dest);
    }
    r
}

fn unpack_into(archive: &[u8], dest: &Path) -> Result<String, Fail> {
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let (mut total, mut n) = (0u64, 0usize);
    let entries = a.entries().map_err(|e| Fail(format!("the archive: {e}")))?;
    for e in entries {
        let mut e = e.map_err(|e| Fail(format!("the archive: {e}")))?;
        n += 1;
        if n > MAX_ENTRIES {
            return Err(Fail(format!(
                "the archive has more than {MAX_ENTRIES} entries; refused"
            )));
        }
        let path = e
            .path()
            .map_err(|e| Fail(format!("the archive: {e}")))?
            .into_owned();
        let rel = below_top(&path)?;
        let kind = e.header().entry_type();
        let Some(rel) = rel else {
            if kind.is_dir() {
                continue;
            }
            return Err(Fail(format!("the archive's {PLUGIN} is not a directory")));
        };
        let t = dest.join(&rel);
        if kind.is_dir() {
            std::fs::create_dir_all(&t).map_err(|e| Fail(format!("{}: {e}", t.display())))?;
        } else if kind.is_file() {
            total += e.size();
            if total > MAX_BYTES {
                return Err(Fail(format!(
                    "the archive unpacks to more than {MAX_BYTES} bytes; refused"
                )));
            }
            let mut data = Vec::new();
            e.read_to_end(&mut data)
                .map_err(|e| Fail(format!("the archive: {e}")))?;
            if let Some(p) = t.parent() {
                std::fs::create_dir_all(p).map_err(|e| Fail(format!("{}: {e}", p.display())))?;
            }
            std::fs::write(&t, &data).map_err(|e| Fail(format!("{}: {e}", t.display())))?;
            let mode = if e.header().mode().unwrap_or(0) & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            std::fs::set_permissions(&t, std::fs::Permissions::from_mode(mode))
                .map_err(|e| Fail(format!("{}: {e}", t.display())))?;
        } else {
            return Err(Fail(format!(
                "the archive holds {} of type {:?} (only files and directories); refused",
                path.display(),
                kind
            )));
        }
    }
    let md =
        std::fs::read_to_string(dest.join(SKILL_IN_PLUGIN).join("SKILL.md")).map_err(|_| {
            Fail(format!(
                "the archive has no {PLUGIN}/{SKILL_IN_PLUGIN}/SKILL.md; refused"
            ))
        })?;
    if !is_hydo_skill_md(&md) {
        return Err(Fail(format!(
            "the archive's SKILL.md is not hydo's {SKILL} skill; refused"
        )));
    }
    if !dest.join(".claude-plugin/plugin.json").is_file() {
        return Err(Fail(format!(
            "the archive has no {PLUGIN}/.claude-plugin/plugin.json; refused"
        )));
    }
    skill_version(&md).ok_or_else(|| Fail("the archive's SKILL.md has no version; refused".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-skill-rel-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn md(version: &str) -> String {
        format!("---\nname: {SKILL}\ndescription: x\nmetadata:\n  version: \"{version}\"\n  hydo-min: \"0.1.0\"\n---\n# x\n")
    }

    /// A plugin folder `d/hydo` with the skill of `version`.
    fn plugin_dir(d: &Path, version: &str) -> PathBuf {
        let p = d.join(PLUGIN);
        let s = p.join(SKILL_IN_PLUGIN);
        std::fs::create_dir_all(s.join("references")).unwrap();
        std::fs::create_dir_all(p.join(".claude-plugin")).unwrap();
        std::fs::write(
            p.join(".claude-plugin/plugin.json"),
            "{\"name\": \"hydo\"}\n",
        )
        .unwrap();
        std::fs::write(s.join("SKILL.md"), md(version)).unwrap();
        std::fs::write(s.join("references/a.md"), "a\n").unwrap();
        p
    }

    #[test]
    fn pack_is_reproducible_and_unpacks() {
        let d = scratch("pack");
        let p = plugin_dir(&d, "1.2.3");
        let (a, v) = pack(&p).unwrap();
        assert_eq!(v, "1.2.3");
        // other times and modes on disk give the same bytes
        std::fs::set_permissions(
            p.join(SKILL_IN_PLUGIN).join("references/a.md"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let (b, _) = pack(&p).unwrap();
        assert_eq!(a, b);
        let out = d.join("out");
        assert_eq!(unpack(&a, &out).unwrap(), "1.2.3");
        assert_eq!(
            std::fs::read(out.join(SKILL_IN_PLUGIN).join("references/a.md")).unwrap(),
            b"a\n"
        );
        assert!(out.join(".claude-plugin/plugin.json").is_file());
        assert!(
            unpack(&a, &out).is_err(),
            "an existing destination is refused"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A tar.gz with hand-made entries (path, type, data).
    fn raw(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (p, t, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(*t);
            h.set_mode(0o644);
            h.set_size(data.len() as u64);
            if *t == tar::EntryType::Symlink {
                h.set_link_name("/etc/passwd").unwrap();
            }
            // write the name bytes directly, so that `..` and absolute names get through
            let name = &mut h.as_old_mut().name;
            name[..p.len()].copy_from_slice(p.as_bytes());
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn bad_archives_are_refused_and_leave_nothing() {
        let d = scratch("bad");
        let md = md("1.0.0");
        let f = tar::EntryType::Regular;
        let sk = "hydo/skills/onboard/SKILL.md";
        let pj = "hydo/.claude-plugin/plugin.json";
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("dotdot", raw(&[("hydo/../evil", f, b"x")])),
            ("absolute", raw(&[("/tmp/evil", f, b"x")])),
            (
                "outside",
                raw(&[("other/skills/onboard/SKILL.md", f, md.as_bytes())]),
            ),
            (
                "symlink",
                raw(&[
                    (sk, f, md.as_bytes()),
                    (pj, f, b"{}"),
                    ("hydo/x", tar::EntryType::Symlink, b""),
                ]),
            ),
            ("no-skill", raw(&[("hydo/README.md", f, b"x")])),
            (
                "other-skill",
                raw(&[(sk, f, b"---\nname: other\n---\n"), (pj, f, b"{}")]),
            ),
            ("no-manifest", raw(&[(sk, f, md.as_bytes())])),
            ("not-gzip", b"plain text".to_vec()),
        ];
        for (tag, a) in cases {
            let out = d.join(tag);
            assert!(unpack(&a, &out).is_err(), "{tag}");
            assert!(!out.exists(), "{tag}: the destination is deleted again");
        }
        assert!(!d.parent().unwrap().join("evil").exists());
        let ok = raw(&[(sk, f, md.as_bytes()), (pj, f, b"{}")]);
        assert_eq!(unpack(&ok, &d.join("ok")).unwrap(), "1.0.0");
        let _ = std::fs::remove_dir_all(&d);
    }
}
