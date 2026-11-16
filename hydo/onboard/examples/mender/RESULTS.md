# Mender under hydo, a standalone A/B update under power cuts and kills

Mender (github.com/mendersoftware/mender, by Northern.tech) updates embedded Linux devices. This
example puts a device with two root file system partitions and Mender's standalone deployments
under hydo's power cuts and kills. An operator installs an Artifact (Mender's update file),
reboots the device, and commits the update. The device is built like the RAUC and SWUpdate
examples (`../rauc/RESULTS.md`, `../swupdate/RESULTS.md`), with Mender's own storage layout and
U-Boot integration.

## The short answers

- **What was found?** Two Mender candidates that no report names.
  1. Mender syncs the state scripts of an Artifact before the scripts' `version` file has its
     content. After a power cut between an install and the next writeback of Linux's page cache,
     the file is empty. Then the update can be neither committed nor rolled back: the healthy v3
     that booted fails its commit, and Mender names its artifact `v3_INCONSISTENT`
     (`disk=cut | run C=4`).
  2. When a power cut or a kill interrupts `mender-update commit`, the operator's next
     `mender-update commit` rolls the update back, prints `Rolled back.`, and exits 0, the exit
     code of a successful commit. When U-Boot had already made the update permanent, the rollback
     undoes it (`disk=cut | run A=mender.commit_started@3+7:cut`).
- **When do they happen?** Finding 1 needs Artifacts with state scripts, and a power cut or a
  hard reset after an install, before the reboot's `sync` or the page cache's writeback. Finding
  2 needs an interruption between Mender's save of the state `ArtifactCommit_Enter` and its save
  of the next state. That window is about 8 ms of guest time on the Mac engine and about 88 ms on
  hvx.
- **Which engines ran it?** The Mac engine and hvx. Both findings give their violations on hvx,
  each with one fingerprint over two runs (the section "On hvx"). KVM did not run the example.
- **Did the rest hold?** Yes. A complete slot booted after every power cut, an update that was not
  committed rolled back, and the application's synced records survived. With Artifacts without
  state scripts, `C=4` and `C=6` pass.
- **What else?** mender-artifact 4.4.2 writes an Artifact's state scripts in a random order, so
  two builds of an Artifact with two or more scripts differ (finding 3).
- **What did hydo lack?** The section "What hydo lacked" at the end.

## The versions

| program | version | source |
|---|---|---|
| mender-update (the Mender client) | 5.2.0, the newest tag on 2026-11-16 | the release archive `mender-5.2.0.tar.gz` with its submodules, sha256 `436ae559…` |
| mender-flash | 1.1.0 | the tag's archive, sha256 `185495ba…` |
| mender-artifact (on the build host) | 4.4.2 | the tag's archive, sha256 `d8e9e18e…`, with `build/mender-artifact-script-order.patch` |
| libubootenv (`fw_printenv`, `fw_setenv`) | 0.3.8 | the tag's archive, sha256 `3374b516…` |
| LMDB (Mender's database) | 0.9.33 | the tag's archive, sha256 `9c352fa9…` |
| libarchive | 3.8.4 | the release archive, sha256 `b2c75b13…` |

`build/Dockerfile` builds each program statically for arm64 and x86_64 on Alpine 3.22 (pinned by
digest). mender-update is built without D-Bus, so mender-auth is inside it
(`MENDER_EMBED_MENDER_AUTH`), and it reads gzip Artifacts only. Standalone mode needs neither
D-Bus nor the server. Mender's `master` of 2026-11-13 (`470eca9a`) has 10 commits after 5.2.0.
None of them changes `src/artifact/`, `src/mender-update/standalone/`,
`src/mender-update/cli/` or `support/modules/rootfs-image`.

## The device

The device has one eMMC with a volatile write cache, laid out as Mender's U-Boot integration
lays out its storage device (`device/lib.sh`):

| offset | size | contents |
|---|---|---|
| 4 MiB and 8 MiB | 128 KiB each | U-Boot's redundant environment (meta-mender's `BOOTENV_SIZE` 0x20000) |
| 16 MiB | 32 MiB | root file system A, Mender's partition 2 |
| 48 MiB | 32 MiB | root file system B, Mender's partition 3 |
| 80 MiB | 48 MiB | the data partition (ext4): `/data/mender` and the application's records |

