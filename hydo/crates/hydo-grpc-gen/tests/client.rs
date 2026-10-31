//! The guest client against an in-process orders server (the Go client tests of Milestone 91,
//! ported): deterministic steps, retries of unknown outcomes with the same request, the end-of-run
//! retry, composer drives and scenarios, field extraction, and reflection.

use bytes::{Buf, Bytes, BytesMut};
use hydo_grpc::client::{extract, Client, Options, WorkerEnv};
use hydo_grpc::rng::R;
use hydo_grpc_gen::{desc, load};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MapKey, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The tests share environment variables (HYDO_OUTCOMES, HYDO_SEED, ...): one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn demo_set() -> Vec<u8> {
    let (set, _) = desc::from_proto_dir(&repo().join("hydo/grpcgen/demo/proto"), &[]).unwrap();
    desc::encode_set(&set)
}

fn pool() -> DescriptorPool {
    DescriptorPool::decode(demo_set().as_slice()).unwrap()
}

#[derive(Default)]
struct Fake {
    by_req: HashMap<String, DynamicMessage>,
    by_id: HashMap<String, String>,
    slow: HashMap<String, i32>,
    down: bool,
    n: i32,
    seen: Vec<String>,
}

type Shared = Arc<Mutex<Fake>>;

fn frame(m: &[u8]) -> Bytes {
    let mut f = BytesMut::new();
    f.extend_from_slice(&[0]);
    f.extend_from_slice(&(m.len() as u32).to_be_bytes());
    f.extend_from_slice(m);
    f.freeze()
}

fn status(code: u32, msg: &str) -> Result<Vec<u8>, (u32, String)> {
    Err((code, msg.to_string()))
}

