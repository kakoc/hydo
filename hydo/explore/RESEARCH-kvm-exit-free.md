# Interrupts for guest code that makes no VM exit on the KVM engine

Written 2026-11-08 (Milestone 194) for the KVM engine (`hydo/crates/hydo-kvm`,
docs/design/kvm-engine.md) after TigerBeetle was found not to run on it (Milestone 189, AGENTS.md
open item 45 (c)). The measurements were made on the build machine (Core i7-13700HX, a hybrid CPU:
performance cores are CPUs 0-15, efficiency cores CPUs 16-23; Linux 6.6.35;
`perf_event_paranoid` 2; no root). Sources are named where they are used; the list is at the end.

## The problem

The KVM engine counts guest time in the VM exits that reach it and delivers every interrupt at
such an exit. Guest code that makes no exit gets no time and no interrupt. The engine's spin
observer resolves two kinds of exit-free loops: an exact cycle of the general registers (the
loop stores nothing), and a counter loop (the rewind mode, Milestone 161). Anything else that runs
without an exit for `HYDO_KVM_SPIN_SECS` ends the run as SPIN (invalid).

**What TigerBeetle does.** The vCPU was sampled at the watchdog's kicks
(`HYDO_KVM_TRACE_UNKNOWN=1`) and the release binary (0.17.9, static, stripped) was disassembled at
the sampled addresses. `tigerbeetle format` waits for its disk writes in
`while (!done) io.tick();` (the loop at `0x1158d60`). `IO.tick` (`0x10f5a30`) calls `flush` and
`flush_completions` (`0x10a13f0`). With nothing to submit and nothing to wait for, neither makes
a system call: `flush_completions` fills its 4 KiB array of completion entries with `0xaa`
(Zig's `undefined` in a ReleaseSafe build, an AVX2 `memset` at `0x14310b0`), reads the completion
queue's head and tail (`0x10a12c0`), finds nothing, checks the overflow flag in the shared ring,
and returns. So the loop:

- makes no VM exit and no system call;
- stores to memory in every pass (the 4 KiB array, the return addresses and saved registers of
  its calls), but every store writes the bytes that are already there;
- uses VEX instructions (the `vmovdqu` stores of the `memset`), which the spin observer's decoder
  does not know.

The disk completion it waits for is due 16 exits after the request. It never comes, because the
loop makes no exit. On hvx the PMU branch clock interrupts the loop after a fixed number of guest
branches. `tigerbeetle start` waits the same way while it opens its data file.

## Option 1: an interrupt at an exact guest branch count from the host's performance counters

**Idea.** hvx's clock, done from user space: a host `perf_event` on the vCPU thread counts the
guest's retired conditional branches only (`exclude_host`), overflows shortly before a target
count, and its signal takes the thread out of `KVM_RUN`. The engine then steps the guest to the
exact count and delivers there.

**How the count is made guest-only.** For an event with `exclude_host`, perf on Intel enables
the counter only in the guest's `IA32_PERF_GLOBAL_CTRL` value, which KVM loads at VM entry and
replaces at VM exit (the VMCS's atomic MSR switch). perf's own NMI handler restores the host value,
which leaves such a counter off; hvx had to fix exactly this in Milestone 156, because hvx
bypassed perf. On AMD the event's Guest-Only bit does the same.

**Measured** (the probe `hv/kvm/pmu/pmuprobe.c`, run on the build machine: a KVM VM
with one vCPU in 32-bit protected mode at CPL 3 running `mov ecx,N; l: dec ecx; jnz l; out`, and
a `perf_event` on the calling thread with `exclude_host`, `exclude_kernel` and `pinned`, raw
event `0xc4` with the core type's umask):

| question | result |
|---|---|
| Can an unprivileged user count the guest at all? | Yes for the guest's user mode (`exclude_kernel`): `perf_event_paranoid` 2 allows a thread's own user-mode events. Counting the guest kernel too was refused (`EACCES`); it needs `CAP_PERFMON` or `perf_event_paranoid` 1 or lower. |
| Which event counts exactly? | Performance core (PMU `cpu_core`, type 4): `0x11c4` (conditional branches, taken and not taken) gave exactly N; `0x01c4` N-1 (taken only), `0x10c4` 1 (not taken), `0x00c4` N+1 (all branches: the `jmp` too). Efficiency core (`cpu_atom`, type 10): `0x7ec4` gave exactly N; `0x00c4` gave N+30 (all branches, with interrupts and VM exits counted as far branches). An event of one core type counts nothing on the other type, so the vCPU thread must stay on one kind of core. |
| Is the count exact over long runs? | 15 runs of 10^9 branches each on a performance core, 15 on an efficiency core, and 30 runs of 3·10^8 with another busy process on the same CPU (the vCPU thread preempted constantly): every count exact. |
| How far past the overflow does the guest get before `KVM_RUN` returns (skid)? | Overflow signal by `F_SETOWN_EX` and `F_SETSIG` to the thread: 26 branches on a performance core every time (50 runs), 8 to 18 on an efficiency core, in this branch-dense loop (two instructions per branch). |
| Can the exact count be reached? | Overflow set 2000 branches before the target, then trap-flag single steps (`KVM_GUESTDBG_SINGLESTEP`) until the counter reads the target: 50 of 50 runs stopped with the same RIP and ECX on each core type. A step cost 1.4 µs on a performance core and 2.5 µs on an efficiency core; the 3,950 steps of that margin took 5.6 ms and 9.8 ms. |
| SMIs | `freeze_on_smi` was 1 for both core types on the build machine (it needs root to set). Without it, branches the firmware retires in system management mode are counted as the guest's: the cause of Milestone 65's rare differences on hvx. |

**What would make it work in hydo.**

- *An exact stop without the trap flag.* Stepping with the trap flag leaks into the guest: PUSHF
  writes the flag to memory, SYSCALL copies it into R11 (and SYSRET then sets it in user mode),
  and an exception during a step pushes it in the exception's frame, from where the handler's IRET
  restores it (Milestone 159 found this in the spin observer and moved it to hardware breakpoints).
  The first two can be recognized from the opcode; a page fault cannot be predicted, and memory
  that a program touches for the first time faults every 4 KiB. Whether a fault falls inside the
  stepped stretch depends on where the overflow signal stopped the guest, which is host timing. So
  the steps must use hardware breakpoints: a breakpoint on both successors of the next conditional
  branch, found by decoding forward from RIP (an instruction-length decoder for every x86-64
  instruction, VEX and EVEX included, and the targets of RET and indirect jumps read from
  registers and memory). The counter itself checks every stop (the count must have grown by
  exactly one), so a decoding mistake shows as a failed run, not as a silent difference. A signal
  handler entered inside the stretch still loses control; that run would have to be refused. hvx
  has none of these problems because it steps with the VMX monitor trap flag, which KVM does not
  offer to user space.
- *A rule that depends on the guest alone*, for example "an exit of the engine's own after every
  P user-mode conditional branches without an exit" or "at every multiple of P user-mode branches";
  the count in snapshots; the event pinned, refusing the run when perf cannot schedule it (another
  user of the counters).
- *Only the guest's user mode*, without `CAP_PERFMON`. Exit-free loops in the guest kernel stay
  the observer's.

**Cost.** Per engine-made exit: the overflow signal and the return from `KVM_RUN` (tens of
microseconds) plus the steps from the overflow to the target. With the margin set from the skid
(about 128 branches), that is a few hundred breakpoint runs of 1.5 to 3 µs each, about 0.5 ms. With
P = 2^22 branches (a few milliseconds of guest code) the cost is around a tenth of the time of
exit-free code, and nothing for code that exits often.

**What it changes.** It is a new clock: every run that executes P user branches without an exit
gets other fingerprints, so it can only be a new opt-in mode with its own recorded values.

**Where it runs, and where it does not.**

- Intel, bare metal or a VM with a virtual PMU, `perf_event_paranoid` 2 or lower: yes. Ubuntu and
  Debian kernels default to 4 (no unprivileged events at all), so many machines need a sysctl
  change by root.
- Hybrid Intel CPUs: the vCPU thread bound to one core type and that type's event (the engine
  already binds v3 vCPUs, `hybrid::choose`).
- AMD: rr's documentation says its retired-conditional-branch counting is not reliable on Zen
  unless the `SpecLockMap` optimization is disabled, a bit in MSR `0xc0011020` that needs root.
  A KVM fix from 2022 (by an rr developer) corrected how KVM mapped rr's AMD event for guests on
  Zen 3. So an AMD host would need root once per boot, and the exactness has not been checked here.
- In a cloud VM or a CI runner the host perf event counts on the VM's virtual PMU, which the
  provider may not offer. A 2016 report to the Xen developers says rr's counts were exact on bare
  metal and in KVM guests and off by a branch or two in a few percent of tests in Xen guests.
  Whether GitHub's hosted runners expose a usable PMU was not checked (this work did not use
  GitHub runners).

**Verdict.** Feasible on Intel hosts where an unprivileged user may count its own events, but the
exact stop needs a full x86-64 decoder with breakpoint stepping (the trap flag is not safe), the
clock is new (new fingerprints), and AMD, cloud VMs and default Ubuntu settings each need
something the engine cannot do by itself. It is the general answer (any exit-free code gets time),
not the next step.

## Option 2: the spin observer proves loops that store, by comparing the whole machine state

**Idea.** A polling loop like TigerBeetle's is an exact cycle of the whole machine, not of the
registers alone: every pass leaves the vCPU and RAM as they were. On one vCPU without an exit
nothing else writes RAM or touches the devices, so a pass that returns to the state it started
from will repeat forever. Proving that needs no decoder and no performance counter:

1. At a kick (no exit for `HYDO_KVM_SPIN_MS`), and only when the existing observer proved nothing,
   save the vCPU's whole state `s0` (registers, segment and control registers, the XSAVE image
   with the vector registers, the KVM-owned MSRs, pending events, debug registers) and turn on
   KVM's dirty-page log.
