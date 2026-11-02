//! hv/hvm/spin_observer.c in Rust: resolving exit-free spin loops deterministically
//! (hv/hvm/spin/RESEARCH.md). Exact-repeat loops are resolved in every run; counter loops need the
//! copy-on-write checkpoint taken at every real exit (the rewind mode, `rw_on`). The functions
//! keep the C names and their order of effects; the statistics live outside the machine state.

use crate::fnv::{fnv, OFFSET};
use crate::hv::{self, sr, HV_REG_CPSR, HV_REG_PC, HV_REG_X0};
use crate::layout::*;
use crate::machine::Mach;
use crate::vmm::{VSnap, Vmm};
use crate::fatal;
use std::io::Write;

/// The observer's outcomes (`OB_*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ob {
    Cont,
    Resolved,
    Deadlock,
    GaveUp,
    Rewound,
    NeedRewind,
}

const OB_MAXHITS: i32 = 64;
/// BAS = all, PMC = EL1 + EL0, enabled
const BCR_ON: u64 = (0xf << 5) | (3 << 1) | 1;

/// `struct sst`: the state compared between breakpoint hits.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Sst {
    pub x: [u64; 31],
    pub pc: u64,
    pub cpsr: u64,
    pub sp0: u64,
    pub sp1: u64,
    pub elr: u64,
    pub spsr: u64,
    pub fpcr: u64,
    pub fpsr: u64,
    pub tp0: u64,
    pub tpro: u64,
    pub tp1: u64,
    pub q: [hv::Q; 32],
}

impl Sst {
    fn hash(&self) -> u64 {
        let mut h = OFFSET;
        for v in self.x {
            h = fnv(&v.to_le_bytes(), h);
        }
        for v in [
            self.pc, self.cpsr, self.sp0, self.sp1, self.elr, self.spsr, self.fpcr, self.fpsr,
            self.tp0, self.tpro, self.tp1,
        ] {
            h = fnv(&v.to_le_bytes(), h);
        }
        for q in &self.q {
            h = fnv(&q.0, h);
        }
        h
    }
}

/// A page and its bytes at a point (`struct cow`).
struct Cow {
    pg: u64,
    data: Vec<u8>,
}

/// The observer's state (spin_observer.c's statics).
pub struct Spin {
    pub rw_on: bool,
    pub ob_on: bool,
    pub ob_stepover: bool,
    pub rp_on: bool,
    /// host-side, re-applied after a vCPU is (re)created
    pub trap_dbg_applied: i32,
    pub protected: bool,
    pub ck_valid: bool,
    ckp: Vec<Cow>,
    ck_vs: VSnap,
    ck_m: Mach,
    pub ob_hits: i32,
    ob_have_prev: i32,
    pub ob_pc: u64,
    ob_mdscr: u64,
    ob_bvr: u64,
    ob_bcr: u64,
    ob_prev: Sst,
    ob_prev2: Sst,
    ob_unch_prev: bool,
    rp_pc: u64,
    dirty: Vec<Cow>,
    splog: Option<std::fs::File>,
    pub dbgreg: u64,
    pub kicks: u64,
    pub checks: u64,
    pub resolved: u64,
    pub notcycle: u64,
    pub steps: u64,
    pub writes: u64,
    pub aff_proven: u64,
    pub aff_resolved: u64,
    pub ck_faults: u64,
    pub ck_takes: u64,
    pub rewinds: u64,
}

impl Spin {
    pub fn new(rw_on: bool, splog: Option<&str>) -> Spin {
        Spin {
            rw_on,
            ob_on: false,
            ob_stepover: false,
            rp_on: false,
            trap_dbg_applied: -1,
            protected: false,
            ck_valid: false,
            ckp: Vec::new(),
            ck_vs: VSnap::default(),
            ck_m: Mach::default(),
            ob_hits: 0,
            ob_have_prev: 0,
            ob_pc: 0,
            ob_mdscr: 0,
            ob_bvr: 0,
            ob_bcr: 0,
            ob_prev: Sst::default(),
            ob_prev2: Sst::default(),
            ob_unch_prev: false,
            rp_pc: 0,
            dirty: Vec::new(),
            splog: splog.and_then(|p| std::fs::File::create(p).ok()),
            dbgreg: 0,
            kicks: 0,
            checks: 0,
            resolved: 0,
            notcycle: 0,
            steps: 0,
            writes: 0,
            aff_proven: 0,
            aff_resolved: 0,
            ck_faults: 0,
            ck_takes: 0,
            rewinds: 0,
        }
    }
    fn log(&mut self, s: std::fmt::Arguments) {
        if let Some(f) = &mut self.splog {
            let _ = f.write_fmt(s);
            let _ = f.write_all(b"\n");
        }
    }
}

