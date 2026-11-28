//! Error paths (Milestone 306, docs/design/error-paths.md): which error-handling places of a
//! program ran under hydo's faults.
//!
//! A *place* is the first instruction of a piece of error handling: the body of a Go `if err !=
//! nil` block, a Rust `return Err(...)`, a `match` arm on `Err`, the error branch of a `?`, the
//! failure of an `unwrap()` or a `panic!`, or a function named for a retry, a recovery or a
//! rollback. The compiler ([`crate::compile`]) finds the places in the sources and the program's
//! debug information; a `// hydo: error path` line adds the next line, and a `// hydo: no error
//! path` line removes the next line, the block it opens, or the function it declares.
//!
//! The engine watches each place once per run: it writes a breakpoint instruction over the
//! place's first instruction in the guest's memory while the guest runs, and at its first hit puts
//! the instruction back and lets the guest go on. The guest runs the same instructions, and the
//! engine counts no exit for the stop, so a run with the plan has the fingerprint of the run
//! without it. The engine writes the first hit of each place, `{"paths":...}`, into the run's
//! records (`<tag>.contracts.jsonl`); the host reads it with [`read_hits`].

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The kinds of place, as the plan and the reports name them.
pub mod kind {
    /// Go: the body of an `if err != nil` block.
    pub const ERR_CHECK: &str = "err-check";
    /// Rust: `return Err(...)`, or an `Err(...)` that a function or an arm gives back.
    pub const ERR_RETURN: &str = "err-return";
    /// Rust: a `match` arm on `Err(..)`, or the body of an `if let Err(..)`.
    pub const ERR_ARM: &str = "err-arm";
    /// Rust: the error branch of a `?` on a `Result`.
    pub const TRY: &str = "try";
    /// Rust: the failure of an `unwrap()` or an `expect()`.
    pub const UNWRAP: &str = "unwrap";
    /// Rust: a `panic!`, `unreachable!` or a failed `assert!`.
    pub const PANIC: &str = "panic";
    /// A function whose name has `retry`, `recover` or `rollback`: its first instruction.
    pub const RECOVERY: &str = "recovery";
    /// A line after `// hydo: error path`.
    pub const DECLARED: &str = "declared";
    /// The first instruction of a function: the live map's watch of every function
    /// (`all_functions: true`, Milestone 394, docs/design/live-map.md). Such places are in
    /// [`crate::plan::Program::fn_places`], never among the error paths.
    pub const FUNCTION: &str = "function";
}

/// The words of a kind in a report.
pub fn kind_words(k: &str) -> &'static str {
    match k {
        kind::ERR_CHECK => "the body of an `if err != nil`",
        kind::ERR_RETURN => "an `Err` given back",
        kind::ERR_ARM => "an arm on `Err`",
        kind::TRY => "the error branch of a `?`",
        kind::UNWRAP => "a failed `unwrap` or `expect`",
        kind::PANIC => "a panic",
        kind::RECOVERY => "a retry, recovery or rollback function",
        kind::DECLARED => "a line marked `// hydo: error path`",
        kind::FUNCTION => "a function's first instruction",
        _ => "an error path",
    }
}

/// One error-handling place of a program.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PathPlace {
    /// Unique in the plan: the hits name it.
    pub id: u32,
    /// One of [`kind`].
    pub kind: String,
    /// The source file, relative to the source folder.
    pub file: String,
    /// The line whose code the place is.
    pub line: u32,
    /// The function the line is written in (its name in the debug information).
    pub function: String,
    /// The addresses of the place's first instruction: one for each copy of the code (a
    /// function the compiler copied into its callers has a copy in each).
    pub sites: Vec<PathSite>,
    /// A function's place (kind `function`, Milestone 394): the size of its code in bytes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size: u64,
    /// A function's place: its module (a Go package's import path, a Rust crate and module),
    /// for the live map's grouping.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub module: String,
    /// A function's place: the function is in the sources that the manifest names (the
    /// project's own code), not in a dependency or the language's runtime.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own: bool,
}

