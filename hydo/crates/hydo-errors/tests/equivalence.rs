//! hydo-errors against the recorded answers of hydo/onboard/hydo_errors.py (the reference until
//! M107, when the Python was deleted).
//!
//! The driver below is tests/py_ref.py (removed in M107; `git show 2a58c4b:hydo/crates/hydo-errors/tests/py_ref.py`)
//! ported case for case: synthetic consoles against known lists and schedules, every load_known
//! message, fault windows, label ranks, explorations, accept and for_run_dir, and with
//! HYDO_ERRLOG_CORPUS=<dir> (a copy of the bare-metal machine's M94 results with
//! known/redis-errors.yaml and known/stack.yaml, the lists those runs used; it was
//! `~/hydo-pc-work/m94` there) every recorded console and exploration of M94. Each case is one line
//! `{"op", "args", "out"}` as py_ref.py printed it (paths as `<TOP>`, `<WORK>`, `<CORPUS>`, a case
//! directory as `<dir>`, a crash's message blank, and the JSON decoder's own words of a .json list
//! that is not JSON blank: serde_json and Python word it differently). `tests/golden/errors.sha256`
//! holds the SHA-256 of every 64 lines of Python's stream (826 cases; 4,757 with the corpus,
//! whose file list is pinned too), recorded at 2a58c4b with Python 3.15.0 on the Mac.
//!
//! `recorded_files` compares the Rust output with the files M94's runs wrote (one.errors.json,
//! consoles/<tag>.errors.json, errors.json and report.md's section), byte for byte.
//!
//! `HYDO_BLESS=1` re-records the golden from the Rust answers (with HYDO_ERRLOG_CORPUS set, the
//! corpus rows too). `HYDO_RECORD_PY=<stream> HYDO_RECORD_WORK=<its --work>` (and
//! `HYDO_RECORD_PY_CORPUS=<stream with --corpus>` with HYDO_ERRLOG_CORPUS) records from saved
//! py_ref.py output instead (how the golden was made).

mod common;

use common::golden;
use hydo_errors as he;
use hydo_manifest::py::{json_dumps, json_dumps_indent};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const CHUNK: usize = 64;

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

fn onb() -> PathBuf {
    top().join("hydo/onboard")
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/errors.sha256")
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-errors-eq-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn srcs(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let n = p[0].as_str().unwrap().to_string();
            let t = he::read_text_lossy(Path::new(p[1].as_str().unwrap())).unwrap();
            (n, t)
        })
        .collect()
}

fn known(a: &Value) -> Result<he::KnownList, he::ErrorsError> {
    he::load_known(a["known"].as_str())
}

fn view(k: &he::KnownList) -> Value {
    json!({"path": k.path, "sha256": k.sha256, "unknown": k.unknown,
           "known": k.known.iter().map(|e| Value::Object(e.fields.clone())).collect::<Vec<_>>()})
}

fn reses() -> Vec<Map<String, Value>> {
    [
        json!({"verdict": "HYDO-ORACLE: OK invariants=1 held"}),
        json!({"verdict": "HYDO-ORACLE: VIOLATION [x] y"}),
        json!({"fp": "0123456789abcdef"}),
    ]
    .into_iter()
    .map(|v| v.as_object().unwrap().clone())
    .collect()
}

type Out = Result<Value, (&'static str, String)>;

fn ee(e: he::ErrorsError) -> (&'static str, String) {
    ("ErrorsError", e.0)
}

/// One case, answered by hydo-errors (the op names and args are py_ref.py's).
fn run(op: &str, a: &Value, work: &Path, n: usize) -> Out {
    Ok(match op {
        "analyse" => {
            let mut k = known(a).map_err(ee)?;
            if let Some(u) = a["unknown"].as_str() {
                k.unknown = u.to_string();
            }
            let an = he::analyse(&srcs(&a["srcs"]), a["schedule"].as_str().unwrap(), &k);
            let mut o = Map::new();
            o.insert(
                "an".into(),
                an.as_ref().map(|x| x.to_json()).unwrap_or(Value::Null),
            );
            o.insert("summary".into(), json!(he::summary_lines(an.as_ref(), 12)));
            let applied: Vec<Value> = reses()
                .into_iter()
                .map(|mut r| {
                    he::apply(&mut r, an.as_ref(), &k);
                    Value::Object(r)
                })
                .collect();
            o.insert("apply".into(), Value::Array(applied));
            if let Some(an) = &an {
                let p = work.join(format!("rs-run-{n}.json"));
                he::write_run_json(&p, an, &k).unwrap();
                o.insert(
                    "run_json".into(),
                    json!(std::fs::read_to_string(&p).unwrap()),
                );
                let _ = std::fs::remove_file(&p);
            }
            Value::Object(o)
        }
        "load_known" => view(&he::load_known(a["path"].as_str()).map_err(ee)?),
        "fault_windows" => json!(he::fault_windows(
            a["schedule"].as_str().unwrap(),
            a["steps"].as_i64().unwrap()
        )),
        "label_rank" => json!(he::label_rank(a["lab"].as_str().unwrap(), 20)),
        "explore" => {
            let k = known(a).map_err(ee)?;
            let runs: Vec<(Value, String, Option<he::Analysis>)> = a["runs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    let sched = r[1].as_str().unwrap().to_string();
                    let an = he::analyse(&srcs(&r[2]), &sched, &k);
                    (r[0].clone(), sched, an)
                })
                .collect();
            let agg = he::collect(&runs);
            let md = he::report_md(&agg, &k, a["shrunk"].as_object().unwrap(), &|s| {
                format!("`hydo run \"{s}\"`")
            });
            json!({"agg": agg.iter().map(|x| Value::Object(x.to_json())).collect::<Vec<_>>(), "md": md})
        }
        "accept" => {
            let d = PathBuf::from(a["dir"].as_str().unwrap());
            std::fs::create_dir_all(&d).unwrap();
            let p = d.join("sub/known-errors.yaml");
            if let Some(b) = a["before"].as_str() {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, b).unwrap();
            }
            let strs = |k: &str| -> Vec<String> {
                a[k].as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect()
            };
            let add = he::accept(
                &p.display().to_string(),
                a["kinds"].as_array().unwrap(),
                a["reason"].as_str(),
                a["by"].as_str().unwrap(),
                &strs("with"),
                &strs("during"),
                Some("2026-10-30"),
            )
            .map_err(ee)?;
            json!({"add": add, "text": std::fs::read_to_string(&p).unwrap()})
        }
        "for_run_dir" => {
            let d = PathBuf::from(a["dir"].as_str().unwrap());
            std::fs::create_dir_all(&d).unwrap();
            for (name, text) in a["files"].as_object().unwrap() {
                std::fs::write(d.join(name), text.as_str().unwrap()).unwrap();
            }
            let mut printed: Vec<String> = Vec::new();
            let (res, an) = he::for_run_dir(
                &d,
                a["schedule"].as_str().unwrap(),
                a["res"].as_object().unwrap().clone(),
                a["known"].as_str(),
                &mut |l| printed.push(l.to_string()),
            );
            // since M115 hydo run reports the analysis in result.json; the M94 file of the same
            // analysis is what the recorded answers hold
            let p = d.join("one.errors.json");
            if let Some((an, k)) = &an {
                he::write_run_json(&p, an, k).unwrap();
            }
            json!({"res": res, "printed": printed,
                   "one_errors": std::fs::read_to_string(&p).ok()})
        }
        other => panic!("unknown op {other}"),
    })
}

