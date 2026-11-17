//! The compiled contracts of an image (`<image>.contracts.json`, written by `hydo build`), and
//! the records an engine writes while a run calls the functions (`<tag>.contracts.jsonl`).
//!
//! An engine reads only what it needs to stop and read: each function's entry address, the
//! first bytes of its code (to tell its program from another program that has code at the same
//! address), how to match a return with its call, and the reads at entry and at return. The
//! clauses are judged on the host after the run ([`crate::eval`]), so the engine never evaluates
//! an expression and never changes the guest.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The plan's version.
pub const PLAN_V: u32 = 1;
/// The plan's `type`.
pub const PLAN_TYPE: &str = "contracts-plan";
/// The file name suffix of a plan next to its image.
pub const PLAN_SUFFIX: &str = ".contracts.json";
/// The file name suffix of a run's records, after the run's tag.
pub const RECORDS_SUFFIX: &str = ".contracts.jsonl";
/// At most this many calls are recorded per run; the rest are counted, and the records' last
/// line ([`End`]) says how many (Milestone 255).
pub const MAX_RECORDS: usize = 100_000;

/// The plan of an image: every program with contracts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub programs: Vec<Program>,
}

/// One program of the image and its contracted functions.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Program {
    /// The program's path in the guest (`/bin/ledger`).
    pub program: String,
    /// The program's sha256 (the file that was compiled; the image holds the same bytes).
    pub sha256: String,
    /// `arm64` or `x86_64`.
    pub arch: String,
    /// A position-independent program (Rust's musl target on x86_64 builds them): the
    /// functions' addresses are offsets from where the guest loads it. An engine finds that
    /// address by the program's first bytes, its ELF header (`head`, in hexadecimal), in the
    /// process's memory (Milestone 255).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pie: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head: String,
    pub functions: Vec<Function>,
    /// Contracts that are not checked, each with the reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unchecked: Vec<Unchecked>,
    /// The direct calls of the functions that record values (Milestone 285): a record's return
    /// address names its caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<CallSite>,
    /// The properties of recorded values that the program's `// hydo: keep` lines declare
    /// (Milestone 291): judged on the host after the run, over the histories of every program.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keeps: Vec<Keep>,
}

/// A property of a recorded value along a key's history (`// hydo: keep <name> [once set]
/// [from <function>] [to <function>] [across boots]`, Milestone 291,
/// docs/design/value-history.md). A run breaks it at a stop where the value is not what the
/// property says, and the break is a finding of kind `values`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Keep {
    /// The line as written after `// hydo:` (`keep overdraft_limit from ... to ...`): the
    /// property's name in a finding and a verdict.
    pub text: String,
    /// The source file of the line, relative to the source folder.
    pub file: String,
    pub line: u32,
    /// The recorded value's name.
    pub name: String,
    /// Once a stop recorded the value, no later stop records it absent (else: every judged stop
    /// records the reference's value).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub once_set: bool,
    /// The function whose last stop before a judged stop is its reference (its name in the
    /// debug information); none: the key's first stop that recorded the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The function whose stops are judged; none: every stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// A reference may be in an earlier boot (a power cut or a reboot between the two stops).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub across_boots: bool,
}

/// A contract that the plan cannot check, with the reason.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Unchecked {
    pub function: String,
    pub file: String,
    pub line: u32,
    pub reason: String,
}

/// The language a function was compiled from (how its return matches its call, and where its
/// result is).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Rust,
    Go,
    C,
}

/// How an engine matches a return with its call: the stack pointer at the call's entry
/// (`sp`: Rust, C), or the goroutine (`g`, Go, whose stacks move: register x28 on arm64, r14
/// on x86_64).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    Sp,
    G,
}

