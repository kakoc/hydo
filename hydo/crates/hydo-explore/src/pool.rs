//! The worker pool: engine workers (each boots the guest once and runs every schedule from its
//! snapshot, [`hydo_engine::Worker`]) fed from one queue.
//!
//! What explore.py did, kept: a worker that loses its engine mid-run starts a new one and runs the
//! same schedule again (runs are deterministic, so nothing is lost); `$HYDO_QUIET_FILE` holds back
//! every new VM while the file exists (checked every 30 s, at most 2 hours); `$HYDO_EXPLORE_CACHE`
//! answers first runs (`seed-`, `fctrace-` tags) of the same schedule, image and engine options
//! from an earlier sweep's results and consoles; each run's history checks and error-log analysis
//! are applied as it finishes.
//!
//! What changed (docs/PERF-RUN-COST.md): no batch barriers. Jobs are submitted in batches, but a
//! worker takes the next job as soon as it is free, whichever batch it belongs to; re-runs and
//! shrink candidates go to the front of the queue, first runs to the back, so the shrinking of a
//! failure found early overlaps the rest of the sweep instead of waiting for it.

use crate::errlog::{self, Known};
use crate::py::{dumps_line, fl, obj, py_str, repr};
use hydo_engine::{BootSpec, Engine, EngineError, Worker};
use hydo_schedule::Schedule;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub type Res = Map<String, Value>;
/// (tag, schedule text, result)
pub type Done = (String, String, Res);

/// A BRANCH request's answer (explore.py Worker.branch): the branch point and its runs.
#[derive(Clone, Debug, Default)]
pub struct BranchDone {
    pub tjlo: Option<u64>,
    pub held: Option<u64>,
    /// (sub tag, the run as a plain schedule, result)
    pub runs: Vec<(String, String, Res)>,
}

enum Kind {
    Run,
    Branch(hydo_engine::BranchRequest, String),
}

struct Job {
    tag: String,
    text: String,
    kind: Kind,
    reply: Sender<(Done, Option<BranchDone>)>,
    /// Set when the batch no longer needs the job: a worker drops it unrun.
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Queue {
    high: VecDeque<Job>,
    low: VecDeque<Job>,
    closed: bool,
    alive: usize,
}

/// Where the per-run host steps (history checks, error logs) find their inputs.
#[derive(Clone)]
pub struct Host {
    pub outdir: PathBuf,
    pub onboard: PathBuf,
    pub known: Option<Known>,
}

pub struct Pool {
    q: Arc<(Mutex<Queue>, Condvar)>,
    threads: Vec<std::thread::JoinHandle<()>>,
    pub boot_secs: Arc<Mutex<Vec<Option<f64>>>>,
    pub host: Host,
    cache: Mutex<CacheState>,
    pub cache_hits: Mutex<usize>,
    /// The cache key's image and engine options.
    pub cache_image: String,
    pub cache_engine: String,
}

#[derive(Default)]
struct CacheState {
    map: HashMap<(String, String, String), Value>,
    pos: u64,
}

// ---------------------------------------------------------------- the quiet window
static QUIET: Mutex<()> = Mutex::new(());
static QUIET_WAITED: Mutex<f64> = Mutex::new(0.0);
const QUIET_MAX: Duration = Duration::from_secs(7200);

/// Seconds the pool waited for quiet windows (the summary's quiet_wait_s).
pub fn quiet_waited() -> f64 {
    *QUIET_WAITED.lock().expect("lock")
}

/// While `$HYDO_QUIET_FILE` exists start no new VM: wait, checking every 30 s, at most 2 hours.
pub fn quiet_wait() {
    let Some(f) = std::env::var_os("HYDO_QUIET_FILE") else {
        return;
    };
    let f = PathBuf::from(f);
    if !f.exists() {
        return;
    }
    let _g = QUIET.lock().expect("lock");
    let t0 = Instant::now();
    if f.exists() {
        eprintln!("quiet window ({}): waiting", f.display());
    }
    while f.exists() && t0.elapsed() < QUIET_MAX {
        std::thread::sleep(Duration::from_secs(30));
    }
    *QUIET_WAITED.lock().expect("lock") += t0.elapsed().as_secs_f64();
}

// ---------------------------------------------------------------- RESULT lines as explore.py read them
fn result_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // explore.py accepted extras starting tj|xt|pct|qn; hvx_explore also prints qbn= with bursts
    // (qb=), which made every --bursts run an "ERROR worker" there: qbn is accepted here.
    R.get_or_init(|| {
        Regex::new(r"^RESULT (\S+) fp=(\S+) wfp=(\S+) secs=(\S+)((?: (?:tj|xt|pct|qbn|qn)\S+)*) verdict=(.*)$")
            .expect("pattern")
    })
}

