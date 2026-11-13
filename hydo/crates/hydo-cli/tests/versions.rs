//! `hydo versions` (Milestone 221) end to end on the scripted engine (`HYDO_SCRIPTED_ENGINE`): the
//! program below builds a stand-in image for each version of a manifest parameter, and answers
//! each replay from the version written in the image (`HYDO_SCRIPTED_IMAGE`). The bug appears at
//! version `BUG_FROM` (or at the versions `PATTERN` lists), and version `NO_BUILD` does not
//! build. No VM runs.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const ENGINE: &str = r#"#!/bin/sh
set -eu
case $1 in
build)   # build <manifest> <out dir> KEY=VALUE...
  out=$3; shift 3; v=
  for a in "$@"; do case $a in version=*) v=${a#version=};; esac; done
  # a git checkout: its VERSION file, which the build command copied to built-version
  [ -z "$v" ] && v=$(cat built-version)
  echo "$v" >> "$LOG/builds"
  if [ "$v" = "${NO_BUILD:-}" ]; then echo "error: no release $v to fetch" >&2; exit 1; fi
  echo "version=$v" > "$out/app-$v-disk.img"
  cp "$out/app-$v-disk.img" "$out/app-$v-arm64.cpio.gz";;
one)     # one <consoles dir> <schedule>
  v=$(sed -n 's/^version=//p' "$HYDO_SCRIPTED_IMAGE")
  echo "$v" >> "$LOG/runs"
  bad=0
  if [ -n "${PATTERN:-}" ]; then
    case ",$PATTERN," in *",$v,"*) bad=1;; esac
  elif [ "$v" -ge "$BUG_FROM" ]; then bad=1; fi
  if [ $bad = 1 ]; then
    verdict="HYDO-ORACLE: VIOLATION [app.acked_writes_survive] lost=$v of 20"
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
  steps: 3
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
        let dir = std::env::temp_dir().join(format!("hydo-versions-{}-{name}", std::process::id()));
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

    fn versions(&self, extra: &[&str], env: &[(&str, &str)]) -> (Value, String) {
        let mut a = vec![
            "versions",
            "disk=none | run C=12",
            "--manifest",
            "hydo/hydo.yaml",
            "--out",
            "out",
            "--cache",
            "../cache",
        ];
        a.extend_from_slice(extra);
        let (rc, text) = self.hydo(&a, env);
        assert_eq!(rc, 0, "{text}");
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(self.dir.join("proj/out/versions.json")).unwrap(),
        )
        .unwrap();
        (doc, text)
    }

    fn log(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log").join(name))
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

fn outcomes(f: &Value) -> Vec<(String, String)> {
    f["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["version"].as_str().unwrap().to_string(),
                v["outcome"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The bug appears at version 6 of 8 and version 5 does not build: the newest and the oldest
/// are replayed first, the bisection steps over version 5, and a second search builds nothing
/// that built before.
#[test]
fn the_first_bad_version_is_found_by_bisection_and_a_broken_build_is_undecided() {
    let t = T::new("bisect");
    let env = [("BUG_FROM", "6"), ("NO_BUILD", "5")];
    let (doc, text) = t.versions(&["--param", "version=1,2,3,4,5,6,7,8"], &env);
    assert_eq!(doc["type"], "versions");
    let f = &doc["findings"][0];
    assert_eq!(t.log("runs"), ["8", "1", "4", "6"], "{text}");
    assert_eq!(f["first_bad"], "6", "{text}");
    assert_eq!(f["last_good"], "4");
    assert_eq!(f["still_present_in_newest"], true);
    assert!(f.get("contradiction").is_none());
    assert_eq!(f["replays"], 4);
    assert_eq!(f["source"], "param version");
    assert_eq!(f["found_on"], "version 8");
    let o = outcomes(f);
    let get = |v: &str| o.iter().find(|x| x.0 == v).unwrap().1.clone();
    assert_eq!(get("5"), "cannot-run");
    assert_eq!(get("6"), "reproduced");
    assert_eq!(get("4"), "passes");
    assert_eq!(get("2"), "not-replayed");
    let v5 = &f["versions"][4];
    assert!(
        v5["why"]
            .as_str()
            .unwrap()
            .contains("no release 5 to fetch"),
        "{v5}"
    );
    assert_eq!(
        f["versions"][7]["fingerprint"],
        format!("{:016x}", 8 * 4097)
    );
    assert!(f["key"].as_str().unwrap().starts_with("f1:invariant:"));
    assert_eq!(
        f["summary"],
        "introduced in 6 (4 passes; 5 in between not decided); still present in 8, the newest \
         version tried"
    );
    assert!(text.contains("  6        reproduced"), "{text}");
    // the image cache: the second search builds only the version that failed to build
    let before = t.log("builds");
    let (doc2, _) = t.versions(&["--param", "version=1,2,3,4,5,6,7,8"], &env);
    let after = t.log("builds");
    assert_eq!(&after[before.len()..], ["5"]);
    assert_eq!(doc2["findings"][0]["versions"][7]["image_reused"], true);
    assert_eq!(doc2["findings"][0]["first_bad"], "6");
    // hydo report of the output directory prints the table
    let (rc, rep) = t.hydo(&["report", "out"], &env);
    assert_eq!(rc, 0, "{rep}");
    assert!(rep.contains("introduced in 6"), "{rep}");
}

/// Replays that contradict one change point are named: with --all, and when the version the
/// finding was found on reproduces between two versions that pass.
#[test]
fn contradicting_replays_are_reported() {
    let t = T::new("contra");
    let (doc, text) = t.versions(
        &["--param", "version=1,2,3,4", "--all"],
        &[("PATTERN", "2,4")],
    );
    let f = &doc["findings"][0];
    assert_eq!(t.log("runs"), ["4", "3", "2", "1"]);
    assert_eq!(
        outcomes(f).iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
        ["passes", "reproduced", "passes", "reproduced"]
    );
    let c = f["contradiction"].as_str().unwrap();
    assert!(
        c.contains("1 passes, 2 reproduces, 3 passes, 4 reproduces"),
        "{c}"
    );
    assert!(text.contains("contradict a single change point"), "{text}");

    let t = T::new("contra2");
    let (doc, _) = t.versions(
        &["--param", "version=1,2,3,4,5", "--found-on", "2"],
        &[("PATTERN", "2")],
    );
    let f = &doc["findings"][0];
    assert_eq!(t.log("runs")[0], "2", "the found-on version first");
    assert!(f["contradiction"].is_string(), "{f}");
    assert_eq!(f["first_bad"], "2");
    assert_eq!(f["fixed_in"], "3");
    assert_eq!(f["still_present_in_newest"], false);
}

/// A run directory's finding: its key is the run's verdict's, the answer is written next to it,
/// and `hydo report` of the run prints it.
#[test]
fn a_run_directory_gets_a_versions_line_in_its_report() {
    let t = T::new("rundir");
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema/examples/results/v1/mac-redis-run-power-cut-violation/result.json");
    let run: Value = serde_json::from_str(&std::fs::read_to_string(src).unwrap()).unwrap();
    let mut run = run;
    // the recorded run's verdict is the stand-in system's, so that its key is the one compared
    run["verdict"]["text"] =
        "HYDO-ORACLE: VIOLATION [app.acked_writes_survive] lost=1 of 20".into();
    run["findings"] = Value::Array(vec![]);
    let rd = t.dir.join("proj/run1");
    std::fs::create_dir_all(&rd).unwrap();
    std::fs::write(rd.join("result.json"), run.to_string()).unwrap();
    let (rc, text) = t.hydo(
        &[
            "versions",
            "run1",
            "--manifest",
            "hydo/hydo.yaml",
            "--param",
            "version=1,2,3",
            "--cache",
            "../cache",
        ],
        &[("BUG_FROM", "3")],
    );
    assert_eq!(rc, 0, "{text}");
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(rd.join("versions/versions.json")).unwrap())
            .unwrap();
    let f = &doc["findings"][0];
    assert_eq!(f["found_on"], "run1");
    assert_eq!(f["first_bad"], "3");
    assert_eq!(f["last_good"], "2");
    let (rc, rep) = t.hydo(&["report", "run1"], &[]);
    assert_eq!(rc, 0, "{rep}");
    assert!(
        rep.contains("versions: introduced in 3 (2 passes); still present in 3"),
        "{rep}"
    );
}

/// `--git A..B`: each first-parent commit is checked out, its build command runs in the
/// checkout's project directory, and its image is built there; the bug appears at the fourth
/// commit. The checkouts are removed afterwards.
#[test]
fn git_revisions_are_checked_out_built_and_bisected() {
    let t = T::new("git");
    let repo = t.dir.join("repo");
    std::fs::create_dir_all(repo.join("app/hydo")).unwrap();
    std::fs::write(repo.join("app/hydo/hydo.yaml"), MANIFEST).unwrap();
    let g = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
    };
    g(&["init", "-q"]);
    for i in 1..=6 {
        std::fs::write(repo.join("app/VERSION"), format!("{i}\n")).unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-q", "-m", &format!("release {i}")]);
        g(&["tag", &format!("r{i}")]);
    }
    let (doc, text) = t.versions(
        &[
            "--git",
            "r1..r6",
            "--project",
            "../repo/app",
            "--build",
            "cp VERSION hydo/built-version",
        ],
        &[("BUG_FROM", "4")],
    );
    let f = &doc["findings"][0];
    assert_eq!(f["source"], "git");
    let labels: Vec<String> = outcomes(f).into_iter().map(|x| x.0).collect();
    assert_eq!(labels, ["r1", "r2", "r3", "r4", "r5", "r6"], "{text}");
    assert_eq!(f["first_bad"], "r4", "{text}");
    assert_eq!(f["last_good"], "r3");
    assert_eq!(f["still_present_in_newest"], true);
    assert!(f["versions"][3]["commit"].as_str().unwrap().len() == 40);
    let wt = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&wt.stdout).lines().count(),
        1,
        "the checkouts are removed"
    );
}
