//! The few Go standard-library behaviours whose output the Go hydo-read printed and this port
//! must print byte for byte: UTF-8 decoding with Go's per-byte error rule, unicode.IsSpace,
//! strings.TrimSpace and strings.Fields, strconv.Quote (`%q`), strconv.FormatFloat(x, 'g', -1, n),
//! and the fmt.Sscanf forms the command line used ("%g", "%d %d").

/// `utf8.DecodeRune`: the rune at the start of `b` and its width; an invalid or truncated
/// sequence is `(None, 1)` (Go's RuneError of width 1), an empty input `(None, 0)`.
pub fn decode_rune(b: &[u8]) -> (Option<char>, usize) {
    if b.is_empty() {
        return (None, 0);
    }
    let c0 = b[0];
    if c0 < 0x80 {
        return (Some(c0 as char), 1);
    }
    let (n, min) = match c0 {
        0xc2..=0xdf => (2, 0x80u32),
        0xe0..=0xef => (3, 0x800),
        0xf0..=0xf4 => (4, 0x10000),
        _ => return (None, 1),
    };
    if b.len() < n {
        return (None, 1);
    }
    let mut v = (c0 as u32) & (0x7f >> n);
    for &c in &b[1..n] {
        if c & 0xc0 != 0x80 {
            return (None, 1);
        }
        v = (v << 6) | (c as u32 & 0x3f);
    }
    if v < min || v > 0x10ffff || (0xd800..=0xdfff).contains(&v) {
        return (None, 1);
    }
    (char::from_u32(v), n)
}

/// `unicode.IsSpace`.
pub fn is_space(c: char) -> bool {
    c.is_whitespace()
}

fn space_at(b: &[u8], i: usize) -> Option<usize> {
    match decode_rune(&b[i..]) {
        (Some(c), w) if is_space(c) => Some(w),
        _ => None,
    }
}

/// `strings.TrimSpace`.
pub fn trim_space(b: &[u8]) -> &[u8] {
    let mut s = 0;
    while s < b.len() {
        match space_at(b, s) {
            Some(w) => s += w,
            None => break,
        }
    }
    let mut e = b.len();
    while e > s {
        // find the start of the last rune
        let mut k = e - 1;
        while k > s && b[k] & 0xc0 == 0x80 && e - k < 4 {
            k -= 1;
        }
        match decode_rune(&b[k..e]) {
            (Some(c), w) if w == e - k && is_space(c) => e = k,
            _ => break,
        }
    }
    &b[s..e]
}

