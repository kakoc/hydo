//! The guest side: drives a project's RPCs with generated requests and records every call's
//! outcome. One static binary (`hydo-grpc`) serves every project; the schema and the hints come
//! from grpc/desc.pb and grpc/plan.json.

use crate::gojson::{marshal, J};
use crate::outcome::{self as oc, classify, code_name, Kind, Outcome};
use crate::pjson;
use crate::record::{self, Recorder};
use crate::rng::{derive, R};
use crate::spec::{From, Plan, Rpc, Scenario};
use crate::state::State;
use crate::transport::Channel;
use crate::valuegen::{self, canonical, Env, Gen, GenError};

use prost_reflect::{
    DescriptorPool, DynamicMessage, Kind as PKind, MessageDescriptor, ReflectMessage, Value,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Debug)]
struct Capture {
    side: String,
    path: String,
    pool: String,
}

/// Drives one plan.
pub struct Client {
    pub plan: Plan,
    pub pool: DescriptorPool,
    pub rec: Recorder,
    pub state: State,
    pub seed: u64,
    conns: Mutex<HashMap<String, Arc<Channel>>>,
    /// HYDO_OUTCOMES: "<id> ack|failed|unknown"; HYDO_ACKED: "<id>"
    sinks: Mutex<(Option<File>, Option<File>)>,
    /// source rpc -> the pools it feeds, in plan order
    captures: BTreeMap<String, Vec<Capture>>,
    asserted: Mutex<HashSet<String>>,
    json_pool: std::sync::OnceLock<DescriptorPool>,
}

/// Options for `Client::new`.
pub struct Options {
    pub desc: String,
    pub plan: String,
    pub state_dir: String,
    pub calls: String,
    pub seed: Option<u64>,
}

/// Loads a serialized FileDescriptorSet.
pub fn read_schema(path: &str) -> Result<DescriptorPool, String> {
    let b = std::fs::read(path).map_err(|e| format!("open {}: {}", path, crate::io_msg(&e)))?;
    DescriptorPool::decode(b.as_slice()).map_err(|e| format!("{path}: {e}"))
}

/// A message type by full name.
pub fn message(pool: &DescriptorPool, name: &str) -> Result<MessageDescriptor, String> {
    pool.get_message_by_name(name)
        .ok_or_else(|| format!("message {name}: proto: not found"))
}

fn open_append(v: &str) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(v).ok()
}

/// A worker's env: unique values from the shared counter, `from` values from the shared pools,
/// preferring what this worker captured itself during a scenario.
pub struct WorkerEnv<'a> {
    c: &'a Client,
    local: HashMap<String, Vec<String>>,
    scen: bool,
}

impl<'a> WorkerEnv<'a> {
    pub fn new(c: &'a Client) -> WorkerEnv<'a> {
        WorkerEnv {
            c,
            local: HashMap::new(),
            scen: false,
        }
    }
}

impl Env for WorkerEnv<'_> {
    fn unique(&mut self) -> u64 {
        self.c.state.next("unique")
    }

    fn take(&mut self, f: &From, r: &mut R) -> Option<String> {
        if let Some(vs) = self.local.get(&f.pool()) {
            if !vs.is_empty() && (self.scen || f.pick == "latest") {
                return vs.last().cloned();
            }
        }
        self.c.state.take(f, r)
    }
}

/// The env of `gen` and of a generated `call`: a local counter, no earlier values.
#[derive(Default)]
pub struct GenEnv {
    n: u64,
}

impl Env for GenEnv {
    fn unique(&mut self) -> u64 {
        self.n += 1;
        self.n
    }

    fn take(&mut self, _: &From, _: &mut R) -> Option<String> {
        None
    }
}

/// What a worker does next: one RPC or a scenario.
#[derive(Clone, Copy)]
enum Item {
    Rpc(usize),
    Scen(usize),
}

