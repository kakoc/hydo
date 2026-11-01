//! Small helpers the feature controllers of M123 share (history, error logs, the composer): a
//! file read whole and byte for byte, the guest's monotonic clock in the shell's centiseconds,
//! and the shell's quoting and line splitting.

use super::{call, F};
use crate::flow::Step;
use hydo_agent_proto::msg::{ClockGet, Command, ErrCode, Read, Reply};

/// The largest chunk read as hex (its reply line stays near the text reads' 3,000 bytes).
const HEX_CHUNK: u32 = 1500;

/// A whole file as bytes, as `cat file 2>/dev/null` gives it: chunks of text, and a chunk the
/// agent cannot send as UTF-8 (a log with other bytes, or a chunk that cuts a character) again as
/// hex. A missing or unreadable file reads as empty, or as what was read before the refusal.
pub(super) fn read_bytes(fx: &mut F<'_>, path: &str) -> Step<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        let offset = (!out.is_empty()).then_some(out.len() as u64);
        let r = fx.send(Command::Read(Read {
            path: path.into(),
            offset,
            max: super::CHUNK as u32,
            hex: None,
        }))?;
        let (data, eof) = match fx.reply(r)? {
            Ok(Reply::Read(r)) => (r.data.into_bytes(), r.eof),
            Err(e) if e.err == ErrCode::NotUtf8.as_str() => {
                let h = fx.send(Command::Read(Read {
                    path: path.into(),
                    offset,
                    max: HEX_CHUNK,
                    hex: Some(true),
                }))?;
                match fx.reply(h)? {
                    Ok(Reply::Read(r)) => (unhex(&r.data), r.eof),
                    _ => return Ok(out),
                }
            }
            _ => return Ok(out),
        };
        let done = eof || data.is_empty();
        out.extend_from_slice(&data);
        if done {
            return Ok(out);
        }
    }
}

fn unhex(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let v = |c: u8| match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    };
    b.chunks(2)
        .filter(|p| p.len() == 2)
        .map(|p| v(p[0]) << 4 | v(p[1]))
        .collect()
}

/// The shell's `tnow` (hydo-composer.sh): the guest's uptime in centiseconds, from the agent's
/// monotonic clock (CLOCK_MONOTONIC, which /proc/uptime also counts).
pub(super) fn centis(fx: &mut F<'_>) -> Step<i64> {
    match call(fx, Command::ClockGet(ClockGet {}))? {
        Ok(Reply::ClockGet(c)) => Ok((c.mono_ms / 10) as i64),
        _ => Ok(0),
    }
}

/// `'…'` quoting for a shell word.
pub(super) fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The lines grep and awk read from a file: split at newlines, a last line without one counts.
pub(super) fn records(b: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = b.split(|c| *c == b'\n').collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// Bytes as report text (a byte that is not UTF-8 becomes U+FFFD; the shell printed it raw).
pub(super) fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(unhex("41e9ff0a"), vec![0x41, 0xe9, 0xff, 0x0a]);
        assert_eq!(shq("a'b"), "'a'\\''b'");
        assert_eq!(records(b"a\n\nb"), vec![&b"a"[..], b"", b"b"]);
        assert_eq!(records(b"a\n"), vec![&b"a"[..]]);
        assert!(records(b"").is_empty());
    }
}
