//! The call sequences of the conformance programs that compared each SDK with a Go 1.26.5
//! binary before the shared cases existed (deleted in M110; `git log --diff-filter=D --
//! sdk/rust/tests/conformance.sh` names the commit, whose parent has them all). Their outputs
//! equal the Go programs' consoles and records byte for byte (docs/HISTORY.md, Milestone 110);
//! only the history programs' versions below zero are left out.

use crate::model::*;

fn case(name: &'static str, about: &'static str, prog: Prog, runs: Vec<Run>) -> Case {
    Case {
        area: "programs",
        name,
        about,
        prog,
        runs,
    }
}

pub fn all() -> Vec<Case> {
    vec![rust_program(), js_program(), python_program(), history_js(), history_python()]
}

fn rust_program() -> Case {
    let rich = d! {
        "s": "<>&\"\\\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f} é \u{2028} \u{2029} 😀",
        "neg": -5,
        "umax": u64::MAX,
        "imin": i64::MIN,
        "f1": 0.1,
        "fneg0": -0.0f64,
        "f21": 1e21,
        "f20": 1e20,
        "fsm": 1.5e-7,
        "f6": 1e-6,
        "fmix": 123456789.125,
        "f3": 3.0,
        "ftiny": 2.5e-300,
        "fbig": 1e100,
        "f32a": 0.1f32,
        "f32b": 1e22f32,
        "f32c": 3.4e-8f32,
        "t": true,
        "nil": (),
        "list": l![1, "a", false, (), d! {"z": 1, "a": 2}],
        "empty": l![],
        "emap": d! {},
        "nest": d! {"é": 1, "B": 2, "a": 3, "_": 4, "a b": d! {"x": "y"}},
    };
    let mut p = Prog::new();
    let a = p.always("c.always", "always <holds> & \"quotes\"");
    let b = p.always_or_unreachable("c.aou", "always or unreachable");
    let s = p.sometimes("c.sometimes", "sometimes é \u{2028} true");
    let r = p.reachable("c.reach", "reached");
    let u = p.unreachable("c.unreach", "never reached");
    p.always("c.never", "never evaluated");
    let cmp = p.always("c.cmp", "a comparison");
    let scmp = p.sometimes("c.scmp", "a sometimes comparison");
    let nan = p.always("c.nan", "details with a NaN");
    let l1 = p.eventually("c.live_ok", "in time", 10_000);
    let l2 = p.eventually("c.live_late", "never", 0);
    let ps: Vec<usize> = ["p.a", "p.b", "p.c", "p.d", "p.e", "p.f", "p.g", "p.h"]
        .iter()
        .map(|id| p.buggify(id, format!("point {}", id.to_uppercase())))
        .collect();
    p.catalog();
    p.check(a, true, None);
    p.check(a, true, Some(d! {"x": 1}));
    p.check(a, false, Some(rich));
    p.check(a, false, Some(d! {"dup": true}));
    p.check(b, true, Some(d! {}));
    p.check(s, false, None);
    p.check(s, true, Some(d! {"n": 3}));
    p.hit(r, None);
    p.hit(r, None);
    p.hit(u, Some(d! {"why": "test"}));
    p.cmp(cmp, 3, "<=", 5, None);
    p.cmp(cmp, 7, "<", 5, Some(d! {"extra": "e"}));
    p.cmp(cmp, 1, "??", 1, None);
    p.cmp(scmp, 1, ">", 2, None);
    p.cmp(scmp, 2, "==", 2, None);
    p.check(nan, true, Some(d! {"f": f64::NAN}));
    p.check(nan, true, Some(d! {"f": 1}));
    p.check(nan, false, Some(d! {"f": f64::INFINITY}));
    let w = p.begin(l1);
    p.done(w, None);
    p.done(w, None);
    p.await_(l1, None, false);
    let w2 = p.begin(l2);
    p.expire(w2, Some(d! {"k": "v"}));
    p.setup_complete();
    for i in 0..50i64 {
        for (j, pt) in ps.iter().enumerate() {
            p.fire(*pt, if j == 0 { Some(d! {"round": i}) } else { None });
        }
    }
    let run = |n: &'static str| Run::assert(n);
    case(
        "rust",
        "the Rust SDK's conformance program (sdk/rust/tests/conformance.sh until M110) under its three BUGGIFY configurations",
        p,
        vec![run("env").bg_env("1234abcd,500,500"), run("file").bg_file("  beef,1000,300\n"), run("none")],
    )
}

