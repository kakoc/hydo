//! sqlgen: a seeded SQL workload for libSQL's server (sqld) over Hrana HTTP (`POST /v2/pipeline`).
//!
//! Every choice comes from the seed hydo gives a command (`HYDO_SEED`), never from the clock, so
//! a run is reproduced by its schedule. Two workloads, each with a property a checker decides:
//!
//! - **bank**: `accounts(id, bal)`, `ACCOUNTS` rows of 100 each. A transfer is one transaction
//!   (BEGIN; debit if the balance allows; credit only if the debit changed a row; COMMIT) sent as
//!   one Hrana batch with conditions, so a failed step rolls the whole transaction back. Property:
//!   the total never changes and no balance is negative, on the primary and on the replica.
//! - **list-append**: `lists(k, v)`, where `v` is a comma-separated list. A transaction is 1..4
//!   micro-operations on keys 0..`KEYS`-1, each an append of a unique element or a read of the
//!   whole list, recorded as an Elle-style `txn` in hydo's history format
//!   (`HYDO-HIST la <process> invoke|ok|fail|info <t> txn a:<k>:<e> r:<k> ...`), so the host's
//!   list-append checker decides lost appends, aborted reads, duplicated elements and the
//!   dependency cycles of the claimed consistency level. Every transaction goes into history
//!   `la` (serializable: the replica may serve stale reads); `lap` (strict-serializable: a lost
//!   acknowledged append is caught, also across a power cut) holds the transactions sent to the
//!   primary and only the appends of those sent to the replica (`Rec`).
//!
//! Commands (the node is `primary` or `replica`, or `seed` to let the seed choose):
//!   sqlgen init                       create the schema and the accounts (idempotent)
//!   sqlgen transfer <node>            one transfer; records ack/failed/unknown for hydo
//!   sqlgen txn <node>                 one list-append transaction, recorded as history `la`
//!   sqlgen itransfer <node>           a transfer as an interactive transaction (read both
//!                                     balances, write the new values; one request per statement)
//!   sqlgen itxn <node>                the list-append transaction, interactive
//!   sqlgen read-all <node> <process>  read every list in one transaction (the final read-back)
//!   sqlgen total <node>               check the bank's total: exit 0 holds, 1 violated, 2 unreadable
//!   sqlgen dump <node>                both tables as sorted JSON Lines (a differential check)
//!
//! Exit status of a workload command: 0 acknowledged, 1 failed or unknown (hydo records the
//! outcome from `HYDO_OUTCOMES`), 2 usage.

use hydo_assert::history::{append_mop, read_mop, read_result_mop, History};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const ACCOUNTS: i64 = 8;
const START: i64 = 100;
const KEYS: u64 = 6;

fn addr(node: &str) -> SocketAddr {
    let var = if node == "replica" { "SQLGEN_REPLICA" } else { "SQLGEN_PRIMARY" };
    let def = if node == "replica" { "127.0.0.1:8081" } else { "127.0.0.1:8080" };
    std::env::var(var).unwrap_or_else(|_| def.into()).parse().expect("node address")
}

// ---------------------------------------------------------------- a seeded random stream

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn env_u64(name: &str) -> u64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

// ---------------------------------------------------------------- a small JSON value