/// One contracted function.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Function {
    /// Unique in the plan: the records name it.
    pub id: u32,
    /// The name in the debug information (`ledger::apply`, `main.apply`).
    pub name: String,
    /// The source file, relative to the contract's source folder.
    pub file: String,
    /// The line of the function's declaration.
    pub line: u32,
    pub lang: Lang,
    /// The address of the first instruction.
    pub entry: u64,
    /// The first bytes of the function's code, in hexadecimal.
    pub code: String,
    pub frame: Frame,
    /// The reads at entry.
    #[serde(rename = "in")]
    pub at_entry: Vec<Loc>,
    /// The reads at return (none: the function's return is not stopped at).
    #[serde(rename = "out")]
    pub at_return: Vec<Loc>,
    pub clauses: Vec<Clause>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Input>,
    /// The values its `// hydo: record` and `// hydo: key` lines record at each call, for the
    /// value history (Milestone 285, docs/design/value-history.md). An engine makes their reads
    /// with the others; only the host reads this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Value>,
    /// The entry read that gives the call's return address, when the function records values:
    /// the host names the caller from [`Program::calls`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ret_addr: Option<u32>,
}

/// One value a function records (`// hydo: record <expr> [as <name>] [at entry|at return]`, or
/// `// hydo: key <expr>`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Value {
    /// The value's name in the history: the `as` name, else the expression as written. Values
    /// of the same name in several functions are one history.
    pub name: String,
    /// The line as written, after `record` or `key`.
    pub text: String,
    pub line: u32,
    /// The key: what the history follows (a transaction's reference), not a value of it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub key: bool,
    /// How to show it at the call's entry, and at its return (none: not recorded there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<Show>,
    #[serde(default, rename = "return", skip_serializing_if = "Option::is_none")]
    pub at_return: Option<Show>,
}

/// How a recorded value is shown from a call's reads (Milestone 285). Every form only reads. A
/// read that needs something (an element inside the length, a variant) carries its guard, so a
/// value that does not exist for a call shows why.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Show {
    /// A number, a boolean or an address: an expression over the reads.
    Val(Expr),
    /// A string: its length, and its first bytes in reads of up to 8 bytes each.
    Text { len: Expr, chunks: Vec<Expr> },
    /// A Go interface's dynamic value: its first word (0: nil), the address of its dynamic
    /// type's descriptor, and for each type hydo reads, the descriptor's address in the program
    /// and how to show the value. A type that is none of them is shown by its address.
    Dyn {
        nil: Expr,
        ty: Expr,
        cases: Vec<DynCase>,
    },
    /// A Go map's value by a constant key, in a map of at most one group (8 entries): the map
    /// pointer (0: a nil map), the directory's length (not 0: a larger map, which the record
    /// does not cover), the group's address (0: an empty map), and each slot.
    Lookup {
        map: Expr,
        dir_len: Expr,
        group: Expr,
        slots: Vec<Slot>,
    },
}

/// One dynamic type of a Go interface value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DynCase {
    /// The type descriptor's address in the program.
    pub addr: u64,
    /// The type's name (`float64`).
    pub name: String,
    pub show: Show,
}

/// One slot of a Go map's group: whether it holds an entry (its control byte is below 0x80),
/// whether its key is the one asked for, and its value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Slot {
    pub full: Expr,
    pub hit: Expr,
    pub val: Show,
}

/// A place in the program that calls a function which records values: the return address of
/// the call, and the function that makes it (Milestone 285).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CallSite {
    /// The function called (its [`Function::id`]).
    pub callee: u32,
    /// The address after the call instruction (the return address).
    pub ret: u64,
    /// The function the call is in.
    pub caller: String,
}

impl Function {
    /// The first bytes of the code as bytes.
    pub fn code_bytes(&self) -> Vec<u8> {
        hex_bytes(&self.code)
    }
}

impl Program {
    /// The ELF header of a position-independent program as bytes.
    pub fn head_bytes(&self) -> Vec<u8> {
        hex_bytes(&self.head)
    }
}

/// Bytes from hexadecimal text.
pub fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok())
        .collect()
}

/// Where a read starts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Base {
    /// A register's value: 0 to 30 are x0 to x30 on arm64 (31 is the stack pointer); on x86_64
    /// the DWARF numbers (0 rax, 1 rdx, 2 rcx, 3 rbx, 4 rsi, 5 rdi, 6 rbp, 7 rsp, 8 to 15 r8 to
    /// r15).
    Reg(u8),
    /// The value of the entry read with this index (at return: a pointer argument as it was at
    /// entry).
    In(u32),
}

impl Default for Base {
    fn default() -> Base {
        Base::Reg(0)
    }
}

