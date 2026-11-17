//! `hydo explore --configs` (Milestone 299) end to end on the scripted engine
//! (`HYDO_SCRIPTED_ENGINE`): the program below builds a stand-in image for each value of the
//! manifest's setting `mode`, and answers each run from the value written in the image
//! (`HYDO_SCRIPTED_IMAGE`): with `mode=safe` every schedule passes, with the other values a
//! schedule with a power cut (`C=`) loses acknowledged writes. No VM runs.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

const ENGINE: &str = r#"#!/bin/sh
set -eu
answer() {   # answer <tag> <schedule>
  m=$(sed -n 's/^mode=//p' "$HYDO_SCRIPTED_IMAGE")
  verdict="HYDO-ORACLE: OK invariants=1 held"
  case "$m:$2" in
    safe:*) ;;
    *C=*) verdict="HYDO-ORACLE: VIOLATION [db.acked_writes_survive] lost=2 of 5 acknowledged writes" ;;
  esac
  fp=$(printf '%s %s' "$m" "$2" | cksum | cut -d' ' -f1)
  echo "RESULT $1 fp=$(printf %016x "$fp") wfp=0 secs=0.1 verdict=$verdict"
}
case $1 in
build)   # build <manifest> <out dir> KEY=VALUE...
  out=$3; shift 3; m=safe
  for a in "$@"; do case $a in mode=*) m=${a#mode=};; esac; done
  echo "$m" >> "$LOG/builds"
  echo "mode=$m" > "$out/db-$m-disk.img"
  cp "$out/db-$m-disk.img" "$out/db-$m-arm64.cpio.gz";;
serve)   # serve <consoles dir>
  echo "READY boot_secs=0.1"
  while read -r tag sched; do
    : > "$2/$tag.log"
    answer "$tag" "$sched"
  done;;
one)     # one <consoles dir> <schedule>
  : > "$2/one.log"
  answer one "$3";;
esac
"#;

const MANIFEST: &str = r#"format: 1
name: db
description: a stand-in store whose durability is a build parameter
hydo_version: "0.3"
params:
  mode: "safe"
configs:
  mode:
    values: [lazy, lazier]
    service: db
    doc: "docs/durability.md"
    tradeoff:
      lazy: "acknowledges before the write is on disk: a power cut can lose the last second"
      lazier: "never syncs: a power cut can lose every write since the last writeback"
root:
  text:
    /etc/db.conf: "mode {{mode}}"
services:
  - name: db
    cmd: "sleep 1000"
    ready: "true"
workload:
  steps: 5
  timeout: 60
  run: "echo ok"
invariants:
  - id: db.acked_writes_survive
    msg: every acknowledged write is read back
    cmd: "true"
faults: [cut]
"#;

struct T {
    dir: PathBuf,
}

impl T {
    fn new(name: &str) -> T {
        let dir = std::env::temp_dir().join(format!("hydo-configs-{}-{name}", std::process::id()));
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

    fn hydo(&self, args: &[&str]) -> (i32, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .args(args)
            .current_dir(self.dir.join("proj"))
            .env("HYDO_CLI_PLATFORM", "mac")
            .env("HYDO_SCRIPTED_ENGINE", self.dir.join("engine.sh"))
            .env("LOG", self.dir.join("log"))
            .env_remove("HYDO_BACKEND")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        (o.status.code().unwrap_or(-1), text)
    }

    fn explore(&self, extra: &[&str]) -> (Value, String) {
        let mut a = vec![
            "explore",
            "--configs",
            "--out",
            "out",
            "--cache",
            "../cache",
            "--seeds",
            "1-10",
            "--workers",
            "1",
            "--no-diagnose",
        ];
        a.extend_from_slice(extra);
        let (rc, text) = self.hydo(&a);
        assert_eq!(rc, 0, "{text}");
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(self.dir.join("proj/out/configs.json")).unwrap(),
        )
        .unwrap();
        (doc, text)
    }