2. Put a hardware breakpoint on `s0`'s instruction and run. At each arrival whose whole vCPU state
   equals `s0`, read the dirty-page log (the pages written since the arrival before) and compare
   those pages with a copy taken at the arrival before. When every page written since was in the
   copy with the same bytes, RAM is what it was, and the machine state has repeated exactly.
3. Walk one period with trap-flag single steps and choose the canonical state: the smallest hash of
   the general registers among the states with interrupts enabled; when that hash occurs at more
   than one point of the period, the whole vCPU state and the pages the period writes decide. The
   canonical state is a function of the cycle alone, not of where the kick came. Step there; the
   clock jumps to the next event, as at an idle HLT, and the interrupt is delivered at the next
   entry, exactly as for the existing exact cycles.

**Why the trap flag is safe here.** The walk steps only through a proven cycle. A fault or an
exception cannot be inside it: the guest kernel's handlers count every fault in memory (the page
fault counters), so a pass with a fault would not repeat. The walk refuses the instructions that
save RFLAGS (PUSHF, POPF, IRET, INT, SYSCALL, SYSRET, SYSENTER, UD2) and runs over a SYSCALL to a
breakpoint at the next instruction instead of stepping it.

**How general.** It resolves any busy-wait whose passes leave the machine as they found it:
polling io_uring's completion queue, a flag in shared memory, a futex word, a ring buffer's head,
including loops that call functions, fill buffers with the same bytes, use AVX, or make system
calls that change nothing. It does not resolve a loop that counts in memory (a pass counter, a
statistics field, a back-off delay stored in memory): those need the rewind mode's counter proof
extended to memory (open). It gives up on a pass that writes more than 1024 pages, on a loop that
does not repeat within its time budget (a quarter of `HYDO_KVM_SPIN_SECS`, at most two seconds),
and on a period with two indistinguishable points.

