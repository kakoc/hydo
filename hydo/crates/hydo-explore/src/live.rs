//! The live map (Milestone 394, docs/design/live-map.md): a page on 127.0.0.1 that shows which
//! functions of the image's programs an exploration's runs reached, while it runs (`hydo
//! explore --live`) or after it (`hydo report --live <explore dir>`).
//!
//! The server reads only what the exploration writes: the map of the programs' code
//! (`function-map.json`, [`crate::funcs::write_map`]) and the run records of `events.jsonl`,
//! whose `functions` and `paths` say what each run reached. It reads the stream from where it
//! stopped the time before, and gives the page each first run in a short form. The page is one
//! file with its style and its script, and it loads nothing from outside; it asks for the runs
//! after the ones it has every 1.5 seconds.
//!
//! A run directory of `hydo run` has a `result.json` instead of a stream: the page then shows
//! that one run.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The default port (the milestone's number).
pub const PORT: u16 = 8394;

/// How many ports after the given one are tried when it is taken.
const TRIES: u16 = 20;

const PAGE: &str = include_str!("live/page.html");

/// What the server has read of the stream.
#[derive(Default)]
struct State {
    /// The bytes of `events.jsonl` read so far (up to a whole line).
    offset: u64,
    /// The first runs in the short form, in the order the stream has them.
    runs: Vec<Value>,
    /// The image and the engine of the start record, for the `hydo run` command.
    image: Option<String>,
    backend: Option<String>,
    /// The stream's end record came: the exploration finished.
    done: bool,
}

struct Server {
    dir: PathBuf,
    /// The functions that a baseline exploration (`--against`, one made with `--no-faults`)
    /// reached, read once: its folder, its first runs, and the functions.
    baseline: Option<(String, usize, Vec<u64>)>,
    state: Mutex<State>,
}

/// Serve the live map of the exploration in `dir` on 127.0.0.1 at `port` (0: any free port;
/// a port that is taken: the next of [`TRIES`]), in a thread of its own. Returns the page's
/// address.
pub fn start(dir: &Path, port: u16, against: Option<&Path>) -> Result<String, String> {
    let baseline = match against {
        Some(a) => Some(baseline(a)?),
        None => None,
    };
    let (listener, port) = bind(port)?;
    let srv = Arc::new(Server {
        dir: dir.to_path_buf(),
        baseline,
        state: Mutex::new(State::default()),
    });
    std::thread::Builder::new()
        .name("hydo-live".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let s = srv.clone();
                // one thread per connection: a page that holds its connection open does not
                // stop another
                let _ = std::thread::Builder::new()
                    .name("hydo-live-conn".into())
                    .spawn(move || s.handle(conn));
            }
        })
        .map_err(|e| format!("a thread for the page: {e}"))?;
    Ok(format!("http://127.0.0.1:{port}/"))
}

/// Serve the live map of a finished exploration (or a run directory) until the process is
/// stopped (`hydo report --live`).
pub fn serve_forever(dir: &Path, port: u16, against: Option<&Path>) -> Result<(), String> {
    let url = start(dir, port, against)?;
    println!("hydo report: the map of the code the runs reached is at {url} (Ctrl-C stops it)");
    loop {
        std::thread::park();
    }
}

/// The functions that the first runs of the exploration (or the run) in `dir` reached: its
/// folder, how many runs, and the functions' ids.
fn baseline(dir: &Path) -> Result<(String, usize, Vec<u64>), String> {
    let srv = Server {
        dir: dir.to_path_buf(),
        baseline: None,
        state: Mutex::new(State::default()),
    };
    let mut st = State::default();
    srv.read_more(&mut st);
    if st.runs.is_empty() {
        return Err(format!(
            "--against {}: no run there (an exploration's folder, or a run directory)",
            dir.display()
        ));
    }
    let mut ids: Vec<u64> = st
        .runs
        .iter()
        .filter_map(|r| r.get("functions").and_then(Value::as_array))
        .flatten()
        .filter_map(|x| x.get(0).and_then(Value::as_u64))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok((dir.display().to_string(), st.runs.len(), ids))
}

fn bind(port: u16) -> Result<(TcpListener, u16), String> {
    let ports: Vec<u16> = if port == 0 {
        vec![0]
    } else {
        (0..TRIES).filter_map(|k| port.checked_add(k)).collect()
    };
    let mut last = String::new();
    for p in ports {
        match TcpListener::bind(("127.0.0.1", p)) {
            Ok(l) => {
                let got = l.local_addr().map(|a| a.port()).unwrap_or(p);
                return Ok((l, got));
            }
            Err(e) => last = format!("127.0.0.1:{p}: {e}"),
        }
    }
    Err(format!(
        "no port to serve the page on ({last}); --port names another"
    ))
}

