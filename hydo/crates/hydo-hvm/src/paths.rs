//! Error paths on the Mac engine (Milestone 306, docs/design/error-paths.md): which of a
//! program's error-handling places ran in a run, with one-shot breakpoint instructions.
//!
//! A plan can list hundreds of places, and the vCPU has 6 breakpoint registers, so the engine
//! writes a breakpoint instruction (`BRK #0x4859`) over each place's first instruction in guest
//! memory. It finds the instruction where the guest's kernel keeps the program's file: the root
//! is in guest RAM, so each page of the file is a page of RAM, which the engine finds by its
//! first nonzero 8 bytes and the hash of its 4,096 bytes ([`hydo_contracts::paths::PathPage`]).
//! Every process of the program runs from those pages.
//!
//! What the guest sees: nothing.
//! - The breakpoints are in memory only while the guest runs (`run_until`); at every stop the
//!   engine puts the instructions back first, so a snapshot, a fingerprint and every host command
//!   see the guest's own memory.
//! - A breakpoint is a debug exception that Hypervisor.framework sends to the engine (the
//!   engine traps debug exceptions while the contracts are armed). The engine puts the place's
//!   instruction back at all of its addresses, records the place's first hit, and lets the guest
//!   run the instruction. It counts no exit, so the guest's clock does not move.
//! - A breakpoint instruction with another immediate is the guest's own: the engine delivers it
//!   to the guest's kernel as the CPU would have.
//! - The guest can also read the instructions as data: a program that reads its own file reads
//!   the page that its kernel keeps of the file. Since Milestone 394 the engine maps each page
//!   that holds a breakpoint execute-only while the guest runs. A read or a write of it is a
//!   data abort that comes to the engine: the page's instructions go back for the rest of the
//!   boot, the page is mapped as before, and the guest does the access again. A Go program with
//!   protobuf hashes eight samples of its own binary at its start, and before this a restarted
//!   Blnk got another hash, and the run another fingerprint.
//!
//! Since Milestone 394 a plan can also list every function of the program (`fn_places`, the live
//! map, docs/design/live-map.md): each is a place at its first instruction, watched the same way,
//! and written on a line of its own.
//!
//! A place stops the guest once per run at most, so the cost is the scan of guest RAM at the
//! run's start and one stop per place that ran.

use hydo_contracts::paths::{brk_word, page_hash, Hits, PathPage, BRK_IMM, PAGE};
use hydo_contracts::plan::Plan;
use std::collections::HashMap;

/// One instruction of the program's file that holds a breakpoint: the places whose first
/// instruction it is (a recovery function's error path and the function's own place, since
/// Milestone 394, share one).
struct Site {
    places: Vec<usize>,
    page: u32,
    off: u32,
    word: u32,
}

/// One address in guest memory (an intermediate physical address) that holds a breakpoint, and
/// the places whose first instruction is there.
struct Inst {
    pa: u64,
    word: u32,
    places: Vec<usize>,
    /// The breakpoint is in memory now.
    live: bool,
    /// The guest read or wrote the instruction's page (Milestone 394): the instruction is the
    /// guest's for the rest of the boot.
    dropped: bool,
}

/// The error paths of a run, and since Milestone 394 every function (the live map): both are
/// places, watched the same way, and written on lines of their own.
pub struct Ep {
    ids: Vec<u32>,
    /// The place is a function's (`fn_places`), not an error path.
    is_fn: Vec<bool>,
    sites: Vec<Site>,
    pages: Vec<PathPage>,
    /// (probe offset, probe) -> the pages with it
    probes: HashMap<(u32, u64), Vec<u32>>,
    offs: Vec<u32>,
    inst: Vec<Inst>,
    by_pa: HashMap<u64, usize>,
    of_place: Vec<Vec<usize>>,
    hit: Vec<Option<(u64, u32)>>,
    order: Vec<usize>,
    pub active: bool,
    /// The guest's own instructions are back for a snapshot at a stop
    /// ([`crate::vmm::Vmm::ct_park`]).
    pub parked: bool,
    scanned: bool,
    /// The next arming invalidates the instruction cache at each breakpoint.
    inval: bool,
    pub boot: u32,
    /// What the run cost: the scans' wall time (s), the pages found, the stops at an error
    /// path and at a function.
    pub scan_s: f64,
    pub found: usize,
    pub stops: u64,
    pub fn_stops: u64,
    has_paths: bool,
    has_fns: bool,
    /// The addresses of each page that the last full search found (Milestone 394). A run
    /// starts from the same snapshot as the run before it on this engine, so the next search
    /// first checks those addresses by the pages' hashes, and searches all of guest RAM only
    /// when one of them changed: the first read of every page of guest RAM costs about half a
    /// second for 1.5 GiB, because guest RAM is mapped from its RAM base file (Milestone 335).
    last: Option<Vec<Vec<u64>>>,
    /// Map the pages that hold breakpoints execute-only (Milestone 394), so that a read of the
    /// program's file, which the guest's kernel serves from those pages, faults to the engine
    /// first ([`Ep::read_fault`]). Off in the rewind mode, which tracks writes with the pages'
    /// permissions itself.
    pub protect: bool,
    /// The engine's option `HVM_BRK_PROTECT` (on, but off by default on macOS 27): without it no
    /// page is made execute-only, and a program that reads its own file can read a breakpoint.
    pub protect_allowed: bool,
    /// The host pages (16 KiB) mapped execute-only now.
    prot: std::collections::HashSet<u64>,
    /// The places whose instructions went back because the guest read their page before they
    /// ran, and how many such reads came.
    dropped: Vec<bool>,
    pub reads: u64,
}