fn sv(m: &DynamicMessage, f: &str) -> String {
    m.get_field_by_name(f)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn handle(
    f: Shared,
    pool: DescriptorPool,
    set: Vec<u8>,
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
) {
    let path = req.uri().path().to_string();
    let mut body = req.into_body();
    let mut buf = BytesMut::new();
    while let Some(Ok(d)) = body.data().await {
        let _ = body.flow_control().release_capacity(d.len());
        buf.extend_from_slice(&d);
    }
    let mut msgs = Vec::new();
    while buf.len() >= 5 {
        let n = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        buf.advance(5);
        msgs.push(buf.split_to(n).to_vec());
    }
    let method = path.rsplit('/').next().unwrap_or("").to_string();
    let answers: Vec<Result<Vec<u8>, (u32, String)>> = if path.starts_with("/grpc.reflection.v1.") {
        msgs.iter().map(|m| Ok(reflect_answer(m, &set))).collect()
    } else {
        let input = pool
            .get_service_by_name("orders.v1.Orders")
            .unwrap()
            .methods()
            .find(|x| x.name() == method)
            .map(|x| x.input());
        let Some(input) = input else {
            let _ = respond.send_response(trailers_only(12, "unknown method"), true);
            return;
        };
        let m =
            DynamicMessage::decode(input, msgs.first().map(Vec::as_slice).unwrap_or(&[])).unwrap();
        vec![orders(&f, &pool, &method, &m).await]
    };
    let mut ok = Vec::new();
    for a in answers {
        match a {
            Ok(b) => ok.push(b),
            Err((code, msg)) => {
                let _ = respond.send_response(trailers_only(code, &msg), true);
                return;
            }
        }
    }
    let resp = http::Response::builder()
        .status(200)
        .header("content-type", "application/grpc")
        .body(())
        .unwrap();
    let Ok(mut send) = respond.send_response(resp, false) else {
        return;
    };
    for b in ok {
        let _ = send.send_data(frame(&b), false);
    }
    let mut t = http::HeaderMap::new();
    t.insert("grpc-status", "0".parse().unwrap());
    let _ = send.send_trailers(t);
}

fn trailers_only(code: u32, msg: &str) -> http::Response<()> {
    http::Response::builder()
        .status(200)
        .header("content-type", "application/grpc")
        .header("grpc-status", code.to_string())
        .header("grpc-message", msg)
        .body(())
        .unwrap()
}

async fn orders(
    f: &Shared,
    pool: &DescriptorPool,
    method: &str,
    m: &DynamicMessage,
) -> Result<Vec<u8>, (u32, String)> {
    let order_md = pool.get_message_by_name("orders.v1.Order").unwrap();
    match method {
        "Create" => {
            let rid = sv(m, "request_id");
            let slow = {
                let mut g = f.lock().unwrap();
                g.seen.push(rid.clone());
                if g.down {
                    return status(14, "down");
                }
                match g.slow.get_mut(&rid) {
                    Some(n) if *n > 0 => {
                        *n -= 1;
                        true
                    }
                    _ => false,
                }
            };
            if slow {
                tokio::time::sleep(Duration::from_millis(700)).await;
                return status(1, "context canceled");
            }
            let mut g = f.lock().unwrap();
            if let Some(o) = g.by_req.get(&rid) {
                return Ok(o.encode_to_vec());
            }
            g.n += 1;
            let id = format!("o{}", g.n);
            let mut o = DynamicMessage::new(order_md);
            o.set_field_by_name("id", Value::String(id.clone()));
            o.set_field_by_name("request_id", Value::String(rid.clone()));
            o.set_field_by_name(
                "customer",
                m.get_field_by_name("customer").unwrap().into_owned(),
            );
            o.set_field_by_name(
                "amount_cents",
                m.get_field_by_name("amount_cents").unwrap().into_owned(),
            );
            o.set_field_by_name("status", Value::EnumNumber(1));
            g.by_req.insert(rid.clone(), o.clone());
            g.by_id.insert(id, rid);
            Ok(o.encode_to_vec())
        }
        "Get" | "Cancel" => {
            let id = sv(m, "id");
            let mut g = f.lock().unwrap();
            let Some(rid) = g.by_id.get(&id).cloned() else {
                return status(5, &format!("no order {id}"));
            };
            let o = g.by_req.get_mut(&rid).unwrap();
            if method == "Cancel" {
                o.set_field_by_name("status", Value::EnumNumber(2));
            }
            Ok(o.encode_to_vec())
        }
        _ => Ok(Vec::new()),
    }
}

/// A ServerReflectionResponse for one request: the services, or the one file asked for (by
/// symbol: the demo's file; by name: that file).
fn reflect_answer(req: &[u8], set: &[u8]) -> Vec<u8> {
    let files = desc::split_set(set).unwrap();
    let (mut num, mut val) = (0u32, String::new());
    let mut i = 0;
    while i < req.len() {
        let key = desc::get_varint(req, &mut i).unwrap();
        let n = desc::get_varint(req, &mut i).unwrap() as usize;
        num = (key >> 3) as u32;
        val = String::from_utf8_lossy(&req[i..i + n]).into_owned();
        i += n;
    }
    let mut inner = Vec::new();
    let mut out = Vec::new();
    match num {
        7 => {
            for s in ["orders.v1.Orders", "grpc.reflection.v1.ServerReflection"] {
                let mut svc = Vec::new();
                desc::put_len(&mut svc, 1, s.as_bytes());
                desc::put_len(&mut inner, 1, &svc);
            }
            desc::put_len(&mut out, 6, &inner);
        }
        _ => {
            let want = if num == 4 {
                "orders/v1/orders.proto".to_string()
            } else {
                val
            };
            for f in files.iter().filter(|f| f.name == want) {
                desc::put_len(&mut inner, 1, &f.bytes);
            }
            desc::put_len(&mut out, 4, &inner);
        }
    }
    out
}

/// Starts the server on a thread of its own; returns its address.
fn serve(f: Shared) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    l.set_nonblocking(true).unwrap();
    let set = demo_set();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::from_std(l).unwrap();
            let pool = DescriptorPool::decode(set.as_slice()).unwrap();
            loop {
                let Ok((sock, _)) = l.accept().await else {
                    continue;
                };
                let (f, pool, set) = (f.clone(), pool.clone(), set.clone());
                tokio::spawn(async move {
                    let Ok(mut conn) = h2::server::handshake(sock).await else {
                        return;
                    };
                    while let Some(Ok((req, respond))) = conn.accept().await {
                        tokio::spawn(handle(f.clone(), pool.clone(), set.clone(), req, respond));
                    }
                });
            }
        });
    });
    addr
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydo-grpc-client-{}-{}-{:?}",
        std::process::id(),
        name,
        std::time::Instant::now()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// desc.pb + plan.json for the demo schema and hints (plus `extra`), pointed at addr, with short
