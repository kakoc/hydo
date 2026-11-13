//! The state search's links (docs/design/state-search.md): a run that records its stops and takes
//! snapshots at stops that show rare features ([`RecLink`]), a branch that replays a parent's
//! recorded stops to its controller and goes live at the parent's snapshot ([`BranchLink`]), and
//! the timing switch of a schedule's `br=` points ([`SwitchLink`]).
//!
//! Each is a [`Link`] around another one, so the runner and the controller stay as they are: the
//! runner calls [`Link::before_step`] at every stop after its acknowledgements and before the
//! controller decides, which is where a snapshot is taken and where a branch goes live.

use crate::control::{Ending, EngineAction, Seen, StopInfo, VTime};
use crate::runner::{Link, LinkError, LinkNext};
use std::collections::{BTreeMap, BTreeSet};

/// One command the runner sent at a stop, with the engine's answer where it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Send { at: VTime, bytes: Vec<u8>, id: u64 },
    Ack(u64),
    Snap(String),
    Engine(EngineAction),
    Timing(String),
    Cont,
}

/// One recorded stop: what the engine reported, the commands sent there in order, and how many
/// of them came before [`Link::before_step`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecStop {
    pub next: LinkNext,
    pub calls: Vec<Call>,
    pub hook_at: usize,
}

impl RecStop {
    pub fn at(&self) -> Option<VTime> {
        match &self.next {
            LinkNext::Stop { stop, .. } => Some(stop.at),
            LinkNext::Done(_) => None,
        }
    }
}

/// The features of the console lines a stop brought: `assert:<id>:<ok>` for an SDK assertion
/// record, `line:<class>` for any other line of the guest's own console.
pub fn features(seen: &[Seen]) -> Vec<String> {
    let mut out = Vec::new();
    for s in seen {
        if let Seen::Console(t) = s {
            if let Some(f) = feature(t) {
                if !out.contains(&f) {
                    out.push(f);
                }
            }
        }
    }
    out
}

/// The feature of one console line (None for an empty line).
pub fn feature(line: &str) -> Option<String> {
    let l = line.trim_end_matches(['\r', '\n']).trim();
    if l.is_empty() {
        return None;
    }
    if let Some(j) = l
        .find("HYDO-ASSERT ")
        .map(|i| &l[i + "HYDO-ASSERT ".len()..])
    {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("?");
            let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
            return Some(format!("assert:{id}:{ok}"));
        }
    }
    Some(format!("line:{}", line_class(l)))
}

/// A line with every run of digits and every hexadecimal word of 6 or more characters made `#`,
/// cut to 96 bytes.
pub fn line_class(l: &str) -> String {
    let mut out = String::new();
    for (i, w) in l.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let hexish = w.len() >= 6 && w.chars().all(|c| c.is_ascii_hexdigit());
        if hexish {
            out.push('#');
            continue;
        }
        let mut digits = false;
        for c in w.chars() {
            if c.is_ascii_digit() {
                if !digits {
                    out.push('#');
                }
                digits = true;
            } else {
                digits = false;
                out.push(c);
            }
        }
    }
    let mut end = out.len().min(96);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    out
}

/// What a recording run is told (the explorer's [`hydo_engine::SearchReq`]).
#[derive(Clone, Debug, Default)]
pub struct SnapPolicy {
    pub names: Vec<String>,
    pub counts: BTreeMap<String, u64>,
    pub rare: u64,
    pub any_new: bool,
    /// Stops up to this index are a parent's replayed prefix: no snapshot there.
    pub skip: usize,
    /// For a target whose console shows no features: snapshots at the first stops whose clock
    /// is this far past the run's first stop (or the branch point), whatever they show.
    pub spread: Vec<u64>,
}

/// A snapshot the run took: its name, the stop's index in the recording, the stop, and the
/// features new to the run at that stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Taken {
    pub name: String,
    pub index: usize,
    pub stop: StopInfo,
    pub new: Vec<String>,
}

