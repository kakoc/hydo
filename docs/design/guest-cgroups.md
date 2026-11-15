# Thread pauses on the Mac and KVM engines: guest kernels with cgroups

Status: built in Milestone 234 (2026-11-14). The user chose option A below on 2026-11-14: images
built before keep the kernels they were recorded with. The change raises the fingerprint epoch
(`hydo_image::EPOCH`) from 2 to 3 and ships in hydo 0.3.4. The proofs and the
pins are in `docs/HISTORY.md` Milestone 234 and `docs/BUILD-PINS.md`. A pause of threads fails
Qdrant's flaky test on the Mac engine and on KVM with one vCPU.

## The problem

A thread pause, `P=workload/<thread>[#<k>]@<at>~<off>:<dur>` (Milestone 226), stops named threads
of the workload with the guest kernel's cgroup v2 freezer. A cgroup is a group of tasks that the
kernel controls together. The freezer is the file `cgroup.freeze` of a cgroup: `1` stops every
thread in it, `0` lets them run again.

Only hvx's guest kernel `bzImage` has cgroups. The four guest kernels of the other engines were
built from `allnoconfig` and a list of options without `CONFIG_CGROUPS`.
The table lists them:

| kernel | engine | sha256 | built by |
|---|---|---|---|
| `Image-explore` | the Mac engine | `f80c5655…` | `hv/hvm/explore/build-guest.sh` (Nix cross gcc) |
| `Image-explore-netns` | the Mac engine, `network: bridge` | `806807e7…` | `hv/hvm/explore/build-guest-netns.sh` (Arm GNU Toolchain 13.3.Rel1) |
| `bzImage-kvm` | the KVM engine, 1 vCPU | `d50fbc67…` | `hv/kvm/linux/kernel.nix` (pinned nixpkgs) |
| `bzImage-kvm-smp` | the KVM engine, 2 to 8 vCPUs | `b40b2711…` | `hv/kvm/linux/kernel-smp.nix` |

So `hydo_engine::refusal_on` refused a thread pause on the Mac and KVM engines. Qdrant's flaky
test (`hydo/onboard/examples/qdrant-flaky-10200`) fails on every hvx run of
`P=workload/tests::wal_reco@0~4433:500`, and the other two engines could not run that schedule.

## The kernel options

Four new kernels add the same lines to the lists of those four
(`hv/kvm/linux/hydo-kvm-cgroups.config`, `hv/hvm/explore/hvm-cgroups.config`):

```text
CONFIG_CGROUPS=y
CONFIG_CGROUP_SCHED=y
CONFIG_FAIR_GROUP_SCHED=y
CONFIG_CFS_BANDWIDTH=y
```

- `CONFIG_CGROUPS` gives the cgroup v2 file system, threaded cgroups and `cgroup.freeze`. The
  v2 freezer has no option of its own; `CONFIG_CGROUP_FREEZER` is the v1 controller, which hydo
  does not use. This line alone is enough for thread pauses.
- The three scheduler lines give the `cpu` controller and its `cpu.max` file, a CPU limit. The
  etcd example (`hydo/onboard/examples/etcd-flaky-22796`) writes `cpu.max` to fail its flaky test
  on hvx (Milestone 211). With these lines that limit can run on the Mac and KVM engines too,
  without a second epoch change.
- The lists set every other controller off, and also automatic per-session groups and pressure
  accounting (`CONFIG_PSI`), whose averages a timer updates. Each build checks that every `=y`
  line is on and every `=n` line is off.

hvx's kernel is nixpkgs' `linux_6_6`, which has all four options. Milestone 226's thread pauses
and Milestone 211's `cpu.max` both ran on it. hvx's kernel does not change.

The controller's freeze script (`hydo-control` `schedule/threads.rs`) mounts `cgroup2` on
`/sys/fs/cgroup` when it is not mounted. The kernel makes that directory once it has
`CONFIG_CGROUPS` and `CONFIG_SYSFS`. The agent, the controller's commands and the images do not
change.

## How the kernels are built and pinned

| new kernel | for | recipe |
|---|---|---|
| `Image-explore-cg` | the Mac engine | `NETNS=0 sh hv/hvm/explore/build-guest-cg.sh` |
| `Image-explore-netns-cg` | the Mac engine, `network: bridge` | `NETNS=1 sh hv/hvm/explore/build-guest-cg.sh` |
| `bzImage-kvm-cg` | the KVM engine, 1 vCPU | `CGROUPS=1 sh hv/kvm/linux/build-kernel.sh` (`kernel-cg.nix`) |
| `bzImage-kvm-smp-cg` | the KVM engine, 2 to 8 vCPUs | `CGROUPS=1 SMP=1 sh hv/kvm/linux/build-kernel.sh` |