/// One read: the base's value, then for each offset of `path` a load from (value + offset);
/// every load but the last reads 8 bytes (a pointer), the last reads `size` bytes. With an
/// empty path the value is the base's own, shifted right by `shift` bits and cut to `size`
/// bytes (a small structure returned in a register has a field in the register's upper bytes).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Loc {
    /// What the read is, as the contract writes it (`amount`, `l.balance`, `result`); for the
    /// report only.
    #[serde(default)]
    pub name: String,
    pub base: Base,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<i64>,
    pub size: u8,
    /// Bits to shift a register's value right by (an empty path only; Milestone 255).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub shift: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

impl Loc {
    /// Make the read: `reg` gives a register's value (the plan's numbering), `entry` the call's
    /// entry reads (for a base [`Base::In`]), and `mem` gives `n` bytes (1 to 8) of guest memory
    /// at a virtual address, little-endian (None: not mapped). Every engine reads with this.
    pub fn load(
        &self,
        reg: &dyn Fn(u8) -> Option<u64>,
        entry: &[Option<u64>],
        mem: &dyn Fn(u64, u8) -> Option<u64>,
    ) -> Option<u64> {
        let mut v = match self.base {
            Base::Reg(n) => reg(n)?,
            Base::In(k) => (*entry.get(k as usize)?)?,
        };
        let n = self.path.len();
        if n == 0 {
            let m = if self.size >= 8 {
                u64::MAX
            } else {
                (1u64 << (8 * self.size)) - 1
            };
            return Some(v.checked_shr(self.shift as u32).unwrap_or(0) & m);
        }
        for (i, off) in self.path.iter().enumerate() {
            let size = if i + 1 == n { self.size.clamp(1, 8) } else { 8 };
            v = mem(v.wrapping_add(*off as u64), size)?;
        }
        Some(v)
    }
}

/// `requires` or `ensures`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClauseKind {
    Requires,
    Ensures,
}

impl ClauseKind {
    pub fn name(self) -> &'static str {
        match self {
            ClauseKind::Requires => "requires",
            ClauseKind::Ensures => "ensures",
        }
    }
}

/// One clause: its text as written, where, and the compiled expression.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Clause {
    pub kind: ClauseKind,
    pub text: String,
    pub line: u32,
    pub expr: Expr,
}

/// An input domain (`// hydo: input amount in 1..=10_000`): kept for active property testing,
/// not used by the checks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Input {
    pub name: String,
    pub text: String,
    pub line: u32,
}

/// The type of a value that a read gives.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Ty {
    /// An unsigned integer of this many bytes.
    U(u8),
    /// A signed integer of this many bytes.
    I(u8),
    Bool,
    /// An address.
    Ptr,
    /// This many bytes of a string, little-endian (shown as text; Milestone 255).
    Bytes(u8),
    /// A floating-point number of this many bytes, 4 or 8, as its IEEE 754 bits (Milestone 285).
    F(u8),
}

/// A compiled expression. Integers are exact (no overflow and no wrapping): `a - b` of two
/// unsigned values may be negative.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Expr {
    /// A literal integer, in decimal.
    Int(String),
    /// A literal floating-point number, as written (Milestone 285).
    Float(String),
    Bool(bool),
    /// An entry read with its index and type.
    In(u32, Ty),
    /// A return read with its index and type.
    Out(u32, Ty),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    /// `then` when `cond` holds; otherwise the clause is undecided for the call, with `why`
    /// (the value of `some(o)` when `o` is `None`, an element past a sequence's length).
    Guard {
        cond: Box<Expr>,
        then: Box<Expr>,
        why: String,
    },
    /// An element chosen by an index the call computes (`v[k]`): the reads of the elements
    /// `first`, `first + 1`, ... (a sequence is read up to a bound). Another index leaves the
    /// clause undecided for the call.
    Elem {
        idx: Box<Expr>,
        first: u64,
        elems: Vec<Expr>,
    },
    /// Whether a string equals a literal: its length, then its bytes in reads of up to 8
    /// bytes (little-endian), which are compared only when the length is the literal's.
    StrEq {
        len: Box<Expr>,
        chunks: Vec<Expr>,
        lit: String,
    },
    /// Whether an enumeration's discriminant names a variant: one of `any` (in decimal); or,
    /// when `any` is empty (the variant without a value of its own, where a niche keeps the
    /// others), none of `not`.
    Is {
        discr: Box<Expr>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        any: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        not: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// One call as an engine recorded it: the function, the guest time (the engine's clock) at
/// entry and at return, and the reads. A read that the guest's page tables could not resolve is
/// `null`. `out` is missing when the call had not returned when the run ended, or when the
/// engine had no breakpoint left for its return.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub f: u32,
    pub at: u64,
    #[serde(rename = "in")]
    pub at_entry: Vec<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ret: Option<u64>,
    #[serde(default, rename = "out", skip_serializing_if = "Option::is_none")]
    pub at_return: Option<Vec<Option<u64>>>,
    /// Why `out` is missing, when it is not the run's end (`no breakpoint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost: Option<String>,
    /// The boot the call was in: 0 is the run's first, and each power cut or reboot of the guest
    /// starts the next (Milestone 285: the value history places the call in its boot).
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub boot: u32,
}

