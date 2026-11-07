//! Inspection sessions (docs/design/inspect.md "Sessions"): `hydo inspect start` starts this
//! program again in the background (`hydo inspect __serve <id>`), which replays the run to the
//! stop and holds it; later calls connect to its socket, send one request line and read one
//! answer line. A session lives in `$HYDO_HOME/inspect/<id>/` (`session.json`, `request.json`,
//! `server.log`, the socket, and the engine's own output in `engine/`) and ends by `stop`, or by
//! itself after `--idle` seconds without a call.

use super::drive::{Held, Machine, Recorded, Taker, SNAP};
use super::work::Work;
use super::{answer, err_doc, freeze_work, run_point, target_of, work_for, Args, Problem, Req};
use crate::ctx::Ctx;
use hydo_control::agent::AgentEngine;
use hydo_control::{Ending, Out, Seen, StopInfo};
use hydo_results::{Inspect, InspectSession, RunPoint};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A session's default idle time, seconds.
pub const IDLE_S: u64 = 900;
/// How long `start` waits for the replay, seconds (a stack run takes minutes).
pub const START_WAIT_S: u64 = 3600;
/// The most engine stops one call may take.
pub const CALL_STOPS: u64 = 200_000;

fn home(ctx: &Ctx) -> PathBuf {
    ctx.config.cli.cache.home.join("inspect")
}

fn dir_of(ctx: &Ctx, id: &str) -> PathBuf {
    home(ctx).join(id)
}

/// The socket: in the session directory when its path is short enough for a Unix socket,
/// else in the temporary directory.
fn sock_of(ctx: &Ctx, id: &str) -> PathBuf {
    let p = dir_of(ctx, id).join("sock");
    if p.as_os_str().len() < 100 {
        p
    } else {
        std::env::temp_dir().join(format!("hydo-inspect-{id}.sock"))
    }
}

