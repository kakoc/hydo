// The messaging recorder writes exactly the lines of the shared case sdk/cases/history/messages
// (which the other SDKs replay with their generic recorders).
#[test]
#[cfg(not(feature = "noop"))]
fn messages_records_the_shared_case() {
    use hydo_assert::history::messages::{Item, Messages, Outcome};
    let p = std::env::temp_dir().join(format!("hydo-hist-msg-test-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    // its own test program: the recorder reads HYDO_HIST_OUT once per process
    std::env::set_var("HYDO_HIST_OUT", &p);
    let q = Messages::new("q");
    let m17 = || Item::new("m17").key("g2");
    let m18 = || Item::new("m18").key("g2");
    q.send(3, "orders", &[m17(), m18()]).ok(&[m17().id(81), m18().id(82)]);
    let m19 = || Item::new("m19").key("g;2").dedup("d=1").delay_ms(5000);
    q.send(3, "orders", &[m19()]).info();
    q.resend(3, "orders", &[m19()]).ok(&[m19().id("83").outcome(Outcome::Ok)]);
    q.send(4, "orders", &[Item::new("m 20")]).fail();
    q.recv(5, "orders", "-").ok(&[m17().id(81).attempt(1).handle("a91f").lease_ms(30000)]);
    q.recv(5, "orders", "-").ok(&[]);
    q.ack(5, "orders", "-", &[Item::new("m17").handle("a91f")]).ok_same();
    q.nack(6, "orders", "-", &[Item::new("m18").handle("b2").reject()]).info();
    q.extend(7, "orders", "-", &[Item::new("m19").handle("c3").lease_ms(60000)]).ok_same();
    q.commit(8, "events", "g1", &[("k1", 41), ("k 2", 7)]).ok_same();
    q.sub(8, "events", "g1", true).ok_same();
    q.sub(8, "events", "g2", false).fail();
    q.drained(9, "orders", "-");
    let file = std::env::var_os("HYDO_HIST_OUT").unwrap();
    let got = std::fs::read_to_string(&file).unwrap_or_default();
    let norm = |t: &str| -> Vec<String> {
        t.lines()
            .filter(|l| l.starts_with("HYDO-HIST q "))
            .map(|l| {
                let mut f: Vec<&str> = l.split(' ').collect();
                f[4] = "*";
                f.join(" ")
            })
            .collect()
    };
    let case = concat!(env!("CARGO_MANIFEST_DIR"), "/../cases/history/messages.out");
    let want = std::fs::read_to_string(case).expect("the shared case (HYDO_BLESS=1 cargo test --test cases writes it)");
    assert_eq!(norm(&got), norm(&want));
    // a second send of one id for another intent is refused
    let r = std::panic::catch_unwind(|| {
        let q = Messages::new("q2");
        q.send(1, "c", &[Item::new("m1")]).ok_same();
        q.send(1, "c", &[Item::new("m1")]).ok_same();
    });
    assert!(r.is_err());
    let _ = std::fs::remove_file(&p);
}
