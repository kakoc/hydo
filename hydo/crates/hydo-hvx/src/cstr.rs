//! The C library's number and string parsing, as `hvx_explore.c` uses it (glibc semantics), so a
//! schedule or a request line is read exactly as the C engine reads it, malformed ones included.

/// C `isspace` in the "C" locale.
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// `strtoull(s, &end, base)` for base 10 or 16: (value, bytes consumed; 0 when no number).
/// Leading white space, an optional sign (a minus negates in unsigned arithmetic), for base 16 an
/// optional `0x`; overflow gives `u64::MAX`.
pub fn strtoull(s: &[u8], base: u32) -> (u64, usize) {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    if base == 16
        && i + 1 < s.len()
        && s[i] == b'0'
        && (s[i + 1] | 0x20) == b'x'
        && s.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit())
    {
        i += 2;
    }
    let start = i;
    let mut v: u64 = 0;
    let mut over = false;
    while i < s.len() {
        let d = match (s[i] as char).to_digit(base) {
            Some(d) => d as u64,
            None => break,
        };
        match v.checked_mul(base as u64).and_then(|x| x.checked_add(d)) {
            Some(x) => v = x,
            None => over = true,
        }
        i += 1;
    }
    if i == start {
        // "0x" with no hex digit: glibc parses the 0 (handled above by requiring a digit)
        return (0, 0);
    }
    if over {
        return (u64::MAX, i);
    }
    (if neg { v.wrapping_neg() } else { v }, i)
}

/// `strtoull(s, NULL, 0)`: the base from the prefix (`0x` hex, `0` octal, else decimal).
pub fn strtoull0(s: &[u8]) -> u64 {
    let t: Vec<u8> = s.iter().copied().skip_while(|&c| is_space(c)).collect();
    let (neg, t) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, &t[..]),
    };
    let v = if t.len() >= 2 && t[0] == b'0' && (t[1] | 0x20) == b'x' {
        strtoull(t, 16).0
    } else if t.first() == Some(&b'0') {
        let mut v: u64 = 0;
        for &c in t.iter().take_while(|c| (b'0'..=b'7').contains(c)) {
            v = v.saturating_mul(8).saturating_add((c - b'0') as u64);
        }
        v
    } else {
        strtoull(t, 10).0
    };
    if neg {
        v.wrapping_neg()
    } else {
        v
    }
}

/// `atof` for the positive decimal numbers the engine reads from the environment.
pub fn atof(s: &str) -> f64 {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .take_while(|(i, c)| {
            c.is_ascii_digit() || *c == '.' || (*i == 0 && (*c == '-' || *c == '+'))
        })
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    t[..end].parse().unwrap_or(0.0)
}

/// `strtoul` on LP64 Linux (unsigned long = 64 bits): the same as [`strtoull`].
pub fn strtoul(s: &[u8], base: u32) -> (u64, usize) {
    strtoull(s, base)
}

/// `strtol(s, &end, 10)`: (value, consumed), saturating at the i64 range.
pub fn strtol(s: &[u8]) -> (i64, usize) {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut v: i128 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = (v * 10 + (s[i] - b'0') as i128).min(i64::MAX as i128 + 1);
        i += 1;
    }
    if i == start {
        return (0, 0);
    }
    let v = if neg { -v } else { v };
    (v.clamp(i64::MIN as i128, i64::MAX as i128) as i64, i)
}

/// `atoi`: strtol truncated to int.
pub fn atoi(s: &[u8]) -> i32 {
    strtol(s).0 as i32
}

/// `strstr`: the offset of the first `needle` in `hay`.
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if hay.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        match hay[i..=last].iter().position(|&c| c == first) {
            None => return None,
            Some(k) => {
                i += k;
                if &hay[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
        }
    }
    None
}

/// A cursor for the `sscanf` formats the engine uses: `%Ns`, `%d`, `%u`, `%llu`, literals,
/// white-space directives and `%n`.
pub struct Scan<'a> {
    s: &'a [u8],
    pub pos: usize,
    /// Conversions done so far (sscanf's return value, without the EOF case).
    pub n: i32,
    failed: bool,
}

