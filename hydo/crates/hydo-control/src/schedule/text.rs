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

/// A whole file as lower-case hex, in chunks of [`HEX_CHUNK`] bytes, at most `max` bytes of it
/// (a binary file such as a screenshot, Milestone 254). A missing or unreadable file reads as
/// empty; a file longer than `max` as empty too, after one read, so that no file arrives cut.
/// With it, the file's size as the agent gave it (0 for a missing file; Milestone 264).
pub(super) fn read_hex_sized(fx: &mut F<'_>, path: &str, max: usize) -> Step<(String, u64)> {
    let mut out = String::new();
    loop {
        let offset = (!out.is_empty()).then_some((out.len() / 2) as u64);
        let r = fx.send(Command::Read(Read {
            path: path.into(),
            offset,
            max: HEX_CHUNK,
            hex: Some(true),
        }))?;
        match fx.reply(r)? {
            Ok(Reply::Read(r)) => {
                if r.size as usize > max {
                    return Ok((String::new(), r.size));
                }
                let done = r.eof || r.data.is_empty();
                out.push_str(&r.data);
                if done {
                    return Ok((out, r.size));
                }
            }
            _ => return Ok((String::new(), 0)),
        }
    }
}

/// Milestone 334: how many reads of [`read_hex_files`] are in flight at once. A read through the
/// agent costs about a tenth of a second on hvx when it waits for its reply alone (Milestone 264);
/// the window sends this many before it waits, and the agent answers them in one go.
pub(super) const WINDOW: usize = 16;

/// Milestone 334: whole files as lower-case hex, each with its size known before (the walker's
/// filmstrip names the size of every frame, and an unknown size is read first): every chunk of
/// [`HEX_CHUNK`] bytes of every file sent [`WINDOW`] at a time before the replies are awaited. A
/// file whose replies do not add up to its size (it changed, or a read failed) comes back empty.
pub(super) fn read_hex_files(fx: &mut F<'_>, files: &[(String, u64)]) -> Step<Vec<String>> {
    let mut chunks: Vec<(usize, u64)> = Vec::new();
    for (i, (_, size)) in files.iter().enumerate() {
        let mut off = 0u64;
        loop {
            chunks.push((i, off));
            off += HEX_CHUNK as u64;
            if off >= *size {
                break;
            }
        }
    }
    let mut out = vec![String::new(); files.len()];
    let mut bad = vec![false; files.len()];
    for win in chunks.chunks(WINDOW) {
        let mut ids = Vec::with_capacity(win.len());
        for (i, off) in win {
            ids.push(fx.send(Command::Read(Read {
                path: files[*i].0.clone(),
                offset: (*off > 0).then_some(*off),
                max: HEX_CHUNK,
                hex: Some(true),
            }))?);
        }
        for ((i, _), id) in win.iter().zip(ids) {
            match fx.reply(id)? {
                Ok(Reply::Read(r)) if r.size == files[*i].1 => out[*i].push_str(&r.data),
                _ => bad[*i] = true,
            }
        }
    }
    for (i, (_, size)) in files.iter().enumerate() {
        if bad[i] || out[i].len() as u64 != size * 2 {
            out[i].clear();
        }
    }
    Ok(out)
}

/// Milestone 334: [`read_hex_sized`] with the chunks after the first one sent [`WINDOW`] at a
/// time: the first read gives the size, and a file larger than `max` comes back empty after it.
pub(super) fn read_hex_window(fx: &mut F<'_>, path: &str, max: usize) -> Step<(String, u64)> {
    let r = fx.send(Command::Read(Read {
        path: path.into(),
        offset: None,
        max: HEX_CHUNK,
        hex: Some(true),
    }))?;
    let first = match fx.reply(r)? {
        Ok(Reply::Read(r)) => r,
        _ => return Ok((String::new(), 0)),
    };
    if first.size as usize > max {
        return Ok((String::new(), first.size));
    }
    if first.eof || first.data.is_empty() {
        return Ok((first.data, first.size));
    }
    let size = first.size;
    let mut chunks = Vec::new();
    let mut off = HEX_CHUNK as u64;
    while off < size {
        chunks.push(off);
        off += HEX_CHUNK as u64;
    }
    let mut out = first.data;
    for win in chunks.chunks(WINDOW) {
        let mut ids = Vec::with_capacity(win.len());
        for off in win {
            ids.push(fx.send(Command::Read(Read {
                path: path.into(),
                offset: Some(*off),
                max: HEX_CHUNK,
                hex: Some(true),
            }))?);
        }
        for id in ids {
            match fx.reply(id)? {
                Ok(Reply::Read(r)) if r.size == size => out.push_str(&r.data),
                _ => return Ok((String::new(), size)),
            }
        }
    }
    if out.len() as u64 != size * 2 {
        return Ok((String::new(), size));
    }
    Ok((out, size))
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
