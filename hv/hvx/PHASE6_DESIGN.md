# Phase 6 (Milestone 15) design — deterministic LAPIC + IOAPIC for hvx

Blueprint for the Phase 6 implementation agent. Grounded in `hv/hvx/hvx.c`, `hv/hvx/hvx_abi.h`,
`hv/hvx/hvx_linux.c`, `src/boot.rs`, `src/detlinux.rs`, `src/state.rs`.

## The load-bearing decision: force x2APIC — emulate the LAPIC via the MSR trap, not MMIO
hvx already traps **every** RDMSR/WRMSR (no MSR bitmap; `setup_vmcs` sets only HLT/RDTSC/IO/SECONDARY
+ pin EXTINT, so all MSRs VM-exit to `hvx_rdmsr`/`hvx_wrmsr`). In x2APIC mode the whole Local APIC
register file lives at **MSRs `0x800`–`0x8FF`** (x2apic MSR = `0x800 + (xapic_offset>>4)`), so LAPIC
emulation becomes a pure extension of those two handlers — **no EPT trap, no MOV decode, no page-table
walk, no shadow page, no APICv**. This needs *nothing* from L0/L1 hardware (biggest de-risk vs nested).
The only genuinely-MMIO device left is the **IOAPIC at `0xFEC00000`** (rare, 2 registers).

Do NOT pursue the VMX "virtualize APIC"/APICv secondary controls — nested KVM in L1 likely won't expose
them, and they still require the register model. x2APIC-by-MSR sidesteps all of it.

**Determinism prerequisite (restated everywhere):** the deterministic boot must run on the instruction
clock — `HVX_RUNC` with `HVX_RUN_SINGLE_STEP` and `HVX_TSC_ICOUNT` — so `icount` is a true retired count
and the virtual TSC is `base + icount*step`. Iterate the device model in fast free-run (`HVX_RUN`), then
prove determinism single-stepped. (Fast single-step needs a future PMU port; out of Phase 6 scope.)

## Forcing x2APIC (in `hvx_cpuid` + MSR model)
1. CPUID.01H:ECX **bit 21 (x2APIC) = 1** (currently ECX=0) and **bit 24 (TSC-Deadline) = 1**. (Neither pulls in XSAVE/AVX.)
2. `IA32_APIC_BASE` (MSR `0x1B`) returns `0xFEE00000 | BSP(1<<8) | EXTD(1<<10) | EN(1<<11)` = `0xFEE00D00`
   so `check_x2apic()` sees "enabled by BIOS" and switches without IRQ remapping. Accept writes, keep EXTD|EN latched.
3. Add `x2apic_phys` to the loader cmdline (`hvx_linux.c` DEFAULT_CMDLINE).
4. Advertise **CPUID.15H** (EAX=1,EBX=64,ECX=25000000 → 1.6 GHz core crystal) and **CPUID.16H** so Linux
   takes the *known-TSC-frequency* path (`X86_FEATURE_TSC_KNOWN_FREQ`) — no empirical calibration busy-wait
   (which under single-step would be astronomically slow) and TSC-deadline clockevent derives mult/shift from it.
   Raise max basic leaf to ≥0x16 (currently 0x7). NOTE: this changes `hvx_cpuid_test.c` — update it.

## LAPIC register model (per-VM `struct hvx_lapic`; route MSRs 0x1B, 0x6E0, 0x800–0x8FF)
| MSR | reg | behavior |
|---|---|---|
|0x1B|IA32_APIC_BASE|return 0xFEE00D00 (EXTD|EN|BSP); writes latch|
|0x802|ID|read-only 0|
|0x803|VERSION|0x00050014 (ver 0x14, max-LVT 5)|
|0x808|TPR|r/w store|
|0x80A|PPR|read 0|
|0x80B|**EOI**|write-only; clear highest ISR bit; must never fault (hot path)|
|0x80D|LDR|read-only 0x00000001 for id0; ignore writes|
|0x80F|SPIV/SVR|read-as-written; bit8 = SW-enable|
|0x810–0x827|ISR/TMR/IRR|read 0 unless modeled|
|0x828|ESR|read 0; write clears|
|0x82F,0x833,0x834|LVT CMCI/Thermal/PMC|read-as-written (masked)|
|0x830|**ICR** (single 64-bit MSR)|INIT/SIPI→no APs, ignore; fixed-to-self→inject vector via M8; no delivery-status poll in x2apic|
|0x832|**LVT Timer**|r/w; [7:0]=vector, bit16=mask, [18:17]=mode(00 one-shot/01 periodic/10 TSC-deadline)|
|0x835,0x836,0x837|LVT LINT0/LINT1/Error|read-as-written (not delivered at boot)|
|0x83F|**SELF IPI**|write-only; inject `val&0xff` via M8 now|
|0x6E0|**IA32_TSC_DEADLINE**|the timer — see below|

