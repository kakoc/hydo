//! `linearizable`: whether a channel's sends and a group's receives can be ordered, consistently
//! with real time, as the operations of one queue: a multiset (`unordered`) or a FIFO queue.
//!
//! Checking linearizability is NP-complete in general; when every value is sent once (message
//! ids are unique by construction) a queue history is linearizable when it shows none of a few
//! patterns, each found in O(n log n) over the operations' positions:
//!
//! - **fresh**: a value received that was never sent, or whose every send began after the
//!   receive returned;
//! - **repeated**: a value received twice;
//! - **ordered** (FIFO only): v1's send completed before v2's send began, and v2 was received
//!   before v1's receive began (or v2 was received and v1, surely still in the queue, never was);
//! - **empty**: an empty receive while, at every point of its interval, some value was surely in
//!   the queue (sent before that point and not yet received; one value or several in turn).
//!
//! A value is surely in the queue from its send's completion to its receive's invocation; a
//! receive with an unknown outcome may have taken any value after it was invoked. Each witness is
//! confirmed by an exhaustive search over its few operations (the sub-history of the witness's
//! values is not linearizable, so neither is the whole history); a witness the search can order
//! is not reported. The test `messages::queue_patterns_match_search` compares the patterns with
//! the exhaustive search on generated small histories.

use super::hist::{MOp, OpKind, Out};
use super::index::{Gk, Ix};
use super::{Claim, Status, Witness};
use std::collections::{BTreeMap, HashSet};

/// One operation of a queue history, by position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QOp {
    pub inv: usize,
    /// usize::MAX: an unknown outcome (may take effect at any later time, or never).
    pub ret: usize,
    pub kind: QKind,
    /// The history operation it came from.
    pub op: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QKind {
    Enq(String),
    /// A successful receive: the values in the order it returned them (empty: an empty receive).
    Deq(Vec<String>),
    /// A receive with an unknown outcome: it may have taken values, or none.
    DeqUnknown,
}

impl QOp {
    fn known(&self) -> bool {
        self.ret != usize::MAX
    }
}

/// The queue operations of channel `g.0` seen by group `g`.
pub fn ops_of(ix: &Ix<'_>, g: &Gk) -> Vec<QOp> {
    let mut v = Vec::new();
    for op in &ix.h.ops {
        if op.q != g.0 {
            continue;
        }
        match op.kind {
            OpKind::Send => {
                for it in &op.items {
                    let out = op.item_out(it);
                    if out == Out::Fail {
                        continue;
                    }
                    v.push(QOp {
                        inv: op.inv.pos,
                        ret: if out == Out::Ok {
                            op.ret_pos()
                        } else {
                            usize::MAX
                        },
                        kind: QKind::Enq(it.m.clone()),
                        op: op.i,
                    });
                }
            }
            OpKind::Recv if op.g == g.1 => v.push(recv(op)),
            _ => {}
        }
    }
    v
}

fn recv(op: &MOp) -> QOp {
    match op.out {
        Out::Ok => QOp {
            inv: op.inv.pos,
            ret: op.ret_pos(),
            kind: QKind::Deq(op.items.iter().map(|i| i.m.clone()).collect()),
            op: op.i,
        },
        // a failed receive took nothing
        Out::Fail => QOp {
            inv: op.inv.pos,
            ret: op.ret_pos(),
            kind: QKind::Deq(Vec::new()),
            op: op.i,
        },
        Out::Info => QOp {
            inv: op.inv.pos,
            ret: usize::MAX,
            kind: QKind::DeqUnknown,
            op: op.i,
        },
    }
}

/// A pattern found: its name, one sentence, and the indices (into the ops) of its operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub pattern: &'static str,
    pub text: String,
    pub ops: Vec<usize>,
}

