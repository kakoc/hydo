//! Judging records: exact integers, entry and return reads, and the first broken call.

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
    };
    Plan {
        v: PLAN_V,
        kind: PLAN_TYPE.into(),
        programs: vec![Program {
            program: "/bin/ledger".into(),
            sha256: String::new(),
            arch: "arm64".into(),
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
    let t = records_text(std::slice::from_ref(&r));
    assert_eq!(
        t,
        "{\"f\":0,\"at\":7,\"in\":[1,null],\"lost\":\"no breakpoint\"}\n"
    );
    assert_eq!(serde_json::from_str::<Record>(t.trim()).unwrap(), r);
}
