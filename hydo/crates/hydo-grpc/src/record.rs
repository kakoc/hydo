//! One line per call event in the history recorder's format (HYDO-HIST, hydo/HISTORIES.md),
//! under one history name (default "grpc"):
//!
//! ```text
//! HYDO-HIST <history> <process> invoke <time> <rpc> <op> <request>
//! HYDO-HIST <history> <process> ok|fail|info <time> <rpc> <op> <CODE> <response-or-message>
//! ```
//!
//! `<process>` is the worker (one operation in flight per process), `<time>` guest nanoseconds
//! since boot (CLOCK_BOOTTIME; deterministic under hydo), `<rpc>` "pkg.Service/Method", `<op>` the
//! operation id (a retry is a new invocation with the same op and request), `<CODE>` the gRPC
//! status name. Bodies are compact protobuf JSON. Every token is percent-encoded: bytes outside
//! '!'..'~', '%' and '@' become %XX; the token "%" alone is the empty string. Lines go to a file
//! (full bodies) and optionally to the guest console (full, or brief: without the bodies).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Mutex;

/// Percent-encodes one token.
pub fn enc(s: &[u8]) -> String {
    if s.is_empty() {
        return "%".into();
    }
    let mut b = String::with_capacity(s.len());
    for &c in s {
        if c <= b' ' || c > b'~' || c == b'%' || c == b'@' {
            b.push_str(&format!("%{c:02X}"));
        } else {
            b.push(c as char);
        }
    }
    b
}

/// Reverses `enc`. Go's `fmt.Sscanf("%02X")` reads one or two hex digits of either case.
pub fn dec(s: &str) -> Result<Vec<u8>, String> {
    if s == "%" {
        return Ok(Vec::new());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        if i + 3 > b.len() {
            return Err(format!("bad escape in {}", crate::gofmt::quote(s)));
        }
        let h = &s[i + 1..i + 3];
        let v = u8::from_str_radix(h, 16).map_err(|_| format!("bad escape in {}", crate::gofmt::quote(s)))?;
        out.push(v);
        i += 3;
    }
    Ok(out)
}

/// One parsed line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    pub process: String,
    pub op: String,
    /// invoke ok fail info
    pub typ: String,
    pub time: String,
    pub rpc: String,
    /// "" for invoke
    pub code: String,
    /// decoded request (invoke) or response/message
    pub body: Vec<u8>,
}

/// Starts every record.
pub const TAG: &str = "HYDO-HIST ";

/// Reads one line of history `h` (None if it is not such a record).
pub fn parse(line: &str, h: &str) -> Option<Event> {
    let i = line.find(TAG)?;
    let f: Vec<&str> = line[i + TAG.len()..].split_ascii_whitespace().collect();
    if f.len() < 6 || f[0] != h {
        return None;
    }
    let mut e = Event {
        process: f[1].into(),
        typ: f[2].into(),
        time: f[3].into(),
        rpc: f[4].into(),
        op: f[5].into(),
        ..Default::default()
    };
    let mut rest = &f[6..];
    if e.typ != "invoke" {
        if rest.is_empty() {
            return None;
        }
        e.code = rest[0].into();
        rest = &rest[1..];
    }
    if let Some(b) = rest.first() {
        e.body = dec(b).ok()?;
    }
    Some(e)
}

/// Guest time: nanoseconds since boot (CLOCK_BOOTTIME on Linux; elsewhere since the process
/// started). Under hydo it is a function of the virtual clock.
pub fn now() -> i64 {
    #[cfg(target_os = "linux")]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime writes one timespec.
        let r = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
        if r != 0 {
            return 0;
        }
        ts.tv_sec * 1_000_000_000 + ts.tv_nsec
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_nanos() as i64
    }
}

/// Console modes.
pub const CONSOLE_OFF: &str = "off";
pub const CONSOLE_BRIEF: &str = "brief";
pub const CONSOLE_FULL: &str = "full";

enum Console {
    None,
    Stderr,
    File(File),
}

struct Sinks {
    file: Option<File>,
    console: Console,
}

/// Appends events.
pub struct Recorder {
    /// the history name (default "grpc")
    pub history: String,
    mode: String,
    sinks: Mutex<Sinks>,
}

