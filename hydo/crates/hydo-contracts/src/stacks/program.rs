//! A guest program as `hydo inspect stacks` reads it from the host's copy: its symbols (with
//! Rust's names demangled), its lines (DWARF), its call frame information (`.eh_frame`), and for a
//! Go program its own table of functions (`.gopclntab`), which a stripped Go program keeps too.

use crate::compile::{rd16, rd32, rd64, Elf};
use gimli::{EndianSlice, LittleEndian, UnwindSection};
use std::collections::BTreeMap;

/// A symbol of the program.
#[derive(Clone, Debug)]
pub struct Sym {
    pub addr: u64,
    pub size: u64,
    pub name: String,
}

/// A line table row.
#[derive(Clone, Copy, Debug)]
struct Row {
    addr: u64,
    file: u32,
    line: u32,
    end: bool,
}

/// What hydo knows of one program.
pub struct Program {
    /// The host's file.
    pub host: String,
    pub arch: &'static str,
    /// ET_DYN: loaded at an address the guest kernel chose.
    pub pie: bool,
    /// The lowest link address of an executable PT_LOAD segment: the kernel's `mm->start_code`
    /// less the load bias.
    pub first_vaddr: u64,
    funcs: Vec<Sym>,
    /// The executable sections: address and bytes.
    code: Vec<(u64, Vec<u8>)>,
    objs: Vec<Sym>,
    files: Vec<String>,
    rows: Vec<Row>,
    eh: Vec<u8>,
    eh_addr: u64,
    text_addr: u64,
    pub go: Option<GoTab>,
    /// The debug information's sections (for Go's runtime types).
    pub(crate) dwarf: BTreeMap<&'static str, Vec<u8>>,
}

/// Where a program counter is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub function: Option<String>,
    pub file: Option<String>,
    pub line: Option<u64>,
}

fn cstr_at(b: &[u8], o: usize) -> String {
    let s = b.get(o..).unwrap_or(&[]);
    let e = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..e]).into_owned()
}

impl Program {
    pub fn load(host: &str) -> Result<Program, String> {
        let data = std::fs::read(host).map_err(|e| format!("{host}: {e}"))?;
        Program::parse(host, data)
    }

    pub fn parse(host: &str, data: Vec<u8>) -> Result<Program, String> {
        let elf = Elf::parse(data).map_err(|e| format!("{host}: {e}"))?;
        let arch = elf.arch()?;
        let b = &elf.data;
        let pie = rd16(b, 16) == Some(3);
        let phoff = rd64(b, 32).unwrap_or(0) as usize;
        let phentsize = rd16(b, 54).unwrap_or(0) as usize;
        let phnum = rd16(b, 56).unwrap_or(0) as usize;
        let mut first_vaddr = u64::MAX;
        for i in 0..phnum {
            let ph = phoff + i * phentsize;
            // PT_LOAD with PF_X
            if rd32(b, ph) == Some(1) && rd32(b, ph + 4).is_some_and(|f| f & 1 != 0) {
                first_vaddr = first_vaddr.min(rd64(b, ph + 16).unwrap_or(u64::MAX));
            }
        }
        let addr_of = |n: &str| {
            elf.sections
                .iter()
                .find(|s| s.name == n)
                .map_or(0, |s| s.addr)
        };
        // the symbol table
        let (mut funcs, mut objs) = (Vec::new(), Vec::new());
        let symtab = elf.section(".symtab")?;
        let strtab = elf.section(".strtab")?;
        for e in symtab.chunks_exact(24) {
            let info = e[4];
            let ty = info & 0xf;
            let value = rd64(e, 8).unwrap_or(0);
            if value == 0 || !(ty == 1 || ty == 2) {
                continue;
            }
            let name = cstr_at(&strtab, rd32(e, 0).unwrap_or(0) as usize);
            if name.is_empty() {
                continue;
            }
            let s = Sym {
                addr: value,
                size: rd64(e, 16).unwrap_or(0),
                name: demangle(&name),
            };
            if ty == 2 {
                funcs.push(s);
            } else {
                objs.push(s);
            }
        }
        funcs.sort_by_key(|s| s.addr);
        objs.sort_by_key(|s| s.addr);
        let mut dwarf = BTreeMap::new();
        for id in [
            gimli::SectionId::DebugAbbrev,
            gimli::SectionId::DebugAddr,
            gimli::SectionId::DebugInfo,
            gimli::SectionId::DebugLine,
            gimli::SectionId::DebugLineStr,
            gimli::SectionId::DebugRanges,
            gimli::SectionId::DebugRngLists,
            gimli::SectionId::DebugStr,
            gimli::SectionId::DebugStrOffsets,
        ] {
            dwarf.insert(id.name(), elf.section(id.name())?);
        }
        // the table's own start of the text is 0 in newer Go programs: then it is `runtime.text`,
        // the start of `.text`
        let go = GoTab::parse(elf.section(".gopclntab")?, addr_of(".text"));
        let code = elf
            .sections
            .iter()
            .filter(|s| s.flags & 4 != 0 && !s.nobits)
            .filter_map(|s| {
                let b = elf
                    .data
                    .get(s.offset as usize..(s.offset + s.size) as usize)?;
                Some((s.addr, b.to_vec()))
            })
            .collect();
        let mut p = Program {
            host: host.to_string(),
            arch,
            pie,
            first_vaddr: if first_vaddr == u64::MAX {
                0
            } else {
                first_vaddr
            },
            funcs,
            code,
            objs,
            files: Vec::new(),
            rows: Vec::new(),
            eh: elf.section(".eh_frame")?,
            eh_addr: addr_of(".eh_frame"),
            text_addr: addr_of(".text"),
            go,
            dwarf,
        };
        if p.go.is_none() {
            p.read_lines();
        }
        Ok(p)
    }

