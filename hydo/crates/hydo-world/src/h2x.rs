//! Cleartext HTTP/2 helpers over the `h2` crate: a connection that replays the bytes the
//! preface check read, writing an answer as Go's HTTP/2 server writes a finished handler's
//! answer, sending data within the flow-control window, and "the client went away".

use crate::goish::{self, canonical_key};
use crate::http1::{sniff, Answer, Hdrs};
use crate::rt::Gone;
use bytes::Bytes;
use h2::server::SendResponse;
use h2::SendStream;
use std::future::poll_fn;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

/// A TCP connection whose first bytes were already read.
pub struct Rewind {
    pre: Vec<u8>,
    pos: usize,
    pub io: TcpStream,
}

impl Rewind {
    pub fn new(pre: Vec<u8>, io: TcpStream) -> Rewind {
        Rewind { pre, pos: 0, io }
    }
}

impl AsyncRead for Rewind {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if self.pos < self.pre.len() {
            let n = (self.pre.len() - self.pos).min(buf.remaining());
            let p = self.pos;
            buf.put_slice(&self.pre[p..p + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Rewind {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, b)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// The server settings of Go's HTTP/2 server: 250 concurrent streams, 1 MiB receive windows.
pub fn server_builder() -> h2::server::Builder {
    let mut b = h2::server::Builder::new();
    b.max_concurrent_streams(250)
        .initial_window_size(1 << 20)
        .initial_connection_window_size(1 << 20)
        .max_header_list_size(1 << 20);
    b
}

/// The client settings of Go's HTTP/2 transport (4 MiB stream and 1 GiB connection windows).
pub fn client_builder() -> h2::client::Builder {
    let mut b = h2::client::Builder::new();
    b.initial_window_size(4 << 20)
        .initial_connection_window_size(1 << 30)
        .max_header_list_size(10 << 20);
    b
}

/// The headers of an h2 request or answer as Go sees them: canonical keys, in order.
pub fn from_map(m: &http::HeaderMap) -> Hdrs {
    m.iter()
        .map(|(k, v)| (canonical_key(k.as_str()), v.as_bytes().to_vec()))
        .collect()
}

/// Headers HTTP/2 forbids (Go drops them from an answer).
fn connection_specific(k: &str) -> bool {
    matches!(
        k,
        "Connection" | "Keep-Alive" | "Proxy-Connection" | "Transfer-Encoding" | "Upgrade"
    )
}

pub fn to_map(h: &Hdrs) -> http::HeaderMap {
    let mut m = http::HeaderMap::new();
    for (k, v) in h {
        if connection_specific(k) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            http::HeaderName::from_bytes(k.as_bytes()),
            http::HeaderValue::from_bytes(v),
        ) {
            m.append(n, v);
        }
    }
    m
}

/// The head of an answer: the status, the handler's headers, and what Go's HTTP/2 server adds
/// (Content-Length when the handler is done, Content-Type by sniffing, Date).
pub fn head(status: u16, h: &Hdrs, body: Option<&[u8]>, head_req: bool) -> http::Response<()> {
    let mut hs = h.clone();
    let has = |k: &str| h.iter().any(|(x, _)| x == k);
    let allowed = goish::body_allowed(status);
    if let Some(b) = body {
        if !has("Content-Length") && allowed && (!b.is_empty() || !head_req) {
            hs.push(("Content-Length".into(), b.len().to_string().into_bytes()));
        }
        if !has("Content-Type") && allowed && !b.is_empty() {
            hs.push(("Content-Type".into(), sniff(b).as_bytes().to_vec()));
        }
    }
    if !has("Date") {
        hs.push(("Date".into(), goish::http_date().into_bytes()));
    }
    let mut r = http::Response::new(());
    *r.status_mut() = http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::OK);
    *r.headers_mut() = to_map(&hs);
    r
}

/// Send all of data, waiting for the peer's flow-control window.
pub async fn send_all(s: &mut SendStream<Bytes>, mut data: Bytes, eos: bool) -> Result<(), h2::Error> {
    if data.is_empty() {
        return s.send_data(data, eos);
    }
    while !data.is_empty() {
        s.reserve_capacity(data.len());
        let cap = match poll_fn(|cx| s.poll_capacity(cx)).await {
            Some(c) => c?,
            None => return Err(h2::Error::from(h2::Reason::CANCEL)),
        };
        if cap == 0 {
            continue;
        }
        let chunk = data.split_to(cap.min(data.len()));
        s.send_data(chunk, eos && data.is_empty())?;
    }
    Ok(())
}

/// Write a finished handler's answer.
pub async fn write_answer(mut respond: SendResponse<Bytes>, a: Answer, head_req: bool) {
    let status = if a.status == 0 { 200 } else { a.status };
    let end = (a.trailers.is_empty() && a.body.is_empty()) || head_req;
    let resp = head(status, &a.headers, Some(&a.body), head_req);
    let Ok(mut s) = respond.send_response(resp, end) else { return };
    if end {
        return;
    }
    if !a.body.is_empty() && send_all(&mut s, Bytes::from(a.body), a.trailers.is_empty()).await.is_err() {
        return;
    }
    if !a.trailers.is_empty() {
        let _ = s.send_trailers(to_map(&a.trailers));
    }
}

/// The client reset the stream (or its connection died) before the answer's headers.
pub struct RespGone<'a>(pub &'a mut SendResponse<Bytes>);

impl Gone for RespGone<'_> {
    fn poll_gone(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.0.poll_reset(cx).map(|_| ())
    }
}

/// The same after the headers were sent.
pub struct StreamGone<'a>(pub &'a mut SendStream<Bytes>);

impl Gone for StreamGone<'_> {
    fn poll_gone(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.0.poll_reset(cx).map(|_| ())
    }
}

/// Read a request body to its end or `limit` bytes, giving the window back.
pub async fn read_body(b: &mut h2::RecvStream, limit: usize) -> Vec<u8> {
    let mut out = Vec::new();
    while out.len() < limit {
        match b.data().await {
            Some(Ok(d)) => {
                let _ = b.flow_control().release_capacity(d.len());
                let k = d.len().min(limit - out.len());
                out.extend_from_slice(&d[..k]);
            }
            _ => break,
        }
    }
    out
}
