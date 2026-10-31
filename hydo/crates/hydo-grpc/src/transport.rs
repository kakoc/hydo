//! A small gRPC-over-HTTP/2 client (cleartext, no compression), shaped after grpc-go's channel so
//! a call sees what the Go client saw: one connection per target, made on the first call; a call
//! waits while the connection is being made and fails at once (UNAVAILABLE, with the dial error)
//! while it is in backoff after a failed attempt; reconnects in the background after 1 s, 1.6 s,
//! 2.56 s, ... (grpc-go's backoff without its jitter); a connection that is lost makes the next
//! call reconnect; a connection is ready once the server's preface arrived. Every attempt has a
//! deadline, sent as `grpc-timeout`; at the deadline the stream is reset (CANCEL). Status codes
//! and messages follow grpc-go's for the cases a fault produces: dial errors, lost connections,
//! RST_STREAM codes, HTTP status codes, missing trailers, oversized messages.

use crate::outcome as oc;
use bytes::{Buf, Bytes, BytesMut};
use h2::client::SendRequest;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::time::{sleep, timeout_at, Instant};

/// A call's final status.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub code: u32,
    pub msg: String,
}

impl Status {
    pub fn new(code: u32, msg: impl Into<String>) -> Status {
        Status { code, msg: msg.into() }
    }

    pub fn ok() -> Status {
        Status::new(oc::OK, "")
    }

    pub fn deadline() -> Status {
        Status::new(oc::DEADLINE_EXCEEDED, "context deadline exceeded")
    }
}

/// grpc-go's default receive limit.
pub const MAX_RECV: usize = 4 * 1024 * 1024;

enum St {
    Idle,
    Connecting,
    Ready(SendRequest<Bytes>, u64),
    /// in backoff (or reconnecting after it): calls fail with this error
    Failure(String),
}

struct Inner {
    st: St,
    conn_id: u64,
}

/// One target's connection.
pub struct Channel {
    target: String,
    inner: Arc<Mutex<Inner>>,
    tx: Arc<watch::Sender<u64>>,
}

/// grpc-go's backoff (base 1 s, factor 1.6, at most 120 s) without its random jitter.
fn backoff(retries: u32) -> Duration {
    if retries == 0 {
        return Duration::from_secs(1);
    }
    let (mut b, max) = (1.0f64, 120.0f64);
    let mut n = retries;
    while b < max && n > 0 {
        b *= 1.6;
        n -= 1;
    }
    Duration::from_secs_f64(b.min(max))
}

/// Go's text for a dial error ("dial tcp 127.0.0.1:7070: connect: connection refused").
fn dial_error(target: &str, e: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    let what = match e.kind() {
        K::ConnectionRefused => "connect: connection refused".to_string(),
        K::TimedOut => "i/o timeout".to_string(),
        K::ConnectionReset => "connect: connection reset by peer".to_string(),
        K::AddrNotAvailable => "connect: cannot assign requested address".to_string(),
        _ => match e.raw_os_error() {
            Some(libc::ENETUNREACH) => "connect: network is unreachable".to_string(),
            Some(libc::EHOSTUNREACH) => "connect: no route to host".to_string(),
            _ => format!("connect: {}", crate::io_msg(e)),
        },
    };
    format!("dial tcp {target}: {what}")
}

fn conn_error(desc: &str) -> String {
    format!("connection error: desc = {}", crate::gofmt::quote(desc))
}

async fn resolve(target: &str) -> std::io::Result<std::net::SocketAddr> {
    if let Ok(a) = target.parse::<std::net::SocketAddr>() {
        return Ok(a);
    }
    let mut it = tokio::net::lookup_host(target).await?;
    it.next().ok_or_else(|| std::io::Error::other("no such host"))
}

/// TCP, the HTTP/2 handshake and the server's preface (a PING answered after its SETTINGS).
async fn dial(target: &str) -> Result<(SendRequest<Bytes>, h2::client::Connection<TcpStream, Bytes>), String> {
    let addr = resolve(target).await.map_err(|e| format!("transport: Error while dialing: dial tcp: lookup {target}: {}", crate::io_msg(&e)))?;
    let tcp = TcpStream::connect(addr).await.map_err(|e| format!("transport: Error while dialing: {}", dial_error(target, &e)))?;
    let _ = tcp.set_nodelay(true);
    let (send, mut conn) = h2::client::Builder::new()
        .enable_push(false)
        .handshake::<_, Bytes>(tcp)
        .await
        .map_err(|e| format!("transport: failed to write client preface: {e}"))?;
    let mut pp = conn.ping_pong().expect("ping_pong taken once");
    // drive the connection until the server answers the PING (its SETTINGS come first)
    let ping = pp.ping(h2::Ping::opaque());
    tokio::pin!(ping);
    let mut c = conn;
    let r = tokio::select! {
        r = &mut ping => r.map(|_| ()).map_err(|e| e.to_string()),
        r = &mut c => Err(match r { Ok(()) => "EOF".to_string(), Err(e) => h2_text(&e) }),
    };
    r.map_err(|e| format!("transport: failed to receive the preface from server: {e}"))?;
    Ok((send, c))
}

