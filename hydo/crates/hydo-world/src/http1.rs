//! HTTP/1.1 as hydo-world's Go version spoke it through `net/http`: the server side of one
//! connection (requests one after another, keep-alive, chunked bodies, `Expect: 100-continue`,
//! the answer with `Date` and `Content-Length`, an abort that closes or resets the connection),
//! and a client for one exchange per connection (`DisableKeepAlives`), for webhooks and the
//! HTTP proxy.

use crate::goish::{self, canonical_key};
use crate::rt::{self, Gone};
use std::io;
use std::os::fd::AsRawFd;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

/// Header lines: canonical key, value, in order.
pub type Hdrs = Vec<(String, Vec<u8>)>;

pub fn hget<'a>(h: &'a Hdrs, k: &str) -> Option<&'a [u8]> {
    h.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_slice())
}

/// Go's `Header.Set`: replace every value of the (canonical) key.
pub fn hset(h: &mut Hdrs, k: &str, v: Vec<u8>) {
    let k = canonical_key(k);
    h.retain(|(x, _)| *x != k);
    h.push((k, v));
}

/// Go's `Header.Add`.
pub fn hadd(h: &mut Hdrs, k: &str, v: Vec<u8>) {
    h.push((canonical_key(k), v));
}

pub fn hdel(h: &mut Hdrs, k: &str) {
    h.retain(|(x, _)| x != k);
}

/// The request line and headers of one request.
pub struct Head {
    pub method: String,
    pub target: Vec<u8>,
    /// the minor version: 1 = HTTP/1.1, 0 = HTTP/1.0
    pub minor: u8,
    pub headers: Hdrs,
}

pub enum HeadErr {
    /// the client closed before (or in the middle of) a request
    Eof,
    /// a malformed request (status, text)
    Bad(u16, &'static str),
}

pub struct Conn {
    rd: OwnedReadHalf,
    pub wr: OwnedWriteHalf,
    buf: Vec<u8>,
    eof: bool,
}

const MAX_HEAD: usize = 1 << 20;

fn parse_headers(raw: &[httparse::Header]) -> Hdrs {
    raw.iter()
        .map(|h| (canonical_key(h.name), trim(h.value).to_vec()))
        .collect()
}

fn trim(v: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = v.len();
    while a < b && (v[a] == b' ' || v[a] == b'\t') {
        a += 1;
    }
    while b > a && (v[b - 1] == b' ' || v[b - 1] == b'\t') {
        b -= 1;
    }
    &v[a..b]
}

impl Conn {
    pub fn new(s: TcpStream) -> Conn {
        Conn::with_buf(s, Vec::new())
    }

    /// A connection whose first bytes were already read (the HTTP/2 preface check).
    pub fn with_buf(s: TcpStream, buf: Vec<u8>) -> Conn {
        let (rd, wr) = s.into_split();
        Conn {
            rd,
            wr,
            buf,
            eof: false,
        }
    }

    async fn fill(&mut self) -> io::Result<usize> {
        let mut b = [0u8; 16384];
        let n = self.rd.read(&mut b).await?;
        if n == 0 {
            self.eof = true;
        }
        self.buf.extend_from_slice(&b[..n]);
        Ok(n)
    }

    /// The next request's head; Eof on a clean close.
    pub async fn read_head(&mut self) -> Result<Head, HeadErr> {
        loop {
            // Go skips empty lines before a request line
            while self.buf.starts_with(b"\r\n") {
                self.buf.drain(..2);
            }
            while self.buf.starts_with(b"\n") {
                self.buf.drain(..1);
            }
            if !self.buf.is_empty() {
                let mut hs = [httparse::EMPTY_HEADER; 256];
                let mut req = httparse::Request::new(&mut hs);
                match req.parse(&self.buf) {
                    Ok(httparse::Status::Complete(n)) => {
                        let head = Head {
                            method: req.method.unwrap_or("").to_string(),
                            target: req.path.unwrap_or("").as_bytes().to_vec(),
                            minor: req.version.unwrap_or(1),
                            headers: parse_headers(req.headers),
                        };
                        self.buf.drain(..n);
                        return Ok(head);
                    }
                    Ok(httparse::Status::Partial) => {
                        if self.buf.len() > MAX_HEAD {
                            return Err(HeadErr::Bad(431, "Request Header Fields Too Large"));
                        }
                    }
                    Err(httparse::Error::TooManyHeaders) => {
                        return Err(HeadErr::Bad(431, "Request Header Fields Too Large"))
                    }
                    Err(_) => return Err(HeadErr::Bad(400, "Bad Request")),
                }
            }
            if self.eof {
                return Err(HeadErr::Eof);
            }
            match self.fill().await {
                Ok(_) => {}
                Err(_) => return Err(HeadErr::Eof),
            }
        }
    }

