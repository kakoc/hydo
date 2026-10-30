//! rkv: a tiny durable key/value server over TCP on loopback, written to show the Rust
//! assertion SDK (sdk/rust, crate hydo-assert) under hydo. One binary, three roles:
//!
//!   rkv serve <dir> <port>             append-only log in <dir>/log, one "PUT k v" per line
//!   rkv put   <port> <key> <value>     prints OK, or ERR <why> (exit 1)
//!   rkv ping  <port>                   exit 0 when the server answers (readiness; no records)
//!   rkv check <port> <acked-file>      oracle: every acknowledged key k:<i> reads back as v<i>
//!                                      (the file lists the acknowledged step numbers i)
//!
//! The server acknowledges a PUT after the log line is written and, unless the file
//! /tmp/rkv-fsync says "never" (read at every PUT), after fdatasync. "never" is the relaxed
//! setting the explorer may choose (knob S): then a power cut loses acknowledged writes, which
//! the oracle's always-assertion reports.
use hydo_assert::{declare, details, hydo_always, hydo_always_less_than_or_equal_to, hydo_buggify, hydo_reachable, hydo_sometimes};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

declare! {
    static ACKED_DURABLE: always("rkv.acked_durable", "every acknowledged PUT reads back with its value");
    static LOG_PARSES: always_or_unreachable("rkv.log_line_parses", "every complete log line parses as PUT k v");
    static RECOVERED: sometimes("rkv.recovered", "the server replayed a non-empty log at start");
    static SERVES: eventually("rkv.serves_after_start", "the server answers a GET within 5 s of the oracle's start", std::time::Duration::from_secs(5));
    static LOST_REPLY: buggify("rkv.put.lost_reply", "the PUT is durable but the reply is lost");
    static SLOW_FSYNC: buggify("rkv.put.slow_fsync", "fdatasync takes 3 s");
}

const MAX_LOG: u64 = 1 << 20;

struct Store {
    map: BTreeMap<String, String>,
    log: File,
    size: u64,
}

fn replay(path: &std::path::Path) -> (BTreeMap<String, String>, u64) {
    let mut map = BTreeMap::new();
    let mut buf = Vec::new();
    if let Ok(mut f) = File::open(path) {
        let _ = f.read_to_end(&mut buf);
    }
    let complete = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    if complete < buf.len() {
        // a write cut short by a crash: the tail was never acknowledged
        hydo_reachable!("rkv.torn_tail" => "the log ended in a partial line (cut off by a crash)", {"bytes": (buf.len() - complete) as u64});
    }
    for line in String::from_utf8_lossy(&buf[..complete]).lines() {
        let mut it = line.splitn(3, ' ');
        let ok = match (it.next(), it.next(), it.next()) {
            (Some("PUT"), Some(k), Some(v)) => {
                map.insert(k.to_string(), v.to_string());
                true
            }
            _ => false,
        };
        LOG_PARSES.check(ok, details! {"line": line});
    }
    (map, complete as u64)
}

fn fsync_on() -> bool {
    std::fs::read_to_string("/tmp/rkv-fsync").map_or(true, |v| v.trim() != "never")
}

fn serve(dir: &str, port: &str) {
    let path = std::path::Path::new(dir).join("log");
    let (map, size) = replay(&path);
    if !map.is_empty() {
        RECOVERED.check(true, details! {"keys": map.len()});
    }
    // drop a torn tail before appending, so the next line starts on a line boundary
    let log = OpenOptions::new().create(true).write(true).open(&path).expect("open log");
    log.set_len(size).expect("truncate log");
    drop(log);
    let log = OpenOptions::new().append(true).open(&path).expect("open log");
    let st = Arc::new(Mutex::new(Store { map, log, size }));
    let l = TcpListener::bind(format!("127.0.0.1:{}", port)).expect("bind");
    for c in l.incoming() {
        let Ok(c) = c else { continue };
        let st = st.clone();
        std::thread::spawn(move || conn(c, st));
    }
}

