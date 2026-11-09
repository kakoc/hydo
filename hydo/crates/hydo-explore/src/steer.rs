//! Steered search (`--steer`, M209; docs/design/steered-search.md): the explorer keeps the
//! schedules whose runs came closest to failing, by a number the system reports, and spends part
//! of its budget on small changes to them.
//!
//! The number is the best value of one run: the smallest (goal `min`) or the largest (goal
//! `max`) that the run's consoles report, either in the SDK's `HYDO-STEER` records of one id or
//! as the first capture group of a pattern over the console lines (a manifest's `steer:` entry,
//! or `--steer-match`). [`value_of`] reads it.
//!
//! The search ([`search`]) runs rounds of `batch` schedules. Each schedule of a round is a
//! mutant of a corpus entry with probability `mix`/1000 once the corpus has an entry, otherwise
//! the strategy's schedule of the next seed (as the plain exploration draws it, so with `mix` 0
//! the search runs the plain exploration's schedules in its order). The corpus ([`Corpus`])
//! keeps the `corpus` best runs; it changes only between rounds, and every choice comes from one
//! stream seeded by the exploration's seeds, so the search is a pure function of its options and
//! of the runs' results, whatever the number of workers. A mutant ([`mutate`]) is one change:
//! a step or a count moved a little, a seed drawn again, a fault removed, or a fault taken from
//! another seed's schedule.

use crate::rng::{mix64, SplitMix};
use crate::sched::{Pause, Tj, S};
use regex::Regex;
use std::collections::HashSet;

/// The default `--steer-mix`: mutants per thousand schedules once the corpus has an entry.
pub const MIX: u64 = 500;
/// The default `--steer-batch`: schedules per round.
pub const BATCH: u64 = 4;
/// The default `--steer-corpus`: the most schedules the corpus keeps.
pub const CORPUS: u64 = 16;
/// The salt of the search's own stream.
pub const SALT: u64 = 0x7374_6565_7273_6561;
/// How often a mutant is tried before the round takes a random schedule instead.
pub const TRIES: usize = 16;
/// The SDK's record tag.
pub const SDK_TAG: &str = "HYDO-STEER ";

/// Which way is closer to failing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    Min,
    Max,
}

impl Goal {
    pub fn parse(s: &str) -> Option<Goal> {
        match s {
            "min" => Some(Goal::Min),
            "max" => Some(Goal::Max),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Goal::Min => "min",
            Goal::Max => "max",
        }
    }

    /// `a` is strictly better than `b`.
    pub fn better(self, a: f64, b: f64) -> bool {
        match self {
            Goal::Min => a < b,
            Goal::Max => a > b,
        }
    }
}

/// Where the value comes from.
#[derive(Clone, Debug)]
pub enum Source {
    /// The SDK's `HYDO-STEER` records with this id.
    Sdk { id: String },
    /// The first capture group (the whole match without a group) of a pattern over every line.
    Pattern { id: String, re: Regex },
}

impl Source {
    pub fn id(&self) -> &str {
        match self {
            Source::Sdk { id } | Source::Pattern { id, .. } => id,
        }
    }
}

/// The best value in `texts` (consoles and the verdict line) and the goal the first SDK record
/// names (a pattern names none). `goal` decides "best"; without one the record's goal does, else
/// `min`.
pub fn value_of(src: &Source, goal: Option<Goal>, texts: &[&str]) -> (Option<f64>, Option<Goal>) {
    let mut vals: Vec<f64> = Vec::new();
    let mut named: Option<Goal> = None;
    for t in texts {
        for line in t.lines() {
            match src {
                Source::Sdk { id } => {
                    let Some(i) = line.find(SDK_TAG) else {
                        continue;
                    };
                    let Ok(v) =
                        serde_json::from_str::<serde_json::Value>(line[i + SDK_TAG.len()..].trim())
                    else {
                        continue;
                    };
                    if v.get("id").and_then(|x| x.as_str()) != Some(id.as_str()) {
                        continue;
                    }
                    if let Some(x) = v.get("v").and_then(|x| x.as_f64()) {
                        if x.is_finite() {
                            vals.push(x);
                        }
                    }
                    if named.is_none() {
                        named = v.get("goal").and_then(|g| g.as_str()).and_then(Goal::parse);
                    }
                }
                Source::Pattern { re, .. } => {
                    for c in re.captures_iter(line) {
                        let m = c.get(1).or_else(|| c.get(0)).map(|m| m.as_str());
                        if let Some(x) = m.and_then(|m| m.trim().parse::<f64>().ok()) {
                            if x.is_finite() {
                                vals.push(x);
                            }
                        }
                    }
                }
            }
        }
    }
    let g = goal.or(named).unwrap_or(Goal::Min);
    let best = vals
        .into_iter()
        .reduce(|a, b| if g.better(b, a) { b } else { a });
    (best, named)
}

