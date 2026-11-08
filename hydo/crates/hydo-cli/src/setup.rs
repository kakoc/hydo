//! `hydo setup [--check | --verify] [--add-to-path]`: make an unpacked release usable, and say
//! what is wrong with one (M154).
//!
//! Until M154 the two `install.sh` scripts did this in shell. It lives in `hydo` now, so every
//! way of installing hydo (the release's `install.sh`, and later channels such as a package
//! manager or a download into a cache) runs the same steps, and `hydo doctor` reports what
//! `hydo setup --check` would fix. `install.sh` only copies the unpacked files into the prefix and
//! runs `<prefix>/bin/hydo setup` (on Linux then `hydo module install`, src/module.rs).
//!
//! What setup checks, on the release root (`<root>/bin/hydo`):
//! - every file the release's `SHA256SUMS` lists, by sha256 (missing or changed = damaged);
//! - the pinned assets (`hydo/dist/<platform>/assets.sha256`, which the release scripts fetch and
//!   check) and the guest programs built from the tree (`guest-programs.sha256` next to it), both
//!   built into this binary: `SHA256SUMS` must list each with its pinned sha256, so a release
//!   tree and a `bin/hydo` of different releases are refused;
//! - what setup itself made, against its record `<root>/.hydo-setup`: on Linux the guest store
//!   tree unpacked from the verified `guest/store.tar` (every entry's kind, mode, sha256 or link
//!   target; the modes go into images), on the Mac the signed `bin/hydo-hvm`.
//!
//! The steps it takes (each only when needed, so a second run does nothing):
//! - make `bin/*` and `libexec/*` executable;
//! - Linux: unpack `guest/store.tar` into `guest/store` (the iproute2 and iptables closures with
//!   their read-only modes; `hydo build` copies them into images) and delete the tar;
//! - Mac: remove the quarantine attribute a download leaves, and sign `bin/hydo-hvm` with the
//!   hypervisor entitlement (`hv/hvm/hv.entitlements`; an ad-hoc signature is enough) unless it
//!   already carries a valid signature with it: a release built since Milestone 172 ships its
//!   programs signed with a Developer ID (docs/design/signing.md), and setup leaves them as they
//!   are; an unsigned build (cargo's ad-hoc linker signature has no entitlement) is signed here;
//! - write the record.
//!
//! A damaged file is restored from the release's archive of the same version (Milestone 162,
//! docs/design/install-cache.md): [`obtain_assets`], between the inspection and the steps, asks
//! the install cache ([`Assets`], `src/cache.rs`) for the files that repair each damage
//! ([`Damage::fix`]), from its store or fetched from the release (not with `--offline`); each
//! restored file must have the sha256 the release's `SHA256SUMS` lists, and the inspection that
//! follows checks everything again. What no restored file repairs (a `bin/hydo` of another
//! release, a record that cannot be read), or an archive that cannot be had, is refused as
//! before: unpack the release again.
//!
//! `--get [VERSION]` installs a version from its archive into the install cache
//! (`$HYDO_HOME/versions/<VERSION>`, set up with these same steps) and points
//! `$HYDO_HOME/bin/hydo` at it; `--archive FILE` puts a downloaded archive into the cache
//! (`src/cache.rs`).
//!
//! Exit status: 0 done (or, with `--check`, nothing to do), 1 a file is damaged (and cannot be
//! restored) or this machine cannot run the release, 2 usage, 3 (`--check` only) steps are
//! pending that `hydo setup` takes (restoring files included). `--verify` checks the files only
//! (pending steps and restorable files are not reported), so an installer can check an unpacked
//! release before it replaces an earlier installation with it.

use crate::ctx::{die_rc, Ctx, Platform};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// The four pin lists are hydo/dist/<platform>/*.sha256, read through the links in this crate's
// pins/ folder, so that the crate published to crates.io carries them (cargo copies a linked
// file's content into the package; docs/PUBLISHING.md).

/// The Linux release's pinned assets, as `assets.sha256` lists them (paths of the asset tree).
pub const LINUX_ASSETS: &str = include_str!("../pins/linux/assets.sha256");
/// The Mac release's pinned assets.
pub const MAC_ASSETS: &str = include_str!("../pins/mac/assets.sha256");
/// The Linux release's guest programs built from the tree, by release path.
pub const LINUX_GUEST: &str = include_str!("../pins/linux/guest-programs.sha256");
/// The Mac release's guest programs built from the tree, by release path.
pub const MAC_GUEST: &str = include_str!("../pins/mac/guest-programs.sha256");

/// setup's record, at the release root.
pub const RECORD: &str = ".hydo-setup";
const SUMS: &str = "SHA256SUMS";
const STORE_TAR: &str = "guest/store.tar";
const STORE_DIR: &str = "guest/store";
const ENGINE_MAC: &str = "bin/hydo-hvm";
const ENTITLEMENTS: &str = "hv/hvm/hv.entitlements";
const QUARANTINE: &str = "com.apple.quarantine";

/// One file whose sha256 is built into this binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// The path in the release tree.
    pub path: String,
    pub sha256: String,
    /// Which list pinned it: "asset" or "guest program".
    pub list: &'static str,
}

/// `sha256sum`/`shasum` lines: `<64 hex>  <path>` (or ` *<path>`); a leading `./` is dropped,
/// empty lines and `#` comments are skipped.
pub fn parse_sums(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut v = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let (sha, rest) = l
            .split_once(' ')
            .ok_or_else(|| format!("line {}: not `<sha256>  <path>`", i + 1))?;
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("line {}: {sha:?} is not a sha256", i + 1));
        }
        let path = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*'));
        let path = path.ok_or_else(|| format!("line {}: not `<sha256>  <path>`", i + 1))?;
        let path = path.strip_prefix("./").unwrap_or(path);
        if path.is_empty() || path.starts_with('/') || path.split('/').any(|c| c == "..") {
            return Err(format!(
                "line {}: {path:?} is not a path in the release",
                i + 1
            ));
        }
        v.push((sha.to_ascii_lowercase(), path.to_string()));
    }
    Ok(v)
}

/// Where a pinned asset (a path of `hydo/dist/<platform>/assets/`) lies in the release tree, as
/// the platform's `build-release.sh` places it.
pub fn asset_release_path(p: Platform, asset: &str) -> Option<String> {
    let a = asset.strip_prefix("./").unwrap_or(asset);
    match p {
        Platform::Linux => {
            if a.starts_with("guest/") || a.starts_with("tools/") {
                Some(a.to_string())
            } else {
                a.strip_prefix("redis/")
                    .map(|f| format!("examples/redis/files/{f}"))
            }
        }
        Platform::Mac => {
            if let Some(f) = a.strip_prefix("redis/") {
                return Some(format!("examples/redis/files/{f}"));
            }
            if let Some(f) = a.strip_prefix("ledger-spin/") {
                return Some(format!("examples/ledger-spin/bin/{f}"));
            }
            // both closures go into one tree, guest/tc (the release's arm64 store)
            if let Some(f) = a.strip_prefix("tc/").or_else(|| a.strip_prefix("netns/")) {
                return Some(format!("guest/tc/{f}"));
            }
            match a {
                "Image-explore" | "Image-explore-netns" => Some(format!("hv/hvm/explore/out/{a}")),
                "busybox"
                | "mke2fs"
                | "hydo_fc"
                | "busybox-applets.txt"
                | "tc-closure.txt"
                | "netns-closure.txt" => Some(format!("guest/{a}")),
                _ => None,
            }
        }
    }
}

/// The pins built into this binary for a platform's release (assets first, then the guest
/// programs). The lists are checked by this module's tests, so a malformed line cannot ship.
pub fn pins(p: Platform) -> Vec<Pin> {
    let (assets, guest) = match p {
        Platform::Linux => (LINUX_ASSETS, LINUX_GUEST),
        Platform::Mac => (MAC_ASSETS, MAC_GUEST),
    };
    let mut v: Vec<Pin> = Vec::new();
    for (sha, a) in parse_sums(assets).unwrap_or_default() {
        if let Some(path) = asset_release_path(p, &a) {
            if !v.iter().any(|x| x.path == path) {
                v.push(Pin {
                    path,
                    sha256: sha,
                    list: "asset",
                });
            }
        }
    }
    for (sha, path) in parse_sums(guest).unwrap_or_default() {
        v.push(Pin {
            path,
            sha256: sha,
            list: "guest program",
        });
    }
    v
}

