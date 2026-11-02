//! The feature controllers of M123 (history, error logs, the composer's rounds).
//!
//! 1. `the_m123_features_recorded_transcripts`: the plans the identity corpus (corpus.rs) cannot
//!    reach yet run on the scripted agent and engine: the three composed examples that also have an
//!    `eventually` check, with that check removed until the liveness controller is merged; and
//!    variants of redis-composed-hist for the composer's rare paths (commands that outlive the
//!    drain, a budget, a short workload.timeout for stop_all, a saved composition for
//!    `W=@<key>`). Schedules: the identity corpus's for the example, and `W=` forms written here.
//!    Each run is typed again from its transcript (the replay's transcript must be the run's),
//!    recorded as a golden (`tests/golden/m123-quick.txt`), and counted: every path of the
//!    three extensions must have been taken at least once.
//! 2. `plans_as_hydo_build`: the controller's plans hold what hydo-build writes into the image
//!    (manifest.sh's HYDO_CHECK_* and HYDO_ERR_* lines, compose.list, compose.checks).
//!
//! Until M136 three more tests ran the shell files themselves (`w_forms_as_compose_sh`: setw and
//! plan of the real hydo-composer.sh; `m98_corpora_as_the_awk`: the M98 corpora against
//! `compose.sh plan`; `errlog_scan_as_the_awk`: EL_SCAN and EL_SUM of hydo-init-errlog.sh against
//! the controller's port). The shell init and the guest composer were deleted at the switch to
//! agent images, so those tests went with them (`git show d9202e7:hydo/crates/hydo-control/tests/m123.rs`).
//! Until Milestone 145 every run was also checked against a shell model of hydo-init-history.sh,
//! hydo-init-errlog.sh and hydo-composer.sh, and a third test showed that the model caught
//! deliberate mistakes; both went with the shell init's images (`tests/support/mod.rs`); the
//! golden is the transcripts of those checked runs.

mod support;

use hydo_control::{Ending, Plan, Transcript};
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use support::{boot, data_of, run_schedule, top, Booted, Item};

// ------------------------------------------------------------------ the plans

fn plan_of_data(d: Value) -> Plan {
    Plan::from_manifest(&hydo_manifest::Manifest::from_validated(d)).unwrap()
}

