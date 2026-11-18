//! Function contracts on the Mac engine (Milestone 241, docs/design/contracts.md): hardware
//! breakpoints at the entry of each contracted function and at the return address of each call
//! in progress, the reads of the plan at each, and a record per call.
//!
//! What the guest sees: nothing. The breakpoints are the debug registers DBGBVR1..5 (the spin
//! observer keeps the first pair), matched at EL0 only; a hit and the single step over the
//! instruction are debug exceptions that Hypervisor.framework sends to this engine, which
//! handles them without counting an exit, so the guest's clock does not move. MDSCR_EL1's MDE
//! bit, which the breakpoints need, is set only while the guest runs, and a vCPU snapshot or a
//! fingerprint takes the guest's own value ([`Vmm::ct_mask`]). The engine only reads guest
//! registers and memory, so a run with contracts has the fingerprint of the same run without
//! them.
//!
//! A hit at a function's entry whose code bytes are not the plan's is another program's code at
//! the same address (static programs share their link address): it is stepped over without a
//! record. The return of a call is matched by the return address, the page tables (TTBR0_EL1)
//! and the stack pointer at entry (Rust, C) or the goroutine (Go: x28).

use crate::hv::{sr, HV_REG_CPSR, HV_REG_PC, HV_REG_X0};
use crate::layout::RAM_BASE;
use crate::vmm::Vmm;
use hydo_contracts::plan::{self, Frame, Loc, Plan, Record};

/// The breakpoint register pairs the contracts use (pair 0 is the spin observer's).
const SLOTS: [usize; 5] = [1, 2, 3, 4, 5];
/// DBGBCR: enabled, every byte of the word, EL0 only (PMC 0b10).
const BCR_EL0: u64 = (0xf << 5) | (2 << 1) | 1;
/// MDSCR_EL1.MDE (monitor debug events: breakpoints) and .SS (software step).
const MDE: u64 = 1 << 15;
const SS: u64 = 1;
/// PSTATE.SS in CPSR.
const PSTATE_SS: u64 = 1 << 21;

fn bvr(n: usize) -> u16 {
    sr::DBGBVR0_EL1 + 8 * n as u16
}
fn bcr(n: usize) -> u16 {
    sr::DBGBCR0_EL1 + 8 * n as u16
}

struct CtFn {
    id: u32,
    entry: u64,
    code: Vec<u8>,
    frame: Frame,
    at_entry: Vec<Loc>,
    at_return: Vec<Loc>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Free,
    Entry,
    /// A return address, with the number of calls waiting for it.
    Return(u32),
}

struct Pending {
    rec: usize,
    fid: usize,
    ret: u64,
    ttbr: u64,
    key: u64,
}

/// The contracts of a run.
pub struct Ct {
    fns: Vec<CtFn>,
    slot_addr: [u64; 6],
    slot_use: [Use; 6],
    /// Recording this run (from the run's start or resume to its end).
    pub active: bool,
    /// The breakpoints are in the vCPU (while the guest runs).
    armed: bool,
    /// Whether the guest's own MDSCR_EL1 had MDE set when the breakpoints went in.
    guest_mde: bool,
    /// The breakpoint address being stepped over.
    stepping: Option<u64>,
    /// A hit to step over without a record: the same call again after an interrupt came
    /// before the instruction at the breakpoint ran (pc, page tables, stack pointer).
    skip: Option<(u64, u64, u64)>,
    pending: Vec<Pending>,
    pub recs: Vec<Record>,
    /// Hits at an entry address in another program; calls past [`plan::MAX_RECORDS`].
    pub foreign: u64,
    pub over: u64,
    /// The boot the calls are in: each power cut or reboot starts the next (Milestone 285).
    pub boot: u32,
    /// The error paths the run watches (Milestone 306, [`crate::paths`]).
    pub ep: Option<crate::paths::Ep>,
}

