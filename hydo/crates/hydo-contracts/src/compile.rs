//! The plan's compiler: a program's ELF file and its debug information (DWARF), and the
//! `// hydo:` comments of its sources, give each contracted function's entry address, where its
//! parameters are at entry, the offsets of the fields its clauses read, and where its result is
//! at return.
//!
//! A value is a [`Place`]: in memory at an address that reads give, or in registers, piece by
//! piece (a Rust `&str` is a pointer and a length in two registers; so is an `Option<u64>`). What
//! it reads (docs/design/contracts.md, "Reading values"):
//!
//! - integers, booleans and pointers, and fields of structures through any number of pointers;
//! - since Milestone 255: Rust `String`, `&str`, `Vec<T>` and slices, and Go strings and slices
//!   (`len()`, elements, and a string compared with a literal), Rust `Option` and `Result` (the
//!   variant, and the value inside), Go's results after the first and its `error` (nil or not);
//! - the result: Rust's in one or two registers or in memory the caller gave, Go's in the
//!   integer registers in order.
//!
//! Everything else is refused with its reason.

use crate::lang::{self, Ast, Builtin, SrcContract, SrcRecord, When};
use crate::plan::{
    Base, BinOp, CallSite, Clause, ClauseKind, DynCase, Expr, Frame, Function, Input, Keep, Lang,
    Loc, Plan, Program, Show, Slot, Ty, Unchecked, Value, PLAN_TYPE, PLAN_V,
};
use gimli::{AttributeValue, EndianSlice, LittleEndian, Operation, UnitOffset};

/// A reference to a type: the unit's index and the offset in it (Go's types are in other units
/// than its functions).
type TRef = (usize, UnitOffset);
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type R<'a> = EndianSlice<'a, LittleEndian>;

/// How many elements an index computed per call can reach (`v[k]`): the engine reads the first
/// this many.
pub const ELEMS: u64 = 8;
/// How many bytes of a string a comparison with a literal reads at most.
pub const STR_BYTES: usize = 64;

/// One program with contracts: where it is in the guest, the file on the host, and the folders
/// (or files) of its sources.
#[derive(Clone, Debug)]
pub struct Source {
    pub program: String,
    pub binary: PathBuf,
    pub sources: Vec<PathBuf>,
}

/// The ELF file: its machine and sections.
struct Elf {
    data: Vec<u8>,
    machine: u16,
    sections: Vec<Sect>,
}

struct Sect {
    name: String,
    flags: u64,
    addr: u64,
    offset: u64,
    size: u64,
    nobits: bool,
}

fn rd16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn rd32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn rd64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

impl Elf {
    fn parse(data: Vec<u8>) -> Result<Elf, String> {
        let b = &data;
        if b.get(0..4) != Some(b"\x7fELF") || b.get(4) != Some(&2) || b.get(5) != Some(&1) {
            return Err("not a 64-bit little-endian ELF file".into());
        }
        let bad = || "a damaged ELF header".to_string();
        let machine = rd16(b, 18).ok_or_else(bad)?;
        let shoff = rd64(b, 40).ok_or_else(bad)? as usize;
        let shentsize = rd16(b, 58).ok_or_else(bad)? as usize;
        let shnum = rd16(b, 60).ok_or_else(bad)? as usize;
        let shstrndx = rd16(b, 62).ok_or_else(bad)? as usize;
        let mut raw = Vec::new();
        for i in 0..shnum {
            let o = shoff + i * shentsize;
            raw.push((
                rd32(b, o).ok_or_else(bad)?,
                rd32(b, o + 4).ok_or_else(bad)?,
                rd64(b, o + 8).ok_or_else(bad)?,
                rd64(b, o + 16).ok_or_else(bad)?,
                rd64(b, o + 24).ok_or_else(bad)?,
                rd64(b, o + 32).ok_or_else(bad)?,
            ));
        }
        let (_, _, _, _, stroff, strsize) = *raw.get(shstrndx).ok_or_else(bad)?;
        let strtab = b
            .get(stroff as usize..(stroff + strsize) as usize)
            .ok_or_else(bad)?;
        let sections = raw
            .iter()
            .map(|&(name, ty, flags, addr, offset, size)| {
                let n = strtab.get(name as usize..).unwrap_or(&[]);
                let end = n.iter().position(|&c| c == 0).unwrap_or(n.len());
                Sect {
                    name: String::from_utf8_lossy(&n[..end]).into_owned(),
                    flags,
                    addr,
                    offset,
                    size,
                    nobits: ty == 8,
                }
            })
            .collect();
        Ok(Elf {
            data,
            machine,
            sections,
        })
    }

    fn arch(&self) -> Result<&'static str, String> {
        match self.machine {
            0xb7 => Ok("arm64"),
            0x3e => Ok("x86_64"),
            m => Err(format!(
                "ELF machine {m:#x}: contracts read arm64 and x86_64 programs"
            )),
        }
    }

    /// A section's bytes, inflated when it is compressed (SHF_COMPRESSED with zlib, as Go
    /// writes its debug information).
    fn section(&self, name: &str) -> Result<Vec<u8>, String> {
        let Some(s) = self.sections.iter().find(|s| s.name == name) else {
            return Ok(Vec::new());
        };
        if s.nobits {
            return Ok(Vec::new());
        }
        let raw = self
            .data
            .get(s.offset as usize..(s.offset + s.size) as usize)
            .ok_or_else(|| format!("section {name} lies outside the file"))?;
        if s.flags & 0x800 == 0 {
            return Ok(raw.to_vec());
        }
        if rd32(raw, 0) != Some(1) {
            return Err(format!("section {name}: a compression other than zlib"));
        }
        let size = rd64(raw, 8).unwrap_or(0) as usize;
        let mut out = Vec::with_capacity(size);
        use std::io::Read;
        flate2::read::ZlibDecoder::new(&raw[24..])
            .read_to_end(&mut out)
            .map_err(|e| format!("section {name}: {e}"))?;
        Ok(out)
    }

    /// `n` bytes of the program at a virtual address.
    fn bytes_at(&self, addr: u64, n: usize) -> Option<&[u8]> {
        let s = self.sections.iter().find(|s| {
            s.flags & 2 != 0 && !s.nobits && s.addr <= addr && addr + n as u64 <= s.addr + s.size
        })?;
        let o = (s.offset + addr - s.addr) as usize;
        self.data.get(o..o + n)
    }
}

/// A type, as far as contracts read it.
#[derive(Clone, Debug)]
enum Type {
    Scalar(Ty, u8),
    /// A floating-point number of this many bytes (read as its bits since Milestone 285).
    Float(u8),
    Pointer(Option<TRef>),
    Struct(Struct),
    /// A fixed-size array: its element type and count (a Go map's group of slots; Milestone
    /// 285).
    Array(Option<TRef>, u64),
    Other(String),
}

#[derive(Clone, Debug, Default)]
struct Struct {
    name: String,
    size: u64,
    members: Vec<Member>,
    /// Rust's generic parameters (`T` of `Vec<T>`).
    tparams: Vec<(String, Option<TRef>)>,
    /// A Rust enumeration's variants.
    variants: Option<Variants>,
}

#[derive(Clone, Debug)]
struct Member {
    name: String,
    off: u64,
    ty: Option<TRef>,
}

#[derive(Clone, Debug, Default)]
struct Variants {
    /// The discriminant's offset and type (none: one variant only).
    discr: Option<(u64, Option<TRef>)>,
    list: Vec<Variant>,
}

#[derive(Clone, Debug)]
struct Variant {
    name: String,
    /// The discriminant's value for this variant (none: every value the others do not have).
    value: Option<u64>,
    off: u64,
    ty: Option<TRef>,
}

/// A string, a vector or a slice: where its pointer and length are, and its elements.
#[derive(Clone, Debug)]
struct Seq {
    ptr_off: u64,
    len_off: u64,
    elem: Option<TRef>,
    /// Text (`&str`, `String`, Go `string`): it compares with a string literal.
    text: bool,
    /// The elements belong to someone else (a Rust `&str` or slice, a Go string or slice): a
    /// parameter's elements in `ensures` are read at return, through the pointer at entry. A Rust
    /// `Vec` or `String` passed by value is the function's own and is dropped by then.
    borrowed: bool,
}

impl Struct {
    fn member(&self, n: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == n)
    }
    /// A Go interface (`error`, `any`): its first word says whether it is nil.
    fn iface(&self) -> bool {
        self.name == "runtime.iface" || self.name == "runtime.eface"
    }
}

struct Dwarf<'a> {
    dwarf: gimli::Dwarf<R<'a>>,
    units: Vec<gimli::Unit<R<'a>>>,
    /// The call frame information (`.eh_frame`) and the addresses its pointers count from.
    eh: Vec<u8>,
    eh_addr: u64,
    text_addr: u64,
    /// Go's runtime type descriptors of the types a recorded interface value is shown as: the
    /// type's name, its debug information, and the descriptor's address (Milestone 285).
    go_types: Vec<(String, TRef, u64)>,
}

/// The dynamic types of a Go interface that a recorded value shows (Milestone 285): Go's
/// `encoding/json` gives numbers as `float64`, and programs put these into `map[string]any`.
pub const GO_DYN_TYPES: [&str; 11] = [
    "float64", "float32", "int", "int64", "int32", "uint", "uint64", "uint32", "bool", "string",
    "uint8",
];

/// Go's attribute with the offset of a type's runtime descriptor in the program's section of
/// type descriptors (`DW_AT_go_runtime_type`).
const DW_AT_GO_RUNTIME_TYPE: gimli::DwAt = gimli::DwAt(0x2904);

impl Dwarf<'_> {
    /// The runtime type descriptors of [`GO_DYN_TYPES`]: each named type at the top of a unit
    /// with `DW_AT_go_runtime_type`, at `base` (the address of the section of type descriptors)
    /// plus the attribute's offset.
    fn find_go_types(&mut self, base: u64) {
        let mut out = Vec::new();
        for (ui, u) in self.units.iter().enumerate() {
            let Ok(mut tree) = u.entries_tree(None) else {
                continue;
            };
            let Ok(root) = tree.root() else { continue };
            let mut ch = root.children();
            while let Ok(Some(c)) = ch.next() {
                let e = c.entry();
                let Some(name) = self.name(ui, e) else {
                    continue;
                };
                if !GO_DYN_TYPES.contains(&name.as_str())
                    || out.iter().any(|x: &(String, TRef, u64)| x.0 == name)
                {
                    continue;
                }
                let off = match e.attr_value(DW_AT_GO_RUNTIME_TYPE) {
                    Ok(Some(AttributeValue::Addr(a))) => a,
                    Ok(Some(v)) => match v.udata_value() {
                        Some(a) => a,
                        None => continue,
                    },
                    _ => continue,
                };
                if off == 0 {
                    continue;
                }
                out.push((name, (ui, e.offset()), base + off));
            }
        }
        self.go_types = out;
    }

    /// The largest offset the call frame information gives the canonical frame address from
    /// register `reg` in the function at `pc`: the offset after the function's prologue, where
    /// the compiler's frame-base offsets count from.
    fn cfa_offset(&self, pc: u64, reg: u16) -> Option<i64> {
        use gimli::UnwindSection;
        let eh = gimli::EhFrame::new(&self.eh, LittleEndian);
        let bases = gimli::BaseAddresses::default()
            .set_eh_frame(self.eh_addr)
            .set_text(self.text_addr);
        let mut ctx: gimli::UnwindContext<usize> = gimli::UnwindContext::new();
        let fde = eh
            .fde_for_address(&bases, pc, gimli::EhFrame::cie_from_offset)
            .ok()?;
        let mut rows = fde.rows(&eh, &bases, &mut ctx).ok()?;
        let mut best: Option<i64> = None;
        while let Ok(Some(row)) = rows.next_row() {
            if let gimli::CfaRule::RegisterAndOffset { register, offset } = row.cfa() {
                if register.0 == reg {
                    best = Some(best.map_or(*offset, |b| b.max(*offset)));
                }
            }
        }
        best
    }
}

