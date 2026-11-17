//! The value history (Milestone 285, docs/design/value-history.md): the values that functions'
//! `// hydo: record` lines name, shown from a run's records, in the order the run made the
//! calls, per key, with each change marked.
//!
//! The engine reads only what the plan says, at the breakpoints the contracts use, so a run with
//! the record plan has the fingerprint of the run without it. Everything here runs on the host
//! after the run.

use crate::eval::{eval, float_text, Undecided, V};
use crate::plan::{Expr, Function, Plan, Program, Record, Show, Ty};
use std::collections::BTreeMap;

/// A recorded value as the history shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    Int(i128),
    Bool(bool),
    Float(f64),
    Ptr(u64),
    /// A string, and whether it was cut at the bytes read.
    Text(String, bool),
    /// A nil interface.
    Nil,
    /// A key that is not in the map, or a nil or empty map.
    Absent,
    /// An interface value of a dynamic type the history does not read: its descriptor's address.
    Other(u64),
    /// The value could not be read for this call, and why.
    Unreadable(String),
}

impl std::fmt::Display for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Shown::Int(x) => write!(f, "{x}"),
            Shown::Bool(b) => write!(f, "{b}"),
            Shown::Float(x) => write!(f, "{}", float_text(*x)),
            Shown::Ptr(0) => write!(f, "nil"),
            Shown::Ptr(p) => write!(f, "{p:#x}"),
            Shown::Text(s, false) => write!(f, "{s:?}"),
            Shown::Text(s, true) => write!(f, "{s:?}..."),
            Shown::Nil => write!(f, "nil"),
            Shown::Absent => write!(f, "absent"),
            Shown::Other(a) => write!(f, "a value of another type (type at {a:#x})"),
            Shown::Unreadable(why) => write!(f, "unreadable ({why})"),
        }
    }
}

impl Shown {
    /// The text of a key: a string without its quotes.
    pub fn key_text(&self) -> String {
        match self {
            Shown::Text(s, _) => s.clone(),
            other => other.to_string(),
        }
    }
}

fn num(e: &Expr, i: &[Option<u64>], o: Option<&[Option<u64>]>) -> Result<i128, Undecided> {
    Ok(match eval(e, i, o)? {
        V::Int(x) => x,
        V::Bool(b) => b as i128,
        V::Float(f) => f as i128,
    })
}

/// The type an expression's top read has (a pointer shows as an address).
fn top_ty(e: &Expr) -> Option<Ty> {
    match e {
        Expr::In(_, t) | Expr::Out(_, t) => Some(*t),
        Expr::Guard { then, .. } => top_ty(then),
        _ => None,
    }
}

/// Show a value from a call's reads.
pub fn show(s: &Show, i: &[Option<u64>], o: Option<&[Option<u64>]>) -> Shown {
    let unread = |u: Undecided| Shown::Unreadable(u.0);
    match s {
        Show::Val(e) => match eval(e, i, o) {
            Ok(V::Int(x)) if top_ty(e) == Some(Ty::Ptr) => Shown::Ptr(x as u64),
            Ok(V::Int(x)) => Shown::Int(x),
            Ok(V::Bool(b)) => Shown::Bool(b),
            Ok(V::Float(f)) => Shown::Float(f),
            Err(u) => unread(u),
        },
        Show::Text { len, chunks } => {
            let n = match num(len, i, o) {
                Ok(n) => n.max(0) as usize,
                Err(u) => return unread(u),
            };
            let mut b = Vec::new();
            for c in chunks {
                if b.len() >= n {
                    break;
                }
                let Some(Ty::Bytes(width)) = top_ty(c) else {
                    break;
                };
                let Ok(V::Int(x)) = eval(c, i, o) else {
                    break;
                };
                for k in 0..width {
                    b.push((x >> (8 * k as u32)) as u8);
                }
            }
            if b.len() < n.min(chunks.len() * 8) {
                return Shown::Unreadable(format!(
                    "{} of the string's {n} bytes could be read",
                    b.len()
                ));
            }
            let cut = n > b.len();
            b.truncate(n);
            Shown::Text(String::from_utf8_lossy(&b).into_owned(), cut)
        }
        Show::Dyn { nil, ty, cases } => {
            match num(nil, i, o) {
                Ok(0) => return Shown::Nil,
                Ok(_) => {}
                Err(u) => return unread(u),
            }
            let t = match num(ty, i, o) {
                Ok(t) => t as u64,
                Err(u) => return unread(u),
            };
            match cases.iter().find(|c| c.addr == t) {
                Some(c) => show(&c.show, i, o),
                None => Shown::Other(t),
            }
        }
        Show::Lookup {
            map,
            dir_len,
            group,
            slots,
        } => {
            match num(map, i, o) {
                Ok(0) => return Shown::Absent,
                Ok(_) => {}
                Err(u) => return unread(u),
            }
            match num(dir_len, i, o) {
                Ok(0) => {}
                Ok(n) => {
                    return Shown::Unreadable(format!(
                        "a map of more than one group ({n} tables); the record reads a map of \
                         at most 8 entries"
                    ))
                }
                Err(u) => return unread(u),
            }
            match num(group, i, o) {
                Ok(0) => return Shown::Absent,
                Ok(_) => {}
                Err(u) => return unread(u),
            }
            for s in slots {
                match num(&s.full, i, o) {
                    Ok(0) => continue,
                    Ok(_) => {}
                    Err(u) => return unread(u),
                }
                // a full slot whose key cannot be read leaves the lookup open
                match eval(&s.hit, i, o) {
                    Ok(V::Bool(true)) => return show(&s.val, i, o),
                    Ok(_) => {}
                    Err(u) => return unread(u),
                }
            }
            Shown::Absent
        }
    }
}

