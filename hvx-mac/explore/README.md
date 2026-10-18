# The explorer and `hydo.yaml` on the Mac VMM

**Result:** `hvx/explore/explore.py --backend hvm` runs fault schedules against an arm64 guest on this Mac (M2 Pro, macOS 26.4.1) exactly as it does against `hvx_explore` on the Linux box. Projects are built from their `hydo.yaml` with `hydo-build --arch arm64`, and the generic guest init `hvx/onboard/hydo-init.sh` runs unchanged. On the Redis example, with the virtio disk attached, power cuts and seeded disk faults work: 100 swarm schedules gave 37 failures, all in expected classes (lying disk, I/O errors, relaxed `appendfsync`), and all 37 reproduced bit-for-bit. There were no failures with durable settings and an honest disk. Throughput is about 11,500 runs per hour with 4 workers.

## Pieces

| File | What |
|---|---|
| `serve.c` (`hvmx`) | the run engine. It includes `../hvm.c` and speaks `hvx_explore`'s protocol: `READY`, `RUN <tag> <schedule>`, `RESULT ...`, `QUIT`. `hvmx one ...` runs one schedule from a fresh boot |
| `build.sh` | builds and signs `hvmx` (with `../virtio*.c`) |
| `build-guest.sh`, `hvm-explore.config` | the arm64 kernel `Image-explore`, built on the Linux box: the M48 kernel with `../linux/hvm-virtio.config`, `../services/svc.config`, MBR partitions and loop, and (by default) the HVC idle below. `HVM_IDLE_HVC=0` rebuilds the earlier WFI-idle kernel |
| `apply-hvcidle.py` | adds `CONFIG_ARM64_HVM_IDLE_HVC`: `cpu_do_idle()` idles through HVC function `0xC6000020` instead of `WFI` (see "Fixed" below) |
| `build-idle-m48.sh` | the M48 kernel configuration plus the HVC idle (`Image-idle`), for `../spin/hvm-spin.c` and M48-style boots |
| `examples/ledger/` | a busybox `httpd` with a CGI that appends ledger entries. The CGI is deliberately not idempotent |
| `examples/redis/` | `hvx/onboard/examples/redis` with the aarch64 Redis 7.2.4 from the nixpkgs cache |
| `../../hvx/onboard/hydo_build_arm64.py` | `hydo-build --arch arm64` |
| `../../hvx/explore/hvm_backend.py` | `explore.py --backend hvm` |

**How a schedule runs.** `hvmx serve` boots once and stops at the guest's `hydo-explore-ready-S0` and `-S1` markers. At each it snapshots the vCPU registers, the machine and device state, the virtio device state with the disk image and its volatile write cache, the console text, and all guest RAM (in an unlinked file). For each schedule it restores S1 into a fresh VM and vCPU, arms the disk fault policy, and types the schedule's guest part on the serial console. Then it runs to `hydo-explore-workload-done`, and either:
- turns faults off, types `go`, and runs to the verdict; or
- after a power cut (`C=`), keeps only the flushed disk bytes, restores S0, puts those bytes on the disk, and types `oracle <acked ids> [failed <ids>]`. Those are the two boots of hvx M27.

The fingerprint covers the console, the registers, the machine state, the virtio state with the disk and cache, and all RAM.

**What the Mac backend supports:**
- **Applied as on hvx (the bare-metal machine):** kills, pauses (both forms), realtime clock jumps, the manifest's durability knobs, and network faults (`N=` two-way, `O=` one-way): `hydo-init.sh` puts netem on `lo` with `tc`, exactly as on the bare-metal machine. With the data disk attached it also applies `disk=none|ioerr:P|cut|lying:P` with `dseed`, which use the same seeded fault classes as hvx (`../VIRTIO.md`), and power cuts.
- **Applied differently:** tick-timing jitter (`tj=`, `tjx=`, `--timing`). Linux delays a tick by a number of guest branches, using the PMU clock. The M2 has no PMU clock, so `../hvm.c` delays the timer interrupt by a seeded number of VM exits instead (see "Tick jitter" below). `hvm_supported()` caps a generated `tjmax` at 1,000 exits.
- **Not applied:** network faults on an image without `tc` (built with `HYDO_TC_ARM64=`, or before `tc` was added). Both `hvm_backend.py` (`image_has_tc`) and `hvmx` (`scan_has_tc`) read that from the image's compiled manifest (`etc/hydo/manifest.sh`, `HYDO_TC`).

## Tick jitter (counted in exits)

