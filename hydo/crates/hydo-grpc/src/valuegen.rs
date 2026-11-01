//! Valid random protobuf messages from a seeded stream.
//!
//! "Valid" means valid for the schema: every value fits its field's type (int32 in range, a
//! declared enum value, at most one member of a oneof, distinct map keys, required proto2 fields
//! set, nesting bounded). Business rules come from hints (`spec::FieldHint`): ranges, choices,
//! lengths, unique values, constants, values taken from earlier calls ("from").
//!
//! Determinism: the result is a pure function of (descriptor, hints, the random stream, what Env
//! returns). Fields are visited in declaration order and map keys are generated, not iterated.
//! The order of draws from the stream is the Go generator's, so the same seed gives the same
//! requests as before.

use crate::b64;
use crate::gofmt;
use crate::rng::R;
use crate::spec::{Defaults, FieldHint, From};
use prost_reflect::{
    Cardinality, DynamicMessage, FieldDescriptor, Kind, MapKey, MessageDescriptor, ReflectMessage,
    Value,
};
use std::collections::BTreeMap;

/// Why a message could not be generated.
#[derive(Clone, Debug, PartialEq)]
pub enum GenError {
    /// a `from` field (fallback skip) has no earlier value yet: the operation is not made now
    Skip,
    Other(String),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::Skip => write!(f, "valuegen: no earlier value for a `from` field yet"),
            GenError::Other(s) => write!(f, "{s}"),
        }
    }
}

type GResult<T> = Result<T, GenError>;

fn other<T>(s: String) -> GResult<T> {
    Err(GenError::Other(s))
}

/// What the generator cannot derive from the seed alone.
pub trait Env {
    /// An integer no other call of `unique` in this run returned.
    fn unique(&mut self) -> u64;
    /// A value recorded for a `from` source (its canonical string form), choosing among several
    /// with r; None if there is none.
    fn take(&mut self, f: &From, r: &mut R) -> Option<String>;
}

/// The fields of a message in declaration order (prost-reflect iterates them by number).
pub fn decl_fields(md: &MessageDescriptor) -> Vec<FieldDescriptor> {
    md.descriptor_proto()
        .field
        .iter()
        .filter_map(|f| md.get_field(f.number() as u32))
        .collect()
}

fn is_message(fd: &FieldDescriptor) -> bool {
    matches!(fd.kind(), Kind::Message(_)) && !fd.is_group()
}

fn is_message_or_group(fd: &FieldDescriptor) -> bool {
    matches!(fd.kind(), Kind::Message(_))
}

/// Generates one message type's values.
pub struct Gen<'a> {
    pub d: &'a Defaults,
    pub hints: &'a BTreeMap<String, FieldHint>,
    pub oneofs: &'a BTreeMap<String, Option<Vec<String>>>,
    pub env: &'a mut dyn Env,
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

static NO_HINT: FieldHint = FieldHint {
    value: None,
    choice: Vec::new(),
    range: Vec::new(),
    len: Vec::new(),
    unique: false,
    prefix: String::new(),
    format: String::new(),
    omit: false,
    prob: None,
    from: None,
};

impl<'a> Gen<'a> {
    /// A new message of type `md` with every field generated.
    pub fn message(&mut self, md: &MessageDescriptor, r: &mut R) -> GResult<DynamicMessage> {
        let mut m = DynamicMessage::new(md.clone());
        self.fill(&mut m, "", 0, r)?;
        Ok(m)
    }

