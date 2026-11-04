# hvx — Architecture Reference

`hvx` is a from-scratch Intel VT-x hypervisor, implemented as a single Linux kernel
module (`hv/hvx/hvx.c`) plus a shared kernel↔userspace ABI (`hv/hvx/hvx_abi.h`). It owns
the VMCS directly — no KVM underneath it — so it can control every source of
nondeterminism a guest can observe. It is the second-generation core of the `hydo`
project: a **deterministic hypervisor**, where the same
inputs produce bit-for-bit identical execution every run, enabling record/replay,
snapshot, and branch-from-snapshot.

This document is a reference for a new engineer. It is written against the code and
milestone history as of **Milestone 23 complete, Milestone 24 (network fault
injection) in progress**. Every claim here is grounded in `hv/hvx/hvx.c`,
`hv/hvx/hvx_abi.h`, `CLAUDE.md`, and `README.md`; where the code and the narrative
diverge, that is called out in the final section.

> Scope note: the `src/*.rs` tree is the **original, KVM-based hydo** — the
> predecessor and design reference. It is described in `README.md` and referenced
> throughout this document as "the KVM path". `hvx` is the subject here; the KVM
> path is cited only to show what `hvx` replaces and why.

---

## 1. What hvx is, and why it exists

### The wall in the KVM-based predecessor

The original hydo (`src/*.rs`) is a deterministic VMM built on Linux KVM. It tames
time, randomness, interrupt timing, and I/O, and it *proves* determinism with a
four-check verify harness (replay / round-trip / branch / seed-sensitivity, over a
BLAKE3 state fingerprint). It boots a real Linux kernel and can bisect the first
point where two boots diverge.

That harness found a **hard wall**. Two boots stay bit-identical for ~730 million
retired instructions — all of decompression and early setup — and then diverge at a
`RDTSC` inside `native_calibrate_cpu_early`. The cause is architectural, not a bug:

> **Stock KVM does not expose the VT-x "RDTSC-exiting" control to a userspace VMM.**
> So the KVM-based hydo cannot make `RDTSC` return a function of the instruction
> clock; the guest sees the real host time-stamp counter, and two runs differ.

Masking `RDRAND`/`RDSEED`, dropping kvm-clock, and resetting the TSC do not move the
wall, because the RDTSC calibration runs first. This is exactly why hydo owns its
own hypervisor rather than layering on KVM.

### The decision: be your own hypervisor

`hvx` is the response: a Linux kernel module that runs the VT-x lifecycle itself
(`VMXON` → `VMLAUNCH`/`VMRESUME` → exit handling → `VMXOFF`), owns the VMCS, and
therefore can enable **RDTSC-exiting** and substitute a deterministic virtual TSC.
Having taken ownership of the VMCS, it then owns *everything* a guest can observe:
the timer, CPUID, MSRs, interrupt timing, serial and network input, and device I/O
completion. That is the whole thesis — a hypervisor that mediates every input so the
guest's execution is a pure function of its initial image plus a seed.

The RDTSC wall is closed at **Milestone 4**, and the full result — a real Linux
kernel booting to a shell so that two boots are bit-for-bit identical *through* the
`native_calibrate_cpu_early` region that walled the KVM path — is proven at
**Milestone 16**.

---

## 2. The nested sandbox model (safety)

`hvx` is kernel code that manipulates VMX hardware state. A bad VMCS field can panic
the machine it runs on. The project has **no root on the physical host** and must
**never risk a host panic**. Both constraints are satisfied by developing entirely
inside a throwaway nested VM.

| Level | What it is | Trust / recovery |
|---|---|---|
| **L0** | The physical host (the build machine, NixOS, kernel 6.6.35, i7-13700HX). Runs KVM with nested virtualization enabled. | **Never touched.** No root; `insmod`/`rmmod`/`sudo` on the host are forbidden by policy *and* by construction. |
| **L1** | A Linux VM run as an unprivileged `qemu -enable-kvm -cpu host,+vmx,pmu=on` process under L0's KVM (VMX exposed via nesting). We are root inside it. `hvx.ko` is built and `insmod`'d **here**. | If `hvx` panics or wedges L1, just restart the QEMU process (`run-l1.sh`) — the host is unaffected. |
| **L2** | The guest that `hvx` itself runs (hand-written blobs, or a real Linux bzImage). | A bad guest triple-faults, which is a clean VM-exit, not a wedge. |

Key operational facts (from `CLAUDE.md`):

- The L0 KVM is **shared** with unrelated workloads. Kills must be scoped to our
  instance (`pkill -f l1-hvx`, or the pid in `~/l1/l1.pid`) — never a broad
  `pkill -f qemu-system`.
- A guest that runs with `RFLAGS.IF=1` can *steal L1's physical timer interrupts*
  and RCU-stall L1; `run-l1.sh` restarts it. The module's pin-control design (see
  §4) is what prevents this in normal operation.
- Rebuild/reload from the build machine: `./dev.sh "~/hydo/hv/hvx/deploy-hvx.sh load"` — syncs
  the tree, copies into L1, builds the module and every `hvx_*_test`, `rmmod
  kvm_intel`, `insmod hvx`, runs the tests against `/dev/hvx`, prints dmesg,
  `rmmod`.

---

## 3. The determinism model and its one invariant

### The invariant

> **Every input the guest can observe is a deterministic function of the
> instruction clock or of explicitly captured state — never of wall-clock time or
> host state.**

If that holds for every source, then the guest's entire execution is reproducible:
replay it, snapshot it mid-flight, branch from the snapshot, and every path reaches
the identical final state. The verify harness (§8) is the empirical proof that the
invariant holds.

### How each source is tamed

