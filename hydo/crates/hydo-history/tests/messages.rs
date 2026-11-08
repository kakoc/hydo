//! The messages checker (docs/design/messaging.md): hand-written histories per claim (violating,
//! passing, and with unknown outcomes), the design's examples, a seeded generator of correct
//! histories of a simulated queue server with every claim declared (they must all hold) and
//! mutations of them that must each break exactly the claims they target, and the queue
//! linearizability patterns against an exhaustive search on generated small histories.

use hydo_history::messages::decl::{self, conflicts, from_spec, Decl};
use hydo_history::messages::queue::{patterns, search, QKind, QOp};
use hydo_history::messages::{self, Status};
use hydo_history::parse_sources;
use hydo_history::py::json_loads;
use std::collections::{BTreeMap, BTreeSet};

// ------------------------------------------------------------------ a history builder

/// One record: process, type, time in ms, operation and arguments.
#[derive(Clone, Debug)]
struct Rec {
    p: String,
    ty: &'static str,
    t_ms: i64,
    f: String,
    args: Vec<String>,
}

#[derive(Default, Clone, Debug)]
struct H {
    recs: Vec<Rec>,
}

impl H {
    fn rec(&mut self, p: impl ToString, ty: &'static str, t_ms: i64, f: &str, args: &[&str]) {
        self.recs.push(Rec {
            p: p.to_string(),
            ty,
            t_ms,
            f: f.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
    }
    fn text(&self) -> String {
        let mut s = String::new();
        for r in &self.recs {
            s.push_str(&format!(
                "HYDO-HIST q {} {} {} {} {}\n",
                r.p,
                r.ty,
                r.t_ms * 1_000_000,
                r.f,
                r.args.join(" ")
            ));
        }
        s
    }
}

/// The claims of a check over these lines: name -> (status, summary).
fn run(text: &str, decl_json: &str) -> BTreeMap<String, (Status, String)> {
    run_sources(&[("w", text.to_string())], decl_json)
}

fn run_sources(srcs: &[(&str, String)], decl_json: &str) -> BTreeMap<String, (Status, String)> {
    let (ev, _, _) = parse_sources(srcs).unwrap();
    let spec = json_loads(decl_json).unwrap();
    let d = from_spec(&spec).unwrap();
    assert!(conflicts(&d, None).is_empty(), "{:?}", conflicts(&d, None));
    let clock = messages::hist::clock_positions(&ev, &messages::hist::clock_lines(srcs));
    let r = messages::check(&ev, "q", &d, &clock);
    r.claims
        .iter()
        .map(|c| (c.name.to_string(), (c.status, c.summary.clone())))
        .collect()
}

fn violated(r: &BTreeMap<String, (Status, String)>) -> BTreeSet<String> {
    r.iter()
        .filter(|(_, v)| v.0 == Status::Violated)
        .map(|(k, _)| k.clone())
        .collect()
}

fn st(r: &BTreeMap<String, (Status, String)>, c: &str) -> Status {
    r.get(c)
        .unwrap_or_else(|| panic!("no claim {c} in {r:?}"))
        .0
}

fn set(xs: &[&str]) -> BTreeSet<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

// ------------------------------------------------------------------ the design's examples

#[test]
fn design_example_holds() {
    // docs/design/messaging.md "Records": process 3 sends two messages, process 5 receives one
    // and acknowledges it; the drain receives the other.
    let text = "\
HYDO-HIST q 3 invoke 12000000000 send orders m17;k=g2 m18;k=g2
HYDO-HIST q 3 ok 12004000000 send orders m17;k=g2;o=81 m18;k=g2;o=82
HYDO-HIST q 5 invoke 12010000000 recv orders -
HYDO-HIST q 5 ok 12013000000 recv orders - m17;k=g2;o=81;n=1;h=a91f;v=30000
HYDO-HIST q 5 invoke 12500000000 ack orders - m17;h=a91f
HYDO-HIST q 5 ok 12502000000 ack orders - m17;h=a91f
HYDO-HIST q 9 invoke 13000000000 recv orders -
HYDO-HIST q 9 ok 13001000000 recv orders - m18;k=g2;o=82;n=1;h=b1;v=30000
HYDO-HIST q 9 invoke 13002000000 ack orders - m18;h=b1
HYDO-HIST q 9 ok 13003000000 ack orders - m18;h=b1
HYDO-HIST q 9 ok 13004000000 drained orders -
";
    let r = run(
        text,
        r#"{"id":"payments","type":"messages","history":"q","claims":{"delivery":"at-least-once",
        "duplicates":"forbidden","order":{"kind":"fifo-blocking","scope":"key","batch":"ordered"},
        "lease":{"ms":"per-delivery"},"after_ack":"no-redelivery","processing":"once"}}"#,
    );
    assert!(violated(&r).is_empty(), "{r:?}");
    for c in [
        "no-phantom",
        "no-loss",
        "duplicates",
        "order",
        "lease",
        "after-ack",
        "processing",
    ] {
        assert_eq!(st(&r, c), Status::Held, "{c}: {r:?}");
    }
}

