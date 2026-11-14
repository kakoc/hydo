//! The install cache against a fake release on a local HTTP server (no real network): verified
//! and cached downloads, corrupted and truncated ones refused or resumed, retries, kernels taken
//! out of the release that shipped them, two versions sharing one cache, offline use, archives
//! put in by hand, and `hydo setup --check` / `hydo setup` restoring a damaged release.

use super::*;
use crate::ctx::Platform;
use crate::fetch::{sha256_bytes, Options};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ------------------------------------------------------------------ the server

/// How the server answers one path.
#[derive(Clone, Debug, Default)]
struct Route {
    body: Vec<u8>,
    /// Answer the first this many requests with this status and no body.
    fail_first: Option<(u32, u16)>,
    /// Cut the body of the first this many requests after this many bytes.
    cut_first: Option<(u32, usize)>,
    /// Answer a Range request with the whole file (200).
    ignore_range: bool,
}

#[derive(Default)]
struct State {
    routes: HashMap<String, Route>,
    /// (path, Range header) of every request.
    log: Vec<(String, Option<String>)>,
    seen: HashMap<String, u32>,
}

struct Server {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Server {
    fn start() -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let state: Arc<Mutex<State>> = Arc::default();
        let st = state.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let st = st.clone();
                std::thread::spawn(move || serve(c, st));
            }
        });
        Server { port, state }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}/releases", self.port)
    }

    fn put(&self, path: &str, r: Route) {
        self.state.lock().unwrap().routes.insert(path.into(), r);
    }

    fn file(&self, path: &str, body: &[u8]) {
        self.put(
            path,
            Route {
                body: body.to_vec(),
                ..Route::default()
            },
        );
    }

    fn log(&self) -> Vec<(String, Option<String>)> {
        self.state.lock().unwrap().log.clone()
    }

    fn requests(&self) -> usize {
        self.state.lock().unwrap().log.len()
    }
}

fn serve(c: TcpStream, st: Arc<Mutex<State>>) {
    let mut w = c.try_clone().unwrap();
    let mut r = BufReader::new(c);
    let mut first = String::new();
    if r.read_line(&mut first).is_err() {
        return;
    }
    let mut range = None;
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("range") {
                range = Some(v.trim().to_string());
            }
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
    let (route, n) = {
        let mut s = st.lock().unwrap();
        s.log.push((path.clone(), range.clone()));
        let n = {
            let e = s.seen.entry(path.clone()).or_insert(0);
            *e += 1;
            *e
        };
        (s.routes.get(&path).cloned(), n)
    };
    let send = |w: &mut TcpStream, status: &str, headers: &[String], body: &[u8]| {
        let mut h = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        for x in headers {
            h.push_str(x);
            h.push_str("\r\n");
        }
        h.push_str("\r\n");
        let _ = w.write_all(h.as_bytes());
        let _ = w.write_all(body);
        let _ = w.flush();
    };
    let Some(route) = route else {
        send(&mut w, "404 Not Found", &["Content-Length: 0".into()], b"");
        return;
    };
    if let Some((k, st)) = route.fail_first {
        if n <= k {
            send(
                &mut w,
                &format!("{st} Failing"),
                &["Content-Length: 0".into()],
                b"",
            );
            return;
        }
    }
    let body = &route.body;
    let start = range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|r| r.parse::<usize>().ok())
        .filter(|_| !route.ignore_range);
    let (status, headers, part): (String, Vec<String>, &[u8]) = match start {
        Some(s) if s >= body.len() => ("416 Range Not Satisfiable".into(), vec![], &[]),
        Some(s) => (
            "206 Partial Content".into(),
            vec![format!(
                "Content-Range: bytes {s}-{}/{}",
                body.len() - 1,
                body.len()
            )],
            &body[s..],
        ),
        None => ("200 OK".into(), vec![], &body[..]),
    };
    let mut headers = headers;
    headers.push(format!("Content-Length: {}", part.len()));
    if let Some((k, cut)) = route.cut_first {
        if n <= k && cut < part.len() {
            send(&mut w, &status, &headers, &part[..cut]);
            let _ = w.shutdown(std::net::Shutdown::Both);
            return;
        }
    }
    send(&mut w, &status, &headers, part);
}

// ------------------------------------------------------------------ fake releases

