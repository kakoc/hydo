//! One vCPU's local APIC in x2APIC mode, emulated by the engine (docs/design/kvm-vcpus.md, "The
//! interrupt controller: a local APIC per vCPU in the engine"). Not used yet: the engine runs one
//! vCPU, which has no APIC (the 8259 PIC is its controller); this model is the part of the
//! several-vCPU work that touches nothing else.
//!
//! KVM creates no in-kernel APIC, so every x2APIC register access (MSRs `0x800..0x8ff`) exits to
//! the engine, which answers it here. The model is a pure function of the accesses and of the
//! clock values the engine passes, so it is deterministic and goes into snapshots and
//! fingerprints as [`Apic::bytes`]:
//!
//! - the request (IRR) and in-service (ISR) registers, the task priority (TPR) and the processor
//!   priority (PPR) derived from them, as the SDM defines;
//! - the local vector table, the spurious-vector register (bit 8 enables the APIC);
//! - the timer, counting in the exit clock with a bus clock of 1 GHz (one count per nanosecond
//!   before the divider), one-shot and periodic; TSC-deadline mode is hidden in CPUID;
//! - the ICR: a write is decoded into a [`Send`] for the engine to route; the model sends
//!   nothing itself;
//! - `IA32_APIC_BASE` latched in x2APIC mode (EN and EXTD; the boot-CPU bit only on the boot
//!   CPU), the rule of hvx (`hv/hvx/hvx.c`, `hvx_lapic_wrmsr`).

/// `IA32_APIC_BASE`.
pub const MSR_APIC_BASE: u32 = 0x1b;
/// The x2APIC MSRs.
pub const X2APIC_FIRST: u32 = 0x800;
pub const X2APIC_LAST: u32 = 0x8ff;

const ID: u32 = 0x802;
const VERSION: u32 = 0x803;
const TPR: u32 = 0x808;
const PPR: u32 = 0x80a;
const EOI: u32 = 0x80b;
const LDR: u32 = 0x80d;
const SVR: u32 = 0x80f;
const ISR0: u32 = 0x810;
const TMR0: u32 = 0x818;
const IRR0: u32 = 0x820;
const ESR: u32 = 0x828;
const LVT_CMCI: u32 = 0x82f;
const ICR: u32 = 0x830;
const LVT_TIMER: u32 = 0x832;
const LVT_ERROR: u32 = 0x837;
const TMICT: u32 = 0x838;
const TMCCT: u32 = 0x839;
const TDCR: u32 = 0x83e;
const SELF_IPI: u32 = 0x83f;

/// The base address, EN (bit 11), EXTD (bit 10, x2APIC mode).
const BASE: u64 = 0xfee0_0000 | (1 << 11) | (1 << 10);
/// The boot-CPU bit of `IA32_APIC_BASE`.
const BSP: u64 = 1 << 8;
/// An LVT entry's mask bit.
const MASKED: u32 = 1 << 16;
/// The version register: version 0x14, 7 LVT entries (CMCI, timer, thermal, performance, LINT0,
/// LINT1, error).
const VERSION_VALUE: u32 = 0x0006_0014;

/// Where an IPI goes (the ICR's shorthand, or its physical destination).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    /// The vCPU whose x2APIC ID is this (the vCPU index).
    Id(u32),
    SelfOnly,
    All,
    AllButSelf,
}

/// What an IPI does at its targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A fixed or lowest-priority interrupt with this vector (lowest priority goes to the one
    /// target the destination names, as in physical mode).
    Fixed(u8),
    Nmi,
    /// INIT with the level asserted: the target waits for STARTUP.
    Init,
    /// STARTUP with this vector: a waiting target starts in real mode at `vector << 12`.
    Startup(u8),
}

/// An IPI the engine routes after an ICR or self-IPI write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Send {
    pub dest: Dest,
    pub kind: Kind,
}

/// The answer to a register write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wrote {
    /// Done; nothing for the engine to do.
    Done,
    /// An IPI to route.
    Send(Send),
    /// A write the model does not emulate: the engine ends the run with this message (Linux in
    /// physical x2APIC mode never makes one).
    Unsupported(String),
    /// A #GP (a read-only or reserved register).
    Fault,
}

