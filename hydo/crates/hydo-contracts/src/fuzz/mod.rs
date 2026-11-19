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
pub mod vm;

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
                 [--target-dir DIR] [--toolchain TC] [--release] [--timeout SECS]
                 [--min-inputs N] [--min-reach N] [--allow-skipped]
                 [--show-inputs N] [--json]
       hydo fuzz [CRATE_DIR] --fn NAME --input 'NAME = VALUE, ...'
       hydo fuzz [CRATE_DIR] --list
  tests each Rust function of the crate that has `// hydo: requires` and `ensures` comments on
  its own, natively: it generates inputs that keep `requires` and the `input` lines, calls the
  function on each, and judges each call by `ensures`. A broken clause or a panic is shrunk to
  a small input. The same seed gives the same inputs, so a finding replays exactly; --input
  replays one input. `hydo guide fuzz` has the comment language and a worked example.
  CRATE_DIR is a package's folder (default .). hydo copies the package to --out DIR (default
  hydo-out/fuzz) and adds a test module there; the project's files stay as they are. A
  function whose test module does not build is reported, and the others run.
  --fn NAME        fuzz only this function (repeat it for more); a name or its path's end
  --cases N        inputs per function that keep `requires` (default 1000)
  --seed S         the generator's seed (default 1)
  --target-dir     cargo's build folder (default DIR/target)
  --toolchain TC   builds with `cargo +TC`
  --release        builds the harness with --release: no overflow checks and no debug
                   assertions, as the program ships (the default is the test profile)
  --timeout SECS   stops a call that takes longer (default 10); after 3 such calls the
                   function's other inputs are not run
  --min-inputs N   fewer calls than N give the verdict `too few inputs` (default a tenth of
                   --cases; 0 turns it off)
  --min-reach N    a clause `a || ... || z` whose `z` decided fewer than N calls, or a
                   property `-> Option<bool>` that answered `Some` fewer than N times, gives
                   the verdict `vacuous`, and a clause that hydo decided on fewer than N
                   calls gives `not judged` (default 1 % of --cases; 0 turns both off)
  --allow-skipped  a contracted function that is not fuzzed (refused, or its harness did
                   not build) leaves the exit status as it is; without it the status is 3
  --show-inputs N  prints N of each function's inputs, spread over the run, and keeps them
                   in fuzz.json as `samples`
  --list           lists the contracted functions and whether each can be fuzzed
  --json           prints the report as one JSON line; the progress goes to standard error
  Each function ends with one verdict: `held`, `broken`, `not judged`, `vacuous` or `too few
  inputs`. The exit status is 1 when a clause broke or a call failed, 3 when none did but a
  function tested too little or a contracted function was not fuzzed, 2 for a mistake in the
  options or the contracts, and 0 otherwise.";

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
    /// The lints that the crate forbids anywhere, which the harness must not allow.
    forbidden: Vec<String>,
}

/// Why a contracted function is not fuzzed.
pub struct Skipped {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub why: String,
}

/// The durability checks among `fns` (Milestone 292): a function that another's `durable` line
/// names is called by `hydo fuzz --vm` after a power cut ([`vm`]), and is not fuzzed on its own.
/// The functions, and the checks with the writer that names each.
pub fn split_checks(fns: Vec<Fn>, sources: &[PathBuf]) -> (Vec<Fn>, Vec<(Fn, String)>) {
    let lines = durable_lines(sources);
    let (mut plain, mut checks) = (Vec::new(), Vec::new());
    for f in fns {
        match vm::checked_by(&f, &lines) {
            Some(of) => checks.push((f, of)),
            None => plain.push(f),
        }
    }
    (plain, checks)
}

/// The contracted functions of a package's sources, the durability checks left out
/// ([`split_checks`]).
pub fn find(
    pkg_dir: &Path,
    sources: &[PathBuf],
    pkg: &str,
) -> (Vec<Fn>, Vec<Skipped>, Vec<String>) {
    let (all, skipped, errors) = find_all(pkg_dir, sources, pkg);
    (split_checks(all, sources).0, skipped, errors)
}

/// Every `durable` line of the sources: the writer's name and the check it names.
pub(crate) fn durable_lines(sources: &[PathBuf]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for f in sources {
        let Ok(t) = std::fs::read_to_string(f) else {
            continue;
        };
        if !t.contains("// hydo: durable") {
            continue;
        }
        for c in lang::scan(&t).0 {
            if let Some((d, _)) = c.durable {
                out.push((c.function, d));
            }
        }
    }
    out
}

/// [`find`] with the durability checks among the functions.
pub fn find_all(
    pkg_dir: &Path,
    sources: &[PathBuf],
    pkg: &str,
) -> (Vec<Fn>, Vec<Skipped>, Vec<String>) {
    let mut items = rust::Items::default();
    let mut texts = Vec::new();
    for f in sources {
        if let Ok(t) = std::fs::read_to_string(f) {
            let rel = f.strip_prefix(pkg_dir).unwrap_or(f);
            rust::read_items_in(&t, &rust::file_module(rel), &mut items);
            texts.push((f.clone(), t));
        }
    }
    let (mut fns, mut skipped, mut errors) = (Vec::new(), Vec::new(), Vec::new());
    // the `build` and `read` lines hold for the whole crate (Milestone 307)
    for (f, text) in &texts {
        if !text.contains("// hydo: build") && !text.contains("// hydo: read") {
            continue;
        }
        let rel = f.strip_prefix(pkg_dir).unwrap_or(f).to_path_buf();
        for c in lang::scan(text).0 {
            for b in &c.builds {
                let mut args = Vec::new();
                for (n, t) in &b.args {
                    match rust::parse_ty(t) {
                        Ok(t) => args.push((n.clone(), t)),
                        Err(e) => errors.push(format!(
                            "{}:{}: `build {}`: the type of `{n}`: {e}",
                            rel.display(),
                            b.line,
                            b.ty
                        )),
                    }
                }
                items
                    .builds
                    .entry(rust::type_key(&b.ty))
                    .or_insert(rust::BuildDef {
                        ctor: b.ctor.clone(),
                        args,
                    });
            }
            for r in &c.reads {
                match rust::parse_ty(&r.view) {
                    Ok(v) => {
                        items
                            .reads
                            .entry(rust::type_key(&r.ty))
                            .or_insert((v, r.method.clone()));
                    }
                    Err(e) => errors.push(format!(
                        "{}:{}: `read {}`: {e}",
                        rel.display(),
                        r.line,
                        r.ty
                    )),
                }
            }
        }
    }
    let mut forbidden: Vec<String> = Vec::new();
    for (_, text) in &texts {
        for l in forbidden_lints(text) {
            if !forbidden.contains(&l) {
                forbidden.push(l);
            }
        }
    }
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
        let file_mod = rust::file_module(&rel);
        for c in contracts {
            let mut module = file_mod.clone();
            let at = rust::line_start(text, c.line);
            module.extend(rust::inline_modules(&m, at));
            // the type or trait of a method, for its name
            let owner: Vec<String> = rust::enclosing(&m, at)
                .iter()
                .rev()
                .find_map(|b| {
                    if let Some((t, _)) = rust::impl_type(&b.header) {
                        return Some(t.text());
                    }
                    let h = b.header.strip_prefix("pub ").unwrap_or(&b.header);
                    h.strip_prefix("trait ").map(|r| {
                        r.chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect()
                    })
                })
                .into_iter()
                .collect();
            let skip = |why: String| Skipped {
                name: [pkg.to_string()]
                    .into_iter()
                    .chain(module.iter().cloned())
                    .chain(owner.iter().cloned())
                    .chain([c.function.clone()])
                    .collect::<Vec<_>>()
                    .join("::"),
                file: rel.clone(),
                line: c.line,
                why,
            };
            // a trait's method is fuzzed once for each `with Self = <type>` line (Milestone 307)
            let selves: Vec<&(String, String)> = c.with.iter().filter(|(n, _)| n == SELF).collect();
            let each: Vec<SrcContract> = if selves.len() > 1 {
                selves
                    .iter()
                    .map(|s| {
                        let mut one = c.clone();
                        one.with.retain(|(n, _)| n != SELF);
                        one.with.push((*s).clone());
                        one
                    })
                    .collect()
            } else {
                vec![c.clone()]
            };
            for c in &each {
                match read_fn(text, &m, c, &items, pkg, &module, &rel) {
                    Ok(mut f) => {
                        f.forbidden = forbidden.clone();
                        fns.push(f)
                    }
                    Err(why) => skipped.push(skip(why)),
                }
            }
        }
    }
    (fns, skipped, errors)
}

/// The type parameter that a `with Self = <type>` line names: the type a trait's method is
/// fuzzed on (Milestone 307).
pub const SELF: &str = "Self";

/// The block a contracted function stands in, as fuzzing needs it.
enum Owner {
    /// A free function.
    None,
    /// A method of `impl Type` or `impl Trait for Type`: the type, the trait, and the block's
    /// type parameters.
    Impl(TyAst, Option<TyAst>, Vec<String>),
    /// A method of a trait: its name and type parameters (Milestone 307).
    Trait(String, Vec<String>),
}

/// A generated value with each value that a `build` line builds marked as such: its
/// constructor's arguments, and the value its `read` line gave (`read`, the harness's answer
/// before the call, decoded with the same type), so that a clause reads an element of a list of
/// built values through the `read` line and never as its constructor's arguments (Milestone
/// 318).
fn with_built(v: &Val, t: &FTy, read: Option<&Val>) -> Val {
    let sub = |i: usize| -> Option<&Val> {
        match read {
            Some(Val::List(l) | Val::Tuple(l)) => l.get(i),
            Some(Val::Struct(f) | Val::Enum(_, f)) => f.get(i).map(|(_, x)| x),
            _ => None,
        }
    };
    match (t, v) {
        (FTy::Built { build: Some(_), .. }, Val::Struct(a)) => {
            let view = read.filter(|r| **r != Val::Opaque).cloned().map(Box::new);
            Val::Built(view, a.clone())
        }
        (FTy::Boxed(_, x), _) => with_built(v, x, read),
        (FTy::List(x) | FTy::Array(x, _), Val::List(l)) => Val::List(
            l.iter()
                .enumerate()
                .map(|(i, e)| with_built(e, x, sub(i)))
                .collect(),
        ),
        (FTy::Opt(x), Val::Opt(Some(e))) => {
            let r = match read {
                Some(Val::Opt(Some(r))) => Some(&**r),
                _ => None,
            };
            Val::Opt(Some(Box::new(with_built(e, x, r))))
        }
        (FTy::Res(a, b), Val::Res(r)) => {
            let (ok, err) = match read {
                Some(Val::Res(Ok(x))) => (Some(&**x), None),
                Some(Val::Res(Err(x))) => (None, Some(&**x)),
                _ => (None, None),
            };
            Val::Res(match r {
                Ok(x) => Ok(Box::new(with_built(x, a, ok))),
                Err(x) => Err(Box::new(with_built(x, b, err))),
            })
        }
        (FTy::Tuple(ts), Val::Tuple(vs)) => Val::Tuple(
            vs.iter()
                .zip(ts)
                .enumerate()
                .map(|(i, (x, t))| with_built(x, t, sub(i)))
                .collect(),
        ),
        (FTy::Struct { fields, .. }, Val::Struct(fs)) => Val::Struct(
            fs.iter()
                .zip(fields)
                .enumerate()
                .map(|(i, ((n, x), (_, t)))| (n.clone(), with_built(x, t, sub(i))))
                .collect(),
        ),
        (FTy::Enum { variants, .. }, Val::Enum(n, fs)) => {
            let Some(var) = variants.iter().find(|x| x.name == *n) else {
                return v.clone();
            };
            let same = matches!(read, Some(Val::Enum(m, _)) if m == n);
            Val::Enum(
                n.clone(),
                fs.iter()
                    .zip(&var.fields)
                    .enumerate()
                    .map(|(i, ((fname, x), (_, t)))| {
                        (
                            fname.clone(),
                            with_built(x, t, if same { sub(i) } else { None }),
                        )
                    })
                    .collect(),
            )
        }
        _ => v.clone(),
    }
}