/// A step `hydo setup` takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Make a program executable (mode 0755).
    Executable(String),
    /// Mac: remove the quarantine attribute from these many files.
    Unquarantine(usize),
    /// Linux: unpack guest/store.tar into guest/store and delete the tar.
    UnpackStore,
    /// Mac: sign bin/hydo-hvm with the hypervisor entitlement.
    Sign,
    /// Write the record of what setup made.
    Record,
}

impl Step {
    pub fn describe(&self) -> String {
        match self {
            Step::Executable(p) => format!("make {p} executable"),
            Step::Unquarantine(n) => format!(
                "remove the quarantine attribute of a download from {n} file{}",
                if *n == 1 { "" } else { "s" }
            ),
            Step::UnpackStore => format!("unpack {STORE_TAR} into {STORE_DIR}"),
            Step::Sign => format!(
                "sign {ENGINE_MAC} on this Mac with the hypervisor entitlement ({ENTITLEMENTS})"
            ),
            Step::Record => format!("write {RECORD} (the record of what setup made)"),
        }
    }
}

/// A file that does not match the release. Setup's steps cannot fix it; restoring the file
/// `fix` names from the version's archive can (Milestone 162, [`Assets`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Damage {
    pub path: String,
    pub why: String,
    /// The release file (a path of `SHA256SUMS`) whose restoring repairs this damage; None: no
    /// restored file can (a `SHA256SUMS` or record that cannot be read, or a file pinned in this
    /// hydo that the release lists with another sha256: a `bin/hydo` of another release).
    pub fix: Option<String>,
}

impl Damage {
    pub fn line(&self) -> String {
        format!("{}: {}", self.path, self.why)
    }
}

/// What an inspection found.
#[derive(Debug, Default)]
pub struct Report {
    /// The root has a SHA256SUMS (it is an unpacked release, not a development tree).
    pub release: bool,
    /// Why this machine cannot run the release (empty when it can).
    pub unsupported: Vec<String>,
    pub damaged: Vec<Damage>,
    pub pending: Vec<Step>,
    /// How many files and store entries matched.
    pub checked: usize,
    /// How many of the matched files are pinned in this binary.
    pub pinned: usize,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.release && self.unsupported.is_empty() && self.damaged.is_empty()
    }
}

/// What setup made, kept at `<root>/.hydo-setup`: one tab-separated record per line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// Linux: the sha256 of the guest/store.tar the store tree was unpacked from.
    pub store_from: Option<String>,
    /// Linux: the store tree's listing (see [`listing`]).
    pub store: Vec<String>,
    /// Mac: (sha256 before, sha256 after) of the engine setup signed.
    pub signed: Option<(String, String)>,
}

impl Record {
    pub fn text(&self) -> String {
        let mut t = String::from(
            "# hydo setup's record (M154): what it made from this release's verified files.\n\
             # `hydo setup --check` and `hydo doctor` compare with it; do not edit it.\n\
             hydo-setup\t1\n",
        );
        if let Some(s) = &self.store_from {
            t.push_str(&format!("store-from\t{s}\n"));
            for l in &self.store {
                t.push_str(&format!("store\t{l}\n"));
            }
        }
        if let Some((a, b)) = &self.signed {
            t.push_str(&format!("signed\t{a}\t{b}\t{ENGINE_MAC}\n"));
        }
        t
    }

    pub fn parse(text: &str) -> Result<Record, String> {
        let mut r = Record::default();
        let mut version = false;
        for l in text.lines() {
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let (k, v) = l.split_once('\t').unwrap_or((l, ""));
            match k {
                "hydo-setup" if v == "1" => version = true,
                "hydo-setup" => return Err(format!("version {v} (this hydo writes 1)")),
                "store-from" => r.store_from = Some(v.to_string()),
                "store" => r.store.push(v.to_string()),
                "signed" => {
                    let f: Vec<&str> = v.split('\t').collect();
                    if f.len() != 3 || f[2] != ENGINE_MAC {
                        return Err(format!("a malformed line: {l}"));
                    }
                    r.signed = Some((f[0].to_string(), f[1].to_string()));
                }
                _ => return Err(format!("an unknown line: {l}")),
            }
        }
        if !version {
            return Err("no version line".into());
        }
        Ok(r)
    }
}

/// The calls setup makes outside its files, so the tests can stand in for the Mac's signing.
pub trait Host {
    /// Why this machine cannot run a release of this platform (empty: it can).
    fn unsupported(&self, p: Platform) -> Vec<String>;
    /// The file carries the quarantine attribute.
    fn quarantined(&self, p: &Path) -> bool;
    fn unquarantine(&self, p: &Path) -> Result<(), String>;
    /// Sign the engine with the entitlements file.
    fn sign(&self, bin: &Path, entitlements: &Path) -> Result<(), String>;
    /// The engine's signature is valid and carries the hypervisor entitlement.
    fn signed_ok(&self, bin: &Path) -> bool;
    /// Unpack a tar into a directory, keeping its modes.
    fn untar(&self, tar: &Path, into: &Path) -> Result<(), String>;
}

/// The real machine.
pub struct System;

fn run(cmd: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let o = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {cmd}: {e}"))?;
    let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
    t.push_str(&String::from_utf8_lossy(&o.stderr));
    if o.status.success() {
        Ok(t)
    } else {
        Err(format!("{cmd} failed: {}", t.trim()))
    }
}

impl Host for System {
    fn unsupported(&self, p: Platform) -> Vec<String> {
        let (sys, mach) = crate::module::uname_sys_machine();
        let mut v = Vec::new();
        match p {
            Platform::Linux => {
                if sys != "Linux" || mach != "x86_64" {
                    v.push(format!(
                        "hydo's Linux release needs x86_64 Linux (found {sys} {mach}; on an Apple Silicon Mac use the Mac release)"
                    ));
                }
            }
            Platform::Mac => {
                if sys != "Darwin" || mach != "arm64" {
                    v.push(format!(
                        "hydo's Mac release needs an Apple Silicon Mac, M1 or later (found {sys} {mach})"
                    ));
                    return v;
                }
                let ver = run("sw_vers", &["-productVersion".as_ref()]).unwrap_or_default();
                let major: u32 = ver
                    .trim()
                    .split('.')
                    .next()
                    .and_then(|m| m.parse().ok())
                    .unwrap_or(0);
                if major < 13 {
                    v.push(format!(
                        "hydo needs macOS 13 or later (found {})",
                        ver.trim()
                    ));
                }
                let hv =
                    run("sysctl", &["-n".as_ref(), "kern.hv_support".as_ref()]).unwrap_or_default();
                if hv.trim() != "1" {
                    v.push(
                        "this Mac does not offer Hypervisor.framework (kern.hv_support is not 1)"
                            .into(),
                    );
                }
            }
        }
        v
    }

    fn quarantined(&self, p: &Path) -> bool {
        xattr_present(p, QUARANTINE)
    }

    fn unquarantine(&self, p: &Path) -> Result<(), String> {
        xattr_remove(p, QUARANTINE)
    }

    fn sign(&self, bin: &Path, entitlements: &Path) -> Result<(), String> {
        run(
            "codesign",
            &[
                "--force".as_ref(),
                "--entitlements".as_ref(),
                entitlements.as_os_str(),
                "-s".as_ref(),
                "-".as_ref(),
                bin.as_os_str(),
            ],
        )
        .map(|_| ())
    }