fn new_id() -> String {
    use sha2::{Digest, Sha256};
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let h = Sha256::digest(format!("{t} {}", std::process::id()).as_bytes());
    format!(
        "i-{}",
        h[..4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn now_ms(t0: Instant) -> u64 {
    t0.elapsed().as_millis() as u64
}

fn write_json(p: &Path, v: &Value) {
    let tmp = p.with_extension("tmp");
    let _ = std::fs::write(
        &tmp,
        serde_json::to_string_pretty(v).unwrap_or_default() + "\n",
    );
    let _ = std::fs::rename(&tmp, p);
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

macro_rules! to_value {
    ($x:expr) => {
        serde_json::to_value($x).unwrap_or(Value::Null)
    };
}

/// The engine for a recorded run (this platform's; another engine's run is refused, since its
/// stops are that engine's).
pub fn machine(ctx: &Ctx, rec: &Recorded, outdir: &Path) -> Result<Machine, Problem> {
    let now = ctx.backend_name();
    if !rec.backend.is_empty() && rec.backend != now {
        return Err(Problem::new(
            "refused",
            format!(
                "the run was recorded by the {} engine and this hydo runs {now}: its stops are that \
                 engine's, so it is inspected only there",
                rec.backend
            ),
            if rec.backend == "kvm" || rec.backend == "hvx" {
                vec![format!("hydo inspect start {} --backend {}", rec.dir.display(), rec.backend)]
            } else {
                vec![]
            },
        ));
    }
    let _ = std::fs::create_dir_all(outdir);
    let (eng, spec) = crate::runs::controlled_engine(ctx, &rec.image, outdir, &rec.eargs, true);
    let a =
        AgentEngine::for_image(eng, &rec.image).map_err(|e| Problem::new("refused", e, vec![]))?;
    Ok(Machine {
        eng: a.inner.clone(),
        spec,
        plan: a.plan.clone(),
        cadence: a.cadence,
    })
}

// ---------------------------------------------------------------- calls served on a live branch

/// Where a live branch's calls come from, and where the answers go.
pub trait Calls {
    /// The next call (None: no more; the session ends).
    fn next(&mut self) -> Option<Req>;
    fn answer(&mut self, v: Value);
}

/// Serves calls inside the conversation, one after another (see the module documentation of
/// `drive`, "live").
pub struct LiveTaker<'c> {
    pub calls: &'c mut dyn Calls,
    pub plan: hydo_control::Plan,
    pub at: RunPoint,
    pub pre: Option<Work>,
    cur: Option<(Req, Work, Instant)>,
    k: u64,
    pub stop: Option<StopInfo>,
    pub served: u64,
    pub status: Value,
}

impl<'c> LiveTaker<'c> {
    pub fn new(
        calls: &'c mut dyn Calls,
        plan: hydo_control::Plan,
        at: RunPoint,
        pre: Option<Work>,
    ) -> LiveTaker<'c> {
        LiveTaker {
            calls,
            plan,
            at,
            pre,
            cur: None,
            k: 0,
            stop: None,
            served: 0,
            status: Value::Null,
        }
    }
}

impl Taker for LiveTaker<'_> {
    fn step(&mut self, stop: &StopInfo, seen: &[Seen], out: &mut Out) {
        self.stop = Some(*stop);
        let mut seen = seen;
        if let Some(w) = self.pre.as_mut() {
            w.step(seen, out);
            if !w.finished() {
                return;
            }
            self.pre = None;
            seen = &[];
        }
        loop {
            if let Some((req, w, t0)) = self.cur.as_mut() {
                w.step(seen, out);
                seen = &[];
                if w.stops >= CALL_STOPS && !w.finished() {
                    w.failed = Some(format!(
                        "no answer within {CALL_STOPS} engine stops (the guest may be stuck)"
                    ));
                }
                if !w.finished() {
                    return;
                }
                let v = match answer(req, w, &self.at, "live", now_ms(*t0)) {
                    Ok(a) => to_value!(&a),
                    Err(p) => err_doc(req.op(), &p),
                };
                self.served += 1;
                self.cur = None;
                self.calls.answer(v);
                continue;
            }
            match self.calls.next() {
                None => {
                    out.end(Ending::Verdict("HYDO-ORACLE: OK inspect".into()));
                    return;
                }
                Some(Req::Stop) => {
                    self.calls
                        .answer(json!({"v": 1, "type": "inspect", "op": "stop",
                        "notes": ["the session ended"]}));
                    out.end(Ending::Verdict("HYDO-ORACLE: OK inspect".into()));
                    return;
                }
                Some(Req::Status) => {
                    let s = self.status.clone();
                    self.calls.answer(s);
                }
                Some(req) => {
                    self.k += 1;
                    match work_for(&req, &self.plan, self.k) {
                        Ok(w) => self.cur = Some((req, w, Instant::now())),
                        Err(p) => self.calls.answer(err_doc(req.op(), &p)),
                    }
                }
            }
        }
    }
}

/// The calls of a one-shot inspection: one request, its answer kept.
struct One {
    req: Option<Req>,
    out: Option<Value>,
}

impl Calls for One {
    fn next(&mut self) -> Option<Req> {
        self.req.take()
    }
    fn answer(&mut self, v: Value) {
        self.out = Some(v);
    }
}

/// `hydo inspect <op> <run dir> [--at P] ...`: replay to the stop, do the one call there on the
/// live branch, end. Nothing is kept.
pub fn oneshot(ctx: &Ctx, dir: &Path, at: &str, req: Req) -> Result<Value, Problem> {
    let t0 = Instant::now();
    let rec = Recorded::load(dir)?;
    let target = target_of(&rec, at)?;
    let tmp = scratch(ctx, "oneshot");
    let m = machine(ctx, &rec, &tmp)?;
    let mut calls = One {
        req: Some(req),
        out: None,
    };
    let pt = run_point(&rec, target);
    let r = {
        let mut lt = LiveTaker::new(&mut calls, m.plan.clone(), pt, None);
        m.reach(&rec, target, &mut lt, false, &tmp.join("engine.err"))
    };
    let _ = std::fs::remove_dir_all(&tmp);
    let (_, reach) = r?;
    let mut v = calls
        .out
        .ok_or_else(|| Problem::new("engine", "the call got no answer", vec![]))?;
    if let Some(c) = v.get_mut("cost") {
        c["wall_ms"] = json!(now_ms(t0));
        c["stops"] = json!(c["stops"].as_u64().unwrap_or(0) + reach.stops);
    }
    if let Some(n) = v.get_mut("notes").and_then(Value::as_array_mut) {
        n.retain(|x| !x.as_str().is_some_and(|s| s.starts_with("live branch")));
    }
    Ok(v)
}

/// A fresh directory for an engine's own files.
pub fn scratch(ctx: &Ctx, what: &str) -> PathBuf {
    let d = home(ctx).join(format!(".{what}-{}", new_id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

// ---------------------------------------------------------------- start, list, call

fn session_doc(ctx: &Ctx, id: &str) -> Option<InspectSession> {
    let v = read_json(&dir_of(ctx, id).join("session.json"))?;
    serde_json::from_value(v.get("session")?.clone()).ok()
}

fn server_alive(ctx: &Ctx, id: &str) -> bool {
    let Some(pid) = read_json(&dir_of(ctx, id).join("session.json"))
        .and_then(|v| v.get("pid").and_then(Value::as_i64))
    else {
        return false;
    };
    // SAFETY: kill with signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// `hydo inspect start <run dir> [--at P] [--freeze] [--idle S] [--wait S]`.
pub fn start(ctx: &Ctx, rest: &[String]) -> Result<Value, Problem> {
    let t0 = Instant::now();
    let mut a = Args::new(rest);
    let at = a.val("--at")?.unwrap_or_else(|| "end".into());
    let freeze = a.flag("--freeze");
    let idle = a.num("--idle")?.unwrap_or(IDLE_S);
    let wait = a.num("--wait")?.unwrap_or(START_WAIT_S);
    let dir = a.one("a run directory (hydo run --out DIR)")?;
    a.done()?;
    let dir = crate::ctx::abspath(Path::new(&dir));
    // checked here, before anything starts, so a mistake is answered at once
    let rec = Recorded::load(&dir)?;
    let target = target_of(&rec, &at)?;
    let id = new_id();
    let sd = dir_of(ctx, &id);
    std::fs::create_dir_all(&sd)
        .map_err(|e| Problem::new("engine", format!("{}: {e}", sd.display()), vec![]))?;
    write_json(
        &sd.join("request.json"),
        &json!({"run_dir": dir, "at": target.point(), "freeze": freeze, "idle_s": idle,
                "backend": ctx.backend_name()}),
    );
    let log = std::fs::File::create(sd.join("server.log"))
        .map_err(|e| Problem::new("engine", e.to_string(), vec![]))?;
    let exe = std::env::current_exe().map_err(|e| Problem::new("engine", e.to_string(), vec![]))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("inspect")
        .arg("__serve")
        .arg(&id)
        .stdin(std::process::Stdio::null())
        .stdout(
            log.try_clone()
                .map_err(|e| Problem::new("engine", e.to_string(), vec![]))?,
        )
        .stderr(log);
    if ctx.kvm {
        cmd.env("HYDO_BACKEND", "kvm");
    }
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid in the child before exec only detaches it from this terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd.spawn()
        .map_err(|e| Problem::new("engine", format!("cannot start the session: {e}"), vec![]))?;
    // wait for the replay
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        if let Some(v) = read_json(&sd.join("session.json")) {
            match v.pointer("/session/status").and_then(Value::as_str) {
                Some("ready") => {
                    let mut out = v.get("answer").cloned().unwrap_or(Value::Null);
                    if let Some(c) = out.get_mut("cost") {
                        c["wall_ms"] = json!(now_ms(t0));
                    }
                    return Ok(out);
                }
                Some("failed") => {
                    return Ok(v.get("answer").cloned().unwrap_or(Value::Null));
                }
                _ => {}
            }
        }
        if Instant::now() > deadline {
            return Err(Problem::new(
                "timeout",
                format!(
                    "the replay to {} did not finish in {wait} s; the session goes on in the \
                     background (its log: {})",
                    target.point(),
                    sd.join("server.log").display()
                ),
                vec![
                    format!("hydo inspect status {id}"),
                    format!("hydo inspect stop {id}"),
                ],
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `hydo inspect list`.
pub fn list(ctx: &Ctx) -> Result<Value, Problem> {
    let mut sessions = Vec::new();
    if let Ok(rd) = std::fs::read_dir(home(ctx)) {
        let mut ids: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.starts_with("i-"))
            .collect();
        ids.sort();
        for id in ids {
            if let Some(mut s) = session_doc(ctx, &id) {
                if s.status != "failed" && !server_alive(ctx, &id) {
                    s.status = "ended".into();
                }
                sessions.push(s);
            }
        }
    }
    Ok(to_value!(&Inspect {
        op: "list".into(),
        sessions,
        ..Inspect::default()
    }))
}

fn no_session(ctx: &Ctx, id: &str) -> Problem {
    let mut t = vec!["hydo inspect list".to_string()];
    if let Some(req) = read_json(&dir_of(ctx, id).join("request.json")) {
        t.push(format!(
            "hydo inspect start {} --at {}",
            req["run_dir"].as_str().unwrap_or("<run dir>"),
            req["at"].as_str().unwrap_or("end")
        ));
    } else {
        t.push("hydo inspect start <run dir> --at end".into());
    }
    Problem::new(
        "no-session",
        format!("no running inspection session {id:?} (it ended, timed out, or never existed)"),
        t,
    )
}

/// A call on a running session.
pub fn call(ctx: &Ctx, id: &str, req: Req) -> Result<Value, Problem> {
    if !id.starts_with("i-") || id.contains('/') {
        return Err(Problem::new(
            "usage",
            format!("{id:?} is neither a session id (i-xxxxxxxx) nor a run directory (with result.json)"),
            vec!["hydo inspect list".into(), "hydo inspect start <run dir> --at end".into()],
        ));
    }
    if !server_alive(ctx, id) {
        return Err(no_session(ctx, id));
    }
    let mut s = UnixStream::connect(sock_of(ctx, id)).map_err(|_| no_session(ctx, id))?;
    let line = serde_json::to_string(&req.to_json()).unwrap_or_default() + "\n";
    s.write_all(line.as_bytes())
        .map_err(|e| Problem::new("engine", format!("session {id}: {e}"), vec![]))?;
    let mut r = String::new();
    BufReader::new(s)
        .read_line(&mut r)
        .map_err(|e| Problem::new("engine", format!("session {id}: {e}"), vec![]))?;
    if r.trim().is_empty() {
        return Err(Problem::new(
            "engine",
            format!(
                "session {id} closed without an answer (its log: {})",
                dir_of(ctx, id).join("server.log").display()
            ),
            vec![format!("hydo inspect status {id}")],
        ));
    }
    serde_json::from_str(&r)
        .map_err(|e| Problem::new("engine", format!("session {id}: {e}"), vec![]))
}

// ---------------------------------------------------------------- the server

/// Requests from the socket, each with where its answer goes.
type Inbox = mpsc::Receiver<(Req, mpsc::Sender<Value>)>;

struct Daemon {
    rx: Inbox,
    idle: Duration,
    reply: Option<mpsc::Sender<Value>>,
    status: InspectSession,
    file: PathBuf,
}

impl Daemon {
    fn save(&self) {
        let mut v = read_json(&self.file).unwrap_or_else(|| json!({}));
        v["session"] = to_value!(&self.status);
        write_json(&self.file, &v);
    }
    fn status_doc(&self) -> Value {
        to_value!(&Inspect {
            op: "status".into(),
            session: Some(self.status.clone()),
            at: Some(self.status.at.clone()),
            ..Inspect::default()
        })
    }
}

impl Calls for Daemon {
    fn next(&mut self) -> Option<Req> {
        loop {
            match self.rx.recv_timeout(self.idle) {
                Ok((Req::Status, tx)) => {
                    let _ = tx.send(self.status_doc());
                }
                Ok((req, tx)) => {
                    self.reply = Some(tx);
                    return Some(req);
                }
                Err(_) => return None,
            }
        }
    }
    fn answer(&mut self, v: Value) {
        self.status.calls += 1;
        self.save();
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(v);
        }
    }
}

fn listen(sock: &Path, tx: mpsc::Sender<(Req, mpsc::Sender<Value>)>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(sock);
    let l = UnixListener::bind(sock)?;
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(match c.try_clone() {
                    Ok(x) => x,
                    Err(_) => return,
                });
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    return;
                }
                let v = match serde_json::from_str::<Value>(&line)
                    .map_err(|e| Problem::new("usage", e.to_string(), vec![]))
                    .and_then(|v| Req::from_json(&v))
                {
                    Ok(req) => {
                        let op = req.op();
                        let (atx, arx) = mpsc::channel();
                        if tx.send((req, atx)).is_err() {
                            err_doc(
                                op,
                                &Problem::new("no-session", "the session is ending", vec![]),
                            )
                        } else {
                            arx.recv().unwrap_or_else(|_| {
                                err_doc(
                                    op,
                                    &Problem::new("no-session", "the session ended", vec![]),
                                )
                            })
                        }
                    }
                    Err(p) => err_doc("?", &p),
                };
                let mut c = c;
                let _ =
                    c.write_all((serde_json::to_string(&v).unwrap_or_default() + "\n").as_bytes());
            });
        }
    });
    Ok(())
}

/// `hydo inspect __serve <id>`: the session's process.
pub fn serve(ctx: &Ctx, id: &str) -> i32 {
    let sd = dir_of(ctx, id);
    let file = sd.join("session.json");
    let fail = |p: &Problem| {
        write_json(
            &file,
            &json!({"pid": std::process::id(), "session": {"id": id, "status": "failed"},
                    "answer": err_doc("start", p)}),
        );
        1
    };
    let Some(req) = read_json(&sd.join("request.json")) else {
        return fail(&Problem::new(
            "engine",
            "the session has no request.json",
            vec![],
        ));
    };
    let dir = PathBuf::from(req["run_dir"].as_str().unwrap_or(""));
    let at = req["at"].as_str().unwrap_or("end").to_string();
    let freeze = req["freeze"].as_bool().unwrap_or(false);
    let idle = Duration::from_secs(req["idle_s"].as_u64().unwrap_or(IDLE_S));
    let rec = match Recorded::load(&dir) {
        Ok(r) => r,
        Err(p) => return fail(&p),
    };
    let target = match target_of(&rec, &at) {
        Ok(t) => t,
        Err(p) => return fail(&p),
    };
    let m = match machine(ctx, &rec, &sd.join("engine")) {
        Ok(m) => m,
        Err(p) => return fail(&p),
    };
    let (tx, rx) = mpsc::channel();
    let sock = sock_of(ctx, id);
    if let Err(e) = listen(&sock, tx) {
        return fail(&Problem::new(
            "engine",
            format!("{}: {e}", sock.display()),
            vec![],
        ));
    }
    let pt = run_point(&rec, target);
    let snapshot = rec.snapshot_ok(target);
    let mut daemon = Daemon {
        rx,
        idle,
        reply: None,
        status: InspectSession {
            id: id.into(),
            run_dir: dir.display().to_string(),
            at: pt.clone(),
            state: if snapshot.is_ok() { "snapshot" } else { "live" }.into(),
            status: "starting".into(),
            idle_timeout_s: idle.as_secs(),
            frozen: freeze.then_some(true),
            engine: ctx.backend_name().into(),
            calls: 0,
        },
        file: file.clone(),
    };
    write_json(
        &file,
        &json!({"pid": std::process::id(), "session": to_value!(&daemon.status)}),
    );
    let started = Instant::now();
    let errlog = sd.join("engine").join("engine.err");
    let ready = |d: &mut Daemon, stops: u64, notes: Vec<String>| {
        d.status.status = "ready".into();
        let a = Inspect {
            op: "start".into(),
            session: Some(d.status.clone()),
            at: Some(d.status.at.clone()),
            cost: Some(hydo_results::InspectCost {
                wall_ms: now_ms(started),
                stops,
            }),
            notes,
            ..Inspect::default()
        };
        let mut v = read_json(&d.file).unwrap_or_else(|| json!({}));
        v["session"] = to_value!(&d.status);
        v["answer"] = to_value!(&a);
        write_json(&d.file, &v);
    };
    match snapshot {
        Ok(()) => {
            let mut snap = FreezeSnap {
                pre: freeze.then(|| freeze_work(&m.plan)),
            };
            let (held, reach) = match m.reach(&rec, target, &mut snap, true, &errlog) {
                Ok(x) => x,
                Err(p) => return fail(&p),
            };
            let Held::Snapshot {
                mut worker,
                mut point,
            } = held
            else {
                return fail(&Problem::new("engine", "the stop was not kept", vec![]));
            };
            ready(&mut daemon, reach.stops, Vec::new());
            let mut k = 0;
            while let Some(req) = daemon.next() {
                if req == Req::Stop {
                    daemon.answer(json!({"v": 1, "type": "inspect", "op": "stop",
                        "notes": ["the session ended"]}));
                    break;
                }
                k += 1;
                let t0 = Instant::now();
                let keep = matches!(req, Req::Advance { .. });
                let v = match work_for(&req, &m.plan, k) {
                    Err(p) => err_doc(req.op(), &p),
                    Ok(mut w) => {
                        match m.call(&mut worker, &mut point, &rec, &mut w, keep, CALL_STOPS) {
                            Err(p) => err_doc(req.op(), &p),
                            Ok(()) => match answer(&req, &w, &pt, "snapshot", now_ms(t0)) {
                                Ok(a) => to_value!(&a),
                                Err(p) => err_doc(req.op(), &p),
                            },
                        }
                    }
                };
                daemon.answer(v);
            }
            let _ = worker.close();
            let _ = std::fs::remove_file(&sock);
            daemon.status.status = "ended".into();
            daemon.save();
            0
        }
        Err(why) => {
            let notes = vec![format!(
                "live session: {why}; calls run one after another on one branch from the stop, \
                 each seeing what the earlier ones left"
            )];
            // the live branch: the replay's conversation serves the calls
            let plan = m.plan.clone();
            let pre = freeze.then(|| freeze_work(&plan));
            // announce readiness at the first call's wait: done by a wrapper
            let mut first = FirstReady {
                d: &mut daemon,
                ready: Some(Box::new(move |d: &mut Daemon, stops| {
                    ready(d, stops, notes.clone())
                })),
            };
            let r = {
                let mut lt = LiveTaker::new(&mut first, plan, pt, pre);
                m.reach(&rec, target, &mut lt, false, &errlog)
            };
            let _ = std::fs::remove_file(&sock);
            match r {
                Ok(_) => {
                    first.d.status.status = "ended".into();
                    first.d.save();
                    0
                }
                Err(p) => fail(&p),
            }
        }
    }
}

/// Calls of a live session: announces the session ready before waiting for the first one.
struct FirstReady<'d> {
    d: &'d mut Daemon,
    ready: Option<Box<dyn FnOnce(&mut Daemon, u64) + 'd>>,
}

impl Calls for FirstReady<'_> {
    fn next(&mut self) -> Option<Req> {
        if let Some(f) = self.ready.take() {
            f(self.d, 0);
        }
        self.d.next()
    }
    fn answer(&mut self, v: Value) {
        self.d.answer(v)
    }
}

/// The snapshot taker with the optional freeze first.
struct FreezeSnap {
    pre: Option<Work>,
}

impl Taker for FreezeSnap {
    fn step(&mut self, _stop: &StopInfo, seen: &[Seen], out: &mut Out) {
        if let Some(w) = self.pre.as_mut() {
            w.step(seen, out);
            if !w.finished() {
                return;
            }
            self.pre = None;
        }
        out.snapshot(SNAP);
        out.end(Ending::AtSnapshot(SNAP.into()));
    }
}
