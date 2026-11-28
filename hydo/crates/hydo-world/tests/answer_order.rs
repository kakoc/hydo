//! Answers in another order and requests sent twice (Milestone 393, the rules `order=<k>` and
//! `twice`), end to end through the `hydo-world` program, on a fake and on an HTTP proxy in front
//! of a service that numbers what it gets. Fixed ports 28781 to 28783 (below Linux's ephemeral
//! port range).

use crate::tmpdir::TmpDir;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One HTTP/1.1 exchange; the answer's status line and body.
fn ask(port: u16, method: &str, target: &str, key: &str) -> (String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let key = if key.is_empty() {
        String::new()
    } else {
        format!("Idempotency-Key: {key}\r\n")
    };
    write!(
        s,
        "{method} {target} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{key}Content-Length: 0\r\n\r\n"
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

fn ctl(sock: &std::path::Path, line: &str) -> String {
    let mut c = UnixStream::connect(sock).unwrap();
    c.write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut s = String::new();
    let _ = c.read_to_string(&mut s);
    s
}

/// Two requests at once, the second 100 ms after the first; the bodies in the order their
/// answers came.
fn two_at_once(port: u16, a: &str, b: &str) -> Vec<String> {
    let got = Arc::new(Mutex::new(Vec::new()));
    let mut ts = Vec::new();
    for (i, t) in [a, b].into_iter().enumerate() {
        let (got, t) = (got.clone(), t.to_string());
        ts.push(std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100 * i as u64));
            let (_, body) = ask(port, "GET", &t, "");
            got.lock().unwrap().push(body);
        }));
    }
    for t in ts {
        t.join().unwrap();
    }
    let v = got.lock().unwrap().clone();
    v
}

#[test]
fn answers_in_another_order_and_requests_sent_twice() {
    let (fake, proxy, svc) = (28781u16, 28782u16, 28783u16);
    let dir = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-world-order-{}", std::process::id())),
    );
    // the service behind the proxy: answers with what it was asked and how many requests it got
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
                if got.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let t = String::from_utf8_lossy(&got).into_owned();
            let target = t.split(' ').nth(1).unwrap_or("").to_string();
            let k = n2.fetch_add(1, Ordering::SeqCst) + 1;
            let body = format!(r#"{{"n":{k},"for":"{target}"}}"#);
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
        "fakes": [{
            "name": "api", "port": fake, "addr": "127.0.0.1", "protocol": "http",
            "idempotency": "header:Idempotency-Key",
            "routes": [
                {"name": "search", "method": "GET", "path": "/api/search", "status": 200,
                 "headers": {"Content-Type": "application/json"}, "body": r#"{"q":"{query:q}"}"#},
                {"name": "order", "method": "POST", "path": "/api/orders", "status": 200,
                 "idempotent": true, "headers": {"Content-Type": "application/json"},
                 "body": r#"{"id":"o{seq}"}"#}
            ]
        }],
        "proxies": [{
            "name": "app", "port": proxy, "addr": "127.0.0.1", "protocol": "http",
            "to": format!("127.0.0.1:{svc}"),
            "routes": [
                {"name": "list", "method": "GET", "path": "/list"},
                {"name": "save", "method": "POST", "path": "/save"}
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
        // without a rule the answers come in the order of the requests
        assert_eq!(
            two_at_once(fake, "/api/search?q=c", "/api/search?q=co"),
            [r#"{"q":"c"}"#, r#"{"q":"co"}"#]
        );
        // order=2: the second request's answer first, on the fake and on the proxy
        let on = ctl(&sock, "on 1 api.search@0-9:order=2,wait=5000");
        assert_eq!(on.trim(), "ok", "{on}");
        assert_eq!(
            two_at_once(fake, "/api/search?q=c", "/api/search?q=co"),
            [r#"{"q":"co"}"#, r#"{"q":"c"}"#]
        );
        ctl(&sock, "off 1");
        assert_eq!(ctl(&sock, "on 2 app.list@0-9:order=2").trim(), "ok");
        let got = two_at_once(proxy, "/list?a", "/list?b");
        assert!(
            got[0].contains("/list?b") && got[1].contains("/list?a"),
            "{got:?}"
        );
        // a lone answer goes out after wait=
        let t = Instant::now();
        let (_, b) = ask(proxy, "GET", "/list?c", "");
        assert!(b.contains("/list?c") && t.elapsed() >= Duration::from_millis(2900));
        ctl(&sock, "off 2");
        // twice: the service gets the request two times, the client the second answer
        let before = n.load(Ordering::SeqCst);
        assert_eq!(ctl(&sock, "on 3 app.save@0-9:twice").trim(), "ok");
        let (st, b) = ask(proxy, "POST", "/save", "");
        assert!(st.contains("200"), "{st}");
        assert_eq!(n.load(Ordering::SeqCst), before + 2);
        assert!(b.contains(&format!(r#""n":{}"#, before + 2)), "{b}");
        ctl(&sock, "off 3");
        // on a fake's idempotent route a second sending with the key is a replay
        assert_eq!(ctl(&sock, "on 4 api.order@0-9:twice").trim(), "ok");
        let (_, b) = ask(fake, "POST", "/api/orders", "k1");
        let (_, b2) = ask(fake, "POST", "/api/orders", "");
        ctl(&sock, "off 4");
        let log = std::fs::read_to_string(dir.join("world.log")).unwrap();
        let outs: Vec<String> = log
            .lines()
            .filter(|l| l.contains("kind=req") && l.contains("route=order"))
            .map(|l| {
                let f = |k: &str| {
                    l.split(' ')
                        .find_map(|x| x.strip_prefix(k))
                        .unwrap_or("")
                        .to_string()
                };
                format!("{} {}", f("fx="), f("out="))
            })
            .collect();
        assert_eq!(
            outs,
            ["1 ok+twice", "0 replay", "1 ok+twice", "1 ok"],
            "{log}"
        );
        assert!(log.contains("copy="), "{log}");
        // the key's second sending got the first answer; without a key there are two orders
        assert!(b.starts_with(r#"{"id":"o"#), "{b}");
        assert!(b2.starts_with(r#"{"id":"o"#), "{b2}");
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        std::panic::resume_unwind(e);
    }
}