#[derive(Debug, Clone)]
enum J {
    Null,
    #[allow(dead_code)]
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn get(&self, k: &str) -> &J {
        static NULL: J = J::Null;
        match self {
            J::Obj(m) => m.iter().find(|(a, _)| a == k).map(|(_, v)| v).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    fn idx(&self, i: usize) -> &J {
        static NULL: J = J::Null;
        match self {
            J::Arr(a) => a.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
    fn arr(&self) -> &[J] {
        match self {
            J::Arr(a) => a,
            _ => &[],
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    fn is_null(&self) -> bool {
        matches!(self, J::Null)
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}
impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn val(&mut self) -> Result<J, String> {
        self.ws();
        let c = *self.b.get(self.i).ok_or("end of input")?;
        match c {
            b'{' => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(J::Obj(m));
                }
                loop {
                    self.ws();
                    let k = match self.val()? {
                        J::Str(s) => s,
                        _ => return Err("key".into()),
                    };
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err("colon".into());
                    }
                    self.i += 1;
                    let v = self.val()?;
                    m.push((k, v));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(J::Obj(m));
                        }
                        _ => return Err("object".into()),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(J::Arr(a));
                }
                loop {
                    a.push(self.val()?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::Arr(a));
                        }
                        _ => return Err("array".into()),
                    }
                }
            }
            b'"' => {
                self.i += 1;
                let mut s = Vec::new();
                loop {
                    let c = *self.b.get(self.i).ok_or("string")?;
                    self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let e = *self.b.get(self.i).ok_or("escape")?;
                            self.i += 1;
                            match e {
                                b'n' => s.push(b'\n'),
                                b't' => s.push(b'\t'),
                                b'r' => s.push(b'\r'),
                                b'b' => s.push(8),
                                b'f' => s.push(12),
                                b'u' => {
                                    let h = std::str::from_utf8(self.b.get(self.i..self.i + 4).ok_or("u")?).map_err(|_| "u")?;
                                    let n = u32::from_str_radix(h, 16).map_err(|_| "u")?;
                                    self.i += 4;
                                    let ch = char::from_u32(n).unwrap_or('?');
                                    let mut buf = [0; 4];
                                    s.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                                }
                                o => s.push(o),
                            }
                        }
                        o => s.push(o),
                    }
                }
                Ok(J::Str(String::from_utf8_lossy(&s).into_owned()))
            }
            b't' if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(J::Bool(true))
            }
            b'f' if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(J::Bool(false))
            }
            b'n' if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(J::Null)
            }
            _ => {
                let st = self.i;
                while self.i < self.b.len() && (self.b[self.i] == b'-' || self.b[self.i] == b'.' || self.b[self.i] == b'+' || self.b[self.i] == b'e' || self.b[self.i] == b'E' || self.b[self.i].is_ascii_digit()) {
                    self.i += 1;
                }
                if st == self.i {
                    return Err(format!("unexpected byte {}", c));
                }
                Ok(J::Num(String::from_utf8_lossy(&self.b[st..self.i]).into_owned()))
            }
        }
    }
}

fn parse(b: &[u8]) -> Result<J, String> {
    P { b, i: 0 }.val()
}

