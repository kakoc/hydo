//! Redis over RESP2, as the Go program spoke it.

use crate::json::{Obj, Val};
use crate::net::{dial_tcp, Conn, NetErr};
use crate::{cannot, lossy, q, rec, usage, Error, ReadOpts, Record, Res};
use std::time::{Duration, Instant};

/// One reply; an error reply is `Err` at the top level.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Simple(Vec<u8>),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Arr(Option<Vec<Reply>>),
    Err(Vec<u8>),
}

impl Reply {
    fn val(&self) -> Val {
        match self {
            Reply::Simple(s) => Val::str(s.clone()),
            Reply::Int(n) => Val::Int(*n),
            Reply::Bulk(None) | Reply::Arr(None) => Val::Null,
            Reply::Bulk(Some(b)) => Val::str(b.clone()),
            Reply::Arr(Some(a)) => Val::Arr(a.iter().map(Reply::val).collect()),
            Reply::Err(e) => {
                let mut s = b"ERR ".to_vec();
                s.extend_from_slice(e);
                Val::str(s)
            }
        }
    }
    /// strs(v): the string elements of an array reply ("" for anything else).
    fn strs(&self) -> Vec<Vec<u8>> {
        match self {
            Reply::Arr(Some(a)) => a
                .iter()
                .map(|x| match x {
                    Reply::Simple(s) | Reply::Bulk(Some(s)) => s.clone(),
                    Reply::Err(e) => {
                        let mut s = b"ERR ".to_vec();
                        s.extend_from_slice(e);
                        s
                    }
                    _ => Vec::new(),
                })
                .collect(),
            _ => Vec::new(),
        }
    }
    fn as_str(&self) -> Vec<u8> {
        match self {
            Reply::Simple(s) | Reply::Bulk(Some(s)) => s.clone(),
            _ => Vec::new(),
        }
    }
}

pub struct RedisConn {
    c: Conn,
    t: Duration,
}

/// The text of a failed command (Go printed the error value).
fn net_text(e: NetErr) -> String {
    e.to_string()
}

/// `dialRedis`: addr is host:port or redis://[:password@]host:port[/db].
pub fn dial(addr: &[u8], t: Duration) -> Res<RedisConn> {
    let mut addr = lossy(addr);
    let mut pass = String::new();
    let mut db = String::new();
    if addr.starts_with("redis://") {
        let u = crate::url::parse(&addr)
            .map_err(|e| usage(format!("redis address {}: {e}", q(addr.as_bytes()))))?;
        if let Some((name, pw)) = &u.user {
            pass = pw.clone().unwrap_or_default();
            if pass.is_empty() {
                pass = name.clone();
            }
        }
        db = u.path.trim_matches('/').to_string();
        addr = u.host;
    }
    if !addr.contains(':') {
        addr.push_str(":6379");
    }
    let c = dial_tcp(&addr, Instant::now() + t).map_err(|e| cannot(net_text(e)))?;
    let mut rc = RedisConn { c, t };
    if !pass.is_empty() {
        rc.do_cmd(&[b"AUTH".to_vec(), pass.into_bytes()])
            .map_err(cannot)?;
    }
    if !db.is_empty() {
        rc.do_cmd(&[b"SELECT".to_vec(), db.into_bytes()])
            .map_err(cannot)?;
    }
    Ok(rc)
}

impl RedisConn {
    /// One command; an error reply is an error with its text.
    pub fn do_cmd(&mut self, args: &[Vec<u8>]) -> Result<Reply, String> {
        self.c.deadline = Instant::now() + self.t;
        let mut b = format!("*{}\r\n", args.len()).into_bytes();
        for a in args {
            b.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
            b.extend_from_slice(a);
            b.extend_from_slice(b"\r\n");
        }
        self.c.write_all(&b).map_err(net_text)?;
        match self.reply()? {
            Reply::Err(e) => Err(lossy(&e)),
            v => Ok(v),
        }
    }

