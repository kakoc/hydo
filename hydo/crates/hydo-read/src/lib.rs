//! hydo-read and hydo-check: ready-made store readers and set-style checks for hydo invariants
//! (hydo/onboard/MANIFEST.md, "Reading the stores"). A port of the Go program of M88 that prints
//! the same bytes: the same JSON Lines, the same order, the same explanation lines and exit
//! statuses.
//!
//!   hydo-read  <store> <args...>     print what is in a table / on a topic / under a key, as
//!                                    JSON Lines, sorted, one record per line
//!   hydo-check <check> <options...>  compare such readings: subset, equal, exactly-once,
//!                                    disjoint, no-duplicates, count, never-wrong-value
//!
//! One static binary; it runs as hydo-check when called by that name (a symlink) or as
//! `hydo-read check ...`. Exit status, for both: 0 = ok / the property holds, 1 = the property
//! is violated (hydo-check only), 2 = could not read a store or bad usage.
//!
//! The store clients are written here, in plain Rust: the MySQL text protocol, the PostgreSQL
//! simple-query protocol, a Kafka client for Metadata, ListOffsets and Fetch, RESP2 for Redis and
//! etcd's v3 JSON gateway over HTTP/1.1.

pub mod check;
pub mod etcd;
pub mod gofmt;
pub mod json;
pub mod kafka;
pub mod kerr;
pub mod mysql;
pub mod net;
pub mod pg;
pub mod redis;
pub mod url;

use json::{Obj, Val};
use std::io::Write;
use std::time::Duration;

pub const EXIT_OK: i32 = 0;
pub const EXIT_VIOLATED: i32 = 1;
pub const EXIT_CANNOT: i32 = 2;

/// One row / message / key: an object, or null (a `null` line of a jsonl file).
pub type Record = Val;

/// A store read failed (cannotRead) or the command line is wrong (usageError), with the Go
/// program's message text.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Cannot(String),
    Usage(String),
    /// A plain error (Go's fmt.Errorf): printed as it is.
    Plain(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cannot(s) => write!(f, "could not read: {s}"),
            Error::Usage(s) => write!(f, "usage: {s}"),
            Error::Plain(s) => write!(f, "{s}"),
        }
    }
}

pub fn usage(s: impl Into<String>) -> Error {
    Error::Usage(s.into())
}
pub fn cannot(s: impl Into<String>) -> Error {
    Error::Cannot(s.into())
}

pub type Res<T> = Result<T, Error>;

/// Raw text of an argument (arguments are bytes on Linux).
pub fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `%q` of a byte string.
pub fn q(b: &[u8]) -> String {
    gofmt::quote(b)
}