- **The slots** hold real root file systems (24 MiB ext4 images, `build/make-images.sh`): busybox,
  mender-update, mender-flash, `fw_printenv` and `fw_setenv`, Mender's rootfs-image Update Module,
  and `/etc/os-release` with the version. v1 to v5 differ in the version; v4 fails its health
  check. The booted slot is mounted read-only at `/sysroot` with the data partition at
  `/sysroot/data`, and every Mender command runs in a chroot there. So the root file system that
  Mender's module checks (`check_device_matches_root`) is the slot that U-Boot booted.
- **The storage model.** Linux sees the eMMC as `/dev/loop0` and its partitions as `/dev/loop2`
  (A), `/dev/loop3` (B) and `/dev/loop1` (data): loop devices with direct I/O. So every write
  reaches the eMMC, and every flush of a partition flushes the eMMC, as on a real eMMC. On the Mac
  engine the eMMC is an added disk with its own cache (`hydo-devices.yaml`, `guest.disks`). On
  hvx it is a file on the guest's data disk (`hydo.yaml`). The runs of the section "On hvx" used
  hvx; every other run on this page used the Mac engine.
- **Mender's configuration** is meta-mender's: `RootfsPartA` and `RootfsPartB` and the device
  type in the data partition, the data store at `/var/lib/mender`, a link to `/data/mender`, and
  a bootstrap Artifact that records v1 at the first command. mender-flash writes the inactive
  partition, as meta-mender recommends it.
- **U-Boot** is an emulation of Mender's U-Boot integration and of U-Boot's boot counter
  (`CONFIG_BOOTCOUNT_ENV`), in `uboot_choose` of `device/lib.sh`. The variables are
  `mender_boot_part` (2 or 3), `mender_boot_part_hex`, `upgrade_available`, `bootcount` and
  `bootlimit=1`. While `upgrade_available=1`, every boot counts `bootcount` up. When `bootcount`
  exceeds `bootlimit`, `mender_altbootcmd` switches the partition and clears
  `upgrade_available`. A slot is verified by its hash before it boots, as dm-verity would refuse a
  root file system with a changed block.
- **The state scripts.** Every Artifact carries Mender state scripts for `ArtifactInstall_Enter`,
  `ArtifactCommit_Enter`, `ArtifactCommit_Leave` and `ArtifactRollback_Enter`, and every slot has
  a `Download_Enter` script. Each prints one console record (`build/state-script.sh`), the event
  of a reaction `A=mender.<state>...`. They change nothing else on the device. With the build
  parameter `artifacts=plain` the operator installs the same Artifacts without state scripts.

## The operator

The workload (`device/client.sh`) runs 8 standalone deployments as Mender's documentation
describes them: `mender-update install /mnt/updates/v<N>.mender`, a reboot, and
`mender-update commit`. The Artifacts are v2, v3, v4 (the broken release), v5, and then the same
four again. The agent (`device/agent.sh`) runs each command on the device. After a boot of a
system that fails its health check, it runs `mender-update rollback` and reboots. The reboots are
real ones (`guest.reboot: true`, Milestone 263).

Each deployment takes two workload steps. Step 2k reboots into update k-1. Step 2k+1 checks the
booted version against the versions that the outcomes so far allow, commits, and installs
update k. So a `C=` power cut falls after an install and before its reboot, and a reaction on a
state script cuts inside a command. A commit whose answer is lost is sent once more.

## The checks

| check | what it says |
|---|---|
| `device.boots` | after any power cut the device boots a complete, verified slot |
| `device.version` | the booted version is one that the outcomes allow: an acknowledged install or commit survives, and an install that did not complete never boots |
| `device.primary_complete` | the partition in `mender_boot_part` holds a complete image |
| `mender.state_matches_slots` | with no update in progress, Mender's artifact name is the running version, and U-Boot boots that partition next with `upgrade_available=0` |
| `data.records_survive` | every record the application synced is there |
| `device.reboots_boot_allowed` | every reboot during the workload booted an allowed version, and every commit that exited 0 left the booted version permanent and named in Mender's database |
| `mender.commit_permanent` | the last check: when Mender still has an update in progress after the run, the operator commits. A commit that exits 0 must leave the booted version permanent and named in Mender's database, and the commit of an update that booted healthy must not fail |

