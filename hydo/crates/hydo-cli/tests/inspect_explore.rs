//! `hydo inspect diagnose <exploration> [--problem N] [--run]` (Milestone 324). A user's
//! `hydo inspect diagnose hydo-out/explore` said `no-run: ./result.json: No such file`, because
//! an exploration keeps no run directory, and the user made one by hand with `hydo run
//! "<schedule>" --out <dir>`. The inputs are the scripted exploration of
//! `tests/fixtures/report/scripted-explore` (no control transcripts, as the scripted engine
//! writes none) and the Redis run of `tests/fixtures/report/redis` as the files a real run
//! leaves. The scripted engine (`HYDO_SCRIPTED_ENGINE`) stands in for `--run`'s one run. No VM
//! runs.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/report")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), &t).unwrap();
        }
    }
}

/// The engine's `one <dir> <schedule>`: the Redis run's files in the run directory, and the
/// schedule in `$LOG`.
const ENGINE: &str = r#"#!/bin/sh
set -eu
[ "$1" = one ] || exit 2
cp "$RUN"/* "$2"/
echo "$3" >> "$LOG"
"#;

struct T {
    dir: PathBuf,
}

impl T {
    fn new() -> T {
        let dir = std::env::temp_dir().join(format!("hydo-inspect-explore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        copy_tree(&fixtures().join("scripted-explore"), &dir.join("explore"));
        let e = dir.join("engine.sh");
        std::fs::write(&e, ENGINE).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755)).unwrap();
        T { dir }
    }

    fn diagnose(&self, args: &[&str]) -> (i32, Value) {
        let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
            .args(["inspect", "diagnose"])
            .args(args)
            .current_dir(&self.dir)
            .env("HYDO_SCRIPTED_ENGINE", self.dir.join("engine.sh"))
            .env("RUN", fixtures().join("redis/hydo-out/run-cut"))
            .env("LOG", self.dir.join("engine.log"))
            .env_remove("HYDO_BACKEND")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&o.stdout);
        let v = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
        (o.status.code().unwrap_or(-1), v)
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn next(v: &Value) -> Vec<String> {
    v["diagnosis"]["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn an_exploration_is_diagnosed_by_its_problems() {
    let t = T::new();
    let one = "hydo run --backend hvx --image scripted-disk.img \"disk=ioerr:2 \
               dseed=9e5651b0ef953636 | run K=kafka@13+3 N=redis@23-27:delay,79ms\" --out \
               explore/runs/seed-8";
    // no run directory and no transcripts: the command that makes one, and --run
    let (rc, v) = t.diagnose(&["explore"]);
    assert_eq!(rc, 2, "{v}");
    assert_eq!(v["code"], "no-run", "{v}");
    let msg = v["message"].as_str().unwrap();
    assert!(
        msg.starts_with("explore keeps no run directory of problem 1 (The run stopped early"),
        "{msg}"
    );
    assert!(
        msg.contains("no control transcript of its run seed-8"),
        "{msg}"
    );
    assert_eq!(v["try"][0], one, "{v}");
    assert_eq!(
        v["try"][1],
        "hydo inspect diagnose explore --problem 1 --run"
    );
    // problem 3 is the services' error lines, and there is no problem 4
    let (rc, v) = t.diagnose(&["explore", "--problem", "3"]);
    assert_eq!((rc, v["code"].as_str()), (2, Some("usage")), "{v}");
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("problem 3 has no failing run"));
    assert_eq!(v["try"][0], "hydo errors explore");
    let (rc, v) = t.diagnose(&["explore", "--problem", "4"]);
    assert_eq!(rc, 2, "{v}");
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("the exploration has 3 problems"));
    // the exploration's own transcripts of the minimal schedule's confirmation
    let cut = fixtures().join("redis/hydo-out/run-cut");
    for (from, to) in [
        ("control.jsonl", "min-a.control.jsonl"),
        ("control.recovery.jsonl", "min-a.control.recovery.jsonl"),
    ] {
        std::fs::copy(cut.join(from), t.dir.join("explore/consoles").join(to)).unwrap();
    }
    let (rc, v) = t.diagnose(&["explore", "--problem", "2"]);
    assert_eq!(rc, 0, "{v}");
    assert_eq!(v["type"], "diagnosis");
    assert_eq!(v["run_dir"], "explore");
    let n = next(&v);
    assert!(
        n[0].starts_with("problem 2 (Your system failed your check with a faulty disk): read from the exploration's own files of run min-a"),
        "{n:?}"
    );
    assert!(
        n[0].ends_with(
            "\"disk=lying:1 dseed=63cbe1e459320dd7 | run C=11\" --out explore/runs/seed-7"
        ),
        "{n:?}"
    );
    assert!(!n.iter().any(|x| x.starts_with("hydo inspect")), "{n:?}");
    // --run makes the run directory (here the scripted engine's) and reads it
    let (rc, v) = t.diagnose(&["explore", "--run"]);
    assert_eq!(rc, 0, "{v}");
    assert_eq!(v["run_dir"], "explore/runs/seed-8");
    assert!(next(&v)[0].starts_with(
        "problem 1 (The run stopped early after disk errors): ran hydo run --backend hvx"
    ));
    let log = std::fs::read_to_string(t.dir.join("engine.log")).unwrap();
    assert_eq!(
        log,
        "disk=ioerr:2 dseed=9e5651b0ef953636 | run K=kafka@13+3 N=redis@23-27:delay,79ms\n"
    );
    // once made, the run directory is read without another run
    let (rc, v) = t.diagnose(&["explore"]);
    assert_eq!(rc, 0, "{v}");
    assert_eq!(next(&v)[0], "problem 1 (The run stopped early after disk errors): its run directory explore/runs/seed-8");
    assert_eq!(
        std::fs::read_to_string(t.dir.join("engine.log")).unwrap(),
        log
    );
    // --problem and --run take an exploration
    let (rc, v) = t.diagnose(&["explore/runs/seed-8", "--problem", "1"]);
    assert_eq!((rc, v["code"].as_str()), (2, Some("usage")), "{v}");
}
