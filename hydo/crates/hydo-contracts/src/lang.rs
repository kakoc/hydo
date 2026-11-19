//! The contract language: `// hydo:` comments in a source file, and the expressions in them.
//!
//! ```text
//! // hydo: requires amount > 0 && from != to
//! // hydo: ensures result == l.balance && l.balance == old(l.balance) - amount
//! // hydo: input amount in 1..=10_000
//! fn apply(l: &mut Ledger, from: u32, to: u32, amount: u64) -> u64
//! ```
//!
//! A run of `// hydo:` lines belongs to the next function declaration; comment lines,
//! attributes (`#[inline(never)]`) and Go directives (`//go:noinline`) may stand between them.
//! An expression names the function's parameters, `result` (in `ensures`), fields with `.`
//! (through pointers and references too), integers, `true` and `false`, and `old(e)`: the value
//! of `e` at the call's entry. A parameter in `ensures` is its value at entry; a field read
//! through it is read at return unless it is inside `old()`. Operators, loosest first: `||`,
//! `&&`, the comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`), `+ -`, `* / %`, then `!` and unary
//! `-`. Integers are exact: no overflow, no wrapping. A call of the program's code is refused:
//! it would run the program, which a check must not do (docs/design/contracts.md). Since
//! Milestone 255 an expression may use string literals (`s == "ok"`), elements (`v[k]`), Go's
//! `nil`, and the functions `len`, `is_some`, `is_none`, `is_ok`, `is_err`, `some`, `ok` and
//! `err`, which only read.

use crate::plan::{BinOp, ClauseKind};
use std::fmt;

/// An expression as written, before its names are bound to reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Ast {
    Int(i128),
    /// A floating-point literal (`50.5`; Milestone 285).
    Float(f64),
    Bool(bool),
    /// A string literal (`s == "hello"`; Milestone 255).
    Str(String),
    /// A parameter, `result`, or a Go result's name.
    Name(String),
    /// A field: `l.balance` is `Field(Name("l"), "balance")`; `result.1` is a tuple field (Rust)
    /// or the second result (Go).
    Field(Box<Ast>, String),
    /// An element: `v[k]` (Milestone 255).
    Index(Box<Ast>, Box<Ast>),
    /// One of the built-in functions over a value (Milestone 255): they read, they run nothing.
    Call(Builtin, Box<Ast>),
    Old(Box<Ast>),
    Not(Box<Ast>),
    Neg(Box<Ast>),
    Bin(BinOp, Box<Ast>, Box<Ast>),
    /// `matches(e, <pattern>)` (Milestone 307): whether a value has the shape of a pattern, such
    /// as `matches(result, Ok(Complete(_)))`. Read by function fuzzing only.
    Matches(Box<Ast>, Pat),
}

/// A pattern of `matches(e, <pattern>)` (Milestone 307): `_`, a literal, a variant's name with
/// its fields' patterns (`Complete(_)`, `Some(0)`), or a tuple (`(_, 0)`). A variant's name may
/// carry its enumeration's path (`Status::Complete`); a bare name matches the variant whatever
/// its fields are.
#[derive(Clone, Debug, PartialEq)]
pub enum Pat {
    Any,
    Int(i128),
    Bool(bool),
    Str(String),
    /// A variant's name (the path's last segment) and, when written, its fields' patterns.
    Variant(String, Option<Vec<Pat>>),
    Tuple(Vec<Pat>),
}

impl fmt::Display for Pat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |v: &[Pat]| v.iter().map(Pat::to_string).collect::<Vec<_>>().join(", ");
        match self {
            Pat::Any => write!(f, "_"),
            Pat::Int(v) => write!(f, "{v}"),
            Pat::Bool(b) => write!(f, "{b}"),
            Pat::Str(s) => write!(f, "{s:?}"),
            Pat::Variant(n, None) => write!(f, "{n}"),
            Pat::Variant(n, Some(v)) => write!(f, "{n}({})", list(v)),
            Pat::Tuple(v) if v.len() == 1 => write!(f, "({},)", v[0]),
            Pat::Tuple(v) => write!(f, "({})", list(v)),
        }
    }
}

/// The functions an expression may call (Milestone 255). Each reads the value it is given; none
/// runs the program's code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    /// The length of a string, a vector or a slice.
    Len,
    /// Which variant a Rust `Option` or `Result` is.
    IsSome,
    IsNone,
    IsOk,
    IsErr,
    /// The value inside a variant: `some(o)`, `ok(r)`, `err(r)` (the clause is undecided for a
    /// call where the value is the other variant).
    Some,
    Ok,
    Err,
    /// Whether a list's elements are in order, each at most the next (Milestone 307): the
    /// quantifier that a sorted input needs (`requires sorted(v)`). Read by function fuzzing
    /// only.
    Sorted,
}

impl Builtin {
    pub fn of(name: &str) -> Option<Builtin> {
        Some(match name {
            "len" => Builtin::Len,
            "is_some" => Builtin::IsSome,
            "is_none" => Builtin::IsNone,
            "is_ok" => Builtin::IsOk,
            "is_err" => Builtin::IsErr,
            "some" => Builtin::Some,
            "ok" => Builtin::Ok,
            "err" => Builtin::Err,
            "sorted" => Builtin::Sorted,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Len => "len",
            Builtin::IsSome => "is_some",
            Builtin::IsNone => "is_none",
            Builtin::IsOk => "is_ok",
            Builtin::IsErr => "is_err",
            Builtin::Some => "some",
            Builtin::Ok => "ok",
            Builtin::Err => "err",
            Builtin::Sorted => "sorted",
        }
    }
}

impl fmt::Display for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ast::Int(v) => write!(f, "{v}"),
            Ast::Float(v) => write!(f, "{v:?}"),
            Ast::Bool(b) => write!(f, "{b}"),
            Ast::Str(s) => write!(f, "{s:?}"),
            Ast::Name(n) => write!(f, "{n}"),
            Ast::Field(x, n) => write!(f, "{x}.{n}"),
            Ast::Index(x, i) => write!(f, "{x}[{i}]"),
            Ast::Call(b, x) => write!(f, "{}({x})", b.name()),
            Ast::Old(x) => write!(f, "old({x})"),
            Ast::Not(x) => write!(f, "!{x}"),
            Ast::Neg(x) => write!(f, "-{x}"),
            Ast::Bin(..) => write!(f, "(...)"),
            Ast::Matches(x, p) => write!(f, "matches({x}, {p})"),
        }
    }
}

