/* SPDX-License-Identifier: GPL-2.0 */
/*
 * hvx_abi.h - shared ABI between the hvx kernel module and userspace (hydo).
 *
 * This is the "mini-KVM" contract for milestone 3: userspace opens /dev/hvx
 * (one VM per open fd), sizes guest RAM, mmap()s that RAM to load guest code,
 * sets guest registers, and runs the guest until a VM-exit.
 *
 *   fd = open("/dev/hvx")
 *   ioctl(fd, HVX_CREATE_VM, &(struct hvx_config){ .mem_size = N })
 *   mem = mmap(NULL, N, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0)  // guest RAM
 *   // write guest code into mem at >= HVX_GUEST_CODE_BASE
 *   ioctl(fd, HVX_SET_REGS, &regs)   // regs.rip = HVX_GUEST_CODE_BASE, ...
 *   ioctl(fd, HVX_RUN, &exit)        // runs until one VM-exit
 *
 * Guest-physical layout the module owns (do not clobber pages 0..2 from
 * userspace): GPA 0x0000 PML4, 0x1000 PDPT, 0x2000 PD (one 2 MiB identity
 * page). Guest runs 64-bit long mode with CR3 = 0 and guest-virtual ==
 * guest-physical, so userspace addresses in mmap() equal guest addresses.
 */
#ifndef _HVX_ABI_H
#define _HVX_ABI_H

#include <linux/types.h>
#include <linux/ioctl.h>

struct hvx_config {
	__u64 mem_size;		/* guest RAM in bytes; rounded up to pages,
				 * 0 => default (256 KiB), max 3 GiB (milestone 28:
				 * raised from 256 MiB via EPT PD fan-out; the ceiling
				 * stays BELOW the 4th-GiB MMIO hole at 0xfe000000, see
				 * MAX_GPAGES in hvx.c) */
	__u32 n_vcpus;		/* number of vCPUs (milestone 19); 0 or 1 => 1.
				 * All vCPUs SHARE guest RAM/EPT, the console, the
				 * serial input ring, and the IOAPIC/PIC/16550 device
				 * model; each vCPU has its OWN register file, sregs,
				 * virtual TSC, instruction clock, Local APIC, and
				 * pending injection. The vCPUs are SERIALIZED (only
				 * one is ever in VMX-root at a time); userspace
				 * round-robins them by a fixed instruction quantum
				 * via HVX_RUNC, so the interleaving is a
				 * deterministic function of the per-vCPU instruction
				 * counts. Select the target of the per-vCPU ioctls
				 * with HVX_SET_VCPU (default 0). */
	__u32 flags;		/* HVX_VM_* creation flags (was _pad; 0 => none, the
				 * default, so every pre-M27 caller is byte-identical). */
};
/*
 * HVX_VM_IDLE_HEARTBEAT (milestone 27): opt in to the deterministic idle heartbeat -- when
 * the guest HALTs fully tickless (no LAPIC timer armed, no pending device completion) the
 * run-loop advances the virtual clock by a fixed step and injects the guest's LAPIC-timer
 * vector: a synthetic periodic tick that runs Linux's timer ISR + softirqs + the scheduler.
 * A co-located loopback service (a redis client<->server) needs it because it blocks in
 * epoll_wait for loopback data whose delivery needs a softirq/tick, which a deterministic
 * tickless idle never provides. OFF by default so it perturbs NO prior fingerprint (the
 * plain no-disk boot idles unarmed too, so it MUST stay off there).
 */
#define HVX_VM_IDLE_HEARTBEAT	(1u << 0)

/*
 * HVX_VM_X86_64_V2 (milestone 31): opt in to advertising the x86-64-v2 CPU feature level in
 * CPUID (leaf 1 ECX SSE3/SSSE3/CX16/SSE4.1/SSE4.2/POPCNT + EDX MMX, leaf 0x80000001 ECX
 * LAHF/SAHF). Real glibc-2.38 software (a distro OpenJDK JRE) aborts at load with "CPU ISA
 * level is lower than required" because the M12 synthetic CPUID omits MMX (an x86-64-baseline
 * requirement) and the whole nixpkgs userland is built for >= x86-64-v2. These are all
 * deterministic integer/SSE features (NO AVX/XSAVE/OSXSAVE -> the guest still never runs
 * XSETBV), so advertising them is determinism-safe; the L1 host CPU supports them natively so
 * any v2 instruction the guest runs executes correctly. OFF by default so it perturbs NO
 * prior fingerprint -- the kernel records CPUID feature flags in RAM, so an ungated bump would
 * move every boot fingerprint; gating keeps M3-M30 byte-identical.
 */
#define HVX_VM_X86_64_V2	(1u << 1)
/*
 * HVX_VM_PMU_CLOCK: opt in to the PMU branch clock. A guest-only hardware counter
 * (retired conditional branches) drives a deterministic tick: every `period` guest
 * branches the virtual TSC advances by `tick_tsc` and the instruction clock by 2048, at
 * an EXACT instruction (counter overflow slightly early, then Monitor-Trap-Flag single
 * steps to the target). Without it virtual time only moves on VM exits, so a guest that
 * computes without exiting freezes its own clock. Needs a bare-metal Intel PMU
 * (HVX_CREATE_VM fails with ENODEV otherwise). Default 0 = byte-identical to before.
 */
#define HVX_VM_PMU_CLOCK	(1u << 2)
/*
 * HVX_VM_PAUSE_EXIT: make every guest PAUSE instruction a VM exit that counts on the
 * instruction clock. Spin-wait loops (kernel cpu_relax, JVM SpinPause, InnoDB ut_delay)
 * are built from PAUSE; without this a thread spinning while it waits for a disk
 * completion or for another thread retires no exit, so the clock that would deliver the
 * completion or the preempting timer tick does not move. Changes the exit stream of any
 * guest that spins, so it is opt-in. Default 0 = byte-identical to before.
 */
#define HVX_VM_PAUSE_EXIT	(1u << 3)
/*
 * HVX_VM_PT_TRACE: allow Intel Processor Trace of this guest (HVX_SET_PT configures it,
 * HVX_GET_PT / HVX_READ_PT read the trace). Tracing is switched on only while the guest
 * runs and filtered so no host code is recorded; it does not change guest execution and
 * is not part of the snapshot. Needs PT usable in VMX operation (HVX_SET_PT fails with
 * ENODEV otherwise). Default 0 = byte-identical to before.
 */
#define HVX_VM_PT_TRACE		(1u << 4)

/*
 * Guest register file. The first 15 fields (rax..r15) are the general-purpose
 * registers loaded/saved around VMLAUNCH by the module's trampoline; their
 * order and offsets are load-bearing (the asm indexes them by byte offset).
 * rip/rsp/rflags live in the VMCS and are moved by the module.
 */
struct hvx_regs {
	__u64 rax, rbx, rcx, rdx, rsi, rdi, rbp;	/* 0x00..0x30 */
	__u64 r8, r9, r10, r11, r12, r13, r14, r15;	/* 0x38..0x70 */
	__u64 rip, rsp, rflags;				/* 0x78..0x88 */
};

struct hvx_exit {
	__u32 reason;		/* basic VM-exit reason (SDM 24.9.1), or
				 * HVX_EXIT_VMFAIL if VMLAUNCH itself failed,
				 * or HVX_EXIT_THROTTLE if the in-kernel run-loop
				 * hit its internal-exit bound (re-run to continue) */
	__u32 insn_len;		/* length of the exiting instruction */
	__u64 qualification;	/* exit qualification, or, for VMFAIL, the
				 * VM-instruction error number */
	__u64 guest_phys;	/* faulting GPA on an EPT-violation exit */
};

