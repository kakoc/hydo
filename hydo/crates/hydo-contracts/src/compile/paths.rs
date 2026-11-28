//! The error paths' places (Milestone 306, docs/design/error-paths.md): the places the sources
//! show ([`crate::paths::scan`]) get their addresses from the debug information's line table, and
//! the places only the code shows (Rust's `?`, `unwrap()` and panics, and functions named for a
//! retry, a recovery or a rollback) come from the debug information and the calls in the code.

use super::{Dwarf, Elf, FnInfo, R};
use crate::paths::{self, kind, PathPage, PathPlace, PathSite, SrcLang, SrcPaths, PAGE};
use crate::plan::Lang;
use gimli::AttributeValue;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// What the compiler found: the places, the pages that hold them, and the places of the
/// sources without code in the program.
#[derive(Default)]
pub(super) struct Found {
    pub places: Vec<PathPlace>,
    pub pages: Vec<PathPage>,
    pub without_code: u32,
    /// The file pages already in `pages`: file page index -> index in `pages`.
    pub seen: HashMap<u64, u32>,
}

/// The core functions whose call is a Rust error path, with the kind: the failures of
/// `unwrap()` and `expect()`, and explicit panics. Bounds checks and arithmetic overflow are
/// left out: no developer wrote them as error handling.
fn panic_kind(name: &str) -> Option<&'static str> {
    let base = name.split('<').next().unwrap_or(name);
    match base {
        "core::result::unwrap_failed"
        | "core::option::unwrap_failed"
        | "core::option::expect_failed" => Some(kind::UNWRAP),
        "core::panicking::panic"
        | "core::panicking::panic_fmt"
        | "core::panicking::panic_display"
        | "core::panicking::panic_explicit"
        | "core::panicking::panic_str"
        | "core::panicking::unreachable_display"
        | "core::panicking::assert_failed"
        | "std::panicking::begin_panic" => Some(kind::PANIC),
        _ => None,
    }
}

/// An inlined call with its call site in one of the sources: its code's range, the source file
/// (index in `files`), the line of the call, and its depth in the tree.
struct Inl {
    lo: u64,
    hi: u64,
    file: usize,
    line: u32,
    depth: isize,
}

/// How the paths of the debug information name the sources: the folder each source file's
/// relative path follows (`github.com/blnkfinance/blnk/` for Go with `-trimpath`, a remapped
/// folder for Rust). A dependency's file with the same relative path (`src/lib.rs`,
/// `options.go`) has another folder, so it is not taken for a source file. Each folder of the
/// manifest's `sources:` gets its own (a Go module and a module it requires, whose files
/// `-trimpath` names `github.com/nsqio/go-diskqueue@v1.1.0/`), so a small second folder is not
/// outvoted by the first.
struct Names {
    /// The source folder (the part of a file's path before its relative path) and the folder
    /// of the debug information's paths that names its files.
    prefixes: Vec<(usize, String)>,
    /// Each source file's folder, by its index in `files`.
    roots: Vec<usize>,
}

/// The source folder of a file of `source_files`: its path without its relative path.
fn root_of(file: &(PathBuf, String)) -> String {
    let p = file.0.to_string_lossy().replace('\\', "/");
    p.strip_suffix(file.1.as_str()).unwrap_or(&p).to_string()
}

impl Names {
    /// The folders that most paths of the debug information share with the sources: for each
    /// source folder, the one that names the most of its files, and every other that names at
    /// least half as many.
    fn of(all: &[String], files: &[(PathBuf, String)]) -> Names {
        // the folders as numbers, in the order of `files`
        let mut folders: Vec<String> = Vec::new();
        let roots: Vec<usize> = files
            .iter()
            .map(|f| {
                let r = root_of(f);
                match folders.iter().position(|x| *x == r) {
                    Some(i) => i,
                    None => {
                        folders.push(r);
                        folders.len() - 1
                    }
                }
            })
            .collect();
        let mut votes: HashMap<(usize, String), std::collections::HashSet<usize>> = HashMap::new();
        for p in all {
            let p = p.replace('\\', "/");
            for (i, (_, rel)) in files.iter().enumerate() {
                if p == *rel {
                    votes
                        .entry((roots[i], String::new()))
                        .or_default()
                        .insert(i);
                } else if let Some(pre) = p.strip_suffix(rel.as_str()) {
                    if pre.ends_with('/') {
                        votes
                            .entry((roots[i], pre.to_string()))
                            .or_default()
                            .insert(i);
                    }
                }
            }
        }
        let mut best: HashMap<usize, usize> = HashMap::new();
        for ((root, _), v) in &votes {
            let b = best.entry(*root).or_default();
            *b = (*b).max(v.len());
        }
        let mut prefixes: Vec<(usize, String)> = votes
            .iter()
            .filter(|((root, _), v)| {
                let b = best.get(root).copied().unwrap_or(0);
                b > 0 && 2 * v.len() >= b
            })
            .map(|(k, _)| k.clone())
            .collect();
        prefixes.sort();
        Names { prefixes, roots }
    }