**What it does.** The guest switches threads at timer interrupts, and this VMM can deliver an interrupt only at a VM exit. So the knob is the exit at which the timer interrupt is delivered. Take a deadline in tick period k, where k = deadline / (CNTFRQ/250) is the guest's 4 ms tick. Its interrupt is raised d(k) exits after the deadline: never earlier, and capped inside the period. The counter the guest reads is not changed. d(k) uses hvx's formula, so `explore.py`'s shrinker works unchanged: `h = mix(seed + k*GOLDEN)`; the tick is delayed when `h % 1000 < permille`; then `d = 1 + (h >> 16) % max`. A tick window `tjlo`/`tjhi` and an explicit list `tjx=k:d,...` also work. The policy is host-side state, outside `struct mach`. It is therefore in neither the snapshot nor the fingerprint, and with it off every path is unchanged. `hvmx` turns it on for the workload only, like the disk fault, and its RESULT line gains `tjk=<first>-<last> tjn=<n>`. `hvm` takes `HVM_TJ=<hex seed>,<permille>,<max exits>[,<lo>,<hi>]` for a whole boot.

**The limit.** Thread switches can still happen only at exits. A race window that contains no exit cannot be reached by any jitter, because no exit happens inside it. Linux's branch-counted jitter can reach such a window. On this guest kernel every clock read is an exit, so windows that read the clock (kernel code, system calls that take time, Go and Java programs) are reachable.

**Measured** with `../race/macrace.c`. This is `hvx/race/hydorace.c`'s once-per-run seat race, with two processes on a shared page. Only the sale of seat 10,000 of 20,000 has a window, and between sales each process reads the clock once. Each build was booted 200 times, one jitter seed per boot (`../race/seeds.sh`, kernel `Image-explore-idle`, `HVM_SOFT_IRQ=1`):

| Window | No jitter | Every tick delayed (permille 1000, max 1,000 exits) | 2 ticks in 10 delayed |
|---|---|---|---|
| 50 clock reads | no oversell | **10 of 200** oversold, all 10 repeat exactly | 1 of 200 |
| 50,000 iterations of plain work (no exit) | no oversell | **0 of 200** | not run |

Every seed gave a different fingerprint, so the jitter does change the run. The plain-work window is never hit, as expected. On Linux, hvx's branch-counted jitter found once-per-run plain-memory windows (CLAUDE.md M43).

**Checks.** With jitter off:
- the M48 boot is still `81711ada76a4a236`;
- the Redis schedule `disk=none | run` is still `ca13bcd1d77e2b4b`.

With every tick delayed, seed `1234` gives `4e8fb9fc0b222248` both as `hvmx one` and from the S1 snapshot in `hvmx serve`. The next schedule in the same serve process, without jitter, gives `ca13bcd1d77e2b4b` again, so the policy is off between runs. The explicit list and jitter with a power cut both run. The Redis workload spans only 7 ticks (12–19), so low densities often delay nothing. On the Mac, use `--tj-pm 500,1000`. `explore.py --backend hvm --strategy swarm --timing --tj-pm 500,1000 --seeds 1-40`: 17 schedules were timed and 15 of those were actually perturbed. 16 failures, all reproduced, all in expected classes (none needed the timing).

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
- kernel `Image-explore` `0e9ad96a…` (WFI idle; the results in this section were measured with it). The default build now gives the HVC-idle kernel `2506bb4d…`; its fingerprints are in "Fixed" below.

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

## Network faults (N=, O=)

**What it took.** The kernel has the three pieces `hydo-init.sh`'s `net_on()` uses built in (`hvm-explore.config`: `NET_SCH_PRIO`, `NET_SCH_NETEM`, `NET_CLS_U32`), and the arm64 root carries the pinned aarch64 `tc` from nixpkgs (`iproute2-6.8.0`, dynamically linked: no static aarch64 build is in the binary cache) with exactly the shared libraries its loader opens (`hydo_build_arm64.py`, via `../services/elfdeps.py`). netem's randomness and timers are the guest kernel's, which the Mac clock makes deterministic, so nothing in the VMM changed except `hvmx`'s check for `tc`.

**Builds** (two each, byte-identical): `Image-explore` `f80c5655…`; `explore-ob-redis-arm64.cpio.gz` `10f7858a…` (disk `9ddfc929…`, unchanged); `explore-ob-ledger-arm64.cpio.gz` `deb1525d…` (disk `ed04ad2a…`). The two example manifests now list `network` and `oneway` in `faults:`.

