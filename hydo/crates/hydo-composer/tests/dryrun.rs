//! hydo/onboard/tests/compose-dryrun.sh re-expressed: the guest composer (hydo-composer.sh, which
//! stays shell) driven the way hydo-init.sh drives it, with every file it reads written by this
//! crate (`stage`: compose.sh, compose.list, compose.checks, compose.saved/; `shell_lines`: the
//! manifest.sh lines): rounds, gates, overlap, drain, a violation, workload.timeout, the resolved
//! form, `W=@<key>` and both error lines. Host timing is not deterministic, so this checks the
//! mechanics, not fingerprints.
//!
//! Needs Linux (/proc/uptime) and a busybox: HYDO_BUSYBOX=/path/to/busybox (the release's static
//! one). Without them the test says so and passes.

use hydo_composer as hc;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn onboard() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard")
}

fn meta(cmds: &[(&str, &str, &str)], checks: Value) -> Value {
    json!({"format": 1,
           "params": {"parallel": 3, "per_round": 2, "round_ms": 100, "command_timeout": 5,
                      "drain_timeout": 3, "check_every": 2, "check_within": 30,
                      "max_commands": 0, "budget_ms": 0},
           "commands": cmds.iter().map(|(k, id, run)| json!({
               "id": id, "kind": k, "name": id[k.len() + 1..], "run": run, "timeout": 5,
               "content": "0000000000000000"})).collect::<Vec<_>>(),
           "checks": checks, "regressions": [], "composed": true, "steps": 6, "stable_compose": 1})
}

const SCRIPT: &str = r#"set -e
T=$WORK; E=$T/etc; mkdir -p $E $T/tmp/hydo
cp $R1/etc/hydo/compose.sh $R1/etc/hydo/compose.list $R1/etc/hydo/compose.checks $E/
{ echo HYDO_STEPS=6; cat $R1/manifest.sh; } > $E/manifest.sh
echo "export PATH=$(dirname $BB):\$PATH" > $E/prelude.sh
echo "0000002a,p3,q2,r100" > $T/tmp/hydo/compose.W
export BB HYDO_ETC=$E HYDO_TMP=$T/tmp/hydo HYDO_CONSOLE=$T/console
CS="$BB sh $E/compose.sh"
$CS first
$CS step 0 3; echo "== fault point at step 3 (the daemon waits; commands keep running)" >> $T/console
$BB sleep 0.3
$CS step 3 6
for i in 1 2; do $CS end $i >> $T/console || echo "end $i rc=$?" >> $T/console; done
$CS check 0 >> $T/console
$CS verdict >> $T/console || echo "verdict rc=$?" >> $T/console
echo "== plan:"; $CS plan
echo "== console:"; cat $T/console
grep -q "HYDO-COMPOSE done" $T/console
grep -q "parallel_driver_v (entry 1) reported a violation" $T/console
# 2. a composition longer than workload.timeout: step stops it (every command killed, recorded)
: > $T/console; rm -rf $T/tmp/hydo/compose
echo HYDO_WORKLOAD_TIMEOUT=3 >> $E/manifest.sh
cp $R2/etc/hydo/compose.list $E/compose.list
echo 00000001,p2,q2,r100 > $T/tmp/hydo/compose.W
t0=$(cut -d. -f1 /proc/uptime)
$CS step 0 6 || echo "step rc=$?" >> $T/console
$CS verdict >> $T/console
echo "== stopped after $(( $(cut -d. -f1 /proc/uptime) - t0 )) s:"; grep -v DECL $T/console
grep -q "status=killed (workload.timeout)" $T/console
[ -z "$(ps -o args | grep -v grep | grep 'compose.sh run1')" ] || { echo "a command is still running"; exit 1; }
# 3. the resolved form (M98): commands by id, whatever the list order; W=@<key> from compose.saved;
#    an id the list lacks fails setw (the init's knob path) with a console line
: > $T/console; rm -rf $T/tmp/hydo/compose
cp $R3/etc/hydo/compose.list $E/compose.list
$CS setw "$RES"
[ "$($CS plan | tr '\n' ' ')" = "3 0 2 parallel_driver 11 4 0 1 parallel_driver 12 9 2 2 parallel_driver 13 " ] || { echo "resolved plan: $($CS plan)"; exit 1; }
cp -r $R3/etc/hydo/compose.saved $E/
$CS setw "@$KEY"
[ "$($CS plan | wc -l)" = 3 ]
$CS step 0 6; $CS verdict >> $T/console
grep -q "HYDO-COMPOSE plan W=@$KEY resolved p=2 r=100 t=0 rounds=6 entries=3" $T/console
grep -q "HYDO-CMD start i=9 r=2 kind=parallel_driver cmd=parallel_driver_a" $T/console
if $CS setw ":p2,r100,cparallel_driver_gone,0.0.0.5" > /dev/null; then echo "a missing command was accepted"; exit 1; fi
grep -q "HYDO-COMPOSE error: W=:p2,r100,cparallel_driver_gone,0.0.0.5: the composition uses command parallel_driver_gone, which this image does not have" $T/console
if $CS setw @ffff > /dev/null; then echo "a missing saved composition was accepted"; exit 1; fi
grep -q "this image carries no saved composition ffff" $T/console
echo "== resolved form:"; grep -v DECL $T/console
echo "compose-dryrun: OK"
"#;