/// A link that records every stop and takes the state search's snapshots.
pub struct RecLink<'a> {
    inner: &'a mut dyn Link,
    pub stops: Vec<RecStop>,
    pub features: Vec<String>,
    seen: BTreeSet<String>,
    pub taken: Vec<Taken>,
    policy: SnapPolicy,
    ended: bool,
    last_at: Option<VTime>,
    /// the clock of the first stop where snapshots may be taken, and the spread points passed
    origin: Option<VTime>,
    spread_next: usize,
}

impl<'a> RecLink<'a> {
    pub fn new(inner: &'a mut dyn Link, policy: SnapPolicy) -> RecLink<'a> {
        RecLink {
            inner,
            stops: Vec::new(),
            features: Vec::new(),
            seen: BTreeSet::new(),
            taken: Vec::new(),
            policy,
            ended: false,
            last_at: None,
            origin: None,
            spread_next: 0,
        }
    }

    fn call(&mut self, c: Call) {
        if let Some(s) = self.stops.last_mut() {
            s.calls.push(c);
        }
    }

    fn want(&self, new: &[String]) -> bool {
        if new.is_empty() {
            return false;
        }
        let rare = new
            .iter()
            .any(|f| self.policy.counts.get(f).copied().unwrap_or(0) <= self.policy.rare);
        rare || self.policy.any_new
    }
}

impl Link for RecLink<'_> {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        let n = self.inner.next_stop()?;
        self.stops.push(RecStop {
            next: n.clone(),
            calls: Vec::new(),
            hook_at: 0,
        });
        Ok(n)
    }
    fn send(&mut self, at: VTime, bytes: &[u8]) -> Result<u64, LinkError> {
        let id = self.inner.send(at, bytes)?;
        self.call(Call::Send {
            at,
            bytes: bytes.to_vec(),
            id,
        });
        Ok(id)
    }
    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        self.inner.ack(send)?;
        self.call(Call::Ack(send));
        Ok(())
    }
    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        self.inner.snap(name)?;
        self.call(Call::Snap(name.to_string()));
        Ok(())
    }
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        self.inner.engine(a)?;
        self.call(Call::Engine(a.clone()));
        if matches!(a, EngineAction::DiskFaultsOff) {
            // the engines refuse snapshots after the workload's end
            self.ended = true;
        }
        Ok(())
    }
    fn engine_cut(&self) -> bool {
        self.inner.engine_cut()
    }
    fn timing(&mut self, tokens: &str) -> Result<(), LinkError> {
        self.inner.timing(tokens)?;
        self.call(Call::Timing(tokens.to_string()));
        Ok(())
    }
    fn cont(&mut self) -> Result<(), LinkError> {
        self.inner.cont()?;
        self.call(Call::Cont);
        Ok(())
    }
    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.ended = true;
        self.inner.end(e)
    }
    fn cut(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.ended = true;
        self.inner.cut(e)
    }
    fn before_step(&mut self, stop: &StopInfo, seen: &[Seen]) -> Result<(), LinkError> {
        self.inner.before_step(stop, seen)?;
        let index = self.stops.len().saturating_sub(1);
        if let Some(s) = self.stops.last_mut() {
            s.hook_at = s.calls.len();
        }
        let mut new = Vec::new();
        for f in features(seen) {
            if self.seen.insert(f.clone()) {
                self.features.push(f.clone());
                new.push(f);
            }
        }
        // a stop at the clock value of the stop before it cannot be told apart by br=
        let distinct = self.last_at.is_none_or(|a| a < stop.at);
        self.last_at = Some(stop.at);
        let in_prefix = index < self.policy.skip || (index == self.policy.skip && index > 0);
        if index >= self.policy.skip && self.origin.is_none() {
            self.origin = Some(stop.at);
        }
        let mut spread = false;
        if let Some(o) = self.origin {
            while self.spread_next < self.policy.spread.len()
                && stop.at.saturating_sub(o) >= self.policy.spread[self.spread_next]
            {
                self.spread_next += 1;
                spread = true;
            }
        }
        if self.ended
            || in_prefix
            || self.taken.len() >= self.policy.names.len()
            || !distinct
            || !(spread || self.want(&new))
        {
            return Ok(());
        }
        let name = self.policy.names[self.taken.len()].clone();
        match self.inner.snap(&name) {
            Ok(()) => {
                self.taken.push(Taken {
                    name,
                    index,
                    stop: *stop,
                    new,
                });
                Ok(())
            }
            // a typed command not yet acknowledged, the recovery phase: try at a later stop
            Err(e) if e.0.starts_with("snapshot ") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The error a branch's offline check ends with when its controller reached the parent's
/// snapshot with the parent's commands.
pub const REACHED: &str = "state search: the branch reached its snapshot";

/// A link that replays a parent's recorded stops (up to and including the snapshot's) to a
/// branch's controller, requires the parent's commands, and goes live at the snapshot. Without a
/// live link it is the offline check: it ends with [`REACHED`].
pub struct BranchLink<'a> {
    rec: &'a [RecStop],
    live: Option<&'a mut dyn Link>,
    i: Option<usize>,
    call: usize,
    is_live: bool,
}

impl<'a> BranchLink<'a> {
    /// `rec`: the parent's stops up to and including the snapshot's.
    pub fn new(rec: &'a [RecStop], live: Option<&'a mut dyn Link>) -> BranchLink<'a> {
        BranchLink {
            rec,
            live,
            i: None,
            call: 0,
            is_live: false,
        }
    }

    fn live(&mut self) -> &mut dyn Link {
        &mut **self.live.as_mut().expect("a live link")
    }

    fn expect(&mut self, c: Call) -> Result<Option<u64>, LinkError> {
        let i = self
            .i
            .ok_or_else(|| LinkError("a command before the first stop".into()))?;
        let got = self.rec[i].calls.get(self.call);
        let id = match (got, &c) {
            (
                Some(Call::Send { at, bytes, id }),
                Call::Send {
                    at: a, bytes: b, ..
                },
            ) if at == a && bytes == b => Some(*id),
            (Some(x), c) if x == c => None,
            (x, c) => {
                return Err(LinkError(format!(
                    "state search: the branch differs from its parent at stop {} command {}: {} \
                     instead of {}",
                    i + 1,
                    self.call + 1,
                    show(c),
                    x.map(show).unwrap_or_else(|| "nothing".into())
                )))
            }
        };
        self.call += 1;
        Ok(id)
    }
}