/// The line as compared (see the module doc).
fn canonical(case: &Value, dirs: &[String], paths: &[(String, &str)]) -> String {
    let mut c = case.clone();
    if c["out"]["err"] == "crash" {
        c["out"]["msg"] = json!("");
    }
    if c["op"] == "load_known"
        && c["out"].get("err").is_some()
        && c["args"]["path"]
            .as_str()
            .is_some_and(|p| p.ends_with(".json"))
        && c["out"]["msg"]
            .as_str()
            .is_some_and(|m| m.contains(" line 1 column"))
    {
        c["out"]["msg"] = json!("<the JSON decoder's message>");
    }
    let mut t = json_dumps(&c);
    for d in dirs {
        let esc = json_dumps(&json!(d));
        t = t.replace(&esc[1..esc.len() - 1], "<dir>");
    }
    for (p, name) in paths {
        let esc = json_dumps(&json!(p));
        t = t.replace(&esc[1..esc.len() - 1], name);
    }
    t
}

// ------------------------------------------------------------------ the driver (py_ref.py)

struct Driver {
    work: PathBuf,
    paths: Vec<(String, &'static str)>,
    sections: golden::Sections,
    n: usize,
}

impl Driver {
    fn section(&mut self, label: &str) {
        self.sections.push((label.to_string(), Vec::new()));
    }
    fn emit(&mut self, op: &str, args: Value, dirs: &[String]) {
        let out = match run(op, &args, &self.work, self.n) {
            Ok(v) => json!({ "ok": v }),
            Err((k, m)) => json!({"err": k, "msg": m}),
        };
        let line = canonical(
            &json!({"op": op, "args": args, "out": out}),
            dirs,
            &self.paths,
        );
        self.sections.last_mut().unwrap().1.push(line);
        self.n += 1;
    }
    fn p(&self, rel: &str) -> String {
        self.work.join(rel).display().to_string()
    }
    fn analyse(&mut self, srcs: &Value, schedule: &str, known: &Value, unknown: Option<&str>) {
        self.emit(
            "analyse",
            json!({"srcs": srcs, "schedule": schedule, "known": known, "unknown": unknown}),
            &[],
        );
    }
    fn explore(&mut self, runs: &Value, known: &Value, shrunk: &Value, label: Option<&str>) {
        self.emit(
            "explore",
            json!({"runs": runs, "known": known, "shrunk": shrunk, "label": label}),
            &[],
        );
    }
}

const CON: &[&str] = &[
    "hydo-ob: x",
    r##"HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"s","tpl":"#:C <date> <time> # Redis is starting","ex":"57:C ..."}"##,
    r##"HYDO-ERRLOG {"svc":"kafka","src":"log","n":3,"at":"10,11","tpl":"ERROR Connection to node # refused","ex":"ERROR Connection to node 1 refused"}"##,
    r##"HYDO-ERRLOG-SUM {"boot":"first","steps":20,"lines":4,"kinds":2,"shown":2}"##,
];
const REC: &[&str] = &[
    r##"HYDO-ERRLOG {"svc":"kafka","src":"log","n":1,"at":"r","tpl":"ERROR Connection to node # refused","ex":"..."}"##,
    r##"HYDO-ERRLOG {"svc":"redis","src":"log","n":1,"at":"r","tpl":"#:M <date> <time> # !!! Warning: short read while loading the AOF file appendonly.aof.#.incr.aof!","ex":"..."}"##,
    r##"HYDO-ERRLOG-SUM {"boot":"recovery","steps":20,"lines":2,"kinds":2,"shown":2}"##,
];
const ODD: &[&str] = &[
    "[   1.2] noise HYDO-ERRLOG {\"svc\":\"mysql\",\"src\":\"file\",\"n\":\"2\",\"at\":\"3,end,s,12,x\",\"tpl\":\"[ERROR] [MY-012592] [InnoDB] Operating system error number # in a file operation.\",\"ex\":\"\u{e9} caf\u{e9} \u{2603}\"}   ",
    r##"HYDO-ERRLOG {"svc":"mysql","src":"file","n":1.9,"at":"r,0","tpl":"[ERROR] [MY-012592] [InnoDB] Operating system error number # in a file operation.","ex":"x"}"##,
    "HYDO-ERRLOG {not json}",
    r##"HYDO-ERRLOG {"svc":"front","n":true,"at":7,"tpl":"panic: runtime error: index out of range [#] with length #"}"##,
    r##"HYDO-ERRLOG-SUMMARY {"x":1}"##,
    "HYDO-ERRLOG-SUM {\"boot\":\"first\",\"steps\":\"12\",\"lines\":5,\"kinds\":7,\"shown\":3} \t",
    r##"HYDO-ERRLOG {"svc":"front","src":"log","n":4,"at":"end","tpl":"level=error msg=\"dial tcp #.#.#.#:#->#.#.#.#:3306: connect: connection refused\"","ex":"dial"}"##,
];
const EMPTY: &str =
    "HYDO-ERRLOG-SUM {\"boot\":\"first\",\"steps\":4,\"lines\":0,\"kinds\":0,\"shown\":0}\n";

fn lists() -> Vec<(&'static str, String)> {
    let sig_yaml = format!(
        "known:\n  - sig: {}\n    reason: \"kafka, by sig\"\n    with: [K=kafka]\n  - sig: {}\n    service: redis\n    reason: banner\n",
        he::signature("kafka", "ERROR Connection to node # refused"),
        he::signature("redis", "#:C <date> <time> # Redis is starting")
    );
    let list_json = json_dumps(&json!({"format": 1, "unknown": "report", "known": [
        {"sig": he::signature("kafka", "ERROR Connection to node # refused"), "reason": "json"}]}));
    vec![
        (
            "match.yaml",
            r#"format: 1
unknown: fail
known:
  - match: 'Connection to node # (refused|reset)'
    service: kafka
    reason: clients retry
    during: K=kafka
  - match: '(?<!xx)short read.*\.aof!$'
    reason: aof
    with: [C, disk=ioerr]
  - match: '(?i)REDIS IS STARTING'
    reason: banner
    service: redis
  - match: 'MY-\d{6}\] \[InnoDB\]'
    reason: innodb
    with: K=mysql
  - match: 'panic: runtime error: index out of range \[#\] with length #'
    reason: 0b
  - match: 'dial tcp [#.]+:#->[#.]{7}:3306'
    reason: front
    during: [N=mysql, K]
"#
            .to_string(),
        ),
        ("sig.yaml", sig_yaml),
        ("empty.yaml", String::new()),
        ("comment.yaml", "# nothing yet\n".into()),
        ("list.json", list_json),
    ]
}

const BAD: &[(&str, &str)] = &[
    ("notmap.yaml", "- a\n- b\n"),
    ("top.yaml", "format: 1\nbogus: 2\nzz: 1\n"),
    ("format.yaml", "format: 2\n"),
    ("format-true.yaml", "format: true\nknown:\n"),
    ("policy.yaml", "unknown: maybe\n"),
    ("known-map.yaml", "known:\n  a: 1\n"),
    ("entry.yaml", "known:\n  - just text\n"),
    (
        "fields.yaml",
        "known:\n  - sig: 0123456789ab\n    reason: x\n    color: red\n",
    ),
    (
        "both.yaml",
        "known:\n  - sig: 0123456789ab\n    match: x\n    reason: x\n",
    ),
    ("neither.yaml", "known:\n  - reason: x\n"),
    ("sig.yaml", "known:\n  - sig: 0123456789AB\n    reason: x\n"),
    (
        "sig-int.yaml",
        "known:\n  - sig: 123456789012\n    reason: x\n",
    ),
    ("reason.yaml", "known:\n  - sig: 0123456789ab\n"),
    (
        "reason-blank.yaml",
        "known:\n  - sig: 0123456789ab\n    reason: '   '\n",
    ),
    (
        "reason-zero.yaml",
        "known:\n  - sig: 0123456789ab\n    reason: 0\n",
    ),
    ("regex.yaml", "known:\n  - match: 'a(b'\n    reason: x\n"),
    ("regex2.yaml", "known:\n  - match: '*a'\n    reason: x\n"),
    ("regex3.yaml", "known:\n  - match: '[z-a]'\n    reason: x\n"),
    (
        "with.yaml",
        "known:\n  - sig: 0123456789ab\n    reason: x\n    with: []\n",
    ),
    (
        "with2.yaml",
        "known:\n  - sig: 0123456789ab\n    reason: x\n    with: 'K mysql'\n",
    ),
    (
        "during.yaml",
        "known:\n  - sig: 0123456789ab\n    reason: x\n    during: [K, '']\n",
    ),
    (
        "during2.yaml",
        "known:\n  - sig: 0123456789ab\n    reason: x\n    during: 3\n",
    ),
    ("yaml.yaml", "known:\n\t- sig: x\n"),
    ("dup.yaml", "known: 1\nknown: 2\n"),
    ("bad.json", "{not json"),
    ("list.json", "[1, 2]"),
];

const SCHEDULES: &[&str] = &[
    "disk=none | run",
    "disk=cut dseed=1 | run K=kafka@10+2 C=14",
    "disk=none | run K=kafka@10+2",
    "disk=none | run K=kafka@10+1",
    "disk=none | run K=kafka@3+2",
    "disk=ioerr:3 dseed=1 | run K=mysql@2+30 N=mysql@0-25:loss,100%",
    "disk=cut cf=7 | run K=redis@3+2 P=kafka@5~10:200 N=mysql@2-6:delay,100ms C=12 T=4:-30 R=everysec",
    "disk=none | run P=kafka@5+3 X=kafka@4 F=kafka<mysql@3#2:100 O=kafka@1-30:req:loss H=pay@0-1:drop W=0000002a,p4",
    "disk=none | K=x@1+1",
    "| run K=@3+1 K=a@b+1 K=a@3+x P=a@3 N=a@3- N=a@3-x C=x T=x:1 C=+5 T= = x=",
];

/// The two repository lists the synthetic cases read, as recorded (their sha256).
const PINS: &[(&str, &str)] = &[
    (
        "hydo/onboard/examples/redis-errors/hydo/known-errors.yaml",
        "7aeb9c62f10ff842c90220b26a136a562bc5e42480572361cb1b94f731db259a",
    ),
    (
        "hydo/onboard/examples/stack/known-errors.yaml",
        "ad216dc2626046594f113c09d4e3984ee6be18442d6a63fb1cb06e9f6b81c373",
    ),
];

fn pair(n: &str, p: &str) -> Value {
    json!([n, p])
}

/// py_ref.py's `synthetic()`; returns the console files by name.
fn synthetic(dr: &mut Driver) -> std::collections::BTreeMap<&'static str, String> {
    dr.section("synthetic");
    let d = dr.work.join("con");
    std::fs::create_dir_all(&d).unwrap();
    let con = CON.join("\n");
    let rec = REC.join("\n");
    let odd = ODD.join("\r\n");
    let appended = format!(
        "{con}\n{}\n{rec}",
        con.replace("\"n\":3", "\"n\":1")
            .replace("Redis is starting", "x")
    );
    let mut files = std::collections::BTreeMap::new();
    for (name, text) in [
        ("con.log", con.clone()),
        ("rec.log", rec),
        ("odd.log", odd),
        ("appended.log", appended),
        ("empty.log", EMPTY.to_string()),
        ("none.log", "no records here\n".to_string()),
    ] {
        let p = d.join(name);
        std::fs::write(&p, text).unwrap();
        files.insert(name, p.display().to_string());
    }
    let f = |n: &str| files[n].clone();
    let sets: Vec<Value> = vec![
        json!([pair("a", &f("con.log")), pair("b", &f("rec.log"))]),
        json!([pair("a", &f("con.log"))]),
        json!([pair("b", &f("rec.log"))]),
        json!([pair("a", &f("odd.log"))]),
        json!([pair("a", &f("appended.log"))]),
        json!([pair("a", &f("empty.log"))]),
        json!([pair("a", &f("none.log"))]),
        json!([pair("a", &f("odd.log")), pair("b", &f("rec.log"))]),
    ];
    let mut ls: Vec<Value> = vec![
        Value::Null,
        json!(onb()
            .join("examples/redis-errors/hydo/known-errors.yaml")
            .display()
            .to_string()),
        json!(onb()
            .join("examples/stack/known-errors.yaml")
            .display()
            .to_string()),
    ];
    for (name, text) in lists() {
        let p = dr.work.join("lists").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        ls.push(json!(p.display().to_string()));
    }
    for srcs in &sets {
        for sched in SCHEDULES {
            for k in &ls {
                dr.analyse(srcs, sched, k, None);
            }
        }
        let l3 = ls[3].clone();
        dr.analyse(srcs, SCHEDULES[1], &l3, Some("fail"));
    }
    for k in ls.clone() {
        dr.emit("load_known", json!({ "path": k }), &[]);
    }
    for (name, text) in BAD {
        let p = dr.work.join("bad").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        dr.emit("load_known", json!({"path": p.display().to_string()}), &[]);
    }
    for sched in SCHEDULES {
        for steps in [0, 4, 20] {
            dr.emit(
                "fault_windows",
                json!({"schedule": sched, "steps": steps}),
                &[],
            );
        }
    }
    for lab in ["s", "end", "r", "0", " 7", "+3", "1_0", "x", "", "-2"] {
        dr.emit("label_rank", json!({"lab": lab, "steps": 20}), &[]);
    }
    let runs: Vec<Value> = sets
        .iter()
        .enumerate()
        .map(|(i, s)| json!([i + 1, SCHEDULES[i % SCHEDULES.len()], s]))
        .collect();
    let runs = Value::Array(runs);
    for k in &ls {
        dr.explore(&runs, k, &json!({}), None);
    }
    files
}

