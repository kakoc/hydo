//! The linearizability checker for a key-value register or compare-and-set object, checked per
//! key (P-compositionality) with the Wing-Gong-Lowe search and Lowe's memoization (as Porcupine
//! and Knossos do), and its witness: the earliest non-linearizable prefix, delta-debugged.
//!
//! The search order, the step count against the budget and the minimization are exactly those of
//! hydohist.py, so the verdicts (including "undecided" at the same budget) and the witnesses are
//! the same; only the representation is faster (interned values, bitsets, a fast hash).

use crate::events::{ret_key, Events, Op, Status};
use crate::fx::{splitmix, Memo};
use crate::fx::{FxHashMap as HashMap, FxHashSet as HashSet};
use crate::py::{self, BigInt, Crash, PyVal};
use crate::{CheckErr, CheckResult, Outcome};
use std::collections::BTreeMap;

const INF: usize = usize::MAX;
/// The value id of "absent" (nil).
const NIL: u32 = 0;

/// `hydohist.pdec`: percent-decode one token (%xx = the byte xx; the result read as UTF-8,
/// invalid bytes replaced). The token "%" alone is the empty string.
pub fn pdec(tok: &str) -> String {
    pdec_cow(tok).into_owned()
}

/// [`pdec`] without a copy when there is nothing to decode.
pub(crate) fn pdec_cow(tok: &str) -> std::borrow::Cow<'_, str> {
    if !tok.contains('%') {
        return std::borrow::Cow::Borrowed(tok);
    }
    if tok == "%" {
        return std::borrow::Cow::Borrowed("");
    }
    let b = tok.as_bytes();
    let mut raw = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00");
            raw.push(u8::from_str_radix(h, 16).unwrap_or(0));
            i += 3;
        } else {
            raw.push(b[i]);
            i += 1;
        }
    }
    std::borrow::Cow::Owned(String::from_utf8_lossy(&raw).into_owned())
}