impl Server {
    fn handle(&self, mut c: TcpStream) {
        let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let mut r = BufReader::new(match c.try_clone() {
            Ok(x) => x,
            Err(_) => return,
        });
        let mut line = String::new();
        if r.read_line(&mut line).is_err() {
            return;
        }
        // the headers, up to the empty line
        loop {
            let mut h = String::new();
            match r.read_line(&mut h) {
                Ok(0) | Err(_) => break,
                Ok(_) if h.trim().is_empty() => break,
                Ok(_) => {}
            }
        }
        let mut w = line.split_whitespace();
        let (method, target) = (w.next().unwrap_or(""), w.next().unwrap_or("/"));
        if method != "GET" {
            return reply(&mut c, 405, "text/plain", b"only GET");
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        match path {
            "/" | "/index.html" => reply(&mut c, 200, "text/html; charset=utf-8", PAGE.as_bytes()),
            "/map.json" => match std::fs::read(self.dir.join(crate::funcs::MAP_FILE)) {
                Ok(b) => reply(&mut c, 200, "application/json", &b),
                Err(_) => reply(&mut c, 404, "application/json", b"null"),
            },
            "/runs" => {
                let from = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("from="))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let body = self.runs_after(from);
                reply(
                    &mut c,
                    200,
                    "application/json",
                    serde_json::to_string(&body)
                        .expect("runs serialize")
                        .as_bytes(),
                )
            }
            _ => reply(&mut c, 404, "text/plain", b"not found"),
        }
    }

    /// The first runs after the first `from`, with what the page needs besides.
    fn runs_after(&self, from: usize) -> Value {
        let mut st = self.state.lock().expect("live state");
        self.read_more(&mut st);
        let runs: Vec<Value> = st.runs.iter().skip(from).cloned().collect();
        let baseline = (from == 0)
            .then_some(())
            .and(self.baseline.as_ref())
            .map(|(dir, runs, fns)| json!({"dir": dir, "runs": runs, "functions": fns}));
        json!({
            "baseline": baseline,
            "dir": self.dir.display().to_string(),
            "image": st.image,
            "backend": st.backend,
            "done": st.done,
            "total": st.runs.len(),
            "from": from.min(st.runs.len()),
            "runs": runs,
        })
    }

    /// Read the stream's new whole lines (or, for a run directory, its `result.json` once).
    fn read_more(&self, st: &mut State) {
        let ev = self.dir.join(hydo_results::EVENTS);
        let Ok(mut f) = std::fs::File::open(&ev) else {
            if st.runs.is_empty() {
                if let Ok(t) = std::fs::read_to_string(self.dir.join("result.json")) {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if let Some(i) = v.pointer("/image/path").and_then(Value::as_str) {
                            st.image = Some(i.to_string());
                        }
                        if let Some(r) = short_run(&v, true) {
                            st.runs.push(r);
                        }
                        st.done = true;
                    }
                }
            }
            return;
        };
        if f.seek(SeekFrom::Start(st.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
            return;
        };
        st.offset += end as u64 + 1;
        for l in buf[..end].split(|&b| b == b'\n') {
            let Ok(v) = serde_json::from_slice::<Value>(l) else {
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("start") => {
                    st.image = v
                        .pointer("/image/path")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    st.backend = v
                        .pointer("/engine/backend")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
                Some("end") => st.done = true,
                Some("run") => {
                    if let Some(r) = short_run(&v, false) {
                        st.runs.push(r);
                    }
                }
                _ => {}
            }
        }
    }
}

/// A run record in the page's short form: only an exploration's first runs (role `first`, and
/// the one run of `--schedule`), or a run directory's run (`any`).
fn short_run(v: &Value, any: bool) -> Option<Value> {
    let role = v.get("role").and_then(Value::as_str).unwrap_or("first");
    if !any && role != hydo_results::role::FIRST && role != hydo_results::role::MANUAL {
        return None;
    }
    let schedule = v.get("schedule").and_then(Value::as_str).unwrap_or("");
    let fns = v.get("functions");
    // [function, step range's first step or -1, 1 when it ran before the first fault]
    let reached: Vec<Value> = fns
        .and_then(|f| f.get("reached"))
        .and_then(Value::as_array)
        .map(|a| {
            let ranges: Vec<(u64, u64)> = fns
                .and_then(|f| f.get("ranges"))
                .and_then(|r| serde_json::from_value(r.clone()).ok())
                .unwrap_or_default();
            a.iter()
                .filter_map(|x| {
                    let id = x.get("function")?.as_u64()?;
                    let step = x
                        .get("range")
                        .and_then(Value::as_u64)
                        .and_then(|i| ranges.get(i as usize))
                        .map_or(-1, |r| r.0 as i64);
                    let before = x.get("before_faults").and_then(Value::as_bool) == Some(true);
                    Some(json!([id, step, before as u8]))
                })
                .collect()
        })
        .unwrap_or_default();
    let paths: Vec<Value> = v
        .pointer("/paths/reached")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("place").cloned()).collect())
        .unwrap_or_default();
    Some(json!({
        "seed": v.get("seed"),
        "tag": v.get("tag").and_then(Value::as_str).unwrap_or(if any { "the run" } else { "" }),
        "schedule": schedule,
        "faults": crate::run::fault_kinds(schedule),
        "first_fault_step": fns.and_then(|f| f.get("first_fault_step")),
        "verdict": v.pointer("/verdict/kind"),
        "text": v.pointer("/verdict/text"),
        "fp": v.pointer("/fingerprints/fp"),
        "watched": fns.and_then(|f| f.get("watched")).and_then(Value::as_bool).unwrap_or(false),
        "functions": reached,
        "paths": paths,
    }))
}