/// py_ref.py's `accept_cases`.
fn accept_cases(dr: &mut Driver, files: &std::collections::BTreeMap<&'static str, String>) {
    dr.section("accept");
    let src = vec![
        (
            "a".to_string(),
            he::read_text_lossy(Path::new(&files["odd.log"])).unwrap(),
        ),
        (
            "b".to_string(),
            he::read_text_lossy(Path::new(&files["rec.log"])).unwrap(),
        ),
    ];
    let all: Vec<Value> = he::parse_consoles(&src)
        .unwrap()
        .kinds
        .iter()
        .map(|k| Value::Object(k.to_json()))
        .collect();
    let has_q = |k: &Value| k["tpl"].as_str().unwrap().contains('"');
    let mut odd: Vec<Value> = all.iter().filter(|k| has_q(k)).cloned().collect();
    odd.push(json!({"sig": "f0c792251150", "svc": "redis", "tpl": "quote \" back \\ tab\t \u{e9}", "ex": ""}));
    let mut kinds: Vec<Value> = all.iter().filter(|k| !has_q(k)).cloned().collect();
    kinds.push(json!({"sig": "0123456789ab", "svc": "x", "tpl": "caf\u{e9} \u{2603} <date> #", "ex": "e".repeat(300)}));
    let sig_yaml = lists()[1].1.clone();
    let befores: Vec<Option<String>> = vec![
        None,
        Some(sig_yaml),
        Some("format: 1\nunknown: report\n".into()),
        Some("# just a comment".into()),
        Some("known:\r\n  - sig: f0c792251150\r\n    reason: x\r\n".into()),
        Some("known:\n  - sig: zz\n".into()),
    ];
    let k2 = [&kinds[..2], &kinds[..1]].concat();
    type Case<'a> = (
        Vec<Value>,
        Option<&'a str>,
        &'a str,
        Vec<&'a str>,
        Vec<&'a str>,
    );
    let cases: Vec<Case> = vec![
        (kinds.clone(), Some("expected"), "t", vec![], vec![]),
        (
            kinds[..1].to_vec(),
            Some("at boot"),
            "someone",
            vec!["C", "disk=ioerr"],
            vec![],
        ),
        (
            kinds[1..3].to_vec(),
            Some("while down"),
            "t",
            vec![],
            vec!["K=kafka"],
        ),
        (k2, Some("twice"), "t", vec!["K"], vec!["N=mysql"]),
        (kinds.clone(), Some("  "), "t", vec![], vec![]),
        (kinds.clone(), None, "t", vec![], vec![]),
        (kinds[..1].to_vec(), Some("x"), "t", vec!["K mysql"], vec![]),
        (odd.clone(), Some("odd"), "t", vec![], vec![]),
    ];
    let mut i = 0;
    for before in &befores {
        for (ks, reason, by, with_, during) in &cases {
            let d = dr.p(&format!("accept/{i}"));
            i += 1;
            dr.emit(
                "accept",
                json!({"before": before, "kinds": ks, "reason": reason, "by": by,
                       "with": with_, "during": during, "dir": d}),
                std::slice::from_ref(&d),
            );
        }
    }
}

