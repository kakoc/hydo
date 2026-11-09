//! The deterministic machine state of hvm.c (`struct mach`, the GIC and the UART), with its exact
//! C byte layout for the fingerprints, and the host-side timing policies: tick jitter (`TJ`) and
//! the forced preemption points (`PCT`).

use crate::layout::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gic {
    pub dctlr: u32,
    pub cctlr: u32,
    pub pmr: u32,
    pub bpr: u32,
    pub enabled: [u32; NIRQ / 32],
    pub pending: [u32; NIRQ / 32],
    pub active: [u32; NIRQ / 32],
    pub cfg: [u32; NIRQ / 16],
    pub group: [u32; NIRQ / 32],
    pub prio: [u8; NIRQ],
    pub targets: [u8; NIRQ],
}

impl Default for Gic {
    fn default() -> Gic {
        Gic {
            dctlr: 0,
            cctlr: 0,
            pmr: 0,
            bpr: 0,
            enabled: [0; NIRQ / 32],
            pending: [0; NIRQ / 32],
            active: [0; NIRQ / 32],
            cfg: [0; NIRQ / 16],
            group: [0; NIRQ / 32],
            prio: [0; NIRQ],
            targets: [0; NIRQ],
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Uart {
    pub ier: u8,
    pub lcr: u8,
    pub mcr: u8,
    pub scr: u8,
    pub dll: u8,
    pub dlm: u8,
    pub fcr: u8,
}

/// `struct mach`: everything here is in the snapshot.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mach {
    /// virtual counter, CNTFRQ ticks
    pub vtime: u64,
    /// all exits taken
    pub exits: u64,
    pub cntp_ctl: u64,
    pub cntp_cval: u64,
    pub gic: Gic,
    pub uart: Uart,
    pub console_len: u64,
    /// PSCI SYSTEM_OFF/RESET
    pub halted: u32,
    /// single-stepping to the instruction where the guest unmasks IRQs
    pub stepping: u32,
    pub mdscr_saved: u64,
    pub n_wfi: u64,
    pub n_wfi_jump: u64,
    pub n_cntpct: u64,
    pub n_mmio: u64,
    pub n_hvc: u64,
    pub n_sysreg_other: u64,
    pub n_irq_line: u64,
    pub n_steps: u64,
    pub n_step_runs: u64,
}

/// sizeof(struct mach) on arm64 macOS.
pub const MACH_SIZE: usize = 328;

impl Mach {
    /// The struct's bytes as the C compiler lays it out (the one padding byte, after the UART, is
    /// zero: the C state is memset and only ever copied whole).
    pub fn bytes(&self) -> [u8; MACH_SIZE] {
        let mut b = [0u8; MACH_SIZE];
        let mut o = 0;
        let put = |b: &mut [u8; MACH_SIZE], o: &mut usize, v: &[u8]| {
            b[*o..*o + v.len()].copy_from_slice(v);
            *o += v.len();
        };
        for v in [self.vtime, self.exits, self.cntp_ctl, self.cntp_cval] {
            put(&mut b, &mut o, &v.to_le_bytes());
        }
        let g = &self.gic;
        for v in [g.dctlr, g.cctlr, g.pmr, g.bpr] {
            put(&mut b, &mut o, &v.to_le_bytes());
        }
        for a in [
            &g.enabled[..],
            &g.pending[..],
            &g.active[..],
            &g.cfg[..],
            &g.group[..],
        ] {
            for v in a {
                put(&mut b, &mut o, &v.to_le_bytes());
            }
        }
        put(&mut b, &mut o, &g.prio);
        put(&mut b, &mut o, &g.targets);
        let u = &self.uart;
        put(
            &mut b,
            &mut o,
            &[u.ier, u.lcr, u.mcr, u.scr, u.dll, u.dlm, u.fcr],
        );
        o += 1; // padding
        put(&mut b, &mut o, &self.console_len.to_le_bytes());
        put(&mut b, &mut o, &self.halted.to_le_bytes());
        put(&mut b, &mut o, &self.stepping.to_le_bytes());
        for v in [
            self.mdscr_saved,
            self.n_wfi,
            self.n_wfi_jump,
            self.n_cntpct,
            self.n_mmio,
            self.n_hvc,
            self.n_sysreg_other,
            self.n_irq_line,
            self.n_steps,
            self.n_step_runs,
        ] {
            put(&mut b, &mut o, &v.to_le_bytes());
        }
        debug_assert_eq!(o, MACH_SIZE);
        b
    }

    /// The machine state minus the pure statistics counters (what the fingerprints hash).
    pub fn without_stats(&self) -> Mach {
        Mach {
            n_wfi: 0,
            n_wfi_jump: 0,
            n_cntpct: 0,
            n_mmio: 0,
            n_hvc: 0,
            n_sysreg_other: 0,
            n_irq_line: 0,
            n_steps: 0,
            n_step_runs: 0,
            ..*self
        }
    }
}

pub fn bit(a: &[u32], i: usize) -> bool {
    (a[i / 32] >> (i % 32)) & 1 != 0
}
pub fn setb(a: &mut [u32], i: usize, v: bool) {
    if v {
        a[i / 32] |= 1 << (i % 32);
    } else {
        a[i / 32] &= !(1u32 << (i % 32));
    }
}

/// The splitmix64 finalizer, after adding the golden ratio (hvx's tick-jitter and fault mix).
pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}
pub const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;

/// Seeded tick jitter (`struct tjit`), counted in exits; host-side, in no snapshot.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tjit {
    pub on: bool,
    pub seed: u64,
    pub permille: u32,
    pub max_delay: u32,
    pub lo: u64,
    pub hi: u64,
    pub n_explicit: usize,
    pub ek: [u64; TJ_MAX_EXPLICIT],
    pub ed: [u32; TJ_MAX_EXPLICIT],
    /// statistics: delays that were actually taken
    pub applied: u64,
    pub last_applied_k: u64,
}

