//! The host's end of one engine's conversation (M142): it sends [`HostMsg`] values (requests,
//! and commands at a stop) and receives [`EngineMsg`] values.
//!
//! Two links exist:
//!
//! - [`ChanLink`]: the hvx engine running on a thread of this process ([`crate::inproc`]); the
//!   values travel over channels as they are (`hydo_hvx::port::ChanPort` is the engine's end);
//! - [`TextLink`]: an engine program (the Mac's `hydo-hvm`, or `hydo-hvx` under `hvx-run sudo`); each
//!   value is its line ([`HostMsg::line`], [`EngineMsg::parse`]) on the program's stdin and stdout.
//!
//! Everything above a link (the worker, the lock-step client, the controller, the explorer) sees
//! the same values either way.

use crate::protocol::{EngineMsg, HostMsg};
use crate::EngineError;
use std::io::{BufRead, Write};
use std::sync::mpsc::{Receiver, Sender};

/// One engine conversation, as values.
pub trait Link: Send {
    /// Send a request or a command.
    fn send(&mut self, m: HostMsg) -> Result<(), EngineError>;
    /// The engine's next report. `Err(Protocol("the engine exited"))` when it has ended.
    fn recv(&mut self) -> Result<EngineMsg, EngineError>;
    /// The line of the last report as the engine printed it (with its newline), when the engine
    /// is a program; None in this process.
    fn last_text(&self) -> Option<&str> {
        None
    }
}

/// The text form over a program's stdin (`w`) and stdout (`r`).
pub struct TextLink<R, W> {
    r: R,
    w: W,
    raw: String,
}

impl<R: BufRead, W: Write> TextLink<R, W> {
    pub fn new(r: R, w: W) -> TextLink<R, W> {
        TextLink {
            r,
            w,
            raw: String::new(),
        }
    }

    /// The writer (what the host sent, for tests over memory).
    pub fn writer(&self) -> &W {
        &self.w
    }

    /// Write one raw line (a transcript replay; normal callers send values).
    pub fn send_line(&mut self, line: &str) -> Result<(), EngineError> {
        self.w.write_all(line.as_bytes())?;
        self.w.write_all(b"\n")?;
        self.w.flush()?;
        Ok(())
    }
}

impl<R: BufRead + Send, W: Write + Send> Link for TextLink<R, W> {
    fn send(&mut self, m: HostMsg) -> Result<(), EngineError> {
        self.send_line(&m.line())
    }

    fn recv(&mut self) -> Result<EngineMsg, EngineError> {
        let mut buf = Vec::new();
        if self.r.read_until(b'\n', &mut buf)? == 0 {
            return Err(EngineError::Protocol("the engine exited".into()));
        }
        self.raw = String::from_utf8_lossy(&buf).into_owned();
        Ok(EngineMsg::parse(&self.raw))
    }

    fn last_text(&self) -> Option<&str> {
        Some(&self.raw)
    }
}

/// The values themselves, over channels to an engine thread of this process.
pub struct ChanLink {
    tx: Sender<HostMsg>,
    rx: Receiver<EngineMsg>,
}

impl ChanLink {
    pub fn new(tx: Sender<HostMsg>, rx: Receiver<EngineMsg>) -> ChanLink {
        ChanLink { tx, rx }
    }
}

impl Link for ChanLink {
    fn send(&mut self, m: HostMsg) -> Result<(), EngineError> {
        self.tx
            .send(m)
            .map_err(|_| EngineError::Protocol("the engine exited".into()))
    }

    fn recv(&mut self) -> Result<EngineMsg, EngineError> {
        self.rx
            .recv()
            .map_err(|_| EngineError::Protocol("the engine exited".into()))
    }
}
