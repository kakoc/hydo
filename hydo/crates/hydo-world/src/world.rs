//! The shared state of every fake and proxy. All decisions and records happen under one lock, in
//! the order requests arrive, so a deterministic guest gives a deterministic record log. Also the
//! webhooks, the control commands, the power cut (carrying the world into the recovery boot) and
//! the console summary.

use crate::config::{render, Config, Endpoint, Route};
use crate::goish::{esc, unesc, unix_milli};
use crate::http1;
use crate::rt::Latch;
use crate::rules::{GrpcExt, Rule};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub struct World {
    pub cfg: Config,
    pub log_path: String,
    st: Mutex<St>,
}

pub struct St {
    pub seq: i64,
    logf: Option<File>,
    pub rules: Vec<Arc<Rule>>,
    /// "<endpoint>/<route>/<idempotency key>" -> seq of the first effect
    pub idem: BTreeMap<Vec<u8>, i64>,
    /// webhooks not delivered yet (carried across a power cut)
    pending: BTreeMap<i64, Hook>,
    /// reorder: the webhook held back per "<fake>.<route>"
    held: BTreeMap<String, Hook>,
    hook_n: i64,
    pub frozen: bool,
    /// fired (and replaced) at every heal: ends held (dropped) requests
    pub heal: Arc<Latch>,
    /// the carried log of a power cut did not fit (checks cannot decide)
    trunc: bool,
    /// gRPC proxies: "<proxy> <path>" -> calls so far
    pub call_n: BTreeMap<String, i64>,
}

#[derive(Clone, Debug)]
pub struct Hook {
    id: i64,
    /// index into cfg.fakes, and of the route there
    ep: usize,
    rt: usize,
    r#ref: Vec<u8>,
    idem: Vec<u8>,
    /// seq of the request that caused it
    src: i64,
    late_ms: i64,
    dup: bool,
    lost: bool,
}

/// `k=<esc(v)>`
pub fn kv(k: &str, v: &[u8]) -> String {
    format!("{k}={}", esc(v))
}

impl St {
    pub fn record(&mut self, kind: &str, fields: &[String]) -> i64 {
        self.seq += 1;
        let line = format!(
            "seq={} t={} kind={kind} {}\n",
            self.seq,
            unix_milli(),
            fields.join(" ")
        );
        self.write_log(line.as_bytes());
        self.seq
    }

    fn write_log(&mut self, b: &[u8]) {
        if let Some(f) = &mut self.logf {
            let _ = f.write_all(b);
        }
    }

    /// The first active request rule for (endpoint, route) that fires for this request. Every
    /// matching rule is rolled (in order), so one rule's decisions never depend on another's.
    pub fn request_rule(&self, ep: &str, route: &str) -> Option<Arc<Rule>> {
        let mut hit = None;
        for r in &self.rules {
            if r.request_rule() && r.matches(ep, route) && r.fires() && hit.is_none() {
                hit = Some(r.clone());
            }
        }
        hit
    }

    /// The first active rule for a gRPC call (like request_rule, with the gRPC filters). Every
    /// rule whose target, route, rpc= and call= fit is rolled, in order.
    pub fn grpc_rule(
        &self,
        ep: &str,
        route: &str,
        path: &[u8],
        call: i64,
    ) -> Option<(Arc<Rule>, GrpcExt)> {
        let mut hit = None;
        for r in &self.rules {
            let x = r.ext.as_ref();
            if !(r.request_rule() || GrpcExt::stream_rule(x)) || !r.matches(ep, route) {
                continue;
            }
            if let Some(x) = x {
                if !rpc_match(&x.rpc, path)
                    || (x.call_lo > 0 && (call < x.call_lo || call > x.call_hi))
                {
                    continue;
                }
            }
            if r.fires() && hit.is_none() {
                hit = Some((r.clone(), x.cloned().unwrap_or_default()));
            }
        }
        hit
    }
}

pub fn rpc_match(want: &str, path: &[u8]) -> bool {
    if want.is_empty() {
        return true;
    }
    if want.starts_with('/') {
        return path == want.as_bytes();
    }
    path.ends_with(format!("/{want}").as_bytes()) || path.ends_with(format!(".{want}").as_bytes())
}

/// What a power cut may carry (the recovery boot gets it on its command line, which shares the
/// 4 KiB serial input with the acknowledged ids).
pub const CARRY_MAX: usize = 3000;

