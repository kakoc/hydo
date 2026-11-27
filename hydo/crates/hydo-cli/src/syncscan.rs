//! `hydo rules scan --kind sync` (Milestone 381, tentative number; docs/design/rule-suggestion.md,
//! "The sync audit"): the writes that a system answers before they reach the disk, and a power
//! cut aimed at each.
//!
//! About a third of hydo's findings before this milestone had one shape: a system acknowledged a
//! write, or finished a state transfer, before the data or a rename was synced, and a power cut
//! or one node's power loss lost it. NSQ's disk queue wrote its metadata only at its first sync
//! and renamed it without a sync of the folder; Kvrocks wrote a fetched checkpoint without a sync
//! and destroyed its old database; RabbitMQ's stream segments, Meilisearch's payloads and dumps,
//! SWUpdate's raw images and Garage's peer list were never synced.
//!
//! The audit reads text, without a compiler, with the clock audit's rules for test code and its
//! map of the code that hydo.yaml's services run (`clockscan`, `clockmap`). It reads Go, C and
//! C++, Rust, Java and Kotlin, Python, JavaScript and TypeScript, Ruby, C#, Erlang, and SQL for
//! settings. Each function is read as a list of events in line order:
//!
//! - a file opened for writing (`os.OpenFile(.., O_CREATE)`, `File::create`, `open(.., O_WRONLY)`,
//!   `fopen(.., "w")`, `new FileOutputStream`, a project's own helper that opens one), and the
//!   variable or field that holds it;
//! - a write to such a file (`f.Write(`, `write_all`, `->Append(`, `<<`, the file passed to a
//!   writer or a copy), or a whole-file write (`os.WriteFile`, `fs::write`, `file:write`);
//! - a sync (`Sync()`, `sync_all`, `fsync`, `fdatasync`, `sync_file_range`, `O_DSYNC`,
//!   `F_FULLFSYNC`, `FileChannel.force`, `file:sync`, `Flush(true)`, or a project's own helper
//!   whose name says sync or flush and which syncs);
//! - a rename (`os.Rename`, `fs::rename`, `rename(2)`, `Files.move`, tempfile's `persist`,
//!   `file:rename`, or a project's helper that renames);
//! - an acknowledgement (a reply, a response, an ack, a confirm, a commit, a task marked
//!   succeeded), and a removal of old data (`DestroyDB`, `RemoveAll`, `unlink`).
//!
//! A write is covered by a sync later in the function at its own depth or above it; a sync inside
//! a condition that the write is not in does not cover it, and the report says so. A function
//! whose every caller syncs after the call is covered too. A rename is covered by a sync after it
//! (of the folder). What is left is a place, one per function, with what is missing: a sync of
//! the file, a sync of the folder after the rename, a sync before the answer. The answer is
//! searched in the function after the write and in its callers by name, three calls deep.
//!
//! Four more shapes are read line by line: a C++ stream (or a buffered writer) written and synced
//! while its buffer may still hold the data (Mender's version file); `O_DIRECT` without
//! `O_DSYNC` or a sync in the file (Garnet's default device); a setting that turns syncing off by
//! default or syncs on an interval (SQLite's WAL mode without `synchronous=FULL`, RocksDB's
//! `sync = false`, ClickHouse's `fsync_after_insert`, NSQ's `SyncEvery`); and a loop that syncs or
//! saves on a timer (Garage's peer list, NSQ's sync ticker).

use crate::clockscan::{self, Lang, Services, TestItems};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

// ---------------------------------------------------------------- the result

/// How an acknowledgement was reached from a place.
#[derive(Clone, Debug, PartialEq)]
pub struct Ack {
    /// The functions from the place to the acknowledgement, each with its file and line:
    /// `ioLoop (diskqueue.go:696)`. Empty when the place's own function acknowledges.
    pub path: Vec<String>,
    pub file: String,
    pub line: usize,
    pub text: String,
    /// `answer` (a reply, a response, an ack, a commit) or `removal` (old data removed after it).
    pub kind: &'static str,
}

/// One place where data can be acknowledged before it is on the disk.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncPlace {
    pub file: String,
    pub line: usize,
    pub lang: &'static str,
    pub text: String,
    /// `write`, `rename`, `stream`, `direct`, `setting`, `interval`, `periodic`.
    pub shape: &'static str,
    pub function: Option<String>,
    /// What the code does, in one sentence.
    pub what: String,
    /// What is missing: `a sync of the file`, `a sync of the folder after the rename`, ...
    pub missing: Vec<String>,
    pub ack: Option<Ack>,
    /// A note on the syncs that are there: a sync inside a condition, a sync on a timer.
    pub notes: Vec<String>,
    /// The window of a periodic sync, in seconds, when the code states it.
    pub window: Option<f64>,
    pub confidence: &'static str,
    pub main_lang: bool,
}

/// What the audit found.
pub struct SyncAudit {
    pub read: usize,
    pub places: Vec<SyncPlace>,
    pub tests: TestItems,
}

// ---------------------------------------------------------------- files

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum L {
    Code(Lang),
    Erlang,
}

impl L {
    fn name(self) -> &'static str {
        match self {
            L::Code(l) => l.name(),
            L::Erlang => "erlang",
        }
    }
    fn is(self, l: Lang) -> bool {
        self == L::Code(l)
    }
}

struct SFile {
    rel: String,
    lang: L,
    raw: Vec<String>,
    /// Each line without comments and without the contents of strings; test code is empty.
    code: Vec<String>,
    /// Each line without comments, with the contents of strings; test code is empty.
    text: Vec<String>,
    /// (name, first line, last line), 0-based.
    funcs: Vec<(String, usize, usize)>,
    /// The innermost function of each line.
    func_of: Vec<Option<usize>>,
    /// The nesting depth at the start of each line.
    depth: Vec<i32>,
    /// Whether the file uses the tempfile crate, whose `persist` is a rename.
    tempfile: bool,
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Erlang without comments: `%` starts one outside a string. `keep` keeps the strings' contents.
fn erlang_clean(text: &str, keep: bool) -> Vec<String> {
    text.split('\n')
        .map(|l| {
            let mut out = String::new();
            let mut in_str = false;
            let mut chars = l.chars().peekable();
            while let Some(c) = chars.next() {
                if in_str {
                    if c == '\\' {
                        if keep {
                            out.push(c);
                            out.extend(chars.next());
                        } else {
                            chars.next();
                        }
                        continue;
                    }
                    if c == '"' {
                        in_str = false;
                        out.push(c);
                    } else if keep {
                        out.push(c);
                    }
                    continue;
                }
                match c {
                    '%' => break,
                    '"' => {
                        in_str = true;
                        out.push(c);
                    }
                    '$' => {
                        // a character literal: `$%` is no comment
                        out.push(c);
                        out.extend(chars.next());
                    }
                    _ => out.push(c),
                }
            }
            out
        })
        .collect()
}

/// An Erlang function's head: a line at column 0 that starts with an atom and a parenthesis.
fn erlang_head(l: &str) -> Option<String> {
    let first = l.chars().next()?;
    if !first.is_ascii_lowercase() {
        return None;
    }
    let name: String = l.chars().take_while(|c| is_ident(*c)).collect();
    l[name.len()..].starts_with('(').then_some(name)
}

fn erlang_funcs(code: &[String]) -> Vec<(String, usize, usize)> {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    for (i, l) in code.iter().enumerate() {
        if l.starts_with('-') {
            if let Some(last) = out.last_mut() {
                if last.2 == usize::MAX {
                    last.2 = i.saturating_sub(1);
                }
            }
            continue;
        }
        if let Some(n) = erlang_head(l) {
            match out.last_mut() {
                Some(last) if last.0 == n && last.2 == usize::MAX => {}
                Some(last) => {
                    if last.2 == usize::MAX {
                        last.2 = i.saturating_sub(1);
                    }
                    out.push((n, i, usize::MAX));
                }
                None => out.push((n, i, usize::MAX)),
            }
        }
    }
    if let Some(last) = out.last_mut() {
        if last.2 == usize::MAX {
            last.2 = code.len().saturating_sub(1);
        }
    }
    out
}

/// C++ raw strings (`R"(...)"`, `R"delim(...)delim"`) hold documentation in some projects
/// (ClickHouse's settings): the lines inside one are blanked.
fn blank_raw_strings(
    raw: &[String],
    mut code: Vec<String>,
    mut kept: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let mut close: Option<String> = None;
    for (i, l) in raw.iter().enumerate() {
        let mut from = 0;
        let mut inside_from_start = close.is_some();
        loop {
            match &close {
                Some(end) => match l[from..].find(end.as_str()) {
                    Some(p) => {
                        from += p + end.len();
                        close = None;
                        inside_from_start = false;
                    }
                    None => {
                        if inside_from_start {
                            if let Some(c) = code.get_mut(i) {
                                c.clear();
                            }
                            if let Some(k) = kept.get_mut(i) {
                                k.clear();
                            }
                        }
                        break;
                    }
                },
                None => match l[from..].find("R\"") {
                    Some(p) => {
                        let start = from + p + 2;
                        let Some(open) = l[start..].find('(') else {
                            break;
                        };
                        let delim = &l[start..start + open];
                        if delim.len() > 16 || delim.contains(' ') {
                            break;
                        }
                        close = Some(format!("){delim}\""));
                        from = start + open + 1;
                    }
                    None => break,
                },
            }
        }
    }
    (code, kept)
}

fn load(rel: String, lang: L, text: &str) -> (SFile, TestItems) {
    let raw: Vec<String> = text
        .split('\n')
        .map(|s| s.trim_end_matches('\r').to_string())
        .collect();
    let (code, kept, funcs, tests) = match lang {
        L::Code(l) => {
            let o = clockscan::outline(l, text);
            let bare = clockscan::clean(l, text);
            let code: Vec<String> = bare
                .iter()
                .zip(o.code.iter())
                .map(|(b, k)| {
                    if k.is_empty() {
                        String::new()
                    } else {
                        b.clone()
                    }
                })
                .collect();
            (code, o.code, o.funcs, o.tests)
        }
        L::Erlang => {
            let code = erlang_clean(text, false);
            let kept = erlang_clean(text, true);
            let funcs = erlang_funcs(&code);
            (code, kept, funcs, TestItems::default())
        }
    };
    let (code, kept) = if lang.is(Lang::C) {
        blank_raw_strings(&raw, code, kept)
    } else {
        (code, kept)
    };
    let n = code.len();
    let mut func_of = vec![None; n];
    for (fi, f) in funcs.iter().enumerate() {
        for slot in func_of
            .iter_mut()
            .take(f.2.min(n.saturating_sub(1)) + 1)
            .skip(f.1)
        {
            // the innermost: a later function that starts inside an earlier one wins
            *slot = Some(fi);
        }
    }
    let mut depth = vec![0i32; n];
    match lang {
        L::Code(Lang::Python) | L::Code(Lang::Ruby) => {
            let mut last = 0i32;
            for (i, l) in code.iter().enumerate() {
                if !l.trim().is_empty() {
                    last = (l.chars().take_while(|c| c.is_whitespace()).count() / 2) as i32;
                }
                depth[i] = last;
            }
        }
        L::Erlang | L::Code(Lang::Sql) => {}
        L::Code(_) => {
            let mut d = 0i32;
            for (i, l) in code.iter().enumerate() {
                // a line that starts by closing a block is at the outer depth
                let lead = l.trim_start().starts_with('}');
                depth[i] = if lead { d - 1 } else { d };
                for c in l.chars() {
                    match c {
                        '{' => d += 1,
                        '}' => d -= 1,
                        _ => {}
                    }
                }
            }
        }
    }
    (
        SFile {
            rel,
            lang,
            raw,
            code,
            text: kept,
            funcs,
            func_of,
            depth,
            tempfile: text.contains("NamedTempFile") || text.contains("tempfile::"),
        },
        tests,
    )
}

// ---------------------------------------------------------------- words

/// The words of names: `persistMetaData` and `write_chunk` give `persist`, `meta`, `data`,
/// `write`, `chunk`.
pub fn name_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn has_word(s: &str, set: &[&str]) -> bool {
    name_words(s).iter().any(|w| set.contains(&w.as_str()))
}

/// The positions of `tok` in `s` as a whole identifier.
fn token_at(s: &str, tok: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = s[from..].find(tok) {
        let at = from + p;
        let before = at == 0 || !is_ident(b[at - 1] as char);
        let end = at + tok.len();
        let after = end >= b.len() || !is_ident(b[end] as char);
        if before && after {
            out.push(at);
        }
        from = at + tok.len().max(1);
    }
    out
}

fn has_token(s: &str, tok: &str) -> bool {
    !token_at(s, tok).is_empty()
}

/// The calls of a line: each name followed by `(`, with the qualified name before it
/// (`os.Rename`, `env_->RenameFile`, `file:write`, `BufWriter::new`) and the position of `(`.
fn calls(code: &str) -> Vec<(String, String, usize)> {
    let b: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '(' && i > 0 {
            // the name before it, past spaces only for C-style `foo (`
            let mut j = i;
            while j > 0 && b[j - 1] == ' ' {
                j -= 1;
            }
            let end = j;
            while j > 0 && is_ident(b[j - 1]) {
                j -= 1;
            }
            if j < end {
                let name: String = b[j..end].iter().collect();
                // the qualifier: a chain of `a.b`, `a->b`, `a::b`, `a:b`
                let mut k = j;
                loop {
                    let two = k >= 2
                        && ((b[k - 1] == '>' && b[k - 2] == '-')
                            || (b[k - 1] == ':' && b[k - 2] == ':'));
                    let sep = if two {
                        2
                    } else if k >= 1 && (b[k - 1] == '.' || b[k - 1] == ':') {
                        1
                    } else {
                        0
                    };
                    if sep == 0 {
                        break;
                    }
                    let mut m = k - sep;
                    // a call's result or an expression before the separator: keep the
                    // separator, so that `x().rename(` reads `.rename`
                    if m > 0 && (b[m - 1] == ')' || b[m - 1] == ']') {
                        k -= sep;
                        break;
                    }
                    let e = m;
                    while m > 0 && is_ident(b[m - 1]) {
                        m -= 1;
                    }
                    if m == e {
                        k -= sep;
                        break;
                    }
                    k = m;
                }
                let qual: String = b[k..end].iter().collect();
                if !name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    out.push((name, qual, i));
                }
            }
        }
        i += 1;
    }
    out
}

