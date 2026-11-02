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

use std::path::{Path, PathBuf};
use std::process::Command;

/// The image every scenario names (the scripted engine never opens it). Until M146 the scenarios
/// took the explorer's default, `explore-disk.img`; that default is gone (a shell-init image), and
/// the goldens were re-recorded once for the new name.
const IMAGE: [&str; 2] = ["--image", "scripted-disk.img"];

fn main() {
    if std::env::var_os("HYDO_FAKE_ENGINE").is_some_and(|v| v == "1") {
        engine::main();
        return;
    }
    let only: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let list: Vec<&(&str, &[&str])> = SCENARIOS
        .iter()
        .filter(|(n, _)| only.is_empty() || only.iter().any(|o| n.contains(o.as_str())))
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

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-explore-fake-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d.canonicalize().expect("scratch")
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
    re3.replace_all(&t, "").into_owned()
}

/// What an exploration writes (M115: the results format and report.md, its view for people).
const FILES: [&str; 3] = ["events.jsonl", "exploration.json", "report.md"];

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
    (
        "stack_sweep_frontkill",
        &["--sweep", "frontkill", "--timing"],
    ),
    (
        "stack_manual",
        &["--schedule", "disk=cut dseed=0 | run C=4 R=everysec"],
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
];

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