/// py_ref.py's `for_run_dir_cases`.
fn for_run_dir_cases(dr: &mut Driver) {
    dr.section("for_run_dir");
    let con = CON.join("\n");
    let rec = REC.join("\n");
    let odd = ODD.join("\r\n");
    let conss = vec![
        json!({"one.log": con, "one.recovery.log": rec, "engine.log": con, "one.checks.log": con,
               "worker-0.log": con, ".hidden.log": con}),
        json!({ "one.log": odd }),
        json!({"x.log": "nothing"}),
        json!({"b.log": rec, "a.recovery.log": con, "a.log": EMPTY}),
    ];
    let mut i = 0;
    for cons in &conss {
        for known in [
            Value::Null,
            json!(dr.p("lists/match.yaml")),
            json!(dr.p("bad/regex.yaml")),
            json!(dr.p("lists/sig.yaml")),
        ] {
            for res in reses() {
                let d = dr.p(&format!("runs/{i}"));
                i += 1;
                dr.emit(
                    "for_run_dir",
                    json!({"files": cons, "schedule": SCHEDULES[1], "res": res, "known": known,
                           "dir": d}),
                    std::slice::from_ref(&d),
                );
            }
        }
    }
}

// ------------------------------------------------------------------ the recorded corpus

/// tag -> schedule for a run or exploration directory (py_ref.py's `schedules_of`).
fn schedules_of(d: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for name in ["runs.jsonl", "results.jsonl"] {
        if let Ok(t) = std::fs::read_to_string(d.join(name)) {
            for line in t.lines() {
                let Ok(r) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if let Some(tag) = r.get("tag") {
                    out.insert(tag.as_str().unwrap().to_string(), str_or(&r["schedule"]));
                } else if let Some(sd) = r.get("seed") {
                    out.insert(
                        format!("seed-{}", sd.as_i64().unwrap()),
                        str_or(&r["schedule"]),
                    );
                }
            }
        }
    }
    if let Ok(t) = std::fs::read_to_string(d.join("run.json")) {
        let r: Value = serde_json::from_str(&t).unwrap();
        out.insert(String::new(), str_or(&r["schedule"]));
    }
    out
}

