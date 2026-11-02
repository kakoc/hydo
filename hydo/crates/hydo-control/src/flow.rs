//! A small runtime that lets a controller be written as straight-line code in the shape of the
//! shell init (one function per init function, `?` where the shell blocks), while staying a pure
//! function of the events seen so far.
//!
//! Each part of the controller (the main sequence, one periodic checker per invariant, a pause
//! timer, a kill timer) is a *thread*: a function that is executed again from its start every
//! time something new was seen, until it finishes. Every effect it has (a command sent, a report
//! line, a change of the shared host state, a thread started) is recorded in the thread's memo
//! the first time it happens, and returned from the memo on every later execution, so it happens
//! once. Waiting is a pure query of the facts (the agent's answers, which only accumulate): the
//! function returns [`Stop::Pending`] until the answer is there. This is the replay technique of
//! workflow engines (a workflow function re-run over its recorded history); here the history is
//! the memo and the facts.
//!
//! Order is fixed: at each stop, after the new facts are recorded, every live thread runs in the
//! order it was started (the main sequence first), and the round is repeated until no thread
//! makes progress. Nothing depends on wall time or on the host's threads.
//!
//! Cancellation (the shell's `kill <loop pid>`): a thread cancelled at stop k still consumes the
//! answers that were seen up to stop k (the guest had finished that work before the host decided),
//! but it sends no further command and starts no thread: the first time it would, it ends.

use crate::control::{Action, Ending, EngineAction, Out};
use hydo_agent_proto::line::ErrorLine;
use hydo_agent_proto::msg::{Command, Done, Event, Exit, ProbeDone, Reply};
use std::collections::{BTreeMap, BTreeSet};

/// Why a thread's function returned early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// It waits for an answer that is not there yet.
    Pending,
    /// It ended the conversation (a verdict, a cut, a snapshot ending).
    Halt,
    /// It was cancelled and would have sent something.
    Cancelled,
}

pub(crate) type Step<T> = Result<T, Stop>;

/// A recorded effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Memo {
    Unit,
    Id(u64),
    Bool(bool),
    Text(String),
    Thread(usize),
}

/// The agent's answers seen so far, by command id. They only accumulate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    pub replies: BTreeMap<u64, Result<Reply, ErrorLine>>,
    pub dones: BTreeMap<u64, Done>,
    pub probes: BTreeMap<u64, ProbeDone>,
    /// Exit events, by the id of the spawn that started the process.
    pub exits: BTreeMap<u64, Exit>,
    /// Every other event (line, froze, resumed, dropped, warn), in the order the agent wrote them.
    pub events: Vec<Event>,
    /// The guest's assertion records on the console, in order, as (id, whether the record is
    /// the assertion's event: `ok:true` of a sometimes/reachable, `ok:false` of the others).
    /// Kept only for a run with reactions (`A=`, M139); empty otherwise.
    pub asserts: Vec<(String, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Live,
    Finished,
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) struct Thread<K> {
    pub kind: K,
    memo: Vec<Memo>,
    state: State,
}

/// The threads of one conversation and the host state they share.
#[derive(Clone, Debug)]
pub(crate) struct Flow<S, K> {
    threads: Vec<Thread<K>>,
    unanswered: BTreeSet<u64>,
    ended: bool,
    pub shared: S,
}

/// What a thread's function can do. Every method that has an effect is recorded in the memo.
pub(crate) struct Fx<'a, S, K> {
    pub facts: &'a Facts,
    pub shared: &'a mut S,
    threads: &'a mut Vec<Thread<K>>,
    unanswered: &'a mut BTreeSet<u64>,
    memo: &'a mut Vec<Memo>,
    cursor: usize,
    out: &'a mut Out,
    by: String,
    cancelled: bool,
}

