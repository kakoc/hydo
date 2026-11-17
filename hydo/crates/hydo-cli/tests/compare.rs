//! `hydo compare-versions` (Milestone 298) end to end on the scripted engine
//! (`HYDO_SCRIPTED_ENGINE`): the program below builds a stand-in image for each version of a
//! manifest parameter and answers each run with a console written into the run directory. From
//! version 2 on, a schedule with a power cut (`C=`) loses the acknowledgment of write 3. The
//! state dump names the version in a line that only a normalization rule makes equal. No VM runs.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

const ENGINE: &str = r#"#!/bin/sh
set -eu
case $1 in
build)   # build <manifest> <out dir> KEY=VALUE...
  out=$3; shift 3; v=
  for a in "$@"; do case $a in version=*) v=${a#version=};; esac; done
  echo "$v" >> "$LOG/builds"
  echo "version=$v" > "$out/app-$v-disk.img"
  cp "$out/app-$v-disk.img" "$out/app-$v-arm64.cpio.gz";;
one)     # one <run dir> <schedule>
  d=$2; s=$3
  v=$(sed -n 's/^version=//p' "$HYDO_SCRIPTED_IMAGE")
  echo "$v $s" >> "$LOG/runs"
  lost=0
  case "$s" in *C=*) [ "$v" -ge 2 ] && lost=1;; esac
  {
    echo "HYDO-HIST kv p1 invoke $((v * 100)) write k1 v1"
    echo "HYDO-HIST kv p1 ok $((v * 100 + 7)) write k1 v1"
    echo "hydo-ob: workload steps 0..3 rc=0"
    if [ $lost = 1 ]; then echo "hydo-explore-acked: 0 1 2"; else echo "hydo-explore-acked: 0 1 2 3"; fi
    echo "HYDO-STATE kv: built-by=v$v"
    echo "HYDO-STATE kv: k:0 v0"
    if [ $lost = 0 ]; then echo "HYDO-STATE kv: k:3 v3"; fi
    echo "HYDO-STATE-END kv rc=0 lines=3 cut=0"
  } > "$d/one.log"
  if [ $lost = 1 ]; then
    verdict="HYDO-ORACLE: VIOLATION [app.acked_writes_survive] key k:3 lost"
  else
    verdict="HYDO-ORACLE: OK invariants=1 held"
  fi
  echo "RESULT one fp=$(printf %016x $((v * 4097))) wfp=0 secs=0.1 verdict=$verdict";;
esac
"#;

const MANIFEST: &str = r#"format: 1
name: app
description: a stand-in system whose versions are built from a parameter
hydo_version: "0.3"
params:
  version: "1"
root:
  files:
    - {src: "app-{{version}}", dst: /bin/app, mode: "0755"}
workload:
  steps: 4
  timeout: 60
  run: "echo ok"
invariants:
  - id: app.acked_writes_survive
    msg: every acknowledged write is read back
    cmd: "true"
"#;

struct T {
    dir: PathBuf,
}