fn show(c: &Call) -> String {
    match c {
        Call::Send { at, bytes, .. } => {
            let t = String::from_utf8_lossy(bytes);
            format!("send at={at} {:?}", t.chars().take(60).collect::<String>())
        }
        c => format!("{c:?}"),
    }
}

impl Link for BranchLink<'_> {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        if self.is_live {
            return self.live().next_stop();
        }
        let i = self.i.map(|i| i + 1).unwrap_or(0);
        if i >= self.rec.len() {
            return Err(LinkError(
                "state search: the branch's replay ran past its snapshot".into(),
            ));
        }
        self.i = Some(i);
        self.call = 0;
        Ok(self.rec[i].next.clone())
    }
    fn send(&mut self, at: VTime, bytes: &[u8]) -> Result<u64, LinkError> {
        if self.is_live {
            return self.live().send(at, bytes);
        }
        let id = self.expect(Call::Send {
            at,
            bytes: bytes.to_vec(),
            id: 0,
        })?;
        Ok(id.unwrap_or(0))
    }
    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        if self.is_live {
            return self.live().ack(send);
        }
        self.expect(Call::Ack(send)).map(|_| ())
    }
    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        if self.is_live {
            return self.live().snap(name);
        }
        self.expect(Call::Snap(name.into())).map(|_| ())
    }
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        if self.is_live {
            return self.live().engine(a);
        }
        self.expect(Call::Engine(a.clone())).map(|_| ())
    }
    fn engine_cut(&self) -> bool {
        match (&self.live, self.is_live) {
            (Some(l), true) => l.engine_cut(),
            _ => false,
        }
    }
    fn timing(&mut self, tokens: &str) -> Result<(), LinkError> {
        if self.is_live {
            return self.live().timing(tokens);
        }
        self.expect(Call::Timing(tokens.into())).map(|_| ())
    }
    fn cont(&mut self) -> Result<(), LinkError> {
        if self.is_live {
            return self.live().cont();
        }
        self.expect(Call::Cont)?;
        let i = self.i.unwrap_or(0);
        if self.call != self.rec[i].calls.len() {
            return Err(LinkError(format!(
                "state search: the branch ran on at stop {} after {} of the parent's {} commands",
                i + 1,
                self.call,
                self.rec[i].calls.len()
            )));
        }
        Ok(())
    }
    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        if self.is_live {
            return self.live().end(e);
        }
        Err(LinkError(format!(
            "state search: the branch ended at stop {} before its snapshot",
            self.i.map(|i| i + 1).unwrap_or(0)
        )))
    }
    fn cut(&mut self, e: &Ending) -> Result<String, LinkError> {
        if self.is_live {
            return self.live().cut(e);
        }
        self.end(e)
    }
    fn before_step(&mut self, stop: &StopInfo, seen: &[Seen]) -> Result<(), LinkError> {
        if self.is_live {
            return self.live().before_step(stop, seen);
        }
        let i = self.i.unwrap_or(0);
        if i + 1 < self.rec.len() {
            return Ok(());
        }
        // the snapshot's stop: the parent took its snapshot here, after these commands
        if self.call != self.rec[i].hook_at {
            return Err(LinkError(format!(
                "state search: the branch sent {} commands at its snapshot's stop where the \
                 parent sent {} before the snapshot",
                self.call, self.rec[i].hook_at
            )));
        }
        let Some(live) = self.live.as_mut() else {
            return Err(LinkError(REACHED.into()));
        };
        // the engine's first stop after RESUME is the snapshot's own
        let want = self.rec[i].at();
        match live.next_stop()? {
            LinkNext::Stop { stop: s, lines } if Some(s.at) == want && lines.is_empty() => {}
            LinkNext::Stop { stop: s, lines } => {
                return Err(LinkError(format!(
                "state search: the engine resumed at {} with {} lines, the snapshot was at {:?}",
                s.at,
                lines.len(),
                want
            )))
            }
            LinkNext::Done(r) => {
                return Err(LinkError(format!(
                    "state search: the engine did not resume: {r}"
                )))
            }
        }
        self.is_live = true;
        Ok(())
    }
}

