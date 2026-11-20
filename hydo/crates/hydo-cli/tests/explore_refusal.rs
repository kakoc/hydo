//! `hydo explore` that refuses before it runs a schedule (Milestone 328). A user's exploration
//! refused for the machine's memory, printed the reason and wrote no report, and the script
//! around it could not tell the stop from an exploration that ran: every such stop now exits
//! with status 5, and the end for a program (`--json`, or stdout that is not a terminal) is one
//! JSON line with the reason. The scripted engine (`HYDO_SCRIPTED_ENGINE`) stands in for the
//! engine, and no schedule runs.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

const ENGINE: &str = r#"#!/bin/sh
set -eu
case $1 in
serve)
  echo "READY boot_secs=0.1"
  while read -r tag sched; do
    : > "$2/$tag.log"
    echo "RESULT $tag fp=0000000000000001 wfp=0 secs=0.1 verdict=HYDO-ORACLE: OK invariants=1 held"
  done;;
one)
  : > "$2/one.log"
  echo "RESULT one fp=0000000000000001 wfp=0 secs=0.1 verdict=HYDO-ORACLE: OK invariants=1 held";;
esac
"#;

struct T {
    dir: PathBuf,
}

impl T {
    fn new(name: &str) -> T {
        let dir = std::env::temp_dir().join(format!("hydo-refusal-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let e = dir.join("engine.sh");
        std::fs::write(&e, ENGINE).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("db-arm64.cpio.gz"), "a stand-in image\n").unwrap();
        T { dir }
    }

    /// The exit status, stdout and stderr of `hydo explore <args>`.
    fn explore(&self, args: &[&str]) -> (i32, String, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .arg("explore")
            .args(args)
            .current_dir(&self.dir)
            .env("HYDO_CLI_PLATFORM", "mac")
            .env("HYDO_SCRIPTED_ENGINE", self.dir.join("engine.sh"))
            .env("HYDO_HOME", self.dir.join("home"))
            .env_remove("HYDO_BACKEND")
            .output()
            .unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn json_line(out: &str) -> Value {
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "one JSON line: {out}");
    serde_json::from_str(lines[0]).unwrap_or_else(|e| panic!("{e}: {out}"))
}

/// A stop of the command before the explorer starts (no image) and a refusal of the explorer
/// (options that do not go together) both exit 5, each with its reason as one JSON line.
#[test]
fn a_refusal_before_any_run_exits_5_with_one_json_line() {
    let t = T::new("refuse");
    let (rc, out, err) = t.explore(&["--image", "missing-arm64.cpio.gz", "--json"]);
    assert_eq!(rc, 5, "{out}{err}");
    let v = json_line(&out);
    assert_eq!(v["status"], 5, "{v}");
    assert_eq!(v["refused"], "no such image: missing-arm64.cpio.gz", "{v}");
    assert!(err.contains("hydo: no such image"), "{err}");

    let (rc, out, err) = t.explore(&[
        "--image",
        "db-arm64.cpio.gz",
        "--search",
        "state",
        "--sweep",
        "freeze",
        "--out",
        "out",
        "--json",
    ]);
    assert_eq!(rc, 5, "{out}{err}");
    let v = json_line(&out);
    assert!(
        v["refused"]
            .as_str()
            .unwrap()
            .starts_with("hydo explore: --search state goes with a seeded exploration"),
        "{v}"
    );
    assert!(
        !t.dir.join("out/report.md").exists(),
        "a refusal writes no report"
    );
}

/// A bad option keeps its status 2 (the usage), and nothing is printed as a refusal.
#[test]
fn a_bad_option_keeps_status_2() {
    let t = T::new("usage");
    let (rc, out, err) = t.explore(&["--image", "db-arm64.cpio.gz", "--no-such-option", "--json"]);
    assert_eq!(rc, 2, "{out}{err}");
    assert!(!out.contains("\"refused\""), "{out}");
}