fn q(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

// ---------------------------------------------------------------- Hrana over HTTP

/// A statement argument.
enum A {
    I(i64),
    T(String),
}

fn stmt(sql: &str, args: &[A]) -> String {
    let a: Vec<String> = args
        .iter()
        .map(|a| match a {
            A::I(n) => format!("{{\"type\":\"integer\",\"value\":\"{}\"}}", n),
            A::T(s) => format!("{{\"type\":\"text\",\"value\":{}}}", q(s)),
        })
        .collect();
    format!("{{\"sql\":{},\"args\":[{}],\"want_rows\":true}}", q(sql), a.join(","))
}

/// The outcome of a request whose answer did not arrive: it may or may not have been applied.
#[derive(Debug)]
enum Err {
    /// No connection was made: the request never left (a definite failure).
    Refused(String),
    /// Sent, but no complete answer (a timeout, a reset): unknown.
    Unknown(String),
    /// The server answered with an error (HTTP status or Hrana error).
    Answer(String),
}

fn post(node: &str, body: &str) -> Result<J, Err> {
    let a = addr(node);
    let ms = match env_u64("SQLGEN_TIMEOUT_MS") {
        0 => 3000,
        n => n.min(60_000),
    };
    let tmo = Duration::from_millis(ms);
    let mut s = TcpStream::connect_timeout(&a, tmo).map_err(|e| Err::Refused(format!("connect {}: {}", a, e)))?;
    s.set_read_timeout(Some(tmo)).ok();
    s.set_write_timeout(Some(tmo)).ok();
    let req = format!(
        "POST /v2/pipeline HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        a,
        body.len(),
        body
    );
    s.write_all(req.as_bytes()).map_err(|e| Err::Unknown(format!("send: {}", e)))?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).map_err(|e| Err::Unknown(format!("receive: {}", e)))?;
    let he = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| Err::Unknown("no HTTP header".into()))?;
    let head = String::from_utf8_lossy(&buf[..he]).to_string();
    let mut body = buf[he + 4..].to_vec();
    let status: u32 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let lower = head.to_ascii_lowercase();
    if lower.contains("transfer-encoding: chunked") {
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            let le = body[i..].windows(2).position(|w| w == b"\r\n").ok_or_else(|| Err::Unknown("short chunk".into()))?;
            let n = usize::from_str_radix(String::from_utf8_lossy(&body[i..i + le]).trim(), 16).map_err(|_| Err::Unknown("chunk size".into()))?;
            i += le + 2;
            if n == 0 {
                break;
            }
            if i + n > body.len() {
                return Err(Err::Unknown("short chunk".into()));
            }
            out.extend_from_slice(&body[i..i + n]);
            i += n + 2;
        }
        body = out;
    } else if let Some(cl) = lower.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()) {
        if body.len() < cl {
            return Err(Err::Unknown(format!("short body {} of {}", body.len(), cl)));
        }
        body.truncate(cl);
    }
    if status != 200 {
        let t = String::from_utf8_lossy(&body).replace('\n', " ");
        // A 5xx from a replica that could not reach the primary may or may not have been applied.
        return Err(if status >= 500 || status == 0 { Err::Unknown(format!("HTTP {} {}", status, t)) } else { Err::Answer(format!("HTTP {} {}", status, t)) });
    }
    parse(&body).map_err(|e| Err::Unknown(format!("bad JSON ({}): {}", e, String::from_utf8_lossy(&body))))
}

/// Runs `steps` as one transaction: BEGIN, each step only if the previous succeeded, COMMIT only
/// if all did, ROLLBACK otherwise. Returns each step's rows (as JSON) on commit.
fn transaction(node: &str, steps: &[String]) -> Result<Vec<J>, Err> {
    let mut s = vec![format!("{{\"stmt\":{}}}", stmt("BEGIN", &[]))];
    for (i, st) in steps.iter().enumerate() {
        s.push(format!("{{\"stmt\":{},\"condition\":{{\"type\":\"ok\",\"step\":{}}}}}", st, i));
    }
    let n = steps.len();
    s.push(format!("{{\"stmt\":{},\"condition\":{{\"type\":\"ok\",\"step\":{}}}}}", stmt("COMMIT", &[]), n));
    s.push(format!(
        "{{\"stmt\":{},\"condition\":{{\"type\":\"not\",\"cond\":{{\"type\":\"ok\",\"step\":{}}}}}}}",
        stmt("ROLLBACK", &[]),
        n + 1
    ));
    let body = format!("{{\"requests\":[{{\"type\":\"batch\",\"batch\":{{\"steps\":[{}]}}}},{{\"type\":\"close\"}}]}}", s.join(","));
    let r = post(node, &body)?;
    let res = r.get("results").idx(0);
    if res.get("type").str() != Some("ok") {
        return Err(Err::Answer(format!("request error: {:?}", res.get("error").get("message").str())));
    }
    let b = res.get("response").get("result");
    let results = b.get("step_results").arr().to_vec();
    let errors = b.get("step_errors").arr();
    // committed: the COMMIT step has a result and no error
    let commit_ok = !results.get(n + 1).map(|j| j.is_null()).unwrap_or(true) && errors.get(n + 1).map(|e| e.is_null()).unwrap_or(true);
    if commit_ok {
        return Ok(results[1..n + 1].to_vec());
    }
    let first = errors.iter().find_map(|e| e.get("message").str().map(|s| s.to_string())).unwrap_or_else(|| "a step did not run".into());
    // If COMMIT itself failed, SQLite rolled back; any earlier failure skipped COMMIT and ran ROLLBACK.
    Err(Err::Answer(first))
}