/// A link that sends the engine `TIME` at the first stop whose clock value reaches each of a
/// schedule's branch points (`br=`), before the controller decides there. It stops at the end of
/// the workload, as the engines refuse `TIME` after it.
pub struct SwitchLink<'a> {
    inner: &'a mut dyn Link,
    points: Vec<(VTime, String)>,
    next: usize,
    done: bool,
}

impl<'a> SwitchLink<'a> {
    /// `points`: each branch point's clock value and its timing tokens, in order.
    pub fn new(inner: &'a mut dyn Link, points: Vec<(VTime, String)>) -> SwitchLink<'a> {
        SwitchLink {
            inner,
            points,
            next: 0,
            done: false,
        }
    }

    /// The branch points of a schedule as [`SwitchLink::new`] takes them.
    pub fn points_of(s: &hydo_schedule::Schedule) -> Vec<(VTime, String)> {
        s.branch_points()
            .into_iter()
            .map(|(at, toks)| {
                (
                    at,
                    toks.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            })
            .collect()
    }
}

impl Link for SwitchLink<'_> {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        self.inner.next_stop()
    }
    fn send(&mut self, at: VTime, bytes: &[u8]) -> Result<u64, LinkError> {
        self.inner.send(at, bytes)
    }
    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        self.inner.ack(send)
    }
    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        self.inner.snap(name)
    }
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        if matches!(a, EngineAction::DiskFaultsOff) {
            self.done = true;
        }
        self.inner.engine(a)
    }
    fn engine_cut(&self) -> bool {
        self.inner.engine_cut()
    }
    fn timing(&mut self, tokens: &str) -> Result<(), LinkError> {
        self.inner.timing(tokens)
    }
    fn cont(&mut self) -> Result<(), LinkError> {
        self.inner.cont()
    }
    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.done = true;
        self.inner.end(e)
    }
    fn cut(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.done = true;
        self.inner.cut(e)
    }
    fn before_step(&mut self, stop: &StopInfo, seen: &[Seen]) -> Result<(), LinkError> {
        self.inner.before_step(stop, seen)?;
        while !self.done && self.next < self.points.len() && stop.at >= self.points[self.next].0 {
            let t = self.points[self.next].1.clone();
            self.next += 1;
            self.inner.timing(&t)?;
        }
        Ok(())
    }
}

