//! Reading JSON text into a serde_json Value with every number read exactly. serde_json's own
//! float parsing (without its `float_roundtrip` feature, which would change the whole
//! workspace's serde_json) can be one bit off; protojson reads exactly, and a retried request
//! must be the same request (`-102548.81881708467` stays that double).

use serde_json::{Map, Number, Value};

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at offset {}", self.i))
    }

    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 10_000 {
            return self.err("nesting too deep");
        }
        self.ws();
        match self.b.get(self.i) {
            None => self.err("unexpected end of JSON input"),
            Some(b'{') => {
                self.i += 1;
                let mut m = Map::new();
                if self.eat(b'}') {
                    return Ok(Value::Object(m));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("expected a key");
                    }
                    let k = self.string()?;
                    if !self.eat(b':') {
                        return self.err("expected ':'");
                    }
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(Value::Object(m));
                    }
                    return self.err("expected ',' or '}'");
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                if self.eat(b']') {
                    return Ok(Value::Array(a));
                }
                loop {
                    a.push(self.value(depth + 1)?);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(Value::Array(a));
                    }
                    return self.err("expected ',' or ']'");
                }
            }
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(_) => self.number(),
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            self.err("invalid literal")
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let st = self.i;
        let b = self.b;
        if b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let ds = self.i;
        while self.i < b.len() && b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.i == ds {
            return self.err("invalid number");
        }
        let mut int = true;
        if b.get(self.i) == Some(&b'.') {
            int = false;
            self.i += 1;
            let fs = self.i;
            while self.i < b.len() && b[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == fs {
                return self.err("invalid number");
            }
        }
        if matches!(b.get(self.i), Some(b'e' | b'E')) {
            int = false;
            self.i += 1;
            if matches!(b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let es = self.i;
            while self.i < b.len() && b[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == es {
                return self.err("invalid number");
            }
        }
        let t = std::str::from_utf8(&b[st..self.i]).expect("ascii");
        if int {
            if let Ok(v) = t.parse::<i64>() {
                return Ok(Value::Number(v.into()));
            }
            if let Ok(v) = t.parse::<u64>() {
                return Ok(Value::Number(v.into()));
            }
        }
        let f: f64 = t.parse().map_err(|_| format!("invalid number {t}"))?;
        Number::from_f64(f)
            .map(Value::Number)
            .ok_or_else(|| format!("number {t} out of range"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self
            .b
            .get(self.i..self.i + 4)
            .ok_or("unexpected end of JSON input")?;
        let s = std::str::from_utf8(h).map_err(|e| e.to_string())?;
        let v = u32::from_str_radix(s, 16)
            .map_err(|_| format!("invalid escape at offset {}", self.i))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // the quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.i) else {
                return self.err("unterminated string");
            };
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let Some(&e) = self.b.get(self.i) else {
                        return self.err("unterminated string");
                    };
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.b[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                u = 0x10000
                                    + ((u - 0xd800) << 10)
                                    + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            char::from_u32(u).unwrap_or('\u{fffd}')
                        }
                        _ => return self.err("invalid escape"),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return self.err("control character in string"),
                c => out.push(c),
            }
        }
    }
}

/// Parses one JSON value (nothing but blanks after it).
pub fn parse(s: &str) -> Result<Value, String> {
    let mut p = P {
        b: s.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return p.err("invalid character after the value");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    #[test]
    fn exact_numbers() {
        let v = super::parse("{\"a\":-102548.81881708467,\"b\":[1,-2,18446744073709551615,1e400],\"c\":\"\\u00e9\\ud83d\\ude00\"}");
        assert!(v.is_err(), "1e400 is out of range");
        let v = super::parse("{\"a\":-102548.81881708467,\"b\":[1,-2,18446744073709551615],\"c\":\"\\u00e9\\ud83d\\ude00\"}").unwrap();
        assert_eq!(
            v["a"].as_f64().unwrap().to_bits(),
            (-102548.81881708467f64).to_bits()
        );
        assert_eq!(v["b"][2].as_u64(), Some(u64::MAX));
        assert_eq!(v["c"].as_str(), Some("é😀"));
        assert!(super::parse("[1,]").is_err());
        assert!(super::parse("{} x").is_err());
    }
}