/// An interactive Hrana stream: one statement per HTTP request, tied together by the baton the
/// server returns, as a client library's `transaction()` runs one. The statements of a
/// transaction reach the server one at a time, so a replica sees reads before it sees a write.
struct Stream {
    node: String,
    baton: Option<String>,
}

impl Stream {
    fn new(node: &str) -> Stream {
        Stream { node: node.into(), baton: None }
    }
    fn send(&mut self, requests: &str) -> Result<J, Err> {
        let body = match &self.baton {
            Some(b) => format!("{{\"baton\":{},\"requests\":[{}]}}", q(b), requests),
            None => format!("{{\"baton\":null,\"requests\":[{}]}}", requests),
        };
        let r = post(&self.node, &body)?;
        if let Some(u) = r.get("base_url").str() {
            return Err(Err::Answer(format!("the server moved the stream to {} (not followed)", u)));
        }
        self.baton = r.get("baton").str().map(|s| s.to_string());
        Ok(r)
    }
    /// Executes one statement in the stream; its result (cols, rows, affected_row_count).
    fn exec(&mut self, st: &str) -> Result<J, Err> {
        let r = self.send(&format!("{{\"type\":\"execute\",\"stmt\":{}}}", st))?;
        let res = r.get("results").idx(0);
        if res.get("type").str() != Some("ok") {
            return Err(Err::Answer(res.get("error").get("message").str().unwrap_or("error").to_string()));
        }
        Ok(res.get("response").get("result").clone())
    }
    /// Ends the stream (best effort; an open stream is rolled back by the server).
    fn close(&mut self) {
        if self.baton.is_some() {
            let _ = self.send("{\"type\":\"close\"}");
        }
    }
}

/// Runs `body` as an interactive transaction: BEGIN, the body's statements one request each,
/// COMMIT. An error before COMMIT was sent is a definite failure (the transaction never
/// committed); a missing answer to COMMIT is unknown.
fn interactive<T>(node: &str, body: impl FnOnce(&mut Stream) -> Result<T, Err>) -> Result<T, Err> {
    let mut s = Stream::new(node);
    let pre = (|| {
        s.exec(&stmt("BEGIN", &[]))?;
        body(&mut s)
    })();
    let v = match pre {
        Ok(v) => v,
        Err(e) => {
            let _ = s.exec(&stmt("ROLLBACK", &[]));
            s.close();
            // nothing was committed: a definite failure whatever the cause
            return Err(match e {
                Err::Unknown(m) | Err::Refused(m) | Err::Answer(m) => Err::Answer(m),
            });
        }
    };
    let c = s.exec(&stmt("COMMIT", &[]));
    s.close();
    c.map(|_| v)
}

fn first_int(j: &J) -> Option<i64> {
    rows(j).first().and_then(|r| r.first()).and_then(|v| v.parse().ok())
}

