//! The host tests of hydo/onboard/tests/test_errlog.py (`test_host`), re-expressed: signatures,
//! merging over boots, the last boot of an appended console, known-list matching with with: and
//! during:, `hydo errors accept` round trip, unknown: fail, fault windows and patterns.

use super::*;
use serde_json::json;

const CON: &str = concat!(
    "hydo-ob: x\n",
    r##"HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"s","tpl":"#:C <date> <time> # Redis is starting","ex":"57:C ..."}"##,
    "\n",
    r##"HYDO-ERRLOG {"svc":"kafka","src":"log","n":3,"at":"10,11","tpl":"ERROR Connection to node # refused","ex":"ERROR Connection to node 1 refused"}"##,
    "\n",
    r##"HYDO-ERRLOG-SUM {"boot":"first","steps":20,"lines":4,"kinds":2,"shown":2}"##
);
const REC: &str = concat!(
    r##"HYDO-ERRLOG {"svc":"kafka","src":"log","n":1,"at":"r","tpl":"ERROR Connection to node # refused","ex":"..."}"##,
    "\n",
    r##"HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"r","tpl":"#:M <date> <time> # !!! Warning: short read while loading the AOF file appendonly.aof.#.incr.aof!","ex":"..."}"##,
    "\n",
    r##"HYDO-ERRLOG-SUM {"boot":"recovery","steps":20,"lines":2,"kinds":2,"shown":2}"##
);