impl Ct {
    /// The arm64 programs of a plan; an error when it has more functions than breakpoints.
    pub fn from_plan(p: &Plan) -> Result<Ct, String> {
        let mut fns = Vec::new();
        for prog in p.programs.iter().filter(|p| p.arch == "arm64") {
            if prog.pie {
                return Err(format!(
                    "{} is position-independent: the Mac engine stops at the addresses the \
                     program was linked at (build it static, as Rust's and Go's arm64 Linux \
                     targets do)",
                    prog.program
                ));
            }
            for f in &prog.functions {
                fns.push(CtFn {
                    id: f.id,
                    entry: f.entry,
                    code: f.code_bytes(),
                    frame: f.frame,
                    at_entry: f.at_entry.clone(),
                    at_return: f.at_return.clone(),
                });
            }
        }
        let mut ct = Ct {
            fns,
            slot_addr: [0; 6],
            slot_use: [Use::Free; 6],
            active: false,
            armed: false,
            guest_mde: false,
            stepping: None,
            skip: None,
            pending: Vec::new(),
            recs: Vec::new(),
            foreign: 0,
            over: 0,
            boot: 0,
            ep: crate::paths::Ep::from_plan(p),
        };
        let mut entries: Vec<u64> = ct.fns.iter().map(|f| f.entry).collect();
        entries.sort_unstable();
        entries.dedup();
        // one breakpoint per entry, and at least one left for returns
        if entries.len() + 1 > SLOTS.len() {
            return Err(format!(
                "the Mac engine has {} breakpoints for contracts: at most {} functions with \
                 contracts (one breakpoint stays for returns); this plan has {}",
                SLOTS.len(),
                SLOTS.len() - 1,
                entries.len()
            ));
        }
        for (i, a) in entries.iter().enumerate() {
            ct.slot_addr[SLOTS[i]] = *a;
            ct.slot_use[SLOTS[i]] = Use::Entry;
        }
        Ok(ct)
    }

    /// A new run: no records, no calls in progress.
    pub fn begin(&mut self) {
        self.active = true;
        self.recs.clear();
        self.pending.clear();
        self.stepping = None;
        self.skip = None;
        self.foreign = 0;
        self.over = 0;
        self.boot = 0;
        if let Some(ep) = self.ep.as_mut() {
            ep.begin();
        }
        for &n in &SLOTS {
            if matches!(self.slot_use[n], Use::Return(_)) {
                self.slot_use[n] = Use::Free;
            }
        }
    }

    /// A power cut: the calls in progress never return (the recovery boot is a new machine).
    pub fn cut(&mut self) {
        self.boot += 1;
        if let Some(ep) = self.ep.as_mut() {
            ep.cut();
        }
        for p in self.pending.drain(..) {
            self.recs[p.rec].lost = Some("power cut".into());
        }
        for &n in &SLOTS {
            if matches!(self.slot_use[n], Use::Return(_)) {
                self.slot_use[n] = Use::Free;
            }
        }
    }

    /// Whether the breakpoints are in the vCPU (the engine then traps debug exceptions).
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// The run ended: write `<tag>.contracts.jsonl` into `outdir` (calls in progress have no
    /// return reads).
    pub fn finish(&mut self, outdir: &std::path::Path, tag: &str) {
        if !self.active {
            return;
        }
        self.active = false;
        let p = outdir.join(format!("{tag}{}", plan::RECORDS_SUFFIX));
        // the error paths' line first (Milestone 306): a plan without places writes as before
        let mut text = match self.ep.as_mut() {
            Some(ep) => hydo_contracts::paths::hits_line(&ep.finish()),
            None => String::new(),
        };
        text.push_str(&plan::records_text(&self.recs, self.over));
        let _ = std::fs::write(p, text);
        self.recs.clear();
        self.pending.clear();
    }
}

impl Vmm {
    fn ct_set(&self, r: u16, v: u64) {
        self.v().set_sys("hv_vcpu_set_sys_reg(contracts)", r, v);
    }

