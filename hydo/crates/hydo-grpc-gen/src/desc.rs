//! The schema: a project's service definitions as one canonical `FileDescriptorSet` (grpc/desc.pb).
//!
//! Byte identity with the Go generator (protocompile + protodesc + `proto.MarshalOptions{
//! Deterministic: true}`) rests on three things:
//! - the well-known imports (`google/protobuf/*.proto`) are the descriptors protocompile v0.14.1
//!   takes from protobuf-go v1.35.2, recorded once in `wkt.pb` (see [`WKT`]);
//! - every file is re-encoded as Go's deterministic marshal of a `descriptorpb` message writes it
//!   ([`normalize`]): known fields in field-number order (each repeated field's elements in their
//!   order, packed as the field is declared), unknown fields (custom options) after them in the
//!   order they came, scalars in their canonical encoding, repeated occurrences of a singular
//!   message merged; the field set is Go's `descriptor.proto` (the one in `wkt.pb`);
//! - files are ordered dependencies first, ties by name, without source info ([`canonical`]).

use prost_reflect::{Cardinality, DescriptorPool, Kind, MessageDescriptor};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The standard imports exactly as the Go generator writes them (one `FileDescriptorSet`):
/// `google/protobuf/{any,api,compiler/plugin,cpp_features,descriptor,duration,empty,field_mask,
/// go_features,java_features,source_context,struct,timestamp,type,wrappers}.proto`, produced by
/// `load.FromProtoDir` (protocompile v0.14.1, protobuf-go v1.35.2) on a stub file that imports
/// them all (docs/HISTORY.md, Milestone 113). sha256 f9696902ad5a824e…
pub const WKT: &[u8] = include_bytes!("wkt.pb");

/// One file of a set: its name, its dependencies and its encoded `FileDescriptorProto`.
#[derive(Clone, Debug)]
pub struct FileBytes {
    pub name: String,
    pub deps: Vec<String>,
    pub bytes: Vec<u8>,
}

/// Errors of this module are plain messages (the generator prints them after "hydo-grpc-gen: ").
pub type Result<T> = std::result::Result<T, String>;

fn descriptor_pool() -> &'static DescriptorPool {
    static P: OnceLock<DescriptorPool> = OnceLock::new();
    P.get_or_init(|| {
        let files = split_set(WKT).expect("wkt.pb");
        let d = files
            .into_iter()
            .find(|f| f.name == "google/protobuf/descriptor.proto")
            .expect("descriptor.proto in wkt.pb");
        let mut set = Vec::new();
        put_len(&mut set, 1, &d.bytes);
        DescriptorPool::decode(set.as_slice()).expect("descriptor.proto decodes")
    })
}

fn file_desc() -> MessageDescriptor {
    descriptor_pool()
        .get_message_by_name("google.protobuf.FileDescriptorProto")
        .expect("FileDescriptorProto")
}

// ---- the wire format ----

pub fn get_varint(b: &[u8], i: &mut usize) -> Result<u64> {
    let mut v: u64 = 0;
    for shift in (0..70).step_by(7) {
        let Some(&c) = b.get(*i) else {
            return Err("unexpected EOF".into());
        };
        *i += 1;
        if shift == 63 && c > 1 {
            return Err("variable length integer overflow".into());
        }
        v |= u64::from(c & 0x7f) << shift;
        if c < 0x80 {
            return Ok(v);
        }
    }
    Err("variable length integer overflow".into())
}

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn put_tag(out: &mut Vec<u8>, num: u32, wt: u8) {
    put_varint(out, (u64::from(num) << 3) | u64::from(wt));
}

pub fn put_len(out: &mut Vec<u8>, num: u32, data: &[u8]) {
    put_tag(out, num, 2);
    put_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// One field occurrence: number, wire type, the payload (for LEN: the bytes; for VARINT: the
/// value; I32/I64: the little-endian value) and the raw bytes of the whole occurrence.
struct Occ<'a> {
    num: u32,
    wt: u8,
    val: u64,
    data: &'a [u8],
    raw: &'a [u8],
}