/// At most this many summary lines (the guest console is 64 KiB).
const SUMMARY_MAX: usize = 150;

pub type Rec = BTreeMap<String, Vec<u8>>;

/// "k=v k=v ..." -> map (values unescaped); None for a line that is not a record.
pub fn parse_record(ln: &[u8]) -> Option<Rec> {
    if !ln.starts_with(b"seq=") {
        return None;
    }
    let mut m = Rec::new();
    for f in ln
        .split(|c| c.is_ascii_whitespace())
        .filter(|f| !f.is_empty())
    {
        let (k, v) = match f.iter().position(|&c| c == b'=') {
            Some(p) => (&f[..p], &f[p + 1..]),
            None => (f, &b""[..]),
        };
        m.insert(String::from_utf8_lossy(k).into_owned(), unesc(v));
    }
    Some(m)
}

pub fn get<'a>(r: &'a Rec, k: &str) -> &'a [u8] {
    r.get(k).map(Vec::as_slice).unwrap_or(&[])
}

impl World {
    pub fn new(cfg: Config, log_path: &str, logf: Option<File>) -> Arc<World> {
        Arc::new(World {
            cfg,
            log_path: log_path.to_string(),
            st: Mutex::new(St {
                seq: 0,
                logf,
                rules: Vec::new(),
                idem: BTreeMap::new(),
                pending: BTreeMap::new(),
                held: BTreeMap::new(),
                hook_n: 0,
                frozen: false,
                heal: Latch::new(),
                trunc: false,
                call_n: BTreeMap::new(),
            }),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A request that arrives after the world was frozen (the system under test lost power)
    /// never gets anywhere. Returns with the lock held.
    pub async fn lock_gate(&self) -> MutexGuard<'_, St> {
        {
            let g = self.lock();
            if !g.frozen {
                return g;
            }
        }
        std::future::pending::<()>().await;
        unreachable!()
    }

    pub fn fake_index(&self, ep: &Endpoint) -> Option<usize> {
        self.cfg.fakes.iter().position(|e| std::ptr::eq(e, ep))
    }

    /// The webhook(s) an effect on rt triggers, with the webhook rules applied. Lock held.
    pub fn new_hooks(
        self: &Arc<Self>,
        st: &mut St,
        ep: usize,
        rt: usize,
        r#ref: &[u8],
        idem: &[u8],
        src: i64,
    ) {
        let e = &self.cfg.fakes[ep];
        let route = &e.routes[rt];
        let mut h = Hook {
            id: 0,
            ep,
            rt,
            r#ref: r#ref.to_vec(),
            idem: idem.to_vec(),
            src,
            late_ms: 0,
            dup: false,
            lost: false,
        };
        let mut reorder = false;
        for r in &st.rules {
            if r.hook_rule() && r.matches(&e.name, &route.name) && r.fires() {
                h.dup = h.dup || r.dup;
                h.lost = h.lost || r.lost;
                reorder = reorder || r.reorder;
                if r.late_ms > h.late_ms {
                    h.late_ms = r.late_ms;
                }
            }
        }
        st.hook_n += 1;
        h.id = st.hook_n;
        if h.lost {
            st.record(
                "hook",
                &[
                    kv("ep", e.name.as_bytes()),
                    kv("route", route.name.as_bytes()),
                    kv("ref", r#ref),
                    kv("src", src.to_string().as_bytes()),
                    "out=lost".into(),
                ],
            );
            return;
        }
        st.pending.insert(h.id, h.clone());
        let key = format!("{}.{}", e.name, route.name);
        if reorder {
            if let Some(prev) = st.held.remove(&key) {
                // deliver this one first, then the one held back
                let w = self.clone();
                tokio::spawn(async move {
                    w.clone().deliver(h, Duration::ZERO).await;
                    w.deliver(prev, Duration::ZERO).await;
                });
            } else {
                st.held.insert(key, h);
            }
            return;
        }
        tokio::spawn(self.clone().deliver(h.clone(), Duration::ZERO));
        if h.dup {
            st.hook_n += 1;
            let mut d = h;
            d.id = st.hook_n;
            st.pending.insert(d.id, d.clone());
            tokio::spawn(self.clone().deliver(d, Duration::ZERO));
        }
    }

    fn flush_held(self: &Arc<Self>, st: &mut St, target: &str) {
        let keys: Vec<String> = st.held.keys().cloned().collect();
        for k in keys {
            if target.is_empty() || k == target || k.starts_with(&format!("{target}.")) {
                if let Some(h) = st.held.remove(&k) {
                    tokio::spawn(self.clone().deliver(h, Duration::ZERO));
                }
            }
        }
    }

    /// POST the webhook (after its delay), retrying a failed delivery with backoff as a payment
    /// provider does. A frozen world (power cut) leaves it pending: it is carried.
    async fn deliver(self: Arc<Self>, h: Hook, extra: Duration) {
        let e = &self.cfg.fakes[h.ep];
        let route = &e.routes[h.rt];
        let Some(wh) = &route.webhook else { return };
        let delay = (wh.delay_ms + h.late_ms).max(0) as u64;
        tokio::time::sleep(Duration::from_millis(delay) + extra).await;
        let seq = h.src.to_string();
        let ctx: [(&str, &[u8]); 5] = [
            ("seq", seq.as_bytes()),
            ("ref", &h.r#ref),
            ("idem", &h.idem),
            ("fake", e.name.as_bytes()),
            ("route", route.name.as_bytes()),
        ];
        let body = render(&wh.body, &ctx, None);
        let mut back = Duration::from_millis(500);
        let mut attempt: i64 = 1;
        loop {
            if self.lock().frozen {
                return;
            }
            let url = render(&wh.url, &ctx, None);
            let mut out = "err".to_string();
            let mut ok = false;
            if let Some((host, target)) = http1::split_url(&url) {
                let mut hs = http1::Hdrs::new();
                for (k, v) in &wh.headers {
                    http1::hset(&mut hs, k, render(v, &ctx, None));
                }
                if http1::hget(&hs, "Content-Type").is_none_or(<[u8]>::is_empty) {
                    http1::hset(&mut hs, "Content-Type", b"application/json".to_vec());
                }
                let req = http1::Req {
                    method: &wh.method,
                    host: &host,
                    target: &target,
                    headers: &hs,
                    body: &body,
                };
                if let Ok(r) = http1::roundtrip_timeout(&req, false, Duration::from_secs(5)).await {
                    out = r.status.to_string();
                    ok = r.status / 100 == 2;
                }
            }
            let done;
            {
                let mut st = self.lock();
                if st.frozen {
                    return;
                }
                st.record(
                    "hook",
                    &[
                        kv("ep", e.name.as_bytes()),
                        kv("route", route.name.as_bytes()),
                        kv("ref", &h.r#ref),
                        kv("idem", &h.idem),
                        format!("src={}", h.src),
                        format!("n={attempt}"),
                        format!("dup={}", if h.dup { 1 } else { 0 }),
                        format!("out={out}"),
                    ],
                );
                done = ok || attempt > wh.retries;
                if done {
                    st.pending.remove(&h.id);
                    if !ok {
                        st.record(
                            "hook",
                            &[
                                kv("ep", e.name.as_bytes()),
                                kv("route", route.name.as_bytes()),
                                kv("ref", &h.r#ref),
                                format!("src={}", h.src),
                                "out=gave-up".into(),
                            ],
                        );
                    }
                }
            }
            if done {
                return;
            }
            tokio::time::sleep(back).await;
            back *= 2;
            attempt += 1;
        }
    }

    // -------------------------------------------------------------- control commands

    pub fn command(self: &Arc<Self>, f: &[&[u8]]) -> Vec<u8> {
        let mut st = self.lock();
        let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        match f[0] {
            b"ping" => b"ok\n".to_vec(),
            b"on" => {
                if f.len() != 3 {
                    return b"error: on <id> <token>\n".to_vec();
                }
                let id = s(f[1]).parse::<i64>().unwrap_or(0);
                let r = match Rule::parse(id, &s(f[2])) {
                    Ok(r) => r,
                    Err(e) => return format!("error: {e}\n").into_bytes(),
                };
                let Some(e) = self.cfg.endpoint(&r.target) else {
                    return format!("error: no fake or proxy {}\n", r.target).into_bytes();
                };
                if GrpcExt::grpc_only(r.ext.as_ref())
                    && !(e.kind == "proxy" && e.protocol == "grpc")
                {
                    return format!(
                        "error: rpc=, call=, cut=, hold=, dir= and reset=conn need a gRPC proxy ({} is not one)\n",
                        r.target
                    )
                    .into_bytes();
                }
                st.rules.push(Arc::new(r));
                b"ok\n".to_vec()
            }
            b"off" => {
                if f.len() != 2 {
                    return b"error: off <id>\n".to_vec();
                }
                let id = s(f[1]).parse::<i64>().unwrap_or(0);
                let rules = std::mem::take(&mut st.rules);
                let mut keep = Vec::new();
                for r in rules {
                    if r.id == id {
                        r.release.fire();
                        if r.reorder {
                            let mut t = r.target.clone();
                            if !r.route.is_empty() {
                                t = format!("{t}.{}", r.route);
                            }
                            self.flush_held(&mut st, &t);
                        }
                    } else {
                        keep.push(r);
                    }
                }
                st.rules = keep;
                b"ok\n".to_vec()
            }
            b"heal" => {
                for r in &st.rules {
                    r.release.fire();
                }
                st.rules.clear();
                self.flush_held(&mut st, "");
                st.heal.fire();
                st.heal = Latch::new();
                b"ok\n".to_vec()
            }
            b"event" => {
                if f.len() != 3 {
                    return b"error: event <name> <ref>\n".to_vec();
                }
                let n = st.record(
                    "event",
                    &[
                        kv("ep", b"-"),
                        kv("route", &unesc(f[1])),
                        kv("ref", &unesc(f[2])),
                        "fx=1".into(),
                    ],
                );
                format!("ok {n}\n").into_bytes()
            }
            b"dump" => {
                st.frozen = true;
                self.carry(&st).into_bytes()
            }
            b"summary" => self.summary(),
            other => format!("error: unknown command {}\n", s(other)).into_bytes(),
        }
    }

    // -------------------------------------------------------------- power cut: carry the world

    /// The records the side-effect checks and the recovery world need, as w: tokens in log
    /// order -- effects (e), events (v), webhooks not delivered yet (p).
    fn carry(&self, st: &St) -> String {
        let Ok(b) = std::fs::read(&self.log_path) else {
            return "w:trunc\nEND\n".into();
        };
        let mut out: Vec<String> = Vec::new();
        for ln in b.split(|&c| c == b'\n') {
            let Some(r) = parse_record(ln) else { continue };
            let kind = get(&r, "kind");
            if get(&r, "fx") != b"1" || (kind != b"req" && kind != b"event") {
                continue;
            }
            let k = if kind == b"event" { "v" } else { "e" };
            out.push(format!(
                "w:{},{k},{},{},{},{}",
                String::from_utf8_lossy(get(&r, "seq")),
                esc(get(&r, "ep")),
                esc(get(&r, "route")),
                esc(get(&r, "ref")),
                esc(get(&r, "idem"))
            ));
        }
        for h in st.pending.values() {
            let e = &self.cfg.fakes[h.ep];
            out.push(format!(
                "w:{},p,{},{},{},{}",
                h.src,
                esc(e.name.as_bytes()),
                esc(e.routes[h.rt].name.as_bytes()),
                esc(&h.r#ref),
                esc(&h.idem)
            ));
        }
        let n: usize = out.iter().map(|s| s.len() + 1).sum();
        if n > CARRY_MAX || st.trunc {
            return "w:trunc\nEND\n".into();
        }
        out.push("END".into());
        out.join("\n") + "\n"
    }

    /// The recovery boot's world starts from what the first boot carried: its effects and events
    /// (so the checks see both boots), its idempotency memory, and its pending webhooks.
    pub fn load_carry(self: &Arc<Self>, path: &str) -> Result<(), String> {
        let b = std::fs::read(path).map_err(|e| crate::goish::path_err("open", path, &e))?;
        let mut st = self.lock();
        let mut hooks = Vec::new();
        for tok in b
            .split(|c| c.is_ascii_whitespace())
            .filter(|t| !t.is_empty())
        {
            if tok == b"w:trunc" {
                st.trunc = true;
                continue;
            }
            let Some(body) = tok.strip_prefix(b"w:") else {
                continue;
            };
            let f: Vec<&[u8]> = body.split(|&c| c == b',').collect();
            if f.len() != 6 {
                continue;
            }
            let seq = std::str::from_utf8(f[0])
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let (ep, route, r#ref, idem) = (unesc(f[2]), unesc(f[3]), unesc(f[4]), unesc(f[5]));
            if seq > st.seq {
                st.seq = seq;
            }
            match f[1] {
                b"e" => {
                    let line = format!(
                        "seq={seq} t=0 kind=req {} {} {} {} fx=1 out=carried\n",
                        kv("ep", &ep),
                        kv("route", &route),
                        kv("ref", &r#ref),
                        kv("idem", &idem)
                    );
                    st.write_log(line.as_bytes());
                    if !idem.is_empty() {
                        let mut key = ep.clone();
                        key.push(b'/');
                        key.extend_from_slice(&route);
                        key.push(b'/');
                        key.extend_from_slice(&idem);
                        st.idem.insert(key, seq);
                    }
                }
                b"v" => {
                    let line = format!(
                        "seq={seq} t=0 kind=event ep=- {} {} fx=1 out=carried\n",
                        kv("route", &route),
                        kv("ref", &r#ref)
                    );
                    st.write_log(line.as_bytes());
                }
                b"p" => {
                    let epn = String::from_utf8_lossy(&ep);
                    let rn = String::from_utf8_lossy(&route);
                    // the first endpoint of that name (Go: cfg.endpoint), a fake with that route
                    let Some(e) = self.cfg.endpoint(&epn) else {
                        continue;
                    };
                    let Some(ri) = e.routes.iter().position(|r| r.name == rn) else {
                        continue;
                    };
                    if e.routes[ri].webhook.is_none() {
                        continue;
                    }
                    let Some(ei) = self.fake_index(e) else {
                        continue;
                    };
                    st.hook_n += 1;
                    hooks.push(Hook {
                        id: st.hook_n,
                        ep: ei,
                        rt: ri,
                        r#ref,
                        idem,
                        src: seq,
                        late_ms: 0,
                        dup: false,
                        lost: false,
                    });
                }
                _ => {}
            }
        }
        if st.trunc {
            st.write_log(b"seq=0 t=0 kind=meta trunc=1\n");
        }
        for h in hooks {
            st.pending.insert(h.id, h.clone());
            tokio::spawn(self.clone().deliver(h, Duration::from_secs(1))); // the provider retries after the outage
        }
        Ok(())
    }

    /// What the outside world saw, one short line per request, webhook and event (at most
    /// SUMMARY_MAX lines), for the console at the end of a run.
    fn summary(&self) -> Vec<u8> {
        let Ok(b) = std::fs::read(&self.log_path) else {
            return b"END\n".to_vec();
        };
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut n = 0usize;
        for ln in b.split(|&c| c == b'\n') {
            let Some(r) = parse_record(ln) else { continue };
            if get(&r, "kind") == b"meta" {
                continue;
            }
            n += 1;
            if out.len() == SUMMARY_MAX {
                continue;
            }
            let g = |k: &str| get(&r, k).to_vec();
            let e = |k: &str| esc(get(&r, k)).into_bytes();
            let parts: Vec<Vec<u8>> = match get(&r, "kind") {
                b"req" => vec![
                    g("seq"),
                    b" ".to_vec(),
                    g("ep"),
                    b".".to_vec(),
                    g("route"),
                    b" ref=".to_vec(),
                    e("ref"),
                    b" idem=".to_vec(),
                    e("idem"),
                    b" fx=".to_vec(),
                    g("fx"),
                    b" ".to_vec(),
                    g("out"),
                ],
                b"hook" => vec![
                    g("seq"),
                    b" webhook ".to_vec(),
                    g("ep"),
                    b".".to_vec(),
                    g("route"),
                    b" ref=".to_vec(),
                    e("ref"),
                    b" src=".to_vec(),
                    g("src"),
                    b" n=".to_vec(),
                    g("n"),
                    b" ".to_vec(),
                    g("out"),
                ],
                b"event" => vec![
                    g("seq"),
                    b" event ".to_vec(),
                    g("route"),
                    b" ref=".to_vec(),
                    e("ref"),
                ],
                b"call" => vec![crate::grpc::call_summary(&r)],
                _ => vec![],
            };
            out.push(parts.concat());
        }
        if n > out.len() {
            out.push(format!("... {} more records", n - out.len()).into_bytes());
        }
        out.push(b"END".to_vec());
        let mut s = out.join(&b"\n"[..]);
        s.push(b'\n');
        s
    }
}

/// For the handlers: the route of a fake endpoint by reference, as an index.
pub fn route_index(e: &Endpoint, r: &Route) -> Option<usize> {
    e.routes.iter().position(|x| std::ptr::eq(x, r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Webhook;

    fn test_world(dir: &std::path::Path) -> Arc<World> {
        let cfg = Config {
            fakes: vec![Endpoint {
                kind: "fake".into(),
                name: "payments".into(),
                routes: vec![Route {
                    name: "charge".into(),
                    webhook: Some(Webhook {
                        url: "http://127.0.0.1:1/x".into(),
                        retries: 1,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = dir.join("world.log");
        let f = File::create(&p).unwrap();
        World::new(cfg, &p.to_string_lossy(), Some(f))
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-world-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A power cut carries effects, events and pending webhooks; the recovery world sees them, in
    /// order, and remembers the idempotency keys.
    #[test]
    fn carry_round_trip() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = tmpdir("carry");
            let w = test_world(&dir);
            {
                let mut st = w.lock();
                st.record("req", &[kv("ep", b"payments"), kv("route", b"charge"), kv("ref", b"o 1"), kv("idem", b"k1"), "fx=1".into(), "out=ok".into()]);
                st.record("req", &[kv("ep", b"payments"), kv("route", b"charge"), kv("ref", b"o2"), kv("idem", b"k2"), "fx=0".into(), "out=-".into()]);
                st.record("event", &[kv("ep", b"-"), kv("route", b"cancelled"), kv("ref", b"o 1"), "fx=1".into()]);
                st.pending.insert(
                    1,
                    Hook { id: 1, ep: 0, rt: 0, r#ref: b"o 1".to_vec(), idem: b"k1".to_vec(), src: 1, late_ms: 0, dup: false, lost: false },
                );
            }
            let out = w.carry(&w.lock());
            assert!(out.ends_with("END\n") && out.matches("w:").count() == 3 && !out.contains("o2"), "carry: {out:?}");
            assert_eq!(out, "w:1,e,payments,charge,o%201,k1\nw:3,v,-,cancelled,o%201,-\nw:1,p,payments,charge,o%201,k1\nEND\n");
            let cf = dir.join("carry");
            std::fs::write(&cf, out.trim_end_matches("END\n")).unwrap();
            let dir2 = tmpdir("carry2");
            let w2 = test_world(&dir2);
            w2.lock().frozen = true; // no webhook delivery attempt in the test
            w2.load_carry(&cf.to_string_lossy()).unwrap();
            {
                let st = w2.lock();
                assert_eq!(st.seq, 3);
                assert_eq!(st.idem.get(&b"payments/charge/k1"[..]), Some(&1));
                assert_eq!(st.pending.len(), 1);
            }
            let b = std::fs::read_to_string(&w2.log_path).unwrap();
            assert!(b.contains("ref=o%201") && b.contains("kind=event"), "recovery log: {b}");
        });
    }

    #[test]
    fn carry_too_long() {
        let dir = tmpdir("long");
        let w = test_world(&dir);
        for i in 0..200 {
            w.lock().record(
                "req",
                &[
                    kv("ep", b"payments"),
                    kv("route", b"charge"),
                    kv("ref", format!("order-{i}").as_bytes()),
                    "fx=1".into(),
                    "out=ok".into(),
                ],
            );
        }
        assert_eq!(w.carry(&w.lock()), "w:trunc\nEND\n");
    }

    #[test]
    fn summary_lines() {
        let dir = tmpdir("summary");
        let w = test_world(&dir);
        std::fs::write(
            &w.log_path,
            "seq=1 t=5 kind=req ep=payments route=charge from=127.0.0.1 method=POST path=/v1/charges ref=o%201 idem=k fx=1 out=ok+delay3000 rule=1\n\
             seq=0 t=0 kind=meta trunc=1\n\
             seq=2 t=6 kind=hook ep=payments route=charge ref=o1 idem=k src=1 n=1 dup=0 out=200\n\
             seq=3 t=7 kind=event ep=- route=cancelled ref=o1 fx=1\n\
             seq=4 t=8 kind=other\n",
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(w.summary()).unwrap(),
            "1 payments.charge ref=o%201 idem=k fx=1 ok+delay3000\n2 webhook payments.charge ref=o1 src=1 n=1 200\n3 event cancelled ref=o1\n\nEND\n"
        );
    }
}
