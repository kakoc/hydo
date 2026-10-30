//! The case generators of the old Python references, ported exactly (CPython's Mersenne Twister in
//! `pyrandom`), so the frozen answers in tests/golden/ belong to the very cases hydohist.py
//! answered: tests/equiv.py's `gen` (the seeded random corpus and the long histories) and
//! test_hydohist.py's `gen_linearizable` (its Randomized tests). Both scripts are at commit
//! 2a58c4b (`git show 2a58c4b:hydo/crates/hydo-history/tests/equiv.py`,
//! `git show 2a58c4b:hydo/history/test_hydohist.py`). Every case line is the JSON text
//! equiv.py wrote (`json.dumps(sort_keys=True, separators=(",", ":"))`); the goldens pin a hash of
//! the case stream too, so a change here is caught as such.

#![allow(dead_code)]

pub mod pyrandom;

use hydo_history::listappend::LEVELS;
use hydo_history::py::{dumps_sorted, repr_float, BigInt, PyVal};
use hydo_history::{pdec, penc, DEFAULT_BUDGET};
use pyrandom::PyRandom;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

// ------------------------------------------------------------------ JSON as Python writes it
fn int(v: i64) -> PyVal {
    PyVal::Int(BigInt::from_i64(v))
}

fn st(s: &str) -> PyVal {
    PyVal::Str(s.to_string())
}

