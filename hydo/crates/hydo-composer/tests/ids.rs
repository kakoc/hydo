//! hydo/onboard/tests/test_composer_ids.py and test_composer_history.py re-expressed in Rust:
//! stable compositions (M98) on examples/redis-composed's plan, and history checks as
//! test.checks entries.

use hydo_composer as hc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn onboard() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../onboard")
        .canonicalize()
        .unwrap()
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-composer-ids-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &dst.join(e.file_name()));
        } else {
            std::fs::copy(&p, dst.join(e.file_name())).unwrap();
        }
    }
}

/// The plan meta hydo-build computes for a hydo/ folder.
fn meta_of(dir: &Path) -> Value {
    let mut m = json!({"name": "t", "workload": {"steps": 20},
                       "test": {"parallel": 4, "per_round": 2, "round_ms": 100}});
    let folder = hc::scan_folder(&dir.display().to_string()).unwrap();
    let mut w = Vec::new();
    hc::expand(&mut m, folder.as_ref(), &mut w)
        .unwrap()
        .unwrap()
}

fn ids(meta: &Value) -> Vec<String> {
    hc::ids_kinds(meta).0
}

fn by_id(es: &[hc::Entry], ids: &[String]) -> Vec<(i128, i128, String, i128)> {
    es.iter()
        .map(|(e, r, c, s)| (*e, *r, ids[*c].clone(), *s))
        .collect()
}

