//! Canonical JSON: the one encoding of every protocol line.
//!
//! A value is `null`, a boolean, an integer (no fractions, no exponents: the protocol carries no
//! floats), a string, an array or an object. [`Value::write`] writes the canonical form: no
//! whitespace, object keys sorted by their UTF-8 bytes, strings with `"`, `\` and every control
//! character escaped (`\n`, `\r`, `\t`, `\b`, `\f`, otherwise `\u00XX` with lower-case hex; DEL
//! as `\u007f`), every other character as itself. [`parse`] reads any JSON text of this value
//! set (whitespace, any key order and `\u` escapes are accepted) and refuses a fraction, an
//! exponent, an integer outside [-2^63, 2^64), a duplicate key, invalid UTF-8, a lone surrogate
//! and nesting deeper than 32. A line is canonical when writing what it parses to gives the
//! same bytes ([`is_canonical`]).
//!
//! The codec is written here, with no dependency, so that the guest agent (std and libc only)
//! and the host parse with the same code.

use std::collections::BTreeMap;
use std::fmt;

/// An object: keys in byte order, which is the canonical order.
pub type Obj = BTreeMap<String, Value>;

/// A JSON value of the protocol's value set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Every integer from -2^63 to 2^64 - 1.
    Int(i128),
    Str(String),
    Arr(Vec<Value>),
    Obj(Obj),
}

/// The deepest nesting [`parse`] accepts.
pub const MAX_DEPTH: usize = 32;
const INT_MIN: i128 = i64::MIN as i128;
const INT_MAX: i128 = u64::MAX as i128;

/// Why a text is not a JSON value of the protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset where reading stopped.
    pub at: usize,
    pub what: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.what, self.at)
    }
}

impl std::error::Error for ParseError {}

