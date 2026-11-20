//! Repeated schedules of one exploration (Milestone 339).
//!
//! The engine is deterministic, so a schedule that an exploration already ran gives the earlier
//! run's result again. The shrinker tries a candidate again in a later round, and an exploration
//! of a small fault space draws one schedule for many seeds
//! (`hydo/explore/RESEARCH-shared-prefix.md`, section "Shrinks and repeated schedules"). Such a
//! run takes the earlier run's result and a copy of its consoles, without a VM.
//!
//! - Only first runs (`seed-`) and shrink candidates (`shrink`, `errshrink-`) are answered so.
//!   The re-runs that prove a failure repeats (`repro-`) and the confirmations of a minimal
//!   schedule (`min-`, `errmin-`) keep their VMs, and so does every other run.
//! - A schedule whose run is still in flight is not started twice: the second one waits for the
//!   first one's result. A cancelled run that another run waits for still runs.
//! - A run that lost its engine (`ERROR worker`) is no result to repeat.
//! - The key is the schedule's whole text. Two texts that differ only in a `dseed=` that decides
//!   nothing are one text since Milestone 339, because the explorer writes it as 0
//!   ([`crate::sched::dseed_shown`]).
//!
//! `exploration.json`'s summary counts the runs answered so (`repeats`) and the seconds of their
//! earlier runs (`repeats_saved_s`). `hydo explore --rerun-repeats` runs every one in a VM, as
//! before Milestone 339.

use crate::pool::{Done, Res};
use crate::py::py_str;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

/// The tags of the runs that a repeat answers: first runs and shrink candidates.
pub const TAGS: [&str; 3] = ["seed-", "shrink", "errshrink-"];

/// A run that waits for the result of the same schedule's run in flight.
pub struct Waiter {
    pub tag: String,
    pub reply: Sender<Done>,
    pub cancel: Arc<AtomicBool>,
}

enum Entry {
    /// The run of `origin` is in flight.
    Running {
        origin: String,
        waiters: Vec<Waiter>,
    },
    /// The run of `origin` gave `res`.
    Done { origin: String, res: Res },
}

/// What a submitted run does.
pub enum Ask {
    /// Its answer, the earlier result (its consoles copied).
    Ready(Res),
    /// It waits for the same schedule's run in flight.
    Wait,
    /// It runs in a VM.
    Run,
}

#[derive(Default)]
pub struct Repeats {
    on: bool,
    map: HashMap<String, Entry>,
    hits: u64,
    saved_s: f64,
}

fn secs(r: &Res) -> f64 {
    r.get("secs").and_then(Value::as_f64).unwrap_or(0.0)
}

fn lost(r: &Res) -> bool {
    py_str(r.get("verdict").unwrap_or(&Value::Null)).starts_with("ERROR worker")
}

/// Copy every console of `origin` (`consoles/<origin>.*`, files and folders) to `tag`.
fn copy_consoles(outdir: &Path, origin: &str, tag: &str) {
    let d = outdir.join("consoles");
    let Ok(rd) = std::fs::read_dir(&d) else {
        return;
    };
    let pre = format!("{origin}.");
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix(&pre) {
            copy_tree(&e.path(), &d.join(format!("{tag}.{rest}")));
        }
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    if src.is_dir() {
        let _ = std::fs::create_dir_all(dst);
        if let Ok(rd) = std::fs::read_dir(src) {
            for e in rd.flatten() {
                copy_tree(&e.path(), &dst.join(e.file_name()));
            }
        }
    } else {
        let _ = std::fs::copy(src, dst);
    }
}

impl Repeats {
    pub fn set_on(&mut self, on: bool) {
        self.on = on;
    }

    /// (runs answered with an earlier result, the seconds of those earlier runs)
    pub fn stats(&self) -> (u64, f64) {
        (self.hits, self.saved_s)
    }

    fn eligible(&self, tag: &str) -> bool {
        self.on && TAGS.iter().any(|p| tag.starts_with(p))
    }

    fn hit(&mut self, outdir: &Path, origin: &str, tag: &str, res: &Res) {
        self.hits += 1;
        self.saved_s += secs(res);
        copy_consoles(outdir, origin, tag);
    }

    /// A run is submitted: its answer, a wait, or a VM.
    pub fn ask(
        &mut self,
        outdir: &Path,
        tag: &str,
        text: &str,
        reply: &Sender<Done>,
        cancel: &Arc<AtomicBool>,
    ) -> Ask {
        if !self.eligible(tag) {
            return Ask::Run;
        }
        match self.map.get_mut(text) {
            None => {
                self.map.insert(
                    text.to_string(),
                    Entry::Running {
                        origin: tag.to_string(),
                        waiters: Vec::new(),
                    },
                );
                Ask::Run
            }
            Some(Entry::Running { waiters, .. }) => {
                waiters.push(Waiter {
                    tag: tag.to_string(),
                    reply: reply.clone(),
                    cancel: cancel.clone(),
                });
                Ask::Wait
            }
            Some(Entry::Done { origin, res }) => {
                let (origin, res) = (origin.clone(), res.clone());
                self.hit(outdir, &origin, tag, &res);
                Ask::Ready(res)
            }
        }
    }

    /// A worker took the cancelled run `tag`: whether it runs anyway, because a run that was not
    /// cancelled waits for it. When none does, the schedule is free to run again later.
    pub fn needed(&mut self, tag: &str, text: &str) -> bool {
        let Some(Entry::Running { origin, waiters }) = self.map.get_mut(text) else {
            return false;
        };
        if origin != tag {
            return false;
        }
        waiters.retain(|w| !w.cancel.load(Ordering::SeqCst));
        if waiters.is_empty() {
            self.map.remove(text);
            false
        } else {
            true
        }
    }

    /// The run `tag` gave `res`: the answers of the runs that wait for it. A result is kept for
    /// later repeats unless the run lost its engine.
    pub fn finish(&mut self, outdir: &Path, tag: &str, text: &str, res: &Res) -> Vec<Answer> {
        let mut out = Vec::new();
        match self.map.remove(text) {
            Some(Entry::Running { origin, waiters }) if origin == tag => {
                for w in waiters {
                    if w.cancel.load(Ordering::SeqCst) {
                        continue;
                    }
                    if !lost(res) {
                        self.hit(outdir, &origin, &w.tag, res);
                    }
                    out.push((w.reply, (w.tag, text.to_string(), res.clone())));
                }
                if !lost(res) {
                    self.map.insert(
                        text.to_string(),
                        Entry::Done {
                            origin,
                            res: res.clone(),
                        },
                    );
                }
            }
            Some(other) => {
                self.map.insert(text.to_string(), other);
            }
            None => {}
        }
        out
    }
}

/// Where an answer goes, and the answer.
pub type Answer = (Sender<Done>, Done);