/// A transfer as a client library writes one: read both balances, decide in the client, write
/// the new balances as values. Under serializable transactions the total cannot change; a lost
/// update (two transfers reading the same balance) changes it.
fn itransfer(node_arg: &str) -> i32 {
    let id = env_u64("HYDO_CMD_ID");
    let mut rng = Rng(env_u64("HYDO_SEED") ^ 0x697472616e73);
    let node = pick_node(node_arg, &mut rng);
    let from = rng.below(ACCOUNTS as u64) as i64;
    let to = (from + 1 + rng.below(ACCOUNTS as u64 - 1) as i64) % ACCOUNTS;
    let amt = 1 + rng.below(60) as i64;
    let r = interactive(&node, |s| {
        let fb = first_int(&s.exec(&stmt("SELECT bal FROM accounts WHERE id = ?", &[A::I(from)]))?).ok_or_else(|| Err::Answer("no such account".into()))?;
        let tb = first_int(&s.exec(&stmt("SELECT bal FROM accounts WHERE id = ?", &[A::I(to)]))?).ok_or_else(|| Err::Answer("no such account".into()))?;
        if fb < amt {
            return Ok(format!("balance {} < {}", fb, amt));
        }
        s.exec(&stmt("UPDATE accounts SET bal = ? WHERE id = ?", &[A::I(fb - amt), A::I(from)]))?;
        s.exec(&stmt("UPDATE accounts SET bal = ? WHERE id = ?", &[A::I(tb + amt), A::I(to)]))?;
        Ok(format!("{} -> {}, {} -> {}", fb, fb - amt, tb, tb + amt))
    });
    match r {
        Ok(what) => {
            println!("itransfer {} via {}: {} -> {} amount {}: {}", id, node, from, to, amt, what);
            outcome(id, "ack");
            0
        }
        Err(Err::Refused(e)) | Err(Err::Answer(e)) => {
            println!("itransfer {} via {}: failed: {}", id, node, e);
            outcome(id, "failed");
            1
        }
        Err(Err::Unknown(e)) => {
            println!("itransfer {} via {}: unknown: {}", id, node, e);
            outcome(id, "unknown");
            1
        }
    }
}

/// The list-append transaction of `txn`, run interactively (one statement per request).
fn itxn(node_arg: &str) -> i32 {
    let id = env_u64("HYDO_CMD_ID");
    let mut rng = Rng(env_u64("HYDO_SEED") ^ 0x69747874);
    let node = pick_node(node_arg, &mut rng);
    let n = 1 + rng.below(4);
    let mut mops = Vec::new();
    for i in 0..n {
        let k = rng.below(KEYS);
        if rng.below(2) == 0 {
            mops.push((true, k, id * 10 + i));
        } else {
            mops.push((false, k, 0));
        }
    }
    let inv: Vec<String> = mops.iter().map(|(a, k, e)| if *a { append_mop(&k.to_string(), &e.to_string()) } else { read_mop(&k.to_string()) }).collect();
    let inv_ref: Vec<&str> = inv.iter().map(|s| s.as_str()).collect();
    let op = Rec::invoke(&node, id, &inv_ref);
    let r = interactive(&node, |s| {
        let mut done = Vec::new();
        for (a, k, e) in &mops {
            if *a {
                s.exec(&stmt("INSERT INTO lists (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = v || ',' || excluded.v", &[A::I(*k as i64), A::T(e.to_string())]))?;
                done.push(append_mop(&k.to_string(), &e.to_string()));
            } else {
                let rs = rows(&s.exec(&stmt("SELECT v FROM lists WHERE k = ?", &[A::I(*k as i64)]))?);
                let l: Vec<String> = match rs.first().and_then(|r| r.first()) {
                    Some(v) if v != "nil" && !v.is_empty() => v.split(',').map(|x| x.to_string()).collect(),
                    _ => vec![],
                };
                let lr: Vec<&str> = l.iter().map(|x| x.as_str()).collect();
                done.push(read_result_mop(&k.to_string(), &lr));
            }
        }
        Ok(done)
    });
    match r {
        Ok(done) => {
            let d: Vec<&str> = done.iter().map(|s| s.as_str()).collect();
            op.ok(&d);
            println!("itxn {} via {}: committed: {}", id, node, done.join(" "));
            outcome(id, "ack");
            0
        }
        Err(Err::Refused(e)) | Err(Err::Answer(e)) => {
            op.fail();
            println!("itxn {} via {}: failed: {}", id, node, e);
            outcome(id, "failed");
            1
        }
        Err(Err::Unknown(e)) => {
            op.info();
            println!("itxn {} via {}: unknown: {}", id, node, e);
            outcome(id, "unknown");
            1
        }
    }
}

