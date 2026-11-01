//! The packet trigger's parts (a port of hydo/onboard/hydo_fc.c): a netfilter queue over raw
//! netlink (no libnetfilter_queue), the IPv4/TCP payload of a queued packet, and the counter that
//! decides at which packet to fire. `fc.rs` (the hydo_fc command line) and the agent's `freeze`
//! command both use them.

use std::io;
use std::os::fd::RawFd;

use crate::sys;

const NFNL_SUBSYS_QUEUE: u16 = 3;
const NFQNL_MSG_PACKET: u16 = 0;
const NFQNL_MSG_VERDICT: u16 = 1;
const NFQNL_MSG_CONFIG: u16 = 2;
const NFQNL_CFG_CMD_BIND: u8 = 1;
const NFQA_CFG_CMD: u16 = 1;
const NFQA_CFG_PARAMS: u16 = 2;
const NFQA_CFG_QUEUE_MAXLEN: u16 = 3;
const NFQNL_COPY_PACKET: u8 = 2;
const NFQA_PACKET_HDR: u16 = 1;
const NFQA_VERDICT_HDR: u16 = 2;
const NFQA_PAYLOAD: u16 = 10;
const NF_ACCEPT: u32 = 1;
const NLMSG_HDRLEN: usize = 16;
const NLMSG_ERROR: u16 = 2;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLA_TYPE_MASK: u16 = !((1 << 15) | (1 << 14));

fn align(n: usize) -> usize {
    (n + 3) & !3
}

/// A netlink message being built.
struct Msg(Vec<u8>);

impl Msg {
    fn new(queue: u16, ty: u16, flags: u16) -> Msg {
        let mut b = vec![0u8; NLMSG_HDRLEN + 4];
        let len = (NLMSG_HDRLEN + 4) as u32;
        b[0..4].copy_from_slice(&len.to_ne_bytes());
        b[4..6].copy_from_slice(&((NFNL_SUBSYS_QUEUE << 8) | ty).to_ne_bytes());
        b[6..8].copy_from_slice(&(NLM_F_REQUEST | flags).to_ne_bytes());
        // nfgenmsg: family AF_UNSPEC, version 0, res_id = queue (big-endian)
        b[NLMSG_HDRLEN + 2..NLMSG_HDRLEN + 4].copy_from_slice(&queue.to_be_bytes());
        Msg(b)
    }

    fn attr(mut self, ty: u16, data: &[u8]) -> Msg {
        let start = align(self.0.len());
        self.0.resize(start, 0);
        let alen = 4 + data.len();
        self.0.extend_from_slice(&(alen as u16).to_ne_bytes());
        self.0.extend_from_slice(&ty.to_ne_bytes());
        self.0.extend_from_slice(data);
        self.0.resize(start + align(alen), 0);
        let len = self.0.len() as u32;
        self.0[0..4].copy_from_slice(&len.to_ne_bytes());
        self
    }
}

/// A bound netfilter queue.
pub struct Nfq {
    pub fd: RawFd,
    queue: u16,
    buf: Vec<u8>,
}

fn kernel() -> libc::sockaddr_nl {
    let mut k: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    k.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    k
}

