# Several vCPUs on the KVM engine

Status: written on 2026-11-13. Stages 1 to 4 ran on GitHub's runners (Milestones 213, 219 and
226). The usefulness test of stage 4 passed: turn boundaries at exits reach the passing order of
the nats example. Stage 4 also found a determinism bug in the SMP guest kernel, which is fixed;
the fixed kernel gave one fingerprint per schedule on AMD and on Intel. The explorer draws KVM
turn lengths and searches them since Milestone 227. On Intel a run from a worker's S1 differed
from a fresh boot; the fix passed its proof run on Intel and AMD. The sections on what the probe and the runs showed
have the numbers. The
engine runs several vCPUs with `--vcpus N` and the pinned SMP guest kernel `bzImage-kvm-smp`; with
one vCPU it runs the code of before. It answers
AGENTS.md open item 45 ("The KVM engine: one vCPU only"). The engine it extends is
described in `docs/design/kvm-engine.md`; this page uses that page's terms (the exit clock, the
spin observer, the rewind mode, the full-state cycles) and does not repeat them.

## Why several vCPUs on KVM

The KVM engine runs hydo's x86_64 images on stock Linux KVM, without hydo's kernel module and
without root. Virtual machines in a cloud, CI runners, and AMD machines can run it, and hvx
cannot run on them. The engine gives the guest one vCPU (virtual CPU). Many concurrency bugs need two threads on two CPUs at once, and
many programs size their thread pools and runtimes by the CPU count.

The example `hydo/onboard/examples/nats-flaky-8363` shows the gap. On the KVM engine with one
vCPU, Go runs with one P (one scheduler slot). There, 209 schedules of the buggy test gave one
verdict, VIOLATION (its `RESULTS.md`, "On the KVM engine: one vCPU, always fails"). On hvx with two
vCPUs the verdict depends on the turn length: `q=18700` passes and `q=18800` fails, each twice
with one fingerprint (Milestone 205).

## The design in short

- **Serialized vCPUs.** The engine creates N KVM vCPUs in one VM and runs exactly one of them at a
  time, all from one host thread.
- **Turns of exits.** A vCPU runs until it has made `q` counted exits, then the next runnable vCPU
  runs. A turn also ends at HLT, at a spin-wait exit of the guest kernel, and at a spin the
  observer proves.
- **One clock.** Every counted exit of any vCPU advances the one VM clock by the step, as today.
- **A local APIC per vCPU in the engine.** The local APIC is the interrupt controller of one x86
  CPU, and x2APIC is its mode with registers as MSRs. The guest's x2APIC register accesses exit to the engine,
  which emulates the interrupts between CPUs (IPIs), the APIC timer on the exit clock, and INIT and STARTUP. KVM has no in-kernel
  interrupt controller, as today.
- **A second guest kernel**, `bzImage-kvm-smp`, built from the same recipe with `CONFIG_SMP=y` and
  a spin-wait exit in the kernel's `cpu_relax()`.
- **Opt-in and inert when off.** With one vCPU the engine runs the code, the kernel, and the
  CPUID table of today, so every recorded KVM fingerprint stays the same.

## One vCPU stays exactly as it is

Every recorded KVM value must stay: `hv/kvm/kvm-check.sh` (16 of 16 with its values), the KVM
entries of `hydo/onboard/recorded.json`, and the values of `docs/REBASELINE.md` section 19. The
rules that keep them:

- The vCPU count is an engine option, `hydo-kvm --vcpus N`. Without it, or with `--vcpus 1`, the
  engine never creates a second vCPU and never calls new code.
- The one-vCPU machine keeps the UP kernel `bzImage-kvm` (`d50fbc67…`), the CPUID table without an
  APIC, the boot without an MP table, and the 8259 PIC as the only interrupt controller.
- The new state (the other vCPUs, the local APICs, the scheduler) lives outside `Mach`, whose bytes
  the fingerprint hashes. A fingerprint adds that state only when N is more than 1.
- The fingerprint epoch (`hydo_image::EPOCH`, now 2) does not change, because no recorded value
  moves. Runs on several vCPUs are new values.

Before the first engine change, a test pins the scripted vCPU's fingerprints of
`hydo/crates/hydo-kvm/tests/engine.rs` as constants. A change to the one-vCPU path then fails a
test on any machine, the Mac included.

## Serialized vCPUs in turns of exits

**Only one vCPU runs at a time.** Parallel vCPUs interleave their memory accesses by host timing,
which no schedule can repeat. hvx serializes its vCPUs for the same reason (`hv/hvx/SMP_PLAN.md`).
While one vCPU runs, the others are stopped in user space, so every memory access in a turn comes
from the running vCPU or from a device at an exit.

**One host thread runs every vCPU.** `KVM_RUN` on a vCPU from a thread other than the one that ran
it last calls `synchronize_rcu()` (Linux 6.6, `virt/kvm/kvm_main.c`, `kvm_vcpu_ioctl`, the
`oldpid != task_pid(current)` branch). That call waits for a grace period, which costs host time
at every switch. KVM compares the pid per vCPU, so one thread that creates and runs every vCPU
never takes that branch after the first run. The engine thread then switches vCPUs by calling
`KVM_RUN` on another vCPU's file descriptor. The watchdog's kick already targets that thread
(`sys.rs`, `kicker`), and the binding of a v3 vCPU to one core type (`hybrid::choose`) covers
all vCPUs at once.

**A turn is a number of exits.** KVM gives user space no exact instruction counter, so a turn
cannot be a number of branches as on hvx. A turn of a vCPU ends at the loop top after its counted
exit number `q`, after the exit's instruction was completed (`Cpu::complete`). The point is a
function of the guest's execution alone. A switch needs no state copy, because every vCPU keeps
its own KVM vCPU. A vCPU in an interrupt shadow or with an event pending keeps that state in KVM
until its next turn.

