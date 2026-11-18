//! The value history (Milestone 285): `record` and `key` lines, how a value is shown from a
//! call's reads (a float, a string, a Go interface's dynamic value, a Go map's value by key), and
//! a run's histories per key with each change.

use hydo_contracts::eval::{eval, V};
use hydo_contracts::lang::{scan, When};
use hydo_contracts::plan::*;
use hydo_contracts::values::{families, histories, points, show, Shown};

#[test]
fn record_and_key_lines_parse() {
    let src = "\
// hydo: key t.Ref
// hydo: record t.Limit as limit
// hydo: record t.Meta[\"overdraft_limit\"] as meta.limit at return
// hydo: record len(t.Items) at entry
// hydo: requires t.Limit > 0.5
func mark(t *txn) {
";
    let (cs, errs) = scan(src);
    assert!(errs.is_empty(), "{errs:?}");
    let r = &cs[0].records;
    assert_eq!(r.len(), 4);
    assert!(r[0].key && r[0].when == When::Entry && r[0].name == "t.Ref");
    assert_eq!((r[1].name.as_str(), r[1].when), ("limit", When::Both));
    assert_eq!(
        (r[2].name.as_str(), r[2].when),
        ("meta.limit", When::Return)
    );
    assert_eq!(
        (r[3].name.as_str(), r[3].when),
        ("len(t.Items)", When::Entry)
    );
    assert_eq!(cs[0].clauses.len(), 1);
    // a tuple field after a field is not a floating-point literal
    let (cs, errs) = scan("// hydo: ensures result.0.1 == 2\nfn f() -> ((u8, u8), u8) {\n");
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(cs[0].clauses.len(), 1);
    for bad in [
        "// hydo: key t.Ref at return\nfunc f(t *T) {\n",
        "// hydo: key t.Ref as r\nfunc f(t *T) {\n",
        "// hydo: record result at entry\nfunc f(t *T) int {\n",
        "// hydo: record old(t.X)\nfunc f(t *T) {\n",
        "// hydo: record t.X as a b\nfunc f(t *T) {\n",
        "// hydo: key t.A\n// hydo: key t.B\nfunc f(t *T) {\n",
    ] {
        let (_, errs) = scan(bad);
        assert_eq!(errs.len(), 1, "{bad}: {errs:?}");
    }
}

#[test]
fn floats_compare_and_compute() {
    let bits = 50.0f64.to_bits();
    let e = Expr::Bin(
        BinOp::Gt,
        Box::new(Expr::In(0, Ty::F(8))),
        Box::new(Expr::Float("0.5".into())),
    );
    assert_eq!(eval(&e, &[Some(bits)], None), Ok(V::Bool(true)));
    let e = Expr::Bin(
        BinOp::Eq,
        Box::new(Expr::In(0, Ty::F(8))),
        Box::new(Expr::Int("50".into())),
    );
    assert_eq!(eval(&e, &[Some(bits)], None), Ok(V::Bool(true)));
    let f32bits = 2.5f32.to_bits() as u64;
    let e = Expr::Bin(
        BinOp::Mul,
        Box::new(Expr::In(0, Ty::F(4))),
        Box::new(Expr::Int("2".into())),
    );
    assert_eq!(eval(&e, &[Some(f32bits)], None), Ok(V::Float(5.0)));
}

fn rd(i: u32, t: Ty) -> Expr {
    Expr::In(i, t)
}

fn le(s: &str) -> u64 {
    s.bytes()
        .enumerate()
        .fold(0, |a, (k, b)| a | (b as u64) << (8 * k))
}

#[test]
fn a_string_an_interface_and_a_map_value_are_shown() {
    // a string: its length and 8-byte chunks, cut to the length
    let text = Show::Text {
        len: rd(0, Ty::U(8)),
        chunks: vec![rd(1, Ty::Bytes(8)), rd(2, Ty::Bytes(8))],
    };
    let r = [Some(3), Some(le("d17\0\0\0\0\0")), None];
    assert_eq!(show(&text, &r, None), Shown::Text("d17".into(), false));
    assert_eq!(show(&text, &r, None).to_string(), "\"d17\"");
    // an interface: nil, a float64, another type
    let dynv = Show::Dyn {
        nil: rd(0, Ty::Ptr),
        ty: rd(0, Ty::Ptr),
        cases: vec![
            DynCase {
                addr: 0x1000,
                name: "float64".into(),
                show: Show::Val(rd(1, Ty::F(8))),
            },
            DynCase {
                addr: 0x2000,
                name: "bool".into(),
                show: Show::Val(rd(2, Ty::Bool)),
            },
        ],
    };
    assert_eq!(show(&dynv, &[Some(0), None, None], None), Shown::Nil);
    assert_eq!(
        show(&dynv, &[Some(0x1000), Some(50.0f64.to_bits()), None], None).to_string(),
        "50.0"
    );
    assert_eq!(
        show(&dynv, &[Some(0x2000), None, Some(1)], None),
        Shown::Bool(true)
    );
    assert!(matches!(
        show(&dynv, &[Some(0x3000), None, None], None),
        Shown::Other(0x3000)
    ));
    // a map of one group: slot 1 holds the key; a nil map and a missing key are absent; a
    // larger map is not read
    let slot = |c: u32, k: u32, v: u32| Slot {
        full: Expr::Bin(
            BinOp::Lt,
            Box::new(rd(c, Ty::U(1))),
            Box::new(Expr::Int("128".into())),
        ),
        hit: Expr::StrEq {
            len: Box::new(rd(k, Ty::U(8))),
            chunks: vec![rd(k + 1, Ty::Bytes(5))],
            lit: "limit".into(),
        },
        val: Show::Val(rd(v, Ty::F(8))),
    };
    let m = Show::Lookup {
        map: rd(0, Ty::Ptr),
        dir_len: rd(1, Ty::I(8)),
        group: rd(2, Ty::Ptr),
        slots: vec![slot(3, 4, 6), slot(7, 8, 10)],
    };
    let mut r = vec![
        Some(0x10),
        Some(0),
        Some(0x20),
        // slot 0: full, key "allow"
        Some(0x12),
        Some(5),
        Some(le("allow")),
        Some(1),
        // slot 1: full, key "limit" = 50.0
        Some(0x33),
        Some(5),
        Some(le("limit")),
        Some(50.0f64.to_bits()),
    ];
    assert_eq!(show(&m, &r, None), Shown::Float(50.0));
    r[7] = Some(0x80); // slot 1 empty
    assert_eq!(show(&m, &r, None), Shown::Absent);
    r[0] = Some(0);
    assert_eq!(show(&m, &r, None).to_string(), "absent");
    r[0] = Some(0x10);
    r[1] = Some(4);
    assert!(matches!(show(&m, &r, None), Shown::Unreadable(_)));
}

fn plan_with(values: Vec<Value>, entry: u64) -> Plan {
    Plan {
        v: PLAN_V,
        kind: PLAN_TYPE.into(),
        programs: vec![Program {
            program: "/bin/blnk".into(),
            sha256: String::new(),
            arch: "arm64".into(),
            pie: false,
            head: String::new(),
            unchecked: vec![],
            calls: vec![CallSite {
                callee: 0,
                ret: 0x500,
                caller: "github.com/x/y.(*Blnk).buildBatch".into(),
            }],
            keeps: vec![],
            paths: vec![],
            path_pages: vec![],
            paths_without_code: 0,
            functions: vec![Function {
                id: 0,
                name: "github.com/x/y.createQueueCopy".into(),
                file: "transaction_queue.go".into(),
                line: 428,
                lang: Lang::Go,
                entry,
                code: "00".into(),
                frame: Frame::G,
                at_entry: vec![],
                at_return: vec![],
                clauses: vec![],
                inputs: vec![],
                values,
                ret_addr: Some(3),
            }],
        }],
    }
}

#[test]
fn a_run_gives_one_history_per_key_with_its_changes() {
    let values = vec![
        Value {
            name: "t.Ref".into(),
            text: "t.Ref".into(),
            line: 420,
            key: true,
            entry: Some(Show::Text {
                len: rd(0, Ty::U(8)),
                chunks: vec![rd(1, Ty::Bytes(8))],
            }),
            at_return: None,
        },
        Value {
            name: "overdraft_limit".into(),
            text: "t.OverdraftLimit as overdraft_limit".into(),
            line: 421,
            key: false,
            entry: Some(Show::Val(rd(2, Ty::F(8)))),
            at_return: None,
        },
    ];
    let plan = plan_with(values, 0x100);
    let rec = |at: u64, key: &str, limit: f64, ra: u64| Record {
        f: 0,
        at,
        at_entry: vec![
            Some(key.len() as u64),
            Some(le(key)),
            Some(limit.to_bits()),
            Some(ra),
        ],
        ..Record::default()
    };
    let recs = vec![
        rec(100, "d17", 50.0, 0x400),
        rec(150, "d18", 50.0, 0x400),
        rec(300, "d17_q", 0.0, 0x500),
        rec(310, "d18_q", 50.0, 0x500),
    ];
    let pts = points(&plan, &recs);
    assert_eq!(pts.len(), 4);
    assert_eq!(
        pts[2].caller.as_deref(),
        Some("github.com/x/y.(*Blnk).buildBatch")
    );
    assert_eq!(pts[0].caller.as_deref(), Some("0x400"));
    let hs = histories(pts);
    assert_eq!(hs.len(), 2);
    assert_eq!(hs[0].key.as_deref(), Some("d17"));
    assert_eq!(hs[0].copies, vec!["d17_q".to_string()]);
    assert_eq!(hs[0].changes.len(), 1);
    let c = &hs[0].changes[0];
    assert_eq!(
        (
            c.name.as_str(),
            c.from.as_str(),
            c.to.as_str(),
            c.before,
            c.point
        ),
        ("overdraft_limit", "50.0", "0.0", 0, 1)
    );
    assert!(hs[1].changes.is_empty());
    let f = families(&["a".into(), "a_q".into(), "ab".into(), "a_q.1".into()]);
    assert_eq!(f["a_q"], "a");
    assert_eq!(f["ab"], "ab");
    assert_eq!(f["a_q.1"], "a");
}

#[test]
fn a_return_not_reached_is_unreadable_and_is_no_change() {
    let values = vec![Value {
        name: "limit".into(),
        text: "t.Limit as limit".into(),
        line: 1,
        key: false,
        entry: Some(Show::Val(rd(0, Ty::F(8)))),
        at_return: Some(Show::Val(Expr::Out(0, Ty::F(8)))),
    }];
    let plan = plan_with(values, 0x100);
    let recs = vec![
        Record {
            f: 0,
            at: 10,
            at_entry: vec![Some(50.0f64.to_bits()), None, None, None],
            ret: Some(20),
            at_return: Some(vec![Some(50.0f64.to_bits())]),
            ..Record::default()
        },
        Record {
            f: 0,
            at: 30,
            at_entry: vec![Some(50.0f64.to_bits()), None, None, None],
            lost: Some("power cut".into()),
            ..Record::default()
        },
        Record {
            f: 0,
            at: 5,
            boot: 1,
            at_entry: vec![Some(0), None, None, None],
            ..Record::default()
        },
    ];
    let pts = points(&plan, &recs);
    assert_eq!(
        pts.iter()
            .map(|p| (p.when, p.at, p.boot))
            .collect::<Vec<_>>(),
        vec![
            ("entry", 10, 0),
            ("return", 20, 0),
            ("entry", 30, 0),
            ("return", 30, 0),
            ("entry", 5, 1),
            ("return", 5, 1),
        ]
    );
    assert!(matches!(&pts[3].values[0].1, Shown::Unreadable(w) if w.contains("power cut")));
    let hs = histories(pts);
    assert_eq!(hs.len(), 1);
    // 50.0 four times (the unreadable return skipped), then 0.0 in the next boot
    assert_eq!(hs[0].changes.len(), 1);
    assert_eq!(hs[0].changes[0].point, 4);
}
