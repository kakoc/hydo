//! The behaviour of `hydo-world`, end to end: one world (HTTP and gRPC fakes; HTTP, TCP and gRPC
//! fault proxies in front of real services this test runs; a webhook receiver) driven through
//! about eighty steps that cover every rule kind of an `H=` token, idempotent replays, webhooks
//! and their rules, events, every side-effect check, a power cut with the carried world and the
//! recovery world. Each step writes what the client saw (answer bytes without the Date header,
//! gRPC headers, messages and trailers, resets and timeouts) and what the real services and the
//! webhook receiver received; at the end come the record log (without its time field), the
//! console summary, the check outputs and the carried world.
//!
//! The transcript is compared with `tests/golden/xcheck.txt`, which was recorded from the Go
//! version of hydo-world (M111; its source, `hydo/onboard/world/`, was deleted after the proofs
//! and is in git at dd2ea9c) and is now the reference. `HYDO_WORLD_BIN` runs it against
//! another binary (the Go one, for example); `HYDO_BLESS=1` rewrites the golden file. The ports
//! are fixed (base `HYDO_XCHECK_PORT`, default 28600, below Linux's ephemeral port range 32768-60999 so that no outgoing connection on the machine can hold one) because they appear in error texts.
//!
//! A step whose records race on the host (a duplicated webhook is delivered twice at once) is
//! marked, and its records are compared as a sorted set.

use bytes::Bytes;
use std::collections::BTreeSet;
use std::future::{poll_fn, Future};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type Seen = Arc<Mutex<Vec<String>>>;

struct Ports {
    payments: u16,
    ledger: u16,
    shop: u16,
    kv: u16,
    orders: u16,
    orders2: u16,
    shop2: u16,
    up_http: u16,
    up_tcp: u16,
    up_grpc: u16,
    up_dead: u16,
    hook: u16,
}

impl Ports {
    fn new(b: u16) -> Ports {
        Ports {
            payments: b + 1,
            ledger: b + 2,
            shop: b + 3,
            kv: b + 4,
            orders: b + 5,
            orders2: b + 6,
            shop2: b + 7,
            up_http: b + 11,
            up_tcp: b + 12,
            up_grpc: b + 13,
            up_dead: b + 19,
            hook: b + 14,
        }
    }
}

fn world_json(p: &Ports) -> String {
    let fakes = serde_json::json!([
        {
            "name": "payments", "port": p.payments, "idempotency": "header:Idempotency-Key", "ref": "json:order",
            "routes": [
                {"name": "charge", "method": "POST", "path": "/v1/charges", "idempotent": true,
                 "body": "{\"id\":\"ch_{seq}\",\"order\":\"{ref}\",\"amount\":{json:amount}}",
                 "webhook": {"url": format!("http://127.0.0.1:{}/hooks/{{fake}}", p.hook),
                             "body": "{\"type\":\"charge.succeeded\",\"order\":\"{ref}\",\"charge\":\"ch_{seq}\",\"key\":\"{idem}\"}",
                             "headers": {"X-Sig": "s-{seq}"}, "retries": 1}},
                {"name": "get", "method": "GET", "path": "/v1/charges/{id}", "ref": "path:id",
                 "body": "{\"id\":\"{path:id}\",\"n\":{json:n}}"},
                {"name": "refund", "method": "POST", "path": "/v1/refunds/*", "status": 201, "ref": "form:order",
                 "headers": {"X-Refund": "r-{ref}"}, "body": "{\"refunded\":\"{ref}\"}"},
                {"name": "search", "method": "GET", "path": "/v1/search", "ref": "query:q", "body": "{\"q\":\"{query:q}\"}"},
                {"name": "plain", "path": "/v1/plain", "headers": {"Content-Type": "text/plain"}, "body": "hello {seq}"}
            ]
        },
        {
            "name": "ledger", "port": p.ledger, "protocol": "grpc", "ref": "pb:1",
            "routes": [
                {"name": "add", "path": "/ledger.Ledger/Add", "message": "0a026f6b"},
                {"name": "deny", "path": "/ledger.Ledger/Deny", "code": 7}
            ]
        }
    ]);
    let proxies = serde_json::json!([
        {"name": "shop", "port": p.shop, "to": format!("127.0.0.1:{}", p.up_http), "ref": "query:order",
         "routes": [{"name": "api", "path": "/api"}]},
        {"name": "kv", "port": p.kv, "protocol": "tcp", "to": format!("127.0.0.1:{}", p.up_tcp)},
        {"name": "orders", "port": p.orders, "protocol": "grpc", "to": format!("127.0.0.1:{}", p.up_grpc), "ref": "pb:1",
         "routes": [{"name": "place", "path": "/t.S/Unary"}]},
        {"name": "orders2", "port": p.orders2, "protocol": "grpc", "to": format!("127.0.0.1:{}", p.up_dead)},
        {"name": "shop2", "port": p.shop2, "to": format!("127.0.0.1:{}", p.up_dead)}
    ]);
    serde_json::json!({"fakes": fakes, "proxies": proxies}).to_string()
}

// ------------------------------------------------------------------ small helpers

/// Bytes as text: printable ASCII as is, everything else as \xNN.
fn show(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        if (0x20..0x7f).contains(&c) && c != b'\\' {
            s.push(c as char);
        } else {
            s.push_str(&format!("\\x{c:02x}"));
        }
    }
    s
}

/// HTTP/1 answers as text: for each answer its head lines (without Date) joined by " | ", then
/// its body (by Content-Length; the rest of the bytes when there is none). A 100 Continue or a
/// second answer on the connection follows after " || ".
fn show_http(mut b: &[u8]) -> String {
    let mut parts = Vec::new();
    while b.starts_with(b"HTTP/1.") {
        let Some(p) = b.windows(4).position(|w| w == b"\r\n\r\n") else {
            break;
        };
        let lines: Vec<String> = b[..p]
            .split(|&c| c == b'\n')
            .map(|l| show(l.strip_suffix(b"\r").unwrap_or(l)))
            .collect();
        let n = lines.iter().find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        });
        let informational = lines[0].starts_with("HTTP/1.1 1");
        let chunked = lines
            .iter()
            .any(|l| l.eq_ignore_ascii_case("transfer-encoding: chunked"));
        let head: Vec<String> = lines
            .into_iter()
            .filter(|l| !l.starts_with("Date: "))
            .collect();
        parts.push(head.join(" | "));
        b = &b[p + 4..];
        if informational {
            continue;
        }
        let k = match n {
            Some(n) if !chunked => n.min(b.len()),
            _ => b.len(),
        };
        if k > 0 {
            parts.push(format!("body={}", show(&b[..k])));
        }
        b = &b[k..];
    }
    if !b.is_empty() {
        parts.push(format!("rest={}", show(b)));
    }
    parts.join(" || ")
}

/// Both futures to their ends (no tokio macros in this crate).
async fn join2<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    let (mut ra, mut rb) = (None, None);
    poll_fn(|cx| {
        if ra.is_none() {
            if let Poll::Ready(x) = a.as_mut().poll(cx) {
                ra = Some(x);
            }
        }
        if rb.is_none() {
            if let Poll::Ready(x) = b.as_mut().poll(cx) {
                rb = Some(x);
            }
        }
        if ra.is_some() && rb.is_some() {
            return Poll::Ready(());
        }
        Poll::Pending
    })
    .await;
    (ra.unwrap(), rb.unwrap())
}

fn frame(m: &[u8]) -> Bytes {
    let mut b = vec![0u8; 5];
    b[1..5].copy_from_slice(&(m.len() as u32).to_be_bytes());
    b.extend_from_slice(m);
    Bytes::from(b)
}

