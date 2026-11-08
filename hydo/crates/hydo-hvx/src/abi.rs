//! Hand-written Rust definitions of `hv/hvx/hvx_abi.h`: the structs the engine passes to the
//! module and the ioctl numbers. Every padding byte the C compiler inserts is an explicit field
//! here (`_pad*`), so every struct is plain bytes and can be zero-initialised and viewed as bytes.
//! `tests/abi.rs` compiles a C probe against the header and checks every size, every field
//! offset and every ioctl number below; change the header and that test fails.

#![allow(non_camel_case_types)]

/// The ioctl request number encoding of Linux (`_IOC` in `asm-generic/ioctl.h`).
pub const fn ioc(dir: u64, ty: u64, nr: u64, size: usize) -> u64 {
    (dir << 30) | ((size as u64) << 16) | (ty << 8) | nr
}
const W: u64 = 1;
const R: u64 = 2;
pub const fn iow<T>(nr: u64) -> u64 {
    ioc(W, MAGIC, nr, std::mem::size_of::<T>())
}
pub const fn ior<T>(nr: u64) -> u64 {
    ioc(R, MAGIC, nr, std::mem::size_of::<T>())
}
pub const fn iowr<T>(nr: u64) -> u64 {
    ioc(R | W, MAGIC, nr, std::mem::size_of::<T>())
}

pub const MAGIC: u64 = 0x48;

pub const HVX_VM_IDLE_HEARTBEAT: u32 = 1 << 0;
pub const HVX_VM_X86_64_V2: u32 = 1 << 1;
pub const HVX_VM_PMU_CLOCK: u32 = 1 << 2;
pub const HVX_VM_PAUSE_EXIT: u32 = 1 << 3;
pub const HVX_VM_PT_TRACE: u32 = 1 << 4;
pub const HVX_VM_SMP_SCHED: u32 = 1 << 5;
pub const HVX_VM_SMP_BOOT: u32 = 1 << 6;
pub const HVX_VM_X86_64_V3: u32 = 1 << 7;
/// M138: the host-call port (hvx_abi.h).
pub const HVX_VM_HOSTCALL: u32 = 1 << 8;
/// M138: the dirty-page log of RAM and disk.
pub const HVX_VM_DIRTY_LOG: u32 = 1 << 9;
/// M195: CPUID describes a fixed cache hierarchy (leaves 2, 4 and 0x8000_0006; hvx_abi.h).
pub const HVX_VM_CPU_CACHE: u32 = 1 << 10;
/// M195: every flag a module of this ABI knows (`HVX_GET_CAPS`).
pub const HVX_VM_KNOWN_FLAGS: u32 = (1 << 11) - 1;
/// M195: CPUID leaf 2 EAX with `HVX_VM_CPU_CACHE`: one round, descriptor 0xff ("see leaf 4").
pub const HVX_CPUID2_EAX: u32 = 0x0000_ff01;
/// M195: CPUID leaf 4, sub-leaves 0..3 (L1 data, L1 instruction, L2, L3), `[EAX, EBX, ECX,
/// EDX]` for one logical processor (`HVX_CPUID4_SUB`). Also the KVM engine's (hydo-kvm's
/// `cpuid`), so both engines describe the same caches.
pub const HVX_CPUID4_SUB: [[u32; 4]; 4] = [
    [0x0000_0121, 0x01c0_003f, 0x0000_003f, 0x0000_0000],
    [0x0000_0122, 0x01c0_003f, 0x0000_003f, 0x0000_0000],
    [0x0000_0143, 0x00c0_003f, 0x0000_03ff, 0x0000_0000],
    [0x0000_0163, 0x03c0_003f, 0x0000_1fff, 0x0000_0006],
];
/// M195: CPUID leaf 0x8000_0006 ECX with `HVX_VM_CPU_CACHE` (the L2: 256 KiB, 4-way, 64 B).
pub const HVX_CPUID_E6_ECX: u32 = 0x0100_4040;
pub const HVX_EXIT_HOSTCALL: u32 = 0xffff_fffa;
pub const HVX_HC_DATA_PORT: u16 = 0x05f0;
pub const HVX_HC_CTL_PORT: u16 = 0x05f4;
pub const HVX_HC_AVAIL_PORT: u16 = 0x05f8;
pub const HVX_HC_MAGIC: u32 = 0x4348_5948;
pub const HVX_HC_SZ: usize = 65536;
pub const HVX_HC_STATE_OVF: u32 = 1 << 0;
pub const HVX_DIRTY_CLEAR: u32 = 1 << 0;

