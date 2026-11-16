# Loops without VM exits on the Mac and KVM engines

This page is the status of Milestone 274 (tentative number), 2026-11-16. The KVM engine has the branch tick
(`guest.branch_tick`, opt-in). The Mac engine's spin observer resolves TigerBeetle's polling loop.
The Mac engine has no answer for a loop whose memory changes in every pass.

## The problem

The Mac and KVM engines count the guest's time in VM exits, and they deliver every interrupt at an
exit. A VM exit is a return of the guest's CPU to the engine: an I/O access, a timer register, a
HLT. Guest code that makes no exit gets no time and no timer interrupt, so the guest kernel never
preempts it. Two programs show it:

- TigerBeetle polls io_uring's completion queue from user space (Milestone 194). On the Mac its
  boot ended `ERROR boot: SPIN` (open item 13).
- kine, the storage layer of k3s, loops on a closed Go channel after a watch is cancelled
  (Milestone 256). Go's `select` draws a random order of its cases in every pass, so the runtime's
  random state in memory changes in every pass.

hvx answers this with its PMU branch clock: the CPU's performance counter interrupts the guest
after a counted number of retired branches (Milestone 33).

The engines already resolve two kinds of such loops, with the spin observer. After 5 ms of host
time without an exit, the watchdog kicks the vCPU out of the guest, and the observer looks at the
loop. A loop that returns to the same state of the whole machine is a cycle. It waits forever.
So the observer delivers the interrupt at a canonical point of the cycle, which depends on the
cycle, not on when the kick came (Milestones 53 and 194). A loop that only counts in registers is
replayed from a checkpoint at the last exit (the rewind mode, Milestones 161 and 53). kine's loop
is neither: its memory differs after every pass, and how many passes ran before the kick depends
on the host.

## What the point must depend on

The interrupt must come at a point that the guest alone fixes. The kick comes at a host time, so
the kick cannot be the point. The point must follow from the guest's execution since its last
exit. For kine's loop, that means a count of something the guest does, from the last exit on. The
count must be exact on every host, and the engine must be able to stop the guest exactly at the
count. Three ways were measured.

## Option 1: a host counter of the guest's branches (KVM)

A `perf_event` on the engine's thread counts the guest's retired conditional branches in user mode
(`exclude_host`, `exclude_kernel`). `perf_event_paranoid` 2 lets a user count such events of its
own thread. Milestone 194 measured it on the build machine. This milestone measured GitHub's
runners with `hv/kvm/pmu/pmuprobe.c`, in the one-off workflow `tmp-exitfree-kvm` (run
43903881480, eight `ubuntu-24.04` jobs):

| host | jobs | a counter? | exact count | overflow skid | trap-flag step | exit, counter off / on / with a read |
|---|---|---|---|---|---|---|
| Intel Xeon 6975P-C | 3 | yes | `0x11c4`: 3,000 of 3,000 runs of 1,000 branches, 50 of 50 of 10^6 | 26 branches (one run 28) | 17.0 µs | 10.6 / 14.4 / 16.1 µs |
| Intel Xeon Platinum 8573C | 1 | no: "no PMU driver" | - | - | - | 5.3 µs / - / - |
| AMD EPYC 7763, 9V45, 9V74 | 4 | no: "PMU not available due to virtualization" | - | - | - | 5.6 to 6.8 µs / - / - |

- On the 6975P-C, `0x01c4` counted only taken branches (999 for 1,000), and `0x00c4` counted the
  loop's unconditional jump too. Only `0x11c4` (`BR_INST_RETIRED.COND`) was exact.
- The runners' own `perf_event_paranoid` is 4, which allows no event. A job sets it to 2 with
  `sudo`. Counting the guest's kernel too needs -1 or `CAP_PERFMON`.
- The skid is how far the guest runs past the counter's overflow before `KVM_RUN` returns. The
  guest never stops early, so the engine sets the overflow before the count and steps the rest.
