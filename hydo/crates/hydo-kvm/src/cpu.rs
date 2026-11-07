//! What the engine needs from a virtual CPU, as a trait: KVM's vCPU (`crate::sys`, Linux) is
//! one implementation, and the tests' scripted CPU ([`crate::script`]) another, so the run loop,
//! the devices, the interrupt rule, snapshots, fingerprints and the lock-step protocol are
//! exercised on any machine.
//!
//! The contract follows KVM's (`KVM_RUN` and the `kvm_run` page):
//!
//! - [`Cpu::run`] runs the guest until an exit that reaches the engine and says what it was;
//! - an I/O read, an MMIO read and an RDMSR are answered with [`Cpu::reply`] before the next run,
//!   and every exit that leaves an instruction to complete (I/O, MMIO, MSR) is then completed
//!   with [`Cpu::complete`] (KVM: a run with `immediate_exit`, which finishes the instruction and
//!   enters no guest code), so the guest is at an instruction boundary at every stop (a string
//!   I/O instruction that needs a second batch gives its next exit from `complete`);
//! - [`Cpu::interruptible`] is whether an external interrupt injected now is taken before the
//!   next instruction (KVM's `ready_for_interrupt_injection`); [`Cpu::inject`] queues one vector
//!   (`KVM_INTERRUPT`); [`Cpu::request_window`] asks for an exit at the next instruction boundary
//!   where the guest can take one (KVM's `request_interrupt_window`, an architectural VM exit);
//! - [`Cpu::save`]/[`Cpu::load`] are the vCPU's whole state ([`CpuState`]).

use crate::abi::{kvm_debugregs, kvm_fpu, kvm_regs, kvm_sregs, kvm_vcpu_events};
use crate::fnv::{fnv, fnv_u64};

/// Why the guest stopped running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// IN or OUT (`count` elements of `size` bytes: a string instruction moves several); for an
    /// OUT, `data` holds the bytes.
    Io {
        port: u16,
        size: u8,
        out: bool,
        count: u32,
        data: Vec<u8>,
    },
    Mmio {
        addr: u64,
        len: u8,
        write: bool,
        data: [u8; 8],
    },
    RdMsr {
        index: u32,
    },
    WrMsr {
        index: u32,
        value: u64,
    },
    Hlt,
    /// The guest can take an interrupt now (the window the engine asked for).
    IrqWindow,
    /// A triple fault.
    Shutdown,
    /// The run was interrupted from the host (a kick); the guest's state did not change.
    Kick,
    /// A step of the spin observer ended at one of its breakpoints ([`Cpu::step_to`]).
    Debug,
    /// Anything else (an internal error, a failed entry): the message.
    Fail(String),
}

/// The answer to a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Io(Vec<u8>),
    Mmio([u8; 8]),
    Msr { value: u64, fault: bool },
}

/// A vCPU's whole state, in KVM's types.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CpuState {
    pub regs: kvm_regs,
    pub sregs: kvm_sregs,
    /// The x87 and SSE state the fingerprint hashes: on KVM read from the XSAVE area's legacy
    /// part (`xsave`), which KVM fills canonically, not with `KVM_GET_FPU`.
    pub fpu: kvm_fpu,
    /// KVM's XSAVE image of the FPU state (`KVM_GET_XSAVE`), restored with `KVM_SET_XSAVE`;
    /// empty for the scripted vCPU.
    pub xsave: Vec<u32>,
    /// the KVM-owned MSRs of [`crate::msr::SNAPSHOT`], in that order
    pub msrs: Vec<(u32, u64)>,
    pub events: kvm_vcpu_events,
    pub debugregs: kvm_debugregs,
}

impl CpuState {
    /// The fingerprint of the state: every architectural field, in a fixed order, no padding.
    pub fn hash(&self, mut h: u64) -> u64 {
        let r = &self.regs;
        for v in [
            r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rsp, r.rbp, r.r8, r.r9, r.r10, r.r11,
            r.r12, r.r13, r.r14, r.r15, r.rip, r.rflags,
        ] {
            h = fnv_u64(v, h);
        }
        let s = &self.sregs;
        for g in [&s.cs, &s.ds, &s.es, &s.fs, &s.gs, &s.ss, &s.tr, &s.ldt] {
            h = fnv_u64(g.base, h);
            h = fnv(&g.limit.to_le_bytes(), h);
            h = fnv(&g.selector.to_le_bytes(), h);
            h = fnv(
                &[
                    g.type_, g.present, g.dpl, g.db, g.s, g.l, g.g, g.avl, g.unusable,
                ],
                h,
            );
        }
        for t in [&s.gdt, &s.idt] {
            h = fnv_u64(t.base, h);
            h = fnv(&t.limit.to_le_bytes(), h);
        }
        for v in [s.cr0, s.cr2, s.cr3, s.cr4, s.cr8, s.efer, s.apic_base] {
            h = fnv_u64(v, h);
        }
        for v in s.interrupt_bitmap {
            h = fnv_u64(v, h);
        }
        let f = &self.fpu;
        for x in &f.fpr {
            h = fnv(x, h);
        }
        h = fnv(&f.fcw.to_le_bytes(), h);
        h = fnv(&f.fsw.to_le_bytes(), h);
        h = fnv(&[f.ftwx], h);
        h = fnv(&f.last_opcode.to_le_bytes(), h);
        h = fnv_u64(f.last_ip, h);
        h = fnv_u64(f.last_dp, h);
        for x in &f.xmm {
            h = fnv(x, h);
        }
        h = fnv(&f.mxcsr.to_le_bytes(), h);
        for (i, v) in &self.msrs {
            h = fnv(&i.to_le_bytes(), h);
            h = fnv_u64(*v, h);
        }
        let e = &self.events;
        h = fnv(
            &[
                e.exception.injected,
                e.exception.nr,
                e.exception.has_error_code,
                e.exception.pending,
            ],
            h,
        );
        h = fnv(&e.exception.error_code.to_le_bytes(), h);
        h = fnv(
            &[
                e.interrupt.injected,
                e.interrupt.nr,
                e.interrupt.soft,
                e.interrupt.shadow,
                e.nmi.injected,
                e.nmi.pending,
                e.nmi.masked,
                e.triple_fault_pending,
                e.exception_has_payload,
            ],
            h,
        );
        h = fnv_u64(e.exception_payload, h);
        let d = &self.debugregs;
        for v in [d.db[0], d.db[1], d.db[2], d.db[3], d.dr6, d.dr7] {
            h = fnv_u64(v, h);
        }
        h
    }
}

