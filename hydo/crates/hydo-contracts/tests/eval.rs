//! Judging records: exact integers, entry and return reads, and the first broken call.

use crate::tmpdir::TmpDir;
use hydo_contracts::eval::{eval, judge, typed, V};
use hydo_contracts::plan::*;

fn bin(op: BinOp, a: Expr, b: Expr) -> Expr {
    Expr::Bin(op, Box::new(a), Box::new(b))
}

#[test]
fn reads_take_their_type() {
    assert_eq!(typed(0xffff_ffff_ffff_fffe, Ty::I(8)), V::Int(-2));
    assert_eq!(typed(0x1_0000_00ff, Ty::U(1)), V::Int(255));
    assert_eq!(typed(0x80, Ty::I(1)), V::Int(-128));
    assert_eq!(typed(0x100, Ty::Bool), V::Bool(false));
}

#[test]
fn integers_are_exact_and_or_stops_early() {
    // 1 - 2 of two unsigned reads is -1, not a wrapped u64
    let e = bin(
        BinOp::Lt,
        bin(BinOp::Sub, Expr::In(0, Ty::U(8)), Expr::In(1, Ty::U(8))),
        Expr::Int("0".into()),
    );
    assert_eq!(eval(&e, &[Some(1), Some(2)], None), Ok(V::Bool(true)));
    // the right side's read failed, and the left side decides
    let e = bin(BinOp::Or, Expr::Bool(true), Expr::Out(0, Ty::U(8)));
    assert_eq!(eval(&e, &[], None), Ok(V::Bool(true)));
    let e = bin(BinOp::And, Expr::Bool(true), Expr::Out(0, Ty::U(8)));
    assert!(eval(&e, &[], None).is_err());
    let e = bin(BinOp::Div, Expr::Int("1".into()), Expr::In(0, Ty::U(8)));
    assert!(eval(&e, &[Some(0)], None).is_err());
}

fn ledger_plan() -> Plan {
    // ensures req > old(last) || balance == old(balance)
    let ensures = bin(
        BinOp::Or,
        bin(BinOp::Gt, Expr::In(0, Ty::U(8)), Expr::In(1, Ty::U(8))),
        bin(BinOp::Eq, Expr::Out(0, Ty::U(8)), Expr::In(2, Ty::U(8))),
    );
    let loc = |name: &str, base, path: Vec<i64>| Loc {
        name: name.into(),
        base,
        path,
        size: 8,
        shift: 0,
    };
    Plan {
        v: PLAN_V,
        kind: PLAN_TYPE.into(),
        programs: vec![Program {
            program: "/bin/ledger".into(),
            sha256: String::new(),
            arch: "arm64".into(),
            pie: false,
            head: String::new(),
            unchecked: vec![],
            functions: vec![Function {
                id: 0,
                name: "ledger::apply".into(),
                file: "src/main.rs".into(),
                line: 38,
                lang: Lang::Rust,
                entry: 0x225ee0,
                code: "00".into(),
                frame: Frame::Sp,
                at_entry: vec![
                    loc("req_id", Base::Reg(2), vec![]),
                    loc("old(l.last_req)", Base::Reg(0), vec![8]),
                    loc("old(l.balance)", Base::Reg(0), vec![0]),
                    loc("l", Base::Reg(0), vec![]),
                ],
                at_return: vec![loc("l.balance", Base::In(3), vec![0])],
                clauses: vec![
                    Clause {
                        kind: ClauseKind::Requires,
                        text: "req_id > 0".into(),
                        line: 34,
                        expr: bin(BinOp::Gt, Expr::In(0, Ty::U(8)), Expr::Int("0".into())),
                    },
                    Clause {
                        kind: ClauseKind::Ensures,
                        text: "req_id > old(l.last_req) || l.balance == old(l.balance)".into(),
                        line: 35,
                        expr: ensures,
                    },
                ],
                inputs: vec![],
            }],
        }],
    }
}

#[test]
fn the_first_broken_call_is_named_with_its_values() {
    let plan = ledger_plan();
    let rec = |at, req, last, bal, after: Option<u64>| Record {
        f: 0,
        at,
        at_entry: vec![Some(req), Some(last), Some(bal), Some(0x1000)],
        ret: after.map(|_| at + 5),
        at_return: after.map(|b| vec![Some(b)]),
        lost: None,
    };
    let recs = vec![
        rec(100, 1, 0, 1000, Some(990)),
        rec(200, 2, 1, 990, Some(980)),
        // the retry of request 2: applied again
        rec(300, 2, 2, 980, Some(970)),
        // a call the run ended inside: requires judged, ensures not
        rec(400, 3, 2, 970, None),
    ];
    let r = judge(&plan, &recs);
    assert_eq!((r.calls, r.without_return), (4, 1));
    let req = &r.outcomes[0];
    assert_eq!((req.calls, req.violations), (4, 0));
    let ens = &r.outcomes[1];
    assert_eq!((ens.calls, ens.violations, ens.undecided), (3, 1, 1));
    let first = ens.first.as_ref().unwrap();
    assert_eq!((first.call, first.at), (3, 300));
    assert_eq!(
        first.values,
        vec![
            "req_id = 2",
            "old(l.last_req) = 2",
            "old(l.balance) = 980",
            "l.balance = 970"
        ]
    );
    assert_eq!(r.violated().count(), 1);
}