/// One address of a place, and where its instruction is in the program's file.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PathSite {
    pub addr: u64,
    /// The program's file page that holds the instruction ([`Program::path_pages`] index).
    ///
    /// [`Program::path_pages`]: crate::plan::Program::path_pages
    pub page: u32,
    /// The instruction's offset in that page.
    pub off: u32,
    /// The instruction (4 bytes, little-endian), which the engine puts back after a hit.
    pub word: u32,
}

/// A page (4 KiB) of the program's file that holds a place. The guest's kernel keeps a file of a
/// root in memory in pages of the file; the engine finds each of them in guest memory by its
/// first nonzero 8 bytes (`probe` at `probe_off`) and the hash of all 4,096 bytes
/// ([`page_hash`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PathPage {
    /// The page's offset in the file, divided by 4,096.
    pub index: u64,
    pub probe_off: u32,
    pub probe: u64,
    pub hash: u64,
}

/// The size of a page that [`PathPage`] describes.
pub const PAGE: usize = 4096;

/// The immediate of the breakpoint instruction the Mac engine writes (`BRK #0x4859`): a
/// breakpoint with another immediate is the guest's own.
pub const BRK_IMM: u32 = 0x4859;

/// The arm64 instruction `BRK #imm`.
pub fn brk_word(imm: u32) -> u32 {
    0xd420_0000 | ((imm & 0xffff) << 5)
}

/// FNV-1a over the bytes, 64 bits: a page's hash.
pub fn page_hash(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A page's probe: the first 8-byte word that is not zero, and its offset (none: an empty page).
pub fn page_probe(b: &[u8]) -> Option<(u32, u64)> {
    b.chunks_exact(8).enumerate().find_map(|(i, w)| {
        let v = u64::from_le_bytes(w.try_into().ok()?);
        (v != 0).then_some((8 * i as u32, v))
    })
}

/// The first hit of a place in a run: the place, the guest clock (the engine's), and the boot (0:
/// the run's first; each power cut or reboot starts the next).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hit {
    pub place: u32,
    pub at: u64,
    pub boot: u32,
}

/// What an engine wrote about the error paths of a run.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hits {
    /// `[place, at, boot]` for each place that ran, in the order of the first hits.
    pub hits: Vec<(u32, u64, u32)>,
    /// The places the engine could not watch: it found none of their code in guest memory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unwatched: Vec<u32>,
    /// The places the engine stopped watching before they ran (Milestone 394): the guest read
    /// or wrote the page that holds their instruction, as a program does that reads its own
    /// file, so the instruction went back for the rest of the boot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<u32>,
    /// How many such reads of a page came.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reads: u64,
    /// What watching cost the engine: its stops at a place (one per place and run, plus a
    /// place that a recovery boot runs again), and the wall time of its searches of guest
    /// memory, in microseconds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub stops: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub scan_us: u64,
    /// The functions' line only (Milestone 397, docs/design/live-map.md): for each hit, in the
    /// order of `hits`, the return address (the link register, x30 on arm64) that the engine
    /// read at the hit's stop: it names the function that called this one first in the run.
    /// The engine only reads the register, so no fingerprint moves. Empty for the error paths'
    /// line and for an engine that does not read it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lr: Vec<u64>,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[derive(Serialize, Deserialize)]
struct HitsLine {
    paths: Hits,
}

/// The functions' line (Milestone 394): the same shape, for the places of
/// [`crate::plan::Program::fn_places`].
#[derive(Serialize, Deserialize)]
struct FnHitsLine {
    functions: Hits,
}

impl Hits {
    /// The hits as [`Hit`] values.
    pub fn list(&self) -> Vec<Hit> {
        self.hits
            .iter()
            .map(|&(place, at, boot)| Hit { place, at, boot })
            .collect()
    }
}

