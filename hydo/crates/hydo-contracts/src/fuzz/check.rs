//! A clause judged over the values of one call: the generated input (the values at entry), the
//! values of the `&mut` parameters after the call, and the result. The meaning is the one of
//! docs/design/contracts.md, "The language": a parameter in `ensures` is its value at entry, a
//! read through a reference parameter is made after the call unless it is inside `old()`, and
//! integers are exact.

use super::value::{Hints, Val};
use crate::lang::{Ast, Builtin};
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

fn eval(a: &Ast, env: &Env, old: bool) -> Result<Val, Undecided> {
    Ok(match a {
        Ast::Int(x) => Val::Int(*x),
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
        Ast::Old(x) => eval(x, env, true)?,
        Ast::Field(x, f) => {
            let v = eval(x, env, old)?;
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
                (_, Val::Opaque) => return Err("a value of a type not read".into()),
                (b, _) => return Err(format!("`{}()` of a value of another type", b.name())),
            }
        }
        Ast::Not(x) => Val::Bool(!truth(&eval(x, env, old)?)?),
        Ast::Neg(x) => Val::Int(
            int(&eval(x, env, old)?)?
                .checked_neg()
                .ok_or("an integer past 128 bits")?,
        ),
        Ast::Bin(op, x, y) => {
            if matches!(op, BinOp::Or | BinOp::And) {
                let l = truth(&eval(x, env, old)?)?;
                if (*op == BinOp::Or && l) || (*op == BinOp::And && !l) {
                    return Ok(Val::Bool(l));
                }
                return Ok(Val::Bool(truth(&eval(y, env, old)?)?));
            }
            let (l, r) = (eval(x, env, old)?, eval(y, env, old)?);
            match op {
                BinOp::Eq | BinOp::Ne => {
                    if l == Val::Opaque || r == Val::Opaque {
                        return Err("a comparison with a value of a type not read".into());
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

fn int(v: &Val) -> Result<i128, Undecided> {
    match v {
        Val::Int(x) => Ok(*x),
        Val::Bool(b) => Ok(*b as i128),
        Val::Opaque => Err("a value of a type not read".into()),
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
        Ast::Not(x) | Ast::Old(x) | Ast::Call(_, x) | Ast::Field(x, _) => paths_in(x, out, consts),
        Ast::Bin(_, x, y) | Ast::Index(x, y) => {
            paths_in(x, out, consts);
            paths_in(y, out, consts);
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
