//! rsv: the Restate example's service and its test client, in one static program.
//!
//!   rsv serve          the deployment: Restate's Rust SDK serves the handlers below on :9080
//!
//! The server is RSV_SERVER (default 10.89.0.11, ingress :8080, admin :9070), the deployment
//! RSV_DEPLOYMENT_ADDR (default 10.89.0.12:9080): the addresses of hydo.yaml's services.
//!   rsv ready <what>   exit 0 when the server (`server`) or the deployment (`svc`) answers
//!   rsv init           register the deployment with the server's admin API (first_init)
//!   rsv op <kind>      one client operation; the composer's HYDO_* variables drive it
//!   rsv drain          the messages check's final: command (history `jobs`)
//!   rsv check <rule>   one check over the client's records, the effects and Restate's state
//!
//! The handlers (one deployment, four services):
//!
//!   Job (object)    run({steps, sleep_ms}): `steps` journaled steps (ctx.run), each one effect,
//!                   with a durable sleep between two steps when sleep_ms > 0. Then the state
//!                   `runs` (+1) and `out` (the invocation id, each step's nonce and wall time).
//!   Acct (object)   add({op, amount}): an effect `in`, a durable sleep of 20 ms, the balance and
//!                   the map of applied ops updated, an effect `out`. One writer per key.
//!   Wait (object)   run(): an awakeable, its id published as an effect, the value it gets
//!                   stored in the state `got` (and `n` +1).
//!   Flow (workflow) run(): waits for the durable promise `p`, stores it in `got`; signal(v)
//!                   resolves `p`.
//!
//! An effect is one line appended to the effects file (RSV_EFFECTS, default /data/fx/effects.log)
//! and synced before the step returns: the outside system that a journaled step changes.
//!
//!   <kind> <key> <invocation id> <step> <nonce> <wall ms>
//!
//! A step's closure first appends `beg` (its start: the step before it was journaled), then
//! `job` with its result. The nonce is <pid>-<counter>: unique per execution of a step's closure. Restate journals the
//! closure's result, so a check compares the journaled nonce of each step with the effects.
//!
//! Client records. $HYDO_ACKED gets one line per acknowledged operation:
//!
//!   call:<job>:<steps>               a request-response call answered 200 (idempotency key = job)
//!   send:<job>:<steps>               a one-way send answered 202
//!   delay:<job>:<sent ms>:<delay ms> a send with a delay, answered 202
//!   sleep:<job>:<sleep ms>           a send of a job with a durable sleep, answered 202
//!   add:<acct>:<op>:<amount>         an add answered 200 (idempotency key = op)
//!   awk:<w>:<value>                  the awakeable's first resolution, answered 202
//!   flow:<f>:<value>                 the workflow promise's first resolution, answered 200
//!
//! $HYDO_OUTCOMES gets <kind>.<key> ack|failed|unknown. The history `jobs` (HYDO-HIST records)
//! has every call and send, for the messages check: the jobs with a delay or a sleep wait for
//! timers on the wall clock, which a backward clock step moves, so the rules judge them.

use restate_sdk::prelude::*;
use serde::{Deserialize as De, Serialize as Ser};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

fn env_def(k: &str, d: &str) -> String {
    std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
}

fn effects_path() -> String {
    env_def("RSV_EFFECTS", "/data/fx/effects.log")
}

fn wall_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Seconds since boot: a clock that a T= step does not move.
fn uptime() -> f64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
}

fn console(line: &str) {
    // one write per line: the client's commands and the service write at once, and two writes
    // (the text, then the newline) let another process's line land in between
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/console") {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// An SDK assertion record: the event of a reaction (A=<id>@...:cut).
fn event(id: &str, msg: &str) {
    let j = serde_json::json!({"t": "sometimes", "id": id, "ok": true, "msg": msg});
    console(&format!("HYDO-ASSERT {j}"));
}

/// One record of the history `jobs`: HYDO-HIST <history> <process> <type> <ns> <op...>.
fn hist(proc_: u64, ty: &str, op: &str) {
    let ns = (uptime() * 1e9) as u64;
    console(&format!("HYDO-HIST jobs {proc_} {ty} {ns} {op}"));
}

fn append_line(path: &str, line: &str) {
    if path.is_empty() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{line}\n").as_bytes()); // one write: O_APPEND keeps it whole
    }
}

fn read_lines(path: &str) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The first 8 characters of the boot's id: two readings of uptime compare only in one boot.
fn boot() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|s| s.trim().chars().take(8).collect()).unwrap_or_else(|_| "-".into())
}

fn up_ms() -> u64 {
    (uptime() * 1000.0) as u64
}

/// When a step ran: the wall clock (which a T= step moves) and the boot's monotonic clock.
#[derive(Ser, De, Clone, Debug, Default)]
struct At {
    nonce: String,
    wall: u64,
    up: u64,
    boot: String,
}

/// Append one effect line and sync it. Returns the nonce and the times.
fn effect(kind: &str, key: &str, inv: &str, step: u32, nonce: Option<&str>) -> std::io::Result<At> {
    let nonce = nonce
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    let (w, u, b) = (wall_ms(), up_ms(), boot());
    let path = effects_path();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(format!("{kind} {key} {inv} {step} {nonce} {w} {u} {b}\n").as_bytes())?;
    f.sync_data()?;
    Ok(At { nonce, wall: w, up: u, boot: b })
}

// ---------------------------------------------------------------------------------------------
// The service (rsv serve)
// ---------------------------------------------------------------------------------------------

#[derive(Ser, De, Clone, Debug, Default)]
struct JobReq {
    steps: u32,
    #[serde(default)]
    sleep_ms: u64,
}

#[derive(Ser, De, Clone, Debug, Default)]
struct JobOut {
    inv: String,
    nonces: Vec<String>,
    walls: Vec<u64>,
    #[serde(default)]
    ups: Vec<u64>,
    #[serde(default)]
    boots: Vec<String>,
    runs: u64,
}

#[derive(Ser, De, Clone, Debug, Default)]
struct AddReq {
    op: String,
    amount: i64,
}

#[derive(Ser, De, Clone, Debug, Default)]
struct AcctState {
    bal: i64,
    ops: BTreeMap<String, u32>,
}

#[derive(Ser, De, Clone, Debug, Default)]
struct Got {
    got: Option<String>,
    n: u64,
}

