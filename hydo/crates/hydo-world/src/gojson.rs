//! JSON as Go's `encoding/json` sees a request body: `json.Unmarshal` into `map[string]any`
//! (numbers are float64, invalid UTF-8 becomes U+FFFD, a number out of range becomes nil) and
//! `json.Marshal` of what the `json:` extractor finds when it is an object or an array (sorted
//! keys, HTML-safe escaping, Go's float format).

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(BTreeMap<String, J>),
}

const MAX_DEPTH: usize = 10000;

/// `json.Unmarshal(body, &map)`: the object, or `None` when the body is not valid JSON or not an
/// object (Go leaves the map nil then).
pub fn parse_object(b: &[u8]) -> Option<BTreeMap<String, J>> {
    let mut p = P { b, i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != b.len() {
        return None;
    }
    match v {
        J::Obj(m) => Some(m),
        _ => None,
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn lit(&mut self, s: &[u8], v: J) -> Option<J> {
        if self.b[self.i..].starts_with(s) {
            self.i += s.len();
            Some(v)
        } else {
            None
        }
    }

    fn value(&mut self, depth: usize) -> Option<J> {
        if depth >= MAX_DEPTH {
            return None;
        }
        match self.peek()? {
            b'{' => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.peek()? == b'}' {
                    self.i += 1;
                    return Some(J::Obj(m));
                }
                loop {
                    self.ws();
                    if self.peek()? != b'"' {
                        return None;
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.peek()? != b':' {
                        return None;
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                    self.ws();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b'}' => {
                            self.i += 1;
                            return Some(J::Obj(m));
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.peek()? == b']' {
                    self.i += 1;
                    return Some(J::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b']' => {
                            self.i += 1;
                            return Some(J::Arr(a));
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self.string().map(J::Str),
            b't' => self.lit(b"true", J::Bool(true)),
            b'f' => self.lit(b"false", J::Bool(false)),
            b'n' => self.lit(b"null", J::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn digits(&mut self) -> usize {
        let s = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        self.i - s
    }

    fn number(&mut self) -> Option<J> {
        let s = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek()? {
            b'0' => self.i += 1,
            b'1'..=b'9' => {
                self.digits();
            }
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if self.digits() == 0 {
                return None;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return None;
            }
        }
        let t = std::str::from_utf8(&self.b[s..self.i]).ok()?;
        match t.parse::<f64>() {
            Ok(f) if f.is_finite() => Some(J::Num(f)),
            _ => Some(J::Null), // Go: ParseFloat's range error, the value is nil
        }
    }

    fn hex4(&self, at: usize) -> Option<u32> {
        let h = self.b.get(at..at + 4)?;
        let mut v = 0u32;
        for &c in h {
            v = v * 16 + (c as char).to_digit(16)?;
        }
        Some(v)
    }

    /// A string: validated as Go's scanner does, decoded as Go's `unquote` does.
    fn string(&mut self) -> Option<String> {
        self.i += 1; // the opening quote
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            match c {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    let e = *self.b.get(self.i + 1)?;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e as char),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let r = self.hex4(self.i + 2)?;
                            self.i += 6;
                            if (0xD800..0xE000).contains(&r) {
                                // a surrogate: a valid pair with the next \u, else U+FFFD
                                if self.b.get(self.i) == Some(&b'\\')
                                    && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    if let Some(r2) = self.hex4(self.i + 2) {
                                        if (0xD800..0xDC00).contains(&r)
                                            && (0xDC00..0xE000).contains(&r2)
                                        {
                                            let cp = 0x10000 + ((r - 0xD800) << 10) + (r2 - 0xDC00);
                                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                            self.i += 6;
                                            continue;
                                        }
                                    }
                                }
                                out.push('\u{fffd}');
                            } else {
                                out.push(char::from_u32(r).unwrap_or('\u{fffd}'));
                            }
                            continue;
                        }
                        _ => return None,
                    }
                    self.i += 2;
                }
                0..=0x1f => return None,
                0x20..=0x7f => {
                    out.push(c as char);
                    self.i += 1;
                }
                _ => {
                    let n = utf8_len(c);
                    match self.b.get(self.i..self.i + n).map(std::str::from_utf8) {
                        Some(Ok(s)) if n > 1 => {
                            out.push_str(s);
                            self.i += n;
                        }
                        _ => {
                            out.push('\u{fffd}');
                            self.i += 1;
                        }
                    }
                }
            }
        }
    }
}

fn utf8_len(c: u8) -> usize {
    match c {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 1,
    }
}

/// `json.Marshal(v)` for a decoded value.
pub fn marshal(v: &J) -> String {
    let mut s = String::new();
    enc(v, &mut s);
    s
}

fn enc(v: &J, s: &mut String) {
    match v {
        J::Null => s.push_str("null"),
        J::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
        J::Num(f) => s.push_str(&float(*f)),
        J::Str(t) => enc_str(t, s),
        J::Arr(a) => {
            s.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                enc(x, s);
            }
            s.push(']');
        }
        J::Obj(m) => {
            s.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                enc_str(k, s);
                s.push(':');
                enc(x, s);
            }
            s.push('}');
        }
    }
}

fn enc_str(t: &str, s: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    s.push('"');
    for c in t.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\u{8}' => s.push_str("\\b"),
            '\u{c}' => s.push_str("\\f"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            '<' | '>' | '&' | '\u{0}'..='\u{1f}' => {
                let b = c as u8;
                s.push_str("\\u00");
                s.push(HEX[(b >> 4) as usize] as char);
                s.push(HEX[(b & 15) as usize] as char);
            }
            '\u{2028}' => s.push_str("\\u2028"),
            '\u{2029}' => s.push_str("\\u2029"),
            c => s.push(c),
        }
    }
    s.push('"');
}

/// Go's JSON float format: 'f' for 1e-6 <= |f| < 1e21, else 'e' with a two-digit exponent
/// cleaned up ("1e-07" -> "1e-7").
fn float(f: f64) -> String {
    let a = f.abs();
    if a != 0.0 && !(1e-6..1e21).contains(&a) {
        let e = format!("{f:e}");
        let (m, x) = e.split_once('e').unwrap_or((&e, "0"));
        let x: i32 = x.parse().unwrap_or(0);
        let mut out = format!("{m}e{}{:02}", if x < 0 { '-' } else { '+' }, x.abs());
        let n = out.len();
        let b = out.as_bytes();
        if n >= 4 && b[n - 4] == b'e' && b[n - 3] == b'-' && b[n - 2] == b'0' {
            let last = b[n - 1] as char;
            out.truncate(n - 2);
            out.push(last);
        }
        out
    } else {
        format_f(f)
    }
}

/// `strconv.FormatFloat(f, 'f', -1, 64)`.
pub fn format_f(f: f64) -> String {
    format!("{f}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects() {
        let m =
            parse_object(br#"{"order":"o7","n":{"amount":12.5},"a":[1,"x"],"b":true}"#).unwrap();
        assert_eq!(m["order"], J::Str("o7".into()));
        assert_eq!(marshal(&m["n"]), r#"{"amount":12.5}"#);
        assert_eq!(marshal(&m["a"]), r#"[1,"x"]"#);
        assert!(parse_object(b"[1]").is_none());
        assert!(parse_object(b"{} x").is_none());
        assert!(parse_object(b"{\"a\":01}").is_none());
        assert!(parse_object(b" {\"a\":1}\n").is_some());
        assert!(parse_object(b"null").is_none());
        let m = parse_object(b"{\"a\":1e400,\"b\":\"\xf0\x9f\x98\x80\\ud800x\",\"c\":\"<&>\"}")
            .unwrap();
        assert_eq!(m["a"], J::Null);
        assert_eq!(m["b"], J::Str("\u{1f600}\u{fffd}x".into()));
        assert_eq!(marshal(&m["c"]), r#""\u003c\u0026\u003e""#); // Go escapes HTML characters
        let m = parse_object(b"{\"a\":\"\xe2\x82x\"}").unwrap();
        assert_eq!(m["a"], J::Str("\u{fffd}\u{fffd}x".into()));
    }

    #[test]
    fn floats() {
        assert_eq!(format_f(12.5), "12.5");
        assert_eq!(format_f(1e21), "1000000000000000000000");
        assert_eq!(format_f(150.0), "150");
        assert_eq!(float(1e21), "1e+21");
        assert_eq!(float(1.5e-7), "1.5e-7");
        assert_eq!(float(1e-10), "1e-10");
        assert_eq!(float(0.000001), "0.000001");
        assert_eq!(float(-2.5e300), "-2.5e+300");
        assert_eq!(float(0.0), "0");
    }
}
