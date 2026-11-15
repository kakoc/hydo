# SWUpdate under hydo, an A/B device update under power cuts

SWUpdate (github.com/sbabic/swupdate) installs software updates on embedded Linux devices. This
example puts a device with two root file system slots and SWUpdate's double-copy update under
hydo's power cuts and kills. The device is the RAUC example's (`../rauc/RESULTS.md`): the boot,
the update agent, the client and the checks are the same, and only the update tool and the
bootloader's variables differ.

## The short answers

- **What was found?** One SWUpdate candidate that no report names. SWUpdate's raw handler never
  flushes a written image to the storage, and SWUpdate then switches the bootloader to the new
  slot. After a power cut between a successful install and the reboot into it, the device boots
  what the slot held before. In the example that is v1 from the factory, older than the v2 that
  ran before the update; the device accepts it as good.
- **When does it happen?** When the bootloader's environment and the slots are on different
  storage, each with its own volatile write cache, so that the environment's flush does not
  flush the slot. With both on one eMMC, Linux writes the slot's data to the eMMC when SWUpdate
  closes the device, the environment's flush makes it durable, and the same schedules pass.
- **And RAUC?** RAUC flushes the slot (`fsync`) before it switches the bootloader, and the same
  schedules pass on the RAUC example.
- **What did hydo lack?** The section "What hydo lacked" of `../rauc/RESULTS.md` covers both
  examples.

## The version

The example runs SWUpdate 2026.05.1, the newest tag on 2026-11-15. `build/Dockerfile` builds it
from its source archive (sha256 pinned) as one static program, with `build/swupdate.config`. That
program has the raw handler, the bootloader handler, sha256 checks of every image, the libconfig
parser, and U-Boot's environment through libubootenv 0.3.8. It has no network, web server, Lua or signing: the device installs a
local file with `swupdate -i`. Signing checks the image before the install and changes nothing on
the device's storage. The code of the finding is the same on SWUpdate's `master` of 2026-11-15,
whose last change of `handlers/raw_handler.c` is `76876a3b` (2026-10-18).

## The device

The device is the one of `../rauc/RESULTS.md`, section "The device", with these differences:

- **The bootloader's state** is U-Boot's usual one for SWUpdate's double copy: `boot_slot` (A or
  B), and U-Boot's boot counter `upgrade_available`, `bootcount` and `bootlimit` (3). While
  `upgrade_available=1`, each boot counts `bootcount` up and saves the environment. When
  `bootcount` exceeds `bootlimit`, U-Boot runs `altbootcmd`, which switches `boot_slot` back and
  clears the counter (`device/lib.sh`, `uboot_choose`).
- **An update** is `swupdate -i v<N>.swu -e stable,copy1` when slot B is booted, and
  `-e stable,copy2` when slot A is booted. Each SWUpdate image (`build/make-swu.sh`) has both
  copies. `copy1` writes the OS image to `/dev/loop0` (slot A) with the raw handler and sets
  `boot_slot=A`, `upgrade_available=1` and `bootcount=0`; `copy2` does the same for slot B.
  SWUpdate also sets its own markers. It sets `recovery_status=in_progress` before the install
  and removes it after the install, and it sets `ustate=1` after the install.
- **The confirmation** after a healthy boot writes `upgrade_available=0` and `bootcount=0` in one
  environment write, as SWUpdate's documentation asks of the application.
- **There is no partition of the update tool.** SWUpdate keeps its state in the bootloader's
  environment only, so the example has no `storage:` and no `G=` or `L=` faults.

The checks are the RAUC example's, without `rauc.status_matches_slots`, and with
`device.primary_complete` reading `boot_slot`.

## The storage model

The slots are loop devices over files on the guest's data disk, as in the RAUC example. The build
parameter `dio` chooses how a write to a slot reaches the disk:

| model | build | what it stands for |
|---|---|---|
| separate storage | `hydo build hydo.yaml` (`dio=0`, the default) | The slots' storage has a volatile write cache of its own. A flush of the environment's storage does not reach it. Examples: the environment on SPI NOR flash or a second device, and the root file systems on an eMMC, an SD card, or an SSD. |
| one eMMC | `hydo build hydo.yaml --set dio=1` | The slots and the environment are on one eMMC. The loop devices use direct I/O (`loopdio`). So the data that Linux writes back when SWUpdate closes the device reaches the guest's disk, and the environment's flush makes it durable. |