/*
 * Deterministic virtual TSC. RDTSC/RDTSCP are configured to VM-exit; the module
 * returns a hypervisor-controlled value (in EDX:EAX, and IA32_TSC_AUX in ECX for
 * RDTSCP) instead of the host time-stamp counter. `mode` selects the policy:
 *
 *   HVX_TSC_COUNTER  return `tsc`, then advance `tsc += step` per RDTSC exit.
 *                    Deterministic in the number of RDTSC *calls*.
 *   HVX_TSC_ICOUNT   return `tsc + icount*step`, where icount is the retired-
 *                    instruction clock (see HVX_RUNC). The TSC is then a function
 *                    of execution POSITION -- the true deterministic model --
 *                    and is meaningful only while single-stepping (MTF on), when
 *                    icount counts every instruction. Here `tsc` is the base and
 *                    `step` the ticks-per-instruction; `tsc` is not mutated.
 */
struct hvx_tsc {
	__u64 tsc;	/* COUNTER: next value / ICOUNT: base */
	__u64 step;	/* COUNTER: per-exit increment / ICOUNT: ticks per instruction */
	__u64 aux;	/* value RDTSCP loads into ECX (IA32_TSC_AUX) */
	__u64 count;	/* [GET only] RDTSC/RDTSCP exits handled since last SET */
	__u64 mode;	/* HVX_TSC_COUNTER (default) or HVX_TSC_ICOUNT */
};

#define HVX_TSC_COUNTER	0
#define HVX_TSC_ICOUNT	1

/*
 * Curated software MSR store (milestone 12). RDMSR/WRMSR are handled in-kernel;
 * a fixed set of MSRs with no host-visible effect in our guest environment is
 * backed by this per-VM array (WRMSR stores, RDMSR returns the stored value, 0
 * until first written), part of the deterministic snapshot. The slot order is
 * fixed (matches the module's SWMSR_* enum):
 *   0 STAR(0xc0000081) 1 LSTAR(0xc0000082) 2 CSTAR(0xc0000083) 3 SFMASK(0xc0000084)
 *   4 FS_BASE(0xc0000100) 5 GS_BASE(0xc0000101) 6 KERNEL_GS_BASE(0xc0000102)
 *   7 SYSENTER_CS(0x174) 8 SYSENTER_ESP(0x175) 9 SYSENTER_EIP(0x176)
 *   10 MISC_ENABLE(0x1a0)
 * IA32_EFER (0xc0000080) and IA32_TSC (0x10) are handled specially (VMCS field /
 * virtual TSC) and are NOT in this array.
 */
#define HVX_NR_SW_MSR	11

/* Number of MSI-X table entries the virtio-blk PCI function advertises (milestone
 * 21). Defined here because struct hvx_snapshot below embeds the table; the rest of
 * the MSI-X constants live with the Phase-B PCI transport section further down. */
#define HVX_MSIX_ENTRIES	4

/*
 * virtio-net core constants (milestone 23). Defined up here because struct
 * hvx_snapshot below embeds the virtio-net queue/device control state; the PCI
 * transport constants live with the Phase-B PCI section further down. The device
 * has TWO virtqueues -- receiveq (0) and transmitq (1) -- and the frame data rides
 * via HVX_PUT_RX / HVX_GET_TX (like the disk image rides via HVX_GET/SET_DISK).
 */
#define HVX_VNET_NQUEUES	2		/* receiveq (0) + transmitq (1) */
#define HVX_VNET_FRAME_MAX	1536		/* max Ethernet frame we deliver/capture */
#define HVX_VNET_HDR_LEN	12		/* virtio_net_hdr_v1 (incl. num_buffers) */

/*
 * Controlled run (HVX_RUNC): like HVX_RUN, but adds an instruction clock. With
 * HVX_RUN_SINGLE_STEP the module sets the Monitor Trap Flag so every guest
 * instruction takes a VM-exit; the module counts those (plus emulated RDTSCs)
 * into a running `icount`, and can stop after exactly `max_insns` of them. This
 * is the determinism-engine primitive "advance the guest to instruction N".
 */
struct hvx_run_ctl {
	__u64 flags;		/* in:  HVX_RUN_* bits */
	__u64 max_insns;	/* in:  single-step: stop after this many instructions
				 *      this call (0 = run to a real exit / safety cap) */
	__u64 icount;		/* out: running retired-instruction count */
	__u64 qualification;	/* out: exit qualification (see hvx_exit) */
	__u64 guest_phys;	/* out: faulting GPA on EPT-violation */
	__u32 reason;		/* out: VM-exit reason; HVX_EXIT_STEP means the
				 *      instruction budget was reached, guest alive */
	__u32 insn_len;		/* out: length of the exiting instruction */
};

#define HVX_RUN_SINGLE_STEP	(1ULL << 0)	/* enable MTF single-stepping */
#define HVX_RUN_RESET_ICOUNT	(1ULL << 1)	/* zero icount before running */

/*
 * Full privileged (system) register state: control registers, EFER, and the
 * segment/descriptor-table registers. HVX_SET_REGS only covers GPRs + rip/rsp/
 * rflags; a real guest (Linux) needs to start in, and later change, this state.
 * `ar` fields use the VMCS access-rights encoding (16-bit low word; the unusable
 * bit does not fit and is handled by the module for LDTR). As of milestone 13
 * the kernel loads the guest from this state each run and saves it back on exit,
 * so guest changes to CR3/segments/GDTR/IDTR persist across HVX_RUN boundaries;
 * HVX_GET_SREGS reads it and HVX_SET_SREGS writes it. It is also folded into the
 * snapshot below, so branch-from-snapshot carries the privileged state.
 */
struct hvx_segment {
	__u64 base;
	__u32 limit;
	__u16 selector;
	__u16 ar;		/* VMCS access-rights format (P/DPL/S/type/L/DB/G/…) */
};
struct hvx_sregs {
	__u64 cr0, cr3, cr4, efer, dr7;
	struct hvx_segment cs, ss, ds, es, fs, gs, tr, ldtr;
	__u64 gdt_base, idt_base;
	__u32 gdt_limit, idt_limit;
};

/*
 * Full deterministic VM state, minus guest RAM. Guest RAM is already visible to
 * userspace through mmap(), so a complete snapshot is:
 *   HVX_GET_SNAPSHOT  +  memcpy(save_buf, mmap_ptr, mem_size)
 * and restore is:
 *   HVX_SET_SNAPSHOT  +  memcpy(mmap_ptr, save_buf, mem_size)
 * This struct is everything NOT in guest RAM that a bit-identical replay needs:
 * the register file, the virtual-TSC policy/state, and the instruction clock.
 * Host-transient state (VMCS, EPT, VMXON pages) is intentionally absent -- it is
 * rebuilt from this state on the next run.
 */