/// The line an engine writes into a run's records.
pub fn hits_line(h: &Hits) -> String {
    let mut s = serde_json::to_string(&HitsLine { paths: h.clone() }).expect("hits serialize");
    s.push('\n');
    s
}

/// Whether a records line is the error paths' line, or the functions' line (Milestone 394).
pub fn is_hits_line(l: &str) -> bool {
    l.starts_with("{\"paths\"") || is_functions_line(l)
}

/// The line an engine writes into a run's records for the functions that ran (Milestone 394).
pub fn functions_line(h: &Hits) -> String {
    let mut s = serde_json::to_string(&FnHitsLine {
        functions: h.clone(),
    })
    .expect("hits serialize");
    s.push('\n');
    s
}

/// Whether a records line is the functions' line.
pub fn is_functions_line(l: &str) -> bool {
    l.starts_with("{\"functions\"")
}

/// The functions that ran in a run, from its records: None when the file has no such line (no
/// plan of every function, or an engine that does not watch them).
pub fn read_function_hits(p: &Path) -> Result<Option<Hits>, String> {
    let Ok(t) = std::fs::read_to_string(p) else {
        return Ok(None);
    };
    match t.lines().find(|l| is_functions_line(l)) {
        Some(l) => {
            let h: FnHitsLine =
                serde_json::from_str(l).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok(Some(h.functions))
        }
        None => Ok(None),
    }
}

/// The error paths of a run's records: None when the file has no such line (no plan of error
/// paths, or an engine that does not watch them).
pub fn read_hits(p: &Path) -> Result<Option<Hits>, String> {
    let Ok(t) = std::fs::read_to_string(p) else {
        return Ok(None);
    };
    match t.lines().find(|l| l.starts_with("{\"paths\"")) {
        Some(l) => {
            let h: HitsLine =
                serde_json::from_str(l).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok(Some(h.paths))
        }
        None => Ok(None),
    }
}

// ---------------------------------------------------------------- the sources

/// A `// hydo:` line about error paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decl {
    /// `// hydo: error path`: the next line of code is a place.
    Add,
    /// `// hydo: no error path`: the next line of code, the block it opens or the function it
    /// declares has no place.
    Remove,
}

/// The declaration a comment's text after `// hydo:` makes, if it is one.
pub fn decl_of(text: &str) -> Option<Decl> {
    let w: Vec<&str> = text.split_whitespace().collect();
    match w.as_slice() {
        ["error", "path"] => Some(Decl::Add),
        ["no", "error", "path"] => Some(Decl::Remove),
        _ => None,
    }
}

/// A place the sources show, before the debug information gives its addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrcPlace {
    pub kind: &'static str,
    /// The line whose code is the place (1-based).
    pub line: u32,
}

/// The places of one source file, and the lines a `// hydo: no error path` removes (inclusive
/// ranges, 1-based).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SrcPaths {
    pub places: Vec<SrcPlace>,
    pub removed: Vec<(u32, u32)>,
}

impl SrcPaths {
    /// Whether a line is in a removed range.
    pub fn is_removed(&self, line: u32) -> bool {
        self.removed.iter().any(|&(a, b)| a <= line && line <= b)
    }
}

/// The language of a source file, by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrcLang {
    Go,
    Rust,
}

impl SrcLang {
    pub fn of(file: &str) -> Option<SrcLang> {
        if file.ends_with(".go") && !file.ends_with("_test.go") {
            Some(SrcLang::Go)
        } else if file.ends_with(".rs") {
            Some(SrcLang::Rust)
        } else {
            None
        }
    }
}