/// The bound on redraws because a `from` field has no value yet.
const MAX_SKIPS: usize = 16;

impl Client {
    /// Loads the schema and plan and opens the record sinks.
    pub fn new(o: Options) -> Result<Client, String> {
        let pool = read_schema(&o.desc)?;
        let plan = Plan::load(&o.plan)?;
        let state = State::new(&o.state_dir)?;
        let seed = o.seed.unwrap_or(plan.seed);
        let calls = if o.calls.is_empty() {
            format!("{}/calls", o.state_dir)
        } else {
            o.calls.clone()
        };
        let mut rec = Recorder::open(&calls, &plan.record.console);
        if !plan.record.history.is_empty() {
            rec.history = plan.record.history.clone();
        }
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let outcomes = env("HYDO_OUTCOMES").and_then(|v| open_append(&v));
        let acked = env("HYDO_ACKED").and_then(|v| open_append(&v));
        let mut captures: BTreeMap<String, Vec<Capture>> = BTreeMap::new();
        for r in &plan.rpcs {
            for path in r.field_paths() {
                if let Some(f) = &r.fields[&path].from {
                    let cs = captures.entry(f.rpc.clone()).or_default();
                    if !cs.iter().any(|x| x.pool == f.pool()) {
                        cs.push(Capture {
                            side: f.side.clone(),
                            path: f.path.clone(),
                            pool: f.pool(),
                        });
                    }
                }
            }
        }
        Ok(Client {
            plan,
            pool,
            rec,
            state,
            seed,
            conns: Mutex::new(HashMap::new()),
            sinks: Mutex::new((outcomes, acked)),
            captures,
            asserted: Mutex::new(HashSet::new()),
            json_pool: std::sync::OnceLock::new(),
        })
    }

    fn conn(&self, target: &str) -> Arc<Channel> {
        let mut g = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(target.to_string())
            .or_insert_with(|| Arc::new(Channel::new(target)))
            .clone()
    }

    fn target(&self, r: &Rpc) -> String {
        if r.target.is_empty() {
            self.plan.target.clone()
        } else {
            r.target.clone()
        }
    }

