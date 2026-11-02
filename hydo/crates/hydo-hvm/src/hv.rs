//! Hypervisor.framework on Apple Silicon, through FFI. With [`crate::mem`] this is the only
//! `unsafe` code of the crate: every other module calls the safe methods of [`Vcpu`] and the
//! VM-wide functions here.
//!
//! The constants are the SDK's (`hv_vcpu_types.h`, `hv_types.h`); `tests/consts.rs` compiles a C
//! probe against the SDK and checks every one, and the layout of `hv_vcpu_exit_t`.
//!
//! A failing call ends the engine as the C engine's `CHK` did (`FAIL <call> = 0x<code>`, status
//! 1), through [`crate::fatal`].

#![allow(non_upper_case_globals)]

use std::ffi::c_void;

pub const HV_REG_X0: u32 = 0;
pub const HV_REG_PC: u32 = 31;
pub const HV_REG_FPCR: u32 = 32;
pub const HV_REG_FPSR: u32 = 33;
pub const HV_REG_CPSR: u32 = 34;
pub const HV_SIMD_FP_REG_Q0: u32 = 0;
pub const HV_INTERRUPT_TYPE_IRQ: u32 = 0;
pub const HV_EXIT_REASON_CANCELED: u32 = 0;
pub const HV_EXIT_REASON_EXCEPTION: u32 = 1;
pub const HV_MEMORY_READ: u64 = 1 << 0;
pub const HV_MEMORY_WRITE: u64 = 1 << 1;
pub const HV_MEMORY_EXEC: u64 = 1 << 2;
pub const RWX: u64 = HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC;
pub const RX: u64 = HV_MEMORY_READ | HV_MEMORY_EXEC;