static N: AtomicU32 = AtomicU32::new(0);

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydo-cache-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    remove_tree(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

const KERNEL: &[u8] = b"a guest kernel shared by both releases";

/// A release archive `hydo-linux-<version>.tar.gz`: its files with modes, and a SHA256SUMS of
/// them (as the release scripts write it).
fn archive(version: &str, extra: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let vfile = format!("{version}\n");
    let mut files: Vec<(String, Vec<u8>, u32)> = vec![
        ("VERSION".into(), vfile.into_bytes(), 0o644),
        (
            "bin/hydo".into(),
            format!("#!hydo {version}\n").into_bytes(),
            0o755,
        ),
        ("guest/bzImage".into(), KERNEL.to_vec(), 0o644),
        ("README.md".into(), b"the release\n".to_vec(), 0o644),
    ];
    for (p, b, m) in extra {
        files.push((p.to_string(), b.to_vec(), *m));
    }
    files.sort();
    let mut sums = String::new();
    for (p, b, _) in &files {
        sums.push_str(&format!("{}  {p}\n", sha256_bytes(b)));
    }
    files.push(("SHA256SUMS".into(), sums.into_bytes(), 0o644));
    let top = top_dir("linux", version);
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut t = tar::Builder::new(gz);
    for (p, b, m) in &files {
        let mut h = tar::Header::new_gnu();
        h.set_size(b.len() as u64);
        h.set_mode(*m);
        h.set_mtime(0);
        h.set_entry_type(tar::EntryType::Regular);
        t.append_data(&mut h, format!("{top}/{p}"), &b[..]).unwrap();
    }
    t.into_inner().unwrap().finish().unwrap()
}

/// The index: 0.1.0 published (its archive pinned), 0.2.0 the running hydo's own version.
fn index(a010: &[u8]) -> Index {
    let k = sha256_bytes(KERNEL);
    let text = format!(
        "archive 0.1.0 linux {} hydo-linux-0.1.0.tar.gz\nfile 0.1.0 linux {k} guest/bzImage\n",
        sha256_bytes(a010)
    );
    let own = hydo_image::kernels::Known {
        arch: "x86_64",
        role: hydo_image::kernels::Role::Kernel,
        file: "bzImage",
        sha256: Box::leak(k.into_boxed_str()),
        version: "6.6.35",
    };
    Index::parse(&text).unwrap().with_own("0.2.0", &[own])
}

struct Fixture {
    server: Server,
    a010: Vec<u8>,
    a020: Vec<u8>,
    dir: PathBuf,
}

fn fixture() -> Fixture {
    let server = Server::start();
    let a010 = archive("0.1.0", &[]);
    let a020 = archive("0.2.0", &[("libexec/tool", b"a tool", 0o755)]);
    server.file("/releases/v0.1.0/hydo-linux-0.1.0.tar.gz", &a010);
    server.file("/releases/v0.2.0/hydo-linux-0.2.0.tar.gz", &a020);
    server.file(
        "/releases/v0.2.0/SHA256SUMS",
        format!(
            "{}  hydo-linux-0.2.0.tar.gz\n{}  hydo-linux-0.2.0-sources.tar.gz\n",
            sha256_bytes(&a020),
            "c".repeat(64)
        )
        .as_bytes(),
    );
    Fixture {
        server,
        a010,
        a020,
        dir: scratch(),
    }
}

impl Fixture {
    /// A cache in `<dir>/<home>`, online through the local server (three attempts, no pauses)
    /// or offline.
    fn cache(&self, home: &str, online: bool) -> Cache {
        let cfg = hydo_config::run::Cache {
            home: self.dir.join(home),
            release_url: self.server.base(),
            release_api: String::new(),
            offline: !online,
        };
        let opts = Options {
            base: self.server.base(),
            proxy: None,
            attempts: 3,
            pause: Duration::ZERO,
            verbose: false,
        };
        let mut c = Cache::new(&cfg, "linux", !online, Some(opts));
        c.index = index(&self.a010);
        c.own_version = "0.2.0".into();
        c
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        remove_tree(&self.dir);
    }
}

/// A machine that can run the release and needs no signing.
struct Host0;

impl Host for Host0 {
    fn unsupported(&self, _: Platform) -> Vec<String> {
        Vec::new()
    }
    fn quarantined(&self, _: &Path) -> bool {
        false
    }
    fn unquarantine(&self, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn sign(&self, _: &Path, _: &Path) -> Result<(), String> {
        Ok(())
    }
    fn signed_ok(&self, _: &Path) -> bool {
        true
    }
    fn untar(&self, _: &Path, _: &Path) -> Result<(), String> {
        Ok(())
    }
}

fn store_entries(c: &Cache) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(c.home.store())
        .map(|rd| {
            rd.flatten()
                .flat_map(|e| {
                    std::fs::read_dir(e.path())
                        .unwrap()
                        .flatten()
                        .collect::<Vec<_>>()
                })
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

// ------------------------------------------------------------------ the tests

#[test]
fn the_own_versions_archive_is_verified_by_the_releases_sums_and_fetched_once() {
    let f = fixture();
    let c = f.cache("h", true);
    let p = c.archive("0.2.0", "a test").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), f.a020);
    assert_eq!(p.file_name().unwrap(), "hydo-linux-0.2.0.tar.gz");
    assert_eq!(f.server.requests(), 2, "{:?}", f.server.log());
    assert!(c.home.release_sums("0.2.0").is_file());
    // again: from the store, no request
    assert_eq!(c.archive("0.2.0", "a test").unwrap(), p);
    assert_eq!(f.server.requests(), 2);
    // offline, the kept SHA256SUMS and the store are enough
    let off = f.cache("h", false);
    assert_eq!(off.archive("0.2.0", "a test").unwrap(), p);
    assert_eq!(f.server.requests(), 2);
    assert!(off.plan("0.2.0").unwrap().contains("in the install cache"));
    // no part file is left
    assert_eq!(std::fs::read_dir(c.home.downloads()).unwrap().count(), 0);
}

#[test]
fn a_pinned_archive_with_other_bytes_is_refused_and_nothing_is_kept() {
    let f = fixture();
    f.file(
        "/releases/v0.1.0/hydo-linux-0.1.0.tar.gz",
        b"not the archive",
    );
    let c = f.cache("h", true);
    let e = c.archive("0.1.0", "a test").unwrap_err();
    assert!(
        e.contains("refused") && e.contains(&sha256_bytes(&f.a010)),
        "{e}"
    );
    // a complete fresh download with the wrong bytes is not fetched again
    assert_eq!(f.server.requests(), 1);
    assert!(store_entries(&c).is_empty());
    assert_eq!(std::fs::read_dir(c.home.downloads()).unwrap().count(), 0);
}

impl Fixture {
    fn file(&self, path: &str, body: &[u8]) {
        self.server.file(path, body);
    }
}

#[test]
fn an_interrupted_download_resumes_with_a_range_request() {
    let f = fixture();
    let half = f.a010.len() / 2;
    f.server.put(
        "/releases/v0.1.0/hydo-linux-0.1.0.tar.gz",
        Route {
            body: f.a010.clone(),
            cut_first: Some((1, half)),
            ..Route::default()
        },
    );
    let c = f.cache("h", true);
    let p = c.archive("0.1.0", "a test").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), f.a010);
    let log = f.server.log();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0].1, None);
    assert_eq!(log[1].1.as_deref(), Some(format!("bytes={half}-").as_str()));
}