impl Value {
    /// The canonical text.
    pub fn to_canonical(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    /// Appends the canonical text to `out`.
    pub fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => out.push_str(&i.to_string()),
            Value::Str(s) => write_str(s, out),
            Value::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Obj(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// The canonical value laid out for people: two-space indentation, one member or item per
    /// line, keys sorted, and a final newline (the committed schema's form).
    pub fn to_pretty(&self) -> String {
        let mut s = String::new();
        self.pretty(0, &mut s);
        s.push('\n');
        s
    }

    fn pretty(&self, depth: usize, out: &mut String) {
        let pad = |d: usize, out: &mut String| out.extend(std::iter::repeat_n(' ', 2 * d));
        match self {
            Value::Arr(a) if !a.is_empty() => {
                out.push_str("[\n");
                for (i, v) in a.iter().enumerate() {
                    pad(depth + 1, out);
                    v.pretty(depth + 1, out);
                    out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
                }
                pad(depth, out);
                out.push(']');
            }
            Value::Obj(m) if !m.is_empty() => {
                out.push_str("{\n");
                for (i, (k, v)) in m.iter().enumerate() {
                    pad(depth + 1, out);
                    write_str(k, out);
                    out.push_str(": ");
                    v.pretty(depth + 1, out);
                    out.push_str(if i + 1 < m.len() { ",\n" } else { "\n" });
                }
                pad(depth, out);
                out.push('}');
            }
            v => v.write(out),
        }
    }

    /// The JSON kind, for messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Int(_) => "an integer",
            Value::Str(_) => "a string",
            Value::Arr(_) => "an array",
            Value::Obj(_) => "an object",
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Reads one JSON value that fills all of `text` (surrounding whitespace allowed).
pub fn parse(text: &[u8]) -> Result<Value, ParseError> {
    let mut p = Parser { b: text, i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(p.err("text after the value"));
    }
    Ok(v)
}

/// Whether `text` is exactly the canonical form of the value it holds.
pub fn is_canonical(text: &[u8]) -> bool {
    parse(text).is_ok_and(|v| v.to_canonical().as_bytes() == text)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, what: &'static str) -> ParseError {
        ParseError { at: self.i, what }
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn lit(&mut self, word: &[u8], v: Value) -> Result<Value, ParseError> {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("not a JSON value"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        match self.peek() {
            None => Err(self.err("end of text where a value was expected")),
            Some(b'n') => self.lit(b"null", Value::Null),
            Some(b't') => self.lit(b"true", Value::Bool(true)),
            Some(b'f') => self.lit(b"false", Value::Bool(false)),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(a));
                        }
                        _ => return Err(self.err("expected , or ] in an array")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut m = Obj::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(m));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(self.err("expected a string key"));
                    }
                    let at = self.i;
                    let k = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected : after a key"));
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    if m.insert(k, v).is_some() {
                        return Err(ParseError {
                            at,
                            what: "duplicate key",
                        });
                    }
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(m));
                        }
                        _ => return Err(self.err("expected , or } in an object")),
                    }
                }
            }
            Some(_) => Err(self.err("not a JSON value")),
        }
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let digits = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        if self.i == digits {
            return Err(self.err("a minus sign without digits"));
        }
        if self.b[digits] == b'0' && self.i - digits > 1 {
            return Err(ParseError {
                at: digits,
                what: "a number with a leading zero",
            });
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err(self.err("a fraction or exponent (the protocol has integers only)"));
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).expect("ASCII digits");
        match s.parse::<i128>() {
            Ok(n) if (INT_MIN..=INT_MAX).contains(&n) => Ok(Value::Int(n)),
            _ => Err(ParseError {
                at: start,
                what: "an integer outside [-2^63, 2^64)",
            }),
        }
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        if self.i + 4 > self.b.len() {
            return Err(self.err("a short \\u escape"));
        }
        let mut n = 0u32;
        for k in 0..4 {
            let d = match self.b[self.i + k] {
                c @ b'0'..=b'9' => c - b'0',
                c @ b'a'..=b'f' => c - b'a' + 10,
                c @ b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(self.err("a bad \\u escape")),
            };
            n = n * 16 + d as u32;
        }
        self.i += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // the opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("an unterminated string"));
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return String::from_utf8(out).map_err(|_| self.err("invalid UTF-8"));
                }
                b'\\' => {
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return Err(self.err("an unterminated string"));
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xd800..0xdc00).contains(&hi) {
                                if !self.b[self.i..].starts_with(b"\\u") {
                                    return Err(self.err("a lone surrogate"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&lo) {
                                    return Err(self.err("a lone surrogate"));
                                }
                                0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                            } else if (0xdc00..0xe000).contains(&hi) {
                                return Err(self.err("a lone surrogate"));
                            } else {
                                hi
                            };
                            char::from_u32(cp).ok_or_else(|| self.err("a bad \\u escape"))?
                        }
                        _ => {
                            self.i -= 1;
                            return Err(self.err("a bad escape"));
                        }
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0..=0x1f => return Err(self.err("a raw control character in a string")),
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_form() {
        let v = parse(
            br#" { "b" : [1, -2, true, null], "a" : "x\u0001\n\"\\\u00e9\ud83d\ude00\u007f" } "#,
        )
        .unwrap();
        assert_eq!(
            v.to_canonical(),
            "{\"a\":\"x\\u0001\\n\\\"\\\\\u{e9}\u{1f600}\\u007f\",\"b\":[1,-2,true,null]}"
        );
        assert!(is_canonical(v.to_canonical().as_bytes()));
        assert!(!is_canonical(b"{\"b\":1,\"a\":2}"));
        assert!(!is_canonical(b"{\"a\": 2}"));
    }

    #[test]
    fn integers_only() {
        assert_eq!(
            parse(b"18446744073709551615").unwrap(),
            Value::Int(u64::MAX as i128)
        );
        assert_eq!(
            parse(b"-9223372036854775808").unwrap(),
            Value::Int(i64::MIN as i128)
        );
        for bad in [
            &b"18446744073709551616"[..],
            b"-9223372036854775809",
            b"1.0",
            b"1e3",
            b"01",
            b"-",
            b"+1",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn refusals() {
        for bad in [
            &b"{\"a\":1,\"a\":2}"[..],
            b"\"\\ud800\"",
            b"\"\\udc00\"",
            b"\"\xff\"",
            b"\"a\nb\"",
            b"[1,]",
            b"{} x",
            b"",
            b"\"\\x\"",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert!(parse(deep.as_bytes()).is_err());
        let ok = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        assert!(parse(ok.as_bytes()).is_ok());
    }
}