/// registers an instruction reads; None = not in the decoded set. `upd` = the X register of a
/// self-update `add/sub Xd, Xd, #imm` (it may read an induction register).
fn insn_reads(i: u32) -> Option<(u32, Option<u32>)> {
    let rn = (i >> 5) & 31;
    let rt = i & 31;
    let rm = (i >> 16) & 31;
    let r = |x: u32| if x != 31 { 1u32 << x } else { 0 };
    if i == 0xd503_201f || i == 0xd503_203f {
        return Some((0, None));
    }
    if (i & 0xfc00_0000) == 0x1400_0000 {
        return Some((0, None));
    }
    if (i & 0xff00_0010) == 0x5400_0000 {
        return Some((0, None));
    }
    if (i & 0x7e00_0000) == 0x3400_0000 || (i & 0x7e00_0000) == 0x3600_0000 {
        return Some((r(rt), None));
    }
    if (i & 0x3fc0_0000) == 0x3940_0000 {
        return Some((r(rn), None));
    }
    if (i & 0x3fc0_0000) == 0x3900_0000 {
        return Some((r(rn) | r(rt), None));
    }
    if (i & 0x3fff_fc00) == 0x08df_fc00 {
        return Some((r(rn), None));
    }
    if (i & 0x7f80_0000) == 0x1100_0000 || (i & 0x7f80_0000) == 0x5100_0000 {
        if rn == rt && rn != 31 {
            return Some((0, Some(rn)));
        }
        return Some((r(rn), None));
    }
    if (i & 0x7f80_0000) == 0x3100_0000 || (i & 0x7f80_0000) == 0x7100_0000 {
        return Some((r(rn), None));
    }
    if (i & 0x7f20_0000) == 0x6b00_0000 || (i & 0x7f20_0000) == 0x2b00_0000 {
        return Some((r(rn) | r(rm), None));
    }
    None
}

fn is_store_exclusive(i: u32) -> bool {
    (i & 0x3fc0_0000) == 0x0800_0000
}
fn is_wfe_sev(i: u32) -> bool {
    i == 0xd503_205f || i == 0xd503_209f || i == 0xd503_20bf
}

/// registers that differ between two states (X0-X30 only; any other difference -> None)
fn sst_xdiff(a: &Sst, b: &Sst) -> Option<([u64; 31], u32)> {
    let mut d = [0u64; 31];
    let mut mask = 0u32;
    let (mut x, mut y) = (*a, *b);
    for i in 0..31 {
        d[i] = a.x[i].wrapping_sub(b.x[i]);
        if d[i] != 0 {
            mask |= 1 << i;
        }
        x.x[i] = 0;
        y.x[i] = 0;
    }
    (x == y).then_some((d, mask))
}

impl Vmm {
    fn sst_take(&self) -> Sst {
        let v = self.v();
        let mut s = Sst::default();
        for i in 0..31 {
            s.x[i] = v.get(HV_REG_X0 + i as u32);
        }
        s.pc = v.get(HV_REG_PC);
        s.cpsr = v.get(HV_REG_CPSR) & !(1u64 << 21); // PSTATE.SS is ours
        s.fpcr = v.get(hv::HV_REG_FPCR);
        s.fpsr = v.get(hv::HV_REG_FPSR);
        s.sp0 = v.sys(sr::SP_EL0, 0);
        s.sp1 = v.sys(sr::SP_EL1, 0);
        s.elr = v.sys(sr::ELR_EL1, 0);
        s.spsr = v.sys(sr::SPSR_EL1, 0);
        s.tp0 = v.sys(sr::TPIDR_EL0, 0);
        s.tpro = v.sys(sr::TPIDRRO_EL0, 0);
        s.tp1 = v.sys(sr::TPIDR_EL1, 0);
        for i in 0..32 {
            s.q[i] = v.get_q(i as u32);
        }
        s
    }