/// How a parameter whose type is `t` is passed: the pass, and for a reference to a reference
/// (`&mut &[u8]`) how the inner reference is made (Milestone 307).
fn passes(t: &TyAst) -> (Pass, Option<Pass>) {
    match t {
        TyAst::Ref(m, inner) if matches!(**inner, TyAst::Ref(..)) => {
            let outer = if *m {
                Pass::RefMut { unsized_ref: false }
            } else {
                Pass::Ref { unsized_ref: false }
            };
            (outer, Some(rust::pass_of(inner)))
        }
        _ => (rust::pass_of(t), None),
    }
}

/// A parameter's refusal, with what to write when the type needs a `build` line: the
/// parameter's own type, or for a list the type that the reason names.
fn refusal(what: &str, t: &TyAst, why: String) -> String {
    let mut base = t;
    while let TyAst::Ref(_, x) = base {
        base = x;
    }
    let named = matches!(base, TyAst::Path(..))
        .then(|| base.text())
        .or_else(|| {
            why.strip_prefix("hydo cannot generate a value of `")
                .and_then(|r| r.split('`').next())
                .map(str::to_string)
        })
        .filter(|n| !n.is_empty() && !why.contains("// hydo: build"));
    if let Some(n) = named {
        let ty = rust::type_key(&n);
        format!(
            "{what}: {why}; a `// hydo: build {ty} from <constructor>(<name>: <type>, ...)` line \
             names a function that builds one"
        )
    } else {
        format!("{what}: {why}")
    }
}

