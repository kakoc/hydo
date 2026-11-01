//! gRPC-level faults (MANIFEST.md, "gRPC faults"): a streaming HTTP/2 proxy for every proxy with
//! `protocol: grpc`, and the gRPC-only rule keys of an `H=` token (parsed in the module `rules`).
//!
//! The proxy forwards each call to the real service as it arrives, message by message, in both
//! directions (unary, client-, server- and bidirectional streaming calls all pass), so a fault can
//! act after the server has answered (drop, reset, status with effect=yes) or in the middle of a
//! stream (cut=, hold=). The rules of hydo-init-fakes.sh keep their meaning; gRPC proxies add:
//!
//! ```text
//! status=<NAME|code>  answer this status without forwarding (effect=yes: forward, let the server
//!                     finish, then answer the status anyway: the server acted, the client sees a
//!                     failure). Names: UNAVAILABLE DEADLINE_EXCEEDED RESOURCE_EXHAUSTED INTERNAL
//!                     ABORTED ... (every gRPC code).
//! drop                forward; the server's answer never reaches the client, which waits until
//!                     its deadline (or the fault ends: then the stream is reset)
//! delay=<ms>          forward; hold the server's answer for ms of guest time
//! reset | reset=stream  forward; when the server answers, reset the call's HTTP/2 stream
//! reset=conn          forward; when the server answers, reset the client's TCP connection
//! cut=<n>             let n messages through (dir=), then reset the stream (dir=req: the
//!                     server's half is cancelled too)
//! hold=<n>            let n messages through (dir=), then forward nothing more in that direction
//!                     until the fault ends (then the stream continues) or the client gives up
//! dir=req|resp        the direction cut=/hold= count messages in (default resp)
//! rpc=<name>          only calls of this method: Method, Service/Method or /pkg.Service/Method
//! call=<a>[-<b>]      only the a-th (to b-th) call of a method on this proxy, counted from 1
//! seed=<n>            changes which calls a p= share hits
//! ```
//!
//! Records: at arrival the usual `kind=req` record (with `call=<k>`); at the end of the call a
//! `kind=call` record:
//!
//! ```text
//! kind=call ep=<proxy> route=<route> path=<method> ref=<ref> call=<k> req=<seq of the req record>
//!           status=<what the client got> server=<1|0|?> upstream=<server's status|-> msgs=<in>/<out>
//!           out=<rule outcome>
//! ```
//!
//! status is the grpc-status the client received (a number), rst (the stream was reset), conn (the
//! connection was reset) or none (the client gave up before any answer); server=1: the server
//! answered, 0: the call never reached it, ?: it was forwarded and the server never answered.
//!
//! One quirk of the Go version is kept on purpose, so that records and client behaviour stay the
//! same: when the real service answers trailers-only (an error status and no messages), the
//! proxy forwards that status in the first headers and ends the stream with an empty DATA frame,
//! not with trailers.

use crate::config::{body_hash, ReqView};
use crate::goish::{self, esc};
use crate::h2x;
use crate::http1::{hget, Answer, Hdrs};
use crate::rt::{self, Either, Gone, Latch, Never};
use crate::rules::{GrpcExt, Rule};
use crate::serve::{endpoint, grpc_status, is_hop, parse_target, read_preface};
use crate::world::{get, kv, Rec, World};
use bytes::Bytes;
use h2::client::SendRequest;
use h2::server::SendResponse;
use h2::{Reason, RecvStream, SendStream};
use std::future::{poll_fn, Future};
use std::os::fd::AsRawFd;
use std::pin::pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

/// The rule's outcome for the records, with the gRPC-only parts.
fn outcome_ext(r: &Rule, x: &GrpcExt) -> String {
    let mut o = r.outcome();
    if x.conn && r.reset {
        o = o.replacen("reset", "resetconn", 1);
    }
    let d = if x.dir_req { "req" } else { "resp" };
    let mut add = |s: String| {
        if !o.is_empty() {
            o.push('+');
        }
        o.push_str(&s);
    };
    if x.cut >= 0 {
        add(format!("cut{}{d}", x.cut));
    }
    if x.hold >= 0 {
        add(format!("hold{}{d}", x.hold));
    }
    o
}

