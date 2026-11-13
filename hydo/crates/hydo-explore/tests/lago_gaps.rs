//! The explorer's part of the Lago onboarding's gaps (open items 89 to 92, Milestone 225):
//!
//! - `clock_points:`: clock faults also step the clock to a time (`T=<at>:@<point>`). Without
//!   the key every strategy draws exactly the old schedules; with it only `T=` changes, and
//!   every schedule and shrink candidate is canonical and passes the controller's names check.
//! - `job: true`: a one-shot job is never a fault target.
//! - `--shrink-causes`: the fault kinds that tell the failures of one signature apart.
//! - a failing check's output (`HYDO-CHECK-OUT` notes) in the run's findings.

use hydo_explore::gen::Ex;
use hydo_explore::space::Space;
use hydo_schedule::Schedule;
use serde_json::{json, Value};

fn system(extra: &[(&str, Value)]) -> Value {
    let mut m = json!({
        "name": "s",
        "services": [
            {"name": "db", "cmd": "a", "ready": "true", "port": 1},
            {"name": "migrate", "cmd": "sleep 1000", "ready": "true", "job": true},
            {"name": "web", "cmd": "b", "ready": "true", "port": 2, "depends_on": ["migrate", "db"]},
        ],
        "workload": {"steps": 20, "run": "true"},
        "invariants": [{"id": "x", "cmd": "true"}],
    });
    for (k, v) in extra {
        m[*k] = v.clone();
    }
    m
}

fn ex(m: &Value) -> Ex {
    let sp = Space::from_text(&serde_json::to_string(m).unwrap(), None).unwrap();
    let mut e = Ex::default();
    sp.apply(&mut e);
    e.new_faults = true;
    e
}

fn plan(m: &Value) -> hydo_control::Plan {
    let text = serde_json::to_string(m).unwrap();
    let mm = hydo_manifest::from_embedded(&text, None).unwrap();
    hydo_control::Plan::from_manifest(&mm).unwrap()
}

const STRATS: [&str; 4] = ["random", "swarm", "swarm-pos", "process"];

#[test]
fn clock_points_add_steps_to_a_time() {
    let base = system(&[]);
    let pts = system(&[("clock_points", json!(["@:10-2s", "@month+9m58s"]))]);
    let (off, on) = (ex(&base), ex(&pts));
    assert!(off.clock_points.is_empty());
    let p = plan(&pts);
    let (mut to, mut by) = (0, 0);
    for st in STRATS {
        for seed in 1..=300u64 {
            let a = off.compose_seed(st, seed, false);
            let s = on.compose_seed(st, seed, false);
            // only T= may differ
            let mut rest = s.clone();
            rest.t = a.t;
            rest.tp = a.tp.clone();
            assert_eq!(rest, a, "{st} {seed}");
            // the clock fault's step stays; its amount is drawn over both lists
            assert_eq!(s.t.map(|t| t.0), a.t.map(|t| t.0), "{st} {seed}");
            let text = on.render(&s);
            if let Some(tp) = &s.tp {
                to += 1;
                assert!(["@:10-2s", "@month+9m58s"].contains(&tp.as_str()), "{text}");
                assert!(text.contains(&format!(":{tp}")), "{text}");
            } else if s.t.is_some() {
                by += 1;
            }
            let parsed = Schedule::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(on.render(&on.parse(&text).unwrap()), text);
            hydo_control::names::check(&p, &parsed).unwrap_or_else(|e| panic!("{text}: {e}"));
            for c in on.candidates(&s, None) {
                let t = on.render(&c);
                let pc = Schedule::parse(&t).unwrap_or_else(|e| panic!("candidate {t}: {e}"));
                hydo_control::names::check(&p, &pc).unwrap_or_else(|e| panic!("{t}: {e}"));
                // a step to a time is removed as a whole, never halved
                if c.tp.is_some() {
                    assert_eq!(c.tp, s.tp, "{t}");
                }
            }
        }
    }
    assert!(
        to > 20 && by > 20,
        "{to} steps to a time, {by} steps by an amount"
    );
}