#[test]
fn a_declaration_of_nothing_measures_and_passes() {
    let text = "HYDO-HIST q 1 invoke 1000000 send c m1\nHYDO-HIST q 1 ok 2000000 send c m1\n";
    let r = run(text, r#"{"id":"bus","type":"messages","history":"q"}"#);
    assert_eq!(r.keys().cloned().collect::<Vec<_>>(), vec!["no-phantom"]);
    assert_eq!(st(&r, "no-phantom"), Status::Held);
}

// ------------------------------------------------------------------ declarations

#[test]
fn unknown_keys_and_values_are_refused() {
    for (j, frag) in [
        (
            r#"{"id":"x","type":"messages","history":"q","claims":{"leese":"none"}}"#,
            "leese",
        ),
        (
            r#"{"id":"x","type":"messages","history":"q","acknowledgment":"none"}"#,
            "acknowledgment",
        ),
        (
            r#"{"id":"x","type":"messages","history":"q","claims":{"order":"sorted"}}"#,
            "sorted",
        ),
        (
            r#"{"id":"x","type":"messages","history":"q","claims":{"lease":{"ms":0}}}"#,
            "more than 0",
        ),
        (
            r#"{"id":"x","type":"messages","history":"q","measures":{"quantiles":[2]}}"#,
            "0 to 1",
        ),
    ] {
        let e = from_spec(&json_loads(j).unwrap()).unwrap_err();
        assert!(e.contains(frag), "{e}");
    }
}

#[test]
fn contradictions_name_both_keys() {
    let d = |j: &str| from_spec(&json_loads(j).unwrap()).unwrap();
    let c = conflicts(&d(r#"{"claims":{"linearizable":"fifo"}}"#), Some(true));
    assert!(
        c[0].contains("claims.linearizable") && c[0].contains("on-receive"),
        "{c:?}"
    );
    let c = conflicts(
        &d(r#"{"acknowledgement":"on-receive","claims":{"lease":{"ms":5},"processing":"once"}}"#),
        Some(true),
    );
    assert_eq!(c.len(), 2, "{c:?}");
    let c = conflicts(
        &d(r#"{"claims":{"delivery":"exactly-once","lease":{"ms":5}}}"#),
        Some(true),
    );
    assert!(c[0].contains("at-most-once"), "{c:?}");
    let c = conflicts(&d(r#"{"claims":{"fan_out":"every-group"}}"#), Some(false));
    assert!(c[0].contains("final:"), "{c:?}");
    assert!(conflicts(&d(r#"{"claims":{"fan_out":"every-group"}}"#), None).is_empty());
    assert!(conflicts(&Decl::default(), Some(false)).is_empty());
}

#[test]
fn the_declaration_shows_every_default() {
    let d = from_spec(&json_loads(r#"{"claims":{"lease":{"ms":30000}}}"#).unwrap()).unwrap();
    let j = hydo_history::py::dumps_sorted(&decl::to_pyval(&d));
    assert_eq!(
        j,
        r#"{"acknowledgement":"explicit","claims":{"after_ack":"allowed","dead_letter":"none","delay":"none","delivery":"none","duplicates":"allowed","fan_out":"none","lease":{"ms":30000},"linearizable":"none","members":"any","order":{"batch":"unordered","kind":"none","scope":"key"},"processing":"any"},"clock_steps":"undecided","measures":{"channels":"all","quantiles":[0.5,0.95,0.99,1],"windows":true}}"#
    );
}

// ------------------------------------------------------------------ hand-written claims

const ALO: &str =
    r#"{"id":"c","type":"messages","history":"q","claims":{"delivery":"at-least-once"}}"#;

fn sent(h: &mut H, p: i64, t: i64, m: &str, out: &'static str) {
    h.rec(p, "invoke", t, "send", &["c", m]);
    if out != "open" {
        h.rec(p, out, t + 1, "send", &["c", m]);
    }
}

fn recv(h: &mut H, p: i64, t: i64, items: &[&str]) {
    h.rec(p, "invoke", t, "recv", &["c", "-"]);
    let mut a = vec!["c", "-"];
    a.extend(items);
    h.rec(p, "ok", t + 1, "recv", &a);
}

fn ack(h: &mut H, p: i64, t: i64, item: &str, out: &'static str) {
    h.rec(p, "invoke", t, "ack", &["c", "-", item]);
    h.rec(p, out, t + 1, "ack", &["c", "-", item]);
}

#[test]
fn no_loss_and_its_excuses() {
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    sent(&mut h, 1, 2, "m2", "ok");
    recv(&mut h, 2, 4, &["m1"]);
    h.rec(9, "ok", 10, "drained", &["c", "-"]);
    let r = run(&h.text(), ALO);
    assert_eq!(st(&r, "no-loss"), Status::Violated);
    assert!(r["no-loss"].1.starts_with("m2 was acknowledged"), "{r:?}");

    // the same with m2's send unknown: no loss is claimed for it
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    sent(&mut h, 1, 2, "m2", "info");
    recv(&mut h, 2, 4, &["m1"]);
    h.rec(9, "ok", 10, "drained", &["c", "-"]);
    assert_eq!(st(&run(&h.text(), ALO), "no-loss"), Status::Held);

    // no drained record: undecided (the check is an ERROR)
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    let r = run(&h.text(), ALO);
    assert_eq!(st(&r, "no-loss"), Status::Undecided);
    assert!(r["no-loss"].1.contains("drained"));

    // retention: a drain long after the send excuses a missing message
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    h.rec(9, "ok", 5000, "drained", &["c", "-"]);
    let ret = r#"{"id":"c","type":"messages","history":"q","retention_ms":1000,"claims":{"delivery":"at-least-once"}}"#;
    assert_eq!(st(&run(&h.text(), ret), "no-loss"), Status::Held);
    // ... but not one shortly after
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    h.rec(9, "ok", 500, "drained", &["c", "-"]);
    assert_eq!(st(&run(&h.text(), ret), "no-loss"), Status::Violated);
    // ... and across a power cut it cannot be measured
    let mut h1 = H::default();
    sent(&mut h1, 1, 0, "m1", "ok");
    let mut h2 = H::default();
    h2.rec(9, "ok", 1, "drained", &["c", "-"]);
    let r = run_sources(&[("w", h1.text()), ("w.recovery", h2.text())], ret);
    assert_eq!(st(&r, "no-loss"), Status::Held);
    // without retention, a loss across a power cut is a loss
    let r = run_sources(&[("w", h1.text()), ("w.recovery", h2.text())], ALO);
    assert_eq!(st(&r, "no-loss"), Status::Violated);

    // on-receive: a lost receive answer may have consumed the message
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    h.rec(2, "invoke", 4, "recv", &["c", "-"]);
    h.rec(2, "info", 6, "recv", &["c", "-"]);
    h.rec(9, "ok", 10, "drained", &["c", "-"]);
    let onr = r#"{"id":"c","type":"messages","history":"q","acknowledgement":"on-receive","claims":{"delivery":"at-least-once"}}"#;
    assert_eq!(st(&run(&h.text(), onr), "no-loss"), Status::Held);
    assert_eq!(st(&run(&h.text(), ALO), "no-loss"), Status::Violated);
}

#[test]
fn no_loss_witness_names_the_gap_by_offset() {
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1;k=a"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;k=a;o=1"]);
    h.rec(1, "invoke", 2, "send", &["c", "m2;k=a"]);
    h.rec(1, "ok", 3, "send", &["c", "m2;k=a;o=2"]);
    recv(&mut h, 2, 4, &["m2;k=a;o=2"]);
    h.rec(9, "ok", 10, "drained", &["c", "-"]);
    let r = run(&h.text(), ALO);
    assert!(r["no-loss"].1.contains("offset 1 is a gap"), "{r:?}");
}

#[test]
fn no_phantom() {
    let d = r#"{"id":"c","type":"messages","history":"q"}"#;
    let mut h = H::default();
    recv(&mut h, 2, 0, &["m9"]);
    let r = run(&h.text(), d);
    assert!(r["no-phantom"].1.contains("nobody sent it"), "{r:?}");
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "fail");
    recv(&mut h, 2, 4, &["m1"]);
    assert!(run(&h.text(), d)["no-phantom"]
        .1
        .contains("every send of it failed"));
    // an unknown send that is delivered is recovered, not a phantom
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "info");
    recv(&mut h, 2, 4, &["m1"]);
    assert_eq!(st(&run(&h.text(), d), "no-phantom"), Status::Held);
    // delivered before it was sent
    let mut h = H::default();
    recv(&mut h, 2, 0, &["m1"]);
    sent(&mut h, 1, 4, "m1", "ok");
    assert!(run(&h.text(), d)["no-phantom"]
        .1
        .contains("before any send"));
}

#[test]
fn at_most_once() {
    let d = r#"{"id":"c","type":"messages","history":"q","claims":{"delivery":"at-most-once"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1"]);
    recv(&mut h, 3, 4, &["m1"]);
    assert_eq!(st(&run(&h.text(), d), "at-most-once"), Status::Violated);
    // another group is another group
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1"]);
    h.rec(3, "invoke", 4, "recv", &["c", "g2"]);
    h.rec(3, "ok", 5, "recv", &["c", "g2", "m1"]);
    assert_eq!(st(&run(&h.text(), d), "at-most-once"), Status::Held);
}

#[test]
fn duplicates() {
    let forb = r#"{"id":"c","type":"messages","history":"q","claims":{"duplicates":"forbidden"}}"#;
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;o=5"]);
    recv(&mut h, 2, 2, &["m1;o=5"]);
    recv(&mut h, 2, 4, &["m1;o=6"]);
    let r = run(&h.text(), forb);
    assert!(r["duplicates"].1.contains("system ids 5 and 6"), "{r:?}");
    // a redelivery of the same stored copy is fine
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;o=5"]);
    recv(&mut h, 2, 2, &["m1;o=5"]);
    recv(&mut h, 2, 4, &["m1;o=5"]);
    assert_eq!(st(&run(&h.text(), forb), "duplicates"), Status::Held);
    // no system ids: undecided
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1"]);
    assert_eq!(st(&run(&h.text(), forb), "duplicates"), Status::Undecided);

    // a deduplication window by id: two sends within it stored twice
    let win = r#"{"id":"c","type":"messages","history":"q","claims":{"duplicates":{"by":"dedup-id","window_ms":1000}}}"#;
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1;x=d1"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;x=d1;o=1"]);
    h.rec(1, "invoke", 2, "send", &["c", "m2;x=d1"]);
    h.rec(1, "ok", 3, "send", &["c", "m2;x=d1;o=2"]);
    assert_eq!(st(&run(&h.text(), win), "duplicates"), Status::Violated);
    // outside the window it is allowed
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1;x=d1"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;x=d1;o=1"]);
    h.rec(1, "invoke", 5000, "send", &["c", "m2;x=d1"]);
    h.rec(1, "ok", 5001, "send", &["c", "m2;x=d1;o=2"]);
    assert_eq!(st(&run(&h.text(), win), "duplicates"), Status::Held);
    // two messages with one deduplication id both delivered (no system ids)
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1;x=d1"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;x=d1"]);
    h.rec(1, "invoke", 2, "send", &["c", "m2;x=d1"]);
    h.rec(1, "info", 3, "send", &["c", "m2;x=d1"]);
    recv(&mut h, 2, 4, &["m1", "m2"]);
    let r = run(&h.text(), win);
    assert_eq!(st(&r, "duplicates"), Status::Undecided, "{r:?}");
    let mut h2 = h.clone();
    h2.recs[3].ty = "ok";
    let r = run(&h2.text(), win);
    assert!(r["duplicates"].1.contains("were both delivered"), "{r:?}");
}

#[test]
fn after_ack() {
    let d = r#"{"id":"c","type":"messages","history":"q","claims":{"after_ack":"no-redelivery"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1;h=a"]);
    ack(&mut h, 2, 4, "m1;h=a", "ok");
    recv(&mut h, 3, 6, &["m1;h=b"]);
    let r = run(&h.text(), d);
    assert!(
        r["after-ack"]
            .1
            .contains("after process 2's acknowledgement"),
        "{r:?}"
    );
    // an unknown acknowledgement allows it
    let mut h2 = H::default();
    sent(&mut h2, 1, 0, "m1", "ok");
    recv(&mut h2, 2, 2, &["m1;h=a"]);
    ack(&mut h2, 2, 4, "m1;h=a", "info");
    recv(&mut h2, 3, 6, &["m1;h=b"]);
    assert_eq!(st(&run(&h2.text(), d), "after-ack"), Status::Held);
    // a send of the message after the acknowledgement excuses it ...
    let mut h3 = h.clone();
    h3.recs.insert(6, h.recs[0].clone());
    h3.recs.insert(7, h.recs[1].clone());
    h3.recs[6].p = "4".into();
    h3.recs[7].p = "4".into();
    assert_eq!(st(&run(&h3.text(), d), "after-ack"), Status::Held);
    // ... unless the redelivery carries the acknowledged delivery's system id
    let mut h4 = H::default();
    sent(&mut h4, 1, 0, "m1", "ok");
    recv(&mut h4, 2, 2, &["m1;h=a;o=7"]);
    ack(&mut h4, 2, 4, "m1;h=a", "ok");
    sent(&mut h4, 4, 5, "m1", "info");
    recv(&mut h4, 3, 7, &["m1;h=b;o=7"]);
    assert_eq!(st(&run(&h4.text(), d), "after-ack"), Status::Violated);
}

#[test]
fn processing() {
    let d = r#"{"id":"c","type":"messages","history":"q","claims":{"processing":"once"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1;h=a"]);
    recv(&mut h, 3, 4, &["m1;h=b"]);
    ack(&mut h, 2, 6, "m1;h=a", "ok");
    ack(&mut h, 3, 8, "m1;h=b", "ok");
    h.rec(9, "ok", 10, "drained", &["c", "-"]);
    let r = run(&h.text(), d);
    assert!(r["processing"].1.contains("processed twice"), "{r:?}");
    // the system refusing the stale acknowledgement keeps the claim
    let mut h2 = h.clone();
    h2.recs[7].ty = "fail";
    assert_eq!(st(&run(&h2.text(), d), "processing"), Status::Held);
    // delivered, never acknowledged, drained
    let mut h3 = H::default();
    sent(&mut h3, 1, 0, "m1", "ok");
    recv(&mut h3, 2, 2, &["m1;h=a"]);
    h3.rec(9, "ok", 10, "drained", &["c", "-"]);
    assert!(run(&h3.text(), d)["processing"]
        .1
        .contains("never acknowledged"));
}

#[test]
fn lease() {
    let d = r#"{"id":"c","type":"messages","history":"q","claims":{"lease":{"ms":30000}}}"#;
    // m41 delivered to process 7 at 12.410 s while process 3's lease from 12.002 s holds
    let mut h = H::default();
    sent(&mut h, 1, 0, "m41", "ok");
    recv(&mut h, 3, 12002, &["m41;h=a"]);
    recv(&mut h, 7, 12409, &["m41;h=b"]);
    let r = run(&h.text(), d);
    assert!(
        r["lease"].1.contains("m41 was delivered to process 7 at 12.410 s while process 3's lease from 12.002 s held until at least 42.002 s"),
        "{r:?}"
    );
    // after the lease
    let mut h = H::default();
    sent(&mut h, 1, 0, "m41", "ok");
    recv(&mut h, 3, 12002, &["m41;h=a"]);
    recv(&mut h, 7, 42100, &["m41;h=b"]);
    assert_eq!(st(&run(&h.text(), d), "lease"), Status::Held);
    // given back by a nack
    let mut h = H::default();
    sent(&mut h, 1, 0, "m41", "ok");
    recv(&mut h, 3, 12002, &["m41;h=a"]);
    h.rec(3, "invoke", 12100, "nack", &["c", "-", "m41;h=a"]);
    h.rec(3, "info", 12101, "nack", &["c", "-", "m41;h=a"]);
    recv(&mut h, 7, 12409, &["m41;h=b"]);
    assert_eq!(st(&run(&h.text(), d), "lease"), Status::Held);
    // an extension accepted while the lease held makes it longer
    let mut h = H::default();
    sent(&mut h, 1, 0, "m41", "ok");
    recv(&mut h, 3, 0, &["m41;h=a"]);
    h.rec(3, "invoke", 20000, "extend", &["c", "-", "m41;h=a;v=30000"]);
    h.rec(3, "ok", 20001, "extend", &["c", "-", "m41;h=a;v=30000"]);
    recv(&mut h, 7, 40000, &["m41;h=b"]);
    assert_eq!(st(&run(&h.text(), d), "lease"), Status::Violated);
    // a clock step inside the interval: undecided by default, judged with strict
    let mut h = H::default();
    sent(&mut h, 1, 0, "m41", "ok");
    recv(&mut h, 3, 12002, &["m41;h=a"]);
    let mut text = h.text();
    text.push_str("HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.clock_fwd\",\"ok\":true,\"msg\":\"the realtime clock was stepped forwards\"}\n");
    let mut h2 = H::default();
    recv(&mut h2, 7, 12409, &["m41;h=b"]);
    text.push_str(&h2.text());
    assert_eq!(st(&run(&text, d), "lease"), Status::Held);
    let strict = r#"{"id":"c","type":"messages","history":"q","clock_steps":"strict","claims":{"lease":{"ms":30000}}}"#;
    assert_eq!(st(&run(&text, strict), "lease"), Status::Violated);
    // per-delivery leases need v=
    let pd =
        r#"{"id":"c","type":"messages","history":"q","claims":{"lease":{"ms":"per-delivery"}}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m41", "ok");
    recv(&mut h, 3, 12002, &["m41;h=a"]);
    recv(&mut h, 7, 12409, &["m41;h=b"]);
    assert_eq!(st(&run(&h.text(), pd), "lease"), Status::Undecided);
    let mut h = H::default();
    sent(&mut h, 1, 0, "m41", "ok");
    recv(&mut h, 3, 12002, &["m41;h=a;v=1000"]);
    recv(&mut h, 7, 12409, &["m41;h=b;v=1000"]);
    assert_eq!(st(&run(&h.text(), pd), "lease"), Status::Violated);
}

#[test]
fn order_fifo_and_blocking() {
    let fifo = r#"{"id":"c","type":"messages","history":"q","claims":{"order":"fifo"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;k=a", "ok");
    sent(&mut h, 1, 2, "m2;k=a", "ok");
    recv(&mut h, 2, 4, &["m2;k=a"]);
    recv(&mut h, 2, 6, &["m1;k=a"]);
    let r = run(&h.text(), fifo);
    assert!(r["order"].1.starts_with("m2 was first delivered"), "{r:?}");
    // another key is another scope
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;k=a", "ok");
    sent(&mut h, 1, 2, "m2;k=b", "ok");
    recv(&mut h, 2, 4, &["m2;k=b"]);
    recv(&mut h, 2, 6, &["m1;k=a"]);
    assert_eq!(st(&run(&h.text(), fifo), "order"), Status::Held);
    // concurrent sends are not ordered
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1;k=a"]);
    h.rec(3, "invoke", 1, "send", &["c", "m2;k=a"]);
    h.rec(1, "ok", 2, "send", &["c", "m1;k=a"]);
    h.rec(3, "ok", 3, "send", &["c", "m2;k=a"]);
    recv(&mut h, 2, 4, &["m2;k=a"]);
    recv(&mut h, 2, 6, &["m1;k=a"]);
    assert_eq!(st(&run(&h.text(), fifo), "order"), Status::Held);
    // one receive returning them reversed
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;k=a", "ok");
    sent(&mut h, 1, 2, "m2;k=a", "ok");
    recv(&mut h, 2, 4, &["m2;k=a", "m1;k=a"]);
    assert_eq!(st(&run(&h.text(), fifo), "order"), Status::Violated);
    // one ordered batch
    let ob = r#"{"id":"c","type":"messages","history":"q","claims":{"order":{"kind":"fifo","batch":"ordered"}}}"#;
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1;k=a", "m2;k=a"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;k=a", "m2;k=a"]);
    recv(&mut h, 2, 4, &["m2;k=a"]);
    recv(&mut h, 2, 6, &["m1;k=a"]);
    assert_eq!(st(&run(&h.text(), ob), "order"), Status::Violated);
    assert_eq!(st(&run(&h.text(), fifo), "order"), Status::Held);
    // an unknown receive before may have delivered m1 unseen: undecided, not a violation
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;k=a", "ok");
    sent(&mut h, 1, 2, "m2;k=a", "ok");
    h.rec(5, "invoke", 3, "recv", &["c", "-"]);
    recv(&mut h, 2, 4, &["m2;k=a"]);
    recv(&mut h, 2, 6, &["m1;k=a"]);
    assert_eq!(st(&run(&h.text(), fifo), "order"), Status::Held);

    let blk = r#"{"id":"c","type":"messages","history":"q","claims":{"order":"fifo-blocking"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;k=a", "ok");
    sent(&mut h, 1, 2, "m2;k=a", "ok");
    recv(&mut h, 2, 4, &["m1;k=a;h=x"]);
    recv(&mut h, 3, 6, &["m2;k=a;h=y"]);
    ack(&mut h, 2, 8, "m1;k=a;h=x", "ok");
    let r = run(&h.text(), blk);
    assert!(
        r["order"]
            .1
            .contains("before the earlier message was acknowledged"),
        "{r:?}"
    );
    assert_eq!(st(&run(&h.text(), fifo), "order"), Status::Held);
}

