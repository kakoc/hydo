//! A clause judged over the values of one call: the generated input (the values at entry), the
//! values of the `&mut` parameters after the call, and the result. The meaning is the one of
//! docs/design/contracts.md, "The language": a parameter in `ensures` is its value at entry, a
//! read through a reference parameter is made after the call unless it is inside `old()`, and
//! integers are exact.

use super::value::{Hints, Val};
use crate::lang::{Ast, Builtin, Pat};
use crate::plan::BinOp;

/// The values one clause reads.
pub struct Env<'a> {
    /// Each parameter's value at entry.
    pub entry: &'a [(String, Val)],
    /// Each parameter's value after the call (a value parameter keeps its entry value).
    pub after: &'a [(String, Val)],
    pub result: Option<&'a Val>,
}

/// Why a clause has no value for a call.
pub type Undecided = String;

/// The value of `a`: `true` or `false` for a clause.
pub fn holds(a: &Ast, env: &Env) -> Result<bool, Undecided> {
    match eval(a, env, false)? {
        Val::Bool(b) => Ok(b),
        Val::Int(x) => Ok(x != 0),
        _ => Err("the clause is not a condition".into()),
    }
}

/// The value of `a` for one call.
pub fn value(a: &Ast, env: &Env) -> Result<Val, Undecided> {
    eval(a, env, false)
}

