//! `hydo findings` (Milestone 365) end to end on the scripted engine (`HYDO_SCRIPTED_ENGINE`):
//! the program below answers every schedule from its text and from `MODE`. With `MODE=lazy` a
//! schedule with a power cut (`C=`) loses acknowledged writes; with `MODE=safe` every schedule
//! passes. An exploration records its finding into the store (`--record-findings`), a second
//! record of the same folder changes nothing, a second exploration of a changed image does not
//! find it (it stays open), a run of its minimal schedule that passes proves it fixed, and a run
//! that fails brings it back. `hydo findings list`, `show` and `history` read the line, `hydo ci
//! page` shows it, and the store holds no path of this machine. No VM runs.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const ENGINE: &str = r#"#!/bin/sh
set -eu
answer() {   # answer <tag> <schedule>
  verdict="HYDO-ORACLE: OK invariants=1 held"
  case "$MODE:$2" in
    safe:*) ;;
    *C=*) verdict="HYDO-ORACLE: VIOLATION [db.acked_writes_survive] lost=2 of 5 acknowledged writes" ;;
  esac
  fp=$(printf '%s %s' "$MODE" "$2" | cksum | cut -d' ' -f1)
  echo "RESULT $1 fp=$(printf %016x "$fp") wfp=0 secs=0.1 verdict=$verdict"
}
case $1 in
serve)
  echo "READY boot_secs=0.1"
  while read -r tag sched; do
    : > "$2/$tag.log"
    answer "$tag" "$sched"
  done;;
one)
  : > "$2/one.log"
  answer one "$3";;
esac
"#;