    fn hint(&self, path: &str) -> &'a FieldHint {
        self.hints.get(path).unwrap_or(&NO_HINT)
    }

    /// the hint fixes the value (so presence is not left to chance)
    fn hinted(&self, path: &str) -> bool {
        match self.hints.get(path) {
            Some(h) => {
                h.value.is_some()
                    || !h.choice.is_empty()
                    || h.from.is_some()
                    || h.unique
                    || !h.range.is_empty()
            }
            None => false,
        }
    }

    fn fill(&mut self, m: &mut DynamicMessage, prefix: &str, depth: i64, r: &mut R) -> GResult<()> {
        let md = m.descriptor();
        if wkt(&md, m, r) {
            return Ok(());
        }
        // oneofs first: decide which member (if any) is set
        let mut chosen: Vec<u32> = Vec::new();
        for od in md.oneofs() {
            if od.is_synthetic() {
                continue; // proto3 `optional`: a field with presence
            }
            let allowed = self
                .oneofs
                .get(&join(prefix, od.name()))
                .and_then(|v| v.as_ref());
            let mut members: Vec<FieldDescriptor> = Vec::new();
            for fd in od.fields() {
                let p = join(prefix, fd.name());
                if let Some(h) = self.hints.get(&p) {
                    if h.omit {
                        continue;
                    }
                }
                if let Some(a) = allowed {
                    if !a.is_empty() && !a.iter().any(|x| x == fd.name()) {
                        continue;
                    }
                }
                if is_message(&fd) && depth + 1 > self.d.depth && !self.hinted(&p) {
                    continue;
                }
                members.push(fd);
            }
            if members.is_empty() || (self.d.oneof_none > 0.0 && r.chance(self.d.oneof_none)) {
                continue;
            }
            let i = r.intn(members.len() as i64) as usize;
            chosen.push(members[i].number());
        }
        for fd in decl_fields(&md) {
            let path = join(prefix, fd.name());
            let h = self.hint(&path);
            if h.omit && fd.cardinality() != Cardinality::Required {
                continue;
            }
            match fd.containing_oneof() {
                Some(od) if !od.is_synthetic() => {
                    if !chosen.contains(&fd.number()) {
                        continue;
                    }
                }
                _ => {
                    if !self.present(&fd, &path, h, depth, r) {
                        continue;
                    }
                }
            }
            self.field(m, &fd, &path, h, depth, r)?;
        }
        Ok(())
    }

    fn present(
        &self,
        fd: &FieldDescriptor,
        path: &str,
        h: &FieldHint,
        depth: i64,
        r: &mut R,
    ) -> bool {
        if fd.cardinality() == Cardinality::Required {
            return true;
        }
        if fd.is_list() || fd.is_map() {
            return true; // the length decides (0 = absent)
        }
        let is_msg = is_message_or_group(fd);
        if is_msg && depth + 1 > self.d.depth && !self.hinted(path) {
            return false;
        }
        if !fd.supports_presence() {
            return true; // proto3 implicit scalar: always has a value (maybe the zero value)
        }
        if let Some(p) = h.prob {
            return r.chance(p);
        }
        if self.hinted(path) {
            return true;
        }
        if is_msg {
            return r.chance(self.d.message_prob);
        }
        r.chance(self.d.optional_prob)
    }

    fn length(h: &FieldHint, def: [i64; 2], r: &mut R) -> i64 {
        let (lo, hi) = if h.len.len() == 2 {
            (h.len[0], h.len[1])
        } else {
            (def[0], def[1])
        };
        r.range(lo, hi)
    }

    fn field(
        &mut self,
        m: &mut DynamicMessage,
        fd: &FieldDescriptor,
        path: &str,
        h: &FieldHint,
        depth: i64,
        r: &mut R,
    ) -> GResult<()> {
        if fd.is_map() {
            let n = Self::length(h, self.d.map_len, r);
            let entry = match fd.kind() {
                Kind::Message(e) => e,
                _ => unreachable!(),
            };
            let kfd = entry.map_entry_key_field();
            let vfd = entry.map_entry_value_field();
            let kpath = format!("{path}.key");
            let vpath = format!("{path}.value");
            let kh = self.hint(&kpath);
            let vh = self.hint(&vpath);
            let mut entries: Vec<(MapKey, Value)> = Vec::new();
            let mut tries = 0;
            while (entries.len() as i64) < n && tries < 8 * n + 8 {
                tries += 1;
                let kv = self.scalar(&kfd, &kpath, kh, r)?;
                let mk = kv.into_map_key().expect("map key");
                if entries.iter().any(|(k, _)| *k == mk) {
                    continue;
                }
                let v = if is_message(&vfd) {
                    let sub = match vfd.kind() {
                        Kind::Message(s) => s,
                        _ => unreachable!(),
                    };
                    let mut vm = DynamicMessage::new(sub);
                    self.fill(&mut vm, &vpath, depth + 1, r)?;
                    Value::Message(vm)
                } else {
                    self.scalar(&vfd, &vpath, vh, r)?
                };
                entries.push((mk, v));
            }
            let mp = m.get_field_mut(fd);
            if let Value::Map(mp) = mp {
                for (k, v) in entries {
                    mp.insert(k, v);
                }
            }
            return Ok(());
        }
        if fd.is_list() {
            let n = Self::length(h, self.d.repeated_len, r);
            let mut items = Vec::new();
            for _ in 0..n {
                if is_message_or_group(fd) {
                    if depth + 1 > self.d.depth {
                        break;
                    }
                    let sub = match fd.kind() {
                        Kind::Message(s) => s,
                        _ => unreachable!(),
                    };
                    let mut e = DynamicMessage::new(sub);
                    self.fill(&mut e, path, depth + 1, r)?;
                    items.push(Value::Message(e));
                } else {
                    items.push(self.scalar(fd, path, h, r)?);
                }
            }
            if let Value::List(l) = m.get_field_mut(fd) {
                l.extend(items);
            }
            return Ok(());
        }
        if let Kind::Message(sub) = fd.kind() {
            let mut s = DynamicMessage::new(sub);
            self.fill(&mut s, path, depth + 1, r)?;
            m.set_field(fd, Value::Message(s));
            return Ok(());
        }
        let v = self.scalar(fd, path, h, r)?;
        m.set_field(fd, v);
        Ok(())
    }

    /// One non-message value of fd's kind.
    fn scalar(
        &mut self,
        fd: &FieldDescriptor,
        path: &str,
        h: &FieldHint,
        r: &mut R,
    ) -> GResult<Value> {
        if let Some(f) = &h.from {
            if let Some(s) = self.env.take(f, r) {
                return parse(fd, &s).or_else(other);
            }
            if f.fallback != "generate" {
                return Err(GenError::Skip);
            }
        }
        if let Some(v) = &h.value {
            return parse(fd, v).or_else(other);
        }
        if !h.choice.is_empty() {
            let i = r.intn(h.choice.len() as i64) as usize;
            return parse(fd, &h.choice[i]).or_else(other);
        }
        if h.unique {
            return self.unique(fd, h);
        }
        let explicit = h.range.len() == 2;
        let wrap = |e: String| GenError::Other(format!("{path}: {e}"));
        Ok(match fd.kind() {
            Kind::Bool => Value::Bool(r.intn(2) == 1),
            Kind::Enum(_) => enum_value(fd, r),
            Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
                let (lo, hi) = int_range(h, i32::MIN as i64, i32::MAX as i64).map_err(wrap)?;
                Value::I32(signed(r, lo, hi, explicit) as i32)
            }
            Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
                let (lo, hi) = int_range(h, i64::MIN, i64::MAX).map_err(wrap)?;
                Value::I64(signed(r, lo, hi, explicit))
            }
            Kind::Uint32 | Kind::Fixed32 => {
                let (lo, hi) = uint_range(h, u32::MAX as u64).map_err(wrap)?;
                Value::U32(unsigned(r, lo, hi, explicit) as u32)
            }
            Kind::Uint64 | Kind::Fixed64 => {
                let (lo, hi) = uint_range(h, u64::MAX).map_err(wrap)?;
                Value::U64(unsigned(r, lo, hi, explicit))
            }
            Kind::Float => Value::F32(float(h, r).map_err(wrap)? as f32),
            Kind::Double => Value::F64(float(h, r).map_err(wrap)?),
            Kind::String => {
                let n = Self::length(h, self.d.string_len, r);
                Value::String(format!("{}{}", h.prefix, rand_string(r, &h.format, n)))
            }
            Kind::Bytes => {
                let n = Self::length(h, self.d.bytes_len, r);
                let b: Vec<u8> = (0..n.max(0)).map(|_| r.next_u64() as u8).collect();
                Value::Bytes(b.into())
            }
            Kind::Message(_) => return other(format!("{path}: unsupported kind message")),
        })
    }

    fn unique(&mut self, fd: &FieldDescriptor, h: &FieldHint) -> GResult<Value> {
        let n = self.env.unique();
        match fd.kind() {
            Kind::String => return Ok(Value::String(format!("{}{}", h.prefix, n))),
            Kind::Bytes => {
                return Ok(Value::Bytes(
                    format!("{}{}", h.prefix, n).into_bytes().into(),
                ))
            }
            _ => {}
        }
        let base = if h.range.len() == 2 {
            h.range[0].as_str()
        } else {
            "1"
        };
        let lo = gofmt::parse_int(base, 64)
            .map_err(|e| GenError::Other(format!("unique: range {}: {}", gofmt::quote(base), e)))?;
        // the first unique value is the range's lower bound (default 1), then lo+1, ...
        let x = lo.wrapping_add(n as i64).wrapping_sub(1);
        if h.range.len() == 2 {
            let hi = gofmt::parse_int(&h.range[1], 64).unwrap_or(0);
            if x > hi {
                return other(format!(
                    "unique: more values than the range [{}, {}] holds",
                    h.range[0], h.range[1]
                ));
            }
        }
        parse(fd, &x.to_string()).or_else(other)
    }
}