fn byte_size(e: &gimli::DebuggingInformationEntry<R<'_>>) -> u64 {
    match e.attr_value(gimli::DW_AT_byte_size) {
        Ok(Some(v)) => v.udata_value().unwrap_or(0),
        _ => 0,
    }
}

fn member_off(e: &gimli::DebuggingInformationEntry<R<'_>>) -> Option<u64> {
    match e.attr_value(gimli::DW_AT_data_member_location) {
        Ok(Some(v)) => v.udata_value(),
        _ => None,
    }
}

impl<'a> Dwarf<'a> {
    fn ty(&self, r: Option<TRef>) -> Type {
        let Some((mut unit, mut off)) = r else {
            return Type::Other("void".into());
        };
        for _ in 0..16 {
            let u = &self.units[unit];
            let Ok(e) = u.entry(off) else {
                return Type::Other("unreadable".into());
            };
            let name = self.name(unit, &e).unwrap_or_default();
            let size = byte_size(&e);
            match e.tag() {
                gimli::DW_TAG_typedef
                | gimli::DW_TAG_const_type
                | gimli::DW_TAG_volatile_type
                | gimli::DW_TAG_atomic_type => match self.type_ref(unit, &e) {
                    Some(t) => {
                        (unit, off) = t;
                        continue;
                    }
                    None => return Type::Other(name),
                },
                gimli::DW_TAG_base_type => {
                    let enc = match e.attr_value(gimli::DW_AT_encoding) {
                        Ok(Some(AttributeValue::Encoding(x))) => x,
                        _ => gimli::DW_ATE_unsigned,
                    };
                    let n = size.min(8) as u8;
                    let t = match enc {
                        gimli::DW_ATE_boolean => Ty::Bool,
                        gimli::DW_ATE_signed | gimli::DW_ATE_signed_char => Ty::I(n),
                        gimli::DW_ATE_unsigned
                        | gimli::DW_ATE_unsigned_char
                        | gimli::DW_ATE_UTF => Ty::U(n),
                        gimli::DW_ATE_float => return Type::Float(n),
                        _ => return Type::Other(name),
                    };
                    if size > 8 {
                        return Type::Other(name);
                    }
                    return Type::Scalar(t, n);
                }
                gimli::DW_TAG_enumeration_type if size <= 8 => {
                    return Type::Scalar(Ty::U(size as u8), size as u8)
                }
                gimli::DW_TAG_pointer_type
                | gimli::DW_TAG_reference_type
                | gimli::DW_TAG_rvalue_reference_type => {
                    return Type::Pointer(self.type_ref(unit, &e))
                }
                gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type => {
                    return Type::Struct(self.structure(unit, off, name, size))
                }
                gimli::DW_TAG_array_type => {
                    let elem = self.type_ref(unit, &e);
                    return match self.array_count(unit, off) {
                        Some(n) => Type::Array(elem, n),
                        None => Type::Other(name),
                    };
                }
                _ => return Type::Other(name),
            }
        }
        Type::Other("a chain of type names".into())
    }

    /// A structure's members, generic parameters and variants.
    fn structure(&self, unit: usize, off: UnitOffset, name: String, size: u64) -> Struct {
        let u = &self.units[unit];
        let mut s = Struct {
            name,
            size,
            ..Struct::default()
        };
        let Ok(mut tree) = u.entries_tree(Some(off)) else {
            return s;
        };
        let Ok(root) = tree.root() else { return s };
        let mut ch = root.children();
        while let Ok(Some(c)) = ch.next() {
            let ce = c.entry().clone();
            match ce.tag() {
                gimli::DW_TAG_member => {
                    if let Some(mo) = member_off(&ce) {
                        s.members.push(Member {
                            name: self.name(unit, &ce).unwrap_or_default(),
                            off: mo,
                            ty: self.type_ref(unit, &ce),
                        });
                    }
                }
                gimli::DW_TAG_template_type_parameter => {
                    s.tparams.push((
                        self.name(unit, &ce).unwrap_or_default(),
                        self.type_ref(unit, &ce),
                    ));
                }
                gimli::DW_TAG_variant_part => {
                    let mut v = Variants::default();
                    let discr_at = Self::same_unit_ref(&ce, gimli::DW_AT_discr);
                    let mut vch = c.children();
                    while let Ok(Some(vc)) = vch.next() {
                        let ve = vc.entry().clone();
                        if ve.tag() == gimli::DW_TAG_member && Some(ve.offset()) == discr_at {
                            v.discr = member_off(&ve).map(|o| (o, self.type_ref(unit, &ve)));
                            continue;
                        }
                        if ve.tag() != gimli::DW_TAG_variant {
                            continue;
                        }
                        let value = match ve.attr_value(gimli::DW_AT_discr_value) {
                            Ok(Some(x)) => x
                                .udata_value()
                                .or_else(|| x.sdata_value().map(|v| v as u64)),
                            _ => None,
                        };
                        let mut mch = vc.children();
                        while let Ok(Some(m)) = mch.next() {
                            let me = m.entry();
                            if me.tag() == gimli::DW_TAG_member {
                                v.list.push(Variant {
                                    name: self.name(unit, me).unwrap_or_default(),
                                    value,
                                    off: member_off(me).unwrap_or(0),
                                    ty: self.type_ref(unit, me),
                                });
                            }
                        }
                    }
                    s.variants = Some(v);
                }
                _ => {}
            }
        }
        s
    }

    /// The number of elements of an array type (its subrange's count, or upper bound plus one).
    fn array_count(&self, unit: usize, off: UnitOffset) -> Option<u64> {
        let u = &self.units[unit];
        let mut tree = u.entries_tree(Some(off)).ok()?;
        let root = tree.root().ok()?;
        let mut ch = root.children();
        while let Ok(Some(c)) = ch.next() {
            let e = c.entry();
            if e.tag() != gimli::DW_TAG_subrange_type {
                continue;
            }
            if let Ok(Some(v)) = e.attr_value(gimli::DW_AT_count) {
                return v.udata_value();
            }
            if let Ok(Some(v)) = e.attr_value(gimli::DW_AT_upper_bound) {
                return v.udata_value().map(|x| x + 1);
            }
        }
        None
    }

    /// The bytes a value of a type takes.
    fn size_of(&self, r: Option<TRef>) -> u64 {
        match self.ty(r) {
            Type::Scalar(_, n) | Type::Float(n) => n as u64,
            Type::Pointer(_) => 8,
            Type::Struct(s) => s.size,
            Type::Array(e, n) => self.size_of(e) * n,
            Type::Other(_) => {
                // an array or another type: its own byte size, when it says one
                let Some((unit, off)) = r else { return 0 };
                self.units[unit]
                    .entry(off)
                    .map(|e| byte_size(&e))
                    .unwrap_or(0)
            }
        }
    }

    /// A string, a vector or a slice.
    fn seq(&self, s: &Struct) -> Option<Seq> {
        let n = s.name.as_str();
        // Rust: &str and &[T] are a pointer and a length
        if let (Some(p), Some(l)) = (s.member("data_ptr"), s.member("length")) {
            let elem = match self.ty(p.ty) {
                Type::Pointer(t) => t,
                _ => None,
            };
            return Some(Seq {
                ptr_off: p.off,
                len_off: l.off,
                elem,
                text: n.ends_with("str"),
                borrowed: true,
            });
        }
        // Rust: String holds a Vec<u8>
        if n == "String" || n.ends_with("::String") {
            let v = s.member("vec")?;
            let Type::Struct(vs) = self.ty(v.ty) else {
                return None;
            };
            let inner = self.seq(&vs)?;
            return Some(Seq {
                ptr_off: v.off + inner.ptr_off,
                len_off: v.off + inner.len_off,
                text: true,
                ..inner
            });
        }
        // Rust: Vec<T> is a raw buffer (whose pointer is untyped) and a length
        if n.starts_with("Vec<") {
            let buf = s.member("buf")?;
            let len = s.member("len")?;
            let ptr = self.first_pointer(buf.ty, 0)?;
            let elem = s.tparams.iter().find(|t| t.0 == "T").and_then(|t| t.1);
            return Some(Seq {
                ptr_off: buf.off + ptr,
                len_off: len.off,
                elem,
                text: false,
                borrowed: false,
            });
        }
        // Go: string, and []T
        if n == "string" {
            let (p, l) = (s.member("str")?, s.member("len")?);
            return Some(Seq {
                ptr_off: p.off,
                len_off: l.off,
                elem: match self.ty(p.ty) {
                    Type::Pointer(t) => t,
                    _ => None,
                },
                text: true,
                borrowed: true,
            });
        }
        // a named slice type (`type nat []Word`, the words of `math/big.Int`) keeps its own
        // name in Go's debug information, with the members of every slice
        let go_slice = s.members.len() == 3
            && s.members
                .iter()
                .map(|m| m.name.as_str())
                .eq(["array", "len", "cap"]);
        if n.starts_with("[]") || go_slice {
            let (p, l) = (s.member("array")?, s.member("len")?);
            return Some(Seq {
                ptr_off: p.off,
                len_off: l.off,
                elem: match self.ty(p.ty) {
                    Type::Pointer(t) => t,
                    _ => None,
                },
                text: false,
                borrowed: true,
            });
        }
        None
    }

    /// The offset of the first pointer inside a structure (a Rust `Vec`'s buffer).
    fn first_pointer(&self, r: Option<TRef>, depth: u32) -> Option<u64> {
        if depth > 8 {
            return None;
        }
        match self.ty(r) {
            Type::Pointer(_) => Some(0),
            Type::Struct(s) => s
                .members
                .iter()
                .find_map(|m| self.first_pointer(m.ty, depth + 1).map(|o| m.off + o)),
            _ => None,
        }
    }

