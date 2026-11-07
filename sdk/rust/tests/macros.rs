//! The Rust-only surface: call-site macros (file/line, registered before main), declare!,
//! catalog order, the async eventually, and the noop feature. One test function,
//! because the SDK's state (sink, seen flags, BUGGIFY configuration) is per process.
use hydo_assert::{declare, details, hydo_always, hydo_buggify, hydo_eventually, hydo_reachable, hydo_sometimes, hydo_unreachable, Value};
use std::time::Duration;

declare! {
    static ACKED: always("m.acked", "every acknowledged write reads back");
    static RECOVERED: sometimes("m.recovered", "a non-empty log was replayed");
    static BACK: eventually("m.back", "answers within 5 s", Duration::from_secs(5));
    static LOST: buggify("m.lost_reply", "the reply is lost");
    static NEVER: unreachable("m.never", "never reached");
}

fn never_called() {
    // never evaluated: still in the catalog (registered before main)
    hydo_always!(false, "m.dead" => "a property in code that never runs");
}

#[tokio::test]
async fn macros_and_catalog() {
    let dir = std::env::temp_dir().join(format!("hydo-assert-macros-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("console");
    std::fs::write(&out, b"").unwrap();
    std::env::set_var("HYDO_ASSERT_OUT", &out);
    std::env::set_var("HYDO_BUGGIFY", "5,1000,1000"); // every point enabled, every evaluation fires
    let _ = never_called;

    hydo_assert::catalog();
    assert!(ACKED.check(true, ()));
    assert!(!ACKED.check(false, details! {"key": "k:1", "got": Value::Null}));
    RECOVERED.check(true, details! {"records": 3});
    for i in 0..3 {
        assert!(hydo_always!(i < 10, "m.small" => "i stays small", {"i": i}));
    }
    hydo_sometimes!(true, "plain message is the id");
    hydo_reachable!("m.reached" => "reached a line");
    hydo_unreachable!("m.bad" => "a line that must not run", {"why": "test"});
    assert!(hydo_assert::hydo_always_cmp!(5u64, ">=", 3u64, "m.ge" => "five is at least three"));
    let fired = LOST.fire(());
    let fired2 = hydo_buggify!("m.site_point" => "a call-site fault point");
    assert_eq!((fired, fired2), (!cfg!(feature = "noop"), !cfg!(feature = "noop")));
    let live = hydo_eventually!(Duration::from_secs(1), "m.async" => "an async condition comes true");
    let mut n = 0;
    let ok = live
        .await_async(|| { n += 1; let k = n; async move { k >= 3 } }, Duration::from_millis(1), |d| tokio::time::sleep(d), ())
        .await;
    assert!(ok);
    let mut w = BACK.begin();
    assert!(w.done(()));
    NEVER.hit(());
    hydo_assert::setup_complete();

    let text = std::fs::read_to_string(&out).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let lines: Vec<&str> = text.lines().collect();
    if cfg!(feature = "noop") {
        assert!(lines.is_empty(), "noop wrote {:?}", lines);
        return;
    }
    let decls: Vec<&str> = lines.iter().filter(|l| l.starts_with("HYDO-ASSERT-DECL ")).copied().collect();
    let ids: Vec<String> = decls.iter().map(|l| l.split("\"id\":\"").nth(1).unwrap().split('"').next().unwrap().to_string()).collect();
    // declare! entries in the order written (one block, one line), then the call sites by line
    assert_eq!(
        ids,
        ["m.acked", "m.recovered", "m.back", "buggify.m.lost_reply", "m.never", "m.dead", "m.small",
         "plain message is the id", "m.reached", "m.bad", "m.ge", "buggify.m.site_point", "m.async"],
        "{}", text
    );
    assert!(decls[0].ends_with(r#""msg":"every acknowledged write reads back"}"#), "{}", decls[0]);
    assert!(decls[5].contains(r#","loc":"tests/macros.rs:"#), "{}", decls[5]);
    let has = |s: &str| lines.iter().any(|l| l.contains(s));
    assert!(has(r#"HYDO-ASSERT {"t":"always","id":"m.acked","ok":false,"msg":"every acknowledged write reads back","details":{"got":null,"key":"k:1"}}"#), "{}", text);
    assert_eq!(lines.iter().filter(|l| l.contains(r#""id":"m.small""#) && l.starts_with("HYDO-ASSERT {")).count(), 1);
    assert!(has(r#""id":"m.small","ok":true,"msg":"i stays small","details":{"i":0},"loc":"tests/macros.rs:"#), "{}", text);
    assert!(has(r#""details":{"left":5,"op":"\u003e=","right":3}"#), "{}", text);
    assert!(has(r#"HYDO-BUGGIFY {"seed":"5","enable_pm":1000,"fire_pm":1000,"enabled":["m.lost_reply","m.site_point"]}"#), "{}", text);
    assert!(has(r#""id":"m.async","ok":true"#) && has(r#""within_ms":1000"#), "{}", text);
    assert!(has(r#"HYDO-ASSERT {"t":"unreachable","id":"m.never","ok":false"#), "{}", text);
    assert_eq!(*lines.last().unwrap(), "HYDO-SETUP-COMPLETE");
}