#[test]
fn a_server_that_ignores_the_range_sends_the_whole_file_again() {
    let f = fixture();
    f.server.put(
        "/releases/v0.1.0/hydo-linux-0.1.0.tar.gz",
        Route {
            body: f.a010.clone(),
            cut_first: Some((1, 100)),
            ignore_range: true,
            ..Route::default()
        },
    );
    let c = f.cache("h", true);
    let p = c.archive("0.1.0", "a test").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), f.a010);
    assert_eq!(f.server.requests(), 2);
}

#[test]
fn a_download_that_is_always_cut_fails_after_the_attempts() {
    let f = fixture();
    f.server.put(
        "/releases/v0.1.0/hydo-linux-0.1.0.tar.gz",
        Route {
            body: f.a010.clone(),
            cut_first: Some((99, 10)),
            ignore_range: true,
            ..Route::default()
        },
    );
    let c = f.cache("h", true);
    let e = c.archive("0.1.0", "a test").unwrap_err();
    assert!(e.contains("after 3 attempts"), "{e}");
    assert_eq!(f.server.requests(), 3);
    assert!(store_entries(&c).is_empty());
}

#[test]
fn server_errors_are_retried_and_a_missing_file_is_not() {
    let f = fixture();
    f.server.put(
        "/releases/v0.1.0/hydo-linux-0.1.0.tar.gz",
        Route {
            body: f.a010.clone(),
            fail_first: Some((2, 503)),
            ..Route::default()
        },
    );
    let c = f.cache("h", true);
    c.archive("0.1.0", "a test").unwrap();
    assert_eq!(f.server.requests(), 3);
    // 0.2.0's SHA256SUMS gone: one request, no retry
    let f2 = fixture();
    f2.server
        .state
        .lock()
        .unwrap()
        .routes
        .remove("/releases/v0.2.0/SHA256SUMS");
    let c2 = f2.cache("h", true);
    let e = c2.archive("0.2.0", "a test").unwrap_err();
    assert!(
        e.contains("HTTP 404") && e.contains("v0.2.0/SHA256SUMS"),
        "{e}"
    );
    assert_eq!(f2.server.requests(), 1);
}

