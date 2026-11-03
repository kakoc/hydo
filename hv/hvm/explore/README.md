# The explorer and `hydo.yaml` on the Mac VMM

**Since M143** the engine hydo runs on the Mac is the Rust port of `serve.c` and `../hvm.c`, `hydo/crates/hydo-hvm` (the program `hydo-hvm`, with this program's command line and protocol; `hydo-hvm hvm print|run|branch` are `../hvm`'s boot probes). `hvmx` below is that C engine; every value on this page is the same with either. **The C engine was deleted in M147** (`serve.c`, `build.sh`, `../hvm.c`, `../virtio*.c`, `../spin_observer.c`; in git at `145c456`), so read `hvmx` and `../hvm` on this page as `hydo-hvm` and `hydo-hvm hvm`. Since M145 the engine boots only agent images (`--control boot serve|one`); the plain `serve`/`one`, RUN and the shell init's S0/S1 markers described below were the shell images'. The commands at the end are the current ones.

**Result:** the explorer with `--backend hvm` (since M102 the Rust `hydo-explore`; the Mac release's `hydo explore` passes it; until M107 `hydo/explore/explore.py`) runs fault schedules against an arm64 guest on this Mac (M2 Pro, macOS 26.4.1) exactly as it does against `hvx_explore` on the Linux box. Projects are built from their `hydo.yaml` with `hydo-build --arch arm64`, and the generic guest init `hydo/onboard/hydo-init.sh` runs unchanged. On the Redis example, with the virtio disk attached, power cuts and seeded disk faults work: 100 swarm schedules gave 37 failures, all in expected classes (lying disk, I/O errors, relaxed `appendfsync`), and all 37 reproduced bit-for-bit. There were no failures with durable settings and an honest disk. Throughput is about 11,500 runs per hour with 4 workers.

## Pieces

| File | What |
|---|---|
| `serve.c` (`hvmx`; deleted in M147) | the run engine. It included `../hvm.c` and spoke `hvx_explore`'s protocol: `READY`, `RUN <tag> <schedule>`, `RESULT ...`, `QUIT`, and the lock-step requests `CTL` and `RESUME` (Milestone 119). `hvmx one ...` ran one schedule from a fresh boot. Now `hydo/crates/hydo-hvm/src/serve.rs` (its module documentation has the Mac engine's particulars) |
| `build.sh` (deleted in M147) | built and signed `hvmx` (with `../virtio*.c`); the engine is now `cargo build --release -p hydo-hvm`, signed by hydo when it runs it |
| `lockstep-check.sh` | the permanent test of lock-step control (Milestone 119; on agent images since Milestone 145): timed input, reactions and SNAP/RESUME branches through `hydo-engine/examples/hvm_lockstep_check.rs`, and `hydo-control/examples/hvm_agent_check.rs` |
| `build-guest-netns.sh`, `hvm-netns.config`, `netnsboot/` | the second arm64 kernel `Image-explore-netns` (Milestone 99): `Image-explore` plus network namespaces, veth, bridge, IPv6, policy routing, iptables + NFQUEUE; only for manifests with `network: bridge`, `addr:` or `faults: [freeze]` (see "The second kernel" below) |
| `build-guest.sh`, `hvm-explore.config` | the arm64 kernel `Image-explore`, built on the Linux box: the M48 kernel with `../linux/hvm-virtio.config`, `../services/svc.config`, MBR partitions and loop, and (by default) the HVC idle below. `HVM_IDLE_HVC=0` rebuilds the earlier WFI-idle kernel |
| `apply-hvcidle.py` | adds `CONFIG_ARM64_HVM_IDLE_HVC`: `cpu_do_idle()` idles through HVC function `0xC6000020` instead of `WFI` (see "Fixed" below) |
| `build-idle-m48.sh` | the M48 kernel configuration plus the HVC idle (`Image-idle`), for `../spin/hvm-spin.c` and M48-style boots |
| `examples/ledger/` | a busybox `httpd` with a CGI that appends ledger entries. The CGI is deliberately not idempotent |
| `examples/redis/` | `hydo/onboard/examples/redis` with the aarch64 Redis 7.2.4 from the nixpkgs cache |
| `../../../hydo/crates/hydo-build` (`src/arm64.rs`) | `hydo-build --arch arm64` (before M103 `hydo_build_arm64.py`) |
| `../../../hydo/crates/hydo-explore`, `../../../hydo/crates/hydo-engine` (`src/hvm.rs`) | `hydo-explore --backend hvm` (before M102 `hvm_backend.py` and `explore.py`) |

