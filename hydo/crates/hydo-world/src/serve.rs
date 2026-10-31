//! `hydo-world serve`: every listener, the fake and HTTP-proxy handler (HTTP/1.1 and cleartext
//! HTTP/2 on one port), the TCP proxy, and the control socket.

use crate::check::{log_path, sock_path};
use crate::config::{render, Config, Endpoint, ReqView, Route};
use crate::goish::{self, join_host_port};
use crate::h2x;
use crate::http1::{self, hget, hset, Answer, Conn, Hdrs, HeadErr};
use crate::rt::{self, Gone, Never};
use crate::world::{kv, route_index, World};
use bytes::Bytes;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener};

pub fn serve_main(args: &[String]) -> i32 {
    if args.is_empty() {
        crate::usage();
    }
    let cfg = match Config::load(&args[0]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hydo-world: {e}");
            return 1;
        }
    };
    let mut carry = String::new();
    let mut i = 1;
    while i + 1 < args.len() {
        if args[i] == "--carry" {
            carry = args[i + 1].clone();
        }
        i += 2;
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hydo-world: {e}");
            return 1;
        }
    };
    rt.block_on(serve(cfg, carry))
}

fn or(s: &str, d: String) -> String {
    if s.is_empty() {
        d
    } else {
        s.to_string()
    }
}

async fn serve(cfg: Config, carry: String) -> i32 {
    let lp = or(&cfg.log, log_path());
    let sp = or(&cfg.sock, sock_path());
    let f = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o644)
            .open(&lp)
    };
    let f = match f {
        Ok(f) => f,
        Err(e) => {
            eprintln!("hydo-world: {}", goish::path_err("open", &lp, &e));
            return 1;
        }
    };
    let w = World::new(cfg, &lp, Some(f));
    if !carry.is_empty() {
        if let Err(e) = w.load_carry(&carry) {
            eprintln!("hydo-world: carry: {e}");
            return 1;
        }
    }
    // every listener is up before the control socket answers, so "ctl ping" = ready
    let n = w.cfg.all().count();
    for idx in 0..n {
        let e = w.cfg.all().nth(idx).cloned().unwrap_or_default();
        let a = join_host_port(&e.addr, &e.port.to_string());
        let ln = match TcpListener::bind(&a).await {
            Ok(l) => l,
            Err(err) => {
                eprintln!(
                    "hydo-world: {}: listen tcp {a}: bind: {}",
                    e.name,
                    goish::errno_text(&err)
                );
                return 1;
            }
        };
        if e.protocol == "tcp" {
            eprintln!(
                "hydo-world: proxy {} tcp on {}:{} -> {}",
                e.name, e.addr, e.port, e.to
            );
            tokio::spawn(serve_tcp(w.clone(), idx, ln));
            continue;
        }
        eprintln!(
            "hydo-world: {} {} {} on {}:{}",
            e.kind, e.name, e.protocol, e.addr, e.port
        );
        if e.kind == "proxy" && e.protocol == "grpc" {
            tokio::spawn(crate::grpc::serve_grpc_proxy(w.clone(), idx, ln)); // the streaming gRPC proxy
            continue;
        }
        tokio::spawn(serve_http(w.clone(), idx, ln));
    }
    let _ = std::fs::remove_file(&sp);
    let cl = match UnixListener::bind(&sp) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "hydo-world: control socket: listen unix {sp}: bind: {}",
                goish::errno_text(&e)
            );
            return 1;
        }
    };
    control(w, cl).await;
    0
}

pub fn endpoint(w: &World, idx: usize) -> &Endpoint {
    w.cfg.all().nth(idx).expect("endpoint index")
}

// ------------------------------------------------------------------ control socket