/// Records one list-append transaction in two histories:
/// - `la`, every transaction as the client saw it, checked as serializable (a replica may serve
///   stale reads, so real-time order is not claimed);
/// - `lap`, checked as strict-serializable: a transaction sent to the primary as it is; one sent
///   to the replica only with its appends (every write executes on the primary within the
///   client's request, so real-time order holds for it; its reads may be the replica's stale
///   copy and are left out). An acknowledged append missing from a later primary read is then a
///   lost write, also across a power cut.
struct Rec {
    la: hydo_assert::history::Op,
    lap: Option<hydo_assert::history::Op>,
    /// for `lap` of a replica transaction: keep only the append micro-operations
    appends_only: bool,
}

fn appends(args: &[&str]) -> Vec<String> {
    args.iter().filter(|a| a.starts_with("a:")).map(|a| a.to_string()).collect()
}

impl Rec {
    fn invoke(node: &str, process: impl std::fmt::Display + Copy, args: &[&str]) -> Rec {
        let la = History::new("la").invoke(process, "txn", args);
        let appends_only = node != "primary";
        let lap_args: Vec<String> = if appends_only { appends(args) } else { args.iter().map(|a| a.to_string()).collect() };
        let lap = if lap_args.is_empty() {
            None
        } else {
            let r: Vec<&str> = lap_args.iter().map(|s| s.as_str()).collect();
            Some(History::new("lap").invoke(process, "txn", &r))
        };
        Rec { la, lap, appends_only }
    }
    fn ok(self, args: &[&str]) {
        self.la.ok(args);
        if let Some(o) = self.lap {
            if self.appends_only {
                let a = appends(args);
                let r: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
                o.ok(&r);
            } else {
                o.ok(args);
            }
        }
    }
    fn fail(self) {
        self.la.fail();
        if let Some(o) = self.lap {
            o.fail();
        }
    }
    fn info(self) {
        self.la.info();
        if let Some(o) = self.lap {
            o.info();
        }
    }
}

fn rows(j: &J) -> Vec<Vec<String>> {
    j.get("rows")
        .arr()
        .iter()
        .map(|r| {
            r.arr()
                .iter()
                .map(|v| match v.get("value") {
                    J::Str(s) => s.clone(),
                    J::Num(n) => n.clone(),
                    _ => "nil".into(),
                })
                .collect()
        })
        .collect()
}

fn affected(j: &J) -> i64 {
    match j.get("affected_row_count") {
        J::Num(n) => n.parse().unwrap_or(-1),
        _ => -1,
    }
}

fn pick_node(arg: &str, rng: &mut Rng) -> String {
    match arg {
        "seed" => {
            if rng.below(2) == 0 {
                "primary".into()
            } else {
                "replica".into()
            }
        }
        n => n.into(),
    }
}

fn append_line(var: &str, line: &str) {
    if let Ok(p) = std::env::var(var) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = f.write_all(format!("{}\n", line).as_bytes());
        }
    }
}

fn outcome(id: u64, what: &str) {
    append_line("HYDO_OUTCOMES", &format!("{} {}", id, what));
    if what == "ack" {
        append_line("HYDO_ACKED", &id.to_string());
    }
}

// ---------------------------------------------------------------- the commands

fn init() -> i32 {
    let mut st = vec![
        stmt("CREATE TABLE IF NOT EXISTS accounts (id INTEGER PRIMARY KEY, bal INTEGER NOT NULL)", &[]),
        stmt("CREATE TABLE IF NOT EXISTS lists (k INTEGER PRIMARY KEY, v TEXT NOT NULL)", &[]),
    ];
    for i in 0..ACCOUNTS {
        st.push(stmt("INSERT OR IGNORE INTO accounts (id, bal) VALUES (?, ?)", &[A::I(i), A::I(START)]));
    }
    match transaction("primary", &st) {
        Ok(_) => {
            println!("schema ready: {} accounts of {}", ACCOUNTS, START);
            0
        }
        Err(e) => {
            println!("init failed: {:?}", e);
            1
        }
    }
}

