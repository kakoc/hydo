//! Judging a run's records against the plan, on the host after the run.

use crate::plan::{BinOp, ClauseKind, Expr, Function, Plan, Record, Ty};
use serde::{Deserialize, Serialize};

/// A value of an expression.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum V {
    Int(i128),
    Bool(bool),
    /// A floating-point number (Milestone 285). An expression with one is computed in floating
    /// point; integers stay exact without one.
    Float(f64),
}

/// A raw read as a value of its type.
pub fn typed(raw: u64, ty: Ty) -> V {
    match ty {
        Ty::Bool => V::Bool(raw & 0xff != 0),
        Ty::F(4) => V::Float(f32::from_bits(raw as u32) as f64),
        Ty::F(_) => V::Float(f64::from_bits(raw)),
        Ty::Ptr => V::Int(raw as i128),
        Ty::U(n) | Ty::Bytes(n) => {
            let n = n.min(8) as u32;
            let m = if n >= 8 {
                u64::MAX
            } else {
                (1u64 << (8 * n)) - 1
            };
            V::Int((raw & m) as i128)
        }
        Ty::I(n) => {
            let n = n.clamp(1, 8) as u32;
            let sh = 64 - 8 * n;
            V::Int((((raw << sh) as i64) >> sh) as i128)
        }
    }
}

/// Why an expression has no value for a call: a read the guest's page tables could not resolve,
/// a return the run did not reach, or a division by zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undecided(pub String);

/// Evaluate an expression with a call's entry and return reads.
pub fn eval(
    e: &Expr,
    at_entry: &[Option<u64>],
    at_return: Option<&[Option<u64>]>,
) -> Result<V, Undecided> {
    let read = |vals: Option<&[Option<u64>]>, i: u32, ty: Ty, what: &str| {
        let vals = vals.ok_or_else(|| Undecided("the call did not return".into()))?;
        match vals.get(i as usize) {
            Some(Some(raw)) => Ok(typed(*raw, ty)),
            _ => Err(Undecided(format!("the {what} read {i} could not be made"))),
        }
    };
    let int = |v: V| match v {
        V::Int(x) => x,
        V::Bool(b) => b as i128,
        V::Float(f) => f as i128,
    };
    let boolean = |v: V| match v {
        V::Bool(b) => b,
        V::Int(x) => x != 0,
        V::Float(f) => f != 0.0,
    };
    Ok(match e {
        Expr::Int(s) => V::Int(s.parse().unwrap_or(0)),
        Expr::Float(s) => V::Float(s.parse().unwrap_or(0.0)),
        Expr::Bool(b) => V::Bool(*b),
        Expr::In(i, ty) => read(Some(at_entry), *i, *ty, "entry")?,
        Expr::Out(i, ty) => read(at_return, *i, *ty, "return")?,
        Expr::Not(x) => V::Bool(!boolean(eval(x, at_entry, at_return)?)),
        Expr::Neg(x) => match eval(x, at_entry, at_return)? {
            V::Float(f) => V::Float(-f),
            v => V::Int(-int(v)),
        },
        Expr::Guard { cond, then, why } => {
            if !boolean(eval(cond, at_entry, at_return)?) {
                return Err(Undecided(why.clone()));
            }
            eval(then, at_entry, at_return)?
        }
        Expr::Elem { idx, first, elems } => {
            let i = int(eval(idx, at_entry, at_return)?);
            let j = i - *first as i128;
            if j < 0 || j >= elems.len() as i128 {
                return Err(Undecided(format!(
                    "index {i} is outside the elements read ({} to {})",
                    first,
                    *first as usize + elems.len().saturating_sub(1)
                )));
            }
            eval(&elems[j as usize], at_entry, at_return)?
        }
        Expr::StrEq { len, chunks, lit } => {
            let n = int(eval(len, at_entry, at_return)?);
            if n != lit.len() as i128 {
                return Ok(V::Bool(false));
            }
            for (j, c) in chunks.iter().enumerate() {
                let want = lit
                    .as_bytes()
                    .iter()
                    .skip(8 * j)
                    .take(8)
                    .enumerate()
                    .fold(0i128, |a, (k, b)| a | ((*b as i128) << (8 * k)));
                if int(eval(c, at_entry, at_return)?) != want {
                    return Ok(V::Bool(false));
                }
            }
            V::Bool(true)
        }
        Expr::Is { discr, any, not } => {
            let d = int(eval(discr, at_entry, at_return)?);
            let has = |l: &[String]| l.iter().any(|s| s.parse::<i128>().ok() == Some(d));
            V::Bool(if any.is_empty() { !has(not) } else { has(any) })
        }
        Expr::Bin(op, a, b) => {
            // `||` and `&&` look at their right side only when they must: a clause like
            // `p == 0 || p.x > 1` stays decided when the left side settles it
            if matches!(op, BinOp::Or | BinOp::And) {
                let l = boolean(eval(a, at_entry, at_return)?);
                if (*op == BinOp::Or && l) || (*op == BinOp::And && !l) {
                    return Ok(V::Bool(l));
                }
                return Ok(V::Bool(boolean(eval(b, at_entry, at_return)?)));
            }
            let (x, y) = (eval(a, at_entry, at_return)?, eval(b, at_entry, at_return)?);
            if matches!(x, V::Float(_)) || matches!(y, V::Float(_)) {
                return float_op(*op, x, y);
            }
            let (i, j) = (int(x), int(y));
            match op {
                BinOp::Eq => V::Bool(i == j),
                BinOp::Ne => V::Bool(i != j),
                BinOp::Lt => V::Bool(i < j),
                BinOp::Le => V::Bool(i <= j),
                BinOp::Gt => V::Bool(i > j),
                BinOp::Ge => V::Bool(i >= j),
                BinOp::Add => V::Int(i.wrapping_add(j)),
                BinOp::Sub => V::Int(i.wrapping_sub(j)),
                BinOp::Mul => V::Int(i.wrapping_mul(j)),
                BinOp::Div | BinOp::Rem if j == 0 => {
                    return Err(Undecided("a division by zero".into()))
                }
                BinOp::Div => V::Int(i / j),
                BinOp::Rem => V::Int(i % j),
                BinOp::Or | BinOp::And => unreachable!("handled above"),
            }
        }
    })
}