async fn control(w: Arc<World>, ln: UnixListener) {
    loop {
        let Ok((mut c, _)) = ln.accept().await else {
            return;
        };
        let w = w.clone();
        tokio::spawn(async move {
            let mut line = Vec::new();
            let mut b = [0u8; 4096];
            loop {
                match c.read(&mut b).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        line.extend_from_slice(&b[..n]);
                        if let Some(p) = line.iter().position(|&x| x == b'\n') {
                            line.truncate(p + 1);
                            break;
                        }
                    }
                }
            }
            let f: Vec<&[u8]> = line
                .split(|x| x.is_ascii_whitespace())
                .filter(|t| !t.is_empty())
                .collect();
            if f.is_empty() {
                return;
            }
            let out = w.command(&f);
            let _ = c.write_all(&out).await;
            let _ = c.shutdown().await;
        });
    }
}

// ------------------------------------------------------------------ HTTP/1.1 and cleartext HTTP/2

/// One request as the handler sees it.
pub struct InReq {
    pub method: String,
    /// the decoded path (Go's URL.Path)
    pub path: Vec<u8>,
    /// the request URI to forward (Go's URL.RequestURI())
    pub uri: Vec<u8>,
    pub raw_query: Vec<u8>,
    pub headers: Hdrs,
    pub body: Vec<u8>,
    pub from: String,
}

/// What the handler did: an answer, or an abort (true = reset the connection).
pub enum Outcome {
    Answer(Answer),
    Abort(bool),
}

/// Split a request target (origin form, absolute form or "*") as Go's `url.ParseRequestURI`
/// does: (decoded path, request URI, raw query); None = 400.
pub fn parse_target(t: &[u8]) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if t == b"*" {
        return Some((b"*".to_vec(), b"*".to_vec(), Vec::new()));
    }
    let rest: &[u8] = if t.starts_with(b"/") {
        t
    } else {
        let p = t.windows(3).position(|w| w == b"://")?;
        if p == 0
            || !t[..p]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(c))
        {
            return None;
        }
        let after = &t[p + 3..];
        match after.iter().position(|&c| c == b'/' || c == b'?') {
            Some(q) => &after[q..],
            None => b"",
        }
    };
    let (p, q, hasq) = match rest.iter().position(|&c| c == b'?') {
        Some(i) => (&rest[..i], &rest[i + 1..], true),
        None => (rest, &b""[..], false),
    };
    let path = goish::unescape(p, false)?;
    let mut uri = if p.is_empty() {
        b"/".to_vec()
    } else {
        p.to_vec()
    };
    if hasq {
        uri.push(b'?');
        uri.extend_from_slice(q);
    }
    Some((path, uri, q.to_vec()))
}

async fn serve_http(w: Arc<World>, idx: usize, ln: TcpListener) {
    loop {
        let Ok((s, _)) = ln.accept().await else {
            return;
        };
        rt::tune(s.as_raw_fd());
        tokio::spawn(http_conn(w.clone(), idx, s));
    }
}

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Go's prior-knowledge check: peek 14 bytes ("PRI * HTTP/2.0"), then 24 (the whole preface).
pub async fn read_preface(s: &mut TcpStream) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    for want in [14, 24] {
        while buf.len() < want {
            let mut b = [0u8; 4096];
            match s.read(&mut b).await {
                Ok(0) | Err(_) => return (buf, false),
                Ok(n) => buf.extend_from_slice(&b[..n]),
            }
        }
        if buf[..want] != H2_PREFACE[..want] {
            return (buf, false);
        }
    }
    (buf, true)
}