/// `hydohist.penc`: percent-encode the bytes that would break a token ('%', whitespace, control
/// characters, '@' and any character in `extra`); the empty string becomes "%", the string
/// "nil" becomes "%6Eil".
pub fn penc(s: &str, extra: &str) -> String {
    if s.is_empty() {
        return "%".into();
    }
    if s == "nil" {
        return "%6Eil".into();
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '%' || ch == '@' || py::is_space(ch) || (ch as u32) < 0x20 || extra.contains(ch) {
            let mut buf = [0u8; 4];
            for x in ch.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{x:02X}"));
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Interned values: id 0 = nil, others index+1 into `strs`.
#[derive(Default)]
struct Values {
    strs: Vec<String>,
    ids: HashMap<String, u32>,
}

impl Values {
    fn id(&mut self, v: Option<String>) -> u32 {
        match v {
            None => NIL,
            Some(s) => {
                if let Some(&i) = self.ids.get(&s) {
                    return i;
                }
                self.strs.push(s.clone());
                let i = self.strs.len() as u32;
                self.ids.insert(s, i);
                i
            }
        }
    }

    fn text(&self, v: u32) -> &str {
        if v == NIL {
            "nil"
        } else {
            &self.strs[v as usize - 1]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RK {
    R,
    W,
    C,
}

/// A register operation on one key. `ver` is an order-preserving rank of the store version.
#[derive(Debug, Clone, Copy)]
struct ROp {
    op: usize,
    kind: RK,
    val: u32,
    ver: Option<u32>,
    old: u32,
    info: bool,
    call: usize,
    ret: usize,
}

/// The register's state: `None` = ANY (the initial value is unknown), else (value, version).
type State = Option<(u32, Option<u32>)>;

fn pack(s: State) -> u64 {
    match s {
        None => u64::MAX,
        Some((v, ver)) => ((v as u64) << 32) | ver.unwrap_or(u32::MAX) as u64,
    }
}

/// The register's sequential specification; None = the operation cannot take effect here.
fn reg_step(state: State, o: &ROp) -> Option<State> {
    let Some((sval, sver)) = state else {
        return Some(match o.kind {
            RK::R | RK::W => Some((o.val, o.ver)),
            RK::C if !o.info => Some((o.val, o.ver)),
            RK::C => None, // cas on an unknown value with an unknown outcome: still unknown
        });
    };
    match o.kind {
        RK::R => {
            if o.val != sval {
                return None;
            }
            if let Some(ov) = o.ver {
                match sver {
                    None => return Some(Some((sval, Some(ov)))),
                    Some(sv) if sv != ov => return None,
                    _ => {}
                }
            }
            Some(state)
        }
        RK::W => {
            if let (Some(ov), Some(sv)) = (o.ver, sver) {
                if ov <= sv {
                    return None;
                }
            }
            Some(Some((o.val, o.ver)))
        }
        RK::C => {
            if sval != o.old {
                return if o.info { Some(state) } else { None };
            }
            if let (Some(ov), Some(sv)) = (o.ver, sver) {
                if ov <= sv {
                    return None;
                }
            }
            Some(Some((o.val, o.ver)))
        }
    }
}

/// Wing-Gong-Lowe search with Lowe's memoization: Some(true) linearizable, Some(false) not,
/// None = the budget ran out.
fn wgl(rops: &[ROp], init: State, budget: i64) -> Option<bool> {
    let n = rops.len();
    if n == 0 {
        return Some(true);
    }
    let mut ev: Vec<(usize, u8, usize)> = Vec::with_capacity(2 * n);
    for (i, o) in rops.iter().enumerate() {
        ev.push((o.call, 0, i));
        ev.push((o.ret, 1, i));
    }
    ev.sort_by_key(|&(t, r, i)| (t, r, rops[i].call));
    let m = ev.len();
    let (head, tail) = (m, m + 1);
    let mut nxt = vec![0usize; m + 2];
    let mut prv = vec![0usize; m + 2];
    let mut is_call = vec![false; m];
    let mut opi = vec![0usize; m];
    let mut mat = vec![0usize; m];
    let mut callnode = vec![0usize; n];
    let mut retnode = vec![0usize; n];
    for (j, &(_, r, i)) in ev.iter().enumerate() {
        is_call[j] = r == 0;
        opi[j] = i;
        if r == 0 {
            callnode[i] = j;
        } else {
            retnode[i] = j;
        }
    }
    for i in 0..n {
        mat[callnode[i]] = retnode[i];
    }
    let order: Vec<usize> = std::iter::once(head).chain(0..m).chain([tail]).collect();
    for w in order.windows(2) {
        nxt[w[0]] = w[1];
        prv[w[1]] = w[0];
    }

    let words = n.div_ceil(64);
    // the memo: (linearized set, state) pairs, the set hashed incrementally (XOR of per-op keys)
    let mut cache = Memo::new(words);
    let zob: Vec<u64> = (0..n as u64).map(splitmix).collect();
    let mut h = 0u64;
    let mut calls: Vec<(usize, State)> = Vec::new();
    let mut state = init;
    let mut lin = vec![0u64; words];
    let mut entry = nxt[head];
    let mut steps: i64 = 0;
    while nxt[head] != tail {
        steps += 1;
        if steps > budget {
            return None;
        }
        if entry == tail {
            return Some(false);
        }
        if is_call[entry] {
            let i = opi[entry];
            if let Some(ns) = reg_step(state, &rops[i]) {
                lin[i / 64] |= 1 << (i % 64);
                if cache.insert(h ^ zob[i], &lin, pack(ns)) {
                    h ^= zob[i];
                    calls.push((entry, state));
                    state = ns;
                    // lift
                    let e = entry;
                    let r = mat[e];
                    nxt[prv[e]] = nxt[e];
                    prv[nxt[e]] = prv[e];
                    nxt[prv[r]] = nxt[r];
                    prv[nxt[r]] = prv[r];
                    entry = nxt[head];
                    continue;
                }
                lin[i / 64] &= !(1 << (i % 64));
            }
            entry = nxt[entry];
        } else {
            let Some((e, s)) = calls.pop() else {
                return Some(false);
            };
            state = s;
            let i = opi[e];
            lin[i / 64] &= !(1 << (i % 64));
            h ^= zob[i];
            // unlift
            let r = mat[e];
            nxt[prv[r]] = r;
            prv[nxt[r]] = r;
            nxt[prv[e]] = e;
            prv[nxt[e]] = e;
            entry = nxt[e];
        }
    }
    Some(true)
}

/// The history as it was known at position t: operations invoked before t; those still open at
/// t have an unknown outcome (reads dropped, writes/CAS may take effect later or never).
fn truncate(rops: &[ROp], t: usize) -> Vec<ROp> {
    let mut out = Vec::new();
    for o in rops {
        if o.call >= t {
            continue;
        }
        if o.ret != INF && o.ret < t {
            out.push(*o);
        } else if o.kind != RK::R {
            out.push(ROp {
                ver: None,
                ret: INF,
                info: true,
                ..*o
            });
        }
    }
    out
}

/// The first position at which the history known so far is already not linearizable (binary
/// search; monotone), or the full history if a search runs out of budget.
fn earliest_violation(rops: &[ROp], init: State, budget: i64) -> Vec<ROp> {
    let mut pts: Vec<usize> = rops.iter().map(|o| o.ret).filter(|&r| r != INF).collect();
    pts.sort_unstable();
    pts.dedup();
    let (mut lo, mut hi) = (0i64, pts.len() as i64 - 1);
    let mut best = None;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let tr = truncate(rops, pts[mid as usize] + 1);
        match wgl(&tr, init, budget) {
            None => return rops.to_vec(),
            Some(false) => {
                best = Some(tr);
                hi = mid - 1;
            }
            Some(true) => lo = mid + 1,
        }
    }
    best.unwrap_or_else(|| rops.to_vec())
}

/// A small witness: the earliest violating truncation, then delta debugging (remove chunks of
/// operations with the reads that observe the values the chunk's writes produced, while the rest
/// is still not linearizable).
fn minimize(rops: &[ROp], init: State, budget: i64) -> Vec<ROp> {
    let mut cur = earliest_violation(rops, init, budget);
    cur.sort_by_key(|o| o.call);
    let mut tries = 0;
    let mut size = (cur.len() / 2).max(1);
    // mark[v] == stamp: value v is produced by a write of the current chunk
    let nvals = cur
        .iter()
        .map(|o| o.val.max(o.old) as usize + 1)
        .max()
        .unwrap_or(1);
    let mut mark = vec![0u32; nvals];
    let mut stamp = 0u32;
    while tries < 3000 {
        let mut i = 0;
        let mut changed = false;
        while i < cur.len() && tries < 3000 {
            let end = (i + size).min(cur.len());
            stamp += 1;
            for o in cur[i..end].iter().filter(|o| o.kind != RK::R) {
                mark[o.val as usize] = stamp;
            }
            let has = |v: u32| mark[v as usize] == stamp;
            let cand: Vec<ROp> = cur
                .iter()
                .enumerate()
                .filter(|&(j, o)| {
                    let observer = (o.kind == RK::R && has(o.val))
                        || (o.kind == RK::C && !o.info && has(o.old));
                    !((i..end).contains(&j) || observer)
                })
                .map(|(_, o)| *o)
                .collect();
            tries += 1;
            if !cand.is_empty() && wgl(&cand, init, budget) == Some(false) {
                cur = cand;
                changed = true;
                continue;
            }
            i += size;
        }
        if size == 1 && !changed {
            break;
        }
        if !changed {
            size = (size / 2).max(1);
        }
    }
    cur
}

struct Ctx<'a> {
    ops: &'a [Op],
    ev: &'a Events,
    vals: Values,
    vers: Vec<BigInt>,
}

impl Ctx<'_> {
    fn vs(&self, val: u32, ver: Option<u32>) -> String {
        let mut s = self.vals.text(val).to_string();
        if let Some(v) = ver {
            s.push('@');
            s.push_str(&self.vers[v as usize].text());
        }
        s
    }

    fn describe(&self, key: &str, o: &ROp) -> Result<String, Crash> {
        let op = &self.ops[o.op];
        let what = match o.kind {
            RK::R => format!("read {key} -> {}", self.vs(o.val, o.ver)),
            RK::W => format!("write {key}={}", self.vs(o.val, o.ver)),
            RK::C => format!(
                "cas {key} {}->{}",
                self.vs(o.old, None),
                self.vs(o.val, o.ver)
            ),
        };
        Ok(format!(
            "p{} {what} ({}, {})",
            op.proc_,
            op.status.words(),
            op.where_(self.ev)?
        ))
    }

    fn explain(&self, key: &str, mins: &[ROp]) -> Result<String, Crash> {
        let ops: Vec<String> = mins
            .iter()
            .map(|o| self.describe(key, o))
            .collect::<Result<_, _>>()?;
        let ws: Vec<&ROp> = mins.iter().filter(|o| o.kind != RK::R).collect();
        let rs: Vec<&ROp> = mins.iter().filter(|o| o.kind == RK::R).collect();
        let head = if rs.len() == 1
            && !ws.is_empty()
            && ws.iter().all(|w| w.ret != INF && w.ret < rs[0].call)
        {
            let mut w = ws[0];
            for x in &ws[1..] {
                if x.ret > w.ret {
                    w = x;
                }
            }
            let r = rs[0];
            if r.val == w.val && r.ver.is_some() && w.ver.is_some() {
                format!(
                    "key {key}: the write was acknowledged with version {}, a later read returned version {}",
                    self.vers[w.ver.unwrap_or(0) as usize].text(),
                    self.vers[r.ver.unwrap_or(0) as usize].text()
                )
            } else {
                format!(
                    "key {key}: an acknowledged write is lost or a read is stale: a read that began after the write of {} completed returned {}{}",
                    self.vs(w.val, w.ver),
                    self.vs(r.val, r.ver),
                    if ws.len() > 1 { " (an older value)" } else { "" }
                )
            }
        } else {
            format!(
                "key {key}: these {} operations cannot be ordered consistently with real time and a register",
                mins.len()
            )
        };
        Ok(format!("{head}: {}", ops.join("; ")))
    }
}

/// value token -> (value or None for nil, version). 'v77@83' = value v77, version 83.
fn parse_val(tok: &str) -> Result<(Option<String>, Option<BigInt>), Crash> {
    if tok == "nil" {
        return Ok((None, None));
    }
    if let Some((v, ver)) = py::split_version(tok)? {
        return Ok((if v == "nil" { None } else { Some(pdec(v)) }, Some(ver)));
    }
    Ok((Some(pdec(tok)), None))
}

/// One register operation before its version is ranked.
struct Raw {
    op: usize,
    kind: RK,
    val: u32,
    ver: Option<BigInt>,
    old: u32,
}

fn register_ops(
    ops: &[Op],
    allow_cas: bool,
    vals: &mut Values,
) -> Result<BTreeMap<String, Vec<Raw>>, CheckErr> {
    let mut per: BTreeMap<String, Vec<Raw>> = BTreeMap::new();
    for op in ops {
        if op.status == Status::Fail {
            continue;
        }
        let a = op.args();
        let mut push = |k: String, r: Raw| per.entry(k).or_default().push(r);
        match op.f.as_str() {
            "read" => {
                if op.status != Status::Ok {
                    continue;
                }
                if a.is_empty() || a.len() % 2 == 1 {
                    return Err(CheckErr::Hist(format!(
                        "read by process {}: an ok read lists <key> <value> pairs (got {})",
                        op.proc_,
                        py::repr_strs(a)
                    )));
                }
                let mut seen: HashSet<String> = HashSet::default();
                for pair in a.chunks(2) {
                    let k = pdec(&pair[0]);
                    if !seen.insert(k.clone()) {
                        continue;
                    }
                    let (v, ver) = parse_val(&pair[1])?;
                    let val = vals.id(v);
                    push(
                        k,
                        Raw {
                            op: op.n,
                            kind: RK::R,
                            val,
                            ver,
                            old: NIL,
                        },
                    );
                }
            }
            "write" => {
                if a.len() < 2 {
                    return Err(CheckErr::Hist(format!(
                        "write by process {} needs <key> <value> (got {})",
                        op.proc_,
                        py::repr_strs(a)
                    )));
                }
                let (v, mut ver) = parse_val(&a[1])?;
                if op.status != Status::Ok {
                    ver = None;
                }
                let val = vals.id(v);
                push(
                    pdec(&a[0]),
                    Raw {
                        op: op.n,
                        kind: RK::W,
                        val,
                        ver,
                        old: NIL,
                    },
                );
            }
            "cas" => {
                if !allow_cas {
                    return Err(CheckErr::Hist(format!(
                        "cas by process {} in a history checked with model: register (use model: cas)",
                        op.proc_
                    )));
                }
                if a.len() < 3 {
                    return Err(CheckErr::Hist(format!(
                        "cas by process {} needs <key> <old> <new> (got {})",
                        op.proc_,
                        py::repr_strs(a)
                    )));
                }
                let (old, _) = parse_val(&a[1])?;
                let (v, mut ver) = parse_val(&a[2])?;
                if op.status != Status::Ok {
                    ver = None;
                }
                let old = vals.id(old);
                let val = vals.id(v);
                push(
                    pdec(&a[0]),
                    Raw {
                        op: op.n,
                        kind: RK::C,
                        val,
                        ver,
                        old,
                    },
                );
            }
            f => {
                return Err(CheckErr::Hist(format!(
                    "operation {} is not part of the register model (read, write, cas)",
                    py::repr_str(f)
                )))
            }
        }
    }
    Ok(per)
}

/// `hydohist.check_linearizable`.
pub fn check_linearizable(
    ops: &[Op],
    ev: &Events,
    spec: &PyVal,
    budget: i64,
) -> Result<CheckResult, CheckErr> {
    let model = spec.get("model").cloned().unwrap_or(PyVal::str("register"));
    if !(model.eq_str("register") || model.eq_str("cas")) {
        return Err(CheckErr::Hist(format!(
            "model {}: register or cas",
            model.repr()
        )));
    }
    let init: State = if spec.get("initial").is_some_and(|v| v.eq_str("any")) {
        None
    } else {
        Some((NIL, None))
    };
    let mut vals = Values::default();
    let raw = register_ops(ops, model.eq_str("cas"), &mut vals)?;
    // rank the versions (order-preserving), so the search compares small integers
    let mut vers: Vec<BigInt> = raw
        .values()
        .flatten()
        .filter_map(|r| r.ver.clone())
        .collect();
    vers.sort();
    vers.dedup();
    let rank: HashMap<&BigInt, u32> = vers
        .iter()
        .enumerate()
        .map(|(i, v)| (v, i as u32))
        .collect();
    let per: BTreeMap<String, Vec<ROp>> = raw
        .iter()
        .map(|(k, rs)| {
            let v = rs
                .iter()
                .map(|r| {
                    let op = &ops[r.op];
                    ROp {
                        op: r.op,
                        kind: r.kind,
                        val: r.val,
                        ver: r.ver.as_ref().map(|x| rank[x]),
                        old: r.old,
                        info: op.status == Status::Info,
                        call: op.call,
                        ret: ret_key(op.ret),
                    }
                })
                .collect();
            (k.clone(), v)
        })
        .collect();
    let ctx = Ctx {
        ops,
        ev,
        vals,
        vers,
    };
    let nops = ops.iter().filter(|o| o.status != Status::Fail).count();
    let mut undecided: Vec<&String> = Vec::new();
    let mut bad: Vec<&String> = Vec::new();
    for (key, rops) in &per {
        match wgl(rops, init, budget) {
            None => undecided.push(key),
            Some(false) => bad.push(key),
            Some(true) => {}
        }
    }
    let mut det: Vec<(String, PyVal)> = vec![
        ("ops".into(), PyVal::Int(BigInt::from_i64(nops as i64))),
        (
            "keys".into(),
            PyVal::Int(BigInt::from_i64(per.len() as i64)),
        ),
    ];
    let int = |x: usize| PyVal::Int(BigInt::from_i64(x as i64));
    if !bad.is_empty() {
        // the key whose minimal witness begins earliest (deterministic)
        let mut best: Option<(usize, &String, Vec<ROp>)> = None;
        for &key in &bad {
            let mins = minimize(&per[key], init, budget);
            let k = (mins[0].call, key);
            if best.as_ref().is_none_or(|b| k < (b.0, b.1)) {
                best = Some((k.0, key, mins));
            }
        }
        let (_, key, mins) = best.unwrap_or_else(|| unreachable!());
        det.push(("key".into(), PyVal::str(key)));
        det.push(("nonlinearizable_keys".into(), int(bad.len())));
        det.push(("witness_ops".into(), int(mins.len())));
        let mut summary = ctx.explain(key, &mins)?;
        if bad.len() != 1 {
            summary.push_str(&format!(" ({} keys not linearizable)", bad.len()));
        }
        let mut lines = vec![
            format!("not linearizable: {} of {} keys", bad.len(), per.len()),
            format!(
                "keys: {}",
                bad.iter()
                    .take(50)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            "witness (these operations alone cannot be ordered; operations are removed while that stays true):".into(),
        ];
        for o in &mins {
            lines.push(format!("  {}", ctx.describe(key, o)?));
        }
        return Ok(CheckResult {
            status: Outcome::Violation,
            details: det,
            summary,
            explanation: lines.join("\n"),
            warnings: Vec::new(),
        });
    }
    if !undecided.is_empty() {
        det.push(("undecided_keys".into(), int(undecided.len())));
        return Ok(CheckResult {
            status: Outcome::Undecided,
            details: det,
            summary: format!(
                "search budget exhausted on {} keys (first {})",
                undecided.len(),
                undecided[0]
            ),
            explanation: format!(
                "undecided keys: {}",
                undecided
                    .iter()
                    .take(50)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            warnings: Vec::new(),
        });
    }
    Ok(CheckResult {
        status: Outcome::Ok,
        details: det,
        summary: format!("{nops} operations on {} keys linearizable", per.len()),
        explanation: String::new(),
        warnings: Vec::new(),
    })
}