hydo has one disk per guest. With `disk=cut` that disk keeps a volatile write cache, and a flush
(`fsync`) anywhere on it makes every earlier write durable. The default model gets a second cache
from the loop device's backing file. Its pages stay in Linux's page cache until a flush of the
slot, as data stays in a second device's cache until a flush of that device. This model loses
more data than such a device, because a device's cache is smaller than the page cache. It loses
the data in the same way.

## Without faults

`disk=cut | run` runs 8 updates and 15 boots and holds every invariant. On hvx it gives
`HYDO-ORACLE: OK invariants=5 held (device.boots: booted slot A v5 after 15 boot(s))` with the
fingerprint `9a6b98defa5b7a98` on two runs.

The fingerprints in this file are of the image built with `workload.timeout: 60`. Milestone 261
removed that line, so the image's bytes and its fingerprints changed. On hvx the regression
`disk=cut | run C=4` gives the same verdict with `24da8c23993cc677` on two runs.

## What was found

### 1. After a power cut before the reboot into an installed update, the device boots the slot's old contents (an SWUpdate candidate, new as far as the search found)

**The schedule.** `disk=cut | run C=4`: the device installs v2 into slot B; in step 2 it reboots
into v2 and starts the install of v3 into slot A. `swupdate` exits 0 and the client records the
install as acknowledged (outcome `i1 ack`) in step 3. The power is cut before step 4, whose
first act is the reboot into v3. On hvx it gives `VIOLATION [device.version] booted v1, but the
updates allow only v3` with the fingerprint `46e6fc00490daefe` on two runs. The schedule without the cut
passes.

**What the device does.** After the cut the environment says `boot_slot=A`,
`upgrade_available=1`: SWUpdate's switch reached the disk. Slot A still holds v1, written there by
the factory, because none of v3's data reached the disk. The verified boot accepts v1, a complete
and genuine image, and v1 is healthy, so the boot counter does not roll back and the application
confirms it (`upgrade_available=0`). The device ends on v1: older than the v2 that ran before the
update, and not the v3 that SWUpdate reported installed. `ustate=1` still says that an update was
installed.

**The same after a broken release.** `disk=cut | run C=6` cuts after the acknowledged install of
v4 into slot B, the broken release. The device boots slot B's old contents, v2, a healthy image
older than the running v3, and keeps it: `VIOLATION [device.version] booted v2, but the updates
allow only v3` (`eda2ae22054d409b`). With the image's data on the disk, v4 fails its health check
three times and the boot counter returns to v3.

**Without the disk's cache.** `disk=none | run C=4` fails the same way (`21861044c9d8b676`): the
disk then keeps every write it took, and v3's data never left the slot's page cache.

**The one-eMMC model.** With `--set dio=1` the same schedules pass, and the device boots v3:
`disk=cut | run C=4` (`5c8d3b910414aaea`), `disk=cut | run C=6` (`5c3983ee3bb9008a`, after v4's
rollback), and `disk=none | run C=4` (`68b415deeed4326c`). There, Linux writes the slot's pages
to the disk when `swupdate` closes the device, and the environment's `fsync` flushes the disk's
cache.

**RAUC.** The RAUC example passes both schedules in the separate-storage model: `disk=cut | run
C=4` boots v3 (`22a61a9d33c3c8e8`), and `disk=cut | run C=6` boots v4 three times and rolls back
to v3 (`209549cca9b1e75b`).

**The cause in SWUpdate's code.** `install_raw_image` in `handlers/raw_handler.c` writes the
image with `copyimage` and closes the device. It calls `fsync` only when it had switched off the
write protection of an eMMC boot partition (`prot_stat == 1`), with the comment "data are not
automatically flushed before ro mode is enabled". For every other device it closes the file
without a flush. `install_images` in `core/installer.c` then runs `update_bootloader_env`, which
stores the environment through libubootenv with an `fsync` of the environment's device. Nothing
in SWUpdate's `core/`, `corelib/` or `handlers/` calls `sync`, `syncfs` or `fsync` for an image
written to a device; the same handler's file installer (`install_raw_file`) does call `fsync`.
Linux's last close of a block device writes the device's dirty pages to the device and sends no
cache flush, so the image can still be in the storage's volatile cache when the switch is
durable.