struct hvx_snapshot {
	struct hvx_regs regs;	/* full GPR + rip/rsp/rflags */
	__u64 tsc;		/* virtual-TSC base / next value */
	__u64 tsc_step;
	__u64 tsc_aux;
	__u64 tsc_mode;		/* HVX_TSC_COUNTER | HVX_TSC_ICOUNT */
	__u64 icount;		/* instruction clock */
	__u64 rdtsc_count;	/* RDTSC/RDTSCP exits handled */
	/* pending instruction-timed interrupt (see struct hvx_inject); part of
	 * the deterministic state -- a snapshot taken before delivery must carry
	 * the schedule so a restored branch injects at the same instruction. */
	__u64 inject_at_icount;
	__u32 inject_vector;
	__u32 inject_valid;
	__u64 n_guest_pages;	/* [GET] guest RAM size in pages; ignored on SET */
	__u64 sw_msr[HVX_NR_SW_MSR];	/* curated software MSR store (milestone 12) */
	struct hvx_sregs sregs;	/* privileged register state (milestone 13) */
	/* --- milestone 15 (Phase 6): APIC / IOAPIC / 8259 device model. Appended
	 * at the struct end so older field offsets are unchanged. The x2APIC LAPIC
	 * lives entirely in MSRs, the IOAPIC is the one MMIO device, and the two
	 * PIC masks round-trip through hvx_pio; all three are part of the
	 * deterministic state a branched timer needs. */
	__u64 lapic_apic_base;		/* IA32_APIC_BASE (MSR 0x1b) */
	__u64 lapic_tsc_deadline;	/* IA32_TSC_DEADLINE (MSR 0x6e0), 0 = disarmed */
	__u32 lapic_reg[0x40];		/* x2APIC MSRs 0x800..0x83f (read-as-written) */
	__u32 ioapic_ioregsel;		/* IOAPIC index register */
	__u32 ioapic_id;		/* IOAPIC ID register (0x00) */
	__u32 ioapic_redir[48];		/* 24 redirection entries x 2 dwords */
	__u32 pic_mask[2];		/* 8259 master/slave IMR (ports 0x21 / 0xa1) */
	__u32 lapic_in_service;		/* injected vector awaiting EOI (0 = none) */
	/* --- milestone 16 (Phase 7): the 16550 UART register latch state. The M15
	 * device model added struct hvx_uart to the VM but NOT to the snapshot; it
	 * persists across HVX_RUN boundaries and is part of the deterministic device
	 * state, so a branch-from-snapshot taken while the serial driver has (e.g.)
	 * DLAB set, or THRI enabled, must carry it or the restored branch decodes the
	 * next UART port access differently and diverges. Appended at the struct end
	 * so older field offsets are unchanged. */
	__u8 uart_ier, uart_lcr, uart_mcr, uart_fcr, uart_scr;	/* 16550 registers */
	__u8 uart_dll, uart_dlm;				/* divisor latch (DLAB) */
	/* --- milestone 20: deterministic virtio-PCI block device (shared). The
	 * transport-agnostic virtio-blk CONTROL state (one split virtqueue + the
	 * device/PCI registers + the pending deterministic completion). The backing
	 * DISK IMAGE is NOT here -- it rides via HVX_GET/SET_DISK exactly like guest
	 * RAM rides via mmap -- so a branch rewinds the disk by save/restore around
	 * the snapshot. Appended at the struct end; all zero when no device (a VM
	 * that never attaches a disk is byte-identical, so the M16 boot fingerprint
	 * is preserved). */
	__u64 vblk_desc, vblk_avail, vblk_used;	/* split-virtqueue GPAs (queue 0) */
	__u32 vblk_qsize, vblk_qready;
	__u32 vblk_last_avail;			/* device cursor into the avail ring */
	__u32 vblk_isr;				/* virtio ISR status (bit0 = queue) */
	__u32 vblk_irq_vec;			/* Phase-A direct completion vector */
	__u32 vblk_irq_mode;			/* 0 = direct inject / 1 = IOAPIC INTx */
	__u32 vblk_irq_pending;			/* completion asserted, awaiting ack */
	__u32 vblk_completion_armed;		/* scheduled; latency not yet elapsed */
	__u64 vblk_completion_due;		/* icount at which completion becomes due */
	__u32 vblk_status;			/* virtio device_status */
	__u32 vblk_qsel;			/* selected queue (modern common cfg) */
	__u32 vblk_dfeat_sel, vblk_gfeat_sel;	/* device/driver feature-select */
	__u64 vblk_gfeatures;			/* driver-accepted feature bits */
	__u32 vblk_pci_command;			/* PCI command register */
	__u32 vblk_pci_bar0;			/* BAR0 (MMIO base) */
	__u32 vblk_pci_cfg_addr;		/* latched 0xcf8 config address */
	__u32 vblk_pci_intline;			/* PCI interrupt line (GSI) */
	/* --- milestone 21: MSI-X state for the virtio-blk PCI function. The device
	 * delivers a completion as a direct LAPIC injection of the queue's MSI-X
	 * message-data vector, so a branch-from-snapshot taken after MSI-X was set up
	 * must carry the message control (enable/mask), the config/queue vector
	 * assignments, and the whole MSI-X table. Appended at the struct end (older
	 * offsets unchanged; the no-disk boot fingerprint hashes only through
	 * uart_dlm, so it is preserved). The PBA is computed on the fly, not stored. */
	__u32 vblk_msix_control;		/* MSI-X message control (enable bit15 / func-mask bit14) */
	__u32 vblk_config_msix;			/* common-cfg msix_config vector (config-change) */
	__u32 vblk_queue_msix;			/* common-cfg queue_msix_vector (queue 0) */
	__u32 vblk_msix_tbl[HVX_MSIX_ENTRIES * 4];	/* {addr_lo, addr_hi, data, ctrl} per entry */
	/* --- milestone 22: deterministic disk fault-injection policy + PRNG state.
	 * Appended at the struct end (older field offsets unchanged; the no-disk and
	 * with-disk boot fingerprints and the virtio iso/pci fingerprints hash only
	 * fields defined up to vblk_msix_tbl, so they are all preserved). The ENTIRE
	 * fault schedule is a pure function of (fault_seed, fault_req_seq) for the
	 * per-request decision and (fault_seed, fault_kick_seq) for the completion
	 * reorder permutation, so carrying these across a snapshot reproduces the
	 * identical fault schedule on a branch/replay. All zero => faults disabled =>
	 * the device is byte-identical to M21. */
	__u64 fault_seed;
	__u32 fault_classes;			/* enabled HVX_FAULT_* bits (0 = inert) */
	__u32 fault_probability;		/* 0..100 : percent of requests faulted */
	__u32 fault_max_delay;			/* DELAY: max extra completion latency (instructions) */
	__u32 fault_kick_seq;			/* per-kick counter (reorder permutation key) */
	__u64 fault_req_seq;			/* per-request counter (fault-roll key) */
	__u64 fault_count;			/* faults injected so far (diagnostic; part of state) */
	/* --- milestone 23: deterministic virtio-net PCI device (shared). Appended at
	 * the struct end (older field offsets unchanged; all prior fingerprints hash
	 * only fields up to fault_count, so they are preserved). All zero when no net
	 * device is attached, so a VM that never attaches one is byte-identical and
	 * every prior boot / iso / pci / fault fingerprint holds. Two virtqueues
	 * (receiveq 0 / transmitq 1). The pending inbound-frame FIFO cursors and the
	 * captured-TX cursor ride here; the frame BYTES ride via HVX_PUT_RX /
	 * HVX_GET_TX (like the disk image rides via HVX_GET/SET_DISK), so a branch-
	 * from-snapshot taken after RX delivery (frame already in guest RAM) or after
	 * a TX capture reproduces the network activity bit-for-bit. */
	__u32 vnet_present;
	__u8  vnet_mac[6];
	__u16 vnet_pad;
	__u32 vnet_dev_status;			/* virtio device_status handshake */
	__u32 vnet_qsel;			/* selected queue (modern common cfg) */
	__u32 vnet_dfeat_sel, vnet_gfeat_sel;	/* device/driver feature-select */
	__u64 vnet_gfeatures;			/* driver-accepted feature bits */
	__u32 vnet_pci_command, vnet_pci_bar0, vnet_pci_intline;
	__u32 vnet_isr;				/* virtio ISR status (bit0 = queue) */
	__u32 vnet_msix_control;		/* MSI-X message control (enable/func-mask) */
	__u32 vnet_config_msix;			/* common-cfg msix_config (config-change vector) */
	__u32 vnet_msix_tbl[HVX_MSIX_ENTRIES * 4];	/* {addr_lo, addr_hi, data, ctrl} per entry */
	__u64 vnet_desc[HVX_VNET_NQUEUES], vnet_avail[HVX_VNET_NQUEUES], vnet_used[HVX_VNET_NQUEUES];
	__u32 vnet_qsize[HVX_VNET_NQUEUES], vnet_qready[HVX_VNET_NQUEUES];
	__u32 vnet_last_avail[HVX_VNET_NQUEUES];	/* device cursor into each avail ring */
	__u32 vnet_queue_msix[HVX_VNET_NQUEUES];	/* per-queue MSI-X table index */
	__u32 vnet_completion_armed[HVX_VNET_NQUEUES];	/* scheduled; latency not yet elapsed */
	__u64 vnet_completion_due[HVX_VNET_NQUEUES];	/* icount at which completion becomes due */
	__u32 vnet_irq_pending[HVX_VNET_NQUEUES];	/* completion asserted (MSI edge), awaiting delivery */
	__u32 vnet_rx_head, vnet_rx_tail;	/* pending inbound-frame FIFO cursors (count = tail - head) */
	__u32 vnet_tx_len, vnet_tx_count;	/* captured outbound bytes / frames */
	__u32 vnet_rx_paced;			/* paced RX delivery enabled (real-OS stretch) */
	__u32 vnet_pad2;
	__u64 vnet_rx_next_deliver;		/* icount at which the next paced frame may deliver */
	/* --- milestone 24: deterministic virtio-net fault-injection policy + PRNG state.
	 * Appended at the struct end (older field offsets unchanged; all prior
	 * fingerprints hash only fields defined up to vnet_rx_next_deliver, so they are
	 * preserved). The ENTIRE net-fault schedule is a pure function of (nfault_seed,
	 * direction, packet index) for the per-packet decision and (nfault_seed,
	 * direction, kick index) for the reorder permutation, so carrying these across a
	 * snapshot reproduces the identical fault schedule on a branch/replay. All zero =>
	 * net faults disabled => the device is byte-identical to M23. */
	__u64 nfault_seed;
	__u32 nfault_classes;			/* enabled HVX_NFAULT_* bits (0 = inert) */
	__u32 nfault_probability;		/* 0..100 : percent of packets faulted */
	__u32 nfault_max_delay;			/* DELAY: max extra RX completion latency (instructions) */
	__u32 nfault_rx_kick_seq;		/* per-RX-kick reorder permutation key */
	__u32 nfault_tx_kick_seq;		/* per-TX-kick reorder permutation key */
	__u32 nfault_pad;
	__u64 nfault_rx_seq;			/* per-RX-packet fault-roll key */
	__u64 nfault_tx_seq;			/* per-TX-packet fault-roll key */
	__u64 nfault_count;			/* faults injected so far (diagnostic; part of state) */
	/* --- guest x87/SSE registers: the 512-byte FXSAVE image of the current vCPU.
	 * Appended at the struct end (older offsets unchanged). The guest's FPU state
	 * now persists across HVX_RUN, so a snapshot must carry it. */
	__u8 fx[512];
	/* --- PMU branch clock (HVX_VM_PMU_CLOCK): guest conditional branches retired so far
	 * and the next tick target. Zero / unused when the clock is off. */
	__u64 pmu_total;
	__u64 pmu_next;
	/* --- pending one-shot interrupt vectors beyond inject_vector (bit n = vector n). */
	__u64 irr[4];
};