/// A number as a floating-point value.
pub fn as_f64(v: V) -> f64 {
    match v {
        V::Int(x) => x as f64,
        V::Bool(b) => b as u8 as f64,
        V::Float(f) => f,
    }
}

/// An operation where one side is a floating-point number: both are compared and computed as
/// IEEE 754 doubles. A NaN compares unequal to everything, as in the program.
fn float_op(op: BinOp, x: V, y: V) -> Result<V, Undecided> {
    let (a, b) = (as_f64(x), as_f64(y));
    Ok(match op {
        BinOp::Eq => V::Bool(a == b),
        BinOp::Ne => V::Bool(a != b),
        BinOp::Lt => V::Bool(a < b),
        BinOp::Le => V::Bool(a <= b),
        BinOp::Gt => V::Bool(a > b),
        BinOp::Ge => V::Bool(a >= b),
        BinOp::Add => V::Float(a + b),
        BinOp::Sub => V::Float(a - b),
        BinOp::Mul => V::Float(a * b),
        BinOp::Div if b == 0.0 => return Err(Undecided("a division by zero".into())),
        BinOp::Div => V::Float(a / b),
        BinOp::Rem if b == 0.0 => return Err(Undecided("a division by zero".into())),
        BinOp::Rem => V::Float(a % b),
        BinOp::Or | BinOp::And => unreachable!("decided before"),
    })
}

/// A floating-point number as hydo shows it: an integral value with `.0` (`50.0`), else the
/// shortest text that reads back as the same number.
pub fn float_text(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        format!("{f}")
    }
}

/// The first call that broke a clause.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Broken {
    /// The call's number among the function's calls in the run (1 is the first).
    pub call: u64,
    /// The engine's clock at the call's entry (VM exits on the Mac and KVM engines).
    pub at: u64,
    /// Every read of the call, by name, as `name = value`.
    pub values: Vec<String>,
}

/// One clause over a run.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Outcome {
    pub function: String,
    pub file: String,
    /// The clause's line.
    pub line: u32,
    pub kind: String,
    pub text: String,
    /// The calls it was judged on.
    pub calls: u64,
    pub violations: u64,
    /// Calls it could not judge (a read that failed, a return not reached).
    pub undecided: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<Broken>,
}

/// Every clause of the plan over one run's records.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
    /// Calls recorded.
    pub calls: u64,
    /// Calls without a return read (the run ended inside the call, or no breakpoint was left).
    pub without_return: u64,
    /// Calls the engine stopped at but did not record, after the first
    /// [`crate::plan::MAX_RECORDS`] of the run: no clause was judged on them (Milestone 255).
    #[serde(default)]
    pub not_recorded: u64,
}

impl Report {
    /// The clauses with a violation.
    pub fn violated(&self) -> impl Iterator<Item = &Outcome> {
        self.outcomes.iter().filter(|o| o.violations > 0)
    }
}

