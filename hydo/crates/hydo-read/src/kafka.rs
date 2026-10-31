//! A minimal Kafka client (Metadata v5, ListOffsets v2, Fetch v4 with v2 record batches), enough
//! to read a topic from the log start offset to the high watermark as the Go program did with
//! franz-go and kadm: the same records in (partition, offset) order, the same handling of null
//! and empty keys, the same timestamps, and franz-go's error texts
//! ("UNKNOWN_TOPIC_OR_PARTITION: This server does not host this topic-partition.").
//!
//! Compressed batches (gzip, snappy raw or xerial-framed, lz4 frames, zstd) are decoded with
//! pure-Rust decoders. Control records (transaction markers) are skipped, as franz-go skips them,
//! but they count as progress, so a transactional topic no longer waits for the timeout.
//!
//! For the store tests the client can also create and delete topics and produce records
//! (CreateTopics v2, DeleteTopics v1, Produce v3).

use crate::json::Val;
use crate::kerr::KERR;
use crate::net::{dial_tcp, Conn, NetErr};
use crate::{bytes_value, cannot, lossy, rec, ReadOpts, Record, Res};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::time::{Duration, Instant};

/// franz-go's kerr text for a code: "NAME: description".
pub fn kerr_text(code: i16) -> String {
    match KERR.iter().find(|e| e.0 == code) {
        Some(e) => format!("{}: {}", e.1, e.3),
        None => "UNKNOWN_SERVER_ERROR: The server experienced an unexpected error when processing the request.".into(),
    }
}

fn retriable(code: i16) -> bool {
    KERR.iter().find(|e| e.0 == code).is_some_and(|e| e.2)
}

// ------------------------------------------------------------------ wire encoding

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn i8(&mut self, v: i8) -> &mut Self {
        self.0.push(v as u8);
        self
    }
    fn i16(&mut self, v: i16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn i64(&mut self, v: i64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn str(&mut self, s: &[u8]) -> &mut Self {
        self.i16(s.len() as i16);
        self.0.extend_from_slice(s);
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.i32(b.len() as i32);
        self.0.extend_from_slice(b);
        self
    }
    fn varint(&mut self, v: i64) -> &mut Self {
        let mut u = ((v << 1) ^ (v >> 63)) as u64;
        loop {
            let b = (u & 0x7f) as u8;
            u >>= 7;
            if u == 0 {
                self.0.push(b);
                return self;
            }
            self.0.push(b | 0x80);
        }
    }
    fn vbytes(&mut self, b: Option<&[u8]>) -> &mut Self {
        match b {
            None => self.varint(-1),
            Some(b) => {
                self.varint(b.len() as i64);
                self.0.extend_from_slice(b);
                self
            }
        }
    }
}

/// A response reader; running off the end is a malformed response.
struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

type Rr<T> = Result<T, String>;

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Rr<&'a [u8]> {
        if self.i + n > self.b.len() {
            return Err("response too short".into());
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn i8(&mut self) -> Rr<i8> {
        Ok(self.take(1)?[0] as i8)
    }
    fn i16(&mut self) -> Rr<i16> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }
    fn i32(&mut self) -> Rr<i32> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i64(&mut self) -> Rr<i64> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes(b.try_into().unwrap_or([0; 8])))
    }
    fn str(&mut self) -> Rr<Vec<u8>> {
        let n = self.i16()?;
        if n < 0 {
            return Ok(Vec::new());
        }
        Ok(self.take(n as usize)?.to_vec())
    }
    fn nbytes(&mut self) -> Rr<Option<&'a [u8]>> {
        let n = self.i32()?;
        if n < 0 {
            return Ok(None);
        }
        Ok(Some(self.take(n as usize)?))
    }
    fn arr(&mut self) -> Rr<usize> {
        let n = self.i32()?;
        Ok(if n < 0 { 0 } else { n as usize })
    }
    fn varint(&mut self) -> Rr<i64> {
        let mut u: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.take(1)?[0];
            u |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 63 {
                return Err("bad varint".into());
            }
        }
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }
    fn vbytes(&mut self) -> Rr<Option<&'a [u8]>> {
        let n = self.varint()?;
        if n < 0 {
            return Ok(None);
        }
        Ok(Some(self.take(n as usize)?))
    }
}

