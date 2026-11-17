//! The cgroup2 mount at boot (Milestone 283, `guest.cgroup2`): the host controller mounts the
//! cgroup2 file system on `/sys/fs/cgroup` in the boot conversation, before the S0 snapshot, so
//! the first boot, a recovery boot and a reboot (both start from S0) have it. What an absent key
//! means is the image's fingerprint epoch: on from epoch 4 ([`plan_of_image`]). A plan read from
//! a manifest alone, as every other test here reads one, mounts nothing unless the key says so,
//! so the recorded transcripts stay the same.

use crate::support::{self, Item};
use crate::tmpdir::TmpDir;
use hydo_agent_proto::msg::Command;
use hydo_control::agent::plan_of_image;
use hydo_control::Plan;
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};

const MANIFEST: &str = "name: cg-test
services:
  - name: n1
    cmd: node 1
    ready: ping 1
workload:
  steps: 2
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
";

fn with_key(v: Option<bool>) -> String {
    match v {
        None => MANIFEST.to_string(),
        Some(b) => format!("{MANIFEST}guest:\n  cgroup2: {b}\n"),
    }
}

fn plan_of(text: &str) -> Plan {
    let m = hydo_manifest::from_embedded(text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

/// The cgroup2 mounts a conversation sent, and the index of each among its items.
fn cgroup2_mounts(items: &[Item]) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| match it {
            Item::Send {
                cmd: Command::Mount(m),
                ..
            } if m.fstype == "cgroup2" => {
                assert_eq!(
                    (m.src.as_str(), m.dst.as_str()),
                    ("cgroup2", "/sys/fs/cgroup")
                );
                assert_eq!((&m.flags, &m.data), (&None, &None));
                Some(i)
            }
            _ => None,
        })
        .collect()
}

fn snap_at(items: &[Item], name: &str) -> usize {
    items
        .iter()
        .position(|it| matches!(it, Item::Snap { name: n, .. } if n == name))
        .unwrap_or_else(|| panic!("no snapshot {name}"))
}

#[test]
fn the_boot_mounts_cgroup2_before_s0_when_the_plan_says_so() {
    let p = plan_of(&with_key(Some(true)));
    assert_eq!(p.cgroup2, Some(true));
    let (c, _) = support::boot_conv(&p, "cg-on").unwrap();
    let at = cgroup2_mounts(&c.items);
    assert_eq!(at.len(), 1, "one mount in the boot conversation");
    assert!(at[0] < snap_at(&c.items, "S0"), "the mount comes before S0");
    // and before the data disk's mount and every service
    let data = c
        .items
        .iter()
        .position(
            |it| matches!(it, Item::Send { cmd: Command::Mount(m), .. } if m.fstype != "cgroup2"),
        )
        .unwrap();
    assert!(at[0] < data);
}

#[test]
fn without_the_key_or_with_false_nothing_is_mounted() {
    for v in [None, Some(false)] {
        let p = plan_of(&with_key(v));
        assert_eq!(p.cgroup2, v);
        let (c, _) = support::boot_conv(&p, "cg-off").unwrap();
        assert!(cgroup2_mounts(&c.items).is_empty(), "{v:?}");
    }
}

#[test]
fn a_recovery_boot_starts_from_s0_and_mounts_nothing_again() {
    let p = plan_of(&with_key(Some(true)));
    let b = support::boot(&p, "cg-cut").unwrap();
    let s = Schedule::parse("disk=cut dseed=1 | run C=1").unwrap();
    let r = support::run_schedule(&b, &s).unwrap();
    assert!(cgroup2_mounts(&r.run.items).is_empty());
    let rec = r.recovery.expect("a power cut gives a recovery boot");
    assert!(cgroup2_mounts(&rec.items).is_empty());
}

/// An arm64 image of `text` built at `epoch`: the manifest, the pinned tools' paths that the
/// manifest's default faults need (`manifest.sh`) and the image record only.
fn image(d: &Path, text: &str, epoch: u64) -> PathBuf {
    let root = d.join(format!("root{epoch}"));
    let etc = root.join("etc/hydo");
    std::fs::create_dir_all(&etc).unwrap();
    std::fs::write(etc.join("hydo.yaml"), text).unwrap();
    std::fs::write(
        etc.join("manifest.sh"),
        "HYDO_TC=/bin/tc\nHYDO_IP=/bin/ip\nHYDO_IPT=/bin/iptables\n",
    )
    .unwrap();
    let mut rec = hydo_image::kernels::record_with("arm64", false, None);
    rec.epoch = epoch;
    std::fs::write(
        root.join(hydo_image::RECORD_PATH),
        hydo_image::canonical(&rec),
    )
    .unwrap();
    let img = d.join(format!("explore-ob-e{epoch}-arm64.cpio.gz"));
    hydo_build::cpio::write_cpio_gz(&root, &img, 0, None, false).unwrap();
    img
}

#[test]
fn the_image_epoch_decides_what_an_absent_key_means() {
    let d = TmpDir::new(&format!("hydo-control-cgroup2-{}", std::process::id()));
    for (epoch, key, want) in [
        (1, None, false),
        (2, None, false),
        (3, None, false),
        (4, None, true),
        (4, Some(false), false),
        (3, Some(true), true),
    ] {
        let sub = d.join(format!("e{epoch}-{key:?}"));
        std::fs::create_dir_all(&sub).unwrap();
        let img = image(&sub, &with_key(key), epoch);
        let p = plan_of_image(&img).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            p.cgroup2,
            Some(want),
            "epoch {epoch}, guest.cgroup2 {key:?}"
        );
    }
}
