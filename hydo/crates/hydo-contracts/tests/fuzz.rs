//! Function fuzzing from contracts (Milestone 275): the reading of Rust sources, the values, the
//! clauses over values, the shrink, the harness, and one run end to end on tests/fuzz-crate.

use crate::tmpdir::TmpDir;
use hydo_contracts::fuzz::check::{hints_of, holds, Env};
use hydo_contracts::fuzz::rust::{
    enclosing, impl_type, mask, pass_of, read_items, signature, FTy, Items, Pass, Receiver,
    Resolver,
};
use hydo_contracts::fuzz::value::{
    decode, encode, int_range, parse, parse_args, show, simpler, Gen, Hints, Rng, Val,
};
use hydo_contracts::fuzz::{harness, Report};
use hydo_contracts::lang::parse_expr;
use std::path::Path;

fn u64t() -> FTy {
    FTy::Int {
        name: "u64".into(),
        signed: false,
        bits: 64,
    }
}

#[test]
fn masking_keeps_offsets_and_hides_comments_and_literals() {
    let src = "fn f() { let s = \"{\"; // }\n let c = '}'; /* { */ let r = r#\"}\"#; }\n";
    let m = mask(src);
    assert_eq!(m.len(), src.len());
    let ms = String::from_utf8(m).unwrap();
    assert_eq!(ms.matches('{').count(), 1, "{ms}");
    assert_eq!(ms.matches('}').count(), 1, "{ms}");
    // a lifetime is not a character literal
    let m2 = String::from_utf8(mask("fn g<'a>(x: &'a str) -> &'a str { x }")).unwrap();
    assert!(m2.contains("&'a str"), "{m2}");
}

#[test]
fn a_signature_gives_the_parameters_the_receiver_and_the_result() {
    let src = "impl Ledger {\n    pub unsafe fn apply<'a>(&mut self, c: &'a mut Conn, ids: &[u64], name: &str, n: Option<u8>) -> Result<u64, Error> {\n        0\n    }\n}\n";
    let m = mask(src);
    let decl = src.find("    pub unsafe").unwrap();
    let s = signature(src, &m, decl).unwrap();
    assert_eq!(s.name, "apply");
    assert!(s.generics.is_empty(), "{:?}", s.generics);
    assert!(s.is_unsafe && !s.is_async);
    assert_eq!(s.receiver, Some(Receiver::RefMut));
    let names: Vec<&str> = s.params.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(names, ["c", "ids", "name", "n"]);
    assert_eq!(pass_of(&s.params[0].1), Pass::RefMut { unsized_ref: false });
    assert_eq!(pass_of(&s.params[1].1), Pass::Ref { unsized_ref: true });
    assert_eq!(pass_of(&s.params[2].1), Pass::Ref { unsized_ref: true });
    assert_eq!(pass_of(&s.params[3].1), Pass::Value);
    assert_eq!(s.ret.unwrap().text(), "Result<u64, Error>");
    let blocks = enclosing(&m, decl);
    assert_eq!(blocks.len(), 1);
    assert_eq!(impl_type(&blocks[0].header).unwrap().0.text(), "Ledger");
    // a generic function names its type parameters
    let g = "fn f<T: Copy>(x: T) -> T { x }";
    let s = signature(g, &mask(g), 0).unwrap();
    assert_eq!(s.generics, ["T: Copy"]);
}

#[test]
fn types_resolve_through_structures_aliases_and_self() {
    let mut items = Items::default();
    read_items(
        "pub struct Ledger { pub balance: u64, last: Option<Score> }\n\
         pub struct U24([u8; 3]);\n\
         type Score = i32;\n\
         struct Node { next: Option<Box<Node>> }\n\
         struct G<T> { t: T }\n",
        &mut items,
    );
    let r = Resolver {
        items: &items,
        self_ty: None,
        module: &[],
        subst: &Default::default(),
    };
    let t = |s: &str| r.resolve(&hydo_contracts::fuzz::rust::parse_ty(s).unwrap());
    let l = t("&mut Ledger");
    assert!(l.generable().is_ok());
    assert_eq!(l.rust(), "Ledger");
    let FTy::Struct { fields, .. } = &l else {
        panic!("{l:?}")
    };
    assert_eq!(fields[1].1.rust(), "Option<i32>");
    let u = t("U24");
    assert!(
        matches!(&u, FTy::Struct { tuple: true, fields, .. } if fields[0].1 == FTy::Array(Box::new(FTy::Int { name: "u8".into(), signed: false, bits: 8 }), 3))
    );
    // a recursive structure and a generic one cannot be generated
    assert!(t("Node").generable().is_err());
    assert!(t("G").generable().is_err());
    // a floating-point type is generated since Milestone 289
    assert_eq!(t("f64"), FTy::Float { bits: 64 });
    assert!(t("f32").generable().is_ok());
    assert_eq!(t("&[u64]"), FTy::List(Box::new(u64t())));
    assert_eq!(t("&str"), FTy::Str);
}

fn ledger_ty() -> FTy {
    FTy::Struct {
        path: "Ledger".into(),
        tuple: false,
        fields: vec![("balance".into(), u64t()), ("last_req".into(), u64t())],
    }
}

#[test]
fn values_go_through_the_wire_and_the_shown_form_unchanged() {
    let t = FTy::Tuple(vec![
        ledger_ty(),
        FTy::List(Box::new(FTy::Str)),
        FTy::Opt(Box::new(FTy::Bool)),
        FTy::Res(Box::new(u64t()), Box::new(FTy::Str)),
    ]);
    let v = Val::Tuple(vec![
        Val::Struct(vec![
            ("balance".into(), Val::Int(7)),
            ("last_req".into(), Val::Int(18446744073709551615)),
        ]),
        Val::List(vec![Val::Str("a b\n\"é".into()), Val::Str(String::new())]),
        Val::Opt(Some(Box::new(Val::Bool(true)))),
        Val::Res(Err(Box::new(Val::Str("no".into())))),
    ]);
    let mut w = String::new();
    encode(&v, &mut w);
    assert_eq!(decode(&t, &mut w.split_ascii_whitespace()).unwrap(), v);
    let shown = show(&v, &t);
    assert_eq!(
        shown,
        "(Ledger { balance: 7, last_req: 18446744073709551615 }, [\"a b\\n\\\"é\", \"\"], Some(true), Err(\"no\"))"
    );
    assert_eq!(parse(&shown, &t).unwrap(), v);
    let params = vec![("l".to_string(), ledger_ty()), ("n".to_string(), u64t())];
    let args = parse_args("n = 3, l = Ledger { balance: 1, last_req: 2 }", &params).unwrap();
    assert_eq!(args[1], Val::Int(3));
    assert!(parse_args("n = 3", &params).is_err());
    assert!(parse("-1", &u64t()).is_err());
}