**Determinism** (`netfault/check.py`: each schedule in two separate serve processes from the S1 snapshot and once as `hvmx one` from a fresh boot; all three fingerprints equal). The README table schedules on the new kernel and image, same verdicts as before:

| Schedule | Fingerprint | Verdict |
|---|---|---|
| `disk=none \| run` | `7fbc98328b6cf493` | OK, 20 acked |
| `disk=cut \| run C=12` | `0c279a6daae800d9` | OK |
| `disk=cut \| run C=12 R=everysec` | `096daef84ac92ff9` | VIOLATION `k:0` lost |
| `disk=lying:10 dseed=5017e0 \| run C=15` | `9674a5cadb027a71` | VIOLATION `k:0` |
| `disk=lying:10 dseed=5017e1 \| run C=15` | `ab7a074d8e1b725b` | VIOLATION `k:6` |
| `disk=ioerr:5 dseed=1234 \| run` | `1efa3d55a1de2694` | ERROR: Redis cannot restart |
| `disk=none \| run K=redis@5+2` | `46bc747f7adfbfae` | OK |
| `disk=none \| run P=redis@8+2` | `86f670644c044826` | OK |

Network faults, all identical (serve, serve, one):

| Schedule | Fingerprint | Verdict |
|---|---|---|
| Redis `N=redis@8-14:delay,20ms,loss,30%` | `64817a74f4032ce5` | OK, 19 acked |
| Redis `N=redis@4-12:delay,20ms,reorder,38%` | `6732e6a5ae862866` | OK |
| Redis `N=redis@6-10:delay,259ms,duplicate,13%` | `30ad53fe6f22d5ef` | OK |
| Redis `O=redis@5-10:req:loss,100%` | `d8bd1940f0487906` | OK, 15 acked |
| Redis `O=redis@5-10:resp:loss,100%` | `d53075bf34610585` | OK, 15 acked |
| Redis `O=redis@3-9:resp:delay,300ms` | `a94e4c6a7cc017ff` | OK |
| Redis `disk=cut \| N=redis@9-12:delay,20ms,reorder,38% C=18` | `4dea8be4c216d44e` | OK |
| ledger, no fault | `5343027a7c6085cf` | OK, 30 acked |
| ledger `O=front@5-8:resp:loss,100%` | `f24b64bdecfb1919` | OK (see below) |
| ledger `O=front@0-30:resp:loss,70%` | `d3f1005e3359d18e` | VIOLATION entry 4 stored 2 times |
| ledger `O=front@0-30:resp:loss,50%` | `6e2bcdd3355578e4` | VIOLATION entry 9 stored 2 times |
| ledger `N=front@0-30:loss,60%` | `e7cbdefd36ce87c3` | VIOLATION entry 7 stored 2 times |

The ledger's lost-reply duplicate needs *partial* reply loss: losing 100% of the server's packets also drops its SYN-ACK, so no connection opens, the request is never applied and the retry stores the only copy. With 50–70% loss some connections open and then lose the answer for longer than the client's 2 s timeout; the retry stores the applied entry again.

**Exploration with network faults** (`explore.py --backend hvm --strategy swarm --new-faults --seeds 1-100 --workers 4`; network faults now drawn because the manifests list them and the images have `tc`):
- **ledger:** 75 of 100 schedules had a network fault. 39 failures, 39 reproduced. 38 are the planted non-idempotent retry (`entry # stored # times`), 5 of them with a network fault and no pause or kill (e.g. seed 28 `O=front@10-14:resp:loss,78%`, seed 58 `O=front@25-30:resp:loss,93%`). 1 is a harness artefact: seed 40 kills the front door while a 2,295 ms request delay is on its port, so the restart's readiness check (`wget -T 2`) never passes (`ERROR front restart failed`). 6,494 runs/hour.
- **Redis:** 75 of 100 had a network fault (58 consoles show `NET FAULT`). 36 failures, 36 reproduced, all expected classes: lying disk 18, I/O errors 11, relaxed `appendfsync` 7; none with durable settings and an honest disk. 787 runs/hour (netem delays and the disk make these runs longer).

## Fixed: a rare divergence under heavy host load (a WFI that does not trap)

**Symptom (before the fix).** With 8 concurrent serve processes on the 12-core Mac and the Mac swapping (15–20 GB of swap in use), about 1 run in 150 ended with a different full-state fingerprint: 14 of 2,240 runs of `disk=none | run` in five stress batches, plus one batch in which one serve process gave the odd value for every run (its S1 snapshot was taken after the divergence). The verdict, the console and the whole exit trace (exit count, exception class, PC, CPSR at every exit) were identical. The odd runs repeat a few discrete alternatives (`fb04c7dca19c1f42` in every batch here).

