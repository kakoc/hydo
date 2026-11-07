//! The install cache (Milestone 162, docs/design/install-cache.md): release archives fetched from
//! the project's releases, verified by sha256 (the release index compiled into hydo, else the
//! release's `SHA256SUMS` asset), kept in the content store, and what is taken out of them: a
//! guest kernel a run needs ([`Cache::obtain_kernel`], registered as `hydo_image::kernels`'
//! source), files `hydo setup` restores into a damaged release ([`Cache::restore`]), and a whole
//! version installed into `versions/<version>` ([`Cache::install`]).
//!
//! Nothing downloaded is executed here: archives are read with the `tar` and `flate2` crates.

use crate::fetch::{self, Fetcher};
use crate::setup::{self, Host, Pin};
use hydo_image::releases::{archive_name, top_dir, Index};
use hydo_image::store::Home;
use hydo_image::FileRef;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

/// The release's asset list: `SHA256SUMS` next to the archives.
pub const SUMS_ASSET: &str = "SHA256SUMS";

/// The install cache of one platform.
pub struct Cache {
    pub home: Home,
    /// `linux` or `mac`: only this platform's archives are fetched.
    pub platform: &'static str,
    /// The releases this hydo knows (the compiled index plus its own version).
    pub index: Index,
    /// This hydo's version.
    pub own_version: String,
    /// Where releases are fetched from (for the messages also when offline).
    pub base: String,
    /// None: offline (`--offline`, `HYDO_OFFLINE`), or the base URL was refused (`base_error`).
    pub fetch: Option<Fetcher>,
    /// Why there is no fetcher although the cache is online (a refused base URL).
    pub base_error: Option<String>,
}

/// An exclusive lock of the cache (released when dropped).
pub struct Lock {
    _f: std::fs::File,
}

fn flock(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: a valid open file descriptor; LOCK_EX blocks until the lock is ours.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        if rc == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn mkdirs(p: &Path) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Make a tree removable (the guest store tree's directories are read-only).
fn writable(dir: &Path) {
    let Ok(md) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if md.file_type().is_symlink() {
        return;
    }
    let m = md.permissions().mode();
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(m | 0o700));
    if md.is_dir() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                writable(&e.path());
            }
        }
    }
}

fn remove_tree(dir: &Path) {
    if dir.exists() || std::fs::symlink_metadata(dir).is_ok() {
        writable(dir);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The path of a tar entry below the archive's top directory `top` (None: not below it, or not
/// a plain relative path).
fn below_top(entry: &Path, top: &str) -> Option<String> {
    let mut parts = Vec::new();
    for c in entry.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            _ => return None,
        }
    }
    if parts.first().map(String::as_str) != Some(top) {
        return None;
    }
    Some(parts[1..].join("/"))
}

/// The `<sha256>  <name>` lines of a SHA256SUMS text, by name.
fn sums_by_name(text: &str) -> Result<BTreeMap<String, String>, String> {
    Ok(setup::parse_sums(text)?
        .into_iter()
        .map(|(s, p)| (p, s))
        .collect())
}

impl Cache {
    /// The cache of the `hydo` command: `cfg` from the environment, `offline` also from
    /// `--offline`.
    pub fn new(
        cfg: &hydo_config::run::Cache,
        platform: &'static str,
        offline: bool,
        opts: Option<fetch::Options>,
    ) -> Cache {
        let base = cfg.release_url.trim_end_matches('/').to_string();
        let (fetch, base_error) = if offline {
            (None, None)
        } else {
            let o = opts.unwrap_or_else(|| fetch::Options::standard(&base));
            match Fetcher::new(o) {
                Ok(f) => (Some(f), None),
                Err(e) => (None, Some(e)),
            }
        };
        Cache {
            home: Home::new(&cfg.home),
            platform,
            index: hydo_image::releases::index().clone(),
            own_version: hydo_image::hydo_version().to_string(),
            base,
            fetch,
            base_error,
        }
    }

    /// Lock the cache (create it first).
    pub fn lock(&self) -> Result<Lock, String> {
        mkdirs(&self.home.root)?;
        let p = self.home.lock_path();
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        flock(&f).map_err(|e| format!("cannot lock {}: {e}", p.display()))?;
        Ok(Lock { _f: f })
    }

    fn url(&self, version: &str, file: &str) -> String {
        fetch::release_url(&self.base, version, file)
    }