/// Words of control flow and declarations that look like calls.
const NOT_CALLS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "return",
    "match",
    "sizeof",
    "defined",
    "elif",
    "foreach",
    "using",
    "lock",
    "when",
    "with",
    "fn",
    "func",
    "def",
    "function",
    "case",
    "and",
    "or",
    "not",
    "in",
    "new",
    "typeof",
    "decltype",
    "static_assert",
    "assert",
    "Some",
    "Ok",
    "Err",
    "await",
];

// ---------------------------------------------------------------- the vocabulary

/// Whether a line syncs a file's data (or opens it so that every write is synced).
fn sync_prim(lang: L, code: &str, text: &str) -> bool {
    let any = |pats: &[&str]| pats.iter().any(|p| code.contains(p));
    match lang {
        L::Code(Lang::Go) => {
            any(&[
                ".Sync()",
                "Fsync(",
                "Fdatasync(",
                "SyncFileRange(",
                "O_SYNC",
                "O_DSYNC",
            ]) || code.contains("unix.Sync(")
                || code.contains("syscall.Sync(")
        }
        L::Code(Lang::Rust) => any(&[
            ".sync_all(",
            ".sync_data(",
            "fsync(",
            "fdatasync(",
            "sync_file_range(",
            "syncfs(",
            "O_SYNC",
            "O_DSYNC",
        ]),
        L::Code(Lang::C) => {
            any(&[
                "fsync(",
                "fdatasync(",
                "sync_file_range(",
                "F_FULLFSYNC",
                "O_SYNC",
                "O_DSYNC",
                "syncfs(",
                "->Sync(",
                "->Fsync(",
                "->RangeSync(",
                ".Sync(",
                ".Fsync(",
                "FlushFileBuffers(",
                "msync(",
            ]) || has_token(code, "sync") && code.contains("sync()")
        }
        L::Code(Lang::Jvm) => {
            any(&[".force(", "getFD().sync()"])
                || has_token(code, "SYNC")
                || has_token(code, "DSYNC")
                || text.contains("\"rws\"")
                || text.contains("\"rwd\"")
        }
        L::Code(Lang::Python) => any(&[
            "os.fsync(",
            "os.fdatasync(",
            "os.sync(",
            "O_SYNC",
            "O_DSYNC",
        ]),
        L::Code(Lang::Js) => any(&[
            "fsync(",
            "fsyncSync(",
            "fdatasync(",
            "fdatasyncSync(",
            ".sync()",
            ".datasync()",
        ]),
        L::Code(Lang::Ruby) => any(&[".fsync", ".fdatasync", "File::SYNC"]),
        L::Code(Lang::CSharp) => any(&[
            ".Flush(true)",
            "WriteThrough",
            "FlushFileBuffers(",
            "fsync(",
            "FlushToDisk",
        ]),
        L::Erlang => {
            any(&[
                "file:sync(",
                "file:datasync(",
                "prim_file:sync(",
                "prim_file:datasync(",
            ]) || (code.contains("file:open(")
                && (has_token(code, "sync") || has_token(code, "datasync")))
        }
        L::Code(Lang::Sql | Lang::Erlang | Lang::Elixir) => false,
    }
}

/// Whether a line renames a file (or moves it into place).
fn rename_prim(f: &SFile, code: &str) -> bool {
    let any = |pats: &[&str]| pats.iter().any(|p| code.contains(p));
    match f.lang {
        L::Code(Lang::Go) => code.contains("os.Rename("),
        L::Code(Lang::Rust) => {
            any(&["fs::rename(", "persist_noclobber("])
                || (code.contains(".persist(") && f.tempfile)
        }
        L::Code(Lang::C) => {
            any(&[
                "renameat(",
                "renameat2(",
                "->RenameFile(",
                ".RenameFile(",
                "MoveFileEx",
            ]) || calls(code).iter().any(|(n, q, _)| {
                n == "rename"
                    && matches!(
                        q.as_str(),
                        "rename"
                            | "::rename"
                            | "std::rename"
                            | "fs::rename"
                            | "filesystem::rename"
                            | "std::filesystem::rename"
                    )
            })
        }
        L::Code(Lang::Jvm) => any(&["Files.move(", ".renameTo("]),
        L::Code(Lang::Python) => any(&["os.rename(", "os.replace(", "shutil.move("]),
        L::Code(Lang::Js) => any(&[
            "fs.rename(",
            "renameSync(",
            "promises.rename(",
            "fsp.rename(",
        ]),
        L::Code(Lang::Ruby) => any(&["File.rename(", "FileUtils.mv(", "FileUtils.move("]),
        L::Code(Lang::CSharp) => any(&["File.Move(", "File.Replace(", "Directory.Move("]),
        L::Erlang => any(&["file:rename(", "prim_file:rename("]),
        L::Code(Lang::Sql | Lang::Erlang | Lang::Elixir) => false,
    }
}

/// Whether a line removes old data: a database, a folder, a file.
fn removal_prim(code: &str) -> bool {
    [
        "DestroyDB(",
        "os.RemoveAll(",
        "remove_dir_all(",
        "fs::remove_file(",
        "unlink(",
        "->DeleteFile(",
        "->DeleteDir(",
        "Files.delete(",
        "os.remove(",
        "shutil.rmtree(",
        "unlinkSync(",
        "rmSync(",
        "File.Delete(",
        "Directory.Delete(",
        "file:delete(",
        "del_dir_r(",
    ]
    .iter()
    .any(|p| code.contains(p))
}

/// Whether a line writes a whole file at once.
fn whole_write(lang: L, code: &str) -> bool {
    let any = |pats: &[&str]| pats.iter().any(|p| code.contains(p));
    match lang {
        L::Code(Lang::Go) => any(&["os.WriteFile(", "ioutil.WriteFile("]),
        L::Code(Lang::Rust) => any(&["fs::write(", "fs::copy("]),
        L::Code(Lang::Python) => any(&[".write_text(", ".write_bytes(", "shutil.copyfile("]),
        L::Code(Lang::Js) => any(&[
            "writeFileSync(",
            "fs.writeFile(",
            "promises.writeFile(",
            "fsp.writeFile(",
            "appendFileSync(",
            "fs.appendFile(",
        ]),
        L::Code(Lang::Ruby) => any(&["File.write(", "File.binwrite(", "IO.write("]),
        L::Code(Lang::CSharp) => any(&[
            "File.WriteAllText(",
            "File.WriteAllBytes(",
            "File.WriteAllLines(",
            "File.AppendAllText(",
            "File.WriteAllTextAsync(",
            "File.WriteAllBytesAsync(",
        ]),
        L::Code(Lang::Jvm) => any(&["Files.write(", "Files.writeString(", "Files.copy("]),
        L::Erlang => any(&[
            "file:write_file(",
            "file:write(",
            "file:pwrite(",
            "prim_file:write(",
            "prim_file:pwrite(",
            "file:copy(",
        ]),
        L::Code(Lang::C | Lang::Sql | Lang::Erlang | Lang::Elixir) => false,
    }
}

/// Whether a statement (a line and the lines that continue it) opens a file for writing.
fn opener(lang: L, stmt: &str, stmt_text: &str) -> bool {
    let any = |pats: &[&str]| pats.iter().any(|p| stmt.contains(p));
    let writing_flags = |s: &str| {
        ["O_WRONLY", "O_RDWR", "O_CREAT", "O_APPEND", "O_TRUNC"]
            .iter()
            .any(|f| s.contains(f))
    };
    // a mode string that writes: "w", "wb", "a", "r+", "w+"
    let mode_writes = |t: &str| {
        let mut ok = false;
        for q in ['"', '\''] {
            for part in t.split(q).skip(1).step_by(2) {
                let p = part.trim();
                if !p.is_empty()
                    && p.len() <= 4
                    && p.chars().all(|c| "rwabxt+U".contains(c))
                    && (p.contains('w') || p.contains('a') || p.contains('+') || p.contains('x'))
                {
                    ok = true;
                }
            }
        }
        ok
    };
    match lang {
        L::Code(Lang::Go) => {
            any(&["os.Create(", "os.CreateTemp(", "ioutil.TempFile("])
                || (stmt.contains("os.OpenFile(") && writing_flags(stmt))
        }
        L::Code(Lang::Rust) => {
            any(&[
                "File::create(",
                "File::create_new(",
                "NamedTempFile::new",
                "NamedTempFile::with_prefix",
                "tempfile::tempfile(",
            ]) || (stmt.contains("OpenOptions::new()")
                && [
                    ".write(true)",
                    ".append(true)",
                    ".create(true)",
                    ".create_new(true)",
                ]
                .iter()
                .any(|p| stmt.contains(p)))
        }
        L::Code(Lang::C) => {
            any(&[
                "NewWritableFile(",
                "ReopenWritableFile(",
                "NewRandomRWFile(",
                "creat(",
            ]) || (calls(stmt)
                .iter()
                .any(|(n, _, _)| matches!(n.as_str(), "open" | "open64" | "openat"))
                && writing_flags(stmt))
                || (stmt.contains("fopen(") && mode_writes(stmt_text))
                || ofstream_decl(stmt).is_some()
        }
        L::Code(Lang::Jvm) => {
            any(&[
                "new FileOutputStream(",
                "Files.newOutputStream(",
                "Files.newBufferedWriter(",
                "new FileWriter(",
            ]) || (any(&["FileChannel.open(", "AsynchronousFileChannel.open("])
                && any(&["WRITE", "CREATE", "APPEND"]))
                || (stmt.contains("new RandomAccessFile(") && stmt_text.contains("\"rw"))
        }
        L::Code(Lang::Python) => {
            any(&["NamedTemporaryFile(", "mkstemp("])
                || (stmt.contains("os.open(") && writing_flags(stmt))
                || (calls(stmt)
                    .iter()
                    .any(|(n, q, _)| n == "open" && (q == "open" || q.ends_with(".open")))
                    && mode_writes(stmt_text))
        }
        L::Code(Lang::Js) => {
            stmt.contains("createWriteStream(")
                || ((stmt.contains("openSync(")
                    || stmt.contains("fs.open(")
                    || stmt.contains("promises.open(")
                    || stmt.contains("fsp.open("))
                    && mode_writes(stmt_text))
        }
        L::Code(Lang::Ruby) => {
            (stmt.contains("File.open(") || stmt.contains("File.new(")) && mode_writes(stmt_text)
        }
        L::Code(Lang::CSharp) => {
            any(&[
                "File.Create(",
                "File.OpenWrite(",
                "File.AppendText(",
                "File.CreateText(",
            ]) || (stmt.contains("new FileStream(")
                && any(&[
                    "FileMode.Create",
                    "FileMode.Append",
                    "FileMode.OpenOrCreate",
                    "FileMode.CreateNew",
                    "FileMode.Truncate",
                    "FileAccess.Write",
                    "FileAccess.ReadWrite",
                ]))
        }
        L::Erlang => {
            any(&["file:open(", "prim_file:open("])
                && (has_token(stmt, "write") || has_token(stmt, "append") || stmt.contains("WRITE"))
        }
        L::Code(Lang::Sql | Lang::Erlang | Lang::Elixir) => false,
    }
}

/// `std::ofstream name(` or `ofstream name{`: the stream's name.
fn ofstream_decl(code: &str) -> Option<String> {
    for kw in ["ofstream", "fstream"] {
        for p in token_at(code, kw) {
            let rest = code[p + kw.len()..].trim_start();
            let name: String = rest.chars().take_while(|c| is_ident(*c)).collect();
            if name.is_empty() {
                continue;
            }
            let after = rest[name.len()..].trim_start();
            if after.starts_with('(') || after.starts_with('{') || after.starts_with(';') {
                return Some(name);
            }
        }
    }
    None
}