impl Ast {
    /// The name a place expression starts from (`l` in `l.balance`, `v` in `len(v)`).
    pub fn root(&self) -> Option<&str> {
        match self {
            Ast::Name(n) => Some(n),
            Ast::Field(x, _) | Ast::Index(x, _) | Ast::Call(_, x) => x.root(),
            _ => None,
        }
    }
}

/// A clause as written.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcClause {
    pub kind: ClauseKind,
    pub text: String,
    pub line: u32,
    pub ast: Ast,
}

/// An input line as written, for function fuzzing: `input <path> <part>...`, each part one of
/// `in <lo>..<hi>` (or `..=`) for an integer, `len <lo>..<hi>` (or `..=`, or one number) for
/// the length of a string or a list, `chars <set>` for a string's characters, and
/// `like "<text>", ...` for strings like the examples (Milestone 282), also `b"<bytes>"` for a list
/// of bytes (Milestone 289). The path is a parameter,
/// a field under one (`l.balance`), or the elements of a list (`v[]`); `len(v) in a..b` is
/// `v len a..b`.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcInput {
    pub name: String,
    /// `in`: the values, both bounds included.
    pub range: Option<(i128, i128)>,
    /// `len`: the lengths, both bounds included (a string's in bytes, as `len()` counts).
    pub len: Option<(i128, i128)>,
    /// `chars`: the characters of a string.
    pub chars: Option<Vec<char>>,
    /// `like`: example strings.
    pub like: Vec<String>,
    /// `like b"..."`: example byte strings, for a list of bytes (Milestone 289).
    pub like_bytes: Vec<Vec<u8>>,
    pub text: String,
    pub line: u32,
}

/// The character sets that `chars` names, besides a quoted set.
pub const CHAR_SETS: &[(&str, &str)] = &[
    ("ascii", "the printable ASCII characters, from space to `~`"),
    ("alnum", "`a-z`, `A-Z` and `0-9`"),
    ("alpha", "`a-z` and `A-Z`"),
    ("digits", "`0-9`"),
    ("hex", "`0-9`, `a-f` and `A-F`"),
    ("lower", "`a-z`"),
    ("upper", "`A-Z`"),
    (
        "any",
        "hydo's default mix: ASCII, a newline, a NUL, and characters of 2, 3 and 4 bytes",
    ),
];
/// When a recorded value is read (Milestone 285).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Entry,
    Return,
    /// At the entry and again at the return (the default).
    Both,
}

/// A value to record as written: `record <expr> [as <name>] [at entry|at return]`, or
/// `key <expr>` (Milestone 285, docs/design/value-history.md).
#[derive(Clone, Debug, PartialEq)]
pub struct SrcRecord {
    pub ast: Ast,
    /// The `as` name, else the expression as written.
    pub name: String,
    pub when: When,
    /// A `key` line: what the history follows, read at the entry.
    pub key: bool,
    pub text: String,
    pub line: u32,
}

/// A property of a recorded value along a key's history (Milestone 291,
/// docs/design/value-history.md): `keep <name> [once set] [from <function>] [to <function>]
/// [across boots]`.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcKeep {
    /// The recorded value's name (a `record` line's `as` name, or its expression).
    pub name: String,
    /// `once set`: once a stop recorded the value, no later stop records it absent. Without it,
    /// every judged stop records the value of its reference stop.
    pub once_set: bool,
    /// `from <function>`: the reference is the last stop of this function before the judged one
    /// (without it: the key's first stop that recorded the value).
    pub from: Option<String>,
    /// `to <function>`: only this function's stops are judged (without it: every stop).
    pub to: Option<String>,
    /// `across boots`: a reference may be in an earlier boot (without it, only in the same boot).
    pub across_boots: bool,
    /// The line as written after `// hydo:`.
    pub text: String,
    pub line: u32,
}

/// The contract of one function in a source file.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcContract {
    /// The function's name as declared (`apply`).
    pub function: String,
    /// The line of the declaration (1-based).
    pub line: u32,
    pub clauses: Vec<SrcClause>,
    pub inputs: Vec<SrcInput>,
    /// `with T = i64`: the concrete type that function fuzzing gives a generic function's type
    /// parameter (Milestone 282).
    pub with: Vec<(String, String)>,
    /// The values the function records for the value history (Milestone 285).
    pub records: Vec<SrcRecord>,
    /// The properties of recorded values declared next to the function (Milestone 291).
    pub keeps: Vec<SrcKeep>,
    /// `durable <check>` (Milestone 292, docs/design/function-fuzzing.md, "Inside the virtual
    /// machine"): the property function that `hydo fuzz --vm` calls after a power cut for each
    /// call of this function, with the call's input and its result; with its line.
    pub durable: Option<(String, u32)>,
    /// `build <Type> from <constructor>(<name>: <type>, ...)` (Milestone 307): how function
    /// fuzzing builds a value of a type that it cannot generate field by field.
    pub builds: Vec<SrcBuild>,
    /// `read <Type> as <type> by <method>()` (Milestone 307): how function fuzzing reads such a
    /// value after a call.
    pub reads: Vec<SrcRead>,
}

/// A `build` line (Milestone 307): `build Bytes from Bytes::from(v: Vec<u8>)`. hydo generates
/// the constructor's arguments and passes the value that the constructor returns.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcBuild {
    /// The type as written (`Bytes`).
    pub ty: String,
    /// The constructor's path as written (`Bytes::from`).
    pub ctor: String,
    /// The constructor's arguments: each name and type as written.
    pub args: Vec<(String, String)>,
    pub line: u32,
}

/// A `read` line (Milestone 307): `read Bytes as Vec<u8> by to_vec()`. hydo reads a value of
/// the type through the method, as a value of the second type.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcRead {
    pub ty: String,
    pub view: String,
    pub method: String,
    pub line: u32,
}

/// A mistake in a contract, with its line.
#[derive(Clone, Debug, PartialEq)]
pub struct LangError {
    pub line: u32,
    pub message: String,
}