impl<S, K: Clone> Fx<'_, S, K> {
    fn recorded(&mut self) -> Option<Memo> {
        let m = self.memo.get(self.cursor).cloned();
        if m.is_some() {
            self.cursor += 1;
        }
        m
    }

    fn record(&mut self, m: Memo) {
        debug_assert_eq!(self.cursor, self.memo.len());
        self.memo.push(m);
        self.cursor += 1;
    }

    /// Send a command (once); its id.
    pub fn send(&mut self, cmd: Command) -> Step<u64> {
        if let Some(m) = self.recorded() {
            let Memo::Id(id) = m else {
                unreachable!("memo out of step")
            };
            return Ok(id);
        }
        if self.cancelled {
            return Err(Stop::Cancelled);
        }
        let id = self.out.send(&self.by, cmd);
        self.unanswered.insert(id);
        self.record(Memo::Id(id));
        Ok(id)
    }

    /// The agent's reply to command `id` (Pending until it is seen).
    pub fn reply(&self, id: u64) -> Step<Result<Reply, ErrorLine>> {
        self.facts.replies.get(&id).cloned().ok_or(Stop::Pending)
    }

    /// The `done` event of run command `id`.
    pub fn done(&self, id: u64) -> Step<Done> {
        self.facts.dones.get(&id).cloned().ok_or(Stop::Pending)
    }

    /// The `probe` event of probe command `id`.
    pub fn probe_done(&self, id: u64) -> Step<ProbeDone> {
        self.facts.probes.get(&id).cloned().ok_or(Stop::Pending)
    }

    /// The `exit` event of the process spawn command `id` started.
    pub fn exit(&self, id: u64) -> Step<Exit> {
        self.facts.exits.get(&id).cloned().ok_or(Stop::Pending)
    }

    /// Change or read the shared host state (once): the value it gave the first time.
    pub fn effect(&mut self, f: impl FnOnce(&mut S, &Facts) -> Memo) -> Memo {
        if let Some(m) = self.recorded() {
            return m;
        }
        let m = f(self.shared, self.facts);
        self.record(m.clone());
        m
    }

    pub fn effect_bool(&mut self, f: impl FnOnce(&mut S, &Facts) -> bool) -> bool {
        match self.effect(|s, fa| Memo::Bool(f(s, fa))) {
            Memo::Bool(b) => b,
            m => unreachable!("memo out of step: {m:?}"),
        }
    }

    pub fn effect_text(&mut self, f: impl FnOnce(&mut S, &Facts) -> String) -> String {
        match self.effect(|s, fa| Memo::Text(f(s, fa))) {
            Memo::Text(t) => t,
            m => unreachable!("memo out of step: {m:?}"),
        }
    }

    /// Wait until a condition on the shared state and the facts holds (once it held, it holds).
    pub fn until(&mut self, cond: impl Fn(&S, &Facts) -> bool) -> Step<()> {
        if self.recorded().is_some() {
            return Ok(());
        }
        if cond(self.shared, self.facts) {
            self.record(Memo::Unit);
            Ok(())
        } else {
            Err(Stop::Pending)
        }
    }

    /// Wait until every command sent so far has been answered (the agent read every byte the
    /// host typed: the snapshot rule of M118, and the end-of-run point of the design).
    pub fn settle(&mut self) -> Step<()> {
        if self.recorded().is_some() {
            return Ok(());
        }
        let facts = self.facts;
        self.unanswered.retain(|id| !facts.replies.contains_key(id));
        if self.unanswered.is_empty() {
            self.record(Memo::Unit);
            Ok(())
        } else {
            Err(Stop::Pending)
        }
    }

    fn act(&mut self, a: impl FnOnce(&mut Out, &str)) {
        if self.recorded().is_some() {
            return;
        }
        a(self.out, &self.by);
        self.record(Memo::Unit);
    }

    pub fn report(&mut self, line: impl Into<String>) {
        let line = line.into();
        self.act(|o, by| o.report(by, line));
    }

    pub fn note(&mut self, line: impl Into<String>) {
        let line = line.into();
        self.act(|o, by| o.note(by, line));
    }

    pub fn snapshot(&mut self, name: &str) {
        self.act(|o, _| o.snapshot(name));
    }

    pub fn engine(&mut self, a: EngineAction) {
        self.act(|o, _| o.engine(a));
    }

    /// End the conversation (returns Halt for `?`).
    pub fn end<T>(&mut self, e: Ending) -> Step<T> {
        self.act(|o, _| o.end(e));
        Err(Stop::Halt)
    }

    /// Start a thread (once); its number.
    pub fn start(&mut self, kind: K) -> Step<usize> {
        if let Some(m) = self.recorded() {
            let Memo::Thread(t) = m else {
                unreachable!("memo out of step")
            };
            return Ok(t);
        }
        if self.cancelled {
            return Err(Stop::Cancelled);
        }
        self.threads.push(Thread {
            kind,
            memo: Vec::new(),
            state: State::Live,
        });
        let t = self.threads.len() - 1;
        self.record(Memo::Thread(t));
        Ok(t)
    }

    /// Cancel a thread (once).
    pub fn cancel(&mut self, t: usize) {
        if self.recorded().is_some() {
            return;
        }
        if self.threads[t].state == State::Live {
            self.threads[t].state = State::Cancelled;
        }
        self.record(Memo::Unit);
    }

    /// Whether this thread was cancelled (it runs once more at the stop of its cancellation, for
    /// the answers seen up to then).
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Wait until a thread has finished (or was cancelled).
    pub fn join(&self, t: usize) -> Step<()> {
        if self.threads[t].state == State::Live {
            Err(Stop::Pending)
        } else {
            Ok(())
        }
    }
}