    /// n requests for an RPC (n = 1 unless client-streaming).
    pub fn generate(
        &self,
        r: &Rpc,
        rnd: &mut R,
        env: &mut dyn Env,
    ) -> Result<Vec<DynamicMessage>, GenError> {
        let md = message(&self.pool, &r.input).map_err(GenError::Other)?;
        let mut g = Gen {
            d: &self.plan.defaults,
            hints: &r.fields,
            oneofs: &r.oneofs,
            env,
        };
        let n = if r.client_streaming {
            rnd.range(r.stream_len[0], r.stream_len[1])
        } else {
            1
        };
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(g.message(&md, rnd)?);
        }
        Ok(out)
    }

    fn json_pool(&self) -> &DescriptorPool {
        self.json_pool.get_or_init(|| json_pool(&self.pool))
    }

    /// Reads requests back from JSON (the inverse of `pjson::json`).
    pub fn parse_json(&self, r: &Rpc, s: &str) -> Result<Vec<DynamicMessage>, String> {
        let v = crate::jsonin::parse(s)?;
        let raws: Vec<serde_json::Value> = if s.trim_start().starts_with('[') {
            match v {
                serde_json::Value::Array(a) => a,
                _ => return Err("not an array".into()),
            }
        } else {
            vec![v]
        };
        let mut out = Vec::new();
        for raw in raws {
            out.push(read_json(self.json_pool(), &r.input, raw)?);
        }
        Ok(out)
    }

    /// One attempt, classified.
    pub async fn call(&self, r: &Rpc, reqs: &[DynamicMessage]) -> (Outcome, Vec<DynamicMessage>) {
        let omd = match message(&self.pool, &r.output) {
            Ok(m) => m,
            Err(e) => {
                return (
                    Outcome {
                        kind: Kind::Unknown,
                        code: oc::OK,
                        msg: e,
                    },
                    Vec::new(),
                )
            }
        };
        let ch = self.conn(&self.target(r));
        let deadline = Instant::now() + Duration::from_millis(r.deadline_ms.max(0) as u64);
        let bodies: Vec<Vec<u8>> = reqs.iter().map(crate::wire::encode).collect();
        let unary = !r.client_streaming && !r.server_streaming;
        let send: &[Vec<u8>] = if r.client_streaming {
            &bodies
        } else {
            &bodies[..bodies.len().min(1)]
        };
        let (msgs, st) = ch.call(&r.path, send, r.client_streaming, deadline).await;
        let mut resps = Vec::new();
        for b in &msgs {
            match DynamicMessage::decode(omd.clone(), b.as_slice()) {
                Ok(m) => resps.push(m),
                Err(e) => {
                    let st = classify(
                        oc::INTERNAL,
                        &format!("grpc: failed to unmarshal the received message: {e}"),
                        &r.classify,
                    );
                    return (
                        st,
                        if r.server_streaming {
                            resps
                        } else {
                            Vec::new()
                        },
                    );
                }
            }
        }
        if st.code != oc::OK {
            let res = classify(st.code, &st.msg, &r.classify);
            return (
                res,
                if r.server_streaming {
                    resps
                } else {
                    Vec::new()
                },
            );
        }
        if !r.server_streaming && resps.len() > 1 {
            return (
                classify(
                    oc::UNKNOWN,
                    "grpc: client streaming protocol violation: get <nil>, want <EOF>",
                    &r.classify,
                ),
                Vec::new(),
            );
        }
        if unary && resps.is_empty() {
            // grpc-go's Invoke returns io.EOF here
            return (classify(oc::UNKNOWN, "EOF", &r.classify), Vec::new());
        }
        (classify(oc::OK, "", &r.classify), resps)
    }

    /// One logical operation: an attempt, then up to `retry.attempts` more with the SAME request
    /// while the outcome is unknown. Every attempt is recorded.
    pub async fn op(
        &self,
        process: i64,
        op: u64,
        r: &Rpc,
        reqs: &[DynamicMessage],
        mut env: Option<&mut WorkerEnv<'_>>,
    ) -> Outcome {
        let body = pjson::json(reqs, r.client_streaming);
        let mut res = Outcome {
            kind: Kind::Unknown,
            code: oc::UNKNOWN,
            msg: String::new(),
        };
        for a in 0..=r.retry.attempts.max(0) {
            if a > 0 && r.retry.backoff_ms > 0 {
                tokio::time::sleep(Duration::from_millis(r.retry.backoff_ms as u64)).await;
            }
            res = self
                .attempt(process, op, r, reqs, &body, env.as_deref_mut())
                .await;
            if res.kind != Kind::Unknown {
                break;
            }
        }
        res
    }

    async fn attempt(
        &self,
        process: i64,
        op: u64,
        r: &Rpc,
        reqs: &[DynamicMessage],
        body: &str,
        env: Option<&mut WorkerEnv<'_>>,
    ) -> Outcome {
        self.rec.invoke(process, op as i64, &r.name, body);
        let (res, resps) = self.call(r, reqs).await;
        let out = if res.kind == Kind::Ok {
            pjson::json(&resps, r.server_streaming)
        } else {
            res.msg.clone()
        };
        self.rec.complete(
            process,
            op as i64,
            res.kind.as_str(),
            &r.name,
            &code_name(res.code),
            &out,
        );
        self.assert_seen(r, res.kind);
        let id = self.outcome_id(r, reqs);
        if !id.is_empty() {
            let mut g = self.sinks.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(f) = g.0.as_mut() {
                let _ = f.write_all(format!("{} {}\n", id, res.kind.word()).as_bytes());
            }
            if res.kind == Kind::Ok {
                if let Some(f) = g.1.as_mut() {
                    let _ = f.write_all(format!("{id}\n").as_bytes());
                }
            }
        }
        // feed the pools: request fields after an ok or unknown outcome (the entity may exist),
        // response fields after an ok
        let mut env = env;
        if let Some(cs) = self.captures.get(&r.name) {
            for cp in cs {
                let mut vals = Vec::new();
                if cp.side == "request" && res.kind != Kind::Fail {
                    for q in reqs {
                        vals.extend(extract(q, &cp.path));
                    }
                } else if cp.side == "response" && res.kind == Kind::Ok {
                    for m in &resps {
                        vals.extend(extract(m, &cp.path));
                    }
                }
                self.state.put(&cp.pool, &vals);
                if let Some(e) = env.as_deref_mut() {
                    e.local.entry(cp.pool.clone()).or_default().extend(vals);
                }
            }
        }
        res
    }

    /// One HYDO-ASSERT record (type reachable) the first time in a run that an RPC ends ok /
    /// with a definite failure / with an unknown outcome.
    fn assert_seen(&self, r: &Rpc, k: Kind) {
        let word = match k {
            Kind::Ok => "ok",
            Kind::Fail => "failed",
            Kind::Unknown => "unknown",
        };
        let id = format!("grpc.{}.{}", r.short, word);
        {
            let mut g = self.asserted.lock().unwrap_or_else(|e| e.into_inner());
            if !g.insert(id.clone()) {
                return;
            }
        }
        if self.state.next(&format!("assert-{id}")) != 1 {
            return; // another process of this run already reported it
        }
        let rec = J::Obj(vec![
            ("t".into(), J::Str("reachable".into())),
            ("id".into(), J::Str(id)),
            ("ok".into(), J::Bool(true)),
            ("msg".into(), J::Str(format!("{} ended {}", r.name, word))),
        ]);
        self.rec.raw(&format!("HYDO-ASSERT {}", marshal(&rec)));
    }

    fn outcome_id(&self, r: &Rpc, reqs: &[DynamicMessage]) -> String {
        if r.outcome_id.is_empty() || reqs.is_empty() {
            return String::new();
        }
        match extract(&reqs[0], &r.outcome_id).into_iter().next() {
            Some(v) => v.replace([' ', '\n'], "_"),
            None => String::new(),
        }
    }

    fn items(&self) -> (Vec<Item>, Vec<i64>) {
        let mut its = Vec::new();
        let mut ws = Vec::new();
        for (i, r) in self.plan.rpcs.iter().enumerate() {
            if r.weight > 0 {
                its.push(Item::Rpc(i));
                ws.push(r.weight);
            }
        }
        for (i, s) in self.plan.scenarios.iter().enumerate() {
            if s.weight > 0 {
                its.push(Item::Scen(i));
                ws.push(s.weight);
            }
        }
        (its, ws)
    }

    /// A single RPC operation with generated values: (ran, ok); ran is false if it had to be skipped.
    async fn one(
        &self,
        process: i64,
        ri: usize,
        rnd: &mut R,
        e: &mut WorkerEnv<'_>,
    ) -> (bool, bool) {
        let r = &self.plan.rpcs[ri];
        let reqs = match self.generate(r, rnd, e) {
            Ok(q) => q,
            Err(GenError::Skip) => return (false, true),
            Err(GenError::Other(err)) => {
                eprintln!("hydo-grpc: {}: {}", r.name, err);
                return (false, true);
            }
        };
        let op = self.state.next("op");
        (
            true,
            self.op(process, op, r, &reqs, Some(e)).await.kind == Kind::Ok,
        )
    }

    async fn run_item(
        &self,
        process: i64,
        it: Item,
        rnd: &mut R,
        e: &mut WorkerEnv<'_>,
    ) -> (bool, bool) {
        let sc: &Scenario = match it {
            Item::Rpc(i) => return self.one(process, i, rnd, e).await,
            Item::Scen(s) => &self.plan.scenarios[s],
        };
        e.scen = true;
        let (mut ran, mut ok) = (false, true);
        for name in &sc.calls {
            let ri = match self.plan.find(name) {
                Ok(i) => i,
                Err(err) => {
                    eprintln!("hydo-grpc: scenario {}: {}", sc.name, err);
                    break;
                }
            };
            let (did, good) = self.one(process, ri, rnd, e).await;
            if !did {
                break; // a later call needs a value an earlier one did not produce
            }
            ran = true;
            ok = ok && good;
        }
        e.scen = false;
        (ran, ok)
    }

    /// Workload steps [from, to): every step, `concurrency` workers in parallel each run `calls`
    /// operations drawn by weight. Worker w of step s draws from derive(seed, [1, s, w]).
    pub async fn steps(self: &Arc<Self>, from: i64, to: i64) {
        let (its, ws) = self.items();
        if its.is_empty() {
            eprintln!("hydo-grpc: no RPC has a weight > 0");
            return;
        }
        let its = Arc::new(its);
        let ws = Arc::new(ws);
        for s in from..to {
            let mut set = tokio::task::JoinSet::new();
            for w in 0..self.plan.steps.concurrency.max(0) {
                let c = self.clone();
                let (its, ws) = (its.clone(), ws.clone());
                set.spawn(async move {
                    let mut rnd = R::new(derive(c.seed, &[1, s as u64, w as u64]));
                    let mut e = WorkerEnv::new(&c);
                    for _ in 0..c.plan.steps.calls.max(0) {
                        for _ in 0..MAX_SKIPS {
                            let it = its[pick(&mut rnd, &ws)];
                            if c.run_item(w, it, &mut rnd, &mut e).await.0 {
                                break;
                            }
                        }
                    }
                });
            }
            while set.join_next().await.is_some() {}
        }
    }

    /// One composer command: `calls` operations of one RPC (or scenario). Under the composer the
    /// stream is derive(seed, [2, $HYDO_SEED]); without it, derive(seed, [3, n]) for the run's
    /// n-th drive invocation. The process id is the OS process id. Ok is whether every
    /// operation ended ok.
    pub async fn drive(&self, name: &str, calls: i64) -> Result<bool, String> {
        let it = match self.plan.find(name) {
            Ok(i) => Item::Rpc(i),
            Err(err) => match self.plan.scenarios.iter().position(|s| s.name == name) {
                Some(s) => Item::Scen(s),
                None => return Err(err),
            },
        };
        let calls = if calls <= 0 {
            self.plan.drive.calls
        } else {
            calls
        };
        let hs = std::env::var("HYDO_SEED").unwrap_or_default();
        let stream = match crate::gofmt::parse_uint(&hs, 64) {
            Ok(v) if !hs.is_empty() => derive(self.seed, &[2, v]),
            _ => derive(self.seed, &[3, self.state.next("drive")]),
        };
        let mut rnd = R::new(stream);
        let mut e = WorkerEnv::new(self);
        let pid = std::process::id() as i64;
        let mut ok = true;
        for _ in 0..calls {
            for _ in 0..MAX_SKIPS {
                let (ran, good) = self.run_item(pid, it, &mut rnd, &mut e).await;
                if ran {
                    ok = ok && good;
                    break;
                }
            }
        }
        Ok(ok)
    }

    /// Re-sends, with the same request, every operation whose last outcome was unknown and whose
    /// RPC has retry.at_end; in op order, each until its outcome is known or its attempts are
    /// used up.
    pub async fn retry(&self, calls: &str) -> Result<(), String> {
        let text = match std::fs::read(calls) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("open {}: {}", calls, crate::io_msg(&e))),
        };
        struct OpState {
            process: i64,
            rpc: String,
            req: String,
            last: String,
        }
        let mut ops: HashMap<u64, OpState> = HashMap::new();
        let mut order: Vec<u64> = Vec::new();
        for line in String::from_utf8_lossy(&text).lines() {
            let Some(ev) = record::parse(line, &self.rec.history) else {
                continue;
            };
            let Ok(id) = crate::gofmt::parse_uint(&ev.op, 64) else {
                continue;
            };
            let o = ops.entry(id).or_insert_with(|| {
                order.push(id);
                OpState {
                    process: ev.process.parse().unwrap_or(0),
                    rpc: ev.rpc.clone(),
                    req: String::new(),
                    last: String::new(),
                }
            });
            if ev.typ == "invoke" {
                o.req = String::from_utf8_lossy(&ev.body).into_owned();
                o.last = "invoke".into();
            } else {
                o.last = ev.typ.clone();
            }
        }
        order.sort_unstable();
        for id in order {
            let o = &ops[&id];
            if o.last != "info" && o.last != "invoke" {
                continue;
            }
            let Ok(ri) = self.plan.find(&o.rpc) else {
                continue;
            };
            let r = &self.plan.rpcs[ri];
            if !r.retry.at_end {
                continue;
            }
            let reqs = match self.parse_json(r, &o.req) {
                Ok(q) => q,
                Err(e) => {
                    eprintln!("hydo-grpc: retry op {id}: {e}");
                    continue;
                }
            };
            let mut rr = r.clone();
            if r.retry.end_tries > 0 {
                rr.retry.attempts = r.retry.end_tries - 1;
            }
            self.op(o.process, id, &rr, &reqs, None).await;
        }
        Ok(())
    }
}

