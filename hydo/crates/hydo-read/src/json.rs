//! JSON as Go's encoding/json writes and reads it, for the values hydo-read handles.
//!
//! Strings are bytes (a Go string may hold any bytes: a binary column, a Redis value); the
//! encoder replaces each invalid UTF-8 byte with `\ufffd`, escapes U+2028 and U+2029, and does
//! not escape HTML characters (`SetEscapeHTML(false)`); object keys are sorted bytewise. The
//! decoder is `json.Decoder` with `UseNumber`: numbers keep their literal text.

use crate::gofmt::{decode_rune, quote};
use std::collections::BTreeMap;

pub type Obj = BTreeMap<Vec<u8>, Val>;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Int(i64),
    /// A json.Number: the literal text of a decoded number.
    Num(String),
    Str(Vec<u8>),
    Arr(Vec<Val>),
    Obj(Obj),
}

impl Val {
    pub fn str(s: impl Into<Vec<u8>>) -> Val {
        Val::Str(s.into())
    }
    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Val::Obj(o) => Some(o),
            _ => None,
        }
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Go's appendString with escapeHTML false.
pub fn enc_str(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b < 0x80 {
            if (0x20..0x80).contains(&b) && b != b'"' && b != b'\\' {
                out.push(b);
            } else {
                out.push(b'\\');
                match b {
                    b'"' | b'\\' => out.push(b),
                    0x08 => out.push(b'b'),
                    0x0c => out.push(b'f'),
                    b'\n' => out.push(b'n'),
                    b'\r' => out.push(b'r'),
                    b'\t' => out.push(b't'),
                    _ => out.extend_from_slice(&[
                        b'u',
                        b'0',
                        b'0',
                        HEX[(b >> 4) as usize],
                        HEX[(b & 15) as usize],
                    ]),
                }
            }
            i += 1;
            continue;
        }
        match decode_rune(&s[i..]) {
            (None, _) => {
                out.extend_from_slice(b"\\ufffd");
                i += 1;
            }
            (Some(c), w) => {
                if c == '\u{2028}' || c == '\u{2029}' {
                    out.extend_from_slice(if c == '\u{2028}' {
                        b"\\u2028"
                    } else {
                        b"\\u2029"
                    });
                } else {
                    out.extend_from_slice(&s[i..i + w]);
                }
                i += w;
            }
        }
    }
    out.push(b'"');
}

pub fn encode_into(out: &mut Vec<u8>, v: &Val) {
    match v {
        Val::Null => out.extend_from_slice(b"null"),
        Val::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Val::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Val::Num(s) => out.extend_from_slice(if s.is_empty() { b"0" } else { s.as_bytes() }),
        Val::Str(s) => enc_str(out, s),
        Val::Arr(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                encode_into(out, x);
            }
            out.push(b']');
        }
        Val::Obj(o) => {
            out.push(b'{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                enc_str(out, k);
                out.push(b':');
                encode_into(out, x);
            }
            out.push(b'}');
        }
    }
}

/// `encode(v)` of the Go program: one line, no trailing newline.
pub fn encode(v: &Val) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(&mut out, v);
    out
}

pub fn encode_obj(o: &Obj) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(&mut out, &Val::Obj(o.clone()));
    out
}

// ------------------------------------------------------------------ decoding

/// A decoding error, with Go's message text.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeError(pub String);