#[test]
fn guest_composer_dry_run() {
    let Some(bb) = std::env::var("HYDO_BUSYBOX")
        .ok()
        .filter(|_| Path::new("/proc/uptime").exists())
    else {
        eprintln!(
            "SKIPPED: the guest composer dry run needs Linux and HYDO_BUSYBOX=<static busybox>"
        );
        return;
    };
    let work = std::env::temp_dir().join(format!("hydo-composer-dry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let r = |i: usize| work.join(format!("r{i}"));
    let x = |s: &str| s.replace("$BB", &bb);
    // 1. the dry run's command list, in its order (not sorted, as the original wrote it)
    let m1 = meta(
        &[
            ("anytime", "anytime_x", &x("exec $BB true")),
            ("eventually", "eventually_e", "echo recovered"),
            ("finally", "finally_f", "echo final"),
            ("first", "first_init", "echo init"),
            (
                "parallel_driver",
                "parallel_driver_a",
                &x("exec $BB sleep 0.25"),
            ),
            ("parallel_driver", "parallel_driver_b", "exit 3"),
            ("serial_driver", "serial_driver_s", &x("exec $BB sleep 0.1")),
            ("parallel_driver", "parallel_driver_v", "exit 100"),
        ],
        json!([{"name": "c_ok", "kind": "final", "run": "echo fine", "msg": "c_ok"}]),
    );
    hc::stage(&m1, &r(1), &onboard(), None).unwrap();
    std::fs::write(
        r(1).join("manifest.sh"),
        hc::shell_lines(&m1).join("\n") + "\n",
    )
    .unwrap();
    let m2 = meta(
        &[(
            "parallel_driver",
            "parallel_driver_long",
            &x("exec $BB sleep 30"),
        )],
        json!([]),
    );
    hc::stage(&m2, &r(2), &onboard(), None).unwrap();
    // 3. a resolved composition staged as a saved regression (W=@<key>)
    let res = ":p2,r100,cparallel_driver_a+parallel_driver_b,3.0.0.11,4.0.1.12,9.2.0.13";
    let key = hc::key_of(res);
    let mut m3 = meta(
        &[
            ("anytime", "anytime_0new", &x("exec $BB true")),
            (
                "parallel_driver",
                "parallel_driver_b",
                &x("exec $BB sleep 0.05"),
            ),
            (
                "parallel_driver",
                "parallel_driver_a",
                &x("exec $BB sleep 0.05"),
            ),
        ],
        json!([]),
    );
    m3["regressions"] = json!([{"name": "saved", "expect": "violation",
                                "schedule": format!("disk=none | run W=@{key}"), "compose": res}]);
    hc::stage(&m3, &r(3), &onboard(), None).unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg(SCRIPT)
        .env("WORK", work.join("t"))
        .env("BB", &bb)
        .env("R1", r(1))
        .env("R2", r(2))
        .env("R3", r(3))
        .env("RES", res)
        .env("KEY", &key)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    eprintln!("{text}{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && text.contains("compose-dryrun: OK"));
    let _ = std::fs::remove_dir_all(&work);
}