/// One kept run.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The run number (1 is the first schedule).
    pub run: u64,
    pub tag: String,
    pub s: S,
    pub text: String,
    pub value: f64,
    /// How often it was chosen as a parent.
    pub picks: u64,
}

/// The best runs, best first (ties: the earlier run first).
#[derive(Clone, Debug, Default)]
pub struct Corpus {
    pub entries: Vec<Entry>,
    pub max: usize,
}

impl Corpus {
    pub fn new(max: usize) -> Corpus {
        Corpus {
            entries: Vec::new(),
            max: max.max(1),
        }
    }

    /// Keeps `e` when it is among the `max` best; false when it was not kept.
    pub fn offer(&mut self, e: Entry, goal: Goal) -> bool {
        if self.entries.iter().any(|x| x.text == e.text) {
            return false;
        }
        let at = self
            .entries
            .iter()
            .position(|x| goal.better(e.value, x.value))
            .unwrap_or(self.entries.len());
        if at >= self.max {
            return false;
        }
        self.entries.insert(at, e);
        self.entries.truncate(self.max);
        true
    }

    /// A parent: the entry of rank r (0 the best) chosen with weight 1 / ((r + 1)(picks + 1)),
    /// so the best entries are chosen most and an entry chosen often gives way to the others.
    pub fn pick(&mut self, rng: &mut SplitMix) -> usize {
        let w: Vec<i64> = self
            .entries
            .iter()
            .enumerate()
            .map(|(r, e)| (1_000_000 / ((r as i64 + 1) * (e.picks as i64 + 1))).max(1))
            .collect();
        let mut x = rng.below(w.iter().sum());
        let mut i = 0;
        while x >= w[i] {
            x -= w[i];
            i += 1;
        }
        self.entries[i].picks += 1;
        i
    }
}

/// How a slot's value moves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Move {
    /// A step, a count, a percentage: by 1 to 3.
    Step,
    /// A length in branches, exits or milliseconds: by about a tenth, or doubled or halved.
    Scale,
}

struct Slot<'a> {
    v: &'a mut i64,
    how: Move,
    min: i64,
}

fn slot(v: &mut i64, how: Move, min: i64) -> Slot<'_> {
    Slot { v, how, min }
}