/// The variable a statement assigns: `f`, `d.writeFile` (as `writeFile`), `tmp_file`, `Fd` of
/// Erlang's `{ok, Fd} =`, `f` of Python's `with open(...) as f:`.
fn assign_target(lang: L, code: &str) -> Option<String> {
    if let Some(n) = ofstream_decl(code) {
        return Some(n);
    }
    if lang.is(Lang::Python) || lang.is(Lang::Ruby) {
        if let Some(p) = code.find(" as ") {
            let n: String = code[p + 4..]
                .trim_start()
                .chars()
                .take_while(|c| is_ident(*c))
                .collect();
            if !n.is_empty() {
                return Some(n);
            }
        }
    }
    let b = code.as_bytes();
    let mut eq = None;
    for (i, c) in code.char_indices() {
        if c != '=' {
            continue;
        }
        let prev = if i > 0 { b[i - 1] as char } else { ' ' };
        let next = b.get(i + 1).map(|x| *x as char).unwrap_or(' ');
        if next == '=' || next == '>' || matches!(prev, '=' | '!' | '<' | '>') {
            continue;
        }
        eq = Some(i);
        break;
    }
    let eq = eq?;
    let lhs = code[..eq].trim_end_matches(':').trim();
    // the first part that is not `_`, `ok`, `err`, `error`
    let skip = ["_", "ok", "err", "error", "e"];
    for part in lhs.split(',') {
        let p = part
            .trim()
            .trim_matches(|c| c == '{' || c == '}' || c == '(' || c == ')');
        let last: String = p
            .chars()
            .rev()
            .take_while(|c| is_ident(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if last.is_empty() || skip.contains(&last.as_str()) {
            continue;
        }
        if matches!(last.as_str(), "let" | "mut" | "var" | "auto" | "const") {
            continue;
        }
        return Some(last);
    }
    None
}

/// Methods that write to the object they are called on.
const WRITE_METHODS: &[&str] = &[
    "write",
    "write_all",
    "write_at",
    "write_all_at",
    "write_vectored",
    "write_fmt",
    "writestring",
    "writeat",
    "readfrom",
    "append",
    "positionedappend",
    "print",
    "println",
    "printf",
    "writebytes",
    "writeasync",
    "writebyte",
    "writeline",
    "writelines",
    "transferfrom",
    "write_str",
];

/// Words of a call that writes what it is given.
const WRITE_CALL_WORDS: &[&str] = &[
    "write",
    "writer",
    "writes",
    "copy",
    "append",
    "print",
    "fprintf",
    "fwrite",
    "fputs",
    "encode",
    "encoder",
    "persist",
    "dump",
    "save",
    "serialize",
    "pwrite",
    "writev",
    "pwritev",
    "sendfile",
    "splice",
];

/// Whether a call writes what it is given: the first or the last word of its name is a word of
/// writing (`copyimage`, `persist_to`, `to_writer`, `io.Copy`, `NewWriter`), or it makes a
/// writer of a type named so (`BufWriter::new`, `GzEncoder::new`).
fn writing_call(name: &str, qual: &str) -> bool {
    let hit = |w: &String| WRITE_CALL_WORDS.iter().any(|x| w.starts_with(x));
    let w = name_words(name);
    if w.first().is_some_and(hit) || w.last().is_some_and(hit) {
        return true;
    }
    if matches!(name, "new" | "New" | "create") {
        let ty = qual.rsplit("::").nth(1).unwrap_or("");
        return name_words(ty)
            .last()
            .is_some_and(|x| x == "writer" || x == "encoder");
    }
    false
}

/// Whether the line writes to the handle `h`.
fn writes_handle(code: &str, h: &str) -> bool {
    for p in token_at(code, h) {
        let after = code[p + h.len()..].trim_start();
        if after.starts_with("<<") {
            return true;
        }
        let rest = after
            .strip_prefix("->")
            .or_else(|| after.strip_prefix('.'))
            .map(|r| r.trim_start());
        if let Some(r) = rest {
            let m: String = r.chars().take_while(|c| is_ident(*c)).collect();
            if r[m.len()..].trim_start().starts_with('(')
                && WRITE_METHODS.contains(&m.to_ascii_lowercase().as_str())
            {
                return true;
            }
            continue;
        }
        // the handle as an argument: the call that encloses it
        let before: Vec<char> = code[..p].chars().collect();
        let mut depth = 0i32;
        let mut k = before.len();
        while k > 0 {
            k -= 1;
            match before[k] {
                ')' => depth += 1,
                '(' if depth == 0 => {
                    let line: String = before[..=k].iter().collect();
                    if let Some((n, q, _)) = calls(&line).last() {
                        if writing_call(n, q) {
                            return true;
                        }
                    }
                    break;
                }
                '(' => depth -= 1,
                _ => {}
            }
        }
    }
    false
}

/// Words that say a line answers the client or commits.
const ACK_WORDS: &[&str] = &[
    "reply",
    "replies",
    "replied",
    "respond",
    "responds",
    "responded",
    "response",
    "ack",
    "acks",
    "acked",
    "acknowledge",
    "acknowledged",
    "confirm",
    "confirms",
    "confirmed",
    "commit",
    "commits",
    "committed",
    "commited",
    "succeeded",
    "accepted",
];

/// Calls and constants that answer an HTTP request with success.
const HTTP_OK: &[&str] = &[
    "WriteHeader(",
    "StatusOK",
    "StatusCreated",
    "StatusAccepted",
    "StatusNoContent",
    "StatusCode::OK",
    "StatusCode::CREATED",
    "StatusCode::ACCEPTED",
    "HttpResponse::Ok",
    "HttpResponse::Created",
    "HttpResponse::Accepted",
    "ResponseEntity.ok(",
    "res.send(",
    "res.json(",
    "c.JSON(",
    "ctx.JSON(",
    "addReply",
];

fn ack_line(code: &str) -> bool {
    has_word(code, ACK_WORDS) || HTTP_OK.iter().any(|p| code.contains(p))
}

/// Calls that run on a timer.
fn ticker_prim(lang: L, code: &str) -> bool {
    let any = |pats: &[&str]| pats.iter().any(|p| code.contains(p));
    match lang {
        L::Code(Lang::Go) => any(&["time.NewTicker(", "time.Tick("]),
        L::Code(Lang::Rust) => any(&["time::interval(", "interval_at("]),
        L::Code(Lang::Jvm) => any(&["scheduleAtFixedRate(", "scheduleWithFixedDelay("]),
        L::Code(Lang::CSharp) => any(&["new PeriodicTimer(", "new Timer("]),
        L::Code(Lang::Js) => any(&["setInterval("]),
        L::Erlang => any(&["timer:send_interval(", "timer:apply_interval("]),
        _ => false,
    }
}

/// A sleep that a loop repeats.
fn sleep_prim(lang: L, code: &str) -> bool {
    let any = |pats: &[&str]| pats.iter().any(|p| code.contains(p));
    match lang {
        L::Code(Lang::Go) => any(&["time.Sleep(", "time.After("]),
        L::Code(Lang::Rust) => any(&["time::sleep(", "thread::sleep("]),
        L::Code(Lang::Jvm) => any(&["Thread.sleep("]),
        L::Code(Lang::CSharp) => any(&["Task.Delay(", "Thread.Sleep("]),
        L::Code(Lang::Python) => any(&["time.sleep(", "asyncio.sleep("]),
        L::Erlang => any(&["erlang:send_after(", "timer:sleep("]),
        L::Code(Lang::C) => any(&["sleep_for(", "usleep(", "nanosleep("]),
        _ => false,
    }
}

fn loop_line(code: &str) -> bool {
    let t = code.trim_start();
    t.starts_with("loop")
        || t.starts_with("for {")
        || t.starts_with("for ")
        || t.starts_with("while")
}

// ---------------------------------------------------------------- the helpers of a project

/// Names of the project's own functions, by what they do.
#[derive(Default)]
struct Helpers {
    /// syncs, and its name says so (`sync`, `fsync`, `flush`, `durable`)
    sync: HashSet<String>,
    /// syncs a folder (`syncDir`, `fsync_parent`, `DataSyncRecursively`)
    dir_sync: HashSet<String>,
    /// opens a file for writing (`NewTmpFile`, `Create`)
    open: HashSet<String>,
    /// renames, and its name says so (`SwapTmpFile`, `persist`)
    rename: HashSet<String>,
}

const SYNC_NAME_WORDS: &[&str] = &["sync", "fsync", "fdatasync", "datasync", "flush", "durable"];
const DIR_WORDS: &[&str] = &[
    "dir",
    "directory",
    "parent",
    "folder",
    "recursively",
    "recursive",
];
const RENAME_NAME_WORDS: &[&str] = &[
    "rename", "swap", "move", "replace", "persist", "install", "publish", "atomic",
];

fn helpers(files: &[SFile]) -> Helpers {
    let mut h = Helpers::default();
    for f in files {
        for (name, a, b) in &f.funcs {
            if name.len() < 3 || b - a > 80 {
                continue;
            }
            let lines = (a + 1)..=(*b).min(f.code.len().saturating_sub(1));
            let mut syncs = false;
            let mut renames = false;
            let mut opens = false;
            for i in lines {
                let c = &f.code[i];
                if c.is_empty() {
                    continue;
                }
                syncs |= sync_prim(f.lang, c, &f.text[i]);
                renames |= rename_prim(f, c);
                opens |= opener(f.lang, c, &f.text[i]);
            }
            let w = name_words(name);
            let says = |set: &[&str]| w.iter().any(|x| set.contains(&x.as_str()));
            if syncs && says(SYNC_NAME_WORDS) {
                h.sync.insert(name.clone());
                if says(DIR_WORDS) {
                    h.dir_sync.insert(name.clone());
                }
            }
            if renames && says(RENAME_NAME_WORDS) {
                h.rename.insert(name.clone());
            }
            if opens
                && says(&[
                    "new", "create", "open", "reopen", "make", "tmp", "temp", "file", "writer",
                ])
            {
                h.open.insert(name.clone());
            }
        }
    }
    // a helper that both opens and syncs is no opener of unsynced files
    let sync = h.sync.clone();
    h.open.retain(|n| !sync.contains(n));
    h
}

fn calls_any(code: &str, set: &HashSet<String>) -> Option<String> {
    calls(code)
        .into_iter()
        .find(|(n, _, _)| set.contains(n) && !NOT_CALLS.contains(&n.as_str()))
        .map(|(n, _, _)| n)
}

// ---------------------------------------------------------------- callers

/// The call sites of a set of names, found in one pass over every file: name -> (file, line).
fn call_sites(files: &[SFile], names: &HashSet<String>) -> HashMap<String, Vec<(usize, usize)>> {
    let mut out: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    if names.is_empty() {
        return out;
    }
    for (fi, f) in files.iter().enumerate() {
        let starts: HashSet<usize> = f.funcs.iter().map(|x| x.1).collect();
        for (i, l) in f.code.iter().enumerate() {
            if l.is_empty() || starts.contains(&i) || !l.contains('(') {
                continue;
            }
            for (n, _, _) in calls(l) {
                if names.contains(&n) {
                    out.entry(n).or_default().push((fi, i));
                }
            }
        }
    }
    out
}

/// The parent folder of a file's path.
fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map(|x| x.0).unwrap_or("")
}

/// The call sites of `name` defined in `def_file`: when a name is called from more than 40
/// places, only the calls in the same folder (a common name such as `write`).
fn sites_of(
    sites: &HashMap<String, Vec<(usize, usize)>>,
    files: &[SFile],
    name: &str,
    def_file: usize,
) -> Vec<(usize, usize)> {
    let all = sites.get(name).cloned().unwrap_or_default();
    if all.len() <= 40 {
        return all;
    }
    let dir = parent(&files[def_file].rel);
    all.into_iter()
        .filter(|(fi, _)| parent(&files[*fi].rel) == dir)
        .collect()
}

// ---------------------------------------------------------------- one function

/// The statement that starts at line `i`: the line, and up to 6 more while the line ends with an
/// open parenthesis, a comma or an operator, or the next one goes on with `.` or `)`.
fn stmt(lines: &[String], i: usize, braces: bool) -> String {
    let mut s = lines[i].clone();
    if !braces {
        return s;
    }
    let mut k = i;
    while k + 1 < lines.len() && k < i + 6 {
        let t = lines[k].trim_end();
        let next = lines[k + 1].trim_start();
        let open = t.ends_with(['(', ',', '|', '&', '+', '=', '.', ':', '\\']);
        let goes_on = next.starts_with('.') || next.starts_with(')') || next.starts_with("?.");
        if !(open || goes_on) || next.is_empty() {
            break;
        }
        k += 1;
        s.push(' ');
        s.push_str(next);
    }
    s
}

fn braces(lang: L) -> bool {
    !matches!(
        lang,
        L::Erlang | L::Code(Lang::Python) | L::Code(Lang::Ruby) | L::Code(Lang::Sql)
    )
}

/// The handles that a file's fields hold: `d.writeFile, err = os.OpenFile(...)` gives
/// `writeFile` in every function of the file.
fn field_handles(f: &SFile, h: &Helpers) -> HashSet<String> {
    let mut out = HashSet::new();
    for i in 0..f.code.len() {
        let c = &f.code[i];
        if c.is_empty() || !c.contains('=') {
            continue;
        }
        let s = stmt(&f.code, i, braces(f.lang));
        let st = stmt(&f.text, i, braces(f.lang));
        if !(opener(f.lang, &s, &st) || calls_any(c, &h.open).is_some()) {
            continue;
        }
        let eq = c.find('=').unwrap_or(0);
        let lhs = &c[..eq];
        if let Some(t) = assign_target(f.lang, c) {
            let member = f.lang.is(Lang::C) && (t.ends_with('_') || t.starts_with("m_"));
            if lhs.contains('.') || lhs.contains("->") || member {
                out.insert(t);
            }
        }
    }
    out
}

/// What one function does with files, in line order.
#[derive(Default)]
struct Flow {
    /// (line, handle or None) of each open for writing
    opens: Vec<(usize, Option<String>)>,
    writes: Vec<(usize, i32)>,
    syncs: Vec<(usize, i32)>,
    /// syncs that cover the whole function (Go's `defer f.Sync()`)
    deferred_sync: bool,
    renames: Vec<(usize, i32, bool)>,
    dir_syncs: Vec<usize>,
    /// streams declared: name -> line
    streams: HashMap<String, usize>,
    /// the first write of each stream after which no flush or close came before a sync
    stream_issue: Option<(usize, String, usize)>,
    ticker: Option<usize>,
    sleep: Option<usize>,
    looped: bool,
    saves: Vec<usize>,
    /// the words of the calls that save
    save_words: String,
}

fn flow(f: &SFile, fi: usize, h: &Helpers, fields: &HashSet<String>) -> Flow {
    let (_, a, b) = &f.funcs[fi];
    let end = (*b).min(f.code.len().saturating_sub(1));
    let mut fl = Flow::default();
    let mut handles: HashSet<String> = fields.clone();
    let mut stream_written: HashMap<String, usize> = HashMap::new();
    for i in (*a + 1)..=end {
        if f.func_of[i] != Some(fi) {
            continue;
        }
        let c = &f.code[i];
        if c.trim().is_empty() {
            continue;
        }
        let t = &f.text[i];
        let d = f.depth[i];
        let s = stmt(&f.code, i, braces(f.lang));
        let st = stmt(&f.text, i, braces(f.lang));
        let opens_here = opener(f.lang, &s, &st) || calls_any(c, &h.open).is_some();
        if opens_here && c.contains('=')
            || ofstream_decl(c).is_some()
            || (opens_here && c.contains(" as "))
        {
            let target = assign_target(f.lang, c);
            if let Some(tg) = &target {
                handles.insert(tg.clone());
            }
            if let Some(sn) = ofstream_decl(c) {
                fl.streams.insert(sn, i);
            }
            fl.opens.push((i, target));
        } else if opens_here {
            fl.opens.push((i, None));
        }
        let synced = sync_prim(f.lang, c, t) || calls_any(c, &h.sync).is_some();
        if synced {
            if c.trim_start().starts_with("defer ") {
                fl.deferred_sync = true;
            }
            fl.syncs.push((i, d));
            // a stream written before this sync and not flushed or closed since
            if fl.stream_issue.is_none() {
                if let Some((n, w)) = stream_written.iter().min_by_key(|x| x.1) {
                    fl.stream_issue = Some((*w, n.clone(), i));
                }
            }
        }
        if calls_any(c, &h.dir_sync).is_some() || (synced && !fl.renames.is_empty()) {
            fl.dir_syncs.push(i);
        }
        let written = whole_write(f.lang, c)
            || handles
                .iter()
                .any(|hd| writes_handle(c, hd) && !wraps_away(c, hd))
            || (opens_here && whole_write(f.lang, &s));
        if written && !synced {
            fl.writes.push((i, d));
        }
        for sn in fl.streams.keys() {
            if writes_handle(c, sn) || has_token(c, sn) && c.contains("<<") {
                stream_written.entry(sn.clone()).or_insert(i);
            }
            let flushed = [".flush(", ".close(", ".Flush(", ".Close("]
                .iter()
                .any(|m| c.contains(&format!("{sn}{m}")));
            if flushed {
                stream_written.remove(sn);
            }
        }
        let prim = rename_prim(f, c);
        let renamed_by_helper =
            !prim && calls_any(c, &h.rename).is_some_and(|n| n != f.funcs[fi].0);
        if prim || renamed_by_helper {
            fl.renames.push((i, d, renamed_by_helper));
        }
        if ticker_prim(f.lang, c) && fl.ticker.is_none() {
            fl.ticker = Some(i);
        }
        if sleep_prim(f.lang, c) && fl.sleep.is_none() {
            fl.sleep = Some(i);
        }
        if loop_line(c) {
            fl.looped = true;
        }
        if synced
            || calls(c).iter().any(|(n, _, _)| {
                // a save, not a constructor (`new SnapshotIteratorManager(`)
                let w = name_words(n);
                !c.contains(&format!("new {n}"))
                    && w.first().is_some_and(|x| {
                        ["save", "persist", "checkpoint", "dump", "snapshot"].contains(&x.as_str())
                    })
            })
        {
            fl.saves.push(i);
            fl.save_words.push(' ');
            fl.save_words.push_str(c.trim());
        }
    }
    fl
}

/// Whether a line only wraps the handle `h` in a writer that it keeps for later or returns
/// (`db.journal = journal.NewWriter(w)`, `tw: table.NewWriter(fw)`, `return BufWriter::new(f)`):
/// the writes and the sync come later, in the wrapper's own methods.
fn wraps_away(code: &str, h: &str) -> bool {
    let direct = token_at(code, h).iter().any(|p| {
        let after = code[p + h.len()..].trim_start();
        after.starts_with("<<") || after.starts_with('.') || after.starts_with("->")
    });
    if direct {
        return false;
    }
    let t = code.trim_start();
    let field_init = {
        let name: String = t.chars().take_while(|c| is_ident(*c)).collect();
        !name.is_empty() && t[name.len()..].starts_with(": ")
    };
    let stored = match code.find('=') {
        Some(p) if !code[p + 1..].starts_with('=') => {
            let lhs = &code[..p];
            lhs.contains('.') || lhs.contains("->")
        }
        _ => false,
    };
    t.starts_with("return ") || field_init || stored
}

/// Whether a sync after line `at` (at depth `d`) covers it: a sync later at that depth or above.
fn covered(syncs: &[(usize, i32)], at: usize, d: i32) -> Option<bool> {
    let after: Vec<&(usize, i32)> = syncs.iter().filter(|s| s.0 > at).collect();
    if after.is_empty() {
        return None;
    }
    Some(after.iter().any(|s| s.1 <= d))
}

// ---------------------------------------------------------------- the audit

pub fn audit(root: &Path) -> SyncAudit {
    let mut paths = Vec::new();
    crate::rules::walk(root, &mut paths);
    let mut texts: Vec<(String, String)> = Vec::new();
    for p in paths {
        let rel = p
            .strip_prefix(root)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        if lang_of(&rel).is_none()
            || clockscan::skipped(&rel.to_ascii_lowercase())
            || build_code(&rel.to_ascii_lowercase())
        {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        if meta.len() > 1_500_000 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if clockscan::generated(&text) {
            continue;
        }
        texts.push((rel, text));
    }
    let refs: Vec<(&str, &str)> = texts
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    audit_texts(&refs)
}

/// Code that builds, samples or benchmarks a project rather than runs in it (lower case): the
/// clock audit's list leaves these in, and their writes are no answers.
fn build_code(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() > 1
        && matches!(
            parts[0],
            "utils" | "tools" | "scripts" | "docker" | "packaging" | "deploy" | "docs" | "website"
        )
    {
        return true;
    }
    let (dirs, name) = parts.split_at(parts.len().saturating_sub(1));
    let name = name.first().copied().unwrap_or("");
    dirs.iter().any(|d| {
        matches!(
            *d,
            "benchmark"
                | "samples"
                | "sample"
                | "demo"
                | "demos"
                | "playground"
                | "website"
                | ".github"
                | "contrib"
                | "ci"
                | "tools"
                | "hack"
                | "utils/build"
        )
    }) || matches!(
        name,
        "magefile.go"
            | "build.rs"
            | "setup.py"
            | "build.js"
            | "build.mjs"
            | "build.ts"
            | "gulpfile.js"
            | "webpack.config.js"
            | "vite.config.ts"
            | "rollup.config.js"
    )
}

fn lang_of(rel: &str) -> Option<L> {
    let low = rel.to_ascii_lowercase();
    if low.ends_with(".erl") || low.ends_with(".hrl") {
        return Some(L::Erlang);
    }
    // Elixir is the clock audit's language only; this audit reads Erlang with its own `L::Erlang`.
    clockscan::lang_of(rel)
        .filter(|l| !matches!(l, Lang::Erlang | Lang::Elixir))
        .map(L::Code)
}

/// The audit of files given as (path, text): the unit tests' entry, and the walk's.
pub fn audit_texts(texts: &[(&str, &str)]) -> SyncAudit {
    let mut files = Vec::new();
    let mut tests = TestItems::default();
    for (rel, text) in texts {
        let Some(l) = lang_of(rel) else { continue };
        let (f, t) = load(rel.to_string(), l, text);
        tests.functions += t.functions;
        tests.modules += t.modules;
        files.push(f);
    }
    let places = places_of(&files);
    SyncAudit {
        read: files.len(),
        places,
        tests,
    }
}

/// A finding of one function before its acknowledgement is searched.
struct Raw {
    file: usize,
    func: Option<usize>,
    line: usize,
    shape: &'static str,
    what: String,
    missing: Vec<String>,
    notes: Vec<String>,
    window: Option<f64>,
    /// whether callers can cover it (a write that a caller may sync)
    callers_cover: bool,
    /// more words for the data test: the saved call of a loop on a timer
    words: String,
    /// a write and a rename in one function: the author meant the file to replace another
    replace: bool,
}

fn places_of(files: &[SFile]) -> Vec<SyncPlace> {
    let h = helpers(files);
    let mut raws: Vec<Raw> = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        if f.lang.is(Lang::Sql) {
            continue;
        }
        let fields = field_handles(f, &h);
        for k in 0..f.funcs.len() {
            if f.funcs[k].2 <= f.funcs[k].1 {
                continue;
            }
            let fl = flow(f, k, &h, &fields);
            function_raws(f, fi, k, &fl, &mut raws);
        }
    }
    // line by line: settings and O_DIRECT
    let full_sync_somewhere = files.iter().any(|f| f.text.iter().any(|l| sqlite_full(l)));
    for (fi, f) in files.iter().enumerate() {
        line_raws(f, fi, full_sync_somewhere, &mut raws);
    }
    // the callers: who covers a write, and who answers after it
    let main = main_langs(files);
    resolve(files, raws, &main, &h)
}

fn main_langs(files: &[SFile]) -> HashSet<L> {
    let code_files = files
        .iter()
        .filter(|f| !f.lang.is(Lang::Sql))
        .count()
        .max(1);
    let mut count: HashMap<L, usize> = HashMap::new();
    for f in files {
        if !f.lang.is(Lang::Sql) {
            *count.entry(f.lang).or_default() += 1;
        }
    }
    count
        .into_iter()
        .filter(|(_, n)| n * 5 >= code_files)
        .map(|(l, _)| l)
        .collect()
}

fn function_raws(f: &SFile, fi: usize, k: usize, fl: &Flow, raws: &mut Vec<Raw>) {
    // a stream whose buffer may hold the data at the sync (Mender's version file)
    if let Some((w, sn, s)) = &fl.stream_issue {
        raws.push(Raw {
            file: fi,
            func: Some(k),
            line: *w,
            shape: "stream",
            what: format!(
                "It writes to the stream {sn} and syncs at line {} while the stream is still open, so the bytes in the stream's buffer reach the file only after the sync.",
                s + 1
            ),
            missing: vec![format!("a flush or a close of {sn} before the sync")],
            notes: Vec::new(),
            window: None,
            callers_cover: false,
            words: String::new(),
            replace: false,
        });
    }
    // writes that no sync covers
    let mut missing = Vec::new();
    let mut notes = Vec::new();
    let mut first: Option<usize> = None;
    if !fl.deferred_sync {
        for (w, d) in &fl.writes {
            match covered(&fl.syncs, *w, *d) {
                Some(true) => {}
                Some(false) => {
                    let s = fl.syncs.iter().find(|s| s.0 > *w).map(|s| s.0).unwrap_or(0);
                    first.get_or_insert(*w);
                    let n = format!(
                        "the sync at line {} runs only under a condition that the write at line {} is not in",
                        s + 1,
                        w + 1
                    );
                    if !notes.contains(&n) && notes.is_empty() {
                        notes.push(n);
                    }
                }
                None => {
                    first.get_or_insert(*w);
                }
            }
        }
    }
    if first.is_some() {
        missing.push("a sync of the file".to_string());
    }
    // renames that no sync of the folder follows
    let mut rename_line = None;
    for (r, _d, helper) in &fl.renames {
        if fl.dir_syncs.iter().any(|s| s > r) {
            continue;
        }
        // a call of a helper that renames is a place where the caller wrote the file itself
        if *helper && fl.writes.is_empty() {
            continue;
        }
        rename_line.get_or_insert((*r, *helper));
    }
    if let Some((r, helper)) = rename_line {
        let m = if helper {
            "a sync of the folder after the rename (the helper it calls renames without one)"
        } else {
            "a sync of the folder after the rename"
        };
        missing.push(m.to_string());
        if first.is_none() {
            first = Some(r);
        }
    }
    if let Some(at) = first {
        let open = fl
            .opens
            .iter()
            .filter(|o| o.0 <= at)
            .next_back()
            .map(|o| o.0 + 1);
        let shape = if rename_line.is_some() && missing.len() == 1 {
            "rename"
        } else {
            "write"
        };
        let what = match (shape, open) {
            ("rename", _) => "It renames a file into place and does not sync the folder, so after a power cut the folder can hold the old name, or neither.".to_string(),
            (_, Some(o)) if o != at + 1 => format!(
                "It writes to a file opened at line {o} and does not sync it, so the data stays in the page cache until the kernel writes it back, about 30 s later."
            ),
            _ => "It writes a file and does not sync it, so the data stays in the page cache until the kernel writes it back, about 30 s later.".to_string(),
        };
        raws.push(Raw {
            file: fi,
            func: Some(k),
            line: at,
            shape,
            what,
            missing,
            notes,
            window: None,
            callers_cover: true,
            words: String::new(),
            replace: rename_line.is_some() && !fl.writes.is_empty(),
        });
    }
    // a loop that syncs or saves on a timer
    let timer = fl.ticker.or(if fl.looped { fl.sleep } else { None });
    if let (Some(t), Some(s)) = (timer, fl.saves.first()) {
        let window = window_of(f, t);
        raws.push(Raw {
            file: fi,
            func: Some(k),
            line: *s,
            shape: "periodic",
            what: format!(
                "It syncs or saves on a timer (line {}){}, so what was written since the last tick is not on the disk.",
                t + 1,
                window
                    .map(|w| format!(", every {}", seconds_text(w)))
                    .unwrap_or_default()
            ),
            missing: vec!["a sync before the answer, not on a timer".to_string()],
            notes: Vec::new(),
            window,
            callers_cover: false,
            words: fl.save_words.clone(),
            replace: false,
        });
    }
}

/// A duration in words: `2 seconds`, `500 ms`, `1 minute`.
fn seconds_text(s: f64) -> String {
    if s >= 1.0 && s.fract() == 0.0 {
        clockscan::seconds_words(s as u64)
    } else if s >= 1.0 {
        format!("{s} seconds")
    } else {
        format!("{} ms", (s * 1000.0).round())
    }
}

/// The interval of a timer at line `t`: a literal, or a constant of the same file.
fn window_of(f: &SFile, t: usize) -> Option<f64> {
    let line = &f.code[t];
    let open = line.find('(')?;
    let arg_end = line[open + 1..].find([')', ','])? + open + 1;
    let arg = line[open + 1..arg_end].trim();
    if let Some(v) = clockscan::duration_value(arg, unit_of(arg)) {
        return Some(v);
    }
    // a name: its constant in the file
    let name: String = arg
        .rsplit(['.', ':', '>'])
        .next()
        .unwrap_or(arg)
        .chars()
        .filter(|c| is_ident(*c))
        .collect();
    if name.is_empty() {
        return None;
    }
    for l in &f.code {
        let t = l.trim_start();
        if !(t.starts_with("const ")
            || t.starts_with("pub const ")
            || t.starts_with("static ")
            || t.starts_with("#define ")
            || t.starts_with("private static final"))
        {
            continue;
        }
        if !has_token(l, &name) {
            continue;
        }
        let rhs = if let Some(p) = l.find('=') {
            &l[p + 1..]
        } else {
            l.split_whitespace().nth(2).unwrap_or("")
        };
        if let Some(v) = clockscan::duration_value(rhs.trim().trim_end_matches(';'), unit_of(&name))
        {
            return Some(v);
        }
    }
    None
}

fn unit_of(name: &str) -> &'static str {
    let l = name.to_ascii_lowercase();
    if l.contains("ms") || l.contains("millis") {
        "ms"
    } else {
        "s"
    }
}