**Where.** With `HVMX_DIAG_CHUNKS` the difference is two words of guest RAM, at RAM offsets `0x3ffd14d8` and `0x3ffd1418`, plus a copy of the first on the idle task's stack (`init_thread_union+0x3c70`, inside an `el1h_64_irq` frame). Against `System.map` these are this CPU's per-cpu `context_tracking.state` (`0x854` vs `0x85c`) and a jiffies-valued field of `tick_cpu_sched` (`0xfe` vs `0xff`). The context-tracking state counts RCU extended-quiescent-state transitions in steps of 4, so `+8` is exactly one extra idle entry and exit.

**When.** `HVM_TRACE` with the new `HVM_TRACE_WATCH=0x3ffd14d8` (the watched guest word on every exit line) pins it to one exit: in a normal and an odd run from the same serve process the 19,818th exit is the trapped `WFI` in `cpu_do_idle` in both, but the word already reads `0x251` vs `0x259` there. So between the previous exit and that WFI exit, the odd run went once more around the idle loop, through a WFI that did NOT exit.

**Root cause.** On Arm, `WFI` traps to the hypervisor (HCR_EL2.TWI) only if it would actually enter a low-power state. If a wake-up event is already pending at that instant it completes as a no-op, inside the guest. A host interrupt that Hypervisor.framework is about to take at EL2 is such an event, and host interrupts are frequent when the Mac is heavily loaded. The guest then repeats its idle loop (RCU idle entry/exit, a tick_nohz recomputation) and executes the WFI again at the same PC, which traps as usual. Nothing in the exit stream changes; only those counters do, and the counters are in RAM. This is the same signature as the spin observer's 1-in-300 (`3820c07d53076f1e`, M48 kernel).

**Fix.** A guest kernel option, `CONFIG_ARM64_HVM_IDLE_HVC` (`apply-hvcidle.py`, on by default in `build-guest.sh`): `cpu_do_idle()` issues `HVC #0` with `x0 = 0xC6000020` instead of `WFI`. An HVC always exits, so every idle entry is a VM exit, and the VMM handles it exactly like a trapped WFI (`idle_wait()` in `../hvm.c`, also in `../spin/hvm-spin.c`). With a WFI-idle kernel nothing changes in the VMM.

**Proof.**
- `disk=none | run`, 8 serve processes × 250 runs (2,000 runs, the Mac swapping): **2,000 of 2,000 identical** (`ca13bcd1d77e2b4b`); a further 1,280 runs (8 × 160) under extra load from the spin hunt below: 1,280 of 1,280. With the old kernel the same harness gave 14 odd of 2,240.
- The table schedules (README "Results") in two serve processes and as `hvmx one`, on the new kernel, all identical, with the same verdicts as before:

  | Schedule | Fingerprint (HVC idle) | Verdict |
  |---|---|---|
  | `disk=none \| run` | `ca13bcd1d77e2b4b` | OK, 20 acked |
  | `disk=cut \| run C=12` | `eb7abc5988d8f407` | OK |
  | `disk=cut \| run C=12 R=everysec` | `5e49fb9cb93d0cf6` | VIOLATION `k:0` lost |
  | `disk=lying:10 dseed=5017e0 \| run C=15` | `659c131dd3bb7fab` | VIOLATION `k:0` |
  | `disk=lying:10 dseed=5017e1 \| run C=15` | `aea81d3abae312a0` | VIOLATION `k:6` |
  | `disk=ioerr:5 dseed=1234 \| run` | `f9d247598212f199` | ERROR: Redis cannot restart |
  | `disk=none \| run K=redis@5+2` | `4b5eed88d6135958` | OK |
  | `disk=none \| run P=redis@8+2` | `97f017cccf4e25a6` | OK |

- In total, with the HVC-idle kernel: **4,880 of 4,880** `disk=none | run` runs identical across three 8-engine batches (2,000, 1,280, 1,600), with the Mac at 15–21 GB of swap.
- On the earlier WFI-idle kernel (`Image-explore` `0e9ad96a…`) the modified `hvmx` reproduces every fingerprint of the "Results" table exactly, so the VMM change is inert for it. The M48 boot (`../hvm`, `81711ada76a4a236`), the virtio network boot (`ae4b05bf854a44f3`) and the virtio disk boot (`952991033b38776d`) are unchanged too; a lying-disk run gives the same fingerprint as main's `hvm`.
- Kernels are byte-reproducible: `build-guest.sh` gives `2506bb4d…` (and `0e9ad96a…` with `HVM_IDLE_HVC=0`) in two builds each, `build-idle-m48.sh` gives `Image-idle` `a224d893…` in two builds.

