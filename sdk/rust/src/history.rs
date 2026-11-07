//! Operation histories for hydo's host-side checkers (hydo/HISTORIES.md): record every
//! operation a client performs -- the invocation before the request, the completion after the
//! reply -- and the host checks the whole history after the run (linearizability of a register /
//! CAS object per key, Elle-style list-append). The same lines as the Go recorder
//! (`sdk/go/hydohist`) and the shell helper `hydo_hist`:
//!
//! ```text
//! HYDO-HIST <history> <process> <invoke|ok|fail|info> <time-ns> <f> <arg>...
//! ```
//!
//! ```ignore
//! use hydo_assert::history::{History, enc, ver, value_or_nil};
//! let kv = History::new("kv");
//! let op = kv.invoke(3, "write", &["k1", &enc(v)]);   // process 3, before the request
//! match put("k1", v) {
//!     Ok(rev) => op.ok(&["k1", &ver(v, rev)]),        // the version the store assigned, if any
//!     Err(e) if e.rejected() => op.fail(),             // definitely had no effect
//!     Err(_) => op.info(),                             // a timeout: unknown outcome
//! }
//! let op = kv.invoke(3, "read", &["k1"]);
//! let got = get("k1");
//! op.ok(&["k1", &value_or_nil(got.as_deref())]);
//! ```
//!
//! One record = one line = one write, to `/dev/console` (`HYDO_HIST_OUT=<path>|stderr|off`
//! overrides it; the assertion records keep `HYDO_ASSERT_OUT`). The host orders operations by the
//! position of their records, so record the invocation before sending and the completion after
//! receiving; a process has at most one operation in flight (give each concurrent client its own
//! process number). The time field is guest time since boot in ns, read from `/proc/uptime`
//! (10 ms resolution; `-` where there is none); the checkers do not order by it.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

enum Out {
    Off,
    Stderr,
    File(Mutex<File>),
}

static OUT: OnceLock<Out> = OnceLock::new();

fn out() -> &'static Out {
    OUT.get_or_init(|| {
        let v = std::env::var_os("HYDO_HIST_OUT").unwrap_or_default();
        if v == "off" || cfg!(feature = "noop") {
            return Out::Off;
        }
        if v == "stderr" {
            return Out::Stderr;
        }
        let p = if v.is_empty() { std::ffi::OsString::from("/dev/console") } else { v };
        match OpenOptions::new().append(true).create(true).open(&p) {
            Ok(f) => Out::File(Mutex::new(f)),
            Err(_) => Out::Stderr,
        }
    })
}

fn now_ns() -> String {
    // "12.34 56.78" -> "12340000000"
    match std::fs::read_to_string("/proc/uptime") {
        Ok(s) => match s.split_whitespace().next().and_then(|u| u.split_once('.')) {
            Some((a, b)) if a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit()) && b.len() <= 9 => {
                format!("{}{}{}", a, b, "0".repeat(9 - b.len()))
            }
            _ => "-".into(),
        },
        Err(_) => "-".into(),
    }
}

/// One named history (a check's `history:`).
#[derive(Clone, Debug)]
pub struct History {
    name: String,
}

/// An invoked operation waiting for its completion.
#[derive(Debug)]
pub struct Op {
    h: History,
    proc_: String,
    f: String,
    args: Vec<String>,
}

impl History {
    /// The recorder for history `name` (`[A-Za-z0-9_.-]+`).
    pub fn new(name: &str) -> History {
        History { name: name.to_string() }
    }

    /// Records the invocation of `f(args...)` by process `proc_` (a number or a token without
    /// spaces, e.g. the test SDK's `ctx.id()`); args are tokens (encode free-form values with [`enc`]).
    pub fn invoke(&self, proc_: impl std::fmt::Display, f: &str, args: &[&str]) -> Op {
        let o = Op { h: self.clone(), proc_: proc_.to_string(), f: f.to_string(), args: args.iter().map(|a| a.to_string()).collect() };
        self.record(&o.proc_, "invoke", f, &o.args);
        o
    }

    /// Writes one raw record (for programs that keep their own bookkeeping).
    pub fn record_raw(&self, proc_: impl std::fmt::Display, typ: &str, f: &str, args: &[&str]) {
        let a: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.record(&proc_.to_string(), typ, f, &a);
    }

    fn record(&self, proc_: &str, typ: &str, f: &str, args: &[String]) {
        let o = out();
        if let Out::Off = o {
            return;
        }
        let mut s = format!("HYDO-HIST {} {} {} {} {}", self.name, proc_, typ, now_ns(), f);
        for a in args {
            s.push(' ');
            s.push_str(if a.is_empty() { "%" } else { a });
        }
        s.push('\n');
        match o {
            Out::Off => {}
            Out::Stderr => {
                let _ = std::io::stderr().write_all(s.as_bytes());
            }
            Out::File(f) => {
                if let Ok(mut f) = f.lock() {
                    let _ = f.write_all(s.as_bytes()); // one write: never interleaved
                }
            }
        }
    }
}

impl Op {
    /// A successful completion with the results in the invocation's layout (a read lists
    /// `<key> <value>` pairs, a txn its micro-ops with read results).
    pub fn ok(self, args: &[&str]) {
        let a: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        self.h.record(&self.proc_, "ok", &self.f, &a);
    }
    /// A successful completion that repeats the invocation's arguments (a write, an append).
    pub fn ok_same(self) {
        self.h.record(&self.proc_, "ok", &self.f, &self.args);
    }
    /// The operation definitely had no effect.
    pub fn fail(self) {
        self.h.record(&self.proc_, "fail", &self.f, &self.args);
    }
    /// The outcome is unknown: it may take effect at any later time, or never.
    pub fn info(self) {
        self.h.record(&self.proc_, "info", &self.f, &self.args);
    }
}

/// A value as one token: `%`, `@`, the list / txn separators `,[]:`, DEL and every byte up to
/// space become `%XX`; all other bytes (non-ASCII text included) stay, as in Go's `Enc`. The
/// empty string is `%`, the string `nil` is `%6Eil` (`nil` = absent).
pub fn enc(s: &str) -> String {
    if s.is_empty() {
        return "%".into();
    }
    if s == "nil" {
        return "%6Eil".into();
    }
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = ch as u32;
        if ch == '%' || ch == '@' || c <= 0x20 || c == 0x7f || ",[]:".contains(ch) {
            o.push_str(&format!("%{:02X}", c));
        } else {
            o.push(ch);
        }
    }
    o
}

/// Value `v` (encoded) with the version the store assigned it: `v@83`.
pub fn ver(v: &str, version: u64) -> String {
    format!("{}@{}", enc(v), version)
}

/// `enc(v)` if present, else `nil`.
pub fn value_or_nil(v: Option<&str>) -> String {
    v.map(enc).unwrap_or_else(|| "nil".into())
}

/// A list value `[e1,e2,...]` (list-append reads).
pub fn list(elems: &[&str]) -> String {
    format!("[{}]", elems.iter().map(|e| enc(e)).collect::<Vec<_>>().join(","))
}

/// List-append txn micro-ops: `a:<k>:<e>`, `r:<k>` (invocation), `r:<k>:[...]` (completion).
pub fn append_mop(k: &str, e: &str) -> String {
    format!("a:{}:{}", enc(k), enc(e))
}
pub fn read_mop(k: &str) -> String {
    format!("r:{}", enc(k))
}
pub fn read_result_mop(k: &str, l: &[&str]) -> String {
    format!("r:{}:{}", enc(k), list(l))
}