// ---------------------------------------------------------------- settings and O_DIRECT

fn sqlite_full(l: &str) -> bool {
    let low = l.to_ascii_lowercase().replace([' ', '\'', '"'], "");
    [
        "synchronous=full",
        "synchronous=extra",
        "synchronous=2",
        "synchronous=3",
        "_sync=full",
        "_sync=2",
        "synchronous(full)",
        "synchronous::full",
        "synchronous.full",
        "synchronous,full",
        "synchronous,extra",
    ]
    .iter()
    .any(|p| low.contains(p))
}

/// Names that switch a sync on or off, and names of something else that has `sync` in it.
const NOT_DISK_SYNC: &[&str] = &[
    "repl",
    "replica",
    "replicas",
    "replication",
    "psync",
    "slave",
    "master",
    "cluster",
    "diskless",
    "gossip",
    "async",
    "clock",
    "ntp",
    "thread",
    "threads",
    "wait",
    "lock",
    "mutex",
    "chan",
    "channel",
    "group",
    "once",
    "map",
    "pool",
    "dir",
    "path",
    "checkpoint",
    "time",
    "timer",
    "sema",
    "semaphore",
    "rsid",
    "peers",
    "metric",
    "metrics",
    "stats",
    "client",
    "token",
    "user",
    "ctx",
    "context",
    "rpc",
    "network",
    "net",
    "http",
    "request",
    "query",
    "drop",
    "kill",
    "nuraft",
    "ui",
    "view",
    "scroll",
    "state",
    "status",
    "cursor",
    "avro",
    "format",
    "output",
    "stream",
    "enforce",
    "settings",
    "specified",
    "users",
    "ldap",
    "calendar",
    "contacts",
    "mail",
    "remote",
    "upstream",
    "s3",
    "git",
    "repo",
];

