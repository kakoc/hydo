//! Messages as compact protobuf JSON, written as Go's protojson writes them
//! (`MarshalOptions{UseProtoNames: true}` followed by `json.Compact`): populated fields in
//! declaration order under their proto names, 64-bit integers as strings, enum value names, the
//! special forms of the well-known types, Go's float and string formatting. The call records
//! hold these bodies, so their bytes match the Go client's.

use crate::b64;
use crate::gofmt;
use crate::valuegen::decl_fields;
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, MapKey, ReflectMessage, Value};

/// protojson's string escaping (no HTML escaping).
fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn float(out: &mut String, x: f64, bits: u32) {
    if x.is_nan() {
        out.push_str("\"NaN\"");
    } else if x == f64::INFINITY {
        out.push_str("\"Infinity\"");
    } else if x == f64::NEG_INFINITY {
        out.push_str("\"-Infinity\"");
    } else {
        out.push_str(&gofmt::json_float(x, bits));
    }
}

/// One message as protojson writes it, or the error it returns.
pub fn marshal(m: &DynamicMessage) -> Result<String, String> {
    let mut out = String::new();
    message(&mut out, m)?;
    Ok(out)
}

fn field_by_name(m: &DynamicMessage, name: &str) -> Option<FieldDescriptor> {
    m.descriptor().get_field_by_name(name)
}

fn get<'a>(m: &'a DynamicMessage, name: &str) -> std::borrow::Cow<'a, Value> {
    let fd = field_by_name(m, name).expect("well-known field");
    m.get_field(&fd)
}

