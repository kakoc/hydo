//! The state search (docs/design/state-search.md): a corpus of mid-run states, snapshots that
//! runs took at stops that showed rare features, and branches that run the rest of a run again
//! from such a state with the faults of a fresh seed moved into the remaining steps (and, on KVM,
//! a fresh seed's timing from the snapshot's stop on, `br=`).
//!
//! The search runs in rounds. Each schedule of a round is a branch with probability
//! `mix`/1000 once the corpus has an entry, else the strategy's schedule of the next seed. The
//! corpus, the feature counts and the free snapshot names change only between rounds, and a
//! round's results are read in order, so the search is a pure function of its parameters and the
//! runs' results.

use crate::rng::{mix64, SplitMix};
use crate::sched::{Pause, S};
use std::collections::{BTreeMap, BTreeSet};

/// Rounds of this many schedules.
pub const BATCH: usize = 4;
/// A search gives up branching after this many invalid branches in a row.
pub const INVALID_RUN: usize = 64;

/// The search's parameters.
#[derive(Clone, Debug)]
pub struct Params {
    pub mix: u64,
    pub batch: usize,
    pub corpus: usize,
    /// Snapshot names beyond the corpus: what one round's runs may take.
    pub spare: usize,
    pub rare: u64,
    pub snaps_per_run: usize,
    pub budget: usize,
    pub stop: bool,
    pub stream_seed: u64,
    pub first_seed: u64,
    /// The workload's steps.
    pub steps: i64,
    /// The engine changes the timing at a branch point (KVM with timing options): a state after
    /// the last step range is still worth a branch.
    pub timing: bool,
}

/// The stream seed of a search whose seeds start at `lo`.
pub fn stream_seed(lo: u64) -> u64 {
    mix64(lo ^ 0x5354_4154_4553_5243) // "STATESRC"
}

/// One snapshot a run took (the worker's record).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snap {
    pub name: String,
    pub stop: u64,
    pub at: u64,
    pub new: Vec<String>,
}

/// What one run gave the search.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub failed: bool,
    pub verdict: String,
    pub features: Vec<String>,
    pub snaps: Vec<Snap>,
    /// (first step, end step, stop of the note)
    pub ranges: Vec<(u64, u64, u64)>,
    pub job_s: f64,
    pub clock: u64,
    /// A branch whose prefix differed from its parent's: no VM ran.
    pub invalid: Option<String>,
}

/// A state of the corpus.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// The run that took it.
    pub tag: String,
    pub text: String,
    pub stop: u64,
    pub at: u64,
    /// The end step of the step range in progress at the snapshot (the steps when none was).
    pub b: i64,
    pub new: Vec<String>,
    pub picks: u64,
    pub run: usize,
}

/// Where a run's schedule came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Fresh {
        seed: u64,
    },
    Branch {
        parent: String,
        from: String,
        stop: u64,
        at: u64,
        b: i64,
        fresh_seed: u64,
    },
}

/// One run the search made.
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    pub run: usize,
    pub tag: String,
    pub text: String,
    pub origin: Origin,
    pub outcome: Outcome,
    /// The features this run showed that no earlier run had shown.
    pub first: Vec<String>,
}

/// What the search did.
#[derive(Clone, Debug, Default)]
pub struct Found {
    pub made: Vec<Made>,
    pub corpus: Vec<Entry>,
    pub invalid: usize,
    pub invalid_why: BTreeMap<String, usize>,
    pub stopped: bool,
    pub counts: BTreeMap<String, u64>,
}

/// One job of a round: tag, schedule text, the names it may take, the snapshot to branch from.
pub struct Job {
    pub tag: String,
    pub text: String,
    pub names: Vec<String>,
    pub from: Option<String>,
    pub counts: BTreeMap<String, u64>,
    pub rare: u64,
    pub any_new: bool,
    /// Snapshot points by clock, for a target whose runs show no features.
    pub spread: Vec<u64>,
}

/// The spread points of a target without features: `k` points evenly inside the median clock of
/// the runs so far (none before the first run, or when some run showed a feature).
pub fn spread_points(made: &[Made], counts: &BTreeMap<String, u64>, k: usize) -> Vec<u64> {
    if !counts.is_empty() || made.is_empty() || k == 0 {
        return Vec::new();
    }
    let mut c: Vec<u64> = made
        .iter()
        .filter(|m| matches!(m.origin, Origin::Fresh { .. }))
        .map(|m| m.outcome.clock)
        .collect();
    if c.is_empty() {
        return Vec::new();
    }
    c.sort_unstable();
    let med = c[c.len() / 2];
    (1..=k as u64).map(|i| med * i / (k as u64 + 1)).collect()
}