fn branch_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^RESULT (\S+) fp=(\S+) wfp=(\S+) secs=(\S+)((?: (?:tj|pct|qbn|qn)\S+)*) verdict=(.*)$")
            .expect("pattern")
    })
}

/// A worker's answer that is not a RESULT line: explore.py's "ERROR worker: <repr(line)>".
pub fn error_result(raw: &str, worker: usize) -> Res {
    let mut r = obj();
    r.insert("fp".into(), json!("0"));
    r.insert("secs".into(), json!(0));
    r.insert(
        "verdict".into(),
        json!(format!("ERROR worker: {}", repr(raw))),
    );
    r.insert("worker".into(), json!(worker));
    r
}

fn extras(r: &mut Res, ex: &str, xt: bool) {
    static TK: OnceLock<Regex> = OnceLock::new();
    static PF: OnceLock<Regex> = OnceLock::new();
    static XN: OnceLock<Regex> = OnceLock::new();
    static QN: OnceLock<Regex> = OnceLock::new();
    let tk = TK.get_or_init(|| Regex::new(r"tjk=(\d+)-(\d+) tjn=(\d+)").expect("p"));
    if let Some(c) = tk.captures(ex) {
        r.insert("tjk".into(), json!([num(&c[1]), num(&c[2])]));
        r.insert("tjn".into(), json!(num(&c[3])));
    }
    if !xt {
        return;
    }
    let pf = PF.get_or_init(|| Regex::new(r"pctf=(\d+) pctw=(\d+)").expect("p"));
    if let Some(c) = pf.captures(ex) {
        r.insert("pctf".into(), json!(num(&c[1])));
        r.insert("pctw".into(), json!(num(&c[2])));
    }
    let xn = XN.get_or_init(|| Regex::new(r"xtn=(\d+)").expect("p"));
    if let Some(c) = xn.captures(ex) {
        r.insert("xtn".into(), json!(num(&c[1])));
    }
    let qn = QN.get_or_init(|| Regex::new(r"qn=(\d+)").expect("p"));
    if let Some(c) = qn.captures(ex) {
        r.insert("qn".into(), json!(num(&c[1])));
    }
}

fn num(s: &str) -> Value {
    s.parse::<u64>().map(Value::from).unwrap_or(Value::Null)
}

/// explore.py `Worker.parse_result`: a RESULT line -> the result dict.
pub fn parse_result(line: &str, worker: usize) -> Res {
    let Some(c) = result_re().captures(line.trim()) else {
        return error_result(line, worker);
    };
    let mut r = obj();
    r.insert("fp".into(), json!(&c[2]));
    r.insert("wfp".into(), json!(&c[3]));
    r.insert("secs".into(), fl(c[4].parse::<f64>().unwrap_or(0.0)));
    r.insert("verdict".into(), json!(&c[6]));
    r.insert("worker".into(), json!(worker));
    extras(&mut r, &c[5], true);
    r
}

// ---------------------------------------------------------------- consoles
/// A console as Python's text mode reads it: lossy UTF-8, `\r\n` and `\r` as `\n`.
pub fn read_text(p: &Path) -> Option<String> {
    let b = std::fs::read(p).ok()?;
    Some(
        String::from_utf8_lossy(&b)
            .replace("\r\n", "\n")
            .replace('\r', "\n"),
    )
}

pub fn console(outdir: &Path, tag: &str, suffix: &str) -> PathBuf {
    outdir.join("consoles").join(format!("{tag}{suffix}.log"))
}

