//! The details value model and a JSON encoder that writes exactly what Go's `encoding/json`
//! writes for the same data (the Go SDK uses `json.Marshal`): object keys sorted bytewise,
//! `<`, `>`, `&`, U+2028 and U+2029 escaped as `\u....`, Go's float formatting, and a record
//! that contains a NaN or an infinity is not written at all (Go's `Marshal` fails on them).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

/// One details value. Build it with `From`/`Into` from Rust values, or with the [`details!`]
/// macro (`crate::details!`).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    /// Encoded as Go encodes a `float64`.
    Float(f64),
    /// Encoded as Go encodes a `float32` (shortest digits of the 32-bit value).
    Float32(f32),
    Str(String),
    List(Vec<Value>),
    Map(Details),
}

/// The free-form `details` object of an evaluation: a map with keys kept in bytewise order,
/// which is the order Go's `encoding/json` writes map keys in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details(BTreeMap<String, Value>);

impl Details {
    /// An empty details object (an evaluation with empty details carries no `details` field).
    pub fn new() -> Self {
        Details(BTreeMap::new())
    }
    /// Sets `key` to `value`, replacing an earlier value.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.0.insert(key.into(), value.into());
        self
    }
    /// Builder form of [`Details::insert`].
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.insert(key, value);
        self
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }
    /// The JSON text of this object, as Go would write it; `None` if it holds a NaN or an
    /// infinity.
    pub fn to_json(&self) -> Option<String> {
        let mut s = String::new();
        write_map(&mut s, self).ok()?;
        Some(s)
    }
}

impl Value {
    /// The JSON text of this value, as Go would write it; `None` if it holds a NaN or an
    /// infinity.
    pub fn to_json(&self) -> Option<String> {
        let mut s = String::new();
        write_value(&mut s, self).ok()?;
        Some(s)
    }
}

macro_rules! from_int {
    ($($t:ty => $v:ident as $c:ty),*) => {$(
        impl From<$t> for Value { fn from(x: $t) -> Self { Value::$v(x as $c) } }
    )*};
}
from_int!(i8 => Int as i64, i16 => Int as i64, i32 => Int as i64, i64 => Int as i64, isize => Int as i64,
          u8 => Uint as u64, u16 => Uint as u64, u32 => Uint as u64, u64 => Uint as u64, usize => Uint as u64);