impl Tjit {
    /// `tj_delay(k)`: in exits, capped inside the tick period.
    pub fn delay(&self, k: u64, step: u64) -> u64 {
        if !self.on || k <= 1 {
            return 0;
        }
        let mut d: u64 = 0;
        if self.n_explicit > 0 {
            for i in 0..self.n_explicit {
                if self.ek[i] == k {
                    d = self.ed[i] as u64;
                }
            }
        } else {
            if k < self.lo || (self.hi != 0 && k >= self.hi) {
                return 0;
            }
            let h = mix(self.seed.wrapping_add(k.wrapping_mul(GOLDEN)));
            if h % 1000 >= self.permille as u64 {
                return 0;
            }
            d = 1 + (h >> 16) % self.max_delay as u64;
        }
        let cap = (TJ_PERIOD / step).wrapping_sub(1);
        d.min(cap)
    }
}

/// PCT-style forced preemption points (`struct pctp`), counted in exits; host-side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pct {
    pub on: bool,
    pub n: u32,
    pub next: u32,
    pub pt: [u64; PCT_MAX],
    pub fired: u64,
    pub missed: u64,
}

impl Default for Pct {
    fn default() -> Pct {
        Pct {
            on: false,
            n: 0,
            next: 0,
            pt: [0; PCT_MAX],
            fired: 0,
            missed: 0,
        }
    }
}

impl Pct {
    /// `pct_set(seed, n, lo, hi)`.
    pub fn set(seed: u64, n: u32, lo: u64, hi: u64) -> Pct {
        let mut p = Pct::default();
        if n == 0 || hi <= lo {
            return p;
        }
        let n = n.min(PCT_MAX as u32);
        for i in 0..n {
            let x = mix(seed.wrapping_add((i as u64 + 1).wrapping_mul(GOLDEN)));
            p.pt[i as usize] = lo + x % (hi - lo);
        }
        for i in 1..n as usize {
            let mut j = i;
            while j > 0 && p.pt[j - 1] > p.pt[j] {
                p.pt.swap(j, j - 1);
                j -= 1;
            }
        }
        p.n = n;
        p.on = true;
        p
    }
}

