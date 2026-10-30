// The history recorder writes the documented lines (hydo/HISTORIES.md), the same as the Go recorder.
use hydo_assert::history::{append_mop, enc, list, read_mop, read_result_mop, ver, value_or_nil, History};

#[test]
#[cfg(not(feature = "noop"))]
fn records() {
    let p = std::env::temp_dir().join(format!("hydo-hist-test-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    std::env::set_var("HYDO_HIST_OUT", &p);
    let kv = History::new("kv");
    let op = kv.invoke(3, "write", &["k1", &enc("a b")]);
    op.ok(&["k1", &ver("a b", 83)]);
    let op = kv.invoke(3, "read", &["k1"]);
    op.ok(&["k1", &value_or_nil(None)]);
    let op = kv.invoke(4, "txn", &[&append_mop("x", "1"), &read_mop("y")]);
    op.ok(&[&append_mop("x", "1"), &read_result_mop("y", &["1", "2"])]);
    kv.invoke(5, "write", &["k2", ""]).info();
    kv.invoke(6, "append", &["l", "7"]).ok_same();
    let got = std::fs::read_to_string(&p).unwrap();
    let want = [
        "HYDO-HIST kv 3 invoke * write k1 a%20b",
        "HYDO-HIST kv 3 ok * write k1 a%20b@83",
        "HYDO-HIST kv 3 invoke * read k1",
        "HYDO-HIST kv 3 ok * read k1 nil",
        "HYDO-HIST kv 4 invoke * txn a:x:1 r:y",
        "HYDO-HIST kv 4 ok * txn a:x:1 r:y:[1,2]",
        "HYDO-HIST kv 5 invoke * write k2 %",
        "HYDO-HIST kv 5 info * write k2 %",
        "HYDO-HIST kv 6 invoke * append l 7",
        "HYDO-HIST kv 6 ok * append l 7",
    ];
    let lines: Vec<String> = got.lines().map(|l| {
        let mut f: Vec<&str> = l.split(' ').collect();
        f[4] = "*";
        f.join(" ")
    }).collect();
    assert_eq!(lines, want);
    assert_eq!(enc("a,b[c]:d@e%f\tg"), "a%2Cb%5Bc%5D%3Ad%40e%25f%09g");
    assert_eq!(enc("nil"), "%6Eil");
    assert_eq!(enc("é x"), "é%20x");
    assert_eq!(list(&[]), "[]");
    let _ = std::fs::remove_file(&p);
}