/// The x87 and SSE state in `kvm_fpu`'s form from the legacy (FXSAVE) part of an XSAVE image:
/// FCW, FSW, the abridged tag byte, FOP, FIP, FDP, MXCSR, ST0-7 and XMM0-15 (MXCSR_MASK, a
/// constant of the host CPU, is left out).
pub fn fpu_of_xsave(region: &[u32]) -> kvm_fpu {
    let mut b = [0u8; 416];
    for (i, w) in region.iter().take(104).enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let mut f = kvm_fpu {
        fcw: u16_at(0),
        fsw: u16_at(2),
        ftwx: b[4],
        last_opcode: u16_at(6),
        last_ip: u64_at(8),
        last_dp: u64_at(16),
        mxcsr: u32::from_le_bytes(b[24..28].try_into().unwrap()),
        ..kvm_fpu::default()
    };
    for k in 0..8 {
        f.fpr[k].copy_from_slice(&b[32 + 16 * k..48 + 16 * k]);
    }
    for k in 0..16 {
        f.xmm[k].copy_from_slice(&b[160 + 16 * k..176 + 16 * k]);
    }
    f
}

/// A virtual CPU (see the module documentation for the contract).
pub trait Cpu: Send {
    fn run(&mut self) -> Exit;
    fn reply(&mut self, r: Reply);
    /// Finish the instruction of the last exit without running further: None, or the next exit
    /// when the instruction itself needs another one (a string I/O instruction longer than one
    /// batch).
    fn complete(&mut self) -> Option<Exit>;
    fn interruptible(&self) -> bool;
    /// RFLAGS.IF at the last exit (KVM's `if_flag`).
    fn if_flag(&self) -> bool;
    fn inject(&mut self, vector: u8);
    fn request_window(&mut self, on: bool);
    fn save(&self) -> CpuState;
    fn load(&mut self, s: &CpuState);
    /// The instruction pointer (diagnostics).
    fn rip(&self) -> u64;
    /// The general registers.
    fn regs(&self) -> kvm_regs;
    /// CR3 (the guest's page tables).
    fn cr3(&self) -> u64;
    /// Arm the spin observer's step (Some): the next run stops at the first of the step's
    /// addresses the guest reaches and returns [`Exit::Debug`] (or the exit the guest makes
    /// first); an instruction that faults has its exception delivered as in free running. None
    /// disarms it.
    fn step_to(&mut self, step: Option<&crate::spin::Step>);
    /// Whether the spin observer may step this vCPU now: 64-bit code, and the guest uses neither
    /// the trap flag nor breakpoints of its own (their debug exceptions would come to the engine
    /// while it steps).
    fn may_step(&self) -> bool {
        true
    }
    /// The step of the next instruction when the spin observer may step it
    /// ([`crate::spin::decode`]), read through the guest's page tables from `ram`.
    fn next_step(&self, ram: &[u8], cr3: u64) -> Option<crate::spin::Step> {
        let rip = self.regs().rip;
        let mut code = Vec::with_capacity(15);
        for k in 0..15 {
            match crate::vmm::translate(ram, cr3, rip.wrapping_add(k)) {
                Some(pa) => code.push(ram[pa as usize]),
                None => break,
            }
        }
        let insn = crate::spin::decode(&code)?;
        crate::spin::step_of(rip, &insn)
    }
    /// A function another thread calls to make a running [`Cpu::run`] return [`Exit::Kick`]
    /// (the watchdog).
    fn kicker(&self) -> Box<dyn Fn() + Send + Sync>;
}

/// Makes VMs: a fresh VM with guest RAM at `ram` (`len` bytes from GPA 0), the CPUID and the MSR
/// filter set, and one vCPU at reset.
pub trait Hypervisor: Send {
    /// # Safety
    /// `ram..ram+len` stays mapped for the life of the returned vCPU.
    unsafe fn fresh(&mut self, ram: *mut u8, len: usize) -> Result<Box<dyn Cpu>, String>;
    /// One line naming the host (its KVM and CPU), for the engine's diagnostics.
    fn describe(&self) -> String;
}
