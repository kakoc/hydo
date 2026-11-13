//! `--sweep turns` (M205): where a schedule's verdict changes with the vCPU turn length.
//!
//! With several vCPUs, hvx runs them in turns of exactly `q=` guest branches, and the KVM engine
//! in turns of `q=` counted VM exits (docs/design/kvm-vcpus.md; the defaults below differ by
//! engine, because the units do). A race between
//! threads can then depend only on the turn length: nats-server's flaky test 8363 fails at
//! `q=18800` and passes at `q=18700` on two vCPUs, each twice with one fingerprint
//! (hydo/onboard/examples/nats-flaky-8363). This sweep finds such boundaries for one schedule:
//!
//! 1. it runs the schedule with every length of a coarse grid, spaced evenly on a log scale
//!    between `--turn-range LO-HI` (`--turn-points` lengths, both ends included);
//! 2. it bisects every pair of neighbouring lengths whose verdict kinds (`ok`, `violation`,
//!    `error`) differ, all pairs in one round at a time, until the two lengths are at most
//!    `--turn-resolution` branches apart;
//! 3. it runs both lengths of every boundary a second time, so that each is reported with two
//!    fresh fingerprints.
//!
//! The search itself is [`search`], a pure function of a runner, so that the tests run it
//! without a VM; [`with_q`] puts a length into a schedule.

/// The default `--turn-range`: from 64 branches to twice the module's default turn (2^20).
pub const RANGE: &str = "64-2097152";
/// The default `--turn-range` on KVM: from 1 exit to 16 times the engine's default turn of 256
/// exits.
pub const RANGE_KVM: &str = "1-4096";
/// The default `--turn-points`.
pub const POINTS: u64 = 12;
/// The default `--turn-resolution`.
pub const RESOLUTION: u64 = 100;
/// The default `--turn-resolution` on KVM: exact (one exit), because a KVM turn length is
/// coarse.
pub const RESOLUTION_KVM: u64 = 1;

/// The turn range searched: `--turn-range`, or the engine's default.
pub fn range_of(a: &crate::args::Args, kvm: bool) -> String {
    match &a.turn_range {
        Some(r) => r.clone(),
        None if kvm => RANGE_KVM.into(),
        None => RANGE.into(),
    }
}

/// The resolution of the narrowing: `--turn-resolution`, or the engine's default.
pub fn resolution_of(a: &crate::args::Args, kvm: bool) -> u64 {
    match a.turn_resolution {
        Some(r) => r.max(1) as u64,
        None if kvm => RESOLUTION_KVM,
        None => RESOLUTION,
    }
}

/// `n` turn-length units: guest branches on hvx, counted VM exits on KVM.
pub fn amount(n: u64, kvm: bool) -> String {
    let unit = match (kvm, n == 1) {
        (true, true) => "exit",
        (true, false) => "exits",
        (false, true) => "branch",
        (false, false) => "branches",
    };
    format!("{n} {unit}")
}
/// The most runs a sweep makes before it stops narrowing (a guard; a sweep with defaults on one
/// boundary takes about 25).
pub const MAX_RUNS: usize = 400;

/// What one run at one length gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// `ok`, `violation` or `error`.
    pub kind: String,
    /// The verdict line (None: no result).
    pub verdict: Option<String>,
    pub fp: Option<String>,
    /// The run's tag.
    pub tag: String,
}

/// The sweep's answer: every length run, in increasing order, and the boundaries (index pairs
/// into `points` of neighbours with different kinds).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub points: Vec<(u64, Outcome)>,
    pub boundaries: Vec<(usize, usize)>,
    /// The guard stopped the narrowing (MAX_RUNS).
    pub capped: bool,
}

/// `LO-HI`, both whole numbers with 1 <= LO < HI.
pub fn parse_range(t: &str) -> Result<(u64, u64), String> {
    let bad = || format!("--turn-range {t}: expected LO-HI, two whole numbers with 1 <= LO < HI");
    let (l, h) = t.split_once('-').ok_or_else(bad)?;
    let lo: u64 = l.trim().parse().map_err(|_| bad())?;
    let hi: u64 = h.trim().parse().map_err(|_| bad())?;
    if lo == 0 || lo >= hi {
        return Err(bad());
    }
    Ok((lo, hi))
}

