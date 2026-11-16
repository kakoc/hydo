//! Function fuzzing from contracts (Milestone 275, docs/design/function-fuzzing.md): the same
//! `// hydo: requires` and `ensures` comments that a run checks at every call drive the testing
//! of one function at a time. `requires` and the `input` lines describe the valid inputs, so
//! hydo generates inputs that keep them; `ensures` judges each call's result. A failing input
//! shrinks to a small one, and the seed or the input replays it exactly.
//!
//! - [`rust`]: the function, its types and the crate's structures, read from the sources.
//! - [`value`]: values, their wire and shown forms, generation, and the shrink's candidates.
//! - [`check`]: a clause judged over one call's values.
//! - [`harness`]: the test module written into a copy of the crate.
//! - [`cargo`]: the copy, its manifest, the build and the runs.

pub mod cargo;
pub mod check;
pub mod harness;
pub mod rust;
pub mod value;

use crate::lang::{self, Ast, SrcContract};
use crate::plan::ClauseKind;
use rust::{FTy, Pass, Receiver, TyAst};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use value::{Hints, Rng, Val};

/// `hydo fuzz --help`.
pub const USAGE: &str =
    "usage: hydo fuzz [CRATE_DIR] [--fn NAME]... [--cases N] [--seed S] [--out DIR]
                 [--target-dir DIR] [--toolchain TC] [--release] [--timeout SECS] [--json]
       hydo fuzz [CRATE_DIR] --fn NAME --input 'NAME = VALUE, ...'
       hydo fuzz [CRATE_DIR] --list
  tests each Rust function of the crate that has `// hydo: requires` and `ensures` comments on
  its own, natively: it generates inputs that keep `requires` and the `input` lines, calls the
  function on each, and judges each call by `ensures`. A broken clause or a panic is shrunk to
  a small input. The same seed gives the same inputs, so a finding replays exactly; --input
  replays one input.
  CRATE_DIR is a package's folder (default .). hydo copies the package to --out DIR (default
  hydo-out/fuzz) and adds a test module there; the project's files stay as they are.
  --fn NAME      fuzz only this function (repeat it for more)
  --cases N      inputs per function that keep `requires` (default 1000)
  --seed S       the generator's seed (default 1)
  --target-dir   cargo's build folder (default DIR/target)
  --toolchain TC builds with `cargo +TC`
  --release      builds the harness with --release: no overflow checks and no debug
                 assertions, as the program ships (the default is the test profile)
  --timeout SECS stops a call that takes longer (default 10)
  --list         lists the contracted functions and whether each can be fuzzed
  The exit status is 1 when a clause broke or a call panicked. `hydo guide manifest`, section
  \"Function contracts\", has the comment language; docs/design/function-fuzzing.md the design.";

/// One function to fuzz, as read from the sources.
pub struct Fn {
    /// `ledger::apply`, `common::delta_pack::compress_sequence`, `Ledger::apply`.
    pub name: String,
    /// The file, relative to the package.
    pub file: PathBuf,
    pub line: u32,
    pub contract: SrcContract,
    pub params: Vec<harness::Param>,
    pub ret: FTy,
    ret_ref: bool,
    receiver: Option<Receiver>,
    call: String,
    is_unsafe: bool,
    /// Where the harness module goes in the file (a byte offset).
    insert_at: usize,
    pub hints: Hints,
}

/// Why a contracted function is not fuzzed.
pub struct Skipped {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub why: String,
}

/// The contracted functions of a package's sources.
pub fn find(
    pkg_dir: &Path,
    sources: &[PathBuf],
    pkg: &str,
) -> (Vec<Fn>, Vec<Skipped>, Vec<String>) {
    let mut items = rust::Items::default();
    let mut texts = Vec::new();
    for f in sources {
        if let Ok(t) = std::fs::read_to_string(f) {
            rust::read_items(&t, &mut items);
            texts.push((f.clone(), t));
        }
    }
    let (mut fns, mut skipped, mut errors) = (Vec::new(), Vec::new(), Vec::new());
    for (f, text) in &texts {
        if !text.contains("// hydo:") {
            continue;
        }
        let rel = f.strip_prefix(pkg_dir).unwrap_or(f).to_path_buf();
        let (contracts, errs) = lang::scan(text);
        for e in errs {
            errors.push(format!("{}:{}: {}", rel.display(), e.line, e.message));
        }
        let m = rust::mask(text);
        for c in contracts {
            let skip = |why: String| Skipped {
                name: format!("{pkg}::{}", c.function),
                file: rel.clone(),
                line: c.line,
                why,
            };
            match read_fn(text, &m, &c, &items, pkg, &rel) {
                Ok(f) => fns.push(f),
                Err(why) => skipped.push(skip(why)),
            }
        }
    }
    (fns, skipped, errors)
}