fn without_eventually(rel: &str) -> Value {
    let mut d = data_of(rel).unwrap();
    let inv: Vec<Value> = d["invariants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v.get("type").and_then(Value::as_str) != Some("eventually"))
        .cloned()
        .collect();
    d["invariants"] = Value::Array(inv);
    d
}

const HIST: &str = "hydo/onboard/examples/redis-composed-hist/hydo/hydo.yaml";

/// (label, the example whose explorer space gives the schedules, the plan)
fn plans() -> Vec<(String, String, Plan)> {
    let mut v = Vec::new();
    for ex in ["redis-composed", "redis-composed-read", "redis-errors"] {
        let rel = format!("hydo/onboard/examples/{ex}/hydo/hydo.yaml");
        v.push((
            format!("{rel}#no-eventually"),
            rel.clone(),
            plan_of_data(without_eventually(&rel)),
        ));
    }
    // error logs with a retry command (lines after `el_mark end`)
    let st = "hydo/onboard/examples/stack/hydo-errors.yaml";
    v.push((s(st), s(st), plan_of_data(data_of(st).unwrap())));
    let base = data_of(HIST).unwrap();
    // commands that outlive the drain
    let mut d = base.clone();
    for c in d["_compose"]["commands"].as_array_mut().unwrap() {
        c["timeout"] = json!(60);
    }
    d["_compose"]["params"]["drain_timeout"] = json!(3);
    v.push((format!("{HIST}#drain"), s(HIST), plan_of_data(d)));
    // a budget: late entries are skipped
    let mut d = base.clone();
    d["_compose"]["params"]["budget_ms"] = json!(400);
    v.push((format!("{HIST}#budget"), s(HIST), plan_of_data(d)));
    // a short workload.timeout: stop_all
    let mut d = base.clone();
    d["workload"]["timeout"] = json!(4);
    v.push((format!("{HIST}#short"), s(HIST), plan_of_data(d)));
    // a saved composition
    let mut d = base;
    d["_compose"]["regressions"] = json!([{
        "name": "saved", "schedule": format!("disk=none | run W=@{}", hydo_composer::key_of(SAVED)),
        "compose": SAVED, "expect": "ok"}]);
    v.push((format!("{HIST}#saved"), s(HIST), plan_of_data(d)));
    v
}

const SAVED: &str =
    ":p2,r100,canytime_read+parallel_driver_set,0.0.1.11,1.0.0.12,2.1.1.13,3.4.1.14";

fn s(x: &str) -> String {
    x.to_string()
}

/// `W=` forms the explorer does not draw, for a plan.
fn written(p: &Plan) -> Vec<String> {
    let cp = p.compose.as_ref().unwrap();
    let id_of = |k: &str| {
        cp.commands
            .iter()
            .find(|c| c.kind == k)
            .map(|c| c.id.clone())
    };
    let par = id_of("parallel_driver").unwrap_or_default();
    let any = id_of("anytime")
        .or_else(|| id_of("serial_driver"))
        .unwrap_or_default();
    let first_cmd = cp
        .commands
        .first()
        .map(|c| c.id.clone())
        .unwrap_or_default();
    let n = p.steps;
    let mut v = vec![
        "disk=none | run W=0000002a,p4,q2,r100".to_string(),
        "disk=none | run W=0000002a,p2,q2,r50,n3".into(),
        "disk=none | run W=0000002a,p4,q2,r100,t300".into(),
        "disk=none | run W=0000002a,p4,q2,r100,x0.2-5,o1".into(),
        format!("disk=none | run W=0000002a,p4,q2,r100,o{par}"),
        "disk=none | run W=-".into(),
        format!("disk=none | run W=:p1,r100,c{par},0.0.0.1,1.0.0.2,2.0.0.3,3.0.0.4,4.0.0.5,5.0.0.6,6.0.0.7"),
        format!("disk=none | run W=:p2,r100,c{par}+{any},0.0.0.1,1.0.1.2,2.3.0.3"),
        format!("disk=none | run W=:p2,r100,c{par}+parallel_driver_nosuch,0.0.0.1"),
        format!("disk=none | run W=:p2,r100,c{first_cmd},0.0.0.1"),
        format!("disk=none | run W=:p2,r100,c{par},0.{},0.1", n + 3),
        "disk=none | run W=@0123456789ab".into(),
        "disk=cut | run W=0000002a,p4,q2,r100 C=5".into(),
        "disk=cut | run W=00000007,p3,q2,r50 C=1".into(),
        "disk=none | run P=redis@2~10:400 W=0000002a,p4,q2,r100".into(),
        "disk=none | run P=redis@3~1:30 W=0000002b,p4,q2,r50".into(),
        "disk=none | run K=redis@3+2 W=0000002a,p4,q2,r100".into(),
        "disk=none | run X=redis@2~200 W=0000002a,p4,q2,r100".into(),
        "rb=1 | run W=00000003,p2,q1,r200".into(),
    ];
    if !cp.saved.is_empty() {
        for k in cp.saved.keys() {
            v.push(format!("disk=none | run W=@{k}"));
            v.push(format!("disk=cut | run W=@{k} C=2"));
        }
    }
    v
}

// the identity corpus's schedules (corpus.rs), fewer seeds
const FLAGS: [&str; 14] = [
    "TIMING",
    "NEW_FAULTS",
    "DURABLE",
    "MORE_FAULTS",
    "PREEMPT",
    "TURNS",
    "BURSTS",
    "BUGGIFY",
    "CONCURRENCY",
    "FREEZE_CALL",
    "PROTOCOL_FAULTS",
    "GRPC",
    "COMPOSE",
    "KNOB_ANY",
];

fn identity_schedules(m: &str, seeds: u64) -> Vec<String> {
    let mut out = Vec::new();
    let mut ex = Ex::default();
    let sp = Space::from_path(&top().join(m)).expect("the explorer reads the manifest");
    ex.grpc_targets = grpc_targets(Some(&sp.m));
    sp.apply(&mut ex);
    ex.backend = Backend::Hvx;
    ex.cf_range = Some((1, 300));
    for strat in ["random", "swarm", "process"] {
        for sd in 1..=seeds {
            let h = hydo_explore::rng::mix64(sd.wrapping_mul(0x1000193) ^ 0x123);
            let on = |i: usize| match sd {
                1 => false,
                2 => true,
                _ => (h >> i) & 1 == 1,
            };
            let b = |k: &str| on(FLAGS.iter().position(|f| *f == k).unwrap());
            ex.timing = b("TIMING");
            ex.new_faults = b("NEW_FAULTS");
            ex.more_faults = b("MORE_FAULTS");
            ex.preempt = b("PREEMPT");
            ex.concurrency = b("CONCURRENCY");
            ex.compose = true;
            ex.knob_rule_any = b("KNOB_ANY");
            out.push(ex.render(&ex.compose_seed(strat, sd, b("DURABLE"))));
        }
    }
    out
}

fn class(t: &Transcript) -> &'static str {
    match t.ending() {
        Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: OK") => "ok",
        Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: VIOLATION") => "violation",
        Some(Ending::Verdict(_)) => "error",
        Some(Ending::PowerCut { .. }) => "cut",
        _ => "other",
    }
}