fn str_or(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn all_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        let d = out[i].clone();
        for e in std::fs::read_dir(&d).unwrap().filter_map(Result::ok) {
            let ft = e.file_type().unwrap();
            if ft.is_dir() {
                out.push(e.path());
            }
        }
        i += 1;
    }
    // Python: sorted(os.walk(root)) -- by the directory's path string
    out.sort_by_key(|a| a.display().to_string());
    out
}

fn files_of(d: &Path) -> Vec<String> {
    let mut fs: Vec<String> = std::fs::read_dir(d)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| !e.file_type().unwrap().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    fs.sort();
    fs
}

/// py_ref.py's `corpus(root)`; returns (runs, explorations).
fn corpus_cases(dr: &mut Driver, root: &Path) -> (usize, usize) {
    dr.section("corpus");
    let known = root.join("known");
    let lists: Vec<Value> = vec![
        Value::Null,
        json!(known.join("redis-errors.yaml").display().to_string()),
        json!(known.join("stack.yaml").display().to_string()),
        json!(root.join("redis-final/fail.yaml").display().to_string()),
    ];
    let (mut nrun, mut nexp) = (0, 0);
    for d in all_dirs(root) {
        let fs = files_of(&d);
        if fs.iter().any(|f| f == "one.log") {
            let sched = schedules_of(&d)
                .get("")
                .cloned()
                .unwrap_or_else(|| "disk=none | run".into());
            let mut cons: Vec<String> = fs
                .iter()
                .filter(|f| {
                    f.ends_with(".log")
                        && !f.starts_with('.')
                        && !f.ends_with(".checks.log")
                        && !f.ends_with("engine.log")
                        && !f.starts_with("worker")
                })
                .map(|f| d.join(f).display().to_string())
                .collect();
            cons.sort();
            cons.sort_by_key(|p| p.ends_with(".recovery.log"));
            let srcs = Value::Array(
                cons.iter()
                    .map(|p| {
                        let n = Path::new(p)
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                        pair(&n, p)
                    })
                    .collect(),
            );
            for k in &lists {
                dr.analyse(&srcs, &sched, k, None);
            }
            nrun += 1;
        }
        if d.file_name().is_some_and(|n| n == "consoles") {
            let parent = d.parent().unwrap().to_path_buf();
            let scheds = schedules_of(&parent);
            let tags: std::collections::BTreeSet<String> = fs
                .iter()
                .filter(|f| f.ends_with(".log"))
                .map(|f| {
                    f.strip_suffix(".recovery.log")
                        .unwrap_or_else(|| &f[..f.len() - 4])
                        .to_string()
                })
                .collect();
            let srcs_of = |t: &str| -> Value {
                Value::Array(
                    [format!("{t}.log"), format!("{t}.recovery.log")]
                        .iter()
                        .map(|n| d.join(n))
                        .filter(|p| p.exists())
                        .map(|p| {
                            let n = p.file_name().unwrap().to_string_lossy().into_owned();
                            pair(&n, &p.display().to_string())
                        })
                        .collect(),
                )
            };
            for t in &tags {
                let srcs = srcs_of(t);
                let sched = scheds
                    .get(t)
                    .cloned()
                    .unwrap_or_else(|| "disk=none | run".into());
                for k in &lists[..3] {
                    dr.analyse(&srcs, &sched, k, None);
                }
            }
            let mut seeds: Vec<i64> = tags
                .iter()
                .filter_map(|t| t.strip_prefix("seed-"))
                .filter(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
                .map(|x| x.parse().unwrap())
                .collect();
            seeds.sort();
            if !seeds.is_empty() {
                let runs = Value::Array(
                    seeds
                        .iter()
                        .map(|sd| {
                            let tag = format!("seed-{sd}");
                            json!([
                                sd,
                                scheds.get(&tag).cloned().unwrap_or_default(),
                                srcs_of(&tag)
                            ])
                        })
                        .collect(),
                );
                let mut shrunk = Map::new();
                if let Ok(t) = std::fs::read_to_string(parent.join("errors.json")) {
                    let ej: Value = serde_json::from_str(&t).unwrap();
                    for k in ej["kinds"].as_array().cloned().unwrap_or_default() {
                        if k.get("shrunk").is_some_and(hydo_manifest::py::truthy) {
                            shrunk.insert(str_or(&k["sig"]), k["shrunk"].clone());
                        }
                    }
                }
                let label = parent.strip_prefix(root).unwrap().display().to_string();
                let label = if label.is_empty() { ".".into() } else { label };
                for k in &lists[..3] {
                    dr.explore(&runs, k, &Value::Object(shrunk.clone()), Some(&label));
                }
                nexp += 1;
            }
        }
    }
    (nrun, nexp)
}

/// Every file of the corpus with its sha256 (the "corpus files" section pins the copy).
fn corpus_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for d in all_dirs(root) {
        for f in files_of(&d) {
            let p = d.join(&f);
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            out.push(format!(
                "{rel} {}",
                golden::sha256_hex(&std::fs::read(&p).unwrap())
            ));
        }
    }
    out.sort();
    out
}

