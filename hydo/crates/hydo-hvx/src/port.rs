//! The engine's end of its conversation with the host (M142): typed requests and commands in,
//! typed reports ([`EngineMsg`]) out.
//!
//! - [`ChanPort`]: hydo-engine runs this engine on a thread of its own process and talks to it
//!   with the values themselves, over channels (`hydo_engine::ChanLink` is the other end);
//! - [`crate::text::TextPort`]: the program `hydo-hvx` reads the host's lines on stdin and prints
//!   each report's line on stdout (the protocol of the deleted C engine, which it reads with the
//!   C engine's own leniency).

use hydo_engine_proto::{escape, EngineMsg, HostCmd, HostMsg, Request};
use std::sync::mpsc::{Receiver, Sender};

/// What the engine reads between runs.
#[derive(Debug)]
pub enum Req {
    Request(Request),
    /// A request it cannot read (answered `RESULT ? ... verdict=ERROR bad request`): one that is
    /// not CTL, RESUME or QUIT (RUN and BRANCH were deleted in M145), or one of those it cannot
    /// read.
    BadRun,
    /// No more requests (QUIT, or the host closed its end).
    End,
}

/// What the engine reads at a stop.
#[derive(Debug)]
pub enum Cmd {
    Host(HostCmd),
    /// A command it cannot read, refused with this reason (`ERR <reason>`).
    Bad(String),
    /// The host closed its end.
    Closed,
}

/// The engine's end of the conversation.
pub trait Port: Send {
    fn emit(&mut self, m: EngineMsg);
    fn request(&mut self) -> Req;
    fn command(&mut self) -> Cmd;
}

/// Values over channels.
pub struct ChanPort {
    rx: Receiver<HostMsg>,
    tx: Sender<EngineMsg>,
}

impl ChanPort {
    pub fn new(rx: Receiver<HostMsg>, tx: Sender<EngineMsg>) -> ChanPort {
        ChanPort { rx, tx }
    }
}

impl Port for ChanPort {
    fn emit(&mut self, m: EngineMsg) {
        // the host may have gone; the engine finishes its run as it would after a closed pipe
        let _ = self.tx.send(m);
    }

    fn request(&mut self) -> Req {
        match self.rx.recv() {
            Ok(HostMsg::Request(Request::Quit)) | Err(_) => Req::End,
            Ok(HostMsg::Request(r)) => Req::Request(r),
            // a command between runs: the text engine reads it as a RUN it cannot read
            Ok(HostMsg::Cmd(_)) => Req::BadRun,
        }
    }

    fn command(&mut self) -> Cmd {
        match self.rx.recv() {
            Ok(HostMsg::Cmd(c)) => Cmd::Host(c),
            // a request at a stop: the text engine answers it as an unknown command
            Ok(HostMsg::Request(r)) => {
                Cmd::Bad(format!("unknown command {}", escape(r.line().as_bytes())))
            }
            Err(_) => Cmd::Closed,
        }
    }
}