pub const HVX_NR_SW_MSR: usize = 11;
pub const HVX_MSIX_ENTRIES: usize = 4;
pub const HVX_VNET_NQUEUES: usize = 2;
pub const HVX_CONSOLE_SZ: usize = 65536;
pub const HVX_INPUT_MAX: usize = 256;
pub const HVX_DISK_MMAP_PGOFF: u64 = 1 << 32;
pub const HVX_DISK_NO_IDMAP: u64 = 1 << 0;
pub const HVX_FAULT_ERROR: u32 = 1 << 1;
pub const HVX_FAULT_DROP: u32 = 1 << 2;
pub const HVX_FAULT_DROP_UNFLUSHED: u32 = 1 << 5;
pub const HVX_EXIT_VMFAIL: u32 = 0xffff_ffff;
pub const HVX_EXIT_THROTTLE: u32 = 0xffff_fffe;
pub const HVX_EXIT_STEP: u32 = 37;
pub const HVX_TSC_COUNTER: u64 = 0;
pub const HVX_TJ_MAX_EXPLICIT: usize = 32;
pub const HVX_XT_MAX_EXPLICIT: usize = 32;
pub const HVX_UNSYNC_MAX: u64 = 256;
pub const HVX_UNSYNC_ENTBYTES: u64 = 8192;
pub const HVX_SCHED_MAX_VCPUS: usize = 8;