pub const READ_USAGE: &str = r#"hydo-read <store> <args...> [--timeout <s>] [--base64]
  mysql    <dsn> <query>                 rows as {"<column>": "<value>"|null}
  postgres <dsn> <query>                 (dsn: user:pw@tcp(h:p)/db or mysql://user@h:p/db;
                                          postgres://user@h:p/db?sslmode=disable)
  kafka    <bootstrap> <topic>           every record from the log start to the high
                                          watermark: partition, offset, key, value, timestamp_ms
  redis    <addr> get <key>...           {key, value|null}
  redis    <addr> scan <pattern>         {key, type, value} for every matching key
  redis    <addr> hgetall|smembers|lrange|zrange <key>
  redis    <addr> cmd <word>...          one raw command, {reply}
  etcd     <endpoint> get <key> [--prefix] [--serializable]   {key, value, create_revision,
                                          mod_revision, version}
  etcd     <endpoint> status             {endpoint, revision, raft_index, raft_term}
  file     <path>                        one {line} per non-empty line
  jsonl    <path>                        hydo-read output saved earlier
  range    <from> <to>                   {n} for n in [from, to)
  hydo     acked|failed|told-failed|unknown   ids from $HYDO_ACKED / $HYDO_FAILED / $HYDO_OUTCOMES
Output: JSON Lines, keys sorted, lines sorted bytewise (kafka, lrange and zrange keep the
store's order). Exit 0 ok, 2 could not read the store or bad usage."#;

/// The options every store takes.
#[derive(Clone, Debug)]
pub struct ReadOpts {
    pub timeout: Duration,
    pub base64: bool,
}

/// Go's time.Duration(s * float64(time.Second)).
pub fn secs(s: f64) -> Duration {
    let ns = s * 1e9;
    if ns >= u64::MAX as f64 {
        Duration::from_secs(u64::MAX / 4)
    } else if ns <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_nanos(ns as u64)
    }
}

/// Removes --timeout/--base64 from args (anywhere).
pub fn split_read_opts(args: &[Vec<u8>]) -> Res<(Vec<Vec<u8>>, ReadOpts)> {
    let mut o = ReadOpts {
        timeout: Duration::from_secs(30),
        base64: false,
    };
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_slice() {
            b"--timeout" => {
                if i + 1 >= args.len() {
                    return Err(usage("--timeout needs seconds"));
                }
                match gofmt::sscan_float(&args[i + 1]) {
                    Some(s) if s > 0.0 => o.timeout = secs(s),
                    _ => {
                        return Err(usage(format!(
                            "--timeout {}: want seconds > 0",
                            q(&args[i + 1])
                        )))
                    }
                }
                i += 1;
            }
            b"--base64" => o.base64 = true,
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    Ok((rest, o))
}

/// `hydo-read <args>`: prints the records, returns the exit status.
pub fn read_main(args: &[Vec<u8>], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.is_empty() || args[0] == b"-h" || args[0] == b"--help" {
        let _ = writeln!(err, "{READ_USAGE}");
        return if args.is_empty() {
            EXIT_CANNOT
        } else {
            EXIT_OK
        };
    }
    match read_store(args) {
        Err(e) => {
            let _ = err.write_all(b"hydo-read: ");
            let _ = err.write_all(&args[0]);
            let _ = writeln!(err, ": {e}");
            if matches!(e, Error::Usage(_)) {
                let _ = writeln!(err, "{READ_USAGE}");
            }
            EXIT_CANNOT
        }
        Ok((recs, ordered)) => {
            let mut w = Vec::new();
            for l in encode_all(&recs, ordered) {
                w.extend_from_slice(&l);
                w.push(b'\n');
            }
            let _ = out.write_all(&w);
            let _ = out.flush();
            EXIT_OK
        }
    }
}

pub fn encode_all(recs: &[Record], ordered: bool) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = recs.iter().map(json::encode).collect();
    if !ordered {
        lines.sort();
    }
    lines
}

/// A record from (key, value) pairs.
pub fn rec(fields: Vec<(&str, Val)>) -> Record {
    let mut o = Obj::new();
    for (k, v) in fields {
        o.insert(k.as_bytes().to_vec(), v);
    }
    Val::Obj(o)
}

fn arg_str(a: &[u8]) -> String {
    lossy(a)
}

/// Runs one store reader; `ordered` = the records are in the store's own order.
pub fn read_store(args: &[Vec<u8>]) -> Res<(Vec<Record>, bool)> {
    let (args, o) = split_read_opts(args)?;
    if args.is_empty() {
        return Err(usage("no store"));
    }
    let store = args[0].as_slice();
    let a = &args[1..];
    match store {
        b"mysql" | b"postgres" => {
            let name = arg_str(store);
            if a.len() != 2 {
                return Err(usage(format!("{name} <dsn> <query> (quote the query)")));
            }
            let r = if store == b"mysql" {
                mysql::read(&a[0], &a[1], &o)?
            } else {
                pg::read(&a[0], &a[1], &o)?
            };
            Ok((r, false))
        }
        b"kafka" => {
            if a.len() != 2 {
                return Err(usage("kafka <bootstrap> <topic>"));
            }
            Ok((kafka::read(&a[0], &a[1], &o)?, true))
        }
        b"redis" => redis::read(a, &o),
        b"etcd" => Ok((etcd::read(a, &o)?, false)),
        b"file" => {
            if a.len() != 1 {
                return Err(usage("file <path>"));
            }
            Ok((read_file(&a[0])?, false))
        }
        b"jsonl" => {
            if a.len() != 1 {
                return Err(usage("jsonl <path>"));
            }
            Ok((read_jsonl(&a[0])?, true))
        }
        b"range" => {
            if a.len() != 2 {
                return Err(usage("range <from> <to>"));
            }
            let mut joined = a[0].clone();
            joined.push(b' ');
            joined.extend_from_slice(&a[1]);
            match gofmt::sscan_two_ints(&joined) {
                Some((from, to)) if to >= from && to.wrapping_sub(from) <= 10_000_000 => {
                    let mut r = Vec::new();
                    let mut n = from;
                    while n < to {
                        r.push(rec(vec![("n", Val::Int(n))]));
                        n += 1;
                    }
                    Ok((r, true))
                }
                _ => Err(usage(format!(
                    "range {} {}: two integers, from <= to",
                    lossy(&a[0]),
                    lossy(&a[1])
                ))),
            }
        }
        b"hydo" => {
            if a.len() != 1 {
                return Err(usage("hydo acked|failed|told-failed|unknown"));
            }
            Ok((read_hydo(&a[0])?, false))
        }
        _ => Err(usage(format!("unknown store {}", q(store)))),
    }
}

fn os_path(p: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(p))
}