#[test]
fn a_job_is_never_a_fault_target() {
    let m = system(&[]);
    let sp = Space::from_text(&serde_json::to_string(&m).unwrap(), None).unwrap();
    assert_eq!(sp.jobs, vec!["migrate".to_string()]);
    assert!(!sp.svcs.contains(&"migrate".to_string()), "{:?}", sp.svcs);
    let e = ex(&m);
    for st in STRATS {
        for seed in 1..=300u64 {
            let text = e.render(&e.compose_seed(st, seed, false));
            assert!(!text.contains("migrate"), "{st} {seed}: {text}");
        }
    }
    // without the key the service is a target as before
    let mut m2 = m.clone();
    m2["services"][1].as_object_mut().unwrap().remove("job");
    let e2 = ex(&m2);
    let hit = STRATS.iter().any(|st| {
        (1..=300u64).any(|seed| {
            e2.render(&e2.compose_seed(st, seed, false))
                .contains("migrate")
        })
    });
    assert!(hit);
}

#[test]
fn fault_kinds_tell_causes_apart() {
    use hydo_explore::run::fault_kinds;
    assert_eq!(fault_kinds("disk=none | run"), "none");
    assert_eq!(
        fault_kinds("disk=none dseed=1 | run W=0000002a,p4,q2,r200"),
        "none"
    );
    assert_eq!(
        fault_kinds("disk=cut dseed=629bfb2777cb6c08 | run C=10 P=api_worker@5+5 T=0:300 W=340bcd65,p1,q1,r3000"),
        "C P T+ disk=cut"
    );
    assert_eq!(
        fault_kinds("disk=none dseed=1717ad91294d20d2 | run T=4:-2700 W=:p2,r3000"),
        "T-"
    );
    assert_eq!(
        fault_kinds("disk=none | run T=0:@:10-2s K=redis@1+4"),
        "K T@"
    );
    assert_eq!(
        fault_kinds("disk=ioerr:3 dseed=2 tj=5 tjpm=100 | run K=a@1+1"),
        "K disk=ioerr tj tjpm"
    );
}

#[test]
fn a_failing_checks_output_reaches_the_findings() {
    use hydo_explore::results::{attach_check_outputs, check_outputs};
    let note = hydo_control::schedule::check_output_note(
        "final_business_result",
        "units:sub4:api_calls: reported 0, acknowledged 12\n1 totals lack acknowledged units\n",
    )
    .unwrap();
    let console = format!("hydo-ob: something\n{note}\nHYDO-ORACLE: VIOLATION [final_business_result] 1 totals lack acknowledged units\n");
    let o = check_outputs(&console);
    assert_eq!(o.len(), 1);
    let mut fs = vec![
        hydo_explore::results::invariant_finding(
            "HYDO-ORACLE: VIOLATION [final_business_result] 1 totals lack acknowledged units",
        )
        .unwrap(),
        hydo_results::Finding {
            kind: "invariant".into(),
            id: "other".into(),
            ..Default::default()
        },
    ];
    attach_check_outputs(&mut fs, &console);
    assert_eq!(
        fs[0].details["output"],
        json!([
            "units:sub4:api_calls: reported 0, acknowledged 12",
            "1 totals lack acknowledged units"
        ])
    );
    assert!(fs[0].details.get("output_omitted").is_none());
    assert!(fs[1].details.get("output").is_none());
    // and hydo report prints them under the finding
    let r = hydo_results::Run {
        schedule: "disk=none | run".into(),
        findings: fs,
        ..Default::default()
    };
    let text = hydo_explore::md::run_report(&r);
    assert!(
        text.contains("    | units:sub4:api_calls: reported 0, acknowledged 12\n"),
        "{text}"
    );
    // a console without notes changes nothing
    let mut fs2 =
        vec![hydo_explore::results::invariant_finding("HYDO-ORACLE: VIOLATION [x] y").unwrap()];
    let before = fs2.clone();
    attach_check_outputs(&mut fs2, "HYDO-ORACLE: VIOLATION [x] y\n");
    assert_eq!(fs2, before);
}
