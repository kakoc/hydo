//! Where the host time of a long exit-free stretch went (`HYDO_KVM_GAPS`, a diagnostic of the
//! host side only: nothing of it reaches the guest, a snapshot or a fingerprint).
//!
//! A stretch is the host time between two exits that reach the engine. When one lasts at least
//! the threshold, the engine prints one line at its end (the next exit, or the SPIN that ends the
//! run) with its parts:
//!
//! - `run`: host time inside `KVM_RUN` while the guest ran freely (between the watchdog's
//!   kicks), `obs` and `state`: host time in the spin observer's looks (single steps, and the
//!   full-state cycles' arrivals), with the number of looks;
//! - from the engine thread's own counters (`/proc/thread-self`): its time on a CPU (`oncpu`), the
//!   time it waited for one (`waitcpu`), the guest time and the host kernel time of that CPU time
//!   (`guest`, `sys`), and its page faults (`minflt`, `majflt`);
//! - `steal`: the time the hypervisor under this machine took from all of its CPUs (`/proc/stat`,
//!   summed over the CPUs);
//! - the vCPU's and the VM's counters from KVM's statistics (`KVM_GET_STATS_FD`): the in-kernel
//!   exits, the page faults KVM handled, the pages that map guest RAM.
//!
//! The counters are read at the stretch's first kick (5 ms in by default) and at its end, so a
//! stretch without a kick gives only its wall time.

use std::time::Instant;

/// A reading of the counters.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    /// the engine thread: clock ticks of user, kernel and guest time; page faults
    pub utime: u64,
    pub stime: u64,
    pub gtime: u64,
    pub minflt: u64,
    pub majflt: u64,
    /// the engine thread's schedstat: nanoseconds on a CPU and waiting for one
    pub run_ns: u64,
    pub wait_ns: u64,
    /// the machine's steal time, clock ticks summed over its CPUs
    pub steal: u64,
    /// KVM's statistics (vCPU, then the VM's with `vm.`)
    pub kvm: Vec<(String, u64)>,
}

fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

impl Sample {
    /// The counters now, with KVM's statistics `kvm`. Where a file cannot be read (not Linux),
    /// its counters read 0.
    pub fn take(kvm: Vec<(String, u64)>) -> Sample {
        Sample::parse(
            kvm,
            &read("/proc/thread-self/stat"),
            &read("/proc/thread-self/schedstat"),
            &read("/proc/stat"),
        )
    }

    /// The counters from the text of a thread's `stat` and `schedstat` and the machine's `stat`.
    pub fn parse(kvm: Vec<(String, u64)>, st: &str, sched: &str, stat: &str) -> Sample {
        let mut s = Sample {
            kvm,
            ..Sample::default()
        };
        if let Some(i) = st.rfind(')') {
            // fields from the third (the state) on
            let f: Vec<u64> = st[i + 1..]
                .split_whitespace()
                .map(|x| x.parse().unwrap_or(0))
                .collect();
            let g = |n: usize| f.get(n - 3).copied().unwrap_or(0);
            s.minflt = g(10);
            s.majflt = g(12);
            s.utime = g(14);
            s.stime = g(15);
            s.gtime = g(43);
        }
        let ss: Vec<u64> = sched
            .split_whitespace()
            .map(|x| x.parse().unwrap_or(0))
            .collect();
        s.run_ns = ss.first().copied().unwrap_or(0);
        s.wait_ns = ss.get(1).copied().unwrap_or(0);
        if let Some(l) = stat.lines().next() {
            s.steal = l
                .split_whitespace()
                .nth(8)
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
        }
        s
    }
}

/// The KVM counters a gap line shows (when they moved; `vm.pages_*` always).
const KVM_SHOWN: [&str; 14] = [
    "exits",
    "pf_taken",
    "pf_fixed",
    "pf_spurious",
    "pf_fast",
    "pf_guest",
    "irq_exits",
    "signal_exits",
    "host_state_reload",
    "nested_run",
    "preemption_reported",
    "vm.pages_4k",
    "vm.pages_2m",
    "vm.pages_1g",
];

/// The state of the diagnostic for the stretch in progress.
#[derive(Default)]
pub struct Gaps {
    /// the threshold in seconds (None: off)
    pub min: Option<f64>,
    /// host seconds in `KVM_RUN` (free runs), the observer's looks and the full-state looks
    pub t_run: f64,
    pub t_obs: f64,
    pub t_state: f64,
    pub looks: u64,
    pub states: u64,
    pub kicks: u64,
    /// the counters at the stretch's first kick, and when they were read
    pub s0: Option<(Instant, Sample)>,
}

impl Gaps {
    /// Log stretches at least `min` seconds long (`HYDO_KVM_GAPS`; None: off).
    pub fn new(min: Option<f64>) -> Gaps {
        Gaps {
            min,
            ..Gaps::default()
        }
    }

    pub fn on(&self) -> bool {
        self.min.is_some()
    }

    /// Forget the stretch (a new one starts).
    pub fn reset(&mut self) {
        let min = self.min;
        *self = Gaps {
            min,
            ..Gaps::default()
        };
    }