#[test]
fn a_bad_part_kept_from_before_is_fetched_again_from_the_start() {
    let f = fixture();
    let c = f.cache("h", true);
    let sha = sha256_bytes(&f.a010);
    let part = c.home.part(&sha);
    std::fs::create_dir_all(part.parent().unwrap()).unwrap();
    std::fs::write(&part, vec![b'x'; 64]).unwrap();
    let p = c.archive("0.1.0", "a test").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), f.a010);
    let log = f.server.log();
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(log[0].1.as_deref(), Some("bytes=64-"));
    assert_eq!(log[1].1, None);
}

#[test]
fn a_kernel_is_taken_out_of_the_release_that_shipped_it() {
    let f = fixture();
    let c = f.cache("h", true);
    let k = FileRef {
        file: "bzImage".into(),
        sha256: sha256_bytes(KERNEL),
    };
    let p = c.obtain_kernel(&k).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), KERNEL);
    assert_eq!(p.file_name().unwrap(), "bzImage");
    // the newest release that shipped it: 0.2.0 (this hydo's own; its archive by SHA256SUMS)
    assert!(f
        .server
        .log()
        .iter()
        .any(|(p, _)| p.ends_with("hydo-linux-0.2.0.tar.gz")));
    let n = f.server.requests();
    assert_eq!(c.obtain_kernel(&k).unwrap(), p);
    assert_eq!(f.server.requests(), n);
    // a sha256 no release shipped
    let e = c
        .obtain_kernel(&FileRef {
            file: "bzImage".into(),
            sha256: "d".repeat(64),
        })
        .unwrap_err();
    assert!(e.contains("no hydo release this hydo knows"), "{e}");
    // offline with an empty cache: the URL to fetch by hand
    let off = f.cache("empty", false);
    let e = off.obtain_kernel(&k).unwrap_err();
    assert!(
        e.contains("offline")
            && e.contains("/v0.2.0/hydo-linux-0.2.0.tar.gz")
            && e.contains("--archive"),
        "{e}"
    );
    // through hydo_image's lookup: the refusal says why the cache could not help
    let img = f.dir.join("explore-ob-x-disk.img");
    let m = hydo_image::kernels::missing_message_why(&img, "kernel", &k, "6.6.35", &[], &e);
    assert!(m.contains("The install cache could not provide it: ") && m.contains("offline"));
}