fn read_fn(
    text: &str,
    m: &[u8],
    c: &SrcContract,
    items: &rust::Items,
    pkg: &str,
    rel: &Path,
) -> Result<Fn, String> {
    let decl = rust::line_start(text, c.line);
    let blocks = rust::enclosing(m, decl);
    let mut impl_ty: Option<TyAst> = None;
    let mut insert_at = None;
    for b in blocks.iter().rev() {
        let h = &b.header;
        if h.starts_with("trait ") || h.contains(" trait ") {
            return Err("a method of a trait: fuzz a function of a type that implements it".into());
        }
        if rust::impl_type(h).is_some() && impl_ty.is_none() {
            let (t, generic) = rust::impl_type(h).ok_or("an `impl` block hydo cannot read")?;
            if generic {
                return Err("a method of a generic `impl` block: not fuzzed yet".into());
            }
            impl_ty = Some(t);
            insert_at = Some(b.close + 1);
            break;
        }
        if h.contains("fn ") {
            return Err("a function inside another function's body".into());
        }
    }
    let sig = rust::signature(text, m, decl)?;
    if sig.is_async {
        return Err("an `async` function: not fuzzed yet".into());
    }
    if !sig.generics.is_empty() {
        return Err(format!(
            "a generic function (`{}`): write the contract on a function of concrete types that calls it",
            sig.generics.join(", ")
        ));
    }
    let res = rust::Resolver {
        items,
        self_ty: impl_ty.as_ref(),
    };
    let mut params = Vec::new();
    if let Some(r) = sig.receiver {
        let t = impl_ty
            .as_ref()
            .ok_or("a `self` parameter outside an `impl` block")?;
        let ty = res.resolve(t);
        ty.generable()
            .map_err(|o| format!("`self`: hydo cannot generate a value of `{o}`"))?;
        params.push(harness::Param {
            name: "self".into(),
            pass: match r {
                Receiver::Value => Pass::Value,
                Receiver::Ref => Pass::Ref { unsized_ref: false },
                Receiver::RefMut => Pass::RefMut { unsized_ref: false },
            },
            ty,
        });
    }
    for (n, t) in &sig.params {
        let ty = res.resolve(t);
        ty.generable()
            .map_err(|o| format!("the parameter `{n}`: hydo cannot generate a value of `{o}`"))?;
        if let TyAst::Ref(_, inner) = t {
            if matches!(**inner, TyAst::Ref(..)) {
                return Err(format!("the parameter `{n}` is a reference to a reference"));
            }
        }
        params.push(harness::Param {
            name: n.clone(),
            pass: rust::pass_of(t),
            ty,
        });
    }
    let (ret, ret_ref) = match &sig.ret {
        Some(t) => (res.resolve(t), matches!(t, TyAst::Ref(..))),
        None => (FTy::Tuple(vec![]), false),
    };
    // every name a clause reads is a parameter or `result`
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    for cl in &c.clauses {
        let mut roots = Vec::new();
        roots_of(&cl.ast, &mut roots);
        for r in roots {
            if r != "result" && !names.contains(&r.as_str()) {
                return Err(format!(
                    "line {}: `{r}` is not a parameter of the function",
                    cl.line
                ));
            }
        }
    }
    let mut hints = Hints::default();
    for cl in &c.clauses {
        check::hints_of(&cl.ast, &mut hints);
    }
    for x in &c.inputs {
        let hi = if x.inclusive { x.hi } else { x.hi - 1 };
        hints.domains.insert(x.name.replace(" ", ""), (x.lo, hi));
    }
    for v in hints.consts.values_mut() {
        v.sort();
        v.dedup();
    }
    let (call, name) = match &impl_ty {
        Some(t) => (
            format!("<{}>::{}", t.text(), sig.name),
            format!("{pkg}::{}::{}", t.text(), sig.name),
        ),
        None => (
            format!("super::{}", sig.name),
            format!("{pkg}::{}", sig.name),
        ),
    };
    let insert_at = match insert_at {
        Some(i) => i,
        None => rust::close_of(m, sig.body_open).ok_or("the function's body does not end")? + 1,
    };
    Ok(Fn {
        name,
        file: rel.to_path_buf(),
        line: c.line,
        contract: c.clone(),
        params,
        ret,
        ret_ref,
        receiver: sig.receiver,
        call,
        is_unsafe: sig.is_unsafe,
        insert_at,
        hints,
    })
}