impl fmt::Display for LangError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The text after `// hydo:` on a line, when the line is a contract line.
fn contract_text(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix("// hydo:")
        .map(str::trim)
        // `// hydo: error path` and `// hydo: no error path` mark error paths, not contracts
        // (Milestone 306)
        .filter(|t| crate::paths::decl_of(t).is_none())
}

/// The name a function declaration line declares: Rust `fn name`, Go `func name(` or
/// `func (recv) name(`.
pub fn declared_function(line: &str) -> Option<String> {
    let t = line.trim_start();
    let ident = |s: &str| -> Option<String> {
        let n: String = s
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!n.is_empty()).then_some(n)
    };
    if let Some(rest) = t.strip_prefix("func ") {
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix('(') {
            let close = r.find(')')?;
            return ident(r[close + 1..].trim_start());
        }
        return ident(rest);
    }
    // Rust: qualifiers, then `fn`
    let mut w = t;
    loop {
        if let Some(r) = w.strip_prefix("fn ") {
            return ident(r.trim_start());
        }
        let q = [
            "pub(crate) ",
            "pub(super) ",
            "pub ",
            "const ",
            "async ",
            "unsafe ",
            "default ",
        ];
        if let Some(p) = q.iter().find(|p| w.starts_with(**p)) {
            w = w[p.len()..].trim_start();
            continue;
        }
        if let Some(r) = w.strip_prefix("extern ") {
            let r = r.trim_start();
            w = match r.strip_prefix('"') {
                Some(r2) => r2.split_once('"').map(|x| x.1).unwrap_or("").trim_start(),
                None => r,
            };
            continue;
        }
        if let Some(r) = w.strip_prefix("pub(in ") {
            w = r.split_once(')').map(|x| x.1).unwrap_or("").trim_start();
            continue;
        }
        return None;
    }
}

/// Every contract in a source file's text. A contract line that does not parse, or a run of
/// contract lines with no function after it, is an error.
pub fn scan(text: &str) -> (Vec<SrcContract>, Vec<LangError>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut errs = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if contract_text(lines[i]).is_none() {
            i += 1;
            continue;
        }
        let first = i;
        let mut clauses = Vec::new();
        let mut inputs = Vec::new();
        let mut with = Vec::new();
        let mut records: Vec<SrcRecord> = Vec::new();
        let mut keeps: Vec<SrcKeep> = Vec::new();
        let mut durable: Option<(String, u32)> = None;
        let mut builds: Vec<SrcBuild> = Vec::new();
        let mut reads: Vec<SrcRead> = Vec::new();
        while i < lines.len() {
            let l = lines[i].trim_start();
            if let Some(t) = contract_text(l) {
                match parse_line(t, i as u32 + 1) {
                    Ok(Line::Clause(c)) => clauses.push(c),
                    Ok(Line::Input(x)) => inputs.push(x),
                    Ok(Line::With(w)) => with.extend(w),
                    Ok(Line::Record(r)) if r.key && records.iter().any(|x| x.key) => {
                        errs.push(LangError {
                            line: r.line,
                            message: "a function has one `key` line".into(),
                        })
                    }
                    Ok(Line::Record(r)) => records.push(r),
                    Ok(Line::Keep(k)) => keeps.push(k),
                    Ok(Line::Durable(..)) if durable.is_some() => errs.push(LangError {
                        line: i as u32 + 1,
                        message: "a function has one `durable` line".into(),
                    }),
                    Ok(Line::Durable(n, l)) => durable = Some((n, l)),
                    Ok(Line::Build(b)) => builds.push(b),
                    Ok(Line::Read(r)) => reads.push(r),
                    Err(e) => errs.push(e),
                }
            } else if !(l.starts_with("//") || l.starts_with("#[") || l.starts_with("#![")) {
                break;
            }
            i += 1;
        }
        match lines.get(i).and_then(|l| declared_function(l)) {
            Some(function) => out.push(SrcContract {
                function,
                line: i as u32 + 1,
                clauses,
                inputs,
                with,
                records,
                keeps,
                durable,
                builds,
                reads,
            }),
            None => errs.push(LangError {
                line: first as u32 + 1,
                message: "a `// hydo:` contract must stand right before a function declaration \
                          (only comments and attributes between them)"
                    .into(),
            }),
        }
        i += 1;
    }
    (out, errs)
}

enum Line {
    Clause(SrcClause),
    Input(SrcInput),
    With(Vec<(String, String)>),
    Record(SrcRecord),
    Keep(SrcKeep),
    /// `durable <check>`: the check's name and the line.
    Durable(String, u32),
    Build(SrcBuild),
    Read(SrcRead),
}

/// `build <Type> from <constructor>(<name>: <type>, ...)` (Milestone 307).
fn parse_build(t: &str, line: u32) -> Result<SrcBuild, String> {
    let usage = "write `build <Type> from <constructor>(<name>: <type>, ...)`, for example \
                 `build Bytes from Bytes::from(v: Vec<u8>)`";
    let (ty, rest) = t
        .split_once(" from ")
        .ok_or_else(|| format!("`build {t}`: {usage}"))?;
    let (ty, rest) = (ty.trim(), rest.trim());
    let open = rest
        .find('(')
        .ok_or_else(|| format!("`build {t}`: {usage}"))?;
    if !rest.ends_with(')') || ty.is_empty() {
        return Err(format!("`build {t}`: {usage}"));
    }
    let ctor = rest[..open].trim();
    if !ok_path(ctor) || !ok_path(ty) {
        return Err(format!("`build {t}`: {usage}"));
    }
    let mut args = Vec::new();
    for a in crate::fuzz::rust::split_top(&rest[open + 1..rest.len() - 1], ',') {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        let (n, ty) = a
            .split_once(':')
            .ok_or_else(|| format!("`build {t}`: the argument `{a}` has no type; {usage}"))?;
        let n = n.trim();
        if n.is_empty() || !n.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(format!("`build {t}`: `{n}` is not a name; {usage}"));
        }
        args.push((n.to_string(), ty.trim().to_string()));
    }
    Ok(SrcBuild {
        ty: ty.to_string(),
        ctor: ctor.to_string(),
        args,
        line,
    })
}