/// What a setting's name says: `switch` (syncing on or off), `inverse` (on means no sync),
/// `interval` (a sync every so often), or None.
fn setting_kind(name: &str) -> Option<&'static str> {
    let w = name_words(name);
    let has = |x: &str| w.iter().any(|y| y == x);
    if w.iter().any(|x| NOT_DISK_SYNC.contains(&x.as_str())) {
        return None;
    }
    let syncish =
        has("fsync") || has("fdatasync") || has("datasync") || has("sync") || has("nosync");
    let flushish = has("flush");
    if !(syncish || flushish || has("synchronous")) {
        return None;
    }
    if has("nosync") || ((has("no") || has("disable") || has("skip") || has("unsafe")) && syncish) {
        return Some("inverse");
    }
    let timed = [
        "every",
        "interval",
        "period",
        "periodic",
        "timeout",
        "delay",
        "ms",
        "millis",
        "milliseconds",
        "seconds",
        "secs",
        "messages",
    ];
    if w.iter().any(|x| timed.contains(&x.as_str())) {
        return Some("interval");
    }
    if has("synchronous") || has("fsync") || has("fdatasync") || has("datasync") {
        return Some("switch");
    }
    if syncish {
        // `sync` alone: only with a word of writing data
        let data = [
            "write", "writes", "wal", "data", "metadata", "log", "commit", "disk", "always",
            "after",
        ];
        if w.iter().any(|x| data.contains(&x.as_str())) {
            return Some("switch");
        }
    }
    None
}

/// The (name, value) pairs that a line declares, each with how it was read.
fn settings_of(lang: L, code: &str, text: &str, prev: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let t = text.trim();
    // a macro table: `DECLARE(Bool, fsync_after_insert, false, ...` or `M(UInt64, name, 0, ...`
    for (n, _, p) in calls(code) {
        if matches!(
            n.as_str(),
            "DECLARE" | "M" | "DECLARE_WITH_ALIAS" | "MAKE_OBSOLETE"
        ) {
            let inner = &code[p + 1..];
            let parts: Vec<&str> = inner.splitn(4, ',').map(|x| x.trim()).collect();
            if parts.len() >= 3 {
                out.push((parts[1].to_string(), parts[2].to_string()));
            }
        }
    }
    // a table of strings: `{"rocksdb.write_options.sync", false, new YesNoField(...)}`
    if let Some(q) = t.find('"') {
        let rest = &t[q + 1..];
        if let Some(e) = rest.find('"') {
            let name = &rest[..e];
            let after = rest[e + 1..].trim_start();
            if let Some(v) = after.strip_prefix(',').or_else(|| after.strip_prefix(':')) {
                let v = v.trim_start();
                let val: String = v
                    .chars()
                    .take_while(|c| !matches!(c, ',' | ')' | '}' | ';'))
                    .collect();
                if !name.contains(' ') && !name.is_empty() && is_literal(val.trim()) {
                    out.push((name.to_string(), val.trim().to_string()));
                }
            }
        }
    }
    // flags: `Bool("fsync", false, ...)`, `BoolVar(&x, "sync", false, ...)`
    for (n, _, p) in calls(text) {
        if matches!(
            n.as_str(),
            "Bool"
                | "BoolVar"
                | "Int"
                | "IntVar"
                | "Int64"
                | "Int64Var"
                | "Duration"
                | "DurationVar"
        ) {
            let inner = &text[p + 1..];
            let parts: Vec<&str> = inner.split(',').map(|x| x.trim()).collect();
            for (k, part) in parts.iter().enumerate() {
                if part.starts_with('"') {
                    if let Some(v) = parts.get(k + 1).filter(|v| is_literal(v)) {
                        out.push((part.trim_matches('"').to_string(), v.to_string()));
                    }
                    break;
                }
            }
        }
    }
    // a field with a literal: `SyncEvery: 2500,`, `fsync = false`, `write_options.sync = false;`
    let c = code.trim();
    for sep in [':', '='] {
        if let Some(p) = c.find(sep) {
            let (l, r) = (&c[..p], &c[p + 1..]);
            if sep == ':' && (r.starts_with(':') || l.ends_with(':')) {
                continue;
            }
            if sep == '='
                && (r.starts_with('=') || l.ends_with(['!', '<', '>', '=', '+', '-', '|', '&']))
            {
                continue;
            }
            // the whole path on the left: `rocks_db.write_options.sync`
            let name: String = l
                .trim()
                .rsplit(|ch: char| !(is_ident(ch) || ch == '.' || ch == '>' || ch == '-'))
                .next()
                .unwrap_or("")
                .to_string();
            let r = r.trim();
            let val: String = if r.starts_with('"') {
                r.chars()
                    .take_while(|ch| *ch != ',' && *ch != ';')
                    .collect()
            } else {
                r.chars()
                    .take_while(|ch| !matches!(ch, ',' | ';' | ')' | ']' | '}'))
                    .collect()
            };
            let val = val.trim().to_string();
            if !name.is_empty() && is_literal(&val) {
                out.push((name, val));
            }
            break;
        }
    }
    // Rust: `#[serde(default)] pub metadata_fsync: bool,`
    if lang.is(Lang::Rust) && c.ends_with(": bool,") {
        let name: String = c
            .trim_end_matches(": bool,")
            .rsplit(|ch: char| !is_ident(ch))
            .next()
            .unwrap_or("")
            .to_string();
        if prev
            .iter()
            .rev()
            .take(3)
            .any(|l| l.contains("serde(default)"))
        {
            out.push((name, "false".into()));
        }
    }
    out
}