/// The end step of the step range in progress at stop `stop`: the first range whose end note is
/// at or after the stop, else the workload's end.
pub fn range_end(ranges: &[(u64, u64, u64)], stop: u64, steps: i64) -> i64 {
    ranges
        .iter()
        .find(|r| r.2 >= stop)
        .map(|r| r.1 as i64)
        .unwrap_or(steps)
}

/// The energy of an entry: 1 / (c × (picks + 1)), c the count of its rarest new feature.
pub fn energy(e: &Entry, counts: &BTreeMap<String, u64>) -> f64 {
    let c = e
        .new
        .iter()
        .map(|f| counts.get(f).copied().unwrap_or(1).max(1))
        .min()
        .unwrap_or(1);
    1.0 / (c as f64 * (e.picks + 1) as f64)
}

/// Run the search. `fresh(seed)` is the strategy's schedule text of a seed; `branch(entry,
/// fresh_seed)` the branch's text (None when it cannot make one); `run` runs a round.
pub fn search(
    p: &Params,
    fresh: &mut dyn FnMut(u64) -> String,
    branch: &mut dyn FnMut(&Entry, u64) -> Option<String>,
    run: &mut dyn FnMut(&[Job]) -> Vec<Outcome>,
) -> Found {
    let mut rng = SplitMix::new(p.stream_seed);
    let mut f = Found::default();
    let mut free: Vec<String> = (0..p.corpus + p.spare).map(|i| format!("x{i}")).collect();
    let mut next_seed = p.first_seed;
    let mut invalid_row = 0usize;
    let mut tagn = 0usize;
    while f.made.len() < p.budget {
        let mut jobs: Vec<(Job, Origin)> = Vec::new();
        let room = (p.budget - f.made.len()).min(p.batch);
        let mut names = free.clone();
        names.reverse();
        let spread = spread_points(&f.made, &f.counts, p.snaps_per_run);
        for _ in 0..room {
            let branching = p.mix > 0
                && !f.corpus.is_empty()
                && invalid_row < INVALID_RUN
                && (rng.next() % 1000) < p.mix;
            let mut made = None;
            if branching {
                let total: f64 = f.corpus.iter().map(|e| energy(e, &f.counts)).sum();
                let mut r = (rng.next() >> 11) as f64 / (1u64 << 53) as f64 * total;
                let mut pick = f.corpus.len() - 1;
                for (i, e) in f.corpus.iter().enumerate() {
                    let w = energy(e, &f.counts);
                    if r < w {
                        pick = i;
                        break;
                    }
                    r -= w;
                }
                let fs = rng.next();
                let e = f.corpus[pick].clone();
                if let Some(text) = branch(&e, fs) {
                    f.corpus[pick].picks += 1;
                    made = Some((
                        text,
                        Some(e.name.clone()),
                        Origin::Branch {
                            parent: e.tag.clone(),
                            from: e.name.clone(),
                            stop: e.stop,
                            at: e.at,
                            b: e.b,
                            fresh_seed: fs,
                        },
                    ));
                }
            }
            let (text, from, origin) = made.unwrap_or_else(|| {
                let sd = next_seed;
                next_seed += 1;
                (fresh(sd), None, Origin::Fresh { seed: sd })
            });
            tagn += 1;
            let mut own = Vec::new();
            if p.mix > 0 {
                for _ in 0..p.snaps_per_run {
                    if let Some(n) = names.pop() {
                        own.push(n);
                    }
                }
            }
            jobs.push((
                Job {
                    tag: format!("seed-{tagn}"),
                    text,
                    names: own,
                    from,
                    counts: f.counts.clone(),
                    rare: p.rare,
                    any_new: f.corpus.is_empty(),
                    spread: spread.clone(),
                },
                origin,
            ));
        }
        let js: Vec<Job> = jobs
            .iter()
            .map(|(j, _)| Job {
                tag: j.tag.clone(),
                text: j.text.clone(),
                names: j.names.clone(),
                from: j.from.clone(),
                counts: j.counts.clone(),
                rare: j.rare,
                any_new: j.any_new,
                spread: j.spread.clone(),
            })
            .collect();
        let outs = run(&js);
        let made_before = f.made.len();
        let mut failed = false;
        let mut new_entries = Vec::new();
        for ((job, origin), o) in jobs.into_iter().zip(outs) {
            if let Some(why) = &o.invalid {
                f.invalid += 1;
                invalid_row += 1;
                let key: String = why.chars().take(80).collect();
                *f.invalid_why.entry(key).or_default() += 1;
                // the tag number is not reused: tags stay unique
                continue;
            }
            invalid_row = 0;
            let run_no = f.made.len() + 1;
            let mut first = Vec::new();
            for ft in &o.features {
                if !f.counts.contains_key(ft) {
                    first.push(ft.clone());
                }
            }
            for s in &o.snaps {
                let b = range_end(&o.ranges, s.stop, p.steps);
                if b >= p.steps && !p.timing {
                    // no fault is left to draw and the timing stays: not worth a branch
                    continue;
                }
                new_entries.push(Entry {
                    name: s.name.clone(),
                    tag: job.tag.clone(),
                    text: job.text.clone(),
                    stop: s.stop,
                    at: s.at,
                    b,
                    new: s.new.clone(),
                    picks: 0,
                    run: run_no,
                });
            }
            failed |= o.failed;
            f.made.push(Made {
                run: run_no,
                tag: job.tag,
                text: job.text,
                origin,
                outcome: o,
                first,
            });
        }
        // the counts of this round's runs, each run's features once
        for m in f.made.iter().skip(made_before) {
            add_counts(&mut f.counts, &m.outcome.features);
        }
        f.corpus.extend(new_entries);
        // free names: everything not in the corpus
        while f.corpus.len() > p.corpus {
            let (i, _) = f
                .corpus
                .iter()
                .enumerate()
                .map(|(i, e)| (i, energy(e, &f.counts)))
                .fold(
                    (0, f64::INFINITY),
                    |acc, (i, w)| {
                        if w < acc.1 {
                            (i, w)
                        } else {
                            acc
                        }
                    },
                );
            f.corpus.remove(i);
        }
        let held: BTreeSet<String> = f.corpus.iter().map(|e| e.name.clone()).collect();
        free = (0..p.corpus + p.spare)
            .map(|i| format!("x{i}"))
            .filter(|n| !held.contains(n))
            .collect();
        if failed && p.stop {
            f.stopped = true;
            break;
        }
    }
    f
}