    /// stage-1 walk (4 KiB granule) to fetch the guest instruction at a virtual PC
    fn fetch_insn(&self, va: u64) -> Option<u32> {
        let v = self.v();
        let sctlr = v.sys(sr::SCTLR_EL1, 0);
        let ram = self.ram.bytes();
        let rd8 = |pa: u64| -> u64 {
            let o = (pa - RAM_BASE) as usize;
            u64::from_le_bytes(ram[o..o + 8].try_into().unwrap())
        };
        let pa;
        if sctlr & 1 == 0 {
            pa = va;
        } else {
            let tcr = v.sys(sr::TCR_EL1, 0);
            let hi = (va >> 63) & 1 != 0;
            let tsz = if hi { (tcr >> 16) & 63 } else { tcr & 63 } as i32;
            let ttbr = v.sys(if hi { sr::TTBR1_EL1 } else { sr::TTBR0_EL1 }, 0);
            let bits = 64 - tsz;
            let first = 4 - (bits - 12 + 8) / 9;
            let mut level = first;
            let mut table = ttbr & 0x0000_ffff_ffff_fffe;
            loop {
                let shift = 12 + 9 * (3 - level);
                let m = if level == first {
                    (1u64 << (bits - shift)).wrapping_sub(1)
                } else {
                    511
                };
                let idx = (va >> shift) & m;
                let dpa = (table & !0xfff).wrapping_add(idx * 8);
                if dpa < RAM_BASE || dpa + 8 > RAM_BASE + self.ram_size {
                    return None;
                }
                let d = rd8(dpa);
                if d & 1 == 0 {
                    return None;
                }
                if level == 3 {
                    pa = (d & 0x0000_ffff_ffff_f000) | (va & 0xfff);
                    break;
                }
                if d & 2 == 0 {
                    let m = (1u64 << shift) - 1;
                    pa = (d & 0x0000_ffff_ffff_f000 & !m) | (va & m);
                    break;
                }
                table = d & 0x0000_ffff_ffff_f000;
                level += 1;
            }
        }
        if pa < RAM_BASE || pa + 4 > RAM_BASE + self.ram_size {
            return None;
        }
        let o = (pa - RAM_BASE) as usize;
        Some(u32::from_le_bytes(ram[o..o + 4].try_into().unwrap()))
    }

    fn page(&self, pg: u64, n: u64) -> Vec<u8> {
        let o = (pg - RAM_BASE) as usize;
        self.ram.bytes()[o..o + n as usize].to_vec()
    }

    // ---------------- the copy-on-write checkpoint (rewind mode) ----------------
    /// first write to pg since E: keep its bytes
    pub fn ck_note(&mut self, pg: u64) {
        if !self.sp.rw_on || self.sp.ckp.iter().any(|c| c.pg == pg) {
            return;
        }
        let data = self.page(pg, 0x4000);
        self.sp.ckp.push(Cow { pg, data });
        self.sp.ck_faults += 1;
    }
    /// all of RAM read-only except the pages already saved
    fn ck_protect_state(&mut self) {
        hv::vm_protect(RAM_BASE, self.ram_size, hv::RX);
        for c in &self.sp.ckp {
            hv::vm_protect(c.pg, 0x4000, hv::RWX);
        }
    }
    /// at a real exit, before the next entry
    pub fn ck_take(&mut self) {
        for c in &self.sp.ckp {
            hv::vm_protect(c.pg, 0x4000, hv::RX);
        }
        self.sp.ckp.clear();
        if !self.sp.ck_valid {
            hv::vm_protect(RAM_BASE, self.ram_size, hv::RX);
        }
        self.sp.ck_vs = self.vtake();
        self.sp.ck_m = self.m;
        self.sp.ck_valid = true;
        self.sp.ck_takes += 1;
    }
    fn ck_rewind(&mut self) {
        let ckp = std::mem::take(&mut self.sp.ckp);
        {
            let b = self.ram.bytes_mut();
            for c in &ckp {
                let o = (c.pg - RAM_BASE) as usize;
                b[o..o + 0x4000].copy_from_slice(&c.data);
            }
        }
        self.sp.ckp = ckp;
        let vs = self.sp.ck_vs;
        self.vput(&vs);
        self.m = self.sp.ck_m;
        self.sp.rewinds += 1;
        self.ck_protect_state();
    }