fn is_literal(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() {
        return false;
    }
    let low = v.to_ascii_lowercase();
    matches!(
        low.as_str(),
        "true"
            | "false"
            | "\"no\""
            | "\"yes\""
            | "\"off\""
            | "\"on\""
            | "\"never\""
            | "\"normal\""
            | "\"always\""
    ) || v.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn off_value(v: &str) -> bool {
    let low = v.trim().trim_matches('"').to_ascii_lowercase();
    matches!(
        low.as_str(),
        "false" | "0" | "no" | "off" | "never" | "none" | "normal"
    )
}

fn on_value(v: &str) -> bool {
    let low = v.trim().trim_matches('"').to_ascii_lowercase();
    matches!(low.as_str(), "true" | "1" | "yes" | "on")
}

/// The seconds of an interval's value: `2 * time.Second`, `2500` (a count is no time), `"1s"`.
fn interval_secs(name: &str, v: &str) -> Option<f64> {
    let w = name_words(name);
    if w.iter().any(|x| x == "every" || x == "messages") {
        return None;
    }
    clockscan::duration_value(v, unit_of(name))
}

fn line_raws(f: &SFile, fi: usize, full_sync: bool, raws: &mut Vec<Raw>) {
    let mut seen: HashSet<String> = HashSet::new();
    let file_syncs = f.code.iter().zip(f.text.iter()).any(|(c, t)| {
        !c.is_empty()
            && (sync_prim(f.lang, c, t) || has_token(c, "O_DSYNC") || has_token(c, "O_SYNC"))
    });
    // O_DIRECT matters where the file is opened for writing, not in a reader's code
    let writes_files = f.code.iter().any(|c| {
        ["O_WRONLY", "O_RDWR", "O_CREAT"]
            .iter()
            .any(|x| has_token(c, x))
    });
    for i in 0..f.code.len() {
        let c = &f.code[i];
        let t = &f.text[i];
        if c.trim().is_empty() {
            continue;
        }
        let reads_only = {
            let w = name_words(&format!(
                "{} {}",
                f.func_of[i]
                    .map(|k| f.funcs[k].0.clone())
                    .unwrap_or_default(),
                f.rel
            ));
            w.iter().any(|x| x == "read") && !w.iter().any(|x| x == "write")
        };
        let low = t.to_ascii_lowercase();
        let func = f.func_of[i];
        // SQLite in WAL mode, or with synchronous off or normal
        if low.contains("journal_mode")
            && low.contains("wal")
            && !full_sync
            && seen.insert("wal".into())
        {
            raws.push(Raw {
                file: fi,
                func,
                line: i,
                shape: "setting",
                what: "It opens SQLite in WAL mode and sets no synchronous=FULL. Most builds then run with synchronous=NORMAL, which does not sync at a commit, so a power cut loses the last commits.".into(),
                missing: vec!["synchronous=FULL (or EXTRA)".into()],
                notes: Vec::new(),
                window: None,
                callers_cover: false,
                words: String::new(),
                replace: false,
            });
        }
        let lowc = low.replace([' ', '\'', '"'], "");
        let relaxed = [
            "synchronous=normal",
            "synchronous=off",
            "synchronous=0",
            "synchronous=1",
            "_sync=normal",
            "_sync=off",
            "synchronous,normal",
            "synchronous,off",
        ];
        if relaxed.iter().any(|x| lowc.contains(x)) && seen.insert(format!("sqlite{i}")) {
            raws.push(Raw {
                file: fi,
                func,
                line: i,
                shape: "setting",
                what: "It sets SQLite's synchronous to NORMAL or OFF, so a commit returns before it is synced, and a power cut loses the last commits.".into(),
                missing: vec!["synchronous=FULL (or EXTRA)".into()],
                notes: Vec::new(),
                window: None,
                callers_cover: false,
                words: String::new(),
                replace: false,
            });
        }
        // O_DIRECT without O_DSYNC or a sync in the file (Garnet's default device)
        if has_token(c, "O_DIRECT") && !file_syncs && !f.lang.is(Lang::Sql) {
            let used = token_at(c, "O_DIRECT").iter().any(|p| {
                let before = c[..*p].trim_end();
                let after = c[p + "O_DIRECT".len()..].trim_start();
                before.ends_with('|')
                    || before.ends_with("|=")
                    || (before.ends_with('=') && !before.ends_with("=="))
                    || before.ends_with('(')
                    || after.starts_with('|')
            }) && !c.contains("& O_DIRECT")
                && writes_files
                && !reads_only
                && !c.contains("&O_DIRECT")
                && !c.trim_start().starts_with("const ")
                && !c.contains("#define");
            if used && seen.insert(format!("odirect{}", func.map(|x| x as i64).unwrap_or(-1))) {
                raws.push(Raw {
                    file: fi,
                    func,
                    line: i,
                    shape: "direct",
                    what: "It opens a file with O_DIRECT, and the file has no O_DSYNC and no sync. O_DIRECT passes the page cache but not the disk's write cache, and a growing file's size and blocks are metadata that only a sync writes.".into(),
                    missing: vec!["O_DSYNC, or a sync after the writes".into()],
                    notes: Vec::new(),
                    window: None,
                    callers_cover: false,
                    words: String::new(),
                    replace: false,
                });
            }
        }
        // settings
        let prev: Vec<String> = f.code[i.saturating_sub(3)..i].to_vec();
        // a line that the SQLite rule named is not a setting twice
        let named = seen.contains(&format!("sqlite{i}"));
        for (name, val) in settings_of(f.lang, c, t, &prev)
            .into_iter()
            .filter(|_| !named)
        {
            let Some(kind) = setting_kind(&name) else {
                continue;
            };
            let (shape, what, missing, window) = match kind {
                "switch" if off_value(&val) => (
                    "setting",
                    format!("The setting {name} is {val} by default, so the writes it covers are answered before they are synced."),
                    format!("{name} on, or a sync of the data before the answer"),
                    None,
                ),
                "inverse" if on_value(&val) => (
                    "setting",
                    format!("The setting {name} is {val}, so the writes it covers are not synced."),
                    format!("{name} off"),
                    None,
                ),
                "interval" if !off_value(&val) && val.trim() != "-1" => {
                    let w = interval_secs(&name, &val);
                    (
                        "interval",
                        format!("The setting {name} is {val} by default: the data is synced every so often, so what was answered since the last sync is lost at a power cut."),
                        "a sync before the answer, not on an interval".to_string(),
                        w,
                    )
                }
                _ => continue,
            };
            if !seen.insert(format!("set:{name}")) {
                continue;
            }
            raws.push(Raw {
                file: fi,
                func,
                line: i,
                shape,
                what,
                missing: vec![missing],
                notes: Vec::new(),
                window,
                callers_cover: false,
                words: String::new(),
                replace: false,
            });
        }
    }
}

// ---------------------------------------------------------------- callers and answers

fn resolve(files: &[SFile], raws: Vec<Raw>, main: &HashSet<L>, h: &Helpers) -> Vec<SyncPlace> {
    // the names whose callers matter, three calls deep
    let mut wanted: HashSet<String> = raws
        .iter()
        .filter_map(|r| r.func.map(|k| files[r.file].funcs[k].0.clone()))
        .collect();
    let mut sites: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for _ in 0..3 {
        let new: HashSet<String> = wanted
            .iter()
            .filter(|n| !sites.contains_key(*n))
            .cloned()
            .collect();
        if new.is_empty() {
            break;
        }
        let found = call_sites(files, &new);
        for n in &new {
            sites.insert(n.clone(), found.get(n).cloned().unwrap_or_default());
        }
        // the functions that call them
        for v in found.values() {
            for (fi, line) in v {
                if let Some(k) = files[*fi].func_of[*line] {
                    wanted.insert(files[*fi].funcs[k].0.clone());
                }
            }
        }
    }
    // the syncs of each calling function, by (file, function)
    let mut flows: HashMap<(usize, usize), Vec<(usize, i32)>> = HashMap::new();
    let mut sync_of = |fi: usize, k: usize| -> Vec<(usize, i32)> {
        flows
            .entry((fi, k))
            .or_insert_with(|| {
                let f = &files[fi];
                let (_, a, b) = &f.funcs[k];
                ((*a + 1)..=(*b).min(f.code.len().saturating_sub(1)))
                    .filter(|i| f.func_of[*i] == Some(k))
                    .filter(|i| {
                        let c = &f.code[*i];
                        !c.is_empty()
                            && (sync_prim(f.lang, c, &f.text[*i])
                                || calls_any(c, &h.sync).is_some())
                    })
                    .map(|i| (i, f.depth[i]))
                    .collect()
            })
            .clone()
    };
    let mut out = Vec::new();
    for r in raws {
        let f = &files[r.file];
        let mut notes = r.notes.clone();
        let mut missing = r.missing.clone();
        // a write whose every caller syncs after the call is covered
        let covers_rename = r.callers_cover && r.shape == "rename";
        if covers_rename {
            if let Some(k) = r.func {
                let callers = sites_of(&sites, files, &f.funcs[k].0, r.file);
                if !callers.is_empty()
                    && callers
                        .iter()
                        .all(|(cf, cl)| match files[*cf].func_of[*cl] {
                            Some(ck) => sync_of(*cf, ck).iter().any(|x| x.0 > *cl),
                            None => false,
                        })
                {
                    continue;
                }
            }
        }
        if r.callers_cover
            && r.shape == "write"
            && missing.iter().any(|m| m == "a sync of the file")
        {
            if let Some(k) = r.func {
                let callers = sites_of(&sites, files, &f.funcs[k].0, r.file);
                if !callers.is_empty()
                    && callers
                        .iter()
                        .all(|(cf, cl)| match files[*cf].func_of[*cl] {
                            Some(ck) => {
                                let s = sync_of(*cf, ck);
                                covered(&s, *cl, files[*cf].depth[*cl]) == Some(true)
                                    || s.iter().any(|x| x.0 == *cl)
                            }
                            None => false,
                        })
                {
                    missing.retain(|m| m != "a sync of the file");
                    if missing.is_empty() {
                        continue;
                    }
                }
            }
        }
        let ack = r
            .func
            .and_then(|k| find_ack(files, &sites, r.file, k, r.line));
        // a sync on a timer among the callers on the way to the answer
        if r.shape == "write" {
            if let Some(k) = r.func {
                if let Some(n) = timer_caller(files, &sites, r.file, k) {
                    notes.push(n);
                }
            }
        }
        if ack.is_some()
            && r.shape == "write"
            && !missing.iter().any(|m| m.contains("before the answer"))
        {
            missing.push("a sync before the answer".into());
        }
        let func_name = r.func.map(|k| f.funcs[k].0.clone());
        let words = format!(
            "{} {} {}",
            func_name.clone().unwrap_or_default(),
            f.rel,
            r.words
        );
        let data = has_word(&words, DATA_WORDS);
        let confidence = match r.shape {
            "stream" | "direct" | "setting" | "interval" => "high",
            "periodic" => {
                if data {
                    "high"
                } else {
                    "medium"
                }
            }
            _ => {
                // the author meant it to be durable: a write renamed into place, or a sync that
                // a condition guards
                let meant = r.replace || !r.notes.is_empty();
                if ack.is_some() || meant || (data && (r.shape == "rename" || missing.len() > 1)) {
                    "high"
                } else {
                    "medium"
                }
            }
        };
        out.push(SyncPlace {
            file: f.rel.clone(),
            line: r.line + 1,
            lang: f.lang.name(),
            text: f
                .raw
                .get(r.line)
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            shape: r.shape,
            function: func_name,
            what: r.what,
            missing,
            ack,
            notes,
            window: r.window,
            confidence,
            main_lang: main.contains(&f.lang),
        });
    }
    out.sort_by(|a, b| (a.file.as_str(), a.line).cmp(&(b.file.as_str(), b.line)));
    out
}

/// Words of a name that hold data a system answers for.
const DATA_WORDS: &[&str] = &[
    "wal",
    "journal",
    "segment",
    "segments",
    "chunk",
    "chunks",
    "manifest",
    "meta",
    "metadata",
    "checkpoint",
    "snapshot",
    "dump",
    "aof",
    "queue",
    "diskqueue",
    "sst",
    "page",
    "pages",
    "block",
    "blocks",
    "index",
    "store",
    "storage",
    "db",
    "database",
    "data",
    "state",
    "peer",
    "peers",
    "persist",
    "persister",
    "payload",
    "commit",
    "record",
    "records",
    "table",
    "part",
    "parts",
    "merge",
    "raft",
    "entry",
    "entries",
    "offset",
    "offsets",
    "txn",
    "transaction",
    "message",
    "messages",
    "commitlog",
    "binlog",
    "oplog",
    "redo",
    "repl",
    "replication",
    "fetch",
    "install",
    "artifact",
];

/// The first acknowledgement after `line` in function `k`, or in its callers after the call,
/// three calls deep.
fn find_ack(
    files: &[SFile],
    sites: &HashMap<String, Vec<(usize, usize)>>,
    fi: usize,
    k: usize,
    line: usize,
) -> Option<Ack> {
    let scan = |f: &SFile, fk: usize, from: usize| -> Option<(usize, &'static str)> {
        let end = f.funcs[fk].2.min(f.code.len().saturating_sub(1));
        for i in from..=end {
            if f.func_of[i] != Some(fk) {
                continue;
            }
            let c = &f.code[i];
            if c.is_empty() {
                continue;
            }
            if ack_line(c) {
                return Some((i, "answer"));
            }
            if removal_prim(c) {
                return Some((i, "removal"));
            }
        }
        None
    };
    if let Some((i, kind)) = scan(&files[fi], k, line + 1) {
        let f = &files[fi];
        return Some(Ack {
            path: Vec::new(),
            file: f.rel.clone(),
            line: i + 1,
            text: f.raw[i].trim().to_string(),
            kind,
        });
    }
    // breadth first through the callers
    let mut q: VecDeque<(usize, usize, Vec<String>)> = VecDeque::new();
    q.push_back((fi, k, Vec::new()));
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    seen.insert((fi, k));
    while let Some((cf, ck, path)) = q.pop_front() {
        if path.len() >= 3 {
            continue;
        }
        let name = &files[cf].funcs[ck].0;
        for (sf, sl) in sites_of(sites, files, name, cf) {
            let Some(sk) = files[sf].func_of[sl] else {
                continue;
            };
            if !seen.insert((sf, sk)) {
                continue;
            }
            let g = &files[sf];
            let mut p = path.clone();
            p.push(format!("{} ({}:{})", g.funcs[sk].0, g.rel, sl + 1));
            if let Some((i, kind)) = scan(g, sk, sl) {
                return Some(Ack {
                    path: p,
                    file: g.rel.clone(),
                    line: i + 1,
                    text: g.raw[i].trim().to_string(),
                    kind,
                });
            }
            q.push_back((sf, sk, p));
        }
    }
    None
}

/// A caller of function `k`, two calls deep, that syncs on a timer: its words for the report.
fn timer_caller(
    files: &[SFile],
    sites: &HashMap<String, Vec<(usize, usize)>>,
    fi: usize,
    k: usize,
) -> Option<String> {
    let mut frontier = vec![(fi, k)];
    for _ in 0..2 {
        let mut next = Vec::new();
        for (cf, ck) in frontier {
            let name = &files[cf].funcs[ck].0;
            for (sf, sl) in sites_of(sites, files, name, cf) {
                let Some(sk) = files[sf].func_of[sl] else {
                    continue;
                };
                let g = &files[sf];
                let (_, a, b) = &g.funcs[sk];
                let end = (*b).min(g.code.len().saturating_sub(1));
                for i in (*a + 1)..=end {
                    if g.func_of[i] == Some(sk) && ticker_prim(g.lang, &g.code[i]) {
                        return Some(format!(
                            "its caller {} syncs on a timer ({}:{}: {})",
                            g.funcs[sk].0,
                            g.rel,
                            i + 1,
                            g.raw[i].trim()
                        ));
                    }
                }
                next.push((sf, sk));
            }
        }
        frontier = next;
    }
    None
}

// ---------------------------------------------------------------- ranking and schedules

/// Words of code that reports, logs, caches or tests: a place there is ranked last.
const REPORT_CODE: &[&str] = &[
    "logger",
    "logging",
    "debug",
    "trace",
    "tracing",
    "metric",
    "metrics",
    "stats",
    "cli",
    "bench",
    "benchmark",
    "example",
    "examples",
    "demo",
    "mock",
    "fixture",
    "profile",
    "pprof",
    "coverage",
    "report",
    "html",
    "doc",
    "docs",
    "gen",
    "generate",
    "codegen",
    "tool",
    "tools",
    "script",
    "cache",
    "pid",
    "lockfile",
    "history",
    "cert",
    "certs",
    "tls",
    "screenshot",
    "export",
    "download",
    "fuzz",
    "fuzzer",
    "printer",
    "completion",
    "dev",
    "playground",
    "sample",
];

/// How much a place is worth a first look: higher is first.
///
/// - The shape: a stream synced before it is flushed 6, `O_DIRECT` without a sync 5, a setting
///   that turns syncing off 4, an interval 3, a loop on a timer 2, a rename 2, a write 0.
/// - An acknowledgement found after it: 5 in its own function, 4 through its callers; a removal
///   of old data after it: 3.
/// - A word of the function's or the file's name about stored data (`wal`, `segment`, `meta`,
///   `checkpoint`, `dump`, `queue`): 3.
/// - A word of the function's or the file's name in the commands of hydo.yaml: 3.
/// - A minor language of the repository: -5. Code that logs, reports, caches or tests: -6.
pub fn rank_score(p: &SyncPlace, svcs: &Services) -> i32 {
    let mut s = match p.shape {
        "stream" => 6,
        "direct" => 5,
        "setting" => 4,
        "interval" => 3,
        "periodic" | "rename" => 2,
        _ => 0,
    };
    if let Some(a) = &p.ack {
        s += match (a.kind, a.path.is_empty()) {
            ("removal", _) => 3,
            (_, true) => 5,
            _ => 4,
        };
    }
    let names = name_words(&format!(
        "{} {}",
        p.function.clone().unwrap_or_default(),
        p.file
    ));
    if names.iter().any(|w| DATA_WORDS.contains(&w.as_str())) {
        s += 3;
    }
    if names.iter().any(|w| svcs.words.contains(w)) {
        s += 3;
    }
    if !p.main_lang {
        s -= 5;
    }
    let tool_path = p.file.split('/').any(|seg| {
        let seg = seg.to_ascii_lowercase();
        [
            "tool", "bench", "example", "sample", "demo", "script", "mock", "fixture", "e2e", "cli",
        ]
        .iter()
        .any(|x| seg.contains(x))
    });
    if tool_path || names.iter().any(|w| REPORT_CODE.contains(&w.as_str())) {
        s -= 6;
    }
    s
}

/// What the report shows once: a function, or the line of a setting.
fn place_key(p: &SyncPlace) -> String {
    match (&p.function, p.shape) {
        (Some(f), "write" | "rename" | "stream" | "periodic") => format!("{}#{f}", p.file),
        _ => format!("{}:{}", p.file, p.line),
    }
}

/// The places to aim schedules at, best first: one per function (or setting), its first high
/// place. Medium places get no schedules.
fn targets<'a>(places: &'a [SyncPlace], svcs: &Services) -> Vec<&'a SyncPlace> {
    let mut seen = HashSet::new();
    let mut out: Vec<&SyncPlace> = places
        .iter()
        .filter(|p| p.confidence == "high")
        .filter(|p| seen.insert(place_key(p)))
        .collect();
    out.sort_by_key(|p| {
        let none = svcs.map.run_by(&p.file).is_some_and(|w| w.is_empty());
        (none, -rank_score(p, svcs))
    });
    out
}