#[test]
fn order_log_and_causal() {
    let log = r#"{"id":"c","type":"messages","history":"q","claims":{"order":"log"}}"#;
    let mut h = H::default();
    for (i, t) in [(1, 0), (2, 2), (3, 4)] {
        h.rec(1, "invoke", t, "send", &["c", &format!("m{i};k=a")]);
        h.rec(1, "ok", t + 1, "send", &["c", &format!("m{i};k=a;o={i}")]);
    }
    recv(&mut h, 2, 10, &["m1;k=a;o=1"]);
    recv(&mut h, 2, 12, &["m3;k=a;o=3"]);
    let r = run(&h.text(), log);
    assert!(r["order"].1.contains("skipping offset 2"), "{r:?}");
    // a consumer that goes back
    let mut h2 = h.clone();
    h2.recs.truncate(6);
    recv(&mut h2, 2, 10, &["m2;k=a;o=2"]);
    recv(&mut h2, 2, 12, &["m1;k=a;o=1"]);
    assert!(run(&h2.text(), log)["order"].1.contains("nonmonotonic"));
    // ... unless it subscribed again (a rebalance)
    let mut h3 = h2.clone();
    h3.recs.insert(
        8,
        Rec {
            p: "2".into(),
            ty: "invoke",
            t_ms: 11,
            f: "sub".into(),
            args: vec!["c".into(), "-".into()],
        },
    );
    h3.recs.insert(
        9,
        Rec {
            p: "2".into(),
            ty: "ok",
            t_ms: 11,
            f: "sub".into(),
            args: vec!["c".into(), "-".into()],
        },
    );
    assert_eq!(st(&run(&h3.text(), log), "order"), Status::Held);
    // two messages at one offset
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1;k=a"]);
    h.rec(1, "ok", 1, "send", &["c", "m1;k=a;o=1"]);
    h.rec(1, "invoke", 2, "send", &["c", "m2;k=a"]);
    h.rec(1, "ok", 3, "send", &["c", "m2;k=a;o=1"]);
    assert!(run(&h.text(), log)["order"].1.contains("share offset 1"));
    // no offsets: undecided
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;k=a", "ok");
    recv(&mut h, 2, 4, &["m1;k=a"]);
    assert_eq!(st(&run(&h.text(), log), "order"), Status::Undecided);

    let causal = r#"{"id":"c","type":"messages","history":"q","claims":{"order":{"kind":"causal","scope":"channel"}}}"#;
    // process 2 receives m1, then sends m2; group g delivers m2 before m1
    let mut h = H::default();
    h.rec(1, "invoke", 0, "send", &["c", "m1"]);
    h.rec(1, "info", 1, "send", &["c", "m1"]);
    recv(&mut h, 2, 2, &["m1"]);
    sent(&mut h, 2, 4, "m2", "ok");
    h.rec(3, "invoke", 6, "recv", &["c", "g"]);
    h.rec(3, "ok", 7, "recv", &["c", "g", "m2"]);
    h.rec(3, "invoke", 8, "recv", &["c", "g"]);
    h.rec(3, "ok", 9, "recv", &["c", "g", "m1"]);
    let r = run(&h.text(), causal);
    assert!(r["order"].1.contains("causally precedes"), "{r:?}");
    // fifo does not see it: m1's send never completed
    assert_eq!(
        st(
            &run(
                &h.text(),
                r#"{"id":"c","type":"messages","history":"q","claims":{"order":{"kind":"fifo","scope":"channel"}}}"#
            ),
            "order"
        ),
        Status::Held
    );
}

