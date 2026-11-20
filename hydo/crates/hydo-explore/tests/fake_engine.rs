//! hydo-explore end to end on a scripted engine, against explore.py's recorded outputs.
//!
//! The scripted engine is this test binary itself: run with `HYDO_FAKE_ENGINE=1` it is the
//! explorer's scripted engine program (`HYDO_SCRIPTED_ENGINE`, `hydo_explore::scripted`, M145;
//! until M145 it stood in for `hvx-run sudo env ... hydo-hvx serve ...` and spoke the engine's
//! RUN/BRANCH protocol of shell images): READY, then a RESULT for every `<tag> <schedule>` line.
//! Every answer is a pure function of the schedule text (never of the tag or of the order of
//! requests), so two explorers that ask for the same schedules get the same answers. It is the port of `tests/fake_engine.py` (until M107;
//! `git show 2a58c4b:hydo/crates/hydo-explore/tests/fake_engine.py`), answer for answer.
//!
//! Each scenario runs hydo-explore with its options and compares its outputs file by file —
//! events.jsonl, exploration.json (the results format, M115), report.md and the printed summary
//! — with `tests/golden/fake/<scenario>/`, after normalising only what is not a function of the
//! schedules: the wall times (sweep_wall_s, runs_per_hour, total_wall_s, stage1/2_wall_s, a
//! stage-2 round's secs), the worker that ran a schedule and the worker count, the loaded hvx
//! module, and the output and scratch paths (`<out>`, `<dir>`). The goldens were explore.py's
//! outputs at commit 2a58c4b on fake_engine.py (recorded in M107); in M115 they were re-recorded
//! in the results format after hydo/crates/hydo-explore/examples/legacy_check.rs found every
//! value of the old files of all 18 scenarios in the new ones (docs/HISTORY.md, Milestone 115),
//! and report.md and the printed summary unchanged. Files up to 4 KiB are kept verbatim; a longer one as its
//! digest (`<file>.sha256`: its SHA-256 and line count, then a hash per 32 lines, so a difference
//! is shown with the written lines of the first differing chunk). `tests/golden/fake/SOURCE.txt`
//! says how they were recorded.
//!
//! Every scenario exercises a path: generators with every option stream, the cf= probe, re-runs,
//! several shrink chains side by side, tick-jitter shrinking to tjx= lists, failed properties and
//! the liveness table, error-log kinds (new and known) and their shrinks, a composed plan's W=,
//! sweeps, the full and the adaptive freeze sweep, protocol and gRPC faults, one manual schedule
//! (the branching scenario went with `--branch` in M145).
//!
//! Needs cc (the property table compiles hydo_assert_report.c). Arguments: scenario names to run
//! (default all). `HYDO_BLESS=1` rewrites the goldens from the outputs (the reference since M107).
//! `HYDO_FAKE_EXPLORER="<program> [args]"` runs another explorer instead of hydo-explore (with 2
//! workers), and `HYDO_FAKE_ENGINE_BIN=<program>` another scripted engine instead of this one: how
//! the goldens were recorded (explore.py on fake_engine.py) and cross-checked (explore.py on this
//! engine, hydo-explore on fake_engine.py).

mod tmpdir;
use std::path::{Path, PathBuf};
use std::process::Command;
use tmpdir::TmpDir;

/// The image every scenario names (the scripted engine never opens it). Until M146 the scenarios
/// took the explorer's default, `explore-disk.img`; that default is gone (a shell-init image), and
/// the goldens were re-recorded once for the new name.
const IMAGE: [&str; 2] = ["--image", "scripted-disk.img"];

fn main() {
    if std::env::var_os("HYDO_FAKE_ENGINE").is_some_and(|v| v == "1") {
        engine::main();
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    // libtest's listing, as cargo-nextest asks for it (`--list --format terse`, and `--ignored`
    // for the ignored tests: none); nextest then runs one scenario as `--exact <name>`
    // (Milestone 171)
    if flag("--list") {
        if !flag("--ignored") {
            for (n, _) in SCENARIOS {
                println!("{n}: test");
            }
        }
        return;
    }
    let exact = flag("--exact");
    let only: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();
    let list: Vec<&(&str, &[&str])> = SCENARIOS
        .iter()
        .filter(|(n, _)| {
            only.is_empty()
                || only.iter().any(|o| {
                    if exact {
                        n == o
                    } else {
                        n.contains(o.as_str())
                    }
                })
        })
        .collect();
    let par = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 6);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let fails = std::sync::Mutex::new(Vec::<String>::new());
    println!("\nrunning {} scenarios", list.len());
    std::thread::scope(|sc| {
        for _ in 0..par.min(list.len().max(1)) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((name, args)) = list.get(i) else {
                    break;
                };
                match std::panic::catch_unwind(|| scenario(name, args)) {
                    Ok(()) => println!("test {name} ... ok"),
                    Err(e) => {
                        let msg = e
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                            .unwrap_or_default();
                        println!("test {name} ... FAILED");
                        fails
                            .lock()
                            .unwrap()
                            .push(format!("---- {name} ----\n{msg}"));
                    }
                }
            });
        }
    });
    let fails = fails.into_inner().unwrap();
    if fails.is_empty() {
        println!("\ntest result: ok. {} passed; 0 failed\n", list.len());
    } else {
        println!("\nfailures:\n\n{}\n", fails.join("\n\n"));
        println!(
            "test result: FAILED. {} passed; {} failed\n",
            list.len() - fails.len(),
            fails.len()
        );
        std::process::exit(101);
    }
}

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("root")
}