fn transfer(node_arg: &str) -> i32 {
    let id = env_u64("HYDO_CMD_ID");
    let mut rng = Rng(env_u64("HYDO_SEED") ^ 0x7472616e73666572);
    let node = pick_node(node_arg, &mut rng);
    let from = rng.below(ACCOUNTS as u64) as i64;
    let to = (from + 1 + rng.below(ACCOUNTS as u64 - 1) as i64) % ACCOUNTS;
    let amt = 1 + rng.below(60) as i64;
    let steps = vec![
        stmt("UPDATE accounts SET bal = bal - ? WHERE id = ? AND bal >= ?", &[A::I(amt), A::I(from), A::I(amt)]),
        stmt("UPDATE accounts SET bal = bal + ? WHERE id = ? AND changes() = 1", &[A::I(amt), A::I(to)]),
    ];
    match transaction(&node, &steps) {
        Ok(r) => {
            let moved = affected(&r[0]) == 1;
            println!("transfer {} via {}: {} -> {} amount {}: {}", id, node, from, to, amt, if moved { "moved" } else { "insufficient balance" });
            outcome(id, "ack");
            0
        }
        Err(Err::Refused(e)) | Err(Err::Answer(e)) => {
            println!("transfer {} via {}: failed: {}", id, node, e);
            outcome(id, "failed");
            1
        }
        Err(Err::Unknown(e)) => {
            println!("transfer {} via {}: unknown: {}", id, node, e);
            outcome(id, "unknown");
            1
        }
    }
}

/// One list-append transaction, recorded in history `la` with process = the command's id.
fn txn(node_arg: &str) -> i32 {
    let id = env_u64("HYDO_CMD_ID");
    let mut rng = Rng(env_u64("HYDO_SEED") ^ 0x6c697374617070);
    let node = pick_node(node_arg, &mut rng);
    let n = 1 + rng.below(4);
    // (is_append, key, element)
    let mut mops = Vec::new();
    for i in 0..n {
        let k = rng.below(KEYS);
        if rng.below(2) == 0 {
            mops.push((true, k, id * 10 + i));
        } else {
            mops.push((false, k, 0));
        }
    }
    let inv: Vec<String> = mops.iter().map(|(a, k, e)| if *a { append_mop(&k.to_string(), &e.to_string()) } else { read_mop(&k.to_string()) }).collect();
    let inv_ref: Vec<&str> = inv.iter().map(|s| s.as_str()).collect();
    let op = Rec::invoke(&node, id, &inv_ref);
    let steps: Vec<String> = mops
        .iter()
        .map(|(a, k, e)| {
            if *a {
                stmt("INSERT INTO lists (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = v || ',' || excluded.v", &[A::I(*k as i64), A::T(e.to_string())])
            } else {
                stmt("SELECT v FROM lists WHERE k = ?", &[A::I(*k as i64)])
            }
        })
        .collect();
    match transaction(&node, &steps) {
        Ok(res) => {
            let mut done = Vec::new();
            for ((a, k, e), r) in mops.iter().zip(res.iter()) {
                if *a {
                    done.push(append_mop(&k.to_string(), &e.to_string()));
                } else {
                    let rs = rows(r);
                    let l: Vec<&str> = match rs.first().and_then(|r| r.first()) {
                        Some(v) if v != "nil" && !v.is_empty() => v.split(',').collect(),
                        _ => vec![],
                    };
                    done.push(read_result_mop(&k.to_string(), &l));
                }
            }
            let d: Vec<&str> = done.iter().map(|s| s.as_str()).collect();
            op.ok(&d);
            println!("txn {} via {}: committed: {}", id, node, done.join(" "));
            outcome(id, "ack");
            0
        }
        Err(Err::Refused(e)) | Err(Err::Answer(e)) => {
            op.fail();
            println!("txn {} via {}: failed: {}", id, node, e);
            outcome(id, "failed");
            1
        }
        Err(Err::Unknown(e)) => {
            op.info();
            println!("txn {} via {}: unknown: {}", id, node, e);
            outcome(id, "unknown");
            1
        }
    }
}

