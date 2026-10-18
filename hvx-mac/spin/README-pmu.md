# pmuprobe: does Apple's host PMU count a guest's instructions exactly?

The M2 gives a Hypervisor.framework guest no performance counters (`PROBE.md`, probe 4).
This probe asks whether the **host's** core PMU can count the guest instead. If it counts
only the guest's retired instructions or branches, exactly and repeatably, it could replace
the exit-count clock with an instruction clock, as hvx's PMU branch clock does on Intel
(CLAUDE.md M33).

**Undocumented.** It uses Apple's private `kperf` and `kperfdata` frameworks. They have no
public headers. The function signatures follow XNU's open-source kpc interface
(`osfmk/kern/kpc.h`, `osfmk/arm64/kpc.c`). The mode bits come from that file: in the counter
config word, bit 17 enables EL0 AArch64 and bit 18 enables EL1. The fixed counters always
count EL0 and EL1 ("monotonic compatibility"). Any macOS update may change all of this.

## Running it

```sh
cd hvx-mac/spin
/usr/bin/clang -O1 -Wall -arch arm64 -o pmuprobe pmuprobe.c sguest.s -framework Hypervisor
codesign --force --entitlements ../hv.entitlements -s - pmuprobe
./pmuprobe          # without root: checks the frameworks and the event database only
sudo ./pmuprobe     # the measurements; about 10 seconds
```

The root run forces the PMU to user control (`kpc_force_all_ctrs_set(1)`) and gives it
back at the end. If the program is killed partway through, run it again or reboot. That
returns the counters to the system.

Already checked without root on this M2 Pro (macOS 26.4.1): both frameworks load, all 19
functions are present, and the event database is named "Apple A15" with 60 events.
`FIXED_INSTRUCTIONS`, `INST_ALL`, `INST_BRANCH` and `FIXED_CYCLES` are all present
(`out/pmuprobe-noroot.txt`).

## Reading the output

For each mode, `EL0`, `EL1` and `EL0+1`, the probe runs the same two-instruction loop
(`subs x3, x3, #1; b.ne`) in two places, ten times each, at N = 1,000,000 and
N = 10,000,000 iterations:
- `guest`: inside the VM, at guest EL1, via one `hv_vcpu_run`;
- `host`: in the probe's own thread, at host EL0.

Each line is one event:

```
  guest  INST_ALL  N=1000000 min A max B spread B-A | N=10000000 min C max D spread D-C | slope S per iteration
```

- **The deltas:** each one is the thread's counter after the run minus before it.
- **`spread`:** max minus min over the ten repetitions. **0 means the count is exact and
  repeatable.**
- **`slope`:** (C − A) / 9,000,000. It is the number of counted events per loop iteration,
  with the fixed per-run cost removed. A loop iteration is 2 instructions and 1 branch.

How to read it:

| Observation | Meaning |
|---|---|
| `guest INST_ALL` slope ≈ 2 and `guest INST_BRANCH` slope ≈ 1 in some mode | The host PMU counts the guest in that mode. |
| Those slopes ≈ 0 in a mode | The guest is not counted in that mode. On a VHE host the guest's EL1 may count under a different mode bit than the host kernel's, which is why `EL1` and `EL0+1` are both tried. |
| `guest` spread 0 at both N | Exact: the same guest code always gives the same count. That is needed for a deterministic clock. |
| `guest` spread > 0 but the slope is exactly 2 or 1 | The guest is counted exactly, but host work around `hv_vcpu_run` (the host kernel's entry and exit path) is counted too and varies. A guest-only mode would be needed. |
| `host` lines | A control: they show what the same loop counts at host EL0 in each mode. |
| `FIXED_*` lines | These ignore the mode bits and always count EL0 and EL1. |

## What this probe does NOT test, and why it matters

Even an exact guest-only count does not by itself interrupt a spinning guest. hvx stops a
spin by letting the counter overflow, which raises an interrupt and forces a VM exit. On
macOS the overflow interrupt (PMI) goes to the kernel's kperf sampler, not to a user
program. A user-space VMM cannot turn it into `hv_vcpus_exit`.

So the realistic use of a good result is narrower:
- an **instruction clock read at each exit**, giving virtual time = guest instructions
  instead of exits;
- or an **exact budget for the rewind-and-replay resolver** (`RESEARCH.md`), so that
  re-execution could stop after K guest instructions instead of at a breakpoint.

Neither removes the need for the host-timer kick as a hint.