fn header() -> Vec<String> {
    vec![
        "# hydo-errors equivalence: SHA-256 of every 64 lines of the answer stream of".into(),
        "# hydo/crates/hydo-errors/tests/py_ref.py (hydo/onboard/hydo_errors.py), recorded at commit".into(),
        "# 2a58c4b with Python 3.15.0 on the Mac (the corpus rows with --corpus, a copy of the M94".into(),
        "# results); restore the script with `git show 2a58c4b:hydo/crates/hydo-errors/tests/py_ref.py`.".into(),
        "# Rows: section, chunk, lines, sha256.".into(),
    ]
}

fn paths(work: &Path, corpus: Option<&Path>) -> Vec<(String, &'static str)> {
    let mut v = vec![(work.display().to_string(), "<WORK>")];
    if let Some(c) = corpus {
        v.push((c.display().to_string(), "<CORPUS>"));
    }
    v.push((top().display().to_string(), "<TOP>"));
    v
}

fn rust_stream(work: &Path, corpus: Option<&Path>) -> golden::Sections {
    let mut dr = Driver {
        work: work.to_path_buf(),
        paths: paths(work, corpus),
        sections: Vec::new(),
        n: 0,
    };
    let files = synthetic(&mut dr);
    accept_cases(&mut dr, &files);
    for_run_dir_cases(&mut dr);
    let (mut nrun, mut nexp) = (0, 0);
    if let Some(c) = corpus {
        (nrun, nexp) = corpus_cases(&mut dr, c);
    }
    let summary = json!({"op": "summary", "args": {}, "out": {"ok": {"ops": dr.n,
                         "corpus_runs": nrun, "corpus_explorations": nexp}}});
    let line = canonical(&summary, &[], &dr.paths);
    let label = if corpus.is_some() {
        "summary with corpus"
    } else {
        "summary"
    };
    dr.sections.push((label.into(), vec![line]));
    if let Some(c) = corpus {
        dr.sections.push(("corpus files".into(), corpus_files(c)));
    }
    dr.sections
}