#[test]
fn dead_letter_and_delay() {
    let dl = r#"{"id":"c","type":"messages","history":"q","claims":{"dead_letter":{"after":2,"to":"dlq"}}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1;n=1"]);
    recv(&mut h, 2, 4, &["m1;n=2"]);
    recv(&mut h, 2, 6, &["m1;n=3"]);
    assert!(run(&h.text(), dl)["dead-letter"]
        .1
        .contains("more than the limit of 2"));
    // the attempt count going down
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1;n=2"]);
    recv(&mut h, 2, 4, &["m1;n=1"]);
    assert!(run(&h.text(), dl)["dead-letter"]
        .1
        .contains("attempt count"));
    // moved, then delivered from its channel again
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1;n=1"]);
    recv(&mut h, 2, 4, &["m1;n=2"]);
    h.rec(5, "invoke", 6, "recv", &["dlq", "-"]);
    h.rec(5, "ok", 7, "recv", &["dlq", "-", "m1"]);
    let ok_text = h.text();
    assert_eq!(st(&run(&ok_text, dl), "dead-letter"), Status::Held);
    // a delivery after the move (an acknowledgement in between keeps it within the limit)
    let mut h2 = h.clone();
    ack(&mut h2, 2, 8, "m1", "ok");
    recv(&mut h2, 2, 10, &["m1;n=3"]);
    let r = run(&h2.text(), dl);
    assert!(
        r["dead-letter"]
            .1
            .contains("after it appeared in the dead-letter channel"),
        "{r:?}"
    );

    let delay = r#"{"id":"c","type":"messages","history":"q","claims":{"delay":"per-message"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;d=5000", "ok");
    recv(&mut h, 2, 4000, &["m1"]);
    assert!(run(&h.text(), delay)["delay"]
        .1
        .contains("before its delay ended"));
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1;d=5000", "ok");
    recv(&mut h, 2, 5000, &["m1"]);
    assert_eq!(st(&run(&h.text(), delay), "delay"), Status::Held);
}

