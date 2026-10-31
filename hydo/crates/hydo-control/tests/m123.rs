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
//! 2. `w_forms_as_compose_sh`: `setw` and `plan` of the real hydo-composer.sh (run by /bin/sh with
//!    its dry-run variables and this machine's awk) against the controller's parse_w/wplan, on W
//!    values the schedule parser would refuse too (unknown fields, a seed that is not hex, keys).
//! 3. `m98_corpora_as_the_awk`: the M98 corpora (3,000 seed-form W values over 80 command sets,
//!    the resolved compositions replayed on a changed and shuffled list, a removed command
//!    refused): the controller's compositions are those `compose.sh plan` prints.
//! 4. `errlog_scan_as_the_awk`: EL_SCAN and EL_SUM (the awk text of hydo-init-errlog.sh) against
//!    the controller's port on generated logs.
//! 5. `plans_as_hydo_build`: the controller's plans hold what hydo-build writes into the image
//!    (manifest.sh's HYDO_CHECK_* and HYDO_ERR_* lines, compose.list, compose.checks).
//! 6. `deliberate_mistakes_are_caught`: mistakes put into recorded conversations are caught by
//!    the model.

mod support;

use hydo_control::schedule::composer::{self, ComposePlan};
use hydo_control::schedule::errlog;
use hydo_control::{Ending, Plan, Transcript};
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command as Proc;
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
    let inputs: Vec<String> = [
        "hydo/onboard/hydo-init-history.sh",
        "hydo/onboard/hydo-init-errlog.sh",
        "hydo/onboard/hydo-composer.sh",
        HIST,
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
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

// ------------------------------------------------------------------ compose.sh itself

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-control-m123-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A guest root for compose.sh's dry run: compose.sh, compose.list, compose.saved/, manifest.sh,
/// and `$BB` as a wrapper that runs its arguments (this machine's cat and awk).
struct Dry {
    dir: PathBuf,
}

impl Dry {
    fn new(tag: &str, cp: &ComposePlan, steps: i64) -> Dry {
        let d = scratch(tag);
        let etc = d.join("etc");
        std::fs::create_dir_all(etc.join("compose.saved")).unwrap();
        std::fs::create_dir_all(d.join("tmp")).unwrap();
        let onb = top().join("hydo/onboard");
        std::fs::copy(onb.join("hydo-composer.sh"), etc.join("compose.sh")).unwrap();
        let mut list = String::new();
        for (i, c) in cp.commands.iter().enumerate() {
            list.push_str(&format!(
                "{i} {} {} {} {}\n",
                c.kind, c.timeout, c.id, c.cmd
            ));
        }
        std::fs::write(etc.join("compose.list"), list).unwrap();
        for (k, c) in &cp.saved {
            std::fs::write(etc.join("compose.saved").join(k), format!("{c}\n")).unwrap();
        }
        let ms = format!(
            "HYDO_STEPS={steps}\nHYDO_COMPOSE=1\nHYDO_COMPOSE_PARALLEL={}\nHYDO_COMPOSE_PER_ROUND={}\nHYDO_COMPOSE_ROUND_MS={}\nHYDO_COMPOSE_COMMAND_TIMEOUT={}\nHYDO_COMPOSE_DRAIN_TIMEOUT={}\nHYDO_COMPOSE_MAX_COMMANDS={}\nHYDO_COMPOSE_BUDGET_MS={}\n",
            cp.parallel, cp.per_round, cp.round_ms, cp.command_timeout, cp.drain_timeout, cp.max_commands, cp.budget_ms
        );
        std::fs::write(etc.join("manifest.sh"), ms).unwrap();
        let bb = d.join("bb");
        std::fs::write(&bb, "#!/bin/sh\nexec \"$@\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bb, std::fs::Permissions::from_mode(0o755)).unwrap();
        Dry { dir: d }
    }

    /// `compose.sh <args>`: (exit status, stdout, the console lines).
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let con = self.dir.join("console");
        let _ = std::fs::write(&con, "");
        let out = Proc::new("/bin/sh")
            .arg(self.dir.join("etc/compose.sh"))
            .args(args)
            .env("BB", self.dir.join("bb"))
            .env("HYDO_ETC", self.dir.join("etc"))
            .env("HYDO_TMP", self.dir.join("tmp"))
            .env("HYDO_CONSOLE", &con)
            .output()
            .expect("/bin/sh");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            std::fs::read_to_string(&con).unwrap_or_default(),
        )
    }
}