// ------------------------------------------------------------------ framing

#[derive(Debug, PartialEq, Eq)]
enum FrameErr {
    /// the stream ended cleanly between messages (Go: io.EOF)
    Eof,
    /// the stream ended inside a message (Go: io.ErrUnexpectedEOF)
    Short,
    /// the stream broke off, or a message is too large
    Broken,
}

/// gRPC length-prefixed messages (5-byte header + payload), whole, from an HTTP/2 body.
struct Frames {
    s: RecvStream,
    buf: Vec<u8>,
    /// the body ended (trailers can be read)
    ended: bool,
}

impl Frames {
    fn new(s: RecvStream) -> Frames {
        Frames {
            s,
            buf: Vec::new(),
            ended: false,
        }
    }

    /// The next message (cancel-safe: data read so far stays in the buffer).
    async fn next(&mut self) -> Result<Vec<u8>, FrameErr> {
        loop {
            if self.buf.len() >= 5 {
                let n = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]])
                    as usize;
                if n > 64 << 20 {
                    return Err(FrameErr::Broken);
                }
                if self.buf.len() >= 5 + n {
                    let rest = self.buf.split_off(5 + n);
                    return Ok(std::mem::replace(&mut self.buf, rest));
                }
            }
            if self.ended {
                return Err(if self.buf.is_empty() {
                    FrameErr::Eof
                } else {
                    FrameErr::Short
                });
            }
            match self.s.data().await {
                Some(Ok(d)) => {
                    let _ = self.s.flow_control().release_capacity(d.len());
                    self.buf.extend_from_slice(&d);
                }
                Some(Err(_)) => return Err(FrameErr::Broken),
                None => self.ended = true,
            }
        }
    }

    /// The trailers, once the body ended (Go: resp.Trailer after the body's EOF).
    async fn trailers(&mut self) -> Hdrs {
        if !self.ended {
            return Hdrs::new();
        }
        match self.s.trailers().await {
            Ok(Some(t)) => h2x::from_map(&t),
            _ => Hdrs::new(),
        }
    }
}

// ------------------------------------------------------------------ the upstream connection

/// The proxy's HTTP/2 connection to the real service (Go: one `http.Transport` with cleartext
/// HTTP/2, which keeps one connection and dials again when it breaks).
struct Upstream {
    to: String,
    conn: tokio::sync::Mutex<Option<SendRequest<Bytes>>>,
}

impl Upstream {
    async fn dial(&self) -> Result<SendRequest<Bytes>, ()> {
        let s = TcpStream::connect(&self.to).await.map_err(|_| ())?;
        rt::tune(s.as_raw_fd());
        let (send, conn) = h2x::client_builder()
            .handshake::<_, Bytes>(s)
            .await
            .map_err(|_| ())?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(send)
    }

    /// A sender ready for one more request.
    async fn ready(&self) -> Result<SendRequest<Bytes>, ()> {
        for attempt in 0..2 {
            let s = {
                let mut g = self.conn.lock().await;
                let broken = match g.as_mut() {
                    None => true,
                    Some(s) => {
                        let mut cx = Context::from_waker(Waker::noop());
                        matches!(s.poll_ready(&mut cx), Poll::Ready(Err(_)))
                    }
                };
                if broken {
                    *g = Some(self.dial().await?);
                }
                g.as_ref().cloned().ok_or(())?
            };
            match s.ready().await {
                Ok(s) => return Ok(s),
                Err(_) if attempt == 0 => {
                    *self.conn.lock().await = None;
                }
                Err(_) => return Err(()),
            }
        }
        Err(())
    }
}

// ------------------------------------------------------------------ the server

pub async fn serve_grpc_proxy(w: Arc<World>, idx: usize, ln: TcpListener) {
    let up = Arc::new(Upstream {
        to: endpoint(&w, idx).to.clone(),
        conn: tokio::sync::Mutex::new(None),
    });
    loop {
        let Ok((s, _)) = ln.accept().await else {
            return;
        };
        rt::tune(s.as_raw_fd());
        tokio::spawn(grpc_conn(w.clone(), idx, up.clone(), s));
    }
}