    fn signed_ok(&self, bin: &Path) -> bool {
        run(
            "codesign",
            &["--verify".as_ref(), "--strict".as_ref(), bin.as_os_str()],
        )
        .is_ok()
            && run(
                "codesign",
                &[
                    "-d".as_ref(),
                    "--entitlements".as_ref(),
                    "-".as_ref(),
                    bin.as_os_str(),
                ],
            )
            .is_ok_and(|t| t.contains("com.apple.security.hypervisor"))
    }

    fn untar(&self, tar: &Path, into: &Path) -> Result<(), String> {
        run(
            "tar",
            &[
                "-C".as_ref(),
                into.as_os_str(),
                "-xpf".as_ref(),
                tar.as_os_str(),
            ],
        )
        .map(|_| ())
    }
}

#[cfg(target_os = "macos")]
fn xattr_present(p: &Path, name: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let (Ok(cp), Ok(cn)) = (
        std::ffi::CString::new(p.as_os_str().as_bytes()),
        std::ffi::CString::new(name),
    ) else {
        return false;
    };
    // SAFETY: both strings are NUL-terminated; a NULL buffer of size 0 asks only for the size.
    unsafe {
        libc::getxattr(
            cp.as_ptr(),
            cn.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            libc::XATTR_NOFOLLOW,
        ) >= 0
    }
}

#[cfg(target_os = "macos")]
fn xattr_remove(p: &Path, name: &str) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let cp = std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let cn = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
    // SAFETY: both strings are NUL-terminated.
    let rc = unsafe { libc::removexattr(cp.as_ptr(), cn.as_ptr(), libc::XATTR_NOFOLLOW) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "cannot remove {name} from {}: {}",
            p.display(),
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn xattr_present(_: &Path, _: &str) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
fn xattr_remove(_: &Path, _: &str) -> Result<(), String> {
    Ok(())
}

fn sha256(p: &Path) -> std::io::Result<String> {
    hydo_engine::image::sha256_file(p)
}

/// A tree's entries, sorted by path, one line each (tab-separated, the path last):
/// `d <mode> <path>`, `f <mode> <sha256> <path>`, `l <target> <path>`. Modes are the low 12 bits
/// in octal. Paths are relative to the tree's root, which is not listed.
pub fn listing(dir: &Path) -> std::io::Result<Vec<String>> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        let mut names: Vec<_> = std::fs::read_dir(dir)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<_, _>>()?;
        names.sort();
        for n in names {
            let p = dir.join(&n);
            let rel = p.strip_prefix(base).unwrap_or(&p).display().to_string();
            let md = std::fs::symlink_metadata(&p)?;
            let mode = md.permissions().mode() & 0o7777;
            if md.file_type().is_symlink() {
                let t = std::fs::read_link(&p)?;
                out.push(format!("l\t{}\t{rel}", t.display()));
            } else if md.is_dir() {
                out.push(format!("d\t{mode:04o}\t{rel}"));
                walk(base, &p, out)?;
            } else {
                out.push(format!("f\t{mode:04o}\t{}\t{rel}", sha256(&p)?));
            }
        }
        Ok(())
    }
    let mut v = Vec::new();
    walk(dir, dir, &mut v)?;
    Ok(v)
}

fn line_path(l: &str) -> &str {
    l.rsplit('\t').next().unwrap_or(l)
}

/// The first difference between a listing setup recorded and the tree now, in words.
fn listing_difference(recorded: &[String], now: &[String]) -> Option<(String, String)> {
    let a: BTreeMap<&str, &str> = recorded
        .iter()
        .map(|l| (line_path(l), l.as_str()))
        .collect();
    let b: BTreeMap<&str, &str> = now.iter().map(|l| (line_path(l), l.as_str())).collect();
    for (p, l) in &a {
        match b.get(p) {
            None => return Some((p.to_string(), "is missing".into())),
            Some(m) if m != l => {
                return Some((
                    p.to_string(),
                    format!("has changed (setup unpacked `{l}`, it is `{m}` now)"),
                ))
            }
            _ => {}
        }
    }
    b.keys()
        .find(|p| !a.contains_key(*p))
        .map(|p| (p.to_string(), "was not in guest/store.tar".into()))
}

/// The programs setup makes executable.
fn is_program(path: &str) -> bool {
    path.starts_with("bin/") || path.starts_with("libexec/")
}

/// Every file under root, for the quarantine check (symlinks not followed).
fn all_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            out.push(p.clone());
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&p, out);
            }
        }
    }
    let mut v = vec![root.to_path_buf()];
    walk(root, &mut v);
    v
}

fn changed(now: &str, want: &str) -> String {
    format!("has changed (sha256 {now}, the release's is {want})")
}

/// Look at a release root: which files are damaged and which steps setup would take.
pub fn inspect(root: &Path, p: Platform, pins: &[Pin], host: &dyn Host) -> Report {
    let mut r = Report {
        unsupported: host.unsupported(p),
        ..Report::default()
    };
    let Ok(sums_text) = std::fs::read_to_string(root.join(SUMS)) else {
        return r;
    };
    r.release = true;
    let damage = |r: &mut Report, path: &str, why: String| {
        r.damaged.push(Damage {
            path: path.to_string(),
            why,
            fix: None,
        })
    };
    let sums = match parse_sums(&sums_text) {
        Ok(s) => s,
        Err(e) => {
            damage(&mut r, SUMS, format!("cannot be read: {e}"));
            return r;
        }
    };
    let record = match std::fs::read_to_string(root.join(RECORD)) {
        Ok(t) => match Record::parse(&t) {
            Ok(rec) => Some(rec),
            Err(e) => {
                damage(&mut r, RECORD, format!("cannot be read: {e}"));
                None
            }
        },
        Err(_) => None,
    };
    let want: BTreeMap<&str, &str> = sums.iter().map(|(s, p)| (p.as_str(), s.as_str())).collect();
    for pin in pins {
        match want.get(pin.path.as_str()) {
            None => damage(
                &mut r,
                &pin.path,
                format!(
                    "is a pinned {} of this hydo, but the release's {SUMS} does not list it \
                     (this bin/hydo belongs to another release)",
                    pin.list
                ),
            ),
            Some(s) if *s != pin.sha256 => damage(
                &mut r,
                &pin.path,
                format!(
                    "the release's {SUMS} lists sha256 {s}, but this hydo's pinned {} is {} \
                     (this bin/hydo belongs to another release)",
                    pin.list, pin.sha256
                ),
            ),
            Some(_) => r.pinned += 1,
        }
    }
    // the damages from here on are of files the release lists: restoring one repairs them
    let file_damages = r.damaged.len();
    let mut record_needed = false;
    for (sha, path) in &sums {
        let file = root.join(path);
        if p == Platform::Linux && path == STORE_TAR {
            record_needed = true;
            match sha256(&file) {
                Ok(h) if h == *sha => {
                    r.checked += 1;
                    r.pending.push(Step::UnpackStore);
                }
                Ok(h) => damage(&mut r, path, changed(&h, sha)),
                Err(_) => {
                    let unpacked = record
                        .as_ref()
                        .filter(|rec| rec.store_from.as_deref() == Some(sha.as_str()));
                    match unpacked {
                        None => damage(
                            &mut r,
                            path,
                            format!(
                                "is missing, and {STORE_DIR} was not unpacked from it by hydo setup"
                            ),
                        ),
                        Some(rec) => match listing(&root.join(STORE_DIR)) {
                            Err(e) => damage(&mut r, STORE_DIR, format!("cannot be read: {e}")),
                            Ok(now) => match listing_difference(&rec.store, &now) {
                                Some((q, why)) => damage(&mut r, &format!("{STORE_DIR}/{q}"), why),
                                None => r.checked += now.len(),
                            },
                        },
                    }
                }
            }
            continue;
        }
        if p == Platform::Mac && path == ENGINE_MAC {
            record_needed = true;
            match sha256(&file) {
                Err(_) => damage(&mut r, path, "is missing".into()),
                // a signed release (Milestone 172: a Developer ID signature with the hypervisor
                // entitlement, attached to the reproducible build) is used as it is; an unsigned
                // build (cargo's ad-hoc linker signature, no entitlement) is signed here
                Ok(h) if h == *sha => {
                    r.checked += 1;
                    if !host.signed_ok(&file) {
                        r.pending.push(Step::Sign);
                    }
                }
                Ok(h) => match record.as_ref().and_then(|rec| rec.signed.as_ref()) {
                    Some((from, to)) if from == sha && *to == h => {
                        if host.signed_ok(&file) {
                            r.checked += 1;
                        } else {
                            damage(
                                &mut r,
                                path,
                                "is not signed with the hypervisor entitlement any more".into(),
                            );
                        }
                    }
                    _ => damage(
                        &mut r,
                        path,
                        format!(
                            "has changed (sha256 {h}; the release's is {sha}, and hydo setup did \
                             not sign it to this)"
                        ),
                    ),
                },
            }
            continue;
        }
        match sha256(&file) {
            Ok(h) if h == *sha => r.checked += 1,
            Ok(h) => damage(&mut r, path, changed(&h, sha)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                damage(&mut r, path, "is missing".into())
            }
            Err(e) => damage(&mut r, path, format!("cannot be read: {e}")),
        }
        if is_program(path) {
            let mode = std::fs::metadata(&file)
                .map(|m| m.permissions().mode())
                .unwrap_or(0o755);
            if mode & 0o111 != 0o111 {
                r.pending.push(Step::Executable(path.clone()));
            }
        }
    }
    if p == Platform::Mac {
        let n = all_files(root)
            .iter()
            .filter(|f| host.quarantined(f))
            .count();
        if n > 0 {
            r.pending.insert(0, Step::Unquarantine(n));
        }
    }
    if record_needed && (record.is_none() || !r.pending.is_empty()) {
        r.pending.push(Step::Record);
    }
    // the store is unpacked after the cheaper steps, the record written last
    r.pending.sort_by_key(|s| match s {
        Step::Unquarantine(_) => 0,
        Step::Executable(_) => 1,
        Step::UnpackStore | Step::Sign => 2,
        Step::Record => 3,
    });
    let listed: BTreeMap<&str, ()> = sums.iter().map(|(_, p)| (p.as_str(), ())).collect();
    for d in &mut r.damaged[file_damages..] {
        d.fix = if d.path == STORE_DIR || d.path.starts_with(&format!("{STORE_DIR}/")) {
            // the store tree is unpacked again from a restored tar
            listed
                .contains_key(STORE_TAR)
                .then(|| STORE_TAR.to_string())
        } else {
            listed.contains_key(d.path.as_str()).then(|| d.path.clone())
        };
    }
    r
}

