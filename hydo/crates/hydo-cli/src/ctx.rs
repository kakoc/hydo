//! What every command shares: the platform, the release root and its paths, and the small helpers
//! the Python CLIs had (die, take, the subprocess environment, timestamps).

use std::path::{Path, PathBuf};

/// The platform: which engine runs guests and which images `hydo build` makes. Every command
/// behaves the same on both (M107); only what a platform cannot do differs (`hydo module` and
/// the udev rule exist on Linux only, arm64 images are built on the Mac only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// x86_64 Linux with VT-x: the hvx engine (the Linux release).
    Linux,
    /// Apple Silicon: the hvm engine (the Mac release).
    Mac,
}

impl Platform {
    /// The platform this binary was built for. `HYDO_CLI_PLATFORM=linux|mac` overrides it; the
    /// CLI test uses that to check the other platform's commands that need no engine.
    pub fn detect(cli: &hydo_config::run::Cli) -> Platform {
        match cli.platform {
            Some(hydo_config::run::Platform::Linux) => Platform::Linux,
            Some(hydo_config::run::Platform::Mac) => Platform::Mac,
            None if cfg!(target_os = "macos") => Platform::Mac,
            None => Platform::Linux,
        }
    }
}

pub const OUT: &str = "hydo-out";
pub const PROJECT_OUT: &str = ".hydo";

/// The release root and the paths under it (the release tree mirrors the repository).
pub struct Ctx {
    pub platform: Platform,
    pub root: PathBuf,
    /// The configuration, read once from the environment in `main` (`hydo_config`, M144).
    pub config: hydo_config::Config,
    /// Linux: run x86_64 images on the KVM engine (`--backend kvm` or `HYDO_BACKEND=kvm`)
    /// instead of hvx.
    pub kvm: bool,
    /// Installed by `cargo install` (no release around the program): the root is this version's
    /// directory of the install cache ([`installed_root`]), which `hydo setup --get` fills.
    pub cargo_installed: bool,
    /// Built in a checkout (`<checkout>/target/.../hydo`, no release around the program): the
    /// checkout. Such a hydo takes its release files from the install cache as a cargo-installed
    /// one does (`cargo_installed` is true too), and `hydo doctor` says so first.
    pub checkout: Option<PathBuf>,
}

impl Ctx {
    pub fn new(config: hydo_config::Config) -> Ctx {
        // the release root: <root>/bin/hydo, symlinks resolved (the Python CLIs used realpath(__file__))
        let exe = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .unwrap_or_else(|_| PathBuf::from("bin/hydo"));
        let root = exe
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        crate::module::set_host_root(config.cli.host_root.clone());
        let checkout = checkout_of(&exe, &root);
        let installed = installed_root(&root, &config.cli.cache.home).or_else(|| {
            checkout
                .as_ref()
                .map(|_| version_root(&config.cli.cache.home))
        });
        let cargo_installed = installed.is_some();
        let root = installed.unwrap_or(root);
        let platform = Platform::detect(&config.cli);
        let kvm = platform == Platform::Linux && config.kvm.backend.as_deref() == Some("kvm");
        Ctx {
            platform,
            root,
            config,
            kvm,
            cargo_installed,
            checkout,
        }
    }

    /// Choose the engine from `--backend hvx|kvm` (removed from `args`); the default is
    /// `HYDO_BACKEND`, else hvx. The Mac has one engine.
    pub fn take_backend(&mut self, args: &mut Vec<String>) {
        let Some(b) = take(args, "--backend") else {
            if let Some(b) = self.config.kvm.backend.as_deref() {
                if hydo_config::kvm::Backend::parse(b).is_none() {
                    die(&format!("HYDO_BACKEND={b}: expected hvx or kvm"));
                }
            }
            return;
        };
        match (self.platform, hydo_config::kvm::Backend::parse(&b)) {
            (Platform::Linux, Some(hydo_config::kvm::Backend::Kvm)) => self.kvm = true,
            (Platform::Linux, Some(hydo_config::kvm::Backend::Hvx)) => self.kvm = false,
            (Platform::Mac, _) => die("--backend: the Mac has one engine (hvm)"),
            (_, None) => die(&format!("--backend {b}: expected hvx or kvm")),
        }
    }

