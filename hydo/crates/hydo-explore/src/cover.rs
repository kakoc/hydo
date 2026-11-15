//! Coverage of the fakes' and proxies' routes by step (`--protocol-cover`, Milestone 257).
//!
//! `--protocol-faults` draws each `H=` token's target and step at random, so a bug that needs one
//! route faulted in one step can stay unreached for many schedules: the web-shop example's double
//! order needs `H=api.place_order@0-1:drop`, and 40 drawn schedules put their `place_order`
//! tokens into step 1, which places no order. The cover walks the cells instead. A cell is a
//! target (a fake's route, a proxy, or a proxy's route), a step, and a rule kind.
//!
//! - **The probe.** Before the first schedule the explorer runs a token on every fake and proxy in
//!   every step, each with a delay of `step + 1` ms ([`probe_runs`]). The world's record at the
//!   end of the run (`hydo-world: <seq> <fake>.<route> ... ok+delay<d>`) then says which routes
//!   each step called ([`called`]). A schedule's guest part holds at most 500 characters, so the
//!   tokens are packed into up to [`PROBE_RUNS_MAX`] schedules; a project that needs more is
//!   walked unprobed, with every pair counted as called.
//! - **The order** ([`plan`]). The (target, step) pairs that the probe saw called come first,
//!   step by step in the workload's order, then the pairs it did not see. Round `r` gives pair `j`
//!   of a part its kind `(r + j) mod K`, where `K` is the number of the target's rule kinds. So
//!   the first round faults every called pair once, with kinds spread over the pairs, and after
//!   `K` rounds every called pair had every kind.
//! - **The draw** ([`crate::gen::Ex::add_protocol_cover`]). The schedule of seed `s` takes cell
//!   `(s - 1) mod C`, where `C` is the number of cells, so any `C` consecutive seeds reach every
//!   cell once. The rule's values (the status code, `effect=`, the delay) come from the cover's
//!   own stream ([`COVER_SALT`]), and `p=` is never drawn: the rule acts on every matching
//!   request of its step. The token is the schedule's first `H=` token, so with
//!   `--protocol-faults` a drawn token on the same route never hides it.
//!
//! A schedule stays a pure function of its seed and of the probe's record, which is itself a run
//! of the image: `hydo explore --seeds 7-7 --protocol-cover` gives seed 7's schedule again.
//! Without the option nothing here runs and every drawn schedule is the same as before.

use crate::sched::{render_h, H, RV};
use crate::space::PfTarget;
use std::collections::BTreeSet;

/// The cover's own stream: the values of each cell's rule.
pub const COVER_SALT: u64 = 0x434F_5645_525F_5046;
/// The most probe runs; a project whose probe needs more is walked unprobed.
pub const PROBE_RUNS_MAX: usize = 4;
/// The tag of the probe's runs: `cover-probe-<n>`, from 1.
pub const PROBE_TAG: &str = "cover-probe";

/// One cell of the walk: a target (an index into the explorer's targets), a step and a kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub target: usize,
    pub step: i64,
    pub kind: String,
}

/// The walk: its cells in order, and what the probe found.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Cover {
    pub cells: Vec<Cell>,
    /// The (target, step) pairs.
    pub pairs: usize,
    /// The pairs that the probe saw called; `None` without a probe or without a record.
    pub called: Option<usize>,
    /// How many cells the called pairs have (they come first).
    pub called_cells: usize,
}

/// A target's rule kinds, in the order `--protocol-faults` draws them.
pub fn kinds(t: &PfTarget) -> Vec<String> {
    let mut k = t.req.clone();
    k.extend(t.hook.iter().cloned());
    k
}

/// The fakes and proxies of `targets` that take a delay, in order: the part of each target's
/// name before its first dot.
pub fn endpoints(targets: &[PfTarget]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in targets
        .iter()
        .filter(|t| t.req.iter().any(|k| k == "delay"))
    {
        let ep = t.name.split('.').next().unwrap_or(&t.name).to_string();
        if !out.contains(&ep) {
            out.push(ep);
        }
    }
    out
}

/// The probe: on every fake and proxy, in every step, `delay=<step + 1>`, packed in order into
/// schedules whose guest part fits ([`crate::compose::GUEST_MAX`]). Empty when that takes more
/// than [`PROBE_RUNS_MAX`] schedules.
pub fn probe_runs(targets: &[PfTarget], steps: i64) -> Vec<Vec<H>> {
    let mut runs: Vec<Vec<H>> = Vec::new();
    let mut len = 0;
    for step in 0..steps {
        for ep in endpoints(targets) {
            let h = H {
                target: ep,
                a: step,
                b: step + 1,
                rules: vec![("delay".into(), RV::Int(step + 1))],
            };
            let n = 1 + render_h(&h).len();
            if runs.is_empty() || len + n > crate::compose::GUEST_MAX {
                runs.push(Vec::new());
                len = "run".len();
            }
            len += n;
            runs.last_mut().expect("a run").push(h);
        }
    }
    if runs.len() > PROBE_RUNS_MAX {
        return Vec::new();
    }
    runs
}

