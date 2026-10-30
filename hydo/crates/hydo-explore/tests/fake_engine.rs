//! hydo-explore against explore.py end to end, on a scripted engine (`tests/fake_engine.py`, put in
//! `$HYDO_HVX_RUN`, so both explorers start it exactly as they start hvx_explore): the same
//! options, and the outputs compared file by file — runs.jsonl, shrink.jsonl, report.json,
//! report.md, errors.json, properties.txt, sweep-points.jsonl, fc-traces.jsonl, fc-adaptive.json and
//! the printed summary — after normalising only what is not a function of the schedules: the wall
//! times (sweep_wall_s, runs_per_hour, total_wall_s, stage1/2_wall_s, a stage-2 round's secs), the
//! worker that ran a schedule and the worker count (explore.py runs here with 2 workers,
//! hydo-explore with 3).
//!
//! Every scenario exercises a path: generators with every option stream, the cf= probe, re-runs,
//! several shrink chains side by side, tick-jitter shrinking to tjx= lists, failed properties and
//! the liveness table, error-log kinds (new and known) and their shrinks, a composed plan's W=,
//! sweeps, the full and the adaptive freeze sweep, protocol and gRPC faults, branching, one
//! manual schedule. Needs python3 and cc; without python3 the test says so and passes.

use std::path::{Path, PathBuf};
use std::process::Command;

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("root")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-explore-fake-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

fn fake() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_engine.py");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
    }
    p
}

fn run(prog: &mut Command, dir: &Path, out: &Path) -> String {
    let o = prog
        .env("HYDO_HVX_RUN", fake())
        .env("HYDO_HVX_EXPLORE", fake())
        .env("HYDO_BUILD", dir)
        .env("HYDO_SNAP_DIR", dir.join("snap"))
        .env_remove("HYDO_EXPLORE_CACHE")
        .env_remove("HYDO_QUIET_FILE")
        .env_remove("HYDO_CLI")
        .env_remove("HYDO_SNAPSHOTS")
        .env(
            "HYDO_FAKE_HIST",
            if out.to_string_lossy().contains("hist") {
                "1"
            } else {
                ""
            },
        )
        .current_dir(top())
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        o.status.success(),
        "{:?} failed in {}:\n{}\n{}",
        prog,
        out.display(),
        stdout,
        String::from_utf8_lossy(&o.stderr)
    );
    stdout
}

