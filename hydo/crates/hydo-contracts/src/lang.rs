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
//! `-`. Integers are exact: no overflow, no wrapping. There are no calls in the first version:
//! a call would run the program's code, which a check must not do (docs/design/contracts.md).

use crate::plan::{BinOp, ClauseKind};
use std::fmt;

/// An expression as written, before its names are bound to reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Ast {
    Int(i128),
    Bool(bool),
    /// A name and the fields read through it: `l.balance` is `Path("l", ["balance"])`.
    Path(String, Vec<String>),
    Old(Box<Ast>),
    Not(Box<Ast>),
    Neg(Box<Ast>),
    Bin(BinOp, Box<Ast>, Box<Ast>),
}

/// A clause as written.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcClause {
    pub kind: ClauseKind,
    pub text: String,
    pub line: u32,
    pub ast: Ast,
}

/// An input domain as written: `input <name> in <lo>..<hi>` or `..=`.
#[derive(Clone, Debug, PartialEq)]
pub struct SrcInput {
    pub name: String,
    pub lo: i128,
    pub hi: i128,
    pub inclusive: bool,
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
    line.trim_start().strip_prefix("// hydo:").map(str::trim)
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
        while i < lines.len() {
            let l = lines[i].trim_start();
            if let Some(t) = contract_text(l) {
                match parse_line(t, i as u32 + 1) {
                    Ok(Line::Clause(c)) => clauses.push(c),
                    Ok(Line::Input(x)) => inputs.push(x),
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
        _ => Err(err(format!(
            "`// hydo: {kw}`: a contract line is `requires <expr>`, `ensures <expr>` or \
             `input <name> in <lo>..<hi>`"
        ))),
    }
}

fn parse_input(t: &str, line: u32) -> Result<SrcInput, String> {
    let usage = || format!("`input {t}`: write `input <name> in <lo>..<hi>` (or `..=`)");
    let (name, range) = t.split_once(" in ").ok_or_else(usage)?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(usage());
    }
    let (lo, hi, inclusive) = match range.split_once("..=") {
        Some((a, b)) => (a, b, true),
        None => {
            let (a, b) = range.split_once("..").ok_or_else(usage)?;
            (a, b, false)
        }
    };
    let num = |s: &str| parse_int(s.trim()).ok_or_else(usage);
    let (lo, hi) = (num(lo)?, num(hi)?);
    if lo > hi || (!inclusive && lo == hi) {
        return Err(format!("`input {t}`: the range is empty"));
    }
    Ok(SrcInput {
        name: name.to_string(),
        lo,
        hi,
        inclusive,
        text: t.to_string(),
        line,
    })
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
        Ast::Path(r, _) => r == name,
        Ast::Old(x) | Ast::Not(x) | Ast::Neg(x) => mentions(x, name),
        Ast::Bin(_, x, y) => mentions(x, name) || mentions(y, name),
        Ast::Int(_) | Ast::Bool(_) => false,
    }
}

fn has_old(a: &Ast) -> bool {
    match a {
        Ast::Old(_) => true,
        Ast::Not(x) | Ast::Neg(x) => has_old(x),
        Ast::Bin(_, x, y) => has_old(x) || has_old(y),
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Int(i128),
    Ident(String),
    Op(&'static str),
}

const OPS: [&str; 19] = [
    "||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "(", ")", ".", ",",
    "[",
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
            Some(Tok::Ident(n)) => n.clone(),
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
        while self.peek_op() == Some(".") {
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
                return Err(no_calls(self.src, &name));
            }
            e = match e {
                Ast::Path(r, mut f) => {
                    f.push(name);
                    Ast::Path(r, f)
                }
                _ => {
                    return Err(format!(
                        "`{}`: a field is read from a name (`l.balance`, `old(l.balance)`), \
                         not from an expression",
                        self.src
                    ))
                }
            };
        }
        if self.peek_op() == Some("[") {
            return Err(format!(
                "`{}`: indexing is not in the first version (docs/design/contracts.md)",
                self.src
            ));
        }
        Ok(e)
    }
    fn primary(&mut self) -> Result<Ast, String> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        match t {
            Some(Tok::Int(v)) => Ok(Ast::Int(v)),
            Some(Tok::Ident(n)) if n == "true" => Ok(Ast::Bool(true)),
            Some(Tok::Ident(n)) if n == "false" => Ok(Ast::Bool(false)),
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
            Some(Tok::Ident(n)) => {
                if self.peek_op() == Some("(") {
                    return Err(no_calls(self.src, &n));
                }
                Ok(Ast::Path(n, Vec::new()))
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

fn no_calls(src: &str, name: &str) -> String {
    format!(
        "`{src}`: `{name}(...)` is a call, and the first version has none: a check reads values \
         and never runs the program's code. Read the field the function would return instead \
         (docs/design/contracts.md, \"Calls\")"
    )
}
