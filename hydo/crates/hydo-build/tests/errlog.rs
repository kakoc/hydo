//! hydo/onboard/tests/test_errlog.py (deleted in M107) re-expressed in Rust, the parts that are not
//! hydo-errors' (its host tests are hydo-errors' `src/tests.rs`):
//!
//! 1. the guest side (hydo-init-errlog.sh) under the real busybox: el_mark / el_report on fixture
//!    logs -- which lines are errors, their templates, labels, the summary record, the power-cut
//!    path, a log rotated by a restart, a file on the data disk, die_oracle, errors.max and
//!    errors.level. The manifest's HYDO_ERR_* lines come from hydo-build's compile_shell. Needs a
//!    busybox: HYDO_BUSYBOX=<static busybox> (the release's guest/busybox) or `busybox` on PATH;
//!    without one that test says so and passes (a Mac has none);
//! 2. hydo-build: a manifest with errors: splices the extension, and the refusals of `errors:`.
//!    (That manifests without errors: compile as before is covered by the build goldens.)

use hydo_build::shell::{compile_shell, shq, ShellTools};
use hydo_build::Ctx;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MYSQL: &[&str] = &[
    "2026-01-01T00:00:01.000001Z 0 [System] [MY-010116] [Server] /nix/store/x-mysql/bin/mysqld starting as process 77",
    "2026-01-01T00:00:02.000002Z 0 [Warning] [MY-010915] [Server] 'NO_ZERO_DATE' is deprecated",
    "2026-01-01T00:00:12.345678Z 0 [ERROR] [MY-012592] [InnoDB] Operating system error number 2 in a file operation.",
    "2026-01-01T00:07:44.000999Z 3 [ERROR] [MY-012592] [InnoDB] Operating system error number 28 in a file operation.",
];
const KAFKA: &[&str] = &[
    "INFO kafka.server.KafkaRaftServer Kafka Server started",
    "WARN org.apache.kafka.clients.NetworkClient [Producer clientId=producer-1] Connection to node 1 (localhost/127.0.0.1:9092) could not be established.",
    "ERROR [Controller id=1, targetBrokerId=1] Connection to node 1 (localhost/127.0.0.1:9093) could not be established. (org.apache.kafka.clients.NetworkClient)",
    "java.net.ConnectException: Connection refused",
    "\tat java.base/sun.nio.ch.Net.pollConnect(Native Method)",
    "Caused by: java.io.IOException: boom",
    "\t... 12 more",
    "ERROR [Controller id=1, targetBrokerId=1] Connection to node 1 (localhost/127.0.0.1:9093) could not be established. (org.apache.kafka.clients.NetworkClient)",
    "INFO kafka.log.LogManager Loading logs",
    "java.lang.IllegalStateException: request 3f2a9c1e-0b7d-4c1e-9a2b-1234567890ab failed at 0x7ffd1234 id deadbeef42",
];
const FRONT: &[&str] = &[
    r#"time=2026-01-01T00:00:05Z level=error msg="dial tcp 127.0.0.1:54321->127.0.0.1:3306: connect: connection refused""#,
    r#"time=2026-01-01T00:00:09Z level=error msg="dial tcp 127.0.0.1:41000->127.0.0.1:3306: connect: connection refused""#,
    "panic: runtime error: index out of range [3] with length 3",
    r#"HYDO-ASSERT {"t":"always","id":"x","ok":false}"#,
    "hydostack: server listening on 127.0.0.1:50051",
    r#"level=error msg="contains \"quotes\" and back\\slash and hydo-explore-workload-done""#,
];
const REDIS: &[&str] = &[
    "57:C 01 Jan 2026 00:00:00.001 # oO0OoO0OoO0Oo Redis is starting oO0OoO0OoO0Oo",
    "57:M 01 Jan 2026 00:00:00.002 * Ready to accept connections tcp",
    "91:M 01 Jan 2026 00:03:10.100 # !!! Warning: short read while loading the AOF file appendonly.aof.1.incr.aof!",
];