const EMPTY_ANY: &str = "type.googleapis.com/hydo.grpc.EmptyAny";

/// protojson reads an empty `google.protobuf.Any` (`{}`, which the client writes for one);
/// prost-reflect wants an `@type`. This is the schema plus one marker type, so that `{}` can be
/// read as an Any of the marker type and emptied afterwards (`read_json`).
pub fn json_pool(pool: &DescriptorPool) -> DescriptorPool {
    let mut p = pool.clone();
    let f = prost_reflect::prost_types::FileDescriptorProto {
        name: Some("hydo/grpc/empty_any.proto".into()),
        package: Some("hydo.grpc".into()),
        message_type: vec![prost_reflect::prost_types::DescriptorProto {
            name: Some("EmptyAny".into()),
            ..Default::default()
        }],
        syntax: Some("proto3".into()),
        ..Default::default()
    };
    if p.add_file_descriptor_proto(f).is_err() {
        return pool.clone();
    }
    p
}

/// One message of type `name` from protobuf JSON, with a pool from `json_pool`.
pub fn read_json(
    json_pool: &DescriptorPool,
    name: &str,
    mut raw: serde_json::Value,
) -> Result<DynamicMessage, String> {
    let md = message(json_pool, name)?;
    mark_any(&md, &mut raw);
    let mut m = DynamicMessage::deserialize(md, raw).map_err(|e| e.to_string())?;
    unmark_any(&mut m);
    Ok(m)
}

