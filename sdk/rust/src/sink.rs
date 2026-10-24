//! Where records go: the guest console, opened once, as the Go SDK does.
//!
//! `HYDO_ASSERT_OUT` unset or empty: `/dev/ttyS0`; a path: that file (appended); `stderr`:
//! standard error (plain logging outside hydo); `off`: nothing. If the file cannot be opened
//! the records go to standard error.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::OnceLock;

enum Sink {
    Off,
    Stderr,
    File(File),
}

static SINK: OnceLock<Sink> = OnceLock::new();

#[cfg(all(any(target_os = "linux", target_os = "android"), not(any(target_arch = "mips", target_arch = "mips64", target_arch = "sparc64"))))]
const O_NOCTTY: i32 = 0o400;
#[cfg(any(target_os = "macos", target_os = "ios"))]
const O_NOCTTY: i32 = 0x20000;
#[cfg(any(target_os = "freebsd", target_os = "netbsd", target_os = "openbsd", target_os = "dragonfly"))]
const O_NOCTTY: i32 = 0x8000;
#[cfg(not(any(
    all(any(target_os = "linux", target_os = "android"), not(any(target_arch = "mips", target_arch = "mips64", target_arch = "sparc64"))),
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
const O_NOCTTY: i32 = 0;

fn open(path: &std::ffi::OsStr) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOCTTY: never make the console the controlling terminal of the caller
        o.custom_flags(O_NOCTTY);
    }
    #[cfg(not(unix))]
    let _ = O_NOCTTY;
    o.open(path)
}

fn sink() -> &'static Sink {
    SINK.get_or_init(|| {
        let v = std::env::var_os("HYDO_ASSERT_OUT").unwrap_or_default();
        if v == "off" {
            return Sink::Off;
        }
        if v == "stderr" {
            return Sink::Stderr;
        }
        let p = if v.is_empty() { std::ffi::OsString::from("/dev/ttyS0") } else { v };
        match open(&p) {
            Ok(f) => Sink::File(f),
            Err(_) => Sink::Stderr,
        }
    })
}

/// Writes one complete line (which must end in `\n`) with a single write, so a record is
/// never interleaved with other console output.
pub(crate) fn write_line(line: &[u8]) {
    if cfg!(feature = "noop") {
        return;
    }
    match sink() {
        Sink::Off => {}
        Sink::Stderr => {
            let _ = std::io::stderr().write_all(line);
        }
        Sink::File(f) => {
            let _ = (&*f).write_all(line);
        }
    }
}

/// True when records are written somewhere (not `off`, not the `noop` feature).
pub(crate) fn active() -> bool {
    !cfg!(feature = "noop") && !matches!(sink(), Sink::Off)
}