/// How many places the text report shows without `--all`.
pub const TOP: usize = 5;

/// The events of the workload that say a write was acknowledged, and the services with
/// `storage:`, read from the folder of hydo.yaml.
#[derive(Default)]
pub struct Workload {
    pub events: Vec<String>,
    pub storage: Vec<String>,
}

const EVENT_ACK_WORDS: &[&str] = &[
    "acked",
    "ack",
    "ok",
    "done",
    "committed",
    "saved",
    "stored",
    "written",
    "persisted",
    "succeeded",
    "confirmed",
    "applied",
    "accepted",
    "published",
    "put",
    "added",
    "created",
];

pub fn workload_near(root: &Path, manifest: Option<&str>) -> Workload {
    let cands: Vec<std::path::PathBuf> = match manifest {
        Some(m) => vec![m.into()],
        None => vec![root.join("hydo").join("hydo.yaml"), root.join("hydo.yaml")],
    };
    let mut w = Workload::default();
    for c in cands {
        let Ok(text) = std::fs::read_to_string(&c) else {
            continue;
        };
        if let Ok(v) = hydo_manifest::yaml::parse_yaml(&text, &c.to_string_lossy()) {
            for svc in v
                .get("services")
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
            {
                if svc.get("storage").is_some() {
                    if let Some(n) = svc.get("name").and_then(|x| x.as_str()) {
                        w.storage.push(n.to_string());
                    }
                }
            }
        }
        let dir = c.parent().unwrap_or(Path::new("."));
        let mut paths = Vec::new();
        crate::rules::walk(dir, &mut paths);
        let mut found: Vec<String> = Vec::new();
        for p in paths.iter().take(4000) {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if clockscan::lang_of(name).is_none() {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(p) else {
                continue;
            };
            for l in t.lines() {
                let low = l.to_ascii_lowercase();
                if !(low.contains("event(")
                    || low.contains("sometimes(")
                    || low.contains("reachable(")
                    || low.contains("hydo-assert"))
                {
                    continue;
                }
                for part in l.split('"').skip(1).step_by(2) {
                    let ok = part.contains('.')
                        && part.split('.').count() == 2
                        && part.chars().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'
                        })
                        && name_words(part)
                            .iter()
                            .any(|x| EVENT_ACK_WORDS.contains(&x.as_str()));
                    if ok && !found.contains(&part.to_string()) {
                        found.push(part.to_string());
                    }
                }
            }
        }
        found.sort();
        w.events = found;
        break;
    }
    w
}

/// One proposed schedule.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub name: &'static str,
    pub schedule: String,
    pub says: String,
}

/// The service that runs a place's code: the only one the map names, else the only one with
/// `storage:`, else `<service>`.
fn service_for(p: &SyncPlace, svcs: &Services, wl: &Workload) -> String {
    if let Some(v) = svcs.map.run_by(&p.file) {
        let v: Vec<String> = v
            .into_iter()
            .filter(|x| x != crate::clockmap::WORKLOAD)
            .collect();
        if v.len() == 1 {
            return v[0].clone();
        }
    }
    if wl.storage.len() == 1 {
        return wl.storage[0].clone();
    }
    if svcs.names.len() == 1 {
        return svcs.names[0].clone();
    }
    "<service>".into()
}

/// The event to aim at: the acknowledgement event that shares the most words with the place.
fn event_for(p: &SyncPlace, wl: &Workload) -> Option<String> {
    let words: HashSet<String> = name_words(&format!(
        "{} {} {}",
        p.function.clone().unwrap_or_default(),
        p.file,
        p.ack.as_ref().map(|a| a.text.clone()).unwrap_or_default()
    ))
    .into_iter()
    .collect();
    wl.events
        .iter()
        .max_by_key(|e| {
            let n = name_words(e).iter().filter(|w| words.contains(*w)).count();
            // the first event in name order wins a tie
            (n, std::cmp::Reverse((*e).clone()))
        })
        .cloned()
}

/// The schedules aimed at a place: one node's power loss right after the acknowledgement that
/// follows it, and a power cut of the whole guest there.
pub fn proposals(p: &SyncPlace, svcs: &Services, wl: &Workload) -> Vec<Proposal> {
    let svc = service_for(p, svcs, wl);
    let steps = svcs.steps.unwrap_or(20).max(4);
    let mid = steps / 2;
    let ev = event_for(p, wl);
    // without hydo.yaml the schedule keeps its placeholder; with it, L= needs storage:
    let has_storage = if svcs.names.is_empty() {
        true
    } else {
        wl.storage.contains(&svc) || (svc.starts_with('<') && !wl.storage.is_empty())
    };
    let mut out = Vec::new();
    if has_storage {
        out.push(match &ev {
            Some(e) => Proposal {
                name: "powerloss-after",
                schedule: format!("disk=cut | run A={e}+10:powerloss-{svc}"),
                says: format!("{svc} loses its power 10 ms after the workload's event {e}, before the kernel writes the data back"),
            },
            None => Proposal {
                name: "powerloss-after",
                schedule: format!("disk=cut | run L={svc}@{mid}+3"),
                says: format!("{svc} loses its power before step {mid}, right after the writes of the steps before it"),
            },
        });
    }
    out.push(match &ev {
        Some(e) => Proposal {
            name: "cut-after",
            schedule: format!("disk=cut | run A={e}+10:cut"),
            says: format!("the whole guest loses its power 10 ms after the workload's event {e}"),
        },
        None => Proposal {
            name: "cut-after",
            schedule: format!("disk=cut | run C={mid}"),
            says: format!("the whole guest loses its power after step {mid}"),
        },
    });
    if let (Some(w), Some(e)) = (p.window, &ev) {
        let ms = ((w * 1000.0) / 2.0).round().max(1.0) as u64;
        out.push(Proposal {
            name: "cut-in-window",
            schedule: format!("disk=cut | run A={e}+{ms}:cut"),
            says: format!(
                "the power goes {ms} ms after the event, inside the window of {} between two syncs",
                seconds_text(w)
            ),
        });
    }
    out
}

// ---------------------------------------------------------------- the reports

pub fn place_json(p: &SyncPlace) -> Value {
    json!({
        "file": p.file,
        "line": p.line,
        "lang": p.lang,
        "text": p.text,
        "shape": p.shape,
        "confidence": p.confidence,
        "function": p.function,
        "what": p.what,
        "missing": p.missing,
        "notes": p.notes,
        "window_seconds": p.window,
        "ack": p.ack.as_ref().map(|a| json!({"path": a.path, "file": a.file, "line": a.line, "text": a.text, "kind": a.kind})),
    })
}

