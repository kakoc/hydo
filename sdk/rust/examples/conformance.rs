//! The Rust half of the conformance test (tests/conformance.sh): the same SDK calls as
//! tests/go-conformance/main.go, in the same order, through the run-time API (the one the Go
//! SDK has). The consoles and stdouts of the two programs must be byte-identical.
use hydo_assert::{self as ha, details, Details, Value};
use std::time::Duration;

fn rich() -> Details {
    details! {
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
        "nil": Value::Null,
        "list": Value::List(vec![1.into(), "a".into(), false.into(), Value::Null, details!{"z": 1, "a": 2}.into()]),
        "empty": Value::List(vec![]),
        "emap": Details::new(),
        "nest": details!{"é": 1, "B": 2, "a": 3, "_": 4, "a b": details!{"x": "y"}},
    }
}

fn main() {
    let a = ha::always("c.always", "always <holds> & \"quotes\"");
    let b = ha::always_or_unreachable("c.aou", "always or unreachable");
    let s = ha::sometimes("c.sometimes", "sometimes é \u{2028} true");
    let r = ha::reachable("c.reach", "reached");
    let u = ha::unreachable("c.unreach", "never reached");
    ha::always("c.never", "never evaluated");
    let cmp = ha::always("c.cmp", "a comparison");
    let scmp = ha::sometimes("c.scmp", "a sometimes comparison");
    let nan = ha::always("c.nan", "details with a NaN");
    let l1 = ha::eventually("c.live_ok", "in time", Duration::from_secs(10));
    let l2 = ha::eventually("c.live_late", "never", Duration::ZERO);
    let ps: Vec<&'static ha::Point> = ["p.a", "p.b", "p.c", "p.d", "p.e", "p.f", "p.g", "p.h"]
        .iter()
        .map(|id| ha::buggify(*id, format!("point {}", id.to_uppercase())))
        .collect();

    ha::catalog();

    a.check(true, ());
    a.check(true, details! {"x": 1});
    a.check(false, rich());
    a.check(false, details! {"dup": true});
    b.check(true, Details::new());
    s.check(false, ());
    s.check(true, details! {"n": 3});
    r.hit(());
    r.hit(());
    u.hit(details! {"why": "test"});
    cmp.cmp(3i64, "<=", 5, ());
    cmp.cmp(7i64, "<", 5, details! {"extra": "e"});
    cmp.cmp(1i64, "??", 1, ());
    scmp.cmp(1i64, ">", 2, ());
    scmp.cmp(2i64, "==", 2, ());
    nan.check(true, details! {"f": f64::NAN});
    nan.check(true, details! {"f": 1});
    nan.check(false, details! {"f": f64::INFINITY});
    let mut w = l1.begin();
    w.done(());
    w.done(());
    l1.await_cond(|| true, Duration::from_millis(1), ());
    let mut w2 = l2.begin();
    w2.expire(details! {"k": "v"});
    ha::setup_complete();

    let mut fires = String::new();
    for i in 0..50 {
        for (j, p) in ps.iter().enumerate() {
            let d = if j == 0 { details! {"round": i} } else { Details::new() };
            fires.push(if p.fire(d) { '1' } else { '0' });
        }
    }
    println!("{}", fires);
}