/// The (target, step) pairs that a probe's consoles show called: every record line
/// `hydo-world: <seq> <fake>.<route> ... <outcome>` whose outcome holds `+delay<d>` was made in
/// step `d - 1`. A target `<proxy>` (without a route) is called when any of its routes is. When
/// the record was cut (`... N more records`), every pair after the last step seen counts as
/// called. `None`: the consoles hold no record line at all.
pub fn called(texts: &[&str], targets: &[PfTarget], steps: i64) -> Option<BTreeSet<(usize, i64)>> {
    let mut seen = false;
    let mut cut = false;
    let mut out = BTreeSet::new();
    for text in texts {
        for line in text.lines() {
            let Some(rest) = line.trim_end_matches('\r').split("hydo-world: ").nth(1) else {
                continue;
            };
            let words: Vec<&str> = rest.split_whitespace().collect();
            if words.first() == Some(&"...") && words.last() == Some(&"records") {
                cut = true;
                continue;
            }
            if words.len() < 3 || words[0].parse::<u64>().is_err() {
                continue;
            }
            seen = true;
            let name = words[1];
            let outcome = words[words.len() - 1];
            let Some(step) = outcome
                .split('+')
                .find_map(|p| p.strip_prefix("delay"))
                .and_then(|d| d.parse::<i64>().ok())
                .map(|d| d - 1)
            else {
                continue;
            };
            if !(0..steps).contains(&step) {
                continue;
            }
            for (i, t) in targets.iter().enumerate() {
                let of_proxy = !t.name.contains('.')
                    && name.len() > t.name.len()
                    && name.starts_with(&t.name)
                    && name.as_bytes()[t.name.len()] == b'.';
                if t.name == name || of_proxy {
                    out.insert((i, step));
                }
            }
        }
    }
    if !seen {
        return None;
    }
    if cut {
        let last = out.iter().map(|(_, s)| *s).max().unwrap_or(-1);
        for step in last + 1..steps {
            for i in 0..targets.len() {
                out.insert((i, step));
            }
        }
    }
    Some(out)
}

/// The walk over `targets` and `steps` (module docs): the called pairs' cells first, then the
/// others'. `called: None` counts every pair as called.
pub fn plan(targets: &[PfTarget], steps: i64, called: Option<&BTreeSet<(usize, i64)>>) -> Cover {
    let mut on = Vec::new();
    let mut off = Vec::new();
    for step in 0..steps.max(0) {
        for i in 0..targets.len() {
            if called.is_none_or(|c| c.contains(&(i, step))) {
                on.push((i, step));
            } else {
                off.push((i, step));
            }
        }
    }
    let walk = |pairs: &[(usize, i64)]| -> Vec<Cell> {
        let ks: Vec<Vec<String>> = pairs.iter().map(|(i, _)| kinds(&targets[*i])).collect();
        let rounds = ks.iter().map(Vec::len).max().unwrap_or(0);
        let mut cells = Vec::new();
        for r in 0..rounds {
            for (j, (i, step)) in pairs.iter().enumerate() {
                let k = &ks[j];
                if r < k.len() {
                    cells.push(Cell {
                        target: *i,
                        step: *step,
                        kind: k[(r + j) % k.len()].clone(),
                    });
                }
            }
        }
        cells
    };
    let mut cells = walk(&on);
    let called_cells = cells.len();
    cells.extend(walk(&off));
    Cover {
        cells,
        pairs: on.len() + off.len(),
        called: called.map(|_| on.len()),
        called_cells,
    }
}

/// The cell of seed `seed`: `(seed - 1) mod C`.
pub fn cell_of(c: &Cover, seed: u64) -> Option<&Cell> {
    if c.cells.is_empty() {
        return None;
    }
    c.cells
        .get((seed.wrapping_sub(1) % c.cells.len() as u64) as usize)
}