fn has(m: &DynamicMessage, name: &str) -> bool {
    field_by_name(m, name).map(|fd| m.has_field(&fd)).unwrap_or(false)
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian calendar.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn trim_frac(mut x: String) -> String {
    for suf in ["000", "000", ".000"] {
        if let Some(s) = x.strip_suffix(suf) {
            x = s.to_string();
        }
    }
    x
}

fn message(out: &mut String, m: &DynamicMessage) -> Result<(), String> {
    let md = m.descriptor();
    let full = md.full_name().to_string();
    match full.as_str() {
        "google.protobuf.Any" => {
            if !has(m, "type_url") {
                if !has(m, "value") {
                    out.push_str("{}");
                    return Ok(());
                }
                return Err(format!("{full}: missing type_url field"));
            }
            let url = get(m, "type_url").as_str().unwrap_or("").to_string();
            return Err(format!("{full}: unable to resolve {}: not found", gofmt::quote(&url)));
        }
        "google.protobuf.Timestamp" => {
            let secs = get(m, "seconds").as_i64().unwrap_or(0);
            let nanos = get(m, "nanos").as_i32().unwrap_or(0);
            if !(-62_135_596_800..=253_402_300_799).contains(&secs) {
                return Err(format!("{full}: seconds out of range {secs}"));
            }
            if !(0..1_000_000_000).contains(&nanos) {
                return Err(format!("{full}: nanos out of range {nanos}"));
            }
            let (y, mo, d) = civil(secs.div_euclid(86400));
            let t = secs.rem_euclid(86400);
            let x = format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}", y, mo, d, t / 3600, t / 60 % 60, t % 60, nanos);
            write_str(out, &(trim_frac(x) + "Z"));
            return Ok(());
        }
        "google.protobuf.Duration" => {
            let mut secs = get(m, "seconds").as_i64().unwrap_or(0);
            let mut nanos = get(m, "nanos").as_i32().unwrap_or(0) as i64;
            if !(-315_576_000_000..=315_576_000_000).contains(&secs) {
                return Err(format!("{full}: seconds out of range {secs}"));
            }
            if nanos <= -1_000_000_000 || nanos >= 1_000_000_000 {
                return Err(format!("{full}: nanos out of range {nanos}"));
            }
            if (secs > 0 && nanos < 0) || (secs < 0 && nanos > 0) {
                return Err(format!("{full}: signs of seconds and nanos do not match"));
            }
            let mut sign = "";
            if secs < 0 || nanos < 0 {
                sign = "-";
                secs = -secs;
                nanos = -nanos;
            }
            let x = format!("{sign}{secs}.{nanos:09}");
            write_str(out, &(trim_frac(x) + "s"));
            return Ok(());
        }
        "google.protobuf.BoolValue" | "google.protobuf.Int32Value" | "google.protobuf.Int64Value"
        | "google.protobuf.UInt32Value" | "google.protobuf.UInt64Value" | "google.protobuf.FloatValue"
        | "google.protobuf.DoubleValue" | "google.protobuf.StringValue" | "google.protobuf.BytesValue" => {
            let fd = md.get_field(1).ok_or("wrapper without field 1")?;
            return singular(out, &fd, &m.get_field(&fd));
        }
        "google.protobuf.Struct" => {
            let fd = md.get_field(1).ok_or("Struct without field 1")?;
            return map(out, &fd, &m.get_field(&fd));
        }
        "google.protobuf.ListValue" => {
            let fd = md.get_field(1).ok_or("ListValue without field 1")?;
            return list(out, &fd, &m.get_field(&fd));
        }
        "google.protobuf.Value" => {
            for fd in decl_fields(&md) {
                if m.has_field(&fd) {
                    if let Value::F64(x) = &*m.get_field(&fd) {
                        if x.is_nan() || x.is_infinite() {
                            return Err(format!("{full}: invalid {} value", gofmt::format_g(*x, 64)));
                        }
                    }
                    return singular(out, &fd, &m.get_field(&fd));
                }
            }
            return Err(format!("{full}: none of the oneof fields is set"));
        }
        "google.protobuf.FieldMask" => {
            let fd = md.get_field(1).ok_or("FieldMask without field 1")?;
            let mut parts = Vec::new();
            if let Value::List(l) = &*m.get_field(&fd) {
                for v in l {
                    let s = v.as_str().unwrap_or("");
                    let c = camel(s);
                    if snake(&c) != s || s.contains("__") {
                        return Err(format!("{full}.paths contains invalid path: {}", gofmt::quote(s)));
                    }
                    parts.push(c);
                }
            }
            write_str(out, &parts.join(","));
            return Ok(());
        }
        _ => {}
    }
    out.push('{');
    let mut first = true;
    for fd in decl_fields(&md) {
        if !m.has_field(&fd) {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        let name = if fd.is_group() {
            match fd.kind() {
                Kind::Message(g) => g.name().to_string(),
                _ => fd.name().to_string(),
            }
        } else {
            fd.name().to_string()
        };
        write_str(out, &name);
        out.push(':');
        let v = m.get_field(&fd);
        if fd.is_map() {
            map(out, &fd, &v)?;
        } else if fd.is_list() {
            list(out, &fd, &v)?;
        } else {
            singular(out, &fd, &v)?;
        }
    }
    out.push('}');
    Ok(())
}

/// strs.JSONCamelCase
fn camel(s: &str) -> String {
    let mut out = String::new();
    let mut was_underscore = false;
    for &c in s.as_bytes() {
        if c != b'_' {
            out.push(if was_underscore && c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c } as char);
        }
        was_underscore = c == b'_';
    }
    out
}

/// strs.JSONSnakeCase
fn snake(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn list(out: &mut String, fd: &FieldDescriptor, v: &Value) -> Result<(), String> {
    out.push('[');
    if let Value::List(l) = v {
        for (i, x) in l.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            singular(out, fd, x)?;
        }
    }
    out.push(']');
    Ok(())
}

