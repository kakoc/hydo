//! The feature controllers of M123 (history, error logs, the composer's rounds) against the
//! shell they replace.
//!
//! 1. `features_do_what_the_shell_extensions_do`: the plans the identity corpus (corpus.rs) cannot
//!    reach yet run on the scripted agent and engine and are checked against the shell model
//!    (`tests/support/model/m123.rs`, transliterated from hydo-init-history.sh,
//!    hydo-init-errlog.sh and hydo-composer.sh): the three composed examples that also have an
//!    `eventually` check, with that check removed until the liveness controller is merged; and
//!    variants of redis-composed-hist for the composer's rare paths (commands that outlive the
//!    drain, a budget, a short workload.timeout for stop_all, a saved composition for
//!    `W=@<key>`). Schedules: the identity corpus's for the example, and `W=` forms written here.
//!    Each run is typed again from its transcript (the replay's transcript must be the run's),
//!    recorded as a golden (`tests/golden/m123-quick.txt`), and counted: every path of the
//!    three extensions must have been taken at least once.
//! 2. `plans_as_hydo_build`: the controller's plans hold what hydo-build writes into the image
//!    (manifest.sh's HYDO_CHECK_* and HYDO_ERR_* lines, compose.list, compose.checks).
//! 3. `deliberate_mistakes_are_caught`: mistakes put into recorded conversations are caught by
//!    the model.
//!
//! Until M136 three more tests ran the shell files themselves (`w_forms_as_compose_sh`: setw and
//! plan of the real hydo-composer.sh; `m98_corpora_as_the_awk`: the M98 corpora against
//! `compose.sh plan`; `errlog_scan_as_the_awk`: EL_SCAN and EL_SUM of hydo-init-errlog.sh against
//! the controller's port). The shell init and the guest composer were deleted at the switch to
//! agent images, so those tests went with them (`git show d9202e7:hydo/crates/hydo-control/tests/m123.rs`);
//! the shell model stays as the frozen description of what the shell images did.

mod support;

use hydo_control::{Ending, Plan, Transcript};
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use support::model::{check, Conv};
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
        let mut ctl = hydo_control::ScheduleController::boot(plan.clone()).unwrap();
        let c = support::converse(
            support::fake::Fake::boot(&salt, 16),
            &mut ctl,
            hydo_control::Start::Boot,
            hydo_control::Header {
                controller: String::new(),
                conversation: "boot".into(),
                schedule: None,
                cadence: "default".into(),
                start: "boot".into(),
            },
        );
        if let Err(e) = check(plan, &c.items, Conv::Boot, "provision", (&[], &[]), None) {
            panic!("{label}: boot {salt}: the controller differs from the shell:\n{e}");
        }
        if let Ok(b) = boot(plan, &salt) {
            return b;
        }
    }
    panic!("{label}: no boot");
}

