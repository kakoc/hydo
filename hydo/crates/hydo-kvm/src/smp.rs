//! Several vCPUs (docs/design/kvm-vcpus.md): the state of a machine with more than one vCPU that
//! the engine adds to [`crate::machine::Mach`], and the rules that need no vCPU. The engine runs
//! the vCPUs one at a time from one thread, in turns of a number of counted exits; this module
//! holds whose turn it is, how long a turn is, each vCPU's local APIC ([`crate::apic`]) and run
//! state, and the MSR model of the vCPUs other than vCPU 0 (vCPU 0's stays in `Mach`, as on one
//! vCPU). A machine with one vCPU has none of it, so its snapshots and fingerprints are the ones
//! of before.
//!
//! The turn length is `q` exits, or with a seed `qs` a length in `[1, q]` for each turn drawn as
//! hvx's in-kernel scheduler draws it (`hvx_sched_len`: `1 + mix(seed + (t + 1) * GOLDEN) % q`
//! for turn `t`). The policy (`q`, `qs`) is the schedule's, like tick jitter: it is in the
//! snapshot but not in the fingerprint.

use crate::apic::Apic;
use crate::msr::Msrs;
use hydo_hvm::machine::{mix, GOLDEN};

/// The turn length when the schedule names none (exits).
pub const Q_DEFAULT: u64 = 256;
/// The most vCPUs the engine runs (the SMP guest kernel's `CONFIG_NR_CPUS`).
pub const MAX_VCPUS: usize = 8;

/// What a vCPU does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    /// runs when its turn comes
    Ready,
    /// executed HLT with interrupts enabled: runs again when an interrupt is deliverable to it
    Halted,
    /// executed HLT with interrupts disabled: nothing the engine emulates wakes it
    Stopped,
    /// waits for a STARTUP IPI (every vCPU but vCPU 0 at reset, and after an INIT)
    WaitSipi,
}

impl Run {
    fn byte(self) -> u8 {
        match self {
            Run::Ready => 0,
            Run::Halted => 1,
            Run::Stopped => 2,
            Run::WaitSipi => 3,
        }
    }
}

/// The machine's several-vCPU state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Smp {
    pub apic: Vec<Apic>,
    pub run: Vec<Run>,
    /// the vCPU has run since the boot (an INIT to such a vCPU is a CPU hot-unplug, which the
    /// engine does not emulate)
    pub started: Vec<bool>,
    /// the MSR model of each vCPU; entry 0 is unused (vCPU 0's is `Mach::msrs`)
    pub msrs: Vec<Msrs>,
    /// whose turn it is
    pub cur: usize,
    /// counted exits left in the turn
    pub left: u64,
    /// turns ended since the boot
    pub turns: u64,
    /// the turn length (the schedule's `q=`; policy, not hashed)
    pub q: u64,
    /// the turn-length seed (`qs=`; 0: every turn is `q` long; policy, not hashed)
    pub qs: u64,
    /// the turn ends at the next loop top (a HLT, a spin-wait exit, a proven spin, or `left` 0)
    pub end: bool,
    /// statistics, not hashed: each vCPU's counted exits
    pub exits: Vec<u64>,
}

/// Where an IPI goes, as vCPU indexes.
pub fn targets(dest: crate::apic::Dest, from: usize, n: usize) -> Vec<usize> {
    use crate::apic::Dest;
    match dest {
        Dest::Id(i) => (i as usize)
            .lt(&n)
            .then_some(i as usize)
            .into_iter()
            .collect(),
        Dest::SelfOnly => vec![from],
        Dest::All => (0..n).collect(),
        Dest::AllButSelf => (0..n).filter(|&i| i != from).collect(),
    }
}

impl Smp {
    /// The state at reset of a machine with `n` vCPUs: vCPU 0 runs, the others wait for STARTUP.
    pub fn new(n: usize) -> Smp {
        let n = n.clamp(2, MAX_VCPUS);
        let mut run = vec![Run::WaitSipi; n];
        run[0] = Run::Ready;
        let mut started = vec![false; n];
        started[0] = true;
        Smp {
            apic: (0..n as u32).map(Apic::new).collect(),
            run,
            started,
            msrs: vec![Msrs::default(); n],
            cur: 0,
            left: Q_DEFAULT,
            turns: 0,
            q: Q_DEFAULT,
            qs: 0,
            end: false,
            exits: vec![0; n],
        }
    }

    pub fn n(&self) -> usize {
        self.run.len()
    }