**The scheduler's rules.**

1. Round-robin by vCPU index over the runnable vCPUs.
2. A vCPU is runnable unless it is halted with nothing deliverable, or waiting for STARTUP.
3. HLT ends the turn. The vCPU becomes runnable again when an interrupt is deliverable to it.
4. When no vCPU is runnable, the clock jumps to the earliest event of any vCPU or device. That is
   the idle rule of today (`Vmm::idle`) over all vCPUs.
5. A halted vCPU with interrupts off, and nothing that can wake another vCPU, ends the run as
   today's halted machine.

**The schedule tokens are hvx's.** `q=<exits>` sets the turn length, and `qs=<seed>` draws each
turn's length from `[1, q]` with hvx's rule (`hvx_sched_len`). On KVM the unit is exits, as the unit
of `tj=` is exits on KVM and branches on hvx. The starting default is 256 exits; stage 2 measures
the turns per boot and chooses the value. A one-vCPU engine refuses `q=` and `qs=`, as today
(`hydo_engine::refusal`).

## What a turn boundary can reach

A turn boundary falls only at a counted exit. A race whose window contains no exit cannot have a
boundary inside it. The nats example's window is "a few microseconds of Go runtime code with no
VM exit in it" (its `RESULTS.md`). This is the largest limit of the design, and stage 4 tests it.

Exits happen at the points where threads communicate, so several vCPUs still reach interleavings
that one vCPU cannot:

- A wake-up of a thread on another CPU sends a reschedule IPI, which is a write to the APIC's interrupt command
  register (ICR) and so an exit.
- Every clock read is an exit (`HYDO_PORT_CLOCK`), and Go's runtime reads the clock in its
  scheduler and timers.
- System calls that block reach the idle loop, which halts the vCPU.
- Kernel code that waits for another CPU makes a spin-wait exit (next section).

In the nats example, the passing order needs the other P to run the ticker goroutine before the
test goroutine counts. The test goroutine's `Unregister()` sends on a channel, and when the other
P's thread sleeps, the send wakes it with a futex call and a reschedule IPI. A turn that ends at
that IPI runs the other vCPU at the point the passing order needs. Stage 4 checks this argument
with the example's images.

hvx's exact turns come from the PMU branch clock. The KVM equivalent is a host performance counter
on the vCPU thread with `exclude_host`. `hydo/explore/RESEARCH-kvm-exit-free.md` (option 1)
measured it, and it counts exactly on Intel. It needs `perf_event_paranoid` 2 or lower, a breakpoint stepper
over a full x86-64 decoder, root on AMD, and a virtual PMU in a cloud VM. That stays a later,
separate opt-in clock, and this design does not depend on it.

## Waits for another virtual CPU

**The problem.** On one vCPU nothing can change memory during a stretch without exits, so the spin
observer can prove a loop infinite. With serialized vCPUs, the vCPU a spinning vCPU waits for does
not run until the turn ends. A turn that ends only at exits never ends for a loop without exits.
So every wait for another CPU must make an exit or be proven by the observer.

**Pause-loop exiting cannot help.** KVM handles a PAUSE-loop exit in the kernel: `handle_pause`
(Linux 6.6, `arch/x86/kvm/vmx/vmx.c`) calls `kvm_vcpu_on_spin` and returns to the guest. No such
exit reaches user space. Its window is counted in host cycles, so its timing is the host's, as hvx
found (`hv/hvx/SMP_PLAN.md`). The same function's comment states that the processor ignores
pause-loop exiting at CPL > 0, so it never applies to user-space spins.
`KVM_CAP_X86_DISABLE_EXITS` with `KVM_X86_DISABLE_EXITS_PAUSE` only turns these exits off.

**Spin-waits in the guest kernel exit.** The SMP guest kernel's `cpu_relax()` executes an OUT to a
new port, `HYDO_PORT_RELAX` (`0x5f0`), instead of PAUSE. Every kernel wait loop uses `cpu_relax()`
on x86:

- the queued spinlock's waiters, through `smp_cond_load_acquire`;
- `csd_lock_wait` of a cross-CPU function call, which a TLB shootdown also uses;
- `multi_cpu_stop` of `stop_machine`;
- seqcount readers and the queued rwlock.

A relax exit is a counted exit and ends the turn at once. The waiting vCPU gives the CPU to the
vCPU it waits for, at a point the guest alone fixes. The vDSO is built from the same header and
runs in user space, where an OUT faults, so the change is compiled only outside `BUILD_VDSO`.

**Paravirtual spinlock waits are a later improvement.** The queued spinlock has paravirtual hooks
(`pv_wait` halts a waiter, `pv_kick` wakes it), which KVM's own guest support uses. With them a
waiter would stop running until the lock holder kicks it. The relax exit already makes the wait
correct, so this is a cost measure for stage 5.

**`vcpu_is_preempted` stays native.** Returning true for every other CPU would stop the mutex's
optimistic spinning. It would also make the scheduler treat every idle CPU as busy
(`available_idle_cpu`), so woken tasks would stay on the waker's CPU, and there would be less
parallelism to test.

**Spin-waits in user space go to the spin observer.** User code cannot be patched, and PAUSE makes
no exit there. Most user-space waits are bounded: glibc's adaptive mutex, Go's `procyield`, and
Rust's `std` mutex spin a few hundred times at most. Then they call `futex` or `sched_yield`, which
reach an exit (the idle HLT, or the clock read of the `sched_yield` patch). An unbounded wait is
the spin observer's case, as on one vCPU. The proofs stay valid with serialized vCPUs, because
during the running vCPU's turn nothing else writes memory. The proven loop would run forever in
that turn, so the resolution ends the turn:

1. The observer steps the vCPU to the loop's canonical state, as today.
2. When another vCPU is runnable, the turn ends there, without a clock jump.
3. When no other vCPU is runnable, the clock jumps to the next event, as today.
4. When nothing can end the loop, the run ends as a deadlock, as today.

The canonical state depends on the loop alone, so the switch point does not depend on when the
kick came. The cost is the kick delay (`HYDO_KVM_SPIN_MS`, 5 ms of host time) at every turn in which
the loop still spins. Stage 3 measures it and sets a shorter delay for several vCPUs if needed.

**Counter loops and the rewind mode.** A loop that counts while it waits (`while (!flag) n++`)
needs the rewind mode (Milestone 161). The mode replays from a checkpoint to the exit count where
the loop was found. With several vCPUs, a checkpoint takes every vCPU and the scheduler, and the
replay is the same run, so the rule carries over. Stage 5 builds it. Until then, a proven counter
loop on several vCPUs ends the run as SPIN (invalid), and the host's run again with `spin=rewind` is
refused for several vCPUs.

## The interrupt controller: a local APIC per vCPU in the engine

**What an SMP Linux guest needs.** Linux on x86 needs a local APIC for SMP: for IPIs between CPUs,
for a timer per CPU, and for INIT and STARTUP to start the other CPUs. The one-vCPU engine hides
the APIC and uses the 8259 PIC.

**Decision: emulate the x2APIC in the engine; create no in-kernel interrupt controller.** The
evidence that KVM sends the accesses to user space (Linux 6.6, `arch/x86/kvm`):

- `kvm_msr_allowed` lets every x2APIC MSR (`0x800..0x8ff`) through the MSR filter.
- `kvm_x2apic_msr_read` and `kvm_x2apic_msr_write` return 1 when `!lapic_in_kernel(vcpu)`. The
  fast path for ICR writes returns early for the same reason.
- `kvm_msr_reason` turns that 1 into `KVM_MSR_EXIT_REASON_INVAL`. The engine already enables that
  reason (`sys.rs`, `KVM_CAP_X86_USER_SPACE_MSR` with `INVAL | UNKNOWN | FILTER`), so the access
  exits to the engine as `KVM_EXIT_X86_RDMSR` or `KVM_EXIT_X86_WRMSR`.
