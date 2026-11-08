//! The part of Linux's KVM ABI (`<linux/kvm.h>`, x86) the engine uses, written by hand as
//! `#[repr(C)]` types and ioctl numbers, as hydo-hvx does for `hvx_abi.h`. Pure data: it builds
//! everywhere, and `tests/abi.rs` compiles a C probe against `<linux/kvm.h>` (where the header
//! exists) and compares every struct size, field offset, ioctl number, exit reason and capability
//! number with these.
//!
//! The removed Rust KVM monitor (before M100) used the `kvm-ioctls` and `kvm-bindings` crates;
//! this engine keeps its own few types instead (no dependency, the same checked-layout discipline
//! as the hvx ABI), see docs/design/kvm-engine.md "What was reused".

#![allow(non_camel_case_types)]

pub const KVMIO: u64 = 0xAE;

const fn ioc(dir: u64, nr: u64, size: usize) -> u64 {
    (dir << 30) | ((size as u64) << 16) | (KVMIO << 8) | nr
}
const fn io(nr: u64) -> u64 {
    ioc(0, nr, 0)
}
const fn iow(nr: u64, size: usize) -> u64 {
    ioc(1, nr, size)
}
const fn ior(nr: u64, size: usize) -> u64 {
    ioc(2, nr, size)
}
const fn iowr(nr: u64, size: usize) -> u64 {
    ioc(3, nr, size)
}

pub const KVM_API_VERSION: i32 = 12;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_userspace_memory_region {
    pub slot: u32,
    pub flags: u32,
    pub guest_phys_addr: u64,
    pub memory_size: u64,
    pub userspace_addr: u64,
}