fn frames(mut b: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let mut out = Vec::new();
    while b.len() >= 5 {
        let n = u32::from_be_bytes([b[1], b[2], b[3], b[4]]) as usize;
        if b.len() < 5 + n {
            break;
        }
        out.push(b[5..5 + n].to_vec());
        b = &b[5 + n..];
    }
    (out, b.len())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn pb1(s: &str) -> Vec<u8> {
    let mut m = vec![0x0a, s.len() as u8];
    m.extend_from_slice(s.as_bytes());
    m
}

fn show_map(m: &http::HeaderMap) -> String {
    let mut v: Vec<String> = m
        .iter()
        .filter(|(k, _)| k.as_str() != "date")
        .map(|(k, v)| format!("{}={}", k, show(v.as_bytes())))
        .collect();
    v.sort();
    v.join(";")
}

fn h2err(e: &h2::Error) -> String {
    match e.reason() {
        Some(r) if e.is_go_away() => format!("[goaway {r:?}]"),
        Some(r) => format!("[rst {r:?}]"),
        None if e.is_io() => "[conn]".into(),
        None => "[error]".into(),
    }
}

// ------------------------------------------------------------------ clients

/// Raw HTTP/1 bytes in, everything that came back until the server closed (or the timeout).
async fn http1_raw(port: u16, req: &[u8], ms: u64) -> String {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return "[connect failed]".into();
    };
    let _ = s.write_all(req).await;
    let mut buf = Vec::new();
    let r = tokio::time::timeout(Duration::from_millis(ms), async {
        loop {
            let mut b = [0u8; 8192];
            match s.read(&mut b).await {
                Ok(0) => return Ok(()),
                Ok(n) => buf.extend_from_slice(&b[..n]),
                Err(e) => return Err(e),
            }
        }
    })
    .await;
    let tail = match r {
        Err(_) => " [timeout]",
        Ok(Ok(())) if buf.is_empty() => "[closed]",
        Ok(Ok(())) => "",
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => " [reset]",
        Ok(Err(_)) => " [error]",
    };
    format!("{}{tail}", show_http(&buf))
}

fn req1(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut s = format!("{method} {path} HTTP/1.1\r\nHost: world\r\nConnection: close\r\n");
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() || method == "POST" {
        s.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    s.push_str("\r\n");
    s.push_str(body);
    s.into_bytes()
}

async fn h2_connect(port: u16) -> Result<h2::client::SendRequest<Bytes>, String> {
    let s = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|_| "[connect failed]".to_string())?;
    let (send, conn) = h2::client::handshake(s).await.map_err(|e| h2err(&e))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok(send)
}

/// One HTTP/2 (or gRPC) exchange: the request messages are sent `gap` apart, then the end of
/// the stream; the answer is its status, headers, body (or gRPC messages) and trailers.
#[allow(clippy::too_many_arguments)]
async fn h2_call(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<Bytes>,
    gap: u64,
    ms: u64,
    grpc: bool,
) -> String {
    let mut out = String::new();
    let r = tokio::time::timeout(Duration::from_millis(ms), async {
        let mut send = match h2_connect(port).await {
            Ok(s) => s,
            Err(e) => {
                out.push_str(&e);
                return;
            }
        };
        let mut b = http::Request::builder()
            .method(method)
            .uri(format!("http://127.0.0.1:{port}{path}"));
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = b.body(()).unwrap();
        let (rf, mut tx) = match send.send_request(req, false) {
            Ok(p) => p,
            Err(e) => {
                out.push_str(&h2err(&e));
                return;
            }
        };
        let sender = async {
            for m in body {
                if tx.send_data(m, false).is_err() {
                    return;
                }
                if gap > 0 {
                    tokio::time::sleep(Duration::from_millis(gap)).await;
                }
            }
            let _ = tx.send_data(Bytes::new(), true);
            // keep the stream's send half until the answer is done
            std::future::pending::<()>().await;
        };
        let receiver = async {
            let resp = match rf.await {
                Ok(r) => r,
                Err(e) => {
                    out.push_str(&h2err(&e));
                    return;
                }
            };
            out.push_str(&format!(
                "status={} h[{}]",
                resp.status().as_u16(),
                show_map(resp.headers())
            ));
            let mut rb = resp.into_body();
            let mut buf = Vec::new();
            loop {
                match rb.data().await {
                    Some(Ok(d)) => {
                        let _ = rb.flow_control().release_capacity(d.len());
                        buf.extend_from_slice(&d);
                        if grpc {
                            let (ms, rest) = frames(&buf);
                            for m in &ms {
                                out.push_str(&format!(" m[{}]", hex(m)));
                            }
                            let keep = buf.len() - rest;
                            buf.drain(..keep);
                        }
                    }
                    Some(Err(e)) => {
                        if !grpc && !buf.is_empty() {
                            out.push_str(&format!(" body={}", show(&buf)));
                        }
                        out.push(' ');
                        out.push_str(&h2err(&e));
                        return;
                    }
                    None => break,
                }
            }
            if !buf.is_empty() {
                out.push_str(&format!(" body={}", show(&buf)));
            }
            match rb.trailers().await {
                Ok(Some(t)) => out.push_str(&format!(" t[{}]", show_map(&t))),
                Ok(None) => out.push_str(" end"),
                Err(e) => {
                    out.push(' ');
                    out.push_str(&h2err(&e));
                }
            }
        };
        let mut sender = pin!(sender);
        let mut receiver = pin!(receiver);
        poll_fn(|cx| {
            let _ = sender.as_mut().poll(cx);
            receiver.as_mut().poll(cx)
        })
        .await;
    })
    .await;
    if r.is_err() {
        out.push_str(" [timeout]");
    }
    out.trim_start().to_string()
}

async fn grpc(port: u16, path: &str, msgs: &[Vec<u8>], gap: u64, ms: u64) -> String {
    let body = msgs.iter().map(|m| frame(m)).collect();
    h2_call(
        port,
        "POST",
        path,
        &[
            ("content-type", "application/grpc"),
            ("te", "trailers"),
            ("user-agent", "xcheck/1"),
        ],
        body,
        gap,
        ms,
        true,
    )
    .await
}

async fn tcp(port: u16, data: &[u8], ms: u64) -> String {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return "[connect failed]".into();
    };
    let _ = s.write_all(data).await;
    let _ = s.shutdown().await;
    let mut buf = Vec::new();
    let r = tokio::time::timeout(Duration::from_millis(ms), async {
        loop {
            let mut b = [0u8; 8192];
            match s.read(&mut b).await {
                Ok(0) => return Ok(()),
                Ok(n) => buf.extend_from_slice(&b[..n]),
                Err(e) => return Err(e),
            }
        }
    })
    .await;
    let tail = match r {
        Err(_) => " [timeout]",
        Ok(Ok(())) => " [closed]",
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset => " [reset]",
        Ok(Err(_)) => " [error]",
    };
    format!("{}{tail}", show(&buf)).trim_start().to_string()
}

// ------------------------------------------------------------------ the real services

async fn read_http_request(s: &mut TcpStream) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let mut b = [0u8; 8192];
        match s.read(&mut b).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&b[..n]),
        }
    };
    let head = buf[..head_end].to_vec();
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    let n = text
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < n {
        let mut b = [0u8; 8192];
        match s.read(&mut b).await {
            Ok(0) | Err(_) => break,
            Ok(k) => body.extend_from_slice(&b[..k]),
        }
    }
    Some((head, body))
}