fn roots_of(a: &Ast, out: &mut Vec<String>) {
    match a {
        Ast::Name(n) => out.push(n.clone()),
        Ast::Field(x, _) | Ast::Call(_, x) | Ast::Old(x) | Ast::Not(x) | Ast::Neg(x) => {
            roots_of(x, out)
        }
        Ast::Index(x, y) | Ast::Bin(_, x, y) => {
            roots_of(x, out);
            roots_of(y, out);
        }
        _ => {}
    }
}

/// How one call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Returned {
        result: Val,
        after: Vec<Val>,
    },
    Panicked {
        location: String,
        message: String,
    },
    /// The process ended during the call (an abort, a stack overflow).
    Ended,
    /// The call took longer than the timeout.
    Hung,
}

/// What a failing call broke: a clause (its index in the contract), a panic at a location, or
/// the process.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Failure {
    Clause(usize),
    Panic(String),
    Ended,
    Hung,
}

/// The judgement of one call.
pub struct Judged {
    pub failures: Vec<Failure>,
    /// The clauses (by index) that could not be judged.
    pub undecided: Vec<usize>,
}

impl Fn {
    fn module(&self, k: usize) -> String {
        format!("__hydo_fuzz_{k}")
    }

    fn spec(&self, k: usize) -> harness::Spec {
        harness::Spec {
            module: self.module(k),
            call: self.call.clone(),
            receiver: self.receiver,
            params: self
                .params
                .iter()
                .map(|p| harness::Param {
                    name: p.name.clone(),
                    pass: p.pass,
                    ty: p.ty.clone(),
                })
                .collect(),
            ret: self.ret.clone(),
            ret_ref: self.ret_ref,
            is_unsafe: self.is_unsafe,
        }
    }

    fn entry_env(&self, args: &[Val]) -> Vec<(String, Val)> {
        self.params
            .iter()
            .zip(args)
            .map(|(p, v)| (p.name.clone(), v.clone()))
            .collect()
    }

    /// Whether the input keeps every `requires` clause (an undecided one does not).
    pub fn admits(&self, args: &[Val]) -> bool {
        let e = self.entry_env(args);
        let env = check::Env {
            entry: &e,
            after: &e,
            result: None,
        };
        self.contract
            .clauses
            .iter()
            .filter(|c| c.kind == ClauseKind::Requires)
            .all(|c| check::holds(&c.ast, &env) == Ok(true))
    }

    /// The values after the call: each `&mut` parameter's from the answer, the others' from
    /// the input.
    fn after_env(&self, args: &[Val], after: &[Val]) -> Vec<(String, Val)> {
        let mut it = after.iter();
        self.params
            .iter()
            .zip(args)
            .map(|(p, v)| {
                let v = if matches!(p.pass, Pass::RefMut { .. }) {
                    it.next().cloned().unwrap_or(Val::Opaque)
                } else {
                    v.clone()
                };
                (p.name.clone(), v)
            })
            .collect()
    }

    pub fn judge(&self, args: &[Val], o: &Outcome) -> Judged {
        let mut j = Judged {
            failures: vec![],
            undecided: vec![],
        };
        match o {
            Outcome::Returned { result, after } => {
                let e = self.entry_env(args);
                let a = self.after_env(args, after);
                let env = check::Env {
                    entry: &e,
                    after: &a,
                    result: Some(result),
                };
                for (i, c) in self.contract.clauses.iter().enumerate() {
                    if c.kind != ClauseKind::Ensures {
                        continue;
                    }
                    match check::holds(&c.ast, &env) {
                        Ok(true) => {}
                        Ok(false) => j.failures.push(Failure::Clause(i)),
                        Err(_) => j.undecided.push(i),
                    }
                }
            }
            Outcome::Panicked { location, .. } => j.failures.push(Failure::Panic(location.clone())),
            Outcome::Ended => j.failures.push(Failure::Ended),
            Outcome::Hung => j.failures.push(Failure::Hung),
        }
        j
    }

