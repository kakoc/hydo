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
//!
//! `hydo report --live A B ...` (Milestone 397's third follow-up) plays several explorations or
//! run directories of one image in the given order, as one recording: the runs of B come after
//! the last run of A. The map is the first folder's. A notes file (`--notes`) names what the
//! page cannot know from the runs: which failure is a known or documented one, with its issue,
//! and which runs the recording leaves out.

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

/// The showcase page (Milestone 397, `--showcase`): the same records, for people who have never
/// seen hydo, on a dark page sized for a screen recording, with the map and the flow of calls
/// from the program's entry points.
const SHOWCASE: &str = include_str!("live/showcase.html");

/// How the page is served (Milestone 397). The default is Milestone 394's page.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    /// Serve the showcase page instead of the map page.
    pub showcase: bool,
    /// The program's entry points for the flow (`/entries.json`): `{"routes": [...]}`, each
    /// route of `hydo rules scan --kind writes --json` with its handler's file and line.
    pub entries: Option<Value>,
    /// The notes of the recording (`--notes FILE`, Milestone 397's third follow-up): the first
    /// note that matches a run says what the run's failure is, or leaves the run out.
    pub notes: Vec<Note>,
    /// More folders whose runs come after the first folder's, in this order (`hydo report
    /// --live A B ...`).
    pub more: Vec<PathBuf>,
}

/// One note of a recording's notes file (`hydo report --live --showcase --notes FILE`): what
/// the runs cannot say about a failure. A note matches a run when every part it names matches:
/// the check that the run's verdict names (`check`), a text in its schedule (`schedule`), its
/// seed (`seed`). The first note that matches a run applies.
#[derive(Clone, Debug, Default, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Note {
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub seed: Option<i64>,
    /// What the failure is: `new` (a bug that nobody had reported), `reported` (a bug that was
    /// new and is reported now), `known` (a bug reported before), or `documented` (behaviour
    /// that the project documents). The page names a documented one as a documented limit, and
    /// counts it as no bug.
    #[serde(default)]
    pub kind: Option<String>,
    /// The issue, `owner/repo#number`, which the page names.
    #[serde(default)]
    pub issue: Option<String>,
    /// What the banner says after its first words, instead of the check's promise: for a
    /// documented limit, the limit (`a kill of nsqd loses a message that a consumer holds`).
    #[serde(default)]
    pub words: Option<String>,
    /// The run is not played back, with the reason (`why`) for the reader of the notes file.
    #[serde(default)]
    pub leave_out: bool,
    #[serde(default)]
    pub why: Option<String>,
}

/// The kinds of [`Note::kind`].
pub const NOTE_KINDS: [&str; 4] = ["new", "reported", "known", "documented"];

/// Read a notes file: a list `notes:` of [`Note`]s.
pub fn read_notes(path: &Path) -> Result<Vec<Note>, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v = hydo_manifest::yaml::parse_yaml(&t, &path.display().to_string())
        .map_err(|e| e.to_string())?;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        #[serde(default)]
        notes: Vec<Note>,
    }
    let f: File = serde_json::from_value(v).map_err(|e| format!("{}: {e}", path.display()))?;
    for n in &f.notes {
        if let Some(k) = &n.kind {
            if !NOTE_KINDS.contains(&k.as_str()) {
                return Err(format!(
                    "{}: a note's kind is one of {}, not {k}",
                    path.display(),
                    NOTE_KINDS.join(", ")
                ));
            }
        }
        if n.check.is_none() && n.schedule.is_none() && n.seed.is_none() {
            return Err(format!(
                "{}: a note names a check, a text of the schedule or a seed",
                path.display()
            ));
        }
    }
    Ok(f.notes)
}

impl Note {
    fn matches(&self, r: &Value) -> bool {
        let check = r.pointer("/what/check").and_then(Value::as_str);
        self.check.as_deref().is_none_or(|c| check == Some(c))
            && self.schedule.as_deref().is_none_or(|t| {
                r.get("schedule")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.contains(t))
            })
            && self
                .seed
                .is_none_or(|n| r.get("seed").and_then(Value::as_i64) == Some(n))
    }
}

