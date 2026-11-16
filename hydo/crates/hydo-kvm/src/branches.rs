//! The branch tick (`guest.branch_tick`, Milestone 274; docs/design/exit-free-loops.md): an
//! interrupt for guest code that makes no VM exit, at a point fixed by the guest alone.
//!
//! The engine's clock counts VM exits and delivers interrupts at exits, so a loop without exits
//! gets no timer interrupt and the guest kernel never preempts it (kine's watch loop in k3s,
//! Milestone 256: `select` on a closed channel changes the Go runtime's random state in every
//! pass, so no spin observer proves a cycle). With the branch tick on, a host performance counter
//! counts the guest's retired conditional branches in user mode (`exclude_host`,
//! `exclude_kernel`: what `perf_event_paranoid` 2 lets a user count). After the image's period of
//! such branches (`/etc/hydo/branch-tick`, 2^26 from hydo-build) of one vCPU without one of its
//! exits, the engine stops the vCPU right after the
//! branch that makes the count, and treats the point as a proven loop: the clock jumps to its
//! next event, as at an idle HLT, and the interrupt is delivered at the next entry.
//!
//! The rule needs the count of the branches since the vCPU's last exit, exact on every host: the
//! engine reads the counter after every `KVM_RUN` ([`Branches::read`]). It needs no overflow for
//! the usual exits: the watchdog's kick (no exit for `HYDO_KVM_SPIN_MS`) arms the counter's
//! overflow [`MARGIN`] branches before the tick ([`Branches::alarm`]). The overflow's signal stops
//! `KVM_RUN` some branches late (26 on a Xeon 6975P-C, Milestone 274), never early, so the engine
//! then reaches the exact count by single steps with the trap flag ([`TfStep`]): an instruction
//! that saves RFLAGS is run over or fixed, and a fault's frame has the flag taken out at the
//! handler's first instruction. A stretch that passed the tick before the engine could arm it
//! ends the run as a SPIN (invalid), never silently.
//!
//! Inert when off: no counter is opened, no read is made, and nothing in the machine changes.

/// The overflow comes this many branches before the tick; the steps cover the rest.
pub const MARGIN: u64 = 256;

/// Steps of one landing before the engine gives up (the run ends as a SPIN).
pub const MAX_STEPS: u64 = 200_000;

/// A host counter of the guest's retired conditional branches in user mode, on the engine's
/// (the vCPUs') thread.
pub trait Branches: Send {
    /// The count since the counter was opened.
    fn read(&mut self) -> u64;
    /// Some(n): the counter's overflow signal (the watchdog's kick signal) after `n` more
    /// branches; None: no overflow.
    fn alarm(&mut self, after: Option<u64>);
    /// One line naming the counter (the engine's diagnostics).
    fn describe(&self) -> String;
}

/// How the landing's trap-flag steps treat the instruction at RIP (64-bit user mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TfStep {
    /// one trap-flag step
    Plain,
    /// PUSHF: one step, then the trap flag out of the pushed word (of this many bytes)
    Pushf(u8),
    /// run to a breakpoint at the next instruction, this many bytes on, without the trap flag
    /// (SYSCALL copies RFLAGS into R11; POPF could set the flag itself)
    RunOver(u8),
    /// an instruction whose exception or return the landing cannot follow (INT, INT3, INTO, INT1,
    /// UD2, IRET, SYSENTER, SYSEXIT, SYSRET): the run ends as a SPIN
    Refuse,
}

/// The class of the instruction at the start of `code`. Unknown bytes (none available) refuse.
pub fn tf_step(code: &[u8]) -> TfStep {
    let mut i = 0usize;
    let mut opsize = false;
    while i < code.len() && i < 14 {
        match code[i] {
            0x66 => opsize = true,
            0x67 | 0xf2 | 0xf3 | 0x2e | 0x3e | 0x26 | 0x64 | 0x65 | 0x36 | 0xf0 => {}
            _ => break,
        }
        i += 1;
    }
    if i < code.len() && (0x40..=0x4f).contains(&code[i]) {
        i += 1; // REX
    }
    let Some(&op) = code.get(i) else {
        return TfStep::Refuse;
    };
    let len = (i + 1) as u8;
    match op {
        0x9c => TfStep::Pushf(if opsize { 2 } else { 8 }),
        0x9d => TfStep::RunOver(len),
        0xcc | 0xcd | 0xce | 0xcf | 0xf1 => TfStep::Refuse,
        0x0f => match code.get(i + 1) {
            Some(0x05) => TfStep::RunOver(len + 1),
            Some(0x0b) | Some(0x07) | Some(0x34) | Some(0x35) => TfStep::Refuse,
            Some(_) => TfStep::Plain,
            None => TfStep::Refuse,
        },
        _ => TfStep::Plain,
    }
}

/// The exception vectors whose handlers get a breakpoint while the landing steps: a fault of a
/// stepped instruction pushes RFLAGS with the trap flag, and the engine takes it out of the frame
/// before the handler's first instruction runs. (#DE, #UD, #GP, #PF; with an error code: #GP and
/// #PF.)
pub const VECTORS: [(u8, bool); 4] = [(0, false), (6, false), (13, true), (14, true)];

/// The handler address of `vector` in a 64-bit IDT (`gate` the 16 bytes of its descriptor).
pub fn gate_target(gate: &[u8]) -> Option<u64> {
    if gate.len() < 16 || gate[5] & 0x80 == 0 {
        return None; // not present
    }
    let lo = u16::from_le_bytes([gate[0], gate[1]]) as u64;
    let mid = u16::from_le_bytes([gate[6], gate[7]]) as u64;
    let hi = u32::from_le_bytes([gate[8], gate[9], gate[10], gate[11]]) as u64;
    Some(lo | (mid << 16) | (hi << 32))
}