/// One client connection: cleartext HTTP/2 with prior knowledge only (Go's server has HTTP/1 off
/// for gRPC proxies). `kill` resets the TCP connection (reset=conn).
async fn grpc_conn(w: Arc<World>, idx: usize, up: Arc<Upstream>, mut s: TcpStream) {
    let from = s
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let fd = s.as_raw_fd();
    let (pre, h2) = read_preface(&mut s).await;
    if !h2 {
        return;
    }
    let Ok(mut conn) = h2x::server_builder()
        .handshake::<_, Bytes>(h2x::Rewind::new(pre, s))
        .await
    else {
        return;
    };
    let kill = Kill {
        kill: Latch::new(),
        done: Latch::new(),
    };
    let kill = Arc::new(kill);
    loop {
        match rt::first(conn.accept(), kill.kill.wait()).await {
            Either::L(Some(Ok((req, respond)))) => {
                tokio::spawn(call(
                    w.clone(),
                    idx,
                    up.clone(),
                    req,
                    respond,
                    from.clone(),
                    kill.clone(),
                ));
            }
            Either::L(_) => {
                kill.done.fire();
                return;
            }
            Either::R(()) => {
                rt::zero_linger(fd); // closing the socket now resets the connection
                drop(conn);
                kill.done.fire();
                return;
            }
        }
    }
}

/// Resetting a client's TCP connection (reset=conn): `kill` asks the connection's task, `done`
/// says the socket is closed.
struct Kill {
    kill: Arc<Latch>,
    done: Arc<Latch>,
}

/// The client's side of the call: before the answer's headers, or after them.
enum Down {
    Head(SendResponse<Bytes>),
    Body(SendStream<Bytes>),
    Done,
}

impl Down {
    /// Reset the call's stream (Go: the handler's panic). After the answer started, the frames
    /// already queued go out first: Go flushed them before the reset, and h2 drops queued frames
    /// of a reset stream, so the connection task gets to write them.
    async fn reset(&mut self) {
        match self {
            Down::Head(r) => r.send_reset(Reason::INTERNAL_ERROR),
            Down::Body(s) => {
                for _ in 0..4 {
                    tokio::task::yield_now().await;
                }
                s.send_reset(Reason::INTERNAL_ERROR)
            }
            Down::Done => {}
        }
        *self = Down::Done;
    }
}

/// The client went away (Go: the request's context is done). Fires `l` so that the request
/// pump, which cannot poll the stream itself, sees it too.
struct DownGone<'a> {
    d: &'a mut Down,
    l: &'a Arc<Latch>,
}

impl Gone for DownGone<'_> {
    fn poll_gone(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.l.fired() {
            return Poll::Ready(());
        }
        let p = match self.d {
            Down::Head(r) => r.poll_reset(cx).map(|_| ()),
            Down::Body(s) => s.poll_reset(cx).map(|_| ()),
            Down::Done => Poll::Ready(()),
        };
        if p.is_ready() {
            self.l.fire();
        }
        p
    }
}

/// A call's end, for its kind=call record.
struct End {
    /// number, rst, conn, none
    status: String,
    /// 1, 0, ?
    server: &'static str,
    upstream: String,
    out: i64,
}

/// How the handler ended (Go: return, or one of the two panics).
enum Fin {
    Ret,
    RstStream,
    RstConn,
}

fn upstream_code(h: &Hdrs, t: &Hdrs) -> String {
    for m in [t, h] {
        if let Some(v) = hget(m, "Grpc-Status").filter(|v| !v.is_empty()) {
            return String::from_utf8_lossy(v).into_owned();
        }
    }
    "-".into()
}

/// The things a wait ends on besides the client: the rule's release and the heal.
struct Ends {
    release: Option<Arc<Latch>>,
    heal: Arc<Latch>,
}

impl Ends {
    fn latches<'a>(&'a self, extra: Option<&'a Arc<Latch>>) -> Vec<&'a Arc<Latch>> {
        let mut v: Vec<&Arc<Latch>> = self.release.iter().collect();
        v.push(&self.heal);
        v.extend(extra);
        v
    }
}

