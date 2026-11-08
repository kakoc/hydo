//! History records for hydo's host-side checkers (hydo/HISTORIES.md): the same lines as the SDKs'
//! recorders, `HYDO-HIST <history> <process> <invoke|ok|fail|info> <time-ns> <f> <arg>...`, one
//! line per write, to `/dev/console` (`HYDO_HIST_OUT=<path>|stderr|off`). An operation is
//! recorded into one history or several (a projection: the same operation, some of its parts
//! left out), each record written whole before the request and after the answer.

use std::io::Write;

fn now_ns() -> String {
    match std::fs::read_to_string("/proc/uptime") {
        Ok(s) => match s.split_whitespace().next().and_then(|u| u.split_once('.')) {
            Some((a, b))
                if a.bytes().all(|c| c.is_ascii_digit())
                    && b.bytes().all(|c| c.is_ascii_digit())
                    && b.len() <= 9 =>
            {
                format!("{a}{b}{}", "0".repeat(9 - b.len()))
            }
            _ => "-".into(),
        },
        Err(_) => "-".into(),
    }
}

fn emit(line: &str) {
    let v = std::env::var("HYDO_HIST_OUT").unwrap_or_default();
    if v == "off" {
        return;
    }
    if v == "stderr" {
        let _ = std::io::stderr().write_all(line.as_bytes());
        return;
    }
    let p = if v.is_empty() {
        "/dev/console".to_string()
    } else {
        v
    };
    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&p)
    {
        Ok(mut f) => {
            let _ = f.write_all(line.as_bytes());
        }
        Err(_) => {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }
}

/// `enc` of hydo_assert::history: one token per value.
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
            o.push_str(&format!("%{c:02X}"));
        } else {
            o.push(ch);
        }
    }
    o
}

pub fn append_mop(k: u64, e: u64) -> String {
    format!("a:{k}:{e}")
}
pub fn read_mop(k: u64) -> String {
    format!("r:{k}")
}
pub fn read_result_mop(k: u64, l: &[String]) -> String {
    format!(
        "r:{k}:[{}]",
        l.iter().map(|e| enc(e)).collect::<Vec<_>>().join(",")
    )
}

/// One record.
pub fn record(history: &str, process: &str, typ: &str, f: &str, args: &[String]) {
    let mut line = format!("HYDO-HIST {history} {process} {typ} {} {f}", now_ns());
    for a in args {
        line.push(' ');
        line.push_str(if a.is_empty() { "%" } else { a });
    }
    line.push('\n');
    emit(&line);
}

/// Which micro-operations of an operation a history records.
pub type Keep = fn(&str) -> bool;
/// A history and what it keeps.
pub type View = (String, Keep);

/// An operation recorded into several histories, each with its own view of the arguments
/// (`None`: this history does not get the operation at all).
pub struct Op {
    f: String,
    process: String,
    /// (history, keep: which arguments it records, the invocation's arguments it recorded;
    /// fail and info repeat them, as the SDKs' recorders do)
    views: Vec<(String, Keep, Vec<String>)>,
}

pub fn all(_: &str) -> bool {
    true
}
/// A real-time projection of a transaction a replica served: its appends only (they execute on
/// the primary within the request); its reads may be stale.
pub fn appends_only(m: &str) -> bool {
    m.starts_with("a:")
}

impl Op {
    pub fn invoke(views: Vec<View>, process: &str, f: &str, args: &[String]) -> Op {
        let mut kept = Vec::new();
        for (h, keep) in views {
            let a: Vec<String> = args.iter().filter(|x| keep(x)).cloned().collect();
            if a.is_empty() && f == "txn" {
                continue;
            }
            record(&h, process, "invoke", f, &a);
            kept.push((h, keep, a));
        }
        Op {
            f: f.into(),
            process: process.into(),
            views: kept,
        }
    }
    pub fn ok(self, args: &[String]) {
        for (h, keep, _) in &self.views {
            let a: Vec<String> = args.iter().filter(|x| keep(x)).cloned().collect();
            record(h, &self.process, "ok", &self.f, &a);
        }
    }
    pub fn fail(self) {
        for (h, _, a) in &self.views {
            record(h, &self.process, "fail", &self.f, a);
        }
    }
    pub fn info(self) {
        for (h, _, a) in &self.views {
            record(h, &self.process, "info", &self.f, a);
        }
    }
}