fn h2_text(e: &h2::Error) -> String {
    if let Some(io) = e.get_io() {
        return match io.kind() {
            std::io::ErrorKind::UnexpectedEof => "EOF".into(),
            std::io::ErrorKind::ConnectionReset => "read: connection reset by peer".into(),
            _ => crate::io_msg(io),
        };
    }
    e.to_string()
}

impl Channel {
    pub fn new(target: &str) -> Channel {
        let (tx, _) = watch::channel(0u64);
        Channel { target: target.to_string(), inner: Arc::new(Mutex::new(Inner { st: St::Idle, conn_id: 0 })), tx: Arc::new(tx) }
    }

    fn bump(tx: &watch::Sender<u64>) {
        tx.send_modify(|v| *v += 1);
    }

    /// The connection task: dial until a connection is ready (in backoff between attempts), then
    /// drive it; when it ends, the channel is idle again.
    fn spawn_connect(&self) {
        let target = self.target.clone();
        let inner = self.inner.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut retries = 0u32;
            loop {
                let start = Instant::now();
                let r = timeout_at(start + Duration::from_secs(20), dial(&target)).await;
                let r = match r {
                    Ok(r) => r,
                    Err(_) => Err(format!("transport: Error while dialing: dial tcp {target}: i/o timeout")),
                };
                match r {
                    Ok((send, conn)) => {
                        let id = {
                            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                            g.conn_id += 1;
                            g.st = St::Ready(send, g.conn_id);
                            g.conn_id
                        };
                        Channel::bump(&tx);
                        let _ = conn.await;
                        {
                            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                            if matches!(g.st, St::Ready(_, x) if x == id) {
                                g.st = St::Idle;
                            }
                        }
                        Channel::bump(&tx);
                        return;
                    }
                    Err(e) => {
                        {
                            let mut g = inner.lock().unwrap_or_else(|e| e.into_inner());
                            g.st = St::Failure(conn_error(&e));
                        }
                        Channel::bump(&tx);
                        let wait = backoff(retries);
                        retries += 1;
                        // the backoff runs from the start of the failed attempt, as in grpc-go
                        sleep((start + wait).saturating_duration_since(Instant::now())).await;
                    }
                }
            }
        });
    }

    /// A ready connection, or the status the call ends with.
    async fn get(&self, deadline: Instant) -> Result<SendRequest<Bytes>, Status> {
        loop {
            let mut rx = self.tx.subscribe();
            {
                let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                match &g.st {
                    St::Ready(s, _) => return Ok(s.clone()),
                    St::Failure(e) => return Err(Status::new(oc::UNAVAILABLE, e.clone())),
                    St::Idle => {
                        g.st = St::Connecting;
                        drop(g);
                        self.spawn_connect();
                    }
                    St::Connecting => {}
                }
            }
            if timeout_at(deadline, rx.changed()).await.is_err() {
                return Err(Status::deadline());
            }
        }
    }

    /// One call: sends the messages (one for a unary or server-streaming RPC) and returns every
    /// response message with the final status.
    pub async fn call(&self, path: &str, reqs: &[Vec<u8>], client_streaming: bool, deadline: Instant) -> (Vec<Vec<u8>>, Status) {
        let mut got = Vec::new();
        let st = match timeout_at(deadline, self.attempt(path, reqs, client_streaming, deadline, &mut got)).await {
            Ok(s) => s,
            Err(_) => Status::deadline(),
        };
        (got, st)
    }

    async fn attempt(&self, path: &str, reqs: &[Vec<u8>], client_streaming: bool, deadline: Instant, got: &mut Vec<Vec<u8>>) -> Status {
        let sr = match self.get(deadline).await {
            Ok(s) => s,
            Err(st) => return st,
        };
        let mut sr = match sr.ready().await {
            Ok(s) => s,
            Err(e) => return lost(&e),
        };
        let req = http::Request::builder()
            .method("POST")
            .uri(format!("http://{}{}", self.target, path))
            .header("content-type", "application/grpc")
            .header("user-agent", concat!("hydo-grpc/", env!("CARGO_PKG_VERSION")))
            .header("te", "trailers")
            .header("grpc-timeout", encode_timeout(deadline.saturating_duration_since(Instant::now())))
            .body(())
            .expect("request");
        let (resp, mut send) = match sr.send_request(req, false) {
            Ok(x) => x,
            Err(e) => return lost(&e),
        };
        for (i, m) in reqs.iter().enumerate() {
            let mut f = BytesMut::with_capacity(5 + m.len());
            f.extend_from_slice(&[0]);
            f.extend_from_slice(&(m.len() as u32).to_be_bytes());
            f.extend_from_slice(m);
            let last = !client_streaming && i + 1 == reqs.len();
            if send.send_data(f.freeze(), last).is_err() {
                break; // the server ended the stream; its status follows
            }
        }
        if client_streaming || reqs.is_empty() {
            let _ = send.send_data(Bytes::new(), true);
        }
        let resp = match resp.await {
            Ok(r) => r,
            Err(e) => return lost(&e),
        };
        let (parts, mut body) = resp.into_parts();
        if let Some(st) = check_headers(&parts.status, &parts.headers) {
            return st;
        }
        let compressed = parts.headers.get("grpc-encoding").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        // a trailers-only answer: the status is in the headers
        let header_status = status_of(&parts.headers);
        let mut buf = BytesMut::new();
        let mut any_data = false;
        while let Some(chunk) = body.data().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => return lost(&e),
            };
            let n = chunk.len();
            any_data = true;
            buf.extend_from_slice(&chunk);
            let _ = body.flow_control().release_capacity(n);
            loop {
                if buf.len() < 5 {
                    break;
                }
                let flag = buf[0];
                let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
                if len > MAX_RECV {
                    return Status::new(oc::RESOURCE_EXHAUSTED, format!("grpc: received message larger than max ({len} vs. {MAX_RECV})"));
                }
                if buf.len() < 5 + len {
                    break;
                }
                if flag == 1 {
                    if compressed.is_empty() || compressed == "identity" {
                        return Status::new(oc::INTERNAL, "grpc: compressed flag set with identity or empty encoding");
                    }
                    return Status::new(oc::UNIMPLEMENTED, format!("grpc: Decompressor is not installed for grpc-encoding {}", crate::gofmt::quote(&compressed)));
                }
                buf.advance(5);
                got.push(buf.split_to(len).to_vec());
            }
        }
        if let Some(st) = header_status.clone() {
            if !any_data {
                return st;
            }
        }
        match body.trailers().await {
            Ok(Some(t)) => status_of(&t).unwrap_or_else(|| Status::new(oc::UNKNOWN, "")),
            Ok(None) => header_status.unwrap_or_else(|| Status::new(oc::INTERNAL, "server closed the stream without sending trailers")),
            Err(e) => lost(&e),
        }
    }
}

