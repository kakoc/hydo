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
//!
//! A place stops the guest once per run at most, so the cost is the scan of guest RAM at the
//! run's start and one stop per place that ran.

use hydo_contracts::paths::{brk_word, page_hash, Hits, PathPage, BRK_IMM, PAGE};
use hydo_contracts::plan::Plan;
use std::collections::HashMap;

struct Site {
    place: usize,
    page: u32,
    off: u32,
    word: u32,
}

/// One address of a place in guest memory (an intermediate physical address).
struct Inst {
    site: usize,
    pa: u64,
    /// The breakpoint is in memory now.
    live: bool,
}

/// The error paths of a run.
pub struct Ep {
    ids: Vec<u32>,
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
    /// What the run cost: the scans' wall time (s), the pages found, the stops.
    pub scan_s: f64,
    pub found: usize,
    pub stops: u64,
}

const BRK: u32 = 0xd420_0000 | (BRK_IMM << 5);

fn rd32(ram: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(ram.get(o..o + 4)?.try_into().ok()?))
}

impl Ep {
    /// The places of a plan's arm64 programs (None: no place).
    pub fn from_plan(p: &Plan) -> Option<Ep> {
        let mut ep = Ep {
            ids: Vec::new(),
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
            for pl in &prog.paths {
                let k = ep.ids.len();
                ep.ids.push(pl.id);
                for s in &pl.sites {
                    ep.sites.push(Site {
                        place: k,
                        page: base + s.page,
                        off: s.off,
                        word: s.word,
                    });
                }
            }
        }
        if ep.ids.is_empty() {
            return None;
        }
        ep.hit = vec![None; ep.ids.len()];
        ep.of_place = vec![Vec::new(); ep.ids.len()];
        Some(ep)
    }

    /// A new run: no place ran yet, and guest memory is searched again before the guest runs.
    pub fn begin(&mut self) {
        self.active = true;
        self.scanned = false;
        self.inval = true;
        self.boot = 0;
        self.stops = 0;
        self.scan_s = 0.0;
        self.hit.iter_mut().for_each(|h| *h = None);
        self.order.clear();
    }

    /// A power cut or a reboot: the next boot, from a snapshot of guest memory.
    pub fn cut(&mut self) {
        self.boot += 1;
        self.inval = true;
        for i in &mut self.inst {
            i.live = false;
        }
    }

    /// The run's hits, for the records; the run ends.
    pub fn finish(&mut self) -> Hits {
        self.active = false;
        let hits = self
            .order
            .iter()
            .filter_map(|&k| self.hit[k].map(|(at, boot)| (self.ids[k], at, boot)))
            .collect();
        let unwatched = (0..self.ids.len())
            .filter(|&k| self.of_place[k].is_empty())
            .map(|k| self.ids[k])
            .collect();
        Hits {
            hits,
            unwatched,
            stops: self.stops,
            scan_us: (self.scan_s * 1e6) as u64,
        }
    }

    /// Find the program's pages in guest RAM (`ram` from `ram_base`): each place's addresses.
    fn scan(&mut self, ram: &[u8], ram_base: u64) {
        let t0 = std::time::Instant::now();
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
        self.inst.clear();
        self.by_pa.clear();
        self.of_place.iter_mut().for_each(Vec::clear);
        for (si, s) in self.sites.iter().enumerate() {
            for &pg in &at[s.page as usize] {
                let pa = pg + s.off as u64;
                let o = (pa - ram_base) as usize;
                if rd32(ram, o) != Some(s.word) {
                    continue;
                }
                self.by_pa.insert(pa, self.inst.len());
                self.of_place[s.place].push(self.inst.len());
                self.inst.push(Inst {
                    site: si,
                    pa,
                    live: false,
                });
            }
        }
        self.found = at.iter().filter(|v| !v.is_empty()).count();
        self.scanned = true;
        self.scan_s += t0.elapsed().as_secs_f64();
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
        for i in &mut self.inst {
            let s = &self.sites[i.site];
            if self.hit[s.place].is_some() || i.live {
                continue;
            }
            let o = (i.pa - ram_base) as usize;
            if rd32(ram, o) != Some(s.word) {
                continue;
            }
            ram[o..o + 4].copy_from_slice(&BRK.to_le_bytes());
            i.live = true;
            if flush {
                inval(ram[o..].as_mut_ptr());
            }
        }
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
                ram[o..o + 4].copy_from_slice(&self.sites[i.site].word.to_le_bytes());
            }
        }
    }

    /// A breakpoint instruction of ours at `pa`: the place ran. Its instructions go back at
    /// every address, and the guest goes on. False when no place is at `pa`.
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
        let place = self.sites[self.inst[k].site].place;
        self.stops += 1;
        if self.hit[place].is_none() {
            self.hit[place] = Some((at, self.boot));
            self.order.push(place);
        }
        for &j in &self.of_place[place] {
            let i = &mut self.inst[j];
            if !i.live {
                continue;
            }
            i.live = false;
            let o = (i.pa - ram_base) as usize;
            if rd32(ram, o) == Some(BRK) {
                ram[o..o + 4].copy_from_slice(&self.sites[i.site].word.to_le_bytes());
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
