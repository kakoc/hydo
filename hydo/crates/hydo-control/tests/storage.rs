//! The storage faults of one service (`G=`, `L=`; docs/design/storage-faults.md) on the scripted
//! agent and engine: the own file system is mounted after the data disk; `L=` stops the service,
//! shuts its file system down, kills it, mounts the file system again and restarts the service at
//! its step; `G=` with `+down` kills, corrupts (the scripted guest lists no file, so nothing is
//! changed) and restarts; a `K=` on the same service with an overlapping down time does not wait
//! for ever for a process the storage fault started again (a hang the first exploration hit).
//! Each run is made twice (one transcript) and typed again from its transcript. The scripted
//! guest answers commands with made-up output, so the tests look at what the controller did, not
//! at the verdict.

use crate::support;

use hydo_agent_proto::msg::Command;
use hydo_control::{Ending, Plan};
use hydo_schedule::Schedule;
use support::{Conv, Item};

fn plan() -> Plan {
    let text = "name: storage-test
data:
  dirs: [db]
services:
  - name: db
    cmd: db-server
    ready: db-ping
    storage: {dir: db, size_mib: 64}
  - name: web
    cmd: web-server
    ready: \"true\"
    storage: web
groups:
  - name: st
    services: [db, web]
workload:
  steps: 8
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
faults: [kill, corrupt, powerloss]
";
    let m = hydo_manifest::from_embedded(text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

fn run(p: &Plan, sched: &str) -> support::Ran {
    let b = support::boot(p, "storage-test").unwrap();
    let s = Schedule::parse(sched).unwrap();
    let a = support::run_schedule(&b, &s).unwrap();
    let again = support::run_schedule(&b, &s).unwrap();
    assert_eq!(
        a.run.fin.transcript, again.run.fin.transcript,
        "{sched}: two runs differ"
    );
    let mut reference = a.run.fin.transcript.clone();
    reference.header.controller = "replay".into();
    assert_eq!(
        a.replay.to_jsonl(),
        reference.to_jsonl(),
        "{sched}: the transcript typed again differs"
    );
    a
}

fn verdict(c: &Conv) -> String {
    match &c.fin.ending {
        Some(Ending::Verdict(v)) => v.clone(),
        e => panic!("not a verdict: {e:?}"),
    }
}

fn notes(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Note { line, .. } => Some(line.clone()),
            _ => None,
        })
        .collect()
}

fn mounts(items: &[Item]) -> Vec<(String, String, Option<String>)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Mount(m),
                ..
            } => Some((m.src.clone(), m.dst.clone(), m.data.clone())),
            _ => None,
        })
        .collect()
}

fn runs(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Run(r),
                ..
            } => Some(r.argv.join(" ")),
            _ => None,
        })
        .collect()
}

fn has(ns: &[String], s: &str) -> bool {
    ns.iter().any(|n| n.contains(s))
}

#[test]
fn the_own_file_system_is_mounted_after_the_data_disk() {
    let p = plan();
    let b = support::boot(&p, "storage-test").unwrap();
    let m = mounts(&b.boot.items);
    assert_eq!(m[0].0, "/dev/vda2");
    assert_eq!(
        m[1],
        (
            "/dev/vda5".into(),
            "/data/db".into(),
            Some("errors=remount-ro".into())
        ),
        "{m:?}"
    );
    assert_eq!(
        m.len(),
        2,
        "web's storage is a directory, not a file system"
    );
    // a schedule without the tokens sends no storage command in the run
    let r = run(&p, "disk=none | run K=db@2+2");
    assert!(mounts(&r.run.items).is_empty());
    assert!(!runs(&r.run.items)
        .iter()
        .any(|c| c.contains("trigger_fs_error")));
}

#[test]
fn a_power_loss_shuts_the_file_system_down_and_restarts_the_service() {
    let r = run(&plan(), "disk=none | run L=db@2+2");
    let ns = notes(&r.run.items);
    assert!(has(&ns, "POWER LOSS of db before step 2"), "{ns:#?}");
    assert!(has(&ns, "restarting db before step 4"), "{ns:#?}");
    assert!(runs(&r.run.items)
        .iter()
        .any(|c| c.contains("echo hydo-power-loss > /sys/fs/ext4/vda5/trigger_fs_error")));
    assert_eq!(mounts(&r.run.items)[0].0, "/dev/vda5");
    verdict(&r.run);
    // a service without its own file system: logged and ignored
    let r = run(&plan(), "disk=none | run L=web@2+2");
    assert!(has(&notes(&r.run.items), "L: web has no own file system"));
}

