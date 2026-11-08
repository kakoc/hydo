//! The measures (docs/design/messaging.md "Measures"): counts and distributions computed for
//! every messages check, whatever it claims. Durations are guest milliseconds between records of
//! one boot; pairs across a power cut are left out and counted (`across_boots`).

use super::claims::{fate, Excuse, Fate};
use super::decl::{quant, Lease};
use super::hist::{span, OpKind, Out};
use super::index::{Gk, Ix};
use crate::py::{BigInt, PyVal};
use std::collections::{BTreeMap, BTreeSet};

fn n(x: u64) -> PyVal {
    PyVal::Int(BigInt::from_i64(x as i64))
}

fn dict(v: Vec<(&str, PyVal)>) -> PyVal {
    PyVal::Dict(v.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn ms(ns: i128) -> f64 {
    // milliseconds, rounded to the microsecond so that the text is stable
    ((ns as f64) / 1e3).round() / 1e3
}

fn num(f: f64) -> PyVal {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        PyVal::Int(BigInt::from_i64(f as i64))
    } else {
        PyVal::Float(f)
    }
}

/// count, then the declared quantiles (nearest rank) as `q<quantile>` keys.
fn dist(mut xs: Vec<f64>, qs: &[f64]) -> PyVal {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut v = vec![("count".to_string(), n(xs.len() as u64))];
    if !xs.is_empty() {
        for q in qs {
            let rank = ((q * xs.len() as f64).ceil() as usize).clamp(1, xs.len());
            let key = format!("q{}", crate::py::dumps_sorted(&quant(*q)));
            v.push((key, num(xs[rank - 1])));
        }
    }
    PyVal::Dict(v)
}

fn gname(g: &Gk) -> PyVal {
    dict(vec![
        ("channel", PyVal::str(&g.0)),
        ("group", PyVal::str(&g.1)),
    ])
}

pub fn measures(ix: &Ix<'_>) -> PyVal {
    let qs = &ix.d.measures.quantiles;
    let in_scope = |q: &str| {
        ix.d.measures
            .channels
            .as_ref()
            .is_none_or(|c| c.iter().any(|x| x == q))
    };
    let mut across = 0u64;
    // sends
    let (mut sent, mut ok, mut fail, mut info, mut recovered) = (0, 0, 0, 0, 0);
    for m in ix.msgs.values().filter(|m| in_scope(&m.q)) {
        for s in &m.sends {
            sent += 1;
            match ix.send_out(*s) {
                Out::Ok => ok += 1,
                Out::Fail => fail += 1,
                Out::Info => info += 1,
            }
        }
        let only_info = m.sends.iter().all(|s| ix.send_out(*s) != Out::Ok)
            && m.sends.iter().any(|s| ix.send_out(*s) == Out::Info);
        if only_info && ix.dl.iter().any(|x| x.m == m.m) {
            recovered += 1;
        }
    }
    // lost, excused, unseen
    let mut lost: BTreeMap<Gk, Vec<String>> = BTreeMap::new();
    let mut excused: BTreeMap<&str, u64> = BTreeMap::new();
    let mut unseen = 0u64;
    let mut undecided_loss = 0u64;
    for m in ix.msgs.values().filter(|m| in_scope(&m.q)) {
        if !ix.acknowledged(m) {
            continue;
        }
        let mut missing = false;
        for g in ix.must_groups(m) {
            if ix.deliveries(&m.m, &g).is_empty() {
                missing = true;
            }
            if ix.groups[&g].drained.is_none() {
                continue;
            }
            match fate(ix, m, &g) {
                Fate::Lost => lost.entry(g).or_default().push(m.m.clone()),
                Fate::Excused(e) => *excused.entry(e.name()).or_default() += 1,
                Fate::Undecided => undecided_loss += 1,
                Fate::Delivered => {}
            }
        }
        if missing {
            unseen += 1;
        }
    }
    let lost_v = PyVal::List(
        lost.iter()
            .map(|(g, ids)| {
                let mut d = gname(g);
                d.set("count", n(ids.len() as u64));
                d.set(
                    "first",
                    PyVal::List(ids.iter().take(20).map(|x| PyVal::str(x)).collect()),
                );
                d
            })
            .collect(),
    );
    let mut exc = Vec::new();
    for e in [
        Excuse::Expired,
        Excuse::DeadLettered,
        Excuse::PossiblyConsumed,
        Excuse::Rejected,
    ] {
        exc.push((e.name(), n(*excused.get(e.name()).unwrap_or(&0))));
    }
    exc.push(("undecided", n(undecided_loss)));
    // unexpected, aborted
    let (mut unexpected, mut aborted) = (0, 0);
    for x in ix.dl.iter().filter(|x| in_scope(&x.g.0)) {
        match ix.msgs.get(&x.m) {
            None => unexpected += 1,
            Some(m) if m.sends.iter().all(|s| ix.send_out(*s) == Out::Fail) => aborted += 1,
            _ => {}
        }
    }
    // deliveries per (message, group)
    let mut total = 0u64;
    let mut hist = [0u64; 4];
    let mut most = 0u64;
    let mut delivered = 0u64;
    for ((_, g), ds) in ix.by_mg.iter().filter(|((_, g), _)| in_scope(&g.0)) {
        let _ = g;
        total += ds.len() as u64;
        delivered += 1;
        most = most.max(ds.len() as u64);
        hist[(ds.len() - 1).min(3)] += 1;
    }
    let rate = if delivered > 0 {
        ((total - delivered) as f64 / delivered as f64 * 1e6).round() / 1e6
    } else {
        0.0
    };
    // redeliveries by cause
    let mut cause: BTreeMap<&str, u64> = BTreeMap::new();
    for k in [
        "after-ack",
        "after-nack",
        "after-lease",
        "during-lease",
        "undecided",
    ] {
        cause.insert(k, 0);
    }
    let lease_ns = |d: usize| -> Option<i128> {
        match ix.d.lease {
            Lease::Ms(x) => Some(x as i128 * 1_000_000),
            Lease::PerDelivery => ix.dlv_item(d).v.map(|v| v as i128 * 1_000_000),
            Lease::None => None,
        }
    };
    let mut invisible: Vec<f64> = Vec::new();
    let mut invisible_ratio: Vec<f64> = Vec::new();
    for ((m, g), ds) in ix.by_mg.iter().filter(|((_, g), _)| in_scope(&g.0)) {
        let acks = ix.acks_of(m, g);
        for w in ds.windows(2) {
            let (d1, d2) = (w[0], w[1]);
            let r2 = ix.dlv_op(d2);
            let ret1 = ix.dlv_ret(d1);
            let ret2 = ix.dlv_ret(d2);
            let acked = acks.iter().any(|a| {
                a.kind == OpKind::Ack && a.out == Out::Ok && ix.op(a.op).ret_pos() < r2.inv.pos
            });
            let nacked = acks.iter().any(|a| {
                a.kind == OpKind::Nack
                    && a.dlv == Some(d1)
                    && a.out != Out::Fail
                    && ix.op(a.op).inv.pos < ret2.pos
            });
            let c = if acked {
                "after-ack"
            } else if nacked {
                "after-nack"
            } else {
                match (
                    lease_ns(d1),
                    span(&ix.dlv_op(d1).inv, &ret2),
                    span(&ret1, &r2.inv),
                ) {
                    (Some(l), Some(early), _) if early < l => "during-lease",
                    (Some(l), _, Some(late)) if late >= l => "after-lease",
                    _ => "undecided",
                }
            };
            *cause.get_mut(c).expect("a cause") += 1;
            if !acked {
                match span(&ret1, &ret2) {
                    Some(dt) => {
                        invisible.push(ms(dt));
                        if let Some(l) = lease_ns(d1) {
                            invisible_ratio.push((dt as f64 / l as f64 * 1e6).round() / 1e6);
                        }
                    }
                    None => across += 1,
                }
            }
        }
    }
    // stored copies, processing, group overlap
    let mut stored = 0u64;
    for m in ix.msgs.values().filter(|m| in_scope(&m.q)) {
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        for s in &m.sends {
            if ix.send_out(*s) == Out::Ok {
                if let Some(o) = &ix.send_item(*s).o {
                    ids.insert(o);
                }
            }
        }
        for x in ix.dl.iter().enumerate().filter(|(_, x)| x.m == m.m) {
            if let Some(o) = &ix.dlv_item(x.0).o {
                ids.insert(o);
            }
        }
        if ids.len() > 1 {
            stored += 1;
        }
    }
    let (mut processing, mut overlap) = (0u64, 0u64);
    let mut held = Vec::new();
    for ((m, g), acks) in ix.acks.iter().filter(|((_, g), _)| in_scope(&g.0)) {
        let ok: Vec<_> = acks
            .iter()
            .filter(|a| a.kind == OpKind::Ack && a.out == Out::Ok)
            .collect();
        let dl: BTreeSet<usize> = ok.iter().filter_map(|a| a.dlv).collect();
        if dl.len() > 1 {
            processing += 1;
        }
        for a in &ok {
            if let (Some(d), Some(ret)) = (a.dlv, ix.op(a.op).ret) {
                match span(&ix.dlv_ret(d), &ret) {
                    Some(dt) => held.push(ms(dt)),
                    None => across += 1,
                }
            }
        }
        // two members surely holding the message at once: received, not yet released
        let ds = ix.deliveries(m, g);
        let release = |d: usize| -> usize {
            acks.iter()
                .filter(|a| a.dlv == Some(d) && a.kind != OpKind::Extend && a.out != Out::Fail)
                .map(|a| ix.op(a.op).inv.pos)
                .min()
                .unwrap_or(0)
        };
        let mut found = false;
        for (i, &d1) in ds.iter().enumerate() {
            for &d2 in &ds[i + 1..] {
                if ix.dlv_op(d1).proc_ == ix.dlv_op(d2).proc_ {
                    continue;
                }
                let (h1, h2) = (release(d1), release(d2));
                if h1 > 0 && h2 > 0 && ix.dlv_ret(d2).pos < h1 && ix.dlv_ret(d1).pos < h2 {
                    found = true;
                }
            }
        }
        if found {
            overlap += 1;
        }
    }
    // reorders per group: inverted pairs of first deliveries, and the largest distance
    let mut reorders = Vec::new();
    for g in ix.groups.keys().filter(|g| in_scope(&g.0)) {
        let (pairs, dist_max) = inversions(ix, g);
        if pairs > 0 || dist_max > 0 {
            let mut d = gname(g);
            d.set("inverted_pairs", n(pairs));
            d.set("max_distance", n(dist_max));
            reorders.push(d);
        }
    }
    // latencies
    let mut first_delivery = Vec::new();
    let mut final_ack = Vec::new();
    for m in ix.msgs.values().filter(|m| in_scope(&m.q)) {
        let Some(s) = ix.first_ok(m) else { continue };
        let Some(sret) = ix.op(s.op).ret else {
            continue;
        };
        for g in ix.groups.keys().filter(|g| g.0 == m.q) {
            if let Some(&d) = ix.deliveries(&m.m, g).first() {
                match span(&sret, &ix.dlv_ret(d)) {
                    Some(dt) => first_delivery.push(ms(dt.max(0))),
                    None => across += 1,
                }
            }
            let first_ack = ix
                .acks_of(&m.m, g)
                .iter()
                .filter(|a| a.kind == OpKind::Ack && a.out == Out::Ok)
                .filter_map(|a| ix.op(a.op).ret)
                .min_by_key(|r| r.pos);
            if let Some(r) = first_ack {
                match span(&sret, &r) {
                    Some(dt) => final_ack.push(ms(dt.max(0))),
                    None => across += 1,
                }
            }
        }
    }
    // availability per operation
    let mut avail: BTreeMap<&str, [u64; 3]> = BTreeMap::new();
    for op in ix.h.ops.iter().filter(|o| in_scope(&o.q)) {
        let e = avail.entry(op.kind.name()).or_default();
        e[match op.out {
            Out::Ok => 0,
            Out::Fail => 1,
            Out::Info => 2,
        }] += 1;
    }
    let mut out = vec![
        (
            "sends",
            dict(vec![
                ("invoked", n(sent)),
                ("ok", n(ok)),
                ("fail", n(fail)),
                ("info", n(info)),
                ("recovered", n(recovered)),
            ]),
        ),
        ("lost", lost_v),
        ("excused", dict(exc)),
        ("unexpected", n(unexpected)),
        ("aborted", n(aborted)),
        (
            "deliveries",
            dict(vec![
                ("total", n(total)),
                ("messages", n(delivered)),
                ("duplicate_rate", num(rate)),
                ("max_per_message", n(most)),
                (
                    "histogram",
                    dict(vec![
                        ("1", n(hist[0])),
                        ("2", n(hist[1])),
                        ("3", n(hist[2])),
                        ("4+", n(hist[3])),
                    ]),
                ),
            ]),
        ),
        (
            "redeliveries",
            PyVal::Dict(cause.iter().map(|(k, v)| (k.to_string(), n(*v))).collect()),
        ),
        ("stored_copies", n(stored)),
        ("processing", n(processing)),
        ("group_overlap", n(overlap)),
        ("reorders", PyVal::List(reorders)),
        (
            "latency",
            dict(vec![
                ("first_delivery", dist(first_delivery, qs)),
                ("final_ack", dist(final_ack, qs)),
            ]),
        ),
        ("held", dist(held, qs)),
        (
            "invisible",
            dict(vec![
                ("ms", dist(invisible, qs)),
                ("lease_ratio", dist(invisible_ratio, qs)),
            ]),
        ),
        ("unseen", n(unseen)),
        (
            "availability",
            PyVal::Dict(
                avail
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.to_string(),
                            dict(vec![("ok", n(v[0])), ("fail", n(v[1])), ("info", n(v[2]))]),
                        )
                    })
                    .collect(),
            ),
        ),
    ];
    out.push(("across_boots", n(across)));
    dict(out)
}