fn scratch(name: &str) -> TmpDir {
    let d = std::env::temp_dir().join(format!("hydo-explore-fake-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    TmpDir::adopt(d.canonicalize().expect("scratch"))
}

fn engine_path() -> PathBuf {
    match std::env::var_os("HYDO_FAKE_ENGINE_BIN") {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe().expect("this test binary"),
    }
}

fn run(prog: &mut Command, dir: &Path, out: &Path) -> String {
    let o = prog
        .env("HYDO_FAKE_ENGINE", "1")
        .env("HYDO_SCRIPTED_ENGINE", engine_path())
        .env("HYDO_BUILD", dir)
        .env_remove("HYDO_EXPLORE_CACHE")
        .env_remove("HYDO_QUIET_FILE")
        .env_remove("HYDO_BLESS")
        .env(
            "HYDO_FAKE_HIST",
            if out.to_string_lossy().contains("hist") {
                "1"
            } else {
                ""
            },
        )
        .env(
            "HYDO_FAKE_KVM_TURNS",
            if out.to_string_lossy().contains("kvm_turns_") {
                "1"
            } else {
                ""
            },
        )
        .env(
            "HYDO_FAKE_STEER",
            if out.to_string_lossy().contains("steer_") {
                "1"
            } else {
                ""
            },
        )
        .env(
            "HYDO_FAKE_RESTART",
            if out.to_string_lossy().contains("restart_") {
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
    // pretty JSON and report.md tables ("key": 1.5, | key | 1.5 |) and compact JSON ("key":1.5)
    let re = regex::Regex::new(
        r#"("?(?:sweep_wall_s|runs_per_hour|total_wall_s|stage1_wall_s|stage2_wall_s|workers|worker)"?(?:: | \| |:))[0-9.]+"#,
    )
    .unwrap();
    let t = re.replace_all(text, "${1}N").into_owned();
    // a stage-2 round's wall time (exploration.json freeze_sweep.rounds)
    let re2 = regex::Regex::new(r#"("failures": \d+,\s*"secs": )[0-9.]+"#).unwrap();
    let t = re2.replace_all(&t, "${1}N").into_owned();
    // the loaded hvx module of the machine the test runs on (absent on a Mac)
    let re3 = regex::Regex::new(r#",?\s*"module_srcversion": ?"[^"]*""#).unwrap();
    let t = re3.replace_all(&t, "").into_owned();
    // the CPU of the machine the test runs on
    let re4 = regex::Regex::new(r#",?\s*"host_cpu": ?\{[^}]*\}"#).unwrap();
    let t = re4.replace_all(&t, "").into_owned();
    // Milestone 339: how many runs an earlier run of their schedule answered depends on which
    // runs were in flight together (repeats_scenario checks them)
    let re5 = regex::Regex::new(r#",?\s*"repeats(?:_saved_s)?": ?[0-9.]+"#).unwrap();
    let t = re5.replace_all(&t, "").into_owned();
    let re6 = regex::Regex::new(r"(?m)^\| repeats(?:_saved_s)? \| [0-9.]+ \|\n").unwrap();
    re6.replace_all(&t, "").into_owned()
}

/// What an exploration writes (M115: the results format and report.md, its view for people;
/// with `--protocol-cover` also the walk's order, Milestone 257).
const FILES: [&str; 4] = [
    "events.jsonl",
    "exploration.json",
    "report.md",
    "protocol-cover.txt",
];

/// Above this a golden file is kept as its digest (`digest`).
const VERBATIM_MAX: usize = 4 * 1024;

/// Lines per chunk of a digest.
const DIGEST_CHUNK: usize = 32;

/// A long output's golden: `<sha256> <lines>`, then one line per chunk of DIGEST_CHUNK lines,
/// `<first line> <lines> <sha256 prefix>`, so a difference is placed within a chunk.
fn digest(t: &str) -> String {
    let lines: Vec<&str> = t.split_inclusive('\n').collect();
    let mut d = format!("{} {}\n", sha256_hex(t.as_bytes()), lines.len());
    for (i, ch) in lines.chunks(DIGEST_CHUNK).enumerate() {
        d.push_str(&format!(
            "{} {} {}\n",
            i * DIGEST_CHUNK + 1,
            ch.len(),
            &sha256_hex(ch.concat().as_bytes())[..16]
        ));
    }
    d
}

/// Where two digests first differ, with the output's lines of that chunk.
fn digest_diff(want: &str, now: &str, t: &str) -> String {
    let (w, n): (Vec<&str>, Vec<&str>) = (want.lines().collect(), now.lines().collect());
    let i = w
        .iter()
        .zip(&n)
        .skip(1)
        .position(|(a, b)| a != b)
        .map(|i| i + 1)
        .unwrap_or(w.len().min(n.len()));
    let count = |x: &[&str]| x[0].split(' ').nth(1).unwrap_or("?").to_string();
    let mut m = format!(
        "recorded {} lines, written {} lines, contents differ",
        count(&w),
        count(&n)
    );
    if let Some(c) = n.get(i) {
        let first: usize = c.split(' ').next().unwrap().parse().unwrap();
        m.push_str(&format!(
            "; the first differing chunk starts at line {first}:"
        ));
        for l in t.lines().skip(first - 1).take(DIGEST_CHUNK) {
            let l: String = l.chars().take(300).collect();
            m.push_str(&format!("\n    {l}"));
        }
    }
    m
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn bless() -> bool {
    std::env::var_os("HYDO_BLESS").is_some_and(|v| !v.is_empty() && v != "0")
}

fn scenario(name: &str, args: &[&str]) {
    if name == CACHE_SCENARIO {
        return cache_scenario();
    }
    if name == TIME_LIMIT_SCENARIO {
        return time_limit_scenario();
    }
    if name == MERGE_SCENARIO {
        return merge_scenario();
    }
    if name == REPEATS_SCENARIO {
        return repeats_scenario();
    }
    if name == RESTART_SCENARIO {
        return restart_scenario();
    }
    let dir = scratch(name);
    let out = dir.join("out");
    let mut cmd = match std::env::var("HYDO_FAKE_EXPLORER") {
        Ok(p) if !p.trim().is_empty() => {
            let w: Vec<&str> = p.split_whitespace().collect();
            let mut c = Command::new(w[0]);
            c.args(&w[1..])
                .args(args)
                .args(IMAGE)
                .args(["--workers", "2", "--out"]);
            c
        }
        _ => {
            let mut c = Command::new(env!("CARGO_BIN_EXE_hydo-explore"));
            c.args(args).args(IMAGE).args(["--workers", "3", "--out"]);
            c
        }
    };
    cmd.arg(&out);
    let stdout = run(&mut cmd, &dir, &out);
    let clean = |t: &str| {
        norm(
            &t.replace(&out.display().to_string(), "<out>")
                .replace(&dir.display().to_string(), "<dir>"),
        )
    };
    let gdir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/fake")
        .join(name);
    let mut got: Vec<(String, String)> = Vec::new();
    for f in FILES {
        if let Ok(t) = std::fs::read_to_string(out.join(f)) {
            got.push((f.to_string(), clean(&t)));
        }
    }
    let last = clean(stdout.lines().last().unwrap_or(""));
    got.push(("summary.txt".into(), format!("{last}\n")));
    if bless() {
        let _ = std::fs::remove_dir_all(&gdir);
        std::fs::create_dir_all(&gdir).unwrap();
        for (f, t) in &got {
            if t.len() > VERBATIM_MAX {
                std::fs::write(gdir.join(format!("{f}.sha256")), digest(t)).unwrap();
            } else {
                std::fs::write(gdir.join(f), t).unwrap();
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let mut diffs = Vec::new();
    let mut want_files: Vec<String> = std::fs::read_dir(&gdir)
        .unwrap_or_else(|e| panic!("{}: {e} (record with HYDO_BLESS=1)", gdir.display()))
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    want_files.sort();
    for (f, t) in &got {
        let verbatim = gdir.join(f);
        let hashed = gdir.join(format!("{f}.sha256"));
        want_files.retain(|w| w != f && *w != format!("{f}.sha256"));
        if let Ok(want) = std::fs::read_to_string(&verbatim) {
            if &want != t {
                let (la, lb): (Vec<&str>, Vec<&str>) =
                    (want.lines().collect(), t.lines().collect());
                let i = la
                    .iter()
                    .zip(&lb)
                    .position(|(x, y)| x != y)
                    .unwrap_or(la.len().min(lb.len()));
                diffs.push(format!(
                    "{f}: first difference at line {} ({} vs {} lines)\n  recorded:     {}\n  hydo-explore: {}",
                    i + 1,
                    la.len(),
                    lb.len(),
                    la.get(i).unwrap_or(&"<end>"),
                    lb.get(i).unwrap_or(&"<end>")
                ));
            }
        } else if let Ok(want) = std::fs::read_to_string(&hashed) {
            let now = digest(t);
            if want != now {
                diffs.push(format!("{f}: {}", digest_diff(&want, &now, t)));
            }
        } else {
            diffs.push(format!("{f}: written, but not recorded"));
        }
    }
    for w in want_files {
        diffs.push(format!("{w}: recorded, but not written"));
    }
    assert!(
        diffs.is_empty(),
        "scenario {name} ({}):\n{}",
        dir.display(),
        diffs.join("\n")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

const KNOWN: &str = "hydo/crates/hydo-explore/tests/fixtures/known-errors.yaml";

const SCENARIOS: &[(&str, &[&str])] = &[
    (
        "stack_random_all_streams",
        &[
            "--seeds",
            "1-60",
            "--timing",
            "--new-faults",
            "--concurrency",
            "--more-faults",
            "--buggify",
            "--known-errors",
            KNOWN,
        ],
    ),
    // M225: several shrinks of one signature, each with other fault kinds
    (
        "stack_shrink_causes",
        &[
            "--seeds",
            "1-40",
            "--new-faults",
            "--more-faults",
            "--shrink-causes",
            "3",
        ],
    ),
    (
        "stack_swarm_pos_preempt",
        &[
            "--seeds",
            "1-50",
            "--strategy",
            "swarm-pos",
            "--preempt",
            "--durable-only",
            "--timing",
        ],
    ),
    (
        "stack_swarm_errors_fail",
        &[
            "--seeds",
            "1-40",
            "--strategy",
            "swarm",
            "--new-faults",
            "--errors",
            "fail",
            "--knob-rule",
            "any",
        ],
    ),
    (
        "stack_process_vcpus",
        &[
            "--seeds",
            "1-40",
            "--strategy",
            "process",
            "--vcpus",
            "2",
            "--turns",
            "--timing",
        ],
    ),
    ("stack_sweep_tail", &["--sweep", "tail"]),
    // M205: the turn-length search on a scripted boundary between q=18750 and q=18751
    (
        "turn_sweep",
        &[
            "--sweep",
            "turns",
            "--vcpus",
            "2",
            "--sweep-with",
            "disk=none dseed=8363 | run",
        ],
    ),
    // the KVM engine's turns of VM exits: drawn turn tokens, and the turn-length search with its
    // KVM range and resolution, on the scripted shape of nats-server's flaky test on KVM
    // (HYDO_FAKE_KVM_TURNS, set for the scenarios named kvm_turns_*)
    (
        "kvm_turns_explore",
        &[
            "--backend",
            "kvm",
            "--seeds",
            "1-40",
            "--vcpus",
            "2",
            "--turns",
        ],
    ),
    (
        "kvm_turns_sweep",
        &["--backend", "kvm", "--sweep", "turns", "--vcpus", "2"],
    ),
    // M209: the steered search on a scripted distance (HYDO-STEER records of fake.distance, the
    // failure at distance 0), and the same seeds without mutants (the plain exploration's
    // schedules in the search's bookkeeping)
    (
        "steer_sdk",
        &[
            "--seeds",
            "1-120",
            "--steer",
            "fake.distance",
            "--steer-stop",
        ],
    ),
    (
        "steer_random",
        &[
            "--seeds",
            "1-40",
            "--steer",
            "fake.distance",
            "--steer-mix",
            "0",
        ],
    ),
    (
        "stack_sweep_frontkill",
        &["--sweep", "frontkill", "--timing"],
    ),
    (
        "stack_manual",
        &["--schedule", "disk=cut dseed=0 | run C=4 R=everysec"],
    ),
    // a schedule without dseed= (or disk=), as `hydo run` takes it: until Milestone 192 the
    // explorer printed the result and then stopped with "no dseed=" and exit status 1
    ("stack_manual_no_dseed", &["--schedule", "disk=none | run"]),
    (
        "stack_manual_no_disk",
        &["--schedule", "| run C=4 R=everysec"],
    ),
    (
        "composed_plan",
        &[
            "--seeds",
            "1-40",
            "--manifest",
            "hydo/onboard/examples/redis-composed/hydo/hydo.yaml",
            "--strategy",
            "process",
        ],
    ),
    (
        "composed_plan_random",
        &[
            "--seeds",
            "1-30",
            "--manifest",
            "hydo/onboard/examples/redis-composed/hydo/hydo.yaml",
            "--new-faults",
        ],
    ),
    (
        "freeze_full_sweep",
        &[
            "--sweep",
            "freeze",
            "--manifest",
            "hydo/onboard/examples/redis/hydo-freeze.yaml",
            "--fc-steps",
            "8-9",
            "--fc-durs",
            "1000",
        ],
    ),
    (
        "freeze_adaptive",
        &[
            "--sweep",
            "freeze",
            "--fc-adaptive",
            "--manifest",
            "hydo/onboard/examples/redis/hydo-freeze.yaml",
            "--fc-steps",
            "7-9",
            "--fc-window",
            "4",
        ],
    ),
    (
        "freeze_call",
        &[
            "--seeds",
            "1-40",
            "--manifest",
            "hydo/onboard/examples/redis/hydo-freeze.yaml",
            "--freeze-call",
            "--strategy",
            "swarm",
        ],
    ),
    (
        "protocol_faults",
        &[
            "--seeds",
            "1-40",
            "--manifest",
            "hydo/onboard/examples/shop/hydo.yaml",
            "--protocol-faults",
            "--strategy",
            "process",
        ],
    ),
    (
        "protocol_cover",
        &[
            "--seeds",
            "1-30",
            "--manifest",
            "hydo/onboard/examples/shop/hydo.yaml",
            "--protocol-cover",
        ],
    ),
    (
        "protocol_sweep",
        &[
            "--sweep",
            "protocol",
            "--manifest",
            "hydo/onboard/examples/shop/hydo.yaml",
        ],
    ),
    (
        "grpc_faults",
        &[
            "--seeds",
            "1-40",
            "--manifest",
            "hydo/onboard/examples/stack-grpc/hydo.yaml",
            "--grpc-faults",
            "--protocol-faults",
        ],
    ),
    (
        "stack_hist_checks",
        &["--seeds", "1-30", "--strategy", "swarm", "--new-faults"],
    ),
    (
        "errors_manifest",
        &[
            "--seeds",
            "1-30",
            "--manifest",
            "hydo/onboard/examples/redis-errors/hydo/hydo.yaml",
            "--new-faults",
            "--errors-shrink",
            "2",
        ],
    ),
    // no golden: cache_scenario() checks the result cache's key instead
    (CACHE_SCENARIO, &[]),
    // no golden: time_limit_scenario() checks where the cut is
    (TIME_LIMIT_SCENARIO, &[]),
    // no golden: merge_scenario() compares a merged exploration with one exploration
    (MERGE_SCENARIO, &[]),
    // no golden: repeats_scenario() compares an exploration with the same one in VMs only
    (REPEATS_SCENARIO, &[]),
    // no golden: restart_scenario() checks which errors are shrunk
    (RESTART_SCENARIO, &[]),
];

const CACHE_SCENARIO: &str = "cache_key_is_the_image_bytes";
const TIME_LIMIT_SCENARIO: &str = "time_limit_cuts_at_a_seed_boundary";
const MERGE_SCENARIO: &str = "merge_equals_one_exploration";
const REPEATS_SCENARIO: &str = "repeats_take_the_earlier_result";
const RESTART_SCENARIO: &str = "restart_errors_are_shrunk";

/// Milestone 339: an error of the system is shrunk like a violation, by its signature. On the
/// RAUC example's manifest the scripted engine ends a run whose service was killed for 3 steps
/// or more with `ERROR device restart failed`, and a run with a `G=` with `ERROR device restart
/// failed (its files were corrupted)`. The first is shrunk to its kill alone. The second is the
/// system's detection of the damage: it is shrunk only with `--shrink-detected-damage`, to its
/// `G=` alone.
fn restart_scenario() {
    let dir = scratch(RESTART_SCENARIO);
    let explore = |name: &str, extra: &[&str]| -> serde_json::Value {
        let out = dir.join(name);
        let mut c = Command::new(env!("CARGO_BIN_EXE_hydo-explore"));
        c.args([
            "--seeds",
            "1-40",
            "--strategy",
            "process",
            "--storage-faults",
            "--manifest",
            "hydo/onboard/examples/rauc/hydo.yaml",
            "--known-errors",
            KNOWN,
            "--errors-shrink",
            "8",
        ])
        .args(extra)
        .args(IMAGE)
        .args(["--workers", "3", "--out"])
        .arg(&out);
        run(&mut c, &dir, &out);
        let t = std::fs::read_to_string(out.join("exploration.json")).unwrap();
        serde_json::from_str(&t).unwrap()
    };
    const DAMAGE: &str = "its files were corrupted";
    // the guest tokens of a schedule, without the composition
    let faults = |s: &str| -> Vec<String> {
        let g = s.split_once("| run").map(|x| x.1).unwrap_or("");
        g.split_whitespace()
            .filter(|t| !t.starts_with("W="))
            .map(str::to_string)
            .collect()
    };
    let shrinks = |d: &serde_json::Value| -> Vec<(String, String)> {
        d["shrinks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["verdict"].as_str().unwrap_or("").to_string(),
                    s["minimal"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    };
    // the error kinds that were shrunk, each with the seed it was first new in
    let kinds = |d: &serde_json::Value| -> Vec<(String, i64, Option<String>)> {
        d["error_kinds"]["kinds"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|k| !k["shrink"].is_null())
                    .map(|k| {
                        (
                            k["signature"].as_str().unwrap_or("").to_string(),
                            k["first_new"]["seed"].as_i64().unwrap_or(0),
                            k["shrink"]["minimal"].as_str().map(str::to_string),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let damaged = |d: &serde_json::Value| -> Vec<i64> {
        d["failures"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| {
                f["run"]["verdict"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .contains(DAMAGE)
            })
            .filter_map(|f| f["run"]["seed"].as_i64())
            .collect()
    };
    let d = explore("plain", &[]);
    let failed: Vec<String> = d["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            f["run"]["verdict"]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(
        failed
            .iter()
            .any(|v| v.contains("restart failed") && !v.contains(DAMAGE)),
        "{failed:?}"
    );
    assert!(failed.iter().any(|v| v.contains(DAMAGE)), "{failed:?}");
    let s = shrinks(&d);
    let restart = s
        .iter()
        .find(|(v, _)| v.contains("restart failed") && !v.contains(DAMAGE))
        .unwrap_or_else(|| panic!("no shrink of the restart: {s:?}"));
    let f = faults(&restart.1);
    assert!(
        f.len() == 1 && f[0].starts_with("K=device@") && f[0].ends_with("+3"),
        "{restart:?}"
    );
    assert!(!s.iter().any(|(v, _)| v.contains(DAMAGE)), "{s:?}");
    // an error kind first new in a run where the system detected the damage is not shrunk
    let (k, dmg) = (kinds(&d), damaged(&d));
    assert!(!k.is_empty(), "the scenario shrinks error kinds");
    for (sig, sd, _) in &k {
        assert!(!dmg.contains(sd), "{sig} of seed {sd} was shrunk");
    }
    // hydo explore --merge of four parts shrinks the same errors as one exploration
    let parts: Vec<PathBuf> = (1..=4)
        .map(|i| {
            explore(&format!("part{i}"), &["--shard", &format!("{i}/4")]);
            dir.join(format!("part{i}"))
        })
        .collect();
    let o = Command::new(env!("CARGO_BIN_EXE_hydo-explore"))
        .arg("--merge")
        .args(&parts)
        .arg("--out")
        .arg(dir.join("merged"))
        .output()
        .expect("run");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = std::fs::read_to_string(dir.join("merged/exploration.json")).unwrap();
    let m: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(shrinks(&m), s, "the merged shrinks");
    assert_eq!(kinds(&m), k, "the merged error shrinks");
    let d = explore("damage", &["--shrink-detected-damage"]);
    let s = shrinks(&d);
    let damage = s
        .iter()
        .find(|(v, _)| v.contains(DAMAGE))
        .unwrap_or_else(|| panic!("no shrink of the detected damage: {s:?}"));
    let f = faults(&damage.1);
    assert!(f.len() == 1 && f[0].starts_with("G=device@"), "{damage:?}");
    // and the error kind of the system's own line about the damage
    let dmg = damaged(&d);
    assert!(
        kinds(&d).iter().any(|(_, sd, _)| dmg.contains(sd)),
        "{:?}",
        kinds(&d)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Milestone 339: a first run or a shrink candidate whose schedule the exploration already ran
/// takes the earlier run's result without a VM. The exploration gives the same records, file for
/// file, as with `--rerun-repeats`, which runs every one of them, and its summary counts the runs
/// answered so. Two explorations show it: one of the stack with shrinks of several signatures
/// and causes, error kinds and their shrinks, whose shrink candidates repeat; and one of a
/// project whose manifest allows clock steps only, where the random strategy draws
/// `disk=none dseed=0 | run` for every seed (the explorer writes `dseed=0` where the disk's seed
/// decides nothing).
fn repeats_scenario() {
    let dir = scratch(REPEATS_SCENARIO);
    let explore = |name: &str, args: &[&str], extra: &[&str]| -> PathBuf {
        let out = dir.join(name);
        let mut c = Command::new(env!("CARGO_BIN_EXE_hydo-explore"));
        c.args(args)
            .args(extra)
            .args(IMAGE)
            .args(["--workers", "3", "--out"])
            .arg(&out);
        run(&mut c, &dir, &out);
        out
    };
    let summary = |out: &Path| -> serde_json::Value {
        let t = std::fs::read_to_string(out.join("exploration.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&t).unwrap()["summary"].clone()
    };
    let stack: &[&str] = &[
        "--seeds",
        "1-40",
        "--strategy",
        "process",
        "--new-faults",
        "--shrink-causes",
        "2",
        "--errors-shrink",
        "3",
        "--known-errors",
        KNOWN,
    ];
    // the option itself is in the recorded command line
    let opt = regex::Regex::new(r#"\s*"--rerun-repeats","#).unwrap();
    let clock: &[&str] = &[
        "--seeds",
        "1-40",
        "--manifest",
        "hydo/onboard/examples/avx2/hydo.yaml",
    ];
    for (name, args) in [("stack", stack), ("clock", clock)] {
        let once = explore(&format!("{name}-once"), args, &[]);
        let vms = explore(&format!("{name}-vms"), args, &["--rerun-repeats"]);
        let (s1, s2) = (summary(&once), summary(&vms));
        let n = s1["repeats"].as_u64().unwrap_or(0);
        assert!(s1["repeats_saved_s"].as_f64().unwrap_or(0.0) > 0.0, "{s1}");
        assert!(s2.get("repeats").is_none(), "--rerun-repeats: {s2}");
        if name == "stack" {
            assert!(n >= 10, "the shrinks repeat candidates: {s1}");
            assert!(s1["failures"].as_u64().unwrap_or(0) > 0, "{s1}");
        } else {
            // the 40 first runs are one schedule, all submitted at once: one VM ran it
            assert_eq!(n, 39, "{s1}");
        }
        for f in ["exploration.json", "events.jsonl", "report.md"] {
            let clean = |out: &Path| {
                let t = std::fs::read_to_string(out.join(f)).unwrap();
                let t = opt.replace_all(&t, "").into_owned();
                norm(&t.replace(&out.display().to_string(), "<out>"))
            };
            let (a, b) = (clean(&once), clean(&vms));
            if a != b {
                let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
                let i = la
                    .iter()
                    .zip(&lb)
                    .position(|(x, y)| x != y)
                    .unwrap_or(la.len().min(lb.len()));
                panic!(
                    "{name} {f}: the answered repeats differ from their runs at line {}:\n  answered: {}\n  in VMs:   {}",
                    i + 1,
                    la.get(i).unwrap_or(&"<end>"),
                    lb.get(i).unwrap_or(&"<end>")
                );
            }
        }
        // a repeat gets the earlier run's consoles under its own tag
        if name == "clock" {
            let a = std::fs::read(once.join("consoles/seed-1.log")).unwrap();
            let b = std::fs::read(once.join("consoles/seed-40.log")).unwrap();
            assert_eq!(a, b);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `hydo explore --merge` (Milestone 304): the parts of a seed range explored apart with
/// `--shard I/N` and merged give the exploration of the whole range, file for file
/// (exploration.json, events.jsonl and report.md), apart from the wall times, the workers and
/// the merge's own `shards`. The options exercise every merged part: failures and their re-runs,
/// shrinks of several signatures and of several causes of one (renumbered across parts), error
/// kinds and their shrinks, the property table and the liveness summary, and the flush-count
/// probe of `--more-faults`. A missing part leaves its seeds in `missing_seeds`, and parts of two
/// explorations are refused.
fn merge_scenario() {
    let dir = scratch(MERGE_SCENARIO);
    let opts = [
        "--seeds",
        "1-60",
        "--timing",
        "--new-faults",
        "--concurrency",
        "--more-faults",
        "--buggify",
        "--shrink-causes",
        "2",
        "--errors-shrink",
        "3",
        "--known-errors",
        KNOWN,
        // a limit the parts never reach (explore.yml gives every part one)
        "--time-limit",
        "3600",
    ];
    let explore = |name: &str, extra: &[&str]| -> PathBuf {
        let out = dir.join(name);
        let mut c = Command::new(env!("CARGO_BIN_EXE_hydo-explore"));
        c.args(opts)
            .args(extra)
            .args(IMAGE)
            .args(["--workers", "3", "--out"])
            .arg(&out);
        run(&mut c, &dir, &out);
        out
    };
    let merge = |parts: &[PathBuf], out: &str| -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_hydo-explore"))
            .arg("--merge")
            .args(parts)
            .arg("--out")
            .arg(dir.join(out))
            .output()
            .expect("run")
    };
    let single = explore("single", &[]);
    let n = 7;
    let parts: Vec<PathBuf> = (1..=n)
        .map(|i| explore(&format!("part{i}"), &["--shard", &format!("{i}/{n}")]))
        .collect();
    // the parts in any order
    let mut shuffled = parts.clone();
    shuffled.reverse();
    let o = merge(&shuffled, "merged");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let merged = dir.join("merged");
    let clean = |out: &Path, t: &str| -> String {
        let t = t.replace(&out.display().to_string(), "<out>");
        let t = norm(&t.replace(&dir.display().to_string(), "<dir>"));
        // the merge's own record of its parts
        let re = regex::Regex::new(r#"(?s),\s*"shards": \[.*?\n  \]"#).unwrap();
        let t = re.replace_all(&t, "").into_owned();
        let re = regex::Regex::new(r"(?s)\n## Parts \(hydo explore --merge\)\n.*?\n\n## ").unwrap();
        re.replace_all(&t, "\n## ").into_owned()
    };
    for f in ["exploration.json", "events.jsonl", "report.md"] {
        let a = clean(&single, &std::fs::read_to_string(single.join(f)).unwrap());
        let b = clean(&merged, &std::fs::read_to_string(merged.join(f)).unwrap());
        if a != b {
            let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
            let i = la
                .iter()
                .zip(&lb)
                .position(|(x, y)| x != y)
                .unwrap_or(la.len().min(lb.len()));
            panic!(
                "{f}: the merge differs from one exploration at line {}:\n  one:    {}\n  merged: {}",
                i + 1,
                la.get(i).unwrap_or(&"<end>"),
                lb.get(i).unwrap_or(&"<end>")
            );
        }
    }
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(merged.join("exploration.json")).unwrap())
            .unwrap();
    assert_eq!(doc["shards"].as_array().unwrap().len(), n);
    assert_eq!(doc["shards"][0]["seeds"], "1-9");
    assert_eq!(doc["shards"][0]["index"], 1);
    assert_eq!(doc["shards"][0]["host_cpu"], doc["engine"]["host_cpu"]);
    assert!(doc.get("missing_seeds").is_none());
    assert!(
        doc["shrinks"].as_array().unwrap().len() >= 4,
        "the scenario shrinks several failures: {}",
        doc["shrinks"]
    );
    assert!(merged.join("consoles/seed-60.log").is_file());
    assert!(merged.join("consoles/min2-a.log").is_file());
    assert!(merged.join("parts/1/exploration.json").is_file());
    // a missing part: its seeds are named, the rest is merged
    let o = merge(&parts[..n - 1], "partial");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("partial/exploration.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(doc["missing_seeds"], "53-60");
    assert_eq!(doc["config"]["seeds"], "1-60");
    // parts of two explorations
    let other = explore("other", &["--strategy", "swarm", "--shard", "1/7"]);
    let o = merge(&[parts[1].clone(), other], "refused");
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("not one exploration"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--time-limit` (stage 2 of docs/design/ci.md, section 7.3) stops drawing first runs at a seed
/// boundary in seed order: the explored seeds are the first ones of the range, whatever runs were
/// in flight, and exploring exactly those seeds without a limit gives the same first runs.
fn time_limit_scenario() {
    let dir = scratch(TIME_LIMIT_SCENARIO);
    let explore = |n: u32, seeds: &str, limit: Option<&str>| -> (serde_json::Value, Vec<String>) {
        let out = dir.join(format!("out{n}"));
        let mut c = Command::new(env!("CARGO_BIN_EXE_hydo-explore"));
        c.args(["--seeds", seeds])
            .args(IMAGE)
            .args(["--workers", "2", "--out"])
            .arg(&out);
        if let Some(l) = limit {
            c.args(["--time-limit", l]);
        }
        let o = c
            .env("HYDO_FAKE_ENGINE", "1")
            .env("HYDO_SCRIPTED_ENGINE", engine_path())
            .env("HYDO_BUILD", &dir)
            .env_remove("HYDO_EXPLORE_CACHE")
            .env_remove("HYDO_QUIET_FILE")
            .env_remove("HYDO_BLESS")
            .env("HYDO_FAKE_HIST", "")
            .current_dir(top())
            .output()
            .expect("run");
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let t = std::fs::read_to_string(out.join("exploration.json")).expect("exploration.json");
        let doc: serde_json::Value = serde_json::from_str(&t).expect("json");
        let ev = std::fs::read_to_string(out.join("events.jsonl")).expect("events.jsonl");
        let first: Vec<String> = ev
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|r| r["type"] == "run" && r["role"] == "first")
            .map(|r| format!("{} {}", r["seed"], r["fingerprints"]["fp"]))
            .collect();
        (doc["summary"].clone(), first)
    };
    // a limit that has passed when the first run comes back
    let (s, first) = explore(1, "1-40", Some("0.000001"));
    let tl = &s["time_limit"];
    assert_eq!(tl["cut"], true, "{s}");
    assert_eq!(tl["planned"], 40);
    let n = tl["explored"].as_u64().unwrap();
    assert!(n < 40, "{s}");
    assert_eq!(s["schedules"], n);
    assert_eq!(first.len() as u64, n);
    for (i, f) in first.iter().enumerate() {
        assert!(
            f.starts_with(&format!("{} ", i + 1)),
            "seed order: {first:?}"
        );
    }
    if n > 0 {
        assert_eq!(tl["explored_seeds"], format!("1-{n}"));
        let (s2, again) = explore(2, &format!("1-{n}"), None);
        assert!(s2.get("time_limit").is_none());
        assert_eq!(again, first, "the same seeds give the same first runs");
    }
    // a limit the exploration never reaches
    let (s3, all) = explore(3, "1-6", Some("3600"));
    assert_eq!(s3["time_limit"]["cut"], false);
    assert_eq!(s3["time_limit"]["explored_seeds"], "1-6");
    assert_eq!(all.len(), 6);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The result cache (`HYDO_EXPLORE_CACHE`) answers a schedule only for the same image bytes
/// (docs/design/ci.md section 7.2): an exploration run twice is answered from the cache the second
/// time, and once the image at the same path has other bytes, nothing is.
fn cache_scenario() {
    let dir = scratch(CACHE_SCENARIO);
    let img = dir.join(IMAGE[1]);
    let cache = dir.join("cache.jsonl");
    let explore = |n: u32| -> serde_json::Value {
        let out = dir.join(format!("out{n}"));
        let o = Command::new(env!("CARGO_BIN_EXE_hydo-explore"))
            .args(["--seeds", "1-4"])
            .args(IMAGE)
            .args(["--workers", "1", "--out"])
            .arg(&out)
            .env("HYDO_FAKE_ENGINE", "1")
            .env("HYDO_SCRIPTED_ENGINE", engine_path())
            .env("HYDO_BUILD", &dir)
            .env("HYDO_EXPLORE_CACHE", &cache)
            .env_remove("HYDO_QUIET_FILE")
            .env_remove("HYDO_BLESS")
            .env("HYDO_FAKE_HIST", "")
            .current_dir(top())
            .output()
            .expect("run");
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let t = std::fs::read_to_string(out.join("exploration.json")).expect("exploration.json");
        serde_json::from_str::<serde_json::Value>(&t).expect("json")["summary"]["cache_hits"]
            .clone()
    };
    std::fs::write(&img, b"the first image").unwrap();
    assert_eq!(explore(1), serde_json::Value::Null, "an empty cache");
    assert_eq!(explore(2), serde_json::json!(4), "the same image again");
    std::fs::write(&img, b"another image at the same path").unwrap();
    assert_eq!(
        explore(3),
        serde_json::Value::Null,
        "another image at the same path must not be answered from the cache"
    );
    assert_eq!(explore(4), serde_json::json!(4), "that image again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The scripted engine (fake_engine.py, ported line for line).
mod engine {
    use regex::Regex;
    use sha2::{Digest, Sha256};
    use std::io::{BufRead, Write};
    use std::path::Path;

    const G: u64 = 0x9E37_79B9_7F4A_7C15;

    fn mix64(x: u64) -> u64 {
        let mut z = x.wrapping_add(G);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn h(text: &str, salt: &str) -> String {
        Sha256::digest(format!("{salt}{text}").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn hx(s: &str) -> u64 {
        u64::from_str_radix(s, 16).unwrap()
    }

    /// A compiled pattern, compiled once per pattern (Python's `re` caches them too).
    fn re(p: &str) -> Regex {
        thread_local! {
            static C: std::cell::RefCell<std::collections::HashMap<String, Regex>> =
                std::cell::RefCell::default();
        }
        C.with(|c| {
            c.borrow_mut()
                .entry(p.to_string())
                .or_insert_with(|| Regex::new(p).unwrap())
                .clone()
        })
    }

    /// The tick delays a host part applies (tjx= or a seeded tj= over ticks 3..80).
    fn delays(host: &str) -> Option<Vec<(i64, i64)>> {
        if let Some(m) = re(r"tjx=(\S+)").captures(host) {
            return Some(
                m[1].split(',')
                    .map(|kd| {
                        let (k, d) = kd.split_once(':').unwrap();
                        (k.parse().unwrap(), d.parse().unwrap())
                    })
                    .collect(),
            );
        }
        let m = re(r"\btj=([0-9a-f]+)").captures(host)?;
        let g = |k: &str, d: i64| -> i64 {
            re(&format!(r"{k}=(\d+)"))
                .captures(host)
                .map(|c| c[1].parse().unwrap())
                .unwrap_or(d)
        };
        let seed = u128::from_str_radix(&m[1], 16).unwrap() as u64;
        let (pm, mx, lo, hi) = (
            g("tjpm", 10),
            g("tjmax", 500000),
            g("tjlo", 0),
            g("tjhi", 0),
        );
        let mut out = Vec::new();
        for k in 4.max(lo)..81 {
            if hi != 0 && k >= hi {
                break;
            }
            let x = mix64(seed.wrapping_add((k as u64).wrapping_mul(G)));
            if (x % 1000) < pm as u64 {
                out.push((k, 1 + ((x >> 16) % mx as u64) as i64));
            }
        }
        Some(out)
    }

    fn tok<'a>(guest: &'a str, k: &str) -> Option<&'a str> {
        let p = format!("{k}=");
        guest
            .split_whitespace()
            .find_map(|t| t.strip_prefix(p.as_str()))
    }

    fn fc_count(f: &str, step: i64) -> i64 {
        (40 - step) * 3 + (f.chars().count() % 5) as i64
    }

    fn nonempty(x: Option<&str>) -> Option<&str> {
        x.filter(|s| !s.is_empty())
    }

    fn after(s: &str, sep: char, i: usize) -> &str {
        s.split(sep).nth(i).unwrap()
    }

    pub fn answer(consoles: &Path, tag: &str, sched: &str) -> String {
        let (host, guest) = sched.split_once('|').unwrap();
        let mut lines = vec![format!("hydo-fake: run {sched}")];
        let mut rec_lines: Option<Vec<String>> = None;
        let secs = format!("{:.1}", 1.0 + (hx(&h(sched, "")[..4]) % 90) as f64 / 10.0);
        let mut extras = String::new();
        let d = delays(host);
        if let Some(d) = &d {
            extras.push_str(&format!(" tjk=3-80 tjn={}", d.len()));
        }
        if host.split_whitespace().any(|t| t == "cfn") {
            let mut e = std::io::stderr();
            let _ = writeln!(e, "FLUSHCUT {tag} target=0 flushes=37");
            let _ = e.flush();
        }
        let (k, n, c, m, r) = (
            tok(guest, "K"),
            tok(guest, "N"),
            tok(guest, "C"),
            tok(guest, "M"),
            tok(guest, "R"),
        );
        let (p, t, o, w, f, q, x) = (
            tok(guest, "P"),
            tok(guest, "T"),
            tok(guest, "O"),
            tok(guest, "W"),
            tok(guest, "F"),
            tok(guest, "Q"),
            tok(guest, "X"),
        );
        let hs: Vec<&str> = guest
            .split_whitespace()
            .filter_map(|t| t.strip_prefix("H="))
            .collect();
        let cf = re(r"\bcf=(\d+)").captures(host);
        let cut = c.is_some() || cf.is_some();
        let relaxed = !matches!(m, None | Some("1"))
            || !matches!(r, None | Some("always"))
            || guest.split_whitespace().any(|t| {
                let p2: String = t.chars().take(2).collect();
                ["A=", "S=", "D="].contains(&p2.as_str()) && !t.ends_with("=always")
            });
        let (kk, nn, pp, oo, ww, qq, xx) = (
            nonempty(k),
            nonempty(n),
            nonempty(p),
            nonempty(o),
            nonempty(w),
            nonempty(q),
            nonempty(x),
        );
        let dd = d.as_ref().filter(|v| !v.is_empty());
        let mut verdict = "HYDO-ORACLE: OK acked=30".to_string();
        if let Some(f) = f {
            let mm = re(r"^([a-z0-9_]+)([<>])([a-z0-9_]+)@(\d+)#(\d+)(?::([^:]*))?:(\d+)")
                .captures(f)
                .unwrap();
            let (svc, dr, peer) = (&mm[1], &mm[2], &mm[3]);
            let st: i64 = mm[4].parse().unwrap();
            let nfc: i64 = mm[5].parse().unwrap();
            let matched = mm.get(6).map(|x| x.as_str());
            let key = format!("{}{}", after(f, '@', 0), matched.unwrap_or(""));
            let plus = if matched.is_some_and(|s| !s.is_empty()) {
                3
            } else {
                0
            };
            if nfc == 0 {
                let cnt = fc_count(&key, st);
                lines.push(format!(
                    "hydo-fc: freeze never fired: {cnt} of 0 matching events after step {st}"
                ));
                for kk in 1..=cnt.min(50) {
                    let (a, b) = if dr == ">" {
                        ("req", "resp")
                    } else {
                        ("resp", "req")
                    };
                    lines.push(format!(
                        "hydo-fc-trace: {a}{kk} len=10 t={}",
                        1000 + st * 100 + kk * 7
                    ));
                    lines.push(format!(
                        "hydo-fc-trace: {b}{kk} len=12 t={}",
                        1003 + st * 100 + kk * 7
                    ));
                }
            } else {
                let word = if dr == ">" { "req" } else { "resp" };
                lines.push(format!(
                    "hydo-fc: FROZE {svc} at {word} #{nfc} ({word}{}, len 10)",
                    nfc + plus
                ));
                if (st == 8 || st == 9) && (20..=27).contains(&(nfc + plus)) {
                    verdict = format!("HYDO-ORACLE: VIOLATION frozen-{peer} order={st}");
                }
            }
        } else if host.contains("disk=lying") {
            let cn: i64 = match c {
                Some(s) if !s.is_empty() => s.parse().unwrap(),
                _ => 3,
            };
            verdict = format!("HYDO-ORACLE: VIOLATION lost acknowledged write order={cn}");
        } else if host.contains("disk=ioerr") && hx(&h(sched, "")[..2]).is_multiple_of(3) {
            verdict = "ERROR harness: guest did not finish (I/O error)".into();
        } else if cut && relaxed {
            let at = match c {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => cf.as_ref().unwrap()[1].to_string(),
            };
            verdict = format!("HYDO-ORACLE: VIOLATION lost acknowledged write order={at}");
        } else if kk.is_some() && nn.is_some_and(|nv| nv.contains(after(kk.unwrap(), '@', 0))) {
            let kv = kk.unwrap();
            verdict = format!(
                "HYDO-ORACLE: VIOLATION duplicate in {} order={}",
                after(kv, '@', 0),
                after(after(kv, '@', 1), '+', 0)
            );
        } else if pp.is_some() && dd.is_some_and(|v| v.iter().any(|(k, _)| k % 5 == 1)) {
            verdict = format!(
                "HYDO-ORACLE: VIOLATION race between orders order={}",
                dd.unwrap().len()
            );
        } else if xx.is_some() && qq.is_some() {
            verdict = format!(
                "HYDO-ORACLE: VIOLATION front lost an order order={}",
                after(after(xx.unwrap(), '@', 1), '~', 0)
            );
        } else if ww.is_some_and(|wv| {
            re(r",p([2-9])").is_match(wv) && hx(&h(wv, "")[..2]).is_multiple_of(2)
        }) {
            verdict = format!(
                "HYDO-ORACLE: VIOLATION lost update key=k{}",
                hx(&h(ww.unwrap(), "")[2..4]) % 5
            );
        } else if !hs.is_empty()
            && hs
                .iter()
                .any(|x| x.contains("drop") || x.contains("status=503"))
        {
            verdict = format!(
                "HYDO-ORACLE: VIOLATION charged twice order={}",
                after(after(hs[0], '@', 1), '-', 0)
            );
        } else if oo.is_some_and(|ov| ov.contains("loss")) && kk.is_some() {
            verdict = format!(
                "HYDO-ORACLE: VIOLATION stale read order={}",
                after(after(oo.unwrap(), '@', 1), '-', 0)
            );
        }
        // the turn sweep's schedules (M205; dseed=8363, which no generator draws): the shape of
        // nats-server's flaky test on two vCPUs, passing with turns up to 18,700 branches
        if host.split_whitespace().any(|t| t == "dseed=8363") {
            if let Some(q) = re(r"\bq=(\d+)").captures(host) {
                let q: u64 = q[1].parse().unwrap();
                verdict = if q <= 18750 {
                    "HYDO-ORACLE: OK pass=2 of 2".into()
                } else {
                    "HYDO-ORACLE: VIOLATION [ats.test_passes] pass=1 fail=1 3 vs 2".into()
                };
            }
        }
        // the KVM turn scenarios (HYDO_FAKE_KVM_TURNS): nats-server's flaky test on the KVM
        // engine with two vCPUs (its RESULTS.md), which passes with turns of 3 and of 8 to 32
        // exits and fails otherwise, the default turns of 256 exits too
        if std::env::var("HYDO_FAKE_KVM_TURNS").is_ok_and(|v| !v.is_empty()) {
            let q: u64 = re(r"(?:^|\s)q=(\d+)")
                .captures(host)
                .map(|c| c[1].parse().unwrap())
                .unwrap_or(256);
            verdict = if q == 3 || (8..=32).contains(&q) {
                "HYDO-ORACLE: OK pass=2 of 2".into()
            } else {
                "HYDO-ORACLE: VIOLATION [ats.test_passes] pass=1 fail=1 3 vs 2".into()
            };
        }
        // the steered search's scenario (M209; HYDO_FAKE_STEER, set for the scenarios named
        // steer_*): a distance the guest reports in HYDO-STEER records, |K step - 17| + |K down
        // - 3| (40 without a K=), and the only failure is at distance 0
        if std::env::var("HYDO_FAKE_STEER").is_ok_and(|v| !v.is_empty()) {
            let dist = match kk {
                Some(kv) => {
                    let at: i64 = after(after(kv, '@', 1), '+', 0).parse().unwrap();
                    let down: i64 = after(kv, '+', 1).parse().unwrap_or(1);
                    (at - 17).abs() + (down - 3).abs()
                }
                None => 40,
            };
            lines.push(format!(
                "HYDO-STEER {{\"id\":\"fake.distance\",\"goal\":\"min\",\"v\":{dist},\"msg\":\"how far the kill is from the bug\"}}"
            ));
            verdict = if dist == 0 {
                "HYDO-ORACLE: VIOLATION killed in the window order=17".into()
            } else {
                "HYDO-ORACLE: OK acked=30".into()
            };
        }
        // the restart scenario (Milestone 339; HYDO_FAKE_RESTART, set for the scenarios named
        // restart_*): a service whose files a G= damaged does not start again, as a system that
        // detects the damage refuses to; and a service killed for 3 steps or more does not start
        // again
        if std::env::var("HYDO_FAKE_RESTART").is_ok_and(|v| !v.is_empty()) {
            if let Some(gv) = nonempty(tok(guest, "G")) {
                verdict = format!(
                    "HYDO-ORACLE: ERROR {} restart failed (its files were corrupted)",
                    after(gv, '@', 0)
                );
            } else if let Some(kv) = kk {
                let down: i64 = after(kv, '+', 1).parse().unwrap_or(1);
                if down >= 3 {
                    verdict = format!("HYDO-ORACLE: ERROR {} restart failed", after(kv, '@', 0));
                }
            }
        }
        // properties: a failed always-property for clock jumps backwards, an eventually with a timing
        let ok_e = !oo.is_some_and(|ov| ov.contains("delay"));
        let el = 0.5 + (hx(&h(sched, "e")[..3]) % 300) as f64 / 100.0;
        lines.push(format!(
            "HYDO-ASSERT {{\"t\":\"eventually\",\"id\":\"orders.drained\",\"ok\":{},\"details\":{{\"elapsed_s\":\"{el:.2}\",\"out\":\"pending={}\"}}}}",
            if ok_e { "true" } else { "false" },
            hx(&h(sched, "o")[..2])
        ));
        if let Some(tv) = nonempty(t) {
            let dv = after(tv, ':', 1);
            if dv.starts_with('-') && dv.parse::<i64>().unwrap() < -3600 {
                lines.push(
                    "HYDO-ASSERT {\"t\":\"always\",\"id\":\"clock.monotonic\",\"ok\":false,\"details\":{}}"
                        .into(),
                );
            }
        }
        lines.push(format!(
            "HYDO-ASSERT {{\"t\":\"sometimes\",\"id\":\"retry.taken\",\"ok\":{}}}",
            if kk.is_some() { "true" } else { "false" }
        ));
        // the world's record of a `--protocol-cover` probe (Milestone 257): each request that an
        // H= token with a delay under 100 ms held, on the routes the shop example calls in each
        // of its 12 steps (a charge and an email every step, a refund at steps 3, 7 and 11)
        let mut wseq = 0;
        for step in 0..12i64 {
            for route in ["payments.charge", "payments.refund", "email.send"] {
                if route == "payments.refund" && step % 4 != 3 {
                    continue;
                }
                let d = hs.iter().find_map(|x| {
                    let (tgt, rest) = x.split_once('@')?;
                    let (win, rule) = rest.split_once(':')?;
                    let (a, b) = win.split_once('-')?;
                    let (a, b): (i64, i64) = (a.parse().ok()?, b.parse().ok()?);
                    let d: i64 = rule.strip_prefix("delay=")?.parse().ok()?;
                    let on =
                        tgt == route || route.strip_prefix(tgt).is_some_and(|r| r.starts_with('.'));
                    (on && a <= step && step < b && d < 100).then_some(d)
                });
                if let Some(d) = d {
                    wseq += 1;
                    lines.push(format!(
                        "hydo-world: {wseq} {route} ref=o{step} idem=k fx=1 ok+delay{d}"
                    ));
                }
            }
        }
        if wseq > 0 {
            lines.push("hydo-world: END".into());
        }
        // error logs: a kind in runs with a kill (new unless the list knows it), one before the workload
        let mut el_recs = Vec::new();
        if let Some(kv) = kk {
            let svc = after(kv, '@', 0);
            el_recs.push(format!(
                "HYDO-ERRLOG {{\"svc\":\"{svc}\",\"src\":\"log\",\"n\":2,\"at\":\"{}\",\"tpl\":\"connection to {svc} lost <n>\",\"ex\":\"connection to {svc} lost 3\"}}",
                after(after(kv, '@', 1), '+', 0)
            ));
        }
        if pp.is_some() {
            el_recs.push("HYDO-ERRLOG {\"svc\":\"app\",\"src\":\"log\",\"n\":1,\"at\":\"end\",\"tpl\":\"slow request <n> ms\",\"ex\":\"slow request 900 ms\"}".into());
        }
        el_recs.push("HYDO-ERRLOG {\"svc\":\"app\",\"src\":\"log\",\"n\":1,\"at\":\"s\",\"tpl\":\"config reloaded\",\"ex\":\"config reloaded\"}".into());
        // the restart scenario (Milestone 339): the line of a system that found its damaged files
        if std::env::var("HYDO_FAKE_RESTART").is_ok_and(|v| !v.is_empty()) {
            if let Some(gv) = nonempty(tok(guest, "G")) {
                el_recs.push(format!(
                    "HYDO-ERRLOG {{\"svc\":\"{}\",\"src\":\"log\",\"n\":1,\"at\":\"end\",\"tpl\":\"checksum mismatch in block <n>\",\"ex\":\"checksum mismatch in block 7\"}}",
                    after(gv, '@', 0)
                ));
            }
        }
        lines.extend(el_recs.iter().cloned());
        let hist = std::env::var("HYDO_FAKE_HIST").is_ok_and(|v| !v.is_empty());
        if hist {
            // a history check (hydo-history on the host)
            lines.push("HYDO-CHECK {\"id\":\"kv.lin\",\"type\":\"linearizable\",\"model\":\"register\",\"history\":\"kv\"}".into());
            let seen = if kk.is_some_and(|kv| kv.contains("redis")) {
                "nil"
            } else {
                "1"
            };
            let read = format!("1 ok read x {seen}");
            for (i, l) in [
                "0 invoke write x 1",
                "0 ok write x 1",
                "1 invoke read x",
                read.as_str(),
            ]
            .iter()
            .enumerate()
            {
                let mut it = l.splitn(3, ' ');
                let (p_, ty, rest) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
                lines.push(format!(
                    "HYDO-HIST kv {p_} {ty} {} {rest}",
                    (i + 1) * 1000000
                ));
            }
        }
        lines.push(format!(
            "HYDO-ERRLOG-SUM {{\"boot\":\"first\",\"steps\":30,\"lines\":{0},\"kinds\":{0},\"shown\":{0}}}",
            el_recs.len()
        ));
        if cut {
            rec_lines = Some(vec![
                format!("hydo-fake: recovery of {tag}"),
                "HYDO-ERRLOG-SUM {\"boot\":\"recovery\",\"steps\":30,\"lines\":0,\"kinds\":0,\"shown\":0}"
                    .into(),
                verdict.clone(),
            ]);
        } else {
            lines.push(verdict.clone());
        }
        // the real engines append to a reused tag's console; with a history check the scripted one
        // starts it afresh (explore.py reused shrink tags across representatives, docs/HISTORY.md M102)
        let mut opts = std::fs::OpenOptions::new();
        if hist {
            opts.write(true).create(true).truncate(true);
        } else {
            opts.append(true).create(true);
        }
        let mut fh = opts.open(consoles.join(format!("{tag}.log"))).unwrap();
        fh.write_all(format!("{}\r\n", lines.join("\r\n")).as_bytes())
            .unwrap();
        if let Some(rl) = rec_lines {
            let mut fh = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(consoles.join(format!("{tag}.recovery.log")))
                .unwrap();
            fh.write_all(format!("{}\n", rl.join("\n")).as_bytes())
                .unwrap();
        }
        format!(
            "RESULT {tag} fp={} wfp={} secs={secs}{extras} verdict={verdict}",
            &h(sched, "fp")[..16],
            &h(sched, "w")[..16]
        )
    }

    /// `serve <consoles>`: READY, then a RESULT for every `<tag> <schedule>` line until EOF;
    /// `one <consoles> <schedule>`: one RESULT (`hydo_explore::scripted`).
    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let consoles = Path::new(&args[1]);
        std::fs::create_dir_all(consoles).unwrap();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        if args[0] == "one" {
            writeln!(out, "{}", answer(consoles, "one", &args[2])).unwrap();
            return;
        }
        writeln!(out, "READY boot_secs=2.5").unwrap();
        out.flush().unwrap();
        let stdin = std::io::stdin();
        let mut buf = String::new();
        let mut rd = stdin.lock();
        loop {
            buf.clear();
            if rd.read_line(&mut buf).unwrap() == 0 {
                return;
            }
            let line = buf.strip_suffix('\n').unwrap_or(&buf);
            let (tag, sched) = line.split_once(' ').unwrap();
            writeln!(out, "{}", answer(consoles, tag, sched)).unwrap();
            out.flush().unwrap();
        }
    }
}
