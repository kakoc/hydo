# virtio-mmio disk and network for `hvm`, with hvx's fault injection

**Result:** the deterministic Hypervisor.framework VMM (`hvm.c`, `BOOT.md`) now has a virtio-mmio block device and a virtio-mmio network device. Real arm64 Linux 6.6.35 mounts an ext4 disk on `/dev/vda`, writes and fsyncs records, and reads them back. It also brings up `eth0` and answers scripted ARP requests. Every run is bit-identical, including:
- replay;
- branch-from-snapshot during the disk workload;
- runs with all 12 host cores saturated.

The fingerprint now covers the device state, the whole disk image, the disk's volatile write cache, and the captured outbound frames. The disk fault engine is hvx's (CLAUDE.md M22/M26), with the same seed-to-schedule mapping. A seeded lying disk loses an fsync'd record reproducibly, and a different seed loses a different one. The flush-aware fault never loses an fsync'd record. A two-boot power cut keeps only the bytes a FLUSH made durable.

With no device attached, nothing changes: the M48 boot still gives `81711ada76a4a236` with this `hvm`.

**Since Milestone 147 the C files of this page are deleted.** The devices are the Rust port in `hydo/crates/hydo-hvm/src/virtio.rs` (M143, byte for byte the same state, hashes and fault decisions; its module documentation keeps the fault model's notes), and `hvm` is the Rust program's boot probe, `hydo-hvm hvm run|print|branch`. The C is in git (`git show 145c456:hv/hvm/virtio_blk.c` and so on). The table below names the files as they were.

## Files

| File | What |
|---|---|
| `virtio.h` (deleted in M147) | the interface `hvm.c` uses, and the environment variables |
| `virtio.c` (deleted in M147) | virtio-mmio version 2 transport, setup from the environment, completion timing, snapshot, fingerprint, output files |
| `virtio_blk.c` (deleted in M147) | virtio-blk: request servicing, FLUSH, the hvx fault classes, the volatile write cache |
| `virtio_net.c` (deleted in M147) | virtio-net: paced scripted RX, captured TX, per-packet drop/corrupt/duplicate |
| `virtio_int.h` (deleted in M147) | state shared by the three files above |
| `cut-test.sh` (deleted in M147) | the two-boot power cut: workload boot, durable image, recovery boot (the two commands are under "How to run") |
| `linux/hvm-virtio.config` | kernel options added for `Image-virtio`: virtio-mmio, virtio-blk, virtio-net, ext4 |
| `linux/recs.c`, `linux/init-disk` | the durability workload and checker, and its `/init` (phases `check`, `cut`, `recover`) |
| `linux/init-net`, `linux/mkarp.py` | `/init` bringing up `eth0` (10.0.0.2/24), and the scripted ARP requests |
| `linux/build-guest.sh` | now also builds `Image-virtio`, `initramfs-disk.cpio.gz`, `initramfs-net.cpio.gz`, `disk-ext4.img` (32 MiB, empty ext4) and `net-rx.bin` |

The plain `Image` and the two M48 initramfs images are built exactly as before. Their hashes are unchanged (`301883d2…`, `cb1fdaa0…`, `fd5032c3…`), so the M48 baselines still hold. The virtio kernel is a separate `Image-virtio`.

Clean rebuild in a fresh directory: byte-identical. `Image-virtio` `837ea04c2a2dc779…`, `initramfs-disk.cpio.gz` `0e6e09b7a35803f9…`, `initramfs-net.cpio.gz` `4f5ada7cd85b5755…`, `disk-ext4.img` `65ac658dbc17a1ec…`, `net-rx.bin` `04f5262a1dcbcaf5…`. The ext4 image uses `E2FSPROGS_FAKE_TIME`, which this e2fsprogs 1.47.0 honours: every superblock and inode time is 2020-09-13 16:26:40, and two runs seconds apart are identical. The pinned `mke2fs` is the one from `build-linux.sh`.

## How it works

- **Transport.** Each device is a virtio-mmio version 2 node in the device tree: device `i` at `0x0a000000 + i·0x200`, SPI `16 + i` (INTID 48 + i), level-triggered, `dma-coherent`. Feature negotiation is minimal:
  - VERSION_1 for both devices;
  - for the disk, FLUSH only when the flush-aware fault class is armed, as in hvx;
  - for the network, MAC only. No STATUS (so Linux starts with the link up, the M23 lesson), no offloads, no control queue.
- **Deterministic completion.** A QueueNotify write services the whole queue at once, from guest RAM. The completion interrupt becomes visible when the virtual clock reaches the kick time plus 16 exits' worth of ticks (hvx's latency), plus any seeded DELAY. That is a pure function of the virtual clock, the M16 discipline.
  - An idle guest's WFI now jumps the clock to the earlier of the timer deadline and the next device event.
  - The device's interrupt line is level (InterruptStatus ≠ 0) and goes through the existing GIC model. So the existing rule applies: the IRQ line is raised only at an entry where the guest has interrupts unmasked.
