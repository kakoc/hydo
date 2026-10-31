//! The controller interface (docs/design/guest-control.md, "Explorer API"): at every numbered stop
//! of a controlled run the controller is told what was seen since the previous stop (the agent's
//! replies and events, other console text, protocol errors) and answers with what to do at this
//! stop: agent commands to type, lines for the run's report, a snapshot, an engine action, or the
//! end of the conversation.
//!
//! Every decision is a pure function of what the controller was built from (the plan, the
//! schedule and its seeds) and of the stops and lines seen so far: a controller reads no clock,
//! draws no random number of the host and does not depend on threads or the worker. Time, when a
//! controller needs it, is the stop's [`StopInfo::at`] (the engine's virtual clock).

use hydo_agent_proto::line::ErrorLine;
use hydo_agent_proto::msg::{Command, Event, Hello, Reply};

/// A point of the engine's virtual clock: hvx's instruction clock `icount`, hvm's exit count.
pub type VTime = u64;

/// Why the engine stopped (the lock-step protocol's `why=`, M118/M119).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// Before the first run call of a phase.
    Start,
    /// Right after a restore from a snapshot.
    Resume,
    /// A run call returned after its event budget (the cadence, `rb=`).
    Cadence,
    /// A queued send became due.
    Send,
    /// A shell-init marker (not used by agent images).
    Marker,
    /// The guest stopped.
    Exit,
}

impl Why {
    pub fn name(self) -> &'static str {
        match self {
            Why::Start => "start",
            Why::Resume => "resume",
            Why::Cadence => "cadence",
            Why::Send => "send",
            Why::Marker => "marker",
            Why::Exit => "exit",
        }
    }

    pub fn from_name(s: &str) -> Option<Why> {
        Some(match s {
            "start" => Why::Start,
            "resume" => Why::Resume,
            "cadence" => Why::Cadence,
            "send" => Why::Send,
            "marker" => Why::Marker,
            "exit" => Why::Exit,
            _ => return None,
        })
    }
}

/// One stop: its number in the run (1 = the first), its virtual time, why it happened, and how
/// many agent lines the session has read by then (counted from the boot's hello).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopInfo {
    pub n: u64,
    pub at: VTime,
    pub why: Why,
    pub seen: u64,
}

/// Something the controller learns at a stop, in the order the engine reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// The agent announced itself (the first agent line of a fresh boot).
    Hello(Hello),
    /// The agent carried out command `id`.
    Reply { id: u64, body: Reply },
    /// The agent refused command `id`.
    Refused { id: u64, error: ErrorLine },
    /// An event of the agent (exit, done, probe, line, ...).
    Event(Event),
    /// An event this protocol version does not know (from a newer agent of the same version).
    UnknownEvent(String),
    /// A console line that is not the agent's (kernel or service text).
    Console(String),
    /// A corrupt, lost or unpaired agent line, or an agent this host cannot speak to: the run
    /// is an ERROR (it is the same in every replay, so it is wrong, never flaky).
    Broken(String),
}

/// When a command is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    /// At this stop.
    Now,
    /// At the first stop at or after this virtual time (the engine caps its run calls so that it
    /// stops there exactly).
    At(VTime),
}

/// An action of the engine that is not an agent command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineAction {
    /// Switch the disk faults off: what the engine does today when the shell init prints
    /// `hydo-explore-workload-done` (it then types `go`). The engine side for agent images is
    /// step C1's.
    DiskFaultsOff,
}

impl EngineAction {
    pub fn name(&self) -> &'static str {
        match self {
            EngineAction::DiskFaultsOff => "disk-faults-off",
        }
    }

    pub fn from_name(s: &str) -> Option<EngineAction> {
        match s {
            "disk-faults-off" => Some(EngineAction::DiskFaultsOff),
            _ => None,
        }
    }
}