fn boot_ok(plan: &Plan, label: &str) -> Booted {
    for i in 0..30 {
        let salt = format!("{label}#{i}");
        if let Ok(b) = boot(plan, &salt) {
            return b;
        }
    }
    panic!("{label}: no boot");
}

/// The paths each extension has, and a line (or a sent command) that shows a run took it.
const PATHS: &[(&str, &str)] = &[
    (
        "composer: a round waited for its time",
        "\\\"sleep\\\",\\\"0.",
    ),
    ("composer: a first command", "HYDO-CMD start i=f"),
    ("composer: an end command", "HYDO-CMD start i=e"),
    ("composer: a command ok", "status=ok"),
    ("composer: a command failed", "status=failed"),
    ("composer: a command ran into its timeout", "status=timeout"),
    ("composer: a violation (exit 100)", "status=violation"),
    (
        "composer: killed by the drain",
        "status=killed (still running",
    ),
    (
        "composer: killed by stop_all",
        "status=killed (workload.timeout)",
    ),
    (
        "composer: stop_all's done line",
        "ran into workload.timeout=",
    ),
    ("composer: a skipped entry (budget)", "HYDO-CMD skip i="),
    ("composer: the seed form", " seed="),
    ("composer: the resolved form", " resolved p="),
    ("composer: a saved composition", "plan W=@"),
    ("composer: setw refused W", "HYDO-COMPOSE error: W="),
    (
        "composer: an assertion of a command",
        "HYDO-ASSERT {\\\"t\\\":\\\"always\\\",\\\"id\\\":\\\"kv.",
    ),
    (
        "composer: compose.<id>.succeeded",
        ".succeeded\\\",\\\"ok\\\":true",
    ),
    (
        "composer: compose.<id>.no_violation",
        ".no_violation\\\",\\\"ok\\\":false",
    ),
    (
        "composer: the verdict invariant failed",
        "[compose.commands] command ",
    ),
    (
        "composer: a pause timer while a command ran",
        "mid_request=true",
    ),
    ("history: HYDO-CHECK", "HYDO-CHECK {"),
    ("history: HYDO-CHECK-FINAL", "HYDO-CHECK-FINAL"),
    ("errlog: a record", "HYDO-ERRLOG {"),
    (
        "errlog: a record of a recovery boot",
        "HYDO-ERRLOG-SUM {\\\"boot\\\":\\\"recovery\\\"",
    ),
    (
        "errlog: a record of a stream's setup log",
        "\\\"src\\\":\\\"setup\\\"",
    ),
    ("errlog: a read as hex", "\\\"hex\\\":true"),
];

