//! Framing on the console.
//!
//! The agent writes every line it sends as `HYDO-AG <len> <crc32> <json>\n`: the prefix, the
//! byte length of the JSON text in decimal, its CRC-32 (IEEE 802.3, the zlib one) as eight
//! lower-case hex digits, and the canonical JSON text. The kernel's own messages share the UART,
//! so the host reads the console line by line ([`Deframer`]) and takes only lines that carry the
//! prefix; one whose length or CRC does not match is a [`Deframed::Corrupt`] line, an error of the
//! run (the same in every replay, so never flaky, only wrong).
//!
//! The host's lines to the agent are not framed: one canonical JSON text and `\n`, typed into the
//! serial input. A line that starts with the byte 0x18 (CAN) after other bytes makes the agent
//! drop what it had read of the current line first ([`CAN`], used by the resynchronisation).

/// The prefix of every agent line.
pub const PREFIX: &str = "HYDO-AG ";
/// The byte that discards a partly read input line (the agent reports how many bytes it dropped).
pub const CAN: u8 = 0x18;

/// CRC-32/IEEE (zlib's `crc32`).
pub fn crc32(data: &[u8]) -> u32 {
    let mut c: u32 = 0xffff_ffff;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// The framed console line for one JSON text, with its newline.
pub fn frame(json: &str) -> String {
    format!(
        "{PREFIX}{} {:08x} {json}\n",
        json.len(),
        crc32(json.as_bytes())
    )
}

/// What a console line is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deframed {
    /// An agent line: its JSON text (length and CRC checked).
    Agent(String),
    /// A line that carries the prefix but whose header, length or CRC is wrong.
    Corrupt { line: String, why: &'static str },
    /// Any other console text (kernel messages, a service's output).
    Other(String),
}

/// Reads one console line (without its newline; a trailing `\r` is ignored).
pub fn deframe_line(line: &[u8]) -> Deframed {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let lossy = || String::from_utf8_lossy(line).into_owned();
    let Some(at) = find(line, PREFIX.as_bytes()) else {
        return Deframed::Other(lossy());
    };
    let rest = &line[at + PREFIX.len()..];
    let corrupt = |why| Deframed::Corrupt { line: lossy(), why };
    let Some(sp) = rest.iter().position(|&b| b == b' ') else {
        return corrupt("no length");
    };
    let Some(len) = std::str::from_utf8(&rest[..sp])
        .ok()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<usize>().ok())
    else {
        return corrupt("a bad length");
    };
    let rest = &rest[sp + 1..];
    if rest.len() < 9 || rest[8] != b' ' {
        return corrupt("no CRC");
    }
    let Some(crc) = std::str::from_utf8(&rest[..8])
        .ok()
        .filter(|s| s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .and_then(|s| u32::from_str_radix(s, 16).ok())
    else {
        return corrupt("a bad CRC field");
    };
    let json = &rest[9..];
    if json.len() != len {
        return corrupt("the length does not match");
    }
    if crc32(json) != crc {
        return corrupt("the CRC does not match");
    }
    match String::from_utf8(json.to_vec()) {
        Ok(s) => Deframed::Agent(s),
        Err(_) => corrupt("invalid UTF-8"),
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Splits a console byte stream into lines and deframes them. Bytes after the last newline wait
/// for the next [`Deframer::push`].
#[derive(Debug, Default, Clone)]
pub struct Deframer {
    buf: Vec<u8>,
}

impl Deframer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds console bytes; returns every complete line read so far.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Deframed> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(nl) = self.buf[start..].iter().position(|&b| b == b'\n') {
            out.push(deframe_line(&self.buf[start..start + nl]));
            start += nl + 1;
        }
        self.buf.drain(..start);
        out
    }

    /// The bytes of an unfinished last line.
    pub fn pending(&self) -> &[u8] {
        &self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_the_check_string() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn frames_round_trip_and_corruption_is_seen() {
        let j = r#"{"ev":"hello","seq":0,"v":1}"#;
        let f = frame(j);
        assert_eq!(f, format!("HYDO-AG 28 {:08x} {j}\n", crc32(j.as_bytes())));
        let mut d = Deframer::new();
        let mut out = d.push(b"[    1.0] kernel says hi\n");
        out.extend(d.push(&f.as_bytes()[..10]));
        assert!(d.pending().starts_with(b"HYDO-AG"));
        out.extend(d.push(&f.as_bytes()[10..]));
        out.extend(d.push(f.replace("hello", "hellp").as_bytes()));
        out.extend(d.push(b"junk HYDO-AG 2 00000000 {}\r\n"));
        assert_eq!(out[0], Deframed::Other("[    1.0] kernel says hi".into()));
        assert_eq!(out[1], Deframed::Agent(j.into()));
        assert!(matches!(out[2], Deframed::Corrupt { why: "the CRC does not match", .. }));
        assert!(matches!(out[3], Deframed::Corrupt { why: "the CRC does not match", .. }));
        assert_eq!(out.len(), 4);
        assert_eq!(
            deframe_line(format!("x {}", frame("{}").trim_end()).as_bytes()),
            Deframed::Agent("{}".into())
        );
    }
}