/// Every whole number of `s` a mutant may move.
fn slots(s: &mut S) -> Vec<Slot<'_>> {
    use Move::*;
    let mut v = Vec::new();
    if let Some((_, at, down)) = s.k.as_mut() {
        v.push(slot(at, Step, 0));
        v.push(slot(down, Step, 1));
    }
    if let Some((_, a, b, _)) = s.n.as_mut() {
        v.push(slot(a, Step, 0));
        v.push(slot(b, Step, 0));
    }
    if let Some(c) = s.c.as_mut() {
        v.push(slot(c, Step, 0));
    }
    match s.p.as_mut() {
        Some(Pause::Plus(_, at, n)) => {
            v.push(slot(at, Step, 0));
            v.push(slot(n, Step, 1));
        }
        Some(Pause::Timed(_, at, off, dur)) => {
            v.push(slot(at, Step, 0));
            v.push(slot(off, Scale, 0));
            v.push(slot(dur, Scale, 1));
        }
        None => {}
    }
    if let Some((at, _)) = s.t.as_mut() {
        v.push(slot(at, Step, 0));
    }
    if let Some((_, a, b, _, _)) = s.o.as_mut() {
        v.push(slot(a, Step, 0));
        v.push(slot(b, Step, 0));
    }
    if let Some((n, _)) = s.cq.as_mut() {
        v.push(slot(n, Step, 1));
    }
    if let Some((_, at, off)) = s.x.as_mut() {
        v.push(slot(at, Step, 0));
        v.push(slot(off, Scale, 0));
    }
    if let Some(f) = s.f.as_mut() {
        v.push(slot(&mut f.step, Step, 0));
        v.push(slot(&mut f.n, Step, 1));
    }
    for h in s.h.iter_mut() {
        v.push(slot(&mut h.a, Step, 0));
        v.push(slot(&mut h.b, Step, 0));
    }
    if let Some(Tj::Seed { pm, max, .. }) = s.tj.as_mut() {
        v.push(slot(pm, Scale, 1));
        v.push(slot(max, Scale, 1));
    }
    if let Some(pp) = s.pp.as_mut() {
        v.push(slot(pp, Scale, 1024));
    }
    if let Some((_, rate, _)) = s.xt.as_mut() {
        v.push(slot(rate, Step, 1));
    }
    if let Some((_, n, k)) = s.pct.as_mut() {
        v.push(slot(n, Step, 1));
        v.push(slot(k, Scale, 1));
    }
    if let Some(cf) = s.cf.as_mut() {
        v.push(slot(cf, Step, 1));
    }
    if let Some((q, _)) = s.q.as_mut() {
        if *q != 0 {
            v.push(slot(q, Scale, 64));
        }
    }
    for a in s.a.iter_mut() {
        v.push(slot(&mut a.at, Step, 0));
        v.push(slot(&mut a.delay, Scale, 0));
    }
    if let Some(g) = s.g.as_mut() {
        v.push(slot(&mut g.at, Step, 0));
        if let Some(d) = g.down.as_mut() {
            v.push(slot(d, Step, 1));
        }
    }
    if let Some((_, at, down)) = s.l.as_mut() {
        v.push(slot(at, Step, 0));
        v.push(slot(down, Step, 1));
    }
    v
}

/// Every seed of `s` a mutant may draw again.
fn seeds(s: &mut S) -> Vec<&mut u64> {
    let mut v = Vec::new();
    if s.disk != "none" {
        v.push(&mut s.dseed);
    }
    if let Some(Tj::Seed { seed, .. }) = s.tj.as_mut() {
        v.push(seed);
    }
    if let Some((seed, _, _)) = s.xt.as_mut() {
        v.push(seed);
    }
    if let Some((seed, _, _)) = s.pct.as_mut() {
        v.push(seed);
    }
    if let Some((_, qs)) = s.q.as_mut() {
        if *qs != 0 {
            v.push(qs);
        }
    }
    if let Some((_, _, _, qbs)) = s.qb.as_mut() {
        v.push(qbs);
    }
    v
}

/// The parts of a schedule a mutant removes or takes from another seed's schedule.
const PARTS: [&str; 24] = [
    "disk", "K", "N", "C", "P", "T", "O", "Q", "X", "B", "F", "H", "W", "A", "G", "L", "knobs",
    "tj", "pp", "xt", "pct", "cf", "q", "qb",
];

fn has(s: &S, part: &str) -> bool {
    match part {
        "disk" => s.disk != "none",
        "K" => s.k.is_some(),
        "N" => s.n.is_some(),
        "C" => s.c.is_some(),
        "P" => s.p.is_some(),
        "T" => s.t.is_some(),
        "O" => s.o.is_some(),
        "Q" => s.cq.is_some(),
        "X" => s.x.is_some(),
        "B" => s.b.is_some(),
        "F" => s.f.is_some(),
        "H" => !s.h.is_empty(),
        "W" => s.w.is_some(),
        "A" => !s.a.is_empty(),
        "G" => s.g.is_some(),
        "L" => s.l.is_some(),
        "knobs" => !s.knobs.is_empty(),
        "tj" => s.tj.is_some(),
        "pp" => s.pp.is_some(),
        "xt" => s.xt.is_some(),
        "pct" => s.pct.is_some(),
        "cf" => s.cf.is_some(),
        "q" => s.q.is_some(),
        "qb" => s.qb.is_some(),
        _ => false,
    }
}