#[test]
fn a_corruption_while_down_kills_and_restarts() {
    let r = run(&plan(), "disk=none | run G=web@3+2:flip,n=2,seed=5");
    let ns = notes(&r.run.items);
    assert!(
        has(&ns, "KILLED web before step 3 (to corrupt its files)"),
        "{ns:#?}"
    );
    assert!(has(&ns, "G: no file to change in /data/web"), "{ns:#?}");
    assert!(has(&ns, "restarting web before step 5"), "{ns:#?}");
    // the scripted guest answers the final check with made-up output: the verdict is the run's end
    verdict(&r.run);
}

#[test]
fn a_kill_and_a_storage_fault_on_one_service_restart_it_once() {
    for s in [
        "disk=none | run K=db@3+4 G=db@3+2:flip",
        "disk=none | run K=db@1+5 L=db@2+1",
        "disk=none | run K=db@4+1 G=db@2+5:zero",
    ] {
        // the run ends (the scripted guest's made-up answers decide which verdict)
        let r = run(&plan(), s);
        verdict(&r.run);
    }
    let r = run(&plan(), "disk=none | run K=db@4+1 G=db@2+5:zero");
    assert!(
        has(
            &notes(&r.run.items),
            "db is running before step 7; not restarted again"
        ),
        "{:#?}",
        notes(&r.run.items)
    );
}

/// Milestone 245: `G=` on a list kills every service of it first, then corrupts each (the k-th
/// with the seed plus k), and starts each again; a group's selector is resolved and logged.
#[test]
fn a_corruption_of_several_services_kills_all_first() {
    let r = run(&plan(), "disk=none | run G=db,web@3+2:flip,seed=5");
    let ns = notes(&r.run.items);
    let at = |s: &str| {
        ns.iter()
            .position(|n| n.contains(s))
            .unwrap_or_else(|| panic!("{s}: {ns:#?}"))
    };
    let (kd, kw) = (
        at("KILLED db before step 3 (to corrupt its files)"),
        at("KILLED web before step 3 (to corrupt its files)"),
    );
    let (gd, gw) = (
        at("G: no file to change in /data/db"),
        at("G: no file to change in /data/web"),
    );
    assert!(kd < kw && kw < gd && gd < gw, "{ns:#?}");
    assert!(at("restarting db before step 5") < at("restarting web before step 5"));
    verdict(&r.run);
    let r = run(&plan(), "disk=none | run G=st.all@3:zero");
    let ns = notes(&r.run.items);
    assert!(has(&ns, "G: st.all = db,web"), "{ns:#?}");
    assert!(!has(&ns, "KILLED"), "{ns:#?}");
    // one known service in a list takes the plain path: the same fault lines as the plain token
    // (the scripted guest's later answers depend on the schedule's text)
    let fault_lines = |sched: &str| -> Vec<String> {
        notes(&run(&plan(), sched).run.items)
            .into_iter()
            .filter(|n| n.contains("KILLED") || n.contains("G: ") || n.contains("restarting"))
            .collect()
    };
    let a = fault_lines("disk=none | run G=web@3+2:flip,seed=5");
    let b = fault_lines("disk=none | run G=web,nosuch@3+2:flip,seed=5");
    assert_eq!(
        b,
        [
            vec!["hydo-ob: G: no service nosuch, ignored".to_string()],
            a
        ]
        .concat()
    );
}

/// Milestone 245: the strict power loss turns the periodic writeback off and mounts the
/// service's file system again without auto_da_alloc before the first step, logs what reached
/// the disk at the loss, and, without network: bridge, says that the links are not cut.
#[test]
fn a_strict_power_loss_stops_the_writeback_first() {
    let r = run(&plan(), "disk=none | run L=db@2+2:strict");
    let cs = runs(&r.run.items);
    let ns = notes(&r.run.items);
    let wb = cs
        .iter()
        .position(|c| c.contains("echo 0 > $w"))
        .unwrap_or_else(|| panic!("{cs:#?}"));
    let fe = cs
        .iter()
        .position(|c| c.contains("trigger_fs_error"))
        .unwrap();
    assert!(wb < fe);
    let m = mounts(&r.run.items);
    assert_eq!(
        m[0],
        (
            "/dev/vda5".into(),
            "/data/db".into(),
            Some("errors=remount-ro,noauto_da_alloc".into())
        ),
        "{m:?}"
    );
    assert!(r.run.items.iter().any(|i| matches!(i,
        Item::Send { cmd: Command::Mount(m), .. }
            if m.flags.as_ref().is_some_and(|f| f.contains(&hydo_agent_proto::msg::MountFlag::Remount)))));
    assert!(has(&ns, "STRICT power loss of db armed"), "{ns:#?}");
    assert!(has(&ns, "STRICT power loss of db before step 2"), "{ns:#?}");
    assert!(has(&ns, "L: db's links are not cut"), "{ns:#?}");
    assert!(
        has(
            &ns,
            "POWER LOSS of db before step 2 (/dev/vda5 shut down and mounted again; what was not synced is lost)"
        ),
        "{ns:#?}"
    );
    assert!(has(&ns, "restarting db before step 4"), "{ns:#?}");
    // the plain power loss sends none of it
    let r = run(&plan(), "disk=none | run L=db@2+2");
    assert!(!runs(&r.run.items).iter().any(|c| c.contains("echo 0 > $w")));
    assert!(!has(&notes(&r.run.items), "STRICT"));
}