/// What the server has read of the stream.
#[derive(Default)]
struct State {
    /// The folder being read: 0 the first, then each of [`Opts::more`] once the one before it
    /// ended.
    part: usize,
    /// The bytes of the folder's `events.jsonl` read so far (up to a whole line).
    offset: u64,
    /// The image of the folder being read, for the runs' `hydo run` command.
    part_image: Option<String>,
    /// The runs that a note left out.
    left_out: usize,
    /// The folders and seeds whose first run a note left out: their re-runs and shrink
    /// candidates are left out with it.
    left_seeds: Vec<(usize, i64)>,
    /// Every other run of the stream (re-runs, shrink candidates, minimal schedules; Milestone
    /// 397's fourth follow-up, the tree of runs) in a short form without its functions.
    others: Vec<Value>,
    /// The first runs in the short form, in the order the stream has them.
    runs: Vec<Value>,
    /// The image and the engine of the start record, for the `hydo run` command.
    image: Option<String>,
    backend: Option<String>,
    /// The stream's end record came: the exploration finished (of the last folder, with
    /// several).
    done: bool,
}

struct Server {
    dir: PathBuf,
    /// The functions that a baseline exploration (`--against`, one made with `--no-faults`)
    /// reached, read once: its folder, its first runs, and the functions.
    baseline: Option<(String, usize, Vec<u64>)>,
    state: Mutex<State>,
    opts: Opts,
}

/// Serve the live map of the exploration in `dir` on 127.0.0.1 at `port` (0: any free port;
/// a port that is taken: the next of [`TRIES`]), in a thread of its own. Returns the page's
/// address.
pub fn start(dir: &Path, port: u16, against: Option<&Path>) -> Result<String, String> {
    start_with(dir, port, against, Opts::default())
}