/// Whether `p` is a path or a type of a `build` or `read` line: names and `::`, with type
/// arguments in angle brackets that may hold commas, spaces, references and arrays
/// (`ArrayVec<u8, 8>`, `ArrayVec::<u8, 8>::from_iter`; Milestone 318).
fn ok_path(p: &str) -> bool {
    let (mut d, mut ok) = (0i32, !p.is_empty());
    for c in p.chars() {
        match c {
            '<' => d += 1,
            '>' => d -= 1,
            ',' | ' ' | '&' | '[' | ']' | ';' | '(' | ')' if d > 0 => {}
            c if c.is_alphanumeric() || c == '_' || c == ':' => {}
            _ => ok = false,
        }
        ok &= d >= 0;
    }
    ok && d == 0
}

/// `read <Type> as <type> by <method>()` (Milestone 307).
fn parse_read(t: &str, line: u32) -> Result<SrcRead, String> {
    let usage = "write `read <Type> as <type> by <method>()`, for example \
                 `read Bytes as Vec<u8> by to_vec()`";
    let (ty, rest) = t
        .split_once(" as ")
        .ok_or_else(|| format!("`read {t}`: {usage}"))?;
    let (view, method) = rest
        .rsplit_once(" by ")
        .ok_or_else(|| format!("`read {t}`: {usage}"))?;
    let method = method.trim();
    let method = method.strip_suffix("()").unwrap_or(method);
    // a method's name, also with a turbofish (`collect::<Vec<u8>>`; Milestone 318)
    let (name, fish) = match method.split_once("::<") {
        Some((n, f)) => (n, Some(format!("<{f}"))),
        None => (method, None),
    };
    if ty.trim().is_empty()
        || !ok_path(ty.trim())
        || view.trim().is_empty()
        || name.is_empty()
        || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
        || fish.is_some_and(|f| !f.ends_with('>') || !ok_path(&f[1..f.len() - 1]))
    {
        return Err(format!("`read {t}`: {usage}"));
    }
    Ok(SrcRead {
        ty: ty.trim().to_string(),
        view: view.trim().to_string(),
        method: method.to_string(),
        line,
    })
}

/// `keep <name> [once set] [from <function>] [to <function>] [across boots]` (Milestone 291).
fn parse_keep(t: &str, line: u32) -> Result<SrcKeep, String> {
    let usage = "write `keep <name> [once set] [from <function>] [to <function>] [across boots]`";
    let words: Vec<&str> = t.split_whitespace().collect();
    let Some(name) = words.first() else {
        return Err(format!("`keep` names no value: {usage}"));
    };
    if ["once", "from", "to", "across"].contains(name) {
        return Err(format!("`keep {}` names no value first: {usage}", t.trim()));
    }
    let mut k = SrcKeep {
        name: name.to_string(),
        once_set: false,
        from: None,
        to: None,
        across_boots: false,
        text: format!("keep {}", words.join(" ")),
        line,
    };
    let mut i = 1;
    while i < words.len() {
        let next = words.get(i + 1).copied();
        match (words[i], next) {
            ("once", Some("set")) if !k.once_set => k.once_set = true,
            ("across", Some("boots")) if !k.across_boots => k.across_boots = true,
            ("from", Some(f)) if k.from.is_none() && !is_keep_word(f) => k.from = Some(f.into()),
            ("to", Some(f)) if k.to.is_none() && !is_keep_word(f) => k.to = Some(f.into()),
            (w, _) => {
                return Err(format!(
                    "`keep {}`: `{w}` is not understood here; {usage}, each part once",
                    t.trim()
                ))
            }
        }
        i += 2;
    }
    Ok(k)
}

fn is_keep_word(w: &str) -> bool {
    ["once", "from", "to", "across"].contains(&w)
}

/// `record <expr> [as <name>] [at entry|at return]` and `key <expr>` (Milestone 285).
fn parse_record(t: &str, line: u32, key: bool) -> Result<SrcRecord, String> {
    let mut rest = t.trim();
    let mut when = if key { When::Entry } else { When::Both };
    for (suffix, w) in [(" at entry", When::Entry), (" at return", When::Return)] {
        if let Some(r) = rest.strip_suffix(suffix) {
            if key {
                return Err("a `key` is read at the call's entry: write `key <expr>`".into());
            }
            rest = r.trim_end();
            when = w;
            break;
        }
    }
    let mut name = None;
    if let Some((e, n)) = rest.rsplit_once(" as ") {
        let n = n.trim();
        let ok = !n.is_empty()
            && n.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if !ok {
            return Err(format!(
                "`as {n}`: a value's name is letters, digits, `_`, `.` and `-`"
            ));
        }
        if key {
            return Err("a `key` has no name of its own: write `key <expr>`".into());
        }
        name = Some(n.to_string());
        rest = e.trim_end();
    }
    if rest.is_empty() {
        return Err(format!(
            "`{}` names no value: write `{} <expr>`",
            t.trim(),
            if key { "key" } else { "record" }
        ));
    }
    let ast = parse_expr(rest)?;
    if has_old(&ast) {
        return Err(
            "a recorded value is read at the entry, at the return, or at both (`at entry`, \
             `at return`); `old()` is for `ensures`"
                .into(),
        );
    }
    if when == When::Entry && mentions(&ast, "result") {
        return Err("`result` has no value at entry: record it `at return`".into());
    }
    if when == When::Both && mentions(&ast, "result") {
        when = When::Return;
    }
    Ok(SrcRecord {
        name: name.unwrap_or_else(|| rest.to_string()),
        ast,
        when,
        key,
        text: t.trim().to_string(),
        line,
    })
}