/// Go's `quoteChar` for a scanner error.
fn quote_char(c: u8) -> String {
    if c == b'\'' {
        return "'\\''".into();
    }
    if c == b'"' {
        return "'\"'".into();
    }
    // strconv.Quote(string(rune(c)))
    let ch = char::from_u32(c as u32).unwrap_or('\u{fffd}');
    let mut buf = [0u8; 4];
    let q = quote(ch.encode_utf8(&mut buf).as_bytes());
    format!("'{}'", &q[1..q.len() - 1])
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

type R<T> = Result<T, DecodeError>;

impl<'a> P<'a> {
    fn eof(&self) -> DecodeError {
        DecodeError("unexpected EOF".into())
    }
    fn bad(&self, c: u8, ctx: &str) -> DecodeError {
        DecodeError(format!("invalid character {} {ctx}", quote_char(c)))
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn value(&mut self, ctx: &str) -> R<Val> {
        self.ws();
        let Some(c) = self.peek() else {
            return Err(self.eof());
        };
        match c {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(Val::Str(self.string()?)),
            b'-' | b'0'..=b'9' => Ok(Val::Num(self.number()?)),
            b't' => self.lit(b"true", Val::Bool(true)),
            b'f' => self.lit(b"false", Val::Bool(false)),
            b'n' => self.lit(b"null", Val::Null),
            _ => Err(self.bad(c, ctx)),
        }
    }
    fn lit(&mut self, w: &[u8], v: Val) -> R<Val> {
        let name = std::str::from_utf8(w).unwrap_or("");
        for (k, &want) in w.iter().enumerate() {
            match self.s.get(self.i + k) {
                None => return Err(self.eof()),
                Some(&c) if c != want => {
                    return Err(self.bad(
                        c,
                        &format!("in literal {name} (expecting {})", quote_char(want)),
                    ))
                }
                _ => {}
            }
        }
        self.i += w.len();
        Ok(v)
    }
    fn number(&mut self) -> R<String> {
        let st = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            None => return Err(self.eof()),
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            Some(c) => return Err(self.bad(c, "in numeric literal")),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            match self.peek() {
                None => return Err(self.eof()),
                Some(b'0'..=b'9') => {}
                Some(c) => return Err(self.bad(c, "after decimal point in numeric literal")),
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            match self.peek() {
                None => return Err(self.eof()),
                Some(b'0'..=b'9') => {}
                Some(c) => return Err(self.bad(c, "in exponent of numeric literal")),
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        Ok(String::from_utf8_lossy(&self.s[st..self.i]).into_owned())
    }
    fn hex4(&mut self) -> R<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            let Some(c) = self.peek() else {
                return Err(self.eof());
            };
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(self.bad(c, "in \\u hexadecimal character escape")),
            };
            v = v * 16 + d as u32;
            self.i += 1;
        }
        Ok(v)
    }
    fn string(&mut self) -> R<Vec<u8>> {
        self.i += 1; // the opening quote
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.eof());
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return Err(self.eof());
                    };
                    self.i += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut r = self.hex4()?;
                            if (0xd800..0xe000).contains(&r) {
                                // a surrogate pair, else U+FFFD
                                let save = self.i;
                                let mut paired = false;
                                if self.s.get(self.i) == Some(&b'\\')
                                    && self.s.get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    if let Ok(r2) = self.hex4() {
                                        if r < 0xdc00 && (0xdc00..0xe000).contains(&r2) {
                                            r = 0x10000 + ((r - 0xd800) << 10) + (r2 - 0xdc00);
                                            paired = true;
                                        }
                                    }
                                }
                                if !paired {
                                    self.i = save;
                                    r = 0xfffd;
                                }
                            }
                            let ch = char::from_u32(r).unwrap_or('\u{fffd}');
                            let mut b = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                        }
                        _ => return Err(self.bad(e, "in string escape code")),
                    }
                }
                c if c < 0x20 => return Err(self.bad(c, "in string literal")),
                c if c < 0x80 => {
                    out.push(c);
                    self.i += 1;
                }
                _ => match decode_rune(&self.s[self.i..]) {
                    (Some(_), w) => {
                        out.extend_from_slice(&self.s[self.i..self.i + w]);
                        self.i += w;
                    }
                    (None, _) => {
                        out.extend_from_slice("\u{fffd}".as_bytes());
                        self.i += 1;
                    }
                },
            }
        }
    }
    fn object(&mut self) -> R<Val> {
        self.i += 1;
        let mut o = Obj::new();
        self.ws();
        match self.peek() {
            None => return Err(self.eof()),
            Some(b'}') => {
                self.i += 1;
                return Ok(Val::Obj(o));
            }
            _ => {}
        }
        loop {
            self.ws();
            match self.peek() {
                None => return Err(self.eof()),
                Some(b'"') => {}
                Some(c) => return Err(self.bad(c, "looking for beginning of object key string")),
            }
            let k = self.string()?;
            self.ws();
            match self.peek() {
                None => return Err(self.eof()),
                Some(b':') => self.i += 1,
                Some(c) => return Err(self.bad(c, "after object key")),
            }
            let v = self.value("looking for beginning of value")?;
            o.insert(k, v);
            self.ws();
            match self.peek() {
                None => return Err(self.eof()),
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Val::Obj(o));
                }
                Some(c) => return Err(self.bad(c, "after object key:value pair")),
            }
        }
    }
    fn array(&mut self) -> R<Val> {
        self.i += 1;
        let mut a = Vec::new();
        self.ws();
        match self.peek() {
            None => return Err(self.eof()),
            Some(b']') => {
                self.i += 1;
                return Ok(Val::Arr(a));
            }
            _ => {}
        }
        loop {
            a.push(self.value("looking for beginning of value")?);
            self.ws();
            match self.peek() {
                None => return Err(self.eof()),
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Val::Arr(a));
                }
                Some(c) => return Err(self.bad(c, "after array element")),
            }
        }
    }
}