/* PMU branch clock policy + statistics (HVX_SET_PMU / HVX_GET_PMU). SET takes `period`
 * and `tick_tsc` (0 = keep the default); GET returns everything. `land_hash` is an FNV
 * hash over the guest (rip, rcx, rflags) at every tick: two runs of the same guest must
 * report the same value, which is how the tick's instruction-exactness is tested. */
struct hvx_pmu {
	__u64 period;		/* guest conditional branches per tick */
	__u64 tick_tsc;		/* virtual-TSC advance per tick */
	__u64 total;		/* [GET] guest conditional branches retired */
	__u64 next;		/* [GET] next tick target */
	__u64 ticks;		/* [GET] ticks delivered */
	__u64 overshoots;	/* [GET] ticks that could NOT be landed exactly (must be 0) */
	__u64 steps;		/* [GET] single-steps spent landing ticks */
	__u64 land_hash;	/* [GET] hash of the tick landing points */
	__u64 hops;		/* [GET] branch-free stretches (REP string ops) skipped while landing */
};
#define HVX_SET_PMU _IOW(HVX_IOC_MAGIC, 40, struct hvx_pmu)
#define HVX_GET_PMU _IOR(HVX_IOC_MAGIC, 41, struct hvx_pmu)

/*
 * Schedule an interrupt to be delivered at an exact instruction count. When the
 * instruction clock reaches `at_icount`, the module delivers vector `vector`
 * through the guest IDT on the next VM-entry (VM_ENTRY_INTR_INFO). One-shot:
 * cleared once delivered. This is how interrupt *timing* becomes deterministic.
 *
 * Type 0 (external interrupt) requires the guest to be interruptible at the
 * delivery point: guest RFLAGS.IF must be 1 (VM-entry enforces this, else the
 * entry fails with invalid-guest-state). Set IF in the register file before the
 * scheduled instruction, or delivery will fault.
 */
struct hvx_inject {
	__u32 vector;		/* interrupt vector (0..255) */
	__u32 type;		/* 0 = external interrupt (only type supported) */
	__u64 at_icount;	/* deliver on the entry when icount >= this */
};

#define HVX_IOC_MAGIC	0x48	/* 'H' */
#define HVX_CREATE_VM	_IOW(HVX_IOC_MAGIC, 1, struct hvx_config)
#define HVX_SET_REGS	_IOW(HVX_IOC_MAGIC, 2, struct hvx_regs)
#define HVX_GET_REGS	_IOR(HVX_IOC_MAGIC, 3, struct hvx_regs)
#define HVX_RUN		_IOR(HVX_IOC_MAGIC, 4, struct hvx_exit)
#define HVX_SET_TSC	_IOW(HVX_IOC_MAGIC, 5, struct hvx_tsc)
#define HVX_GET_TSC	_IOR(HVX_IOC_MAGIC, 6, struct hvx_tsc)
#define HVX_RUNC	_IOWR(HVX_IOC_MAGIC, 7, struct hvx_run_ctl)
/*
 * Guest serial console. Guest OUT to the 16550 UART's transmit register (port
 * 0x3f8) is trapped and the bytes accumulated here (capped at HVX_CONSOLE_SZ);
 * IN from the line-status register returns "transmit ready". This is what lets
 * a real payload -- eventually Linux earlyprintk -- print without its I/O
 * escaping to the host's physical ports.
 */
#define HVX_CONSOLE_SZ	65536		/* a full Linux boot log exceeds 4 KiB (M15) */
struct hvx_console {
	__u32 len;			/* bytes accumulated (may exceed SZ; buf capped) */
	__u8  buf[HVX_CONSOLE_SZ];
};
/*
 * NOTE: struct hvx_console is 64 KiB+, far larger than the 14-bit size the _IOC
 * number can encode, so HVX_GET_CONSOLE below is defined WITHOUT the struct type
 * (embedding it would make the ioctl number "not an integer constant"). The
 * handler transfers the whole struct explicitly with copy_to_user(sizeof).
 */

/*
 * Serial input (milestone 18). Userspace pushes up to HVX_INPUT_MAX bytes per
 * call into the guest's UART receive queue; a guest IN from the 16550 RBR (port
 * 0x3f8, DLAB=0) pops them in order, LSR bit0 (Data Ready) reflects the queue,
 * and -- once the guest has enabled ERBFI (IER bit0) and unmasked IOAPIC
 * redirection entry 4 (ISA IRQ4 -> COM1, per the MP table) -- a queued byte
 * raises COM1's IRQ through the deterministic instruction-timed injection path,
 * so a real Linux serial console becomes interactive (typing echoes, commands
 * run). The queue is TRANSIENT host input, NOT part of the deterministic
 * snapshot/fingerprint; with an empty queue the whole RX path is inert, so a
 * boot with no input fed stays bit-identical (M16 fingerprint preserved).
 */
#define HVX_INPUT_MAX 256
struct hvx_input {
	__u32 len;			/* bytes in buf to enqueue (clamped to HVX_INPUT_MAX) */
	__u8  buf[HVX_INPUT_MAX];
};