/// One vCPU's x2APIC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Apic {
    /// The x2APIC ID: the vCPU index.
    pub id: u32,
    /// `IA32_APIC_BASE` as the guest reads it.
    pub base: u64,
    irr: [u64; 4],
    isr: [u64; 4],
    tpr: u32,
    svr: u32,
    esr: u32,
    /// LVT entries 0x82f and 0x832..0x837: CMCI, timer, thermal, performance, LINT0, LINT1,
    /// error.
    lvt: [u32; 7],
    icr: u64,
    tdcr: u32,
    tmict: u32,
    /// The clock value (ns) at which the timer fires; 0: not armed.
    deadline: u64,
}

fn lvt_index(msr: u32) -> Option<usize> {
    match msr {
        LVT_CMCI => Some(0),
        LVT_TIMER..=LVT_ERROR => Some((msr - LVT_TIMER) as usize + 1),
        _ => None,
    }
}

fn set(r: &mut [u64; 4], v: u8, on: bool) {
    let m = 1u64 << (v % 64);
    if on {
        r[v as usize / 64] |= m;
    } else {
        r[v as usize / 64] &= !m;
    }
}
fn highest(r: &[u64; 4]) -> Option<u8> {
    (0..4)
        .rev()
        .find(|&i| r[i] != 0)
        .map(|i| (i * 64 + 63 - r[i].leading_zeros() as usize) as u8)
}

impl Apic {
    /// The APIC of vCPU `id` after reset (the boot CPU is vCPU 0): every LVT entry masked, the
    /// APIC software-disabled (SVR 0xff), x2APIC mode latched.
    pub fn new(id: u32) -> Apic {
        Apic {
            id,
            base: BASE | if id == 0 { BSP } else { 0 },
            irr: [0; 4],
            isr: [0; 4],
            tpr: 0,
            svr: 0xff,
            esr: 0,
            lvt: [MASKED; 7],
            icr: 0,
            tdcr: 0,
            tmict: 0,
            deadline: 0,
        }
    }

    /// INIT: the reset state, keeping the ID and the base (hvx's `hvx_ap_init`).
    pub fn init(&mut self) {
        let (id, base) = (self.id, self.base);
        *self = Apic::new(id);
        self.base = base;
    }

    /// Bit 8 of the spurious-vector register: the APIC accepts and delivers interrupts.
    pub fn enabled(&self) -> bool {
        self.svr & (1 << 8) != 0
    }

    /// The processor priority: the TPR, or the class of the highest vector in service if higher.
    pub fn ppr(&self) -> u32 {
        let isrv = highest(&self.isr).map_or(0, |v| v as u32);
        if (self.tpr & 0xf0) >= (isrv & 0xf0) {
            self.tpr & 0xff
        } else {
            isrv & 0xf0
        }
    }

    /// A fixed interrupt arrives (an IPI, the timer, a self-IPI): its IRR bit is set. Vectors
    /// below 16 are illegal and set the receive-illegal-vector error instead.
    pub fn post(&mut self, vector: u8) {
        if vector < 16 {
            self.esr |= 1 << 6;
            return;
        }
        set(&mut self.irr, vector, true);
    }

    /// The vector the vCPU would take now, if any: the highest requested vector whose priority
    /// class is above the PPR's, with the APIC enabled.
    pub fn pending(&self) -> Option<u8> {
        if !self.enabled() {
            return None;
        }
        let v = highest(&self.irr)?;
        ((v as u32 & 0xf0) > (self.ppr() & 0xf0)).then_some(v)
    }

    /// The vCPU takes the pending vector (the engine injected it): it moves from the IRR to the
    /// ISR.
    pub fn ack(&mut self) -> Option<u8> {
        let v = self.pending()?;
        set(&mut self.irr, v, false);
        set(&mut self.isr, v, true);
        Some(v)
    }

    /// Whether LINT0 takes the 8259 PIC's vector (ExtINT, unmasked): the boot CPU in virtual
    /// wire mode. A disabled APIC passes LINT0 through, as after reset.
    pub fn extint(&self) -> bool {
        let lint0 = self.lvt[4];
        !self.enabled() || (lint0 & MASKED == 0 && (lint0 >> 8) & 7 == 7)
    }

    fn divide(&self) -> u64 {
        // TDCR bits 0, 1 and 3: 0b000 divides by 2, ..., 0b011 by 16, 0b100 by 32, ..., 0b111 by 1
        let d = (self.tdcr & 3) | ((self.tdcr >> 1) & 4);
        if d == 7 {
            1
        } else {
            2u64 << d
        }
    }

    fn timer_mode(&self) -> u32 {
        (self.lvt[1] >> 17) & 3
    }

    /// The clock value at which the timer fires next; 0 when it is not armed.
    pub fn timer_deadline(&self) -> u64 {
        self.deadline
    }