/// `strings.Fields`.
pub fn fields(b: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut start: Option<usize> = None;
    while i < b.len() {
        let (c, w) = decode_rune(&b[i..]);
        let w = w.max(1);
        if c.is_some_and(is_space) {
            if let Some(s) = start.take() {
                out.push(&b[s..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
        i += w;
    }
    if let Some(s) = start {
        out.push(&b[s..]);
    }
    out
}

/// `unicode.IsPrint`, close enough: ASCII graphic and space, and any other rune that is not a
/// control, format, separator other than U+0020, private-use or noncharacter code point.
fn is_print(c: char) -> bool {
    let u = c as u32;
    if u < 0x80 {
        return (0x20..0x7f).contains(&u);
    }
    if c.is_control() || (c.is_whitespace()) {
        return false;
    }
    !matches!(u,
        0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x180e | 0x200b..=0x200f
        | 0x202a..=0x202e | 0x2060..=0x206f | 0xfeff | 0xfff9..=0xfffb
        | 0xe000..=0xf8ff | 0xfdd0..=0xfdef | 0xf0000..=0x10ffff)
        && u & 0xfffe != 0xfffe
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// `strconv.Quote` (what `%q` prints for a string).
pub fn quote(s: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut i = 0;
    while i < s.len() {
        let (r, w) = decode_rune(&s[i..]);
        let Some(r) = r else {
            out.push_str("\\x");
            out.push(HEX[(s[i] >> 4) as usize] as char);
            out.push(HEX[(s[i] & 15) as usize] as char);
            i += 1;
            continue;
        };
        i += w;
        if r == '"' || r == '\\' {
            out.push('\\');
            out.push(r);
            continue;
        }
        if is_print(r) {
            out.push(r);
            continue;
        }
        match r {
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x0b' => out.push_str("\\v"),
            _ => {
                let u = r as u32;
                if u < 0x20 || u == 0x7f {
                    out.push_str("\\x");
                    out.push(HEX[(u >> 4) as usize] as char);
                    out.push(HEX[(u & 15) as usize] as char);
                } else if u < 0x10000 {
                    out.push_str(&format!("\\u{u:04x}"));
                } else {
                    out.push_str(&format!("\\U{u:08x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// `strconv.FormatFloat(x, 'g', -1, 64)` (`bits32`: `..., 32)`, which is also what `fmt.Sprint`
/// prints for a float32).
pub fn format_float(x: f64, bits32: bool) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    let e = if bits32 {
        format!("{:e}", x as f32)
    } else {
        format!("{x:e}")
    };
    let (neg, e) = match e.strip_prefix('-') {
        Some(r) => (true, r.to_string()),
        None => (false, e),
    };
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i64 = exp.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mant.bytes().filter(|c| c.is_ascii_digit()).collect();
    while digits.len() > 1 && digits.last() == Some(&b'0') {
        digits.pop();
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if digits == b"0" {
        out.push('0');
        return out;
    }
    let nd = digits.len() as i64;
    let dp = exp + 1;
    let x = dp - 1;
    if !(-4..6).contains(&x) {
        out.push(digits[0] as char);
        if nd > 1 {
            out.push('.');
            out.push_str(std::str::from_utf8(&digits[1..]).unwrap_or(""));
        }
        out.push('e');
        out.push(if x < 0 { '-' } else { '+' });
        let a = x.unsigned_abs();
        if a < 10 {
            out.push('0');
        }
        out.push_str(&a.to_string());
        return out;
    }
    let digit = |i: i64| -> char {
        if i >= 0 && i < nd {
            digits[i as usize] as char
        } else {
            '0'
        }
    };
    if dp > 0 {
        for i in 0..dp {
            out.push(digit(i));
        }
    } else {
        out.push('0');
    }
    let prec = (nd - dp).max(0);
    if prec > 0 {
        out.push('.');
        for i in 0..prec {
            out.push(digit(dp + i));
        }
    }
    out
}

/// fmt's scanner: the remaining input as runes.
struct Scan<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<(char, usize)> {
        match decode_rune(&self.s[self.i..]) {
            (Some(c), w) => Some((c, w)),
            (None, 0) => None,
            (None, _) => Some(('\u{fffd}', 1)),
        }
    }
    /// SkipSpace for Sscanf: spaces are skipped, a newline is an error.
    fn skip_space(&mut self) -> Result<(), ()> {
        while let Some((c, w)) = self.peek() {
            if c == '\n' {
                return Err(());
            }
            if !is_space(c) {
                break;
            }
            self.i += w;
        }
        Ok(())
    }
    fn accept(&mut self, set: &[u8]) -> bool {
        if let Some(&c) = self.s.get(self.i) {
            if set.contains(&c) {
                self.i += 1;
                return true;
            }
        }
        false
    }
    fn int(&mut self) -> Result<i64, ()> {
        self.skip_space()?;
        let start = self.i;
        self.accept(b"+-");
        let d0 = self.i;
        while self.accept(b"0123456789") {}
        if self.i == d0 {
            return Err(());
        }
        let tok = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| ())?;
        tok.parse::<i64>().map_err(|_| ())
    }
}

/// `fmt.Sscanf(s, "%d %d", &a, &b)` succeeding.
pub fn sscan_two_ints(s: &[u8]) -> Option<(i64, i64)> {
    let mut sc = Scan { s, i: 0 };
    let a = sc.int().ok()?;
    // a space in the format matches one or more spaces (not newlines) or the end of input
    let before = sc.i;
    while let Some((c, w)) = sc.peek() {
        if c == '\n' || !is_space(c) {
            break;
        }
        sc.i += w;
    }
    if sc.i == before && sc.i < s.len() {
        return None;
    }
    let b = sc.int().ok()?;
    Some((a, b))
}

/// `fmt.Sscanf(s, "%g", &f)` succeeding.
pub fn sscan_float(s: &[u8]) -> Option<f64> {
    let mut sc = Scan { s, i: 0 };
    sc.skip_space().ok()?;
    let start = sc.i;
    sc.accept(b"+-");
    let d0 = sc.i;
    while sc.accept(b"0123456789") {}
    let mut nd = sc.i - d0;
    if sc.accept(b".") {
        let f0 = sc.i;
        while sc.accept(b"0123456789") {}
        nd += sc.i - f0;
    }
    if nd == 0 {
        return None;
    }
    if sc.accept(b"eE") {
        sc.accept(b"+-");
        let e0 = sc.i;
        while sc.accept(b"0123456789") {}
        if sc.i == e0 {
            return None;
        }
    }
    std::str::from_utf8(&s[start..sc.i]).ok()?.parse().ok()
}

/// Go's `syscall.Errno` text for an I/O error (the lower-case strerror Go prints).
pub fn errno_text(e: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    let s = match e.kind() {
        K::NotFound => "no such file or directory",
        K::PermissionDenied => "permission denied",
        K::ConnectionRefused => "connection refused",
        K::ConnectionReset => "connection reset by peer",
        K::ConnectionAborted => "software caused connection abort",
        K::NotConnected => "transport endpoint is not connected",
        K::AddrInUse => "address already in use",
        K::AddrNotAvailable => "cannot assign requested address",
        K::BrokenPipe => "broken pipe",
        K::AlreadyExists => "file exists",
        K::WouldBlock => "resource temporarily unavailable",
        K::InvalidInput => "invalid argument",
        K::TimedOut => "connection timed out",
        K::Interrupted => "interrupted system call",
        K::OutOfMemory => "cannot allocate memory",
        K::HostUnreachable => "no route to host",
        K::NetworkUnreachable => "network is unreachable",
        K::NetworkDown => "network is down",
        K::IsADirectory => "is a directory",
        K::NotADirectory => "not a directory",
        K::ReadOnlyFilesystem => "read-only file system",
        K::InvalidFilename => "file name too long",
        K::TooManyLinks => "too many links",
        K::StorageFull => "no space left on device",
        K::ResourceBusy => "device or resource busy",
        _ => {
            return match e.raw_os_error() {
                Some(5) => "input/output error".into(),
                Some(6) => "no such device or address".into(),
                Some(24) => "too many open files".into(),
                Some(40) => "too many levels of symbolic links".into(),
                Some(n) => format!("errno {n}"),
                None => e.to_string(),
            };
        }
    };
    s.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_as_go_prints_them() {
        for (x, want) in [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (0.1, "0.1"),
            (100000.0, "100000"),
            (123456.0, "123456"),
            (1234567.0, "1.234567e+06"),
            (1e21, "1e+21"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-1.5e-7, "-1.5e-07"),
            (3.14159, "3.14159"),
            (1e100, "1e+100"),
            (f64::INFINITY, "+Inf"),
        ] {
            assert_eq!(format_float(x, false), want, "{x}");
        }
        assert_eq!(format_float(0.1f32 as f64, true), "0.1");
        assert_eq!(format_float(16777216.0, true), "1.6777216e+07");
    }

    #[test]
    fn quote_as_go() {
        assert_eq!(quote(b"a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(quote(b"\n\x01\xff"), r#""\n\x01\xff""#);
        assert_eq!(quote("é".as_bytes()), "\"é\"");
        assert_eq!(quote("\u{a0}".as_bytes()), r#""\u00a0""#);
    }

    #[test]
    fn spaces() {
        assert_eq!(trim_space(" \t a b \u{a0}".as_bytes()), b"a b");
        assert_eq!(fields(b" 1  ack \n"), vec![&b"1"[..], b"ack"]);
        assert_eq!(trim_space(b"\xff "), b"\xff");
    }

    #[test]
    fn scanf() {
        assert_eq!(sscan_two_ints(b"0 6"), Some((0, 6)));
        assert_eq!(sscan_two_ints(b"+1 7x"), Some((1, 7)));
        assert_eq!(sscan_two_ints(b"1x 7"), None);
        assert_eq!(sscan_two_ints(b"a 7"), None);
        assert_eq!(sscan_float(b"2.5"), Some(2.5));
        assert_eq!(sscan_float(b"2s"), Some(2.0));
        assert_eq!(sscan_float(b"x"), None);
    }
}