Read-as-written for the rest so nothing faults. ID/VERSION/LDR/PPR computed read-only. Only EOI/ICR/SELF-IPI/DEADLINE have side effects.

## The timer, deterministically: TSC-deadline → M8 injection
Advertise TSC-deadline; Linux writes an absolute TSC to `IA32_TSC_DEADLINE` (0x6E0). Convert:
`deadline_icount = (deadline_tsc - vm->tsc_base) / vm->tsc_step` (write 0 disarms). Arm the existing M8
mechanism: `inject_vector = lvt_timer & 0xff; inject_at_icount = deadline_icount; inject_valid = 1` — reusing
the run-loop arming at the `VM_ENTRY_INTR_INFO` site. The handler writes the next deadline → re-arms → periodic
is free. Deterministic by construction (deadline is absolute on a clock that is a pure function of icount).

**Two required fixes to the M8 injection path:**
1. **Lift the vector cap** `INTR_VECTOR_MAX` (0x20) to 0xFF in `HVX_INJECT` — Linux installs its own 256-vector
   IDT (idt_base persists via M13), so the timer vector (~0xEC) is valid.
2. **Eligibility gate.** Before arming, only inject when `GUEST_RFLAGS.IF` set AND `GUEST_INTERRUPTIBILITY`
   bits 0/1 (STI/MOV-SS shadow) clear; otherwise set PROCBASED **interrupt-window exiting** (bit 2) and handle
   `EXIT_REASON_INTERRUPT_WINDOW` (reason 7) to re-check. Delivery lands on the first interruptible instruction
   at/after the due icount — deterministic because the IF/shadow timeline is deterministic. (Mirrors the README
   "injection deferred until after sti" behavior.)

Empirical-calibration fallback (only if a kernel insists): model current-count `CCR = initial - (icount -
start)/(divisor*K)`; both TSC and CCR are pure functions of icount, so the measured ratio is a bit-identical
constant. Pick K so a tick lands every ~50k–500k instructions. TSC-deadline avoids all of this.

## IOAPIC (the one real MMIO) + PIC masking
**IOAPIC** at 0xFEC00000, still faults for free (EPT `l3[3]` for 3–4 GiB is unmapped; guest RAM ≤256 MiB).
On EPT-violation (reason 48): offset = `GUEST_PHYS_ADDR & 0xFF` (0x00=IOREGSEL, 0x10=IOWIN), direction =
EXIT_QUALIFICATION bit0(read)/bit1(write). Minimal MOV decode (Option A): fetch instr bytes at RIP (needs a
small 4-level guest page-table walker from `sregs.cr3` via `gpa_host`), skip optional REX (0x40–0x4F), confirm
opcode 0x89 (store) / 0x8B (load), extract `reg = ((ModRM>>3)&7) | (REX.R<<3)`. **GOTCHA:** x86 reg number
(0=rAX,1=rCX,2=rDX,3=rBX,4=rSP,5=rBP,6=rSI,7=rDI,8..15) ≠ `struct hvx_regs` field order — map through an explicit
table. Store: `iowin_write(sel, (u32)reg)`; load: `reg = (u64)iowin_read(sel)` (32-bit zero-extends). Then
`rip += insn_len; VMW(GUEST_RIP); icount++; continue` (like RDTSC/CPUID handlers — **must bump icount in-kernel**).
Model: `u32 ioregsel; u32 redir[48]; u32 id`. Indirect regs: 0x00=ID, 0x01=VERSION=`0x00170011` (ver 0x11, max-redir 23),
0x10–0x3F = 24 redirection entries (2 dwords each), read-as-written/masked. (Option B shadow-page+MTF documented
but avoided — touching high EPT risks L1 panic.)