fn map(out: &mut String, fd: &FieldDescriptor, v: &Value) -> Result<(), String> {
    let Kind::Message(entry) = fd.kind() else { return Err("map without entry".into()) };
    let vfd = entry.map_entry_value_field();
    out.push('{');
    if let Value::Map(mp) = v {
        let mut keys: Vec<&MapKey> = mp.keys().collect();
        keys.sort_by(|a, b| key_order(a, b));
        for (i, k) in keys.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let ks = match k {
                MapKey::Bool(b) => b.to_string(),
                MapKey::I32(x) => x.to_string(),
                MapKey::I64(x) => x.to_string(),
                MapKey::U32(x) => x.to_string(),
                MapKey::U64(x) => x.to_string(),
                MapKey::String(s) => s.clone(),
            };
            write_str(out, &ks);
            out.push(':');
            singular(out, &vfd, &mp[*k])?;
        }
    }
    out.push('}');
    Ok(())
}

/// order.GenericKeyOrder
pub fn key_order(a: &MapKey, b: &MapKey) -> std::cmp::Ordering {
    match (a, b) {
        (MapKey::Bool(x), MapKey::Bool(y)) => x.cmp(y),
        (MapKey::I32(x), MapKey::I32(y)) => x.cmp(y),
        (MapKey::I64(x), MapKey::I64(y)) => x.cmp(y),
        (MapKey::U32(x), MapKey::U32(y)) => x.cmp(y),
        (MapKey::U64(x), MapKey::U64(y)) => x.cmp(y),
        (MapKey::String(x), MapKey::String(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ => std::cmp::Ordering::Equal,
    }
}

fn singular(out: &mut String, fd: &FieldDescriptor, v: &Value) -> Result<(), String> {
    match v {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::I32(x) => out.push_str(&x.to_string()),
        Value::U32(x) => out.push_str(&x.to_string()),
        Value::I64(x) => {
            out.push('"');
            out.push_str(&x.to_string());
            out.push('"');
        }
        Value::U64(x) => {
            out.push('"');
            out.push_str(&x.to_string());
            out.push('"');
        }
        Value::F32(x) => float(out, f64::from(*x), 32),
        Value::F64(x) => float(out, *x, 64),
        Value::String(s) => write_str(out, s),
        Value::Bytes(b) => write_str(out, &b64::encode(b)),
        Value::EnumNumber(n) => {
            let Kind::Enum(e) = fd.kind() else { return Err("enum value of a non-enum field".into()) };
            if e.full_name() == "google.protobuf.NullValue" {
                out.push_str("null");
            } else {
                match e.enum_descriptor_proto().value.iter().find(|x| x.number() == *n) {
                    Some(ev) => write_str(out, ev.name()),
                    None => out.push_str(&n.to_string()),
                }
            }
        }
        Value::Message(m) => message(out, m)?,
        Value::List(_) | Value::Map(_) => return Err("unexpected list or map".into()),
    }
    Ok(())
}

/// The JSON of one call's messages (an array for a stream); a message protojson cannot write
/// becomes `{"hydo_grpc_error":"..."}`, as in the Go client.
pub fn json(ms: &[DynamicMessage], stream: bool) -> String {
    let parts: Vec<String> = ms
        .iter()
        .map(|m| match marshal(m) {
            Ok(s) => s,
            Err(e) => format!("{{\"hydo_grpc_error\":{}}}", gofmt::quote(&e)),
        })
        .collect();
    if !stream && parts.len() == 1 {
        return parts.into_iter().next().unwrap();
    }
    format!("[{}]", parts.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(1_767_225_600 / 86400), (2026, 1, 1));
        assert_eq!(civil(-62_135_596_800 / 86400), (1, 1, 1));
        assert_eq!(civil(253_402_300_799 / 86400), (9999, 12, 31));
        assert_eq!(trim_frac("2026-01-01T00:00:00.000000000".into()), "2026-01-01T00:00:00");
        assert_eq!(trim_frac("2026-01-01T00:00:00.120000000".into()), "2026-01-01T00:00:00.120");
        assert_eq!(trim_frac("1.000001000".into()), "1.000001");
    }
}
