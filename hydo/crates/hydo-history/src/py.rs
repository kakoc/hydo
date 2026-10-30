//! The Python semantics the checkers' reference implementation (hydo/history/hydohist.py) relies
//! on, so that every verdict, witness and message is the same text: `str.split()`,
//! `str.splitlines()` and `str.strip()` with Python's whitespace, `int()` of a string (signs,
//! underscores, every Unicode decimal digit, arbitrary size), `"%.2f"`, `repr()` and `str()`,
//! `json.loads()` (NaN, Infinity, big integers, duplicate keys) into [`PyVal`], `json.dumps()` with
//! `sort_keys=True` and compact separators, truthiness, and the equality a dict uses for its keys.

use crate::unitab::{DECIMAL_ZEROS, NOT_PRINTABLE};
use std::cmp::Ordering;
use std::fmt::Write as _;

/// A Python exception the reference implementation does not catch: the whole check stops there
/// (the bridge and `hydo-check` exit with status 1). The text names the exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crash(pub String);

impl std::fmt::Display for Crash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Crash {}

// ------------------------------------------------------------------ characters and strings
/// Python's `str.isspace()` for one character.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '\n'
            | '\x0b'
            | '\x0c'
            | '\r'
            | '\x1c'..='\x1f'
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Python's `str.split()` without arguments.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|x| !x.is_empty()).collect()
}

/// Python's `str.strip()` without arguments.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// Python's `str.splitlines()` (no line ends kept).
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !brk {
            continue;
        }
        out.push(&s[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = it.peek() {
                it.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The value of a Unicode decimal digit (`unicodedata.decimal`), as Python's `int()` and `\d`
/// accept them.
pub fn decimal_value(c: char) -> Option<u32> {
    let u = c as u32;
    if c.is_ascii_digit() {
        return Some(u - 0x30);
    }
    if u < 0x80 {
        return None;
    }
    let i = DECIMAL_ZEROS.partition_point(|&z| z <= u);
    if i == 0 {
        return None;
    }
    let z = DECIMAL_ZEROS[i - 1];
    (u - z < 10).then_some(u - z)
}

/// Python's `str.isprintable()` for one character.
pub fn is_printable(c: char) -> bool {
    let u = c as u32;
    let i = NOT_PRINTABLE.partition_point(|&(a, _)| a <= u);
    !(i > 0 && u <= NOT_PRINTABLE[i - 1].1)
}

/// Python's `s[:n]` (characters).
pub fn head(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `repr()` of a str.
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x7f && (c as u32) >= 0x20 => out.push(c),
            c if (c as u32) >= 0x80 && is_printable(c) => out.push(c),
            c => {
                let u = c as u32;
                if u < 0x100 {
                    let _ = write!(out, "\\x{u:02x}");
                } else if u < 0x10000 {
                    let _ = write!(out, "\\u{u:04x}");
                } else {
                    let _ = write!(out, "\\U{u:08x}");
                }
            }
        }
    }
    out.push(q);
    out
}

/// `repr()` of a list of str.
pub fn repr_strs<S: AsRef<str>>(xs: &[S]) -> String {
    let v: Vec<String> = xs.iter().map(|x| repr_str(x.as_ref())).collect();
    format!("[{}]", v.join(", "))
}

// ------------------------------------------------------------------ integers
/// A Python int of any size: sign and decimal digits without leading zeros ("0" for zero).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BigInt {
    pub neg: bool,
    pub digits: String,
}

impl BigInt {
    pub fn from_digits(neg: bool, digits: &str) -> BigInt {
        let d = digits.trim_start_matches('0');
        if d.is_empty() {
            BigInt {
                neg: false,
                digits: "0".into(),
            }
        } else {
            BigInt {
                neg,
                digits: d.to_string(),
            }
        }
    }

    pub fn from_i64(v: i64) -> BigInt {
        BigInt::from_digits(v < 0, &v.unsigned_abs().to_string())
    }

    pub fn is_zero(&self) -> bool {
        self.digits == "0"
    }

    /// `"%d" % x`.
    pub fn text(&self) -> String {
        if self.neg {
            format!("-{}", self.digits)
        } else {
            self.digits.clone()
        }
    }