impl Drop for Dry {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The controller's setw against compose.sh's.
fn same_setw(dry: &Dry, cp: &ComposePlan, steps: i64, w: &str) -> Result<(), String> {
    let (st, out, con) = dry.run(&["setw", w]);
    let (lines, r) = composer::wplan(cp, steps, w);
    let mut want_con = lines.clone();
    match &r {
        Err(composer::WFail::Werr(e)) => want_con.push(format!("HYDO-COMPOSE error: {e}")),
        Err(composer::WFail::Crash) | Ok(_) => {}
    }
    let con_lines: Vec<String> = con.lines().map(str::to_string).collect();
    if con_lines != want_con {
        return Err(format!(
            "W={w}: console\n  compose.sh: {con_lines:?}\n  controller: {want_con:?}"
        ));
    }
    match (&r, st) {
        (Ok(_), 0) => {}
        (Err(composer::WFail::Werr(e)), 1) if out.trim_end_matches('\n') == e => {}
        (Err(composer::WFail::Crash), s) if s != 0 => {}
        _ => {
            return Err(format!(
                "W={w}: setw status {st} stdout {out:?}, the controller {r:?}"
            ))
        }
    }
    if let Ok((_, es)) = &r {
        let (st, out, _) = dry.run(&["plan"]);
        let want: Vec<String> = es.iter().map(|e| e.line()).collect();
        let got: Vec<String> = out.lines().map(str::to_string).collect();
        if st != 0 || got != want {
            return Err(format!(
                "W={w}: plan\n  compose.sh: {got:?}\n  controller: {want:?}"
            ));
        }
    }
    Ok(())
}

fn cmd(kind: &str, id: &str) -> composer::Cmd {
    composer::Cmd {
        kind: kind.into(),
        timeout: 60,
        id: id.into(),
        cmd: "exec /bin/true".into(),
    }
}

fn cplan(cmds: Vec<composer::Cmd>) -> ComposePlan {
    ComposePlan {
        parallel: 4,
        per_round: 2,
        round_ms: 200,
        command_timeout: 60,
        drain_timeout: 60,
        max_commands: 0,
        budget_ms: 0,
        commands: cmds,
        checks: Vec::new(),
        saved: BTreeMap::new(),
        has_first: false,
        own_provision: None,
    }
}

#[test]
fn w_forms_as_compose_sh() {
    let mut cp = cplan(vec![
        cmd("anytime", "anytime_read"),
        cmd("eventually", "eventually_serves"),
        cmd("first", "first_accounts"),
        cmd("parallel_driver", "parallel_driver_set"),
        cmd("parallel_driver", "parallel_driver_transfer"),
        cmd("serial_driver", "serial_driver_rewrite"),
    ]);
    let key = hydo_composer::key_of(SAVED);
    cp.saved
        .insert(key.clone(), ":p2,r100,canytime_read,0.0.0.5,1.3.0.6".into());
    let dry = Dry::new("w", &cp, 20);
    let ws = [
        "".to_string(),
        "-".into(),
        "0000002a".into(),
        "0000002a,p4,q2,r100".into(),
        "0000002a,p1,q1,r50,n3".into(),
        "0000002a,p4,q2,r100,n0".into(),
        "0000002a,p4,q3,r100,t500".into(),
        "0000002a,p4,q2,r100,x0.2-5.9,o3".into(),
        "0000002a,p4,q2,r100,oparallel_driver_set+anytime_read".into(),
        "0000002a,p4,q2,r100,zz".into(),
        "0000002a,,p2".into(),
        "0000002a,p4,q2,r100,".into(),
        "ffffffffff,p2".into(),
        "zz,p2".into(),
        "0000002a,q0".into(),
        format!("@{key}"),
        "@0123456789ab".into(),
        "@XYZ".into(),
        "@".into(),
        ":p2,r100,canytime_read+parallel_driver_set,0.0.0.1,1.0.1.2,2.3.1.3".into(),
        ":p2,r100,t300,cparallel_driver_set,0.19.0.1".into(),
        ":p2,r100,cparallel_driver_set,0.20.0.1".into(),
        ":p2,r100,cparallel_driver_set,0.0.1.1".into(),
        ":p2,r100,cparallel_driver_nosuch,0.0.0.1".into(),
        ":p2,r100,cfirst_accounts,0.0.0.1".into(),
        ":p2,r100,cparallel_driver_set".into(),
        ":p2,r100,c,0.0.0.1".into(),
        ":p2,r100,cparallel_driver_set,bogus".into(),
        ":p2,r100,cparallel_driver_set,canytime_read,0.0.0.1".into(),
        ":pX,r100,cparallel_driver_set,0.0.0.1".into(),
        ":p2,r100,cparallel_driver_set,0.0.0.1,".into(),
    ];
    let mut n = 0;
    for w in &ws {
        same_setw(&dry, &cp, 20, w).unwrap_or_else(|e| panic!("{e}"));
        n += 1;
    }
    eprintln!("w forms: {n} W values, compose.sh setw/plan == the controller");
}

// ------------------------------------------------------------------ the M98 corpora

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn choice<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len())]
    }
}