    fn read_lines(&mut self) {
        let sec = &self.dwarf;
        let Ok(sections) = gimli::DwarfSections::load(|id| -> Result<&[u8], ()> {
            Ok(sec.get(id.name()).map(Vec::as_slice).unwrap_or(&[]))
        }) else {
            return;
        };
        let dwarf = sections.borrow(|s| EndianSlice::new(s, LittleEndian));
        let mut files: Vec<String> = Vec::new();
        let mut idx: BTreeMap<String, u32> = BTreeMap::new();
        let mut rows = Vec::new();
        let mut it = dwarf.units();
        while let Ok(Some(h)) = it.next() {
            let Ok(u) = dwarf.unit(h) else { continue };
            let Some(lp) = u.line_program.clone() else {
                continue;
            };
            let mut r = lp.rows();
            while let Ok(Some((hdr, row))) = r.next_row() {
                let file = match row.file(hdr) {
                    Some(f) => {
                        let mut path = String::new();
                        if let Some(d) = f.directory(hdr) {
                            if let Ok(s) = dwarf.attr_string(&u, d) {
                                path.push_str(&s.to_string_lossy());
                            }
                        }
                        if let Ok(s) = dwarf.attr_string(&u, f.path_name()) {
                            let s = s.to_string_lossy();
                            if s.starts_with('/') || path.is_empty() {
                                path = s.into_owned();
                            } else {
                                path = format!("{path}/{s}");
                            }
                        }
                        let n = files.len() as u32;
                        *idx.entry(path.clone()).or_insert_with(|| {
                            files.push(path);
                            n
                        })
                    }
                    None => u32::MAX,
                };
                rows.push(Row {
                    addr: row.address(),
                    file,
                    line: row.line().map(|l| l.get() as u32).unwrap_or(0),
                    end: row.end_sequence(),
                });
            }
        }
        // an end of a sequence sorts before a row that starts at the same address
        rows.sort_by_key(|r| (r.addr, !r.end));
        self.files = files;
        self.rows = rows;
    }