/// Inverted pairs of first deliveries in a group, by the claim's precedence (a send completed ok
/// before the other's was invoked) within the declared order scope (the key when no order is
/// declared), and the largest number of messages that follow one and were first delivered
/// before it.
fn inversions(ix: &Ix<'_>, g: &Gk) -> (u64, u64) {
    let scope_channel = ix.d.order.scope == super::decl::Scope::Channel
        && ix.d.order.kind != super::decl::OrderKind::None;
    let mut by_scope: BTreeMap<String, Vec<Prec>> = BTreeMap::new();
    for m in ix.msgs.values().filter(|m| m.q == g.0) {
        let (Some(a), Some(b)) = (ix.first_ok(m), ix.first_live(m)) else {
            continue;
        };
        let Some(&d) = ix.deliveries(&m.m, g).first() else {
            continue;
        };
        let sc = if scope_channel {
            String::new()
        } else {
            m.k.clone()
        };
        by_scope.entry(sc).or_default().push((
            ix.op(a.op).ret_pos(),
            ix.op(b.op).inv.pos,
            ix.dlv_key(d),
        ));
    }
    let (mut pairs, mut most) = (0u64, 0u64);
    for v in by_scope.values() {
        let (p, m) = count_inversions(v);
        pairs += p;
        most = most.max(m);
    }
    (pairs, most)
}