fn add_cmd(d: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = d.join("workload").join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn stable_compositions() {
    let td = scratch("stable");
    let d = td.join("hydo");
    copy_dir(&onboard().join("examples/redis-composed/hydo"), &d);
    let meta = meta_of(&d);
    let ids1 = ids(&meta);
    assert!(meta["commands"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["content"].as_str().unwrap().len() == 16));
    // the M86 minimal composition: two transfers in round 4
    let w = "fc94c8ea,p2,q2,r200,n2,x4,o0.4.6";
    let res = hc::stable_w(w, &meta, None, None).unwrap();
    assert_eq!(
        res,
        ":p2,r200,cparallel_driver_transfer,7.4.0.721476632,8.4.0.40276333"
    );
    let desc = "r4: parallel_driver_transfer, parallel_driver_transfer";
    assert_eq!(hc::describe(&res, &meta, None).unwrap(), desc);
    assert_eq!(hc::describe(w, &meta, None).unwrap(), desc);
    // hydo save -> a file -> read back
    let contents = hc::contents_of(&res, &meta, None, None).unwrap();
    let x = json!({"from_seed": 6, "class": "violation",
                   "min_verdict": "VIOLATION [total_conserved] balances sum to 495",
                   "composition": desc, "minimal": format!("disk=none dseed=1 | run W={res}"),
                   "stable": res, "stable_schedule": format!("disk=none dseed=1 | run W={res}"),
                   "contents": contents});
    let rd = d.join("regressions");
    let found = rd.join("found.schedule");
    std::fs::write(&found, hc::saved_regression_text(&x).unwrap()).unwrap();
    let r = hc::read_regression(&found.display().to_string()).unwrap();
    let transfer = meta["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "parallel_driver_transfer")
        .unwrap()["content"]
        .clone();
    assert_eq!(r["contents"], json!({"parallel_driver_transfer": transfer}));
    let st = hc::regression_status(&r, &meta, None).unwrap();
    assert!(st.error.is_none() && st.warnings.is_empty() && st.schedule == r["schedule"]);
    // add two commands (one sorts first: every index moves) and rebuild the meta
    add_cmd(&d, "anytime_aaa_ping", "#!/bin/sh\ntrue\n");
    add_cmd(&d, "parallel_driver_incr", "#!/bin/sh\ntrue\n");
    let meta2 = meta_of(&d);
    let ids2 = ids(&meta2);
    let pos = |l: &[String]| l.iter().position(|x| x == "parallel_driver_transfer");
    assert_ne!(pos(&ids2), pos(&ids1));
    let regs = meta2["regressions"].as_array().unwrap();
    let r2 = regs.iter().find(|x| x["name"] == "found").unwrap();
    let st = hc::regression_status(r2, &meta2, None).unwrap();
    assert!(st.error.is_none() && st.warnings.is_empty(), "{st:?}");
    let w2 = hc::w_of(r2["schedule"].as_str().unwrap()).unwrap();
    let same = by_id(&hc::entries_of(&w2, &meta2, None, None).unwrap(), &ids2);
    assert_eq!(
        same,
        by_id(&hc::entries_of(w, &meta, None, None).unwrap(), &ids1)
    );
    // the seed form would now compose something else
    assert_ne!(
        by_id(&hc::entries_of(w, &meta2, None, None).unwrap(), &ids2),
        same
    );
    // the old index-form regression of the example (seed form + its saved list): resolved
    let old = regs.iter().find(|x| x["name"] == "lost-update").unwrap();
    assert_eq!(
        old["saved_schedule"],
        "disk=none dseed=1 | run W=0000002a,p4,q2,r100"
    );
    let ow = hc::w_of(old["schedule"].as_str().unwrap()).unwrap();
    assert!(ow.starts_with('@') && old["compose"].is_string());
    let orig = by_id(
        &hc::entries_of("0000002a,p4,q2,r100", &meta, None, None).unwrap(),
        &ids1,
    );
    let mut saved = hc::Saved::new();
    saved.insert(
        ow[1..].to_string(),
        old["compose"].as_str().unwrap().to_string(),
    );
    let now = by_id(
        &hc::entries_of(&ow, &meta2, None, Some(&saved)).unwrap(),
        &ids2,
    );
    assert_eq!(now, orig);
    assert_eq!(orig.len(), 34);
    // a shrunk failure whose resolved form did not fail the same way: saved in the index form
    let mut xs = x.clone();
    xs["minimal"] = json!(format!("disk=none dseed=1 | run W={w}"));
    xs["stable_note"] = json!("did not fail the same way");
    xs["commands"] = json!(ids1);
    let p = td.join("fallback.schedule");
    std::fs::write(&p, hc::saved_regression_text(&xs).unwrap()).unwrap();
    let rf = hc::read_regression(&p.display().to_string()).unwrap();
    assert!(rf["schedule"]
        .as_str()
        .unwrap()
        .ends_with(&format!("W={w}")));
    assert_eq!(rf["commands"], json!(ids1));
    assert!(rf["contents"].is_object());
    let st = hc::regression_status(&rf, &meta, None).unwrap();
    assert!(st.schedule == rf["schedule"] && st.error.is_none() && st.warnings.is_empty());
    let st = hc::regression_status(&rf, &meta2, None).unwrap(); // the list changed: resolved
    assert_eq!(hc::w_of(&st.schedule).unwrap(), res);
    assert!(st.error.is_none());
    // staging: the saved form's file
    let root = td.join("root");
    hc::stage(&meta2, &root, &onboard(), None).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("etc/hydo/compose.saved").join(&ow[1..]))
            .unwrap()
            .trim(),
        old["compose"].as_str().unwrap()
    );
    let cj: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("etc/hydo/compose.json")).unwrap())
            .unwrap();
    assert_eq!(cj["stable_compose"], 1);
    // a changed command warns
    let tp = d.join("workload/parallel_driver_transfer");
    let mut body = std::fs::read_to_string(&tp).unwrap();
    body.push_str("# edited\n");
    std::fs::write(&tp, body).unwrap();
    let st = hc::regression_status(&r, &meta_of(&d), None).unwrap();
    assert!(
        st.error.is_none() && st.warnings[0].contains("changed since the regression was saved")
    );
    // a removed command fails, naming it
    std::fs::remove_file(&tp).unwrap();
    let st = hc::regression_status(&r, &meta_of(&d), None).unwrap();
    let e = st.error.unwrap();
    assert!(
        e.contains("parallel_driver_transfer") && e.contains("no longer has"),
        "{e}"
    );
    // a round the plan does not run, an edited key
    let mut late = meta.clone();
    late["steps"] = json!(4);
    assert!(hc::regression_status(&r, &late, None)
        .unwrap()
        .error
        .unwrap()
        .contains("round 4"));
    let mut bad = r.clone();
    bad["schedule"] = json!("disk=none | run W=@000000000000");
    bad["compose"] = json!(res);
    assert!(hc::regression_status(&bad, &meta, None)
        .unwrap()
        .error
        .unwrap()
        .contains("disagree"));
    bad["schedule"] = json!(format!("disk=none | run W=@{}", hc::key_of(&res)));
    assert!(hc::regression_status(&bad, &meta, None)
        .unwrap()
        .error
        .is_none());
    // a seed form without a saved list: runs as it is, with a warning
    let st = hc::regression_status(
        &json!({"name": "n", "schedule": "disk=none | run W=0000002a", "expect": "ok"}),
        &meta,
        None,
    )
    .unwrap();
    assert!(st.schedule.ends_with("W=0000002a") && st.warnings[0].contains("seed form"));
    // o by id renders and parses, and omits the same command as by index
    let dd = hc::parse_seed("0000002a,p4,q2,r100,oparallel_driver_set", &meta).unwrap();
    assert_eq!(
        hc::render_w(&dd),
        "0000002a,p4,q2,r100,oparallel_driver_set"
    );
    let i = ids1
        .iter()
        .position(|x| x == "parallel_driver_set")
        .unwrap();
    let di = hc::parse_seed(&format!("0000002a,p4,q2,r100,o{i}"), &meta).unwrap();
    assert_eq!(
        hc::kept_of(&dd, 20, &meta).unwrap(),
        hc::kept_of(&di, 20, &meta).unwrap()
    );
    // staging copies the folder with its modes and times (shutil.copytree(symlinks=True))
    let root = td.join("root2");
    let folder = hc::scan_folder(&d.display().to_string()).unwrap();
    hc::stage(&meta_of(&d), &root, &onboard(), folder.as_ref()).unwrap();
    for e in std::fs::read_dir(d.join("workload"))
        .unwrap()
        .filter_map(Result::ok)
    {
        let a = std::fs::metadata(e.path()).unwrap();
        let b = std::fs::metadata(root.join("hydo/workload").join(e.file_name())).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            (a.mode(), a.mtime(), a.mtime_nsec()),
            (b.mode(), b.mtime(), b.mtime_nsec())
        );
    }
    let _ = std::fs::remove_dir_all(&td);
}

