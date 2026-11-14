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
    /// empty for the scripted vCPU. At x86-64-v3 it holds the AVX state (the upper halves of
    /// YMM0-15) too, in the standard format ([`AVX_WORDS`]).
    pub xsave: Vec<u32>,
    /// The guest's XCR0 (`KVM_GET_XCRS`) at x86-64-v3, where the guest enables XSAVE components
    /// itself (1, 3 or 7); 0 at v2, where it cannot (KVM keeps it at its reset value 1) and the
    /// engine neither reads nor restores it. Only a non-zero value adds the XSAVE state beyond
    /// the legacy area to [`CpuState::hash`], so a v2 fingerprint is the same as before v3 existed.
    pub xcr0: u64,
    /// the KVM-owned MSRs of [`crate::msr::SNAPSHOT`], in that order
    pub msrs: Vec<(u32, u64)>,
    pub events: kvm_vcpu_events,
    pub debugregs: kvm_debugregs,
}

impl CpuState {
    /// The state without what KVM keeps of an event it has finished with: `KVM_GET_VCPU_EVENTS`
    /// reports the vector and error code of the last exception (and the vector of the last
    /// interrupt) KVM injected or re-injected even when nothing is injected or pending any more.
    /// A re-injection happens when a VM exit (an EPT fault, which the dirty-page log causes at
    /// writes) interrupts the delivery of the guest's own exception, so those leftovers depend on
    /// the host; the guest never sees them. Every fingerprint hashes this form since the
    /// fingerprint epoch 2 ([`crate::vmm::Vmm::xfp`], Milestone 204); an image built before is
    /// fingerprinted with them, as it was then, unless the full-state cycles run
    /// (docs/design/kvm-engine.md, "Loops that store").
    pub fn without_stale_events(&self) -> CpuState {
        let mut s = self.clone();
        let e = &mut s.events;
        if e.exception.injected == 0 && e.exception.pending == 0 {
            e.exception.nr = 0;
            e.exception.has_error_code = 0;
            e.exception.error_code = 0;
            if e.exception_has_payload == 0 {
                e.exception_payload = 0;
            }
        }
        if e.interrupt.injected == 0 {
            e.interrupt.nr = 0;
            e.interrupt.soft = 0;
        }
        s
    }

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
        if self.xcr0 != 0 {
            // x86-64-v3: XCR0, which components the header says are not in their initial state
            // (the guest's own XSAVE writes it into its RAM; only the components the guest may
            // enable, not what the host's KVM adds such as PKRU), and the AVX state
            h = fnv_u64(self.xcr0, h);
            if self.xsave.len() >= AVX_WORDS.end {
                let bv = self.xsave[XSTATE_BV_WORD] as u64 & XCR0_GUEST;
                h = fnv_u64(bv, h);
                for w in &self.xsave[AVX_WORDS] {
                    h = fnv(&w.to_le_bytes(), h);
                }
            }
        }
        h
    }
}