    /// The engine's name in run documents (`hvx`, `kvm` or `hvm`).
    pub fn backend_name(&self) -> &'static str {
        match (self.platform, self.kvm) {
            (Platform::Mac, _) => "hvm",
            (Platform::Linux, true) => "kvm",
            (Platform::Linux, false) => "hvx",
        }
    }

    pub fn p(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for part in rel.split('/') {
            p.push(part);
        }
        p
    }

    pub fn onboard(&self) -> PathBuf {
        self.p("hydo/onboard")
    }

    pub fn guest(&self) -> PathBuf {
        self.p("guest")
    }

    pub fn libexec(&self) -> PathBuf {
        self.p("libexec")
    }
}

/// The release root of a `hydo` that `cargo install` put into `<prefix>/bin` (Milestone 169,
/// docs/PUBLISHING.md): `<cache home>/versions/<this hydo's version>`, the directory
/// `hydo setup --get` fills with this version's release (Milestone 162), whether it exists yet or
/// not (until it does, `hydo doctor` names what is missing). Recognised by cargo's install record
/// `.crates2.json` in the prefix naming the package `hydo`, and only when the prefix is not an
/// unpacked release itself (no `SHA256SUMS`); every other layout (a release, a test's
/// release-like root, a tree's `target/`) keeps the program's own parent's parent.
pub fn installed_root(prefix: &Path, cache_home: &Path) -> Option<PathBuf> {
    if prefix.join("SHA256SUMS").exists() {
        return None;
    }
    let record = std::fs::read_to_string(prefix.join(".crates2.json")).ok()?;
    if !record.contains("\"hydo ") {
        return None;
    }
    Some(version_root(cache_home))
}

/// This hydo version's directory of the install cache (`<cache home>/versions/<version>`).
fn version_root(cache_home: &Path) -> PathBuf {
    hydo_image::store::Home::new(cache_home.to_path_buf()).version(hydo_image::hydo_version())
}

/// The checkout a `hydo` was built in, when it runs from that checkout's cargo target directory
/// (`<checkout>/target/release/hydo`, `<checkout>/target/<triple>/release/hydo`) and no release is
/// around it (`root`, the program's parent's parent, has no `SHA256SUMS`). A checkout is a
/// directory with `Cargo.toml`, `hydo/VERSION` and `hydo/crates/hydo-cli`. Such a hydo has none of
/// a release's files (guest kernels, engine programs, tools; the TigerBeetle onboarding found
/// `hydo doctor` failing ten checks and `hydo build` naming version 0.0.0), so it uses the install
/// cache's copy of its own version's release, as a cargo-installed hydo does (Milestone 169).
/// A copy of the program in a release-like prefix (deploy-metal.sh, the rebaseline prefix, the CLI
/// test) is not inside a checkout's target directory and keeps its own root.
pub fn checkout_of(exe: &Path, root: &Path) -> Option<PathBuf> {
    if root.join("SHA256SUMS").exists() {
        return None;
    }
    exe.ancestors()
        .filter(|a| a.file_name().is_some_and(|n| n == "target"))
        .filter_map(Path::parent)
        .find(|c| {
            c.join("Cargo.toml").is_file()
                && c.join("hydo").join("VERSION").is_file()
                && c.join("hydo").join("crates").join("hydo-cli").is_dir()
        })
        .map(Path::to_path_buf)
}

/// `hydo: <msg>` on stderr and exit with status 1.
pub fn die(msg: &str) -> ! {
    die_rc(msg, 1)
}

pub fn die_rc(msg: &str, rc: i32) -> ! {
    eprintln!("hydo: {msg}");
    std::process::exit(rc)
}