/// The host tokens hvx alone applies (the PMU clock, vCPU turns, the k-th-flush cut; `rb=`
/// outside a lock-step run): the first one's refusal, as the C engine words it. The Mac and KVM
/// engines refuse such a schedule with it; `hydo_engine::refusal` says it before any VM.
pub fn host_token_problem(s: &hydo_schedule::Schedule) -> Option<String> {
    use hydo_schedule::HostToken;
    for t in &s.host {
        let hvx_only = matches!(
            t,
            HostToken::Rb(_)
                | HostToken::Pp(_)
                | HostToken::Pptsc(_)
                | HostToken::Xt(_)
                | HostToken::Xtr(_)
                | HostToken::Xtpm(_)
                | HostToken::Xtlo(_)
                | HostToken::Xthi(_)
                | HostToken::Xtsc(_)
                | HostToken::Xtx(_)
                | HostToken::Cf(_)
                | HostToken::Cfn
                | HostToken::Q(_)
                | HostToken::Qs(_)
                | HostToken::Qb { .. }
                | HostToken::Qbs(_)
                | HostToken::Qm(_)
        );
        if hvx_only {
            let w = t.to_string();
            let w = &w[..w.len().min(60)];
            let mut msg = format!(
                "host token {w} needs hvx (the PMU clock, vCPU turns or a power cut at a disk FLUSH)"
            );
            msg.truncate(199);
            return Some(msg);
        }
    }
    None
}

/// What one forced preemption point did (`pct=`), in [`PctRec`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PctResult {
    /// The timer was armed and not yet due: the clock moved to its deadline, so the guest's
    /// timer interrupt comes at the engine's next interrupt decision (counted in `pctf=`).
    Moved,
    /// The timer was armed and already due: nothing moved (counted in `pctf=`).
    Due,
    /// No timer was armed: the point did nothing (not counted in `pctf=`).
    Missed,
    /// The point's exit had passed when the points were set: it did nothing.
    Passed,
}

impl PctResult {
    pub fn word(self) -> &'static str {
        match self {
            PctResult::Moved => "moved",
            PctResult::Due => "due",
            PctResult::Missed => "missed",
            PctResult::Passed => "passed",
        }
    }
}

/// The guest's own state at an exit, read from the vCPU's registers without running guest code
/// (the preemption record's "where" and "who").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Where {
    /// The instruction pointer (RIP, PC).
    pub pc: u64,
    /// The guest ran its kernel (CPL 0, EL1) rather than a program (CPL 3, EL0).
    pub kernel: bool,
    /// The root of the page tables (CR3; TTBR0_EL1): the process.
    pub page_table: u64,
    /// The user thread pointer (the FS base; TPIDR_EL0): the thread, also while the kernel runs
    /// for it (a kernel thread has the last program's value or 0).
    pub thread_pointer: u64,
    /// The stack pointer (RSP; at EL1 SP_EL0, which Linux keeps as the current task).
    pub sp: u64,
}

/// One forced preemption point of a run as it fired: the record `HYDO_PREEMPT_RECORD=1` writes
/// to `<tag>.preempt.jsonl` in the run directory (Milestone 208; the KVM and Mac engines). The
/// engine only reads registers, so the guest and the fingerprints are as without it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PctRec {
    /// The point's index among the run's points, in exit order (0 = the first).
    pub i: u32,
    /// The engine's exit count at the point (the clock of `pctk=` and of the stops' `at`).
    pub exit: u64,
    pub result: PctResult,
    /// The guest's clock (ns) at the point and, for [`PctResult::Moved`], where it moved to.
    pub vtime: u64,
    pub to: u64,
    pub at: Where,
}

impl PctRec {
    /// The record as one JSON line (keys in this order, numbers as numbers, addresses as hex
    /// text), without the newline.
    pub fn line(&self) -> String {
        format!(
            "{{\"v\":1,\"type\":\"preempt\",\"i\":{},\"exit\":{},\"result\":\"{}\",\"vtime_ns\":{},\"to_ns\":{},\"pc\":\"{:#x}\",\"mode\":\"{}\",\"page_table\":\"{:#x}\",\"thread_pointer\":\"{:#x}\",\"sp\":\"{:#x}\"}}",
            self.i,
            self.exit,
            self.result.word(),
            self.vtime,
            self.to,
            self.at.pc,
            if self.at.kernel { "kernel" } else { "user" },
            self.at.page_table,
            self.at.thread_pointer,
            self.at.sp
        )
    }
}