/// Go's `wait(d)`: until the client gives up, the time passes (d > 0), the rule is switched off
/// or every fault heals; true = the client is still there.
async fn wait(d: &mut Down, gone: &Arc<Latch>, ends: &Ends, ms: i64) -> bool {
    let t = (ms > 0).then(|| Duration::from_millis(ms as u64));
    let mut g = DownGone { d, l: gone };
    rt::wait(&mut g, t, &ends.latches(None)).await;
    !g.gone_now()
}

/// Go's `hang()`: forever, bounded by the client, the rule and the heal.
async fn hang(d: &mut Down, gone: &Arc<Latch>, ends: &Ends) {
    if ends.release.is_none() {
        let mut g = DownGone { d, l: gone };
        rt::wait(&mut g, None, &[]).await;
        return;
    }
    wait(d, gone, ends, 0).await;
}

/// The trailers-only answer of a fault (or of a failed forward).
async fn answer(d: &mut Down, end: &mut End, code: i64) {
    end.status = code.to_string();
    if let Down::Head(r) = std::mem::replace(d, Down::Done) {
        let mut a = Answer::default();
        grpc_status(&mut a, code, "injected by hydo");
        h2x::write_answer(r, a, false).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn call(
    w: Arc<World>,
    idx: usize,
    up: Arc<Upstream>,
    req: http::Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    from: String,
    kill: Arc<Kill>,
) {
    let e = endpoint(&w, idx);
    let (parts, body) = req.into_parts();
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .as_bytes()
        .to_vec();
    let Some((path, uri, raw_query)) = parse_target(&target) else {
        respond.send_reset(Reason::PROTOCOL_ERROR);
        return;
    };
    let method = parts.method.as_str().to_string();
    let headers = h2x::from_map(&parts.headers);
    let mut frames = Frames::new(body);
    let mut v = ReqView {
        path: path.clone(),
        header: headers.clone(),
        query: goish::parse_query(&raw_query).0,
        ..Default::default()
    };
    let (rt, vars) = e.matches(&method, &path);
    v.vars = vars;
    let mut rname = "any".to_string();
    let mut ref_spec = e.r#ref.clone();
    if let Some(r) = rt {
        rname = r.name.clone();
        if !r.r#ref.is_empty() {
            ref_spec = r.r#ref.clone();
        }
    }
    // the first message is read before the decision only when a ref or key is taken from it (a
    // bidirectional call whose server speaks first would otherwise wait for it)
    let mut first: Option<Vec<u8>> = None;
    let mut body_h = "-".to_string();
    if ref_spec.starts_with("pb:")
        || e.idempotency.starts_with("pb:")
        || ref_spec == "body"
        || e.idempotency == "body"
    {
        if let Ok(f) = frames.next().await {
            v.msg = f[5..].to_vec();
            v.body = f[5..].to_vec();
            body_h = body_hash(&f[5..]);
            first = Some(f);
        }
    }
    let (r#ref, idem) = (v.extract(&ref_spec), v.extract(&e.idempotency));

    let (rule, x, call_k, req_seq, out, ends) = {
        let mut st = w.lock_gate().await;
        let key = format!("{} {}", e.name, String::from_utf8_lossy(&path));
        let call_k = {
            let n = st.call_n.entry(key).or_insert(0);
            *n += 1;
            *n
        };
        let hit = st.grpc_rule(&e.name, &rname, &path, call_k);
        let (rule, x) = match hit {
            Some((r, x)) => (Some(r), Some(x)),
            None => (None, None),
        };
        let apply = rule.as_ref().is_none_or(|r| r.applied());
        let mut out = if apply { "fwd" } else { "-" }.to_string();
        let mut extra = format!(" call={call_k}");
        if let (Some(r), Some(x)) = (&rule, &x) {
            out = format!("{out}+{}", outcome_ext(r, x));
            extra = format!("{extra} rule={}", r.id);
        }
        let req_seq = st.record(
            "req",
            &[
                kv("ep", e.name.as_bytes()),
                kv("route", rname.as_bytes()),
                kv("from", from.as_bytes()),
                kv("method", method.as_bytes()),
                kv("path", &path),
                kv("ref", &r#ref),
                kv("idem", &idem),
                format!("body={body_h}"),
                format!("fx={}", if apply { 1 } else { 0 }),
                format!("out={out}{extra}"),
            ],
        );
        let ends = Ends {
            release: rule.as_ref().map(|r| r.release.clone()),
            heal: st.heal.clone(),
        };
        (rule, x, call_k, req_seq, out, ends)
    };

    let mut end = End {
        status: "none".into(),
        server: "0",
        upstream: "-".into(),
        out: 0,
    };
    let msgs_in = AtomicI64::new(0);
    let mut down = Down::Head(respond);
    let fin = run(
        &up,
        &uri,
        &headers,
        rule.as_deref(),
        x.as_ref(),
        &ends,
        first,
        frames,
        &mut down,
        &mut end,
        &msgs_in,
    )
    .await;
    match fin {
        Fin::Ret => {}
        Fin::RstStream => end.status = "rst".into(),
        Fin::RstConn => {
            // Go closes the connection first, then the call's record is written
            end.status = "conn".into();
            kill.kill.fire();
            let _ = tokio::time::timeout(Duration::from_secs(1), kill.done.wait()).await;
            down = Down::Done;
        }
    }
    {
        let mut st = w.lock();
        if !st.frozen {
            st.record(
                "call",
                &[
                    kv("ep", e.name.as_bytes()),
                    kv("route", rname.as_bytes()),
                    kv("path", &path),
                    kv("ref", &r#ref),
                    format!("call={call_k}"),
                    format!("req={req_seq}"),
                    format!("status={}", end.status),
                    format!("server={}", end.server),
                    format!("upstream={}", end.upstream),
                    format!("msgs={}/{}", msgs_in.load(Ordering::SeqCst), end.out),
                    format!("out={out}"),
                ],
            );
        }
    }
    if let Fin::RstStream = fin {
        down.reset().await;
    }
    drop(down);
}

/// The call after the decision: a fault without forwarding, or the forward with the faults that
/// act on the server's answer and on the streams.
#[allow(clippy::too_many_arguments)]
async fn run(
    up: &Upstream,
    uri: &[u8],
    headers: &Hdrs,
    rule: Option<&Rule>,
    x: Option<&GrpcExt>,
    ends: &Ends,
    first: Option<Vec<u8>>,
    frames: Frames,
    down: &mut Down,
    end: &mut End,
    msgs_in: &AtomicI64,
) -> Fin {
    let gone = Latch::new();
    let apply = rule.is_none_or(|r| r.applied());
    if let (false, Some(r)) = (apply, rule) {
        // the server never sees the call
        if r.delay_ms > 0 && !wait(down, &gone, ends, r.delay_ms).await {
            return Fin::Ret;
        }
        let conn = x.is_some_and(|x| x.conn);
        if r.drop {
            hang(down, &gone, ends).await;
            if !(DownGone { d: down, l: &gone }).gone_now() {
                return Fin::RstStream;
            }
            return Fin::Ret;
        }
        if r.reset && conn {
            return Fin::RstConn;
        }
        if r.reset {
            return Fin::RstStream;
        }
        let code = if r.status > 0 { r.status } else { 14 }; // UNAVAILABLE: no answer from upstream
        answer(down, end, code).await;
        return Fin::Ret;
    }

    // forward: the client's messages go up as they arrive
    let mut ureq = match http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{}{}", up.to, String::from_utf8_lossy(uri)))
        .body(())
    {
        Ok(r) => r,
        Err(_) => {
            answer(down, end, 13).await;
            return Fin::Ret;
        }
    };
    let hs: Hdrs = headers
        .iter()
        .filter(|(k, _)| !is_hop(k) && k != "Host" && k != "Content-Length")
        .cloned()
        .collect();
    let mut m = h2x::to_map(&hs);
    // what Go's HTTP/2 transport adds to a request
    if !m.contains_key(http::header::USER_AGENT) {
        m.insert(
            http::header::USER_AGENT,
            http::HeaderValue::from_static("Go-http-client/2.0"),
        );
    }
    if !m.contains_key(http::header::ACCEPT_ENCODING) && !m.contains_key(http::header::RANGE) {
        m.insert(
            http::header::ACCEPT_ENCODING,
            http::HeaderValue::from_static("gzip"),
        );
    }
    *ureq.headers_mut() = m;
    let sender = {
        let mut g = DownGone { d: down, l: &gone };
        rt::or_gone(&mut g, up.ready()).await
    };
    let mut sender = match sender {
        None => {
            end.server = "?";
            return Fin::Ret;
        }
        Some(Err(())) => {
            end.server = "?";
            answer(down, end, 14).await;
            return Fin::Ret;
        }
        Some(Ok(s)) => s,
    };
    let (resp_fut, up_tx) = match sender.send_request(ureq, false) {
        Ok(p) => p,
        Err(_) => {
            end.server = "?";
            answer(down, end, 14).await;
            return Fin::Ret;
        }
    };
    let req_cut = Latch::new();
    let pump = pump(frames, first, up_tx, x, ends, &gone, &req_cut, msgs_in);
    let main = relay(resp_fut, rule, x, ends, &gone, &req_cut, down, end);
    rt::drive(main, pump).await
}

/// The client's messages to the server, as they arrive (Go's request goroutine and pipe).
#[allow(clippy::too_many_arguments)]
async fn pump(
    mut frames: Frames,
    first: Option<Vec<u8>>,
    mut up_tx: SendStream<Bytes>,
    x: Option<&GrpcExt>,
    ends: &Ends,
    gone: &Arc<Latch>,
    req_cut: &Arc<Latch>,
    msgs_in: &AtomicI64,
) {
    let mut n: i64 = 0;
    let mut pending = first;
    loop {
        let f = match pending.take() {
            Some(f) => f,
            None => match frames.next().await {
                Ok(f) => f,
                Err(FrameErr::Eof) => {
                    let _ = up_tx.send_data(Bytes::new(), true);
                    return;
                }
                Err(_) => {
                    up_tx.send_reset(Reason::CANCEL); // Go: the body's error aborts the request
                    return;
                }
            },
        };
        if let Some(x) = x.filter(|x| x.dir_req) {
            if x.cut >= 0 && n >= x.cut {
                req_cut.fire();
                up_tx.send_reset(Reason::CANCEL);
                return;
            }
            if x.hold >= 0 && n >= x.hold {
                // held until the fault ends or the client gives up
                rt::wait(&mut Never, None, &ends.latches(Some(gone))).await;
                if gone.fired() {
                    up_tx.send_reset(Reason::CANCEL);
                    return;
                }
            }
        }
        if h2x::send_all(&mut up_tx, Bytes::from(f), false)
            .await
            .is_err()
        {
            return;
        }
        n += 1;
        msgs_in.store(n, Ordering::SeqCst);
    }
}

/// Wait for the server's answer, apply the rule to it, and relay it message by message.
#[allow(clippy::too_many_arguments)]
async fn relay(
    resp_fut: h2::client::ResponseFuture,
    rule: Option<&Rule>,
    x: Option<&GrpcExt>,
    ends: &Ends,
    gone: &Arc<Latch>,
    req_cut: &Arc<Latch>,
    down: &mut Down,
    end: &mut End,
) -> Fin {
    enum Got {
        Cut,
        Resp(Result<http::Response<RecvStream>, h2::Error>),
        Gone,
    }
    let got = {
        let mut g = DownGone { d: down, l: gone };
        let mut cut = pin!(req_cut.wait());
        let mut rf = pin!(resp_fut);
        poll_fn(|cx| {
            if cut.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Got::Cut);
            }
            if let Poll::Ready(r) = rf.as_mut().poll(cx) {
                return Poll::Ready(Got::Resp(r));
            }
            if g.poll_gone(cx).is_ready() {
                return Poll::Ready(Got::Gone);
            }
            Poll::Pending
        })
        .await
    };
    let resp = match got {
        Got::Cut => {
            // cut=, dir=req: the server's half is cancelled, the client's reset
            end.server = "?";
            return Fin::RstStream;
        }
        Got::Gone => {
            end.server = "?";
            return Fin::Ret;
        }
        Got::Resp(Err(_)) => {
            end.server = "?";
            if (DownGone { d: down, l: gone }).gone_now() {
                return Fin::Ret;
            }
            answer(down, end, 14).await;
            return Fin::Ret;
        }
        Got::Resp(Ok(r)) => r,
    };
    end.server = "1";
    let (rparts, rbody) = resp.into_parts();
    let rheaders = h2x::from_map(&rparts.headers);
    let mut ub = Frames::new(rbody);

    if let Some(r) = rule {
        if r.delay_ms > 0 && !wait(down, gone, ends, r.delay_ms).await {
            return Fin::Ret;
        }
    }
    let conn = x.is_some_and(|x| x.conn);
    if let Some(r) = rule {
        if r.drop {
            drain(&mut ub, &rheaders, down, gone, end).await;
            hang(down, gone, ends).await;
            if !(DownGone { d: down, l: gone }).gone_now() {
                return Fin::RstStream;
            }
            return Fin::Ret;
        }
        if r.reset && conn {
            return Fin::RstConn;
        }
        if r.reset {
            return Fin::RstStream;
        }
        if r.status > 0 {
            drain(&mut ub, &rheaders, down, gone, end).await;
            answer(down, end, r.status).await;
            return Fin::Ret;
        }
    }

    // relay the answer, message by message
    let hs: Hdrs = rheaders
        .iter()
        .filter(|(k, _)| !is_hop(k) && k != "Content-Length")
        .cloned()
        .collect();
    let Down::Head(mut respond) = std::mem::replace(down, Down::Done) else {
        return Fin::Ret;
    };
    let head = h2x::head(rparts.status.as_u16(), &hs, None, false);
    match respond.send_response(head, false) {
        Ok(s) => *down = Down::Body(s),
        Err(_) => return Fin::Ret,
    }
    let mut n: i64 = 0;
    loop {
        if let Some(x) = x.filter(|x| !x.dir_req) {
            if x.cut >= 0 && n >= x.cut {
                return Fin::RstStream;
            }
            if x.hold >= 0 && n >= x.hold && !wait(down, gone, ends, 0).await {
                return Fin::Ret;
            }
        }
        let f = {
            let mut g = DownGone { d: down, l: gone };
            match rt::or_gone(&mut g, ub.next()).await {
                Some(Ok(f)) => f,
                _ => break,
            }
        };
        let Down::Body(s) = down else { return Fin::Ret };
        let sent = {
            let fut = h2x::send_all(s, Bytes::from(f), false);
            let mut fut = pin!(fut);
            let gl = gone.clone();
            poll_fn(|cx| {
                if gl.fired() {
                    return Poll::Ready(false);
                }
                fut.as_mut().poll(cx).map(|r| r.is_ok())
            })
            .await
        };
        if !sent {
            return Fin::Ret;
        }
        n += 1;
        end.out = n;
    }
    let trailers = ub.trailers().await;
    end.upstream = upstream_code(&rheaders, &trailers);
    if end.upstream == "-" {
        // the server's half broke off without a status (or cut=, dir=req)
        if (DownGone { d: down, l: gone }).gone_now() {
            return Fin::Ret; // the client gave up
        }
        return Fin::RstStream;
    }
    if let Down::Body(s) = down {
        if trailers.is_empty() {
            let _ = s.send_data(Bytes::new(), true);
        } else {
            let _ = s.send_trailers(h2x::to_map(&trailers));
        }
    }
    *down = Down::Done;
    end.status = end.upstream.clone();
    Fin::Ret
}

/// Let the server finish: read its answer to the end, keep its status.
async fn drain(
    ub: &mut Frames,
    rheaders: &Hdrs,
    down: &mut Down,
    gone: &Arc<Latch>,
    end: &mut End,
) {
    {
        let mut g = DownGone { d: down, l: gone };
        rt::or_gone(&mut g, async { while ub.next().await.is_ok() {} }).await;
    }
    let t = ub.trailers().await;
    end.upstream = upstream_code(rheaders, &t);
}

/// A kind=call record as one line of the console summary.
pub fn call_summary(r: &Rec) -> Vec<u8> {
    let g = |k: &str| get(r, k);
    [
        g("seq"),
        b" call ",
        g("ep"),
        b".",
        g("route"),
        b" ",
        g("path"),
        b" ref=",
        esc(g("ref")).as_bytes(),
        b" n=",
        g("call"),
        b" status=",
        g("status"),
        b" server=",
        g("server"),
        b" upstream=",
        g("upstream"),
        b" msgs=",
        g("msgs"),
        b" ",
        g("out"),
    ]
    .concat()
}

/// The gRPC check kinds; None = not one of them.
///
/// ```text
/// check calls <proxy>[.<route>]   count the calls by outcome: answered OK; failed after the
///                                 server acted; failed before it was reached; failed, unknown
/// ```
pub fn check_ext(kind: &[u8], target: &[u8], recs: &[Rec]) -> Option<i32> {
    if kind != b"calls" {
        return None;
    }
    let (ep, route) = match target.iter().position(|&c| c == b'.') {
        Some(p) => (&target[..p], &target[p + 1..]),
        None => (target, &b""[..]),
    };
    let (mut n, mut ok, mut acted, mut before, mut unknown) = (0, 0, 0, 0, 0);
    for r in recs {
        if get(r, "kind") != b"call"
            || get(r, "ep") != ep
            || (!route.is_empty() && get(r, "route") != route)
        {
            continue;
        }
        n += 1;
        if get(r, "status") == b"0" {
            ok += 1;
        } else if get(r, "server") == b"1" {
            acted += 1;
        } else if get(r, "server") == b"0" {
            before += 1;
        } else {
            unknown += 1;
        }
    }
    let msg = format!(
        ": {n} calls: {ok} answered OK, {acted} failed after the server acted, {before} failed before reaching it, {unknown} failed with the server's part unknown"
    );
    Some(crate::check::say_ok(&[target, msg.as_bytes()].concat()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes() {
        let r = Rule::parse(1, "orders@1-2:reset=conn").unwrap();
        assert_eq!(outcome_ext(&r, r.ext.as_ref().unwrap()), "resetconn");
        let r = Rule::parse(1, "orders@1-2:cut=2,dir=req").unwrap();
        assert_eq!(outcome_ext(&r, r.ext.as_ref().unwrap()), "cut2req");
        let r = Rule::parse(1, "orders@1-2:delay=50,hold=1").unwrap();
        assert_eq!(
            outcome_ext(&r, r.ext.as_ref().unwrap()),
            "delay50+hold1resp"
        );
        let r = Rule::parse(1, "orders@1-2:status=UNAVAILABLE,effect=yes").unwrap();
        assert_eq!(outcome_ext(&r, &GrpcExt::default()), "status14");
    }

    #[test]
    fn upstream_codes() {
        let h: Hdrs = vec![("Grpc-Status".into(), b"5".to_vec())];
        let t: Hdrs = vec![("Grpc-Status".into(), b"0".to_vec())];
        assert_eq!(upstream_code(&h, &t), "0");
        assert_eq!(upstream_code(&h, &Hdrs::new()), "5");
        assert_eq!(upstream_code(&Hdrs::new(), &Hdrs::new()), "-");
    }

    #[test]
    fn summary_and_check() {
        let line = b"seq=7 t=1 kind=call ep=orders route=place path=/hydo.Orders/Place ref=o%201 call=3 req=6 status=0 server=1 upstream=0 msgs=1/1 out=fwd";
        let r = crate::world::parse_record(line).unwrap();
        assert_eq!(
            String::from_utf8(call_summary(&r)).unwrap(),
            "7 call orders.place /hydo.Orders/Place ref=o%201 n=3 status=0 server=1 upstream=0 msgs=1/1 fwd"
        );
        assert_eq!(check_ext(b"other", b"orders", &[]), None);
    }
}