#[test]
fn two_versions_share_the_cache_and_an_installed_version_needs_no_network() {
    let f = fixture();
    let c = f.cache("h", true);
    let done = c.install("0.1.0", &[], &Host0, Platform::Linux).unwrap();
    assert!(
        done[0].starts_with("unpacked hydo-linux-0.1.0.tar.gz"),
        "{done:?}"
    );
    let v1 = c.home.version("0.1.0");
    assert_eq!(
        std::fs::read_to_string(v1.join("VERSION")).unwrap(),
        "0.1.0\n"
    );
    let link = c.home.bin().join("hydo");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        Path::new("../versions/0.1.0/bin/hydo")
    );
    assert_eq!(
        std::fs::read_to_string(&link).unwrap(),
        "#!hydo 0.1.0\n",
        "the link resolves"
    );
    let mode = std::fs::metadata(v1.join("bin/hydo"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    c.install("0.2.0", &[], &Host0, Platform::Linux).unwrap();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        Path::new("../versions/0.2.0/bin/hydo")
    );
    // one kernel entry for both versions, next to the two archives
    assert_eq!(
        store_entries(&c),
        [
            "bzImage",
            "hydo-linux-0.1.0.tar.gz",
            "hydo-linux-0.2.0.tar.gz"
        ]
    );
    // an intact installed version: nothing fetched, nothing unpacked, offline
    let n = f.server.requests();
    let off = f.cache("h", false);
    let done = off.install("0.1.0", &[], &Host0, Platform::Linux).unwrap();
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(f.server.requests(), n);
    // a damaged one is unpacked again from the archive in the store, still offline
    std::fs::remove_file(v1.join("README.md")).unwrap();
    let done = off.install("0.1.0", &[], &Host0, Platform::Linux).unwrap();
    assert!(done[0].starts_with("unpacked"), "{done:?}");
    assert!(v1.join("README.md").is_file());
    assert_eq!(f.server.requests(), n);
    // a version this hydo does not know is refused before anything is fetched
    let e = c
        .install("0.3.0", &[], &Host0, Platform::Linux)
        .unwrap_err();
    assert!(e[0].contains("does not know the release 0.3.0"), "{e:?}");
    assert_eq!(f.server.requests(), n);
    // offline and empty: refused with the URL
    let e = f
        .cache("empty", false)
        .install("0.1.0", &[], &Host0, Platform::Linux)
        .unwrap_err();
    assert!(
        e[0].contains("offline") && e[0].contains("v0.1.0/hydo-linux-0.1.0.tar.gz"),
        "{e:?}"
    );
}

#[test]
fn a_release_installed_into_the_same_directory_keeps_its_program() {
    // install.sh's default prefix is ~/.hydo too: its bin/hydo is a program, not the link
    let f = fixture();
    let c = f.cache("h", true);
    std::fs::create_dir_all(c.home.bin()).unwrap();
    std::fs::write(c.home.bin().join("hydo"), b"#!the flat install\n").unwrap();
    std::fs::write(c.home.root.join("VERSION"), b"0.0.9\n").unwrap();
    let done = c.install("0.1.0", &[], &Host0, Platform::Linux).unwrap();
    let last = done.last().unwrap();
    assert!(
        last.starts_with("left ")
            && last.contains("holds hydo 0.0.9")
            && last.contains("versions/0.1.0/bin/hydo"),
        "{done:?}"
    );
    assert_eq!(
        std::fs::read(c.home.bin().join("hydo")).unwrap(),
        b"#!the flat install\n"
    );
    assert_eq!(c.program("0.1.0"), c.home.version("0.1.0").join("bin/hydo"));
    // without it, the link
    std::fs::remove_file(c.home.bin().join("hydo")).unwrap();
    c.install("0.1.0", &[], &Host0, Platform::Linux).unwrap();
    assert_eq!(c.program("0.1.0"), c.home.bin().join("hydo"));
}