fn src(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-errors-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn host() {
    let p = parse_consoles(&src(&[("a", CON), ("b", REC)])).unwrap();
    assert_eq!(p.kinds.len(), 3, "three kinds over both boots");
    let k = p.kinds.iter().find(|x| x.svc == "kafka").unwrap().clone();
    assert_eq!(
        (k.n, k.at.clone()),
        (4, vec!["10".into(), "11".into(), "r".into()])
    );
    assert_eq!(
        k.sig,
        signature("kafka", "ERROR Connection to node # refused")
    );
    assert_eq!(k.sig.len(), 12);
    assert!(parse_consoles(&src(&[("a", "no records")])).is_none());
    let again = format!(
        "{CON}\n{}",
        CON.replace(r#""n":3"#, r#""n":1"#)
            .replace("redis is starting", "x")
    );
    let again = parse_consoles(&src(&[("a", &again)])).unwrap();
    assert_eq!(again.kinds.iter().map(|k| k.n).sum::<i64>(), 2);
    assert_eq!(
        again.boots.len(),
        1,
        "only the last boot of an appended console counts"
    );

    let d = scratch("host");
    let f = d.join("known-errors.yaml").display().to_string();
    let kj = |k: &Kind| Value::Object(k.to_json());
    let ban = p
        .kinds
        .iter()
        .find(|x| x.svc == "redis" && x.tpl.contains("starting"))
        .unwrap();
    let aof = p
        .kinds
        .iter()
        .find(|x| x.tpl.contains("short read"))
        .unwrap();
    let date = Some("2026-10-30");
    accept(
        &f,
        &[kj(ban)],
        Some("the startup banner: Redis logs it at its warning level"),
        Some("test"),
        &[],
        &[],
        date,
    )
    .unwrap();
    accept(
        &f,
        &[kj(aof)],
        Some("a power cut truncates the AOF; aof-load-truncated loads what was synced"),
        Some("test"),
        &["C".into()],
        &[],
        date,
    )
    .unwrap();
    accept(
        &f,
        &[kj(&k)],
        Some("Kafka's clients retry while kafka is down"),
        Some("test"),
        &[],
        &["K=kafka".into()],
        date,
    )
    .unwrap();
    assert!(
        accept(&f, &[kj(&k)], Some("again"), Some("test"), &[], &[], date)
            .unwrap()
            .is_empty()
    );
    let mut kn = load_known(Some(&f)).unwrap();
    assert_eq!((kn.known.len(), kn.unknown.as_str()), (3, "report"));
    let an = analyse(
        &src(&[("a", CON), ("b", REC)]),
        "disk=cut dseed=1 | run K=kafka@10+2 C=14",
        &kn,
    )
    .unwrap();
    assert_eq!(
        an.new.iter().map(|x| x.svc.as_str()).collect::<Vec<_>>(),
        ["kafka"]
    );
    let an2 = analyse(&src(&[("a", CON)]), "disk=none | run K=kafka@10+2", &kn).unwrap();
    assert!(
        an2.new.is_empty() && an2.known.len() == 2,
        "inside the kill window"
    );
    let an3 = analyse(&src(&[("a", CON)]), "disk=none | run K=kafka@10+1", &kn).unwrap();
    assert!(
        an3.new.is_empty(),
        "the range right after the window counts as during"
    );
    let an4 = analyse(&src(&[("a", CON)]), "disk=none | run K=kafka@3+2", &kn).unwrap();
    assert_eq!(
        an4.new.iter().map(|x| x.svc.as_str()).collect::<Vec<_>>(),
        ["kafka"]
    );
    let an5 = analyse(&src(&[("b", REC)]), "disk=none | run", &kn).unwrap();
    assert!(
        an5.new.iter().any(|x| x.tpl.contains("short read")),
        "with: [C]"
    );
    let mut res = json!({"verdict": "HYDO-ORACLE: OK x"})
        .as_object()
        .unwrap()
        .clone();
    apply(&mut res, Some(&an4), &kn);
    assert_eq!(res["verdict"], "HYDO-ORACLE: OK x");
    assert_eq!(res["errors_new"], json!([k.sig]));
    kn.unknown = "fail".into();
    let mut res = json!({"verdict": "HYDO-ORACLE: OK x"})
        .as_object()
        .unwrap()
        .clone();
    apply(&mut res, Some(&an4), &kn);
    assert!(res["verdict"]
        .as_str()
        .unwrap()
        .starts_with("VIOLATION error-log [kafka]"));
    assert!(res["pre_errlog_verdict"]
        .as_str()
        .unwrap()
        .starts_with("HYDO-ORACLE: OK"));
    let w = fault_windows("disk=cut cf=7 | run K=redis@3+2 P=kafka@5~10:200 N=mysql@2-6:delay,100ms C=12 T=4:-30 R=everysec", 20);
    let want: Vec<(String, i64, i64)> = [
        ("disk=cut", 0, 20),
        ("cf=7", 0, INF),
        ("K=redis@3+2", 3, 5),
        ("P=kafka@5~10:200", 5, 6),
        ("N=mysql@2-6:delay,100ms", 2, 6),
        ("C=12", 12, INF),
        ("T=4:-30", 4, INF),
        ("R=everysec", -1, INF),
    ]
    .iter()
    .map(|(a, b, c)| (a.to_string(), *b, *c))
    .collect();
    assert_eq!(w, want);
    assert!(
        fault_matches("C", "cf=7")
            && fault_matches("K", "K=redis@3+2")
            && fault_matches("K=redis", "K=redis@3+2")
    );
    assert!(!fault_matches("K=red", "K=redis@3+2") && fault_matches("disk", "disk=cut"));
    let mut t = std::fs::read_to_string(&f).unwrap();
    t.push_str("  - match: '('\n    reason: x\n");
    std::fs::write(&f, t).unwrap();
    let e = load_known(Some(&f)).unwrap_err();
    assert!(
        e.0.contains(
            "is not a regular expression (missing ), unterminated subpattern at position 0)"
        ),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn missing_list_is_a_message() {
    let e = load_known(Some("/nonexistent/hydo/known-errors.yaml")).unwrap_err();
    assert!(
        e.0.starts_with("/nonexistent/hydo/known-errors.yaml: no such file"),
        "{e}"
    );
}

#[test]
fn accept_writes_no_name_unless_given() {
    // open item 84: the list is usually tracked, so `hydo errors accept` without --by names no
    // one (before, it wrote the login name as `by:`)
    let p = parse_consoles(&src(&[("a", CON)])).unwrap();
    let k = Value::Object(p.kinds[0].to_json());
    let d = scratch("by");
    let f = d.join("known-errors.yaml").display().to_string();
    let add = accept(
        &f,
        std::slice::from_ref(&k),
        Some("expected"),
        None,
        &[],
        &[],
        None,
    )
    .unwrap();
    assert_eq!(add.len(), 1);
    assert!(!add[0].contains("by:"), "{}", add[0]);
    let text = std::fs::read_to_string(&f).unwrap();
    assert!(
        !text.split("known:").nth(1).unwrap().contains("by:"),
        "{text}"
    );
    assert_eq!(load_known(Some(&f)).unwrap().known.len(), 1);
    let named = d.join("named.yaml").display().to_string();
    let add = accept(&named, &[k], Some("expected"), Some("team"), &[], &[], None).unwrap();
    assert!(
        add[0].contains("    by: ") && add[0].contains("team"),
        "{}",
        add[0]
    );
    let _ = std::fs::remove_dir_all(&d);
}