/// Count each run's features once: called by [`search`] between rounds.
pub fn add_counts(counts: &mut BTreeMap<String, u64>, features: &[String]) {
    for ft in features.iter().collect::<BTreeSet<_>>() {
        *counts.entry(ft.clone()).or_default() += 1;
    }
}

/// The first step of a schedule's fault fields, for the branch rule.
fn starts(s: &S) -> Vec<(&'static str, i64)> {
    let mut v = Vec::new();
    if let Some((_, at, _)) = &s.k {
        v.push(("k", *at));
    }
    if let Some((_, a, _, _)) = &s.n {
        v.push(("n", *a));
    }
    if let Some(c) = s.c {
        v.push(("c", c));
    }
    match &s.p {
        Some(Pause::Plus(_, at, _)) | Some(Pause::Timed(_, at, _, _)) => v.push(("p", *at)),
        None => {}
    }
    if let Some((at, _)) = s.t {
        v.push(("t", at));
    }
    if let Some((_, a, _, _, _)) = &s.o {
        v.push(("o", *a));
    }
    if let Some((_, at, _)) = &s.x {
        v.push(("x", *at));
    }
    if let Some(fc) = &s.f {
        v.push(("f", fc.step));
    }
    if let Some(g) = &s.g {
        v.push(("g", g.at));
    }
    if let Some((_, at, _)) = &s.l {
        v.push(("l", *at));
    }
    v
}

/// A fault field's start moved into the steps after `b`: `b + 1 + at mod (steps - 1 - b)`, or
/// None when no step is left.
fn moved(at: i64, b: i64, steps: i64) -> Option<i64> {
    let room = steps - 1 - b;
    if room <= 0 {
        return None;
    }
    Some(b + 1 + at.rem_euclid(room))
}