| Source of nondeterminism | How `hvx` makes it deterministic | Milestone |
|---|---|---|
| **`RDTSC` / `RDTSCP`** | RDTSC-exiting is enabled; the handler returns a hypervisor-controlled virtual TSC (COUNTER or ICOUNT mode, §4), never the host counter. | M4, M6 |
| **Timer interrupts** | The guest programs the LAPIC TSC-deadline MSR; the run-loop injects the timer vector when the *virtual* TSC reaches the deadline — an instruction-timed event, not a wall-clock one. | M15 |
| **Interrupt timing generally** | `HVX_INJECT` schedules a vector at an exact `icount`; the run-loop arms `VM_ENTRY_INTR_INFO` on the entry where `icount >= at_icount`. | M8 |
| **CPUID** | A fixed synthetic table is written into the guest GPRs in-kernel; no CPUID exit reaches userspace and no host feature leaks in. | M12 |
| **MSRs (RDMSR/WRMSR)** | In-kernel: EFER via the VMCS field, IA32_TSC via the virtual TSC, a curated software MSR store for the rest; unknown MSRs read 0 / drop the write. | M12 |
| **Serial input** | Host keystrokes/bytes are pushed via `HVX_PUT_INPUT` into a ring the guest drains; the ring is **transient**, deliberately excluded from the snapshot, so a no-input boot is bit-identical. Delivery IRQ rides the instruction-timed path. | M18 |
| **Disk I/O completion** | virtio-blk completion fires at `kick_icount + HVX_VBLK_LATENCY` (16 instructions) — a pure function of the instruction clock — delivered via the injection path. | M20/M21 |
| **Network RX/TX completion** | virtio-net RX/TX completions fire at `kick_icount + HVX_VNET_LATENCY` (16 instructions); scripted inbound frames (`HVX_PUT_RX`) and captured outbound frames (`HVX_GET_TX`) ride in the snapshot. | M23 |
| **Fault injection** | Disk (M22) and network (M24) faults are drawn from a seeded splitmix64 PRNG keyed by request/packet index — a pure function of `(seed, index)`, never host state — so a bug a fault surfaces replays bit-for-bit. | M22, M24 |
| **External (physical host) interrupts** | Forced to VM-exit (pin control); serviced *inline* in a one-instruction `sti; nop; cli` window so the **host** IDT handles them, and **never counted as guest instructions**. They therefore cannot perturb the instruction clock. | M8 |
| **SMP / memory ordering** | vCPUs are **serialized** onto one execution context and interleaved by a deterministic instruction quantum (§7). | M19 |

Two subtle disciplines make the timer/completion story hold under host interrupt
jitter — both are load-bearing and were found by diffing two boots (M16):

1. **Deterministic THROTTLE cadence.** In free-run, the guest only returns to
   userspace at a `HVX_EXIT_THROTTLE` boundary. That boundary must be a
   deterministic function of the guest, so the `guard` counter that triggers
   THROTTLE counts **only retired (deterministic) exits** (`HVX_MAX_INKERNEL_EXITS`
   = 1<<16). A separate, larger `HVX_SAFETY_CAP` (1<<20) bounds *total* iterations
   against a pathological interrupt storm.
2. **Deterministic virtual-TSC creep.** In COUNTER free-run, `vm->tsc` advances one
   `step` per retired exit while a deadline is armed. That mutation lives in the
   **retired block**, not the loop top, so the virtual TSC is a pure function of
   `icount` and a host-interrupt iteration never moves it. (Before this fix, the
   LAPIC timer injected one instruction early/late, shifting jiffies and printk
   timestamps and cascading into RAM.)

---

## 4. Core mechanisms

### 4.1 VMX / EPT and the guest address space

Each `HVX_RUN` is a **self-contained VMX-root critical section**, not a held state:

```
kernel_fpu_begin() → preempt_disable() → local_irq_save()
  VMXON → VMCLEAR → VMPTRLD → setup_vmcs() → INVEPT
  load guest SYSCALL/KERNEL_GS_BASE MSRs into hardware
  for (;;) { VMLAUNCH/VMRESUME; handle exit in-kernel or return }
  restore host MSRs → VMXOFF
local_irq_restore() → preempt_enable() → kernel_fpu_end()
```

VMX is deliberately **not held across ioctls** (documented design decision): each
new `HVX_RUN` re-establishes the full VMCS from the software register/sregs state and
does a fresh `VMLAUNCH` from the saved RIP — equivalent to `VMRESUME` for our
purposes, with no stale-CPU / migration hazards. Holding VMXON is an optimization
deferred to single-step throughput, never a correctness requirement.

`kernel_fpu_begin()/end()` (M14) wraps the whole section because a real guest uses
SSE/FPU very early and would otherwise clobber the interrupted host task's live XMM
state.

**EPT** gives the guest its own physical address space (M2, scaled to 256 MiB at
M11). Guest RAM is a set of **individually-allocated order-0 pages** (not one
high-order block — tail pages of a non-compound allocation are not individually
refcounted, which corrupts page state when the mmap fault handler hands a page to
userspace). EPT uses 4 KiB leaves throughout, with `n_ept_pt = ceil(pages/512)` PT
pages hung off a single EPT PD.

**Guest-physical layout the module owns** (`GPA_*` in `hvx.c`). The GDT/IDT/TSS and
the interrupt handler live in the *free space* of the page-table pages (the walker
only reads entry [0] of each), keeping the userspace code base fixed at `0x3000`:

| GPA | Contents |
|---|---|
| `0x0000` | Guest PML4 (page 0); GDT at `0x0800`, TSS at `0x0a00` |
| `0x1000` | Guest PDPT (page 1); IDT at `0x1800` |
| `0x2000` | Guest PD (page 2); ISR blob at `0x2800`, interrupt counter at `0x2f00` |
| `0x3000` | `HVX_GUEST_CODE_BASE` — userspace-loaded guest code starts here |

Guest paging identity-maps the low 1 GiB (512 × 2 MiB PS pages), so
guest-virtual == guest-physical for hand-written guests, and userspace `mmap`
addresses equal guest addresses. The IOAPIC (`0xfec00000`) and virtio BARs
(`0xfe000000`, `0xfe020000`) live in the 4th GiB, deliberately **EPT-unmapped**, so
guest accesses to them fault out as EPT violations the module decodes (§5). For
hand-written guests, `HVX_SET_DISK` / `HVX_NET_ATTACH` poke a single 1 GiB identity
PDPT leaf at `pdpt[3]` (with Accessed/Dirty pre-set, see §9) so the guest can reach
that MMIO; a real OS builds its own tables and passes `HVX_DISK_NO_IDMAP` /
`HVX_NET_NO_IDMAP`.

### 4.2 The `/dev/hvx` ioctl ABI

`/dev/hvx` is a misc device (mode 0600), one VM per open fd. `struct hvx_vm` (guest
RAM, EPT, device model, and the vCPU array) persists across ioctls. Guest RAM is
reached with `mmap(fd)`. The complete ABI:

| ioctl | Dir | Struct | Purpose | M |
|---|---|---|---|---|
| `HVX_CREATE_VM` | W | `hvx_config` | Size guest RAM, choose `n_vcpus`; build EPT + identity paging + GDT/IDT/handler. | M3/M19 |
| `HVX_SET_REGS` / `HVX_GET_REGS` | W/R | `hvx_regs` | GPRs + rip/rsp/rflags. GPR order is load-bearing (asm trampoline indexes by byte offset). | M3 |
| `HVX_RUN` | R | `hvx_exit` | Run until one userspace-visible VM-exit; returns reason/insn_len/qualification/guest_phys. | M3 |
| `HVX_SET_TSC` / `HVX_GET_TSC` | W/R | `hvx_tsc` | Virtual-TSC seed/step/aux/mode + read-back exit count. | M4/M6 |
| `HVX_RUNC` | WR | `hvx_run_ctl` | Controlled run with the instruction clock: single-step (MTF), reset icount, stop after `max_insns`; returns running `icount`. | M5 |
| `HVX_GET_SNAPSHOT` / `HVX_SET_SNAPSHOT` | R/W | `hvx_snapshot` | Capture/restore full deterministic state minus guest RAM. | M7 |
| `HVX_INJECT` | W | `hvx_inject` | Schedule vector at an exact `at_icount`. | M8 |
| `HVX_GET_CONSOLE` | R | (`__u32`)¹ | Read accumulated UART TX bytes. | M10 |
| `HVX_GET_SREGS` / `HVX_SET_SREGS` | R/W | `hvx_sregs` | Privileged state: CRs, EFER, DR7, segments, GDTR/IDTR. | M13 |
| `HVX_PUT_INPUT` | W | `hvx_input` | Push up to 256 bytes into the guest UART RX ring. | M18 |
| `HVX_SET_VCPU` | W | `__u32` | Select the current vCPU (target of per-vCPU ioctls). | M19 |
| `HVX_SET_DISK` / `HVX_GET_DISK` | W/WR | `hvx_disk` | Attach/read the virtio-blk backing image (≤ 1 MiB); attaching enables the device. | M20 |
| `HVX_SET_FAULT` / `HVX_GET_FAULT` | W/R | `hvx_fault` | Seeded disk fault policy + running PRNG counters. | M22 |
| `HVX_NET_ATTACH` | W | `hvx_net_config` | Attach the virtio-net device with a MAC; enables it. | M23 |
| `HVX_PUT_RX` / `HVX_GET_TX` | W/WR | `hvx_rx` / `hvx_tx` | Push one scripted inbound frame / read back captured outbound frames. | M23 |
| `HVX_SET_NET_FAULT` / `HVX_GET_NET_FAULT` | W/R | `hvx_net_fault` | Seeded network fault policy + counters. | **M24 (in progress)** |

¹ `struct hvx_console` is 64 KiB, larger than the 14-bit `_IOC` size field can
encode, so `HVX_GET_CONSOLE` is declared with `__u32` and the handler transfers the
whole struct explicitly.

Sentinel exit reasons: `HVX_EXIT_VMFAIL` (0xffffffff, VMLAUNCH itself failed),
`HVX_EXIT_THROTTLE` (0xfffffffe, in-kernel loop bound hit — re-run to continue),
`HVX_EXIT_STEP` (37, MTF instruction budget reached, guest alive).

### 4.3 The MTF instruction clock

The determinism engine's core primitive (M5). With single-stepping requested,
`setup_vmcs` sets the **Monitor Trap Flag** proc-based control (bit 27), so every
guest instruction takes an MTF VM-exit (basic reason 37). The run-loop counts those
— plus emulated RDTSC/RDTSCP — into `vcpu->icount`, and `HVX_RUNC` can stop after
exactly `max_insns`. This is "advance the guest to instruction N", the primitive
that snapshot boundaries, injection scheduling, and the ICOUNT TSC all build on.

`icount` semantics: it counts instructions the monitor *observes retiring*. While
single-stepping (MTF on) that is a full instruction count; under plain `HVX_RUN`
(no MTF) it advances only on emulated RDTSCs and other retired-in-kernel exits.

The MTF clock is **exact but slow** — one VM-exit per instruction (~10⁸ for a boot).
The intended fast alternative (a guest-only PMU counter) is blocked on this hardware;
see §9.

### 4.4 The deterministic virtual TSC (COUNTER vs ICOUNT)

`HVX_SET_TSC.mode` selects the policy:

- **`HVX_TSC_COUNTER`** (default): RDTSC returns `tsc`, then `tsc += step` per RDTSC
  exit. Deterministic in the number of RDTSC *calls*. This is the mode used for
  whole-boot free-run bring-up (M16), with the "creep" discipline (§3) making the
  LAPIC-deadline timer advance one `step` per retired exit.
- **`HVX_TSC_ICOUNT`**: RDTSC returns `tsc + icount·step` — a pure function of
  execution **position** (instructions retired). `tsc` is the base, `step` the
  ticks-per-instruction, and `tsc` is not mutated. This is the "true" deterministic
  model, meaningful while single-stepping. Wired at M6: the RDTSC handler computes
  the ICOUNT value from `icount` *before* the shared retired bump, so it reflects
  instructions strictly before this RDTSC.

M6's test shows a 6× TSC delta spread from execution position alone (COUNTER delta
1000 vs ICOUNT delta 6000 across the same 6 instructions).

### 4.5 Snapshot / branch-from-snapshot / replay

Two ioctls (`HVX_GET/SET_SNAPSHOT`, `struct hvx_snapshot`) copy the full
deterministic state **minus guest RAM**. Guest RAM is already `mmap`'d, so a complete
snapshot is:

```
HVX_GET_SNAPSHOT  +  memcpy(save_buf, mmap_ptr, mem_size)  [+ HVX_GET_DISK / HVX_GET_TX]
```

and restore is the inverse. Host-transient state (VMCS/EPT/VMXON pages) is
intentionally absent — it is rebuilt from the software state on the next run.