    /// The length of turn `t` under the policy.
    pub fn len_of(&self, t: u64) -> u64 {
        let q = self.q.max(1);
        if self.qs == 0 {
            q
        } else {
            1 + mix(self.qs.wrapping_add(t.wrapping_add(1).wrapping_mul(GOLDEN))) % q
        }
    }

    /// Set the turn policy; the turn in progress keeps what is left of it.
    pub fn set_policy(&mut self, q: u64, qs: u64) {
        self.q = q.max(1);
        self.qs = qs;
    }

    /// One counted exit of the running vCPU: the turn ends when it has made its exits.
    pub fn count(&mut self) {
        self.exits[self.cur] += 1;
        self.left = self.left.saturating_sub(1);
        if self.left == 0 {
            self.end = true;
        }
    }

    /// Hand the turn to vCPU `j` (it may be the one that ran): a new turn.
    pub fn switch_to(&mut self, j: usize) {
        if self.end || j != self.cur {
            self.turns += 1;
        }
        self.end = false;
        self.cur = j;
        self.left = self.len_of(self.turns);
        self.started[j] = true;
        if self.run[j] == Run::Halted {
            self.run[j] = Run::Ready;
        }
    }

    /// The canonical bytes for snapshots' checks and fingerprints: every vCPU's APIC, run state
    /// and MSR model, then whose turn it is, what is left of it, and the turn count. The policy
    /// is left out.
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(self.n() as u32).to_le_bytes());
        for i in 0..self.n() {
            b.extend_from_slice(&self.apic[i].bytes());
            b.push(self.run[i].byte());
            b.push(self.started[i] as u8);
            if i > 0 {
                b.extend_from_slice(&self.msrs[i].bytes());
            }
        }
        for v in [self.cur as u64, self.left, self.turns] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.push(self.end as u8);
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apic::Dest;

    #[test]
    fn the_reset_state() {
        let s = Smp::new(3);
        assert_eq!(s.run, vec![Run::Ready, Run::WaitSipi, Run::WaitSipi]);
        assert_eq!((s.cur, s.left, s.n()), (0, Q_DEFAULT, 3));
        assert_eq!(s.apic[2].id, 2);
        assert_eq!(
            Smp::new(1).n(),
            2,
            "a machine of this module has several vCPUs"
        );
        assert_eq!(Smp::new(99).n(), MAX_VCPUS);
    }

    #[test]
    fn turns_are_counted_in_exits_and_seeded_lengths_are_hvx_s() {
        let mut s = Smp::new(2);
        s.set_policy(3, 0);
        s.switch_to(0);
        for _ in 0..2 {
            s.count();
            assert!(!s.end);
        }
        s.count();
        assert!(s.end);
        s.switch_to(1);
        // the first switch_to(0) began no new turn (nothing had ended); this one is the first
        assert_eq!((s.cur, s.left, s.end, s.turns), (1, 3, false, 1));
        // seeded: every length in [1, q], the same sequence every time, not all equal
        let mut a = Smp::new(2);
        a.set_policy(100, 0x1f);
        let l: Vec<u64> = (0..50).map(|t| a.len_of(t)).collect();
        assert!(l.iter().all(|&x| (1..=100).contains(&x)));
        assert_eq!(l, (0..50).map(|t| a.len_of(t)).collect::<Vec<_>>());
        assert!(l.windows(2).any(|w| w[0] != w[1]));
        // hvx's rule: 1 + splitmix64(seed + (t+1) * GOLDEN) % q
        assert_eq!(
            a.len_of(4),
            1 + mix(0x1f_u64.wrapping_add(5_u64.wrapping_mul(GOLDEN))) % 100
        );
    }

    #[test]
    fn ipi_targets() {
        assert_eq!(targets(Dest::Id(1), 0, 2), vec![1]);
        assert_eq!(
            targets(Dest::Id(5), 0, 2),
            Vec::<usize>::new(),
            "no such vCPU"
        );
        assert_eq!(targets(Dest::SelfOnly, 1, 4), vec![1]);
        assert_eq!(targets(Dest::All, 1, 3), vec![0, 1, 2]);
        assert_eq!(targets(Dest::AllButSelf, 1, 3), vec![0, 2]);
    }

    #[test]
    fn the_policy_is_not_in_the_bytes() {
        let a = Smp::new(2);
        let mut b = a.clone();
        b.set_policy(17, 9);
        assert_eq!(a.bytes(), b.bytes());
        b.switch_to(1);
        assert_ne!(a.bytes(), b.bytes());
        let mut c = a.clone();
        c.apic[1].post(0x40);
        assert_ne!(a.bytes(), c.bytes());
    }
}