    /// The input as `name = value, ...`.
    pub fn show_args(&self, args: &[Val]) -> String {
        self.params
            .iter()
            .zip(args)
            .map(|(p, v)| format!("{} = {}", p.name, value::show(v, &p.ty)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The values after a call: each `&mut` parameter and the result.
    pub fn show_after(&self, o: &Outcome) -> String {
        match o {
            Outcome::Returned { result, after } => {
                let mut parts = Vec::new();
                let mut it = after.iter();
                for p in &self.params {
                    if matches!(p.pass, Pass::RefMut { .. }) {
                        if let Some(v) = it.next() {
                            parts.push(format!("{} = {}", p.name, value::show(v, &p.ty)));
                        }
                    }
                }
                if self.ret != FTy::Tuple(vec![]) {
                    parts.push(format!("result = {}", value::show(result, &self.ret)));
                }
                parts.join(", ")
            }
            Outcome::Panicked { location, message } => {
                format!("panicked at {location}: {message}")
            }
            Outcome::Ended => "the process ended during the call".into(),
            Outcome::Hung => "the call did not return within the timeout".into(),
        }
    }

    /// Generate the `k`-th input: the first is the smallest values, the second the largest,
    /// the others random.
    fn generate(&self, rng: &mut Rng, k: usize) -> Vec<Val> {
        let mut g = value::Gen {
            rng,
            hints: &self.hints,
            mode: match k {
                0 => 0,
                1 => 1,
                _ => 2,
            },
            max_len: if k < 100 { 4 } else { 16 },
        };
        let mut args: Vec<Val> = self.params.iter().map(|p| g.val(&p.ty, &p.name)).collect();
        if k >= 2 {
            // relate the paths that a clause compares: sometimes equal, or one apart
            for (a, b) in &self.hints.pairs {
                if g.rng.below(4) != 0 {
                    continue;
                }
                let (Some((ia, pa)), Some((ib, pb))) = (self.split_path(a), self.split_path(b))
                else {
                    continue;
                };
                let Some(Val::Int(bv)) = value::at(&args[ib], &pb).cloned() else {
                    continue;
                };
                let Some(FTy::Int { signed, bits, .. }) = value::ty_at(&self.params[ia].ty, &pa)
                else {
                    continue;
                };
                let (lo, hi) = value::int_range(*signed, *bits);
                let (lo, hi) = match self.hints.domains.get(a) {
                    Some(&(x, y)) => (x.max(lo), y.min(hi)),
                    None => (lo, hi),
                };
                let nv = bv + g.rng.below(3) as i128 - 1;
                if (lo..=hi).contains(&nv) {
                    if let Some(slot) = value::at_mut(&mut args[ia], &pa) {
                        *slot = Val::Int(nv);
                    }
                }
            }
        }
        args
    }

    /// A path's parameter index and the rest of the path (none for paths into lists).
    fn split_path<'a>(&self, path: &'a str) -> Option<(usize, Vec<&'a str>)> {
        if path.contains('[') || path.contains('#') {
            return None;
        }
        let mut it = path.split('.');
        let first = it.next()?;
        let i = self.params.iter().position(|p| p.name == first)?;
        Some((i, it.collect()))
    }

    /// Simpler inputs for the shrink, each one change away and each keeping `requires`.
    fn candidates(&self, args: &[Val]) -> Vec<Vec<Val>> {
        let mut out = Vec::new();
        for (i, p) in self.params.iter().enumerate() {
            for s in value::simpler(&args[i], &p.ty, &p.name, &self.hints) {
                let mut c = args.to_vec();
                c[i] = s;
                if self.admits(&c) && !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out
    }
}

/// The compiled harness of one function, ready to run inputs.
pub struct Runner<'a> {
    pub f: &'a Fn,
    pub exe: PathBuf,
    pub filter: String,
    pub work: PathBuf,
    pub timeout: Duration,
    /// The copy's folder: a location under it is shown relative to the package.
    pub copy: PathBuf,
    /// Runs of the test program so far.
    pub runs: u64,
}

impl Runner<'_> {
    /// Run inputs and return each one's outcome. A process that ends or hangs during a call
    /// gives that call `Ended` or `Hung`, and the calls after it run in a new process.
    pub fn run(&mut self, inputs: &[Vec<Val>]) -> Result<Vec<Outcome>, String> {
        let mut outs: Vec<Option<Outcome>> = vec![None; inputs.len()];
        let mut from = 0;
        while from < inputs.len() {
            let mut text = String::new();
            for (k, a) in inputs.iter().enumerate().skip(from) {
                text.push_str(&k.to_string());
                text.push(' ');
                for v in a {
                    value::encode(v, &mut text);
                }
                text.push('\n');
            }
            let cases = self.work.join("cases.txt");
            let answers = self.work.join("answers.txt");
            std::fs::write(&cases, &text).map_err(|e| e.to_string())?;
            let ran = cargo::run(&self.exe, &self.filter, &cases, &answers, self.timeout, &[])?;
            self.runs += 1;
            let mut next = from;
            for line in &ran.lines {
                let (k, rest) = line.split_once(' ').unwrap_or((line, ""));
                let Ok(k) = k.parse::<usize>() else { continue };
                if k >= inputs.len() {
                    continue;
                }
                outs[k] = Some(self.parse_answer(rest)?);
                next = k + 1;
            }
            if next < inputs.len() {
                // the call at `next` ended the process or did not return
                outs[next] = Some(if ran.hung {
                    Outcome::Hung
                } else {
                    Outcome::Ended
                });
                next += 1;
            }
            from = next;
        }
        Ok(outs
            .into_iter()
            .map(|o| o.unwrap_or(Outcome::Ended))
            .collect())
    }

    fn parse_answer(&self, rest: &str) -> Result<Outcome, String> {
        let mut w = rest.split_ascii_whitespace();
        match w.next() {
            Some("R") => {
                let result = value::decode(&self.f.ret, &mut w)?;
                let mut after = Vec::new();
                for p in &self.f.params {
                    if matches!(p.pass, Pass::RefMut { .. }) {
                        after.push(value::decode(&p.ty, &mut w)?);
                    }
                }
                Ok(Outcome::Returned { result, after })
            }
            Some("P") => {
                let mut s = || match value::decode(&FTy::Str, &mut w) {
                    Ok(Val::Str(s)) => s,
                    _ => String::new(),
                };
                let mut location = s();
                for base in [
                    self.copy.clone(),
                    self.copy.canonicalize().unwrap_or_default(),
                ] {
                    let b = format!("{}/", base.display());
                    if let Some(r) = location.strip_prefix(&b) {
                        location = r.to_string();
                    }
                }
                let message = s();
                Ok(Outcome::Panicked { location, message })
            }
            o => Err(format!("the harness answered `{}`", o.unwrap_or(""))),
        }
    }
}

/// One clause or failure over all cases, for the report and `fuzz.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    /// `ensures`, `panic`, `ended` or `hung`.
    pub kind: String,
    /// The clause's text, or the panic's location.
    pub text: String,
    /// The clause's line (0 for the others).
    pub line: u32,
    /// The cases it broke on.
    pub cases: u64,
    /// The first case (0-based) that broke it.
    pub first_case: u64,
    pub first_input: String,
    /// The smallest input that still breaks it, with what the call gave.
    pub input: String,
    pub after: String,
    pub shrink_steps: u64,
    /// The input replays to the same failure.
    pub replayed: bool,
}

/// A clause that held, and on how many cases it was judged.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Held {
    pub text: String,
    pub line: u32,
    pub cases: u64,
    pub undecided: u64,
}

