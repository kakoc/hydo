//! What every command shares: the platform, the release root and its paths, and the small helpers
//! the Python CLIs had (die, take, the subprocess environment, timestamps).

use std::path::{Path, PathBuf};

/// The platform's behaviour. Each release keeps its own CLI's behaviour where the two Python CLIs
/// differed (docs/HISTORY.md, M101 lists every difference).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// x86_64 Linux with VT-x: the hvx engine (the Linux release).
    Linux,
    /// Apple Silicon: the hvm engine (the Mac release).
    Mac,
}

impl Platform {
    /// The platform this binary was built for. `HYDO_CLI_PLATFORM=linux|mac` overrides it; the
    /// equivalence tests use that to compare the other release's commands that need no engine.
    pub fn detect() -> Platform {
        match std::env::var("HYDO_CLI_PLATFORM").as_deref() {
            Ok("linux") => Platform::Linux,
            Ok("mac") => Platform::Mac,
            _ if cfg!(target_os = "macos") => Platform::Mac,
            _ => Platform::Linux,
        }
    }
}

pub const OUT: &str = "hydo-out";
pub const PROJECT_OUT: &str = ".hydo";

/// The release root and the paths under it (the release tree mirrors the repository).
pub struct Ctx {
    pub platform: Platform,
    pub root: PathBuf,
}

impl Ctx {
    pub fn new() -> Ctx {
        // the release root: <root>/bin/hydo, symlinks resolved (the Python CLIs used realpath(__file__))
        let exe = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .unwrap_or_else(|_| PathBuf::from("bin/hydo"));
        let root = exe
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Ctx {
            platform: Platform::detect(),
            root,
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

/// `hydo: <msg>` on stderr and exit (the Python CLIs' die()).
pub fn die(msg: &str) -> ! {
    die_rc(msg, 1)
}

pub fn die_rc(msg: &str, rc: i32) -> ! {
    eprintln!("hydo: {msg}");
    std::process::exit(rc)
}

/// Remove `flag VALUE` from args and return VALUE (the Linux CLI's take(): dies without a value).
pub fn take(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    if i + 1 >= args.len() {
        die(&format!("{flag} needs a value"));
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

/// The Mac CLI's `args.index(flag); v = args[i + 1]; del args[i:i + 2]` (a missing value was an
/// IndexError there; here it is the Linux CLI's message).
pub fn take_mac(args: &mut Vec<String>, flag: &str) -> Option<String> {
    take(args, flag)
}

/// The Python interpreter the remaining Python parts run with (`HYDO_PYTHON`, default python3).
pub fn python() -> String {
    std::env::var("HYDO_PYTHON").unwrap_or_else(|_| "python3".into())
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