    /// The source file (an index in `files`) that a path of the debug information names.
    fn file_of(&self, p: &str, files: &[(PathBuf, String)]) -> Option<usize> {
        let p = p.replace('\\', "/");
        self.prefixes.iter().find_map(|(root, pre)| {
            let rel = p.strip_prefix(pre.as_str())?;
            files
                .iter()
                .enumerate()
                .position(|(i, f)| f.1 == rel && self.roots[i] == *root)
        })
    }
}

fn attr_u64(e: &gimli::DebuggingInformationEntry<R<'_>>, at: gimli::DwAt) -> Option<u64> {
    match e.attr_value(at).ok()?? {
        AttributeValue::FileIndex(i) => Some(i),
        v => v.udata_value(),
    }
}

/// The error-handling places of a program from its sources (`files`), with the debug
/// information `d`, the functions `fns` and the code in `elf`. Place ids start at `first_id`.
pub(super) fn find(
    elf: &Elf,
    d: &Dwarf,
    fns: &[FnInfo],
    arch: &str,
    files: &[(PathBuf, String)],
    first_id: u32,
) -> Found {
    // the paths of the line tables' files, and the folder they name the sources by
    let mut seen: std::collections::HashSet<(usize, u64)> = std::collections::HashSet::new();
    let mut all: Vec<String> = fns.iter().map(|f| f.file.clone()).collect();
    for (ui, u) in d.units.iter().enumerate() {
        let Some(lp) = u.line_program.clone() else {
            continue;
        };
        let mut it = lp.rows();
        while let Ok(Some((_, row))) = it.next_row() {
            if seen.insert((ui, row.file_index())) {
                if let Some(p) = d.file_name(ui, row.file_index()) {
                    all.push(p);
                }
            }
        }
    }
    all.sort();
    all.dedup();
    let names = Names::of(&all, files);
    let file_of = |p: &str| names.file_of(p, files);
    // which file each path of a unit's file table is (cached per unit and index)
    let mut fcache: HashMap<(usize, u64), Option<usize>> = HashMap::new();
    let mut fidx = |ui: usize, i: u64| -> Option<usize> {
        *fcache
            .entry((ui, i))
            .or_insert_with(|| d.file_name(ui, i).and_then(|p| file_of(&p)))
    };
    // the line table: the statement addresses of each line of the sources, and for Rust the
    // rows of every address (to name the line of a call in the code)
    let mut by_line: HashMap<(usize, u32), Vec<u64>> = HashMap::new();
    let mut compiled = vec![false; files.len()];
    let mut rows: Vec<(u64, Option<usize>, u32)> = Vec::new();
    let mut rust_units = Vec::new();
    for (ui, u) in d.units.iter().enumerate() {
        let lang = match u.entries().next_dfs() {
            Ok(Some((_, root))) => match root.attr_value(gimli::DW_AT_language) {
                Ok(Some(AttributeValue::Language(l))) if l == gimli::DW_LANG_Rust => Lang::Rust,
                Ok(Some(AttributeValue::Language(l))) if l == gimli::DW_LANG_Go => Lang::Go,
                _ => Lang::C,
            },
            _ => Lang::C,
        };
        if lang == Lang::Rust {
            rust_units.push(ui);
        }
        let Some(lp) = u.line_program.clone() else {
            continue;
        };
        let mut it = lp.rows();
        while let Ok(Some((_, row))) = it.next_row() {
            if row.end_sequence() {
                if lang == Lang::Rust {
                    rows.push((row.address(), None, 0));
                }
                continue;
            }
            let line = row.line().map_or(0, |l| l.get() as u32);
            let f = fidx(ui, row.file_index());
            if let Some(f) = f {
                compiled[f] = true;
                if row.is_stmt() && line > 0 {
                    by_line.entry((f, line)).or_default().push(row.address());
                }
            }
            if lang == Lang::Rust {
                rows.push((row.address(), f, line));
            }
        }
    }
    rows.sort_by_key(|r| r.0);
    // the functions with code of their own, by address: a line's copies are grouped by them
    let mut ranges: Vec<(u64, u64)> = fns
        .iter()
        .filter(|f| f.concrete && f.high_pc > f.low_pc)
        .map(|f| (f.low_pc, f.high_pc))
        .collect();
    ranges.sort();
    let group = |a: u64| -> u64 {
        let i = ranges.partition_point(|r| r.0 <= a);
        match i.checked_sub(1).and_then(|k| ranges.get(k)) {
            Some(r) if a < r.1 => r.0,
            _ => a,
        }
    };
    let first_of_each = |addrs: &[u64]| -> Vec<u64> {
        let mut m: BTreeMap<u64, u64> = BTreeMap::new();
        for &a in addrs {
            let g = group(a);
            let e = m.entry(g).or_insert(a);
            *e = (*e).min(a);
        }
        let mut v: Vec<u64> = m.into_values().collect();
        v.sort_unstable();
        v
    };
    // places: (file, line) -> (kind, addresses)
    let mut at: BTreeMap<(usize, u32), (&'static str, Vec<u64>)> = BTreeMap::new();
    let mut src: Vec<SrcPaths> = Vec::with_capacity(files.len());
    let mut without_code = 0u32;
    for (fi, (path, rel)) in files.iter().enumerate() {
        let lang = SrcLang::of(rel);
        let text = lang.and_then(|_| std::fs::read_to_string(path).ok());
        let sp = match (lang, text) {
            (Some(l), Some(t)) => paths::scan(&t, l),
            _ => SrcPaths::default(),
        };
        if compiled[fi] {
            for p in &sp.places {
                match by_line.get(&(fi, p.line)) {
                    Some(a) => {
                        at.insert((fi, p.line), (p.kind, first_of_each(a)));
                    }
                    None => without_code += 1,
                }
            }
        }
        src.push(sp);
    }
    // Rust: the inlined calls with a call site in the sources, and the `?` among them
    let mut inl: Vec<Inl> = Vec::new();
    for &ui in &rust_units {
        let u = &d.units[ui];
        let mut cur = u.entries();
        let mut depth = 0isize;
        while let Ok(Some((delta, e))) = cur.next_dfs() {
            depth += delta;
            if e.tag() != gimli::DW_TAG_inlined_subroutine {
                continue;
            }
            let Some(cf) = attr_u64(e, gimli::DW_AT_call_file).and_then(|i| fidx(ui, i)) else {
                continue;
            };
            let line = attr_u64(e, gimli::DW_AT_call_line).unwrap_or(0) as u32;
            let mut rs = Vec::new();
            if let Ok(mut it) = d.dwarf.die_ranges(u, e) {
                while let Ok(Some(r)) = it.next() {
                    if r.end > r.begin {
                        rs.push((r.begin, r.end));
                    }
                }
            }
            if rs.is_empty() || line == 0 {
                continue;
            }
            for &(lo, hi) in &rs {
                inl.push(Inl {
                    lo,
                    hi,
                    file: cf,
                    line,
                    depth,
                });
            }
            // the error branch of a `?`: `<Result as FromResidual>::from_residual`, inlined
            let origin = super::Dwarf::same_unit_ref(e, gimli::DW_AT_abstract_origin)
                .and_then(|o| u.entry(o).ok());
            let Some(o) = origin else { continue };
            let name = d.name(ui, &o).unwrap_or_default();
            let decl = attr_u64(&o, gimli::DW_AT_decl_file)
                .and_then(|i| d.file_name(ui, i))
                .unwrap_or_default();
            if name.starts_with("from_residual") && decl.ends_with("result.rs") {
                let lo = rs.iter().map(|r| r.0).min().unwrap_or(0);
                let e = at.entry((cf, line)).or_insert((kind::TRY, Vec::new()));
                if e.0 == kind::TRY && !e.1.contains(&lo) {
                    e.1.push(lo);
                }
            }
        }
    }
    // the source line of an address: its row when that is in the sources, else the innermost
    // inlined call with a call site there
    let line_of = |a: u64| -> Option<(usize, u32)> {
        let i = rows.partition_point(|r| r.0 <= a);
        if let Some(&(_, Some(f), l)) = i.checked_sub(1).and_then(|k| rows.get(k)) {
            if l > 0 {
                return Some((f, l));
            }
        }
        inl.iter()
            .filter(|x| x.lo <= a && a < x.hi)
            .max_by_key(|x| x.depth)
            .map(|x| (x.file, x.line))
    };
    // Rust: the calls of the panic functions (and of a `from_residual` the compiler kept)
    if !rust_units.is_empty() {
        let mut targets: HashMap<u64, &'static str> = HashMap::new();
        for f in fns.iter().filter(|f| f.concrete && f.lang == Lang::Rust) {
            if let Some(k) = panic_kind(&f.name) {
                targets.insert(f.low_pc, k);
            } else if f.name.contains("::from_residual") && f.file.ends_with("result.rs") {
                targets.insert(f.low_pc, kind::TRY);
            }
        }
        for (pc, target) in calls(elf, arch) {
            let Some(&k) = targets.get(&target) else {
                continue;
            };
            let Some(fl) = line_of(pc) else { continue };
            let e = at.entry(fl).or_insert((k, Vec::new()));
            if !e.1.contains(&pc) {
                e.1.push(pc);
            }
        }
    }
    // functions named for a retry, a recovery or a rollback: their first instruction
    for f in fns.iter().filter(|f| f.concrete && f.low_pc != 0) {
        if !paths::is_recovery_name(&f.name) {
            continue;
        }
        let Some(fi) = file_of(&f.file) else {
            continue;
        };
        let e = at
            .entry((fi, f.line))
            .or_insert((kind::RECOVERY, Vec::new()));
        if e.0 == kind::RECOVERY && !e.1.contains(&f.low_pc) {
            e.1.push(f.low_pc);
        }
    }
    // the function each line is written in: the last one declared at or before it in its file
    let mut decls: HashMap<usize, Vec<(u32, &str)>> = HashMap::new();
    for f in fns {
        if let Some(fi) = file_of(&f.file) {
            decls.entry(fi).or_default().push((f.line, f.name.as_str()));
        }
    }
    for v in decls.values_mut() {
        v.sort();
        v.dedup_by_key(|x| x.0);
    }
    let function_of = |fi: usize, line: u32| -> String {
        decls
            .get(&fi)
            .and_then(|v| {
                let i = v.partition_point(|x| x.0 <= line);
                i.checked_sub(1).map(|k| v[k].1.to_string())
            })
            .unwrap_or_default()
    };
    let mut out = Found {
        without_code,
        ..Found::default()
    };
    let mut pages: HashMap<u64, u32> = HashMap::new();
    let mut id = first_id;
    for ((fi, line), (k, mut addrs)) in at {
        if src[fi].is_removed(line) {
            continue;
        }
        addrs.sort_unstable();
        addrs.dedup();
        let mut sites = Vec::new();
        for a in addrs {
            let Some(site) = site_of(elf, a, &mut pages, &mut out.pages) else {
                continue;
            };
            sites.push(site);
        }
        if sites.is_empty() {
            continue;
        }
        out.places.push(PathPlace {
            id,
            kind: k.to_string(),
            file: files[fi].1.clone(),
            line,
            function: function_of(fi, line),
            sites,
            ..PathPlace::default()
        });
        id += 1;
    }
    out.seen = pages;
    out
}

/// Every function of the program with code of its own, each a place at its first instruction
/// (kind `function`, Milestone 394, docs/design/live-map.md), ids from 0. `seen` and `pages` are
/// the pages the error paths found; the functions' pages are added to them. A function the
/// compiler copied into every caller has no first instruction of its own and is no place; two
/// names at one address are one place, under the first name.
pub(super) fn functions(
    elf: &Elf,
    fns: &[FnInfo],
    files: &[(PathBuf, String)],
    seen: &mut HashMap<u64, u32>,
    pages: &mut Vec<PathPage>,
) -> Vec<PathPlace> {
    let mut all: Vec<String> = fns.iter().map(|f| f.file.clone()).collect();
    all.sort();
    all.dedup();
    let names = Names::of(&all, files);
    let mut list: Vec<&FnInfo> = fns
        .iter()
        .filter(|f| f.concrete && f.low_pc != 0 && f.high_pc > f.low_pc)
        .collect();
    list.sort_by(|a, b| a.low_pc.cmp(&b.low_pc).then(a.name.cmp(&b.name)));
    list.dedup_by_key(|f| f.low_pc);
    let mut out = Vec::with_capacity(list.len());
    for f in list {
        let Some(site) = site_of(elf, f.low_pc, seen, pages) else {
            continue;
        };
        let own = names.file_of(&f.file, files);
        out.push(PathPlace {
            id: out.len() as u32,
            kind: kind::FUNCTION.to_string(),
            file: match own {
                Some(i) => files[i].1.clone(),
                None => f.file.clone(),
            },
            line: f.line,
            function: f.name.clone(),
            sites: vec![site],
            size: f.high_pc - f.low_pc,
            module: paths::module_of(&f.name, f.lang == Lang::Go),
            own: own.is_some(),
            calls: Vec::new(),
        });
    }
    // the static call graph of the project's own code (Milestone 397's follow-up): each direct
    // call of an own function to another own function's first instruction
    if elf.machine == 0xb7 {
        let starts: HashMap<u64, u32> = out
            .iter()
            .filter(|p| p.own)
            .filter_map(|p| Some((p.sites.first()?.addr, p.id)))
            .collect();
        for p in out.iter_mut().filter(|p| p.own) {
            let Some(addr) = p.sites.first().map(|s| s.addr) else {
                continue;
            };
            let Some(code) = elf.bytes_at(addr, p.size as usize) else {
                continue;
            };
            let mut calls: Vec<u32> = paths::arm64_branch_targets(code, addr)
                .into_iter()
                .filter_map(|t| starts.get(&t).copied())
                .filter(|&t| t != p.id)
                .collect();
            calls.sort_unstable();
            calls.dedup();
            p.calls = calls;
        }
    }
    out
}

/// The site of an address: the file page that holds the instruction (added to `pages` once),
/// its offset there, and the instruction. None for an address outside the program's file or
/// on a page of zeros.
fn site_of(
    elf: &Elf,
    addr: u64,
    seen: &mut HashMap<u64, u32>,
    pages: &mut Vec<PathPage>,
) -> Option<PathSite> {
    let s = elf
        .sections
        .iter()
        .find(|s| s.flags & 4 != 0 && !s.nobits && s.addr <= addr && addr + 4 <= s.addr + s.size)?;
    let off = s.offset + (addr - s.addr);
    let word = u32::from_le_bytes(
        elf.data
            .get(off as usize..off as usize + 4)?
            .try_into()
            .ok()?,
    );
    let index = off / PAGE as u64;
    let page = match seen.get(&index) {
        Some(&p) => p,
        None => {
            let start = (index as usize) * PAGE;
            let mut b = vec![0u8; PAGE];
            let end = (start + PAGE).min(elf.data.len());
            b[..end - start].copy_from_slice(&elf.data[start..end]);
            let (probe_off, probe) = paths::page_probe(&b)?;
            pages.push(PathPage {
                index,
                probe_off,
                probe,
                hash: paths::page_hash(&b),
            });
            let p = (pages.len() - 1) as u32;
            seen.insert(index, p);
            p
        }
    };
    Some(PathSite {
        addr,
        page,
        off: (off % PAGE as u64) as u32,
        word,
    })
}

/// Every direct call in the program's code: (the call's address, its target).
fn calls(elf: &Elf, arch: &str) -> Vec<(u64, u64)> {
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
                out.push((pc, pc.wrapping_add(imm as u64)));
            }
        } else {
            for k in 0..code.len().saturating_sub(4) {
                if code[k] != 0xe8 {
                    continue;
                }
                let rel = i32::from_le_bytes([code[k + 1], code[k + 2], code[k + 3], code[k + 4]]);
                let pc = s.addr + k as u64;
                out.push((pc, (pc + 5).wrapping_add(rel as i64 as u64)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_source_folder_gets_its_own_folder_of_the_debug_information() {
        // NSQ with -trimpath: its own files under github.com/nsqio/nsq/, and the one file of the
        // disk queue it requires under the module's path with its version
        let files: Vec<(PathBuf, String)> = ["nsqd/topic.go", "nsqd/channel.go", "nsqd/guid.go"]
            .iter()
            .map(|r| (PathBuf::from(format!("/w/nsq/{r}")), r.to_string()))
            .chain([(
                PathBuf::from("/w/go-diskqueue/diskqueue.go"),
                "diskqueue.go".to_string(),
            )])
            .collect();
        let all: Vec<String> = [
            "github.com/nsqio/nsq/nsqd/topic.go",
            "github.com/nsqio/nsq/nsqd/channel.go",
            "github.com/nsqio/nsq/nsqd/guid.go",
            "github.com/nsqio/go-diskqueue@v1.1.0/diskqueue.go",
            "github.com/other/lib@v1.0.0/nsqd/topic.go",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let n = Names::of(&all, &files);
        assert_eq!(n.file_of(&all[0], &files), Some(0));
        assert_eq!(n.file_of(&all[3], &files), Some(3));
        // a dependency's file with a source file's relative path is not that file
        assert_eq!(n.file_of(&all[4], &files), None);
        // one folder alone: the folder that names most files, as before
        let one = Names::of(&all, &files[..3]);
        assert_eq!(one.file_of(&all[3], &files[..3]), None);
        assert_eq!(one.file_of(&all[1], &files[..3]), Some(1));
    }
}