impl From<bool> for Value {
    fn from(x: bool) -> Self {
        Value::Bool(x)
    }
}
impl From<f64> for Value {
    fn from(x: f64) -> Self {
        Value::Float(x)
    }
}
impl From<f32> for Value {
    fn from(x: f32) -> Self {
        Value::Float32(x)
    }
}
impl From<&str> for Value {
    fn from(x: &str) -> Self {
        Value::Str(x.to_string())
    }
}
impl From<String> for Value {
    fn from(x: String) -> Self {
        Value::Str(x)
    }
}
impl From<&String> for Value {
    fn from(x: &String) -> Self {
        Value::Str(x.clone())
    }
}
impl<'a> From<Cow<'a, str>> for Value {
    fn from(x: Cow<'a, str>) -> Self {
        Value::Str(x.into_owned())
    }
}
impl From<char> for Value {
    /// A `char` becomes a one-character string.
    fn from(x: char) -> Self {
        Value::Str(x.to_string())
    }
}
impl From<()> for Value {
    fn from(_: ()) -> Self {
        Value::Null
    }
}
impl From<Details> for Value {
    fn from(x: Details) -> Self {
        Value::Map(x)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    /// `None` is `null`, as a nil pointer is in Go.
    fn from(x: Option<T>) -> Self {
        x.map_or(Value::Null, Into::into)
    }
}
impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(x: Vec<T>) -> Self {
        Value::List(x.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Value> + Clone> From<&[T]> for Value {
    fn from(x: &[T]) -> Self {
        Value::List(x.iter().cloned().map(Into::into).collect())
    }
}
impl<K: Into<String>, V: Into<Value>> From<BTreeMap<K, V>> for Value {
    fn from(x: BTreeMap<K, V>) -> Self {
        Value::Map(x.into())
    }
}
impl<K: Into<String>, V: Into<Value>, S> From<HashMap<K, V, S>> for Value {
    fn from(x: HashMap<K, V, S>) -> Self {
        Value::Map(x.into())
    }
}

impl From<()> for Details {
    /// `()` means "no details".
    fn from(_: ()) -> Self {
        Details::new()
    }
}
impl From<Option<Details>> for Details {
    /// `None` means "no details", like a nil `D` in Go.
    fn from(x: Option<Details>) -> Self {
        x.unwrap_or_default()
    }
}
impl<K: Into<String>, V: Into<Value>> From<BTreeMap<K, V>> for Details {
    fn from(x: BTreeMap<K, V>) -> Self {
        Details(x.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}
impl<K: Into<String>, V: Into<Value>, S> From<HashMap<K, V, S>> for Details {
    fn from(x: HashMap<K, V, S>) -> Self {
        Details(x.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}
impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Details {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(it: I) -> Self {
        Details(it.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

#[cfg(feature = "serde_json")]
impl From<serde_json::Value> for Value {
    fn from(x: serde_json::Value) -> Self {
        match x {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(u) = n.as_u64() {
                    Value::Uint(u)
                } else {
                    Value::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            serde_json::Value::String(s) => Value::Str(s),
            serde_json::Value::Array(a) => Value::List(a.into_iter().map(Into::into).collect()),
            serde_json::Value::Object(m) => Value::Map(m.into()),
        }
    }
}
#[cfg(feature = "serde_json")]
impl From<serde_json::Map<String, serde_json::Value>> for Details {
    fn from(m: serde_json::Map<String, serde_json::Value>) -> Self {
        Details(m.into_iter().map(|(k, v)| (k, v.into())).collect())
    }
}
#[cfg(feature = "serde_json")]
impl From<serde_json::Value> for Details {
    /// An object becomes the details; any other value becomes `{"value": <it>}`.
    fn from(x: serde_json::Value) -> Self {
        match x {
            serde_json::Value::Object(m) => m.into(),
            serde_json::Value::Null => Details::new(),
            other => Details::new().with("value", other),
        }
    }
}

// ---- encoder ----

/// Returned when a value cannot be encoded (a NaN or an infinity); the record is dropped.
pub(crate) struct Unencodable;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Appends `s` as a JSON string the way Go's `encoding/json` does with HTML escaping on.
pub(crate) fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '<' || c == '>' || c == '&' => {
                let b = c as u32 as u8;
                out.push_str("\\u00");
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Go's float encoding: shortest digits; plain notation, or exponent notation when the
/// magnitude is below 1e-6 or at least 1e21, with Go's exponent form (`e+21`, `e-7`).
fn write_float(out: &mut String, f: f64, f32v: Option<f32>) -> Result<(), Unencodable> {
    if !f.is_finite() {
        return Err(Unencodable);
    }
    let exp = match f32v {
        Some(x) => x != 0.0 && (x.abs() < 1e-6 || x.abs() >= 1e21),
        None => f != 0.0 && (f.abs() < 1e-6 || f.abs() >= 1e21),
    };
    let s = match (f32v, exp) {
        (Some(x), false) => format!("{}", x),
        (Some(x), true) => format!("{:e}", x),
        (None, false) => format!("{}", f),
        (None, true) => format!("{:e}", f),
    };
    if exp {
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        out.push_str(m);
        out.push('e');
        if let Some(neg) = e.strip_prefix('-') {
            out.push('-');
            out.push_str(neg);
        } else {
            out.push('+');
            out.push_str(e);
        }
    } else {
        out.push_str(&s);
    }
    Ok(())
}

pub(crate) fn write_map(out: &mut String, d: &Details) -> Result<(), Unencodable> {
    out.push('{');
    for (i, (k, v)) in d.0.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_str(out, k);
        out.push(':');
        write_value(out, v)?;
    }
    out.push('}');
    Ok(())
}

pub(crate) fn write_value(out: &mut String, v: &Value) -> Result<(), Unencodable> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => {
            let _ = write!(out, "{}", i);
        }
        Value::Uint(u) => {
            let _ = write!(out, "{}", u);
        }
        Value::Float(f) => write_float(out, *f, None)?,
        Value::Float32(f) => write_float(out, *f as f64, Some(*f))?,
        Value::Str(s) => write_str(out, s),
        Value::List(l) => {
            out.push('[');
            for (i, x) in l.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, x)?;
            }
            out.push(']');
        }
        Value::Map(m) => write_map(out, m)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(v: impl Into<Value>) -> String {
        v.into().to_json().unwrap()
    }

    #[test]
    fn strings() {
        assert_eq!(j("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(j("<&>"), "\"\\u003c\\u0026\\u003e\"");
        assert_eq!(j("\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f}"), "\"\\n\\r\\t\\b\\f\\u0001\\u001f\u{7f}\"");
        assert_eq!(j("x\u{2028}y\u{2029}é"), "\"x\\u2028y\\u2029é\"");
    }

    #[test]
    fn floats() {
        assert_eq!(j(0.1f64), "0.1");
        assert_eq!(j(-0.0f64), "-0");
        assert_eq!(j(1e20f64), "100000000000000000000");
        assert_eq!(j(1e21f64), "1e+21");
        assert_eq!(j(1.5e-7f64), "1.5e-7");
        assert_eq!(j(1e-6f64), "0.000001");
        assert_eq!(j(1e100f64), "1e+100");
        assert_eq!(j(0.1f32), "0.1");
        assert!(Value::Float(f64::NAN).to_json().is_none());
    }

    #[test]
    fn maps_sorted() {
        let d = Details::new().with("b", 1).with("a", vec![1u8, 2]).with("A", Details::new());
        assert_eq!(d.to_json().unwrap(), r#"{"A":{},"a":[1,2],"b":1}"#);
    }
}
