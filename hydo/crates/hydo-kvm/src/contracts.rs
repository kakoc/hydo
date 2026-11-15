//! Function contracts on the KVM engine (Milestone 255, docs/design/contracts.md): hardware
//! breakpoints at the entry of each contracted function and at the return address of each call
//! in progress, the plan's reads at each, and a record per call, in the Mac engine's format.
//!
//! What the guest sees: nothing. The breakpoints are the debug registers DR0 to DR3, set with
//! `KVM_SET_GUEST_DEBUG` (`KVM_GUESTDBG_USE_HW_BP`), which KVM keeps apart from the guest's own
//! debug registers. A hit is a `KVM_EXIT_DEBUG` that the engine handles without counting an
//! exit, so the guest's clock does not move; the vCPU resumes over the instruction with
//! RFLAGS.RF, which the CPU clears when the instruction completes. The engine only reads guest
//! registers and memory, so a run with contracts has the fingerprint of the same run without
//! them. Without a plan nothing of this exists, and the vCPU's debugging stays off.
//!
//! The breakpoints are in the vCPU only while the spin observer neither steps it nor walks it
//! with single steps (those use the debug registers themselves); a call that the observer steps
//! into, by a jump from a loop without exits, is not recorded.
//!
//! A position-independent program (Rust's musl target on x86_64) is loaded where the guest's
//! kernel chooses, so the plan's addresses are offsets. At the first exit in each address space
//! (CR3) the engine looks for the program's ELF header in the process's mapped pages (the
//! program's startup reads its own header, so the page is mapped); the header's address is the
//! load address. The search runs again in an address space without the program every
//! [`RESCAN`] exits. Every process of the program has its own address, and with four debug
//! registers the engine stops in at most three of them at once: those where calls were recorded
//! first, then the ones seen last.
//!
//! A return is matched with its call by the return address (read from the stack at entry), the
//! page tables (CR3), and the stack pointer (Rust, C: 8 more at the return) or the goroutine
//! (Go: r14).

use crate::vmm::{translate, Vmm};
use hydo_contracts::plan::{self, Frame, Loc, Plan, Record};

/// The debug registers.
pub const SLOTS: usize = 4;
/// Exits between two searches for a position-independent program in an address space without
/// it.
pub const RESCAN: u64 = 256;

struct CtFn {
    id: u32,
    prog: usize,
    entry: u64,
    code: Vec<u8>,
    frame: Frame,
    at_entry: Vec<Loc>,
    at_return: Vec<Loc>,
}

struct CtProg {
    pie: bool,
    head: Vec<u8>,
}

#[derive(Clone)]
struct Pending {
    rec: usize,
    fid: usize,
    ret: u64,
    cr3: u64,
    key: u64,
}

/// Where a position-independent program is in one address space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Base {
    cr3: u64,
    prog: usize,
    /// None: the program was not found (at exit `seen`).
    at: Option<u64>,
    /// The exit count when the address space was last seen.
    seen: u64,
    /// Calls were recorded in it.
    hot: bool,
}

/// What a rewind of the rewind mode takes back ([`Ct::mark`]).
#[derive(Clone)]
pub struct Mark {
    nrecs: usize,
    pending: Vec<Pending>,
    bases: Vec<Base>,
    foreign: u64,
    over: u64,
}

/// The contracts of a run.
pub struct Ct {
    fns: Vec<CtFn>,
    progs: Vec<CtProg>,
    /// Recording this run (from the run's start or resume to its end).
    pub active: bool,
    pending: Vec<Pending>,
    bases: Vec<Base>,
    /// The address space at the last exit.
    last_cr3: Option<u64>,
    /// The breakpoints now: (address, function or None for a return address).
    armed: Vec<(u64, Option<usize>)>,
    /// The last hit (pc, CR3, stack pointer), and the hit to step over without a record: the
    /// same call again, after a stop came between the hit and its instruction.
    last: Option<(u64, u64, u64)>,
    skip: Option<(u64, u64, u64)>,
    pub recs: Vec<Record>,
    /// Hits at an entry address in another program; calls past [`plan::MAX_RECORDS`].
    pub foreign: u64,
    pub over: u64,
}