/// [`start`] with the page's options (Milestone 397).
pub fn start_with(
    dir: &Path,
    port: u16,
    against: Option<&Path>,
    opts: Opts,
) -> Result<String, String> {
    let baseline = match against {
        Some(a) => Some(baseline(a)?),
        None => None,
    };
    let (listener, port) = bind(port)?;
    let srv = Arc::new(Server {
        dir: dir.to_path_buf(),
        baseline,
        state: Mutex::new(State::default()),
        opts,
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
    serve_forever_with(dir, port, against, Opts::default())
}

/// [`serve_forever`] with the page's options (Milestone 397).
pub fn serve_forever_with(
    dir: &Path,
    port: u16,
    against: Option<&Path>,
    opts: Opts,
) -> Result<(), String> {
    let url = start_with(dir, port, against, opts)?;
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
        opts: Opts::default(),
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
            "/" | "/index.html" => {
                let page = if self.opts.showcase { SHOWCASE } else { PAGE };
                reply(&mut c, 200, "text/html; charset=utf-8", page.as_bytes())
            }
            "/entries.json" => {
                let body = self.opts.entries.clone().unwrap_or(Value::Null);
                reply(
                    &mut c,
                    200,
                    "application/json",
                    serde_json::to_string(&body)
                        .expect("entries serialize")
                        .as_bytes(),
                )
            }
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
                let ofrom = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("ofrom="))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let body = self.runs_after(from, ofrom);
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
    fn runs_after(&self, from: usize, ofrom: usize) -> Value {
        let mut st = self.state.lock().expect("live state");
        self.read_more(&mut st);
        let runs: Vec<Value> = st.runs.iter().skip(from).cloned().collect();
        let others: Vec<Value> = st.others.iter().skip(ofrom).cloned().collect();
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
            "parts": 1 + self.opts.more.len(),
            "left_out": st.left_out,
            "total": st.runs.len(),
            "from": from.min(st.runs.len()),
            "runs": runs,
            "others_total": st.others.len(),
            "others": others,
        })
    }

    /// Read the new whole lines of the folder being read, and of the folders after it once it
    /// ended.
    fn read_more(&self, st: &mut State) {
        // the last folder ended: nothing more comes
        if st.done {
            return;
        }
        loop {
            let dir = if st.part == 0 {
                self.dir.clone()
            } else {
                match self.opts.more.get(st.part - 1) {
                    Some(d) => d.clone(),
                    None => return,
                }
            };
            let mut ended = false;
            self.read_part(&dir, st, &mut ended);
            if !ended || st.part >= self.opts.more.len() {
                st.done |= ended;
                return;
            }
            st.part += 1;
            st.offset = 0;
            st.part_image = None;
        }
    }

    /// A run of the folder being read, with its note, or none when a note leaves it out.
    fn add_run(&self, st: &mut State, mut r: Value) {
        let other = r.get("role").and_then(Value::as_str).is_some_and(|x| {
            x != hydo_results::role::FIRST && x != hydo_results::role::MANUAL && x != "the run"
        });
        let seed = r.get("seed").and_then(Value::as_i64);
        if other && seed.is_some_and(|s| st.left_seeds.contains(&(st.part, s))) {
            return;
        }
        if let Some(n) = self.opts.notes.iter().find(|n| n.matches(&r)) {
            if n.leave_out {
                if other {
                    return;
                }
                st.left_out += 1;
                if let Some(s) = seed {
                    st.left_seeds.push((st.part, s));
                }
                return;
            }
            r["note"] = json!({"kind": n.kind, "issue": n.issue, "words": n.words});
        }
        if let Some(i) = &st.part_image {
            r["image"] = json!(i);
        }
        r["part"] = json!(st.part);
        if other {
            st.others.push(r);
        } else {
            st.runs.push(r);
        }
    }

    /// Read the stream's new whole lines of `dir` (or, for a run directory, its `result.json`
    /// once); `ended` when its end came.
    fn read_part(&self, dir: &Path, st: &mut State, ended: &mut bool) {
        let ev = dir.join(hydo_results::EVENTS);
        let Ok(mut f) = std::fs::File::open(&ev) else {
            if let Ok(t) = std::fs::read_to_string(dir.join("result.json")) {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    if let Some(i) = v.pointer("/image/path").and_then(Value::as_str) {
                        if st.image.is_none() {
                            st.image = Some(i.to_string());
                        }
                        st.part_image = Some(i.to_string());
                    }
                    if let Some(r) = short_run(&v, true) {
                        self.add_run(st, r);
                    }
                    *ended = true;
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
                    let image = v
                        .pointer("/image/path")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if st.part == 0 {
                        st.image = image.clone();
                        st.backend = v
                            .pointer("/engine/backend")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    st.part_image = image;
                }
                Some("end") => *ended = true,
                Some("run") => {
                    if let Some(r) = short_run(&v, false) {
                        self.add_run(st, r);
                    } else if let Some(r) = other_run(&v) {
                        self.add_run(st, r);
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
    // [function, step range's first step or -1, 1 when it ran before the first fault, the
    // function that called it first or -1 (Milestone 397)]
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
                    let caller = x.get("caller").and_then(Value::as_i64).unwrap_or(-1);
                    Some(json!([id, step, before as u8, caller]))
                })
                .collect()
        })
        .unwrap_or_default();
    let paths: Vec<Value> = v
        .pointer("/paths/reached")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("place").cloned()).collect())
        .unwrap_or_default();
    // the schedule and the broken check in words, for the showcase (Milestone 397)
    let said: Vec<Value> = hydo_schedule::describe::describe_text(schedule, &[])
        .into_iter()
        .map(|x| {
            json!({
                "token": x.token,
                "sentence": x.sentence,
                "gloss": x.gloss,
                "step": x.step,
                "fault": x.part == hydo_schedule::describe::Part::Fault,
            })
        })
        .collect();
    let verdict_text = v
        .pointer("/verdict/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    let findings: Vec<hydo_results::Finding> = v
        .get("findings")
        .and_then(|f| serde_json::from_value(f.clone()).ok())
        .unwrap_or_default();
    let w = crate::advice::what_of(verdict_text, &findings);
    // how much attention the explorer's advice gives the failure: `look`, or `setting` and
    // `expected` for a setting that trades safety for speed and a fault that no system survives
    // (Milestone 397's third follow-up: the banner does not call those a broken promise)
    let attention = (v.pointer("/verdict/kind").and_then(Value::as_str) != Some("ok")).then(|| {
        let class = v.get("class").and_then(Value::as_str).unwrap_or("");
        let parsed = hydo_schedule::Schedule::parse(schedule).ok();
        crate::advice::problem(class, &w, "", parsed.as_ref(), &[])
            .attention
            .id()
    });
    let what = match w {
        crate::advice::What::Check {
            id,
            description,
            found,
        } => json!({"check": id, "promise": description, "found": found}),
        w => json!({"words": crate::advice::what_words(&w)}),
    };
    Some(json!({
        "role": role,
        "said": said,
        "what": what,
        "seed": v.get("seed"),
        "tag": v.get("tag").and_then(Value::as_str).unwrap_or(if any { "the run" } else { "" }),
        "schedule": schedule,
        "faults": crate::run::fault_kinds(schedule),
        "first_fault_step": fns.and_then(|f| f.get("first_fault_step")),
        // the end of the workload's last step range, to place each step on the run's track
        "steps": fns
            .and_then(|f| f.get("ranges"))
            .and_then(Value::as_array)
            .and_then(|r| r.last())
            .and_then(|r| r.get(1)),
        "verdict": v.pointer("/verdict/kind"),
        "attention": attention,
        "text": v.pointer("/verdict/text"),
        "fp": v.pointer("/fingerprints/fp"),
        "watched": fns.and_then(|f| f.get("watched")).and_then(Value::as_bool).unwrap_or(false),
        "functions": reached,
        "paths": paths,
    }))
}