    async fn need(&mut self, n: usize) -> io::Result<()> {
        while self.buf.len() < n {
            if self.eof || self.fill().await? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        Ok(())
    }

    async fn line(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if let Some(p) = self.buf.iter().position(|&c| c == b'\n') {
                let mut l: Vec<u8> = self.buf.drain(..=p).collect();
                l.pop();
                if l.last() == Some(&b'\r') {
                    l.pop();
                }
                return Ok(l);
            }
            if self.buf.len() > MAX_HEAD {
                return Err(io::ErrorKind::InvalidData.into());
            }
            if self.eof || self.fill().await? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
    }

    /// The request body (Content-Length or chunked); None = a malformed framing (400).
    pub async fn read_body(&mut self, h: &Head) -> Result<Vec<u8>, ()> {
        let te = hget(&h.headers, "Transfer-Encoding").map(|v| v.to_ascii_lowercase());
        if let Some(te) = te {
            if te != b"chunked" {
                return Err(());
            }
            let mut body = Vec::new();
            loop {
                let l = self.line().await.map_err(|_| ())?;
                let sz = l.split(|&c| c == b';').next().unwrap_or(&[]);
                let sz = std::str::from_utf8(trim(sz)).map_err(|_| ())?;
                let n = usize::from_str_radix(sz, 16).map_err(|_| ())?;
                if n == 0 {
                    // trailers, until the empty line
                    loop {
                        if self.line().await.map_err(|_| ())?.is_empty() {
                            break;
                        }
                    }
                    return Ok(body);
                }
                self.need(n + 2).await.map_err(|_| ())?;
                body.extend(self.buf.drain(..n));
                self.buf.drain(..2);
            }
        }
        let cl = match hget(&h.headers, "Content-Length") {
            None => return Ok(Vec::new()),
            Some(v) => std::str::from_utf8(v)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or(())?,
        };
        let n = usize::try_from(cl).map_err(|_| ())?;
        let mut body = Vec::new();
        let mut left = n;
        while left > 0 {
            if self.buf.is_empty() && (self.eof || self.fill().await.map_err(|_| ())? == 0) {
                return Err(());
            }
            let k = left.min(self.buf.len());
            body.extend(self.buf.drain(..k));
            left -= k;
        }
        Ok(body)
    }

    pub fn peer_ip(&self) -> String {
        self.rd
            .peer_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_default()
    }

    pub async fn write(&mut self, b: &[u8]) -> io::Result<()> {
        self.wr.write_all(b).await
    }

    /// Close the connection (HTTP/1 abort): with rst, SO_LINGER 0 makes it a reset. The halves
    /// are put together first, so the close is one close(2) (a dropped write half would send a
    /// FIN of its own first).
    pub fn abort(self, rst: bool) {
        if let Ok(s) = self.rd.reunite(self.wr) {
            if rst {
                rt::zero_linger(s.as_raw_fd());
            }
            drop(s);
        }
    }

    /// Go's background read while a handler runs: the client closed = gone. Bytes that arrive
    /// meanwhile (a pipelined request) are kept for the next request.
    pub fn gone(&mut self) -> ConnGone<'_> {
        ConnGone(self)
    }
}

pub struct ConnGone<'a>(&'a mut Conn);