fn mark_one(md: &MessageDescriptor, x: &mut serde_json::Value) {
    if md.full_name() == "google.protobuf.Any" {
        if x.as_object().is_some_and(|o| o.is_empty()) {
            *x = serde_json::json!({ "@type": EMPTY_ANY });
        }
    } else {
        mark_any(md, x);
    }
}

/// Every empty Any object under `v` (a message of type `md`) becomes an Any of the marker type.
fn mark_any(md: &MessageDescriptor, v: &mut serde_json::Value) {
    let Some(obj) = v.as_object_mut() else { return };
    for (k, val) in obj.iter_mut() {
        let Some(fd) = md
            .get_field_by_name(k)
            .or_else(|| md.get_field_by_json_name(k))
        else {
            continue;
        };
        let PKind::Message(sub) = fd.kind() else {
            continue;
        };
        if fd.is_map() {
            if let PKind::Message(vm) = sub.map_entry_value_field().kind() {
                if let Some(o) = val.as_object_mut() {
                    o.values_mut().for_each(|x| mark_one(&vm, x));
                }
            }
        } else if fd.is_list() {
            if let Some(a) = val.as_array_mut() {
                a.iter_mut().for_each(|x| mark_one(&sub, x));
            }
        } else {
            mark_one(&sub, val);
        }
    }
}