#define HVX_GET_SNAPSHOT _IOR(HVX_IOC_MAGIC, 8, struct hvx_snapshot)
#define HVX_SET_SNAPSHOT _IOW(HVX_IOC_MAGIC, 9, struct hvx_snapshot)
#define HVX_INJECT	 _IOW(HVX_IOC_MAGIC, 10, struct hvx_inject)
#define HVX_GET_CONSOLE	 _IOR(HVX_IOC_MAGIC, 11, __u32)	/* see struct hvx_console note */
#define HVX_GET_SREGS	 _IOR(HVX_IOC_MAGIC, 12, struct hvx_sregs)
#define HVX_SET_SREGS	 _IOW(HVX_IOC_MAGIC, 13, struct hvx_sregs)
#define HVX_PUT_INPUT	 _IOW(HVX_IOC_MAGIC, 14, struct hvx_input)
/*
 * Select the current vCPU (milestone 19). A VM created with n_vcpus > 1 holds an
 * array of vCPUs; every PER-VCPU ioctl -- HVX_SET/GET_REGS, HVX_SET/GET_TSC,
 * HVX_SET/GET_SREGS, HVX_GET/SET_SNAPSHOT, HVX_INJECT, HVX_RUN and HVX_RUNC --
 * targets the vCPU selected here (default 0). This is the single-fd analogue of
 * KVM's per-vCPU fd. arg is a __u32 index in [0, n_vcpus). Shared ioctls
 * (HVX_GET_CONSOLE, HVX_PUT_INPUT) ignore the selection. A single-vCPU VM never
 * needs this call, so all pre-M19 clients are unaffected.
 */
#define HVX_SET_VCPU	 _IOW(HVX_IOC_MAGIC, 15, __u32)

/*
 * Backing disk image for the virtio-blk device (milestone 20). Attaching a disk
 * ENABLES the device (a VM with no disk attached is byte-identical to pre-M20, so
 * the boot fingerprint is preserved). SET (re)allocates the image to `len` bytes
 * and copies them from `data`; GET copies min(len, image size) back to `data` and
 * returns the image size in `len`. The image is snapshot-able alongside guest RAM:
 * a full snapshot is HVX_GET_SNAPSHOT + memcpy(RAM) + HVX_GET_DISK, and restore is
 * the inverse -- so branch/replay rewinds the disk bit-for-bit.
 */
/*
 * Backing-image cap. Bumped 1 MiB -> 64 MiB (M26, SQLite-on-a-real-fs) -> 4 GiB
 * (milestone 28, Rung 3) so a multi-GB data/root disk fits. The bump is
 * FINGERPRINT-NEUTRAL: HVX_SET_DISK allocates exactly `dk.len` (rounded to pages,
 * never the cap) and the fingerprint hashes `disk_size` bytes, so every VM that
 * keeps a small disk (all pre-M28 tests + boots) is byte-identical -- the constant
 * is only an upper bound in the EINVAL check. As of M28 the backing image is a page
 * array (unsigned long disk_pages[], mirroring guest RAM -- see hvx.c M11) rather
 * than one contiguous kvmalloc, so a GB-scale disk is a robust per-page allocation
 * and can be loaded / fingerprinted zero-copy through the disk mmap window below.
 */
#define HVX_DISK_MAX (4UL << 30)	/* 4 GiB backing-image cap (M28: multi-GB disk) */

/*
 * Disk mmap window (milestone 28). One fd exposes TWO mmap regions: guest RAM at
 * mmap offset 0 (pgoff [0, n_guest_pages)) and the virtio-blk backing disk image at
 * mmap offset HVX_DISK_MMAP_PGOFF*PAGE_SIZE (pgoff [HVX_DISK_MMAP_PGOFF,
 * +n_disk_pages)). The sentinel sits far above any real RAM page index (3 GiB RAM =
 * 786432 pages << 2^32), so the two windows never collide. Userspace loads a GB-scale
 * rootfs into the disk and fingerprints it zero-copy over the page cache -- no
 * whole-image HVX_SET/GET_DISK copy. HVX_SET/GET_DISK remain for small disks and the
 * hand-written tests.
 *
 *   disk_mmap = mmap(NULL, disk_len, PROT_READ|PROT_WRITE, MAP_SHARED, fd,
 *                    (off_t)HVX_DISK_MMAP_PGOFF * 4096)
 */
#define HVX_DISK_MMAP_PGOFF	(1ULL << 32)
struct hvx_disk {
	__u64 len;			/* SET: image size to allocate; GET: [out] image size */
	__u64 data;			/* userspace pointer to the disk bytes */
	__u64 flags;			/* SET: HVX_DISK_* (milestone 21); 0 = default (M20) */
};
/*
 * By default HVX_SET_DISK also identity-maps the 4th GiB [3 GiB, 4 GiB) into the
 * guest's page tables so a HAND-WRITTEN guest (flat identity paging) can reach the
 * IOAPIC / virtio BAR MMIO. A real OS builds its own page tables and ioremaps MMIO
 * itself, so it must NOT have its tables poked (the module's fixed GPA_PDPT slot is
 * the OS's PML4) -- pass HVX_DISK_NO_IDMAP for a real-Linux boot (milestone 21).
 */
#define HVX_DISK_NO_IDMAP	(1ULL << 0)
#define HVX_SET_DISK	 _IOW(HVX_IOC_MAGIC, 16, struct hvx_disk)
#define HVX_GET_DISK	 _IOWR(HVX_IOC_MAGIC, 17, struct hvx_disk)

/*
 * Deterministic disk fault injection (milestone 22), as in FoundationDB's
 * simulation. A SEEDED policy perturbs virtio-blk I/O so that any bug it surfaces
 * replays bit-for-bit. EVERY fault decision is a pure function of a seeded PRNG
 * keyed by the per-device request index (and the per-kick index for reordering),
 * never of wall-clock time or host state, so: same seed => same fault schedule =>
 * same result => reproducible bug. The policy + PRNG counters ride in the snapshot
 * (see struct hvx_snapshot above), so branch/replay reproduces the exact faults.
 *
 * Fault classes (a bitmask; combine freely). For each virtio-blk request the
 * engine draws one 64-bit roll h = splitmix64(seed + (req_index+1)*GOLDEN); if
 * (h % 100) < probability a fault applies, chosen from the ENABLED classes
 * applicable to the request type, in this FIXED order (so userspace can replicate
 * the schedule): DELAY, ERROR, DROP, TORN -- selected by ((h>>8) % n_applicable):
 */
#define HVX_FAULT_DELAY		(1u << 0)	/* push the shared completion out by 1..max_delay extra instructions */
#define HVX_FAULT_ERROR		(1u << 1)	/* return VIRTIO_BLK_S_IOERR + skip the data transfer (I/O error) */
#define HVX_FAULT_DROP		(1u << 2)	/* WRITE only: ack S_OK but do NOT persist the data (silent durability bug) */
#define HVX_FAULT_REORDER	(1u << 3)	/* complete a kick's requests in a seeded-permuted used-ring order */
#define HVX_FAULT_TORN		(1u << 4)	/* WRITE only: persist only the first sector of a multi-sector write */
/*
 * FLUSH-AWARE DROP (milestone 26) -- a FAITHFUL power-loss model, distinct from the
 * harsher HVX_FAULT_DROP "lying disk" above. A WRITE rolled for this class is held in
 * a per-device VOLATILE WRITE CACHE (not yet persisted; reads are served from it so the
 * guest sees its own writes) instead of being silently discarded. A subsequent virtio-
 * blk FLUSH (VIRTIO_BLK_T_FLUSH, which the guest issues on fsync ONCE the device
 * advertises VIRTIO_BLK_F_FLUSH -- hvx advertises it ONLY when this class is enabled, so
 * all prior fingerprints are preserved) COMMITS the cache to stable media, making those
 * writes DURABLE and NO LONGER droppable. So: a write the guest fsync'd survives; only an
 * UN-flushed write is lost -- exactly "an un-synced write lost on power failure", and
 * unlike HVX_FAULT_DROP it CANNOT lose a properly-barriered write. Contrast the two on
 * the same SUT to isolate the flush barrier: HVX_FAULT_DROP loses an fsync'd committed
 * row (the disk lied about durability), HVX_FAULT_DROP_UNFLUSHED does not.
 */