**The spin observer (`../spin/hvm-spin.c`).** Its 1-in-300 divergence under load was the same WFI effect: on the M48 kernel it gave the same odd value as before (`3820c07d53076f1e`, 2 of 400 runs with `HVM_SPIN_MS` 1–20 under the 8-engine load). On `Image-idle` that value is gone, but a second, rarer problem showed up: 3 of 3,368 runs ended differently, each with a different value. The spin log of one shows why: a kick landed at a host-timed PC outside any loop (in `gic_handle_irq`, while the vCPU thread had been descheduled), the observer armed its breakpoint there, the breakpoint was hit once, and when the guest then spun in the user-space loop the observer never re-armed, so the run ran into the SPIN limit instead of resolving the loop. The observer now re-arms whenever a kick lands away from its armed PC (re-arming is invisible to the guest). With both fixes: **2,000 of 2,000** spin boots identical (`37a4599547ab42f2`, all `stop=1`), `HVM_SPIN_MS` 1–20, under the 8-engine load. The M48 kernel's normal spin fingerprint (`3a36c4ee97b37d37`) is unchanged.

**Remaining envelope.** Other guest instructions that complete differently when a host interrupt is pending stay a risk of the same kind: a `WFE` (woken by the exception return of a host interrupt) and an exclusive load/store pair (an exception return clears the exclusive monitor, so `STXR` fails once more and the retry loop runs once more). This kernel uses LSE atomics and has the event stream off, and neither showed up in 3,280 runs, but user programs with LL/SC loops or `WFE` (for example musl's atomics without LSE) could still diverge this way under heavy load. Trapping `WFE` is possible only through a private Hypervisor.framework control (`../spin`); exclusive pairs cannot be trapped.

## Exit-free spins (the spin observer)

`hvmx` includes `../hvm.c`, so it has the spin observer (`../spin_observer.c`,
`../spin/RESEARCH.md` "In the main engine"): a guest that busy-waits in user space without any
exit is resolved deterministically instead of ending the run as SPIN.
- **Exact-repeat loops** (a load, load-acquire, `yield`, same-value store, exclusive load) are
  resolved in every run.
- **Counter loops** (`while (flag) n++;`) need the rewind mode. `HVM_SPIN_REWIND=auto` (the
  default here) runs a schedule plainly and, only if a phase ends as SPIN, restores S1 and runs it
  again with rewind on; that run is reported, and stderr says so. `=1` always, `=0` never; the
  host token `spin=rewind` forces it for one schedule. The RESULT line format is unchanged.
- **Kick interval** `HVM_SPIN_MS`, default 5 ms: lower is faster for a spinning guest, at the
  price of more (harmless) kicks on a loaded host.

`examples/ledger-spin` is the ledger with a busy-wait in every request (`/bin/spin_load`; every
tenth entry `/bin/spin_count`, a counter loop), built from `../spin/spinvar.c`. On the netem
kernel its schedules give one fingerprint each in two serve processes and `hvmx one`, with and
without tick jitter, and every run escalates to rewind because of the counter loop. A short
exploration (`explore.py --backend hvm --image explore-ob-ledger-spin-arm64.cpio.gz --strategy swarm
--new-faults --seeds 1-30 --workers 3`, 5 ms kicks): 14 failures, 14 reproduced, all the planted
non-idempotent retry (`entry N stored twice`, e.g. seed 6 `P=front@26+2 T=10:-30`, seed 7
`P=front@10+2`); 929 runs/hour with every schedule re-run in rewind mode.

## Commands

```sh
# the build machine (Linux with Nix), scratch dir: the kernel and the images (a tmpfs such as /dev/shm is fine)
WORK=$PWD sh <repo>/hvx-mac/explore/build-guest.sh                               # out/Image-explore (HVC idle; HVM_IDLE_HVC=0: WFI idle)
WORK=$PWD sh <repo>/hvx-mac/explore/build-idle-m48.sh                            # out/Image-idle (M48 config + HVC idle)
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
- `HVM_TRACE=<file>` logs one line per exit, with a `RUN <tag>` line per schedule; `HVM_TRACE_WATCH=<RAM offset>` adds the guest u64 at that offset to every line.