impl<'a> Scan<'a> {
    pub fn new(s: &'a [u8]) -> Scan<'a> {
        Scan {
            s,
            pos: 0,
            n: 0,
            failed: false,
        }
    }

    pub fn ok(&self) -> bool {
        !self.failed
    }

    /// A white-space directive: skip any amount of white space (none included).
    pub fn ws(&mut self) -> &mut Self {
        if !self.failed {
            while self.pos < self.s.len() && is_space(self.s[self.pos]) {
                self.pos += 1;
            }
        }
        self
    }

    /// A literal (no white space in it).
    pub fn lit(&mut self, l: &str) -> &mut Self {
        if !self.failed {
            if self.s[self.pos..].starts_with(l.as_bytes()) {
                self.pos += l.len();
            } else {
                self.failed = true;
            }
        }
        self
    }

    /// `%Ns`: skips white space, then at most `max` non-white-space bytes.
    pub fn word(&mut self, max: usize) -> Option<&'a [u8]> {
        if self.failed {
            return None;
        }
        self.ws();
        let start = self.pos;
        while self.pos < self.s.len() && !is_space(self.s[self.pos]) && self.pos - start < max {
            self.pos += 1;
        }
        if self.pos == start {
            self.failed = true;
            return None;
        }
        self.n += 1;
        Some(&self.s[start..self.pos])
    }

    fn num(&mut self, signed: bool) -> Option<u64> {
        if self.failed {
            return None;
        }
        let rest = &self.s[self.pos..];
        let (v, used) = if signed {
            let (v, u) = strtol(rest);
            (v as u64, u)
        } else {
            strtoull(rest, 10)
        };
        if used == 0 {
            self.failed = true;
            return None;
        }
        self.pos += used;
        self.n += 1;
        Some(v)
    }

    /// `%d`.
    pub fn int(&mut self) -> Option<i32> {
        self.num(true).map(|v| v as i64 as i32)
    }

    /// `%u` (an unsigned int; a minus sign wraps).
    pub fn uint(&mut self) -> Option<u32> {
        self.num(false).map(|v| v as u32)
    }

    /// `%llu`.
    pub fn ull(&mut self) -> Option<u64> {
        self.num(false)
    }

    /// `%llx`.
    pub fn hex(&mut self) -> Option<u64> {
        if self.failed {
            return None;
        }
        let (v, used) = strtoull(&self.s[self.pos..], 16);
        if used == 0 {
            self.failed = true;
            return None;
        }
        self.pos += used;
        self.n += 1;
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtoull_like_glibc() {
        assert_eq!(strtoull(b"123 x", 10), (123, 3));
        assert_eq!(strtoull(b"  42", 10), (42, 4));
        assert_eq!(strtoull(b"x", 10), (0, 0));
        assert_eq!(strtoull(b"", 10), (0, 0));
        assert_eq!(strtoull(b"-1", 10), (u64::MAX, 2));
        assert_eq!(strtoull(b"ff ", 16), (255, 2));
        assert_eq!(strtoull(b"0x1f", 16), (31, 4));
        assert_eq!(strtoull(b"0xg", 16), (0, 1));
        assert_eq!(strtoull(b"99999999999999999999", 10), (u64::MAX, 20));
        assert_eq!(strtoull(b"4d5cb825dfaab05b", 16), (0x4d5cb825dfaab05b, 16));
    }

    #[test]
    fn scanning() {
        let line = b"BRANCH t1 -3 10 500000 @held disk=none | run\n";
        let mut s = Scan::new(line);
        s.lit("BRANCH").ws();
        assert_eq!(s.word(63), Some(&b"t1"[..]));
        assert_eq!(s.ws().int(), Some(-3));
        assert_eq!(s.ws().uint(), Some(10));
        assert_eq!(s.ws().ull(), Some(500000));
        assert_eq!(s.ws().word(127), Some(&b"@held"[..]));
        s.ws();
        assert_eq!(s.n, 5);
        assert_eq!(&line[s.pos..], b"disk=none | run\n");
        let mut t = Scan::new(b"RUN ");
        t.lit("RUN").ws();
        assert_eq!(t.word(63), None);
        assert_eq!(t.n, 0);
        assert_eq!(find(b"abcabd", b"abd"), Some(3));
        assert_eq!(find(b"ab", b"abc"), None);
    }
}