#[test]
fn fan_out_and_members() {
    let fo = r#"{"id":"c","type":"messages","history":"q","claims":{"delivery":"at-least-once","fan_out":"every-group"}}"#;
    let mut h = H::default();
    for g in ["g1", "g2"] {
        h.rec(5, "invoke", 0, "sub", &["c", g, "from=start"]);
        h.rec(5, "ok", 0, "sub", &["c", g, "from=start"]);
    }
    sent(&mut h, 1, 1, "m1", "ok");
    h.rec(2, "invoke", 3, "recv", &["c", "g1"]);
    h.rec(2, "ok", 4, "recv", &["c", "g1", "m1"]);
    h.rec(9, "ok", 10, "drained", &["c", "g1"]);
    h.rec(9, "ok", 11, "drained", &["c", "g2"]);
    let r = run(&h.text(), fo);
    assert_eq!(violated(&r), set(&["fan-out"]), "{r:?}");
    // a group that joined after the send (from=now) need not receive it
    let mut h = H::default();
    sent(&mut h, 1, 1, "m1", "ok");
    h.rec(5, "invoke", 2, "sub", &["c", "g1"]);
    h.rec(5, "ok", 2, "sub", &["c", "g1"]);
    h.rec(9, "ok", 11, "drained", &["c", "g1"]);
    let r = run(&h.text(), fo);
    assert_eq!(st(&r, "fan-out"), Status::Undecided, "{r:?}");

    let mem = r#"{"id":"c","type":"messages","history":"q","claims":{"members":"exclusive"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &["m1;h=a"]);
    recv(&mut h, 3, 4, &["m1;h=b"]);
    ack(&mut h, 2, 6, "m1;h=a", "ok");
    ack(&mut h, 3, 8, "m1;h=b", "ok");
    assert!(run(&h.text(), mem)["members"].1.contains("two members"));
}

#[test]
fn linearizable_queues() {
    let lin = r#"{"id":"c","type":"messages","history":"q","acknowledgement":"on-receive","claims":{"linearizable":"fifo"}}"#;
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    sent(&mut h, 1, 2, "m2", "ok");
    recv(&mut h, 2, 4, &["m2"]);
    recv(&mut h, 2, 6, &["m1"]);
    let r = run(&h.text(), lin);
    assert!(r["linearizable"].1.contains("(ordered)"), "{r:?}");
    let un = r#"{"id":"c","type":"messages","history":"q","acknowledgement":"on-receive","claims":{"linearizable":"unordered"}}"#;
    assert_eq!(st(&run(&h.text(), un), "linearizable"), Status::Held);
    // an empty receive while m1 is surely in the queue
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    recv(&mut h, 2, 2, &[]);
    recv(&mut h, 2, 4, &["m1"]);
    assert!(run(&h.text(), un)["linearizable"].1.contains("(empty)"));
}

// ------------------------------------------------------------------ queue patterns against the search

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// A random small queue history: up to 4 values, each sent once (some unknown), receives of one
/// value or none, at random overlapping intervals.
fn small_history(r: &mut Rng) -> Vec<QOp> {
    let nv = 1 + r.below(4) as usize;
    let mut ops = Vec::new();
    let mut pos = 0usize;
    let mut next = || {
        pos += 1;
        pos
    };
    // invocation and completion positions drawn from one counter with random overlap
    let mut evs: Vec<(usize, usize)> = Vec::new(); // (op, 0 inv / 1 ret)
    for v in 0..nv {
        ops.push(QOp {
            inv: 0,
            ret: 0,
            kind: QKind::Enq(format!("v{v}")),
            op: 0,
        });
    }
    let nd = r.below(nv as u64 + 2) as usize;
    let vals: Vec<usize> = (0..nv).collect();
    for i in 0..nd {
        let k = if r.chance(30) || i >= vals.len() {
            QKind::Deq(vec![])
        } else {
            QKind::Deq(vec![format!(
                "v{}",
                vals[(i + r.below(nv as u64) as usize) % nv]
            )])
        };
        ops.push(QOp {
            inv: 0,
            ret: 0,
            kind: k,
            op: 0,
        });
    }
    for i in 0..ops.len() {
        evs.push((i, 0));
        evs.push((i, 1));
    }
    // a random order where each op's invocation comes before its completion
    let mut order: Vec<(usize, usize)> = Vec::new();
    let mut started = vec![false; ops.len()];
    let mut pending: Vec<usize> = (0..ops.len()).collect();
    let mut open: Vec<usize> = Vec::new();
    while !pending.is_empty() || !open.is_empty() {
        let start = !pending.is_empty() && (open.is_empty() || r.chance(50));
        if start {
            let i = pending.remove(r.below(pending.len() as u64) as usize);
            started[i] = true;
            order.push((i, 0));
            open.push(i);
        } else {
            let i = open.remove(r.below(open.len() as u64) as usize);
            order.push((i, 1));
        }
    }
    let _ = evs;
    for (i, e) in order {
        let p = next();
        if e == 0 {
            ops[i].inv = p;
        } else {
            ops[i].ret = p;
        }
    }
    // some enqueues with an unknown outcome
    for o in ops.iter_mut() {
        if matches!(o.kind, QKind::Enq(_)) && r.chance(15) {
            o.ret = usize::MAX;
        }
    }
    // values received twice are kept rare but possible
    ops
}

#[test]
fn queue_patterns_match_search() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut agree, mut bad) = (0, 0);
    let mut mismatches = Vec::new();
    for _ in 0..20_000 {
        let ops = small_history(&mut r);
        for fifo in [false, true] {
            let pat = patterns(&ops, fifo);
            let lin = search(&ops, fifo);
            if pat.is_some() != lin {
                agree += 1;
            } else {
                bad += 1;
                if mismatches.len() < 3 {
                    mismatches.push((fifo, ops.clone(), pat.clone(), lin));
                }
            }
            // soundness above all: a pattern is never reported for a linearizable history
            assert!(
                !(pat.is_some() && lin),
                "a pattern on a linearizable history: fifo {fifo} {ops:?} {pat:?}"
            );
        }
    }
    assert_eq!(bad, 0, "{agree} agree, {bad} not: {mismatches:#?}");
}

// ------------------------------------------------------------------ generated correct histories

/// A simulated queue server that keeps every claim of [`FULL`]: channel `q` with groups g1 and
/// g2 (both subscribed from the start), leases of 50 ms, stale acknowledgements refused,
/// first deliveries in send order per key, a dead-letter channel `dlq` after 3 deliveries in g1,
/// per-message delays on keys of their own, system ids (offsets) per message; producers send
/// batches of one key, consumers hold messages for random times (sometimes past the lease),
/// acknowledge or give back, and some answers are lost (unknown outcomes). The drains read
/// everything at the end.
const FULL: &str = r#"{"id":"c","type":"messages","history":"q","claims":{
  "delivery":"at-least-once","duplicates":"forbidden","after_ack":"no-redelivery",
  "processing":"once","lease":{"ms":50},"order":{"kind":"fifo","scope":"key","batch":"ordered"},
  "dead_letter":{"after":3,"to":"dlq"},"delay":"per-message","fan_out":"every-group",
  "members":"exclusive"}}"#;

const LEASE_MS: i64 = 50;

#[derive(Clone, Debug)]
struct SMsg {
    m: String,
    k: String,
    o: u64,
    d: i64,
    sent_at: i64,
    seq: u64,
}

#[derive(Clone, Debug, Default)]
struct GState {
    /// per message: deliveries, current lease (handle, until), acked, dead
    deliveries: BTreeMap<String, u64>,
    lease: BTreeMap<String, (String, i64)>,
    /// given back, to be delivered again
    ready: BTreeSet<String>,
    acked: BTreeSet<String>,
    dead: BTreeSet<String>,
    first_done: BTreeSet<String>,
}

