//! pkv: a small key/value server over TCP on loopback, for hydo's performance tests
//! (`perf:` in hydo.yaml, `hydo guide workflow`). One binary, four roles:
//!
//!   pkv serve <dir> <port>             keeps the values in memory and appends each PUT to <dir>/log
//!   pkv ping  <port>                   exit 0 when the server answers (readiness)
//!   pkv bench <port> <step> <n>        n PUTs and n GETs over one connection, then COMMIT (one
//!                                      fdatasync); with PKV_LATENCIES=1, appends the PUTs'
//!                                      p50 and p99 in guest time to $HYDO_PERF
//!   pkv check <port> <acked-file>      every key of every acknowledged step reads back
//!
//! Each PUT costs the server a fixed amount of CPU work: it mixes the value BASE_ROUNDS times,
//! as an index or a checksum would. A build with `PKV_EXTRA_PERMILLE=<n>` in the environment does
//! n thousandths more of that work per PUT: a planted slowdown, as a commit that makes the request
//! handler slower would. `PKV_SYNC_EACH_PUT=1` plants a slowdown of another kind: an fdatasync
//! after every PUT, as a commit that waits for the disk more often would. `PKV_TAG=<text>` changes
//! the binary's bytes without changing its work: a commit that changes nothing that matters to
//! performance.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The rounds of mixing per PUT (the request handler's own CPU work).
const BASE_ROUNDS: u64 = 100_000;

/// The planted slowdown, in thousandths of BASE_ROUNDS (0 in a normal build).
fn extra_permille() -> u64 {
    option_env!("PKV_EXTRA_PERMILLE")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The planted disk slowdown: an fdatasync after every PUT (`PKV_SYNC_EACH_PUT=1`).
fn sync_each_put() -> bool {
    option_env!("PKV_SYNC_EACH_PUT") == Some("1")
}

/// The build's tag (`PKV_TAG`), printed when the server starts.
fn tag() -> &'static str {
    option_env!("PKV_TAG").unwrap_or("plain")
}

/// The handler's work: `rounds` rounds of xorshift with a branch that depends on the data.
fn mix(seed: u64, rounds: u64) -> u64 {
    let mut x = seed | 1;
    let mut acc = 0u64;
    for _ in 0..rounds {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if std::hint::black_box(x) & 1 == 1 {
            acc = acc.wrapping_add(x);
        } else {
            acc ^= x.rotate_left(5);
        }
    }
    acc
}

fn seed_of(s: &str) -> u64 {
    s.bytes()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

struct Store {
    map: BTreeMap<String, (String, u64)>,
    log: File,
}

fn serve(dir: &str, port: &str) {
    let rounds = BASE_ROUNDS + BASE_ROUNDS * extra_permille() / 1000;
    let sync = sync_each_put();
    eprintln!("pkv serve: build {}, {rounds} rounds per PUT, fdatasync per PUT {sync}", tag());
    let path = std::path::Path::new(dir).join("log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("open log");
    let st = Arc::new(Mutex::new(Store {
        map: BTreeMap::new(),
        log,
    }));
    let l = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    for c in l.incoming() {
        let Ok(c) = c else { continue };
        let st = st.clone();
        std::thread::spawn(move || conn(c, st, rounds, sync));
    }
}

fn conn(c: TcpStream, st: Arc<Mutex<Store>>, rounds: u64, sync: bool) {
    let mut w = c.try_clone().expect("clone");
    for line in BufReader::new(c).lines() {
        let Ok(line) = line else { return };
        let mut it = line.splitn(3, ' ');
        let reply = match (it.next(), it.next(), it.next()) {
            (Some("PUT"), Some(k), Some(v)) => {
                let sum = mix(seed_of(v), rounds);
                let mut s = st.lock().unwrap();
                if s.log.write_all(format!("PUT {k} {v} {sum:x}\n").as_bytes()).is_err()
                    || (sync && s.log.sync_data().is_err())
                {
                    "ERR write".to_string()
                } else {
                    s.map.insert(k.to_string(), (v.to_string(), sum));
                    "OK".to_string()
                }
            }
            (Some("GET"), Some(k), None) => {
                let s = st.lock().unwrap();
                s.map
                    .get(k)
                    .map_or("NONE".to_string(), |(v, _)| format!("VAL {v}"))
            }
            (Some("COMMIT"), None, None) => {
                let s = st.lock().unwrap();
                match s.log.sync_data() {
                    Ok(()) => "OK".to_string(),
                    Err(e) => format!("ERR sync {e}"),
                }
            }
            (Some("PING"), None, None) => "PONG".to_string(),
            _ => "ERR syntax".to_string(),
        };
        if w.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return;
        }
    }
}

struct Client {
    w: TcpStream,
    r: BufReader<TcpStream>,
}

impl Client {
    fn connect(port: &str) -> Result<Client, String> {
        let c = TcpStream::connect(format!("127.0.0.1:{port}")).map_err(|e| format!("connect: {e}"))?;
        c.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let r = BufReader::new(c.try_clone().map_err(|e| e.to_string())?);
        Ok(Client { w: c, r })
    }

    fn ask(&mut self, line: &str) -> Result<String, String> {
        self.w
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|e| format!("send: {e}"))?;
        let mut a = String::new();
        self.r.read_line(&mut a).map_err(|e| format!("recv: {e}"))?;
        if a.is_empty() {
            return Err("closed without a reply".into());
        }
        Ok(a.trim_end().to_string())
    }
}

