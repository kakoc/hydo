//! The Go generator's unit tests (load and valuegen, Milestone 91), ported.

use hydo_grpc::pjson;
use hydo_grpc::rng::{derive, R};
use hydo_grpc::spec::{default_defaults, FieldHint, From};
use hydo_grpc::valuegen::{canonical, decl_fields, parse, Env, Gen, GenError};
use hydo_grpc_gen::{desc, load};
use prost::Message;
use prost_reflect::{
    Cardinality, DescriptorPool, DynamicMessage, Kind, MessageDescriptor, ReflectMessage, Value,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

fn here() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn demo() -> DescriptorPool {
    let (set, names) = desc::from_proto_dir(&here().join("../../grpcgen/demo/proto"), &[]).unwrap();
    assert_eq!(names, vec!["orders/v1/orders.proto"]);
    DescriptorPool::decode(desc::encode_set(&set).as_slice()).unwrap()
}

fn kitchen() -> DescriptorPool {
    let (set, _) = desc::from_proto_dir(&here().join("tests/testdata/kitchen"), &[]).unwrap();
    DescriptorPool::decode(desc::encode_set(&set).as_slice()).unwrap()
}

fn hints(y: &str) -> load::Hints {
    let p = std::env::temp_dir().join(format!(
        "hydo-grpc-units-{}-{}.yaml",
        std::process::id(),
        y.len()
    ));
    std::fs::write(&p, y).unwrap();
    let h = load::read_hints(p.to_str().unwrap()).unwrap();
    let _ = std::fs::remove_file(&p);
    h
}

#[test]
fn build_demo() {
    let h = load::read_hints(
        here()
            .join("../../grpcgen/demo/hints.yaml")
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let p = load::build(&demo(), &h, None).unwrap();
    let names: Vec<String> = p
        .rpcs
        .iter()
        .map(|r| format!("{}={}", r.short, r.name))
        .collect();
    assert_eq!(names.join(" "), "create=orders.v1.Orders/Create get=orders.v1.Orders/Get cancel=orders.v1.Orders/Cancel list=orders.v1.Orders/List");
    let g = &p.rpcs[p.find("Get").unwrap()];
    let f = g.fields["id"].from.as_ref().unwrap();
    assert_eq!(
        (f.rpc.as_str(), f.side.as_str(), f.path.as_str()),
        ("orders.v1.Orders/Create", "response", "id")
    );
    assert_eq!(g.needs.len(), 1);
    let c = &p.rpcs[p.find("orders.v1.Orders/Create").unwrap()];
    assert!(
        c.retry.attempts == 4 && c.retry.at_end && c.outcome_id == "request_id" && c.weight == 3
    );
    // the plan is a pure function of its inputs
    assert_eq!(
        p.marshal(),
        load::build(&demo(), &h, None).unwrap().marshal()
    );
}

#[test]
fn hint_errors() {
    let pool = demo();
    for (y, want) in [
        ("rpcs: {Nope: {}}", "no rpc \"Nope\""),
        (
            "rpcs: {Create: {fields: {nope: {value: 1}}}}",
            "no field \"nope\"",
        ),
        (
            "rpcs: {Create: {fields: {customer: {range: [1, 2]}}}}",
            "range needs a number field",
        ),
        (
            "rpcs: {Create: {fields: {amount_cents: {range: [5, 1]}}}}",
            "lo > hi",
        ),
        (
            "rpcs: {Create: {fields: {channel: {choice: [PURPLE]}}}}",
            "not a value of enum",
        ),
        (
            "rpcs: {Create: {fields: {items: {value: x}}}}",
            "needs a scalar",
        ),
        (
            "rpcs: {Create: {fields: {customer: {format: base64}}}}",
            "format",
        ),
        (
            "rpcs: {Get: {fields: {id: {from: Create.amount_cents}}}}",
            "",
        ), // int64 -> string is allowed
        (
            "rpcs: {Get: {fields: {id: {from: Create.items}}}}",
            "is repeated message",
        ),
        (
            "rpcs: {Get: {fields: {id: {from: Create.nope}}}}",
            "no field \"nope\"",
        ),
        (
            "rpcs: {Create: {fields: {labels.key: {len: [2, 2]}, labels.bogus: {}}}}",
            "comes key or value",
        ),
        (
            "rpcs: {Create: {oneofs: {payment: [cash]}}}",
            "not a member",
        ),
        (
            "rpcs: {Create: {oneofs: {pay: [card]}}}",
            "no oneof \"pay\"",
        ),
        (
            "rpcs: {Create: {classify: {TIMEOUT: fail}}}",
            "not a gRPC code name",
        ),
        (
            "rpcs: {Create: {classify: {UNAVAILABLE: maybe}}}",
            "ok, fail, info",
        ),
        ("rpcs: {Create: {outcome_id: items.sku}}", "single scalar"),
        ("services: [Payments]", "not in the schema"),
        (
            "scenarios: [{name: s, calls: [Create, Nope]}]",
            "no rpc \"Nope\"",
        ),
    ] {
        let r = load::build(&pool, &hints(y), None);
        match (want.is_empty(), r) {
            (true, Err(e)) => panic!("{y}: unexpected error {e}"),
            (false, Ok(_)) => panic!("{y}: want an error containing {want:?}"),
            (false, Err(e)) => assert!(
                e.contains(want),
                "{y}: want an error containing {want:?}, got {e}"
            ),
            (true, Ok(_)) => {}
        }
    }
    // unknown keys are errors, not silently ignored
    let p = std::env::temp_dir().join(format!("hydo-grpc-units-typo-{}.yaml", std::process::id()));
    std::fs::write(&p, "rpcs: {Create: {wieght: 3}}\n").unwrap();
    let e = load::read_hints(p.to_str().unwrap()).unwrap_err();
    assert!(e.contains("wieght"), "{e}");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn parse_from_forms() {
    for (input, want) in [
        ("Create.id", "Create response id"),
        ("Create.request.request_id", "Create request request_id"),
        (
            "orders.v1.Orders/Create.response.id",
            "orders.v1.Orders/Create response id",
        ),
        (
            "orders.v1.Orders/List.orders.id",
            "orders.v1.Orders/List response orders.id",
        ),
    ] {
        let f = load::parse_from(&hydo_grpc_gen::yaml::Any::Str(input.into())).unwrap();
        assert_eq!(format!("{} {} {}", f.rpc, f.side, f.path), want, "{input}");
    }
    assert!(load::parse_from(&hydo_grpc_gen::yaml::Any::Str("Create".into())).is_err());
    let m = hydo_grpc_gen::yaml::Any::Map(
        ["rpc", "path", "pick", "fallback"]
            .iter()
            .zip(["Create", "id", "latest", "generate"])
            .map(|(k, v)| {
                (
                    hydo_grpc_gen::yaml::Any::Str(k.to_string()),
                    hydo_grpc_gen::yaml::Any::Str(v.into()),
                )
            })
            .collect(),
        true,
    );
    let f = load::parse_from(&m).unwrap();
    assert!(f.pick == "latest" && f.fallback == "generate");
}

#[test]
fn snake_names() {
    for (i, w) in [
        ("Create", "create"),
        ("GetOrder", "get_order"),
        ("ListHTTPRoutes", "list_http_routes"),
        ("V2Get", "v2_get"),
    ] {
        assert_eq!(load::snake(i), w);
    }
}

/// The descriptor set is canonical: the same schema serializes to the same bytes, dependencies first.
#[test]
fn canonical_set() {
    let (a, _) = desc::from_proto_dir(&here().join("tests/testdata/kitchen"), &[]).unwrap();
    let (b, _) = desc::from_proto_dir(&here().join("tests/testdata/kitchen"), &[]).unwrap();
    assert_eq!(desc::encode_set(&a), desc::encode_set(&b));
    let pos: HashMap<&str, usize> = a
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();
    assert!(
        pos["legacy.proto"] < pos["kitchen.proto"]
            && pos["google/protobuf/any.proto"] < pos["kitchen.proto"]
    );
}

#[derive(Default)]
struct TEnv {
    n: u64,
    pool: HashMap<String, Vec<String>>,
}

impl Env for TEnv {
    fn unique(&mut self) -> u64 {
        self.n += 1;
        self.n
    }

    fn take(&mut self, f: &From, r: &mut R) -> Option<String> {
        let vs = self.pool.get(&f.pool())?;
        if vs.is_empty() {
            return None;
        }
        Some(vs[r.intn(vs.len() as i64) as usize].clone())
    }
}

fn gen(
    md: &MessageDescriptor,
    hints: &BTreeMap<String, FieldHint>,
    oneofs: &BTreeMap<String, Option<Vec<String>>>,
    seed: u64,
    n: usize,
) -> Vec<DynamicMessage> {
    let d = default_defaults();
    let mut env = TEnv::default();
    let mut g = Gen {
        d: &d,
        hints,
        oneofs,
        env: &mut env,
    };
    let mut r = R::new(seed);
    (0..n).map(|_| g.message(md, &mut r).unwrap()).collect()
}

fn wire(ms: &[DynamicMessage]) -> Vec<u8> {
    let mut b = Vec::new();
    for m in ms {
        let x = hydo_grpc::wire::encode(m);
        b.extend(format!("{}:", x.len()).into_bytes());
        b.extend(x);
    }
    b
}

/// The same seed gives the same messages, byte for byte; another seed gives others.
#[test]
fn deterministic() {
    let pool = kitchen();
    let md = pool.get_message_by_name("kitchen.Sink").unwrap();
    let none = BTreeMap::new();
    let no = BTreeMap::new();
    let a = wire(&gen(&md, &none, &no, 42, 300));
    assert_eq!(
        a,
        wire(&gen(&md, &none, &no, 42, 300)),
        "same seed, different messages"
    );
    assert_ne!(
        a,
        wire(&gen(&md, &none, &no, 43, 300)),
        "different seeds, same messages"
    );
    assert!(
        derive(1, &[2, 3]) != derive(1, &[3, 2]) && derive(1, &[2]) != derive(2, &[1]),
        "derive is symmetric in its labels"
    );
}

fn required_set(m: &DynamicMessage) -> bool {
    for fd in decl_fields(&m.descriptor()) {
        if fd.cardinality() == Cardinality::Required && !m.has_field(&fd) {
            return false;
        }
        if m.has_field(&fd) {
            match &*m.get_field(&fd) {
                Value::Message(s) if !required_set(s) => return false,
                Value::List(l) => {
                    for v in l {
                        if let Value::Message(s) = v {
                            if !required_set(s) {
                                return false;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    true
}

fn check_msg(m: &DynamicMessage, depth: i64, seen: &mut HashSet<String>) {
    let md = m.descriptor();
    assert!(
        depth <= default_defaults().depth + 1,
        "nesting deeper than the limit in {}",
        md.full_name()
    );
    for od in md.oneofs() {
        let fs: Vec<_> = od.fields().collect();
        if od.is_synthetic() {
            seen.insert(format!(
                "{}:{}",
                fs[0].name(),
                if m.has_field(&fs[0]) { "set" } else { "unset" }
            ));
            continue;
        }
        let set: Vec<_> = fs.iter().filter(|f| m.has_field(f)).collect();
        assert!(
            set.len() <= 1,
            "oneof {} has {} members set",
            od.full_name(),
            set.len()
        );
        for f in set {
            seen.insert(format!("choice:{}", f.name()));
        }
    }
    for fd in decl_fields(&md) {
        if !m.has_field(&fd) {
            continue;
        }
        let v = m.get_field(&fd);
        match &*v {
            Value::Map(mp) => {
                seen.insert(format!("map:{}", fd.name()));
                for x in mp.values() {
                    if let Value::Message(s) = x {
                        check_msg(s, depth + 1, seen);
                    }
                }
            }
            Value::List(l) => {
                for x in l {
                    match x {
                        Value::Message(s) => check_msg(s, depth + 1, seen),
                        x => check_scalar(&fd, x, seen),
                    }
                }
            }
            Value::Message(s) => check_msg(s, depth + 1, seen),
            x => check_scalar(&fd, x, seen),
        }
    }
}

fn check_scalar(fd: &prost_reflect::FieldDescriptor, v: &Value, seen: &mut HashSet<String>) {
    match (fd.kind(), v) {
        (Kind::Enum(e), Value::EnumNumber(n)) => {
            let ev = e
                .get_value(*n)
                .unwrap_or_else(|| panic!("{}: undeclared enum value {n}", fd.full_name()));
            assert!(
                !ev.name().ends_with("UNSPECIFIED"),
                "{}: generated the UNSPECIFIED value",
                fd.full_name()
            );
            if fd.name() == "color" {
                seen.insert(format!("color:{}", ev.name()));
            }
        }
        (_, Value::I32(x)) => {
            if *x < 0 {
                seen.insert("neg".into());
            }
            if *x == i32::MAX {
                seen.insert("edge-max".into());
            }
        }
        (_, Value::F32(x)) => assert!(x.is_finite()),
        (_, Value::F64(x)) => assert!(x.is_finite()),
        _ => {}
    }
}

/// Every generated message is valid for its schema: required fields set, JSON and wire round
/// trips, one oneof member, declared enum values, bounded nesting; every shape is produced.
#[test]
fn valid() {
    let pool = kitchen();
    let md = pool.get_message_by_name("kitchen.Sink").unwrap();
    let mut seen = HashSet::new();
    let jp = hydo_grpc::client::json_pool(&pool);
    let ms = gen(&md, &BTreeMap::new(), &BTreeMap::new(), 7, 2000);
    for m in ms {
        assert!(required_set(&m), "required field missing");
        let js = pjson::marshal(&m).unwrap();
        let v = hydo_grpc::jsonin::parse(&js).unwrap();
        let back = hydo_grpc::client::read_json(&jp, md.full_name(), v)
            .unwrap_or_else(|e| panic!("JSON back: {e} ({js})"));
        // the same JSON and the same bytes (the pools differ by the marker file)
        assert_eq!(
            pjson::marshal(&back).unwrap(),
            js,
            "JSON round trip changed the message"
        );
        assert_eq!(
            hydo_grpc::wire::encode(&back),
            hydo_grpc::wire::encode(&m),
            "JSON round trip changed the message: {js}"
        );
        let back = DynamicMessage::decode(md.clone(), m.encode_to_vec().as_slice()).unwrap();
        assert!(
            hydo_grpc::wire::encode(&back) == hydo_grpc::wire::encode(&m),
            "wire round trip"
        );
        check_msg(&m, 0, &mut seen);
    }
    for want in [
        "choice:c_str",
        "choice:c_leaf",
        "choice:c_num",
        "opt:set",
        "opt:unset",
        "color:RED",
        "color:GREEN",
        "color:BLUE",
        "map:ms",
        "map:ml",
        "map:mb",
        "neg",
        "edge-max",
    ] {
        assert!(seen.contains(want), "never generated {want}");
    }
}

fn h(f: impl FnOnce(&mut FieldHint)) -> FieldHint {
    let mut x = FieldHint::default();
    f(&mut x);
    x
}

/// Hints narrow the values: ranges, choices, lengths, constants, unique values, formats, omitted
/// fields, allowed oneof members.
#[test]
fn field_hints() {
    let pool = kitchen();
    let md = pool.get_message_by_name("kitchen.Sink").unwrap();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut hints = BTreeMap::new();
    hints.insert("i32".to_string(), h(|x| x.range = s(&["10", "20"])));
    hints.insert("u64".into(), h(|x| x.range = s(&["5", "5"])));
    hints.insert("db".into(), h(|x| x.range = s(&["-1.5", "1.5"])));
    hints.insert("s".into(), h(|x| x.choice = s(&["x", "y"])));
    hints.insert("by".into(), h(|x| x.len = vec![4, 4]));
    hints.insert("color".into(), h(|x| x.choice = s(&["BLUE"])));
    hints.insert("leaf".into(), h(|x| x.prob = Some(1.0)));
    hints.insert(
        "leaf.name".into(),
        h(|x| {
            x.unique = true;
            x.prefix = "id-".into();
        }),
    );
    hints.insert("rl".into(), h(|x| x.len = vec![2, 2]));
    hints.insert("rl.n".into(), h(|x| x.value = Some("-7".into())));
    hints.insert("ri".into(), h(|x| x.len = vec![0, 0]));
    hints.insert("ms".into(), h(|x| x.len = vec![3, 3]));
    hints.insert(
        "ms.key".into(),
        h(|x| {
            x.format = "digits".into();
            x.len = vec![6, 6];
        }),
    );
    hints.insert("ms.value".into(), h(|x| x.range = s(&["1", "3"])));
    hints.insert("opt".into(), h(|x| x.omit = true));
    hints.insert("tree".into(), h(|x| x.omit = true));
    hints.insert("wrapped.value".into(), h(|x| x.format = "uuid".into()));
    let mut oneofs = BTreeMap::new();
    oneofs.insert("choice".to_string(), Some(vec!["c_num".to_string()]));
    let mut names = HashSet::new();
    let get = |m: &DynamicMessage, n: &str| m.get_field_by_name(n).unwrap().into_owned();
    for m in gen(&md, &hints, &oneofs, 9, 500) {
        let i = get(&m, "i32").as_i32().unwrap();
        assert!((10..=20).contains(&i));
        assert_eq!(get(&m, "u64").as_u64(), Some(5));
        let d = get(&m, "db").as_f64().unwrap();
        assert!((-1.5..=1.5).contains(&d));
        let sv = get(&m, "s");
        assert!(sv.as_str() == Some("x") || sv.as_str() == Some("y"));
        assert_eq!(get(&m, "by").as_bytes().unwrap().len(), 4);
        assert_eq!(get(&m, "color").as_enum_number(), Some(5));
        assert!(m.has_field_by_name("leaf"), "leaf with prob 1 unset");
        let leaf = get(&m, "leaf");
        let name = leaf
            .as_message()
            .unwrap()
            .get_field_by_name("name")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            name.starts_with("id-") && names.insert(name.clone()),
            "leaf.name {name} not unique / no prefix"
        );
        let rl = get(&m, "rl");
        let rl = rl.as_list().unwrap();
        assert_eq!(rl.len(), 2);
        assert_eq!(
            rl[0]
                .as_message()
                .unwrap()
                .get_field_by_name("n")
                .unwrap()
                .as_i32(),
            Some(-7)
        );
        assert!(get(&m, "ri").as_list().unwrap().is_empty());
        let ms = get(&m, "ms");
        let ms = ms.as_map().unwrap();
        assert_eq!(ms.len(), 3);
        for (k, v) in ms {
            let k = match k {
                prost_reflect::MapKey::String(s) => s.clone(),
                _ => unreachable!(),
            };
            let v = v.as_i64().unwrap();
            assert!(
                k.len() == 6 && k.bytes().all(|c| c.is_ascii_digit()) && (1..=3).contains(&v),
                "ms entry {k}={v}"
            );
        }
        assert!(
            !m.has_field_by_name("opt") && !m.has_field_by_name("tree"),
            "omitted field set"
        );
        assert!(
            m.has_field_by_name("c_num"),
            "oneof member outside the allowed set"
        );
        if m.has_field_by_name("wrapped") {
            let w = get(&m, "wrapped");
            let u = w
                .as_message()
                .unwrap()
                .get_field_by_name("value")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            assert!(u.len() == 36 && u.as_bytes()[14] == b'4', "not a uuid: {u}");
        }
    }
}

#[test]
fn from_pools() {
    let pool = kitchen();
    let md = pool.get_message_by_name("kitchen.Ids").unwrap();
    let from = From {
        rpc: "kitchen.Kitchen/Put".into(),
        side: "response".into(),
        path: "id".into(),
        fallback: "skip".into(),
        pick: "random".into(),
    };
    let mut hints = BTreeMap::new();
    hints.insert("id".to_string(), h(|x| x.from = Some(from.clone())));
    let d = default_defaults();
    let no = BTreeMap::new();
    let mut env = TEnv::default();
    let mut g = Gen {
        d: &d,
        hints: &hints,
        oneofs: &no,
        env: &mut env,
    };
    assert_eq!(g.message(&md, &mut R::new(1)).unwrap_err(), GenError::Skip);
    let mut env = TEnv::default();
    env.pool
        .insert(from.pool(), vec!["a".into(), "b".into(), "c".into()]);
    let mut g = Gen {
        d: &d,
        hints: &hints,
        oneofs: &no,
        env: &mut env,
    };
    let mut r = R::new(2);
    let mut got = HashSet::new();
    for _ in 0..100 {
        let m = g.message(&md, &mut r).unwrap();
        got.insert(
            m.get_field_by_name("id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(got.len(), 3, "pool values not used: {got:?}");
    let mut gen_from = from.clone();
    gen_from.fallback = "generate".into();
    let mut hints = BTreeMap::new();
    hints.insert("id".to_string(), h(|x| x.from = Some(gen_from)));
    let mut env = TEnv::default();
    let mut g = Gen {
        d: &d,
        hints: &hints,
        oneofs: &no,
        env: &mut env,
    };
    assert!(g.message(&md, &mut R::new(1)).is_ok(), "fallback generate");
}

#[test]
fn canonical_parse() {
    let pool = kitchen();
    let md = pool.get_message_by_name("kitchen.Sink").unwrap();
    for m in gen(&md, &BTreeMap::new(), &BTreeMap::new(), 3, 200) {
        for fd in decl_fields(&md) {
            if !m.has_field(&fd)
                || fd.is_list()
                || fd.is_map()
                || matches!(fd.kind(), Kind::Message(_))
            {
                continue;
            }
            let v = m.get_field(&fd).into_owned();
            let s = canonical(&fd, &v);
            let back = parse(&fd, &s).unwrap_or_else(|e| panic!("{}: {e}", fd.name()));
            assert_eq!(back, v, "{}: {s}", fd.name());
        }
    }
}