/// The declared values of an enum field's type in declaration order.
fn enum_decl(fd: &FieldDescriptor) -> Vec<(String, i32)> {
    match fd.kind() {
        Kind::Enum(e) => e
            .enum_descriptor_proto()
            .value
            .iter()
            .map(|v| (v.name().to_string(), v.number()))
            .collect(),
        _ => Vec::new(),
    }
}

/// a declared value; number 0 named *UNSPECIFIED is left out (the proto style guide's "no
/// value" marker, usually rejected by servers) unless it is the only one.
fn enum_value(fd: &FieldDescriptor, r: &mut R) -> Value {
    let vs = enum_decl(fd);
    let ok: Vec<i32> = vs
        .iter()
        .filter(|(n, x)| !(*x == 0 && n.ends_with("UNSPECIFIED")))
        .map(|(_, x)| *x)
        .collect();
    if ok.is_empty() {
        return Value::EnumNumber(vs.first().map(|v| v.1).unwrap_or(0));
    }
    Value::EnumNumber(ok[r.intn(ok.len() as i64) as usize])
}

fn int_range(h: &FieldHint, min: i64, max: i64) -> Result<(i64, i64), String> {
    if h.range.len() != 2 {
        return Ok((min, max));
    }
    let lo = gofmt::parse_int(&h.range[0], 64)
        .map_err(|e| format!("range {}: {}", gofmt::quote(&h.range[0]), e))?;
    let hi = gofmt::parse_int(&h.range[1], 64)
        .map_err(|e| format!("range {}: {}", gofmt::quote(&h.range[1]), e))?;
    if lo < min || hi > max || lo > hi {
        return Err(format!(
            "range [{lo}, {hi}] does not fit the field ({min}..{max})"
        ));
    }
    Ok((lo, hi))
}