// ------------------------------------------------------------------ connections

struct Broker {
    c: Conn,
    corr: i32,
}

/// Why a request failed: the connection (retry another time), or something final.
#[derive(Debug)]
enum KErr {
    Dial(String),
    Io(String),
}

impl KErr {
    fn text(&self) -> String {
        match self {
            KErr::Dial(s) => format!("unable to dial: {s}"),
            KErr::Io(s) => s.clone(),
        }
    }
}

impl Broker {
    fn dial(addr: &str, deadline: Instant) -> Result<Broker, KErr> {
        let c = dial_tcp(addr, deadline).map_err(|e| KErr::Dial(e.to_string()))?;
        Ok(Broker { c, corr: 0 })
    }

    fn request(&mut self, key: i16, ver: i16, body: &[u8], deadline: Instant) -> Result<Vec<u8>, KErr> {
        self.c.deadline = deadline;
        self.corr += 1;
        let mut h = W::default();
        h.i16(key).i16(ver).i32(self.corr).str(b"kgo");
        let mut p = W::default();
        p.i32((h.0.len() + body.len()) as i32);
        p.0.extend_from_slice(&h.0);
        p.0.extend_from_slice(body);
        let io = |e: NetErr| match e {
            NetErr::Timeout(_) => KErr::Io("context deadline exceeded".into()),
            NetErr::Eof => KErr::Io("EOF".into()),
            e => KErr::Io(e.to_string()),
        };
        self.c.write_all(&p.0).map_err(io)?;
        let l = self.c.read_exact(4).map_err(io)?;
        let n = i32::from_be_bytes([l[0], l[1], l[2], l[3]]);
        if n < 4 {
            return Err(KErr::Io(format!("invalid large response size {n}")));
        }
        let b = self.c.read_exact(n as usize).map_err(io)?;
        let corr = i32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        if corr != self.corr {
            return Err(KErr::Io(format!(
                "correlation ID mismatch: got {corr}, expected {}",
                self.corr
            )));
        }
        Ok(b[4..].to_vec())
    }
}

struct PartMeta {
    err: i16,
    leader: i32,
}

struct Meta {
    brokers: HashMap<i32, String>,
    topic_err: i16,
    parts: BTreeMap<i32, PartMeta>,
}

/// A connection pool keyed by address, with the seeds for metadata.
struct Client {
    seeds: Vec<String>,
    conns: HashMap<String, Broker>,
    deadline: Instant,
}