- **The KVM kernels.** `hv/kvm/linux/kernel-cg.nix` takes `kernel.nix` or `kernel-smp.nix` and
  appends `hydo-kvm-cgroups.config` to its list. Neither recipe changes, so the old pins rebuild.
- **The Mac kernels.** Both are built with `build-guest-netns.sh`'s method: the kernel.org tarball
  and the Arm GNU Toolchain, both pinned by sha256, without Nix. `build-guest.sh` takes its
  compiler from the Nix flake registry, which is not pinned, so it cannot give pinned bytes again.
- **Where.** The bare-metal machine builds them, with Nix 2.18.9 for the KVM kernels and the Arm
  toolchain for the Mac kernels, each twice. Outside Nix's sandbox a `nix-build --check` of a
  KVM kernel can differ (docs/BUILD-PINS.md). So the pins of the KVM kernels are the bytes that a
  sandboxed Nix on GitHub's runners builds twice, as for `bzImage-kvm-smp` (Milestone 219).
- **The pins** are in `docs/BUILD-PINS.md`, `hydo_image::kernels` (the Mac kernels),
  `hydo_kvm::kernel` (the KVM kernels) and both releases' `assets.sha256`.

## How an image gets its kernel

An image built at epoch 3 or later boots a kernel with cgroups on every engine. An image built
before keeps the kernel it was recorded with, so its fingerprints stay.

- **The Mac engine.** An image's record names its kernel by sha256 (Milestone 155).
  `hydo_image::kernels::record_for` names `Image-explore-cg` or `Image-explore-netns-cg`; an older
  image names `Image-explore` or `Image-explore-netns`, as before. The run code does not change.
- **The KVM engine.** The record names hvx's `bzImage` (`0a9ede35…`) for every x86_64 image, and
  hvx's kernel does not change, so the sha256 cannot tell old images from new ones. Each KVM build
  in `hydo_kvm::kernel` names the epochs of the images it boots, and `twin_for(of, vcpus, epoch)`
  takes the epoch from the image's record.
- **The refusal.** `refusal_on(kind, vcpus, epoch, schedule)` takes the image's epoch. On the Mac
  and KVM engines it refuses a thread pause on an image older than epoch 3, before any VM, with a
  message that says to rebuild the image. `hydo explore --pause-thread` refuses such an image the
  same way. `hydo_image::defaults(epoch, arch).cgroups` is the one rule.

## What the releases ship

Both kernel sets: an image built by 0.3.x runs offline with the kernel it names. The new kernels
add about 5.9 MB to the Mac archive and about 6.5 MB to the Linux archive:

| archive (0.3.2) | size | the two new kernels, compressed |
|---|---|---|
| `hydo-mac-0.3.2.tar.gz` | 34.6 MB | `Image-explore` gzips to 2.78 MB and `Image-explore-netns` to 3.08 MB; the new ones are of the same size |
| `hydo-linux-0.3.2.tar.gz` | 71.9 MB | `bzImage-kvm` is 3.12 MB and `bzImage-kvm-smp` 3.36 MB, compressed already |

The other way was to ship only the new kernels and let the install cache (Milestone 162) fetch
an old kernel by its sha256 from 0.3.2's archive. That costs a download of the whole old archive
(35 MB or 72 MB) on first use and needs the network, to save 6 MB.

## Every recorded fingerprint moves for new images

The epoch is a field of the image's record `/etc/hydo/image.json`, which is inside the image. So
every image built at epoch 3 has new bytes and new fingerprints on all three engines, also on hvx,
whose kernel does not change. Milestone 204 showed this: on the Mac and KVM only the record
changed, and all 76 Mac and 13 KVM runs got new fingerprints with the same verdicts.

Under the rule "Re-recording runs in batches" (AGENTS.md), this change does not re-record
`hydo/onboard/recorded.json` itself. Its values wait in open item 105 for the next pass.

## Option B, which was not taken

Option B replaced the four kernels outright under their old names and refused every image of an
older epoch, on every engine. It would have removed the epoch tables (`hydo_image::defaults`, the
KVM engine's `--image-epoch`) instead of adding a third epoch to them. Without the refusal, an
x86_64 image of epoch 2 would boot the new KVM kernel without a word, because the twin lookup
keys on hvx's unchanged `bzImage`. The user kept the strict rule: an image built before keeps its
recorded values.