    /// Back to a clean observer state after the vCPU/RAM were restored (`spin_reset`).
    pub fn spin_reset(&mut self) {
        self.sp.ckp.clear();
        self.sp.ck_valid = false;
        self.sp.dirty.clear();
        self.sp.ob_on = false;
        self.sp.ob_stepover = false;
        self.sp.rp_on = false;
        self.sp.protected = false;
        self.watch.set_backoff(1.0);
        hv::vm_protect(RAM_BASE, self.ram_size, hv::RWX);
    }

    // ---------------- the observer ----------------
    fn sp_protect(&mut self, on: bool) {
        if !on && self.sp.rw_on && self.sp.ck_valid {
            self.ck_protect_state();
        } else {
            hv::vm_protect(RAM_BASE, self.ram_size, if on { hv::RX } else { hv::RWX });
        }
        self.sp.protected = on;
    }
    pub fn dirty_add(&mut self, pg: u64) {
        self.ck_note(pg);
        if self.sp.dirty.iter().any(|c| c.pg == pg) {
            return;
        }
        let data = self.page(pg, PG);
        self.sp.dirty.push(Cow { pg, data });
        hv::vm_protect(pg, PG, hv::RWX);
        self.sp.writes += 1;
    }
    fn dirty_unchanged(&self) -> bool {
        let b = self.ram.bytes();
        self.sp.dirty.iter().all(|c| {
            let o = (c.pg - RAM_BASE) as usize;
            c.data[..] == b[o..o + PG as usize]
        })
    }
    /// re-protect what was written, forget the copies
    fn dirty_reset(&mut self) {
        for c in &self.sp.dirty {
            hv::vm_protect(c.pg, PG, hv::RX);
        }
        self.sp.dirty.clear();
    }
    fn set_sr(&self, what: &str, r: u16, v: u64) {
        self.v().set_sys(what, r, v);
    }
    pub fn ob_arm(&mut self, pc: u64) {
        if !self.sp.ob_on {
            self.sp.ob_mdscr = self.v().sys(sr::MDSCR_EL1, self.sp.ob_mdscr);
            self.sp.ob_bvr = self.v().sys(sr::DBGBVR0_EL1, self.sp.ob_bvr);
            self.sp.ob_bcr = self.v().sys(sr::DBGBCR0_EL1, self.sp.ob_bcr);
            self.sp_protect(true);
            self.v().trap_debug_reg_accesses(true);
            self.sp.ob_on = true;
        } else {
            self.dirty_reset();
        }
        self.sp.ob_pc = pc;
        self.sp.ob_hits = 0;
        self.sp.ob_have_prev = 0;
        self.sp.ob_unch_prev = false;
        self.set_sr("hv_vcpu_set_sys_reg(MDSCR_EL1)", sr::MDSCR_EL1, self.sp.ob_mdscr | (1 << 15) | (1 << 13));
        self.set_sr("hv_vcpu_set_sys_reg(DBGBVR0_EL1)", sr::DBGBVR0_EL1, pc);
        self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1)", sr::DBGBCR0_EL1, BCR_ON);
        self.sp.checks += 1;
        if self.sp.splog.is_some() {
            let (ex, cpsr) = (self.m.exits, self.rd(HV_REG_CPSR));
            self.sp.log(format_args!("arm at exit {ex} pc 0x{pc:x} cpsr 0x{cpsr:x}"));
        }
    }
    pub fn ob_disarm(&mut self) {
        if !self.sp.ob_on {
            return;
        }
        self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1)", sr::DBGBCR0_EL1, self.sp.ob_bcr);
        self.set_sr("hv_vcpu_set_sys_reg(DBGBVR0_EL1)", sr::DBGBVR0_EL1, self.sp.ob_bvr);
        self.set_sr("hv_vcpu_set_sys_reg(MDSCR_EL1)", sr::MDSCR_EL1, self.sp.ob_mdscr);
        self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) & !(1u64 << 21));
        self.sp.dirty.clear();
        if self.sp.protected {
            self.sp_protect(false);
        }
        self.v().trap_debug_reg_accesses(false);
        self.sp.ob_on = false;
        self.sp.ob_stepover = false;
    }
    /// While armed, guest accesses to MDSCR_EL1 and the debug registers trap: emulate them on the
    /// guest's own values. True if handled (not a guest-visible exit).
    pub fn ob_debug_reg(&mut self) -> bool {
        let s = self.esr();
        let op0 = (s >> 20) & 3;
        let op2 = (s >> 17) & 7;
        let op1 = (s >> 14) & 7;
        let crn = (s >> 10) & 15;
        let rt = ((s >> 5) & 31) as u32;
        let crm = (s >> 1) & 15;
        let rdir = s & 1 != 0;
        if op0 != 2 || op1 != 0 || crn != 0 {
            return false;
        }
        if !(op2 >= 4 || (crm == 2 && op2 == 2)) {
            return false;
        }
        let r = ((op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2) as u16;
        let slot = match r {
            sr::MDSCR_EL1 => Some(0),
            sr::DBGBVR0_EL1 => Some(1),
            sr::DBGBCR0_EL1 => Some(2),
            _ => None,
        };
        if rdir {
            let v = match slot {
                Some(0) => self.sp.ob_mdscr,
                Some(1) => self.sp.ob_bvr,
                Some(2) => self.sp.ob_bcr,
                _ => self.v().sys(r, 0),
            };
            self.xset(rt, v);
        } else {
            let v = self.xget(rt);
            match slot {
                Some(0) => self.sp.ob_mdscr = v,
                Some(1) => self.sp.ob_bvr = v,
                Some(2) => self.sp.ob_bcr = v,
                _ => {
                    self.v().set_sys_try(r, v);
                }
            }
        }
        if r == sr::MDSCR_EL1 {
            let ss = if self.sp.ob_stepover { 1 } else { 0 };
            self.set_sr(
                "hv_vcpu_set_sys_reg(MDSCR_EL1)",
                sr::MDSCR_EL1,
                self.sp.ob_mdscr | (1 << 15) | (1 << 13) | ss,
            );
        }
        self.skip();
        self.sp.dbgreg += 1;
        true
    }
    /// one exact single step inside a PROVEN cycle; Err on anything but a completed step
    fn cyc_step(&mut self) -> Result<(), i32> {
        let pc = self.rd(HV_REG_PC);
        match self.fetch_insn(pc) {
            Some(i) if !is_store_exclusive(i) && !is_wfe_sev(i) => {}
            _ => return Err(-1),
        }
        self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1, 0)", sr::DBGBCR0_EL1, 0);
        loop {
            self.set_sr(
                "hv_vcpu_set_sys_reg(MDSCR_EL1)",
                sr::MDSCR_EL1,
                self.sp.ob_mdscr | 1 | (1 << 13) | (1 << 15),
            );
            self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) | (1u64 << 21));
            self.v().run();
            let e = self.v().exit();
            if e.reason == hv::HV_EXIT_REASON_CANCELED {
                continue;
            }
            let ec = (e.syndrome >> 26) & 0x3f;
            if ec == 0x32 || ec == 0x33 {
                self.sp.steps += 1;
                return Ok(());
            }
            if ec == 0x24 && self.sp.protected && e.pa >= RAM_BASE && e.pa < RAM_BASE + self.ram_size {
                self.dirty_add(e.pa & !(PG - 1));
                continue;
            }
            return Err(-2); // a real exit inside a proven cycle: impossible
        }
    }
    /// cur, prev, prev2 at three consecutive breakpoint hits: an affine counter loop? The
    /// canonical PC if proven infinite.
    fn aff_prove(&mut self, cur: &Sst) -> Option<u64> {
        let (d1, m1) = sst_xdiff(cur, &self.sp.ob_prev)?;
        let (d2, m2) = sst_xdiff(&self.sp.ob_prev, &self.sp.ob_prev2)?;
        if m1 == 0 || m1 != m2 || d1 != d2 {
            return None;
        }
        self.dirty_reset();
        let mut best = u64::MAX;
        let mut steps = 0;
        let st = loop {
            let st = self.sst_take();
            if steps != 0 && st.pc == cur.pc {
                break st;
            }
            if st.cpsr & 0x80 == 0 && st.pc < best {
                best = st.pc;
            }
            let insn = self.fetch_insn(st.pc)?;
            let (rmask, _) = insn_reads(insn)?;
            if rmask & m1 != 0 {
                return None;
            }
            let s0 = steps;
            steps += 1;
            if s0 > 4096 || self.cyc_step().is_err() {
                return None;
            }
        };
        let (d3, m3) = sst_xdiff(&st, cur)?;
        if m3 != m1 || d3 != d1 {
            return None;
        }
        if !self.dirty_unchanged() || best == u64::MAX {
            return None;
        }
        Some(best)
    }
    /// A proven exit-free loop is an idle wait: deliver as idle_wait() does.
    fn spin_deliver(&mut self) -> Ob {
        if self.irq_line() {
            return Ob::Resolved;
        }
        let next = self.next_event();
        if next > self.m.vtime {
            self.m.vtime = next;
            self.m.n_wfi_jump += 1;
            return Ob::Resolved;
        }
        if next != 0 {
            return Ob::Resolved;
        }
        Ob::Deadlock
    }
    /// the breakpoint at ob_pc was hit (the instruction there has not executed)
    pub fn ob_hit(&mut self) -> Ob {
        let cur = self.sst_take();
        if self.sp.ob_have_prev != 0 && cur == self.sp.ob_prev && self.dirty_unchanged() {
            // one full iteration returned to exactly this state and left memory as it was
            let mut st: Vec<Sst> = Vec::with_capacity(64);
            let mut h: Vec<u64> = Vec::with_capacity(64);
            self.dirty_reset();
            st.push(cur);
            h.push(cur.hash());
            let mut p = 0usize;
            loop {
                if p >= 4096 || self.cyc_step().is_err() {
                    self.ob_disarm();
                    self.sp.notcycle += 1;
                    return Ob::GaveUp;
                }
                p += 1;
                let s = self.sst_take();
                if s == st[0] {
                    st.push(s);
                    h.push(0);
                    break;
                }
                h.push(s.hash());
                st.push(s);
            }
            if !self.dirty_unchanged() {
                self.ob_disarm();
                self.sp.notcycle += 1;
                return Ob::GaveUp;
            }
            let mut best: Option<usize> = None;
            for k in 0..p {
                if st[k].cpsr & 0x80 == 0 && best.is_none_or(|b| h[k] < h[b]) {
                    best = Some(k);
                }
            }
            let Some(best) = best else {
                self.ob_disarm();
                return Ob::Deadlock;
            };
            for _ in 0..best {
                if self.cyc_step().is_err() {
                    fatal(3, "hvm: spin: cycle changed");
                }
            }
            self.ob_disarm();
            self.sp.resolved += 1;
            if self.sp.splog.is_some() {
                let pc = self.rd(HV_REG_PC);
                self.sp
                    .log(format_args!("  resolved: period {p}, canonical +{best} at pc 0x{pc:x}"));
            }
            return self.spin_deliver();
        }
        if !self.sp.rw_on
            && self.sp.ob_have_prev >= 2
            && self.sp.ob_unch_prev
            && self.dirty_unchanged()
            && self.aff_prove(&cur).is_some()
        {
            // without the checkpoint a counter loop cannot be resolved: stop at once (SPIN)
            self.ob_disarm();
            self.sp.aff_proven += 1;
            return Ob::NeedRewind;
        }
        if self.sp.rw_on && self.sp.ck_valid && self.sp.ob_have_prev >= 2 && self.sp.ob_unch_prev && self.dirty_unchanged() {
            if let Some(pstar) = self.aff_prove(&cur) {
                self.sp.aff_proven += 1;
                if self.sp.splog.is_some() {
                    let ex = self.sp.ck_m.exits;
                    self.sp.log(format_args!(
                        "  counter loop proven at pc 0x{:x}, canonical pc 0x{pstar:x}: rewind to exit {ex}",
                        cur.pc
                    ));
                }
                self.ob_disarm();
                self.ck_rewind();
                self.sp.rp_pc = pstar;
                self.sp.rp_on = true;
                self.sp.ob_mdscr = self.v().sys(sr::MDSCR_EL1, self.sp.ob_mdscr);
                self.sp.ob_bvr = self.v().sys(sr::DBGBVR0_EL1, self.sp.ob_bvr);
                self.sp.ob_bcr = self.v().sys(sr::DBGBCR0_EL1, self.sp.ob_bcr);
                self.v().trap_debug_reg_accesses(true);
                self.set_sr("hv_vcpu_set_sys_reg(MDSCR_EL1)", sr::MDSCR_EL1, self.sp.ob_mdscr | (1 << 15) | (1 << 13));
                self.set_sr("hv_vcpu_set_sys_reg(DBGBVR0_EL1)", sr::DBGBVR0_EL1, self.sp.rp_pc);
                self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1)", sr::DBGBCR0_EL1, BCR_ON);
                return Ob::Rewound;
            }
        }
        self.sp.ob_hits += 1;
        if self.sp.ob_hits > OB_MAXHITS {
            self.ob_disarm();
            self.sp.notcycle += 1;
            self.sp.log(format_args!("  gave up after {OB_MAXHITS} hits"));
            return Ob::GaveUp;
        }
        self.sp.ob_unch_prev = if self.sp.ob_have_prev != 0 {
            self.dirty_unchanged()
        } else {
            false
        };
        self.sp.ob_prev2 = self.sp.ob_prev;
        self.sp.ob_prev = cur;
        self.sp.ob_have_prev = if self.sp.ob_have_prev >= 2 {
            2
        } else {
            self.sp.ob_have_prev + 1
        };
        self.dirty_reset();
        // step over the breakpointed instruction: breakpoint off, one software step
        match self.fetch_insn(cur.pc) {
            Some(i) if !is_store_exclusive(i) => {}
            _ => {
                self.ob_disarm();
                self.sp.notcycle += 1;
                return Ob::GaveUp;
            }
        }
        self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1, 0)", sr::DBGBCR0_EL1, 0);
        self.set_sr(
            "hv_vcpu_set_sys_reg(MDSCR_EL1)",
            sr::MDSCR_EL1,
            self.sp.ob_mdscr | 1 | (1 << 13) | (1 << 15),
        );
        self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) | (1u64 << 21));
        self.sp.ob_stepover = true;
        Ob::Cont
    }

    fn rp_disarm(&mut self) {
        self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1)", sr::DBGBCR0_EL1, self.sp.ob_bcr);
        self.set_sr("hv_vcpu_set_sys_reg(DBGBVR0_EL1)", sr::DBGBVR0_EL1, self.sp.ob_bvr);
        self.set_sr("hv_vcpu_set_sys_reg(MDSCR_EL1)", sr::MDSCR_EL1, self.sp.ob_mdscr);
        self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) & !(1u64 << 21));
        self.v().trap_debug_reg_accesses(false);
        self.sp.rp_on = false;
        self.sp.ob_stepover = false;
    }
    /// replay reached the canonical PC (the instruction there has not executed)
    pub fn rp_hit(&mut self) -> Ob {
        if self.rd(HV_REG_CPSR) & 0x80 != 0 {
            // IRQs masked here: step over, keep counting
            self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1, 0)", sr::DBGBCR0_EL1, 0);
            self.set_sr(
                "hv_vcpu_set_sys_reg(MDSCR_EL1)",
                sr::MDSCR_EL1,
                self.sp.ob_mdscr | 1 | (1 << 13) | (1 << 15),
            );
            self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) | (1u64 << 21));
            self.sp.ob_stepover = true;
            return Ob::Cont;
        }
        self.rp_disarm();
        self.sp.aff_resolved += 1;
        if self.sp.splog.is_some() {
            let pc = self.rd(HV_REG_PC);
            self.sp.log(format_args!("  replay delivered at pc 0x{pc:x}"));
        }
        self.spin_deliver()
    }
    pub fn ob_stepover_done(&mut self) {
        self.sp.ob_stepover = false;
        self.set_sr(
            "hv_vcpu_set_sys_reg(MDSCR_EL1)",
            sr::MDSCR_EL1,
            self.sp.ob_mdscr | (1 << 15) | (1 << 13),
        );
        self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) & !(1u64 << 21));
        self.set_sr("hv_vcpu_set_sys_reg(DBGBCR0_EL1)", sr::DBGBCR0_EL1, BCR_ON);
    }

    /// The spin statistics lines of hvm.c's print_fp.
    pub fn spin_stats(&self, tag: &str) -> Vec<String> {
        let s = &self.sp;
        let mut v = Vec::new();
        if s.kicks != 0 {
            v.push(format!(
                "{tag}: spin kicks {}, looks {}, loops resolved {}, not a loop {}, look steps {}, write faults {}, debug-register accesses emulated {}",
                s.kicks, s.checks, s.resolved, s.notcycle, s.steps, s.writes, s.dbgreg
            ));
        }
        if s.rw_on {
            v.push(format!(
                "{tag}: counter loops proven {}, resolved {}, rewinds {}; checkpoints {}, copy-on-write faults {}",
                s.aff_proven, s.aff_resolved, s.rewinds, s.ck_takes, s.ck_faults
            ));
        }
        v
    }
}