impl Gone for ConnGone<'_> {
    fn poll_gone(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let c = &mut *self.0;
        if c.eof {
            return Poll::Ready(());
        }
        loop {
            let mut b = [0u8; 4096];
            let mut rb = ReadBuf::new(&mut b);
            match std::pin::Pin::new(&mut c.rd).poll_read(cx, &mut rb) {
                Poll::Ready(Ok(())) if rb.filled().is_empty() => {
                    c.eof = true;
                    return Poll::Ready(());
                }
                Poll::Ready(Ok(())) => c.buf.extend_from_slice(rb.filled()),
                Poll::Ready(Err(_)) => {
                    c.eof = true;
                    return Poll::Ready(());
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

use tokio::io::AsyncRead;

/// An answer to write.
#[derive(Default, Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Hdrs,
    pub body: Vec<u8>,
    pub trailers: Hdrs,
}

/// Go's `DetectContentType`, for the answers that carry no Content-Type (the proxied ones).
pub fn sniff(b: &[u8]) -> &'static str {
    let mut i = 0;
    while i < b.len() && matches!(b[i], b'\t' | b'\n' | 0x0c | b'\r' | b' ') {
        i += 1;
    }
    let ws = &b[i..b.len().min(i + 512)];
    const HTML: [&[u8]; 17] = [
        b"<!DOCTYPE HTML", b"<HTML", b"<HEAD", b"<SCRIPT", b"<IFRAME", b"<H1", b"<DIV", b"<FONT",
        b"<TABLE", b"<A", b"<STYLE", b"<TITLE", b"<B", b"<BODY", b"<BR", b"<P", b"<!--",
    ];
    for p in HTML {
        if ws.len() > p.len() && ws[..p.len()].eq_ignore_ascii_case(p) && matches!(ws[p.len()], b' ' | b'>') {
            return "text/html; charset=utf-8";
        }
    }
    if ws.starts_with(b"<?xml") {
        return "text/xml; charset=utf-8";
    }
    let d = &b[..b.len().min(512)];
    if d.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    if d.starts_with(b"\x89PNG\x0D\x0A\x1A\x0A") {
        return "image/png";
    }
    if d.starts_with(b"\xFF\xD8\xFF") {
        return "image/jpeg";
    }
    if d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if d.starts_with(b"\x1F\x8B\x08") {
        return "application/x-gzip";
    }
    if d.starts_with(b"\xEF\xBB\xBF") {
        return "text/plain; charset=utf-8";
    }
    if d.starts_with(b"\xFE\xFF") {
        return "text/plain; charset=utf-16be";
    }
    if d.starts_with(b"\xFF\xFE") {
        return "text/plain; charset=utf-16le";
    }
    if d.iter().any(|&c| c <= 0x08 || c == 0x0B || (0x0E..=0x1A).contains(&c) || (0x1C..=0x1F).contains(&c)) {
        return "application/octet-stream";
    }
    "text/plain; charset=utf-8"
}

/// `http.Error(rw, msg, code)`
pub fn error_answer(msg: &str, code: u16) -> Answer {
    let mut a = Answer {
        status: code,
        ..Default::default()
    };
    hset(&mut a.headers, "Content-Type", b"text/plain; charset=utf-8".to_vec());
    hset(&mut a.headers, "X-Content-Type-Options", b"nosniff".to_vec());
    a.body = format!("{msg}\n").into_bytes();
    a
}

/// The bytes of an answer as Go's HTTP/1 server writes a finished handler's answer: the headers
/// in key order, Date, Content-Length, and Connection: close when the connection ends after it.
pub fn answer_bytes(a: &Answer, head: &Head, close: bool) -> Vec<u8> {
    let mut status = a.status;
    if status == 0 {
        status = 200;
    }
    let text = goish::status_text(status);
    let mut out = if text.is_empty() {
        format!("HTTP/1.1 {status} status code {status}\r\n")
    } else {
        format!("HTTP/1.1 {status} {text}\r\n")
    }
    .into_bytes();
    let head_req = head.method == "HEAD";
    let mut hs: Vec<&(String, Vec<u8>)> = a
        .headers
        .iter()
        .filter(|(k, _)| k != "Content-Length" && k != "Transfer-Encoding" && k != "Connection")
        .collect();
    hs.sort_by(|x, y| x.0.cmp(&y.0));
    let has = |k: &str| a.headers.iter().any(|(x, _)| x == k);
    for (k, v) in hs {
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend(v.iter().map(|&c| if c == b'\r' || c == b'\n' { b' ' } else { c }));
        out.extend_from_slice(b"\r\n");
    }
    let allowed = goish::body_allowed(status);
    if !has("Content-Type") && allowed && !a.body.is_empty() && !head_req {
        out.extend_from_slice(format!("Content-Type: {}\r\n", sniff(&a.body)).as_bytes());
    }
    if !has("Date") {
        out.extend_from_slice(format!("Date: {}\r\n", goish::http_date()).as_bytes());
    }
    if allowed && !head_req {
        out.extend_from_slice(format!("Content-Length: {}\r\n", a.body.len()).as_bytes());
    }
    if close && head.minor >= 1 {
        out.extend_from_slice(b"Connection: close\r\n");
    } else if !close && head.minor == 0 {
        out.extend_from_slice(b"Connection: keep-alive\r\n");
    }
    out.extend_from_slice(b"\r\n");
    if allowed && !head_req {
        out.extend_from_slice(&a.body);
    }
    out
}

/// Does the connection end after this request? (Go: HTTP/1.0 without keep-alive, or a
/// `Connection: close`.)
pub fn wants_close(h: &Head) -> bool {
    let conn = hget(&h.headers, "Connection")
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_default();
    let has = |t: &[u8]| conn.split(|&c| c == b',').any(|x| trim(x) == t);
    if h.minor == 0 {
        !has(b"keep-alive")
    } else {
        has(b"close")
    }
}

pub fn bad_request(code: u16, text: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {code} {text}\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\n{code} {text}"
    )
    .into_bytes()
}

// ------------------------------------------------------------------ the client

/// One HTTP/1.1 exchange on its own connection.
pub struct Req<'a> {
    pub method: &'a str,
    /// host:port to dial (and the Host header)
    pub host: &'a str,
    /// the request target (path?query)
    pub target: &'a [u8],
    pub headers: &'a Hdrs,
    pub body: &'a [u8],
}