fn parse_line(t: &str, line: u32) -> Result<Line, LangError> {
    let err = |m: String| LangError { line, message: m };
    let (kw, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
    let rest = rest.trim();
    match kw {
        "requires" | "ensures" => {
            let kind = if kw == "requires" {
                ClauseKind::Requires
            } else {
                ClauseKind::Ensures
            };
            let ast = parse_expr(rest).map_err(err)?;
            if kind == ClauseKind::Requires && (mentions(&ast, "result") || has_old(&ast)) {
                return Err(err(
                    "a `requires` clause is judged at entry: it cannot use `result` or `old()`"
                        .into(),
                ));
            }
            Ok(Line::Clause(SrcClause {
                kind,
                text: rest.to_string(),
                line,
                ast,
            }))
        }
        "input" => parse_input(rest, line).map(Line::Input).map_err(err),
        "with" => {
            let mut out = Vec::new();
            // commas inside a type (`with T = (usize, u64)`) do not split it (Milestone 307)
            for part in crate::fuzz::rust::split_top(rest, ',') {
                let (n, t) = part.split_once('=').ok_or_else(|| {
                    err(format!(
                        "`with {rest}`: write `with <type parameter> = <type>`, for example \
                         `with T = i64`"
                    ))
                })?;
                let (n, t) = (n.trim(), t.trim());
                if n.is_empty()
                    || t.is_empty()
                    || !n.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    return Err(err(format!(
                        "`with {rest}`: write `with <type parameter> = <type>`"
                    )));
                }
                out.push((n.to_string(), t.to_string()));
            }
            Ok(Line::With(out))
        }
        "record" | "key" => parse_record(rest, line, kw == "key")
            .map(Line::Record)
            .map_err(err),
        "keep" => parse_keep(rest, line).map(Line::Keep).map_err(err),
        "build" => parse_build(rest, line).map(Line::Build).map_err(err),
        "read" => parse_read(rest, line).map(Line::Read).map_err(err),
        "durable" => {
            let ok = !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == ':');
            if !ok {
                return Err(err(format!(
                    "`durable {rest}`: write `durable <function>`, the property function that \
                     checks a call after a power cut"
                )));
            }
            Ok(Line::Durable(rest.to_string(), line))
        }
        _ => Err(err(format!(
            "`// hydo: {kw}`: a contract line is `requires <expr>`, `ensures <expr>`, \
             `input <path> ...`, `with <type parameter> = <type>`, `record <expr> [as <name>] \
             [at entry|at return]`, `key <expr>`, `keep <name> ...`, `durable <function>`, `build <Type> from \
             <constructor>(...)` or `read <Type> as <type> by <method>()`"
        ))),
    }
}

/// The words, numbers, ranges and quoted strings of an `input` line.
#[derive(Clone, Debug, PartialEq)]
enum ITok {
    Word(String),
    Quoted(String),
    /// A byte string, `b"..."`.
    Bytes(Vec<u8>),
    Comma,
}

fn input_tokens(t: &str) -> Result<Vec<ITok>, String> {
    let mut out = Vec::new();
    let mut it = t.chars().peekable();
    while let Some(&c) = it.peek() {
        if c.is_whitespace() {
            it.next();
        } else if c == ',' {
            it.next();
            out.push(ITok::Comma);
        } else if c == '"' {
            it.next();
            let b = quoted(&mut it, false)?;
            let s = String::from_utf8(b)
                .map_err(|_| "a `\\x` escape past `\\x7f` in a string: write it in `b\"...\"`")?;
            out.push(ITok::Quoted(s));
        } else if c == 'b' && {
            let mut la = it.clone();
            la.next();
            la.peek() == Some(&'"')
        } {
            // a byte string, `b"GET / HTTP/1.1\r\n"` (Milestone 289)
            it.next();
            it.next();
            out.push(ITok::Bytes(quoted(&mut it, true)?));
        } else {
            let mut w = String::new();
            while let Some(&c) = it.peek() {
                if c.is_whitespace() || c == ',' || c == '"' {
                    break;
                }
                w.push(c);
                it.next();
            }
            out.push(ITok::Word(w));
        }
    }
    Ok(out)
}