/// Milestone 245: with network: bridge the strict power loss sets the service's bridge port down
/// right after it stops, and up again before it starts (or at the end, if it is down then).
#[test]
fn a_strict_power_loss_cuts_the_links_while_down() {
    let text = "name: storage-bridge
network: bridge
data:
  dirs: [db]
services:
  - name: db
    cmd: db-server
    ready: db-ping
    addr: 10.89.0.11
    port: 4001
    storage: {dir: db, size_mib: 64}
  - name: peer
    cmd: peer-server
    ready: \"true\"
    addr: 10.89.0.12
    port: 4002
workload:
  steps: 8
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
faults: [kill, powerloss, network]
";
    let m = hydo_manifest::from_embedded(text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    let p = Plan::from_manifest(&m).unwrap();
    for (s, back) in [
        (
            "disk=none | run L=db@2+2:strict",
            "LINKS of db back before step 4",
        ),
        (
            "disk=none | run L=db@6+9:strict",
            "LINKS of db back at the end",
        ),
    ] {
        let r = run(&p, s);
        let cs = runs(&r.run.items);
        let ns = notes(&r.run.items);
        let down = cs
            .iter()
            .position(|c| c.ends_with("link set vh1 down"))
            .unwrap_or_else(|| panic!("{s}: {cs:#?}"));
        let up = cs
            .iter()
            .position(|c| c.ends_with("link set vh1 up"))
            .unwrap_or_else(|| panic!("{s}: {cs:#?}"));
        let fe = cs
            .iter()
            .position(|c| c.contains("trigger_fs_error"))
            .unwrap();
        assert!(down < fe && fe < up, "{s}: {cs:#?}");
        assert!(has(&ns, back), "{s}: {ns:#?}");
        assert!(has(&ns, "its links cut"), "{s}: {ns:#?}");
    }
    // the plain power loss leaves the links alone
    let r = run(&p, "disk=none | run L=db@2+2");
    assert!(!runs(&r.run.items)
        .iter()
        .any(|c| c.contains("link set vh1")));
}

/// An image service has its storage directory bound into its root (`mounts:`); that bind kept the
/// shut-down file system alive, and mounting the device again failed with "Resource busy" (the
/// YugabyteDB onboarding). Its power loss unmounts every mount of the device and the launcher's
/// once-per-boot marker; a plain service's unmounts only its directory, as before.
#[test]
fn a_power_loss_of_an_image_service_unmounts_its_binds_too() {
    let text = "name: storage-image
data:
  dirs: [db]
services:
  - name: db
    image: images/db.tar
    ready: \"true\"
    storage: {dir: db, size_mib: 64}
    mounts:
      - {src: /data/db, dst: /var/lib/db}
workload:
  steps: 8
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
faults: [kill, powerloss]
";
    let m = hydo_manifest::from_embedded(text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    let p = Plan::from_manifest(&m).unwrap();
    let r = run(&p, "disk=none | run L=db@2+2");
    let cs = runs(&r.run.items);
    assert!(
        cs.iter().any(|c| c.contains("awk '$1 == \"/dev/vda5\"")
            && c.contains("rm -f /tmp/hydo/oci-mnt.db.done")),
        "{cs:#?}"
    );
    assert!(has(&notes(&r.run.items), "POWER LOSS of db before step 2"));
    // a service that is not an image's keeps the plain unmount of its directory
    let r = run(&plan(), "disk=none | run L=db@2+2");
    let cs = runs(&r.run.items);
    assert!(cs.iter().any(|c| c.contains("umount")), "{cs:#?}");
    assert!(!cs.iter().any(|c| c.contains("oci-mnt")), "{cs:#?}");
}