    /// The scalar pieces of a value (offset, size), over every variant: how Rust's calling
    /// convention returns it ([`Reads::result_place`]).
    fn slots(
        &self,
        r: Option<TRef>,
        at: u64,
        out: &mut Vec<(u64, u64)>,
        depth: u32,
    ) -> Result<(), String> {
        if depth > 16 {
            return Err("a type nested too deep".into());
        }
        match self.ty(r) {
            Type::Scalar(_, n) => out.push((at, n as u64)),
            Type::Pointer(_) => out.push((at, 8)),
            Type::Float(_) => return Err("a floating-point value".into()),
            Type::Other(n) => return Err(format!("a type the compiler does not read ({n})")),
            Type::Array(..) => return Err("an array".into()),
            Type::Struct(s) => {
                for m in &s.members {
                    self.slots(m.ty, at + m.off, out, depth + 1)?;
                }
                if let Some(v) = &s.variants {
                    if let Some((o, t)) = v.discr {
                        self.slots(t, at + o, out, depth + 1)?;
                    }
                    for x in &v.list {
                        self.slots(x.ty, at + x.off, out, depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The machine words of a Go value in the order Go's calling convention puts them into
    /// registers (offset, size).
    fn go_words(
        &self,
        r: Option<TRef>,
        at: u64,
        out: &mut Vec<(u64, u64)>,
        depth: u32,
    ) -> Result<(), String> {
        if depth > 16 {
            return Err("a type nested too deep".into());
        }
        match self.ty(r) {
            Type::Scalar(_, n) => out.push((at, n as u64)),
            Type::Pointer(_) => out.push((at, 8)),
            Type::Float(_) => {
                return Err(
                    "a floating-point result (Go returns it in a floating-point register)".into(),
                )
            }
            Type::Array(..) => return Err("a result that is an array".into()),
            Type::Other(n) => {
                return Err(format!(
                    "a result of a type the compiler does not read ({n})"
                ))
            }
            Type::Struct(s) => {
                for m in &s.members {
                    self.go_words(m.ty, at + m.off, out, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    fn name(&self, unit: usize, e: &gimli::DebuggingInformationEntry<R<'a>>) -> Option<String> {
        let v = e.attr_value(gimli::DW_AT_name).ok()??;
        let s = self.dwarf.attr_string(&self.units[unit], v).ok()?;
        Some(s.to_string_lossy().into_owned())
    }

    /// The DIE an attribute refers to in the same unit.
    fn same_unit_ref(
        e: &gimli::DebuggingInformationEntry<R<'a>>,
        at: gimli::DwAt,
    ) -> Option<UnitOffset> {
        match e.attr_value(at).ok()? {
            Some(AttributeValue::UnitRef(o)) => Some(o),
            _ => None,
        }
    }

    /// The type an entry's DW_AT_type names, in its own unit or another.
    fn type_ref(&self, unit: usize, e: &gimli::DebuggingInformationEntry<R<'a>>) -> Option<TRef> {
        match e.attr_value(gimli::DW_AT_type).ok()? {
            Some(AttributeValue::UnitRef(o)) => Some((unit, o)),
            Some(AttributeValue::DebugInfoRef(g)) => self
                .units
                .iter()
                .enumerate()
                .find_map(|(i, u)| g.to_unit_offset(&u.header).map(|o| (i, o))),
            _ => None,
        }
    }

    fn file_name(&self, unit: usize, idx: u64) -> Option<String> {
        let u = &self.units[unit];
        let lp = u.line_program.as_ref()?;
        let h = lp.header();
        let f = h.file(idx)?;
        let mut path = String::new();
        if let Some(d) = f.directory(h) {
            if let Ok(s) = self.dwarf.attr_string(u, d) {
                path.push_str(&s.to_string_lossy());
            }
        }
        let n = self.dwarf.attr_string(u, f.path_name()).ok()?;
        let n = n.to_string_lossy();
        if n.starts_with('/') || path.is_empty() {
            return Some(n.into_owned());
        }
        if !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(&n);
        Some(path)
    }
}

/// The stack pointer's register number in the plan (DWARF's numbering).
fn sp_reg(arch: &str) -> u8 {
    if arch == "arm64" {
        31
    } else {
        7
    }
}

/// A register's value as a read.
fn reg(r: u8) -> Loc {
    Loc {
        base: Base::Reg(r),
        size: 8,
        ..Loc::default()
    }
}

/// Where a value is.
#[derive(Clone, Debug)]
enum Place {
    /// In memory, at the value of `addr` (a read that gives an address) plus `off`.
    Mem { addr: Loc, off: i64 },
    /// In registers or memory piece by piece: each piece's offset in the value, its size, and
    /// the read that gives it. A part of the value without a piece was optimised out.
    Pieces(Vec<(i64, u64, Loc)>),
}

impl Place {
    /// The part of the value at `off` of `size` bytes.
    fn field(&self, off: u64, size: u64) -> Place {
        let off = off as i64;
        match self {
            Place::Mem { addr, off: o } => Place::Mem {
                addr: addr.clone(),
                off: o + off,
            },
            Place::Pieces(ps) => Place::Pieces(
                ps.iter()
                    .map(|(po, s, l)| (po - off, *s, l.clone()))
                    .filter(|(po, s, _)| *po < size as i64 && po + *s as i64 > 0)
                    .collect(),
            ),
        }
    }

    /// The read of the first `n` bytes of the value.
    fn scalar(&self, n: u8, what: &str) -> Result<Loc, String> {
        match self {
            Place::Mem { addr, off } => {
                let mut path = addr.path.clone();
                path.push(*off);
                Ok(Loc {
                    base: addr.base,
                    path,
                    size: n,
                    ..Loc::default()
                })
            }
            Place::Pieces(ps) => {
                let p = ps
                    .iter()
                    .find(|(po, s, _)| *po <= 0 && po + *s as i64 >= n as i64);
                let Some((po, _, l)) = p else {
                    return Err(format!(
                        "`{what}` is not where the debug information says the value is at the \
                         function's entry (the compiler optimised it out, or keeps it in parts)"
                    ));
                };
                let mut l = l.clone();
                l.size = n;
                if *po < 0 {
                    if l.path.is_empty() {
                        l.shift = (-po * 8) as u8;
                    } else if let Some(last) = l.path.last_mut() {
                        *last -= po;
                    }
                }
                Ok(l)
            }
        }
    }
}

/// A function's frame base at its first instruction, as a register and an offset: the stack
/// pointer (Rust's `DW_OP_reg7`, `DW_OP_reg31`), or the canonical frame address (Go's
/// `DW_OP_call_frame_cfa`), which is the stack pointer on arm64 and the stack pointer plus the
/// return address on x86_64.
fn frame_base(
    d: &Dwarf,
    unit: usize,
    e: &gimli::DebuggingInformationEntry<R<'_>>,
    arch: &str,
    pc: u64,
) -> Result<(u8, i64), String> {
    let u = &d.units[unit];
    let Ok(Some(AttributeValue::Exprloc(x))) = e.attr_value(gimli::DW_AT_frame_base) else {
        return Err("a stack location without the function's frame base".into());
    };
    let mut r = x.0;
    let op = Operation::parse(&mut r, u.encoding()).map_err(|e| e.to_string())?;
    // at the first instruction the canonical frame address is the stack pointer on arm64, and
    // the stack pointer plus the return address on x86_64
    let at_entry: i64 = if arch == "arm64" { 0 } else { 8 };
    match op {
        Operation::Register { register } if r.is_empty() => {
            // a register's frame base (Rust: the stack pointer, or the frame pointer) counts
            // from the register's value after the prologue: the frame address minus the
            // offset the call frame information gives there
            let off = d.cfa_offset(pc, register.0).ok_or(
                "a stack location relative to a register, without call frame information for it",
            )?;
            Ok((sp_reg(arch), at_entry - off))
        }
        Operation::CallFrameCFA if r.is_empty() => Ok((sp_reg(arch), at_entry)),
        _ => Err("a frame base the compiler does not read".into()),
    }
}

/// Where a parameter is at the function's first instruction, from its location expression.
fn entry_place(
    d: &Dwarf,
    unit: usize,
    v: AttributeValue<R>,
    pc: u64,
    arch: &str,
    size: u64,
    fb: &Result<(u8, i64), String>,
) -> Result<Place, String> {
    let u = &d.units[unit];
    let expr = match v {
        AttributeValue::Exprloc(e) => e,
        other => {
            let mut it = d
                .dwarf
                .attr_locations(u, other)
                .map_err(|e| format!("its location list: {e}"))?
                .ok_or("no location")?;
            let mut found = None;
            while let Ok(Some(l)) = it.next() {
                if l.range.begin <= pc && pc < l.range.end {
                    found = Some(l.data);
                    break;
                }
            }
            found.ok_or("no location at the function's entry")?
        }
    };
    let mut r = expr.0;
    let mut ops = Vec::new();
    while !r.is_empty() {
        ops.push(Operation::parse(&mut r, u.encoding()).map_err(|e| e.to_string())?);
    }
    // the general registers only (x86_64: 0 to 15; arm64: x0 to x30 and sp)
    let gpr = |r: u16| if arch == "arm64" { r <= 31 } else { r <= 15 };
    let one =
        |ops: &[Operation<R>]| -> Result<Option<Place>, String> {
            match ops {
                [] => Ok(None),
                [Operation::Register { register }] if gpr(register.0) => Ok(Some(Place::Pieces(
                    vec![(0, size.max(1), reg(register.0 as u8))],
                ))),
                [Operation::Register { .. }] => {
                    Err("a value in a floating-point or vector register".into())
                }
                [Operation::RegisterOffset {
                    register, offset, ..
                }] => Ok(Some(Place::Mem {
                    addr: reg(register.0 as u8),
                    off: *offset,
                })),
                [Operation::FrameOffset { offset }] => {
                    let (r, adj) = fb.clone()?;
                    Ok(Some(Place::Mem {
                        addr: reg(r),
                        off: *offset + adj,
                    }))
                }
                // a constant the compiler folded into the code: not in the machine at the entry
                [.., Operation::StackValue] | [Operation::ImplicitValue { .. }] => Ok(None),
                [Operation::CallFrameCFA] => {
                    Err("an unusual location (the frame address itself)".into())
                }
                _ => Err(format!(
                    "a location the compiler does not read ({} operations)",
                    ops.len()
                )),
            }
        };
    if !ops.iter().any(|o| matches!(o, Operation::Piece { .. })) {
        return one(&ops)?.ok_or_else(|| {
            "a constant the compiler folded into the code: not in the machine at the entry".into()
        });
    }
    // pieces: each run of operations up to a DW_OP_piece is one part of the value
    let mut pieces = Vec::new();
    let mut at: i64 = 0;
    let mut start = 0;
    for (i, o) in ops.iter().enumerate() {
        let Operation::Piece {
            size_in_bits,
            bit_offset,
        } = o
        else {
            continue;
        };
        if bit_offset.is_some() || size_in_bits % 8 != 0 {
            return Err("a value split at bits, not bytes".into());
        }
        let n = size_in_bits / 8;
        match one(&ops[start..i])? {
            None => {}
            Some(Place::Pieces(p)) => {
                for (po, _, l) in p {
                    pieces.push((at + po, n, l));
                }
            }
            Some(p @ Place::Mem { .. }) => {
                // a part of the value in memory: read it as a scalar of the piece's size
                pieces.push((at, n, p.scalar(n.min(8) as u8, "")?));
            }
        }
        at += n as i64;
        start = i + 1;
    }
    Ok(Place::Pieces(pieces))
}
fn is_fn_tag(t: gimli::DwTag) -> bool {
    t == gimli::DW_TAG_subprogram
}

/// Every function of the debug information, with the namespaces of Rust's names.
fn functions(d: &Dwarf, arch: &str) -> Vec<FnInfo> {
    let mut out = Vec::new();
    for (ui, u) in d.units.iter().enumerate() {
        let Ok(mut tree) = u.entries_tree(None) else {
            continue;
        };
        let Ok(root) = tree.root() else { continue };
        let lang = match root.entry().attr_value(gimli::DW_AT_language) {
            Ok(Some(AttributeValue::Language(l))) if l == gimli::DW_LANG_Rust => Lang::Rust,
            Ok(Some(AttributeValue::Language(l))) if l == gimli::DW_LANG_Go => Lang::Go,
            _ => Lang::C,
        };
        walk(d, ui, root, &mut Vec::new(), lang, arch, &mut out);
    }
    out
}

fn walk<'a>(
    d: &Dwarf<'a>,
    ui: usize,
    node: gimli::EntriesTreeNode<'_, '_, '_, R<'a>>,
    ns: &mut Vec<String>,
    lang: Lang,
    arch: &str,
    out: &mut Vec<FnInfo>,
) {
    let e = node.entry().clone();
    let tag = e.tag();
    if is_fn_tag(tag) {
        if let Some(f) = fn_info(d, ui, &e, ns, lang, arch) {
            out.push(f);
        }
        return;
    }
    let pushed = if tag == gimli::DW_TAG_namespace
        || tag == gimli::DW_TAG_structure_type
        || tag == gimli::DW_TAG_class_type
    {
        ns.push(d.name(ui, &e).unwrap_or_default());
        true
    } else {
        false
    };
    let mut ch = node.children();
    while let Ok(Some(c)) = ch.next() {
        walk(d, ui, c, ns, lang, arch, out);
    }
    if pushed {
        ns.pop();
    }
}

/// What a function's debug information says.
struct FnInfo {
    name: String,
    file: String,
    line: u32,
    low_pc: u64,
    /// The end of its code (0: not known).
    high_pc: u64,
    lang: Lang,
    params: Vec<Param>,
    /// Rust's return type (named `result`), or Go's results in order with their names (`~r0`
    /// for one without a name).
    results: Vec<(String, Option<TRef>)>,
    concrete: bool,
    /// The compiler changed how the function is called and returns (`DW_CC_nocall`): its
    /// result is not where its type says.
    nocall: bool,
}

struct Param {
    name: String,
    ty: Option<TRef>,
    /// Where the parameter is at the first instruction, or why it cannot be read there.
    place: Result<Place, String>,
}

fn fn_info<'a>(
    d: &Dwarf<'a>,
    ui: usize,
    e: &gimli::DebuggingInformationEntry<R<'a>>,
    ns: &[String],
    lang: Lang,
    arch: &str,
) -> Option<FnInfo> {
    let u = &d.units[ui];
    let low_pc = match e.attr_value(gimli::DW_AT_low_pc).ok()? {
        Some(v) => d.dwarf.attr_address(u, v).ok()?,
        None => None,
    };
    // the end of its code: an address, or an offset from low_pc
    let high_pc = match e.attr_value(gimli::DW_AT_high_pc) {
        Ok(Some(AttributeValue::Addr(a))) => a,
        Ok(Some(v)) => match (v.udata_value(), low_pc) {
            (Some(n), Some(lo)) => lo + n,
            _ => d.dwarf.attr_address(u, v).ok().flatten().unwrap_or(0),
        },
        _ => 0,
    };
    // the name, file and line may be on the abstract instance or the declaration
    let mut src = e.clone();
    for at in [gimli::DW_AT_abstract_origin, gimli::DW_AT_specification] {
        if let Some(o) = Dwarf::same_unit_ref(&src, at) {
            if let Ok(x) = u.entry(o) {
                src = x;
            }
        }
    }
    let base = d.name(ui, &src).or_else(|| d.name(ui, e))?;
    let name = if lang == Lang::Rust && !ns.is_empty() {
        format!("{}::{base}", ns.join("::"))
    } else {
        base
    };
    let file = match src.attr_value(gimli::DW_AT_decl_file).ok()? {
        Some(AttributeValue::FileIndex(i)) => d.file_name(ui, i).unwrap_or_default(),
        Some(v) => v
            .udata_value()
            .and_then(|i| d.file_name(ui, i))
            .unwrap_or_default(),
        None => String::new(),
    };
    let line = match src.attr_value(gimli::DW_AT_decl_line).ok()? {
        Some(v) => v.udata_value().unwrap_or(0) as u32,
        None => 0,
    };
    let mut params = Vec::new();
    let mut results = Vec::new();
    if lang == Lang::Rust || lang == Lang::C {
        results.push((
            "result".to_string(),
            d.type_ref(ui, &src).or_else(|| d.type_ref(ui, e)),
        ));
    }
    let fb = frame_base(d, ui, e, arch, low_pc.unwrap_or(0));
    if let (Some(pc), Ok(mut tree)) = (low_pc, u.entries_tree(Some(e.offset()))) {
        if let Ok(root) = tree.root() {
            let mut ch = root.children();
            while let Ok(Some(c)) = ch.next() {
                let ce = c.entry();
                if ce.tag() != gimli::DW_TAG_formal_parameter {
                    continue;
                }
                let mut pe = ce.clone();
                if let Some(o) = Dwarf::same_unit_ref(ce, gimli::DW_AT_abstract_origin) {
                    if let Ok(x) = u.entry(o) {
                        pe = x;
                    }
                }
                let pname = d.name(ui, &pe).unwrap_or_default();
                let ty = d.type_ref(ui, &pe).or_else(|| d.type_ref(ui, ce));
                let is_result = matches!(
                    pe.attr_value(gimli::DW_AT_variable_parameter),
                    Ok(Some(AttributeValue::Flag(true)))
                );
                if lang == Lang::Go && is_result {
                    results.push((pname, ty));
                    continue;
                }
                let size = d.size_of(ty);
                let place = match ce.attr_value(gimli::DW_AT_location) {
                    Ok(Some(v)) => entry_place(d, ui, v, pc, arch, size, &fb),
                    _ => Err("no location (optimised out)".into()),
                };
                params.push(Param {
                    name: pname,
                    ty,
                    place,
                });
            }
        }
    }
    Some(FnInfo {
        name,
        file,
        line,
        low_pc: low_pc.unwrap_or(0),
        high_pc,
        lang,
        params,
        results,
        concrete: low_pc.is_some(),
        nocall: matches!(
            e.attr_value(gimli::DW_AT_calling_convention),
            Ok(Some(AttributeValue::CallingConvention(gimli::DW_CC_nocall)))
        ),
    })
}
/// Every source file under the given folders (or the files themselves), with the path relative
/// to its folder.
fn source_files(roots: &[PathBuf]) -> Vec<(PathBuf, String)> {
    fn rec(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut ents: Vec<_> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
        ents.sort();
        for p in ents {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if p.is_dir() {
                if n != "target" && n != "vendor" && !n.starts_with('.') {
                    rec(root, &p, out);
                }
            } else if ["rs", "go", "c", "h", "cc", "cpp"]
                .iter()
                .any(|x| n.ends_with(&format!(".{x}")))
            {
                let rel = p
                    .strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((p.clone(), rel));
            }
        }
    }
    let mut out = Vec::new();
    for r in roots {
        if r.is_file() {
            let n = r
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push((r.clone(), n));
        } else {
            rec(r, r, &mut out);
        }
    }
    out
}

/// Whether a path from the debug information is the source file `rel`.
fn same_file(dwarf_path: &str, rel: &str) -> bool {
    let p = dwarf_path.replace('\\', "/");
    p == rel || p.ends_with(&format!("/{rel}"))
}

fn name_matches(dwarf_name: &str, declared: &str) -> bool {
    dwarf_name == declared
        || dwarf_name.ends_with(&format!("::{declared}"))
        || dwarf_name.ends_with(&format!(".{declared}"))
}

/// Compile the contracts of the sources of one program.
pub fn compile_program(src: &Source, first_id: u32) -> Result<Program, String> {
    let data = std::fs::read(&src.binary).map_err(|e| format!("{}: {e}", src.binary.display()))?;
    let sha256 = sha256_hex(&data);
    let elf = Elf::parse(data).map_err(|e| format!("{}: {e}", src.binary.display()))?;
    let arch = elf.arch()?;
    let mut sec: BTreeMap<&'static str, Vec<u8>> = BTreeMap::new();
    for id in [
        gimli::SectionId::DebugAbbrev,
        gimli::SectionId::DebugAddr,
        gimli::SectionId::DebugInfo,
        gimli::SectionId::DebugLine,
        gimli::SectionId::DebugLineStr,
        gimli::SectionId::DebugLoc,
        gimli::SectionId::DebugLocLists,
        gimli::SectionId::DebugRanges,
        gimli::SectionId::DebugRngLists,
        gimli::SectionId::DebugStr,
        gimli::SectionId::DebugStrOffsets,
    ] {
        sec.insert(id.name(), elf.section(id.name())?);
    }
    if sec[".debug_info"].is_empty() {
        return Err(format!(
            "{}: the program has no debug information; build it with it (Rust: `debug = true` in \
             the release profile; Go keeps it unless `-ldflags=-w`)",
            src.binary.display()
        ));
    }
    let sections = gimli::DwarfSections::load(|id| -> Result<&[u8], ()> {
        Ok(sec.get(id.name()).map(Vec::as_slice).unwrap_or(&[]))
    })
    .expect("loading sections from memory");
    let dwarf = sections.borrow(|s| EndianSlice::new(s, LittleEndian));
    let mut units = Vec::new();
    let mut it = dwarf.units();
    while let Ok(Some(h)) = it.next() {
        if let Ok(u) = dwarf.unit(h) {
            units.push(u);
        }
    }
    let addr_of = |n: &str| {
        elf.sections
            .iter()
            .find(|s| s.name == n)
            .map_or(0, |s| s.addr)
    };
    let mut d = Dwarf {
        dwarf,
        units,
        eh: elf.section(".eh_frame")?,
        eh_addr: addr_of(".eh_frame"),
        text_addr: addr_of(".text"),
        go_types: Vec::new(),
    };
    // Go's type descriptors: `DW_AT_go_runtime_type` is an offset in the section that holds them
    // (`.go.type` since Go 1.26; `.rodata` before)
    let types_base = match addr_of(".go.type") {
        0 => addr_of(".rodata"),
        a => a,
    };
    d.find_go_types(types_base);
    let d = d;
    let fns = functions(&d, arch);
    // ET_DYN: a position-independent program, loaded where the guest's kernel chooses
    let pie = rd16(&elf.data, 16) == Some(3);
    let mut prog = Program {
        program: src.program.clone(),
        sha256,
        arch: arch.into(),
        pie,
        head: if pie {
            elf.data[..64.min(elf.data.len())]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        } else {
            String::new()
        },
        functions: Vec::new(),
        unchecked: Vec::new(),
        calls: Vec::new(),
        keeps: Vec::new(),
    };
    let mut lang_errors = Vec::new();
    // the `keep` lines, with their file and the function they stand next to (Milestone 291)
    let mut keeps: Vec<(lang::SrcKeep, String, String)> = Vec::new();
    let mut id = first_id;
    for (path, rel) in source_files(&src.sources) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (contracts, errs) = lang::scan(&text);
        for e in errs {
            lang_errors.push(format!("{}:{}: {}", path.display(), e.line, e.message));
        }
        for c in contracts {
            for k in &c.keeps {
                keeps.push((k.clone(), rel.clone(), c.function.clone()));
            }
            // a function with only `keep` lines records nothing and needs no breakpoint
            if !c.keeps.is_empty()
                && c.clauses.is_empty()
                && c.records.is_empty()
                && c.inputs.is_empty()
            {
                continue;
            }
            let cands: Vec<&FnInfo> = fns
                .iter()
                .filter(|f| {
                    f.line == c.line
                        && same_file(&f.file, &rel)
                        && name_matches(&f.name, &c.function)
                })
                .collect();
            let unchecked = |reason: String| Unchecked {
                function: c.function.clone(),
                file: rel.clone(),
                line: c.line,
                reason,
            };
            let Some(f) = cands.iter().find(|f| f.concrete) else {
                prog.unchecked.push(unchecked(if cands.is_empty() {
                    "not in the program's debug information: is the program built from these \
                     sources, with debug information?"
                        .into()
                } else {
                    "the compiler copied it into its callers and kept no copy of its own; the \
                     first version stops only at a function of its own (Rust \
                     `#[inline(never)]`, Go `//go:noinline`)"
                        .into()
                }));
                continue;
            };
            match function_plan(&d, &elf, f, &c, &rel, id, arch) {
                Ok(func) => {
                    prog.functions.push(func);
                    id += 1;
                }
                Err(e) => prog.unchecked.push(unchecked(e)),
            }
        }
    }
    if !lang_errors.is_empty() {
        return Err(lang_errors.join("\n"));
    }
    prog.calls = call_sites(&elf, arch, &fns, &prog.functions);
    for (k, file, function) in keeps {
        match keep_plan(&prog.functions, &k, &file) {
            Ok(p) => prog.keeps.push(p),
            Err(reason) => prog.unchecked.push(Unchecked {
                function,
                file,
                line: k.line,
                reason,
            }),
        }
    }
    Ok(prog)
}

/// The recording function a `keep` line names: its full name in the debug information equals
/// the name, or ends with it after `/`, `.` or `::` (`blnk.setTransactionMetadata` names
/// `github.com/blnkfinance/blnk.setTransactionMetadata`).
pub fn keep_function<'a>(
    funcs: &'a [Function],
    given: &str,
    name: &str,
) -> Result<&'a str, String> {
    let hits: Vec<&Function> = funcs
        .iter()
        .filter(|f| !f.values.is_empty())
        .filter(|f| name_matches(&f.name, given) || f.name.ends_with(&format!("/{given}")))
        .collect();
    match hits.as_slice() {
        [f] if f.values.iter().any(|v| !v.key && v.name == name) => Ok(&f.name),
        [f] => Err(format!(
            "{} records no value named `{name}` (a `record` line's `as` name, else its \
             expression)",
            f.name
        )),
        [] => Err(format!(
            "no function with `record` lines in the plan is named `{given}` (one the compiler \
             copied into its callers has no stop of its own)"
        )),
        more => Err(format!(
            "`{given}` names {} functions with `record` lines; write more of the name ({})",
            more.len(),
            more.iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A `keep` line's property for the plan (Milestone 291): a function of the program records its
/// value, and the functions it names record it too.
pub fn keep_plan(funcs: &[Function], k: &lang::SrcKeep, file: &str) -> Result<Keep, String> {
    if !funcs
        .iter()
        .flat_map(|f| &f.values)
        .any(|v| !v.key && v.name == k.name)
    {
        return Err(format!(
            "`{}`: no `record` line of the program records a value named `{}`",
            k.text, k.name
        ));
    }
    let resolve = |g: &Option<String>| -> Result<Option<String>, String> {
        match g {
            None => Ok(None),
            Some(g) => keep_function(funcs, g, &k.name)
                .map(|n| Some(n.to_string()))
                .map_err(|e| format!("`{}`: {e}", k.text)),
        }
    };
    Ok(Keep {
        text: k.text.clone(),
        file: file.to_string(),
        line: k.line,
        name: k.name.clone(),
        once_set: k.once_set,
        from: resolve(&k.from)?,
        to: resolve(&k.to)?,
        across_boots: k.across_boots,
    })
}

/// The direct calls of the functions that record values (Milestone 285), found in the program's
/// code: on arm64 every `BL` whose target is the function's entry; on x86_64 every `CALL rel32`
/// (`E8`) whose target is the entry, at an offset inside a function of the debug information
/// (a byte scan, so a call is kept only when it lies in a function's code). The caller is the
/// function whose code holds the call. A call through a register (an interface's method, a
/// function value) is not found; its record names the return address only.
fn call_sites(elf: &Elf, arch: &str, fns: &[FnInfo], funcs: &[Function]) -> Vec<CallSite> {
    let wanted: Vec<(u64, u32)> = funcs
        .iter()
        .filter(|f| !f.values.is_empty())
        .map(|f| (f.entry, f.id))
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut ranges: Vec<(u64, u64, &str)> = fns
        .iter()
        .filter(|f| f.concrete && f.high_pc > f.low_pc)
        .map(|f| (f.low_pc, f.high_pc, f.name.as_str()))
        .collect();
    ranges.sort();
    let caller = |pc: u64| -> Option<&str> {
        let i = ranges.partition_point(|r| r.0 <= pc);
        let r = ranges.get(i.checked_sub(1)?)?;
        (pc < r.1).then_some(r.2)
    };
    let mut out = Vec::new();
    for s in elf
        .sections
        .iter()
        .filter(|s| s.flags & 4 != 0 && !s.nobits)
    {
        let Some(code) = elf
            .data
            .get(s.offset as usize..(s.offset + s.size) as usize)
        else {
            continue;
        };
        if arch == "arm64" {
            for (k, w) in code.chunks_exact(4).enumerate() {
                let ins = u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
                if ins & 0xfc00_0000 != 0x9400_0000 {
                    continue;
                }
                let pc = s.addr + 4 * k as u64;
                let imm = (((ins & 0x03ff_ffff) << 6) as i32 >> 6) as i64 * 4;
                let target = pc.wrapping_add(imm as u64);
                for &(_, id) in wanted.iter().filter(|w| w.0 == target) {
                    if let Some(c) = caller(pc) {
                        out.push(CallSite {
                            callee: id,
                            ret: pc + 4,
                            caller: c.to_string(),
                        });
                    }
                }
            }
        } else {
            for k in 0..code.len().saturating_sub(4) {
                if code[k] != 0xe8 {
                    continue;
                }
                let rel = i32::from_le_bytes([code[k + 1], code[k + 2], code[k + 3], code[k + 4]]);
                let pc = s.addr + k as u64;
                let target = (pc + 5).wrapping_add(rel as i64 as u64);
                for &(_, id) in wanted.iter().filter(|w| w.0 == target) {
                    if let Some(c) = caller(pc) {
                        out.push(CallSite {
                            callee: id,
                            ret: pc + 5,
                            caller: c.to_string(),
                        });
                    }
                }
            }
        }
    }
    out.sort_by_key(|c| (c.callee, c.ret));
    out
}

/// Compile every program's contracts into one plan.
pub fn compile(sources: &[Source]) -> Result<Plan, String> {
    let mut plan = Plan {
        v: PLAN_V,
        kind: PLAN_TYPE.into(),
        programs: Vec::new(),
    };
    let mut id = 0;
    for s in sources {
        let p = compile_program(s, id)?;
        id += p.functions.len() as u32;
        plan.programs.push(p);
    }
    Ok(plan)
}

/// A value an expression names, on its way to a read.
#[derive(Clone, Debug)]
struct Pv {
    place: Place,
    ty: Option<TRef>,
    /// The reads happen at the return (else at the entry).
    ret: bool,
    /// A parameter in an `ensures` clause, outside `old()`, that points at something (a
    /// reference, a pointer, a borrowed string or slice): what it points to is read at return,
    /// through the pointer as it was at entry.
    lazy: bool,
    /// What the value needs to exist (its variant, an index inside the length), with the reason
    /// a call is undecided without it.
    guards: Vec<(Expr, String)>,
    /// How the contract writes it (the report's names).
    text: String,
}

/// The reads of one function, collected while its clauses compile.
struct Reads<'a> {
    d: &'a Dwarf<'a>,
    f: &'a FnInfo,
    arch: &'a str,
    at_entry: Vec<Loc>,
    at_return: Vec<Loc>,
}

/// The first index in a chain of fields and elements that is not a number (`k` in `v[k].x`).
fn dyn_index(a: &Ast) -> Option<&Ast> {
    match a {
        Ast::Field(x, _) | Ast::Call(_, x) => dyn_index(x),
        Ast::Index(x, i) => dyn_index(x).or(match **i {
            Ast::Int(_) | Ast::Str(_) => None,
            _ => Some(&**i),
        }),
        _ => None,
    }
}

/// `a` with the index `target` replaced by the number `j`.
fn subst(a: &Ast, target: &Ast, j: i128) -> Ast {
    match a {
        Ast::Field(x, f) => Ast::Field(Box::new(subst(x, target, j)), f.clone()),
        Ast::Call(b, x) => Ast::Call(*b, Box::new(subst(x, target, j))),
        Ast::Index(x, i) if std::ptr::eq(&**i, target) => {
            Ast::Index(Box::new(subst(x, target, j)), Box::new(Ast::Int(j)))
        }
        Ast::Index(x, i) => Ast::Index(Box::new(subst(x, target, j)), i.clone()),
        other => other.clone(),
    }
}

impl Reads<'_> {
    fn add(list: &mut Vec<Loc>, l: Loc) -> u32 {
        if let Some(i) = list.iter().position(|x| {
            x.base == l.base && x.path == l.path && x.size == l.size && x.shift == l.shift
        }) {
            return i as u32;
        }
        list.push(l);
        (list.len() - 1) as u32
    }

    /// A read of the value, wrapped in what it needs.
    fn read(&mut self, pv: &Pv, mut loc: Loc, ty: Ty) -> Expr {
        loc.name = pv.text.clone();
        let e = if pv.ret {
            Expr::Out(Self::add(&mut self.at_return, loc), ty)
        } else {
            Expr::In(Self::add(&mut self.at_entry, loc), ty)
        };
        Self::guarded(pv, e)
    }

    fn guarded(pv: &Pv, e: Expr) -> Expr {
        pv.guards.iter().rev().fold(e, |e, (c, why)| Expr::Guard {
            cond: Box::new(c.clone()),
            then: Box::new(e),
            why: why.clone(),
        })
    }

    fn param(&self, name: &str) -> Result<&Param, String> {
        self.f
            .params
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| {
                let names: Vec<&str> = self.f.params.iter().map(|p| p.name.as_str()).collect();
                format!(
                    "`{name}` is not a parameter of {} (its parameters: {}; the result is `result`)",
                    self.f.name,
                    names.join(", ")
                )
            })
    }

    /// Whether a value points at something a parameter's `ensures` reads at return.
    fn points(&self, ty: Option<TRef>) -> bool {
        match self.d.ty(ty) {
            Type::Pointer(_) => true,
            Type::Struct(s) => self.d.seq(&s).is_some_and(|q| q.borrowed),
            _ => false,
        }
    }

    /// The result with this name: `result` (the first), or a Go result's own name.
    fn result_index(&self, n: &str) -> Option<usize> {
        if n == "result" {
            return (!self.f.results.is_empty()).then_some(0);
        }
        if self.f.lang == Lang::Go && !n.starts_with('~') {
            return self.f.results.iter().position(|r| r.0 == n);
        }
        None
    }

    /// A parameter or a result.
    fn name(&mut self, n: &str, kind: ClauseKind, old: bool) -> Result<Pv, String> {
        if let Some(i) = self.result_index(n) {
            if kind == ClauseKind::Requires || old {
                return Err(format!("`{n}` is a result: it has no value at entry"));
            }
            return self.result(i, n);
        }
        let p = self.param(n)?;
        let place = p
            .place
            .clone()
            .map_err(|e| format!("`{n}` cannot be read at entry: {e}"))?;
        let ty = p.ty;
        Ok(Pv {
            place,
            ty,
            ret: false,
            lazy: kind == ClauseKind::Ensures && !old && self.points(ty),
            guards: Vec::new(),
            text: n.to_string(),
        })
    }

    /// The `i`-th result at return.
    fn result(&mut self, i: usize, text: &str) -> Result<Pv, String> {
        let fname = &self.f.name;
        if self.f.nocall {
            return Err(format!(
                "the compiler changed how {fname} returns (it knew something of every call, for \
                 example that the result is always `Some`), so `{text}` is not where its type \
                 says"
            ));
        }
        let ty = self.f.results.get(i).and_then(|r| r.1);
        if ty.is_none() {
            return Err(format!("{fname} returns nothing"));
        }
        let arm = self.arch == "arm64";
        let place = if self.f.lang == Lang::Go {
            // Go's calling convention: the results' words in the integer registers in order
            // (amd64: RAX, RBX, RCX, RDI, RSI, R8 to R11; arm64: R0 to R15)
            let regs: Vec<u8> = if arm {
                (0..16).collect()
            } else {
                vec![0, 3, 2, 5, 4, 8, 9, 10, 11]
            };
            let mut next = 0;
            let mut place = None;
            for (j, (_, t)) in self.f.results.iter().enumerate() {
                let mut w = Vec::new();
                let r = self.d.go_words(*t, 0, &mut w, 0);
                if j == i {
                    r.map_err(|e| format!("`{text}` is {e}"))?;
                    if next + w.len() > regs.len() {
                        return Err(format!(
                            "`{text}` is returned on the stack (the results need more than \
                             {} registers)",
                            regs.len()
                        ));
                    }
                    place = Some(Place::Pieces(
                        w.iter()
                            .enumerate()
                            .map(|(k, (o, s))| (*o as i64, *s, reg(regs[next + k])))
                            .collect(),
                    ));
                    break;
                }
                if r.is_ok() && next + w.len() <= regs.len() {
                    next += w.len();
                }
            }
            place.ok_or_else(|| format!("{fname} has no result {i}"))?
        } else {
            // Rust's calling convention: one scalar in the first return register, two scalars
            // in the first two, another value of up to 8 bytes as the first register's bytes,
            // and a larger one in memory the caller gives (its address in x8 on arm64, rdi on
            // x86_64, at entry)
            let mut sl = Vec::new();
            self.d
                .slots(ty, 0, &mut sl, 0)
                .map_err(|e| format!("`{text}` is {e}"))?;
            sl.sort_unstable();
            sl.dedup();
            let size = self.d.size_of(ty);
            if sl.is_empty() || size == 0 {
                return Err(format!("{fname} returns nothing"));
            }
            if sl.len() == 1 {
                Place::Pieces(vec![(sl[0].0 as i64, sl[0].1, reg(0))])
            } else if sl.len() == 2 && sl[0].0 + sl[0].1 <= sl[1].0 && sl[1].1 <= 8 {
                Place::Pieces(vec![
                    (sl[0].0 as i64, sl[0].1, reg(0)),
                    (sl[1].0 as i64, sl[1].1, reg(1)),
                ])
            } else if size <= 8 {
                Place::Pieces(vec![(0, size, reg(0))])
            } else if self.f.lang == Lang::C {
                return Err(format!(
                    "`{text}` is a structure a C function returns: the compiler reads it only \
                     through a pointer"
                ));
            } else {
                // the caller's memory: x86_64 passes its address in rdi; on arm64 Rust 1.90
                // passes it in x0 as a first argument (the parameters then start at x1), and
                // the C convention in x8
                let uses_x0 = self.f.params.iter().any(|p| match &p.place {
                    Ok(Place::Pieces(ps)) => ps.iter().any(|(_, _, l)| l.base == Base::Reg(0)),
                    Ok(Place::Mem { addr, .. }) => addr.base == Base::Reg(0),
                    Err(_) => false,
                });
                let sret = match (arm, uses_x0) {
                    (false, _) => 5,
                    (true, false) => 0,
                    (true, true) => 8,
                };
                let k = Self::add(
                    &mut self.at_entry,
                    Loc {
                        name: format!("address of {text}"),
                        ..reg(sret)
                    },
                );
                Place::Mem {
                    addr: Loc {
                        base: Base::In(k),
                        size: 8,
                        ..Loc::default()
                    },
                    off: 0,
                }
            }
        };
        Ok(Pv {
            place,
            ty,
            ret: true,
            lazy: false,
            guards: Vec::new(),
            text: text.to_string(),
        })
    }

    /// What a pointer value points to.
    fn deref(&mut self, pv: Pv) -> Result<Pv, String> {
        let target = match self.d.ty(pv.ty) {
            Type::Pointer(t) => t,
            _ => None,
        };
        let loc = pv.place.scalar(8, &pv.text)?;
        if pv.lazy && !pv.ret {
            // the pointer as it was at entry; what it points to, at return
            let k = Self::add(
                &mut self.at_entry,
                Loc {
                    name: pv.text.clone(),
                    ..loc
                },
            );
            return Ok(Pv {
                place: Place::Mem {
                    addr: Loc {
                        base: Base::In(k),
                        size: 8,
                        ..Loc::default()
                    },
                    off: 0,
                },
                ty: target,
                ret: true,
                lazy: false,
                ..pv
            });
        }
        Ok(Pv {
            place: Place::Mem { addr: loc, off: 0 },
            ty: target,
            lazy: false,
            ..pv
        })
    }

    /// Through pointers (references, Go pointers) to the value they point to.
    fn auto_deref(&mut self, mut pv: Pv) -> Result<Pv, String> {
        for _ in 0..4 {
            if !matches!(self.d.ty(pv.ty), Type::Pointer(_)) {
                return Ok(pv);
            }
            pv = self.deref(pv)?;
        }
        Ok(pv)
    }

    fn structure(&mut self, pv: Pv) -> Result<(Pv, Struct), String> {
        let pv = self.auto_deref(pv)?;
        match self.d.ty(pv.ty) {
            Type::Struct(s) => Ok((pv, s)),
            _ => Err(format!("`{}` is not a structure", pv.text)),
        }
    }

    fn field(&mut self, pv: Pv, name: &str) -> Result<Pv, String> {
        let (pv, s) = self.structure(pv)?;
        let want = if self.f.lang == Lang::Rust && name.chars().all(|c| c.is_ascii_digit()) {
            format!("__{name}")
        } else {
            name.to_string()
        };
        let Some(m) = s.member(&want) else {
            let names: Vec<&str> = s.members.iter().map(|m| m.name.as_str()).collect();
            return Err(format!(
                "`{}` has no field `{name}` (its fields: {})",
                s.name,
                names.join(", ")
            ));
        };
        Ok(Pv {
            place: pv.place.field(m.off, self.d.size_of(m.ty)),
            ty: m.ty,
            lazy: false,
            text: format!("{}.{name}", pv.text),
            ..pv
        })
    }

    /// A string, a vector or a slice, and where its parts are.
    fn seq(&mut self, pv: Pv) -> Result<(Pv, Seq), String> {
        let lazy = pv.lazy;
        let (mut pv, s) = self
            .structure(pv)
            .map_err(|_| "not a string, a vector or a slice".to_string())?;
        let q = self.d.seq(&s).ok_or_else(|| {
            format!(
                "`{}` is not a string, a vector or a slice (it is `{}`)",
                pv.text, s.name
            )
        })?;
        // a borrowed string or slice parameter: its elements at return
        pv.lazy = lazy && q.borrowed && !pv.ret;
        Ok((pv, q))
    }

    fn len(&mut self, pv: Pv) -> Result<Expr, String> {
        let text = format!("len({})", pv.text);
        let (pv, q) = self.seq(pv)?;
        let len_ty = match &self.d.ty(pv.ty) {
            Type::Struct(s) => s
                .members
                .iter()
                .find(|m| m.off == q.len_off)
                .and_then(|m| m.ty),
            _ => None,
        };
        let n = Pv {
            place: pv.place.field(q.len_off, 8),
            ty: len_ty,
            text,
            lazy: false,
            ..pv
        };
        match self.d.ty(len_ty) {
            Type::Scalar(t, w) => {
                let l = n.place.scalar(w, &n.text)?;
                Ok(self.read(&n, l, t))
            }
            // a Vec's length is a plain usize; String's is through its Vec
            _ => {
                let l = n.place.scalar(8, &n.text)?;
                Ok(self.read(&n, l, Ty::U(8)))
            }
        }
    }

    /// Element `j` of a string, a vector or a slice (undecided past its length).
    fn elem(&mut self, pv: Pv, j: u64) -> Result<Pv, String> {
        let text = format!("{}[{j}]", pv.text);
        let len = self.len(pv.clone())?;
        let (pv, q) = self.seq(pv)?;
        let esize = self.d.size_of(q.elem);
        if esize == 0 {
            return Err(format!(
                "`{}`: elements of a size the compiler does not know",
                pv.text
            ));
        }
        let ptr = Pv {
            place: pv.place.field(q.ptr_off, 8),
            ty: None,
            ..pv.clone()
        };
        let mut e = self.deref(ptr)?;
        e.place = e.place.field(j * esize, esize);
        e.ty = q.elem;
        e.guards.push((
            Expr::Bin(BinOp::Lt, Box::new(Expr::Int(j.to_string())), Box::new(len)),
            format!("index {j} is past the length of `{}`", pv.text),
        ));
        e.text = text;
        e.lazy = false;
        Ok(e)
    }

    /// Whether an `Option` or `Result` is the variant `want`.
    fn is_variant(&mut self, pv: Pv, want: &str) -> Result<(Pv, Struct, Expr), String> {
        let (pv, s) = self.structure(pv)?;
        let Some(v) = &s.variants else {
            return Err(format!(
                "`{}` is not an Option or a Result (it is `{}`)",
                pv.text, s.name
            ));
        };
        let Some(var) = v.list.iter().find(|x| x.name == want) else {
            let names: Vec<&str> = v.list.iter().map(|x| x.name.as_str()).collect();
            return Err(format!(
                "`{}` has no variant {want} (its variants: {})",
                pv.text,
                names.join(", ")
            ));
        };
        let Some((doff, dty)) = v.discr else {
            return Ok((pv.clone(), s.clone(), Expr::Bool(true)));
        };
        let n = self.d.size_of(dty).clamp(1, 8) as u8;
        let m = if n >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * n)) - 1
        };
        let d = Pv {
            place: pv.place.field(doff, n as u64),
            text: format!("the variant of {}", pv.text),
            ..pv.clone()
        };
        let l = d.place.scalar(n, &d.text)?;
        let discr = Box::new(self.read(&d, l, Ty::U(n)));
        let e = match var.value {
            Some(x) => Expr::Is {
                discr,
                any: vec![(x & m).to_string()],
                not: Vec::new(),
            },
            None => Expr::Is {
                discr,
                any: Vec::new(),
                not: v
                    .list
                    .iter()
                    .filter_map(|o| o.value.map(|x| (x & m).to_string()))
                    .collect(),
            },
        };
        Ok((pv, s, e))
    }

    /// The value inside a variant: `some(o)`, `ok(r)`, `err(r)`.
    fn payload(&mut self, pv: Pv, b: Builtin, want: &str) -> Result<Pv, String> {
        let text = format!("{}({})", b.name(), pv.text);
        let (pv, s, cond) = self.is_variant(pv, want)?;
        let var = s
            .variants
            .as_ref()
            .and_then(|v| v.list.iter().find(|x| x.name == want))
            .cloned()
            .ok_or("no variant")?;
        let Type::Struct(vs) = self.d.ty(var.ty) else {
            return Err(format!("`{}`'s variant {want} has no value", pv.text));
        };
        let Some(m) = vs.member("__0") else {
            return Err(format!("`{}`'s variant {want} has no value", pv.text));
        };
        let mut guards = pv.guards.clone();
        guards.push((cond, format!("`{}` is not {want}", pv.text)));
        Ok(Pv {
            place: pv.place.field(var.off + m.off, self.d.size_of(m.ty)),
            ty: m.ty,
            lazy: false,
            guards,
            text,
            ..pv
        })
    }

    /// The value a name, a field, an element or a variant's value names.
    fn place(&mut self, a: &Ast, kind: ClauseKind, old: bool) -> Result<Pv, String> {
        match a {
            Ast::Name(n) => self.name(n, kind, old),
            Ast::Field(x, f) => {
                // Go's results after the first: result.1, result.2, ...
                if let (Ast::Name(r), Ok(i)) = (&**x, f.parse::<usize>()) {
                    if r == "result" && self.f.lang == Lang::Go && self.f.results.len() > 1 {
                        if kind == ClauseKind::Requires || old {
                            return Err("`result` has no value at entry".into());
                        }
                        if i >= self.f.results.len() {
                            return Err(format!(
                                "{} has {} results (result.0 to result.{})",
                                self.f.name,
                                self.f.results.len(),
                                self.f.results.len() - 1
                            ));
                        }
                        return self.result(i, &format!("result.{i}"));
                    }
                }
                let pv = self.place(x, kind, old)?;
                self.field(pv, f)
            }
            Ast::Index(x, i) => match **i {
                Ast::Int(j) if j >= 0 => {
                    let pv = self.place(x, kind, old)?;
                    self.elem(pv, j as u64)
                }
                _ => Err(format!("`{a}`: an index is a number of zero or more")),
            },
            Ast::Call(b @ (Builtin::Some | Builtin::Ok | Builtin::Err), x) => {
                let want = match b {
                    Builtin::Some => "Some",
                    Builtin::Ok => "Ok",
                    _ => "Err",
                };
                let pv = self.place(x, kind, old)?;
                self.payload(pv, *b, want)
            }
            Ast::Old(x) => self.place(x, kind, true),
            _ => Err(format!("`{a}` is not a value with fields or elements")),
        }
    }

    /// The scalar a value is: an integer, a boolean, a pointer, or a Go interface (nil or not).
    fn scalar(&mut self, pv: Pv) -> Result<Expr, String> {
        match self.d.ty(pv.ty) {
            Type::Scalar(t, n) => {
                let l = pv.place.scalar(n, &pv.text)?;
                Ok(self.read(&pv, l, t))
            }
            Type::Pointer(_) => {
                let l = pv.place.scalar(8, &pv.text)?;
                Ok(self.read(&pv, l, Ty::Ptr))
            }
            Type::Struct(s) if s.iface() => {
                // nil or not: the interface's type word
                let l = pv.place.scalar(8, &pv.text)?;
                Ok(self.read(&pv, l, Ty::Ptr))
            }
            Type::Struct(s) if self.d.seq(&s).is_some() => Err(format!(
                "`{}` is a string, a vector or a slice: use len({0}), {0}[i], or compare it \
                 with a string literal",
                pv.text
            )),
            Type::Struct(s) if s.variants.is_some() => Err(format!(
                "`{}` is an enumeration ({}): use is_some, is_ok, is_err, some, ok or err",
                pv.text, s.name
            )),
            Type::Struct(s) => Err(format!(
                "`{}` is a structure ({}); compare its fields one by one",
                pv.text, s.name
            )),
            // its IEEE 754 bits; the host compares it as a floating-point number (Milestone 285)
            Type::Float(n @ (4 | 8)) => {
                let l = pv.place.scalar(n, &pv.text)?;
                Ok(self.read(&pv, l, Ty::F(n)))
            }
            Type::Float(n) => Err(format!(
                "`{}` is a floating-point number of {n} bytes; contracts read those of 4 and 8",
                pv.text
            )),
            Type::Array(..) => Err(format!(
                "`{}` is an array; read its elements one by one",
                pv.text
            )),
            Type::Other(n) => Err(format!(
                "`{}` has a type the compiler does not read ({n})",
                pv.text
            )),
        }
    }

    /// `x == "literal"`.
    fn str_eq(&mut self, x: &Ast, lit: &str, kind: ClauseKind, old: bool) -> Result<Expr, String> {
        if dyn_index(x).is_some() {
            return Err(format!(
                "`{x}`: a string chosen by a computed index is not compared with a literal"
            ));
        }
        if lit.len() > STR_BYTES {
            return Err(format!(
                "a string literal of more than {STR_BYTES} bytes (the engine reads at most \
                 {STR_BYTES} bytes of a string)"
            ));
        }
        let pv = self.place(x, kind, old)?;
        self.str_eq_pv(pv, lit)
    }

    /// A string value's length and its first `n` bytes, in reads of up to 8 bytes.
    fn text_reads(&mut self, pv: Pv, n: usize) -> Result<(Expr, Vec<Expr>), String> {
        let len = self.len(pv.clone())?;
        let (pv, q) = self.seq(pv)?;
        if !q.text {
            return Err(format!("`{}` is not a string", pv.text));
        }
        let mut chunks = Vec::new();
        let mut at = 0;
        while at < n {
            let k = (n - at).min(8);
            let ptr = Pv {
                place: pv.place.field(q.ptr_off, 8),
                ty: None,
                ..pv.clone()
            };
            let mut e = self.deref(ptr)?;
            e.place = e.place.field(at as u64, k as u64);
            e.text = format!("{}[{at}..{}]", pv.text, at + k);
            let l = e.place.scalar(k as u8, &e.text)?;
            chunks.push(self.read(&e, l, Ty::Bytes(k as u8)));
            at += k;
        }
        Ok((len, chunks))
    }

    /// Whether a string value equals a literal.
    fn str_eq_pv(&mut self, pv: Pv, lit: &str) -> Result<Expr, String> {
        let (len, chunks) = self.text_reads(pv, lit.len())?;
        Ok(Expr::StrEq {
            len: Box::new(len),
            chunks,
            lit: lit.to_string(),
        })
    }

    fn compile(&mut self, a: &Ast, kind: ClauseKind, old: bool) -> Result<Expr, String> {
        if let Some(i) = dyn_index(a) {
            // an index the call computes: every element up to the bound, and the index
            let idx = self.compile(i, kind, old)?;
            let mut elems = Vec::new();
            for j in 0..ELEMS as i128 {
                elems.push(self.compile(&subst(a, i, j), kind, old)?);
            }
            return Ok(Expr::Elem {
                idx: Box::new(idx),
                first: 0,
                elems,
            });
        }
        Ok(match a {
            Ast::Int(v) => Expr::Int(v.to_string()),
            Ast::Float(v) => Expr::Float(format!("{v:?}")),
            Ast::Bool(b) => Expr::Bool(*b),
            Ast::Str(s) => {
                return Err(format!(
                    "the string {s:?} is compared with a string: `s == {s:?}`"
                ))
            }
            Ast::Not(x) => Expr::Not(Box::new(self.compile(x, kind, old)?)),
            Ast::Neg(x) => Expr::Neg(Box::new(self.compile(x, kind, old)?)),
            Ast::Old(x) => self.compile(x, kind, true)?,
            Ast::Bin(op @ (BinOp::Eq | BinOp::Ne), x, y)
                if matches!(**x, Ast::Str(_)) || matches!(**y, Ast::Str(_)) =>
            {
                let (v, lit) = match (&**x, &**y) {
                    (Ast::Str(l), v) | (v, Ast::Str(l)) => (v, l),
                    _ => unreachable!("one side is a string"),
                };
                if matches!(v, Ast::Str(_)) {
                    return Err("two string literals compared".into());
                }
                let (v, old) = match v {
                    Ast::Old(inner) => (&**inner, true),
                    v => (v, old),
                };
                let e = self.str_eq(v, lit, kind, old)?;
                if *op == BinOp::Ne {
                    Expr::Not(Box::new(e))
                } else {
                    e
                }
            }
            Ast::Bin(op, x, y) => Expr::Bin(
                *op,
                Box::new(self.compile(x, kind, old)?),
                Box::new(self.compile(y, kind, old)?),
            ),
            Ast::Call(Builtin::Sorted, _) | Ast::Matches(..) => {
                return Err(format!(
                    "`{a}`: `matches()` and `sorted()` are read by `hydo fuzz` only; a run reads \
                     `is_some`, `is_ok` and the other functions"
                ))
            }
            Ast::Call(Builtin::Len, x) => {
                let pv = self.place(x, kind, old)?;
                self.len(pv)?
            }
            Ast::Call(
                b @ (Builtin::IsSome | Builtin::IsNone | Builtin::IsOk | Builtin::IsErr),
                x,
            ) => {
                let want = match b {
                    Builtin::IsSome => "Some",
                    Builtin::IsNone => "None",
                    Builtin::IsOk => "Ok",
                    _ => "Err",
                };
                let pv = self.place(x, kind, old)?;
                let (pv, _, e) = self.is_variant(pv, want)?;
                Self::guarded(&pv, e)
            }
            Ast::Name(_) | Ast::Field(..) | Ast::Index(..) | Ast::Call(..) => {
                let pv = self.place(a, kind, old)?;
                self.scalar(pv)?
            }
        })
    }
}

/// How many bytes of a recorded string are read (the history shows longer ones cut).
pub const TEXT_BYTES: usize = 32;

/// Where a Go map's parts are (Milestone 285), from the types Go's linker writes for it:
/// `map<K,V>` (`dirPtr`, `dirLen`), `table<K,V>`, `groupReference<K,V>` and the group type,
/// whose slots are an array of `{key, elem}` or, with Go's split groups, arrays `keys` and
/// `elems`.
struct GoMap {
    dir_ptr: u64,
    dir_len: u64,
    ctrl: u64,
    slots: u64,
    key_ty: Option<TRef>,
    elem_ty: Option<TRef>,
    /// Slot i's key is at `key.0 + i * key.1` in the group, its value at `elem.0 + i * elem.1`.
    key: (u64, u64),
    elem: (u64, u64),
}

impl Reads<'_> {
    /// A recorded value (Milestone 285): read at the call's entry, or at its return (`ret`),
    /// where a field read through a pointer parameter is the one at return.
    fn show(&mut self, a: &Ast, ret: bool) -> Result<Show, String> {
        let kind = if ret {
            ClauseKind::Ensures
        } else {
            ClauseKind::Requires
        };
        if dyn_index(a).is_some() {
            return Err(format!(
                "`{a}`: a recorded value names its elements with numbers (`v[0]`)"
            ));
        }
        match a {
            Ast::Index(x, k) => {
                let pv = self.place(x, kind, false)?;
                if let Some(m) = self.go_map(&pv) {
                    return self.lookup(pv, m, k);
                }
                if matches!(**k, Ast::Str(_)) {
                    return Err(format!(
                        "`{x}` is not a Go map: a value is read by a string key from a map only"
                    ));
                }
                let pv = self.place(a, kind, false)?;
                self.show_pv(pv)
            }
            Ast::Name(_)
            | Ast::Field(..)
            | Ast::Call(Builtin::Some | Builtin::Ok | Builtin::Err, _) => {
                let pv = self.place(a, kind, false)?;
                self.show_pv(pv)
            }
            _ => Ok(Show::Val(self.compile(a, kind, false)?)),
        }
    }

    /// How a value of its type is shown: a string as text, a Go interface by its dynamic value,
    /// any other scalar as itself.
    fn show_pv(&mut self, pv: Pv) -> Result<Show, String> {
        match self.d.ty(pv.ty) {
            Type::Struct(s) if s.iface() => self.dyn_show(pv, &s),
            Type::Struct(s) if self.d.seq(&s).is_some_and(|q| q.text) => {
                let (len, chunks) = self.text_reads(pv, TEXT_BYTES)?;
                Ok(Show::Text { len, chunks })
            }
            Type::Pointer(_) if self.go_map(&pv).is_some() => Err(format!(
                "`{}` is a Go map: record one of its values by its key, `{0}[\"key\"]`",
                pv.text
            )),
            _ => Ok(Show::Val(self.scalar(pv)?)),
        }
    }

    /// A Go interface's dynamic value: `runtime.eface` (`any`) has its type at word 0;
    /// `runtime.iface` has its itab there, whose type is at offset 8. The value is behind the
    /// data word for every type [`GO_DYN_TYPES`] names.
    fn dyn_show(&mut self, pv: Pv, s: &Struct) -> Result<Show, String> {
        let word = |n: &str| -> Result<u64, String> {
            s.member(n)
                .map(|m| m.off)
                .ok_or_else(|| format!("`{}` ({}) has no `{n}` word", pv.text, s.name))
        };
        let first = Pv {
            place: pv.place.field(
                word(if s.name == "runtime.eface" {
                    "_type"
                } else {
                    "tab"
                })?,
                8,
            ),
            ty: None,
            text: format!("the type word of {}", pv.text),
            ..pv.clone()
        };
        let l = first.place.scalar(8, &first.text)?;
        let nil = self.read(&first, l, Ty::Ptr);
        let ty = if s.name == "runtime.eface" {
            nil.clone()
        } else {
            let mut t = self.deref(first)?;
            t.place = t.place.field(8, 8);
            t.text = format!("the dynamic type of {}", pv.text);
            let l = t.place.scalar(8, &t.text)?;
            self.read(&t, l, Ty::Ptr)
        };
        let data = Pv {
            place: pv.place.field(word("data")?, 8),
            ty: None,
            text: pv.text.clone(),
            ..pv.clone()
        };
        let mut cases = Vec::new();
        for (name, tref, addr) in self.d.go_types.clone() {
            let mut v = self.deref(data.clone())?;
            v.place = v.place.field(0, self.d.size_of(Some(tref)));
            v.ty = Some(tref);
            v.text = format!("{}.({name})", pv.text);
            if let Ok(show) = self.show_pv(v) {
                cases.push(DynCase { addr, name, show });
            }
        }
        Ok(Show::Dyn { nil, ty, cases })
    }

    /// Whether a value is a Go map, and where its parts are.
    fn go_map(&self, pv: &Pv) -> Option<GoMap> {
        let Type::Pointer(t) = self.d.ty(pv.ty) else {
            return None;
        };
        let Type::Struct(m) = self.d.ty(t) else {
            return None;
        };
        if !m.name.starts_with("map<") {
            return None;
        }
        let (dp, dl) = (m.member("dirPtr")?, m.member("dirLen")?);
        let Type::Pointer(t1) = self.d.ty(dp.ty) else {
            return None;
        };
        let Type::Pointer(t2) = self.d.ty(t1) else {
            return None;
        };
        let Type::Struct(table) = self.d.ty(t2) else {
            return None;
        };
        let Type::Struct(gr) = self.d.ty(table.member("groups")?.ty) else {
            return None;
        };
        let Type::Pointer(gt) = self.d.ty(gr.member("data")?.ty) else {
            return None;
        };
        let Type::Struct(g) = self.d.ty(gt) else {
            return None;
        };
        let ctrl = g.member("ctrl")?.off;
        if let Some(sl) = g.member("slots") {
            let Type::Array(st, n) = self.d.ty(sl.ty) else {
                return None;
            };
            let Type::Struct(slot) = self.d.ty(st) else {
                return None;
            };
            let (k, e) = (slot.member("key")?, slot.member("elem")?);
            return Some(GoMap {
                dir_ptr: dp.off,
                dir_len: dl.off,
                ctrl,
                slots: n,
                key_ty: k.ty,
                elem_ty: e.ty,
                key: (sl.off + k.off, slot.size),
                elem: (sl.off + e.off, slot.size),
            });
        }
        let (ks, es) = (g.member("keys")?, g.member("elems")?);
        let Type::Array(kt, n) = self.d.ty(ks.ty) else {
            return None;
        };
        let Type::Array(et, _) = self.d.ty(es.ty) else {
            return None;
        };
        Some(GoMap {
            dir_ptr: dp.off,
            dir_len: dl.off,
            ctrl,
            slots: n,
            key_ty: kt,
            elem_ty: et,
            key: (ks.off, self.d.size_of(kt)),
            elem: (es.off, self.d.size_of(et)),
        })
    }

    /// `m["key"]` (or `m[5]`) of a Go map: every slot of the map's one group, its control byte,
    /// its key compared with the constant, and its value.
    fn lookup(&mut self, pv: Pv, m: GoMap, k: &Ast) -> Result<Show, String> {
        let text = format!("{}[{k}]", pv.text);
        match k {
            Ast::Str(s) if s.len() > STR_BYTES => {
                return Err(format!(
                    "a map key of more than {STR_BYTES} bytes (the engine reads at most \
                     {STR_BYTES} bytes of a string)"
                ))
            }
            Ast::Str(_) | Ast::Int(_) => {}
            _ => {
                return Err(format!(
                    "`{text}`: a map's value is read by a constant key, a string or an integer"
                ))
            }
        }
        let map = self.scalar(pv.clone())?;
        let ms = self.deref(pv.clone())?;
        let part = |off: u64, what: &str| Pv {
            place: ms.place.field(off, 8),
            ty: None,
            text: format!("{what} of {}", pv.text),
            ..ms.clone()
        };
        let dl = part(m.dir_len, "the directory length");
        let l = dl.place.scalar(8, &dl.text)?;
        let dir_len = self.read(&dl, l, Ty::I(8));
        let dp = part(m.dir_ptr, "the group");
        let l = dp.place.scalar(8, &dp.text)?;
        let group = self.read(&dp, l, Ty::Ptr);
        let g = self.deref(dp)?;
        let (ksize, esize) = (self.d.size_of(m.key_ty), self.d.size_of(m.elem_ty));
        let mut slots = Vec::new();
        for i in 0..m.slots {
            let at = |off: u64, size: u64, ty: Option<TRef>, what: String| Pv {
                place: g.place.field(off, size),
                ty,
                text: what,
                ..g.clone()
            };
            let c = at(
                m.ctrl + i,
                1,
                None,
                format!("{}'s control byte {i}", pv.text),
            );
            let l = c.place.scalar(1, &c.text)?;
            let ctrl = self.read(&c, l, Ty::U(1));
            let full = Expr::Bin(BinOp::Lt, Box::new(ctrl), Box::new(Expr::Int("128".into())));
            let kp = at(
                m.key.0 + i * m.key.1,
                ksize,
                m.key_ty,
                format!("{}'s key {i}", pv.text),
            );
            let hit = match k {
                Ast::Str(s) => self.str_eq_pv(kp, s)?,
                Ast::Int(v) => Expr::Bin(
                    BinOp::Eq,
                    Box::new(self.scalar(kp)?),
                    Box::new(Expr::Int(v.to_string())),
                ),
                _ => unreachable!("checked above"),
            };
            let vp = at(m.elem.0 + i * m.elem.1, esize, m.elem_ty, text.clone());
            let val = self.show_pv(vp)?;
            slots.push(Slot { full, hit, val });
        }
        Ok(Show::Lookup {
            map,
            dir_len,
            group,
            slots,
        })
    }

    /// The values of a function's `record` and `key` lines.
    fn values(&mut self, recs: &[SrcRecord]) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for rc in recs {
            let at = |ret: bool, r: &mut Self| {
                r.show(&rc.ast, ret)
                    .map_err(|e| format!("line {}: {e}", rc.line))
            };
            let entry = match rc.when {
                When::Entry | When::Both => Some(at(false, self)?),
                When::Return => None,
            };
            let at_return = match rc.when {
                When::Return | When::Both => Some(at(true, self)?),
                When::Entry => None,
            };
            out.push(Value {
                name: rc.name.clone(),
                text: rc.text.clone(),
                line: rc.line,
                key: rc.key,
                entry,
                at_return,
            });
        }
        Ok(out)
    }
}

fn function_plan<'a>(
    d: &'a Dwarf<'a>,
    elf: &Elf,
    f: &'a FnInfo,
    c: &SrcContract,
    rel: &str,
    id: u32,
    arch: &'a str,
) -> Result<Function, String> {
    let code = elf
        .bytes_at(f.low_pc, 16)
        .ok_or("its code is not in the program's file")?;
    let mut r = Reads {
        d,
        f,
        arch,
        at_entry: Vec::new(),
        at_return: Vec::new(),
    };
    let mut clauses = Vec::new();
    for cl in &c.clauses {
        let expr = r
            .compile(&cl.ast, cl.kind, false)
            .map_err(|e| format!("line {}: {e}", cl.line))?;
        clauses.push(Clause {
            kind: cl.kind,
            text: cl.text.clone(),
            line: cl.line,
            expr,
        });
    }
    let mut inputs = Vec::new();
    for x in &c.inputs {
        r.param(x.name.split(['.', '[']).next().unwrap_or(&x.name))
            .map_err(|e| format!("line {}: {e}", x.line))?;
        inputs.push(Input {
            name: x.name.clone(),
            text: x.text.clone(),
            line: x.line,
        });
    }
    let values = r.values(&c.records)?;
    // the return address at entry names the caller (x30 on arm64; the top of the stack on
    // x86_64)
    let ret_addr = (!values.is_empty()).then(|| {
        let l = if arch == "arm64" {
            Loc {
                name: "the return address".into(),
                base: Base::Reg(30),
                size: 8,
                ..Loc::default()
            }
        } else {
            Loc {
                name: "the return address".into(),
                base: Base::Reg(7),
                path: vec![0],
                size: 8,
                ..Loc::default()
            }
        };
        Reads::add(&mut r.at_entry, l)
    });
    Ok(Function {
        id,
        name: f.name.clone(),
        file: rel.into(),
        line: c.line,
        lang: f.lang,
        entry: f.low_pc,
        code: code.iter().map(|b| format!("{b:02x}")).collect(),
        frame: if f.lang == Lang::Go {
            Frame::G
        } else {
            Frame::Sp
        },
        at_entry: r.at_entry,
        at_return: r.at_return,
        clauses,
        inputs,
        values,
        ret_addr,
    })
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