/// os.ReadFile with Go's error text ("open <path>: no such file or directory").
pub fn read_os_file(path: &[u8]) -> Res<Vec<u8>> {
    let p = os_path(path);
    let shown = lossy(path);
    let mut f = std::fs::File::open(&p)
        .map_err(|e| cannot(format!("open {shown}: {}", gofmt::errno_text(&e))))?;
    let mut b = Vec::new();
    std::io::Read::read_to_end(&mut f, &mut b)
        .map_err(|e| cannot(format!("read {shown}: {}", gofmt::errno_text(&e))))?;
    Ok(b)
}

/// The lines of a file as strings.Split(b, "\n") gives them.
fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    b.split(|&c| c == b'\n').collect()
}

pub fn read_file(path: &[u8]) -> Res<Vec<Record>> {
    let b = read_os_file(path)?;
    let mut r = Vec::new();
    for l in split_lines(&b) {
        let mut l = l;
        while let Some(x) = l.strip_suffix(b"\r") {
            l = x;
        }
        if gofmt::trim_space(l).is_empty() {
            continue;
        }
        r.push(rec(vec![("line", Val::str(l))]));
    }
    Ok(r)
}

pub fn read_jsonl(path: &[u8]) -> Res<Vec<Record>> {
    let b = read_os_file(path)?;
    let mut r = Vec::new();
    for (i, l) in split_lines(&b).into_iter().enumerate() {
        if gofmt::trim_space(l).is_empty() {
            continue;
        }
        match json::decode_record_line(l) {
            Err(e) => {
                return Err(cannot(format!("{} line {}: {}", lossy(path), i + 1, e.0)));
            }
            Ok(None) => r.push(Val::Null),
            Ok(Some(mut o)) => {
                for v in o.values_mut() {
                    if let Val::Num(n) = v {
                        *v = match n.parse::<i64>() {
                            Ok(x) if !n.starts_with('+') => Val::Int(x),
                            _ => Val::Str(n.clone().into_bytes()),
                        };
                    }
                }
                r.push(Val::Obj(o));
            }
        }
    }
    Ok(r)
}