fn js_program() -> Case {
    let rich = d! {
        "s": "<>&\"\\\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f} é \u{2028} \u{2029} 😀 \u{ffff} 中文 ok",
        "bad": x(r"a\xffb"),
        "neg": -5,
        "zero": 0,
        "umax": u64::MAX,
        "imin": i64::MIN,
        "i53": 9007199254740993i64,
        "f1": 0.1,
        "fneg0": -0.0f64,
        "f21": 1e21,
        "fn21": -1e21,
        "f20": 1e20,
        "f64": 18446744073709551616.0,
        "fsm": 1.5e-7,
        "fsm2": 9.99e-7,
        "f6": 1e-6,
        "f6b": 1.234e-6,
        "fmix": 123456789.125,
        "f3": 3.0,
        "ftiny": 2.5e-300,
        "fsub": 5e-324,
        "fmax": f64::MAX,
        "fbig": 1e100,
        "fthird": 1.0 / 3.0,
        "t": true,
        "f": false,
        "nil": (),
        "bytes": bytes(&[0, 1, 2, 250, 255]),
        "hello": bytes(b"hello"),
        "ints": l![1, -2, 3],
        "list": l![1, "a", false, (), d! {"z": 1, "a": 2}, l![]],
        "empty": l![],
        "emap": d! {},
        "nest": d! {"é": 1, "B": 2, "a": 3, "_": 4, "a b": d! {"x": "y"}, "\u{ffff}": 5, "😀": 6, "": 7, "Z": d! {"deep": l![d! {"k": "<v>"}]}},
    };
    let mut p = Prog::new();
    let a = p.always("c.always", "always <holds> & \"quotes\"");
    let b = p.always_or_unreachable("c.aou", "always or unreachable");
    let s = p.sometimes("c.sometimes", "sometimes é \u{2028} true 😀");
    let r = p.reachable("c.reach", "reached");
    let u = p.unreachable("c.unreach", "never reached");
    p.always("c.never", "never evaluated");
    let cmp = p.always("c.cmp", "a comparison");
    let scmp = p.sometimes("c.scmp", "a sometimes comparison");
    let nan = p.always("c.nan", "details with a NaN");
    let uni = p.always("c.ünï", "ünïcödé id \u{1}");
    let l1 = p.eventually("c.live_ok", "in time", 10_000);
    let l2 = p.eventually("c.live_late", "never", 0);
    let ps: Vec<usize> = ["p.a", "p.b", "p.c", "p.d", "p.e", "p.f", "p.g", "p.h"]
        .iter()
        .map(|id| p.buggify(id, format!("point {}", id.to_uppercase())))
        .collect();
    p.catalog();
    p.check(a, true, None);
    p.check(a, true, Some(d! {"x": 1}));
    p.check(a, false, Some(rich));
    p.check(a, false, Some(d! {"dup": true}));
    p.check(b, true, Some(d! {}));
    p.check(s, false, None);
    p.check(s, true, Some(d! {"n": 3}));
    p.hit(r, None);
    p.hit(r, None);
    p.hit(u, Some(d! {"why": "test"}));
    p.cmp(cmp, 3, "<=", 5, None);
    p.cmp(cmp, 7, "<", 5, Some(d! {"extra": "e"}));
    p.cmp(cmp, 1, "??", 1, None);
    p.cmp_helper(scmp, "greater_than", 1, 2, None);
    p.cmp(scmp, 2, "==", 2, None);
    p.check(nan, true, Some(d! {"f": f64::NAN}));
    p.check(nan, true, Some(d! {"f": 1}));
    p.check(nan, false, Some(d! {"f": f64::INFINITY}));
    p.check(nan, false, Some(d! {"f": f64::NEG_INFINITY}));
    p.check(uni, true, Some(d! {"k": "v"}));
    let w = p.begin(l1);
    p.done(w, None);
    p.done(w, None);
    p.await_(l1, None, false);
    let w2 = p.begin(l2);
    p.expire(w2, Some(d! {"k": "v"}));
    p.setup_complete();
    for i in 0..50i64 {
        for (j, pt) in ps.iter().enumerate() {
            let det = match j {
                0 => Some(d! {"round": i}),
                1 | 2 => Some(d! {"round": i, "stmt": "x"}),
                _ => None,
            };
            p.fire(*pt, det);
        }
    }
    p.catalog();
    let run = |n: &'static str| Run::assert(n);
    case(
        "js",
        "the JavaScript SDK's conformance program (sdk/js/test/conformance.sh until M110) under its five BUGGIFY configurations",
        p,
        vec![
            run("env").bg_env("1234abcd,500,500"),
            run("file").bg_file("  beef,1000,300\n"),
            run("none"),
            run("odd").bg_env("FFFFFFFFFFFFFFFF,abc,1000"),
            run("bad").bg_env("0x12"),
        ],
    )
}