    /// Put the breakpoints into the vCPU before the guest runs.
    pub fn ct_arm(&mut self) {
        if !self.ct.as_ref().is_some_and(|c| c.active && !c.armed) {
            return;
        }
        let m = self.v().sys(sr::MDSCR_EL1, 0);
        let Some(ct) = self.ct.as_mut() else { return };
        ct.guest_mde = m & MDE != 0;
        ct.armed = true;
        let (addr, used, stepping) = (ct.slot_addr, ct.slot_use, ct.stepping);
        self.ct_set(
            sr::MDSCR_EL1,
            m | MDE | if stepping.is_some() { SS } else { 0 },
        );
        for &n in &SLOTS {
            if used[n] != Use::Free && Some(addr[n]) != stepping {
                self.ct_set(bvr(n), addr[n]);
                self.ct_set(bcr(n), BCR_EL0);
            } else {
                self.ct_set(bcr(n), 0);
            }
        }
        // the error paths' breakpoint instructions (Milestone 306)
        if let Some(ep) = self.ct.as_mut().and_then(|c| c.ep.as_mut()) {
            ep.arm(self.ram.bytes_mut(), RAM_BASE, &|p| {
                crate::hv::icache_invalidate(p, 4)
            });
        }
    }

    /// The MDE bit the contracts add to MDSCR_EL1 while armed (0 when the guest set it
    /// itself, or when not armed): the engine's own saves of MDSCR_EL1 leave it out.
    pub fn ct_mde(&self) -> u64 {
        match &self.ct {
            Some(c) if c.armed && !c.guest_mde => MDE,
            _ => 0,
        }
    }