#[test]
fn the_m123_features_recorded_transcripts() {
    let seeds = if std::env::var_os("HYDO_CONTROL_FULL").is_some() {
        60
    } else {
        10
    };
    let mut golden = Vec::new();
    let mut seen_paths: BTreeMap<&str, usize> = BTreeMap::new();
    let mut runs = 0;
    for (label, space, plan) in plans() {
        let b = boot_ok(&plan, &label);
        let mut texts = identity_schedules(&space, seeds);
        if plan.compose.is_some() {
            texts.extend(written(&plan));
        } else {
            texts.extend(
                [
                    "disk=none | run",
                    "disk=none | run K=kafka+2",
                    "disk=cut | run C=5",
                    "disk=none | run P=mysql+3",
                    "disk=none | run K=redis+1",
                ]
                .map(String::from),
            );
        }
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut stream = Vec::new();
        let mut done = std::collections::BTreeSet::new();
        for text in &texts {
            let Ok(s) = Schedule::parse(text) else {
                *counts.entry("unparsed".into()).or_default() += 1;
                continue;
            };
            if !done.insert(s.guest_text()) {
                continue;
            }
            let r = match run_schedule(&b, &s) {
                Ok(r) => r,
                Err(e) => {
                    *counts
                        .entry(format!(
                            "refused:{}",
                            e.seam.map(|x| x.name()).unwrap_or("other")
                        ))
                        .or_default() += 1;
                    continue;
                }
            };
            let ctx = |w: &str| format!("{label}: {text}: {w}");
            let mut reference = r.run.fin.transcript.clone();
            reference.header.controller = "replay".into();
            assert_eq!(
                r.replay.to_jsonl(),
                reference.to_jsonl(),
                "{}",
                ctx("transcript replay differs from the policy run")
            );
            let mut all = r.run.fin.transcript.to_jsonl();
            if let Some(rc) = &r.recovery {
                assert!(
                    matches!(r.run.fin.ending, Some(Ending::PowerCut { .. })),
                    "{}",
                    ctx("a recovery without a power cut")
                );
                all.push_str(&rc.fin.transcript.to_jsonl());
                *counts
                    .entry(format!("recovery:{}", class(&rc.fin.transcript)))
                    .or_default() += 1;
            }
            all.push_str(&b.boot.fin.transcript.to_jsonl());
            for (name, pat) in PATHS {
                if all.contains(pat) {
                    *seen_paths.entry(name).or_default() += 1;
                }
            }
            for name in item_paths(&r.run.items)
                .into_iter()
                .chain(r.recovery.iter().flat_map(|c| item_paths(&c.items)))
            {
                *seen_paths.entry(name).or_default() += 1;
            }
            *counts
                .entry(format!("run:{}", class(&r.run.fin.transcript)))
                .or_default() += 1;
            runs += 1;
            stream.push(format!("S\t{text}"));
            stream.extend(
                all.lines()
                    .filter(|l| !l.contains("\"t\":\"note\""))
                    .map(str::to_string),
            );
        }
        golden.push(format!(
            "plan\t{label}\t{}",
            counts
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        golden.push(format!(
            "boot-transcript\t{label}\t{}",
            b.boot.fin.transcript.hash()
        ));
        golden.extend(support::chunked(&label, &stream, 500));
    }
    golden.push(format!(
        "paths\t{}",
        seen_paths
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    ));
    eprintln!("features: {runs} runs; paths: {seen_paths:?}");
    let missing: Vec<&str> = PATHS
        .iter()
        .map(|p| p.0)
        .chain(ITEM_PATHS.iter().copied())
        .filter(|n| !seen_paths.contains_key(n))
        .collect();
    assert!(missing.is_empty(), "paths no run took: {missing:?}");
    let inputs: Vec<String> = [HIST].iter().map(|x| x.to_string()).collect();
    support::check_golden(
        if seeds == 10 { "m123-quick.txt" } else { "m123-full.txt" },
        &[
            "The feature controllers of M123 on the plans the identity corpus cannot reach yet",
            "(hydo/crates/hydo-control/tests/m123.rs). Re-record: HYDO_BLESS=1 cargo test -p hydo-control --test m123",
        ],
        &inputs,
        &golden,
    );
}

// ------------------------------------------------------------------ scratch roots

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-control-m123-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
// ------------------------------------------------------------------ the plans and hydo-build

#[test]
fn plans_as_hydo_build() {
    let sq = |x: &str| hydo_build::shell::shq(x);
    let mut n = 0;
    for rel in support::manifests() {
        let Ok(d) = data_of(&rel) else { continue };
        let Ok(plan) = Plan::from_manifest(&hydo_manifest::Manifest::from_validated(d.clone()))
        else {
            continue;
        };
        let m = d.clone();
        let order: Vec<String> = plan.services.iter().map(|x| x.name.clone()).collect();
        let tools = hydo_build::shell::ShellTools {
            tc: "TC".into(),
            fc: None,
            ip: None,
        };
        let Ok((ms, _)) = hydo_build::shell::compile_shell(
            &hydo_build::Ctx::standalone(Default::default()),
            &m,
            &order,
            &tools,
        ) else {
            continue;
        };
        for (i, c) in plan.history.iter().enumerate() {
            assert!(
                ms.contains(&format!("HYDO_CHECK_c{i}_json={}\n", sq(&c.json))),
                "{rel}: {}",
                c.json
            );
            assert!(
                ms.contains(&format!("HYDO_CHECK_c{i}_final={}\n", sq(&c.final_))),
                "{rel}"
            );
        }
        if let Some(ep) = &plan.errors {
            assert!(ms.contains(&format!("HYDO_ERR_MAX={}\n", ep.max)));
            let names: Vec<&str> = ep.services.iter().map(|x| x.name.as_str()).collect();
            assert!(
                ms.contains(&format!("HYDO_ERR_SERVICES={}\n", sq(&names.join(" ")))),
                "{rel}"
            );
            for sv in &ep.services {
                let n = &sv.name;
                for (k, v) in [
                    ("level", sv.level.clone()),
                    ("exc", sv.exc.clone()),
                    ("ignore", sv.ignore.clone()),
                    ("files", sv.files.join(" ")),
                    ("norm", sv.norm.join("\n")),
                ] {
                    assert!(
                        ms.contains(&format!("HYDO_ERR_{n}_{k}={}\n", sq(&v))),
                        "{rel}: HYDO_ERR_{n}_{k}"
                    );
                }
            }
        }
        if let Some(cp) = &plan.compose {
            let meta = &d["_compose"];
            let root = scratch(&format!("stage{n}"));
            let mut meta2 = meta.clone();
            meta2.as_object_mut().unwrap().remove("_folder");
            hydo_composer::stage(&meta2, &root, None).unwrap();
            let list = std::fs::read_to_string(root.join("etc/hydo/compose.list")).unwrap();
            for (i, (l, c)) in list.lines().zip(&cp.commands).enumerate() {
                let w: Vec<&str> = l.splitn(5, ' ').collect();
                assert_eq!(
                    (w[0], w[1], w[2], w[3], w[4].trim()),
                    (
                        i.to_string().as_str(),
                        c.kind.as_str(),
                        c.timeout.to_string().as_str(),
                        c.id.as_str(),
                        c.cmd.as_str()
                    ),
                    "{rel}"
                );
            }
            let checks = std::fs::read_to_string(root.join("etc/hydo/compose.checks")).unwrap();
            for (l, c) in checks.lines().zip(&cp.checks) {
                let w: Vec<&str> = l.splitn(5, ' ').collect();
                assert_eq!(
                    (w[2], w[3], w[4].trim()),
                    (
                        c.timeout.to_string().as_str(),
                        c.id.as_str(),
                        c.cmd.as_str()
                    ),
                    "{rel}"
                );
            }
            for l in hydo_composer::shell_lines(meta) {
                assert!(
                    ms.contains(&format!("{l}\n")) || l.starts_with("HYDO_KNOB_W"),
                    "{rel}: {l}"
                );
            }
            let _ = std::fs::remove_dir_all(&root);
        }
        n += 1;
    }
    assert!(n > 20);
}

/// The paths a transcript's text does not show, read from the conversation's items.
const ITEM_PATHS: &[&str] = &[
    "errlog: a label of a workload range",
    "errlog: the end label",
    "composer: a check timed out",
];

fn item_paths(items: &[Item]) -> Vec<&'static str> {
    use hydo_agent_proto::msg::{Command, Event};
    let mut out = Vec::new();
    let mut checks = std::collections::BTreeSet::new();
    for it in items {
        match it {
            Item::Report { line, .. } if line.starts_with("HYDO-ERRLOG {") => {
                let at = line
                    .split("\"at\":\"")
                    .nth(1)
                    .unwrap_or("")
                    .split('"')
                    .next()
                    .unwrap_or("");
                if at
                    .split(',')
                    .any(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()))
                {
                    out.push(ITEM_PATHS[0]);
                }
                if at.split(',').any(|l| l == "end") {
                    out.push(ITEM_PATHS[1]);
                }
            }
            Item::Send {
                id,
                cmd: Command::Run(r),
                ..
            } if r
                .env
                .as_ref()
                .is_some_and(|e| e.iter().any(|x| x.starts_with("HYDO_CHECK="))) =>
            {
                checks.insert(*id);
            }
            Item::Got {
                msg: support::fake::Msg::Event(Event::Done(d)),
                ..
            } if d.status == 143 && checks.contains(&d.id) => {
                out.push(ITEM_PATHS[2]);
            }
            _ => {}
        }
    }
    out
}

