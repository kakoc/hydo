# RAUC under hydo, an A/B device update under power cuts

RAUC (github.com/rauc/rauc) installs software updates on embedded Linux devices: it writes a
signed bundle into the slot that is not running and switches the bootloader to it. This example
tests whether hydo finds bugs in such update tools. The device has two root file system slots,
U-Boot's environment, and a data partition. hydo cuts its power, kills its update agent, and
corrupts its storage during installs and reboots. The SWUpdate example
(`../swupdate/RESULTS.md`) runs the same device with SWUpdate.

## The short answers

- **What was found in RAUC?** Nothing. The explorations ran 80 schedules on the Mac engine and
  58 on hvx, and directed power cuts hit installs, reboots, and the marking of a slot as good.
  The device always booted a complete slot of a version the updates allow, and RAUC's records
  matched the slots. RAUC flushes a slot before it switches the bootloader, writes its status
  file atomically, and keeps a `pending` mark in it during an install.
- **What was found with it?** The same device found an SWUpdate candidate (`../swupdate/`). After
  a power cut between a successful install and the reboot into it, the device boots the slot's
  old contents. On RAUC the same schedules pass.
- **A known bug the scenario touches.** libubootenv 0.3.8, which RAUC calls to write U-Boot's
  environment (`fw_setenv`), ignores the result of its `fsync`. It is fixed on libubootenv's
  `master` (`ed3fce96`, "uboot_env: report fsync errors when writing to a file", 2026-11-13), in
  no release yet. The explorations drew no disk errors, so no run showed it.
- **What did hydo lack?** Section "What hydo lacked": a way to reboot the guest, a second disk,
  a device's kernel command line, and more.

## The version

RAUC 1.15.2 (2026-03-27), the newest release on 2026-11-15, from its release archive; libubootenv
0.3.8 (2026-10-16) for `fw_printenv` and `fw_setenv`. `build/Dockerfile` builds both as static
programs in Alpine 3.22 (pinned by digest), for arm64 (the Mac engine) and x86_64 (hvx), with the
sources pinned by sha256. RAUC is built without its D-Bus service, network and streaming:
`rauc install` runs in its own process, as on a device without D-Bus. One patch,
`build/rauc-unsquashfs.patch`, lets RAUC unpack a bundle with `unsquashfs` when the kernel has no
squashfs, and only when `RAUC_UNSQUASHFS_FALLBACK` is set: the Mac engine's guest kernel has no
squashfs. On hvx, whose guest kernel loads `squashfs.ko`, RAUC mounts its bundles as it does on a
device. The bundles use RAUC's `plain` format, because the Mac's guest kernel has no device
mapper for `verity`.

## The device

| part | what it is |
|---|---|
| slots A and B | two 16 MiB files on the guest's data disk, attached as `/dev/loop0` and `/dev/loop1`; RAUC's slots `rootfs.0` and `rootfs.1` (type `ext4`, bootnames A and B) |
| the OS images | five 12 MiB ext4 images, v1 to v5, each with 6 MiB of firmware data made from its version (`build/make-bundles.sh`); v4 is a broken release whose health check fails |
| the bundles | v2 to v5, signed with the example's own key; the device trusts its CA |
| the bootloader's state | U-Boot's redundant environment, two 16 KiB copies in `/data/flash/uboot.env`, read and written with libubootenv; RAUC's `uboot` backend keeps `BOOT_ORDER` and `BOOT_<slot>_LEFT` in it |
| the data partition | `/data/part`, a file system of its own (`services[].storage`): RAUC's `data-directory`, with its status file `central.raucs`; the target of `G=` and `L=` |
| the application's data | `/data/app/records`, one synced line per update |

**The boot** (`device/lib.sh`, `cold_boot`) is what U-Boot and the kernel do, in the guest:

1. The factory, at the very first boot, writes v1 into both slots and U-Boot's first
   environment: `BOOT_ORDER=A B`, three attempts each.
2. U-Boot's boot script from RAUC's documentation takes the first slot of `BOOT_ORDER` with
   attempts left, counts one attempt down, and saves the environment. With no such slot it sets
   both counters to 3 and resets.
3. The kernel's verified boot is a hash of the whole slot, as dm-verity refuses a root file
   system with a changed block. A slot whose hash is no OS image's panics, and the watchdog
   resets the device into U-Boot again. Twelve resets in a row without a slot make the device
   bricked.
4. RAUC finds the booted slot in the kernel's command line (`rauc.slot=A`). hydo's guest kernel
   line cannot carry it, so the boot binds a file with the guest's line and `rauc.slot=<slot>`
   over `/proc/cmdline`.