/// The first pattern of a queue history, or None (linearizable when the history's values are
/// unique). `fifo`: a FIFO queue, else a multiset. Failed receives must be given as empty
/// successful ones only when they were (callers pass [`ops_of`]'s list; a failed receive is
/// left out by [`check_ops`]).
pub fn patterns(ops: &[QOp], fifo: bool) -> Option<Found> {
    // values: their enqueues (live), their dequeues
    let mut enq: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut deq: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new(); // (op, place)
    for (i, o) in ops.iter().enumerate() {
        match &o.kind {
            QKind::Enq(v) => enq.entry(v).or_default().push(i),
            QKind::Deq(vs) => {
                for (j, v) in vs.iter().enumerate() {
                    deq.entry(v).or_default().push((i, j));
                }
            }
            QKind::DeqUnknown => {}
        }
    }
    let first_unknown = ops
        .iter()
        .filter(|o| o.kind == QKind::DeqUnknown)
        .map(|o| o.inv)
        .min()
        .unwrap_or(usize::MAX);
    // fresh, repeated
    for (v, ds) in &deq {
        let (d, _) = ds[0];
        let es = enq.get(v).cloned().unwrap_or_default();
        if es.is_empty() || es.iter().all(|&e| ops[e].inv > ops[d].ret) {
            let mut w = es.clone();
            w.push(d);
            return Some(Found {
                pattern: "fresh",
                text: if es.is_empty() {
                    format!("{v} was received but never sent")
                } else {
                    format!("{v} was received before any send of it began")
                },
                ops: w,
            });
        }
        if ds.len() > 1 {
            return Some(Found {
                pattern: "repeated",
                text: format!("{v} was received twice"),
                ops: vec![ds[0].0, ds[1].0],
            });
        }
    }
    // the surest enqueue completion of each value: its earliest ok send's completion
    let enq_ret = |v: &str| -> usize {
        enq.get(v)
            .map(|es| es.iter().map(|&e| ops[e].ret).min().unwrap_or(usize::MAX))
            .unwrap_or(usize::MAX)
    };
    let enq_inv = |v: &str| -> usize {
        enq.get(v)
            .map(|es| es.iter().map(|&e| ops[e].inv).min().unwrap_or(usize::MAX))
            .unwrap_or(usize::MAX)
    };
    if fifo {
        // ordered: v1 -> v2 (v1's enqueue completed before v2's began) and v2 left first
        let mut preds: Vec<(usize, &str)> = enq
            .keys()
            .map(|v| (enq_ret(v), *v))
            .filter(|x| x.0 != usize::MAX)
            .collect();
        preds.sort();
        let mut succs: Vec<(usize, &str)> = deq.keys().map(|v| (enq_inv(v), *v)).collect();
        succs.sort();
        let mut pi = 0;
        let mut best: Option<(usize, &str)> = None; // latest dequeue invocation of a predecessor
        let mut undelivered: Option<&str> = None;
        for (sinv, v2) in &succs {
            while pi < preds.len() && preds[pi].0 < *sinv {
                let v1 = preds[pi].1;
                match deq.get(v1) {
                    Some(ds) => {
                        let inv = ops[ds[0].0].inv;
                        if best.is_none_or(|b| inv > b.0) {
                            best = Some((inv, v1));
                        }
                    }
                    None => {
                        if undelivered.is_none() {
                            undelivered = Some(v1);
                        }
                    }
                }
                pi += 1;
            }
            let (d2, _) = deq[v2][0];
            let r2 = ops[d2].ret;
            if let Some((inv1, v1)) = best {
                if r2 < inv1 {
                    let mut w = enq[v1].clone();
                    w.extend(enq[v2].iter().copied());
                    w.push(d2);
                    w.push(deq[v1][0].0);
                    return Some(Found {
                        pattern: "ordered",
                        text: format!(
                            "{v1} was sent before {v2}, and {v2} was received before the receive of {v1} began"
                        ),
                        ops: w,
                    });
                }
            }
            if let Some(v1) = undelivered {
                if first_unknown > r2 {
                    let mut w = enq[v1].clone();
                    w.extend(enq[v2].iter().copied());
                    w.push(d2);
                    return Some(Found {
                        pattern: "ordered",
                        text: format!(
                            "{v1} was sent before {v2} and never received, while {v2} was received"
                        ),
                        ops: w,
                    });
                }
            }
        }
        // the same receive: a value received earlier in a batch than one sent before it
        for o in ops.iter().enumerate() {
            if let QKind::Deq(vs) = &o.1.kind {
                for (j, v2) in vs.iter().enumerate() {
                    for v1 in &vs[j + 1..] {
                        if enq_ret(v1) < enq_inv(v2) {
                            let mut w = enq[v1.as_str()].clone();
                            w.extend(enq[v2.as_str()].iter().copied());
                            w.push(o.0);
                            return Some(Found {
                                pattern: "ordered",
                                text: format!(
                                    "{v1} was sent before {v2}, and one receive returned {v2} before {v1}"
                                ),
                                ops: w,
                            });
                        }
                    }
                }
            }
        }
    }
    // empty: the intervals in which values are surely in the queue
    let mut pres: Vec<(usize, usize, &str)> = Vec::new();
    for v in enq.keys() {
        let a = enq_ret(v);
        if a == usize::MAX {
            continue;
        }
        let b = deq
            .get(v)
            .map(|ds| ops[ds[0].0].inv)
            .unwrap_or(usize::MAX)
            .min(first_unknown);
        if a < b {
            pres.push((a, b, v));
        }
    }
    pres.sort();
    for (i, o) in ops.iter().enumerate() {
        let QKind::Deq(vs) = &o.kind else { continue };
        if !vs.is_empty() || !o.known() {
            continue;
        }
        // greedy cover of [inv, ret] by closed intervals
        let (x, y) = (o.inv, o.ret);
        let mut reach = x;
        let mut chain: Vec<&str> = Vec::new();
        let mut k = 0;
        let mut best: Option<(usize, &str)> = None;
        loop {
            while k < pres.len() && pres[k].0 <= reach {
                if best.is_none_or(|b| pres[k].1 > b.0) {
                    best = Some((pres[k].1, pres[k].2));
                }
                k += 1;
            }
            match best {
                Some((b, v)) if b > reach => {
                    chain.push(v);
                    reach = b;
                    best = None;
                    if reach >= y {
                        break;
                    }
                }
                _ => break,
            }
        }
        if reach >= y && !chain.is_empty() && pres.iter().any(|p| p.0 <= x) {
            let mut w = Vec::new();
            for v in &chain {
                w.extend(enq[*v].iter().copied());
                if let Some(ds) = deq.get(*v) {
                    w.push(ds[0].0);
                }
            }
            w.push(i);
            return Some(Found {
                pattern: "empty",
                text: format!(
                    "a receive returned nothing while {} surely in the queue for its whole duration",
                    if chain.len() == 1 {
                        format!("{} was", chain[0])
                    } else {
                        format!("{} were in turn", chain.join(", "))
                    }
                ),
                ops: w,
            });
        }
    }
    None
}