fn onboard() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard")
}

fn manifest(level: &str) -> Value {
    json!({"name": "t", "services": [
        {"name": "mysql", "cmd": "x", "ready": "x", "errors": {"files": ["/tmp/mysqld.err"]}},
        {"name": "kafka", "cmd": "x", "ready": "x"},
        {"name": "front", "cmd": "x", "ready": "x"},
        {"name": "redis", "cmd": "x", "ready": "x", "errors": {"format": "redis"}}],
        "workload": {"steps": 20, "run": "x"}, "invariants": [{"id": "i", "cmd": "true"}],
        "errors": {"level": level}})
}

fn busybox() -> Option<String> {
    if let Some(b) = std::env::var_os("HYDO_BUSYBOX") {
        return Some(b.to_string_lossy().into_owned());
    }
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("busybox"))
            .find(|b| b.is_file())
            .map(|b| b.display().to_string())
    })
}

fn tempdir(prefix: &str) -> PathBuf {
    hydo_build::tempdir(prefix).unwrap()
}

/// The manifest's errors lines (hydo-build's compile_errors: the HYDO_ERR_* block at the end of
/// manifest.sh) for the services in manifest order.
fn errors_lines(m: &Value) -> String {
    let mut m = m.clone();
    hydo_manifest::validate_data(&mut m).unwrap();
    let order: Vec<String> = m["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    let tools = ShellTools {
        tc: "TC".into(),
        fc: None,
        ip: None,
    };
    let (ms, _) = compile_shell(&Ctx::standalone(), &m, &order, &tools)
        .map_err(|e| e.message())
        .unwrap();
    let i = ms.find("\nHYDO_ERR_MAX=").expect("no HYDO_ERR_ lines") + 1;
    ms[i..].to_string()
}

/// Run the extension under busybox sh with stub init functions; returns the console text.
fn guest_run(bb: &str, m: &Value, steps: &str, recovery: bool) -> String {
    let d = tempdir("errlog-");
    let h = d.join("h");
    std::fs::create_dir_all(&h).unwrap();
    let order: Vec<&str> = m["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    let ext = std::fs::read_to_string(onboard().join("hydo-init-errlog.sh")).unwrap();
    let parts = [
        format!("BB={bb}"),
        format!("H={}", h.display()),
        format!("HYDO_SERVICES={}", shq(&order.join(" "))),
        "HYDO_STEPS=20".into(),
        "PHASE=serve".into(),
        format!(
            "CMD={}",
            if recovery {
                "'oracle 1 2'"
            } else {
                "provision"
            }
        ),
        "cut=0".into(),
        errors_lines(m).trim_end_matches('\n').to_string(),
        "log() { echo \"hydo-ob: $*\"; }".into(),
        "svc_sig() { :; }".into(),
        "_el_orig_mount_data() { :; }".into(),
        "_el_orig_step_range() { :; }".into(),
        "_el_orig_retry_workload() { :; }".into(),
        "_el_orig_failed_ids() { :; }".into(),
        "_el_orig_final_verdict() { echo 'HYDO-ORACLE: OK invariants=1 held'; }".into(),
        "_el_orig_die_oracle() { echo \"$1\"; }".into(),
        ext,
        "W() { printf '%s\\n' \"$2\" >> \"$1\"; }   # append one line to a log".into(),
        steps.replace("$D", &d.display().to_string()),
    ];
    let p = d.join("t.sh");
    std::fs::write(&p, parts.join("\n") + "\n").unwrap();
    // stdout and stderr in one stream, in order (as subprocess.STDOUT)
    let out = std::process::Command::new(bb)
        .args(["sh", "-c", "exec 2>&1 \"$0\" sh \"$1\"", bb])
        .arg(&p)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&d);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn recs(out: &str) -> (Vec<Value>, Vec<Value>) {
    let body = |l: &str| serde_json::from_str::<Value>(l.split_once(' ').unwrap().1).unwrap();
    (
        out.lines()
            .filter(|l| l.starts_with("HYDO-ERRLOG "))
            .map(body)
            .collect(),
        out.lines()
            .filter(|l| l.starts_with("HYDO-ERRLOG-SUM "))
            .map(body)
            .collect(),
    )
}

fn put(name: &str, lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| format!("W $H/{name} {}\n", shq(l)))
        .collect()
}