fn shown(v: Option<u64>, ty: Option<Ty>) -> String {
    match (v, ty) {
        (None, _) => "unreadable".into(),
        (Some(r), Some(t)) => match typed(r, t) {
            V::Int(x) if t == Ty::Ptr => format!("{x:#x}"),
            V::Int(x) if matches!(t, Ty::Bytes(_)) => {
                let n = if let Ty::Bytes(n) = t { n.min(8) } else { 0 };
                let b: Vec<u8> = (0..n).map(|k| (x >> (8 * k)) as u8).collect();
                format!("{:?}", String::from_utf8_lossy(&b))
            }
            V::Int(x) => x.to_string(),
            V::Bool(b) => b.to_string(),
            V::Float(f) => float_text(f),
        },
        (Some(r), None) => r.to_string(),
    }
}

/// The type each read has in a function's clauses (a read no clause uses has none).
fn read_types(f: &Function) -> (Vec<Option<Ty>>, Vec<Option<Ty>>) {
    fn walk(e: &Expr, i: &mut [Option<Ty>], o: &mut [Option<Ty>]) {
        match e {
            Expr::In(k, t) => {
                if let Some(s) = i.get_mut(*k as usize) {
                    *s = Some(*t)
                }
            }
            Expr::Out(k, t) => {
                if let Some(s) = o.get_mut(*k as usize) {
                    *s = Some(*t)
                }
            }
            Expr::Not(x) | Expr::Neg(x) => walk(x, i, o),
            Expr::Bin(_, a, b)
            | Expr::Guard {
                cond: a, then: b, ..
            } => {
                walk(a, i, o);
                walk(b, i, o);
            }
            Expr::Elem { idx, elems, .. } => {
                walk(idx, i, o);
                for e in elems {
                    walk(e, i, o);
                }
            }
            Expr::StrEq { len, chunks, .. } => {
                walk(len, i, o);
                for e in chunks {
                    walk(e, i, o);
                }
            }
            Expr::Is { discr, .. } => walk(discr, i, o),
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) => {}
        }
    }
    let mut i = vec![None; f.at_entry.len()];
    let mut o = vec![None; f.at_return.len()];
    for c in &f.clauses {
        walk(&c.expr, &mut i, &mut o);
    }
    (i, o)
}

/// Judge a run's records.
pub fn judge(plan: &Plan, recs: &[Record]) -> Report {
    let mut rep = Report::default();
    let fns: Vec<&Function> = plan.programs.iter().flat_map(|p| &p.functions).collect();
    for f in &fns {
        for c in &f.clauses {
            rep.outcomes.push(Outcome {
                function: f.name.clone(),
                file: f.file.clone(),
                line: c.line,
                kind: c.kind.name().into(),
                text: c.text.clone(),
                ..Outcome::default()
            });
        }
    }
    let mut ncalls = std::collections::BTreeMap::<u32, u64>::new();
    for r in recs {
        rep.calls += 1;
        // a function stopped at its entry only (no return reads) has no return to miss
        let has_ret = fns.iter().any(|f| f.id == r.f && !f.at_return.is_empty());
        if r.at_return.is_none() && has_ret {
            rep.without_return += 1;
        }
        let n = ncalls.entry(r.f).or_default();
        *n += 1;
        let mut k = 0;
        for f in &fns {
            if f.id != r.f {
                k += f.clauses.len();
                continue;
            }
            let (ti, to) = read_types(f);
            for c in &f.clauses {
                let o = &mut rep.outcomes[k];
                k += 1;
                if c.kind == ClauseKind::Ensures && r.at_return.is_none() {
                    o.undecided += 1;
                    continue;
                }
                o.calls += 1;
                match eval(&c.expr, &r.at_entry, r.at_return.as_deref()) {
                    Ok(V::Bool(true)) => {}
                    Ok(V::Int(x)) if x != 0 => {}
                    Ok(V::Float(x)) if x != 0.0 => {}
                    Ok(_) => {
                        o.violations += 1;
                        if o.first.is_none() {
                            let mut values = Vec::new();
                            for (j, l) in f.at_entry.iter().enumerate() {
                                if ti[j].is_some() {
                                    let v = r.at_entry.get(j).copied().flatten();
                                    values.push(format!("{} = {}", l.name, shown(v, ti[j])));
                                }
                            }
                            if let Some(out) = &r.at_return {
                                for (j, l) in f.at_return.iter().enumerate() {
                                    if to[j].is_some() {
                                        let v = out.get(j).copied().flatten();
                                        values.push(format!("{} = {}", l.name, shown(v, to[j])));
                                    }
                                }
                            }
                            o.first = Some(Broken {
                                call: *n,
                                at: r.at,
                                values,
                            });
                        }
                    }
                    Err(_) => o.undecided += 1,
                }
            }
        }
    }
    rep
}
