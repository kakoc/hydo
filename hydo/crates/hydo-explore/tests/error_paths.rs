//! Error paths (Milestone 306, docs/design/error-paths.md): a run's line of hits becomes the
//! result's `paths`, it never changes the verdict, and an exploration's first runs give each
//! place the runs that reached it, the first schedule, and whether only runs with faults did.

use hydo_contracts::paths::{functions_line, hits_line, Hits, PathPlace, PathSite};
use hydo_contracts::plan::*;
use hydo_results::RunPaths;
use serde_json::{json, Map};

fn place(id: u32, kind: &str, file: &str, line: u32, function: &str) -> PathPlace {
    PathPlace {
        id,
        kind: kind.into(),
        file: file.into(),
        line,
        function: function.into(),
        sites: vec![PathSite::default()],
        ..PathPlace::default()
    }
}

fn plan() -> Plan {
    Plan {
        v: PLAN_V,
        kind: PLAN_TYPE.into(),
        programs: vec![Program {
            program: "/bin/ledger".into(),
            arch: "arm64".into(),
            paths: vec![
                place(
                    0,
                    "err-check",
                    "queue.go",
                    10,
                    "github.com/x/ledger.(*Q).Push",
                ),
                place(
                    1,
                    "err-check",
                    "queue.go",
                    20,
                    "github.com/x/ledger.(*Q).Push",
                ),
                place(
                    2,
                    "recovery",
                    "queue.go",
                    40,
                    "github.com/x/ledger.retryPush",
                ),
                place(3, "err-check", "db.go", 7, "github.com/x/ledger/db.Open"),
            ],
            ..Program::default()
        }],
    }
}

/// A run's result after its records: the hits line, then an empty contracts' part.
fn run(hits: &[(u32, u64, u32)], tag: &str) -> Map<String, serde_json::Value> {
    let h = Hits {
        hits: hits.to_vec(),
        ..Hits::default()
    };
    let p = std::env::temp_dir().join(format!(
        "hydo-error-paths-{}-{tag}.contracts.jsonl",
        std::process::id()
    ));
    std::fs::write(
        &p,
        format!(
            "{}{}",
            hits_line(&h),
            hydo_contracts::plan::records_text(&[], 0)
        ),
    )
    .unwrap();
    let mut res = Map::new();
    res.insert("verdict".into(), json!("HYDO-ORACLE: OK"));
    let j = hydo_explore::contracts::fold(&plan(), &p, &mut res);
    let _ = std::fs::remove_file(&p);
    j.unwrap();
    res
}

#[test]
fn a_runs_hits_are_its_paths_and_leave_the_verdict() {
    let res = run(&[(1, 900, 0), (2, 950, 1)], "one");
    assert_eq!(res["verdict"], json!("HYDO-ORACLE: OK"));
    let r = hydo_explore::results::run_of("one", "disk=none | run K=db@3+1", &res, &[]);
    let p = r.paths.expect("the run's paths");
    assert!(p.watched);
    assert_eq!(p.places, 4);
    assert_eq!(p.reached.len(), 2);
    assert_eq!(
        (p.reached[1].place, p.reached[1].at, p.reached[1].boot),
        (2, 950, 1)
    );
    assert_eq!(
        hydo_explore::paths::run_line(&p),
        "hydo: error paths: 2 of 4 places ran (`hydo report --paths <run dir>` lists them)"
    );
    // a records file without the line: an engine that does not watch them
    let p = std::env::temp_dir().join(format!("hydo-error-paths-{}-kvm", std::process::id()));
    std::fs::write(&p, hydo_contracts::plan::records_text(&[], 0)).unwrap();
    let rp = hydo_explore::paths::run_paths(&plan(), &p).unwrap();
    let _ = std::fs::remove_file(&p);
    assert!(!rp.watched);
    assert!(hydo_explore::paths::run_line(&rp).contains("does not watch them"));
}

#[test]
fn an_exploration_names_the_places_only_faults_reached() {
    let of =
        |r: Map<String, serde_json::Value>| -> Option<RunPaths> { hydo_explore::paths::of_res(&r) };
    let runs = vec![
        (
            1,
            "seed-1".to_string(),
            "disk=none dseed=1 | run W=a".to_string(),
            of(run(&[(0, 10, 0)], "s1")),
        ),
        (
            2,
            "seed-2".to_string(),
            "disk=none dseed=2 | run K=db@3+1 W=b".to_string(),
            of(run(&[(0, 12, 0), (1, 80, 0)], "s2")),
        ),
        (
            3,
            "seed-3".to_string(),
            "disk=cut dseed=3 | run C=4 W=c".to_string(),
            of(run(&[(1, 70, 0), (2, 5, 1)], "s3")),
        ),
    ];
    let c = hydo_explore::paths::coverage(&plan(), &runs);
    assert_eq!((c.runs, c.runs_without_faults), (3, 1));
    let p = |id: usize| &c.places[id];
    assert_eq!((p(0).runs, p(0).runs_without_faults), (2, 1));
    assert_eq!((p(1).runs, p(1).runs_without_faults), (2, 0));
    assert_eq!(p(1).first.as_ref().unwrap().tag, "seed-2");
    assert_eq!(p(2).first.as_ref().unwrap().at, 5);
    assert_eq!(p(3).runs, 0);
    let t = hydo_explore::paths::text(&c, None);
    assert_eq!(
        t[0],
        "3 of 4 error paths ran in 3 runs (1 of them without a fault)."
    );
    let at = |head: &str| t.iter().position(|l| l.starts_with(head)).unwrap();
    let only = at("Ran only with faults (2)");
    assert!(t[only + 1].starts_with("  queue.go:20 ledger.(*Q).Push (the body of an `if err != nil`): 2 runs; first in seed-2"), "{t:#?}");
    assert!(t[only + 2].starts_with("  queue.go:40 ledger.retryPush (a retry, recovery or rollback function): 1 run; first in seed-3"), "{t:#?}");
    assert!(t[at("Ran also without faults (1)") + 1].starts_with("  queue.go:10"));
    assert_eq!(
        t[at("Never reached (1 in 1 functions)") + 1],
        "  db.Open (1 place): db.go:7"
    );
    // against an exploration without faults that reached place 1: it leaves the first list
    let mut other = hydo_explore::paths::coverage(&plan(), &runs[..1]);
    other.places[1].runs = 1;
    let t = hydo_explore::paths::text(&c, Some(&other));
    assert!(
        t.iter().any(|l| l.starts_with("Ran only with faults (1)")),
        "{t:#?}"
    );
}