/// A run record that is no first run (a re-run, a shrink candidate, a minimal schedule) in the
/// page's short form, for the tree of runs (Milestone 397's fourth follow-up): the first run's
/// form without the functions and the error paths, which only first runs carry to the page.
fn other_run(v: &Value) -> Option<Value> {
    let role = v.get("role").and_then(Value::as_str)?;
    if !matches!(
        role,
        hydo_results::role::RERUN
            | hydo_results::role::SHRINK
            | hydo_results::role::MINIMAL
            | hydo_results::role::ERROR_SHRINK
            | hydo_results::role::ERROR_MINIMAL
    ) {
        return None;
    }
    let mut r = short_run(v, true)?;
    if let Some(o) = r.as_object_mut() {
        o.remove("functions");
        o.remove("paths");
    }
    Some(r)
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
             {\"function\":2,\"at\":9,\"range\":1,\"caller\":0}]}}\n\
             {\"type\":\"run\",\"role\":\"shrink\",\"seed\":1,\"schedule\":\"x\",\"verdict\":{\"kind\":\"ok\"}}\n",
        )
        .unwrap();
        let url = start(&d, 0, None).unwrap();
        assert!(get(&url, "/").contains("<title>"));
        let v: Value = serde_json::from_str(&get(&url, "/runs?from=0")).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["image"], "blnk.img");
        assert_eq!(
            v["runs"][0]["functions"],
            json!([[0, 0, 1, -1], [2, 3, 0, 0]])
        );
        assert_eq!(v["runs"][0]["steps"], 4);
        assert_eq!(v["runs"][0]["said"][0]["token"], "K=db@3+1");
        assert_eq!(v["runs"][0]["said"][0]["fault"], true);
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
        assert_eq!(get(&url, "/entries.json"), "null");
        assert!(!get(&url, "/").contains("Breaking"));
        // Milestone 397: the showcase page, and the entry points of its flow
        let routes = json!({"routes": [{"method": "POST", "path": "/transactions"}]});
        let opts = Opts {
            showcase: true,
            entries: Some(routes.clone()),
            ..Default::default()
        };
        let url = start_with(&d, 0, None, opts).unwrap();
        assert!(get(&url, "/").contains("Breaking"));
        let e: Value = serde_json::from_str(&get(&url, "/entries.json")).unwrap();
        assert_eq!(e, routes);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn several_folders_play_in_order_with_their_notes() {
        let base = std::env::temp_dir().join(format!("hydo-live-parts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b) = (base.join("a"), base.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
        }
        let run_as = |role: &str, seed: u64, sch: &str, verdict: &str| {
            let kind = if verdict.is_empty() {
                "ok"
            } else {
                "violation"
            };
            format!(
                "{{\"type\":\"run\",\"role\":\"{role}\",\"seed\":{seed},\"schedule\":\"{sch}\",\
                 \"class\":\"process\",\"verdict\":{{\"kind\":\"{kind}\",\"text\":\"{verdict}\"}}}}\n"
            )
        };
        let run = |seed: u64, sch: &str, verdict: &str| run_as("first", seed, sch, verdict);
        std::fs::write(
            a.join(hydo_results::EVENTS),
            format!(
                "{{\"type\":\"start\",\"image\":{{\"path\":\"one.img\"}}}}\n{}{}{}{{\"type\":\"end\"}}\n",
                run(1, "disk=none | run", ""),
                run(2, "disk=none | run K=nsqd@4+1", "VIOLATION [jobs.no-loss] m0 lost"),
                run_as("rerun", 2, "disk=none | run K=nsqd@4+1", "VIOLATION [jobs.no-loss] m0 lost"),
            ),
        )
        .unwrap();
        std::fs::write(
            b.join(hydo_results::EVENTS),
            format!(
                "{{\"type\":\"start\",\"image\":{{\"path\":\"/x/one.img\"}}}}\n{}{}{}{{\"type\":\"end\"}}\n",
                run(1, "disk=none | run A=nsq.msg_received+2200:kill-nsqd", "VIOLATION [jobs.acked-outcome] m0 has no final state"),
                run(2, "disk=none | run P=nsqd@3+1", "VIOLATION [jobs.no-loss] m3 lost"),
                run_as("shrink", 2, "disk=none | run P=nsqd@3+0", "VIOLATION [jobs.no-loss] m3 lost"),
            ),
        )
        .unwrap();
        let notes_file = base.join("notes.yaml");
        std::fs::write(
            &notes_file,
            "notes:\n  - {check: jobs.no-loss, schedule: \"K=nsqd\", kind: reported, issue: \"nsqio/nsq#1542\"}\n  - check: jobs.acked-outcome\n    kind: documented\n    issue: \"nsqio/nsq#1386\"\n  - {schedule: \"P=nsqd\", leave_out: true, why: not traced}\n",
        )
        .unwrap();
        let notes = read_notes(&notes_file).unwrap();
        assert_eq!(notes.len(), 3);
        let opts = Opts {
            showcase: true,
            notes,
            more: vec![b.clone()],
            ..Default::default()
        };
        let url = start_with(&a, 0, None, opts).unwrap();
        let v: Value = serde_json::from_str(&get(&url, "/runs?from=0")).unwrap();
        assert_eq!(v["parts"], 2);
        assert_eq!(v["total"], 3);
        assert_eq!(v["left_out"], 1);
        assert_eq!(v["done"], true);
        assert_eq!(v["image"], "one.img");
        // a second page reads the same: the end stays, and no run comes twice
        let again: Value = serde_json::from_str(&get(&url, "/runs?from=0")).unwrap();
        assert_eq!(again["done"], true);
        assert_eq!(again["total"], 3);
        let r = v["runs"].as_array().unwrap();
        assert_eq!(r[0]["note"], Value::Null);
        assert_eq!(r[0]["attention"], Value::Null);
        assert_eq!(
            r[1]["note"],
            json!({"kind": "reported", "issue": "nsqio/nsq#1542", "words": null})
        );
        assert_eq!(r[1]["attention"], "look");
        assert_eq!(
            r[2]["schedule"],
            "disk=none | run A=nsq.msg_received+2200:kill-nsqd"
        );
        assert_eq!(r[2]["note"]["kind"], "documented");
        assert_eq!(r[2]["image"], "/x/one.img");
        // Milestone 397's fourth follow-up: the other runs, without the left-out seed's
        let o = v["others"].as_array().unwrap();
        assert_eq!(v["others_total"], 1);
        assert_eq!(o[0]["role"], "rerun");
        assert_eq!(o[0]["seed"], 2);
        assert_eq!(o[0]["part"], 0);
        assert_eq!(o[0]["note"]["issue"], "nsqio/nsq#1542");
        assert!(o[0].get("functions").is_none());
        assert_eq!(r[2]["part"], 1);
        // a kind that is none of the four, and a note that names nothing
        std::fs::write(&notes_file, "notes:\n  - {check: x, kind: bug}\n").unwrap();
        assert!(read_notes(&notes_file)
            .unwrap_err()
            .contains("kind is one of"));
        std::fs::write(&notes_file, "notes:\n  - {kind: known}\n").unwrap();
        assert!(read_notes(&notes_file).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