/// The step ranges a run's transcript records (`workload steps a..z rc=`): (first step, end step,
/// stop number of the note).
pub fn ranges(t: &crate::transcript::Transcript) -> Vec<(u64, u64, u64)> {
    use crate::transcript::Record;
    let mut out = Vec::new();
    for r in &t.records {
        let (n, line) = match r {
            Record::Note { n, line, .. } | Record::Report { n, line, .. } => (*n, line),
            _ => continue,
        };
        let Some(rest) = line.split("workload steps ").nth(1) else {
            continue;
        };
        let Some((a, rest)) = rest.split_once("..") else {
            continue;
        };
        let z: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let (Ok(a), Ok(z)) = (a.parse::<u64>(), z.parse::<u64>()) {
            out.push((a, z + 1, n));
        }
    }
    out
}

/// The guest clock a recorded run advanced from the stop at `from`: the sum, over its stretches
/// of rising clock values (a recovery VM starts its own), of the last value minus the first.
pub fn clock(stops: &[RecStop], from: usize) -> (u64, u64) {
    let ats: Vec<u64> = stops.iter().skip(from).filter_map(|s| s.at()).collect();
    let first = ats.first().copied().unwrap_or(0);
    let (mut sum, mut lo, mut prev) = (0u64, first, first);
    for &a in ats.iter().skip(1) {
        if a < prev {
            sum += prev - lo;
            lo = a;
        }
        prev = a;
    }
    sum += prev.saturating_sub(lo);
    (sum, first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Why;

    #[test]
    fn line_classes_hide_numbers_and_hex_words() {
        assert_eq!(
            line_class("Current applied_seq: 449 at 0x1f deadbeef12"),
            "Current applied_seq: # at #x#f #"
        );
        assert_eq!(
            feature(r#"HYDO-ASSERT {"id":"server.store_error","ok":true,"type":"sometimes"}"#),
            Some("assert:server.store_error:true".into())
        );
        assert_eq!(feature("   "), None);
    }

    #[test]
    fn the_clock_adds_the_stretches() {
        let st = |at| RecStop {
            next: LinkNext::Stop {
                stop: StopInfo {
                    n: 1,
                    at,
                    why: Why::Cadence,
                    seen: 0,
                },
                lines: Vec::new(),
            },
            calls: Vec::new(),
            hook_at: 0,
        };
        let s = vec![st(100), st(150), st(400), st(20), st(70)];
        assert_eq!(clock(&s, 0), (300 + 50, 100));
        assert_eq!(clock(&s, 1), (250 + 50, 150));
    }
}