impl Ct {
    /// The x86_64 programs of a plan; an error when one program has more functions than the
    /// breakpoints can hold.
    pub fn from_plan(p: &Plan) -> Result<Ct, String> {
        let mut fns = Vec::new();
        let mut progs = Vec::new();
        for prog in p.programs.iter().filter(|p| p.arch == "x86_64") {
            let k = progs.len();
            progs.push(CtProg {
                pie: prog.pie,
                head: prog.head_bytes(),
            });
            if prog.pie && progs[k].head.len() < 16 {
                return Err(format!(
                    "{}: a position-independent program without its ELF header in the plan \
                     (build the image again with this hydo)",
                    prog.program
                ));
            }
            for f in &prog.functions {
                fns.push(CtFn {
                    id: f.id,
                    prog: k,
                    entry: f.entry,
                    code: f.code_bytes(),
                    frame: f.frame,
                    at_entry: f.at_entry.clone(),
                    at_return: f.at_return.clone(),
                });
            }
        }
        let has_ret = fns.iter().any(|f| !f.at_return.is_empty());
        let room = if has_ret { SLOTS - 1 } else { SLOTS };
        let fixed = fns.iter().filter(|f| !progs[f.prog].pie).count();
        let widest = (0..progs.len())
            .filter(|&k| progs[k].pie)
            .map(|k| fns.iter().filter(|f| f.prog == k).count())
            .max()
            .unwrap_or(0);
        if fixed + widest > room {
            return Err(format!(
                "the KVM engine has {SLOTS} breakpoints for contracts: at most {room} functions \
                 with contracts{} (this plan has {})",
                if has_ret {
                    " (one breakpoint stays for returns)"
                } else {
                    ""
                },
                fixed + widest
            ));
        }
        Ok(Ct {
            fns,
            progs,
            active: false,
            pending: Vec::new(),
            bases: Vec::new(),
            last_cr3: None,
            armed: Vec::new(),
            last: None,
            skip: None,
            recs: Vec::new(),
            foreign: 0,
            over: 0,
        })
    }

    /// A new run: no records, no calls in progress.
    pub fn begin(&mut self) {
        self.active = true;
        self.recs.clear();
        self.pending.clear();
        self.skip = None;
        self.foreign = 0;
        self.over = 0;
        self.last_cr3 = None;
        self.last = None;
        // a run from a snapshot has other address spaces than the run before it
        self.bases.clear();
    }

    /// What the run's contracts did, for the engine's log: the calls recorded and not, the hits
    /// in other programs, and the address spaces where a position-independent program was found.
    pub fn summary(&self) -> String {
        let found = self.bases.iter().filter(|b| b.at.is_some()).count();
        let mut at: Vec<u64> = self.bases.iter().filter_map(|b| b.at).collect();
        at.sort_unstable();
        at.dedup();
        format!(
            "hydo-kvm: contracts: {} calls recorded, {} not recorded, {} hits in other programs, \
             {found} address spaces with a position-independent program at {} addresses",
            self.recs.len(),
            self.over,
            self.foreign,
            at.len()
        )
    }

    /// A power cut: the calls in progress never return (the recovery boot is a new machine),
    /// and the address spaces are gone.
    pub fn cut(&mut self) {
        for p in self.pending.drain(..) {
            self.recs[p.rec].lost = Some("power cut".into());
        }
        self.bases.clear();
        self.last_cr3 = None;
        self.skip = None;
    }

    /// The run ended: write `<tag>.contracts.jsonl` into `outdir` (calls in progress have no
    /// return reads).
    pub fn finish(&mut self, outdir: &std::path::Path, tag: &str) {
        if !self.active {
            return;
        }
        self.active = false;
        let p = outdir.join(format!("{tag}{}", plan::RECORDS_SUFFIX));
        let _ = std::fs::write(p, plan::records_text(&self.recs, self.over));
        self.recs.clear();
        self.pending.clear();
    }

    /// Whether a program needs the search for its load address.
    fn any_pie(&self) -> bool {
        self.progs.iter().any(|p| p.pie)
    }

    /// The state the rewind mode's checkpoint keeps.
    pub fn mark(&self) -> Mark {
        Mark {
            nrecs: self.recs.len(),
            pending: self.pending.clone(),
            bases: self.bases.clone(),
            foreign: self.foreign,
            over: self.over,
        }
    }

