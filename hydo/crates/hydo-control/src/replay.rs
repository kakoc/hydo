//! Typing a recorded control transcript again (`hydo repro --transcript`, step C1): at each
//! recorded stop the [`ReplayController`] sends the recorded commands with their ids, times and
//! bytes, and repeats the reports, snapshots, engine actions and the end, without running any
//! policy. The runner's own records (its resynchronisation after a restore, the
//! acknowledgements) are not replayed: the runner makes them again.
//!
//! A replay checks that the run follows the recording: a recorded stop must come with the same
//! virtual time and the same number of agent lines seen. At the first stop that differs the
//! replay ends the conversation with an ERROR verdict that says where (a saved failure whose
//! image or engine changed is then reported, not silently run differently).

use crate::control::{Controller, Ending, Out, Seen, StopInfo, When};
use crate::transcript::{Record, Transcript};
use hydo_agent_proto::line::HostLine;
use hydo_agent_proto::msg::PROTO;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct ReplayController {
    stops: BTreeMap<u64, (u64, u64)>,
    by_stop: BTreeMap<u64, Vec<Record>>,
    ended: bool,
}

impl ReplayController {
    pub fn new(t: &Transcript) -> ReplayController {
        let mut stops = BTreeMap::new();
        let mut by_stop: BTreeMap<u64, Vec<Record>> = BTreeMap::new();
        for r in &t.records {
            match r {
                Record::Stop { n, at, seen, .. } => {
                    stops.insert(*n, (*at, *seen));
                }
                Record::Ack { .. } => {}
                Record::Send { by, .. } if by == "runner" => {}
                r => by_stop.entry(r.stop()).or_default().push(r.clone()),
            }
        }
        ReplayController {
            stops,
            by_stop,
            ended: false,
        }
    }

    fn diverged(&mut self, out: &mut Out, why: String) {
        let line = format!("HYDO-ORACLE: ERROR the replay diverged from its transcript: {why}");
        // drop whatever was pushed at this stop: the replay stops here
        let mut fresh = Out::new(out.next_id());
        fresh.report("replay", line.clone());
        fresh.end(Ending::Verdict(line));
        *out = fresh;
        self.ended = true;
    }
}

impl Controller for ReplayController {
    fn name(&self) -> &str {
        "replay"
    }

    fn step(&mut self, stop: &StopInfo, _seen: &[Seen], out: &mut Out) {
        if self.ended {
            return;
        }
        if let Some((at, lines)) = self.stops.get(&stop.n).copied() {
            if at != stop.at || lines != stop.seen {
                return self.diverged(
                    out,
                    format!(
                        "stop {} at {} after {} agent lines (recorded at {at} after {lines})",
                        stop.n, stop.at, stop.seen
                    ),
                );
            }
        }
        let Some(recs) = self.by_stop.remove(&stop.n) else {
            return;
        };
        for r in recs {
            match r {
                Record::Send {
                    id, at, bytes, by, ..
                } => {
                    let text = bytes.strip_suffix('\n').unwrap_or(&bytes);
                    let hl = match HostLine::decode(text.as_bytes(), &[PROTO]) {
                        Ok(h) => h,
                        Err(e) => {
                            return self.diverged(
                                out,
                                format!("recorded command {id} does not decode: {}", e.msg),
                            )
                        }
                    };
                    if hl.id != id {
                        return self
                            .diverged(out, format!("recorded command {id} carries id {}", hl.id));
                    }
                    let when = if at == stop.at {
                        When::Now
                    } else {
                        When::At(at)
                    };
                    if let Err(e) = out.send_as(id, &by, when, hl.cmd) {
                        return self.diverged(out, e);
                    }
                }
                Record::Report { by, line, .. } => out.report(&by, line),
                Record::Note { by, line, .. } => out.note(&by, line),
                Record::Snap { name, .. } => out.snapshot(&name),
                Record::Engine { action, .. } => out.engine(action),
                Record::End { end, .. } => {
                    out.end(end);
                    self.ended = true;
                }
                Record::Stop { .. } | Record::Ack { .. } => {}
            }
        }
    }
}
