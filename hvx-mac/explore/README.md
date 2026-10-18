# The explorer and `hydo.yaml` on the Mac VMM

**Result:** `hvx/explore/explore.py --backend hvm` runs fault schedules against an arm64 guest on this Mac (M2 Pro, macOS 26.4.1) exactly as it does against `hvx_explore` on the Linux box. Projects are built from their `hydo.yaml` with `hydo-build --arch arm64`, and the generic guest init `hvx/onboard/hydo-init.sh` runs unchanged. On the Redis example, with the virtio disk attached, power cuts and seeded disk faults work: 100 swarm schedules gave 37 failures, all in expected classes (lying disk, I/O errors, relaxed `appendfsync`), and all 37 reproduced bit-for-bit. There were no failures with durable settings and an honest disk. Throughput is about 11,500 runs per hour with 4 workers.

## Pieces

| File | What |
|---|---|
| `serve.c` (`hvmx`) | the run engine. It includes `../hvm.c` and speaks `hvx_explore`'s protocol: `READY`, `RUN <tag> <schedule>`, `RESULT ...`, `QUIT`. `hvmx one ...` runs one schedule from a fresh boot |
| `build.sh` | builds and signs `hvmx` (with `../virtio*.c`) |
| `build-guest.sh`, `hvm-explore.config` | the arm64 kernel `Image-explore`, built on the Linux box: the M48 kernel with `../linux/hvm-virtio.config`, `../services/svc.config`, MBR partitions and loop |
| `examples/ledger/` | a busybox `httpd` with a CGI that appends ledger entries. The CGI is deliberately not idempotent |
| `examples/redis/` | `hvx/onboard/examples/redis` with the aarch64 Redis 7.2.4 from the nixpkgs cache |
| `../../hvx/onboard/hydo_build_arm64.py` | `hydo-build --arch arm64` |
| `../../hvx/explore/hvm_backend.py` | `explore.py --backend hvm` |

**How a schedule runs.** `hvmx serve` boots once and stops at the guest's `hydo-explore-ready-S0` and `-S1` markers. At each it snapshots the vCPU registers, the machine and device state, the virtio device state with the disk image and its volatile write cache, the console text, and all guest RAM (in an unlinked file). For each schedule it restores S1 into a fresh VM and vCPU, arms the disk fault policy, and types the schedule's guest part on the serial console. Then it runs to `hydo-explore-workload-done`, and either:
- turns faults off, types `go`, and runs to the verdict; or
- after a power cut (`C=`), keeps only the flushed disk bytes, restores S0, puts those bytes on the disk, and types `oracle <acked ids> [failed <ids>]`. Those are the two boots of hvx M27.

The fingerprint covers the console, the registers, the machine state, the virtio state with the disk and cache, and all RAM.

**What the Mac backend supports:**
- **Applied as on the box:** kills, pauses (both forms), realtime clock jumps and the manifest's durability knobs. With the data disk attached it also applies `disk=none|ioerr:P|cut|lying:P` with `dseed`, which use the same seeded fault classes as hvx (`../VIRTIO.md`), and power cuts.
- **Not applied:**
  - network faults (`N=`, `O=`), because the arm64 root has no `tc`;
  - tick-timing jitter (`tj*`), because this hardware has no PMU clock.

  `hvm_supported()` removes these from generated schedules before they are rendered. If a schedule asks for one anyway, `hvmx` answers `verdict=UNSUPPORTED`.

## Changes outside this directory (hook points)

- **`../hvm.c`**, all inert by default (the M48 boot is still `81711ada76a4a236`):
  - a UART receive queue kept outside `struct mach`, so input can be typed on the console;
  - `hvm_stop_hook`, checked at the top of the run loop;
  - `#ifndef HVM_NO_MAIN` around `main`;
  - `HVM_SOFT_IRQ=1`, which emulates the IRQ exception entry (VBAR + offset, SPSR/ELR, DAIF, PAN/SSBS) instead of raising the line. `hvmx` turns it on. The M48 boot gives the same fingerprint with it.
- **`hvx/explore/explore.py`**, a small hook:
  - `--backend hvm` and `--hvm-kernel`;
  - `render()` applies `hvm_supported()` after the manifest's `SPACE.restrict()`;
  - the worker's command line;
  - the report's hand-reproduction line;
  - the manifest read from an arm64 initramfs.

  The Linux path is byte-identical: 16,600 generated schedule strings (seeds 1–1000 × random/swarm × `--timing` × `--new-faults` × `--durable-only`, plus every sweep) equal main's. Through this `explore.py` on the bare-metal machine, the pinned reproducers still give `2403499c9a3f8419` (explore-disk.img) and `f87130453609a5d6` (explore-fix-disk.img).
- **`hvx/onboard/hydo-build`**: `--arch arm64` calls `hydo_build_arm64.py`. The default path is unchanged.

## Results

**Images.** Byte-identical in two builds each:
- `explore-ob-ledger-arm64.cpio.gz` `ec5f57f6…`, `-disk.img` `ed04ad2a…`;
- `explore-ob-redis-arm64.cpio.gz` `4c957510…`, `-disk.img` `9ddfc929…`;
- kernel `Image-explore` `0e9ad96a…`.

**Snapshot branch = fresh boot.** Ten Redis schedules with the disk were each run in two serve processes and as `hvmx one`, and all three agree:

| Schedule | Fingerprint | Verdict |
|---|---|---|
| `disk=none \| run` | `9bc2dfcef021dd8a` | OK, 20 acked |
| `disk=cut \| run C=12` | `9d8513b6735ba610` | OK |
| `disk=cut \| run C=12 R=everysec` | `19970ebdde6ecb55` | VIOLATION `k:0` lost (Redis's documented window, as hvx M27) |
| `disk=lying:10 dseed=5017e0 \| run C=15` | `75bbe5ff55652ab2` | VIOLATION `k:0` |
| `disk=lying:10 dseed=5017e1 \| run C=15` | `956f4d2af53ee4ac` | VIOLATION `k:6` (a different seed loses a different key) |
| `disk=ioerr:5 dseed=1234 \| run` | `52c5eb54b6d4b896` | ERROR: Redis cannot restart |
| `K=redis@5+2`, `P=redis@8+2`, and four more | identical | OK or as above |

The ledger without a disk (first version): 6 schedules, two serve processes and a fresh boot each, 18 of 18 identical.

**Explorer runs** (`explore.py --backend hvm --strategy swarm --new-faults --seeds 1-100 --workers 4`):
- **Redis** (manifest fault space: kill, pause, cut, clock, disk; knob R): 57 schedules with a cut, 52 with a disk fault. 37 failures:
  - 18 lying disk;
  - 12 I/O errors;
  - 7 relaxed `appendfsync`.

  All 37 reproduced, with 0 durable-settings failures. The minimal reproducers were re-run by hand with identical fingerprints:
  - `disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no` → `k:0` lost, `fc31be36342e2e84`;
  - `disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20` → `k:6` lost, `1b975c197a6f78fa`.

  11,568 runs/hour.
- **Ledger:** 23 failures, all `entry N stored 2 times` (the planted non-idempotent CGI), 23 of 23 reproduced. Shrunk to `disk=none dseed=ea4fb8211cfa3702 | run P=front@26+1`: request 26 is accepted into the paused server's backlog, the client times out, the server applies it after the resume, and the retry applies it again. 8,611 runs/hour.
- **More workers are slower.** The first ledger version did 8,088 runs/hour with 4 workers but only 1,977 with 8, with the Mac swapping (about 20 GB of swap in use).

## Open: a rare divergence in long-lived serve processes under heavy host load

Under heavy host load, about 1 run in 100 to 150 inside a serve process that had already run other schedules ends with a different full-state fingerprint. This was 8 concurrent serve processes on the 12-core Mac with about 20 GB of swap in use.

**What is measured:**
- **Rate:**
  - 3 of 624, 3 of 288 and 2 of 288 runs in such stress tests;
  - 0 of 216 with 2 processes;
  - 0 of 284 fresh boots (`hvmx one`) under the same 8-way load;
  - 0 of 48 serve processes that ran one schedule each.
- **What differs:** the verdict and the console are identical, and so are the exit traces (exit count, exception class, PC and CPSR at every exit). The fingerprint differs by a few bytes of guest RAM: stale values on kernel stacks, such as `0xce4` vs `0xcec` in the idle task's stack (`init_thread_union`) and `0x190` vs `0x191`. Each odd run repeats one of a few alternative values, so the alternatives are discrete.
- **Tried, did not remove it:**
  - software IRQ entry (`HVM_SOFT_IRQ`);
  - an instruction-cache invalidate after the RAM restore;
  - a fresh VM rather than just a fresh vCPU per restore;
  - `mlock` of guest RAM.

  These stay in, as defensive measures.
- **Fixed during this work, and not the cause:** S0's restore overwrote the console text that S1's fingerprint covers. It is now saved with each snapshot. It changed every schedule after a power cut and is fixed.

**Most likely cause, not proven:** host-side events that Hypervisor.framework hides from us, such as host interrupts and preemptions of the vCPU thread. Each one is an exception return into the guest. That clears the exclusive monitor and can end a WFE early, which changes only how many times a guest retry or wait loop ran. That would match the "dead stack residue only" signature.

**Effect on the explorer:** a failure's reproduction check compares fingerprints, so with many workers a real failure can occasionally be reported as not reproduced. With 4 workers none was, in 300 schedules.

## Commands

```sh
# build machine, scratch dir: the kernel and the images
WORK=$PWD sh <repo>/hvx-mac/explore/build-guest.sh                               # out/Image-explore
nix shell nixpkgs#python3 --command python3 <repo>/hvx/onboard/hydo-build \
    <repo>/hvx-mac/explore/examples/redis/hydo.yaml --arch arm64 --build-dir ob  # ob/explore-ob-redis-arm64{.cpio.gz,-disk.img}
# Mac: copy Image-explore and the two images to hvx-mac/explore/out/, then
sh hvx-mac/explore/build.sh
cd hvx/explore && python3 explore.py --backend hvm --image explore-ob-redis-arm64.cpio.gz \
    --strategy swarm --new-faults --seeds 1-100 --workers 4 --out /tmp/hvm-redis
# one schedule by hand (the report prints this line for each minimal reproducer)
HVM_DISK=hvx-mac/explore/out/explore-ob-redis-arm64-disk.img hvx-mac/explore/hvmx one \
    hvx-mac/explore/out/Image-explore hvx-mac/explore/out/explore-ob-redis-arm64.cpio.gz /tmp/o "disk=cut dseed=0 | run C=12 R=everysec"
```

Diagnostics:
- `HVMX_DIAG=1` writes per-component hashes of each run's end state, with RAM in 1 MiB chunks, to `<outdir>/<tag>.diag`.
- `HVMX_DIAG_CHUNKS=0x700000,...` adds raw copies of those chunks.
- `HVMX_CHECK_RESTORE=1` reads the registers back after every restore and reports any that differ.
- `HVM_TRACE=<file>` logs one line per exit, with a `RUN <tag>` line per schedule.