/// The function of a thread kind.
pub(crate) type Body<S, K> = fn(&mut Fx<'_, S, K>, &K) -> Step<()>;

impl<S, K: Clone> Flow<S, K> {
    pub fn new(shared: S, main: K) -> Flow<S, K> {
        Flow {
            threads: vec![Thread {
                kind: main,
                memo: Vec::new(),
                state: State::Live,
            }],
            unanswered: BTreeSet::new(),
            ended: false,
            shared,
        }
    }

    pub fn ended(&self) -> bool {
        self.ended
    }

    /// The conversation ended outside the threads (a protocol error).
    pub fn end_now(&mut self) {
        self.ended = true;
    }

    /// A command typed outside the threads (the runner's resynchronisation `sync`) that an
    /// ending must also wait for: the agent has read every byte the host typed (M123: a run can
    /// now end before the controller sent anything).
    pub fn expect_answer(&mut self, id: u64) {
        self.unanswered.insert(id);
    }

    /// Run every live thread (and every thread cancelled at this stop) until none makes
    /// progress. `label` names a thread kind; `body` runs one.
    pub fn pump(
        &mut self,
        facts: &Facts,
        out: &mut Out,
        label: fn(&K) -> String,
        body: Body<S, K>,
    ) {
        if self.ended {
            return;
        }
        // threads cancelled before this stop do not run again; one cancelled during it does,
        // for the answers seen up to now
        let mut runnable: Vec<bool> = self
            .threads
            .iter()
            .map(|t| t.state == State::Live)
            .collect();
        loop {
            let mut progress = false;
            let mut i = 0;
            while i < self.threads.len() {
                if i >= runnable.len() {
                    runnable.push(true);
                }
                if !runnable[i] {
                    i += 1;
                    continue;
                }
                let kind = self.threads[i].kind.clone();
                let cancelled = self.threads[i].state == State::Cancelled;
                let mut memo = std::mem::take(&mut self.threads[i].memo);
                let before = (memo.len(), out.actions().len());
                let r = {
                    let mut fx = Fx {
                        facts,
                        shared: &mut self.shared,
                        threads: &mut self.threads,
                        unanswered: &mut self.unanswered,
                        memo: &mut memo,
                        cursor: 0,
                        out,
                        by: label(&kind),
                        cancelled,
                    };
                    body(&mut fx, &kind)
                };
                if (memo.len(), out.actions().len()) != before {
                    progress = true;
                }
                self.threads[i].memo = memo;
                match r {
                    Ok(()) => {
                        if self.threads[i].state == State::Live {
                            self.threads[i].state = State::Finished;
                        }
                        runnable[i] = false;
                        progress = true;
                    }
                    Err(Stop::Halt) => {
                        self.ended = true;
                        return;
                    }
                    Err(Stop::Cancelled) => runnable[i] = false,
                    Err(Stop::Pending) => {}
                }
                i += 1;
            }
            if !progress {
                break;
            }
        }
        if out.actions().iter().any(|a| matches!(a, Action::End(_))) {
            self.ended = true;
        }
    }
}