/// grpc-go's grpc-timeout encoding: the smallest unit whose value has at most 8 digits, rounded up.
pub fn encode_timeout(t: Duration) -> String {
    let ns = t.as_nanos() as i64;
    if ns <= 0 {
        return "0n".into();
    }
    const MAX: i64 = 100_000_000 - 1;
    for (unit, d) in [("n", 1i64), ("u", 1_000), ("m", 1_000_000), ("S", 1_000_000_000), ("M", 60_000_000_000), ("H", 3_600_000_000_000)] {
        let v = (ns + d - 1) / d;
        if v <= MAX {
            return format!("{v}{unit}");
        }
    }
    format!("{}H", (ns + 3_600_000_000_000 - 1) / 3_600_000_000_000)
}

fn reason_name(r: u32) -> String {
    match r {
        0 => "NO_ERROR".into(),
        1 => "PROTOCOL_ERROR".into(),
        2 => "INTERNAL_ERROR".into(),
        3 => "FLOW_CONTROL_ERROR".into(),
        4 => "SETTINGS_TIMEOUT".into(),
        5 => "STREAM_CLOSED".into(),
        6 => "FRAME_SIZE_ERROR".into(),
        7 => "REFUSED_STREAM".into(),
        8 => "CANCEL".into(),
        9 => "COMPRESSION_ERROR".into(),
        10 => "CONNECT_ERROR".into(),
        11 => "ENHANCE_YOUR_CALM".into(),
        12 => "INADEQUATE_SECURITY".into(),
        13 => "HTTP_1_1_REQUIRED".into(),
        x => format!("unknown error code 0x{x:x}"),
    }
}