- `kvm_set_cpu_caps` (`cpuid.c`) always offers X2APIC ("KVM emulates x2apic in software
  irrespective of host support"), so the fixed CPUID table can include it on every host.
- `__kvm_emulate_halt` returns HLT to user space when the APIC is not in the kernel, as today.
- `KVM_INTERRUPT` and `KVM_NMI` inject into any vCPU of a VM without an in-kernel irqchip.

Stage 1's probe checks each of these on KVM before engine code depends on them.

**The model per vCPU.** Each vCPU's x2APIC has the request register (IRR, the vectors waiting)
and the in-service register (ISR, the vectors being handled). It also has the task and processor
priorities (TPR, PPR), the local vector table, the spurious-vector register, and the timer
(initial count, divide, one-shot and periodic modes). The APIC timer counts in the exit clock with
a bus clock of 1 GHz, so its current count is a function of the guest's execution.

The interrupt rule of today runs per vCPU at its loop top. The engine
injects the highest deliverable vector with `KVM_INTERRUPT` when KVM says the vCPU can take it,
and else it asks for the interrupt window. An APIC vector moves from the IRR to the ISR at its
injection, and an EOI clears the highest ISR bit. TSC-deadline mode stays hidden in CPUID.

**IPIs.** An ICR write with a fixed vector sets the target's IRR. The target takes the vector at
its next turn under the delivery rule, because it is not running. A self-IPI is taken at the next
entry of the sender. The engine honours physical destinations and the three shorthands. Linux runs
in physical x2APIC mode, so a logical destination ends the run with a message. NMI IPIs use
`KVM_NMI`; Linux sends them only for backtraces and panics.

**Device interrupts.** In stages 1 to 4 the 8259 PIC stays the only device interrupt controller.
Its output goes to the boot CPU's local interrupt pin LINT0, set to take the PIC's vector (ExtINT).
This is "virtual wire" mode, and it sends every device interrupt to vCPU 0. Linux sets LINT0 to
ExtINT on the boot CPU only when the I/O APIC is turned off, in `setup_local_APIC`.
With an MP table and the masked LINT0 of the reset state, it keeps LINT0 masked. So the SMP
kernel's command line has `noapic`.

An APIC vector of vCPU 0 is taken before the PIC's vector, as QEMU's user-space APIC does
(`cpu_get_pic_interrupt`). An emulated I/O APIC that routes a device to any vCPU is stage 5.

**Rejected: KVM's in-kernel local APIC** (`KVM_CAP_SPLIT_IRQCHIP`). It handles IPIs and EOIs
without exits, but:

- HLT stays in the kernel, so a halted vCPU blocks until a host-timed wake-up, and the engine
  would need a guest-kernel idle port to know about it.
- Its APIC timer runs on host time.
- KVM decides when a pending interrupt is taken, with APICv or AVIC where the host has them.
  Nested runners and bare metal then take different code paths.
- Snapshots would need `KVM_GET_LAPIC`, whose timer fields are host time.

With the APIC in the engine, every interrupt decision is made at a counted exit, the rule
`docs/design/kvm-engine.md` keeps for one vCPU.

**Fallback: the xAPIC page.** If a host's KVM does not send x2APIC accesses to user space, the
guest can use the xAPIC registers at `0xfee00000` instead. KVM sends every access to that page to
user space as MMIO when the APIC is not in the kernel. Each access then goes through KVM's
instruction emulator, which costs more per exit.

**What comes from hvx.** hvx's module is C in the kernel, so no code is shared. The rules are
taken over from `hv/hvx/hvx.c` and Milestone 69:

- only vCPU 0 has the boot-CPU bit (BSP) in `IA32_APIC_BASE`;
- `IA32_APIC_BASE` stays latched in x2APIC mode, so Linux never falls back to the xAPIC page;
- INIT puts a vCPU in the wait for STARTUP and resets its APIC, keeping the ID and the base;
- STARTUP with vector V starts a waiting vCPU in real mode at `CS = V << 8`, `IP = 0`, and a second
  STARTUP is ignored (`hvx_ap_sipi`);
- the vCPU index enters the tick-jitter hash as `index * salt`, which is 0 for vCPU 0.

hvx's model has one in-service slot and no priorities. The engine's model has the full IRR and ISR,
because Linux's IPIs and the APIC timer can be pending together on one vCPU.

## What the guest sees

**CPUID per vCPU.** Each vCPU gets its own table (`KVM_SET_CPUID2` per vCPU), the one-vCPU table
plus, for N more than 1 only:

- the APIC and x2APIC bits in leaf 1;
- the initial APIC ID (leaf 1 EBX bits 31:24) and HTT;
- leaf 0xB with one thread per core and N cores;
- ARAT in leaf 6 (the APIC timer keeps running when idle, so Linux needs no broadcast timer);
- the cache leaves of `guest.cpu_cache` with the L1 and L2 per core and the L3 shared by N cores.

KVM sets the APIC bit of leaf 1 from the vCPU's `IA32_APIC_BASE` enable bit
(`kvm_update_cpuid_runtime`, `kvm_lapic_set_base`). The one-vCPU engine keeps the base at 0 for
that reason. With N more than 1 the engine sets the base through `KVM_SET_SREGS`, and every
guest access to the MSR goes to the engine's model.

**The MP table.** `hydo_hvx::boot::build_mptable_n` already writes a table with N processor
entries, and the KVM boot erases it today. With N more than 1, the boot writes the table without
its I/O APIC and interrupt entries. Linux then registers no I/O APIC and reads no I/O APIC
register, and `noapic` puts the PIC on the boot CPU's LINT0. The kernel runs `acpi=off`, as on
hvx.

**Starting the other vCPUs.** Linux starts each other CPU (an AP) with INIT and two STARTUP
IPIs. The engine
handles both: vCPU j does not run until a STARTUP, then the engine loads the real-mode state of
`hvx_ap_sipi` with `KVM_SET_REGS` and `KVM_SET_SREGS`. KVM runs real mode with unrestricted guest
on Intel and always on AMD; the engine already sets the TSS for the other case. KVM's own
`KVM_SET_MP_STATE` refuses `SIPI_RECEIVED` without an in-kernel APIC
(`kvm_arch_vcpu_ioctl_set_mpstate`), so the wait for STARTUP is the scheduler's state, not KVM's.
An INIT to a vCPU that already ran (CPU hot-unplug) ends the run with a message.

## The guest kernel `bzImage-kvm-smp`

The SMP kernel is built by `hv/kvm/linux/kernel-smp.nix`, which overrides the UP recipe
`kernel.nix` without changing it, so the UP kernel's pin stays. `SMP=1 sh
hv/kvm/linux/build-kernel.sh` builds it.

Configuration: `hv/kvm/linux/hydo-kvm-smp.config` sets `CONFIG_SMP=y`, `CONFIG_NR_CPUS=8`,
`CONFIG_X86_X2APIC=y`, and the MP table parser on top of `hydo-kvm.config`, and there is no
ACPI. The command line adds `x2apic_phys noapic`.

`hv/kvm/linux/hydo-kvm-smp.patch` applies after `hydo-kvm-guest.patch`. Each change is under
`CONFIG_SMP`:

- `cpu_relax()` executes the OUT to the relax port (`asm/vdso/processor.h`).
- `lapic_timer_period` is preset to the 1 GHz bus clock, so Linux skips the APIC timer
  calibration, which would wait without an exit (`calibrate_APIC_clock`: "lapic timer already
  calibrated"). VMware's and Jailhouse's guest support preset it the same way.
- The hydo platform's `x2apic_available` returns true, so Linux uses x2APIC without interrupt
  remapping (`try_to_enable_x2apic`).
- The one-shot timer on IRQ 0 is not registered, so there is no global clock event. Every CPU
  uses its APIC timer from `setup_percpu_clockev`, as a Jailhouse guest does.
- `cpu_init()` sets CR4.TSD again right after it clears TSD, so user-space RDTSC faults on every
  CPU and returns the exit clock. Without it, a program's RDTSC on another CPU read the host's TSC
  (the section "The determinism bug").

The engine's kernel table (`hydo_kvm::kernel::TWINS`) pairs the image's recorded hvx kernel with
both KVM builds: the UP build for one vCPU and the SMP build for several. Images do not change.
The pin follows `docs/BUILD-PINS.md`: two sandboxed builds with the same bytes, and
`hv/kvm/linux/audit-rdtsc.sh` on the new `vmlinux`. The release ships `guest/bzImage-kvm-smp`
next to `guest/bzImage-kvm`.

## Snapshots, the spin observer's modes, and fingerprints

**Snapshots.** `XSnap` keeps vCPU 0 as today. For N more than 1 it adds each other vCPU's
`CpuState` and its `kvm_run` flags (Milestone 175's fix, per vCPU), each vCPU's APIC and engine MSR
model, and the scheduler state. The scheduler state is the current vCPU, the exits left in its turn,
the turn count, and each vCPU's run state. A restore creates a fresh VM with N vCPUs and loads each
of them. Since Milestone 227 the machine that takes a snapshot goes on in a fresh VM with the
snapshot's states loaded, too, so a fresh boot and a run from S1 start from the same KVM state
(section "Stage 4: the explorer and Intel").

**The full-state cycles** (Milestone 194) save and compare the running vCPU's whole state and the
VM's dirty pages. During one turn the other vCPUs are stopped, so the proof holds as on one vCPU.

**Fingerprints.** For N more than 1 the fingerprint adds, after today's parts:

1. N;
2. vCPUs 1 to N-1, without KVM's stale event fields (Milestone 204);
3. every vCPU's APIC;
4. the scheduler state.

The turn policy (`q=`, `qs=`) is left out, as the tick-jitter policy is. A run on one vCPU hashes
nothing new.

**The state search's timing.** The state search (`docs/design/state-search.md`, on its own branch)
makes a SNAP keep the run's tick jitter and preemption points, and adds TIME, which replaces the
timing at a workload stop. The turn policy is timing of the same kind. A SNAP keeps it with the
other timing, and TIME can carry `q=` and `qs=` for an engine with several vCPUs. Stage 4 does this
after the state search is merged.

## Cost

These numbers are known today:

- One exit costs 1.06 µs on the build machine's performance core
  (`hydo/explore/RESEARCH-kvm-exit-free.md`).
- On GitHub's runners, nested in a VM, TigerBeetle's single replica took 120 to 200 s per run
  against about 20 s on the build machine (`docs/design/kvm-engine.md`, "On an AMD host").
- On hvx a turn end costs about 0.8 ms, almost all of it the exact stop (Milestone 74). A KVM switch
  makes no exact stop and copies no state.

A switch on KVM costs one `KVM_RUN` on another vCPU, which loads another VMCS or VMCB in the host
kernel. Stage 1 measured it on GitHub's runners: 21 to 124 ns over an exit on AMD. On Intel it
was 2,467 ns, where an exit costs 13.6 µs (the section on what the probe showed). An exit there
costs 8 to 14 µs, against 1.06 µs on the build machine, so a switch adds at most a fifth of an exit.

Each APIC access is an exit: an EOI per interrupt, an initial-count write per timer arm, and an ICR
write per IPI. The UP kernel already makes about as many exits for the PIC's EOI and the timer port.

Two costs grow with several vCPUs, and stage 3 measures both:

- The guest kernel flushes the TLB at every change of a present page-table entry
  (`docs/design/kvm-engine.md`). For a process with threads on several CPUs, each flush is a
  shootdown IPI, a turn switch to every target, and relax exits in the sender.
- A user-space spin that the observer resolves costs the kick delay in every turn that finds it
  still spinning.

## Risks

1. **Turns only at exits.** A race window without an exit cannot be reached by a turn boundary.
   If the nats example's passing order is not reachable (stage 4), several vCPUs on KVM test SMP
   kernel paths and CPU-count-dependent programs, but miss races in pure computation.
2. **A KVM behaviour this design reads from the source but has not run.** This covers the x2APIC MSR
   exits, the STARTUP to real mode from user space, and one thread running several vCPUs. Stage 1
   runs each on Intel and AMD hosts before engine code depends on it.
3. **Host-dependent state that a switch exposes.** A vCPU that did not run for a while has the TLB
   the host left it. The guest-kernel flushes cover the effects the guest can count: spurious
   faults and accessed bits. With several vCPUs these flushes become shootdowns, and the
   fingerprints of repeated runs are the test.
4. **Cost.** Relax exits, shootdowns, and spin kicks can make runs slow. Stage 3 measures a boot
   and an example's run against one vCPU.
5. **Spin observer coverage.** An unbounded user-space wait that the observer cannot prove (it
   counts in memory, or its pass writes more than 1024 pages) ends the run as SPIN, as on one vCPU.

## How determinism is proved

Each claim has its test:

- The same schedule on N vCPUs gives one fingerprint on repeated runs: every check of stages 2 and 3
  runs twice, also under host CPU load.
- A run from a snapshot equals the straight run. Stage 1 takes a snapshot at every turn boundary of
  a hand-written guest. Stage 3 compares the explorer's S1 with a fresh boot and repeats the test of
  Milestone 175 (`a_run_from_s1_equals_a_fresh_boot_wherever_s1_is_taken`) on several vCPUs.
- The turn length moves the interleaving reproducibly: a two-vCPU race guest whose lost updates
  are a function of `q`, each `q` from 1 to 200 twice with one result.
- Hosts of one CPU vendor agree: the same checks on each GitHub runner model seen (EPYC 7763, 9V74,
  and 9V45 in Milestone 194) give one value per check. Across vendors only verdicts compare, as for
  one vCPU.
- One vCPU is unchanged: the pinned scripted fingerprints, `kvm-check.sh` 16 of 16, and the KVM
  entries of `recorded.json` with `hydo-bench rebaseline --check`.
- No vCPU reads the host's TSC: with several vCPUs, every fingerprint checks that each started
  vCPU has CR4.TSD set when vCPU 0 has it, and the run ends otherwise. A check at the end of a turn
  would not do it, because user code seldom makes an exit.

## Testing without the build machine

The build machine is the user's laptop and is not always reachable. The bare-metal machine must
never run KVM (AGENTS.md). Two places remain:

- **This Mac** builds the engine and runs every test on the scripted vCPU (`cargo test -p
  hydo-kvm`): the scheduler, the APIC model, snapshots, and fingerprints.
- **GitHub's `ubuntu-24.04` runners** have `/dev/kvm` on AMD EPYC with nested virtualization
  (Milestones 194 and 210). A one-off workflow on the branch `tmp-kvm-vcpus` runs the KVM checks.
  The lead pushes the branch, and it is deleted after its runs, as `tmp-cargo-linux` was in
  Milestone 210. The workflow gives the runner's user `/dev/kvm` with the udev rule of
  `docs/design/kvm-engine.md` ("On an AMD host") and keeps every log as an artifact.

The SMP kernel is built on the runner with Nix's multi-user installer, which builds in a sandbox, as
a pin requires. A second job rebuilds it with `nix-build --check`. The bare-metal machine has Nix
(Milestone 140) and can build a kernel for a quick trial, but its single-user Nix has no sandbox,
so its output is never the pin.

Intel KVM is checked when the build machine is reachable again. A runner is AMD, and Intel's VMX
paths (unrestricted guest, the VMCS switch) differ from AMD's SVM paths.

## Stage 1: what the probe showed

`hv/kvm/smp/smpprobe.c` ran on 2026-11-13 in four jobs of a one-off workflow on GitHub's
`ubuntu-24.04` runners: https://github.com/kakoc/hydo/actions/runs/43407838670. The hosts were an
AMD EPYC 7763, two AMD EPYC 9V74, and an Intel Xeon 6975P-C. Each had 4 vCPUs, nested KVM, and
Linux 7.0.0 (Azure and AWS kernels). Each job ran the probe twice and its race test once more
under four busy loops.

Every job passed 17 of 17 checks in both runs, and 4 of 4 under load. The values were the same on
every host, the Intel one included:

| check | result on every job |
|---|---|
| `x2apic` | the read of `0x802` and the writes of the ICR (`0x830`) and the EOI exit to user space with their index. Leaf 1 has the x2APIC and APIC bits. A read of `0xfee00030` is an MMIO exit |
| `sipi` | one INIT and one STARTUP taken, the second STARTUP ignored; vCPU 1 makes its real-mode exit (port `0x20`), then reaches protected mode (CS `0x08`, CR0 `0x60000011`) |
| `ipi` | the handler ran once, after one `KVM_INTERRUPT` and no window exit; `d5862710e31e565b` in both runs |
| `race` | every exit of every run equals the model's. Each `q` from 1 to 200 gave one hash twice; grid hash `9ec051b85c9f3a4b`. The 100 odd turn lengths lose updates (`q=1` loses 200, `q=3` 133, `q=5` 120). 206 runs from snapshots in fresh VMs equal their straight runs |

The grid hash covers the counter and the vCPUs' general registers, which have no
vendor-dependent values in this guest, so it matches across vendors. Linux guests' fingerprints
still differ between vendors, as on one vCPU.

The cost of an exit, in ns, from the first run of each job (the probe's `COST` line):

| host | one vCPU | two vCPUs, a switch at every exit, one thread | two threads, a handover at every exit | one vCPU run by two threads in turn |
|---|---|---|---|---|
| EPYC 7763 | 9,880 | 10,004 | 19,891 | 25,446 |
| EPYC 9V74 | 8,213 | 8,234 | 16,744 | 18,318 |
| EPYC 9V74 | 8,274 | 8,339 | 16,385 | 19,147 |
| Xeon 6975P-C | 13,616 | 16,083 | 24,234 | 40,237 |

The decision for one thread holds: a handover between two threads costs about one exit more,
and a vCPU that changes threads costs more again. The runners' kernels are newer than Linux 6.6, so
the cost of `synchronize_rcu()` there was not measured on its own.

## Stages 2 and 3: what the runs showed

Stage 2 is the engine (`hydo/crates/hydo-kvm/src/smp.rs`, `vmm.rs`, `serve.rs`, `sys.rs`). Stage 3
is the SMP guest kernel (`hv/kvm/linux/kernel-smp.nix`), pinned in docs/BUILD-PINS.md. On this
Mac the scripted tests of `hydo/crates/hydo-kvm/tests/engine.rs` pass. Two scripted vCPUs started
by INIT and STARTUP give one fingerprint per turn length, and other turn lengths give other
interleavings. A run from S1 equals a fresh boot at each of 39 boot stops. The one-vCPU pins
pass after every change.

A one-off workflow ran it on 2026-11-13 in four jobs on GitHub's `ubuntu-24.04` runners:
https://github.com/kakoc/hydo/actions/runs/43440090997. The hosts were AMD EPYC 9V45, 9V74, 7763,
and 9V45 again. Each had nested KVM and Linux 7.0.0. Every job passed.

One vCPU, with this branch's `hydo` (two jobs):

- `hv/kvm/kvm-check.sh` passed 15 of 15 checks on both hosts. The release's Redis example gave the
  AMD values Milestone 210 recorded: `2846084b7f0ab1e2` (OK) and `bca22e72dbf0da18` (VIOLATION,
  key k:1 lost).
- A timing branch of the state search (Milestone 212), `disk=none dseed=1
  pct=ec699251106e8e0c pctn=40 pctk=3000 br=40000000 pct=5be2f1a09c3d7e44 pctn=40 pctk=3000 | run`,
  gave `2039c438b0971596` twice from a fresh boot and once from the worker's S1. Its control
  without `br=` gave the same value. The control transcripts differ, so the controller sent
  TIME, but the new preemption points changed nothing the fingerprint sees.

Several vCPUs, with the SMP kernel built on the same runner (two jobs, EPYC 7763 and 9V45). Each
schedule ran twice from a fresh boot, and both hosts gave the same fingerprints:

| vCPUs | schedule | verdict | fp | turns (`qn=`) |
|---|---|---|---|---|
| 2 | `disk=none dseed=1 \| run` | OK, acked=20 | `cb3f16360593598e` | 231 |
| 2 | `disk=none dseed=1 q=64 \| run` | OK | `2e44e4e7d6c7173a` | 399 |
| 2 | `disk=none dseed=1 q=7 qs=1f \| run` | OK | `65afef462b5bb2cb` | 2,476 |
| 2 | `disk=cut \| run C=12 R=everysec` | VIOLATION, key k:1 lost | `25b2b20d93cdf8fc` | 164 |
| 4 | `disk=none dseed=1 \| run` | OK | `5ad6cb7c9b3bd84a` | 503 |

These values were made with the first SMP kernel, which had the determinism bug of stage 4. The
section "The determinism bug" has the values of the fixed kernel.

Linux used the second vCPU: in the default two-vCPU run vCPU 1 made 6,396 of the workload's
26,560 counted exits (the engine's `HYDO_KVM_DIAG` statistics). The one failed check was the run
from the worker's S1: `hydo explore` refused `--vcpus` for KVM. Milestone 219 lets the explorer
pass the count, and the next runner run checks S1 against a fresh boot there.

## Stage 4: the nats example

A third workflow run (https://github.com/kakoc/hydo/actions/runs/43446326976, AMD EPYC 7763 and
9V74) finished stage 3. On both hosts `hv/kvm/smp/smp-check.sh` passed 13 of 13 checks. The run
from the worker's S1 on 2 vCPUs gave the fresh boot's `cb3f16360593598e`. An exploration of 8
seeds on 2 vCPUs made 27 runs, and `hydo repro` of it reproduced 6 of 6 with the recorded
fingerprint.

The usefulness test was `hv/kvm/smp/nats-check.sh`: nats-server's flaky test
(`hydo/onboard/examples/nats-flaky-8363`) on 2 vCPUs over 28 turn settings. Risk 1 did not hold
here: 10 settings pass and 18 fail, with one verdict per setting on both hosts (the example's
`RESULTS.md` has the table). One vCPU fails every time, and the fixed test passes on 2 vCPUs. So a
turn boundary at the exit before the race window, here the reschedule IPI or a clock read, lets
the other vCPU's thread run in time.

These numbers were made with the first SMP kernel and are not valid, as the next section shows.
With the fixed kernel, 7 settings pass and 21 fail (the example's `RESULTS.md`).

### The determinism bug

In the stage 4 run, `q=12`, `q=1024`, `q=2048` and `q=4096` gave one fingerprint per host. A repeat
run (https://github.com/kakoc/hydo/actions/runs/43464674850) ran `q=12`, `q=1024` and `q=2048`
three times each, and each gave three fingerprints in three runs on every host. So it was a
determinism bug, not a difference between CPU models.

How it was found:

1. RAM dumps of two runs of `q=12` (`hv/kvm/smp/nats-diff.sh`) differed in one 8-byte word. Its
   values had the size of the guest's TSC after some seconds. Every exit, register, and APIC was
   equal, so the guest stored the value without branching on it.
2. A temporary watch in the engine logged each change of that word with the exit, the vCPU, and
   RIP. The first change that differs came at the same exit on every host, on vCPU 1. The word
   next to it held a clock reading in nanoseconds, as in the pair of a TSC reading and a clock
   reading that Go's runtime keeps.
3. More or fewer looks of the spin observer, and the engine pinned to one host CPU, changed
   nothing.

The cause: the guest patch makes user-space RDTSC fault, so that the kernel returns the exit clock.
It sets CR4.TSD on the boot CPU only. Linux drops TSD twice on every other CPU: in
`secondary_startup_64` (`head_64.S`), which loads CR4 with MCE, PAE and PGE only, and in
`cpu_init()`, which clears it. KVM does not trap RDTSC, so a program on vCPU 1 read the host's
TSC. One vCPU never had the bug. The fix sets TSD again in `cpu_init()`, after its clear. Two
earlier tries, TSD in the trampoline's CR4 and TSD in `cr4_init()`, had no effect, because
`head_64.S` and `cpu_init()` came after them. Milestone 219 in `docs/HISTORY.md` has the runs.

With the fixed kernel (`b40b2711…`, docs/BUILD-PINS.md), run
https://github.com/kakoc/hydo/actions/runs/43496814855 on AMD EPYC 9V45 and two EPYC 7763 gave
these results, the same on all three hosts:

- `smp-check.sh` 13 of 13, `kvm-check.sh` 15 of 15.
- `nats-check.sh`: 31 settings, three runs each, one fingerprint per setting.
- The Redis example on several vCPUs, each schedule twice:

| vCPUs | schedule | verdict | fp | turns (`qn=`) |
|---|---|---|---|---|
| 2 | `disk=none dseed=1 \| run` | OK, acked=20 | `851da6841d14cbad` | 229 |
| 2 | `disk=none dseed=1 q=64 \| run` | OK | `cc2a072b654ecfe2` | 432 |
| 2 | `disk=none dseed=1 q=7 qs=1f \| run` | OK | `dcb22e9bc56af461` | 2,407 |
| 2 | `disk=cut \| run C=12 R=everysec` | VIOLATION, key k:1 lost | `c3c57641abcec195` | 159 |
| 4 | `disk=none dseed=1 \| run` | OK | `b5fab129b81cb213` | 504 |

The earlier runs on Intel had the bug. The fixed kernel ran on Intel in Milestone 227 (next
section).

**A fix found while reviewing the spin paths.** On one vCPU, a proven cycle with interrupts off is a
deadlock, and the run ends. With several vCPUs, another vCPU can release such a vCPU. Since
Milestone 219, with several vCPUs such a cycle ends the turn at its smallest state when another
vCPU can run. A cycle with a state that has interrupts on is resolved as before, so no value of
these runs can move with the fix.

### Stage 4: the explorer and Intel

Since Milestone 227, `hydo explore --backend kvm --vcpus N --turns` draws `q=` and `qs=`. It uses
hvx's stream with lengths in exits: a third 1 to 32, a third 33 to 255, and the rest 128 to 512.
`--sweep turns` searches 1 to 4,096 exits and narrows a boundary to one exit. Run
https://github.com/kakoc/hydo/actions/runs/43527210060 ran both on the nats example on AMD EPYC
9V74 and 7763, with the same results on both hosts:

- the exploration of seeds 1 to 40 gave 8 passes and 32 failures, and `hydo repro` gave 33 of 33;
- the sweep found boundaries between `q=15` and `q=16` and between `q=39` and `q=40`, each
  length with one fingerprint twice.

The same run ran `nats-check.sh` with `REPEAT=3` on six hosts: an Intel Xeon 6975P-C and five AMD
hosts. Every one of the 31 settings gave one fingerprint on every host, with the same verdicts
everywhere. `smp-check.sh` passed 13 of 13 on the AMD hosts and 11 of 13 on the Intel host. There
a run from the explorer worker's S1 on 2 vCPUs ended with `d25d8c657ebe3746` and the fresh boot
with `336869d674c85757`. The verdict, the turn count and the control transcript were the same.
One vCPU from S1 agreed with the fresh boot there.

A fresh boot runs on in the VM that took S1, and a run from S1 starts in a fresh VM with the saved
states loaded. On Intel, with several vCPUs, a load did not give back all of the state that the
fingerprint hashes. Since Milestone 227 the machine that takes a snapshot with several vCPUs goes
on in a fresh VM with the snapshot's states loaded, as a run from it does. The engine also prints
each part of a vCPU's state that reads back changed after a load. With one vCPU nothing changed.
The fix passed on Intel and AMD: run 43535344097: on eight hosts (five Intel: Xeon 6975P-C and Xeon Platinum 8573C; three AMD: EPYC 9V74 and 7763) smp-check passed 13 of 13, turns-check 7 of 7 and kvm-check 15 of 15; the engine's read-back lines name the field on Intel only: the hidden attributes of unusable null segments (DS, FS), which Intel's VMX normalises in a load.

## The plan in stages

**Stage 1: the KVM probe** (no change to `hydo-kvm`; done, GO). A C program `hv/kvm/smp/smpprobe.c`, in the
manner of `hv/kvm/pmu/pmuprobe.c`, runs hand-written guests on two vCPUs with no in-kernel irqchip.
Its tests:

1. `x2apic`: x2APIC reads and writes exit to user space with their index, and leaf 1 shows APIC
   and x2APIC.
2. `sipi`: vCPU 0 writes INIT and STARTUP to the ICR, and the probe starts vCPU 1 in real mode at
   the vector's page. vCPU 1 reaches protected mode.
3. `ipi`: an ICR write from vCPU 0 is injected into vCPU 1 with `KVM_INTERRUPT` at its next turn,
   and vCPU 1's handler runs once.
4. `race`: two vCPUs increment a shared counter with an exit inside the read-modify-write, in turns
   of `q` exits, for `q` from 1 to 200. Each `q` runs twice, and a snapshot at every seventh turn,
   restored into a fresh VM, gives the straight run's hash.
5. `cost`: exits per second on one vCPU, and with a switch at every exit, on one thread.

Go or no-go after stage 1, on at least two runner jobs:

- Go when tests 1 to 4 pass with one hash per `q` on every job and CPU model, and a switch costs
  less than 20 µs on a runner.
- If test 1 fails, the fallback is the xAPIC page, tested by the same probe.
- If test 2 or 4 fails, or if a switch costs more than 20 µs, the work stops before any engine
  code, and the report says why.

**Stage 2: the engine on hand-written and scripted guests** (done, GO). It started after the
state search was merged into `main`, because both change `hydo/crates/hydo-kvm`. Two parts came
first:

- the test `engine::one_vcpu_fingerprints_are_pinned` (`hydo/crates/hydo-kvm/tests/engine.rs`)
  pins six scripted one-vCPU runs: idle, tick jitter, preemption points, another cadence, an
  exact spin, and a counter loop in the rewind mode;
- the APIC model `hydo_kvm::apic` with its unit tests: priorities, EOI, the TPR, the
  APIC turned off in software, ExtINT on LINT0, ICR decoding, and the timer in the exit clock.

- `Vmm` with N vCPUs on one thread, the scheduler, and the APIC model.
- INIT and STARTUP, the CPUID tables per vCPU, snapshots, and fingerprints.
- `hydo-kvm --vcpus N` and the `q=` and `qs=` tokens in exits.
- The one-vCPU pins first, then scripted tests of each rule, then the probe's guests run through
  the engine on a runner.

**Stage 3: Linux on several vCPUs** (done, GO; the S1 check waits for the next runner run). The
SMP kernel recipe (pinned), the MP table, and the relax port.
Linux boots with 2 and 4 vCPUs and prints `smp: Brought up 1 node, 2 CPUs`. Each check runs twice and once
under load, on every runner model:

- two boots with one fingerprint, and S1 against a fresh boot;
- `kvm-check.sh` on 2 vCPUs;
- the measurements of the section on cost.

Then the kernel's pin.

**Stage 4: the product** (the usefulness test passed). `--vcpus N` for `--backend kvm` reaches the
engine from `hydo run`, `hydo explore` and `hydo repro` of an exploration since Milestone 219, and
the help text says so. Left:

- `hydo repro` of one run's folder and `hydo explain` with several vCPUs on KVM were not run;
- `bzImage-kvm-smp` in the Linux release;
- `recorded.json` entries with `vcpus` for KVM, and TIME with turn tokens.

**Stage 5: what is left open.** The rewind mode across vCPUs, an emulated I/O APIC, paravirtual
spinlock waits, waking a spinning vCPU only after another vCPU wrote the pages it reads, and CPU
hot-plug.