The checks read Mender's database with LMDB's `mdb_dump`, without changing it. Only the last
check changes the device, which is why it comes last.

## Without faults

`disk=cut | run` runs 8 deployments with 10 reboots, 2 of them after v4's rollback. It holds every
check, `HYDO-ORACLE: OK invariants=7 held (device.boots: booted partition 2 v5 ...)`, with
`d854f95e3fe00126` on two runs on the Mac engine. With `--set artifacts=plain` it gives
`a7688db8be3af3ec`. The values on this page come from the image
`explore-ob-mender-ota-devices-arm64.cpio.gz`, sha256 `31df3901bb65554e…`, unless a row names
another one.

## What was found

### 1. After a power cut soon after an install, the update can be neither committed nor rolled back (a Mender candidate, new as far as the search found)

**The schedules.** On the Mac engine (`hydo-devices.yaml`):

| schedule | what is cut | verdict | fingerprint |
|---|---|---|---|
| `disk=cut \| run C=4` | the power, after the acknowledged install of v3 and before its reboot | VIOLATION `[mender.commit_permanent]`: the commit of the healthy v3 failed, and Mender names `v3_INCONSISTENT` | `e06faa5d58198d94` on 2 runs |
| `disk=cut \| run C=6` | the power, after the install of the broken v4 | VIOLATION `[mender.state_matches_slots]`: no update in progress, Mender names `v4_INCONSISTENT`, the device runs v3 | `780103d63e23cd06` on 2 runs |
| `disk=none \| run C=4` | the same as the first row, with no cache on the main disk | the same as the first row | `006c3b078505d65d` |
| `disk=cut \| run C=4`, `artifacts=plain` | the first row's cut, Artifacts without state scripts | OK, the commit of v3 holds | `8d8856a1f74fd536` |
| `disk=cut \| run C=6`, `artifacts=plain` | the second row's cut, without state scripts | OK, v4 rolls back to v3 | `88a7134f4ee4346c` |