/// grpc-go's http2ErrConvTab.
fn reason_code(r: u32) -> u32 {
    match r {
        3 | 11 => oc::RESOURCE_EXHAUSTED,
        7 => oc::UNAVAILABLE,
        8 => oc::CANCELED,
        12 => oc::PERMISSION_DENIED,
        _ => oc::INTERNAL,
    }
}

/// The status of a call whose stream or connection failed.
fn lost(e: &h2::Error) -> Status {
    if e.is_reset() && e.is_remote() {
        if let Some(r) = e.reason() {
            let r = u32::from(r);
            return Status::new(reason_code(r), format!("stream terminated by RST_STREAM with error code: {}", reason_name(r)));
        }
    }
    if e.is_go_away() {
        return Status::new(oc::UNAVAILABLE, "the connection is draining");
    }
    Status::new(oc::UNAVAILABLE, format!("error reading from server: {}", h2_text(e)))
}

fn status_text(c: u16) -> &'static str {
    match c {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Request Entity Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn valid_content_type(v: &str) -> bool {
    let Some(rest) = v.strip_prefix("application/grpc") else { return false };
    rest.is_empty() || rest.starts_with('+') || rest.starts_with(';')
}

/// grpc-go's checks of the response headers.
fn check_headers(status: &http::StatusCode, h: &http::HeaderMap) -> Option<Status> {
    let mut errs = Vec::new();
    let code = status.as_u16();
    let ct = h.get("content-type").map(|v| v.to_str().unwrap_or(""));
    let mut grpc = false;
    let mut ct_err = Some("malformed header: missing HTTP content-type".to_string());
    if let Some(ct) = ct {
        if valid_content_type(ct) {
            ct_err = None;
            grpc = true;
        } else {
            ct_err = Some(format!("transport: received unexpected content-type {}", crate::gofmt::quote(ct)));
        }
    }
    let http_err = if code == 200 { None } else { Some(format!("unexpected HTTP status code received from server: {} ({})", code, status_text(code))) };
    if grpc && http_err.is_none() {
        return None;
    }
    let c = match code {
        400 => oc::INTERNAL,
        401 => oc::UNAUTHENTICATED,
        403 => oc::PERMISSION_DENIED,
        404 => oc::UNIMPLEMENTED,
        429 | 502 | 503 | 504 => oc::UNAVAILABLE,
        _ => oc::UNKNOWN,
    };
    if let Some(e) = http_err {
        errs.push(e);
    }
    if let Some(e) = ct_err {
        errs.push(e);
    }
    Some(Status::new(c, errs.join("; ")))
}

/// grpc-go's decodeGrpcMessage: %XX sequences decoded, anything else kept.
fn decode_message(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).expect("hex"));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn status_of(h: &http::HeaderMap) -> Option<Status> {
    let s = h.get("grpc-status")?;
    let s = s.to_str().unwrap_or("");
    let code = match s.parse::<i64>() {
        Ok(c) if (i32::MIN as i64..=i32::MAX as i64).contains(&c) => c as i32 as u32,
        _ => return Some(Status::new(oc::INTERNAL, format!("transport: malformed grpc-status: strconv.ParseInt: parsing {}: invalid syntax", crate::gofmt::quote(s)))),
    };
    let msg = h.get("grpc-message").map(|v| decode_message(&String::from_utf8_lossy(v.as_bytes()))).unwrap_or_default();
    Some(Status::new(code, msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts() {
        assert_eq!(encode_timeout(Duration::from_millis(1000)), "1000000u");
        assert_eq!(encode_timeout(Duration::from_nanos(999_985_123)), "999986u");
        assert_eq!(encode_timeout(Duration::from_nanos(5)), "5n");
        assert_eq!(encode_timeout(Duration::from_secs(20)), "20000000u");
        assert_eq!(encode_timeout(Duration::from_secs(200)), "200000m");
        assert_eq!(encode_timeout(Duration::ZERO), "0n");
    }

    #[test]
    fn messages() {
        assert_eq!(decode_message("a%20b%zz%4"), "a b%zz%4");
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(1), Duration::from_secs_f64(1.6));
        assert_eq!(backoff(20), Duration::from_secs(120));
        assert_eq!(conn_error("x \"y\""), "connection error: desc = \"x \\\"y\\\"\"");
    }
}