/// explore.py `host_checks`: the history checks (hydo-history) of a run whose console declares
/// them (HYDO-CHECK); their records and report go next to the console, and the combined verdict
/// replaces the guest's (kept as `guest_verdict`).
pub fn host_checks(h: &Host, tag: &str, res: &mut Res) {
    let d = h.outdir.join("consoles");
    let srcs = hydo_history::console_sources(&d, tag);
    if !hydo_history::has_checks(&srcs) {
        return;
    }
    let verdict = res.get("verdict").cloned().unwrap_or(Value::Null);
    let r =
        match hydo_history::run_checks(&srcs, verdict.as_str(), hydo_history::DEFAULT_BUDGET, None)
        {
            Ok(r) => r,
            Err(e) => {
                // where hydohist.py raised: explore.py stopped with a traceback
                eprintln!("hydo-explore: history checks of {tag}: {e}");
                std::process::exit(1);
            }
        };
    let _ = hydo_history::write_checks_files(&d, tag, &r);
    res.insert("guest_verdict".into(), verdict);
    res.insert("verdict".into(), json!(r.verdict));
}

/// explore.py `host_errlog`: the error-log analysis of a run whose console carries records.
pub fn host_errlog(h: &Host, tag: &str, text: &str, res: &mut Res) {
    let Some(known) = &h.known else { return };
    if res
        .get("verdict")
        .map(py_str)
        .unwrap_or_default()
        .starts_with("ERROR worker")
    {
        return;
    }
    let consoles: Vec<String> = ["", ".recovery"]
        .iter()
        .filter_map(|s| read_text(&console(&h.outdir, tag, s)))
        .collect();
    let prefix = h.outdir.join("consoles").join(tag);
    match known.analyse(&h.onboard, &prefix, text, &consoles) {
        Ok(Some(an)) => {
            let _ = std::fs::write(
                h.outdir.join("consoles").join(format!("{tag}.errors.json")),
                crate::py::dumps(&an, 1),
            );
            errlog::apply(res, &an, known);
        }
        Ok(None) => {}
        Err(e) => eprintln!("hydo-explore: error logs of {tag}: {e}"),
    }
}

pub fn errlog_of(outdir: &Path, tag: &str) -> Option<Value> {
    let t =
        std::fs::read_to_string(outdir.join("consoles").join(format!("{tag}.errors.json"))).ok()?;
    serde_json::from_str(&t).ok()
}

// ---------------------------------------------------------------- the pool
pub const CACHED_TAGS: [&str; 2] = ["seed-", "fctrace-"];

fn cache_file() -> Option<PathBuf> {
    std::env::var_os("HYDO_EXPLORE_CACHE").map(PathBuf::from)
}

impl Pool {
    /// Start `n` workers of `engine` for `spec`; each boots in its own thread.
    pub fn new(
        engine: Arc<dyn Engine + Send + Sync>,
        spec: BootSpec,
        n: usize,
        host: Host,
        cache_image: String,
        cache_engine: String,
    ) -> Pool {
        let _ = std::fs::create_dir_all(host.outdir.join("consoles"));
        let q = Arc::new((
            Mutex::new(Queue {
                alive: n,
                ..Queue::default()
            }),
            Condvar::new(),
        ));
        let boot_secs = Arc::new(Mutex::new(vec![None; n]));
        let mut threads = Vec::new();
        for i in 0..n {
            let (q2, eng, sp, h, bs) = (
                q.clone(),
                engine.clone(),
                spec.clone(),
                host.clone(),
                boot_secs.clone(),
            );
            threads.push(std::thread::spawn(move || {
                worker_main(i, q2, eng, sp, h, bs)
            }));
        }
        let p = Pool {
            q,
            threads,
            boot_secs,
            host,
            cache: Mutex::new(CacheState::default()),
            cache_hits: Mutex::new(0),
            cache_image,
            cache_engine,
        };
        p.cache_load();
        p
    }

    pub fn workers(&self) -> usize {
        self.threads.len()
    }

    fn push(&self, job: Job, high: bool) {
        let (m, cv) = &*self.q;
        let mut q = m.lock().expect("lock");
        if q.alive == 0 {
            drop(q);
            let raw = "";
            let res = error_result(raw, 0);
            let _ = job.reply.send(((job.tag, job.text, res), None));
            return;
        }
        if high {
            q.high.push_back(job);
        } else {
            q.low.push_back(job);
        }
        cv.notify_one();
    }