fn uint_range(h: &FieldHint, max: u64) -> Result<(u64, u64), String> {
    if h.range.len() != 2 {
        return Ok((0, max));
    }
    let lo = gofmt::parse_uint(&h.range[0], 64)
        .map_err(|e| format!("range {}: {}", gofmt::quote(&h.range[0]), e))?;
    let hi = gofmt::parse_uint(&h.range[1], 64)
        .map_err(|e| format!("range {}: {}", gofmt::quote(&h.range[1]), e))?;
    if hi > max || lo > hi {
        return Err(format!(
            "range [{lo}, {hi}] does not fit the field (0..{max})"
        ));
    }
    Ok((lo, hi))
}

/// With an explicit range, uniform in it. Without one, mostly small non-negative values (what
/// most APIs accept), sometimes larger ones, the edges, and a few negatives.
fn signed(r: &mut R, lo: i64, hi: i64, explicit: bool) -> i64 {
    if explicit {
        return r.range(lo, hi);
    }
    let p = r.intn(100);
    if p < 70 {
        r.range(0, 100)
    } else if p < 85 {
        r.range(0, 1_000_000)
    } else if p < 95 {
        let edges = [0, 1, hi];
        edges[r.intn(3) as usize]
    } else {
        r.range(-100, -1)
    }
}

fn unsigned(r: &mut R, lo: u64, hi: u64, explicit: bool) -> u64 {
    if explicit {
        return r.urange(lo, hi);
    }
    let p = r.intn(100);
    if p < 70 {
        r.urange(0, 100)
    } else if p < 90 {
        r.urange(0, 1_000_000)
    } else {
        let edges = [0, 1, hi];
        edges[r.intn(3) as usize]
    }
}

fn float(h: &FieldHint, r: &mut R) -> Result<f64, String> {
    if h.range.len() == 2 {
        let lo = gofmt::parse_float(&h.range[0], 64)
            .map_err(|e| format!("range {}: {}", gofmt::quote(&h.range[0]), e))?;
        let hi = gofmt::parse_float(&h.range[1], 64)
            .map_err(|e| format!("range {}: {}", gofmt::quote(&h.range[1]), e))?;
        if lo > hi {
            return Err(format!(
                "range [{}, {}] is empty",
                gofmt::format_g(lo, 64),
                gofmt::format_g(hi, 64)
            ));
        }
        return Ok(lo + r.float() * (hi - lo));
    }
    let p = r.intn(100);
    Ok(if p < 70 {
        (r.float() * 10000.0).round() / 100.0 // 0.00 .. 100.00
    } else if p < 90 {
        (r.float() * 2.0 - 1.0) * 1e6
    } else {
        let edges = [0.0, 1.0, 0.5];
        edges[r.intn(3) as usize]
    })
}