    /// The function (symbol table or Go's table), file and line at `pc` (a link address).
    pub fn place(&self, pc: u64) -> Place {
        if let Some(g) = &self.go {
            if let Some(f) = g.func(pc) {
                return Place {
                    function: Some(g.name(&f)),
                    file: g.file(&f, pc),
                    line: g.line(&f, pc).map(|l| l as u64),
                };
            }
        }
        let function = self.func(pc).map(|s| s.name.clone());
        let i = self.rows.partition_point(|r| r.addr <= pc);
        let (file, line) = match i.checked_sub(1).map(|i| self.rows[i]) {
            Some(r) if !r.end && r.line > 0 => (
                self.files.get(r.file as usize).cloned(),
                Some(r.line as u64),
            ),
            _ => (None, None),
        };
        Place {
            function,
            file,
            line,
        }
    }

    /// The function that holds `pc`: its start and end (link addresses).
    pub fn range(&self, pc: u64) -> Option<(u64, u64)> {
        if let Some(g) = &self.go {
            if let Some(f) = g.func(pc) {
                return Some((f.entry, g.end(&f)));
            }
        }
        let s = self.func(pc)?;
        (s.size > 0).then_some((s.addr, s.addr + s.size))
    }

    /// Whether the code in `[lo, hi)` refers to an address in `[a, b)` (x86_64: a 32-bit
    /// displacement from the instruction's end, as `lea` and `mov` with a `rip`-relative operand
    /// have it in their last four bytes). A heuristic: the bytes are not decoded.
    pub fn refers(&self, lo: u64, hi: u64, a: u64, b: u64) -> bool {
        if self.arch != "x86_64" {
            return false;
        }
        for (base, bytes) in &self.code {
            let end = base + bytes.len() as u64;
            let (s, e) = (lo.max(*base), hi.min(end));
            if s >= e {
                continue;
            }
            let c = &bytes[(s - base) as usize..(e - base) as usize];
            for i in 0..c.len().saturating_sub(3) {
                let d = i32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]) as i64;
                let t = (s + i as u64 + 4).wrapping_add(d as u64);
                if a <= t && t < b {
                    return true;
                }
            }
        }
        false
    }

    /// The global variable `addr` is in: its start and end (link addresses).
    pub fn object_range(&self, addr: u64) -> Option<(u64, u64)> {
        let i = self
            .objs
            .partition_point(|s| s.addr <= addr)
            .checked_sub(1)?;
        let s = &self.objs[i];
        (addr < s.addr + s.size.max(1)).then_some((s.addr, s.addr + s.size.max(1)))
    }

    /// The function symbol that holds `pc`.
    pub fn func(&self, pc: u64) -> Option<&Sym> {
        let i = self
            .funcs
            .partition_point(|s| s.addr <= pc)
            .checked_sub(1)?;
        let s = &self.funcs[i];
        (s.size == 0 || pc < s.addr + s.size).then_some(s)
    }

    /// The global variable that holds `addr`: its name and the offset in it.
    pub fn object(&self, addr: u64) -> Option<(String, u64)> {
        let i = self
            .objs
            .partition_point(|s| s.addr <= addr)
            .checked_sub(1)?;
        let s = &self.objs[i];
        (addr < s.addr + s.size.max(1)).then(|| (s.name.clone(), addr - s.addr))
    }

    /// A global symbol's address by its name (as the table shows it, demangled).
    pub fn symbol(&self, name: &str) -> Option<&Sym> {
        self.objs
            .iter()
            .chain(self.funcs.iter())
            .find(|s| s.name == name)
    }

    /// The canonical frame address and the return address's place at `pc`, from `.eh_frame`:
    /// `(cfa register, cfa offset, return address offset from the cfa, frame pointer offset)`.
    pub fn cfi(&self, pc: u64) -> Option<Cfi> {
        if self.eh.is_empty() {
            return None;
        }
        let eh = gimli::EhFrame::new(&self.eh, LittleEndian);
        let bases = gimli::BaseAddresses::default()
            .set_eh_frame(self.eh_addr)
            .set_text(self.text_addr);
        let mut ctx: Box<gimli::UnwindContext<usize>> = Box::new(gimli::UnwindContext::new());
        let row = eh
            .unwind_info_for_address(&bases, &mut ctx, pc, gimli::EhFrame::cie_from_offset)
            .ok()?;
        let (reg, off) = match row.cfa() {
            gimli::CfaRule::RegisterAndOffset { register, offset } => (register.0, *offset),
            _ => return None,
        };
        let (ra_reg, fp_reg) = if self.arch == "x86_64" {
            (16, 6)
        } else {
            (30, 29)
        };
        let rule = |r: u16| match row.register(gimli::Register(r)) {
            gimli::RegisterRule::Offset(o) => Some(o),
            _ => None,
        };
        Some(Cfi {
            cfa_reg: reg,
            cfa_off: off,
            ra: rule(ra_reg),
            fp: rule(fp_reg),
        })
    }
}