/// The engine's page (Hypervisor.framework maps guest memory in 16 KiB pages).
const HOST_PG: u64 = crate::layout::PG;

const BRK: u32 = 0xd420_0000 | (BRK_IMM << 5);

fn rd32(ram: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(ram.get(o..o + 4)?.try_into().ok()?))
}

impl Ep {
    /// The places of a plan's arm64 programs: the error paths and the functions (None: no
    /// place).
    pub fn from_plan(p: &Plan) -> Option<Ep> {
        let mut ep = Ep {
            ids: Vec::new(),
            is_fn: Vec::new(),
            sites: Vec::new(),
            pages: Vec::new(),
            probes: HashMap::new(),
            offs: Vec::new(),
            inst: Vec::new(),
            by_pa: HashMap::new(),
            of_place: Vec::new(),
            hit: Vec::new(),
            order: Vec::new(),
            active: false,
            parked: false,
            scanned: false,
            inval: true,
            boot: 0,
            scan_s: 0.0,
            found: 0,
            stops: 0,
            fn_stops: 0,
            has_paths: false,
            has_fns: false,
            last: None,
            protect: true,
            protect_allowed: true,
            prot: std::collections::HashSet::new(),
            dropped: Vec::new(),
            reads: 0,
        };
        debug_assert_eq!(brk_word(BRK_IMM), BRK);
        for prog in p.programs.iter().filter(|p| p.arch == "arm64" && !p.pie) {
            let base = ep.pages.len() as u32;
            for pg in &prog.path_pages {
                ep.probes
                    .entry((pg.probe_off, pg.probe))
                    .or_default()
                    .push(ep.pages.len() as u32);
                if !ep.offs.contains(&pg.probe_off) {
                    ep.offs.push(pg.probe_off);
                }
                ep.pages.push(pg.clone());
            }
            let mut at: HashMap<(u32, u32), usize> = HashMap::new();
            let lists = [(&prog.paths, false), (&prog.fn_places, true)];
            for (list, is_fn) in lists {
                for pl in list.iter() {
                    let k = ep.ids.len();
                    ep.ids.push(pl.id);
                    ep.is_fn.push(is_fn);
                    ep.has_fns |= is_fn;
                    ep.has_paths |= !is_fn;
                    for s in &pl.sites {
                        let key = (base + s.page, s.off);
                        match at.get(&key) {
                            Some(&si) => ep.sites[si].places.push(k),
                            None => {
                                at.insert(key, ep.sites.len());
                                ep.sites.push(Site {
                                    places: vec![k],
                                    page: base + s.page,
                                    off: s.off,
                                    word: s.word,
                                });
                            }
                        }
                    }
                }
            }
        }
        if ep.ids.is_empty() {
            return None;
        }
        ep.hit = vec![None; ep.ids.len()];
        ep.of_place = vec![Vec::new(); ep.ids.len()];
        ep.dropped = vec![false; ep.ids.len()];
        Some(ep)
    }

    /// A new run: no place ran yet, and guest memory is searched again before the guest runs.
    pub fn begin(&mut self) {
        self.unprotect_all();
        self.dropped.iter_mut().for_each(|d| *d = false);
        self.reads = 0;
        self.active = true;
        self.scanned = false;
        self.inval = true;
        self.boot = 0;
        self.stops = 0;
        self.fn_stops = 0;
        self.scan_s = 0.0;
        self.hit.iter_mut().for_each(|h| *h = None);
        self.order.clear();
    }

    /// A power cut or a reboot: the next boot, from a snapshot of guest memory.
    pub fn cut(&mut self) {
        self.unprotect_all();
        self.boot += 1;
        self.inval = true;
        for i in &mut self.inst {
            i.live = false;
            i.dropped = false;
        }
    }

    /// Every page mapped as it was before the breakpoints.
    fn unprotect_all(&mut self) {
        for pg in self.prot.drain() {
            crate::hv::vm_protect(pg, HOST_PG, crate::hv::RWX);
        }
    }