    /// The clock reached `now`: a due timer posts its vector (unless its LVT entry is masked),
    /// and a periodic one re-arms. True when an interrupt was posted.
    pub fn tick(&mut self, now: u64) -> bool {
        if self.deadline == 0 || now < self.deadline {
            return false;
        }
        let period = self.tmict as u64 * self.divide();
        if self.timer_mode() == 1 && period != 0 {
            // as many periods as have passed, so a long idle jump fires once, not once a period
            let late = (now - self.deadline) / period;
            self.deadline += (late + 1) * period;
        } else {
            self.deadline = 0;
        }
        if self.lvt[1] & MASKED != 0 {
            return false;
        }
        self.post((self.lvt[1] & 0xff) as u8);
        true
    }

    fn current_count(&self, now: u64) -> u32 {
        if self.deadline == 0 || now >= self.deadline {
            return 0;
        }
        ((self.deadline - now).div_ceil(self.divide())).min(u32::MAX as u64) as u32
    }

    /// RDMSR of `msr` at clock value `now`: None is a #GP.
    pub fn rdmsr(&self, msr: u32, now: u64) -> Option<u64> {
        if msr == MSR_APIC_BASE {
            return Some(self.base);
        }
        let v = match msr {
            ID => self.id,
            VERSION => VERSION_VALUE,
            TPR => self.tpr,
            PPR => self.ppr(),
            LDR => ((self.id >> 4) << 16) | (1 << (self.id & 0xf)),
            SVR => self.svr,
            ESR => self.esr,
            ISR0..=0x817 => {
                (self.isr[(msr - ISR0) as usize / 2] >> (32 * ((msr - ISR0) % 2))) as u32
            }
            TMR0..=0x81f => 0, // every interrupt is edge-triggered
            IRR0..=0x827 => {
                (self.irr[(msr - IRR0) as usize / 2] >> (32 * ((msr - IRR0) % 2))) as u32
            }
            ICR => return Some(self.icr),
            TMICT => self.tmict,
            TMCCT => self.current_count(now),
            TDCR => self.tdcr,
            m => match lvt_index(m) {
                Some(i) => self.lvt[i],
                None => return None,
            },
        };
        Some(v as u64)
    }

    /// WRMSR of `msr` with `value` at clock value `now`.
    pub fn wrmsr(&mut self, msr: u32, value: u64, now: u64) -> Wrote {
        if msr == MSR_APIC_BASE {
            // latched in x2APIC mode whatever the guest writes, so it never uses the xAPIC page
            return Wrote::Done;
        }
        let v32 = value as u32;
        match msr {
            TPR => self.tpr = v32 & 0xff,
            EOI => {
                if let Some(v) = highest(&self.isr) {
                    set(&mut self.isr, v, false);
                }
            }
            SVR => {
                self.svr = v32 & 0x1ff;
                if !self.enabled() {
                    // software-disabled: every LVT entry is masked
                    for e in self.lvt.iter_mut() {
                        *e |= MASKED;
                    }
                }
            }
            ESR => self.esr = 0,
            ICR => {
                self.icr = value;
                return decode_icr(value);
            }
            SELF_IPI => {
                return Wrote::Send(Send {
                    dest: Dest::SelfOnly,
                    kind: Kind::Fixed(v32 as u8),
                })
            }
            TMICT => {
                self.tmict = v32;
                self.deadline = if v32 == 0 || self.timer_mode() == 2 {
                    0
                } else {
                    now + v32 as u64 * self.divide()
                };
            }
            TDCR => self.tdcr = v32 & 0xb,
            m => match lvt_index(m) {
                Some(i) => {
                    let mut e = v32;
                    if !self.enabled() {
                        e |= MASKED;
                    }
                    self.lvt[i] = e;
                }
                None => return Wrote::Fault,
            },
        }
        Wrote::Done
    }

    /// The canonical bytes for snapshots and fingerprints, field by field, little-endian.
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(160);
        b.extend_from_slice(&self.id.to_le_bytes());
        b.extend_from_slice(&self.base.to_le_bytes());
        for w in self.irr.iter().chain(self.isr.iter()) {
            b.extend_from_slice(&w.to_le_bytes());
        }
        for v in [self.tpr, self.svr, self.esr] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.lvt {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&self.icr.to_le_bytes());
        for v in [self.tdcr, self.tmict] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&self.deadline.to_le_bytes());
        b
    }
}