/// The code of each line: comments removed, and the contents of string and character literals
/// replaced by spaces (so a `{` or `//` inside a string is no code).
pub fn code_lines(text: &str, lang: SrcLang) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut in_raw = false; // Go: a raw string across lines
    for l in text.lines() {
        let c: Vec<char> = l.chars().collect();
        let mut s = String::with_capacity(c.len());
        let mut i = 0;
        while i < c.len() {
            if in_block {
                if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    in_block = false;
                    i += 2;
                    s.push_str("  ");
                } else {
                    s.push(' ');
                    i += 1;
                }
                continue;
            }
            if in_raw {
                if c[i] == '`' {
                    in_raw = false;
                    s.push('`');
                } else {
                    s.push(' ');
                }
                i += 1;
                continue;
            }
            match c[i] {
                '/' if c.get(i + 1) == Some(&'/') => break,
                '/' if c.get(i + 1) == Some(&'*') => {
                    in_block = true;
                    s.push_str("  ");
                    i += 2;
                }
                '`' if lang == SrcLang::Go => {
                    in_raw = true;
                    s.push('`');
                    i += 1;
                }
                '"' => {
                    s.push('"');
                    i += 1;
                    while i < c.len() && c[i] != '"' {
                        if c[i] == '\\' {
                            s.push(' ');
                            i += 1;
                        }
                        if i < c.len() {
                            s.push(' ');
                            i += 1;
                        }
                    }
                    if i < c.len() {
                        s.push('"');
                        i += 1;
                    }
                }
                '\'' => {
                    // a character literal ('x', '\n'); a Rust lifetime ('a) has no closing quote
                    let close = if c.get(i + 1) == Some(&'\\') {
                        (i + 2..c.len().min(i + 12)).find(|&j| c[j] == '\'')
                    } else if c.get(i + 2) == Some(&'\'') {
                        Some(i + 2)
                    } else {
                        None
                    };
                    match close {
                        Some(j) => {
                            s.push('\'');
                            s.extend(std::iter::repeat_n(' ', j - i - 1));
                            s.push('\'');
                            i = j + 1;
                        }
                        None => {
                            s.push('\'');
                            i += 1;
                        }
                    }
                }
                ch => {
                    s.push(ch);
                    i += 1;
                }
            }
        }
        out.push(s);
    }
    out
}

/// The next line at or after `from` (0-based) whose code is not blank.
fn next_code(code: &[String], from: usize) -> Option<usize> {
    (from..code.len()).find(|&j| !code[j].trim().is_empty())
}

/// The last line (0-based) of the block that the first `{` at or after line `from` opens.
fn block_end(code: &[String], from: usize) -> usize {
    let mut depth = 0i64;
    let mut opened = false;
    for (j, l) in code.iter().enumerate().skip(from) {
        for ch in l.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
            if opened && depth <= 0 {
                return j;
            }
        }
    }
    code.len().saturating_sub(1)
}

/// Whether a Go condition compares an error with nil: `err != nil`, `txErr != nil`, `rerr !=
/// nil`.
fn err_not_nil(t: &str) -> bool {
    let mut rest = t;
    while let Some(p) = rest.find("!= nil") {
        let before = rest[..p].trim_end();
        let id: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let low = id.to_ascii_lowercase();
        if low == "err" || low.starts_with("err") || low.ends_with("err") || low.ends_with("error")
        {
            return true;
        }
        rest = &rest[p + 6..];
    }
    false
}