fn python_program() -> Case {
    let rich = d! {
        "s": "<>&\"\\\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f} é \u{2028} \u{2029} 😀 日本語",
        "neg": -5,
        "zero": 0,
        "umax": u64::MAX,
        "imin": i64::MIN,
        "imax": i64::MAX,
        "f1": 0.1,
        "fneg0": -0.0f64,
        "f0": 0.0,
        "f21": 1e21,
        "f20": 1e20,
        "fsm": 1.5e-7,
        "f6": 1e-6,
        "f6b": 9.99e-7,
        "fmix": 123456789.125,
        "f3": 3.0,
        "fneg": -2.5,
        "ftiny": 2.5e-300,
        "fbig": 1e100,
        "fmaxf": f64::MAX,
        "fsub": 5e-324,
        "fthird": 1.0 / 3.0,
        "f1e15": 123456789012345680.0,
        "t": true,
        "f": false,
        "nil": (),
        "bytes": bytes(b"hi\x00\xff"),
        "ebytes": bytes(b""),
        "list": l![1, "a", false, (), d! {"z": 1, "a": 2}, l![l![]]],
        "empty": l![],
        "emap": d! {},
        "nest": d! {"é": 1, "B": 2, "a": 3, "_": 4, "a b": d! {"x": "y"}, "😀": 5, "Z": 6, "ÿ": 7, "~": 8},
        "deep": d! {"l1": d! {"l2": d! {"l3": l![d! {"k": "v", "j": l![1.25, -0.5]}]}}},
        "<key>": "&",
        "inv": x(r"a\xffb\xc3"),
    };
    // the invalid-UTF-8 key of tests/conformance.py ("\udcffk") next to the other keys
    let V::Map(mut m) = rich else { unreachable!() };
    m.push((x(r"\xffk"), d! {x(r"a\xfe") => 1, "a" => 2}));
    let rich = V::Map(m);
    let mut p = Prog::new();
    let a = p.always("c.always", "always <holds> & \"quotes\"");
    let b = p.always_or_unreachable("c.aou", "always or unreachable");
    let s = p.sometimes("c.sometimes", "sometimes é \u{2028} true");
    let r = p.reachable("c.reach", "reached");
    let u = p.unreachable("c.unreach", "never reached");
    p.always("c.never", "never evaluated");
    let cmp = p.always("c.cmp", "a comparison");
    let scmp = p.sometimes("c.scmp", "a sometimes comparison");
    let gt = p.always("c.gt", "greater than");
    let ge = p.sometimes("c.ge", "greater than or equal");
    let lt = p.always("c.lt", "less than");
    let le = p.always("c.le", "less than or equal");
    let nan = p.always("c.nan", "details with a NaN");
    let uni = p.always("c.ünï/日本", "unicode id and \u{1} control");
    let lp = p.always("c.loop", "evaluated many times");
    let l1 = p.eventually("c.live_ok", "in time", 10_000);
    let l2 = p.eventually("c.live_late", "never", 0);
    let l3 = p.eventually("c.live_await", "await, true at once", 1500);
    let ps: Vec<usize> = ["p.a", "p.b", "p.c", "p.d", "p.e", "p.f", "p.g", "p.h", "p.é"]
        .iter()
        .map(|id| p.buggify(id, format!("point {}", id.to_uppercase())))
        .collect();
    p.catalog();
    p.check(a, true, None);
    p.check(a, true, Some(d! {"x": 1}));
    p.check(a, false, Some(rich));
    p.check(a, false, Some(d! {"dup": true}));
    p.check(b, true, Some(d! {}));
    p.check(b, false, Some(d! {"why": ()}));
    p.check(s, false, None);
    p.check(s, true, Some(d! {"n": 3}));
    p.check(s, true, Some(d! {"n": 4}));
    p.hit(r, None);
    p.hit(r, Some(d! {"again": 1}));
    p.hit(u, Some(d! {"why": "test"}));
    p.hit(u, None);
    p.cmp(cmp, 3, "<=", 5, None);
    p.cmp(cmp, 7, "<", 5, Some(d! {"extra": "e", "left": "overwritten"}));
    p.cmp(cmp, 1, "??", 1, None);
    p.cmp(scmp, 1, ">", 2, None);
    p.cmp(scmp, 2, "==", 2, None);
    p.cmp_helper(gt, "greater_than", 2, 1, None);
    p.cmp_helper(gt, "greater_than", 1, 2, Some(d! {"k": "v"}));
    p.cmp_helper(ge, "greater_than_or_equal_to", -1, 0, None);
    p.cmp_helper(ge, "greater_than_or_equal_to", 0, 0, None);
    p.cmp_helper(lt, "less_than", i64::MIN, i64::MAX, None);
    p.cmp_helper(le, "less_than_or_equal_to", 5, 4, None);
    p.cmp(le, 4, "!=", 4, None);
    p.check(nan, true, Some(d! {"f": f64::NAN}));
    p.check(nan, true, Some(d! {"f": 1}));
    p.check(nan, false, Some(d! {"nested": d! {"f": f64::INFINITY}}));
    p.check(nan, false, Some(d! {"f": 2}));
    p.check(uni, false, Some(d! {"ключ": "значение", "\t": "\t"}));
    for i in 0..1000i64 {
        p.check(lp, i != 500, Some(d! {"i": i}));
    }
    let w = p.begin(l1);
    p.done(w, None);
    p.done(w, None);
    p.await_(l1, None, false);
    let w2 = p.begin(l2);
    p.expire(w2, Some(d! {"k": "v"}));
    p.done(w2, None);
    p.await_(l3, Some(d! {"probe": "ping"}), false);
    p.setup_complete();
    for i in 0..60i64 {
        for (j, pt) in ps.iter().enumerate() {
            let det = match j {
                0 => Some(d! {"round": i}),
                1 => Some(d! {"hit": "replaced", "x": l![i]}),
                _ => None,
            };
            p.fire(*pt, det);
        }
    }
    p.setup_complete();
    let run = |n: &'static str| Run::assert(n);
    case(
        "python",
        "the Python SDK's conformance program (sdk/python/tests/conformance.sh until M110, with its invalid-UTF-8 details) under its eight BUGGIFY configurations",
        p,
        vec![
            run("env").bg_env("1234abcd,500,500"),
            run("file").bg_file("  beef,1000,300\n"),
            run("none"),
            run("odd").bg_env("  BEEF,07,+5\t"),
            run("prefix").bg_env("0xbeef"),
            run("overflow").bg_env("1ffffffffffffffff"),
            run("sign").bg_env("ab, 300,1_0"),
            run("all").bg_env("ffffffffffffffff,1000,1000"),
        ],
    )
}