/// The branch's schedule (docs/design/state-search.md "A branch"): the host part and the whole
/// run's settings of `a`; every fault of `a` that starts at or before `b`; and every fault of
/// `f` of a kind `a` does not keep, moved into the steps after `b`.
pub fn branch_schedule(a: &S, f: &S, b: i64, steps: i64) -> S {
    let mut out = a.clone();
    let keep: BTreeSet<&str> = starts(a)
        .into_iter()
        .filter(|(_, at)| *at <= b)
        .map(|(k, _)| k)
        .collect();
    let take = |k: &str| !keep.contains(k);
    let mv = |at: i64| moved(at, b, steps);
    if take("k") {
        out.k =
            f.k.as_ref()
                .and_then(|(s, at, d)| mv(*at).map(|x| (s.clone(), x, *d)));
    }
    if take("n") {
        out.n = f.n.as_ref().and_then(|(s, a0, b0, kinds)| {
            mv(*a0).map(|x| (s.clone(), x, (x + (b0 - a0)).min(steps), kinds.clone()))
        });
    }
    if take("c") {
        out.c = f.c.and_then(mv);
    }
    if take("p") {
        out.p = match &f.p {
            Some(Pause::Plus(s, at, n)) => mv(*at).map(|x| Pause::Plus(s.clone(), x, *n)),
            Some(Pause::Timed(s, at, o, d)) => mv(*at).map(|x| Pause::Timed(s.clone(), x, *o, *d)),
            None => None,
        };
    }
    if take("t") {
        out.t = f.t.and_then(|(at, d)| mv(at).map(|x| (x, d)));
    }
    if take("o") {
        out.o = f.o.as_ref().and_then(|(s, a0, b0, dir, kinds)| {
            mv(*a0).map(|x| {
                (
                    s.clone(),
                    x,
                    (x + (b0 - a0)).min(steps),
                    dir.clone(),
                    kinds.clone(),
                )
            })
        });
    }
    if take("x") {
        out.x =
            f.x.as_ref()
                .and_then(|(s, at, off)| mv(*at).map(|x| (s.clone(), x, *off)));
    }
    if take("f") {
        out.f = f.f.as_ref().and_then(|fc| {
            mv(fc.step).map(|x| {
                let mut c = fc.clone();
                c.step = x;
                c
            })
        });
    }
    if take("g") {
        out.g = f.g.as_ref().and_then(|g| {
            mv(g.at).map(|x| {
                let mut c = g.clone();
                c.at = x;
                c
            })
        });
    }
    if take("l") {
        out.l =
            f.l.as_ref()
                .and_then(|(s, at, d)| mv(*at).map(|x| (s.clone(), x, *d)));
    }
    // the protocol-fault windows and the reactions: a's that start by b, then f's moved
    out.h.retain(|h| h.a <= b);
    for h in &f.h {
        if let Some(x) = mv(h.a) {
            let mut c = h.clone();
            c.b = (x + (h.b - h.a)).min(steps);
            c.a = x;
            out.h.push(c);
        }
    }
    out.a.retain(|r| r.at <= b);
    for r in &f.a {
        if let Some(x) = mv(r.at) {
            let mut c = r.clone();
            c.at = x;
            out.a.push(c);
        }
    }
    // a cut and a k-th-flush cut are two power cuts: the host part's cf= wins
    if out.cf.is_some() {
        out.c = None;
    }
    out
}

/// A schedule text split at its first branch point: the text without the branch points, and the
/// branch points' words (each starting with a space), to put back before the `|`.
pub fn split_br(text: &str) -> (String, String) {
    let Some((host, guest)) = text.split_once('|') else {
        return (text.to_string(), String::new());
    };
    let words: Vec<&str> = host.split_whitespace().collect();
    match words.iter().position(|w| w.starts_with("br=")) {
        None => (text.to_string(), String::new()),
        Some(i) => (
            format!("{} |{guest}", words[..i].join(" ")),
            words[i..].iter().map(|w| format!(" {w}")).collect(),
        ),
    }
}

/// `text` with `extra` (words, each starting with a space) at the end of its host part.
pub fn with_host_words(text: &str, extra: &str) -> String {
    match text.split_once(" |") {
        Some((h, g)) => format!("{h}{extra} |{g}"),
        None => text.to_string(),
    }
}