/// The parts in which two vCPU states differ, each as `name: a -> b` (empty: none): the
/// registers, each segment and descriptor table, the control registers, the FPU and XSAVE
/// state, the MSRs, the events and the debug registers.
pub fn differences(a: &CpuState, b: &CpuState) -> Vec<String> {
    let mut d = Vec::new();
    let mut part = |name: &str, x: String, y: String| {
        if x != y {
            d.push(format!("{name}: {x} -> {y}"));
        }
    };
    part("regs", format!("{:x?}", a.regs), format!("{:x?}", b.regs));
    let (s, t) = (&a.sregs, &b.sregs);
    for (name, x, y) in [
        ("cs", &s.cs, &t.cs),
        ("ds", &s.ds, &t.ds),
        ("es", &s.es, &t.es),
        ("fs", &s.fs, &t.fs),
        ("gs", &s.gs, &t.gs),
        ("ss", &s.ss, &t.ss),
        ("tr", &s.tr, &t.tr),
        ("ldt", &s.ldt, &t.ldt),
    ] {
        part(name, format!("{x:x?}"), format!("{y:x?}"));
    }
    part("gdt", format!("{:x?}", s.gdt), format!("{:x?}", t.gdt));
    part("idt", format!("{:x?}", s.idt), format!("{:x?}", t.idt));
    for (name, x, y) in [
        ("cr0", s.cr0, t.cr0),
        ("cr2", s.cr2, t.cr2),
        ("cr3", s.cr3, t.cr3),
        ("cr4", s.cr4, t.cr4),
        ("cr8", s.cr8, t.cr8),
        ("efer", s.efer, t.efer),
        ("apic_base", s.apic_base, t.apic_base),
    ] {
        part(name, format!("{x:#x}"), format!("{y:#x}"));
    }
    part(
        "interrupt_bitmap",
        format!("{:x?}", s.interrupt_bitmap),
        format!("{:x?}", t.interrupt_bitmap),
    );
    part("fpu", format!("{:x?}", a.fpu), format!("{:x?}", b.fpu));
    if a.xsave != b.xsave {
        let w: Vec<usize> = (0..a.xsave.len().max(b.xsave.len()))
            .filter(|&i| a.xsave.get(i) != b.xsave.get(i))
            .take(8)
            .collect();
        part("xsave words", format!("{w:?}"), String::new());
    }
    part("xcr0", format!("{:#x}", a.xcr0), format!("{:#x}", b.xcr0));
    part("msrs", format!("{:x?}", a.msrs), format!("{:x?}", b.msrs));
    part(
        "events",
        format!("{:?}", a.events),
        format!("{:?}", b.events),
    );
    part(
        "debugregs",
        format!("{:x?}", a.debugregs),
        format!("{:x?}", b.debugregs),
    );
    d
}