/// `struct kvm_dirty_log` (the bitmap pointer member of its union).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_dirty_log {
    pub slot: u32,
    pub padding1: u32,
    pub dirty_bitmap: u64,
}
/// The memory slot's flag that starts KVM's dirty-page log.
pub const KVM_MEM_LOG_DIRTY_PAGES: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_regs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
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
    pub rflags: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_segment {
    pub base: u64,
    pub limit: u32,
    pub selector: u16,
    pub type_: u8,
    pub present: u8,
    pub dpl: u8,
    pub db: u8,
    pub s: u8,
    pub l: u8,
    pub g: u8,
    pub avl: u8,
    pub unusable: u8,
    pub padding: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_dtable {
    pub base: u64,
    pub limit: u16,
    pub padding: [u16; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_sregs {
    pub cs: kvm_segment,
    pub ds: kvm_segment,
    pub es: kvm_segment,
    pub fs: kvm_segment,
    pub gs: kvm_segment,
    pub ss: kvm_segment,
    pub tr: kvm_segment,
    pub ldt: kvm_segment,
    pub gdt: kvm_dtable,
    pub idt: kvm_dtable,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub apic_base: u64,
    pub interrupt_bitmap: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_fpu {
    pub fpr: [[u8; 16]; 8],
    pub fcw: u16,
    pub fsw: u16,
    pub ftwx: u8,
    pub pad1: u8,
    pub last_opcode: u16,
    pub last_ip: u64,
    pub last_dp: u64,
    pub xmm: [[u8; 16]; 16],
    pub mxcsr: u32,
    pub pad2: u32,
}

/// `struct kvm_xsave`: the vCPU's FPU state in the standard (non-compacted) XSAVE layout, the
/// legacy FXSAVE area first. KVM fills components in their initial state with their initial
/// values (`KVM_GET_XSAVE`), and `KVM_SET_XSAVE` sets the header's XSTATE_BV with them, which
/// `KVM_SET_FPU` does not (Milestone 159).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct kvm_xsave {
    pub region: [u32; 1024],
}
impl Default for kvm_xsave {
    fn default() -> kvm_xsave {
        kvm_xsave { region: [0; 1024] }
    }
}

/// `struct kvm_xcr`: one extended control register (XCR0, the guest's enabled XSAVE features).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_xcr {
    pub xcr: u32,
    pub reserved: u32,
    pub value: u64,
}

/// `struct kvm_xcrs` (`KVM_GET_XCRS`/`KVM_SET_XCRS`): the guest's XCRs; x86-64-v3 guests only
/// (a v2 guest cannot enable XSAVE, its XCR0 stays at the reset value 1).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_xcrs {
    pub nr_xcrs: u32,
    pub flags: u32,
    pub xcrs: [kvm_xcr; 16],
    pub padding: [u64; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_msr_entry {
    pub index: u32,
    pub reserved: u32,
    pub data: u64,
}

/// `struct kvm_msrs` with room for [`MSRS_MAX`] entries (the kernel reads `nmsrs`).
pub const MSRS_MAX: usize = 32;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_msrs_header {
    pub nmsrs: u32,
    pub pad: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct kvm_msrs {
    pub h: kvm_msrs_header,
    pub entries: [kvm_msr_entry; MSRS_MAX],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_cpuid_entry2 {
    pub function: u32,
    pub index: u32,
    pub flags: u32,
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub padding: [u32; 3],
}

/// `KVM_CPUID_FLAG_SIGNIFCANT_INDEX`: the entry is for one subleaf.
pub const KVM_CPUID_FLAG_SIGNIFCANT_INDEX: u32 = 1;

/// `struct kvm_cpuid2` with room for [`CPUID_MAX`] entries.
pub const CPUID_MAX: usize = 256;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_cpuid2_header {
    pub nent: u32,
    pub padding: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct kvm_cpuid2 {
    pub h: kvm_cpuid2_header,
    pub entries: [kvm_cpuid_entry2; CPUID_MAX],
}

impl Default for kvm_cpuid2 {
    fn default() -> kvm_cpuid2 {
        kvm_cpuid2 {
            h: kvm_cpuid2_header::default(),
            entries: [kvm_cpuid_entry2::default(); CPUID_MAX],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_interrupt {
    pub irq: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_vcpu_events_exception {
    pub injected: u8,
    pub nr: u8,
    pub has_error_code: u8,
    pub pending: u8,
    pub error_code: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_vcpu_events_interrupt {
    pub injected: u8,
    pub nr: u8,
    pub soft: u8,
    pub shadow: u8,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_vcpu_events_nmi {
    pub injected: u8,
    pub pending: u8,
    pub masked: u8,
    pub pad: u8,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_vcpu_events_smi {
    pub smm: u8,
    pub pending: u8,
    pub smm_inside_nmi: u8,
    pub latched_init: u8,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_vcpu_events {
    pub exception: kvm_vcpu_events_exception,
    pub interrupt: kvm_vcpu_events_interrupt,
    pub nmi: kvm_vcpu_events_nmi,
    pub sipi_vector: u32,
    pub flags: u32,
    pub smi: kvm_vcpu_events_smi,
    pub triple_fault_pending: u8,
    pub reserved: [u8; 26],
    pub exception_has_payload: u8,
    pub exception_payload: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_debugregs {
    pub db: [u64; 4],
    pub dr6: u64,
    pub dr7: u64,
    pub flags: u64,
    pub reserved: [u64; 9],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct kvm_enable_cap {
    pub cap: u32,
    pub flags: u32,
    pub args: [u64; 4],
    pub pad: [u8; 64],
}

impl Default for kvm_enable_cap {
    fn default() -> kvm_enable_cap {
        kvm_enable_cap {
            cap: 0,
            flags: 0,
            args: [0; 4],
            pad: [0; 64],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_msr_filter_range {
    pub flags: u32,
    pub nmsrs: u32,
    pub base: u32,
    pub bitmap: u64,
}

pub const KVM_MSR_FILTER_MAX_RANGES: usize = 16;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_msr_filter {
    pub flags: u32,
    pub ranges: [kvm_msr_filter_range; KVM_MSR_FILTER_MAX_RANGES],
}

pub const KVM_MSR_FILTER_READ: u32 = 1 << 0;
pub const KVM_MSR_FILTER_WRITE: u32 = 1 << 1;
pub const KVM_MSR_FILTER_DEFAULT_DENY: u32 = 1 << 0;
pub const KVM_MSR_EXIT_REASON_INVAL: u64 = 1 << 0;
pub const KVM_MSR_EXIT_REASON_UNKNOWN: u64 = 1 << 1;
pub const KVM_MSR_EXIT_REASON_FILTER: u64 = 1 << 2;
pub const KVM_PMU_CAP_DISABLE: u64 = 1 << 0;

/// The head of `struct kvm_run` (the shared page of a vCPU), up to the exit union.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_run_head {
    pub request_interrupt_window: u8,
    pub immediate_exit: u8,
    pub padding1: [u8; 6],
    pub exit_reason: u32,
    pub ready_for_interrupt_injection: u8,
    pub if_flag: u8,
    pub flags: u16,
    pub cr8: u64,
    pub apic_base: u64,
}
/// Where the exit union starts in `struct kvm_run`.
pub const KVM_RUN_EXIT_OFFSET: usize = 32;

/// The `io` member of the exit union.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_run_io {
    pub direction: u8,
    pub size: u8,
    pub port: u16,
    pub count: u32,
    pub data_offset: u64,
}
/// The `mmio` member.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_run_mmio {
    pub phys_addr: u64,
    pub data: [u8; 8],
    pub len: u32,
    pub is_write: u8,
}
/// The `msr` member (KVM_EXIT_X86_RDMSR/WRMSR).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_run_msr {
    pub error: u8,
    pub pad: [u8; 7],
    pub reason: u32,
    pub index: u32,
    pub data: u64,
}
/// The `internal` member (KVM_EXIT_INTERNAL_ERROR).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_run_internal {
    pub suberror: u32,
    pub ndata: u32,
    pub data: [u64; 16],
}

pub const KVM_EXIT_UNKNOWN: u32 = 0;
pub const KVM_EXIT_EXCEPTION: u32 = 1;
pub const KVM_EXIT_IO: u32 = 2;
pub const KVM_EXIT_HYPERCALL: u32 = 3;
pub const KVM_EXIT_DEBUG: u32 = 4;
pub const KVM_EXIT_HLT: u32 = 5;
pub const KVM_EXIT_MMIO: u32 = 6;
pub const KVM_EXIT_IRQ_WINDOW_OPEN: u32 = 7;
pub const KVM_EXIT_SHUTDOWN: u32 = 8;
pub const KVM_EXIT_FAIL_ENTRY: u32 = 9;
pub const KVM_EXIT_INTR: u32 = 10;
pub const KVM_EXIT_INTERNAL_ERROR: u32 = 17;
pub const KVM_EXIT_SYSTEM_EVENT: u32 = 24;
pub const KVM_EXIT_X86_RDMSR: u32 = 29;
pub const KVM_EXIT_X86_WRMSR: u32 = 30;
pub const KVM_EXIT_IO_IN: u8 = 0;
pub const KVM_EXIT_IO_OUT: u8 = 1;

pub const KVM_CAP_USER_MEMORY: u32 = 3;
pub const KVM_CAP_SET_TSS_ADDR: u32 = 4;
pub const KVM_CAP_EXT_CPUID: u32 = 7;
pub const KVM_CAP_SET_IDENTITY_MAP_ADDR: u32 = 37;
pub const KVM_CAP_VCPU_EVENTS: u32 = 41;
pub const KVM_CAP_DEBUGREGS: u32 = 50;
pub const KVM_CAP_XSAVE: u32 = 55;
pub const KVM_CAP_XCRS: u32 = 56;
pub const KVM_CAP_IMMEDIATE_EXIT: u32 = 136;
pub const KVM_CAP_X86_USER_SPACE_MSR: u32 = 188;
pub const KVM_CAP_X86_MSR_FILTER: u32 = 189;
pub const KVM_CAP_ENFORCE_PV_FEATURE_CPUID: u32 = 190;
pub const KVM_CAP_PMU_CAPABILITY: u32 = 212;

pub const KVM_GET_API_VERSION: u64 = io(0x00);
pub const KVM_CREATE_VM: u64 = io(0x01);
pub const KVM_CHECK_EXTENSION: u64 = io(0x03);
pub const KVM_GET_VCPU_MMAP_SIZE: u64 = io(0x04);
pub const KVM_GET_SUPPORTED_CPUID: u64 = iowr(0x05, std::mem::size_of::<kvm_cpuid2_header>());
pub const KVM_CREATE_VCPU: u64 = io(0x41);
pub const KVM_GET_DIRTY_LOG: u64 = iow(0x42, std::mem::size_of::<kvm_dirty_log>());
pub const KVM_SET_USER_MEMORY_REGION: u64 =
    iow(0x46, std::mem::size_of::<kvm_userspace_memory_region>());
pub const KVM_SET_TSS_ADDR: u64 = io(0x47);
pub const KVM_SET_IDENTITY_MAP_ADDR: u64 = iow(0x48, 8);
pub const KVM_RUN: u64 = io(0x80);
pub const KVM_GET_REGS: u64 = ior(0x81, std::mem::size_of::<kvm_regs>());
pub const KVM_SET_REGS: u64 = iow(0x82, std::mem::size_of::<kvm_regs>());
pub const KVM_GET_SREGS: u64 = ior(0x83, std::mem::size_of::<kvm_sregs>());
pub const KVM_SET_SREGS: u64 = iow(0x84, std::mem::size_of::<kvm_sregs>());
pub const KVM_INTERRUPT: u64 = iow(0x86, std::mem::size_of::<kvm_interrupt>());
pub const KVM_GET_MSRS: u64 = iowr(0x88, std::mem::size_of::<kvm_msrs_header>());
pub const KVM_SET_MSRS: u64 = iow(0x89, std::mem::size_of::<kvm_msrs_header>());
pub const KVM_GET_FPU: u64 = ior(0x8c, std::mem::size_of::<kvm_fpu>());
pub const KVM_SET_FPU: u64 = iow(0x8d, std::mem::size_of::<kvm_fpu>());
pub const KVM_GET_XSAVE: u64 = ior(0xa4, std::mem::size_of::<kvm_xsave>());
pub const KVM_SET_XSAVE: u64 = iow(0xa5, std::mem::size_of::<kvm_xsave>());
pub const KVM_GET_XCRS: u64 = ior(0xa6, std::mem::size_of::<kvm_xcrs>());
pub const KVM_SET_XCRS: u64 = iow(0xa7, std::mem::size_of::<kvm_xcrs>());
pub const KVM_SET_CPUID2: u64 = iow(0x90, std::mem::size_of::<kvm_cpuid2_header>());
pub const KVM_GET_VCPU_EVENTS: u64 = ior(0x9f, std::mem::size_of::<kvm_vcpu_events>());
pub const KVM_SET_VCPU_EVENTS: u64 = iow(0xa0, std::mem::size_of::<kvm_vcpu_events>());
pub const KVM_GET_DEBUGREGS: u64 = ior(0xa1, std::mem::size_of::<kvm_debugregs>());
pub const KVM_SET_DEBUGREGS: u64 = iow(0xa2, std::mem::size_of::<kvm_debugregs>());
pub const KVM_ENABLE_CAP: u64 = iow(0xa3, std::mem::size_of::<kvm_enable_cap>());
pub const KVM_X86_SET_MSR_FILTER: u64 = iow(0xc6, std::mem::size_of::<kvm_msr_filter>());
pub const KVM_SET_GUEST_DEBUG: u64 = iow(0x9b, std::mem::size_of::<kvm_guest_debug>());

/// `struct kvm_guest_debug` (x86: the debug registers' array).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_guest_debug {
    pub control: u32,
    pub pad: u32,
    pub debugreg: [u64; 8],
}
/// `struct kvm_debug_exit_arch` (x86): the `debug` member of `kvm_run`'s exit union.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct kvm_debug_exit_arch {
    pub exception: u32,
    pub pad: u32,
    pub pc: u64,
    pub dr6: u64,
    pub dr7: u64,
}
pub const KVM_GUESTDBG_ENABLE: u32 = 1;
pub const KVM_GUESTDBG_SINGLESTEP: u32 = 2;
pub const KVM_GUESTDBG_USE_HW_BP: u32 = 0x0002_0000;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The layouts the C probe checks (`tests/abi.rs`), also checked here so a mistake shows on a
    /// machine without `<linux/kvm.h>`. The values are `<linux/kvm.h>`'s on x86_64.
    #[test]
    fn layouts() {
        assert_eq!(size_of::<kvm_regs>(), 144);
        assert_eq!(size_of::<kvm_segment>(), 24);
        assert_eq!(size_of::<kvm_dtable>(), 16);
        assert_eq!(size_of::<kvm_sregs>(), 312);
        assert_eq!(size_of::<kvm_fpu>(), 416);
        assert_eq!(offset_of!(kvm_fpu, xmm), 152);
        assert_eq!(size_of::<kvm_vcpu_events>(), 64);
        assert_eq!(offset_of!(kvm_vcpu_events, exception_payload), 56);
        assert_eq!(size_of::<kvm_debugregs>(), 128);
        assert_eq!(size_of::<kvm_enable_cap>(), 104);
        assert_eq!(size_of::<kvm_msr_filter>(), 392);
        assert_eq!(size_of::<kvm_cpuid_entry2>(), 40);
        assert_eq!(size_of::<kvm_run_head>(), KVM_RUN_EXIT_OFFSET);
        assert_eq!(KVM_RUN, 0xae80);
        assert_eq!(KVM_INTERRUPT, 0x4004_ae86);
        assert_eq!(KVM_GET_REGS, 0x8090_ae81);
        assert_eq!(KVM_SET_SREGS, 0x4138_ae84);
        assert_eq!(size_of::<kvm_dirty_log>(), 16);
        assert_eq!(KVM_GET_DIRTY_LOG, 0x4010_ae42);
        assert_eq!(size_of::<kvm_xcr>(), 16);
        assert_eq!(size_of::<kvm_xcrs>(), 392);
        assert_eq!(KVM_GET_XCRS, 0x8188_aea6);
        assert_eq!(KVM_SET_XCRS, 0x4188_aea7);
    }
}