/// The exhaustive search: whether the operations can be ordered, consistently with real time, as
/// one queue. Unknown outcomes may take effect at any later time or never; an unknown receive
/// takes up to two values. Exponential: for small histories and witnesses only.
pub fn search(ops: &[QOp], fifo: bool) -> bool {
    assert!(
        ops.len() <= 24,
        "the exhaustive search is for small histories"
    );
    let mut seen: HashSet<(u32, Vec<String>)> = HashSet::new();
    rec(ops, fifo, 0, &mut Vec::new(), &mut seen)
}

fn rec(
    ops: &[QOp],
    fifo: bool,
    done: u32,
    q: &mut Vec<String>,
    seen: &mut HashSet<(u32, Vec<String>)>,
) -> bool {
    let pending: Vec<usize> = (0..ops.len()).filter(|i| done & (1 << i) == 0).collect();
    if pending.iter().all(|&i| !ops[i].known()) {
        return true;
    }
    let mut key_q = q.clone();
    if !fifo {
        key_q.sort();
    }
    if !seen.insert((done, key_q)) {
        return false;
    }
    let min_ret = pending
        .iter()
        .map(|&i| ops[i].ret)
        .min()
        .unwrap_or(usize::MAX);
    for &i in &pending {
        if ops[i].inv > min_ret {
            continue;
        }
        let nd = done | (1 << i);
        match &ops[i].kind {
            QKind::Enq(v) => {
                q.push(v.clone());
                if rec(ops, fifo, nd, q, seen) {
                    return true;
                }
                q.pop();
            }
            QKind::Deq(vs) => {
                let saved = q.clone();
                let ok = if vs.is_empty() {
                    q.is_empty()
                } else if fifo {
                    q.len() >= vs.len() && q[..vs.len()] == vs[..] && {
                        q.drain(..vs.len());
                        true
                    }
                } else {
                    let mut ok = true;
                    for v in vs {
                        match q.iter().position(|x| x == v) {
                            Some(p) => {
                                q.remove(p);
                            }
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    ok
                };
                if ok && rec(ops, fifo, nd, q, seen) {
                    return true;
                }
                *q = saved;
            }
            QKind::DeqUnknown => {
                // takes nothing, one value, or two
                let saved = q.clone();
                let mut choices: Vec<Vec<usize>> = vec![vec![]];
                if fifo {
                    for k in 1..=q.len().min(2) {
                        choices.push((0..k).collect());
                    }
                } else {
                    for a in 0..q.len() {
                        choices.push(vec![a]);
                        for b in a + 1..q.len() {
                            choices.push(vec![a, b]);
                        }
                    }
                }
                for c in choices {
                    let mut nq: Vec<String> = Vec::new();
                    for (j, x) in saved.iter().enumerate() {
                        if !c.contains(&j) {
                            nq.push(x.clone());
                        }
                    }
                    *q = nq;
                    if rec(ops, fifo, nd, q, seen) {
                        *q = saved;
                        return true;
                    }
                }
                *q = saved;
            }
        }
    }
    false
}

/// The claim: per group of each channel.
pub fn claim(ix: &Ix<'_>) -> Claim {
    let fifo = ix.d.linearizable == super::decl::Linearizable::Fifo;
    let mut judged = 0;
    let mut unconfirmed = 0;
    for g in ix.groups.keys() {
        let ops = ops_of(ix, g);
        if ops.is_empty() {
            continue;
        }
        judged += 1;
        let Some(f) = patterns(&ops, fifo) else {
            continue;
        };
        // confirm on the witness's operations
        let mut sub: Vec<QOp> = Vec::new();
        let mut idx: Vec<usize> = f.ops.clone();
        idx.sort();
        idx.dedup();
        for &i in &idx {
            sub.push(ops[i].clone());
        }
        // unknown receives may matter to the witness: keep up to four
        for o in ops.iter().filter(|o| o.kind == QKind::DeqUnknown).take(4) {
            if !sub.contains(o) {
                sub.push(o.clone());
            }
        }
        // a receive in the witness may return values whose sends are not in it: add them
        let sent: Vec<String> = sub
            .iter()
            .filter_map(|o| match &o.kind {
                QKind::Enq(v) => Some(v.clone()),
                _ => None,
            })
            .collect();
        let mut extra = Vec::new();
        for o in &sub {
            if let QKind::Deq(vs) = &o.kind {
                for v in vs {
                    if !sent.contains(v) {
                        extra.extend(
                            ops.iter()
                                .filter(|x| x.kind == QKind::Enq(v.clone()))
                                .cloned(),
                        );
                    }
                }
            }
        }
        sub.extend(extra);
        if sub.len() <= 22 && search(&sub, fifo) {
            unconfirmed += 1;
            continue;
        }
        let mut w = Witness::default();
        for o in &sub {
            w.op(ix, o.op);
        }
        return Claim {
            name: "linearizable",
            status: Status::Violated,
            judged,
            undecided: unconfirmed,
            summary: format!(
                "channel {} as group {} sees it is not one {} queue ({}): {}",
                g.0,
                g.1,
                if fifo { "FIFO" } else { "unordered" },
                f.pattern,
                f.text
            ),
            witness: w,
        };
    }
    Claim {
        name: "linearizable",
        status: Status::Held,
        judged,
        undecided: unconfirmed,
        summary: format!(
            "the sends and receives are linearizable as one {} queue per group ({judged} judged, {unconfirmed} undecided)",
            if fifo { "FIFO" } else { "unordered" }
        ),
        witness: Witness::default(),
    }
}