fn reply(c: &mut TcpStream, code: u16, ty: &str, body: &[u8]) {
    let why = match code {
        200 => "OK",
        404 => "Not Found",
        _ => "Method Not Allowed",
    };
    let head = format!(
        "HTTP/1.1 {code} {why}\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCache-Control: \
         no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = c.write_all(head.as_bytes());
    let _ = c.write_all(body);
    let _ = c.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(url: &str, path: &str) -> String {
        let addr = url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out.split_once("\r\n\r\n").unwrap().1.to_string()
    }

    #[test]
    fn the_page_reads_the_runs_as_the_stream_grows() {
        let d = std::env::temp_dir().join(format!("hydo-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let ev = d.join(hydo_results::EVENTS);
        std::fs::write(
            &ev,
            "{\"type\":\"start\",\"image\":{\"path\":\"blnk.img\"},\"engine\":{\"backend\":\"hvm\"}}\n\
             {\"type\":\"run\",\"role\":\"first\",\"seed\":1,\"tag\":\"seed-1\",\"schedule\":\"disk=none | run K=db@3+1\",\
             \"verdict\":{\"kind\":\"ok\"},\"functions\":{\"functions\":3,\"watched\":true,\"ranges\":[[0,3],[3,4]],\
             \"first_fault_step\":3,\"reached\":[{\"function\":0,\"at\":5,\"range\":0,\"before_faults\":true},\
             {\"function\":2,\"at\":9,\"range\":1}]}}\n\
             {\"type\":\"run\",\"role\":\"shrink\",\"seed\":1,\"schedule\":\"x\",\"verdict\":{\"kind\":\"ok\"}}\n",
        )
        .unwrap();
        let url = start(&d, 0, None).unwrap();
        assert!(get(&url, "/").contains("<title>"));
        let v: Value = serde_json::from_str(&get(&url, "/runs?from=0")).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["image"], "blnk.img");
        assert_eq!(v["runs"][0]["functions"], json!([[0, 0, 1], [2, 3, 0]]));
        assert_eq!(v["runs"][0]["faults"], "K");
        assert_eq!(v["done"], false);
        // a line written after the first read, and the end
        let mut f = std::fs::OpenOptions::new().append(true).open(&ev).unwrap();
        writeln!(
            f,
            "{{\"type\":\"run\",\"role\":\"first\",\"seed\":2,\"schedule\":\"disk=none | run\",\"verdict\":{{\"kind\":\"violation\"}}}}\n{{\"type\":\"end\"}}"
        )
        .unwrap();
        let v: Value = serde_json::from_str(&get(&url, "/runs?from=1")).unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["runs"].as_array().unwrap().len(), 1);
        assert_eq!(v["runs"][0]["seed"], 2);
        assert_eq!(v["done"], true);
        assert_eq!(get(&url, "/map.json"), "null");
        let _ = std::fs::remove_dir_all(&d);
    }
}