impl Client {
    /// One request to `addr`, redialling a broken connection until the deadline (kgo retries
    /// connection errors until its retry timeout).
    fn req(&mut self, addr: &str, key: i16, ver: i16, body: &[u8]) -> Result<Vec<u8>, KErr> {
        let mut last: Option<KErr> = None;
        let mut backoff = Duration::from_millis(100);
        loop {
            if Instant::now() >= self.deadline {
                return Err(last.unwrap_or(KErr::Io("context deadline exceeded".into())));
            }
            if !self.conns.contains_key(addr) {
                match Broker::dial(addr, self.deadline) {
                    Ok(b) => {
                        self.conns.insert(addr.to_string(), b);
                    }
                    Err(e) => {
                        last = Some(e);
                        sleep_until(self.deadline, backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(1));
                        continue;
                    }
                }
            }
            let b = self.conns.get_mut(addr).expect("connection");
            match b.request(key, ver, body, self.deadline) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    self.conns.remove(addr);
                    if Instant::now() >= self.deadline {
                        return Err(e);
                    }
                    last = Some(e);
                    sleep_until(self.deadline, backoff);
                }
            }
        }
    }

    /// Metadata for one topic from the first seed that answers.
    fn metadata(&mut self, topic: &[u8]) -> Result<Meta, KErr> {
        let mut w = W::default();
        w.i32(1).str(topic).i8(0);
        let mut last = None;
        let mut resp = None;
        for s in self.seeds.clone() {
            match self.req(&s, 3, 5, &w.0) {
                Ok(r) => {
                    resp = Some(r);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let Some(r) = resp else {
            return Err(last.unwrap_or(KErr::Io("no seed brokers".into())));
        };
        parse_metadata(&r, topic).map_err(KErr::Io)
    }
}

fn sleep_until(deadline: Instant, d: Duration) {
    let now = Instant::now();
    if now < deadline {
        std::thread::sleep(d.min(deadline - now));
    }
}

fn parse_metadata(r: &[u8], topic: &[u8]) -> Rr<Meta> {
    let mut rd = Rd { b: r, i: 0 };
    rd.i32()?; // throttle
    let mut brokers = HashMap::new();
    for _ in 0..rd.arr()? {
        let id = rd.i32()?;
        let host = lossy(&rd.str()?);
        let port = rd.i32()?;
        rd.str()?; // rack
        let addr = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        brokers.insert(id, addr);
    }
    rd.str()?; // cluster id
    rd.i32()?; // controller
    let mut topic_err = 3;
    let mut parts = BTreeMap::new();
    for _ in 0..rd.arr()? {
        let err = rd.i16()?;
        let name = rd.str()?;
        rd.i8()?; // internal
        let mut ps = BTreeMap::new();
        for _ in 0..rd.arr()? {
            let perr = rd.i16()?;
            let idx = rd.i32()?;
            let leader = rd.i32()?;
            for _ in 0..3 {
                for _ in 0..rd.arr()? {
                    rd.i32()?;
                }
            }
            ps.insert(idx, PartMeta { err: perr, leader });
        }
        if name == topic {
            topic_err = err;
            parts = ps;
        }
    }
    Ok(Meta {
        brokers,
        topic_err,
        parts,
    })
}

/// ListOffsets for every partition of the topic at `ts` (-1 latest, -2 earliest), kadm style:
/// per-partition results, retriable errors retried against fresh metadata until the deadline.
fn list_offsets(cl: &mut Client, topic: &[u8], ts: i64) -> Result<BTreeMap<i32, i64>, String> {
    let mut out: BTreeMap<i32, i64> = BTreeMap::new();
    loop {
        let meta = cl.metadata(topic).map_err(|e| e.text())?;
        if meta.topic_err != 0 {
            return Err(kerr_text(meta.topic_err));
        }
        let mut by_leader: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        let mut retry_err: Option<String> = None;
        for (p, pm) in &meta.parts {
            if out.contains_key(p) {
                continue;
            }
            if pm.leader < 0 || !meta.brokers.contains_key(&pm.leader) {
                retry_err = Some(kerr_text(if pm.err != 0 { pm.err } else { 5 }));
                continue;
            }
            by_leader.entry(pm.leader).or_default().push(*p);
        }
        for (leader, ps) in by_leader {
            let addr = meta.brokers[&leader].clone();
            let mut w = W::default();
            w.i32(-1).i8(0).i32(1).str(topic).i32(ps.len() as i32);
            for p in &ps {
                w.i32(*p).i64(ts);
            }
            let r = match cl.req(&addr, 2, 2, &w.0) {
                Ok(r) => r,
                Err(e) => {
                    retry_err = Some(e.text());
                    continue;
                }
            };
            let mut rd = Rd { b: &r, i: 0 };
            let parsed: Rr<()> = (|| {
                rd.i32()?;
                for _ in 0..rd.arr()? {
                    rd.str()?;
                    for _ in 0..rd.arr()? {
                        let p = rd.i32()?;
                        let err = rd.i16()?;
                        rd.i64()?;
                        let off = rd.i64()?;
                        if err == 0 {
                            out.insert(p, off);
                        } else if retriable(err) {
                            retry_err = Some(kerr_text(err));
                        } else {
                            return Err(kerr_text(err));
                        }
                    }
                }
                Ok(())
            })();
            parsed?;
        }
        if meta.parts.keys().all(|p| out.contains_key(p)) {
            return Ok(out);
        }
        if Instant::now() >= cl.deadline {
            return Err(retry_err.unwrap_or_else(|| "context deadline exceeded".into()));
        }
        sleep_until(cl.deadline, Duration::from_millis(250));
    }
}

// ------------------------------------------------------------------ record batches

const CRC32C_POLY: u32 = 0x82f63b78;

pub fn crc32c(b: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &x in b {
        crc ^= x as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ CRC32C_POLY } else { crc >> 1 };
        }
    }
    !crc
}