fn tpl(r: &Value) -> &str {
    r["tpl"].as_str().unwrap()
}

#[test]
fn guest_side_under_busybox() {
    let Some(bb) = busybox() else {
        eprintln!("SKIPPED: the guest tests need a busybox (set HYDO_BUSYBOX)");
        return;
    };
    let d0 = tempdir("errlog-f-");
    let mysqlerr = d0.join("mysqld.err").display().to_string();
    let mut m = manifest("error");
    m["services"][0]["errors"]["files"] = json!([mysqlerr]);
    let wf = |ls: &[&str]| -> String {
        ls.iter()
            .map(|l| format!("W {mysqlerr} {}\n", shq(l)))
            .collect()
    };
    let script = [
        put("mysql.log", &["mysqld starting"]),
        put("redis.log", &REDIS[..2]),
        put("kafka.log", &KAFKA[..2]),
        "mount_data x\n".into(),
        wf(&MYSQL[..2]),
        "step_range 0 10\n".into(),
        put("front.log", &FRONT[..1]),
        wf(&MYSQL[2..3]),
        put("kafka.log", &KAFKA[2..7]),
        // a restart of kafka before step 10: start_svc moves the log into .log.old
        "cat $H/kafka.log >> $H/kafka.log.old; : > $H/kafka.log\n".into(),
        "step_range 10 20\n".into(),
        put("kafka.log", &KAFKA[7..]),
        put("front.log", &FRONT[1..]),
        wf(&MYSQL[3..]),
        "retry_workload\n".into(),
        put("redis.log", &REDIS[2..]),
        "final_verdict\nfinal_verdict\n".into(),
    ]
    .concat();
    let out = guest_run(&bb, &m, &script, false);
    eprintln!("{out}");
    let (rs, sm) = recs(&out);
    assert!(
        sm.len() == 1 && sm[0]["boot"] == "first" && sm[0]["steps"] == 20,
        "one summary record, first boot, steps=20: {sm:?}"
    );
    assert!(
        out.trim_end()
            .lines()
            .last()
            .unwrap()
            .starts_with("HYDO-ORACLE: OK"),
        "the verdict line stays the last line"
    );
    assert_eq!(
        out.matches("HYDO-ERRLOG-SUM").count(),
        1,
        "el_report runs once per boot"
    );
    let my: Vec<&Value> = rs.iter().filter(|r| r["svc"] == "mysql").collect();
    assert!(
        my.len() == 1 && my[0]["n"] == 2 && my[0]["at"] == "0,10",
        "MySQL: two [ERROR] lines, one kind, in ranges 0 and 10: {my:?}"
    );
    let t = tpl(my[0]);
    assert!(
        t.contains("[MY-012592]") && t.contains("<time>") && t.contains("number #"),
        "MySQL template keeps the error code, drops time and errno-like numbers: {t}"
    );
    assert!(
        !rs.iter()
            .any(|r| r["svc"] != "redis" && tpl(r).contains("Warning")),
        "warning-level lines are not errors at level error"
    );
    let ka: Vec<&Value> = rs.iter().filter(|r| r["svc"] == "kafka").collect();
    assert_eq!(
        ka.len(),
        2,
        "Kafka: the ERROR line (twice, across the restart) and the standalone exception: {ka:?}"
    );
    let ke: Vec<&&Value> = ka.iter().filter(|r| tpl(r).starts_with("ERROR")).collect();
    assert!(
        !ke.is_empty()
            && ke[0]["n"] == 2
            && ke[0]["at"] == "0,10"
            && tpl(ke[0]).contains("#.#.#.#:9093"),
        "Kafka ERROR kind: n=2 over the rotated log, service port kept: {ke:?}"
    );
    assert!(
        !ka.iter()
            .any(|r| tpl(r).starts_with("java.net.ConnectException")),
        "an exception inside an error's trace is not counted again"
    );
    let ex: Vec<&&Value> = ka
        .iter()
        .filter(|r| tpl(r).contains("IllegalStateException"))
        .collect();
    assert!(
        !ex.is_empty() && tpl(ex[0]).contains("<uuid>") && tpl(ex[0]).contains("<hex>"),
        "uuid and hex replaced: {ex:?}"
    );
    let fr: Vec<&Value> = rs.iter().filter(|r| r["svc"] == "front").collect();
    let lv: Vec<&&Value> = fr
        .iter()
        .filter(|r| tpl(r).contains("level=error") && tpl(r).contains("dial"))
        .collect();
    assert!(
        lv.len() == 1 && lv[0]["n"] == 2 && tpl(lv[0]).contains("#.#.#.#:#->#.#.#.#:3306"),
        "logfmt level=error: ephemeral ports replaced, the service port kept, one kind: {lv:?}"
    );
    assert!(
        fr.iter()
            .any(|r| tpl(r).starts_with("panic: runtime error")),
        "Go panic line"
    );
    assert!(
        !fr.iter()
            .any(|r| tpl(r).contains("HYDO") && tpl(r).contains("ASSERT")),
        "our own HYDO- lines are never errors"
    );
    let q: Vec<&&Value> = fr.iter().filter(|r| tpl(r).contains("quotes")).collect();
    let qex = q.first().map(|r| r["ex"].as_str().unwrap()).unwrap_or("");
    assert!(
        !q.is_empty() && !qex.contains('"') && !qex.contains('\\') && !qex.contains("hydo-"),
        "no double quote, backslash or hydo- marker in the record text: {qex}"
    );
    let rd: Vec<&Value> = rs.iter().filter(|r| r["svc"] == "redis").collect();
    assert!(
        rd.len() == 2 && rd[0]["at"] == "s" && rd[1]["at"] == "end",
        "Redis preset: the # lines, at setup and at end: {rd:?}"
    );
    assert!(
        rd.iter().all(|r| tpl(r).contains("<date> <time>")),
        "Redis dates replaced"
    );
    let _ = std::fs::remove_dir_all(&d0);

    // the power cut path: failed_ids with cut=1 reports, the recovery boot labels everything r
    let out = guest_run(
        &bb,
        &manifest("error"),
        &format!(
            "step_range 0 12\n{}cut=1\nfailed_ids\nfailed_ids\n",
            put("kafka.log", &KAFKA[2..3])
        ),
        false,
    );
    let (rs, sm) = recs(&out);
    assert!(
        sm.len() == 1 && !rs.is_empty() && rs[0]["at"] == "0",
        "power cut: the first boot reports at the cut: {rs:?}"
    );
    let out = guest_run(
        &bb,
        &manifest("error"),
        &format!(
            "{}mount_data x\nfinal_verdict\n",
            put("kafka.log", &KAFKA[2..3])
        ),
        true,
    );
    let (rs, sm) = recs(&out);
    assert!(
        !sm.is_empty() && sm[0]["boot"] == "recovery" && !rs.is_empty() && rs[0]["at"] == "r",
        "recovery boot: label r: {sm:?} {rs:?}"
    );
    // a file on the data disk: lines from before this boot's mount are not reported again
    let d0 = tempdir("errlog-f-");
    let f = d0.join("err.log");
    std::fs::write(&f, format!("{}\n", MYSQL[2])).unwrap();
    let mut m = manifest("error");
    m["services"][0]["errors"]["files"] = json!([f.display().to_string()]);
    let out = guest_run(
        &bb,
        &m,
        &format!(
            "mount_data x\nW {} {}\nfinal_verdict\n",
            f.display(),
            shq(MYSQL[3])
        ),
        true,
    );
    let (rs, _) = recs(&out);
    let my: Vec<&Value> = rs.iter().filter(|r| r["svc"] == "mysql").collect();
    assert!(
        my.len() == 1 && my[0]["n"] == 1,
        "a data-disk file: only the lines written after this boot's mount: {my:?}"
    );
    let _ = std::fs::remove_dir_all(&d0);
    // die_oracle reports before its verdict line
    let out = guest_run(
        &bb,
        &manifest("error"),
        &format!(
            "{}die_oracle 'HYDO-ORACLE: VIOLATION service cannot recover (kafka)'\n",
            put("kafka.log", &KAFKA[2..3])
        ),
        false,
    );
    let l: Vec<&str> = out.trim().lines().collect();
    assert!(
        l.last().unwrap().starts_with("HYDO-ORACLE: VIOLATION")
            && l[..l.len() - 1]
                .iter()
                .any(|x| x.starts_with("HYDO-ERRLOG-SUM")),
        "die_oracle: the records come before its verdict"
    );
    // max: at most HYDO_ERR_MAX kinds printed, the summary says how many there were
    let mut m = manifest("error");
    m["errors"]["max"] = json!(2);
    let kinds: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|w| format!("ERROR kind {w}"))
        .collect();
    let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    let out = guest_run(
        &bb,
        &m,
        &format!("{}final_verdict\n", put("front.log", &kinds)),
        false,
    );
    let (rs, sm) = recs(&out);
    assert!(
        rs.len() == 2 && sm[0]["kinds"] == 4 && sm[0]["shown"] == 2,
        "errors.max bounds the records: {sm:?}"
    );
    // level: warning adds warning lines
    let out = guest_run(
        &bb,
        &manifest("warning"),
        &format!("{}final_verdict\n", put("kafka.log", &KAFKA[..2])),
        false,
    );
    let (rs, _) = recs(&out);
    assert!(
        rs.len() == 1 && tpl(&rs[0]).starts_with("WARN"),
        "level: warning: WARN lines too: {rs:?}"
    );
}

