//! TCP (and Unix socket) connections with one deadline for the whole read, and Go's net error
//! texts ("dial tcp 127.0.0.1:3306: connect: connection refused", "... i/o timeout", "EOF").

use crate::gofmt::errno_text;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// Why a network operation failed.
#[derive(Debug, Clone, PartialEq)]
pub enum NetErr {
    /// The peer closed the connection (Go's io.EOF; "unexpected EOF" inside a message).
    Eof,
    /// The deadline passed ("... i/o timeout").
    Timeout(String),
    /// Anything else, with Go's full text.
    Other(String),
}

impl std::fmt::Display for NetErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetErr::Eof => write!(f, "EOF"),
            NetErr::Timeout(s) | NetErr::Other(s) => write!(f, "{s}"),
        }
    }
}

enum Sock {
    Tcp(TcpStream),
    Unix(UnixStream),
}

pub struct Conn {
    sock: Sock,
    /// "tcp 127.0.0.1:41234->127.0.0.1:3306" for error texts.
    desc: String,
    pub deadline: Instant,
    buf: Vec<u8>,
    pos: usize,
}

fn remaining(deadline: Instant) -> Option<Duration> {
    let now = Instant::now();
    if now >= deadline {
        None
    } else {
        Some(deadline - now)
    }
}

/// Go's SplitHostPort, enough for host:port, [v6]:port.
pub fn split_host_port(addr: &str) -> Result<(String, String), String> {
    if let Some(rest) = addr.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let host = &rest[..end];
            let after = &rest[end + 1..];
            if let Some(p) = after.strip_prefix(':') {
                return Ok((host.to_string(), p.to_string()));
            }
            return Err(format!("address {addr}: missing port in address"));
        }
        return Err(format!("address {addr}: missing ']' in address"));
    }
    match addr.rfind(':') {
        None => Err(format!("address {addr}: missing port in address")),
        Some(i) => {
            let host = &addr[..i];
            if host.contains(':') {
                return Err(format!("address {addr}: too many colons in address"));
            }
            Ok((host.to_string(), addr[i + 1..].to_string()))
        }
    }
}

/// `net.Dial("tcp", addr)` with the deadline.
pub fn dial_tcp(addr: &str, deadline: Instant) -> Result<Conn, NetErr> {
    let (host, port) =
        split_host_port(addr).map_err(|e| NetErr::Other(format!("dial tcp: {e}")))?;
    let host = if host.is_empty() {
        "127.0.0.1".to_string()
    } else {
        host
    };
    let portn: u16 = match port.parse() {
        Ok(p) => p,
        Err(_) => {
            return Err(NetErr::Other(format!(
                "dial tcp: lookup tcp/{port}: unknown port"
            )))
        }
    };
    let addrs: Vec<SocketAddr> = match (host.as_str(), portn).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(_) => {
            return Err(NetErr::Other(format!(
                "dial tcp: lookup {host}: no such host"
            )))
        }
    };
    if addrs.is_empty() {
        return Err(NetErr::Other(format!(
            "dial tcp: lookup {host}: no such host"
        )));
    }
    let mut first: Option<NetErr> = None;
    for sa in addrs {
        let Some(left) = remaining(deadline) else {
            return Err(first.unwrap_or(NetErr::Timeout(format!("dial tcp {sa}: i/o timeout"))));
        };
        match TcpStream::connect_timeout(&sa, left) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                let local = s.local_addr().map(|l| l.to_string()).unwrap_or_default();
                return Ok(Conn {
                    sock: Sock::Tcp(s),
                    desc: format!("tcp {local}->{sa}"),
                    deadline,
                    buf: Vec::new(),
                    pos: 0,
                });
            }
            Err(e) => {
                let err = if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock {
                    NetErr::Timeout(format!("dial tcp {sa}: i/o timeout"))
                } else {
                    NetErr::Other(format!("dial tcp {sa}: connect: {}", errno_text(&e)))
                };
                if first.is_none() {
                    first = Some(err);
                }
            }
        }
    }
    Err(first.unwrap_or(NetErr::Other("dial tcp: no address".into())))
}