    fn cache_load(&self) {
        let Some(f) = cache_file() else { return };
        let Ok(mut fh) = std::fs::File::open(&f) else {
            return;
        };
        use std::io::{Read, Seek, SeekFrom};
        let mut st = self.cache.lock().expect("lock");
        if fh.seek(SeekFrom::Start(st.pos)).is_err() {
            return;
        }
        let mut text = String::new();
        let _ = fh.read_to_string(&mut text);
        for line in text.split_inclusive('\n') {
            if !line.ends_with('\n') {
                break;
            }
            st.pos += line.len() as u64;
            if let Ok(rec) = serde_json::from_str::<Value>(line) {
                let k = (
                    py_str(&rec["image"]),
                    py_str(&rec["engine"]),
                    py_str(&rec["text"]),
                );
                st.map.insert(k, rec);
            }
        }
    }

    fn cache_key(&self, text: &str) -> (String, String, String) {
        (
            self.cache_image.clone(),
            self.cache_engine.clone(),
            text.to_string(),
        )
    }

    fn cache_get(&self, tag: &str, text: &str) -> Option<Res> {
        cache_file()?;
        if !CACHED_TAGS.iter().any(|p| tag.starts_with(p)) {
            return None;
        }
        let rec = self
            .cache
            .lock()
            .expect("lock")
            .map
            .get(&self.cache_key(text))
            .cloned()?;
        let cons = rec["consoles"].as_object()?;
        if !cons.values().all(|c| Path::new(&py_str(c)).exists()) {
            return None;
        }
        for (suffix, c) in cons {
            let _ = std::fs::copy(py_str(c), console(&self.host.outdir, tag, suffix));
        }
        let mut r = rec["result"].as_object()?.clone();
        r.insert("cached".into(), json!(true));
        if r.contains_key("errors_new") {
            if let Some(v) = r.remove("pre_errlog_verdict") {
                r.insert("verdict".into(), v);
            }
            r.remove("errors_new");
            r.remove("errors_known");
            host_errlog(&self.host, tag, text, &mut r);
        }
        *self.cache_hits.lock().expect("lock") += 1;
        Some(r)
    }

    fn cache_put(&self, tag: &str, text: &str, res: &Res) {
        let Some(f) = cache_file() else { return };
        if !CACHED_TAGS.iter().any(|p| tag.starts_with(p))
            || py_str(res.get("verdict").unwrap_or(&Value::Null)).starts_with("ERROR worker")
        {
            return;
        }
        let d = PathBuf::from(format!("{}.d", f.display()));
        let _ = std::fs::create_dir_all(&d);
        let (image, engine, _) = self.cache_key(text);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|x| x.as_nanos())
            .unwrap_or(0);
        let h = {
            use sha2::{Digest, Sha256};
            let x = Sha256::digest(format!("{image}|{engine}|{text}|{nonce}").as_bytes());
            x.iter().map(|b| format!("{b:02x}")).collect::<String>()[..16].to_string()
        };
        let mut cons = Map::new();
        for suffix in ["", ".recovery", ".checks"] {
            let src = console(&self.host.outdir, tag, suffix);
            if src.exists() {
                let dst = d.join(format!("{h}{suffix}.log"));
                let _ = std::fs::copy(&src, &dst);
                cons.insert(suffix.into(), json!(dst.display().to_string()));
            }
        }
        let rec = json!({"image": image, "engine": engine, "text": text, "result": res, "consoles": cons});
        if let Ok(mut fh) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&f)
        {
            let _ = fh.write_all((dumps_line(&rec) + "\n").as_bytes());
        }
    }

    /// Submit items; their results arrive on the batch in completion order (cached ones first).
    pub fn batch(&self, items: &[(String, String)], high: bool) -> Batch<'_> {
        self.cache_load();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut ready = VecDeque::new();
        let mut pending = 0;
        for (tag, text) in items {
            if let Some(r) = self.cache_get(tag, text) {
                ready.push_back((tag.clone(), text.clone(), r));
                continue;
            }
            pending += 1;
            self.push(
                Job {
                    tag: tag.clone(),
                    text: text.clone(),
                    kind: Kind::Run,
                    reply: tx.clone(),
                    cancel: cancel.clone(),
                },
                high,
            );
        }
        Batch {
            pool: self,
            ready,
            rx,
            pending,
            cancel,
        }
    }

    /// explore.py `Pool.run_all`: items -> {tag: (text, result)} (waits for all of them).
    pub fn run_all(
        &self,
        items: &[(String, String)],
        high: bool,
    ) -> HashMap<String, (String, Res)> {
        let mut b = self.batch(items, high);
        let mut out = HashMap::new();
        while let Some((tag, text, r)) = b.next_done() {
            out.insert(tag, (text, r));
        }
        out
    }

    /// BRANCH requests (hvx only), run side by side; their answers in request order.
    pub fn branch_all(
        &self,
        reqs: Vec<(String, hydo_engine::BranchRequest, String)>,
    ) -> Vec<BranchDone> {
        let rxs: Vec<Receiver<(Done, Option<BranchDone>)>> = reqs
            .into_iter()
            .map(|(tag, req, sched)| {
                let (tx, rx) = channel();
                self.push(
                    Job {
                        tag,
                        text: sched.clone(),
                        kind: Kind::Branch(req, sched),
                        reply: tx,
                        cancel: Arc::new(AtomicBool::new(false)),
                    },
                    false,
                );
                rx
            })
            .collect();
        rxs.into_iter()
            .map(|rx| rx.recv().ok().and_then(|(_, b)| b).unwrap_or_default())
            .collect()
    }

    pub fn close(self) {
        {
            let (m, cv) = &*self.q;
            m.lock().expect("lock").closed = true;
            cv.notify_all();
        }
        for t in self.threads {
            let _ = t.join();
        }
    }

    pub fn mean_boot(&self) -> f64 {
        let b: Vec<f64> = self
            .boot_secs
            .lock()
            .expect("lock")
            .iter()
            .flatten()
            .copied()
            .collect();
        b.iter().sum::<f64>() / (b.len().max(1) as f64)
    }
}