pub fn json_report(dir: &str, a: &SyncAudit, svcs: &Services, wl: &Workload) -> String {
    let t: HashMap<(String, usize), usize> = targets(&a.places, svcs)
        .iter()
        .enumerate()
        .map(|(k, p)| ((p.file.clone(), p.line), k + 1))
        .collect();
    let places: Vec<Value> = a
        .places
        .iter()
        .map(|p| {
            let mut v = place_json(p);
            if let Some(who) = svcs.map.run_by(&p.file) {
                v["run_by"] = who.into();
            }
            if let Some(rank) = t.get(&(p.file.clone(), p.line)) {
                v["rank"] = (*rank).into();
                v["schedules"] = proposals(p, svcs, wl)
                    .iter()
                    .map(|x| json!({"name": x.name, "schedule": x.schedule, "says": x.says}))
                    .collect();
            }
            v
        })
        .collect();
    let mut doc = json!({
        "dir": dir,
        "files_read": a.read,
        "sync": places,
        "tests_left_out": {"functions": a.tests.functions, "modules": a.tests.modules},
    });
    if !svcs.names.is_empty() {
        doc["services_run"] = svcs.map.words().into();
        doc["events"] = wl.events.clone().into();
    }
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn place_text(
    o: &mut String,
    k: usize,
    p: &SyncPlace,
    all: &[SyncPlace],
    svcs: &Services,
    wl: &Workload,
) {
    use std::fmt::Write;
    let _ = writeln!(
        o,
        "\n{}. {}:{}{} [{}, {}]",
        k + 1,
        p.file,
        p.line,
        p.function
            .as_ref()
            .map(|f| format!(", in {f}"))
            .unwrap_or_default(),
        p.shape,
        p.confidence
    );
    let _ = writeln!(o, "     {}", p.text);
    let _ = writeln!(o, "   {}", p.what);
    let _ = writeln!(o, "   Missing: {}.", clockscan::and_words(&p.missing));
    if let Some(a) = &p.ack {
        let how = if a.kind == "removal" {
            "It removes old data after it"
        } else {
            "The answer"
        };
        if a.path.is_empty() {
            let _ = writeln!(o, "   {how}: line {}: {}", a.line, a.text);
        } else {
            let _ = writeln!(
                o,
                "   {how}: through {}, line {}: {}",
                a.path.join(", then "),
                a.line,
                a.text
            );
        }
    }
    for n in &p.notes {
        let _ = writeln!(o, "   Note: {n}.");
    }
    match svcs.map.run_by(&p.file) {
        Some(v) if v.is_empty() => {
            let _ = writeln!(
                o,
                "   No service of hydo.yaml and not the workload runs this code."
            );
        }
        Some(v) => {
            let _ = writeln!(
                o,
                "   Run by {} ({}).",
                clockscan::and_words(&v),
                svcs.map.programs_of(&p.file).join(", ")
            );
        }
        None => {}
    }
    let more: Vec<String> = all
        .iter()
        .filter(|x| {
            x.file == p.file && x.function.is_some() && x.function == p.function && x.line != p.line
        })
        .map(|x| format!("{} ({})", x.line, x.shape))
        .collect();
    if !more.is_empty() {
        let _ = writeln!(
            o,
            "   The same function also has places at lines {}.",
            more.join(", ")
        );
    }
    let _ = writeln!(o, "   The schedules:");
    for s in proposals(p, svcs, wl) {
        let _ = writeln!(o, "     {:<16} {}", s.name, s.schedule);
        let _ = writeln!(o, "     {:<16} {}", "", s.says);
    }
}

pub fn text_report(
    dir: &str,
    a: &SyncAudit,
    svcs: &Services,
    wl: &Workload,
    max: usize,
    all: bool,
) -> String {
    use std::fmt::Write;
    let mut o = String::new();
    let ts = targets(&a.places, svcs);
    let _ = writeln!(
        o,
        "hydo rules scan --kind sync: {} places in {} source files of {dir} can answer a write before it is on the disk",
        ts.len(),
        a.read
    );
    let t = a.tests;
    if t.functions + t.modules > 0 {
        let mut what = Vec::new();
        if t.functions > 0 {
            what.push(format!(
                "{} test function{}",
                t.functions,
                if t.functions == 1 { "" } else { "s" }
            ));
        }
        if t.modules > 0 {
            what.push(format!(
                "{} test module{} or class{}",
                t.modules,
                if t.modules == 1 { "" } else { "s" },
                if t.modules == 1 { "" } else { "es" }
            ));
        }
        let _ = writeln!(
            o,
            "It left out {} inside the source files, besides the test files.",
            clockscan::and_words(&what)
        );
    }
    if !svcs.names.is_empty() {
        let _ = writeln!(o, "{}", svcs.map.words());
    }
    if ts.is_empty() {
        let _ = writeln!(
            o,
            "\nNo write was found that can be answered before it is synced."
        );
        return o;
    }
    let _ = writeln!(
        o,
        "\nA write is on the disk only after a sync of the file (fsync, fdatasync), and a rename only\nafter a sync of its folder. A system that answers before that loses the answered write at a\npower cut, or at one node's power loss. The most likely places first:"
    );
    let shown = if all { max } else { TOP.min(max) };
    for (k, p) in ts.iter().take(shown).enumerate() {
        place_text(&mut o, k, p, &a.places, svcs, wl);
    }
    if !all {
        let keys: HashSet<String> = a.places.iter().map(place_key).collect();
        let more = keys.len().saturating_sub(shown.min(ts.len()));
        if more > 0 {
            let _ = writeln!(o, "\nand {more} more: hydo rules scan --kind sync --all");
        }
    } else {
        if ts.len() > shown {
            let _ = writeln!(
                o,
                "\n... {} more places with schedules (--max N, or --json for all)",
                ts.len() - shown
            );
        }
        let rest: Vec<&SyncPlace> = a.places.iter().filter(|p| p.confidence != "high").collect();
        if !rest.is_empty() {
            let _ = writeln!(o, "\nMedium confidence (no answer was found after the write, and the names do not say that it holds data; read the code before you aim a schedule at it):");
            for p in rest.iter().take(max) {
                let _ = writeln!(o, "  {}:{}: {}", p.file, p.line, p.text);
            }
            if rest.len() > max {
                let _ = writeln!(o, "  ... {} more", rest.len() - max);
            }
        }
    }
    let shown_places: Vec<&&SyncPlace> = ts.iter().take(shown).collect();
    let placeholders = shown_places.iter().any(|p| {
        proposals(p, svcs, wl)
            .iter()
            .any(|x| x.schedule.contains('<'))
    });
    let _ = writeln!(
        o,
        "\nTo try a schedule: hydo explore --schedule \"<schedule>\" (it runs it once)."
    );
    if placeholders {
        let _ = writeln!(o, "Replace <service> with the service of hydo.yaml that runs the code. A power loss of one service (L=, powerloss-) needs storage: on it.");
    }
    if wl.events.is_empty() {
        let _ = writeln!(o, "The schedules cut at step {} of {} workload steps: a workload that prints an event when a write is answered (an SDK's event or sometimes record) gets a cut 10 ms after it instead. hydo guide faults, \"Find the writes acknowledged before they are synced\".", svcs.steps.unwrap_or(20).max(4) / 2, svcs.steps.unwrap_or(20).max(4));
    } else {
        let _ = writeln!(o, "The schedules aim at the workload's events of an answered write ({}). hydo guide faults, \"Find the writes acknowledged before they are synced\".", wl.events.join(", "));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit_of(files: &[(&str, &str)]) -> Vec<SyncPlace> {
        audit_texts(files).places
    }

    fn at<'a>(ps: &'a [SyncPlace], file: &str, func: &str) -> Option<&'a SyncPlace> {
        ps.iter()
            .find(|p| p.file == file && p.function.as_deref() == Some(func))
    }

    /// NSQ's go-diskqueue v1.1.0, shortened: a write answered at once and synced on a timer,
    /// and the metadata renamed without a sync of the folder.
    const DISKQUEUE: &str = r#"package diskqueue

func (d *diskQueue) writeOne(data []byte) error {
	if d.writeFile == nil {
		d.writeFile, err = os.OpenFile(name, os.O_RDWR|os.O_CREATE, 0600)
		if err != nil {
			return err
		}
	}
	_, err = d.writeFile.Write(d.writeBuf.Bytes())
	d.writePos += totalBytes
	return err
}

func (d *diskQueue) sync() error {
	if d.writeFile != nil {
		err := d.writeFile.Sync()
		if err != nil {
			return err
		}
	}
	return d.persistMetaData()
}

func (d *diskQueue) persistMetaData() error {
	f, err = os.OpenFile(tmpFileName, os.O_RDWR|os.O_CREATE, 0600)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(f, "%d\n", d.depth)
	f.Sync()
	f.Close()
	return os.Rename(tmpFileName, fileName)
}

func (d *diskQueue) ioLoop() {
	syncTicker := time.NewTicker(d.syncTimeout)
	for {
		if needSync {
			err = d.sync()
		}
		select {
		case dataWrite := <-d.writeChan:
			count++
			d.writeResponseChan <- d.writeOne(dataWrite)
		case <-syncTicker.C:
			needSync = true
		}
	}
}
"#;

    #[test]
    fn the_disk_queue_answers_before_its_sync() {
        let ps = audit_of(&[("diskqueue.go", DISKQUEUE)]);
        let w = at(&ps, "diskqueue.go", "writeOne").expect("writeOne");
        assert_eq!(w.shape, "write");
        assert_eq!(w.line, 10);
        assert!(w.missing.iter().any(|m| m == "a sync of the file"), "{w:?}");
        let a = w.ack.as_ref().expect("an answer");
        assert_eq!(a.line, 45);
        assert_eq!(a.path, vec!["ioLoop (diskqueue.go:45)".to_string()]);
        assert!(
            w.notes
                .iter()
                .any(|n| n.contains("ioLoop syncs on a timer")),
            "{w:?}"
        );
        assert_eq!(w.confidence, "high");
        let m = at(&ps, "diskqueue.go", "persistMetaData").expect("persistMetaData");
        assert_eq!(m.shape, "rename");
        assert_eq!(m.line, 33);
        assert_eq!(
            m.missing,
            vec!["a sync of the folder after the rename".to_string()]
        );
        // the loop syncs on a timer
        let l = at(&ps, "diskqueue.go", "ioLoop").expect("ioLoop");
        assert_eq!(l.shape, "periodic");
        // the sync function itself writes nothing unsynced
        assert!(at(&ps, "diskqueue.go", "sync").is_none(), "{ps:?}");
    }

    #[test]
    fn a_synced_save_is_no_place_and_an_unsynced_one_is() {
        // the fuzz-vm example's two saves, with tempfile
        let text = r#"use std::io::Write;

pub fn save(text: String) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new_in(".").map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    f.persist("saved.txt").map_err(|e| e.to_string())?;
    Ok(())
}

pub fn save_synced(text: String) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new_in(".").map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    f.as_file().sync_all().map_err(|e| e.to_string())?;
    f.persist("saved.txt").map_err(|e| e.to_string())?;
    std::fs::File::open(".")
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}
"#;
        let ps = audit_of(&[("src/lib.rs", text)]);
        let s = at(&ps, "src/lib.rs", "save").expect("save");
        assert_eq!(s.line, 5);
        assert_eq!(
            s.missing,
            vec![
                "a sync of the file".to_string(),
                "a sync of the folder after the rename".to_string()
            ]
        );
        assert!(at(&ps, "src/lib.rs", "save_synced").is_none(), "{ps:?}");
    }

    #[test]
    fn a_sync_inside_a_condition_does_not_cover_the_write() {
        // SWUpdate's raw handler: the sync only when the write protection was off
        let text = r#"static int install_raw_image(struct img_type *img)
{
	int prot_stat = blkprotect(img, false);
	fdout = open(img->device, O_RDWR);
	ret = copyimage(&fdout, img, NULL);
	if (prot_stat == 1) {
		fsync(fdout);
		blkprotect(img, true);
	}
	close(fdout);
	return ret;
}
"#;
        let ps = audit_of(&[("handlers/raw_handler.c", text)]);
        let p = at(&ps, "handlers/raw_handler.c", "install_raw_image").expect("place");
        assert_eq!(p.line, 5);
        assert!(
            p.notes[0].contains("the sync at line 7 runs only under a condition"),
            "{p:?}"
        );
    }

    #[test]
    fn a_stream_synced_before_its_buffer_is_flushed() {
        // Mender's version file
        let text = r#"error::Error Parse(const Config &conf) {
	{
		ofstream myfile(artifact_script_version_file);
		if (!myfile.good()) {
			return error::Error();
		}
		myfile << to_string(conf.artifact_scripts_version);
		auto err = path::DataSyncRecursively(conf.artifact_scripts_filesystem_path);
		if (err != error::NoError) {
			return err;
		}
	}
	return error::NoError;
}
"#;
        let sync = r#"error::Error DataSyncRecursively(const string &dir) {
	int fd = open(dir.c_str(), O_RDONLY);
	if (fdatasync(fd) != 0) {
		return error::Error();
	}
	return error::NoError;
}
"#;
        let ps = audit_of(&[("src/header.cpp", text), ("src/path.cpp", sync)]);
        let p = ps.iter().find(|p| p.shape == "stream").expect("the stream");
        assert_eq!(p.line, 7);
        assert_eq!(
            p.missing,
            vec!["a flush or a close of myfile before the sync".to_string()]
        );
        // closed first: no place
        let fixed = text.replace(
            "\t\tauto err = path::Data",
            "\t\tmyfile.close();\n\t\tauto err = path::Data",
        );
        let ps = audit_of(&[("src/header.cpp", &fixed), ("src/path.cpp", sync)]);
        assert!(!ps.iter().any(|p| p.shape == "stream"), "{ps:?}");
    }

    #[test]
    fn settings_that_turn_syncing_off() {
        let ps = audit_of(&[
            (
                "src/Storages/MergeTreeSettings.cpp",
                "    DECLARE(Bool, fsync_after_insert, false, R\"(\n    DECLARE(Bool, fsync_metadata, true, R\"(\n",
            ),
            (
                "src/Interpreters/Drop.cpp",
                "void drop() {\n  ast_drop_query.sync = false;\n}\n",
            ),
            (
                "src/Storages/StorageDistributed.cpp",
                "    factory.registerStorage(\"Distributed\", R\"(\nfsync_after_insert=0,\n)\");\n",
            ),
            (
                "src/config/config.cc",
                "      {\"rocksdb.write_options.sync\", false, new YesNoField(&rocks_db.write_options.sync, false)},\n      {\"repl-diskless-sync\", false, new YesNoField(&x, false)},\n",
            ),
            (
                "src/util/config.rs",
                "pub struct Config {\n\t/// Whether to fsync\n\t#[serde(default)]\n\tpub metadata_fsync: bool,\n}\n",
            ),
            (
                "pkg/db/db.go",
                "package db\n\nfunc open() {\n\tengine, err = xorm.NewEngine(\"sqlite3\", dbPath+\"?_busy_timeout=5000&_journal_mode=WAL\")\n}\n",
            ),
            (
                "nsqd/options.go",
                "package nsqd\n\nfunc NewOptions() *Options {\n\treturn &Options{\n\t\tSyncEvery:   2500,\n\t\tSyncTimeout: 2 * time.Second,\n\t}\n}\n",
            ),
        ]);
        let names: Vec<(String, usize, &str)> = ps
            .iter()
            .map(|p| (p.file.clone(), p.line, p.shape))
            .collect();
        assert!(
            names.contains(&("src/Storages/MergeTreeSettings.cpp".into(), 1, "setting")),
            "{names:?}"
        );
        assert!(
            !names.contains(&("src/Storages/MergeTreeSettings.cpp".into(), 2, "setting")),
            "{names:?}"
        );
        assert!(
            names.contains(&("src/config/config.cc".into(), 1, "setting")),
            "{names:?}"
        );
        assert!(
            !names.iter().any(|n| n.0 == "src/Interpreters/Drop.cpp"),
            "{names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.0 == "src/Storages/StorageDistributed.cpp"),
            "{names:?}"
        );
        assert!(
            !names.contains(&("src/config/config.cc".into(), 2, "setting")),
            "{names:?}"
        );
        assert!(
            names.contains(&("src/util/config.rs".into(), 4, "setting")),
            "{names:?}"
        );
        assert!(
            names.contains(&("pkg/db/db.go".into(), 4, "setting")),
            "{names:?}"
        );
        assert!(
            names.contains(&("nsqd/options.go".into(), 5, "interval")),
            "{names:?}"
        );
        let t = ps
            .iter()
            .find(|p| p.file == "nsqd/options.go" && p.line == 6)
            .expect("timeout");
        assert_eq!(t.window, Some(2.0));
    }

    #[test]
    fn o_direct_without_a_sync() {
        let text = "int File::Open(int flags) {\n  flags |= O_DIRECT;\n  fd_ = ::open(name, flags | O_RDWR, 0600);\n  return fd_;\n}\n";
        let ps = audit_of(&[("cc/src/device/file_linux.cc", text)]);
        assert!(
            ps.iter().any(|p| p.shape == "direct" && p.line == 2),
            "{ps:?}"
        );
        let synced = text.replace("  return fd_;", "  fdatasync(fd_);\n  return fd_;");
        let ps = audit_of(&[("cc/src/device/file_linux.cc", &synced)]);
        assert!(!ps.iter().any(|p| p.shape == "direct"), "{ps:?}");
        // O_DIRECTORY is no O_DIRECT, and a test of the flag sets nothing
        let dir = "int syncdir(const char *p) {\n  int fd = ::open(p, O_RDONLY | O_DIRECTORY);\n  if (flags & O_DIRECT) {\n  }\n  return fd;\n}\n";
        let ps = audit_of(&[("src/dir.cc", dir)]);
        assert!(!ps.iter().any(|p| p.shape == "direct"), "{ps:?}");
    }

    #[test]
    fn erlang_segments_without_a_sync() {
        let text = "-module(osiris_log).\n\nwrite_chunk(Chunk, #?MODULE{fd = Fd} = State) ->\n    ok = file:write(Fd, Chunk),\n    State.\n\nreplica_ack(State) ->\n    {reply, ok, write_chunk(c, State)}.\n";
        let ps = audit_of(&[("src/osiris_log.erl", text)]);
        let p = at(&ps, "src/osiris_log.erl", "write_chunk").expect("write_chunk");
        assert_eq!(p.line, 4);
        assert_eq!(p.lang, "erlang");
        assert!(p.ack.is_some(), "{p:?}");
    }

    #[test]
    fn a_caller_that_syncs_covers_the_write() {
        let text = "package s\n\nfunc (s *S) writeRec(b []byte) error {\n\t_, err := s.file.Write(b)\n\treturn err\n}\n\nfunc (s *S) Append(b []byte) error {\n\tif err := s.writeRec(b); err != nil {\n\t\treturn err\n\t}\n\treturn s.file.Sync()\n}\n\nfunc (s *S) open() {\n\ts.file, _ = os.OpenFile(p, os.O_WRONLY|os.O_APPEND, 0600)\n}\n";
        let ps = audit_of(&[("s/s.go", text)]);
        assert!(at(&ps, "s/s.go", "writeRec").is_none(), "{ps:?}");
    }
}