/// The error-handling places of a source file, and its `// hydo: error path` lines.
pub fn scan(text: &str, lang: SrcLang) -> SrcPaths {
    let raw: Vec<&str> = text.lines().collect();
    let code = code_lines(text, lang);
    let mut out = SrcPaths::default();
    let add = |kind: &'static str, j: usize, out: &mut SrcPaths| {
        let line = j as u32 + 1;
        if !out.places.iter().any(|p| p.line == line) {
            out.places.push(SrcPlace { kind, line });
        }
    };
    // the declarations first, so that a declared line keeps its kind
    for (i, l) in raw.iter().enumerate() {
        let Some(t) = l.trim_start().strip_prefix("// hydo:") else {
            continue;
        };
        let Some(d) = decl_of(t) else { continue };
        let Some(j) = next_code(&code, i + 1) else {
            continue;
        };
        match d {
            Decl::Add => add(kind::DECLARED, j, &mut out),
            Decl::Remove => {
                let t = code[j].trim();
                let end = if crate::lang::declared_function(t).is_some() || t.ends_with('{') {
                    block_end(&code, j)
                } else {
                    j
                };
                out.removed.push((j as u32 + 1, end as u32 + 1));
            }
        }
    }
    for i in 0..code.len() {
        let t = code[i].trim();
        let body = |code: &[String]| -> Option<usize> {
            let j = next_code(code, i + 1)?;
            (!code[j].trim().starts_with('}')).then_some(j)
        };
        match lang {
            SrcLang::Go => {
                let is_if = t.starts_with("if ") || t.starts_with("} else if ");
                if is_if && t.ends_with('{') && err_not_nil(t) {
                    if let Some(j) = body(&code) {
                        add(kind::ERR_CHECK, j, &mut out);
                    }
                }
            }
            SrcLang::Rust => {
                let arm = t.starts_with("Err(") || t.starts_with("| Err(");
                if arm && t.contains("=>") {
                    let rest = t.split_once("=>").map(|x| x.1.trim()).unwrap_or("");
                    if rest.is_empty() || rest == "{" {
                        if let Some(j) = body(&code) {
                            add(kind::ERR_ARM, j, &mut out);
                        }
                    } else {
                        add(kind::ERR_ARM, i, &mut out);
                    }
                } else if (t.starts_with("if let Err(")
                    || t.starts_with("} else if let Err(")
                    || t.starts_with("while let Err("))
                    && t.ends_with('{')
                {
                    if let Some(j) = body(&code) {
                        add(kind::ERR_ARM, j, &mut out);
                    }
                } else if t.contains("return Err(")
                    || t.contains("=> Err(")
                    || t.starts_with("Err(")
                {
                    add(kind::ERR_RETURN, i, &mut out);
                }
            }
        }
    }
    out.places.sort_by_key(|p| p.line);
    out
}

/// Whether a function's name says it retries, recovers or rolls back (its last part, without
/// the package or the type).
pub fn is_recovery_name(name: &str) -> bool {
    let last = name
        .rsplit("::")
        .next()
        .unwrap_or(name)
        .rsplit('.')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    // `{closure#0}` and Go's `func1` are no names of their own
    ["retry", "recover", "rollback"]
        .iter()
        .any(|w| last.contains(w))
}

/// The short name of a function for a report: Go's import path before the package left out
/// (`github.com/blnkfinance/blnk.(*Blnk).X` is `blnk.(*Blnk).X`).
pub fn short_function(name: &str) -> &str {
    match name.find(".(").or_else(|| name.rfind('.')) {
        Some(dot) => match name[..dot].rfind('/') {
            Some(slash) => &name[slash + 1..],
            None => name,
        },
        None => name,
    }
}

/// The module of a function for the live map (Milestone 394): a Go function's package import
/// path (`github.com/blnkfinance/blnk/internal/metrics.Init` is
/// `github.com/blnkfinance/blnk/internal/metrics`), a Rust function's path without its last
/// part (`redb::tree_store::btree::Btree::get` is `redb::tree_store::btree::Btree`). A name of
/// neither shape is its own module.
pub fn module_of(name: &str, go: bool) -> String {
    if go {
        // generated helpers (`type:.eq.<T>`) and generic instances (`pkg.F[...]`)
        let n = name.split('[').next().unwrap_or(name);
        let after_slash = n.rfind('/').map_or(0, |i| i + 1);
        return match n[after_slash..].find('.') {
            Some(dot) => n[..after_slash + dot].to_string(),
            None => n.to_string(),
        };
    }
    // Rust: split at `::` outside of angle brackets
    let mut depth = 0i32;
    let b = name.as_bytes();
    let mut last = None;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b':' if depth == 0 && b.get(i + 1) == Some(&b':') => {
                last = Some(i);
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    match last {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}