/// `HVX_UNSYNC_BYTES(n)`.
pub const fn unsync_bytes(n: u64) -> u64 {
    n * (std::mem::size_of::<hvx_unsync_rec>() as u64 + HVX_UNSYNC_ENTBYTES)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_config {
    pub mem_size: u64,
    pub n_vcpus: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_regs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_exit {
    pub reason: u32,
    pub insn_len: u32,
    pub qualification: u64,
    pub guest_phys: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_tsc {
    pub tsc: u64,
    pub step: u64,
    pub aux: u64,
    pub count: u64,
    pub mode: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_run_ctl {
    pub flags: u64,
    pub max_insns: u64,
    pub icount: u64,
    pub qualification: u64,
    pub guest_phys: u64,
    pub reason: u32,
    pub insn_len: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_segment {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub ar: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_sregs {
    pub cr0: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub efer: u64,
    pub dr7: u64,
    pub cs: hvx_segment,
    pub ss: hvx_segment,
    pub ds: hvx_segment,
    pub es: hvx_segment,
    pub fs: hvx_segment,
    pub gs: hvx_segment,
    pub tr: hvx_segment,
    pub ldtr: hvx_segment,
    pub gdt_base: u64,
    pub idt_base: u64,
    pub gdt_limit: u32,
    pub idt_limit: u32,
}

/// `struct hvx_snapshot`: the VM state minus guest RAM (and minus the disk).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct hvx_snapshot {
    pub regs: hvx_regs,
    pub tsc: u64,
    pub tsc_step: u64,
    pub tsc_aux: u64,
    pub tsc_mode: u64,
    pub icount: u64,
    pub rdtsc_count: u64,
    pub inject_at_icount: u64,
    pub inject_vector: u32,
    pub inject_valid: u32,
    pub n_guest_pages: u64,
    pub sw_msr: [u64; HVX_NR_SW_MSR],
    pub sregs: hvx_sregs,
    pub lapic_apic_base: u64,
    pub lapic_tsc_deadline: u64,
    pub lapic_reg: [u32; 0x40],
    pub ioapic_ioregsel: u32,
    pub ioapic_id: u32,
    pub ioapic_redir: [u32; 48],
    pub pic_mask: [u32; 2],
    pub lapic_in_service: u32,
    pub uart_ier: u8,
    pub uart_lcr: u8,
    pub uart_mcr: u8,
    pub uart_fcr: u8,
    pub uart_scr: u8,
    pub uart_dll: u8,
    pub uart_dlm: u8,
    pub _pad0: [u8; 5],
    pub vblk_desc: u64,
    pub vblk_avail: u64,
    pub vblk_used: u64,
    pub vblk_qsize: u32,
    pub vblk_qready: u32,
    pub vblk_last_avail: u32,
    pub vblk_isr: u32,
    pub vblk_irq_vec: u32,
    pub vblk_irq_mode: u32,
    pub vblk_irq_pending: u32,
    pub vblk_completion_armed: u32,
    pub vblk_completion_due: u64,
    pub vblk_status: u32,
    pub vblk_qsel: u32,
    pub vblk_dfeat_sel: u32,
    pub vblk_gfeat_sel: u32,
    pub vblk_gfeatures: u64,
    pub vblk_pci_command: u32,
    pub vblk_pci_bar0: u32,
    pub vblk_pci_cfg_addr: u32,
    pub vblk_pci_intline: u32,
    pub vblk_msix_control: u32,
    pub vblk_config_msix: u32,
    pub vblk_queue_msix: u32,
    pub vblk_msix_tbl: [u32; HVX_MSIX_ENTRIES * 4],
    pub _pad1: u32,
    pub fault_seed: u64,
    pub fault_classes: u32,
    pub fault_probability: u32,
    pub fault_max_delay: u32,
    pub fault_kick_seq: u32,
    pub fault_req_seq: u64,
    pub fault_count: u64,
    pub vnet_present: u32,
    pub vnet_mac: [u8; 6],
    pub vnet_pad: u16,
    pub vnet_dev_status: u32,
    pub vnet_qsel: u32,
    pub vnet_dfeat_sel: u32,
    pub vnet_gfeat_sel: u32,
    pub _pad2: u32,
    pub vnet_gfeatures: u64,
    pub vnet_pci_command: u32,
    pub vnet_pci_bar0: u32,
    pub vnet_pci_intline: u32,
    pub vnet_isr: u32,
    pub vnet_msix_control: u32,
    pub vnet_config_msix: u32,
    pub vnet_msix_tbl: [u32; HVX_MSIX_ENTRIES * 4],
    pub vnet_desc: [u64; HVX_VNET_NQUEUES],
    pub vnet_avail: [u64; HVX_VNET_NQUEUES],
    pub vnet_used: [u64; HVX_VNET_NQUEUES],
    pub vnet_qsize: [u32; HVX_VNET_NQUEUES],
    pub vnet_qready: [u32; HVX_VNET_NQUEUES],
    pub vnet_last_avail: [u32; HVX_VNET_NQUEUES],
    pub vnet_queue_msix: [u32; HVX_VNET_NQUEUES],
    pub vnet_completion_armed: [u32; HVX_VNET_NQUEUES],
    pub vnet_completion_due: [u64; HVX_VNET_NQUEUES],
    pub vnet_irq_pending: [u32; HVX_VNET_NQUEUES],
    pub vnet_rx_head: u32,
    pub vnet_rx_tail: u32,
    pub vnet_tx_len: u32,
    pub vnet_tx_count: u32,
    pub vnet_rx_paced: u32,
    pub vnet_pad2: u32,
    pub vnet_rx_next_deliver: u64,
    pub nfault_seed: u64,
    pub nfault_classes: u32,
    pub nfault_probability: u32,
    pub nfault_max_delay: u32,
    pub nfault_rx_kick_seq: u32,
    pub nfault_tx_kick_seq: u32,
    pub nfault_pad: u32,
    pub nfault_rx_seq: u64,
    pub nfault_tx_seq: u64,
    pub nfault_count: u64,
    pub fx: [u8; 512],
    pub pmu_total: u64,
    pub pmu_next: u64,
    pub irr: [u64; 4],
    pub xcr0: u64,
    pub xsave_ext: [u8; 320],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_pmu {
    pub period: u64,
    pub tick_tsc: u64,
    pub total: u64,
    pub next: u64,
    pub ticks: u64,
    pub overshoots: u64,
    pub steps: u64,
    pub land_hash: u64,
    pub hops: u64,
}

#[repr(C)]
pub struct hvx_console {
    pub len: u32,
    pub buf: [u8; HVX_CONSOLE_SZ],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct hvx_input {
    pub len: u32,
    pub buf: [u8; HVX_INPUT_MAX],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_disk {
    pub len: u64,
    pub data: u64,
    pub flags: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_fault {
    pub seed: u64,
    pub classes: u32,
    pub probability: u32,
    pub max_delay: u32,
    pub _pad: u32,
    pub req_seq: u64,
    pub fault_count: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_tj_explicit {
    pub tick: u64,
    pub delay: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_tjitter {
    pub seed: u64,
    pub max_delay: u64,
    pub permille: u32,
    pub n_explicit: u32,
    pub tick_lo: u64,
    pub tick_hi: u64,
    pub explicit_: [hvx_tj_explicit; HVX_TJ_MAX_EXPLICIT],
    pub applied: u64,
    pub applied_hash: u64,
    pub last_tick: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_unsync_rec {
    pub doff: u64,
    pub len: u32,
    pub valid: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_unsync_io {
    pub addr: u64,
    pub len: u64,
    pub n: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_xtick {
    pub seed: u64,
    pub rate: u32,
    pub permille: u32,
    pub lo: u64,
    pub hi: u64,
    pub tsc: u64,
    pub n_explicit: u32,
    pub pad: u32,
    pub explicit_: [u64; HVX_XT_MAX_EXPLICIT],
    pub applied: u64,
    pub applied_hash: u64,
    pub first: u64,
    pub last: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_flushcut {
    pub on: u32,
    pub off: u32,
    pub target: u64,
    pub count: u64,
    pub lo: u64,
    pub hi: u64,
    pub hung: u64,
    pub pad: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_sched_vcpu {
    pub branches: u64,
    pub turns: u64,
    pub cr2: u64,
    pub halted: u32,
    pub halt_len: u32,
    pub reinj_valid: u32,
    pub reinj_info: u32,
    pub reinj_ec: u32,
    pub reinj_len: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_sched {
    pub quantum: u64,
    pub seed: u64,
    pub stop_turn: u64,
    pub pause_k: u32,
    pub n_vcpus: u32,
    pub cur: u32,
    pub in_turn: u32,
    pub turn_over: u32,
    pub pause_run: u32,
    pub turn_end: u64,
    pub turns: u64,
    pub guard: u64,
    pub turn_hash: u64,
    pub tsc: u64,
    pub tsc_step: u64,
    pub tsc_mode: u64,
    pub icount: u64,
    pub pmu_total: u64,
    pub pmu_next: u64,
    pub time_jumps: u64,
    pub deferred: u64,
    pub overshoots: u64,
    pub wakeups: u64,
    pub v: [hvx_sched_vcpu; HVX_SCHED_MAX_VCPUS],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_turnctl {
    pub margin: u64,
    pub burst_seed: u64,
    pub burst_len: u64,
    pub burst_q: u64,
    pub burst_permille: u32,
    pub pad0: u32,
    pub burst_window: u64,
    pub burst_turns: u64,
    pub clipped: u64,
    pub pad: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_ftstat_vcpu {
    pub calls: u64,
    pub total_cyc: u64,
    pub fpu_cyc: u64,
    pub vmxon_cyc: u64,
    pub pmu_cyc: u64,
    pub vmcs_cyc: u64,
    pub load_cyc: u64,
    pub save_cyc: u64,
    pub pmuoff_cyc: u64,
    pub vmxoff_cyc: u64,
    pub step_entries: u64,
    pub step_cyc: u64,
    pub free_entries: u64,
    pub free_cyc: u64,
    pub ovf: u64,
    pub skid_max: u64,
    pub skid_sum: u64,
    pub skid_hist: [u64; 10],
    pub steps_turn: u64,
    pub steps_tick: u64,
    pub rearms: u64,
    pub resident: u64,
    pub step_hw_cyc: u64,
    pub pad: [u64; 5],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_ftstat {
    pub n_vcpus: u32,
    pub pad0: u32,
    pub tsc_khz: u64,
    pub sched_cyc: u64,
    pub sched_turns: u64,
    pub pad: [u64; 4],
    pub v: [hvx_ftstat_vcpu; 8],
}

pub const HVX_PT_USER: u32 = 1 << 1;
pub const HVX_PT_NORETCOMP: u32 = 1 << 3;

/// Intel PT of the guest (HVX_VM_PT_TRACE): the trace's configuration.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_pt_config {
    pub buf_mib: u64,
    pub flags: u32,
    pub pad: u32,
    pub cr3: u64,
    pub lo: [u64; 2],
    pub hi: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_pt_status {
    pub bytes: u64,
    pub buf_bytes: u64,
    pub status: u64,
    pub ctl: u64,
    pub saved: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_pt_read {
    pub offset: u64,
    pub len: u64,
    pub addr: u64,
}

pub const HVX_CREATE_VM: u64 = iow::<hvx_config>(1);
pub const HVX_SET_REGS: u64 = iow::<hvx_regs>(2);
pub const HVX_GET_REGS: u64 = ior::<hvx_regs>(3);
pub const HVX_RUN: u64 = ior::<hvx_exit>(4);
pub const HVX_SET_TSC: u64 = iow::<hvx_tsc>(5);
pub const HVX_GET_TSC: u64 = ior::<hvx_tsc>(6);
pub const HVX_RUNC: u64 = iowr::<hvx_run_ctl>(7);
pub const HVX_GET_SNAPSHOT: u64 = ior::<hvx_snapshot>(8);
pub const HVX_SET_SNAPSHOT: u64 = iow::<hvx_snapshot>(9);
/// Declared with `__u32` in the header (the struct is too large for the size field).
pub const HVX_GET_CONSOLE: u64 = ior::<u32>(11);
pub const HVX_SET_SREGS: u64 = iow::<hvx_sregs>(13);
pub const HVX_PUT_INPUT: u64 = iow::<hvx_input>(14);
pub const HVX_SET_VCPU: u64 = iow::<u32>(15);
pub const HVX_SET_DISK: u64 = iow::<hvx_disk>(16);
pub const HVX_SET_FAULT: u64 = iow::<hvx_fault>(18);
pub const HVX_SET_PMU: u64 = iow::<hvx_pmu>(40);
pub const HVX_GET_PMU: u64 = ior::<hvx_pmu>(41);
pub const HVX_SET_PT: u64 = iow::<hvx_pt_config>(42);
pub const HVX_GET_PT: u64 = ior::<hvx_pt_status>(43);
pub const HVX_READ_PT: u64 = iow::<hvx_pt_read>(44);
pub const HVX_SET_TJITTER: u64 = iow::<hvx_tjitter>(45);
pub const HVX_GET_TJITTER: u64 = ior::<hvx_tjitter>(46);
pub const HVX_GET_UNSYNC: u64 = iowr::<hvx_unsync_io>(47);
pub const HVX_SET_UNSYNC: u64 = iow::<hvx_unsync_io>(48);
pub const HVX_SET_XTICK: u64 = iow::<hvx_xtick>(49);
pub const HVX_GET_XTICK: u64 = ior::<hvx_xtick>(50);
pub const HVX_SET_FLUSHCUT: u64 = iow::<hvx_flushcut>(51);
pub const HVX_GET_FLUSHCUT: u64 = ior::<hvx_flushcut>(52);
pub const HVX_GET_SCHED: u64 = ior::<hvx_sched>(53);
pub const HVX_SET_SCHED: u64 = iow::<hvx_sched>(54);
pub const HVX_GET_FTSTAT: u64 = ior::<hvx_ftstat>(60);
pub const HVX_RESET_FTSTAT: u64 = ioc(0, MAGIC, 61, 0);
pub const HVX_SET_TURNCTL: u64 = iow::<hvx_turnctl>(62);
pub const HVX_GET_TURNCTL: u64 = ior::<hvx_turnctl>(63);
pub const HVX_RESET_VM: u64 = ioc(0, MAGIC, 64, 0);
pub const HVX_GET_DIRTY: u64 = iowr::<hvx_dirty>(65);
pub const HVX_HC_GET: u64 = iowr::<hvx_hc_io>(66);
pub const HVX_HC_PUT: u64 = iow::<hvx_hc_io>(67);
pub const HVX_HC_GET_STATE: u64 = iowr::<hvx_hc_state>(68);
pub const HVX_HC_SET_STATE: u64 = iow::<hvx_hc_state>(69);
pub const HVX_GET_CAPS: u64 = ior::<hvx_caps>(70);

/// `struct hvx_caps` (M195).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_caps {
    pub vm_flags: u32,
    pub reserved: [u32; 7],
}

/// `struct hvx_hc_io` (M138).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_hc_io {
    pub addr: u64,
    pub len: u32,
    pub n: u32,
    pub records: u64,
    pub errors: u64,
}

/// `struct hvx_hc_state` (M138).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_hc_state {
    pub addr: u64,
    pub cap: u32,
    pub out_len: u32,
    pub out_done: u32,
    pub in_len: u32,
    pub flags: u32,
    pub pad: u32,
}

/// `struct hvx_dirty` (M138).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct hvx_dirty {
    pub ram_addr: u64,
    pub ram_bytes: u64,
    pub disk_addr: u64,
    pub disk_bytes: u64,
    pub flags: u32,
    pub pad: u32,
    pub ram_dirty: u64,
    pub disk_dirty: u64,
}

/// A plain-bytes ABI struct: every bit pattern is a valid value (no references, no enums, no
/// implicit padding), so it may be zero-initialised and read as bytes.
///
/// # Safety
/// Implement only for `#[repr(C)]` structs made of integers and arrays of integers whose
/// padding is explicit.
pub unsafe trait Pod: Sized {
    fn zeroed() -> Self {
        // SAFETY: all-zero bytes are a valid value of a Pod type
        unsafe { std::mem::zeroed() }
    }
    fn zeroed_box() -> Box<Self> {
        let layout = std::alloc::Layout::new::<Self>();
        // SAFETY: the layout is the type's; all-zero bytes are a valid value (Pod)
        unsafe {
            let p = std::alloc::alloc_zeroed(layout) as *mut Self;
            if p.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(p)
        }
    }
    fn as_bytes(&self) -> &[u8] {
        // SAFETY: Pod types have no padding and no invalid bytes
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}

macro_rules! pod {
    ($($t:ty),*) => { $(unsafe impl Pod for $t {})* };
}
pod!(
    hvx_pt_config,
    hvx_pt_status,
    hvx_pt_read,
    hvx_config,
    hvx_regs,
    hvx_exit,
    hvx_tsc,
    hvx_run_ctl,
    hvx_segment,
    hvx_sregs,
    hvx_snapshot,
    hvx_pmu,
    hvx_console,
    hvx_input,
    hvx_disk,
    hvx_fault,
    hvx_tjitter,
    hvx_unsync_rec,
    hvx_unsync_io,
    hvx_xtick,
    hvx_flushcut,
    hvx_sched_vcpu,
    hvx_sched,
    hvx_turnctl,
    hvx_ftstat_vcpu,
    hvx_ftstat,
    hvx_hc_io,
    hvx_hc_state,
    hvx_dirty,
    hvx_caps
);

/// The bytes of a field of a Pod struct (for the fingerprint's field-by-field hashing).
pub fn bytes_of<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: only used on integer fields and integer arrays of Pod structs
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}
