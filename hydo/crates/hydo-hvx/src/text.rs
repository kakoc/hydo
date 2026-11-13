//! The program `hydo-hvx`'s text: the host's lines on stdin become typed requests and commands,
//! and every report the engine makes is printed as its line on stdout (M142; before, the engine
//! itself read and printed text, in the program and in process alike).
//!
//! The lines are read as the deleted C engine read them (`hvx_explore.c`: `fgets` in pieces of
//! at most 4095 bytes, a line cut at its first NUL, `sscanf` words of at most 63 bytes, `strtoull`
//! numbers), so a host that wrote to the C engine gets the same answers, with two exceptions: the
//! schedule of a CTL is read by hydo-schedule (`Schedule::parse`), which refuses what the C read
//! loosely (`hydo-hvx/tests/golden/parse_differences.txt`), and the shell-init images' requests
//! RUN and BRANCH were deleted in Milestone 145; such a request is answered as the C answered a
//! request it could not read (`RESULT ? ... verdict=ERROR bad request`).

use crate::cstr::{strtoull, Scan};
use crate::ctl;
use crate::port::{Cmd, Port, Req};
use hydo_engine_proto::{EngineMsg, HostCmd, Request};
use hydo_schedule::Schedule;
use std::io::{BufRead, Write};

pub struct TextPort {
    input: Box<dyn BufRead + Send>,
    out: Box<dyn Write + Send>,
    line_buf: Vec<u8>,
}

impl TextPort {
    pub fn new(input: Box<dyn BufRead + Send>, out: Box<dyn Write + Send>) -> TextPort {
        TextPort {
            input,
            out,
            line_buf: Vec::new(),
        }
    }

    /// `fgets(line, 4096, stdin)`: the next piece of at most 4095 bytes, new line included.
    fn fgets(&mut self) -> Option<Vec<u8>> {
        if self.line_buf.is_empty() {
            match self.input.read_until(b'\n', &mut self.line_buf) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
        }
        let n = self.line_buf.len().min(4095);
        let rest = self.line_buf.split_off(n);
        Some(std::mem::replace(&mut self.line_buf, rest))
    }
}