fn fields(b: &[u8]) -> Result<Vec<Occ<'_>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let key = get_varint(b, &mut i)?;
        let num = (key >> 3) as u32;
        let wt = (key & 7) as u8;
        if num == 0 || key >> 3 > 0x1fff_ffff {
            return Err("invalid field number".into());
        }
        let (val, data): (u64, &[u8]) = match wt {
            0 => (get_varint(b, &mut i)?, &[]),
            1 => {
                let s = b.get(i..i + 8).ok_or("unexpected EOF")?;
                i += 8;
                (u64::from_le_bytes(s.try_into().unwrap()), &[])
            }
            5 => {
                let s = b.get(i..i + 4).ok_or("unexpected EOF")?;
                i += 4;
                (u64::from(u32::from_le_bytes(s.try_into().unwrap())), &[])
            }
            2 => {
                let n = get_varint(b, &mut i)? as usize;
                let s = b.get(i..i.checked_add(n).ok_or("unexpected EOF")?).ok_or("unexpected EOF")?;
                i += n;
                (0, s)
            }
            3 => {
                // a group: skip to its end tag (descriptor.proto has none; keep it raw)
                let mut depth = 1;
                while depth > 0 {
                    let k = get_varint(b, &mut i)?;
                    match k & 7 {
                        0 => {
                            get_varint(b, &mut i)?;
                        }
                        1 => i += 8,
                        5 => i += 4,
                        2 => {
                            let n = get_varint(b, &mut i)? as usize;
                            i += n;
                        }
                        3 => depth += 1,
                        4 => depth -= 1,
                        _ => return Err("invalid wire type".into()),
                    }
                    if i > b.len() {
                        return Err("unexpected EOF".into());
                    }
                }
                (0, &[])
            }
            _ => return Err("invalid wire type".into()),
        };
        out.push(Occ { num, wt, val, data, raw: &b[start..i] });
    }
    Ok(out)
}

/// Splits an encoded `FileDescriptorSet` into its files (unknown top-level fields are dropped,
/// as Go drops nothing but has none to keep: a set has only field 1).
pub fn split_set(b: &[u8]) -> Result<Vec<FileBytes>> {
    let mut out = Vec::new();
    for o in fields(b)? {
        if o.num == 1 && o.wt == 2 {
            out.push(file_info(o.data.to_vec())?);
        }
    }
    Ok(out)
}

/// The name and dependencies of an encoded `FileDescriptorProto`.
pub fn file_info(bytes: Vec<u8>) -> Result<FileBytes> {
    let mut name = String::new();
    let mut deps = Vec::new();
    for o in fields(&bytes)? {
        match (o.num, o.wt) {
            (1, 2) => name = String::from_utf8_lossy(o.data).into_owned(),
            (3, 2) => deps.push(String::from_utf8_lossy(o.data).into_owned()),
            _ => {}
        }
    }
    Ok(FileBytes { name, deps, bytes })
}

fn packable(k: &Kind) -> bool {
    !matches!(k, Kind::String | Kind::Bytes | Kind::Message(_))
}

fn wire_of(k: &Kind) -> u8 {
    match k {
        Kind::Double | Kind::Fixed64 | Kind::Sfixed64 => 1,
        Kind::Float | Kind::Fixed32 | Kind::Sfixed32 => 5,
        Kind::String | Kind::Bytes | Kind::Message(_) => 2,
        _ => 0,
    }
}

/// A scalar's canonical value as Go holds it after unmarshalling, from one wire value.
fn canon_scalar(k: &Kind, v: u64) -> u64 {
    match k {
        Kind::Int32 | Kind::Enum(_) => v as u32 as i32 as i64 as u64,
        Kind::Uint32 => v as u32 as u64,
        Kind::Sint32 => v as u32 as u64,
        Kind::Bool => u64::from(v != 0),
        Kind::Fixed32 | Kind::Sfixed32 | Kind::Float => v as u32 as u64,
        _ => v,
    }
}

fn put_scalar(out: &mut Vec<u8>, k: &Kind, v: u64) {
    match wire_of(k) {
        1 => out.extend_from_slice(&v.to_le_bytes()),
        5 => out.extend_from_slice(&(v as u32).to_le_bytes()),
        _ => put_varint(out, v),
    }
}

