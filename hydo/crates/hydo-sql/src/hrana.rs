//! libSQL / SQLite through sqld's HTTP API: Hrana over HTTP (`POST /v2/pipeline`), HTTP/1.1
//! with `Connection: close`. A batch is one request whose steps run only when the previous ones
//! succeeded (BEGIN, the statements, COMMIT, else ROLLBACK); an interactive transaction is a
//! stream, one request per statement, tied together by the baton the server returns.

use crate::db::{sqlite_retry, Fail, Rows};
use crate::json::{self, Val};
use crate::net::dial_tcp;
use std::time::{Duration, Instant};

fn get<'a>(v: &'a Val, k: &str) -> &'a Val {
    static NULL: Val = Val::Null;
    match v {
        Val::Obj(o) => o.get(k.as_bytes()).unwrap_or(&NULL),
        _ => &NULL,
    }
}

fn arr(v: &Val) -> &[Val] {
    match v {
        Val::Arr(a) => a,
        _ => &[],
    }
}

fn s(v: &Val) -> Option<String> {
    match v {
        Val::Str(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Val::Num(n) => Some(n.clone()),
        Val::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A JSON string literal.
pub fn quote(t: &str) -> String {
    let mut o = Vec::new();
    json::enc_str(&mut o, t.as_bytes());
    String::from_utf8_lossy(&o).into_owned()
}

/// A Hrana statement without arguments (the workloads inline their literals).
pub fn stmt(sql: &str) -> String {
    format!("{{\"sql\":{},\"want_rows\":true}}", quote(sql))
}

/// Why a POST failed: not sent, an HTTP error status (with the body), or no complete answer.
enum PostFail {
    NotSent(String),
    Status(u32, String),
    Lost(String),
}

fn post(addr: &str, body: &str, timeout: Duration) -> Result<Val, PostFail> {
    let mut c =
        dial_tcp(addr, Instant::now() + timeout).map_err(|e| PostFail::NotSent(e.to_string()))?;
    let req = format!(
        "POST /v2/pipeline HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    c.write_all(req.as_bytes())
        .map_err(|e| PostFail::Lost(format!("send: {e}")))?;
    let buf = c
        .read_to_end()
        .map_err(|e| PostFail::Lost(format!("receive: {e}")))?;
    let he = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| PostFail::Lost("no HTTP header".into()))?;
    let head = String::from_utf8_lossy(&buf[..he]).to_string();
    let mut body = buf[he + 4..].to_vec();
    let status: u32 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let lower = head.to_ascii_lowercase();
    if lower.contains("transfer-encoding: chunked") {
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            let le = body
                .get(i..)
                .and_then(|b| b.windows(2).position(|w| w == b"\r\n"))
                .ok_or_else(|| PostFail::Lost("short chunk".into()))?;
            let n = usize::from_str_radix(String::from_utf8_lossy(&body[i..i + le]).trim(), 16)
                .map_err(|_| PostFail::Lost("chunk size".into()))?;
            i += le + 2;
            if n == 0 {
                break;
            }
            if i + n > body.len() {
                return Err(PostFail::Lost("short chunk".into()));
            }
            out.extend_from_slice(&body[i..i + n]);
            i += n + 2;
        }
        body = out;
    } else if let Some(cl) = lower
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if body.len() < cl {
            return Err(PostFail::Lost(format!("short body {} of {cl}", body.len())));
        }
        body.truncate(cl);
    }
    if status != 200 {
        return Err(PostFail::Status(
            status,
            String::from_utf8_lossy(&body).replace('\n', " "),
        ));
    }
    json::unmarshal(&body).map_err(|e| {
        PostFail::Lost(format!(
            "bad JSON ({}): {}",
            e.0,
            String::from_utf8_lossy(&body)
        ))
    })
}

/// A POST that is a whole transaction or a statement's answer: an HTTP 5xx (a node that could
/// not forward the write, a server that failed half-way) leaves the outcome open; a 4xx is the
/// server refusing the request.
fn post_fail(f: PostFail) -> Fail {
    match f {
        PostFail::NotSent(m) => Fail::NotSent(m),
        PostFail::Lost(m) => Fail::Unknown(m),
        PostFail::Status(st, b) if st >= 500 || st == 0 => Fail::Unknown(format!("HTTP {st} {b}")),
        PostFail::Status(st, b) => Fail::Rejected {
            retry: sqlite_retry(&b),
            msg: format!("HTTP {st} {b}"),
        },
    }
}

fn result_rows(r: &Val) -> Rows {
    let cols = arr(get(r, "cols"))
        .iter()
        .map(|c| s(get(c, "name")).unwrap_or_default())
        .collect();
    let rows = arr(get(r, "rows"))
        .iter()
        .map(|row| {
            arr(row)
                .iter()
                .map(|v| match get(v, "type") {
                    Val::Str(t) if t == b"null" => None,
                    _ => s(get(v, "value")),
                })
                .collect()
        })
        .collect();
    let affected = match get(r, "affected_row_count") {
        Val::Num(n) => n.parse().unwrap_or(-1),
        Val::Int(n) => *n,
        _ => -1,
    };
    Rows {
        cols,
        rows,
        affected,
    }
}

fn hrana_err(e: &Val) -> Fail {
    let msg = s(get(e, "message")).unwrap_or_else(|| "error".into());
    let code = s(get(e, "code")).unwrap_or_default();
    Fail::Rejected {
        retry: sqlite_retry(&msg) || sqlite_retry(&code),
        msg,
    }
}

/// One transaction as one request. The result of each of `stmts`.
pub fn batch(addr: &str, stmts: &[String], timeout: Duration) -> Result<Vec<Rows>, Fail> {
    let n = stmts.len();
    let mut steps = vec![format!("{{\"stmt\":{}}}", stmt("BEGIN"))];
    for (i, st) in stmts.iter().enumerate() {
        steps.push(format!(
            "{{\"stmt\":{},\"condition\":{{\"type\":\"ok\",\"step\":{i}}}}}",
            stmt(st)
        ));
    }
    steps.push(format!(
        "{{\"stmt\":{},\"condition\":{{\"type\":\"ok\",\"step\":{n}}}}}",
        stmt("COMMIT")
    ));
    steps.push(format!(
        "{{\"stmt\":{},\"condition\":{{\"type\":\"not\",\"cond\":{{\"type\":\"ok\",\"step\":{}}}}}}}",
        stmt("ROLLBACK"),
        n + 1
    ));
    let body = format!("{{\"requests\":[{{\"type\":\"batch\",\"batch\":{{\"steps\":[{}]}}}},{{\"type\":\"close\"}}]}}", steps.join(","));
    let r = post(addr, &body, timeout).map_err(post_fail)?;
    let res = arr(get(&r, "results"))
        .first()
        .cloned()
        .unwrap_or(Val::Null);
    if s(get(&res, "type")).as_deref() != Some("ok") {
        return Err(hrana_err(get(&res, "error")));
    }
    let b = get(get(&res, "response"), "result");
    let results = arr(get(b, "step_results"));
    let errors = arr(get(b, "step_errors"));
    let commit_ok = results.get(n + 1).is_some_and(|j| *j != Val::Null)
        && errors.get(n + 1).is_none_or(|e| *e == Val::Null);
    if commit_ok {
        return Ok(results[1..n + 1].iter().map(result_rows).collect());
    }
    match errors.iter().find(|e| **e != Val::Null) {
        Some(e) => Err(hrana_err(e)),
        None => Err(Fail::Rejected {
            msg: "a step did not run".into(),
            retry: false,
        }),
    }
}

/// Statements in autocommit mode, as one request of several `execute`s (each its own
/// transaction; the first error ends the request's useful part).
pub fn autocommit(addr: &str, stmts: &[String], timeout: Duration) -> Result<Vec<Rows>, Fail> {
    let mut reqs: Vec<String> = stmts
        .iter()
        .map(|st| format!("{{\"type\":\"execute\",\"stmt\":{}}}", stmt(st)))
        .collect();
    reqs.push("{\"type\":\"close\"}".into());
    let r = post(
        addr,
        &format!("{{\"requests\":[{}]}}", reqs.join(",")),
        timeout,
    )
    .map_err(post_fail)?;
    let mut out = Vec::new();
    for res in arr(get(&r, "results")).iter().take(stmts.len()) {
        if s(get(res, "type")).as_deref() != Some("ok") {
            return Err(hrana_err(get(res, "error")));
        }
        out.push(result_rows(get(get(res, "response"), "result")));
    }
    Ok(out)
}

/// An interactive stream.
pub struct Stream {
    addr: String,
    baton: Option<String>,
    timeout: Duration,
}

impl Stream {
    pub fn new(addr: &str, timeout: Duration) -> Stream {
        Stream {
            addr: addr.into(),
            baton: None,
            timeout,
        }
    }

    fn send(&mut self, requests: &str) -> Result<Val, Fail> {
        let body = match &self.baton {
            Some(b) => format!("{{\"baton\":{},\"requests\":[{requests}]}}", quote(b)),
            None => format!("{{\"baton\":null,\"requests\":[{requests}]}}"),
        };
        let r = post(&self.addr, &body, self.timeout).map_err(post_fail)?;
        if let Some(u) = s(get(&r, "base_url")) {
            return Err(Fail::Rejected {
                msg: format!("the server moved the stream to {u} (not followed)"),
                retry: false,
            });
        }
        self.baton = s(get(&r, "baton"));
        Ok(r)
    }

    pub fn exec(&mut self, sql: &str) -> Result<Rows, Fail> {
        let r = self.send(&format!("{{\"type\":\"execute\",\"stmt\":{}}}", stmt(sql)))?;
        let res = arr(get(&r, "results"))
            .first()
            .cloned()
            .unwrap_or(Val::Null);
        if s(get(&res, "type")).as_deref() != Some("ok") {
            return Err(hrana_err(get(&res, "error")));
        }
        Ok(result_rows(get(get(&res, "response"), "result")))
    }

    /// Ends the stream (best effort; an open stream is rolled back by the server).
    pub fn close(&mut self) {
        if self.baton.is_some() {
            let _ = self.send("{\"type\":\"close\"}");
            self.baton = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_from_hrana() {
        let v = json::unmarshal(br#"{"cols":[{"name":"k"},{"name":"v"}],"rows":[[{"type":"integer","value":"1"},{"type":"text","value":"a,b"}],[{"type":"integer","value":"2"},{"type":"null"}]],"affected_row_count":0}"#).unwrap();
        let r = result_rows(&v);
        assert_eq!(r.cols, vec!["k", "v"]);
        assert_eq!(
            r.rows,
            vec![
                vec![Some("1".into()), Some("a,b".into())],
                vec![Some("2".into()), None]
            ]
        );
        assert_eq!(r.affected, 0);
    }

    #[test]
    fn status_classes() {
        assert!(matches!(
            post_fail(PostFail::Status(502, "x".into())),
            Fail::Unknown(_)
        ));
        assert!(matches!(
            post_fail(PostFail::Status(400, "database is locked".into())),
            Fail::Rejected { retry: true, .. }
        ));
        assert!(matches!(
            post_fail(PostFail::NotSent("refused".into())),
            Fail::NotSent(_)
        ));
    }
}