/// A C string ends at its first NUL.
fn c_str(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

fn schedule(b: &[u8]) -> Option<Schedule> {
    Schedule::parse(std::str::from_utf8(b).ok()?).ok()
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// One request line (serve mode).
pub fn read_request(inp: &[u8]) -> Req {
    let inp = c_str(inp);
    if inp.starts_with(b"QUIT") {
        return Req::End;
    }
    if inp.starts_with(b"RESUME ") {
        let mut s = Scan::new(inp);
        s.lit("RESUME").ws();
        let tag = s.word(63).map(lossy);
        let name = s.ws().word(63).map(lossy);
        return match (tag, name) {
            (Some(tag), Some(snapshot)) => Req::Request(Request::Resume { tag, snapshot }),
            _ => Req::BadRun,
        };
    }
    // RUN and BRANCH (the shell-init images' requests) were deleted in Milestone 145
    if !inp.starts_with(b"CTL ") {
        return Req::BadRun;
    }
    let rq = &inp[4..];
    let mut s = Scan::new(rq);
    let tag = s.word(63).map(lossy);
    s.ws();
    let off = if s.ok() { s.pos } else { 0 };
    let sc = match (&tag, off) {
        (Some(_), o) if o != 0 => schedule(&rq[o..]),
        _ => None,
    };
    match (tag, sc) {
        (Some(tag), Some(schedule)) => Req::Request(Request::Ctl { tag, schedule }),
        _ => Req::BadRun,
    }
}

/// One host line at a stop (its new line and carriage returns already cut).
pub fn read_command(ln: &[u8]) -> Cmd {
    let ln = c_str(ln);
    if ln == b"CONT" {
        return Cmd::Host(HostCmd::Cont);
    }
    if ln.starts_with(b"END") && (ln.len() == 3 || ln[3] == b' ') {
        return Cmd::Host(HostCmd::End((ln.len() > 3).then(|| lossy(&ln[4..]))));
    }
    if let Some(r) = ln.strip_prefix(b"SEND at=") {
        let (at, used) = strtoull(r, 10);
        let rest = &r[used..];
        let payload = if used == 0 || rest.first() != Some(&b' ') {
            None
        } else {
            ctl::unesc(&rest[1..])
        };
        return match payload {
            Some(bytes) => Cmd::Host(HostCmd::Send { at, bytes }),
            None => Cmd::Bad("send bad request".into()),
        };
    }
    if let Some(r) = ln.strip_prefix(b"ACK ") {
        return Cmd::Host(HostCmd::Ack(strtoull(r, 10).0));
    }
    if let Some(r) = ln.strip_prefix(b"SNAP ") {
        let mut sc = Scan::new(r);
        let name = sc.word(63).map(lossy);
        let mode = name.as_ref().and_then(|_| sc.word(15).map(|w| w.to_vec()));
        let n = sc.n;
        return match name {
            Some(name) if n >= 1 && !(n == 2 && mode.as_deref() != Some(b"retype")) => {
                Cmd::Host(HostCmd::Snap {
                    name,
                    retype: n >= 2,
                })
            }
            _ => Cmd::Bad("snap bad request".into()),
        };
    }
    if ln == b"DISKOFF" {
        return Cmd::Host(HostCmd::DiskOff);
    }
    if ln == b"CUT" {
        return Cmd::Host(HostCmd::Cut);
    }
    if ln == b"TIME" {
        return Cmd::Host(HostCmd::Time(String::new()));
    }
    if let Some(r) = ln.strip_prefix(b"TIME ") {
        return Cmd::Host(HostCmd::Time(lossy(r)));
    }
    let mut l = "unknown command ".to_string();
    ctl::esc(ln, &mut l);
    Cmd::Bad(l)
}

impl Port for TextPort {
    fn emit(&mut self, m: EngineMsg) {
        let _ = self.out.write_all(m.line().as_bytes());
        let _ = self.out.write_all(b"\n");
        let _ = self.out.flush();
    }

    fn request(&mut self) -> Req {
        match self.fgets() {
            None => Req::End,
            Some(raw) => read_request(&raw),
        }
    }

    fn command(&mut self) -> Cmd {
        let mut l = Vec::new();
        match self.input.read_until(b'\n', &mut l) {
            Ok(0) | Err(_) => Cmd::Closed,
            Ok(_) => {
                while matches!(l.last(), Some(b'\n') | Some(b'\r')) {
                    l.pop();
                }
                read_command(&l)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(r: Req) -> String {
        match r {
            Req::Request(r) => r.line(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn requests() {
        // RUN and BRANCH were the shell-init images' requests (deleted in M145)
        assert_eq!(
            line(read_request(b"RUN a disk=none dseed=1 | run K=redis@5+2\n")),
            "BadRun"
        );
        assert_eq!(
            line(read_request(b"CTL a  disk=none rb=16 | run\n")),
            "CTL a disk=none rb=16 | run"
        );
        assert_eq!(line(read_request(b"RESUME b same\n")), "RESUME b same");
        assert_eq!(
            line(read_request(
                b"BRANCH t -3 20 500000 @held disk=cut | run C=3\n"
            )),
            "BadRun"
        );
        assert_eq!(line(read_request(b"QUIT\n")), "End");
        // the C engine ran these; hydo-schedule refuses them
        assert_eq!(
            line(read_request(b"CTL a disk=none bogus=1 | run\n")),
            "BadRun"
        );
        assert_eq!(
            line(read_request(b"CTL a disk=none | run  K=redis@5+2\n")),
            "BadRun"
        );
        assert_eq!(line(read_request(b"RESUME b\n")), "BadRun");
        assert_eq!(line(read_request(b"HELLO\n")), "BadRun");
    }

    #[test]
    fn commands() {
        let c = |l: &[u8]| match read_command(l) {
            Cmd::Host(h) => h.line(),
            Cmd::Bad(r) => format!("ERR {r}"),
            Cmd::Closed => "closed".into(),
        };
        for l in [
            "CONT",
            "END",
            "END HYDO-ORACLE: OK",
            "SEND at=5 ping\\n",
            "ACK 3",
            "SNAP s",
            "SNAP s retype",
            "DISKOFF",
            "CUT",
        ] {
            assert_eq!(c(l.as_bytes()), l);
        }
        assert_eq!(c(b"SEND at=5ping"), "ERR send bad request");
        assert_eq!(c(b"SEND at=5 \\q"), "ERR send bad request");
        assert_eq!(c(b"ACK x"), "ACK 0");
        assert_eq!(c(b"SNAP s again"), "ERR snap bad request");
        assert_eq!(c(b"SNAP "), "ERR snap bad request");
        assert_eq!(c(b"ENDX"), "ERR unknown command ENDX");
        assert_eq!(c(b"END\x00junk"), "END");
        assert_eq!(c(b"PING\x01"), "ERR unknown command PING\\x01");
    }
}