fn io_err(e: std::io::Error) -> HandlerError {
    HandlerError::from(format!("effect: {e}"))
}

struct Job;

#[restate_sdk::object]
impl Job {
    #[handler]
    async fn run(&self, ctx: ObjectContext<'_>, req: Json<JobReq>) -> HandlerResult<Json<JobOut>> {
        let req = req.into_inner();
        let key = ctx.key().to_string();
        let inv = ctx.invocation_id().to_string();
        let mut out = JobOut { inv: inv.clone(), ..Default::default() };
        for k in 1..=req.steps {
            if k > 1 && req.sleep_ms > 0 {
                ctx.sleep(Duration::from_millis(req.sleep_ms)).await?;
            }
            let (key2, inv2) = (key.clone(), inv.clone());
            let r: Json<At> = ctx
                .run(|| async move {
                    // `beg`: the closure of step k started, so step k-1's result was journaled
                    // one nonce for both lines: a check pairs each result with its start
                    let nonce = format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst));
                    effect("beg", &key2, &inv2, k, Some(&nonce)).map_err(io_err)?;
                    if k > 1 {
                        event("rs.step", &format!("{key2} step {k}"));
                    }
                    effect("job", &key2, &inv2, k, Some(&nonce)).map(Json).map_err(io_err)
                })
                .name(format!("step{k}"))
                .await?;
            let at = r.into_inner();
            out.nonces.push(at.nonce);
            out.walls.push(at.wall);
            out.ups.push(at.up);
            out.boots.push(at.boot);
        }
        let runs = ctx.get::<u64>("runs").await?.unwrap_or(0) + 1;
        ctx.set("runs", runs);
        out.runs = runs;
        ctx.set("out", Json(out.clone()));
        Ok(Json(out))
    }

    #[handler]
    async fn get(&self, ctx: SharedObjectContext<'_>) -> HandlerResult<Json<Option<JobOut>>> {
        Ok(Json(ctx.get::<Json<JobOut>>("out").await?.map(|j| j.into_inner())))
    }
}

struct Acct;

#[restate_sdk::object]
impl Acct {
    #[handler]
    async fn add(&self, ctx: ObjectContext<'_>, req: Json<AddReq>) -> HandlerResult<i64> {
        let req = req.into_inner();
        let key = ctx.key().to_string();
        let inv = ctx.invocation_id().to_string();
        let (k1, i1, o1) = (key.clone(), inv.clone(), req.op.clone());
        ctx.run(|| async move { effect("in", &k1, &i1, 0, Some(&o1)).map(|_| ()).map_err(io_err) })
            .name("in")
            .await?;
        let mut st = ctx.get::<Json<AcctState>>("st").await?.map(|j| j.into_inner()).unwrap_or_default();
        ctx.sleep(Duration::from_millis(20)).await?;
        st.bal += req.amount;
        *st.ops.entry(req.op.clone()).or_insert(0) += 1;
        let bal = st.bal;
        ctx.set("st", Json(st));
        let (k2, i2, o2) = (key, inv, req.op);
        ctx.run(|| async move { effect("out", &k2, &i2, 0, Some(&o2)).map(|_| ()).map_err(io_err) })
            .name("out")
            .await?;
        Ok(bal)
    }

    #[handler]
    async fn get(&self, ctx: SharedObjectContext<'_>) -> HandlerResult<Json<AcctState>> {
        Ok(Json(ctx.get::<Json<AcctState>>("st").await?.map(|j| j.into_inner()).unwrap_or_default()))
    }
}

struct Wait;

#[restate_sdk::object]
impl Wait {
    #[handler]
    async fn run(&self, ctx: ObjectContext<'_>) -> HandlerResult<String> {
        let key = ctx.key().to_string();
        let inv = ctx.invocation_id().to_string();
        let (aid, fut) = ctx.awakeable::<String>();
        let (k1, i1, a1) = (key.clone(), inv.clone(), aid.clone());
        ctx.run(|| async move { effect("awk", &k1, &i1, 0, Some(&a1)).map(|_| ()).map_err(io_err) })
            .name("publish")
            .await?;
        let v = fut.await?;
        let n = ctx.get::<u64>("n").await?.unwrap_or(0) + 1;
        ctx.set("n", n);
        ctx.set("got", v.clone());
        let (k2, i2, v2) = (key, inv, v.clone());
        ctx.run(|| async move { effect("awkgot", &k2, &i2, 0, Some(&v2)).map(|_| ()).map_err(io_err) })
            .name("got")
            .await?;
        Ok(v)
    }

    #[handler]
    async fn get(&self, ctx: SharedObjectContext<'_>) -> HandlerResult<Json<Got>> {
        Ok(Json(Got { got: ctx.get::<String>("got").await?, n: ctx.get::<u64>("n").await?.unwrap_or(0) }))
    }
}

struct Flow;

#[restate_sdk::workflow]
impl Flow {
    #[handler]
    async fn run(&self, ctx: WorkflowContext<'_>, _req: String) -> HandlerResult<String> {
        let key = ctx.key().to_string();
        let inv = ctx.invocation_id().to_string();
        let v: String = ctx.promise::<String>("p").await?;
        let n = ctx.get::<u64>("n").await?.unwrap_or(0) + 1;
        ctx.set("n", n);
        ctx.set("got", v.clone());
        let (k2, i2, v2) = (key, inv, v.clone());
        ctx.run(|| async move { effect("flow", &k2, &i2, 0, Some(&v2)).map(|_| ()).map_err(io_err) })
            .name("got")
            .await?;
        Ok(v)
    }

    #[handler]
    async fn signal(&self, ctx: SharedWorkflowContext<'_>, v: String) -> HandlerResult<()> {
        ctx.resolve_promise("p", v);
        Ok(())
    }

    #[handler]
    async fn get(&self, ctx: SharedWorkflowContext<'_>) -> HandlerResult<Json<Got>> {
        Ok(Json(Got { got: ctx.get::<String>("got").await?, n: ctx.get::<u64>("n").await?.unwrap_or(0) }))
    }
}

fn serve() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        HttpServer::new(Endpoint::builder().bind(Job).bind(Acct).bind(Wait).bind(Flow).build())
            .listen_and_serve("0.0.0.0:9080".parse().unwrap())
            .await;
    });
}

