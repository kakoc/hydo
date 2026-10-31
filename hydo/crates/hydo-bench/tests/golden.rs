//! hydo-bench against the Python tools' recorded answers (tests/golden/, recorded on the Mac from
//! the Python of commit 2a58c4b before it was deleted; docs/HISTORY.md M108):
//!
//! - schedules.golden: run.py `schedules` (and bench_more_faults.py's added X=/cf=) for every
//!   seeded strategy x --new-faults x --buggify x --concurrency x --more-faults (--cf-range 1-309),
//!   and --vcpus 2 --turns, on the seeds of repetitions 1-3 with budget 60, with each schedule's
//!   finding class under both knob rules; per configuration the count, the SHA-256 of the lines
//!   "<seed> <class cut> <class any> <schedule>\n" and the first line;
//! - addbuggify.golden: addbuggify.py on the nine inputs of build-bench-bg.sh (control and d1-d7 on
//!   base/main-0112b48.go plus the defect patch, d0 on base/main-7e32c4a.go) and on two bases that
//!   already carry the wrappers: the report line and the SHA-256 of the rewritten file;
//! - d7prob-*.golden: d7prob.py with 20,000 and 200,000 seeds;
//! - table-*.golden: run.py --table on result folders of M46, M56, M59, M72 and the several-vCPU
//!   cells (tests/fixtures/results, copied from the bare-metal machine's /var/tmp);
//! - depth1map-*: depth1map.py on M55's tail sweeps of d7 and its control, both ways, with --sig
//!   and --json.

use hydo_bench::{analysis, buggify, cells};
use hydo_explore::gen::Ex;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn here() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(here().join("tests/golden").join(name)).expect("a golden file")
}

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn cell(strategy: &str, nf: bool, bg: bool, cc: bool, mf: bool, turns: bool) -> cells::Cell {
    cells::Cell {
        strategy: strategy.into(),
        knob_rule_any: false,
        new_faults: nf,
        buggify: bg,
        concurrency: cc,
        more_faults: mf,
        cf_range: if mf { Some((1, 309)) } else { None },
        vcpus: if turns { 2 } else { 1 },
        turns,
        window: 0,
        reps: vec![1, 2, 3],
        budget: 60,
        workers: 1,
        out: PathBuf::new(),
    }
}

fn config_line(c: &cells::Cell, label: &str) -> String {
    let seeds: Vec<i64> = (1..=3)
        .flat_map(|r| (1..=60).map(move |i| r * 1000 + i))
        .collect();
    let ex = c.ex();
    let any = Ex {
        knob_rule_any: true,
        ..Ex::default()
    };
    let cut = Ex::default();
    let items = cells::schedules(&ex, &c.strategy, &seeds).expect("schedules");
    let lines: Vec<String> = items
        .iter()
        .map(|(sd, t)| {
            let s = cut.parse(t).expect("parses");
            format!(
                "{sd} {} {} {t}",
                cut.finding_class(&s),
                any.finding_class(&any.parse(t).expect("parses"))
            )
        })
        .collect();
    let blob = lines.join("\n") + "\n";
    format!(
        "{label}|{}|{}|{}",
        lines.len(),
        hex(blob.as_bytes()),
        lines[0]
    )
}

#[test]
fn schedules_and_classes_are_run_py_s() {
    let mut got = Vec::new();
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for bits in 0..16u32 {
            // itertools.product((False, True), repeat=4) order: nf, bg, cc, mf
            let (nf, bg, cc, mf) = (bits & 8 != 0, bits & 4 != 0, bits & 2 != 0, bits & 1 != 0);
            let label = format!(
                "{st} nf={} bg={} cc={} mf={} turns=0",
                nf as u8, bg as u8, cc as u8, mf as u8
            );
            got.push(config_line(&cell(st, nf, bg, cc, mf, false), &label));
        }
    }
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for cc in [false, true] {
            let label = format!("{st} nf=0 bg=0 cc={} mf=0 turns=1", cc as u8);
            got.push(config_line(
                &cell(st, false, false, cc, false, true),
                &label,
            ));
        }
    }
    let want: Vec<String> = golden("schedules.golden")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g, w);
    }
}

#[test]
fn labels_are_run_py_s() {
    assert_eq!(
        cell("random", true, false, true, false, false).label(),
        "random+nf+cc"
    );
    assert_eq!(
        cell("process", true, true, true, false, false).label(),
        "process+bg+cc"
    );
    assert_eq!(
        cell("swarm", false, false, false, false, true).label(),
        "swarm+v2+turns"
    );
    assert_eq!(
        cell("process", false, false, false, true, false).label(),
        "process+xcf"
    );
}

fn sh(cmd: &str, dir: &Path) {
    let st = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .status()
        .expect("sh");
    assert!(st.success(), "{cmd}");
}