The snapshot struct is **append-only** across milestones (older field offsets never
shift), so a VM that does not use a later feature hashes an identical prefix and
preserves earlier fingerprints. It now carries: the register file; virtual-TSC
policy/state; `icount`; `rdtsc_count`; the pending injection schedule; the curated
software MSR store; full `sregs`; the LAPIC/IOAPIC/PIC device state (M15); the 16550
UART register latch (M16); virtio-blk control state + MSI-X table (M20/M21); the disk
fault policy + PRNG counters (M22); virtio-net device/queue/RX-FIFO/captured-TX state
(M23); and the net fault policy + PRNG counters (M24). What is **deliberately not**
in the snapshot: guest RAM (rides via mmap), the disk image (`HVX_GET/SET_DISK`),
captured frames (`HVX_GET_TX`), and the **transient** serial-input ring — because
folding host-timed input into the deterministic state would make it nondeterministic.

Snapshots are taken only at MTF single-step boundaries or free-run THROTTLE
boundaries — clean instruction boundaries with no interruptibility shadow and no
split-I/O mid-flight — mirroring the KVM path's "snapshot only at reproducible
boundaries" guarantee. Deferred: VMCS interruptibility/activity/pending-debug state
and an in-flight `VM_ENTRY_INTR_INFO` (our injections are only *scheduled* in
software at snapshot time, which *is* captured), needed only if a future guest can be
snapshotted mid-STI-shadow.

### 4.6 The canonical FNV fingerprint

The verify harness (M9, userspace-only) computes a canonical fingerprint over the
full state, mirroring the KVM path's `Snapshot::fingerprint()`. It feeds
**meaningful fields in fixed little-endian order** so structure padding never enters
the hash: the register file, then the TSC/clock/injection/MSR/sregs/device fields in
a fixed order, then a length-prefix and the guest RAM bytes (and the disk image /
captured TX where relevant). It uses a self-contained **FNV-1a**, not BLAKE3 — the
harness only needs to *detect* divergence, so no crypto library is pulled in. (The
KVM path uses BLAKE3 for the same role.)

### 4.7 Instruction-timed interrupt injection

`HVX_INJECT` schedules a one-shot vector at an exact `at_icount`; the run-loop arms
`VM_ENTRY_INTR_INFO = 0x80000000 | vector` on the entry where `icount >= at_icount`,
and the CPU delivers it through the guest IDT. Two hardware subtleties are
load-bearing:

- **External-interrupt exiting is mandatory** (`PINBASED_EXTINT_EXITING`). In
  non-root operation, physical host interrupts are delivered into the *guest's* IDT
  whenever guest `RFLAGS.IF=1`, gated by the *guest's* IF — so `local_irq_save` does
  **not** protect the host. Without this control a guest with IF=1 steals L1's timer
  and RCU-stalls it. With it, physical interrupts VM-exit (reason 1); the run-loop
  services them inline via a `sti; nop; cli` window (host IDT handles them) and does
  **not** count them as guest instructions.
- **Injecting an external interrupt requires guest `RFLAGS.IF=1`** — VM-entry
  enforces it (else invalid-guest-state, exit `0x80000021`).

The injection vector ceiling was raised from 0x20 to 0xFF at M15 (Linux installs a
256-vector IDT), while the module's own built-in IDT stays at gates 0..0x20. An
**interruptibility gate** only injects when guest IF=1 and there is no STI/MOV-SS
shadow, else it sets interrupt-window exiting and waits for reason 7. A minimal LAPIC
**in-service + EOI** model ensures at most one interrupt is injected until the guest
writes EOI (MSR 0x80B), so a second device vector is never nested inside a running
handler.

### 4.8 In-kernel CPUID / MSR emulation

Handled in the run-loop's retired block (emulate → advance RIP by the exit
instruction length → `retired = true` so it counts on the clock → VMRESUME), so no
CPUID/MSR exit reaches userspace:

- **CPUID** (`hvx_cpuid`): a fixed synthetic table. Leaf 0 = max-leaf + "GenuineIntel";
  leaf 1 = family 6, EDX = FPU|PSE|TSC|MSR|PAE|CX8|APIC|PGE|CMOV|CLFLUSH|FXSR|SSE|SSE2
  (**deliberately no XSAVE/AVX/OSXSAVE**, so the guest never runs XSETBV), ECX now
  also advertises x2APIC + TSC-Deadline + the hypervisor-present bit (M15); leaves
  0x15/0x16 give a known TSC frequency; leaf 0x40000000 = "KVMKVMKVM" with an empty
  feature leaf (M15, see §6); leaf 0x80000001 EDX = SYSCALL|NX|LM.
- **RDMSR/WRMSR** (`hvx_rdmsr`/`hvx_wrmsr`): IA32_EFER round-trips through the VMCS
  `GUEST_IA32_EFER` field; IA32_TSC returns/rebases the virtual TSC; FS/GS base
  reflect into the VMCS guest fields (M14); a curated software store
  (`sw_msr[HVX_NR_SW_MSR]` = 11 slots: STAR/LSTAR/CSTAR/SFMASK/FS_BASE/GS_BASE/
  KERNEL_GS_BASE/SYSENTER_CS/ESP/EIP/MISC_ENABLE) backs the rest; x2APIC MSRs route
  to the LAPIC model (§5); any other MSR reads 0 / drops the write (**no #GP**, to
  keep a booting guest alive).

The SYSCALL MSRs (STAR/LSTAR/CSTAR/SFMASK/KERNEL_GS_BASE) are additionally loaded
into the *physical* MSRs around each run, because `SYSCALL` and `swapgs` do **not**
VM-exit and are not in the VMCS (`wrmsrl_safe`, since a guest value may be
non-canonical).

### 4.9 sregs persistence