pub struct Batch<'a> {
    pool: &'a Pool,
    ready: VecDeque<Done>,
    rx: Receiver<(Done, Option<BranchDone>)>,
    pending: usize,
    cancel: Arc<AtomicBool>,
}

impl Batch<'_> {
    /// The rest of this batch is not needed: queued jobs are dropped unrun (running ones finish).
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.pending = 0;
        self.ready.clear();
    }

    /// The next finished run of this batch, None when all are done.
    pub fn next_done(&mut self) -> Option<Done> {
        if let Some(d) = self.ready.pop_front() {
            return Some(d);
        }
        if self.pending == 0 {
            return None;
        }
        let ((tag, text, r), _) = self.rx.recv().ok()?;
        self.pending -= 1;
        self.pool.cache_put(&tag, &text, &r);
        Some((tag, text, r))
    }
}

fn take(q: &(Mutex<Queue>, Condvar)) -> Option<Job> {
    let (m, cv) = q;
    let mut g = m.lock().expect("lock");
    loop {
        if let Some(j) = g.high.pop_front().or_else(|| g.low.pop_front()) {
            return Some(j);
        }
        if g.closed {
            return None;
        }
        g = cv.wait(g).expect("lock");
    }
}

fn start(i: usize, engine: &dyn Engine, spec: &BootSpec, outdir: &Path) -> Option<Box<dyn Worker>> {
    quiet_wait();
    match engine.start(spec, &outdir.join(format!("worker-{i}.err"))) {
        Ok(w) => Some(w),
        Err(e) => {
            let line = match &e {
                EngineError::NotReady(l) => l.clone(),
                other => other.to_string(),
            };
            eprintln!("worker {i} failed to start: {}", repr(&line));
            None
        }
    }
}