/// `to`'s part becomes `from`'s (removed when `from` lacks it).
fn take(to: &mut S, from: &S, part: &str) {
    match part {
        "disk" => {
            to.disk = from.disk.clone();
            to.dseed = from.dseed;
        }
        "K" => to.k = from.k.clone(),
        "N" => to.n = from.n.clone(),
        "C" => to.c = from.c,
        "P" => to.p = from.p.clone(),
        "T" => to.t = from.t,
        "O" => to.o = from.o.clone(),
        "Q" => to.cq = from.cq,
        "X" => to.x = from.x.clone(),
        "B" => to.b = from.b.clone(),
        "F" => to.f = from.f.clone(),
        "H" => to.h = from.h.clone(),
        "W" => to.w = from.w.clone(),
        "A" => to.a = from.a.clone(),
        "G" => to.g = from.g.clone(),
        "L" => to.l = from.l.clone(),
        "knobs" => to.knobs = from.knobs.clone(),
        "tj" => to.tj = from.tj.clone(),
        "pp" => to.pp = from.pp,
        "xt" => to.xt = from.xt,
        "pct" => to.pct = from.pct,
        "cf" => to.cf = from.cf,
        "q" => to.q = from.q,
        "qb" => to.qb = from.qb,
        _ => {}
    }
}

/// One change to `parent`: a step or a count moved (four tries in ten), a seed drawn again
/// (two), a part removed (one and a half) or a part taken from `donor`, another seed's schedule
/// of the same strategy (two and a half). None when the change found nothing to change (the
/// caller tries again).
pub fn mutate(parent: &S, donor: &S, rng: &mut SplitMix) -> Option<S> {
    let mut s = parent.clone();
    let op = rng.below(1000);
    if op < 400 {
        let mut v = slots(&mut s);
        if v.is_empty() {
            return None;
        }
        let i = rng.below(v.len() as i64) as usize;
        let sl = &mut v[i];
        let old = *sl.v;
        let new = match sl.how {
            Move::Step => {
                let d = if rng.below(10) < 7 { 1 } else { rng.rng(2, 3) };
                if rng.below(2) == 0 {
                    old + d
                } else {
                    old - d
                }
            }
            Move::Scale => match rng.below(4) {
                0 => old.saturating_mul(2),
                1 => old / 2,
                _ => {
                    let d = (old.abs() / 10).max(1);
                    if rng.below(2) == 0 {
                        old + d
                    } else {
                        old - d
                    }
                }
            },
        };
        *sl.v = new.max(sl.min);
        order(&mut s);
    } else if op < 600 {
        let mut v = seeds(&mut s);
        if v.is_empty() {
            return None;
        }
        let i = rng.below(v.len() as i64) as usize;
        *v[i] = rng.next();
    } else if op < 750 {
        let present: Vec<&str> = PARTS.iter().copied().filter(|p| has(&s, p)).collect();
        if present.is_empty() {
            return None;
        }
        let p = present[rng.below(present.len() as i64) as usize];
        take(&mut s, &S::new("none", 0), p);
    } else {
        let offered: Vec<&str> = PARTS.iter().copied().filter(|p| has(donor, p)).collect();
        if offered.is_empty() {
            return None;
        }
        let p = offered[rng.below(offered.len() as i64) as usize];
        take(&mut s, donor, p);
    }
    (s != *parent).then_some(s)
}