fn oracle_cmd(acked: &[String], failed: &[String]) -> String {
    let mut cmd = String::from("oracle");
    for i in acked {
        cmd.push(' ');
        cmd.push_str(i);
    }
    if !failed.is_empty() {
        cmd.push_str(" failed");
        for i in failed {
            cmd.push(' ');
            cmd.push_str(i);
        }
    }
    cmd
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
fn features_do_what_the_shell_extensions_do() {
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
            if let Err(e) = check(
                &plan,
                &r.run.items,
                Conv::Run,
                &s.guest_text(),
                (&[], &[]),
                Some(&b.s1.2),
            ) {
                panic!(
                    "{}\n{e}\n--- transcript:\n{}",
                    ctx("the run differs from the shell"),
                    r.run.fin.transcript.to_jsonl()
                );
            }
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
                let Some(Ending::PowerCut { acked, failed, .. }) = r.run.fin.ending.clone() else {
                    unreachable!()
                };
                if let Err(e) = check(
                    &plan,
                    &rc.items,
                    Conv::Recovery,
                    &oracle_cmd(&acked, &failed),
                    (&acked, &failed),
                    Some(&b.s0.2),
                ) {
                    panic!(
                        "{}\n{e}\n--- transcript:\n{}",
                        ctx("the recovery differs from the shell"),
                        rc.fin.transcript.to_jsonl()
                    );
                }
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

// ------------------------------------------------------------------ deliberate mistakes

/// Mistakes put into a recorded conversation (as a controller that made them would have sent or
/// printed them): each must be caught by the model.
#[test]
fn deliberate_mistakes_are_caught() {
    type Mut = fn(&mut Vec<Item>) -> bool;
    fn edit_report(
        items: &mut [Item],
        pick: impl Fn(&str) -> bool,
        f: impl Fn(&str) -> String,
    ) -> bool {
        for it in items.iter_mut() {
            if let Item::Report { line, .. } = it {
                if pick(line) {
                    *line = f(line);
                    return true;
                }
            }
        }
        false
    }
    let mutations: Vec<(&str, Mut)> = vec![
        ("an error record's template changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-ERRLOG {"),
                |l| l.replacen("\"tpl\":\"", "\"tpl\":\"x", 1),
            )
        }),
        ("an error record's count changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-ERRLOG {"),
                |l| l.replacen("\"n\":", "\"n\":9", 1),
            )
        }),
        ("the labels of an error kind changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-ERRLOG {") && l.contains("\"at\":\"s"),
                |l| l.replacen("\"at\":\"s", "\"at\":\"r", 1),
            )
        }),
        ("the summary's boot changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-ERRLOG-SUM"),
                |l| l.replace("first", "recovery"),
            )
        }),
        ("an error record after the held assertion records", |v| {
            let Some(a) = v.iter().position(
                |i| matches!(i, Item::Report { line, .. } if line.starts_with("HYDO-ERRLOG-SUM")),
            ) else {
                return false;
            };
            let Some(b) = v[a..].iter().position(
                |i| matches!(i, Item::Report { line, .. } if line.starts_with("HYDO-ASSERT {")),
            ) else {
                return false;
            };
            let (x, y) = (v[a].clone(), v[a + b].clone());
            if let (Item::Report { line: l1, .. }, Item::Report { line: l2, .. }) = (&x, &y) {
                let (l1, l2) = (l1.clone(), l2.clone());
                if let Item::Report { line, .. } = &mut v[a] {
                    *line = l2;
                }
                if let Item::Report { line, .. } = &mut v[a + b] {
                    *line = l1;
                }
                return true;
            }
            false
        }),
        ("a read-back's exit status changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-CHECK-FINAL"),
                |l| l.replace("\"rc\":", "\"rc\":1"),
            )
        }),
        ("a check declaration changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-CHECK {"),
                |l| l.replacen("\"id\":\"", "\"id\":\"z", 1),
            )
        }),
        ("an entry's round changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-CMD start i=") && l.contains(" r=1 "),
                |l| l.replacen(" r=1 ", " r=2 ", 1),
            )
        }),
        ("a command's duration changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-CMD end i=") && l.contains(" ms="),
                |l| l.replacen(" ms=", " ms=1", 1),
            )
        }),
        ("an end status changed", |v| {
            edit_report(
                v,
                |l| l.contains("status=ok"),
                |l| l.replace("status=ok", "status=failed"),
            )
        }),
        ("the done line's counts changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-COMPOSE done"),
                |l| l.replacen(" ok=", " ok=1", 1),
            )
        }),
        ("the plan line changed", |v| {
            edit_report(
                v,
                |l| l.starts_with("HYDO-COMPOSE plan"),
                |l| l.replacen("entries=", "entries=1", 1),
            )
        }),
        ("HYDO_SEED changed", |v| {
            for it in v.iter_mut() {
                if let Item::Send {
                    cmd: hydo_agent_proto::msg::Command::Spawn(sp),
                    ..
                } = it
                {
                    if let Some(e) = sp.env.as_mut() {
                        if let Some(x) = e.iter_mut().find(|x| x.starts_with("HYDO_SEED=")) {
                            x.push('7');
                            return true;
                        }
                    }
                }
            }
            false
        }),
        ("a composed command's timeout changed", |v| {
            for it in v.iter_mut() {
                if let Item::Send {
                    by,
                    cmd: hydo_agent_proto::msg::Command::Spawn(sp),
                    ..
                } = it
                {
                    if by.starts_with("cmd:") {
                        sp.argv[2] = "61".into();
                        return true;
                    }
                }
            }
            false
        }),
        ("a launch moved before the gate opened", |v| {
            // the first launch of round 1 put at the end of round 0's first launch
            let Some(a) = v.iter().position(|i| matches!(i, Item::Report { by, line, .. } if by == "composer" && line.starts_with("HYDO-CMD start") && line.contains(" r=0 "))) else { return false };
            let Some(b) = v.iter().position(|i| matches!(i, Item::Report { by, line, .. } if by == "composer" && line.starts_with("HYDO-CMD start") && line.contains(" r=1 "))) else { return false };
            let it = v.remove(b);
            v.insert(a + 1, it);
            true
        }),
    ];
    // the conversations to break: a few runs of the error-log stack, a history example and the
    // composed plans
    let mut convs: Vec<(Plan, Vec<Item>, hydo_control::GuestState, String)> = Vec::new();
    let mut add = |plan: Plan, label: &str, ws: &[&str]| {
        let b = boot_ok(&plan, label);
        for w in ws {
            let s = Schedule::parse(w).unwrap();
            if let Ok(r) = run_schedule(&b, &s) {
                convs.push((
                    plan.clone(),
                    r.run.items.clone(),
                    b.s1.2.clone(),
                    s.guest_text(),
                ));
            }
        }
    };
    add(
        support::plan_of("hydo/onboard/examples/stack/hydo-errors.yaml").unwrap(),
        "stack-errors",
        &["disk=none | run", "disk=none | run K=kafka@3+2"],
    );
    add(
        support::plan_of("hydo/onboard/examples/redis/hydo-hist.yaml").unwrap(),
        "redis-hist",
        &["disk=none | run"],
    );
    let ps = plans();
    add(
        ps[2].2.clone(),
        &ps[2].0,
        &[
            "disk=none | run W=0000002a,p4,q2,r100",
            "disk=none | run W=00000005,p1,q2,r50",
        ],
    );
    add(
        support::plan_of(HIST).unwrap(),
        HIST,
        &[
            "disk=none | run W=0000002a,p4,q2,r100",
            "disk=none | run W=00000009,p2,q2,r100",
        ],
    );
    for (plan, items, s1, cmd) in &convs {
        check(plan, items, Conv::Run, cmd, (&[], &[]), Some(s1))
            .unwrap_or_else(|e| panic!("unchanged: {e}"));
    }
    let mut caught = 0;
    for (name, f) in &mutations {
        let mut applied = false;
        for (plan, items, s1, cmd) in &convs {
            let mut v = items.clone();
            if !f(&mut v) {
                continue;
            }
            applied = true;
            assert!(
                check(plan, &v, Conv::Run, cmd, (&[], &[]), Some(s1)).is_err(),
                "not caught: {name}"
            );
            caught += 1;
            break;
        }
        assert!(
            applied,
            "the mistake could not be put into any conversation: {name}"
        );
    }
    eprintln!("mistakes: {caught} of {} caught", mutations.len());
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