fn unmark_value(v: &mut Value) {
    match v {
        Value::Message(m) => {
            if m.descriptor().full_name() == "google.protobuf.Any" {
                if m.get_field_by_name("type_url")
                    .is_some_and(|u| u.as_str() == Some(EMPTY_ANY))
                {
                    m.clear_field_by_name("type_url");
                    m.clear_field_by_name("value");
                }
            } else {
                unmark_any(m);
            }
        }
        Value::List(l) => l.iter_mut().for_each(unmark_value),
        Value::Map(mp) => mp.values_mut().for_each(unmark_value),
        _ => {}
    }
}

/// The marker Anys of `mark_any`, emptied again.
fn unmark_any(m: &mut DynamicMessage) {
    for (_, v) in m.fields_mut() {
        unmark_value(v);
    }
}

fn pick(r: &mut R, ws: &[i64]) -> usize {
    let tot: i64 = ws.iter().sum();
    let mut x = r.intn(tot);
    for (i, &w) in ws.iter().enumerate() {
        if x < w {
            return i;
        }
        x -= w;
    }
    ws.len() - 1
}

/// The canonical string form of every value at a field path (a repeated field on the way gives
/// one value per element; an unset field gives none).
pub fn extract(m: &DynamicMessage, path: &str) -> Vec<String> {
    let segs: Vec<&str> = path.split('.').collect();
    let mut out = Vec::new();
    walk(m, &segs, 0, &mut out);
    out
}

