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
/// At most this many calls are recorded per run; the rest are counted.
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
    pub functions: Vec<Function>,
    /// Contracts that are not checked, each with the reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unchecked: Vec<Unchecked>,
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
}

impl Function {
    /// The first bytes of the code as bytes.
    pub fn code_bytes(&self) -> Vec<u8> {
        (0..self.code.len() / 2)
            .filter_map(|i| u8::from_str_radix(&self.code[2 * i..2 * i + 2], 16).ok())
            .collect()
    }
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

/// One read: the base's value, then for each offset of `path` a load from (value + offset);
/// every load but the last reads 8 bytes (a pointer), the last reads `size` bytes. With an
/// empty path the value is the base's own, cut to `size` bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Loc {
    /// What the read is, as the contract writes it (`amount`, `l.balance`, `result`); for the
    /// report only.
    #[serde(default)]
    pub name: String,
    pub base: Base,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<i64>,
    pub size: u8,
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
}

/// A compiled expression. Integers are exact (no overflow and no wrapping): `a - b` of two
/// unsigned values may be negative.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Expr {
    /// A literal integer, in decimal.
    Int(String),
    Bool(bool),
    /// An entry read with its index and type.
    In(u32, Ty),
    /// A return read with its index and type.
    Out(u32, Ty),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
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

/// The records of a run, one JSON object per line.
pub fn records_text(recs: &[Record]) -> String {
    let mut s = String::new();
    for r in recs {
        s.push_str(&serde_json::to_string(r).expect("a record serializes"));
        s.push('\n');
    }
    s
}

/// Read a run's records (a missing file: none).
pub fn read_records(p: &Path) -> Result<Vec<Record>, String> {
    let Ok(t) = std::fs::read_to_string(p) else {
        return Ok(Vec::new());
    };
    t.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}: {l}", p.display())))
        .collect()
}