fn conn(c: TcpStream, st: Arc<Mutex<Store>>) {
    let mut w = c.try_clone().expect("clone");
    for line in BufReader::new(c).lines() {
        let Ok(line) = line else { return };
        let mut it = line.splitn(3, ' ');
        let reply = match (it.next(), it.next(), it.next()) {
            (Some("PUT"), Some(k), Some(v)) => {
                let mut s = st.lock().unwrap();
                let rec = format!("PUT {} {}\n", k, v);
                if s.log.write_all(rec.as_bytes()).is_err() {
                    "ERR write".to_string()
                } else {
                    s.size += rec.len() as u64;
                    if fsync_on() {
                        if hydo_buggify!("rkv.fsync.before" => "a 50 ms stall before fdatasync") {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        if SLOW_FSYNC.fire(details! {"key": k}) {
                            std::thread::sleep(Duration::from_secs(3));
                        }
                        let _ = s.log.sync_data();
                    }
                    s.map.insert(k.to_string(), v.to_string());
                    hydo_always_less_than_or_equal_to!(s.size, MAX_LOG, "rkv.log_size" => "the log stays under 1 MiB");
                    if LOST_REPLY.fire(details! {"key": k}) {
                        return; // the connection closes with no reply; the write is durable
                    }
                    "OK".to_string()
                }
            }
            (Some("GET"), Some(k), None) => {
                let s = st.lock().unwrap();
                s.map.get(k).map_or("NONE".to_string(), |v| format!("VAL {}", v))
            }
            (Some("PING"), None, None) => "PONG".to_string(),
            _ => "ERR syntax".to_string(),
        };
        if w.write_all(format!("{}\n", reply).as_bytes()).is_err() {
            return;
        }
    }
}

fn request(port: &str, line: &str, timeout: Duration) -> Result<String, String> {
    let c = TcpStream::connect(format!("127.0.0.1:{}", port)).map_err(|e| format!("connect: {}", e))?;
    c.set_read_timeout(Some(timeout)).ok();
    (&c).write_all(format!("{}\n", line).as_bytes()).map_err(|e| format!("send: {}", e))?;
    let mut r = String::new();
    BufReader::new(&c).read_line(&mut r).map_err(|e| format!("recv: {}", e))?;
    if r.is_empty() {
        return Err("closed without a reply".into());
    }
    Ok(r.trim_end().to_string())
}

fn put(port: &str, k: &str, v: &str) -> i32 {
    match request(port, &format!("PUT {} {}", k, v), Duration::from_secs(2)) {
        Ok(r) if r == "OK" => {
            println!("OK");
            0
        }
        Ok(r) => {
            println!("ERR {}", r);
            1
        }
        Err(e) => {
            hydo_sometimes!(true, "rkv.client.no_reply" => "a PUT got no reply (outcome unknown)", {"key": k, "why": e.as_str()});
            println!("ERR {}", e);
            1
        }
    }
}

fn check(port: &str, acked: &str) -> i32 {
    // The oracle carries the catalog of the whole binary (every declared property and every
    // macro site of serve, put and check): it runs in every schedule, after the workload and
    // after a power cut's recovery, so its console always holds the DECL lines. (The server
    // starts before the explorer's S1 snapshot, so what it wrote then is not in a run's console.)
    hydo_assert::catalog();
    let up = SERVES.await_cond(|| request(port, "PING", Duration::from_millis(500)).is_ok(), Duration::from_millis(100), ());
    if !up {
        println!("rkv check: the server does not answer");
        return 2;
    }
    let keys = std::fs::read_to_string(acked).unwrap_or_default();
    let (mut n, mut lost) = (0u64, 0u64);
    for i in keys.split_whitespace() {
        n += 1;
        let k = format!("k:{}", i);
        let want = format!("VAL v{}", i);
        // A transport error is "could not ask", not "lost": the server may still be busy with
        // PUTs whose clients gave up (a slow fdatasync). Ask again for up to 30 s; if it never
        // answers, the check cannot be evaluated (exit 2), which is not a violation.
        let mut got = None;
        for _ in 0..15 {
            match request(port, &format!("GET {}", k), Duration::from_secs(2)) {
                Ok(r) => {
                    got = Some(r);
                    break;
                }
                Err(_) => hydo_sometimes!(true, "rkv.check.retried_get" => "the oracle had to ask again (the server was busy)"),
            };
        }
        let Some(got) = got else {
            println!("rkv check: the server stopped answering at key {}", k);
            return 2;
        };
        let ok = ACKED_DURABLE.check(got == want, details! {"key": k.as_str(), "got": got.as_str(), "want": want.as_str()});
        if !ok {
            lost += 1;
            if lost == 1 {
                println!("rkv check: key {} lost (got {:?})", k, got);
            }
        }
    }
    hydo_always!(n > 0 || keys.is_empty(), "rkv.check_read_acked" => "the oracle read the acknowledged keys");
    if lost == 0 {
        println!("rkv check: acked={} all present", n);
        0
    } else {
        println!("rkv check: acked={} lost={}", n, lost);
        1
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let code = match a.get(1).map(String::as_str) {
        Some("serve") if a.len() == 4 => {
            serve(&a[2], &a[3]);
            0
        }
        Some("put") if a.len() == 5 => put(&a[2], &a[3], &a[4]),
        Some("ping") if a.len() == 3 => match request(&a[2], "PING", Duration::from_millis(500)) {
            Ok(r) if r == "PONG" => 0,
            _ => 1,
        },
        Some("check") if a.len() == 4 => check(&a[2], &a[3]),
        _ => {
            eprintln!("usage: rkv serve <dir> <port> | put <port> <key> <value> | ping <port> | check <port> <acked-file>");
            2
        }
    };
    std::process::exit(code);
}