#[test]
fn setup_restores_a_damaged_release_from_its_archive_and_check_says_so() {
    let f = fixture();
    let c = f.cache("h", true);
    c.install("0.2.0", &[], &Host0, Platform::Linux).unwrap();
    let root = c.home.version("0.2.0");
    let va = VersionAssets {
        cache: &c,
        version: "0.2.0".into(),
    };
    let (_, rc) = setup::check_lines(&root, Platform::Linux, &[], &Host0, false, Some(&va));
    assert_eq!(rc, 0);
    std::fs::write(root.join("libexec/tool"), b"changed").unwrap();
    std::fs::remove_file(root.join("guest/bzImage")).unwrap();
    let (lines, rc) = setup::check_lines(&root, Platform::Linux, &[], &Host0, false, Some(&va));
    assert_eq!(rc, 3, "{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains(
            "hydo setup would: restore 2 files from hydo-linux-0.2.0.tar.gz in the install cache"
        )),
        "{lines:?}"
    );
    // --verify checks the files only
    let (_, rc) = setup::check_lines(&root, Platform::Linux, &[], &Host0, true, Some(&va));
    assert_eq!(rc, 1);
    // without a source (M154's behaviour): damaged
    let (_, rc) = setup::check_lines(&root, Platform::Linux, &[], &Host0, false, None);
    assert_eq!(rc, 1);
    // offline with an empty cache: damaged, and the line says why
    let off = f.cache("empty", false);
    let va_off = VersionAssets {
        cache: &off,
        version: "0.2.0".into(),
    };
    let (lines, rc) = setup::check_lines(&root, Platform::Linux, &[], &Host0, false, Some(&va_off));
    assert_eq!(rc, 1);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("cannot restore") && l.contains("offline")),
        "{lines:?}"
    );
    let e = setup::setup_with(&root, Platform::Linux, &[], &Host0, Some(&va_off)).unwrap_err();
    assert!(e.last().unwrap().contains("could not restore"), "{e:?}");
    // restored, from the store, without a request
    let n = f.server.requests();
    let (after, done) = setup::setup_with(&root, Platform::Linux, &[], &Host0, Some(&va)).unwrap();
    assert!(after.ok());
    assert_eq!(f.server.requests(), n);
    assert!(
        done[0].starts_with("restore 2 files from hydo-linux-0.2.0.tar.gz: "),
        "{done:?}"
    );
    assert_eq!(std::fs::read(root.join("libexec/tool")).unwrap(), b"a tool");
    let mode = std::fs::metadata(root.join("libexec/tool"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(std::fs::read(root.join("guest/bzImage")).unwrap(), KERNEL);
    let (_, rc) = setup::check_lines(&root, Platform::Linux, &[], &Host0, false, Some(&va));
    assert_eq!(rc, 0);
}

#[test]
fn a_downloaded_archive_is_put_in_by_hand_when_it_is_verified() {
    let f = fixture();
    let dl = f.dir.join("dl");
    std::fs::create_dir_all(&dl).unwrap();
    let off = f.cache("h", false);
    // 0.2.0: verified by the SHA256SUMS next to it
    let a = dl.join("hydo-linux-0.2.0.tar.gz");
    std::fs::write(&a, &f.a020).unwrap();
    let e = off.import(&a).unwrap_err();
    assert!(e.contains("SHA256SUMS must be next to it"), "{e}");
    std::fs::write(
        dl.join("SHA256SUMS"),
        format!("{}  hydo-linux-0.2.0.tar.gz\n", sha256_bytes(&f.a020)),
    )
    .unwrap();
    off.import(&a).unwrap();
    assert!(off.archive("0.2.0", "a test").is_ok());
    // 0.1.0: pinned in the index; other bytes are refused
    let b = dl.join("hydo-linux-0.1.0.tar.gz");
    std::fs::write(&b, b"not it").unwrap();
    let e = off.import(&b).unwrap_err();
    assert!(e.contains("refused"), "{e}");
    std::fs::write(&b, &f.a010).unwrap();
    off.import(&b).unwrap();
    off.install("0.1.0", &[], &Host0, Platform::Linux).unwrap();
    // names that are not this platform's release archives
    for n in [
        "hydo-mac-0.1.0.tar.gz",
        "hydo-linux-0.1.0-sources.tar.gz",
        "x.tar.gz",
    ] {
        let p = dl.join(n);
        std::fs::write(&p, b"x").unwrap();
        assert!(off.import(&p).is_err(), "{n}");
    }
    assert_eq!(f.server.requests(), 0);
}

#[test]
fn an_http_base_url_off_the_loopback_is_refused() {
    let cfg = hydo_config::run::Cache {
        home: scratch(),
        release_url: "http://example.org/releases".into(),
        release_api: String::new(),
        offline: false,
    };
    let c = Cache::new(&cfg, "linux", false, None);
    assert!(c.fetch.is_none());
    let e = c
        .obtain_kernel(&FileRef {
            file: "bzImage".into(),
            sha256: hydo_image::kernels::X86_KERNEL.sha256.into(),
        })
        .unwrap_err();
    assert!(e.contains("not https://"), "{e}");
    remove_tree(&cfg.home);
}

#[test]
fn moving_the_bin_link_to_another_version_is_said_first() {
    let cfg = hydo_config::run::Cache {
        home: scratch(),
        release_url: String::new(),
        release_api: String::new(),
        offline: true,
    };
    let c = Cache::new(&cfg, "linux", true, None);
    // no link yet: nothing to say
    assert_eq!(c.repoint_note("0.3.3"), None);
    std::fs::create_dir_all(cfg.home.join("bin")).unwrap();
    std::os::unix::fs::symlink("../versions/0.3.1/bin/hydo", cfg.home.join("bin/hydo")).unwrap();
    assert_eq!(c.repoint_note("0.3.1"), None);
    let n = c.repoint_note("0.3.3").expect("another version");
    assert!(
        n.contains("at hydo 0.3.3; it pointed at hydo 0.3.1")
            && n.contains("`hydo setup --get 0.3.1` points it back"),
        "{n}"
    );
    remove_tree(&cfg.home);
}