/// The history sequence shared by the JavaScript and Python recorder conformance programs.
/// Their Go halves passed U+FFFD where the other language passed a lone surrogate, and
/// decimal strings where it passed integers; those Go values are used here. The versions
/// below zero of the Go programs are left out: the Rust recorder's `ver` takes a `u64`.
fn history_common(p: &mut Prog) -> (usize, usize, usize) {
    let kv = p.history("kv");
    let la = p.history("la");
    let other = p.history("Other.hist_2-x");
    let mut ascii = String::new();
    for c in 0u8..0x80 {
        ascii.push(c as char);
    }
    let vals: Vec<String> = [
        "",
        "nil",
        "Nil",
        "nil ",
        " nil",
        "nilnil",
        "%",
        "%6Eil",
        "@",
        " ",
        "\t",
        "\n",
        "\r",
        "\u{0}",
        "\u{1}",
        "\u{1f}",
        "\u{7f}",
        ",",
        "[",
        "]",
        ":",
        "a,b[c]:d@e%f\tg",
        "a b",
        "plain-Token_1.2",
        "~!#$^&*()_+-={}|\\;'\"<>?/`",
        "é",
        "café au lait",
        "中文",
        "日本語,テスト",
        "😀",
        "a😀b c",
        "\u{a0}",
        "\u{2028}\u{2029}",
        "\u{fffd}",
        "\u{ffff}",
        "a\u{fffd}b",
        "\u{fffd}\u{fffd}",
        "42",
        "-7",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(std::iter::once(ascii))
    .collect();
    for (i, v) in vals.iter().enumerate() {
        let k = format!("k{}", i);
        let o = p.invoke(kv, (i % 3) as i64, "write", vec![Arg::Tok(k.clone().into()), enc(v.as_str())]);
        match i % 4 {
            0 => p.ok_same(o),
            1 => p.ok(o, vec![Arg::Tok(k.into()), ver(v.as_str(), i as u64 * 1000003)]),
            2 => p.fail(o),
            _ => p.info(o),
        }
    }
    // Go: {0, 1, -1, 83, 0, 2^53, MaxInt64, MinInt64, 2^53+1, -(2^53+1)}; negatives left out
    for (i, n) in [
        (0, 0u64),
        (1, 1),
        (3, 83),
        (4, 0),
        (5, 9007199254740992),
        (6, i64::MAX as u64),
        (8, 9007199254740993),
    ] {
        let o = p.invoke(kv, 10 + i, "write", vec!["kv".into(), "v".into()]);
        p.ok(o, vec!["kv".into(), ver("v", n)]);
    }
    let o = p.invoke(kv, 20, "read", vec!["k1".into(), "k2".into(), "k3".into()]);
    p.ok(o, vec!["k1".into(), von(Some("")), "k2".into(), von(None), "k3".into(), Arg::Nil]);
    let o = p.invoke(kv, 20, "read", vec!["k4".into()]);
    p.ok(o, vec!["k4".into(), von(Some("x y"))]);
    let odd = hlist(&["a", "b,c", "[x]", "nil", "", "é", "1 2", "%", ":"]);
    let o = p.invoke(la, 1, "append", vec!["x".into(), "1".into()]);
    p.ok_same(o);
    let o = p.invoke(la, 1, "read", vec!["x".into()]);
    p.ok(o, vec!["x".into(), hlist(&["1"])]);
    let o = p.invoke(la, 2, "read", vec!["empty".into()]);
    p.ok(o, vec!["empty".into(), hlist(&[])]);
    let o = p.invoke(la, 2, "read", vec!["odd".into()]);
    p.ok(o, vec!["odd".into(), odd]);
    let mops = [
        amop("x", "2"),
        rmop("y"),
        amop("k:1", "e,2"),
        rmop(""),
        rrmop("y", &[]),
        rrmop("", &["a", "b c"]),
    ];
    let o = p.invoke(la, 3, "txn", vec![mops[0].clone(), mops[1].clone(), mops[2].clone(), mops[3].clone()]);
    p.ok(o, vec![mops[0].clone(), mops[4].clone(), mops[2].clone(), mops[5].clone()]);
    let o = p.invoke(la, 3, "txn", vec![amop("x", "3")]);
    p.fail(o);
    let o = p.invoke(la, 4, "txn", vec![rmop("x"), amop("y", "1")]);
    p.info(o);
    p.record(other, 7, "invoke", "cas", vec!["k".into(), "a".into(), "b".into()]);
    p.record(other, 7, "ok", "cas", vec!["k".into(), "a".into(), "b@9".into()]);
    p.record(other, 8, "info", "read", vec![]);
    p.record(other, 0, "fail", "write", vec!["".into(), "x".into(), "".into()]);
    let o = p.invoke(kv, 9, "write", vec!["".into(), "".into()]);
    p.ok(o, vec!["".into(), "".into()]);
    let o = p.invoke(kv, 9, "write", vec!["".into(), "".into()]);
    p.ok_same(o);
    let o = p.invoke(kv, 5, "noop", vec![]);
    p.ok(o, vec![]);
    let o = p.invoke(kv, 5, "noop", vec![]);
    p.fail(o);
    let o = p.invoke(kv, 5, "noop", vec![]);
    p.info(o);
    let o = p.invoke(kv, 1 << 40, "write", vec!["big".into(), "1".into()]);
    p.ok_same(o);
    let o = p.invoke(kv, -5, "write", vec!["neg".into(), "1".into()]);
    p.ok_same(o);
    let o = p.invoke(kv, i64::MAX, "write", vec!["max".into(), "1".into()]);
    p.info(o);
    let o = p.invoke(kv, i64::MIN, "write", vec!["min".into(), "1".into()]);
    p.fail(o);
    let a = p.invoke(kv, 30, "write", vec!["i".into(), "1".into()]);
    let b = p.invoke(la, 30, "append", vec!["i".into(), "1".into()]);
    let c = p.invoke(other, 31, "read", vec!["i".into()]);
    p.ok_same(b);
    p.ok(c, vec!["i".into(), "1".into()]);
    p.info(a);
    let o = p.invoke(other, 40, "write", vec!["x\u{fffd}".into(), "é".into()]);
    p.ok_same(o);
    let o = p.invoke(other, 41, "write", vec!["n".into(), "42".into()]);
    p.ok(o, vec!["n".into(), "42".into(), "-7".into()]);
    (kv, la, other)
}

fn hist_runs() -> Vec<Run> {
    let run = |n: &'static str| Run::hist(n).hist_out(n);
    vec![run("append"), run("create"), run("stderr"), run("fallback"), run("off")]
}

fn history_js() -> Case {
    let mut p = Prog::new();
    history_common(&mut p);
    case(
        "history_js",
        "the JavaScript history recorder's conformance program (sdk/js/test/history-conformance.sh until M110, without its negative versions) under its five sink settings",
        p,
        hist_runs(),
    )
}

fn history_python() -> Case {
    let mut p = Prog::new();
    let (kv, _, other) = history_common(&mut p);
    // invalid UTF-8 (Python passes bytes or surrogateescape strings)
    let bad = [
        r"\xff",
        r"a\xffb",
        r"\x80",
        r"\xc3",
        r"\xc3(",
        r"\xed\xa0\x80",
        r"\xe2\x82",
        r"é\xff中",
        r"\xff",
        "\u{fffd}",
        r"\xff\xfe nil",
        "nil",
    ];
    for (i, b) in bad.iter().enumerate() {
        let o = p.invoke(kv, 50 + i as i64, "write", vec!["b".into(), enc(x(b))]);
        p.ok(o, vec!["b".into(), ver(x(b), i as u64)]);
    }
    let o = p.invoke(kv, 60, "write", vec![x(r"\xffraw").into(), "x".into()]);
    p.ok_same(o);
    let o = p.invoke(kv, 61, "write", vec!["k".into(), x(r"é\xff").into()]);
    p.ok(o, vec!["k".into(), Arg::List(vec![x(r"\xff"), "ok".into(), x(r"\xc3")])]);
    p.record(other, 62, "ok", "read", vec![x(r"\xe2\x82").into(), "".into()]);
    case(
        "history_python",
        "the Python history recorder's conformance program (sdk/python/tests/history_conformance.sh until M110, without its negative versions) under its five sink settings",
        p,
        hist_runs(),
    )
}