/// The ids of $HYDO_ACKED / $HYDO_FAILED / $HYDO_OUTCOMES (MANIFEST.md).
///
///   acked        $HYDO_ACKED: every id the client saw acknowledged
///   failed       $HYDO_FAILED: ids whose last outcome was a definite failure
///   told-failed  failed minus acked: told "failed" and never acknowledged later
///   unknown      ids whose last outcome in $HYDO_OUTCOMES is "unknown"
pub fn read_hydo(what: &[u8]) -> Res<Vec<Record>> {
    fn env_path(env: &str) -> Res<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt;
        match std::env::var_os(env) {
            Some(p) if !p.is_empty() => Ok(p.as_bytes().to_vec()),
            _ => Err(cannot(format!(
                "${env} is not set (not running under hydo-init?)"
            ))),
        }
    }
    fn line_of(r: &Record) -> Vec<u8> {
        match r {
            Val::Obj(o) => match o.get(&b"line"[..]) {
                Some(Val::Str(s)) => s.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
    type Ids = (std::collections::HashSet<Vec<u8>>, Vec<Vec<u8>>);
    fn ids(env: &str) -> Res<Ids> {
        let p = env_path(env)?;
        let recs = read_file(&p)?;
        let mut set = std::collections::HashSet::new();
        let mut list = Vec::new();
        for r in &recs {
            let l = line_of(r);
            let id = gofmt::fields(&l)[0].to_vec();
            if set.insert(id.clone()) {
                list.push(id);
            }
        }
        Ok((set, list))
    }
    fn out(list: Vec<Vec<u8>>) -> Vec<Record> {
        list.into_iter()
            .map(|id| rec(vec![("id", Val::Str(id))]))
            .collect()
    }
    match what {
        b"acked" => Ok(out(ids("HYDO_ACKED")?.1)),
        b"failed" => Ok(out(ids("HYDO_FAILED")?.1)),
        b"told-failed" => {
            let (acked, _) = ids("HYDO_ACKED")?;
            let (_, failed) = ids("HYDO_FAILED")?;
            Ok(out(failed
                .into_iter()
                .filter(|x| !acked.contains(x))
                .collect()))
        }
        b"unknown" => {
            let p = env_path("HYDO_OUTCOMES")?;
            let recs = read_file(&p)?;
            let mut last: std::collections::HashMap<Vec<u8>, Vec<u8>> = Default::default();
            let mut order = Vec::new();
            for r in &recs {
                let l = line_of(r);
                let f = gofmt::fields(&l);
                if f.len() < 2 {
                    continue;
                }
                if !last.contains_key(f[0]) {
                    order.push(f[0].to_vec());
                }
                last.insert(f[0].to_vec(), f[1].to_vec());
            }
            Ok(out(order
                .into_iter()
                .filter(|id| last.get(id).map(|v| v.as_slice()) == Some(b"unknown"))
                .collect()))
        }
        _ => Err(usage(format!(
            "hydo {}: acked, failed, told-failed or unknown",
            q(what)
        ))),
    }
}

/// Bytes as a Kafka/etcd value: null for nil, else the string or its base64.
pub fn bytes_value(b: Option<&[u8]>, b64: bool) -> Val {
    match b {
        None => Val::Null,
        Some(b) if b64 => Val::str(base64_encode(b)),
        Some(b) => Val::str(b),
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// base64.StdEncoding.EncodeToString.
pub fn base64_encode(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// base64.StdEncoding.DecodeString, returning what was decoded before an error (as Go's
/// `k, _ := DecodeString(s)` keeps).
pub fn base64_decode(s: &[u8]) -> Vec<u8> {
    let val = |c: u8| B64.iter().position(|&x| x == c).map(|p| p as u32);
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let chunk = &s[i..(i + 4).min(s.len())];
        if chunk.len() < 4 {
            break;
        }
        let pads = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pads > 2 {
            break;
        }
        let mut n = 0u32;
        let mut ok = true;
        for (k, &c) in chunk.iter().enumerate() {
            let v = if k >= 4 - pads { Some(0) } else { val(c) };
            match v {
                Some(v) => n = n << 6 | v,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            break;
        }
        out.push((n >> 16) as u8);
        if pads < 2 {
            out.push((n >> 8) as u8);
        }
        if pads < 1 {
            out.push(n as u8);
        }
        if pads > 0 {
            break;
        }
        i += 4;
    }
    out
}

/// The program: hydo-check under that name or as `hydo-read check`, else hydo-read.
pub fn main_with(argv0: &[u8], args: &[Vec<u8>], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let base = argv0.rsplit(|&c| c == b'/').next().unwrap_or(argv0);
    if base == b"hydo-check" {
        return check::check_main(args, out, err);
    }
    if !args.is_empty() && args[0] == b"check" {
        return check::check_main(&args[1..], out, err);
    }
    read_main(args, out, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in [&b""[..], b"0", b"MA", b"100000", b"\x00\xff\x10"] {
            assert_eq!(base64_decode(base64_encode(s).as_bytes()), s);
        }
        assert_eq!(base64_encode(b"0"), "MA==");
        assert_eq!(base64_encode(b"100000"), "MTAwMDAw");
    }
}