fn esc(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(v)` with the default separators, in insertion order.
pub fn dumps_default(v: &PyVal) -> String {
    fn go(v: &PyVal, out: &mut String) {
        match v {
            PyVal::None => out.push_str("null"),
            PyVal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            PyVal::Int(i) => out.push_str(&i.text()),
            PyVal::Float(f) => out.push_str(&repr_float(*f)),
            PyVal::Str(s) => esc(s, out),
            PyVal::List(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    go(x, out);
                }
                out.push(']');
            }
            PyVal::Dict(m) => {
                out.push('{');
                for (i, (k, x)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    esc(k, out);
                    out.push_str(": ");
                    go(x, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    go(v, &mut out);
    out
}

/// `d[k] = v` on an insertion-ordered dict.
fn set(d: &mut PyVal, k: &str, v: PyVal) {
    let PyVal::Dict(m) = d else { unreachable!() };
    match m.iter_mut().find(|(x, _)| x == k) {
        Some(e) => e.1 = v,
        None => m.push((k.to_string(), v)),
    }
}

fn get<'a>(d: &'a PyVal, k: &str) -> &'a PyVal {
    let PyVal::Dict(m) = d else { unreachable!() };
    &m.iter().find(|(x, _)| x == k).unwrap().1
}

fn dict(pairs: Vec<(&str, PyVal)>) -> PyVal {
    PyVal::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn strs<S: AsRef<str>>(xs: &[S]) -> PyVal {
    PyVal::List(xs.iter().map(|x| st(x.as_ref())).collect())
}

// ------------------------------------------------------------------ equiv.py's corpus
const VALS: [&str; 14] = [
    "0", "1", "2", "3", "v1", "v2", "a%20b", "%", "%6Eil", "x%40y", "%C3%A9", "%E2%82", "\u{e9}",
    "1_0",
];
const JUNK: [&str; 19] = [
    "",
    "random console text",
    "[   1.234] kernel: something",
    "HYDO-HIST",
    "HYDO-HIST kv 0",
    "HYDO-HIST kv 0 bogus 1 write x 1",
    "HYDO-HIST k$v 0 invoke 1 write x 1",
    "HYDO-HIST kv 0 invoke 12x write x 1",
    "HYDO-ORACLE: OK invariants=1 held (a: b)",
    "HYDO-CHECK {not json",
    "HYDO-CHECK-FINAL {\"rc\":1}",
    "HYDO-CHECK-FINAL [1]x",
    "HYDO-CHECK [1,2]",
    "HYDO-CHECK {\"type\":\"linearizable\"}",
    "\x1c",
    "x\u{2028}y",
    "HYDO-HIST kv 9 invoke 1 txn a:k:1:1",
    "HYDO-HIST log 9 ok 1 txn r:x:[1,2]",
    "HYDO-CHECK-FINAL {\"id\": \"kv.lin\", \"rc\": \" 4 \"}",
];
const CRASH: [&str; 3] = [
    "HYDO-CHECK-FINAL [1]",
    "HYDO-CHECK {\"id\": [1], \"type\": \"list-append\"}",
    "HYDO-CHECK-FINAL {\"id\": \"kv.lin\", \"rc\": Infinity}",
];

/// One record before it is printed: (process, type, function, arguments).
#[derive(Clone)]
pub struct Rec {
    p: usize,
    ty: String,
    f: String,
    args: Vec<String>,
}

fn rec(p: usize, ty: &str, f: &str, args: Vec<String>) -> Rec {
    Rec {
        p,
        ty: ty.into(),
        f: f.into(),
        args,
    }
}

fn ts(rnd: &mut PyRandom, i: usize) -> String {
    let c = rnd.random();
    if c < 0.85 {
        return ((i as i64 + 1) * 1_000_000 + rnd.range(0, 1000)).to_string();
    }
    rnd.choice(&[
        "-",
        "0",
        "125000000",
        "-5",
        "1_000",
        "\u{661}\u{662}",
        "99999999999999999999999",
        "375000000",
        "-0",
        "+7",
    ])
    .to_string()
}

fn emit(rnd: &mut PyRandom, hist: &str, r: &Rec, i: usize) -> String {
    let pre = *rnd.choice(&["", "", "", "", "[  12.3] ", "junk ", "\t"]);
    let t = ts(rnd, i);
    let args: String = r.args.iter().map(|a| format!(" {a}")).collect();
    format!("{pre}HYDO-HIST {hist} {} {} {t} {}{args}", r.p, r.ty, r.f)
}

type Cell = Option<(String, Option<i64>)>;

enum RegRes {
    None,
    Rev(i64),
    Lost,
    NoMatch,
    Read(Cell),
    All(Vec<(String, Cell)>),
}

fn cell_text(x: &Cell, versions: bool) -> String {
    match x {
        None => "nil".into(),
        Some((v, ver)) => match (versions, ver) {
            (true, Some(n)) => format!("{v}@{n}"),
            _ => v.clone(),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_register(
    rnd: &mut PyRandom,
    keys: &[String],
    nproc: usize,
    nops: usize,
    cas: bool,
    versions: bool,
    init_any: bool,
) -> Vec<Rec> {
    let mut store: HashMap<String, (String, Option<i64>)> = HashMap::new();
    if init_any {
        for k in keys {
            let v = rnd.choice(&VALS[..4]).to_string();
            store.insert(k.clone(), (v, None));
        }
    }
    let mut rev = rnd.range(1, 50);
    let mut lines = Vec::new();
    // f, key, value (or old, new), done, result
    let mut open: BTreeMap<usize, (String, String, Vec<String>, bool, RegRes)> = BTreeMap::new();
    let mut n = 0;
    while n < nops || !open.is_empty() {
        let p = rnd.below(nproc);
        if !open.contains_key(&p) && n < nops {
            let k = rnd.choice(keys).clone();
            let c = rnd.random();
            if cas && c < 0.25 {
                let mut olds: Vec<&str> = VALS[..4].to_vec();
                olds.push("nil");
                let old = rnd.choice(&olds).to_string();
                let new = rnd.choice(&VALS[..6]).to_string();
                lines.push(rec(
                    p,
                    "invoke",
                    "cas",
                    vec![k.clone(), old.clone(), new.clone()],
                ));
                open.insert(p, ("cas".into(), k, vec![old, new], false, RegRes::None));
            } else if c < 0.55 {
                let v = rnd.choice(&VALS).to_string();
                lines.push(rec(p, "invoke", "write", vec![k.clone(), v.clone()]));
                open.insert(p, ("write".into(), k, vec![v], false, RegRes::None));
            } else if c < 0.62 {
                open.insert(
                    p,
                    ("readall".into(), String::new(), vec![], false, RegRes::None),
                );
                lines.push(rec(p, "invoke", "read", vec!["*".into()]));
            } else {
                lines.push(rec(p, "invoke", "read", vec![k.clone()]));
                open.insert(p, ("read".into(), k, vec![], false, RegRes::None));
            }
            n += 1;
            continue;
        }
        if !open.contains_key(&p) {
            continue;
        }
        let done = open[&p].3;
        if !done {
            let (f, k, v, _, _) = open.remove(&p).unwrap();
            let fate = rnd.random();
            let res = match f.as_str() {
                "write" => {
                    if fate < 0.9 {
                        rev += rnd.range(1, 3);
                        store.insert(k.clone(), (v[0].clone(), Some(rev)));
                        RegRes::Rev(rev)
                    } else {
                        RegRes::Lost
                    }
                }
                "cas" => {
                    let cur = store.get(&k).map(|x| x.0.clone());
                    let ok = match &cur {
                        None => v[0] == "nil",
                        Some(c) => *c == v[0],
                    };
                    if ok {
                        rev += 1;
                        store.insert(k.clone(), (v[1].clone(), Some(rev)));
                        RegRes::Rev(rev)
                    } else {
                        RegRes::NoMatch
                    }
                }
                "read" => RegRes::Read(store.get(&k).cloned()),
                _ => RegRes::All(
                    keys.iter()
                        .map(|kk| (kk.clone(), store.get(kk).cloned()))
                        .collect(),
                ),
            };
            open.insert(p, (f, k, v, true, res));
            continue;
        }
        let (f, k, v, _, res) = open.remove(&p).unwrap();
        let r = rnd.random();
        if r < 0.05 {
            continue; // never completed: unknown outcome
        }
        match f.as_str() {
            "write" => match res {
                RegRes::Lost => lines.push(rec(
                    p,
                    if r < 0.5 { "info" } else { "fail" },
                    "write",
                    vec![k, v[0].clone()],
                )),
                _ if r < 0.1 => lines.push(rec(p, "info", "write", vec![k, v[0].clone()])),
                RegRes::Rev(n) => {
                    let val = if versions {
                        format!("{}@{n}", v[0])
                    } else {
                        v[0].clone()
                    };
                    lines.push(rec(p, "ok", "write", vec![k, val]))
                }
                _ => unreachable!(),
            },
            "cas" => match res {
                RegRes::NoMatch => {
                    lines.push(rec(p, "fail", "cas", vec![k, v[0].clone(), v[1].clone()]))
                }
                RegRes::Rev(n) => {
                    let new = if versions {
                        format!("{}@{n}", v[1])
                    } else {
                        v[1].clone()
                    };
                    lines.push(rec(p, "ok", "cas", vec![k, v[0].clone(), new]))
                }
                _ => unreachable!(),
            },
            "read" => {
                if r < 0.08 {
                    lines.push(rec(p, "info", "read", vec![k]));
                } else {
                    let RegRes::Read(x) = res else { unreachable!() };
                    lines.push(rec(p, "ok", "read", vec![k, cell_text(&x, versions)]));
                }
            }
            _ => {
                let RegRes::All(mut all) = res else {
                    unreachable!()
                };
                all.sort_by(|a, b| a.0.cmp(&b.0));
                let mut args = Vec::new();
                for (kk, x) in all {
                    args.push(kk);
                    args.push(cell_text(&x, versions));
                }
                lines.push(rec(p, "ok", "read", args));
            }
        }
    }
    lines
}

fn mutate_register(rnd: &mut PyRandom, lines: Vec<Rec>) -> Vec<Rec> {
    let mut lines = lines;
    let times = *rnd.choice(&[0usize, 1, 1, 2, 3]);
    for _ in 0..times {
        if lines.is_empty() {
            break;
        }
        let i = rnd.below(lines.len());
        let c = rnd.random();
        let l = lines[i].clone();
        if l.ty == "ok" && l.f == "read" && l.args.len() >= 2 && c < 0.5 {
            let j = 1 + 2 * rnd.below(l.args.len() / 2);
            let mut a = l.args.clone();
            if a[j].contains('@') && rnd.random() < 0.5 {
                let (v, ver) = a[j].rsplit_once('@').unwrap();
                let ver: i64 = ver.parse().unwrap();
                let d = *rnd.choice(&[-1i64, 1, 2]);
                a[j] = format!("{v}@{}", ver + d);
            } else {
                let mut vs: Vec<&str> = VALS.to_vec();
                vs.push("nil");
                vs.push("zz");
                a[j] = rnd.choice(&vs).to_string();
            }
            lines[i] = Rec { args: a, ..l };
        } else if l.ty == "ok" && l.f == "write" && c < 0.7 {
            let ty = rnd.choice(&["fail", "info"]).to_string();
            lines[i] = Rec { ty, ..l };
        } else if c < 0.85 && i + 1 < lines.len() {
            lines.swap(i, i + 1);
        } else {
            lines.remove(i);
        }
    }
    lines
}

/// A micro-operation: an append of an element, or a read (with what it read, once known).
#[derive(Clone)]
enum Mop {
    A(String, String),
    R(String, Option<Vec<String>>),
}

fn lines_args(f: &str, mops: &[Mop], ok: bool) -> Vec<String> {
    let lst = |l: &[String]| format!("[{}]", l.join(","));
    let key = |k: &str| penc(&pdec(k), ":,[]");
    match f {
        "append" => match &mops[0] {
            Mop::A(k, e) => vec![key(k), e.clone()],
            Mop::R(..) => unreachable!(),
        },
        "read" => match &mops[0] {
            Mop::R(k, l) => {
                let mut v = vec![key(k)];
                if let (true, Some(l)) = (ok, l) {
                    v.push(lst(l));
                }
                v
            }
            Mop::A(..) => unreachable!(),
        },
        _ => mops
            .iter()
            .map(|m| match m {
                Mop::A(k, e) => format!("a:{}:{e}", key(k)),
                Mop::R(k, Some(l)) if ok => format!("r:{}:{}", key(k), lst(l)),
                Mop::R(k, _) => format!("r:{}", key(k)),
            })
            .collect(),
    }
}

type Store = HashMap<String, Vec<String>>;

struct Pend {
    f: String,
    mops: Vec<Mop>,
    done: bool,
    res: Option<(bool, Vec<Mop>)>, // (ok, micro-ops); false = "fail"
    snap: Option<Store>,
}

fn gen_lists(
    rnd: &mut PyRandom,
    keys: &[String],
    nproc: usize,
    nops: usize,
    txn: bool,
    bug: Option<&str>,
) -> Vec<Rec> {
    let mut store: Store = HashMap::new();
    let mut elem = 0;
    let mut lines = Vec::new();
    let mut open: BTreeMap<usize, Pend> = BTreeMap::new();
    let mut n = 0;
    while n < nops || !open.is_empty() {
        let p = rnd.below(nproc);
        if !open.contains_key(&p) && n < nops {
            if txn && rnd.random() < 0.6 {
                let mut mops = Vec::new();
                for _ in 0..rnd.range(1, 5) {
                    let k = rnd.choice(keys).clone();
                    if rnd.random() < 0.5 {
                        elem += 1;
                        mops.push(Mop::A(k, elem.to_string()));
                    } else {
                        mops.push(Mop::R(k, None));
                    }
                }
                lines.push(rec(p, "invoke", "txn", lines_args("txn", &mops, false)));
                open.insert(
                    p,
                    Pend {
                        f: "txn".into(),
                        mops,
                        done: false,
                        res: None,
                        snap: Some(store.clone()),
                    },
                );
            } else if rnd.random() < 0.5 {
                elem += 1;
                let k = rnd.choice(keys).clone();
                open.insert(
                    p,
                    Pend {
                        f: "append".into(),
                        mops: vec![Mop::A(k.clone(), elem.to_string())],
                        done: false,
                        res: None,
                        snap: None,
                    },
                );
                lines.push(rec(p, "invoke", "append", vec![k, elem.to_string()]));
            } else {
                let k = rnd.choice(keys).clone();
                open.insert(
                    p,
                    Pend {
                        f: "read".into(),
                        mops: vec![Mop::R(k.clone(), None)],
                        done: false,
                        res: None,
                        snap: Some(store.clone()),
                    },
                );
                lines.push(rec(p, "invoke", "read", vec![k]));
            }
            n += 1;
            continue;
        }
        let Some(pd) = open.get_mut(&p) else {
            continue;
        };
        if !pd.done && bug == Some("step") && rnd.random() < 0.06 {
            pd.done = true;
            pd.res = Some((false, pd.mops.clone()));
            continue;
        }
        if !pd.done && bug == Some("step") {
            // every micro-op takes effect on its own turn
            let mut out = pd.res.as_ref().map(|r| r.1.clone()).unwrap_or_default();
            let m = pd.mops[out.len()].clone();
            match m {
                Mop::A(ref k, ref e) => {
                    store.entry(k.clone()).or_default().push(e.clone());
                    out.push(m);
                }
                Mop::R(k, _) => {
                    let l = store.get(&k).cloned().unwrap_or_default();
                    out.push(Mop::R(k, Some(l)));
                }
            }
            pd.done = out.len() == pd.mops.len();
            pd.res = Some((true, out));
            continue;
        }
        if !pd.done {
            let fate = rnd.random();
            if fate < 0.06 {
                pd.res = Some((false, pd.mops.clone()));
            } else {
                let mut own: HashMap<String, Vec<String>> = HashMap::new();
                let mut out = Vec::new();
                for m in pd.mops.clone() {
                    match m {
                        Mop::A(ref k, ref e) => {
                            if !(bug == Some("lossy") && rnd.random() < 0.05) {
                                store.entry(k.clone()).or_default().push(e.clone());
                            }
                            own.entry(k.clone()).or_default().push(e.clone());
                            out.push(m);
                        }
                        Mop::R(k, _) => {
                            let mut l: Vec<String> = match (&pd.snap, bug) {
                                (Some(snap), Some("si")) => {
                                    let mut l = snap.get(&k).cloned().unwrap_or_default();
                                    l.extend(own.get(&k).cloned().unwrap_or_default());
                                    l
                                }
                                _ => store.get(&k).cloned().unwrap_or_default(),
                            };
                            if bug == Some("dup") && !l.is_empty() && rnd.random() < 0.02 {
                                let x = l[0].clone();
                                l.push(x);
                            }
                            if bug == Some("swap") && l.len() > 1 && rnd.random() < 0.02 {
                                l.swap(0, 1);
                            }
                            out.push(Mop::R(k, Some(l)));
                        }
                    }
                }
                pd.res = Some((true, out));
            }
            pd.done = true;
            continue;
        }
        let pd = open.remove(&p).unwrap();
        let r = rnd.random();
        if r < 0.04 {
            continue;
        }
        let (ok, out) = pd.res.unwrap();
        if !ok {
            lines.push(rec(p, "fail", &pd.f, lines_args(&pd.f, &pd.mops, false)));
        } else if r < 0.09 {
            lines.push(rec(p, "info", &pd.f, lines_args(&pd.f, &pd.mops, false)));
        } else {
            lines.push(rec(p, "ok", &pd.f, lines_args(&pd.f, &out, true)));
        }
    }
    lines
}

fn verdicts() -> Vec<PyVal> {
    let mut v = vec![PyVal::None];
    for s in [
        "HYDO-ORACLE: OK invariants=0 held ()",
        "HYDO-ORACLE: OK invariants=1 held (a: b)",
        "HYDO-ORACLE: VIOLATION [g] guest first",
        "HYDO-ORACLE: ERROR boom",
        "  HYDO-ORACLE: OK invariants=0 held ()  ",
        "",
    ] {
        v.push(st(s));
    }
    v
}

fn gen_case(rnd: &mut PyRandom) -> PyVal {
    let mut hists: Vec<(String, Vec<Rec>)> = Vec::new();
    let mut specs: Vec<PyVal> = Vec::new();
    let nh = *rnd.choice(&[1usize, 1, 1, 2]);
    let mut sizes = vec![8usize, 15, 25, 40, 60, 100];
    if rnd.random() < 0.1 {
        sizes.push(250);
    }
    let size = *rnd.choice(&sizes);
    for h in 0..nh {
        let mut name = rnd
            .choice(&["kv".to_string(), "log".to_string(), format!("h{h}")])
            .clone();
        if hists.iter().any(|x| x.0 == name) {
            name += &h.to_string();
        }
        let nproc = *rnd.choice(&[1usize, 2, 3, 5, 8]);
        let k = *rnd.choice(&[1usize, 1, 2, 3]);
        let keys: Vec<String> = rnd
            .sample(&["x", "y", "z", "k:1", "k%202", "w"], k)
            .into_iter()
            .map(String::from)
            .collect();
        let mut lines;
        let mut sp;
        if rnd.random() < 0.55 {
            let cas = rnd.random() < 0.4;
            let versions = rnd.random() < 0.4;
            let init_any = rnd.random() < 0.15;
            lines = gen_register(rnd, &keys, nproc, size, cas, versions, init_any);
            if rnd.random() < 0.6 {
                lines = mutate_register(rnd, lines);
            }
            sp = dict(vec![
                ("id", st(&format!("{name}.lin"))),
                ("type", st("linearizable")),
                ("history", st(&name)),
            ]);
            if cas || rnd.random() < 0.3 {
                let m = if cas || rnd.random() < 0.5 {
                    "cas"
                } else {
                    "register"
                };
                set(&mut sp, "model", st(m));
            }
            if init_any || rnd.random() < 0.05 {
                set(&mut sp, "initial", st("any"));
            }
        } else {
            let txn = rnd.random() < 0.6;
            let bug = *rnd.choice(&[
                None,
                None,
                Some("si"),
                Some("si"),
                Some("si"),
                Some("step"),
                Some("step"),
                Some("step"),
                Some("lossy"),
                Some("dup"),
                Some("swap"),
            ]);
            lines = gen_lists(rnd, &keys, nproc, size, txn, bug);
            if rnd.random() < 0.1 {
                lines = mutate_register(rnd, lines);
            }
            sp = dict(vec![
                ("id", st(&format!("{name}.la"))),
                ("type", st("list-append")),
                ("history", st(&name)),
            ]);
            if rnd.random() < 0.85 {
                set(&mut sp, "consistency", st(rnd.choice::<&str>(&LEVELS)));
            }
        }
        let c = rnd.random();
        if c < 0.03 {
            let v = rnd.choice(&[st("linear"), PyVal::None, int(5)]).clone();
            set(&mut sp, "type", v);
        } else if c < 0.05 {
            let v = rnd
                .choice(&[st("reg"), PyVal::None, PyVal::List(vec![int(1), st("a")])])
                .clone();
            set(&mut sp, "model", v);
        } else if c < 0.07 {
            let v = rnd.choice(&[st("linearizable"), int(3)]).clone();
            set(&mut sp, "consistency", v);
        }
        if rnd.random() < 0.3 {
            let v = rnd
                .choice(&[
                    st("acked writes stay"),
                    st(""),
                    st("\u{e9} is fine"),
                    int(7),
                    PyVal::List(vec![st("l")]),
                    dict(vec![("b", int(1)), ("a", PyVal::Float(2.5))]),
                ])
                .clone();
            set(&mut sp, "msg", v);
        }
        if rnd.random() < 0.05 {
            let v = rnd.choice(&[PyVal::None, int(1), st("nohist")]).clone();
            set(&mut sp, "history", v);
        }
        if rnd.random() < 0.03 {
            let v = rnd
                .choice(&[int(1), PyVal::Float(2.0), PyVal::Bool(true), st("x y")])
                .clone();
            set(&mut sp, "id", v);
        }
        specs.push(sp);
        hists.push((name, lines));
    }
    // interleave the histories' records (each keeps its own order)
    let mut recs: Vec<String> = Vec::new();
    let mut idx = vec![0usize; hists.len()];
    let mut counter = 0;
    while (0..hists.len()).any(|i| idx[i] < hists[i].1.len()) {
        let live: Vec<usize> = (0..hists.len())
            .filter(|&j| idx[j] < hists[j].1.len())
            .collect();
        let i = *rnd.choice(&live);
        let r = hists[i].1[idx[i]].clone();
        idx[i] += 1;
        let line = emit(rnd, &hists[i].0, &r, counter);
        recs.push(line);
        counter += 1;
        if rnd.random() < 0.02 {
            recs.push(rnd.choice(&JUNK).to_string());
        }
    }
    let mut decl: Vec<String> = specs
        .iter()
        .map(|sp| format!("HYDO-CHECK {}", dumps_default(sp)))
        .collect();
    if rnd.random() < 0.05 {
        decl.push(decl[0].clone());
    }
    let mut pre = decl;
    pre.push("some boot line".into());
    if rnd.random() < 0.01 {
        pre.push(rnd.choice(&CRASH).to_string());
    }
    let mut finals: Vec<String> = Vec::new();
    if rnd.random() < 0.15 {
        let sp = rnd.choice(&specs).clone();
        let rc = rnd
            .choice(&[
                int(0),
                int(2),
                st("3"),
                PyVal::Float(1.5),
                PyVal::None,
                st("x"),
            ])
            .clone();
        let d = dict(vec![("id", get(&sp, "id").clone()), ("rc", rc)]);
        finals.push(format!("HYDO-CHECK-FINAL {}", dumps_default(&d)));
    }
    let cut = if rnd.random() < 0.3 {
        rnd.below(recs.len() + 1)
    } else {
        recs.len()
    };
    let mut work_lines = pre.clone();
    work_lines.extend(recs[..cut].iter().cloned());
    let mut work = work_lines.join("\n") + "\n";
    let mut srcs: Vec<(String, String)> = Vec::new();
    if cut < recs.len() || rnd.random() < 0.1 {
        let mut rl: Vec<String> = recs[cut..].to_vec();
        rl.extend(finals.iter().cloned());
        srcs.push(("one".into(), work));
        srcs.push(("one.recovery".into(), rl.join("\n") + "\n"));
    } else {
        work += &finals.join("\n");
        if !finals.is_empty() {
            work.push('\n');
        }
        srcs.push(("one".into(), work));
    }
    let verdict = rnd.choice(&verdicts()).clone();
    let mut budgets = vec![DEFAULT_BUDGET; 8];
    budgets.extend([5, 50, 500, 5000, -1]);
    let budget = *rnd.choice(&budgets);
    let given = if rnd.random() < 0.05 {
        PyVal::List(specs.clone())
    } else {
        PyVal::None
    };
    checks_case(&srcs, verdict, budget, given)
}

fn checks_case(srcs: &[(String, String)], verdict: PyVal, budget: i64, specs: PyVal) -> PyVal {
    dict(vec![
        ("kind", st("checks")),
        (
            "srcs",
            PyVal::List(
                srcs.iter()
                    .map(|(a, b)| PyVal::List(vec![st(a), st(b)]))
                    .collect(),
            ),
        ),
        ("verdict", verdict),
        ("budget", int(budget)),
        ("specs", specs),
    ])
}

fn gen_large(rnd: &mut PyRandom, witness: bool) -> PyVal {
    let name = "big";
    let nproc = *rnd.choice(&[5usize, 10, 16, 24]);
    let nk = *rnd.choice(&[1usize, 1, 2, 3]);
    let keys: Vec<String> = ["x", "y", "z"][..nk]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let size = *rnd.choice(&[500usize, 1000, 2000, 3000]);
    let lines;
    let sp;
    if witness {
        let np = *rnd.choice(&[2usize, 3, 4, 6]);
        let no = *rnd.choice(&[200usize, 500, 1000, 1500]);
        let versions = rnd.random() < 0.5;
        let mut l = gen_register(rnd, &["x".to_string()], np, no, false, versions, false);
        let reads: Vec<usize> = (0..l.len())
            .filter(|&i| l[i].ty == "ok" && l[i].f == "read")
            .collect();
        let i = reads[rnd.range((reads.len() / 2) as i64, reads.len() as i64) as usize];
        let v = rnd.choice(&["0", "1", "nil"]).to_string();
        let k = l[i].args[0].clone();
        l[i] = rec(l[i].p, "ok", "read", vec![k, v]);
        lines = l;
        sp = dict(vec![
            ("id", st("big.lin")),
            ("type", st("linearizable")),
            ("history", st(name)),
        ]);
    } else if rnd.random() < 0.6 {
        let cas = rnd.random() < 0.3;
        let versions = rnd.random() < 0.5;
        let mut l = gen_register(rnd, &keys, nproc, size, cas, versions, false);
        if rnd.random() < 0.5 {
            l = mutate_register(rnd, l);
        }
        lines = l;
        sp = dict(vec![
            ("id", st("big.lin")),
            ("type", st("linearizable")),
            ("history", st(name)),
            ("model", st(if cas { "cas" } else { "register" })),
        ]);
    } else {
        let bug = *rnd.choice(&[None, Some("si"), Some("step"), Some("lossy")]);
        let txn = rnd.random() < 0.7;
        lines = gen_lists(rnd, &keys, nproc, size, txn, bug);
        sp = dict(vec![
            ("id", st("big.la")),
            ("type", st("list-append")),
            ("history", st(name)),
            ("consistency", st(rnd.choice::<&str>(&LEVELS))),
        ]);
    }
    let recs: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, r)| emit(rnd, name, r, i))
        .collect();
    let text = format!("HYDO-CHECK {}\n{}\n", dumps_default(&sp), recs.join("\n"));
    checks_case(
        &[("one".into(), text)],
        st("HYDO-ORACLE: OK invariants=0 held ()"),
        DEFAULT_BUDGET,
        PyVal::None,
    )
}

/// What `equiv.py gen` writes: `n` cases from `seed` (`large`/`witness`: the long histories),
/// one JSON line each, with a percent-decoder, -encoder and console-decoding case every 50; it
/// stops early once `max_lines` lines are made (the stream's prefix).
pub fn gen(n: usize, seed: u64, large: bool, witness: bool, max_lines: usize) -> Vec<String> {
    let mut rnd = PyRandom::new(seed);
    let mut out = Vec::new();
    for i in 0..n {
        if out.len() >= max_lines {
            break;
        }
        if large || witness {
            out.push(dumps_sorted(&gen_large(&mut rnd, witness)));
            continue;
        }
        out.push(dumps_sorted(&gen_case(&mut rnd)));
        if i % 50 == 0 {
            let alpha = [
                "%", "4", "1", "e", "E", "f", "%C3", "%A9", "a", "@", "%2", "\u{e9}",
            ];
            let toks: Vec<String> = (0..30)
                .map(|_| {
                    let m = rnd.range(0, 8);
                    (0..m).map(|_| *rnd.choice(&alpha)).collect()
                })
                .collect();
            out.push(dumps_sorted(&dict(vec![
                ("kind", st("pdec")),
                ("toks", strs(&toks)),
            ])));
            let alpha = [
                "a", " ", "%", "@", ",", "[", ":", "\t", "\x01", "\u{e9}", "\u{3000}", "nil",
                "\x1d",
            ];
            let ss: Vec<String> = (0..30)
                .map(|_| {
                    let m = rnd.range(0, 6);
                    (0..m).map(|_| *rnd.choice(&alpha)).collect()
                })
                .collect();
            let extra = *rnd.choice(&["", ",", ",[]", ":"]);
            out.push(dumps_sorted(&dict(vec![
                ("kind", st("penc")),
                ("strs", strs(&ss)),
                ("extra", st(extra)),
            ])));
            let bytes = [
                0x41u8, 0x0a, 0x0d, 0xc3, 0xa9, 0xe2, 0x82, 0xac, 0xf0, 0x9f, 0x80, 0xff, 0xed,
                0xa0, 0xc0,
            ];
            let m = rnd.range(0, 40);
            let hex: String = (0..m)
                .map(|_| format!("{:02x}", rnd.choice(&bytes)))
                .collect();
            out.push(dumps_sorted(&dict(vec![
                ("kind", st("decode")),
                ("hex", st(&hex)),
            ])));
        }
    }
    out.truncate(max_lines);
    out
}

// ------------------------------------------------------------------ test_hydohist.py's Randomized
/// An open operation of gen_linearizable: f, key, value, took effect, what it read.
type LinPending = (String, String, String, bool, Option<Vec<String>>);

/// test_hydohist.gen_linearizable: a history linearizable by construction, as "p type f args"
/// lines.
pub fn gen_linearizable(
    seed: u64,
    nproc: usize,
    nops: usize,
    keys: &[&str],
    lists: bool,
) -> Vec<String> {
    let mut rnd = PyRandom::new(seed);
    let mut store: HashMap<String, Vec<String>> = HashMap::new();
    let mut lines = Vec::new();
    // f, k, v, done, res
    let mut open: BTreeMap<usize, LinPending> = BTreeMap::new();
    let (mut n, mut elem) = (0, 0);
    while n < nops || !open.is_empty() {
        let p = rnd.below(nproc);
        if !open.contains_key(&p) && n < nops {
            let k = rnd.choice(keys).to_string();
            if lists {
                let f = *rnd.choice(&["append", "read"]);
                if f == "append" {
                    elem += 1;
                    open.insert(
                        p,
                        ("append".into(), k.clone(), elem.to_string(), false, None),
                    );
                    lines.push(format!("{p} invoke append {k} {elem}"));
                } else {
                    open.insert(p, ("read".into(), k.clone(), String::new(), false, None));
                    lines.push(format!("{p} invoke read {k}"));
                }
            } else {
                let f = *rnd.choice(&["write", "read", "read"]);
                let v = rnd.below(4).to_string();
                lines.push(if f == "write" {
                    format!("{p} invoke {f} {k} {v}")
                } else {
                    format!("{p} invoke {f} {k}")
                });
                open.insert(p, (f.into(), k, v, false, None));
            }
            n += 1;
            continue;
        }
        let Some((f, k, v, done, _)) = open.get(&p).cloned() else {
            continue;
        };
        if !done {
            let res = match f.as_str() {
                "write" => {
                    store.insert(k.clone(), vec![v.clone()]);
                    None
                }
                "append" => {
                    store.entry(k.clone()).or_default().push(v.clone());
                    None
                }
                _ => store.get(&k).cloned(),
            };
            open.insert(p, (f, k, v, true, res));
            continue;
        }
        let (f, k, v, _, res) = open.remove(&p).unwrap();
        lines.push(match f.as_str() {
            "write" => format!("{p} ok write {k} {v}"),
            "append" => format!("{p} ok append {k} {v}"),
            _ if lists => format!("{p} ok read {k} [{}]", res.unwrap_or_default().join(",")),
            _ => format!(
                "{p} ok read {k} {}",
                res.map(|r| r[0].clone()).unwrap_or_else(|| "nil".into())
            ),
        });
    }
    lines
}

/// test_hydohist.hist: "p type f args" lines -> console text (time = line number in ms).
pub fn hist(lines: &[String], name: &str) -> String {
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        let mut it = l.splitn(3, ' ');
        let (p, ty, rest) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
        let _ = writeln!(
            out,
            "HYDO-HIST {name} {p} {ty} {} {rest}",
            (i + 1) * 1_000_000
        );
    }
    out
}

/// The case the captured `lin(lines)` / `la(lines)` call made (units of tests/equiv.py).
pub fn unit_case(lines: &[String], kind: &str) -> String {
    let sp = dict(vec![
        ("id", st("t")),
        ("type", st(kind)),
        ("history", st("kv")),
    ]);
    dumps_sorted(&checks_case(
        &[("w".into(), hist(lines, "kv"))],
        PyVal::None,
        DEFAULT_BUDGET,
        PyVal::List(vec![sp]),
    ))
}

/// The Randomized tests' cases, in the order unittest ran them (methods sorted by name), with the
/// test name of each.
pub fn randomized_units() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for seed in 0..20 {
        let l = gen_linearizable(seed, 5, 300, &["x", "y"], false);
        out.push((
            "test_random_linearizable_registers",
            unit_case(&l, "linearizable"),
        ));
    }
    for seed in 0..20 {
        let l = gen_linearizable(seed, 5, 300, &["x", "y", "z"], true);
        out.push(("test_random_list_append", unit_case(&l, "list-append")));
    }
    for seed in 0..20 {
        let mut l = gen_linearizable(seed, 5, 60, &["x"], false);
        let reads: Vec<usize> = (0..l.len())
            .filter(|&i| l[i].contains(" ok read "))
            .collect();
        let i = reads[reads.len() / 2];
        l[i] = format!("{} 7", l[i].rsplit_once(' ').unwrap().0);
        out.push((
            "test_random_mutation_detected",
            unit_case(&l, "linearizable"),
        ));
    }
    let mut l = gen_linearizable(7, 10, 400, &["x"], false);
    let i = (0..l.len())
        .filter(|&i| l[i].contains(" ok read "))
        .nth(150)
        .unwrap();
    l[i] = format!("{} 9", l[i].rsplit_once(' ').unwrap().0);
    out.push((
        "test_witness_for_unwritten_value",
        unit_case(&l, "linearizable"),
    ));
    out
}
