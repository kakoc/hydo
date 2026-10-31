//! etcd through its v3 JSON gateway (POST /v3/kv/range, /v3/maintenance/status) over a small
//! HTTP/1.1 client, with the Go program's records and error texts.

use crate::json::{self, field_fold, Obj, Val};
use crate::net::{dial_tcp, NetErr};
use crate::{base64_decode, base64_encode, bytes_value, cannot, lossy, q, rec, usage};
use crate::{ReadOpts, Record, Res};
use std::time::{Duration, Instant};

fn etcd_base(ep: &str) -> String {
    let mut ep = ep.to_string();
    if !ep.starts_with("http://") && !ep.starts_with("https://") {
        ep = format!("http://{ep}");
    }
    ep.trim_end_matches('/').to_string()
}

/// The response of one POST: status code and body.
fn http_post(url: &str, body: &[u8], t: Duration) -> Result<(u16, Vec<u8>), String> {
    let deadline = Instant::now() + t;
    let fail = |e: String| format!("Post {}: {e}", q(url.as_bytes()));
    let u = crate::url::parse(url).map_err(|e| e.to_string())?;
    if u.scheme != "http" && u.scheme != "https" {
        return Err(fail(format!("unsupported protocol scheme {}", q(u.scheme.as_bytes()))));
    }
    let host = u.host.clone();
    let addr = if crate::net::split_host_port(&host).is_ok() {
        host.clone()
    } else if u.scheme == "https" {
        format!("{host}:443")
    } else {
        format!("{host}:80")
    };
    // the request target: everything after the authority
    let after = &url[url.find("//").map(|i| i + 2).unwrap_or(0)..];
    let target = after.find('/').map(|i| &after[i..]).unwrap_or("/");
    let mut c = dial_tcp(&addr, deadline).map_err(|e| match e {
        NetErr::Timeout(s) => fail(format!("{s} (Client.Timeout exceeded while awaiting headers)")),
        e => fail(e.to_string()),
    })?;
    if u.scheme == "https" {
        return Err(fail("https endpoints are not supported by hydo-read (no TLS)".into()));
    }
    let mut req = format!(
        "POST {target} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Go-http-client/1.1\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend_from_slice(body);
    let headers_err = |e: NetErr| match e {
        NetErr::Timeout(_) => {
            fail("context deadline exceeded (Client.Timeout exceeded while awaiting headers)".into())
        }
        e => fail(e.to_string()),
    };
    c.write_all(&req).map_err(headers_err)?;
    let status_line = c.read_line().map_err(headers_err)?;
    let sl = String::from_utf8_lossy(&status_line);
    let code: u16 = sl
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| fail(format!("malformed HTTP response {}", q(sl.trim_end().as_bytes()))))?;
    let mut clen: Option<usize> = None;
    let mut chunked = false;
    loop {
        let h = c.read_line().map_err(headers_err)?;
        let h = String::from_utf8_lossy(&h).trim_end().to_string();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "content-length" {
                clen = v.parse().ok();
            } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }
    }
    let body_err = |e: NetErr| match e {
        NetErr::Timeout(_) => {
            "context deadline exceeded (Client.Timeout or context cancellation while reading body)"
                .to_string()
        }
        NetErr::Eof => "unexpected EOF".to_string(),
        e => e.to_string(),
    };
    let body = if chunked {
        let mut out = Vec::new();
        loop {
            let l = c.read_line().map_err(body_err)?;
            let ls = String::from_utf8_lossy(&l);
            let size_s = ls.trim().split(';').next().unwrap_or("").to_string();
            let n = usize::from_str_radix(&size_s, 16).map_err(|_| "malformed chunked encoding".to_string())?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&c.read_exact(n).map_err(body_err)?);
            c.read_exact(2).map_err(body_err)?;
        }
        out
    } else if let Some(n) = clen {
        c.read_exact(n).map_err(body_err)?
    } else {
        c.read_to_end().map_err(body_err)?
    };
    Ok((code, body))
}

fn etcd_post(ep: &str, path: &str, body: &[u8], t: Duration) -> Res<Val> {
    let url = format!("{}{path}", etcd_base(ep));
    let (code, rb) = http_post(&url, body, t).map_err(cannot)?;
    if code != 200 {
        return Err(cannot(format!(
            "etcd {path}: HTTP {code}: {}",
            lossy(crate::gofmt::trim_space(&rb))
        )));
    }
    json::unmarshal(&rb).map_err(|e| cannot(format!("etcd {path}: {}", e.0)))
}

/// `clientv3.GetPrefixRangeEnd`.
fn prefix_end(p: &[u8]) -> Vec<u8> {
    let mut b = p.to_vec();
    for i in (0..b.len()).rev() {
        if b[i] < 0xff {
            b[i] += 1;
            b.truncate(i + 1);
            return b;
        }
    }
    vec![0]
}