fn is_zero32(v: &u32) -> bool {
    *v == 0
}

/// The plan next to an image.
pub fn plan_path(image: &Path) -> PathBuf {
    let mut s = image.as_os_str().to_os_string();
    s.push(PLAN_SUFFIX);
    PathBuf::from(s)
}

/// Read a plan.
pub fn read_plan(p: &Path) -> Result<Plan, String> {
    let t = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let plan: Plan = serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))?;
    if plan.v != PLAN_V || plan.kind != PLAN_TYPE {
        return Err(format!(
            "{}: not a contracts plan of version {PLAN_V}",
            p.display()
        ));
    }
    Ok(plan)
}

/// The plan next to an image, when there is one.
pub fn plan_of_image(image: &Path) -> Option<Result<Plan, String>> {
    let p = plan_path(image);
    p.exists().then(|| read_plan(&p))
}

/// Write a plan (pretty JSON with a final newline).
pub fn plan_text(plan: &Plan) -> String {
    let mut s = serde_json::to_string_pretty(plan).expect("a plan serializes");
    s.push('\n');
    s
}

/// The last line of a run's records (Milestone 255): the calls recorded, and the calls the
/// engine stopped at but did not record, because the run had recorded [`MAX_RECORDS`] calls.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct End {
    pub recorded: u64,
    pub not_recorded: u64,
}

#[derive(Serialize, Deserialize)]
struct EndLine {
    end: End,
}

/// The records of a run, one JSON object per line, and then the line [`End`] with the calls
/// that were not recorded.
pub fn records_text(recs: &[Record], not_recorded: u64) -> String {
    let mut s = String::new();
    for r in recs {
        s.push_str(&serde_json::to_string(r).expect("a record serializes"));
        s.push('\n');
    }
    let end = EndLine {
        end: End {
            recorded: recs.len() as u64,
            not_recorded,
        },
    };
    s.push_str(&serde_json::to_string(&end).expect("an end line serializes"));
    s.push('\n');
    s
}

/// Read a run's records and their end line (a missing file: none). Records written before
/// Milestone 255 have no end line: every call was recorded or the count was lost.
pub fn read_run(p: &Path) -> Result<(Vec<Record>, End), String> {
    let Ok(t) = std::fs::read_to_string(p) else {
        return Ok((Vec::new(), End::default()));
    };
    let mut recs = Vec::new();
    let mut end = None;
    for l in t.lines().filter(|l| !l.trim().is_empty()) {
        let bad = |e: serde_json::Error| format!("{}: {e}: {l}", p.display());
        if l.starts_with("{\"end\"") {
            end = Some(serde_json::from_str::<EndLine>(l).map_err(bad)?.end);
        } else {
            recs.push(serde_json::from_str(l).map_err(bad)?);
        }
    }
    let end = end.unwrap_or(End {
        recorded: recs.len() as u64,
        not_recorded: 0,
    });
    Ok((recs, end))
}

/// Read a run's records (a missing file: none).
pub fn read_records(p: &Path) -> Result<Vec<Record>, String> {
    read_run(p).map(|r| r.0)
}