/// A window's ends in order after a move.
fn order(s: &mut S) {
    if let Some((_, a, b, _)) = s.n.as_mut() {
        if *a > *b {
            std::mem::swap(a, b);
        }
    }
    if let Some((_, a, b, _, _)) = s.o.as_mut() {
        if *a > *b {
            std::mem::swap(a, b);
        }
    }
    for h in s.h.iter_mut() {
        if h.a > h.b {
            std::mem::swap(&mut h.a, &mut h.b);
        }
    }
}

/// A mutant's change in words: the schedule's words it lost and gained (`pctn=39 -> pctn=40`
/// for one of each).
pub fn change(parent: &str, child: &str) -> String {
    let words = |t: &str| -> Vec<String> { t.split_whitespace().map(str::to_string).collect() };
    let (a, b) = (words(parent), words(child));
    let lost: Vec<&String> = a.iter().filter(|w| !b.contains(w)).collect();
    let got: Vec<&String> = b.iter().filter(|w| !a.contains(w)).collect();
    let join = |v: &[&String]| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
    match (lost.len(), got.len()) {
        (0, 0) => "the same words in another order".into(),
        (0, _) => format!("added {}", join(&got)),
        (_, 0) => format!("removed {}", join(&lost)),
        _ => format!("{} -> {}", join(&lost), join(&got)),
    }
}

/// The search's options.
#[derive(Clone, Debug)]
pub struct Params {
    /// None: the first SDK record's goal, else `min`.
    pub goal: Option<Goal>,
    pub mix: u64,
    pub batch: usize,
    pub corpus: usize,
    /// First runs in all.
    pub budget: usize,
    /// Stop after the round of the first failure.
    pub stop: bool,
    pub stream_seed: u64,
    /// The seed of the first random schedule (`--seeds LO-HI`'s LO); the next ones follow it.
    pub first_seed: u64,
}

/// The stream seed of an exploration whose seeds start at `lo`.
pub fn stream_seed(lo: u64) -> u64 {
    mix64(lo ^ SALT)
}

/// What one run gave the search.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub value: Option<f64>,
    /// The goal its SDK records name.
    pub goal: Option<Goal>,
    /// It failed (not OK, not refused).
    pub failed: bool,
}

/// Where a schedule came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Random { seed: u64 },
    Mutant { parent: String, change: String },
}

/// One first run of the search.
#[derive(Clone, Debug)]
pub struct Made {
    pub run: u64,
    pub tag: String,
    pub text: String,
    pub origin: Origin,
    pub outcome: Outcome,
    /// Its value was better than every earlier run's.
    pub best: bool,
}

/// The search's answer.
#[derive(Clone, Debug)]
pub struct Found {
    pub made: Vec<Made>,
    pub goal: Goal,
    pub corpus: Vec<Entry>,
    pub stopped: bool,
}

/// One round of runs: the tags and schedules, in, the outcomes in the same order, out.
pub type Runner<'a> = dyn FnMut(&[(String, String)]) -> Vec<Outcome> + 'a;