const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const DIGITS: &[u8] = b"0123456789";
const HEXD: &[u8] = b"0123456789abcdef";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn rand_string(r: &mut R, format: &str, n: i64) -> String {
    let set = match format {
        "lower" => LOWER,
        "digits" => DIGITS,
        "hex" => HEXD,
        "uuid" => {
            let mut b = [0u8; 16];
            for x in b.iter_mut() {
                *x = r.next_u64() as u8;
            }
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            return format!(
                "{}-{}-{}-{}-{}",
                hex(&b[0..4]),
                hex(&b[4..6]),
                hex(&b[6..8]),
                hex(&b[8..10]),
                hex(&b[10..16])
            );
        }
        _ => ALNUM,
    };
    let mut s = String::new();
    for _ in 0..n.max(0) {
        s.push(set[r.intn(set.len() as i64) as usize] as char);
    }
    s
}

/// The accepted string formats.
pub const FORMATS: [&str; 6] = ["", "alnum", "lower", "digits", "hex", "uuid"];

/// A scalar value as the string form `parse` reads (pools, choices, records).
pub fn canonical(fd: &FieldDescriptor, v: &Value) -> String {
    match (fd.kind(), v) {
        (Kind::Bool, Value::Bool(b)) => b.to_string(),
        (Kind::Enum(_), Value::EnumNumber(n)) => match enum_decl(fd).iter().find(|(_, x)| x == n) {
            Some((name, _)) => name.clone(),
            None => n.to_string(),
        },
        (_, Value::I32(x)) => x.to_string(),
        (_, Value::I64(x)) => x.to_string(),
        (_, Value::U32(x)) => x.to_string(),
        (_, Value::U64(x)) => x.to_string(),
        (_, Value::F32(x)) => gofmt::format_g(f64::from(*x), 32),
        (_, Value::F64(x)) => gofmt::format_g(*x, 64),
        (_, Value::String(s)) => s.clone(),
        (_, Value::Bytes(b)) => b64::encode(b),
        _ => String::new(),
    }
}

/// Reads a value of fd's kind from its string form: decimal numbers, true/false, enum value
/// names (or numbers), base64 for bytes (or the raw text if it is not valid base64).
pub fn parse(fd: &FieldDescriptor, s: &str) -> Result<Value, String> {
    let bad = |e: String| format!("field {}: value {}: {}", fd.full_name(), gofmt::quote(s), e);
    Ok(match fd.kind() {
        Kind::Bool => Value::Bool(gofmt::parse_bool(s).map_err(bad)?),
        Kind::Enum(e) => {
            let vs = enum_decl(fd);
            if let Some((_, n)) = vs.iter().find(|(name, _)| name == s) {
                Value::EnumNumber(*n)
            } else {
                match gofmt::parse_int(s, 32) {
                    Ok(n) if vs.iter().any(|(_, x)| i64::from(*x) == n) => {
                        Value::EnumNumber(n as i32)
                    }
                    _ => return Err(bad(format!("not a value of enum {}", e.full_name()))),
                }
            }
        }
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            Value::I32(gofmt::parse_int(s, 32).map_err(bad)? as i32)
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            Value::I64(gofmt::parse_int(s, 64).map_err(bad)?)
        }
        Kind::Uint32 | Kind::Fixed32 => Value::U32(gofmt::parse_uint(s, 32).map_err(bad)? as u32),
        Kind::Uint64 | Kind::Fixed64 => Value::U64(gofmt::parse_uint(s, 64).map_err(bad)?),
        Kind::Float => Value::F32(gofmt::parse_float(s, 32).map_err(bad)? as f32),
        Kind::Double => Value::F64(gofmt::parse_float(s, 64).map_err(bad)?),
        Kind::String => Value::String(s.to_string()),
        Kind::Bytes => match b64::decode(s) {
            Some(b) => Value::Bytes(b.into()),
            None => Value::Bytes(s.as_bytes().to_vec().into()),
        },
        Kind::Message(_) => return Err(bad("kind message has no string form".into())),
    })
}

/// The well-known types whose generic fill would be meaningless; true if it handled m.
fn wkt(md: &MessageDescriptor, m: &mut DynamicMessage, r: &mut R) -> bool {
    match md.full_name() {
        "google.protobuf.Timestamp" => {
            // 2026-01-01 .. +30 days, whole seconds: the guest clock starts at 2026-01-01
            m.set_field_by_name(
                "seconds",
                Value::I64(1_767_225_600 + r.range(0, 30 * 86400)),
            );
            true
        }
        "google.protobuf.Duration" => {
            m.set_field_by_name("seconds", Value::I64(r.range(0, 3600)));
            true
        }
        // empty: there is no type to put in an Any; an empty JSON object / list
        "google.protobuf.Any" | "google.protobuf.Struct" | "google.protobuf.ListValue" => true,
        "google.protobuf.Value" => {
            m.set_field_by_name("null_value", Value::EnumNumber(0)); // JSON null
            true
        }
        _ => false,
    }
}