impl Pct {
    /// Whether a point is due at exit count `exits` (the engines read the timer and the
    /// registers only then).
    pub fn due(&self, exits: u64) -> bool {
        self.on && self.next < self.n && self.pt[self.next as usize] <= exits
    }

    /// Fire every point due at exit count `exits` (called once per counted exit, when
    /// [`Pct::due`]): `timer` is (armed, deadline) of the guest's timer, `vtime` the guest's
    /// clock, which a point moves to the deadline. With `log`, each point's record is pushed
    /// with the guest's state `at`.
    pub fn at_exit(
        &mut self,
        exits: u64,
        timer: (bool, u64),
        vtime: &mut u64,
        log: &mut Option<Vec<PctRec>>,
        at: Where,
    ) {
        if !self.on {
            return;
        }
        while self.next < self.n && self.pt[self.next as usize] <= exits {
            let p = self.pt[self.next as usize];
            let before = *vtime;
            let result = if p != exits {
                PctResult::Passed
            } else {
                let (armed, due) = timer;
                if armed && due > *vtime {
                    *vtime = due;
                    self.fired += 1;
                    PctResult::Moved
                } else if !armed {
                    self.missed += 1;
                    PctResult::Missed
                } else {
                    self.fired += 1;
                    PctResult::Due
                }
            };
            if let Some(l) = log.as_mut() {
                l.push(PctRec {
                    i: self.next,
                    exit: p,
                    result,
                    vtime: before,
                    to: if result == PctResult::Moved {
                        *vtime
                    } else {
                        0
                    },
                    at,
                });
            }
            self.next += 1;
        }
    }
}

/// The records of one run (None: not recording).
pub type PctLog = Option<Vec<PctRec>>;

/// The file a run's preemption records go to in the engine's output directory.
pub fn preempt_file(outdir: &str, tag: &str) -> String {
    format!("{outdir}/{tag}.preempt.jsonl")
}

/// Write a run's records; none recorded gives an empty file, so a reader can tell "recorded,
/// and no point fired" from "not recorded".
pub fn write_preempt(outdir: &str, tag: &str, log: &[PctRec]) {
    let mut t = String::new();
    for r in log {
        t.push_str(&r.line());
        t.push('\n');
    }
    let _ = std::fs::write(preempt_file(outdir, tag), t);
}

#[cfg(test)]
mod pct_tests {
    use super::*;

    #[test]
    fn the_record_does_not_change_what_a_point_does() {
        // the same points with and without a log: the same clock moves and counts
        let at = Where {
            pc: 0x401000,
            kernel: false,
            page_table: 0x1000,
            thread_pointer: 0x7f00,
            sp: 0x7ffc,
        };
        let run = |log: &mut Option<Vec<PctRec>>| {
            let mut p = Pct::set(0xec69_9251_106e_8e0c, 40, 100, 3100);
            let mut vtime = 0u64;
            let mut moves = Vec::new();
            for x in 100..3200u64 {
                vtime += 2500;
                let armed = x % 7 != 0;
                if p.due(x) {
                    p.at_exit(x, (armed, vtime + 1_000_000), &mut vtime, log, at);
                }
                moves.push(vtime);
            }
            (p.fired, p.missed, moves)
        };
        let mut log = Some(Vec::new());
        assert_eq!(run(&mut None), run(&mut log));
        let log = log.unwrap();
        assert_eq!(log.len(), 40);
        assert!(log
            .windows(2)
            .all(|w| w[0].exit <= w[1].exit && w[0].i + 1 == w[1].i));
        let l = log[0].line();
        assert!(
            l.starts_with("{\"v\":1,\"type\":\"preempt\",\"i\":0,"),
            "{l}"
        );
        assert!(l.contains("\"pc\":\"0x401000\",\"mode\":\"user\""), "{l}");
        // 39 points are the first 39 of the 40 (the seeded draws do not depend on n)
        let p39 = Pct::set(0xec69_9251_106e_8e0c, 39, 100, 3100);
        let p40 = Pct::set(0xec69_9251_106e_8e0c, 40, 100, 3100);
        let mut a: Vec<u64> = p39.pt[..39].to_vec();
        let b: Vec<u64> = p40.pt[..40].to_vec();
        for x in &a {
            assert!(b.contains(x));
        }
        a.sort();
        assert_eq!(a.len() + 1, b.len());
    }
}
