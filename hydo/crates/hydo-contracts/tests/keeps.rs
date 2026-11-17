//! Value properties (Milestone 291): `keep` lines parse, the compiler resolves the functions
//! they name, and a run's histories break or keep them, by key, in one boot or across boots.

use hydo_contracts::compile::keep_plan;
use hydo_contracts::lang::scan;
use hydo_contracts::plan::*;
use hydo_contracts::values::{break_lines, histories, judge_keeps, points};

#[test]
fn keep_lines_parse() {
    let src = "\
// hydo: keep overdraft_limit from blnk.setTransactionMetadata to model.canProcessTransaction
// hydo: keep meta.limit once set across boots
// hydo: keep limit
// hydo: key t.Ref
// hydo: record t.Limit as limit
func decide(t *txn) {
";
    let (cs, errs) = scan(src);
    assert!(errs.is_empty(), "{errs:?}");
    let k = &cs[0].keeps;
    assert_eq!(k.len(), 3);
    assert_eq!(k[0].name, "overdraft_limit");
    assert_eq!(k[0].from.as_deref(), Some("blnk.setTransactionMetadata"));
    assert_eq!(k[0].to.as_deref(), Some("model.canProcessTransaction"));
    assert!(!k[0].once_set && !k[0].across_boots);
    assert_eq!(
        k[0].text,
        "keep overdraft_limit from blnk.setTransactionMetadata to model.canProcessTransaction"
    );
    assert!(k[1].once_set && k[1].across_boots && k[1].from.is_none());
    assert_eq!(k[2].text, "keep limit");
    assert_eq!(cs[0].records.len(), 2);
    for bad in [
        "// hydo: keep\nfunc f(t *T) {\n",
        "// hydo: keep from f\nfunc f(t *T) {\n",
        "// hydo: keep x from\nfunc f(t *T) {\n",
        "// hydo: keep x from a from b\nfunc f(t *T) {\n",
        "// hydo: keep x once\nfunc f(t *T) {\n",
        "// hydo: keep x always\nfunc f(t *T) {\n",
        "// hydo: keep x from to\nfunc f(t *T) {\n",
    ] {
        let (_, errs) = scan(bad);
        assert_eq!(errs.len(), 1, "{bad}: {errs:?}");
    }
}

const API: &str = "github.com/x/blnk.setTransactionMetadata";
const COPY: &str = "github.com/x/blnk.createQueueCopy";
const DECIDE: &str = "github.com/x/blnk/model.canProcessTransaction";

fn rd(i: u32, t: Ty) -> Expr {
    Expr::In(i, t)
}

fn le(s: &str) -> u64 {
    s.bytes()
        .enumerate()
        .fold(0, |a, (k, b)| a | (b as u64) << (8 * k))
}

fn value(name: &str, key: bool, show: Show) -> Value {
    Value {
        name: name.into(),
        text: name.into(),
        line: 1,
        key,
        entry: Some(show),
        at_return: None,
    }
}

fn function(id: u32, name: &str) -> Function {
    Function {
        id,
        name: name.into(),
        file: "queue.go".into(),
        line: 10 + id,
        lang: Lang::Go,
        entry: 0x100 * (id as u64 + 1),
        code: "00".into(),
        frame: Frame::G,
        at_entry: vec![],
        at_return: vec![],
        clauses: vec![],
        inputs: vec![],
        values: vec![
            value(
                "t.Ref",
                true,
                Show::Text {
                    len: rd(0, Ty::U(8)),
                    chunks: vec![rd(1, Ty::Bytes(8))],
                },
            ),
            value("overdraft_limit", false, Show::Val(rd(2, Ty::F(8)))),
            value("meta", false, Show::Val(rd(3, Ty::Ptr))),
        ],
        ret_addr: None,
    }
}

fn plan(keeps: &[&str]) -> Plan {
    let funcs = vec![function(0, API), function(1, COPY), function(2, DECIDE)];
    let src: String = keeps
        .iter()
        .map(|k| format!("// hydo: {k}\n"))
        .collect::<String>()
        + "func decide(t *T) {\n";
    let (cs, errs) = scan(&src);
    assert!(errs.is_empty(), "{errs:?}");
    let keeps = cs[0]
        .keeps
        .iter()
        .map(|k| keep_plan(&funcs, k, "model/model.go").expect("a keep"))
        .collect();
    Plan {
        v: PLAN_V,
        kind: PLAN_TYPE.into(),
        programs: vec![Program {
            program: "/bin/blnk".into(),
            arch: "arm64".into(),
            functions: funcs,
            keeps,
            ..Program::default()
        }],
    }
}

fn rec(f: u32, at: u64, boot: u32, key: &str, limit: f64, meta: u64) -> Record {
    Record {
        f,
        at,
        boot,
        at_entry: vec![
            Some(key.len() as u64),
            Some(le(key)),
            Some(limit.to_bits()),
            Some(meta),
        ],
        ..Record::default()
    }
}

/// d17: the API sets 50.0, the worker's copy loses it, the decision sees 0.0. d18 keeps 50.0.
/// d19: 50.0 before a power cut, 0.0 at the decision in the recovery boot.
fn run() -> Vec<Record> {
    vec![
        rec(0, 100, 0, "d17", 50.0, 0x10),
        rec(0, 110, 0, "d18", 50.0, 0),
        rec(1, 200, 0, "d17", 50.0, 0x10),
        rec(1, 300, 0, "d17_q", 0.0, 0),
        rec(2, 310, 0, "d17_q", 0.0, 0),
        rec(2, 320, 0, "d18_q", 50.0, 0),
        rec(0, 400, 0, "d19", 50.0, 0x10),
        rec(2, 50, 1, "d19_q", 0.0, 0),
    ]
}

#[test]
fn a_keep_from_one_function_to_another_breaks_at_the_decision() {
    let p = plan(&[
        "keep overdraft_limit from blnk.setTransactionMetadata to model.canProcessTransaction",
    ]);
    let k = &p.programs[0].keeps[0];
    assert_eq!(k.from.as_deref(), Some(API));
    assert_eq!(k.to.as_deref(), Some(DECIDE));
    let hs = histories(points(&p, &run()));
    let out = judge_keeps(&p, &hs);
    assert_eq!(out.len(), 1);
    let o = &out[0];
    // d17 and d18 are judged once each; d19's decision is in another boot than its reference
    assert_eq!((o.keys, o.stops, o.breaks.len()), (2, 2, 1));
    let b = &o.breaks[0];
    let h = &hs[b.history];
    assert_eq!(h.key.as_deref(), Some("d17"));
    assert_eq!(h.points[b.point].function, DECIDE);
    assert_eq!(h.points[b.reference].function, API);
    // the first changing call: the worker's copy
    assert_eq!(h.points[b.changed].function, COPY);
    assert_eq!(h.points[b.changed].key.as_deref(), Some("d17_q"));
    assert_eq!((b.value.as_str(), b.expected.as_str()), ("0.0", "50.0"));
    let lines = break_lines(h, b, &o.keep);
    assert_eq!(lines.len(), 5, "{lines:#?}");
    assert!(lines[1].contains("setTransactionMetadata") && lines[1].ends_with("<- the reference"));
    assert!(
        lines[3].contains("createQueueCopy entry [d17_q]")
            && lines[3].ends_with("<- first changed here")
    );
    assert!(lines[4].contains("= 0.0") && lines[4].ends_with("<- BROKEN"));
}

#[test]
fn a_keep_across_boots_judges_the_recovery_boot() {
    let p = plan(&[
        "keep overdraft_limit from blnk.setTransactionMetadata to model.canProcessTransaction across boots",
    ]);
    let hs = histories(points(&p, &run()));
    let o = &judge_keeps(&p, &hs)[0];
    assert_eq!((o.keys, o.stops, o.breaks.len()), (3, 3, 2));
    let b = &o.breaks[1];
    assert_eq!(hs[b.history].key.as_deref(), Some("d19"));
    assert_eq!(hs[b.history].points[b.point].boot, 1);
}

#[test]
fn a_plain_keep_breaks_at_the_first_change_and_once_set_at_the_first_absence() {
    let p = plan(&["keep overdraft_limit", "keep meta once set"]);
    let hs = histories(points(&p, &run()));
    let out = judge_keeps(&p, &hs);
    let same = &out[0];
    assert_eq!(same.breaks.len(), 1);
    let b = &same.breaks[0];
    // without `from` the reference is the key's first value, and the copy breaks it
    assert_eq!(hs[b.history].points[b.point].function, COPY);
    assert_eq!((b.reference, b.changed), (0, b.point));
    let set = &out[1];
    // d17's meta goes from 0x10 to nil; d18 never had it; d19's nil is in the next boot
    assert_eq!(set.breaks.len(), 1);
    let b = &set.breaks[0];
    assert_eq!(hs[b.history].key.as_deref(), Some("d17"));
    assert_eq!(b.value, "nil");
}

#[test]
fn a_keep_names_recording_functions_and_values() {
    let funcs = vec![function(0, API), function(1, COPY)];
    for (line, why) in [
        ("keep nothing", "no `record` line"),
        ("keep overdraft_limit from blnk.nowhere", "no function"),
        ("keep overdraft_limit from x", "no function"),
        ("keep t.Ref", "no `record` line"),
    ] {
        let (cs, errs) = scan(&format!("// hydo: {line}\nfunc f(t *T) {{\n"));
        assert!(errs.is_empty());
        let e = keep_plan(&funcs, &cs[0].keeps[0], "a.go").unwrap_err();
        assert!(e.contains(why), "{line}: {e}");
    }
    // a short name that two functions end with is refused with both names
    let two = vec![function(0, "a/x.f"), function(1, "b/x.f")];
    let (cs, _) = scan("// hydo: keep overdraft_limit from x.f\nfunc f(t *T) {\n");
    let e = keep_plan(&two, &cs[0].keeps[0], "a.go").unwrap_err();
    assert!(e.contains("a/x.f") && e.contains("b/x.f"), "{e}");
}