/// hydo-composer's tests/awk.rs command sets (80 of them).
fn cmd_sets() -> Vec<Vec<(String, String)>> {
    let k = |kind: &str, n: usize| -> Vec<(String, String)> {
        (0..n)
            .map(|i| (kind.to_string(), format!("{kind}_{i}")))
            .collect()
    };
    let mut sets = Vec::new();
    for npar in [0, 1, 3] {
        for nser in [0, 1, 2] {
            for nsg in [0, 1, 2] {
                for nany in [0, 1, 2] {
                    if npar + nser + nsg + nany == 0 {
                        continue;
                    }
                    let mut cs = [
                        k("parallel_driver", npar),
                        k("serial_driver", nser),
                        k("singleton_driver", nsg),
                        k("anytime", nany),
                        k("first", 1),
                        k("eventually", 1),
                    ]
                    .concat();
                    cs.sort_by(|a, b| a.1.cmp(&b.1));
                    sets.push(cs);
                }
            }
        }
    }
    sets
}

fn meta_of(cmds: &[(String, String)], steps: i64) -> Value {
    json!({"params": {"parallel": 4, "per_round": 2, "round_ms": 200, "command_timeout": 60,
                      "drain_timeout": 60, "check_every": 2, "check_within": 30,
                      "max_commands": 0, "budget_ms": 0},
           "commands": cmds.iter().map(|(k, id)| json!({"kind": k, "id": id})).collect::<Vec<_>>(),
           "steps": steps})
}

fn cplan_of(cmds: &[(String, String)]) -> ComposePlan {
    cplan(cmds.iter().map(|(k, id)| cmd(k, id)).collect())
}