    /// The fetcher, or why there is none (offline, or the base URL refused). `sums`: the archive
    /// to put in by hand is verified by the release's `SHA256SUMS` next to it (not pinned).
    fn online(&self, wanted: &str, url: &str, sums: bool) -> Result<&Fetcher, String> {
        if let Some(f) = &self.fetch {
            return Ok(f);
        }
        if let Some(e) = &self.base_error {
            return Err(format!("{wanted} is not in the install cache, and {e}"));
        }
        let next_to = if sums {
            " (with the release's SHA256SUMS in the same directory)"
        } else {
            ""
        };
        Err(format!(
            "{wanted} is not in the install cache ({}), and hydo is offline (--offline or \
             HYDO_OFFLINE): download {url} and run `hydo setup --archive <file>`{next_to}, or run \
             without --offline",
            self.home.store().display()
        ))
    }

    /// The release's `SHA256SUMS` asset: from the cache, else fetched (and kept).
    fn release_sums(&self, version: &str) -> Result<BTreeMap<String, String>, String> {
        let p = self.home.release_sums(version);
        if let Ok(t) = std::fs::read_to_string(&p) {
            return sums_by_name(&t).map_err(|e| format!("{}: {e}", p.display()));
        }
        let url = self.url(version, SUMS_ASSET);
        let both = format!(
            "{} and {url}",
            self.url(version, &archive_name(self.platform, version))
        );
        let f = self.online(&format!("hydo {version}'s {SUMS_ASSET}"), &both, true)?;
        let body = f.get_small(&url, 1 << 20)?;
        let text = String::from_utf8(body).map_err(|_| format!("{url}: not text"))?;
        let by = sums_by_name(&text).map_err(|e| format!("{url}: {e}"))?;
        mkdirs(p.parent().unwrap_or(Path::new(".")))?;
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, &text)
            .and_then(|_| std::fs::rename(&tmp, &p))
            .map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(by)
    }

    /// The sha256 the archive of `version` must have: the index's pin, else the release's
    /// `SHA256SUMS` line (for this hydo's own version only).
    pub fn archive_sha(&self, version: &str) -> Result<String, String> {
        let name = archive_name(self.platform, version);
        match self.index.archive(self.platform, version) {
            Some(a) => {
                if let Some(s) = &a.sha256 {
                    return Ok(s.clone());
                }
            }
            None => {
                return Err(format!(
                    "hydo {} does not know the release {version} (known: {}): install it from its \
                     tarball or with its own hydo",
                    self.own_version,
                    self.index.versions(self.platform).join(", ")
                ))
            }
        }
        let sums = self.release_sums(version)?;
        sums.get(&name)
            .cloned()
            .ok_or_else(|| format!("{} does not list {name}", self.url(version, SUMS_ASSET)))
    }

    /// Where the archive of `version` comes from, in words, without fetching it (for
    /// `hydo setup --check`); Err: it cannot come from anywhere now.
    pub fn plan(&self, version: &str) -> Result<String, String> {
        let name = archive_name(self.platform, version);
        let url = self.url(version, &name);
        let pinned = self
            .index
            .archive(self.platform, version)
            .and_then(|a| a.sha256.clone());
        let is_pinned = pinned.is_some();
        let known_sha = match pinned {
            Some(s) => Some(s),
            None => std::fs::read_to_string(self.home.release_sums(version))
                .ok()
                .and_then(|t| sums_by_name(&t).ok())
                .and_then(|m| m.get(&name).cloned()),
        };
        if let Some(s) = &known_sha {
            if let Some(p) = self.home.get(s) {
                return Ok(format!("{name} in the install cache ({})", p.display()));
            }
        }
        if self.index.archive(self.platform, version).is_none() {
            return Err(self.archive_sha(version).unwrap_err());
        }
        self.online(&name, &url, !is_pinned)?;
        Ok(format!("{name}, fetched from {url}"))
    }

    /// The archive of `version` in the store: from the store, else fetched. Call with the lock
    /// held.
    pub fn archive(&self, version: &str, why: &str) -> Result<PathBuf, String> {
        let sha = self.archive_sha(version)?;
        if let Some(p) = self.home.get(&sha) {
            return Ok(p);
        }
        let name = archive_name(self.platform, version);
        let url = self.url(version, &name);
        let pinned = self
            .index
            .archive(self.platform, version)
            .is_some_and(|a| a.sha256.is_some());
        let f = self.online(&format!("{name} (sha256 {sha})"), &url, !pinned)?;
        let part = self.home.part(&sha);
        f.download(&url, &part, &sha, why)?;
        let _ = std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o444));
        let p = self.home.put_file(&part, &name, &sha)?;
        let _ = std::fs::remove_file(&part);
        Ok(p)
    }

    /// Visit the entries of `version`'s archive below its top directory.
    fn entries(
        &self,
        archive: &Path,
        version: &str,
        mut each: impl FnMut(
            &str,
            &mut tar::Entry<'_, flate2::read::GzDecoder<std::fs::File>>,
        ) -> Result<bool, String>,
    ) -> Result<(), String> {
        let top = top_dir(self.platform, version);
        let f = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
        let mut a = tar::Archive::new(flate2::read::GzDecoder::new(f));
        for e in a
            .entries()
            .map_err(|e| format!("{}: {e}", archive.display()))?
        {
            let mut e = e.map_err(|e| format!("{}: {e}", archive.display()))?;
            let path = e
                .path()
                .map_err(|x| format!("{}: {x}", archive.display()))?;
            let Some(rel) = below_top(&path, &top) else {
                continue;
            };
            if !each(&rel, &mut e)? {
                break;
            }
        }
        Ok(())
    }

    /// A file of `version`'s archive (its path below the top directory) in the store, if its
    /// sha256 is `sha256`. Call with the lock held.
    pub fn member(
        &self,
        version: &str,
        path: &str,
        sha256: &str,
        why: &str,
    ) -> Result<PathBuf, String> {
        if let Some(p) = self.home.get(sha256) {
            return Ok(p);
        }
        let archive = self.archive(version, why)?;
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let mut found = None;
        self.entries(&archive, version, |rel, e| {
            if rel != path || !e.header().entry_type().is_file() {
                return Ok(true);
            }
            found = Some(self.home.put_reader(e, &name, sha256));
            Ok(false)
        })?;
        match found {
            Some(r) => r.map_err(|e| format!("{path} of {}: {e}", archive.display())),
            None => Err(format!("{} has no file {path}", archive.display())),
        }
    }

    /// A guest file with `want`'s sha256: the store, else the newest release of this platform
    /// the index says shipped it (the file is taken out of that release's archive).
    pub fn obtain_kernel(&self, want: &FileRef) -> Result<PathBuf, String> {
        if let Some(p) = self.home.get(&want.sha256) {
            return Ok(p);
        }
        let _lock = self.lock()?;
        if let Some(p) = self.home.get(&want.sha256) {
            return Ok(p);
        }
        let ms = self.index.with_sha256(self.platform, &want.sha256);
        let Some(m) = ms.first() else {
            return Err(format!(
                "no hydo release this hydo knows (the {} releases {}) shipped a file with sha256 \
                 {}: the image was built by a newer hydo or by a development tree",
                self.platform,
                self.index.versions(self.platform).join(", "),
                want.sha256
            ));
        };
        let why = format!(
            "{} (sha256 {}) of hydo {}",
            want.file,
            &want.sha256[..16],
            m.version
        );
        self.member(&m.version, &m.path, &m.sha256, &why)
    }

    /// Write `files` (path, sha256) of `version`'s archive into the release tree `root`,
    /// replacing what is there; each must have its sha256. Returns the archive's description.
    pub fn restore(
        &self,
        root: &Path,
        version: &str,
        files: &[(String, String)],
    ) -> Result<String, String> {
        let _lock = self.lock()?;
        let archive = self.archive(version, "files to restore")?;
        let mut want: BTreeMap<&str, &str> = files
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        self.entries(&archive, version, |rel, e| {
            let Some(sha) = want.get(rel).copied() else {
                return Ok(true);
            };
            if !e.header().entry_type().is_file() {
                return Ok(true);
            }
            let dest = root.join(rel);
            mkdirs(dest.parent().unwrap_or(root))?;
            let tmp = dest.with_file_name(format!(
                ".{}.hydo-restore",
                dest.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ));
            let res = (|| {
                let mut f =
                    std::fs::File::create(&tmp).map_err(|x| format!("{}: {x}", tmp.display()))?;
                let mut buf = vec![0u8; 1 << 16];
                loop {
                    let n = e.read(&mut buf).map_err(|x| format!("{rel}: {x}"))?;
                    if n == 0 {
                        break;
                    }
                    f.write_all(&buf[..n])
                        .map_err(|x| format!("{}: {x}", tmp.display()))?;
                }
                drop(f);
                let got = hydo_image::store::sha256_file_now(&tmp)
                    .map_err(|x| format!("{}: {x}", tmp.display()))?;
                if got != sha {
                    return Err(format!(
                        "{rel} in {} has sha256 {got}, but the release lists {sha}",
                        archive.display()
                    ));
                }
                let mode = e.header().mode().unwrap_or(0o644) & 0o7777;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
                    .map_err(|x| format!("{}: {x}", tmp.display()))?;
                std::fs::rename(&tmp, &dest).map_err(|x| format!("{}: {x}", dest.display()))
            })();
            if res.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            res?;
            want.remove(rel);
            Ok(!want.is_empty())
        })?;
        if let Some((p, _)) = want.iter().next() {
            return Err(format!("{} has no file {p}", archive.display()));
        }
        Ok(archive_name(self.platform, version))
    }

    /// Put a downloaded archive into the store (`hydo setup --archive FILE`): its name says the
    /// release; its sha256 must be the index's pin, or the line of a `SHA256SUMS` in the same
    /// directory (kept as the release's asset list). Returns the store entry.
    pub fn import(&self, file: &Path) -> Result<PathBuf, String> {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some((platform, version)) = hydo_image::releases::parse_archive_name(&name) else {
            return Err(format!(
                "{}: not a release archive name (hydo-<linux|mac>-<version>.tar.gz)",
                file.display()
            ));
        };
        if platform != self.platform {
            return Err(format!(
                "{}: an archive of the {platform} release; this hydo is the {} one",
                file.display(),
                self.platform
            ));
        }
        let _lock = self.lock()?;
        let got = hydo_image::store::sha256_file_now(file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let pinned = match self.index.archive(&platform, &version) {
            None => return Err(self.archive_sha(&version).unwrap_err()),
            Some(a) => a.sha256.clone(),
        };
        let want = match pinned {
            Some(s) => s,
            None => {
                let sums = file.with_file_name(SUMS_ASSET);
                let text = std::fs::read_to_string(&sums).map_err(|e| {
                    format!(
                        "{}: {e}; hydo {version}'s archive is not pinned in this hydo, so the \
                         release's SHA256SUMS must be next to it",
                        sums.display()
                    )
                })?;
                let by = sums_by_name(&text).map_err(|e| format!("{}: {e}", sums.display()))?;
                let s = by
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| format!("{} does not list {name}", sums.display()))?;
                if s == got {
                    let p = self.home.release_sums(&version);
                    mkdirs(p.parent().unwrap_or(Path::new(".")))?;
                    std::fs::write(&p, &text).map_err(|e| format!("{}: {e}", p.display()))?;
                }
                s
            }
        };
        if got != want {
            return Err(format!(
                "{}: sha256 {got}, but hydo {version}'s {name} has {want}; refused",
                file.display()
            ));
        }
        let mut f = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
        self.home.put_reader(&mut f, &name, &want)
    }

    /// Enter the guest files the index lists for `version` from its tree into the store (hard
    /// links when possible), so later versions find them without the network.
    fn store_guest_files(&self, root: &Path, version: &str) {
        for m in self.index.files_of(self.platform, version) {
            let p = root.join(&m.path);
            if p.is_file() {
                let _ = self.home.put_file(&p, m.name(), &m.sha256);
            }
        }
    }

    /// The program to run `version` as: `bin/hydo` when it is this cache's link to that version,
    /// else the version's own `bin/hydo`.
    pub fn program(&self, version: &str) -> PathBuf {
        let link = self.home.bin().join("hydo");
        let target = Path::new("..")
            .join("versions")
            .join(version)
            .join("bin")
            .join("hydo");
        if std::fs::read_link(&link).ok().as_deref() == Some(target.as_path()) {
            link
        } else {
            self.home.version(version).join("bin").join("hydo")
        }
    }

    /// Point `bin/hydo` at `versions/<version>/bin/hydo` (atomically). Ok(Err(line)): left
    /// alone, because `bin/hydo` is a program, not this cache's link (a release installed by
    /// its `install.sh` into the same directory, whose default prefix is `~/.hydo` too).
    fn link(&self, version: &str) -> Result<Result<PathBuf, String>, String> {
        let bin = self.home.bin();
        let link = bin.join("hydo");
        if let Ok(md) = std::fs::symlink_metadata(&link) {
            if !md.file_type().is_symlink() {
                let v = std::fs::read_to_string(self.home.root.join("VERSION"))
                    .map(|v| format!("hydo {}", v.trim()))
                    .unwrap_or_else(|_| "a hydo".into());
                return Ok(Err(format!(
                    "left {} alone: {} holds {v} installed from its tarball; run hydo {version} \
                     as {}",
                    link.display(),
                    self.home.root.display(),
                    self.home.version(version).join("bin/hydo").display()
                )));
            }
        }
        mkdirs(&bin)?;
        let tmp = bin.join(".hydo.tmp");
        let _ = std::fs::remove_file(&tmp);
        let target = Path::new("..")
            .join("versions")
            .join(version)
            .join("bin")
            .join("hydo");
        std::os::unix::fs::symlink(&target, &tmp)
            .and_then(|_| std::fs::rename(&tmp, &link))
            .map_err(|e| format!("{}: {e}", link.display()))?;
        Ok(Ok(link))
    }

    /// `hydo setup --get VERSION`: install VERSION into `versions/<VERSION>` (unpacked, set up,
    /// checked), enter its guest files into the store and point `bin/hydo` at it. `pins` are
    /// this hydo's pins when VERSION is its own (else empty: the archive is pinned in the
    /// index). Returns what was done, one line each.
    pub fn install(
        &self,
        version: &str,
        pins: &[Pin],
        host: &dyn Host,
        p: crate::ctx::Platform,
    ) -> Result<Vec<String>, Vec<String>> {
        let one = |e: String| vec![e];
        if !hydo_image::releases::valid_version(version) {
            return Err(one(format!("{version}: not a version")));
        }
        let u = host.unsupported(p);
        if !u.is_empty() {
            return Err(u);
        }
        let _lock = self.lock().map_err(one)?;
        let dest = self.home.version(version);
        let mut done = Vec::new();
        let intact = dest.join("SHA256SUMS").is_file() && {
            let r = setup::inspect(&dest, p, pins, host);
            r.ok() && r.pending.is_empty()
        };
        if !intact {
            let archive = self
                .archive(version, &format!("hydo {version}"))
                .map_err(one)?;
            let versions = self.home.versions();
            mkdirs(&versions).map_err(one)?;
            let tmp = versions.join(format!(".tmp-{version}-{}", std::process::id()));
            remove_tree(&tmp);
            mkdirs(&tmp).map_err(one)?;
            let res = (|| {
                let f = std::fs::File::open(&archive)
                    .map_err(|e| one(format!("{}: {e}", archive.display())))?;
                let mut a = tar::Archive::new(flate2::read::GzDecoder::new(f));
                a.set_preserve_permissions(true);
                a.set_preserve_mtime(true);
                a.unpack(&tmp)
                    .map_err(|e| one(format!("cannot unpack {}: {e}", archive.display())))?;
                let top = top_dir(self.platform, version);
                let names: Vec<String> = std::fs::read_dir(&tmp)
                    .map_err(|e| one(format!("{}: {e}", tmp.display())))?
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                if names != [top.clone()] {
                    return Err(one(format!(
                        "{} does not hold exactly the directory {top} (it holds {})",
                        archive.display(),
                        names.join(", ")
                    )));
                }
                let root = tmp.join(&top);
                let (_, steps) = setup::setup(&root, p, pins, host)?;
                Ok((root, steps))
            })();
            let (root, steps) = match res {
                Ok(x) => x,
                Err(e) => {
                    remove_tree(&tmp);
                    return Err(e);
                }
            };
            let old = versions.join(format!(".old-{version}-{}", std::process::id()));
            if std::fs::symlink_metadata(&dest).is_ok() {
                std::fs::rename(&dest, &old)
                    .map_err(|e| one(format!("{}: {e}", dest.display())))?;
            }
            std::fs::rename(&root, &dest).map_err(|e| one(format!("{}: {e}", dest.display())))?;
            remove_tree(&old);
            remove_tree(&tmp);
            done.push(format!(
                "unpacked {} into {}",
                archive_name(self.platform, version),
                dest.display()
            ));
            done.extend(steps);
        }
        self.store_guest_files(&dest, version);
        match self.link(version).map_err(one)? {
            Ok(link) => done.push(format!(
                "{} -> {}",
                link.display(),
                dest.join("bin/hydo").display()
            )),
            Err(left) => done.push(left),
        }
        Ok(done)
    }
}

impl hydo_image::kernels::Source for Cache {
    fn obtain(&self, want: &FileRef) -> Result<PathBuf, String> {
        self.obtain_kernel(want)
    }
}

/// [`setup::Assets`] for one version of the cache.
pub struct VersionAssets<'a> {
    pub cache: &'a Cache,
    pub version: String,
}

impl setup::Assets for VersionAssets<'_> {
    fn plan(&self) -> Result<String, String> {
        self.cache.plan(&self.version)
    }

    fn restore(&self, root: &Path, files: &[(String, String)]) -> Result<String, String> {
        self.cache.restore(root, &self.version, files)
    }
}

#[cfg(test)]
pub mod tests;