fn decompress(codec: i16, src: &[u8]) -> Rr<Vec<u8>> {
    match codec {
        0 => Ok(src.to_vec()),
        1 => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(src)
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            Ok(out)
        }
        2 => {
            const XERIAL: &[u8] = &[130, b'S', b'N', b'A', b'P', b'P', b'Y', 0];
            if src.len() > 16 && src.starts_with(XERIAL) {
                let mut out = Vec::new();
                let mut s = &src[16..];
                while !s.is_empty() {
                    if s.len() < 4 {
                        return Err("malformed xerial framing".into());
                    }
                    let n = u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize;
                    s = &s[4..];
                    if s.len() < n {
                        return Err("malformed xerial framing".into());
                    }
                    out.extend(snap::raw::Decoder::new().decompress_vec(&s[..n]).map_err(|e| e.to_string())?);
                    s = &s[n..];
                }
                return Ok(out);
            }
            snap::raw::Decoder::new()
                .decompress_vec(src)
                .map_err(|e| e.to_string())
        }
        3 => {
            let mut out = Vec::new();
            lz4_flex::frame::FrameDecoder::new(src)
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            Ok(out)
        }
        4 => {
            let mut out = Vec::new();
            let mut d = ruzstd::StreamingDecoder::new(src).map_err(|e| e.to_string())?;
            d.read_to_end(&mut out).map_err(|e| e.to_string())?;
            Ok(out)
        }
        _ => Err("unknown compression codec".into()),
    }
}

/// One fetched record (or a control record, which only moves the position).
struct FRec {
    offset: i64,
    control: bool,
    key: Option<Vec<u8>>,
    value: Option<Vec<u8>>,
    ts: i64,
}

/// The records of a partition's record set; `next` = the offset after the last complete batch.
fn parse_records(set: &[u8], next: &mut i64) -> Rr<Vec<FRec>> {
    let mut out = Vec::new();
    let mut rd = Rd { b: set, i: 0 };
    while set.len() - rd.i >= 12 {
        let start = rd.i;
        let base = rd.i64()?;
        let len = rd.i32()?;
        if len < 0 || rd.i + len as usize > set.len() {
            break; // a truncated batch at the end
        }
        let end = rd.i + len as usize;
        rd.i32()?; // leader epoch
        let magic = rd.i8()?;
        if magic != 2 {
            return Err(format!("unknown batch magic {magic}"));
        }
        let crc = rd.i32()? as u32;
        let calc = crc32c(&set[rd.i..end]);
        if calc != crc {
            return Err(format!(
                "encoded crc {:x} does not match calculated crc {:x}",
                crc as i32, calc as i32
            ));
        }
        let attrs = rd.i16()?;
        let last_delta = rd.i32()?;
        let first_ts = rd.i64()?;
        let max_ts = rd.i64()?;
        rd.take(14)?; // producer id, epoch, base sequence
        let count = rd.i32()?;
        let raw = &set[rd.i..end];
        let body = decompress(attrs & 7, raw)?;
        let control = attrs & 0x20 != 0;
        let log_append = attrs & 0x08 != 0;
        let mut br = Rd { b: &body, i: 0 };
        for _ in 0..count.max(0) {
            let Ok(l) = br.varint() else { break };
            if l < 0 || br.i + l as usize > body.len() {
                break;
            }
            let rend = br.i + l as usize;
            let mut r = Rd {
                b: &body[..rend],
                i: br.i,
            };
            let rec: Rr<FRec> = (|| {
                r.i8()?;
                let ts_delta = r.varint()?;
                let off_delta = r.varint()?;
                let key = r.vbytes()?.map(<[u8]>::to_vec);
                let value = r.vbytes()?.map(<[u8]>::to_vec);
                Ok(FRec {
                    offset: base + off_delta,
                    control,
                    key,
                    value,
                    ts: if log_append { max_ts } else { first_ts + ts_delta },
                })
            })();
            br.i = rend;
            match rec {
                Ok(x) => out.push(x),
                Err(_) => break,
            }
        }
        *next = (*next).max(base + last_delta as i64 + 1);
        rd.i = end;
        let _ = start;
    }
    Ok(out)
}

