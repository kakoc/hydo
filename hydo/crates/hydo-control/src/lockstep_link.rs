//! [`Link`] over the engines' lock-step protocol (`hydo_engine::lockstep`, M118/M119), for step
//! C1. Not used by anything yet.
//!
//! Two parts are C1's and are refused here instead of guessed: the engine side of switching the
//! disk faults off for an agent image ([`EngineAction::DiskFaultsOff`]; for shell images the
//! engine does it at the init's marker), and how a power-cut ending reaches the engine's
//! recovery boot ([`Ending::PowerCut`]; today the engine reads the init's acked lines). A verdict
//! ending becomes `END <verdict>`; a snapshot ending ends the run with the engine's own verdict.

use crate::control::{Ending, EngineAction, StopInfo, VTime, Why};
use crate::runner::{Link, LinkError, LinkNext};
use hydo_engine::lockstep::{self, Lockstep, Next, SnapAnswer, StopWhy};

pub struct LockstepLink<'a, 'b> {
    pub ls: &'b mut Lockstep<'a>,
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
    }
}

impl Link for LockstepLink<'_, '_> {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        match self.ls.next_stop().map_err(err)? {
            Next::Done(r) => Ok(LinkNext::Done(format!(
                "RESULT {} fp={} verdict={}",
                r.tag, r.fp, r.verdict
            ))),
            Next::Stop(rep) => {
                if !rep.dropped.is_empty() {
                    return Err(LinkError(format!(
                        "the engine dropped sends {:?} at a phase change",
                        rep.dropped
                    )));
                }
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
        Err(LinkError(format!(
            "the engine action {} for agent images is step C1's",
            a.name()
        )))
    }

    fn cont(&mut self) -> Result<(), LinkError> {
        self.ls.cont().map_err(err)
    }

    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        let r = match e {
            Ending::Verdict(v) => self.ls.end(Some(v)).map_err(err)?,
            Ending::AtSnapshot(_) => self.ls.end(None).map_err(err)?,
            Ending::PowerCut { .. } => {
                return Err(LinkError(
                    "handing a power cut to the engine's recovery boot is step C1's".into(),
                ))
            }
        };
        Ok(format!(
            "RESULT {} fp={} verdict={}",
            r.tag, r.fp, r.verdict
        ))
    }
}