fn read_scalars(o: &Occ, k: &Kind, into: &mut Vec<u64>) -> Result<()> {
    let w = wire_of(k);
    if o.wt == 2 && packable(k) {
        let d = o.data;
        let mut i = 0;
        while i < d.len() {
            let v = match w {
                1 => {
                    let s = d.get(i..i + 8).ok_or("unexpected EOF")?;
                    i += 8;
                    u64::from_le_bytes(s.try_into().unwrap())
                }
                5 => {
                    let s = d.get(i..i + 4).ok_or("unexpected EOF")?;
                    i += 4;
                    u64::from(u32::from_le_bytes(s.try_into().unwrap()))
                }
                _ => get_varint(d, &mut i)?,
            };
            into.push(canon_scalar(k, v));
        }
        return Ok(());
    }
    if o.wt != w {
        return Err(format!("field {}: wrong wire type {}", o.num, o.wt));
    }
    into.push(canon_scalar(k, o.val));
    Ok(())
}

/// How Go came by a file, which decides how it writes the file's custom options.
#[derive(Clone, Copy)]
pub enum Origin<'a> {
    /// compiled from .proto source (protocompile, protodesc): custom options are extension
    /// fields of the options messages, written before the known fields (sorted by number), their
    /// message values in Go's legacy field order; float and double defaults are re-rendered from
    /// their values. The pool is the compiled schema (it knows the extensions).
    Source(&'a DescriptorPool),
    /// decoded from bytes (a descriptor set, reflection): custom options are unknown fields,
    /// written after the known ones, as they came.
    Wire,
}

enum Vals {
    Scalars(Vec<u64>),
    Bytes(Vec<Vec<u8>>),
    Msgs(Vec<Vec<u8>>),
}

/// `defval.Marshal` of a float or double default ("1e+10", "inf", "nan").
fn render_default(text: &[u8], float32: bool) -> Option<Vec<u8>> {
    let s = std::str::from_utf8(text).ok()?;
    let v: f64 = match s {
        "inf" => f64::INFINITY,
        "-inf" => f64::NEG_INFINITY,
        "nan" | "-nan" => f64::NAN,
        _ => s.parse().ok()?,
    };
    let (v, bits) = if float32 { (f64::from(v as f32), 32) } else { (v, 64) };
    let out = if v.is_nan() {
        "nan".to_string()
    } else if v == f64::INFINITY {
        "inf".to_string()
    } else if v == f64::NEG_INFINITY {
        "-inf".to_string()
    } else {
        hydo_grpc::gofmt::format_g(v, bits)
    };
    Some(out.into_bytes())
}