struct Sim {
    r: Rng,
    h: H,
    t: i64,
    msgs: Vec<SMsg>,
    off: u64,
    hc: u64,
    g: BTreeMap<String, GState>,
    dlq: Vec<String>,
    dlq_got: BTreeSet<String>,
}

impl Sim {
    fn tick(&mut self) -> i64 {
        self.t += 1;
        self.t
    }

    fn send(&mut self, p: u64, n: usize) {
        let key = if self.r.chance(15) {
            format!("d{}", self.msgs.len())
        } else {
            format!("k{}", self.r.below(3))
        };
        let delay = if key.starts_with('d') {
            20 + self.r.below(60) as i64
        } else {
            0
        };
        let mut inv = Vec::new();
        let mut out = Vec::new();
        let base = self.msgs.len();
        let ids: Vec<String> = (0..n).map(|i| format!("m{}", base + i)).collect();
        for id in &ids {
            inv.push(if delay > 0 {
                format!("{id};k={key};d={delay}")
            } else {
                format!("{id};k={key}")
            });
        }
        let t0 = self.tick();
        let iv: Vec<&str> = inv.iter().map(String::as_str).collect();
        let mut args = vec!["q"];
        args.extend(&iv);
        self.h.rec(p, "invoke", t0, "send", &args);
        let outcome = if self.r.chance(8) {
            "info"
        } else if self.r.chance(4) {
            "fail"
        } else {
            "ok"
        };
        let applied = match outcome {
            "ok" => true,
            "fail" => false,
            _ => self.r.chance(50),
        };
        let t1 = self.tick();
        if applied {
            for id in &ids {
                self.off += 1;
                self.msgs.push(SMsg {
                    m: id.clone(),
                    k: key.clone(),
                    o: self.off,
                    d: delay,
                    sent_at: t0,
                    seq: self.off,
                });
                out.push(format!("{id};k={key};o={}", self.off));
            }
        }
        match outcome {
            "ok" => {
                let ov: Vec<&str> = out.iter().map(String::as_str).collect();
                let mut a = vec!["q"];
                a.extend(&ov);
                self.h.rec(p, "ok", t1, "send", &a);
            }
            "fail" => self.h.rec(p, "fail", t1, "send", &args),
            _ => self.h.rec(p, "info", t1, "send", &args),
        }
        if !applied {
            // the ids are spent: a later send must not reuse them
            for _ in &ids {
                self.msgs.push(SMsg {
                    m: String::new(),
                    k: String::new(),
                    o: 0,
                    d: 0,
                    sent_at: 0,
                    seq: 0,
                });
            }
        }
    }

    /// What a receive in group g returns at time now: expired leases first (redeliveries), then
    /// first deliveries in send order per key (a key waits while its earliest undelivered message
    /// is delayed), up to `max`.
    /// A message given back (its lease ended, or a nack): ready again, or dead after its third
    /// delivery.
    fn give_back(&mut self, g: &str, m: String) {
        let st = self.g.get_mut(g).expect("group");
        if st.deliveries.get(&m).copied().unwrap_or(0) >= 3 {
            st.dead.insert(m.clone());
            self.dlq.push(m);
        } else {
            st.ready.insert(m);
        }
    }

    fn pick(&mut self, g: &str, now: i64, max: usize) -> Vec<String> {
        let st = self.g.get_mut(g).expect("group");
        let expired: Vec<String> = st
            .lease
            .iter()
            .filter(|(_, (_, until))| *until <= now)
            .map(|(m, _)| m.clone())
            .collect();
        for m in expired {
            self.g.get_mut(g).expect("group").lease.remove(&m);
            self.give_back(g, m);
        }
        let st = self.g.get_mut(g).expect("group");
        let mut out: Vec<String> = Vec::new();
        while out.len() < max {
            let Some(m) = st.ready.iter().next().cloned() else {
                break;
            };
            st.ready.remove(&m);
            out.push(m);
        }
        let mut by_key: BTreeMap<&str, Vec<&SMsg>> = BTreeMap::new();
        for m in self.msgs.iter().filter(|m| !m.m.is_empty()) {
            by_key.entry(&m.k).or_default().push(m);
        }
        for ms in by_key.values() {
            for m in ms {
                if st.first_done.contains(&m.m) {
                    continue;
                }
                if out.len() >= max || m.sent_at + m.d > now {
                    break; // the key waits
                }
                out.push(m.m.clone());
            }
        }
        out
    }

    fn recv(&mut self, p: u64, g: &str) {
        let t0 = self.tick();
        self.h.rec(p, "invoke", t0, "recv", &["q", g]);
        let max = 1 + self.r.below(2) as usize;
        let got = self.pick(g, t0, max);
        // g2's consumer is careful: its answers are never lost and it never holds past a lease
        let careful = g == "g2";
        let lost = !careful && self.r.chance(5);
        let t1 = self.tick();
        let mut items = Vec::new();
        for m in &got {
            self.hc += 1;
            let h = format!("h{}", self.hc);
            let st = self.g.get_mut(g).expect("group");
            let n = st.deliveries.entry(m.clone()).or_default();
            *n += 1;
            let n = *n;
            st.first_done.insert(m.clone());
            st.lease.insert(m.clone(), (h.clone(), t0 + LEASE_MS));
            let sm = self.msgs.iter().find(|x| &x.m == m).expect("a message");
            items.push(format!(
                "{m};k={};o={};n={n};h={h};v={LEASE_MS}",
                sm.k, sm.o
            ));
        }
        if lost {
            self.h.rec(p, "info", t1, "recv", &["q", g]);
            return;
        }
        let iv: Vec<&str> = items.iter().map(String::as_str).collect();
        let mut a = vec!["q", g];
        a.extend(&iv);
        self.h.rec(p, "ok", t1, "recv", &a);
        // hold, then acknowledge (or give back) each
        for it in items {
            let hold = if !careful && self.r.chance(20) {
                LEASE_MS + 10 + self.r.below(40) as i64
            } else {
                self.r.below(20) as i64
            };
            self.t += hold;
            let m = it.split(';').next().unwrap().to_string();
            let h = it
                .split(";h=")
                .nth(1)
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string();
            let tok = format!("{m};h={h}");
            let nack = self.r.chance(10) && g == "g1";
            let f = if nack { "nack" } else { "ack" };
            let t0 = self.tick();
            self.h.rec(p, "invoke", t0, f, &["q", g, &tok]);
            let st = self.g.get_mut(g).expect("group");
            let current = st
                .lease
                .get(&m)
                .is_some_and(|(hh, until)| hh == &h && *until > t0);
            let t1 = self.t + 1;
            self.t = t1;
            if current {
                st.lease.remove(&m);
                if !nack {
                    st.acked.insert(m.clone());
                }
                let out = if self.r.chance(5) { "info" } else { "ok" };
                self.h.rec(p, out, t1, f, &["q", g, &tok]);
                if nack {
                    self.give_back(g, m);
                }
            } else {
                self.h.rec(p, "fail", t1, f, &["q", g, &tok]);
            }
        }
    }