fn read_fn(
    text: &str,
    m: &[u8],
    c: &SrcContract,
    items: &rust::Items,
    pkg: &str,
    module: &[String],
    rel: &Path,
) -> Result<Fn, String> {
    let decl = rust::line_start(text, c.line);
    let blocks = rust::enclosing(m, decl);
    let mut owner = Owner::None;
    let mut insert_at = None;
    for b in blocks.iter().rev() {
        let h = &b.header;
        if let Some((name, params)) = rust::trait_parts(h) {
            owner = Owner::Trait(name, params);
            insert_at = Some(b.close + 1);
            break;
        }
        if let Some((t, tr, params)) = rust::impl_parts(h) {
            owner = Owner::Impl(t, tr, params);
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
    if !sig.has_body && !matches!(owner, Owner::Trait(..)) {
        return Err("the function has no body (a declaration)".into());
    }
    // each type parameter takes the type that a `with` line names: the function's own, the
    // `impl` block's, and the trait's (Milestone 307)
    let mut subst: BTreeMap<String, TyAst> = BTreeMap::new();
    let mut turbofish = Vec::new();
    let mut missing = Vec::new();
    let with_ty = |name: &str| -> Option<Result<TyAst, String>> {
        c.with
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| rust::parse_ty(t).map_err(|e| format!("`with {name} = {t}`: {e}")))
    };
    let outer: Vec<String> = match &owner {
        Owner::Impl(_, _, p) | Owner::Trait(_, p) => p.clone(),
        Owner::None => vec![],
    };
    for name in &outer {
        if name.starts_with("const ") {
            return Err(format!(
                "a const generic parameter (`{name}`): not fuzzed yet"
            ));
        }
        match with_ty(name) {
            Some(t) => {
                subst.insert(name.clone(), t?);
            }
            None => missing.push(name.clone()),
        }
    }
    for g in &sig.generics {
        if g == "where" {
            continue;
        }
        if g.starts_with("const ") {
            return Err(format!("a const generic parameter (`{g}`): not fuzzed yet"));
        }
        let name = g.split([':', '=']).next().unwrap_or("").trim().to_string();
        match with_ty(&name) {
            Some(t) => {
                let ty = t?;
                turbofish.push(ty.text());
                subst.insert(name, ty);
            }
            None => missing.push(name),
        }
    }
    if let Some(m) = missing.first() {
        let mut all: Vec<String> = outer.clone();
        all.extend(sig.generics.iter().filter(|g| *g != "where").cloned());
        return Err(format!(
            "a generic function (`{}`): name the type to fuzz it with, for example \
             `// hydo: with {m} = i64`, or write the contract on a function of concrete types \
             that calls it",
            all.join(", ")
        ));
    }
    // the type `Self` stands for, and the path of the call
    let (self_ty, call_base, owner_name) = match &owner {
        Owner::None => (None, None, None),
        Owner::Impl(t, tr, _) => {
            let t = rust::subst_ty(t, &subst);
            let base = match tr {
                Some(tr) => format!("<{} as {}>", t.text(), rust::subst_ty(tr, &subst).text()),
                None => format!("<{}>", t.text()),
            };
            let name = t.text();
            (Some(t), Some(base), Some(name))
        }
        Owner::Trait(name, params) => {
            let Some(t) = with_ty(SELF) else {
                return Err(format!(
                    "a method of the trait `{name}`: name the type to fuzz it on, for example \
                     `// hydo: with Self = i64`; the type must implement `{name}`"
                ));
            };
            let t = t?;
            let tr = if params.is_empty() {
                name.clone()
            } else {
                format!(
                    "{name}<{}>",
                    params
                        .iter()
                        .map(|p| subst[p].text())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let base = format!("<{} as {tr}>", t.text());
            (Some(t), Some(base.clone()), Some(base))
        }
    };
    let res = rust::Resolver {
        items,
        self_ty: self_ty.as_ref(),
        module,
        subst: &subst,
    };
    let mut params = Vec::new();
    if let Some(r) = sig.receiver {
        let t = self_ty
            .as_ref()
            .ok_or("a `self` parameter outside an `impl` block")?;
        let ty = res.resolve(t);
        let what = match &owner {
            Owner::Trait(..) => format!("`self` (`with Self = {}`)", t.text()),
            _ => "`self`".to_string(),
        };
        ty.generable().map_err(|why| refusal(&what, t, why))?;
        let inner = matches!(t, TyAst::Ref(..)).then(|| rust::pass_of(t));
        let pass = match r {
            Receiver::Value => rust::pass_of(t),
            Receiver::Ref => Pass::Ref { unsized_ref: false },
            Receiver::RefMut => Pass::RefMut { unsized_ref: false },
        };
        let inner = if r == Receiver::Value { None } else { inner };
        params.push(harness::Param {
            name: "self".into(),
            pass,
            ty,
            inner,
        });
    }
    for (n, t) in &sig.params {
        let ty = res.resolve(t);
        ty.generable()
            .map_err(|why| refusal(&format!("the parameter `{n}`"), t, why))?;
        let (pass, inner) = passes(t);
        params.push(harness::Param {
            name: n.clone(),
            pass,
            ty,
            inner,
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
        check_patterns(&cl.ast, &params, &ret).map_err(|e| format!("line {}: {e}", cl.line))?;
    }
    let mut hints = Hints::default();
    for cl in &c.clauses {
        check::hints_of(&cl.ast, &mut hints);
        if cl.kind == ClauseKind::Requires {
            check::sorted_hints(&cl.ast, &mut hints);
        }
    }
    for x in &c.inputs {
        input_hints(x, &params, &mut hints)
            .map_err(|e| format!("line {}: `input {}`: {e}", x.line, x.text))?;
    }
    for v in hints.consts.values_mut() {
        v.sort();
        v.dedup();
    }
    let prefix: String = [pkg.to_string()]
        .into_iter()
        .chain(module.iter().cloned())
        .collect::<Vec<_>>()
        .join("::");
    let (call, name) = match (call_base, owner_name) {
        (Some(base), Some(owner)) => (
            format!("{base}::{}{}", sig.name, fish(&turbofish)),
            format!("{prefix}::{owner}::{}", sig.name),
        ),
        _ => (
            format!("super::{}{}", sig.name, fish(&turbofish)),
            format!("{prefix}::{}", sig.name),
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
        forbidden: Vec::new(),
    })
}

/// The type of a place that a clause names (a parameter, `result`, their fields, a variant's
/// fields, `some()`, `ok()`, `err()` and elements), when hydo knows it.
fn type_of(a: &Ast, params: &[harness::Param], ret: &FTy) -> Option<FTy> {
    let t = match a {
        Ast::Name(n) if n == "result" => ret.clone(),
        Ast::Name(n) => params.iter().find(|p| p.name == *n)?.ty.clone(),
        Ast::Old(x) => type_of(x, params, ret)?,
        Ast::Field(x, f) => match plain(type_of(x, params, ret)?) {
            FTy::Struct { fields, .. } => fields.into_iter().find(|(n, _)| n == f)?.1,
            FTy::Tuple(v) => v.into_iter().nth(f.parse().ok()?)?,
            FTy::Enum { variants, .. } => {
                let v = variants.into_iter().find(|v| v.name == *f)?;
                FTy::Struct {
                    path: v.name,
                    tuple: v.kind != rust::VKind::Named,
                    fields: v.fields,
                }
            }
            _ => return None,
        },
        Ast::Index(x, _) => match plain(type_of(x, params, ret)?) {
            FTy::List(e) | FTy::Array(e, _) => *e,
            _ => return None,
        },
        Ast::Call(lang::Builtin::Some, x) => match plain(type_of(x, params, ret)?) {
            FTy::Opt(e) => *e,
            _ => return None,
        },
        Ast::Call(b @ (lang::Builtin::Ok | lang::Builtin::Err), x) => {
            match plain(type_of(x, params, ret)?) {
                FTy::Res(o, e) => {
                    if *b == lang::Builtin::Ok {
                        *o
                    } else {
                        *e
                    }
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(plain(t))
}

/// A type without its box, and a type that a `read` line reads as the type it is read as.
fn plain(t: FTy) -> FTy {
    match t {
        FTy::Boxed(_, x) => plain(*x),
        FTy::Built {
            read: Some((v, _)), ..
        } => plain(*v),
        o => o,
    }
}

/// Check the patterns of a clause's `matches()` and the variants it names against the types
/// (Milestone 307), so that a misspelt variant is refused before the run, not undecided on
/// every call.
fn check_patterns(a: &Ast, params: &[harness::Param], ret: &FTy) -> Result<(), String> {
    match a {
        Ast::Matches(x, p) => {
            check_patterns(x, params, ret)?;
            if let Some(t) = type_of(x, params, ret) {
                check_pat(p, &t).map_err(|e| format!("`matches({x}, {p})`: {e}"))?;
            }
        }
        Ast::Field(x, f) => {
            check_patterns(x, params, ret)?;
            if let Some(FTy::Enum { path, variants, .. }) = type_of(x, params, ret) {
                if !variants.iter().any(|v| v.name == *f) {
                    return Err(format!(
                        "`{a}`: `{}` has no variant `{f}`; its variants are {}",
                        path.trim_start_matches("self::"),
                        variant_names(&variants)
                    ));
                }
            }
        }
        Ast::Call(_, x) | Ast::Old(x) | Ast::Not(x) | Ast::Neg(x) => {
            check_patterns(x, params, ret)?
        }
        Ast::Index(x, y) | Ast::Bin(_, x, y) => {
            check_patterns(x, params, ret)?;
            check_patterns(y, params, ret)?;
        }
        _ => {}
    }
    Ok(())
}

fn variant_names(v: &[rust::Variant]) -> String {
    v.iter()
        .map(|v| format!("`{}`", v.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether a pattern fits a type, else why not.
fn check_pat(p: &lang::Pat, t: &FTy) -> Result<(), String> {
    use lang::Pat;
    let t = plain(t.clone());
    let fields = |ps: &[Pat], ts: &[&FTy], what: &str| -> Result<(), String> {
        if ps.len() != ts.len() {
            return Err(format!(
                "the pattern gives {} fields, and {what} has {}",
                ps.len(),
                ts.len()
            ));
        }
        ps.iter().zip(ts).try_for_each(|(p, t)| check_pat(p, t))
    };
    match (p, &t) {
        (Pat::Any, _) => Ok(()),
        (_, FTy::Opaque(o)) => Err(format!("hydo does not read a value of `{o}`")),
        (Pat::Int(_), FTy::Int { .. } | FTy::Float { .. })
        | (Pat::Bool(_), FTy::Bool)
        | (Pat::Str(_), FTy::Str) => Ok(()),
        (Pat::Tuple(ps), FTy::Tuple(ts)) => fields(ps, &ts.iter().collect::<Vec<_>>(), "the tuple"),
        (Pat::Variant(n, sub), FTy::Opt(x)) => match (n.as_str(), sub) {
            ("None", None) => Ok(()),
            ("Some", None) => Ok(()),
            ("Some", Some(ps)) => fields(ps, &[x], "`Some`"),
            _ => Err(format!("an `Option` is `Some(_)` or `None`, not `{p}`")),
        },
        (Pat::Variant(n, sub), FTy::Res(o, e)) => match (n.as_str(), sub) {
            ("Ok" | "Err", None) => Ok(()),
            ("Ok", Some(ps)) => fields(ps, &[o], "`Ok`"),
            ("Err", Some(ps)) => fields(ps, &[e], "`Err`"),
            _ => Err(format!("a `Result` is `Ok(_)` or `Err(_)`, not `{p}`")),
        },
        (Pat::Variant(n, sub), FTy::Enum { path, variants, .. }) => {
            let Some(v) = variants.iter().find(|v| v.name == *n) else {
                return Err(format!(
                    "`{}` has no variant `{n}`; its variants are {}",
                    path.trim_start_matches("self::"),
                    variant_names(variants)
                ));
            };
            match (sub, v.kind) {
                (None, _) => Ok(()),
                (Some(ps), rust::VKind::Named) if !ps.is_empty() => Err(format!(
                    "`{n}` has named fields: write `{n}` in the pattern, and read a field with \
                     `.{n}.<field>`"
                )),
                (Some(ps), _) => fields(
                    ps,
                    &v.fields.iter().map(|(_, t)| t).collect::<Vec<_>>(),
                    &format!("`{n}`"),
                ),
            }
        }
        (
            Pat::Variant(n, sub),
            FTy::Struct {
                path, fields: fs, ..
            },
        ) => {
            let name = path.rsplit("::").next().unwrap_or(path);
            if n != name {
                return Err(format!("the value is a `{name}`, not a `{n}`"));
            }
            match sub {
                None => Ok(()),
                Some(ps) => fields(
                    ps,
                    &fs.iter().map(|(_, t)| t).collect::<Vec<_>>(),
                    &format!("`{n}`"),
                ),
            }
        }
        (p, t) => Err(format!(
            "the pattern `{p}` does not fit a value of `{}`",
            t.rust()
        )),
    }
}

/// The lints that a source's attributes forbid: `#![forbid(unsafe_code)]`, `#[forbid(...)]`,
/// and `forbid(...)` inside `cfg_attr`.
pub fn forbidden_lints(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("forbid(") {
        let at = from + i;
        let after = at + "forbid(".len();
        from = after;
        // only in an attribute: its line starts with `#`
        let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
        let in_attr = text[line_start..at].trim_start().starts_with('#')
            || text[..line_start]
                .lines()
                .next_back()
                .is_some_and(|p| p.trim_start().starts_with("#") && !p.contains(']'));
        if !in_attr {
            continue;
        }
        let end = text[after..].find(')').map_or(text.len(), |e| after + e);
        for name in text[after..end].split(',') {
            let name = name.trim();
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
                && !out.iter().any(|x| x == name)
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// The type arguments of a call (`::<i64>`), or nothing.
fn fish(types: &[String]) -> String {
    if types.is_empty() {
        String::new()
    } else {
        format!("::<{}>", types.join(", "))
    }
}

/// The type at an `input` line's path: a parameter, its fields (`l.balance`), and the elements of
/// lists (`v[]`).
fn input_ty<'a>(path: &str, params: &'a [harness::Param]) -> Result<&'a FTy, String> {
    let mut segs = path.split('.');
    let first = segs.next().unwrap_or("");
    let name = first.trim_end_matches("[]");
    let p = params
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("`{name}` is not a parameter of the function"))?;
    let elems = |mut t: &'a FTy, s: &str| -> Result<&'a FTy, String> {
        for _ in 0..s.matches("[]").count() {
            t = match t {
                FTy::List(x) | FTy::Array(x, _) => x,
                FTy::Boxed(_, b) => match &**b {
                    FTy::List(x) | FTy::Array(x, _) => x,
                    _ => return Err(format!("`{s}`: not a list")),
                },
                _ => return Err(format!("`{s}`: not a list")),
            };
        }
        Ok(t)
    };
    let mut t = elems(&p.ty, first)?;
    let mut variant: Option<&'a [(String, FTy)]> = None;
    for s in segs {
        let f = s.trim_end_matches("[]");
        let inner = match t {
            FTy::Boxed(_, b) => &**b,
            o => o,
        };
        // a variant's field (`op.Clamp.lo`; Milestone 307)
        if let Some(fields) = variant.take() {
            t = fields
                .iter()
                .find(|(n, _)| n == f)
                .map(|(_, t)| t)
                .ok_or_else(|| format!("no field `{f}`"))?;
            t = elems(t, s)?;
            continue;
        }
        t = match inner {
            FTy::Enum { path, variants, .. } => {
                let v = variants
                    .iter()
                    .find(|v| v.name == f)
                    .ok_or_else(|| format!("`{path}` has no variant `{f}`"))?;
                variant = Some(&v.fields);
                continue;
            }
            // a constructor's argument (`self.v`; Milestone 307)
            FTy::Built { build: Some(c), .. } => c
                .args
                .iter()
                .find(|(n, _, _)| n == f)
                .map(|(_, t, _)| t)
                .ok_or_else(|| format!("no argument `{f}`"))?,
            FTy::Struct { fields, .. } => fields
                .iter()
                .find(|(n, _)| n == f)
                .map(|(_, t)| t)
                .ok_or_else(|| format!("no field `{f}`"))?,
            FTy::Tuple(v) => f
                .parse::<usize>()
                .ok()
                .and_then(|i| v.get(i))
                .ok_or_else(|| format!("no field `{f}`"))?,
            _ => return Err(format!("`.{f}` of a value without fields")),
        };
        t = elems(t, s)?;
    }
    if variant.is_some() {
        return Err(format!(
            "`{path}` names a variant: name one of its fields (`{path}.0`, `{path}.<field>`)"
        ));
    }
    Ok(match t {
        FTy::Boxed(_, b) => b,
        o => o,
    })
}

/// What an `input` line tells the generator, checked against the type at its path.
fn input_hints(x: &lang::SrcInput, params: &[harness::Param], h: &mut Hints) -> Result<(), String> {
    let t = input_ty(&x.name, params)?;
    let what = match t {
        FTy::Int { .. } => "an integer",
        FTy::Float { .. } => "a floating-point number",
        FTy::Str => "a string",
        FTy::List(_) | FTy::Array(..) => "a list",
        _ => "a value of another type",
    };
    if let Some(r) = x.range {
        if !matches!(t, FTy::Int { .. } | FTy::Float { .. }) {
            return Err(format!(
                "`in` narrows a number, and `{}` is {what}; write `len` for a length",
                x.name
            ));
        }
        h.domains.insert(x.name.clone(), r);
    }
    if let Some(r) = x.len {
        if !matches!(t, FTy::Str | FTy::List(_)) {
            return Err(format!(
                "`len` is for a string or a list, and `{}` is {what}",
                x.name
            ));
        }
        h.domains.insert(format!("{}#len", x.name), r);
    }
    let bytes = matches!(t, FTy::List(e) | FTy::Array(e, _)
        if matches!(**e, FTy::Int { signed: false, bits: 8, .. }));
    if bytes && (!x.like.is_empty() || !x.like_bytes.is_empty()) && x.chars.is_none() {
        // a list of bytes like the examples: `like b"GET / HTTP/1.1\r\n"` or a text's bytes
        let e = h.like_bytes.entry(x.name.clone()).or_default();
        e.extend(x.like.iter().map(|s| s.as_bytes().to_vec()));
        e.extend(x.like_bytes.iter().cloned());
        return Ok(());
    }
    if !x.like_bytes.is_empty() {
        return Err(format!(
            "`like b\"...\"` is for a list of bytes (`&[u8]`, `Vec<u8>`), and `{}` is {what}; \
             write `like \"...\"` for a string",
            x.name
        ));
    }
    if x.chars.is_some() || !x.like.is_empty() {
        if *t != FTy::Str {
            return Err(format!(
                "`chars` is for a string and `like` for a string or a list of bytes, and `{}` \
                 is {what}",
                x.name
            ));
        }
        if let Some(c) = &x.chars {
            h.chars.insert(x.name.clone(), c.clone());
        }
        if !x.like.is_empty() {
            h.like
                .entry(x.name.clone())
                .or_default()
                .extend(x.like.iter().cloned());
        }
    }
    Ok(())
}

fn roots_of(a: &Ast, out: &mut Vec<String>) {
    match a {
        Ast::Name(n) => out.push(n.clone()),
        Ast::Field(x, _)
        | Ast::Call(_, x)
        | Ast::Old(x)
        | Ast::Not(x)
        | Ast::Neg(x)
        | Ast::Matches(x, _) => roots_of(x, out),
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
    /// The call was not made: earlier calls did not return ([`MAX_HUNG`]).
    NotRun,
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
    /// Why each undecided clause could not be judged, by index.
    pub why: Vec<(usize, String)>,
    /// The clauses `a || ... || z` (by index) whose last part decided the call: every part
    /// before it was false. A clause that no call reached held without checking its point.
    pub reached: Vec<usize>,
}

/// The parts of a clause's text `a || b || c`, split at the `||` outside parentheses and
/// strings.
pub fn or_parts(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let (mut depth, mut in_str, mut from) = (0i32, false, 0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if in_str => i += 1,
            b'"' => in_str = !in_str,
            b'(' | b'[' if !in_str => depth += 1,
            b')' | b']' if !in_str => depth -= 1,
            b'|' if !in_str && depth == 0 && b.get(i + 1) == Some(&b'|') => {
                out.push(text[from..i].trim());
                from = i + 2;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(text[from..].trim());
    out
}

/// Whether a clause can hold without checking its point: `a || ... || z` (it holds through a
/// part before `z`), or `result` of a property that returns `Option<bool>` (it holds on `None`).
fn can_skip(f: &Fn, c: &lang::SrcClause) -> bool {
    disjuncts(&c.ast).len() > 1
        || (c.ast == Ast::Name("result".into()) && f.ret == FTy::Opt(Box::new(FTy::Bool)))
}

/// The parts of a clause `a || b || c`, in order (one part for any other clause).
pub fn disjuncts(a: &Ast) -> Vec<&Ast> {
    match a {
        Ast::Bin(crate::plan::BinOp::Or, x, y) => {
            let mut v = disjuncts(x);
            v.extend(disjuncts(y));
            v
        }
        o => vec![o],
    }
}

/// The calls after which a function's inputs stop: calls that do not return each wait for the
/// timeout, and the first three show the failure (Milestone 282).
pub const MAX_HUNG: u32 = 3;

/// The longest shrink of one failure; it then keeps the smallest input so far.
pub const SHRINK_BUDGET: Duration = Duration::from_secs(60);

impl Fn {
    pub(crate) fn module(&self, k: usize) -> String {
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
                    inner: p.inner,
                })
                .collect(),
            ret: self.ret.clone(),
            ret_ref: self.ret_ref,
            is_unsafe: self.is_unsafe,
            forbidden: self.forbidden.clone(),
        }
    }

    /// The values at entry. A value that a `build` line builds is its constructor's arguments,
    /// and with `pre` (the harness's answer) also what its `read` line gave before the call:
    /// the parameter itself, and since Milestone 318 each such element, field or variant's
    /// field inside it.
    fn entry_env(&self, args: &[Val], pre: Option<&[Val]>) -> Vec<(String, Val)> {
        let mut pre = pre.map(|p| p.iter());
        self.params
            .iter()
            .zip(args)
            .map(|(p, v)| {
                let read = if harness::is_read_built(&p.ty) {
                    pre.as_mut().and_then(|it| it.next())
                } else {
                    None
                };
                (p.name.clone(), with_built(v, &p.ty, read))
            })
            .collect()
    }

    /// How many values after the call the harness answers before the read values at entry:
    /// one for each `&mut` parameter.
    fn n_after(&self) -> usize {
        self.params
            .iter()
            .filter(|p| matches!(p.pass, Pass::RefMut { .. }))
            .count()
    }

    /// The answer of a call that returned, from the harness's words after `R` (Milestone 307:
    /// the read values at entry of the built parameters follow the values after the call).
    pub(crate) fn returned<'w>(
        &self,
        w: &mut impl Iterator<Item = &'w str>,
    ) -> Result<Outcome, String> {
        let result = value::decode(&self.ret, w)?;
        let mut after = Vec::new();
        for p in &self.params {
            if matches!(p.pass, Pass::RefMut { .. }) {
                after.push(value::decode(&p.ty, w)?);
            }
        }
        for p in &self.params {
            if harness::is_read_built(&p.ty) {
                after.push(value::decode(&p.ty, w)?);
            }
        }
        Ok(Outcome::Returned { result, after })
    }

    /// The first `requires` clause (by index) that the input does not keep, an undecided one
    /// included.
    pub fn unmet(&self, args: &[Val]) -> Option<usize> {
        let e = self.entry_env(args, None);
        let env = check::Env {
            entry: &e,
            after: &e,
            result: None,
        };
        self.contract
            .clauses
            .iter()
            .position(|c| c.kind == ClauseKind::Requires && check::holds(&c.ast, &env) != Ok(true))
    }

    /// Whether the input keeps every `requires` clause (an undecided one does not).
    pub fn admits(&self, args: &[Val]) -> bool {
        self.unmet(args).is_none()
    }

    /// The values after the call: each `&mut` parameter's from the answer, the others' from
    /// the input.
    fn after_env(&self, entry: &[(String, Val)], after: &[Val]) -> Vec<(String, Val)> {
        let mut it = after.iter();
        self.params
            .iter()
            .zip(entry)
            .map(|(p, (_, v))| {
                let v = if matches!(p.pass, Pass::RefMut { .. }) {
                    let now = it.next().cloned().unwrap_or(Val::Opaque);
                    match v {
                        // a built value keeps its constructor's arguments
                        Val::Built(_, a) => Val::Built(Some(Box::new(now)), a.clone()),
                        _ => now,
                    }
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
            why: vec![],
            reached: vec![],
        };
        match o {
            Outcome::Returned { result, after } => {
                let n = self.n_after().min(after.len());
                let e = self.entry_env(args, Some(&after[n..]));
                let a = self.after_env(&e, &after[..n]);
                let env = check::Env {
                    entry: &e,
                    after: &a,
                    result: Some(result),
                };
                for (i, c) in self.contract.clauses.iter().enumerate() {
                    if c.kind != ClauseKind::Ensures {
                        continue;
                    }
                    // a property that returns `Option<bool>`: `None` is an input it cannot use
                    if let Ok(Val::Opt(o)) = check::value(&c.ast, &env) {
                        match o.as_deref() {
                            None => {}
                            Some(Val::Bool(b)) => {
                                j.reached.push(i);
                                if !b {
                                    j.failures.push(Failure::Clause(i));
                                }
                            }
                            _ => {
                                j.undecided.push(i);
                                j.why
                                    .push((i, "the property's `Some` holds no boolean".into()));
                            }
                        }
                        continue;
                    }
                    match check::holds(&c.ast, &env) {
                        Ok(v) => {
                            if !v {
                                j.failures.push(Failure::Clause(i));
                            }
                            let parts = disjuncts(&c.ast);
                            if parts.len() > 1
                                && parts[..parts.len() - 1]
                                    .iter()
                                    .all(|p| check::holds(p, &env) == Ok(false))
                            {
                                j.reached.push(i);
                            }
                        }
                        Err(e) => {
                            j.undecided.push(i);
                            j.why.push((i, e));
                        }
                    }
                }
            }
            Outcome::Panicked { location, .. } => j.failures.push(Failure::Panic(location.clone())),
            Outcome::Ended => j.failures.push(Failure::Ended),
            Outcome::Hung => j.failures.push(Failure::Hung),
            Outcome::NotRun => {}
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
            Outcome::NotRun => "the call was not made".into(),
        }
    }

    /// Generate the `k`-th input: the first is the smallest values, the second the largest,
    /// the others random.
    pub(crate) fn generate(&self, rng: &mut Rng, k: usize) -> Vec<Val> {
        let mut g = value::Gen {
            rng,
            hints: &self.hints,
            mode: match k {
                0 => 0,
                1 => 1,
                _ => 2,
            },
            max_len: value::max_len_for(k),
            budget: value::BUDGET,
        };
        let mut args: Vec<Val> = self.params.iter().map(|p| g.val(&p.ty, &p.name)).collect();
        if k >= 2 {
            // relate the paths that a clause compares: sometimes equal, or one apart; a value
            // and a list's elements: the value put into the list
            for (a, b) in &self.hints.pairs {
                if g.rng.below(4) != 0 {
                    continue;
                }
                if self.plant(&mut args, a, b, g.rng) {
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
        // a list that `requires sorted(v)` names is generated in order (Milestone 307)
        for p in &self.hints.sorted {
            let Some((i, rest)) = self.split_path(p) else {
                continue;
            };
            if let Some(Val::List(l)) = value::at_mut(&mut args[i], &rest) {
                value::sort(l);
            }
        }
        args
    }

    /// For a pair of a list's elements (`v[]`) and a value (`x`), put the value at a random
    /// place of the list, as a search for it needs. Whether the pair is such a pair.
    fn plant(&self, args: &mut [Val], a: &str, b: &str, rng: &mut Rng) -> bool {
        let (lp, sp) = if a.ends_with("[]") && !b.contains('[') {
            (a, b)
        } else if b.ends_with("[]") && !a.contains('[') {
            (b, a)
        } else {
            return false;
        };
        let base = &lp[..lp.len() - 2];
        let (Some((il, pl)), Some((is, ps))) = (self.split_path(base), self.split_path(sp)) else {
            return true;
        };
        let Some(v) = value::at(&args[is], &ps).cloned() else {
            return true;
        };
        let et = match value::ty_at(&self.params[il].ty, &pl) {
            Some(FTy::List(e) | FTy::Array(e, _)) => e,
            _ => return true,
        };
        let fits = match (&**et, &v) {
            (FTy::Int { signed, bits, .. }, Val::Int(x)) => {
                let (lo, hi) = value::int_range(*signed, *bits);
                let (lo, hi) = match self.hints.domains.get(lp) {
                    Some(&(p, q)) => (p.max(lo), q.min(hi)),
                    None => (lo, hi),
                };
                (lo..=hi).contains(x)
            }
            (FTy::Str, Val::Str(_)) | (FTy::Bool, Val::Bool(_)) => true,
            _ => false,
        };
        if !fits {
            return true;
        }
        if let Some(Val::List(l)) = value::at_mut(&mut args[il], &pl) {
            if !l.is_empty() {
                let at = rng.below(l.len() as u64) as usize;
                l[at] = v;
            }
        }
        true
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
    pub(crate) fn candidates(&self, args: &[Val]) -> Vec<Vec<Val>> {
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

    /// The function's name without the package (`Version::parse`), as `--fn` takes it.
    pub fn short_name(&self) -> &str {
        self.name.split_once("::").map_or(&self.name, |x| x.1)
    }
}

/// How often a long run prints a progress line on standard error.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(5);

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
    /// Calls made so far, the shrink's included.
    pub calls: u64,
    /// Calls that did not return so far.
    pub hung: u32,
    /// Stop making calls after [`MAX_HUNG`] calls that did not return.
    pub hung_cap: bool,
    /// What the progress line says the run does (`calls of 1000`, `shrinking`).
    pub phase: String,
    pub started: std::time::Instant,
    pub last_print: std::time::Instant,
}

impl<'a> Runner<'a> {
    pub fn new(
        f: &'a Fn,
        exe: PathBuf,
        filter: String,
        work: PathBuf,
        timeout: Duration,
        copy: PathBuf,
    ) -> Runner<'a> {
        let now = std::time::Instant::now();
        Runner {
            f,
            exe,
            filter,
            work,
            timeout,
            copy,
            runs: 0,
            calls: 0,
            hung: 0,
            hung_cap: false,
            phase: String::new(),
            started: now,
            last_print: now,
        }
    }

    /// Run inputs and return each one's outcome. A process that ends or hangs during a call
    /// gives that call `Ended` or `Hung`, and the calls after it run in a new process.
    pub fn run(&mut self, inputs: &[Vec<Val>]) -> Result<Vec<Outcome>, String> {
        let mut outs: Vec<Option<Outcome>> = vec![None; inputs.len()];
        let mut from = 0;
        while from < inputs.len() {
            if self.hung_cap && self.hung >= MAX_HUNG {
                for o in outs.iter_mut().skip(from) {
                    *o = Some(Outcome::NotRun);
                }
                break;
            }
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
            let (exe, filter, timeout) = (self.exe.clone(), self.filter.clone(), self.timeout);
            let before = self.calls;
            let name = self.f.name.clone();
            let (phase, started, last) = (&self.phase, self.started, &mut self.last_print);
            let mut tick = |done: u64| {
                if last.elapsed() >= PROGRESS_EVERY {
                    *last = std::time::Instant::now();
                    eprintln!(
                        "hydo: fuzz: {name}: {phase}: {} calls, {} s",
                        before + done,
                        started.elapsed().as_secs()
                    );
                }
            };
            // Milestone 296: each call works in an empty folder of its own, not in the current
            // folder, so that a function that writes files leaves nothing behind
            let dirs = self.work.join("calls");
            let _ = std::fs::remove_dir_all(&dirs);
            std::fs::create_dir_all(&dirs).map_err(|e| format!("{}: {e}", dirs.display()))?;
            let dirs = dirs.canonicalize().unwrap_or(dirs);
            let env = [("HYDO_FUZZ_DIRS".to_string(), dirs.display().to_string())];
            let ran = cargo::run(&exe, &filter, &cases, &answers, timeout, &env, &mut tick);
            let _ = std::fs::remove_dir_all(&dirs);
            let ran = ran?;
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
            self.calls += (next - from) as u64;
            if next < inputs.len() {
                // the call at `next` ended the process or did not return
                outs[next] = Some(if ran.hung {
                    self.hung += 1;
                    Outcome::Hung
                } else {
                    Outcome::Ended
                });
                self.calls += 1;
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
            Some("R") => self.f.returned(&mut w),
            Some("P") => {
                let mut s = || match value::decode(&FTy::Str, &mut w) {
                    Ok(Val::Str(s)) => s,
                    _ => String::new(),
                };
                let mut location = s();
                // the copy's path, also as the current folder sees it, is left out
                let cwd = std::env::current_dir().unwrap_or_default();
                for base in [
                    self.copy.clone(),
                    self.copy.canonicalize().unwrap_or_default(),
                    self.copy
                        .strip_prefix(&cwd)
                        .map(Path::to_path_buf)
                        .unwrap_or_default(),
                ] {
                    if base.as_os_str().is_empty() {
                        continue;
                    }
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
    /// For a clause `a || ... || z`: the calls on which every part before `z` was false, so
    /// that `z` decided (Milestone 282).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reached: Option<u64>,
    /// The clause held, but its last part decided fewer calls than `--min-reach`.
    #[serde(default)]
    pub vacuous: bool,
    /// The clause was decided on fewer calls than `--min-reach`: hydo could not read a value
    /// that it compares (Milestone 289).
    #[serde(default)]
    pub not_judged: bool,
    /// Why the first undecided call could not be judged.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub undecided_why: String,
    /// For a clause whose last part decided too few calls: the calls that panicked, ended the
    /// process or did not return, which could not be judged and may be the ones that reach it.
    /// The function's verdict is then the failure, and the clause is not called vacuous.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failed_calls: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A `requires` clause and the generated inputs it turned away.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rejection {
    pub text: String,
    pub line: u32,
    pub inputs: u64,
}

/// What fuzzing one function gave.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FnReport {
    pub function: String,
    pub file: String,
    pub line: u32,
    pub seed: u64,
    /// The inputs that kept `requires`.
    pub cases: u64,
    /// Generated inputs that broke `requires` and were not run.
    pub rejected: u64,
    pub broken: Vec<Finding>,
    pub held: Vec<Held>,
    /// Runs of the test program, the shrink's included.
    pub runs: u64,
    /// `broken`, `held`, `vacuous` (a clause was hardly checked) or `too few inputs`
    /// (Milestone 282).
    #[serde(default)]
    pub verdict: String,
    /// The calls made of the cases (the others were not made after calls that did not return).
    #[serde(default)]
    pub calls: u64,
    /// The rejected inputs by the first `requires` clause they broke, most first.
    #[serde(default)]
    pub rejected_by: Vec<Rejection>,
    /// What to change in the contract, for a verdict `vacuous` or `too few inputs`.
    #[serde(default)]
    pub advice: Vec<String>,
    /// Some of the inputs that were run, spread over the run (`--show-inputs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<String>,
}

/// When a run tested too little to count (Milestone 282).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Fewer calls than this give the verdict `too few inputs`.
    pub min_inputs: u64,
    /// A clause `a || ... || z` whose last part decided fewer calls than this is `vacuous`.
    pub min_reach: u64,
    /// The generated inputs to keep in the report as samples (`--show-inputs`).
    pub show_inputs: usize,
}

/// Fuzz one function: `cases` inputs from `seed`, then a shrink of the first input of each
/// failure.
pub fn fuzz(r: &mut Runner, seed: u64, cases: u64, lim: Limits) -> Result<FnReport, String> {
    let f = r.f;
    let mut rng = Rng::new(seed ^ fnv(&f.name));
    let (mut inputs, mut rejected) = (Vec::new(), 0u64);
    let mut by_clause: BTreeMap<usize, u64> = BTreeMap::new();
    let mut k = 0;
    while (inputs.len() as u64) < cases && k < (cases as usize).saturating_mul(50).max(100) {
        let a = f.generate(&mut rng, k);
        k += 1;
        match f.unmet(&a) {
            None => inputs.push(a),
            Some(ci) => {
                rejected += 1;
                *by_clause.entry(ci).or_default() += 1;
            }
        }
    }
    r.phase = format!("{} inputs", inputs.len());
    r.hung_cap = true;
    let outs = r.run(&inputs)?;
    r.hung_cap = false;
    let mut by_failure: BTreeMap<Failure, (u64, usize)> = BTreeMap::new();
    let ens: Vec<usize> = f
        .contract
        .clauses
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind == ClauseKind::Ensures)
        .map(|(i, _)| i)
        .collect();
    let mut held: Vec<Held> = ens
        .iter()
        .map(|&i| {
            let c = &f.contract.clauses[i];
            Held {
                text: c.text.clone(),
                line: c.line,
                reached: can_skip(f, c).then_some(0),
                ..Held::default()
            }
        })
        .collect();
    let (mut calls, mut failed_calls) = (0u64, 0u64);
    for (i, (a, o)) in inputs.iter().zip(&outs).enumerate() {
        if *o != Outcome::NotRun {
            calls += 1;
        }
        if matches!(o, Outcome::Panicked { .. } | Outcome::Ended | Outcome::Hung) {
            failed_calls += 1;
        }
        let j = f.judge(a, o);
        for x in &j.failures {
            let e = by_failure.entry(x.clone()).or_insert((0, i));
            e.0 += 1;
        }
        if matches!(o, Outcome::Returned { .. }) {
            for (h, ci) in held.iter_mut().zip(&ens) {
                if j.undecided.contains(ci) {
                    h.undecided += 1;
                    if h.undecided_why.is_empty() {
                        if let Some((_, w)) = j.why.iter().find(|(k, _)| k == ci) {
                            h.undecided_why = w.clone();
                        }
                    }
                } else {
                    h.cases += 1;
                }
                if j.reached.contains(ci) {
                    if let Some(n) = h.reached.as_mut() {
                        *n += 1;
                    }
                }
            }
        }
    }
    let mut broken = Vec::new();
    for (x, (n, first)) in &by_failure {
        let start = inputs[*first].clone();
        r.phase = format!("shrinking {}", describe(f, x));
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
    for h in &mut held {
        // a clause that hydo could hardly decide is not judged, whatever its parts did
        h.not_judged = calls > 0 && h.cases < lim.min_reach;
        h.vacuous = !h.not_judged && h.reached.is_some_and(|n| n < lim.min_reach);
        // calls that failed may be the ones that reach the last part: the failure is the
        // verdict, and the inputs need no other shape
        if h.vacuous && failed_calls > 0 && h.reached.unwrap_or(0) + failed_calls >= lim.min_reach {
            h.vacuous = false;
            h.failed_calls = failed_calls;
        }
    }
    let mut rejected_by: Vec<Rejection> = by_clause
        .iter()
        .map(|(&ci, &n)| {
            let c = &f.contract.clauses[ci];
            Rejection {
                text: c.text.clone(),
                line: c.line,
                inputs: n,
            }
        })
        .collect();
    rejected_by.sort_by(|a, b| b.inputs.cmp(&a.inputs).then(a.line.cmp(&b.line)));
    let verdict = if !broken.is_empty() {
        "broken"
    } else if calls < lim.min_inputs {
        "too few inputs"
    } else if held.iter().any(|h| h.not_judged) {
        "not judged"
    } else if held.iter().any(|h| h.vacuous) {
        "vacuous"
    } else {
        "held"
    };
    let mut rep = FnReport {
        function: f.name.clone(),
        file: f.file.to_string_lossy().into(),
        line: f.line,
        seed,
        cases: inputs.len() as u64,
        rejected,
        broken,
        held,
        runs: r.runs,
        verdict: verdict.into(),
        calls,
        rejected_by,
        advice: vec![],
        samples: samples(f, &inputs, lim.show_inputs),
    };
    rep.advice = advice(f, &rep, lim);
    Ok(rep)
}

/// `n` of the inputs, spread evenly from the first to the last, as `name = value, ...`.
fn samples(f: &Fn, inputs: &[Vec<Val>], n: usize) -> Vec<String> {
    let n = n.min(inputs.len());
    (0..n)
        .map(|i| {
            let k = if n == 1 {
                0
            } else {
                i * (inputs.len() - 1) / (n - 1)
            };
            f.show_args(&inputs[k])
        })
        .collect()
}

/// What to change in a contract whose run tested too little: an `input` line that keeps the
/// `requires` clause that turned most inputs away, and for a clause that was hardly checked,
/// inputs of the shape the function needs.
fn advice(f: &Fn, rep: &FnReport, lim: Limits) -> Vec<String> {
    let mut out = Vec::new();
    if rep.calls < lim.min_inputs {
        if let Some(top) = rep.rejected_by.first() {
            let mut lines = input_lines_for(f, top.line);
            for c in f.contract.clauses.iter().filter(|c| c.line == top.line) {
                string_eqs(&c.ast, &mut lines);
            }
            if lines.is_empty() {
                out.push(format!(
                    "add `input` lines that keep `requires {}` (line {}), so that the generated \
                     inputs need not be thrown away: `input <path> in <lo>..=<hi>` for an \
                     integer, `input <path> len <lo>..=<hi>` for a length",
                    top.text, top.line
                ));
            } else {
                out.push(format!(
                    "add an input line that keeps `requires {}` (line {}): {}",
                    top.text,
                    top.line,
                    lines
                        .iter()
                        .map(|l| format!("`// hydo: {l}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        } else if rep.calls < rep.cases {
            out.push(format!(
                "calls did not return within the timeout, and the calls after the first {MAX_HUNG} \
                 were not made: narrow the sizes with `input` lines, or raise --timeout"
            ));
        }
    }
    for h in rep.held.iter().filter(|h| h.not_judged) {
        let why = if h.undecided_why.is_empty() {
            "a value it reads is of a type that hydo does not read"
        } else {
            h.undecided_why.as_str()
        };
        out.push(format!(
            "`ensures {}` (line {}) could not be judged on {} of {} calls ({why}): compare a \
             part that hydo reads, such as an integer, a floating-point number, a string or a \
             list (`--input` shows a value it does not read as `<not read>`)",
            h.text,
            h.line,
            h.undecided,
            h.cases + h.undecided,
        ));
    }
    if rep.held.iter().any(|h| h.vacuous) {
        let strings: Vec<&str> = f
            .params
            .iter()
            .filter(|p| p.ty == FTy::Str && !f.hints.like.contains_key(&p.name))
            .map(|p| p.name.as_str())
            .collect();
        let mut s = "give the inputs the shape that reaches the clause's last part".to_string();
        if let Some(n) = strings.first() {
            s.push_str(&format!(
                ": for a string the function parses, `// hydo: input {n} like \"<a text it \
                 accepts>\"`, or `// hydo: input {n} len 0..=64 chars \"<its characters>\"`"
            ));
        } else if let Some(p) = f.params.iter().find(|p| {
            matches!(&p.ty, FTy::List(e) if matches!(**e, FTy::Int { signed: false, bits: 8, .. }))
                && !f.hints.like_bytes.contains_key(&p.name)
        }) {
            s.push_str(&format!(
                ": for bytes the function parses, `// hydo: input {} like b\"<bytes it \
                 accepts>\"`",
                p.name
            ));
        } else {
            s.push_str(": `input` lines with ranges, lengths and characters");
        }
        out.push(s);
    }
    out
}

/// `input <path> like "<text>"` for each comparison of a string with a literal in a clause.
fn string_eqs(a: &Ast, out: &mut Vec<String>) {
    use crate::plan::BinOp;
    match a {
        Ast::Bin(BinOp::And | BinOp::Or, x, y) => {
            string_eqs(x, out);
            string_eqs(y, out);
        }
        Ast::Bin(BinOp::Eq, x, y) => {
            let (p, s) = match (check::path_of(x), &**y, check::path_of(y), &**x) {
                (Some(p), Ast::Str(s), _, _) | (_, _, Some(p), Ast::Str(s)) => (p, s),
                _ => return,
            };
            if !p.contains('#') && !p.starts_with("result") {
                out.push(format!("input {p} like {s:?}"));
            }
        }
        _ => {}
    }
}

/// `input` lines that keep the `requires` clause on `line`, when its comparisons of a path
/// with a number give both of the path's bounds (`len(s) == 64`, `n >= 1 && n <= 8`).
fn input_lines_for(f: &Fn, line: u32) -> Vec<String> {
    use crate::plan::BinOp;
    let mut bounds: BTreeMap<String, (Option<i128>, Option<i128>)> = BTreeMap::new();
    let mut named: Vec<String> = Vec::new();
    fn walk(
        a: &Ast,
        bounds: &mut BTreeMap<String, (Option<i128>, Option<i128>)>,
        named: Option<&mut Vec<String>>,
    ) {
        match a {
            Ast::Bin(BinOp::And, x, y) => {
                let mut n2 = Vec::new();
                walk(x, bounds, Some(&mut n2));
                walk(y, bounds, Some(&mut n2));
                if let Some(n) = named {
                    n.extend(n2);
                }
            }
            Ast::Bin(op, x, y) => {
                let (p, k, op) = match (check::path_of(x), &**y, check::path_of(y), &**x) {
                    (Some(p), Ast::Int(k), _, _) => (p, *k, *op),
                    (_, _, Some(p), Ast::Int(k)) => (
                        p,
                        *k,
                        match op {
                            BinOp::Lt => BinOp::Gt,
                            BinOp::Le => BinOp::Ge,
                            BinOp::Gt => BinOp::Lt,
                            BinOp::Ge => BinOp::Le,
                            o => *o,
                        },
                    ),
                    _ => return,
                };
                let b = bounds.entry(p.clone()).or_default();
                match op {
                    BinOp::Eq => *b = (Some(k), Some(k)),
                    BinOp::Le => b.1 = Some(k),
                    BinOp::Lt => b.1 = Some(k - 1),
                    BinOp::Ge => b.0 = Some(k),
                    BinOp::Gt => b.0 = Some(k + 1),
                    _ => return,
                }
                if let Some(n) = named {
                    n.push(p);
                }
            }
            _ => {}
        }
    }
    for c in &f.contract.clauses {
        if c.kind == ClauseKind::Requires {
            walk(&c.ast, &mut bounds, (c.line == line).then_some(&mut named));
        }
    }
    named.sort();
    named.dedup();
    let mut out = Vec::new();
    for p in named {
        let (lo, hi) = bounds.get(&p).copied().unwrap_or_default();
        let (base, len) = match p.strip_suffix("#len") {
            Some(b) => (b.to_string(), true),
            None => (p.clone(), false),
        };
        if base == "result" || base.starts_with("result.") {
            continue;
        }
        let lo = if len { Some(lo.unwrap_or(0)) } else { lo };
        let (Some(lo), Some(hi)) = (lo, hi) else {
            continue;
        };
        if lo > hi {
            continue;
        }
        let range = if lo == hi {
            format!("{lo}")
        } else {
            format!("{lo}..={hi}")
        };
        out.push(if len {
            format!("input {base} len {range}")
        } else {
            format!("input {base} in {lo}..={hi}")
        });
    }
    out
}

/// Shrink an input that fails with `x`: take the first simpler input that fails the same way,
/// until none does. A call that does not return waits for the timeout, so a shrink stops after
/// [`MAX_HUNG`] such calls or [`SHRINK_BUDGET`], with the smallest input so far.
fn shrink(r: &mut Runner, start: &[Val], x: &Failure) -> Result<(Vec<Val>, u64), String> {
    let f = r.f;
    let mut cur = start.to_vec();
    let mut steps = 0;
    let began = std::time::Instant::now();
    let hung_before = r.hung;
    let rounds = if *x == Failure::Hung { 3 } else { 300 };
    // the candidates run in chunks, and a round ends at the first chunk with a failing one
    let chunk = if *x == Failure::Hung { 2 } else { 64 };
    'rounds: for _ in 0..rounds {
        let mut cands = f.candidates(&cur);
        cands.truncate(if *x == Failure::Hung { 4 } else { 4096 });
        for part in cands.chunks(chunk) {
            if began.elapsed() > SHRINK_BUDGET || r.hung >= hung_before + MAX_HUNG {
                break 'rounds;
            }
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
    min_inputs: Option<u64>,
    min_reach: Option<u64>,
    allow_skipped: bool,
    show_inputs: usize,
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
        min_inputs: None,
        min_reach: None,
        allow_skipped: false,
        show_inputs: 0,
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
            "--min-inputs" => {
                o.min_inputs = Some(val("--min-inputs")?.parse().map_err(|_| "--min-inputs N")?)
            }
            "--min-reach" => {
                o.min_reach = Some(val("--min-reach")?.parse().map_err(|_| "--min-reach N")?)
            }
            "--show-inputs" => {
                o.show_inputs = val("--show-inputs")?
                    .parse()
                    .map_err(|_| "--show-inputs N")?
            }
            "--allow-skipped" => o.allow_skipped = true,
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

impl Opts {
    fn limits(&self) -> Limits {
        Limits {
            min_inputs: self.min_inputs.unwrap_or(self.cases.div_ceil(10)),
            min_reach: self.min_reach.unwrap_or(self.cases.div_ceil(100).max(1)),
            show_inputs: self.show_inputs,
        }
    }

    /// The options that a replay needs to build and call the function as this run did.
    fn replay_flags(&self) -> String {
        let mut s = String::new();
        if self.release {
            s.push_str(" --release");
        }
        if let Some(t) = &self.toolchain {
            s.push_str(&format!(" --toolchain {}", shell_quote(t)));
        }
        if let Some(t) = &self.target_dir {
            s.push_str(&format!(
                " --target-dir {}",
                shell_quote(&t.to_string_lossy())
            ));
        }
        if self.timeout != 10 {
            s.push_str(&format!(" --timeout {}", self.timeout));
        }
        s
    }
}

/// Whether the function is the one a `--fn` names: by its name, or by the end of its full name.
pub(crate) fn named(f: &Fn, want: &[String]) -> bool {
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

/// Where each harness module stands in the copy: its function's index, the file, the line and
/// the first and last columns (1-based characters, as rustc counts).
struct Placed {
    k: usize,
    file: PathBuf,
    line: u32,
    cols: (u32, u32),
}

/// The contracted functions that the run builds harnesses for, and the others of the same
/// target, which the copy keeps in use (Milestone 289).
pub(crate) struct Set<'a> {
    pub(crate) fns: &'a [Fn],
    pub(crate) others: &'a [Fn],
}

impl Set<'_> {
    /// The function of index `k`: `fns`, then `others`.
    fn get(&self, k: usize) -> &Fn {
        if k < self.fns.len() {
            &self.fns[k]
        } else {
            &self.others[k - self.fns.len()]
        }
    }
}

/// Write the copy's sources with the harness modules of the functions `active` (indices into
/// `set.fns`) and a use of each function `keeps` (indices into `set.fns` and then `set.others`,
/// as `fns.len() + index`): every file that holds a function starts again from the project's
/// own text. The uses keep a property function that only hydo calls from being dead code in a
/// crate that denies warnings, when the run does not build its harness (Milestone 289).
fn place(
    pkg_dir: &Path,
    copy: &Path,
    set: &Set,
    active: &[usize],
    keeps: &[usize],
) -> Result<Vec<Placed>, String> {
    let fns = set.fns;
    let mut by_file: BTreeMap<&Path, Vec<(usize, usize, String)>> = BTreeMap::new();
    for f in fns.iter().chain(set.others) {
        by_file.entry(f.file.as_path()).or_default();
    }
    for &k in active {
        let f = &fns[k];
        by_file.entry(f.file.as_path()).or_default().push((
            f.insert_at,
            k,
            harness::module(&f.spec(k)),
        ));
    }
    for &k in keeps.iter().filter(|k| !active.contains(k)) {
        let f = set.get(k);
        by_file.entry(f.file.as_path()).or_default().push((
            f.insert_at,
            k,
            harness::keep(&f.spec(k)),
        ));
    }
    let mut placed = Vec::new();
    for (file, mut ins) in by_file {
        let src = pkg_dir.join(file);
        let orig = std::fs::read_to_string(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        ins.sort_by_key(|x| x.0);
        let mut text = String::with_capacity(orig.len());
        let mut from = 0;
        for (at, k, m) in ins {
            text.push_str(&orig[from..at]);
            let start = text.len();
            let line = text.matches('\n').count() as u32 + 1;
            let line_start = text.rfind('\n').map_or(0, |i| i + 1);
            let col = text[line_start..start].chars().count() as u32 + 1;
            text.push_str(&m);
            placed.push(Placed {
                k,
                file: file.to_path_buf(),
                line,
                cols: (col, col + m.chars().count() as u32),
            });
            from = at;
        }
        text.push_str(&orig[from..]);
        let p = copy.join(file);
        std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(placed)
}

/// The functions whose harness module an error names, with the error's first line.
fn blame(errors: &[cargo::Diag], placed: &[Placed], copy: &Path) -> BTreeMap<usize, String> {
    let mut out = BTreeMap::new();
    for e in errors {
        for (file, line, col) in &e.spans {
            let p = Path::new(file);
            let rel = p.strip_prefix(copy).unwrap_or(p);
            for pl in placed {
                if pl.file == rel && pl.line == *line && (pl.cols.0..=pl.cols.1).contains(col) {
                    out.entry(pl.k).or_insert_with(|| e.message.clone());
                }
            }
        }
    }
    out
}

/// The test program (none when no harness built), the functions in it, and the functions left
/// out with the compiler's first error.
pub(crate) type Built = (Option<PathBuf>, Vec<usize>, Vec<(usize, String)>);

/// Build the copy with only the uses `keeps`, leaving out each use that an error names. The
/// uses that stay, or the last build's failure.
#[allow(clippy::too_many_arguments)]
fn build_bare(
    cg: &cargo::Cargo,
    pkg_dir: &Path,
    copy: &Path,
    target_dir: &Path,
    target: &cargo::Target,
    log: &Path,
    set: &Set,
    mut keeps: Vec<usize>,
) -> Result<Vec<usize>, cargo::BuildFail> {
    loop {
        let placed = place(pkg_dir, copy, set, &[], &keeps).map_err(|e| cargo::BuildFail {
            errors: vec![],
            text: e,
        })?;
        match cargo::build(cg, copy, target_dir, target, log) {
            Ok(_) => return Ok(keeps),
            Err(e) => {
                let blamed = blame(&e.errors, &placed, copy);
                if blamed.is_empty() || keeps.is_empty() {
                    return Err(e);
                }
                keeps.retain(|k| !blamed.contains_key(k));
            }
        }
    }
}

/// Build the harness of every function that builds. A function whose harness module does not
/// compile is left out with the compiler's message, and the others are built again without it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_each(
    cg: &cargo::Cargo,
    pkg: &cargo::Pkg,
    copy: &Path,
    target_dir: &Path,
    target: &cargo::Target,
    log: &Path,
    set: &Set,
) -> Result<Built, String> {
    let fns = set.fns;
    let all = fns.len() + set.others.len();
    let mut active: Vec<usize> = (0..fns.len()).collect();
    // every function the run does not build a harness for is named by a use
    let mut keeps: Vec<usize> = (0..all).collect();
    let mut failed: Vec<(usize, String)> = Vec::new();
    let mut alone_tried = false;
    loop {
        let placed = place(&pkg.dir, copy, set, &active, &keeps)?;
        let err = match cargo::build(cg, copy, target_dir, target, log) {
            Ok(exe) => return Ok((Some(exe), active, failed)),
            Err(e) => e,
        };
        let blamed = blame(&err.errors, &placed, copy);
        if !blamed.is_empty() {
            for (k, m) in blamed {
                if !active.contains(&k) {
                    // a use of a function whose harness is not built: left out
                    keeps.retain(|&x| x != k);
                    continue;
                }
                active.retain(|&x| x != k);
                failed.push((k, m));
            }
            if active.is_empty() {
                return Ok((None, active, failed));
            }
            continue;
        }
        // an error outside the harness modules: the crate itself, or a function's harness that
        // breaks code elsewhere; the crate without any harness tells them apart
        if let Err(e) = build_bare(
            cg,
            &pkg.dir,
            copy,
            target_dir,
            target,
            log,
            set,
            keeps.clone(),
        ) {
            return Err(bare_failure(
                cg, pkg, copy, target_dir, target, log, set, &keeps, &e,
            ));
        }
        if alone_tried || active.len() <= 1 {
            if let Some(&k) = active.first() {
                failed.push((k, err.first_error()));
            }
            return Ok((None, vec![], failed));
        }
        alone_tried = true;
        let mut ok = Vec::new();
        for &k in &active {
            place(&pkg.dir, copy, set, &[k], &keeps)?;
            match cargo::build(cg, copy, target_dir, target, log) {
                Ok(_) => ok.push(k),
                Err(e) => failed.push((k, e.first_error())),
            }
        }
        if ok.is_empty() {
            return Ok((None, ok, failed));
        }
        active = ok;
    }
}

/// Why the copy without any harness does not build (Milestone 289): the crate, when the crate
/// with its own Cargo.toml and the same sources does not build either, else hydo's copy.
#[allow(clippy::too_many_arguments)]
fn bare_failure(
    cg: &cargo::Cargo,
    pkg: &cargo::Pkg,
    copy: &Path,
    target_dir: &Path,
    target: &cargo::Target,
    log: &Path,
    set: &Set,
    keeps: &[usize],
    e: &cargo::BuildFail,
) -> String {
    let scratch = copy.with_file_name("original");
    // the scratch copy gets the same uses as hydo's, so that only the manifest differs
    let prepare = |dir: &Path| place(&pkg.dir, dir, set, &[], keeps).map(|_| ());
    let profile = if cg.release { "release" } else { "test" };
    match cargo::original_builds(cg, pkg, target, target_dir, &scratch, &prepare) {
        Some(true) => format!(
            "hydo's copy of the crate does not build, though the crate itself builds with \
             `cargo test --no-run`: the cause is hydo's copy (its Cargo.toml, which hydo writes \
             from `cargo metadata`, or its harness), not the crate. The first error: {}. Hint: \
             a setting of the crate's Cargo.toml that the copy lost, such as a lint or a `cfg` \
             list, gives such an error; compare {} with the crate's own, and report it to \
             hydo's maintainers with cargo's output: {}",
            e.first_error(),
            copy.join("Cargo.toml").display(),
            log.display()
        ),
        Some(false) => format!(
            "the crate does not build in the {profile} profile (`cargo test --no-run` fails \
             without hydo too), so no function can be fuzzed{}: {}",
            if e.first_error().contains("is never used") {
                " (a function that only `hydo fuzz` calls is unused in the crate's own tests, \
                 and the crate denies warnings: mark it `#[allow(dead_code)]`)"
            } else {
                ""
            },
            e.text
        ),
        None => format!(
            "hydo's copy of the crate does not build without hydo's harness, so no function can \
             be fuzzed; hydo could not build the crate apart to tell whether the crate or the \
             copy is the cause: {}",
            e.text
        ),
    }
}

fn run_all(o: &Opts) -> Result<i32, String> {
    let cg = cargo::Cargo {
        toolchain: o.toolchain.clone(),
        release: o.release,
    };
    let lim = o.limits();
    let pkg = cargo::metadata(&cg, &o.dir)?;
    let sources = pkg.sources();
    let (fns, mut skipped, errors) = find_all(&pkg.dir, &sources, &pkg.name);
    // Milestone 292: a durability check runs only inside the VM (`hydo fuzz --vm`)
    let (fns, checks) = split_checks(fns, &sources);
    for e in &errors {
        eprintln!("hydo fuzz: {e}");
    }
    if !errors.is_empty() {
        return Ok(2);
    }
    let (fns, mut others): (Vec<Fn>, Vec<Fn>) = fns.into_iter().partition(|f| named(f, &o.fns));
    let checks: Vec<(String, PathBuf, u32, String)> = checks
        .into_iter()
        .map(|(c, of)| {
            let row = (c.name.clone(), c.file.clone(), c.line, of);
            others.push(c);
            row
        })
        .collect();
    let no_contract = || {
        format!(
            "no function of {} has a `// hydo:` contract: write `// hydo: ensures <expr>` (and \
             `requires`, `input`) right before a function (`hydo guide fuzz`)",
            pkg.dir.display()
        )
    };
    if o.list {
        if fns.is_empty() && skipped.is_empty() {
            println!("hydo: fuzz: {}", no_contract());
        }
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
        for (name, file, line, of) in &checks {
            println!(
                "{name} ({}:{line}): the durability check of `{of}`: `hydo fuzz --vm` calls it \
                 after the run",
                file.display()
            );
        }
        return Ok(0);
    }
    let wanted = |name: &str| {
        o.fns.is_empty()
            || o.fns
                .iter()
                .any(|w| name == w || name.ends_with(&format!("::{w}")))
    };
    // with --json, standard output holds only the JSON
    let say = |line: String| {
        if o.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    for s in &skipped {
        if wanted(&s.name) {
            say(format!(
                "hydo: fuzz: {} ({}:{}) is not fuzzed: {}",
                s.name,
                s.file.display(),
                s.line,
                s.why
            ));
        }
    }
    if fns.is_empty() {
        return Err(if !o.fns.is_empty() {
            format!("no function named {} can be fuzzed", o.fns.join(", "))
        } else if skipped.is_empty() {
            no_contract()
        } else {
            "no function of the package can be fuzzed (`hydo fuzz --list` says why)".into()
        });
    }
    if o.input.is_some() && fns.len() > 1 {
        return Err(format!(
            "--fn {} names {} functions: {}; name one by its path",
            o.fns[0],
            fns.len(),
            fns.iter()
                .map(|f| f.short_name().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
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
                say(format!(
                    "hydo: fuzz: {} is in another target of the package than {}; fuzz it with its own --fn",
                    f.name, first.name
                ));
            }
            same
        })
        .collect();
    let in_first = |f: &Fn| {
        pkg.target_of(&pkg.dir.join(&f.file))
            .is_some_and(|t| t.name == first.name && t.kind == first.kind)
    };
    let others: Vec<Fn> = others.into_iter().filter(in_first).collect();
    std::fs::write(copy.join("Cargo.toml"), cargo::manifest(&pkg, &first))
        .map_err(|e| e.to_string())?;
    let target_dir = o.target_dir.clone().unwrap_or_else(|| out.join("target"));
    eprintln!(
        "hydo: fuzz: building the harness of {} function{} ({})",
        fns.len(),
        if fns.len() == 1 { "" } else { "s" },
        copy.display()
    );
    let (exe, active, failed) = build_each(
        &cg,
        &pkg,
        &copy,
        &target_dir,
        &first,
        &out.join("build.log"),
        &Set {
            fns: &fns,
            others: &others,
        },
    )?;
    for (k, why) in &failed {
        let f = &fns[*k];
        let why = format!("its harness did not build: {why}");
        say(format!(
            "hydo: fuzz: {} ({}:{}) is not fuzzed: {why}",
            f.name,
            f.file.display(),
            f.line
        ));
        skipped.push(Skipped {
            name: f.name.clone(),
            file: f.file.clone(),
            line: f.line,
            why,
        });
    }
    let work = out.join("work");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let timeout = Duration::from_secs(o.timeout.max(1));
    let mut report = Report {
        package: pkg.name.clone(),
        functions: vec![],
        skipped: skipped
            .iter()
            .filter(|s| wanted(&s.name))
            .map(|s| format!("{} ({}:{}): {}", s.name, s.file.display(), s.line, s.why))
            .collect(),
    };
    let Some(exe) = exe else {
        write_report(&report, &out, o.json)?;
        return Err(format!(
            "no harness built (cargo's output: {})",
            out.join("build.log").display()
        ));
    };
    if let Some(inp) = &o.input {
        let Some(&k) = active.first() else {
            return Err("the function's harness did not build".into());
        };
        let f = &fns[k];
        let named: Vec<(String, FTy)> = f
            .params
            .iter()
            .map(|p| (p.name.clone(), p.ty.clone()))
            .collect();
        let args = value::parse_args(inp, &named)?;
        let mut r = Runner::new(
            f,
            exe.clone(),
            format!("{}::run", f.module(k)),
            work.clone(),
            timeout,
            copy.clone(),
        );
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
    for &k in &active {
        let f = &fns[k];
        let mut r = Runner::new(
            f,
            exe.clone(),
            format!("{}::run", f.module(k)),
            work.clone(),
            timeout,
            copy.clone(),
        );
        let rep = fuzz(&mut r, o.seed, o.cases, lim)?;
        if !o.json {
            print_report(&rep, f, o, lim);
        }
        report.functions.push(rep);
    }
    write_report(&report, &out, o.json)?;
    let count = |v: &str| report.functions.iter().filter(|f| f.verdict == v).count();
    let (broken, vacuous, few, unjudged) = (
        count("broken"),
        count("vacuous"),
        count("too few inputs"),
        count("not judged"),
    );
    let not_fuzzed = report.skipped.len();
    if !o.json {
        let mut parts = vec![format!("{} held", count("held"))];
        for (n, w) in [
            (broken, "broken"),
            (unjudged, "not judged"),
            (vacuous, "vacuous"),
            (few, "too few inputs"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {w}"));
            }
        }
        let not = if not_fuzzed == 0 {
            String::new()
        } else {
            format!("; {not_fuzzed} not fuzzed")
        };
        println!(
            "hydo: fuzz: {} function{} fuzzed: {}{not}",
            report.functions.len(),
            if report.functions.len() == 1 { "" } else { "s" },
            parts.join(", ")
        );
        if not_fuzzed > 0 {
            println!(
                "hydo: fuzz: {not_fuzzed} contracted function{} not fuzzed (each line \
                 `is not fuzzed` above says why), so the run did not test every contract{}",
                if not_fuzzed == 1 { " was" } else { "s were" },
                if o.allow_skipped {
                    "; --allow-skipped lets it pass"
                } else if broken > 0 {
                    ""
                } else {
                    ": the exit status is 3 unless --allow-skipped is given"
                }
            );
        }
    }
    Ok(exit_status(
        broken,
        vacuous + few + unjudged,
        if o.allow_skipped { 0 } else { not_fuzzed },
    ))
}

/// The exit status of a run: 1 when a clause broke or a call failed; else 3 when a function
/// tested too little or a contracted function was not fuzzed; else 0.
pub fn exit_status(broken: usize, too_little: usize, not_fuzzed: usize) -> i32 {
    if broken > 0 {
        1
    } else if too_little + not_fuzzed > 0 {
        3
    } else {
        0
    }
}

fn write_report(report: &Report, out: &Path, json: bool) -> Result<(), String> {
    let j = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
    std::fs::write(out.join("fuzz.json"), format!("{j}\n")).map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string(report).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "hydo: fuzz: the report is {}",
            out.join("fuzz.json").display()
        );
    }
    Ok(())
}

pub(crate) fn describe(f: &Fn, x: &Failure) -> String {
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

fn pct(n: u64, of: u64) -> String {
    if of == 0 {
        return "0 %".into();
    }
    let p = n as f64 * 100.0 / of as f64;
    if p > 0.0 && p < 0.1 {
        "less than 0.1 %".into()
    } else {
        format!("{p:.1} %")
    }
}

fn print_report(r: &FnReport, f: &Fn, o: &Opts, lim: Limits) {
    let generated = r.cases + r.rejected;
    println!(
        "hydo: fuzz: {} ({}:{}): {} calls from seed {}; {} of {} generated inputs broke `requires` ({})",
        r.function,
        r.file,
        r.line,
        r.calls,
        r.seed,
        r.rejected,
        generated,
        pct(r.rejected, generated)
    );
    if let Some(top) = r.rejected_by.first() {
        if r.rejected * 2 > generated {
            println!(
                "hydo:   most were turned away by `requires {}` (line {}): {} inputs",
                top.text, top.line, top.inputs
            );
        }
    }
    for (i, x) in r.samples.iter().enumerate() {
        println!("hydo:   input {} of {}: {x}", i + 1, r.samples.len());
    }
    for b in &r.broken {
        let what = match b.kind.as_str() {
            "ensures" => format!(
                "contract broken: {} ensures {} ({}:{})",
                r.function, b.text, r.file, b.line
            ),
            "panic" => format!("{} panicked at {}", r.function, b.text),
            _ => format!("{}: {}", r.function, b.text),
        };
        println!("hydo: {what} on {} of {} calls", b.cases, r.calls);
        println!(
            "hydo:   smallest input ({} shrink steps): {}",
            b.shrink_steps, b.input
        );
        println!("hydo:   the call gave: {}", b.after);
        println!(
            "hydo:   again: hydo fuzz {} --fn {} --input {}{}",
            shell_quote(&o.dir.to_string_lossy()),
            shell_quote(f.short_name()),
            shell_quote(&b.input),
            o.replay_flags()
        );
    }
    if r.calls < r.cases {
        println!(
            "hydo:   {} inputs were not run after {MAX_HUNG} calls that did not return",
            r.cases - r.calls
        );
    }
    for h in &r.held {
        let und = if h.undecided > 0 {
            format!(", undecided on {}", h.undecided)
        } else {
            String::new()
        };
        let reach = match h.reached {
            Some(n) if or_parts(&h.text).len() > 1 => format!(", its last part decided {n}"),
            Some(n) => format!(", `Some` on {n}"),
            None => String::new(),
        };
        let word = if h.not_judged {
            "not judged"
        } else if h.vacuous {
            "vacuous"
        } else if h.failed_calls > 0 {
            "not reached"
        } else {
            "held"
        };
        println!(
            "hydo:   {word}: ensures {} (line {}) on {} calls{reach}{und}",
            h.text, h.line, h.cases
        );
        if h.not_judged {
            println!(
                "hydo:     hydo decided it on {} of {} calls, fewer than --min-reach ({}): {}",
                h.cases,
                h.cases + h.undecided,
                lim.min_reach,
                if h.undecided_why.is_empty() {
                    "a value it reads is of a type that hydo does not read"
                } else {
                    h.undecided_why.as_str()
                }
            );
        }
        if h.failed_calls > 0 {
            let parts = or_parts(&h.text);
            println!(
                "hydo:     `{}` decided {} of the calls; the {} calls that failed above could \
                 not be judged and may be the ones that reach it: fix the failure, then run again",
                parts[parts.len() - 1],
                h.reached.unwrap_or(0),
                h.failed_calls
            );
        }
        if h.vacuous {
            let parts = or_parts(&h.text);
            let n = h.reached.unwrap_or(0);
            if parts.len() > 1 {
                println!(
                    "hydo:     `{}` decided {n} of the calls: on the others `{}` was true, so the \
                     clause held without checking it",
                    parts[parts.len() - 1],
                    parts[..parts.len() - 1].join(" || ")
                );
            } else {
                println!(
                    "hydo:     the property answered `Some` on {n} of the calls and `None` (an \
                     input it cannot use) on the others"
                );
            }
        }
    }
    for a in &r.advice {
        println!("hydo:   to fix: {a}");
    }
    match r.verdict.as_str() {
        "held" => println!("hydo: fuzz: {}: every clause held", r.function),
        "broken" => println!(
            "hydo: fuzz: {}: broken ({} failure{})",
            r.function,
            r.broken.len(),
            if r.broken.len() == 1 { "" } else { "s" }
        ),
        "too few inputs" => println!(
            "hydo: fuzz: {}: too few inputs: {} calls of the {} wanted, and --min-inputs is {}",
            r.function, r.calls, o.cases, lim.min_inputs
        ),
        "not judged" => println!(
            "hydo: fuzz: {}: not judged: a clause was decided on fewer than {} calls \
             (--min-reach)",
            r.function, lim.min_reach
        ),
        _ => println!(
            "hydo: fuzz: {}: vacuous: a clause's last part decided fewer than {} calls (--min-reach)",
            r.function,
            lim.min_reach
        ),
    }
}