fn i64_of(s: &[u8]) -> i64 {
    std::str::from_utf8(s)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// A string field of a Go struct decoded from `o` (fold-case name matching; null keeps "").
fn str_field(o: &Obj, name: &str, path: &str) -> Res<Vec<u8>> {
    match field_fold(o, name) {
        None | Some(Val::Null) => Ok(Vec::new()),
        Some(Val::Str(s)) => Ok(s.clone()),
        Some(v) => Err(cannot(format!(
            "json: cannot unmarshal {} into Go struct field .{path} of type string",
            match v {
                Val::Bool(_) => "bool",
                Val::Int(_) | Val::Num(_) => "number",
                Val::Arr(_) => "array",
                _ => "object",
            }
        ))),
    }
}

fn json_body(fields: Vec<(&str, Val)>) -> Vec<u8> {
    json::encode(&rec(fields))
}

pub fn read(a: &[Vec<u8>], o: &ReadOpts) -> Res<Vec<Record>> {
    if a.len() < 2 {
        return Err(usage(
            "etcd <endpoint> get <key> [--prefix] [--serializable] | status",
        ));
    }
    let ep = lossy(&a[0]);
    match a[1].as_slice() {
        b"get" => {
            let mut key: Vec<u8> = Vec::new();
            let (mut prefix, mut serial) = (false, false);
            for x in &a[2..] {
                match x.as_slice() {
                    b"--prefix" => prefix = true,
                    b"--serializable" => serial = true,
                    _ => {
                        if !key.is_empty() {
                            return Err(usage("etcd get: one key"));
                        }
                        key = x.clone();
                    }
                }
            }
            let mut fields = vec![
                ("key", Val::str(base64_encode(&key))),
                ("serializable", Val::Bool(serial)),
            ];
            if prefix {
                if key.is_empty() {
                    fields[0].1 = Val::str(base64_encode(&[0]));
                }
                fields.push(("range_end", Val::str(base64_encode(&prefix_end(&key)))));
            }
            let resp = etcd_post(&ep, "/v3/kv/range", &json_body(fields), o.timeout)?;
            let mut out = Vec::new();
            let path = "/v3/kv/range";
            let top = match &resp {
                Val::Obj(m) => m.clone(),
                Val::Null => Obj::new(),
                _ => {
                    return Err(cannot(format!(
                        "etcd {path}: json: cannot unmarshal {} into Go value of type struct {{ Kvs []struct {{ Key string; Value string; CreateRevision string \"json:\\\"create_revision\\\"\"; ModRevision string \"json:\\\"mod_revision\\\"\"; Version string }} }}",
                        if matches!(resp, Val::Arr(_)) { "array" } else { "value" }
                    )))
                }
            };
            if let Some(Val::Arr(kvs)) = field_fold(&top, "Kvs") {
                for kv in kvs {
                    let Val::Obj(m) = kv else { continue };
                    let wrap = |e: crate::Error| match e {
                        crate::Error::Cannot(s) => cannot(format!("etcd {path}: {s}")),
                        e => e,
                    };
                    let k = base64_decode(&str_field(m, "Key", "Kvs.Key").map_err(wrap)?);
                    let v = base64_decode(&str_field(m, "Value", "Kvs.Value").map_err(wrap)?);
                    let cr = str_field(m, "create_revision", "Kvs.create_revision").map_err(wrap)?;
                    let mr = str_field(m, "mod_revision", "Kvs.mod_revision").map_err(wrap)?;
                    let ver = str_field(m, "Version", "Kvs.Version").map_err(wrap)?;
                    out.push(rec(vec![
                        ("key", bytes_value(Some(&k), o.base64)),
                        ("value", bytes_value(Some(&v), o.base64)),
                        ("create_revision", Val::Int(i64_of(&cr))),
                        ("mod_revision", Val::Int(i64_of(&mr))),
                        ("version", Val::Int(i64_of(&ver))),
                    ]));
                }
            }
            Ok(out)
        }
        b"status" => {
            if a.len() != 2 {
                return Err(usage("etcd <endpoint> status"));
            }
            let path = "/v3/maintenance/status";
            let resp = etcd_post(&ep, path, b"{}", o.timeout)?;
            let top = resp.as_obj().cloned().unwrap_or_default();
            let wrap = |e: crate::Error| match e {
                crate::Error::Cannot(s) => cannot(format!("etcd {path}: {s}")),
                e => e,
            };
            let hdr = match field_fold(&top, "Header") {
                Some(Val::Obj(h)) => h.clone(),
                _ => Obj::new(),
            };
            let rev = str_field(&hdr, "Revision", "Header.Revision").map_err(wrap)?;
            let idx = str_field(&top, "raftIndex", "raftIndex").map_err(wrap)?;
            let term = str_field(&top, "raftTerm", "raftTerm").map_err(wrap)?;
            Ok(vec![rec(vec![
                ("endpoint", Val::str(a[0].clone())),
                ("revision", Val::Int(i64_of(&rev))),
                ("raft_index", Val::Int(i64_of(&idx))),
                ("raft_term", Val::Int(i64_of(&term))),
            ])])
        }
        op => Err(usage(format!("etcd: unknown operation {}", q(op)))),
    }
}

/// PUT for the store tests.
pub fn put(ep: &str, k: &[u8], v: &[u8]) -> Result<(), String> {
    let body = json_body(vec![
        ("key", Val::str(base64_encode(k))),
        ("value", Val::str(base64_encode(v))),
    ]);
    let (code, b) = http_post(&format!("{}/v3/kv/put", etcd_base(ep)), &body, Duration::from_secs(5))?;
    if code != 200 {
        return Err(format!("HTTP {code}: {}", lossy(&b)));
    }
    Ok(())
}