#[test]
fn plans_and_records_read_back() {
    let plan = ledger_plan();
    let text = plan_text(&plan);
    let back: Plan = serde_json::from_str(&text).unwrap();
    assert_eq!(back, plan);
    let r = Record {
        f: 0,
        at: 7,
        at_entry: vec![Some(1), None],
        ret: None,
        at_return: None,
        lost: Some("no breakpoint".into()),
    };
    let t = records_text(std::slice::from_ref(&r), 3);
    assert_eq!(
        t,
        "{\"f\":0,\"at\":7,\"in\":[1,null],\"lost\":\"no breakpoint\"}\n\
         {\"end\":{\"recorded\":1,\"not_recorded\":3}}\n"
    );
    assert_eq!(
        serde_json::from_str::<Record>(t.lines().next().unwrap()).unwrap(),
        r
    );
    // the end line says how many calls were not recorded; records without one (before
    // Milestone 255) count every call as recorded
    let dir = TmpDir::fresh(
        std::env::temp_dir().join(format!("hydo-contracts-end-{}", std::process::id())),
    );
    let p = dir.join("a.contracts.jsonl");
    std::fs::write(&p, &t).unwrap();
    let (recs, end) = read_run(&p).unwrap();
    assert_eq!(
        (recs, end.recorded, end.not_recorded),
        (vec![r.clone()], 1, 3)
    );
    std::fs::write(&p, t.lines().next().unwrap()).unwrap();
    let (recs, end) = read_run(&p).unwrap();
    assert_eq!((recs.len(), end.recorded, end.not_recorded), (1, 1, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_report_counts_the_calls_not_judged() {
    let mut rep = judge(&ledger_plan(), &[]);
    rep.not_recorded = 100_000;
    assert_eq!((rep.calls, rep.not_recorded), (0, 100_000));
    let back: hydo_contracts::eval::Report =
        serde_json::from_str(&serde_json::to_string(&rep).unwrap()).unwrap();
    assert_eq!(back.not_recorded, 100_000);
}

#[test]
fn guards_elements_strings_and_variants() {
    // some(o) when o is Some (discriminant 1), undecided when it is None
    let some = Expr::Guard {
        cond: Box::new(Expr::Is {
            discr: Box::new(Expr::In(0, Ty::U(8))),
            any: vec!["1".into()],
            not: vec![],
        }),
        then: Box::new(Expr::In(1, Ty::U(8))),
        why: "`o` is not Some".into(),
    };
    assert_eq!(eval(&some, &[Some(1), Some(7)], None), Ok(V::Int(7)));
    assert!(eval(&some, &[Some(0), Some(7)], None).is_err());
    // a niche: the variant without a value is every discriminant the others do not have
    let niche = Expr::Is {
        discr: Box::new(Expr::In(0, Ty::U(8))),
        any: vec![],
        not: vec!["0".into()],
    };
    assert_eq!(eval(&niche, &[Some(0x1000)], None), Ok(V::Bool(true)));
    assert_eq!(eval(&niche, &[Some(0)], None), Ok(V::Bool(false)));
    // v[k]: the element the index names, undecided outside the elements read
    let elem = Expr::Elem {
        idx: Box::new(Expr::In(0, Ty::U(8))),
        first: 0,
        elems: vec![Expr::In(1, Ty::U(4)), Expr::In(2, Ty::U(4))],
    };
    assert_eq!(
        eval(&elem, &[Some(1), Some(10), Some(20)], None),
        Ok(V::Int(20))
    );
    assert!(eval(&elem, &[Some(2), Some(10), Some(20)], None).is_err());
    // s == "hello": the length first, then the bytes
    let s = Expr::StrEq {
        len: Box::new(Expr::In(0, Ty::U(8))),
        chunks: vec![Expr::In(1, Ty::Bytes(5))],
        lit: "hello".into(),
    };
    let hello = u64::from_le_bytes(*b"hello\0\0\0");
    assert_eq!(eval(&s, &[Some(5), Some(hello)], None), Ok(V::Bool(true)));
    assert_eq!(
        eval(&s, &[Some(5), Some(hello + 1)], None),
        Ok(V::Bool(false))
    );
    // another length decides without the bytes, even unreadable ones
    assert_eq!(eval(&s, &[Some(4), None], None), Ok(V::Bool(false)));
}

#[test]
fn a_register_read_shifts_and_cuts() {
    let l = Loc {
        base: Base::Reg(0),
        size: 2,
        shift: 16,
        ..Loc::default()
    };
    let v = l.load(&|_| Some(0x1122_3344_5566_7788), &[], &|_, _| None);
    assert_eq!(v, Some(0x5566));
    // a path: the last load reads `size` bytes
    let m = Loc {
        base: Base::Reg(0),
        path: vec![8, 4],
        size: 1,
        ..Loc::default()
    };
    let v = m.load(&|_| Some(0x1000), &[], &|va, n| {
        Some(if va == 0x1008 { 0x2000 } else { va + n as u64 })
    });
    assert_eq!(v, Some(0x2005));
}