/// The branch tick's state in the engine: nothing of it is guest state (it is in no snapshot and
/// no fingerprint: every snapshot is taken at an exit, where every count since an exit is 0).
pub struct Bt {
    pub ctr: Box<dyn Branches>,
    /// the counter's value at the last read
    pub last: u64,
    /// per vCPU: its branches since its last exit (or its last tick)
    pub since: Vec<u64>,
    /// `since` of the running vCPU at the last kick (an advancing count: the tick, not the spin
    /// observer, ends the stretch)
    pub at_kick: u64,
    /// the overflow is armed
    pub armed: bool,
    pub ticks: u64,
    pub steps: u64,
    pub max_steps: u64,
    pub runovers: u64,
    pub fixed_frames: u64,
    pub landing_secs: f64,
}

impl Bt {
    pub fn new(ctr: Box<dyn Branches>, vcpus: usize) -> Bt {
        let mut b = Bt {
            ctr,
            last: 0,
            since: vec![0; vcpus.max(1)],
            at_kick: 0,
            armed: false,
            ticks: 0,
            steps: 0,
            max_steps: 0,
            runovers: 0,
            fixed_frames: 0,
            landing_secs: 0.0,
        };
        b.last = b.ctr.read();
        b
    }

    /// After a `KVM_RUN` of vCPU `cur`: its branches since its last exit.
    pub fn account(&mut self, cur: usize) -> u64 {
        let c = self.ctr.read();
        let d = c.wrapping_sub(self.last);
        self.last = c;
        if let Some(s) = self.since.get_mut(cur) {
            *s += d;
            *s
        } else {
            0
        }
    }

    /// A new stretch of vCPU `cur` (an exit, a resolved loop, a tick).
    pub fn reset(&mut self, cur: usize) {
        if let Some(s) = self.since.get_mut(cur) {
            *s = 0;
        }
        self.at_kick = 0;
        if self.armed {
            self.ctr.alarm(None);
            self.armed = false;
        }
    }

    /// Every vCPU at the start of a stretch (a restore, a rewind).
    pub fn reset_all(&mut self) {
        for s in self.since.iter_mut() {
            *s = 0;
        }
        self.last = self.ctr.read();
        self.at_kick = 0;
        if self.armed {
            self.ctr.alarm(None);
            self.armed = false;
        }
    }

    pub fn stats(&self) -> String {
        format!(
            "branch tick: {} ticks, {} steps (at most {} in one), {} run over, {} frames fixed, {:.3} s landing",
            self.ticks,
            self.steps,
            self.max_steps,
            self.runovers,
            self.fixed_frames,
            self.landing_secs
        )
    }
}

/// The counter with a fixed answer (the scripted tests): `read` gives what `set` put there.
pub struct Fixed {
    pub n: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub alarms: std::sync::Arc<std::sync::Mutex<Vec<Option<u64>>>>,
}

impl Branches for Fixed {
    fn read(&mut self) -> u64 {
        self.n.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn alarm(&mut self, after: Option<u64>) {
        if let Ok(mut a) = self.alarms.lock() {
            a.push(after);
        }
    }
    fn describe(&self) -> String {
        "a scripted counter".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_know_the_instructions_that_save_or_restore_rflags() {
        assert_eq!(tf_step(&[0x9c]), TfStep::Pushf(8));
        assert_eq!(tf_step(&[0x66, 0x9c]), TfStep::Pushf(2));
        assert_eq!(tf_step(&[0x9d]), TfStep::RunOver(1));
        assert_eq!(tf_step(&[0x0f, 0x05]), TfStep::RunOver(2));
        assert_eq!(tf_step(&[0x48, 0x0f, 0x05]), TfStep::RunOver(3));
        for code in [
            &[0xccu8][..],
            &[0xcd, 0x80],
            &[0x0f, 0x0b],
            &[0xcf],
            &[0x0f, 0x34],
        ] {
            assert_eq!(tf_step(code), TfStep::Refuse, "{code:x?}");
        }
        assert_eq!(tf_step(&[0x48, 0x89, 0xc8]), TfStep::Plain);
        assert_eq!(tf_step(&[0x0f, 0x84, 0, 0, 0, 0]), TfStep::Plain);
        assert_eq!(tf_step(&[]), TfStep::Refuse);
    }

    #[test]
    fn a_gate_gives_its_handler() {
        let mut g = [0u8; 16];
        g[0..2].copy_from_slice(&0x3456u16.to_le_bytes());
        g[5] = 0x8e;
        g[6..8].copy_from_slice(&0x9012u16.to_le_bytes());
        g[8..12].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert_eq!(gate_target(&g), Some(0xffff_ffff_9012_3456));
        g[5] = 0;
        assert_eq!(gate_target(&g), None);
    }

    #[test]
    fn the_count_since_an_exit_is_per_vcpu() {
        let n = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(100));
        let alarms = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut b = Bt::new(
            Box::new(Fixed {
                n: n.clone(),
                alarms: alarms.clone(),
            }),
            2,
        );
        n.store(150, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(b.account(0), 50);
        n.store(170, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(b.account(1), 20);
        n.store(200, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(b.account(0), 80);
        b.armed = true;
        b.reset(0);
        assert_eq!(b.since, vec![0, 20]);
        assert_eq!(*alarms.lock().unwrap(), vec![None]);
    }
}