/// Where a damaged release's files come from again (Milestone 162): the install cache's archive
/// of the release's version (`cache::VersionAssets`). The release's own `SHA256SUMS` says what
/// each restored file must be.
pub trait Assets {
    /// Where the files would come from, in words ("hydo-linux-0.1.0.tar.gz in the install
    /// cache", "…, fetched from <url>"), or why they cannot come from anywhere now (offline).
    fn plan(&self) -> Result<String, String>;
    /// Write each (path, sha256) into the release tree `root`, verified. Returns the source in
    /// words.
    fn restore(&self, root: &Path, files: &[(String, String)]) -> Result<String, String>;
}

/// The files that would repair `damaged`, with the sha256 the release's `SHA256SUMS` lists
/// (None: a damage no restored file repairs).
fn repairs(root: &Path, damaged: &[Damage]) -> Option<Vec<(String, String)>> {
    let sums = parse_sums(&std::fs::read_to_string(root.join(SUMS)).ok()?).ok()?;
    let by: BTreeMap<&str, &str> = sums.iter().map(|(s, p)| (p.as_str(), s.as_str())).collect();
    let mut v: Vec<(String, String)> = Vec::new();
    for d in damaged {
        let f = d.fix.as_deref()?;
        let s = by.get(f)?;
        if !v.iter().any(|(p, _)| p == f) {
            v.push((f.to_string(), s.to_string()));
        }
    }
    Some(v)
}

/// Restore the damaged files from `assets`. Ok: what was done (empty when nothing was
/// damaged); Err: why it could not.
fn obtain_assets(
    root: &Path,
    damaged: &[Damage],
    assets: Option<&dyn Assets>,
) -> Result<Vec<String>, String> {
    let (Some(a), false) = (assets, damaged.is_empty()) else {
        return Ok(Vec::new());
    };
    let Some(files) = repairs(root, damaged) else {
        return Err("a damage no file of the release's archive repairs is among them".into());
    };
    // a restored guest/store.tar is unpacked again by the step UnpackStore, which replaces the
    // damaged tree
    let from = a.restore(root, &files)?;
    Ok(vec![format!(
        "restore {} file{} from {from}: {}",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        files
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )])
}

fn chmod_tree_writable(dir: &Path) {
    for f in all_files(dir) {
        if let Ok(md) = std::fs::symlink_metadata(&f) {
            if !md.file_type().is_symlink() {
                let m = md.permissions().mode();
                let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(m | 0o200));
            }
        }
    }
}

/// Take the pending steps of a report. Returns what was done, one line per step.
pub fn apply(root: &Path, report: &Report, host: &dyn Host) -> Result<Vec<String>, String> {
    let mut record = std::fs::read_to_string(root.join(RECORD))
        .ok()
        .and_then(|t| Record::parse(&t).ok())
        .unwrap_or_default();
    let mut done = Vec::new();
    for step in &report.pending {
        match step {
            Step::Executable(p) => {
                std::fs::set_permissions(root.join(p), std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("cannot make {p} executable: {e}"))?;
            }
            Step::Unquarantine(_) => {
                for f in all_files(root) {
                    if host.quarantined(&f) {
                        host.unquarantine(&f)?;
                    }
                }
            }
            Step::UnpackStore => {
                let tar = root.join(STORE_TAR);
                let dir = root.join(STORE_DIR);
                let from = sha256(&tar).map_err(|e| format!("cannot read {STORE_TAR}: {e}"))?;
                if dir.exists() {
                    // a store tree left by an interrupted setup (its directories are read-only)
                    chmod_tree_writable(&dir);
                    std::fs::remove_dir_all(&dir)
                        .map_err(|e| format!("cannot remove the old {STORE_DIR}: {e}"))?;
                }
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("cannot create {STORE_DIR}: {e}"))?;
                host.untar(&tar, &dir)?;
                record.store =
                    listing(&dir).map_err(|e| format!("cannot read {STORE_DIR}: {e}"))?;
                record.store_from = Some(from);
                // the record says what the tree came from before the tar is gone
                write_record(root, &record)?;
                std::fs::remove_file(&tar)
                    .map_err(|e| format!("cannot remove {STORE_TAR}: {e}"))?;
            }
            Step::Sign => {
                let bin = root.join(ENGINE_MAC);
                let from = sha256(&bin).map_err(|e| format!("cannot read {ENGINE_MAC}: {e}"))?;
                host.sign(&bin, &root.join(ENTITLEMENTS))?;
                if !host.signed_ok(&bin) {
                    return Err(format!(
                        "{ENGINE_MAC} was signed, but codesign does not show the hypervisor entitlement"
                    ));
                }
                let to = sha256(&bin).map_err(|e| format!("cannot read {ENGINE_MAC}: {e}"))?;
                record.signed = Some((from, to));
            }
            Step::Record => write_record(root, &record)?,
        }
        done.push(step.describe());
    }
    Ok(done)
}

fn write_record(root: &Path, r: &Record) -> Result<(), String> {
    let tmp = root.join(format!("{RECORD}.tmp"));
    std::fs::write(&tmp, r.text())
        .and_then(|_| std::fs::rename(&tmp, root.join(RECORD)))
        .map_err(|e| format!("cannot write {RECORD}: {e}"))
}

/// What setup does on a root, end to end: inspect, take the steps, inspect again. Err holds the
/// lines that say why it refused.
pub fn setup(
    root: &Path,
    p: Platform,
    pins: &[Pin],
    host: &dyn Host,
) -> Result<(Report, Vec<String>), Vec<String>> {
    setup_with(root, p, pins, host, None)
}