    /// The line for a stretch of `wall` seconds that ended at exit `exit` with `why`, or None
    /// when it was shorter than the threshold.
    pub fn line(
        &self,
        wall: f64,
        exit: u64,
        rip: u64,
        why: &str,
        now: Option<Sample>,
    ) -> Option<String> {
        let min = self.min?;
        if wall < min {
            return None;
        }
        use std::fmt::Write as _;
        let mut t = String::new();
        let _ = write!(
            t,
            "hydo-kvm: gap {wall:.3} s at exit {exit} ({why}) rip {rip:#x}: run {:.3} s, obs {:.3} s in {} looks, state {:.3} s in {} looks, kicks {}",
            self.t_run, self.t_obs, self.looks, self.t_state, self.states, self.kicks
        );
        if let (Some((t0, a)), Some(b)) = (&self.s0, now) {
            let tick = 0.01; // USER_HZ 100
            let d = |x: u64, y: u64| y.saturating_sub(x);
            let _ = write!(
                t,
                "; from the first kick ({:.3} s): oncpu {:.3} s, waitcpu {:.3} s, guest {:.2} s, sys {:.2} s, user {:.2} s, minflt {}, majflt {}, steal {:.2} s",
                t0.elapsed().as_secs_f64(),
                d(a.run_ns, b.run_ns) as f64 / 1e9,
                d(a.wait_ns, b.wait_ns) as f64 / 1e9,
                d(a.gtime, b.gtime) as f64 * tick,
                d(a.stime, b.stime) as f64 * tick,
                d(a.utime, b.utime).saturating_sub(d(a.gtime, b.gtime)) as f64 * tick,
                d(a.minflt, b.minflt),
                d(a.majflt, b.majflt),
                d(a.steal, b.steal) as f64 * tick,
            );
            for name in KVM_SHOWN {
                let x = a.kvm.iter().find(|(n, _)| n == name).map(|v| v.1);
                let y = b.kvm.iter().find(|(n, _)| n == name).map(|v| v.1);
                if let (Some(x), Some(y)) = (x, y) {
                    if name.starts_with("vm.pages") {
                        let _ = write!(t, ", {name} {y}");
                    } else if y != x {
                        let _ = write!(t, ", {name} +{}", y.wrapping_sub(x));
                    }
                }
            }
        }
        Some(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // a thread's stat (fields 1 to 44), its schedstat and the machine's stat, as Linux writes
    // them; the name has a space and a parenthesis, as a thread's may
    const STAT: &str = "4242 (kvm (vcpu 0)) R 1 2 3 0 -1 4194560 2163 0 7 0 \
        237 97 0 0 20 0 3 0 100 1000 200 0 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 137 0";
    const SCHED: &str = "2433000000 1000000 412\n";
    const PSTAT: &str = "cpu  10 0 20 3000 5 0 1 42 0 0\ncpu0 1 0 2 300 0 0 0 4 0 0\n";

    #[test]
    fn the_counters_are_read_from_their_fields() {
        let s = Sample::parse(vec![("exits".into(), 9)], STAT, SCHED, PSTAT);
        assert_eq!((s.minflt, s.majflt), (2163, 7));
        assert_eq!((s.utime, s.stime, s.gtime), (237, 97, 137));
        assert_eq!((s.run_ns, s.wait_ns), (2_433_000_000, 1_000_000));
        assert_eq!(s.steal, 42);
        assert_eq!(s.kvm, vec![("exits".to_string(), 9)]);
        // unreadable files (not Linux): every counter 0
        let z = Sample::parse(Vec::new(), "", "", "");
        assert_eq!((z.utime, z.run_ns, z.steal), (0, 0, 0));
    }

    #[test]
    fn a_stretch_shorter_than_the_threshold_has_no_line() {
        let g = Gaps::new(None);
        assert!(!g.on());
        assert_eq!(g.line(100.0, 1, 0, "an exit", None), None);
        let mut g = Gaps::new(Some(0.2));
        assert!(g.on());
        assert_eq!(g.line(0.1, 1, 0, "an exit", None), None);
        g.t_run = 0.125;
        g.t_state = 8.002;
        g.states = 4;
        g.kicks = 4;
        let l = g.line(8.127, 1297, 0xffff_ffff_813d_3e01, "an exit", None);
        assert_eq!(
            l.as_deref(),
            Some(
                "hydo-kvm: gap 8.127 s at exit 1297 (an exit) rip 0xffffffff813d3e01: run 0.125 s, \
                 obs 0.000 s in 0 looks, state 8.002 s in 4 looks, kicks 4"
            )
        );
    }

    #[test]
    fn the_line_shows_what_moved_between_the_first_kick_and_the_end() {
        let mut g = Gaps::new(Some(0.0));
        let kvm = |e: u64, p: u64| {
            vec![
                ("exits".to_string(), e),
                ("pf_taken".to_string(), 5),
                ("vm.pages_4k".to_string(), p),
            ]
        };
        let a = Sample::parse(kvm(100, 10), STAT, SCHED, PSTAT);
        let mut b = a.clone();
        b.kvm = kvm(150, 20);
        b.run_ns += 500_000_000;
        b.utime += 30;
        b.gtime += 20;
        b.stime += 10;
        b.steal += 3;
        g.s0 = Some((Instant::now(), a));
        let l = g.line(0.5, 7, 0x1000, "SPIN", Some(b)).unwrap_or_default();
        assert!(
            l.contains("oncpu 0.500 s, waitcpu 0.000 s, guest 0.20 s, sys 0.10 s, user 0.10 s"),
            "{l}"
        );
        assert!(l.contains("steal 0.03 s"), "{l}");
        // a counter that moved shows its growth, one that did not is left out, and the page
        // counts show their value
        assert!(l.ends_with(", exits +50, vm.pages_4k 20"), "{l}");
        // a new stretch forgets the old one
        g.reset();
        assert!(g.s0.is_none() && g.on());
    }
}