/// One stop of a recording function: its entry or its return, with the values recorded there.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    pub function: String,
    pub file: String,
    /// The function's declaration line.
    pub line: u32,
    /// The call's number among the function's recorded calls (1 is the first).
    pub call: u64,
    /// `entry` or `return`.
    pub when: &'static str,
    /// The engine's clock at the stop (VM exits on the Mac and KVM engines).
    pub at: u64,
    pub boot: u32,
    /// The call's key, as text.
    pub key: Option<String>,
    /// The function the call came from (by its return address), or the return address.
    pub caller: Option<String>,
    /// The values by name, in the order the function's lines name them.
    pub values: Vec<(String, Shown)>,
}

fn caller(prog: &Program, f: &Function, rec: &Record) -> Option<String> {
    let k = f.ret_addr?;
    let ra = rec.at_entry.get(k as usize).copied().flatten()?;
    Some(
        prog.calls
            .iter()
            .find(|c| c.callee == f.id && c.ret == ra)
            .map(|c| c.caller.clone())
            .unwrap_or_else(|| format!("{ra:#x}")),
    )
}

/// Every stop of a recording function in a run, in the run's order: by boot, then by the
/// engine's clock, an entry before the return of the same call.
pub fn points(plan: &Plan, recs: &[Record]) -> Vec<Point> {
    let mut fns: BTreeMap<u32, (&Program, &Function)> = BTreeMap::new();
    for p in &plan.programs {
        for f in p.functions.iter().filter(|f| !f.values.is_empty()) {
            fns.insert(f.id, (p, f));
        }
    }
    let mut calls: BTreeMap<u32, u64> = BTreeMap::new();
    let mut out: Vec<((u32, u64, usize, u8), Point)> = Vec::new();
    for (ri, r) in recs.iter().enumerate() {
        let Some((prog, f)) = fns.get(&r.f) else {
            continue;
        };
        let n = calls.entry(r.f).or_default();
        *n += 1;
        let out_reads = r.at_return.as_deref();
        let key = f
            .values
            .iter()
            .find(|v| v.key)
            .and_then(|v| v.entry.as_ref())
            .map(|s| show(s, &r.at_entry, None).key_text());
        let caller = caller(prog, f, r);
        let mk = |when: &'static str, at: u64, vals: Vec<(String, Shown)>| Point {
            function: f.name.clone(),
            file: f.file.clone(),
            line: f.line,
            call: *n,
            when,
            at,
            boot: r.boot,
            key: key.clone(),
            caller: caller.clone(),
            values: vals,
        };
        let at_entry: Vec<(String, Shown)> = f
            .values
            .iter()
            .filter(|v| !v.key)
            .filter_map(|v| {
                let s = v.entry.as_ref()?;
                Some((v.name.clone(), show(s, &r.at_entry, None)))
            })
            .collect();
        if !at_entry.is_empty() {
            out.push(((r.boot, r.at, ri, 0), mk("entry", r.at, at_entry)));
        }
        let at_ret: Vec<(String, Shown)> = f
            .values
            .iter()
            .filter(|v| !v.key)
            .filter_map(|v| {
                let s = v.at_return.as_ref()?;
                let shown = match out_reads {
                    Some(o) => show(s, &r.at_entry, Some(o)),
                    None => Shown::Unreadable(
                        r.lost
                            .clone()
                            .map(|l| format!("the return was not recorded: {l}"))
                            .unwrap_or_else(|| "the call had not returned".into()),
                    ),
                };
                Some((v.name.clone(), shown))
            })
            .collect();
        if !at_ret.is_empty() {
            let at = r.ret.unwrap_or(r.at);
            out.push(((r.boot, at, ri, 1), mk("return", at, at_ret)));
        }
    }
    out.sort_by_key(|(k, _)| *k);
    out.into_iter().map(|(_, p)| p).collect()
}