/// The corpora of hydo-composer's awk.rs (the same generator and seed, so the same W values):
/// the controller's composition against `compose.sh plan` after `setw`.
#[test]
fn m98_corpora_as_the_awk() {
    let sets = cmd_sets();
    assert_eq!(sets.len(), 80);
    let mut rnd = Rng(7);
    let n_values = std::env::var("HYDO_AWK_N")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(3000);
    let mut checked = 0;
    let mut drys: BTreeMap<(usize, i64), Dry> = BTreeMap::new();
    for n in 0..n_values {
        let si = n % sets.len();
        let cmds = &sets[si];
        let steps = rnd.choice(&[1i64, 4, 20, 57]);
        let m = meta_of(cmds, steps);
        let (ids, kinds) = hydo_composer::ids_kinds(&m);
        let seed = if n % 5 != 0 {
            (rnd.next() & 0xffff_ffff) as i128
        } else {
            n as i128
        };
        let mut d = hydo_composer::WSeed {
            seed,
            p: 1 + rnd.below(4) as i128,
            q: 1 + rnd.below(3) as i128,
            r: 100,
            n: rnd.choice(&[0i128, 0, 3, 10]),
            t: 0,
            x: Vec::new(),
            o: Vec::new(),
        };
        let full = hydo_composer::plan(d.seed, d.q, steps as i128, &kinds).unwrap();
        if !full.is_empty() && n % 3 == 0 {
            for _ in 0..(1 + rnd.below(5)).min(full.len()) {
                d.x.push(rnd.below(full.len()) as i128);
            }
        }
        if n % 4 == 0 {
            d.o = vec![hydo_composer::OItem::I(rnd.below(cmds.len()) as i128)];
        } else if n % 4 == 1 {
            for _ in 0..(1 + rnd.below(2)).min(cmds.len()) {
                d.o.push(hydo_composer::OItem::S(ids[rnd.below(ids.len())].clone()));
            }
        }
        let w = hydo_composer::render_w(&d);
        let cp = cplan_of(cmds);
        let dry = drys
            .entry((si, steps))
            .or_insert_with(|| Dry::new(&format!("c{si}-{steps}"), &cp, steps));
        same_setw(dry, &cp, steps, &w).unwrap_or_else(|e| panic!("{e}"));
        checked += 1;
    }
    drop(drys);
    // the resolved form: replayed by id on a changed and shuffled list; a removed command fails
    let (mut nres, mut nerr) = (0, 0);
    for n in 0..(n_values / 5).max(200) {
        let cmds = &sets[n % sets.len()];
        let steps = rnd.choice(&[4i64, 20, 57]);
        let w = hydo_composer::render_w(&hydo_composer::WSeed {
            seed: (rnd.next() & 0xffff_ffff) as i128,
            p: 1 + rnd.below(4) as i128,
            q: 1 + rnd.below(3) as i128,
            r: 100,
            n: rnd.choice(&[0i128, 3]),
            t: rnd.choice(&[0i128, 0, 500]),
            x: vec![],
            o: vec![],
        });
        let m = meta_of(cmds, steps);
        let old = hydo_composer::entries_of(&w, &m, None, None).unwrap();
        if old.is_empty() {
            continue;
        }
        let res = hydo_composer::stable_w(&w, &m, None, None).unwrap();
        let mut now = cmds.clone();
        now.push(("parallel_driver".into(), format!("parallel_driver_new{n}")));
        now.push(("anytime".into(), "anytime_0new".into()));
        let mut dropped = None;
        if n % 7 == 3 {
            let used: Vec<String> = hydo_composer::parse_resolved(&res)
                .unwrap()
                .entries
                .iter()
                .map(|e| e.2.clone())
                .collect();
            let d = used[rnd.below(used.len())].clone();
            now.retain(|c| c.1 != d);
            dropped = Some(d);
        }
        now.sort_by(|a, b| a.1.cmp(&b.1));
        if n % 2 == 1 {
            for i in (1..now.len()).rev() {
                now.swap(i, rnd.below(i + 1));
            }
        }
        let cp = cplan_of(&now);
        let dry = Dry::new(&format!("r{n}"), &cp, steps);
        same_setw(&dry, &cp, steps, &res).unwrap_or_else(|e| panic!("{e}"));
        let (_, r) = composer::wplan(&cp, steps, &res);
        match (dropped, r) {
            (Some(d), Err(composer::WFail::Werr(e))) => {
                assert!(e.contains(&d), "{e}");
                nerr += 1;
            }
            (None, Ok((_, es))) => {
                // the same commands, rounds, entries and seeds as the seed form on the old list
                let got: Vec<(i64, i64, String, i64)> = es
                    .iter()
                    .map(|e| (e.entry, e.round, now[e.idx].1.clone(), e.seed))
                    .collect();
                let want: Vec<(i64, i64, String, i64)> = old
                    .iter()
                    .map(|(e, r, c, s)| (*e as i64, *r as i64, cmds[*c].1.clone(), *s as i64))
                    .collect();
                assert_eq!(got, want, "{res}");
                nres += 1;
            }
            (d, r) => panic!("{res}: dropped {d:?}, the controller {r:?}"),
        }
    }
    eprintln!("m98: {checked} seed-form W values; {nres} resolved compositions replayed by id; {nerr} with a removed command refused; compose.sh (awk = this machine's) == the controller");
    assert!(checked >= 3000.min(n_values) && nres > 400 && nerr > 50);
}