/// One row of call frame information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cfi {
    pub cfa_reg: u16,
    pub cfa_off: i64,
    /// Where the return address is saved, from the CFA (none: it stays in its register, as
    /// arm64's x30 in a leaf).
    pub ra: Option<i64>,
    /// Where the frame pointer is saved, from the CFA.
    pub fp: Option<i64>,
}

// ---------------------------------------------------------------------------------------- Go

/// Go's table of functions (`.gopclntab`, Go 1.18 and later): each function's entry, name,
/// frame size at each program counter, file and line.
pub struct GoTab {
    d: Vec<u8>,
    quantum: u64,
    nfunc: usize,
    text: u64,
    names: usize,
    cus: usize,
    files: usize,
    pcs: usize,
    funcs: usize,
    /// 120: Go 1.20 and later; 118: 1.18 and 1.19.
    ver: u32,
}

/// One function of Go's table.
#[derive(Clone, Copy, Debug)]
pub struct GoFunc {
    pub entry: u64,
    name: i32,
    pcsp: u32,
    pcfile: u32,
    pcln: u32,
    cu: u32,
}

fn uvarint(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for s in (0..64).step_by(7) {
        let c = *b.get(*p)?;
        *p += 1;
        v |= ((c & 0x7f) as u64) << s;
        if c & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

impl GoTab {
    /// `text`: where the program's text starts, for a table that leaves its own field 0.
    pub fn parse(d: Vec<u8>, text: u64) -> Option<GoTab> {
        let magic = rd32(&d, 0)?;
        let ver = match magic {
            0xffff_fff1 => 120,
            0xffff_fff0 => 118,
            _ => return None,
        };
        if d[7] != 8 {
            return None;
        }
        let w = |i: usize| rd64(&d, 8 + i * 8).map(|v| v as usize);
        Some(GoTab {
            quantum: d[6] as u64,
            nfunc: w(0)?,
            text: match w(2)? as u64 {
                0 => text,
                t => t,
            },
            names: w(3)?,
            cus: w(4)?,
            files: w(5)?,
            pcs: w(6)?,
            funcs: w(7)?,
            ver,
            d,
        })
    }

    /// The function that holds `pc`.
    pub fn func(&self, pc: u64) -> Option<GoFunc> {
        let off = pc.checked_sub(self.text)?;
        let ent = |i: usize| rd32(&self.d, self.funcs + i * 8).map(|v| v as u64);
        let (mut lo, mut hi) = (0usize, self.nfunc);
        if off >= ent(self.nfunc)? {
            return None;
        }
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if ent(mid)? <= off {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        if ent(lo)? > off {
            return None;
        }
        let fo = self.funcs + rd32(&self.d, self.funcs + lo * 8 + 4)? as usize;
        let r = |o: usize| rd32(&self.d, fo + o);
        Some(GoFunc {
            entry: self.text + r(0)? as u64,
            name: r(4)? as i32,
            pcsp: r(16)?,
            pcfile: r(20)?,
            pcln: r(24)?,
            cu: r(32)?,
        })
    }

    /// Where a function ends: the next function's entry.
    pub fn end(&self, f: &GoFunc) -> u64 {
        let ent = |i: usize| rd32(&self.d, self.funcs + i * 8).map(|v| v as u64);
        let off = f.entry - self.text;
        let (mut lo, mut hi) = (0usize, self.nfunc);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if ent(mid).unwrap_or(u64::MAX) <= off {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        self.text + ent(lo + 1).unwrap_or(off + 1)
    }

    pub fn name(&self, f: &GoFunc) -> String {
        cstr_at(&self.d, self.names + f.name as usize)
    }

    /// The value of one of a function's tables (`pcsp`, `pcfile`, `pcln`) at `pc`.
    fn value(&self, f: &GoFunc, table: u32, pc: u64) -> Option<i64> {
        if table == 0 {
            return None;
        }
        let mut p = self.pcs + table as usize;
        let (mut val, mut at, mut first) = (-1i64, f.entry, true);
        loop {
            let uv = uvarint(&self.d, &mut p)?;
            if uv == 0 && !first {
                return None;
            }
            first = false;
            let delta = if uv & 1 != 0 {
                !((uv >> 1) as i64)
            } else {
                (uv >> 1) as i64
            };
            val += delta;
            at += uvarint(&self.d, &mut p)? * self.quantum;
            if pc < at {
                return Some(val);
            }
        }
    }

    /// The frame's size at `pc`: how far the stack pointer is below its value at the entry.
    pub fn sp_delta(&self, f: &GoFunc, pc: u64) -> Option<i64> {
        self.value(f, f.pcsp, pc)
    }

    pub fn line(&self, f: &GoFunc, pc: u64) -> Option<i64> {
        self.value(f, f.pcln, pc)
    }

    pub fn file(&self, f: &GoFunc, pc: u64) -> Option<String> {
        let i = self.value(f, f.pcfile, pc)?;
        let o = rd32(&self.d, self.cus + (f.cu as usize + i as usize) * 4)?;
        (o != u32::MAX).then(|| cstr_at(&self.d, self.files + o as usize))
    }

    pub fn version(&self) -> u32 {
        self.ver
    }
}

// ------------------------------------------------------------------------------ Rust's names

/// A Rust legacy-mangled name (`_ZN4bank7journal17h0123456789abcdefE`) as `bank::journal`;
/// other names unchanged.
pub fn demangle(s: &str) -> String {
    let Some(mut r) = s.strip_prefix("_ZN") else {
        return s.to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    loop {
        if let Some(rest) = r.strip_prefix('E') {
            if !rest.is_empty() && !rest.starts_with('.') {
                return s.to_string();
            }
            break;
        }
        let n: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(len) = n.parse::<usize>() else {
            return s.to_string();
        };
        r = &r[n.len()..];
        let Some(id) = r.get(..len) else {
            return s.to_string();
        };
        parts.push(id.to_string());
        r = &r[len..];
    }
    if parts.last().is_some_and(|h| {
        h.len() == 17 && h.starts_with('h') && h[1..].chars().all(|c| c.is_ascii_hexdigit())
    }) {
        parts.pop();
    }
    let unesc = |p: &str| -> String {
        let p = p
            .strip_prefix("_$")
            .map(|x| format!("${x}"))
            .unwrap_or(p.to_string());
        let mut o = p.replace("..", "::");
        for (a, b) in [
            ("$SP$", "@"),
            ("$BP$", "*"),
            ("$RF$", "&"),
            ("$LT$", "<"),
            ("$GT$", ">"),
            ("$LP$", "("),
            ("$RP$", ")"),
            ("$C$", ","),
            ("$u7e$", "~"),
            ("$u20$", " "),
            ("$u27$", "'"),
            ("$u3d$", "="),
            ("$u5b$", "["),
            ("$u5d$", "]"),
            ("$u7b$", "{"),
            ("$u7d$", "}"),
            ("$u3b$", ";"),
            ("$u2b$", "+"),
            ("$u22$", "\""),
        ] {
            o = o.replace(a, b);
        }
        o
    };
    parts
        .iter()
        .map(|p| unesc(p))
        .collect::<Vec<_>>()
        .join("::")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_names() {
        assert_eq!(
            demangle("_ZN4bank7journal17h0123456789abcdefE"),
            "bank::journal"
        );
        assert_eq!(
            demangle("_ZN3std4sync5mutex14Mutex$LT$T$GT$4lock17h00000000000000aaE"),
            "std::sync::mutex::Mutex<T>::lock"
        );
        assert_eq!(demangle("main.worker"), "main.worker");
        assert_eq!(demangle("_ZN3fooE"), "foo");
    }

    #[test]
    fn go_pcvalue_tables() {
        // a table of two functions at text 0x1000: f0 [0x1000,0x1010), f1 [0x1010,0x1030)
        let mut d = vec![0u8; 0x400];
        d[0..4].copy_from_slice(&0xffff_fff1u32.to_le_bytes());
        d[6] = 1;
        d[7] = 8;
        let w = |d: &mut Vec<u8>, i: usize, v: u64| {
            d[8 + i * 8..16 + i * 8].copy_from_slice(&v.to_le_bytes())
        };
        // nfunc, nfiles, text, names, cus, files, pcs, funcs
        for (i, v) in [2u64, 1, 0x1000, 0x100, 0x140, 0x160, 0x180, 0x200]
            .into_iter()
            .enumerate()
        {
            w(&mut d, i, v);
        }
        d[0x100..0x108].copy_from_slice(b"main.f0\0");
        d[0x108..0x110].copy_from_slice(b"main.f1\0");
        // the cu table: file 0 at offset 0 of the file names
        d[0x140..0x144].copy_from_slice(&0u32.to_le_bytes());
        d[0x160..0x168].copy_from_slice(b"/x.go\0\0\0");
        // pcsp of f1: value 0 for 4 bytes, then 24 for the rest (deltas +1, then +24)
        // uv = zigzag: +1 -> 2, +24 -> 48; pc deltas 4 and 0x1c
        d[0x181..0x186].copy_from_slice(&[2, 4, 48, 0x1c, 0]);
        // pcln of f1: line 7 for all (+8 from -1 -> uv 16), pc delta 0x20
        d[0x190..0x193].copy_from_slice(&[16, 0x20, 0]);
        // pcfile of f1: file 0 (+1 -> 2)
        d[0x1a0..0x1a3].copy_from_slice(&[2, 0x20, 0]);
        // functab: (entryoff, funcoff) x 3
        let ft = [(0u32, 0x40u32), (0x10, 0x80), (0x30, 0)];
        for (i, (e, f)) in ft.iter().enumerate() {
            d[0x200 + i * 8..0x204 + i * 8].copy_from_slice(&e.to_le_bytes());
            d[0x204 + i * 8..0x208 + i * 8].copy_from_slice(&f.to_le_bytes());
        }
        // _func of f1 at funcs + 0x80
        let f = 0x280;
        d[f..f + 4].copy_from_slice(&0x10u32.to_le_bytes());
        d[f + 4..f + 8].copy_from_slice(&8u32.to_le_bytes());
        d[f + 16..f + 20].copy_from_slice(&1u32.to_le_bytes());
        d[f + 20..f + 24].copy_from_slice(&0x20u32.to_le_bytes());
        d[f + 24..f + 28].copy_from_slice(&0x10u32.to_le_bytes());
        let g = GoTab::parse(d.clone(), 0).unwrap();
        // a table whose own text field is 0 takes the section's
        let mut z = d;
        z[8 + 16..8 + 24].copy_from_slice(&0u64.to_le_bytes());
        let gz = GoTab::parse(z, 0x1000).unwrap();
        assert_eq!(gz.func(0x1015).map(|f| f.entry), Some(0x1010));
        let fun = g.func(0x1015).unwrap();
        assert_eq!(fun.entry, 0x1010);
        assert_eq!(g.name(&fun), "main.f1");
        assert_eq!(g.sp_delta(&fun, 0x1011), Some(0));
        assert_eq!(g.sp_delta(&fun, 0x1020), Some(24));
        assert_eq!(g.line(&fun, 0x1020), Some(7));
        assert_eq!(g.file(&fun, 0x1020).as_deref(), Some("/x.go"));
        assert!(g.func(0x1030).is_none());
        assert!(g.func(0xfff).is_none());
    }
}