/// One line for the explorer's output and `protocol-cover.txt`'s head.
pub fn describe(c: &Cover, targets: &[PfTarget]) -> String {
    let probe = match c.called {
        Some(n) => format!(
            "the probe saw {n} of {} (route, step) pairs called; their {} cells come first",
            c.pairs, c.called_cells
        ),
        None => format!(
            "no probe record: all {} (route, step) pairs in step order",
            c.pairs
        ),
    };
    format!(
        "protocol cover: {} targets, {probe}; {} cells, so every cell within {} schedules",
        targets.len(),
        c.cells.len(),
        c.cells.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tg(name: &str, hook: bool) -> PfTarget {
        PfTarget {
            name: name.into(),
            req: ["status", "delay", "drop", "reset"]
                .map(String::from)
                .to_vec(),
            hook: if hook {
                ["dup", "reorder", "late", "lost"]
                    .map(String::from)
                    .to_vec()
            } else {
                Vec::new()
            },
            proto: "http".into(),
            statuses: Vec::new(),
        }
    }

    #[test]
    fn the_probe_has_one_token_per_fake_and_step() {
        let t = [
            tg("api.a", false),
            tg("api.b", false),
            tg("px", false),
            tg("px.r", false),
        ];
        assert_eq!(endpoints(&t), ["api", "px"]);
        let p = probe_runs(&t, 3);
        assert_eq!(p.len(), 1);
        let text: Vec<String> = p[0].iter().map(render_h).collect();
        assert_eq!(
            text.join(" "),
            "H=api@0-1:delay=1 H=px@0-1:delay=1 H=api@1-2:delay=2 H=px@1-2:delay=2 \
             H=api@2-3:delay=3 H=px@2-3:delay=3"
        );
        // 30 steps of two fakes: two runs, each guest part within the limit
        let p = probe_runs(&t, 30);
        assert_eq!(p.len(), 3);
        for r in &p {
            let g: Vec<String> = r.iter().map(render_h).collect();
            assert!(format!("run {}", g.join(" ")).len() <= crate::compose::GUEST_MAX);
        }
        assert_eq!(p.iter().map(Vec::len).sum::<usize>(), 60);
        // too many: no probe
        assert!(probe_runs(&t, 200).is_empty());
    }

    #[test]
    fn the_record_says_which_routes_each_step_called() {
        let t = [tg("api.a", false), tg("api.b", false), tg("px", false)];
        let console = "x\r\nhydo-world: 1 api.a ref=o1 idem=- fx=1 ok+delay1\r\n\
                       hydo-world: 2 api.b ref=o1 idem=- fx=1 replay+delay2\r\n\
                       hydo-world: 3 px.get ref=- idem=- fx=1 ok+delay2\r\n\
                       hydo-world: 4 webhook api.a ref=o1 src=1 n=1 200\r\n\
                       hydo-world: END\r\n";
        let c = called(&[console], &t, 3).unwrap();
        assert_eq!(
            c.into_iter().collect::<Vec<_>>(),
            vec![(0, 0), (1, 1), (2, 1)]
        );
        assert_eq!(called(&["no record"], &t, 3), None);
        // a record cut short: the steps after the last one seen count as called
        let cut = "hydo-world: 1 api.a ref=o1 idem=- fx=1 ok+delay1\n\
                   hydo-world: ... 900 more records\n";
        let c = called(&[cut], &t, 3).unwrap();
        assert_eq!(c.len(), 1 + 2 * 3);
    }

    #[test]
    fn the_walk_reaches_every_cell_called_pairs_first() {
        let t = [tg("api.a", false), tg("api.b", true)];
        let seen: BTreeSet<(usize, i64)> = [(0, 0), (1, 1)].into_iter().collect();
        let c = plan(&t, 2, Some(&seen));
        assert_eq!(c.pairs, 4);
        assert_eq!(c.called, Some(2));
        // api.a has 4 kinds, api.b 8 (its webhook), per pair
        assert_eq!(c.cells.len(), 2 * (4 + 8));
        assert_eq!(c.called_cells, 4 + 8);
        // the first round: each called pair once, kinds spread
        assert_eq!(
            (&c.cells[0].kind, c.cells[0].target, c.cells[0].step),
            (&"status".to_string(), 0, 0)
        );
        assert_eq!(
            (&c.cells[1].kind, c.cells[1].target, c.cells[1].step),
            (&"delay".to_string(), 1, 1)
        );
        // every cell once
        let mut all: Vec<(usize, i64, String)> = c
            .cells
            .iter()
            .map(|x| (x.target, x.step, x.kind.clone()))
            .collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), c.cells.len());
        // the called pairs' cells before any other
        assert!(c.cells[..c.called_cells]
            .iter()
            .all(|x| seen.contains(&(x.target, x.step))));
        // seed 1 is the first cell, and C seeds later the walk starts again
        let n = c.cells.len() as u64;
        assert_eq!(cell_of(&c, 1), Some(&c.cells[0]));
        assert_eq!(cell_of(&c, 1 + n), Some(&c.cells[0]));
        assert_eq!(cell_of(&c, n), c.cells.last());
        // no probe: every pair, in step order
        let all = plan(&t, 2, None);
        assert_eq!(all.called, None);
        assert_eq!(all.called_cells, all.cells.len());
        assert_eq!((all.cells[1].target, all.cells[1].step), (1, 0));
    }
}