/// A message in a group's order: (first ok send completion, first live send invocation, first
/// delivery's (receive completion, place)).
pub(crate) type Prec = (usize, usize, (usize, usize));

/// A Fenwick tree of counts.
struct Bit(Vec<u64>);

impl Bit {
    fn add(&mut self, mut i: usize) {
        i += 1;
        while i < self.0.len() {
            self.0[i] += 1;
            i += i & i.wrapping_neg();
        }
    }
    /// The count of indices below i.
    fn below(&self, mut i: usize) -> u64 {
        let mut s = 0;
        while i > 0 {
            s += self.0[i];
            i -= i & i.wrapping_neg();
        }
        s
    }
}

/// (ok completion, live invocation, first delivery) per message: the pairs a -> b (a.0 < b.1)
/// with b delivered first (b.2 < a.2), and the most such b for one a. O(n log n).
pub(crate) fn count_inversions(v: &[Prec]) -> (u64, u64) {
    let mut keys: Vec<(usize, usize)> = v.iter().map(|x| x.2).collect();
    keys.sort();
    let rank = |k: (usize, usize)| keys.partition_point(|x| *x < k);
    let len = v.len();
    // pairs: for each b by invocation, the a's completed before it and delivered after it
    let mut by_ret: Vec<usize> = (0..len).collect();
    by_ret.sort_by_key(|&i| v[i].0);
    let mut by_inv: Vec<usize> = (0..len).collect();
    by_inv.sort_by_key(|&i| v[i].1);
    let mut bit = Bit(vec![0; len + 1]);
    let (mut pairs, mut k, mut inserted) = (0u64, 0usize, 0u64);
    for &b in &by_inv {
        while k < len && v[by_ret[k]].0 < v[b].1 {
            bit.add(rank(v[by_ret[k]].2));
            inserted += 1;
            k += 1;
        }
        pairs += inserted - bit.below(rank(v[b].2) + 1);
    }
    // the most: for each a (latest completion first), the b's invoked after it, delivered first
    let mut bit = Bit(vec![0; len + 1]);
    let mut most = 0u64;
    let mut k = len;
    for &a in by_ret.iter().rev() {
        while k > 0 && v[by_inv[k - 1]].1 > v[a].0 {
            k -= 1;
            bit.add(rank(v[by_inv[k]].2));
        }
        most = most.max(bit.below(rank(v[a].2)));
    }
    (pairs, most)
}