/// deadlines and backoffs.
fn setup(addr: &str, extra: &str) -> PathBuf {
    let dir = tmp("plan");
    let hints = std::fs::read_to_string(repo().join("hydo/grpcgen/demo/hints.yaml")).unwrap();
    let hp = dir.join("hints.yaml");
    std::fs::write(&hp, format!("{hints}{extra}")).unwrap();
    let mut h = load::read_hints(hp.to_str().unwrap()).unwrap();
    h.target = addr.to_string();
    h.deadline_ms = 300;
    for r in h.rpcs.values_mut() {
        r.deadline_ms = 300;
        if let Some(x) = r.retry.as_mut() {
            x.backoff_ms = 10;
        }
    }
    let p = load::build(&pool(), &h, None).unwrap();
    std::fs::write(dir.join("desc.pb"), demo_set()).unwrap();
    std::fs::write(dir.join("plan.json"), p.marshal()).unwrap();
    dir
}

fn client(dir: &Path, seed: u64) -> (Arc<Client>, PathBuf) {
    let st = tmp("state");
    std::env::set_var("HYDO_GRPC_CONSOLE", "off");
    std::env::set_var("HYDO_OUTCOMES", st.join("outcomes"));
    std::env::set_var("HYDO_ACKED", st.join("acked"));
    let c = Client::new(Options {
        desc: dir.join("desc.pb").display().to_string(),
        plan: dir.join("plan.json").display().to_string(),
        state_dir: st.display().to_string(),
        calls: String::new(),
        seed: Some(seed),
    })
    .unwrap();
    (Arc::new(c), st)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// The calls record with the time field masked.
fn calls(st: &Path) -> Vec<String> {
    let b = std::fs::read_to_string(st.join("calls")).unwrap_or_default();
    b.trim()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.splitn(6, ' ').collect();
            if f.len() == 6 {
                format!("{} {} {} {} T {}", f[0], f[1], f[2], f[3], f[5])
            } else {
                l.to_string()
            }
        })
        .collect()
}