    fn drain(&mut self, p: u64, g: &str) {
        for _ in 0..200 {
            self.t += LEASE_MS + 1;
            let before = self.h.recs.len();
            let t0 = self.tick();
            self.h.rec(p, "invoke", t0, "recv", &["q", g]);
            let got = self.pick(g, t0, 4);
            let t1 = self.tick();
            let mut items = Vec::new();
            for m in &got {
                self.hc += 1;
                let h = format!("h{}", self.hc);
                let st = self.g.get_mut(g).expect("group");
                let n = st.deliveries.entry(m.clone()).or_default();
                *n += 1;
                let n = *n;
                st.first_done.insert(m.clone());
                st.lease.insert(m.clone(), (h.clone(), t0 + LEASE_MS));
                let sm = self.msgs.iter().find(|x| &x.m == m).expect("a message");
                items.push((
                    m.clone(),
                    h.clone(),
                    format!("{m};k={};o={};n={n};h={h};v={LEASE_MS}", sm.k, sm.o),
                ));
            }
            let iv: Vec<&str> = items.iter().map(|x| x.2.as_str()).collect();
            let mut a = vec!["q", g];
            a.extend(&iv);
            self.h.rec(p, "ok", t1, "recv", &a);
            for (m, h, _) in items {
                let tok = format!("{m};h={h}");
                let t0 = self.tick();
                self.h.rec(p, "invoke", t0, "ack", &["q", g, &tok]);
                let st = self.g.get_mut(g).expect("group");
                st.lease.remove(&m);
                st.acked.insert(m.clone());
                let t1 = self.tick();
                self.h.rec(p, "ok", t1, "ack", &["q", g, &tok]);
            }
            let st = &self.g[g];
            let pending = self
                .msgs
                .iter()
                .any(|m| !m.m.is_empty() && !st.acked.contains(&m.m) && !st.dead.contains(&m.m));
            if !pending && self.h.recs.len() == before + 2 {
                let t = self.tick();
                self.h.rec(p, "ok", t, "drained", &["q", g]);
                return;
            }
        }
        panic!("the drain of {g} did not end");
    }

    fn drain_dlq(&mut self, p: u64) {
        let t0 = self.tick();
        self.h.rec(p, "invoke", t0, "recv", &["dlq", "-"]);
        let items: Vec<String> = self
            .dlq
            .iter()
            .filter(|m| !self.dlq_got.contains(*m))
            .cloned()
            .collect();
        for m in &items {
            self.dlq_got.insert(m.clone());
        }
        let t1 = self.tick();
        let iv: Vec<&str> = items.iter().map(String::as_str).collect();
        let mut a = vec!["dlq", "-"];
        a.extend(&iv);
        self.h.rec(p, "ok", t1, "recv", &a);
        let t = self.tick();
        self.h.rec(p, "ok", t, "drained", &["dlq", "-"]);
    }
}

fn simulate(seed: u64) -> H {
    let mut s = Sim {
        r: Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1),
        h: H::default(),
        t: 0,
        msgs: Vec::new(),
        off: 0,
        hc: 0,
        g: BTreeMap::new(),
        dlq: Vec::new(),
        dlq_got: BTreeSet::new(),
    };
    for g in ["g1", "g2"] {
        s.g.insert(g.into(), GState::default());
        let t = s.tick();
        s.h.rec(50, "invoke", t, "sub", &["q", g, "from=start"]);
        s.h.rec(50, "ok", t, "sub", &["q", g, "from=start"]);
    }
    let steps = 30 + s.r.below(40);
    for _ in 0..steps {
        match s.r.below(10) {
            0..=3 => {
                let p = 1 + s.r.below(3);
                let n = 1 + s.r.below(2) as usize;
                s.send(p, n);
            }
            4..=7 => {
                let p = 10 + s.r.below(3);
                s.recv(p, "g1");
            }
            _ => s.recv(20, "g2"),
        }
    }
    s.drain(90, "g1");
    s.drain(91, "g2");
    s.drain_dlq(92);
    let _ = s.msgs.iter().map(|m| m.seq).max();
    s.h
}

#[test]
fn generated_correct_histories_keep_every_claim() {
    for seed in 1..=300u64 {
        let h = simulate(seed);
        let r = run(&h.text(), FULL);
        let bad: Vec<_> = r.iter().filter(|(_, v)| v.0 != Status::Held).collect();
        assert!(bad.is_empty(), "seed {seed}: {bad:?}\n{}", h.text());
    }
}

/// Indices of records matching a predicate.
fn find(h: &H, f: impl Fn(&Rec) -> bool) -> Vec<usize> {
    (0..h.recs.len()).filter(|&i| f(&h.recs[i])).collect()
}

fn item_m(tok: &str) -> &str {
    tok.split(';').next().unwrap()
}

/// Each mutation: what it does to a generated history (None when the history has no place for
/// it) and the claims it must break, exactly.
type Mutation = (
    &'static str,
    fn(&H, &mut Rng) -> Option<H>,
    &'static [&'static str],
);