/// `hv_sys_reg_t` values used by the engine.
pub mod sr {
    pub const SCTLR_EL1: u16 = 49280;
    pub const CPACR_EL1: u16 = 49282;
    pub const TTBR0_EL1: u16 = 49408;
    pub const TTBR1_EL1: u16 = 49409;
    pub const TCR_EL1: u16 = 49410;
    pub const SPSR_EL1: u16 = 49664;
    pub const ELR_EL1: u16 = 49665;
    pub const SP_EL0: u16 = 49672;
    pub const SP_EL1: u16 = 57864;
    pub const AFSR0_EL1: u16 = 49800;
    pub const AFSR1_EL1: u16 = 49801;
    pub const ESR_EL1: u16 = 49808;
    pub const FAR_EL1: u16 = 49920;
    pub const PAR_EL1: u16 = 50080;
    pub const MAIR_EL1: u16 = 50448;
    pub const AMAIR_EL1: u16 = 50456;
    pub const VBAR_EL1: u16 = 50688;
    pub const CONTEXTIDR_EL1: u16 = 50817;
    pub const TPIDR_EL1: u16 = 50820;
    pub const CNTKCTL_EL1: u16 = 50952;
    pub const CSSELR_EL1: u16 = 53248;
    pub const TPIDR_EL0: u16 = 56962;
    pub const TPIDRRO_EL0: u16 = 56963;
    pub const CNTV_CTL_EL0: u16 = 57113;
    pub const CNTV_CVAL_EL0: u16 = 57114;
    pub const MDSCR_EL1: u16 = 32786;
    pub const MDCCINT_EL1: u16 = 32784;
    pub const ACTLR_EL1: u16 = 49281;
    pub const SCXTNUM_EL0: u16 = 56967;
    pub const SCXTNUM_EL1: u16 = 50823;
    pub const TPIDR2_EL0: u16 = 56965;
    pub const APIAKEYLO_EL1: u16 = 49416;
    pub const APIAKEYHI_EL1: u16 = 49417;
    pub const APIBKEYLO_EL1: u16 = 49418;
    pub const APIBKEYHI_EL1: u16 = 49419;
    pub const APDAKEYLO_EL1: u16 = 49424;
    pub const APDAKEYHI_EL1: u16 = 49425;
    pub const APDBKEYLO_EL1: u16 = 49426;
    pub const APDBKEYHI_EL1: u16 = 49427;
    pub const APGAKEYLO_EL1: u16 = 49432;
    pub const APGAKEYHI_EL1: u16 = 49433;
    pub const DBGBVR0_EL1: u16 = 32772;
    pub const DBGBCR0_EL1: u16 = 32773;
    pub const DBGWVR0_EL1: u16 = 32774;
    pub const DBGWCR0_EL1: u16 = 32775;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ExitException {
    syndrome: u64,
    virtual_address: u64,
    physical_address: u64,
}

/// `hv_vcpu_exit_t`.
#[repr(C)]
#[derive(Clone, Copy)]
struct HvExit {
    reason: u32,
    exception: ExitException,
}

/// One 128-bit SIMD&FP register (`hv_simd_fp_uchar16_t`, 16-byte aligned).
#[repr(C, align(16))]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Q(pub [u8; 16]);

#[link(name = "Hypervisor", kind = "framework")]
extern "C" {
    fn hv_vm_create(config: *mut c_void) -> i32;
    fn hv_vm_destroy() -> i32;
    fn hv_vm_map(addr: *mut c_void, ipa: u64, size: usize, flags: u64) -> i32;
    fn hv_vm_protect(ipa: u64, size: usize, flags: u64) -> i32;
    fn hv_vcpu_create(vcpu: *mut u64, exit: *mut *const HvExit, config: *mut c_void) -> i32;
    fn hv_vcpu_destroy(vcpu: u64) -> i32;
    fn hv_vcpu_run(vcpu: u64) -> i32;
    fn hv_vcpus_exit(vcpus: *const u64, count: u32) -> i32;
    fn hv_vcpu_get_reg(vcpu: u64, reg: u32, value: *mut u64) -> i32;
    fn hv_vcpu_set_reg(vcpu: u64, reg: u32, value: u64) -> i32;
    fn hv_vcpu_get_sys_reg(vcpu: u64, reg: u16, value: *mut u64) -> i32;
    fn hv_vcpu_set_sys_reg(vcpu: u64, reg: u16, value: u64) -> i32;
    fn hv_vcpu_get_simd_fp_reg(vcpu: u64, reg: u32, value: *mut Q) -> i32;
    // (vcpu, reg, value) with the 16-byte vector passed in q0: called from inline assembly in
    // [`Vcpu::set_q`], because stable Rust cannot pass a SIMD vector by value through FFI
    fn hv_vcpu_set_simd_fp_reg();
    fn hv_vcpu_set_pending_interrupt(vcpu: u64, ty: u32, pending: bool) -> i32;
    fn hv_vcpu_set_trap_debug_exceptions(vcpu: u64, value: bool) -> i32;
    fn hv_vcpu_set_trap_debug_reg_accesses(vcpu: u64, value: bool) -> i32;
}

extern "C" {
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
}

/// The C engine's `CHK`: a failed call ends the engine.
pub fn chk(what: &str, r: i32) {
    if r != 0 {
        crate::fatal(1, &format!("FAIL {what} = 0x{:x}", r as u32));
    }
}

/// `hv_vm_create(NULL)`: the process's VM (Hypervisor.framework allows one per process).
pub fn vm_create() -> i32 {
    // SAFETY: a NULL configuration is the documented default
    unsafe { hv_vm_create(std::ptr::null_mut()) }
}

pub fn vm_destroy() -> i32 {
    // SAFETY: no arguments; fails cleanly when there is no VM
    unsafe { hv_vm_destroy() }
}

/// Map `size` bytes of host memory at `addr` into the guest at `ipa`.
///
/// # Safety
/// `addr..addr+size` must stay mapped (and page aligned) for as long as the VM uses it.
pub unsafe fn vm_map(addr: *mut u8, ipa: u64, size: usize, flags: u64) -> i32 {
    hv_vm_map(addr.cast(), ipa, size, flags)
}

pub fn vm_protect(ipa: u64, size: u64, flags: u64) {
    // SAFETY: only changes the stage-2 permissions of guest addresses
    chk("hv_vm_protect", unsafe {
        hv_vm_protect(ipa, size as usize, flags)
    });
}

/// Make the instruction cache coherent with `len` bytes the host wrote at `p`.
pub fn icache_invalidate(p: *mut u8, len: usize) {
    // SAFETY: the caller's memory is mapped; the call only flushes caches
    unsafe { sys_icache_invalidate(p.cast(), len) }
}

/// What an exit says (`hv_vcpu_exit_t`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Exit {
    pub reason: u32,
    pub syndrome: u64,
    pub va: u64,
    pub pa: u64,
}

/// A vCPU, owned by the thread that created it.
pub struct Vcpu {
    id: u64,
    ex: *const HvExit,
}

