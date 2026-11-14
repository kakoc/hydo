//! An HTTP proxy route with `idempotent: true` (Milestone 242, the emulators of
//! docs/design/emulators.md), end to end through the `hydo-world` program: the proxy stands for a
//! provider that keeps the first answer of every idempotency key, as Stripe does, in front of an
//! emulator that keeps none. A repeat of a key gets the first answer without reaching the
//! service and is recorded as a replay (`fx=0 out=replay orig=<seq>`); a new key reaches it. With
//! `status=500,effect=yes` the service acts, the client sees 500, and its retry with the same key
//! gets the stored answer. Fixed ports 28771 and 28772 (below Linux's ephemeral port range).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One HTTP/1.1 exchange with an idempotency key; the answer's status line and body.
fn post(port: u16, path: &str, key: &str, body: &str) -> (String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nIdempotency-Key: {key}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut b = Vec::new();
    let _ = s.read_to_end(&mut b);
    let t = String::from_utf8_lossy(&b).into_owned();
    let (head, body) = t.split_once("\r\n\r\n").unwrap_or((&t, ""));
    (
        head.lines().next().unwrap_or("").to_string(),
        body.to_string(),
    )
}

/// A control command on the world's socket; its answer.
fn ctl(sock: &std::path::Path, line: &str) -> String {
    let mut c = UnixStream::connect(sock).unwrap();
    c.write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut s = String::new();
    let _ = c.read_to_string(&mut s);
    s
}

#[test]
fn a_proxy_replays_the_first_answer_of_a_key() {
    let (proxy, svc) = (28771u16, 28772u16);
    let dir = std::env::temp_dir().join(format!("hydo-world-pidem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // the emulator: every request it gets is a new object, numbered
    let n = Arc::new(AtomicUsize::new(0));
    let ln = TcpListener::bind(("127.0.0.1", svc)).unwrap();
    let n2 = n.clone();
    std::thread::spawn(move || {
        for c in ln.incoming().flatten() {
            let mut c = c;
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut b = vec![0u8; 8192];
            let mut got = Vec::new();
            while let Ok(k) = c.read(&mut b) {
                if k == 0 {
                    break;
                }
                got.extend_from_slice(&b[..k]);
                let t = String::from_utf8_lossy(&got);
                if let Some((h, body)) = t.split_once("\r\n\r\n") {
                    let len = h
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
            }
            let k = n2.fetch_add(1, Ordering::SeqCst) + 1;
            let body = format!(r#"{{"id":"ch_{k}"}}"#);
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let sock = dir.join("world.sock");
    let cfg = serde_json::json!({
        "log": dir.join("world.log"), "sock": sock,
        "fakes": [],
        "proxies": [{
            "name": "stripe", "port": proxy, "addr": "127.0.0.1", "protocol": "http",
            "to": format!("127.0.0.1:{svc}"),
            "idempotency": "header:Idempotency-Key", "ref": "form:metadata[order]",
            "routes": [
                {"name": "charge", "method": "POST", "path": "/v1/charges", "idempotent": true},
                {"name": "refund", "method": "POST", "path": "/v1/refunds"}
            ]
        }]
    });
    let cf = dir.join("world.json");
    std::fs::write(&cf, cfg.to_string()).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_hydo-world"))
        .arg("serve")
        .arg(&cf)
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while TcpStream::connect(("127.0.0.1", proxy)).is_err() || !sock.exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "hydo-world did not listen"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let r = std::panic::catch_unwind(|| {
        let order = "amount=500&metadata[order]=o1";
        let (st, b) = post(proxy, "/v1/charges", "k1", order);
        assert!(st.contains("200"), "{st}");
        assert_eq!(b, r#"{"id":"ch_1"}"#);
        // the same key again: the first answer, the service is not called
        let (st, b) = post(proxy, "/v1/charges", "k1", order);
        assert!(st.contains("200"), "{st}");
        assert_eq!(b, r#"{"id":"ch_1"}"#);
        assert_eq!(n.load(Ordering::SeqCst), 1);
        // a new key is a new charge
        let (_, b) = post(proxy, "/v1/charges", "k2", order);
        assert_eq!(b, r#"{"id":"ch_2"}"#);
        // a route without idempotent: every request reaches the service
        post(proxy, "/v1/refunds", "r1", order);
        post(proxy, "/v1/refunds", "r1", order);
        assert_eq!(n.load(Ordering::SeqCst), 4);
        // charged, then failed to say so; the retry with the same key gets the charge
        let on = ctl(&sock, "on 1 stripe.charge@0-9:status=500,effect=yes");
        assert_eq!(on.trim(), "ok", "{on}");
        let (st, _) = post(proxy, "/v1/charges", "k3", order);
        assert!(st.contains("500"), "{st}");
        assert_eq!(n.load(Ordering::SeqCst), 5);
        ctl(&sock, "off 1");
        let (st, b) = post(proxy, "/v1/charges", "k3", order);
        assert!(st.contains("200"), "{st}");
        assert_eq!(b, r#"{"id":"ch_5"}"#);
        assert_eq!(n.load(Ordering::SeqCst), 5);
        let log = std::fs::read_to_string(dir.join("world.log")).unwrap();
        let outs: Vec<String> = log
            .lines()
            .filter(|l| l.contains("kind=req"))
            .map(|l| {
                let f = |k: &str| {
                    l.split(' ')
                        .find_map(|x| x.strip_prefix(k))
                        .unwrap_or("")
                        .to_string()
                };
                format!("{} {} {}", f("route="), f("fx="), f("out="))
            })
            .collect();
        assert_eq!(
            outs,
            [
                "charge 1 fwd",
                "charge 0 replay",
                "charge 1 fwd",
                "refund 1 fwd",
                "refund 1 fwd",
                "charge 1 fwd+status500",
                "charge 0 replay",
            ],
            "{log}"
        );
        assert!(log.contains("out=replay orig=1"), "{log}");
        assert!(log.contains("out=replay orig=6"), "{log}");
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        std::panic::resume_unwind(e);
    }
}