    /// Take the breakpoints out (at every stop), so the guest's own state is what the host's
    /// commands, the snapshots and the fingerprints see.
    pub fn ct_disarm(&mut self) {
        if !self.ct.as_ref().is_some_and(|c| c.armed) {
            return;
        }
        let mde = self.ct_mde();
        let m = self.v().sys(sr::MDSCR_EL1, 0);
        let Some(ct) = self.ct.as_mut() else { return };
        ct.armed = false;
        let mut m = m & !mde;
        if ct.stepping.take().is_some() {
            // a stop came between a hit and its step: the instruction ran (a real exit came
            // from it), so the step is done
            m &= !SS;
            self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) & !PSTATE_SS);
        }
        for &n in &SLOTS {
            self.ct_set(bcr(n), 0);
        }
        self.ct_set(sr::MDSCR_EL1, m);
        // the guest's own instructions back at every error path (Milestone 306)
        if let Some(ep) = self.ct.as_mut().and_then(|c| c.ep.as_mut()) {
            ep.disarm(self.ram.bytes_mut(), RAM_BASE);
        }
    }

    /// Before a snapshot at a stop inside `run_until` (Milestone 306): the guest's own
    /// instructions back at every error path, until [`Vmm::ct_unpark`] before the guest runs.
    pub fn ct_park(&mut self) {
        if !self.ct.as_ref().is_some_and(|c| c.armed) {
            return;
        }
        if let Some(ep) = self.ct.as_mut().and_then(|c| c.ep.as_mut()) {
            ep.disarm(self.ram.bytes_mut(), RAM_BASE);
            ep.parked = true;
        }
    }

    /// The error paths' breakpoints again after [`Vmm::ct_park`].
    pub fn ct_unpark(&mut self) {
        if let Some(ep) = self.ct.as_mut().and_then(|c| c.ep.as_mut()) {
            if std::mem::take(&mut ep.parked) {
                ep.arm(self.ram.bytes_mut(), RAM_BASE, &|p| {
                    crate::hv::icache_invalidate(p, 4)
                });
            }
        }
    }

    /// A `BRK` while the contracts are armed (EC 0x3C, Milestone 306): an error path's
    /// breakpoint (its instructions go back and the guest goes on), or the guest's own, which
    /// the guest's kernel gets as the CPU would have given it. True when handled: the guest
    /// resumes, and no exit is counted.
    pub fn ct_brk(&mut self, syndrome: u64) -> bool {
        if !self.ct.as_ref().is_some_and(|c| c.armed) {
            return false;
        }
        if crate::paths::is_ours(syndrome) {
            let pc = self.rd(HV_REG_PC);
            let exits = self.m.exits;
            if let Some(pa) = self.ct_pa(pc) {
                if let Some(ep) = self.ct.as_mut().and_then(|c| c.ep.as_mut()) {
                    if ep.hit(pa, exits, self.ram.bytes_mut(), RAM_BASE, &|p| {
                        crate::hv::icache_invalidate(p, 4)
                    }) {
                        return true;
                    }
                }
            }
        }
        self.inject_sync(syndrome)
    }

    /// The guest's own values of what the contracts change, for a vCPU snapshot: (MDSCR_EL1,
    /// CPSR) as the guest has them.
    pub fn ct_mask(&self, mdscr: u64, cpsr: u64) -> (u64, u64) {
        match &self.ct {
            Some(ct) if ct.armed => {
                let mut m = mdscr & !self.ct_mde();
                let mut c = cpsr;
                if ct.stepping.is_some() {
                    m &= !SS;
                    c &= !PSTATE_SS;
                }
                (m, c)
            }
            _ => (mdscr, cpsr),
        }
    }

    /// Whether the contracts are stepping over an instruction, or have a breakpoint at `pc`:
    /// the spin observer then leaves the guest alone (a kick can come in the middle of a step).
    pub fn ct_busy(&self, pc: u64) -> bool {
        match &self.ct {
            Some(ct) if ct.armed => {
                ct.stepping.is_some()
                    || SLOTS
                        .iter()
                        .any(|&n| ct.slot_use[n] != Use::Free && ct.slot_addr[n] == pc)
            }
            _ => false,
        }
    }

    /// Whether a debug exception (EC 0x30, 0x32, 0x33) is the contracts'. Handles it if so: the
    /// guest then resumes, and no exit is counted.
    pub fn ct_exit(&mut self, ec: u64) -> bool {
        let Some(ct) = self.ct.as_ref() else {
            return false;
        };
        if !ct.armed {
            return false;
        }
        match ec {
            0x32 | 0x33 => {
                let Some(pc) = ct.stepping else { return false };
                self.ct_step_done(pc);
                true
            }
            0x30 | 0x31 => {
                let pc = self.rd(HV_REG_PC);
                let mine = SLOTS
                    .iter()
                    .any(|&n| ct.slot_use[n] != Use::Free && ct.slot_addr[n] == pc);
                if !mine || (self.sp.ob_on && self.sp.ob_pc == pc) {
                    return false;
                }
                self.ct_hit(pc);
                true
            }
            _ => false,
        }
    }

    fn ct_step_done(&mut self, pc: u64) {
        let el0 = self.rd(HV_REG_CPSR) & 0xf == 0;
        let ttbr = self.v().sys(sr::TTBR0_EL1, 0);
        let sp_el0 = self.v().sys(sr::SP_EL0, 0);
        let elr = self.v().sys(sr::ELR_EL1, 0);
        let m = self.v().sys(sr::MDSCR_EL1, 0);
        let Some(ct) = self.ct.as_mut() else { return };
        ct.stepping = None;
        // an interrupt was taken before the instruction at the breakpoint ran: the guest comes
        // back to it, and that hit is the same call
        if !el0 && elr == pc {
            ct.skip = Some((pc, ttbr, sp_el0));
        }
        let (addr, used) = (ct.slot_addr, ct.slot_use);
        for &n in &SLOTS {
            if used[n] != Use::Free && addr[n] == pc {
                self.ct_set(bcr(n), BCR_EL0);
            }
        }
        if self.m.stepping == 0 {
            self.ct_set(sr::MDSCR_EL1, m & !SS);
            self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) & !PSTATE_SS);
        }
    }

    /// Step over the instruction at a breakpoint: its breakpoint off, one software step.
    fn ct_step_over(&mut self, pc: u64) {
        let m = self.v().sys(sr::MDSCR_EL1, 0);
        let Some(ct) = self.ct.as_mut() else { return };
        ct.stepping = Some(pc);
        let (addr, used) = (ct.slot_addr, ct.slot_use);
        for &n in &SLOTS {
            if used[n] != Use::Free && addr[n] == pc {
                self.ct_set(bcr(n), 0);
            }
        }
        self.ct_set(sr::MDSCR_EL1, m | MDE | SS);
        self.wr(HV_REG_CPSR, self.rd(HV_REG_CPSR) | PSTATE_SS);
    }

    /// A register's value at EL0 (31: the stack pointer SP_EL0).
    fn ct_reg(&self, n: u8) -> u64 {
        if n == 31 {
            self.v().sys(sr::SP_EL0, 0)
        } else if n <= 30 {
            self.rd(HV_REG_X0 + n as u32)
        } else {
            0
        }
    }

    /// A stage-1 walk of the EL0 page tables (TTBR0_EL1, 4 KiB granule).
    fn ct_pa(&self, va: u64) -> Option<u64> {
        let v = self.v();
        if v.sys(sr::SCTLR_EL1, 0) & 1 == 0 {
            return Some(va);
        }
        if va >> 63 != 0 {
            return None;
        }
        let tcr = v.sys(sr::TCR_EL1, 0);
        let tsz = (tcr & 63) as i32;
        let bits = 64 - tsz;
        let first = 4 - (bits - 12 + 8) / 9;
        let ram = self.ram.bytes();
        let rd8 = |pa: u64| -> Option<u64> {
            if pa < RAM_BASE || pa + 8 > RAM_BASE + self.ram_size {
                return None;
            }
            let o = (pa - RAM_BASE) as usize;
            Some(u64::from_le_bytes(ram[o..o + 8].try_into().ok()?))
        };
        let mut level = first;
        let mut table = v.sys(sr::TTBR0_EL1, 0) & 0x0000_ffff_ffff_fffe;
        loop {
            let shift = 12 + 9 * (3 - level);
            let m = if level == first {
                (1u64 << (bits - shift)).wrapping_sub(1)
            } else {
                511
            };
            let idx = (va >> shift) & m;
            let d = rd8((table & !0xfff).wrapping_add(idx * 8))?;
            if d & 1 == 0 {
                return None;
            }
            if level == 3 {
                return Some((d & 0x0000_ffff_ffff_f000) | (va & 0xfff));
            }
            if d & 2 == 0 {
                let m = (1u64 << shift) - 1;
                return Some((d & 0x0000_ffff_ffff_f000 & !m) | (va & m));
            }
            table = d & 0x0000_ffff_ffff_f000;
            level += 1;
        }
    }

    /// `n` bytes (1 to 8) of EL0 memory at `va`, little-endian.
    fn ct_read(&self, va: u64, n: u8) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..n as u64 {
            let pa = self.ct_pa(va.wrapping_add(i))?;
            if pa < RAM_BASE || pa >= RAM_BASE + self.ram_size {
                return None;
            }
            v |= (self.ram.bytes()[(pa - RAM_BASE) as usize] as u64) << (8 * i);
        }
        Some(v)
    }

    fn ct_load(&self, l: &Loc, entry: &[Option<u64>]) -> Option<u64> {
        l.load(&|n| Some(self.ct_reg(n)), entry, &|va, n| {
            self.ct_read(va, n)
        })
    }

    fn ct_hit(&mut self, pc: u64) {
        let ttbr = self.v().sys(sr::TTBR0_EL1, 0);
        let sp_el0 = self.v().sys(sr::SP_EL0, 0);
        let exits = self.m.exits;
        let skip = self.ct.as_ref().and_then(|c| c.skip);
        if skip == Some((pc, ttbr, sp_el0)) {
            if let Some(ct) = self.ct.as_mut() {
                ct.skip = None;
            }
            self.ct_step_over(pc);
            return;
        }
        if let Some(ct) = self.ct.as_mut() {
            ct.skip = None;
        }
        self.ct_return(pc, ttbr, sp_el0, exits);
        self.ct_entry(pc, ttbr, sp_el0, exits);
        self.ct_step_over(pc);
    }

    /// A hit at a return address: the matching call in progress, if any, gets its return reads.
    fn ct_return(&mut self, pc: u64, ttbr: u64, sp_el0: u64, exits: u64) {
        let g = self.rd(HV_REG_X0 + 28);
        let Some(ct) = self.ct.as_ref() else { return };
        let Some(i) = ct.pending.iter().rposition(|p| {
            p.ret == pc
                && p.ttbr == ttbr
                && p.key
                    == match ct.fns[p.fid].frame {
                        Frame::Sp => sp_el0,
                        Frame::G => g,
                    }
        }) else {
            return;
        };
        let (rec, fid) = (ct.pending[i].rec, ct.pending[i].fid);
        let entry = ct.recs[rec].at_entry.clone();
        let out: Vec<Option<u64>> = ct.fns[fid]
            .at_return
            .iter()
            .map(|l| self.ct_load(l, &entry))
            .collect();
        let Some(ct) = self.ct.as_mut() else { return };
        ct.pending.remove(i);
        ct.recs[rec].at_return = Some(out);
        ct.recs[rec].ret = Some(exits);
        let mut freed = Vec::new();
        for &n in &SLOTS {
            if ct.slot_addr[n] == pc {
                if let Use::Return(k) = ct.slot_use[n] {
                    ct.slot_use[n] = if k <= 1 {
                        Use::Free
                    } else {
                        Use::Return(k - 1)
                    };
                    if ct.slot_use[n] == Use::Free {
                        freed.push(n);
                    }
                }
            }
        }
        for n in freed {
            self.ct_set(bcr(n), 0);
        }
    }

    /// A hit at an entry address: a call of each function there whose code is at the
    /// address in this process.
    fn ct_entry(&mut self, pc: u64, ttbr: u64, sp_el0: u64, exits: u64) {
        let Some(ct) = self.ct.as_ref() else { return };
        let fids: Vec<usize> = (0..ct.fns.len())
            .filter(|&i| ct.fns[i].entry == pc)
            .collect();
        if fids.is_empty() {
            return;
        }
        let mut any = false;
        for fid in fids {
            let Some(ct) = self.ct.as_ref() else { return };
            let f = &ct.fns[fid];
            let same = f
                .code
                .iter()
                .enumerate()
                .all(|(i, b)| self.ct_read(pc + i as u64, 1) == Some(*b as u64));
            if !same {
                continue;
            }
            any = true;
            let at_entry: Vec<Option<u64>> =
                f.at_entry.iter().map(|l| self.ct_load(l, &[])).collect();
            let (frame, has_ret, id) = (f.frame, !f.at_return.is_empty(), f.id);
            let key = match frame {
                Frame::Sp => sp_el0,
                Frame::G => self.rd(HV_REG_X0 + 28),
            };
            let ret = self.rd(HV_REG_X0 + 30);
            let Some(ct) = self.ct.as_mut() else { return };
            if ct.recs.len() >= plan::MAX_RECORDS {
                ct.over += 1;
                continue;
            }
            ct.recs.push(Record {
                f: id,
                at: exits,
                boot: ct.boot,
                at_entry,
                ..Record::default()
            });
            let rec = ct.recs.len() - 1;
            if !has_ret {
                continue;
            }
            // the return address's breakpoint: one already there, else a free pair
            let slot = SLOTS
                .iter()
                .copied()
                .find(|&n| matches!(ct.slot_use[n], Use::Return(_)) && ct.slot_addr[n] == ret)
                .or_else(|| SLOTS.iter().copied().find(|&n| ct.slot_use[n] == Use::Free));
            let Some(n) = slot else {
                ct.recs[rec].lost = Some("no breakpoint".into());
                continue;
            };
            let fresh = ct.slot_use[n] == Use::Free;
            ct.slot_use[n] = match ct.slot_use[n] {
                Use::Return(k) => Use::Return(k + 1),
                _ => Use::Return(1),
            };
            ct.slot_addr[n] = ret;
            ct.pending.push(Pending {
                rec,
                fid,
                ret,
                ttbr,
                key,
            });
            if fresh {
                self.ct_set(bvr(n), ret);
                self.ct_set(bcr(n), BCR_EL0);
            }
        }
        if !any {
            if let Some(ct) = self.ct.as_mut() {
                ct.foreign += 1;
            }
        }
    }
}
