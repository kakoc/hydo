//! Example transcripts: what the host typed and what the agent wrote, in order.
//!
//! One entry per line of a text file:
//! - `# ...` a comment (the first lines say what the transcript shows), and empty lines;
//! - `> <json>` a host line, typed as the JSON text followed by `\n`;
//! - `>> <escaped>` bytes typed exactly as given (for broken or partial input and the CAN byte),
//!   with the escapes `\\`, `\n`, `\r`, `\t` and `\xHH` (the lock-step engines' escaping);
//! - `< <line>` one agent line exactly as it appeared on the console, without its newline.
//!
//! The files in `hydo/schema/examples/agent/v1/` are recorded from real runs of the agent and
//! never edited after they are committed. [`check`] reads one as the host would: the first agent
//! line is a hello the session accepts, every agent line is framed, canonical and numbered in
//! order, and every reply or refusal answers a command that was typed.

use crate::frame::{deframe_line, Deframed, CAN};
use crate::json::is_canonical;
use crate::line::HostLine;
use crate::session::{Incoming, Session};

/// One entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Comment(String),
    /// Bytes the host typed (with the newline of a `>` line).
    Host(Vec<u8>),
    /// An agent line as written (without its newline).
    Agent(String),
}

/// Reads a transcript.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let at = |m: String| format!("line {}: {m}", i + 1);
        if line.is_empty() {
            continue;
        }
        if let Some(c) = line.strip_prefix('#') {
            out.push(Entry::Comment(c.trim_start().to_string()));
        } else if let Some(r) = line.strip_prefix(">> ") {
            out.push(Entry::Host(unescape(r).map_err(at)?));
        } else if let Some(j) = line.strip_prefix("> ") {
            let mut b = j.as_bytes().to_vec();
            b.push(b'\n');
            out.push(Entry::Host(b));
        } else if let Some(a) = line.strip_prefix("< ") {
            out.push(Entry::Agent(a.to_string()));
        } else {
            return Err(at(format!("not a transcript line: {line}")));
        }
    }
    Ok(out)
}

/// Writes entries in the file form.
pub fn write(entries: &[Entry]) -> String {
    let mut s = String::new();
    for e in entries {
        match e {
            Entry::Comment(c) => {
                s.push_str("# ");
                s.push_str(c);
            }
            Entry::Host(b) => match b.strip_suffix(b"\n") {
                Some(j)
                    if !j.is_empty()
                        && !j.contains(&b'\n')
                        && !j.contains(&CAN)
                        && std::str::from_utf8(j).is_ok()
                        && j[0] == b'{' =>
                {
                    s.push_str("> ");
                    s.push_str(std::str::from_utf8(j).unwrap_or_default());
                }
                _ => {
                    s.push_str(">> ");
                    s.push_str(&escape(b));
                }
            },
            Entry::Agent(a) => {
                s.push_str("< ");
                s.push_str(a);
            }
        }
        s.push('\n');
    }
    s
}

/// The escaping of `>>` lines.
pub fn escape(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s
}

/// The inverse of [`escape`].
pub fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b'\\') => out.push(b'\\'),
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            Some(b't') => out.push(b'\t'),
            Some(b'x') => {
                let h = s.get(i + 2..i + 4).ok_or("a short \\x escape")?;
                out.push(u8::from_str_radix(h, 16).map_err(|_| format!("a bad \\x escape {h}"))?);
                i += 2;
            }
            _ => return Err(format!("a bad escape at byte {i}")),
        }
        i += 2;
    }
    Ok(out)
}

/// What a transcript holds, as [`check`] counted it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub host_lines: usize,
    pub agent_lines: usize,
    /// Ops of the commands that were answered with a reply.
    pub replied: std::collections::BTreeSet<String>,
    /// Event names seen.
    pub events: std::collections::BTreeSet<String>,
    /// Error codes seen.
    pub refusals: std::collections::BTreeSet<String>,
}

/// Reads a transcript as the host would (see the module text).
pub fn check(entries: &[Entry]) -> Result<Summary, String> {
    let mut session: Option<Session> = None;
    let mut sum = Summary::default();
    for (k, e) in entries.iter().enumerate() {
        let at = |m: String| format!("entry {}: {m}", k + 1);
        match e {
            Entry::Comment(_) => {}
            Entry::Host(bytes) => {
                sum.host_lines += 1;
                let s = session
                    .as_mut()
                    .ok_or_else(|| at("a host line before the agent's hello".into()))?;
                // every complete line typed: the part after the last CAN
                for line in bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
                    let line = match line.iter().rposition(|&b| b == CAN) {
                        Some(p) => &line[p + 1..],
                        None => line,
                    };
                    match HostLine::decode(line, &[s.proto()]) {
                        Ok(h) => s.note_sent(h.id, h.cmd.name()),
                        Err(r) => {
                            if let (Some(id), Some(op)) = (r.re, r.op.as_deref()) {
                                s.note_sent(id, op);
                            }
                        }
                    }
                }
            }
            Entry::Agent(text) => {
                sum.agent_lines += 1;
                let json = match deframe_line(text.as_bytes()) {
                    Deframed::Agent(j) if text.starts_with(crate::frame::PREFIX) => j,
                    Deframed::Agent(_) => return Err(at("text before the agent's frame".into())),
                    Deframed::Corrupt { why, .. } => {
                        return Err(at(format!("corrupt frame: {why}")))
                    }
                    Deframed::Other(_) => return Err(at("not an agent line".into())),
                };
                if !is_canonical(json.as_bytes()) {
                    return Err(at(format!("not canonical JSON: {json}")));
                }
                let Some(s) = session.as_mut() else {
                    session = Some(Session::from_hello(&json).map_err(|e| at(e.to_string()))?);
                    sum.events.insert("hello".into());
                    continue;
                };
                match s.receive(&json).map_err(|e| at(e.to_string()))? {
                    Incoming::Reply { body, .. } => {
                        sum.replied.insert(body.name().into());
                    }
                    Incoming::Refused { error, .. } => {
                        sum.refusals.insert(error.err.clone());
                    }
                    Incoming::Event(ev) => {
                        sum.events.insert(ev.name().into());
                    }
                    Incoming::UnknownEvent { ev } => {
                        return Err(at(format!("an event version 1 does not know: {ev}")))
                    }
                }
            }
        }
    }
    if session.is_none() {
        return Err("no agent hello".into());
    }
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_round_trip() {
        let b = b"\x18{\"a\":\"\\n\"}\n\x00\xff";
        assert_eq!(unescape(&escape(b)).unwrap(), b);
        let e = vec![
            Entry::Comment("x".into()),
            Entry::Host(b"{\"id\":1}\n".to_vec()),
            Entry::Host(b"\x18{\"id\":2}\n".to_vec()),
            Entry::Agent("HYDO-AG 2 00000000 {}".into()),
        ];
        let t = write(&e);
        assert_eq!(
            t,
            "# x\n> {\"id\":1}\n>> \\x18{\"id\":2}\\n\n< HYDO-AG 2 00000000 {}\n"
        );
        assert_eq!(parse(&t).unwrap(), e);
    }
}