/// The search. `fresh(seed)` is the strategy's schedule of a seed; `render(s)` its text and the
/// schedule read back from it (None when the text does not read); `run` runs one round (each
/// schedule once) and returns the outcomes in the same order. Tags are `seed-<run number>`.
pub fn search(
    p: &Params,
    fresh: &mut dyn FnMut(u64) -> S,
    render: &mut dyn FnMut(&S) -> (String, Option<S>),
    run: &mut Runner<'_>,
) -> Found {
    let mut rng = SplitMix::new(p.stream_seed);
    let mut seen: HashSet<String> = HashSet::new();
    let mut corpus = Corpus::new(p.corpus);
    let mut goal = p.goal;
    let mut next_seed = p.first_seed;
    let mut made: Vec<Made> = Vec::new();
    let mut best: Option<f64> = None;
    let mut stopped = false;
    while made.len() < p.budget {
        let n = p.batch.max(1).min(p.budget - made.len());
        let mut round: Vec<(String, String, S, Origin)> = Vec::new();
        for _ in 0..n {
            let run_no = made.len() + round.len() + 1;
            let mut got = None;
            if !corpus.entries.is_empty() && (rng.below(1000) as u64) < p.mix {
                for _ in 0..TRIES {
                    let i = corpus.pick(&mut rng);
                    let donor = fresh(rng.next());
                    let parent = &corpus.entries[i];
                    let Some(child) = mutate(&parent.s, &donor, &mut rng) else {
                        continue;
                    };
                    let (text, back) = render(&child);
                    let Some(back) = back else { continue };
                    if text == parent.text || seen.contains(&text) {
                        continue;
                    }
                    let origin = Origin::Mutant {
                        parent: parent.tag.clone(),
                        change: change(&parent.text, &text),
                    };
                    got = Some((text, back, origin));
                    break;
                }
            }
            let (text, s, origin) = got.unwrap_or_else(|| {
                let sd = next_seed;
                next_seed += 1;
                let s0 = fresh(sd);
                let (text, back) = render(&s0);
                (text, back.unwrap_or(s0), Origin::Random { seed: sd })
            });
            seen.insert(text.clone());
            round.push((format!("seed-{run_no}"), text, s, origin));
        }
        let items: Vec<(String, String)> =
            round.iter().map(|r| (r.0.clone(), r.1.clone())).collect();
        let outs = run(&items);
        let mut failed = false;
        for ((tag, text, s, origin), o) in round.into_iter().zip(outs) {
            let run_no = made.len() as u64 + 1;
            if goal.is_none() && o.value.is_some() {
                goal = o.goal;
            }
            let g = goal.unwrap_or(Goal::Min);
            let is_best = o.value.is_some_and(|v| best.is_none_or(|b| g.better(v, b)));
            if is_best {
                best = o.value;
            }
            if let Some(v) = o.value {
                corpus.offer(
                    Entry {
                        run: run_no,
                        tag: tag.clone(),
                        s,
                        text: text.clone(),
                        value: v,
                        picks: 0,
                    },
                    g,
                );
            }
            failed |= o.failed;
            made.push(Made {
                run: run_no,
                tag,
                text,
                origin,
                outcome: o,
                best: is_best,
            });
        }
        if p.stop && failed {
            stopped = made.len() < p.budget;
            break;
        }
    }
    Found {
        made,
        goal: goal.unwrap_or(Goal::Min),
        corpus: corpus.entries,
        stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(re: &str) -> Source {
        Source::Pattern {
            id: "match".into(),
            re: Regex::new(re).unwrap(),
        }
    }

    #[test]
    fn a_pattern_gives_the_best_of_every_line() {
        let src = pat(r"length: (\d+)");
        let t = "hydo-qdrant: ShardUpdateQueueInfo { length: 18, op_num: Some(481) }\n\
                 noise\nverdict ... after load: { length: 7, op_num: x }";
        assert_eq!(value_of(&src, None, &[t]), (Some(7.0), None));
        assert_eq!(value_of(&src, Some(Goal::Max), &[t]), (Some(18.0), None));
        assert_eq!(value_of(&src, None, &["nothing here"]), (None, None));
        // no group: the whole match
        assert_eq!(
            value_of(&pat(r"-?\d+\.\d+"), None, &["a 2.5 b -1.25"]).0,
            Some(-1.25)
        );
    }

    #[test]
    fn sdk_records_of_one_id_and_their_goal() {
        let src = Source::Sdk { id: "q.len".into() };
        let t = "x HYDO-STEER {\"id\":\"q.len\",\"goal\":\"max\",\"v\":3,\"msg\":\"m\"}\n\
                 HYDO-STEER {\"id\":\"other\",\"goal\":\"min\",\"v\":100}\n\
                 HYDO-STEER {\"id\":\"q.len\",\"goal\":\"max\",\"v\":9}\n\
                 HYDO-STEER {broken";
        assert_eq!(value_of(&src, None, &[t]), (Some(9.0), Some(Goal::Max)));
        assert_eq!(
            value_of(&src, Some(Goal::Min), &[t]),
            (Some(3.0), Some(Goal::Max))
        );
    }

    fn entry(run: u64, v: f64) -> Entry {
        Entry {
            run,
            tag: format!("seed-{run}"),
            s: S::new("none", run),
            text: format!("t{run}"),
            value: v,
            picks: 0,
        }
    }

    #[test]
    fn the_corpus_keeps_the_best_in_order() {
        let mut c = Corpus::new(3);
        for (r, v) in [(1, 5.0), (2, 3.0), (3, 9.0), (4, 3.0), (5, 1.0)] {
            c.offer(entry(r, v), Goal::Min);
        }
        let got: Vec<(u64, f64)> = c.entries.iter().map(|e| (e.run, e.value)).collect();
        assert_eq!(got, vec![(5, 1.0), (2, 3.0), (4, 3.0)]);
        assert!(!c.offer(entry(6, 8.0), Goal::Min));
        assert!(!c.offer(entry(5, 0.0), Goal::Min), "the same text twice");
        let mut m = Corpus::new(2);
        for (r, v) in [(1, 5.0), (2, 3.0), (3, 9.0)] {
            m.offer(entry(r, v), Goal::Max);
        }
        assert_eq!(m.entries[0].run, 3);
    }

    #[test]
    fn picks_favour_the_best_and_spread() {
        let mut c = Corpus::new(4);
        for r in 1..=4 {
            c.offer(entry(r, r as f64), Goal::Min);
        }
        let mut rng = SplitMix::new(7);
        let mut n = [0u32; 4];
        for _ in 0..400 {
            n[c.pick(&mut rng)] += 1;
        }
        assert!(n[0] >= n[1] && n[1] >= n[3], "{n:?}");
        assert!(n.iter().all(|&k| k > 0), "{n:?}");
        // the same stream picks the same entries
        let mut c2 = Corpus::new(4);
        for r in 1..=4 {
            c2.offer(entry(r, r as f64), Goal::Min);
        }
        let mut c3 = c2.clone();
        let (mut a, mut b) = (SplitMix::new(3), SplitMix::new(3));
        let x: Vec<usize> = (0..50).map(|_| c2.pick(&mut a)).collect();
        let y: Vec<usize> = (0..50).map(|_| c3.pick(&mut b)).collect();
        assert_eq!(x, y);
    }

    fn sched() -> S {
        let mut s = S::new("cut", 11);
        s.k = Some(("redis".into(), 5, 2));
        s.pct = Some((0xec69, 39, 3000));
        s
    }

    #[test]
    fn a_mutant_is_one_change_and_a_function_of_the_stream() {
        let parent = sched();
        let mut donor = S::new("none", 0);
        donor.t = Some((12, 300));
        let mut kinds = HashSet::new();
        for seed in 0..300 {
            let mut r1 = SplitMix::new(seed);
            let mut r2 = SplitMix::new(seed);
            let a = mutate(&parent, &donor, &mut r1);
            assert_eq!(a, mutate(&parent, &donor, &mut r2));
            let Some(m) = a else { continue };
            assert_ne!(m, parent);
            if m.t.is_some() {
                kinds.insert("graft");
            }
            if m.k.is_none() || m.pct.is_none() || m.disk == "none" {
                kinds.insert("drop");
            }
            if m.dseed != 11 || m.pct.is_some_and(|p| p.0 != 0xec69) {
                kinds.insert("reseed");
            }
            if m.pct.is_some_and(|p| p.1 != 39) || m.k.as_ref().is_some_and(|k| k.1 != 5) {
                kinds.insert("move");
            }
            if let Some(k) = &m.k {
                assert!(k.1 >= 0 && k.2 >= 1);
            }
        }
        assert_eq!(kinds.len(), 4, "{kinds:?}");
    }

    #[test]
    fn a_change_in_words() {
        assert_eq!(
            change(
                "disk=none pct=1 pctn=39 pctk=3000 | run",
                "disk=none pct=1 pctn=40 pctk=3000 | run"
            ),
            "pctn=39 -> pctn=40"
        );
        assert_eq!(change("a | run", "a | run T=12:300"), "added T=12:300");
        assert_eq!(change("a | run K=x@1+1", "a | run"), "removed K=x@1+1");
    }

    /// A scripted system: the value is |K step - 17| + |K down - 3| when there is a K=, else 40;
    /// it fails at 0. The random schedules draw K= with a step in 0..30 and a down time in 1..9.
    fn scripted(p: &Params) -> Found {
        let mut fresh = |seed: u64| {
            let mut r = SplitMix::new(seed);
            let mut s = S::new("none", seed);
            if r.below(2) == 0 {
                s.k = Some(("redis".into(), r.rng(0, 29), r.rng(1, 9)));
            }
            s
        };
        let mut render = |s: &S| {
            let t = match &s.k {
                Some((_, a, d)) => format!("disk=none dseed={} | run K=redis@{a}+{d}", s.dseed),
                None => format!("disk=none dseed={} | run", s.dseed),
            };
            (t, Some(s.clone()))
        };
        let mut run = |items: &[(String, String)]| -> Vec<Outcome> {
            items
                .iter()
                .map(|(_, t)| {
                    let v = match t.split_once("K=redis@") {
                        Some((_, k)) => {
                            let (a, d) = k.split_once('+').unwrap();
                            let (a, d): (i64, i64) = (a.parse().unwrap(), d.parse().unwrap());
                            ((a - 17).abs() + (d - 3).abs()) as f64
                        }
                        None => 40.0,
                    };
                    Outcome {
                        value: Some(v),
                        goal: None,
                        failed: v == 0.0,
                    }
                })
                .collect()
        };
        search(p, &mut fresh, &mut render, &mut run)
    }

    fn params(mix: u64, lo: u64) -> Params {
        Params {
            goal: None,
            mix,
            batch: 4,
            corpus: 16,
            budget: 300,
            stop: true,
            stream_seed: stream_seed(lo),
            first_seed: lo,
        }
    }

    fn first_failure(f: &Found) -> Option<u64> {
        f.made.iter().find(|m| m.outcome.failed).map(|m| m.run)
    }

    #[test]
    fn the_search_is_a_function_of_its_options() {
        let a = scripted(&params(500, 1));
        let b = scripted(&params(500, 1));
        let ta: Vec<&String> = a.made.iter().map(|m| &m.text).collect();
        let tb: Vec<&String> = b.made.iter().map(|m| &m.text).collect();
        assert_eq!(ta, tb);
        assert_eq!(a.goal, Goal::Min);
        // the first round has no corpus yet: four random schedules of seeds 1..4
        let first: Vec<&Origin> = a.made.iter().take(4).map(|m| &m.origin).collect();
        assert_eq!(
            first,
            (1..=4)
                .map(|s| Origin::Random { seed: s })
                .collect::<Vec<_>>()
                .iter()
                .collect::<Vec<_>>()
        );
        // the stop ends the search after the failing round
        assert!(a.stopped);
        assert_eq!(a.made.len() % 4, 0);
        assert!(first_failure(&a).is_some());
    }

    #[test]
    fn without_mutants_it_is_the_random_exploration() {
        let f = scripted(&params(0, 1));
        for (i, m) in f.made.iter().enumerate() {
            assert_eq!(m.origin, Origin::Random { seed: i as u64 + 1 });
            assert_eq!(m.tag, format!("seed-{}", i + 1));
        }
    }

    /// On the scripted system steering finds the failure in fewer runs than random schedules do,
    /// for each of ten stream seeds (the random search's runs to its first failure are counted
    /// over the same seeds).
    #[test]
    fn steering_helps_where_the_value_leads_to_the_failure() {
        let (mut st, mut rnd) = (Vec::new(), Vec::new());
        for k in 0..10u64 {
            let lo = 1 + 1000 * k;
            st.push(first_failure(&scripted(&params(500, lo))).unwrap_or(301));
            rnd.push(first_failure(&scripted(&params(0, lo))).unwrap_or(301));
        }
        st.sort_unstable();
        rnd.sort_unstable();
        assert!(st[5] < rnd[5], "steered {st:?} random {rnd:?}");
    }
}