/// `net.Dial("unix", path)`.
pub fn dial_unix(path: &str, deadline: Instant) -> Result<Conn, NetErr> {
    match UnixStream::connect(path) {
        Ok(s) => Ok(Conn {
            sock: Sock::Unix(s),
            desc: format!("unix ->{path}"),
            deadline,
            buf: Vec::new(),
            pos: 0,
        }),
        Err(e) => Err(NetErr::Other(format!(
            "dial unix {path}: connect: {}",
            errno_text(&e)
        ))),
    }
}

impl Conn {
    fn io_err(&self, op: &str, e: std::io::Error) -> NetErr {
        match e.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => {
                NetErr::Timeout(format!("{op} {}: i/o timeout", self.desc))
            }
            ErrorKind::UnexpectedEof => NetErr::Eof,
            _ => NetErr::Other(format!("{op} {}: {op}: {}", self.desc, errno_text(&e))),
        }
    }

    fn set_timeouts(&self) -> Result<(), NetErr> {
        let Some(left) = remaining(self.deadline) else {
            return Err(NetErr::Timeout(format!("read {}: i/o timeout", self.desc)));
        };
        let r = match &self.sock {
            Sock::Tcp(s) => s
                .set_read_timeout(Some(left))
                .and_then(|_| s.set_write_timeout(Some(left))),
            Sock::Unix(s) => s
                .set_read_timeout(Some(left))
                .and_then(|_| s.set_write_timeout(Some(left))),
        };
        r.map_err(|e| self.io_err("read", e))
    }

    pub fn write_all(&mut self, b: &[u8]) -> Result<(), NetErr> {
        self.set_timeouts()
            .map_err(|_| NetErr::Timeout(format!("write {}: i/o timeout", self.desc)))?;
        let r = match &mut self.sock {
            Sock::Tcp(s) => s.write_all(b),
            Sock::Unix(s) => s.write_all(b),
        };
        r.map_err(|e| self.io_err("write", e))
    }

    fn fill(&mut self) -> Result<(), NetErr> {
        if self.pos < self.buf.len() {
            return Ok(());
        }
        self.set_timeouts()?;
        self.buf.resize(65536, 0);
        self.pos = 0;
        let r = match &mut self.sock {
            Sock::Tcp(s) => s.read(&mut self.buf),
            Sock::Unix(s) => s.read(&mut self.buf),
        };
        match r {
            Ok(0) => {
                self.buf.clear();
                Err(NetErr::Eof)
            }
            Ok(n) => {
                self.buf.truncate(n);
                Ok(())
            }
            Err(e) => {
                self.buf.clear();
                Err(self.io_err("read", e))
            }
        }
    }

    pub fn read_byte(&mut self) -> Result<u8, NetErr> {
        self.fill()?;
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// Exactly n bytes; the peer closing part-way is `Eof` too.
    pub fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, NetErr> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            self.fill()?;
            let take = (n - out.len()).min(self.buf.len() - self.pos);
            out.extend_from_slice(&self.buf[self.pos..self.pos + take]);
            self.pos += take;
        }
        Ok(out)
    }

    /// Up to and including the next '\n' (bufio.Reader.ReadString('\n')).
    pub fn read_line(&mut self) -> Result<Vec<u8>, NetErr> {
        let mut out = Vec::new();
        loop {
            self.fill()?;
            let rest = &self.buf[self.pos..];
            if let Some(i) = rest.iter().position(|&c| c == b'\n') {
                out.extend_from_slice(&rest[..=i]);
                self.pos += i + 1;
                return Ok(out);
            }
            out.extend_from_slice(rest);
            self.pos = self.buf.len();
        }
    }

    /// Everything until the peer closes the connection.
    pub fn read_to_end(&mut self) -> Result<Vec<u8>, NetErr> {
        let mut out = Vec::new();
        loop {
            match self.fill() {
                Ok(()) => {
                    out.extend_from_slice(&self.buf[self.pos..]);
                    self.pos = self.buf.len();
                }
                Err(NetErr::Eof) => return Ok(out),
                Err(e) => return Err(e),
            }
        }
    }
}
