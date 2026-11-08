//! The focused cases: one behaviour each. To add a case, add a function call here (or a new
//! function to `all`), run `HYDO_BLESS=1 cargo test --test cases`, and commit the new files in
//! sdk/cases; the other SDKs' case tests pick them up.

use crate::model::*;

fn case(area: &'static str, name: &'static str, about: &'static str, prog: Prog, runs: Vec<Run>) -> Case {
    Case { area, name, about, prog, runs }
}

/// One run with assertion records to an empty file.
fn one() -> Vec<Run> {
    vec![Run::assert("")]
}

pub fn all() -> Vec<Case> {
    let mut v = Vec::new();
    v.extend(assert_cases());
    v.extend(compare_cases());
    v.extend(details_cases());
    v.extend(catalog_cases());
    v.extend(eventually_cases());
    v.extend(buggify_cases());
    v.extend(output_cases());
    v.extend(history_cases());
    v
}

// ---- assertion types and deduplication ----

fn assert_cases() -> Vec<Case> {
    let mut v = Vec::new();

    let mut p = Prog::new();
    let a = p.always("t.always", "an always property");
    p.check(a, true, None);
    p.check(a, true, Some(d! {"again": 1}));
    p.check(a, false, Some(d! {"x": 1}));
    p.check(a, false, Some(d! {"x": 2}));
    p.check(a, true, None);
    v.push(case(
        "assert",
        "always",
        "always: the first pass and the first failure are written, each with its own details, and nothing after them",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always_or_unreachable("t.aou", "an always-or-unreachable property");
    p.check(a, true, Some(d! {}));
    p.check(a, false, Some(d! {"why": ()}));
    p.check(a, false, None);
    v.push(case(
        "assert",
        "always_or_unreachable",
        "always_or_unreachable is written like always",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.sometimes("t.sometimes", "a sometimes property");
    p.check(a, false, Some(d! {"n": 1}));
    p.check(a, true, Some(d! {"n": 2}));
    p.check(a, true, Some(d! {"n": 3}));
    p.check(a, false, None);
    v.push(case(
        "assert",
        "sometimes",
        "sometimes: a false evaluation is never written, the first true one is, later ones are not",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.sometimes("t.sometimes_never", "a sometimes property that is never true");
    p.check(a, false, None);
    p.check(a, false, Some(d! {"n": 1}));
    v.push(case(
        "assert",
        "sometimes_false_only",
        "a sometimes property that is only ever false writes nothing",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.reachable("t.reach", "a reachable location");
    p.hit(a, None);
    p.hit(a, Some(d! {"again": 1}));
    v.push(case("assert", "reachable", "reachable: the first hit is written as ok:true", p, one()));

    let mut p = Prog::new();
    let a = p.unreachable("t.unreach", "an unreachable location");
    p.hit(a, Some(d! {"why": "test"}));
    p.hit(a, None);
    v.push(case(
        "assert",
        "unreachable",
        "unreachable: the first hit is written as ok:false (a violation)",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always("t.dup", "declared twice");
    let b = p.always("t.dup", "declared twice");
    p.check(a, true, None);
    p.check(b, true, None);
    p.check(a, true, None);
    v.push(case(
        "assert",
        "same_id_twice",
        "two assertions with the same id are deduplicated separately",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always("t.none", "no details");
    let b = p.always("t.empty", "empty details");
    let c = p.always("t.null", "a null value");
    p.check(a, true, None);
    p.check(b, true, Some(d! {}));
    p.check(c, true, Some(d! {"v": ()}));
    v.push(case(
        "assert",
        "details_absent_or_empty",
        "no details and empty details both leave out the details field; a null value is kept",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always("t.loop", "evaluated many times");
    for i in 0..200 {
        p.check(a, i != 120, Some(d! {"i": i}));
    }
    v.push(case(
        "assert",
        "many_evaluations",
        "200 evaluations write two records: the first pass and the first failure",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always("t.\"quoted\" <id> & é", "a message with \"quotes\", <html> & \\ \n\t\u{1} é 😀 \u{2028}");
    p.check(a, false, None);
    p.catalog();
    v.push(case(
        "assert",
        "escaped_id_and_message",
        "ids and messages are JSON strings with Go's escapes in both the evaluation and the declaration",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always(x(r"t.bad\xff"), x(r"invalid \xc3 UTF-8 \xff\xfe in a message"));
    p.catalog();
    p.check(a, true, None);
    v.push(case(
        "assert",
        "invalid_utf8_message",
        "an invalid byte in an id or a message is written as \\ufffd, as Go 1.26 writes it",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always("t.a", "a");
    let b = p.sometimes("t.b", "b");
    let r = p.reachable("t.r", "r");
    p.check(b, true, None);
    p.hit(r, None);
    p.check(a, false, Some(d! {"order": 1}));
    p.check(a, true, Some(d! {"order": 2}));
    v.push(case(
        "assert",
        "interleaved",
        "records are written in evaluation order, whatever the declaration order",
        p,
        one(),
    ));
    v
}

// ---- numeric comparisons ----

fn compare_cases() -> Vec<Case> {
    let mut v = Vec::new();
    for (op, name, t, f) in [
        ("<", "lt", (1, 2), (2, 2)),
        ("<=", "le", (2, 2), (3, 2)),
        ("==", "eq", (5, 5), (5, 6)),
        ("!=", "ne", (5, 6), (5, 5)),
        (">=", "ge", (2, 2), (1, 2)),
        (">", "gt", (3, 2), (2, 2)),
    ] {
        let mut p = Prog::new();
        let a = p.always(format!("c.always_{}", name), format!("always {} {} {}", "left", op, "right"));
        let s = p.sometimes(format!("c.sometimes_{}", name), format!("sometimes left {} right", op));
        p.cmp(a, t.0, op, t.1, None);
        p.cmp(a, f.0, op, f.1, None);
        p.cmp(s, f.0, op, f.1, None);
        p.cmp(s, t.0, op, t.1, None);
        let about: &'static str = Box::leak(
            format!(
                "cmp with {}: the details carry left, op and right; a false sometimes comparison is not written",
                op
            )
            .into_boxed_str(),
        );
        v.push(case("compare", Box::leak(format!("op_{}", name).into_boxed_str()), about, p, one()));
    }

    let mut p = Prog::new();
    let a = p.always("c.unknown", "an unknown operator");
    p.cmp(a, 1, "??", 1, None);
    p.cmp(a, 1, "=<", 2, Some(d! {"k": "v"}));
    v.push(case("compare", "unknown_op", "an unknown operator evaluates to false", p, one()));

    let mut p = Prog::new();
    let a = p.always("c.details", "a comparison with details");
    p.cmp(a, 7, "<", 5, Some(d! {"extra": "e", "left": "overwritten", "op": 1, "right": l![1]}));
    p.cmp(a, 1, "<", 5, Some(d! {"z": true}));
    v.push(case(
        "compare",
        "details_merged",
        "the caller's details are kept and left, op and right replace keys of the same name",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.always("c.extremes", "int64 extremes");
    let b = p.always("c.extremes_false", "int64 extremes, false");
    p.cmp(a, i64::MIN, "<", i64::MAX, None);
    p.cmp(b, i64::MAX, "<=", i64::MIN, None);
    p.cmp(b, -1, "==", -1, None);
    v.push(case(
        "compare",
        "int64_extremes",
        "comparisons of int64 extremes and negative numbers",
        p,
        one(),
    ));

    v
}

// ---- details encoding ----

fn details_case(name: &'static str, about: &'static str, d: V) -> Case {
    let mut p = Prog::new();
    let a = p.always(format!("d.{}", name), "details encoding");
    p.check(a, true, Some(d));
    case("details", name, about, p, one())
}

fn details_cases() -> Vec<Case> {
    vec![
        details_case(
            "string_escapes",
            "strings: Go's escapes for quote, backslash, \\b \\f \\n \\r \\t, other control characters as \\u00xx, and the HTML characters",
            d! {"s": "<>&\"\\\n\r\t\u{8}\u{c}\u{0}\u{1}\u{1f}\u{7f}", "html": "<script>a && b</script>", "slash": "a/b"},
        ),
        details_case(
            "string_unicode",
            "non-ASCII text is written as UTF-8 except U+2028 and U+2029, which are escaped",
            d! {"latin": "é ÿ", "cjk": "中文 日本語", "emoji": "😀 a😀b", "u2028": "a\u{2028}b\u{2029}c", "nbsp": "a\u{a0}b\u{85}c", "fffd": "\u{fffd}", "ffff": "\u{ffff}", "bom": "\u{feff}x"},
        ),
        details_case(
            "string_invalid_utf8",
            "each invalid UTF-8 byte of a string is written as \\ufffd (Go 1.26; Go 1.27 writes a raw U+FFFD)",
            d! {"a": x(r"a\xffb"), "trunc": x(r"\xe2\x82"), "lead": x(r"\xc3("), "surrogate": x(r"\xed\xa0\x80"), "end": x(r"ok\xc3"), "mixed": x(r"é\xff中")},
        ),
        details_case(
            "keys_invalid_utf8",
            "an invalid byte in a key is written as \\ufffd and the key sorts by its raw bytes",
            d! {x(r"\xffk") => d! {x(r"a\xfe") => 1, "a" => 2}, "a" => 1, "~" => 2},
        ),
        details_case(
            "integers",
            "int64 and uint64 values, including the extremes and 2^53 + 1",
            d! {"zero": 0, "neg": -5, "imin": i64::MIN, "imax": i64::MAX, "umax": u64::MAX, "i53": 9007199254740993i64, "u": 7u64},
        ),
        details_case(
            "floats",
            "float64 values in Go's format: shortest digits, exponent form below 1e-6 and from 1e21, -0",
            d! {
                "f0": 0.0, "fneg0": -0.0, "f1": 0.1, "f3": 3.0, "fneg": -2.5, "fthird": 1.0 / 3.0,
                "f20": 1e20, "f21": 1e21, "fn21": -1e21, "f64": 18446744073709551616.0, "fbig": 1e100,
                "fmax": f64::MAX, "f6": 1e-6, "f6b": 1.234e-6, "fsm": 1.5e-7, "fsm2": 9.99e-7,
                "ftiny": 2.5e-300, "fsub": 5e-324, "fmix": 123456789.125, "f17": 123456789012345680.0,
            },
        ),
        details_case(
            "float32",
            "float32 values (Go's float32 format, shortest digits of the 32-bit value); JavaScript and Python have no float32",
            d! {"a": 0.1f32, "b": 1e22f32, "c": 3.4e-8f32, "d": 16777216.0f32, "e": -0.0f32, "f": 1e21f32, "g": 1.5f32},
        ),
        details_case(
            "bytes",
            "byte strings are written as base64 (Go's []byte)",
            d! {"b": bytes(&[0, 1, 2, 250, 255]), "hello": bytes(b"hello"), "empty": bytes(b""), "hi": bytes(b"hi\x00\xff")},
        ),
        details_case("null_and_bool", "null, true and false", d! {"n": (), "t": true, "f": false}),
        details_case(
            "lists",
            "lists of mixed values, empty and nested lists",
            d! {"mixed": l![1, "a", false, (), d! {"z": 1, "a": 2}, l![]], "empty": l![], "nested": l![l![l![]], l![1, l![2]]]},
        ),
        details_case(
            "key_order",
            "map keys are written in the order of their UTF-8 bytes",
            d! {"é" => 1, "B" => 2, "a" => 3, "_" => 4, "a b" => 5, "\u{ffff}" => 6, "😀" => 7, "" => 8, "Z" => 9, "ÿ" => 10, "~" => 11, "<key>" => "&", "aa" => 12, "a\u{0}" => 13},
        ),
        details_case(
            "nested_maps",
            "maps inside maps and lists, each sorted",
            d! {"deep": d! {"l1": d! {"l2": d! {"l3": l![d! {"k": "v", "j": l![1.25, -0.5]}]}}}, "emap": d! {}, "list": l![d! {"b": 1, "a": l![d! {"y": 2, "x": 1}]}]},
        ),
        {
            let mut p = Prog::new();
            let a = p.always("d.nan", "details with a NaN");
            let b = p.always("d.inf", "details with an infinity");
            p.check(a, true, Some(d! {"f": f64::NAN}));
            p.check(a, true, Some(d! {"f": 1}));
            p.check(a, false, Some(d! {"f": 2}));
            p.check(b, false, Some(d! {"nested": d! {"f": f64::INFINITY}}));
            p.check(b, false, Some(d! {"f": 3}));
            p.check(b, true, Some(d! {"list": l![f64::NEG_INFINITY]}));
            p.check(b, true, Some(d! {"f": 4}));
            case(
                "details",
                "nan_and_infinity",
                "a NaN or an infinity anywhere in the details drops the record (Go's json.Marshal fails), and the outcome counts as written",
                p,
                one(),
            )
        },
        {
            let mut p = Prog::new();
            let r = p.reachable("d.reach", "a reachable location with details");
            let u = p.unreachable("d.unreach", "an unreachable location with details");
            p.hit(r, Some(d! {"where": "here", "n": l![1, 2]}));
            p.hit(u, Some(d! {"order": 7}));
            case("details", "on_hit", "hit records carry their details", p, one())
        },
    ]
}

// ---- the catalog and the lifecycle line ----

fn catalog_cases() -> Vec<Case> {
    let mut v = Vec::new();

    let mut p = Prog::new();
    p.always("k.always", "always");
    p.always_or_unreachable("k.aou", "always or unreachable");
    p.sometimes("k.sometimes", "sometimes");
    p.reachable("k.reachable", "reachable");
    p.unreachable("k.unreachable", "unreachable");
    p.eventually("k.eventually", "eventually", 5000);
    p.buggify("k.point", "a fault point");
    p.catalog();
    v.push(case(
        "catalog",
        "every_type",
        "the catalog lists every declaration in declaration order; a BUGGIFY point is a sometimes property buggify.<id>",
        p,
        one(),
    ));

    let mut p = Prog::new();
    p.catalog();
    v.push(case("catalog", "empty", "a catalog with nothing declared writes nothing", p, one()));

    let mut p = Prog::new();
    let a = p.always("k.a", "first");
    p.check(a, true, None);
    p.catalog();
    p.sometimes("k.b", "declared after the first catalog");
    p.catalog();
    v.push(case(
        "catalog",
        "twice",
        "each catalog call writes every declaration so far, also after evaluations",
        p,
        one(),
    ));

    let mut p = Prog::new();
    p.setup_complete();
    let a = p.always("k.a", "between");
    p.check(a, true, None);
    p.setup_complete();
    v.push(case("catalog", "setup_complete", "HYDO-SETUP-COMPLETE is written at every call", p, one()));
    v
}

// ---- eventually ----

fn eventually_cases() -> Vec<Case> {
    let mut v = Vec::new();

    let mut p = Prog::new();
    let l = p.eventually("e.in_time", "true within 10 s", 10_000);
    let w = p.begin(l);
    p.done(w, Some(d! {"n": 1}));
    p.done(w, Some(d! {"n": 2}));
    v.push(case(
        "eventually",
        "begin_done",
        "an episode that ends in time writes ok:true with elapsed_ms and within_ms; a second done is ignored",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let l = p.eventually("e.late", "never true", 0);
    let w = p.begin(l);
    p.expire(w, Some(d! {"k": "v"}));
    p.done(w, None);
    p.expire(w, None);
    v.push(case("eventually", "expire", "expire writes ok:false; the episode is then over", p, one()));

    let mut p = Prog::new();
    let l = p.eventually("e.await", "true at once", 1500);
    p.await_(l, Some(d! {"probe": "ping"}), false);
    v.push(case(
        "eventually",
        "await",
        "await with a condition that is true at once writes ok:true with elapsed_ms 0",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let l = p.eventually("e.await_async", "true at once (async)", 250);
    p.await_(l, None, true);
    v.push(case(
        "eventually",
        "await_async",
        "the async await (Go: Await) writes the same record",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let l = p.eventually("e.episodes", "several episodes", 10_000);
    let w1 = p.begin(l);
    let w2 = p.begin(l);
    p.done(w1, Some(d! {"episode": 1}));
    p.done(w2, Some(d! {"episode": 2}));
    let l0 = p.eventually("e.zero", "within 0", 0);
    let w3 = p.begin(l0);
    p.expire(w3, Some(d! {"episode": 3}));
    let w4 = p.begin(l0);
    p.expire(w4, Some(d! {"episode": 4}));
    v.push(case(
        "eventually",
        "episodes_deduplicated",
        "each outcome of an eventually property is written once per process, whatever the episode",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let l = p.eventually("e.keys", "details with the same keys", 2000);
    let w = p.begin(l);
    p.done(w, Some(d! {"elapsed_ms": "mine", "within_ms": "mine", "other": true}));
    v.push(case(
        "eventually",
        "details_merged",
        "elapsed_ms and within_ms replace caller keys of the same name",
        p,
        one(),
    ));
    v
}

// ---- BUGGIFY ----

fn points_prog(rounds: usize) -> Prog {
    let mut p = Prog::new();
    let ids = ["b.a", "b.b", "b.c", "b.d", "b.e", "b.f", "b.g", "b.h", "b.é"];
    let ps: Vec<usize> = ids.iter().map(|id| p.buggify(id, format!("point {}", id))).collect();
    let a = p.always("b.other", "an ordinary assertion next to the points");
    p.catalog();
    p.check(a, true, None);
    for i in 0..rounds {
        for (j, pt) in ps.iter().enumerate() {
            let det = match j {
                0 => Some(d! {"round": i as i64}),
                1 => Some(d! {"hit": "replaced", "x": l![i as i64]}),
                _ => None,
            };
            p.fire(*pt, det);
        }
    }
    p
}

fn buggify_cases() -> Vec<Case> {
    let mut v = Vec::new();
    let bg = |name: &'static str| Run::assert(name);
    v.push(case(
        "buggify",
        "configurations",
        "nine fault points evaluated 30 times under each configuration: which points are enabled, which evaluations fire, the HYDO-BUGGIFY line and the first-fire records",
        points_prog(30),
        vec![
            bg("none"),
            bg("env").bg_env("1234abcd,500,500"),
            bg("env_seed_only").bg_env("5abc"),
            bg("file").bg_file("  beef,1000,300\n"),
            bg("env_over_file").bg_env("77").bg_file("beef,1000,1000"),
            bg("empty_env_reads_file").bg_env("").bg_file("1234,1000,500"),
            bg("all").bg_env("ffffffffffffffff,1000,1000"),
            bg("enable_zero").bg_env("5,0,1000"),
            bg("fire_zero").bg_env("5,1000,0"),
            bg("upper_spaces").bg_env("  BEEF,07,+5\t"),
            bg("bad_enable").bg_env("FFFFFFFFFFFFFFFF,abc,1000"),
            bg("bad_fields").bg_env("ab, 300,1_0"),
            bg("extra_fields").bg_env("ab,600,600,7,x"),
            bg("prefix_0x").bg_env("0x12"),
            bg("overflow").bg_env("1ffffffffffffffff"),
            bg("signed").bg_env("-5"),
            bg("empty_file").bg_file(""),
            bg("garbage_file").bg_file("seed=5\n"),
        ],
    ));

    let mut p = Prog::new();
    let a = p.buggify("b.one", "always fires");
    p.fire(a, None);
    p.fire(a, Some(d! {"second": true}));
    p.catalog();
    v.push(case(
        "buggify",
        "first_fire",
        "the configuration is read at the first evaluation, which writes HYDO-BUGGIFY; only the first firing is recorded, with \"hit\" (the evaluation's index at the point)",
        p,
        vec![Run::assert("").bg_env("1,1000,1000")],
    ));

    let mut p = Prog::new();
    let a = p.buggify("b.off", "never configured");
    p.fire(a, None);
    p.fire(a, None);
    p.catalog();
    v.push(case(
        "buggify",
        "not_configured",
        "without a configuration no point fires and nothing is written but the catalog",
        p,
        one(),
    ));

    let mut p = Prog::new();
    let a = p.buggify("b.late", "declared before");
    let b = p.always("b.x", "an assertion");
    p.check(b, true, None);
    p.fire(a, Some(d! {"hit": "mine", "k": "v"}));
    let c = p.buggify("b.later", "declared after the first evaluation");
    p.fire(c, None);
    p.fire(a, None);
    v.push(case(
        "buggify",
        "declared_late",
        "the HYDO-BUGGIFY line lists the points declared before the first evaluation; a later point still fires by its own id; \"hit\" replaces a caller key",
        p,
        vec![Run::assert("").bg_env("abc,1000,1000")],
    ));
    v
}

// ---- where records go ----

fn output_cases() -> Vec<Case> {
    let mut p = Prog::new();
    let a = p.always("o.a", "an assertion");
    let b = p.buggify("o.p", "a point");
    p.catalog();
    p.check(a, false, Some(d! {"k": "v"}));
    p.fire(b, None);
    p.setup_complete();
    let run = |n: &'static str, o: &'static str| Run::assert(n).out(o).bg_env("1,1000,1000");
    vec![case(
        "output",
        "assert_out",
        "HYDO_ASSERT_OUT: a file (appended to, never created), stderr, a missing file (standard error), or off (nothing anywhere)",
        p,
        vec![
            run("file", "file"),
            run("append", "append"),
            run("stderr", "stderr"),
            run("missing", "missing"),
            run("off", "off"),
        ],
    )]
}

// ---- history records ----

/// The messaging case: generic records whose item tokens the Rust `Item` makes (so every SDK
/// replays them with its generic recorder; `tests/history.rs` checks that the Rust `Messages`
/// recorder writes the same lines).
fn messages_prog() -> Prog {
    use hydo_assert::history::messages::{item_enc, Item, Outcome};
    let t = |items: &[Item]| -> Vec<Arg> { items.iter().map(|i| Arg::Tok(i.token().into())).collect() };
    let with = |head: &[&str], items: &[Item]| -> Vec<Arg> {
        let mut a: Vec<Arg> = head.iter().map(|h| Arg::Tok(item_enc(h).into())).collect();
        a.extend(t(items));
        a
    };
    let mut p = Prog::new();
    let q = p.history("q");
    let m17 = || Item::new("m17").key("g2");
    let m18 = || Item::new("m18").key("g2");
    let o = p.invoke(q, 3, "send", with(&["orders"], &[m17(), m18()]));
    p.ok(o, with(&["orders"], &[m17().id(81), m18().id(82)]));
    let m19 = || Item::new("m19").key("g;2").dedup("d=1").delay_ms(5000);
    let o = p.invoke(q, 3, "send", with(&["orders"], &[m19()]));
    p.info(o);
    let o = p.invoke(q, 3, "send", with(&["orders"], &[m19()]));
    p.ok(o, with(&["orders"], &[m19().id("83").outcome(Outcome::Ok)]));
    let o = p.invoke(q, 4, "send", with(&["orders"], &[Item::new("m 20")]));
    p.fail(o);
    let o = p.invoke(q, 5, "recv", with(&["orders", "-"], &[]));
    p.ok(o, with(&["orders", "-"], &[m17().id(81).attempt(1).handle("a91f").lease_ms(30000)]));
    let o = p.invoke(q, 5, "recv", with(&["orders", "-"], &[]));
    p.ok(o, with(&["orders", "-"], &[]));
    let o = p.invoke(q, 5, "ack", with(&["orders", "-"], &[Item::new("m17").handle("a91f")]));
    p.ok_same(o);
    let o = p.invoke(q, 6, "nack", with(&["orders", "-"], &[Item::new("m18").handle("b2").reject()]));
    p.info(o);
    let o = p.invoke(q, 7, "extend", with(&["orders", "-"], &[Item::new("m19").handle("c3").lease_ms(60000)]));
    p.ok_same(o);
    let mut c = with(&["events", "g1"], &[]);
    c.push(Arg::Tok(format!("{}@41", item_enc("k1")).into()));
    c.push(Arg::Tok(format!("{}@7", item_enc("k 2")).into()));
    let o = p.invoke(q, 8, "commit", c);
    p.ok_same(o);
    let mut a = with(&["events", "g1"], &[]);
    a.push("from=start".into());
    let o = p.invoke(q, 8, "sub", a);
    p.ok_same(o);
    let mut a = with(&["events", "g2"], &[]);
    a.push("from=now".into());
    let o = p.invoke(q, 8, "sub", a);
    p.fail(o);
    p.record(q, 9, "ok", "drained", with(&["orders", "-"], &[]));
    p
}

fn hcase(name: &'static str, about: &'static str, p: Prog) -> Case {
    case("history", name, about, p, vec![Run::hist("")])
}

fn history_cases() -> Vec<Case> {
    let mut v = Vec::new();

    let mut p = Prog::new();
    let kv = p.history("kv");
    let o = p.invoke(kv, 3, "write", vec!["k1".into(), enc("a b")]);
    p.ok(o, vec!["k1".into(), ver("a b", 83)]);
    let o = p.invoke(kv, 3, "write", vec!["k2".into(), enc("v")]);
    p.fail(o);
    let o = p.invoke(kv, 4, "write", vec!["k3".into(), enc("v")]);
    p.info(o);
    let o = p.invoke(kv, 4, "append", vec!["l".into(), "7".into()]);
    p.ok_same(o);
    v.push(hcase(
        "write_ok_fail_info",
        "invoke, then ok (with results), fail, info or ok_same (the invocation's arguments)",
        p,
    ));

    let mut p = Prog::new();
    let kv = p.history("kv");
    let o = p.invoke(kv, 20, "read", vec!["k1".into(), "k2".into(), "k3".into()]);
    p.ok(o, vec!["k1".into(), von(Some("")), "k2".into(), von(None), "k3".into(), Arg::Nil]);
    let o = p.invoke(kv, 20, "read", vec!["k4".into()]);
    p.ok(o, vec!["k4".into(), von(Some("x y"))]);
    let o = p.invoke(kv, 20, "read", vec!["k5".into()]);
    p.ok(o, vec!["k5".into(), von(Some("nil"))]);
    v.push(hcase(
        "reads",
        "a read lists key/value pairs; value_or_nil gives nil for an absent key, and the constant NIL is nil",
        p,
    ));

    let mut p = Prog::new();
    let kv = p.history("kv");
    let mut ascii = String::new();
    for c in 0u8..0x80 {
        ascii.push(c as char);
    }
    let vals: Vec<&str> = vec![
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
        "\u{85}",
        "\u{2028}\u{2029}",
        "\u{3000}",
        "\u{fffd}",
        "\u{ffff}",
        "42",
        "-7",
    ];
    for (i, s) in vals.iter().copied().chain(std::iter::once(ascii.as_str())).enumerate() {
        let o = p.invoke(kv, i as i64, "write", vec![Arg::Tok(format!("k{}", i).into()), enc(s)]);
        p.ok_same(o);
    }
    v.push(hcase(
        "enc",
        "enc percent-encodes '%', '@', ',[]:', DEL and every byte up to space; other bytes, all non-ASCII text included, stay; \"\" is % and nil is %6Eil",
        p,
    ));

    let mut p = Prog::new();
    let kv = p.history("kv");
    for (i, n) in [0u64, 1, 83, 9007199254740992, 9007199254740993, i64::MAX as u64].iter().enumerate() {
        let o = p.invoke(kv, 10 + i as i64, "write", vec!["kv".into(), "v".into()]);
        p.ok(o, vec!["kv".into(), ver("v", *n)]);
    }
    let o = p.invoke(kv, 9, "write", vec!["x".into(), "y".into()]);
    p.ok(o, vec![ver("", 5), ver("nil", 6), ver("a@b", 7), ver("12", 12)]);
    v.push(hcase("ver", "ver writes enc(value)@version", p));

    let mut p = Prog::new();
    let la = p.history("la");
    let o = p.invoke(la, 1, "read", vec!["x".into()]);
    p.ok(o, vec!["x".into(), hlist(&[])]);
    let o = p.invoke(la, 1, "read", vec!["x".into()]);
    p.ok(
        o,
        vec![
            "x".into(),
            hlist(&[""]),
            hlist(&["a"]),
            hlist(&["a", "b,c", "[x]", "nil", "", "é", "1 2", "%", ":"]),
            hlist(&["1", "2", "3"]),
        ],
    );
    v.push(hcase("lists", "list writes [e1,e2,...] with each element encoded", p));

    let mut p = Prog::new();
    let la = p.history("la");
    let o = p.invoke(la, 3, "txn", vec![amop("x", "2"), rmop("y"), amop("k:1", "e,2"), rmop("")]);
    p.ok(o, vec![amop("x", "2"), rrmop("y", &[]), amop("k:1", "e,2"), rrmop("", &["a", "b c"])]);
    let o = p.invoke(la, 3, "txn", vec![amop("x", "3")]);
    p.fail(o);
    let o = p.invoke(la, 4, "txn", vec![rmop("x"), amop("y", "1"), rrmop("nil", &["nil"]), amop("", ""), rmop("@")]);
    p.info(o);
    v.push(hcase(
        "txn",
        "list-append micro-ops: a:<k>:<e>, r:<k> and r:<k>:[...], keys and elements encoded",
        p,
    ));

    let mut p = Prog::new();
    let other = p.history("Other.hist_2-x");
    p.record(other, 7, "invoke", "cas", vec!["k".into(), "a".into(), "b".into()]);
    p.record(other, 7, "ok", "cas", vec!["k".into(), "a".into(), "b@9".into()]);
    p.record(other, 8, "info", "read", vec![]);
    p.record(other, 0, "fail", "write", vec!["".into(), "x".into(), "".into()]);
    v.push(hcase("record", "record writes one raw record; an empty argument is written as %", p));

    let mut p = Prog::new();
    let kv = p.history("kv");
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
    v.push(hcase("empty_arguments", "empty arguments are % and an operation may have no arguments", p));

    v.push(hcase(
        "messages",
        "messaging records (docs/design/messaging.md): send, recv, ack, nack, extend, commit, sub and drained, with items m;k=;x=;d=;o=;n=;h=;v=;s=;r= in one fixed order and their values encoded with enc plus ; and = (the tokens are the Rust Item's; tests/history.rs checks that the Messages recorder writes exactly these lines)",
        messages_prog(),
    ));

    let mut p = Prog::new();
    let kv = p.history("kv");
    for (n, ty) in [(0i64, "ok"), (1 << 40, "ok"), (-5, "ok"), (i64::MAX, "info"), (i64::MIN, "fail")] {
        let o = p.invoke(kv, n, "write", vec!["k".into(), "1".into()]);
        match ty {
            "ok" => p.ok_same(o),
            "info" => p.info(o),
            _ => p.fail(o),
        }
    }
    v.push(hcase("process_numbers", "process numbers are decimal int64s", p));

    let mut p = Prog::new();
    let kv = p.history("kv");
    let la = p.history("la");
    let other = p.history("other");
    let a = p.invoke(kv, 30, "write", vec!["i".into(), "1".into()]);
    let b = p.invoke(la, 30, "append", vec!["i".into(), "1".into()]);
    let c = p.invoke(other, 31, "read", vec!["i".into()]);
    p.ok_same(b);
    p.ok(c, vec!["i".into(), "1".into()]);
    p.info(a);
    v.push(hcase(
        "interleaved",
        "operations of several histories complete in any order; each record names its history",
        p,
    ));

    let mut p = Prog::new();
    let kv = p.history("kv");
    for (i, s) in [
        r"\xff",
        r"a\xffb",
        r"\x80",
        r"\xc3",
        r"\xc3(",
        r"\xed\xa0\x80",
        r"\xe2\x82",
        r"é\xff中",
        r"\xff\xfe nil",
    ]
    .iter()
    .enumerate()
    {
        let o = p.invoke(kv, 50 + i as i64, "write", vec!["b".into(), enc(x(s))]);
        p.ok(o, vec!["b".into(), ver(x(s), i as u64), Arg::Von(Some(x(s)))]);
    }
    let o = p.invoke(kv, 60, "write", vec![x(r"\xffraw").into(), "x".into()]);
    p.ok(
        o,
        vec![
            Arg::List(vec![x(r"\xff"), "ok".into(), x(r"\xc3")]),
            amop(x(r"\xff"), x(r"\x80")),
            Arg::ReadResultMop(x(r"\xfe"), vec![x(r"\xff")]),
        ],
    );
    p.record(kv, 62, "ok", "read", vec![x(r"\xe2\x82").into(), "".into()]);
    v.push(hcase(
        "invalid_utf8",
        "invalid UTF-8 bytes in values and tokens are written as they are (JavaScript strings cannot hold them)",
        p,
    ));

    let mut p = Prog::new();
    let kv = p.history("kv");
    let o = p.invoke(kv, 1, "write", vec!["k".into(), enc("v 1")]);
    p.ok_same(o);
    let o = p.invoke(kv, 1, "read", vec!["k".into()]);
    p.ok(o, vec!["k".into(), von(Some("v 1"))]);
    let run = |n: &'static str, o: &'static str| Run::hist(n).hist_out(o);
    v.push(case(
        "history",
        "hist_out",
        "HYDO_HIST_OUT: a file (appended to), a missing file (created), stderr, a path that cannot be opened (standard error), or off",
        p,
        vec![
            run("file", "file"),
            run("append", "append"),
            run("create", "create"),
            run("stderr", "stderr"),
            run("fallback", "fallback"),
            run("off", "off"),
        ],
    ));
    v
}