**What SWUpdate promises.** `doc/source/overview.rst`, "Double copy with fall-back": "it is
possible to guarantee that there is always a working copy even if the software update is
interrupted or a power off occurs", and "an update is always safe even in case of power off". For
the double copy, its section "Power Failure" says that after a power off "the same version of
software, that was not touched by the update, is started again". The documentation names no
requirement that the environment and the slots share one storage device, and no setting that
adds the flush. `hydo rules scan` listed these sentences as the documentation's evidence.

**The search.** On 2026-11-15:

- GitHub's search of sbabic/swupdate's issues and pull requests (the repository has issues
  switched off) for `fsync`, `sync`, `flush`, `power cut`, `power loss`, `power-off`,
  `raw handler`, `durable`, `bootloader environment` and `corrupted` found nothing.
- A web search of the mailing list (groups.google.com/g/swupdate) and its patchwork
  (patchwork.ozlabs.org/project/swupdate) for `raw_handler fsync`, `install_raw_image`,
  `power cut corrupted rootfs` and `data not flushed before bootloader environment` found two
  related threads and no report:
  - "[PATCH] raw_handler: Add atomic-install support" (2022) adds `atomic-install` and an
    `fsync` to the file installer, not to images written to a device.
  - "Swupdate issue with IDE drives" (2021) reports a root file system found corrupted when it
    was mounted right after SWUpdate wrote it. The maintainer answered that "the kernel should
    ensure that all data is flushed when umount is called" and suspected data still in the
    kernel's buffers or the IDE driver; no change followed.

**Without hydo.** Not reproduced: the cause is plain in `install_raw_image`, a few lines.

**The regression.** `regressions/stale-slot-after-switch.schedule`.

## The explorations

Both explorations ran on hvx with one worker, on images with `workload.timeout: 300`. A later
image had 60 s (`../rauc/RESULTS.md`, "What hydo lacked", item 4). The current one has the
default 300 s again (Milestone 261). The fingerprints of other images differ:

```sh
hydo explore --seeds 1-60 --strategy process --more-faults --reactive fault \
  --react-events ota.install_started,ota.reboot_started,ota.mark_good_started
```

- **The client of this example**, which reboots into an update at the start of the next one:
  60 schedules, 8 failures, each reproduced with its fingerprint. Seven are finding 1 in five
  signatures. The shrinker kept a power cut, alone or with a kill. The minimal schedules are
  `disk=cut dseed=4127cc1347fae6ba | run C=16` (seed 1, `f9d27fc70f7c72d0`, the device boots v3
  instead of the acknowledged v5), `K=device@6+1 C=16` (seed 32), `K=device@14+1 C=16` (seed
  21), `K=device@5+2 T=2:-3600 A=ota.reboot_started@5:cut` (seed 15), and `K=device@0+5
  A=ota.install_started+5000:cut` (seed 38). 231 runs per hour.
- The eighth failure is hydo's, not SWUpdate's: `disk=cut dseed=ea6b44d03ce268f0 | run
  K=device@4+3 T=1:-30 A=ota.install_started+5000:cut` (seed 41) ends with `ERROR agent
  protocol: the agent refused a line it could not read: bad-json text after the value at byte
  6`, also in its rerun. The host put a line while the agent was inside its read of the line
  before, and the agent's last 4-byte read took the new line's `{`. Milestone 261 fixed it in
  the hvx engine: on the image without `workload.timeout: 60` the schedule gives `OK
  invariants=5 held` with `cacaaf979daca400` on two runs.
- **A first client**, which rebooted right after each install: 25 schedules, no failure. Its
  window between the switch and the reboot's `sync` was a few milliseconds, and no drawn power
  cut fell into it. A device that installs in the background and reboots later has a window as
  long as that wait.

## Files

| file | what it is |
|---|---|
| `build.sh`, `build/` | the static tools (`build/Dockerfile`, `build/swupdate.config`, `build/loopdio.c`), the OS images and the SWUpdate images (`build/make-swu.sh`) |
| `fetch-modules.sh` | the hvx guest kernel's loop module (on Linux) |
| `hydo.yaml` | the device: one service, the workload, the checks |
| `device/` | the device's scripts: `lib.sh` (the slots, U-Boot with its boot counter, the boot, the client's model), `boot.sh`, `agent.sh`, `client.sh`, `check.sh` |
| `regressions/` | the minimal schedule of finding 1 |