#[derive(Debug, Default)]
pub struct Resp {
    pub status: u16,
    pub headers: Hdrs,
    pub body: Vec<u8>,
    pub trailers: Hdrs,
}

/// The Go text of a failed `Client.Do`: `Post "http://h/p": <reason>`.
pub fn url_err(method: &str, url: &str, why: &str) -> String {
    let m = match method {
        "" => "Get".to_string(),
        m if m.is_ascii() => format!("{}{}", &m[..1], m[1..].to_ascii_lowercase()),
        m => m.to_string(),
    };
    format!("{m} \"{url}\": {why}")
}

pub async fn dial(host: &str) -> Result<TcpStream, String> {
    match TcpStream::connect(host).await {
        Ok(s) => {
            rt::tune(s.as_raw_fd());
            Ok(s)
        }
        Err(e) => Err(format!("dial tcp {host}: connect: {}", goish::errno_text(&e))),
    }
}

fn shall_send_length(method: &str, n: usize) -> bool {
    n > 0 || matches!(method, "POST" | "PUT" | "PATCH")
}

/// Write the request, read the answer's head and (if `body`) its body.
pub async fn roundtrip(r: &Req<'_>, body: bool) -> Result<Resp, String> {
    let s = dial(r.host).await?;
    let mut c = Conn::new(s);
    let mut out = Vec::new();
    out.extend_from_slice(r.method.as_bytes());
    out.push(b' ');
    out.extend_from_slice(if r.target.is_empty() { b"/" } else { r.target });
    out.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    out.extend_from_slice(r.host.as_bytes());
    out.extend_from_slice(b"\r\n");
    if let Some(ua) = hget(r.headers, "User-Agent") {
        out.extend_from_slice(b"User-Agent: ");
        out.extend_from_slice(ua);
        out.extend_from_slice(b"\r\n");
    }
    if shall_send_length(r.method, r.body.len()) {
        out.extend_from_slice(format!("Content-Length: {}\r\n", r.body.len()).as_bytes());
    }
    out.extend_from_slice(b"Connection: close\r\n");
    let mut hs: Vec<&(String, Vec<u8>)> = r
        .headers
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "Host" | "User-Agent" | "Content-Length" | "Transfer-Encoding" | "Trailer" | "Connection"
            )
        })
        .collect();
    hs.sort_by(|x, y| x.0.cmp(&y.0));
    for (k, v) in hs {
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(r.body);
    c.write(&out).await.map_err(|e| goish::errno_text(&e))?;
    loop {
        let (status, headers, consumed) = loop {
            let mut hs = [httparse::EMPTY_HEADER; 256];
            let mut resp = httparse::Response::new(&mut hs);
            match resp.parse(&c.buf) {
                Ok(httparse::Status::Complete(n)) => {
                    break (resp.code.unwrap_or(0), parse_headers(resp.headers), n);
                }
                Ok(httparse::Status::Partial) => {
                    if c.eof {
                        return Err(if c.buf.is_empty() { "EOF".into() } else { "unexpected EOF".into() });
                    }
                    if c.fill().await.map_err(|e| goish::errno_text(&e))? == 0 && c.buf.is_empty() {
                        return Err("EOF".into());
                    }
                }
                Err(e) => return Err(format!("malformed HTTP response: {e}")),
            }
        };
        c.buf.drain(..consumed);
        if (100..200).contains(&status) && status != 101 {
            continue; // 100 Continue and other informational answers
        }
        let mut rs = Resp {
            status,
            headers,
            ..Default::default()
        };
        if !body || r.method == "HEAD" || !goish::body_allowed(status) {
            return Ok(rs);
        }
        let te = hget(&rs.headers, "Transfer-Encoding").map(|v| v.to_ascii_lowercase());
        if te.as_deref() == Some(b"chunked") {
            loop {
                let l = c.line().await.map_err(|_| "unexpected EOF".to_string())?;
                let sz = l.split(|&x| x == b';').next().unwrap_or(&[]);
                let n = std::str::from_utf8(trim(sz))
                    .ok()
                    .and_then(|s| usize::from_str_radix(s, 16).ok())
                    .ok_or("malformed chunked encoding")?;
                if n == 0 {
                    loop {
                        let t = c.line().await.map_err(|_| "unexpected EOF".to_string())?;
                        if t.is_empty() {
                            break;
                        }
                        if let Some(p) = t.iter().position(|&x| x == b':') {
                            let k = String::from_utf8_lossy(&t[..p]).into_owned();
                            hadd(&mut rs.trailers, &k, trim(&t[p + 1..]).to_vec());
                        }
                    }
                    hdel(&mut rs.headers, "Transfer-Encoding");
                    return Ok(rs);
                }
                c.need(n + 2).await.map_err(|_| "unexpected EOF".to_string())?;
                rs.body.extend(c.buf.drain(..n));
                c.buf.drain(..2);
            }
        }
        match hget(&rs.headers, "Content-Length")
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
        {
            Some(n) => {
                c.need(n).await.map_err(|_| "unexpected EOF".to_string())?;
                rs.body = c.buf.drain(..n).collect();
            }
            None => loop {
                rs.body.append(&mut c.buf);
                if c.eof || c.fill().await.map_err(|e| goish::errno_text(&e))? == 0 {
                    rs.body.append(&mut c.buf);
                    break;
                }
            },
        }
        return Ok(rs);
    }
}

