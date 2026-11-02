//! [`Link`] over the engines' lock-step protocol (`hydo_engine::lockstep`, M118/M119), with the
//! engine actions of agent images (M126): the end of the workload
//! ([`EngineAction::DiskFaultsOff`] = `DISKOFF`, which a `cf=` schedule's engine turns into the
//! power cut, [`Link::engine_cut`]) and the host's power cut ([`Ending::PowerCut`] = `CUT`; the
//! engine's next stop is the recovery VM's, restored from the host's S0 with the flushed data
//! disk). A verdict ending becomes `END <verdict>`; a snapshot ending ends the run with the
//! engine's own verdict.

use crate::control::{Ending, EngineAction, StopInfo, VTime, Why};
use crate::runner::{Link, LinkError, LinkNext};
use hydo_engine::lockstep::{self, Lockstep, Next, Phase, SnapAnswer, StopWhy};

pub struct LockstepLink<'a, 'b> {
    pub ls: &'b mut Lockstep<'a>,
    /// The phase of the last stop.
    pub phase: Option<Phase>,
    /// The last DISKOFF answered `cut=1`.
    cut: bool,
    /// The run's RESULT line, once the engine printed it.
    pub result: Option<hydo_engine::RunResult>,
}

impl<'a, 'b> LockstepLink<'a, 'b> {
    pub fn new(ls: &'b mut Lockstep<'a>) -> LockstepLink<'a, 'b> {
        LockstepLink {
            ls,
            phase: None,
            cut: false,
            result: None,
        }
    }
}

fn err(e: impl std::fmt::Display) -> LinkError {
    LinkError(e.to_string())
}

fn why(w: StopWhy) -> Why {
    match w {
        StopWhy::Start => Why::Start,
        StopWhy::Resume => Why::Resume,
        StopWhy::Cadence => Why::Cadence,
        StopWhy::Send => Why::Send,
        StopWhy::Marker => Why::Marker,
        StopWhy::Exit => Why::Exit,
        StopWhy::Hostcall => Why::Hostcall,
    }
}

impl Link for LockstepLink<'_, '_> {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        match self.ls.next_stop().map_err(err)? {
            Next::Done(r) => {
                let line = r.line.clone();
                self.result = Some(r);
                Ok(LinkNext::Done(line))
            }
            Next::Stop(rep) => {
                // input still queued when the run moves to the recovery VM is discarded with the
                // old VM (the recovery conversation starts from S0's own agent state)
                let recovery_start =
                    rep.stop.phase == Phase::Recovery && rep.stop.why == StopWhy::Start;
                if !rep.dropped.is_empty() && !recovery_start {
                    return Err(LinkError(format!(
                        "the engine dropped sends {:?} outside a move to the recovery VM",
                        rep.dropped
                    )));
                }
                self.phase = Some(rep.stop.phase);
                Ok(LinkNext::Stop {
                    stop: StopInfo {
                        n: rep.stop.n,
                        at: rep.stop.at,
                        why: why(rep.stop.why),
                        seen: 0,
                    },
                    lines: rep
                        .events
                        .into_iter()
                        .map(|e: lockstep::Event| e.text)
                        .collect(),
                })
            }
        }
    }

    fn send(&mut self, at: VTime, bytes: &[u8]) -> Result<u64, LinkError> {
        Ok(self.ls.send(at, bytes).map_err(err)?.id())
    }

    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        self.ls.ack(send).map_err(err)?.map_err(LinkError)
    }

    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        match self.ls.snap(name, false).map_err(err)? {
            SnapAnswer::Taken { .. } => Ok(()),
            SnapAnswer::Refused(why) => Err(LinkError(format!("snapshot {name} refused: {why}"))),
        }
    }

    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        match a {
            EngineAction::DiskFaultsOff => {
                let d = self
                    .ls
                    .diskoff()
                    .map_err(err)?
                    .map_err(|e| LinkError(format!("the engine refused DISKOFF: {e}")))?;
                self.cut = d.cut;
                Ok(())
            }
        }
    }

    fn engine_cut(&self) -> bool {
        self.cut
    }

    fn cont(&mut self) -> Result<(), LinkError> {
        self.ls.cont().map_err(err)
    }

    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        let r = match e {
            Ending::Verdict(v) => self.ls.end(Some(v)).map_err(err)?,
            Ending::AtSnapshot(_) => self.ls.end(None).map_err(err)?,
            Ending::PowerCut { .. } => return self.cut(e),
        };
        let line = r.line.clone();
        self.result = Some(r);
        Ok(line)
    }

    fn cut(&mut self, _e: &Ending) -> Result<String, LinkError> {
        self.ls
            .cut()
            .map_err(err)?
            .map_err(|e| LinkError(format!("the engine refused CUT: {e}")))?;
        Ok(String::new())
    }
}
