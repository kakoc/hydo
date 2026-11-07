//! A fake that remembers what it answered and signs its webhooks, end to end through the
//! `hydo-world` program: a payment intent created with a currency is read back with it, a capture
//! changes its status, a read of an unknown id is answered 404 and takes no effect, and the
//! creation's webhook carries `t=<ts>,v1=<HMAC-SHA256 of "<ts>.<body>">` under the endpoint
//! secret (src/objects.rs, src/sign.rs). Fixed ports from `HYDO_OBJECTS_PORT` (default 38760).

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn hmac(key: &[u8], msg: &[u8]) -> String {
    let mut k = [0u8; 64];
    k[..key.len()].copy_from_slice(key);
    let mut i = Sha256::new();
    i.update(k.map(|x| x ^ 0x36));
    i.update(msg);
    let mut o = Sha256::new();
    o.update(k.map(|x| x ^ 0x5c));
    o.update(i.finalize());
    o.finalize().iter().map(|x| format!("{x:02x}")).collect()
}

/// One HTTP/1.1 exchange; the answer's status line and body.
fn http(port: u16, method: &str, path: &str, body: &str) -> (String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
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

#[test]
fn objects_and_signed_webhooks() {
    let base: u16 = std::env::var("HYDO_OBJECTS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(38760);
    let (stripe, hook) = (base + 1, base + 2);
    let dir = std::env::temp_dir().join(format!("hydo-world-objects-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let show = r#"{"id":"{path:id}","currency":"{obj:currency}","amount":{obj:amount},"status":"{obj:status}"}"#;
    let cfg = serde_json::json!({
        "log": dir.join("world.log"), "sock": dir.join("world.sock"),
        "fakes": [{
            "name": "stripe", "port": stripe, "ref": "form:metadata[order]",
            "routes": [
                {"name": "intent_create", "method": "POST", "path": "/v1/payment_intents",
                 "body": r#"{"id":"pi_{seq}","currency":"{obj:currency}","status":"{obj:status}"}"#,
                 "save": {"key": "pi_{seq}", "fields": {"currency": "{form:currency}", "amount": "{form:amount}", "status": "requires_capture"}},
                 "webhook": {"url": format!("http://127.0.0.1:{hook}/hooks"),
                             "body": r#"{"type":"payment_intent.created","id":"pi_{seq}"}"#,
                             "retries": 1,
                             "sign": {"header": "Stripe-Signature", "secret": "whsec_test"}}},
                {"name": "intent_capture", "method": "POST", "path": "/v1/payment_intents/{id}/capture",
                 "load": "{path:id}", "save": {"key": "{path:id}", "fields": {"status": "succeeded"}}, "body": show},
                {"name": "intent_get", "method": "GET", "path": "/v1/payment_intents/{id}", "load": "{path:id}", "body": show}
            ]
        }]
    });
    let cf = dir.join("world.json");
    std::fs::write(&cf, cfg.to_string()).unwrap();
    // the webhook receiver: every request's head and body
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let ln = TcpListener::bind(("127.0.0.1", hook)).unwrap();
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for c in ln.incoming().flatten() {
            let mut c = c;
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut b = vec![0u8; 8192];
            let mut got = Vec::new();
            while let Ok(n) = c.read(&mut b) {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&b[..n]);
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
            let _ =
                c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            s2.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&got).into_owned());
        }
    });
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_hydo-world"))
        .arg("serve")
        .arg(&cf)
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while TcpStream::connect(("127.0.0.1", stripe)).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "hydo-world did not listen"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let r = std::panic::catch_unwind(|| {
        let (st, b) = http(
            stripe,
            "POST",
            "/v1/payment_intents",
            "amount=1250&currency=eur&metadata[order]=o7",
        );
        assert!(st.contains("200"), "{st}");
        assert_eq!(
            b,
            r#"{"id":"pi_1","currency":"eur","status":"requires_capture"}"#
        );
        let (_, b) = http(stripe, "GET", "/v1/payment_intents/pi_1", "");
        assert_eq!(
            b,
            r#"{"id":"pi_1","currency":"eur","amount":1250,"status":"requires_capture"}"#
        );
        let (_, b) = http(stripe, "POST", "/v1/payment_intents/pi_1/capture", "");
        assert_eq!(
            b,
            r#"{"id":"pi_1","currency":"eur","amount":1250,"status":"succeeded"}"#
        );
        let (_, b) = http(stripe, "GET", "/v1/payment_intents/pi_1", "");
        assert!(b.contains(r#""status":"succeeded""#), "{b}");
        let (st, b) = http(stripe, "POST", "/v1/payment_intents/pi_9/capture", "");
        assert!(st.contains("404"), "{st}");
        assert_eq!(b, r#"{"error":"hydo fake: no object pi_9"}"#);
        // the webhook of the creation, signed
        let t1 = Instant::now();
        while seen.lock().unwrap().is_empty() {
            assert!(t1.elapsed() < Duration::from_secs(20), "no webhook");
            std::thread::sleep(Duration::from_millis(50));
        }
        let req = seen.lock().unwrap()[0].clone();
        let (head, body) = req.split_once("\r\n\r\n").unwrap();
        assert_eq!(body, r#"{"type":"payment_intent.created","id":"pi_1"}"#);
        let sig = head
            .lines()
            .find_map(|l| l.strip_prefix("Stripe-Signature: "))
            .unwrap_or_else(|| panic!("no signature header: {head}"));
        let (t, v1) = sig.split_once(",v1=").unwrap();
        let ts = t.strip_prefix("t=").unwrap();
        assert!(ts.parse::<i64>().is_ok(), "{sig}");
        assert_eq!(v1, hmac(b"whsec_test", format!("{ts}.{body}").as_bytes()));
        // the record: the unknown id took no effect
        let log = std::fs::read_to_string(dir.join("world.log")).unwrap();
        let miss = log
            .lines()
            .find(|l| l.contains("path=/v1/payment_intents/pi_9/capture"))
            .unwrap();
        assert!(
            miss.contains("fx=0") && miss.contains("out=- obj=missing"),
            "{miss}"
        );
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        std::panic::resume_unwind(e);
    }
}