    /// Back to a checkpoint: the calls after it are recorded again by the replay.
    pub fn rewind(&mut self, m: &Mark) {
        self.recs.truncate(m.nrecs);
        for p in &m.pending {
            if let Some(r) = self.recs.get_mut(p.rec) {
                r.at_return = None;
                r.ret = None;
                r.lost = None;
            }
        }
        self.pending = m.pending.clone();
        self.bases = m.bases.clone();
        self.foreign = m.foreign;
        self.over = m.over;
        self.last_cr3 = None;
        self.skip = None;
    }

    /// The breakpoints the vCPU should have: the entries of the programs at fixed addresses,
    /// the entries in the address spaces chosen, then the return addresses of calls in
    /// progress, newest first.
    fn want(&mut self) -> Vec<(u64, Option<usize>)> {
        let mut out: Vec<(u64, Option<usize>)> = Vec::new();
        for (i, f) in self.fns.iter().enumerate() {
            if !self.progs[f.prog].pie {
                out.push((f.entry, Some(i)));
            }
        }
        let has_ret = self.fns.iter().any(|f| !f.at_return.is_empty());
        let room = if has_ret { SLOTS - 1 } else { SLOTS };
        let mut found: Vec<&Base> = self.bases.iter().filter(|b| b.at.is_some()).collect();
        // where calls were recorded, then the address spaces seen last
        found.sort_by_key(|b| (!b.hot, u64::MAX - b.seen));
        for b in found {
            let base = b.at.unwrap_or(0);
            let add: Vec<(u64, Option<usize>)> = self
                .fns
                .iter()
                .enumerate()
                .filter(|(_, f)| f.prog == b.prog)
                .map(|(i, f)| (base.wrapping_add(f.entry), Some(i)))
                .filter(|a| !out.iter().any(|o| o.0 == a.0))
                .collect();
            if out.len() + add.len() > room {
                continue;
            }
            out.extend(add);
        }
        for p in self.pending.iter().rev() {
            if out.len() >= SLOTS {
                break;
            }
            if !out.iter().any(|o| o.0 == p.ret) {
                out.push((p.ret, None));
            }
        }
        out
    }
}

/// `n` bytes (1 to 8) of guest memory at a virtual address through the page tables at `cr3`,
/// little-endian.
fn read_va(ram: &[u8], cr3: u64, va: u64, n: u8) -> Option<u64> {
    let mut v = 0u64;
    let mut i = 0u64;
    while i < n as u64 {
        let a = va.wrapping_add(i);
        let o = translate(ram, cr3, a)? as usize;
        // the rest of the read within this page
        let k = ((n as u64 - i).min(0x1000 - (a & 0xfff))) as usize;
        let b = ram.get(o..o + k)?;
        for (j, x) in b.iter().enumerate() {
            v |= (*x as u64) << (8 * (i as usize + j));
        }
        i += k as u64;
    }
    Some(v)
}

/// Whether guest memory at `va` holds `bytes`.
fn same_bytes(ram: &[u8], cr3: u64, va: u64, bytes: &[u8]) -> bool {
    bytes.iter().enumerate().all(|(i, b)| {
        translate(ram, cr3, va.wrapping_add(i as u64))
            .and_then(|o| ram.get(o as usize))
            .is_some_and(|x| x == b)
    })
}