/// The rest of a quoted text after its opening `"`, as bytes: Rust's escapes `\n`, `\t`, `\r`,
/// `\0`, `\\`, `\"`, `\'`, `\xNN` and, outside a byte string, `\u{...}`.
fn quoted(it: &mut std::iter::Peekable<std::str::Chars>, bytes: bool) -> Result<Vec<u8>, String> {
    let mut s: Vec<u8> = Vec::new();
    let push = |s: &mut Vec<u8>, c: char| {
        let mut b = [0u8; 4];
        s.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
    };
    loop {
        match it.next() {
            None => return Err("a string without its closing `\"`".into()),
            Some('"') => break,
            Some('\\') => match it.next() {
                Some('n') => s.push(b'\n'),
                Some('t') => s.push(b'\t'),
                Some('r') => s.push(b'\r'),
                Some('0') => s.push(0),
                Some('x') => {
                    let h: String = it.by_ref().take(2).collect();
                    let v = u8::from_str_radix(&h, 16)
                        .map_err(|_| format!("a bad `\\x{h}` escape: write two hex digits"))?;
                    s.push(v);
                }
                Some('u') if !bytes => {
                    let mut h = String::new();
                    for c in it.by_ref() {
                        if c == '}' {
                            break;
                        }
                        if c != '{' {
                            h.push(c);
                        }
                    }
                    let c = u32::from_str_radix(&h, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or("a bad `\\u{...}` escape")?;
                    push(&mut s, c);
                }
                Some(c) => push(&mut s, c),
                None => return Err("a string without its closing `\"`".into()),
            },
            Some(c) => push(&mut s, c),
        }
    }
    Ok(s)
}

/// The characters of a set that `chars` names: a name of [`CHAR_SETS`], or a quoted set in
/// which `a-z` is a range (a `-` first or last is itself).
pub fn char_set(name: &str, quoted: bool) -> Result<Vec<char>, String> {
    let range = |a: char, b: char| (a..=b).collect::<Vec<char>>();
    if quoted {
        let c: Vec<char> = name.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < c.len() {
            if i + 2 < c.len() && c[i + 1] == '-' {
                if c[i] > c[i + 2] {
                    return Err(format!("the range `{}-{}` is empty", c[i], c[i + 2]));
                }
                out.extend(range(c[i], c[i + 2]));
                i += 3;
            } else {
                out.push(c[i]);
                i += 1;
            }
        }
        out.sort();
        out.dedup();
        if out.is_empty() {
            return Err("the set is empty".into());
        }
        return Ok(out);
    }
    Ok(match name {
        "ascii" => range(' ', '~'),
        "alnum" => [range('0', '9'), range('A', 'Z'), range('a', 'z')].concat(),
        "alpha" => [range('A', 'Z'), range('a', 'z')].concat(),
        "digits" | "digit" => range('0', '9'),
        "hex" => [range('0', '9'), range('A', 'F'), range('a', 'f')].concat(),
        "lower" => range('a', 'z'),
        "upper" => range('A', 'Z'),
        "any" => DEFAULT_CHARS.to_vec(),
        o => {
            let names: Vec<&str> = CHAR_SETS.iter().map(|x| x.0).collect();
            return Err(format!(
                "`{o}` is no character set: write one of {} or a quoted set such as \"0-9.+-\"",
                names.join(", ")
            ));
        }
    })
}

/// The characters of `chars any`, the default of a string without a `chars` part.
pub const DEFAULT_CHARS: &[char] = &[
    'a', 'b', 'c', 'x', 'y', 'z', '0', '1', '9', ' ', '-', '_', '.', '/', 'A', 'Z', '\n', '\0',
    'é', '€', '😀',
];

fn parse_input(t: &str, line: u32) -> Result<SrcInput, String> {
    let usage = || {
        format!(
            "`input {t}`: write `input <path> in <lo>..<hi>` for an integer, `input <path> len \
             <lo>..<hi>` for a length, `input <path> chars <set>` for a string's characters, or \
             `input <path> like \"<text>\", ...` for strings like examples, `b\"<bytes>\"` for \
             bytes (`..=` includes <hi>)"
        )
    };
    let toks = input_tokens(t).map_err(|e| format!("`input {t}`: {e}"))?;
    let mut it = toks.into_iter().peekable();
    let Some(ITok::Word(first)) = it.next() else {
        return Err(usage());
    };
    // `len(v) in a..b` is `v len a..b`
    let (name, mut len_from_in) = match first.strip_prefix("len(").and_then(|r| r.strip_suffix(')'))
    {
        Some(p) => (p.to_string(), true),
        None => (first.clone(), false),
    };
    // a parameter, a field path under one (`l.balance`, Milestone 275), or the elements of a
    // list (`v[]`, Milestone 282)
    let segs_ok = name.split('.').all(|s| {
        let s = s.trim_end_matches("[]");
        !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_')
    });
    if !segs_ok {
        return Err(usage());
    }
    let mut x = SrcInput {
        name,
        range: None,
        len: None,
        chars: None,
        like: Vec::new(),
        like_bytes: Vec::new(),
        text: t.to_string(),
        line,
    };
    let range_of = |w: &str| -> Result<(i128, i128), String> {
        let (lo, hi, inclusive) = match w.split_once("..=") {
            Some((a, b)) => (a, b, true),
            None => match w.split_once("..") {
                Some((a, b)) => (a, b, false),
                None => {
                    let n = parse_int(w.trim()).ok_or_else(usage)?;
                    return Ok((n, n));
                }
            },
        };
        let (lo, hi) = (
            parse_int(lo.trim()).ok_or_else(usage)?,
            parse_int(hi.trim()).ok_or_else(usage)?,
        );
        let hi = if inclusive { hi } else { hi - 1 };
        if lo > hi {
            return Err(format!("`input {t}`: the range is empty"));
        }
        Ok((lo, hi))
    };
    let mut any = false;
    while let Some(tok) = it.next() {
        let ITok::Word(kw) = tok else {
            return Err(usage());
        };
        any = true;
        let mut word = || match it.next() {
            Some(ITok::Word(w)) => Ok(w),
            _ => Err(usage()),
        };
        match kw.as_str() {
            "in" if len_from_in => {
                x.len = Some(range_of(&word()?)?);
                len_from_in = false;
            }
            "in" => {
                if x.range.is_some() {
                    return Err(format!("`input {t}`: two `in` parts"));
                }
                x.range = Some(range_of(&word()?)?);
            }
            "len" => {
                if x.len.is_some() {
                    return Err(format!("`input {t}`: two `len` parts"));
                }
                x.len = Some(range_of(&word()?)?);
                if x.len.is_some_and(|(lo, _)| lo < 0) {
                    return Err(format!("`input {t}`: a length is not negative"));
                }
            }
            "chars" => {
                let set = match it.next() {
                    Some(ITok::Word(w)) => char_set(&w, false),
                    Some(ITok::Quoted(q)) => char_set(&q, true),
                    _ => return Err(usage()),
                };
                x.chars = Some(set.map_err(|e| format!("`input {t}`: {e}"))?);
            }
            "like" => loop {
                match it.next() {
                    Some(ITok::Quoted(q)) => x.like.push(q),
                    Some(ITok::Bytes(b)) => x.like_bytes.push(b),
                    _ => return Err(usage()),
                }
                if it.peek() == Some(&ITok::Comma) {
                    it.next();
                } else {
                    break;
                }
            },
            _ => return Err(usage()),
        }
    }
    if !any || len_from_in {
        return Err(usage());
    }
    Ok(x)
}

fn parse_int(s: &str) -> Option<i128> {
    let s = s.replace('_', "");
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r.to_string()),
        None => (false, s),
    };
    let v = match s.strip_prefix("0x") {
        Some(h) => i128::from_str_radix(h, 16).ok()?,
        None => s.parse::<i128>().ok()?,
    };
    Some(if neg { -v } else { v })
}

/// Whether an expression names `name` as a root.
pub fn mentions(a: &Ast, name: &str) -> bool {
    match a {
        Ast::Name(r) => r == name,
        Ast::Field(x, _) | Ast::Call(_, x) => mentions(x, name),
        Ast::Old(x) | Ast::Not(x) | Ast::Neg(x) => mentions(x, name),
        Ast::Bin(_, x, y) | Ast::Index(x, y) => mentions(x, name) || mentions(y, name),
        Ast::Matches(x, _) => mentions(x, name),
        Ast::Int(_) | Ast::Float(_) | Ast::Bool(_) | Ast::Str(_) => false,
    }
}