/// `roundtrip` bounded by the client's timeout (Go: `Client.Timeout`).
pub async fn roundtrip_timeout(r: &Req<'_>, body: bool, t: Duration) -> Result<Resp, String> {
    match tokio::time::timeout(t, roundtrip(r, body)).await {
        Ok(x) => x,
        Err(_) => Err("context deadline exceeded (Client.Timeout exceeded while awaiting headers)".into()),
    }
}

/// Split an http:// URL into host:port and the request target.
pub fn split_url(u: &[u8]) -> Option<(String, Vec<u8>)> {
    let s = std::str::from_utf8(u).ok()?;
    let rest = s.strip_prefix("http://").or_else(|| {
        (s.len() > 7 && s[..7].eq_ignore_ascii_case("http://")).then(|| &s[7..])
    })?;
    let (auth, path) = match rest.find(['/', '?', '#']) {
        Some(p) => (&rest[..p], &rest[p..]),
        None => (rest, ""),
    };
    let path = path.split('#').next().unwrap_or("");
    if auth.is_empty() || auth.contains(['@', ' ']) {
        return None;
    }
    let host = if auth.starts_with('[') {
        if auth.contains("]:") {
            auth.to_string()
        } else {
            format!("{auth}:80")
        }
    } else if auth.contains(':') {
        auth.to_string()
    } else {
        format!("{auth}:80")
    };
    let target = if path.is_empty() || path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_string()
    };
    Some((host, target.into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            split_url(b"http://127.0.0.1:8080/webhooks/payments"),
            Some(("127.0.0.1:8080".into(), b"/webhooks/payments".to_vec()))
        );
        assert_eq!(split_url(b"http://shop?a=1"), Some(("shop:80".into(), b"/?a=1".to_vec())));
        assert_eq!(split_url(b"https://x/"), None);
        assert_eq!(url_err("POST", "http://x/", "EOF"), "Post \"http://x/\": EOF");
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(b"hello"), "text/plain; charset=utf-8");
        assert_eq!(sniff(b"  <html><body>"), "text/html; charset=utf-8");
        assert_eq!(sniff(b"\x00\x01"), "application/octet-stream");
    }
}