fn read_all(node: &str, process: &str) -> i32 {
    let inv: Vec<String> = (0..KEYS).map(|k| read_mop(&k.to_string())).collect();
    let inv_ref: Vec<&str> = inv.iter().map(|s| s.as_str()).collect();
    let op = Rec::invoke(node, process, &inv_ref);
    let steps: Vec<String> = (0..KEYS).map(|k| stmt("SELECT v FROM lists WHERE k = ?", &[A::I(k as i64)])).collect();
    match transaction(node, &steps) {
        Ok(res) => {
            let mut done = Vec::new();
            for (k, r) in res.iter().enumerate() {
                let rs = rows(r);
                let l: Vec<&str> = match rs.first().and_then(|r| r.first()) {
                    Some(v) if v != "nil" && !v.is_empty() => v.split(',').collect(),
                    _ => vec![],
                };
                done.push(read_result_mop(&k.to_string(), &l));
            }
            let d: Vec<&str> = done.iter().map(|s| s.as_str()).collect();
            op.ok(&d);
            println!("read-all via {}: {}", node, done.join(" "));
            0
        }
        Err(e) => {
            op.fail();
            println!("read-all via {}: could not read: {:?}", node, e);
            2
        }
    }
}

fn total(node: &str) -> i32 {
    let steps = vec![stmt("SELECT COUNT(*), COALESCE(SUM(bal), 0), COALESCE(MIN(bal), 0) FROM accounts", &[])];
    match transaction(node, &steps) {
        Ok(r) => {
            let rs = rows(&r[0]);
            let row = rs.first().cloned().unwrap_or_default();
            let g = |i: usize| row.get(i).and_then(|v| v.parse::<i64>().ok()).unwrap_or(-1);
            let (count, sum, min) = (g(0), g(1), g(2));
            if count == 0 {
                // The replica has not received the schema yet: nothing to judge.
                println!("{}: no accounts yet", node);
                return 0;
            }
            if count != ACCOUNTS || sum != ACCOUNTS * START || min < 0 {
                println!("{}: count {} sum {} min {}", node, count, sum, min);
                println!("{}: the bank's total is {} (want {}), lowest balance {}", node, sum, ACCOUNTS * START, min);
                return 1;
            }
            println!("{}: total {} across {} accounts, lowest balance {}", node, sum, count, min);
            0
        }
        Err(e) => {
            println!("{}: cannot read the accounts: {:?}", node, e);
            2
        }
    }
}

fn dump(node: &str) -> i32 {
    let steps = vec![stmt("SELECT id, bal FROM accounts ORDER BY id", &[]), stmt("SELECT k, v FROM lists ORDER BY k", &[])];
    match transaction(node, &steps) {
        Ok(r) => {
            for row in rows(&r[0]) {
                println!("{{\"table\":\"accounts\",\"id\":{},\"bal\":{}}}", row[0], row[1]);
            }
            for row in rows(&r[1]) {
                println!("{{\"table\":\"lists\",\"k\":{},\"v\":{}}}", row[0], q(&row[1]));
            }
            0
        }
        Err(e) => {
            eprintln!("{}: cannot read: {:?}", node, e);
            2
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).map(|s| s.as_str()).unwrap_or("");
    let rc = match arg(1) {
        "init" => init(),
        "transfer" => transfer(if arg(2).is_empty() { "seed" } else { arg(2) }),
        "txn" => txn(if arg(2).is_empty() { "seed" } else { arg(2) }),
        "itransfer" => itransfer(if arg(2).is_empty() { "seed" } else { arg(2) }),
        "itxn" => itxn(if arg(2).is_empty() { "seed" } else { arg(2) }),
        "read-all" => read_all(arg(2), if arg(3).is_empty() { "final" } else { arg(3) }),
        "total" => total(arg(2)),
        "dump" => dump(arg(2)),
        _ => {
            eprintln!("usage: sqlgen init | transfer [node] | txn [node] | itransfer [node] | itxn [node] | read-all <node> [process] | total <node> | dump <node>");
            2
        }
    };
    std::process::exit(rc);
}
