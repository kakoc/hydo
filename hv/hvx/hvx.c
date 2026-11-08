// SPDX-License-Identifier: GPL-2.0
/*
 * hvx - hydo VT-x milestone 27: flagship on UNMODIFIED Redis over in-kernel loopback -- Redis's documented AOF durability window (appendfsync=always survives, everysec loses the last acked writes) reproduced bit-for-bit under a faithful volatile-cache power loss; builds on the M26 UNMODIFIED-SQLite committed-row-loss flagship (seeded disk fault, replay + branch) and the flush-aware DROP power-loss model
 *
 * A Linux kernel module that owns the VMCS directly (no KVM). Milestones 1 and
 * 2 ran a hardcoded guest blob entirely inside module_init: milestone 1 in the
 * host's address space, milestone 2 in the guest's OWN address space via EPT.
 * Milestone 3 made the module a device: guest memory, registers, and "run"
 * come from userspace (hydo) through /dev/hvx, a tiny KVM-like ioctl API.
 * Milestone 4 used our ownership of the VM-execution controls to enable "RDTSC
 * exiting" and return a DETERMINISTIC virtual TSC (userspace-seeded, see
 * HVX_SET_TSC) instead of the host time-stamp counter.
 *
 * Milestone 5 added the determinism engine's core primitive: an INSTRUCTION
 * CLOCK. With the Monitor Trap Flag (single-step), every guest instruction
 * takes a VM-exit; the run-loop counts these into vm->icount and can stop after
 * exactly N instructions (HVX_RUNC) -- "advance the guest to instruction N".
 *
 * Milestone 6 tied the two together: in TSC mode HVX_TSC_ICOUNT, RDTSC returns
 * base + icount*step, so the guest's time-stamp counter is a function of
 * execution POSITION (instructions retired), not of wall time.
 *
 * Milestone 7 made execution states first-class: HVX_GET_SNAPSHOT captures the
 * full deterministic state minus guest RAM (register file, virtual-TSC state,
 * instruction clock), HVX_SET_SNAPSHOT restores it, and guest RAM rides along
 * via mmap -- so snapshot-at-instruction-N + restore replays bit-for-bit.
 *
 * Milestone 8 makes interrupt TIMING deterministic. The guest now gets a real
 * protected environment (a GDT + IDT + handler built into the low system pages)
 * so it can receive an interrupt. HVX_INJECT schedules a vector to be delivered
 * at an exact instruction count; when the clock reaches it, the run-loop arms
 * VM_ENTRY_INTR_INFO and the CPU delivers the interrupt through the guest IDT on
 * the next entry. The pending schedule is part of the snapshot, so a restored
 * branch injects at the identical instruction. (Milestone 9 added a userspace
 * verify harness -- canonical fingerprint over replay/round-trip/branch -- with
 * no kernel change.)
 *
 * Milestone 10 begins the road to booting Linux as an L2 guest: all guest IN/OUT
 * now VM-exit (unconditional I/O exiting) instead of hitting the host's physical
 * ports, and the module emulates a 16550 UART -- OUT to 0x3f8 accumulates into a
 * per-VM console readable via HVX_GET_CONSOLE. Milestone 11 lifted the guest-RAM
 * cap from 2 MiB to 256 MiB (page-by-page EPT PTs under a fan of EPT PT pages).
 *
 * Milestone 12 makes CPUID and RDMSR/WRMSR DETERMINISTIC and IN-KERNEL -- the
 * last classic sources of "the CPU tells you something about the real host" that
 * would otherwise diverge two runs. hvx now OWNS the CPUID answer: a fixed
 * synthetic table (GenuineIntel, family 6, a curated feature set that omits
 * XSAVE/AVX/OSXSAVE so the guest never runs XSETBV) is written into the guest
 * GPRs and the instruction stepped over -- no CPUID exit reaches userspace. MSRs
 * are likewise handled in-kernel: IA32_EFER round-trips through the VMCS field,
 * IA32_TSC returns the deterministic virtual TSC, a small curated software MSR
 * store (STAR, LSTAR, CSTAR, SFMASK, FS_BASE, GS_BASE, KERNEL_GS_BASE, the three
 * SYSENTER MSRs, and MISC_ENABLE) round-trips writes to reads, and any other MSR
 * reads 0 / drops the write (no #GP). All of these advance the instruction clock
 * like any retired instruction, and the software MSR store rides along in the
 * snapshot.
 *
 * Milestone 13 makes the guest's PRIVILEGED state persist across HVX_RUN calls.
 * Previously setup_vmcs rebuilt CR0/CR3/CR4/EFER/DR7/segments/GDTR/IDTR from the
 * hardcoded flat defaults every entry, so a guest that reprogrammed CR3 (or a
 * segment, or an LGDT) lost the change the moment an ioctl returned. Now a
 * software struct hvx_sregs is seeded on CREATE, LOADED into the VMCS on each
 * entry, and SAVED back (VMREAD) after each exit; HVX_GET/SET_SREGS read/drive it
 * and it rides along in the snapshot. This is the last prerequisite before a
 * guest OS -- which switches address spaces -- can free-run under hvx.
 *
 * The ioctl API (see hvx_abi.h):
 *   HVX_CREATE_VM  allocate guest RAM (<= 2 MiB), build EPT + guest paging + a
 *                  GDT/IDT/handler; state persists on the fd.
 *   mmap(fd)       map that guest RAM into userspace to load code / read results
 *   HVX_SET_REGS   set the guest register file (rax..r15, rip, rsp, rflags)
 *   HVX_GET_REGS   read it back
 *   HVX_SET_TSC    set the deterministic virtual-TSC seed / step / aux / mode
 *   HVX_GET_TSC    read the virtual TSC and the RDTSC-exit count
 *   HVX_RUN        run the guest; RDTSC handled in-kernel, other exits returned
 *   HVX_RUNC       run with an instruction clock: single-step and/or stop after
 *                  N instructions; returns the running instruction count
 *   HVX_GET/SET_SNAPSHOT  capture / restore the full state (minus mmap'd RAM)
 *   HVX_INJECT     schedule an interrupt vector for an exact instruction count
 *   HVX_GET_CONSOLE  read the bytes the guest wrote to the UART (port 0x3f8)
 *
 * The VM's state (guest RAM, EPT, VMCS image, register file) lives across
 * ioctls in a per-fd struct hvx_vm. Each HVX_RUN is self-contained: it does
 * VMXON -> VMLAUNCH -> (exit) -> VMXOFF inside ONE preempt-disabled, IRQ-off
 * section, loading guest GPRs from the software register file and saving them
 * back on exit. We deliberately do NOT keep VMXON held across ioctls: that
 * would force us to pin the fd to a CPU and handle migration/teardown of live
 * VMX-root state; holding it is an optimization for the RDTSC milestone, not a
 * requirement for driving a guest from userspace. Because we VMXOFF between
 * runs and re-establish the full VMCS from the register file each time, every
 * run is a fresh VMLAUNCH from the saved RIP -- equivalent to VMRESUME for our
 * purposes and free of any stale-CPU hazards.
 *
 * This is developed and run INSIDE a throwaway nested L1 guest. A bad VMCS
 * field panics L1, which is expected and harmless; the host is never touched.
 *
 * References: Intel SDM Vol 3C ch. 24-29; KVM arch/x86/kvm/vmx.
 */

#include <linux/module.h>
#include <linux/kernel.h>
#include <linux/init.h>
#include <linux/fs.h>
#include <linux/miscdevice.h>
#include <linux/slab.h>
#include <linux/vmalloc.h>
#include <linux/mutex.h>
#include <linux/spinlock.h>
#include <linux/mm.h>
#include <linux/gfp.h>
#include <linux/string.h>
#include <linux/preempt.h>
#include <linux/irqflags.h>
#include <linux/uaccess.h>
#include <asm/msr.h>
#include <asm/io.h>
#include <asm/nmi.h>		/* register_nmi_handler: chaos_nmi (M156) */
#include <linux/random.h>
#include <asm/fpu/api.h>	/* kernel_fpu_begin/end: protect host FPU/XMM (M14) */
#include <asm/fpu/xcr.h>	/* xgetbv/xsetbv: guest XCR0 (HVX_VM_X86_64_V3) */
#include <linux/timekeeping.h>	/* ktime_get_ns: exit-histogram wall-time (diagnostic) */

#include "hvx_abi.h"

MODULE_LICENSE("GPL");

/* Device name: "hvx" (/dev/hvx). A second copy of this module can be built with
 * -DHVX_DEVNAME='"hvxpt"' and loaded next to it (each run is a self-contained
 * VMXON..VMXOFF section, so two copies never share VMX root on one CPU). */
#ifndef HVX_DEVNAME
#define HVX_DEVNAME "hvx"
#endif

/* Intel PT guest tracing forced from module parameters, for running unmodified
 * harnesses traced (see HVX_VM_PT_TRACE / HVX_SET_PT for the per-VM ioctl). All 0 =
 * off, which is the default: nothing below runs unless a VM asks for tracing. */
static int pt_force;			/* bit0 trace guest CPL0, bit1 trace guest CPL3 */
static int pt_force_mib = 256;		/* trace buffer per VM */
static unsigned long pt_force_lo0, pt_force_hi0, pt_force_lo1, pt_force_hi1;
static int pt_force_noretc;		/* 1: no RET compression (every RET emits a TIP) */
module_param(pt_force, int, 0644);
module_param(pt_force_mib, int, 0644);
module_param(pt_force_lo0, ulong, 0644);
module_param(pt_force_hi0, ulong, 0644);
module_param(pt_force_lo1, ulong, 0644);
module_param(pt_force_hi1, ulong, 0644);
module_param(pt_force_noretc, int, 0644);

/*
 * Determinism self-test knobs ("host chaos"), all 0 = off (the default; nothing below runs
 * then). They make the HOST side of a run deliberately irregular so that any guest-visible
 * dependence on where host-timed exits land shows up quickly:
 *   chaos_ptimer=N   arm the VMX-preemption timer with a random value in [1, N] before every
 *                    entry, so the guest takes extra exits at random instructions;
 *   chaos_preempt=P  at a host-timed (non-retired) exit, leave VMX-root with
 *                    HVX_EXIT_PREEMPT with probability P/1000 (as if the host scheduler
 *                    wanted the CPU) -- exercises the save/re-enter path.
 * A correct hvx gives the same fingerprints with or without them.
 */
static unsigned int chaos_ptimer;
static unsigned int chaos_preempt;
module_param(chaos_ptimer, uint, 0644);
module_param(chaos_preempt, uint, 0644);
/* probe_rand=1: make RDRAND/RDSEED VM-exit (they are not advertised in CPUID, so a
 * guest that runs them anyway would read real hardware randomness). Each one is logged
 * (first 20) and answered with a deterministic value, CF=1. Diagnostic only. */
static unsigned int probe_rand;
module_param(probe_rand, uint, 0644);
/* ft_opt: the cost cuts of hv/hvx/FASTTURN.md, as bits, for A/B timing on one module
 * (none changes any guest-visible result): bit 0 = a landing step keeps PMC0 running
 * instead of re-programming it, bit 1 = no PERF_GLOBAL_STATUS read after a landing step,
 * bit 2 = VMX capability MSRs served from a cache. Default: all on. */
static unsigned int ft_opt = 7;
module_param(ft_opt, uint, 0644);
static inline bool hvx_chaos_preempt(void)
{
	return chaos_preempt && (get_random_u32() % 1000) < chaos_preempt;
}
/* chaos_nmi=N (M156): every N-th VM exit of a PMU-clock vCPU, send this CPU an NMI in root mode,
 * between the exit and the next entry, where a host NMI (the perf-based watchdog's) can arrive; the
 * module's own NMI handler claims it, and perf's handler runs as for any NMI. Testing only: with
 * pmu_guard=0 it makes M156's miscount happen on demand, with pmu_guard=1 no result may change. */
static unsigned int chaos_nmi;
module_param(chaos_nmi, uint, 0644);
static DEFINE_PER_CPU(int, hvx_chaos_nmi_pending);
static int hvx_chaos_nmi_handler(unsigned int type, struct pt_regs *regs)
{
	if (!this_cpu_read(hvx_chaos_nmi_pending))
		return NMI_DONE;
	this_cpu_write(hvx_chaos_nmi_pending, 0);
	return NMI_HANDLED;
}
MODULE_AUTHOR("hydo");
MODULE_DESCRIPTION("hydo VT-x milestone 27: FLAGSHIP on UNMODIFIED Redis (redis-server + redis-cli co-located over the in-kernel 127.0.0.1 loopback) reproducing Redis's documented AOF durability window -- appendfsync=always SURVIVES a faithful volatile-cache power loss while everysec LOSES the last acked writes -- bit-for-bit (replay + branch-from-snapshot). Builds on the M26 UNMODIFIED-SQLite committed-row-loss flagship (seeded virtio-blk DROP fault) and the flush-aware DROP power-loss model (HVX_FAULT_DROP_UNFLUSHED, VIRTIO_BLK_F_FLUSH advertised only when armed); adds a deterministic idle heartbeat + CRNG seed for co-located loopback services. Inert when unused, so every prior fingerprint is preserved");

/* ---- MSRs ---- */
#define MSR_FEAT_CTL            0x0000003a
#define MSR_VMX_BASIC           0x00000480
#define MSR_VMX_PINBASED        0x00000481
#define MSR_VMX_PROCBASED       0x00000482
#define MSR_VMX_EXIT            0x00000483
#define MSR_VMX_ENTRY           0x00000484
#define MSR_VMX_CR0_FIXED0      0x00000486
#define MSR_VMX_CR0_FIXED1      0x00000487
#define MSR_VMX_CR4_FIXED0      0x00000488
#define MSR_VMX_CR4_FIXED1      0x00000489
#define MSR_VMX_PROCBASED2      0x0000048b
#define MSR_VMX_EPT_VPID_CAP    0x0000048c
#define MSR_VMX_TRUE_PINBASED   0x0000048d
#define MSR_VMX_TRUE_PROCBASED  0x0000048e
#define MSR_VMX_TRUE_EXIT       0x0000048f
#define MSR_VMX_TRUE_ENTRY      0x00000490
#define MSR_EFER_               0xc0000080
#define MSR_FS_BASE_            0xc0000100
#define MSR_GS_BASE_            0xc0000101
#define MSR_SYSENTER_CS         0x00000174
#define MSR_SYSENTER_ESP        0x00000175
#define MSR_SYSENTER_EIP        0x00000176

/* Guest-facing MSR indices the module emulates deterministically (milestone 12).
 * IA32_EFER routes through the VMCS field, IA32_TSC returns the virtual TSC, and
 * the rest are backed by a curated per-VM software store (see swmsr_slot). */
#define GMSR_IA32_TSC           0x00000010
#define GMSR_MISC_ENABLE        0x000001a0
#define GMSR_SYSENTER_CS        0x00000174
#define GMSR_SYSENTER_ESP       0x00000175
#define GMSR_SYSENTER_EIP       0x00000176
#define GMSR_EFER               0xc0000080
#define GMSR_STAR               0xc0000081
#define GMSR_LSTAR              0xc0000082
#define GMSR_CSTAR              0xc0000083
#define GMSR_SFMASK             0xc0000084
#define GMSR_FS_BASE            0xc0000100
#define GMSR_GS_BASE            0xc0000101
#define GMSR_KERNEL_GS_BASE     0xc0000102

/* ---- Local APIC (x2APIC, MSR-mapped) + IOAPIC (MMIO) -- milestone 15 ---- */
#define HVX_MSR_APICBASE       0x0000001b
#define HVX_MSR_TSC_DEADLINE   0x000006e0
#define APICBASE_BSP            (1ULL << 8)
#define APICBASE_EXTD           (1ULL << 10)	/* x2APIC enable */
#define APICBASE_EN             (1ULL << 11)	/* global enable */
/* base 0xfee00000 | BSP | EXTD | EN => "x2APIC enabled by BIOS" (0xfee00d00) */
#define APICBASE_DEFAULT        (0xfee00000ULL | APICBASE_BSP | APICBASE_EXTD | APICBASE_EN)

/* x2APIC register MSRs live at 0x800..0x83f (= 0x800 + (xapic_off >> 4)) */
#define X2APIC_BASE_MSR         0x800
#define X2APIC_TOP_MSR          0x83f
#define X2APIC_ID               0x802
#define X2APIC_VERSION          0x803
#define X2APIC_PPR              0x80a
#define X2APIC_EOI              0x80b
#define X2APIC_LDR              0x80d
#define X2APIC_ESR              0x828
#define X2APIC_ICR              0x830
#define X2APIC_LVT_TIMER        0x832
#define X2APIC_CCR              0x839	/* timer current count */
#define X2APIC_SELF_IPI         0x83f
#define LVT_MASKED              (1U << 16)

/* IOAPIC MMIO window (guest RAM is <= 256 MiB, so this GPA is EPT-unmapped and
 * every access faults out to us as an EPT violation we decode in-kernel). */
#define IOAPIC_BASE             0xfec00000ULL
#define IOAPIC_TOP              0xfec01000ULL

#define FEAT_CTL_LOCK           (1ULL << 0)
#define FEAT_CTL_VMXON_OUT_SMX  (1ULL << 2)
#define CR4_VMXE                (1UL << 13)

/* ---- Control-bit selections ---- */
#define PINBASED_EXTINT_EXITING (1U << 0)   /* physical interrupts -> VM-exit */
#define PINBASED_NMI_EXITING    (1U << 3)   /* physical NMIs -> VM-exit */
#define PINBASED_PREEMPT_TIMER  (1U << 6)   /* VMX-preemption timer (chaos_ptimer only) */
#define PROCBASED_INTR_WINDOW   (1U << 2)   /* interrupt-window exiting (M15 inject gate) */
#define PROCBASED_RDTSC_EXITING (1U << 12)
#define PROCBASED_IO_EXITING    (1U << 24)  /* all IN/OUT -> VM-exit (no bitmap) */
#define PROCBASED_HLT_EXITING   (1U << 7)
#define PROCBASED_PAUSE_EXITING (1U << 30)  /* PAUSE -> VM-exit (HVX_VM_PAUSE_EXIT) */
#define PROCBASED_MONITOR_TRAP  (1U << 27)   /* Monitor Trap Flag (single-step) */
#define PROCBASED_SECONDARY     (1U << 31)
#define SEC_ENABLE_EPT          (1U << 1)
#define SEC_UNRESTRICTED_GUEST  (1U << 7)   /* real mode / paging off in VMX non-root (SMP boot) */
#define SEC_CONCEAL_VMX_PT      (1U << 19)
#define SEC_RDRAND_EXITING      (1U << 11)
#define SEC_RDSEED_EXITING      (1U << 16)  /* no PIP/VMCS packets for VMX transitions */
#define EXIT_CONCEAL_VMX_PT     (1U << 24)
#define ENTRY_CONCEAL_VMX_PT    (1U << 17)
/* Intel PT MSRs (SDM vol 3, chapter "Intel Processor Trace") */
#define MSR_RTIT_OUTPUT_BASE    0x00000560
#define MSR_RTIT_OUTPUT_MASK    0x00000561
#define MSR_RTIT_CTL            0x00000570
#define MSR_RTIT_STATUS         0x00000571
#define MSR_RTIT_CR3_MATCH      0x00000572
#define MSR_RTIT_ADDR0_A        0x00000580	/* ADDRn_A = 0x580 + 2n, ADDRn_B = 0x581 + 2n */
#define HVX_RTIT_TRACEEN        (1ULL << 0)
#define HVX_RTIT_OS             (1ULL << 2)
#define HVX_RTIT_USR            (1ULL << 3)
#define HVX_RTIT_CR3EN          (1ULL << 7)
#define HVX_RTIT_TOPA           (1ULL << 8)
#define HVX_RTIT_DISRETC        (1ULL << 11)
#define HVX_RTIT_BRANCH         (1ULL << 13)
#define HVX_RTIT_ADDR_CFG(n)    (1ULL << (32 + 4 * (n)))	/* range n = filter (trace inside) */
#define HVX_RTIT_ST_ERROR       (1ULL << 4)
#define HVX_RTIT_ST_STOPPED     (1ULL << 5)
#define TOPA_END                (1ULL << 0)
#define TOPA_STOP               (1ULL << 4)
#define HVX_PT_CHUNK_ORDER      9		/* 2 MiB output regions */
#define EXIT_HOST_ADDR_SPACE_64 (1U << 9)
#define EXIT_SAVE_IA32_EFER     (1U << 20)
#define EXIT_LOAD_IA32_EFER     (1U << 21)
#define EXIT_LOAD_PERF_GLOBAL   (1U << 12)  /* load host IA32_PERF_GLOBAL_CTRL on exit */
#define ENTRY_LOAD_PERF_GLOBAL  (1U << 13)  /* load guest IA32_PERF_GLOBAL_CTRL on entry */
#define ENTRY_LOAD_DEBUG_CTLS   (1U << 2)   /* load guest DR7 + IA32_DEBUGCTL on entry */
#define DEBUGCTL_FREEZE_WHILE_SMM (1ULL << 14)	/* perf counters stop while in SMM */
#define ENTRY_IA32E_MODE        (1U << 9)
#define ENTRY_LOAD_IA32_EFER    (1U << 15)

/* ---- EPT/VPID capability bits (IA32_VMX_EPT_VPID_CAP) ---- */
#define EPT_CAP_WALK4           (1ULL << 6)
#define EPT_CAP_WB              (1ULL << 14)
#define EPT_CAP_INVEPT          (1ULL << 20)
#define EPT_CAP_INVEPT_SINGLE   (1ULL << 25)
#define EPT_CAP_INVEPT_GLOBAL   (1ULL << 26)

/* ---- CR bits / EFER bits ---- */
#define CR0_PE_ (1UL << 0)
#define CR0_NE_ (1UL << 5)
#define CR0_PG_ (1UL << 31)
#define CR4_PAE_ (1UL << 5)
#define EFER_LME_ (1UL << 8)
#define EFER_LMA_ (1UL << 10)

/* ---- VMCS field encodings (Intel SDM appendix B) ---- */
enum {
	/* 16-bit guest */
	GUEST_ES_SEL = 0x0800, GUEST_CS_SEL = 0x0802, GUEST_SS_SEL = 0x0804,
	GUEST_DS_SEL = 0x0806, GUEST_FS_SEL = 0x0808, GUEST_GS_SEL = 0x080a,
	GUEST_LDTR_SEL = 0x080c, GUEST_TR_SEL = 0x080e,
	/* 16-bit host */
	HOST_ES_SEL = 0x0c00, HOST_CS_SEL = 0x0c02, HOST_SS_SEL = 0x0c04,
	HOST_DS_SEL = 0x0c06, HOST_FS_SEL = 0x0c08, HOST_GS_SEL = 0x0c0a,
	HOST_TR_SEL = 0x0c0c,
	/* 64-bit control */
	EPT_POINTER = 0x201a,
	VMCS_LINK_PTR = 0x2800,
	GUEST_IA32_DEBUGCTL = 0x2802,
	GUEST_IA32_EFER = 0x2806,
	HOST_IA32_EFER = 0x2c02,
	GUEST_IA32_PERF_GLOBAL = 0x2808, HOST_IA32_PERF_GLOBAL = 0x2c04,
	VM_EXIT_MSR_LOAD_ADDR = 0x2008, VM_ENTRY_MSR_LOAD_ADDR = 0x200a,
	/* 64-bit read-only */
	GUEST_PHYS_ADDR = 0x2400,
	/* 32-bit control */
	PIN_EXEC_CTL = 0x4000, PROC_EXEC_CTL = 0x4002, EXCEPTION_BITMAP = 0x4004,
	PF_EC_MASK = 0x4006, PF_EC_MATCH = 0x4008, CR3_TARGET_COUNT = 0x400a,
	VM_EXIT_CTL = 0x400c, VM_EXIT_MSR_STORE_CNT = 0x400e,
	VM_EXIT_MSR_LOAD_CNT = 0x4010, VM_ENTRY_CTL = 0x4012,
	VM_ENTRY_MSR_LOAD_CNT = 0x4014, VM_ENTRY_INTR_INFO = 0x4016,
	VM_ENTRY_EXC_EC = 0x4018, VM_ENTRY_INSN_LEN = 0x401a,
	SEC_EXEC_CTL = 0x401e,
	/* 32-bit read-only */
	VM_INSTRUCTION_ERROR = 0x4400, VM_EXIT_REASON = 0x4402,
	VM_EXIT_INTR_INFO = 0x4404, VM_EXIT_INSN_LEN = 0x440c, VMX_INSN_INFO = 0x440e,
	IDT_VECTORING_INFO = 0x4408, IDT_VECTORING_EC = 0x440a,
	/* 32-bit guest */
	GUEST_ES_LIM = 0x4800, GUEST_CS_LIM = 0x4802, GUEST_SS_LIM = 0x4804,
	GUEST_DS_LIM = 0x4806, GUEST_FS_LIM = 0x4808, GUEST_GS_LIM = 0x480a,
	GUEST_LDTR_LIM = 0x480c, GUEST_TR_LIM = 0x480e,
	GUEST_GDTR_LIM = 0x4810, GUEST_IDTR_LIM = 0x4812,
	GUEST_ES_AR = 0x4814, GUEST_CS_AR = 0x4816, GUEST_SS_AR = 0x4818,
	GUEST_DS_AR = 0x481a, GUEST_FS_AR = 0x481c, GUEST_GS_AR = 0x481e,
	GUEST_LDTR_AR = 0x4820, GUEST_TR_AR = 0x4822,
	GUEST_INTERRUPTIBILITY = 0x4824, GUEST_ACTIVITY_STATE = 0x4826,
	VMX_PREEMPT_TIMER = 0x482e,
	GUEST_SYSENTER_CS_F = 0x482a,
	/* 32-bit host */
	HOST_SYSENTER_CS_F = 0x4c00,
	/* natural-width control */
	CR0_GH_MASK = 0x6000, CR4_GH_MASK = 0x6002,
	CR0_READ_SHADOW = 0x6004, CR4_READ_SHADOW = 0x6006,
	/* natural-width read-only */
	EXIT_QUALIFICATION = 0x6400, GUEST_LINEAR_ADDR = 0x640a,
	/* natural-width guest */
	GUEST_CR0 = 0x6800, GUEST_CR3 = 0x6802, GUEST_CR4 = 0x6804,
	GUEST_ES_BASE = 0x6806, GUEST_CS_BASE = 0x6808, GUEST_SS_BASE = 0x680a,
	GUEST_DS_BASE = 0x680c, GUEST_FS_BASE_F = 0x680e, GUEST_GS_BASE_F = 0x6810,
	GUEST_LDTR_BASE = 0x6812, GUEST_TR_BASE = 0x6814,
	GUEST_GDTR_BASE = 0x6816, GUEST_IDTR_BASE = 0x6818,
	GUEST_DR7 = 0x681a, GUEST_RSP = 0x681c, GUEST_RIP = 0x681e,
	GUEST_RFLAGS = 0x6820, GUEST_PENDING_DBG = 0x6822,
	GUEST_SYSENTER_ESP_F = 0x6824, GUEST_SYSENTER_EIP_F = 0x6826,
	/* natural-width host */
	HOST_CR0 = 0x6c00, HOST_CR3 = 0x6c02, HOST_CR4 = 0x6c04,
	HOST_FS_BASE_F = 0x6c06, HOST_GS_BASE_F = 0x6c08, HOST_TR_BASE_F = 0x6c0a,
	HOST_GDTR_BASE = 0x6c0c, HOST_IDTR_BASE = 0x6c0e,
	HOST_SYSENTER_ESP_F = 0x6c10, HOST_SYSENTER_EIP_F = 0x6c12,
	HOST_RSP_F = 0x6c14, HOST_RIP_F = 0x6c16,
};

#define EXIT_REASON_EXC_NMI      0
#define EXIT_REASON_EXTERNAL_INT 1
#define EXIT_REASON_TRIPLE_FAULT 2
#define EXIT_REASON_INTERRUPT_WINDOW 7 /* guest became interruptible (M15) */
#define EXIT_REASON_CPUID        10
#define EXIT_REASON_HLT          12
#define EXIT_REASON_RDTSC        16
#define EXIT_REASON_VMCALL       18
#define EXIT_REASON_PAUSE        40   /* PAUSE with PAUSE-exiting set */
#define EXIT_REASON_MONITOR_TRAP 37   /* Monitor Trap Flag: one instruction retired */
#define EXIT_REASON_IO_INSTRUCTION 30 /* IN/OUT with I/O-exiting set */
#define EXIT_REASON_RDMSR        31
#define EXIT_REASON_WRMSR        32
#define EXIT_REASON_EPT_VIOLATION 48
#define EXIT_REASON_EPT_MISCONFIG 49
#define EXIT_REASON_RDTSCP       51
#define EXIT_REASON_PREEMPT_TIMER 52
#define EXIT_REASON_RDRAND       57
#define EXIT_REASON_RDSEED       61

/*
 * THROTTLE cadence (milestone 16 — deterministic stop point). HVX_RUN returns to
 * userspace when the in-kernel run-loop hits this bound. To make that boundary a
 * DETERMINISTIC function of the guest (so a whole-boot harness that stops "when
 * the console first shows marker X" resumes at a bit-identical guest state every
 * time), we count ONLY retired guest exits (RDTSC/IO/CPUID/MSR/EPT/HLT — the
 * deterministic ones) toward this primary cadence. External-interrupt and
 * interrupt-window exits arrive at nondeterministic host-driven times; counting
 * them here would jitter the boundary and diverge two otherwise-identical boots.
 */
#define HVX_MAX_INKERNEL_EXITS   (1U << 16)
/*
 * Separate, larger safety cap on TOTAL in-kernel iterations (retired + external
 * interrupts + interrupt-window). It only exists to bound the IRQs-off window
 * against a pathological interrupt storm that retires no guest instructions;
 * physical host interrupts are already serviced INLINE (the sti;nop;cli window on
 * each external-int exit), so a large value is safe. During a normal boot the
 * retired cadence above always fires first, so this never perturbs the boundary.
 */
#define HVX_SAFETY_CAP           (1U << 20)
/*
 * Host-safety bound on a guest that retires NO exits (a tight loop with no
 * RDTSC/IO/HLT): the only exits are then the host's own timer ticks, and the
 * safety cap above would hold this CPU, preemption off, for ~17 minutes at
 * HZ=1000 (observed on bare metal: soft lockups + an RCU stall that froze
 * logins). After this many CONSECUTIVE non-retired exits the run-loop leaves
 * VMX-root so the ioctl handler can cond_resched() and re-enter. The retired
 * cadence (`guard`) and step count are carried across the re-entry, so this is
 * invisible to userspace and does not move any THROTTLE boundary.
 */
#define HVX_PREEMPT_STREAK       2000U
/* Host seconds without any guest progress (no retired exit, no retired branch, no
 * REP counter movement) before the ioctl gives up and reports HVX_EXIT_STALL to
 * userspace: the guest is wedged. */
#define HVX_STALL_SECS           10U
/* internal-only run-loop result: never returned to userspace */
#define HVX_EXIT_PREEMPT         0xfffffffcU

/*
 * Deterministic idle heartbeat (milestone 27). A guest can HALT fully tickless with NO
 * LAPIC timer armed (tsc_deadline==0) and NO pending device completion -- e.g. a networked
 * service (redis) blocked in epoll_wait for loopback data, which needs a softirq/tick to
 * be delivered. On real hardware the periodic timer tick would wake it; deterministically
 * nothing will, so it would deadlock. When that happens the run-loop advances the virtual
 * clock by this fixed amount and injects the guest's LAPIC-timer vector -- a synthetic
 * periodic tick that runs Linux's timer ISR + softirqs (delivering the loopback frame) +
 * the scheduler. It is a PURE FUNCTION of the deterministic instruction stream (it fires at
 * a deterministic idle instruction and advances the clock by a constant), and it is INERT
 * for every guest that never idles unarmed before its stop marker (all pre-M27 workloads
 * block on ARMED disk/timer events or reach their marker first), so no prior fingerprint
 * moves. The value is ~one scheduler tick of virtual time at the boot TSC step.
 */
#define HVX_IDLE_HEARTBEAT_TSC   (1ULL << 20)

/*
 * Guest-physical layout the module owns. Guest paging identity-maps the first
 * 2 MiB (one large page) so guest-virtual == guest-physical; EPT maps only the
 * pages actually allocated. Userspace code is expected at GPA_CODE and above.
 *
 * The GDT/IDT/TSS and the interrupt handler live in the FREE space of the three
 * page-table pages: the page-walk hardware only reads entry [0] of each, so the
 * rest of those pages is ours. This keeps GPA_CODE fixed at 0x3000 (the ABI)
 * while giving the guest an environment that can actually take an interrupt.
 */
#define GPA_PML4   0x0000UL
#define GPA_GDT    0x0800UL		/* GDT (null, code64, data, TSS)  [page 0] */
#define GPA_TSS    0x0a00UL		/* 64-bit TSS (zeroed; IST unused) [page 0] */
#define GPA_PDPT   0x1000UL
#define GPA_IDT    0x1800UL		/* IDT, vectors 0..0x20            [page 1] */
#define GPA_PD     0x2000UL
#define GPA_ISR    0x2800UL		/* interrupt handler blob          [page 2] */
#define GPA_INTR_COUNT 0x2f00UL		/* handler increments this u64     [page 2] */
#define GPA_CODE   HVX_GUEST_CODE_BASE	/* 0x3000 */
#define MIN_GPAGES 8
/*
 * Guest-RAM ceiling (milestone 28, Rung 3). Raised 256 MiB -> 3 GiB. The EPT PD
 * level FANS OUT (ept_pd[], one PD page per GiB) to address this; the single-PD
 * structure only reached 1 GiB. The cap MUST stay strictly below the 4th-GiB MMIO
 * hole HVX_MMIO_HOLE_BASE (virtio BAR 0xfe000000, IOAPIC 0xfec00000, LAPIC
 * 0xfee00000 are DELIBERATELY EPT-unmapped so accesses trap and are decoded in
 * kernel). A flat guest-RAM GPA space reaching 0xfe000000 would back those MMIO
 * GPAs with real RAM and SILENTLY break every MMIO trap -- no fault, just wrong
 * bytes. 3 GiB (0xc0000000) sits below the hole; going higher needs the PCI-hole
 * split (RAM below the hole + RAM above 4 GiB + a split e820), a deferred follow-on.
 */
#define HVX_MMIO_HOLE_BASE 0xfe000000UL	/* 4th-GiB MMIO window base (must stay unbacked) */
/*
 * M179: guest RAM up to 8 GiB, split around the hole. RAM page index i (guest_pages[i], the
 * mmap offset, the dirty log's bit) is guest-physical page i while i < n_low_pages (the low
 * block, at most 3 GiB: every guest of 3 GiB or less is exactly as before), and guest-physical
 * page HVX_RAM_HIGH_GFN + (i - n_low_pages) above that (the high block, from 4 GiB). The low
 * block is full (3 GiB, a whole number of 2 MiB EPT PTs) whenever a high block exists, so EPT PT
 * k (RAM block k) maps either GPA block k or GPA block (k - 1536) + 2048 (hvx_ept_gblock).
 */
#define HVX_RAM_LOW_PAGES 786432U	/* 3 GiB / 4 KiB: the low block's most (below the MMIO hole) */
#define HVX_RAM_HIGH_GFN 0x100000UL	/* 4 GiB / 4 KiB: the high block's first guest-physical page */
#define MAX_GPAGES 2097152U		/* 8 GiB / 4 KiB (M179; was the low block's 3 GiB) */
#define EPT_PT_LEAVES 512		/* 4 KiB EPT leaves per PT page (also PD entries per PD page) */

#define GDT_LIMIT  0x27			/* null + code + data + 16-byte TSS */
#define IDT_LIMIT  0x20f		/* 33 gates (0..0x20) * 16 - 1 */
#define IDT_MAX_GATE 0x20		/* module IDT builds gates 0..0x20 (M8 tests) */
/* Highest vector HVX_INJECT will accept. Raised from 0x20 to 0xff at M15: a
 * booting Linux installs its own 256-vector IDT, so its LAPIC-timer vector
 * (~0xec) is valid even though the module's built-in IDT only spans 0..0x20. */
#define INTR_VECTOR_MAX 0xff

/* Guest UART receive ring size (milestone 18). Power of two: the head/tail are
 * free-running counters indexed with & (HVX_INPUT_SZ - 1). */
#define HVX_INPUT_SZ 4096

/* Synchronous fuzz-input port (Phase 3 search loop). The guest pulls a host-staged op-program
 * byte-by-byte via IN from the DATA port, with a data-available STATUS port. Distinct from the
 * emulated 16550 UART (0x3f8-0x3ff), the PIC (0x20/0x21/0xa0/0xa1), and the PCI config latch
 * (0xcf8/0xcfc). CAVEAT: 0x510/0x511 fall INSIDE the M20 virtio-blk Phase-A test doorbell range
 * (0x500-0x52f; 0x510 == HVX_VPIO_USED_LO), which is only active when a disk is attached -- so
 * hvx_pio routes an IN of these two ports to the fuzz dispatch BEFORE the virtio doorbell (which
 * only writes those offsets), or a disk-attached fuzz reader would spin forever. See hvx_pio. */
#define HVX_FUZZ_SZ         8192		/* max staged program bytes (op-programs are tiny) */
#define HVX_FUZZ_DATA_PORT  0x510	/* IN: next program byte, advancing the offset (0 when drained) */
#define HVX_FUZZ_STATUS_PORT 0x511	/* IN: 1 while bytes remain to read, else 0 */

/*
 * Per-fd VM. Everything here persists across ioctls; nothing holds live VMX
 * state between calls (VMXON is entered and exited within a single HVX_RUN).
 *
 * Guest RAM is a set of individually-allocated order-0 pages, not one big
 * high-order block: EPT maps guest-physical page by page anyway, and per-page
 * allocation keeps each page's refcount correct when we hand it to userspace
 * through the mmap fault handler (a high-order block's tail pages are not
 * individually refcounted, so mixing get_page() with a whole-block free
 * corrupts page state).
 */
/* x2APIC Local APIC register model (milestone 15). The whole register file is
 * MSR-mapped in x2APIC mode, so it is emulated through the RDMSR/WRMSR trap; no
 * MMIO, no shadow page. `reg[]` is the read-as-written backing store for MSRs
 * 0x800..0x83f; ID/VERSION/PPR/LDR and ISR/TMR/IRR are computed read-only. */
struct hvx_lapic {
	u64 apic_base;		/* IA32_APIC_BASE (0x1b): base | BSP | EXTD | EN */
	u64 tsc_deadline;	/* IA32_TSC_DEADLINE (0x6e0): 0 = disarmed */
	u32 reg[0x40];		/* x2APIC MSRs 0x800..0x83f */
	u32 in_service;		/* vector currently injected & awaiting EOI (0 = none) */
};

/* Minimal IOAPIC (milestone 15): one index/data register pair at 0xfec00000.
 * Redirection entries are read-as-written and stay masked, so no IOAPIC IRQ is
 * ever delivered -- the boot reaches userspace on the LAPIC timer alone. */
struct hvx_ioapic {
	u32 ioregsel;
	u32 id;
	u32 redir[48];		/* 24 entries x 2 dwords (regs 0x10..0x3f) */
};

/* Minimal 16550 UART (milestone 15). M10 captured only TX bytes; a userspace
 * console needs the register model: DLAB tracking (so divisor-latch writes are
 * not captured as console bytes) and a "carrier present" modem-status register
 * (MSR = DCD|DSR|CTS). Without DCD the tty layer hangs up the console and the
 * shell's stdin EOFs -> init exits. Mirrors src/uart.rs. */
struct hvx_uart {
	u8 ier, lcr, mcr, fcr, scr;	/* interrupt-enable, line-ctl, modem-ctl, fifo-ctl, scratch */
	u8 dll, dlm;			/* divisor latch (ignored; DLAB=LCR bit 7) */
};

/*
 * Deterministic virtio-PCI block device (milestone 20). The virtio-blk CORE is
 * transport-agnostic: one split virtqueue processed against an in-memory backing
 * image, with the completion interrupt delivered at a DETERMINISTIC instruction
 * (kick_icount + HVX_VBLK_LATENCY) through the M8 injection path. Two transports
 * feed the SAME core: a Phase-A test PIO doorbell (irq_mode 0, direct vector) and
 * a Phase-B PCI/MMIO transport (irq_mode 1, INTx via the IOAPIC). The device is
 * INERT until a disk is attached (disk == NULL), so a VM that never touches it is
 * byte-identical -- the M16 boot fingerprint is preserved. Shared per-VM (like the
 * console / IOAPIC / UART); its control state is folded into the snapshot, the
 * disk image rides via HVX_GET/SET_DISK.
 */
/* flush-aware DROP volatile-write-cache bounds (milestone 26). A write rolled for
 * HVX_FAULT_DROP_UNFLUSHED is held (not persisted) until a FLUSH commits it; cap the
 * tracked writes and the per-write bytes (a write bigger than a slot, or one that
 * overflows the table, just persists normally -- a safe, honest fallback). */
/* HVX_UNSYNC_MAX / HVX_UNSYNC_ENTBYTES: hvx_abi.h (HVX_GET_UNSYNC / HVX_SET_UNSYNC) */
struct hvx_vblk {
	/* Backing image as a PAGE ARRAY (milestone 28, mirroring guest RAM -- see M11).
	 * Individually-allocated order-0 pages so a GB-scale image is a robust per-page
	 * allocation (not one fragile multi-GB contiguous block) and each page's refcount
	 * is correct when the disk-mmap fault handler hands it to userspace. disk_pages ==
	 * NULL => device absent. The last page is partial; disk_size is the exact length. */
	unsigned long *disk_pages;	/* host-virt of each disk page [n_disk_pages]; NULL => absent */
	unsigned int   n_disk_pages;	/* pages backing the image (ceil(disk_size/PAGE)) */
	u64   disk_size;		/* image size in bytes */

	/* one split virtqueue (queue 0) */
	u64 desc_gpa, avail_gpa, used_gpa;
	u32 qsize;			/* queue size (entries) */
	u32 qready;			/* queue live */
	u32 last_avail;			/* device's cursor into the avail ring */

	/* deterministic completion + interrupt */
	u32 isr;			/* virtio ISR status register (bit0 = queue), read-to-clear */
	u32 irq_vec;			/* Phase-A direct completion vector */
	u32 irq_mode;			/* 0 = direct injection / 1 = IOAPIC INTx (GSI) */
	u32 irq_pending;		/* completion asserted (level), awaiting guest ack */
	u32 completion_armed;		/* scheduled; waiting out the instruction latency */
	u64 completion_due;		/* icount at which the completion becomes due */

	/* modern virtio negotiation / status (Phase B common cfg) */
	u32 status;			/* device_status */
	u32 qsel;			/* selected queue */
	u32 dfeat_sel, gfeat_sel;	/* device/driver feature-select */
	u64 gfeatures;			/* driver-accepted feature bits */

	/* PCI config (Phase B) */
	u32 pci_command;
	u32 pci_bar0;
	u32 pci_cfg_addr;		/* latched 0xcf8 address */
	u32 pci_intline;		/* interrupt line (GSI) */

	/* MSI-X (milestone 21). The virtio-blk function advertises an MSI-X
	 * capability whose table + PBA sit in BAR0; a completion is delivered as a
	 * direct LAPIC injection of the queue's message-data vector (edge), NOT
	 * through the IOAPIC/INTx -- this is what real Linux's modern virtio_pci
	 * uses, and it avoids the acpi=off INTx-routing wall. */
	u32 msix_control;		/* MSI-X cap message control: enable(15)/func-mask(14) writable; table-size RO */
	u32 config_msix_vector;		/* common-cfg msix_config (config-change vector); NO_VECTOR until set */
	u32 queue_msix_vector;		/* common-cfg queue_msix_vector for queue 0; NO_VECTOR until set */
	u32 msix_tbl[HVX_MSIX_ENTRIES * 4];	/* {addr_lo, addr_hi, msg_data, vector_ctrl} per entry */

	/* deterministic fault injection (milestone 22). A SEEDED policy perturbs I/O
	 * (delay / error / drop / reorder / torn); every decision is a pure function
	 * of (fault_seed, fault_req_seq) [per-request] and (fault_seed, fault_kick_seq)
	 * [per-kick reorder], never host time, so the fault schedule replays bit-for-
	 * bit and rides in the snapshot. INERT when fault_classes == 0 (the default),
	 * so the device is byte-identical to M21. */
	u64 fault_seed;
	u32 fault_classes;		/* enabled HVX_FAULT_* bits; 0 => inert */
	u32 fault_probability;		/* 0..100 : percent of requests faulted */
	u32 fault_max_delay;		/* DELAY: max extra completion latency (instructions) */
	u32 fault_kick_seq;		/* per-kick counter (reorder permutation key) */
	u64 fault_req_seq;		/* per-request counter (fault-roll key) */
	u64 fault_count;		/* faults injected so far (diagnostic + snapshot) */

	/* flush-aware DROP (milestone 26, HVX_FAULT_DROP_UNFLUSHED). A VOLATILE WRITE
	 * CACHE: a WRITE rolled for that class is stashed here (NOT persisted); reads
	 * are served from it (read-your-writes); a virtio-blk FLUSH commits it to the
	 * backing image (durable) and clears it. Un-flushed entries at end-of-run are
	 * lost -- the faithful power-loss model. Not in struct hvx_snapshot (too big): a
	 * snapshot taken while writes are held moves it with HVX_GET/SET_UNSYNC (M44).
	 * Cleared on SET_DISK / SET_FAULT / device reset. unsync_data is lazily kvmalloc'd
	 * only when the class is first used, so a VM that never uses it is byte-identical
	 * to M25. Same layout as struct hvx_unsync_rec. */
	struct hvx_unsync_ent { u64 doff; u32 len; u32 valid; } unsync[HVX_UNSYNC_MAX];
	u8 *unsync_data;		/* HVX_UNSYNC_MAX * HVX_UNSYNC_ENTBYTES, lazily allocated */
	u32 unsync_n;			/* live entries */
	/* power cut at the k-th FLUSH (HVX_SET_FLUSHCUT); inert while fc_on == 0 */
	u32 fc_on, fc_off;
	u64 fc_target, fc_count, fc_lo, fc_hi, fc_hung;
	/* dirty-page log of the disk (HVX_VM_DIRTY_LOG, M138): the pages the device wrote;
	 * NULL without the flag. Covers dl_pages pages (n_disk_pages when allocated). */
	unsigned long *dl;
	unsigned int dl_pages;
};

/*
 * Deterministic virtio-net PCI device (milestone 23). A SECOND virtio-PCI function
 * (vendor 0x1af4, modern virtio-net device 0x1041) alongside the virtio-blk one,
 * sharing the PCI-config + MMIO-BAR + MSI-X foundation. Two virtqueues: receiveq
 * (0) and transmitq (1).
 *
 *   INBOUND (RX): scripted frames pushed from userspace (HVX_PUT_RX) sit in a small
 *   FIFO; at the next receiveq KICK -- a guest-driven, icount-exact instant -- the
 *   device consumes a posted receive buffer, writes virtio_net_hdr_v1 + the frame +
 *   the used-ring entry, and schedules the completion MSI-X at kick_icount +
 *   HVX_VNET_LATENCY (the M8 instruction-timed path, M16 creep discipline). The
 *   frame content is deterministic scripted input, so the whole RX path is a pure
 *   function of the instruction clock and reproduces on branch/replay.
 *
 *   OUTBOUND (TX): frames the guest posts to the transmitq are serviced
 *   synchronously at the transmitq KICK and captured (payload after the 12-byte
 *   header) into a host-side buffer, readable via HVX_GET_TX (like the console).
 *
 * INERT until attached (present == 0): PCI config returns "no device", the BAR/PIO
 * windows return false, no completion ever arms -- so a VM that never attaches a
 * net device is byte-identical and every prior fingerprint is preserved. Shared
 * per-VM (like the console / IOAPIC / UART / virtio-blk).
 */
#define HVX_VNET_RX_SLOTS	8	/* pending inbound-frame FIFO depth (power of two) */
#define HVX_VNET_TX_CAP		8192	/* captured-outbound-frame buffer bytes */
#define HVX_VNET_RX_PACE	200000	/* paced RX: instructions between trickled frames */

struct hvx_vnet_queue {
	u64 desc_gpa, avail_gpa, used_gpa;
	u32 qsize;			/* queue size (entries) */
	u32 qready;			/* queue live */
	u32 last_avail;			/* device cursor into the avail ring */
	u32 msix_vector;		/* queue_msix_vector (table index; NO_VECTOR until set) */
	u32 completion_armed;		/* scheduled; waiting out the instruction latency */
	u64 completion_due;		/* icount at which the completion becomes due */
	u32 irq_pending;		/* MSI edge pending for this queue, awaiting delivery */
};

struct hvx_vnet {
	u32 present;			/* attached (HVX_NET_ATTACH); 0 => device absent */
	u8  mac[6];			/* advertised MAC (VIRTIO_NET_F_MAC) */

	struct hvx_vnet_queue q[HVX_VNET_NQUEUES];	/* [0]=receiveq, [1]=transmitq */

	/* modern virtio negotiation / status (common cfg) */
	u32 status;			/* device_status handshake */
	u32 qsel;			/* selected queue */
	u32 dfeat_sel, gfeat_sel;	/* device/driver feature-select */
	u64 gfeatures;			/* driver-accepted feature bits */

	/* PCI config */
	u32 pci_command, pci_bar0, pci_intline;

	/* virtio ISR status (bit0 = queue), read-to-clear */
	u32 isr;

	/* MSI-X (like virtio-blk): a completion is delivered as a direct LAPIC
	 * injection of the queue's message-data vector (edge), NOT via INTx. */
	u32 msix_control;		/* MSI-X cap message control: enable(15)/func-mask(14) */
	u32 config_msix_vector;		/* common-cfg msix_config (config-change vector) */
	u32 msix_tbl[HVX_MSIX_ENTRIES * 4];	/* {addr_lo, addr_hi, msg_data, vector_ctrl} per entry */

	/* pending inbound (RX) FIFO -- deterministic scripted frames (HVX_PUT_RX).
	 * Free-running head/tail (count = tail - head); drained only at a receiveq
	 * kick. The frame content is deterministic input; the cursors ride in the
	 * snapshot. NOT cleared by a device reset (inbound network data is
	 * independent of the driver reset). */
	u8  rx_buf[HVX_VNET_RX_SLOTS][HVX_VNET_FRAME_MAX];
	u32 rx_flen[HVX_VNET_RX_SLOTS];
	u32 rx_head, rx_tail;
	/* PACED delivery (opt-in via HVX_NET_PACED, for a real OS). A real driver posts
	 * a deep receive ring and kicks the receiveq ONCE at open, so kick-triggered
	 * delivery would place every scripted frame in that first NAPI burst -- before
	 * the interface finishes coming up (NETDEV_UP / local address active), so an
	 * ARP would be received but not answered. When paced, the run-loop instead
	 * trickles one pending frame per HVX_VNET_RX_PACE instructions (against the
	 * already-posted buffers), a deterministic icount cadence, so a later frame
	 * arrives after the interface is fully up and gets answered. OFF by default, so
	 * the hand-written test (kick-triggered, single frame) is byte-identical. */
	u32 rx_paced;
	u64 rx_next_deliver;		/* icount at which the next paced frame may deliver */

	/* captured outbound (TX) frames -- payload bytes, concatenated (HVX_GET_TX).
	 * Rewound on a device reset so a replay re-captures identically. */
	u8  tx_buf[HVX_VNET_TX_CAP];
	u32 tx_len;			/* bytes captured (write offset) */
	u32 tx_count;			/* frames captured */

	/* deterministic fault injection (milestone 24). A SEEDED policy perturbs packets
	 * (drop / delay / reorder / corrupt / duplicate) on RX and TX; every decision is a
	 * pure function of (nfault_seed, direction, packet index) [per-packet] and
	 * (nfault_seed, direction, kick index) [per-kick reorder], never host time, so the
	 * schedule replays bit-for-bit and rides in the snapshot. INERT when
	 * nfault_classes == 0 (the default), so the device is byte-identical to M23. */
	u64 nfault_seed;
	u32 nfault_classes;		/* enabled HVX_NFAULT_* bits; 0 => inert */
	u32 nfault_probability;		/* 0..100 : percent of packets faulted */
	u32 nfault_max_delay;		/* DELAY: max extra RX completion latency (instructions) */
	u32 nfault_rx_kick_seq;		/* per-RX-kick reorder permutation key */
	u32 nfault_tx_kick_seq;		/* per-TX-kick reorder permutation key */
	u64 nfault_rx_seq;		/* per-RX-packet fault-roll key */
	u64 nfault_tx_seq;		/* per-TX-packet fault-roll key */
	u64 nfault_count;		/* faults injected so far (diagnostic + snapshot) */
};

#define HVX_MAX_VCPUS 8		/* deterministic SMP: vCPUs serialized onto one CPU (M19) */

struct hvx_vm;			/* forward: hvx_vcpu carries a back-pointer to its VM */

/* struct hvx_vcpu.halted (and struct hvx_sched_vcpu.halted) */
#define HVX_HALT_RUN	0	/* runnable */
#define HVX_HALT_HLT	1	/* at a HLT, until an interrupt can be delivered */
#define HVX_HALT_SIPI	2	/* after INIT / at power-on of an AP: until a STARTUP IPI */

/*
 * Per-vCPU state (milestone 19). Split out of struct hvx_vm so one VM can hold
 * N >= 1 vCPUs. Everything here is PRIVATE to a single vCPU: its VMCS/VMXON
 * pages, register file, privileged (sregs) state, deterministic virtual TSC +
 * instruction clock, curated software MSR store, pending instruction-timed
 * injection, and its own Local APIC. SHARED VM state (guest RAM/EPT, console,
 * serial input, IOAPIC/PIC/16550 UART) lives in struct hvx_vm, reached via ->vm.
 *
 * The vCPUs are SERIALIZED: only ONE is ever in VMX-root at a time (userspace
 * round-robins them by a fixed instruction quantum via HVX_RUNC + HVX_SET_VCPU),
 * so there is no real parallelism and no concurrent memory interleaving. The
 * interleaving happens only at quantum boundaries and is therefore a
 * deterministic function of the per-vCPU instruction counts.
 */
struct hvx_vcpu {
	struct hvx_vm *vm;			/* owning VM (all shared state) */
	u32 index;				/* vCPU index == x2APIC physical ID */

	unsigned long vmxon_page;
	unsigned long vmcs_page;
	/* guest x87/SSE state (512-byte FXSAVE image in its own page, so it is
	 * 16-byte aligned). Loaded before entry and saved after the last exit of
	 * every run: without it the guest's live XMM/x87 registers were replaced
	 * by host leftovers each time HVX_RUN returned to userspace. */
	unsigned long fx_page;
	/* guest XCR0 (HVX_VM_X86_64_V3 only; 0 otherwise). With it, fx_page holds the
	 * standard-format XSAVE area (x87|SSE|AVX, 832 bytes) instead of an FXSAVE image. */
	u64 xcr0;
	/* retired-exit cadence / step count carried across an internal preempt
	 * re-entry (HVX_EXIT_PREEMPT); zero at the start of every ioctl */
	unsigned int carry_guard;
	u64 carry_stepped;
	/* An event whose delivery through the guest IDT was cut short by a VM exit
	 * (IDT-vectoring information valid): it must be injected again on the next
	 * entry or the guest never sees it -- and, for an interrupt we injected, never
	 * EOIs it, which blocks every later interrupt (in_service stays set). */
	/* Pending one-shot vectors beyond the single inject_* slot (a minimal LAPIC
	 * IRR). The slot alone lost an event whenever a second one arrived before the
	 * first was delivered -- e.g. the guest's self-IPI replacing a timer tick that
	 * was waiting for interrupts to be re-enabled; in periodic mode Linux never
	 * re-arms after a lost tick, so the guest's clock stopped for good. */
	unsigned long irr[4];
	u32 reinj_info, reinj_ec, reinj_len;
	bool reinj_valid;
	u64 reinj_count;			/* statistics */
	/* guest CR2. It is not a VMCS field: it lives in the hardware register, which
	 * the host overwrites on its own page faults. Captured after every exit and
	 * reloaded before every entry, so a guest #PF handler that has not read CR2
	 * yet still sees its own fault address after the host ran in between. */
	unsigned long cr2;
	/* PMU branch clock (HVX_VM_PMU_CLOCK); see the block above setup_vmcs */
	bool pmu_live;			/* armed for the current run */
	bool pmu_step;			/* last entry was an MTF landing step */
	u64 pmu_n;			/* branches until overflow, as programmed for the last entry */
	u64 pmu_base;			/* PMC0 value at the last entry (48 bits) */
	u64 pmu_cur;			/* PMC0 value read at the last exit */
	bool pmu_carry;			/* PMC0 still counts from a landing step: the next step
					 * can keep it instead of re-programming it (FASTTURN.md) */
	bool pmu_guarded;		/* this run counts in the guest only (pmu_guard, M156) */
	u64 pmu_total, pmu_next;	/* guest branches retired / next tick target (in the snapshot) */
	u64 pmu_ticks, pmu_over, pmu_steps, pmu_land;	/* statistics */
	u32 pmu_flat;			/* consecutive landing steps that retired no branch */
	u64 pmu_hops;			/* statistics: branch-free stretches skipped (see HVX_PMU_HOP_STEPS) */
	u64 chaos_nmi_k;		/* exits counted for chaos_nmi */
	u64 tj_applied, tj_hash, tj_last;	/* tick-jitter statistics (HVX_GET_TJITTER) */
	u64 xt_applied, xt_hash, xt_first, xt_last;	/* inserted-tick statistics (HVX_GET_XTICK) */
	/* in-kernel scheduler (HVX_VM_SMP_SCHED); see struct hvx_sched in hvx_abi.h */
	u64 br_own;			/* guest conditional branches this vCPU retired */
	u64 sched_turns;		/* turns this vCPU has run */
	u8 halted;			/* HVX_HALT_*: 1 = stopped at a HLT (RIP still at it) until an
					 * interrupt; 2 = waiting for a STARTUP IPI (HVX_VM_SMP_BOOT) */
	u32 halt_len;

	struct hvx_regs regs;			/* software guest register file */

	/* deterministic virtual TSC (see struct hvx_tsc in hvx_abi.h) */
	u64 tsc;				/* COUNTER: next value / ICOUNT: base */
	u64 tsc_step;				/* COUNTER: per-exit step / ICOUNT: per-insn */
	u64 tsc_aux;				/* RDTSCP's ECX (IA32_TSC_AUX) */
	u64 tsc_mode;				/* HVX_TSC_COUNTER | HVX_TSC_ICOUNT */
	u64 rdtsc_count;			/* RDTSC/RDTSCP exits handled */

	/* instruction clock: retired instructions the monitor observed
	 * (MTF single-steps + emulated RDTSC/RDTSCP); a full instruction count
	 * only while single-stepping */
	u64 icount;

	/* pending instruction-timed interrupt (one-shot; see struct hvx_inject).
	 * A cross-vCPU IPI (LAPIC ICR write) latches this on the TARGET vCPU. */
	u64 inject_at_icount;
	u32 inject_vector;
	u32 inject_valid;

	/* curated software MSR store (milestone 12); index by SWMSR_* enum. These
	 * are per-CPU MSRs (SYSCALL STAR/LSTAR/..., GS base). Folded into snapshot. */
	u64 sw_msr[HVX_NR_SW_MSR];

	/* privileged (system) register state (milestone 13): CR0/CR3/CR4/EFER/DR7,
	 * the segment descriptors, and GDTR/IDTR. setup_vmcs LOADS the guest from
	 * here every entry (seeded to flat long-mode defaults on CREATE) and the
	 * run-loop SAVES it back after each exit, so guest changes to CR3, segments,
	 * or the descriptor tables persist across HVX_RUN boundaries. */
	struct hvx_sregs sregs;

	/* per-CPU Local APIC (x2APIC, MSR-mapped -- milestone 15). Part of the
	 * deterministic state; folded into the snapshot. */
	struct hvx_lapic lapic;

	/*
	 * DIAGNOSTIC (performance instrumentation, NOT deterministic state).
	 * Per-vCPU VM-exit histogram indexed by basic exit reason (bits 15:0),
	 * total exit count, and total wall-nanoseconds spent inside hvx_run_vcpu.
	 * These are kernel-internal counters only: never read into guest state,
	 * never in the snapshot/fingerprint, never used in any deterministic
	 * decision -- so they are fingerprint-neutral. Dumped to dmesg at VM free.
	 */
	u64 exit_hist[64];
	u64 exit_total;
	u64 run_ns;
	/* turn-end cost diagnostics (HVX_GET_FTSTAT; host-side only, fingerprint-neutral) */
	struct hvx_ftstat_vcpu fts;
	bool fts_tgt_turn;		/* the current landing aims at a turn end, not a tick */
};

struct hvx_vm {
	struct mutex lock;
	/* M97: the mmap fault handlers run without `lock` (taking it there would invert
	 * the lock order of an ioctl that faults on user memory while holding it). This
	 * leaf lock guards what they read: ram_ready + guest_pages[], and the disk page
	 * array, which HVX_SET_DISK replaces. */
	spinlock_t map_lock;
	bool ram_ready;

	/* vCPUs (milestone 19). n_vcpus >= 1; cur_vcpu is selected by HVX_SET_VCPU
	 * and is the target of every per-vCPU ioctl (default 0 -> the single-vCPU
	 * path, so all pre-M19 clients see exactly the old behavior). */
	unsigned int n_vcpus;
	unsigned int cur_vcpu;
	struct hvx_vcpu *vcpu;			/* [n_vcpus] */

	/* Deterministic idle heartbeat (milestone 27, HVX_VM_IDLE_HEARTBEAT). Opt-in;
	 * 0 by default so the plain no-disk boot (which also idles unarmed) is byte-
	 * identical to pre-M27. Set from cfg->flags at HVX_CREATE_VM. */
	unsigned int idle_hb;

	/* x86-64-v2 CPUID level (milestone 31, HVX_VM_X86_64_V2). Opt-in; 0 by default
	 * so hvx_cpuid reports the exact M12 feature set and every prior boot fingerprint
	 * is byte-identical. When set, hvx_cpuid ORs in MMX + the v2 SSE/POPCNT/CX16/LAHF
	 * bits so a real glibc userland (a distro OpenJDK JRE) loads. Set at HVX_CREATE_VM. */
	unsigned int cpu_v2;
	unsigned int cpu_v3;		/* HVX_VM_X86_64_V3: guest AVX/XSAVE (implies cpu_v2) */
	unsigned int pmu_clock;		/* HVX_VM_PMU_CLOCK */
	/* In-kernel vCPU scheduler (HVX_VM_SMP_SCHED with n_vcpus > 1). `sched` holds the
	 * policy, the scheduler state and the VM-wide clock (its v[] array is unused here:
	 * the per-vCPU parts live in struct hvx_vcpu). sched_stop is a THROTTLE / max_insns
	 * stop waiting for a clean boundary (always 0 between ioctls). */
	unsigned int sched_on;
	/* HVX_VM_SMP_BOOT (with sched_on): INIT/SIPI start-up of vCPUs 1..n-1, unrestricted
	 * guest (real mode), CPUID topology, device interrupts routed by destination. */
	unsigned int smp_boot;
	struct hvx_sched sched;
	u32 sched_stop;
	unsigned int pause_exit;	/* HVX_VM_PAUSE_EXIT */
	/* Intel PT guest trace (HVX_VM_PT_TRACE / HVX_SET_PT / pt_force). Host-owned ToPA
	 * output; enabled only around VM entry (hvx_vmx_run_pt) and filtered so no host
	 * code is traced. Diagnostic only: not in the snapshot, invisible to the guest. */
	bool pt_on;
	bool pt_allowed;		/* created with HVX_VM_PT_TRACE */
	u64 pt_ctl;			/* RTIT_CTL while the guest runs (TraceEn set) */
	u64 pt_lo[2], pt_hi[2], pt_cr3;
	struct page **pt_chunk;		/* 2 MiB output regions */
	unsigned int pt_nchunk;
	struct page *pt_topa;		/* the ToPA table */
	u64 pt_mask, pt_status;		/* RTIT_OUTPUT_MASK_PTRS / RTIT_STATUS between runs */
	u64 pmu_period, pmu_tick_tsc;
	/* turn control (HVX_SET_TURNCTL, hv/hvx/FASTTURN.md): the turn-end landing margin and
	 * the seeded burst policy. Policy, not state: not in any snapshot. */
	u64 turn_margin;
	struct hvx_turnctl tc;
	/* seeded tick-timing jitter (HVX_SET_TJITTER); off when tj_on == 0. Not in the
	 * snapshot (see hvx_abi.h): a restored run takes whatever policy is set. */
	bool tj_on;
	struct hvx_tjitter tj;
	/* inserted PMU-clock ticks (HVX_SET_XTICK); off when xt_on == 0. Not in the snapshot. */
	bool xt_on;
	struct hvx_xtick xt;

	/* SHARED guest RAM + EPT (all vCPUs share one physical address space) */
	unsigned int  n_guest_pages;		/* size of guest RAM in pages */
	unsigned int  n_low_pages;		/* of them below the hole, from GPA 0 (M179; the rest from 4 GiB) */
	unsigned long *guest_pages;		/* host-virt of each guest page [n], by RAM page index */
	unsigned long ept_pg[2];		/* EPT PML4, PDPT (fixed top two levels) */
	unsigned long *ept_pd;			/* EPT PD pages [n_ept_pd] (M28 fan-out; 512 PT entries each = 1 GiB) */
	unsigned int  n_ept_pd;
	unsigned long *ept_pt;			/* EPT PT pages [n_ept_pt] (4 KiB leaves) */
	unsigned int  n_ept_pt;
	u64 eptp;

	/* SHARED serial console: bytes the guest wrote to the UART transmit register */
	u8  console[HVX_CONSOLE_SZ];
	u32 console_len;

	/* SHARED IOAPIC / 8259 device model (milestone 15). The 16550 UART is one
	 * shared device (COM1). Part of the deterministic state; folded into snapshot. */
	struct hvx_ioapic ioapic;
	struct hvx_uart   uart;	/* 16550 register state (DLAB, modem ctl, ...) */
	u32 pic_mask[2];	/* 8259 master/slave IMR (ports 0x21 / 0xa1) */

	/* SHARED serial input ring (milestone 18): bytes pushed from userspace via
	 * HVX_PUT_INPUT, consumed by a guest IN from the UART RBR (0x3f8). Free-
	 * running head/tail counters (count = tail - head, empty when equal). This
	 * is TRANSIENT host input, deliberately NOT in the snapshot/fingerprint
	 * (like the console buffer): with an empty ring the RX path is fully inert,
	 * so a boot with no input fed stays bit-identical (M16 fingerprint). */
	u8  input_buf[HVX_INPUT_SZ];
	u32 input_head, input_tail;

	/* SYNCHRONOUS fuzz-input port (Phase 3 search loop): a host-staged op-program the guest pulls
	 * byte-by-byte via IN from HVX_FUZZ_DATA_PORT (0x510), with a data-available status at
	 * HVX_FUZZ_STATUS_PORT (0x511). Set per snapshot-restore via HVX_SET_FUZZ_INPUT (which resets
	 * fuzz_off=0). Like the serial input ring above, this is TRANSIENT host input, deliberately NOT
	 * in the snapshot/fingerprint -- with fuzz_len=0 both ports are inert (status 0, data 0), so a
	 * boot with no program staged stays bit-identical (the Phase-2 fingerprint is preserved). */
	u8  fuzz_buf[HVX_FUZZ_SZ];
	u32 fuzz_len, fuzz_off;

	/* SHARED virtio-blk device (milestone 20). Inert until a disk is attached. */
	struct hvx_vblk vblk;

	/* SHARED virtio-net device (milestone 23). Inert until HVX_NET_ATTACH. */
	struct hvx_vnet vnet;

	/* turn-end cost diagnostics (HVX_GET_FTSTAT; fingerprint-neutral) */
	u64 fts_sched_cyc, fts_sched_turns;

	/* host-call port (HVX_VM_HOSTCALL, M138). hc_out holds committed records [0, hc_out_done)
	 * and the record being written [hc_out_done, hc_out_len); hc_in holds staged bytes, read
	 * from hc_in_off. Both buffers exist only with the flag (hc_on), so every other VM runs
	 * exactly the code it ran before (the ports stay unhandled). */
	bool hc_on;
	bool hc_exit;			/* a record was committed: end the run call after this exit */
	bool hc_ovf;			/* the partial record did not fit */
	u8  *hc_out, *hc_in;		/* [HVX_HC_SZ] each */
	u32 hc_out_len, hc_out_done;
	u32 hc_in_len, hc_in_off;
	u64 hc_records, hc_errors;	/* statistics */

	/* dirty-page log (HVX_VM_DIRTY_LOG, M138): RAM pages the module wrote (the guest's own
	 * writes are in the EPT dirty flags) and disk pages the device wrote. NULL without it. */
	bool dl_on;
	unsigned long *dl_ram;		/* [BITS_TO_LONGS(n_guest_pages)]; the disk's is vblk.dl */
};

#define UART_BASE 0x3f8			/* 16550 COM1 base I/O port */

/* virtio-blk GSI (INTx) constants, kept next to the run-loop that uses them */
#define VBLK_IRQ_MODE_DIRECT 0		/* Phase A: inject the vector directly */
#define VBLK_IRQ_MODE_INTX   1		/* Phase B: assert INTx through the IOAPIC */

/* VMX revision id and per-CPU support, established once at module load. */
static u32 hvx_rev;

/*
 * VM-entry trampoline.
 *   long hvx_vmx_run(struct hvx_regs *regs, long resume)  -- regs %rdi, resume %rsi
 * resume==0 => VMLAUNCH, resume!=0 => VMRESUME.
 * Returns 1 if a VM-exit occurred, 0 if VMLAUNCH/VMRESUME itself failed (VMfail).
 * Loads the guest GPRs from *regs before entry and saves them back on exit;
 * rip/rsp/rflags are carried in the VMCS by the C code, not here. Sets HOST_RSP
 * and HOST_RIP so the exit re-enters this function with a valid stack frame.
 *
 * The register-file byte offsets below MUST match struct hvx_regs:
 *   0x00 rax 0x08 rbx 0x10 rcx 0x18 rdx 0x20 rsi 0x28 rdi 0x30 rbp
 *   0x38 r8  0x40 r9  0x48 r10 0x50 r11 0x58 r12 0x60 r13 0x68 r14 0x70 r15
 *
 * Stack while the guest runs (top first): [resume][regs ptr][r15..rbp]. HOST_RSP
 * points at [resume], so the exit re-enters with regs ptr at 8(%rsp).
 */
asm(".text\n"
    ".globl hvx_vmx_run\n"
    "hvx_vmx_run:\n"
    "    push %rbp\n"
    "    push %rbx\n"
    "    push %r12\n"
    "    push %r13\n"
    "    push %r14\n"
    "    push %r15\n"
    "    push %rdi\n"               /* [8(%rsp) at entry] saved regs ptr */
    "    push %rsi\n"               /* [ (%rsp) ] saved resume flag */
    "    mov $0x6c14, %rdx\n"       /* HOST_RSP = &resume */
    "    vmwrite %rsp, %rdx\n"
    "    lea 2f(%rip), %rax\n"
    "    mov $0x6c16, %rdx\n"       /* HOST_RIP = exit label */
    "    vmwrite %rax, %rdx\n"
    /* load guest GPRs from *regs (rdi = regs ptr) */
    "    mov 0x00(%rdi), %rax\n"
    "    mov 0x08(%rdi), %rbx\n"
    "    mov 0x10(%rdi), %rcx\n"
    "    mov 0x18(%rdi), %rdx\n"
    "    mov 0x20(%rdi), %rsi\n"
    "    mov 0x30(%rdi), %rbp\n"
    "    mov 0x38(%rdi), %r8\n"
    "    mov 0x40(%rdi), %r9\n"
    "    mov 0x48(%rdi), %r10\n"
    "    mov 0x50(%rdi), %r11\n"
    "    mov 0x58(%rdi), %r12\n"
    "    mov 0x60(%rdi), %r13\n"
    "    mov 0x68(%rdi), %r14\n"
    "    mov 0x70(%rdi), %r15\n"
    "    mov 0x28(%rdi), %rdi\n"    /* guest rdi last (clobbers regs ptr) */
    "    cmpq $0, (%rsp)\n"         /* resume flag (still on stack) */
    "    jne 5f\n"
    "    vmlaunch\n"
    "    jmp 6f\n"
    "5:  vmresume\n"
    "6:  add $16, %rsp\n"           /* VMfail: drop resume + regs ptr */
    "    xor %eax, %eax\n"
    "    jmp 3f\n"
    "2:\n"                          /* VM-exit re-enters here */
    "    push %rax\n"               /* stack: [g_rax][resume][regs ptr] */
    "    mov 16(%rsp), %rax\n"      /* rax = regs ptr */
    "    mov %rbx, 0x08(%rax)\n"
    "    mov %rcx, 0x10(%rax)\n"
    "    mov %rdx, 0x18(%rax)\n"
    "    mov %rsi, 0x20(%rax)\n"
    "    mov %rdi, 0x28(%rax)\n"
    "    mov %rbp, 0x30(%rax)\n"
    "    mov %r8,  0x38(%rax)\n"
    "    mov %r9,  0x40(%rax)\n"
    "    mov %r10, 0x48(%rax)\n"
    "    mov %r11, 0x50(%rax)\n"
    "    mov %r12, 0x58(%rax)\n"
    "    mov %r13, 0x60(%rax)\n"
    "    mov %r14, 0x68(%rax)\n"
    "    mov %r15, 0x70(%rax)\n"
    "    mov (%rsp), %rbx\n"        /* rbx = saved guest rax */
    "    mov %rbx, 0x00(%rax)\n"
    "    add $24, %rsp\n"           /* drop g_rax, resume, regs ptr */
    "    mov $1, %eax\n"
    "3:  pop %r15\n"
    "    pop %r14\n"
    "    pop %r13\n"
    "    pop %r12\n"
    "    pop %rbx\n"
    "    pop %rbp\n"
    "    ret\n");
extern long hvx_vmx_run(struct hvx_regs *regs, long resume);

/*
 * hvx_vmx_run_pt(regs, resume, rtit_ctl): hvx_vmx_run with Intel PT switched on for
 * exactly the guest's time. This CPU has no VM-entry "load IA32_RTIT_CTL" control, so
 * TraceEn is written here: after the resume/launch decision (no host branch is taken
 * while tracing) and before the guest's rax/rcx/rdx/rdi are loaded; it is cleared as
 * the first thing after the exit. The few host instructions that run with TraceEn set
 * are in this module, outside every IP filter range (and at CPL0, outside a user-only
 * trace), so they produce no packets. Same register-file and stack contract as
 * hvx_vmx_run, with one extra slot: [resume][regs ptr][rtit_ctl][r15..rbp].
 */
asm(".text\n"
    ".globl hvx_vmx_run_pt\n"
    "hvx_vmx_run_pt:\n"
    "    push %rbp\n"
    "    push %rbx\n"
    "    push %r12\n"
    "    push %r13\n"
    "    push %r14\n"
    "    push %r15\n"
    "    push %rdx\n"               /* [16(%rsp)] RTIT_CTL for the guest */
    "    push %rdi\n"               /* [ 8(%rsp)] regs ptr */
    "    push %rsi\n"               /* [  (%rsp)] resume flag */
    "    mov $0x6c14, %rdx\n"       /* HOST_RSP = &resume */
    "    vmwrite %rsp, %rdx\n"
    "    lea 2f(%rip), %rax\n"
    "    mov $0x6c16, %rdx\n"       /* HOST_RIP = exit label */
    "    vmwrite %rax, %rdx\n"
    "    mov 0x08(%rdi), %rbx\n"
    "    mov 0x20(%rdi), %rsi\n"
    "    mov 0x30(%rdi), %rbp\n"
    "    mov 0x38(%rdi), %r8\n"
    "    mov 0x40(%rdi), %r9\n"
    "    mov 0x48(%rdi), %r10\n"
    "    mov 0x50(%rdi), %r11\n"
    "    mov 0x58(%rdi), %r12\n"
    "    mov 0x60(%rdi), %r13\n"
    "    mov 0x68(%rdi), %r14\n"
    "    mov 0x70(%rdi), %r15\n"
    "    cmpq $0, (%rsp)\n"
    "    jne 5f\n"
    "    mov 16(%rsp), %rax\n"
    "    mov %rax, %rdx\n"
    "    shr $32, %rdx\n"
    "    mov $0x570, %ecx\n"
    "    wrmsr\n"                   /* TraceEn = 1 */
    "    mov 0x00(%rdi), %rax\n"
    "    mov 0x10(%rdi), %rcx\n"
    "    mov 0x18(%rdi), %rdx\n"
    "    mov 0x28(%rdi), %rdi\n"
    "    vmlaunch\n"
    "    jmp 6f\n"
    "5:  mov 16(%rsp), %rax\n"
    "    mov %rax, %rdx\n"
    "    shr $32, %rdx\n"
    "    mov $0x570, %ecx\n"
    "    wrmsr\n"                   /* TraceEn = 1 */
    "    mov 0x00(%rdi), %rax\n"
    "    mov 0x10(%rdi), %rcx\n"
    "    mov 0x18(%rdi), %rdx\n"
    "    mov 0x28(%rdi), %rdi\n"
    "    vmresume\n"
    "6:  mov $0x570, %ecx\n"       /* VMfail: tracing off again */
    "    xor %eax, %eax\n"
    "    xor %edx, %edx\n"
    "    wrmsr\n"
    "    add $24, %rsp\n"           /* drop resume, regs ptr, rtit_ctl */
    "    xor %eax, %eax\n"
    "    jmp 3f\n"
    "2:\n"                          /* VM-exit re-enters here */
    "    push %rax\n"
    "    push %rcx\n"
    "    push %rdx\n"
    "    mov $0x570, %ecx\n"
    "    xor %eax, %eax\n"
    "    xor %edx, %edx\n"
    "    wrmsr\n"                   /* TraceEn = 0 */
    "    pop %rdx\n"
    "    pop %rcx\n"
    "    pop %rax\n"
    "    push %rax\n"               /* stack: [g_rax][resume][regs ptr][rtit_ctl] */
    "    mov 16(%rsp), %rax\n"
    "    mov %rbx, 0x08(%rax)\n"
    "    mov %rcx, 0x10(%rax)\n"
    "    mov %rdx, 0x18(%rax)\n"
    "    mov %rsi, 0x20(%rax)\n"
    "    mov %rdi, 0x28(%rax)\n"
    "    mov %rbp, 0x30(%rax)\n"
    "    mov %r8,  0x38(%rax)\n"
    "    mov %r9,  0x40(%rax)\n"
    "    mov %r10, 0x48(%rax)\n"
    "    mov %r11, 0x50(%rax)\n"
    "    mov %r12, 0x58(%rax)\n"
    "    mov %r13, 0x60(%rax)\n"
    "    mov %r14, 0x68(%rax)\n"
    "    mov %r15, 0x70(%rax)\n"
    "    mov (%rsp), %rbx\n"
    "    mov %rbx, 0x00(%rax)\n"
    "    add $32, %rsp\n"           /* drop g_rax, resume, regs ptr, rtit_ctl */
    "    mov $1, %eax\n"
    "3:  pop %r15\n"
    "    pop %r14\n"
    "    pop %r13\n"
    "    pop %r12\n"
    "    pop %rbx\n"
    "    pop %rbp\n"
    "    ret\n");
extern long hvx_vmx_run_pt(struct hvx_regs *regs, long resume, u64 rtit_ctl);

/* ---- VMX instruction wrappers (return 0 on success) ---- */
static inline int vmxon(u64 pa)
{
	u8 err;
	asm volatile("vmxon %1; setna %0" : "=rm"(err) : "m"(pa) : "cc", "memory");
	return err;
}
static inline int vmxoff(void)
{
	u8 err;
	asm volatile("vmxoff; setna %0" : "=rm"(err) : : "cc", "memory");
	return err;
}
static inline int vmclear(u64 pa)
{
	u8 err;
	asm volatile("vmclear %1; setna %0" : "=rm"(err) : "m"(pa) : "cc", "memory");
	return err;
}
static inline int vmptrld(u64 pa)
{
	u8 err;
	asm volatile("vmptrld %1; setna %0" : "=rm"(err) : "m"(pa) : "cc", "memory");
	return err;
}
static inline int __vmwrite(unsigned long field, unsigned long value)
{
	u8 err;
	asm volatile("vmwrite %2, %1; setna %0"
		     : "=rm"(err) : "r"(field), "rm"(value) : "cc", "memory");
	return err;
}
static inline unsigned long vmread(unsigned long field)
{
	unsigned long value;
	asm volatile("vmread %1, %0" : "=rm"(value) : "r"(field) : "cc");
	return value;
}

static int wr_err;
#define VMW(field, val) do { \
	if (__vmwrite((field), (unsigned long)(val))) { \
		pr_err("hvx: vmwrite failed field=0x%x\n", (field)); wr_err++; } \
	} while (0)

/* ---- helpers ---- */
struct dtr { u16 limit; u64 base; } __packed;

/*
 * The VMX capability MSRs (0x480..0x490) are constant, but setup_vmcs read about a dozen
 * of them on every run call -- a few thousand cycles per vCPU turn (hv/hvx/FASTTURN.md).
 * hvx_init fills this cache once; rd() serves them from it afterwards.
 */
#define HVX_VMXCAP_LO	0x480
#define HVX_VMXCAP_N	0x11
static u64 hvx_vmxcap[HVX_VMXCAP_N];
static bool hvx_vmxcap_ok;

static inline u64 rd(u32 msr)
{
	u64 v;

	if (hvx_vmxcap_ok && msr - HVX_VMXCAP_LO < HVX_VMXCAP_N && (ft_opt & 4))
		return hvx_vmxcap[msr - HVX_VMXCAP_LO];
	rdmsrl(msr, v);
	return v;
}

static inline u32 adjust_ctl(u32 want, u32 msr)
{
	u64 v = rd(msr);
	u32 lo = (u32)v;         /* allowed-0: bit set => must be 1 */
	u32 hi = (u32)(v >> 32); /* allowed-1: bit clear => must be 0 */
	want |= lo;
	want &= hi;
	return want;
}

/* guest CR values must satisfy the IA32_VMX_CRn_FIXED MSRs */
static inline u64 fixed_cr(u32 fixed0, u32 fixed1, u64 want)
{
	return (want | rd(fixed0)) & rd(fixed1);
}

static inline u64 hvx_cr0(void) { u64 v; asm volatile("mov %%cr0,%0" : "=r"(v)); return v; }
static inline u64 hvx_cr3(void) { u64 v; asm volatile("mov %%cr3,%0" : "=r"(v)); return v; }
static inline u64 hvx_cr4(void) { u64 v; asm volatile("mov %%cr4,%0" : "=r"(v)); return v; }
static inline void hvx_set_cr4(u64 v) { asm volatile("mov %0,%%cr4" : : "r"(v)); }

/* flush cached EPT translations for all/this EPTP (fresh tables, be safe) */
static inline void invept_flush(u64 eptp)
{
	struct { u64 eptp, rsvd; } desc = { eptp, 0 };
	u64 cap = rd(MSR_VMX_EPT_VPID_CAP);
	u64 type;

	if (!(cap & EPT_CAP_INVEPT))
		return;
	type = (cap & EPT_CAP_INVEPT_GLOBAL) ? 2 : 1;
	asm volatile("invept %1, %0" : : "r"(type), "m"(desc) : "cc", "memory");
}

/* decode 64-bit TSS descriptor base from the GDT */
static u64 tr_base_from_gdt(u64 gdt_base, u16 tr_sel)
{
	u32 *d = (u32 *)(gdt_base + (tr_sel & 0xfff8));
	u64 base = ((d[0] >> 16) & 0xffff)
		 | ((u64)(d[1] & 0xff) << 16)
		 | ((u64)((d[1] >> 24) & 0xff) << 24)
		 | ((u64)d[2] << 32);
	return base;
}

static const char *reason_name(u32 r)
{
	switch (r) {
	case EXIT_REASON_EXC_NMI:       return "EXC/NMI";
	case EXIT_REASON_EXTERNAL_INT:  return "EXT-INT";
	case EXIT_REASON_TRIPLE_FAULT:  return "TRIPLE-FAULT";
	case EXIT_REASON_INTERRUPT_WINDOW: return "INT-WINDOW";
	case EXIT_REASON_CPUID:         return "CPUID";
	case EXIT_REASON_HLT:           return "HLT";
	case EXIT_REASON_RDTSC:         return "RDTSC";
	case EXIT_REASON_RDTSCP:        return "RDTSCP";
	case EXIT_REASON_RDMSR:         return "RDMSR";
	case EXIT_REASON_WRMSR:         return "WRMSR";
	case EXIT_REASON_MONITOR_TRAP:  return "MTF";
	case EXIT_REASON_PAUSE:         return "PAUSE";
	case EXIT_REASON_IO_INSTRUCTION: return "IO";
	case EXIT_REASON_VMCALL:        return "VMCALL";
	case EXIT_REASON_EPT_VIOLATION: return "EPT-VIOLATION";
	case EXIT_REASON_EPT_MISCONFIG: return "EPT-MISCONFIG";
	default: return "other";
	}
}

/*
 * Curated software MSR store (milestone 12). These MSRs have no host-visible
 * effect in our guest environment, so we back them with a per-VM array: WRMSR
 * stores, RDMSR returns the stored value (0 until first written). The slot order
 * here is the on-the-wire order in struct hvx_snapshot.sw_msr, so it must stay
 * fixed; SWMSR_COUNT is asserted equal to the ABI's HVX_NR_SW_MSR at load time.
 */
enum {
	SWMSR_STAR = 0,		/* 0xc0000081 */
	SWMSR_LSTAR,		/* 0xc0000082 */
	SWMSR_CSTAR,		/* 0xc0000083 */
	SWMSR_SFMASK,		/* 0xc0000084 */
	SWMSR_FS_BASE,		/* 0xc0000100 */
	SWMSR_GS_BASE,		/* 0xc0000101 */
	SWMSR_KERNEL_GS_BASE,	/* 0xc0000102 */
	SWMSR_SYSENTER_CS,	/* 0x174 */
	SWMSR_SYSENTER_ESP,	/* 0x175 */
	SWMSR_SYSENTER_EIP,	/* 0x176 */
	SWMSR_MISC_ENABLE,	/* 0x1a0 */
	SWMSR_COUNT
};

/* Map a guest MSR index to a software-store slot, or -1 if not stored here. */
static int swmsr_slot(u32 idx)
{
	switch (idx) {
	case GMSR_STAR:           return SWMSR_STAR;
	case GMSR_LSTAR:          return SWMSR_LSTAR;
	case GMSR_CSTAR:          return SWMSR_CSTAR;
	case GMSR_SFMASK:         return SWMSR_SFMASK;
	case GMSR_FS_BASE:        return SWMSR_FS_BASE;
	case GMSR_GS_BASE:        return SWMSR_GS_BASE;
	case GMSR_KERNEL_GS_BASE: return SWMSR_KERNEL_GS_BASE;
	case GMSR_SYSENTER_CS:    return SWMSR_SYSENTER_CS;
	case GMSR_SYSENTER_ESP:   return SWMSR_SYSENTER_ESP;
	case GMSR_SYSENTER_EIP:   return SWMSR_SYSENTER_EIP;
	case GMSR_MISC_ENABLE:    return SWMSR_MISC_ENABLE;
	default:                  return -1;
	}
}

/*
 * Guest page tables, built INSIDE guest memory: one 2 MiB identity page covers
 * guest-physical 0..0x1FFFFF (all guest RAM, since we cap at 512 pages). Only
 * EPT-backed pages actually resolve; touching beyond them raises an EPT
 * violation, reported to userspace.
 */
static void build_guest_page_tables(struct hvx_vm *vm)
{
	u64 *pml4 = (u64 *)vm->guest_pages[GPA_PML4 >> 12];
	u64 *pdpt = (u64 *)vm->guest_pages[GPA_PDPT >> 12];
	u64 *pd   = (u64 *)vm->guest_pages[GPA_PD >> 12];
	unsigned int j;

	pml4[0] = GPA_PDPT | 0x3;	/* present | write */
	pdpt[0] = GPA_PD   | 0x3;
	/* 512 identity 2 MiB pages cover the low 1 GiB (like the KVM path); only
	 * EPT-backed GPAs actually resolve, the rest fault as EPT violations.
	 * KNOWN LIMIT: build_guest_system() later writes the interrupt-handler blob
	 * (GPA_ISR 0x2800) and its counter (0x2f00) into this same PD page, which
	 * overwrites PD entries 256..511 -- so this default map is only valid for the
	 * low 512 MiB. Hand-written guests needing more install their own PDPT leaves
	 * (hvx_scale_test, hvx_bigram_test); real Linux builds its own tables. Left as
	 * is because those bytes are in every hand-written test's hashed RAM. */
	for (j = 0; j < 512; j++)
		pd[j] = ((u64)j << 21) | 0x83;	/* 2 MiB PS page, present | write */
}

/* host-virtual address backing a guest-physical address (must be an allocated
 * page; used only for the low system pages, which always exist) */
static inline void *gpa_host(struct hvx_vm *vm, unsigned long gpa)
{
	unsigned long gfn = gpa >> 12;

	if (gfn >= vm->n_low_pages)		/* the high block (M179) */
		gfn = vm->n_low_pages + (gfn - HVX_RAM_HIGH_GFN);
	return (void *)(vm->guest_pages[gfn] + (gpa & (PAGE_SIZE - 1)));
}

/* M179: the RAM page index of guest-physical page `gfn`, or -1 when it is not guest RAM (the
 * hole between the low block's end and 4 GiB, or above the high block). */
static inline long hvx_gfn_ram(const struct hvx_vm *vm, u64 gfn)
{
	if (gfn < vm->n_low_pages)
		return (long)gfn;
	if (gfn >= HVX_RAM_HIGH_GFN && gfn - HVX_RAM_HIGH_GFN < vm->n_guest_pages - vm->n_low_pages)
		return (long)(vm->n_low_pages + (gfn - HVX_RAM_HIGH_GFN));
	return -1;
}

static inline bool hvx_gpa_is_ram(const struct hvx_vm *vm, u64 gpa)
{
	return hvx_gfn_ram(vm, gpa >> PAGE_SHIFT) >= 0;
}

/* M179: the guest-physical 2 MiB block that EPT PT k (RAM pages k*512 ..) maps */
static inline unsigned int hvx_ept_gblock(const struct hvx_vm *vm, unsigned int k)
{
	unsigned int low_pts = DIV_ROUND_UP(vm->n_low_pages, EPT_PT_LEAVES);

	return k < low_pts ? k : k - low_pts + (unsigned int)(HVX_RAM_HIGH_GFN / EPT_PT_LEAVES);
}

/*
 * Interrupt handler blob (runs at GPA_ISR when an injected vector is delivered):
 *   inc qword [GPA_INTR_COUNT]   ; record that the handler ran
 *   iretq                        ; return to the interrupted instruction
 */
static const u8 hvx_isr_blob[] = {
	0x48, 0xff, 0x04, 0x25, (u8)GPA_INTR_COUNT, (u8)(GPA_INTR_COUNT >> 8), 0x00, 0x00,
	0x48, 0xcf,
};

/*
 * Build the guest's protected environment so it can actually receive an
 * interrupt: a GDT (null / 64-bit code 0x08 / data 0x10 / TSS 0x18), a 64-bit
 * IDT whose gates point at GPA_ISR with CS=0x08, and the handler blob. All of
 * this lives in the free space of the page-table pages (see the GPA_ map). The
 * segments match the flat DPL0 hidden state we load in the VMCS, so interrupt
 * delivery reloading CS/SS from the GDT stays consistent.
 */
static void build_guest_system(struct hvx_vm *vm)
{
	u64 *gdt = gpa_host(vm, GPA_GDT);
	u64 *idt = gpa_host(vm, GPA_IDT);
	u64 tss_lo, gate_lo;
	unsigned int v;

	gdt[0] = 0;
	gdt[1] = 0x00af9b000000ffffULL;		/* 0x08: 64-bit code, DPL0, present */
	gdt[2] = 0x00cf93000000ffffULL;		/* 0x10: data, DPL0, present */
	/* 0x18: 16-byte 64-bit TSS descriptor, base=GPA_TSS, limit=0x67, busy */
	tss_lo = 0x67ULL
	       | ((GPA_TSS & 0xffffffULL) << 16)
	       | (0x8bULL << 40)
	       | (((GPA_TSS >> 24) & 0xffULL) << 56);
	gdt[3] = tss_lo;
	gdt[4] = (GPA_TSS >> 32) & 0xffffffffULL;

	/* IDT: all gates not-present except the vectors we may inject (0..MAX),
	 * each a 64-bit interrupt gate to GPA_ISR with selector 0x08. */
	gate_lo = (GPA_ISR & 0xffffULL)
		| (0x08ULL << 16)		/* selector */
		| (0x8eULL << 40)		/* P=1, DPL0, type=0xE (int gate) */
		| (((GPA_ISR >> 16) & 0xffffULL) << 48);
	for (v = 0; v <= IDT_MAX_GATE; v++) {
		idt[2 * v]     = gate_lo;
		idt[2 * v + 1] = (GPA_ISR >> 32) & 0xffffffffULL;
	}

	memcpy(gpa_host(vm, GPA_ISR), hvx_isr_blob, sizeof(hvx_isr_blob));
}

/*
 * EPT identity structure over guest RAM: GPA page i -> host page
 * guest_pages[i], RWX, memtype WB with ignore-PAT, using 4 KiB leaves. One EPT PT
 * covers 512 pages (2 MiB); n_ept_pt = ceil(n/512) of them. Milestone 28: the PD
 * level FANS OUT -- one PD page holds 512 PT entries (1 GiB), so n_ept_pd =
 * ceil(n_ept_pt/512) PD pages hang off the single PDPT (PDPT entry p -> PD page p),
 * reaching up to 512 GiB of GPA. M179: the PTs of RAM above the low block hang at 4 GiB
 * (hvx_ept_gblock); the PD of the 4th GiB then has no entries. Returns the EPTP (4-level
 * walk, WB).
 */
static u64 build_ept(struct hvx_vm *vm)
{
	u64 *l4 = (u64 *)vm->ept_pg[0], *l3 = (u64 *)vm->ept_pg[1];
	unsigned int k, i, p;

	l4[0] = __pa((void *)vm->ept_pg[1]) | 0x7;
	/* PDPT entry p -> PD page p (each PD page maps 512 PTs = 1 GiB of GPA) */
	for (p = 0; p < vm->n_ept_pd; p++)
		l3[p] = __pa((void *)vm->ept_pd[p]) | 0x7;
	for (k = 0; k < vm->n_ept_pt; k++) {
		unsigned int g = hvx_ept_gblock(vm, k);	/* == k below the hole (M179) */
		u64 *l2 = (u64 *)vm->ept_pd[g / EPT_PT_LEAVES];
		u64 *l1 = (u64 *)vm->ept_pt[k];

		l2[g % EPT_PT_LEAVES] = __pa((void *)vm->ept_pt[k]) | 0x7;
		for (i = 0; i < EPT_PT_LEAVES; i++) {
			unsigned int pg = k * EPT_PT_LEAVES + i;

			if (pg >= vm->n_guest_pages)
				break;
			l1[i] = __pa((void *)vm->guest_pages[pg]) |
				0x7 | (6ULL << 3) | (1ULL << 6);
		}
	}

	return __pa((void *)vm->ept_pg[0]) | (3ULL << 3) | 6ULL;
}

/*
 * Seed the software sregs (milestone 13) to the flat long-mode defaults hvx has
 * always hardcoded, so every existing guest starts bit-identically. The control
 * registers are stored already post-fixed_cr() (idempotent, since setup_vmcs
 * re-applies fixed_cr to guard against a user SET_SREGS that omits fixed bits).
 */
static void seed_sregs(struct hvx_vcpu *vcpu)
{
	struct hvx_sregs *s = &vcpu->sregs;

	memset(s, 0, sizeof(*s));
	s->cr0  = fixed_cr(MSR_VMX_CR0_FIXED0, MSR_VMX_CR0_FIXED1,
			   CR0_PE_ | CR0_NE_ | CR0_PG_);
	s->cr3  = GPA_PML4;
	s->cr4  = fixed_cr(MSR_VMX_CR4_FIXED0, MSR_VMX_CR4_FIXED1, CR4_PAE_);
	s->efer = EFER_LME_ | EFER_LMA_;
	s->dr7  = 0x400;

	s->cs.selector = 0x08; s->cs.base = 0; s->cs.limit = 0xffffffff; s->cs.ar = 0xa09b;
	s->ss.selector = 0x10; s->ss.base = 0; s->ss.limit = 0xffffffff; s->ss.ar = 0xc093;
	s->ds = s->ss; s->es = s->ss; s->fs = s->ss; s->gs = s->ss;  /* all flat DPL0 data */
	s->tr.selector = 0x18; s->tr.base = GPA_TSS; s->tr.limit = 0x67; s->tr.ar = 0x8b;
	/* ldtr stays zeroed: it is unusable, and its VMCS AR bit 16 (unusable) does
	 * not fit hvx_segment.ar (u16); setup_vmcs hardcodes the unusable LDTR. */
	s->gdt_base = GPA_GDT; s->gdt_limit = GDT_LIMIT;
	s->idt_base = GPA_IDT; s->idt_limit = IDT_LIMIT;
}

/*
 * Save the guest's privileged state out of the current VMCS into vm->sregs after
 * a VM exit (milestone 13), so a change the guest made -- a new CR3, a reloaded
 * segment, an LGDT -- survives to the next HVX_RUN (which rebuilds the VMCS from
 * vm->sregs). Segment AR is truncated to the ABI's 16-bit field; the only AR our
 * guests ever leave with bit 16 (unusable) set is LDTR, which setup_vmcs forces
 * unusable regardless, so nothing is lost. The VMCS must be current (post
 * vmptrld, pre vmclear) when this runs.
 */
/*
 * An UNUSABLE segment (VMCS access-rights bit 16: the guest loaded a null selector)
 * has no architecturally defined limit or attribute bits, and the CPU leaves
 * different leftovers there depending on history: 0x000fffff / 0 right after the
 * guest's own null load, 0xffffffff / G|DB once the VMCS has been rebuilt and
 * re-entered. Neither is visible to the guest, but both land in the snapshot, so a
 * VMCS rebuild at a host-timed point (a preempt re-entry) changed the fingerprint of
 * an otherwise identical run. Record one fixed form. Selector and base are kept:
 * in 64-bit mode the FS/GS base is used even when the segment is unusable.
 */
static inline void seg_canon(struct hvx_segment *sg, u32 vmcs_ar)
{
	if (vmcs_ar & 0x10000) {
		sg->limit = 0xffffffff;
		sg->ar = 0xc000;
	}
}

static void save_sregs(struct hvx_vcpu *vcpu)
{
	struct hvx_sregs *s = &vcpu->sregs;

	s->cr0  = vmread(GUEST_CR0);
	s->cr3  = vmread(GUEST_CR3);
	s->cr4  = vmread(GUEST_CR4);
	s->efer = vmread(GUEST_IA32_EFER);
	s->dr7  = vmread(GUEST_DR7);

	s->cs.selector = (u16)vmread(GUEST_CS_SEL); s->cs.base = vmread(GUEST_CS_BASE);
	s->cs.limit = (u32)vmread(GUEST_CS_LIM);    s->cs.ar = (u16)vmread(GUEST_CS_AR);
	s->ss.selector = (u16)vmread(GUEST_SS_SEL); s->ss.base = vmread(GUEST_SS_BASE);
	s->ss.limit = (u32)vmread(GUEST_SS_LIM);    s->ss.ar = (u16)vmread(GUEST_SS_AR);
	seg_canon(&s->ss, (u32)vmread(GUEST_SS_AR));
	s->ds.selector = (u16)vmread(GUEST_DS_SEL); s->ds.base = vmread(GUEST_DS_BASE);
	s->ds.limit = (u32)vmread(GUEST_DS_LIM);    s->ds.ar = (u16)vmread(GUEST_DS_AR);
	seg_canon(&s->ds, (u32)vmread(GUEST_DS_AR));
	s->es.selector = (u16)vmread(GUEST_ES_SEL); s->es.base = vmread(GUEST_ES_BASE);
	s->es.limit = (u32)vmread(GUEST_ES_LIM);    s->es.ar = (u16)vmread(GUEST_ES_AR);
	seg_canon(&s->es, (u32)vmread(GUEST_ES_AR));
	s->fs.selector = (u16)vmread(GUEST_FS_SEL); s->fs.base = vmread(GUEST_FS_BASE_F);
	s->fs.limit = (u32)vmread(GUEST_FS_LIM);    s->fs.ar = (u16)vmread(GUEST_FS_AR);
	seg_canon(&s->fs, (u32)vmread(GUEST_FS_AR));
	s->gs.selector = (u16)vmread(GUEST_GS_SEL); s->gs.base = vmread(GUEST_GS_BASE_F);
	s->gs.limit = (u32)vmread(GUEST_GS_LIM);    s->gs.ar = (u16)vmread(GUEST_GS_AR);
	seg_canon(&s->gs, (u32)vmread(GUEST_GS_AR));
	s->tr.selector = (u16)vmread(GUEST_TR_SEL); s->tr.base = vmread(GUEST_TR_BASE);
	s->tr.limit = (u32)vmread(GUEST_TR_LIM);    s->tr.ar = (u16)vmread(GUEST_TR_AR);
	s->ldtr.selector = (u16)vmread(GUEST_LDTR_SEL); s->ldtr.base = vmread(GUEST_LDTR_BASE);
	s->ldtr.limit = (u32)vmread(GUEST_LDTR_LIM);    s->ldtr.ar = (u16)vmread(GUEST_LDTR_AR);

	s->gdt_base = vmread(GUEST_GDTR_BASE); s->gdt_limit = (u32)vmread(GUEST_GDTR_LIM);
	s->idt_base = vmread(GUEST_IDTR_BASE); s->idt_limit = (u32)vmread(GUEST_IDTR_LIM);
}

/*
 * Reconstruct a VMCS segment access-rights value from the ABI's 16-bit `ar`
 * (milestone 14). The ABI hvx_segment.ar (u16) cannot hold the VMCS "unusable"
 * bit (16), so a segment the guest made unusable -- e.g. real Linux nulling
 * DS/ES/SS in long mode -- round-trips through save_sregs as an `ar` with the
 * present bit (7) clear. A usable segment always has P=1, and a present-clear
 * "usable" segment is invalid for VM-entry anyway, so any `ar` with P=0 is
 * reloaded as unusable (0x10000). Without this, the M13 sregs save/reload
 * corrupts DS/ES/SS across an HVX_RUN (THROTTLE) boundary and VM-entry fails
 * with "invalid guest state" (exit 0x80000021).
 */
static inline u32 seg_ar(u16 ar)
{
	return (ar & 0x80) ? (u32)ar : 0x10000u;
}

/* Fill the VMCS from the VM's register file and EPT. Pinned, IRQs off.
 * mtf enables the Monitor Trap Flag (single-step: a VM-exit per instruction). */
/* ---- PMU branch clock (HVX_VM_PMU_CLOCK) ----
 * GP counter 0 counts retired conditional branches (event 0xC4 umask 0x01 -- the
 * event rr relies on for exact replay; plain instruction counts are perturbed by
 * interrupt delivery), ring 0 + ring 3, enabled ONLY in VMX non-root: the VMCS
 * loads IA32_PERF_GLOBAL_CTRL = PMC0 on entry and = 0 on exit, so not one host
 * branch is counted. The counter is programmed to overflow HVX_PMU_MARGIN
 * branches before the next tick; the overflow interrupt (LVTPC, re-pointed from
 * NMI to a harmless fixed vector for the duration of the run) forces a VM exit
 * with some skid, and from there MTF single-steps to the exact branch.
 */
#define MSR_PMC0		0x000000c1
#define MSR_PERFEVTSEL0		0x00000186
#define MSR_PERF_GLOBAL_STATUS	0x0000038e
#define MSR_PERF_GLOBAL_CTRL	0x0000038f
#define MSR_PERF_GLOBAL_OVF	0x00000390
#define HVX_PMU_EVTSEL		(0xc4ULL | (0x01ULL << 8) | (1ULL << 16) | (1ULL << 17) | \
				 (1ULL << 20) | (1ULL << 22))	/* cond-branch, USR|OS|INT|EN */
#define HVX_PMU_MASK		((1ULL << 48) - 1)
#define HVX_PMU_MARGIN		256ULL		/* branches of slack for overflow skid */
/*
 * Landing steps are per INSTRUCTION, and a REP string instruction takes one MTF exit per
 * iteration without retiring a single conditional branch -- a 64 MiB `rep stos` would be
 * tens of millions of steps. So after HVX_PMU_HOP_STEPS consecutive steps with no branch
 * retired, and while at least HVX_PMU_HOP_MIN branches still remain, the landing "hops":
 * it runs free with the counter set to overflow on the very next branch, then resumes
 * stepping. HOP_MIN leaves room for that overflow's skid.
 */
#define HVX_PMU_HOP_STEPS	32U
#define HVX_PMU_HOP_MIN		128ULL
#define HVX_PMU_PERIOD		(1ULL << 20)	/* default: branches per tick */
#define HVX_PMU_TICK_TSC	3200000ULL	/* default: 2 ms of the 1.6 GHz virtual TSC */
/*
 * Instruction-clock units per tick. Device latencies are counted on that clock
 * (a disk completion is due 16 units after its kick), and it otherwise moves one
 * unit per retired exit. A tick that moved it by only one would make a disk
 * request take 16 ticks (~16M guest branches) whenever the rest of the guest is
 * spinning without exits -- MySQL's redo-log threads do exactly that, and the
 * data-directory initialisation took minutes. A real Linux boot retires about one
 * exit per 360 branches, so 2^20 branches are worth roughly 2900 units; 2048 keeps
 * the two notions of elapsed time on the same scale.
 */
#define HVX_PMU_TICK_ICOUNT	2048ULL
#define HVX_PMU_TICK_GUARD	256U		/* THROTTLE-cadence weight of one tick */
#define HVX_PMU_LVT_VECTOR	0xf2		/* host POSTED_INTR_VECTOR: handler only EOIs */
/*
 * The branch counter counts in the guest only (M156). The VMCS loads IA32_PERF_GLOBAL_CTRL = 1 on
 * entry and 0 on exit, but a host NMI that arrives in root mode between an exit and the next entry
 * runs perf's NMI handler, which ends by writing IA32_PERF_GLOBAL_CTRL with every counter enabled.
 * PMC0 then counted the host's own conditional branches up to the next entry, and a landing step
 * whose counter was kept running (or one programmed before the NMI) added them to the guest's count:
 * the next tick or turn end landed early, a host-timing-dependent difference (hvx_sched_sweep_test,
 * about one run in 40; a self-NMI in root mode reproduced it every time). With pmu_guard (the
 * default) PERFEVTSEL0 is enabled only while the guest runs: the VM-entry MSR-load area writes the
 * event, the VM-exit MSR-load area writes 0, and root mode keeps it 0, so nothing the host does
 * between two entries can count. No guest-visible value changes when no NMI arrives.
 * pmu_guard=0: the old behaviour (for A/B timing).
 */
static unsigned int pmu_guard = 1;
module_param(pmu_guard, uint, 0644);
#define HVX_PMU_MSR_AREA	2048		/* offset in fx_page: [0] entry load, [1] exit load */
struct hvx_msr_ent {			/* a VM-entry/exit MSR-load area entry (SDM 25.7.2, 25.8.2) */
	u32 index;
	u32 reserved;
	u64 value;
};
#define APIC_LVTPC_OFF		0x340
static bool hvx_pmu_ok;			/* CPU + VMX controls can run the branch clock */
/*
 * The branch clock must count ONLY guest branches. A System Management Interrupt taken
 * while the guest runs executes the firmware's SMM handler with the guest's
 * IA32_PERF_GLOBAL_CTRL still loaded, and without IA32_DEBUGCTL.FREEZE_WHILE_SMM the
 * counter keeps counting the handler's branches (SMM code runs at CPL 0, which the event
 * select counts). The guest then sees its next tick land early, at a host-timed point.
 * hvx does not use "load debug controls" otherwise, so DEBUGCTL inside the guest is
 * whatever the last VM exit left there: 0. With hvx_smm_freeze the branch-clock entry
 * loads DEBUGCTL = FREEZE_WHILE_SMM (and DR7 from the register file, as on any VMCS that
 * loads debug controls; guest DR7 writes were never persistent, an exit resets DR7 to
 * 0x400, and the seeded value is 0x400).
 */
static bool hvx_smm_freeze;		/* PERF_CAPABILITIES.SMM_FREEZE + "load debug controls" */
static bool hvx_pt_ok;			/* Intel PT usable in VMX non-root, ToPA output, 2 IP ranges */
static bool hvx_ept_ad;			/* EPT accessed/dirty flags (IA32_VMX_EPT_VPID_CAP bit 21; M138) */

/* The last traced VM's buffer, kept after the VM is freed so that a harness that
 * never asks for the trace (pt_force) can still be read out: HVX_GET_PT / HVX_READ_PT
 * on an fd with no PT VM refer to it. Protected by hvx_pt_last_lock. */
static DEFINE_MUTEX(hvx_pt_last_lock);
static struct page **hvx_pt_last_chunk;
static unsigned int hvx_pt_last_nchunk;
static u64 hvx_pt_last_bytes, hvx_pt_last_status;

static void hvx_pt_free_chunks(struct page **ch, unsigned int n)
{
	unsigned int i;

	if (!ch)
		return;
	for (i = 0; i < n; i++)
		if (ch[i])
			__free_pages(ch[i], HVX_PT_CHUNK_ORDER);
	kfree(ch);
}

/* bytes written so far, from a saved RTIT_OUTPUT_MASK_PTRS: entry index in bits 31:7,
 * offset into that entry's region in bits 63:32 */
static u64 hvx_pt_bytes(u64 mask, unsigned int nchunk)
{
	u64 idx = (mask & 0xffffff80ULL) >> 7, off = mask >> 32;

	if (idx >= nchunk)
		return (u64)nchunk << (PAGE_SHIFT + HVX_PT_CHUNK_ORDER);
	return (idx << (PAGE_SHIFT + HVX_PT_CHUNK_ORDER)) + off;
}

static void hvx_pt_release(struct hvx_vm *vm)
{
	if (!vm->pt_chunk)
		return;
	/* keep the buffer as "the last trace" instead of freeing it */
	mutex_lock(&hvx_pt_last_lock);
	hvx_pt_free_chunks(hvx_pt_last_chunk, hvx_pt_last_nchunk);
	hvx_pt_last_chunk = vm->pt_chunk;
	hvx_pt_last_nchunk = vm->pt_nchunk;
	hvx_pt_last_bytes = hvx_pt_bytes(vm->pt_mask, vm->pt_nchunk);
	hvx_pt_last_status = vm->pt_status;
	mutex_unlock(&hvx_pt_last_lock);
	pr_info("hvx: PT trace kept: %llu bytes%s\n", hvx_pt_last_bytes,
		(vm->pt_status & HVX_RTIT_ST_STOPPED) ? " (buffer FULL, trace truncated)" : "");
	vm->pt_chunk = NULL;
	vm->pt_nchunk = 0;
	if (vm->pt_topa)
		__free_page(vm->pt_topa);
	vm->pt_topa = NULL;
	vm->pt_on = false;
}

/* Allocate the output (mib MiB in 2 MiB regions, one ToPA table, STOP at the end) and
 * compute the guest RTIT_CTL. Kernel tracing needs at least one IP range: the host
 * also runs at CPL0, and the ranges are what keep host code out of the trace. */
static inline bool hvx_canonical(u64 a)	/* 48-bit virtual addresses */
{
	return (u64)((s64)(a << 16) >> 16) == a;
}

static int hvx_pt_setup(struct hvx_vm *vm, u64 mib, u32 flags, const u64 *lo,
			const u64 *hi, u64 cr3)
{
	unsigned int n, i;
	u64 *topa, ctl = HVX_RTIT_TRACEEN | HVX_RTIT_TOPA | HVX_RTIT_BRANCH;

	if (!hvx_pt_ok)
		return -ENODEV;
	if (vm->pt_chunk || !(flags & (HVX_PT_KERNEL | HVX_PT_USER)))
		return -EINVAL;
	if ((flags & HVX_PT_KERNEL) && !hi[0] && !hi[1])
		return -EINVAL;
	/* M97: the run loop writes these into IA32_RTIT_ADDRn_A/B and IA32_RTIT_CR3_MATCH
	 * with wrmsrl; a non-canonical address or a CR3 with bits 4:0 set #GPs there
	 * ("unchecked MSR access error") and leaves the filter unprogrammed */
	for (i = 0; i < 2; i++)
		if (hi[i] && (!hvx_canonical(lo[i]) || !hvx_canonical(hi[i] - 1)))
			return -EINVAL;
	if ((flags & HVX_PT_CR3) && (cr3 & 0x1f))
		return -EINVAL;
	n = (unsigned int)DIV_ROUND_UP(mib ? mib : 256, 2);
	if (n < 1 || n > 511)
		return -EINVAL;
	vm->pt_chunk = kcalloc(n, sizeof(*vm->pt_chunk), GFP_KERNEL);
	vm->pt_topa = alloc_page(GFP_KERNEL | __GFP_ZERO);
	if (!vm->pt_chunk || !vm->pt_topa)
		goto nomem;
	vm->pt_nchunk = n;
	topa = page_address(vm->pt_topa);
	for (i = 0; i < n; i++) {
		vm->pt_chunk[i] = alloc_pages(GFP_KERNEL | __GFP_ZERO | __GFP_NOWARN,
					      HVX_PT_CHUNK_ORDER);
		if (!vm->pt_chunk[i])
			goto nomem;
		topa[i] = page_to_phys(vm->pt_chunk[i]) | ((u64)HVX_PT_CHUNK_ORDER << 6);
	}
	topa[n - 1] |= TOPA_STOP;			/* full: stop, do not wrap */
	topa[n] = page_to_phys(vm->pt_topa) | TOPA_END;
	if (flags & HVX_PT_KERNEL)
		ctl |= HVX_RTIT_OS;
	if (flags & HVX_PT_USER)
		ctl |= HVX_RTIT_USR;
	if (flags & HVX_PT_NORETCOMP)
		ctl |= HVX_RTIT_DISRETC;
	for (i = 0; i < 2; i++) {
		vm->pt_lo[i] = lo[i];
		vm->pt_hi[i] = hi[i];
		if (hi[i])
			ctl |= HVX_RTIT_ADDR_CFG(i);
	}
	if (flags & HVX_PT_CR3) {
		vm->pt_cr3 = cr3;
		ctl |= HVX_RTIT_CR3EN;
	}
	vm->pt_ctl = ctl;
	vm->pt_mask = 0x7f;				/* entry 0, offset 0 */
	vm->pt_status = 0;
	vm->pt_on = true;
	pr_info("hvx: PT guest trace on: %u MiB, ctl=%#llx ranges [%#llx,%#llx) [%#llx,%#llx)\n",
		n * 2, ctl, lo[0], hi[0], lo[1], hi[1]);
	return 0;
nomem:
	hvx_pt_free_chunks(vm->pt_chunk, n);
	vm->pt_chunk = NULL;
	vm->pt_nchunk = 0;
	if (vm->pt_topa)
		__free_page(vm->pt_topa);
	vm->pt_topa = NULL;
	return -ENOMEM;
}

/* Copy trace bytes [off, off+len) of a chunk list to userspace. */
static int hvx_pt_copy(struct page **ch, unsigned int n, u64 total, u64 off, u64 len,
		       void __user *dst)
{
	const u64 csz = 1ULL << (PAGE_SHIFT + HVX_PT_CHUNK_ORDER);

	if (off > total)
		return -EINVAL;
	if (len > total - off)
		len = total - off;
	while (len) {
		u64 ci = off / csz, co = off % csz, l = min(len, csz - co);

		if (ci >= n)
			return -EINVAL;
		if (copy_to_user(dst, (u8 *)page_address(ch[ci]) + co, l))
			return -EFAULT;
		dst += l; off += l; len -= l;
	}
	return 0;
}
static void __iomem *hvx_lapic;		/* host xAPIC page (NULL when the host is x2APIC) */
static bool hvx_host_x2apic;

static inline u32 hvx_lvtpc_read(void)
{
	return hvx_host_x2apic ? (u32)rd(0x834) : readl(hvx_lapic + APIC_LVTPC_OFF);
}
static inline void hvx_lvtpc_write(u32 v)
{
	if (hvx_host_x2apic)
		wrmsrl(0x834, v);
	else
		writel(v, hvx_lapic + APIC_LVTPC_OFF);
}
/* chaos_nmi: an NMI to this CPU (its own APIC ID, physical destination, level assert) */
static void hvx_chaos_nmi_send(void)
{
	this_cpu_write(hvx_chaos_nmi_pending, 1);
	if (hvx_host_x2apic) {
		wrmsrl(0x830, (rd(0x802) << 32) | 0x4400);
	} else {
		writel(readl(hvx_lapic + 0x20) & 0xff000000U, hvx_lapic + 0x310);
		writel(0x4400, hvx_lapic + 0x300);
	}
}

static int setup_vmcs(struct hvx_vcpu *vcpu, bool mtf)
{
	struct hvx_vm *vm = vcpu->vm;		/* shared: eptp */
	struct dtr gdt, idt;
	u16 cs, ss, ds, es, fs, gs, tr;
	u64 tr_base;
	u32 proc, proc2;
	int use_true = (rd(MSR_VMX_BASIC) >> 55) & 1;
	u32 pin_msr   = use_true ? MSR_VMX_TRUE_PINBASED  : MSR_VMX_PINBASED;
	u32 proc_msr  = use_true ? MSR_VMX_TRUE_PROCBASED : MSR_VMX_PROCBASED;
	u32 exit_msr  = use_true ? MSR_VMX_TRUE_EXIT      : MSR_VMX_EXIT;
	u32 entry_msr = use_true ? MSR_VMX_TRUE_ENTRY     : MSR_VMX_ENTRY;
	/* milestone 13: guest privileged state comes from vm->sregs (seeded to the
	 * flat long-mode defaults on CREATE, updated from the VMCS after each exit).
	 * fixed_cr() still forces the IA32_VMX_CRn_FIXED bits so a user-supplied CR0/
	 * CR4 that omits them cannot fault VM-entry. */
	u64 guest_cr0 = fixed_cr(MSR_VMX_CR0_FIXED0, MSR_VMX_CR0_FIXED1, vcpu->sregs.cr0);
	u64 guest_cr4 = fixed_cr(MSR_VMX_CR4_FIXED0, MSR_VMX_CR4_FIXED1, vcpu->sregs.cr4);
	/* SMP boot: an AP starts in real mode, so with the "unrestricted guest" control
	 * CR0.PE and CR0.PG are not forced to 1, and the IA-32e-mode entry control follows
	 * the guest's EFER.LMA instead of being always on. */
	const bool ug = vm->smp_boot;
	bool ia32e = true;

	if (ug) {
		guest_cr0 = (vcpu->sregs.cr0 | (rd(MSR_VMX_CR0_FIXED0) & ~(u64)(CR0_PE_ | CR0_PG_))) &
			    rd(MSR_VMX_CR0_FIXED1);
		ia32e = !!(vcpu->sregs.efer & EFER_LMA_);
	}

	asm volatile("mov %%cs,%0" : "=r"(cs));
	asm volatile("mov %%ss,%0" : "=r"(ss));
	asm volatile("mov %%ds,%0" : "=r"(ds));
	asm volatile("mov %%es,%0" : "=r"(es));
	asm volatile("mov %%fs,%0" : "=r"(fs));
	asm volatile("mov %%gs,%0" : "=r"(gs));
	asm volatile("str %0" : "=r"(tr));
	asm volatile("sgdt %0" : "=m"(gdt));
	asm volatile("sidt %0" : "=m"(idt));
	tr_base = tr_base_from_gdt(gdt.base, tr);

	/* ---- controls: secondary ctls + EPT + RDTSC-exiting are mandatory;
	 *      MTF is requested only when single-stepping ---- */
	proc = adjust_ctl(PROCBASED_HLT_EXITING | PROCBASED_RDTSC_EXITING |
			  PROCBASED_IO_EXITING | (mtf ? PROCBASED_MONITOR_TRAP : 0) |
			  (vcpu->vm->pause_exit ? PROCBASED_PAUSE_EXITING : 0) |
			  PROCBASED_SECONDARY, proc_msr);
	if (!(proc & PROCBASED_SECONDARY)) {
		pr_err("hvx: secondary exec controls not supported\n");
		return -1;
	}
	if (!(proc & PROCBASED_RDTSC_EXITING)) {
		pr_err("hvx: RDTSC-exiting not supported by this CPU\n");
		return -1;
	}
	if (!(proc & PROCBASED_IO_EXITING)) {
		pr_err("hvx: unconditional I/O exiting not supported by this CPU\n");
		return -1;
	}
	if (mtf && !(proc & PROCBASED_MONITOR_TRAP)) {
		pr_err("hvx: Monitor Trap Flag not supported by this CPU\n");
		return -1;
	}
	proc2 = adjust_ctl(SEC_ENABLE_EPT | (vcpu->vm->pt_on ? SEC_CONCEAL_VMX_PT : 0) |
			   (probe_rand ? SEC_RDRAND_EXITING | SEC_RDSEED_EXITING : 0) |
			   (ug ? SEC_UNRESTRICTED_GUEST : 0),
			   MSR_VMX_PROCBASED2);
	if (!(proc2 & SEC_ENABLE_EPT)) {
		pr_err("hvx: EPT not supported by this CPU\n");
		return -1;
	}
	if (ug && !(proc2 & SEC_UNRESTRICTED_GUEST)) {
		pr_err("hvx: unrestricted guest not supported by this CPU\n");
		return -1;
	}

	/* external-interrupt exiting: physical interrupts must VM-exit to us, not
	 * be delivered into the guest's IDT (which happens in non-root operation
	 * whenever guest RFLAGS.IF=1, regardless of the host's masked IF) */
	VMW(PIN_EXEC_CTL,  adjust_ctl(PINBASED_EXTINT_EXITING | PINBASED_NMI_EXITING |
				      (chaos_ptimer ? PINBASED_PREEMPT_TIMER : 0), pin_msr));
	VMW(PROC_EXEC_CTL, proc);
	VMW(SEC_EXEC_CTL,  proc2);
	VMW(EPT_POINTER,   vm->eptp);
	VMW(VM_EXIT_CTL,   adjust_ctl(EXIT_HOST_ADDR_SPACE_64 |
				      EXIT_SAVE_IA32_EFER | EXIT_LOAD_IA32_EFER |
				      (vcpu->pmu_live ? EXIT_LOAD_PERF_GLOBAL : 0) |
				      (vm->pt_on ? EXIT_CONCEAL_VMX_PT : 0), exit_msr));
	VMW(VM_ENTRY_CTL,  adjust_ctl((ia32e ? ENTRY_IA32E_MODE : 0) | ENTRY_LOAD_IA32_EFER |
				      (vcpu->pmu_live ? ENTRY_LOAD_PERF_GLOBAL : 0) |
				      (vcpu->pmu_live && hvx_smm_freeze ? ENTRY_LOAD_DEBUG_CTLS : 0) |
				      (vm->pt_on ? ENTRY_CONCEAL_VMX_PT : 0), entry_msr));
	if (vcpu->pmu_live) {
		VMW(GUEST_IA32_PERF_GLOBAL, 1);		/* PMC0 counts in the guest ... */
		VMW(HOST_IA32_PERF_GLOBAL, 0);		/* ... and nothing counts in the host */
		if (hvx_smm_freeze)			/* ... nor in SMM (see hvx_smm_freeze) */
			VMW(GUEST_IA32_DEBUGCTL, DEBUGCTL_FREEZE_WHILE_SMM);
	}

	VMW(EXCEPTION_BITMAP, 0);
	VMW(PF_EC_MASK, 0);
	VMW(PF_EC_MATCH, 0);
	VMW(CR3_TARGET_COUNT, 0);
	VMW(VM_EXIT_MSR_STORE_CNT, 0);
	if (vcpu->pmu_live && vcpu->pmu_guarded) {
		/* M156 (see pmu_guard): PERFEVTSEL0 = the branch event while the guest runs, 0 in
		 * root mode, switched by the VM entry and the VM exit themselves */
		struct hvx_msr_ent *a = (struct hvx_msr_ent *)(vcpu->fx_page + HVX_PMU_MSR_AREA);

		a[0] = (struct hvx_msr_ent){ .index = MSR_PERFEVTSEL0, .value = HVX_PMU_EVTSEL };
		a[1] = (struct hvx_msr_ent){ .index = MSR_PERFEVTSEL0, .value = 0 };
		VMW(VM_ENTRY_MSR_LOAD_ADDR, __pa(&a[0]));
		VMW(VM_EXIT_MSR_LOAD_ADDR, __pa(&a[1]));
		VMW(VM_ENTRY_MSR_LOAD_CNT, 1);
		VMW(VM_EXIT_MSR_LOAD_CNT, 1);
	} else {
		VMW(VM_EXIT_MSR_LOAD_CNT, 0);
		VMW(VM_ENTRY_MSR_LOAD_CNT, 0);
	}
	VMW(VM_ENTRY_INTR_INFO, 0);
	VMW(VM_ENTRY_EXC_EC, 0);
	VMW(VM_ENTRY_INSN_LEN, 0);

	/* ---- host state (return cleanly to us on exit) ---- */
	VMW(HOST_CR0, hvx_cr0());
	VMW(HOST_CR3, hvx_cr3());
	VMW(HOST_CR4, hvx_cr4());
	VMW(HOST_CS_SEL, cs & 0xf8);
	VMW(HOST_SS_SEL, ss & 0xf8);
	VMW(HOST_DS_SEL, ds & 0xf8);
	VMW(HOST_ES_SEL, es & 0xf8);
	VMW(HOST_FS_SEL, fs & 0xf8);
	VMW(HOST_GS_SEL, gs & 0xf8);
	VMW(HOST_TR_SEL, tr & 0xf8);
	VMW(HOST_FS_BASE_F, rd(MSR_FS_BASE_));
	VMW(HOST_GS_BASE_F, rd(MSR_GS_BASE_));
	VMW(HOST_TR_BASE_F, tr_base);
	VMW(HOST_GDTR_BASE, gdt.base);
	VMW(HOST_IDTR_BASE, idt.base);
	VMW(HOST_SYSENTER_CS_F, rd(MSR_SYSENTER_CS));
	VMW(HOST_SYSENTER_ESP_F, rd(MSR_SYSENTER_ESP));
	VMW(HOST_SYSENTER_EIP_F, rd(MSR_SYSENTER_EIP));
	VMW(HOST_IA32_EFER, rd(MSR_EFER_));
	/* HOST_RSP/HOST_RIP are set inside hvx_vmx_run */

	/* ---- guest state: 64-bit, OWN address space via EPT (from vcpu->sregs) ---- */
	VMW(GUEST_CR0, guest_cr0);
	VMW(GUEST_CR3, vcpu->sregs.cr3);   /* persists the guest's page-table root */
	VMW(GUEST_CR4, guest_cr4);
	VMW(GUEST_IA32_EFER, vcpu->sregs.efer);
	VMW(GUEST_DR7, vcpu->sregs.dr7);

	/* rip/rsp/rflags from the userspace-set register file */
	VMW(GUEST_RIP, vcpu->regs.rip);
	VMW(GUEST_RSP, vcpu->regs.rsp);
	VMW(GUEST_RFLAGS, vcpu->regs.rflags ? vcpu->regs.rflags : 0x2);

	/* segments from vcpu->sregs (seeded flat DPL0; a guest that reloads one, or
	 * that we drive via HVX_SET_SREGS, has the new descriptor persist). LDTR is
	 * forced unusable here -- its unusable AR bit does not fit the ABI's u16. */
	VMW(GUEST_CS_SEL, vcpu->sregs.cs.selector); VMW(GUEST_CS_BASE, vcpu->sregs.cs.base);
	VMW(GUEST_CS_LIM, vcpu->sregs.cs.limit);    VMW(GUEST_CS_AR,   seg_ar(vcpu->sregs.cs.ar));
	VMW(GUEST_SS_SEL, vcpu->sregs.ss.selector); VMW(GUEST_SS_BASE, vcpu->sregs.ss.base);
	VMW(GUEST_SS_LIM, vcpu->sregs.ss.limit);    VMW(GUEST_SS_AR,   seg_ar(vcpu->sregs.ss.ar));
	VMW(GUEST_DS_SEL, vcpu->sregs.ds.selector); VMW(GUEST_DS_BASE, vcpu->sregs.ds.base);
	VMW(GUEST_DS_LIM, vcpu->sregs.ds.limit);    VMW(GUEST_DS_AR,   seg_ar(vcpu->sregs.ds.ar));
	VMW(GUEST_ES_SEL, vcpu->sregs.es.selector); VMW(GUEST_ES_BASE, vcpu->sregs.es.base);
	VMW(GUEST_ES_LIM, vcpu->sregs.es.limit);    VMW(GUEST_ES_AR,   seg_ar(vcpu->sregs.es.ar));
	VMW(GUEST_FS_SEL, vcpu->sregs.fs.selector); VMW(GUEST_FS_BASE_F, vcpu->sregs.fs.base);
	VMW(GUEST_FS_LIM, vcpu->sregs.fs.limit);    VMW(GUEST_FS_AR,   seg_ar(vcpu->sregs.fs.ar));
	VMW(GUEST_GS_SEL, vcpu->sregs.gs.selector); VMW(GUEST_GS_BASE_F, vcpu->sregs.gs.base);
	VMW(GUEST_GS_LIM, vcpu->sregs.gs.limit);    VMW(GUEST_GS_AR,   seg_ar(vcpu->sregs.gs.ar));
	VMW(GUEST_TR_SEL, vcpu->sregs.tr.selector); VMW(GUEST_TR_BASE, vcpu->sregs.tr.base);
	VMW(GUEST_TR_LIM, vcpu->sregs.tr.limit);    VMW(GUEST_TR_AR,   seg_ar(vcpu->sregs.tr.ar));
	VMW(GUEST_LDTR_SEL, vcpu->sregs.ldtr.selector);
	VMW(GUEST_LDTR_BASE, vcpu->sregs.ldtr.base);
	VMW(GUEST_LDTR_LIM, vcpu->sregs.ldtr.limit);
	VMW(GUEST_LDTR_AR, 0x10000);     /* unusable (see note above) */

	/* GDTR + IDTR from vcpu->sregs (seeded to the module-built GDT/IDT so the
	 * guest can take an injected interrupt; a guest LGDT/LIDT now persists) */
	VMW(GUEST_GDTR_BASE, vcpu->sregs.gdt_base); VMW(GUEST_GDTR_LIM, vcpu->sregs.gdt_limit);
	VMW(GUEST_IDTR_BASE, vcpu->sregs.idt_base); VMW(GUEST_IDTR_LIM, vcpu->sregs.idt_limit);

	VMW(GUEST_SYSENTER_CS_F, 0);
	VMW(GUEST_SYSENTER_ESP_F, 0);
	VMW(GUEST_SYSENTER_EIP_F, 0);
	VMW(GUEST_INTERRUPTIBILITY, 0);
	VMW(GUEST_ACTIVITY_STATE, 0);
	VMW(GUEST_PENDING_DBG, 0);
	VMW(VMCS_LINK_PTR, ~0ULL);

	/*
	 * CR4.VMXE (and any other IA32_VMX_CR4_FIXED0-forced bit) MUST stay 1 while
	 * the CPU is in VMX operation. But a real guest -- Linux's compressed
	 * head_64.S -- reads CR4, masks it down to PAE|MCE|LA57, and writes it back
	 * WITHOUT VMXE. That write violates the fixed-bit rule and #GPs; with no
	 * guest IDT yet it triple-faults (the M14 wall at guest RIP 0x10027e).
	 *
	 * Hide those bits the way KVM does: the CR4 guest/host mask makes the host
	 * OWN the VMX-forced bits, and the CR4 read shadow presents them as the
	 * guest expects (cleared). A guest MOV-to-CR4 whose owned bits equal the
	 * shadow does NOT VM-exit and leaves the physical (host-forced) VMXE intact,
	 * while the unmasked bits write through. Reads of CR4 return the shadow, so
	 * the guest never sees VMXE. CR0's VMX-forced bits (PE/PG/NE) are never
	 * cleared by the guest, so CR0 stays fully guest-owned (mask 0).
	 */
	{
		u64 cr4_owned = rd(MSR_VMX_CR4_FIXED0);	/* forced-1 CR4 bits (incl. VMXE) */

		VMW(CR0_GH_MASK, 0);
		VMW(CR4_GH_MASK, cr4_owned);
		VMW(CR0_READ_SHADOW, guest_cr0);
		VMW(CR4_READ_SHADOW, guest_cr4 & ~cr4_owned);
	}
	return 0;
}

/*
 * ---- Guest AVX / XSAVE (HVX_VM_X86_64_V3) ----
 * fx_page holds the vCPU's standard-format XSAVE area for x87|SSE|AVX (832 bytes;
 * bytes 0..511 are the FXSAVE-compatible legacy region, 512..575 the header, 576..831
 * the upper halves of YMM0-15). The run loads it with XRSTOR after kernel_fpu_begin and
 * saves it with plain XSAVE (never XSAVEOPT: its "modified" optimisation depends on VMX
 * transitions). XSAVE may skip a component in its init state (XSTATE_BV bit 0, data not
 * written): hvx_xs_save writes the init values into such a component so the bytes are a
 * pure function of the guest, and keeps XSTATE_BV, so the next XRSTOR re-creates the
 * same init tracking the guest's own XSAVE would observe. Every XRSTOR runs in the
 * HOST, where a #GP is fatal, so the area is sanitised before each one.
 */
#define EXIT_REASON_XSETBV	55
#define HVX_XS_MASK		0x7ULL	/* x87 | SSE | AVX: the only components offered */
#define HVX_XS_SIZE		832	/* standard-format area for HVX_XS_MASK */
#define HVX_XS_HDR		512
#define HVX_XS_YMMH		576

static inline void hvx_xsave(void *a, u64 m)
{
	asm volatile("xsave64 (%0)" :: "r" (a), "a" ((u32)m), "d" ((u32)(m >> 32)) : "memory");
}

static inline void hvx_xrstor(void *a, u64 m)
{
	asm volatile("xrstor64 (%0)" :: "r" (a), "a" ((u32)m), "d" ((u32)(m >> 32)) : "memory");
}

/* a valid guest XCR0 for the offered components (the hardware XSETBV rules) */
static bool hvx_xcr0_ok(u64 v)
{
	return (v & 1) && !(v & ~HVX_XS_MASK) && (!(v & 4) || (v & 2));
}

/* clear every header/MXCSR bit that would make XRSTOR #GP in the host */
static void hvx_xs_sanitize(u8 *a)
{
	*(u64 *)(a + HVX_XS_HDR) &= HVX_XS_MASK;	/* XSTATE_BV */
	memset(a + HVX_XS_HDR + 8, 0, 56);		/* XCOMP_BV (standard form) + reserved */
	*(u32 *)(a + 24) &= 0xffffU;			/* MXCSR reserved bits */
}

static void hvx_xs_load(struct hvx_vcpu *vc)
{
	hvx_xs_sanitize((u8 *)vc->fx_page);
	hvx_xrstor((void *)vc->fx_page, HVX_XS_MASK);	/* XSTATE_BV 0 => init */
}

static void hvx_xs_save(struct hvx_vcpu *vc)
{
	u8 *a = (u8 *)vc->fx_page;
	/* x87 and SSE registers exist whatever XCR0 says; AVX only when enabled */
	u64 rfbm = (vc->xcr0 | 3) & HVX_XS_MASK, bv;

	hvx_xsave(a, rfbm);
	bv = *(u64 *)(a + HVX_XS_HDR) & rfbm;
	if (!(bv & 1)) {				/* x87 init: FCW 0x37f, rest 0 */
		memset(a, 0, 24);
		*(u16 *)a = 0x037f;
		memset(a + 32, 0, 128);
	}
	if (!(bv & 2))
		memset(a + 160, 0, 256);		/* XMM0-15 */
	if (!(bv & 4))
		memset(a + HVX_XS_YMMH, 0, 256);	/* YMM0-15 upper halves */
	*(u64 *)(a + HVX_XS_HDR) = bv;
}

/*
 * XSETBV always VM-exits. Emulate it with the hardware checks (#UD for CR4.OSXSAVE=0
 * is raised by the CPU before the exit): #GP(0) at CPL > 0, ECX != 0, bit 0 clear,
 * AVX without SSE, or a bit outside the offered mask. When AVX becomes enabled its
 * register state is initialised (VZEROUPPER, run in the host where XCR0 includes AVX)
 * so it never carries leftovers. Returns false if a #GP was queued (RIP not advanced).
 */
static bool hvx_xsetbv(struct hvx_vcpu *vc)
{
	u64 v = (u32)vc->regs.rax | ((u64)(u32)vc->regs.rdx << 32);
	u32 cpl = ((u32)vmread(GUEST_SS_AR) >> 5) & 3;

	if (cpl || (u32)vc->regs.rcx || !hvx_xcr0_ok(v)) {
		vc->reinj_info = 0x80000000U | (3U << 8) | 0x800U | 13;	/* #GP, error code */
		vc->reinj_ec = 0;
		vc->reinj_len = 0;
		vc->reinj_valid = true;
		return false;
	}
	if ((v & 4) && !(vc->xcr0 & 4))
		asm volatile("vzeroupper" ::: "memory");
	vc->xcr0 = v;
	return true;
}

/*
 * Deterministic in-kernel CPUID (milestone 12). The guest's CPUID is configured
 * to VM-exit; rather than let the answer depend on the real host CPU (which would
 * diverge two runs and could even advertise features -- XSAVE/AVX/OSXSAVE -- that
 * make the guest run XSETBV), the module writes a FIXED synthetic result into the
 * guest GPRs. Leaf = EAX, subleaf = ECX. CPUID writes EAX/EBX/ECX/EDX, i.e. the
 * low 32 bits of RAX/RBX/RCX/RDX zero-extended (plain u32 -> u64 assignment).
 */
static void hvx_cpuid(struct hvx_vcpu *vcpu)
{
	u32 leaf = (u32)vcpu->regs.rax;
	u32 subleaf = (u32)vcpu->regs.rcx;
	u32 a = 0, b = 0, c = 0, d = 0;

	switch (leaf) {
	case 0x0:		/* max basic leaf + "GenuineIntel" vendor string */
		a = 0x00000016;	/* raise max basic leaf to 0x16 (0x15/0x16 = TSC freq) */
		b = 0x756e6547;	/* "Genu" */
		d = 0x49656e69;	/* "ineI" */
		c = 0x6c65746e;	/* "ntel" */
		break;
	case 0x1:		/* family/model/stepping + feature flags */
		a = 0x000006f1;	/* family 6 */
		/* EBX: CLFLUSH line size (bits 8..15) = 8, and the initial APIC ID
		 * (bits 24..31) = this vCPU's index, so an SMP guest can tell its
		 * CPUs apart (milestone 19; index 0 keeps the pre-M19 value). */
		b = 0x00000800 | ((u32)vcpu->index << 24);
		/* SMP boot: n logical processors per package (EBX[23:16]) + HTT */
		if (vcpu->vm->smp_boot)
			b |= (u32)vcpu->vm->n_vcpus << 16;
		c = (1U << 21) |	/* x2APIC       (M15: force MSR-mapped LAPIC) */
		    (1U << 24) |	/* TSC-Deadline (M15: LAPIC deadline timer) */
		    (1U << 31);		/* HYPERVISOR present (M15: see 0x40000000 below) */
				/* still NO XSAVE/AVX/OSXSAVE (guest never runs XSETBV) */
		d = (1U << 0)  |	/* FPU     */
		    (1U << 3)  |	/* PSE     */
		    (1U << 4)  |	/* TSC     */
		    (1U << 5)  |	/* MSR     */
		    (1U << 6)  |	/* PAE     */
		    (1U << 8)  |	/* CX8     */
		    (1U << 9)  |	/* APIC    */
		    (1U << 13) |	/* PGE     */
		    (1U << 15) |	/* CMOV    */
		    (1U << 19) |	/* CLFLUSH */
		    (1U << 24) |	/* FXSR    */
		    (1U << 25) |	/* SSE     */
		    (1U << 26);		/* SSE2    */
		if (vcpu->vm->smp_boot)
			d |= (1U << 28);	/* HTT: EBX[23:16] is valid */
		if (vcpu->vm->cpu_v2) {
			/* x86-64-v2 (M31, HVX_VM_X86_64_V2): add MMX (baseline requires it)
			 * + the v2 SSE3/SSSE3/CX16/SSE4.1/SSE4.2/POPCNT features so a real
			 * glibc userland loads. All deterministic; NO AVX/XSAVE. Opt-in, so
			 * the default CPUID (and every prior fingerprint) is unchanged. */
			c |= (1U << 0)  |	/* SSE3    */
			     (1U << 9)  |	/* SSSE3   */
			     (1U << 13) |	/* CMPXCHG16B */
			     (1U << 19) |	/* SSE4.1  */
			     (1U << 20) |	/* SSE4.2  */
			     (1U << 23);	/* POPCNT  */
			d |= (1U << 23);	/* MMX     */
		}
		if (vcpu->vm->cpu_v3) {
			c |= (1U << 12) |	/* FMA     */
			     (1U << 22) |	/* MOVBE   */
			     (1U << 26) |	/* XSAVE   */
			     (1U << 28) |	/* AVX     */
			     (1U << 29);	/* F16C    */
			if (vmread(GUEST_CR4) & X86_CR4_OSXSAVE)
				c |= (1U << 27);	/* OSXSAVE mirrors guest CR4 */
		}
		break;
	case 0x7:		/* structured extended features: none advertised */
		a = b = c = d = 0;
		if (vcpu->vm->cpu_v3 && subleaf == 0)
			b = (1U << 3) |	/* BMI1 */
			    (1U << 5) |	/* AVX2 */
			    (1U << 8);	/* BMI2 */
		break;
	case 0xd:		/* XSAVE features + sizes (HVX_VM_X86_64_V3 only) */
		if (!vcpu->vm->cpu_v3)
			break;
		if (subleaf == 0) {
			a = HVX_XS_MASK;	/* XCR0 bits supported */
			b = (vcpu->xcr0 & 4) ? HVX_XS_SIZE : HVX_XS_YMMH;	/* for current XCR0 */
			c = HVX_XS_SIZE;	/* for all supported */
		} else if (subleaf == 2) {
			a = 256;		/* AVX: size */
			b = HVX_XS_YMMH;	/* AVX: offset */
		}			/* subleaf 1: no XSAVEOPT/XSAVEC/XGETBV1/XSAVES */
		break;
	case 0xb:		/* x2APIC topology (SMP boot only; else all zero as before) */
		if (vcpu->vm->smp_boot) {
			u32 nv = vcpu->vm->n_vcpus;

			d = vcpu->index;		/* x2APIC ID */
			if (subleaf == 0) {		/* SMT level: one thread per core */
				a = 0;
				b = 1;
				c = (1U << 8) | 0;
			} else if (subleaf == 1) {	/* core level: n cores in the package */
				a = (u32)order_base_2(nv);
				b = nv;
				c = (2U << 8) | 1;
			} else {
				c = subleaf & 0xff;	/* invalid level ends the list */
			}
		}
		break;
	case 0x15:		/* TSC / core-crystal clock ratio: TSC = crystal*EBX/EAX */
		a = 1;		/* denominator */
		b = 64;		/* numerator   */
		c = 25000000;	/* core crystal Hz  => TSC = 1.6 GHz (known freq) */
		break;		/* d = 0 */
	case 0x16:		/* processor frequency (MHz); backup TSC-freq source */
		a = 1600;	/* base  MHz */
		b = 1600;	/* max   MHz */
		c = 100;	/* bus   MHz */
		break;
	/*
	 * Hypervisor CPUID leaves (M15). We advertise the KVM signature with an
	 * EMPTY feature set. The point is narrow: Linux keeps x2APIC enabled
	 * without interrupt remapping only when x86_init.hyper.x2apic_available()
	 * returns true, which on a plain "bare metal" CPUID is false -- so
	 * enable_IR_x2apic() would disable x2APIC and fall back to (unmapped)
	 * xAPIC MMIO and panic. Presenting KVM makes that hook return true. No PV
	 * features are advertised (leaf 0x40000001 EAX = 0), so no kvm-clock /
	 * PV-EOI / async-PF / steal-time path is taken -- only the x2APIC hook.
	 */
	case 0x40000000:	/* hypervisor id: max leaf + "KVMKVMKVM\0\0\0" */
		a = 0x40000001;
		b = 0x4b4d564b;	/* "KVMK" */
		c = 0x564b4d56;	/* "VMKV" */
		d = 0x0000004d;	/* "M\0\0\0" */
		break;
	case 0x40000001:	/* KVM feature flags: none */
		a = b = c = d = 0;
		break;
	case 0x80000000:	/* max extended leaf */
		a = 0x80000001;
		break;
	case 0x80000001:	/* extended feature flags */
		d = (1U << 11) |	/* SYSCALL */
		    (1U << 20) |	/* NX      */
		    (1U << 29);		/* LM (long mode) */
		if (vcpu->vm->cpu_v2)
			c |= (1U << 0);	/* LAHF/SAHF in 64-bit (x86-64-v2, M31) */
		if (vcpu->vm->cpu_v3)
			c |= (1U << 5);	/* LZCNT (ABM) */
		break;
	default:		/* every other leaf/subleaf: all zero */
		break;
	}
	vcpu->regs.rax = a;
	vcpu->regs.rbx = b;
	vcpu->regs.rcx = c;
	vcpu->regs.rdx = d;
}

/*
 * x2APIC Local APIC emulation (milestone 15). In x2APIC mode the entire LAPIC
 * register file is MSR-mapped at 0x800..0x83f, plus IA32_APIC_BASE (0x1b) and
 * IA32_TSC_DEADLINE (0x6e0). Everything is read-as-written except the handful of
 * registers with real semantics: ID/VERSION/PPR/LDR/CCR are computed read-only,
 * EOI/ESR clear, and ICR/SELF-IPI schedule a self-interrupt via the M8 path.
 * hvx_lapic_rdmsr/hvx_lapic_wrmsr return true iff the MSR belongs to the LAPIC.
 */
static bool hvx_lapic_rdmsr(struct hvx_vcpu *vcpu, u32 idx, u64 *out)
{
	u32 v;

	if (idx == HVX_MSR_APICBASE) { *out = vcpu->lapic.apic_base; return true; }
	if (idx == HVX_MSR_TSC_DEADLINE) { *out = vcpu->lapic.tsc_deadline; return true; }
	if (idx < X2APIC_BASE_MSR || idx > X2APIC_TOP_MSR)
		return false;

	switch (idx) {
	case X2APIC_ID:      v = vcpu->index; break;		/* this vCPU's x2APIC ID */
	case X2APIC_VERSION: v = 0x00050014; break;		/* ver 0x14, max-LVT 5 */
	case X2APIC_PPR:     v = 0; break;
	case X2APIC_LDR:					/* x2APIC logical dest for this ID */
		v = ((vcpu->index >> 4) << 16) | (1u << (vcpu->index & 0xf));
		break;
	case X2APIC_CCR:     v = 0; break;			/* timer current count */
	default:
		if (idx >= 0x810 && idx <= 0x827)		/* ISR / TMR / IRR */
			v = 0;
		else
			v = vcpu->lapic.reg[idx - X2APIC_BASE_MSR];
		break;
	}
	*out = v;
	return true;
}

/* Latch a one-shot interrupt on a TARGET vCPU, reusing the M8 instruction-timed
 * injection slot. For a cross-vCPU IPI the target is not the running vCPU, so its
 * instruction clock is frozen; scheduling at its CURRENT icount means "deliver on
 * the target's next entry" -- a deterministic point given the fixed round-robin.
 * For a self-IPI it is due on the next VMRESUME within the running slice. */
/* Post a one-shot interrupt to a vCPU: into the inject_* slot if it is free (or
 * already holds this vector -- a second edge of a pending vector coalesces, as in
 * a real IRR), otherwise into the overflow IRR, from which the slot is refilled,
 * highest vector first, once it has been delivered. */
/*
 * "Now" on the instruction clock for an interrupt posted to t. With the in-kernel
 * scheduler the VM has one clock and only the running vCPU (sched.cur) has the live
 * copy -- the target's copy is stale by an amount that depends on where the last
 * ioctl returned, so it must not be used.
 */
static inline u64 hvx_post_icount(const struct hvx_vcpu *t)
{
	const struct hvx_vm *vm = t->vm;

	return vm->sched_on ? vm->vcpu[vm->sched.cur].icount : t->icount;
}

static void hvx_oneshot_post(struct hvx_vcpu *t, u32 vector)
{
	vector &= 0xff;
	if (t->inject_valid && t->inject_vector != vector) {
		__set_bit(vector, t->irr);
		return;
	}
	t->inject_vector = vector;
	t->inject_at_icount = hvx_post_icount(t);
	t->inject_valid = 1;
}
static void hvx_oneshot_refill(struct hvx_vcpu *t)
{
	int v;

	if (t->inject_valid)
		return;
	for (v = 255; v >= 0; v--)
		if (test_bit(v, t->irr)) {
			__clear_bit(v, t->irr);
			t->inject_vector = v;
			t->inject_at_icount = hvx_post_icount(t);
			t->inject_valid = 1;
			return;
		}
}

static void hvx_lapic_deliver(struct hvx_vcpu *t, u32 vector)
{
	hvx_oneshot_post(t, vector);		/* due at the target's next entry */
}

/*
 * Route a fixed / lowest-priority IPI from `src` per the x2APIC ICR destination
 * (milestone 19). Physical destination mode: `dest` is the target x2APIC ID (==
 * vCPU index). Shorthands: 1 = self, 2 = all-including-self, 3 = all-excluding-
 * self. INIT/SIPI/NMI/SMI delivery modes are NOT modeled (there is no hardware AP
 * bringup; the SMP test starts each vCPU directly at its entry point).
 */
static void hvx_lapic_send_ipi(struct hvx_vcpu *src, u32 dsh, u32 dest, u32 vector)
{
	struct hvx_vm *vm = src->vm;
	unsigned int i;

	for (i = 0; i < vm->n_vcpus; i++) {
		struct hvx_vcpu *t = &vm->vcpu[i];
		bool hit;

		switch (dsh) {
		case 1:  hit = (t == src);         break;	/* self */
		case 2:  hit = true;               break;	/* all including self */
		case 3:  hit = (t != src);         break;	/* all excluding self */
		default: hit = (t->index == dest); break;	/* physical destination */
		}
		if (hit)
			hvx_lapic_deliver(t, vector);
	}
}

/*
 * AP start-up (HVX_VM_SMP_BOOT), as on hardware and in KVM. INIT puts the target in
 * wait-for-SIPI: it does not run, its local APIC is reset (ID and IA32_APIC_BASE kept)
 * and pending interrupts are dropped. A STARTUP IPI with vector V, to a vCPU in
 * wait-for-SIPI, starts it in real mode at CS = V<<8, IP = 0 (base V<<12); a STARTUP to
 * a vCPU that is already running is ignored, so Linux's second SIPI is harmless. Both
 * only change the target's state; it runs at its next turn (the scheduler's round-robin
 * is the only thing that decides when). A vCPU never INITs itself.
 */
static void hvx_fx_init(u8 *fx);

static void hvx_ap_init(struct hvx_vcpu *t)
{
	t->halted = HVX_HALT_SIPI;
	t->halt_len = 0;
	memset(t->lapic.reg, 0, sizeof(t->lapic.reg));
	t->lapic.tsc_deadline = 0;
	t->lapic.in_service = 0;
	t->inject_valid = 0;
	t->inject_vector = 0;
	t->inject_at_icount = 0;
	memset(t->irr, 0, sizeof(t->irr));
	t->reinj_valid = false;
}

static void hvx_ap_sipi(struct hvx_vcpu *t, u32 vector)
{
	struct hvx_sregs *s = &t->sregs;

	if (t->halted != HVX_HALT_SIPI)
		return;
	memset(&t->regs, 0, sizeof(t->regs));
	t->regs.rip = 0;
	t->regs.rflags = 0x2;
	t->regs.rdx = 0x000006f1;		/* processor signature, as after INIT (CPUID 1 EAX) */
	memset(s, 0, sizeof(*s));
	s->cr0 = 0x10;				/* ET; CD/NW clear (as KVM); NE is added by fixed_cr */
	s->dr7 = 0x400;
	s->cs.selector = (u16)(vector << 8);
	s->cs.base = (u64)vector << 12;
	s->cs.limit = 0xffff;
	s->cs.ar = 0x9b;			/* present code, read, accessed */
	s->ss.limit = 0xffff;
	s->ss.ar = 0x93;			/* present data, write, accessed */
	s->ds = s->ss; s->es = s->ss; s->fs = s->ss; s->gs = s->ss;
	s->tr.limit = 0xffff;
	s->tr.ar = 0x8b;			/* busy 32-bit TSS (VM entry needs a busy TSS type) */
	s->gdt_limit = 0xffff;
	s->idt_limit = 0xffff;
	memset(t->sw_msr, 0, sizeof(t->sw_msr));
	hvx_fx_init((u8 *)t->fx_page);
	if (t->vm->cpu_v3)	/* guest AVX: XSAVE header + YMM upper halves to init (XSTATE_BV 0); XCR0 kept, as INIT keeps it */
		memset((u8 *)t->fx_page + 512, 0, 320);
	t->cr2 = 0;
	t->halted = HVX_HALT_RUN;
}

static void hvx_lapic_init_sipi(struct hvx_vcpu *src, u64 val)
{
	struct hvx_vm *vm = src->vm;
	u32 dm = (u32)(val >> 8) & 7, dsh = (u32)(val >> 18) & 3, dest = (u32)(val >> 32);
	unsigned int i;

	if (dm == 5 && !(val & (1U << 14)))
		return;				/* INIT level de-assert: no effect */
	for (i = 0; i < vm->n_vcpus; i++) {
		struct hvx_vcpu *t = &vm->vcpu[i];
		bool hit;

		switch (dsh) {
		case 1:  hit = false;              break;	/* self: never */
		case 2:
		case 3:  hit = (t != src);         break;	/* all (a CPU does not INIT/SIPI itself) */
		default: hit = (t->index == dest && t != src); break;
		}
		if (!hit)
			continue;
		if (dm == 5)
			hvx_ap_init(t);
		else
			hvx_ap_sipi(t, (u32)val & 0xff);
	}
}

static bool hvx_lapic_wrmsr(struct hvx_vcpu *vcpu, u32 idx, u64 val)
{
	if (idx == HVX_MSR_APICBASE) {
		/* Keep x2APIC latched on (EXTD|EN) regardless of what Linux writes,
		 * so it never falls back to xAPIC MMIO. Only vCPU 0 is the boot CPU. */
		vcpu->lapic.apic_base = vcpu->index ? (APICBASE_DEFAULT & ~APICBASE_BSP)
						    : APICBASE_DEFAULT;
		return true;
	}
	if (idx == HVX_MSR_TSC_DEADLINE) {
		vcpu->lapic.tsc_deadline = val;	/* 0 disarms; nonzero arms the timer */
		return true;
	}
	if (idx < X2APIC_BASE_MSR || idx > X2APIC_TOP_MSR)
		return false;

	switch (idx) {
	case X2APIC_EOI:				/* end-of-interrupt: release the in-service slot */
		vcpu->lapic.in_service = 0;
		break;
	case X2APIC_ESR:				/* write clears the error status */
		vcpu->lapic.reg[idx - X2APIC_BASE_MSR] = 0;
		break;
	case X2APIC_ICR: {
		u32 dm = (u32)(val >> 8) & 7;		/* delivery mode */

		vcpu->lapic.reg[idx - X2APIC_BASE_MSR] = (u32)val;
		if (dm == 0 || dm == 1) {		/* fixed / lowest-priority */
			if (vcpu->vm->n_vcpus == 1) {
				/* Preserve the exact pre-M19 UP behavior: a fixed/lowest-
				 * priority ICR write self-injects, regardless of the
				 * destination/shorthand fields (there is only one CPU).
				 * This keeps the M16/M18 boot bit-identical. */
				hvx_lapic_deliver(vcpu, (u32)val & 0xff);
			} else {
				u32 dsh  = (u32)(val >> 18) & 3;	/* destination shorthand */
				u32 dest = (u32)(val >> 32);	/* physical destination ID */

				hvx_lapic_send_ipi(vcpu, dsh, dest, (u32)val & 0xff);
			}
		} else if ((dm == 5 || dm == 6) && vcpu->vm->smp_boot)
			hvx_lapic_init_sipi(vcpu, val);	/* AP start-up (SMP boot) */
		/* otherwise dm 4/5/6 = NMI/INIT/SIPI are dropped */
		break;
	}
	case X2APIC_SELF_IPI:
		hvx_lapic_deliver(vcpu, (u32)val & 0xff);
		break;
	default:
		vcpu->lapic.reg[idx - X2APIC_BASE_MSR] = (u32)val;
		break;
	}
	return true;
}

/*
 * Deterministic in-kernel RDMSR (milestone 12). ECX = MSR index; result goes in
 * EDX:EAX (zero-extended into RDX:RAX in 64-bit mode). IA32_EFER reads back the
 * VMCS guest field, IA32_TSC returns the current virtual TSC (matching what RDTSC
 * would yield in this tsc_mode), the curated store returns its slot, and any
 * unknown MSR reads 0 (we deliberately do not inject #GP).
 */
static void hvx_rdmsr(struct hvx_vcpu *vcpu)
{
	u32 idx = (u32)vcpu->regs.rcx;
	u64 val;
	int slot;

	if (hvx_lapic_rdmsr(vcpu, idx, &val)) {
		/* x2APIC register / IA32_APIC_BASE / IA32_TSC_DEADLINE (M15) */
	} else if (idx == GMSR_EFER) {
		val = vmread(GUEST_IA32_EFER);
	} else if (idx == GMSR_FS_BASE) {
		/* FS/GS base are BOTH MSRs and VMCS guest fields -- read the VMCS so
		 * the answer reflects what the guest's segment base actually is (M14). */
		val = vmread(GUEST_FS_BASE_F);
	} else if (idx == GMSR_GS_BASE) {
		val = vmread(GUEST_GS_BASE_F);
	} else if (idx == GMSR_IA32_TSC) {
		val = (vcpu->tsc_mode == HVX_TSC_ICOUNT)
			? vcpu->tsc + vcpu->icount * vcpu->tsc_step
			: vcpu->tsc;
	} else if (idx == GMSR_KERNEL_GS_BASE) {
		/* Live in the physical MSR during the run; a guest swapgs mutates it,
		 * so read it back rather than the (stale) sw_msr copy (M15). */
		val = rd(GMSR_KERNEL_GS_BASE);
	} else if ((slot = swmsr_slot(idx)) >= 0) {
		val = vcpu->sw_msr[slot];
	} else {
		val = 0;
	}

	vcpu->regs.rax = (u32)val;
	vcpu->regs.rdx = (u32)(val >> 32);
}

/*
 * Deterministic in-kernel WRMSR (milestone 12). ECX = MSR index; value is
 * EDX:EAX. IA32_EFER writes the VMCS guest field (single-run; cross-run
 * persistence is Phase 4, since setup_vmcs re-seeds EFER each entry), IA32_TSC
 * re-bases the virtual TSC, the curated store keeps the value, and any unknown
 * MSR is silently dropped (no #GP) to keep a booting guest safe.
 */
static void hvx_wrmsr(struct hvx_vcpu *vcpu)
{
	u32 idx = (u32)vcpu->regs.rcx;
	u64 val = ((u64)(u32)vcpu->regs.rdx << 32) | (u32)vcpu->regs.rax;
	int slot;

	if (hvx_lapic_wrmsr(vcpu, idx, val)) {
		/* x2APIC register / IA32_APIC_BASE / IA32_TSC_DEADLINE (M15) */
	} else if (idx == GMSR_EFER) {
		VMW(GUEST_IA32_EFER, val);
	} else if (idx == GMSR_FS_BASE) {
		/* Reflect a write of IA32_FS_BASE/IA32_GS_BASE into the VMCS guest
		 * segment-base field so the guest's actual FS/GS base changes (M14).
		 * Linux's head_64.S sets GS base this way just before calling
		 * x86_64_start_kernel, which then loads the stack canary from %gs:0x28;
		 * without this the base stays 0 and that load triple-faults. save_sregs
		 * reads these fields back into vm->sregs each exit, so the base persists
		 * across HVX_RUN re-entries (and into the snapshot). */
		VMW(GUEST_FS_BASE_F, val);
	} else if (idx == GMSR_GS_BASE) {
		VMW(GUEST_GS_BASE_F, val);
	} else if (idx == GMSR_IA32_TSC) {
		vcpu->tsc = val;
	} else if ((slot = swmsr_slot(idx)) >= 0) {
		vcpu->sw_msr[slot] = val;
		/* STAR/LSTAR/CSTAR/SFMASK/KERNEL_GS_BASE are also loaded into the
		 * physical MSRs around the guest run (SYSCALL/swapgs use them without
		 * a VM-exit). Keep the hardware coherent so a write during a run takes
		 * effect for a later SYSCALL/swapgs in the same run (M15). */
		if (idx == GMSR_STAR || idx == GMSR_LSTAR || idx == GMSR_CSTAR ||
		    idx == GMSR_SFMASK || idx == GMSR_KERNEL_GS_BASE)
			wrmsrl_safe(idx, val);	/* safe: guest value may be non-canonical */
	}
	/* unknown MSR: ignore the write */
}

/* ---- serial input ring (milestone 18) ----
 * Fed from userspace (HVX_PUT_INPUT), drained by a guest RBR read. Accessed only
 * while the VM's mutex is held: uart_rx_push runs from the ioctl path (outside a
 * run), pop/pending from the in-kernel run-loop (inside a run) -- HVX_RUN holds
 * the mutex for its whole duration, so push and pop never overlap; no locking. */
static inline bool uart_rx_pending(struct hvx_vm *vm)
{
	return vm->input_tail != vm->input_head;
}
static inline u8 uart_rx_pop(struct hvx_vm *vm)
{
	u8 b = vm->input_buf[vm->input_head & (HVX_INPUT_SZ - 1)];

	vm->input_head++;
	return b;
}
static inline void uart_rx_push(struct hvx_vm *vm, u8 b)
{
	if (vm->input_tail - vm->input_head >= HVX_INPUT_SZ)
		return;			/* ring full: drop (host fed faster than guest reads) */
	vm->input_buf[vm->input_tail & (HVX_INPUT_SZ - 1)] = b;
	vm->input_tail++;
}

/* ---- synchronous fuzz-input port (Phase 3) ----
 * Set from userspace (HVX_SET_FUZZ_INPUT), pulled by a guest IN on the data/status ports from the
 * in-kernel run-loop. Same locking argument as the serial ring: HVX_RUN holds the mutex, and the
 * ioctl runs outside a run, so the set and the pulls never overlap. Purely synchronous: each guest
 * IN is a VM-exit the module services and returns from at once (no interrupt, no blocking read),
 * so HVX_RUN keeps returning and the idle heartbeat can stay on for the workload. */
static inline bool fuzz_pending(struct hvx_vm *vm)
{
	return vm->fuzz_off < vm->fuzz_len;
}
static inline u8 fuzz_pop(struct hvx_vm *vm)
{
	if (vm->fuzz_off >= vm->fuzz_len)
		return 0;		/* drained: reads return 0 (inert when nothing staged) */
	return vm->fuzz_buf[vm->fuzz_off++];
}

/* ==== virtio-blk core (transport-agnostic; milestone 20) =================== *
 * One split virtqueue serviced against an in-memory backing image, with the
 * completion interrupt delivered at a DETERMINISTIC instruction (kick_icount +
 * HVX_VBLK_LATENCY) through the M8 injection path. Two transports feed this same
 * core -- the Phase-A test PIO doorbell (below) and the Phase-B PCI/MMIO
 * transport (further down) -- so the novel determinism lives in ONE place. All
 * of this is inert unless a disk is attached (d->disk_pages != NULL).
 * ========================================================================== */

/* one split-virtqueue descriptor (virtio 1.0 §2.7.5), 16 bytes */
struct virtq_desc { u64 addr; u32 len; u16 flags; u16 next; } __packed;

/* ==== deterministic fault injection (milestone 22) ======================== *
 * A seeded policy perturbs virtio-blk I/O so any bug it surfaces replays bit-for-
 * bit (as in FoundationDB's simulation). Every decision is a PURE FUNCTION of
 * (fault_seed, request index) -- never host time -- and the policy + PRNG counters
 * ride in the snapshot, so branch/replay reproduces the identical schedule. Inert
 * (early-out, no state touched) when fault_classes == 0, so the disabled path is
 * byte-identical to M21. The mixing function + keying are documented in the ABI so
 * the userspace test can predict the exact schedule.
 * ========================================================================== */
#define HVX_FAULT_REORDER_MAX 16	/* cap on requests reordered per kick (test uses <=8) */

enum vblk_fault { VBF_NONE = 0, VBF_DELAY, VBF_ERROR, VBF_DROP, VBF_TORN, VBF_DROP_UNFLUSH };

/* SplitMix64 finalizer: a strong integer hash; pure, trivially replicated in the
 * userspace test (same constants as hvx_abi.h's GOLDEN). */
static inline u64 fault_mix(u64 x)
{
	x += 0x9e3779b97f4a7c15ULL;
	x = (x ^ (x >> 30)) * 0xbf58476d1ce4e5b9ULL;
	x = (x ^ (x >> 27)) * 0x94d049bb133111ebULL;
	return x ^ (x >> 31);
}

/* Per-request 64-bit roll: a pure function of (seed, request index). */
static inline u64 fault_roll(u64 seed, u64 req_index)
{
	return fault_mix(seed + (req_index + 1) * 0x9e3779b97f4a7c15ULL);
}

/*
 * Decide the fault for one request given its type and per-device index. Pure
 * function of (fault_seed, req_index, type); advances no state (the caller owns
 * the fault_req_seq counter). Returns the class, and for DELAY the extra
 * completion latency in *extra (in instructions). The applicable-class list is
 * built in the FIXED order DELAY, ERROR, DROP, TORN (DROP/TORN writes only), and
 * the class is selected by ((h>>8) % n) -- documented so the test replicates it.
 */
static enum vblk_fault vblk_fault_decide(struct hvx_vblk *d, u64 req_index,
					 u32 type, u32 *extra)
{
	u64 h;
	u32 pct;
	enum vblk_fault cand[5];
	int n = 0, sel;

	*extra = 0;
	if (d->fault_classes == 0)
		return VBF_NONE;
	h = fault_roll(d->fault_seed, req_index);
	pct = (u32)(h % 100);
	if (pct >= d->fault_probability)
		return VBF_NONE;
	if (d->fault_classes & HVX_FAULT_DELAY)
		cand[n++] = VBF_DELAY;
	if (d->fault_classes & HVX_FAULT_ERROR)
		cand[n++] = VBF_ERROR;
	if ((d->fault_classes & HVX_FAULT_DROP) && type == VIRTIO_BLK_T_OUT)
		cand[n++] = VBF_DROP;
	if ((d->fault_classes & HVX_FAULT_TORN) && type == VIRTIO_BLK_T_OUT)
		cand[n++] = VBF_TORN;
	/* M26: flush-aware DROP appended LAST so it never shifts the pre-M26 selection
	 * indices (prior tests never enable it, so their candidate list is unchanged). */
	if ((d->fault_classes & HVX_FAULT_DROP_UNFLUSHED) && type == VIRTIO_BLK_T_OUT)
		cand[n++] = VBF_DROP_UNFLUSH;
	if (n == 0)
		return VBF_NONE;	/* only write-only classes enabled, but this is a read */
	sel = (int)((h >> 8) % (u64)n);
	if (cand[sel] == VBF_DELAY) {
		u32 md = d->fault_max_delay ? d->fault_max_delay : 1;

		*extra = 1 + (u32)((h >> 16) % md);
	}
	return cand[sel];
}

/* Copy `len` bytes between guest-physical [gpa, gpa+len) and a host buffer, page
 * by page (guest RAM pages are individually allocated, non-contiguous in host
 * VA). Bounds-checked against guest RAM; returns false on an out-of-range range. */
static bool hvx_guest_rw(struct hvx_vm *vm, u64 gpa, void *buf, u64 len, bool to_guest)
{
	u64 low = (u64)vm->n_low_pages * PAGE_SIZE;
	u64 high = (u64)(vm->n_guest_pages - vm->n_low_pages) * PAGE_SIZE;
	u64 hb = HVX_RAM_HIGH_GFN << PAGE_SHIFT;
	u8 *b = buf;

	if (len == 0)
		return true;
	/* the range lies in one block: the hole between them is not RAM (M179) */
	if (!(gpa < low && len <= low && gpa + len <= low) &&
	    !(gpa >= hb && len <= high && gpa - hb + len <= high))
		return false;
	while (len) {
		u64 off = gpa & (PAGE_SIZE - 1);
		u64 chunk = PAGE_SIZE - off;
		void *host = gpa_host(vm, gpa);

		if (chunk > len)
			chunk = len;
		if (to_guest) {
			memcpy(host, b, chunk);
			if (vm->dl_ram)		/* dirty log (M138): a module write into the guest */
				__set_bit(hvx_gfn_ram(vm, gpa >> PAGE_SHIFT), vm->dl_ram);
		} else {
			memcpy(b, host, chunk);
		}
		gpa += chunk; b += chunk; len -= chunk;
	}
	return true;
}

/* Write `len` bytes from a contiguous host buffer into the page-array disk image at
 * [doff, doff+len) (milestone 28). Caller bounds-checks against disk_size. */
static void disk_write_bytes(struct hvx_vblk *d, u64 doff, const void *src, u64 len)
{
	const u8 *b = src;

	while (len) {
		unsigned long pg = doff >> PAGE_SHIFT;
		u64 off = doff & (PAGE_SIZE - 1);
		u64 chunk = PAGE_SIZE - off;

		if (chunk > len)
			chunk = len;
		memcpy((u8 *)d->disk_pages[pg] + off, b, chunk);
		if (d->dl && pg < d->dl_pages)	/* dirty log (M138) */
			__set_bit(pg, d->dl);
		doff += chunk; b += chunk; len -= chunk;
	}
}

/* Copy between the page-array backing disk image and guest RAM (both sides are
 * non-contiguous page arrays; milestone 28). Split the disk range at disk-page
 * boundaries, delegating each fragment to hvx_guest_rw (which further splits at
 * guest-page boundaries). Byte-for-byte identical to the pre-M28 contiguous copy. */
static bool vblk_disk_rw(struct hvx_vm *vm, u64 gpa, u64 doff, u32 len, bool disk_to_guest)
{
	struct hvx_vblk *d = &vm->vblk;
	u64 remaining = len;

	if (!d->disk_pages || doff > d->disk_size || (u64)doff + len > d->disk_size)
		return false;
	while (remaining) {
		unsigned long pg = doff >> PAGE_SHIFT;
		u64 off = doff & (PAGE_SIZE - 1);
		u64 chunk = PAGE_SIZE - off;

		if (chunk > remaining)
			chunk = remaining;
		if (!hvx_guest_rw(vm, gpa, (u8 *)d->disk_pages[pg] + off, chunk, disk_to_guest))
			return false;
		if (!disk_to_guest && d->dl && pg < d->dl_pages)	/* dirty log (M138) */
			__set_bit(pg, d->dl);
		gpa += chunk; doff += chunk; remaining -= chunk;
	}
	return true;
}

/* ---- flush-aware DROP: the volatile write cache (milestone 26) ----
 * A WRITE rolled for HVX_FAULT_DROP_UNFLUSHED is HELD here (not persisted) until a
 * FLUSH commits it. Faithful power-loss model: an un-flushed write is lost, a flushed
 * one is durable. All entries are a pure function of the deterministic guest writes +
 * the seeded fault schedule, applied in insertion order, so the result is
 * deterministic. Every helper is inert (unsync_n stays 0) unless the class is used, so
 * the disabled path is byte-identical to M25. */
static inline void vblk_unsync_clear(struct hvx_vblk *d)
{
	d->unsync_n = 0;
}

/* Stash `len` guest bytes at [gpa..) as an un-flushed write covering disk offset doff.
 * Returns true if held (caller must NOT persist), false to fall back to a normal
 * persist (too big / table full / alloc failed) -- a safe, honest degradation. */
static bool vblk_unsync_stash(struct hvx_vm *vm, u64 doff, u64 gpa, u32 len)
{
	struct hvx_vblk *d = &vm->vblk;
	u8 *slot;

	/* unsync_data is allocated eagerly by HVX_SET_FAULT (GFP_KERNEL, OUTSIDE this
	 * IRQs-off/FPU-disabled run-loop). If it is absent or the write is too big / the
	 * table is full, fall back to a normal persist -- never allocate here. */
	if (!d->unsync_data || len == 0 || len > HVX_UNSYNC_ENTBYTES || d->unsync_n >= HVX_UNSYNC_MAX)
		return false;
	if (doff > d->disk_size || (u64)doff + len > d->disk_size)
		return false;
	slot = d->unsync_data + (size_t)d->unsync_n * HVX_UNSYNC_ENTBYTES;
	if (!hvx_guest_rw(vm, gpa, slot, len, false))	/* copy guest write data into the cache */
		return false;
	d->unsync[d->unsync_n].doff = doff;
	d->unsync[d->unsync_n].len = len;
	d->unsync[d->unsync_n].valid = 1;
	d->unsync_n++;
	return true;
}

/* Read-your-writes: overlay any held (un-flushed) bytes that intersect a READ of
 * [doff..doff+len) onto the guest buffer already filled from the backing image. */
static void vblk_unsync_overlay(struct hvx_vm *vm, u64 gpa, u64 doff, u32 len)
{
	struct hvx_vblk *d = &vm->vblk;
	u32 i;

	if (d->unsync_n == 0)
		return;
	for (i = 0; i < d->unsync_n; i++) {		/* insertion order: later writes win */
		u64 es = d->unsync[i].doff, ee = es + d->unsync[i].len;
		u64 rs = doff, re = doff + len, os, oe;

		if (!d->unsync[i].valid)
			continue;
		os = es > rs ? es : rs;
		oe = ee < re ? ee : re;
		if (os >= oe)
			continue;			/* no overlap */
		hvx_guest_rw(vm, gpa + (os - rs),
			     d->unsync_data + (size_t)i * HVX_UNSYNC_ENTBYTES + (os - es),
			     (u32)(oe - os), true);
	}
}

/* A newer PERSISTED write to a range supersedes any held (un-flushed) bytes there:
 * invalidate overlapping cache entries so reads/commit never resurrect stale data. */
static void vblk_unsync_invalidate(struct hvx_vblk *d, u64 doff, u32 len)
{
	u32 i;

	if (d->unsync_n == 0)
		return;
	for (i = 0; i < d->unsync_n; i++) {
		u64 es = d->unsync[i].doff, ee = es + d->unsync[i].len;

		if (d->unsync[i].valid && doff < ee && es < doff + len)
			d->unsync[i].valid = 0;
	}
}

/* A FLUSH makes the volatile cache durable: write every held entry (in order) into the
 * backing image, then clear the cache. */
static void vblk_unsync_commit(struct hvx_vm *vm)
{
	struct hvx_vblk *d = &vm->vblk;
	u32 i;

	for (i = 0; i < d->unsync_n; i++) {
		if (!d->unsync[i].valid || !d->disk_pages)
			continue;
		/* overflow-safe: doff comes from HVX_SET_UNSYNC and may be near 2^64 (M97) */
		if (d->unsync[i].doff <= d->disk_size &&
		    d->unsync[i].len <= d->disk_size - d->unsync[i].doff)
			disk_write_bytes(d, d->unsync[i].doff,
			       d->unsync_data + (size_t)i * HVX_UNSYNC_ENTBYTES,
			       d->unsync[i].len);
	}
	vblk_unsync_clear(d);
}

/*
 * Process every available request on the virtqueue -- the deterministic core.
 * Runs synchronously at a kick (NOTIFY), so it is a pure function of guest RAM +
 * the disk image. For each chain [header][data...][status]: service the read or
 * write against the disk, write the status byte and the used-ring entry, advance
 * the cursors. Then raise the ISR status and SCHEDULE the completion interrupt at
 * icount + HVX_VBLK_LATENCY -- a deterministic instruction, delivered later via
 * the M8 injection path (see the run-loop). No host-nondeterministic input.
 */
/* HVX_SET_FLUSHCUT: the request starting at `head` -- its type, and whether it touches
 * the power domain [fc_lo, fc_hi) (a FLUSH always does) */
/*
 * Work bounds for a virtqueue kick (M97). The device code runs with IRQs off in VMX
 * root, and the guest controls queue_size (any u32), the avail index and the
 * descriptor `next` links, so a looping chain could hold the host CPU for minutes.
 * A virtio queue has at most 32768 entries, so a chain is at most that long, and a
 * driver can have at most queue_size descriptors outstanding, so a kick of a valid
 * driver reads at most 32768 descriptors. Valid guests never reach these bounds.
 */
#define HVX_VQ_MAX_SIZE		32768U
#define HVX_VQ_KICK_BUDGET	(4U * HVX_VQ_MAX_SIZE)	/* descriptor reads per kick */
static inline int hvx_vq_chain_max(u32 qsize)
{
	return (int)(qsize < HVX_VQ_MAX_SIZE ? qsize : HVX_VQ_MAX_SIZE) + 2;
}

static u32 vblk_peek(struct hvx_vm *vm, struct hvx_vblk *d, u16 head, bool *in_domain,
		     u32 *walked)
{
	struct virtq_desc desc;
	u8 hdr[16];
	u32 type = 0;
	u64 sector = 0, len = 0;
	u16 di = head;
	int n;

	*in_domain = true;
	(*walked)++;
	if (!hvx_guest_rw(vm, d->desc_gpa + (u64)di * 16, &desc, 16, false) ||
	    !hvx_guest_rw(vm, desc.addr, hdr, 16, false))
		return 0;
	memcpy(&type, &hdr[0], 4);
	memcpy(&sector, &hdr[8], 8);
	if (type == VIRTIO_BLK_T_FLUSH || !d->fc_hi)
		return type;
	for (n = 0; n < hvx_vq_chain_max(d->qsize) && (desc.flags & VIRTQ_DESC_F_NEXT); n++) {
		di = desc.next;
		(*walked)++;
		if (!hvx_guest_rw(vm, d->desc_gpa + (u64)di * 16, &desc, 16, false))
			break;
		if (desc.flags & VIRTQ_DESC_F_NEXT)	/* a data descriptor (the last is the status) */
			len += desc.len;
	}
	*in_domain = sector * HVX_VBLK_SECTOR < d->fc_hi && sector * HVX_VBLK_SECTOR + len > d->fc_lo;
	return type;
}

static void virtio_blk_process(struct hvx_vcpu *vcpu)
{
	struct hvx_vm *vm = vcpu->vm;
	struct hvx_vblk *d = &vm->vblk;
	u16 avail_idx;
	bool any = false;
	u32 kick_extra = 0;		/* M22: summed DELAY latency for this kick */
	bool do_reorder = (d->fault_classes & HVX_FAULT_REORDER) != 0;	/* M22 */
	struct { u16 head; u32 used_len; } pend[HVX_FAULT_REORDER_MAX];	/* M22 reorder buffer */
	int n_pend = 0;
	u32 walked = 0;			/* descriptors read this kick (M97 bound) */

	if (!d->disk_pages || !d->qready || d->qsize == 0)
		return;
	if (!hvx_guest_rw(vm, d->avail_gpa + 2, &avail_idx, 2, false))
		return;

	/* a kick that would read more than HVX_VQ_KICK_BUDGET descriptors (no valid
	 * driver does) leaves the rest of the avail ring for the next kick */
	while ((u16)d->last_avail != avail_idx && walked < HVX_VQ_KICK_BUDGET) {
		u16 slot = (u16)((u32)d->last_avail % d->qsize);
		u16 head, di;
		u32 type = 0;
		u64 sector = 0, doff = 0;
		u8 status = VIRTIO_BLK_S_OK;
		u32 used_len = 0;
		u64 status_gpa = 0;
		bool have_status = false;
		bool first = true;
		int guardn;
		enum vblk_fault fclass = VBF_NONE;	/* M22: decided after the header parse */
		u32 extra = 0;

		if (!hvx_guest_rw(vm, d->avail_gpa + 4 + 2 * slot, &head, 2, false))
			break;
		if (d->fc_on) {		/* HVX_SET_FLUSHCUT */
			bool dom;
			u32 t = vblk_peek(vm, d, head, &dom, &walked);

			if (!d->fc_off && t == VIRTIO_BLK_T_FLUSH) {
				if (d->fc_target && d->fc_count + 1 == d->fc_target)
					d->fc_off = 1;	/* lose power BEFORE this flush commits */
				else
					d->fc_count++;
			}
			if (d->fc_off && dom) {
				/* dark: consume the request but never complete or persist it */
				d->fc_hung++;
				d->last_avail++;
				continue;
			}
		}
		di = head;
		for (guardn = 0; guardn < hvx_vq_chain_max(d->qsize); guardn++) {
			struct virtq_desc desc;
			bool has_next;

			walked++;
			if (!hvx_guest_rw(vm, d->desc_gpa + (u64)di * 16, &desc, 16, false))
				break;
			has_next = desc.flags & VIRTQ_DESC_F_NEXT;

			if (first) {			/* request header {type, reserved, sector} */
				u8 hdr[16];

				if (hvx_guest_rw(vm, desc.addr, hdr, 16, false)) {
					memcpy(&type, &hdr[0], 4);
					memcpy(&sector, &hdr[8], 8);
					doff = sector * HVX_VBLK_SECTOR;
				}
				/* M22: decide this request's fault now that its type is known.
				 * The per-request PRNG key (fault_req_seq) advances ONLY when
				 * faults are enabled, so the disabled path never touches state
				 * and stays byte-identical to M21. */
				if (d->fault_classes) {
					fclass = vblk_fault_decide(d, d->fault_req_seq, type, &extra);
					d->fault_req_seq++;
					if (fclass != VBF_NONE)
						d->fault_count++;
					if (fclass == VBF_ERROR)
						status = VIRTIO_BLK_S_IOERR;
					else if (fclass == VBF_DELAY)
						kick_extra += extra;
				}
				/* M26 flush-aware DROP: a successful FLUSH commits the volatile
				 * write cache to stable media (a FLUSH carries no data descriptor,
				 * so handle it here at the header). Inert when the cache is empty. */
				if (type == VIRTIO_BLK_T_FLUSH && fclass != VBF_ERROR)
					vblk_unsync_commit(vm);
			} else if (!has_next) {		/* final descriptor: the 1-byte status */
				status_gpa = desc.addr;
				have_status = true;
			} else {			/* data descriptor(s) */
				bool ok = true;

				if (fclass == VBF_ERROR) {
					/* I/O error: transfer no data (used_len stays status-only) */
					status = VIRTIO_BLK_S_IOERR;
				} else if (type == VIRTIO_BLK_T_OUT && fclass == VBF_DROP) {
					/* lying-disk durability bug: ack S_OK but do NOT persist. Even a
					 * later FLUSH cannot recover it (the disk lied); supersede any
					 * held bytes so the drop wins. */
					vblk_unsync_invalidate(d, doff, desc.len);
				} else if (type == VIRTIO_BLK_T_OUT && fclass == VBF_DROP_UNFLUSH) {
					/* M26 flush-aware DROP: HOLD the write in the volatile cache
					 * (a later FLUSH commits it; else it is lost at power-off). If it
					 * cannot be held (too big / cache full), persist normally -- an
					 * honest fallback (that sector is simply not made vulnerable). */
					if (!vblk_unsync_stash(vm, doff, desc.addr, desc.len)) {
						ok = vblk_disk_rw(vm, desc.addr, doff, desc.len, false);
						/* This persisted write is NEWER than anything still held
						 * for the same range: drop those held bytes, or a later
						 * read would overlay the stale data on top of it and a
						 * later FLUSH would commit the stale data over it. */
						vblk_unsync_invalidate(d, doff, desc.len);
						if (!ok)
							status = VIRTIO_BLK_S_IOERR;
					}
				} else if (type == VIRTIO_BLK_T_OUT && fclass == VBF_TORN) {
					/* torn write: persist ONLY the first sector of the payload */
					u64 torn_end = sector * HVX_VBLK_SECTOR + HVX_VBLK_SECTOR;

					if (doff < torn_end) {
						u32 wlen = (doff + desc.len <= torn_end) ? desc.len
							 : (u32)(torn_end - doff);
						ok = vblk_disk_rw(vm, desc.addr, doff, wlen, false);
					}
					if (!ok)
						status = VIRTIO_BLK_S_IOERR;
				} else {		/* normal path -- byte-identical to M20/M21 */
					if (type == VIRTIO_BLK_T_IN) {
						ok = vblk_disk_rw(vm, desc.addr, doff, desc.len, true);
						/* read-your-writes: overlay any held un-flushed bytes (M26;
						 * inert when the cache is empty, so pre-M26 is unchanged) */
						if (ok)
							vblk_unsync_overlay(vm, desc.addr, doff, desc.len);
					} else {
						ok = vblk_disk_rw(vm, desc.addr, doff, desc.len, false);
						/* a newer persisted write supersedes any held bytes (M26) */
						vblk_unsync_invalidate(d, doff, desc.len);
					}
					if (!ok)
						status = VIRTIO_BLK_S_IOERR;
					if (type == VIRTIO_BLK_T_IN)
						used_len += desc.len;
				}
				doff += desc.len;
			}
			first = false;
			if (!has_next)
				break;
			di = desc.next;
		}

		if (have_status) {
			hvx_guest_rw(vm, status_gpa, &status, 1, true);
			used_len += 1;			/* the status byte is device-written */
		}

		if (do_reorder && n_pend < HVX_FAULT_REORDER_MAX) {
			/* M22: defer the used-ring write so the whole kick's completions can
			 * be emitted in a seeded-permuted order (below). */
			pend[n_pend].head = head;
			pend[n_pend].used_len = used_len;
			n_pend++;
		} else {			/* used ring: {id = head, len = bytes written} */
			u16 used_idx;
			struct { u32 id; u32 len; } ue = { head, used_len };

			if (hvx_guest_rw(vm, d->used_gpa + 2, &used_idx, 2, false)) {
				u16 uslot = (u16)((u32)used_idx % d->qsize);

				hvx_guest_rw(vm, d->used_gpa + 4 + 8 * uslot, &ue, 8, true);
				used_idx++;
				hvx_guest_rw(vm, d->used_gpa + 2, &used_idx, 2, true);
			}
		}
		d->last_avail++;
		any = true;
	}

	/* M22: flush the deferred used-ring entries in a seeded-permuted order, so the
	 * guest observes completions out of submission order -- a reproducible function
	 * of (fault_seed, this kick's index). */
	if (n_pend) {
		u64 pr = fault_mix((d->fault_seed ^ 0xD15EA5E5C0FFEE11ULL)
				   + (d->fault_kick_seq + 1) * 0x9e3779b97f4a7c15ULL);
		int order[HVX_FAULT_REORDER_MAX];
		u16 used_idx;
		int i;

		for (i = 0; i < n_pend; i++)
			order[i] = i;
		for (i = n_pend - 1; i > 0; i--) {	/* Fisher-Yates */
			int j, t;

			pr = fault_mix(pr);
			j = (int)(pr % (u64)(i + 1));
			t = order[i]; order[i] = order[j]; order[j] = t;
		}
		if (hvx_guest_rw(vm, d->used_gpa + 2, &used_idx, 2, false)) {
			for (i = 0; i < n_pend; i++) {
				int k = order[i];
				struct { u32 id; u32 len; } ue = { pend[k].head, pend[k].used_len };
				u16 uslot = (u16)((u32)used_idx % d->qsize);

				hvx_guest_rw(vm, d->used_gpa + 4 + 8 * uslot, &ue, 8, true);
				used_idx++;
			}
			hvx_guest_rw(vm, d->used_gpa + 2, &used_idx, 2, true);
		}
	}

	if (any) {
		d->isr |= 1;			/* virtio ISR: queue interrupt */
		d->completion_armed = 1;	/* raised at completion_due in the run-loop */
		/* M22: DELAY faults push the shared completion out by kick_extra
		 * instructions; still a pure function of icount, so deterministic. */
		d->completion_due = vcpu->icount + HVX_VBLK_LATENCY + kick_extra;
		if (d->fault_classes)
			d->fault_kick_seq++;	/* advance the per-kick reorder key */
	}
}

/* ---- Phase-B PCI config space (0xCF8/0xCFC) -------------------------------
 * One function: virtio vendor 0x1af4, modern virtio-blk device 0x1042, a 32-bit
 * MMIO BAR0 at HVX_VBLK_BAR, and the modern virtio-PCI capability list. Present
 * only when a disk is attached (else the caller falls through to the default
 * "0xffffffff / no device" path, so a PCI-scanning boot with no disk is
 * unchanged). */
/* Effective BAR0 base the guest programmed (masked to the BAR size). Using this
 * rather than the fixed HVX_VBLK_BAR keeps the MMIO decode correct even if the OS
 * reassigns BAR0 to another address within the trapped 4th-GiB window (M21). */
static inline u32 vblk_bar_base(struct hvx_vblk *d)
{
	return d->pci_bar0 & 0xfffffff0 & ~(u32)(HVX_VBLK_BAR_SIZE - 1);
}

static u32 hvx_pci_read(struct hvx_vm *vm, u32 reg)
{
	struct hvx_vblk *d = &vm->vblk;

	switch (reg & 0xfc) {
	case 0x00: return ((u32)HVX_VBLK_DEVICE << 16) | HVX_VBLK_VENDOR;
	case 0x04: return (0x0010U << 16) | (d->pci_command & 0xffff); /* status: cap list */
	case 0x08: return 0x01800001;	/* class 01 (storage) / sub 80 / prog 00 / rev 01 */
	case 0x0c: return 0x00000000;	/* cacheline / latency / header type 0 / BIST */
	case 0x10: return (d->pci_bar0 & 0xfffffff0) & ~(u32)(HVX_VBLK_BAR_SIZE - 1); /* BAR0 */
	case 0x14: case 0x18: case 0x1c: case 0x20: case 0x24: return 0; /* BAR1..5 */
	case 0x28: return 0;		/* cardbus CIS */
	case 0x2c: return (0x0002U << 16) | HVX_VBLK_VENDOR; /* subsystem */
	case 0x30: return 0;		/* expansion ROM */
	case 0x34: return 0x40;		/* capabilities pointer -> 0x40 */
	case 0x38: return 0;
	case 0x3c: return (0x01U << 8) | (d->pci_intline & 0xff); /* INTA# | interrupt line */
	/* virtio-pci capability list: COMMON @0x40, NOTIFY @0x50, ISR @0x64, DEVICE @0x74 */
	case 0x40: return 0x01105009;	/* vndr09 next0x50 len16 cfg_type1(COMMON) */
	case 0x44: return 0x00000000;	/* bar 0 */
	case 0x48: return 0x00000000;	/* offset 0x0000 */
	case 0x4c: return 0x00001000;	/* length 0x1000 */
	case 0x50: return 0x02146409;	/* vndr09 next0x64 len20 cfg_type2(NOTIFY) */
	case 0x54: return 0x00000000;	/* bar 0 */
	case 0x58: return 0x00001000;	/* offset 0x1000 */
	case 0x5c: return 0x00001000;	/* length 0x1000 */
	case 0x60: return 0x00000000;	/* notify_off_multiplier = 0 */
	case 0x64: return 0x03107409;	/* vndr09 next0x74 len16 cfg_type3(ISR) */
	case 0x68: return 0x00000000;	/* bar 0 */
	case 0x6c: return 0x00002000;	/* offset 0x2000 */
	case 0x70: return 0x00001000;	/* length 0x1000 */
	case 0x74: return 0x04109009;	/* vndr09 next0x90 len16 cfg_type4(DEVICE) -> MSI-X */
	case 0x78: return 0x00000000;	/* bar 0 */
	case 0x7c: return 0x00003000;	/* offset 0x3000 */
	case 0x80: return 0x00001000;	/* length 0x1000 */
	/* MSI-X capability (milestone 21) at 0x90, end of the list. Message control:
	 * table size (RO, HVX_MSIX_ENTRIES-1) | driver-writable enable(15)/func-mask(14). */
	case 0x90: return (((d->msix_control & 0xc000) | (HVX_MSIX_ENTRIES - 1)) << 16) |
			  (0x00U << 8) | 0x11U;
	case 0x94: return HVX_VPCI_MSIX_TABLE | 0;	/* table offset | BIR 0 (BAR0) */
	case 0x98: return HVX_VPCI_MSIX_PBA | 0;	/* PBA offset | BIR 0 (BAR0) */
	default:   return 0;
	}
}

/*
 * A minimal Intel host bridge at 00:00.0 (milestone 21). Real Linux's conf1 probe
 * (pci_sanity_check) rejects the config mechanism unless bus 0 has a host-bridge /
 * VGA / Intel/Compaq device -- our lone virtio device (vendor 0x1af4, class 0x0180)
 * matched none, so PCI was declared "not supported" and the virtio-blk device was
 * never discovered. Presenting a standard host bridge makes the bus well-formed so
 * conf1 is accepted and the real scan then finds the virtio function at dev 3. */
static u32 hvx_pci_read_bridge(u32 reg)
{
	switch (reg & 0xfc) {
	case 0x00: return (0x1237U << 16) | 0x8086U;	/* Intel 82441FX (i440FX) host bridge */
	case 0x08: return 0x06000000;			/* class 06 (bridge) / sub 00 (host) / rev 0 */
	case 0x0c: return 0x00000000;			/* header type 0, no multifunction */
	default:   return 0;				/* no BARs, no caps, no interrupt */
	}
}

static void hvx_pci_write(struct hvx_vm *vm, u32 reg, u32 val)
{
	struct hvx_vblk *d = &vm->vblk;

	switch (reg & 0xfc) {
	case 0x04: d->pci_command = val & 0xffff; break;
	case 0x10: d->pci_bar0 = val; break;		/* BAR0 sizing / assignment */
	case 0x3c: d->pci_intline = val & 0xff; break;	/* OS-assigned interrupt line */
	case 0x90:					/* MSI-X message control (bits 31:16) */
		d->msix_control = (val >> 16) & 0xc000;	/* only enable(15)/func-mask(14) writable */
		break;
	default:   break;
	}
}

/* virtio-net PCI config accessors (defined with the virtio-net core below) */
static u32 hvx_vnet_pci_read(struct hvx_vm *vm, u32 reg);
static void hvx_vnet_pci_write(struct hvx_vm *vm, u32 reg, u32 val);

static bool hvx_pci_cfg_pio(struct hvx_vcpu *vcpu, u32 port, bool is_in, u32 size)
{
	struct hvx_vm *vm = vcpu->vm;
	struct hvx_vblk *d = &vm->vblk;
	u32 addr, reg, byteoff, dw, v;
	int which = 0;				/* 0 none, 1 virtio-blk, 2 virtio-net */

	if (port == HVX_PCI_CONFIG_ADDR) {		/* 0xcf8: 32-bit address latch (shared) */
		if (is_in)
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffffffffULL) | d->pci_cfg_addr;
		else
			d->pci_cfg_addr = (u32)vcpu->regs.rax;
		return true;
	}
	addr = d->pci_cfg_addr;				/* 0xcfc..0xcff: sized data window */
	reg = addr & 0xfc;
	byteoff = (port - HVX_PCI_CONFIG_DATA) + (addr & 3);
	{
		bool bus0fn0 = (addr & 0x80000000U) &&
			       ((addr >> 16) & 0xff) == 0 &&	/* bus 0 */
			       ((addr >> 8) & 0x7) == 0;	/* function 0 */
		u32 dev = (addr >> 11) & 0x1f;

		if (bus0fn0 && dev == 0) {		/* 00:00.0 host bridge (read-only) */
			dw = hvx_pci_read_bridge(reg);
		} else if (bus0fn0 && dev == HVX_VBLK_PCI_DEV && d->disk_pages) {
			dw = hvx_pci_read(vm, reg);	/* the virtio-blk function */
			which = 1;
		} else if (bus0fn0 && dev == HVX_VNET_PCI_DEV && vm->vnet.present) {
			dw = hvx_vnet_pci_read(vm, reg);	/* the virtio-net function */
			which = 2;
		} else {
			dw = 0xffffffff;		/* absent device */
		}
	}

	if (is_in) {
		v = (byteoff < 4) ? (dw >> (byteoff * 8)) : 0;
		if (size == 1)
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffULL) | (v & 0xff);
		else if (size == 2)
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffffULL) | (v & 0xffff);
		else
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffffffffULL) | v;
	} else if (which && byteoff < 4) {
		/* byteoff >= 4 (address bits 1:0 set plus a data port past 0xcfc) lies
		 * outside the dword: drop the write, as a read of it returns 0. Shifting
		 * by byteoff*8 >= 32 was undefined behaviour (UBSAN, M97). */
		u32 nv = (u32)vcpu->regs.rax, merged;

		if (size == 1) {
			u32 sh = byteoff * 8;

			merged = (dw & ~(0xffU << sh)) | ((nv & 0xff) << sh);
		} else if (size == 2) {
			u32 sh = byteoff * 8;

			merged = (dw & ~(0xffffU << sh)) | ((nv & 0xffff) << sh);
		} else {
			merged = nv;
		}
		if (which == 1)
			hvx_pci_write(vm, reg, merged);
		else
			hvx_vnet_pci_write(vm, reg, merged);
	}
	return true;
}

/*
 * virtio transport PIO: the Phase-A test register block, and the Phase-B PCI
 * config mechanism. Returns true if the port belongs to the (attached) device.
 * With no disk attached this returns false for EVERY port, so the pre-M20 PIO
 * decode (and the PCI-less boot) is byte-identical.
 */
static bool hvx_virtio_pio(struct hvx_vcpu *vcpu, u32 port, bool is_in, u32 size)
{
	struct hvx_vm *vm = vcpu->vm;
	struct hvx_vblk *d = &vm->vblk;

	if (!d->disk_pages && !vm->vnet.present)	/* neither device attached: pre-M20 decode */
		return false;

	if (port >= HVX_PCI_CONFIG_ADDR && port <= HVX_PCI_CONFIG_ADDR + 7)
		return hvx_pci_cfg_pio(vcpu, port, is_in, size);

	if (d->disk_pages && port >= HVX_VIRTIO_PIO_BASE && port < HVX_VIRTIO_PIO_BASE + 0x30) {
		u32 off = port - HVX_VIRTIO_PIO_BASE;

		(void)size;
		if (is_in) {
			u32 v = 0;

			switch (off) {
			case HVX_VPIO_ISR:      v = d->isr; d->isr = 0; d->irq_pending = 0; break;
			case HVX_VPIO_CAPACITY: v = (u32)(d->disk_size / HVX_VBLK_SECTOR); break;
			case HVX_VPIO_QSIZE:    v = d->qsize; break;
			case HVX_VPIO_QREADY:   v = d->qready; break;
			case HVX_VPIO_IRQ_VEC:  v = d->irq_vec; break;
			default: break;
			}
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffffffffULL) | v;
		} else {
			u32 v = (u32)vcpu->regs.rax;

			switch (off) {
			case HVX_VPIO_DESC_LO:  d->desc_gpa  = (d->desc_gpa  & ~0xffffffffULL) | v; break;
			case HVX_VPIO_DESC_HI:  d->desc_gpa  = (d->desc_gpa  &  0xffffffffULL) | ((u64)v << 32); break;
			case HVX_VPIO_AVAIL_LO: d->avail_gpa = (d->avail_gpa & ~0xffffffffULL) | v; break;
			case HVX_VPIO_AVAIL_HI: d->avail_gpa = (d->avail_gpa &  0xffffffffULL) | ((u64)v << 32); break;
			case HVX_VPIO_USED_LO:  d->used_gpa  = (d->used_gpa  & ~0xffffffffULL) | v; break;
			case HVX_VPIO_USED_HI:  d->used_gpa  = (d->used_gpa  &  0xffffffffULL) | ((u64)v << 32); break;
			case HVX_VPIO_QSIZE:    d->qsize = v; break;
			case HVX_VPIO_QREADY:   d->qready = v; if (v) d->last_avail = 0; break;
			case HVX_VPIO_IRQ_VEC:  d->irq_vec = v; d->irq_mode = VBLK_IRQ_MODE_DIRECT; break;
			case HVX_VPIO_NOTIFY:   virtio_blk_process(vcpu); break;
			default: break;
			}
		}
		return true;
	}
	return false;
}

/*
 * The host-call port (HVX_VM_HOSTCALL, M138; hvx_abi.h). A DATA OUT appends the low `size`
 * bytes of RAX (little-endian) to the record being written; a CTL OUT of the record's length
 * commits it (the writer pads the last OUT, so the bytes written may exceed the length by at
 * most 3) and ends the run call; a CTL IN reads the magic; an AVAIL IN the staged bytes left;
 * a DATA IN pops up to `size` staged bytes into RAX (zero-filled past the end). No guest
 * memory is touched and nothing loops, so a guest can only fill the fixed buffers.
 */
static void hvx_hc_pio(struct hvx_vcpu *vcpu, u32 port, bool is_in, u32 size)
{
	struct hvx_vm *vm = vcpu->vm;
	u64 v = 0;
	u32 k;

	if (is_in) {
		if (port == HVX_HC_CTL_PORT) {
			v = HVX_HC_MAGIC;
		} else if (port == HVX_HC_AVAIL_PORT) {
			v = vm->hc_in_len - vm->hc_in_off;
		} else {
			for (k = 0; k < size && vm->hc_in_off < vm->hc_in_len; k++)
				v |= (u64)vm->hc_in[vm->hc_in_off++] << (8 * k);
			if (vm->hc_in_off == vm->hc_in_len)
				vm->hc_in_off = vm->hc_in_len = 0;
		}
		if (size == 1)
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffULL) | (v & 0xff);
		else if (size == 2)
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffffULL) | (v & 0xffff);
		else
			vcpu->regs.rax = (u32)v;	/* a 32-bit IN zero-extends RAX */
		return;
	}
	v = vcpu->regs.rax;
	if (port == HVX_HC_DATA_PORT) {
		for (k = 0; k < size; k++) {
			if (vm->hc_out_len >= HVX_HC_SZ) {
				vm->hc_ovf = true;
				break;
			}
			vm->hc_out[vm->hc_out_len++] = (u8)(v >> (8 * k));
		}
	} else if (port == HVX_HC_CTL_PORT && size == 4) {
		u32 n = vm->hc_out_len - vm->hc_out_done, len = (u32)v;

		if (vm->hc_ovf || len > n || n - len >= 4) {
			vm->hc_errors++;
			vm->hc_out_len = vm->hc_out_done;	/* drop the record */
		} else {
			vm->hc_out_len = vm->hc_out_done + len;
			vm->hc_out_done = vm->hc_out_len;
			vm->hc_records++;
		}
		vm->hc_ovf = false;
		if (!vm->sched_on)
			vm->hc_exit = true;
	}
}

/*
 * Emulate a trapped IN/OUT so guest I/O never reaches the host's physical ports.
 * OUT to the UART transmit register (0x3f8) is captured into the console; a read
 * of the receive register (RBR) pops a queued input byte (milestone 18); a read
 * of the line-status register (0x3f8+5) returns "transmit ready" plus Data-Ready
 * when input is queued; every other port is inert (IN reads 0xff, OUT dropped).
 * String (OUTS/INS) is handled for our identity-mapped guests, where the
 * guest-linear address equals the GPA.
 */
static void hvx_pio(struct hvx_vcpu *vcpu)
{
	struct hvx_vm *vm = vcpu->vm;		/* shared: console, uart, PIC, input, RAM */
	u64 q = vmread(EXIT_QUALIFICATION);
	u32 port = (u32)(q >> 16) & 0xffff;
	u32 size = (u32)(q & 7) + 1;		/* size code 0/1/3 -> 1/2/4 bytes */
	bool is_in = q & (1U << 3);
	bool is_string = q & (1U << 4);
	bool is_rep = q & (1U << 5);

	if (is_string) {
		u64 count = is_rep ? vcpu->regs.rcx : 1;
		u64 addr = vmread(GUEST_LINEAR_ADDR);	/* GVA == GPA (identity map) */
		u64 i, b;

		/*
		 * Only OUTS to the UART has an effect (bytes into the console). The guest
		 * chooses RCX, so this loop must not run `count` times: it ran with IRQs off
		 * in VMX root, and a REP INS/OUTS with RCX near 2^64 held the host CPU for
		 * good (M97). Stop once the console is full or the source leaves guest RAM;
		 * a source address can only come back into RAM by wrapping past 2^64.
		 */
		for (i = 0; i < count && !is_in && port == UART_BASE &&
			    vm->console_len < HVX_CONSOLE_SZ && hvx_gpa_is_ram(vm, addr); i++, addr += size) {
			for (b = 0; b < size; b++)
				if (hvx_gpa_is_ram(vm, addr + b) && vm->console_len < HVX_CONSOLE_SZ)
					vm->console[vm->console_len++] =
						*(u8 *)gpa_host(vm, addr + b);
		}
		if (is_in)
			vcpu->regs.rdi += count * size;	/* assume DF=0 */
		else
			vcpu->regs.rsi += count * size;
		if (is_rep)
			vcpu->regs.rcx = 0;
		return;
	}

	/* virtio-blk device ports (Phase-A test transport + Phase-B PCI config).
	 * Inert (returns false) unless a disk is attached, so the boot is unchanged.
	 *
	 * COLLISION FIX: the synchronous fuzz-input ports 0x510/0x511 fall INSIDE the
	 * Phase-A doorbell range 0x500-0x52f (0x510 == HVX_VPIO_USED_LO). With a disk
	 * attached, virtio would shadow them, so a guest READING 0x511 gets the doorbell
	 * default (0 = "no data") instead of fuzz_pending -- and a fuzz reader spins
	 * forever (the Phase-3 deep-resume "hang"). The Phase-A doorbell only ever
	 * *writes* those offsets and never reads them (IN 0x510/0x511 -> default 0), so
	 * routing an IN of the fuzz ports to the fuzz dispatch below is safe for
	 * hvx_virtio_iso_test: with no program staged, fuzz IN also returns 0, identical
	 * to the virtio default -> every M20+ fingerprint is preserved. OUT is untouched. */
	if (!(is_in && (port == HVX_FUZZ_DATA_PORT || port == HVX_FUZZ_STATUS_PORT)) &&
	    hvx_virtio_pio(vcpu, port, is_in, size))
		return;

	/* the host-call port (HVX_VM_HOSTCALL, M138); without the flag these ports fall
	 * through to the unhandled-port path below, as before */
	if (vm->hc_on && (port == HVX_HC_DATA_PORT || port == HVX_HC_CTL_PORT ||
			  port == HVX_HC_AVAIL_PORT)) {
		hvx_hc_pio(vcpu, port, is_in, size);
		return;
	}

	if (is_in) {
		u32 val = 0xffffffff;			/* unhandled port */

		if (port >= UART_BASE && port < UART_BASE + 8) {
			bool dlab = vm->uart.lcr & 0x80;

			switch (port - UART_BASE) {
			case 0:	/* RBR: pop a queued input byte (clears the RX cause);
				 * DLL when DLAB=1 */
				if (dlab)
					val = vm->uart.dll;
				else if (uart_rx_pending(vm))
					val = uart_rx_pop(vm);
				else
					val = 0;
				break;
			case 1: val = dlab ? vm->uart.dlm : vm->uart.ier; break;
			case 2: /* IIR (bit0=0 => an interrupt is pending; the serial ISR reads
				 * this to pick a cause). Received-data-available (0x04) has
				 * priority when an RX byte is queued and ERBFI (IER bit0) is set;
				 * else THR-empty (0x02) while THRI (bit1) is set (we TX
				 * instantly); else no interrupt pending (0x01). */
				if (uart_rx_pending(vm) && (vm->uart.ier & 0x01))
					val = 0x04;
				else if (vm->uart.ier & 0x02)
					val = 0x02;
				else
					val = 0x01;
				break;
			case 3: val = vm->uart.lcr; break;
			case 4: val = vm->uart.mcr; break;
			case 5: /* LSR: THRE|TEMT always; DR (bit0) when input is queued */
				val = 0x60 | (uart_rx_pending(vm) ? 0x01 : 0x00);
				break;
			case 6: val = 0xb0; break;	/* MSR: DCD|DSR|CTS (carrier present) */
			case 7: val = vm->uart.scr; break;
			}
		} else if (port == 0x21)		/* 8259 master IMR */
			val = vm->pic_mask[0];
		else if (port == 0xa1)			/* 8259 slave IMR */
			val = vm->pic_mask[1];
		else if (port == 0x20 || port == 0xa0)	/* 8259 command/status */
			val = 0;
		else if (port == HVX_FUZZ_DATA_PORT)	/* 0x510: next staged program byte */
			val = fuzz_pop(vm);
		else if (port == HVX_FUZZ_STATUS_PORT)	/* 0x511: 1 while bytes remain, else 0 */
			val = fuzz_pending(vm) ? 1 : 0;
		if (size == 1)
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffULL) | (val & 0xff);
		else if (size == 2)
			vcpu->regs.rax = (vcpu->regs.rax & ~0xffffULL) | (val & 0xffff);
		else
			vcpu->regs.rax = val;		/* 32-bit IN zero-extends RAX */
	} else if (port >= UART_BASE && port < UART_BASE + 8) {
		bool dlab = vm->uart.lcr & 0x80;
		u8 v = (u8)vcpu->regs.rax;

		switch (port - UART_BASE) {
		case 0:					/* THR / DLL */
			if (dlab)
				vm->uart.dll = v;
			else if (vm->console_len < HVX_CONSOLE_SZ)
				vm->console[vm->console_len++] = v;
			break;
		case 1: if (dlab) vm->uart.dlm = v; else vm->uart.ier = v; break;
		case 2: vm->uart.fcr = v; break;
		case 3: vm->uart.lcr = v; break;
		case 4: vm->uart.mcr = v; break;
		case 7: vm->uart.scr = v; break;
		/* LSR (5) / MSR (6) are read-only */
		}
	} else if (port == 0x21) {			/* Linux masks the PICs when it */
		vm->pic_mask[0] = (u8)vcpu->regs.rax;	/* has an IOAPIC; keep them latched */
	} else if (port == 0xa1) {			/* masked so no spurious IRQ fires */
		vm->pic_mask[1] = (u8)vcpu->regs.rax;
	}
	/* 8259 command writes (0x20/0xa0) and every other port: dropped */
}

/* ---- IOAPIC MMIO (milestone 15) ---- */

/* Fetch guest instruction bytes at RIP. Two identity windows resolve directly to
 * a backed guest page: the decompressed x86-64 kernel maps its text at
 * __START_KERNEL_map (0xffffffff80000000 + phys), and our hand-written long-mode
 * guests run identity-mapped in low RAM (guest-virtual == guest-physical). So a
 * kernel-text RIP maps as rip - KBASE, and a low RIP maps as rip itself -- both
 * the shortcut hvx_linux.c's dump_fault_insn uses. Returns false if RIP is in
 * neither window or the bytes are unbacked. */
static bool fetch_guest_insn(struct hvx_vm *vm, u64 rip, u8 *buf, int n)
{
	const u64 KBASE = 0xffffffff80000000ULL;
	u64 phys;
	int i;

	phys = (rip >= KBASE) ? (rip - KBASE) : rip;	/* kernel text, or identity-mapped low RAM */
	for (i = 0; i < n; i++) {
		u64 p = phys + i;

		if (!hvx_gpa_is_ram(vm, p))
			return false;
		buf[i] = *(u8 *)gpa_host(vm, p);
	}
	return true;
}

/* Map an x86 GPR number (0=rAX,1=rCX,2=rDX,3=rBX,4=rSP,5=rBP,6=rSI,7=rDI,8..15)
 * to its slot in struct hvx_regs (whose field order differs). rSP lives in the
 * VMCS, not the register file, and is never an MMIO data operand -- return NULL. */
static u64 *hvx_gpr(struct hvx_vcpu *vcpu, int n)
{
	struct hvx_regs *r = &vcpu->regs;

	switch (n) {
	case 0:  return &r->rax; case 1:  return &r->rcx; case 2:  return &r->rdx;
	case 3:  return &r->rbx; case 4:  return NULL;    case 5:  return &r->rbp;
	case 6:  return &r->rsi; case 7:  return &r->rdi;
	case 8:  return &r->r8;  case 9:  return &r->r9;  case 10: return &r->r10;
	case 11: return &r->r11; case 12: return &r->r12; case 13: return &r->r13;
	case 14: return &r->r14; case 15: return &r->r15;
	}
	return NULL;
}

static u32 hvx_ioapic_read(struct hvx_vm *vm, u32 sel)
{
	if (sel == 0x00 || sel == 0x02)
		return vm->ioapic.id & 0x0f000000;	/* ID / arbitration id */
	if (sel == 0x01)
		return 0x00170011;			/* VERSION: ver 0x11, max-redir 23 */
	if (sel >= 0x10 && sel <= 0x3f)
		return vm->ioapic.redir[sel - 0x10];
	return 0;
}

static void hvx_ioapic_write(struct hvx_vm *vm, u32 sel, u32 val)
{
	if (sel == 0x00)
		vm->ioapic.id = val & 0x0f000000;
	else if (sel >= 0x10 && sel <= 0x3f)
		vm->ioapic.redir[sel - 0x10] = val;	/* read-as-written; stays masked */
}

/*
 * Emulate a 32-bit MOV to/from the IOAPIC window at 0xfec00000 on an EPT
 * violation. Linux uses readl/writel = MOV r32,r/m32 (0x8b) / MOV r/m32,r32
 * (0x89); we decode just enough (legacy prefixes, optional REX, opcode, ModRM
 * reg) to find the register operand -- the effective address is already in the
 * VMCS as the faulting GPA. Offset 0x00 = IOREGSEL, 0x10 = IOWIN. Returns true
 * on success (RIP advanced past the instruction). */
static bool hvx_ioapic_mmio(struct hvx_vcpu *vcpu, u64 gpa, u32 insn_len)
{
	struct hvx_vm *vm = vcpu->vm;		/* shared: IOAPIC + guest RAM (insn fetch) */
	u32 off = (u32)gpa & 0xff;
	u8 ib[16];
	int i = 0, regnum;
	u8 rex = 0, op;
	bool is_write;
	u64 *reg;

	if (!fetch_guest_insn(vm, vcpu->regs.rip, ib, sizeof(ib)))
		return false;

	while (i < 15 && (ib[i] == 0x66 || ib[i] == 0x67 || ib[i] == 0xf0 ||
			  ib[i] == 0xf2 || ib[i] == 0xf3 || ib[i] == 0x2e ||
			  ib[i] == 0x36 || ib[i] == 0x3e || ib[i] == 0x26 ||
			  ib[i] == 0x64 || ib[i] == 0x65))
		i++;
	if (i < 15 && (ib[i] & 0xf0) == 0x40)
		rex = ib[i++];				/* REX prefix */
	if (i >= 15)
		return false;
	op = ib[i++];
	if (op != 0x89 && op != 0x8b)			/* only the MOV r/m32 forms */
		return false;
	if (i >= 16)
		return false;
	regnum = ((ib[i] >> 3) & 7) | ((rex & 4) ? 8 : 0);   /* ModRM.reg | REX.R */
	is_write = (op == 0x89);			/* store to memory == IOAPIC write */
	reg = hvx_gpr(vcpu, regnum);

	if (off == 0x00) {				/* IOREGSEL */
		if (is_write) {
			if (reg)
				vm->ioapic.ioregsel = (u32)*reg;
		} else if (reg) {
			*reg = vm->ioapic.ioregsel;	/* 32-bit load zero-extends */
		}
	} else if (off == 0x10) {			/* IOWIN */
		if (is_write)
			hvx_ioapic_write(vm, vm->ioapic.ioregsel, reg ? (u32)*reg : 0);
		else if (reg)
			*reg = hvx_ioapic_read(vm, vm->ioapic.ioregsel);
	} else if (!is_write && reg) {			/* other regs (EOI etc.): read 0 */
		*reg = 0;
	}

	vcpu->regs.rip += insn_len;
	VMW(GUEST_RIP, vcpu->regs.rip);
	return true;
}

/* ---- Phase-B virtio-pci MMIO BAR (modern layout; milestone 20) ----
 * BAR0 (HVX_VBLK_BAR) is EPT-unmapped, so guest accesses trap as EPT violations
 * we decode in-kernel. The BAR holds the modern virtio structures: common cfg
 * (0x0000), notify (0x1000), ISR (0x2000), device cfg (0x3000). Same virtio-blk
 * core as Phase A; only the register front-end differs. */
static u64 hvx_virtio_bar_read(struct hvx_vcpu *vcpu, u32 off, int size)
{
	struct hvx_vblk *d = &vcpu->vm->vblk;

	if (off >= HVX_VPCI_MSIX_TABLE && off < HVX_VPCI_MSIX_TABLE + HVX_MSIX_ENTRIES * 16) {
		u32 idx = (off - HVX_VPCI_MSIX_TABLE) / 16;	/* table entry (16 B each) */
		u32 word = ((off - HVX_VPCI_MSIX_TABLE) % 16) / 4; /* addr_lo/hi/data/ctrl */

		return d->msix_tbl[idx * 4 + word];
	}
	if (off >= HVX_VPCI_MSIX_PBA && off < HVX_VPCI_MSIX_PBA + 64) {
		/* Pending-bit array: a completion held pending (masked at delivery time)
		 * shows as a set bit for the queue's vector; computed, not stored. */
		u32 v = 0;

		if (d->irq_pending && (d->msix_control & 0x8000) &&
		    d->queue_msix_vector < HVX_MSIX_ENTRIES)
			v = 1u << d->queue_msix_vector;
		return v;
	}
	if (off < HVX_VPCI_NOTIFY) {			/* common cfg */
		switch (off) {
		case 0x00: return d->dfeat_sel;
		case 0x04:					/* device_feature for the selected dword */
			if (d->dfeat_sel == 1)
				return 1u;			/* VIRTIO_F_VERSION_1 (bit 32) */
			/* low 32: VIRTIO_BLK_F_FLUSH (bit 9) ONLY when the flush-aware fault
			 * class is armed (M26), so the guest issues FLUSH on fsync and we can
			 * make a flushed write durable. Off otherwise => every prior VM
			 * negotiates no low features and is byte-identical. */
			return (d->fault_classes & HVX_FAULT_DROP_UNFLUSHED) ? (1u << VIRTIO_BLK_F_FLUSH) : 0u;
		case 0x08: return d->gfeat_sel;
		case 0x0c: return (d->gfeat_sel == 1) ? (u32)(d->gfeatures >> 32) : (u32)d->gfeatures;
		case 0x10: return d->config_msix_vector;	/* msix_config (config-change vector) */
		case 0x12: return 1;				/* num_queues */
		case 0x14: return d->status & 0xff;		/* device_status */
		case 0x15: return 0;				/* config_generation */
		case 0x16: return d->qsel;			/* queue_select */
		case 0x18: return d->qsize;			/* queue_size (device max; default set on attach) */
		case 0x1a: return d->queue_msix_vector;		/* queue_msix_vector (queue 0) */
		case 0x1c: return d->qready;			/* queue_enable */
		case 0x1e: return 0;				/* queue_notify_off */
		case 0x20: return (size == 8) ? d->desc_gpa  : (u32)d->desc_gpa;
		case 0x24: return (u32)(d->desc_gpa >> 32);
		case 0x28: return (size == 8) ? d->avail_gpa : (u32)d->avail_gpa;
		case 0x2c: return (u32)(d->avail_gpa >> 32);
		case 0x30: return (size == 8) ? d->used_gpa  : (u32)d->used_gpa;
		case 0x34: return (u32)(d->used_gpa >> 32);
		default:   return 0;
		}
	}
	if (off < HVX_VPCI_ISR)				/* notify: read side unused */
		return 0;
	if (off < HVX_VPCI_DEVICE) {			/* ISR status, read-to-clear */
		u32 v = d->isr;

		d->isr = 0;
		d->irq_pending = 0;
		return v;
	}
	if (off < HVX_VPCI_MSIX_TABLE) {		/* device cfg: virtio_blk_config.capacity */
		u64 cap = d->disk_size / HVX_VBLK_SECTOR;
		u32 devoff = off - HVX_VPCI_DEVICE;

		if (devoff == 0)
			return (size == 8) ? cap : (u32)cap;
		if (devoff == 4)
			return (u32)(cap >> 32);
		return 0;
	}
	return 0;
}

static void hvx_virtio_bar_write(struct hvx_vcpu *vcpu, u32 off, int size, u64 val)
{
	struct hvx_vblk *d = &vcpu->vm->vblk;

	(void)size;
	if (off >= HVX_VPCI_MSIX_TABLE && off < HVX_VPCI_MSIX_TABLE + HVX_MSIX_ENTRIES * 16) {
		u32 idx = (off - HVX_VPCI_MSIX_TABLE) / 16;	/* {addr_lo,addr_hi,data,ctrl} */
		u32 word = ((off - HVX_VPCI_MSIX_TABLE) % 16) / 4;

		d->msix_tbl[idx * 4 + word] = (u32)val;
		return;					/* PBA is read-only; an unmask is picked up
							 * by the run-loop's vblk_msix_irq check */
	}
	if (off < HVX_VPCI_NOTIFY) {			/* common cfg */
		switch (off) {
		case 0x00: d->dfeat_sel = (u32)val; break;
		case 0x08: d->gfeat_sel = (u32)val; break;
		case 0x0c:
			if (d->gfeat_sel == 1)
				d->gfeatures = (d->gfeatures & 0xffffffffULL) | ((u64)(u32)val << 32);
			else
				d->gfeatures = (d->gfeatures & ~0xffffffffULL) | (u32)val;
			break;
		case 0x10: d->config_msix_vector = (u32)val & 0xffff; break;	/* msix_config */
		case 0x14:				/* device_status (0 = reset) */
			d->status = (u32)val & 0xff;
			if (d->status == 0) {
				d->qready = 0; d->last_avail = 0;
				d->isr = 0; d->irq_pending = 0; d->completion_armed = 0;
				/* virtio reset reverts MSI-X vector assignments to NO_VECTOR;
				 * the driver re-programs them on re-init. MSI-X enable itself
				 * lives in PCI config space (not device_status), so leave it. */
				d->config_msix_vector = HVX_MSIX_NO_VECTOR;
				d->queue_msix_vector = HVX_MSIX_NO_VECTOR;
				vblk_unsync_clear(d);	/* reset drops the volatile write cache (M26) */
			}
			break;
		case 0x16: d->qsel = (u32)val; break;
		case 0x18: d->qsize = (u32)val; break;
		case 0x1a: d->queue_msix_vector = (u32)val & 0xffff; break;	/* queue_msix_vector */
		case 0x1c: d->qready = (u32)val; if (val) d->last_avail = 0; break;
		case 0x20: d->desc_gpa  = (d->desc_gpa  & ~0xffffffffULL) | (u32)val; break;
		case 0x24: d->desc_gpa  = (d->desc_gpa  &  0xffffffffULL) | ((u64)(u32)val << 32); break;
		case 0x28: d->avail_gpa = (d->avail_gpa & ~0xffffffffULL) | (u32)val; break;
		case 0x2c: d->avail_gpa = (d->avail_gpa &  0xffffffffULL) | ((u64)(u32)val << 32); break;
		case 0x30: d->used_gpa  = (d->used_gpa  & ~0xffffffffULL) | (u32)val; break;
		case 0x34: d->used_gpa  = (d->used_gpa  &  0xffffffffULL) | ((u64)(u32)val << 32); break;
		default:   break;
		}
		/* If MSI-X is not the chosen path, this is the INTx (Phase-B) transport. */
		if (!(d->msix_control & 0x8000))
			d->irq_mode = VBLK_IRQ_MODE_INTX;
		return;
	}
	if (off < HVX_VPCI_ISR) {			/* notify: doorbell */
		virtio_blk_process(vcpu);
		return;
	}
	/* ISR (read-only from the driver) and device cfg (RO) ignore writes */
}

/*
 * Decode a MOV to/from the virtio BAR region on an EPT violation. Like the IOAPIC
 * decoder, the effective address is already the faulting GPA; we decode just the
 * operand register + direction + size (legacy prefixes, 0x66, REX, MOV opcodes
 * 0x88/0x89/0x8a/0x8b -- the forms the guests and Linux's io accessors emit).
 * Returns true on success (RIP advanced past the instruction).
 */
static bool hvx_virtio_mmio(struct hvx_vcpu *vcpu, u64 gpa, u32 insn_len)
{
	struct hvx_vm *vm = vcpu->vm;
	u32 off = (u32)(gpa - vblk_bar_base(&vm->vblk));
	u8 ib[16];
	int i = 0, regnum, size;
	u8 rex = 0, op;
	bool opsz16 = false, is_write;
	u64 *reg, val = 0;

	if (!fetch_guest_insn(vm, vcpu->regs.rip, ib, sizeof(ib)))
		return false;
	while (i < 15 && (ib[i] == 0x66 || ib[i] == 0x67 || ib[i] == 0xf0 ||
			  ib[i] == 0xf2 || ib[i] == 0xf3 || ib[i] == 0x2e ||
			  ib[i] == 0x36 || ib[i] == 0x3e || ib[i] == 0x26 ||
			  ib[i] == 0x64 || ib[i] == 0x65)) {
		if (ib[i] == 0x66)
			opsz16 = true;
		i++;
	}
	if (i < 15 && (ib[i] & 0xf0) == 0x40)
		rex = ib[i++];
	if (i >= 15)
		return false;
	op = ib[i++];
	if (i >= 16)
		return false;
	regnum = ((ib[i] >> 3) & 7) | ((rex & 4) ? 8 : 0);	/* ModRM.reg | REX.R */
	switch (op) {
	case 0x88: is_write = true;  size = 1; break;			/* MOV r/m8, r8 */
	case 0x8a: is_write = false; size = 1; break;			/* MOV r8, r/m8 */
	case 0x89: is_write = true;  size = (rex & 8) ? 8 : (opsz16 ? 2 : 4); break;
	case 0x8b: is_write = false; size = (rex & 8) ? 8 : (opsz16 ? 2 : 4); break;
	default:   return false;
	}
	reg = hvx_gpr(vcpu, regnum);

	if (is_write) {
		if (reg)
			val = *reg;
		if (size == 1)      val &= 0xff;
		else if (size == 2) val &= 0xffff;
		else if (size == 4) val &= 0xffffffff;
		hvx_virtio_bar_write(vcpu, off, size, val);
	} else {
		val = hvx_virtio_bar_read(vcpu, off, size);
		if (reg) {
			if (size == 1)      *reg = (*reg & ~0xffULL) | (val & 0xff);
			else if (size == 2) *reg = (*reg & ~0xffffULL) | (val & 0xffff);
			else if (size == 4) *reg = val & 0xffffffff;
			else                *reg = val;
		}
	}
	vcpu->regs.rip += insn_len;
	VMW(GUEST_RIP, vcpu->regs.rip);
	return true;
}

/* ==== virtio-net core (transport-agnostic; milestone 23) =================== *
 * Two split virtqueues -- receiveq (0) and transmitq (1) -- serviced against the
 * host-side RX FIFO / TX capture buffer. RX delivery + TX capture run
 * synchronously at a queue kick (a deterministic instruction); the RX completion
 * MSI-X is scheduled at kick_icount + HVX_VNET_LATENCY, a pure function of icount
 * (the M16 creep discipline). Inert unless a net device is attached.
 * ========================================================================== */

static inline u32 vnet_bar_base(struct hvx_vnet *v)
{
	return v->pci_bar0 & 0xfffffff0 & ~(u32)(HVX_VNET_BAR_SIZE - 1);
}

/* ==== deterministic virtio-net fault injection (milestone 24) ============= *
 * A seeded policy perturbs packets (drop / delay / reorder / corrupt / duplicate)
 * so any bug it surfaces replays bit-for-bit -- symmetric to
 * the M22 disk faults. Every decision is a PURE FUNCTION of (nfault_seed, direction,
 * packet index), never host time, and the policy + PRNG counters ride in the
 * snapshot, so branch/replay reproduces the identical schedule. Inert (early-out, no
 * state touched) when nfault_classes == 0, so the disabled path is byte-identical to
 * M23. Reuses the disk faults' splitmix64 finalizer (fault_mix). The keying is
 * documented in the ABI so the userspace test can predict the exact schedule.
 * ========================================================================== */
#define HVX_VNET_FAULT_BATCH	32		/* cap on used-ring entries buffered per kick */
#define HVX_NFAULT_RX_SALT	0x0052000000000000ULL	/* 'R' << 48 : RX schedule salt */
#define HVX_NFAULT_TX_SALT	0x0054000000000000ULL	/* 'T' << 48 : TX schedule salt */

enum vnet_fault { VNF_NONE = 0, VNF_DROP, VNF_DELAY, VNF_CORRUPT, VNF_DUP };

/* Per-packet 64-bit roll: a pure function of (seed, direction, packet index). */
static inline u64 net_fault_roll(u64 seed, u32 dir, u64 index)
{
	u64 s = seed ^ (dir ? HVX_NFAULT_TX_SALT : HVX_NFAULT_RX_SALT);

	return fault_mix(s + (index + 1) * 0x9e3779b97f4a7c15ULL);
}
/* Per-kick reorder roll: a pure function of (seed, direction, kick index). */
static inline u64 net_reorder_roll(u64 seed, u32 dir, u64 kick)
{
	u64 s = (seed ^ (dir ? HVX_NFAULT_TX_SALT : HVX_NFAULT_RX_SALT))
		^ 0xD15EA5E5C0FFEE11ULL;

	return fault_mix(s + (kick + 1) * 0x9e3779b97f4a7c15ULL);
}

/*
 * Decide one packet's fault given its direction (0 = RX, 1 = TX) and per-direction
 * index. Pure function of (nfault_seed, dir, index); advances no state (the caller
 * owns the rx/tx seq counter). Returns the class, and: for DELAY the extra RX
 * completion latency in *extra; for CORRUPT the raw byte position in *cpos (the
 * caller mods by the frame length) and the nonzero XOR value in *cxor. The
 * applicable-class list is built in the FIXED order DROP, DELAY (RX only), CORRUPT,
 * DUPLICATE and the class is selected by ((h>>8) % n) -- documented so the test
 * replicates it. REORDER is handled separately (per-kick permutation).
 */
static enum vnet_fault vnet_fault_decide(struct hvx_vnet *v, u32 dir, u64 index,
					 u32 *extra, u32 *cpos, u8 *cxor)
{
	u64 h;
	u32 pct;
	enum vnet_fault cand[4];
	int n = 0, sel;

	*extra = 0;
	*cpos = 0;
	*cxor = 0;
	if (v->nfault_classes == 0)
		return VNF_NONE;
	h = net_fault_roll(v->nfault_seed, dir, index);
	pct = (u32)(h % 100);
	if (pct >= v->nfault_probability)
		return VNF_NONE;
	if (v->nfault_classes & HVX_NFAULT_DROP)
		cand[n++] = VNF_DROP;
	if ((v->nfault_classes & HVX_NFAULT_DELAY) && dir == 0)	/* RX only */
		cand[n++] = VNF_DELAY;
	if (v->nfault_classes & HVX_NFAULT_CORRUPT)
		cand[n++] = VNF_CORRUPT;
	if (v->nfault_classes & HVX_NFAULT_DUPLICATE)
		cand[n++] = VNF_DUP;
	if (n == 0)
		return VNF_NONE;	/* only RX-only DELAY enabled, but this is a TX packet */
	sel = (int)((h >> 8) % (u64)n);
	if (cand[sel] == VNF_DELAY) {
		u32 md = v->nfault_max_delay ? v->nfault_max_delay : 1;

		*extra = 1 + (u32)((h >> 16) % md);
	} else if (cand[sel] == VNF_CORRUPT) {
		*cpos = (u32)(h >> 16);
		*cxor = (u8)((h >> 32) | 1);	/* nonzero => always changes the byte */
	}
	return cand[sel];
}

/*
 * Deliver pending inbound frames into the receiveq (queue 0). Called at a receiveq
 * kick (max_consume <= 0 => drain the whole batch) or from the paced run-loop path
 * (max_consume == 1 => one FIFO frame). For each consumed frame: decide its fault
 * (M24); on DROP the frame is discarded (consumed from the FIFO, no descriptor, no
 * used entry); otherwise consume the head descriptor(s), write the 12-byte
 * virtio_net_hdr_v1 (num_buffers = 1) + the (optionally CORRUPTed) frame, and buffer
 * a used-ring entry -- twice for DUPLICATE. The buffered used entries are flushed at
 * the end, in a seeded-permuted order for REORDER. One completion is armed at
 * icount + HVX_VNET_LATENCY + summed DELAY, delivered as an MSI-X by vnet_msix_irq.
 * Pure function of guest RAM + the (scripted, deterministic) RX FIFO + the fault
 * policy. With nfault_classes == 0 this is byte-identical to the M23 delivery.
 */
static void virtio_net_rx_deliver(struct hvx_vcpu *vcpu, int max_consume)
{
	struct hvx_vm *vm = vcpu->vm;
	struct hvx_vnet *v = &vm->vnet;
	struct hvx_vnet_queue *q = &v->q[0];
	u16 avail_idx;
	bool any = false;
	u32 batch_extra = 0;
	bool do_reorder = (v->nfault_classes & HVX_NFAULT_REORDER) != 0;
	struct { u32 id; u32 len; } used[HVX_VNET_FAULT_BATCH];
	int n_used = 0, consumed = 0;

	if (!v->present || !q->qready || q->qsize == 0)
		return;
	if (!hvx_guest_rw(vm, q->avail_gpa + 2, &avail_idx, 2, false))
		return;

	while (v->rx_tail != v->rx_head && (u16)q->last_avail != avail_idx) {
		u32 fslot = v->rx_head & (HVX_VNET_RX_SLOTS - 1);
		u32 flen = v->rx_flen[fslot];
		u8 *fbuf = v->rx_buf[fslot];	/* deliver directly from the FIFO slot */
		enum vnet_fault fc = VNF_NONE;
		u32 extra = 0, cpos = 0;
		u8 cxor = 0;
		int copies, c;

		if (max_consume > 0 && consumed >= max_consume)
			break;

		/* decide this frame's fault now (the per-packet PRNG key advances ONLY
		 * when faults are enabled, so the disabled path never touches state and
		 * stays byte-identical to M23). */
		if (v->nfault_classes) {
			fc = vnet_fault_decide(v, 0 /*RX*/, v->nfault_rx_seq, &extra, &cpos, &cxor);
			v->nfault_rx_seq++;
			if (fc != VNF_NONE)
				v->nfault_count++;
		}

		/* consume the frame from the FIFO regardless -- it "arrived on the wire" */
		v->rx_head++;
		consumed++;

		if (fc == VNF_DROP)
			continue;	/* discarded: no descriptor, no used entry */
		/* CORRUPT flips one byte in place; the slot is already consumed (free), so
		 * this is safe and both DUPLICATE copies carry the same corrupted frame. */
		if (fc == VNF_CORRUPT && flen)
			fbuf[cpos % flen] ^= cxor;
		if (fc == VNF_DELAY)
			batch_extra += extra;
		copies = (fc == VNF_DUP) ? 2 : 1;

		for (c = 0; c < copies; c++) {
			u16 slot, head;
			struct virtq_desc desc;
			u8 hdr[HVX_VNET_HDR_LEN];
			u32 used_len = 0;

			if ((u16)q->last_avail == avail_idx)
				break;			/* out of posted receive buffers */
			slot = (u16)((u32)q->last_avail % q->qsize);
			if (!hvx_guest_rw(vm, q->avail_gpa + 4 + 2 * slot, &head, 2, false))
				break;
			if (!hvx_guest_rw(vm, q->desc_gpa + (u64)head * 16, &desc, 16, false))
				break;
			/* virtio_net_hdr_v1: all zero except num_buffers (offset 10) = 1 */
			memset(hdr, 0, sizeof(hdr));
			hdr[10] = 1;
			if ((desc.flags & VIRTQ_DESC_F_WRITE) &&
			    desc.len >= (u32)HVX_VNET_HDR_LEN + flen) {
				hvx_guest_rw(vm, desc.addr, hdr, HVX_VNET_HDR_LEN, true);
				hvx_guest_rw(vm, desc.addr + HVX_VNET_HDR_LEN, fbuf, flen, true);
				used_len = HVX_VNET_HDR_LEN + flen;
			}
			if (n_used < HVX_VNET_FAULT_BATCH) {
				used[n_used].id = head;
				used[n_used].len = used_len;
				n_used++;
			}
			q->last_avail++;
			any = true;
		}
	}

	/* flush the buffered used-ring entries, seeded-permuted for REORDER */
	if (n_used) {
		int order[HVX_VNET_FAULT_BATCH];
		u16 used_idx;
		int i;

		for (i = 0; i < n_used; i++)
			order[i] = i;
		if (do_reorder && n_used > 1) {
			u64 pr = net_reorder_roll(v->nfault_seed, 0 /*RX*/, v->nfault_rx_kick_seq);

			for (i = n_used - 1; i > 0; i--) {	/* Fisher-Yates */
				int j, t;

				pr = fault_mix(pr);
				j = (int)(pr % (u64)(i + 1));
				t = order[i]; order[i] = order[j]; order[j] = t;
			}
		}
		if (hvx_guest_rw(vm, q->used_gpa + 2, &used_idx, 2, false)) {
			for (i = 0; i < n_used; i++) {
				int k = order[i];
				struct { u32 id; u32 len; } ue = { used[k].id, used[k].len };
				u16 uslot = (u16)((u32)used_idx % q->qsize);

				hvx_guest_rw(vm, q->used_gpa + 4 + 8 * uslot, &ue, 8, true);
				used_idx++;
			}
			hvx_guest_rw(vm, q->used_gpa + 2, &used_idx, 2, true);
		}
	}

	if (any) {
		v->isr |= 1;
		q->completion_armed = 1;
		q->completion_due = vcpu->icount + HVX_VNET_LATENCY + batch_extra;
		if (v->nfault_classes)
			v->nfault_rx_kick_seq++;	/* advance the per-kick reorder key */
	}
}

/*
 * Service the transmitq (queue 1): capture each posted frame. Called at a transmitq
 * kick. A chain is [12-byte virtio_net_hdr][frame data...] across one or more
 * readable descriptors; skip the header bytes and copy the payload into the TX
 * capture buffer, write the used ring (device wrote nothing -> len 0), advance the
 * cursors. Arms a TX completion only if the driver bound an MSI-X vector to the TX
 * queue (real Linux does; the hand-written test leaves it NO_VECTOR).
 */
static void virtio_net_tx_process(struct hvx_vcpu *vcpu)
{
	struct hvx_vm *vm = vcpu->vm;
	struct hvx_vnet *v = &vm->vnet;
	struct hvx_vnet_queue *q = &v->q[1];
	u16 avail_idx;
	bool any = false;
	bool do_reorder = (v->nfault_classes & HVX_NFAULT_REORDER) != 0;
	struct { u32 id; u32 len; } used[HVX_VNET_FAULT_BATCH];
	int n_used = 0;
	u32 walked = 0;			/* descriptors read this kick (M97 bound) */

	if (!v->present || !q->qready || q->qsize == 0)
		return;
	if (!hvx_guest_rw(vm, q->avail_gpa + 2, &avail_idx, 2, false))
		return;

	while ((u16)q->last_avail != avail_idx && walked < HVX_VQ_KICK_BUDGET) {
		u16 slot = (u16)((u32)q->last_avail % q->qsize);
		u16 head, di;
		u32 skip = HVX_VNET_HDR_LEN;	/* bytes of virtio_net_hdr to drop */
		int guardn;
		u32 cap_start = v->tx_len;	/* capture the payload straight into tx_buf */
		u32 cap_len = 0;
		enum vnet_fault fc = VNF_NONE;
		u32 extra = 0, cpos = 0;
		u8 cxor = 0;

		if (!hvx_guest_rw(vm, q->avail_gpa + 4 + 2 * slot, &head, 2, false))
			break;
		di = head;
		for (guardn = 0; guardn < hvx_vq_chain_max(q->qsize); guardn++) {
			struct virtq_desc desc;
			bool has_next;

			walked++;
			if (!hvx_guest_rw(vm, q->desc_gpa + (u64)di * 16, &desc, 16, false))
				break;
			has_next = desc.flags & VIRTQ_DESC_F_NEXT;
			if (!(desc.flags & VIRTQ_DESC_F_WRITE)) {	/* readable: outbound */
				u32 off = 0, len = desc.len;

				if (skip) {			/* drop the header prefix */
					u32 s = skip < len ? skip : len;

					skip -= s; off += s; len -= s;
				}
				/* written so that a guest descriptor length near 2^32 cannot wrap the sum (M97) */
				if (len && v->tx_len <= HVX_VNET_TX_CAP && len <= HVX_VNET_TX_CAP - v->tx_len) {
					hvx_guest_rw(vm, desc.addr + off, v->tx_buf + v->tx_len, len, false);
					v->tx_len += len;
					cap_len += len;
				}
			}
			if (!has_next)
				break;
			di = desc.next;
		}

		/* decide this frame's fault (the per-packet PRNG key advances ONLY when
		 * faults are enabled, so the disabled path stays byte-identical to M23). */
		if (v->nfault_classes) {
			fc = vnet_fault_decide(v, 1 /*TX*/, v->nfault_tx_seq, &extra, &cpos, &cxor);
			v->nfault_tx_seq++;
			if (fc != VNF_NONE)
				v->nfault_count++;
		}
		(void)extra;	/* DELAY is RX-only */
		if (fc == VNF_DROP) {
			v->tx_len = cap_start;		/* dropped on the wire: un-capture */
		} else {
			if (fc == VNF_CORRUPT && cap_len)
				v->tx_buf[cap_start + (cpos % cap_len)] ^= cxor;
			if (cap_len)
				v->tx_count++;
			if (fc == VNF_DUP && cap_len && v->tx_len + cap_len <= HVX_VNET_TX_CAP) {
				memcpy(v->tx_buf + v->tx_len, v->tx_buf + cap_start, cap_len);
				v->tx_len += cap_len;
				v->tx_count++;
			}
		}

		/* the guest's transmit buffer is always consumed/acked (a dropped packet
		 * is dropped on the wire, not un-posted), so always buffer a used entry. */
		if (n_used < HVX_VNET_FAULT_BATCH) {
			used[n_used].id = head;
			used[n_used].len = 0;
			n_used++;
		}
		q->last_avail++;
		any = true;
	}

	/* flush the buffered used-ring entries, seeded-permuted for REORDER */
	if (n_used) {
		int order[HVX_VNET_FAULT_BATCH];
		u16 used_idx;
		int i;

		for (i = 0; i < n_used; i++)
			order[i] = i;
		if (do_reorder && n_used > 1) {
			u64 pr = net_reorder_roll(v->nfault_seed, 1 /*TX*/, v->nfault_tx_kick_seq);

			for (i = n_used - 1; i > 0; i--) {	/* Fisher-Yates */
				int j, t;

				pr = fault_mix(pr);
				j = (int)(pr % (u64)(i + 1));
				t = order[i]; order[i] = order[j]; order[j] = t;
			}
		}
		if (hvx_guest_rw(vm, q->used_gpa + 2, &used_idx, 2, false)) {
			for (i = 0; i < n_used; i++) {
				int k = order[i];
				struct { u32 id; u32 len; } ue = { used[k].id, used[k].len };
				u16 uslot = (u16)((u32)used_idx % q->qsize);

				hvx_guest_rw(vm, q->used_gpa + 4 + 8 * uslot, &ue, 8, true);
				used_idx++;
			}
			hvx_guest_rw(vm, q->used_gpa + 2, &used_idx, 2, true);
		}
	}

	if (any) {
		v->isr |= 1;
		if (v->nfault_classes)
			v->nfault_tx_kick_seq++;	/* advance the per-kick reorder key */
		if (q->msix_vector < HVX_MSIX_ENTRIES) {
			q->completion_armed = 1;
			q->completion_due = vcpu->icount + HVX_VNET_LATENCY;
		}
	}
}

/* virtio-net PCI config: one function (vendor 0x1af4, modern virtio-net 0x1041),
 * class 02/00 (Ethernet), a 32-bit MMIO BAR0, the modern virtio-pci capability
 * list + an MSI-X capability -- the same layout as the virtio-blk function. */
static u32 hvx_vnet_pci_read(struct hvx_vm *vm, u32 reg)
{
	struct hvx_vnet *v = &vm->vnet;

	switch (reg & 0xfc) {
	case 0x00: return ((u32)HVX_VNET_DEVICE << 16) | HVX_VNET_VENDOR;
	case 0x04: return (0x0010U << 16) | (v->pci_command & 0xffff);	/* status: cap list */
	case 0x08: return 0x02000001;	/* class 02 (network) / sub 00 (ethernet) / rev 01 */
	case 0x0c: return 0x00000000;
	case 0x10: return (v->pci_bar0 & 0xfffffff0) & ~(u32)(HVX_VNET_BAR_SIZE - 1); /* BAR0 */
	case 0x14: case 0x18: case 0x1c: case 0x20: case 0x24: return 0;
	case 0x28: return 0;
	case 0x2c: return (0x0001U << 16) | HVX_VNET_VENDOR;	/* subsystem: virtio type 1 */
	case 0x30: return 0;
	case 0x34: return 0x40;		/* capabilities pointer -> 0x40 */
	case 0x38: return 0;
	case 0x3c: return (0x01U << 8) | (v->pci_intline & 0xff);	/* INTA# | interrupt line */
	/* virtio-pci capability list: COMMON @0x40, NOTIFY @0x50, ISR @0x64, DEVICE @0x74 */
	case 0x40: return 0x01105009;	/* vndr09 next0x50 len16 cfg_type1(COMMON) */
	case 0x44: return 0x00000000;
	case 0x48: return 0x00000000;
	case 0x4c: return 0x00001000;
	case 0x50: return 0x02146409;	/* vndr09 next0x64 len20 cfg_type2(NOTIFY) */
	case 0x54: return 0x00000000;
	case 0x58: return 0x00001000;
	case 0x5c: return 0x00001000;
	case 0x60: return 0x00000000;	/* notify_off_multiplier = 0 */
	case 0x64: return 0x03107409;	/* vndr09 next0x74 len16 cfg_type3(ISR) */
	case 0x68: return 0x00000000;
	case 0x6c: return 0x00002000;
	case 0x70: return 0x00001000;
	case 0x74: return 0x04109009;	/* vndr09 next0x90 len16 cfg_type4(DEVICE) -> MSI-X */
	case 0x78: return 0x00000000;
	case 0x7c: return 0x00003000;
	case 0x80: return 0x00001000;
	/* MSI-X capability at 0x90 (message control: table size RO | enable/func-mask) */
	case 0x90: return (((v->msix_control & 0xc000) | (HVX_MSIX_ENTRIES - 1)) << 16) |
			  (0x00U << 8) | 0x11U;
	case 0x94: return HVX_VPCI_MSIX_TABLE | 0;	/* table offset | BIR 0 */
	case 0x98: return HVX_VPCI_MSIX_PBA | 0;		/* PBA offset | BIR 0 */
	default:   return 0;
	}
}

static void hvx_vnet_pci_write(struct hvx_vm *vm, u32 reg, u32 val)
{
	struct hvx_vnet *v = &vm->vnet;

	switch (reg & 0xfc) {
	case 0x04: v->pci_command = val & 0xffff; break;
	case 0x10: v->pci_bar0 = val; break;
	case 0x3c: v->pci_intline = val & 0xff; break;
	case 0x90: v->msix_control = (val >> 16) & 0xc000; break;	/* enable/func-mask */
	default:   break;
	}
}

static u64 hvx_vnet_bar_read(struct hvx_vcpu *vcpu, u32 off, int size)
{
	struct hvx_vnet *v = &vcpu->vm->vnet;
	struct hvx_vnet_queue *q = &v->q[v->qsel & (HVX_VNET_NQUEUES - 1)];

	if (off >= HVX_VPCI_MSIX_TABLE && off < HVX_VPCI_MSIX_TABLE + HVX_MSIX_ENTRIES * 16) {
		u32 idx = (off - HVX_VPCI_MSIX_TABLE) / 16;
		u32 word = ((off - HVX_VPCI_MSIX_TABLE) % 16) / 4;

		return v->msix_tbl[idx * 4 + word];
	}
	if (off >= HVX_VPCI_MSIX_PBA && off < HVX_VPCI_MSIX_PBA + 64)
		return 0;			/* pending-bit array: computed as 0 (delivered promptly) */
	if (off < HVX_VPCI_NOTIFY) {		/* common cfg */
		switch (off) {
		case 0x00: return v->dfeat_sel;
		case 0x04:			/* device_feature for the selected dword */
			/* dword 0: MAC (bit 5) ONLY. Deliberately NOT VIRTIO_NET_F_STATUS: with
			 * STATUS the driver starts carrier-OFF and only brings the link up on a
			 * config-change interrupt (which this device does not generate), so the
			 * interface never becomes RUNNING and never answers ARP. Without STATUS
			 * virtio_net assumes the link is up (netif_carrier_on at probe).
			 * dword 1: VERSION_1 (bit 32). */
			if (v->dfeat_sel == 0) return (1u << VIRTIO_NET_F_MAC);
			if (v->dfeat_sel == 1) return 1u;	/* VIRTIO_F_VERSION_1 (bit 32) */
			return 0;
		case 0x08: return v->gfeat_sel;
		case 0x0c: return (v->gfeat_sel == 1) ? (u32)(v->gfeatures >> 32) : (u32)v->gfeatures;
		case 0x10: return v->config_msix_vector;	/* msix_config */
		case 0x12: return HVX_VNET_NQUEUES;		/* num_queues */
		case 0x14: return v->status & 0xff;		/* device_status */
		case 0x15: return 0;				/* config_generation */
		case 0x16: return v->qsel;			/* queue_select */
		case 0x18: return q->qsize;			/* queue_size */
		case 0x1a: return q->msix_vector;		/* queue_msix_vector */
		case 0x1c: return q->qready;			/* queue_enable */
		case 0x1e: return 0;				/* queue_notify_off */
		case 0x20: return (size == 8) ? q->desc_gpa  : (u32)q->desc_gpa;
		case 0x24: return (u32)(q->desc_gpa >> 32);
		case 0x28: return (size == 8) ? q->avail_gpa : (u32)q->avail_gpa;
		case 0x2c: return (u32)(q->avail_gpa >> 32);
		case 0x30: return (size == 8) ? q->used_gpa  : (u32)q->used_gpa;
		case 0x34: return (u32)(q->used_gpa >> 32);
		default:   return 0;
		}
	}
	if (off < HVX_VPCI_ISR)			/* notify: read side unused */
		return 0;
	if (off < HVX_VPCI_DEVICE) {		/* ISR status, read-to-clear */
		u32 val = v->isr;

		v->isr = 0;
		return val;
	}
	if (off < HVX_VPCI_MSIX_TABLE) {	/* device cfg: virtio_net_config {mac[6], status[2]} */
		u32 devoff = off - HVX_VPCI_DEVICE;

		if (devoff < 6) {
			u64 macv = 0;
			int i;

			for (i = 0; i < size && devoff + i < 6; i++)
				macv |= (u64)v->mac[devoff + i] << (8 * i);
			return macv;
		}
		if (devoff == 6)
			return VIRTIO_NET_S_LINK_UP;	/* status: link up */
		return 0;
	}
	return 0;
}

static void hvx_vnet_bar_write(struct hvx_vcpu *vcpu, u32 off, int size, u64 val)
{
	struct hvx_vnet *v = &vcpu->vm->vnet;
	struct hvx_vnet_queue *q = &v->q[v->qsel & (HVX_VNET_NQUEUES - 1)];

	(void)size;
	if (off >= HVX_VPCI_MSIX_TABLE && off < HVX_VPCI_MSIX_TABLE + HVX_MSIX_ENTRIES * 16) {
		u32 idx = (off - HVX_VPCI_MSIX_TABLE) / 16;
		u32 word = ((off - HVX_VPCI_MSIX_TABLE) % 16) / 4;

		v->msix_tbl[idx * 4 + word] = (u32)val;
		return;
	}
	if (off < HVX_VPCI_NOTIFY) {		/* common cfg */
		switch (off) {
		case 0x00: v->dfeat_sel = (u32)val; break;
		case 0x08: v->gfeat_sel = (u32)val; break;
		case 0x0c:
			if (v->gfeat_sel == 1)
				v->gfeatures = (v->gfeatures & 0xffffffffULL) | ((u64)(u32)val << 32);
			else
				v->gfeatures = (v->gfeatures & ~0xffffffffULL) | (u32)val;
			break;
		case 0x10: v->config_msix_vector = (u32)val & 0xffff; break;
		case 0x14:			/* device_status (0 = reset) */
			v->status = (u32)val & 0xff;
			if (v->status == 0) {
				int i;

				for (i = 0; i < HVX_VNET_NQUEUES; i++) {
					v->q[i].qready = 0; v->q[i].last_avail = 0;
					v->q[i].completion_armed = 0; v->q[i].irq_pending = 0;
					v->q[i].msix_vector = HVX_MSIX_NO_VECTOR;
				}
				v->isr = 0;
				v->config_msix_vector = HVX_MSIX_NO_VECTOR;
				/* rewind the TX capture so a replay re-captures identically;
				 * the inbound RX FIFO is NOT cleared (it is network data,
				 * independent of the driver reset). */
				v->tx_len = 0; v->tx_count = 0;
			}
			break;
		case 0x16: v->qsel = (u32)val; break;
		case 0x18: q->qsize = (u32)val; break;
		case 0x1a: q->msix_vector = (u32)val & 0xffff; break;
		case 0x1c: q->qready = (u32)val; if (val) q->last_avail = 0; break;
		case 0x20: q->desc_gpa  = (q->desc_gpa  & ~0xffffffffULL) | (u32)val; break;
		case 0x24: q->desc_gpa  = (q->desc_gpa  &  0xffffffffULL) | ((u64)(u32)val << 32); break;
		case 0x28: q->avail_gpa = (q->avail_gpa & ~0xffffffffULL) | (u32)val; break;
		case 0x2c: q->avail_gpa = (q->avail_gpa &  0xffffffffULL) | ((u64)(u32)val << 32); break;
		case 0x30: q->used_gpa  = (q->used_gpa  & ~0xffffffffULL) | (u32)val; break;
		case 0x34: q->used_gpa  = (q->used_gpa  &  0xffffffffULL) | ((u64)(u32)val << 32); break;
		default:   break;
		}
		return;
	}
	if (off < HVX_VPCI_ISR) {		/* notify: doorbell -- val = queue index */
		if ((u32)val == 0)
			/* Kick delivery: one-per-kick in PACED mode (a real OS kicks the deep
			 * receive ring once at open, so trickle frames across its NAPI refills
			 * -- byte-identical to M23's boot). A non-paced (hand-written) guest
			 * instead drains the whole batch at the kick, so the fault engine sees a
			 * multi-packet batch (DROP/REORDER/DUPLICATE) at once; with a single
			 * pushed frame (the M23 hand-written test) this delivers exactly one, so
			 * that fingerprint is preserved. */
			virtio_net_rx_deliver(vcpu, v->rx_paced ? 1 : 0);
		else if ((u32)val == 1)
			virtio_net_tx_process(vcpu);
		return;
	}
	/* ISR (RO from driver) and device cfg (RO) ignore writes */
}

/* Decode a MOV to/from the virtio-net BAR on an EPT violation (same MOV decoder as
 * the virtio-blk BAR; only the register callbacks differ). */
static bool hvx_vnet_mmio(struct hvx_vcpu *vcpu, u64 gpa, u32 insn_len)
{
	struct hvx_vm *vm = vcpu->vm;
	u32 off = (u32)(gpa - vnet_bar_base(&vm->vnet));
	u8 ib[16];
	int i = 0, regnum, size;
	u8 rex = 0, op;
	bool opsz16 = false, is_write;
	u64 *reg, val = 0;

	if (!fetch_guest_insn(vm, vcpu->regs.rip, ib, sizeof(ib)))
		return false;
	while (i < 15 && (ib[i] == 0x66 || ib[i] == 0x67 || ib[i] == 0xf0 ||
			  ib[i] == 0xf2 || ib[i] == 0xf3 || ib[i] == 0x2e ||
			  ib[i] == 0x36 || ib[i] == 0x3e || ib[i] == 0x26 ||
			  ib[i] == 0x64 || ib[i] == 0x65)) {
		if (ib[i] == 0x66)
			opsz16 = true;
		i++;
	}
	if (i < 15 && (ib[i] & 0xf0) == 0x40)
		rex = ib[i++];
	if (i >= 15)
		return false;
	op = ib[i++];
	if (i >= 16)
		return false;
	regnum = ((ib[i] >> 3) & 7) | ((rex & 4) ? 8 : 0);
	switch (op) {
	case 0x88: is_write = true;  size = 1; break;
	case 0x8a: is_write = false; size = 1; break;
	case 0x89: is_write = true;  size = (rex & 8) ? 8 : (opsz16 ? 2 : 4); break;
	case 0x8b: is_write = false; size = (rex & 8) ? 8 : (opsz16 ? 2 : 4); break;
	default:   return false;
	}
	reg = hvx_gpr(vcpu, regnum);

	if (is_write) {
		if (reg)
			val = *reg;
		if (size == 1)      val &= 0xff;
		else if (size == 2) val &= 0xffff;
		else if (size == 4) val &= 0xffffffff;
		hvx_vnet_bar_write(vcpu, off, size, val);
	} else {
		val = hvx_vnet_bar_read(vcpu, off, size);
		if (reg) {
			if (size == 1)      *reg = (*reg & ~0xffULL) | (val & 0xff);
			else if (size == 2) *reg = (*reg & ~0xffffULL) | (val & 0xffff);
			else if (size == 4) *reg = val & 0xffffffff;
			else                *reg = val;
		}
	}
	vcpu->regs.rip += insn_len;
	VMW(GUEST_RIP, vcpu->regs.rip);
	return true;
}

/*
 * virtio-net completion via MSI-X (milestone 23). Scans the two queues for a
 * pending completion (irq_pending) whose bound MSI-X table entry is enabled and
 * unmasked, and returns its message-data vector; *clear points at that queue's
 * irq_pending so the run-loop drops it on delivery (MSI is edge). Device IRQs
 * target the BSP (vCPU 0), like virtio-blk. Inert with no device.
 */
static bool vnet_msix_irq(struct hvx_vm *vm, u32 *vec, u32 **clear)
{
	struct hvx_vnet *v = &vm->vnet;
	int i;

	if (!v->present)
		return false;
	if (!(v->msix_control & 0x8000))		/* MSI-X disabled */
		return false;
	if (v->msix_control & 0x4000)			/* function-mask */
		return false;
	for (i = 0; i < HVX_VNET_NQUEUES; i++) {
		struct hvx_vnet_queue *q = &v->q[i];
		u32 idx = q->msix_vector, ctrl;

		if (!q->irq_pending)
			continue;
		if (idx >= HVX_MSIX_ENTRIES)		/* NO_VECTOR / out of range */
			continue;
		ctrl = v->msix_tbl[idx * 4 + 3];
		if (ctrl & 1)				/* this vector masked -> stay pending */
			continue;
		*vec = v->msix_tbl[idx * 4 + 2] & 0xff;
		*clear = &q->irq_pending;
		return true;
	}
	return false;
}

/*
 * virtio-blk completion IRQ, INTx routed through the IOAPIC (Phase B). Asserts
 * while a completion is pending (irq_pending), the transport is the PCI/INTx one,
 * and the guest has unmasked IOAPIC redirection entry HVX_VBLK_GSI with a real
 * vector. Level-like: the guest ISR acks by reading the virtio ISR register
 * (read-to-clear), which drops irq_pending. Inert with no device (irq_pending 0).
 */
static bool vblk_irq(struct hvx_vm *vm, u32 *vec)
{
	u32 rlo;

	if (!vm->vblk.irq_pending || vm->vblk.irq_mode != VBLK_IRQ_MODE_INTX)
		return false;
	if (vm->vblk.msix_control & 0x8000)		/* MSI-X enabled: INTx suppressed (M21) */
		return false;
	rlo = vm->ioapic.redir[HVX_VBLK_GSI * 2];	/* redirection entry low dword */
	if (rlo & (1U << 16))				/* masked in the IOAPIC */
		return false;
	if ((rlo & 0xff) < 0x20)			/* no real vector programmed yet */
		return false;
	*vec = rlo & 0xff;
	return true;
}

/*
 * virtio-blk completion via MSI-X (milestone 21) -- how REAL Linux takes the
 * interrupt. MSI is a direct, edge-triggered LAPIC delivery: when a completion is
 * pending, MSI-X is enabled + unmasked, and the queue has a valid table entry, the
 * vector is the entry's message-data byte and the destination is encoded in the
 * message address (0xFEE0_0xxx -> LAPIC id in bits 19:12). We inject that vector on
 * the running BSP (device IRQs target vCPU 0, as with INTx), at the deterministic
 * completion instruction. Returns the vector in *vec, or false if not deliverable
 * (which naturally holds the completion pending until the driver unmasks it -- the
 * MSI-X pending-bit-array semantics, checked again on the next entry). */
static bool vblk_msix_irq(struct hvx_vm *vm, u32 *vec)
{
	struct hvx_vblk *d = &vm->vblk;
	u32 idx, ctrl;

	if (!d->irq_pending)
		return false;
	if (!(d->msix_control & 0x8000))		/* MSI-X function disabled */
		return false;
	if (d->msix_control & 0x4000)			/* function-mask set: all vectors masked */
		return false;
	idx = d->queue_msix_vector;			/* table index the driver bound to queue 0 */
	if (idx >= HVX_MSIX_ENTRIES)			/* NO_VECTOR / out of range */
		return false;
	ctrl = d->msix_tbl[idx * 4 + 3];		/* vector_ctrl */
	if (ctrl & 1)					/* this vector masked -> stay pending (PBA) */
		return false;
	*vec = d->msix_tbl[idx * 4 + 2] & 0xff;		/* message data low byte = interrupt vector */
	return true;
}

/*
 * Level-triggered COM1 (IRQ4) interrupt (milestones 15/18). The 8250 driver is
 * interrupt-driven; it asserts IRQ4 for two causes, both routed through IOAPIC
 * redirection entry 4 (ISA IRQ4 -> INTIN4, per the MP table):
 *   - TX-empty (THRI, IER bit 1): the driver enables it when it has bytes to
 *     send. We transmit instantly, so THR is always empty -> assert while THRI on.
 *   - received-data-available (ERBFI, IER bit 0): assert while an input byte is
 *     queued in the RX ring (milestone 18 -- the receive path for interactivity).
 * Either way the entry must be unmasked with a real vector; until the guest has
 * programmed it we assert NOTHING (a spurious IRQ4 before the driver is ready
 * corrupts Linux's IRQ state). Recomputed every entry (level), so it clears the
 * moment the driver disables the enabling IER bit or the RX ring drains; the
 * interruptibility gate + LAPIC in-service prevent a delivery storm. Returns the
 * routed vector in *vec, or false if not asserting.
 */
static bool uart_irq(struct hvx_vm *vm, u32 *vec)
{
	bool tx = vm->uart.ier & 0x02;				/* THRI: TX-empty */
	bool rx = (vm->uart.ier & 0x01) && uart_rx_pending(vm);	/* ERBFI + data ready */
	u32 rlo;

	if (!tx && !rx)
		return false;
	rlo = vm->ioapic.redir[4 * 2];		/* IOAPIC redirection entry 4, low dword */
	if (rlo & (1U << 16))			/* masked in the IOAPIC */
		return false;
	if ((rlo & 0xff) < 0x20)		/* no real vector programmed yet */
		return false;
	*vec = rlo & 0xff;
	return true;
}

/*
 * The vCPU a device interrupt is delivered to. vCPU 0 (the boot CPU) always, except in
 * an SMP boot (HVX_VM_SMP_BOOT), where the physical destination APIC ID chosen by the
 * guest selects it: bits 63:56 of an IOAPIC redirection entry, bits 19:12 of an MSI
 * message address. A logical destination or an ID with no vCPU falls back to vCPU 0.
 */
static u32 hvx_ioapic_dest(const struct hvx_vm *vm, u32 gsi)
{
	u32 lo = vm->ioapic.redir[gsi * 2], id = vm->ioapic.redir[gsi * 2 + 1] >> 24;

	if (!vm->smp_boot || (lo & (1U << 11)) || id >= vm->n_vcpus)
		return 0;
	return id;
}

static u32 hvx_msi_dest(const struct hvx_vm *vm, u32 addr_lo)
{
	u32 id = (addr_lo >> 12) & 0xff;

	if (!vm->smp_boot || (addr_lo & (1U << 2)) || id >= vm->n_vcpus)
		return 0;
	return id;
}

/*
 * The asserted device interrupt for vCPU v, in the fixed priority order the one-vCPU
 * code always used: COM1 (IOAPIC 4), virtio-blk INTx (IOAPIC HVX_VBLK_GSI), virtio-blk
 * MSI-X, virtio-net MSI-X. A source counts only if it is routed to v. *msix / *vclear
 * say whether delivering it consumes an MSI edge (see the arming block).
 */
static bool hvx_dev_irq(struct hvx_vcpu *v, u32 *vec, bool *msix, u32 **vclear)
{
	struct hvx_vm *vm = v->vm;
	u32 idx;

	*msix = false;
	*vclear = NULL;
	if (!vm->smp_boot) {
		if (v->index != 0)
			return false;
		if (uart_irq(vm, vec) || vblk_irq(vm, vec))
			return true;
		if (vblk_msix_irq(vm, vec)) {
			*msix = true;
			return true;
		}
		return vnet_msix_irq(vm, vec, vclear);
	}
	if (uart_irq(vm, vec) && hvx_ioapic_dest(vm, 4) == v->index)
		return true;
	if (vblk_irq(vm, vec) && hvx_ioapic_dest(vm, HVX_VBLK_GSI) == v->index)
		return true;
	if (vblk_msix_irq(vm, vec)) {
		idx = vm->vblk.queue_msix_vector;
		if (hvx_msi_dest(vm, vm->vblk.msix_tbl[idx * 4]) == v->index) {
			*msix = true;
			return true;
		}
	}
	if (vnet_msix_irq(vm, vec, vclear)) {
		/* vnet_msix_irq picked the first deliverable queue; route by its entry */
		struct hvx_vnet_queue *q = container_of(*vclear, struct hvx_vnet_queue, irq_pending);

		if (hvx_msi_dest(vm, vm->vnet.msix_tbl[q->msix_vector * 4]) == v->index)
			return true;
		*vclear = NULL;
	}
	return false;
}

/*
 * Run the guest. Self-contained VMX-root critical section: VMXON, load the
 * VMCS, then a small run-loop that VMLAUNCHes and VMRESUMEs the guest, handling
 * exits IN-KERNEL where it can and returning the rest to userspace. VMXOFF on
 * the way out. Runs pinned with IRQs off.
 *
 * In-kernel-handled exits (do not return; VMRESUME):
 *   - RDTSC/RDTSCP: substitute the deterministic virtual TSC, skip the insn.
 *   - MTF (single_step only): one instruction retired; just count it.
 * Both advance the instruction clock (vm->icount). `max_insns`, when nonzero,
 * stops after that many instructions THIS call (reason HVX_EXIT_STEP, guest
 * alive). A safety cap on in-kernel iterations bounds the IRQs-off window.
 *
 * Fills *ex on the returned exit (and on VMfail/throttle/step). Returns 0 if
 * VMX-root operation itself succeeded, negative on a host-side VMX setup failure.
 */
/*
 * Seeded tick-timing jitter (HVX_SET_TJITTER, see hvx_abi.h). The delay of PMU tick
 * k is a pure function of the policy and k. The next tick goes on the nominal grid
 * point after the current target, plus that delay; delays are < period, so the grid
 * point is recovered from pmu_next alone and a restored snapshot continues the same
 * schedule without any extra saved state.
 */
/*
 * The index of the vCPU whose tick schedule this is, mixed into the jitter and
 * inserted-tick hashes so that vCPUs with their own clocks (several vCPUs driven
 * from userspace) do not all get the same pattern. It enters the hash as
 * idx * HVX_CLK_IDX_SALT, i.e. not at all for vCPU 0, so one-vCPU guests are
 * unchanged. With the in-kernel scheduler the VM has ONE tick grid, owned by
 * whichever vCPU runs, so the index is 0 there.
 */
#define HVX_CLK_IDX_SALT 0xbf58476d1ce4e5b9ULL
static inline u64 hvx_clk_idx(const struct hvx_vm *vm, const struct hvx_vcpu *v)
{
	return vm->sched_on ? 0 : v->index;
}

static u64 hvx_tj_delay(const struct hvx_vm *vm, u64 k, u64 idx)
{
	const struct hvx_tjitter *t = &vm->tj;
	u64 h, d;
	u32 i;

	if (k <= 1)
		return 0;
	if (t->n_explicit) {
		for (i = 0; i < t->n_explicit; i++)
			if (t->explicit_[i].tick == k)
				break;
		d = (i < t->n_explicit) ? t->explicit_[i].delay : 0;
	} else {
		if (k < t->tick_lo || (t->tick_hi && k >= t->tick_hi))
			return 0;
		h = fault_mix(t->seed + k * 0x9e3779b97f4a7c15ULL + idx * HVX_CLK_IDX_SALT);
		if (h % 1000 >= t->permille)
			return 0;
		d = 1 + (h >> 16) % t->max_delay;
	}
	return d < vm->pmu_period ? d : vm->pmu_period - 1;	/* SET_PMU may shrink the period later */
}

static void hvx_tj_next(struct hvx_vm *vm, struct hvx_vcpu *vcpu)
{
	u64 p = vm->pmu_period;
	u64 base = vcpu->pmu_next - vcpu->pmu_next % p + p;
	u64 k = base / p, d = hvx_tj_delay(vm, k, hvx_clk_idx(vm, vcpu));

	vcpu->pmu_next = base + d;
	vcpu->tj_last = k;
	if (d) {
		vcpu->tj_applied++;
		vcpu->tj_hash = (((vcpu->tj_hash ^ k) * 0x100000001b3ULL) ^ d) * 0x100000001b3ULL;
	}
}

/*
 * Inserted ticks (HVX_SET_XTICK, see hvx_abi.h). With them on, the tick targets of
 * period j (branch counts in [j*p, (j+1)*p)) are its grid tick j*p + d_j (d_j the tick
 * jitter, 0 when off) plus its extra ticks; the next target is the smallest one above
 * the current target, looked up in the current period and the next one (which always
 * has its grid tick). So, as with the jitter alone, pmu_next is enough to continue the
 * schedule after a restore.
 */
static u64 hvx_xt_main(const struct hvx_vm *vm, u64 j, u64 idx)
{
	return j * vm->pmu_period + (vm->tj_on ? hvx_tj_delay(vm, j, idx) : 0);
}

static bool hvx_xt_window(const struct hvx_xtick *x, u64 j)
{
	return j >= 2 && j >= x->lo && (!x->hi || j < x->hi);
}

/* smallest extra tick of period j above t, or U64_MAX */
static u64 hvx_xt_extra_after(const struct hvx_vm *vm, u64 j, u64 t, u64 idx)
{
	const struct hvx_xtick *x = &vm->xt;
	u64 p = vm->pmu_period, best = U64_MAX, h, e;
	u32 i;

	if (x->n_explicit) {
		for (i = 0; i < x->n_explicit; i++) {
			e = x->explicit_[i];
			if (e / p == j && e > t && e < best)
				best = e;
		}
		return best;
	}
	if (!hvx_xt_window(x, j))
		return U64_MAX;
	for (i = 0; i < x->rate; i++) {
		h = fault_mix(x->seed + j * 0x9e3779b97f4a7c15ULL + (u64)(i + 1) * 0xd1b54a32d192ed03ULL +
			      idx * HVX_CLK_IDX_SALT);
		if (h % 1000 >= x->permille)
			continue;
		e = j * p + 1 + (h >> 16) % (p - 1);
		if (e > t && e < best)
			best = e;
	}
	return best;
}

/* Advance pmu_next past the tick just taken; returns true if that tick was an extra one. */
static bool hvx_xt_next(struct hvx_vm *vm, struct hvx_vcpu *vcpu)
{
	u64 p = vm->pmu_period, t = vcpu->pmu_next, j = t / p, m, best, e;
	u64 idx = hvx_clk_idx(vm, vcpu);
	bool extra = t != hvx_xt_main(vm, j, idx);

	best = U64_MAX;
	m = hvx_xt_main(vm, j, idx);
	if (m > t)
		best = m;
	e = hvx_xt_extra_after(vm, j, t, idx);
	if (e < best)
		best = e;
	m = hvx_xt_main(vm, j + 1, idx);		/* > t: every target of period j is below (j+1)*p */
	if (m < best)
		best = m;
	e = hvx_xt_extra_after(vm, j + 1, t, idx);
	if (e < best)
		best = e;
	vcpu->pmu_next = best;
	if (best == hvx_xt_main(vm, best / p, idx) && vm->tj_on) {	/* jitter statistics, as hvx_tj_next */
		u64 k = best / p, d = best - k * p;

		vcpu->tj_last = k;
		if (d) {
			vcpu->tj_applied++;
			vcpu->tj_hash = (((vcpu->tj_hash ^ k) * 0x100000001b3ULL) ^ d) * 0x100000001b3ULL;
		}
	}
	if (extra) {
		if (!vcpu->xt_applied)
			vcpu->xt_first = t;
		vcpu->xt_last = t;
		vcpu->xt_applied++;
		vcpu->xt_hash = (vcpu->xt_hash ^ t) * 0x100000001b3ULL;
	}
	return extra;
}

/*
 * The LAPIC TSC-deadline timer of one vCPU: once the virtual TSC has reached the
 * deadline, turn it into a pending one-shot of the LVT-timer vector. Called at the
 * run-loop top (for the running vCPU) and by the scheduler (for every vCPU).
 */
static void hvx_lapic_timer_due(struct hvx_vcpu *vcpu)
{
	u64 vtsc = (vcpu->tsc_mode == HVX_TSC_ICOUNT)
		 ? vcpu->tsc + vcpu->icount * vcpu->tsc_step : vcpu->tsc;

	if (vcpu->lapic.tsc_deadline && vtsc >= vcpu->lapic.tsc_deadline) {
		u32 lvt = vcpu->lapic.reg[X2APIC_LVT_TIMER - X2APIC_BASE_MSR];

		vcpu->lapic.tsc_deadline = 0;
		if (!(lvt & LVT_MASKED))
			hvx_oneshot_post(vcpu, lvt);
	}
}

/*
 * Device events that come due on the instruction clock (all routed to vCPU 0, so
 * every block is a no-op for any other vCPU). Called at the run-loop top and by the
 * scheduler's all-halted clock jump.
 */
static void hvx_dev_due(struct hvx_vcpu *vcpu)
{
	struct hvx_vm *vm = vcpu->vm;
	/* SMP boot: the VM has one clock, so device events come due on whichever vCPU
	 * runs (vCPU 0's copy of the clock is stale while another vCPU runs) */
	const bool here = vcpu->index == 0 || vm->smp_boot;

	/*
	 * virtio-blk deterministic completion (milestone 20). A kick scheduled
	 * the completion at completion_due = kick_icount + LATENCY (set in a
	 * retired IO/MMIO exit, so a pure function of icount). Checked here at the
	 * loop top like the LAPIC timer, so a due completion becomes an injection
	 * before the next entry; a host-interrupt iteration never advances icount,
	 * so it fires at a run-independent instruction. Routed to the BSP (vCPU 0),
	 * matching the device-IRQ-on-BSP rule. Phase A (direct) turns it into a
	 * one-shot M8 injection; Phase B leaves irq_pending set for the IOAPIC-
	 * routed level source (vblk_irq) to deliver.
	 */
	if (here && vm->vblk.completion_armed &&
	    vcpu->icount >= vm->vblk.completion_due) {
		vm->vblk.completion_armed = 0;
		vm->vblk.irq_pending = 1;
		if (vm->vblk.irq_mode == VBLK_IRQ_MODE_DIRECT) {
			/* Phase A: a fire-and-forget one-shot delivered directly (the
			 * test's ISR does not EOI), so -- exactly like HVX_INJECT -- it
			 * is NOT gated on a prior EOI. Clear in_service so a completion
			 * from a previous run/replay cannot block this one. Always to
			 * vCPU 0 (the running one, except in an SMP boot). */
			struct hvx_vcpu *t = &vm->vcpu[0];

			t->inject_vector = vm->vblk.irq_vec & 0xff;
			t->inject_at_icount = vcpu->icount;
			t->inject_valid = 1;
			t->lapic.in_service = 0;
			vm->vblk.irq_pending = 0;	/* consumed by the one-shot */
		}
	}

	/*
	 * virtio-net PACED RX delivery (milestone 23 stretch). When paced (a real
	 * OS), trickle one pending scripted frame per HVX_VNET_RX_PACE instructions
	 * against the already-posted receive buffers -- a deterministic icount
	 * cadence (the condition is a pure function of the instruction clock, so two
	 * runs deliver at the identical instruction). This lets a later frame arrive
	 * after the interface is fully up. OFF by default (kick-triggered only).
	 */
	if (here && vm->vnet.present && vm->vnet.rx_paced &&
	    vm->vnet.rx_tail != vm->vnet.rx_head && vm->vnet.q[0].qready &&
	    vcpu->icount >= vm->vnet.rx_next_deliver) {
		virtio_net_rx_deliver(vcpu, 1);	/* one FIFO frame per paced interval */
		vm->vnet.rx_next_deliver = vcpu->icount + HVX_VNET_RX_PACE;
	}

	/*
	 * virtio-net deterministic completion (milestone 23). Each queue's
	 * completion was armed at its kick (completion_due = kick_icount +
	 * HVX_VNET_LATENCY, in the retired MMIO exit), so it is a pure function of
	 * icount. When due, latch irq_pending for the MSI-X source (vnet_msix_irq)
	 * to deliver. Same loop-top / retired-mutation split as virtio-blk (M16
	 * creep discipline). Device IRQs on the BSP (vCPU 0).
	 */
	if (here && vm->vnet.present) {
		int qi;

		for (qi = 0; qi < HVX_VNET_NQUEUES; qi++)
			if (vm->vnet.q[qi].completion_armed &&
			    vcpu->icount >= vm->vnet.q[qi].completion_due) {
				vm->vnet.q[qi].completion_armed = 0;
				vm->vnet.q[qi].irq_pending = 1;
			}
	}
}

/*
 * In-kernel scheduler: stops (THROTTLE / max_insns) noticed in the run loop wait in
 * vm->sched_stop for a clean boundary.
 */
#define HVX_SCHED_STOP_THROTTLE	1U
#define HVX_SCHED_STOP_STEP	2U
#define HVX_SCHED_QUANTUM	(1ULL << 20)	/* default turn length (guest branches) */

/*
 * Would the run loop deliver an interrupt to this vCPU at its next entry? (IF=1,
 * nothing in service, and a due one-shot -- timer / IPI / injection -- or, for
 * vCPU 0, an asserted device source.) The scheduler wakes a halted vCPU with it.
 */
static bool hvx_vcpu_irq_ready(struct hvx_vcpu *v)
{
	u32 vec, *clr;
	bool msix;

	if (!(v->regs.rflags & 0x200) || v->lapic.in_service)
		return false;
	if (v->inject_valid && v->icount >= v->inject_at_icount)
		return true;
	return hvx_dev_irq(v, &vec, &msix, &clr);
}

static int hvx_run_vcpu(struct hvx_vcpu *vcpu, struct hvx_exit *ex,
			bool single_step, u64 max_insns)
{
	struct hvx_vm *vm = vcpu->vm;	/* shared: eptp, UART/IOAPIC/PIC/console/input */
	u64 vmxon_pa = __pa((void *)vcpu->vmxon_page);
	u64 vmcs_pa = __pa((void *)vcpu->vmcs_page);
	unsigned long flags;
	unsigned int guard = vcpu->carry_guard;	/* retired guest exits: deterministic THROTTLE cadence */
	unsigned int safety = 0;	/* all in-kernel iterations: pathological-storm cap */
	unsigned int streak = 0;	/* consecutive non-retired exits: host-preempt bound */
	u64 stepped = vcpu->carry_stepped;
	long resume = 0;
	int ret = 0;
	/* In-kernel vCPU scheduler (HVX_VM_SMP_SCHED): this call runs one turn (or the
	 * rest of one). Every sched-only path below is guarded by this flag, so a VM
	 * without the scheduler runs exactly the code it ran before. */
	const bool sched = vm->sched_on;
	/* host copies of the guest-owned SYSCALL / KERNEL_GS_BASE MSRs (M15). These
	 * MSRs are NOT in the VMCS and SYSCALL/swapgs do NOT VM-exit, so the guest
	 * would otherwise run them against the host's values. We load the guest's
	 * before entry and restore the host's after -- read up-front so the restore
	 * at out_clear is always valid even if setup fails before the load. */
	u64 host_star = rd(GMSR_STAR), host_lstar = rd(GMSR_LSTAR);
	u64 host_cstar = rd(GMSR_CSTAR), host_sfmask = rd(GMSR_SFMASK);
	u64 host_kgs = rd(GMSR_KERNEL_GS_BASE);
	bool msrs_loaded = false;
	u32 pcc = 0;			/* PROC_EXEC_CTL as last written (saves two VMREADs per entry) */
	u64 pmu_h_gctl = 0, pmu_h_sel = 0, pmu_h_pmc = 0;	/* host PMU state, restored at exit */
	u32 pmu_h_lvtpc = 0;
	u64 pt_h[9];			/* host RTIT state, restored at the end of the run */
	bool pt_live = false;
	u64 t0 = ktime_get_ns();	/* DIAGNOSTIC: wall-time accounting (fingerprint-neutral) */
	struct hvx_ftstat_vcpu *fts = &vcpu->fts;	/* DIAGNOSTIC: turn-end cost (FASTTURN.md) */
	u64 ft_c0 = rdtsc(), ft_c = ft_c0, ft_n, ft_hw = 0;
	/* guest AVX (HVX_VM_X86_64_V3): host XCR0, swapped with the guest's around entries */
	const bool xs = vm->cpu_v3;
	u64 host_xcr0 = 0;

	*(u32 *)vcpu->vmxon_page = hvx_rev;
	*(u32 *)vcpu->vmcs_page = hvx_rev;

	/*
	 * Milestone 14: a real guest (Linux) uses SSE/FPU very early. The guest
	 * clobbers the live XMM/FPU registers, which otherwise belong to the host
	 * task interrupted by this ioctl. kernel_fpu_begin() saves and takes over
	 * the FPU for the duration of the run; kernel_fpu_end() (below) restores
	 * the host state. It also disables preemption itself, so it must bracket
	 * the whole VMX-root section. No host FPU code runs inside the run-loop
	 * (the in-kernel exit handlers are pure integer), so the guest's XMM state
	 * persists across VMRESUMEs and is simply discarded on the final exit.
	 */
	kernel_fpu_begin();
	preempt_disable();
	local_irq_save(flags);
	ft_n = rdtsc(); fts->fpu_cyc += ft_n - ft_c; ft_c = ft_n;
	if (xs) {
		host_xcr0 = xgetbv(XCR_XFEATURE_ENABLED_MASK);
		if ((host_xcr0 & HVX_XS_MASK) != HVX_XS_MASK || !hvx_xcr0_ok(vcpu->xcr0)) {
			ret = -EIO;	/* never XRSTOR/XSETBV what the host cannot take */
			goto out_cr4;
		}
	}

	hvx_set_cr4(hvx_cr4() | CR4_VMXE);
	if (vmxon(vmxon_pa)) {
		pr_err("hvx: VMXON failed\n");
		ret = -EIO;
		goto out_cr4;
	}
	if (vmclear(vmcs_pa)) { pr_err("hvx: VMCLEAR failed\n"); ret = -EIO; goto out_vmxoff; }
	if (vmptrld(vmcs_pa)) { pr_err("hvx: VMPTRLD failed\n"); ret = -EIO; goto out_vmxoff; }
	ft_n = rdtsc(); fts->vmxon_cyc += ft_n - ft_c; ft_c = ft_n;

	wr_err = 0;
	/* PMU branch clock: take over GP counter 0 for the run (host counters off --
	 * the VMCS keeps IA32_PERF_GLOBAL_CTRL at 0 in root mode). Not combined with
	 * the MTF instruction clock, which already sees every instruction. */
	vcpu->pmu_live = vm->pmu_clock && hvx_pmu_ok && !single_step;
	if (vcpu->pmu_live) {
		pmu_h_gctl = rd(MSR_PERF_GLOBAL_CTRL);
		pmu_h_sel = rd(MSR_PERFEVTSEL0);
		pmu_h_pmc = rd(MSR_PMC0);
		pmu_h_lvtpc = hvx_lvtpc_read();
		wrmsrl(MSR_PERF_GLOBAL_CTRL, 0);
		wrmsrl(MSR_PERFEVTSEL0, 0);
		wrmsrl(MSR_PERF_GLOBAL_OVF, 1);			/* drop a stale PMC0 overflow */
		hvx_lvtpc_write(HVX_PMU_LVT_VECTOR);		/* fixed vector, unmasked */
		/* M156: with the guard the event is enabled by each VM entry and disabled by
		 * each exit (setup_vmcs), so it stays 0 in root mode */
		vcpu->pmu_guarded = !!pmu_guard;
		wrmsrl(MSR_PERFEVTSEL0, vcpu->pmu_guarded ? 0 : HVX_PMU_EVTSEL);
		if (!vcpu->pmu_next)
			vcpu->pmu_next = vm->pmu_period;
		vcpu->pmu_carry = false;	/* the first entry programs PMC0 */
	}
	/* Intel PT: take the trace MSRs for this run. Every RTIT MSR except RTIT_CTL can
	 * only be written with TraceEn clear, which it is (the host's is cleared first;
	 * the guest's is only set by hvx_vmx_run_pt around each entry). */
	if (vm->pt_on && vm->pt_chunk) {
		int k;

		pt_live = true;
		pt_h[0] = rd(MSR_RTIT_CTL);
		if (pt_h[0] & HVX_RTIT_TRACEEN)
			wrmsrl(MSR_RTIT_CTL, pt_h[0] & ~HVX_RTIT_TRACEEN);
		pt_h[1] = rd(MSR_RTIT_STATUS);
		pt_h[2] = rd(MSR_RTIT_OUTPUT_BASE);
		pt_h[3] = rd(MSR_RTIT_OUTPUT_MASK);
		pt_h[4] = rd(MSR_RTIT_CR3_MATCH);
		for (k = 0; k < 4; k++)
			pt_h[5 + k] = rd(MSR_RTIT_ADDR0_A + k);
		wrmsrl(MSR_RTIT_CTL, 0);
		wrmsrl(MSR_RTIT_OUTPUT_BASE, page_to_phys(vm->pt_topa));
		wrmsrl(MSR_RTIT_OUTPUT_MASK, vm->pt_mask);
		wrmsrl(MSR_RTIT_STATUS, vm->pt_status);
		wrmsrl(MSR_RTIT_CR3_MATCH, vm->pt_cr3);
		for (k = 0; k < 2; k++) {
			wrmsrl(MSR_RTIT_ADDR0_A + 2 * k, vm->pt_lo[k]);
			wrmsrl(MSR_RTIT_ADDR0_A + 2 * k + 1, vm->pt_hi[k] ? vm->pt_hi[k] - 1 : 0);
		}
	}
	ft_n = rdtsc(); fts->pmu_cyc += ft_n - ft_c; ft_c = ft_n;
	if (setup_vmcs(vcpu, single_step)) { ret = -EIO; goto out_clear; }
	if (wr_err) { ret = -EIO; goto out_clear; }

	invept_flush(vm->eptp);
	pcc = (u32)vmread(PROC_EXEC_CTL);
	ft_n = rdtsc(); fts->vmcs_cyc += ft_n - ft_c; ft_c = ft_n;

	/* Load the guest's SYSCALL / KERNEL_GS_BASE MSRs into the physical MSRs so
	 * guest SYSCALL and swapgs use guest values. The host does not execute
	 * SYSCALL or swapgs inside this IRQ-off VMX section, so this is safe until
	 * we restore the host's at out_clear. wrmsrl_safe: a guest may hold a
	 * non-canonical LSTAR/CSTAR/KERNEL_GS_BASE (e.g. hvx_cpuid_test's scratch
	 * value), and an unsafe wrmsr of that would #GP and crash the host. */
	wrmsrl_safe(GMSR_STAR,   vcpu->sw_msr[SWMSR_STAR]);
	wrmsrl_safe(GMSR_LSTAR,  vcpu->sw_msr[SWMSR_LSTAR]);
	wrmsrl_safe(GMSR_CSTAR,  vcpu->sw_msr[SWMSR_CSTAR]);
	wrmsrl_safe(GMSR_SFMASK, vcpu->sw_msr[SWMSR_SFMASK]);
	wrmsrl_safe(GMSR_KERNEL_GS_BASE, vcpu->sw_msr[SWMSR_KERNEL_GS_BASE]);
	msrs_loaded = true;
	/* load the guest's x87/SSE registers (the host's were saved by
	 * kernel_fpu_begin above); saved back at out_clear */
	if (xs)
		hvx_xs_load(vcpu);	/* x87/SSE/AVX (XSAVE area) */
	else
		asm volatile("fxrstor64 (%0)" :: "r" ((void *)vcpu->fx_page) : "memory");
	ft_n = rdtsc(); fts->load_cyc += ft_n - ft_c; ft_c = ft_n;

	for (;;) {
		u32 reason, basic;
		long ok;

		/*
		 * PMU branch clock tick. Done HERE, at the loop top, before the timer
		 * check below: the tick moves the virtual TSC, and a deadline it crosses
		 * must turn into an injection on this very entry. (Processed after the
		 * check, the injection would wait for the next exit, which can be a
		 * host interrupt -- a nondeterministic point.)
		 */
		if (vcpu->pmu_live) {
			/*
			 * A tick is due once the branch count reaches the target. Reached
			 * while single-stepping (or not past it), the landing point is the
			 * first instruction boundary after the target-th branch retired --
			 * exact. Past it without stepping means the overflow skid exceeded
			 * the margin: counted as an overshoot (must stay 0).
			 */
			while (vcpu->pmu_total >= vcpu->pmu_next) {
				if (vcpu->pmu_total > vcpu->pmu_next || !vcpu->pmu_step)
					vcpu->pmu_over++;
				bool xtra = false;

				if (vm->xt_on)
					xtra = hvx_xt_next(vm, vcpu);	/* grid (+ jitter) + inserted ticks */
				else if (vm->tj_on)
					hvx_tj_next(vm, vcpu);	/* nominal grid + seeded delay */
				else
					vcpu->pmu_next += vm->pmu_period;
				vcpu->pmu_ticks++;
				vcpu->pmu_land = (vcpu->pmu_land ^ vcpu->regs.rip) * 0x100000001b3ULL;
				vcpu->pmu_land = (vcpu->pmu_land ^ vcpu->regs.rcx) * 0x100000001b3ULL;
				vcpu->pmu_land = (vcpu->pmu_land ^ vcpu->regs.rflags) * 0x100000001b3ULL;
				vcpu->tsc += (xtra && vm->xt.tsc) ? vm->xt.tsc : vm->pmu_tick_tsc;
				vcpu->icount += HVX_PMU_TICK_ICOUNT;
				stepped++;
				streak = 0;
				if (sched) {
					/* stops wait for a clean boundary (checked below) */
					if (max_insns && stepped >= max_insns)
						vm->sched_stop |= HVX_SCHED_STOP_STEP;
					guard += HVX_PMU_TICK_GUARD;
					if (guard >= HVX_MAX_INKERNEL_EXITS)
						vm->sched_stop |= HVX_SCHED_STOP_THROTTLE;
					continue;
				}
				if (max_insns && stepped >= max_insns) {
					ex->reason = HVX_EXIT_STEP;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
				/* A tick stands for ~2 ms of guest time, so it weighs more on
				 * the THROTTLE cadence than one retired exit: a guest that only
				 * ticks (pure computation) still returns to userspace about
				 * every 256 ticks instead of every 65536. */
				guard += HVX_PMU_TICK_GUARD - 1;
				if (++guard >= HVX_MAX_INKERNEL_EXITS) {
					ex->reason = HVX_EXIT_THROTTLE;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
			}
		}

		/*
		 * In-kernel scheduler: end the turn once the VM-wide branch count reaches
		 * turn_end (the landing above stops exactly there), and take a pending
		 * THROTTLE / max_insns stop -- but only at a CLEAN boundary: setup_vmcs
		 * does not carry an STI / MOV-SS shadow into the next entry, and a half-
		 * delivered event must be finished first. Until then the PMU block below
		 * single-steps. Checked before any injection is armed for this entry.
		 */
		if (sched) {
			struct hvx_sched *sc = &vm->sched;

			if (!sc->turn_over && vcpu->pmu_total >= sc->turn_end) {
				if (vcpu->pmu_total > sc->turn_end || !vcpu->pmu_step)
					sc->overshoots++;
				sc->turn_over = 1;
			}
			if (sc->turn_over || vm->sched_stop) {
				if (!vcpu->reinj_valid &&
				    !((u32)vmread(GUEST_INTERRUPTIBILITY) & 3)) {
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					if (vm->sched_stop)
						ex->reason = (vm->sched_stop & HVX_SCHED_STOP_THROTTLE)
							   ? HVX_EXIT_THROTTLE : HVX_EXIT_STEP;
					else
						ex->reason = HVX_EXIT_TURN;
					goto out_clear;
				}
				sc->deferred++;
			}
		}

		/*
		 * LAPIC TSC-deadline timer (milestone 15). The virtual TSC is a
		 * function of the instruction clock in ICOUNT mode; in the free-run
		 * COUNTER mode used to bring the device model up it creeps forward once
		 * per RETIRED guest exit while a deadline is armed (see the retired
		 * block below), so time advances even between RDTSCs yet stays a pure
		 * function of icount. When the deadline is reached, turn it into a
		 * pending injection of the LVT-timer vector (one-shot -- Linux re-arms
		 * the next deadline in its handler; periodic falls out for free).
		 *
		 * The check (not the creep) is done here at the top so a deadline that
		 * has come due is turned into an injection before the next entry.
		 */
		hvx_lapic_timer_due(vcpu);

		hvx_dev_due(vcpu);

		/*
		 * Pick the interrupt to deliver this entry: a due one-shot event
		 * (timer / ICR / self-IPI, latched in vm->inject_*) takes priority
		 * over the level-triggered UART TX IRQ (recomputed each entry).
		 */
		{
			bool oneshot = vcpu->inject_valid && vcpu->icount >= vcpu->inject_at_icount;
			u32 vec = 0;
			bool pend = false;
			bool via_msix = false;	/* MSI is edge: consumed (irq_pending cleared) on delivery */
			u32 *vnet_clear = NULL;	/* virtio-net MSI edge: the queue's irq_pending to drop */

			/*
			 * At most one interrupt in service at a time (minimal LAPIC ISR):
			 * deliver nothing new until the guest EOIs the current one. This
			 * mirrors LAPIC arbitration -- delivering a device vector directly
			 * (bypassing the LAPIC) while another handler runs corrupts Linux's
			 * IRQ state. A due one-shot (timer/IPI) outranks the level UART IRQ.
			 *
			 * The UART is a single shared device; its level IRQ is routed only
			 * to the BSP (vCPU 0), matching the boot's COM1-on-BSP wiring and
			 * keeping device-IRQ targeting deterministic under SMP. A one-shot
			 * (LAPIC timer / cross-vCPU IPI) is per-vCPU and applies to whichever
			 * vCPU is running.
			 */
			if (vcpu->lapic.in_service == 0) {
				if (oneshot) {
					vec = vcpu->inject_vector;
					pend = true;
				} else if (hvx_dev_irq(vcpu, &vec, &via_msix, &vnet_clear)) {
					/* COM1 / virtio-blk INTx via the IOAPIC (M15/M20), virtio-blk
					 * MSI-X (M21), virtio-net MSI-X (M23): vCPU 0, or in an SMP
					 * boot the vCPU the guest routed it to */
					pend = true;
				}
			}

			/*
			 * Gate on guest interruptibility. VM-entry injection of an
			 * external interrupt requires RFLAGS.IF=1 and no STI/MOV-SS
			 * shadow, else the entry fails. If not yet interruptible,
			 * request interrupt-window exiting and retry when the window
			 * opens (EXIT_REASON_INTERRUPT_WINDOW). One-shot events persist
			 * in vm->inject_* across THROTTLE re-entries; the UART level
			 * source simply re-asserts.
			 */
			if (pend && !vcpu->reinj_valid) {
				u64 rflags = vmread(GUEST_RFLAGS);
				u32 istate = (u32)vmread(GUEST_INTERRUPTIBILITY);
				u32 pc = pcc;

				if ((rflags & (1ULL << 9)) && !(istate & 0x3)) {
					VMW(VM_ENTRY_INTR_INFO,
					    0x80000000U | (vec & 0xff)); /* valid, type 0 */
					vcpu->lapic.in_service = vec | 0x10000;	/* nonzero until EOI */
					if (oneshot) {
						vcpu->inject_valid = 0;
						hvx_oneshot_refill(vcpu);	/* next pending vector, if any */
					}
					if (via_msix)
						vm->vblk.irq_pending = 0;	/* MSI edge consumed (M21) */
					if (vnet_clear)
						*vnet_clear = 0;		/* virtio-net MSI edge consumed (M23) */
					if (pc & PROCBASED_INTR_WINDOW) {
						pcc = pc & ~PROCBASED_INTR_WINDOW;
						VMW(PROC_EXEC_CTL, pcc);
					}
				} else {
					VMW(VM_ENTRY_INTR_INFO, 0);
					pcc = pc | PROCBASED_INTR_WINDOW;
					VMW(PROC_EXEC_CTL, pcc);
				}
			} else {
				VMW(VM_ENTRY_INTR_INFO, 0);
				if (pcc & PROCBASED_INTR_WINDOW) {
					pcc &= ~PROCBASED_INTR_WINDOW;
					VMW(PROC_EXEC_CTL, pcc);
				}
			}
		}

		if (vcpu->pmu_live) {
			u64 rem, n, tgt = vcpu->pmu_next, margin = HVX_PMU_MARGIN;
			bool step, force = false;

			if (sched) {
				/* land on whichever comes first: the next tick or the turn's
				 * end; once the end (or a stop) is due, step to a clean boundary */
				if (vm->sched.turn_over || vm->sched_stop)
					force = true;
				else if (vm->sched.turn_end < tgt) {
					tgt = vm->sched.turn_end;
					margin = vm->turn_margin;	/* see HVX_SET_TURNCTL */
				}
			}
			rem = tgt - vcpu->pmu_total;
			vcpu->fts_tgt_turn = force || tgt != vcpu->pmu_next;	/* DIAGNOSTIC */
			step = force || rem <= margin;
			n = step ? 0x40000000ULL : rem - margin;
			if (n > 0x40000000ULL)
				n = 0x40000000ULL;
			if (!force && step && vcpu->pmu_flat >= HVX_PMU_HOP_STEPS && rem >= HVX_PMU_HOP_MIN) {
				step = false;		/* hop over a branch-free stretch */
				n = 1;
				vcpu->pmu_flat = 0;
				vcpu->pmu_hops++;
			}
			vcpu->pmu_n = n;
			vcpu->pmu_step = step;
			/*
			 * PMC0 = -n (a legacy PMC write sign-extends bit 31 to bit 47). A
			 * landing step after a landing step keeps the counter running from
			 * where the previous step left it -- far from overflow -- instead
			 * of re-programming it: WRMSR is serializing and was a large part
			 * of a step's cost. The branch count read at the exit is the same
			 * either way: got = PMC0(exit) - PMC0(entry).
			 */
			if (step && vcpu->pmu_carry && (ft_opt & 1) &&
			    ((vcpu->pmu_cur - ((0ULL - 0x40000000ULL) & HVX_PMU_MASK)) & HVX_PMU_MASK)
			    < 0x20000000ULL) {
				vcpu->pmu_base = vcpu->pmu_cur;
			} else {
				wrmsrl(MSR_PMC0, (u64)(u32)(0U - (u32)n));	/* sign-extended: -n */
				vcpu->pmu_base = (0ULL - n) & HVX_PMU_MASK;
			}
			if (step != !!(pcc & PROCBASED_MONITOR_TRAP)) {
				pcc = step ? (pcc | PROCBASED_MONITOR_TRAP) : (pcc & ~PROCBASED_MONITOR_TRAP);
				VMW(PROC_EXEC_CTL, pcc);
			}
		}

		if (vm->smp_boot) {
			/* An AP switches from real mode to long mode without VM exits (CR0 is
			 * guest-owned), so keep the IA-32e-mode entry control equal to the
			 * guest's EFER.LMA, which VM entry checks. */
			u32 ec = (u32)vmread(VM_ENTRY_CTL);
			bool lma = !!(vmread(GUEST_IA32_EFER) & EFER_LMA_);

			if (!!(ec & ENTRY_IA32E_MODE) != lma)
				VMW(VM_ENTRY_CTL, lma ? (ec | ENTRY_IA32E_MODE) : (ec & ~ENTRY_IA32E_MODE));
		}
		if (vcpu->reinj_valid) {
			/* finish the interrupted event delivery first */
			VMW(VM_ENTRY_INTR_INFO, vcpu->reinj_info);
			if (vcpu->reinj_info & 0x800)
				VMW(VM_ENTRY_EXC_EC, vcpu->reinj_ec);
			VMW(VM_ENTRY_INSN_LEN, vcpu->reinj_len);
			vcpu->reinj_valid = false;
		}
		if (chaos_ptimer)
			VMW(VMX_PREEMPT_TIMER, 1 + get_random_u32() % chaos_ptimer);
		asm volatile("mov %0, %%cr2" :: "r" (vcpu->cr2));
		if (xs && vcpu->xcr0 != host_xcr0)
			xsetbv(XCR_XFEATURE_ENABLED_MASK, vcpu->xcr0);	/* validated */
		ft_hw = rdtsc();	/* DIAGNOSTIC */
		ok = pt_live ? hvx_vmx_run_pt(&vcpu->regs, resume, vm->pt_ctl)
			     : hvx_vmx_run(&vcpu->regs, resume);
		ft_hw = rdtsc() - ft_hw;
		if (xs && vcpu->xcr0 != host_xcr0)
			xsetbv(XCR_XFEATURE_ENABLED_MASK, host_xcr0);
		asm volatile("mov %%cr2, %0" : "=r" (vcpu->cr2));
		if (ok) {
			u32 iv = (u32)vmread(IDT_VECTORING_INFO);

			if (iv & 0x80000000U) {
				vcpu->reinj_info = iv & 0x80000fffU;	/* valid|ec-valid|type|vector */
				vcpu->reinj_ec = (u32)vmread(IDT_VECTORING_EC);
				vcpu->reinj_len = (u32)vmread(VM_EXIT_INSN_LEN);
				vcpu->reinj_valid = true;
				vcpu->reinj_count++;
			}
		}

		if (vcpu->pmu_live) {
			/* branches the guest retired during this entry */
			u64 got;

			vcpu->pmu_cur = rd(MSR_PMC0) & HVX_PMU_MASK;
			got = (vcpu->pmu_cur - vcpu->pmu_base) & HVX_PMU_MASK;
			vcpu->pmu_carry = vcpu->pmu_step;	/* a step's counter can run on */

			/* DIAGNOSTIC: per-entry cost and the overflow skid */
			ft_n = rdtsc();
			if (vcpu->pmu_step) {
				fts->step_entries++;
				fts->step_cyc += ft_n - ft_c;
				fts->step_hw_cyc += ft_hw;
				if (vcpu->fts_tgt_turn)
					fts->steps_turn++;
				else
					fts->steps_tick++;
			} else {
				fts->free_entries++;
				fts->free_cyc += ft_n - ft_c;
				if (got >= vcpu->pmu_n) {
					u64 sk = got - vcpu->pmu_n;
					unsigned int b = sk ? min_t(unsigned int, fls64(sk),
								    HVX_FTSTAT_SKID_BUCKETS - 1) : 0;

					fts->ovf++;
					fts->skid_sum += sk;
					if (sk > fts->skid_max)
						fts->skid_max = sk;
					fts->skid_hist[b]++;
				}
			}
			ft_c = ft_n;
			vcpu->pmu_total += got;
			if (chaos_nmi && ++vcpu->chaos_nmi_k % chaos_nmi == 0)
				hvx_chaos_nmi_send();	/* testing only (M156) */
			vcpu->br_own += got;
			vcpu->pmu_n = 0;
			if (got || !vcpu->pmu_step)
				vcpu->pmu_flat = 0;
		}

		if (!ok) {
			ex->reason = HVX_EXIT_VMFAIL;
			ex->insn_len = 0;
			ex->qualification = vmread(VM_INSTRUCTION_ERROR);
			ex->guest_phys = 0;
			goto out_clear;		/* ret stays 0: reported via ex->reason */
		}
		resume = 1;			/* every entry after the first is a VMRESUME */

		reason = (u32)vmread(VM_EXIT_REASON);
		basic = reason & 0xffff;

		/*
		 * DIAGNOSTIC: count EVERY VM-exit by basic reason (including the ones
		 * handled in-kernel and VMRESUMEd below). Kernel-internal only; does
		 * not touch guest state, so it is fingerprint-neutral.
		 */
		if (basic < 64)
			vcpu->exit_hist[basic]++;
		vcpu->exit_total++;

		/* Refresh rip/rsp/rflags on EVERY exit, before any path that can leave
		 * the loop: setup_vmcs reloads them from the register file on the next
		 * run, so an exit path that skipped this (external-interrupt / NMI /
		 * interrupt-window -> out_clear) would re-enter at a stale RIP. */
		vcpu->regs.rip    = vmread(GUEST_RIP);
		vcpu->regs.rsp    = vmread(GUEST_RSP);
		vcpu->regs.rflags = vmread(GUEST_RFLAGS);

		if (vcpu->pmu_live) {
			/* a landing step's counter is ~2^30 branches from overflow: only an
			 * exit after a free-running entry can carry our overflow (FASTTURN.md) */
			if ((!vcpu->pmu_step || !(ft_opt & 2)) && (rd(MSR_PERF_GLOBAL_STATUS) & 1)) {
				/* our overflow: ack it and unmask LVTPC (a PMI masks it) */
				wrmsrl(MSR_PERF_GLOBAL_OVF, 1);
				hvx_lvtpc_write(HVX_PMU_LVT_VECTOR);
			}
			if (basic == EXIT_REASON_MONITOR_TRAP && !(reason & 0x80000000U)) {
				/* a landing step: not an event on the instruction clock (how
				 * many steps it takes depends on the overflow skid). It is
				 * bounded like any other non-retired exit, so a guest that
				 * never retires another branch cannot hold the host CPU. */
				vcpu->pmu_steps++;
				vcpu->pmu_flat++;
				if (++safety >= HVX_SAFETY_CAP && !sched) {
					ex->reason = HVX_EXIT_THROTTLE;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
				if ((++streak >= HVX_PREEMPT_STREAK || need_resched() || hvx_chaos_preempt() ||
				     (sched && safety >= HVX_SAFETY_CAP)) &&
				    (u32)vmread(GUEST_INTERRUPTIBILITY) == 0) {
					vcpu->carry_guard = guard;
					vcpu->carry_stepped = stepped;
					ex->reason = HVX_EXIT_PREEMPT;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
				continue;
			}
		}

		/*
		 * Physical external interrupt (we did NOT ack it on exit): it is
		 * still pending. Open a one-instruction IF window so the HOST's IDT
		 * services it, then resume the guest transparently -- it is not a
		 * guest instruction, so the instruction clock is untouched.
		 */
		if (basic == EXIT_REASON_EXTERNAL_INT && !(reason & 0x80000000U)) {
			asm volatile("sti; nop; cli" ::: "memory");
			/* NOT counted toward the deterministic `guard` cadence and does
			 * NOT creep the virtual TSC (the creep lives in the retired block,
			 * so host interrupts -- nondeterministic timing -- never move vm->tsc
			 * or shift the timer-injection icount) (M16). */
			if (++safety >= HVX_SAFETY_CAP && !sched) {
				ex->reason = HVX_EXIT_THROTTLE;
				ex->insn_len = 0;
				ex->qualification = 0;
				ex->guest_phys = 0;
				goto out_clear;
			}
			if ((++streak >= HVX_PREEMPT_STREAK || need_resched() || hvx_chaos_preempt() ||
			     (sched && safety >= HVX_SAFETY_CAP)) &&
			    (u32)vmread(GUEST_INTERRUPTIBILITY) == 0) {
				/* let the host schedule; the ioctl re-enters (see
				 * HVX_PREEMPT_STREAK). Only at a shadow-free boundary,
				 * since setup_vmcs does not carry interruptibility. */
				vcpu->carry_guard = guard;
				vcpu->carry_stepped = stepped;
				ex->reason = HVX_EXIT_PREEMPT;
				ex->insn_len = 0;
				ex->qualification = 0;
				ex->guest_phys = 0;
				goto out_clear;
			}
			continue;
		}

		/*
		 * Physical NMI (NMI-exiting): like external interrupts, a host NMI
		 * (e.g. the perf-based hard-lockup watchdog) must NOT be delivered
		 * through the GUEST's IDT -- that is a nondeterministic event inside
		 * the guest ("NMI: PCI system error ... Dazed and confused") and the
		 * host loses its NMI. Hand it to the host's NMI handler with a
		 * software INT 2 (its IRET also lifts the NMI blocking the exit left
		 * set). Not a guest instruction; accounted like an external interrupt.
		 */
		if (basic == EXIT_REASON_EXC_NMI && !(reason & 0x80000000U) &&
		    ((u32)vmread(VM_EXIT_INTR_INFO) & 0x80000700U) == 0x80000200U) {
			asm volatile("int $2" ::: "memory");
			if (++safety >= HVX_SAFETY_CAP && !sched) {
				ex->reason = HVX_EXIT_THROTTLE;
				ex->insn_len = 0;
				ex->qualification = 0;
				ex->guest_phys = 0;
				goto out_clear;
			}
			continue;
		}

		/*
		 * Interrupt-window exit (milestone 15): the guest just became
		 * interruptible after we deferred a pending injection. Loop back so
		 * the arming block above re-checks and delivers it. Not a guest
		 * instruction, so the instruction clock is untouched.
		 */
		/* chaos_ptimer: a host-timed exit at a random instruction. Not a guest event:
		 * resume transparently (no `safety` count, so it never ends a run call). */
		if (basic == EXIT_REASON_PREEMPT_TIMER && !(reason & 0x80000000U)) {
			if ((++streak >= HVX_PREEMPT_STREAK || need_resched() || hvx_chaos_preempt()) &&
			    (u32)vmread(GUEST_INTERRUPTIBILITY) == 0) {
				vcpu->carry_guard = guard;
				vcpu->carry_stepped = stepped;
				ex->reason = HVX_EXIT_PREEMPT;
				ex->insn_len = 0;
				ex->qualification = 0;
				ex->guest_phys = 0;
				goto out_clear;
			}
			continue;
		}

		if (basic == EXIT_REASON_INTERRUPT_WINDOW) {
			/* Not counted toward the deterministic `guard` cadence; does not
			 * creep the virtual TSC (M16). */
			if (++safety >= HVX_SAFETY_CAP && !sched) {
				ex->reason = HVX_EXIT_THROTTLE;
				ex->insn_len = 0;
				ex->qualification = 0;
				ex->guest_phys = 0;
				goto out_clear;
			}
			if ((++streak >= HVX_PREEMPT_STREAK || need_resched() || hvx_chaos_preempt() ||
			     (sched && safety >= HVX_SAFETY_CAP)) &&
			    (u32)vmread(GUEST_INTERRUPTIBILITY) == 0) {
				/* let the host schedule; the ioctl re-enters (see
				 * HVX_PREEMPT_STREAK). Only at a shadow-free boundary,
				 * since setup_vmcs does not carry interruptibility. */
				vcpu->carry_guard = guard;
				vcpu->carry_stepped = stepped;
				ex->reason = HVX_EXIT_PREEMPT;
				ex->insn_len = 0;
				ex->qualification = 0;
				ex->guest_phys = 0;
				goto out_clear;
			}
			continue;
		}

		if (!(reason & 0x80000000U)) {
			bool retired = false;

			/*
			 * Deterministic RDTSC/RDTSCP: return the virtual TSC instead
			 * of the host counter, advance past the instruction, resume.
			 * RDTSC writes EAX:EDX (zero-extended into RAX:RDX in 64-bit
			 * mode); RDTSCP additionally writes ECX = IA32_TSC_AUX.
			 */
			if (basic == EXIT_REASON_RDTSC || basic == EXIT_REASON_RDTSCP) {
				u64 t;

				/*
				 * ICOUNT: TSC is a function of instructions retired
				 * BEFORE this RDTSC (vcpu->icount is bumped below, in
				 * the shared retired-instruction block). COUNTER: a
				 * free-running per-exit counter.
				 */
				if (vcpu->tsc_mode == HVX_TSC_ICOUNT)
					t = vcpu->tsc + vcpu->icount * vcpu->tsc_step;
				else
					t = vcpu->tsc;

				vcpu->regs.rax = (u32)t;
				vcpu->regs.rdx = (u32)(t >> 32);
				if (basic == EXIT_REASON_RDTSCP)
					vcpu->regs.rcx = (u32)vcpu->tsc_aux;
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);

				if (vcpu->tsc_mode != HVX_TSC_ICOUNT)
					vcpu->tsc += vcpu->tsc_step;
				vcpu->rdtsc_count++;
				retired = true;
			} else if (basic == EXIT_REASON_IO_INSTRUCTION) {
				/* emulate the port access, then skip the instruction */
				hvx_pio(vcpu);
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
			} else if (basic == EXIT_REASON_CPUID) {
				/* deterministic synthetic CPUID, then skip the insn */
				hvx_cpuid(vcpu);
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
			} else if (basic == EXIT_REASON_XSETBV && xs) {
				/* guest AVX: emulated XSETBV (or a queued #GP) */
				if (hvx_xsetbv(vcpu)) {
					vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
					VMW(GUEST_RIP, vcpu->regs.rip);
				}
				retired = true;
			} else if (basic == EXIT_REASON_RDMSR) {
				hvx_rdmsr(vcpu);
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
			} else if (basic == EXIT_REASON_WRMSR) {
				hvx_wrmsr(vcpu);
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
			} else if (basic == EXIT_REASON_PAUSE) {
				/* PAUSE (HVX_VM_PAUSE_EXIT): a spin-wait iteration. Skip it and
				 * count it like any retired exit, so a guest spinning on a lock
				 * or on a pending I/O moves the instruction clock (and with it
				 * device completions and the timer) instead of freezing it. */
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
				/* scheduler: K PAUSEs in a row (a spin-wait) end the turn, at
				 * the next clean boundary (the loop-top check) */
				if (sched && vm->sched.pause_k &&
				    ++vm->sched.pause_run >= vm->sched.pause_k)
					vm->sched.turn_over = 1;
			} else if (basic == EXIT_REASON_MONITOR_TRAP) {
				/* one instruction retired; MTF stays armed for the next */
				retired = true;
			} else if (basic == EXIT_REASON_RDRAND || basic == EXIT_REASON_RDSEED) {
				/* probe_rand only: deterministic value, CF=1 */
				static atomic_t logged;
				static const u8 map[16] = { 0, 2, 3, 1, 15, 6, 4, 5,
							    7, 8, 9, 10, 11, 12, 13, 14 };
				u32 info = (u32)vmread(VMX_INSN_INFO);
				u32 r = (info >> 3) & 0xf, sz = (info >> 11) & 3;
				u64 *gpr = (u64 *)&vcpu->regs;
				u64 v = fault_mix(0x52444e44ULL + vcpu->rdtsc_count + vcpu->icount);

				if (atomic_inc_return(&logged) <= 20)
					pr_info("hvx: probe_rand: %s rip=%#llx reg=%u size=%u icount=%llu\n",
						basic == EXIT_REASON_RDRAND ? "RDRAND" : "RDSEED",
						vcpu->regs.rip, r, sz, vcpu->icount);
				if (sz == 0) v &= 0xffff; else if (sz == 1) v &= 0xffffffffULL;
				if (map[r] == 15) VMW(GUEST_RSP, v);
				else if (sz == 0) gpr[map[r]] = (gpr[map[r]] & ~0xffffULL) | v;
				else gpr[map[r]] = v;
				vcpu->regs.rflags = (vcpu->regs.rflags & ~0x8d5ULL) | 1;	/* CF=1 */
				VMW(GUEST_RFLAGS, vcpu->regs.rflags);
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
			} else if (basic == EXIT_REASON_EPT_VIOLATION) {
				/* IOAPIC MMIO (0xfec00000): decode + emulate the MOV in
				 * kernel, then skip it. Any other unbacked GPA (e.g. a
				 * stray 0xfee00000 LAPIC access if x2APIC did not take) is
				 * left for userspace to report. */
				u64 gpa = vmread(GUEST_PHYS_ADDR);

				if (gpa >= IOAPIC_BASE && gpa < IOAPIC_TOP &&
				    hvx_ioapic_mmio(vcpu, gpa, (u32)vmread(VM_EXIT_INSN_LEN)))
					retired = true;
				else if (vm->vblk.disk_pages &&
					 gpa >= vblk_bar_base(&vm->vblk) &&
					 gpa < vblk_bar_base(&vm->vblk) + HVX_VBLK_BAR_SIZE &&
					 hvx_virtio_mmio(vcpu, gpa, (u32)vmread(VM_EXIT_INSN_LEN)))
					retired = true;	/* virtio-pci BAR MMIO (M20 Phase B / M21 MSI-X) */
				else if (vm->vnet.present &&
					 gpa >= vnet_bar_base(&vm->vnet) &&
					 gpa < vnet_bar_base(&vm->vnet) + HVX_VNET_BAR_SIZE &&
					 hvx_vnet_mmio(vcpu, gpa, (u32)vmread(VM_EXIT_INSN_LEN)))
					retired = true;	/* virtio-net BAR MMIO (M23) */
			} else if (sched && basic == EXIT_REASON_HLT) {
				/* Scheduler: with an interrupt already deliverable the HLT is
				 * just stepped over (it is the wake-up). Otherwise the vCPU
				 * halts: the turn ends here, RIP stays on the HLT, and the
				 * scheduler skips this vCPU until an interrupt is posted to it.
				 * Time does not jump here -- only when every vCPU is halted.
				 * The boundary is clean by construction (the HLT is the end of
				 * any STI shadow it sat in; on wake-up RIP moves past it). */
				if (hvx_vcpu_irq_ready(vcpu)) {
					vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
					VMW(GUEST_RIP, vcpu->regs.rip);
					retired = true;
				} else {
					vcpu->halted = HVX_HALT_HLT;
					vcpu->halt_len = (u32)vmread(VM_EXIT_INSN_LEN);
					ex->reason = HVX_EXIT_TURN;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
			} else if (basic == EXIT_REASON_HLT &&
				   (vcpu->lapic.tsc_deadline ||
				    (vcpu->index == 0 && vm->vblk.completion_armed) ||
				    (vcpu->index == 0 && vm->vnet.present &&
				     (vm->vnet.q[0].completion_armed || vm->vnet.q[1].completion_armed)))) {
				/* Idle with a pending timed event -- the LAPIC timer and/or a
				 * scheduled virtio-blk completion. Jump the deterministic clock
				 * to the due event so it fires and wakes the guest, skip the HLT,
				 * and resume; the interrupt injects on the next entry. Without
				 * this a tickless idle stalls forever. Crucially the disk case:
				 * when the guest blocks on a disk read it may idle waiting ONLY
				 * on the completion IRQ, with NO LAPIC timer armed (tsc_deadline
				 * 0) -- so the HLT must also honor a pending disk completion, or
				 * the read never completes (M21). */
				bool cmp = (vcpu->index == 0 && vm->vblk.completion_armed);

				if (vcpu->tsc_mode == HVX_TSC_ICOUNT) {
					u64 step = vcpu->tsc_step ? vcpu->tsc_step : 1;
					u64 need = (vcpu->lapic.tsc_deadline > vcpu->tsc)
						 ? (vcpu->lapic.tsc_deadline - vcpu->tsc) / step : 0;

					if (need > vcpu->icount)
						vcpu->icount = need;
				} else if (vcpu->lapic.tsc_deadline > vcpu->tsc) {
					vcpu->tsc = vcpu->lapic.tsc_deadline;
				}
				/* Advance the instruction clock to the disk completion so it
				 * becomes due (its delivery point is icount-based, in both TSC
				 * modes). Only forward. */
				if (cmp && vm->vblk.completion_due > vcpu->icount)
					vcpu->icount = vm->vblk.completion_due;
				/* Same for a pending virtio-net completion (RX/TX): an idle guest
				 * that HLTs waiting only on a network interrupt must not stall. */
				if (vcpu->index == 0 && vm->vnet.present) {
					int qi;

					for (qi = 0; qi < HVX_VNET_NQUEUES; qi++)
						if (vm->vnet.q[qi].completion_armed &&
						    vm->vnet.q[qi].completion_due > vcpu->icount)
							vcpu->icount = vm->vnet.q[qi].completion_due;
				}
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
			} else if (basic == EXIT_REASON_HLT && (vcpu->regs.rflags & 0x200) &&
				   (vcpu->inject_valid || vm->vblk.irq_pending ||
				    (vm->vnet.present && (vm->vnet.q[0].irq_pending ||
							  vm->vnet.q[1].irq_pending)))) {
				/* An interrupt is already pending (latched but not yet
				 * delivered): it is the wake-up, so step past the HLT and let
				 * the loop top deliver it, as hardware would. Typical case:
				 * a tick lands on the STI of Linux's `sti; hlt` idle, the
				 * STI shadow blocks the injection for one instruction, and
				 * the HLT exits with the timer interrupt queued. Returning
				 * that HLT to userspace stopped a healthy guest; it used to
				 * be handled only for VMs with the idle heartbeat. */
				vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
				VMW(GUEST_RIP, vcpu->regs.rip);
				retired = true;
			} else if (basic == EXIT_REASON_HLT && vm->idle_hb) {
				/* UNARMED idle HLT: no timer, no pending completion (see the
				 * HVX_IDLE_HEARTBEAT_TSC note). Only when the VM opted in via
				 * HVX_VM_IDLE_HEARTBEAT (OFF by default, so the no-disk boot -- which
				 * also idles unarmed -- stays byte-identical to pre-M27). Inject a
				 * synthetic LAPIC-timer
				 * tick so Linux's timer ISR + softirqs (loopback delivery) + the
				 * scheduler run, breaking the tickless deadlock. Only if the guest
				 * has a real, unmasked LAPIC-timer vector and interrupts are on
				 * (VM-entry requires IF=1 to inject an external interrupt); else
				 * this is a genuine wedge -- fall through and return the HLT. */
				u32 lvt = vcpu->lapic.reg[X2APIC_LVT_TIMER - X2APIC_BASE_MSR];

				if ((lvt & 0xff) >= 0x20 && !(lvt & LVT_MASKED) &&
				    (vcpu->regs.rflags & 0x200) && !vcpu->inject_valid) {
					vcpu->tsc += HVX_IDLE_HEARTBEAT_TSC;
					vcpu->inject_vector = lvt & 0xff;
					vcpu->inject_at_icount = vcpu->icount;
					vcpu->inject_valid = 1;
					vcpu->regs.rip += (u32)vmread(VM_EXIT_INSN_LEN);
					VMW(GUEST_RIP, vcpu->regs.rip);
					retired = true;
				}
			}

			if (retired) {
				/*
				 * COUNTER-mode virtual-TSC creep (M16): advance vcpu->tsc by one
				 * step per RETIRED (deterministic) exit while a deadline is
				 * armed, so time moves forward even between RDTSCs yet vcpu->tsc
				 * is a pure function of icount. Doing it here -- not at the loop
				 * top -- keeps nondeterministic external-interrupt / interrupt-
				 * window iterations from creeping it, which would otherwise make
				 * the timer inject at a run-dependent instruction and diverge two
				 * boots that are byte-identical in guest execution.
				 */
				if (vcpu->tsc_mode != HVX_TSC_ICOUNT && vcpu->lapic.tsc_deadline)
					vcpu->tsc += vcpu->tsc_step;
				vcpu->icount++;
				stepped++;
				safety++;
				streak = 0;

				if (sched) {
					u32 is = (u32)vmread(GUEST_INTERRUPTIBILITY);

					/* The instruction was emulated and skipped, so any STI /
					 * MOV-SS shadow it sat in is over, as on hardware (KVM does
					 * the same). Left set, it shifted the next interrupt: after
					 * a stepped-over `sti; hlt` it covered the `cli` and the
					 * pending interrupt waited for the next sti. */
					if (is & 3)
						VMW(GUEST_INTERRUPTIBILITY, is & ~3U);
					/* stops wait for a clean boundary (loop top) */
					if (basic != EXIT_REASON_PAUSE)
						vm->sched.pause_run = 0;
					if (max_insns && stepped >= max_insns)
						vm->sched_stop |= HVX_SCHED_STOP_STEP;
					if (++guard >= HVX_MAX_INKERNEL_EXITS)
						vm->sched_stop |= HVX_SCHED_STOP_THROTTLE;
					continue;
				}
				/* host-call port (M138): a committed record ends the run call right
				 * after its OUT, before the step budget is looked at */
				if (vm->hc_exit) {
					vm->hc_exit = false;
					ex->reason = HVX_EXIT_HOSTCALL;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
				if (max_insns && stepped >= max_insns) {
					ex->reason = HVX_EXIT_STEP;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
				/* deterministic cadence: retired exits only (M16) */
				if (++guard >= HVX_MAX_INKERNEL_EXITS) {
					ex->reason = HVX_EXIT_THROTTLE;
					ex->insn_len = 0;
					ex->qualification = 0;
					ex->guest_phys = 0;
					goto out_clear;
				}
				continue;
			}
		}

		/* everything else is returned to userspace */
		ex->reason = reason;
		ex->insn_len = (u32)vmread(VM_EXIT_INSN_LEN);
		ex->qualification = vmread(EXIT_QUALIFICATION);
		ex->guest_phys = vmread(GUEST_PHYS_ADDR);
		break;
	}

out_clear:
	ft_c = rdtsc();			/* DIAGNOSTIC */
	if (sched) {
		/* the scheduler carries the cadence and step count across turns */
		vcpu->carry_guard = guard;
		vcpu->carry_stepped = stepped;
	}
	/* Capture the guest's SYSCALL / KERNEL_GS_BASE MSRs (a guest swapgs/WRMSR may
	 * have changed them mid-run) so they persist into vm->sw_msr + the snapshot,
	 * then restore the host's. Only if we actually loaded them (msrs_loaded). */
	if (msrs_loaded) {
		/* Only KERNEL_GS_BASE can change in hardware without a WRMSR exit (via
		 * guest swapgs); STAR/LSTAR/CSTAR/SFMASK only change via WRMSR, which
		 * already updated sw_msr -- re-capturing them would clobber a stored
		 * (possibly non-canonical, un-loadable) value. Then restore the host. */
		vcpu->sw_msr[SWMSR_KERNEL_GS_BASE] = rd(GMSR_KERNEL_GS_BASE);
		if (xs)
			hvx_xs_save(vcpu);
		else
			asm volatile("fxsave64 (%0)" :: "r" ((void *)vcpu->fx_page) : "memory");
		wrmsrl(GMSR_STAR,   host_star);
		wrmsrl(GMSR_LSTAR,  host_lstar);
		wrmsrl(GMSR_CSTAR,  host_cstar);
		wrmsrl(GMSR_SFMASK, host_sfmask);
		wrmsrl(GMSR_KERNEL_GS_BASE, host_kgs);
	}

	/* persist the guest's privileged state (CR3/segments/GDTR/IDTR/EFER) for the
	 * next entry; the VMCS is still current here. Skip on host-side setup failure
	 * (ret != 0), where no guest state ran. */
	if (ret == 0)
		save_sregs(vcpu);
	ft_n = rdtsc(); fts->save_cyc += ft_n - ft_c; ft_c = ft_n;
	if (vcpu->pmu_live) {
		/* give the PMU back to the host exactly as we found it */
		wrmsrl(MSR_PERFEVTSEL0, 0);
		wrmsrl(MSR_PERF_GLOBAL_OVF, 1);
		wrmsrl(MSR_PMC0, (u64)(u32)pmu_h_pmc);
		wrmsrl(MSR_PERFEVTSEL0, pmu_h_sel);
		hvx_lvtpc_write(pmu_h_lvtpc);
		wrmsrl(MSR_PERF_GLOBAL_CTRL, pmu_h_gctl);
		vcpu->pmu_live = false;
	}
	if (pt_live) {
		/* remember where the output got to, then give PT back to the host */
		int k;

		wrmsrl(MSR_RTIT_CTL, 0);
		vm->pt_mask = rd(MSR_RTIT_OUTPUT_MASK);
		vm->pt_status = rd(MSR_RTIT_STATUS);
		wrmsrl(MSR_RTIT_STATUS, pt_h[1]);
		wrmsrl(MSR_RTIT_OUTPUT_BASE, pt_h[2]);
		wrmsrl(MSR_RTIT_OUTPUT_MASK, pt_h[3]);
		wrmsrl(MSR_RTIT_CR3_MATCH, pt_h[4]);
		for (k = 0; k < 4; k++)
			wrmsrl(MSR_RTIT_ADDR0_A + k, pt_h[5 + k]);
		wrmsrl(MSR_RTIT_CTL, pt_h[0]);
	}
	ft_n = rdtsc(); fts->pmuoff_cyc += ft_n - ft_c; ft_c = ft_n;
	vmclear(vmcs_pa);
out_vmxoff:
	if (vmxoff())
		pr_err("hvx: VMXOFF failed\n");
out_cr4:
	hvx_set_cr4(hvx_cr4() & ~CR4_VMXE);
	local_irq_restore(flags);
	preempt_enable();
	kernel_fpu_end();		/* restore the host FPU/XMM state (M14) */
	vcpu->run_ns += ktime_get_ns() - t0;	/* DIAGNOSTIC (fingerprint-neutral) */
	ft_n = rdtsc();
	fts->vmxoff_cyc += ft_n - ft_c;
	fts->total_cyc += ft_n - ft_c0;
	fts->calls++;
	return ret;
}

/*
 * Run the vCPU, transparently re-entering after an internal preempt return
 * (HVX_EXIT_PREEMPT: the host scheduler wants this CPU, or the guest retired no
 * exits for HVX_PREEMPT_STREAK host interrupts). Between re-entries the host may
 * schedule, so no guest can monopolise a CPU. If the guest makes no progress at
 * all for HVX_STALL_SECS it is wedged: report
 * HVX_EXIT_STALL so userspace can dump RIP + console instead of hanging.
 */
static int hvx_run_resched(struct hvx_vcpu *vc, struct hvx_exit *ex,
			   bool single_step, u64 max_insns)
{
	unsigned long last_progress = jiffies;
	unsigned int last_guard = 0;
	u64 last_total = vc->pmu_total, last_rcx = vc->regs.rcx;
	int ret;

	vc->carry_guard = 0;
	vc->carry_stepped = 0;
	for (;;) {
		memset(ex, 0, sizeof(*ex));
		ret = hvx_run_vcpu(vc, ex, single_step, max_insns);
		if (ret || ex->reason != HVX_EXIT_PREEMPT)
			break;
		/* progress = a retired exit, a retired branch, or a REP counter moving */
		if (vc->carry_guard != last_guard || vc->pmu_total != last_total ||
		    vc->regs.rcx != last_rcx) {
			last_guard = vc->carry_guard;
			last_total = vc->pmu_total;
			last_rcx = vc->regs.rcx;
			last_progress = jiffies;
		}
		if (time_after(jiffies, last_progress + HVX_STALL_SECS * HZ)) {
			ex->reason = HVX_EXIT_STALL;
			break;
		}
		if (fatal_signal_pending(current)) {
			ret = -EINTR;
			break;
		}
		cond_resched();
	}
	vc->carry_guard = 0;
	vc->carry_stepped = 0;
	return ret;
}

/* ---- in-kernel vCPU scheduler (HVX_VM_SMP_SCHED) ----
 *
 * One HVX_RUN drives every vCPU. Turns run one vCPU each through hvx_run_vcpu (a
 * full VMXON..VMXOFF section per turn, so the host CPU is released between turns
 * and every per-vCPU save/restore path is the one a single vCPU uses). The VM has
 * ONE clock -- virtual TSC, instruction clock and PMU tick grid -- held in vm->sched
 * and copied into the vCPU that runs; a vCPU's own branches are counted in br_own.
 */
static void hvx_clk_put(const struct hvx_sched *c, struct hvx_vcpu *v)
{
	v->tsc = c->tsc;
	v->tsc_step = c->tsc_step;
	v->tsc_mode = c->tsc_mode;
	v->icount = c->icount;
	v->pmu_total = c->pmu_total;
	v->pmu_next = c->pmu_next;
}

static void hvx_clk_sync(struct hvx_vm *vm)
{
	unsigned int i;

	for (i = 0; i < vm->n_vcpus; i++)
		hvx_clk_put(&vm->sched, &vm->vcpu[i]);
}

/* make v's copy of the clock the VM's clock (after v ran, or after an ioctl set it) */
static void hvx_clk_take(struct hvx_vm *vm, const struct hvx_vcpu *v)
{
	struct hvx_sched *c = &vm->sched;

	c->tsc = v->tsc;
	c->tsc_step = v->tsc_step;
	c->tsc_mode = v->tsc_mode;
	c->icount = v->icount;
	c->pmu_total = v->pmu_total;
	c->pmu_next = v->pmu_next;
	hvx_clk_sync(vm);
}

/* the base length of the turn that starts now (turn number c->turns) */
static u64 hvx_sched_len0(const struct hvx_sched *c)
{
	if (!c->seed)
		return c->quantum;
	return 1 + fault_mix(c->seed + (c->turns + 1) * 0x9e3779b97f4a7c15ULL) % c->quantum;
}

/* burst window: W = max(burst_window, quantum, burst_len) (see struct hvx_turnctl) */
static u64 hvx_burst_w(const struct hvx_vm *vm)
{
	const struct hvx_turnctl *t = &vm->tc;

	return max3(t->burst_window, vm->sched.quantum, t->burst_len);
}

/* does window w hold a burst? *zs = its first branch */
static bool hvx_burst_zone(const struct hvx_vm *vm, u64 w, u64 W, u64 *zs)
{
	const struct hvx_turnctl *t = &vm->tc;
	u64 h = fault_mix(t->burst_seed + (w + 1) * 0x9e3779b97f4a7c15ULL);

	if (h % 1000 >= t->burst_permille)
		return false;
	*zs = w * W + (h >> 16) % (W - t->burst_len + 1);
	return true;
}

/*
 * Length of the turn that starts now. Without bursts: hvx_sched_len0. With bursts
 * (struct hvx_turnctl): a pure function of the policy and the VM branch count at the
 * turn's start -- burst_q inside a burst (cut at its end), else the base length cut
 * where the next burst starts. The base length is at most quantum <= W, so the next
 * burst can only be in this window or the next one.
 */
static u64 hvx_sched_len(struct hvx_vm *vm)
{
	const struct hvx_sched *c = &vm->sched;
	struct hvx_turnctl *t = &vm->tc;
	u64 len = hvx_sched_len0(c), x = c->pmu_total, W, w, k, zs;

	if (!t->burst_seed || !t->burst_permille)
		return len;
	W = hvx_burst_w(vm);
	w = x / W;
	if (hvx_burst_zone(vm, w, W, &zs) && x >= zs && x < zs + t->burst_len) {
		t->burst_turns++;
		return min(t->burst_q, zs + t->burst_len - x);
	}
	for (k = w; k <= w + 1; k++) {
		if (hvx_burst_zone(vm, k, W, &zs) && zs > x) {
			if (zs - x < len) {
				len = zs - x;
				t->clipped++;
			}
			break;
		}
	}
	return len;
}

/* due timers on every vCPU, due device events (vCPU 0) */
static void hvx_sched_events(struct hvx_vm *vm)
{
	unsigned int i;

	for (i = 0; i < vm->n_vcpus; i++)
		hvx_lapic_timer_due(&vm->vcpu[i]);
	hvx_dev_due(&vm->vcpu[0]);
}

/*
 * Next vCPU to run: round-robin after `cur`. A halted vCPU is skipped unless an
 * interrupt is now deliverable to it; then it wakes (RIP moves past its HLT, as a
 * real CPU leaves the halt state). Returns -1 if every vCPU stays halted.
 */
static int hvx_sched_pick(struct hvx_vm *vm)
{
	unsigned int n = vm->n_vcpus, k;

	hvx_sched_events(vm);
	for (k = 1; k <= n; k++) {
		struct hvx_vcpu *v = &vm->vcpu[(vm->sched.cur + k) % n];

		if (v->halted == HVX_HALT_SIPI)
			continue;		/* only a STARTUP IPI makes it runnable */
		if (v->halted) {
			if (!hvx_vcpu_irq_ready(v))
				continue;
			v->halted = HVX_HALT_RUN;
			v->regs.rip += v->halt_len;
			vm->sched.wakeups++;
		}
		return v->index;
	}
	return -1;
}

/*
 * Every vCPU is halted: jump the VM clock forward to the earliest armed event -- a
 * LAPIC deadline (on the TSC) and/or a device completion or scheduled injection (on
 * the instruction clock), as the one-vCPU idle HLT does. Only forward. Returns
 * whether anything was armed.
 */
static bool hvx_sched_idle_jump(struct hvx_vm *vm)
{
	struct hvx_sched *c = &vm->sched;
	u64 dl = U64_MAX, due = U64_MAX;
	unsigned int i;
	int qi;

	for (i = 0; i < vm->n_vcpus; i++) {
		struct hvx_vcpu *v = &vm->vcpu[i];

		if (v->lapic.tsc_deadline && v->lapic.tsc_deadline < dl)
			dl = v->lapic.tsc_deadline;
		if (v->inject_valid && v->inject_at_icount < due)
			due = v->inject_at_icount;
	}
	if (vm->vblk.completion_armed && vm->vblk.completion_due < due)
		due = vm->vblk.completion_due;
	if (vm->vnet.present)
		for (qi = 0; qi < HVX_VNET_NQUEUES; qi++)
			if (vm->vnet.q[qi].completion_armed && vm->vnet.q[qi].completion_due < due)
				due = vm->vnet.q[qi].completion_due;
	if (dl == U64_MAX && due == U64_MAX)
		return false;
	if (dl != U64_MAX) {
		if (c->tsc_mode == HVX_TSC_ICOUNT) {
			u64 step = c->tsc_step ? c->tsc_step : 1;
			u64 need = dl > c->tsc ? (dl - c->tsc + step - 1) / step : 0;

			if (need > c->icount)
				c->icount = need;
		} else if (dl > c->tsc) {
			c->tsc = dl;
		}
	}
	if (due != U64_MAX && due > c->icount)
		c->icount = due;
	c->time_jumps++;
	hvx_clk_sync(vm);
	return true;
}

static int hvx_sched_run(struct hvx_vm *vm, struct hvx_exit *ex, u64 max_insns)
{
	struct hvx_sched *c = &vm->sched;
	unsigned long last_progress = jiffies;
	u64 last_total = c->pmu_total, last_turns = c->turns, last_guard = c->guard;
	u64 last_rcx = vm->vcpu[c->cur].regs.rcx;
	u64 stepped = 0;
	u64 ft_c0 = rdtsc();		/* DIAGNOSTIC (FASTTURN.md) */
	int ret = 0;

	vm->sched_stop = 0;
	hvx_clk_sync(vm);
	for (;;) {
		struct hvx_vcpu *v;

		memset(ex, 0, sizeof(*ex));
		if (!c->in_turn) {
			int next = hvx_sched_pick(vm);

			if (next < 0 && hvx_sched_idle_jump(vm))
				next = hvx_sched_pick(vm);
			if (next < 0) {
				/* everyone halted, nothing armed: report the HLT */
				ex->reason = EXIT_REASON_HLT;
				ex->insn_len = vm->vcpu[c->cur].halt_len;
				break;
			}
			v = &vm->vcpu[next];
			c->cur = next;
			c->in_turn = 1;
			c->turn_over = 0;
			c->pause_run = 0;
			c->turn_end = c->pmu_total + hvx_sched_len(vm);
			v->pmu_flat = 0;
			v->sched_turns++;
		}
		v = &vm->vcpu[c->cur];
		v->carry_guard = (unsigned int)c->guard;
		v->carry_stepped = stepped;
		ret = hvx_run_vcpu(v, ex, false, max_insns);
		c->guard = v->carry_guard;
		stepped = v->carry_stepped;
		v->carry_guard = 0;
		v->carry_stepped = 0;
		hvx_clk_take(vm, v);
		if (ret)
			break;
		if (ex->reason == HVX_EXIT_TURN) {
			u64 h = c->turn_hash;

			h = (h ^ v->index) * 0x100000001b3ULL;
			h = (h ^ v->regs.rip) * 0x100000001b3ULL;
			h = (h ^ v->regs.rcx) * 0x100000001b3ULL;
			h = (h ^ v->regs.rflags) * 0x100000001b3ULL;
			h = (h ^ c->pmu_total) * 0x100000001b3ULL;
			h = (h ^ (u64)v->halted) * 0x100000001b3ULL;
			c->turn_hash = h;
			c->in_turn = 0;
			c->turn_over = 0;
			c->turns++;
			vm->fts_sched_turns++;		/* DIAGNOSTIC */
			/* a turn switch counts on the THROTTLE cadence, so a guest that runs
			 * only in tiny turns still returns to userspace */
			if (++c->guard >= HVX_MAX_INKERNEL_EXITS)
				vm->sched_stop |= HVX_SCHED_STOP_THROTTLE;
			memset(ex, 0, sizeof(*ex));
			if (c->stop_turn && c->turns == c->stop_turn) {
				ex->reason = HVX_EXIT_TURN;
				break;
			}
			if (vm->sched_stop) {
				ex->reason = (vm->sched_stop & HVX_SCHED_STOP_THROTTLE)
					   ? HVX_EXIT_THROTTLE : HVX_EXIT_STEP;
				break;
			}
		} else if (ex->reason != HVX_EXIT_PREEMPT) {
			break;		/* THROTTLE / STEP (both at clean boundaries), or a real exit */
		}
		/* between turns or after a preempt: let the host run, bound a wedge */
		if (c->guard != last_guard || c->pmu_total != last_total ||
		    c->turns != last_turns || v->regs.rcx != last_rcx) {
			last_guard = c->guard;
			last_total = c->pmu_total;
			last_turns = c->turns;
			last_rcx = v->regs.rcx;
			last_progress = jiffies;
		}
		if (time_after(jiffies, last_progress + HVX_STALL_SECS * HZ)) {
			memset(ex, 0, sizeof(*ex));
			ex->reason = HVX_EXIT_STALL;
			break;
		}
		if (fatal_signal_pending(current)) {
			ret = -EINTR;
			break;
		}
		cond_resched();
	}
	/* a THROTTLE restarts the cadence -- also when another stop is reported at the
	 * same boundary, so the next THROTTLE does not depend on which one was */
	if (vm->sched_stop & HVX_SCHED_STOP_THROTTLE)
		c->guard = 0;
	vm->sched_stop = 0;
	vm->fts_sched_cyc += rdtsc() - ft_c0;	/* DIAGNOSTIC */
	return ret;
}

/* ---- VM lifecycle ---- */

/* Free the page-array backing disk image (milestone 28): each order-0 page, then the
 * pointer array. Mirrors the guest-RAM free loop below. */
static void vblk_disk_free(struct hvx_vblk *d, spinlock_t *map_lock)
{
	unsigned long *pages, n, i;

	spin_lock(map_lock);		/* unpublish first (M97: see hvx_disk_fault) */
	pages = d->disk_pages;
	n = d->n_disk_pages;
	d->disk_pages = NULL;
	d->n_disk_pages = 0;
	d->disk_size = 0;
	spin_unlock(map_lock);
	if (pages) {
		for (i = 0; i < n; i++)
			if (pages[i]) free_page(pages[i]);
		kvfree(pages);
	}
}

/* Allocate a page-array backing disk image of `len` bytes (milestone 28): a
 * kvmalloc_array of individually-allocated order-0 pages (the M11 recipe), so a
 * GB-scale image is a robust per-page allocation with correct per-page refcounts for
 * the disk-mmap fault handler. Any prior image is freed first. Returns 0 or -ENOMEM. */
/*
 * Guest RAM and disk images are sized by userspace (up to 3 GiB and 4 GiB) out of
 * order-0 pages (M97). With plain GFP_KERNEL a request larger than the free memory
 * never failed: the page allocator called the OOM killer, which cannot see these
 * pages (they are not in any process's RSS), killed other processes and then
 * panicked "System is deadlocked on memory". __GFP_RETRY_MAYFAIL reclaims as hard as
 * before but fails instead of OOM-killing, so the ioctl returns ENOMEM; the pages are
 * charged to the caller's memory cgroup like KVM's (__GFP_ACCOUNT), and a pending
 * fatal signal stops the loop. Same zeroed pages as before when memory suffices.
 */
#define HVX_GFP_BULK (GFP_KERNEL_ACCOUNT | __GFP_RETRY_MAYFAIL | __GFP_NOWARN | __GFP_ZERO)

static int vblk_disk_alloc(struct hvx_vblk *d, u64 len, spinlock_t *map_lock)
{
	unsigned long np = DIV_ROUND_UP(len, PAGE_SIZE), i;
	unsigned long *pages, *old_pages;
	unsigned long old_n;

	pages = kvmalloc_array(np, sizeof(*pages), GFP_KERNEL | __GFP_ZERO);
	if (!pages)
		return -ENOMEM;
	for (i = 0; i < np; i++) {
		pages[i] = __get_free_page(HVX_GFP_BULK);
		if (!(i & 511))
			cond_resched();
		if (!pages[i] || fatal_signal_pending(current)) {
			if (pages[i])
				free_page(pages[i]);
			while (i--)
				free_page(pages[i]);
			kvfree(pages);
			return -ENOMEM;
		}
	}
	/* publish the new image, then drop any prior one; a concurrent disk-mmap fault
	 * sees either the old array or the new one, never a freed one (M97) */
	spin_lock(map_lock);
	old_pages = d->disk_pages;
	old_n = d->n_disk_pages;
	d->disk_pages = pages;
	d->n_disk_pages = np;
	d->disk_size = len;
	spin_unlock(map_lock);
	if (old_pages) {
		for (i = 0; i < old_n; i++)
			if (old_pages[i]) free_page(old_pages[i]);
		kvfree(old_pages);
	}
	return 0;
}

/* Page-wise copy of `len` bytes from a userspace buffer INTO the page-array disk
 * image (milestone 28; replaces the pre-M28 one contiguous copy_from_user). Caller
 * ensures len <= disk_size. Returns 0 or -EFAULT. */
static int vblk_disk_from_user(struct hvx_vblk *d, const void __user *usrc, u64 len)
{
	const u8 __user *src = usrc;
	u64 off = 0;

	while (len) {
		unsigned long pg = off >> PAGE_SHIFT;
		u64 poff = off & (PAGE_SIZE - 1);
		u64 chunk = PAGE_SIZE - poff;

		if (chunk > len)
			chunk = len;
		if (copy_from_user((u8 *)d->disk_pages[pg] + poff, src + off, chunk))
			return -EFAULT;
		off += chunk; len -= chunk;
	}
	return 0;
}

/* Page-wise copy of `len` bytes FROM the page-array disk image to a userspace buffer
 * (milestone 28). Caller ensures len <= disk_size. Returns 0 or -EFAULT. */
static int vblk_disk_to_user(struct hvx_vblk *d, void __user *udst, u64 len)
{
	u8 __user *dst = udst;
	u64 off = 0;

	while (len) {
		unsigned long pg = off >> PAGE_SHIFT;
		u64 poff = off & (PAGE_SIZE - 1);
		u64 chunk = PAGE_SIZE - poff;

		if (chunk > len)
			chunk = len;
		if (copy_to_user(dst + off, (u8 *)d->disk_pages[pg] + poff, chunk))
			return -EFAULT;
		off += chunk; len -= chunk;
	}
	return 0;
}

static void hvx_vm_free_pages(struct hvx_vm *vm)
{
	unsigned int i;

	hvx_pt_release(vm);		/* Intel PT: keep the buffer as the last trace */

	/* no mmap fault may use guest_pages[] from here on (M97) */
	spin_lock(&vm->map_lock);
	vm->ram_ready = false;
	spin_unlock(&vm->map_lock);
	/* virtio-blk backing image (milestone 20; page array as of M28) */
	vblk_disk_free(&vm->vblk, &vm->map_lock);
	/* flush-aware DROP volatile write cache (milestone 26) */
	if (vm->vblk.unsync_data) {
		kvfree(vm->vblk.unsync_data);
		vm->vblk.unsync_data = NULL;
	}
	vm->vblk.unsync_n = 0;
	/* M138: the host-call buffers and the dirty log */
	kvfree(vm->hc_out);
	kvfree(vm->hc_in);
	vm->hc_out = vm->hc_in = NULL;
	vm->hc_on = false;
	kvfree(vm->dl_ram);
	kvfree(vm->vblk.dl);
	vm->dl_ram = NULL;
	vm->vblk.dl = NULL;
	vm->vblk.dl_pages = 0;
	vm->dl_on = false;
	if (vm->guest_pages) {
		for (i = 0; i < vm->n_guest_pages; i++)
			if (vm->guest_pages[i]) free_page(vm->guest_pages[i]);
		kvfree(vm->guest_pages);
		vm->guest_pages = NULL;
	}
	vm->n_guest_pages = 0;
	vm->n_low_pages = 0;
	if (vm->ept_pt) {
		for (i = 0; i < vm->n_ept_pt; i++)
			if (vm->ept_pt[i]) free_page(vm->ept_pt[i]);
		kvfree(vm->ept_pt);
		vm->ept_pt = NULL;
	}
	vm->n_ept_pt = 0;
	if (vm->ept_pd) {				/* EPT PD fan-out (M28) */
		for (i = 0; i < vm->n_ept_pd; i++)
			if (vm->ept_pd[i]) free_page(vm->ept_pd[i]);
		kvfree(vm->ept_pd);
		vm->ept_pd = NULL;
	}
	vm->n_ept_pd = 0;
	for (i = 0; i < 2; i++)
		if (vm->ept_pg[i]) { free_page(vm->ept_pg[i]); vm->ept_pg[i] = 0; }
	/* per-vCPU VMXON/VMCS pages, then the vCPU array (milestone 19) */
	if (vm->vcpu) {
		for (i = 0; i < vm->n_vcpus; i++) {
			if (vm->vcpu[i].vmcs_page)  free_page(vm->vcpu[i].vmcs_page);
			if (vm->vcpu[i].vmxon_page) free_page(vm->vcpu[i].vmxon_page);
			if (vm->vcpu[i].fx_page)    free_page(vm->vcpu[i].fx_page);
		}
		kvfree(vm->vcpu);
		vm->vcpu = NULL;
	}
	vm->n_vcpus = 0;
	vm->cur_vcpu = 0;
}

/*
 * Seed one vCPU's software state to the deterministic power-on defaults
 * (milestone 19). This is exactly the pre-M19 single-VM reset, applied per vCPU,
 * so vCPU 0 of a single-vCPU VM is byte-for-byte the old behavior. Each vCPU
 * starts at GPA_CODE with its own flat long-mode sregs, virtual TSC, cleared
 * instruction clock, empty MSR store, and its own x2APIC (ID = index).
 */
/* Power-on x87/SSE image: FCW 0x37f (all x87 exceptions masked), MXCSR 0x1f80
 * (all SSE exceptions masked), every register zero. */
static void hvx_fx_init(u8 *fx)
{
	memset(fx, 0, 512);
	*(u16 *)(fx + 0) = 0x037f;
	*(u32 *)(fx + 24) = 0x1f80;
}

static void seed_vcpu(struct hvx_vcpu *vc, struct hvx_vm *vm, u32 index)
{
	vc->vm = vm;
	vc->index = index;
	/* NB: vmxon_page/vmcs_page are allocated by the caller (hvx_create_vm) and
	 * must persist -- seed only the SOFTWARE state here, never those pages. */
	seed_sregs(vc);			/* flat long-mode privileged-state defaults (M13) */

	memset(&vc->regs, 0, sizeof(vc->regs));
	vc->regs.rip    = GPA_CODE;
	vc->regs.rsp    = (u64)vm->n_low_pages * PAGE_SIZE - 16;	/* the low block's top (M179) */
	vc->regs.rflags = 0x2;		/* reserved bit set, IF=0 */
	vc->tsc = 0;
	vc->tsc_step = 1;		/* RDTSC returns 0, 1, 2, ... by default */
	vc->tsc_aux = 0;
	vc->tsc_mode = HVX_TSC_COUNTER;
	vc->rdtsc_count = 0;
	vc->icount = 0;
	vc->inject_valid = 0;
	vc->inject_vector = 0;
	vc->inject_at_icount = 0;
	memset(vc->sw_msr, 0, sizeof(vc->sw_msr));	/* RDMSR before WRMSR -> 0 */

	/* per-CPU x2APIC starts enabled-by-BIOS (milestone 15) */
	memset(&vc->lapic, 0, sizeof(vc->lapic));
	vc->lapic.apic_base = index ? (APICBASE_DEFAULT & ~APICBASE_BSP) : APICBASE_DEFAULT;
	vc->br_own = 0;
	vc->sched_turns = 0;
	vc->halted = HVX_HALT_RUN;
	vc->halt_len = 0;
}

static int hvx_create_vm(struct hvx_vm *vm, struct hvx_config *cfg)
{
	unsigned long n, nlow, npt, npd, gtop;
	unsigned int i, nv;
	int ok = 1;

	if (vm->n_guest_pages)
		return -EEXIST;

	nv = cfg->n_vcpus ? cfg->n_vcpus : 1;	/* default / 0 => single vCPU (M19) */
	if (nv > HVX_MAX_VCPUS)
		return -EINVAL;
	if ((cfg->flags & HVX_VM_PMU_CLOCK) && !hvx_pmu_ok)
		return -ENODEV;			/* no usable PMU (nested / hybrid host) */
	if ((cfg->flags & HVX_VM_SMP_SCHED) && !(cfg->flags & HVX_VM_PMU_CLOCK))
		return -EINVAL;			/* turns are counted on the PMU branch clock */
	if ((cfg->flags & HVX_VM_SMP_BOOT) && !(cfg->flags & HVX_VM_SMP_SCHED))
		return -EINVAL;			/* APs are started and run by the scheduler */
	if ((cfg->flags & HVX_VM_SMP_BOOT) && nv > 1 &&
	    !((rd(MSR_VMX_PROCBASED2) >> 32) & SEC_UNRESTRICTED_GUEST))
		return -ENODEV;			/* APs start in real mode */
	if ((cfg->flags & HVX_VM_X86_64_V3) &&
	    (!boot_cpu_has(X86_FEATURE_AVX2) || !boot_cpu_has(X86_FEATURE_OSXSAVE) ||
	     (xgetbv(XCR_XFEATURE_ENABLED_MASK) & HVX_XS_MASK) != HVX_XS_MASK))
		return -ENODEV;			/* guest AVX needs host AVX2 + XCR0 >= x87|SSE|AVX */
	BUILD_BUG_ON(HVX_XS_SIZE > PAGE_SIZE);
	BUILD_BUG_ON(HVX_XS_SIZE > HVX_PMU_MSR_AREA);	/* M156: the MSR-load areas follow it in fx_page */
	if ((cfg->flags & HVX_VM_DIRTY_LOG) && !hvx_ept_ad)
		return -ENODEV;			/* the RAM log reads the EPT dirty flags */

	n = cfg->mem_size ? DIV_ROUND_UP(cfg->mem_size, PAGE_SIZE) : 64;
	if (n < MIN_GPAGES)
		n = MIN_GPAGES;
	if (n > MAX_GPAGES)			/* > 8 GiB rejected (M179; 3 GiB before) */
		return -EINVAL;
	nlow = min_t(unsigned long, n, HVX_RAM_LOW_PAGES);	/* the rest from 4 GiB (M179) */
	npt = DIV_ROUND_UP(n, EPT_PT_LEAVES);	/* EPT PT pages (<= 1536 for 3 GiB, 4096 for 8 GiB) */
	/* the guest-physical 2 MiB blocks the PTs reach: as many as the PTs below the hole */
	gtop = n > nlow ? HVX_RAM_HIGH_GFN / EPT_PT_LEAVES + (npt - nlow / EPT_PT_LEAVES) : npt;
	npd = DIV_ROUND_UP(gtop, EPT_PT_LEAVES);	/* EPT PD pages (M28 fan-out; 3 for 3 GiB, 9 for 8 GiB) */

	/* compile-time ABI/layout checks (were in the old create path) */
	BUILD_BUG_ON(SWMSR_COUNT != HVX_NR_SW_MSR);
	BUILD_BUG_ON(HVX_SCHED_MAX_VCPUS != HVX_MAX_VCPUS);
	BUILD_BUG_ON(sizeof(struct hvx_unsync_ent) != sizeof(struct hvx_unsync_rec));
	BUILD_BUG_ON(sizeof(((struct hvx_vcpu *)0)->lapic.reg) !=
		     sizeof(((struct hvx_snapshot *)0)->lapic_reg));
	BUILD_BUG_ON(sizeof(vm->ioapic.redir) != sizeof(((struct hvx_snapshot *)0)->ioapic_redir));
	/* M28 guardrails. (1) Guest RAM must stay out of the 4th-GiB MMIO hole, else a
	 * flat GPA space silently backs the LAPIC/IOAPIC/virtio BARs with RAM and breaks
	 * every MMIO trap with no fault: the low block ends below it, the rest starts at
	 * 4 GiB (M179). (2) The EPT PD fan-out hangs off ONE PDPT page (512 entries), so
	 * the whole PD level must fit in 512 pages. (3) The low block is whole PTs. */
	BUILD_BUG_ON((u64)HVX_RAM_LOW_PAGES * PAGE_SIZE > HVX_MMIO_HOLE_BASE);
	BUILD_BUG_ON(HVX_RAM_LOW_PAGES % EPT_PT_LEAVES);
	BUILD_BUG_ON((HVX_RAM_HIGH_GFN << PAGE_SHIFT) < 0x100000000ULL);
	BUILD_BUG_ON(DIV_ROUND_UP(HVX_RAM_HIGH_GFN + MAX_GPAGES - HVX_RAM_LOW_PAGES,
				  (u64)EPT_PT_LEAVES * EPT_PT_LEAVES) > EPT_PT_LEAVES);
	BUILD_BUG_ON(HVX_DISK_MMAP_PGOFF <= MAX_GPAGES);	/* the two mmap windows stay apart */
	BUILD_BUG_ON((u64)HVX_RAM_LOW_PAGES * PAGE_SIZE != HVX_RAM_LOW_MAX);	/* hvx_abi.h's layout */
	BUILD_BUG_ON((u64)HVX_RAM_HIGH_GFN * PAGE_SIZE != HVX_RAM_HIGH_BASE);
	BUILD_BUG_ON((u64)MAX_GPAGES * PAGE_SIZE != HVX_RAM_MAX);

	vm->guest_pages = kvmalloc_array(n, sizeof(*vm->guest_pages),
					 GFP_KERNEL | __GFP_ZERO);
	vm->ept_pt = kvmalloc_array(npt, sizeof(*vm->ept_pt), GFP_KERNEL | __GFP_ZERO);
	vm->ept_pd = kvmalloc_array(npd, sizeof(*vm->ept_pd), GFP_KERNEL | __GFP_ZERO);
	vm->vcpu = kvmalloc_array(nv, sizeof(*vm->vcpu), GFP_KERNEL | __GFP_ZERO);
	if (cfg->flags & HVX_VM_HOSTCALL) {		/* M138: the host-call port's buffers */
		vm->hc_out = kvzalloc(HVX_HC_SZ, GFP_KERNEL);
		vm->hc_in = kvzalloc(HVX_HC_SZ, GFP_KERNEL);
	}
	if (cfg->flags & HVX_VM_DIRTY_LOG)		/* M138: the RAM part of the dirty log */
		vm->dl_ram = kvcalloc(BITS_TO_LONGS(n), sizeof(unsigned long), GFP_KERNEL);
	if (!vm->guest_pages || !vm->ept_pt || !vm->ept_pd || !vm->vcpu ||
	    ((cfg->flags & HVX_VM_HOSTCALL) && (!vm->hc_out || !vm->hc_in)) ||
	    ((cfg->flags & HVX_VM_DIRTY_LOG) && !vm->dl_ram)) {
		hvx_vm_free_pages(vm);
		return -ENOMEM;
	}
	vm->hc_on = !!(cfg->flags & HVX_VM_HOSTCALL);
	vm->dl_on = !!(cfg->flags & HVX_VM_DIRTY_LOG);
	vm->n_guest_pages = n;
	vm->n_low_pages = nlow;
	vm->n_ept_pt = npt;
	vm->n_ept_pd = npd;
	vm->n_vcpus = nv;
	vm->cur_vcpu = 0;
	vm->idle_hb = (cfg->flags & HVX_VM_IDLE_HEARTBEAT) ? 1 : 0;
	vm->cpu_v2 = (cfg->flags & HVX_VM_X86_64_V2) ? 1 : 0;
	vm->cpu_v3 = (cfg->flags & HVX_VM_X86_64_V3) ? 1 : 0;
	if (vm->cpu_v3)
		vm->cpu_v2 = 1;		/* v3 includes v2 */
	vm->pmu_clock = (cfg->flags & HVX_VM_PMU_CLOCK) ? 1 : 0;
	vm->pause_exit = (cfg->flags & HVX_VM_PAUSE_EXIT) ? 1 : 0;
	vm->pt_allowed = !!(cfg->flags & HVX_VM_PT_TRACE);
	vm->sched_on = (cfg->flags & HVX_VM_SMP_SCHED) && nv > 1;
	vm->smp_boot = vm->sched_on && (cfg->flags & HVX_VM_SMP_BOOT);
	memset(&vm->sched, 0, sizeof(vm->sched));
	vm->sched.quantum = HVX_SCHED_QUANTUM;
	vm->turn_margin = HVX_PMU_MARGIN;
	memset(&vm->tc, 0, sizeof(vm->tc));
	vm->sched.cur = nv - 1;			/* the first pick is vCPU 0 */
	vm->sched.tsc_step = 1;			/* the VM clock starts as every vCPU's (seed_vcpu) */
	vm->sched.tsc_mode = HVX_TSC_COUNTER;
	if (pt_force && hvx_pt_ok) {		/* module-parameter tracing (see pt_force) */
		u64 lo[2] = { pt_force_lo0, pt_force_lo1 }, hi[2] = { pt_force_hi0, pt_force_hi1 };
		u32 fl = ((pt_force & 1) ? HVX_PT_KERNEL : 0) | ((pt_force & 2) ? HVX_PT_USER : 0) |
			 (pt_force_noretc ? HVX_PT_NORETCOMP : 0);

		if (hvx_pt_setup(vm, pt_force_mib, fl, lo, hi, 0))
			pr_warn("hvx: pt_force: could not set up the trace buffer\n");
	}
	vm->pmu_period = HVX_PMU_PERIOD;
	vm->pmu_tick_tsc = HVX_PMU_TICK_TSC;

	for (i = 0; i < n && ok; i++) {
		if (!(vm->guest_pages[i] = __get_free_page(HVX_GFP_BULK)) || fatal_signal_pending(current))
			ok = 0;
		if (!(i & 511))
			cond_resched();
	}
	/* after a failed guest-RAM allocation nothing more is allocated (M97): RAM is
	 * then nearly exhausted, and these GFP_KERNEL pages would call the OOM killer */
	for (i = 0; i < npt && ok; i++)
		if (!(vm->ept_pt[i] = __get_free_page(GFP_KERNEL | __GFP_ZERO)))
			ok = 0;
	for (i = 0; i < npd && ok; i++)
		if (!(vm->ept_pd[i] = __get_free_page(GFP_KERNEL | __GFP_ZERO)))
			ok = 0;
	for (i = 0; i < 2 && ok; i++)
		if (!(vm->ept_pg[i] = __get_free_page(GFP_KERNEL | __GFP_ZERO)))
			ok = 0;
	/* one VMXON + one VMCS page PER vCPU (each is loaded only while that vCPU
	 * runs; the vCPUs are serialized, so a single host CPU touches one at a time) */
	for (i = 0; i < nv && ok; i++) {
		vm->vcpu[i].vmxon_page = __get_free_page(GFP_KERNEL | __GFP_ZERO);
		vm->vcpu[i].vmcs_page  = __get_free_page(GFP_KERNEL | __GFP_ZERO);
		vm->vcpu[i].fx_page    = __get_free_page(GFP_KERNEL | __GFP_ZERO);
		if (!vm->vcpu[i].vmxon_page || !vm->vcpu[i].vmcs_page || !vm->vcpu[i].fx_page)
			ok = 0;
		else
			hvx_fx_init((u8 *)vm->vcpu[i].fx_page);
		vm->vcpu[i].xcr0 = vm->cpu_v3 ? 1 : 0;	/* XCR0 reset value (guest AVX) */
	}
	if (!ok) {
		hvx_vm_free_pages(vm);
		return -ENOMEM;
	}

	build_guest_page_tables(vm);
	build_guest_system(vm);
	vm->eptp = build_ept(vm);
	if (vm->dl_on)
		vm->eptp |= 1ULL << 6;		/* EPT accessed/dirty flags (the RAM dirty log) */

	/* seed each vCPU to the deterministic power-on defaults (userspace overrides
	 * via HVX_SET_VCPU + HVX_SET_REGS / HVX_SET_TSC / HVX_SET_SREGS) */
	for (i = 0; i < nv; i++)
		seed_vcpu(&vm->vcpu[i], vm, i);
	/* SMP boot: only the boot CPU runs; the others wait for INIT + STARTUP */
	if (vm->smp_boot)
		for (i = 1; i < nv; i++)
			vm->vcpu[i].halted = HVX_HALT_SIPI;

	/* SHARED device model (milestone 15). x2APIC per-vCPU is seeded above; the
	 * console, IOAPIC, both PIC masks, the 16550 UART, and the input ring are one
	 * shared instance. */
	vm->console_len = 0;
	memset(&vm->ioapic, 0, sizeof(vm->ioapic));
	memset(&vm->uart, 0, sizeof(vm->uart));
	vm->pic_mask[0] = 0xff;
	vm->pic_mask[1] = 0xff;
	vm->input_head = vm->input_tail = 0;	/* empty serial input ring (M18) */
	vm->fuzz_len = vm->fuzz_off = 0;		/* no staged fuzz program (Phase 3 port inert) */

	spin_lock(&vm->map_lock);	/* guest RAM may now be mmap-faulted (M97) */
	vm->ram_ready = true;
	spin_unlock(&vm->map_lock);

	pr_info("hvx: VM created: %lu guest pages (%lu MiB, %u EPT PT / %u EPT PD), %u vCPU(s), eptp=0x%llx\n",
		n, n * PAGE_SIZE / (1024 * 1024), vm->n_ept_pt, vm->n_ept_pd, nv, vm->eptp);
	return 0;
}

/* ---- char device ---- */

static int hvx_open(struct inode *inode, struct file *filp)
{
	/* kvzalloc: the VM struct is about 100 KiB (64 KiB console, input, fuzz and
	 * network buffers), an order-5 block if contiguous, which fails on a fragmented host
	 * ("open: Cannot allocate memory"). Nothing needs it contiguous: every page whose
	 * physical address is used (guest RAM, EPT, VMCS/VMXON) is allocated separately. */
	struct hvx_vm *vm = kvzalloc(sizeof(*vm), GFP_KERNEL);

	if (!vm)
		return -ENOMEM;
	mutex_init(&vm->lock);
	spin_lock_init(&vm->map_lock);
	filp->private_data = vm;
	return 0;
}

/*
 * DIAGNOSTIC: dump the per-vCPU VM-exit histogram to dmesg at VM teardown.
 * Kernel-internal performance instrumentation only -- reads counters that are
 * never part of guest-visible or deterministic state, so it is fingerprint-
 * neutral. Prints total exits, wall-nanoseconds spent in hvx_run_vcpu, the
 * derived exits/second, and every nonzero exit-reason bucket (count + %).
 */
static void hvx_dump_exit_stats(struct hvx_vm *vm)
{
	unsigned int i, b;

	if (!vm->vcpu)
		return;
	for (i = 0; i < vm->n_vcpus; i++) {
		struct hvx_vcpu *vc = &vm->vcpu[i];
		u64 total = vc->exit_total, ns = vc->run_ns;
		u64 eps = ns ? div64_u64(total * 1000000000ULL, ns) : 0;

		if (!total)
			continue;
		pr_info("hvx: EXIT-HISTOGRAM vcpu=%u total=%llu run_ns=%llu (%llu.%03llu ms) exits/s=%llu icount=%llu rdtsc=%llu reinjected=%llu\n",
			i, total, ns, ns / 1000000ULL, (ns / 1000ULL) % 1000ULL,
			eps, vc->icount, vc->rdtsc_count, vc->reinj_count);
		if (vc->fts.ovf || vc->fts.step_entries) {
			const struct hvx_ftstat_vcpu *f = &vc->fts;

			pr_info("hvx: TURNSTAT vcpu=%u calls=%llu steps=%llu (turn %llu tick %llu) step_cyc=%llu ovf=%llu skid_max=%llu skid_sum=%llu hist=%llu,%llu,%llu,%llu,%llu,%llu,%llu,%llu,%llu,%llu rearms=%llu\n",
				i, f->calls, f->step_entries, f->steps_turn, f->steps_tick,
				f->step_cyc, f->ovf, f->skid_max, f->skid_sum,
				f->skid_hist[0], f->skid_hist[1], f->skid_hist[2], f->skid_hist[3],
				f->skid_hist[4], f->skid_hist[5], f->skid_hist[6], f->skid_hist[7],
				f->skid_hist[8], f->skid_hist[9], f->rearms);
		}
		for (b = 0; b < 64; b++) {
			u64 c = vc->exit_hist[b];
			u64 pct10;			/* percent * 10 (one decimal) */

			if (!c)
				continue;
			pct10 = total ? div64_u64(c * 1000ULL, total) : 0;
			pr_info("hvx:   reason %2u %-14s %12llu  %3llu.%01llu%%\n",
				b, reason_name(b), c, pct10 / 10ULL, pct10 % 10ULL);
		}
	}
}

static int hvx_release(struct inode *inode, struct file *filp)
{
	struct hvx_vm *vm = filp->private_data;

	if (vm) {
		hvx_dump_exit_stats(vm);	/* DIAGNOSTIC (fingerprint-neutral) */
		hvx_vm_free_pages(vm);
		kvfree(vm);
	}
	return 0;
}

/* Fault-in guest RAM to userspace: page-offset i -> guest_pages[i]. */
static vm_fault_t hvx_vm_fault(struct vm_fault *vmf)
{
	struct hvx_vm *vm = vmf->vma->vm_private_data;
	pgoff_t idx = vmf->pgoff;
	struct page *page = NULL;

	if (!vm)
		return VM_FAULT_SIGBUS;
	/* M97: a failing HVX_CREATE_VM in another thread may be freeing guest_pages[] */
	spin_lock(&vm->map_lock);
	if (vm->ram_ready && idx < vm->n_guest_pages && vm->guest_pages[idx]) {
		page = virt_to_page(vm->guest_pages[idx]);
		get_page(page);
	}
	spin_unlock(&vm->map_lock);
	if (!page)
		return VM_FAULT_SIGBUS;
	vmf->page = page;
	return 0;
}

static const struct vm_operations_struct hvx_vm_ops = {
	.fault = hvx_vm_fault,
};

/* Fault-in the backing disk image to userspace (milestone 28): the disk maps at
 * pgoff >= HVX_DISK_MMAP_PGOFF, so disk page i is at pgoff HVX_DISK_MMAP_PGOFF + i.
 * Mirrors hvx_vm_fault; get_page() keeps the per-page refcount correct (the M11
 * lesson -- each disk page is an individually-allocated order-0 page). */
static vm_fault_t hvx_disk_fault(struct vm_fault *vmf)
{
	struct hvx_vm *vm = vmf->vma->vm_private_data;
	unsigned long idx = vmf->pgoff - HVX_DISK_MMAP_PGOFF;
	struct page *page = NULL;

	if (!vm)
		return VM_FAULT_SIGBUS;
	/* M97: HVX_SET_DISK in another thread may be replacing the page array; it
	 * swaps it under map_lock and frees the old one after, and the get_page()
	 * below keeps this page alive for the mapping */
	spin_lock(&vm->map_lock);
	if (vm->vblk.disk_pages && idx < vm->vblk.n_disk_pages && vm->vblk.disk_pages[idx]) {
		page = virt_to_page(vm->vblk.disk_pages[idx]);
		get_page(page);
	}
	spin_unlock(&vm->map_lock);
	if (!page)
		return VM_FAULT_SIGBUS;
	vmf->page = page;
	return 0;
}

static const struct vm_operations_struct hvx_disk_ops = {
	.fault = hvx_disk_fault,
};

static int hvx_mmap(struct file *filp, struct vm_area_struct *vma)
{
	struct hvx_vm *vm = filp->private_data;

	if (!vm)
		return -EINVAL;

	/* Disk window (M28): pgoff >= HVX_DISK_MMAP_PGOFF maps the backing disk image
	 * zero-copy. Must start at the disk window base and fit within the image. */
	if (vma->vm_pgoff >= HVX_DISK_MMAP_PGOFF) {
		unsigned long npg = (vma->vm_end - vma->vm_start) >> PAGE_SHIFT;

		if (!vm->vblk.disk_pages)
			return -EINVAL;
		if (vma->vm_pgoff != HVX_DISK_MMAP_PGOFF)	/* map from the disk start */
			return -EINVAL;
		if (npg > vm->vblk.n_disk_pages)
			return -EINVAL;
		vma->vm_private_data = vm;
		vma->vm_ops = &hvx_disk_ops;
		return 0;
	}

	/* Guest-RAM window (unchanged): pgoff 0 maps guest RAM. */
	if (!vm->n_guest_pages)
		return -EINVAL;
	if (vma->vm_pgoff != 0)
		return -EINVAL;
	if (vma->vm_end - vma->vm_start > (unsigned long)vm->n_guest_pages * PAGE_SIZE)
		return -EINVAL;

	vma->vm_private_data = vm;
	vma->vm_ops = &hvx_vm_ops;
	return 0;
}

/* ---- milestone 138: the dirty-page log and the in-place reset ---- */

/* Fold the EPT dirty flags of guest RAM into dl_ram; with `clear`, clear the flags. The VM is
 * not running (vm->lock is held), and every run's entry does INVEPT, so a cleared flag is set
 * again by the guest's next write to that page. */
static void hvx_dl_fold(struct hvx_vm *vm, bool clear)
{
	unsigned int k, i;

	if (!vm->dl_ram)
		return;
	for (k = 0; k < vm->n_ept_pt; k++) {
		u64 *l1 = (u64 *)vm->ept_pt[k];

		for (i = 0; i < EPT_PT_LEAVES; i++) {
			unsigned int pg = k * EPT_PT_LEAVES + i;

			if (pg >= vm->n_guest_pages)
				break;
			if (l1[i] & (1ULL << 9)) {
				__set_bit(pg, vm->dl_ram);
				if (clear)
					l1[i] &= ~(1ULL << 9);
			}
		}
	}
}

static void hvx_dl_clear(struct hvx_vm *vm)
{
	hvx_dl_fold(vm, true);
	if (vm->dl_ram)
		bitmap_zero(vm->dl_ram, vm->n_guest_pages);
	if (vm->vblk.dl)
		bitmap_zero(vm->vblk.dl, vm->vblk.dl_pages);
}

/*
 * HVX_RESET_VM: the state HVX_CREATE_VM (and the first HVX_SET_DISK) left, except the contents
 * of guest RAM and of the disk, which the caller restores. Every vCPU is cleared and seeded as
 * hvx_create_vm does (its VMXON, VMCS and FPU pages are kept; the VMCS is not current anywhere:
 * every run ends with VMCLEAR); the shared device models and buffers, the policies and every
 * statistic are set as in hvx_create_vm; the disk keeps its pages, its size, the volatile
 * cache's buffer and its dirty-log bitmap, and everything else of the device goes back to its
 * first-attach values; the dirty log is cleared.
 */
static void hvx_reset_vm(struct hvx_vm *vm)
{
	struct hvx_vblk *d = &vm->vblk;
	unsigned int i, nv = vm->n_vcpus;

	for (i = 0; i < nv; i++) {
		struct hvx_vcpu *vc = &vm->vcpu[i];
		unsigned long vmxon = vc->vmxon_page, vmcs = vc->vmcs_page, fx = vc->fx_page;

		memset(vc, 0, sizeof(*vc));
		vc->vmxon_page = vmxon;
		vc->vmcs_page = vmcs;
		vc->fx_page = fx;
		memset((void *)vmxon, 0, PAGE_SIZE);
		memset((void *)vmcs, 0, PAGE_SIZE);
		memset((void *)fx, 0, PAGE_SIZE);
		hvx_fx_init((u8 *)fx);
		vc->xcr0 = vm->cpu_v3 ? 1 : 0;
		seed_vcpu(vc, vm, i);
	}
	if (vm->smp_boot)
		for (i = 1; i < nv; i++)
			vm->vcpu[i].halted = HVX_HALT_SIPI;
	vm->cur_vcpu = 0;
	memset(&vm->sched, 0, sizeof(vm->sched));
	vm->sched.quantum = HVX_SCHED_QUANTUM;
	vm->turn_margin = HVX_PMU_MARGIN;
	memset(&vm->tc, 0, sizeof(vm->tc));
	vm->sched.cur = nv - 1;
	vm->sched.tsc_step = 1;
	vm->sched.tsc_mode = HVX_TSC_COUNTER;
	vm->sched_stop = 0;
	vm->tj_on = false;
	memset(&vm->tj, 0, sizeof(vm->tj));
	vm->xt_on = false;
	memset(&vm->xt, 0, sizeof(vm->xt));
	vm->pmu_period = HVX_PMU_PERIOD;
	vm->pmu_tick_tsc = HVX_PMU_TICK_TSC;

	vm->console_len = 0;
	memset(vm->console, 0, sizeof(vm->console));
	memset(&vm->ioapic, 0, sizeof(vm->ioapic));
	memset(&vm->uart, 0, sizeof(vm->uart));
	vm->pic_mask[0] = 0xff;
	vm->pic_mask[1] = 0xff;
	vm->input_head = vm->input_tail = 0;
	memset(vm->input_buf, 0, sizeof(vm->input_buf));
	vm->fuzz_len = vm->fuzz_off = 0;
	memset(vm->fuzz_buf, 0, sizeof(vm->fuzz_buf));

	spin_lock(&vm->map_lock);	/* the disk mmap fault handler reads disk_pages (M97) */
	{
		unsigned long *pages = d->disk_pages, *dl = d->dl;
		unsigned int np = d->n_disk_pages, dlp = d->dl_pages;
		u64 size = d->disk_size;
		u8 *unsync = d->unsync_data;

		memset(d, 0, sizeof(*d));
		d->disk_pages = pages;
		d->n_disk_pages = np;
		d->disk_size = size;
		d->unsync_data = unsync;
		d->dl = dl;
		d->dl_pages = dlp;
		if (pages) {		/* HVX_SET_DISK's first-attach values */
			d->pci_bar0 = (u32)HVX_VBLK_BAR;
			d->pci_intline = HVX_VBLK_GSI;
			d->qsize = HVX_VBLK_QSIZE;
			d->irq_mode = VBLK_IRQ_MODE_INTX;
			d->msix_control = 0;
			d->config_msix_vector = HVX_MSIX_NO_VECTOR;
			d->queue_msix_vector = HVX_MSIX_NO_VECTOR;
		}
	}
	spin_unlock(&vm->map_lock);
	memset(&vm->vnet, 0, sizeof(vm->vnet));
	vm->fts_sched_cyc = vm->fts_sched_turns = 0;

	vm->hc_exit = vm->hc_ovf = false;
	vm->hc_out_len = vm->hc_out_done = 0;
	vm->hc_in_len = vm->hc_in_off = 0;
	vm->hc_records = vm->hc_errors = 0;
	hvx_dl_clear(vm);
}

static long hvx_ioctl(struct file *filp, unsigned int cmd, unsigned long arg)
{
	struct hvx_vm *vm = filp->private_data;
	void __user *uarg = (void __user *)arg;
	struct hvx_vcpu *vc;	/* the per-vCPU ioctl target (HVX_SET_VCPU selects it) */
	long ret;

	if (!vm)
		return -EINVAL;
	if (mutex_lock_interruptible(&vm->lock))
		return -ERESTARTSYS;

	/* Post-CREATE, vm->vcpu is allocated and cur_vcpu is bounded [0, n_vcpus);
	 * every per-vCPU case below first checks n_guest_pages (=> vcpu allocated),
	 * so vc is valid wherever it is used. */
	vc = vm->vcpu ? &vm->vcpu[vm->cur_vcpu] : NULL;

	switch (cmd) {
	case HVX_CREATE_VM: {
		struct hvx_config cfg;

		if (copy_from_user(&cfg, uarg, sizeof(cfg))) { ret = -EFAULT; break; }
		ret = hvx_create_vm(vm, &cfg);
		break;
	}
	case HVX_SET_REGS:
		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		ret = copy_from_user(&vc->regs, uarg, sizeof(vc->regs)) ? -EFAULT : 0;
		break;
	case HVX_GET_REGS:
		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		ret = copy_to_user(uarg, &vc->regs, sizeof(vc->regs)) ? -EFAULT : 0;
		break;
	case HVX_SET_TSC: {
		struct hvx_tsc t;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&t, uarg, sizeof(t))) { ret = -EFAULT; break; }
		if (t.mode != HVX_TSC_COUNTER && t.mode != HVX_TSC_ICOUNT) {
			ret = -EINVAL;
			break;
		}
		vc->tsc = t.tsc;
		vc->tsc_step = t.step;
		vc->tsc_aux = t.aux;
		vc->tsc_mode = t.mode;
		vc->rdtsc_count = 0;
		if (vm->sched_on)
			hvx_clk_take(vm, vc);	/* one clock for the whole VM */
		ret = 0;
		break;
	}
	case HVX_GET_TSC: {
		struct hvx_tsc t;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		t.tsc = vc->tsc;
		t.step = vc->tsc_step;
		t.aux = vc->tsc_aux;
		t.count = vc->rdtsc_count;
		t.mode = vc->tsc_mode;
		ret = copy_to_user(uarg, &t, sizeof(t)) ? -EFAULT : 0;
		break;
	}
	case HVX_GET_SNAPSHOT: {
		struct hvx_snapshot *s;	/* heap: too big for the ioctl stack frame */

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		s = kzalloc(sizeof(*s), GFP_KERNEL);
		if (!s) { ret = -ENOMEM; break; }
		s->regs = vc->regs;
		s->tsc = vc->tsc;
		s->tsc_step = vc->tsc_step;
		s->tsc_aux = vc->tsc_aux;
		s->tsc_mode = vc->tsc_mode;
		s->icount = vc->icount;
		s->rdtsc_count = vc->rdtsc_count;
		s->inject_at_icount = vc->inject_at_icount;
		s->inject_vector = vc->inject_vector;
		s->inject_valid = vc->inject_valid;
		s->n_guest_pages = vm->n_guest_pages;
		memcpy(s->sw_msr, vc->sw_msr, sizeof(s->sw_msr));
		s->sregs = vc->sregs;
		s->lapic_apic_base = vc->lapic.apic_base;
		s->lapic_tsc_deadline = vc->lapic.tsc_deadline;
		memcpy(s->lapic_reg, vc->lapic.reg, sizeof(s->lapic_reg));
		s->ioapic_ioregsel = vm->ioapic.ioregsel;
		s->ioapic_id = vm->ioapic.id;
		memcpy(s->ioapic_redir, vm->ioapic.redir, sizeof(s->ioapic_redir));
		s->pic_mask[0] = vm->pic_mask[0];
		s->pic_mask[1] = vm->pic_mask[1];
		s->lapic_in_service = vc->lapic.in_service;
		/* 16550 UART register latch (milestone 16) */
		s->uart_ier = vm->uart.ier;
		s->uart_lcr = vm->uart.lcr;
		s->uart_mcr = vm->uart.mcr;
		s->uart_fcr = vm->uart.fcr;
		s->uart_scr = vm->uart.scr;
		s->uart_dll = vm->uart.dll;
		s->uart_dlm = vm->uart.dlm;
		/* virtio-blk device control state (milestone 20); disk rides via GET_DISK */
		s->vblk_desc = vm->vblk.desc_gpa;
		s->vblk_avail = vm->vblk.avail_gpa;
		s->vblk_used = vm->vblk.used_gpa;
		s->vblk_qsize = vm->vblk.qsize;
		s->vblk_qready = vm->vblk.qready;
		s->vblk_last_avail = vm->vblk.last_avail;
		s->vblk_isr = vm->vblk.isr;
		s->vblk_irq_vec = vm->vblk.irq_vec;
		s->vblk_irq_mode = vm->vblk.irq_mode;
		s->vblk_irq_pending = vm->vblk.irq_pending;
		s->vblk_completion_armed = vm->vblk.completion_armed;
		s->vblk_completion_due = vm->vblk.completion_due;
		s->vblk_status = vm->vblk.status;
		s->vblk_qsel = vm->vblk.qsel;
		s->vblk_dfeat_sel = vm->vblk.dfeat_sel;
		s->vblk_gfeat_sel = vm->vblk.gfeat_sel;
		s->vblk_gfeatures = vm->vblk.gfeatures;
		s->vblk_pci_command = vm->vblk.pci_command;
		s->vblk_pci_bar0 = vm->vblk.pci_bar0;
		s->vblk_pci_cfg_addr = vm->vblk.pci_cfg_addr;
		s->vblk_pci_intline = vm->vblk.pci_intline;
		/* MSI-X interrupt state (milestone 21) */
		s->vblk_msix_control = vm->vblk.msix_control;
		s->vblk_config_msix = vm->vblk.config_msix_vector;
		s->vblk_queue_msix = vm->vblk.queue_msix_vector;
		memcpy(s->vblk_msix_tbl, vm->vblk.msix_tbl, sizeof(s->vblk_msix_tbl));
		/* deterministic fault-injection policy + PRNG state (milestone 22) */
		s->fault_seed = vm->vblk.fault_seed;
		s->fault_classes = vm->vblk.fault_classes;
		s->fault_probability = vm->vblk.fault_probability;
		s->fault_max_delay = vm->vblk.fault_max_delay;
		s->fault_kick_seq = vm->vblk.fault_kick_seq;
		s->fault_req_seq = vm->vblk.fault_req_seq;
		s->fault_count = vm->vblk.fault_count;
		/* virtio-net device control state (milestone 23); RX/TX frame bytes ride
		 * via HVX_PUT_RX / HVX_GET_TX (like the disk via GET/SET_DISK) */
		{
			int qi;

			s->vnet_present = vm->vnet.present;
			memcpy(s->vnet_mac, vm->vnet.mac, 6);
			s->vnet_dev_status = vm->vnet.status;
			s->vnet_qsel = vm->vnet.qsel;
			s->vnet_dfeat_sel = vm->vnet.dfeat_sel;
			s->vnet_gfeat_sel = vm->vnet.gfeat_sel;
			s->vnet_gfeatures = vm->vnet.gfeatures;
			s->vnet_pci_command = vm->vnet.pci_command;
			s->vnet_pci_bar0 = vm->vnet.pci_bar0;
			s->vnet_pci_intline = vm->vnet.pci_intline;
			s->vnet_isr = vm->vnet.isr;
			s->vnet_msix_control = vm->vnet.msix_control;
			s->vnet_config_msix = vm->vnet.config_msix_vector;
			memcpy(s->vnet_msix_tbl, vm->vnet.msix_tbl, sizeof(s->vnet_msix_tbl));
			for (qi = 0; qi < HVX_VNET_NQUEUES; qi++) {
				s->vnet_desc[qi] = vm->vnet.q[qi].desc_gpa;
				s->vnet_avail[qi] = vm->vnet.q[qi].avail_gpa;
				s->vnet_used[qi] = vm->vnet.q[qi].used_gpa;
				s->vnet_qsize[qi] = vm->vnet.q[qi].qsize;
				s->vnet_qready[qi] = vm->vnet.q[qi].qready;
				s->vnet_last_avail[qi] = vm->vnet.q[qi].last_avail;
				s->vnet_queue_msix[qi] = vm->vnet.q[qi].msix_vector;
				s->vnet_completion_armed[qi] = vm->vnet.q[qi].completion_armed;
				s->vnet_completion_due[qi] = vm->vnet.q[qi].completion_due;
				s->vnet_irq_pending[qi] = vm->vnet.q[qi].irq_pending;
			}
			s->vnet_rx_head = vm->vnet.rx_head;
			s->vnet_rx_tail = vm->vnet.rx_tail;
			s->vnet_tx_len = vm->vnet.tx_len;
			s->vnet_tx_count = vm->vnet.tx_count;
			s->vnet_rx_paced = vm->vnet.rx_paced;
			s->vnet_rx_next_deliver = vm->vnet.rx_next_deliver;
			/* deterministic net fault policy + PRNG state (milestone 24) */
			s->nfault_seed = vm->vnet.nfault_seed;
			s->nfault_classes = vm->vnet.nfault_classes;
			s->nfault_probability = vm->vnet.nfault_probability;
			s->nfault_max_delay = vm->vnet.nfault_max_delay;
			s->nfault_rx_kick_seq = vm->vnet.nfault_rx_kick_seq;
			s->nfault_tx_kick_seq = vm->vnet.nfault_tx_kick_seq;
			s->nfault_rx_seq = vm->vnet.nfault_rx_seq;
			s->nfault_tx_seq = vm->vnet.nfault_tx_seq;
			s->nfault_count = vm->vnet.nfault_count;
		}
		memcpy(s->fx, (void *)vc->fx_page, sizeof(s->fx));
		if (vm->cpu_v3) {	/* guest AVX: XCR0 + XSAVE header + YMM upper halves */
			s->xcr0 = vc->xcr0;
			memcpy(s->xsave_ext, (u8 *)vc->fx_page + HVX_XS_HDR, sizeof(s->xsave_ext));
		}
		s->pmu_total = vc->pmu_total;
		memcpy(s->irr, vc->irr, sizeof(s->irr));
		s->pmu_next = vc->pmu_next;
		ret = copy_to_user(uarg, s, sizeof(*s)) ? -EFAULT : 0;
		kfree(s);
		break;
	}
	case HVX_SET_SNAPSHOT: {
		struct hvx_snapshot *s;	/* heap: too big for the ioctl stack frame */

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		s = kzalloc(sizeof(*s), GFP_KERNEL);
		if (!s) { ret = -ENOMEM; break; }
		if (copy_from_user(s, uarg, sizeof(*s))) { kfree(s); ret = -EFAULT; break; }
		/* M97: vnet_tx_len is an offset into the fixed tx_buf (GET_TX copies that
		 * many bytes out of it, the next TX capture appends at it); a larger value
		 * read and wrote past the buffer. Only a forged snapshot can hold one. */
		if ((s->tsc_mode != HVX_TSC_COUNTER && s->tsc_mode != HVX_TSC_ICOUNT) ||
		    s->vnet_tx_len > HVX_VNET_TX_CAP) {
			kfree(s);
			ret = -EINVAL;
			break;
		}
		vc->regs = s->regs;
		vc->tsc = s->tsc;
		vc->tsc_step = s->tsc_step;
		vc->tsc_aux = s->tsc_aux;
		vc->tsc_mode = s->tsc_mode;
		vc->icount = s->icount;
		vc->rdtsc_count = s->rdtsc_count;
		vc->inject_at_icount = s->inject_at_icount;
		vc->inject_vector = s->inject_vector;
		vc->inject_valid = s->inject_valid;
		memcpy(vc->sw_msr, s->sw_msr, sizeof(vc->sw_msr));
		vc->sregs = s->sregs;
		vc->lapic.apic_base = s->lapic_apic_base;
		vc->lapic.tsc_deadline = s->lapic_tsc_deadline;
		memcpy(vc->lapic.reg, s->lapic_reg, sizeof(vc->lapic.reg));
		vm->ioapic.ioregsel = s->ioapic_ioregsel;
		vm->ioapic.id = s->ioapic_id;
		memcpy(vm->ioapic.redir, s->ioapic_redir, sizeof(vm->ioapic.redir));
		vm->pic_mask[0] = s->pic_mask[0];
		vm->pic_mask[1] = s->pic_mask[1];
		vc->lapic.in_service = s->lapic_in_service;
		/* 16550 UART register latch (milestone 16) */
		vm->uart.ier = s->uart_ier;
		vm->uart.lcr = s->uart_lcr;
		vm->uart.mcr = s->uart_mcr;
		vm->uart.fcr = s->uart_fcr;
		vm->uart.scr = s->uart_scr;
		vm->uart.dll = s->uart_dll;
		vm->uart.dlm = s->uart_dlm;
		/* virtio-blk device control state (milestone 20); disk via SET_DISK */
		vm->vblk.desc_gpa = s->vblk_desc;
		vm->vblk.avail_gpa = s->vblk_avail;
		vm->vblk.used_gpa = s->vblk_used;
		vm->vblk.qsize = s->vblk_qsize;
		vm->vblk.qready = s->vblk_qready;
		vm->vblk.last_avail = s->vblk_last_avail;
		vm->vblk.isr = s->vblk_isr;
		vm->vblk.irq_vec = s->vblk_irq_vec;
		vm->vblk.irq_mode = s->vblk_irq_mode;
		vm->vblk.irq_pending = s->vblk_irq_pending;
		vm->vblk.completion_armed = s->vblk_completion_armed;
		vm->vblk.completion_due = s->vblk_completion_due;
		vm->vblk.status = s->vblk_status;
		vm->vblk.qsel = s->vblk_qsel;
		vm->vblk.dfeat_sel = s->vblk_dfeat_sel;
		vm->vblk.gfeat_sel = s->vblk_gfeat_sel;
		vm->vblk.gfeatures = s->vblk_gfeatures;
		vm->vblk.pci_command = s->vblk_pci_command;
		vm->vblk.pci_bar0 = s->vblk_pci_bar0;
		vm->vblk.pci_cfg_addr = s->vblk_pci_cfg_addr;
		vm->vblk.pci_intline = s->vblk_pci_intline;
		/* MSI-X interrupt state (milestone 21) */
		vm->vblk.msix_control = s->vblk_msix_control;
		vm->vblk.config_msix_vector = s->vblk_config_msix;
		vm->vblk.queue_msix_vector = s->vblk_queue_msix;
		memcpy(vm->vblk.msix_tbl, s->vblk_msix_tbl, sizeof(vm->vblk.msix_tbl));
		/* deterministic fault-injection policy + PRNG state (milestone 22); a
		 * branch restores the exact schedule so subsequent faults reproduce. */
		vm->vblk.fault_seed = s->fault_seed;
		vm->vblk.fault_classes = s->fault_classes;
		vm->vblk.fault_probability = s->fault_probability;
		vm->vblk.fault_max_delay = s->fault_max_delay;
		vm->vblk.fault_kick_seq = s->fault_kick_seq;
		vm->vblk.fault_req_seq = s->fault_req_seq;
		vm->vblk.fault_count = s->fault_count;
		/* virtio-net device control state (milestone 23); a branch restores the
		 * queues/completion/MSI-X so subsequent RX/TX reproduce. The RX/TX frame
		 * bytes are NOT here -- they ride via HVX_PUT_RX / HVX_GET_TX (and the
		 * delivered RX frame is already in guest RAM, restored by the RAM copy). */
		{
			int qi;

			vm->vnet.present = s->vnet_present;
			memcpy(vm->vnet.mac, s->vnet_mac, 6);
			vm->vnet.status = s->vnet_dev_status;
			vm->vnet.qsel = s->vnet_qsel;
			vm->vnet.dfeat_sel = s->vnet_dfeat_sel;
			vm->vnet.gfeat_sel = s->vnet_gfeat_sel;
			vm->vnet.gfeatures = s->vnet_gfeatures;
			vm->vnet.pci_command = s->vnet_pci_command;
			vm->vnet.pci_bar0 = s->vnet_pci_bar0;
			vm->vnet.pci_intline = s->vnet_pci_intline;
			vm->vnet.isr = s->vnet_isr;
			vm->vnet.msix_control = s->vnet_msix_control;
			vm->vnet.config_msix_vector = s->vnet_config_msix;
			memcpy(vm->vnet.msix_tbl, s->vnet_msix_tbl, sizeof(vm->vnet.msix_tbl));
			for (qi = 0; qi < HVX_VNET_NQUEUES; qi++) {
				vm->vnet.q[qi].desc_gpa = s->vnet_desc[qi];
				vm->vnet.q[qi].avail_gpa = s->vnet_avail[qi];
				vm->vnet.q[qi].used_gpa = s->vnet_used[qi];
				vm->vnet.q[qi].qsize = s->vnet_qsize[qi];
				vm->vnet.q[qi].qready = s->vnet_qready[qi];
				vm->vnet.q[qi].last_avail = s->vnet_last_avail[qi];
				vm->vnet.q[qi].msix_vector = s->vnet_queue_msix[qi];
				vm->vnet.q[qi].completion_armed = s->vnet_completion_armed[qi];
				vm->vnet.q[qi].completion_due = s->vnet_completion_due[qi];
				vm->vnet.q[qi].irq_pending = s->vnet_irq_pending[qi];
			}
			vm->vnet.rx_head = s->vnet_rx_head;
			vm->vnet.rx_tail = s->vnet_rx_tail;
			vm->vnet.tx_len = s->vnet_tx_len;
			vm->vnet.tx_count = s->vnet_tx_count;
			vm->vnet.rx_paced = s->vnet_rx_paced;
			vm->vnet.rx_next_deliver = s->vnet_rx_next_deliver;
			/* deterministic net fault policy + PRNG state (milestone 24); a branch
			 * restores the exact schedule so subsequent RX/TX faults reproduce. */
			vm->vnet.nfault_seed = s->nfault_seed;
			vm->vnet.nfault_classes = s->nfault_classes;
			vm->vnet.nfault_probability = s->nfault_probability;
			vm->vnet.nfault_max_delay = s->nfault_max_delay;
			vm->vnet.nfault_rx_kick_seq = s->nfault_rx_kick_seq;
			vm->vnet.nfault_tx_kick_seq = s->nfault_tx_kick_seq;
			vm->vnet.nfault_rx_seq = s->nfault_rx_seq;
			vm->vnet.nfault_tx_seq = s->nfault_tx_seq;
			vm->vnet.nfault_count = s->nfault_count;
		}
		memcpy((void *)vc->fx_page, s->fx, sizeof(s->fx));
		vc->pmu_total = s->pmu_total;
		memcpy(vc->irr, s->irr, sizeof(vc->irr));
		vc->pmu_next = s->pmu_next;
		/* FXRSTOR #GPs in the HOST on a reserved MXCSR bit: never trust it */
		*(u32 *)((u8 *)vc->fx_page + 24) &= 0xffffU;
		if (vm->cpu_v3) {	/* guest AVX: XCR0 + XSAVE header + YMM upper halves */
			vc->xcr0 = hvx_xcr0_ok(s->xcr0) ? s->xcr0 : 1;
			memcpy((u8 *)vc->fx_page + HVX_XS_HDR, s->xsave_ext, sizeof(s->xsave_ext));
			hvx_xs_sanitize((u8 *)vc->fx_page);	/* XRSTOR runs in the host */
		}
		if (vm->sched_on)
			hvx_clk_take(vm, vc);	/* HVX_SET_SCHED, which follows, sets it too */
		kfree(s);
		ret = 0;
		break;
	}
	case HVX_RUN: {
		struct hvx_exit ex;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (vm->sched_on) {
			ret = hvx_sched_run(vm, &ex, 0);
			if (ret == 0 && copy_to_user(uarg, &ex, sizeof(ex)))
				ret = -EFAULT;
			break;
		}
		ret = hvx_run_resched(vc, &ex, false, 0);
		if (ret == 0) {
			const char *nm = ex.reason == HVX_EXIT_VMFAIL ? "VMFAIL" :
					 ex.reason == HVX_EXIT_THROTTLE ? "THROTTLE" :
					 reason_name(ex.reason);

			pr_info("hvx: vcpu%u VM-exit reason=%u (%s) rip=0x%llx insn_len=%u qual=0x%llx rdtsc_count=%llu\n",
				vc->index, ex.reason, nm, vc->regs.rip, ex.insn_len,
				ex.qualification, vc->rdtsc_count);
			if (copy_to_user(uarg, &ex, sizeof(ex)))
				ret = -EFAULT;
		}
		break;
	}
	case HVX_RUNC: {
		struct hvx_run_ctl ctl;
		struct hvx_exit ex;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&ctl, uarg, sizeof(ctl))) { ret = -EFAULT; break; }
		if (vm->sched_on) {
			/* the scheduler runs on the PMU clock; MTF single-stepping is the
			 * userspace round-robin of a VM without it */
			if (ctl.flags & HVX_RUN_SINGLE_STEP) { ret = -EINVAL; break; }
			if (ctl.flags & HVX_RUN_RESET_ICOUNT) {
				vm->sched.icount = 0;
				hvx_clk_sync(vm);
			}
			ret = hvx_sched_run(vm, &ex, ctl.max_insns);
			if (ret == 0) {
				ctl.icount = vm->sched.icount;
				ctl.qualification = ex.qualification;
				ctl.guest_phys = ex.guest_phys;
				ctl.reason = ex.reason;
				ctl.insn_len = ex.insn_len;
				if (copy_to_user(uarg, &ctl, sizeof(ctl)))
					ret = -EFAULT;
			}
			break;
		}
		if (ctl.flags & HVX_RUN_RESET_ICOUNT)
			vc->icount = 0;
		ret = hvx_run_resched(vc, &ex, !!(ctl.flags & HVX_RUN_SINGLE_STEP),
				      ctl.max_insns);
		if (ret == 0) {
			ctl.icount = vc->icount;
			ctl.qualification = ex.qualification;
			ctl.guest_phys = ex.guest_phys;
			ctl.reason = ex.reason;
			ctl.insn_len = ex.insn_len;
			if (copy_to_user(uarg, &ctl, sizeof(ctl)))
				ret = -EFAULT;
		}
		break;
	}
	case HVX_INJECT: {
		struct hvx_inject inj;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&inj, uarg, sizeof(inj))) { ret = -EFAULT; break; }
		if (inj.type != 0 || inj.vector > INTR_VECTOR_MAX) { ret = -EINVAL; break; }
		vc->inject_vector = inj.vector;
		vc->inject_at_icount = inj.at_icount;
		vc->inject_valid = 1;
		vc->lapic.in_service = 0;	/* force: not gated on EOI */
		ret = 0;
		break;
	}
	case HVX_GET_CONSOLE: {
		struct hvx_console __user *uc = uarg;
		__u32 len = vm->console_len;
		size_t n = len < HVX_CONSOLE_SZ ? len : HVX_CONSOLE_SZ;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		/* Copy straight from vm->console (the VM lock is held). A 64 KiB kzalloc here
		 * is an order-5 allocation that fails under host memory fragmentation, and a
		 * failed read made a harness miss its stop marker (M57). Bytes of buf past
		 * the console length are left as the caller had them. */
		ret = (put_user(len, &uc->len) || copy_to_user(uc->buf, vm->console, n)) ? -EFAULT : 0;
		break;
	}
	case HVX_GET_SREGS:
		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		ret = copy_to_user(uarg, &vc->sregs, sizeof(vc->sregs)) ? -EFAULT : 0;
		break;
	case HVX_SET_SREGS: {
		struct hvx_sregs sr;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&sr, uarg, sizeof(sr))) { ret = -EFAULT; break; }
		vc->sregs = sr;		/* setup_vmcs re-applies fixed_cr on the next run */
		ret = 0;
		break;
	}
	case HVX_SET_VCPU: {
		u32 idx;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&idx, uarg, sizeof(idx))) { ret = -EFAULT; break; }
		if (idx >= vm->n_vcpus) { ret = -EINVAL; break; }
		vm->cur_vcpu = idx;	/* subsequent per-vCPU ioctls target this vCPU */
		ret = 0;
		break;
	}
	case HVX_PUT_INPUT: {
		struct hvx_input in;	/* 260 bytes; fits the ioctl stack frame */
		u32 i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&in, uarg, sizeof(in))) { ret = -EFAULT; break; }
		for (i = 0; i < in.len && i < HVX_INPUT_MAX; i++)
			uart_rx_push(vm, in.buf[i]);	/* guest RBR reads drain it (M18) */
		ret = 0;
		break;
	}
	case HVX_SET_FUZZ_INPUT: {
		struct hvx_fuzz_input *fi;	/* ~8 KiB: heap, too big for the ioctl stack frame */
		u32 n;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		fi = kzalloc(sizeof(*fi), GFP_KERNEL);
		if (!fi) { ret = -ENOMEM; break; }
		if (copy_from_user(fi, uarg, sizeof(*fi))) { kfree(fi); ret = -EFAULT; break; }
		n = fi->len;
		if (n > HVX_FUZZ_SZ) n = HVX_FUZZ_SZ;
		memcpy(vm->fuzz_buf, fi->buf, n);
		vm->fuzz_len = n;
		vm->fuzz_off = 0;		/* per-restore: the guest re-reads from the start */
		kfree(fi);
		ret = 0;
		break;
	}
	case HVX_SET_DISK: {
		/* Attach / resize / (re)load the virtio-blk backing image (M20; page array
		 * as of M28). This ENABLES the device. Also used to restore the disk on
		 * branch/replay. */
		struct hvx_disk dk;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&dk, uarg, sizeof(dk))) { ret = -EFAULT; break; }
		if (dk.len == 0 || dk.len > HVX_DISK_MAX) { ret = -EINVAL; break; }
		/* Restoring/replacing the durable image resets the volatile write cache
		 * (M26): the backing bytes ARE the durable truth on a branch/replay. */
		vblk_unsync_clear(&vm->vblk);
		if (vm->vblk.disk_pages && vm->vblk.disk_size == dk.len) {
			/* same size: reuse the existing page array (restore path) */
		} else {
			/* (re)allocate the page-array backing image (M28, the M11 recipe);
			 * any prior image is freed inside vblk_disk_alloc. */
			if (vblk_disk_alloc(&vm->vblk, dk.len, &vm->map_lock) < 0) { ret = -ENOMEM; break; }
			if (vm->dl_on) {	/* M138: the disk part of the dirty log */
				kvfree(vm->vblk.dl);
				vm->vblk.dl_pages = 0;
				vm->vblk.dl = kvcalloc(BITS_TO_LONGS(vm->vblk.n_disk_pages),
						       sizeof(unsigned long), GFP_KERNEL);
				if (!vm->vblk.dl) { ret = -ENOMEM; break; }
				vm->vblk.dl_pages = vm->vblk.n_disk_pages;
			}
			/* first attach: default the transport-visible PCI registers */
			vm->vblk.pci_bar0 = (u32)HVX_VBLK_BAR;
			vm->vblk.pci_intline = HVX_VBLK_GSI;
			/* Device-advertised max queue size: modern virtio_pci reads it
			 * first and needs a nonzero power of two (M21). The driver writes
			 * back the ring size it creates; the Phase-A/B tests overwrite it. */
			vm->vblk.qsize = HVX_VBLK_QSIZE;
			/* Default to the INTx transport; the Phase-A PIO test opts into the
			 * DIRECT path via HVX_VPIO_IRQ_VEC, and real Linux's MSI-X overrides
			 * INTx once it enables the MSI-X capability (M21). Leaving it DIRECT
			 * (the 0 default) would wrongly route a PCI/Linux completion through
			 * the Phase-A one-shot path. */
			vm->vblk.irq_mode = VBLK_IRQ_MODE_INTX;
			/* MSI-X starts disabled with no vectors assigned (M21). */
			vm->vblk.msix_control = 0;
			vm->vblk.config_msix_vector = HVX_MSIX_NO_VECTOR;
			vm->vblk.queue_msix_vector = HVX_MSIX_NO_VECTOR;
		}
		/* Bulk image bytes: page-wise from userspace when data is supplied. dk.data
		 * == 0 loads a ZEROED image (freshly-allocated pages are __GFP_ZERO), for the
		 * M28 zero-copy path where userspace fills the disk through the mmap window. */
		if (dk.data) {
			ret = vblk_disk_from_user(&vm->vblk,
						  (void __user *)(unsigned long)dk.data, dk.len);
			if (ret)
				break;
		}
		/*
		 * Map the 4th GiB [3 GiB, 4 GiB) -- where the IOAPIC (0xfec00000) and the
		 * virtio BAR (0xfe000000) live -- into the guest's identity page tables via
		 * a 1 GiB PDPT leaf, so a hand-written guest can touch those MMIO regions
		 * (they stay EPT-unmapped, so accesses trap and are decoded in-kernel, like
		 * how real Linux ioremaps them). This is done ONLY when a disk is attached
		 * (virtio VMs), so the boot -- which never attaches a disk -- keeps its
		 * pristine low-1-GiB page tables and the M16 fingerprint is unchanged.
		 *
		 * A real OS (Linux, M21) builds its own page tables at these GPAs and
		 * ioremaps MMIO itself, so poking the module's fixed GPA_PDPT slot would
		 * corrupt the OS's PML4. It passes HVX_DISK_NO_IDMAP to skip the poke. */
		if (!(dk.flags & HVX_DISK_NO_IDMAP)) {
			u64 *pdpt = (u64 *)vm->guest_pages[GPA_PDPT >> 12];

			/* present|write|PS|Accessed|Dirty (1 GiB identity of the 4th GiB).
			 * The A/D bits are PRE-SET deliberately: this PTE maps the MMIO
			 * window (virtio BAR 0xfe000000, IOAPIC 0xfec00000) which is EPT-
			 * unmapped, so a guest MMIO access is a page walk that FAULTS at the
			 * EPT stage. Whether the CPU writes the ACCESSED bit into this guest
			 * PTE on such a faulting walk is implementation-specific and, under
			 * nested VMX, HOST-LOAD-DEPENDENT -- a nondeterministic 1-byte change
			 * to the guest page tables (which the deterministic fingerprint hashes)
			 * that made hvx_virtio_pci_test flip. Pre-setting A|D means the walker
			 * finds them already set and never writes, so the PTE is a pure
			 * function of the guest. (The low identity tables get A/D set by their
			 * own SUCCESSFUL walks, which is deterministic, so they need no fix.) */
			pdpt[3] = (3ULL << 30) | 0x83 | 0x60;
		}
		ret = 0;
		break;
	}
	case HVX_GET_DISK: {
		struct hvx_disk dk;
		u64 n;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (!vm->vblk.disk_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&dk, uarg, sizeof(dk))) { ret = -EFAULT; break; }
		n = dk.len < vm->vblk.disk_size ? dk.len : vm->vblk.disk_size;
		if (n) {
			ret = vblk_disk_to_user(&vm->vblk,
						(void __user *)(unsigned long)dk.data, n);
			if (ret)
				break;
		}
		dk.len = vm->vblk.disk_size;	/* report the true image size back */
		ret = copy_to_user(uarg, &dk, sizeof(dk)) ? -EFAULT : 0;
		break;
	}
	case HVX_SET_FAULT: {
		/* Install a deterministic disk fault-injection policy (milestone 22).
		 * A fresh policy resets the seeded schedule (req/kick counters), so the
		 * next request is decision #0. classes == 0 disables faults (default). */
		struct hvx_fault f;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&f, uarg, sizeof(f))) { ret = -EFAULT; break; }
		if (f.probability > 100) { ret = -EINVAL; break; }
		vm->vblk.fault_seed = f.seed;
		vm->vblk.fault_classes = f.classes;
		vm->vblk.fault_probability = f.probability;
		vm->vblk.fault_max_delay = f.max_delay;
		vm->vblk.fault_req_seq = 0;
		vm->vblk.fault_kick_seq = 0;
		vm->vblk.fault_count = 0;
		vblk_unsync_clear(&vm->vblk);
		/* M26: flush-aware DROP needs a volatile-write-cache buffer. Allocate it HERE
		 * (GFP_KERNEL, outside the IRQs-off run-loop) when the class is armed, so the
		 * per-write stash never has to allocate. Freed with the VM. */
		if ((f.classes & HVX_FAULT_DROP_UNFLUSHED) && !vm->vblk.unsync_data) {
			vm->vblk.unsync_data = kvmalloc((size_t)HVX_UNSYNC_MAX * HVX_UNSYNC_ENTBYTES,
							GFP_KERNEL);
			if (!vm->vblk.unsync_data) { ret = -ENOMEM; break; }
		}
		ret = 0;
		break;
	}
	case HVX_SET_PT: {
		struct hvx_pt_config pc;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&pc, uarg, sizeof(pc))) { ret = -EFAULT; break; }
		if (!vm->pt_allowed) { ret = -EPERM; break; }
		ret = hvx_pt_setup(vm, pc.buf_mib, pc.flags, pc.lo, pc.hi, pc.cr3);
		break;
	}
	case HVX_GET_PT: {
		struct hvx_pt_status ps;

		memset(&ps, 0, sizeof(ps));
		if (vm->pt_chunk) {
			ps.bytes = hvx_pt_bytes(vm->pt_mask, vm->pt_nchunk);
			ps.buf_bytes = (u64)vm->pt_nchunk << (PAGE_SHIFT + HVX_PT_CHUNK_ORDER);
			ps.status = vm->pt_status;
			ps.ctl = vm->pt_ctl;
		} else {
			mutex_lock(&hvx_pt_last_lock);
			ps.bytes = hvx_pt_last_bytes;
			ps.buf_bytes = (u64)hvx_pt_last_nchunk << (PAGE_SHIFT + HVX_PT_CHUNK_ORDER);
			ps.status = hvx_pt_last_status;
			ps.saved = 1;
			mutex_unlock(&hvx_pt_last_lock);
		}
		ret = copy_to_user(uarg, &ps, sizeof(ps)) ? -EFAULT : 0;
		break;
	}
	case HVX_READ_PT: {
		struct hvx_pt_read pr;

		if (copy_from_user(&pr, uarg, sizeof(pr))) { ret = -EFAULT; break; }
		if (vm->pt_chunk) {
			ret = hvx_pt_copy(vm->pt_chunk, vm->pt_nchunk,
					  hvx_pt_bytes(vm->pt_mask, vm->pt_nchunk), pr.offset, pr.len,
					  (void __user *)(unsigned long)pr.addr);
		} else {
			mutex_lock(&hvx_pt_last_lock);
			ret = hvx_pt_last_chunk
			    ? hvx_pt_copy(hvx_pt_last_chunk, hvx_pt_last_nchunk, hvx_pt_last_bytes,
					  pr.offset, pr.len, (void __user *)(unsigned long)pr.addr)
			    : -ENOENT;
			mutex_unlock(&hvx_pt_last_lock);
		}
		break;
	}
	case HVX_SET_PMU: {
		struct hvx_pmu pm;

		if (copy_from_user(&pm, uarg, sizeof(pm))) { ret = -EFAULT; break; }
		if (pm.period) {
			if (pm.period <= 2 * HVX_PMU_MARGIN) { ret = -EINVAL; break; }
			vm->pmu_period = pm.period;
		}
		if (pm.tick_tsc)
			vm->pmu_tick_tsc = pm.tick_tsc;
		ret = 0;
		break;
	}
	case HVX_GET_UNSYNC: {
		struct hvx_unsync_io io;
		struct hvx_vblk *d = &vm->vblk;
		u64 meta;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&io, uarg, sizeof(io))) { ret = -EFAULT; break; }
		io.n = d->unsync_n;
		meta = (u64)io.n * sizeof(struct hvx_unsync_rec);
		ret = 0;
		if (io.len < HVX_UNSYNC_BYTES(io.n))
			ret = -ENOSPC;
		else if (io.n &&
			 (copy_to_user((void __user *)(unsigned long)io.addr, d->unsync, meta) ||
			  copy_to_user((void __user *)(unsigned long)(io.addr + meta), d->unsync_data,
				       (size_t)io.n * HVX_UNSYNC_ENTBYTES)))
			ret = -EFAULT;
		if (copy_to_user(uarg, &io, sizeof(io)))
			ret = -EFAULT;
		break;
	}
	case HVX_SET_UNSYNC: {
		struct hvx_unsync_io io;
		struct hvx_vblk *d = &vm->vblk;
		u64 meta;
		u32 i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&io, uarg, sizeof(io))) { ret = -EFAULT; break; }
		if (io.n > HVX_UNSYNC_MAX || io.len < HVX_UNSYNC_BYTES(io.n)) { ret = -EINVAL; break; }
		ret = 0;
		vblk_unsync_clear(d);
		if (!io.n)
			break;
		if (!d->unsync_data) {
			d->unsync_data = kvmalloc((size_t)HVX_UNSYNC_MAX * HVX_UNSYNC_ENTBYTES, GFP_KERNEL);
			if (!d->unsync_data) { ret = -ENOMEM; break; }
		}
		meta = (u64)io.n * sizeof(struct hvx_unsync_rec);
		if (copy_from_user(d->unsync, (void __user *)(unsigned long)io.addr, meta) ||
		    copy_from_user(d->unsync_data, (void __user *)(unsigned long)(io.addr + meta),
				   (size_t)io.n * HVX_UNSYNC_ENTBYTES)) {
			ret = -EFAULT;
			break;
		}
		for (i = 0; i < io.n; i++)
			if (d->unsync[i].len > HVX_UNSYNC_ENTBYTES)
				ret = -EINVAL;
		if (ret)
			break;
		d->unsync_n = io.n;
		break;
	}
	case HVX_SET_TJITTER: {
		struct hvx_tjitter t;
		bool on;
		u32 i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&t, uarg, sizeof(t))) { ret = -EFAULT; break; }
		if (t.n_explicit > HVX_TJ_MAX_EXPLICIT) { ret = -EINVAL; break; }
		on = t.n_explicit || (t.max_delay && t.permille);
		ret = 0;
		if (on && !vm->pmu_clock)
			ret = -EINVAL;			/* the jitter moves PMU-clock ticks */
		if (!t.n_explicit && t.max_delay >= vm->pmu_period)
			ret = -EINVAL;
		for (i = 0; i < t.n_explicit; i++)
			if (t.explicit_[i].delay >= vm->pmu_period)
				ret = -EINVAL;
		if (ret)
			break;
		if (t.permille > 1000)
			t.permille = 1000;
		t.applied = t.applied_hash = t.last_tick = 0;
		vm->tj = t;
		vm->tj_on = on;
		for (i = 0; i < vm->n_vcpus; i++)
			vm->vcpu[i].tj_applied = vm->vcpu[i].tj_hash = vm->vcpu[i].tj_last = 0;
		break;
	}
	case HVX_SET_XTICK: {
		struct hvx_xtick x;
		bool on;
		u32 i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&x, uarg, sizeof(x))) { ret = -EFAULT; break; }
		if (x.n_explicit > HVX_XT_MAX_EXPLICIT || x.rate > HVX_XT_MAX_RATE) { ret = -EINVAL; break; }
		on = x.n_explicit || (x.rate && x.permille);
		ret = 0;
		if (on && !vm->pmu_clock)
			ret = -EINVAL;			/* extra ticks are PMU-clock ticks */
		if (ret)
			break;
		if (x.permille > 1000)
			x.permille = 1000;
		x.applied = x.applied_hash = x.first = x.last = 0;
		vm->xt = x;
		vm->xt_on = on;
		for (i = 0; i < vm->n_vcpus; i++) {
			struct hvx_vcpu *v = &vm->vcpu[i];

			v->xt_applied = v->xt_hash = v->xt_first = v->xt_last = 0;
			if (!on && v->pmu_next) {	/* back on the grid: the target may be an extra tick */
				u64 j = v->pmu_next / vm->pmu_period, m = hvx_xt_main(vm, j, hvx_clk_idx(vm, v));

				if (m <= v->pmu_total)
					m = hvx_xt_main(vm, j + 1, hvx_clk_idx(vm, v));
				if (m != v->pmu_next && m > v->pmu_total)
					v->pmu_next = m;
			}
			if (on && v->pmu_next) {	/* the next target may now be an extra tick (sched: VM clock, same for all) */
				u64 j = v->pmu_next / vm->pmu_period;
				u64 e = hvx_xt_extra_after(vm, j - (j > 0), v->pmu_total, hvx_clk_idx(vm, v));

				if (e > v->pmu_total && e < v->pmu_next)
					v->pmu_next = e;
				e = hvx_xt_extra_after(vm, j, v->pmu_total, hvx_clk_idx(vm, v));
				if (e > v->pmu_total && e < v->pmu_next)
					v->pmu_next = e;
			}
		}
		if (vm->sched_on)
			hvx_clk_take(vm, vc);
		break;
	}
	case HVX_GET_SCHED: {
		struct hvx_sched *sc;	/* heap: keep the ioctl frame small */
		unsigned int i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		sc = kzalloc(sizeof(*sc), GFP_KERNEL);
		if (!sc) { ret = -ENOMEM; break; }
		*sc = vm->sched;
		memset(sc->v, 0, sizeof(sc->v));
		sc->n_vcpus = vm->n_vcpus;
		for (i = 0; i < vm->n_vcpus; i++) {
			struct hvx_vcpu *v = &vm->vcpu[i];

			sc->v[i].branches = v->br_own;
			sc->v[i].turns = v->sched_turns;
			sc->v[i].cr2 = v->cr2;
			sc->v[i].halted = v->halted;
			sc->v[i].halt_len = v->halt_len;
			sc->v[i].reinj_valid = v->reinj_valid;
			sc->v[i].reinj_info = v->reinj_info;
			sc->v[i].reinj_ec = v->reinj_ec;
			sc->v[i].reinj_len = v->reinj_len;
		}
		ret = copy_to_user(uarg, sc, sizeof(*sc)) ? -EFAULT : 0;
		kfree(sc);
		break;
	}
	case HVX_SET_SCHED: {
		struct hvx_sched *sc;
		unsigned int i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		sc = kzalloc(sizeof(*sc), GFP_KERNEL);
		if (!sc) { ret = -ENOMEM; break; }
		if (copy_from_user(sc, uarg, sizeof(*sc))) { kfree(sc); ret = -EFAULT; break; }
		ret = 0;
		if (vm->sched_on && (!sc->quantum || sc->cur >= vm->n_vcpus ||
				     (sc->tsc_mode != HVX_TSC_COUNTER && sc->tsc_mode != HVX_TSC_ICOUNT)))
			ret = -EINVAL;
		for (i = 0; i < vm->n_vcpus; i++)
			if (sc->v[i].halt_len > 15 || sc->v[i].halted > HVX_HALT_SIPI ||
			    (sc->v[i].halted == HVX_HALT_SIPI && !vm->smp_boot) ||
			    (sc->v[i].reinj_valid && !(sc->v[i].reinj_info & 0x80000000U)))
				ret = -EINVAL;
		if (ret) { kfree(sc); break; }
		if (vm->sched_on) {
			vm->sched = *sc;
			memset(vm->sched.v, 0, sizeof(vm->sched.v));
			vm->sched.n_vcpus = vm->n_vcpus;
			hvx_clk_sync(vm);
		}
		for (i = 0; i < vm->n_vcpus; i++) {
			struct hvx_vcpu *v = &vm->vcpu[i];

			v->br_own = sc->v[i].branches;
			v->sched_turns = sc->v[i].turns;
			v->cr2 = sc->v[i].cr2;
			v->reinj_valid = !!sc->v[i].reinj_valid;
			v->reinj_info = sc->v[i].reinj_info & 0x80000fffU;
			v->reinj_ec = sc->v[i].reinj_ec;
			v->reinj_len = sc->v[i].reinj_len;
			if (vm->sched_on) {
				v->halted = (u8)sc->v[i].halted;
				v->halt_len = sc->v[i].halt_len;
			}
		}
		kfree(sc);
		break;
	}
	case HVX_SET_FLUSHCUT: {
		struct hvx_flushcut f;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&f, uarg, sizeof(f))) { ret = -EFAULT; break; }
		vm->vblk.fc_on = f.on ? 1 : 0;
		vm->vblk.fc_target = f.on ? f.target : 0;
		vm->vblk.fc_count = 0;
		vm->vblk.fc_off = 0;
		vm->vblk.fc_lo = f.lo;
		vm->vblk.fc_hi = f.hi;
		vm->vblk.fc_hung = 0;
		ret = 0;
		break;
	}
	case HVX_GET_FLUSHCUT: {
		struct hvx_flushcut f;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		memset(&f, 0, sizeof(f));
		f.on = vm->vblk.fc_on;
		f.off = vm->vblk.fc_off;
		f.target = vm->vblk.fc_target;
		f.count = vm->vblk.fc_count;
		f.lo = vm->vblk.fc_lo;
		f.hi = vm->vblk.fc_hi;
		f.hung = vm->vblk.fc_hung;
		ret = copy_to_user(uarg, &f, sizeof(f)) ? -EFAULT : 0;
		break;
	}
	case HVX_GET_XTICK: {
		struct hvx_xtick x;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		x = vm->xt;
		x.applied = vc->xt_applied;
		x.applied_hash = vc->xt_hash;
		x.first = vc->xt_first;
		x.last = vc->xt_last;
		ret = copy_to_user(uarg, &x, sizeof(x)) ? -EFAULT : 0;
		break;
	}
	case HVX_GET_TJITTER: {
		struct hvx_tjitter t;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		t = vm->tj;
		t.applied = vc->tj_applied;
		t.applied_hash = vc->tj_hash;
		t.last_tick = vc->tj_last;
		ret = copy_to_user(uarg, &t, sizeof(t)) ? -EFAULT : 0;
		break;
	}
	case HVX_SET_TURNCTL: {
		struct hvx_turnctl t;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&t, uarg, sizeof(t))) { ret = -EFAULT; break; }
		ret = 0;
		if (!vm->sched_on || (t.margin && (t.margin < 8 || t.margin > (1ULL << 20))) ||
		    t.burst_permille > 1000)
			ret = -EINVAL;
		if (t.burst_seed && t.burst_permille &&
		    (!t.burst_len || !t.burst_q || t.burst_len > (1ULL << 40) ||
		     t.burst_window > (1ULL << 40)))
			ret = -EINVAL;
		if (ret)
			break;
		t.burst_turns = 0;
		t.clipped = 0;
		t.pad0 = 0;
		memset(t.pad, 0, sizeof(t.pad));
		vm->tc = t;
		vm->turn_margin = t.margin ? t.margin : HVX_PMU_MARGIN;
		break;
	}
	case HVX_GET_TURNCTL: {
		struct hvx_turnctl t;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (!vm->sched_on) { ret = -EINVAL; break; }
		t = vm->tc;
		t.margin = vm->turn_margin;
		ret = copy_to_user(uarg, &t, sizeof(t)) ? -EFAULT : 0;
		break;
	}
	case HVX_GET_FTSTAT: {
		struct hvx_ftstat *f;	/* heap: keep the ioctl frame small */
		unsigned int i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		f = kzalloc(sizeof(*f), GFP_KERNEL);
		if (!f) { ret = -ENOMEM; break; }
		f->n_vcpus = vm->n_vcpus;
		f->tsc_khz = tsc_khz;
		f->sched_cyc = vm->fts_sched_cyc;
		f->sched_turns = vm->fts_sched_turns;
		for (i = 0; i < vm->n_vcpus && i < 8; i++)
			f->v[i] = vm->vcpu[i].fts;
		ret = copy_to_user(uarg, f, sizeof(*f)) ? -EFAULT : 0;
		kfree(f);
		break;
	}
	case HVX_RESET_FTSTAT: {
		unsigned int i;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		vm->fts_sched_cyc = 0;
		vm->fts_sched_turns = 0;
		for (i = 0; i < vm->n_vcpus; i++)
			memset(&vm->vcpu[i].fts, 0, sizeof(vm->vcpu[i].fts));
		ret = 0;
		break;
	}
	case HVX_GET_PMU: {
		struct hvx_pmu pm;

		/* M97: before HVX_CREATE_VM there is no vCPU (vc is NULL) */
		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		pm = (struct hvx_pmu){
			.period = vm->pmu_period, .tick_tsc = vm->pmu_tick_tsc,
			.total = vc->pmu_total, .next = vc->pmu_next, .ticks = vc->pmu_ticks,
			.overshoots = vc->pmu_over, .steps = vc->pmu_steps,
			.land_hash = vc->pmu_land,
			.hops = vc->pmu_hops,
		};
		ret = copy_to_user(uarg, &pm, sizeof(pm)) ? -EFAULT : 0;
		break;
	}
	case HVX_GET_FAULT: {
		struct hvx_fault f;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		memset(&f, 0, sizeof(f));
		f.seed = vm->vblk.fault_seed;
		f.classes = vm->vblk.fault_classes;
		f.probability = vm->vblk.fault_probability;
		f.max_delay = vm->vblk.fault_max_delay;
		f.req_seq = vm->vblk.fault_req_seq;
		f.fault_count = vm->vblk.fault_count;
		ret = copy_to_user(uarg, &f, sizeof(f)) ? -EFAULT : 0;
		break;
	}
	case HVX_NET_ATTACH: {
		/* Attach the virtio-net device (milestone 23). ENABLES the device (a VM
		 * with no net device is byte-identical to pre-M23). Seeds the advertised
		 * MAC and defaults the transport-visible PCI/MSI-X registers. */
		struct hvx_net_config nc;
		int qi;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (copy_from_user(&nc, uarg, sizeof(nc))) { ret = -EFAULT; break; }
		memset(&vm->vnet, 0, sizeof(vm->vnet));		/* fresh device (empty FIFO/capture) */
		vm->vnet.present = 1;
		vm->vnet.rx_paced = (nc.flags & HVX_NET_PACED) ? 1 : 0;
		vm->vnet.rx_next_deliver = 0;
		memcpy(vm->vnet.mac, nc.mac, 6);
		vm->vnet.pci_bar0 = (u32)HVX_VNET_BAR;
		vm->vnet.pci_intline = HVX_VNET_GSI;
		vm->vnet.config_msix_vector = HVX_MSIX_NO_VECTOR;
		for (qi = 0; qi < HVX_VNET_NQUEUES; qi++) {
			vm->vnet.q[qi].qsize = HVX_VNET_QSIZE;	/* device-advertised default */
			vm->vnet.q[qi].msix_vector = HVX_MSIX_NO_VECTOR;
		}
		/*
		 * Map the 4th GiB [3 GiB, 4 GiB) -- where the virtio BAR MMIO windows live
		 * -- into the guest's identity page tables (1 GiB PDPT leaf) so a HAND-
		 * WRITTEN guest can reach the BAR (it stays EPT-unmapped, so accesses trap
		 * and are decoded in-kernel). A/D are PRE-SET to avoid the M22-class page-
		 * table nondeterminism on a faulting walk over an EPT-unmapped window. A
		 * real OS builds its own tables + ioremaps MMIO itself -> HVX_NET_NO_IDMAP.
		 * (This is the SAME pdpt[3] slot HVX_SET_DISK pokes; harmless if both do.)
		 */
		if (!(nc.flags & HVX_NET_NO_IDMAP)) {
			u64 *pdpt = (u64 *)vm->guest_pages[GPA_PDPT >> 12];

			pdpt[3] = (3ULL << 30) | 0x83 | 0x60;	/* present|write|PS|Accessed|Dirty */
		}
		ret = 0;
		break;
	}
	case HVX_PUT_RX: {
		/* Push one scripted inbound Ethernet frame into the device's RX FIFO
		 * (milestone 23). Delivered deterministically at the next receiveq kick. */
		struct hvx_rx *rx;
		u32 slot;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (!vm->vnet.present) { ret = -EINVAL; break; }
		rx = kzalloc(sizeof(*rx), GFP_KERNEL);
		if (!rx) { ret = -ENOMEM; break; }
		if (copy_from_user(rx, uarg, sizeof(*rx))) { kfree(rx); ret = -EFAULT; break; }
		if (rx->len == 0 || rx->len > HVX_VNET_FRAME_MAX) { kfree(rx); ret = -EINVAL; break; }
		if (vm->vnet.rx_tail - vm->vnet.rx_head >= HVX_VNET_RX_SLOTS) {
			kfree(rx); ret = -EAGAIN; break;	/* FIFO full: host fed faster than delivery */
		}
		slot = vm->vnet.rx_tail & (HVX_VNET_RX_SLOTS - 1);
		memcpy(vm->vnet.rx_buf[slot], rx->buf, rx->len);
		vm->vnet.rx_flen[slot] = rx->len;
		vm->vnet.rx_tail++;
		kfree(rx);
		ret = 0;
		break;
	}
	case HVX_GET_TX: {
		/* Read back the captured outbound frames (milestone 23). */
		struct hvx_tx tx;
		u64 n;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (!vm->vnet.present) { ret = -EINVAL; break; }
		if (copy_from_user(&tx, uarg, sizeof(tx))) { ret = -EFAULT; break; }
		n = tx.len < vm->vnet.tx_len ? tx.len : vm->vnet.tx_len;
		if (n && copy_to_user((void __user *)(unsigned long)tx.data, vm->vnet.tx_buf, n)) {
			ret = -EFAULT;
			break;
		}
		tx.len = vm->vnet.tx_len;	/* report the true captured byte count / frame count */
		tx.count = vm->vnet.tx_count;
		ret = copy_to_user(uarg, &tx, sizeof(tx)) ? -EFAULT : 0;
		break;
	}
	case HVX_SET_NET_FAULT: {
		/* Install a deterministic virtio-net fault-injection policy (milestone 24).
		 * A fresh policy resets the seeded schedule (rx/tx/kick counters), so the
		 * next RX and TX packets are decision #0. classes == 0 disables faults. */
		struct hvx_net_fault f;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (!vm->vnet.present) { ret = -EINVAL; break; }
		if (copy_from_user(&f, uarg, sizeof(f))) { ret = -EFAULT; break; }
		if (f.probability > 100) { ret = -EINVAL; break; }
		vm->vnet.nfault_seed = f.seed;
		vm->vnet.nfault_classes = f.classes;
		vm->vnet.nfault_probability = f.probability;
		vm->vnet.nfault_max_delay = f.max_delay;
		vm->vnet.nfault_rx_seq = 0;
		vm->vnet.nfault_tx_seq = 0;
		vm->vnet.nfault_rx_kick_seq = 0;
		vm->vnet.nfault_tx_kick_seq = 0;
		vm->vnet.nfault_count = 0;
		ret = 0;
		break;
	}
	case HVX_GET_NET_FAULT: {
		struct hvx_net_fault f;

		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		memset(&f, 0, sizeof(f));
		f.seed = vm->vnet.nfault_seed;
		f.classes = vm->vnet.nfault_classes;
		f.probability = vm->vnet.nfault_probability;
		f.max_delay = vm->vnet.nfault_max_delay;
		f.rx_seq = vm->vnet.nfault_rx_seq;
		f.tx_seq = vm->vnet.nfault_tx_seq;
		f.fault_count = vm->vnet.nfault_count;
		ret = copy_to_user(uarg, &f, sizeof(f)) ? -EFAULT : 0;
		break;
	}
	case HVX_RESET_VM:
		if (!vm->n_guest_pages) { ret = -EINVAL; break; }
		if (vm->pt_on || vm->pt_chunk) { ret = -EBUSY; break; }
		hvx_dump_exit_stats(vm);	/* one histogram per run, as with a VM per run */
		hvx_reset_vm(vm);
		ret = 0;
		break;
	case HVX_GET_DIRTY: {
		struct hvx_dirty dd;
		struct hvx_vblk *d = &vm->vblk;
		u64 rb, db;

		if (!vm->n_guest_pages || !vm->dl_on) { ret = -EINVAL; break; }
		if (copy_from_user(&dd, uarg, sizeof(dd))) { ret = -EFAULT; break; }
		hvx_dl_fold(vm, !!(dd.flags & HVX_DIRTY_CLEAR));
		rb = (u64)BITS_TO_LONGS(vm->n_guest_pages) * sizeof(unsigned long);
		db = d->dl ? (u64)BITS_TO_LONGS(d->dl_pages) * sizeof(unsigned long) : 0;
		ret = 0;
		if ((dd.ram_addr && dd.ram_bytes < rb) || (dd.disk_addr && db && dd.disk_bytes < db))
			ret = -ENOSPC;
		else if ((dd.ram_addr &&
			  copy_to_user((void __user *)(unsigned long)dd.ram_addr, vm->dl_ram, rb)) ||
			 (dd.disk_addr && db &&
			  copy_to_user((void __user *)(unsigned long)dd.disk_addr, d->dl, db)))
			ret = -EFAULT;
		if (ret)
			break;
		dd.ram_dirty = bitmap_weight(vm->dl_ram, vm->n_guest_pages);
		dd.disk_dirty = d->dl ? bitmap_weight(d->dl, d->dl_pages) : 0;
		dd.ram_bytes = rb;
		dd.disk_bytes = db;
		if (dd.flags & HVX_DIRTY_CLEAR) {
			bitmap_zero(vm->dl_ram, vm->n_guest_pages);
			if (d->dl)
				bitmap_zero(d->dl, d->dl_pages);
		}
		ret = copy_to_user(uarg, &dd, sizeof(dd)) ? -EFAULT : 0;
		break;
	}
	case HVX_HC_GET: {
		struct hvx_hc_io io;
		u32 n;

		if (!vm->n_guest_pages || !vm->hc_on) { ret = -EINVAL; break; }
		if (copy_from_user(&io, uarg, sizeof(io))) { ret = -EFAULT; break; }
		n = io.len < vm->hc_out_done ? io.len : vm->hc_out_done;
		if (n && copy_to_user((void __user *)(unsigned long)io.addr, vm->hc_out, n)) {
			ret = -EFAULT;
			break;
		}
		memmove(vm->hc_out, vm->hc_out + n, vm->hc_out_len - n);
		vm->hc_out_len -= n;
		vm->hc_out_done -= n;
		io.n = n;
		io.records = vm->hc_records;
		io.errors = vm->hc_errors;
		ret = copy_to_user(uarg, &io, sizeof(io)) ? -EFAULT : 0;
		break;
	}
	case HVX_HC_PUT: {
		struct hvx_hc_io io;

		if (!vm->n_guest_pages || !vm->hc_on) { ret = -EINVAL; break; }
		if (copy_from_user(&io, uarg, sizeof(io))) { ret = -EFAULT; break; }
		if (vm->hc_in_off) {		/* drop what the guest has read */
			memmove(vm->hc_in, vm->hc_in + vm->hc_in_off, vm->hc_in_len - vm->hc_in_off);
			vm->hc_in_len -= vm->hc_in_off;
			vm->hc_in_off = 0;
		}
		if (io.len > HVX_HC_SZ - vm->hc_in_len) { ret = -ENOSPC; break; }
		if (io.len && copy_from_user(vm->hc_in + vm->hc_in_len,
					     (void __user *)(unsigned long)io.addr, io.len)) {
			ret = -EFAULT;
			break;
		}
		vm->hc_in_len += io.len;
		ret = 0;
		break;
	}
	case HVX_HC_GET_STATE: {
		struct hvx_hc_state st;
		u32 in = vm->hc_in_len - vm->hc_in_off;
		u8 __user *a;

		if (!vm->n_guest_pages || !vm->hc_on) { ret = -EINVAL; break; }
		if (copy_from_user(&st, uarg, sizeof(st))) { ret = -EFAULT; break; }
		if ((u64)st.cap < (u64)vm->hc_out_len + in) { ret = -ENOSPC; break; }
		a = (u8 __user *)(unsigned long)st.addr;
		if ((vm->hc_out_len && copy_to_user(a, vm->hc_out, vm->hc_out_len)) ||
		    (in && copy_to_user(a + vm->hc_out_len, vm->hc_in + vm->hc_in_off, in))) {
			ret = -EFAULT;
			break;
		}
		st.out_len = vm->hc_out_len;
		st.out_done = vm->hc_out_done;
		st.in_len = in;
		st.flags = vm->hc_ovf ? HVX_HC_STATE_OVF : 0;
		ret = copy_to_user(uarg, &st, sizeof(st)) ? -EFAULT : 0;
		break;
	}
	case HVX_HC_SET_STATE: {
		struct hvx_hc_state st;
		const u8 __user *a;

		if (!vm->n_guest_pages || !vm->hc_on) { ret = -EINVAL; break; }
		if (copy_from_user(&st, uarg, sizeof(st))) { ret = -EFAULT; break; }
		if (st.out_len > HVX_HC_SZ || st.out_done > st.out_len || st.in_len > HVX_HC_SZ) {
			ret = -EINVAL;
			break;
		}
		a = (const u8 __user *)(unsigned long)st.addr;
		vm->hc_out_len = vm->hc_out_done = vm->hc_in_len = vm->hc_in_off = 0;
		vm->hc_ovf = vm->hc_exit = false;
		if ((st.out_len && copy_from_user(vm->hc_out, a, st.out_len)) ||
		    (st.in_len && copy_from_user(vm->hc_in, a + st.out_len, st.in_len))) {
			ret = -EFAULT;
			break;
		}
		vm->hc_out_len = st.out_len;
		vm->hc_out_done = st.out_done;
		vm->hc_in_len = st.in_len;
		vm->hc_ovf = !!(st.flags & HVX_HC_STATE_OVF);
		ret = 0;
		break;
	}
	default:
		ret = -ENOTTY;
	}

	mutex_unlock(&vm->lock);
	return ret;
}

static const struct file_operations hvx_fops = {
	.owner          = THIS_MODULE,
	.open           = hvx_open,
	.release        = hvx_release,
	.unlocked_ioctl = hvx_ioctl,
	.compat_ioctl   = hvx_ioctl,
	.mmap           = hvx_mmap,
};

static struct miscdevice hvx_miscdev = {
	.minor = MISC_DYNAMIC_MINOR,
	.name  = HVX_DEVNAME,
	.fops  = &hvx_fops,
	.mode  = 0600,
};

/* ---- module init/exit: verify VMX+EPT, register the device ---- */

static int __init hvx_init(void)
{
	u64 fc, basic, ept_cap;
	int err;

	pr_info("hvx: loading (hydo VT-x milestone 27: FLAGSHIP on UNMODIFIED Redis over in-kernel loopback -- Redis's documented AOF durability window (appendfsync=always survives, everysec loses the last acked writes) reproduced bit-for-bit under a faithful volatile-cache power loss; builds on the M26 UNMODIFIED-SQLite flagship + the flush-aware DROP model; inert when unused, so every prior fingerprint is preserved)\n");

	if (!boot_cpu_has(X86_FEATURE_VMX)) {
		pr_err("hvx: CPU does not report VMX\n");
		return -ENODEV;
	}

	fc = rd(MSR_FEAT_CTL);
	if (!(fc & FEAT_CTL_LOCK)) {
		wrmsrl(MSR_FEAT_CTL, fc | FEAT_CTL_LOCK | FEAT_CTL_VMXON_OUT_SMX);
		fc = rd(MSR_FEAT_CTL);
		pr_info("hvx: IA32_FEATURE_CONTROL was unlocked; set lock+vmxon (now 0x%llx)\n", fc);
	} else if (!(fc & FEAT_CTL_VMXON_OUT_SMX)) {
		pr_err("hvx: VMX disabled by firmware (FEATURE_CONTROL=0x%llx)\n", fc);
		return -ENODEV;
	}

	{
		unsigned int k;

		for (k = 0; k < HVX_VMXCAP_N; k++)
			if (rdmsrl_safe(HVX_VMXCAP_LO + k, &hvx_vmxcap[k]))
				hvx_vmxcap[k] = 0;	/* absent (e.g. no secondary controls) */
		hvx_vmxcap_ok = true;
	}
	basic = rd(MSR_VMX_BASIC);
	hvx_rev = (u32)basic & 0x7fffffff;
	ept_cap = rd(MSR_VMX_EPT_VPID_CAP);
	pr_info("hvx: IA32_VMX_BASIC=0x%llx revision_id=%u ept_vpid_cap=0x%llx\n",
		basic, hvx_rev, ept_cap);

	if (!(ept_cap & EPT_CAP_WALK4) || !(ept_cap & EPT_CAP_WB)) {
		pr_err("hvx: EPT lacks 4-level walk or WB memtype; aborting\n");
		return -ENODEV;
	}
	/* M138, HVX_VM_DIRTY_LOG: EPT accessed/dirty flags, and INVEPT, which every run's entry
	 * does, so a dirty flag cleared between runs is set again by the guest's next write */
	hvx_ept_ad = (ept_cap & (1ULL << 21)) && (ept_cap & EPT_CAP_INVEPT);

	/* PMU branch clock capability: an architectural PMU v2+ with a GP counter, the
	 * VMCS load-IA32_PERF_GLOBAL_CTRL controls on both entry and exit, and a way to
	 * reach the host LVTPC. Absent under nested virtualization on a hybrid host. */
	{
		u32 a, b, c, d;
		bool use_true = !!(basic & (1ULL << 55));
		u64 exit_cap = rd(use_true ? 0x48f : 0x483), entry_cap = rd(use_true ? 0x490 : 0x484);
		u64 apic_base = rd(0x1b);

		cpuid(0x0a, &a, &b, &c, &d);
		hvx_host_x2apic = !!(apic_base & (1ULL << 10));
		if (!hvx_host_x2apic)
			hvx_lapic = ioremap(apic_base & ~0xfffULL, 0x1000);
		hvx_pmu_ok = (a & 0xff) >= 2 && ((a >> 8) & 0xff) >= 1 &&
			     ((exit_cap >> 32) & EXIT_LOAD_PERF_GLOBAL) &&
			     ((entry_cap >> 32) & ENTRY_LOAD_PERF_GLOBAL) &&
			     (hvx_host_x2apic || hvx_lapic) &&
			     !boot_cpu_has(X86_FEATURE_HYBRID_CPU);
		{
			u32 c1a, c1b, c1c, c1d;
			u64 perf_cap = 0;

			cpuid(0x01, &c1a, &c1b, &c1c, &c1d);
			if ((c1c & (1U << 15)) && rdmsrl_safe(0x345, &perf_cap))	/* PDCM */
				perf_cap = 0;
			hvx_smm_freeze = hvx_pmu_ok && (perf_cap & (1ULL << 12)) &&
					 ((entry_cap >> 32) & ENTRY_LOAD_DEBUG_CTLS);
		}
		pr_info("hvx: PMU branch clock %s (CPUID.0AH eax=0x%x, host %s, freeze in SMM %s)\n",
			hvx_pmu_ok ? "available" : "NOT available", a,
			hvx_host_x2apic ? "x2APIC" : "xAPIC", hvx_smm_freeze ? "yes" : "no");

		/* Intel PT guest tracing: PT itself, usable in VMX operation (VMX_MISC
		 * bit 14), ToPA output with multiple entries, 2 IP filter ranges, and the
		 * three "conceal VMX from PT" controls. */
		{
			u32 pa, pb, pc, pd, ra, rb, rc, rd2;
			u64 p2cap = rd(MSR_VMX_PROCBASED2);

			cpuid_count(0x07, 0, &pa, &pb, &pc, &pd);
			hvx_pt_ok = !!(pb & (1U << 25));
			if (hvx_pt_ok) {
				cpuid_count(0x14, 0, &pa, &pb, &pc, &pd);
				cpuid_count(0x14, 1, &ra, &rb, &rc, &rd2);
				hvx_pt_ok = (pb & (1U << 2)) && (pc & 3U) == 3U && (ra & 7U) >= 2 &&
					    (rd(0x485) & (1ULL << 14))	/* IA32_VMX_MISC: PT in VMX */ &&
					    ((p2cap >> 32) & SEC_CONCEAL_VMX_PT) &&
					    ((exit_cap >> 32) & EXIT_CONCEAL_VMX_PT) &&
					    ((entry_cap >> 32) & ENTRY_CONCEAL_VMX_PT);
			}
			pr_info("hvx: Intel PT guest tracing %s\n", hvx_pt_ok ? "available" : "NOT available");
		}
	}

	err = register_nmi_handler(NMI_LOCAL, hvx_chaos_nmi_handler, 0, HVX_DEVNAME);	/* chaos_nmi */
	if (err) {
		if (hvx_lapic)
			iounmap(hvx_lapic);
		return err;
	}
	err = misc_register(&hvx_miscdev);
	if (err) {
		pr_err("hvx: misc_register failed: %d\n", err);
		unregister_nmi_handler(NMI_LOCAL, HVX_DEVNAME);
		if (hvx_lapic)
			iounmap(hvx_lapic);
		return err;
	}
	pr_info("hvx: /dev/%s ready (minor=%d)\n", HVX_DEVNAME, hvx_miscdev.minor);
	return 0;
}

static void __exit hvx_exit(void)
{
	hvx_pt_free_chunks(hvx_pt_last_chunk, hvx_pt_last_nchunk);
	if (hvx_lapic)
		iounmap(hvx_lapic);
	misc_deregister(&hvx_miscdev);
	unregister_nmi_handler(NMI_LOCAL, HVX_DEVNAME);
	pr_info("hvx: unloaded\n");
}

module_init(hvx_init);
module_exit(hvx_exit);