/// Sections of a saved py_ref.py stream: synthetic, accept, for_run_dir, corpus, summary.
fn python_sections(path: &str, pywork: &str, corpus: Option<&Path>) -> golden::Sections {
    let ps = paths(Path::new(pywork), corpus);
    let mut secs: golden::Sections = vec![("synthetic".into(), Vec::new())];
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let case: Value = serde_json::from_str(line).unwrap();
        let op = case["op"].as_str().unwrap();
        let cur = secs.last().unwrap().0.clone();
        let want = match (op, cur.as_str()) {
            ("summary", _) if corpus.is_some() => "summary with corpus",
            ("summary", _) => "summary",
            ("accept", _) => "accept",
            ("for_run_dir", _) => "for_run_dir",
            (_, "for_run_dir") | (_, "corpus") => "corpus",
            (_, c) => {
                if c == "synthetic" {
                    "synthetic"
                } else {
                    panic!("unexpected {op} after {c}")
                }
            }
        };
        if want != cur {
            secs.push((want.into(), Vec::new()));
        }
        let mut dirs = Vec::new();
        if op == "accept" || op == "for_run_dir" {
            // py_ref.py already wrote the case directory as <dir>
            dirs.push(case["args"]["dir"].as_str().unwrap().to_string());
        }
        let c = canonical(&case, &dirs, &ps);
        secs.last_mut().unwrap().1.push(c);
    }
    if let Some(c) = corpus {
        secs.push(("corpus files".into(), corpus_files(c)));
    }
    secs
}

fn is_corpus_label(l: &str) -> bool {
    l == "corpus" || l == "summary with corpus" || l == "corpus files"
}

#[test]
fn errors_match_recorded_python() {
    for (rel, sha) in PINS {
        assert_eq!(
            golden::sha256_hex(&std::fs::read(top().join(rel)).unwrap()),
            *sha,
            "{rel} changed since the recording; check the change, then re-record with HYDO_BLESS=1"
        );
    }
    let corpus = std::env::var("HYDO_ERRLOG_CORPUS")
        .ok()
        .map(|c| PathBuf::from(c).canonicalize().unwrap());
    if let (Ok(p), Ok(w)) = (
        std::env::var("HYDO_RECORD_PY"),
        std::env::var("HYDO_RECORD_WORK"),
    ) {
        let mut secs = python_sections(&p, &w, None);
        if let (Ok(pc), Some(c)) = (std::env::var("HYDO_RECORD_PY_CORPUS"), &corpus) {
            secs.extend(
                python_sections(&pc, &w, Some(c))
                    .into_iter()
                    .filter(|s| is_corpus_label(&s.0)),
            );
        }
        golden::write(&golden_path(), &header(), &golden::digest(&secs, CHUNK));
        eprintln!("recorded {}", golden_path().display());
    }
    let work = scratch("work");
    let sections = rust_stream(&work, corpus.as_deref());
    let _ = std::fs::remove_dir_all(&work);
    let with_corpus = corpus.is_some();
    golden::check(&golden_path(), &header(), &sections, CHUNK, &|l| {
        if with_corpus {
            l != "summary"
        } else {
            !is_corpus_label(l)
        }
    });
    let n: usize = sections
        .iter()
        .filter(|s| s.0 != "corpus files" && !s.0.starts_with("summary"))
        .map(|s| s.1.len())
        .sum();
    assert_eq!(n, if with_corpus { 4_757 } else { 826 });
    if !with_corpus {
        eprintln!("NOTE: HYDO_ERRLOG_CORPUS is not set: the M94 consoles were not compared");
    }
    eprintln!("errors equivalence: {n} cases identical to the recorded hydo_errors.py answers");
}