/// The XSAVE components a v3 guest may enable (x87, SSE, AVX: `crate::cpuid::XCR0_V3`).
const XCR0_GUEST: u64 = crate::cpuid::XCR0_V3 as u64;
/// The 32-bit word of a standard XSAVE image that holds the low half of the header's XSTATE_BV
/// (byte 512).
pub const XSTATE_BV_WORD: usize = 512 / 4;
/// The words of a standard XSAVE image that hold the AVX state (bytes 576 to 831).
pub const AVX_WORDS: std::ops::Range<usize> =
    (crate::cpuid::XSAVE_AVX_OFFSET as usize / 4)..(crate::cpuid::XSAVE_SIZE_V3 as usize / 4);

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
    /// Where the guest is and which of its threads runs, from the registers only (the
    /// preemption record, Milestone 208): RIP, CPL 0 or not, CR3, the FS base, RSP.
    fn where_now(&self) -> hydo_hvm::machine::Where {
        let r = self.regs();
        hydo_hvm::machine::Where {
            pc: r.rip,
            kernel: false,
            page_table: self.cr3(),
            thread_pointer: 0,
            sp: r.rsp,
        }
    }
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
    /// ([`crate::spin::decode`]), read through the guest's page tables from `ram`, and its data
    /// flow.
    fn next_step(&self, ram: &[u8], cr3: u64) -> Option<(crate::spin::Step, crate::spin::Flow)> {
        let rip = self.regs().rip;
        let mut code = Vec::with_capacity(15);
        for k in 0..15 {
            match crate::vmm::translate(ram, cr3, rip.wrapping_add(k)) {
                Some(pa) => code.push(ram[pa as usize]),
                None => break,
            }
        }
        let insn = crate::spin::decode(&code)?;
        Some((crate::spin::step_of(rip, &insn)?, insn.flow))
    }
    /// The outputs KVM leaves in `kvm_run` at an exit that the interrupt rule reads (whether an
    /// interrupt can be injected now, RFLAGS.IF) and the window request. They are not part of
    /// [`CpuState`] (`KVM_SET_*` does not set them; a fresh vCPU has them 0 until its first
    /// `KVM_RUN`), so every restore into a fresh vCPU gives them back: a snapshot
    /// ([`crate::vmm::XSnap`]) and the rewind mode's checkpoint keep them.
    fn run_flags(&self) -> [u8; 3] {
        [0; 3]
    }
    fn set_run_flags(&mut self, _f: [u8; 3]) {}
    /// Whether CR4.TSD is set. Without it, user-space RDTSC reads the host's TSC without an exit
    /// (KVM does not trap it), so the guest kernel must set TSD on every CPU
    /// (docs/design/kvm-vcpus.md, the determinism bug of Milestone 219).
    fn tsd(&self) -> bool {
        self.save().sregs.cr4 & (1 << 2) != 0
    }
    /// Whether [`Cpu::single_step`] works (KVM's vCPU; not the scripted one).
    fn can_single_step(&self) -> bool {
        false
    }
    /// Trap-flag single steps on or off (`KVM_GUESTDBG_SINGLESTEP`): while on, every run stops
    /// after one instruction with [`Exit::Debug`]. Only for code the caller has checked: the
    /// flag is visible to an instruction that saves RFLAGS (PUSHF, SYSCALL, an exception's
    /// frame). [`Cpu::step_to`] turns it off.
    fn single_step(&mut self, _on: bool) {}
    /// Start (or stop) KVM's dirty-page log of guest RAM (the rewind mode).
    fn dirty_log(&mut self, _on: bool) {}
    /// Append the pages the guest wrote since the last call (or since the log started), and
    /// start the log afresh.
    fn take_dirty(&mut self, _out: &mut Vec<u64>) {}
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
    /// One VM with `n` vCPUs around the same RAM, vCPU 0 first (docs/design/kvm-vcpus.md). The
    /// default makes one; a maker without several vCPUs refuses more.
    ///
    /// # Safety
    /// As [`Hypervisor::fresh`].
    unsafe fn fresh_n(
        &mut self,
        ram: *mut u8,
        len: usize,
        n: usize,
    ) -> Result<Vec<Box<dyn Cpu>>, String> {
        if n > 1 {
            return Err(format!("hydo-kvm: this vCPU maker has one vCPU, not {n}"));
        }
        Ok(vec![self.fresh(ram, len)?])
    }
    /// One line naming the host (its KVM and CPU), for the engine's diagnostics.
    fn describe(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fnv::OFFSET;

    fn with_xsave(xcr0: u64) -> CpuState {
        let mut s = CpuState {
            xsave: vec![0u32; 1024],
            xcr0,
            ..CpuState::default()
        };
        s.regs.rip = 0x1000;
        s.xsave[6] = 0x1f80; // MXCSR
        s.xsave[XSTATE_BV_WORD] = 3;
        s
    }

    #[test]
    fn a_v2_state_hashes_as_before_v3() {
        // the hash of a v2 state (xcr0 0) reads no XSAVE byte beyond the legacy area, which it
        // takes from `fpu`: whatever the image holds there, the hash is the recorded one
        let a = with_xsave(0);
        let mut b = a.clone();
        b.xsave[AVX_WORDS.start] = 0xdead_beef;
        b.xsave[XSTATE_BV_WORD] = 7;
        assert_eq!(a.hash(OFFSET), b.hash(OFFSET));
        assert_eq!(
            format!("{:016x}", CpuState::default().hash(OFFSET)),
            V2_DEFAULT
        );
    }

    #[test]
    fn a_v3_state_hashes_xcr0_the_header_and_the_avx_state() {
        let a = with_xsave(7);
        assert_ne!(a.hash(OFFSET), with_xsave(0).hash(OFFSET));
        assert_ne!(a.hash(OFFSET), with_xsave(3).hash(OFFSET));
        let changed = |f: &dyn Fn(&mut CpuState)| {
            let mut b = a.clone();
            f(&mut b);
            b.hash(OFFSET) != a.hash(OFFSET)
        };
        assert!(
            changed(&|b| b.xsave[AVX_WORDS.start] = 1),
            "YMM0's upper half"
        );
        assert!(
            changed(&|b| b.xsave[AVX_WORDS.end - 1] = 1),
            "YMM15's upper half"
        );
        assert!(changed(&|b| b.xsave[XSTATE_BV_WORD] = 7), "AVX in use");
        // what the guest cannot see: a component it cannot enable (PKRU, bit 9, which KVM adds
        // on some hosts), the image beyond the AVX state, the header's other words
        assert!(!changed(&|b| b.xsave[XSTATE_BV_WORD] |= 1 << 9));
        assert!(!changed(&|b| b.xsave[AVX_WORDS.end] = 1));
        assert!(!changed(&|b| b.xsave[XSTATE_BV_WORD + 2] = 1));
        assert_eq!(AVX_WORDS, 144..208);
    }

    /// `CpuState::default().hash(OFFSET)` (recorded: the hash of a v2 state did not change when
    /// v3 came).
    const V2_DEFAULT: &str = "e29be6aad729e905";
}