#define HVX_FAULT_DROP_UNFLUSHED (1u << 5)	/* WRITE only: hold in a volatile cache; lost only if never FLUSHed */

#define GOLDEN 0x9e3779b97f4a7c15ULL		/* splitmix64 increment (documented for the test) */

struct hvx_fault {
	__u64 seed;		/* PRNG seed */
	__u32 classes;		/* enabled HVX_FAULT_* bits; 0 => faults disabled (default) */
	__u32 probability;	/* 0..100 : percent of requests that receive a fault */
	__u32 max_delay;	/* DELAY: max extra completion latency in instructions (>=1) */
	__u32 _pad;
	/* [GET only] running counters, part of the deterministic state (mirror the
	 * snapshot's fault_req_seq / fault_count); ignored on SET. */
	__u64 req_seq;		/* total requests the fault engine has decided on */
	__u64 fault_count;	/* how many of those received a (non-normal) fault */
};
#define HVX_SET_FAULT	 _IOW(HVX_IOC_MAGIC, 18, struct hvx_fault)
#define HVX_GET_FAULT	 _IOR(HVX_IOC_MAGIC, 19, struct hvx_fault)

/*
 * Deterministic virtio-net PCI device (milestone 23). A second virtio-PCI
 * function (vendor 0x1af4, modern virtio-net device 0x1041) on the same bus as
 * the virtio-blk function, with two virtqueues: receiveq (0) and transmitq (1).
 *
 * HVX_NET_ATTACH enables the device (a VM with no net device attached is byte-
 * identical to pre-M23, so every prior fingerprint is preserved). It seeds the
 * MAC the device advertises (VIRTIO_NET_F_MAC) and, like HVX_SET_DISK, maps the
 * 4th GiB into the guest's identity page tables so a HAND-WRITTEN guest can reach
 * the BAR MMIO -- pass HVX_NET_NO_IDMAP for a real-OS boot (it builds its own
 * page tables + ioremaps MMIO itself). The 4th-GiB idmap PTE pre-sets Accessed/
 * Dirty to avoid the M22-class page-table nondeterminism on the EPT-unmapped
 * MMIO window.
 *
 * HVX_PUT_RX pushes ONE scripted inbound Ethernet frame into the device's pending
 * RX FIFO. Delivery is DETERMINISTIC: at the next receiveq kick (a guest-driven,
 * icount-exact instant) the device consumes a posted receive buffer, writes the
 * 12-byte virtio_net_hdr_v1 + the frame, writes the used ring, and schedules the
 * completion MSI-X at kick_icount + HVX_VNET_LATENCY (M8 instruction-timed path,
 * M16 clock-creep discipline). The FIFO cursors ride in the snapshot; a boot with
 * NO frames pushed leaves the whole RX path inert.
 *
 * HVX_GET_TX reads back the frames the guest posted to the transmitq (serviced
 * synchronously at the transmitq kick and captured into a host-side buffer, like
 * the console). Concatenated frame bytes; `len`/`count` report totals.
 */
#define HVX_NET_NO_IDMAP	(1ULL << 0)
/* PACED RX delivery (for a real OS): a real driver kicks the receiveq once at open,
 * so trickle scripted frames one per fixed instruction interval against the already-
 * posted buffers, letting a later frame arrive after the interface is fully up.
 * OFF for the hand-written test (kick-triggered), so its fingerprint is unchanged. */
#define HVX_NET_PACED		(1ULL << 1)
struct hvx_net_config {
	__u8  mac[6];
	__u16 _pad;
	__u64 flags;			/* HVX_NET_NO_IDMAP | HVX_NET_PACED */
};
struct hvx_rx {
	__u32 len;			/* frame length (1..HVX_VNET_FRAME_MAX) */
	__u8  buf[HVX_VNET_FRAME_MAX];
};
struct hvx_tx {
	__u64 len;			/* SET: size of `data` buffer; [out] total captured bytes */
	__u64 data;			/* userspace pointer to receive captured frame bytes */
	__u64 count;			/* [out] number of frames captured */
};
#define HVX_NET_ATTACH	 _IOW(HVX_IOC_MAGIC, 20, struct hvx_net_config)
#define HVX_PUT_RX	 _IOW(HVX_IOC_MAGIC, 21, struct hvx_rx)
#define HVX_GET_TX	 _IOWR(HVX_IOC_MAGIC, 22, struct hvx_tx)

/*
 * Deterministic virtio-net fault injection (milestone 24), the network side,
 * symmetric to the M22 disk fault injection. A SEEDED policy perturbs
 * packets on the virtio-net device (both RX frames delivered to the guest and TX frames
 * captured from it) so that any bug it surfaces replays bit-for-bit. EVERY fault
 * decision is a PURE FUNCTION of a seeded PRNG keyed by DIRECTION + the per-direction
 * packet index (and the per-kick index for the reorder permutation), never of wall-clock
 * time or host state: same seed => same fault schedule => same result => reproducible.
 * The policy + PRNG counters ride in the snapshot (struct hvx_snapshot above), so
 * branch/replay reproduces the exact faults. classes == 0 (the default) disables faults
 * => the device is byte-identical to M23 (every prior net fingerprint is preserved).
 *
 * For each packet the engine draws one 64-bit roll
 *     h = splitmix64((seed ^ dir_salt) + (index+1)*GOLDEN)
 * where dir_salt = ('R'<<48) for RX and ('T'<<48) for TX, keeping the two directions'
 * schedules independent. If (h % 100) < probability a fault applies, chosen from the
 * ENABLED classes applicable to the direction, in this FIXED order (so userspace can
 * replicate the schedule): DROP, DELAY (RX only), CORRUPT, DUPLICATE -- selected by
 * ((h>>8) % n_applicable). For CORRUPT the flipped byte position is (h>>16) % frame_len
 * and the XOR value is (u8)((h>>32) | 1) (always nonzero, so the byte always changes).
 * For DELAY the extra RX-completion latency is 1 + (h>>16) % max_delay instructions.
 * REORDER is a SEPARATE per-kick class: when a kick delivers/captures MULTIPLE packets,
 * their used-ring completion order is Fisher-Yates permuted with a roll keyed by
 *     splitmix64((seed ^ dir_salt ^ 0xD15EA5E5C0FFEE11) + (kick_index+1)*GOLDEN)
 * -- exactly like the disk's HVX_FAULT_REORDER.
 */
#define HVX_NFAULT_DROP       (1u << 0)  /* packet discarded (RX: not delivered / TX: not transmitted) */
#define HVX_NFAULT_DELAY      (1u << 1)  /* RX only: deliver the batch completion 1..max_delay insns later */
#define HVX_NFAULT_CORRUPT    (1u << 2)  /* flip one PRNG-chosen byte (position + value from the roll) */
#define HVX_NFAULT_DUPLICATE  (1u << 3)  /* deliver/capture the packet twice */
#define HVX_NFAULT_REORDER    (1u << 4)  /* seeded-permuted used-ring order for a multi-packet kick */

struct hvx_net_fault {
	__u64 seed;		/* PRNG seed */
	__u32 classes;		/* enabled HVX_NFAULT_* bits; 0 => faults disabled (default) */
	__u32 probability;	/* 0..100 : percent of packets that receive a fault */
	__u32 max_delay;	/* DELAY: max extra RX-completion latency in instructions (>=1) */
	__u32 _pad;
	/* [GET only] running counters, part of the deterministic state (mirror the
	 * snapshot's nfault_rx_seq / nfault_tx_seq / nfault_count); ignored on SET. */
	__u64 rx_seq;		/* total RX packets the fault engine has decided on */
	__u64 tx_seq;		/* total TX packets the fault engine has decided on */
	__u64 fault_count;	/* how many packets received a (non-normal) fault */
};
#define HVX_SET_NET_FAULT _IOW(HVX_IOC_MAGIC, 23, struct hvx_net_fault)
#define HVX_GET_NET_FAULT _IOR(HVX_IOC_MAGIC, 24, struct hvx_net_fault)