impl Nfq {
    fn send(&self, m: &Msg) -> io::Result<()> {
        let k = kernel();
        let r = unsafe {
            libc::sendto(
                self.fd,
                m.0.as_ptr().cast(),
                m.0.len(),
                0,
                (&k as *const libc::sockaddr_nl).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn send_ack(&self, m: &Msg) -> io::Result<()> {
        self.send(m)?;
        let mut r = [0u8; 1024];
        let n = unsafe { libc::recv(self.fd, r.as_mut_ptr().cast(), r.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize >= NLMSG_HDRLEN + 4 {
            let ty = u16::from_ne_bytes([r[4], r[5]]);
            let err = i32::from_ne_bytes([r[16], r[17], r[18], r[19]]);
            if ty == NLMSG_ERROR && err != 0 {
                return Err(io::Error::from_raw_os_error(-err));
            }
        }
        Ok(())
    }

    /// Opens a netlink socket and binds `queue` (copy whole packets, at most 8,192 queued).
    /// Failure names the step: ("netlink" | "bind queue" | "queue params", the error).
    pub fn bind(queue: u16) -> Result<Nfq, (&'static str, io::Error)> {
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_NETFILTER,
            )
        };
        if fd < 0 {
            return Err(("netlink", io::Error::last_os_error()));
        }
        let me = kernel();
        let r = unsafe {
            libc::bind(
                fd,
                (&me as *const libc::sockaddr_nl).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if r < 0 {
            let e = io::Error::last_os_error();
            sys::close(fd);
            return Err(("netlink", e));
        }
        let rb: libc::c_int = 8 << 20;
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUFFORCE,
                (&rb as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        let q = Nfq {
            fd,
            queue,
            buf: vec![0u8; 1 << 18],
        };
        // nfqnl_msg_config_cmd: command, pad, pf (big-endian)
        let mut cmd = vec![NFQNL_CFG_CMD_BIND, 0];
        cmd.extend_from_slice(&(libc::AF_INET as u16).to_be_bytes());
        if let Err(e) =
            q.send_ack(&Msg::new(queue, NFQNL_MSG_CONFIG, NLM_F_ACK).attr(NFQA_CFG_CMD, &cmd))
        {
            sys::close(fd);
            return Err(("bind queue", e));
        }
        let mut par = 0xffffu32.to_be_bytes().to_vec();
        par.push(NFQNL_COPY_PACKET);
        let m = Msg::new(queue, NFQNL_MSG_CONFIG, NLM_F_ACK)
            .attr(NFQA_CFG_PARAMS, &par)
            .attr(NFQA_CFG_QUEUE_MAXLEN, &8192u32.to_be_bytes());
        if let Err(e) = q.send_ack(&m) {
            sys::close(fd);
            return Err(("queue params", e));
        }
        Ok(q)
    }

    /// Lets packet `id` (as the kernel sent it, big-endian) through.
    pub fn accept(&self, id: u32) -> io::Result<()> {
        let mut v = NF_ACCEPT.to_be_bytes().to_vec();
        v.extend_from_slice(&id.to_ne_bytes());
        self.send(&Msg::new(self.queue, NFQNL_MSG_VERDICT, 0).attr(NFQA_VERDICT_HDR, &v))
    }

    /// Reads one batch of queued packets. `Err` with ENOBUFS: the queue overflowed.
    pub fn recv(&mut self) -> io::Result<Vec<Queued>> {
        let n = unsafe { libc::recv(self.fd, self.buf.as_mut_ptr().cast(), self.buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(parse_batch(&self.buf[..n as usize]))
    }
}

/// The queued packets in one netlink batch (NLMSG_OK / NLMSG_NEXT: a message's length is padded
/// to 4 bytes, but the last message of a batch need not be, so the next offset can pass the end).
pub fn parse_batch(b: &[u8]) -> Vec<Queued> {
    let mut out = Vec::new();
    let mut off = 0;
    while off + NLMSG_HDRLEN <= b.len() {
        let len = u32::from_ne_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as usize;
        if len < NLMSG_HDRLEN || len > b.len() - off {
            break;
        }
        let ty = u16::from_ne_bytes([b[off + 4], b[off + 5]]);
        if ty & 0xff == NFQNL_MSG_PACKET && ty >> 8 == NFNL_SUBSYS_QUEUE && len >= NLMSG_HDRLEN + 4
        {
            // the attributes, within this message (alen counts what is left of it)
            let mut id = None;
            let mut pkt: &[u8] = &[];
            let mut a = off + NLMSG_HDRLEN + align(4);
            let mut alen = len as isize - (NLMSG_HDRLEN + 4) as isize;
            while alen >= 4 {
                let al = u16::from_ne_bytes([b[a], b[a + 1]]) as usize;
                if al < 4 || al as isize > alen {
                    break;
                }
                let at = u16::from_ne_bytes([b[a + 2], b[a + 3]]) & NLA_TYPE_MASK;
                if at == NFQA_PACKET_HDR && al >= 8 {
                    id = Some(u32::from_ne_bytes([b[a + 4], b[a + 5], b[a + 6], b[a + 7]]));
                }
                if at == NFQA_PAYLOAD {
                    pkt = &b[a + 4..a + al];
                }
                alen -= align(al) as isize;
                a += align(al);
            }
            if let Some(id) = id {
                out.push(Queued {
                    id,
                    pkt: pkt.to_vec(),
                });
            }
        }
        off += align(len);
    }
    out
}

impl Drop for Nfq {
    fn drop(&mut self) {
        sys::close(self.fd);
    }
}

/// One queued packet: its id and its bytes (IP header first).
#[derive(Debug, PartialEq, Eq)]
pub struct Queued {
    pub id: u32,
    pub pkt: Vec<u8>,
}

/// The TCP payload of an IPv4 packet, with its ports and source address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tcp<'a> {
    pub saddr: [u8; 4],
    pub sport: u16,
    pub dport: u16,
    pub payload: &'a [u8],
}

/// hydo_fc's parse: IPv4 and TCP, the payload as far as the total length and the captured bytes
/// reach. `None` when there is no payload.
pub fn tcp(pkt: &[u8]) -> Option<Tcp<'_>> {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 || pkt[9] != 6 {
        return None;
    }
    let ihl = (pkt[0] & 15) as usize * 4;
    let mut tot = ((pkt[2] as usize) << 8) | pkt[3] as usize;
    let saddr = [pkt[12], pkt[13], pkt[14], pkt[15]];
    if pkt.len() < ihl + 20 {
        return None;
    }
    let doff = (pkt[ihl + 12] >> 4) as usize * 4;
    let sport = ((pkt[ihl] as u16) << 8) | pkt[ihl + 1] as u16;
    let dport = ((pkt[ihl + 2] as u16) << 8) | pkt[ihl + 3] as u16;
    tot = tot.min(pkt.len());
    if tot > ihl + doff {
        Some(Tcp {
            saddr,
            sport,
            dport,
            payload: &pkt[ihl + doff..tot],
        })
    } else {
        None
    }
}

/// hydo_fc's printable form of payload bytes: printable ASCII except space kept, others '.'.
pub fn snippet(p: &[u8], max: usize) -> String {
    p.iter()
        .take(max)
        .map(|&c| {
            if (0x21..0x7f).contains(&c) {
                c as char
            } else {
                '.'
            }
        })
        .collect()
}

/// hydo_fc's match argument: bytes, %xx escapes any byte.
pub fn unescape_match(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            out.push(strtol16(h));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// strtol(h, NULL, 16) of two characters, truncated to a byte as hydo_fc's cast does.
fn strtol16(h: &str) -> u8 {
    let mut n: u32 = 0;
    for c in h.chars() {
        match c.to_digit(16) {
            Some(d) => n = n * 16 + d,
            None => break,
        }
    }
    n as u8
}

/// How packets are told apart and counted.
pub struct Counter {
    /// A request is a packet from this source address ...
    pub sip: Option<[u8; 4]>,
    /// ... or else (`own`) one from this source port, or else one to this destination port.
    pub own: bool,
    pub tport: Option<u16>,
    pub want_req: bool,
    pub pattern: Vec<u8>,
    /// Fire at this counted packet (0: never).
    pub target: u64,
    pub nreq: u64,
    pub nresp: u64,
    pub counted: u64,
}

/// What a payload packet is.
pub struct Seen {
    pub is_req: bool,
    /// Its number in its direction.
    pub k: u64,
    /// Whether it was counted.
    pub hit: bool,
    /// Whether it is the packet to fire at.
    pub fire: bool,
}

impl Counter {
    pub fn see(&mut self, t: &Tcp<'_>) -> Seen {
        let is_req = match (self.sip, self.tport) {
            (Some(ip), _) => t.saddr == ip,
            (None, Some(p)) if self.own => t.sport == p,
            (None, Some(p)) => t.dport == p,
            // hydo_fc: tport = -1 never equals a port
            (None, None) => false,
        };
        let k = if is_req {
            self.nreq += 1;
            self.nreq
        } else {
            self.nresp += 1;
            self.nresp
        };
        let hit = is_req == self.want_req
            && (self.pattern.is_empty() || contains(t.payload, &self.pattern));
        if hit {
            self.counted += 1;
        }
        let fire = hit && self.target > 0 && self.counted == self.target;
        Seen {
            is_req,
            k,
            hit,
            fire,
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// hydo_fc's trace line of one payload packet.
pub fn log_line(s: &Seen, counted: u64, t: &Tcp<'_>, now: u64) -> String {
    let c = if s.hit {
        format!(" #{counted}")
    } else {
        String::new()
    };
    format!(
        "{}{} {}>{} len={}{} t={} {}\n",
        if s.is_req { "req" } else { "resp" },
        s.k,
        t.sport,
        t.dport,
        t.payload.len(),
        c,
        now,
        snippet(t.payload, 160)
    )
}

/// hydo_fc's address argument (inet_pton; a bad one is 0.0.0.0).
pub fn parse_ipv4(s: &str) -> [u8; 4] {
    let parts: Vec<_> = s.split('.').collect();
    if parts.len() == 4 {
        let mut a = [0u8; 4];
        for (i, p) in parts.iter().enumerate() {
            match p.parse::<u8>() {
                Ok(v) if !p.is_empty() && p.len() <= 3 => a[i] = v,
                _ => return [0; 4],
            }
        }
        return a;
    }
    [0; 4]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(saddr: [u8; 4], sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 40];
        p[0] = 0x45;
        let tot = (40 + payload.len()) as u16;
        p[2..4].copy_from_slice(&tot.to_be_bytes());
        p[9] = 6;
        p[12..16].copy_from_slice(&saddr);
        p[20..22].copy_from_slice(&sport.to_be_bytes());
        p[22..24].copy_from_slice(&dport.to_be_bytes());
        p[32] = 5 << 4;
        p.extend_from_slice(payload);
        p
    }

    /// A NFQNL_MSG_PACKET message as the kernel sends it: header, nfgenmsg, the packet header
    /// attribute (id), the payload attribute; not padded at its end.
    fn queued_msg(id: u32, payload: &[u8]) -> Vec<u8> {
        let mut m = Msg::new(7, NFQNL_MSG_PACKET, 0);
        let mut hdr = id.to_be_bytes().to_vec();
        hdr.extend_from_slice(&[0x08, 0x00, 3]);
        m = m.attr(NFQA_PACKET_HDR, &hdr).attr(NFQA_PAYLOAD, payload);
        let mut b = m.0;
        let unpadded = b.len() - (align(4 + payload.len()) - (4 + payload.len()));
        b.truncate(unpadded);
        let len = b.len() as u32;
        b[0..4].copy_from_slice(&len.to_ne_bytes());
        b
    }

    #[test]
    fn a_batch_whose_last_message_is_not_padded() {
        // found in a guest (M117): the next offset after the last message passed the batch's end
        let mut batch = queued_msg(1, &[0x45; 41]);
        batch.resize(align(batch.len()), 0);
        batch.extend(queued_msg(2, &[0x45; 30]));
        assert_eq!(batch.len() % 4, 2);
        let got = parse_batch(&batch);
        assert_eq!(
            got.iter()
                .map(|q| (u32::from_be(q.id), q.pkt.len()))
                .collect::<Vec<_>>(),
            vec![(1, 41), (2, 30)]
        );
        for cut in 0..batch.len() {
            parse_batch(&batch[..cut]);
        }
    }

    #[test]
    fn parse_count_and_fire() {
        let a = packet([10, 0, 0, 1], 5000, 6379, b"*1\r\n$4\r\nPING\r\n");
        let t = tcp(&a).unwrap();
        assert_eq!((t.sport, t.dport, t.payload.len()), (5000, 6379, 14));
        assert_eq!(snippet(t.payload, 60), "*1..$4..PING..");
        assert!(tcp(&packet([10, 0, 0, 1], 1, 2, b"")).is_none());
        let mut c = Counter {
            sip: None,
            own: false,
            tport: Some(6379),
            want_req: true,
            pattern: unescape_match("PI%4eG"),
            target: 2,
            nreq: 0,
            nresp: 0,
            counted: 0,
        };
        assert_eq!(c.pattern, b"PING");
        let s1 = c.see(&t);
        assert!(s1.hit && !s1.fire && s1.k == 1);
        let b = packet([10, 0, 0, 2], 6379, 5000, b"+PONG\r\n");
        let s2 = c.see(&tcp(&b).unwrap());
        assert!(!s2.is_req && !s2.hit);
        let s3 = c.see(&t);
        assert!(s3.fire && s3.k == 2);
        assert_eq!(
            log_line(&s3, c.counted, &t, 77),
            "req2 5000>6379 len=14 #2 t=77 *1..$4..PING..\n"
        );
        assert_eq!(parse_ipv4("10.0.0.7"), [10, 0, 0, 7]);
        assert_eq!(parse_ipv4("x"), [0; 4]);
    }
}
