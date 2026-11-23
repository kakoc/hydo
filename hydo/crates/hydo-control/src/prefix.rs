//! The snapshots a worker keeps for shrinks (Milestone 340, docs/design/shrink-snapshots.md).
//!
//! A shrink candidate is the failing schedule with one fault left out or made weaker, so it runs
//! what an earlier candidate ran up to the step range where the two differ. A worker's runs of
//! shrink candidates take a snapshot at the start of each of their step ranges after the first
//! ([`crate::search::SnapPolicy::ranges`]), and a later candidate on the same worker continues
//! from the deepest of them that it reaches: the same host part (the engine's schedule), and its
//! controller sends exactly the commands of the snapshot's own run up to the snapshot
//! ([`crate::agent::check_branch`], the state search's offline check). This module is the book
//! of those snapshots; [`crate::agent::AgentWorker`] runs them.

use crate::search::RecStop;
use std::collections::HashMap;
use std::sync::Arc;

/// The snapshots one worker keeps on the Mac engine and KVM (they keep 64 names, the host's S0
/// and S1 among them, and each holds only the pages written since S1).
pub const KEEP: usize = 8;

/// The snapshots one worker keeps on engine `kind`: hvx keeps 8 names, S0 and S1 among them,
/// and each holds every page of guest RAM that is not zero, in host memory.
pub fn keep(kind: hydo_engine::EngineKind) -> usize {
    match kind {
        hydo_engine::EngineKind::Hvx => 2,
        _ => KEEP,
    }
}

/// The snapshots one run takes at most (its first step ranges after the first).
pub const PER_RUN: usize = 4;

/// One kept snapshot.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The engine's schedule of the run that took it (its host part and `run`).
    pub eng: String,
    /// The run's recorded stops up to and including the snapshot's.
    pub rec: Arc<Vec<RecStop>>,
    /// The guest clock from S1 to the snapshot.
    pub clock: u64,
    /// When it was last made or continued from (the book's own counter).
    pub used: u64,
    /// Its generation: a new snapshot under the same name gets a new one.
    pub gen: u64,
}

/// The book: one slot per snapshot name.
#[derive(Debug, Default)]
pub struct Store {
    slots: Vec<Option<Entry>>,
    tick: u64,
    gens: u64,
    /// Whether a schedule's controller reaches a slot's snapshot: (slot, generation, schedule).
    reach: HashMap<(usize, u64, String), bool>,
}

impl Store {
    pub fn new(keep: usize) -> Store {
        Store {
            slots: vec![None; keep],
            ..Store::default()
        }
    }

    /// The number of slots.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The engine's name of slot `i`.
    pub fn name(i: usize) -> String {
        format!("shrink-snap{i}")
    }

    pub fn entry(&self, i: usize) -> Option<&Entry> {
        self.slots.get(i).and_then(Option::as_ref)
    }

    /// The slot whose snapshot `full` (the run's schedule, with its cadence) continues from: the
    /// deepest by guest clock among those of the same engine schedule `eng` that `reaches` says
    /// its controller reaches. Each answer of `reaches` is kept per slot generation.
    pub fn best(
        &mut self,
        full: &str,
        eng: &str,
        mut reaches: impl FnMut(&[RecStop]) -> bool,
    ) -> Option<usize> {
        let mut c: Vec<(u64, usize)> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().filter(|e| e.eng == eng).map(|e| (e.clock, i)))
            .collect();
        c.sort_by(|a, b| b.cmp(a));
        for (_, i) in c {
            let e = self.slots[i].as_ref().expect("a kept snapshot");
            let key = (i, e.gen, full.to_string());
            let ok = match self.reach.get(&key) {
                Some(ok) => *ok,
                None => {
                    let ok = reaches(&e.rec);
                    self.reach.insert(key, ok);
                    ok
                }
            };
            if ok {
                return Some(i);
            }
        }
        None
    }

    /// The slots a run may take its snapshots in, at most `n`: the free ones first, then the
    /// least recently used, never `keep` (the snapshot the run continues from).
    pub fn slots_for_run(&self, keep: Option<usize>, n: usize) -> Vec<usize> {
        let mut c: Vec<(u64, usize)> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != keep)
            .map(|(i, e)| (e.as_ref().map(|e| e.used + 1).unwrap_or(0), i))
            .collect();
        c.sort();
        c.into_iter().take(n).map(|(_, i)| i).collect()
    }

    /// Slot `i` holds a new snapshot.
    pub fn put(&mut self, i: usize, eng: &str, rec: Arc<Vec<RecStop>>, clock: u64) {
        self.tick += 1;
        self.gens += 1;
        self.reach.retain(|k, _| k.0 != i);
        self.slots[i] = Some(Entry {
            eng: eng.to_string(),
            rec,
            clock,
            used: self.tick,
            gen: self.gens,
        });
    }

    /// A run continued from slot `i`.
    pub fn touch(&mut self, i: usize) {
        self.tick += 1;
        if let Some(e) = self.slots[i].as_mut() {
            e.used = self.tick;
        }
    }

    /// Slot `i`'s snapshot is no use (a continuation from it failed).
    pub fn drop_slot(&mut self, i: usize) {
        self.slots[i] = None;
        self.reach.retain(|k, _| k.0 != i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deepest_reached_snapshot_of_the_same_host_part() {
        let mut s = Store::new(4);
        let rec = Arc::new(Vec::new());
        s.put(0, "disk=none rb=64 | run", rec.clone(), 100);
        s.put(1, "disk=none rb=64 | run", rec.clone(), 300);
        s.put(2, "disk=cut rb=64 | run", rec.clone(), 900);
        let mut asked = 0;
        // slot 1 is deeper but not reached, slot 2 is of another host part
        let mut reach = |_: &[RecStop]| {
            asked += 1;
            asked != 1
        };
        assert_eq!(s.best("a", "disk=none rb=64 | run", &mut reach), Some(0));
        // the answers are kept
        assert_eq!(
            s.best("a", "disk=none rb=64 | run", |_| panic!("asked again")),
            Some(0)
        );
        assert_eq!(s.best("a", "disk=lying rb=64 | run", |_| true), None);
        // free slots first, then the least recently used, never the one continued from
        assert_eq!(s.slots_for_run(Some(0), 4), vec![3, 1, 2]);
        s.touch(1);
        assert_eq!(s.slots_for_run(None, 2), vec![3, 0]);
        // a new snapshot in a slot forgets the old answers
        s.put(1, "disk=none rb=64 | run", rec, 50);
        assert_eq!(s.entry(1).map(|e| e.clock), Some(50));
        s.drop_slot(0);
        assert_eq!(s.best("a", "disk=none rb=64 | run", |_| false), None);
    }
}