**What happens.** `mender-update install` writes the Artifact's state scripts into
`/var/lib/mender/scripts`, with a file `version` that holds `3`. It installs v3 and exits 0. The
power is cut before the reboot. In the recovery boot the agent logs `Mender's Artifact scripts:
5 files, the version file 0 bytes`. U-Boot boots v3 once (`bootcount` 1), and v3 is healthy. The
operator's `mender-update commit` fails at the first state script: `Error reading the version
number from the version file`. Mender's error path then runs the rollback, whose scripts fail on
the same file. The rootfs-image module's `ArtifactRollback` still writes v2's partition back, so
U-Boot boots v2 next. Mender ends the update with a failed rollback, so it records the artifact
name `v3_INCONSISTENT`. With the broken v4 (`C=6`), the agent's `mender-update rollback` after
the health check fails the same way. U-Boot's boot counter returns to v3, and Mender names
`v4_INCONSISTENT` with no update in progress. Mender reports the artifact `<name>_INCONSISTENT`
when an update failed and its rollback failed too.

**The cause in Mender's code.** `Parse` in `src/artifact/v3/header/header.cpp` writes the
scripts, then the version file, inside one block:

```cpp
ofstream myfile(artifact_script_version_file);
...
myfile << to_string(conf.artifact_scripts_version);
...
// Sync the directory so we know it is permanent.
auto err = path::DataSyncRecursively(conf.artifact_scripts_filesystem_path);
```

`myfile` is still open when `DataSyncRecursively` runs `fdatasync` on every file. The one byte is
still in the `ofstream`'s buffer, so the sync makes an empty `version` file permanent. The byte
reaches Linux's page cache when `myfile` closes at the end of the block, and nothing syncs it
later. Mender's later database commits sync only its database file. The byte reaches the eMMC at
the page cache's writeback or at a `sync`. Linux's default `vm.dirty_expire_centisecs` writes a
page back about 30 s after the write; the guest's value was not read. hydo's reboot runs `sync`
(docs/design/devices.md), so in this workload the window is the time between an install and its
reboot. `CorrectVersionFile` in
`src/artifact/v3/scripts/executor.cpp` accepts a missing file and refuses an empty one. The sync
came in `b8a032e0` (2023-11-23, "Sync artifact scripts permanently to disk during artifact
parsing"), "so that we don't risk half written scripts in case of a spontaneous reboot after the
installation has begun". The code is the same on `master`, and it runs in managed mode too; only
standalone mode ran here.

**What Mender promises.** The overview, "Robust Operating System updates": "It must be able to
recover from an update that fails, including loss of power or network connectivity during the
update process." State scripts are a documented part of an Artifact (`mender-artifact write ...
-s`). The device uses Mender's documented integration: one storage device, U-Boot's environment
in its boot area, an ext4 data partition with default options, mender-flash, and the rootfs-image
module.

**The search.** On 2026-11-16, with no report found:

- Mender's Jira (tracker.mender.io, now northerntech.atlassian.net), every project but CFEngine's,
  through its public search: `version file`, `Error reading the version number`, `scripts
  version file`, `INCONSISTENT`, `state scripts power`, `artifact scripts sync`. The nearest
  ticket is MEN-4490 (2021): the client's Debian package lacked the scripts' version file.
- GitHub, mendersoftware/mender's issues and pull requests: `version file`, `Error reading the
  version number`, `DataSyncRecursively`, `state scripts sync`, `INCONSISTENT`. Pull request
  1690 (MEN-7721, 2024) checks the Artifact's own `version` entry, another file.
- Mender Hub (hub.mender.io): `Error reading the version number from the version file`, `scripts
  version file`, `_INCONSISTENT`, `state scripts power loss`, `commit fails after power loss`.
  Topic 8381 (2026-11-03) reports a Jetson reboot loop after a power cut before a commit, in a
  Jetson integration's boot control.

**Without hydo.** Not reproduced: the cause is plain in eight lines of `header.cpp`.

**The regressions.** `regressions/empty-scripts-version-commit-fails.schedule` (`C=4`) and
`regressions/empty-scripts-version-rollback-fails.schedule` (`C=6`).

### 2. After an interrupted commit, `mender-update commit` rolls the update back and exits 0 (a Mender candidate, new as far as the search found)

**The schedules.** On the Mac engine (`hydo-devices.yaml`):

| schedule | the interruption | verdict | fingerprint |
|---|---|---|---|
| `disk=cut \| run A=mender.commit_started@3+7:cut` | a power cut 7 ms into the commit of v2, after `upgrade_available=0` | VIOLATION `[mender.commit_permanent]`: the commit exited 0, and U-Boot boots v1 next | `d86603abb67442fe` on 2 runs |
| `disk=cut \| run A=mender.commit_started@3+7:kill-device` | a kill of the agent and mender-update at the same point | the same violation | `837445ed6cac8646` on 2 runs |
| `disk=cut \| run A=mender.commit_started@3:cut` | a power cut at the start of the commit, before `upgrade_available=0` | VIOLATION `[mender.commit_permanent]`: the commit exited 0, and Mender's artifact is v1 | `f84173ba7aaf1f25` |
| `disk=cut \| run A=mender.commit_started@3+8:cut` | a power cut 8 ms into the commit, after Mender saved the next state | OK | `f23c37c27bf666d1` |
| `disk=cut \| run A=ota.commit_started@3+9:cut`, `artifacts=plain` | a power cut 9 ms after the agent started the commit, without state scripts | the first row's violation | `5ecc32799b213b19` |

The event `ota.commit_started` comes from the agent, just before it runs `mender-update commit`.
Without state scripts, `+8` gave the third row's violation (`3f6f5b28e96595f7`), `+10` the first
row's (`92a2fe964db52d81`), and `+11` passed (`05d20a39f1e3897a`). Every `artifacts=plain`
value on this page comes from an image built before the last edits of the folder (two of the
regressions and a comment of `device/lib.sh`).

**What happens.** The operator installs v2 into partition 3, reboots into it, and runs
`mender-update commit`:

1. Mender saves the standalone state `ArtifactCommit_Enter` in its database and runs the
   `ArtifactCommit_Enter` state scripts.
2. The rootfs-image Update Module writes `upgrade_available=0` with `fw_setenv`. From here on,
   U-Boot boots partition 3 for good.
3. Mender saves the state `Before_ArtifactCommit_Leave`, runs the leave scripts, and records v2
   as its artifact in the `Cleanup` state.

A power cut or a kill between 1 and 3 leaves the state `ArtifactCommit_Enter` in Mender's
database. The operator's next `mender-update commit`, or `mender-update resume`, starts at
Mender's rollback state. The module's `ArtifactRollback` finds `upgrade_available=0` and writes
the partition of the file `orig-part` back: `mender_boot_part=2`. Mender removes its state,
keeps v1 as its artifact, prints `Rolled back.` and exits 0.

- After a cut in 2..3 (`+7`), the device runs v2 until its next reboot, and boots v1 after it.
  Mender's database names v1 while v2 runs. By the module's code, every `mender-update install`
  before that reboot fails, because the mounted root does not match `mender_boot_part`; no run
  installed in that state.
- After a cut in 1..2 (`+0`), U-Boot's boot counter has already returned to v1, and the rollback
  is right. The commit still exits 0 for an update that it did not commit.

**The cause in Mender's code.** In `src/mender-update/standalone/standalone.cpp`,
`StateMachine::SetStartStateFromStateData` maps the saved state `ArtifactCommit_Enter` to
`save_artifact_rollback_state_`, and `Commit` accepts that saved state. In
`src/mender-update/cli/actions.cpp`, `ResultHandler` turns a result without `Result::Failed`
into success. A completed rollback sets `RolledBack` and `Cleaned` and no `Failed`. So
`CommitAction` returns no error, and `mender-update` exits 0. In this example a commit that failed
exited 1, as when U-Boot had rolled back and the state was `Before_ArtifactCommit_Enter`. A
commit with nothing to do exited 2. Mender's unit tests (`tests/src/mender-update/cli/cli_test.cpp`)
stop a commit with `--stop-before`, and none of them resumes a commit from the saved state
`ArtifactCommit_Enter`.

**What Mender promises.** The standalone deployment page: "If you are happy with the deployment,
you can make it permanent by running the following command in your device terminal: `mender-update
commit`. By running this command, Mender will mark the update as successful and permanent." The
command's help: "Commit current Artifact. Returns (2) if no update in progress". The overview,
"Robust Operating System updates", promises recovery from a loss of power during the update.

**The search.** On 2026-11-16, with no report found:

- Mender's Jira, every project but CFEngine's: `standalone power`, `standalone rollback`, `commit
  exit code`, `interrupted commit`, `ArtifactCommit_Enter`, `spontaneous reboot standalone`,
  `commit rolled back`, `Rolled back`, `standalone resume`, `standalone commit`, `power loss
  commit`. The nearest tickets are MEN-6633 (the daemon reported success after a restart during
  an update, fixed in 2023), MEN-3895 (a rollback before the reboot set the wrong partition in
  the 2.x client, won't do) and MEN-7818 (a standalone regression from 4.x to 5.x, fixed).
- GitHub, mendersoftware/mender's issues and pull requests: `commit rolled back`, `standalone
  commit`, `InStateArtifactCommit`, `power loss`, `spontaneous reboot`, `exit code commit`,
  `standalone`, `rollback commit`, `exit code`, `Rolled back`, `power`. The open pull request
  2041 (2026-11-09) fixes a daemon-mode report after a restart in the `Cleanup` state, another
  state and mode.
- Mender Hub: `standalone commit power`, `mender-update commit rolled back`, `commit exit code`,
  `standalone commit rollback`, `power loss during commit`, `mender-update resume`,
  `mender-update commit exit 0`, `commit returns 0 rollback`. Topic 6411 (2023) says that in
  standalone mode the client does not run the error state after a power loss. Topic 4409 (2021)
  says that a bootloader rollback leaves Mender's update in progress until the next commit or
  rollback. Neither names the exit code or the rollback of a committed update.

**Without hydo.** Not reproduced: the cause is plain in `SetStartStateFromStateData` and
`ResultHandler`, a few lines each.

**The regressions.** `regressions/commit-after-cut-rolls-back.schedule` and
`regressions/commit-after-kill-rolls-back.schedule`.

### 3. mender-artifact writes an Artifact's state scripts in a random order (a tooling candidate)

`Scripts.Get` in mender-artifact's `artifact/scripter.go` returns the scripts from a Go map, and
Go randomizes the order of a map's iteration. So `mender-artifact write ... -s A -s B` writes the
scripts into `header.tar.gz` in a random order, and two runs with the same inputs give different
Artifacts. In this example's first two builds, three of the four Artifacts got other bytes. Pull
request 848 (merged on 2026-10-29, after 4.4.2) makes the archive's times follow
`SOURCE_DATE_EPOCH`. Its description says that the times are the only source of nondeterminism,
and `master` of 2026-11-16 (`e56270c4`) still iterates the map. The client sorts the scripts
before it runs them, so a device behaves the same with either order. The example's build sorts
them (`build/mender-artifact-script-order.patch`) and copies each Artifact's outer tar with fixed
times (`build/retime.go`). The search (mendersoftware/mender-artifact's issues and pull requests
for `reproducible`, `deterministic`, `scripts order`, `checksum differs`; Jira for `reproducible
artifact`, `mender-artifact reproducible`, `deterministic artifact`; Mender Hub for `reproducible
mender-artifact`) found MEN-10162, the ticket of pull request 848, and nothing on the order.

## What held

Every run booted a complete slot, and no synced record of the application was lost. These
schedules held every check on the Mac engine:

- a power cut at the start of an image's write (`A=mender.download_started@1:cut`, seed 11 of the
  exploration below: the device keeps v1);
- a power cut in a commit after Mender saved the next state (`A=mender.commit_started@3+8:cut`)
  and 50 ms or 1 s after a commit started (seeds 22 and 18);
- a power cut 5 s after a reboot started, at a reboot's start, and 5 s after v4's rollback
  started (seeds 24, 15 and 9);
- kills of the device's agent in 12 schedules, and clock steps from 30 days back to 300 s
  forward;
- `C=4` and `C=6` with Artifacts without state scripts, where U-Boot's boot counter and
  Mender's rollback return to v3.

## The explorations

On the Mac engine, the final image, one worker:

```sh
hydo explore --seeds 1-24 --workers 1 --strategy process --reactive fault \
  --react-events mender.download_started,mender.install_started,mender.commit_started,mender.commit_leave,mender.rollback_started,ota.reboot_started,ota.commit_started,ota.install_started
