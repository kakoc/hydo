//! The s3 emulator's proxy with the request shapes of real S3 clients (Milestone 329), end to end
//! through the `hydo-world` program, in front of a small S3 service this test runs. It keeps each
//! object's bytes as it received them, answers a HEAD with the object's size in Content-Length,
//! a GET with the bytes, and a PUT with `If-None-Match: *` over an object with 412.
//!
//! Before Milestone 329 the proxy dropped the service's Content-Length from every answer. So the
//! answer to a HEAD had no size: rclone read 0 and deleted the object it had just written as
//! damaged, and `object_store` failed with a missing Content-Length. It also cut every request
//! body to 1 MiB. The shapes:
//!
//! - rclone and the AWS SDKs: a PUT with Content-Length, then a HEAD for the size;
//! - the AWS SDKs' signed streaming upload: `Content-Encoding: aws-chunked` in an HTTP chunked
//!   body, after `Expect: 100-continue`;
//! - `object_store` and multipart parts: a body of 3 MB, read back with a GET whose answer has
//!   a Content-Length;
//! - conditional writes (`If-None-Match`, `If-Match`) on the routes `put_if_absent` and
//!   `put_if_match`, and an `H=` rule that answers them 412 without reaching the service.
//!
//! Fixed ports 28774 and 28775 (below Linux's ephemeral port range).

use crate::tmpdir::TmpDir;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A request the S3 service read: method, key, headers, body.
type Request = (String, String, Vec<(String, String)>, Vec<u8>);

type Store = Arc<Mutex<HashMap<String, (Vec<u8>, Vec<(String, String)>)>>>;

/// One request of the S3 service: method, path, headers (lowercase keys), body (a chunked body
/// is decoded as HTTP does; aws-chunked stays as it came).
fn read_req(r: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut it = line.split_whitespace();
    let m = it.next()?.to_string();
    // the key: the path without the query (`?x-id=PutObject`)
    let p = it.next()?.split('?').next()?.to_string();
    let mut hs = Vec::new();
    loop {
        let mut l = String::new();
        r.read_line(&mut l).ok()?;
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        hs.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let h = |k: &str| hs.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
    let mut body = Vec::new();
    if h("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut l = String::new();
            r.read_line(&mut l).ok()?;
            let n = usize::from_str_radix(l.trim().split(';').next()?, 16).ok()?;
            let mut c = vec![0u8; n + 2];
            r.read_exact(&mut c).ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&c[..n]);
        }
    } else if let Some(n) = h("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body = vec![0u8; n];
        r.read_exact(&mut body).ok()?;
    }
    Some((m, p, hs, body))
}

fn s3_service(ln: TcpListener, store: Store) {
    for c in ln.incoming().flatten() {
        let store = store.clone();
        std::thread::spawn(move || {
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut w = c.try_clone().unwrap();
            let mut r = BufReader::new(c);
            while let Some((m, p, hs, body)) = read_req(&mut r) {
                let h = |k: &str| hs.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone());
                let mut st = store.lock().unwrap();
                let (code, extra, out): (&str, String, Vec<u8>) = match m.as_str() {
                    "PUT" if h("if-none-match").is_some() && st.contains_key(&p) => {
                        ("412 Precondition Failed", String::new(), Vec::new())
                    }
                    "PUT" => {
                        st.insert(p.clone(), (body, hs.clone()));
                        ("200 OK", "ETag: \"e\"\r\n".into(), Vec::new())
                    }
                    "HEAD" | "GET" => match st.get(&p) {
                        Some((b, _)) => ("200 OK", String::new(), b.clone()),
                        None => ("404 Not Found", String::new(), Vec::new()),
                    },
                    _ => ("400 Bad Request", String::new(), Vec::new()),
                };
                drop(st);
                let mut a = format!(
                    "HTTP/1.1 {code}\r\nServer: AmazonS3\r\n{extra}Content-Length: {}\r\n\r\n",
                    out.len()
                )
                .into_bytes();
                if m != "HEAD" {
                    a.extend_from_slice(&out);
                }
                if w.write_all(&a).is_err() {
                    return;
                }
            }
        });
    }
}