/// Re-encodes a message of type `md` (a descriptor.proto message) as Go's deterministic marshal
/// of the decoded message writes it.
pub fn normalize(md: &MessageDescriptor, b: &[u8], origin: Origin) -> Result<Vec<u8>> {
    let mut known: BTreeMap<u32, Vals> = BTreeMap::new();
    let mut exts: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut unknown: Vec<u8> = Vec::new();
    let has_ranges = md.extension_ranges().len() > 0;
    for o in fields(b)? {
        let Some(fd) = md.get_field(o.num) else {
            match origin {
                Origin::Source(pool) if has_ranges => {
                    let mut raw = o.raw.to_vec();
                    if o.wt == 2 {
                        let ext = pool.get_message_by_name(md.full_name()).and_then(|m| m.get_extension(o.num));
                        if let Some(Kind::Message(emd)) = ext.map(|e| e.kind()) {
                            raw.clear();
                            put_len(&mut raw, o.num, &normalize_dyn(&emd, o.data, pool)?);
                        }
                    }
                    exts.push((o.num, raw));
                }
                _ => unknown.extend_from_slice(o.raw),
            }
            continue;
        };
        let kind = fd.kind();
        let repeated = fd.cardinality() == Cardinality::Repeated;
        match &kind {
            Kind::Message(_) => {
                if o.wt != 2 {
                    unknown.extend_from_slice(o.raw);
                    continue;
                }
                let e = known.entry(o.num).or_insert_with(|| Vals::Msgs(Vec::new()));
                if let Vals::Msgs(v) = e {
                    if repeated || v.is_empty() {
                        v.push(o.data.to_vec());
                    } else {
                        // a singular message seen twice: Go merges, which is parsing the
                        // concatenation of both encodings
                        v[0].extend_from_slice(o.data);
                    }
                }
            }
            Kind::String | Kind::Bytes => {
                if o.wt != 2 {
                    unknown.extend_from_slice(o.raw);
                    continue;
                }
                let e = known.entry(o.num).or_insert_with(|| Vals::Bytes(Vec::new()));
                if let Vals::Bytes(v) = e {
                    if !repeated {
                        v.clear();
                    }
                    v.push(o.data.to_vec());
                }
            }
            k => {
                let mut got = Vec::new();
                if read_scalars(&o, k, &mut got).is_err() {
                    unknown.extend_from_slice(o.raw);
                    continue;
                }
                let e = known.entry(o.num).or_insert_with(|| Vals::Scalars(Vec::new()));
                if let Vals::Scalars(v) = e {
                    if repeated {
                        v.extend(got);
                    } else if let Some(last) = got.last() {
                        *v = vec![*last];
                    }
                }
            }
        }
    }
    if let Origin::Source(_) = origin {
        if md.full_name() == "google.protobuf.FieldDescriptorProto" {
            let typ = match known.get(&5) {
                Some(Vals::Scalars(v)) => v.last().copied(),
                _ => None,
            };
            if let (Some(t @ (1 | 2)), Some(Vals::Bytes(d))) = (typ, known.get_mut(&7)) {
                if let Some(last) = d.last_mut() {
                    if let Some(r) = render_default(last, t == 2) {
                        *last = r;
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    exts.sort_by_key(|(n, _)| *n);
    for (_, raw) in exts {
        out.extend_from_slice(&raw);
    }
    for (num, vals) in known {
        let fd = md.get_field(num).expect("known field");
        let kind = fd.kind();
        match vals {
            Vals::Msgs(v) => {
                let sub = kind.as_message().expect("message kind").clone();
                for m in v {
                    let n = normalize(&sub, &m, origin)?;
                    put_len(&mut out, num, &n);
                }
            }
            Vals::Bytes(v) => {
                for s in v {
                    put_len(&mut out, num, &s);
                }
            }
            Vals::Scalars(v) => {
                if fd.is_packed() {
                    if !v.is_empty() {
                        let mut p = Vec::new();
                        for x in &v {
                            put_scalar(&mut p, &kind, *x);
                        }
                        put_len(&mut out, num, &p);
                    }
                } else {
                    for x in v {
                        put_tag(&mut out, num, wire_of(&kind));
                        put_scalar(&mut out, &kind, x);
                    }
                }
            }
        }
    }
    out.extend_from_slice(&unknown);
    Ok(out)
}

/// A map entry's key, for Go's deterministic key order.
#[derive(PartialEq, PartialOrd)]
enum MapKeyOrd {
    I(i64),
    U(u64),
    S(Vec<u8>),
}

fn entry_key(entry: &MessageDescriptor, data: &[u8]) -> Result<MapKeyOrd> {
    let kfd = entry.map_entry_key_field();
    let kind = kfd.kind();
    let mut key = None;
    for o in fields(data)? {
        if o.num == 1 {
            key = Some(match kind {
                Kind::String => MapKeyOrd::S(o.data.to_vec()),
                Kind::Int32 | Kind::Sfixed32 => MapKeyOrd::I(o.val as u32 as i32 as i64),
                Kind::Int64 | Kind::Sfixed64 => MapKeyOrd::I(o.val as i64),
                Kind::Sint32 | Kind::Sint64 => MapKeyOrd::I(((o.val >> 1) as i64) ^ -((o.val & 1) as i64)),
                Kind::Bool => MapKeyOrd::U(u64::from(o.val != 0)),
                _ => MapKeyOrd::U(o.val),
            });
        }
    }
    Ok(key.unwrap_or(match kind {
        Kind::String => MapKeyOrd::S(Vec::new()),
        Kind::Uint32 | Kind::Uint64 | Kind::Fixed32 | Kind::Fixed64 | Kind::Bool => MapKeyOrd::U(0),
        _ => MapKeyOrd::I(0),
    }))
}

/// Re-encodes a value of a user message type (a custom option's value) as Go's deterministic
/// marshal of a dynamicpb message writes it (order.LegacyFieldOrder): extensions first (by
/// number), then the fields outside a oneof (by number), then the oneof members (by oneof, then
/// number), then unknown fields; map entries by key. Scalars keep their encoding.
pub fn normalize_dyn(md: &MessageDescriptor, b: &[u8], pool: &DescriptorPool) -> Result<Vec<u8>> {
    // (sort key, raw occurrences); repeated fields keep their order
    let mut groups: BTreeMap<(u8, u32, u32), Vec<Vec<u8>>> = BTreeMap::new();
    let mut unknown: Vec<u8> = Vec::new();
    for o in fields(b)? {
        if let Some(fd) = md.get_field(o.num) {
            let key = match fd.containing_oneof() {
                Some(od) if !od.is_synthetic() => {
                    let idx = md.oneofs().position(|x| x.name() == od.name()).unwrap_or(0) as u32;
                    (2u8, idx, o.num)
                }
                _ => (1u8, 0, o.num),
            };
            let mut raw = o.raw.to_vec();
            if o.wt == 2 && !fd.is_map() {
                if let Kind::Message(sub) = fd.kind() {
                    raw.clear();
                    put_len(&mut raw, o.num, &normalize_dyn(&sub, o.data, pool)?);
                }
            }
            groups.entry(key).or_default().push(raw);
        } else if let Some(ext) = md.get_extension(o.num) {
            let mut raw = o.raw.to_vec();
            if o.wt == 2 {
                if let Kind::Message(sub) = ext.kind() {
                    raw.clear();
                    put_len(&mut raw, o.num, &normalize_dyn(&sub, o.data, pool)?);
                }
            }
            groups.entry((0u8, 0, o.num)).or_default().push(raw);
        } else {
            unknown.extend_from_slice(o.raw);
        }
    }
    let mut out = Vec::new();
    for ((_, _, num), mut raws) in groups {
        if let Some(fd) = md.get_field(num) {
            if fd.is_map() {
                if let Kind::Message(entry) = fd.kind() {
                    let mut keyed = Vec::new();
                    for r in raws.drain(..) {
                        let occ = fields(&r)?;
                        let data = occ.first().map(|o| o.data.to_vec()).unwrap_or_default();
                        keyed.push((entry_key(&entry, &data)?, r));
                    }
                    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    raws = keyed.into_iter().map(|(_, r)| r).collect();
                }
            }
        }
        for r in raws {
            out.extend_from_slice(&r);
        }
    }
    out.extend_from_slice(&unknown);
    Ok(out)
}

/// A file as the Go generator keeps it: normalized, without source info (field 9).
pub fn clean_file(bytes: &[u8], origin: Origin) -> Result<FileBytes> {
    let n = normalize(&file_desc(), bytes, origin)?;
    let mut out = Vec::new();
    for o in fields(&n)? {
        if o.num != 9 {
            out.extend_from_slice(o.raw);
        }
    }
    file_info(out)
}

/// `load.Canonical`: files dependencies first (ties by name), without source info, each name
/// once (the last copy wins, in the place of the first).
pub fn canonical(files: Vec<FileBytes>, origin: Origin) -> Result<Vec<FileBytes>> {
    let mut by: BTreeMap<String, FileBytes> = BTreeMap::new();
    for f in files {
        let c = clean_file(&f.bytes, origin)?;
        by.insert(c.name.clone(), c);
    }
    let names: Vec<String> = by.keys().cloned().collect();
    let mut done: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    fn visit(
        n: &str,
        by: &BTreeMap<String, FileBytes>,
        done: &mut HashSet<String>,
        out: &mut Vec<FileBytes>,
    ) {
        if !done.insert(n.to_string()) {
            return;
        }
        let Some(f) = by.get(n) else { return };
        let mut deps = f.deps.clone();
        deps.sort();
        for d in &deps {
            visit(d, by, done, out);
        }
        out.push(f.clone());
    }
    for n in &names {
        visit(n, &by, &mut done, &mut out);
    }
    Ok(out)
}

/// The encoded set (`load.Marshal`).
pub fn encode_set(files: &[FileBytes]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in files {
        put_len(&mut out, 1, &f.bytes);
    }
    out
}

/// Every `.proto` file under `dir` (recursively), as slash-separated paths relative to it, sorted.
fn proto_names(dir: &Path) -> Result<Vec<String>> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        let mut ents: Vec<_> = fs::read_dir(d)?.collect::<std::io::Result<Vec<_>>>()?;
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_dir() {
                walk(base, &p, out)?;
            } else if p.to_string_lossy().ends_with(".proto") {
                let rel = p.strip_prefix(base).unwrap_or(&p);
                let s: Vec<String> =
                    rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                out.push(s.join("/"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out).map_err(|e| format!("{}: {}", dir.display(), e))?;
    out.sort();
    Ok(out)
}

struct WktResolver {
    files: Vec<FileBytes>,
}

impl protox::file::FileResolver for WktResolver {
    fn open_file(&self, name: &str) -> std::result::Result<protox::file::File, protox::Error> {
        for f in &self.files {
            if f.name == name {
                return protox::file::File::decode_file_descriptor_proto(f.bytes.as_slice())
                    .map_err(protox::Error::new);
            }
        }
        Err(protox::Error::file_not_found(name))
    }
}

/// `load.FromProtoDir`: compiles every `.proto` file under `dir` (import paths: `dir`, then
/// `extra`; the well-known types are built in) and returns the canonical set and the names.
pub fn from_proto_dir(dir: &Path, extra: &[PathBuf]) -> Result<(Vec<FileBytes>, Vec<String>)> {
    let names = proto_names(dir)?;
    if names.is_empty() {
        return Err(format!("no .proto files under {}", dir.display()));
    }
    let mut includes = vec![dir.to_path_buf()];
    includes.extend(extra.iter().cloned());
    let mut chain = protox::file::ChainFileResolver::new();
    for inc in &includes {
        chain.add(protox::file::IncludeFileResolver::new(inc.clone()));
    }
    let wkt = split_set(WKT)?;
    chain.add(WktResolver { files: wkt.clone() });
    let mut c = protox::Compiler::with_file_resolver(chain);
    c.include_imports(true);
    c.include_source_info(false);
    for n in &names {
        c.open_file(n).map_err(|e| format!("{e:?}"))?;
    }
    let set = c.encode_file_descriptor_set();
    let mut files = Vec::new();
    for f in split_set(&set)? {
        // a standard import that no import path holds: the recorded Go descriptor
        let on_disk = includes.iter().any(|i| i.join(&f.name).is_file());
        match wkt.iter().find(|w| w.name == f.name) {
            Some(w) if !on_disk => files.push(w.clone()),
            _ => files.push(f),
        }
    }
    let pool = c.descriptor_pool();
    Ok((canonical(files, Origin::Source(&pool))?, names))
}

/// `load.FromDescriptorSet`: a compiled `FileDescriptorSet` (protoc --include_imports -o, buf
/// build -o).
pub fn from_descriptor_set(path: &Path) -> Result<Vec<FileBytes>> {
    let b = fs::read(path).map_err(|e| format!("open {}: {}", path.display(), io_msg(&e)))?;
    let files = split_set(&b)
        .map_err(|e| format!("{}: not a FileDescriptorSet: {}", path.display(), e))?;
    canonical(files, Origin::Wire)
}

/// Go's wording of the common I/O errors ("no such file or directory").
pub fn io_msg(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ => {
            let s = e.to_string();
            match s.find(" (os error") {
                Some(i) => s[..i].to_lowercase(),
                None => s,
            }
        }
    }
}