/// `n` lengths between `lo` and `hi` (both included), evenly spaced on a log scale, rounded and
/// without repeats; at least the two ends.
pub fn grid(lo: u64, hi: u64, n: u64) -> Vec<u64> {
    let n = n.max(2);
    let (a, b) = ((lo as f64).ln(), (hi as f64).ln());
    let mut v: Vec<u64> = (0..n)
        .map(|i| {
            if i == 0 {
                lo
            } else if i == n - 1 {
                hi
            } else {
                (a + (b - a) * i as f64 / (n - 1) as f64).exp().round() as u64
            }
        })
        .map(|q| q.clamp(lo, hi))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The schedule with its turn length `q`: the host part's own `q=` and `qs=` (a seeded turn
/// stream) are replaced; everything else is kept as written.
pub fn with_q(base: &str, q: u64) -> String {
    let (host, guest) = match base.split_once('|') {
        Some((h, g)) => (h, Some(g)),
        None => (base, None),
    };
    let mut words: Vec<String> = host
        .split_whitespace()
        .filter(|w| !w.starts_with("q=") && !w.starts_with("qs="))
        .map(str::to_string)
        .collect();
    words.push(format!("q={q}"));
    match guest {
        Some(g) => format!("{} |{g}", words.join(" ")),
        None => format!("{} | run", words.join(" ")),
    }
}

/// The search. `run` runs a batch of lengths (each once, from a fresh start) and returns their
/// outcomes in the same order; batches are the grid, then one round of midpoints at a time.
pub fn search(
    lo: u64,
    hi: u64,
    points: u64,
    resolution: u64,
    run: &mut dyn FnMut(&[u64]) -> Vec<Outcome>,
) -> Found {
    let resolution = resolution.max(1);
    let mut got: std::collections::BTreeMap<u64, Outcome> = Default::default();
    let first = grid(lo, hi, points);
    for (q, o) in first.iter().zip(run(&first)) {
        got.insert(*q, o);
    }
    let mut capped = false;
    loop {
        let keys: Vec<u64> = got.keys().copied().collect();
        let mids: Vec<u64> = keys
            .windows(2)
            .filter(|w| got[&w[0]].kind != got[&w[1]].kind && w[1] - w[0] > resolution)
            .map(|w| w[0] + (w[1] - w[0]) / 2)
            .collect();
        if mids.is_empty() {
            break;
        }
        if got.len() + mids.len() > MAX_RUNS {
            capped = true;
            break;
        }
        for (q, o) in mids.iter().zip(run(&mids)) {
            got.insert(*q, o);
        }
    }
    let points: Vec<(u64, Outcome)> = got.into_iter().collect();
    let boundaries = (1..points.len())
        .filter(|&i| points[i - 1].1.kind != points[i].1.kind)
        .map(|i| (i - 1, i))
        .collect();
    Found {
        points,
        boundaries,
        capped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(kind: &str, q: u64) -> Outcome {
        Outcome {
            kind: kind.into(),
            verdict: Some(kind.to_uppercase()),
            fp: Some(format!("{q:016x}")),
            tag: format!("q-{q}"),
        }
    }

    #[test]
    fn the_grid_is_log_spaced_and_has_both_ends() {
        assert_eq!(grid(64, 2097152, 12).first(), Some(&64));
        assert_eq!(grid(64, 2097152, 12).last(), Some(&2097152));
        assert_eq!(grid(64, 2097152, 12).len(), 12);
        assert_eq!(grid(1, 4, 10), vec![1, 2, 3, 4]);
        assert_eq!(grid(100, 10000, 3), vec![100, 1000, 10000]);
        assert_eq!(grid(5, 6, 1), vec![5, 6]);
    }

    #[test]
    fn a_length_replaces_the_schedules_own() {
        assert_eq!(with_q("disk=none | run", 18800), "disk=none q=18800 | run");
        assert_eq!(
            with_q("disk=none q=5 qs=1f dseed=3 | run K=a@1+1", 7),
            "disk=none dseed=3 q=7 | run K=a@1+1"
        );
        assert_eq!(with_q("disk=none", 9), "disk=none q=9 | run");
        assert!(hydo_schedule::Schedule::parse(&with_q("disk=none | run", 18800)).is_ok());
    }

    #[test]
    fn the_defaults_depend_on_the_engine() {
        let mut a = crate::args::Args::default();
        assert_eq!(range_of(&a, false), RANGE);
        assert_eq!(range_of(&a, true), RANGE_KVM);
        assert_eq!(resolution_of(&a, false), RESOLUTION);
        assert_eq!(resolution_of(&a, true), RESOLUTION_KVM);
        a.turn_range = Some("8-64".into());
        a.turn_resolution = Some(4);
        assert_eq!(range_of(&a, true), "8-64");
        assert_eq!(resolution_of(&a, false), 4);
        assert_eq!(amount(1, true), "1 exit");
        assert_eq!(amount(4096, true), "4096 exits");
        assert_eq!(amount(100, false), "100 branches");
    }

    #[test]
    fn the_range_is_read_and_checked() {
        assert_eq!(parse_range("64-2097152"), Ok((64, 2097152)));
        assert!(parse_range("0-5").is_err());
        assert!(parse_range("9-9").is_err());
        assert!(parse_range("x").is_err());
    }

    /// The nats-flaky-8363 shape: passes up to 18,700 branches, fails from 18,800 on.
    #[test]
    fn one_boundary_is_narrowed_to_the_resolution() {
        let mut calls = Vec::new();
        let mut run = |qs: &[u64]| {
            calls.push(qs.to_vec());
            qs.iter()
                .map(|&q| out(if q <= 18750 { "ok" } else { "violation" }, q))
                .collect()
        };
        let f = search(64, 2097152, 12, 100, &mut run);
        assert!(!f.capped);
        assert_eq!(f.boundaries.len(), 1);
        let (a, b) = f.boundaries[0];
        let (qa, qb) = (f.points[a].0, f.points[b].0);
        assert!(qa <= 18750 && qb > 18750 && qb - qa <= 100, "{qa} {qb}");
        assert_eq!(f.points[a].1.kind, "ok");
        assert_eq!(f.points[b].1.kind, "violation");
        // the grid, then one midpoint per round
        assert_eq!(calls[0].len(), 12);
        assert!(calls[1..].iter().all(|c| c.len() == 1));
        // points are in increasing order and unique
        assert!(f.points.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn several_boundaries_are_narrowed_together() {
        // fails only in [1000, 5000)
        let mut rounds = 0;
        let mut run = |qs: &[u64]| {
            rounds += 1;
            qs.iter()
                .map(|&q| {
                    out(
                        if (1000..5000).contains(&q) {
                            "violation"
                        } else {
                            "ok"
                        },
                        q,
                    )
                })
                .collect()
        };
        let f = search(64, 2097152, 12, 1, &mut run);
        assert_eq!(f.boundaries.len(), 2);
        let q = |i: usize| f.points[i].0;
        assert_eq!((q(f.boundaries[0].0), q(f.boundaries[0].1)), (999, 1000));
        assert_eq!((q(f.boundaries[1].0), q(f.boundaries[1].1)), (4999, 5000));
        assert!(rounds < 30, "{rounds}");
    }

    #[test]
    fn one_verdict_everywhere_has_no_boundary() {
        let mut run = |qs: &[u64]| qs.iter().map(|&q| out("violation", q)).collect();
        let f = search(64, 2097152, 12, 100, &mut run);
        assert!(f.boundaries.is_empty());
        assert_eq!(f.points.len(), 12);
    }

    #[test]
    fn the_guard_stops_a_search_that_would_not_end() {
        // the verdict alternates with every length: a boundary between every two
        let mut run = |qs: &[u64]| {
            qs.iter()
                .map(|&q| out(if q % 2 == 0 { "ok" } else { "error" }, q))
                .collect()
        };
        let f = search(1, 1 << 30, 64, 1, &mut run);
        assert!(f.capped);
        assert!(f.points.len() <= MAX_RUNS);
    }
}