// ---------------------------------------------------------------------------------------------
// A small HTTP/1.1 client (the server's ingress on :8080 and admin API on :9070)
// ---------------------------------------------------------------------------------------------

struct Resp {
    status: u16,
    body: String,
}

fn http(method: &str, port: u16, path: &str, headers: &[(&str, &str)], body: &str, timeout: Duration) -> Result<Resp, String> {
    let host = env_def("RSV_SERVER", "10.89.0.11");
    let addr = (host.as_str(), port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| format!("connect: {e}"))?;
    s.set_read_timeout(Some(timeout)).ok();
    s.set_write_timeout(Some(timeout)).ok();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        // Restate refuses a content type on a handler without input
        req.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, rest) = text.split_once("\r\n\r\n").ok_or("no header end")?;
    let status: u16 = head.split_whitespace().nth(1).and_then(|v| v.parse().ok()).ok_or("no status")?;
    let chunked = head.lines().any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    let body = if chunked { dechunk(rest) } else { rest.to_string() };
    Ok(Resp { status, body })
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        s = rest[n..].trim_start_matches("\r\n");
    }
    out
}

fn ingress(path: &str, headers: &[(&str, &str)], body: &str, timeout: Duration) -> Result<Resp, String> {
    http("POST", 8080, path, headers, body, timeout)
}

// ---------------------------------------------------------------------------------------------
// The client: ready, init, op
// ---------------------------------------------------------------------------------------------

fn ready(what: &str) -> i32 {
    let ok = match what {
        "server" => {
            http("GET", 9070, "/health", &[], "", Duration::from_secs(3)).map(|r| r.status == 200).unwrap_or(false)
                && http("GET", 8080, "/restate/health", &[], "", Duration::from_secs(3)).map(|r| r.status == 200).unwrap_or(false)
        }
        _ => env_def("RSV_DEPLOYMENT_ADDR", "10.89.0.12:9080")
            .parse()
            .map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok())
            .unwrap_or(false),
    };
    if ok { 0 } else { 1 }
}