    fn reply(&mut self) -> Result<Reply, String> {
        let mut line = self.c.read_line().map_err(net_text)?;
        while matches!(line.last(), Some(b'\r' | b'\n')) {
            line.pop();
        }
        if line.is_empty() {
            return Err("redis: empty reply line".into());
        }
        let body = &line[1..];
        let num = |b: &[u8]| -> Result<i64, String> {
            let s = String::from_utf8_lossy(b);
            s.parse::<i64>().map_err(|_| {
                format!(
                    "strconv.{}: parsing {}: invalid syntax",
                    if line[0] == b':' { "ParseInt" } else { "Atoi" },
                    q(b)
                )
            })
        };
        match line[0] {
            b'+' => Ok(Reply::Simple(body.to_vec())),
            b'-' => Ok(Reply::Err(body.to_vec())),
            b':' => Ok(Reply::Int(num(body)?)),
            b'$' => {
                let n = num(body)?;
                if n < 0 {
                    return Ok(Reply::Bulk(None));
                }
                let mut buf = self.c.read_exact(n as usize + 2).map_err(|e| match e {
                    NetErr::Eof => "unexpected EOF".to_string(),
                    e => net_text(e),
                })?;
                buf.truncate(n as usize);
                Ok(Reply::Bulk(Some(buf)))
            }
            b'*' => {
                let n = num(body)?;
                if n < 0 {
                    return Ok(Reply::Arr(None));
                }
                let mut arr = Vec::with_capacity(n.min(1 << 16) as usize);
                for _ in 0..n {
                    arr.push(self.reply()?);
                }
                Ok(Reply::Arr(Some(arr)))
            }
            _ => Err(format!("redis: unexpected reply {}", q(&line))),
        }
    }

    /// One key of any type: a string, a {field: value} map, a sorted member list (set), a list,
    /// or [[member, score], ...] (zset); streams and module types: the type only.
    fn value(&mut self, key: &[u8]) -> Result<(Vec<u8>, Val), String> {
        let t = self.do_cmd(&[b"TYPE".to_vec(), key.to_vec()])?;
        let typ = match &t {
            Reply::Simple(s) => s.clone(),
            _ => Vec::new(),
        };
        let k = key.to_vec();
        let v = match typ.as_slice() {
            b"none" => Val::Null,
            b"string" => self.do_cmd(&[b"GET".to_vec(), k])?.val(),
            b"hash" => {
                let s = self.do_cmd(&[b"HGETALL".to_vec(), k])?.strs();
                let mut m = Obj::new();
                for p in s.chunks_exact(2) {
                    m.insert(p[0].clone(), Val::str(p[1].clone()));
                }
                Val::Obj(m)
            }
            b"set" => {
                let mut s = self.do_cmd(&[b"SMEMBERS".to_vec(), k])?.strs();
                s.sort();
                Val::Arr(s.into_iter().map(Val::Str).collect())
            }
            b"list" => self
                .do_cmd(&[b"LRANGE".to_vec(), k, b"0".to_vec(), b"-1".to_vec()])?
                .val(),
            b"zset" => {
                let s = self
                    .do_cmd(&[
                        b"ZRANGE".to_vec(),
                        k,
                        b"0".to_vec(),
                        b"-1".to_vec(),
                        b"WITHSCORES".to_vec(),
                    ])?
                    .strs();
                if s.len() < 2 {
                    Val::Null
                } else {
                    Val::Arr(
                        s.chunks_exact(2)
                            .map(|p| Val::Arr(vec![Val::str(p[0].clone()), Val::str(p[1].clone())]))
                            .collect(),
                    )
                }
            }
            _ => Val::Null,
        };
        Ok((typ, v))
    }
}

