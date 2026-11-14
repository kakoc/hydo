//! The plan's compiler: a program's ELF file and its debug information (DWARF), and the
//! `// hydo:` comments of its sources, give each contracted function's entry address, where its
//! parameters are at entry, the offsets of the fields its clauses read, and where its result is
//! at return.
//!
//! What the first version reads: integer, boolean and pointer parameters in a register or on the
//! stack at entry; fields of structures through any number of pointers (Rust references, Go
//! pointers), and of a structure parameter passed in memory; an integer, boolean or pointer
//! result (Rust's return value, Go's first result). Everything else is refused with its reason
//! (docs/design/contracts.md, "Reading values").

use crate::lang::{self, Ast, SrcContract};
use crate::plan::{
    Base, Clause, ClauseKind, Expr, Frame, Function, Input, Lang, Loc, Plan, Program, Ty,
    Unchecked, PLAN_TYPE, PLAN_V,
};
use gimli::{AttributeValue, EndianSlice, LittleEndian, Operation, UnitOffset};

/// A reference to a type: the unit's index and the offset in it (Go's types are in other units
/// than its functions).
type TRef = (usize, UnitOffset);
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type R<'a> = EndianSlice<'a, LittleEndian>;

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
    Pointer(Option<TRef>),
    Struct(String, Vec<(String, u64, Option<TRef>)>),
    Other(String),
}

/// What a function's debug information says.
struct FnInfo {
    name: String,
    file: String,
    line: u32,
    low_pc: u64,
    lang: Lang,
    params: Vec<Param>,
    /// Rust's return type, or Go's result parameters in order.
    results: Vec<Option<TRef>>,
    concrete: bool,
}

struct Param {
    name: String,
    ty: Option<TRef>,
    /// Where the parameter is at the first instruction, or why it cannot be read there.
    loc: Result<Loc, String>,
}

struct Dwarf<'a> {
    dwarf: gimli::Dwarf<R<'a>>,
    units: Vec<gimli::Unit<R<'a>>>,
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
            let size = match e.attr_value(gimli::DW_AT_byte_size) {
                Ok(Some(v)) => v.udata_value().unwrap_or(0),
                _ => 0,
            };
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
                    let mut members = Vec::new();
                    if let Ok(mut tree) = u.entries_tree(Some(off)) {
                        if let Ok(root) = tree.root() {
                            let mut ch = root.children();
                            while let Ok(Some(c)) = ch.next() {
                                let ce = c.entry();
                                if ce.tag() != gimli::DW_TAG_member {
                                    continue;
                                }
                                let mname = self.name(unit, ce).unwrap_or_default();
                                let mo = match ce.attr_value(gimli::DW_AT_data_member_location) {
                                    Ok(Some(v)) => v.udata_value(),
                                    _ => None,
                                };
                                if let Some(mo) = mo {
                                    members.push((mname, mo, self.type_ref(unit, ce)));
                                }
                            }
                        }
                    }
                    return Type::Struct(name, members);
                }
                _ => return Type::Other(name),
            }
        }
        Type::Other("a chain of type names".into())
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