**How a schedule runs.** `hvmx serve` boots once and stops at the guest's `hydo-explore-ready-S0` and `-S1` markers. At each it snapshots the vCPU registers, the machine and device state, the virtio device state with the disk image and its volatile write cache, the console text, and all guest RAM (in an unlinked file). For each schedule it restores S1 into a fresh VM and vCPU, arms the disk fault policy, and types the schedule's guest part on the serial console. Then it runs to `hydo-explore-workload-done`, and either:
- turns faults off, types `go`, and runs to the verdict; or
- after a power cut (`C=`), keeps only the flushed disk bytes, restores S0, puts those bytes on the disk, and types `oracle <acked ids> [failed <ids>]`. Those are the two boots of hvx M27.

The fingerprint covers the console, the registers, the machine state, the virtio state with the disk and cache, and all RAM.

**What the Mac backend supports:**
- **Applied as on hvx (the bare-metal machine):** kills, pauses (both forms), realtime clock jumps, the manifest's durability knobs, and network faults (`N=` two-way, `O=` one-way): `hydo-init.sh` puts netem on `lo` with `tc`, exactly as on the bare-metal machine. With the data disk attached it also applies `disk=none|ioerr:P|cut|lying:P` with `dseed`, which use the same seeded fault classes as hvx (`../VIRTIO.md`), and power cuts.
- **Applied differently:** tick-timing jitter (`tj=`, `tjx=`, `--timing`). Linux delays a tick by a number of guest branches, using the PMU clock. The M2 has no PMU clock, so `../hvm.c` delays the timer interrupt by a seeded number of VM exits instead (see "Tick jitter" below). `hvm_supported()` caps a generated `tjmax` at 1,000 exits.
- **Not applied:** network faults on an image without `tc` (built with `HYDO_TC_ARM64=`, or before `tc` was added). Both the explorer (hydo-explore; before M102 `hvm_backend.py`'s `image_has_tc`) and `hvmx` (`scan_has_tc`) read that from the image's compiled manifest (`etc/hydo/manifest.sh`, `HYDO_TC`).

## Tick jitter (counted in exits)

**What it does.** The guest switches threads at timer interrupts, and this VMM can deliver an interrupt only at a VM exit. So the knob is the exit at which the timer interrupt is delivered. Take a deadline in tick period k, where k = deadline / (CNTFRQ/250) is the guest's 4 ms tick. Its interrupt is raised d(k) exits after the deadline: never earlier, and capped inside the period. The counter the guest reads is not changed. d(k) uses hvx's formula, so `explore.py`'s shrinker works unchanged: `h = mix(seed + k*GOLDEN)`; the tick is delayed when `h % 1000 < permille`; then `d = 1 + (h >> 16) % max`. A tick window `tjlo`/`tjhi` and an explicit list `tjx=k:d,...` also work. The policy is host-side state, outside `struct mach`. It is therefore in neither the snapshot nor the fingerprint, and with it off every path is unchanged. `hvmx` turns it on for the workload only, like the disk fault, and its RESULT line gains `tjk=<first>-<last> tjn=<n>`. `hvm` takes `HVM_TJ=<hex seed>,<permille>,<max exits>[,<lo>,<hi>]` for a whole boot.

**The limit.** Thread switches can still happen only at exits. A race window that contains no exit cannot be reached by any jitter, because no exit happens inside it. Linux's branch-counted jitter can reach such a window. On this guest kernel every clock read is an exit, so windows that read the clock (kernel code, system calls that take time, Go and Java programs) are reachable.

**Measured** with `../race/macrace.c`. This is `hv/hvx/race/hydorace.c`'s once-per-run seat race, with two processes on a shared page. Only the sale of seat 10,000 of 20,000 has a window, and between sales each process reads the clock once. Each build was booted 200 times, one jitter seed per boot (`../race/seeds.sh`, kernel `Image-explore-idle`, `HVM_SOFT_IRQ=1`):

| Window | No jitter | Every tick delayed (permille 1000, max 1,000 exits) | 2 ticks in 10 delayed |
|---|---|---|---|
| 50 clock reads | no oversell | **10 of 200** oversold, all 10 repeat exactly | 1 of 200 |
| 50,000 iterations of plain work (no exit) | no oversell | **0 of 200** | not run |

Every seed gave a different fingerprint, so the jitter does change the run. The plain-work window is never hit, as expected. On Linux, hvx's branch-counted jitter found once-per-run plain-memory windows (CLAUDE.md M43).

**Checks.** With jitter off:
- the M48 boot is still `81711ada76a4a236`;
- the Redis schedule `disk=none | run` is still `ca13bcd1d77e2b4b`.

With every tick delayed, seed `1234` gives `4e8fb9fc0b222248` both as `hvmx one` and from the S1 snapshot in `hvmx serve`. The next schedule in the same serve process, without jitter, gives `ca13bcd1d77e2b4b` again, so the policy is off between runs. The explicit list and jitter with a power cut both run. The Redis workload spans only 7 ticks (12–19), so low densities often delay nothing. On the Mac, use `--tj-pm 500,1000`. `explore.py --backend hvm --strategy swarm --timing --tj-pm 500,1000 --seeds 1-40`: 17 schedules were timed and 15 of those were actually perturbed. 16 failures, all reproduced, all in expected classes (none needed the timing).

## Forced preemption points (PCT-style, counted in exits)

**What it does.** PCT (Burckhardt et al. 2010) forces a thread switch at d-1 points chosen at random among the k steps of a run. Its guaranteed chance of finding a bug that needs d orderings is at least 1/(n·k^(d-1)). Here a step is a VM exit, the only place this VMM can act. A race in `../race/macrace.c` spans about 20,500 exits, against about a billion branches. `../hvm.c` (`PCT`, `pct_set`, `pct_at_exit`) picks m exits uniformly in a range from a seed. At each chosen exit it makes the guest's timer deadline due at once: the clock jumps forward to the deadline. So the guest takes its timer interrupt there. The guest's scheduler then switches threads if the running one has used its slice, which happened at about two in three points. PCT's other half, random thread priorities, is not modelled: the guest's scheduler picks the next thread. `hvm` takes `HVM_PCT=<hex seed>,<points>,<first exit>,<last exit>`; `hvmx` takes `pct=<hex seed> pctn=<points> [pctk=<exits>]` (the first pctk exits of the workload, default 20,000) and reports `pctf=`. Host-side state like the tick jitter: in neither the snapshot nor the fingerprint. Without the tokens `hvmx` gives `main`'s results (Redis `7fbc98328b6cf493`, `K=redis@5+2` `46bc747f7adfbfae`). With them, one seed repeats exactly and another seed differs.

**Measured** with `../race/` (`mkimg.sh`, `cells.sh`, `one.sh`), using four builds of the race:
- one window or two windows (`-DNWIN=2`: seats 15,000 and 5,000; the bug needs both windows to be interleaved);
- each window made of 50 clock reads (exits) or of plain work (no exit).

The kernel was `Image-explore-idle` with `HVM_SOFT_IRQ=1`. Points were spread over the race's exits 14,327–34,800. Each strategy ran 200 seeds; the table shows runs with the bug, and the median involuntary switches per run (14 with no help):

| Strategy | One clock window | Two clock windows (both needed) | Plain-work windows | Switches |
|---|---|---|---|---|
| jitter, every tick | 11 | 0 (window 1: 1, window 2: 0) | 0 | 12 |
| 1 point | 5 | – | 0 | 14 |
| 2 points | 6 | 0 (2, 14) | 0 | 15 |
| 10 points | 11 | 0 (9, 6) | 0 | 19 |
| 40 points | **17** | 0 (18, 17) | 0 | 41 |

1,000 more seeds of the two-window race: **40 points found it 7 times** (window 1: 90, window 2: 89; 7 is what independent windows predict), and jitter found it 0 times (window 1: 2, window 2: 0). Every hit repeated exactly: 17 one-window hits and 7 two-window hits. Windows of plain work were never hit.

**In the explorer.** `hydo explore --preempt` on the Mac gives half the schedules forced points. Each gets `pct=<seed> pctn=<10|40> pctk=<3000|10000>`, from `--preempt`'s own random stream, so every other field equals the same seed without it. Set the exit ranges with `--pct-exits`; a run's RESULT reports its workload length as `pctw=`, so the ranges can be matched to the project. The Linux levers `pp=`/`xt=` are dropped on this backend, because they need the PMU clock. The shrinker drops the points, then halves their number. On Linux, the 8,000 generated schedules (random/swarm × `--timing` × `--new-faults` × `--preempt`, seeds 1–500) are identical to `main`'s. On Redis, 100 swarm seeds: 49 schedules got points, and the median fired was 9 of 10 or 36 of 40. Redis workloads take 1,700–13,000 exits. There were 39 failures, all reproduced, none with durable settings, none needing the points.

**Reading.** Few points, PCT's own setting (1 or 2 points for a 1- or 2-ordering bug), did no better than jitter. A forced point does not only switch threads: it moves the guest's clock and every later tick. And the guest chooses the next thread, so PCT's guarantee does not carry over. Many points did better: more switches mean more chances to land in a window. Only the many-point setting found the two-ordering bug. It hits each window independently, so a bug that needs two windows costs the product of the two chances. Jitter reaches only a window that happens to sit near a tick: here it hit the second window 0 times in 1,200 runs.

## Changes outside this directory (hook points)

- **`../hvm.c`**, all inert by default (the M48 boot is still `81711ada76a4a236`):
  - a UART receive queue kept outside `struct mach`, so input can be typed on the console;
  - `hvm_stop_hook`, checked at the top of the run loop;
  - `#ifndef HVM_NO_MAIN` around `main`;
  - `HVM_SOFT_IRQ=1`, which emulates the IRQ exception entry (VBAR + offset, SPSR/ELR, DAIF, PAN/SSBS) instead of raising the line. `hvmx` turns it on. The M48 boot gives the same fingerprint with it.
- **`hydo/explore/explore.py`** (the Python explorer of that milestone, replaced by hydo-explore in M102 and deleted in M107), a small hook:
  - `--backend hvm` and `--hvm-kernel`;
  - `render()` applies `hvm_supported()` after the manifest's `SPACE.restrict()`;
  - the worker's command line;
  - the report's hand-reproduction line;
  - the manifest read from an arm64 initramfs.

  The Linux path is byte-identical: 16,600 generated schedule strings (seeds 1–1000 × random/swarm × `--timing` × `--new-faults` × `--durable-only`, plus every sweep) equal main's. Through this `explore.py` on the bare-metal machine, the pinned reproducers still give `2403499c9a3f8419` (explore-disk.img) and `f87130453609a5d6` (explore-fix-disk.img).
- **`hydo/onboard/hydo-build`** (the Python builder of that milestone, replaced by the `hydo-build` program in M103 and deleted in M107): `--arch arm64` called `hydo_build_arm64.py`. The default path was unchanged.

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

**Determinism** (`netfault/check.py`, deleted in M147 with the C engine it drove: each schedule in two separate serve processes from the S1 snapshot and once as `hvmx one` from a fresh boot; all three fingerprints equal). The README table schedules on the new kernel and image, same verdicts as before:

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
- **Under lock-step control** (CTL, RESUME, `--control one|boot`; Milestone 130) a schedule with
  `spin=rewind` (or `HVM_SPIN_REWIND=1`) runs its workload, check and recovery phases in the rewind
  mode, and a SNAP keeps the mode for its RESUME. The stops stay a pure function of the run: the
  checkpoint is taken at the loop top of every exit after the stop hook, and the guest makes no
  exit between it and a rewind, so a rewind restores the same exit count, console and queued
  input. hvmx does not re-run a controlled SPIN itself (the host's conversation has happened); the
  host does: hydo-control runs an agent image's schedule again with `spin=rewind` when its run
  ended as SPIN (`agent::rewind_again`; `HVM_SPIN_REWIND=0` turns that off too).
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

## The second kernel: service networks, link faults and the freeze (Milestone 99)

**Two kernels.** `Image-explore` (above) is unchanged and stays the default, so every recorded
Mac fingerprint is unchanged. `Image-explore-netns` is the same Linux 6.6.35, the same two patches
(`../linux/apply-cntpct.py`, `apply-hvcidle.py`) and the same configuration, plus
`hvm-netns.config`: `NAMESPACES`, `NET_NS`, `VETH`, `BRIDGE` (with `LLC`, `STP`), `IPV6`,
`IP_ADVANCED_ROUTER` + `IP_MULTIPLE_TABLES`, and `NETFILTER` + `NETFILTER_NETLINK_QUEUE` +
`NETFILTER_XTABLES` + `NETFILTER_XT_TARGET_NFQUEUE` + `IP_NF_IPTABLES` + `IP_NF_FILTER`; kept off:
`BRIDGE_NETFILTER`, `BRIDGE_IGMP_SNOOPING`, `NF_CONNTRACK`, modules. That is what
`hydo/onboard/hydo-init-netns.sh` (`network: bridge`), `hydo-init-links.sh` (`addr:`, and the
etcd example's per-user `ip rule`) and `hydo-init-freeze.sh` (`F=`) need; on x86_64 the NixOS
guest kernel has all of it.

**Build** (`build-guest-netns.sh`, no Nix; any x86_64 Linux machine; built on the bare-metal machine):
the kernel.org tarball and the Arm GNU Toolchain 13.3.Rel1, both checked by sha256
(`docs/BUILD-PINS.md`). Two builds in different directories give the same bytes:
`Image-explore-netns` `806807e7…`, `.config` `0aff8337…`.

**Which images use it.** `hydo-build --arch arm64` builds a manifest with `network: bridge`, an
`addr:` or `freeze` in `faults:` for it and writes `/etc/hydo/kernel` (`Image-explore-netns
<sha256>`) into the image; it prints the choice. `hydo_engine::hvm::kernel_for` reads the record:
the explorer on hvm boots that kernel (the report's summary gains `guest_kernel`, and its
reproduction lines name the kernel file), and so do the Mac `hydo run`, `repro` and `explore`. The
kernel must sit next to `Image-explore` and have the recorded sha256; another file is refused. An
image without the record boots `Image-explore` exactly as before. These images also get iproute2's
`ip` (`network: bridge`), and for `freeze` the aarch64 `iptables-legacy` with the three extensions
the rules load plus `hydo_fc` built for aarch64 (`hydo/crates/hydo-build/src/arm64.rs`).

**Deterministic, like M48** (`netnsboot/`: an initramfs whose `/init` makes two namespaces on a
bridge, runs a web server on port 80 in each, fetches across the bridge and inside a namespace,
pings, puts netem on a veth, adds an iptables NFQUEUE rule and checks `::1`; `mkinit.py` builds it,
`5b634950…` twice): `../hvm print` 20 of 20 fresh boots `33ab6e219a6afd5c` (21,013 exits, 3.25 s of
virtual time), 15 of 15 the same with 12 busy loops on the host (and the Mac swapping), branches at
three points (kernel start, after the namespaces, after the netem and NFQUEUE steps) IDENTICAL to
the continued run, 1 GiB twice `85464ddcc0a84e19`.

**Examples.** `examples/redis-bridge` (two Redis servers in their own namespaces, both on port
6379, a replica, a fault proxy): every schedule twice from a fresh boot, identical, and every fault
visible on the console; an explorer campaign of 30 schedules; and Docker's voting app
(`hydo/onboard/examples/compose-voting/hydo-mac.yaml`). Values in `docs/HISTORY.md`, Milestone 99.

## Lock-step control (Milestone 119)

`hvmx serve` also takes `CTL <tag> <schedule>` and `RESUME <tag> <snapshot>`, and
`hvmx --control one ...` runs `one` the same way. This is `hv/hvx/hvx_explore.c`'s lock-step protocol
(Milestone 118, the design in `docs/design/guest-control.md`), so one host client drives both engines:
hydo-engine's `lockstep` module (`Worker::lockstep`, `Hvm::one_lockstep`). The only difference is the
clock: `at=` is the exit count here and hvx's icount there. The protocol's lines are in
hydo-engine-proto's crate documentation, and the Mac engine's particulars in
`hydo/crates/hydo-hvm/src/serve.rs` (until M147 `serve.c`'s comment above `ctl_esc`).

- The engine stops and waits for the host at the run's start, at the exit where a queued `SEND`
  is due (the bytes are typed there, before the guest runs again, so delivery is exact to the exit),
  at the shell init's markers (where RUN stops), every `rb=<n>` exits (the schedule's `rb=` is the
  cadence in a CTL; default 65,536), and when the guest stops. Before each `STOP` it prints the
  complete console lines since the last stop (`EVENT`) and the sends typed there (`SENT`).
- A snapshot (`SNAP <name>`) is refused while a typed send is not acknowledged (`ACK <id>`), unless
  it is taken with `retype`: bytes still in the UART receive queue are in no snapshot, so `RESUME`
  types every such send again right after the restore. Queued sends are part of the snapshot.
  `RESUME` is the Mac's mid-run branch with typed input; the hvx-style `BRANCH` request (timing
  variants after a marker) is still answered `unsupported on hvm`.
- Without CTL, RESUME or `--control` nothing changes. A CTL whose host only answers CONT gives RUN's
  fingerprint, for the quickstart schedules from S1 and from a fresh boot.

`lockstep-check.sh <dir with Image-explore> <the Redis example's agent image> <out dir>
[<ledger-spin's agent image>]` is the permanent test (`hydo-engine/examples/hvm_lockstep_check.rs`,
then `hydo-control/examples/hvm_agent_check.rs`). Since Milestone 145 it runs on agent images only
(the shell images' RUN and `--control one` were deleted): typed input at exact exit counts from the
host's S1 and from a fresh boot, reactions to the agent's lines and SNAP/RESUME branches, each
against its recorded value; with the fourth argument also ledger-spin's schedules in the rewind mode
(Milestone 130). Values in `docs/HISTORY.md`, Milestones 119, 130 and 145.

## Commands

```sh
# the build machine (Linux with Nix), scratch dir: the kernel and the images (a tmpfs such as /dev/shm is fine)
WORK=$PWD sh <repo>/hv/hvm/explore/build-guest.sh                               # out/Image-explore (HVC idle; HVM_IDLE_HVC=0: WFI idle)
WORK=$PWD sh <repo>/hv/hvm/explore/build-idle-m48.sh                            # out/Image-idle (M48 config + HVC idle)
hydo-build <repo>/hydo/onboard/examples-mac/redis/hydo.yaml --arch arm64 --build-dir ob  # ob/explore-ob-redis-arm64{.cpio.gz,-disk.img}
    # (the hydo-build program: cargo build --release -p hydo-build; or `hydo build` of the Mac release, on the Mac)
# Mac: copy Image-explore and the two images to hv/hvm/explore/out/, then (the engine is
# target/release/hydo-hvm, which hydo signs with the hypervisor entitlement before it runs it)
cargo build --release -p hydo-hvm -p hydo-explore -p hydo-cli
target/release/hydo-explore --backend hvm --image hv/hvm/explore/out/explore-ob-redis-arm64.cpio.gz \
    --strategy swarm --new-faults --seeds 1-100 --workers 4 --out /tmp/hvm-redis
# one schedule by hand (the report prints this line for each minimal reproducer); the host
# controller drives the agent image, so a schedule runs through `hydo run`, not the engine alone
target/release/hydo run --image hv/hvm/explore/out/explore-ob-redis-arm64.cpio.gz --out /tmp/o \
    "disk=cut dseed=0 | run C=12 R=everysec"
```

Diagnostics:
- `HVMX_DIAG=1` writes per-component hashes of each run's end state, with RAM in 1 MiB chunks, to `<outdir>/<tag>.diag`.
- `HVMX_DIAG_CHUNKS=0x700000,...` adds raw copies of those chunks.
- `HVMX_CHECK_RESTORE=1` reads the registers back after every restore and reports any that differ.
- `HVM_TRACE=<file>` logs one line per exit, with a `RUN <tag>` line per schedule; `HVM_TRACE_WATCH=<RAM offset>` adds the guest u64 at that offset to every line.