#[test]
fn no_faults_keeps_each_seeds_workload_and_drops_its_faults() {
    let mut ex = hydo_explore::gen::Ex::default();
    let mut faults = 0;
    for seed in 1..40u64 {
        let with = ex.render(&ex.compose_seed("random", seed, false));
        faults += hydo_explore::paths::has_faults(&with) as u32;
    }
    assert!(faults > 30, "the random strategy draws faults");
    ex.no_faults = true;
    for seed in 1..40u64 {
        let s = ex.compose_seed("random", seed, false);
        let text = ex.render(&s);
        assert!(!hydo_explore::paths::has_faults(&text), "{text}");
        assert_eq!(s.disk, "none");
    }
}

/// Milestone 394: a plan that watches every function, a run whose records have the functions'
/// line, and its control transcript with two step ranges.
#[test]
fn a_runs_functions_have_their_step_range_and_their_place_before_the_faults() {
    let mut pl = plan();
    pl.programs[0].fn_places = (0..3)
        .map(|i| PathPlace {
            id: i,
            kind: "function".into(),
            function: format!("github.com/x/ledger.f{i}"),
            size: 64,
            module: "github.com/x/ledger".into(),
            own: true,
            sites: vec![PathSite::default()],
            ..PathPlace::default()
        })
        .collect();
    let d = std::env::temp_dir().join(format!("hydo-functions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let h = Hits {
        hits: vec![(0, 50, 0), (1, 250, 0), (2, 20, 1)],
        ..Hits::default()
    };
    let rec = d.join("seed-1.contracts.jsonl");
    std::fs::write(
        &rec,
        format!(
            "{}{}",
            functions_line(&h),
            hydo_contracts::plan::records_text(&[], 0)
        ),
    )
    .unwrap();
    // stops at the clocks 100 and 300, each followed by the note of its step range
    std::fs::write(
        d.join("seed-1.control.jsonl"),
        concat!(
            r#"{"cadence":"rb=64","control":1,"controller":"schedule","conversation":"run","schedule":"disk=none | run K=db@3+1","start":"resume S1"}"#,
            "\n",
            r#"{"at":100,"n":1,"seen":0,"t":"stop","why":"resume"}"#,
            "\n",
            r#"{"by":"main","line":"hydo-ob: workload steps 0..2 rc=0","n":1,"t":"note"}"#,
            "\n",
            r#"{"at":300,"n":2,"seen":0,"t":"stop","why":"cadence"}"#,
            "\n",
            r#"{"by":"main","line":"hydo-ob: workload steps 3..5 rc=0","n":2,"t":"note"}"#,
            "\n",
        ),
    )
    .unwrap();
    let mut res = Map::new();
    res.insert("verdict".into(), json!("HYDO-ORACLE: OK"));
    hydo_explore::contracts::fold(&pl, &rec, &mut res).unwrap();
    let r = hydo_explore::results::run_of("seed-1", "disk=none | run K=db@3+1", &res, &[]);
    let f = r.functions.expect("the run's functions");
    let _ = std::fs::remove_dir_all(&d);
    assert!(f.watched);
    assert_eq!(f.functions, 3);
    assert_eq!(f.first_fault_step, Some(3));
    assert_eq!(f.ranges, vec![(0, 3), (3, 6)]);
    let got: Vec<(u32, Option<u32>, bool)> = f
        .reached
        .iter()
        .map(|x| (x.function, x.range, x.before_faults))
        .collect();
    // f0 ran in steps 0..2, before the kill at step 3; f1 in the steps of the kill; f2 in a
    // recovery boot
    assert_eq!(
        got,
        vec![(0, Some(0), true), (1, Some(1), false), (2, None, false)]
    );
    assert!(hydo_explore::funcs::run_line(&f)
        .starts_with("hydo: functions: 3 of 3 functions ran, 1 of them before the first fault"));
    // the map groups them by module and file
    let m = hydo_explore::funcs::map(&pl);
    assert_eq!(m["programs"][0]["functions"], 3);
    assert_eq!(
        m["programs"][0]["modules"][0]["module"],
        "github.com/x/ledger"
    );
    // a run without a fault: every function ran before the faults
    let r = hydo_explore::results::run_of("seed-1", "disk=none | run", &res, &[]);
    assert!(r.functions.unwrap().reached.iter().all(|x| x.before_faults));
}