#[test]
fn addbuggify_is_addbuggify_py() {
    let bench = here().join("../../bench");
    let work = std::env::temp_dir().join(format!("hydo-bench-bg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    for line in golden("addbuggify.golden").lines() {
        let f: Vec<&str> = line.split('|').collect();
        let (name, report, sha) = (f[0], f[1], f[2]);
        let d = work.join(name);
        std::fs::create_dir_all(&d).expect("dir");
        let base = match name {
            "d0-old-front-door" => "main-7e32c4a.go".to_string(),
            n if n.starts_with("main-") => format!("{n}.go"),
            _ => "main-0112b48.go".to_string(),
        };
        std::fs::copy(bench.join("base").join(&base), d.join("main.go")).expect("copy");
        if name.starts_with('d') && name != "d0-old-front-door" {
            let p = bench.join("patches").join(format!("{name}.patch"));
            sh(&format!("patch -s -p1 < '{}'", p.display()), &d);
        }
        let src = std::fs::read_to_string(d.join("main.go")).expect("read");
        let (out, counts) = buggify::rewrite(&src);
        assert_eq!(buggify::report("main.go", &counts), report, "{name}");
        assert_eq!(hex(out.as_bytes()), sha, "{name}");
    }
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn d7prob_is_d7prob_py() {
    assert_eq!(analysis::d7prob(20000), golden("d7prob-20000.golden"));
    assert_eq!(analysis::d7prob(200000), golden("d7prob-200000.golden"));
}

#[test]
fn tables_are_run_py_s() {
    let f = here().join("tests/fixtures/results");
    let t = |dirs: &[&str]| {
        cells::table(&dirs.iter().map(|d| f.join(d)).collect::<Vec<_>>()).expect("table")
    };
    assert_eq!(
        t(&[
            "m46-bench/random",
            "m46-bench/swarm",
            "m46-bench/random-nf",
            "m46-bench/swarm-nf",
            "m46-bench/portfolio"
        ]),
        golden("table-m46.golden")
    );
    assert_eq!(
        t(&[
            "m46-bench/random",
            "m46-bench/swarm",
            "m56-bg/random-bg",
            "m56-bg/swarm-bg"
        ]),
        golden("table-m56.golden")
    );
    assert_eq!(
        t(&[
            "m59-bench/random-cc",
            "m59-bench/swarm-cc",
            "m59-bench/process-cc",
            "m59-bench/swarm-bg-cc",
            "m59-bench/random-nocc"
        ]),
        golden("table-m59.golden")
    );
    assert_eq!(
        t(&[
            "bench2/a",
            "bench2/b",
            "bench2/c",
            "bench2/d",
            "bench2/rb",
            "bench2/rd"
        ]),
        golden("table-smp.golden")
    );
    assert_eq!(
        t(&["kr/process-xcf-kr", "kr/swarm-xcf-kr", "nothing-here"]),
        golden("table-kr.golden")
    );
}

#[test]
fn depth1map_is_depth1map_py() {
    let f = here().join("tests/fixtures/results/m55-tail");
    let d7 = f.join("sweep-explore-bench-d7-skip-last-record");
    let nf = f.join("sweep-explore-nf");
    let (text, json) =
        analysis::depth1map(&d7, &nf, r"present \d+ times|kafka_exactly_once").expect("map");
    assert_eq!(text, golden("depth1map-d7.golden"));
    assert_eq!(json, golden("depth1map-d7.json"));
    let (text, _) = analysis::depth1map(&nf, &d7, "").expect("map");
    assert_eq!(text, golden("depth1map-rev.golden"));
}

/// The same sweeps in the results format (M115): their first runs as an events.jsonl give the
/// same map.
#[test]
fn depth1map_reads_the_results_format() {
    let f = here().join("tests/fixtures/results/m55-tail");
    let tmp = std::env::temp_dir().join(format!("hydo-bench-d1m-{}", std::process::id()));
    let conv = |name: &str| {
        let src = f.join(name);
        let pts: std::collections::HashMap<i64, String> =
            std::fs::read_to_string(src.join("sweep-points.jsonl"))
                .unwrap()
                .lines()
                .map(|l| {
                    let v: serde_json::Value = serde_json::from_str(l).unwrap();
                    (
                        v["seed"].as_i64().unwrap(),
                        v["point"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
        let mut out = String::new();
        for l in std::fs::read_to_string(src.join("runs.jsonl"))
            .unwrap()
            .lines()
        {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            let sd = v["seed"].as_i64().unwrap();
            let verdict = v["verdict"].as_str().unwrap();
            let mut r = hydo_results::Run::new(
                v["schedule"].as_str().unwrap(),
                hydo_results::Verdict {
                    kind: hydo_explore::report::verdict_kind(verdict).into(),
                    text: verdict.into(),
                    ..Default::default()
                },
                v["fp"].as_str().unwrap(),
            );
            r.seed = Some(sd);
            r.role = Some(hydo_results::role::FIRST.into());
            r.point = pts.get(&sd).cloned();
            out.push_str(&hydo_results::to_line(&r));
        }
        let d = tmp.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(hydo_results::EVENTS), out).unwrap();
        d
    };
    let d7 = conv("sweep-explore-bench-d7-skip-last-record");
    let nf = conv("sweep-explore-nf");
    let (text, json) =
        analysis::depth1map(&d7, &nf, r"present \d+ times|kafka_exactly_once").expect("map");
    assert_eq!(text, golden("depth1map-d7.golden"));
    assert_eq!(json, golden("depth1map-d7.json"));
    let _ = std::fs::remove_dir_all(&tmp);
}