const MANIFEST: &str = r#"format: 1
name: db
description: a stand-in store that loses acknowledged writes at a power cut
hydo_version: "0.3"
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
        let dir = std::env::temp_dir().join(format!("hydo-findings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj/hydo")).unwrap();
        std::fs::write(dir.join("proj/hydo/hydo.yaml"), MANIFEST).unwrap();
        std::fs::write(
            dir.join("proj/db-arm64.cpio.gz"),
            "a stand-in image, lazy\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("proj/db2-arm64.cpio.gz"),
            "a stand-in image, safe\n",
        )
        .unwrap();
        let e = dir.join("engine.sh");
        std::fs::write(&e, ENGINE).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755)).unwrap();
        T { dir }
    }

    fn store(&self) -> PathBuf {
        self.dir.join("store")
    }

    /// The exit status, stdout and stderr of `hydo <args>` in the project with `MODE=mode`.
    fn hydo(&self, mode: &str, args: &[&str]) -> (i32, String, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .args(args)
            .current_dir(self.dir.join("proj"))
            .env("HYDO_CLI_PLATFORM", "mac")
            .env("HYDO_SCRIPTED_ENGINE", self.dir.join("engine.sh"))
            .env("HYDO_HOME", self.dir.join("home"))
            .env("HYDO_FINDINGS_STORE", self.store())
            .env("MODE", mode)
            .env_remove("HYDO_BACKEND")
            .output()
            .unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn explore(&self, mode: &str, image: &str, out: &str) -> String {
        let (rc, out_text, err) = self.hydo(
            mode,
            &[
                "explore",
                "--image",
                image,
                "--manifest",
                "hydo/hydo.yaml",
                "--seeds",
                "1-10",
                "--workers",
                "1",
                "--out",
                out,
                "--no-diagnose",
                "--json",
                "--record-findings",
            ],
        );
        assert_eq!(rc, 0, "{out_text}{err}");
        // a program reads stdout: the record's line is on stderr
        assert!(!out_text.contains("hydo findings"), "{out_text}");
        err
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut a = vec!["findings"];
        a.extend_from_slice(args);
        a.push("--json");
        let (rc, out, err) = self.hydo("safe", &a);
        assert_eq!(rc, 0, "{out}{err}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    /// A run directory as `hydo run` writes it (its result.json), of `schedule` on `image`.
    fn run_dir(&self, name: &str, image: &str, schedule: &str, ok: bool) -> String {
        let d = self.dir.join("proj").join(name);
        std::fs::create_dir_all(&d).unwrap();
        let text = if ok {
            "HYDO-ORACLE: OK invariants=1 held"
        } else {
            "HYDO-ORACLE: VIOLATION [db.acked_writes_survive] lost=2 of 5 acknowledged writes"
        };
        let doc = json!({
            "v": 1, "type": "run", "hydo_version": "0.6.0", "epoch": 6,
            "schedule": schedule,
            "image": {"path": self.dir.join("proj").join(image).display().to_string(),
                      "sha256": format!("{:0>64}", image.len())},
            "engine": {"backend": "hvm", "args": ["--hvm-kernel", self.dir.join("k").display().to_string()]},
            "verdict": {"kind": if ok { "ok" } else { "violation" }, "text": text},
            "fingerprints": {"fp": if ok { "00000000000000aa" } else { "00000000000000bb" }},
            "findings": []
        });
        std::fs::write(d.join("result.json"), doc.to_string()).unwrap();
        name.to_string()
    }

    /// Every file of the store, with its text.
    fn store_files(&self) -> Vec<(PathBuf, String)> {
        fn walk(d: &Path, out: &mut Vec<(PathBuf, String)>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push((p.clone(), std::fs::read_to_string(&p).unwrap_or_default()));
                }
            }
        }
        let mut v = Vec::new();
        walk(&self.store(), &mut v);
        v
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn events(f: &Value) -> Vec<String> {
    f["sightings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["event"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_finding_is_recorded_once_and_its_history_follows_runs_and_explorations() {
    let t = T::new("line");
    // 1. an exploration records its finding
    let err = t.explore("lazy", "db-arm64.cpio.gz", "e1");
    assert!(
        err.contains("hydo findings: recorded local:")
            && err.contains("of db on local")
            && err.contains("new VIOLATION [db.acked_writes_survive]"),
        "{err}"
    );
    // 2. the same folder again: nothing changes
    let (rc, out, _) = t.hydo("lazy", &["findings", "record", "e1"]);
    assert_eq!(rc, 0);
    assert!(
        out.contains("is already in") && out.contains("nothing changed"),
        "{out}"
    );
    let d = t.json(&["list"]);
    assert_eq!(d["type"], "findings");
    assert_eq!(d["view"], "list");
    let fs = d["findings"].as_array().unwrap();
    assert_eq!(fs.len(), 1, "{d}");
    let f = &fs[0];
    assert_eq!(f["project"], "db");
    assert_eq!(f["branch"], "local");
    assert_eq!(f["state"], "open");
    assert_eq!(f["seen"], 1);
    assert_eq!(f["kind"], "invariant");
    assert!(f["key"].as_str().unwrap().starts_with("f1:invariant:"));
    assert!(
        f["verdict"].as_str().unwrap().contains("lost=2 of 5"),
        "{f}"
    );
    assert!(f["engine"].as_str().unwrap().starts_with("hvm/"), "{f}");
    assert!(f.get("sightings").is_none(), "a list has no sightings: {f}");
    let key = f["key"].as_str().unwrap().to_string();
    let minimal = f["minimal"].as_str().unwrap().to_string();
    assert!(minimal.contains("C="), "{minimal}");
    assert_eq!(
        f["repro"].as_str().unwrap(),
        format!("hydo run db-arm64.cpio.gz '{minimal}'")
    );

    // 3. another exploration of the lazy image under another folder: one more sighting
    let err = t.explore("lazy", "db-arm64.cpio.gz", "e1b");
    assert!(err.contains("seen again VIOLATION"), "{err}");
    // 4. an exploration of a changed image that finds nothing: the finding stays open
    let err = t.explore("safe", "db2-arm64.cpio.gz", "e2");
    assert!(err.contains(": no finding"), "{err}");
    let d = t.json(&["list"]);
    assert_eq!(d["findings"][0]["state"], "open");
    assert_eq!(d["findings"][0]["seen"], 2);

    // 5. a run of the minimal schedule on the changed image passes: fixed by replay
    let r1 = t.run_dir("r1", "db2-arm64.cpio.gz", &minimal, true);
    let (rc, out, err) = t.hydo("safe", &["findings", "record", &r1]);
    assert_eq!(rc, 0, "{err}");
    assert!(out.contains("fixed by replay VIOLATION"), "{out}");
    // a passing run of another schedule proves nothing
    let r0 = t.run_dir("r0", "db2-arm64.cpio.gz", "disk=none | run", true);
    let (_, out, _) = t.hydo("safe", &["findings", "record", &r0]);
    assert!(out.contains(": no finding"), "{out}");
    let d = t.json(&["list"]);
    assert!(d["findings"].as_array().unwrap().is_empty(), "{d}");
    let d = t.json(&["list", "--state", "fixed"]);
    assert_eq!(d["findings"][0]["state"], "fixed");

    // 6. it fails again: back again
    let r2 = t.run_dir("r2", "db-arm64.cpio.gz", &minimal, false);
    let (_, out, _) = t.hydo("lazy", &["findings", "record", &r2]);
    assert!(out.contains("back again VIOLATION"), "{out}");

    // 7. its history, by a part of its key
    let part = &key[key.len() - 6..];
    let d = t.json(&["history", part]);
    let f = &d["findings"][0];
    assert_eq!(
        events(f),
        ["new", "failing", "not-found", "fixed", "back"],
        "{f}"
    );
    assert_eq!(f["state"], "open");
    assert_eq!(f["reappeared"], true);
    assert_eq!(f["seen"], 3);
    let s = &f["sightings"].as_array().unwrap();
    assert_eq!(s[0]["by"], "hydo explore");
    assert_eq!(s[3]["by"], "hydo run");
    assert_eq!(s[3]["proof"], "head-replay-ok");
    assert_eq!(s[3]["fp"], "00000000000000aa");
    // the text views
    let (_, out, _) = t.hydo("safe", &["findings", "history", part]);
    assert!(
        out.contains("first seen")
            && out.contains("not found (still open)")
            && out.contains("fixed: its schedule passed")
            && out.contains("back again after a fix"),
        "{out}"
    );
    let (_, out, _) = t.hydo("safe", &["findings", "show", &key]);
    assert!(
        out.contains("verdict: HYDO-ORACLE: VIOLATION") && out.contains("replay: hydo run"),
        "{out}"
    );
    let (_, out, _) = t.hydo("safe", &["findings", "list", "--since", "2999-01-01"]);
    assert!(out.contains("has no open findings"), "{out}");
    let d = t.json(&["list", "--class", "no-such-class"]);
    assert!(d["findings"].as_array().unwrap().is_empty());

    // 8. hydo ci page reads the same store
    let (rc, out, err) = t.hydo(
        "safe",
        &["ci", "page", "--store", t.store().to_str().unwrap()],
    );
    assert_eq!(rc, 0, "{out}{err}");
    let page = std::fs::read_to_string(t.store().join("index.html")).unwrap();
    assert!(
        page.contains("hydo explore")
            && page.contains("hydo run")
            && page.contains("db.acked_writes_survive"),
        "the page shows the local records"
    );

    // 9. the store keeps no path of this machine
    let here = t.dir.display().to_string();
    let real = t.dir.canonicalize().unwrap().display().to_string();
    for (p, text) in t.store_files() {
        if p.extension().is_some_and(|x| x == "html") {
            continue;
        }
        assert!(
            !text.contains(&here) && !text.contains(&real) && !text.contains("--hvm-kernel"),
            "{} holds a path of this machine:\n{text}",
            p.display()
        );
    }
}

#[test]
fn an_empty_store_reads_as_empty() {
    let t = T::new("empty");
    let d = t.json(&["list"]);
    assert_eq!(d["findings"].as_array().unwrap().len(), 0);
    let (rc, out, _) = t.hydo("safe", &["findings", "list"]);
    assert_eq!(rc, 0);
    assert!(out.contains("does not exist yet"), "{out}");
    let (rc, _, err) = t.hydo("safe", &["findings", "record", "nowhere"]);
    assert_ne!(rc, 0);
    assert!(err.contains("nowhere"), "{err}");
}