**Cost.** Per resolved wait: the kick delay (5 ms by default), a few dozen breakpoint runs, two
dirty-log reads and page compares, and one to three walks of the period with single steps
(TigerBeetle's period is a few hundred instructions: about a millisecond). No cost when the mode is
off, and no cost for runs without exit-free loops.

**What it changes.** Nothing for a valid run: it acts only where the existing observer proved
nothing, and a loop it proves would have ended the run as SPIN. Its delivery point depends only on
the guest. It does not need anything of the host beyond what the engine already uses (the dirty-page
log and guest debugging), so it works the same on Intel and AMD.

**Verdict.** Feasible, small, and enough for TigerBeetle: prototyped (below).

## Option 3: help from the guest kernel

hydo builds its own guest kernel for KVM, so a patch could add exits, as the `sched_yield` patch
already does (a clock read, so a thread that yields sees the time move). But a kernel gets control
from user code only through a system call, a fault or an interrupt, and TigerBeetle's loop makes
none of them. What was considered:

- *An exit in a system call that polls* (`io_uring_enter` with nothing to submit and nothing to
  wait for): TigerBeetle's loop skips the system call in exactly that case, so it would not help.
- *Completing disk requests at once*: if the virtio disk completed a request at the exit of its
  notification, the interrupt would be delivered before the submitting system call returned, and
  the completion would be in the ring before the loop starts. This changes the device model for
  every run (every disk fingerprint), cannot model disk delay faults, and helps only waits for the
  disk, not for the network or for another process.
- *io_uring settings*: a kernel polling thread (`IORING_SETUP_SQPOLL`) needs another CPU to run on;
  deferred task work (`IORING_SETUP_DEFER_TASKRUN`) posts completions only inside `io_uring_enter`,
  which makes the problem worse. Either changes what the program does.
- *Rings that exit when read*: mapping io_uring's rings into user space through a page the engine
  does not back with RAM would turn every access into an MMIO exit. KVM's instruction emulator would
  then have to emulate the program's VEX loads and stores, which it does not, and it would apply to
  io_uring only.

**Verdict.** No general remedy: without an interrupt the guest kernel cannot preempt user code.

## Prior art

- **rr** (O'Callahan, Jones, Froyd, Huey, Noll, Partush, "Engineering Record And Replay For
  Deployability", USENIX ATC 2017; arXiv:1705.05937) counts a thread's retired conditional branches
  with a performance counter, uses the counter's interrupt to preempt threads, and reaches the
  exact recorded point in replay by stopping early and then running to breakpoints and stepping.
  Its documentation asks for `perf_event_paranoid` 1 when recording and for the `SpecLockMap`
  workaround on AMD Zen.
- **Hermit** (Meta, 2022) runs Linux programs deterministically with ptrace and preempts threads
  after a fixed number of retired conditional branches; an issue in its tracker shows a missed
  preemption that overshot by about ten million branches.
- **ReVirt** (Dunlap, King, Cinar, Basrai, Chen, OSDI 2002) replayed a virtual machine's
  asynchronous events at the same point by logging the instruction pointer together with a
  hardware branch count.
- **Weaver, Terpstra and Moore**, "Non-determinism and overcount on modern hardware performance
  counter implementations" (ISPASS 2013), measured run-to-run variation of counters that should be
  exact on eleven x86-64 implementations; Weaver's notes add that retired-branch counts include an
  extra event for every hardware interrupt or page fault, even when only user space is counted.
  The efficiency core's all-branches count above (N+30) shows the same effect; the
  conditional-branch events did not.
- **KVM and guest counters**: a 2016 report to the Xen developers by an rr developer says rr's
  counts are exact on bare metal and in KVM guests and inexact in Xen guests; a 2022 KVM patch
  fixed the event mapping rr needs in AMD Zen 3 guests; a 2026 KVM proposal (an RFC) lets user
  space stop KVM from adding the instructions it emulates to a guest's counters, which would make
  emulated instructions missing from such counts.
- **QEMU's `-icount`** makes the emulator (TCG, not KVM) count guest instructions and is what its
  record and replay mode needs.

## Recommendation

1. Take option 2 now. It is enough for TigerBeetle's single replica (prototype below), it changes
   no valid run, needs nothing of the host, and fits the engine's rule that exit-free loops are
   resolved at points the guest alone fixes. Before making it the default: review the soundness
   argument and run the recorded checks with it on (they should not change, since it only acts
   where a run would end as SPIN). It already ran on AMD (below).
2. Extend the counter proof to memory next (loops that keep a counter in memory) if a program needs
   it.
3. Keep option 1 as the general answer for later, as its own opt-in clock with its own values, and
   only after a breakpoint stepper over a full x86-64 decoder exists; measure AMD and a cloud VM
   first.
4. Option 3 is not worth doing.

## The prototype (`HYDO_KVM_SPIN_STATE=1`)

Option 2, opt-in and inert when off: `Vmm::observe_state` and `Vmm::state_walk` in
`hydo/crates/hydo-kvm/src/vmm.rs`, `Cpu::single_step` (`KVM_GUESTDBG_SINGLESTEP`) in `cpu.rs` and
`sys.rs`, `spin::saves_flags` and `spin::syscall_len`, and the variable in hydo-config's registry.
The design and its numbers are in docs/design/kvm-engine.md, "Loops that store: full-state cycles".
In short, on the build machine:

- TigerBeetle's single-replica image (`hydo-single.yaml`, 0.17.9, v3, 3072 MiB) ends
  `ERROR boot: SPIN` without the mode. With it, `disk=none | run` is OK with every command and
  invariant passing, `3a4bc40c537fa2e5` six times from fresh boots and once from the worker's S1,
  about 20 s per run, 205 cycles resolved per run. A kill, a power cut with its recovery boot, a
  pause and a clock step each gave one fingerprint twice.
- Two things were found on the way. First, `start`'s loop calls one `memset` twice from one frame,
  so the general registers alone did not single out a canonical state; the walk now tells such
  points apart by the whole vCPU state and the written pages. Second, the first runs gave two
  fingerprints that differed only in what KVM keeps of a finished page fault (its error code, with
  nothing injected or pending). KVM re-injects an exception when a VM exit interrupts its delivery,
  and the dirty-page log, turned on at host-timed kicks, makes such exits. With the mode on, the
  fingerprint leaves those leftovers out. The default fingerprint keeps them so that no recorded
  value moves; whether they can differ without the mode is open.
- With the mode off, `hv/kvm/kvm-check.sh` gave 16 of 16 with every value of Milestone 189's run.
- The performance counter adds nothing measurable to an exit: 1,000,000 exits took 1.06 µs each
  with the counter and 1.08 µs without on a performance core (1.95 and 1.91 µs on an efficiency
  core), so option 1's cost would be in its stops, not in normal running.
- On AMD (GitHub's `ubuntu-24.04` runners, EPYC 7763, 9V74 and 9V45, nested virtualization), the
  same image with the mode on gave `disk=none | run` OK `adf23d40c773219d`, a kill OK
  `abc2842bcb614ec0` and a power cut OK `37e02cc8895a8935`, each twice on every runner and the
  same on all three models, and the clean run from the worker's S1 gave the fresh boot's
  `adf23d40c773219d`; 120 to 200 s per run. With the mode off `kvm-check.sh` gave
  16 of 16 there. Details: docs/design/kvm-engine.md "On an AMD host".

## What a check on AMD would need

The prototype's check below was run on 2026-11-08 (the results above); option 1's was not.

- For option 2 (the prototype): a host with AMD-V and `/dev/kvm` for the user, AVX2 and AES-NI
  for TigerBeetle's binary, and the steps of docs/design/kvm-engine.md "On an AMD host" with this
  branch's `hydo`; then `hv/kvm/kvm-check.sh` with the mode off (16 of 16 expected, AMD's own
  values) and TigerBeetle's single-replica image twice with `HYDO_KVM_SPIN_STATE=1` (one
  fingerprint expected). Nothing in the mode is vendor-specific: KVM's guest debugging and
  dirty-page log exist on SVM too.
- For option 1: root once to set `perf_event_paranoid` to 2 or lower and to disable `SpecLockMap`
  (MSR `0xc0011020` bit 54, as rr's `zen_workaround.py` does), then the probe of this note with
  the event `ex_ret_cond` (`0xd1`): exactness over long runs, under preemption, and the skid. In a
  VM (a CI runner) the provider must expose a virtual PMU, and the MSR is usually not writable
  there.

## Sources

- rr: "Engineering Record And Replay For Deployability", https://arxiv.org/abs/1705.05937 ; "Will rr
  work on my system", https://github.com/rr-debugger/rr/wiki/Will-rr-work-on-my-system ; Zen,
  https://github.com/rr-debugger/rr/wiki/Zen
- Hermit: https://developers.facebook.com/blog/post/2022/11/22/hermit-deterministic-linux-testing/ ;
  issue 20, https://github.com/facebookexperimental/hermit/issues/20
- ReVirt: Dunlap et al., "ReVirt: Enabling Intrusion Analysis through Virtual-Machine Logging and
  Replay", OSDI 2002.
- Weaver, Terpstra, Moore, ISPASS 2013, doi:10.1109/ISPASS.2013.6557172 ; extended version
  https://web.eece.maine.edu/~vweaver/projects/deterministic/deterministic_counters.pdf ; retired
  branches, https://web.eece.maine.edu/~vweaver/projects/perf_counters/retired_branches.html
- Xen and rr, 2016: https://old-list-archives.xen.org/archives/html/xen-devel/2016-10/msg01288.html
- KVM, AMD Zen 3 event mapping (2022): https://www.lkml.org/lkml/2022/5/3/21 ; emulated
  instruction accounting (RFC, 2026): https://lkml.iu.edu/2607.2/08518.html