fn mutations() -> Vec<Mutation> {
    vec![
        // a delivery of a message nobody sent
        (
            "phantom",
            |h, _| {
                let i = *find(h, |r| r.ty == "ok" && r.f == "recv" && r.args[1] == "g2").first()?;
                let mut h = h.clone();
                h.recs[i].args.push("zz1;k=k0;o=999".into());
                Some(h)
            },
            &["no-phantom"],
        ),
        // every delivery of an acknowledged message in g2 removed (its acks too): lost in g2
        (
            "lose-in-one-group",
            |h, r| {
                let ms: Vec<String> = find(h, |x| x.ty == "ok" && x.f == "send")
                    .iter()
                    .flat_map(|&i| {
                        h.recs[i].args[1..]
                            .iter()
                            .map(|t| item_m(t).to_string())
                            .collect::<Vec<_>>()
                    })
                    .collect();
                if ms.is_empty() {
                    return None;
                }
                let m = ms[r.below(ms.len() as u64) as usize].clone();
                // a message in the dead-letter channel is excused in every group (the channel does
                // not say which group moved it)
                if !find(h, |x| {
                    x.f == "recv"
                        && x.args[0] == "dlq"
                        && x.args[2..].iter().any(|t| item_m(t) == m)
                })
                .is_empty()
                {
                    return None;
                }
                let mut h = h.clone();
                for rec in h.recs.iter_mut() {
                    if rec.args.first().map(String::as_str) == Some("q")
                        && rec.args.get(1).map(String::as_str) == Some("g2")
                        && rec.f != "send"
                    {
                        rec.args.retain(|t| item_m(t) != m || t == "q" || t == "g2");
                    }
                }
                // an ack with no item left is dropped with its invocation
                let mut out = H::default();
                for rec in &h.recs {
                    if rec.f == "ack" && rec.args.len() == 2 {
                        continue;
                    }
                    out.recs.push(rec.clone());
                }
                Some(out)
            },
            &["fan-out"],
        ),
        // a delivery whose system id differs from the send's
        (
            "second-copy",
            |h, r| {
                let is = find(h, |x| {
                    x.ty == "ok" && x.f == "recv" && x.args[0] == "q" && x.args.len() > 2
                });
                if is.is_empty() {
                    return None;
                }
                let i = is[r.below(is.len() as u64) as usize];
                let mut h = h.clone();
                let t = h.recs[i].args[2].clone();
                let o = t.split(";o=").nth(1)?.split(';').next()?.to_string();
                h.recs[i].args[2] = t.replace(&format!(";o={o};"), ";o=77777;");
                Some(h)
            },
            &["duplicates"],
        ),
        // a stale acknowledgement accepted: two accepted acknowledgements of one message
        (
            "stale-ack-accepted",
            |h, r| {
                let fails = find(h, |x| x.ty == "fail" && x.f == "ack" && x.args[1] == "g1");
                if fails.is_empty() {
                    return None;
                }
                let i = fails[r.below(fails.len() as u64) as usize];
                let m = item_m(&h.recs[i].args[2]).to_string();
                // it breaks processing only when the message has another accepted acknowledgement
                let other = find(h, |x| {
                    x.ty == "ok" && x.f == "ack" && x.args[1] == "g1" && item_m(&x.args[2]) == m
                });
                if other.is_empty() {
                    return None;
                }
                let mut h = h.clone();
                h.recs[i].ty = "ok";
                Some(h)
            },
            &["processing", "after-ack"],
        ),
        // a redelivery long after the message was acknowledged
        (
            "redeliver-after-ack",
            |h, r| {
                let acks = find(h, |x| x.ty == "ok" && x.f == "ack" && x.args[1] == "g2");
                if acks.is_empty() {
                    return None;
                }
                let i = acks[r.below(acks.len() as u64) as usize];
                let m = item_m(&h.recs[i].args[2]).to_string();
                // a message whose send's outcome was unknown may have been accepted late: excused
                let unknown = find(h, |x| {
                    x.f == "send" && x.ty == "info" && x.args[1..].iter().any(|t| item_m(t) == m)
                });
                if !unknown.is_empty() {
                    return None;
                }
                let mut h = h.clone();
                let t = h.recs[i].t_ms + 1;
                // the drain's last receive of g2 delivers it again, after its lease would have ended
                let j = *find(&h, |x| {
                    x.ty == "ok" && x.f == "recv" && x.args[1] == "g2" && x.p == "91"
                })
                .last()?;
                if h.recs[j].t_ms < t + LEASE_MS {
                    return None;
                }
                h.recs[j].args.push(format!("{m};k=k0;h=hx;v={LEASE_MS}"));
                Some(h)
            },
            &["after-ack"],
        ),
        // a redelivery inside the lease of a delivery that was neither acknowledged nor given back
        (
            "redeliver-in-lease",
            |h, _| {
                // a g1 delivery whose acknowledgement failed (held past the lease): move the next
                // delivery of the message into the lease
                for &i in &find(h, |x| x.ty == "fail" && x.f == "ack" && x.args[1] == "g1") {
                    let m = item_m(&h.recs[i].args[2]).to_string();
                    let handle = h.recs[i].args[2].split(";h=").nth(1)?.to_string();
                    let has = |x: &Rec, m: &str| x.args[2..].iter().any(|t| item_m(t) == m);
                    let ds = find(h, |x| {
                        x.ty == "ok" && x.f == "recv" && x.args[1] == "g1" && has(x, &m)
                    });
                    // the delivery the failed acknowledgement was for, and the next one
                    let Some(k) = ds.iter().position(|&d| {
                        h.recs[d].args[2..]
                            .iter()
                            .any(|t| item_m(t) == m && t.contains(&format!(";h={handle};")))
                    }) else {
                        continue;
                    };
                    if k + 1 >= ds.len() {
                        continue;
                    }
                    let (a, b) = (ds[k], ds[k + 1]);
                    if h.recs[b - 1].f != "recv" || h.recs[b - 1].ty != "invoke" {
                        continue;
                    }
                    // one item only, so that no other message's delay is affected
                    if h.recs[b].args.len() != 3 {
                        continue;
                    }
                    let mut h = h.clone();
                    let t = h.recs[a].t_ms + 5;
                    // the second receive keeps its place; its times move into the first one's lease
                    let shift = h.recs[b].t_ms - t;
                    if shift <= 0 {
                        continue;
                    }
                    h.recs[b - 1].t_ms -= shift;
                    h.recs[b].t_ms -= shift;
                    return Some(h);
                }
                None
            },
            &["lease", "members"],
        ),
        // a batch receive that returns two messages of one key reversed
        (
            "reverse-batch",
            |h, _| {
                // g2 never loses a receive's answer, so its first deliveries are all seen
                for i in find(h, |x| {
                    x.ty == "ok" && x.f == "recv" && x.args[1] == "g2" && x.args.len() >= 4
                }) {
                    let (a, b) = (&h.recs[i].args[2], &h.recs[i].args[3]);
                    let key = |t: &str| {
                        t.split(";k=")
                            .nth(1)
                            .map(|x| x.split(';').next().unwrap().to_string())
                    };
                    let n1 = |t: &str| t.contains(";n=1;");
                    // the earlier one's send must have completed (an unknown send orders nothing)
                    let m1 = item_m(a).to_string();
                    let unknown = !find(h, |x| {
                        x.f == "send"
                            && x.ty == "info"
                            && x.args[1..].iter().any(|t| item_m(t) == m1)
                    })
                    .is_empty();
                    if key(a) == key(b) && n1(a) && n1(b) && !unknown {
                        let mut h = h.clone();
                        h.recs[i].args.swap(2, 3);
                        return Some(h);
                    }
                }
                None
            },
            &["order"],
        ),
        // an attempt count that goes down
        (
            "attempts-down",
            |h, _| {
                let i = *find(h, |x| {
                    x.ty == "ok"
                        && x.f == "recv"
                        && x.args[1] == "g1"
                        && x.args[2..].iter().any(|t| t.contains(";n=2;"))
                })
                .first()?;
                let mut h = h.clone();
                for t in h.recs[i].args.iter_mut() {
                    if t.contains(";n=2;") {
                        *t = t.replace(";n=2;", ";n=0;");
                        break;
                    }
                }
                Some(h)
            },
            &["dead-letter"],
        ),
        // a delay that had not ended when the message was delivered
        (
            "early-delay",
            |h, _| {
                let i = *find(h, |x| {
                    x.ty == "invoke" && x.f == "send" && x.args.iter().any(|t| t.contains(";d="))
                })
                .first()?;
                let mut h = h.clone();
                for t in h.recs[i].args.iter_mut() {
                    if let Some((pre, _)) = t.split_once(";d=") {
                        *t = format!("{pre};d=999999999");
                    }
                }
                // only a message that was delivered at all breaks it
                Some(h)
            },
            &["delay"],
        ),
    ]
}

#[test]
fn mutations_break_exactly_their_claims() {
    let mut applied: BTreeMap<&str, u32> = BTreeMap::new();
    for seed in 1..=300u64 {
        let h = simulate(seed);
        let mut r = Rng(seed | 0x100);
        for (name, f, expect) in mutations() {
            let Some(m) = f(&h, &mut r) else { continue };
            let got = violated(&run(&m.text(), FULL));
            let mut want = set(expect);
            if name == "redeliver-in-lease" {
                // members breaks with lease only when the two holders are different processes
                if !got.contains("members") {
                    want.remove("members");
                }
            }
            if name == "stale-ack-accepted" && !got.contains("after-ack") {
                // the accepted stale ack may come after the other's: then nothing came after it
                want.remove("after-ack");
            }
            if name == "early-delay" && got.is_empty() {
                continue; // that message was never delivered
            }
            assert_eq!(got, want, "seed {seed}, mutation {name}\n{}", m.text());
            *applied.entry(name).or_default() += 1;
        }
    }
    for (name, _, _) in mutations() {
        assert!(
            applied.get(name).copied().unwrap_or(0) >= 5,
            "{name} applied {applied:?}"
        );
    }
}

#[test]
fn run_checks_reports_one_property_per_claim() {
    let mut h = H::default();
    sent(&mut h, 1, 0, "m1", "ok");
    sent(&mut h, 1, 2, "m2", "ok");
    recv(&mut h, 2, 4, &["m1"]);
    h.rec(9, "ok", 10, "drained", &["c", "-"]);
    let mut text = String::from(
        "HYDO-CHECK {\"claims\":{\"delivery\":\"at-least-once\"},\"history\":\"q\",\"id\":\"jobs\",\"msg\":\"jobs\",\"type\":\"messages\"}\n",
    );
    text.push_str(&h.text());
    let r =
        hydo_history::run_checks(&[("w", text)], None, hydo_history::DEFAULT_BUDGET, None).unwrap();
    let ids: Vec<&str> = r.checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["jobs.no-phantom", "jobs.no-loss"]);
    let v = r.verdict.unwrap();
    assert!(
        v.starts_with("HYDO-ORACLE: VIOLATION [jobs.no-loss] m2 was acknowledged"),
        "{v}"
    );
    assert_eq!(r.messages.len(), 1);
    let j = json_loads(&r.messages[0]).unwrap();
    assert!(
        j.get("measures").and_then(|m| m.get("lost")).is_some(),
        "{}",
        r.messages[0]
    );
    // a claim key this hydo does not know: an ERROR naming it, never a pass
    let text = "HYDO-CHECK {\"claims\":{\"retention\":\"forever\"},\"history\":\"q\",\"id\":\"jobs\",\"type\":\"messages\"}\n";
    let r =
        hydo_history::run_checks(&[("w", text)], None, hydo_history::DEFAULT_BUDGET, None).unwrap();
    let v = r.verdict.unwrap();
    assert!(
        v.contains("ERROR check jobs") && v.contains("retention"),
        "{v}"
    );
}