/// How a conversation ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The verdict: one `HYDO-ORACLE:` line, as the shell init prints it.
    Verdict(String),
    /// A power cut: every service is frozen; the engine cuts the power (the volatile write cache
    /// is lost) and a recovery conversation follows, given the acknowledged and the definitely
    /// failed ids (`hydo-explore-acked:` / `hydo-explore-failed:`).
    PowerCut { acked: Vec<String>, failed: Vec<String> },
    /// The conversation ends at the snapshot of this name, which later conversations resume
    /// from (the boot conversation ends at S1).
    AtSnapshot(String),
}

/// One thing the controller does at a stop. `by` names the part of the controller that did it
/// (`main`, `every:<invariant>`, `pause-timer`, `kill-timer`, ...): the transcript keeps it so a
/// reader (and the equivalence test) can follow each part on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Type agent command `id` (ids increase by one per command, see [`Out`]).
    Send {
        id: u64,
        at: When,
        cmd: Command,
        by: String,
    },
    /// A line of the run's report: what the shell init printed on the console for the host's
    /// tools (assertion declarations and records, the setup marker, the acknowledged ids, the
    /// verdict).
    Report { by: String, line: String },
    /// A line for people (the shell init's `hydo-ob:` log lines); not part of the transcript hash.
    Note { by: String, line: String },
    /// Snapshot the VM at this stop under this name.
    Snapshot(String),
    /// An engine action.
    Engine(EngineAction),
    /// The end of the conversation.
    End(Ending),
}

/// What a controller answers at one stop. Command ids are given out here, in order, from the
/// agent session's next id, so a controller never needs to know where the numbering stands.
#[derive(Debug)]
pub struct Out {
    next_id: u64,
    actions: Vec<Action>,
}

impl Out {
    /// An answer whose first command gets id `next_id`.
    pub fn new(next_id: u64) -> Out {
        Out {
            next_id,
            actions: Vec::new(),
        }
    }

    /// The id the next command gets.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Type `cmd` at this stop; returns its id.
    pub fn send(&mut self, by: &str, cmd: Command) -> u64 {
        self.send_at(by, When::Now, cmd)
    }

    /// Type `cmd` at `at`; returns its id.
    pub fn send_at(&mut self, by: &str, at: When, cmd: Command) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.actions.push(Action::Send {
            id,
            at,
            cmd,
            by: by.to_string(),
        });
        id
    }

    /// Type `cmd` under a given id (a replay of a transcript). The id must be the next one.
    pub fn send_as(&mut self, id: u64, by: &str, at: When, cmd: Command) -> Result<(), String> {
        if id != self.next_id {
            return Err(format!(
                "command id {id} where the session's next id is {}",
                self.next_id
            ));
        }
        self.send_at(by, at, cmd);
        Ok(())
    }

    pub fn report(&mut self, by: &str, line: impl Into<String>) {
        self.actions.push(Action::Report {
            by: by.to_string(),
            line: line.into(),
        });
    }

    pub fn note(&mut self, by: &str, line: impl Into<String>) {
        self.actions.push(Action::Note {
            by: by.to_string(),
            line: line.into(),
        });
    }

    pub fn snapshot(&mut self, name: &str) {
        self.actions.push(Action::Snapshot(name.to_string()));
    }

    pub fn engine(&mut self, a: EngineAction) {
        self.actions.push(Action::Engine(a));
    }

    pub fn end(&mut self, e: Ending) {
        self.actions.push(Action::End(e));
    }

    /// The actions so far, in order.
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    /// The actions, in order.
    pub fn into_actions(self) -> Vec<Action> {
        self.actions
    }
}

/// A host-side controller of one conversation with a guest agent (a boot, a run, a recovery
/// boot). The design note's shape (`step(now, events) -> Vec<Action>`), with the command ids
/// given out by [`Out`].
pub trait Controller {
    /// A short name for the transcript (`schedule`, `replay`).
    fn name(&self) -> &str;

    /// Called at every stop with what was seen since the previous stop (empty at most stops);
    /// pushes what to do at this stop into `out`. After an [`Action::End`] it is not called again.
    fn step(&mut self, stop: &StopInfo, seen: &[Seen], out: &mut Out);
}