fn has_old(a: &Ast) -> bool {
    match a {
        Ast::Old(_) => true,
        Ast::Not(x) | Ast::Neg(x) | Ast::Field(x, _) | Ast::Call(_, x) | Ast::Matches(x, _) => {
            has_old(x)
        }
        Ast::Bin(_, x, y) | Ast::Index(x, y) => has_old(x) || has_old(y),
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Int(i128),
    Float(f64),
    Ident(String),
    Str(String),
    Op(&'static str),
}

const OPS: [&str; 21] = [
    "::", "||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "(", ")",
    ".", ",", "[", "]",
];

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            // a floating-point literal (`50.5`; Milestone 285), but not a tuple field after a
            // field (`t.0.1`)
            let after_dot = out.last() == Some(&Tok::Op("."));
            if !after_dot
                && i + 1 < b.len()
                && b[i] == b'.'
                && b[i + 1].is_ascii_digit()
                && s[st..i].bytes().all(|c| c.is_ascii_digit() || c == b'_')
            {
                i += 1;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                    i += 1;
                }
                let t = s[st..i].replace('_', "");
                out.push(Tok::Float(
                    t.parse()
                        .map_err(|_| format!("`{t}` is not a floating-point number"))?,
                ));
                continue;
            }
            let t = &s[st..i];
            out.push(Tok::Int(
                parse_int(t).ok_or_else(|| format!("`{t}` is not an integer"))?,
            ));
        } else if c.is_alphabetic() || c == '_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Ident(s[st..i].to_string()));
        } else if c == '"' {
            // a string literal: `\"`, `\\` and `\n` are its escapes
            let mut lit = String::new();
            let mut it = s[i + 1..].char_indices();
            let end = loop {
                match it.next() {
                    None => return Err(format!("a string without its closing `\"` in `{s}`")),
                    Some((k, '"')) => break i + 1 + k + 1,
                    Some((_, '\\')) => match it.next() {
                        Some((_, '"')) => lit.push('"'),
                        Some((_, '\\')) => lit.push('\\'),
                        Some((_, 'n')) => lit.push('\n'),
                        _ => {
                            return Err(format!("`{s}`: a string's escapes are \\\", \\\\ and \\n"))
                        }
                    },
                    Some((_, ch)) => lit.push(ch),
                }
            };
            out.push(Tok::Str(lit));
            i = end;
        } else if let Some(op) = OPS.iter().find(|o| s[i..].starts_with(**o)) {
            out.push(Tok::Op(op));
            i += op.len();
        } else {
            return Err(format!("unexpected `{c}` in `{s}`"));
        }
    }
    Ok(out)
}

/// Parse one expression.
pub fn parse_expr(s: &str) -> Result<Ast, String> {
    let toks = lex(s)?;
    if toks.is_empty() {
        return Err("an empty clause".into());
    }
    let mut p = Parser {
        t: toks,
        i: 0,
        src: s,
    };
    let e = p.expr(0)?;
    if p.i != p.t.len() {
        return Err(format!("`{s}`: unexpected `{}`", p.show(p.i)));
    }
    Ok(e)
}

struct Parser<'a> {
    t: Vec<Tok>,
    i: usize,
    src: &'a str,
}

fn binop(op: &str) -> Option<(BinOp, u8)> {
    Some(match op {
        "||" => (BinOp::Or, 1),
        "&&" => (BinOp::And, 2),
        "==" => (BinOp::Eq, 3),
        "!=" => (BinOp::Ne, 3),
        "<" => (BinOp::Lt, 3),
        "<=" => (BinOp::Le, 3),
        ">" => (BinOp::Gt, 3),
        ">=" => (BinOp::Ge, 3),
        "+" => (BinOp::Add, 4),
        "-" => (BinOp::Sub, 4),
        "*" => (BinOp::Mul, 5),
        "/" => (BinOp::Div, 5),
        "%" => (BinOp::Rem, 5),
        _ => return None,
    })
}

impl Parser<'_> {
    fn show(&self, i: usize) -> String {
        match self.t.get(i) {
            Some(Tok::Int(v)) => v.to_string(),
            Some(Tok::Float(v)) => format!("{v:?}"),
            Some(Tok::Ident(n)) => n.clone(),
            Some(Tok::Str(s)) => format!("{s:?}"),
            Some(Tok::Op(o)) => o.to_string(),
            None => "end".into(),
        }
    }
    fn peek_op(&self) -> Option<&'static str> {
        match self.t.get(self.i) {
            Some(Tok::Op(o)) => Some(o),
            _ => None,
        }
    }
    fn expect(&mut self, op: &str) -> Result<(), String> {
        if self.peek_op() == Some(op) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!(
                "`{}`: expected `{op}`, found `{}`",
                self.src,
                self.show(self.i)
            ))
        }
    }
    fn expr(&mut self, min: u8) -> Result<Ast, String> {
        let mut lhs = self.unary()?;
        while let Some((op, prec)) = self.peek_op().and_then(binop) {
            if prec < min || prec == 0 {
                break;
            }
            self.i += 1;
            // comparisons do not chain: `a < b < c` is refused
            let next = if prec == 3 { 4 } else { prec + 1 };
            let rhs = self.expr(next)?;
            if prec == 3 {
                if let Some((_, 3)) = self.peek_op().and_then(binop) {
                    return Err(format!(
                        "`{}`: comparisons do not chain; write `a < b && b < c`",
                        self.src
                    ));
                }
            }
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }
    fn unary(&mut self) -> Result<Ast, String> {
        match self.peek_op() {
            Some("!") => {
                self.i += 1;
                Ok(Ast::Not(Box::new(self.unary()?)))
            }
            Some("-") => {
                self.i += 1;
                Ok(Ast::Neg(Box::new(self.unary()?)))
            }
            _ => self.postfix(),
        }
    }
    fn postfix(&mut self) -> Result<Ast, String> {
        let mut e = self.primary()?;
        loop {
            match self.peek_op() {
                Some(".") => {
                    self.i += 1;
                    let name = match self.t.get(self.i) {
                        Some(Tok::Ident(n)) => n.clone(),
                        Some(Tok::Int(v)) => v.to_string(),
                        _ => {
                            return Err(format!(
                                "`{}`: a field name after `.`, found `{}`",
                                self.src,
                                self.show(self.i)
                            ))
                        }
                    };
                    self.i += 1;
                    if self.peek_op() == Some("(") {
                        if let Some(b) = Builtin::of(&name) {
                            return Err(format!(
                                "`{}`: write `{n}({e})` for `{e}.{n}()`: contracts call their \
                                 functions with the value as the argument",
                                self.src,
                                n = b.name()
                            ));
                        }
                        return Err(no_calls(self.src, &name));
                    }
                    if e.root().is_none() {
                        return Err(format!(
                            "`{}`: a field is read from a name (`l.balance`, `some(o).x`), not \
                             from an expression",
                            self.src
                        ));
                    }
                    e = Ast::Field(Box::new(e), name);
                }
                Some("[") => {
                    self.i += 1;
                    let ix = self.expr(0)?;
                    self.expect("]")?;
                    if e.root().is_none() {
                        return Err(format!(
                            "`{}`: an element is read from a name (`v[0]`), not from an \
                             expression",
                            self.src
                        ));
                    }
                    e = Ast::Index(Box::new(e), Box::new(ix));
                }
                _ => return Ok(e),
            }
        }
    }
    fn primary(&mut self) -> Result<Ast, String> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        match t {
            Some(Tok::Int(v)) => Ok(Ast::Int(v)),
            Some(Tok::Float(v)) => Ok(Ast::Float(v)),
            Some(Tok::Str(s)) => Ok(Ast::Str(s)),
            Some(Tok::Ident(n)) if n == "true" => Ok(Ast::Bool(true)),
            Some(Tok::Ident(n)) if n == "false" => Ok(Ast::Bool(false)),
            // Go's nil: a pointer, or an interface whose type word is 0
            Some(Tok::Ident(n)) if n == "nil" => Ok(Ast::Int(0)),
            Some(Tok::Ident(n)) if n == "old" && self.peek_op() == Some("(") => {
                self.i += 1;
                let e = self.expr(0)?;
                self.expect(")")?;
                if has_old(&e) {
                    return Err(format!("`{}`: `old()` inside `old()`", self.src));
                }
                if mentions(&e, "result") {
                    return Err(format!(
                        "`{}`: `result` has no value at entry, so not inside `old()`",
                        self.src
                    ));
                }
                Ok(Ast::Old(Box::new(e)))
            }
            Some(Tok::Ident(n)) if n == "matches" && self.peek_op() == Some("(") => {
                self.i += 1;
                let e = self.expr(0)?;
                if self.peek_op() != Some(",") {
                    return Err(format!(
                        "`{}`: write `matches(<value>, <pattern>)`, for example \
                         `matches(result, Ok(Complete(_)))`",
                        self.src
                    ));
                }
                self.i += 1;
                let p = self.pattern()?;
                self.expect(")")?;
                if e.root().is_none() && !matches!(e, Ast::Old(_)) {
                    return Err(format!(
                        "`{}`: `matches()` reads a value named by a parameter or `result`, not \
                         an expression",
                        self.src
                    ));
                }
                Ok(Ast::Matches(Box::new(e), p))
            }
            Some(Tok::Ident(n)) => {
                if self.peek_op() == Some("(") {
                    let Some(b) = Builtin::of(&n) else {
                        return Err(no_calls(self.src, &n));
                    };
                    self.i += 1;
                    let e = self.expr(0)?;
                    self.expect(")")?;
                    if e.root().is_none() && !matches!(e, Ast::Old(_)) {
                        return Err(format!(
                            "`{}`: `{}()` reads a value named by a parameter or `result`, not an \
                             expression",
                            self.src,
                            b.name()
                        ));
                    }
                    // `len(old(v))` is `old(len(v))`
                    if let Ast::Old(x) = e {
                        return Ok(Ast::Old(Box::new(Ast::Call(b, x))));
                    }
                    return Ok(Ast::Call(b, Box::new(e)));
                }
                Ok(Ast::Name(n))
            }
            Some(Tok::Op("(")) => {
                let e = self.expr(0)?;
                self.expect(")")?;
                Ok(e)
            }
            _ => Err(format!(
                "`{}`: unexpected `{}`",
                self.src,
                self.show(self.i - 1)
            )),
        }
    }
}