impl Recorder {
    /// Opens the calls file (path "" = none) and the console per mode (HYDO_GRPC_CONSOLE
    /// overrides; the device is HYDO_GRPC_CONSOLE_DEV, default /dev/ttyS0, falling back to stderr).
    pub fn open(path: &str, mode: &str) -> Recorder {
        let mut mode = mode.to_string();
        if let Ok(v) = std::env::var("HYDO_GRPC_CONSOLE") {
            if !v.is_empty() {
                mode = v;
            }
        }
        let mut file = None;
        if !path.is_empty() {
            match OpenOptions::new().create(true).append(true).mode(0o644).open(path) {
                Ok(f) => file = Some(f),
                Err(e) => eprintln!("hydo-grpc: cannot open {}: open {}: {}", path, path, crate::io_msg(&e)),
            }
        }
        let mut console = Console::None;
        if mode == CONSOLE_BRIEF || mode == CONSOLE_FULL {
            let dev = std::env::var("HYDO_GRPC_CONSOLE_DEV").ok().filter(|s| !s.is_empty());
            let dev = dev.unwrap_or_else(|| "/dev/ttyS0".into());
            console = if dev == "stderr" {
                Console::Stderr
            } else {
                match OpenOptions::new().append(true).custom_flags(libc::O_NOCTTY).open(&dev) {
                    Ok(f) => Console::File(f),
                    Err(_) => Console::Stderr,
                }
            };
        }
        Recorder { history: "grpc".into(), mode, sinks: Mutex::new(Sinks { file, console }) }
    }

    /// Records the start of an attempt.
    pub fn invoke(&self, process: i64, op: i64, rpc: &str, request: &str) {
        let head = format!("{}{} {} invoke {} {} {}", TAG, self.history, process, now(), rpc, op);
        self.write(&head, &enc(request.as_bytes()));
    }

    /// Records the outcome of an attempt.
    pub fn complete(&self, process: i64, op: i64, kind: &str, rpc: &str, code: &str, body: &str) {
        let head = format!("{}{} {} {} {} {} {} {}", TAG, self.history, process, kind, now(), rpc, op, code);
        self.write(&head, &enc(body.as_bytes()));
    }

    fn write(&self, head: &str, body: &str) {
        let mut s = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        let full = format!("{head} {body}\n");
        if let Some(f) = s.file.as_mut() {
            // one write: O_APPEND keeps the lines of concurrent processes whole
            let _ = f.write_all(full.as_bytes());
        }
        let line = if self.mode == CONSOLE_FULL { full } else { format!("{head}\n") };
        match &mut s.console {
            Console::None => {}
            Console::Stderr => {
                let _ = std::io::stderr().write_all(line.as_bytes());
            }
            Console::File(f) => {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }

    /// Writes an arbitrary line to the console sink (assertion records).
    pub fn raw(&self, line: &str) {
        let mut s = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
        let l = format!("{line}\n");
        match &mut s.console {
            Console::None => {}
            Console::Stderr => {
                let _ = std::io::stderr().write_all(l.as_bytes());
            }
            Console::File(f) => {
                let _ = f.write_all(l.as_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::R;

    #[test]
    fn enc_dec() {
        let mut r = R::new(5);
        for s in ["", "%", "a@b", "a b", "{\"k\":\"v w\"}", "naïve\n\t", "100%"] {
            let e = enc(s.as_bytes());
            assert!(!e.contains([' ', '\n', '\t', '@']) && (s.is_empty() || e != "%"), "{s:?} -> {e:?}");
            assert_eq!(dec(&e).unwrap(), s.as_bytes());
        }
        for _ in 0..1000 {
            let n = r.intn(40);
            let b: Vec<u8> = (0..n).map(|_| r.next_u64() as u8).collect();
            assert_eq!(dec(&enc(&b)).unwrap(), b);
        }
        assert!(dec("%4").is_err());
        assert_eq!(enc("{\"a\":\"b c\"}".as_bytes()), "{\"a\":\"b%20c\"}");
    }

    #[test]
    fn recorder() {
        let dir = std::env::temp_dir().join(format!("hydo-grpc-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("calls");
        let _ = std::fs::remove_file(&p);
        let rec = Recorder { history: "grpc".into(), mode: "off".into(), sinks: Mutex::new(Sinks { file: None, console: Console::None }) };
        drop(rec);
        let rec = {
            let f = OpenOptions::new().create(true).append(true).open(&p).unwrap();
            Recorder { history: "grpc".into(), mode: "off".into(), sinks: Mutex::new(Sinks { file: Some(f), console: Console::None }) }
        };
        assert!(parse("HYDO-HIST kv 1 invoke 5 write k1 v1", "grpc").is_none());
        rec.invoke(3, 17, "pkg.S/M", "{\"a\":\"b c\"}");
        rec.complete(3, 17, "info", "pkg.S/M", "DEADLINE_EXCEEDED", "context deadline exceeded");
        rec.complete(3, 18, "ok", "pkg.S/M", "OK", "");
        let b = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = b.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);
        let e = parse(&format!("console junk {}", lines[0]), "grpc").unwrap();
        assert_eq!((e.process.as_str(), e.op.as_str(), e.typ.as_str(), e.rpc.as_str()), ("3", "17", "invoke", "pkg.S/M"));
        assert_eq!(e.body, b"{\"a\":\"b c\"}");
        let e = parse(lines[1], "grpc").unwrap();
        assert_eq!((e.typ.as_str(), e.code.as_str()), ("info", "DEADLINE_EXCEEDED"));
        assert_eq!(e.body, b"context deadline exceeded");
        let e = parse(lines[2], "grpc").unwrap();
        assert!(e.typ == "ok" && e.body.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