/* ---- synchronous fuzz-input port (Phase 3 search loop) ----
 * Stage one op-program the guest pulls byte-by-byte via IN from the data port (0x510), with a
 * data-available status at the status port (0x511). SYNCHRONOUS: each guest IN is a serviced
 * VM-exit (no interrupt, no blocking read), so HVX_RUN keeps returning and the idle heartbeat can
 * stay on for the workload; the read is a pure function of this buffer -> byte-deterministic across
 * snapshot restores. Like HVX_PUT_INPUT it is TRANSIENT host input, deliberately NOT part of
 * HVX_GET/SET_SNAPSHOT or the fingerprint (with len 0 the ports are inert). SET resets the read
 * offset to 0, so the host stages the next program per restore and the guest re-reads from the start. */
#define HVX_FUZZ_MAX 8192
struct hvx_fuzz_input {
	__u32 len;			/* bytes in buf to stage (clamped to HVX_FUZZ_MAX) */
	__u8  buf[HVX_FUZZ_MAX];
};
#define HVX_SET_FUZZ_INPUT _IOW(HVX_IOC_MAGIC, 25, struct hvx_fuzz_input)

/* ---- virtio-net PCI transport (milestone 23) ---- */
#define HVX_VNET_PCI_DEV	4		/* bus 0, device 4, function 0 */
#define HVX_VNET_VENDOR		0x1af4
#define HVX_VNET_DEVICE		0x1041		/* modern virtio-net (0x1040 + type 1) */
#define HVX_VNET_BAR		0xfe020000UL	/* MMIO BAR base (EPT-unmapped -> trap; distinct from vblk 0xfe000000) */
#define HVX_VNET_BAR_SIZE	0x8000		/* 32 KiB: common/notify/isr/device + MSI-X table/PBA */
#define HVX_VNET_GSI		10		/* nominal INTx GSI (MSI-X is the real path) */
#define HVX_VNET_QSIZE		256		/* device-advertised default queue size */
#define HVX_VNET_MSIX_CAP	0x90		/* MSI-X capability offset in PCI config space */
#define HVX_VNET_LATENCY	16		/* deterministic RX/TX completion latency (instructions) */
/* virtio-net feature bits + config (virtio 1.0 §5.1) */
#define VIRTIO_NET_F_MAC	5		/* device advertises a MAC address */
#define VIRTIO_NET_F_STATUS	16		/* device advertises a config status field (link state) */
#define VIRTIO_NET_S_LINK_UP	1		/* virtio_net_config.status: link up */

/* ---- virtio-blk core (transport-agnostic; milestone 20) ---- */
#define HVX_VBLK_SECTOR		512
/* Deterministic device completion latency, measured in the instruction clock:
 * the completion interrupt fires exactly this many instructions after the kick,
 * via the M8 instruction-timed injection path -- a pure function of icount. */
#define HVX_VBLK_LATENCY	16
/* split-virtqueue descriptor flags (virtio 1.0 §2.7.5) */
#define VIRTQ_DESC_F_NEXT	1
#define VIRTQ_DESC_F_WRITE	2
/* virtio-blk request types + status (virtio 1.0 §5.2.6) */
#define VIRTIO_BLK_T_IN		0	/* read from disk into guest buffer */
#define VIRTIO_BLK_T_OUT	1	/* write guest buffer to disk */
#define VIRTIO_BLK_T_FLUSH	4	/* flush the volatile write cache to stable media */
#define VIRTIO_BLK_S_OK		0
#define VIRTIO_BLK_S_IOERR	1
/* virtio-blk feature bits (virtio 1.0 §5.2.3). F_FLUSH advertises a volatile write
 * cache + the VIRTIO_BLK_T_FLUSH command; hvx advertises it ONLY when the flush-aware
 * fault class (HVX_FAULT_DROP_UNFLUSHED) is armed, so every prior VM is byte-identical. */
#define VIRTIO_BLK_F_FLUSH	9

/*
 * Phase-A TEST TRANSPORT (isolated from PCI). A simple PIO register block drives
 * the virtio-blk core directly so the novel deterministic-completion mechanism is
 * validated ON ITS OWN before the PCI transport is layered on top. All registers
 * are 32-bit (out dx,eax / in eax,dx). Inert unless a disk is attached.
 */
#define HVX_VIRTIO_PIO_BASE	0x0500
#define HVX_VPIO_DESC_LO	0x00	/* split-queue GPAs, low/high 32 bits */
#define HVX_VPIO_DESC_HI	0x04
#define HVX_VPIO_AVAIL_LO	0x08
#define HVX_VPIO_AVAIL_HI	0x0c
#define HVX_VPIO_USED_LO	0x10
#define HVX_VPIO_USED_HI	0x14
#define HVX_VPIO_QSIZE		0x18	/* queue size (entries) */
#define HVX_VPIO_QREADY		0x1c	/* write 1 to make the queue live */
#define HVX_VPIO_IRQ_VEC	0x20	/* completion vector (direct injection) */
#define HVX_VPIO_NOTIFY		0x24	/* write -> process the avail ring (doorbell) */
#define HVX_VPIO_ISR		0x28	/* read: ISR status, read-to-clear */
#define HVX_VPIO_CAPACITY	0x2c	/* read: disk size in 512-byte sectors */

/*
 * Phase-B PCI TRANSPORT. Standard 0xCF8/0xCFC config mechanism exposes ONE PCI
 * function (virtio vendor 0x1af4, modern virtio-blk device 0x1042) whose BAR0 is
 * an MMIO window holding the modern virtio-pci structures (common/notify/isr/
 * device cfg), advertised through the virtio-PCI capability list. The device's
 * INTx line is routed through the existing M15 IOAPIC (redirection entry
 * HVX_VBLK_GSI -> vector -> the M8 injection path). Same virtio-blk core as A.
 */
#define HVX_PCI_CONFIG_ADDR	0xcf8
#define HVX_PCI_CONFIG_DATA	0xcfc
#define HVX_VBLK_PCI_DEV	3		/* bus 0, device 3, function 0 */
#define HVX_VBLK_VENDOR		0x1af4
#define HVX_VBLK_DEVICE		0x1042		/* modern virtio-blk (0x1040 + type 2) */
#define HVX_VBLK_BAR		0xfe000000UL	/* MMIO BAR base (EPT-unmapped -> trap) */
#define HVX_VBLK_BAR_SIZE	0x8000		/* 32 KiB: common/notify/isr/device + MSI-X table/PBA */
#define HVX_VBLK_GSI		11		/* INTx -> IOAPIC redirection entry 11 */
/* modern virtio-pci structure offsets within BAR0 */
#define HVX_VPCI_COMMON		0x0000
#define HVX_VPCI_NOTIFY		0x1000
#define HVX_VPCI_ISR		0x2000
#define HVX_VPCI_DEVICE		0x3000
/* Device-advertised default virtqueue size (milestone 21). The modern virtio-pci
 * driver reads queue_size FIRST to learn the device's max, so it must be a nonzero
 * power of two; the driver then writes back the ring size it actually created. */
#define HVX_VBLK_QSIZE		256

/*
 * MSI-X (milestone 21). The virtio-blk PCI function advertises an MSI-X capability
 * at config offset HVX_VBLK_MSIX_CAP, whose table + PBA live in BAR0 (BIR 0) at the
 * offsets below (beyond the common/notify/isr/device structures). Modern
 * virtio_pci in real Linux prefers MSI-X; the device delivers a completion as a
 * direct LAPIC injection of the vector programmed in the queue's MSI-X table entry
 * (msg_data) at the deterministic completion instruction -- no IOAPIC, no INTx, so
 * it sidesteps the acpi=off INTx-routing problem entirely. INTx stays available for
 * the hand-written Phase-B test; Linux chooses MSI-X.
 */