#[test]
fn build_splices_and_refuses() {
    let ctx = Ctx::standalone();
    let p = onboard().join("examples/redis-errors/hydo/hydo.yaml");
    let l = hydo_manifest::load(&p, &hydo_manifest::LoadOptions::default())
        .map_err(|e| e.message)
        .unwrap();
    let init = hydo_build::init::init_text(&ctx, &l.manifest.data)
        .map_err(|e| e.message())
        .unwrap();
    assert!(
        init.contains("_el_orig_final_verdict()") && init.contains("el_report()"),
        "a manifest with errors: splices hydo-init-errlog.sh"
    );
    for (errors, svc_errors, why) in [
        (json!({"bogus": 1}), None, "unknown field"),
        (json!({"max": 0}), None, "max"),
        (
            Value::Null,
            Some(json!({"format": "redis"})),
            "services[0].errors needs errors:",
        ),
        (json!(true), Some(json!({"match": "("})), "bad regex"),
        (
            json!(true),
            Some(json!({"files": ["relative.log"]})),
            "relative file",
        ),
    ] {
        let mut m = json!({"name": "t", "services": [{"name": "a", "cmd": "x", "ready": "x"}],
                           "workload": {"steps": 2, "run": "x"},
                           "invariants": [{"id": "i", "cmd": "true"}]});
        if !errors.is_null() {
            m["errors"] = errors;
        }
        if let Some(se) = svc_errors {
            m["services"][0]["errors"] = se;
        }
        assert!(
            hydo_manifest::validate_data(&mut m).is_err(),
            "refused: {why}"
        );
    }
}