/// The timing words of a fresh schedule's text, as a branch point's segment: its tick jitter and
/// its forced preemption points (the host words whose keys a branch point takes), each with a
/// space before it.
pub fn timing_words(text: &str) -> String {
    let host = text.split('|').next().unwrap_or("");
    host.split_whitespace()
        .take_while(|w| !w.starts_with("br="))
        .filter(|w| {
            w.split('=')
                .next()
                .is_some_and(|k| hydo_schedule::BRANCH_KEYS.contains(&k))
        })
        .map(|w| format!(" {w}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> S {
        S::new("none", 1)
    }

    #[test]
    fn a_branch_keeps_the_prefix_and_moves_the_fresh_faults() {
        let mut a = st();
        a.k = Some(("mysql".into(), 12, 3));
        a.c = Some(20);
        let mut f = st();
        f.p = Some(Pause::Plus("kafka".into(), 3, 2));
        f.c = Some(5);
        f.k = Some(("redis".into(), 1, 1));
        let b = branch_schedule(&a, &f, 12, 30);
        // K starts at 12 <= b: kept; C=20 starts after b: replaced by f's C moved
        assert_eq!(b.k, a.k);
        assert_eq!(b.c, Some(18));
        assert_eq!(b.p, Some(Pause::Plus("kafka".into(), 13 + 3, 2)));
        // nothing left after the last step: a's C=20 is in the prefix, f's pause has no room
        let b = branch_schedule(&a, &f, 29, 30);
        assert_eq!(b.c, Some(20));
        assert_eq!(b.p, None);
    }

    #[test]
    fn branch_points_split_and_join() {
        let (m, br) = split_br("disk=none dseed=1 pct=5 pctn=3 pctk=100 br=900 pct=7 pctn=2 | run");
        assert_eq!(m, "disk=none dseed=1 pct=5 pctn=3 pctk=100 | run");
        assert_eq!(br, " br=900 pct=7 pctn=2");
        assert_eq!(
            with_host_words(&m, &br),
            "disk=none dseed=1 pct=5 pctn=3 pctk=100 br=900 pct=7 pctn=2 | run"
        );
        assert_eq!(split_br("disk=none | run K=a@1+1").1, "");
        assert_eq!(
            timing_words("disk=none dseed=3 tj=5 tjpm=100 tjmax=1000 pct=7 pctn=2 pctk=9 | run"),
            " tj=5 tjpm=100 tjmax=1000 pct=7 pctn=2 pctk=9"
        );
    }

    #[test]
    fn the_range_in_progress_ends_where_its_note_is() {
        let r = [(0, 12, 40), (12, 15, 60), (15, 30, 90)];
        assert_eq!(range_end(&r, 10, 30), 12);
        assert_eq!(range_end(&r, 40, 30), 12);
        assert_eq!(range_end(&r, 41, 30), 15);
        assert_eq!(range_end(&r, 95, 30), 30);
    }

    /// A scripted system: a run fails when a "rare" line was seen and its schedule has C= at
    /// step 29. The state search reaches it from snapshots; it is a pure function of its inputs.
    #[test]
    fn the_search_is_deterministic_and_branches_from_its_states() {
        let p = Params {
            mix: 500,
            batch: 4,
            corpus: 4,
            spare: 8,
            rare: 2,
            snaps_per_run: 2,
            budget: 40,
            stop: false,
            stream_seed: stream_seed(1),
            first_seed: 1,
            steps: 30,
            timing: false,
        };
        let go = || {
            let mut fresh = |sd: u64| format!("disk=none dseed={sd:x} | run C={}", sd % 30);
            let mut branch = |e: &Entry, fs: u64| Some(format!("{} X={}", e.text, fs % 7));
            let mut run = |jobs: &[Job]| -> Vec<Outcome> {
                jobs.iter()
                    .map(|j| {
                        let rare = j.text.contains("dseed=3 ") || j.from.is_some();
                        Outcome {
                            failed: j.from.is_some() && j.text.ends_with("X=3"),
                            verdict: "OK".into(),
                            features: if rare {
                                vec!["line:rare".into()]
                            } else {
                                vec!["line:common".into()]
                            },
                            snaps:
                                j.names
                                    .iter()
                                    .take(1)
                                    .map(|n| Snap {
                                        name: n.clone(),
                                        stop: 5,
                                        at: 100,
                                        new: vec![
                                            if rare { "line:rare" } else { "line:common" }.into()
                                        ],
                                    })
                                    .collect(),
                            ranges: vec![(0, 10, 9)],
                            job_s: 1.0,
                            clock: 10,
                            invalid: None,
                        }
                    })
                    .collect()
            };
            search(&p, &mut fresh, &mut branch, &mut run)
        };
        let (a, b) = (go(), go());
        assert_eq!(a.made, b.made);
        assert!(a
            .made
            .iter()
            .any(|m| matches!(m.origin, Origin::Branch { .. })));
        assert!(a.corpus.len() <= 4);
        // with mix 0 the plain schedules in seed order
        let p0 = Params { mix: 0, ..p };
        let mut fresh = |sd: u64| format!("s{sd}");
        let mut branch = |_: &Entry, _: u64| None;
        let mut run =
            |jobs: &[Job]| -> Vec<Outcome> { jobs.iter().map(|_| Outcome::default()).collect() };
        let z = search(&p0, &mut fresh, &mut branch, &mut run);
        let texts: Vec<String> = z.made.iter().map(|m| m.text.clone()).collect();
        assert_eq!(texts, (1..=40).map(|i| format!("s{i}")).collect::<Vec<_>>());
    }
}