/* HVX_MSIX_ENTRIES is defined earlier (the snapshot struct embeds the table). */
#define HVX_VBLK_MSIX_CAP	0x90		/* MSI-X capability offset in PCI config space */
#define HVX_VPCI_MSIX_TABLE	0x4000		/* MSI-X table within BAR0 (16 B/entry) */
#define HVX_VPCI_MSIX_PBA	0x5000		/* MSI-X pending-bit array within BAR0 */
#define HVX_MSIX_NO_VECTOR	0xffff		/* virtio "no MSI-X vector assigned" */

/* Sentinel exit reason: VMLAUNCH/VMRESUME failed (not a real guest exit). */
#define HVX_EXIT_VMFAIL		0xffffffffU
/* Sentinel exit reason: in-kernel run-loop bound hit; re-run to continue. */
#define HVX_EXIT_THROTTLE	0xfffffffeU
/* The guest retired no VM-exit at all for several seconds of host time (a tight
 * loop with no RDTSC/IO/HLT, e.g. after a guest panic): it is wedged. The guest is
 * still alive and HVX_RUN may be called again; the stop point is NOT deterministic. */
#define HVX_EXIT_STALL		0xfffffffdU
/* HVX_RUNC exit reason: single-step instruction budget reached, guest alive
 * (this is the real Monitor-Trap-Flag basic exit reason, 37). */
#define HVX_EXIT_STEP		37U

/* Recommended first guest-physical address for userspace-loaded code:
 * pages 0..2 hold the module-built identity page tables. */
#define HVX_GUEST_CODE_BASE	0x3000UL

/* ---- Intel PT guest coverage (HVX_VM_PT_TRACE) ---- */
#define HVX_PT_KERNEL		(1u << 0)	/* trace guest CPL0 (needs an IP range: the host is CPL0 too) */
#define HVX_PT_USER		(1u << 1)	/* trace guest CPL3 */
#define HVX_PT_CR3		(1u << 2)	/* only while guest CR3 == cr3 */
#define HVX_PT_NORETCOMP	(1u << 3)	/* every RET emits a TIP (no return compression) */
struct hvx_pt_config {
	__u64 buf_mib;		/* output buffer in MiB (rounded up to 2 MiB; max 1022); 0 = 256 */
	__u32 flags;		/* HVX_PT_* */
	__u32 pad;
	__u64 cr3;		/* with HVX_PT_CR3 */
	__u64 lo[2], hi[2];	/* IP filter ranges [lo, hi); hi = 0 = range unused */
};
struct hvx_pt_status {
	__u64 bytes;		/* trace bytes written */
	__u64 buf_bytes;	/* buffer size; bytes == buf_bytes with STOPPED = truncated */
	__u64 status;		/* IA32_RTIT_STATUS (bit 4 error, bit 5 stopped) */
	__u64 ctl;		/* IA32_RTIT_CTL used for the guest */
	__u32 saved;		/* 1: this is the last freed VM's trace (no PT VM on this fd) */
	__u32 pad;
};
struct hvx_pt_read {
	__u64 offset, len;	/* byte range of the trace */
	__u64 addr;		/* user buffer */
};
#define HVX_SET_PT  _IOW(HVX_IOC_MAGIC, 42, struct hvx_pt_config)
#define HVX_GET_PT  _IOR(HVX_IOC_MAGIC, 43, struct hvx_pt_status)
#define HVX_READ_PT _IOW(HVX_IOC_MAGIC, 44, struct hvx_pt_read)

/*
 * Seeded tick-timing jitter (HVX_SET_TJITTER / HVX_GET_TJITTER). Needs the PMU branch
 * clock. Tick k of the clock normally lands exactly k*period guest conditional
 * branches into the run; with jitter on it may land LATER by d < period branches
 * (never earlier, and it never drifts: every tick is placed relative to the nominal
 * grid). Moving the tick moves the instruction at which time advances, so it moves
 * where the guest's timer interrupt -- and with it a thread switch -- happens. That
 * is how the explorer tries different thread interleavings, each one reproducible.
 *
 * A tick's delay is a pure function of (seed, k):
 *     h = splitmix64(seed + k*GOLDEN)
 *     delayed if tick_lo <= k < tick_hi (tick_hi 0 = no upper bound) and
 *                h % 1000 < permille;   d = 1 + (h >> 16) % max_delay
 * or, when n_explicit > 0, ONLY the listed (tick, delay) pairs are applied (seed,
 * permille, window and max_delay are ignored). The window and the explicit list let
 * a shrinker narrow a failing seed down to the few ticks that matter.
 *
 * The policy is NOT part of the snapshot, on purpose: restore a snapshot, then set
 * a different policy, to try many timings from one point. The tick index is derived
 * from the snapshot's pmu_next (the grid point below it), so a restored run applies
 * the same delays. Tick 1 is never delayed. Set it before running; off by default
 * (all zero), so runs without it are unchanged.
 */
#define HVX_TJ_MAX_EXPLICIT 32
struct hvx_tjitter {
	__u64 seed;
	__u64 max_delay;	/* seeded mode: max delay in guest branches (< PMU period); 0 = off */
	__u32 permille;		/* seeded mode: chance (per 1000) that a tick is delayed */
	__u32 n_explicit;	/* > 0: apply only explicit[0..n_explicit) */
	__u64 tick_lo, tick_hi;	/* seeded mode: only ticks k in [tick_lo, tick_hi) */
	struct { __u64 tick, delay; } explicit_[HVX_TJ_MAX_EXPLICIT];
	__u64 applied;		/* [GET] ticks delayed so far on the current vCPU */
	__u64 applied_hash;	/* [GET] FNV hash over the (k, d) pairs applied */
	__u64 last_tick;	/* [GET] index of the last tick processed */
};
#define HVX_SET_TJITTER _IOW(HVX_IOC_MAGIC, 45, struct hvx_tjitter)
#define HVX_GET_TJITTER _IOR(HVX_IOC_MAGIC, 46, struct hvx_tjitter)

/*
 * The disk's volatile write cache (HVX_FAULT_DROP_UNFLUSHED, milestone 26): writes the
 * guest was told are done but that no FLUSH has made durable yet. It is NOT in struct
 * hvx_snapshot (up to HVX_UNSYNC_MAX * HVX_UNSYNC_ENTBYTES = 2 MiB). A snapshot taken while
 * writes are held -- in the middle of a power-cut schedule -- must save it with
 * HVX_GET_UNSYNC and put it back with HVX_SET_UNSYNC after HVX_SET_SNAPSHOT, or the
 * restored run silently loses those writes. Buffer layout at `addr` (`len` bytes): n
 * struct hvx_unsync_rec, then n * HVX_UNSYNC_ENTBYTES bytes of data (entry i at offset
 * i * HVX_UNSYNC_ENTBYTES of the data part). GET sets n (and fails with ENOSPC, n still
 * set, if len is too small); SET replaces the cache with the n entries (n = 0 empties it).
 */
#define HVX_UNSYNC_MAX		256	/* max un-flushed writes held at once */
#define HVX_UNSYNC_ENTBYTES	8192	/* max bytes per held write (>= a DB page) */
struct hvx_unsync_rec {
	__u64 doff;		/* byte offset on the disk */
	__u32 len;		/* bytes held (<= HVX_UNSYNC_ENTBYTES) */
	__u32 valid;		/* 0 = superseded by a later persisted write */
};
struct hvx_unsync_io {
	__u64 addr;		/* user buffer */
	__u64 len;		/* its size in bytes */
	__u32 n;		/* entries (GET: out, SET: in) */
	__u32 pad;
};
#define HVX_UNSYNC_BYTES(n) ((__u64)(n) * (sizeof(struct hvx_unsync_rec) + HVX_UNSYNC_ENTBYTES))
#define HVX_GET_UNSYNC _IOWR(HVX_IOC_MAGIC, 47, struct hvx_unsync_io)
#define HVX_SET_UNSYNC _IOW(HVX_IOC_MAGIC, 48, struct hvx_unsync_io)

#endif /* _HVX_ABI_H */