pub fn read(bootstrap: &[u8], topic: &[u8], o: &ReadOpts) -> Res<Vec<Record>> {
    let t = lossy(topic);
    let deadline = Instant::now() + o.timeout;
    let mut cl = Client {
        seeds: lossy(bootstrap).split(',').map(str::to_string).collect(),
        conns: HashMap::new(),
        deadline,
    };
    let ends = list_offsets(&mut cl, topic, -1)
        .map_err(|e| cannot(format!("kafka {t}: end offsets: {e}")))?;
    let starts = list_offsets(&mut cl, topic, -2)
        .map_err(|e| cannot(format!("kafka {t}: start offsets: {e}")))?;
    if ends.is_empty() {
        return Err(cannot(format!("kafka: topic {t} does not exist")));
    }
    let mut next: BTreeMap<i32, i64> = BTreeMap::new();
    let mut by_part: BTreeMap<i32, Vec<Record>> = BTreeMap::new();
    for (p, e) in &ends {
        let s = starts.get(p).copied().unwrap_or(0);
        if s < *e {
            next.insert(*p, s);
        }
    }
    let count = |m: &BTreeMap<i32, Vec<Record>>| m.values().map(Vec::len).sum::<usize>();
    let mut meta: Option<Meta> = None;
    while !next.is_empty() {
        if Instant::now() >= deadline {
            return Err(cannot(format!(
                "kafka {t}: timed out after reading {} records (context deadline exceeded)",
                count(&by_part)
            )));
        }
        if meta.is_none() {
            meta = cl.metadata(topic).ok();
        }
        let Some(m) = meta.as_ref() else {
            continue;
        };
        let brokers = m.brokers.clone();
        let mut by_leader: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
        for p in next.keys() {
            if let Some(pm) = m.parts.get(p) {
                if pm.leader >= 0 && m.brokers.contains_key(&pm.leader) {
                    by_leader.entry(pm.leader).or_default().push(*p);
                }
            }
        }
        if by_leader.is_empty() {
            meta = None;
            sleep_until(deadline, Duration::from_millis(250));
            continue;
        }
        for (leader, ps) in by_leader {
            let addr = brokers[&leader].clone();
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(200));
            let mut w = W::default();
            w.i32(-1)
                .i32(wait.as_millis() as i32)
                .i32(1)
                .i32(50 << 20)
                .i8(0)
                .i32(1)
                .str(topic)
                .i32(ps.len() as i32);
            for p in &ps {
                w.i32(*p).i64(next[p]).i32(1 << 20);
            }
            let r = match cl.req(&addr, 1, 4, &w.0) {
                Ok(r) => r,
                Err(_) => {
                    meta = None;
                    continue;
                }
            };
            let mut rd = Rd { b: &r, i: 0 };
            let res: Rr<Vec<(i32, i16, Vec<u8>)>> = (|| {
                let mut v = Vec::new();
                rd.i32()?;
                for _ in 0..rd.arr()? {
                    rd.str()?;
                    for _ in 0..rd.arr()? {
                        let p = rd.i32()?;
                        let err = rd.i16()?;
                        rd.i64()?; // high watermark
                        rd.i64()?; // last stable offset
                        let na = rd.i32()?;
                        for _ in 0..na.max(0) {
                            rd.i64()?;
                            rd.i64()?;
                        }
                        let recs = rd.nbytes()?.unwrap_or(&[]).to_vec();
                        v.push((p, err, recs));
                    }
                }
                Ok(v)
            })();
            let parts = res.map_err(|e| cannot(format!("kafka {t}: {e}")))?;
            for (p, err, set) in parts {
                let Some(&pos) = next.get(&p) else { continue };
                match err {
                    0 => {}
                    1 => {
                        // OFFSET_OUT_OF_RANGE: franz-go resets to the log start
                        if let Ok(s) = list_offsets(&mut cl, topic, -2) {
                            if let Some(&s) = s.get(&p) {
                                next.insert(p, s.max(pos));
                            }
                        }
                        continue;
                    }
                    e if retriable(e) => {
                        meta = None;
                        continue;
                    }
                    e => {
                        return Err(cannot(format!(
                            "kafka {t} partition {p}: {}",
                            kerr_text(e)
                        )))
                    }
                }
                let end = ends[&p];
                let mut np = pos;
                let recs = parse_records(&set, &mut np)
                    .map_err(|e| cannot(format!("kafka {t} partition {p}: {e}")))?;
                let mut cur = pos;
                for r in recs {
                    if r.offset < cur || r.offset >= end {
                        continue;
                    }
                    cur = r.offset + 1;
                    if r.control {
                        continue;
                    }
                    by_part.entry(p).or_default().push(rec(vec![
                        ("partition", Val::Int(p as i64)),
                        ("offset", Val::Int(r.offset)),
                        ("key", bytes_value(r.key.as_deref(), o.base64)),
                        ("value", bytes_value(r.value.as_deref(), o.base64)),
                        ("timestamp_ms", Val::Int(r.ts)),
                    ]));
                }
                let np = np.max(cur);
                if np >= end {
                    next.remove(&p);
                } else {
                    next.insert(p, np);
                }
            }
        }
    }
    let mut out = Vec::new();
    for p in ends.keys() {
        if let Some(v) = by_part.remove(p) {
            out.extend(v);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ test helpers

fn admin(bootstrap: &str) -> Client {
    Client {
        seeds: bootstrap.split(',').map(str::to_string).collect(),
        conns: HashMap::new(),
        deadline: Instant::now() + Duration::from_secs(60),
    }
}

/// CreateTopics v2 with the given partition count (replication factor 1); the error code.
pub fn create_topic(bootstrap: &str, topic: &str, partitions: i32) -> Result<i16, String> {
    let mut cl = admin(bootstrap);
    let mut w = W::default();
    w.i32(1).str(topic.as_bytes()).i32(partitions).i16(1).i32(0).i32(0).i32(30000).i8(0);
    let seed = cl.seeds[0].clone();
    let r = cl.req(&seed, 19, 2, &w.0).map_err(|e| e.text())?;
    let mut rd = Rd { b: &r, i: 0 };
    rd.i32()?;
    rd.arr()?;
    rd.str()?;
    rd.i16()
}

/// DeleteTopics v1; the error code.
pub fn delete_topic(bootstrap: &str, topic: &str) -> Result<i16, String> {
    let mut cl = admin(bootstrap);
    let mut w = W::default();
    w.i32(1).str(topic.as_bytes()).i32(30000);
    let seed = cl.seeds[0].clone();
    let r = cl.req(&seed, 20, 1, &w.0).map_err(|e| e.text())?;
    let mut rd = Rd { b: &r, i: 0 };
    rd.i32()?;
    rd.arr()?;
    rd.str()?;
    rd.i16()
}

/// Topic names in the cluster (Metadata v5 for all topics).
pub fn list_topics(bootstrap: &str) -> Result<Vec<String>, String> {
    let mut cl = admin(bootstrap);
    let mut w = W::default();
    w.i32(-1).i8(0);
    let seed = cl.seeds[0].clone();
    let r = cl.req(&seed, 3, 5, &w.0).map_err(|e| e.text())?;
    let mut rd = Rd { b: &r, i: 0 };
    rd.i32()?;
    for _ in 0..rd.arr()? {
        rd.i32()?;
        rd.str()?;
        rd.i32()?;
        rd.str()?;
    }
    rd.str()?;
    rd.i32()?;
    let mut names = Vec::new();
    for _ in 0..rd.arr()? {
        rd.i16()?;
        names.push(lossy(&rd.str()?));
        rd.i8()?;
        for _ in 0..rd.arr()? {
            rd.i16()?;
            rd.i32()?;
            rd.i32()?;
            for _ in 0..3 {
                for _ in 0..rd.arr()? {
                    rd.i32()?;
                }
            }
        }
    }
    Ok(names)
}

/// One record to produce: partition, key, value, timestamp (ms).
pub struct ProduceRec<'a> {
    pub partition: i32,
    pub key: Option<&'a [u8]>,
    pub value: Option<&'a [u8]>,
    pub ts: i64,
}

/// Produce v3 (acks=all), one uncompressed batch per record, in order.
pub fn produce(bootstrap: &str, topic: &str, recs: &[ProduceRec]) -> Result<(), String> {
    let mut cl = admin(bootstrap);
    let meta = cl.metadata(topic.as_bytes()).map_err(|e| e.text())?;
    for r in recs {
        let leader = meta.parts.get(&r.partition).ok_or("no such partition")?.leader;
        let addr = meta.brokers.get(&leader).ok_or("no leader")?.clone();
        let mut rec = W::default();
        rec.i8(0).varint(0).varint(0).vbytes(r.key).vbytes(r.value).varint(0);
        let mut recs_b = W::default();
        recs_b.varint(rec.0.len() as i64);
        recs_b.0.extend_from_slice(&rec.0);
        let mut after_crc = W::default();
        after_crc.i16(0).i32(0).i64(r.ts).i64(r.ts).i64(-1).i16(-1).i32(-1).i32(1);
        after_crc.0.extend_from_slice(&recs_b.0);
        let crc = crc32c(&after_crc.0);
        let mut batch = W::default();
        batch.i64(0).i32((4 + 1 + 4 + after_crc.0.len()) as i32).i32(0).i8(2).i32(crc as i32);
        batch.0.extend_from_slice(&after_crc.0);
        let mut w = W::default();
        w.i16(-1).i16(-1).i32(30000).i32(1).str(topic.as_bytes()).i32(1).i32(r.partition).bytes(&batch.0);
        let resp = cl.req(&addr, 0, 3, &w.0).map_err(|e| e.text())?;
        let mut rd = Rd { b: &resp, i: 0 };
        rd.arr()?;
        rd.str()?;
        rd.arr()?;
        rd.i32()?;
        let err = rd.i16()?;
        if err != 0 {
            return Err(kerr_text(err));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_kerr() {
        assert_eq!(crc32c(b"123456789"), 0xe3069283);
        assert_eq!(
            kerr_text(3),
            "UNKNOWN_TOPIC_OR_PARTITION: This server does not host this topic-partition."
        );
        assert!(retriable(6));
    }

    #[test]
    fn varints() {
        for v in [0i64, 1, -1, 63, -64, 300, -300, i32::MAX as i64] {
            let mut w = W::default();
            w.varint(v);
            let mut r = Rd { b: &w.0, i: 0 };
            assert_eq!(r.varint().unwrap(), v);
        }
    }
}