    /// The value as an i64, saturated.
    pub fn saturating_i64(&self) -> i64 {
        match self.digits.parse::<i64>() {
            Ok(v) if self.neg => -v,
            Ok(v) => v,
            Err(_) if self.neg => i64::MIN,
            Err(_) => i64::MAX,
        }
    }

    /// `float(x)`, as Python converts an int (correctly rounded); `None` = OverflowError.
    pub fn to_f64(&self) -> Option<f64> {
        let f: f64 = self.digits.parse().ok()?;
        if f.is_infinite() {
            return None;
        }
        Some(if self.neg { -f } else { f })
    }
}

impl Ord for BigInt {
    fn cmp(&self, o: &BigInt) -> Ordering {
        let mag = |a: &BigInt, b: &BigInt| {
            a.digits
                .len()
                .cmp(&b.digits.len())
                .then_with(|| a.digits.cmp(&b.digits))
        };
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag(self, o),
            (true, true) => mag(o, self),
        }
    }
}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, o: &BigInt) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Python's limit on the digits of an int read from a string (3.11+, `sys.int_info`).
const MAX_STR_DIGITS: usize = 4300;

/// The ASCII digits of a run of Unicode decimal digits that may hold single underscores between
/// digits (Python's literal rules), or None.
fn digits_with_underscores(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut prev_us = true; // no leading underscore
    for c in s.chars() {
        if c == '_' {
            if prev_us {
                return None;
            }
            prev_us = true;
            continue;
        }
        out.push(char::from_digit(decimal_value(c)?, 10)?);
        prev_us = false;
    }
    if prev_us {
        return None; // empty, or a trailing underscore
    }
    Some(out)
}

/// Python's `int(s)` of a str (base 10): `None` = ValueError.
pub fn parse_int(s: &str) -> Option<BigInt> {
    let t = strip(s);
    let (neg, rest) = match t.chars().next() {
        Some('-') => (true, &t[1..]),
        Some('+') => (false, &t[1..]),
        _ => (false, t),
    };
    let d = digits_with_underscores(rest)?;
    if d.len() > MAX_STR_DIGITS {
        return None;
    }
    Some(BigInt::from_digits(neg, &d))
}

/// The match of `^(.*)@(\d+)$` on a token without line breaks: (value part, version); Err = the
/// ValueError `int()` raises for more than 4300 digits.
pub fn split_version(tok: &str) -> Result<Option<(&str, BigInt)>, Crash> {
    let Some(at) = tok.rfind('@') else {
        return Ok(None);
    };
    let ds = &tok[at + 1..];
    if ds.is_empty() {
        return Ok(None);
    }
    let mut d = String::with_capacity(ds.len());
    for c in ds.chars() {
        match decimal_value(c).and_then(|v| char::from_digit(v, 10)) {
            Some(x) => d.push(x),
            None => return Ok(None),
        }
    }
    let b = BigInt::from_digits(false, &d);
    if d.len() > MAX_STR_DIGITS {
        return Err(Crash(
            "ValueError: Exceeds the limit (4300 digits) for integer string conversion".into(),
        ));
    }
    Ok(Some((&tok[..at], b)))
}

// ------------------------------------------------------------------ floats
/// Python's `"%.2f" % x`.
pub fn fmt_2f(x: f64) -> String {
    format!("{x:.2}")
}

/// `repr()` of a float.
pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let e = format!("{f:e}");
    let (mant, ex) = e.split_once('e').unwrap_or((&e, "0"));
    let ex: i32 = ex.parse().unwrap_or(0);
    if !(-4..16).contains(&ex) {
        let sign = if ex < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", ex.abs());
    }
    let s = format!("{f}");
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

// ------------------------------------------------------------------ values read from JSON
/// A value `json.loads()` returns, with Python's types.
#[derive(Debug, Clone, PartialEq)]
pub enum PyVal {
    None,
    Bool(bool),
    Int(BigInt),
    Float(f64),
    Str(String),
    List(Vec<PyVal>),
    /// Insertion order, keys unique (a later duplicate replaces the value in place).
    Dict(Vec<(String, PyVal)>),
}

/// How a hashable value compares as a dict key (`1 == 1.0 == True`; a NaN equals nothing).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyRepr {
    None,
    Num(String),
    Str(String),
    NaN(u64),
}