fn fake() -> Shared {
    Arc::new(Mutex::new(Fake::default()))
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Two runs with the same seed against equal servers send the same requests in the same order
/// and record the same outcomes; another seed sends other requests.
#[test]
fn steps_deterministic() {
    let _g = lock();
    std::env::remove_var("HYDO_SEED");
    let mut runs = Vec::new();
    for seed in [11, 11, 12] {
        let addr = serve(fake());
        let (c, st) = client(&setup(&addr, ""), seed);
        rt().block_on(c.steps(0, 12));
        runs.push(calls(&st));
    }
    assert_eq!(runs[0], runs[1], "same seed, different calls");
    assert_ne!(runs[0], runs[2], "different seeds, same calls");
    let all = runs[0].join("\n");
    for want in [
        "invoke T orders.v1.Orders/Create",
        "ok T orders.v1.Orders/Create 1 OK",
        "invoke T orders.v1.Orders/Cancel",
    ] {
        assert!(all.contains(want), "no {want:?} in\n{all}");
    }
    // Cancel and Get only ever use ids an earlier Create returned
    for l in &runs[0] {
        if l.contains("invoke T orders.v1.Orders/Cancel")
            || l.contains("invoke T orders.v1.Orders/Get")
        {
            assert!(
                l.contains("%22id%22:%22o") || l.contains("{\"id\":\"o"),
                "not an id from Create: {l}"
            );
        }
    }
}

/// An unknown outcome is retried with the same request under the same op; definite failures are
/// not retried; the outcome files get one line per attempt.
#[test]
fn retry_and_outcomes() {
    let _g = lock();
    let f = fake();
    f.lock().unwrap().slow.insert("r1".into(), 2); // two attempts time out, the third succeeds
    let addr = serve(f);
    let (c, st) = client(&setup(&addr, ""), 1);
    let ri = c.plan.find("Create").unwrap();
    let r = c.plan.rpcs[ri].clone();
    let rt = rt();
    let reqs = c
        .generate(&r, &mut R::new(1), &mut WorkerEnv::new(&c))
        .unwrap();
    let res = rt.block_on(c.op(0, 1, &r, &reqs, Some(&mut WorkerEnv::new(&c))));
    assert_eq!(res.kind.as_str(), "ok");
    let cl = calls(&st);
    assert_eq!(cl.len(), 6, "{}", cl.join("\n"));
    assert!(
        cl[1].starts_with("HYDO-HIST grpc 0 info T orders.v1.Orders/Create 1 DEADLINE_EXCEEDED"),
        "{}",
        cl[1]
    );
    assert!(
        cl[5].starts_with("HYDO-HIST grpc 0 ok T orders.v1.Orders/Create 1 OK"),
        "{}",
        cl[5]
    );
    assert!(
        cl[0] == cl[2] && cl[2] == cl[4],
        "attempts with different requests"
    );
    assert_eq!(
        std::fs::read_to_string(st.join("outcomes")).unwrap(),
        "r1 unknown\nr1 unknown\nr1 ack\n"
    );
    assert_eq!(std::fs::read_to_string(st.join("acked")).unwrap(), "r1\n");
    // a definite failure: one attempt only
    let g = c.plan.rpcs[c.plan.find("Cancel").unwrap()].clone();
    let reqs = c.parse_json(&g, "{\"id\":\"o99\"}").unwrap();
    assert_eq!(
        rt.block_on(c.op(0, 2, &g, &reqs, None)).kind.as_str(),
        "fail"
    );
    assert_eq!(calls(&st).len(), 8, "definite failure retried");
}

/// `hydo-grpc retry` re-sends operations whose last outcome was unknown, with the same request,
/// after the faults heal.
#[test]
fn retry_at_end() {
    let _g = lock();
    let f = fake();
    f.lock().unwrap().down = true;
    let addr = serve(f.clone());
    let (c, st) = client(&setup(&addr, ""), 3);
    let rt = rt();
    rt.block_on(c.steps(0, 4));
    let before = f.lock().unwrap().seen.len();
    assert!(before > 0, "no Create was attempted");
    f.lock().unwrap().down = false;
    rt.block_on(c.retry(st.join("calls").to_str().unwrap()))
        .unwrap();
    let g = f.lock().unwrap();
    assert!(!g.by_req.is_empty(), "retry created nothing");
    let sent: Vec<&String> = g.seen[..before].iter().collect();
    for r in g.by_req.keys() {
        assert!(sent.contains(&r), "retry sent a new request_id {r}");
    }
    drop(g);
    assert!(std::fs::read_to_string(st.join("outcomes"))
        .unwrap()
        .contains(" ack\n"));
    // nothing left unknown -> a second retry sends nothing
    let n = f.lock().unwrap().seen.len();
    rt.block_on(c.retry(st.join("calls").to_str().unwrap()))
        .unwrap();
    assert_eq!(
        f.lock().unwrap().seen.len(),
        n,
        "second retry re-sent known outcomes"
    );
}

/// Reflection gives the same schema as the .proto files.
#[test]
fn reflection() {
    let _g = lock();
    let addr = serve(fake());
    let got = hydo_grpc_gen::reflect::from_reflection(&addr, Duration::from_secs(5)).unwrap();
    let (want, _) = desc::from_proto_dir(&repo().join("hydo/grpcgen/demo/proto"), &[]).unwrap();
    assert_eq!(
        got.iter().map(|f| &f.name).collect::<Vec<_>>(),
        want.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert_eq!(desc::encode_set(&got), desc::encode_set(&want));
    let pool = DescriptorPool::decode(desc::encode_set(&got).as_slice()).unwrap();
    let p = load::build(&pool, &load::Hints::default(), None).unwrap();
    assert_eq!(p.rpcs.len(), 4);
}

#[test]
fn extract_paths() {
    let pool = pool();
    let lr = pool.get_message_by_name("orders.v1.ListResponse").unwrap();
    let om = pool.get_message_by_name("orders.v1.Order").unwrap();
    let im = pool.get_message_by_name("orders.v1.Item").unwrap();
    let item = |s: &str| {
        let mut m = DynamicMessage::new(im.clone());
        m.set_field_by_name("sku", Value::String(s.into()));
        Value::Message(m)
    };
    let mut o1 = DynamicMessage::new(om.clone());
    o1.set_field_by_name("id", Value::String("o1".into()));
    o1.set_field_by_name("items", Value::List(vec![item("a"), item("b")]));
    let mut o2 = DynamicMessage::new(om);
    o2.set_field_by_name("id", Value::String("o2".into()));
    let mut m = DynamicMessage::new(lr);
    m.set_field_by_name(
        "orders",
        Value::List(vec![Value::Message(o1), Value::Message(o2)]),
    );
    assert_eq!(extract(&m, "orders.id").join(","), "o1,o2");
    assert_eq!(extract(&m, "orders.items.sku").join(","), "a,b");
    let mut cr = DynamicMessage::new(pool.get_message_by_name("orders.v1.CreateRequest").unwrap());
    let mut labels = HashMap::new();
    labels.insert(MapKey::String("z".into()), Value::String("1".into()));
    labels.insert(MapKey::String("a".into()), Value::String("2".into()));
    cr.set_field_by_name("labels", Value::Map(labels));
    assert_eq!(extract(&cr, "labels.key").join(","), "a,z");
    assert!(extract(&cr, "note").is_empty(), "unset optional extracted");
}

fn gen_drive(dir: &Path, seed: &str) -> String {
    let addr = serve(fake());
    std::env::set_var("HYDO_SEED", seed);
    // the same plan pointed at the new server
    let nd = tmp("plan2");
    let plan = std::fs::read_to_string(dir.join("plan.json")).unwrap();
    let mut out = String::new();
    for l in plan.lines() {
        if l.starts_with("  \"target\": ") {
            out.push_str(&format!("  \"target\": \"{addr}\",\n"));
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    std::fs::write(nd.join("plan.json"), out).unwrap();
    std::fs::copy(dir.join("desc.pb"), nd.join("desc.pb")).unwrap();
    let (c, st) = client(&nd, 9);
    rt().block_on(c.drive("Create", 2)).unwrap();
    calls(&st)
        .iter()
        .filter(|l| l.contains(" invoke "))
        .map(|l| l[l.find("orders.v1").unwrap()..].to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A composer command: N operations of one RPC under the OS process id; two invocations of one
/// run draw from different streams; a scenario passes the id its own Create returned to its
/// Cancel; HYDO_SEED decides the requests.
#[test]
fn drive_and_scenario() {
    let _g = lock();
    std::env::remove_var("HYDO_SEED");
    let f = fake();
    let addr = serve(f.clone());
    let dir = setup(
        &addr,
        "scenarios:\n  - {name: create_cancel, calls: [Create, Cancel]}\n",
    );
    let (c, st) = client(&dir, 5);
    let rt = rt();
    assert!(rt.block_on(c.drive("create", 3)).unwrap());
    std::env::set_var("HYDO_SEED", "77"); // under the composer: the entry's seed picks the stream
    assert!(rt.block_on(c.drive("Create", 3)).unwrap());
    let cl = calls(&st);
    assert_eq!(cl.len(), 12, "{}", cl.join("\n"));
    assert!(
        cl[0].starts_with(&format!(
            "HYDO-HIST grpc {} invoke T orders.v1.Orders/Create 1 ",
            std::process::id()
        )),
        "{}",
        cl[0]
    );
    assert_eq!(f.lock().unwrap().by_req.len(), 6, "unique request ids");
    rt.block_on(c.drive("create_cancel", 2)).unwrap();
    let cl = calls(&st)[12..].to_vec();
    for i in (0..cl.len()).step_by(4) {
        let created = cl[i + 1]
            .split("\"id\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        assert!(
            !created.is_empty() && cl[i + 2].contains(&format!("{{\"id\":\"{created}\"")),
            "scenario did not cancel the order it created:\n{}",
            cl.join("\n")
        );
    }
    assert!(
        rt.block_on(c.drive("nope", 1)).is_err(),
        "unknown rpc accepted"
    );
    let a = gen_drive(&dir, "41");
    let b = gen_drive(&dir, "41");
    let d = gen_drive(&dir, "42");
    assert!(
        a == b && a != d,
        "HYDO_SEED does not decide the requests:\n{a}\n{b}\n{d}"
    );
    std::env::remove_var("HYDO_SEED");
}