/// The user-half virtual address of a 4 KiB page that starts with `head`, in the page tables at
/// `cr3`: where a position-independent program's ELF header is mapped.
fn find_head(ram: &[u8], cr3: u64, head: &[u8]) -> Option<u64> {
    const ADDR: u64 = 0x000f_ffff_ffff_f000;
    let len = ram.len() as u64;
    let rd = |pa: u64| -> Option<u64> {
        let o = crate::layout::ram_offset(len, pa, 8)?;
        ram.get(o..o + 8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
    };
    let page = |pa: u64| -> Option<&[u8]> {
        let o = crate::layout::ram_offset(len, pa, 4096)?;
        ram.get(o..o + 4096)
    };
    let mut budget = 1u32 << 20;
    let l4 = cr3 & ADDR;
    // the user half: PML4 entries 0 to 255
    for i4 in 0..256u64 {
        let e4 = rd(l4 + i4 * 8)?;
        if e4 & 1 == 0 {
            continue;
        }
        for i3 in 0..512u64 {
            let e3 = rd((e4 & ADDR) + i3 * 8)?;
            if e3 & 1 == 0 || e3 & 0x80 != 0 {
                continue;
            }
            for i2 in 0..512u64 {
                let e2 = rd((e3 & ADDR) + i2 * 8)?;
                if e2 & 1 == 0 || e2 & 0x80 != 0 {
                    continue;
                }
                for i1 in 0..512u64 {
                    budget = budget.checked_sub(1)?;
                    let e1 = rd((e2 & ADDR) + i1 * 8)?;
                    if e1 & 1 == 0 {
                        continue;
                    }
                    if page(e1 & ADDR).is_some_and(|p| p.starts_with(head)) {
                        return Some((i4 << 39) | (i3 << 30) | (i2 << 21) | (i1 << 12));
                    }
                }
            }
        }
    }
    None
}

/// A register's value by the plan's (DWARF's) number: 0 rax, 1 rdx, 2 rcx, 3 rbx, 4 rsi, 5 rdi,
/// 6 rbp, 7 rsp, 8 to 15 r8 to r15.
fn dwarf_reg(r: &crate::abi::kvm_regs, n: u8) -> Option<u64> {
    Some(match n {
        0 => r.rax,
        1 => r.rdx,
        2 => r.rcx,
        3 => r.rbx,
        4 => r.rsi,
        5 => r.rdi,
        6 => r.rbp,
        7 => r.rsp,
        8 => r.r8,
        9 => r.r9,
        10 => r.r10,
        11 => r.r11,
        12 => r.r12,
        13 => r.r13,
        14 => r.r14,
        15 => r.r15,
        _ => return None,
    })
}

impl Vmm {
    /// Put the breakpoints the contracts want into the vCPU that runs next (none when no run
    /// records).
    pub(crate) fn ct_arm(&mut self) {
        let Some(ct) = self.ct.as_mut() else { return };
        let want = if ct.active { ct.want() } else { Vec::new() };
        ct.armed = want;
        let addrs: Vec<u64> = ct.armed.iter().map(|a| a.0).collect();
        self.cpu_mut().set_contract_bps(&addrs);
    }

    /// At an exit: in a new address space, where a position-independent program is.
    pub(crate) fn ct_at_exit(&mut self) {
        let Some(ct) = self.ct.as_ref() else { return };
        if !ct.active || !ct.any_pie() {
            return;
        }
        let cr3 = self.cpu().cr3() & !0xfff;
        let exits = self.m.exits;
        let Some(ct) = self.ct.as_mut() else { return };
        let fresh = ct.last_cr3 != Some(cr3);
        ct.last_cr3 = Some(cr3);
        let ram = self.ram.bytes();
        for k in 0..ct.progs.len() {
            if !ct.progs[k].pie {
                continue;
            }
            let head = &ct.progs[k].head;
            let i = ct.bases.iter().position(|b| b.cr3 == cr3 && b.prog == k);
            let look = match i.map(|i| ct.bases[i]) {
                // a known address, still the program's (page tables are reused)
                Some(b) if b.at.is_some() => {
                    if fresh && !same_bytes(ram, cr3, b.at.unwrap_or(0), head) {
                        true
                    } else {
                        if let Some(i) = i {
                            ct.bases[i].seen = exits;
                        }
                        false
                    }
                }
                Some(b) => exits >= b.seen + RESCAN,
                None => true,
            };
            if !look {
                continue;
            }
            let at = find_head(ram, cr3, head);
            let b = Base {
                cr3,
                prog: k,
                at,
                seen: exits,
                hot: false,
            };
            match i {
                Some(i) => ct.bases[i] = b,
                None => ct.bases.push(b),
            }
        }
        // a bounded table: the address spaces seen last
        if ct.bases.len() > 64 {
            ct.bases.sort_by_key(|b| u64::MAX - b.seen);
            ct.bases.truncate(48);
        }
    }

    /// A contract's breakpoint at `pc`: a return of a call in progress, an entry, or both; then
    /// the vCPU goes on over the instruction.
    pub(crate) fn ct_hit(&mut self, pc: u64) {
        let r = self.cpu().regs();
        let cr3 = self.cpu().cr3() & !0xfff;
        let exits = self.m.exits;
        let here = Some((pc, cr3, r.rsp));
        let skip = self.ct.as_mut().and_then(|c| {
            c.last = here;
            c.skip.take()
        });
        if skip != here {
            self.ct_return(pc, &r, cr3, exits);
            self.ct_entry(pc, &r, cr3, exits);
        }
        self.cpu_mut().contract_resume(pc);
    }

    /// The vCPU stops before the instruction a contract's stop resumed over: the guest's RFLAGS
    /// is its own again, and the hit that comes again when the guest goes on is the same call.
    pub(crate) fn ct_settle(&mut self) {
        if self.ct.is_some() && self.cpu_mut().contract_settle() {
            if let Some(ct) = self.ct.as_mut() {
                ct.skip = ct.last;
            }
        }
    }

    fn ct_load(
        &self,
        l: &Loc,
        r: &crate::abi::kvm_regs,
        cr3: u64,
        entry: &[Option<u64>],
    ) -> Option<u64> {
        let ram = self.ram.bytes();
        l.load(&|n| dwarf_reg(r, n), entry, &|va, n| {
            read_va(ram, cr3, va, n)
        })
    }

    fn ct_return(&mut self, pc: u64, r: &crate::abi::kvm_regs, cr3: u64, exits: u64) {
        let Some(ct) = self.ct.as_ref() else { return };
        let Some(i) = ct.pending.iter().rposition(|p| {
            p.ret == pc
                && p.cr3 == cr3
                && p.key
                    == match ct.fns[p.fid].frame {
                        Frame::Sp => r.rsp,
                        Frame::G => r.r14,
                    }
        }) else {
            return;
        };
        let (rec, fid) = (ct.pending[i].rec, ct.pending[i].fid);
        let entry = ct.recs[rec].at_entry.clone();
        let out: Vec<Option<u64>> = ct.fns[fid]
            .at_return
            .iter()
            .map(|l| self.ct_load(l, r, cr3, &entry))
            .collect();
        let Some(ct) = self.ct.as_mut() else { return };
        ct.pending.remove(i);
        ct.recs[rec].at_return = Some(out);
        ct.recs[rec].ret = Some(exits);
    }

    fn ct_entry(&mut self, pc: u64, r: &crate::abi::kvm_regs, cr3: u64, exits: u64) {
        let Some(ct) = self.ct.as_ref() else { return };
        let fids: Vec<usize> = ct
            .armed
            .iter()
            .filter(|a| a.0 == pc)
            .filter_map(|a| a.1)
            .collect();
        if fids.is_empty() {
            return;
        }
        let ram = self.ram.bytes();
        let mut any = false;
        for fid in fids {
            let Some(ct) = self.ct.as_ref() else { return };
            let f = &ct.fns[fid];
            if !same_bytes(ram, cr3, pc, &f.code) {
                continue;
            }
            any = true;
            let at_entry: Vec<Option<u64>> = f
                .at_entry
                .iter()
                .map(|l| self.ct_load(l, r, cr3, &[]))
                .collect();
            let (frame, has_ret, id, prog) = (f.frame, !f.at_return.is_empty(), f.id, f.prog);
            // at the entry the return address is on the top of the stack; the return pops it
            let ret = read_va(ram, cr3, r.rsp, 8);
            let key = match frame {
                Frame::Sp => r.rsp.wrapping_add(8),
                Frame::G => r.r14,
            };
            let Some(ct) = self.ct.as_mut() else { return };
            if let Some(b) = ct
                .bases
                .iter_mut()
                .find(|b| b.cr3 == cr3 && b.prog == prog && b.at.is_some())
            {
                b.hot = true;
            }
            if ct.recs.len() >= plan::MAX_RECORDS {
                ct.over += 1;
                continue;
            }
            ct.recs.push(Record {
                f: id,
                at: exits,
                at_entry,
                ..Record::default()
            });
            let rec = ct.recs.len() - 1;
            if !has_ret {
                continue;
            }
            let Some(ret) = ret else {
                ct.recs[rec].lost = Some("unreadable stack".into());
                continue;
            };
            // a breakpoint for the return: one already there, else a free debug register
            let returns: Vec<u64> = {
                let mut v: Vec<u64> = ct.pending.iter().map(|p| p.ret).collect();
                v.sort_unstable();
                v.dedup();
                v
            };
            let entries = ct.armed.iter().filter(|a| a.1.is_some()).count();
            if !returns.contains(&ret) && entries + returns.len() >= SLOTS {
                ct.recs[rec].lost = Some("no breakpoint".into());
                continue;
            }
            ct.pending.push(Pending {
                rec,
                fid,
                ret,
                cr3,
                key,
            });
        }
        if !any {
            if let Some(ct) = self.ct.as_mut() {
                ct.foreign += 1;
            }
        }
    }
}