fn head_lines(h: &[u8]) -> String {
    h.split(|&c| c == b'\n')
        .map(|l| show(l.strip_suffix(b"\r").unwrap_or(l)))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The gzip answer of `/gz` (`{"gz":true,"text":"aaa…"}`), as fixed bytes: an encoder's output
/// depends on which flate2 backend a whole-workspace build unifies to.
const GZ_BODY: [u8; 54] = [
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x8d, 0xc0, 0xb1, 0x0d, 0x00, 0x20,
    0x08, 0x44, 0xd1, 0x5d, 0x7e, 0xed, 0x04, 0x6c, 0x63, 0x41, 0xec, 0xcd, 0x99, 0x18, 0x08, 0xbb,
    0xb3, 0x02, 0x2f, 0x39, 0x81, 0xe9, 0x3e, 0x5f, 0xc8, 0xbf, 0x30, 0xf6, 0x00, 0xd5, 0xde, 0xca,
    0xce, 0x9e, 0x39, 0x00, 0x00, 0x00,
];

/// The real HTTP service behind the proxy `shop`.
async fn up_http(ln: TcpListener, seen: Seen) {
    loop {
        let Ok((mut s, _)) = ln.accept().await else {
            return;
        };
        let seen = seen.clone();
        tokio::spawn(async move {
            let Some((head, body)) = read_http_request(&mut s).await else {
                return;
            };
            seen.lock().unwrap().push(format!(
                "upstream http: {} || body={}",
                head_lines(&head),
                show(&body)
            ));
            let line = String::from_utf8_lossy(&head)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            let path = line.split(' ').nth(1).unwrap_or("").to_string();
            let resp: Vec<u8> = if path.starts_with("/gz") {
                let z: &[u8] = &GZ_BODY;
                let mut r = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nVary: Accept-Encoding\r\nContent-Length: {}\r\n\r\n",
                    z.len()
                )
                .into_bytes();
                r.extend_from_slice(z);
                r
            } else if path.starts_with("/chunked") {
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nTrailer: X-Sum\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\nX-Sum: 11\r\n\r\n".to_vec()
            } else {
                if path.starts_with("/slow") {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                let b = format!("{{\"path\":\"{path}\",\"n\":{}}}", body.len());
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Upstream: yes\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n{b}",
                    b.len()
                )
                .into_bytes()
            };
            let _ = s.write_all(&resp).await;
            let _ = s.shutdown().await;
        });
    }
}

/// The real TCP service behind the proxy `kv`: every line comes back with a '+'.
async fn up_tcp(ln: TcpListener, seen: Seen) {
    loop {
        let Ok((mut s, _)) = ln.accept().await else {
            return;
        };
        let seen = seen.clone();
        tokio::spawn(async move {
            let mut got = Vec::new();
            let mut b = [0u8; 4096];
            loop {
                match s.read(&mut b).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        got.extend_from_slice(&b[..n]);
                        let mut out = Vec::new();
                        for l in b[..n].split_inclusive(|&c| c == b'\n') {
                            out.push(b'+');
                            out.extend_from_slice(l);
                        }
                        if s.write_all(&out).await.is_err() {
                            break;
                        }
                    }
                }
            }
            seen.lock()
                .unwrap()
                .push(format!("upstream tcp: {}", show(&got)));
            let _ = s.shutdown().await;
        });
    }
}

/// The real gRPC service behind the proxy `orders`.
async fn up_grpc(ln: TcpListener, seen: Seen) {
    loop {
        let Ok((s, _)) = ln.accept().await else {
            return;
        };
        let seen = seen.clone();
        tokio::spawn(async move {
            let Ok(mut conn) = h2::server::handshake(s).await else {
                return;
            };
            while let Some(Ok((req, resp))) = conn.accept().await {
                tokio::spawn(up_call(req, resp, seen.clone()));
            }
        });
    }
}