    fn hits_of(&self, fns: bool, stops: u64) -> Hits {
        let hits = self
            .order
            .iter()
            .filter(|&&k| self.is_fn[k] == fns)
            .filter_map(|&k| self.hit[k].map(|(at, boot)| (self.ids[k], at, boot)))
            .collect();
        let unwatched = (0..self.ids.len())
            .filter(|&k| self.is_fn[k] == fns && self.of_place[k].is_empty())
            .map(|k| self.ids[k])
            .collect();
        let dropped = (0..self.ids.len())
            .filter(|&k| self.is_fn[k] == fns && self.dropped[k] && self.hit[k].is_none())
            .map(|k| self.ids[k])
            .collect();
        Hits {
            hits,
            unwatched,
            dropped,
            reads: self.reads,
            stops,
            scan_us: (self.scan_s * 1e6) as u64,
        }
    }

    /// The run's hits, for the records: the error paths' (None for a plan without them) and the
    /// functions' (None for a plan without them); the run ends.
    pub fn finish(&mut self) -> (Option<Hits>, Option<Hits>) {
        self.unprotect_all();
        self.active = false;
        (
            self.has_paths.then(|| self.hits_of(false, self.stops)),
            self.has_fns.then(|| self.hits_of(true, self.fn_stops)),
        )
    }

    /// Find the program's pages in guest RAM (`ram` from `ram_base`): each place's addresses.
    fn scan(&mut self, ram: &[u8], ram_base: u64) {
        let t0 = std::time::Instant::now();
        // the addresses of the search before, when it found every page and each is still there
        let kept = self.last.take().filter(|last| {
            last.iter().all(|v| !v.is_empty())
                && last.iter().enumerate().all(|(i, v)| {
                    v.iter().all(|&pa| {
                        let o = (pa - ram_base) as usize;
                        ram.get(o..o + PAGE)
                            .is_some_and(|b| page_hash(b) == self.pages[i].hash)
                    })
                })
        });
        let at = match kept {
            Some(at) => at,
            None => self.search(ram, ram_base),
        };
        self.place(&at, ram, ram_base);
        self.found = at.iter().filter(|v| !v.is_empty()).count();
        self.last = Some(at);
        self.scanned = true;
        self.scan_s += t0.elapsed().as_secs_f64();
    }

    /// Search all of guest RAM for the program's pages: the addresses of each.
    fn search(&self, ram: &[u8], ram_base: u64) -> Vec<Vec<u64>> {
        let mut at: Vec<Vec<u64>> = vec![Vec::new(); self.pages.len()];
        for (pi, page) in ram.chunks_exact(PAGE).enumerate() {
            let mut hash = None;
            for &off in &self.offs {
                let o = off as usize;
                let w = u64::from_le_bytes(page[o..o + 8].try_into().expect("8 bytes"));
                if w == 0 {
                    continue;
                }
                let Some(cands) = self.probes.get(&(off, w)) else {
                    continue;
                };
                let h = *hash.get_or_insert_with(|| page_hash(page));
                for &c in cands {
                    if self.pages[c as usize].hash == h {
                        at[c as usize].push(ram_base + (pi * PAGE) as u64);
                    }
                }
            }
        }
        at
    }

    /// Each place's instructions at the pages' addresses `at`.
    fn place(&mut self, at: &[Vec<u64>], ram: &[u8], ram_base: u64) {
        self.inst.clear();
        self.by_pa.clear();
        self.of_place.iter_mut().for_each(Vec::clear);
        for s in &self.sites {
            for &pg in &at[s.page as usize] {
                let pa = pg + s.off as u64;
                let o = (pa - ram_base) as usize;
                if rd32(ram, o) != Some(s.word) {
                    continue;
                }
                // two pages of the file with the same bytes are found at the same address: one
                // instruction there, with the places of both
                let k = match self.by_pa.get(&pa) {
                    Some(&k) => k,
                    None => {
                        self.by_pa.insert(pa, self.inst.len());
                        self.inst.push(Inst {
                            pa,
                            word: s.word,
                            places: Vec::new(),
                            live: false,
                            dropped: false,
                        });
                        self.inst.len() - 1
                    }
                };
                for &p in &s.places {
                    if !self.inst[k].places.contains(&p) {
                        self.inst[k].places.push(p);
                        self.of_place[p].push(k);
                    }
                }
            }
        }
    }

    /// Whether every place of an instruction ran (its breakpoint is not needed any more).
    fn done(&self, k: usize) -> bool {
        self.inst[k].places.iter().all(|&p| self.hit[p].is_some())
    }

