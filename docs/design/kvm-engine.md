# The KVM engine (hydo-kvm)

Status: built, run end to end on a KVM machine (the build machine, "Phase 2" below), reviewed
and verified again in Milestone 159 ("Milestone 159: the review and the second verification"),
counter loops resolved and snapshots made to share memory in Milestone 161 ("Milestone 161" at
the end); on a branch for after the first release, not merged. Crate `hydo/crates/hydo-kvm`;
guest kernel recipe `hv/kvm/linux/`; end-to-end check `hv/kvm/kvm-check.sh`.

What a user needs to know, in short: the KVM engine runs hydo's x86_64 images on any x86_64
Linux machine where the user can open `/dev/kvm` (a cloud VM with nested virtualization, a CI
runner, a laptop), with no kernel module and no root (`--backend kvm`). It gives the guest one
vCPU at the x86-64-v2 level, or at x86-64-v3 (AVX2) for images with `guest.cpu: v3` on a host
that has it (Milestone 187; images that ask for several vCPUs are refused).
Guest time is counted in VM exits that reach the engine, not in instructions, so a stretch of
guest code that makes no exit takes no guest time and gets no timer interrupt; a loop that spins
without exits is resolved when it is an exact cycle of register states, and a loop that also
counts while it spins is resolved in the rewind mode, which the host chooses by running such a
schedule again (anything else that runs without exits ends the run as invalid, SPIN). Snapshots
share the image's disk and RAM, so a worker's memory grows with what its runs write. Its
fingerprints are its own: they compare with
runs of this engine with the same guest kernel on the same kind of host CPU, never with hvx's or
the Mac engine's (across engines only verdicts compare, and `hydo repro` says so); a v3 guest's
fingerprints also differ between the performance and the efficiency cores of a hybrid CPU, so its
vCPU runs on one kind ("x86-64-v3"). It has run on an Intel machine (the build machine) and, at v2,
on GitHub's AMD runners.

## Why

hydo's x86_64 engine, hvx, is our own VT-x hypervisor in a kernel module. It needs bare metal (or
nested VT-x), an Intel CPU, root to load the module, and no running KVM. Most Linux machines
people test on are not like that: cloud VMs and CI runners offer KVM through nested
virtualization, many machines are AMD, and loading a module is often not allowed. The KVM engine
runs the same x86_64 agent images on stock Linux KVM (`/dev/kvm`), without a module and without
root, with the trade-offs the Mac engine (hvm) makes on Hypervisor.framework: the clock counts VM
exits, every interrupt is delivered at such an exit, one vCPU, and a guest that computes without
exits is handled by a spin observer (exact cycles, and counter loops in the rewind mode) or ends
the run as invalid (anything else).

The first Rust KVM monitor (removed in M100) failed because stock KVM cannot trap RDTSC and Linux
calibrates its clocks with it: two boots stayed bit-identical for about 730 million instructions
and then diverged at the RDTSC in `native_calibrate_cpu_early` (docs/HISTORY.md, the notes before
Milestone 1). This engine does not try to trap RDTSC. It makes the guest kernel never execute it:
a small kernel patch takes every clock of the guest from the engine's exit clock, and the guest's
CPUID says the CPU has no TSC.

## The design in one page

- **One vCPU on KVM with no in-kernel interrupt controller and no in-kernel PIT.** Every port I/O,
  MMIO access, HLT, MSR access the filter sends out, and interrupt window comes to the engine.
- **The clock is the number of those exits** (`vtime` in nanoseconds advances by
  `HYDO_KVM_STEP`, default 2500, per exit). Exits KVM handles in the kernel (EPT faults, host
  interrupts, PAUSE-loop exits) are not counted: they depend on the host. The engine's own kicks
  and single steps are not counted.
- **The guest kernel reads the clock on an I/O port** (each read is an exit, so a guest polling
  the time sees it move) and arms a one-shot timer on another port; the timer's interrupt is IRQ 0
  of an 8259 PIC the engine emulates. `rdtsc()` and `rdtsc_ordered()` in the kernel return the
  exit clock; user-space RDTSC faults (CR4.TSD) and the kernel emulates it with the same clock.
- **Interrupts are decided only at exits**, injected with `KVM_INTERRUPT` when the vCPU takes them
  before its next instruction, else at the interrupt-window exit KVM gives at the exact instruction
  where the guest can take one.
- **Nothing of the host reaches the guest**: one fixed CPUID table (the same on every host), KVM's
  paravirtual interface hidden behind another signature, every MSR KVM would answer from the host
  sent to the engine's fixed model, the PMU off, and no TLB-dependent spurious page faults (the
  guest kernel flushes at every page-table change).