fn walk(m: &DynamicMessage, segs: &[&str], i: usize, out: &mut Vec<String>) {
    let md = m.descriptor();
    let Some(fd) = md.get_field_by_name(segs[i]) else {
        return;
    };
    let last = i == segs.len() - 1;
    if fd.is_map() {
        if i + 1 >= segs.len() {
            return;
        }
        let sel = segs[i + 1];
        let PKind::Message(entry) = fd.kind() else {
            return;
        };
        let (kfd, vfd) = (entry.map_entry_key_field(), entry.map_entry_value_field());
        if let Value::Map(mp) = &*m.get_field(&fd) {
            for (k, v) in mp {
                if sel == "key" {
                    let kv: Value = k.clone().into();
                    out.push(canonical(&kfd, &kv));
                } else if i + 2 < segs.len() && matches!(vfd.kind(), PKind::Message(_)) {
                    if let Value::Message(vm) = v {
                        walk(vm, segs, i + 2, out);
                    }
                } else if i + 2 == segs.len() {
                    out.push(canonical(&vfd, v));
                }
            }
        }
        out.sort(); // map order is random; the pool must not depend on it
        return;
    }
    if fd.is_list() {
        if let Value::List(l) = &*m.get_field(&fd) {
            for v in l {
                if last {
                    out.push(canonical(&fd, v));
                } else if let Value::Message(sub) = v {
                    walk(sub, segs, i + 1, out);
                }
            }
        }
        return;
    }
    if last {
        if fd.supports_presence() && !m.has_field(&fd) {
            return;
        }
        out.push(canonical(&fd, &m.get_field(&fd)));
        return;
    }
    if matches!(fd.kind(), PKind::Message(_)) && m.has_field(&fd) {
        if let Value::Message(sub) = &*m.get_field(&fd) {
            walk(sub, segs, i + 1, out);
        }
    }
}

/// Exposes `valuegen::decl_fields` for callers that walk messages.
pub use valuegen::decl_fields;