/// Where a parameter is at the function's first instruction, from its location expression.
fn entry_loc(
    d: &Dwarf,
    unit: usize,
    v: AttributeValue<R>,
    pc: u64,
    arch: &str,
    size: u8,
) -> Result<Loc, String> {
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
    let sp = sp_reg(arch);
    // at the first instruction the canonical frame address is the stack pointer on arm64, and
    // the stack pointer plus the return address on x86_64
    let cfa_adj: i64 = if arch == "arm64" { 0 } else { 8 };
    match ops.as_slice() {
        [Operation::Register { register }] => Ok(Loc {
            name: String::new(),
            base: Base::Reg(register.0 as u8),
            path: vec![],
            size,
        }),
        [Operation::RegisterOffset {
            register, offset, ..
        }] => Ok(Loc {
            name: String::new(),
            base: Base::Reg(register.0 as u8),
            path: vec![*offset],
            size,
        }),
        [Operation::FrameOffset { offset }] => Ok(Loc {
            name: String::new(),
            base: Base::Reg(sp),
            path: vec![*offset + cfa_adj],
            size,
        }),
        [Operation::CallFrameCFA] => Err("an unusual location (the frame address itself)".into()),
        _ if ops.iter().any(|o| matches!(o, Operation::Piece { .. })) => {
            Err("a value split over several registers (the first version reads a structure's fields only through a pointer)".into())
        }
        _ => Err(format!("a location the first version does not read ({} operations)", ops.len())),
    }
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
        results.push(d.type_ref(ui, &src).or_else(|| d.type_ref(ui, e)));
    }
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
                    results.push(ty);
                    continue;
                }
                let size = match d.ty(ty) {
                    Type::Scalar(_, n) => n,
                    Type::Pointer(_) => 8,
                    _ => 8,
                };
                let loc = match ce.attr_value(gimli::DW_AT_location) {
                    Ok(Some(v)) => entry_loc(d, ui, v, pc, arch, size),
                    _ => Err("no location (optimised out)".into()),
                };
                params.push(Param {
                    name: pname,
                    ty,
                    loc,
                });
            }
        }
    }
    Some(FnInfo {
        name,
        file,
        line,
        low_pc: low_pc.unwrap_or(0),
        lang,
        params,
        results,
        concrete: low_pc.is_some(),
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
    let d = Dwarf { dwarf, units };
    let fns = functions(&d, arch);
    let mut prog = Program {
        program: src.program.clone(),
        sha256,
        arch: arch.into(),
        functions: Vec::new(),
        unchecked: Vec::new(),
    };
    let mut lang_errors = Vec::new();
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
            match function_plan(&d, &elf, f, &c, &rel, id) {
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
    Ok(prog)
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

/// The reads of one function, collected while its clauses compile.
struct Reads<'a> {
    d: &'a Dwarf<'a>,
    f: &'a FnInfo,
    at_entry: Vec<Loc>,
    at_return: Vec<Loc>,
}

impl Reads<'_> {
    fn add(list: &mut Vec<Loc>, l: Loc) -> u32 {
        if let Some(i) = list
            .iter()
            .position(|x| x.base == l.base && x.path == l.path && x.size == l.size)
        {
            return i as u32;
        }
        list.push(l);
        (list.len() - 1) as u32
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

    /// Follow `fields` from a value at `loc` of type `ty`: the read of the last field and its
    /// type.
    fn fields(
        &self,
        mut loc: Loc,
        mut ty: Option<TRef>,
        fields: &[String],
        what: &str,
    ) -> Result<(Loc, Ty), String> {
        let mut shown = what.to_string();
        for (k, fname) in fields.iter().enumerate() {
            let mut t = self.d.ty(ty);
            // read through pointers (Rust references, Go pointers)
            let mut deref = 0;
            while let Type::Pointer(p) = t {
                if deref > 0 {
                    loc.path.push(0);
                }
                deref += 1;
                ty = p;
                t = self.d.ty(ty);
                if deref > 4 {
                    break;
                }
            }
            let Type::Struct(sname, members) = &t else {
                return Err(format!(
                    "`{shown}` has no field `{fname}`: it is not a structure"
                ));
            };
            let want = if self.f.lang == Lang::Rust && fname.chars().all(|c| c.is_ascii_digit()) {
                format!("__{fname}")
            } else {
                fname.clone()
            };
            let Some((_, off, mty)) = members.iter().find(|m| m.0 == want) else {
                let names: Vec<&str> = members.iter().map(|m| m.0.as_str()).collect();
                return Err(format!(
                    "`{sname}` has no field `{fname}` (its fields: {})",
                    names.join(", ")
                ));
            };
            if deref > 0 {
                // the value is a pointer: load it, then the field
                loc.path.push(*off as i64);
            } else if let Some(last) = loc.path.last_mut() {
                // a structure in memory: the field is further in
                *last += *off as i64;
            } else {
                return Err(format!(
                    "`{shown}` is a structure passed in registers; the first version reads a \
                     structure's fields only through a pointer"
                ));
            }
            ty = *mty;
            shown = format!(
                "{shown}.{}",
                fields[..=k].last().cloned().unwrap_or_default()
            );
        }
        match self.d.ty(ty) {
            Type::Scalar(t, n) => {
                loc.size = n;
                Ok((loc, t))
            }
            Type::Pointer(_) => {
                loc.size = 8;
                Ok((loc, Ty::Ptr))
            }
            Type::Struct(n, _) => Err(format!(
                "`{shown}` is a structure ({n}); compare its fields one by one"
            )),
            Type::Other(n) => Err(format!(
                "`{shown}` has a type the first version does not read ({n})"
            )),
        }
    }

    fn result_loc(&self) -> Result<(Loc, Option<TRef>), String> {
        let ty = match self.f.results.first() {
            Some(t) => *t,
            None => return Err(format!("{} has no result", self.f.name)),
        };
        if ty.is_none() {
            return Err(format!("{} returns nothing", self.f.name));
        }
        // Rust's and C's scalar return value, and Go's first integer result, are in the first
        // return register (x0, rax: DWARF register 0 on both)
        Ok((
            Loc {
                name: "result".into(),
                base: Base::Reg(0),
                path: vec![],
                size: 8,
            },
            ty,
        ))
    }

    fn compile(&mut self, a: &Ast, kind: ClauseKind, entry: bool) -> Result<Expr, String> {
        Ok(match a {
            Ast::Int(v) => Expr::Int(v.to_string()),
            Ast::Bool(b) => Expr::Bool(*b),
            Ast::Not(x) => Expr::Not(Box::new(self.compile(x, kind, entry)?)),
            Ast::Neg(x) => Expr::Neg(Box::new(self.compile(x, kind, entry)?)),
            Ast::Old(x) => self.compile(x, kind, true)?,
            Ast::Bin(op, x, y) => Expr::Bin(
                *op,
                Box::new(self.compile(x, kind, entry)?),
                Box::new(self.compile(y, kind, entry)?),
            ),
            Ast::Path(root, fields) => {
                let text = std::iter::once(root.as_str())
                    .chain(fields.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(".");
                if root == "result" {
                    let (loc, ty) = self.result_loc()?;
                    let (mut loc, t) = self.fields(loc, ty, fields, "result")?;
                    if fields.is_empty() {
                        if let Type::Scalar(_, n) = self.d.ty(ty) {
                            loc.size = n;
                        }
                    }
                    loc.name = text;
                    return Ok(Expr::Out(Self::add(&mut self.at_return, loc), t));
                }
                let p = self.param(root)?;
                let ploc = p
                    .loc
                    .clone()
                    .map_err(|e| format!("`{root}` cannot be read at entry: {e}"))?;
                let pty = p.ty;
                if entry || fields.is_empty() || kind == ClauseKind::Requires {
                    let (mut loc, t) = self.fields(ploc, pty, fields, root)?;
                    loc.name = if entry && kind == ClauseKind::Ensures && !fields.is_empty() {
                        format!("old({text})")
                    } else {
                        text
                    };
                    return Ok(Expr::In(Self::add(&mut self.at_entry, loc), t));
                }
                // a field read after the call: through the parameter's value at entry, which
                // must be a pointer (a structure passed by value cannot change for the caller)
                if !matches!(self.d.ty(pty), Type::Pointer(_)) {
                    let (mut loc, t) = self.fields(ploc, pty, fields, root)?;
                    loc.name = text;
                    return Ok(Expr::In(Self::add(&mut self.at_entry, loc), t));
                }
                let mut pl = ploc;
                pl.size = 8;
                pl.name = root.clone();
                let k = Self::add(&mut self.at_entry, pl);
                let base = Loc {
                    name: String::new(),
                    base: Base::In(k),
                    path: vec![],
                    size: 8,
                };
                let (mut loc, t) = self.fields(base, pty, fields, root)?;
                loc.name = text;
                Expr::Out(Self::add(&mut self.at_return, loc), t)
            }
        })
    }
}

fn function_plan<'a>(
    d: &'a Dwarf<'a>,
    elf: &Elf,
    f: &'a FnInfo,
    c: &SrcContract,
    rel: &str,
    id: u32,
) -> Result<Function, String> {
    let code = elf
        .bytes_at(f.low_pc, 16)
        .ok_or("its code is not in the program's file")?;
    let mut r = Reads {
        d,
        f,
        at_entry: Vec::new(),
        at_return: Vec::new(),
    };
    let mut clauses = Vec::new();
    for cl in &c.clauses {
        let expr = r
            .compile(&cl.ast, cl.kind, cl.kind == ClauseKind::Requires)
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
        r.param(&x.name)
            .map_err(|e| format!("line {}: {e}", x.line))?;
        inputs.push(Input {
            name: x.name.clone(),
            text: x.text.clone(),
            line: x.line,
        });
    }
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
    })
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
