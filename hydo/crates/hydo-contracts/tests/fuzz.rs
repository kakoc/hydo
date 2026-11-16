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
    assert!(t("f64").generable().is_err());
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
            },
            harness::Param {
                name: "ids".into(),
                pass: Pass::Ref { unsized_ref: true },
                ty: FTy::List(Box::new(u64t())),
            },
        ],
        ret: FTy::Opt(Box::new(u64t())),
        ret_ref: false,
        is_unsafe: false,
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
