//! A fake route's `errors:` and the `{uuid}` template (Milestone 249, the fakes that
//! `hydo fake from-openapi` writes), end to end through the `hydo-world` program. An `H=` rule
//! `status=<code>` answers with the route's own error body for that status, else for its class
//! (`4XX`, `5XX`), else `default`, else hydo's own `{"error":"injected by hydo",...}`. `{uuid}`
//! is a UUID made from the answer's sequence number, so the same in every run, and a `save:` key
//! of `{uuid}` is found again by a later `load: '{path:id}'`. Fixed port 28773 (below Linux's
//! ephemeral port range).

use crate::tmpdir::TmpDir;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// One HTTP/1.1 exchange; the answer's status line and body.
fn call(port: u16, method: &str, path: &str, body: &str) -> (String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
fn error_answers_by_status_and_uuids_by_sequence() {
    let port = 28773u16;
    let dir =
        TmpDir::fresh(std::env::temp_dir().join(format!("hydo-world-ferr-{}", std::process::id())));
    let sock = dir.join("world.sock");
    let cfg = serde_json::json!({
        "log": dir.join("world.log"), "sock": sock,
        "fakes": [{
            "name": "pay", "port": port, "addr": "127.0.0.1", "protocol": "http",
            "routes": [
                {"name": "create", "method": "POST", "path": "/v1/things", "status": 201,
                 "body": r#"{"id":"{uuid}","amount":{json:amount}}"#,
                 "save": {"key": "{uuid}", "fields": {"amount": "{json:amount}"}},
                 "errors": {
                     "409": r#"{"code":"conflict","amount":{json:amount}}"#,
                     "5XX": r#"{"code":"server_error","seq":{seq}}"#
                 }},
                {"name": "get", "method": "GET", "path": "/v1/things/{id}",
                 "load": "{path:id}",
                 "body": r#"{"id":"{path:id}","amount":{obj:amount}}"#,
                 "errors": {"default": r#"{"code":"other","id":"{path:id}","amount":{obj:amount}}"#}},
                {"name": "plain", "method": "POST", "path": "/v1/plain"}
            ]
        }],
        "proxies": []
    });
    let cf = dir.join("world.json");
    std::fs::write(&cf, cfg.to_string()).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_hydo-world"))
        .arg("serve")
        .arg(&cf)
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() || !sock.exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "hydo-world did not listen"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let r = std::panic::catch_unwind(|| {
        let (st, b) = call(port, "POST", "/v1/things", r#"{"amount":7}"#);
        assert!(st.contains("201"), "{st}");
        assert_eq!(
            b,
            r#"{"id":"00000000-0000-4000-8000-000000000001","amount":7}"#
        );
        let (st, b) = call(
            port,
            "GET",
            "/v1/things/00000000-0000-4000-8000-000000000001",
            "",
        );
        assert!(st.contains("200"), "{st}");
        assert_eq!(
            b,
            r#"{"id":"00000000-0000-4000-8000-000000000001","amount":7}"#
        );
        // the exact status
        assert_eq!(ctl(&sock, "on 1 pay.create@0-9:status=409").trim(), "ok");
        let (st, b) = call(port, "POST", "/v1/things", r#"{"amount":8}"#);
        assert!(st.contains("409"), "{st}");
        assert_eq!(b, r#"{"code":"conflict","amount":8}"#);
        ctl(&sock, "off 1");
        // the class; the answer has its own sequence number
        assert_eq!(ctl(&sock, "on 2 pay.create@0-9:status=503").trim(), "ok");
        let (st, b) = call(port, "POST", "/v1/things", r#"{"amount":9}"#);
        assert!(st.contains("503"), "{st}");
        assert_eq!(b, r#"{"code":"server_error","seq":4}"#);
        ctl(&sock, "off 2");
        // no body for 400 on this route: hydo's own
        assert_eq!(ctl(&sock, "on 3 pay.create@0-9:status=400").trim(), "ok");
        let (st, b) = call(port, "POST", "/v1/things", r#"{"amount":9}"#);
        assert!(st.contains("400"), "{st}");
        assert_eq!(b, r#"{"error":"injected by hydo","status":400}"#);
        ctl(&sock, "off 3");
        // default, with the loaded object's fields
        assert_eq!(ctl(&sock, "on 4 pay.get@0-9:status=500").trim(), "ok");
        let (st, b) = call(
            port,
            "GET",
            "/v1/things/00000000-0000-4000-8000-000000000001",
            "",
        );
        assert!(st.contains("500"), "{st}");
        assert_eq!(
            b,
            r#"{"code":"other","id":"00000000-0000-4000-8000-000000000001","amount":7}"#
        );
        ctl(&sock, "off 4");
        // a route without errors: hydo's own
        assert_eq!(ctl(&sock, "on 5 pay.plain@0-9:status=502").trim(), "ok");
        let (st, b) = call(port, "POST", "/v1/plain", "{}");
        assert!(st.contains("502"), "{st}");
        assert_eq!(b, r#"{"error":"injected by hydo","status":502}"#);
    });
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = r {
        std::panic::resume_unwind(e);
    }
}