- **The devices are the Mac engine's**: its virtio-mmio block device with hvx's seeded disk faults
  and its network device (hydo-hvm's `virtio` module, used as it is), its 16550 model; the
  lock-step protocol is the Mac engine's, so the host controller, agent images and the explorer
  work unchanged.
- **The images are hvx's**: the same disk image and initramfs, booted with the KVM build of the
  image's recorded kernel. No image changes, no recorded fingerprint moves.
- **Exit-free loops** are resolved at points the guest alone fixes: an exact cycle at its
  canonical state, a counter loop (Milestone 161) by a rewind to a checkpoint and a hardware
  breakpoint at its canonical instruction ("Exit-free loops", "Counter loops: the rewind mode").
- **Snapshots share memory** (Milestone 161): the disk is copy-on-write over the image file, RAM a
  sealed memory file mapped privately plus the pages that differ ("Memory: snapshots that share").
- **Fingerprints are this engine's own** (prefix `hydo-kvm-v1`); they compare with runs of this
  engine on the same kind of host, never with hvx's or hvm's.

## What was reused, and what was replaced

From the removed KVM monitor (`git show 9c5e41f^:src/vmm.rs`, `detlinux.rs`, `linux.rs`):

| reused | where now |
|---|---|
| VM and vCPU setup: `KVM_CREATE_VM`, guest RAM as one memory slot from GPA 0 (two above 3 GiB: the rest from 4 GiB), `KVM_SET_TSS_ADDR` and `KVM_SET_IDENTITY_MAP_ADDR` at `0xfffbd000`/`0xfffbc000`, the vCPU and its `kvm_run` page | `src/sys.rs` (`Kvm::fresh`) |
| long-mode entry state (page tables, GDT, segments, CR0/CR4/EFER) and the bzImage loader | already ported into hydo-hvx's boot recipe in M14; called as it is (`src/boot.rs`) |
| interrupt injection with `KVM_INTERRUPT`, `ready_for_interrupt_injection` and `request_interrupt_window` | `src/vmm.rs` (`deliver`), as the delivery rule |
| finishing an I/O instruction with a `KVM_RUN` that has `immediate_exit` set (the monitor's `drain_pending_io`) | `Cpu::complete`, now after every exit that needs it, not only before a snapshot (see "Completion") |
| single-stepping with `KVM_SET_GUEST_DEBUG` (its instruction clock) | the spin observer's steps (`src/spin.rs`) |
| the snapshot's parts: regs, sregs, FPU, `kvm_vcpu_events`, RAM, device state | `CpuState` (`src/cpu.rs`), plus the KVM-owned MSRs and the debug registers |
| the CPUID masking of RDRAND and RDSEED and the dropped kvmclock leaf | replaced by one fixed table (`src/cpuid.rs`) |
| per-VM device state, never process-global (its port-0x61 bug: two boots in one process shared state and diverged) | every device's state is in `Mach` and saved with the snapshot |

Replaced: the `kvm-ioctls`/`kvm-bindings` crates (hand-written ABI types in `src/abi.rs` with
a C probe against `<linux/kvm.h>`, as hydo-hvx does for `hvx_abi.h`; no new dependency), the
in-kernel irqchip of its Linux boot (an engine PIC), the instruction clock with single-step and the
PMU fast path (an exit-count clock; the PMU skid and descheduling problems it fought are gone with
it), and the host's supported CPUID (a fixed table).

Every point where the monitor lost determinism, from its history and code, and what this engine
does about each:

1. **RDTSC in Linux's calibration** (`native_calibrate_cpu_early`): the TSC rate is host time and
   KVM cannot trap RDTSC. Closed: the guest's CPUID has no TSC, `rdtsc()` and `rdtsc_ordered()`
   read the exit clock, and `vmlinux` contains no RDTSC, RDTSCP or RDPID instruction at all
   (`hv/kvm/linux/audit-rdtsc.sh`).
2. **kvmclock** (the shared page KVM stamps with host time): closed by the hypervisor signature
   (`hydo-kvm-x86`, not `KVMKVMKVM`), so the guest never enables any KVM paravirtual feature, and
   by the guest config (`CONFIG_KVM_GUEST=n`).
3. **RDRAND/RDSEED**: hidden in CPUID; on Intel KVM also makes them fault (`RDRAND exiting` when
   the guest's CPUID lacks them).
4. **The in-kernel LAPIC timer and PIT** fire on host time: no in-kernel irqchip and no PIT are
   created; the guest's timer is the engine's.
5. **PMU overflow skid and descheduling** (the instruction clock's fast path): no PMU clock; KVM's
   PMU is disabled for the VM where the host allows it, and hidden in CPUID either way.
6. **A snapshot between the two halves of an I/O instruction**: every such instruction is
   completed before the engine looks at the guest again.
7. **Process-global device state** shared by two VMs of one process: none.
8. **The IA32_TSC write** it used to reset the TSC: not needed; the MSR is the engine's (reads
   give the exit clock, writes are ignored).

## The clock and the guest kernel

`hv/kvm/linux/hydo-kvm-guest.patch` (C, against Linux 6.6.35) adds `CONFIG_HYDO_KVM_GUEST` and
`arch/x86/kernel/hydo.c`, a hypervisor platform detected by the CPUID signature:

- I/O ports (`arch/x86/include/asm/hydo.h`): `0x5e0` the clock (read with `rep insl` of two
  dwords: one instruction, one exit, the low and high half of the same exit's value), `0x5e4` the
  wall clock at boot (a constant of the engine, 2026-01-01T00:00:00Z), `0x5e8` the one-shot timer
  (nanoseconds from now; 0 disarms; the engine disarms it when it delivers its interrupt), `0x5ec`
  power off (1) and reset (2).
- A clocksource `hydo` (rating 450, 1 GHz), `sched_clock` through `paravirt_set_sched_clock`, a
  one-shot clock event device on IRQ 0 of the 8259 (the APIC is hidden, so Linux keeps the PIC),
  `get_wallclock`, a preset `loops_per_jiffy` (100,000: the calibration loop would wait for a
  jiffy without making an exit and spin forever), no CMOS RTC, no i8042, power off and restart.
- `rdtsc()` and `rdtsc_ordered()` (`asm/msr.h`) return the exit clock. With the TSC hidden in
  CPUID almost nothing calls them; `random_get_entropy()` and `get_cycles()` do, at every
  interrupt, and now read the exit clock (an exit each).
- CR4.TSD is set at boot and kept set across context switches (the per-task `PR_SET_TSC` toggle
  no longer clears it); user-space RDTSC faults with #GP, and the #GP handler emulates it with the
  exit clock (a task that asked for `PR_TSC_SIGSEGV` still gets its signal).
- **A TLB flush at every change of a present page-table entry** (`pte_needs_flush` and
  `huge_pmd_needs_flush` return true; `ptep_set_access_flags` flushes the page). x86 Linux skips
  the flush when it only adds permissions (`mprotect` to writable, a fault or GUP that makes a
  PTE writable) and relies on the CPU to fault on the stale entry and find nothing to do. Whether
  the stale entry is still cached depends on the host (the TLB's size, what the host evicted), and
  the spurious fault is counted by the guest (`vm_event_states` PGFAULT, a task's `min_flt`):
  found in phase 2 as two bytes of guest RAM that differed in 4 of 100 runs on the efficiency
  cores of a hybrid CPU, gone with the flush (0 of 150).
- **A TLB flush when the accessed bit is cleared** (`ptep_clear_flush_young`, Milestone 159):
  x86 Linux clears a page's accessed bit for reclaim without a flush, and the CPU sets the bit
  again only on a TLB miss, so whether the next access marks the page young depends on what the
  host's TLB kept, and with it which pages reclaim keeps (only under memory pressure in the
  guest). Found in the review, not in a run.
- **A clock read in every `sched_yield`**, so a thread that yields while it waits for a timeout
  sees the time move (a yield alone is no exit).

Why port reads and not a page the engine updates: a value written into guest memory at each exit
would let a guest read the time without an exit, but a loop that polls the time until it passes a
deadline would then never see it move (no exit, no tick) and would spin. With a port read each
observation is an exit, which advances the clock, as the Mac engine's trapped counter reads do.

The configuration (`hv/kvm/linux/hydo-kvm.config`, on top of allnoconfig) builds every driver in
(`CONFIG_MODULES=n`): virtio-mmio (devices named on the command line), virtio-blk and -net, ext4,
the 8250, network namespaces, veth, bridge, IPv6, policy routing, iptables with NFQUEUE, the
netem, prio and u32 schedulers: the features the Mac guest kernels have for the same images. One
CPU (`CONFIG_SMP=n`), no ACPI, no PCI, HZ 250, no stack protector (its canary would come from the
random pool before it is seeded).

The kernel command line: hydo-hvx's without the words this kernel does not know (an unknown word
would reach the image's `/init` as an argument), plus `io_delay=none` and one
`virtio_mmio.device=512@<base>:<irq>` per device, and ` quiet loglevel=1` as on the other engines.

### The build (pinned, reproducible)

`hv/kvm/linux/kernel.nix` builds it with the pinned nixpkgs (NixOS 24.05 `89c49874`, the one
docs/BUILD-PINS.md uses everywhere) from kernel.org's `linux-6.6.35.tar.xz` (sha256
`fce3ee72…`, the tarball of the Mac netns kernel); `hv/kvm/linux/build-kernel.sh` runs it with
`nice -n 19`, at most 8 jobs, and `CHECK=1` rebuilds it with `nix-build --check`. Fixed
`KBUILD_BUILD_*` values, and no build-id notes: the kernel's is hashed before `scripts/sorttable`
sorts the exception and ORC tables in place, and the vDSO's (20 bytes of `raw_data` in the
kernel's `.rodata`) differed in a build outside Nix's sandbox.

| output | sha256 |
|---|---|
| `bzImage-kvm` | `d50fbc67261a2544404d7a5f3e7a79cabe3b0a2e1f455158ccc5369d878d6207` |
| `bzImage-kvm.config` | `412c482c740b37f05c68d951b0b358775d4f72cb54b1e1948eaf7c6a81f564e5` |

The pin is Milestone 159's (store path `7hqyd9n0…-hydo-kvm-guest-kernel-6.6.35`, the recipe with
the accessed-bit flush): built with `nix-build` and rebuilt with `nix-build --check` on the build
machine, sandboxed, with the same bytes. The previous pin, `319b234e…` (store path `5kp5mg03…`,
the recipe without that flush), was rebuilt from the transplanted recipe in Milestone 159 with
`nix-build --check` too and gave its pinned bytes again.

Reproducibility: every version of the recipe was built twice on the build machine (`nix-build`
and `nix-build --check`, sandboxed) with the same bytes; before Milestone 159 the same store path
also came out of the bare-metal machine's single-user Nix (no sandbox) with the same bytes on its
first build (the current pin was not built there). Its
`--check` rebuilds without a sandbox differed: first in the vDSO's build-id (removed), then, with
equal `vmlinux` bytes, in the compressed payload of `bzImage`; a pin should be made with a
sandboxed Nix (the default of a multi-user installation).

`hv/kvm/linux/audit-rdtsc.sh` disassembles `vmlinux` and lists every RDTSC, RDTSCP, RDPID,
RDRAND, RDSEED and RDPMC (10 sites in the pinned kernel, checked again in Milestone 159): there
is no RDTSC, RDTSCP or RDPID; RDRAND and RDSEED appear only in
`x86_init_rdrand`, `extract_entropy` and `random_init_early`, each behind its CPUID feature bit,
which the guest does not have; RDPMC only in the perf PMU driver, which does not start without the
architectural PMU leaf (family 6 model 15 has no P6 PMU driver either).

## Images and the kernel an image names

An x86_64 image records the kernel its root and initramfs were made for (M155,
`/etc/hydo/image.json`: hvx's `bzImage` by sha256). Decision: `hydo build` does not change. The
engine pairs each recorded kernel with its KVM build (`hydo_kvm::kernel::TWINS`, today one pair:
hvx's `0a9ede35…` with `bzImage-kvm` `d50fbc67…`) and looks the KVM build up by sha256 where the
image's files are looked up (the release's `guest/`, a tree's `build/` and `hv/kvm/linux/out/`);
`HYDO_KVM_KERNEL` names another file. Why: a new field in the record would change the bytes of
every image built afterwards (the record is inside the image), and so every recorded fingerprint
of the other engines; with a pairing table every existing image runs on the KVM engine as it is,
and the record keeps meaning "the kernel lineage this image was built for". When a later hydo
changes the guest kernel, the new kernel gets its KVM twin in the same table.

The image's initramfs is used as it is: it insmods hvx's kernel's modules, which fail on this
kernel (it has every driver built in), and it goes on to mount `/dev/vda1` and switch to it, as it
was written to do (its errors are ignored by design). The host controller's own `insmod`s of
network modules are ignored the same way.

## Devices and layout

| what | where |
|---|---|
| guest RAM | 256..8192 MiB: up to 3 GiB from GPA 0 (below the devices), the rest from GPA 4 GiB (`layout::ram_blocks`); e820 as hvx |
| 8259 PIC pair | ports `0x20/0x21`, `0xa0/0xa1`; every line level-triggered, fixed priorities |
| hydo clock, wall clock, timer, power | ports `0x5e0`, `0x5e4`, `0x5e8` (IRQ 0), `0x5ec` |
| 16550 UART (the console, the agent's channel) | ports `0x3f8..0x3ff`, IRQ 4 |
| virtio-mmio block (the image's disk) | `0xd0000000`, IRQ 5 |
| virtio-mmio network (when asked) | `0xd0000200`, IRQ 6 |
| KVM's TSS and identity map | `0xfffbc000..0xfffc0000` |
| any other port | reads all ones, writes ignored (`HYDO_KVM_TRACE_UNKNOWN=1` lists them) |
| any other MMIO | reads zero, writes ignored |

The PIC (`src/pic.rs`) treats every line as level-triggered: a line's request bit is its device's
level, recomputed at every exit, as the Mac engine's GIC pending bits are. So a device that keeps
its line up after an EOI interrupts again, and the virtio devices' level semantics (the line stays
up until the driver acknowledges) need no edge logic. The commands Linux's i8259 driver uses are
emulated (ICW1-4, masks, specific and non-specific EOI, IRR/ISR reads, poll, auto-EOI).

The virtio devices are hydo-hvm's (`virtio::Virtio`), unchanged: the engine translates their
guest addresses (`0xd0000000 + i*0x200` to the Mac's `0x0a000000 + i*0x200`) and gives them guest
RAM from address 0. The disk is the whole image file (the root on `vda1`, the data on `vda2`), with
the flush-aware fault class at probability 0 at boot so the guest negotiates FLUSH, and the
schedule's `disk=`/`dseed=` arming hvx's seeded classes. A power cut keeps only the flushed bytes
and runs the recovery boot from the host's S0, as on the Mac. Snapshots share the disk and RAM
with the image and with each other ("Memory: snapshots that share", Milestone 161; until then a
2.4 GB stack image cost 2.4 GB per snapshot).

## Memory: snapshots that share (Milestone 161)

Until Milestone 161 every snapshot (the host's S0 and S1, every SNAP, the durable disk kept at a
power cut) held a full copy of guest RAM and of the disk image, and the live VM held one more: one
`hydo run` of the stack image peaked at 12.3 GB, an exploration worker on it at 10 GB. Now a
snapshot costs what the runs changed:

- **The disk** (hydo-hvm's `disk::Disk`, the virtio block device of both engines that use it):
  the image file is the base, mapped read-only and private (its pages are the page cache, shared
  by every process that reads the image, and the kernel drops them under memory pressure; where
  the file cannot be mapped it is read once). The 4 KiB blocks a run writes are held in a
  two-level table of reference-counted blocks (chunks of 512 blocks, 2 MiB of disk each). A copy
  (a snapshot, the durable image of a power cut, a checkpoint of the rewind mode) copies the
  table's top level, a few thousand pointers for a disk of gigabytes, and shares every block; a
  write copies a block, or a chunk's table, only while a copy shares it. The volatile write
  cache's data and the network device's capture buffer are shared the same way. Every read, the
  hash and `HVM_DISK_OUT` see exactly the bytes the flat image held (`disk::tests`), so no
  fingerprint moves (the Mac engine's included). `HVM_DISK_OUT` is written to a new file that
  then takes the name, so a path the base is mapped from is never rewritten under the mapping.
  The image file must not be rebuilt while an engine runs from it; that was never supported
  (workers that booted before and after a rebuild already ran different images).
- **RAM** (`mem::RamBase`, `mem::RamImage`): the host's S0 and S1, which every run restores, each
  become a base: RAM's nonzero pages written into a sealed memory file (`memfd`, zero pages left
  as holes), and guest RAM is then mapped from it privately, so that a page reads the base until
  it is written and only written pages take memory of their own (the kernel copies a page at its
  first write, whether the guest writes it through KVM or a device does). Which pages those are
  comes from the kernel's page map (`/proc/self/pagemap`: present and not the file's, or swapped
  out; where the page map cannot be read, every page is compared with the base, the same answer).
  Any other snapshot (a SNAP during a run) keeps only those pages. A restore maps the snapshot's
  base again, which drops every page of the run before it at once, writes the snapshot's own
  pages back, and creates the fresh VM as before. The fingerprint still hashes every byte of RAM
  in order (`mix64` page by page gives the value of `mix64` over all of RAM): RAM's own pages from
  RAM, the others from the base's read-only view, a hole's zeros without reading it (reading a
  hole through a mapping would give it memory). S0 and S1 are taken during the boot, which goes
  on in the same VM: guest RAM is remapped under the running VM, with the same bytes, and KVM
  follows the new mapping (its MMU notifier drops the old pages' entries); nothing of it is
  visible to the guest.

The measurements (the build machine, one VM at a time, `hydo run` and `hydo explore` with the
engine in hydo's process; memory is the largest proportional set size seen, `/proc/<pid>/smaps_rollup`
sampled every 0.2 s, in MiB, with the image file's page cache that the run read listed apart,
since it is shared and the kernel can drop it; before = the KVM branch at 7728ccc, after = this
milestone; every fingerprint and verdict the same before and after):

| run | before | after |
|---|---|---|
| Redis, `disk=none \| run` (boot, S0, S1, the run) | 2,494 MiB, 1.9 s | 225 MiB (anonymous 7, memory files 83, image file and program 134), 1.1 s |
| Redis, `disk=cut \| run C=12 R=everysec` (a power cut and the recovery boot) | 3,468 MiB (peak 3,598), 2.5 s | 230 MiB (anonymous 12, memory files 83, files 134), 1.1 s |
| Redis, exploration, 20 seeds, swarm, 1 worker (46 runs) | 3,600 MiB; 36.8 s, 0.64 s per run | 238 MiB; 29.7 s, 0.58 s per run |
| Redis, the same with 2 workers | 7,191 MiB; 20.2 s | 482 MiB; 15.4 s |
| stack (old order service), the pinned reproducers' schedule | 12,301 MiB, 21.4 s | 3,592 MiB, of which 2,310 the image file's page cache (anonymous 878, memory files 455); 18.9 s |
| stack, exploration, 6 seeds, 1 worker (7 runs) | 10,000 MiB; 46.5 s, 4.50 s per run | 3,221 MiB, of which 2,312 the image file's page cache (anonymous 880, memory files 460); 43.1 s, 4.56 s per run |

A stack worker's own memory is about 1.3 GB (the run's written RAM pages and disk blocks, and the
two bases' nonzero pages) where it was 10 GB; the 2.3 GB of the image file are the page cache's,
shared by every worker and process that reads the image. The time per run is the same within
noise (Redis 0.62-0.64 s before, 0.56-0.58 s after, over three explorations each; the stack
4.43-4.50 s and 4.49-4.56 s); a boot is faster (no full copies at S0 and S1; Redis `hydo run`
1.9 s to 1.1 s).

## The interrupt delivery rule and completion

At the top of the run loop (after the lock-step stop hook) the engine recomputes the device
levels; if the PIC has a vector:

- if KVM says the vCPU can take an external interrupt now (`ready_for_interrupt_injection`: IF
  set, no STI or MOV SS shadow, nothing already being injected), the engine acknowledges it (the
  PIC's INTA: ISR set, the timer disarmed when it is IRQ 0) and injects it with `KVM_INTERRUPT`;
  the CPU takes it at VM entry, before the next guest instruction;
- else it sets `request_interrupt_window`, and KVM exits to the engine at the first instruction
  boundary where the guest can take one: an architectural VM exit, as exact as any other; that
  exit is counted, and the decision is made again there.

**Completion.** KVM finishes an I/O, MMIO or MSR instruction only on the next `KVM_RUN`. The engine
finishes it at once, with a `KVM_RUN` that has `immediate_exit` set (KVM completes the instruction
and returns without entering the guest), before the stop hook and the interrupt decision run. Why
always, not only before a snapshot: KVM's readiness depends on whether the instruction is finished
(an `IN` right after `STI` is in the STI shadow until it completes), so a decision made before the
completion in one run and after it in a resumed one would differ. A string I/O instruction longer
than one batch gives its next exit from the completion; it is counted and handled at once.

**Idle.** KVM returns HLT to the engine. With interrupts on and nothing pending, the clock jumps
to the next event the guest can wait for (the timer's deadline with its tick-jitter delay, a
device's completion), as on the Mac; with nothing armed the run stops (`guest idle with no
timer`); a HLT with interrupts off is a halted machine.

## Exit-free loops: the spin observer

A guest that computes without an exit gets no interrupt and no time. On one vCPU, a thread that
spins on a flag another thread must set waits forever: found in phase 2, where `mysqld
--initialize` spun in `mov rax,[rdi+0x10]; test; jz; mov eax,[rax]; cmp eax,2; jne; pause; jnz`
(waiting for another thread to change a state word) and the stack image's boot ended as a SPIN.

`src/spin.rs` ports the Mac engine's rule "a provable cycle is idle" to x86 and KVM. On a kick (no
exit for `HYDO_KVM_SPIN_MS`, default 5 ms of host time, doubled after each look that proved
nothing) the engine steps the vCPU one instruction at a time, but only over instructions
`spin::decode` accepts before each step: loads, compares, tests, register arithmetic, jumps,
PAUSE, which write only general registers and flags (no store, no push, no I/O, no LOCK, no
SYSCALL). A step is a run to hardware instruction breakpoints (`KVM_GUESTDBG_USE_HW_BP`, DR0 and
DR1) at the addresses where the instruction can end: the next instruction, and a jump's target
(`spin::decode` computes the instruction's length and its relative target; RFLAGS.RF lets a jump
to itself run once). Until Milestone 159 a step was a trap-flag single step
(`KVM_GUESTDBG_SINGLESTEP`), and the review found that it leaked: a stepped load that faults (a
page the guest has not mapped in yet) has its exception delivered with the trap flag set in the
RFLAGS image the CPU pushes, so that image lands in guest RAM, and the handler's IRET could even
restore the trap flag after the observer had stopped (a SIGTRAP in the guest) — all depending on
when the host-timed kick came. With breakpoints, a fault is delivered exactly as in free running
and its handler runs as it would have; the observer does not step at all when the guest uses the
trap flag or breakpoints of its own, or runs outside 64-bit mode. After each step it hashes the general registers, RIP and RFLAGS; when
two full periods of states repeat, the loop is proven infinite (nothing else changes memory on one
vCPU without an exit), the vCPU is stepped to the cycle's canonical state (the smallest hash among
the states with interrupts enabled), the clock jumps to the next event as at an idle HLT, and the
interrupt is delivered at the next entry. Every iteration is the same state, so the delivery state
does not depend on when the kick came. Anything else (an instruction outside the set, a real exit
while stepping, which is then handled as any exit, no repeat within 4096 steps) leaves the guest
where free running would have taken it; a guest that stays without exits for
`HYDO_KVM_SPIN_SECS` (default 10) ends the run as a SPIN (invalid). A proven cycle that no
interrupt can end (nothing armed, or the due event's line masked) ends the run at once as a
deadlock (Milestone 159; before, a due but masked event kept the engine resolving the same cycle
forever). Counter loops (`while (!flag) n++`), whose state never repeats, are the next section's.

## Counter loops: the rewind mode (Milestone 161)

A loop that counts while it waits returns to the same instruction with other register values in
every pass, so the exact rule never sees a repeat, and the state at the kick is the entry state
plus k passes, k depending on when the host-timed kick came. The Mac engine resolves such loops in
its rewind mode (Milestones 57 and 130); this engine has a port of it, adapted to x86 and KVM.

**The proof** (`spin::counter_loop`). While the observer steps, it keeps the register states and
the data flow of every stepped instruction (`spin::Flow`, computed by the same decoder: the
registers read, written and used in an address, whether the arithmetic flags are read, written in
full or in part, whether the instruction is a conditional branch; an 8- or 16-bit write merges and
so reads its register, AH..BH are the second bytes of rax..rbx without a REX prefix, `xor`/`sub`
of a register with itself reads nothing). Two consecutive passes back to the same instruction
prove an infinite counter loop when: they run the same instructions; every bit of RFLAGS but the
arithmetic flags is the same; the registers that change between passes (the counters, and the
flags when they change) are tainted, the taint closed over a pass (what a tainted value writes is
tainted, what only untainted values write is not), and then no tainted value reaches a
conditional branch or a memory address; and the two passes agree, state by state, on every
untainted register (and on the flags where they are untainted). Then every untainted value is
the same in every later pass (the stepped instructions store nothing, and on one vCPU without
exits nothing else writes memory, so the loads read the same bytes from the same addresses), the
path is the same, and the loop never ends without an interrupt. The checks on two passes rather
than one also rule out a pass in which a fault's handler ran inside a step and changed memory. A
counter loop is reported only when the observer's whole budget (4096 steps) found no exact cycle,
so a loop the exact rule resolves (also one whose register only toggles) is resolved as before.

**Without the rewind mode** a proven counter loop ends the run at once as a SPIN (before
Milestone 161 it ended the same way after `HYDO_KVM_SPIN_SECS`), and the host runs the schedule
again with `spin=rewind` (hydo-control's `rewind_again`, now for the KVM engine as for the Mac's;
`HVM_SPIN_REWIND=0` turns it off for both). A plain run that completes is reported as it is.

**The rewind mode** (`vmm::Rewind`, `spin=rewind`; the workload, check and recovery phases of a
run, and a SNAP keeps it for its RESUME, as on the Mac). The engine keeps a checkpoint at a loop
top, after the stop hook and before the interrupt decision: the vCPU's state, the `kvm_run`
outputs the decision reads (`ready_for_interrupt_injection`, `if_flag`, the window request), the
machine state, the devices (the copy-on-write disk is a cheap copy), tick jitter, preemption
points and the receive queue; guest RAM as a second mapping kept equal to RAM at the checkpoint,
updated at each checkpoint from KVM's dirty-page log (`KVM_GET_DIRTY_LOG` on each memory slot,
whose flag the mode turns on) and from the pages the devices wrote (hydo-hvm's `Ram` view logs
them). A checkpoint is taken at the start of every phase, at the loop top after every stop of the
lock-step protocol (the host may have typed input there), after every rewind, and every 1024
exits (`vmm::CK_EVERY`), never where a kick left the guest. A proven counter loop then rewinds:
the written pages back from the copy, the vCPU into a fresh VM (as a restore does), and the run
goes on from the checkpoint, without calling the stop hook up to the exit count where the loop
was found (the host saw those exits and their stops already; the guest makes them again exactly,
the engine being deterministic) and without echoing or tracing them again; at that exit's loop
top a hardware breakpoint is armed at the loop's canonical instruction, the smallest address of
the pass with interrupts enabled. At the first arrival there with interrupts enabled (an arrival
with interrupts off steps over it), the clock jumps to the next event, as at an idle HLT, and the
interrupt is delivered at the next entry. The delivery point is fixed by the guest alone; the
host's timing decides only how many exits are replayed. A replay that makes an exit before the
breakpoint, or finds another counter loop, would mean the replay is not the run; it is reported
and the loop stays unresolved (a SPIN), never resolved at another point.

Differences from the Mac's rewind mode: the Mac takes a copy-on-write checkpoint at every exit
(RAM write-protected at stage 2, the old page copied at the first write), which makes a rewind
mode run several times slower; this engine checkpoints at stops and every 1024 exits and replays
the exits between, using KVM's dirty-page log instead of write faults (KVM gives user space no
fault on a guest write). The proof is register data flow over x86 instructions instead of the
Mac's "only `add`/`sub` of an induction register reads it", which also accepts the x86 forms
where the counter's flags are written and never read.

## Nondeterminism under KVM, and how each source is closed

| source | how it is closed | test |
|---|---|---|
| RDTSC/RDTSCP in the kernel | no TSC in CPUID; `rdtsc()` is the exit clock; none in `vmlinux` | `audit-rdtsc.sh`; boots twice identical |
| RDTSC in user space | CR4.TSD; #GP emulation with the exit clock | runs twice identical (MySQL reads the TSC) |
| RDTSCP, RDPID | hidden; KVM makes RDTSCP fault without the CPUID bit | `cpuid::tests::hides_every_host_leak` |
| kvmclock, steal time, async page faults, PV EOI, PV TLB flush, PV spinlocks, PV send-IPI, PV sched yield, poll control | another signature (`hydo-kvm-x86`), leaf `0x40000001` zero, `CONFIG_KVM_GUEST=n` | `cpuid` tests |
| invariant TSC, TSC deadline, TSC_ADJUST | hidden in CPUID; TSC MSRs are the engine's | `cpuid` and `msr` tests |
| the local APIC timer, the in-kernel PIT and IOAPIC | not created; APIC hidden (and APIC_BASE set to 0, so KVM keeps the APIC CPUID bit clear) | `boot::tests`; the guest boots in PIC mode |
| host interrupts, EPT faults, PAUSE-loop exits, preemption of the vCPU thread | handled inside KVM, not counted, invisible to the guest | runs twice identical, pinned and under load |
| when an injected interrupt lands | only at exits; injected only where the CPU takes it before the next instruction, else at the architectural interrupt-window exit | `tests/engine.rs`; runs twice identical |
| an unfinished I/O/MMIO/MSR instruction at a decision or snapshot | completed at every exit with `immediate_exit` | `snap_and_resume_continue_the_same_run`; S1 run equals fresh boot |
| what KVM leaves in `kvm_run` at an exit (`ready_for_interrupt_injection`, `if_flag`), which the interrupt rule reads at the first loop top after a restore, before any `KVM_RUN` sets them again (a fresh vCPU says 0: "cannot take an interrupt now") | a snapshot keeps them and a restore gives them back to the fresh vCPU, as the rewind mode's checkpoint does (until this fix, a run restored at a stop where an interrupt was pending asked for the interrupt window, one exit more, where the run that took the snapshot injected at once; "A run from S1 against a fresh boot" below) | `a_run_from_s1_equals_a_fresh_boot_wherever_s1_is_taken` (the scripted vCPU keeps the flags as KVM does); S1 run equals fresh boot over a sweep of exit steps |
| MSRs KVM answers from the host (IA32_TSC, PLATFORM_INFO, ARCH_CAPABILITIES, UCODE_REV, MISC_ENABLE, the PMU's, KVM's own) | MSR filter with default deny, every denied access to the engine's fixed model, unknown MSRs #GP | `msr` tests; `HYDO_KVM_TRACE_UNKNOWN` lists the guest's accesses (one at boot: 0x140) |
| CPUID (vendor, model, features, caches, topology, the hybrid core-type leaf 0x1A) | one fixed table on every host, checked against what the host's KVM supports | `cpuid` tests; P-core and E-core runs equal |
| the PMU (RDPMC, counters) | leaf 0xA zero; KVM's PMU disabled where `KVM_CAP_PMU_CAPABILITY` allows; CR4.PCE clear | `cpuid` tests |
| RDRAND, RDSEED | hidden; on Intel KVM makes them fault | `cpuid` tests; audit |
| MONITOR/MWAIT, WAITPKG (TPAUSE, UMWAIT) | hidden; KVM intercepts MWAIT | `cpuid` tests |
| XSAVE state size and features, AVX | hidden at v2 (as hvx's default); FXSAVE state only. At v3 (Milestone 187): x87, SSE and AVX in the standard format, and the sources of the table in "x86-64-v3" | `cpuid` tests |
| stale TLB entries after a permission upgrade (spurious page faults the guest counts) | the guest kernel flushes at every change of a present entry | 4 of 100 E-core runs differed before, 0 of 150 after |
| accessed bits cleared without a flush (reclaim decides by them; the CPU sets them again only on a TLB miss) | the guest kernel flushes the page in `ptep_clear_flush_young` (M159) | the patch (found in the review; it needs memory pressure in the guest) |
| a thread that yields while it waits for a timeout | every `sched_yield` reads the clock (an exit) | the patch |
| a thread that spins without exits on a flag another thread sets | the spin observer: proven exact cycles become idle, deterministically | `an_exit_free_spin_is_resolved...`; the stack boots |
| the observer's own steps (a fault while stepping pushed the trap flag into guest RAM, until M159) | steps run to hardware instruction breakpoints, so a fault is delivered as in free running; no stepping when the guest uses TF or DR7 | `spin::tests::lengths_and_ends`; the stack's spins resolved, runs twice identical |
| the FPU state in a snapshot (`KVM_SET_FPU` does not set the XSAVE header's component bits, so a fresh vCPU could load the x87/SSE initial state instead; `KVM_GET_FPU` reads the raw buffer) | `KVM_GET_XSAVE`/`KVM_SET_XSAVE`; the fingerprint hashes the canonical legacy area without MXCSR_MASK (M159) | S1 run equals fresh boot |
| KVM's paravirtual hypercalls (`KVM_HC_CLOCK_PAIRING` writes the host's clock into guest memory) | `KVM_CAP_ENFORCE_PV_FEATURE_CPUID`: with no KVM leaf in the table, none is offered (M159) | the capability is required |
| the wall clock (CMOS RTC) | a constant from the engine's port | the patch |
| the kernel's delay-loop calibration | preset `loops_per_jiffy` | boots finish |
| the random pool | seeded only from the exit clock and the guest's own inputs; RDRAND/RDSEED absent | runs twice identical |
| guest RAM's initial contents | anonymous zero pages | — |
| a kick (the watchdog's signal) | not an exit: nothing changes in the guest, nothing is counted | the run loop |
| the host's KVM version and CPU microarchitecture (undefined flag results, FPU pointers) | not closed: a fingerprint is this engine's on one kind of host; verdicts compare everywhere | see "Where fingerprints compare" |
| counter loops without exits (the state at a kick depends on when it came) | the rewind mode: back to a checkpoint, the same exits again, the interrupt at the first arrival at the loop's canonical instruction (M161); without the mode the run ends at once as a SPIN and the host runs it again in the mode | `counter_loop` tests; `a_counter_loop_is_...`; ledger-spin on KVM, each schedule twice on a performance and an efficiency core |
| what the host's memory holds for a snapshot | not the guest's: RAM bases, private pages and the copy-on-write disk give the same bytes as full copies; the fingerprint reads every byte (zero pages of a base without reading them) | `mem` and `disk` tests; every M159 fingerprint again |

## Where fingerprints compare

A run's fingerprint hashes the console, every vCPU register (with the FPU's last-instruction
pointers), the engine's machine state, the devices (the disk with its volatile write cache) and
all guest RAM. Two runs compare when they ran on this engine with the same kernel, the same CPUID
table and step, on hosts whose CPUs execute the guest's instructions alike. Instructions with
architecturally undefined results (flags after MUL or a shift, BSF of zero) and the FPU's
last-instruction pointers can differ between CPU microarchitectures and vendors; compilers do not
rely on the former, and the latter are saved by the guest's context switches into its RAM. So:
within one host, and between the cores of a hybrid CPU (measured below), fingerprints compare;
across vendors or generations, compare verdicts (as `hydo repro` does across fingerprint epochs).
Other host differences the guest can see and CPUID cannot hide: the MXCSR_MASK the guest's own
FXSAVE writes into its RAM (the engine leaves it out of the vCPU part of the fingerprint, but not
out of RAM), and how a KVM version batches string I/O and splits MMIO accesses (the number of
exits, so the clock, could change with the host kernel's KVM). A fingerprint of this engine never
compares with one of hvx or the Mac engine: `hydo repro` of a run or an exploration recorded by
another engine compares verdicts and says why (Milestone 159), and refuses to type another
engine's control transcript (its stops are counted in that engine's clock).

Everything that defines the guest's state is in a snapshot and in the fingerprint: the general
registers, the segment registers with their hidden parts, the descriptor tables, CR0, CR2-CR4,
CR8, EFER and APIC_BASE (`kvm_sregs`, with the pending-interrupt bitmap), the x87, SSE and MXCSR
state (from `KVM_GET_XSAVE`; at v2 XCR0 stays at its reset value 1, since the guest cannot
enable XSAVE; at v3 XCR0 and the AVX state too, "x86-64-v3"), the KVM-owned MSRs (SYSENTER, PAT, STAR, LSTAR, CSTAR, SFMASK, KERNEL_GS_BASE; FS and GS
bases are in the segments), the pending events (exception, interrupt, NMI, the interrupt shadow,
a pending triple fault), the debug registers, the engine's machine state (the clock, the exit
count, the timer, the PIC, the UART, the MSR model), the devices with the disk's volatile cache,
and all guest RAM. The guest's TSC is not (the guest never reads it), and neither are SMM state
and SIPI (no SMM, one vCPU).

## The protocol

`src/serve.rs` is the Mac engine's `serve` with this machine: the controlled boot (the host's
SNAP S0/S1, READY), CTL, RESUME, the lock-step stops (EVENT, SENT, STOP; SEND, ACK, SNAP, CONT,
END), DISKOFF and CUT with the recovery boot, the same RESULT lines with `stops=`, `typed=`,
`inh=`. Differences: the cadence stop is due when the exit count crosses a multiple of `rb=`
(an instruction that needs two exits can step over one); the default cadence of agent images is 64
exits, as on the Mac. `spin=rewind` runs a schedule's phases in the rewind mode (Milestone 161;
refused before); the mode's checkpoints are taken after the stop hook and its replays do not call
it, so the stops, the typed input and `inh=` are those of the run, and a schedule that completes
plainly gives the same fingerprint with the mode on. A run from the worker's S1 and the same
schedule from a fresh boot give the same fingerprint (tested on the scripted vCPU and on KVM; until
the fix described in "A run from S1 against a fresh boot", only when no interrupt was pending at
the stop where the host took S1).

## Using it

- `hydo run --backend kvm …`, `hydo repro --backend kvm …`, `hydo explore --backend kvm …`,
  `hydo doctor --backend kvm`, or `HYDO_BACKEND=kvm` for all of them (Linux). The Mac has one
  engine.
- `hydo doctor --backend kvm` checks x86_64 Linux, VT-x or AMD-V, `/dev/kvm` and that this user can
  open it, what KVM offers (it opens the device and asks; no VM is made), the guest kernel by
  sha256 and the initramfs.
- The engine runs on a thread of the `hydo` process (KVM allows several VMs in one process);
  `HYDO_KVM=process` runs the program `hydo-kvm` instead, with the Mac engine's command line plus
  the disk: `hydo-kvm [--ram-mib N] [--cpu v3] --control boot serve|one <bzImage-kvm> <initramfs>
  <disk|-> <outdir> ["<schedule>"]`.
- The explorer draws schedules for it as for the Mac (the tokens both apply: no `cf=`, `pp=`,
  `xt*`; forced preemption points `pct=` instead), with hvx's images.
- An image that asks for several vCPUs is refused by the engine itself (for `hydo run`, the
  explorer and hydo-bench alike), not run on fewer (Milestone 159). An image with `guest.cpu: v3`
  runs at x86-64-v3 (`hydo-kvm --cpu v3`, Milestone 187) on a host whose KVM has AVX2, and is
  refused elsewhere with what the host lacks; `hydo doctor --backend kvm` says which.
- A run that ends as a SPIN (a counter loop the engine proved, Milestone 161) is run again by the
  host with `spin=rewind` (`hydo run`, `hydo repro`, the explorer and hydo-bench alike), and the
  second run is the one reported, as on the Mac; `HVM_SPIN_REWIND=0` turns that off for both
  engines. A schedule can also carry `spin=rewind` itself.
- `hydo repro` without `--backend kvm` of a KVM run (or with it, of an hvx run) runs the other
  engine and compares verdicts, not fingerprints, and says so; a control transcript is typed
  again only by the engine that recorded it.
- Where it runs: x86_64 Linux with `/dev/kvm` openable by the user and KVM from Linux 5.10 on
  (the user-space MSR exits and the MSR filter; `KVM_CAP_XSAVE` and
  `KVM_CAP_ENFORCE_PV_FEATURE_CPUID` too). In a VM, the provider must expose nested
  virtualization. It is not in either release tarball yet: a release needs `bzImage-kvm` in its
  `guest/`.
- Variables (hydo-config's registry, group `Kvm`): `HYDO_BACKEND`, `HYDO_KVM`, `HYDO_KVM_ENGINE`,
  `HYDO_KVM_KERNEL`, `HYDO_KVM_DEV`, `HYDO_KVM_STEP`, `HYDO_KVM_RUN_EXITS`, `HYDO_KVM_SPIN_SECS`,
  `HYDO_KVM_SPIN_MS`, `HYDO_KVM_TRACE`, `HYDO_KVM_ECHO`, `HYDO_KVM_DIAG`, `HYDO_KVM_DIAG_RAM`,
  `HYDO_KVM_TRACE_UNKNOWN`; the ABI probe's `HYDO_KVM_HEADERS` (tests).

## x86-64-v3 (`guest.cpu: v3`, Milestone 187)

Programs built for newer CPUs need AVX2 at start: TigerBeetle's release binary, the official
Restate image, and anything compiled with `-march=x86-64-v3` stop with "illegal instruction" on a
v2 guest. Until Milestone 187 the KVM engine refused such images; now it gives them hvx's
x86-64-v3 CPU (Milestone 68) on a host whose KVM has AVX2. A v2 guest is exactly what it was:
the same CPUID table, the same vCPU state, the same fingerprints.

**What the guest sees.** `cpuid::table_for(Level::V3)`: the v2 table plus FMA, MOVBE, XSAVE, AVX
and F16C (leaf 1), BMI1, AVX2 and BMI2 (leaf 7), LZCNT (leaf 0x80000001), the basic leaves up to
0xD, and the XSAVE leaf for x87, SSE and AVX only, in the standard format (an 832-byte area, AVX at
offset 576; sub-leaf 1 zero: no XSAVEOPT, XSAVEC, XGETBV1 or XSAVES). No AVX-512, MPX, PKRU or AMX.
The vendor and everything hidden at v2 stay as at v2 (no TSC, no APIC, no RDRAND or RDSEED, no
PMU, no topology). KVM itself keeps two values of the table in step with the guest, as hardware
does: OSXSAVE mirrors the guest's CR4.OSXSAVE, and CPUID.0xD.0:EBX is the area's size for the
guest's current XCR0 (576 or 832 bytes). Both are functions of the guest's own state. The guest
kernel says "x86/fpu: Enabled xstate features 0x7, context size is 832 bytes, using 'standard'
format", `/proc/cpuinfo` lists `fma movbe xsave avx f16c abm bmi1 avx2 bmi2`, and glibc reports
x86-64-v3 as supported, as under hvx.

**How it is set.** `hydo-kvm --cpu v3` (the engine options carry it for `hydo run`, `hydo repro`,
the explorer and hydo-bench, from the image's manifest as for hvx). `sys::Kvm::with_level` checks
the host's `KVM_GET_SUPPORTED_CPUID` for every v3 bit, for XCR0 x87|SSE|AVX, and that the host's
XSAVE layout puts AVX at offset 576, and it needs `KVM_CAP_XCRS`; a host without them is refused
with the list of what it lacks (`hydo doctor --backend kvm` says whether v3 can run there). The
guest enables XSAVE components itself with XSETBV, which KVM handles in the kernel against the
table (a bit outside x87|SSE|AVX is a #GP), so XCR0 is guest state.

**Snapshots and fingerprints.** A v3 vCPU's state adds XCR0 (`KVM_GET_XCRS`/`KVM_SET_XCRS`,
restored before the XSAVE image); the YMM upper halves were already in the XSAVE image the engine
keeps (`KVM_GET_XSAVE`, 4096 bytes, standard format). The fingerprint of a v3 vCPU adds XCR0, the
header's XSTATE_BV restricted to x87|SSE|AVX (what the guest's own XSAVE writes; KVM may add PKRU on
a host with protection keys, which the guest cannot see) and the AVX state. A v2 state hashes as
before: `CpuState.xcr0` is 0 there, and the hash then reads nothing beyond the legacy area
(`cpu::tests::a_v2_state_hashes_as_before_v3`).

**What a v3 guest can see of the host, and how each is handled.**

| source | how it is handled | test |
|---|---|---|
| the feature bits, the XSAVE leaf, the vendor | one fixed v3 table on every host, checked against the host's KVM | `cpuid::tests::v3_*` |
| XCR0 | set by the guest's XSETBV (in KVM, no exit, no clock), limited by the table; in snapshots and in the fingerprint | S1 run equals fresh boot |
| the YMM upper halves | in the XSAVE image of every snapshot; in the fingerprint | S1 run equals fresh boot; `a_v3_state_hashes_...` |
| the XSAVE area's size and layout (Intel and AMD) | standard format only (no XSAVEC/XSAVES), AVX at 576 on both vendors, checked on the host | `v3_check_names_what_is_missing` |
| XSAVEOPT's "modified" optimization (whether it writes depends on the last XRSTOR, which host exits and context switches change) | hidden (CPUID.0xD.1 zero), so the guest kernel and glibc use plain XSAVE; VT-x cannot make XSAVEOPT or XSAVEC fault, so a program that uses them without checking CPUID could see it | the guest kernel's "standard" line |
| which components a core tracks as being in their initial state (XINUSE, the init optimization: the header's XSTATE_BV that the guest's own XSAVE writes into its RAM) | the core design's choice. On the build machine's hybrid Core i7-13700HX the x87 bit of a saved area is 0 on a performance core and 1 on an efficiency core, so the same run gave one fingerprint on each kind, and a third when the vCPU thread moved between them. A v3 vCPU thread is bound to one kind of core at its first run: the performance cores the process may use, else its efficiency cores (`hybrid::choose`, the CPUs in the engine's host line); across CPU models, as before, compare verdicts | unpinned runs equal the runs pinned to a performance core; pinned to CPU 16 they are the efficiency cores' own value |
| XGETBV with ECX=1 (reads XINUSE) | hidden (XGETBV1 is not in the table) but VT-x cannot make it fault: a program that executes it without checking CPUID reads the core's tracking | — |
| MXCSR_MASK (also written by XSAVE) | as at v2: left out of the vCPU part of the fingerprint, in RAM when the guest saves it | — |
| AVX frequency changes | timing only, which the exit clock does not see | — |
| the spin observer | its decoder knows no VEX instruction: a loop without exits that uses AVX is not resolved and ends as a SPIN, as any undecoded loop | — |

**Measured** (the build machine, Core i7-13700HX, Linux 6.6.35, `bzImage-kvm` `d50fbc67…`; every
schedule from a fresh boot unless marked S1, which is a run from the explorer's worker snapshot):

| image (sha256) | schedule | verdict | fp |
|---|---|---|---|
| the avx2 example (`2dcb4305…`, M140's build) | `disk=none \| run` | OK, 8 AVX2/FMA steps equal the scalar ones; xstate 0x7, 832 bytes, standard format | `963aa26e9b7a5a08` (twice, twice pinned to CPU 0, and S1) |
| | the same, pinned to CPU 16 (an efficiency core) | OK | `2068294767586e14` (twice) |
| | `disk=none \| run T=4:3600` | OK | `9da9f9e442eb9aa4` (twice, twice on CPU 0, S1); CPU 16 `8ee59a997783b9cd` (twice) |
| | `disk=none \| run` with `hydo explore --cpu v2` (the control) | VIOLATION `acked=0 good=0` (no AVX2) | `9398a9f5d5df5774` |
| restate-oci (`77978db5…`, the official image built for Broadwell) | `disk=none \| run` | OK, 6 of 6 steps answered | `62acce98929d1369` (twice) |
| | `disk=none \| run K=restate@2+2` | OK, 4 of 6 | `69c72be4dbd8e583` (twice) |
| | `disk=cut \| run C=4` | OK, 4 of 6 | `04f9abeff6e9c10b` (twice) |

The verdicts are hvx's for the same images (docs/REBASELINE.md 13.2). The 20 unbound runs of the
clock schedule after the binding gave `9da9f9e442eb9aa4` every time.

Before the binding to one kind of core, unbound runs of the avx2 image gave `c23937045b3bd29c`
twice, and the clock schedule gave the performance cores' value 14 times and another value once:
the thread had moved between the two kinds during those runs.

Every recorded v2 value of this document is unchanged with the v3 code in the engine:
`hv/kvm/kvm-check.sh` gave 16 of 16 (with the ledger-spin check, CPUs 0 and 16) and every recorded
KVM value of `hydo/onboard/recorded.json` (the release's Redis image's eight schedules, the netem
image, the S1 run `ac512c2616cef6a4`, the exploration's 46 runs and `hydo repro` 14 of 14), and the
stack's pinned reproducers' schedule gave `c3419179cf1a55ba` (VIOLATION, order 11) and
`c0746a7568ccf3b8` (OK), each twice.

`hv/kvm/v3-check.sh` checks v3 end to end on any KVM host, Intel or AMD: the doctor, two fresh
boots and a run from S1 of a v3 image (`hv/kvm/v3/hydo.yaml`, the avx2 example without its Nix
glibc, built by the release on the machine), a clock step twice, and the v2 control. On the build
machine it gave 5 of 5 (its image `1ae6a578…`, `disk=none | run` `856667408ac3beaa` from fresh
boots and from S1).

**TigerBeetle does not run on this engine, at any CPU level.** Its three-replica image of
Milestone 179 (`4dfaec44…`, 8 GiB, v3) boots on it and starts `tigerbeetle format` in the first
replica's `setup:`, whose AVX2 code runs (the vCPU was found in its `vmovdqu` stores), and then
makes no VM exit again: the run ends `ERROR boot: SPIN` after `HYDO_KVM_SPIN_SECS` (10 s by
default; with 600 s it spun until stopped, with constant memory). `format` waits for its disk writes
by polling io_uring's completion queue from user space without a system call (TigerBeetle's
`io.tick()`, Zig's `copy_cqes` with no wait). On this engine the disk's completion interrupt is
raised 16 exits after the request, and a completion reaches user space only when the guest kernel
is entered again; a loop with no exit and no system call gets neither, so it waits forever. On hvx
the PMU clock interrupts it. The spin observer cannot prove the loop a cycle (it stores to memory,
calls functions and uses VEX instructions), so it is the class "anything else that runs without
exits ends the run as invalid" (AGENTS.md open item 45 (c)), not a v3 problem. A remedy needs an
exit at a point the guest alone fixes inside such a loop: a proof of exact cycles that also
covers stores (writes that put back the same bytes) and AVX registers, or a branch-counting
interrupt as hvx's.

**On an AMD host (GitHub's runners).** Not run yet. The steps, on `ubuntu-24.04` (AMD EPYC there
on 2026-11-08), with the branch checked out:

1. Give the runner's user `/dev/kvm`: write the udev rule
   `KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"` to
   `/etc/udev/rules.d/99-kvm4all.rules` with sudo, then `sudo udevadm control --reload-rules` and
   `sudo udevadm trigger --name-match=kvm`.
2. Install the published Linux release (it has `guest/bzImage-kvm`): download
   `hydo-linux-0.2.0.tar.gz` from the GitHub release `v0.2.0`, unpack it, and run
   `./hydo-linux-0.2.0/install.sh --no-module --prefix "$HOME/hydo-rel"`.
3. Build this tree's `hydo` as a static program: `sudo apt-get install -y musl-tools`,
   `rustup target add x86_64-unknown-linux-musl`, then
   `CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --locked -p hydo --target x86_64-unknown-linux-musl`.
4. Run the check: `HYDO=target/x86_64-unknown-linux-musl/release/hydo REL="$HOME/hydo-rel"
   OUT="$RUNNER_TEMP/v3" sh hv/kvm/v3-check.sh` (it compiles `hydoavx` with the runner's `cc`
   and builds the image with the release), and keep `$RUNNER_TEMP/v3/v3-check.log` as an artifact,
   with `grep -m1 'model name' /proc/cpuinfo`.

Expected: 5 of 5, with fingerprints of their own (AMD's XINUSE tracking and MXCSR_MASK are its
own, as v2's fingerprints already differ by vendor). Running `hv/kvm/kvm-check.sh` there as well
shows the v2 values unchanged (`93da6e186fe00d1a` and `ea0afd368b0ab9b1` on the Redis image).

Left: no AMD host has run a v3 guest yet (AMD's XINUSE tracking and its MXCSR_MASK will give
other fingerprints, as v2's already differ by vendor; the verdicts should be equal); several vCPUs
stay hvx's.

## One vCPU

Several vCPUs need turns of an exact length of guest work (hvx counts retired branches with the
PMU, M66); KVM gives no instruction-exact clock, and the exit count is not one. The engine has
one vCPU; `--vcpus` stays an hvx option (the v3 CPU level is the KVM engine's too since
Milestone 187).

## AMD hosts

The table says `GenuineIntel` on every host, so the guest runs the same kernel paths everywhere.
On AMD, KVM's cross-vendor support covers what a 64-bit Linux guest with this table could hit
(SYSENTER in 64-bit mode is not used: no 32-bit compatibility). The differences that remain are
the hardware's (undefined flags, as above) and one gap: AMD has no RDRAND or RDSEED exiting, so a
user program that executes RDRAND without checking CPUID gets hardware random bits there (glibc,
OpenSSL and the Go and Rust runtimes check CPUID first). GitHub's AMD runners ran it at v2 on
2026-11-08 (AGENTS.md, open item 45): the same verdicts as on Intel, fingerprints of their own. At
v3 the standard XSAVE format is the same on both vendors (the engine checks the AVX offset on the
host); which components a core tracks as initial, and MXCSR_MASK, are the core's, so a v3 guest's
fingerprints are per CPU kind there too. No AMD host has run a v3 guest yet (`hv/kvm/v3-check.sh`
does it).

## Code structure

- `abi.rs` the KVM ABI subset (pure; `tests/abi.rs` compares it with `<linux/kvm.h>`);
- `sys.rs` the ioctls, the `kvm_run` page, kicks: with `mem.rs` the only `unsafe` code;
- `cpu.rs` the `Cpu` and `Hypervisor` traits and `CpuState`; `script.rs` a scripted vCPU (its
  steps run to the next operation's address, as KVM's run to a breakpoint);
- `mem.rs` guest RAM, the RAM bases snapshots share, and RAM images (with `sys.rs` the only
  `unsafe` code); the disk's copy-on-write blocks are hydo-hvm's `disk`;
- `cpuid.rs` (the v2 and v3 tables), `msr.rs`, `pic.rs`, `uart.rs`, `machine.rs`, `layout.rs`,
  `boot.rs`, `kernel.rs`; `hybrid.rs` which kind of core a v3 vCPU runs on;
- `vmm.rs` the machine, the run loop (generic over the vCPU) and the rewind mode; `spin.rs` the
  spin observer's rules, the decoder's data flow and the counter-loop proof;
- `serve.rs` the protocol; `bin/hydo-kvm.rs` the program.

Everything but `sys.rs` builds and is tested on any system: the run loop, devices, interrupt rule,
snapshots, fingerprints, the spin observer and the protocol run on the scripted vCPU in
`tests/engine.rs`.

## The check on a KVM machine

`hv/kvm/kvm-check.sh` does it end to end on any x86_64 Linux machine where the user can open
`/dev/kvm` (no root, no module): the doctor; the quickstart's Redis image built by the release; two
boots from scratch identical; the quickstart's two schedules each twice identical; a run from the
worker's S1 equal to the fresh boot; the power-cut schedule ten times on each listed CPU; an
exploration of 20 seeds with 2 workers and `hydo repro` of it; a kill, a pause, a clock step, an
I/O error, tick jitter and forced preemption points, each twice; a network fault (the release's
`hydo-netem.yaml`) twice. It needs: nested virtualization when the machine is a VM (Intel VT-x
or AMD-V exposed to the guest), the Linux release unpacked, `bzImage-kvm` in its `guest/`, and this
tree's `hydo`. On the build machine it takes about 2 minutes. With `SPIN_BIN=<dir>` (the two
programs of `hv/kvm/spin/build.sh`) it also builds the ledger-spin example and checks that its
counter loops are resolved in the rewind mode with one fingerprint on every CPU of `CPUS`
(Milestone 161; 16 checks).

## Phase 2: the runs on the build machine

The build machine: a 13th Gen Intel Core i7-13700HX (hybrid: performance cores are CPUs 0-15 with
Hyper-Threading, efficiency cores CPUs 16-23), NixOS, Linux 6.6.35, `/dev/kvm` usable by the login
user (no root used). Release `hydo-linux-0.1.0` (`e9538ed9…`), the Redis image built by it
(`d255fb2b…`, the quickstart's bytes); final kernel of phase 2 `319b234e…` (Milestone 159 pinned `d50fbc67…`; its values are in the last section).

| check | result |
|---|---|
| `hydo doctor --backend kvm` | all checks passed |
| `disk=none \| run`, two fresh boots | `e83662dd590459b9` twice, OK acked=20 |
| `disk=cut \| run C=12 R=everysec`, 10 runs on CPU 0 and 10 on CPU 16 | `3d182f077f11bd93` all 22, VIOLATION key k:1 lost (hvx: the same verdict) |
| `disk=none dseed=1 \| run` from the worker's S1 and from a fresh boot | `54b23fde79633d11` both |
| `K=redis@5+2`, `P=redis@4+3`, `T=6:3600`, `disk=ioerr:20 dseed=7`, `tj=1234 tjpm=300`, `pct=99 pctn=4`, each twice | `1ac74b78a5d8b93d`, `3aab5eacc751c221`, `cd8bb36b76e0330f`, `f497abecdf327a82`, `68743a7fd3d5357b`, `21c58cec1be78116`, each twice the same |
| netem image, `N=redis@3-6:delay,300ms`, twice | `ff5b507ffd7ddda5` twice, OK retrans=0 |
| exploration, seeds 1-20, swarm, 2 workers | 38 runs in 17 s (4,435 runs/hour, 0.7 s per run, worker boot 1.2 s); 9 failures (io-errors 3, lying-disk 4, unsafe-knob 2), all reproduced; minimal schedules `disk=lying:3 dseed=eac0b527c5ac67d9 \| run C=20` and `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no`, hvx's two, each twice identical; `hydo repro` 11 of 11 with the recorded fingerprint |
| the same exploration twice (earlier kernel) | the same 40 fingerprints and the same report |
| the pinned reproducer schedule `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` on the stack images (MySQL 8.4, Kafka 3.5 on the JVM, Redis, the Go order service), twice on CPU 2 and twice on CPU 18 | old order service: `fa7e8cd18379d640` four times, VIOLATION order=11 is in kafka but the client was never ACKed (hvx: the same verdict); fixed one: `ac8e66b59f4f7a38` four times, OK |
| an exploration of the stack image (old order service), seeds 1-6, one worker | 6 schedules in 46 s (4.8 s per run, worker boot 12.8 s); 1 failure (io-errors, `ERROR front restart failed`), reproduced |

## Milestone 159: the review and the second verification

The engine was transplanted onto the rewritten history (first verified unchanged: with the phase-2
kernel `319b234e…` rebuilt from the transplanted recipe, every value of the table above came out
again), then reviewed with the code read on its own. What the review changed (docs/HISTORY.md
"Milestone 159" has each finding with its reasoning):

1. The spin observer steps to hardware instruction breakpoints instead of the trap flag ("Exit-free
   loops"): a fault during a step had left TF in an exception frame in guest RAM, depending on
   when the kick came.
2. The guest kernel flushes the TLB entry when it clears an accessed bit (a new kernel pin).
3. Snapshots take the FPU state with `KVM_GET_XSAVE`/`KVM_SET_XSAVE`; `KVM_SET_FPU` did not mark
   the x87/SSE components present in the XSAVE header.
4. `KVM_CAP_ENFORCE_PV_FEATURE_CPUID`: no KVM hypercall or PV MSR reaches the guest.
5. A bounds check in the guest page-table walk (a guest could panic the engine thread).
6. Several vCPUs and `guest.cpu: v3` are refused by the engine for every caller; `hydo repro` and
   `--transcript` know the engine a run was recorded by.

The values with the code and kernel of this milestone (the build machine, a 13th Gen Intel Core
i7-13700HX: performance cores CPUs 0-15, efficiency cores CPUs 16-23; Linux 6.6.35; the Linux
release of M158 for the guest files; nothing as root, no module touched):

| check | phase 2 (kernel `319b234e…`) | Milestone 159 (kernel `d50fbc67…`) |
|---|---|---|
| `hv/kvm/kvm-check.sh` | 15 of 15 | 15 of 15 |
| `disk=none \| run`, two fresh boots | `e83662dd590459b9` | `bfa03053d51bec8a`, OK acked=20 |
| `disk=cut \| run C=12 R=everysec` | `3d182f077f11bd93`, 22 runs | `8ed16d6da60c34c6`, 122 runs (60 on performance cores 0 and 2, 60 on efficiency cores 16 and 18, 2 unpinned), VIOLATION k:1 lost |
| S1 against a fresh boot | `54b23fde79633d11` | `ac512c2616cef6a4` |
| kill, pause, clock step, I/O error, tick jitter, preemption points | six values, each twice | `818c50920c9ced38`, `b72c5c7732e6a67f`, `49a776b9f1c8febd`, `b059f4d71358bc4d`, `06ee67ef4d1a50b2`, `aec48eb5ca42eca3`, each twice, the same verdicts |
| netem delay | `ff5b507ffd7ddda5` | `0fcb6905c8f54cd1`, OK retrans=0 |
| exploration, 20 seeds, 2 workers | 38 runs, 9 failures | 46 runs, 11 failures (hvx's classes and counts), hvx's two minimal schedules plus a third (data disk unmountable), repro 14 of 14; run twice: the same 46 fingerprints, reports equal but for wall-clock lines |
| stack, pinned reproducers' schedule, CPUs 2 and 18, twice each | old `fa7e8cd18379d640`, fixed `ac8e66b59f4f7a38` | old `c3419179cf1a55ba` VIOLATION order 11, fixed `c0746a7568ccf3b8` OK; 90 and 98 spins resolved per run with breakpoint steps |

Every fingerprint moved (the guest kernel's bytes and the hashed FPU state changed); every
verdict is the one of phase 2 and of hvx.

Left: an AMD host (never run; RDRAND, RDSEED and TSX cannot be made to fault there); several
vCPUs (no instruction-exact clock on KVM); counting loops without exits (the Mac's rewind mode is
not ported); snapshots keep the whole disk in memory; the engine is in neither release tarball.

## Milestone 161: counter loops and shared snapshots

What was built ("Counter loops: the rewind mode", "Memory: snapshots that share"): the
counter-loop proof and the rewind mode, `spin=rewind` accepted and the host's re-run of a SPIN
extended to this engine, a copy-on-write disk (in hydo-hvm, so the Mac engine's snapshots share
too) and RAM bases with private pages. The guest to prove it: `hv/kvm/spin/ledger-spin`,
the Mac's ledger-spin for x86_64, whose CGI busy-waits in user space for a timer signal in every
request, without a system call or a clock read (`hv/kvm/spin/spinvar.c`: `spin_load` an exact
cycle, `spin_count` the counter loop `mov edx,[fired]; add rax,1; test edx,edx; jne`, built by
`hv/kvm/spin/build.sh` with the pinned static gcc 13.2.0 and binutils 2.41: `97fc2b0e…` and
`f46111a9…`; the image `f16a3762…`, built twice with the same bytes by the Linux release of M158).

| check (the build machine; kernel `d50fbc67…`; the release of M158) | result |
|---|---|
| `hv/kvm/kvm-check.sh`, three times (the second and third time with the new ledger-spin check: 16 of 16; the third from the final tree) | 15 of 15, every value of Milestone 159's table: `bfa03053d51bec8a`, `8ed16d6da60c34c6` (22 runs each time: twice, 10 on CPU 0, 10 on CPU 16; and 3 in the measurements), S1 against a fresh boot `ac512c2616cef6a4`, `818c50920c9ced38`, `b72c5c7732e6a67f`, `49a776b9f1c8febd`, `b059f4d71358bc4d`, `06ee67ef4d1a50b2`, `aec48eb5ca42eca3`, `0fcb6905c8f54cd1`; the exploration 46 runs, `hydo repro` 14 of 14 |
| the exploration again (20 seeds, swarm, 2 workers), and once with the code before this milestone | the same 46 schedules and fingerprints, three times |
| the stack, the pinned reproducers' schedule, twice on CPU 2 and twice on CPU 18 | old `c3419179cf1a55ba` four times, VIOLATION order 11; fixed `c0746a7568ccf3b8` four times, OK (Milestone 159's values) |
| ledger-spin `disk=none \| run` without the rewind mode | `ERROR workload: SPIN` at once, the counter loop proven at `rip 0x4010cc` (the code before this milestone: the same SPIN after 10 s) |
| ledger-spin with the host's re-run in the rewind mode | `disk=none \| run` `f796ae624a9473d1` OK acked=30; `K=front@10+2` `cab50b0e332bc0b7` OK; `P=front@5+3` `603bcb8735ba9c4b` VIOLATION entry 5 stored twice (the example's planted bug); `T=8:3600` `9fa90d803a6ebb1d` OK; each four times, twice on CPU 2 and twice on CPU 18; in one run 79 counter loops proven and resolved, 478 checkpoints, 1,039 exits replayed |
| ledger-spin, exploration of 10 seeds with 2 workers, twice | the same 10 fingerprints, every run OK |
| the Mac engine with the copy-on-write disk (this tree's `hydo-hvm`, the Mac release of M158, each twice) | Redis `67e5d6469f1a90a5`, `C=12 R=everysec` `a6870a92a1bbf0ae`, `disk=lying:3 dseed=eac0b527c5ac67d9 \| run C=20` `0441476f5fd74d0b`, ledger-spin `aa732142c0f5567f` and `P=front@10+2` `3b4dd4910d2a6123`: docs/REBASELINE.md 13.4's values |

No fingerprint moved: the RAM and disk hashes read the same bytes in the same order, the
observer steps exactly as before and reports a counter loop only where it gave up before, and the
rewind mode is on only in runs the host starts with `spin=rewind`.

Left: the rewind mode on an AMD host and in a guest with several counter loops at once in one
pass (both untested); a loop without exits whose counter decides when it ends (a wait counted in
iterations) is not an infinite loop, so nothing is proven: it runs on and, when it lasts
`HYDO_KVM_SPIN_SECS`, ends the run as a SPIN, as before; S0 and
S1 are two RAM bases (S0 could be kept as the pages where it differs from S1); the image file's
page cache counts in each process's resident set (shared and reclaimable, but visible in RSS).

## Milestone 172: in the Linux release

The Linux release ships the engine from 0.2.0 on: `bin/hydo-kvm` (the program; by default the
engine runs inside `hydo`) and `guest/bzImage-kvm` (a pinned asset, `d50fbc67…`, with its own
entry in the component list and its recipe in the sources archive). `hydo doctor` reports the
engine's files and the state of `/dev/kvm`; the release's QUICKSTART has a section "Run without the
kernel module (KVM)". The 0.2.0 candidate, installed with `install.sh --no-module` on the build
machine, gave this document's values for the release's Redis image (`bfa03053d51bec8a`,
`8ed16d6da60c34c6`, netem `0fcb6905c8f54cd1`, the exploration's 46 runs twice the same);
docs/HISTORY.md "Milestone 172".

## A run from S1 against a fresh boot (after Milestone 172)

Found in the libSQL onboarding (`hydo/onboard/examples/libsql-replica/RESULTS.md`, friction 19): on
one image every schedule gave one fingerprint from a fresh boot (`hydo run`) and another from the
explorer's worker, which restores its S1 for every run; each was the same every time and the
verdicts agreed. So `hydo repro` of an explorer finding on such an image says DIFFERENT.

**The cause.** At a stop the host's commands (SNAP among them) are served at the top of the run
loop, before the interrupt rule. When an interrupt became pending at the exit before that stop
(for example an idle HLT whose clock jump reached the timer's deadline, the case of the test
below), the run that took S1 goes
on in the same vCPU, whose `kvm_run` still says what KVM said at that exit
(`ready_for_interrupt_injection` = 1), and injects the interrupt at once. A run restored from S1
loads the vCPU's whole state into a fresh vCPU, but `kvm_run`'s outputs are not part of that state:
a fresh vCPU says 0 until its first `KVM_RUN`. The interrupt rule therefore asked for the interrupt
window, KVM gave that exit at once, and the exit was counted: from there on the restored run was
one exit (2.5 µs of guest time) behind the fresh boot, which moves where events fall on the
cadence stops (the libSQL transcripts first differed at a cadence stop, `at=` 62848 against 62912,
one cadence apart) and with it everything after. Where nothing was pending at S1's stop the two
runs agree, which is why the Redis image's check at the default step passed. The rewind mode
(Milestone 161) already kept these flags with its checkpoints; snapshots did not.

**The fix** (`hydo/crates/hydo-kvm/src/vmm.rs`): `XSnap` keeps the vCPU's `run_flags()` and a
restore gives them back after loading the state, as the rewind mode does. The scripted vCPU of the
tests now keeps the flags as KVM does (set only when a run or a completion returns, 0 in a fresh
vCPU), and `a_run_from_s1_equals_a_fresh_boot_wherever_s1_is_taken` takes S1 at each of the first
40 stops of a boot with a stop at every exit: without the fix it fails where S1 lands on the
HLT whose clock jump makes the timer pending (one exit more, 84 stops against 83).

**The measurements** (the build machine; `HYDO_KVM_STEP`, the nanoseconds per exit, varied only to
move where S1 lands; each line a run from a worker's S1 by `hydo explore --schedule` and a fresh
boot by `hydo run`):

| image, schedule | before the fix | with the fix |
|---|---|---|
| the release's Redis image (`d255fb2b…`), `disk=none dseed=1 \| run`, steps 2300 to 2700 by 20 (21 steps) | 8 of 21 differ (2340, 2460, 2480, 2600, 2640, 2660, 2680, 2700) | 0 of 21 differ; every fresh-boot fingerprint the same as before the fix |
| the release's Redis image, the default step | `ac512c2616cef6a4` both | the same |
| the stack's pinned reproducers' schedule, old and fixed order service | `c3419179cf1a55ba`, `c0746a7568ccf3b8` both | the same |
| libsql-replica rebuilt from its folder: "set once" with `disk=none dseed=1 \| run` (the default step and six others) and `K=primary@10+4`; the final folder with `disk=none dseed=1 \| run` and `disk=cut dseed=1 \| run C=20` | all equal (this rebuild's S1 does not land on such a stop; the onboarding's image did) | all equal, the same values |

A fresh boot restores no snapshot until a power cut, whose recovery boot restores the host's S0
(the fix applies there too); none of the recorded values moved: `hv/kvm/kvm-check.sh` with the
fix (the release of M158 with this tree's `hydo`, CPUs 0 and 16, with the ledger-spin check) gave
16 of 16 and every value of this document (`bfa03053d51bec8a`, `8ed16d6da60c34c6` 22 times, S1
`ac512c2616cef6a4`, the six fault schedules, netem `0fcb6905c8f54cd1`, the exploration's 46 runs
with `hydo repro` 14 of 14, ledger-spin `f796ae624a9473d1`), and the stack's pinned reproducers'
schedule (a power cut) gave `c3419179cf1a55ba` and `c0746a7568ccf3b8` from S1 and from a fresh boot.
A run from S1 moves only on an image where it differed from the fresh boot, and then to the fresh
boot's value; an exploration recorded before the fix on such an image does not reproduce its
fingerprints any more (its verdicts do).