impl T {
    fn new(name: &str) -> T {
        let dir = std::env::temp_dir().join(format!("hydo-compare-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj/hydo")).unwrap();
        std::fs::create_dir_all(dir.join("log")).unwrap();
        std::fs::write(dir.join("proj/hydo/hydo.yaml"), MANIFEST).unwrap();
        let e = dir.join("engine.sh");
        std::fs::write(&e, ENGINE).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755)).unwrap();
        T { dir }
    }

    fn hydo(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hydo"));
        c.args(args)
            .current_dir(self.dir.join("proj"))
            .env("HYDO_SCRIPTED_ENGINE", self.dir.join("engine.sh"))
            .env("LOG", self.dir.join("log"))
            .env("HYDO_HOME", self.dir.join("home"))
            .env_remove("HYDO_BACKEND");
        for (k, v) in env {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        (o.status.code().unwrap_or(-1), text)
    }

    fn compare(&self, extra: &[&str], env: &[(&str, &str)]) -> (i32, Value, String) {
        let mut a = vec![
            "compare-versions",
            "--manifest",
            "hydo/hydo.yaml",
            "--out",
            "out",
            "--cache",
            "../cache",
        ];
        a.extend_from_slice(extra);
        let (rc, text) = self.hydo(&a, env);
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(self.dir.join("proj/out/compare.json"))
                .unwrap_or_else(|e| panic!("no compare.json: {e}\n{text}")),
        )
        .unwrap();
        (rc, doc, text)
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn one_version_against_itself_reports_no_difference() {
    let t = T::new("same");
    let (rc, doc, text) = t.compare(&["disk=cut | run C=3", "--param", "version=2,2"], &[]);
    assert_eq!(rc, 0, "{text}");
    assert_eq!(doc["type"], "compare");
    let s = &doc["schedules"][0];
    assert_eq!(s["identical"], true, "{text}");
    assert!(
        s["summary"]
            .as_str()
            .unwrap()
            .contains("the same fingerprint"),
        "{text}"
    );
    assert!(doc["summary"]
        .as_str()
        .unwrap()
        .starts_with("no difference"));
}

#[test]
fn a_change_shows_its_first_difference_and_the_rules_blank_what_may_differ() {
    let t = T::new("differ");
    // without a rule the build's own line differs on every schedule
    let (rc, doc, text) = t.compare(&["disk=none | run", "--param", "version=1,2"], &[]);
    assert_eq!(rc, 1, "{text}");
    let d = &doc["schedules"][0]["differences"];
    assert_eq!(d.as_array().unwrap().len(), 1, "{text}");
    assert_eq!(d[0]["stream"], "state kv");
    assert_eq!(d[0]["a"], "built-by=v1");
    // with it, the schedule without a power cut agrees, and the one with a cut does not
    let (rc, doc, text) = t.compare(
        &[
            "disk=none | run",
            "disk=cut | run C=3",
            "--param",
            "version=1,2",
            "--ignore",
            r"built-by=\S+",
        ],
        &[],
    );
    assert_eq!(rc, 1, "{text}");
    let s = doc["schedules"].as_array().unwrap();
    assert_eq!(s[0]["identical"], true, "{text}");
    assert_eq!(s[1]["identical"], false, "{text}");
    let first = &s[1]["differences"][s[1]["first"].as_u64().unwrap() as usize];
    assert_eq!(first["level"], "observations", "{text}");
    assert_eq!(first["stream"], "acked");
    assert_eq!(first["a"], "3");
    assert_eq!(first["step_a"], "0..3");
    let streams: Vec<&str> = s[1]["differences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["stream"].as_str().unwrap())
        .collect();
    assert_eq!(streams, ["acked", "state kv", "verdict"], "{text}");
    assert!(
        s[1]["summary"]
            .as_str()
            .unwrap()
            .contains("the client saw `3` acknowledged at workload steps 0..3 in A (1) only"),
        "{text}"
    );
    assert_eq!(doc["normalize"][0], r"regex built-by=\S+ => <ignored>");
    // hydo report prints it, and its web page names the change
    let (rc, rep) = t.hydo(&["report", "out"], &[]);
    assert_eq!(rc, 0, "{rep}");
    assert!(rep.contains("1 and 2 differ on 1 of 2 schedules"), "{rep}");
    let (rc, h) = t.hydo(&["report", "--html", "out"], &[]);
    assert_eq!(rc, 0, "{h}");
    let page = std::fs::read_to_string(t.dir.join("proj/out/report.html")).unwrap();
    assert!(page.contains("2 behaves differently from 1"), "{page}");
    // the levels are optional: the verdicts alone
    let (_, doc, _) = t.compare(
        &[
            "disk=cut | run C=3",
            "--param",
            "version=1,2",
            "--levels",
            "verdicts",
        ],
        &[],
    );
    let d = doc["schedules"][0]["differences"].as_array().unwrap();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["stream"], "verdict");
    assert_eq!(d[0]["b"], "violation app.acked_writes_survive");
}

#[test]
fn a_search_draws_schedules_and_shrinks_each_difference() {
    let t = T::new("search");
    // the explorer draws from the built-in fault space when the scripted engine runs
    let env = [("HYDO_CLI_PLATFORM", "linux"), ("HYDO_BACKEND", "hvx")];
    let (rc, doc, text) = t.compare(
        &[
            "--seeds",
            "1-12",
            "--param",
            "version=1,2",
            "--ignore",
            r"built-by=\S+",
        ],
        &env,
    );
    assert_eq!(rc, 1, "{text}");
    let sr = &doc["search"];
    assert_eq!(sr["drawn"], 12, "{text}");
    let differ = sr["differ"].as_u64().unwrap();
    assert!(differ >= 1, "{text}");
    let s = doc["schedules"].as_array().unwrap();
    for x in s.iter().filter(|x| x["identical"] == false) {
        assert!(x["schedule"].as_str().unwrap().contains("C="), "{x}");
    }
    let mins: Vec<&Value> = s.iter().filter(|x| x["shrunk_from"].is_string()).collect();
    assert!(!mins.is_empty(), "{text}");
    for m in mins {
        let min = m["schedule"].as_str().unwrap();
        let from = m["shrunk_from"].as_str().unwrap();
        assert!(min.contains("C="), "{min}");
        let words = |x: &str| x.split_whitespace().count();
        assert!(words(min) <= words(from), "{min} from {from}");
        assert_eq!(m["identical"], false);
    }
}