fn base(checks: Value) -> Value {
    json!({"name": "t", "workload": {"steps": 3},
           "test": {"commands": [{"name": "w", "kind": "parallel_driver", "run": ["/bin/w"]}],
                    "checks": checks}})
}

#[test]
fn history_checks() {
    let mut m = base(json!([
        {"name": "kv.lin", "kind": "linearizable", "history": "kv", "model": "cas", "run": ["/bin/readback"]},
        {"name": "log.la", "kind": "list-append", "history": "log", "consistency": "serializable"},
        {"name": "ok", "kind": "final", "run": ["/bin/ok"]}]));
    let mut w = Vec::new();
    let meta = hc::expand(&mut m, None, &mut w).unwrap().unwrap();
    let names: Vec<&Value> = meta["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["name"])
        .collect();
    assert_eq!(names, [&json!("ok")]);
    assert_eq!(
        m["checks"],
        json!([
            {"id": "kv.lin", "type": "linearizable", "history": "kv", "msg": "kv.lin", "model": "cas",
             "final": "exec '/bin/readback'"},
            {"id": "log.la", "type": "list-append", "history": "log", "msg": "log.la",
             "consistency": "serializable"}])
    );
    assert!(!m["invariants"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["id"] == "kv.lin"));
    for bad in [
        json!({"name": "x", "kind": "linearizable"}),
        json!({"name": "x", "kind": "linearizable", "history": "h", "within": 3}),
        json!({"name": "x", "kind": "final", "run": ["/bin/x"], "history": "h"}),
    ] {
        assert!(hc::expand(&mut base(json!([bad])), None, &mut Vec::new()).is_err());
    }
    // the folder: a check file with a header
    let d = scratch("hist");
    std::fs::create_dir_all(d.join("checks")).unwrap();
    let p = d.join("checks/list_append_app.log");
    std::fs::write(
        &p,
        "#!/bin/sh\n# hydo: history=log consistency=snapshot-isolation\n# hydo-msg: lists\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fo = hc::scan_folder(&d.display().to_string()).unwrap().unwrap();
    assert_eq!(
        fo["checks"],
        json!([{"name": "app.log", "kind": "list_append", "run": ["/hydo/checks/list_append_app.log"],
                "history": "log", "consistency": "snapshot-isolation", "msg": "lists"}])
    );
    let mut m = base(json!([]));
    hc::expand(&mut m, Some(&fo), &mut Vec::new()).unwrap();
    assert_eq!(m["checks"][0]["type"], "list-append");
    assert_eq!(
        m["checks"][0]["final"],
        "exec '/hydo/checks/list_append_app.log'"
    );
    let _ = std::fs::remove_dir_all(&d);
    // the example builds (hydo-build --check)
    let y = onboard().join("examples/redis-composed-hist/hydo/hydo.yaml");
    let l = hydo_manifest::load(&y, &hydo_manifest::LoadOptions::default()).unwrap();
    let lines = hydo_manifest::check_summary(&y.display().to_string(), &l);
    assert!(lines[0].contains("+ 2 history checks"), "{lines:?}");
}