impl Vcpu {
    pub fn create() -> Vcpu {
        let mut id = 0u64;
        let mut ex: *const HvExit = std::ptr::null();
        // SAFETY: both out-pointers are valid; a NULL configuration is the default
        chk("hv_vcpu_create(&vcpu, &ex, NULL)", unsafe {
            hv_vcpu_create(&mut id, &mut ex, std::ptr::null_mut())
        });
        Vcpu { id, ex }
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn destroy(self) {
        // SAFETY: the vCPU was created by this thread and is not running
        chk("hv_vcpu_destroy(vcpu)", unsafe { hv_vcpu_destroy(self.id) });
        std::mem::forget(self);
    }
    pub fn run(&self) {
        // SAFETY: called on the vCPU's own thread
        chk("hv_vcpu_run(vcpu)", unsafe { hv_vcpu_run(self.id) });
    }
    pub fn exit(&self) -> Exit {
        // SAFETY: the framework keeps the exit structure valid for the vCPU's life
        let e = unsafe { *self.ex };
        Exit {
            reason: e.reason,
            syndrome: e.exception.syndrome,
            va: e.exception.virtual_address,
            pa: e.exception.physical_address,
        }
    }
    pub fn get(&self, r: u32) -> u64 {
        let mut v = 0;
        // SAFETY: a valid out-pointer
        chk("hv_vcpu_get_reg(vcpu, r, &v)", unsafe {
            hv_vcpu_get_reg(self.id, r, &mut v)
        });
        v
    }
    pub fn set(&self, r: u32, v: u64) {
        // SAFETY: plain value call
        chk("hv_vcpu_set_reg(vcpu, r, v)", unsafe {
            hv_vcpu_set_reg(self.id, r, v)
        });
    }
    /// `hv_vcpu_get_sys_reg` without CHK: (return code, value; the value starts at `init`).
    pub fn sys_try(&self, r: u16, init: u64) -> (i32, u64) {
        let mut v = init;
        // SAFETY: a valid out-pointer
        let rc = unsafe { hv_vcpu_get_sys_reg(self.id, r, &mut v) };
        (rc, v)
    }
    /// `hv_vcpu_get_sys_reg` whose result the C engine ignored: the value, or `init`.
    pub fn sys(&self, r: u16, init: u64) -> u64 {
        self.sys_try(r, init).1
    }
    pub fn sys_chk(&self, what: &str, r: u16) -> u64 {
        let (rc, v) = self.sys_try(r, 0);
        chk(what, rc);
        v
    }
    /// `hv_vcpu_set_sys_reg`: the return code (callers CHK or ignore it as the C did).
    pub fn set_sys_try(&self, r: u16, v: u64) -> i32 {
        // SAFETY: plain value call
        unsafe { hv_vcpu_set_sys_reg(self.id, r, v) }
    }
    pub fn set_sys(&self, what: &str, r: u16, v: u64) {
        chk(what, self.set_sys_try(r, v));
    }
    pub fn get_q(&self, i: u32) -> Q {
        let mut q = Q::default();
        // SAFETY: a valid, 16-byte aligned out-pointer
        chk(
            "hv_vcpu_get_simd_fp_reg(vcpu, HV_SIMD_FP_REG_Q0 + i, &s->q[i])",
            unsafe { hv_vcpu_get_simd_fp_reg(self.id, HV_SIMD_FP_REG_Q0 + i, &mut q) },
        );
        q
    }
    pub fn set_q(&self, i: u32, q: Q) {
        let r: u64;
        // SAFETY: the AAPCS64 call hv_vcpu_set_simd_fp_reg(x0 = vcpu, w1 = reg, q0 = value): q0 is
        // loaded from `q` (16-byte aligned), the call clobbers what the C ABI lets it clobber
        // (clobber_abi), and its result comes back in x0
        unsafe {
            std::arch::asm!(
                "ldr q0, [{p}]",
                "bl {f}",
                p = in(reg) &q as *const Q,
                f = sym hv_vcpu_set_simd_fp_reg,
                inout("x0") self.id => r,
                in("x1") (HV_SIMD_FP_REG_Q0 + i) as u64,
                clobber_abi("C"),
            );
        }
        chk(
            "hv_vcpu_set_simd_fp_reg(vcpu, HV_SIMD_FP_REG_Q0 + i, s->q[i])",
            r as u32 as i32,
        );
    }
    pub fn set_pending_irq(&self, line: bool) {
        // SAFETY: plain value call
        chk(
            "hv_vcpu_set_pending_interrupt(vcpu, HV_INTERRUPT_TYPE_IRQ, line)",
            unsafe { hv_vcpu_set_pending_interrupt(self.id, HV_INTERRUPT_TYPE_IRQ, line) },
        );
    }
    pub fn trap_debug_exceptions(&self, on: bool) {
        // SAFETY: plain value call
        chk("hv_vcpu_set_trap_debug_exceptions(vcpu, want)", unsafe {
            hv_vcpu_set_trap_debug_exceptions(self.id, on)
        });
    }
    pub fn trap_debug_reg_accesses(&self, on: bool) {
        // SAFETY: plain value call
        chk("hv_vcpu_set_trap_debug_reg_accesses(vcpu, on)", unsafe {
            hv_vcpu_set_trap_debug_reg_accesses(self.id, on)
        });
    }
}

impl Drop for Vcpu {
    /// A vCPU dropped without [`Vcpu::destroy`] (an engine that ended with an error) is
    /// destroyed quietly, so the next engine of this process can create its VM.
    fn drop(&mut self) {
        // SAFETY: the vCPU belongs to this thread and is not running
        unsafe {
            hv_vcpu_destroy(self.id);
        }
    }
}

/// `hv_vcpus_exit(&id, 1)` from another thread (the spin watchdog's kick).
pub fn vcpu_kick(id: u64) {
    // SAFETY: a pointer to one vCPU id; the framework accepts ids of other threads' vCPUs
    unsafe {
        hv_vcpus_exit(&id, 1);
    }
}