A cold boot happens at the first boot, in hydo's recovery boot after a power cut, and when the
update agent reboots the device. A reboot is clean: `sync`, then the boot, in the running
guest. A kill of the service (`K=`) is a crash of the update agent, which hydo starts again
without a boot.

**The update agent** (`device/agent.sh`, the service `device`) checks the health of the booted
system after a boot and runs `rauc status mark-good`, as RAUC's `rauc-mark-good.service` does. A
system that fails its health check reboots without marking it, so the boot counter rolls the
device back. Then it serves the client's requests one at a time: `rauc install <bundle>` and
reboots.

**The client** (`device/client.sh`, the workload) runs 8 updates in 16 steps:

- Step 2k reboots the device into update k-1, waits until the booted slot is marked good, and
  checks the booted version. Then it asks for the install of bundle v2, v3, v4 or v5, and
  returns after a delay of 0 to 1.75 s.
- Step 2k+1 waits for the install's answer and appends a synced record to the application's
  data.

So a fault before step 2k+1 meets the install, and a fault before step 2k+2 meets an installed
update that has not booted yet: a device installs in the background and reboots later. Each
install, reboot and record is an id in `$HYDO_OUTCOMES`. The agent writes a console record when an install, a reboot or a mark-good
starts (`ota.install_started`, `ota.reboot_started`, `ota.mark_good_started`), so that a reaction
cuts the power at a chosen time after it: `A=ota.install_started@4+35:cut` cuts 35 ms after the
install that starts in step 4.

## The storage model, and what it can and cannot reach

The data disk is hydo's virtio block device. With `disk=cut` it keeps a volatile write cache, as
eMMC, SD cards, and SSDs do: a power cut loses every write that no flush (`fsync`) made durable.
The slots are loop devices over files on that disk. A slot's write is durable after a flush of
the slot, and a flush of the environment does not flush the slot. That is a device whose slots
and environment have caches of their own. The SWUpdate example's `dio=1` adds the model of one
eMMC for both.

The model reaches:

- power cuts at any point of an install, a reboot, a mark-good or a boot counter's write;
- a torn write of one environment copy, the redundant copy, and the order of the flushes;
- a crash of the update tool, and a power loss of the data partition alone (`L=`);
- flipped, zeroed or cut bytes in RAUC's status file (`G=`);
- disk I/O errors and a disk that drops acknowledged writes (`disk=ioerr`, `disk=lying`).

It cannot reach:

- raw NAND or NOR flash: MTD, UBI and UBIFS, erase blocks, bad blocks, the environment in NOR;
- the device's own kernel and bootloader: the boot is a script, and U-Boot's own `saveenv` code
  is not run (libubootenv writes the same format);
- eMMC specifics: boot partitions, write protection, the eMMC's own wear levelling and its
  behaviour at a power loss inside one write;
- bundle formats that need the device mapper (`verity`, `crypt`) on the Mac, and streaming
  installs. On hvx the device mapper's modules exist; the example did not use them.

## The checks

The promises come from RAUC's documentation. `hydo rules scan` listed them as its "docs"
evidence:

- `docs/updating.rst:29`: "A failure occurring during this installation must not brick your
  device".
- `docs/updating.rst:32`: "the operation that switches the boot device must be atomic".

The scan found no schema or state of RAUC's own to propose rules from. RAUC keeps its state in a
key file and in the bootloader.

| invariant | the promise |
|---|---|
| `device.boots` | after any power cut, the device boots a complete, verified slot (not bricked) |
| `device.version` | the booted version is one the updates allow: an acknowledged install of a good version is booted, a broken one rolls back, an install that did not finish never boots (`device/lib.sh`, `allowed_versions`) |
| `device.primary_complete` | the slot U-Boot boots first holds a complete image |
| `rauc.status_matches_slots` | a slot that RAUC's status file calls `ok` holds the image with the checksum the file records |
| `data.records_survive` | every synced record of the application is there |
| `device.reboots_boot_allowed` | every reboot during the workload booted an allowed version (checked every 2 s) |

## Without faults

`disk=cut | run` runs 8 updates and 15 boots. The device boots v2 and v3; then v4 boots three
times, fails its health check each time, and rolls back to v3; then v5, v2, v3, v4 (rolled back),
and v5. On hvx it holds every invariant: `HYDO-ORACLE: OK invariants=6 held (device.boots: booted
slot A v5 after 15 boot(s))`, the fingerprint `da3f619faa6eb4c4` on two runs. The Mac engine
gave the same verdict with the first client (a reboot right after each install), after 16 boots
(`e2badaf98f96dfc1`). An earlier build of that client gave one fingerprint on two Mac runs
(`bba27579504084b7`).