**8259 PIC:** extend `hvx_pio` to accept/drop writes to 0x20/0x21/0xA0/0xA1, store the 0x21/0xA1 masks, return
them (or 0xFF) on reads. Linux masks the PICs when it has an IOAPIC; keep everything masked → no spurious IRQ.

**To reach a prompt (output only):** leave all IOAPIC redir entries masked and never deliver an IOAPIC IRQ —
boot completes on the LAPIC timer alone. **Serial input** (interactive) is separable: model `redir[4]` (ISA IRQ4→COM1
per the MP table) and inject that vector via M8 when hydo has an input byte.

MP table (already built by `boot.rs`/`hvx_linux.c`) must stay consistent: LAPIC 0xFEE00000, IOAPIC 0xFEC00000,
version 0x14, 1 CPU apic-id 0 flags 0x03, 16 ISA IRQs INTIN n. apic-id 0 matches the x2APIC id 0 we return.

## Snapshot / fingerprint (fold into `struct hvx_snapshot` + the fingerprint, fixed field order)
LAPIC: apic_base, spiv, tpr, esr, lvt_timer/lint0/lint1/error/pmc/thermal/cmci, ISR/IRR/TMR if modeled,
tsc_deadline (or initial_count/divide/start_icount/lvt_mode for the fallback). IOAPIC: ioregsel, redir[48], id.
PIC: master/slave mask. Injection fields (inject_at_icount/vector/valid) are ALREADY snapshotted — that's what
makes a branched timer fire at the identical instruction. Append at the struct end; mirror the `sw_msr`
BUILD_BUG_ON discipline.

## Phased sub-plan (smallest testable first; P6.0–P6.5 in fast free-run, P6.6 single-stepped)
- **P6.0** confirm the wall: Phase-5 loader stops on EPT-violation (48) with guest_phys in 0xFEE00xxx.
- **P6.1** force x2APIC + CPUID 0x15/0x16 + IA32_APIC_BASE: assert console shows "x2apic enabled by BIOS" and
  it STOPS faulting on 0xFEE00000 (LAPIC now arrives as MSR 0x800+). Temporarily log unknown 0x800–0x8FF.
- **P6.2** LAPIC MSR model + IOAPIC MMIO + PIC mask: assert boot passes local-APIC + IOAPIC setup with no #GP/VMFAIL/EPT-violation, stalls waiting for a tick.
- **P6.3** (milestone) TSC-deadline → M8 injection + eligibility gate + lift vector cap: assert timer fires,
  jiffies advance, scheduler runs, initramfs mounts, `/init` runs, **shell prompt prints**.
- **P6.4** serial input IRQ (redir[4]) → interactive typing echoes.
- **P6.5** snapshot fields + fingerprint round-trip.
- **P6.6** switch to single-step + ICOUNT; run verify/boot-verify → two boots bit-identical PAST the RDTSC wall.

## Risks (nested L1)
- x2APIC-by-MSR needs nothing from the nested platform — definitive reason to prefer it (avoids MMIO decode incl. per-interrupt EOI and ICR).
- Trapping 0xFEE/0xFEC via EPT doesn't touch L1's real APIC (L2 GPAs, unmapped in hvx's EPT; VMXOFF before return).
- Verify at P6.1 that the kernel actually stays in x2APIC ("enabled by BIOS" path); fallback is xAPIC-MMIO decode (Option A) for the LAPIC too.
- `hvx_cpuid_test.c` asserts current CPUID values — update it for the new leaves/bits. KVM-path baselines (det64/timer64 fingerprints) are unaffected (produced by `src/vmm.rs`, not hvx).
- Single-step perf: no PMU fast-path yet → deterministic boot is slow; the CPUID.15H/16H+TSC-deadline choice both makes the timebase deterministic AND removes the calibration busy-wait. Iterate the model in free-run first.