#[test]
fn generation_is_the_same_from_the_same_seed_and_keeps_the_domains() {
    let mut h = Hints::default();
    h.domains.insert("amount".into(), (1, 10));
    let gen = |seed| {
        let mut rng = Rng::new(seed);
        let mut g = Gen {
            rng: &mut rng,
            hints: &h,
            mode: 2,
            max_len: 8,
            budget: hydo_contracts::fuzz::value::BUDGET,
        };
        (0..200)
            .map(|_| {
                (
                    g.val(&u64t(), "amount"),
                    g.val(&FTy::List(Box::new(FTy::Str)), "v"),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(gen(1), gen(1));
    assert_ne!(gen(1), gen(2));
    for (a, _) in gen(3) {
        let Val::Int(x) = a else { panic!() };
        assert!((1..=10).contains(&x), "{x}");
    }
    assert_eq!(int_range(true, 8), (-128, 127));
    assert_eq!(int_range(false, 128).1, i128::MAX);
}

#[test]
fn a_clause_reads_old_values_after_values_and_the_result() {
    let a = parse_expr("req_id > old(l.last_req) || l.balance == old(l.balance) - amount").unwrap();
    let l = |b, r| {
        Val::Struct(vec![
            ("balance".into(), Val::Int(b)),
            ("last_req".into(), Val::Int(r)),
        ])
    };
    let entry = vec![
        ("l".into(), l(10, 5)),
        ("req_id".into(), Val::Int(6)),
        ("amount".into(), Val::Int(3)),
    ];
    let mut after = entry.clone();
    after[0].1 = l(7, 6);
    let env = Env {
        entry: &entry,
        after: &after,
        result: Some(&Val::Int(7)),
    };
    assert_eq!(holds(&a, &env), Ok(true));
    let mut e2 = entry.clone();
    e2[1].1 = Val::Int(5);
    let after_bad = vec![("l".into(), l(9, 6)), e2[1].clone(), e2[2].clone()];
    let env = Env {
        entry: &e2,
        after: &after_bad,
        result: None,
    };
    assert_eq!(holds(&a, &env), Ok(false));
    // an element past the length and the other variant leave the clause undecided
    let v = vec![("v".into(), Val::List(vec![Val::Int(1)]))];
    let env = Env {
        entry: &v,
        after: &v,
        result: Some(&Val::Opt(None)),
    };
    assert!(holds(&parse_expr("v[1] == 1").unwrap(), &env).is_err());
    assert!(holds(&parse_expr("some(result) == 1").unwrap(), &env).is_err());
    assert_eq!(
        holds(&parse_expr("is_none(result) && len(v) == 1").unwrap(), &env),
        Ok(true)
    );
    // integers are exact: no wrapping at 64 bits
    let big = vec![("x".into(), Val::Int(u64::MAX as i128))];
    let env = Env {
        entry: &big,
        after: &big,
        result: None,
    };
    assert_eq!(holds(&parse_expr("x + 1 > x").unwrap(), &env), Ok(true));
}

#[test]
fn clauses_give_the_generator_constants_and_related_paths() {
    let mut h = Hints::default();
    hints_of(&parse_expr("amount > 0 && len(v) <= 4").unwrap(), &mut h);
    hints_of(&parse_expr("req_id > old(l.last_req)").unwrap(), &mut h);
    assert_eq!(h.consts["amount"], [0]);
    assert_eq!(h.consts["v#len"], [4]);
    assert_eq!(h.pairs, [("req_id".to_string(), "l.last_req".to_string())]);
}

#[test]
fn a_shrink_moves_integers_towards_zero_and_lists_towards_empty() {
    let h = Hints::default();
    let c = simpler(&Val::Int(1000), &u64t(), "x", &h);
    assert_eq!(c[0], Val::Int(0));
    assert!(c.contains(&Val::Int(500)) && c.contains(&Val::Int(999)));
    let mut d = Hints::default();
    d.domains.insert("x".into(), (5, 9));
    assert_eq!(simpler(&Val::Int(9), &u64t(), "x", &d)[0], Val::Int(5));
    assert!(simpler(&Val::Int(5), &u64t(), "x", &d).is_empty());
    let l = Val::List(vec![Val::Int(3), Val::Int(4)]);
    let c = simpler(&l, &FTy::List(Box::new(u64t())), "v", &h);
    assert_eq!(c[0], Val::List(vec![]));
}

#[test]
fn the_harness_is_one_line_that_calls_the_function() {
    let s = harness::Spec {
        module: "__hydo_fuzz_3".into(),
        call: "<Ledger>::apply".into(),
        receiver: Some(Receiver::RefMut),
        params: vec![
            harness::Param {
                name: "self".into(),
                pass: Pass::RefMut { unsized_ref: false },
                ty: ledger_ty(),
                inner: None,
            },
            harness::Param {
                name: "ids".into(),
                pass: Pass::Ref { unsized_ref: true },
                ty: FTy::List(Box::new(u64t())),
                inner: None,
            },
        ],
        ret: FTy::Opt(Box::new(u64t())),
        ret_ref: false,
        is_unsafe: false,
        forbidden: vec![],
    };
    let m = harness::module(&s);
    assert!(!m.contains('\n'));
    assert!(m.contains("mod __hydo_fuzz_3 {"), "{m}");
    assert!(m.contains("<Ledger>::apply(&mut a0, &a1[..])"), "{m}");
    assert!(m.contains("#[cfg(test)]"));
}

/// The fixture's report, built and fuzzed once for the tests below.
fn fuzz_fixture(dir: &Path, extra: &[&str]) -> (i32, Report) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-crate");
    let out = dir.join("out");
    let mut args: Vec<String> = vec![
        fixture.to_string_lossy().into(),
        "--out".into(),
        out.to_string_lossy().into(),
        "--cases".into(),
        "300".into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let code = hydo_contracts::fuzz::main(&args);
    let text = std::fs::read_to_string(out.join("fuzz.json")).unwrap_or_default();
    (code, serde_json::from_str(&text).unwrap_or_default())
}

#[test]
fn fuzzing_the_fixture_finds_each_planted_bug_with_a_small_input() {
    let t = TmpDir::new("hydo-contracts-fuzz-e2e");
    let (code, rep) = fuzz_fixture(t.path(), &[]);
    assert_eq!(code, 1, "{rep:?}");
    let f = |n: &str| {
        rep.functions
            .iter()
            .find(|f| f.function.ends_with(n))
            .unwrap_or_else(|| panic!("{n}: {rep:?}"))
    };
    let debit = f("::debit");
    assert_eq!(debit.broken.len(), 1, "{debit:?}");
    assert_eq!(debit.broken[0].kind, "ensures");
    assert_eq!(debit.broken[0].line, 13);
    assert_eq!(
        debit.broken[0].input,
        "a = Account { balance: 1, last_req: 1 }, seen = 0, req_id = 1, amount = 1"
    );
    assert!(debit.broken[0].replayed);
    let avg = f("::avg");
    assert_eq!(avg.broken.len(), 1, "{avg:?}");
    assert_eq!(avg.broken[0].kind, "panic");
    assert!(avg.broken[0].text.starts_with("src/lib.rs:"), "{avg:?}");
    assert!(avg.broken[0].after.contains("overflow"), "{avg:?}");
    let fw = f("::first_word");
    assert_eq!(fw.broken[0].input, "s = \"aaaaaaaaa\"");
    let pay = f("::Account::can_pay");
    assert_eq!(
        pay.broken[0].input,
        "self = Account { balance: 0, last_req: 0 }, amount = 0"
    );
    let pos = f("::position_of");
    assert!(pos.broken.is_empty(), "{pos:?}");
    assert_eq!(pos.held.len(), 2);
    // the same seed gives the same report
    let t2 = TmpDir::new("hydo-contracts-fuzz-e2e-again");
    assert_eq!(fuzz_fixture(t2.path(), &[]).1, rep);
    // an input replays alone
    let (code, _) = fuzz_fixture(
        t2.path(),
        &[
            "--fn",
            "debit",
            "--input",
            "a = Account { balance: 1, last_req: 1 }, seen = 0, req_id = 1, amount = 1",
        ],
    );
    assert_eq!(code, 1);
    let (code, _) = fuzz_fixture(
        t2.path(),
        &[
            "--fn",
            "debit",
            "--input",
            "a = Account { balance: 1, last_req: 0 }, seen = 0, req_id = 1, amount = 1",
        ],
    );
    assert_eq!(code, 0);
}

// ---------------------------------------------------------------- Milestone 282

#[test]
fn input_lines_take_lengths_characters_examples_and_elements() {
    use hydo_contracts::lang::scan;
    let src = "\
// hydo: input s len 0..200 chars ascii
// hydo: input v len 0..=1000
// hydo: input v[] in 1..=255
// hydo: input len(a) in 64..=64
// hydo: input t like \"1.2.3\", \"0.0.0-alpha\"
// hydo: input u len 3 chars \"a-c.\"
// hydo: with T = i64
fn f<T>(s: &str, v: &[u8], a: &str, t: &str, u: &str, x: T) {}
";
    let (cs, errs) = scan(src);
    assert!(errs.is_empty(), "{errs:?}");
    let i = &cs[0].inputs;
    assert_eq!(i[0].name, "s");
    assert_eq!(i[0].len, Some((0, 199)));
    assert_eq!(i[0].chars.as_ref().map(Vec::len), Some(95));
    assert_eq!(i[1].len, Some((0, 1000)));
    assert_eq!((i[2].name.as_str(), i[2].range), ("v[]", Some((1, 255))));
    assert_eq!((i[3].name.as_str(), i[3].len), ("a", Some((64, 64))));
    assert_eq!(i[4].like, ["1.2.3", "0.0.0-alpha"]);
    assert_eq!(i[5].len, Some((3, 3)));
    assert_eq!(i[5].chars, Some(vec!['.', 'a', 'b', 'c']));
    assert_eq!(cs[0].with, [("T".to_string(), "i64".to_string())]);
    for bad in [
        "// hydo: input s chars greek\nfn f(s: &str) {}\n",
        "// hydo: input s len\nfn f(s: &str) {}\n",
        "// hydo: input s like 1.2.3\nfn f(s: &str) {}\n",
        "// hydo: with T\nfn f<T>(x: T) {}\n",
    ] {
        let (_, errs) = scan(bad);
        assert_eq!(errs.len(), 1, "{bad}");
    }
}

#[test]
fn lengths_characters_and_examples_reach_long_and_valid_values() {
    let mut h = Hints::default();
    h.domains.insert("v#len".into(), (0, 1000));
    h.domains.insert("s#len".into(), (5, 5));
    h.chars.insert("s".into(), vec!['x', 'y']);
    h.like.insert("t".into(), vec!["1.2.3".into()]);
    let mut rng = Rng::new(7);
    let (mut longest, mut examples) = (0, 0);
    for _ in 0..500 {
        let mut g = Gen {
            rng: &mut rng,
            hints: &h,
            mode: 2,
            max_len: 4,
            budget: hydo_contracts::fuzz::value::BUDGET,
        };
        let Val::List(v) = g.val(&FTy::List(Box::new(u64t())), "v") else {
            panic!()
        };
        assert!(v.len() <= 1000);
        longest = longest.max(v.len());
        let Val::Str(s) = g.val(&FTy::Str, "s") else {
            panic!()
        };
        assert!(
            s.len() == 5 && s.chars().all(|c| c == 'x' || c == 'y'),
            "{s}"
        );
        let Val::Str(t) = g.val(&FTy::Str, "t") else {
            panic!()
        };
        examples += (t == "1.2.3") as u32;
        // a size without an input line stays small
        let usize_t = FTy::Int {
            name: "usize".into(),
            signed: false,
            bits: 64,
        };
        let Val::Int(n) = g.val(&usize_t, "n") else {
            panic!()
        };
        assert!(n <= hydo_contracts::fuzz::value::SIZE_BOUND, "{n}");
    }
    assert!(longest > 900, "{longest}");
    assert!(examples > 100 && examples < 400, "{examples}");
    // without a len part, the later inputs reach the edge sizes past 64
    let mut rng = Rng::new(3);
    let mut past = 0;
    for _ in 0..400 {
        let mut g = Gen {
            rng: &mut rng,
            hints: &Hints::default(),
            mode: 2,
            max_len: 256,
            budget: hydo_contracts::fuzz::value::BUDGET,
        };
        if let Val::List(v) = g.val(&FTy::List(Box::new(u64t())), "v") {
            past += (v.len() > 64) as u32;
        }
    }
    assert!(past > 20, "{past}");
}

#[test]
fn a_clause_splits_at_its_top_level_or() {
    use hydo_contracts::fuzz::or_parts;
    assert_eq!(
        or_parts("is_none(result) || (a || b) || v[some(result)] == \"||\""),
        ["is_none(result)", "(a || b)", "v[some(result)] == \"||\""]
    );
    assert_eq!(or_parts("x > 0"), ["x > 0"]);
}

#[test]
fn the_harness_builds_without_std_and_names_no_hidden_field() {
    let s = harness::Spec {
        module: "__hydo_fuzz_0".into(),
        call: "super::version".into(),
        receiver: None,
        params: vec![],
        ret: FTy::Struct {
            path: "Version".into(),
            tuple: false,
            fields: vec![
                ("major".into(), u64t()),
                ("pre".into(), FTy::Hidden("private".into())),
            ],
        },
        ret_ref: false,
        is_unsafe: false,
        forbidden: vec![],
    };
    let m = harness::module(&s);
    assert!(m.contains("extern crate std;"), "{m}");
    assert!(!m.contains(" ::std::") && !m.contains("(::std::"), "{m}");
    assert!(!m.contains(".pre"), "{m}");
    // a value with a hidden field cannot be generated, and the reason says why
    let why = FTy::Struct {
        path: "Version".into(),
        tuple: false,
        fields: vec![(
            "pre".into(),
            FTy::Hidden("the field `head` is private".into()),
        )],
    }
    .generable()
    .unwrap_err();
    assert!(why.contains("the field `head` is private"), "{why}");
}

#[test]
fn a_structure_in_another_module_hides_its_private_fields() {
    let mut items = Items::default();
    hydo_contracts::fuzz::rust::read_items_in(
        "pub struct Ident { head: u8, pub len: u8 }\nconst N: usize = 3;\npub struct A { pub a: [u8; N] }\n",
        &["ident".to_string()],
        &mut items,
    );
    let ty = hydo_contracts::fuzz::rust::parse_ty("Ident").unwrap();
    let from_root = Resolver {
        items: &items,
        self_ty: None,
        module: &[],
        subst: &Default::default(),
    }
    .resolve(&ty);
    let FTy::Struct { fields, .. } = &from_root else {
        panic!("{from_root:?}")
    };
    assert!(matches!(fields[0].1, FTy::Hidden(_)), "{fields:?}");
    assert!(matches!(fields[1].1, FTy::Int { .. }), "{fields:?}");
    let inside = vec!["ident".to_string(), "deeper".to_string()];
    let r = Resolver {
        items: &items,
        self_ty: None,
        module: &inside,
        subst: &Default::default(),
    };
    assert!(r.resolve(&ty).generable().is_ok());
    // an array's length through a constant
    let a = r.resolve(&hydo_contracts::fuzz::rust::parse_ty("A").unwrap());
    assert!(a.generable().is_ok(), "{a:?}");
    let module = |p: &str| hydo_contracts::fuzz::rust::file_module(Path::new(p));
    assert_eq!(module("src/memmem/mod.rs"), ["memmem"]);
    assert_eq!(module("src/a/b.rs"), ["a", "b"]);
    assert!(module("src/lib.rs").is_empty());
}

/// The second fixture's report: a `#![no_std]` crate with what stopped Milestone 280.
fn fuzz_nostd(dir: &Path, extra: &[&str]) -> (i32, Report) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-nostd");
    let out = dir.join("out");
    let mut args: Vec<String> = vec![
        fixture.to_string_lossy().into(),
        "--out".into(),
        out.to_string_lossy().into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let code = hydo_contracts::fuzz::main(&args);
    let text = std::fs::read_to_string(out.join("fuzz.json")).unwrap_or_default();
    (code, serde_json::from_str(&text).unwrap_or_default())
}

#[test]
fn a_no_std_crate_fuzzes_function_by_function_with_verdicts() {
    let t = TmpDir::new("hydo-contracts-fuzz-nostd");
    let (code, rep) = fuzz_nostd(t.path(), &[]);
    assert_eq!(code, 1, "{rep:?}");
    let f = |n: &str| {
        rep.functions
            .iter()
            .find(|f| f.function.ends_with(n))
            .unwrap_or_else(|| panic!("{n}: {rep:?}"))
    };
    let skipped = |n: &str| {
        rep.skipped
            .iter()
            .find(|s| s.contains(n))
            .unwrap_or_else(|| panic!("{n}: {:?}", rep.skipped))
            .clone()
    };
    // a result with a hidden field is read but for it; a parameter with one is refused
    assert_eq!(f("::version").verdict, "held");
    assert!(skipped("::major_of").contains("is private to the module `ident`"));
    // one function whose harness does not build stops no other
    assert!(skipped("::b::get").contains("its harness did not build: error[E0560]"));
    // too few inputs, and a clause that is never checked
    let name = f("::is_name");
    assert_eq!((name.verdict.as_str(), name.calls), ("too few inputs", 0));
    assert!(
        name.advice[0].contains("input s like \"hydo\""),
        "{:?}",
        name.advice
    );
    let d = f("::digit_of");
    assert_eq!(d.verdict, "vacuous");
    assert_eq!(d.held[0].reached, Some(0));
    assert!(d.advice[0].contains("input s like"), "{:?}", d.advice);
    let dl = f("::digit_of_like");
    assert_eq!(dl.verdict, "held");
    assert!(dl.held[0].reached.unwrap() > 100, "{dl:?}");
    // a bug past the 100th element, found from the contract alone
    let late = f("::find_late");
    assert_eq!(late.verdict, "broken", "{late:?}");
    assert!(late.broken[0].replayed);
    // a size without an input line returns at once
    assert_eq!(f("::zeros").verdict, "held");
    assert!(f("::zeros").runs <= 2, "{:?}", f("::zeros"));
    // a generic function with `with T = u8`
    let tw = f("::twice");
    assert_eq!(tw.broken[0].kind, "panic");
    assert_eq!(tw.broken[0].input, "x = 128");
    // a vacuous run exits 3; --min-reach 0 turns the check off
    let (code, _) = fuzz_nostd(t.path(), &["--fn", "digit_of"]);
    assert_eq!(code, 3);
    let (code, rep) = fuzz_nostd(t.path(), &["--fn", "digit_of", "--min-reach", "0"]);
    assert_eq!(code, 0, "{rep:?}");
}

// ---------------------------------------------------------------- Milestone 289

#[test]
fn floating_point_values_go_through_the_wire_and_compare_exactly() {
    let f64t = FTy::Float { bits: 64 };
    let t = FTy::Tuple(vec![f64t.clone(), FTy::Float { bits: 32 }]);
    let v = Val::Tuple(vec![
        Val::Float(0.1f64.to_bits()),
        Val::Float((0.1f32 as f64).to_bits()),
    ]);
    let mut w = String::new();
    encode(&v, &mut w);
    assert_eq!(decode(&t, &mut w.split_ascii_whitespace()).unwrap(), v);
    assert_eq!(show(&v, &t), "(0.1, 0.1)");
    assert_eq!(parse("(0.1, 0.1)", &t).unwrap(), v);
    assert_eq!(parse("NaN", &f64t).unwrap(), Val::Float(f64::NAN.to_bits()));
    assert_eq!(
        parse("-inf", &f64t).unwrap(),
        Val::Float(f64::NEG_INFINITY.to_bits())
    );
    let judge = |x: f64, c: &str| {
        let e: Vec<(String, Val)> = vec![("x".into(), Val::Float(x.to_bits()))];
        let r = Val::Float(x.to_bits());
        let env = Env {
            entry: &e,
            after: &e,
            result: Some(&r),
        };
        holds(&parse_expr(c).unwrap(), &env)
    };
    assert_eq!(
        judge(1.0, "result <= 1 && result >= 1 && result == 1"),
        Ok(true)
    );
    assert_eq!(
        judge(1.0, "result < 1 || result > 1 || result != 1"),
        Ok(false)
    );
    assert_eq!(judge(1.0, "result * 2 == 2.0 && -result < 0"), Ok(true));
    // an integer compares exactly: 2^53 + 1 is not the double next to it
    let big = 9007199254740992.0;
    assert_eq!(judge(big, "result < 9007199254740993"), Ok(true));
    assert_eq!(judge(big, "result == 9007199254740992"), Ok(true));
    assert_eq!(
        judge(0.5, "result > 0 && result < 1 && result != 0"),
        Ok(true)
    );
    // a NaN compares unequal to everything, itself included
    assert_eq!(
        judge(f64::NAN, "x != x && !(result <= 1) && !(result > 1)"),
        Ok(true)
    );
    // a float where an integer must be is undecided
    assert!(judge(1.0, "len(result) == 1").is_err());
}

#[test]
fn the_harness_allows_no_lint_that_the_crate_forbids() {
    use hydo_contracts::fuzz::forbidden_lints;
    let src = "#![forbid(unsafe_code)]\n#![cfg_attr(test, forbid(\n    missing_docs,\n))]\n\
               // forbid(x) in a comment\nfn f() { let s = \"forbid(y)\"; }\n";
    assert_eq!(forbidden_lints(src), ["unsafe_code", "missing_docs"]);
    let mut s = harness::Spec {
        module: "__hydo_fuzz_0".into(),
        call: "super::f".into(),
        receiver: None,
        params: vec![],
        ret: FTy::Float { bits: 64 },
        ret_ref: false,
        is_unsafe: false,
        forbidden: vec!["warnings".into()],
    };
    let m = harness::module(&s);
    assert!(m.contains("#[allow(unused, clippy::all"), "{m}");
    assert!(
        !m.contains("unsafe_code") && !m.contains("warnings,"),
        "{m}"
    );
    assert!(m.contains("ef(<f64 as"), "{m}");
    s.is_unsafe = true;
    s.forbidden.clear();
    assert!(harness::module(&s).contains("unsafe_code"));
    let k = harness::keep(&s);
    assert!(
        k.contains("const _: () = { let _ = super::f; };") && !k.contains('\n'),
        "{k}"
    );
}

#[test]
fn the_copy_keeps_the_package_lints_and_the_workspace_ones() {
    use hydo_contracts::fuzz::cargo::lints;
    let own = "[package]\nname = \"x\"\n\n[lints.rust.unexpected_cfgs]\nlevel = \"warn\"\n\
               check-cfg = [\n    \"cfg(x_simd)\",\n]\n\n[profile.bench]\nlto = true\n";
    let l = lints(own, None);
    assert!(
        l.contains("[lints.rust.unexpected_cfgs]\nlevel = \"warn\""),
        "{l}"
    );
    assert!(
        l.contains("\"cfg(x_simd)\",\n]") && !l.contains("profile"),
        "{l}"
    );
    let member = "[package]\nname = \"m\"\n\n[lints]\nworkspace = true\n";
    let ws = "[workspace]\nmembers = [\"m\"]\n\n[workspace.lints.rust]\nunsafe_code = \"deny\"\n\n\
              [workspace.lints.clippy]\nall = \"warn\"\n";
    let l = lints(member, Some(ws));
    assert!(l.contains("[lints.rust]\nunsafe_code = \"deny\""), "{l}");
    assert!(
        l.contains("[lints.clippy]\nall = \"warn\"") && !l.contains("workspace"),
        "{l}"
    );
    assert_eq!(lints("[package]\nname = \"y\"\n", None), "");
}

#[test]
fn byte_lists_take_byte_string_examples_and_show_as_byte_strings() {
    use hydo_contracts::lang::scan;
    let src = "// hydo: input buf like b\"GET / HTTP/1.1\\r\\n\", \"A:b\", b\"\\x00\\xff\"\n\
               fn f(buf: &[u8]) {}\n";
    let (cs, errs) = scan(src);
    assert!(errs.is_empty(), "{errs:?}");
    let i = &cs[0].inputs[0];
    assert_eq!(i.like, ["A:b"]);
    assert_eq!(
        i.like_bytes,
        [b"GET / HTTP/1.1\r\n".to_vec(), vec![0u8, 255]]
    );
    let bytes = FTy::List(Box::new(FTy::Int {
        name: "u8".into(),
        signed: false,
        bits: 8,
    }));
    let v = Val::List(
        [0u8, b'G', b'\r', b'"', 255]
            .iter()
            .map(|&b| Val::Int(b as i128))
            .collect(),
    );
    assert_eq!(show(&v, &bytes), "b\"\\x00G\\r\\\"\\xff\"");
    assert_eq!(parse(&show(&v, &bytes), &bytes).unwrap(), v);
    assert_eq!(
        parse("[0, 71]", &bytes).unwrap(),
        Val::List(vec![Val::Int(0), Val::Int(71)])
    );
    // the generator changes the examples, as it changes strings
    let mut h = Hints::default();
    h.like_bytes.insert("buf".into(), i.like_bytes.clone());
    let mut rng = Rng::new(3);
    let mut seen_example = false;
    for k in 0..200u8 {
        let mut g = Gen {
            rng: &mut rng,
            hints: &h,
            mode: k.min(2),
            max_len: 64,
            budget: 1 << 16,
        };
        if let Val::List(l) = g.val(&bytes, "buf") {
            let b: Vec<u8> = l
                .iter()
                .map(|v| match v {
                    Val::Int(x) => *x as u8,
                    _ => 0,
                })
                .collect();
            seen_example |= b == b"GET / HTTP/1.1\r\n";
        }
    }
    assert!(seen_example);
}

/// The third fixture's report: what stopped the second evaluation (Milestone 289).
fn fuzz_m289(dir: &Path, extra: &[&str]) -> (i32, Report) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-m289");
    let out = dir.join("out");
    let mut args: Vec<String> = vec![
        fixture.to_string_lossy().into(),
        "--out".into(),
        out.to_string_lossy().into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let code = hydo_contracts::fuzz::main(&args);
    let text = std::fs::read_to_string(out.join("fuzz.json")).unwrap_or_default();
    (code, serde_json::from_str(&text).unwrap_or_default())
}

#[test]
fn a_crate_that_forbids_unsafe_code_fuzzes_with_floats_bytes_and_honest_verdicts() {
    let t = TmpDir::new("hydo-contracts-fuzz-m289");
    // the crate forbids `unsafe_code`, and denies warnings with a `cfg` its lints list
    let (code, rep) = fuzz_m289(t.path(), &["--show-inputs", "3"]);
    assert_eq!(code, 1, "{rep:?}");
    let f = |n: &str| {
        rep.functions
            .iter()
            .find(|f| f.function.ends_with(n))
            .unwrap_or_else(|| panic!("{n}: {rep:?}"))
    };
    // an f64 result is read and judged on every call
    let ratio = f("::ratio");
    assert_eq!(ratio.verdict, "held");
    assert_eq!((ratio.held[0].cases, ratio.held[0].undecided), (1000, 0));
    assert_eq!(ratio.samples.len(), 3);
    let score = f("::score");
    assert_eq!(score.broken[0].input, "x = 200");
    assert_eq!(score.broken[0].after, "result = 1.0");
    assert_eq!(f("::clamp_unit").verdict, "held");
    // a clause that hydo cannot decide is never `held`
    let kind = f("::kind");
    assert_eq!(kind.verdict, "not judged", "{kind:?}");
    assert!(kind.held[0].not_judged && kind.held[0].cases == 0);
    assert!(kind.advice[0].contains("could not be judged on 1000 of 1000 calls"));
    // the only inputs that reach a clause panic: the verdict is the panic, not `vacuous`
    let abs = f("::abs32");
    assert_eq!(abs.verdict, "broken");
    assert_eq!(abs.broken[0].kind, "panic");
    assert!(
        !abs.held[0].vacuous && abs.held[0].failed_calls > 0,
        "{abs:?}"
    );
    assert!(abs.advice.is_empty(), "{:?}", abs.advice);
    // bytes like the examples reach the parser's success
    let chunk = f("::chunk_len");
    assert_eq!(chunk.verdict, "held");
    assert!(chunk.held[0].reached.unwrap() > 100, "{chunk:?}");
    assert!(rep.skipped.iter().any(|s| s.contains("::Shape::area")));
    // a clause never decided exits 3
    let (code, _) = fuzz_m289(t.path(), &["--fn", "kind"]);
    assert_eq!(code, 3);
    // a run that skips a named contracted function exits 3, unless --allow-skipped
    let (code, rep) = fuzz_m289(t.path(), &["--fn", "ratio", "--fn", "area"]);
    assert_eq!(code, 3, "{rep:?}");
    let (code, _) = fuzz_m289(
        t.path(),
        &["--fn", "ratio", "--fn", "area", "--allow-skipped"],
    );
    assert_eq!(code, 0);
    // one function alone: the others stay in use, so `deny(warnings)` finds no dead code
    let (code, _) = fuzz_m289(
        t.path(),
        &["--fn", "chunk_len", "--input", "buf = b\"4\\r\\n\""],
    );
    assert_eq!(code, 0);
}

// ---------------------------------------------------------------- Milestone 307

#[test]
fn enumerations_resolve_go_through_the_wire_and_match_patterns() {
    use hydo_contracts::fuzz::check::matches_pat;
    use hydo_contracts::lang::Pat;
    let mut items = Items::default();
    read_items(
        "pub enum Status<T> { Complete(T), Partial }\n\
         #[derive(Debug)]\n\
         enum Op { Neg = 3, Add(i32), Clamp { lo: i32, #[allow(x)] hi: i32 } }\n\
         pub type Result<T> = result::Result<Status<T>, u8>;\n",
        &mut items,
    );
    let r = Resolver {
        items: &items,
        self_ty: None,
        module: &[],
        subst: &Default::default(),
    };
    let t = |s: &str| r.resolve(&hydo_contracts::fuzz::rust::parse_ty(s).unwrap());
    let st = t("Result<Status<(usize, u64)>, u8>");
    let v = Val::Res(Ok(Box::new(Val::Enum(
        "Complete".into(),
        vec![("0".into(), Val::Tuple(vec![Val::Int(2), Val::Int(0)]))],
    ))));
    let mut w = String::new();
    encode(&v, &mut w);
    assert_eq!(w, "O VComplete 2 0 ");
    assert_eq!(decode(&st, &mut w.split_whitespace()).unwrap(), v);
    assert_eq!(show(&v, &st), "Ok(Complete((2, 0)))");
    assert_eq!(parse("Ok(Status::Complete((2, 0)))", &st).unwrap(), v);
    let op = t("Op");
    assert!(op.generable().is_ok());
    let c = Val::Enum(
        "Clamp".into(),
        vec![("lo".into(), Val::Int(0)), ("hi".into(), Val::Int(-1))],
    );
    assert_eq!(show(&c, &op), "Clamp { lo: 0, hi: -1 }");
    assert_eq!(parse("Clamp { lo: 0, hi: -1 }", &op).unwrap(), c);
    assert_eq!(parse("Neg", &op).unwrap(), Val::Enum("Neg".into(), vec![]));
    // patterns
    let pat = |s: &str| {
        let Ok(hydo_contracts::lang::Ast::Matches(_, p)) = parse_expr(&format!("matches(x, {s})"))
        else {
            panic!("{s}")
        };
        p
    };
    assert_eq!(matches_pat(&v, &pat("Ok(Complete(_))")), Ok(true));
    assert_eq!(matches_pat(&v, &pat("Ok(Complete((_, 0)))")), Ok(true));
    assert_eq!(matches_pat(&v, &pat("Ok(Complete((_, 1)))")), Ok(false));
    assert_eq!(matches_pat(&v, &pat("Ok(Partial)")), Ok(false));
    assert_eq!(matches_pat(&v, &pat("Err(_)")), Ok(false));
    assert_eq!(matches_pat(&c, &pat("Op::Clamp")), Ok(true));
    assert!(matches_pat(&Val::Opaque, &pat("Ok(_)")).is_err());
    assert_eq!(matches_pat(&Val::Opaque, &Pat::Any), Ok(true));
    // a variant's field reads like a field, and a variant is not compared with `==`
    let env_v = vec![("x".to_string(), c.clone())];
    let env = Env {
        entry: &env_v,
        after: &env_v,
        result: None,
    };
    assert_eq!(
        holds(&parse_expr("x.Clamp.hi == -1").unwrap(), &env),
        Ok(true)
    );
    assert!(holds(&parse_expr("x.Add.0 == 1").unwrap(), &env)
        .unwrap_err()
        .contains("not `Add`"));
    assert!(holds(&parse_expr("x == 1").unwrap(), &env).is_err());
    // the crate's own alias with a type parameter, over the standard name
    let a = t("Result<usize>");
    assert!(
        matches!(&a, FTy::Res(ok, _) if matches!(**ok, FTy::Enum { .. })),
        "{a:?}"
    );
    assert!(matches!(t("io::Result<usize>"), FTy::Res(ok, _) if matches!(*ok, FTy::Int { .. })));
    // the standard library's Ordering
    let o = t("std::cmp::Ordering");
    assert_eq!(o.rust(), "self::std::cmp::Ordering");
    assert!(matches!(t("atomic::Ordering"), FTy::Opaque(_)));
    // generation keeps to the variants, and a shrink tries an earlier variant without fields
    let mut rng = Rng::new(3);
    let hints = Hints::default();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..50 {
        let mut g = Gen {
            rng: &mut rng,
            hints: &hints,
            mode: 2,
            max_len: 4,
            budget: 1 << 16,
        };
        if let Val::Enum(n, _) = g.val(&op, "op") {
            seen.insert(n);
        }
    }
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(
        simpler(&c, &op, "op", &hints)[0],
        Val::Enum("Neg".into(), vec![])
    );
}

#[test]
fn the_harness_builds_enumerations_constructed_values_and_references_to_references() {
    use hydo_contracts::fuzz::rust::{Ctor, VKind, Variant};
    let st = FTy::Enum {
        path: "Status".into(),
        args: vec![u64t()],
        variants: vec![
            Variant {
                name: "Complete".into(),
                kind: VKind::Tuple,
                fields: vec![("0".into(), u64t())],
            },
            Variant {
                name: "Partial".into(),
                kind: VKind::Unit,
                fields: vec![],
            },
        ],
    };
    let blob = FTy::Built {
        ty: "Blob".into(),
        build: Some(Ctor {
            path: "Blob::from_vec".into(),
            args: vec![(
                "v".into(),
                FTy::List(Box::new(u64t())),
                Pass::Ref { unsized_ref: true },
            )],
        }),
        read: Some((Box::new(FTy::List(Box::new(u64t()))), "to_vec".into())),
    };
    let s = harness::Spec {
        module: "__hydo_fuzz_0".into(),
        call: "<&[u8] as Take>::take".into(),
        receiver: Some(Receiver::RefMut),
        params: vec![
            harness::Param {
                name: "self".into(),
                pass: Pass::RefMut { unsized_ref: false },
                ty: FTy::List(Box::new(u64t())),
                inner: Some(Pass::Ref { unsized_ref: true }),
            },
            harness::Param {
                name: "s".into(),
                pass: Pass::Value,
                ty: st.clone(),
                inner: None,
            },
            harness::Param {
                name: "b".into(),
                pass: Pass::RefMut { unsized_ref: false },
                ty: blob,
                inner: None,
            },
        ],
        ret: st,
        ret_ref: false,
        is_unsafe: false,
        forbidden: vec![],
    };
    let m = harness::module(&s);
    assert!(!m.contains('\n'));
    // a constructor's panic is the call's: the values are built under catch_unwind
    assert!(
        m.contains("let (mut a0, mut a1, mut a2,): (self::std::vec::Vec<u64>, Status<u64>, Blob,)"),
        "{m}"
    );
    assert!(
        m.contains("\"VComplete\" => Status::Complete(r.int())"),
        "{m}"
    );
    assert!(
        m.contains(
            "Blob::from_vec(&(*self::std::boxed::Box::leak(self::std::boxed::Box::new(c.0)))[..])"
        ),
        "{m}"
    );
    assert!(m.contains("let mut b0 = &a0[..];"), "{m}");
    assert!(
        m.contains("<&[u8] as Take>::take(&mut b0, a1, &mut a2)"),
        "{m}"
    );
    assert!(
        m.contains("Status::Partial => { o.push_str(\"VPartial \");"),
        "{m}"
    );
    assert!(m.contains("(&a2).to_vec()"), "{m}");
}

/// The fourth fixture's report: the parts of a Rust API refused before (Milestone 307).
fn fuzz_m307(dir: &Path, extra: &[&str]) -> (i32, Report) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-m307");
    let out = dir.join("out");
    let mut args: Vec<String> = vec![
        fixture.to_string_lossy().into(),
        "--out".into(),
        out.to_string_lossy().into(),
        "--cases".into(),
        "500".into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let code = hydo_contracts::fuzz::main(&args);
    let text = std::fs::read_to_string(out.join("fuzz.json")).unwrap_or_default();
    (code, serde_json::from_str(&text).unwrap_or_default())
}

#[test]
fn enumerations_trait_methods_built_values_and_sorted_inputs_are_fuzzed() {
    let t = TmpDir::new("hydo-contracts-fuzz-m307");
    let (code, rep) = fuzz_m307(t.path(), &[]);
    assert_eq!(code, 1, "{rep:?}");
    let f = |n: &str| {
        rep.functions
            .iter()
            .find(|f| f.function.ends_with(n))
            .unwrap_or_else(|| panic!("{n}: {rep:?}"))
    };
    // an enumeration result: the RFC's rule on the parser itself
    let chunk = f("::chunk_size");
    assert_eq!(chunk.broken[0].input, "buf = b\"\\r\\n\"");
    assert_eq!(chunk.broken[0].after, "result = Ok(Complete((2, 0)))");
    assert_eq!(chunk.held.len(), 1);
    // an enumeration parameter with fields of each kind
    let apply = f("::apply");
    assert_eq!(apply.broken[0].input, "op = Clamp { lo: 0, hi: -1 }, x = 0");
    assert_eq!(apply.held.len(), 2);
    assert_eq!(f("::compare").verdict, "held");
    // a trait's method on each type its `with Self` lines name
    assert_eq!(f("::<u8 as Halve>::halve").verdict, "held");
    assert_eq!(f("::<i64 as Halve>::halve").broken[0].input, "self = -3");
    // a generic default method on `&[u8]`, whose `&mut self` advances the slice
    let take = f("::<&[u8] as Take>::take_uint");
    assert_eq!(take.broken[0].input, "self = b\"\\x00\", n = 0");
    assert_eq!(take.broken[0].after, "self = b\"\", result = Some(0)");
    // a type with a pointer, built by its constructor and read through a method
    assert_eq!(f("::Blob::from_vec").verdict, "held");
    assert_eq!(f("::Blob::split_to").verdict, "held");
    let trunc = f("::Blob::truncate");
    assert_eq!(
        trunc.broken[0].input,
        "self = Blob::from_vec(b\"\\x00\\x00\\x00\\x00\\x00\"), len = 3"
    );
    // sorted inputs keep `requires sorted(v)` from the generator
    let search = f("::search");
    assert_eq!(search.verdict, "held");
    assert_eq!(search.rejected, 0, "{search:?}");
    assert_eq!(f("::roundtrip").verdict, "held");
    // what is refused says what to write
    let why = |n: &str| {
        rep.skipped
            .iter()
            .find(|s| s.contains(n))
            .unwrap_or_else(|| panic!("{n}: {:?}", rep.skipped))
    };
    assert!(why("::Size::size").contains("with Self = i64"));
    assert!(why("::Raw::len").contains("// hydo: build Raw from"));
    assert!(why("::compare_typo").contains("no variant `Greatr`"));
    // a finding replays alone
    let (code, _) = fuzz_m307(
        t.path(),
        &["--fn", "<i64 as Halve>::halve", "--input", "self = -3"],
    );
    assert_eq!(code, 1);
    let (code, _) = fuzz_m307(
        t.path(),
        &["--fn", "<i64 as Halve>::halve", "--input", "self = 4"],
    );
    assert_eq!(code, 0);
}