pub fn read(a: &[Vec<u8>], o: &ReadOpts) -> Res<(Vec<Record>, bool)> {
    if a.len() < 2 {
        return Err(usage(
            "redis <addr> get|scan|hgetall|smembers|lrange|zrange|cmd ...",
        ));
    }
    let (addr, op, rest) = (&a[0], a[1].as_slice(), &a[2..]);
    let need = |n: usize, form: &str| -> Res<()> {
        if rest.len() != n {
            return Err(usage(format!("redis <addr> {form}")));
        }
        Ok(())
    };
    match op {
        b"get" if rest.is_empty() => return Err(usage("redis <addr> get <key>...")),
        b"get" => {}
        b"scan" => need(1, "scan <pattern>")?,
        b"hgetall" | b"smembers" | b"lrange" | b"zrange" => {
            need(1, &format!("{} <key>", lossy(op)))?
        }
        b"cmd" if rest.is_empty() => return Err(usage("redis <addr> cmd <word>...")),
        b"cmd" => {}
        _ => return Err(usage(format!("redis: unknown operation {}", q(op)))),
    }
    let mut rc = dial(addr, o.timeout)?;
    let fail = |e: String| -> Error { cannot(e) };
    let mut out = Vec::new();
    let key = |x: &[u8]| Val::str(x.to_vec());
    match op {
        b"get" => {
            for k in rest {
                let v = rc.do_cmd(&[b"GET".to_vec(), k.clone()]).map_err(fail)?;
                out.push(rec(vec![("key", key(k)), ("value", v.val())]));
            }
        }
        b"scan" => {
            let mut cursor = b"0".to_vec();
            let mut seen = std::collections::HashSet::new();
            let mut keys: Vec<Vec<u8>> = Vec::new();
            loop {
                let v = rc
                    .do_cmd(&[
                        b"SCAN".to_vec(),
                        cursor.clone(),
                        b"MATCH".to_vec(),
                        rest[0].clone(),
                        b"COUNT".to_vec(),
                        b"1000".to_vec(),
                    ])
                    .map_err(fail)?;
                let Reply::Arr(Some(arr)) = &v else {
                    return Err(fail("redis: bad SCAN reply".into()));
                };
                if arr.len() != 2 {
                    return Err(fail("redis: bad SCAN reply".into()));
                }
                cursor = arr[0].as_str();
                for k in arr[1].strs() {
                    if seen.insert(k.clone()) {
                        keys.push(k);
                    }
                }
                if cursor == b"0" {
                    break;
                }
            }
            keys.sort();
            for k in keys {
                let (typ, v) = rc.value(&k).map_err(fail)?;
                if typ == b"none" {
                    continue; // deleted between SCAN and TYPE
                }
                out.push(rec(vec![("key", key(&k)), ("type", Val::Str(typ)), ("value", v)]));
            }
        }
        b"hgetall" => {
            let s = rc
                .do_cmd(&[b"HGETALL".to_vec(), rest[0].clone()])
                .map_err(fail)?
                .strs();
            for p in s.chunks_exact(2) {
                out.push(rec(vec![
                    ("key", key(&rest[0])),
                    ("field", Val::str(p[0].clone())),
                    ("value", Val::str(p[1].clone())),
                ]));
            }
        }
        b"smembers" => {
            let s = rc
                .do_cmd(&[b"SMEMBERS".to_vec(), rest[0].clone()])
                .map_err(fail)?
                .strs();
            for m in s {
                out.push(rec(vec![("key", key(&rest[0])), ("member", Val::Str(m))]));
            }
        }
        b"lrange" => {
            let s = rc
                .do_cmd(&[
                    b"LRANGE".to_vec(),
                    rest[0].clone(),
                    b"0".to_vec(),
                    b"-1".to_vec(),
                ])
                .map_err(fail)?
                .strs();
            for (i, m) in s.into_iter().enumerate() {
                out.push(rec(vec![
                    ("key", key(&rest[0])),
                    ("index", Val::Int(i as i64)),
                    ("value", Val::Str(m)),
                ]));
            }
            return Ok((out, true));
        }
        b"zrange" => {
            let s = rc
                .do_cmd(&[
                    b"ZRANGE".to_vec(),
                    rest[0].clone(),
                    b"0".to_vec(),
                    b"-1".to_vec(),
                    b"WITHSCORES".to_vec(),
                ])
                .map_err(fail)?
                .strs();
            for p in s.chunks_exact(2) {
                out.push(rec(vec![
                    ("key", key(&rest[0])),
                    ("member", Val::str(p[0].clone())),
                    ("score", Val::str(p[1].clone())),
                ]));
            }
            return Ok((out, true));
        }
        _ => {
            let v = rc.do_cmd(rest).map_err(fail)?;
            out.push(rec(vec![("reply", v.val())]));
        }
    }
    Ok((out, false))
}

/// Raw commands for the store tests (FLUSHALL, SET, ...).
pub fn run_commands(addr: &[u8], cmds: &[Vec<Vec<u8>>]) -> Result<(), String> {
    let mut rc = dial(addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    for c in cmds {
        rc.do_cmd(c)?;
    }
    Ok(())
}