    /// Before the guest runs: a breakpoint over each place that did not run yet. `ram` is guest
    /// RAM from `ram_base`; `inval` is called with each changed word's host address.
    pub fn arm(&mut self, ram: &mut [u8], ram_base: u64, inval: &dyn Fn(*mut u8)) {
        if !self.active {
            return;
        }
        if !self.scanned {
            self.scan(ram, ram_base);
        }
        let flush = std::mem::take(&mut self.inval);
        for k in 0..self.inst.len() {
            if self.inst[k].live || self.inst[k].dropped || self.done(k) {
                continue;
            }
            let i = &mut self.inst[k];
            let o = (i.pa - ram_base) as usize;
            if rd32(ram, o) != Some(i.word) {
                continue;
            }
            ram[o..o + 4].copy_from_slice(&BRK.to_le_bytes());
            i.live = true;
            if flush {
                inval(ram[o..].as_mut_ptr());
            }
            let pg = i.pa & !(HOST_PG - 1);
            if self.protect && self.protect_allowed && self.prot.insert(pg) {
                crate::hv::vm_protect(pg, HOST_PG, crate::hv::HV_MEMORY_EXEC);
            }
        }
    }

    /// A data abort at `pa` (Milestone 394): when it is in a page mapped execute-only for the
    /// breakpoints, the guest read or wrote that page, for example its kernel serving a read of
    /// the program's file. Every instruction of the page goes back for the rest of the boot, the
    /// page is mapped as before, and the guest does the access again, so it reads its own
    /// bytes. A place that had not run yet then counts as dropped. False for another address.
    pub fn read_fault(
        &mut self,
        pa: u64,
        ram: &mut [u8],
        ram_base: u64,
        inval: &dyn Fn(*mut u8),
    ) -> bool {
        let pg = pa & !(HOST_PG - 1);
        if !self.prot.remove(&pg) {
            return false;
        }
        self.reads += 1;
        for k in 0..self.inst.len() {
            if self.inst[k].pa & !(HOST_PG - 1) != pg {
                continue;
            }
            let i = &mut self.inst[k];
            i.dropped = true;
            if i.live {
                i.live = false;
                let o = (i.pa - ram_base) as usize;
                if rd32(ram, o) == Some(BRK) {
                    ram[o..o + 4].copy_from_slice(&i.word.to_le_bytes());
                    inval(ram[o..].as_mut_ptr());
                }
            }
            for &p in &self.inst[k].places {
                self.dropped[p] = true;
            }
        }
        crate::hv::vm_protect(pg, HOST_PG, crate::hv::RWX);
        true
    }

    /// At a stop: the guest's own instructions back.
    pub fn disarm(&mut self, ram: &mut [u8], ram_base: u64) {
        for i in &mut self.inst {
            if !i.live {
                continue;
            }
            i.live = false;
            let o = (i.pa - ram_base) as usize;
            if rd32(ram, o) == Some(BRK) {
                ram[o..o + 4].copy_from_slice(&i.word.to_le_bytes());
            }
        }
    }

    /// A breakpoint instruction of ours at `pa`: its places ran. Their instructions go back at
    /// every address whose places all ran, and the guest goes on. False when no place is at
    /// `pa`.
    pub fn hit(
        &mut self,
        pa: u64,
        at: u64,
        ram: &mut [u8],
        ram_base: u64,
        inval: &dyn Fn(*mut u8),
    ) -> bool {
        let Some(&k) = self.by_pa.get(&pa) else {
            return false;
        };
        let places = self.inst[k].places.clone();
        if places.iter().any(|&p| self.is_fn[p]) {
            self.fn_stops += 1;
        } else {
            self.stops += 1;
        }
        let mut new = Vec::new();
        for &p in &places {
            if self.hit[p].is_none() {
                self.hit[p] = Some((at, self.boot));
                self.order.push(p);
                new.push(p);
            }
        }
        let mut back: Vec<usize> = vec![k];
        for &p in &new {
            back.extend(self.of_place[p].iter().copied());
        }
        back.sort_unstable();
        back.dedup();
        for j in back {
            if !self.inst[j].live || !self.done(j) {
                continue;
            }
            let i = &mut self.inst[j];
            i.live = false;
            let o = (i.pa - ram_base) as usize;
            if rd32(ram, o) == Some(BRK) {
                ram[o..o + 4].copy_from_slice(&i.word.to_le_bytes());
                inval(ram[o..].as_mut_ptr());
            }
        }
        true
    }

    /// The places, and how many of them ran so far.
    pub fn counts(&self) -> (usize, usize) {
        (self.ids.len(), self.order.len())
    }
}

/// Whether an exception syndrome is a `BRK` with the engine's immediate.
pub fn is_ours(syndrome: u64) -> bool {
    (syndrome >> 26) & 0x3f == 0x3c && syndrome & 0xffff == BRK_IMM as u64
}