// ------------------------------------------------------------------ EL_SCAN and EL_SUM

fn awk_prog(name: &str) -> String {
    let t = std::fs::read_to_string(top().join("hydo/onboard/hydo-init-errlog.sh")).unwrap();
    let start = t.find(&format!("\n{name}='")).unwrap() + name.len() + 3;
    let len = t[start..].find("'\n").unwrap();
    t[start..start + len].to_string()
}

const LOG: &[&str] = &[
    "57:C 01 Jan 2026 00:00:00.001 # oO0OoO0OoO0Oo Redis is starting oO0OoO0OoO0Oo",
    "57:M 01 Jan 2026 00:00:00.002 * Ready to accept connections tcp",
    "91:M 01 Jan 2026 00:03:10.100 # !!! Warning: short read while loading the AOF file appendonly.aof.1.incr.aof!",
    "2026-01-01T00:00:12.345678Z 0 [ERROR] [MY-012592] [InnoDB] Operating system error number 2 in a file operation.",
    "2026-01-01T00:07:44.000999Z 3 [ERROR] [MY-012592] [InnoDB] Operating system error number 28 in a file operation.",
    "ERROR [Controller id=1, targetBrokerId=1] Connection to node 1 (localhost/127.0.0.1:9093) could not be established. (org.apache.kafka.clients.NetworkClient)",
    "java.net.ConnectException: Connection refused",
    "\tat java.base/sun.nio.ch.Net.pollConnect(Native Method)",
    "Caused by: java.io.IOException: boom",
    "\t... 12 more",
    "INFO kafka.log.LogManager Loading logs",
    "java.lang.IllegalStateException: request 3f2a9c1e-0b7d-4c1e-9a2b-1234567890ab failed at 0x7ffd1234 id deadbeef42",
    "time=2026-01-01T00:00:05Z level=error msg=\"dial tcp 127.0.0.1:54321->127.0.0.1:3306: connect: connection refused\"",
    "panic: runtime error: index out of range [3] with length 3",
    "HYDO-ASSERT {\"t\":\"always\",\"id\":\"x\",\"ok\":false}",
    "level=error msg=\"contains \\\"quotes\\\" and back slash and hydo-explore-workload-done\"",
    "WARN org.apache.kafka.clients.NetworkClient [Producer clientId=producer-1] Connection to node 1 could not be established.",
    "ctrl\u{1}char\u{7f} line ERROR: here 7",
    "Fatal: something broke in thread12 on localhost:6379 and front:8080",
    "[error] bracketed at Mon, Jan 5 2026 12:00:01",
    "Error: errno 5 code=7 status: 9 HY000 E1234 x1y2z3 ABCDEFGHIJKLMNOP1",
    "  continuation that is not a trace",
    "fatal error: concurrent map writes",
    "ERROR caf\u{e9} \u{2192} 2026/01/02 and 10:11:12,5 and 1 Feb 2026",
];

