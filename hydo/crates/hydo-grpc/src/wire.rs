//! The wire encoding of a request, deterministic: fields in number order, map entries in key
//! order (key and value always written), repeated scalars packed where the field is. prost-reflect
//! keeps maps in a HashMap and writes them in its random order; the bytes a server receives
//! should not depend on that. (Go's client wrote dynamic messages in its map order too; under
//! hydo that order was fixed by the guest's seeded random numbers. The servers decode either.)

use crate::pjson::key_order;
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, MapKey, ReflectMessage, Value};

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn tag(out: &mut Vec<u8>, num: u32, wt: u8) {
    varint(out, (u64::from(num) << 3) | u64::from(wt));
}

fn wire_type(k: &Kind) -> u8 {
    match k {
        Kind::Double | Kind::Fixed64 | Kind::Sfixed64 => 1,
        Kind::Float | Kind::Fixed32 | Kind::Sfixed32 => 5,
        Kind::String | Kind::Bytes | Kind::Message(_) => 2,
        _ => 0,
    }
}

/// A scalar's payload (no tag).
fn scalar(out: &mut Vec<u8>, k: &Kind, v: &Value) {
    match (k, v) {
        (Kind::Bool, Value::Bool(b)) => varint(out, u64::from(*b)),
        (Kind::Int32, Value::I32(x)) => varint(out, *x as i64 as u64),
        (Kind::Sint32, Value::I32(x)) => varint(out, u64::from(((*x << 1) ^ (*x >> 31)) as u32)),
        (Kind::Sfixed32, Value::I32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (Kind::Int64, Value::I64(x)) => varint(out, *x as u64),
        (Kind::Sint64, Value::I64(x)) => varint(out, ((*x << 1) ^ (*x >> 63)) as u64),
        (Kind::Sfixed64, Value::I64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (Kind::Uint32, Value::U32(x)) => varint(out, u64::from(*x)),
        (Kind::Fixed32, Value::U32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (Kind::Uint64, Value::U64(x)) => varint(out, *x),
        (Kind::Fixed64, Value::U64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (Kind::Float, Value::F32(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (Kind::Double, Value::F64(x)) => out.extend_from_slice(&x.to_le_bytes()),
        (Kind::Enum(_), Value::EnumNumber(x)) => varint(out, *x as i64 as u64),
        (Kind::String, Value::String(s)) => {
            varint(out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        (Kind::Bytes, Value::Bytes(b)) => {
            varint(out, b.len() as u64);
            out.extend_from_slice(b);
        }
        _ => {}
    }
}

fn single(out: &mut Vec<u8>, num: u32, fd: &FieldDescriptor, v: &Value) {
    let k = fd.kind();
    if let (Kind::Message(_), Value::Message(m)) = (&k, v) {
        if fd.is_group() {
            tag(out, num, 3);
            out.extend(encode(m));
            tag(out, num, 4);
        } else {
            let b = encode(m);
            tag(out, num, 2);
            varint(out, b.len() as u64);
            out.extend(b);
        }
        return;
    }
    tag(out, num, wire_type(&k));
    scalar(out, &k, v);
}

/// The message's encoding.
pub fn encode(m: &DynamicMessage) -> Vec<u8> {
    let md = m.descriptor();
    let mut out = Vec::new();
    for fd in md.fields() {
        if !m.has_field(&fd) {
            continue;
        }
        let v = m.get_field(&fd);
        let num = fd.number();
        match &*v {
            Value::Map(mp) => {
                let Kind::Message(entry) = fd.kind() else {
                    continue;
                };
                let (kf, vf) = (entry.map_entry_key_field(), entry.map_entry_value_field());
                let mut keys: Vec<&MapKey> = mp.keys().collect();
                keys.sort_by(|a, b| key_order(a, b));
                for k in keys {
                    let mut e = Vec::new();
                    single(&mut e, 1, &kf, &Value::from(k.clone()));
                    single(&mut e, 2, &vf, &mp[k]);
                    tag(&mut out, num, 2);
                    varint(&mut out, e.len() as u64);
                    out.extend(e);
                }
            }
            Value::List(l) => {
                if fd.is_packed() {
                    let mut p = Vec::new();
                    for x in l {
                        scalar(&mut p, &fd.kind(), x);
                    }
                    tag(&mut out, num, 2);
                    varint(&mut out, p.len() as u64);
                    out.extend(p);
                } else {
                    for x in l {
                        single(&mut out, num, &fd, x);
                    }
                }
            }
            x => single(&mut out, num, &fd, x),
        }
    }
    out
}
