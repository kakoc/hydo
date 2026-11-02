//! Lock-step control's bookkeeping (M118, `hvx_explore.c` "Lock-step control"): the host's sends
//! and their states, and the escaping of console text and payloads on the text protocol.

/// `ctl_esc`: printable ASCII as is, `\\`, `\n`, `\r`, `\t`, every other byte `\xHH`.
pub fn esc(p: &[u8], out: &mut String) {
    for &c in p {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(c as char),
            _ => out.push_str(&format!("\\x{c:02x}")),
        }
    }
}

/// `ctl_unesc`: the inverse of [`esc`]; None on a malformed escape.
pub fn unesc(s: &[u8]) -> Option<Vec<u8>> {
    let mut o = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'\\' {
            o.push(s[i]);
            i += 1;
            continue;
        }
        i += 1;
        match s.get(i) {
            Some(b'\\') => o.push(b'\\'),
            Some(b'n') => o.push(b'\n'),
            Some(b'r') => o.push(b'\r'),
            Some(b't') => o.push(b'\t'),
            Some(b'x')
                if s.get(i + 1).is_some_and(|c| c.is_ascii_hexdigit())
                    && s.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit()) =>
            {
                let h = std::str::from_utf8(&s[i + 1..i + 3]).ok()?;
                o.push(u8::from_str_radix(h, 16).ok()?);
                i += 2;
            }
            _ => return None,
        }
        i += 1;
    }
    Some(o)
}

pub const MAX_SENDS: usize = 1024;
pub const MAX_SNAPS: usize = 8;
/// The module's serial receive ring (HVX_INPUT_SZ in hvx.c).
pub const SEND_MAX: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendState {
    Pending,
    Typed,
    Acked,
    Retype,
}

#[derive(Clone, Debug)]
pub struct Send {
    pub id: u64,
    pub at: u64,
    pub sent: u64,
    pub state: SendState,
    pub buf: Vec<u8>,
}

/// `struct ctl_book`: what a snapshot carries besides the VM.
#[derive(Clone, Debug, Default)]
pub struct Book {
    pub nstop: u64,
    pub nev: u64,
    pub next_id: u64,
    pub ntyped: u64,
    pub inh: u64,
    pub evfrom: usize,
    /// M138: where the next EVENT line of the host-call records starts.
    pub pevfrom: usize,
    pub s: Vec<Send>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_round_trips() {
        let b = b"a\\b\nc\rd\te\x01\xff ~";
        let mut s = String::new();
        esc(b, &mut s);
        assert_eq!(s, "a\\\\b\\nc\\rd\\te\\x01\\xff ~");
        assert_eq!(unesc(s.as_bytes()).unwrap(), b.to_vec());
        assert!(unesc(b"x\\").is_none());
        assert!(unesc(b"\\xg0").is_none());
        assert!(unesc(b"\\q").is_none());
    }
}