#[test]
fn errlog_scan_as_the_awk() {
    let awk = std::env::var("HYDO_AWK").unwrap_or_else(|_| "awk".into());
    let awk: Vec<&str> = awk.split_whitespace().collect();
    let d = scratch("el");
    let (scan, sum) = (awk_prog("EL_SCAN"), awk_prog("EL_SUM"));
    let hosts = vec![s("redis"), s("front"), s("kafka")];
    let presets: [(&str, String, String); 4] = [
        (
            "auto",
            format!("({})", errlog::LEVEL_ERROR),
            errlog::EXCEPTION_LINE.into(),
        ),
        (
            "warning",
            format!("({})|({})", errlog::LEVEL_ERROR, errlog::LEVEL_WARNING),
            errlog::EXCEPTION_LINE.into(),
        ),
        ("redis", format!("({})", errlog::REDIS_LEVEL), String::new()),
        (
            "match",
            format!("({})|(boom|Loading)", errlog::LEVEL_ERROR),
            errlog::EXCEPTION_LINE.into(),
        ),
    ];
    let mut rnd = Rng(11);
    let mut n = 0;
    for round in 0..120 {
        let (name, lev, exc) = &presets[round % presets.len()];
        let ign = if round % 5 == 0 {
            s("Connection refused")
        } else {
            String::new()
        };
        let lines: Vec<String> = (0..1 + rnd.below(30))
            .map(|_| LOG[rnd.below(LOG.len())].to_string())
            .collect();
        let text = lines.join("\n") + if round % 7 == 0 { "" } else { "\n" };
        let nl = lines.len();
        let marks: String = (0..rnd.below(4))
            .map(|k| {
                format!(
                    "{} {}\n",
                    if k == 3 {
                        s("end")
                    } else {
                        (k * 5).to_string()
                    },
                    rnd.below(nl + 1)
                )
            })
            .collect::<Vec<_>>()
            .concat();
        let mut ms: Vec<String> = marks.lines().map(str::to_string).collect();
        ms.sort_by_key(|l| l.split(' ').nth(1).unwrap().parse::<usize>().unwrap());
        let marks = ms.iter().map(|l| format!("{l}\n")).collect::<String>();
        let base = if round % 6 == 0 {
            (rnd.below(nl + 1)).to_string()
        } else {
            s("0")
        };
        let first = if round % 9 == 0 { "r" } else { "s" };
        let (stream, mf) = (d.join("stream"), d.join("marks"));
        std::fs::write(&stream, &text).unwrap();
        std::fs::write(&mf, &marks).unwrap();
        let out = Proc::new(awk[0])
            .args(&awk[1..])
            .arg(&scan)
            .arg(&stream)
            .env("EL_SVC", "svc")
            .env("EL_SRC", "log")
            .env("EL_MARKS", &mf)
            .env("EL_FIRST", first)
            .env("EL_BASE", &base)
            .env("EL_LEVEL", lev)
            .env("EL_EXC", exc)
            .env("EL_IGNORE", &ign)
            .env("EL_NORM", "")
            .env("EL_HOSTS", hosts.join(" "))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let rec = d.join("rec");
        std::fs::write(&rec, &out.stdout).unwrap();
        let out2 = Proc::new(awk[0])
            .args(&awk[1..])
            .arg(&sum)
            .arg(&rec)
            .env("EL_MAX", "5")
            .env("EL_BOOT", "first")
            .env("EL_STEPS", "20")
            .output()
            .unwrap();
        let want = String::from_utf8_lossy(&out2.stdout).into_owned();
        let sv = errlog::ErrSvc {
            name: s("svc"),
            level: lev.clone(),
            exc: exc.clone(),
            ignore: ign.clone(),
            files: vec![],
            norm: vec![],
        };
        let sc = errlog::scan(
            &sv,
            0,
            text.as_bytes(),
            marks.as_bytes(),
            base.as_bytes(),
            first,
            &hosts,
        );
        assert!(sc.err.is_none());
        let got: String = errlog::summarize(&sc.recs, 5, "first", 20)
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            got, want,
            "preset {name}, round {round}\n--- log:\n{text}\n--- marks:\n{marks}"
        );
        n += 1;
    }
    let _ = std::fs::remove_dir_all(&d);
    eprintln!(
        "errlog: {n} logs, EL_SCAN+EL_SUM ({}) == the controller",
        awk.join(" ")
    );
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
        let Ok((ms, _)) =
            hydo_build::shell::compile_shell(&hydo_build::Ctx::standalone(), &m, &order, &tools)
        else {
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
            hydo_composer::stage(&meta2, &root, &top().join("hydo/onboard"), None).unwrap();
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
        hydo_composer::stage(&m2, &root, &top().join("hydo/onboard"), None).unwrap();
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