/// The answer to raw request bytes: status line, headers (lowercase keys), body.
fn exchange(
    port: u16,
    head: &str,
    body: &[u8],
    wait_continue: bool,
) -> (String, Vec<(String, String)>, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(head.as_bytes()).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    if wait_continue {
        // an AWS SDK sends the body after the 100 Continue
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        assert!(l.starts_with("HTTP/1.1 100"), "{l}");
        r.read_line(&mut l).unwrap();
    }
    s.write_all(body).unwrap();
    let mut status = String::new();
    r.read_line(&mut status).unwrap();
    let mut hs = Vec::new();
    loop {
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        let l = l.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':').unwrap();
        hs.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let mut b = Vec::new();
    let _ = r.read_to_end(&mut b);
    (status.trim_end().to_string(), hs, b)
}

fn hv<'a>(hs: &'a [(String, String)], k: &str) -> Option<&'a str> {
    hs.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str())
}

fn ctl(sock: &std::path::Path, line: &str) -> String {
    let mut c = UnixStream::connect(sock).unwrap();
    c.write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut s = String::new();
    let _ = c.read_to_string(&mut s);
    s
}

#[test]
fn the_s3_proxy_carries_real_clients_uploads() {
    let (proxy, svc) = (28774u16, 28775u16);
    let dir =
        TmpDir::fresh(std::env::temp_dir().join(format!("hydo-world-ps3-{}", std::process::id())));
    let store: Store = Arc::default();
    let ln = TcpListener::bind(("127.0.0.1", svc)).unwrap();
    let st2 = store.clone();
    std::thread::spawn(move || s3_service(ln, st2));
    let sock = dir.join("world.sock");
    // the s3 emulator's proxy as `emulators: [{use: s3}]` expands it
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>PreconditionFailed</Code><RequestId>hydo-{seq}</RequestId></Error>";
    let cfg = serde_json::json!({
        "log": dir.join("world.log"), "sock": sock,
        "fakes": [],
        "proxies": [{
            "name": "s3", "port": proxy, "addr": "127.0.0.1", "protocol": "http",
            "to": format!("127.0.0.1:{svc}"),
            "routes": [
                {"name": "create_bucket", "method": "PUT", "path": "/{bucket}"},
                {"name": "put_if_absent", "method": "PUT", "path": "/*",
                 "if_header": "If-None-Match", "errors": {"412": xml}},
                {"name": "put_if_match", "method": "PUT", "path": "/*",
                 "if_header": "If-Match", "errors": {"412": xml}},
                {"name": "put_object", "method": "PUT", "path": "/*"},
                {"name": "head", "method": "HEAD", "path": "/*"},
                {"name": "read", "method": "GET", "path": "/*"}
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
        // rclone: a PUT, then a HEAD whose size it compares with the file's
        let put = "PUT /b/hello.txt?x-id=PutObject HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: 6\r\nContent-Md5: sZRqySSS0jR8YjW00mERhA==\r\nX-Amz-Content-Sha256: UNSIGNED-PAYLOAD\r\n\r\n";
        let (st, _, _) = exchange(proxy, put, b"hello\n", false);
        assert!(st.contains("200"), "{st}");
        let (st, hs, b) = exchange(
            proxy,
            "HEAD /b/hello.txt HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            b"",
            false,
        );
        assert!(st.contains("200"), "{st}");
        assert_eq!(hv(&hs, "content-length"), Some("6"), "{hs:?}");
        assert!(b.is_empty());

        // the AWS SDKs' signed streaming upload after Expect: 100-continue
        let aws = b"6;chunk-signature=00\r\nhello\n\r\n0;chunk-signature=00\r\n\r\n";
        let mut chunked = format!("{:x}\r\n", aws.len()).into_bytes();
        chunked.extend_from_slice(aws);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let head = "PUT /b/streamed HTTP/1.1\r\nHost: x\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\nContent-Encoding: aws-chunked\r\nX-Amz-Decoded-Content-Length: 6\r\nX-Amz-Content-Sha256: STREAMING-AWS4-HMAC-SHA256-PAYLOAD\r\n\r\n";
        let (st, _, _) = exchange(proxy, head, &chunked, true);
        assert!(st.contains("200"), "{st}");
        {
            let s = store.lock().unwrap();
            let (b, hs) = &s["/b/streamed"];
            assert_eq!(
                b.as_slice(),
                aws,
                "the aws-chunked body as the client sent it"
            );
            assert_eq!(hv(hs, "content-encoding"), Some("aws-chunked"));
            assert_eq!(hv(hs, "x-amz-decoded-content-length"), Some("6"));
        }

        // object_store: a body over 1 MiB, its size from a HEAD, its bytes from a GET
        let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let head = format!(
            "PUT /b/big HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            big.len()
        );
        let (st, _, _) = exchange(proxy, &head, &big, false);
        assert!(st.contains("200"), "{st}");
        assert_eq!(store.lock().unwrap()["/b/big"].0.len(), big.len());
        let (_, hs, _) = exchange(
            proxy,
            "HEAD /b/big HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            b"",
            false,
        );
        assert_eq!(hv(&hs, "content-length"), Some("3000000"));
        let (st, hs, b) = exchange(
            proxy,
            "GET /b/big HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
            b"",
            false,
        );
        assert!(st.contains("200"), "{st}");
        assert_eq!(hv(&hs, "content-length"), Some("3000000"), "{hs:?}");
        assert_eq!(hv(&hs, "transfer-encoding"), None);
        assert!(b == big, "the object read back");

        // conditional writes: the service's own 412 passes through
        let cput = |k: &str, hdr: &str| {
            format!("PUT /b/{k} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{hdr}\r\nContent-Length: 2\r\n\r\n")
        };
        let (st, _, _) = exchange(proxy, &cput("hello.txt", "If-None-Match: *"), b"v2", false);
        assert!(st.contains("412"), "{st}");
        // an H= rule on put_if_absent answers 412 itself, and the service never sees the PUT
        let on = ctl(&sock, "on 1 s3.put_if_absent@0-9:status=412");
        assert_eq!(on.trim(), "ok", "{on}");
        let (st, hs, b) = exchange(proxy, &cput("new", "If-None-Match: *"), b"v1", false);
        assert!(st.contains("412"), "{st}");
        assert_eq!(hv(&hs, "content-type"), Some("application/xml"));
        assert!(String::from_utf8_lossy(&b).contains("<Code>PreconditionFailed</Code>"));
        assert!(!store.lock().unwrap().contains_key("/b/new"));
        // a plain PUT and an If-Match PUT are other routes
        let (st, _, _) = exchange(proxy, &cput("plain", ""), b"v1", false);
        assert!(st.contains("200"), "{st}");
        let (st, _, _) = exchange(proxy, &cput("plain", "If-Match: \"e\""), b"v2", false);
        assert!(st.contains("200"), "{st}");
        ctl(&sock, "off 1");
        let log = std::fs::read_to_string(dir.join("world.log")).unwrap();
        let routes: Vec<String> = log
            .lines()
            .filter(|l| l.contains("kind=req") && l.contains("method=PUT"))
            .map(|l| {
                let f = |k: &str| l.split(' ').find_map(|x| x.strip_prefix(k)).unwrap_or("");
                format!("{} {}", f("route="), f("out="))
            })
            .collect();
        assert_eq!(
            routes,
            [
                "put_object fwd",
                "put_object fwd",
                "put_object fwd",
                "put_if_absent fwd",
                "put_if_absent -+status412",
                "put_object fwd",
                "put_if_match fwd",
            ],
            "{log}"
        );
    });
    let _ = child.kill();
    let _ = child.wait();
    drop(dir);
    if let Err(e) = r {
        std::panic::resume_unwind(e);
    }
}