- In four later runs every Xeon 6975P-C host had the counter, and no other host had one (a Xeon
  Platinum 8370C and the AMD EPYC models). How many of eight jobs got a 6975P-C varied from 1 to 6.

The counter costs every exit 3.8 µs while it counts, and a read of it 1.7 µs more, on these
nested hosts. A run without long stretches pays that and nothing else.

## Option 2: a guest PMU that the guest kernel programs (KVM)

KVM offers the guest a virtual PMU (the probe read CPUID leaf 0xA as version 2, 8 counters of 48
bits, on the 6975P-C). The guest kernel could program a counter and take its overflow interrupt.
This was not built, for two reasons. The interrupt goes through KVM's in-kernel local APIC, and the
engine has no in-kernel interrupt controller: its APIC and timer are its own. And the overflow
interrupt has the same skid as option 1, with no way for the engine to land on the count.

## Option 3: single steps from a fixed exit count

Without a counter, the engine would have to single-step the guest from its last exit to a fixed
instruction count. Every exit-free stretch that reaches the count must be stepped, and the engine
learns that a stretch is long only from the host-timed kick. So the count must be at least the
instructions a fast host runs before the kick: about 2.5·10^7 for a 5 ms kick. One tick would
take about 425 s on the runners (17 µs per step) and 60 s on the Mac (2.4 µs). This was not built.

## The Mac

Hypervisor.framework gives the engine no counter of the guest:

- The guest's PMU registers trap to the engine with nothing behind them (`hv/hvm/PROBE.md`, 3a).
  With guest EL2 on, the framework of macOS 15 emulates a guest PMU. EL2 needs an M3 or later;
  this Mac is an M2 Pro (`hv_vm_config_get_el2_supported` is 0).
- The host's counters, configured with root, never count the guest in the configurable counters,
  and the fixed instruction counter counts host work too, with a spread of thousands
  (Milestone 48).
- A software step costs 2.4 µs (420,000 steps per second, PROBE.md 4), and a stop at a hardware
  breakpoint 4.8 µs (Milestone 241). Both are exact, but option 3's cost applies.

So the Mac engine cannot interrupt kine's loop at a point the guest fixes. It can resolve
TigerBeetle's loop, which is a cycle of the whole machine. The Mac observer already compared the
pages a pass writes, but it armed its breakpoint at the kick's instruction. TigerBeetle's kicks
come inside `memset`, which one frame calls twice per pass, so consecutive arrivals there never
had the same registers, and every look gave up after 64 arrivals. The observer now moves its
breakpoint to the return address (`x30`) after 16 arrivals without a repeat, up to three times.
Where a look is armed decides only whether a cycle is found, never the canonical state it is
resolved at. A run that resolved a loop before resolves it at the same point.

## The branch tick, as built (KVM)

`guest.branch_tick: true` is a format-1 addition. `hydo build` writes the period, 2^26, into the
image's `/etc/hydo/branch-tick`. hydo-engine passes it to the KVM engine as `HYDO_BRANCH_TICK`.
The Mac engine refuses the image before any VM, and hvx ignores the key. The code is
`hydo/crates/hydo-kvm/src/branches.rs` and the `bt_` functions of `vmm.rs`.

**The rule.** After 2^26 retired conditional branches in user mode of one vCPU without one of
its exits, the engine stops that vCPU right after the branch that makes the count. It treats the
point as a proven loop: the clock jumps to its next event, as at an idle HLT, and the interrupt is
delivered at the next entry. With several vCPUs, the turn ends there when another vCPU can run.

**The count.** The engine opens the counter on its thread at the first run, pinned, and reads it
after every `KVM_RUN`. The difference goes to the vCPU that ran. An exit of a vCPU sets its count
to 0. Every snapshot and checkpoint is taken at an exit, so the count is in no snapshot and no
fingerprint.