/// A key's family: a key that another key of the run starts, followed by `_`, `-`, `.`, `:`, `/`
/// or `#`, belongs to it (a queue copy `d17_q` of the transaction `d17`).
pub fn families(keys: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for k in keys {
        let mut best: Option<&String> = None;
        for o in keys {
            if o.len() < k.len()
                && k.starts_with(o.as_str())
                && k[o.len()..].starts_with(['_', '-', '.', ':', '/', '#'])
                && best.is_none_or(|b| o.len() < b.len())
            {
                best = Some(o);
            }
        }
        out.insert(k.clone(), best.unwrap_or(k).clone());
    }
    out
}

/// One change of a named value between two stops of a key's history.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub name: String,
    pub from: String,
    pub to: String,
    /// The index of the stop where the new value was seen, in the key's points.
    pub point: usize,
    /// The index of the stop with the value before.
    pub before: usize,
}

/// A key's history: its points in order, and every change of a value between consecutive
/// points that recorded it.
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    /// The key (none: the values of functions without a `key` line).
    pub key: Option<String>,
    /// Other keys of the family (`d17_q` under `d17`).
    pub copies: Vec<String>,
    pub names: Vec<String>,
    pub points: Vec<Point>,
    pub changes: Vec<Change>,
}

/// The histories of a run's points, one per key family, in the order the run first recorded
/// each key.
pub fn histories(points: Vec<Point>) -> Vec<History> {
    let keys: Vec<String> = points.iter().filter_map(|p| p.key.clone()).collect();
    let fam = families(&keys);
    let mut order: Vec<Option<String>> = Vec::new();
    let mut by: BTreeMap<Option<String>, History> = BTreeMap::new();
    for p in points {
        let k = p
            .key
            .as_ref()
            .map(|k| fam.get(k).cloned().unwrap_or(k.clone()));
        let h = by.entry(k.clone()).or_insert_with(|| {
            order.push(k.clone());
            History {
                key: k.clone(),
                copies: Vec::new(),
                names: Vec::new(),
                points: Vec::new(),
                changes: Vec::new(),
            }
        });
        if let Some(own) = &p.key {
            if Some(own) != k.as_ref() && !h.copies.contains(own) {
                h.copies.push(own.clone());
            }
        }
        for (n, _) in &p.values {
            if !h.names.contains(n) {
                h.names.push(n.clone());
            }
        }
        h.points.push(p);
    }
    let mut out = Vec::new();
    for k in order {
        let Some(mut h) = by.remove(&k) else { continue };
        let mut last: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for (pi, p) in h.points.iter().enumerate() {
            for (n, v) in &p.values {
                if matches!(v, Shown::Unreadable(_)) {
                    continue;
                }
                let t = v.to_string();
                if let Some((prev, at)) = last.get(n) {
                    if *prev != t {
                        h.changes.push(Change {
                            name: n.clone(),
                            from: prev.clone(),
                            to: t.clone(),
                            point: pi,
                            before: *at,
                        });
                    }
                }
                last.insert(n.clone(), (t, pi));
            }
        }
        out.push(h);
    }
    out
}