fn norm(text: &str) -> String {
    let re = regex::Regex::new(
        r#"("?(?:sweep_wall_s|runs_per_hour|total_wall_s|stage1_wall_s|stage2_wall_s|workers|worker)"?(?::| \|) )[0-9.]+"#,
    )
    .unwrap();
    let t = re.replace_all(text, "${1}N").into_owned();
    // a stage-2 round's wall time in fc-adaptive.json
    let re2 = regex::Regex::new(r#"("failures": \d+,\s*"secs": )[0-9.]+"#).unwrap();
    re2.replace_all(&t, "${1}N").into_owned()
}

const FILES: [&str; 9] = [
    "runs.jsonl",
    "shrink.jsonl",
    "report.json",
    "report.md",
    "errors.json",
    "properties.txt",
    "sweep-points.jsonl",
    "fc-traces.jsonl",
    "fc-adaptive.json",
];

fn scenario(name: &str, args: &[&str]) {
    let dir = scratch(name);
    let (py, rs) = (dir.join("py"), dir.join("rs"));
    let mut p = Command::new("python3");
    p.arg(top().join("hydo/explore/explore.py"))
        .args(args)
        .args(["--workers", "2", "--out"])
        .arg(&py);
    let py_out = run(&mut p, &dir, &py);
    let mut r = Command::new(env!("CARGO_BIN_EXE_hydo-explore"));
    r.args(args).args(["--workers", "3", "--out"]).arg(&rs);
    let rs_out = run(&mut r, &dir, &rs);
    let mut diffs = Vec::new();
    for f in FILES {
        let (a, b) = (
            std::fs::read_to_string(py.join(f)),
            std::fs::read_to_string(rs.join(f)),
        );
        match (a, b) {
            (Err(_), Err(_)) => {}
            (Ok(a), Ok(b)) => {
                let (a, b) = (
                    norm(&a.replace(&py.display().to_string(), "<out>")),
                    norm(&b.replace(&rs.display().to_string(), "<out>")),
                );
                if a != b {
                    let (la, lb): (Vec<&str>, Vec<&str>) =
                        (a.lines().collect(), b.lines().collect());
                    let i = la
                        .iter()
                        .zip(&lb)
                        .position(|(x, y)| x != y)
                        .unwrap_or(la.len().min(lb.len()));
                    diffs.push(format!(
                        "{f}: first difference at line {} ({} vs {} lines)\n  explore.py:   {}\n  hydo-explore: {}",
                        i + 1,
                        la.len(),
                        lb.len(),
                        la.get(i).unwrap_or(&"<end>"),
                        lb.get(i).unwrap_or(&"<end>")
                    ));
                }
            }
            (a, b) => diffs.push(format!(
                "{f}: explore.py {} it, hydo-explore {}",
                if a.is_ok() { "wrote" } else { "did not write" },
                if b.is_ok() { "wrote it" } else { "did not" }
            )),
        }
    }
    let last = |s: &str| norm(s.lines().last().unwrap_or(""));
    if last(&py_out) != last(&rs_out) {
        diffs.push(format!(
            "summary line:\n  {}\n  {}",
            last(&py_out),
            last(&rs_out)
        ));
    }
    assert!(
        diffs.is_empty(),
        "scenario {name} ({}):\n{}",
        dir.display(),
        diffs.join("\n")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn have_python() -> bool {
    Command::new("python3")
        .arg("-c")
        .arg("pass")
        .status()
        .is_ok_and(|s| s.success())
}

macro_rules! scenarios {
    ($($name:ident: [$($a:expr),*];)*) => {$(
        #[test]
        fn $name() {
            if !have_python() {
                eprintln!("SKIPPED: python3 is not available; the reference is explore.py");
                return;
            }
            scenario(stringify!($name), &[$($a),*]);
        }
    )*};
}

const KNOWN: &str = "hydo/crates/hydo-explore/tests/fixtures/known-errors.yaml";

scenarios! {
    stack_random_all_streams: ["--seeds", "1-60", "--timing", "--new-faults", "--concurrency", "--more-faults", "--buggify", "--known-errors", KNOWN];
    stack_swarm_pos_preempt: ["--seeds", "1-50", "--strategy", "swarm-pos", "--preempt", "--durable-only", "--timing"];
    stack_swarm_errors_fail: ["--seeds", "1-40", "--strategy", "swarm", "--new-faults", "--errors", "fail", "--knob-rule", "any"];
    stack_process_vcpus: ["--seeds", "1-40", "--strategy", "process", "--vcpus", "2", "--turns", "--timing"];
    stack_sweep_tail: ["--sweep", "tail"];
    stack_sweep_frontkill: ["--sweep", "frontkill", "--timing"];
    stack_branch: ["--seeds", "1-12", "--branch", "hydo-stack-marker:", "--variants", "3"];
    stack_manual: ["--schedule", "disk=cut dseed=0 | run C=4 R=everysec"];
    composed_plan: ["--seeds", "1-40", "--manifest", "hydo/onboard/examples/redis-composed/hydo/hydo.yaml", "--strategy", "process"];
    composed_plan_random: ["--seeds", "1-30", "--manifest", "hydo/onboard/examples/redis-composed/hydo/hydo.yaml", "--new-faults"];
    freeze_full_sweep: ["--sweep", "freeze", "--manifest", "hydo/onboard/examples/redis/hydo-freeze.yaml", "--fc-steps", "8-9", "--fc-durs", "1000"];
    freeze_adaptive: ["--sweep", "freeze", "--fc-adaptive", "--manifest", "hydo/onboard/examples/redis/hydo-freeze.yaml", "--fc-steps", "7-9", "--fc-window", "4"];
    freeze_call: ["--seeds", "1-40", "--manifest", "hydo/onboard/examples/redis/hydo-freeze.yaml", "--freeze-call", "--strategy", "swarm"];
    protocol_faults: ["--seeds", "1-40", "--manifest", "hydo/onboard/examples/shop/hydo.yaml", "--protocol-faults", "--strategy", "process"];
    protocol_sweep: ["--sweep", "protocol", "--manifest", "hydo/onboard/examples/shop/hydo.yaml"];
    grpc_faults: ["--seeds", "1-40", "--manifest", "hydo/onboard/examples/stack-grpc/hydo.yaml", "--grpc-faults", "--protocol-faults"];
    stack_hist_checks: ["--seeds", "1-30", "--strategy", "swarm", "--new-faults"];
    errors_manifest: ["--seeds", "1-30", "--manifest", "hydo/onboard/examples/redis-errors/hydo/hydo.yaml", "--new-faults", "--errors-shrink", "2"];
}