static NAN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl PyVal {
    pub fn str(s: &str) -> PyVal {
        PyVal::Str(s.to_string())
    }

    /// `d.get(k)` of a dict (None for anything else or a missing key).
    pub fn get(&self, k: &str) -> Option<&PyVal> {
        match self {
            PyVal::Dict(m) => m.iter().find(|(x, _)| x == k).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PyVal::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Set a key of a dict (in place if present, else at the end).
    pub fn set(&mut self, k: &str, v: PyVal) {
        if let PyVal::Dict(m) = self {
            if let Some(e) = m.iter_mut().find(|(x, _)| x == k) {
                e.1 = v;
            } else {
                m.push((k.to_string(), v));
            }
        }
    }

    pub fn is_dict(&self) -> bool {
        matches!(self, PyVal::Dict(_))
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            PyVal::None => false,
            PyVal::Bool(b) => *b,
            PyVal::Int(i) => !i.is_zero(),
            PyVal::Float(f) => *f != 0.0,
            PyVal::Str(s) => !s.is_empty(),
            PyVal::List(a) => !a.is_empty(),
            PyVal::Dict(m) => !m.is_empty(),
        }
    }

    /// `x == "<s>"`.
    pub fn eq_str(&self, s: &str) -> bool {
        matches!(self, PyVal::Str(x) if x == s)
    }

    /// The key a dict files this value under; Err = TypeError (unhashable).
    pub fn key(&self) -> Result<KeyRepr, Crash> {
        Ok(match self {
            PyVal::None => KeyRepr::None,
            PyVal::Bool(b) => KeyRepr::Num(if *b { "1" } else { "0" }.into()),
            PyVal::Int(i) => KeyRepr::Num(i.text()),
            PyVal::Float(f) if f.is_nan() => {
                KeyRepr::NaN(NAN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
            }
            PyVal::Float(f) if f.is_infinite() => KeyRepr::Num(repr_float(*f)),
            PyVal::Float(f) if f.fract() == 0.0 => {
                KeyRepr::Num(BigInt::from_digits(*f < 0.0, &format!("{:.0}", f.abs())).text())
            }
            PyVal::Float(f) => KeyRepr::Num(format!("f{f:e}")),
            PyVal::Str(s) => KeyRepr::Str(s.clone()),
            PyVal::List(_) => return Err(Crash("TypeError: unhashable type: 'list'".into())),
            PyVal::Dict(_) => return Err(Crash("TypeError: unhashable type: 'dict'".into())),
        })
    }

    /// `repr(x)`.
    pub fn repr(&self) -> String {
        match self {
            PyVal::None => "None".into(),
            PyVal::Bool(b) => if *b { "True" } else { "False" }.into(),
            PyVal::Int(i) => i.text(),
            PyVal::Float(f) => repr_float(*f),
            PyVal::Str(s) => repr_str(s),
            PyVal::List(a) => format!(
                "[{}]",
                a.iter().map(PyVal::repr).collect::<Vec<_>>().join(", ")
            ),
            PyVal::Dict(m) => format!(
                "{{{}}}",
                m.iter()
                    .map(|(k, v)| format!("{}: {}", repr_str(k), v.repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// `str(x)` / `"%s" % x`.
    pub fn str_of(&self) -> String {
        match self {
            PyVal::Str(s) => s.clone(),
            other => other.repr(),
        }
    }

    /// `int(x)` of a value: Ok(None) = ValueError or TypeError (both caught by the reference
    /// where it calls int() on JSON), Err = an exception it does not catch (OverflowError).
    pub fn int_of(&self) -> Result<Option<BigInt>, Crash> {
        Ok(match self {
            PyVal::Bool(b) => Some(BigInt::from_i64(*b as i64)),
            PyVal::Int(i) => Some(i.clone()),
            PyVal::Float(f) if f.is_nan() => None,
            PyVal::Float(f) if f.is_infinite() => {
                return Err(Crash(
                    "OverflowError: cannot convert float infinity to integer".into(),
                ))
            }
            PyVal::Float(f) => Some(BigInt::from_digits(
                *f < 0.0,
                &format!("{:.0}", f.abs().trunc()),
            )),
            PyVal::Str(s) => parse_int(s),
            _ => None,
        })
    }

    /// `"%d" % x`; Err = the TypeError/ValueError/OverflowError Python raises.
    pub fn fmt_d(&self) -> Result<String, Crash> {
        match self {
            PyVal::Bool(_) | PyVal::Int(_) => {
                Ok(self.int_of()?.map(|b| b.text()).unwrap_or_default())
            }
            PyVal::Float(f) if f.is_nan() => Err(Crash(
                "ValueError: cannot convert float NaN to integer".into(),
            )),
            PyVal::Float(_) => Ok(self.int_of()?.map(|b| b.text()).unwrap_or_default()),
            other => Err(Crash(format!(
                "TypeError: %d format: a real number is required, not {}",
                other.type_name()
            ))),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            PyVal::None => "NoneType",
            PyVal::Bool(_) => "bool",
            PyVal::Int(_) => "int",
            PyVal::Float(_) => "float",
            PyVal::Str(_) => "str",
            PyVal::List(_) => "list",
            PyVal::Dict(_) => "dict",
        }
    }
}

/// `json.dumps(x, sort_keys=True, separators=(",", ":"))` (ASCII only, as Python's default).
pub fn dumps_sorted(v: &PyVal) -> String {
    let mut out = String::new();
    dump(v, &mut out);
    out
}

fn dump(v: &PyVal, out: &mut String) {
    match v {
        PyVal::None => out.push_str("null"),
        PyVal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PyVal::Int(i) => out.push_str(&i.text()),
        PyVal::Float(f) => {
            if f.is_nan() {
                out.push_str("NaN")
            } else if f.is_infinite() {
                out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" })
            } else {
                out.push_str(&repr_float(*f))
            }
        }
        PyVal::Str(s) => dump_str(s, out),
        PyVal::List(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump(x, out);
            }
            out.push(']');
        }
        PyVal::Dict(m) => {
            let mut ks: Vec<&(String, PyVal)> = m.iter().collect();
            ks.sort_by(|a, b| a.0.cmp(&b.0));
            out.push('{');
            for (i, (k, x)) in ks.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump_str(k, out);
                out.push(':');
                dump(x, out);
            }
            out.push('}');
        }
    }
}

fn dump_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.loads()` refused the text (Python's JSONDecodeError or ValueError).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonError;

/// `json.loads(s)` as CPython's C scanner reads it (Err = ValueError, JSONDecodeError). A lone
/// surrogate escape (`"\ud800"`), which Python keeps in its str, becomes U+FFFD here.
pub fn json_loads(s: &str) -> Result<PyVal, JsonError> {
    let b = s.as_bytes();
    let mut p = Parser { s, b, i: 0 };
    p.ws();
    let v = p.value(0).map_err(|()| JsonError)?;
    p.ws();
    if p.i != b.len() {
        return Err(JsonError);
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn lit(&mut self, w: &str) -> bool {
        if self.s[self.i..].starts_with(w) {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<PyVal, ()> {
        if depth > 900 {
            return Err(()); // RecursionError in Python; reported as a bad line here
        }
        let c = *self.b.get(self.i).ok_or(())?;
        match c {
            b'"' => {
                self.i += 1;
                Ok(PyVal::Str(self.string()?))
            }
            b'{' => {
                self.i += 1;
                let mut m: Vec<(String, PyVal)> = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(PyVal::Dict(m));
                }
                loop {
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(());
                    }
                    self.i += 1;
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(());
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    if let Some(e) = m.iter_mut().find(|(x, _)| *x == k) {
                        e.1 = v;
                    } else {
                        m.push((k, v));
                    }
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(PyVal::Dict(m));
                        }
                        Some(b',') => {
                            self.i += 1;
                            self.ws();
                        }
                        _ => return Err(()),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(PyVal::List(a));
                }
                loop {
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b']') => {
                            self.i += 1;
                            return Ok(PyVal::List(a));
                        }
                        Some(b',') => {
                            self.i += 1;
                            self.ws();
                        }
                        _ => return Err(()),
                    }
                }
            }
            b'n' if self.lit("null") => Ok(PyVal::None),
            b't' if self.lit("true") => Ok(PyVal::Bool(true)),
            b'f' if self.lit("false") => Ok(PyVal::Bool(false)),
            b'N' if self.lit("NaN") => Ok(PyVal::Float(f64::NAN)),
            b'I' if self.lit("Infinity") => Ok(PyVal::Float(f64::INFINITY)),
            b'-' if self.lit("-Infinity") => Ok(PyVal::Float(f64::NEG_INFINITY)),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(()),
        }
    }

    fn number(&mut self) -> Result<PyVal, ()> {
        let b = self.b;
        let start = self.i;
        let mut i = self.i;
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(b'1'..=b'9') => {
                while matches!(b.get(i), Some(b'0'..=b'9')) {
                    i += 1;
                }
            }
            _ => return Err(()),
        }
        let mut float = false;
        if b.get(i) == Some(&b'.') && matches!(b.get(i + 1), Some(b'0'..=b'9')) {
            float = true;
            i += 1;
            while matches!(b.get(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            let mut j = i + 1;
            if matches!(b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if matches!(b.get(j), Some(b'0'..=b'9')) {
                while matches!(b.get(j), Some(b'0'..=b'9')) {
                    j += 1;
                }
                float = true;
                i = j;
            }
        }
        self.i = i;
        let t = &self.s[start..i];
        if float {
            t.parse::<f64>().map(PyVal::Float).map_err(|_| ())
        } else {
            let neg = t.starts_with('-');
            let d = t.trim_start_matches('-');
            if d.len() > MAX_STR_DIGITS {
                return Err(());
            }
            Ok(PyVal::Int(BigInt::from_digits(neg, d)))
        }
    }

    fn hex4(&mut self) -> Result<u32, ()> {
        let h = self.s.get(self.i..self.i + 4).ok_or(())?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(());
        }
        self.i += 4;
        u32::from_str_radix(h, 16).map_err(|_| ())
    }

    fn string(&mut self) -> Result<String, ()> {
        let mut out = String::new();
        loop {
            let c = self.s[self.i..].chars().next().ok_or(())?;
            self.i += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = *self.b.get(self.i).ok_or(())?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.s[self.i..].starts_with("\\u")
                            {
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xdc00..0xe000).contains(&lo) {
                                    let cp = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00);
                                    out.push(char::from_u32(cp).ok_or(())?);
                                    continue;
                                }
                                self.i = save;
                            }
                            out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(()),
                    }
                }
                c if (c as u32) < 0x20 => return Err(()),
                c => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings() {
        assert_eq!(splitlines("a\r\nb\rc\x1cd\n"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n\nx"), vec!["", "", "x"]);
        assert_eq!(split_ws(" a\x1fb  c "), vec!["a", "b", "c"]);
        assert_eq!(repr_str("a'b"), "\"a'b\"");
        assert_eq!(repr_str("\u{378}é\x7f"), "'\\u0378é\\x7f'");
        assert_eq!(head("héllo", 2), "hé");
    }

    #[test]
    fn ints() {
        assert_eq!(parse_int(" -0_1 ").unwrap().text(), "-1");
        assert_eq!(parse_int("-0").unwrap().text(), "0");
        assert!(parse_int("1__0").is_none());
        assert!(parse_int("_1").is_none());
        assert_eq!(parse_int("\u{661}\u{662}").unwrap().text(), "12");
        assert_eq!(split_version("a@b@0012").unwrap().unwrap().1.text(), "12");
        assert!(split_version("a@").unwrap().is_none());
    }

    #[test]
    fn floats() {
        assert_eq!(fmt_2f(0.125), "0.12");
        assert_eq!(fmt_2f(0.375), "0.38");
        assert_eq!(fmt_2f(-1e-9), "-0.00");
        assert_eq!(fmt_2f(2.675), "2.67");
        assert_eq!(repr_float(1e16), "1e+16");
        assert_eq!(repr_float(123.0), "123.0");
        assert_eq!(repr_float(1.5e-5), "1.5e-05");
    }

    #[test]
    fn json() {
        let v = json_loads(" {\"b\":1,\"a\":[NaN,-Infinity,1e400,true],\"b\":2} ").unwrap();
        assert_eq!(
            dumps_sorted(&v),
            "{\"a\":[NaN,-Infinity,Infinity,true],\"b\":2}"
        );
        assert!(json_loads("[1,]").is_err());
        assert!(json_loads("01").is_err());
        assert_eq!(json_loads("\"\\ud83d\\ude00\"").unwrap(), PyVal::str("😀"));
        assert_eq!(
            json_loads("123456789012345678901234567890").unwrap().repr(),
            "123456789012345678901234567890"
        );
        assert_eq!(json_loads("{'a': 1}").map(|_| ()), Err(JsonError));
    }
}