Before M13, `setup_vmcs` rebuilt CR0/CR3/CR4/EFER/DR7/segments/GDTR/IDTR from
hardcoded flat defaults every entry, discarding any guest change on ioctl return.
Now a software `struct hvx_sregs` is seeded on CREATE, **loaded** into the VMCS each
entry, and **saved back** (VMREAD) after each exit, so a guest that reprograms CR3,
a segment, or LGDT/LIDT keeps the change across `HVX_RUN` boundaries — the last
prerequisite for a real OS to free-run. Load-bearing details: `fixed_cr()` always ORs
the `IA32_VMX_CRn_FIXED` bits onto CR0/CR4 (so a user SET_SREGS can't fault entry);
CR4.VMXE is hidden from the guest via the CR4 guest/host mask + read-shadow (M14, so
the decompressor's CR4 masking doesn't drop VMXE and triple-fault); and `seg_ar()`
maps a not-present segment to the VMCS "unusable" bit (M14, since the ABI's 16-bit
`ar` can't hold bit 16 — real Linux nulls DS/ES/SS in long mode).

---

## 5. Device models

All device models are **shared per-VM** (like guest RAM), and every one is **inert
until attached / used**, so a VM that never touches a device hashes identically and
preserves earlier fingerprints.

### 16550 UART (serial console, in + out)

Guest `IN`/`OUT` are forced to VM-exit (unconditional I/O-exiting, M10), so guest
serial traffic never escapes to L1's physical ports. `hvx_pio` emulates a 16550:
`OUT` to 0x3f8 (THR) accumulates into the per-VM console (`HVX_GET_CONSOLE`); the
register model tracks DLAB (so divisor writes aren't captured as text), a
carrier-present modem-status register (`0xb0` = DCD|DSR|CTS — without DCD the tty
hangs up and init's stdin EOFs), and an IIR that reports THR-empty so the serial ISR
transmits. Serial **input** (M18): `HVX_PUT_INPUT` fills a ring; a guest `IN` from the
RBR pops it, LSR bit 0 reflects data-ready, and once the guest enables ERBFI and
unmasks IOAPIC redirection entry 4 (ISA IRQ4 → COM1), a queued byte raises COM1's IRQ
through the instruction-timed injection path — making a real Linux serial console
interactive.

### LAPIC / IOAPIC / PIC — via x2APIC-over-MSR

The Local APIC is **not** emulated as MMIO. Instead the guest is *forced into
x2APIC mode* (M15), where the whole LAPIC register file is MSR-mapped (MSRs
0x800–0x83F), so it is emulated entirely through the existing RDMSR/WRMSR trap — no
LAPIC MMIO, no shadow page, no APICv. x2APIC registers are read-as-written except
ID/VERSION/PPR/LDR/CCR (computed) and EOI/ICR/SELF-IPI (side effects). The **IOAPIC**
(0xfec00000) is the one MMIO device, EPT-unmapped, decoded from EPT-violation exits
by an in-kernel MOV decoder; redirection entries default masked. The **8259 PIC**
ports (0x20/0x21/0xA0/0xA1) store/return the IMR masks. The **TSC-deadline timer**
(MSR 0x6E0) is wired to the M8 injection path with the interruptibility gate and
LAPIC in-service/EOI. Cross-vCPU **IPIs** (M19): an x2APIC ICR write routes a
deterministic instruction-timed injection to the target vCPU's LAPIC.

### PCI config space + MSI-X

A standard 0xCF8/0xCFC config mechanism (extended into `hvx_pio`) exposes the virtio
functions. A minimal Intel host bridge at 00:00.0 (`[8086:1237]`, class 0x060000)
exists only when a device is attached, because Linux's conf1 sanity check rejects the
bus unless it finds a host-bridge/VGA/Intel device (M21). Modern virtio-pci prefers
**MSI-X**: each function advertises an MSI-X capability (table + PBA in BAR0);
completions are delivered as a **direct, edge-triggered LAPIC injection** of the
queue's message-data vector at the deterministic completion instruction — no IOAPIC,
no INTx, which sidesteps the `acpi=off` INTx-routing problem entirely. INTx-through-
IOAPIC remains available for the hand-written tests.

### virtio-blk disk (M20/M21) + deterministic fault injection (M22)

`struct hvx_vblk` is the **transport-agnostic** virtio-blk core: one split virtqueue
serviced synchronously at a kick against an in-memory backing image
(`HVX_SET/GET_DISK`, ≤ 1 MiB, snapshot-able). `virtio_blk_process` walks each
`[header][data…][status]` descriptor chain, copies between guest RAM and the disk,
writes the status + used-ring entry, and **schedules the single completion at
`kick_icount + HVX_VBLK_LATENCY` (16 instructions)** — a pure function of the clock,
delivered as MSI-X (or INTx for the tests). Two transports feed the same core: a
Phase-A test PIO doorbell (ports 0x500–0x52f, validated in isolation first) and a
Phase-B modern virtio-pci MMIO BAR at 0xfe000000. Real Linux binds `virtio_pci` +
`virtio_blk`, creates `/dev/vda`, and reads it (M21).

**Fault injection** (M22, as in FoundationDB's simulation): a seeded policy
(`HVX_SET_FAULT`) perturbs virtio-blk I/O so any bug replays bit-for-bit. Every fault
decision is a **pure function of `(fault_seed, per-request index)`** via a
self-contained splitmix64 (`fault_roll(seed, k) = splitmix64(seed + (k+1)·GOLDEN)`);
the completion-reorder permutation is keyed by `(seed, per-kick index)`. Classes
(bitmask, applied in a fixed order so userspace can replicate the schedule):
`DELAY` (push completion out 1..max_delay instructions), `ERROR` (return
`VIRTIO_BLK_S_IOERR`, skip transfer), `DROP` (write acks OK but doesn't persist —
silent durability bug), `REORDER` (seeded Fisher-Yates used-ring order), `TORN`
(persist only the first sector of a multi-sector write). Inert when `classes == 0`.

### virtio-net (M23) + network fault injection (M24, in progress)

`struct hvx_vnet` is a second virtio-PCI function at bus 0 / dev 4
(`[1af4:1041]`, BAR0 0xfe020000), reusing the same PCI + MMIO-BAR + MSI-X foundation,
with two virtqueues — **receiveq (0)** and **transmitq (1)**. Features are
deliberately minimal: `VIRTIO_NET_F_MAC` + `VIRTIO_F_VERSION_1` only — no
checksum/GSO/TSO/mergeable-rx-bufs, no controlq, and **no `F_STATUS`** (with STATUS,
`virtnet_probe` starts carrier-off and waits for a config-change interrupt this device
never generates, so eth0 never becomes RUNNING — M23 wall #1). Inbound: `HVX_PUT_RX`
pushes one scripted frame into a per-device FIFO (cursors ride in the snapshot);
`virtio_net_rx_deliver` writes the 12-byte `virtio_net_hdr_v1` + frame + used-ring
entry and arms the completion MSI-X at `kick_icount + HVX_VNET_LATENCY`. Outbound:
`virtio_net_tx_process` captures the payload host-side (`HVX_GET_TX`). A real driver
posts a deep RX ring and kicks once at open, so `HVX_NET_PACED` **trickles** one
frame per fixed instruction interval so a later frame arrives after the interface is
fully up (M23 wall #2). Real Linux binds `virtio_net`, brings up eth0 with a static
IP, and answers a scripted ARP — deterministically.

**Network fault injection** (M24, *in progress*) is symmetric to M22 and applies to
both directions. The ABI is already defined (`HVX_SET/GET_NET_FAULT`,
`struct hvx_net_fault`, the `nfault_*` snapshot fields, and the `HVX_NFAULT_*`
classes: DROP / DELAY (RX only) / CORRUPT / DUPLICATE / REORDER). The per-packet roll
is keyed by direction *and* packet index — `splitmix64((seed ^ dir_salt) +
(index+1)·GOLDEN)`, with `dir_salt = 'R'<<48` for RX and `'T'<<48` for TX — keeping
the two directions independent; CORRUPT's byte position and XOR value and DELAY's
latency all come from the same roll. The kernel PRNG helpers (`net_fault_roll`,
`net_reorder_roll`, `vnet_fault_decide`) already exist in `hvx.c`. Treat this
milestone as landing; do not rely on any half-written M24 code.

---

## 6. The real-Linux boot path

`hvx` boots the same NixOS 6.6.35 bzImage the KVM path boots, reusing that recipe
(`hv/hvx/hvx_boot.h`, factored from the KVM path's `boot.rs`/`detlinux.rs`):

- **64-bit bzImage protocol**: protected-mode kernel loaded at `0x100000`, **entry
  `0x100200` directly in 64-bit long mode** (so no real-mode entry and no
  unrestricted-guest control — a major de-risk). `boot_params`/zero-page at `0x10000`
  (RSI), e820 + cmdline at `0x20000`, MP table at `0x9FC00`, initial long-mode
  GDT/paging, and CPUID/MSRs synthesized deterministically (hvx owns CPUID).
- Cmdline: `console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off
  x2apic_phys`.

The loader (`hv/hvx/hvx_linux.c`) free-runs `HVX_RUN`, re-running on `HVX_EXIT_THROTTLE`
(normal — the 65536-exit cap fires many times) and draining `HVX_GET_CONSOLE` each
iteration. On any unexpected stop it dumps the exit reason/qualification/GPA/RIP and
the faulting instruction bytes (translating a kernel-virtual RIP to guest-physical
via `phys = rip − 0xffffffff80000000`). **The entire boot bring-up was done by
running to a wall, decoding the faulting instruction, and fixing it.** The walls, in
order:

| Wall | Fix | M |
|---|---|---|
| Decompressor `head_64.S` masks CR4, dropping **VMXE** → #GP → triple-fault at 0x10027e | CR4 guest/host mask owns VMXE; read-shadow shows it as 0 | M14 |
| Guest sets **GS base** via WRMSR, then reads the stack canary from `%gs:0x28`; base stayed 0 → triple-fault | Reflect FS/GS base MSRs into the VMCS guest fields | M14 |
| Real Linux nulls DS/ES/SS → **unusable** segments; the 16-bit ABI `ar` truncated bit 16 → invalid-guest-state after a THROTTLE | `seg_ar()` maps not-present → VMCS unusable bit | M14 |
| Guest uses SSE early, clobbering host XMM | `kernel_fpu_begin()/end()` around the run | M14 |
| `apic_read(APIC_ID)` on the LAPIC MMIO page → EPT violation at 0xfee00020 | **Force x2APIC**; emulate the LAPIC through the MSR trap | M15 |
| `enable_IR_x2apic()` finds no IR on "bare metal" → disables x2APIC → falls back to unmapped xAPIC MMIO → panic | Present a feature-less **KVM hypervisor signature** (leaf 0x40000000 = "KVMKVMKVM") so Linux keeps x2APIC | M15 |
| PIT calibration busy-wait / RDTSC calibration | CPUID.15H/16H give a known TSC frequency → known-freq path | M15 |
| Timer, first userspace syscall, LAPIC-timer phase | TSC-deadline timer → M8 injection; load guest SYSCALL/KERNEL_GS_BASE MSRs into hardware; LAPIC in-service/EOI | M15 |
| PCI bus rejected (`No config space access function`) | Present a minimal Intel host bridge at 00:00.0 | M21 |
| Disk read hangs (idle HLT with no timer armed) | HLT handler jumps the clock forward to a pending disk completion | M21 |

**Result**: a real Linux kernel boots to the busybox `~ #` shell prompt (M15,
interactive at M18), and — the headline result (**M16**) — the whole boot is a
rigorous determinism result. `hv/hvx/hvx_boot_verify.c` boots in FAST free-run, stops
at a well-defined console marker, and fingerprints the **full** end state (console +
snapshot + all 256 MiB RAM). Two fresh boots and a mid-boot branch-from-snapshot all
produce the **same** fingerprint, and the two consoles are byte-for-byte equal — past
the RDTSC-calibration region that walled the KVM path. This is whole-boot (free-run)
determinism, not instruction-exact bisection (which would need the blocked PMU fast
clock, §9, and is moot here since free-run is already fully deterministic).

---

## 7. SMP — serialized execution + instruction-quantum round-robin

You **cannot** run vCPUs in parallel on physical cores and stay deterministic: real
concurrent memory interleaving is nondeterministic. Following the state of the art
(rr, Hermit), `hvx` (M19) **serializes** the vCPUs onto one execution
context and interleaves them by a deterministic **quantum measured in the MTF
instruction clock**: userspace runs vCPU0 for Q instructions (single-step), then
vCPU1 for Q, round-robin, via `HVX_SET_VCPU(i)` + `HVX_RUNC(single_step,
max_insns=Q)`. The interleaving becomes a pure function of the per-vCPU instruction
counts, so it reproduces bit-for-bit; changing Q changes the interleaving in a still-
reproducible way.

The refactor split `struct hvx_vm` into a shared VM plus an array of
`struct hvx_vcpu`. **Per-vCPU** (private): VMCS/VMXON pages, register file, `sregs`,
virtual TSC, `icount`, pending injection, the software MSR store, and its own x2APIC
(ID = vCPU index). **Shared**: guest RAM + EPT, the console, the serial input ring,
and the IOAPIC/PIC/UART/virtio device model. **Only one vCPU is ever in VMX-root at a
time**, so cross-vCPU writes (an IPI landing on another vCPU's injection slot) are
race-free with no locking beyond the per-fd mutex. IPIs are delivered via the x2APIC
ICR (physical destination + self/all/all-but-self shorthands); INIT/SIPI/NMI are not
modeled (no hardware AP bringup — the SMP test starts each vCPU at its entry point).

`hvx_smp_test.c` proves it with a lock-free shared-counter race: two vCPUs each do 20
non-atomic increments of one shared counter; Q=200 (serialized) → counter 40, Q=1
(finest interleave) → counter 20, each reproducible under its Q, and a cross-vCPU IPI
fires the target's ISR exactly once. **Honest scope**: this is the deterministic
multi-vCPU *foundation* on small hand-written guests. A deterministic SMP boot of
real Linux is **not** the target — MTF-serialized SMP is far too slow without the
(blocked) PMU fast clock, and needs full INIT/SIPI AP bringup. `n_vcpus == 1` is
byte-for-byte the old single-vCPU behavior, so the real-Linux path is unaffected.

---

## 8. The verify / determinism harness

`hvx` reproduces the KVM path's verification model natively and userspace-only. Three
checks, each over the canonical FNV fingerprint (§4.6):

1. **replay** — two fresh runs from the same image fingerprint identically.
2. **round-trip** — `SET_SNAPSHOT` then re-fingerprint == the same value (the
   snapshot is lossless).
3. **branch** — step to a midpoint (`HVX_RUNC max_insns`), snapshot, finish →
   `fp_orig`; restore, finish → `fp_branch`; assert `fp_orig == fp_branch ==
   fp_full`. This is the time-travel / exploration capability.

### Baseline fingerprints (regression tripwires)

Collect these from `CLAUDE.md`; a change to any is a regression to investigate.

**Whole-boot (real Linux, free-run, full end state):**

| Boot | Fingerprint | Harness |
|---|---|---|
| No-disk boot | `640ece2f900fe5b3` | `hvx_boot_verify` |
| With-disk boot (virtio-blk over MSI-X) | `36583d533d7c989b` | `hvx_disk_boot_verify` |
| With-disk boot under seeded I/O-error faults | `b49eb759b9d607aa` | `hvx_disk_boot_verify HVX_FAULT=0x2222` |
| With-net boot (virtio-net, scripted ARP) | `fd5663b60bf9412e` | `hvx_net_boot_verify` |

> Historical note: the pre-M21 no-disk value `1bdf266a0db52bf7` is **not
> recoverable** — it was tied to a non-reproducible `initramfs.cpio.gz` blob (cpio +
> gzip embed mtimes). `build-linux.sh` was made byte-reproducible (fixed mtimes,
> sorted names, `gzip -n`) and the busybox store path pinned, so `640ece2f900fe5b3`
> and `36583d533d7c989b` regenerate identically. The inert-device guarantee the old
> value stood for is preserved.

**Hand-written guests (regs + device + RAM [+ disk/TX]):**

| Test | Fingerprint(s) | Proves |
|---|---|---|
| `hvx_verify` | `99393734aa84b0f5` | replay/round-trip/branch on the RDTSC-accumulator loop |
| `hvx_virtio_iso_test` (Phase A, PIO) | `7e586473f6e494d7` | deterministic disk completion, isolated |
| `hvx_virtio_pci_test` (Phase B, INTx) | `3f95ef833747f3ec` | PCI transport + completion INTx |
| `hvx_fault_test` (M22) | `91170d4dbf101c5b` (seed A), `88f680fc22cc9bb0` (seed B), `9090286f3fc59682` (faults off = M20/M21) | seeded fault schedule reproducible; seed controls it; disabled == baseline |
| `hvx_net_test` (M23) | `0e2b7acf2f4f94ee` | virtio-net RX (MSI-X) + TX capture |
| `hvx_smp_test` (M19) | `27ead8c880ef7820` (Q=200), `88ee51bd9f8e40e0` (Q=1), `d042bb01201c269d` (IPI) | quantum controls interleaving, reproducibly |

The KVM path also has its own baselines (`det64` = `ed882eafa4643e09`, `timer64
--timer 200 --insn-clock` = `0b3b7c95eadbcfae`), unrelated to hvx.

---

## 9. The wall and known limits

### PMU fast instruction clock — BLOCKED on this hardware (M17)

The intended successor to the slow MTF clock is a **guest-only PMU counter**
(INST_RETIRED.ANY) — the near-native technique the KVM path uses (`src/pmu.rs`,
~33× faster). On the build machine it is **blocked at the hardware+kernel level, not a config
toggle**:

- The L0 CPU is a **13th-Gen i7-13700HX (Raptor Lake, HYBRID: P-cores + E-cores)**.
- KVM 6.6.35 hardcodes `if (X86_FEATURE_HYBRID_CPU) enable_pmu = false;`, because
  hybrid P/E-core counters are unreliable to virtualize. So the nested L1 vCPU has
  **no architectural PMU at all**: `CPUID.0AH = 0` and every PMU MSR `#GP`s.
- Proven not a toggle: reloading `kvm` with `enable_pmu=1` succeeds (all rc=0) yet
  `/sys/module/kvm/parameters/enable_pmu` stays `N`.

The VMX load/save-`PERF_GLOBAL_CTRL` controls **are** present, so the design is
directly implementable on a **non-hybrid host** (a Xeon or pre-Alder-Lake Core). The
recommendation on the build machine is **Option 3: stay on the exact MTF clock (M5)** — it is
exact and deterministic, only slow. Whole-boot free-run determinism (M16) does not
need the fast clock. What the fast clock would unblock: single-step ICOUNT boots →
instruction-exact divergence bisection à la the KVM path, and an ICOUNT-tied boot
TSC. `run-l1.sh` already launches with `pmu=on`, which becomes live automatically on
a host that grants the vPMU. Expected next walls once on a vPMU-capable host:
nested-counting correctness (cross-check against the M5 201-instruction MTF ground
truth), then LAPIC-timer phase and jitter entropy.

### Page-table Accessed/Dirty-bit determinism hazard (fixed)

A rare, host-load-dependent nondeterminism surfaced post-M22 as a flaky
`hvx_virtio_pci_test`. Root cause (present since M20): the 4th-GiB idmap PTE at
`pdpt[3]` covers the **EPT-unmapped** MMIO window (virtio BARs + IOAPIC), so a guest
MMIO access is a page walk that *faults at the EPT stage*. Whether the CPU writes the
**Accessed bit** into that guest PTE on such a faulting walk is implementation-
specific and, under nested VMX, host-load-dependent — a 1-byte flip the fingerprint
hashes. **Fix**: `HVX_SET_DISK` / `HVX_NET_ATTACH` **pre-set A|D** in the PTE
(`pdpt[3] = (3<<30) | 0x83 | 0x60`), so the walker finds them already set and never
writes — making the PTE a pure function of the guest. Verified stable across 40
back-to-back runs under concurrent kernel-build load. (The boots pass
`*_NO_IDMAP` / attach nothing, so their fingerprints are untouched.) This is a good
worked example of how subtle the determinism requirement is: even a hardware-set
page-table bit is an observable input that must be tamed.

### Other deferred / honest-scope items

- **Snapshot completeness**: VMCS interruptibility/activity/pending-debug state and
  an in-flight (armed-but-not-delivered) `VM_ENTRY_INTR_INFO` are not captured — safe
  today because snapshots are taken at clean boundaries where injections are only
  *scheduled* in software (which is captured). Add if a guest can be snapshotted
  mid-STI-shadow. FPU/XMM/extended-CR state is likewise deferred (flat guests and the
  boot path don't need it in the snapshot; the FPU is protected at runtime by
  `kernel_fpu_begin/end`).
- **Disk**: backing image capped at 1 MiB; real-Linux writes through `/dev/vda` and
  filesystem-level access are untested (the disk `/init` reads raw). The fault delay
  model is one shared per-kick completion pushed out by summed per-request delays,
  not N independent IRQs; reorder is capped at 16 requests/kick.
- **Network**: proves inbound ARP → reply and TX capture; TCP/UDP flows, multi-frame
  NAPI bursts, and GSO/mergeable offloads are out of scope. RX to a real OS uses
  PACED delivery (a deterministic device-timing trickle), not a change to Linux.
- **Cooperative vs. transparent**: unlike the KVM path (where guests draw
  time/randomness from a paravirtual port), hvx traps `RDTSC`/CPUID/MSRs directly, so
  a real, unmodified OS runs. `RDRAND`/`RDSEED` transparent handling is not
  separately documented here; the boot guests do not rely on it.

---

## 10. Milestone timeline

| M | Status | What it added |
|---|---|---|
| M1 | done | VMXON → VMLAUNCH a trivial `cpuid; hlt` guest → VMXOFF, entirely in `module_init`. |
| M2 | done | EPT: the guest runs in its own physical address space. |
| M3 | done | `/dev/hvx` device: guest RAM/regs/run from userspace (mini-KVM ioctl API). |
| M4 | done | **Deterministic RDTSC interception** — the wall the KVM path could not cross. |
| M5 | done | MTF instruction clock (`HVX_RUNC`, `icount`). |
| M6 | done | Instruction-clock TSC (`HVX_TSC_ICOUNT`). |
| M7 | done | Snapshot / branch-from-snapshot (minus RAM, which rides via mmap). |
| M8 | done | Instruction-timed interrupt injection (`HVX_INJECT`) + guest GDT/IDT/handler. |
| M9 | done | Canonical FNV fingerprint + replay/round-trip/branch verify harness (userspace-only). |
| M10 | done | PIO trap + 16550 UART console (Phase 1 of the L2-Linux road). |
| M11 | done | Guest RAM to 256 MiB. |
| M12 | done | Deterministic in-kernel CPUID + RDMSR/WRMSR. |
| M13 | done | Privileged-state (sregs) persistence + `HVX_GET/SET_SREGS`. |
| M14 | done | First real Linux serial output as L2 (four decompressor/early-init walls fixed). |
| M15 | done | Real Linux boots to the busybox shell prompt (x2APIC-over-MSR LAPIC, timer, device model). |
| M16 | done | **Whole-boot determinism + branch-from-snapshot on real Linux** (two boots + branch identical). |
| M17 | **blocked** | PMU fast clock — blocked by hybrid L0 CPU + KVM's hybrid gate (not a config toggle). |
| M18 | done | Serial input → interactive Linux shell (P6.4). |
| M19 | done | Deterministic SMP / multi-vCPU (serialized + instruction-quantum round-robin + IPIs). |
| M20 | done | Deterministic virtio-PCI block device (transport-agnostic core + PIO/PCI transports, INTx). |
| M21 | done | Real Linux uses the virtio-blk disk over **MSI-X**, deterministically (M20 stretch). |
| M22 | done | Deterministic **disk fault injection** (seeded splitmix64; delay/error/drop/reorder/torn). |
| M23 | done | Deterministic **virtio-net** device (RX via MSI-X, TX capture; real Linux answers a scripted ARP). |
| **M24** | **in progress** | Deterministic **virtio-net fault injection** (drop/delay/corrupt/duplicate/reorder), symmetric to M22. ABI + kernel PRNG helpers landed; treat the codebase as complete through M23. |

---

## Appendix — key files

| File | Role |
|---|---|
| `hv/hvx/hvx.c` | The module: VMX/EPT + `/dev/hvx` + deterministic RDTSC/MTF/TSC + CPUID/MSR + sregs + x2APIC/IOAPIC/UART + SMP + virtio-blk/net + fault injection. |
| `hv/hvx/hvx_abi.h` | Shared kernel↔userspace ioctl ABI, structs, and device constants. |
| `hv/hvx/hvx_boot.h` | Shared bzImage-loader setup (the byte-for-byte KVM-path boot recipe). |
| `hv/hvx/hvx_linux.c` | The bzImage loader / real-Linux runner (free-run loop, console drain, fault dump). |
| `hv/hvx/hvx_boot_verify.c`, `hvx_disk_boot_verify.c`, `hvx_net_boot_verify.c` | Whole-boot determinism harnesses (no-disk / with-disk / with-net). |
| `hv/hvx/hvx_*_test.c` | Per-milestone unit tests (M3–M23) proving each capability + fingerprint. |
| `hv/hvx/hvx_pmu_probe.c` | M17 capability probe (reports the vPMU is unavailable on the build machine). |
| `hv/hvx/deploy-hvx.sh`, `run-l1.sh`, `run-*-demo.sh`, `cloud-init/` | L1 sandbox bring-up, build+load+test, and demo runners. |
| `src/*.rs` | The **predecessor** KVM-based hydo (design reference; see `README.md`). |