**The overflow.** The watchdog's first kick of a stretch arms the counter's overflow 256 branches
before the tick (`HYDO_KVM_BRANCH_MARGIN`, a developer variable). The overflow's signal is the
watchdog's own signal, so `KVM_RUN` returns as at a kick. A stretch that passed the tick before the
kick could arm it ends the run as a SPIN, never silently. 2^26 branches take tens of
milliseconds of guest code, and the first kick comes after 5 ms of host time.

**The landing.** From the overflow's stop to the exact count, the engine single-steps the vCPU with
the trap flag (`KVM_GUESTDBG_SINGLESTEP`) and reads the counter after each step. The trap flag is
visible to the guest in three ways, and the landing handles each:

- `PUSHF` pushes the flag: after its step the engine clears the flag in the pushed word.
- `SYSCALL` copies RFLAGS into R11, and `POPF` could load the flag: the engine runs over them to a
  hardware breakpoint at the next instruction, without the trap flag.
- A fault of a stepped instruction pushes RFLAGS in its frame. Hardware breakpoints at the guest's
  handlers of #DE, #UD, #GP and #PF (read from its IDT) stop the vCPU at the handler's first
  instruction. The engine clears the flag in the frame, and runs to a breakpoint at the frame's
  return address.

`INT`, `INT3`, `UD2`, `IRET`, `SYSENTER` and `SYSRET` end the run as a SPIN: their exception or
return cannot be followed. A real exit during the landing ends the stretch, and the exit is handled
as any exit.

**The spin observer.** With the tick on, a kick whose count advanced since the previous kick
skips the observer: the tick ends that stretch. A look whose steps advanced the count proves
nothing. Without that, a cycle with branches would be resolved either by the observer or by the
tick, depending on which the host's timing reached first. A loop in the guest kernel, or one
without conditional branches, does not count, and the observer resolves it as before.

**Inert when off.** Without the key no counter is opened, no read is made, and the run loop's new
calls do nothing. The Redis release image (`f3a152d6…`, `kvm/release-redis`) gave the published
values with this tree on seven Intel hosts and one AMD host (run 43910568187).

## The proofs

- k3s-lease's `containers/hydo.yaml` with the key, on 2 vCPUs of a Xeon 6975P-C runner (run
  43917522895): the node was Ready at 3.1 s of guest time, the StatefulSet's 3 pods at 9.2 s, and a
  pod led on the Lease. `disk=none | run` gave `8c43fbd8700b2862` and OK three times, once with a
  margin of 1024 branches. Each boot had 32 to 63 ticks, each landed by about 1,500 steps (90 ms).
  Without the key the boot stops with `ERROR boot: SPIN`.
- An earlier run (43913419444) on five such hosts gave one fingerprint on every run with margins of
  256 and 3000 branches, up to 18,136 steps in one landing. A trap flag left in the guest would
  have made the margins differ.
- TigerBeetle 0.17.9's single replica on the Mac: five schedules, each twice from a fresh boot,
  one fingerprint each, verdict OK (`hydo/onboard/examples/tigerbeetle/RESULTS.md`).
- Nothing recorded moved: the recorded Mac values of `hvm/redis`, `hvm/ledger` and
  `hvm/ledger-spin` (16 schedules), and the published KVM values of the Redis release image on
  Intel and AMD runners.

## Limits

- Intel only. AMD's count is exact only with a setting that needs root (rr's `SpecLockMap` note),
  and no AMD runner had a counter to measure.
- Only user mode is counted. An exit-free loop in the guest kernel stays the observer's.
- A fault of a stepped instruction other than the four vectors leaves the trap flag in its frame.
- Function contracts (Milestone 255) and the tick both need the debug registers; the engine
  refuses the two together.
- Only the Xeon 6975P-C was measured. Skylake to Comet Lake use another event (`0x01c4`), not
  measured.
- The Mac engine has no answer for a loop whose memory changes in every pass. The emulated guest
  PMU of an M3 or later is the one route left, and it was not measured.

AGENTS.md open items 13, 119 and 138 hold what is left.