/// An agent image carries its manifest text and the composer's meta (`/etc/hydo/compose.json`, as
/// hydo-build stages it): `agent::plan_of_embedded` gives the plan the explorer's reading gives.
#[test]
fn agent_images_carry_the_composer() {
    let mut n = 0;
    for rel in support::manifests() {
        let Ok(d) = data_of(&rel) else { continue };
        let Some(meta) = d.get("_compose").cloned() else {
            continue;
        };
        let root = scratch(&format!("img{n}"));
        let mut m2 = meta.clone();
        m2.as_object_mut().unwrap().remove("_folder");
        hydo_composer::stage(&m2, &root, None).unwrap();
        let cj = std::fs::read_to_string(root.join("etc/hydo/compose.json")).unwrap();
        let text = std::fs::read_to_string(top().join(&rel)).unwrap();
        let from_image = hydo_control::agent::plan_of_embedded(&text, Some(&cj));
        let from_explorer = support::plan_of(&rel);
        match (&from_image, &from_explorer) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a.compose, b.compose, "{rel}");
                assert_eq!(a.invariants, b.invariants, "{rel}");
                assert_eq!(a.provision, b.provision, "{rel}");
                assert_eq!(a.workload_run, b.workload_run, "{rel}");
                assert!(a.compose.is_some(), "{rel}");
            }
            (Err(a), Err(b)) => assert_eq!(a.seam, b.seam, "{rel}"),
            other => panic!("{rel}: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
        n += 1;
    }
    assert!(n >= 4, "{n} composed examples");
}