async fn http_conn(w: Arc<World>, idx: usize, mut s: TcpStream) {
    let from = s
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let (pre, h2) = read_preface(&mut s).await;
    if h2 {
        serve_h2(w, idx, h2x::Rewind::new(pre, s), from).await;
        return;
    }
    let mut c = Conn::with_buf(s, pre);
    loop {
        let head = match c.read_head().await {
            Ok(h) => h,
            Err(HeadErr::Eof) => return,
            Err(HeadErr::Bad(code, text)) => {
                let _ = c.write(&http1::bad_request(code, text)).await;
                return;
            }
        };
        let Some((path, uri, raw_query)) = parse_target(&head.target) else {
            let _ = c.write(&http1::bad_request(400, "Bad Request")).await;
            return;
        };
        let has_body = hget(&head.headers, "Transfer-Encoding").is_some()
            || hget(&head.headers, "Content-Length").is_some_and(|v| v != b"0");
        if head.minor >= 1
            && has_body
            && hget(&head.headers, "Expect")
                .is_some_and(|v| v.eq_ignore_ascii_case(b"100-continue"))
            && c.write(b"HTTP/1.1 100 Continue\r\n\r\n").await.is_err()
        {
            return;
        }
        let mut body = match c.read_body(&head).await {
            Ok(b) => b,
            Err(()) => {
                let _ = c.write(&http1::bad_request(400, "Bad Request")).await;
                return;
            }
        };
        body.truncate(1 << 20);
        let close = http1::wants_close(&head);
        let mut headers = head.headers.clone();
        headers.retain(|(k, _)| k != "Host");
        let rq = InReq {
            method: head.method.clone(),
            path,
            uri,
            raw_query,
            headers,
            body,
            from: from.clone(),
        };
        let out = {
            let mut g = c.gone();
            handle(&w, idx, rq, &mut g).await
        };
        match out {
            Outcome::Answer(a) => {
                if c.write(&http1::answer_bytes(&a, &head, close))
                    .await
                    .is_err()
                    || close
                {
                    return;
                }
            }
            Outcome::Abort(rst) => {
                c.abort(rst);
                return;
            }
        }
    }
}

async fn serve_h2(w: Arc<World>, idx: usize, io: h2x::Rewind, from: String) {
    let Ok(mut conn) = h2x::server_builder().handshake::<_, Bytes>(io).await else {
        return;
    };
    while let Some(r) = conn.accept().await {
        let Ok((req, respond)) = r else { break };
        let w = w.clone();
        let from = from.clone();
        tokio::spawn(async move { h2_stream(w, idx, req, respond, from).await });
    }
}

async fn h2_stream(
    w: Arc<World>,
    idx: usize,
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    from: String,
) {
    let (parts, mut body) = req.into_parts();
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .as_bytes()
        .to_vec();
    let Some((path, uri, raw_query)) = parse_target(&target) else {
        respond.send_reset(h2::Reason::PROTOCOL_ERROR);
        return;
    };
    let body = h2x::read_body(&mut body, 1 << 20).await;
    let method = parts.method.as_str().to_string();
    let rq = InReq {
        method: method.clone(),
        path,
        uri,
        raw_query,
        headers: h2x::from_map(&parts.headers),
        body,
        from,
    };
    let out = {
        let mut g = h2x::RespGone(&mut respond);
        handle(&w, idx, rq, &mut g).await
    };
    match out {
        Outcome::Answer(a) => h2x::write_answer(respond, a, method == "HEAD").await,
        Outcome::Abort(_) => respond.send_reset(h2::Reason::INTERNAL_ERROR), // Go: the handler's panic
    }
}

pub fn grpc_frame(msg: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 5];
    b[1..5].copy_from_slice(&(msg.len() as u32).to_be_bytes());
    b.extend_from_slice(msg);
    b
}

pub fn grpc_first(body: &[u8]) -> Vec<u8> {
    if body.len() < 5 {
        return Vec::new();
    }
    let n = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    if 5 + n > body.len() {
        return Vec::new();
    }
    body[5..5 + n].to_vec()
}

/// A trailers-only gRPC answer.
pub fn grpc_status(a: &mut Answer, code: i64, msg: &str) {
    hset(&mut a.headers, "Content-Type", b"application/grpc".to_vec());
    hset(&mut a.headers, "Grpc-Status", code.to_string().into_bytes());
    hset(&mut a.headers, "Grpc-Message", msg.as_bytes().to_vec());
    a.status = 200;
}

/// `hex.DecodeString`: the bytes decoded before the first bad digit.
fn hex_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    for p in b.chunks_exact(2) {
        match (char::from(p[0]).to_digit(16), char::from(p[1]).to_digit(16)) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            _ => break,
        }
    }
    out
}

pub const HOP: [&str; 7] = [
    "Connection",
    "Keep-Alive",
    "Proxy-Connection",
    "Transfer-Encoding",
    "Upgrade",
    "Proxy-Authenticate",
    "Proxy-Authorization",
];