The hvx fingerprints in this file are of the image built with `workload.timeout: 60`. Milestone
261 removed that line, so the image's bytes and its fingerprints changed. On hvx the image
without it gives that verdict with `bafed17412f8b5a9`, and the regression `disk=cut | run
C=4` boots v3 with `f87883f5a12c011f`.

## What was found

No RAUC bug. What the faults showed:

- **Power cuts during an install.** On the Mac, where an install takes about 50 ms of guest time,
  `A=ota.install_started@<step>+<ms>:cut` cut the second and the third install at 5 ms to 60 ms
  in 5 ms steps. Before RAUC's last write the device boots the old slot; after it, the new one.
  After the cut 35 ms into the third install, RAUC's status file says `status=pending` with no
  checksum for the slot it was writing, and the environment has `BOOT_ORDER=A` and
  `BOOT_B_LEFT=0`.
- **Power cuts after an install, before the reboot.** `disk=cut | run C=4` and `disk=cut | run
  C=6`, the SWUpdate example's failing schedules, boot v3 on hvx (`22a61a9d33c3c8e8`, and
  `209549cca9b1e75b` after v4's rollback). RAUC flushes the slot (`fsync` in `copy_raw_image`,
  `src/update_handler.c`) before it calls `fw_setenv`.
  `disk=none | run C=4` boots v3 as well (`3633860fae33dab7`).
- **Kills of the agent during installs** and **power losses of the data partition** (`L=`): no
  check failed in the explorations. RAUC writes its status with GLib's
  `g_key_file_save_to_file`, which writes a new file, flushes it, and renames it. The strict
  power loss, which loses every write the partition's file system had not synced, held too:
  `disk=cut | run L=device@4+1:strict` (`787429d465e23b9e`), `L=device@6+2:strict`
  (`58a0b7276df3fef5`), and `L=device@9+1:strict C=12` (`6cf847513b9ed0b3`).
- **Disk faults** break the device model itself, so their failures say nothing about RAUC.
  `disk=lying:3 dseed=7 | run C=16` makes the data partition unmountable after the cut (`Data
  consistency error`). In `disk=ioerr:2 dseed=7 | run` RAUC reports every install as failed, so
  every install writes slot B and the device runs slot A. From the sixth reboot on, the verified
  boot cannot read slot A's v1, which nothing wrote, and the device ends bricked.
- **Corruption of the status file** (`G=`). RAUC's code moves a status file that it cannot parse
  to `central.raucs.broken` and writes a new one (`src/status_file.c`). One schedule of the Mac
  exploration flipped bits inside a checksum of the file, which still parses:
  `disk=none dseed=d5d1af48fa55e199 | run G=device@11:flip,n=32,seed=1cf26ef3 L=device@9+3`.
  The status then names a checksum that no image has, and `rauc.status_matches_slots` fails.
  That is corrupted storage, not a RAUC bug. RAUC's status file has no checksum of its own, and
  a wrong checksum there only makes RAUC write the slot again.

## The explorations

| engine | schedules | options | failures |
|---|---|---|---|
| Mac, the first client | 80 | `--strategy process --storage-faults --new-faults --reactive fault --react-events ota.install_started,ota.reboot_started,ota.mark_good_started` | 1: the corrupted checksum above |
| hvx, the final client | 58 (stopped at seed 59 of 80) | `--strategy process --more-faults --storage-faults --reactive fault`, the same events | 2, both hydo's: a `cf=` cut that hangs the rest of the run (`cf=83` with `L=device@6+2`, seed 37; with `G=`, seed 58), `TIMEOUT workload` |

The Mac exploration drew 20 power cuts at a step (`C=`), 52 reactions, 58 kills, 30 power losses
of the data partition, 36 corruptions and 57 clock steps, at 524 runs per hour. The hvx one drew
8 cuts at a disk flush (`cf=`), 12 power cuts at a step, 33 reactions, 41 kills, 22 power losses
of the data partition, 28 corruptions and 43 clock steps. Neither drew disk errors or
`L=...:strict`, which the explorer never draws.

## Real reboots (Milestone 263)

The agent's reboots are real ones since Milestone 263 (`guest.reboot: true`). The client asks for
the reboot into update k-1 in step 2k; the reboot ends that step, and step 2k+1 checks the booted
version in the new boot before it installs update k. So `C=4` still cuts after the acknowledged
install of v3 and before the reboot into it. A failed health check reboots the machine too, three
times for the broken v4, before U-Boot's boot counter returns to v3. The fingerprints of Milestone
259 above came from the scripted reboot and do not repeat with this client.