/// What fuzzing one function gave.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FnReport {
    pub function: String,
    pub file: String,
    pub line: u32,
    pub seed: u64,
    pub cases: u64,
    /// Generated inputs that broke `requires` and were not run.
    pub rejected: u64,
    pub broken: Vec<Finding>,
    pub held: Vec<Held>,
    /// Runs of the test program, the shrink's included.
    pub runs: u64,
}

/// Fuzz one function: `cases` inputs from `seed`, then a shrink of the first input of each
/// failure.
pub fn fuzz(r: &mut Runner, seed: u64, cases: u64) -> Result<FnReport, String> {
    let f = r.f;
    let mut rng = Rng::new(seed ^ fnv(&f.name));
    let (mut inputs, mut rejected) = (Vec::new(), 0u64);
    let mut k = 0;
    while (inputs.len() as u64) < cases && k < (cases as usize).saturating_mul(50).max(100) {
        let a = f.generate(&mut rng, k);
        k += 1;
        if f.admits(&a) {
            inputs.push(a);
        } else {
            rejected += 1;
        }
    }
    let outs = r.run(&inputs)?;
    let mut by_failure: BTreeMap<Failure, (u64, usize)> = BTreeMap::new();
    let mut held: Vec<Held> = f
        .contract
        .clauses
        .iter()
        .filter(|c| c.kind == ClauseKind::Ensures)
        .map(|c| Held {
            text: c.text.clone(),
            line: c.line,
            ..Held::default()
        })
        .collect();
    let ens: Vec<usize> = f
        .contract
        .clauses
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == ClauseKind::Ensures)
        .map(|(i, _)| i)
        .collect();
    for (i, (a, o)) in inputs.iter().zip(&outs).enumerate() {
        let j = f.judge(a, o);
        for x in &j.failures {
            let e = by_failure.entry(x.clone()).or_insert((0, i));
            e.0 += 1;
        }
        if matches!(o, Outcome::Returned { .. }) {
            for (h, ci) in held.iter_mut().zip(&ens) {
                if j.undecided.contains(ci) {
                    h.undecided += 1;
                } else {
                    h.cases += 1;
                }
            }
        }
    }
    let mut broken = Vec::new();
    for (x, (n, first)) in &by_failure {
        let start = inputs[*first].clone();
        let (min, steps) = shrink(r, &start, x)?;
        let again = r.run(std::slice::from_ref(&min))?;
        let replayed = f.judge(&min, &again[0]).failures.contains(x);
        let (kind, text, line) = match x {
            Failure::Clause(i) => {
                let c = &f.contract.clauses[*i];
                ("ensures".to_string(), c.text.clone(), c.line)
            }
            Failure::Panic(l) => ("panic".into(), l.clone(), 0),
            Failure::Ended => ("ended".into(), "the process ended".into(), 0),
            Failure::Hung => ("hung".into(), "the call did not return".into(), 0),
        };
        broken.push(Finding {
            kind,
            text,
            line,
            cases: *n,
            first_case: *first as u64,
            first_input: f.show_args(&start),
            input: f.show_args(&min),
            after: f.show_after(&again[0]),
            shrink_steps: steps,
            replayed,
        });
    }
    held.retain(|h| {
        !broken
            .iter()
            .any(|b| b.kind == "ensures" && b.line == h.line && b.text == h.text)
    });
    Ok(FnReport {
        function: f.name.clone(),
        file: f.file.to_string_lossy().into(),
        line: f.line,
        seed,
        cases: inputs.len() as u64,
        rejected,
        broken,
        held,
        runs: r.runs,
    })
}