- **Disk.** The image file is loaded into memory. `HVM_DISK_OUT` writes the stable-media bytes at the end, without the volatile write cache, which is what survives a power cut.
- **Faults.** `HVM_FAULT=<seed>`, `HVM_FAULT_CLASSES` (`delay,error,drop,reorder,torn,unflushed`, or hvx's bit number), `HVM_FAULT_PROB`, `HVM_FAULT_MAXDELAY`. The decision is the same as hvx's `vblk_fault_decide`:
  - every request's roll is `splitmix64(seed + (index+1)·GOLDEN)`;
  - a fault applies when `roll % 100 < prob`;
  - the class is picked among enabled, applicable classes in the order DELAY, ERROR, DROP, TORN, DROP_UNFLUSHED by `(roll >> 8) % n`;
  - REORDER uses hvx's per-kick roll.

  So a seed means the same schedule on both. The units differ: DELAY counts exits here, instructions on hvx.
- **Two deliberate differences from hvx, both more faithful:**
  - the volatile cache has no size cap (hvx holds at most 256 writes of up to 8 KiB and persists anything larger);
  - request types other than IN/OUT/FLUSH get UNSUPP.
- **Network.** Frames in `HVM_NET_RX` (`[u16 big-endian length][frame]`…) are delivered paced, one per `HVM_NET_RX_PACE` virtual ticks (default 0.2 s), into a receive buffer the driver has posted. Delivering at the receive-queue kick would land them before the interface is up, the M23 wall. Outbound frames are captured in the same format to `HVM_NET_TX_OUT`.
  - Optional per-packet faults: `HVM_NET_FAULT=<seed>` with `HVM_NET_FAULT_CLASSES` drop, corrupt, dup and `HVM_NET_FAULT_PROB`. These use hvx M24's rolls; RX DELAY and REORDER are not ported.
  - Most stacks only need the guest's loopback, which needs no device.
- **Snapshot.** `virtio_save` and `virtio_restore` copy the device state, the disk image, the volatile cache and the TX capture. The `branch` mode uses them.

## Proofs (M2 Pro, macOS 26.4.1; 256 MiB guest, `HVM_STEP=64`)

Disk workload: phase `check` writes 16 records of 4 KiB with an fsync after each, unmounts, mounts again so the checker reads the device, and verifies.

| Check | Result |
|---|---|
| Plain M48 boot with this `hvm`, no device | `81711ada76a4a236`, unchanged |
| Disk `check`, 3 fresh boots | all `952991033b38776d`, `HYDO-ORACLE: OK acked=16 durable` |
| Same, 3 boots with 12 host cores saturated | all `952991033b38776d` |
| Branch at `hydo-disk: mounted` and at `hydo-rec: acked 7` (mid-workload); restore into a new vCPU | continue = restore = fresh boot, IDENTICAL |
| Lying disk `drop` @10%, seed `0x5017E0`, twice | `VIOLATION record=12`, records present `1111111111110110`, both `8afdcfbe731e3dd9` (re-measured 2026-10-18 with the branch's own `hvm` cce2b19 and main's; the earlier `51b40058b1665219` was recorded before the initramfs changed for the `cut` phase) |
| Same, seed `0x5017E1`, twice | `VIOLATION record=2`, present `1101110111111111`, `bb138c4d2d7e36bc` (re-measured; earlier `57c54ae6c36aa6ef`, same reason) (differs from the first seed) |
| Same, seed `0x5017E3` | `VIOLATION the record file is missing` (directory metadata dropped), twice `b0cc2184d02eb57b` |
| `delay`, `reorder`, `torn`, `error`, `delay,reorder,drop,torn` @30%, twice each | identical pairs; delay and reorder OK, torn and the mix lose data, error makes the disk unmountable |

Before the init was changed for the `cut` phase, the `check` baseline was `705bac1bb8a87fa5`. It changed only because the initramfs bytes changed.

Power cut (`cut-test.sh <dir> <n> <nsync>`):
- **Boot 1** writes `nsync` fsync'd records, then the rest without fsync. It lets writeback push those to the device for 1 s (no flush, no journal commit yet), then stops.
- **Boot 2** is a fresh VM on the durable image, with no faults.

| Scenario | Boot 1 | Boot 2 |
|---|---|---|
| Honest disk, 16 records, 8 fsync'd | no cache (write-through) | `OK acked=8`, present `1111111100000000` |
| Volatile cache, every write held until a flush (`unflushed` @100), 16/8 | 57 writes held, 53 committed by flushes, **4 still held at the cut and lost** | `OK acked=8`; two runs identical (`2246f5674101bc52` / `745c520ad20538bb`) |
| Lying disk `drop` @10%, all 16 fsync'd, seed `0x5017E0` | the disk acks without persisting | **`VIOLATION record=2`**, present `1100000000000000` |
| Flush-aware `unflushed` @10%, same seed | 10 held, all committed by fsync's flushes | **`OK acked=16`** |
| Seed `0x5017E1`: lying / flush-aware | | `VIOLATION record=2`, present `1101000000000000` / `OK acked=16` |
| Flush-aware @100%, all 16 fsync'd | 107 held, 3 still held at the cut | `OK acked=16` |
| Branch of boot 1 (`unflushed` @100, 16/8) at `hydo-rec: wrote 9` | continue = restore = the fresh cut boot `2246f5674101bc52`, IDENTICAL | |

This is the M26 contrast on the Mac. With the same seed, a disk that lies about durability loses fsync'd records. A disk with a real volatile cache and an honest FLUSH loses only what was never flushed.

Network (`initramfs-net.cpio.gz`, `HVM_NET=1 HVM_NET_RX=guest/net-rx.bin`):
- **The ARP exchange:** `eth0` comes up with MAC `52:54:00:12:34:56`. All three scripted ARP requests get an `ARP reply 10.0.0.2 is-at 52:54:00:12:34:56`, captured in `HVM_NET_TX_OUT`.
- **Determinism:** two boots give `ae4b05bf854a44f3`, and branch at `hydo-net: eth0 up` is IDENTICAL.
- **Faults:** drop+corrupt+dup at 60% gives seed 3 → `6c4f50d5e04a1bd9` twice, and seed 4 → `8d2861c56c73cced` twice. Drop at 100% gives zero packets received and no reply.

## How to run

```sh
# the build machine (Linux with Nix), scratch dir: WORK=$PWD sh <repo>/hv/hvm/linux/build-guest.sh; copy out/* to hv/hvm/guest/
# on the Mac, at the repository root: the engine, signed with the hypervisor entitlement
cargo build --release -p hydo-hvm
codesign --force --entitlements hv/hvm/hv.entitlements -s - target/release/hydo-hvm
cd hv/hvm; H="../../target/release/hydo-hvm hvm"
HVM_DISK=guest/disk-ext4.img $H print guest/Image-virtio guest/initramfs-disk.cpio.gz "hydo-disk: oracle complete"
HVM_DISK=guest/disk-ext4.img HVM_FAULT=0x5017E0 HVM_FAULT_CLASSES=drop HVM_FAULT_PROB=10 $H run guest/Image-virtio guest/initramfs-disk.cpio.gz "hydo-disk: oracle complete"
HVM_DISK=guest/disk-ext4.img $H branch guest/Image-virtio guest/initramfs-disk.cpio.gz "hydo-rec: acked 7" "hydo-disk: oracle complete"
HVM_NET=1 HVM_NET_RX=guest/net-rx.bin HVM_NET_TX_OUT=/tmp/tx.bin $H run guest/Image-virtio guest/initramfs-net.cpio.gz "hydo-net: done"
# the two-boot power cut (what cut-test.sh did; here 16 records, 16 fsync'd): boot 1 with the
# faults, the durable image to /tmp/cut/durable.img; boot 2 a fresh VM on it, no faults
BA="console=ttyS0 earlycon=uart8250,mmio,0x09000000 clocksource.arm_arch_timer.evtstrm=0 nokaslr rdinit=/init loglevel=7 random.trust_bootloader=on"
mkdir -p /tmp/cut
HVM_FAULT=0x5017E0 HVM_FAULT_CLASSES=unflushed HVM_FAULT_PROB=10 HVM_DISK=guest/disk-ext4.img HVM_DISK_OUT=/tmp/cut/durable.img \
  HVM_BOOTARGS="$BA hydo_phase=cut hydo_n=16 hydo_sync=16" $H run guest/Image-virtio guest/initramfs-disk.cpio.gz "hydo-disk: power cut here"
HVM_DISK=/tmp/cut/durable.img HVM_BOOTARGS="$BA hydo_phase=recover hydo_n=16 hydo_sync=16" \
  $H run guest/Image-virtio guest/initramfs-disk.cpio.gz "hydo-disk: oracle complete"
```
Kernel command-line parameters for `init-disk`: `hydo_phase=check|cut|recover`, `hydo_n=<records>`, `hydo_sync=<fsync'd>`. They are passed through `HVM_BOOTARGS`, which replaces the whole command line; the power cut above shows the base.

## Hook points in `hvm.c` (for merging; history: the Rust port in `hydo/crates/hydo-hvm` keeps the same points in `vmm.rs`, `fdt.rs` and `virtio.rs`)

All small, and all inert when no device is attached:
1. `#include "virtio.h"`.
2. `build_fdt`: a loop adding one `virtio,mmio` node per device, before the root node closes.
3. `gic_update_levels`: `virtio_poll(M.vtime)`, then each device's line into its SPI's pending bit.
4. `handle_mmio`: the unmapped-address branch first offers the access to `virtio_mmio(ipa, write, size, &v, M.vtime)`.
5. `handle_wfi`: the clock jumps to the earlier of the timer deadline and `virtio_next_due()`. The old behaviour is the case where that returns 0.
6. `fingerprint`: `p.mach = virtio_hash(p.mach)` when a device is attached.
7. `print_fp`: `virtio_stats(tag)`, which prints nothing without devices.
8. `main`:
   - `virtio_setup()` before `vm_create`, and `virtio_bind(...)` after it;
   - `virtio_finish()` after a `run`/`print` (writes `HVM_DISK_OUT` and `HVM_NET_TX_OUT`);
   - `virtio_save()`/`virtio_restore()` around `branch`.
9. `build.sh` compiles `virtio.c virtio_blk.c virtio_net.c` with `hvm.c`.

## Limits

- **The disk is held in memory,** like hvx's page array. A multi-GiB image needs as much host RAM, and so does a snapshot of it (a file-backed snapshot is the next step).
- **The network is single-queue, without offloads, RX DELAY or REORDER.** There is no host bridge: RX is scripted and TX is captured.
- **The spin limit in `BOOT.md` still applies.** A guest waiting on a device by spinning without exits would not be woken. Linux's virtio drivers sleep on the interrupt, so they don't hit it.