/// The value of `a`, a value that a `build` line built read through its `read` line.
fn eval(a: &Ast, env: &Env, old: bool) -> Result<Val, Undecided> {
    match eval_raw(a, env, old)? {
        Val::Built(Some(v), _) => Ok(*v),
        Val::Built(None, args) => Err(format!(
            "`{a}` is a value that a `build` line built: a clause reads it through its type's \
             `read` line (`// hydo: read <Type> as <type> by <method>()`), in `ensures` only, so \
             hydo does not compare it here; name its constructor's arguments instead ({})",
            args.iter()
                .map(|(n, _)| format!("`{a}.{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        v => Ok(v),
    }
}

/// The value of `a`, a built value as it is (Milestone 307), so that a field can name a
/// constructor's argument.
fn eval_raw(a: &Ast, env: &Env, old: bool) -> Result<Val, Undecided> {
    Ok(match a {
        Ast::Int(x) => Val::Int(*x),
        Ast::Float(f) => Val::Float(f.to_bits()),
        Ast::Bool(b) => Val::Bool(*b),
        Ast::Str(s) => Val::Str(s.clone()),
        Ast::Name(n) => {
            if n == "result" {
                return env
                    .result
                    .cloned()
                    .ok_or_else(|| "the call did not return".to_string());
            }
            let from = if old { env.entry } else { env.after };
            from.iter()
                .find(|(p, _)| p == n)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("`{n}` is not a parameter"))?
        }
        Ast::Old(x) => eval_raw(x, env, true)?,
        Ast::Field(x, f) => {
            let v = eval_raw(x, env, old)?;
            // a constructor's argument, else a field of the value that the `read` line gives
            let v = match v {
                Val::Built(view, args) => match args.into_iter().find(|(n, _)| n == f) {
                    Some((_, a)) => return Ok(a),
                    None => match view {
                        Some(v) => *v,
                        None => return Err(format!("no argument `{f}`")),
                    },
                },
                v => v,
            };
            match v {
                Val::Struct(fs) => fs
                    .into_iter()
                    .find(|(n, _)| n == f)
                    .map(|(_, v)| v)
                    .ok_or_else(|| format!("no field `{f}`"))?,
                Val::Tuple(t) => f
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| t.into_iter().nth(i))
                    .ok_or_else(|| format!("no field `{f}`"))?,
                // `m.Complete.0`: the fields of the variant that the value is (Milestone 307)
                Val::Enum(n, fs) if n == *f => Val::Struct(fs),
                Val::Enum(n, _) => return Err(format!("the value is `{n}`, not `{f}`")),
                Val::Opaque => return Err(format!("`.{f}` reads a value of a type not read")),
                _ => return Err(format!("`.{f}` of a value without fields")),
            }
        }
        Ast::Index(x, i) => {
            let v = eval(x, env, old)?;
            let k = int(&eval(i, env, old)?)?;
            match v {
                Val::List(l) => {
                    if k < 0 || k as u128 >= l.len() as u128 {
                        return Err(format!("index {k} is past the length {}", l.len()));
                    }
                    l[k as usize].clone()
                }
                _ => return Err("an element of a value that is not a list".into()),
            }
        }
        Ast::Call(b, x) => {
            let v = eval(x, env, old)?;
            match (b, v) {
                (Builtin::Len, Val::List(l)) => Val::Int(l.len() as i128),
                (Builtin::Len, Val::Str(s)) => Val::Int(s.len() as i128),
                (Builtin::IsSome, Val::Opt(o)) => Val::Bool(o.is_some()),
                (Builtin::IsNone, Val::Opt(o)) => Val::Bool(o.is_none()),
                (Builtin::IsOk, Val::Res(r)) => Val::Bool(r.is_ok()),
                (Builtin::IsErr, Val::Res(r)) => Val::Bool(r.is_err()),
                (Builtin::Some, Val::Opt(Some(x))) => *x,
                (Builtin::Some, Val::Opt(None)) => return Err("the value is None".into()),
                (Builtin::Ok, Val::Res(Ok(x))) => *x,
                (Builtin::Err, Val::Res(Err(x))) => *x,
                (Builtin::Ok | Builtin::Err, Val::Res(_)) => {
                    return Err("the value is the other variant".into())
                }
                (Builtin::Sorted, Val::List(l)) => Val::Bool(sorted(&l)?),
                (_, Val::Opaque) => return Err("a value of a type not read".into()),
                (b, _) => return Err(format!("`{}()` of a value of another type", b.name())),
            }
        }
        Ast::Matches(x, p) => Val::Bool(matches_pat(&eval(x, env, old)?, p)?),
        Ast::Not(x) => Val::Bool(!truth(&eval(x, env, old)?)?),
        Ast::Neg(x) => match eval(x, env, old)? {
            Val::Float(b) => Val::Float((-f64::from_bits(b)).to_bits()),
            v => Val::Int(int(&v)?.checked_neg().ok_or("an integer past 128 bits")?),
        },
        Ast::Bin(op, x, y) => {
            if matches!(op, BinOp::Or | BinOp::And) {
                let l = truth(&eval(x, env, old)?)?;
                if (*op == BinOp::Or && l) || (*op == BinOp::And && !l) {
                    return Ok(Val::Bool(l));
                }
                return Ok(Val::Bool(truth(&eval(y, env, old)?)?));
            }
            let (l, r) = (eval(x, env, old)?, eval(y, env, old)?);
            // `&`, `|`, `^`, `<<` and `>>` (Milestone 326): on two booleans as Rust's, and on
            // integers exactly
            if op.is_bits() {
                if let (Val::Bool(p), Val::Bool(q), false) =
                    (&l, &r, matches!(op, BinOp::Shl | BinOp::Shr))
                {
                    let v = op.bits(*p as i128, *q as i128).unwrap_or(0);
                    return Ok(Val::Bool(v != 0));
                }
                if matches!(l, Val::Float(_)) || matches!(r, Val::Float(_)) {
                    return Err(format!("`{a}`: a bit operator on a floating-point value"));
                }
                return op.bits(int(&l)?, int(&r)?).map(Val::Int).ok_or_else(|| {
                    format!("`{a}`: a shift by a negative amount, or a `<<` past 128 bits")
                });
            }
            if matches!(l, Val::Float(_)) || matches!(r, Val::Float(_)) {
                return float_op(*op, &l, &r);
            }
            match op {
                BinOp::Eq | BinOp::Ne => {
                    // also a list, an option or a structure that holds such a value: hydo
                    // cannot tell two of them apart, so the clause is undecided, never broken
                    // (Milestone 333)
                    if has_unread(&l) || has_unread(&r) {
                        return Err("a comparison with a value of a type not read".into());
                    }
                    // values of two kinds are never equal by accident: hydo cannot compare
                    // them, so the clause is undecided, never broken (Milestone 318)
                    if let (Some(kl), Some(kr)) = (kind(&l), kind(&r)) {
                        if kl != kr {
                            return Err(format!(
                                "`{a}` compares {kl} with {kr}, which hydo cannot compare"
                            ));
                        }
                    }
                    if matches!(l, Val::Enum(..)) || matches!(r, Val::Enum(..)) {
                        return Err(
                            "a variant is compared with `matches(<value>, <pattern>)`".into()
                        );
                    }
                    let eq = match (&l, &r) {
                        (Val::Int(_) | Val::Bool(_), Val::Int(_) | Val::Bool(_)) => {
                            int(&l)? == int(&r)?
                        }
                        _ => l == r,
                    };
                    Val::Bool(if *op == BinOp::Eq { eq } else { !eq })
                }
                _ => {
                    let (i, j) = (int(&l)?, int(&r)?);
                    let over = || "an integer past 128 bits".to_string();
                    match op {
                        BinOp::Lt => Val::Bool(i < j),
                        BinOp::Le => Val::Bool(i <= j),
                        BinOp::Gt => Val::Bool(i > j),
                        BinOp::Ge => Val::Bool(i >= j),
                        BinOp::Add => Val::Int(i.checked_add(j).ok_or_else(over)?),
                        BinOp::Sub => Val::Int(i.checked_sub(j).ok_or_else(over)?),
                        BinOp::Mul => Val::Int(i.checked_mul(j).ok_or_else(over)?),
                        BinOp::Div | BinOp::Rem if j == 0 => {
                            return Err("a division by zero".into())
                        }
                        BinOp::Div => Val::Int(i / j),
                        BinOp::Rem => Val::Int(i % j),
                        _ => unreachable!("handled above"),
                    }
                }
            }
        }
    })
}

/// Whether a value is or holds a value of a type that hydo does not read (Milestone 333): an
/// opaque value, or a built value that no `read` line read.
pub fn has_unread(v: &Val) -> bool {
    match v {
        Val::Opaque | Val::Built(None, _) => true,
        Val::Built(Some(x), _) => has_unread(x),
        Val::List(l) | Val::Tuple(l) => l.iter().any(has_unread),
        Val::Opt(Some(x)) | Val::Res(Ok(x)) | Val::Res(Err(x)) => has_unread(x),
        Val::Struct(f) | Val::Enum(_, f) => f.iter().any(|(_, x)| has_unread(x)),
        _ => false,
    }
}

/// Whether each element of a list is at most the next (Milestone 307): integers, floating-point
/// values and strings compare as values; a NaN is in order with nothing.
fn sorted(l: &[Val]) -> Result<bool, Undecided> {
    for w in l.windows(2) {
        let le = match (&w[0], &w[1]) {
            (a, b) if has_unread(a) || has_unread(b) => {
                return Err("a value of a type not read".into())
            }
            (Val::Str(a), Val::Str(b)) => a <= b,
            (a, b) => matches!(float_op(BinOp::Le, a, b)?, Val::Bool(true)),
        };
        if !le {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether a value has the shape of a pattern (Milestone 307).
pub fn matches_pat(v: &Val, p: &Pat) -> Result<bool, Undecided> {
    let all = |vs: &[&Val], ps: &[Pat], what: &str| -> Result<bool, Undecided> {
        if vs.len() != ps.len() {
            return Err(format!(
                "the pattern gives {} fields, and {what} has {}",
                ps.len(),
                vs.len()
            ));
        }
        for (v, p) in vs.iter().zip(ps) {
            if !matches_pat(v, p)? {
                return Ok(false);
            }
        }
        Ok(true)
    };
    match (p, v) {
        (Pat::Any, _) => Ok(true),
        (_, Val::Opaque) => Err("a value of a type not read".into()),
        (Pat::Int(k), Val::Int(x)) => Ok(x == k),
        (Pat::Int(k), Val::Float(b)) => Ok(f64::from_bits(*b) == *k as f64),
        (Pat::Bool(k), Val::Bool(x)) => Ok(x == k),
        (Pat::Str(k), Val::Str(x)) => Ok(x == k),
        (Pat::Tuple(ps), Val::Tuple(vs)) => all(&vs.iter().collect::<Vec<_>>(), ps, "the tuple"),
        (Pat::Variant(n, sub), _) => {
            let (name, fields): (&str, Vec<&Val>) = match v {
                Val::Opt(None) => ("None", vec![]),
                Val::Opt(Some(x)) => ("Some", vec![&**x]),
                Val::Res(Ok(x)) => ("Ok", vec![&**x]),
                Val::Res(Err(x)) => ("Err", vec![&**x]),
                Val::Enum(n, f) => (n.as_str(), f.iter().map(|(_, x)| x).collect()),
                // a structure: its name was checked against the type before the run
                Val::Struct(f) => (n.as_str(), f.iter().map(|(_, x)| x).collect()),
                _ => {
                    return Err(format!(
                        "`{p}` is a pattern of a variant, and the value is not one"
                    ))
                }
            };
            if name != n {
                return Ok(false);
            }
            match sub {
                None => Ok(true),
                Some(ps) => all(&fields, ps, &format!("`{n}`")),
            }
        }
        // named fields (Milestone 326): each named one matches its pattern
        (Pat::Fields(n, ps), Val::Enum(..) | Val::Struct(_)) => {
            let (vname, fs) = match v {
                Val::Enum(m, fs) => (m.as_str(), fs),
                // a structure: its name was checked against the type before the run
                Val::Struct(fs) => (n.as_str(), fs),
                _ => unreachable!("matched above"),
            };
            if vname != n {
                return Ok(false);
            }
            for (k, p) in ps {
                let Some((_, x)) = fs.iter().find(|(f, _)| f == k) else {
                    return Err(format!("`{n}` has no field `{k}`"));
                };
                if !matches_pat(x, p)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Pat::Fields(..), _) => Err(format!(
            "`{p}` is a pattern of named fields, and the value has none"
        )),
        (p, _) => Err(format!("the pattern `{p}` does not fit the value's type")),
    }
}

/// A number of a side of an operation with a floating-point value: an integer exactly, or the
/// floating-point value.
enum Num {
    I(i128),
    F(f64),
}

fn num(v: &Val) -> Result<Num, Undecided> {
    match v {
        Val::Float(b) => Ok(Num::F(f64::from_bits(*b))),
        o => Ok(Num::I(int(o)?)),
    }
}

/// How an integer compares with a floating-point value, exactly (no rounding of the integer);
/// `None` for a NaN.
fn cmp_int_float(i: i128, f: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if f.is_nan() {
        return None;
    }
    if f >= 1.7e38 {
        return Some(Ordering::Less);
    }
    if f <= -1.7e38 {
        return Some(Ordering::Greater);
    }
    let fl = f.floor();
    // `fl` is an integer within i128's range, so the cast is exact
    let fi = fl as i128;
    Some(match i.cmp(&fi) {
        Ordering::Equal if f == fl => Ordering::Equal,
        Ordering::Equal => Ordering::Less,
        o => o,
    })
}

/// An operation where one side is a floating-point value (Milestone 289). A comparison with an
/// integer is exact. Arithmetic is the machine's `f64` arithmetic, as in the program. A NaN
/// compares unequal to everything, itself included.
fn float_op(op: BinOp, l: &Val, r: &Val) -> Result<Val, Undecided> {
    use std::cmp::Ordering;
    let (a, b) = (num(l)?, num(r)?);
    let ord: Option<Ordering> = match (&a, &b) {
        (Num::F(x), Num::F(y)) => x.partial_cmp(y),
        (Num::I(i), Num::F(f)) => cmp_int_float(*i, *f),
        (Num::F(f), Num::I(i)) => cmp_int_float(*i, *f).map(Ordering::reverse),
        (Num::I(i), Num::I(j)) => Some(i.cmp(j)),
    };
    let fl = |n: &Num| match n {
        Num::I(i) => *i as f64,
        Num::F(f) => *f,
    };
    let (x, y) = (fl(&a), fl(&b));
    let f = |v: f64| Val::Float(v.to_bits());
    Ok(match op {
        BinOp::Eq => Val::Bool(ord == Some(Ordering::Equal)),
        BinOp::Ne => Val::Bool(ord != Some(Ordering::Equal)),
        BinOp::Lt => Val::Bool(ord == Some(Ordering::Less)),
        BinOp::Le => Val::Bool(matches!(ord, Some(Ordering::Less | Ordering::Equal))),
        BinOp::Gt => Val::Bool(ord == Some(Ordering::Greater)),
        BinOp::Ge => Val::Bool(matches!(ord, Some(Ordering::Greater | Ordering::Equal))),
        BinOp::Add => f(x + y),
        BinOp::Sub => f(x - y),
        BinOp::Mul => f(x * y),
        BinOp::Div => f(x / y),
        BinOp::Rem => f(x % y),
        _ => unreachable!("decided before"),
    })
}

/// What kind of value a comparison sees, for a refusal of a comparison of two kinds; none for
/// a value that compares with any (Milestone 318).
fn kind(v: &Val) -> Option<&'static str> {
    Some(match v {
        Val::Int(_) | Val::Bool(_) | Val::Float(_) => "a number",
        Val::Str(_) => "a string",
        Val::List(_) => "a list",
        Val::Opt(_) => "an `Option`",
        Val::Res(_) => "a `Result`",
        Val::Tuple(_) => "a tuple",
        Val::Struct(_) => "a structure",
        Val::Enum(..) => "a variant",
        Val::Built(..) => "a value built by a `build` line",
        Val::Opaque => return None,
    })
}

fn int(v: &Val) -> Result<i128, Undecided> {
    match v {
        Val::Int(x) => Ok(*x),
        Val::Bool(b) => Ok(*b as i128),
        Val::Opaque => Err("a value of a type not read".into()),
        Val::Float(_) => Err("a floating-point value where an integer must be".into()),
        _ => Err("arithmetic on a value that is not a number".into()),
    }
}

fn truth(v: &Val) -> Result<bool, Undecided> {
    match v {
        Val::Bool(b) => Ok(*b),
        Val::Int(x) => Ok(*x != 0),
        _ => Err("a condition on a value that is not one".into()),
    }
}

/// The path a place expression names, as the generator keys its hints: `l.balance`, `v[]` for
/// the elements of `v`, `v#len` for its length. `old()` is looked through.
pub fn path_of(a: &Ast) -> Option<String> {
    match a {
        Ast::Name(n) => Some(n.clone()),
        Ast::Field(x, f) => Some(format!("{}.{f}", path_of(x)?)),
        Ast::Index(x, _) => Some(format!("{}[]", path_of(x)?)),
        Ast::Call(Builtin::Len, x) => Some(format!("{}#len", path_of(x)?)),
        Ast::Call(Builtin::Some | Builtin::Ok | Builtin::Err, x) | Ast::Old(x) => path_of(x),
        _ => None,
    }
}

/// Every path a side of a comparison mentions, with its integer constants.
fn paths_in(a: &Ast, out: &mut Vec<String>, consts: &mut Vec<i128>) {
    if let Some(p) = path_of(a) {
        out.push(p);
        return;
    }
    match a {
        Ast::Int(x) => consts.push(*x),
        Ast::Neg(x) => {
            if let Ast::Int(v) = **x {
                consts.push(-v);
            } else {
                paths_in(x, out, consts);
            }
        }
        Ast::Not(x) | Ast::Old(x) | Ast::Call(_, x) | Ast::Field(x, _) | Ast::Matches(x, _) => {
            paths_in(x, out, consts)
        }
        Ast::Bin(_, x, y) | Ast::Index(x, y) => {
            paths_in(x, out, consts);
            paths_in(y, out, consts);
        }
        _ => {}
    }
}

/// The lists that a `requires` clause wants sorted: each `sorted(v)` among its conjuncts
/// (Milestone 307).
pub fn sorted_hints(a: &Ast, h: &mut Hints) {
    match a {
        Ast::Bin(BinOp::And, x, y) => {
            sorted_hints(x, h);
            sorted_hints(y, h);
        }
        Ast::Call(Builtin::Sorted, x) => {
            if let Some(p) = path_of(x) {
                if !h.sorted.contains(&p) {
                    h.sorted.push(p);
                }
            }
        }
        // two lists of one length (Milestone 326): `len(self) == len(src)`
        Ast::Bin(BinOp::Eq, x, y) => {
            if let (Ast::Call(Builtin::Len, x), Ast::Call(Builtin::Len, y)) = (&**x, &**y) {
                if let (Some(a), Some(b)) = (path_of(x), path_of(y)) {
                    if a != b && !h.same_len.iter().any(|(p, q)| (p, q) == (&a, &b)) {
                        h.same_len.push((a, b));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Add what a clause says about the paths it compares: the constants each is compared with, and
/// the pairs of paths compared with each other.
pub fn hints_of(a: &Ast, h: &mut Hints) {
    match a {
        Ast::Bin(BinOp::Or | BinOp::And, x, y) => {
            hints_of(x, h);
            hints_of(y, h);
        }
        Ast::Not(x) => hints_of(x, h),
        Ast::Bin(BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, x, y) => {
            let (mut lp, mut lc, mut rp, mut rc) = (vec![], vec![], vec![], vec![]);
            paths_in(x, &mut lp, &mut lc);
            paths_in(y, &mut rp, &mut rc);
            for (ps, cs) in [(&lp, &rc), (&rp, &lc)] {
                if cs.is_empty() {
                    continue;
                }
                for p in ps {
                    h.consts
                        .entry(p.clone())
                        .or_default()
                        .extend(cs.iter().copied());
                }
            }
            for a in &lp {
                for b in &rp {
                    if a != b && !h.pairs.contains(&(a.clone(), b.clone())) {
                        h.pairs.push((a.clone(), b.clone()));
                    }
                }
            }
        }
        _ => {}
    }
}