/// An ICR value as an IPI.
pub fn decode_icr(icr: u64) -> Wrote {
    let vector = icr as u8;
    let mode = (icr >> 8) & 7;
    let logical = icr >> 11 & 1 != 0;
    let assert = icr >> 14 & 1 != 0;
    let dest = match (icr >> 18) & 3 {
        0 if logical => {
            return Wrote::Unsupported(format!(
                "hydo-kvm: an IPI to a logical destination (ICR {icr:#x}); the guest must use physical x2APIC mode (x2apic_phys)"
            ))
        }
        0 => Dest::Id((icr >> 32) as u32),
        1 => Dest::SelfOnly,
        2 => Dest::All,
        _ => Dest::AllButSelf,
    };
    let kind = match mode {
        0 | 1 => Kind::Fixed(vector),
        4 => Kind::Nmi,
        5 if assert => Kind::Init,
        5 => return Wrote::Done, // INIT level de-assert: no effect
        6 => Kind::Startup(vector),
        _ => {
            return Wrote::Unsupported(format!(
                "hydo-kvm: an IPI with delivery mode {mode} (ICR {icr:#x}) is not emulated"
            ))
        }
    };
    Wrote::Send(Send { dest, kind })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Apic {
        let mut a = Apic::new(1);
        assert_eq!(a.wrmsr(SVR, 0x1ff, 0), Wrote::Done);
        a
    }

    #[test]
    fn the_reset_state_and_the_latched_base() {
        let a = Apic::new(0);
        assert_eq!(a.rdmsr(MSR_APIC_BASE, 0), Some(0xfee0_0d00));
        assert_eq!(Apic::new(1).rdmsr(MSR_APIC_BASE, 0), Some(0xfee0_0c00));
        assert_eq!(a.rdmsr(ID, 0), Some(0));
        assert_eq!(a.rdmsr(LVT_TIMER, 0), Some(MASKED as u64));
        assert!(!a.enabled());
        let mut b = a.clone();
        assert_eq!(b.wrmsr(MSR_APIC_BASE, 0, 0), Wrote::Done);
        assert_eq!(b, a, "IA32_APIC_BASE stays latched");
        assert_eq!(a.rdmsr(0x801, 0), None, "a reserved register is a #GP");
        assert_eq!(b.wrmsr(ID, 5, 0), Wrote::Fault, "the ID is read-only");
    }

    #[test]
    fn priorities_decide_what_is_pending_and_eoi_ends_the_highest() {
        let mut a = on();
        a.post(0x31);
        a.post(0x52);
        assert_eq!(a.pending(), Some(0x52));
        assert_eq!(a.ack(), Some(0x52));
        // 0x31 is of a lower class than the one in service
        assert_eq!(a.pending(), None);
        assert_eq!(a.ppr(), 0x50);
        a.post(0x61);
        assert_eq!(a.ack(), Some(0x61), "a higher class nests");
        assert_eq!(a.wrmsr(EOI, 0, 0), Wrote::Done);
        assert_eq!(
            a.rdmsr(ISR0 + 2, 0),
            Some(1 << (0x52 - 64)),
            "0x61 ended, 0x52 stays"
        );
        assert_eq!(a.wrmsr(EOI, 0, 0), Wrote::Done);
        assert_eq!(a.ack(), Some(0x31));
        // the TPR holds back a class
        let mut b = on();
        b.wrmsr(TPR, 0x40, 0);
        b.post(0x45);
        assert_eq!(b.pending(), None);
        b.post(0x51);
        assert_eq!(b.pending(), Some(0x51));
        assert_eq!(
            b.rdmsr(IRR0 + 2, 0),
            Some((1 << (0x45 - 64)) | (1 << (0x51 - 64)))
        );
    }

    #[test]
    fn a_disabled_apic_delivers_nothing_and_masks_its_lvt() {
        let mut a = Apic::new(0);
        a.post(0x40);
        assert_eq!(a.pending(), None);
        assert!(a.extint(), "a disabled APIC passes the PIC through");
        a.wrmsr(SVR, 0x1ff, 0);
        assert_eq!(a.pending(), Some(0x40));
        a.wrmsr(0x835, 0x700, 0); // LINT0: ExtINT, unmasked
        assert!(a.extint());
        a.wrmsr(0x835, MASKED as u64 | 0x700, 0);
        assert!(!a.extint());
        a.wrmsr(LVT_TIMER, 0x30, 0);
        a.wrmsr(SVR, 0xff, 0);
        assert_eq!(a.rdmsr(LVT_TIMER, 0), Some(MASKED as u64 | 0x30));
        a.post(5);
        assert_eq!(
            a.rdmsr(ESR, 0),
            Some(1 << 6),
            "an illegal vector sets the error"
        );
    }

    #[test]
    fn the_icr_decodes_into_ipis() {
        let send = |dest, kind| Wrote::Send(Send { dest, kind });
        assert_eq!(
            decode_icr(0x1_0000_00fd),
            send(Dest::Id(1), Kind::Fixed(0xfd))
        );
        assert_eq!(
            decode_icr(0x40fd | 1 << 18),
            send(Dest::SelfOnly, Kind::Fixed(0xfd))
        );
        assert_eq!(
            decode_icr(0xfb | 2 << 18),
            send(Dest::All, Kind::Fixed(0xfb))
        );
        assert_eq!(
            decode_icr(0xfb | 3 << 18),
            send(Dest::AllButSelf, Kind::Fixed(0xfb))
        );
        assert_eq!(decode_icr(0x2_0000_4500), send(Dest::Id(2), Kind::Init));
        assert_eq!(decode_icr(0x2_0000_8500), Wrote::Done, "INIT de-assert");
        assert_eq!(
            decode_icr(0x2_0000_0608),
            send(Dest::Id(2), Kind::Startup(8))
        );
        assert_eq!(decode_icr(0x1_0000_0400), send(Dest::Id(1), Kind::Nmi));
        assert!(
            matches!(decode_icr(0x1_0000_08fd), Wrote::Unsupported(_)),
            "logical"
        );
        assert!(
            matches!(decode_icr(0x1_0000_02fd), Wrote::Unsupported(_)),
            "SMI"
        );
        let mut a = on();
        assert_eq!(
            a.wrmsr(SELF_IPI, 0x44, 0),
            send(Dest::SelfOnly, Kind::Fixed(0x44))
        );
        a.wrmsr(ICR, 0x1_0000_00fd, 0);
        assert_eq!(a.rdmsr(ICR, 0), Some(0x1_0000_00fd));
    }

    #[test]
    fn the_timer_counts_in_the_exit_clock() {
        let mut a = on();
        a.wrmsr(TDCR, 0x3, 0); // divide by 16
        a.wrmsr(LVT_TIMER, 0xec, 0); // one-shot, vector 0xec
        a.wrmsr(TMICT, 1000, 5_000);
        assert_eq!(a.timer_deadline(), 21_000);
        assert_eq!(a.rdmsr(TMCCT, 13_000), Some(500));
        assert!(!a.tick(20_999));
        assert!(a.tick(21_000));
        assert_eq!(a.timer_deadline(), 0, "one shot");
        assert_eq!(a.rdmsr(TMCCT, 21_000), Some(0));
        assert_eq!(a.ack(), Some(0xec));
        // periodic, divide by 1; an idle jump over several periods fires once
        let mut p = on();
        p.wrmsr(TDCR, 0xb, 0);
        p.wrmsr(LVT_TIMER, 1 << 17 | 0xec, 0);
        p.wrmsr(TMICT, 100, 0);
        assert!(p.tick(100));
        assert_eq!(p.timer_deadline(), 200);
        assert!(p.tick(450));
        assert_eq!(p.timer_deadline(), 500);
        // masked: the deadline passes and nothing is posted; 0 disarms
        let mut m = on();
        m.wrmsr(LVT_TIMER, MASKED as u64 | 0xec, 0);
        m.wrmsr(TMICT, 10, 0);
        assert!(!m.tick(40));
        assert_eq!(m.pending(), None);
        m.wrmsr(TMICT, 10, 50);
        m.wrmsr(TMICT, 0, 51);
        assert_eq!(m.timer_deadline(), 0);
        // TSC-deadline mode is not offered: arming it does nothing
        let mut t = on();
        t.wrmsr(LVT_TIMER, 2 << 17 | 0xec, 0);
        t.wrmsr(TMICT, 10, 0);
        assert_eq!(t.timer_deadline(), 0);
    }

    #[test]
    fn init_keeps_the_id_and_the_base_and_bytes_are_canonical() {
        let mut a = on();
        a.post(0x40);
        a.wrmsr(TMICT, 0, 0);
        let fresh = Apic::new(1);
        assert_ne!(a.bytes(), fresh.bytes());
        a.init();
        assert_eq!(a, fresh);
        assert_eq!(a.bytes(), fresh.bytes());
        assert_eq!(fresh.bytes().len(), 4 + 8 + 64 + 12 + 28 + 8 + 8 + 8);
    }
}