/// [`setup`], restoring damaged files from `assets` first (Milestone 162). Every restored file
/// is checked by the inspection that follows, against the release's `SHA256SUMS` and this
/// hydo's pins.
pub fn setup_with(
    root: &Path,
    p: Platform,
    pins: &[Pin],
    host: &dyn Host,
    assets: Option<&dyn Assets>,
) -> Result<(Report, Vec<String>), Vec<String>> {
    let mut r = inspect(root, p, pins, host);
    if !r.release {
        return Err(vec![not_a_release(root)]);
    }
    let mut restored = Vec::new();
    if r.unsupported.is_empty() && !r.damaged.is_empty() && assets.is_some() {
        match obtain_assets(root, &r.damaged, assets) {
            Ok(done) => {
                restored = done;
                r = inspect(root, p, pins, host);
            }
            Err(e) => {
                let mut v = refusal(&r);
                let last = v.len() - 1;
                v[last] = format!(
                    "the release is damaged or incomplete, and hydo setup could not restore it \
                     from the release's archive ({e}): unpack the release again (check the \
                     download) and run its install.sh"
                );
                return Err(v);
            }
        }
    }
    if !r.unsupported.is_empty() || !r.damaged.is_empty() {
        return Err(refusal(&r));
    }
    let mut done = restored;
    done.extend(apply(root, &r, host).map_err(|e| vec![e])?);
    let after = inspect(root, p, pins, host);
    if !after.ok() || !after.pending.is_empty() {
        let mut v = refusal(&after);
        v.extend(
            after
                .pending
                .iter()
                .map(|s| format!("still to do: {}", s.describe())),
        );
        return Err(v);
    }
    Ok((after, done))
}

fn not_a_release(root: &Path) -> String {
    format!(
        "{} is not an unpacked hydo release (it has no {SUMS}); `hydo setup` sets up a release's \
         own directory",
        root.display()
    )
}

fn refusal(r: &Report) -> Vec<String> {
    let mut v: Vec<String> = r.unsupported.clone();
    if !r.damaged.is_empty() {
        v.extend(r.damaged.iter().take(20).map(Damage::line));
        if r.damaged.len() > 20 {
            v.push(format!("... and {} more", r.damaged.len() - 20));
        }
        v.push(
            "the release is damaged or incomplete, and hydo setup repairs no file: unpack the \
             release again (check the download) and run its install.sh"
                .into(),
        );
    }
    v
}

/// The shell startup file `--add-to-path` writes to: by the login shell, `~/.zshrc` for zsh and
/// `~/.bashrc` (Linux) or `~/.bash_profile` (Mac) for bash; otherwise the platform's usual one.
pub fn rc_file(home: &Path, shell: &str, p: Platform) -> PathBuf {
    let sh = Path::new(shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match (sh.as_str(), p) {
        ("zsh", _) => ".zshrc",
        ("bash", Platform::Mac) => ".bash_profile",
        ("bash", Platform::Linux) => ".bashrc",
        (_, Platform::Mac) => ".zshrc",
        (_, Platform::Linux) => ".bashrc",
    };
    home.join(name)
}

/// Add `export PATH="<bin>:$PATH"` to the startup file once. Returns whether it was added.
pub fn add_to_path(rc: &Path, bin: &Path) -> std::io::Result<bool> {
    let line = format!("export PATH=\"{}:$PATH\"", bin.display());
    let text = std::fs::read_to_string(rc).unwrap_or_default();
    if text.lines().any(|l| l == line) {
        return Ok(false);
    }
    let mut t = text;
    if !t.is_empty() && !t.ends_with('\n') {
        t.push('\n');
    }
    t.push_str(&line);
    t.push('\n');
    std::fs::write(rc, t)?;
    Ok(true)
}

fn platform_word(p: Platform) -> &'static str {
    match p {
        Platform::Linux => "Linux",
        Platform::Mac => "Mac",
    }
}

/// The inspection `hydo doctor` shows.
pub fn inspect_ctx(ctx: &Ctx) -> Report {
    inspect(&ctx.root, ctx.platform, &pins(ctx.platform), &System)
}

const USAGE: &str = "usage: hydo setup [--check | --verify] [--add-to-path] [--offline]\n       \
                     hydo setup --get [VERSION] [--add-to-path] [--offline]\n       \
                     hydo setup --archive FILE";

/// What `hydo setup --check` and `--verify` print, and their exit status (0 nothing to do or
/// intact, 1 damaged, 3 steps pending). Damage that `assets` can restore is a pending step.
pub fn check_lines(
    root: &Path,
    p: Platform,
    pins: &[Pin],
    host: &dyn Host,
    verify: bool,
    assets: Option<&dyn Assets>,
) -> (Vec<String>, i32) {
    let mut out = Vec::new();
    let r = inspect(root, p, pins, host);
    if !r.release {
        out.push(format!("  {}", not_a_release(root)));
        return (out, 1);
    }
    for u in &r.unsupported {
        out.push(format!("  cannot run here: {u}"));
    }
    out.push(format!(
        "  {} files and store entries match ({} of them pinned in this hydo)",
        r.checked, r.pinned
    ));
    for d in &r.damaged {
        out.push(format!("  damaged: {}", d.line()));
    }
    // what restoring from the version's archive would do (not for --verify, which checks the
    // files only)
    let restore = match (verify, assets, r.damaged.is_empty()) {
        (false, Some(a), false) => match repairs(root, &r.damaged) {
            None => Err("a damage no file of the release's archive repairs is among them".into()),
            Some(files) => a.plan().map(|from| (files.len(), from)),
        },
        _ => Err(String::new()),
    };
    if !r.unsupported.is_empty() || (!r.damaged.is_empty() && restore.is_err()) {
        if let Err(e) = &restore {
            if !e.is_empty() {
                out.push(format!("  cannot restore the damaged files: {e}"));
            }
        }
        out.push(
            "  the release is damaged or incomplete: unpack it again and run its install.sh".into(),
        );
        return (out, 1);
    }
    if verify {
        out.push("  the release's files are intact".into());
        return (out, 0);
    }
    if let Ok((n, from)) = &restore {
        out.push(format!(
            "  hydo setup would: restore {n} file{} from {from} (then check the release again)",
            if *n == 1 { "" } else { "s" }
        ));
        return (out, 3);
    }
    if r.pending.is_empty() {
        out.push("  set up: nothing to do".into());
        return (out, 0);
    }
    for s in &r.pending {
        out.push(format!("  hydo setup would: {}", s.describe()));
    }
    (out, 3)
}

fn print_add_to_path(bin: &Path, p: Platform) -> i32 {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    let shell = std::env::var("SHELL").unwrap_or_default();
    let rc = rc_file(&home, &shell, p);
    match add_to_path(&rc, bin) {
        Ok(true) => println!("  added {} to PATH in {}", bin.display(), rc.display()),
        Ok(false) => println!("  {} is on PATH in {} already", bin.display(), rc.display()),
        Err(e) => {
            eprintln!("hydo setup: cannot write {}: {e}", rc.display());
            return 1;
        }
    }
    0
}

fn platform_name(p: Platform) -> &'static str {
    match p {
        Platform::Linux => "linux",
        Platform::Mac => "mac",
    }
}

/// The install cache of this `hydo` command (Milestone 162).
pub fn cache_of(ctx: &Ctx, offline: bool) -> crate::cache::Cache {
    let c = &ctx.config.cli.cache;
    crate::cache::Cache::new(c, platform_name(ctx.platform), offline || c.offline, None)
}