/// The files M94's runs wrote, recomputed in Rust from their consoles and the lists they used.
#[test]
fn recorded_files() {
    let Ok(root) = std::env::var("HYDO_ERRLOG_CORPUS") else {
        eprintln!(
            "SKIPPED: set HYDO_ERRLOG_CORPUS to a copy of the M94 results (see the module doc)"
        );
        return;
    };
    let root = PathBuf::from(root);
    let list_for = |recorded: &Value| -> Option<String> {
        let f = recorded.as_str()?;
        Some(
            if f.ends_with("redis-errors/hydo/known-errors.yaml") || f == "hydo/known-errors.yaml" {
                root.join("known/redis-errors.yaml").display().to_string()
            } else if f.ends_with("stack/known-errors.yaml") {
                root.join("known/stack.yaml").display().to_string()
            } else if f.ends_with("redis-final/fail.yaml") {
                root.join("redis-final/fail.yaml").display().to_string()
            } else {
                panic!("unknown recorded list {f}")
            },
        )
    };
    let (mut nrun, mut ntag, mut nexp, mut nmd) = (0, 0, 0, 0);
    let mut dirs = vec![root.clone()];
    while let Some(d) = dirs.pop() {
        let mut ents: Vec<PathBuf> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        ents.sort();
        dirs.extend(ents.iter().filter(|p| p.is_dir()).cloned());
        // hydo run: one.errors.json and the verdict in run.json
        let rec = d.join("one.errors.json");
        if rec.is_file() && d.join("one.log").is_file() {
            let want = std::fs::read_to_string(&rec).unwrap();
            let wj: Value = serde_json::from_str(&want).unwrap();
            let run: Value = std::fs::read_to_string(d.join("run.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let sched = run["schedule"].as_str().unwrap_or("disk=none | run");
            let mut k = he::load_known(list_for(&wj["known_file"]).as_deref()).unwrap();
            k.path = wj["known_file"].as_str().map(str::to_string);
            let out = root.join("..").join(format!("rec-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            for e in std::fs::read_dir(&d).unwrap().filter_map(Result::ok) {
                if e.path().is_file() && e.file_name().to_string_lossy().ends_with(".log") {
                    std::fs::copy(e.path(), out.join(e.file_name())).unwrap();
                }
            }
            let srcs = he::run_files(&out.join("one"));
            let an = he::analyse(&srcs, sched, &k).unwrap();
            let p = out.join("one.errors.json");
            he::write_run_json(&p, &an, &k).unwrap();
            assert_eq!(
                std::fs::read_to_string(&p).unwrap(),
                want,
                "{}",
                rec.display()
            );
            if run.get("errors_new").is_some() {
                let mut res = Map::new();
                res.insert(
                    "verdict".into(),
                    run.get("pre_errlog_verdict")
                        .or(run.get("verdict"))
                        .cloned()
                        .unwrap(),
                );
                he::apply(&mut res, Some(&an), &k);
                assert_eq!(res["verdict"], run["verdict"], "{}", d.display());
                assert_eq!(res["errors_new"], run["errors_new"], "{}", d.display());
                assert_eq!(res["errors_known"], run["errors_known"], "{}", d.display());
            }
            let _ = std::fs::remove_dir_all(&out);
            nrun += 1;
        }
        // an exploration: consoles/<tag>.errors.json, errors.json, report.md's section
        let cons = d.join("consoles");
        let ej = d.join("errors.json");
        if cons.is_dir() && ej.is_file() {
            let ejv: Value = serde_json::from_str(&std::fs::read_to_string(&ej).unwrap()).unwrap();
            let mut k = he::load_known(list_for(&ejv["known_file"]).as_deref()).unwrap();
            k.path = ejv["known_file"].as_str().map(str::to_string);
            let mut sched_of = std::collections::BTreeMap::new();
            for l in std::fs::read_to_string(d.join("runs.jsonl"))
                .unwrap()
                .lines()
            {
                let r: Value = serde_json::from_str(l).unwrap();
                sched_of.insert(
                    format!("seed-{}", r["seed"]),
                    (
                        r["seed"].clone(),
                        r["schedule"].as_str().unwrap().to_string(),
                        r,
                    ),
                );
            }
            let mut runs = Vec::new();
            let mut seeds: Vec<&(Value, String, Value)> = sched_of.values().collect();
            seeds.sort_by_key(|x| x.0.as_i64());
            for (seed, sched, r) in seeds {
                let tag = format!("seed-{seed}");
                let an = he::analyse(&he::run_files(&cons.join(&tag)), sched, &k);
                let rec = cons.join(format!("{tag}.errors.json"));
                if let Some(an) = &an {
                    let want = std::fs::read_to_string(&rec).unwrap();
                    assert_eq!(
                        json_dumps_indent(&an.to_json(), 1, false),
                        want,
                        "{}",
                        rec.display()
                    );
                    let mut res = Map::new();
                    res.insert("verdict".into(), json!("HYDO-ORACLE: OK x"));
                    he::apply(&mut res, Some(an), &k);
                    assert_eq!(res["errors_new"], r["errors_new"], "{tag}");
                    assert_eq!(res["errors_known"], r["errors_known"], "{tag}");
                    ntag += 1;
                } else {
                    assert!(!rec.exists());
                }
                runs.push((seed.clone(), sched.clone(), an));
            }
            let agg = he::collect(&runs);
            let mut shrunk = Map::new();
            let mut kinds = Vec::new();
            for (a, rec) in agg.iter().zip(ejv["kinds"].as_array().unwrap()) {
                let mut m = a.to_json();
                m.insert("new".into(), json!(a.new_runs > 0));
                m.insert("shrunk".into(), rec["shrunk"].clone());
                if !rec["shrunk"].is_null() {
                    shrunk.insert(a.sig.clone(), rec["shrunk"].clone());
                }
                kinds.push(Value::Object(m));
            }
            let mine = json!({"known_file": ejv["known_file"], "known_sha256": k.sha256,
                              "unknown": k.unknown, "kinds": kinds});
            assert_eq!(
                json_dumps_indent(&mine, 1, false),
                std::fs::read_to_string(&ej).unwrap(),
                "{}",
                ej.display()
            );
            if let Ok(md) = std::fs::read_to_string(d.join("report.md")) {
                nmd += 1;
                let start = md.find("## Error logs").unwrap();
                let rest = &md[start..];
                let end = rest[3..].find("\n## ").map(|i| i + 4).unwrap_or(rest.len());
                let want: Vec<&str> = rest[..end].trim_end_matches('\n').split('\n').collect();
                // the explorer's "by hand" line is the explorer's: taken from the recorded report
                let by_hand = |s: &str| -> String {
                    want.iter()
                        .find_map(|l| {
                            l.strip_prefix("- by hand: ")
                                .filter(|x| x.contains(&format!("\"{s}\"")))
                        })
                        .unwrap()
                        .to_string()
                };
                let got = he::report_md(&agg, &k, &shrunk, &by_hand);
                let got: Vec<String> = got
                    .join("\n")
                    .trim_end_matches('\n')
                    .split('\n')
                    .map(str::to_string)
                    .collect();
                assert_eq!(got, want, "{}", d.display());
            }
            nexp += 1;
        }
    }
    assert!(nrun > 0 && nexp > 0);
    eprintln!(
        "recorded M94 files: {nrun} one.errors.json (+ run.json verdicts), {ntag} consoles/<tag>.errors.json, \
         {nexp} errors.json and {nmd} report.md sections identical"
    );
}