fn init() -> i32 {
    let end = uptime() + 100.0;
    loop {
        match http("POST", 9070, "/deployments", &[], &format!(r#"{{"uri":"http://{}","force":true}}"#, env_def("RSV_DEPLOYMENT_ADDR", "10.89.0.12:9080")), Duration::from_secs(20)) {
            Ok(r) if r.status / 100 == 2 => {
                println!("rsv: deployment registered: {}", trunc(&r.body, 200));
                return 0;
            }
            Ok(r) => eprintln!("rsv: register: {} {}", r.status, trunc(&r.body, 300)),
            Err(e) => eprintln!("rsv: register: {e}"),
        }
        if uptime() > end {
            return 1;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn cmd_id() -> u64 {
    env_def("HYDO_CMD_ID", "0").parse().unwrap_or(0)
}

fn outcome(oid: &str, o: &str) {
    append_line(&env_def("HYDO_OUTCOMES", ""), &format!("{oid} {o}"));
}

fn acked(rec: &str) {
    append_line(&env_def("HYDO_ACKED", ""), rec);
}

/// One request with up to `tries` attempts while the answer is missing or a 5xx (the same
/// intent each time: the idempotency key deduplicates). Returns the last answer, if any.
fn with_retries(tries: u32, f: impl Fn() -> Result<Resp, String>) -> Option<Resp> {
    let mut last = None;
    for i in 0..tries {
        match f() {
            Ok(r) if r.status < 500 => return Some(r),
            Ok(r) => {
                eprintln!("rsv: attempt {i}: {} {}", r.status, trunc(&r.body, 300));
                last = Some(r);
            }
            Err(e) => eprintln!("rsv: attempt {i}: {e}"),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    last.filter(|r| r.status < 500)
}

fn inv_of(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("invocationId").or_else(|| v.get("inv")).and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Send a job (one-way, optionally with a delay) or call it (request-response).
fn op_job(kind: &str) -> i32 {
    let c = cmd_id();
    let job = format!("j{c}");
    let (steps, sleep_ms, delay_ms, call) = match kind {
        "call" => (3, 0, 0, true),
        "send" => (2, 0, 0, false),
        "delay" => (1, 0, 2000, false),
        _ => (2, 3000, 0, false), // sleep
    };
    let body = format!(r#"{{"steps":{steps},"sleep_ms":{sleep_ms}}}"#);
    let path = if call {
        format!("/Job/{job}/run")
    } else if delay_ms > 0 {
        format!("/Job/{job}/run/send?delay={delay_ms}ms")
    } else {
        format!("/Job/{job}/run/send")
    };
    let item = if delay_ms > 0 { format!("{job};d={delay_ms}") } else { job.clone() };
    let oid = format!("{kind}.{job}");
    let (sent, sent_up, sent_boot) = (wall_ms(), up_ms(), boot());
    let hist = |c: u64, ty: &str, op: &str| if delay_ms == 0 && sleep_ms == 0 { hist(c, ty, op) };
    hist(c, "invoke", &format!("send jobs {item}"));
    let tmo = Duration::from_secs(if call { 15 } else { 8 });
    let r = with_retries(3, || ingress(&path, &[("idempotency-key", &job)], &body, tmo));
    match r {
        Some(r) if (call && r.status == 200) || (!call && r.status == 202) => {
            let inv = inv_of(&r.body).unwrap_or_default();
            let rec = match kind {
                "call" => format!("call:{job}:{steps}"),
                "send" => format!("send:{job}:{steps}"),
                "delay" => format!("delay:{job}:{sent}:{delay_ms}:{sent_up}:{sent_boot}"),
                _ => format!("sleep:{job}:{sleep_ms}"),
            };
            acked(&rec);
            outcome(&oid, "ack");
            hist(c, "ok", &format!("send jobs {item};o={inv}"));
            event(if call { "rs.call_acked" } else { "rs.send_acked" }, &job);
            println!("rsv: {kind} {job} acknowledged: {}", trunc(&r.body, 200));
            0
        }
        Some(r) => {
            outcome(&oid, "failed");
            hist(c, "fail", &format!("send jobs {item}"));
            eprintln!("rsv: {kind} {job}: {} {}", r.status, trunc(&r.body, 300));
            0
        }
        None => {
            outcome(&oid, "unknown");
            hist(c, "info", &format!("send jobs {item}"));
            0
        }
    }
}

fn op_add() -> i32 {
    let c = cmd_id();
    let acct = format!("a{}", c % 3);
    let op = format!("o{c}");
    let amount = (c % 7 + 1) as i64;
    let body = format!(r#"{{"op":"{op}","amount":{amount}}}"#);
    let path = format!("/Acct/{acct}/add");
    let oid = format!("add.{op}");
    match with_retries(3, || ingress(&path, &[("idempotency-key", &op)], &body, Duration::from_secs(15))) {
        Some(r) if r.status == 200 => {
            acked(&format!("add:{acct}:{op}:{amount}"));
            outcome(&oid, "ack");
            event("rs.add_acked", &op);
            0
        }
        Some(r) => {
            outcome(&oid, "failed");
            eprintln!("rsv: add {op}: {} {}", r.status, trunc(&r.body, 300));
            0
        }
        None => {
            outcome(&oid, "unknown");
            0
        }
    }
}

/// The awakeable's id, from the effects (the handler publishes it with a journaled step).
fn awakeable_of(w: &str) -> Option<String> {
    read_lines(&effects_path()).iter().rev().find_map(|l| {
        let f: Vec<&str> = l.split(' ').collect();
        (f.len() == 8 && f[0] == "awk" && f[1] == w).then(|| f[4].to_string())
    })
}

fn op_awk() -> i32 {
    let c = cmd_id();
    let w = format!("w{c}");
    let oid = format!("awk.{w}");
    match with_retries(3, || ingress(&format!("/Wait/{w}/run/send"), &[("idempotency-key", &w)], "", Duration::from_secs(8))) {
        Some(r) if r.status == 202 => {}
        _ => {
            outcome(&oid, "unknown");
            return 0;
        }
    }
    let end = uptime() + 12.0;
    let aid = loop {
        if let Some(a) = awakeable_of(&w) {
            break a;
        }
        if uptime() > end {
            eprintln!("rsv: awk {w}: no awakeable id within 12 s");
            outcome(&oid, "unknown");
            return 0;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let v1 = format!("v1-{w}");
    let r1 = http("POST", 8080, &format!("/restate/awakeables/{aid}/resolve"), &[], &format!("\"{v1}\""), Duration::from_secs(8));
    match r1 {
        Ok(r) if r.status / 100 == 2 => {
            acked(&format!("awk:{w}:{v1}"));
            outcome(&oid, "ack");
            event("rs.awk_resolved", &w);
        }
        Ok(r) => {
            outcome(&oid, "failed");
            eprintln!("rsv: awk {w} resolve: {} {}", r.status, trunc(&r.body, 300));
        }
        Err(e) => {
            outcome(&oid, "unknown");
            eprintln!("rsv: awk {w} resolve: {e}");
        }
    }
    // a second resolution with another value: the first one must stay
    let r2 = http("POST", 8080, &format!("/restate/awakeables/{aid}/resolve"), &[], &format!("\"v2-{w}\""), Duration::from_secs(8));
    println!("rsv: awk {w}: second resolve: {}", r2.map(|r| r.status.to_string()).unwrap_or_else(|e| e));
    0
}

fn op_flow() -> i32 {
    let c = cmd_id();
    let f = format!("f{c}");
    let oid = format!("flow.{f}");
    match with_retries(3, || ingress(&format!("/Flow/{f}/run/send"), &[], "\"go\"", Duration::from_secs(8))) {
        Some(r) if r.status == 202 => {}
        _ => {
            outcome(&oid, "unknown");
            return 0;
        }
    }
    let v1 = format!("v1-{f}");
    match ingress(&format!("/Flow/{f}/signal"), &[("idempotency-key", &format!("{f}-s1"))], &format!("\"{v1}\""), Duration::from_secs(10)) {
        Ok(r) if r.status == 200 => {
            acked(&format!("flow:{f}:{v1}"));
            outcome(&oid, "ack");
            event("rs.flow_signalled", &f);
        }
        Ok(r) => {
            outcome(&oid, "failed");
            eprintln!("rsv: flow {f} signal: {} {}", r.status, trunc(&r.body, 300));
        }
        Err(e) => {
            outcome(&oid, "unknown");
            eprintln!("rsv: flow {f} signal: {e}");
        }
    }
    let r2 = ingress(&format!("/Flow/{f}/signal"), &[], &format!("\"v2-{f}\""), Duration::from_secs(10));
    println!("rsv: flow {f}: second signal: {}", r2.map(|r| r.status.to_string()).unwrap_or_else(|e| e));
    // a second submission of the same workflow id: it must not run again
    let r3 = ingress(&format!("/Flow/{f}/run/send"), &[], "\"again\"", Duration::from_secs(8));
    println!("rsv: flow {f}: second submission: {}", r3.map(|r| format!("{} {}", r.status, trunc(&r.body, 120))).unwrap_or_else(|e| e));
    0
}

// ---------------------------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------------------------

struct Records {
    acked: Vec<Vec<String>>,          // split by ':'
    outcomes: BTreeMap<String, String>, // oid -> last outcome
}

fn records() -> Records {
    let acked = read_lines(&env_def("HYDO_ACKED", "")).iter().map(|l| l.split(':').map(|s| s.to_string()).collect()).collect();
    let mut outcomes = BTreeMap::new();
    for l in read_lines(&env_def("HYDO_OUTCOMES", "")) {
        if let Some((k, v)) = l.split_once(' ') {
            outcomes.insert(k.to_string(), v.to_string());
        }
    }
    Records { acked, outcomes }
}

#[derive(Clone, Debug)]
struct Fx {
    kind: String,
    key: String,
    inv: String,
    step: u32,
    nonce: String,
    wall: u64,
    up: u64,
    boot: String,
}

fn effects() -> Vec<Fx> {
    read_lines(&effects_path())
        .iter()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f.len() == 8).then(|| Fx {
                kind: f[0].into(),
                key: f[1].into(),
                inv: f[2].into(),
                step: f[3].parse().unwrap_or(0),
                nonce: f[4].into(),
                wall: f[5].parse().unwrap_or(0),
                up: f[6].parse().unwrap_or(0),
                boot: f[7].into(),
            })
        })
        .collect()
}

fn get_state<T: for<'a> De<'a>>(path: &str) -> Result<T, String> {
    let r = ingress(path, &[], "", Duration::from_secs(5))?;
    if r.status != 200 {
        return Err(format!("{} {}", r.status, trunc(&r.body, 200)));
    }
    serde_json::from_str(&r.body).map_err(|e| format!("{e}: {}", trunc(&r.body, 200)))
}

/// What Restate's introspection tables say about the invocations of one key: its status, the
/// next attempt, a pending sleep, the last failure. Printed before a violation's lines.
fn introspect(key: &str) -> String {
    let q = format!(
        "SELECT i.id, i.status, i.journal_size, i.retry_count, i.created_at, i.modified_at, i.scheduled_start_at, i.next_retry_at, i.last_failure, \
         (SELECT max(j.sleep_wakeup_at) FROM sys_journal j WHERE j.id = i.id) AS sleep_wakeup_at FROM sys_invocation i WHERE i.target_service_key = '{key}' AND i.target_handler_name <> 'get'"
    );
    let body = serde_json::json!({ "query": q }).to_string();
    let inv = match http("POST", 9070, "/query", &[("Accept", "application/json")], &body, Duration::from_secs(10)) {
        Ok(r) => format!("now {} sys_invocation: {} {}", wall_ms(), r.status, trunc(&r.body, 700)),
        Err(e) => format!("sys_invocation: {e}"),
    };
    let q2 = format!(
        "SELECT j.index, j.entry_type, j.name, j.completed, j.sleep_wakeup_at, j.appended_at, j.entry_lite_json FROM sys_journal j \
         JOIN sys_invocation i ON j.id = i.id WHERE i.target_service_key = '{key}' AND i.target_handler_name <> 'get' ORDER BY j.index"
    );
    let body2 = serde_json::json!({ "query": q2 }).to_string();
    let journal = match http("POST", 9070, "/query", &[("Accept", "application/json")], &body2, Duration::from_secs(10)) {
        Ok(r) => format!("sys_journal: {} {}", r.status, trunc(&r.body, 1500)),
        Err(e) => format!("sys_journal: {e}"),
    };
    detail(format!("{key}: {journal}"));
    // the scheduler's own view: the entry's stage and status, and when it may run next
    let q3 = format!(
        "SELECT e.stage, e.status, e.next_at, e.created_at, e.first_runnable_at, e.has_lock FROM sys_vqueue_entry_status e \
         JOIN sys_invocation i ON e.entry_id = i.id WHERE i.target_service_key = '{key}' AND i.target_handler_name <> 'get'"
    );
    let body3 = serde_json::json!({ "query": q3 }).to_string();
    let vq = match http("POST", 9070, "/query", &[("Accept", "application/json")], &body3, Duration::from_secs(10)) {
        Ok(r) => format!("now {} sys_vqueue_entry_status: {} {}", wall_ms(), r.status, trunc(&r.body, 400)),
        Err(e) => format!("sys_vqueue_entry_status: {e}"),
    };
    DETAILS.lock().unwrap().insert(0, format!("{key}: {vq}"));
    *LAST_VQ.lock().unwrap() = vq;
    inv
}

/// A timestamp of the introspection's JSON (milliseconds, or "YYYY-MM-DDTHH:MM:SS[.fff]") in ms.
fn parse_ts(v: &serde_json::Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    let s = v.as_str()?;
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let frac = s.get(19..).unwrap_or("").trim_start_matches('.');
    let ms: i64 = frac.chars().take_while(|c| c.is_ascii_digit()).chain("000".chars()).take(3).collect::<String>().parse().unwrap_or(0);
    // days from the civil date (Howard Hinnant's algorithm)
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((((days * 24 + h) * 60 + mi) * 60 + se) as u64 * 1000 + ms as u64)
}

/// When introspection shows a sleep or a scheduled start later than the wall clock now: in
/// how many seconds it is due.
static LAST_VQ: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn rows_of(text: &str) -> Vec<serde_json::Value> {
    text.split_once(" 200 ")
        .and_then(|(_, j)| serde_json::from_str::<serde_json::Value>(j).ok())
        .and_then(|v| v.get("rows").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
}

/// [`timer_due_in`] of the last [`introspect`]: the invocation's row, and the scheduler's
/// `next_at` (a delay that the ingress computed before a step back is due later on the wall clock).
fn timer_due_in_last(info: &str) -> Option<i64> {
    timer_due_in(info).or_else(|| {
        let waits = rows_of(info).iter().any(|r| r.get("status").and_then(|s| s.as_str()) == Some("scheduled"));
        let now = wall_ms() as i64;
        let vq = LAST_VQ.lock().unwrap().clone();
        rows_of(&vq).iter().filter(|_| waits).find_map(|r| {
            let t = parse_ts(r.get("next_at")?)? as i64;
            (t > now).then(|| (t - now) / 1000)
        })
    })
}

fn timer_due_in(info: &str) -> Option<i64> {
    let now = wall_ms() as i64;
    rows_of(info).iter().find_map(|r| {
        // only an invocation that waits for a timer (a sleep, a delayed start): one that is
        // `ready` waits for nothing and should run
        let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("");
        if !matches!(status, "scheduled" | "suspended" | "backing-off") {
            return None;
        }
        // a last change later than the wall clock now: the clock was stepped back since, and the
        // invocation's timers wait for the wall clock
        ["sleep_wakeup_at", "scheduled_start_at", "next_retry_at", "modified_at"].iter().find_map(|k| {
            let t = parse_ts(r.get(*k)?)? as i64;
            (t > now).then(|| (t - now) / 1000)
        })
    })
}

fn job_out(job: &str) -> Result<Option<JobOut>, String> {
    get_state(&format!("/Job/{job}/get"))
}

/// The jobs whose send or call may have reached Restate: acknowledged or unknown.
fn job_ids(rec: &Records) -> (BTreeSet<String>, BTreeSet<String>) {
    job_ids_of(rec, &["call", "send", "delay", "sleep"])
}

/// [`job_ids`] of the kinds named: the messages check's history has the calls and sends only.
fn job_ids_of(rec: &Records, kinds: &[&str]) -> (BTreeSet<String>, BTreeSet<String>) {
    let (mut acked, mut maybe) = (BTreeSet::new(), BTreeSet::new());
    for (oid, o) in &rec.outcomes {
        if let Some((kind, key)) = oid.split_once('.') {
            if kinds.contains(&kind) {
                match o.as_str() {
                    "ack" => {
                        acked.insert(key.to_string());
                    }
                    "unknown" => {
                        maybe.insert(key.to_string());
                    }
                    _ => {}
                }
            }
        }
    }
    (acked, maybe)
}

/// Wait (up to `secs` of uptime) until every acknowledged operation has its result in Restate.
fn settle(rec: &Records, secs: f64) {
    // one wait per boot: the first check waits, the others judge what it waited for (the
    // marker names the boot, so a recovery boot waits again)
    let marker_path = format!("{}.settled-{}", effects_path(), boot());
    let marker = std::path::Path::new(&marker_path);
    if marker.exists() {
        return;
    }
    let end = uptime() + secs;
    let (jobs, _) = job_ids(rec);
    let mut pending: BTreeSet<String> = jobs.iter().map(|j| format!("/Job/{j}/get")).collect();
    for a in &rec.acked {
        match a[0].as_str() {
            "add" => {
                pending.insert(format!("/Acct/{}/get#{}", a[1], a[2]));
            }
            "awk" => {
                pending.insert(format!("/Wait/{}/get", a[1]));
            }
            "flow" => {
                pending.insert(format!("/Flow/{}/get", a[1]));
            }
            _ => {}
        }
    }
    while !pending.is_empty() && uptime() < end {
        let mut done = Vec::new();
        for p in &pending {
            let (path, op) = p.split_once('#').map(|(a, b)| (a, Some(b))).unwrap_or((p.as_str(), None));
            let ok = if path.starts_with("/Job/") {
                matches!(get_state::<Option<JobOut>>(path), Ok(Some(_)))
            } else if path.starts_with("/Acct/") {
                get_state::<AcctState>(path).map(|s| s.ops.contains_key(op.unwrap_or(""))).unwrap_or(false)
            } else {
                get_state::<Got>(path).map(|g| g.got.is_some()).unwrap_or(false)
            };
            if ok {
                done.push(p.clone());
            }
        }
        for d in done {
            pending.remove(&d);
        }
        if !pending.is_empty() {
            std::thread::sleep(Duration::from_millis(1000));
        }
    }
    if !pending.is_empty() {
        println!("rsv: {} acknowledged operations without a result after {secs} s", pending.len());
    }
    let _ = std::fs::write(marker, "");
}

static DETAILS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A line of what Restate's introspection said: printed before the violations (a run keeps the
/// last lines of a check's output, and the violations matter more).
fn detail(line: String) {
    DETAILS.lock().unwrap().push(line);
}

fn finish(rule: &str, bad: &[String], summary: &str) -> i32 {
    for d in DETAILS.lock().unwrap().iter().take(6) {
        println!("{}", trunc(d, 240));
    }
    for b in bad.iter().take(20) {
        println!("{b}");
    }
    if bad.is_empty() {
        println!("{rule}: holds: {summary}");
        0
    } else {
        println!("{rule}: {} violations: {summary}", bad.len());
        100
    }
}

/// Every acknowledged job ran once, all its steps; no journaled step ran again; one
/// invocation per idempotency key.
fn check_jobs() -> i32 {
    let rec = records();
    settle(&rec, 100.0);
    let (acked, maybe) = job_ids(&rec);
    let fx = effects();
    let mut bad = Vec::new();
    let mut steps_of: BTreeMap<String, u32> = BTreeMap::new();
    for a in &rec.acked {
        match a[0].as_str() {
            "call" | "send" => {
                steps_of.insert(a[1].clone(), a[2].parse().unwrap_or(0));
            }
            "delay" => {
                steps_of.insert(a[1].clone(), 1);
            }
            "sleep" => {
                steps_of.insert(a[1].clone(), 2);
            }
            _ => {}
        }
    }
    let mut judged = 0;
    let mut delayed: Vec<String> = Vec::new();
    let mut slowest: (String, u64) = (String::new(), 0);
    for job in acked.iter().chain(maybe.iter()) {
        let is_acked = acked.contains(job);
        let out = match job_out(job) {
            Ok(o) => o,
            Err(e) => {
                bad.push(format!("{job}: its state cannot be read: {e}"));
                continue;
            }
        };
        let mine: Vec<&Fx> = fx.iter().filter(|f| f.kind == "job" && &f.key == job).collect();
        let with_beg: Vec<&Fx> = fx.iter().filter(|f| (f.kind == "job" || f.kind == "beg") && &f.key == job).collect();
        let Some(out) = out else {
            if is_acked {
                let info = introspect(job);
                detail(format!("{job}: {info}"));
                match timer_due_in_last(&info) {
                    // Restate's timers follow the wall clock, which its documentation asks to keep
                    // in sync: a timer due later on it is delayed, not lost
                    Some(s) => delayed.push(format!("{job} ({s} s)")),
                    None => {
                        let kind = rec.acked.iter().find(|a| a.len() > 1 && &a[1] == job).map(|a| a[0].as_str()).unwrap_or("?");
                        bad.push(format!("{job} ({kind}): acknowledged, never completed ({} effects)", mine.len()))
                    }
                }
            }
            continue;
        };
        judged += 1;
        // the longest wait of the job between two of its effects in one boot, by the boot's clock,
        // without its own sleep: how long Restate held it (a measure, not a rule)
        let own_sleep = rec.acked.iter().find(|a| a[0] == "sleep" && &a[1] == job).map(|a| a[2].parse::<u64>().unwrap_or(0)).unwrap_or(0);
        for w in with_beg.windows(2) {
            if w[0].boot == w[1].boot && w[1].up > w[0].up + own_sleep {
                let gap = w[1].up - w[0].up - own_sleep;
                if gap > slowest.1 {
                    slowest = (job.clone(), gap);
                }
            }
        }
        if out.runs != 1 {
            bad.push(format!("{job}: the handler completed {} times (runs = {})", out.runs, out.runs));
        }
        if let Some(&n) = steps_of.get(job) {
            if out.nonces.len() as u32 != n {
                bad.push(format!("{job}: {} journaled steps, {n} expected", out.nonces.len()));
            }
        }
        let invs: BTreeSet<&str> = mine.iter().map(|f| f.inv.as_str()).collect();
        if invs.len() > 1 {
            bad.push(format!("{job}: effects of {} invocations for one idempotency key: {:?}", invs.len(), invs));
        }
        // the effects of each step: the last one is the journaled one, and no step runs after a later one
        // a closure of step k starts only after step k-1's result was journaled (`beg`)
        // (the starts only: an attempt that Restate gave up on, a zombie, may finish a closure it
        // had started before, and that is the at-least-once of a step that was not journaled)
        let mut max_step = 0;
        for f in with_beg.iter().filter(|f| f.kind == "beg") {
            if f.step < max_step {
                bad.push(format!("{job}: step {} started again ({} at {}) after step {max_step} had started", f.step, f.nonce, f.wall));
            }
            max_step = max_step.max(f.step);
        }
        let pos = |kind: &str, nonce: &str| with_beg.iter().position(|f| f.kind == kind && f.nonce == nonce);
        for (i, n) in out.nonces.iter().enumerate() {
            let k = i as u32 + 1;
            let of_k: Vec<&&Fx> = mine.iter().filter(|f| f.step == k).collect();
            match of_k.iter().position(|f| &f.nonce == n) {
                None => bad.push(format!("{job}: the journaled nonce {n} of step {k} has no effect")),
                Some(p) => {
                    // a later execution of step k that also started after the journaled one ended
                    let journaled_end = pos("job", n);
                    let again: Vec<&str> = of_k[p + 1..]
                        .iter()
                        .filter(|f| match (pos("beg", &f.nonce), journaled_end) {
                            (Some(b), Some(e)) => b > e,
                            _ => true,
                        })
                        .map(|f| f.nonce.as_str())
                        .collect();
                    if !again.is_empty() {
                        bad.push(format!("{job}: step {k} ran again after its result {n} was journaled: {again:?}"));
                    } else if p + 1 < of_k.len() {
                        println!("{job}: step {k} also ended in an attempt that had started before {n} was journaled (a zombie, at least once)");
                    }
                }
            }
        }
    }
    if !delayed.is_empty() {
        println!("jobs: {} acknowledged jobs wait for a timer that the wall clock has not reached: {}", delayed.len(), delayed.join(", "));
    }
    finish(
        "jobs",
        &bad,
        &format!(
            "{} acknowledged and {} unknown jobs, {judged} completed, {} effects; the longest wait between two steps {} ms ({})",
            acked.len(),
            maybe.len(),
            fx.len(),
            slowest.1,
            slowest.0
        ),
    )
}

/// A durable sleep ends at or after its time; a delayed send runs at or after its delay.
fn check_timers() -> i32 {
    let rec = records();
    settle(&rec, 100.0);
    let mut bad = Vec::new();
    let mut n = 0;
    // Too short only when both clocks say so: the wall clock (a T= step moves it) and, within one
    // boot, the boot's monotonic clock. Across a power cut only the wall clock is judged.
    let short = |ms: u64, w0: u64, w1: u64, m: Option<(u64, u64)>| -> Option<String> {
        let wall_short = w1 < w0 + ms;
        let mono_short = m.map(|(u0, u1)| u1 < u0 + ms).unwrap_or(true);
        (wall_short && mono_short).then(|| {
            let mono = m.map(|(u0, u1)| format!(", {} ms by the boot's clock", u1 as i64 - u0 as i64)).unwrap_or_default();
            format!("{} ms by the wall clock{mono}", w1 as i64 - w0 as i64)
        })
    };
    for a in &rec.acked {
        match a[0].as_str() {
            "sleep" => {
                let ms: u64 = a[2].parse().unwrap_or(0);
                if let Ok(Some(o)) = job_out(&a[1]) {
                    n += 1;
                    if o.walls.len() == 2 {
                        let m = (o.ups.len() == 2 && o.boots.len() == 2 && o.boots[0] == o.boots[1]).then(|| (o.ups[0], o.ups[1]));
                        if let Some(d) = short(ms, o.walls[0], o.walls[1], m) {
                            bad.push(format!("{}: a sleep of {ms} ms ended after {d}", a[1]));
                        }
                    }
                }
            }
            "delay" if a.len() >= 6 => {
                let (sent, ms, up): (u64, u64, u64) = (a[2].parse().unwrap_or(0), a[3].parse().unwrap_or(0), a[4].parse().unwrap_or(0));
                if let Ok(Some(o)) = job_out(&a[1]) {
                    n += 1;
                    if let (Some(&w), Some(&u), Some(b)) = (o.walls.first(), o.ups.first(), o.boots.first()) {
                        let m = (b == &a[5]).then_some((up, u));
                        if let Some(d) = short(ms, sent, w, m) {
                            bad.push(format!("{}: a send with a delay of {ms} ms ran after {d}", a[1]));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    finish("timers", &bad, &format!("{n} sleeps and delayed sends judged"))
}

/// Virtual objects: one invocation at a time per key; the state has each acknowledged add once.
fn check_objects() -> i32 {
    let rec = records();
    settle(&rec, 100.0);
    let fx = effects();
    let mut bad = Vec::new();
    let mut amounts: BTreeMap<String, i64> = BTreeMap::new();
    let mut acked_by_acct: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in &rec.acked {
        if a[0] == "add" {
            amounts.insert(a[2].clone(), a[3].parse().unwrap_or(0));
            acked_by_acct.entry(a[1].clone()).or_default().push(a[2].clone());
        }
    }
    // the amounts of unknown adds, from the op number
    for (oid, o) in &rec.outcomes {
        if let Some(op) = oid.strip_prefix("add.") {
            if o != "failed" {
                let c: i64 = op.trim_start_matches('o').parse().unwrap_or(0);
                amounts.entry(op.to_string()).or_insert(c % 7 + 1);
            }
        }
    }
    let mut keys: BTreeSet<String> = acked_by_acct.keys().cloned().collect();
    for f in &fx {
        if f.kind == "in" || f.kind == "out" {
            keys.insert(f.key.clone());
        }
    }
    for k in &keys {
        // the effects: no `in` of another invocation while one holds the key
        // A line of an invocation that had already left is a zombie's: an attempt that Restate
        // gave up on finishing a closure it had started (at least once), not a second writer.
        let mut holder: Option<&str> = None;
        let mut left: BTreeSet<&str> = BTreeSet::new();
        for f in fx.iter().filter(|f| &f.key == k && (f.kind == "in" || f.kind == "out")) {
            if left.contains(f.inv.as_str()) {
                println!("{k}: {} {} again after it had left (a zombie, at least once)", f.inv, f.kind);
                continue;
            }
            match (f.kind.as_str(), holder) {
                ("in", Some(h)) if h != f.inv => {
                    bad.push(format!("{k}: {} ({}) entered while {h} was inside", f.inv, f.nonce));
                    holder = Some(&f.inv);
                }
                ("in", _) => holder = Some(&f.inv),
                ("out", Some(h)) if h == f.inv => {
                    holder = None;
                    left.insert(&f.inv);
                }
                ("out", Some(h)) => bad.push(format!("{k}: {} ({}) left while {h} was inside", f.inv, f.nonce)),
                ("out", None) => {
                    left.insert(&f.inv);
                }
                _ => {}
            }
        }
        let st = match get_state::<AcctState>(&format!("/Acct/{k}/get")) {
            Ok(s) => s,
            Err(e) => {
                bad.push(format!("{k}: its state cannot be read: {e}"));
                continue;
            }
        };
        let mut sum = 0;
        for (op, n) in &st.ops {
            if *n != 1 {
                bad.push(format!("{k}: op {op} applied {n} times"));
            }
            // An op without a record was in flight at a power cut: its amount comes from its
            // number, as the client chose it. An op whose name is not the client's is a phantom.
            let amount = amounts.get(op).copied().or_else(|| op.strip_prefix('o').and_then(|c| c.parse::<i64>().ok()).map(|c| c % 7 + 1));
            match amount {
                Some(a) => sum += a * *n as i64,
                None => bad.push(format!("{k}: op {op} applied, never sent")),
            }
        }
        if sum != st.bal {
            bad.push(format!("{k}: balance {} but its applied ops sum to {sum}", st.bal));
        }
        for op in acked_by_acct.get(k).into_iter().flatten() {
            if !st.ops.contains_key(op) {
                detail(format!("{k}: {}", introspect(k)));
                bad.push(format!("{k}: op {op} acknowledged, not in the state"));
            }
        }
    }
    finish("objects", &bad, &format!("{} accounts, {} acknowledged adds", keys.len(), acked_by_acct.values().map(|v| v.len()).sum::<usize>()))
}

/// Awakeables and workflow promises: the first acknowledged resolution wins, once.
fn check_signals() -> i32 {
    let rec = records();
    settle(&rec, 100.0);
    let fx = effects();
    let mut bad = Vec::new();
    let mut n = 0;
    for a in &rec.acked {
        let path = match a[0].as_str() {
            "awk" => format!("/Wait/{}/get", a[1]),
            "flow" => format!("/Flow/{}/get", a[1]),
            _ => continue,
        };
        n += 1;
        match get_state::<Got>(&path) {
            Ok(g) => {
                if g.got.as_deref() != Some(a[2].as_str()) {
                    bad.push(format!("{} {}: got {:?}, the first resolution was {}", a[0], a[1], g.got, a[2]));
                }
                if g.n > 1 {
                    bad.push(format!("{} {}: the value was stored {} times", a[0], a[1], g.n));
                }
            }
            Err(e) => bad.push(format!("{} {}: its state cannot be read: {e}", a[0], a[1])),
        }
        let kind = if a[0] == "awk" { "awkgot" } else { "flow" };
        let invs: BTreeSet<&str> = fx.iter().filter(|f| f.kind == kind && f.key == a[1]).map(|f| f.inv.as_str()).collect();
        if invs.len() > 1 {
            bad.push(format!("{} {}: {} invocations completed it: {:?}", a[0], a[1], invs.len(), invs));
        }
    }
    finish("signals", &bad, &format!("{n} acknowledged resolutions"))
}

/// The messages check's drain: one receive per completed run of each job that was sent.
fn drain() -> i32 {
    let rec = records();
    let (acked, maybe) = job_ids_of(&rec, &["call", "send"]);
    let p = 100000;
    let deadline = uptime() + 150.0;
    let mut quiet_from = uptime();
    let mut done: BTreeMap<String, u64> = BTreeMap::new();
    while uptime() < deadline {
        let mut changed = false;
        for job in acked.iter().chain(maybe.iter()) {
            if done.contains_key(job) {
                continue;
            }
            hist(p, "invoke", "recv jobs -");
            match job_out(job) {
                Ok(Some(o)) => {
                    let items: Vec<String> = (0..o.runs).map(|_| format!("{job};o={}", o.inv)).collect();
                    hist(p, "ok", &format!("recv jobs - {}", items.join(" ")));
                    done.insert(job.clone(), o.runs);
                    changed = true;
                }
                Ok(None) => hist(p, "ok", "recv jobs -"),
                Err(_) => {
                    hist(p, "info", "recv jobs -");
                    quiet_from = uptime();
                }
            }
        }
        if changed {
            quiet_from = uptime();
        }
        let all_acked = acked.iter().all(|j| done.contains_key(j));
        if all_acked && uptime() - quiet_from >= 10.0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
    hist(p, "ok", "drained jobs -");
    println!("rsv: drained: {} of {} acknowledged and {} unknown jobs completed", done.len(), acked.len(), maybe.len());
    0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let a = |i: usize| args.get(i).map(|s| s.as_str()).unwrap_or("");
    let code = match a(1) {
        "serve" => {
            serve();
            0
        }
        "ready" => ready(a(2)),
        "init" => init(),
        "op" => match a(2) {
            "call" | "send" | "delay" | "sleep" => op_job(a(2)),
            "add" => op_add(),
            "awk" => op_awk(),
            "flow" => op_flow(),
            k => {
                eprintln!("rsv: unknown op {k}");
                2
            }
        },
        "drain" => drain(),
        "check" => match a(2) {
            "jobs" => check_jobs(),
            "timers" => check_timers(),
            "objects" => check_objects(),
            "signals" => check_signals(),
            k => {
                eprintln!("rsv: unknown check {k}");
                2
            }
        },
        "get" => match ingress(a(2), &[], "", Duration::from_secs(5)) {
            Ok(r) => {
                println!("{} {}", r.status, r.body);
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        _ => {
            eprintln!("usage: rsv serve | ready server|svc | init | op <kind> | drain | check <rule> | get <path>");
            2
        }
    };
    std::process::exit(code);
}