pub fn cmd_setup(ctx: &Ctx, argv: &[String]) -> i32 {
    let mut check = false;
    let mut verify = false;
    let mut add_path = false;
    let mut offline = false;
    let mut get: Option<Option<String>> = None;
    let mut archive: Option<PathBuf> = None;
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "--check" => check = true,
            "--verify" => verify = true,
            "--add-to-path" => add_path = true,
            "--offline" => offline = true,
            "--get" => {
                let v = argv.get(i + 1).filter(|v| !v.starts_with('-')).cloned();
                if v.is_some() {
                    i += 1;
                }
                get = Some(v);
            }
            "--archive" => {
                let Some(f) = argv.get(i + 1) else {
                    die_rc(&format!("setup: --archive needs a file ({USAGE})"), 2)
                };
                archive = Some(PathBuf::from(f));
                i += 1;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            _ => die_rc(&format!("setup: unknown argument {a} ({USAGE})"), 2),
        }
        i += 1;
    }
    if check && verify {
        die_rc("setup: --check or --verify, not both", 2);
    }
    if add_path && (check || verify) {
        die_rc(
            "setup: --add-to-path changes a file; not with --check or --verify",
            2,
        );
    }
    if (get.is_some() || archive.is_some()) && (check || verify) {
        die_rc(
            "setup: --get and --archive change the install cache; not with --check or --verify",
            2,
        );
    }
    // a hydo installed by cargo (M169) whose version is not installed yet: plain `hydo setup`
    // installs it, as `hydo setup --get` does
    if get.is_none()
        && !check
        && !verify
        && ctx.cargo_installed
        && !ctx.root.join("SHA256SUMS").exists()
    {
        get = Some(None);
    }
    let cache = cache_of(ctx, offline);
    if let Some(f) = &archive {
        match cache.import(f) {
            Ok(p) => println!(
                "hydo setup: {} is in the install cache ({})",
                f.display(),
                p.display()
            ),
            Err(e) => {
                eprintln!("hydo setup: {e}");
                return 1;
            }
        }
        if get.is_none() {
            return 0;
        }
    }
    if let Some(v) = get {
        let version = v.unwrap_or_else(|| cache.own_version.clone());
        // this hydo's pins check its own version; another version's archive is pinned in the
        // release index
        let pins = if version == cache.own_version {
            pins(ctx.platform)
        } else {
            Vec::new()
        };
        println!(
            "hydo setup: hydo {version} for the {} into {}",
            platform_word(ctx.platform),
            cache.home.version(&version).display()
        );
        match cache.install(&version, &pins, &System, ctx.platform) {
            Err(lines) => {
                for l in lines {
                    eprintln!("hydo setup: {l}");
                }
                return 1;
            }
            Ok(done) => {
                for d in &done {
                    println!("  done: {d}");
                }
            }
        }
        if add_path && print_add_to_path(&cache.home.bin(), ctx.platform) != 0 {
            return 1;
        }
        if ctx.cargo_installed && version == cache.own_version {
            let how = if ctx.checkout.is_some() {
                "built in a checkout"
            } else {
                "installed by cargo"
            };
            println!("hydo setup: hydo {version} is installed; this hydo ({how}) uses it");
        } else {
            println!(
                "hydo setup: hydo {version} is installed; run it as {}",
                cache.program(&version).display()
            );
        }
        return 0;
    }
    let root = &ctx.root;
    let pins = pins(ctx.platform);
    let version = std::fs::read_to_string(root.join("VERSION"))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|_| "?".into());
    let head = format!(
        "hydo setup: {} (hydo {version}, the {} release)",
        root.display(),
        platform_word(ctx.platform)
    );
    let va = crate::cache::VersionAssets {
        cache: &cache,
        version: version.clone(),
    };
    let assets: Option<&dyn Assets> =
        hydo_image::releases::valid_version(&version).then_some(&va as &dyn Assets);
    if check || verify {
        println!("{head}");
        let (lines, rc) = check_lines(root, ctx.platform, &pins, &System, verify, assets);
        for l in lines {
            println!("{l}");
        }
        return rc;
    }
    println!("{head}");
    match setup_with(root, ctx.platform, &pins, &System, assets) {
        Err(lines) => {
            for l in lines {
                eprintln!("hydo setup: {l}");
            }
            1
        }
        Ok((r, done)) => {
            for d in &done {
                println!("  done: {d}");
            }
            println!(
                "  {} files and store entries match ({} of them pinned in this hydo)",
                r.checked, r.pinned
            );
            if add_path && print_add_to_path(&root.join("bin"), ctx.platform) != 0 {
                return 1;
            }
            println!(
                "hydo setup: {}",
                if done.is_empty() {
                    "set up already; nothing to do"
                } else {
                    "done"
                }
            );
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hydo-setup-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        if d.exists() {
            chmod_tree_writable(&d);
            let _ = std::fs::remove_dir_all(&d);
        }
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn cleanup(d: &Path) {
        chmod_tree_writable(d);
        let _ = std::fs::remove_dir_all(d);
    }

    fn write(root: &Path, rel: &str, body: &[u8], mode: u32) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn sums(root: &Path, files: &[&str]) {
        let mut t = String::new();
        for f in files {
            t.push_str(&format!("{}  {f}\n", sha256(&root.join(f)).unwrap()));
        }
        std::fs::write(root.join(SUMS), t).unwrap();
    }

    fn pin(root: &Path, rel: &str, list: &'static str) -> Pin {
        Pin {
            path: rel.into(),
            sha256: sha256(&root.join(rel)).unwrap(),
            list,
        }
    }

    /// The machine as the tests see it: supported, no quarantine unless told, and a signature
    /// that appends a marker (so the signed engine's bytes change as codesign's do).
    #[derive(Default)]
    struct Fake {
        quarantined: RefCell<Vec<PathBuf>>,
        signs: RefCell<usize>,
    }

    const MARK: &[u8] = b"\nSIGNED-WITH-HYPERVISOR";

    impl Host for Fake {
        fn unsupported(&self, _: Platform) -> Vec<String> {
            Vec::new()
        }
        fn quarantined(&self, p: &Path) -> bool {
            self.quarantined.borrow().iter().any(|q| q == p)
        }
        fn unquarantine(&self, p: &Path) -> Result<(), String> {
            self.quarantined.borrow_mut().retain(|q| q != p);
            Ok(())
        }
        fn sign(&self, bin: &Path, ent: &Path) -> Result<(), String> {
            assert!(ent.is_file(), "no entitlements file {}", ent.display());
            *self.signs.borrow_mut() += 1;
            let mut b = std::fs::read(bin).unwrap();
            if !b.ends_with(MARK) {
                b.extend_from_slice(MARK);
            }
            std::fs::write(bin, b).map_err(|e| e.to_string())
        }
        fn signed_ok(&self, bin: &Path) -> bool {
            std::fs::read(bin).is_ok_and(|b| b.ends_with(MARK))
        }
        fn untar(&self, tar: &Path, into: &Path) -> Result<(), String> {
            System.untar(tar, into)
        }
    }

    /// A small Linux release: a program, a pinned asset, a guest program and a guest store tar
    /// with a read-only directory, a read-only file and a symlink.
    fn linux_release() -> (PathBuf, Vec<Pin>) {
        let root = scratch();
        write(&root, "bin/hydo", b"#!/bin/sh\n", 0o755);
        write(&root, "VERSION", b"0.1.0\n", 0o644);
        write(&root, "guest/bzImage", b"a kernel", 0o644);
        write(&root, "guest/hydo-agent", b"an agent", 0o755);
        let st = root.join("st");
        write(&st, "nix/store/abc-tool/bin/tool", b"a tool", 0o555);
        std::os::unix::fs::symlink("tool", st.join("nix/store/abc-tool/bin/tool2")).unwrap();
        for d in ["nix/store/abc-tool/bin", "nix/store/abc-tool"] {
            std::fs::set_permissions(st.join(d), std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        let ok = Command::new("tar")
            .arg("-C")
            .arg(&st)
            .arg("-cf")
            .arg(root.join(STORE_TAR))
            .arg("nix")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        cleanup(&st);
        let files = [
            "VERSION",
            "bin/hydo",
            "guest/bzImage",
            "guest/hydo-agent",
            STORE_TAR,
        ];
        sums(&root, &files);
        let pins = vec![
            pin(&root, "guest/bzImage", "asset"),
            pin(&root, STORE_TAR, "asset"),
            pin(&root, "guest/hydo-agent", "guest program"),
        ];
        (root, pins)
    }

    fn mac_release() -> (PathBuf, Vec<Pin>) {
        let root = scratch();
        write(&root, "bin/hydo", b"hydo", 0o755);
        write(&root, ENGINE_MAC, b"the engine", 0o755);
        write(&root, ENTITLEMENTS, b"<plist/>", 0o644);
        write(&root, "guest/busybox", b"busybox", 0o755);
        sums(
            &root,
            &["bin/hydo", ENGINE_MAC, ENTITLEMENTS, "guest/busybox"],
        );
        let pins = vec![pin(&root, "guest/busybox", "asset")];
        (root, pins)
    }

    #[test]
    fn the_built_in_lists_parse_and_map_into_the_release() {
        for (p, assets, guest, n_assets, n_guest) in [
            (Platform::Linux, LINUX_ASSETS, LINUX_GUEST, 39, 4),
            (Platform::Mac, MAC_ASSETS, MAC_GUEST, 62, 3),
        ] {
            let a = parse_sums(assets).unwrap();
            assert_eq!(a.len(), n_assets, "{p:?} assets");
            let mut seen: BTreeMap<String, String> = BTreeMap::new();
            for (sha, path) in &a {
                let rel = asset_release_path(p, path)
                    .unwrap_or_else(|| panic!("{p:?}: asset {path} has no place in the release"));
                if let Some(old) = seen.insert(rel.clone(), sha.clone()) {
                    assert_eq!(&old, sha, "{p:?}: {rel} pinned twice with different sha256");
                }
            }
            let g = parse_sums(guest).unwrap();
            assert_eq!(g.len(), n_guest, "{p:?} guest programs");
            assert_eq!(pins(p).len(), seen.len() + g.len());
            // the agent's pin is the one hydo-build stages
            let agent = g.iter().find(|(_, p)| p == "guest/hydo-agent").unwrap();
            assert_eq!(
                agent.0,
                match p {
                    Platform::Linux => hydo_build::agent::AGENT_SHA256_X86_64,
                    Platform::Mac => hydo_build::agent::AGENT_SHA256_ARM64,
                }
            );
        }
    }

    #[test]
    fn linux_setup_unpacks_the_store_once_and_then_has_nothing_to_do() {
        let (root, pins) = linux_release();
        let host = Fake::default();
        let r = inspect(&root, Platform::Linux, &pins, &host);
        assert!(r.ok(), "{:?}", r.damaged);
        assert_eq!(r.pending, vec![Step::UnpackStore, Step::Record]);
        assert_eq!(r.pinned, 3);
        let (after, done) = setup(&root, Platform::Linux, &pins, &host).unwrap();
        assert_eq!(done.len(), 2, "{done:?}");
        assert!(after.pending.is_empty());
        assert!(!root.join(STORE_TAR).exists());
        let tool = root.join("guest/store/nix/store/abc-tool/bin/tool");
        assert_eq!(std::fs::read(&tool).unwrap(), b"a tool");
        assert_eq!(
            std::fs::metadata(&tool).unwrap().permissions().mode() & 0o777,
            0o555
        );
        let rec = std::fs::read_to_string(root.join(RECORD)).unwrap();
        let tree = listing(&root).unwrap();
        // idempotent: a second run takes no step and changes no file
        let (again, done2) = setup(&root, Platform::Linux, &pins, &host).unwrap();
        assert!(done2.is_empty(), "{done2:?}");
        assert_eq!(again.checked, after.checked);
        assert_eq!(std::fs::read_to_string(root.join(RECORD)).unwrap(), rec);
        assert_eq!(listing(&root).unwrap(), tree);
        cleanup(&root);
    }

    #[test]
    fn a_changed_file_is_refused_with_its_name_and_both_sha256() {
        let (root, pins) = linux_release();
        let host = Fake::default();
        let want = sha256(&root.join("guest/bzImage")).unwrap();
        write(&root, "guest/bzImage", b"a damaged kernel", 0o644);
        let now = sha256(&root.join("guest/bzImage")).unwrap();
        let r = inspect(&root, Platform::Linux, &pins, &host);
        assert_eq!(
            r.damaged,
            vec![Damage {
                path: "guest/bzImage".into(),
                why: format!("has changed (sha256 {now}, the release's is {want})"),
                fix: Some("guest/bzImage".into()),
            }]
        );
        let err = setup(&root, Platform::Linux, &pins, &host).unwrap_err();
        assert!(err[0].starts_with("guest/bzImage: has changed"), "{err:?}");
        assert!(
            err.last().unwrap().contains("unpack the release again"),
            "{err:?}"
        );
        // nothing was done: the tar is still there and no record was written
        assert!(root.join(STORE_TAR).exists());
        assert!(!root.join(RECORD).exists());
        cleanup(&root);
    }

    #[test]
    fn a_missing_file_and_a_foreign_pin_are_refused() {
        let (root, mut pins) = linux_release();
        let host = Fake::default();
        std::fs::remove_file(root.join("guest/hydo-agent")).unwrap();
        pins.push(Pin {
            path: "guest/hydo-world".into(),
            sha256: "0".repeat(64),
            list: "guest program",
        });
        pins[0].sha256 = "1".repeat(64);
        let r = inspect(&root, Platform::Linux, &pins, &host);
        let lines: Vec<String> = r.damaged.iter().map(Damage::line).collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("guest/bzImage: the release's SHA256SUMS lists sha256 "));
        assert!(lines[0].ends_with(&format!(
            "but this hydo's pinned asset is {} (this bin/hydo belongs to another release)",
            "1".repeat(64)
        )));
        assert!(lines[1].starts_with(
            "guest/hydo-world: is a pinned guest program of this hydo, but the release's SHA256SUMS does not list it"
        ));
        assert_eq!(lines[2], "guest/hydo-agent: is missing");
        assert!(setup(&root, Platform::Linux, &pins, &host).is_err());
        cleanup(&root);
    }

    #[test]
    fn a_changed_store_entry_is_found_after_the_tar_is_gone() {
        let (root, pins) = linux_release();
        let host = Fake::default();
        setup(&root, Platform::Linux, &pins, &host).unwrap();
        let bin = root.join("guest/store/nix/store/abc-tool/bin");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = inspect(&root, Platform::Linux, &pins, &host);
        assert_eq!(r.damaged.len(), 1, "{:?}", r.damaged);
        assert_eq!(r.damaged[0].path, "guest/store/nix/store/abc-tool/bin");
        assert_eq!(
            r.damaged[0].why,
            "has changed (setup unpacked `d\t0555\tnix/store/abc-tool/bin`, it is `d\t0755\tnix/store/abc-tool/bin` now)"
        );
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(inspect(&root, Platform::Linux, &pins, &host).ok());
        // a damaged record is damage too, and without the tar the tree cannot be checked
        std::fs::remove_file(root.join(RECORD)).unwrap();
        let r = inspect(&root, Platform::Linux, &pins, &host);
        assert_eq!(
            r.damaged[0].line(),
            "guest/store.tar: is missing, and guest/store was not unpacked from it by hydo setup"
        );
        cleanup(&root);
    }

    #[test]
    fn a_program_without_its_mode_is_made_executable() {
        let (root, pins) = linux_release();
        let host = Fake::default();
        setup(&root, Platform::Linux, &pins, &host).unwrap();
        std::fs::set_permissions(
            root.join("bin/hydo"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let r = inspect(&root, Platform::Linux, &pins, &host);
        assert_eq!(
            r.pending,
            vec![Step::Executable("bin/hydo".into()), Step::Record]
        );
        let (_, done) = setup(&root, Platform::Linux, &pins, &host).unwrap();
        assert_eq!(done[0], "make bin/hydo executable");
        assert_eq!(
            std::fs::metadata(root.join("bin/hydo"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        cleanup(&root);
    }

    #[test]
    fn mac_setup_signs_the_engine_once_and_knows_the_signed_bytes() {
        let (root, pins) = mac_release();
        let host = Fake::default();
        host.quarantined.borrow_mut().push(root.join("bin/hydo"));
        let r = inspect(&root, Platform::Mac, &pins, &host);
        assert!(r.ok(), "{:?}", r.damaged);
        assert_eq!(
            r.pending,
            vec![Step::Unquarantine(1), Step::Sign, Step::Record]
        );
        let (_, done) = setup(&root, Platform::Mac, &pins, &host).unwrap();
        assert_eq!(done.len(), 3, "{done:?}");
        assert_eq!(*host.signs.borrow(), 1);
        assert!(host.quarantined.borrow().is_empty());
        let rec = Record::parse(&std::fs::read_to_string(root.join(RECORD)).unwrap()).unwrap();
        let (from, to) = rec.signed.unwrap();
        assert_ne!(from, to);
        assert_eq!(to, sha256(&root.join(ENGINE_MAC)).unwrap());
        // idempotent
        let (_, done2) = setup(&root, Platform::Mac, &pins, &host).unwrap();
        assert!(done2.is_empty(), "{done2:?}");
        assert_eq!(*host.signs.borrow(), 1);
        // the engine changed after signing: damaged; the release's unsigned bytes: sign again
        write(&root, ENGINE_MAC, b"another engine", 0o755);
        let r = inspect(&root, Platform::Mac, &pins, &host);
        assert_eq!(r.damaged.len(), 1);
        assert!(r.damaged[0]
            .why
            .contains("hydo setup did not sign it to this"));
        write(&root, ENGINE_MAC, b"the engine", 0o755);
        let r = inspect(&root, Platform::Mac, &pins, &host);
        assert_eq!(r.pending, vec![Step::Sign, Step::Record]);
        cleanup(&root);
    }

    #[test]
    fn a_signed_mac_release_is_not_signed_again() {
        // Milestone 172: the release's own engine already carries a valid signature with the
        // hypervisor entitlement (the Developer ID signature attached to the reproducible build)
        let root = scratch();
        write(&root, "bin/hydo", b"hydo", 0o755);
        let mut engine = b"the engine".to_vec();
        engine.extend_from_slice(MARK);
        write(&root, ENGINE_MAC, &engine, 0o755);
        write(&root, ENTITLEMENTS, b"<plist/>", 0o644);
        sums(&root, &["bin/hydo", ENGINE_MAC, ENTITLEMENTS]);
        let host = Fake::default();
        let r = inspect(&root, Platform::Mac, &[], &host);
        assert!(r.ok(), "{:?}", r.damaged);
        assert_eq!(r.pending, vec![Step::Record]);
        let (_, done) = setup(&root, Platform::Mac, &[], &host).unwrap();
        assert_eq!(done.len(), 1, "{done:?}");
        assert_eq!(
            *host.signs.borrow(),
            0,
            "a signed engine is not signed again"
        );
        assert_eq!(std::fs::read(root.join(ENGINE_MAC)).unwrap(), engine);
        let r = inspect(&root, Platform::Mac, &[], &host);
        assert!(r.ok() && r.pending.is_empty(), "{r:?}");
        cleanup(&root);
    }

    #[test]
    fn a_directory_without_sha256sums_is_not_a_release() {
        let root = scratch();
        let r = inspect(
            &root,
            Platform::Linux,
            &pins(Platform::Linux),
            &Fake::default(),
        );
        assert!(!r.release && !r.ok());
        let err = setup(&root, Platform::Linux, &[], &Fake::default()).unwrap_err();
        assert!(
            err[0].contains("is not an unpacked hydo release"),
            "{err:?}"
        );
        cleanup(&root);
    }

    /// The release's archive as a copy of the files (the install cache's part is in
    /// src/cache/tests.rs).
    struct Copy {
        from: PathBuf,
        restored: RefCell<Vec<String>>,
    }

    impl Assets for Copy {
        fn plan(&self) -> Result<String, String> {
            Ok("the copy".into())
        }
        fn restore(&self, root: &Path, files: &[(String, String)]) -> Result<String, String> {
            for (p, s) in files {
                assert_eq!(&sha256(&self.from.join(p)).unwrap(), s);
                let _ = std::fs::remove_file(root.join(p));
                std::fs::copy(self.from.join(p), root.join(p)).unwrap();
                self.restored.borrow_mut().push(p.clone());
            }
            Ok("the copy".into())
        }
    }

    #[test]
    fn a_damaged_store_tree_is_unpacked_again_from_a_restored_tar() {
        let (root, pins) = linux_release();
        let copy = scratch();
        std::fs::create_dir_all(copy.join("guest")).unwrap();
        std::fs::copy(root.join(STORE_TAR), copy.join(STORE_TAR)).unwrap();
        let host = Fake::default();
        setup(&root, Platform::Linux, &pins, &host).unwrap();
        let tool = root.join("guest/store/nix/store/abc-tool/bin/tool");
        let bin = tool.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&tool, b"changed").unwrap();
        let a = Copy {
            from: copy.clone(),
            restored: RefCell::new(Vec::new()),
        };
        let (lines, rc) = check_lines(&root, Platform::Linux, &pins, &host, false, Some(&a));
        assert_eq!(rc, 3, "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l == "  hydo setup would: restore 1 file from the copy (then check the release again)"),
            "{lines:?}"
        );
        let (after, done) = setup_with(&root, Platform::Linux, &pins, &host, Some(&a)).unwrap();
        assert!(
            after.ok() && after.pending.is_empty(),
            "{:?}",
            after.damaged
        );
        assert_eq!(*a.restored.borrow(), vec![STORE_TAR.to_string()]);
        assert_eq!(done[0], "restore 1 file from the copy: guest/store.tar");
        assert!(done.contains(&Step::UnpackStore.describe()), "{done:?}");
        assert_eq!(std::fs::read(&tool).unwrap(), b"a tool");
        assert!(!root.join(STORE_TAR).exists());
        // a file of another release (a foreign pin) is not restored: refused as before
        let mut foreign = pins.clone();
        foreign[0].sha256 = "e".repeat(64);
        let (_, rc) = check_lines(&root, Platform::Linux, &foreign, &host, false, Some(&a));
        assert_eq!(rc, 1);
        let e = setup_with(&root, Platform::Linux, &foreign, &host, Some(&a)).unwrap_err();
        assert!(e.last().unwrap().contains("could not restore"), "{e:?}");
        assert_eq!(a.restored.borrow().len(), 1);
        cleanup(&root);
        cleanup(&copy);
    }

    #[test]
    fn the_record_reads_back_what_it_wrote() {
        let r = Record {
            store_from: Some("a".repeat(64)),
            store: vec!["d\t0555\tnix".into(), "l\tx\tnix/y".into()],
            signed: Some(("b".repeat(64), "c".repeat(64))),
        };
        assert_eq!(Record::parse(&r.text()).unwrap(), r);
        assert!(Record::parse("hydo-setup\t2\n").is_err());
        assert!(Record::parse("store-from\tx\n").is_err());
    }

    #[test]
    fn add_to_path_writes_one_line_to_the_shells_startup_file() {
        let home = scratch();
        assert_eq!(
            rc_file(&home, "/bin/zsh", Platform::Linux),
            home.join(".zshrc")
        );
        assert_eq!(
            rc_file(&home, "/bin/bash", Platform::Linux),
            home.join(".bashrc")
        );
        assert_eq!(
            rc_file(&home, "/bin/bash", Platform::Mac),
            home.join(".bash_profile")
        );
        assert_eq!(rc_file(&home, "", Platform::Mac), home.join(".zshrc"));
        let rc = home.join(".bashrc");
        std::fs::write(&rc, "alias x=y").unwrap();
        assert!(add_to_path(&rc, Path::new("/opt/hydo/bin")).unwrap());
        assert!(!add_to_path(&rc, Path::new("/opt/hydo/bin")).unwrap());
        assert_eq!(
            std::fs::read_to_string(&rc).unwrap(),
            "alias x=y\nexport PATH=\"/opt/hydo/bin:$PATH\"\n"
        );
        cleanup(&home);
    }
}