async fn up_call(
    req: http::Request<h2::RecvStream>,
    mut resp: h2::server::SendResponse<Bytes>,
    seen: Seen,
) {
    let path = req.uri().path().to_string();
    let what = format!(
        "{} {} {} h[{}]",
        req.method(),
        req.uri().authority().map(|a| a.as_str()).unwrap_or("-"),
        path,
        show_map(req.headers())
    );
    let mut body = req.into_body();
    let mut buf = Vec::new();
    let mut end = "eos";
    loop {
        match body.data().await {
            Some(Ok(d)) => {
                let _ = body.flow_control().release_capacity(d.len());
                buf.extend_from_slice(&d);
            }
            Some(Err(_)) => {
                end = "reset";
                break;
            }
            None => break,
        }
    }
    let (msgs, _) = frames(&buf);
    seen.lock().unwrap().push(format!(
        "upstream grpc: {what} msgs={} end={end}",
        msgs.len()
    ));
    if end == "reset" {
        return;
    }
    let ok_head = || {
        http::Response::builder()
            .status(200)
            .header("content-type", "application/grpc")
            .body(())
            .unwrap()
    };
    let ok_trailers = || {
        let mut t = http::HeaderMap::new();
        t.insert("grpc-status", http::HeaderValue::from_static("0"));
        t.insert("grpc-message", http::HeaderValue::from_static(""));
        t
    };
    let only = |code: &'static str, msg: &'static str| {
        http::Response::builder()
            .status(200)
            .header("content-type", "application/grpc")
            .header("grpc-status", code)
            .header("grpc-message", msg)
            .body(())
            .unwrap()
    };
    let first = msgs.first().cloned().unwrap_or_default();
    match path.as_str() {
        "/t.S/Unary" | "/t.S/Count" | "/t.S/Slow" => {
            if path == "/t.S/Slow" {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            let Ok(mut s) = resp.send_response(ok_head(), false) else {
                return;
            };
            let mut m = b"ok:".to_vec();
            m.extend_from_slice(&first);
            let _ = s.send_data(frame(&m), false);
            let _ = s.send_trailers(ok_trailers());
        }
        "/t.S/Stream" => {
            let Ok(mut s) = resp.send_response(ok_head(), false) else {
                return;
            };
            for i in 1..=3 {
                let _ = s.send_data(frame(format!("s{i}").as_bytes()), false);
            }
            let _ = s.send_trailers(ok_trailers());
        }
        "/t.S/Client" => {
            let Ok(mut s) = resp.send_response(ok_head(), false) else {
                return;
            };
            let _ = s.send_data(frame(msgs.len().to_string().as_bytes()), false);
            let _ = s.send_trailers(ok_trailers());
        }
        "/t.S/Fail" => {
            let _ = resp.send_response(only("5", "nope"), true);
        }
        _ => {
            let _ = resp.send_response(only("12", "unknown method"), true);
        }
    }
    // let the frames go out before the stream handles are dropped
    tokio::time::sleep(Duration::from_millis(20)).await;
}

/// The system's webhook endpoint: 200, or 500 when the body says "fail".
async fn hook_receiver(ln: TcpListener, seen: Seen) {
    loop {
        let Ok((mut s, _)) = ln.accept().await else {
            return;
        };
        let seen = seen.clone();
        tokio::spawn(async move {
            let Some((head, body)) = read_http_request(&mut s).await else {
                return;
            };
            let fail = body.windows(4).any(|w| w == b"fail");
            seen.lock().unwrap().push(format!(
                "webhook got: {} || body={} -> {}",
                head_lines(&head),
                show(&body),
                if fail { 500 } else { 200 }
            ));
            let r: &[u8] = if fail {
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = s.write_all(r).await;
            let _ = s.shutdown().await;
        });
    }
}

// ------------------------------------------------------------------ the harness

struct H {
    bin: PathBuf,
    dir: PathBuf,
    log: PathBuf,
    sock: PathBuf,
    child: Option<Child>,
    out: Vec<String>,
    seen: Seen,
    racy: BTreeSet<i64>,
    next_id: i64,
}

fn max_seq(log: &Path) -> i64 {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.strip_prefix("seq=")?
                .split(' ')
                .next()?
                .parse::<i64>()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

impl H {
    fn run(&self, args: &[String], env_log: Option<&Path>) -> (String, i32) {
        let mut c = Command::new(&self.bin);
        c.args(args)
            .env("HYDO_WORLD_SOCK", &self.sock)
            .env("HYDO_WORLD_LOG", env_log.unwrap_or(&self.log))
            .env_remove("HYDO_ACKED")
            .stdin(Stdio::null());
        let o = c.output().expect("run hydo-world");
        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
        let e = String::from_utf8_lossy(&o.stderr);
        if !e.is_empty() {
            s.push_str("stderr: ");
            s.push_str(&e);
        }
        (self.norm(&s), o.status.code().unwrap_or(-1))
    }

    fn norm(&self, s: &str) -> String {
        s.replace(&self.dir.to_string_lossy().into_owned(), "<dir>")
    }

    async fn cli(&mut self, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (bin, dir, log, sock) = (
            self.bin.clone(),
            self.dir.clone(),
            self.log.clone(),
            self.sock.clone(),
        );
        let h = H {
            bin,
            dir,
            log,
            sock,
            child: None,
            out: Vec::new(),
            seen: self.seen.clone(),
            racy: BTreeSet::new(),
            next_id: 0,
        };
        let a2 = args.clone();
        let (o, rc) = tokio::task::spawn_blocking(move || h.run(&a2, None))
            .await
            .unwrap();
        let line = format!(
            "$ {}: {} rc={rc}",
            args.join(" "),
            o.trim_end().replace('\n', " / ")
        );
        let line = self.norm(&line);
        self.out.push(line);
        o
    }

    async fn on(&mut self, token: &str) -> i64 {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.cli(&["ctl", "on", &id, token]).await;
        self.next_id
    }

    async fn off(&mut self, id: i64) {
        self.cli(&["ctl", "off", &id.to_string()]).await;
    }

    /// What a client saw, then wait for the log to settle (webhooks and call records come
    /// after the answer).
    async fn say(&mut self, label: &str, r: String) {
        self.out.push(format!("{label}: {r}"));
        self.settle().await;
    }

    fn step(&mut self, name: &str) {
        self.out.push(format!("## {name}"));
    }

    /// Wait until the record log has been quiet for a while, then list what the real services
    /// and the webhook receiver got (sorted: they can arrive in either order).
    async fn settle(&mut self) {
        let t0 = Instant::now();
        let mut last = (u64::MAX, Instant::now());
        loop {
            let n = std::fs::metadata(&self.log).map(|m| m.len()).unwrap_or(0);
            if n != last.0 {
                last = (n, Instant::now());
            } else if last.1.elapsed() > Duration::from_millis(300) {
                break;
            }
            if t0.elapsed() > Duration::from_secs(8) {
                self.out.push("(the log did not settle)".into());
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let mut seen = std::mem::take(&mut *self.seen.lock().unwrap());
        seen.sort();
        for s in seen {
            self.out.push(format!("  {s}"));
        }
    }

    async fn wait_log(&mut self, pat: &str, n: usize) {
        let t0 = Instant::now();
        while std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .matches(pat)
            .count()
            < n
        {
            if t0.elapsed() > Duration::from_secs(8) {
                self.out.push(format!("(no {pat} in the log)"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    async fn start(&mut self, cfg: &Path, carry: Option<&Path>) {
        let mut c = Command::new(&self.bin);
        c.arg("serve").arg(cfg);
        if let Some(f) = carry {
            c.arg("--carry").arg(f);
        }
        c.env("HYDO_WORLD_SOCK", &self.sock)
            .env("HYDO_WORLD_LOG", &self.log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        self.child = Some(c.spawn().expect("start hydo-world serve"));
        let t0 = Instant::now();
        loop {
            let h = H {
                bin: self.bin.clone(),
                dir: self.dir.clone(),
                log: self.log.clone(),
                sock: self.sock.clone(),
                child: None,
                out: Vec::new(),
                seen: self.seen.clone(),
                racy: BTreeSet::new(),
                next_id: 0,
            };
            let (o, _) =
                tokio::task::spawn_blocking(move || h.run(&["ctl".into(), "ping".into()], None))
                    .await
                    .unwrap();
            if o == "ok\n" {
                break;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "hydo-world did not start: {o}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Stop the world; its `hydo-world:` lines on stderr (sorted: the gRPC proxy's line comes
    /// from another goroutine in the Go version).
    fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            let mut e = String::new();
            if let Some(mut s) = c.stderr.take() {
                let _ = s.read_to_string(&mut e);
            }
            // only the lines the guest init reads (Go also prints a handler's panic there)
            let mut lines: Vec<String> = e
                .lines()
                .filter(|l| l.starts_with("hydo-world: "))
                .map(|l| self.norm(l))
                .collect();
            lines.sort();
            self.out.push("## the world's stderr".into());
            self.out.extend(lines.into_iter().map(|l| format!("  {l}")));
        }
    }

    /// The record log without the time field; the records of racy steps sorted, without seq.
    fn log_section(&mut self, title: &str) {
        self.out.push(format!("## {title}"));
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<(i64, String)> = text
            .lines()
            .map(|l| {
                let seq = l
                    .strip_prefix("seq=")
                    .and_then(|r| r.split(' ').next())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let f: Vec<&str> = l.split(' ').filter(|t| !t.starts_with("t=")).collect();
                (seq, f.join(" "))
            })
            .collect();
        let racy = self.racy.clone();
        self.out.extend(group_racy(lines, &racy, |l| {
            l.split_once(' ')
                .map(|x| x.1.to_string())
                .unwrap_or_default()
        }));
    }

    async fn summary(&mut self) {
        let o = self.cli_quiet(&["ctl", "summary"]).await;
        self.out.push("## summary".into());
        let lines: Vec<(i64, String)> = o
            .lines()
            .map(|l| {
                (
                    l.split(' ')
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    l.to_string(),
                )
            })
            .collect();
        let racy = self.racy.clone();
        self.out.extend(group_racy(lines, &racy, |l| {
            l.split_once(' ')
                .map(|x| x.1.to_string())
                .unwrap_or_default()
        }));
    }

    async fn cli_quiet(&mut self, args: &[&str]) -> String {
        let n = self.out.len();
        let o = self.cli(args).await;
        self.out.truncate(n);
        o
    }
}

/// Lines in order; a run of lines whose seq is racy is sorted after `strip` (seq removed).
fn group_racy(
    lines: Vec<(i64, String)>,
    racy: &BTreeSet<i64>,
    strip: impl Fn(&str) -> String,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut run: Vec<String> = Vec::new();
    for (seq, l) in lines {
        if racy.contains(&seq) {
            run.push(format!("  ~ {}", strip(&l)));
            continue;
        }
        run.sort();
        out.append(&mut run);
        out.push(format!("  {l}"));
    }
    run.sort();
    out.append(&mut run);
    out
}

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-world-xcheck-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn xcheck() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(scenario());
}

async fn scenario() {
    let base: u16 = std::env::var("HYDO_XCHECK_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(28600);
    let p = Ports::new(base);
    let bin = std::env::var_os("HYDO_WORLD_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hydo-world")));
    let dir = tmpdir();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    for (port, kind) in [(p.up_http, 0), (p.up_tcp, 1), (p.up_grpc, 2), (p.hook, 3)] {
        let ln = TcpListener::bind(("127.0.0.1", port))
            .await
            .expect("bind a test service port");
        let s = seen.clone();
        match kind {
            0 => tokio::spawn(up_http(ln, s)),
            1 => tokio::spawn(up_tcp(ln, s)),
            2 => tokio::spawn(up_grpc(ln, s)),
            _ => tokio::spawn(hook_receiver(ln, s)),
        };
    }
    let cfg = dir.join("world.json");
    std::fs::write(&cfg, world_json(&p)).unwrap();
    let mut h = H {
        bin,
        log: dir.join("world.log"),
        sock: dir.join("world.sock"),
        dir: dir.clone(),
        child: None,
        out: Vec::new(),
        seen: seen.clone(),
        racy: BTreeSet::new(),
        next_id: 0,
    };
    h.start(&cfg, None).await;

    world_steps(&mut h, &p).await;

    // ---------------------------------------------------------------- checks on the live log
    h.step("checks");
    let acked = dir.join("acked");
    std::fs::write(&acked, "1\n2\n3\nfail1\nw:1,e,x,y,z,-\nclock=5\n").unwrap();
    let a = acked.to_string_lossy().into_owned();
    for args in [
        vec!["check", "at-most-once", "payments.charge"],
        vec!["check", "at-most-once", "payments.charge", "--by", "idem"],
        vec![
            "check",
            "exactly-once",
            "payments.charge",
            "--by",
            "idem",
            "--ref",
            "o{id}",
            "--acked",
            &a,
        ],
        vec![
            "check",
            "effect-for-acked",
            "payments.charge",
            "--ref",
            "o{id}",
            "--acked",
            &a,
        ],
        vec![
            "check",
            "effect-for-acked",
            "payments.charge",
            "--ref",
            "o{id}",
            "--acked",
            &a,
            "--unless",
            "event:cancelled",
        ],
        vec![
            "check",
            "none-after",
            "payments.charge",
            "--after",
            "event:cancelled",
        ],
        vec!["check", "none-after", "payments.charge"],
        vec!["check", "count", "payments.charge"],
        vec!["check", "count", "payments"],
        vec!["check", "count", "event:cancelled"],
        vec!["check", "calls", "orders"],
        vec!["check", "calls", "orders.place"],
        vec!["check", "calls", "orders2"],
        vec!["check", "frobnicate", "payments"],
        vec!["check", "count"],
    ] {
        h.cli(&args).await;
    }
    h.summary().await;
    h.log_section("record log");

    // ---------------------------------------------------------------- the checks of world_test.go
    h.step("checks on a written log");
    let log2 = dir.join("w.log");
    std::fs::write(
        &log2,
        [
            "seq=1 t=0 kind=req ep=payments route=charge ref=o1 idem=a fx=1 out=ok",
            "seq=2 t=0 kind=req ep=payments route=charge ref=o2 idem=b fx=1 out=ok",
            "seq=3 t=0 kind=event ep=- route=cancelled ref=o2 fx=1",
            "seq=4 t=0 kind=req ep=payments route=charge ref=o1 idem=a fx=0 out=replay",
            "seq=5 t=0 kind=req ep=payments route=charge ref=o2 idem=c fx=1 out=ok",
            "seq=0 t=0 kind=meta trunc=1",
            "",
        ]
        .join("\n"),
    )
    .unwrap();
    let acked2 = dir.join("acked2");
    std::fs::write(&acked2, "1\n2\n3\nw:1,e,x,y,z,-\nclock=5\n").unwrap();
    let a2 = acked2.to_string_lossy().into_owned();
    for args in [
        vec!["check", "at-most-once", "payments.charge"],
        vec!["check", "at-most-once", "payments.charge", "--by", "idem"],
        vec![
            "check",
            "effect-for-acked",
            "payments.charge",
            "--ref",
            "o{id}",
            "--acked",
            &a2,
        ],
        vec![
            "check",
            "none-after",
            "payments.charge",
            "--after",
            "event:cancelled",
        ],
        vec![
            "check",
            "exactly-once",
            "payments.charge",
            "--by",
            "idem",
            "--ref",
            "o{id}",
            "--acked",
            &a2,
        ],
        vec!["check", "count", "payments.charge"],
    ] {
        let args2: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (o, rc) = h.run(&args2, Some(&log2));
        let line = format!(
            "$ {}: {} rc={rc}",
            args.join(" "),
            o.trim_end().replace('\n', " / ")
        );
        let line = h.norm(&line);
        h.out.push(line);
    }
    let (o, rc) = h.run(
        &["check".into(), "count".into(), "payments.charge".into()],
        Some(&dir.join("missing")),
    );
    h.out.push(format!(
        "$ check count payments.charge (no log): {} rc={rc}",
        o.trim_end()
    ));

    // ---------------------------------------------------------------- power cut
    h.step("power cut: a webhook pending, the world dumped");
    let id = h.on("payments.charge@0-1:late=3000").await;
    h.say(
        "charge o30",
        http1_raw(
            p.payments,
            &req1(
                "POST",
                "/v1/charges",
                &[("Idempotency-Key", "k30")],
                r#"{"order":"o30","amount":3}"#,
            ),
            2000,
        )
        .await,
    )
    .await;
    let _ = id;
    let carry = h.cli(&["ctl", "dump"]).await;
    h.say(
        "charge o31 after the cut (never answered)",
        http1_raw(
            p.payments,
            &req1("POST", "/v1/charges", &[], r#"{"order":"o31","amount":3}"#),
            300,
        )
        .await,
    )
    .await;
    h.log_section("record log at the cut");
    h.stop();

    h.step("the recovery world");
    let cf = dir.join("carry");
    std::fs::write(&cf, &carry).unwrap();
    h.log = dir.join("world2.log");
    h.start(&cfg, Some(&cf)).await;
    h.wait_log("kind=hook", 1).await;
    h.settle().await;
    h.say(
        "charge o1 k1 again (remembered)",
        http1_raw(
            p.payments,
            &req1(
                "POST",
                "/v1/charges",
                &[("Idempotency-Key", "k1")],
                r#"{"order":"o1","amount":12.5}"#,
            ),
            2000,
        )
        .await,
    )
    .await;
    h.cli(&["check", "at-most-once", "payments.charge", "--by", "idem"])
        .await;
    h.cli(&["check", "count", "payments.charge"]).await;
    h.summary().await;
    h.log_section("record log of the recovery world");
    h.stop();
    let _ = h.cli(&["ctl", "ping"]).await; // no world any more

    // ---------------------------------------------------------------- compare
    let text = h.out.join("\n") + "\n";
    let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/xcheck.txt");
    let got = dir.join("xcheck.txt");
    std::fs::write(&got, &text).unwrap();
    if std::env::var_os("HYDO_BLESS").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &text).unwrap();
        eprintln!("wrote {}", golden.display());
        return;
    }
    let want = std::fs::read_to_string(&golden)
        .expect("tests/golden/xcheck.txt (HYDO_BLESS=1 records it)");
    if want != text {
        let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), text.lines().collect());
        let mut diffs = Vec::new();
        for i in 0..w.len().max(g.len()) {
            let (a, b) = (
                w.get(i).copied().unwrap_or("<none>"),
                g.get(i).copied().unwrap_or("<none>"),
            );
            if a != b {
                diffs.push(format!("line {}:\n  want: {a}\n  got:  {b}", i + 1));
            }
            if diffs.len() == 20 {
                break;
            }
        }
        panic!(
            "the transcript differs from the golden one (whole transcript: {}):\n{}",
            got.display(),
            diffs.join("\n")
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[allow(clippy::too_many_lines)]
async fn world_steps(h: &mut H, p: &Ports) {
    let charge = |order: &str, key: Option<&str>| {
        let body = format!(r#"{{"order":"{order}","amount":12.5}}"#);
        let mut hs = vec![("Content-Type", "application/json")];
        if let Some(k) = key {
            hs.push(("Idempotency-Key", k));
        }
        req1("POST", "/v1/charges", &hs, &body)
    };

    // ---------------------------------------------------------------- control commands
    h.step("control commands");
    for args in [
        vec!["ctl", "ping"],
        vec!["ctl", "frob"],
        vec!["ctl", "on", "1"],
        vec!["ctl", "off"],
        vec!["ctl", "event", "x"],
        vec!["ctl", "on", "1", "nocolon"],
        vec!["ctl", "on", "1", "@0-1:drop"],
        vec!["ctl", "on", "1", "nobody@0-1:drop"],
        vec!["ctl", "on", "1", "payments@0-1:frob"],
        vec!["ctl", "on", "1", "payments@0-1:delay=x"],
        vec!["ctl", "on", "1", "payments@0-1:effect=maybe"],
        vec!["ctl", "on", "1", "payments@0-1:rpc=Unary"],
        vec!["ctl", "on", "1", "payments@0-1:reset=conn"],
        vec!["ctl", "on", "1", "orders@0-1:cut=x"],
        vec!["ctl", "on", "1", "orders@0-1:dir=up"],
        vec!["ctl", "on", "1", "orders@0-1:call=0"],
        vec!["ctl", "on", "1", "orders@0-1:call=3-2"],
        vec!["ctl", "on", "1", "orders@0-1:reset=foo"],
        vec!["ctl", "on", "1", "orders@0-1:seed="],
        vec!["ctl", "on", "1", "orders@0-1:rpc="],
        vec!["ctl", "on", "1", "orders@0-1:status=NOPE"],
        vec!["ctl"],
    ] {
        h.cli(&args).await;
    }

    // ---------------------------------------------------------------- the HTTP fake
    h.step("fake: charges, replays, routes");
    h.say(
        "charge o1 k1",
        http1_raw(p.payments, &charge("o1", Some("k1")), 2000).await,
    )
    .await;
    h.say(
        "charge o1 k1 again",
        http1_raw(p.payments, &charge("o1", Some("k1")), 2000).await,
    )
    .await;
    h.say(
        "charge o2 (no key)",
        http1_raw(p.payments, &charge("o2", None), 2000).await,
    )
    .await;
    h.say(
        "get ch_1",
        http1_raw(p.payments, &req1("GET", "/v1/charges/ch_1", &[], ""), 2000).await,
    )
    .await;
    h.say(
        "get with a json body",
        http1_raw(
            p.payments,
            &req1("GET", "/v1/charges/ch%202", &[], r#"{"n":[1,{"a":true}]}"#),
            2000,
        )
        .await,
    )
    .await;
    h.say(
        "search",
        http1_raw(
            p.payments,
            &req1("GET", "/v1/search?q=a%20b&q=c", &[], ""),
            2000,
        )
        .await,
    )
    .await;
    h.say(
        "refund (form)",
        http1_raw(
            p.payments,
            &req1(
                "POST",
                "/v1/refunds/x/y",
                &[("Content-Type", "application/x-www-form-urlencoded")],
                "order=o1&why=dup",
            ),
            2000,
        )
        .await,
    )
    .await;
    h.say(
        "plain",
        http1_raw(p.payments, &req1("PUT", "/v1/plain", &[], "x"), 2000).await,
    )
    .await;
    h.say(
        "no route",
        http1_raw(p.payments, &req1("GET", "/nope", &[], ""), 2000).await,
    )
    .await;
    h.say(
        "HEAD of a route",
        http1_raw(p.payments, &req1("HEAD", "/v1/search?q=h", &[], ""), 2000).await,
    )
    .await;
    h.say(
        "expect 100-continue",
        http1_raw(
            p.payments,
            b"POST /v1/charges HTTP/1.1\r\nHost: w\r\nExpect: 100-continue\r\nContent-Length: 30\r\nConnection: close\r\n\r\n{\"order\":\"o3\",\"amount\":\"7\"}  ",
            2000,
        )
        .await,
    ).await;
    h.say(
        "two requests on one connection",
        http1_raw(
            p.payments,
            b"GET /v1/search?q=one HTTP/1.1\r\nHost: w\r\n\r\nGET /v1/search?q=two HTTP/1.1\r\nHost: w\r\nConnection: close\r\n\r\n",
            2000,
        )
        .await,
    ).await;
    h.say(
        "HTTP/1.0",
        http1_raw(p.payments, b"GET /v1/search?q=old HTTP/1.0\r\n\r\n", 2000).await,
    )
    .await;
    h.say(
        "chunked request",
        http1_raw(
            p.payments,
            b"POST /v1/charges HTTP/1.1\r\nHost: w\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\n{\"order\"\r\n9\r\n:\"o4\"}   \r\n0\r\n\r\n",
            2000,
        )
        .await,
    ).await;
    h.say(
        "garbage",
        http1_raw(p.payments, b"GARBAGE\r\n\r\n", 2000).await,
    )
    .await;
    h.say(
        "absolute URI",
        http1_raw(
            p.payments,
            b"GET http://x/v1/search?q=abs HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            2000,
        )
        .await,
    )
    .await;

    h.step("fake over HTTP/2");
    let json = [
        ("content-type", "application/json"),
        ("idempotency-key", "k5"),
    ];
    h.say(
        "h2 charge o5 k5",
        h2_call(
            p.payments,
            "POST",
            "/v1/charges",
            &json,
            vec![Bytes::from_static(br#"{"order":"o5","amount":1}"#)],
            0,
            2000,
            false,
        )
        .await,
    )
    .await;
    h.say(
        "h2 charge o5 k5 again",
        h2_call(
            p.payments,
            "POST",
            "/v1/charges",
            &json,
            vec![Bytes::from_static(br#"{"order":"o5","amount":1}"#)],
            0,
            2000,
            false,
        )
        .await,
    )
    .await;
    h.say(
        "h2 get",
        h2_call(
            p.payments,
            "GET",
            "/v1/charges/ch_9",
            &[],
            vec![],
            0,
            2000,
            false,
        )
        .await,
    )
    .await;
    h.say(
        "h2 no route",
        h2_call(p.payments, "GET", "/zzz", &[], vec![], 0, 2000, false).await,
    )
    .await;

    h.step("gRPC fake");
    h.say(
        "Add",
        grpc(p.ledger, "/ledger.Ledger/Add", &[pb1("l1")], 0, 2000).await,
    )
    .await;
    h.say(
        "Deny",
        grpc(p.ledger, "/ledger.Ledger/Deny", &[pb1("l2")], 0, 2000).await,
    )
    .await;
    h.say(
        "unknown",
        grpc(p.ledger, "/ledger.Ledger/Nope", &[pb1("l3")], 0, 2000).await,
    )
    .await;
    let id = h.on("ledger.add@0-1:status=UNAVAILABLE").await;
    h.say(
        "Add, UNAVAILABLE",
        grpc(p.ledger, "/ledger.Ledger/Add", &[pb1("l4")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("ledger@0-1:reset").await;
    h.say(
        "Add, reset",
        grpc(p.ledger, "/ledger.Ledger/Add", &[pb1("l5")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    h.settle().await;

    // ---------------------------------------------------------------- HTTP faults
    h.step("fake faults: status, delay, drop, reset");
    let id = h.on("payments.charge@3-6:status=503,retry-after=2").await;
    h.say(
        "charge o6",
        http1_raw(p.payments, &charge("o6", Some("k6")), 2000).await,
    )
    .await;
    h.say(
        "search (other route)",
        http1_raw(p.payments, &req1("GET", "/v1/search?q=s", &[], ""), 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("payments.charge@0-1:status=429,effect=yes").await;
    h.say(
        "charge o7, 429 but done",
        http1_raw(p.payments, &charge("o7", Some("k7")), 2000).await,
    )
    .await;
    h.off(id).await;
    h.settle().await;
    let id = h.on("payments.charge@0-1:delay=100").await;
    h.say(
        "charge o8 delayed",
        http1_raw(p.payments, &charge("o8", None), 2000).await,
    )
    .await;
    h.off(id).await;
    h.settle().await;
    let id = h.on("payments.charge@0-1:drop").await;
    h.say(
        "charge o9 dropped",
        http1_raw(p.payments, &charge("o9", None), 300).await,
    )
    .await;
    h.off(id).await;
    h.settle().await;
    let id = h.on("payments.charge@0-1:reset").await;
    h.say(
        "charge o10 reset",
        http1_raw(p.payments, &charge("o10", None), 2000).await,
    )
    .await;
    h.off(id).await;
    h.settle().await;
    let id = h.on("payments@0-1:status=500").await;
    h.say(
        "search, every route",
        http1_raw(p.payments, &req1("GET", "/v1/search?q=e", &[], ""), 2000).await,
    )
    .await;
    h.say(
        "no route, every route",
        http1_raw(p.payments, &req1("GET", "/nope", &[], ""), 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("payments.charge@0-1:delay=50,effect=no").await;
    h.say(
        "charge o11, no effect",
        http1_raw(p.payments, &charge("o11", Some("k11")), 2000).await,
    )
    .await;
    h.off(id).await;
    h.settle().await;

    h.step("fake faults: p=50");
    let id = h.on("payments.charge@2-4:status=503,p=50").await;
    for i in 12..20 {
        let r = http1_raw(p.payments, &charge(&format!("o{i}"), None), 2000).await;
        h.say(&format!("charge o{i}"), r).await;
    }
    h.off(id).await;
    h.settle().await;

    h.step("fake faults: a drop ended by off, by heal");
    let id = h.on("payments.charge@0-1:drop").await;
    let (r, _) = join2(http1_raw(p.payments, &charge("o20", None), 3000), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let o = std::process::Command::new(&h.bin)
            .args(["ctl", "off", &id.to_string()])
            .env("HYDO_WORLD_SOCK", &h.sock)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    })
    .await;
    h.say("charge o20, off after 200 ms", r).await;
    let _id = h.on("payments.charge@0-1:drop").await;
    let _id2 = h.on("payments.get@0-1:drop").await;
    let (r, _) = join2(http1_raw(p.payments, &charge("o21", None), 3000), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::Command::new(&h.bin)
            .args(["ctl", "heal"])
            .env("HYDO_WORLD_SOCK", &h.sock)
            .output()
            .unwrap()
    })
    .await;
    h.say("charge o21, heal after 200 ms", r).await;
    h.say(
        "get after the heal",
        http1_raw(p.payments, &req1("GET", "/v1/charges/ch_1", &[], ""), 2000).await,
    )
    .await;

    h.step("fake faults over HTTP/2");
    let id = h.on("payments.charge@0-1:status=503").await;
    h.say(
        "h2 charge o22, 503",
        h2_call(
            p.payments,
            "POST",
            "/v1/charges",
            &[],
            vec![Bytes::from_static(br#"{"order":"o22"}"#)],
            0,
            2000,
            false,
        )
        .await,
    )
    .await;
    h.off(id).await;
    let id = h.on("payments.charge@0-1:drop").await;
    h.say(
        "h2 charge o23, dropped",
        h2_call(
            p.payments,
            "POST",
            "/v1/charges",
            &[],
            vec![Bytes::from_static(br#"{"order":"o23"}"#)],
            0,
            300,
            false,
        )
        .await,
    )
    .await;
    h.off(id).await;
    let id = h.on("payments.charge@0-1:reset").await;
    h.say(
        "h2 charge o24, reset",
        h2_call(
            p.payments,
            "POST",
            "/v1/charges",
            &[],
            vec![Bytes::from_static(br#"{"order":"o24"}"#)],
            0,
            2000,
            false,
        )
        .await,
    )
    .await;
    h.off(id).await;
    h.settle().await;

    // ---------------------------------------------------------------- webhooks
    h.step("webhooks: dup");
    let s0 = max_seq(&h.log);
    let id = h.on("payments.charge@0-1:dup").await;
    h.say(
        "charge o25",
        http1_raw(p.payments, &charge("o25", Some("k25")), 2000).await,
    )
    .await;
    h.wait_log("ref=o25", 3).await;
    h.settle().await;
    h.off(id).await;
    for s in s0 + 2..=max_seq(&h.log) {
        h.racy.insert(s); // the two deliveries race
    }
    h.step("webhooks: lost, late, reorder, failing");
    let id = h.on("payments.charge@0-1:lost").await;
    h.say(
        "charge o26",
        http1_raw(p.payments, &charge("o26", None), 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("payments.charge@0-1:late=200").await;
    h.say(
        "charge o27",
        http1_raw(p.payments, &charge("o27", None), 2000).await,
    )
    .await;
    h.wait_log("ref=o27", 2).await;
    h.settle().await;
    h.off(id).await;
    let id = h.on("payments.charge@0-1:reorder").await;
    h.say(
        "charge o28",
        http1_raw(p.payments, &charge("o28", None), 2000).await,
    )
    .await;
    h.say(
        "charge o29",
        http1_raw(p.payments, &charge("o29", None), 2000).await,
    )
    .await;
    h.wait_log("ref=o28", 2).await;
    h.settle().await;
    h.say(
        "charge o32 (held)",
        http1_raw(p.payments, &charge("o32", None), 2000).await,
    )
    .await;
    h.off(id).await; // delivers the held one
    h.wait_log("ref=o32", 2).await;
    h.settle().await;
    h.say(
        "charge fail1",
        http1_raw(p.payments, &charge("fail1", None), 2000).await,
    )
    .await;
    h.wait_log("out=gave-up", 1).await;
    h.settle().await;

    // ---------------------------------------------------------------- events
    h.step("events");
    h.cli(&["event", "cancelled", "o2"]).await;
    h.cli(&["event", "cancelled", "o 3,x"]).await;
    h.cli(&["event", "shipped", ""]).await;
    h.cli(&["event", "x"]).await;

    // ---------------------------------------------------------------- the HTTP proxy
    h.step("HTTP proxy");
    h.say(
        "get /api?order=o40",
        http1_raw(
            p.shop,
            &req1("GET", "/api?order=o40", &[("X-Trace", "t1")], ""),
            2000,
        )
        .await,
    )
    .await;
    h.say(
        "post with hop headers",
        http1_raw(
            p.shop,
            &req1(
                "POST",
                "/api/x?order=o41",
                &[
                    ("Proxy-Authorization", "secret"),
                    ("Te", "trailers"),
                    ("Keep-Alive", "5"),
                    ("User-Agent", "client/2"),
                    ("Accept", "*/*"),
                ],
                "abc",
            ),
            2000,
        )
        .await,
    )
    .await;
    h.say(
        "gzip, the client asked for nothing",
        http1_raw(p.shop, &req1("GET", "/gz?order=o42", &[], ""), 2000).await,
    )
    .await;
    h.say(
        "gzip, the client asked for it",
        http1_raw(
            p.shop,
            &req1("GET", "/gz?order=o43", &[("Accept-Encoding", "gzip")], ""),
            2000,
        )
        .await,
    )
    .await;
    h.say(
        "chunked with a trailer",
        http1_raw(p.shop, &req1("GET", "/chunked?order=o44", &[], ""), 2000).await,
    )
    .await;
    h.say(
        "over HTTP/2",
        h2_call(
            p.shop,
            "GET",
            "/api?order=o45",
            &[("x-h2", "1")],
            vec![],
            0,
            2000,
            false,
        )
        .await,
    )
    .await;
    let id = h.on("shop@0-1:status=502").await;
    h.say(
        "502, not forwarded",
        http1_raw(p.shop, &req1("GET", "/api?order=o46", &[], ""), 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("shop.api@0-1:drop").await;
    h.say(
        "dropped after forwarding",
        http1_raw(p.shop, &req1("GET", "/api?order=o47", &[], ""), 300).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("shop@0-1:reset").await;
    h.say(
        "reset after forwarding",
        http1_raw(p.shop, &req1("GET", "/api?order=o48", &[], ""), 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("shop@0-1:delay=100,effect=no").await;
    h.say(
        "delay without effect",
        http1_raw(p.shop, &req1("GET", "/api?order=o49", &[], ""), 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("shop@0-1:status=503,effect=yes,retry-after=9").await;
    h.say(
        "503 after forwarding",
        http1_raw(p.shop, &req1("GET", "/api?order=o50", &[], ""), 2000).await,
    )
    .await;
    h.off(id).await;
    h.say(
        "slow upstream, client gives up",
        http1_raw(p.shop, &req1("GET", "/slow?order=o51", &[], ""), 100).await,
    )
    .await;
    h.say(
        "no upstream",
        http1_raw(p.shop2, &req1("POST", "/x?y=1", &[], "z"), 2000).await,
    )
    .await;

    // ---------------------------------------------------------------- the TCP proxy
    h.step("TCP proxy");
    h.say("echo", tcp(p.kv, b"PING\nSET a 1\n", 2000).await)
        .await;
    let id = h.on("kv@0-1:delay=100").await;
    h.say("delayed", tcp(p.kv, b"GET a\n", 2000).await).await;
    h.off(id).await;
    let id = h.on("kv@0-1:reset").await;
    h.say("reset", tcp(p.kv, b"GET b\n", 2000).await).await;
    h.off(id).await;
    let id = h.on("kv@0-1:drop").await;
    h.say("dropped", tcp(p.kv, b"GET c\n", 300).await).await;
    h.off(id).await;
    let id = h.on("kv@0-1:drop,effect=no").await;
    h.say("dropped, no effect", tcp(p.kv, b"GET d\n", 300).await)
        .await;
    h.off(id).await;
    let id = h.on("kv@0-1:reset,effect=no").await;
    h.say("reset, no effect", tcp(p.kv, b"GET e\n", 2000).await)
        .await;
    h.off(id).await;
    let id = h.on("kv@0-1:status=500").await;
    h.say("status on a byte stream", tcp(p.kv, b"GET f\n", 2000).await)
        .await;
    h.off(id).await;

    // ---------------------------------------------------------------- the gRPC proxy
    let o = p.orders;
    h.step("gRPC proxy: calls");
    h.say(
        "Unary o1",
        grpc(o, "/t.S/Unary", &[pb1("o1")], 0, 2000).await,
    )
    .await;
    h.say("Stream", grpc(o, "/t.S/Stream", &[pb1("s")], 0, 2000).await)
        .await;
    h.say(
        "Client, 3 messages",
        grpc(
            o,
            "/t.S/Client",
            &[pb1("c1"), pb1("c2"), pb1("c3")],
            20,
            2000,
        )
        .await,
    )
    .await;
    h.say(
        "Fail (trailers-only from the server)",
        grpc(o, "/t.S/Fail", &[pb1("f")], 0, 2000).await,
    )
    .await;
    h.say("unknown method", grpc(o, "/t.S/Nope", &[], 0, 2000).await)
        .await;
    h.say(
        "no upstream",
        grpc(p.orders2, "/t.S/Unary", &[pb1("o2")], 0, 2000).await,
    )
    .await;
    h.say(
        "slow, client gives up",
        grpc(o, "/t.S/Slow", &[pb1("o3")], 0, 100).await,
    )
    .await;

    h.step("gRPC proxy: faults without forwarding");
    let id = h.on("orders@0-1:status=UNAVAILABLE").await;
    h.say(
        "status=UNAVAILABLE",
        grpc(o, "/t.S/Unary", &[pb1("o4")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("orders@0-1:drop,effect=no").await;
    h.say(
        "drop, no effect",
        grpc(o, "/t.S/Unary", &[pb1("o5")], 0, 300).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("orders@0-1:reset,effect=no").await;
    h.say(
        "reset, no effect",
        grpc(o, "/t.S/Unary", &[pb1("o6")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("orders@0-1:reset=conn,effect=no").await;
    h.say(
        "reset=conn, no effect",
        grpc(o, "/t.S/Unary", &[pb1("o7")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("orders@0-1:delay=100,effect=no").await;
    h.say(
        "delay, no effect",
        grpc(o, "/t.S/Unary", &[pb1("o8")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let _ = h.on("orders@0-1:drop,effect=no").await;
    let (r, _) = join2(grpc(o, "/t.S/Unary", &[pb1("o9")], 0, 3000), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::Command::new(&h.bin)
            .args(["ctl", "heal"])
            .env("HYDO_WORLD_SOCK", &h.sock)
            .output()
            .unwrap()
    })
    .await;
    h.say("drop, no effect, healed after 200 ms", r).await;

    h.step("gRPC proxy: faults on the answer");
    let id = h.on("orders@0-1:status=ABORTED,effect=yes").await;
    h.say(
        "status=ABORTED after forwarding",
        grpc(o, "/t.S/Unary", &[pb1("o10")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("orders@0-1:drop").await;
    h.say("drop", grpc(o, "/t.S/Unary", &[pb1("o11")], 0, 300).await)
        .await;
    let (r, _) = join2(grpc(o, "/t.S/Unary", &[pb1("o12")], 0, 3000), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::Command::new(&h.bin)
            .args(["ctl", "off", &id.to_string()])
            .env("HYDO_WORLD_SOCK", &h.sock)
            .output()
            .unwrap()
    })
    .await;
    h.say("drop, off after 200 ms", r).await;
    let id = h.on("orders@0-1:delay=100").await;
    h.say("delay", grpc(o, "/t.S/Unary", &[pb1("o13")], 0, 2000).await)
        .await;
    h.off(id).await;
    let id = h.on("orders@0-1:reset").await;
    h.say("reset", grpc(o, "/t.S/Unary", &[pb1("o14")], 0, 2000).await)
        .await;
    h.off(id).await;
    let id = h.on("orders@0-1:reset=conn").await;
    h.say(
        "reset=conn",
        grpc(o, "/t.S/Unary", &[pb1("o15")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("orders.place@0-1:status=14").await;
    h.say(
        "route place: Unary",
        grpc(o, "/t.S/Unary", &[pb1("o16")], 0, 2000).await,
    )
    .await;
    h.say(
        "route place: Stream passes",
        grpc(o, "/t.S/Stream", &[pb1("s")], 0, 2000).await,
    )
    .await;
    h.off(id).await;

    h.step("gRPC proxy: streams");
    let id = h.on("orders@0-1:cut=1").await;
    h.say("cut=1", grpc(o, "/t.S/Stream", &[pb1("s")], 0, 2000).await)
        .await;
    h.off(id).await;
    let id = h.on("orders@0-1:cut=0").await;
    h.say("cut=0", grpc(o, "/t.S/Stream", &[pb1("s")], 0, 2000).await)
        .await;
    h.off(id).await;
    let id = h.on("orders@0-1:hold=1").await;
    h.say(
        "hold=1, client gives up",
        grpc(o, "/t.S/Stream", &[pb1("s")], 0, 300).await,
    )
    .await;
    let (r, _) = join2(grpc(o, "/t.S/Stream", &[pb1("s")], 0, 3000), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::Command::new(&h.bin)
            .args(["ctl", "off", &id.to_string()])
            .env("HYDO_WORLD_SOCK", &h.sock)
            .output()
            .unwrap()
    })
    .await;
    h.say("hold=1, off after 200 ms", r).await;
    let id = h.on("orders@0-1:cut=1,dir=req").await;
    h.say(
        "cut=1 dir=req",
        grpc(
            o,
            "/t.S/Client",
            &[pb1("c1"), pb1("c2"), pb1("c3")],
            20,
            2000,
        )
        .await,
    )
    .await;
    h.off(id).await;
    let _ = h.on("orders@0-1:hold=1,dir=req").await;
    let (r, _) = join2(
        grpc(
            o,
            "/t.S/Client",
            &[pb1("c1"), pb1("c2"), pb1("c3")],
            0,
            3000,
        ),
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::process::Command::new(&h.bin)
                .args(["ctl", "heal"])
                .env("HYDO_WORLD_SOCK", &h.sock)
                .output()
                .unwrap()
        },
    )
    .await;
    h.say("hold=1 dir=req, healed after 200 ms", r).await;

    h.step("gRPC proxy: rpc=, call=, p=, seed=");
    let id = h.on("orders@0-1:status=INTERNAL,rpc=Stream").await;
    h.say(
        "rpc=Stream: Unary passes",
        grpc(o, "/t.S/Unary", &[pb1("o17")], 0, 2000).await,
    )
    .await;
    h.say(
        "rpc=Stream: Stream fails",
        grpc(o, "/t.S/Stream", &[pb1("s")], 0, 2000).await,
    )
    .await;
    h.off(id).await;
    let id = h.on("orders@0-1:status=13,rpc=/t.S/Count,call=2-3").await;
    for i in 1..=4 {
        let r = grpc(o, "/t.S/Count", &[pb1(&format!("n{i}"))], 0, 2000).await;
        h.say(&format!("Count call {i}"), r).await;
    }
    h.off(id).await;
    let id = h.on("orders@0-1:status=14,p=50,seed=7").await;
    for i in 1..=6 {
        let r = grpc(o, "/t.S/Unary", &[pb1(&format!("p{i}"))], 0, 2000).await;
        h.say(&format!("p=50 call {i}"), r).await;
    }
    h.off(id).await;
    h.settle().await;
}