/// Shrink an input that fails with `x`: take the first simpler input that fails the same way,
/// until none does.
fn shrink(r: &mut Runner, start: &[Val], x: &Failure) -> Result<(Vec<Val>, u64), String> {
    let f = r.f;
    let mut cur = start.to_vec();
    let mut steps = 0;
    let rounds = if *x == Failure::Hung { 3 } else { 300 };
    // the candidates run in chunks, and a round ends at the first chunk with a failing one
    let chunk = if *x == Failure::Hung { 2 } else { 64 };
    'rounds: for _ in 0..rounds {
        let mut cands = f.candidates(&cur);
        cands.truncate(if *x == Failure::Hung { 4 } else { 4096 });
        for part in cands.chunks(chunk) {
            let outs = r.run(part)?;
            if let Some(k) = part
                .iter()
                .zip(&outs)
                .position(|(a, o)| f.judge(a, o).failures.contains(x))
            {
                cur = part[k].clone();
                steps += 1;
                continue 'rounds;
            }
        }
        break;
    }
    Ok((cur, steps))
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Everything `hydo fuzz` wrote to `fuzz.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub package: String,
    pub functions: Vec<FnReport>,
    /// Contracted functions that were not fuzzed, each with why.
    pub skipped: Vec<String>,
}

struct Opts {
    dir: PathBuf,
    fns: Vec<String>,
    cases: u64,
    seed: u64,
    out: PathBuf,
    target_dir: Option<PathBuf>,
    toolchain: Option<String>,
    release: bool,
    timeout: u64,
    input: Option<String>,
    list: bool,
    json: bool,
}

fn opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        dir: PathBuf::from("."),
        fns: vec![],
        cases: 1000,
        seed: 1,
        out: PathBuf::from("hydo-out/fuzz"),
        target_dir: None,
        toolchain: None,
        release: false,
        timeout: 10,
        input: None,
        list: false,
        json: false,
    };
    let mut it = args.iter();
    let mut dir_set = false;
    while let Some(a) = it.next() {
        let mut val = |n: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{n} needs a value"))
        };
        match a.as_str() {
            "--fn" => o.fns.push(val("--fn")?),
            "--cases" => o.cases = val("--cases")?.parse().map_err(|_| "--cases N")?,
            "--seed" => o.seed = val("--seed")?.parse().map_err(|_| "--seed S")?,
            "--out" => o.out = PathBuf::from(val("--out")?),
            "--target-dir" => o.target_dir = Some(PathBuf::from(val("--target-dir")?)),
            "--toolchain" => o.toolchain = Some(val("--toolchain")?),
            "--timeout" => o.timeout = val("--timeout")?.parse().map_err(|_| "--timeout SECS")?,
            "--input" => o.input = Some(val("--input")?),
            "--list" => o.list = true,
            "--release" => o.release = true,
            "--json" => o.json = true,
            s if !s.starts_with('-') && !dir_set => {
                o.dir = PathBuf::from(s);
                dir_set = true;
            }
            s => return Err(format!("unknown option `{s}`")),
        }
    }
    if o.input.is_some() && o.fns.len() != 1 {
        return Err("--input needs exactly one --fn".into());
    }
    Ok(o)
}

/// Whether the function is the one a `--fn` names: by its name, or by the end of its full name.
fn named(f: &Fn, want: &[String]) -> bool {
    want.is_empty()
        || want.iter().any(|w| {
            let short = f.name.rsplit("::").next().unwrap_or(&f.name);
            w == short || w == &f.name || f.name.ends_with(&format!("::{w}"))
        })
}

/// `hydo fuzz`.
pub fn main(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let o = match opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("hydo fuzz: {e}\n{USAGE}");
            return 2;
        }
    };
    match run_all(&o) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("hydo fuzz: {e}");
            2
        }
    }
}

fn run_all(o: &Opts) -> Result<i32, String> {
    let cg = cargo::Cargo {
        toolchain: o.toolchain.clone(),
        release: o.release,
    };
    let pkg = cargo::metadata(&cg, &o.dir)?;
    let sources = pkg.sources();
    let (fns, skipped, errors) = find(&pkg.dir, &sources, &pkg.name);
    for e in &errors {
        eprintln!("hydo fuzz: {e}");
    }
    if !errors.is_empty() {
        return Ok(2);
    }
    let fns: Vec<Fn> = fns.into_iter().filter(|f| named(f, &o.fns)).collect();
    if o.list {
        for f in &fns {
            println!(
                "{} ({}:{}): can be fuzzed",
                f.name,
                f.file.display(),
                f.line
            );
        }
        for s in &skipped {
            println!(
                "{} ({}:{}): not fuzzed: {}",
                s.name,
                s.file.display(),
                s.line,
                s.why
            );
        }
        return Ok(0);
    }
    for s in &skipped {
        if o.fns.is_empty() || o.fns.iter().any(|w| s.name.ends_with(w.as_str())) {
            println!(
                "hydo: fuzz: {} ({}:{}) is not fuzzed: {}",
                s.name,
                s.file.display(),
                s.line,
                s.why
            );
        }
    }
    if fns.is_empty() {
        return Err(if o.fns.is_empty() {
            "no function of the package can be fuzzed (`hydo fuzz --list` says why)".into()
        } else {
            format!("no function named {} can be fuzzed", o.fns.join(", "))
        });
    }
    // the copy, with the harness modules
    let out = if o.out.is_absolute() {
        o.out.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(&o.out)
    };
    let copy = out.join("crate");
    if copy.exists() {
        std::fs::remove_dir_all(&copy).map_err(|e| format!("{}: {e}", copy.display()))?;
    }
    std::fs::create_dir_all(&copy).map_err(|e| e.to_string())?;
    cargo::copy_package(&pkg, &copy)?;
    let first = pkg
        .target_of(&pkg.dir.join(&fns[0].file))
        .ok_or_else(|| {
            format!(
                "{}: in no library or binary of the package",
                fns[0].file.display()
            )
        })?
        .clone();
    let fns: Vec<Fn> = fns
        .into_iter()
        .filter(|f| {
            let t = pkg.target_of(&pkg.dir.join(&f.file));
            let same = t.is_some_and(|t| t.name == first.name && t.kind == first.kind);
            if !same {
                println!(
                    "hydo: fuzz: {} is in another target of the package than {}; fuzz it with its own --fn",
                    f.name, first.name
                );
            }
            same
        })
        .collect();
    std::fs::write(copy.join("Cargo.toml"), cargo::manifest(&pkg, &first))
        .map_err(|e| e.to_string())?;
    let mut by_file: BTreeMap<&Path, Vec<(usize, String)>> = BTreeMap::new();
    for (k, f) in fns.iter().enumerate() {
        by_file
            .entry(f.file.as_path())
            .or_default()
            .push((f.insert_at, harness::module(&f.spec(k))));
    }
    for (file, mut ins) in by_file {
        let p = copy.join(file);
        let mut text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        ins.sort_by(|a, b| b.0.cmp(&a.0));
        for (at, m) in ins {
            text.insert_str(at, &m);
        }
        std::fs::write(&p, text).map_err(|e| e.to_string())?;
    }
    let target_dir = o.target_dir.clone().unwrap_or_else(|| out.join("target"));
    println!(
        "hydo: fuzz: building the harness of {} function{} ({})",
        fns.len(),
        if fns.len() == 1 { "" } else { "s" },
        copy.display()
    );
    let exe = cargo::build(&cg, &copy, &target_dir, &first, &out.join("build.log"))?;
    let work = out.join("work");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let timeout = Duration::from_secs(o.timeout.max(1));
    if let Some(inp) = &o.input {
        let f = &fns[0];
        let named: Vec<(String, FTy)> = f
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        let args = value::parse_args(inp, &named)?;
        let mut r = Runner {
            f,
            exe: exe.clone(),
            filter: format!("{}::run", f.module(0)),
            work: work.clone(),
            timeout,
            copy: copy.clone(),
            runs: 0,
        };
        if !f.admits(&args) {
            println!(
                "hydo: fuzz: the input breaks a `requires` clause of {}",
                f.name
            );
        }
        let out = r.run(std::slice::from_ref(&args))?;
        let j = f.judge(&args, &out[0]);
        println!("hydo: fuzz: {}({})", f.name, f.show_args(&args));
        println!("hydo:   {}", f.show_after(&out[0]));
        for x in &j.failures {
            println!("hydo:   broken: {}", describe(f, x));
        }
        for i in &j.undecided {
            println!(
                "hydo:   undecided: ensures {} (line {})",
                f.contract.clauses[*i].text, f.contract.clauses[*i].line
            );
        }
        return Ok(if j.failures.is_empty() { 0 } else { 1 });
    }
    let mut report = Report {
        package: pkg.name.clone(),
        functions: vec![],
        skipped: skipped
            .iter()
            .map(|s| format!("{} ({}:{}): {}", s.name, s.file.display(), s.line, s.why))
            .collect(),
    };
    let mut any = false;
    for (k, f) in fns.iter().enumerate() {
        let mut r = Runner {
            f,
            exe: exe.clone(),
            filter: format!("{}::run", f.module(k)),
            work: work.clone(),
            timeout,
            copy: copy.clone(),
            runs: 0,
        };
        let rep = fuzz(&mut r, o.seed, o.cases)?;
        if !o.json {
            print_report(&rep, &o.dir, o.seed);
        }
        any |= !rep.broken.is_empty();
        report.functions.push(rep);
    }
    let j = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(out.join("fuzz.json"), format!("{j}\n")).map_err(|e| e.to_string())?;
    if o.json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "hydo: fuzz: the report is {}",
            out.join("fuzz.json").display()
        );
    }
    Ok(if any { 1 } else { 0 })
}

fn describe(f: &Fn, x: &Failure) -> String {
    match x {
        Failure::Clause(i) => {
            let c = &f.contract.clauses[*i];
            format!("ensures {} (line {})", c.text, c.line)
        }
        Failure::Panic(l) => format!("the call panicked at {l}"),
        Failure::Ended => "the process ended during the call".into(),
        Failure::Hung => "the call did not return".into(),
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn print_report(r: &FnReport, dir: &Path, seed: u64) {
    println!(
        "hydo: fuzz: {} ({}:{}): {} inputs from seed {}, {} generated inputs broke `requires`",
        r.function, r.file, r.line, r.cases, seed, r.rejected
    );
    let short = r.function.rsplit("::").next().unwrap_or(&r.function);
    for b in &r.broken {
        let what = match b.kind.as_str() {
            "ensures" => format!(
                "contract broken: {} ensures {} ({}:{})",
                r.function, b.text, r.file, b.line
            ),
            "panic" => format!("{} panicked at {}", r.function, b.text),
            _ => format!("{}: {}", r.function, b.text),
        };
        println!("hydo: {what} on {} of {} inputs", b.cases, r.cases);
        println!(
            "hydo:   smallest input ({} shrink steps): {}",
            b.shrink_steps, b.input
        );
        println!("hydo:   the call gave: {}", b.after);
        println!(
            "hydo:   again: hydo fuzz {} --fn {short} --input {}",
            dir.display(),
            shell_quote(&b.input)
        );
    }
    for h in &r.held {
        let und = if h.undecided > 0 {
            format!(", undecided on {}", h.undecided)
        } else {
            String::new()
        };
        println!(
            "hydo:   held: ensures {} (line {}) on {} inputs{und}",
            h.text, h.line, h.cases
        );
    }
    if r.broken.is_empty() {
        println!("hydo: fuzz: {}: every clause held", r.function);
    }
}