/// Remove `flag VALUE` from args and return VALUE (dies without a value).
pub fn take(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    if i + 1 >= args.len() {
        die(&format!("{flag} needs a value"));
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

/// `os.path.abspath`: absolute and normalised, without resolving symlinks.
pub fn abspath(p: &Path) -> PathBuf {
    let base = if p.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let mut out = PathBuf::new();
    for c in base.join(p).components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `os.path.expanduser("~/...")`.
pub fn home_join(rel: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Path::new(&home).join(rel)
}

/// `time.strftime(fmt)` in local time.
pub fn strftime(fmt: &str) -> String {
    // SAFETY: localtime_r and strftime write into buffers we own; time(NULL) has no side effects.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let f = std::ffi::CString::new(fmt).unwrap_or_default();
        let mut buf = [0u8; 128];
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), f.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// `os.access(p, R_OK | W_OK)` and friends.
pub fn access(p: &Path, mode: libc::c_int) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), mode) == 0 }
}

pub fn getuid() -> u32 {
    // SAFETY: no arguments, no side effects.
    unsafe { libc::getuid() }
}

/// `shutil.which(name) is not None`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file() && access(p, libc::X_OK))
}

/// Read a text file as Python's `open(p, errors="replace").read()` does (universal newlines).
pub fn read_text(p: &Path) -> std::io::Result<String> {
    let b = std::fs::read(p)?;
    Ok(universal_newlines(&String::from_utf8_lossy(&b)))
}

pub fn universal_newlines(s: &str) -> String {
    if !s.contains('\r') {
        return s.to_string();
    }
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Python's `sorted(glob.glob(pattern))` for a pattern `<dir>/<prefix>*<suffix>` (no other wildcards).
pub fn glob_simple(dir: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    !n.starts_with('.')
                        && n.starts_with(prefix)
                        && n.ends_with(suffix)
                        && n.len() >= prefix.len() + suffix.len()
                })
                .map(|n| dir.join(n))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Join a relative directory the way Python's `os.path.join(OUT, "explore")` prints it.
pub fn pjoin(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

#[cfg(test)]
mod tests {
    use super::checkout_of;
    use std::path::Path;

    fn checkout(at: &Path) {
        std::fs::create_dir_all(at.join("hydo/crates/hydo-cli")).unwrap();
        std::fs::write(at.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(at.join("hydo/VERSION"), "0.1.0\n").unwrap();
    }

    /// A hydo in a checkout's target directory (also a cross build's) is a checkout build; a copy
    /// in a release-like prefix, a release, or a target directory outside a checkout is not.
    #[test]
    fn checkout_builds_are_recognised() {
        let d = std::env::temp_dir().join(format!("hydo-ctx-checkout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let t = d.join("tree");
        checkout(&t);
        let exe = t.join("target/release/hydo");
        assert_eq!(checkout_of(&exe, &t.join("target")), Some(t.clone()));
        let cross = t.join("target/x86_64-unknown-linux-musl/release/hydo");
        assert_eq!(
            checkout_of(&cross, &t.join("target/x86_64-unknown-linux-musl")),
            Some(t.clone())
        );
        // a release unpacked inside the checkout keeps its own root
        let rel = t.join("target/rel");
        std::fs::create_dir_all(&rel).unwrap();
        std::fs::write(rel.join("SHA256SUMS"), "").unwrap();
        assert_eq!(checkout_of(&rel.join("bin/hydo"), &rel), None);
        // a prefix with the program copied in (deploy-metal.sh, the rebaseline prefix)
        let p = d.join("prefix");
        assert_eq!(checkout_of(&p.join("bin/hydo"), &p), None);
        // a target directory whose parent is not a hydo checkout
        let other = d.join("other");
        std::fs::create_dir_all(other.join("target/release")).unwrap();
        std::fs::write(other.join("Cargo.toml"), "").unwrap();
        assert_eq!(
            checkout_of(&other.join("target/release/hydo"), &other.join("target")),
            None
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