pub fn is_hop(k: &str) -> bool {
    HOP.contains(&k)
}

/// The fake and HTTP-proxy handler (Go's `handler`).
async fn handle(w: &Arc<World>, idx: usize, rq: InReq, gone: &mut dyn Gone) -> Outcome {
    let e = endpoint(w, idx);
    let grpc = e.protocol == "grpc";
    let fake = e.kind == "fake";
    let mut v = ReqView {
        path: rq.path.clone(),
        header: rq.headers.clone(),
        query: goish::parse_query(&rq.raw_query).0,
        body: rq.body.clone(),
        ..Default::default()
    };
    if grpc {
        v.msg = grpc_first(&rq.body);
    }
    let (rt, vars) = e.matches(&rq.method, &rq.path);
    v.vars = vars;
    let mut rname = "-".to_string();
    let mut ref_spec = e.r#ref.clone();
    if let Some(r) = rt {
        rname = r.name.clone();
        if !r.r#ref.is_empty() {
            ref_spec = r.r#ref.clone();
        }
    } else if !fake {
        rname = "any".into();
    }
    let (r#ref, idem) = (v.extract(&ref_spec), v.extract(&e.idempotency));

    let (rule, ans_seq, heal) = {
        let mut st = w.lock_gate().await;
        let rule = st.request_rule(&e.name, &rname);
        let apply = rule.as_ref().is_none_or(|r| r.applied());
        let mut fx =
            apply && (!fake || (rt.is_some() && Route::effectful(rt, &rq.method, &e.protocol)));
        let seq = st.seq + 1;
        let mut ans_seq = seq;
        let mut out = if fake { "ok" } else { "fwd" }.to_string();
        if !apply {
            out = "-".into(); // the outside world never acted on it
        }
        let mut key = format!("{}/{}/", e.name, rname).into_bytes();
        key.extend_from_slice(&idem);
        let mut extra = String::new();
        if fake && fx && rt.is_some_and(|r| r.idempotent) && !idem.is_empty() {
            if let Some(&s) = st.idem.get(&key) {
                // a repeat of an idempotent request: same answer, no new effect
                fx = false;
                ans_seq = s;
                out = "replay".into();
                extra = format!(" orig={s}");
            }
        }
        if let Some(r) = &rule {
            out = format!("{out}+{}", r.outcome());
            extra = format!("{extra} rule={}", r.id);
        }
        if fx && !idem.is_empty() {
            st.idem.insert(key, seq);
        }
        st.record(
            "req",
            &[
                kv("ep", e.name.as_bytes()),
                kv("route", rname.as_bytes()),
                kv("from", rq.from.as_bytes()),
                kv("method", rq.method.as_bytes()),
                kv("path", &rq.path),
                kv("ref", &r#ref),
                kv("idem", &idem),
                format!("body={}", crate::config::body_hash(&rq.body)),
                format!("fx={}", if fx { 1 } else { 0 }),
                format!("out={out}{extra}"),
            ],
        );
        if fx && fake {
            if let Some(r) = rt.filter(|r| r.webhook.is_some()) {
                if let (Some(ei), Some(ri)) = (w.fake_index(e), route_index(e, r)) {
                    w.new_hooks(&mut st, ei, ri, &r#ref, &idem, seq);
                }
            }
        }
        (rule, ans_seq, st.heal.clone())
    };
    let apply = rule.as_ref().is_none_or(|r| r.applied());

    let seq_s = ans_seq.to_string();
    let ctx: [(&str, &[u8]); 5] = [
        ("seq", seq_s.as_bytes()),
        ("ref", &r#ref),
        ("idem", &idem),
        ("fake", e.name.as_bytes()),
        ("route", rname.as_bytes()),
    ];
    // the real service's answer (proxy) comes first: the fault acts on what it answered
    let mut up: Option<Result<http1::Resp, String>> = None;
    if !fake && apply {
        up = Some(match rt::or_gone(gone, forward(e, &rq)).await {
            Some(r) => r,
            None => Err(http1::url_err(
                &rq.method,
                &format!("http://{}{}", e.to, String::from_utf8_lossy(&rq.uri)),
                "context canceled",
            )),
        });
    }
    if let Some(r) = &rule {
        if r.delay_ms > 0 {
            rt::wait(gone, Some(Duration::from_millis(r.delay_ms as u64)), &[]).await;
        }
        if r.drop {
            // accept, never answer (until the client gives up or the fault heals)
            rt::wait(gone, None, &[&r.release, &heal]).await;
            return Outcome::Abort(false);
        }
        if r.reset {
            return Outcome::Abort(true);
        }
        if r.status > 0 {
            let mut a = Answer::default();
            if grpc {
                grpc_status(&mut a, r.status, "injected by hydo");
                return Outcome::Answer(a);
            }
            hset(&mut a.headers, "Content-Type", b"application/json".to_vec());
            if !r.retry_after.is_empty() {
                hset(
                    &mut a.headers,
                    "Retry-After",
                    r.retry_after.as_bytes().to_vec(),
                );
            }
            a.status = r.status.clamp(0, 999) as u16;
            a.body =
                format!(r#"{{"error":"injected by hydo","status":{}}}"#, r.status).into_bytes();
            return Outcome::Answer(a);
        }
    }
    if !fake {
        return match up {
            Some(Ok(resp)) => {
                let mut a = Answer::default();
                copy_response(&mut a, resp);
                Outcome::Answer(a)
            }
            Some(Err(err)) => {
                let msg = format!("hydo proxy: upstream: {err}");
                if grpc {
                    let mut a = Answer::default();
                    grpc_status(&mut a, 14, &msg);
                    Outcome::Answer(a)
                } else {
                    Outcome::Answer(http1::error_answer(&msg, 502))
                }
            }
            // effect=no with only a delay: Go's handler then copies a missing answer and
            // panics, which ends the exchange without an answer
            None => Outcome::Abort(false),
        };
    }
    let Some(r) = rt else {
        let mut a;
        if grpc {
            a = Answer::default();
            grpc_status(
                &mut a,
                12,
                &format!(
                    "hydo fake: no route for {}",
                    String::from_utf8_lossy(&rq.path)
                ),
            );
        } else {
            a = http1::error_answer(r#"{"error":"hydo fake: no route"}"#, 404);
        }
        return Outcome::Answer(a);
    };
    let mut a = Answer::default();
    for (k, val) in &r.headers {
        hset(&mut a.headers, k, render(val, &ctx, Some(&v)));
    }
    if grpc {
        let msg = hex_decode(&r.message);
        hset(&mut a.headers, "Content-Type", b"application/grpc".to_vec());
        if r.code != 0 {
            grpc_status(&mut a, r.code, "hydo fake");
            return Outcome::Answer(a);
        }
        a.status = 200;
        a.body = grpc_frame(&msg);
        a.trailers.push(("Grpc-Status".into(), b"0".to_vec()));
        a.trailers.push(("Grpc-Message".into(), Vec::new()));
        return Outcome::Answer(a);
    }
    if hget(&a.headers, "Content-Type").is_none_or(<[u8]>::is_empty) {
        hset(&mut a.headers, "Content-Type", b"application/json".to_vec());
    }
    a.status = r.status.clamp(0, 999) as u16;
    a.body = render(&r.body, &ctx, Some(&v));
    Outcome::Answer(a)
}

/// The request to the real service of an HTTP proxy.
async fn forward(e: &Endpoint, rq: &InReq) -> Result<http1::Resp, String> {
    let grpc = e.protocol == "grpc";
    let hs: Hdrs = rq
        .headers
        .iter()
        .filter(|(k, _)| !is_hop(k) && (k != "Te" || grpc))
        .cloned()
        .collect();
    let req = http1::Req {
        method: &rq.method,
        host: &e.to,
        target: &rq.uri,
        headers: &hs,
        body: &rq.body,
    };
    http1::roundtrip(&req, true).await
}

fn copy_response(a: &mut Answer, resp: http1::Resp) {
    for (k, v) in resp.headers {
        if !is_hop(&k) && k != "Content-Length" {
            a.headers.push((k, v));
        }
    }
    a.status = resp.status;
    a.body = resp.body;
    a.trailers = resp.trailers;
}

// ------------------------------------------------------------------ the TCP proxy (any protocol)

/// A fault proxy for a service that is not HTTP (Redis, a database). One connection is one
/// request in the records (route "conn"); delay = every chunk the service sends back is held
/// that long; drop = the client's bytes reach the service, its answer never comes back; reset =
/// the client's bytes reach the service, the connection is reset when the answer arrives.
/// effect=no: nothing reaches the service (drop holds the connection, reset resets it at once).
async fn serve_tcp(w: Arc<World>, idx: usize, ln: TcpListener) {
    loop {
        let Ok((s, _)) = ln.accept().await else {
            return;
        };
        rt::tune(s.as_raw_fd());
        tokio::spawn(tcp_conn(w.clone(), idx, s));
    }
}

/// Close a connection with a reset.
fn rst(s: TcpStream) {
    rt::zero_linger(s.as_raw_fd());
    drop(s);
}

enum Down {
    Rst,
    Done,
}

async fn tcp_conn(w: Arc<World>, idx: usize, c: TcpStream) {
    let e = endpoint(&w, idx);
    let from = c
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let (rule, heal) = {
        let mut st = w.lock_gate().await;
        let rule = st.request_rule(&e.name, "conn");
        // status= has no meaning on a byte stream
        let apply = rule.as_ref().is_none_or(|r| r.applied() || r.status > 0);
        let mut out = if apply { "fwd" } else { "-" }.to_string();
        let mut extra = String::new();
        if let Some(r) = &rule {
            out = format!("{out}+{}", r.outcome());
            extra = format!(" rule={}", r.id);
        }
        st.record(
            "req",
            &[
                kv("ep", e.name.as_bytes()),
                "route=conn".into(),
                kv("from", from.as_bytes()),
                format!("fx={}", if apply { 1 } else { 0 }),
                format!("out={out}{extra}"),
            ],
        );
        (rule, st.heal.clone())
    };
    let apply = rule.as_ref().is_none_or(|r| r.applied() || r.status > 0);
    let hold = || async {
        if let Some(r) = &rule {
            rt::wait(&mut Never, None, &[&r.release, &heal]).await;
        }
    };
    let (mut crd, mut cwr) = c.into_split();
    if !apply {
        if rule.as_ref().is_some_and(|r| r.reset) {
            if let Ok(s) = crd.reunite(cwr) {
                rst(s);
            }
            return;
        }
        let discard = async {
            let mut b = [0u8; 32 << 10];
            while let Ok(n) = crd.read(&mut b).await {
                if n == 0 {
                    break;
                }
            }
        };
        rt::drive(hold(), discard).await;
        if let Ok(s) = crd.reunite(cwr) {
            drop(s);
        }
        return;
    }
    let u = match TcpStream::connect(&e.to).await {
        Ok(u) => u,
        Err(_) => return,
    };
    rt::tune(u.as_raw_fd());
    let (mut urd, mut uwr) = u.into_split();
    let upward = async {
        let _ = tokio::io::copy(&mut crd, &mut uwr).await;
        let _ = uwr.shutdown().await;
    };
    let downward = async {
        let mut buf = vec![0u8; 32 << 10];
        loop {
            let r = urd.read(&mut buf).await;
            let n = *r.as_ref().unwrap_or(&0);
            if n > 0 {
                if let Some(r) = &rule {
                    if r.reset {
                        return Down::Rst;
                    }
                    if r.drop {
                        continue;
                    }
                    if r.delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(r.delay_ms as u64)).await;
                    }
                }
                if cwr.write_all(&buf[..n]).await.is_err() {
                    return Down::Done;
                }
            }
            if n == 0 {
                break;
            }
        }
        if rule.as_ref().is_some_and(|r| r.drop) {
            hold().await;
        }
        let _ = cwr.shutdown().await;
        Down::Done
    };
    let d = rt::drive(downward, upward).await;
    if let Ok(s) = crd.reunite(cwr) {
        match d {
            Down::Rst => rst(s),
            Down::Done => drop(s),
        }
    }
}