/// A worker thread: boot, then run jobs until the queue closes.
fn worker_main(
    i: usize,
    q: Arc<(Mutex<Queue>, Condvar)>,
    engine: Arc<dyn Engine + Send + Sync>,
    spec: BootSpec,
    h: Host,
    boot: Arc<Mutex<Vec<Option<f64>>>>,
) {
    let mut w = start(i, engine.as_ref(), &spec, &h.outdir);
    if let Some(x) = &w {
        boot.lock().expect("lock")[i] = Some(x.boot_secs());
    }
    if w.is_none() {
        give_up(&q);
        return;
    }
    while let Some(job) = take(&q) {
        if job.cancel.load(Ordering::SeqCst) {
            continue;
        }
        quiet_wait();
        match job.kind {
            Kind::Branch(req, sched) => {
                let out = run_branch(w.as_mut().expect("a worker"), i, &h, &req, &sched);
                let _ = job.reply.send(((job.tag, job.text, obj()), Some(out)));
            }
            Kind::Run => {
                let mut res = None;
                for attempt in 0..2 {
                    let Some(wk) = w.as_mut() else { break };
                    let r = match Schedule::parse(&job.text) {
                        Ok(s) => wk.run(&job.tag, &s),
                        Err(e) => {
                            // a schedule the engines would refuse: no run (explore.py sent it and
                            // got the engine's own error verdict)
                            res = Some(parse_result(
                                &format!(
                                    "RESULT {} fp=0 wfp=0 secs=0 verdict=ERROR refused: {}",
                                    job.tag, e.message
                                ),
                                i,
                            ));
                            break;
                        }
                    };
                    match r {
                        Ok(rr) => {
                            res = Some(parse_result(&rr.line, i));
                            break;
                        }
                        Err(EngineError::Protocol(line)) if line != "the engine exited" => {
                            res = Some(parse_result(&line, i));
                            break;
                        }
                        Err(e) => {
                            if attempt == 1 {
                                break;
                            }
                            eprintln!(
                                "worker {i}: engine exited during {} ({e}); restarting it",
                                job.tag
                            );
                            if let Some(old) = w.take() {
                                let _ = old.close();
                            }
                            w = start(i, engine.as_ref(), &spec, &h.outdir);
                        }
                    }
                }
                let mut res = res.unwrap_or_else(|| error_result("", i));
                if !py_str(&res["verdict"]).starts_with("ERROR worker") {
                    host_checks(&h, &job.tag, &mut res);
                    host_errlog(&h, &job.tag, &job.text, &mut res);
                }
                let _ = job.reply.send(((job.tag, job.text, res), None));
                if w.is_none() {
                    give_up(&q);
                    return;
                }
            }
        }
    }
    if let Some(x) = w {
        let _ = x.close();
    }
}

/// This worker is gone: when it was the last one, answer every queued job with an error result.
fn give_up(q: &(Mutex<Queue>, Condvar)) {
    let (m, _) = q;
    let mut g = m.lock().expect("lock");
    g.alive -= 1;
    if g.alive > 0 {
        return;
    }
    let mut jobs: Vec<Job> = g.high.drain(..).collect();
    jobs.extend(g.low.drain(..));
    drop(g);
    for j in jobs {
        let _ = j.reply.send(((j.tag, j.text, error_result("", 0)), None));
    }
}

fn run_branch(
    w: &mut Box<dyn Worker>,
    i: usize,
    h: &Host,
    req: &hydo_engine::BranchRequest,
    sched: &str,
) -> BranchDone {
    let mut out = BranchDone::default();
    let Ok(o) = w.branch(req) else { return out };
    out.tjlo = o.tjlo;
    out.held = o.held;
    let (host, guest) = sched.split_once('|').unwrap_or((sched, ""));
    static VM: OnceLock<Regex> = OnceLock::new();
    let vm = VM.get_or_init(|| Regex::new(r"-v(\d+)$").expect("p"));
    for rr in o.runs {
        let Some(c) = branch_re().captures(rr.line.trim()) else {
            continue;
        };
        let sub = c[1].to_string();
        let mut res = obj();
        res.insert("fp".into(), json!(&c[2]));
        res.insert("wfp".into(), json!(&c[3]));
        res.insert("secs".into(), fl(c[4].parse::<f64>().unwrap_or(0.0)));
        res.insert("verdict".into(), json!(&c[6]));
        res.insert("worker".into(), json!(i));
        let st = match (vm.captures(&sub), out.tjlo.filter(|x| *x != 0)) {
            (Some(v), Some(lo)) => format!(
                "{} tj={:x} tjpm={} tjmax={} tjlo={} tjhi=0 rb={} |{}",
                host.trim_end(),
                v[1].parse::<u64>().unwrap_or(0),
                req.permille,
                req.max_delay,
                lo,
                crate::gen::BRANCH_BUDGET,
                guest
            ),
            _ => format!(
                "{} rb={} |{}",
                host.trim_end(),
                crate::gen::BRANCH_BUDGET,
                guest
            ),
        };
        extras(&mut res, &c[5], false);
        host_checks(h, &sub, &mut res);
        host_errlog(h, &sub, &st, &mut res);
        out.runs.push((sub, st, res));
    }
    out
}