| run | engine | result |
|---|---|---|
| `disk=cut \| run` | the Mac | OK, booted v5 after 14 reboots, `1b886a1aa84b9fdc` on 2 runs |
| `disk=cut \| run C=4` | the Mac | OK, booted v3, `e8a91854d76263b7` |
| `disk=cut \| run C=6` | the Mac | OK: the recovery boot boots v4 three times and rolls back to v3, `61fc4401e4e90686` on 2 runs |

On hvx the same schedules give `8db65afe7545c3b2` (OK, 2 runs), `c66b70a83653b127` (OK, booted
v3) and `23d1282db24e337b` (OK, v4 three times and back to v3, 2 runs). RAUC held with the real
reboots, as it did with the scripted one.

## What hydo lacked

hydo has no device model. Each item says what the example did instead, and what a device team
loses until hydo has it.

1. **A reboot of the guest** (built in Milestone 263). Until then hydo's only reboot was the
   recovery boot after a power cut, and the example's reboot was a script in the running guest:
   Linux's page cache, the loop devices and the kernel stayed. Since Milestone 263 the agent
   asks for a real reboot (`guest.reboot: true`, the console line `HYDO-REBOOT`). hydo shuts the
   guest down cleanly and starts a new boot of the machine with the disk's bytes (section "Real
   reboots" below, docs/design/devices.md).
2. **A second disk, or a disk with partitions of its own** (built in Milestone 263 for the Mac
   and KVM engines). A guest had one disk, and a flush anywhere on it made every earlier write
   durable. `guest.disks` adds disks with caches of their own. The SWUpdate example's
   `hydo-devices.yaml` puts the slots on an eMMC and the environment on SPI NOR flash. This
   example keeps its loop devices over files. The hvx engine has one block device, in its kernel
   module, and refuses added disks (AGENTS.md open item 128).
3. **The device's kernel command line.** RAUC and other tools read the booted slot from
   `/proc/cmdline`; the example binds a file over it.
4. **A power cut inside a step on the Mac.** `cf=` (a cut at the k-th flush) runs on hvx only.
   On the Mac the example needed its own console records and reactions (`A=...:cut`) to cut
   inside an install. Until Milestone 261 a `cf=` cut also left the rest of the run hanging.
   A process that touched the dead disk waited forever, such as the workload's client or the
   agent's own mount of an `L=`. Such a run ended at the explorer's limit of 900 s
   (`TIMEOUT workload`). The examples used `workload.timeout: 60`. Since Milestone 261 the run
   is cut at the first stop after the flush. In `disk=cut dseed=b867e156d88e8c87 cf=83 | run
   T=12:-300 L=device@6+2` the flush is the mount of the `L=`. Until Milestone 271 the agent
   made that mount itself, waited on the dead disk, and the run ended in `ERROR the disk lost
   power at its flush of cf= while the agent itself waited on it`. Since Milestone 271 busybox
   mounts in a child process in a `cf=` run. The schedule reaches its recovery boot and gives
   `OK invariants=6 held` (`0cad7c03d59862fa` on two hvx runs, on Milestone 261's final image).
5. **Kernel features of devices.** The Mac's guest kernel has no squashfs and no device mapper,
   so RAUC needed a patch there and no engine ran `verity` bundles. The hvx guest kernel has
   them as modules, which the example loads from the kernel's build (`fetch-modules.sh`).
6. **A way to keep faults off one fault target.** `G=` picks a file of the service's storage,
   so the application's records had to move off the data partition to keep the corruption on
   RAUC's own files.
7. **Rules for system software.** `hydo rules scan` reads schemas, state columns and APIs;
   RAUC and SWUpdate have none. Its documentation evidence named the promises, and the checks
   were written by hand.
8. **The explorer's disk faults.** `--strategy process` drew no `disk=ioerr` or `disk=lying` in
   80 schedules, and no explorer option draws the strict power loss.

## Files

| file | what it is |
|---|---|
| `build.sh`, `build/` | the static tools (`build/Dockerfile`, `build/rauc-unsquashfs.patch`), the keys, the OS images and the bundles (`build/make-bundles.sh`) |
| `fetch-modules.sh` | the hvx guest kernel's loop and squashfs modules (on Linux) |
| `hydo.yaml` | the device: one service, the workload, the checks |
| `device/` | the device's scripts: `lib.sh` (the slots, U-Boot, the boot, the client's model), `boot.sh`, `agent.sh`, `client.sh`, `check.sh` |
| `regressions/` | the SWUpdate finding's schedule, which passes on RAUC |