```

24 schedules gave 9 failures in three signatures, each reproduced with its fingerprint (128 runs
per hour, 10.0 s per run). The shrinker gave one minimal schedule per signature:

| signature | seeds | minimal schedule | fingerprint |
|---|---|---|---|
| finding 1: the commit of v5, which booted healthy, failed | 1, 5, 8, 12, 17, 20 | `disk=cut dseed=4127cc1347fae6ba \| run C=16` | `1b083708a8690666` |
| finding 1: no update in progress, Mender names `v4_INCONSISTENT`, v3 runs | 6, 21 | `disk=cut dseed=581cc68c71772e2c \| run K=device@14+1 C=16` | `49aaa2aa7d4bbe25` |
| finding 2: the commit exited 0 (`Rolled back.`), Mender names v1 | 10 | `disk=cut dseed=c054734a6f769762 \| run A=mender.commit_started:cut` | `7c6a84af1e182056` |

`C=16` cuts the power after the install of v5 in step 15, before the workload's last reboot. In
seed 6 the kill before step 14 stops the reboot into v4, so v4's scripts are not synced when the
power is cut. The explorer's report flags one passing run that checked nothing: in seed 11 the
power was cut before the first install was acknowledged.

An exploration of an earlier image (seeds 1-17, stopped there) gave the same signatures at seeds
6 and 10, with a check that missed a failed commit.

## On hvx

The example ran on hvx with `hydo.yaml` on 2026-11-16, on the bare-metal machine. There the eMMC
is the file `/data/flash/emmc.img` on the guest's data disk, and `disk=cut` gives that disk a
volatile write cache. The programs `hydo`, `hydo-hvx` and `hydo-build` were built from this tree
over the 0.3.4 Linux release, and each VM ran through `~/bin/hvx-run`, one at a time. The
x86_64 tools came from `sh build.sh amd64` on the Mac (section "Building the same bytes"), and
the loop module from `fetch-modules.sh`. Two builds gave the image
`explore-ob-mender-ota-disk.img` with the sha256 `6d46a6a1b4ea1a6d…`; with `--set
artifacts=plain` it is `d12f74e89b593aef…`.

| schedule | verdict on hvx | fingerprint | the same as on the Mac engine? |
|---|---|---|---|
| `disk=cut \| run` | OK `invariants=7 held (device.boots: booted partition 2 v5 ...)`, with 10 reboots | `43a98c8ee471b072` on 2 runs | yes |
| `disk=cut \| run C=4` (a regression) | VIOLATION `[mender.commit_permanent]`: the commit of the healthy v3 failed, and Mender names `v3_INCONSISTENT` | `a5450ade5f9535cc` on 2 runs | yes, the same text |
| `disk=cut \| run C=6` (a regression) | VIOLATION `[mender.state_matches_slots]`: no update in progress, Mender names `v4_INCONSISTENT`, and the device runs v3 | `bc2380c4573776f9` on 2 runs | yes, the same text |
| `disk=cut \| run C=4`, `artifacts=plain` | OK, the device runs v3 | `2323dea1290f9937` on 2 runs | yes |
| `disk=cut \| run C=6`, `artifacts=plain` | OK, the device runs v3 | `2a3e428b780f6fd5` on 2 runs | yes |
| `disk=cut \| run A=mender.commit_started@3+7:cut` (a regression) | VIOLATION `[mender.commit_permanent]`: the commit exited 0 (`Rolled back.`), and Mender's artifact is v1 while v1 runs | `216a4d3c4eaf41f7` on 2 runs | the same check, in the form of `@3:cut` on the Mac |
| `disk=cut \| run A=mender.commit_started@3+7:kill-device` (a regression) | the same violation as the row before | `a6ad6d8ee0e65e9f` on 2 runs | the same check, in the form of `@3:cut` on the Mac |
| `disk=cut \| run A=mender.commit_started@3:cut` | the same violation as the row before | `85acf4397831368d` on 2 runs | yes |
| `disk=cut \| run A=mender.commit_started@3+80:cut` | VIOLATION `[mender.commit_permanent]`: the commit exited 0 on v2 in partition 3, but U-Boot boots partition 2 (v1) next | `45683a8ad6f64cfa` on 3 runs | the text of `+7` on the Mac |
| `disk=cut \| run A=mender.commit_started@3+80:kill-device` | the same violation as the row before | `d129e303c1b8e8db` on 2 runs | the text of `+7:kill-device` on the Mac |

So finding 1 gives its Mac verdicts on hvx. Finding 2's regressions give their violation on hvx
too, so their `expect: violation` holds on both engines. But 7 ms after the event, hvx has not yet
reached the module's `fw_setenv upgrade_available 0`. So the cut leaves a commit that exits 0
for an update that U-Boot has already rolled back, the Mac's form of `@3:cut`. The rollback of
an update that U-Boot had made permanent needs a later point on hvx.

A scan of the cut's delay after `mender.commit_started` found that point, with one run per delay:

| delay (ms) | the result on hvx |
|---|---|
| 1 to 16, 18, 20, 25, 30, 40, 50, 60 to 63, 65 to 68 | the commit exits 0 after U-Boot rolled back, and Mender names v1 |
| 64, 69, 70, 73, 76, 79, 80, 83, 86, 87 | the commit exits 0 and undoes v2, which U-Boot had made permanent |
| 88, 89, 92, 95, 98, 100, 150, 200, 300, 500, 1,000 | OK, the device runs v2 |
| 2,000 and 5,000 | OK, the device runs v5: the commit's step ended before the cut was due, and the run went on |

On hvx the window of finding 2 is 0 to 87 ms of guest time, and the Mac's form is 69 to 87 ms
(and 64 ms alone). Why 64 ms gives the later form and 65 to 68 ms the earlier one was not
investigated. The regressions keep the Mac's delay of 7 ms; `+80` reaches the later form on hvx.

The table's values come from the first build of the x86_64 tools. A second build gave another
`mender-update` (section "Building the same bytes") and the image `c7d80b2a7df37018…`. With it,
one run each gave the table's verdicts for `disk=cut | run` (`06c8329b0209afa7`), `C=4`
(`f98d56872f1b5cf3`), `C=6` (`6e23979a26e77df2`), `+7:cut` (`86a3b37807b3f007`) and
`+7:kill-device` (`89b1a9247bd07ff8`), each the same on a second run. The later form of finding
2 moved: it came at 65, 70 and 75 ms, and 80 ms gave OK. `+80:kill-device` still gave it
(`1a6f57bae2757626`). A delay of 70 ms gives the later form with both builds.

## Building the same bytes

`sh build.sh` builds the tools in Docker, and then the images and Artifacts. Every file time in
the slot images, the data partition, the environment and the Artifacts is `SOURCE_DATE_EPOCH`
(2026-01-01). mke2fs gets fixed UUIDs and hash seeds, and the Artifacts' state scripts are sorted
(finding 3). Two builds on this Mac gave the same sha256 for every file of `out/aarch64/` and
`out/common/`.

On a Mac, Docker builds the x86_64 tools under QEMU's emulation of x86_64. There the compilers
end with a segmentation fault at a random source file now and then. Without a retry, no build
finished: `cc1plus` stopped the Mender build three times, `cc1` stopped libarchive's once, and
Go's compiler stopped mender-artifact's at once. So every `make` and `cc` of `build/Dockerfile`
runs through `build/retry.sh`, which runs the command again up to 10 times. The x86_64 image ends
at the stage `tools`, without mender-artifact, which runs only in the native image. The arm64
tools of the changed Dockerfile have the bytes of the arm64 tools before the change.

Two x86_64 builds still gave two `mender-update` programs, `e3bcd860…` and `ad400115…`. The
other programs were the same, but the slot images and the Artifacts carry `mender-update`, so
they differ too. The cause is libarchive's configuration. CMake's checks compile small programs,
and a check whose compiler crashes reports a missing feature. The first build's libarchive has no
`openat`, `mbrtowc` or `tzset`, and the second's no `wchar_t` and no size of `int`. A third build
of the Mender step on the second build's libarchive gave the second build's bytes. So two x86_64
builds on a Mac give the same bytes only by chance. A build on an x86_64 host with Docker was not
tried.

## What hydo lacked

These are open item 136 of `AGENTS.md`. Open items 125 and 128 already list the device gaps of
the RAUC and SWUpdate examples: `cf=` runs on hvx only and not with `guest.reboot`, hvx has no
added disks, and `G=` and `L=` do not reach an added disk.

1. **A power cut at a point inside a command.** Finding 2's window is about 1 ms of guest time
   after an event. A reaction's delay places a cut there, but the explorer draws the delay from
   0, 10, 50, 200, 1,000 and 5,000 ms only (`REACT_DELAYS` in `hydo-explore`). The example found
   the window with a sweep of the delay by hand, in steps of 1 ms. A sweep of one reaction's
   delay is not built.
2. **Reactions while a service starts.** Reactions act during workload steps only. A device
   commits or rolls back in its boot service, while hydo starts the services, so the example
   moved the commit into the operator's step. No reaction can cut the rollback after a failed
   health check.
3. **A restart after a reaction's kill.** `A=<event>:kill-<svc>` keeps the service down until
   the heal, whatever its `restart:` says. A device's service manager restarts a killed agent at
   once, and the operator retries its command. So in the example the retry after a kill comes
   only at the end of the workload, after the operator's reboot.
4. **A service's own log in an OK run.** The run directory keeps a service's output only for a
   failed run's diagnosis. The device writes its log lines to the console as well, so every
   boot's `boot<n>.log` has them.
5. **More loop devices on the Mac.** The Mac engine's guest kernel had no `/dev/loop4`
   (`losetup: No such device or address`), so the example uses `/dev/loop0` to `/dev/loop3` for
   the eMMC and its three partitions.

## Files

| file | what it is |
|---|---|
| `build.sh`, `build/` | the static tools (`build/Dockerfile`, `build/loopdio.c`, `build/retry.sh`), the slot images, the data partition, U-Boot's environment and the Artifacts (`build/make-images.sh`, `build/state-script.sh`, `build/retime.go`, `build/mender-artifact-script-order.patch`) |
| `fetch-modules.sh` | the hvx guest kernel's loop module (on Linux) |
| `hydo-devices.yaml` | the device with its eMMC as a disk of its own (`guest.disks`; the Mac and KVM engines) |
| `hydo.yaml` | the same device with its eMMC as a file on the data disk (hvx) |
| `device/` | the device's scripts: `lib.sh` (the eMMC, U-Boot, the boot, Mender's database, the operator's model), `boot.sh`, `agent.sh`, `client.sh`, `check.sh` |
| `regressions/` | the minimal schedules of findings 1 and 2 |