impl Parser<'_> {
    /// A pattern of `matches()`: `_`, an integer (also negative), a string, `true`, `false`, a
    /// variant's path with its fields' patterns in parentheses, or a tuple of patterns.
    fn pattern(&mut self) -> Result<Pat, String> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        match t {
            Some(Tok::Int(v)) => Ok(Pat::Int(v)),
            Some(Tok::Op("-")) => match self.t.get(self.i).cloned() {
                Some(Tok::Int(v)) => {
                    self.i += 1;
                    Ok(Pat::Int(-v))
                }
                _ => Err(format!("`{}`: `-` in a pattern takes an integer", self.src)),
            },
            Some(Tok::Str(s)) => Ok(Pat::Str(s)),
            Some(Tok::Ident(n)) if n == "_" => Ok(Pat::Any),
            Some(Tok::Ident(n)) if n == "true" || n == "false" => Ok(Pat::Bool(n == "true")),
            Some(Tok::Ident(mut n)) => {
                // `Status::Complete`: the variant is the last segment
                while self.peek_op() == Some("::") {
                    self.i += 1;
                    match self.t.get(self.i).cloned() {
                        Some(Tok::Ident(x)) => {
                            n = x;
                            self.i += 1;
                        }
                        _ => return Err(format!("`{}`: a name after `::` in a pattern", self.src)),
                    }
                }
                if self.peek_op() != Some("(") {
                    return Ok(Pat::Variant(n, None));
                }
                self.i += 1;
                Ok(Pat::Variant(n, Some(self.patterns()?)))
            }
            Some(Tok::Op("(")) => {
                let v = self.patterns()?;
                Ok(Pat::Tuple(v))
            }
            _ => Err(format!(
                "`{}`: a pattern is `_`, a literal, a variant such as `Some(_)` or \
                 `Status::Complete(_)`, or a tuple such as `(_, 0)`; found `{}`",
                self.src,
                self.show(self.i - 1)
            )),
        }
    }

    /// The patterns up to the closing `)`, separated by commas.
    fn patterns(&mut self) -> Result<Vec<Pat>, String> {
        let mut v = Vec::new();
        if self.peek_op() == Some(")") {
            self.i += 1;
            return Ok(v);
        }
        loop {
            v.push(self.pattern()?);
            match self.peek_op() {
                Some(",") => {
                    self.i += 1;
                    if self.peek_op() == Some(")") {
                        self.i += 1;
                        return Ok(v);
                    }
                }
                Some(")") => {
                    self.i += 1;
                    return Ok(v);
                }
                _ => return Err(format!("`{}`: expected `,` or `)` in a pattern", self.src)),
            }
        }
    }
}

fn no_calls(src: &str, name: &str) -> String {
    format!(
        "`{src}`: `{name}(...)` is a call of the program's code, which a check never runs: a \
         check reads values. Read the field the function would return instead. The functions a \
         contract may use are len, is_some, is_none, is_ok, is_err, some, ok, err and old; \
         `hydo fuzz` also reads matches and sorted (docs/design/contracts.md, \"The language\")"
    )
}