    fn builds(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log/builds"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn each_value_is_explored_and_a_failure_only_with_tradeoff_values_is_by_design() {
    let t = T::new("table");
    let (doc, text) = t.explore(&[]);
    assert_eq!(doc["type"], "configs");
    assert_eq!(doc["mode"], "all");
    assert_eq!(doc["seeds"], "1-10");
    assert_eq!(t.builds(), vec!["safe", "lazy", "lazier"], "{text}");
    let combos = doc["combinations"].as_array().unwrap();
    let labels: Vec<&str> = combos
        .iter()
        .map(|c| c["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["mode=safe", "mode=lazy", "mode=lazier"]);
    assert_eq!(combos[0]["defaults"], true);
    for c in combos {
        assert_eq!(c["status"], "explored", "{c}");
        assert_eq!(c["schedules"], 10, "{c}");
    }
    assert_eq!(combos[0]["failed"], 0, "{text}");
    assert!(combos[0]["classes"].as_array().unwrap().is_empty());
    for c in &combos[1..] {
        let k = &c["classes"][0];
        let s = k["schedule"]
            .as_str()
            .unwrap_or_else(|| panic!("{c}\n{text}"));
        assert!(s.contains("C="), "{c}");
        assert!(
            k["replay"]
                .as_str()
                .unwrap()
                .starts_with("hydo run --image "),
            "{c}"
        );
    }
    let f = doc["findings"].as_array().unwrap();
    assert_eq!(f.len(), 1, "{text}");
    assert_eq!(f[0]["by_design"], true);
    assert_eq!(f[0]["not_found_with"][0], "mode=safe");
    let only: Vec<&str> = f[0]["only_with"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["value"].as_str().unwrap())
        .collect();
    assert_eq!(only, vec!["lazy", "lazier"]);
    assert!(
        text.contains("found only with mode=lazy, lazier; not with mode=safe"),
        "{text}"
    );
    assert!(
        text.contains("by design: the setting's documented trade-off, not a bug"),
        "{text}"
    );
    assert!(
        text.contains("The defaults passed every schedule."),
        "{text}"
    );
    // each combination's exploration is a whole exploration directory
    let d = t
        .dir
        .join("proj/out")
        .join(combos[1]["dir"].as_str().unwrap());
    assert!(d.join("exploration.json").is_file());

    // a second run builds nothing: every image comes from the cache by its input key
    let (doc2, _) = t.explore(&[]);
    assert_eq!(t.builds().len(), 3);
    for c in doc2["combinations"].as_array().unwrap() {
        assert_eq!(c["image_reused"], true, "{c}");
    }
    assert_eq!(doc2["findings"], doc["findings"]);

    // hydo report prints the table, --html writes a page
    let (rc, rep) = t.hydo(&["report", "out"]);
    assert_eq!(rc, 0, "{rep}");
    assert!(rep.contains("mode=lazier"), "{rep}");
    assert!(rep.contains("documented trade-off"), "{rep}");
    let (rc, h) = t.hydo(&["report", "--html", "out"]);
    assert_eq!(rc, 0, "{h}");
    let page = std::fs::read_to_string(t.dir.join("proj/out/report.html")).unwrap();
    assert!(
        page.contains("the setting&#39;s documented trade-off, not a bug")
            || page.contains("the setting's documented trade-off, not a bug"),
        "{page}"
    );
    assert!(page.contains("mode=lazier"));
}

#[test]
fn one_at_a_time_and_refusals() {
    let t = T::new("one");
    let (doc, _) = t.explore(&["--configs-mode", "one"]);
    assert_eq!(doc["mode"], "one");
    assert_eq!(doc["combinations"].as_array().unwrap().len(), 3);
    // a setting cannot also be fixed with --set
    let (rc, text) = t.hydo(&["explore", "--configs", "--set", "mode=lazy"]);
    assert_ne!(rc, 0);
    assert!(text.contains("a setting that configs: explores"), "{text}");
    let (rc, text) = t.hydo(&["explore", "--configs", "--configs-mode", "every"]);
    assert_ne!(rc, 0);
    assert!(text.contains("all, one or pairs"), "{text}");
    let (rc, text) = t.hydo(&["explore", "--configs", "--help"]);
    assert_eq!(rc, 0);
    assert!(text.contains("hydo explore --configs"), "{text}");
}