/// The p-th percentile (0..=100) of sorted values.
fn pct(sorted: &[u128], p: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * p) / 100]
}

fn bench(port: &str, step: &str, n: usize) -> i32 {
    let mut c = match Client::connect(port) {
        Ok(c) => c,
        Err(e) => {
            println!("ERR {e}");
            return 1;
        }
    };
    // the guest clock moves in steps of about 1 ms on hvx's branch clock, so these percentiles
    // say little there (docs/HISTORY.md Milestone 237); reading the clock is guest work too
    let latencies = std::env::var("PKV_LATENCIES").as_deref() == Ok("1");
    let mut lat = Vec::with_capacity(n);
    for j in 0..n {
        let t = latencies.then(Instant::now);
        match c.ask(&format!("PUT k{step}.{j} v{step}.{j}")) {
            Ok(r) if r == "OK" => {}
            Ok(r) => {
                println!("ERR {r}");
                return 1;
            }
            Err(e) => {
                println!("ERR {e}");
                return 1;
            }
        }
        if let Some(t) = t {
            lat.push(t.elapsed().as_nanos());
        }
    }
    for j in 0..n {
        if c.ask(&format!("GET k{step}.{j}")).as_deref() != Ok(&format!("VAL v{step}.{j}")) {
            println!("ERR get k{step}.{j}");
            return 1;
        }
    }
    if c.ask("COMMIT").as_deref() != Ok("OK") {
        println!("ERR commit");
        return 1;
    }
    lat.sort_unstable();
    if let (true, Some(p)) = (latencies, std::env::var_os("HYDO_PERF")) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "put_p50.{step} {} ns", pct(&lat, 50));
            let _ = writeln!(f, "put_p99.{step} {} ns", pct(&lat, 99));
        }
    }
    println!("OK");
    0
}

fn check(port: &str, acked: &str, n: usize) -> i32 {
    let mut c = None;
    for _ in 0..50 {
        if let Ok(x) = Client::connect(port) {
            c = Some(x);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let Some(mut c) = c else {
        println!("pkv check: the server does not answer");
        return 2;
    };
    let steps = std::fs::read_to_string(acked).unwrap_or_default();
    let mut keys = 0;
    for s in steps.split_whitespace() {
        for j in 0..n {
            let want = format!("VAL v{s}.{j}");
            match c.ask(&format!("GET k{s}.{j}")) {
                Ok(r) if r == want => keys += 1,
                r => {
                    println!("pkv check: key k{s}.{j} lost (got {r:?})");
                    return 1;
                }
            }
        }
    }
    println!("pkv check: {keys} keys of the acknowledged steps all present");
    0
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n = |i: usize| a.get(i).and_then(|v| v.parse().ok()).unwrap_or(0);
    let code = match a.get(1).map(String::as_str) {
        Some("serve") if a.len() == 4 => {
            serve(&a[2], &a[3]);
            0
        }
        Some("ping") if a.len() == 3 => match Client::connect(&a[2]).and_then(|mut c| c.ask("PING")) {
            Ok(r) if r == "PONG" => 0,
            _ => 1,
        },
        Some("bench") if a.len() == 5 => bench(&a[2], &a[3], n(4)),
        Some("check") if a.len() == 5 => check(&a[2], &a[3], n(4)),
        _ => {
            eprintln!("usage: pkv serve <dir> <port> | ping <port> | bench <port> <step> <n> | check <port> <acked-file> <n>");
            2
        }
    };
    std::process::exit(code);
}