fn kind(v: &Val) -> &'static str {
    match v {
        Val::Null => "null",
        Val::Bool(_) => "bool",
        Val::Int(_) | Val::Num(_) => "number",
        Val::Str(_) => "string",
        Val::Arr(_) => "array",
        Val::Obj(_) => "object",
    }
}

/// `json.NewDecoder(strings.NewReader(line)).Decode(&rec)` with UseNumber, rec a
/// `map[string]any`: the first value of the line (what follows it is not read). `Ok(None)` is
/// a JSON null (a nil map).
pub fn decode_record_line(line: &[u8]) -> Result<Option<Obj>, DecodeError> {
    let mut p = P { s: line, i: 0 };
    let v = p.value("looking for beginning of value")?;
    match v {
        Val::Obj(o) => Ok(Some(o)),
        Val::Null => Ok(None),
        other => Err(DecodeError(format!(
            "json: cannot unmarshal {} into Go value of type main.Record",
            kind(&other)
        ))),
    }
}

/// `json.Unmarshal(b, &v)` into an `any`: the whole input must be one value.
pub fn unmarshal(b: &[u8]) -> Result<Val, DecodeError> {
    let mut p = P { s: b, i: 0 };
    p.ws();
    if p.i >= b.len() {
        return Err(DecodeError("unexpected end of JSON input".into()));
    }
    let v = p.value("looking for beginning of value").map_err(|e| {
        if e.0 == "unexpected EOF" {
            DecodeError("unexpected end of JSON input".into())
        } else {
            e
        }
    })?;
    p.ws();
    if let Some(c) = p.peek() {
        return Err(p.bad(c, "after top-level value"));
    }
    Ok(v)
}

/// Go's struct-field matching for Unmarshal: the key equal to `name`, else one equal to it
/// ignoring ASCII case.
pub fn field_fold<'a>(o: &'a Obj, name: &str) -> Option<&'a Val> {
    if let Some(v) = o.get(name.as_bytes()) {
        return Some(v);
    }
    o.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name.as_bytes()))
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &Val) -> String {
        String::from_utf8(encode(v)).unwrap()
    }

    #[test]
    fn strings_as_go_writes_them() {
        assert_eq!(s(&Val::str(&b"a\"\\<>&/\x7f"[..])), r#""a\"\\<>&/\x7f""#.replace("\\x7f", "\x7f"));
        assert_eq!(s(&Val::str(&b"\x00\x08\x0c\n\r\t\x1f"[..])), r#""\u0000\b\f\n\r\t\u001f""#);
        assert_eq!(s(&Val::str(&b"\xff\xe2\x82"[..])), r#""\ufffd\ufffd\ufffd""#);
        assert_eq!(s(&Val::str("é\u{2028}\u{fffd}")), "\"é\\u2028\u{fffd}\"");
    }

    #[test]
    fn decode_like_go() {
        let o = decode_record_line(br#"{"b":1,"a":[1.50,"x"],"a":true} trailing"#).unwrap().unwrap();
        assert_eq!(String::from_utf8(encode_obj(&o)).unwrap(), r#"{"a":true,"b":1}"#);
        assert_eq!(decode_record_line(b"null").unwrap(), None);
        assert_eq!(
            decode_record_line(b"[1]").unwrap_err().0,
            "json: cannot unmarshal array into Go value of type main.Record"
        );
        assert_eq!(
            decode_record_line(b"x").unwrap_err().0,
            "invalid character 'x' looking for beginning of value"
        );
        assert_eq!(decode_record_line(br#"{"a":1"#).unwrap_err().0, "unexpected EOF");
        let o = decode_record_line(br#"{"k":"\ud83d\ude00\ud800x\u00e9"}"#).unwrap().unwrap();
        assert_eq!(o[&b"k"[..].to_vec()], Val::str("\u{1f600}\u{fffd}x\u{e9}"));
    }
}
