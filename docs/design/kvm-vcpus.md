# Several vCPUs on the KVM engine

Status: a design, written on 2026-11-13. Stage 1, the KVM probe, passed on 2026-11-13 on three
AMD and one Intel model (Milestone 213, and the section on what the probe showed). The APIC model
(`hydo/crates/hydo-kvm/src/apic.rs`) and the SMP kernel's recipe exist; the engine does not use
them yet, and one vCPU is the only machine. It answers AGENTS.md open item 45 ("The KVM engine: one vCPU only"). The engine it extends is
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
of them.

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

**Stage 2: the engine on hand-written and scripted guests.** Its engine part starts only after the
state search is merged into `main`, because both change `hydo/crates/hydo-kvm`. Two parts are
ready already:

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

**Stage 3: Linux on several vCPUs.** The SMP kernel recipe (ready, not pinned), the MP table,
and the relax port.
Linux boots with 2 and 4 vCPUs and prints `smp: Brought up 1 node, 2 CPUs`. Each check runs twice and once
under load, on every runner model:

- two boots with one fingerprint, and S1 against a fresh boot;
- `kvm-check.sh` on 2 vCPUs;
- the measurements of the section on cost.

Then the kernel's pin.

**Stage 4: the product.** `--vcpus N` for `--backend kvm` in `hydo run`, `hydo repro`, `hydo
explain`, and `hydo explore`, and a KVM range for the turn-length search. Then `recorded.json`
entries with `vcpus` for KVM, and TIME with turn tokens. The usefulness test is the nats example
on 2 vCPUs: whether some `q` passes and some fails. If every `q` gives one verdict, risk 1 holds,
and the result goes into `RESULTS.md` and open item 45.

**Stage 5: what is left open.** The rewind mode across vCPUs, an emulated I/O APIC, paravirtual
spinlock waits, waking a spinning vCPU only after another vCPU wrote the pages it reads, and CPU
hot-plug.
