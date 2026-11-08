//! The claims (docs/design/messaging.md "The properties"). Each returns a [`Claim`]: held,
//! violated with a small witness (the operations that together prove it), or undecided when the
//! records a claim needs are missing. Pairs a claim cannot judge (a clock step inside the
//! interval, records in two boots, no time) are counted as undecided and never decide.

use super::decl::{Ack, DedupBy, Delay, Duplicates, Lease, OrderKind, Scope};
use super::hist::{span, At, OpKind, Out};
use super::index::{Dlv, Gk, Ix, Msg};
use super::{Claim, Status, Witness};
use std::collections::{BTreeMap, BTreeSet};

const MS: i128 = 1_000_000;

fn secs(t: Option<i128>) -> String {
    match t {
        Some(t) => format!("{:.3} s", t as f64 / 1e9),
        None => "an unknown time".into(),
    }
}

fn gname(g: &Gk) -> String {
    if g.1 == "-" {
        format!("channel {}", g.0)
    } else {
        format!("group {} of channel {}", g.1, g.0)
    }
}

fn held(name: &'static str, judged: u64, undecided: u64, what: &str) -> Claim {
    Claim {
        name,
        status: Status::Held,
        judged,
        undecided,
        summary: format!("{what} ({judged} judged, {undecided} undecided)"),
        witness: Witness::default(),
    }
}

fn undecided(name: &'static str, judged: u64, undecided: u64, why: String) -> Claim {
    Claim {
        name,
        status: Status::Undecided,
        judged,
        undecided,
        summary: why,
        witness: Witness::default(),
    }
}

fn violated(name: &'static str, judged: u64, undecided: u64, s: String, w: Witness) -> Claim {
    Claim {
        name,
        status: Status::Violated,
        judged,
        undecided,
        summary: s,
        witness: w,
    }
}

// ------------------------------------------------------------------ no-phantom

pub fn no_phantom(ix: &Ix<'_>) -> Claim {
    let mut judged = 0;
    for (n, x) in ix.dl.iter().enumerate() {
        judged += 1;
        let op = ix.dlv_op(n);
        let ret = ix.dlv_ret(n);
        let Some(m) = ix.msgs.get(&x.m) else {
            let mut w = Witness::default();
            w.op(ix, x.op);
            return violated(
                "no-phantom",
                judged,
                0,
                format!(
                    "{} was delivered to process {} in {} but nobody sent it (unexpected)",
                    x.m,
                    op.proc_,
                    gname(&x.g)
                ),
                w,
            );
        };
        let live: Vec<_> = m
            .sends
            .iter()
            .filter(|s| ix.send_out(**s) != Out::Fail)
            .collect();
        if live.is_empty() {
            let mut w = Witness::default();
            for s in &m.sends {
                w.op(ix, s.op);
            }
            w.op(ix, x.op);
            return violated(
                "no-phantom",
                judged,
                0,
                format!(
                    "{} was delivered to process {} in {} although every send of it failed (an aborted delivery)",
                    x.m,
                    op.proc_,
                    gname(&x.g)
                ),
                w,
            );
        }
        if live.iter().all(|s| ix.op(s.op).inv.pos > ret.pos) {
            let mut w = Witness::default();
            w.op(ix, x.op);
            w.op(ix, live[0].op);
            return violated(
                "no-phantom",
                judged,
                0,
                format!(
                    "{} was delivered to process {} in {} before any send of it was invoked",
                    x.m,
                    op.proc_,
                    gname(&x.g)
                ),
                w,
            );
        }
    }
    held(
        "no-phantom",
        judged,
        0,
        "every delivered message was sent before it was delivered",
    )
}

// ------------------------------------------------------------------ no-loss and fan-out

/// Why a message missing from a group is not a loss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Excuse {
    Expired,
    DeadLettered,
    PossiblyConsumed,
    Rejected,
}

impl Excuse {
    pub fn name(self) -> &'static str {
        match self {
            Excuse::Expired => "expired",
            Excuse::DeadLettered => "dead-lettered",
            Excuse::PossiblyConsumed => "possibly-consumed",
            Excuse::Rejected => "rejected",
        }
    }
}

/// What became of an acknowledged message in one group that must receive it and was drained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    Delivered,
    Excused(Excuse),
    /// Retention could have run out while the guest was off (across a power cut).
    Undecided,
    Lost,
}

pub fn fate(ix: &Ix<'_>, m: &Msg, g: &Gk) -> Fate {
    if !ix.deliveries(&m.m, g).is_empty() {
        return Fate::Delivered;
    }
    if ix
        .acks_of(&m.m, g)
        .iter()
        .any(|a| a.kind == OpKind::Nack && a.out != Out::Fail && a.item.r.as_deref() == Some("0"))
    {
        return Fate::Excused(Excuse::Rejected);
    }
    if let Some(to) = ix.d.dead_letter.as_ref().and_then(|x| x.to.as_ref()) {
        if ix.dl.iter().any(|x| &x.g.0 == to && x.m == m.m) {
            return Fate::Excused(Excuse::DeadLettered);
        }
    }
    let first = ix.first_live(m).map(|s| ix.op(s.op).inv);
    if ix.d.ack == Ack::OnReceive {
        if let (Some(gr), Some(f)) = (ix.groups.get(g), first) {
            if gr.info_recvs.iter().any(|&p| p > f.pos) {
                return Fate::Excused(Excuse::PossiblyConsumed);
            }
        }
    }
    if let (Some(r), Some(f)) = (ix.d.retention_ms, first) {
        let dr = ix
            .groups
            .get(g)
            .and_then(|x| x.drained)
            .map(|i| &ix.h.drained[i]);
        if let Some(dr) = dr {
            match span(&f, &dr.at) {
                Some(dt) if dt > r as i128 * MS => return Fate::Excused(Excuse::Expired),
                Some(_) => {}
                None => return Fate::Undecided,
            }
        }
    }
    Fate::Lost
}

/// The witness of a loss: the message's acknowledged send, the group's drained record, and a
/// delivered message of the same key with a higher offset (a gap, not a slow consumer).
fn loss_witness(ix: &Ix<'_>, m: &Msg, g: &Gk) -> (Witness, String) {
    let mut w = Witness::default();
    let s = ix.first_ok(m).expect("an acknowledged message");
    w.op(ix, s.op);
    let mut extra = String::new();
    if let Some(dr) = ix.groups.get(g).and_then(|x| x.drained) {
        w.drained(ix, dr);
    }
    let off = ix
        .send_item(s)
        .o
        .as_deref()
        .and_then(|o| o.parse::<i64>().ok());
    if let Some(off) = off {
        let higher = ix
            .dl
            .iter()
            .enumerate()
            .filter(|(_, x)| &x.g == g)
            .find(|(n, _)| {
                let it = ix.dlv_item(*n);
                it.key() == m.k
                    && it
                        .o
                        .as_deref()
                        .and_then(|o| o.parse::<i64>().ok())
                        .is_some_and(|o| o > off)
            });
        if let Some((n, x)) = higher {
            w.op(ix, x.op);
            extra = format!(
                "; {} of the same key at offset {} was delivered, so offset {off} is a gap",
                x.m,
                ix.dlv_item(n).o.clone().unwrap_or_default()
            );
        }
    }
    (w, extra)
}

/// `every`: fan-out (every drained group that must receive it); else no-loss (at least one).
pub fn no_loss(ix: &Ix<'_>, every: bool) -> Claim {
    let name = if every { "fan-out" } else { "no-loss" };
    let (mut judged, mut und) = (0u64, 0u64);
    let mut any_drained = false;
    for m in ix.msgs.values() {
        if !ix.acknowledged(m) {
            continue;
        }
        let must = ix.must_groups(m);
        let drained: Vec<&Gk> = must
            .iter()
            .filter(|g| ix.groups[*g].drained.is_some())
            .collect();
        und += (must.len() - drained.len()) as u64;
        if drained.is_empty() {
            continue;
        }
        any_drained = true;
        let fates: Vec<(Fate, &Gk)> = drained.iter().map(|g| (fate(ix, m, g), *g)).collect();
        judged += 1;
        let lost: Vec<&Gk> = fates
            .iter()
            .filter(|(f, _)| *f == Fate::Lost)
            .map(|x| x.1)
            .collect();
        if fates.iter().any(|(f, _)| *f == Fate::Undecided) {
            und += 1;
        }
        // fan-out: lost in any drained group; no-loss: lost in every one
        let bad = if every || lost.len() == fates.len() {
            lost.first().copied()
        } else {
            None
        };
        if let Some(g) = bad {
            let (w, extra) = loss_witness(ix, m, g);
            let s = ix.first_ok(m).expect("acknowledged");
            let op = ix.op(s.op);
            return violated(
                name,
                judged,
                und,
                format!(
                    "{} was acknowledged to process {} at {} and never delivered in {}, which was drained{extra}",
                    m.m,
                    op.proc_,
                    secs(op.ret.and_then(|r| r.shown)),
                    gname(g)
                ),
                w,
            );
        }
    }
    if !any_drained && ix.msgs.values().any(|m| ix.acknowledged(m)) {
        return undecided(
            name,
            judged,
            und,
            "no group that had to receive the acknowledged messages has a drained record: the \
             check's final: drain must write `drained <channel> <group>` when it has read \
             everything"
                .into(),
        );
    }
    held(
        name,
        judged,
        und,
        if every {
            "every acknowledged message reached every drained group that had to receive it"
        } else {
            "every acknowledged message was delivered"
        },
    )
}

// ------------------------------------------------------------------ at-most-once

pub fn at_most_once(ix: &Ix<'_>) -> Claim {
    let mut judged = 0;
    for ((m, g), ds) in &ix.by_mg {
        judged += 1;
        if ds.len() > 1 {
            let mut w = Witness::default();
            w.op(ix, ix.dl[ds[0]].op);
            w.op(ix, ix.dl[ds[1]].op);
            return violated(
                "at-most-once",
                judged,
                0,
                format!(
                    "{m} was delivered twice in {}: to process {}, then to process {}",
                    gname(g),
                    ix.dlv_op(ds[0]).proc_,
                    ix.dlv_op(ds[1]).proc_
                ),
                w,
            );
        }
    }
    held(
        "at-most-once",
        judged,
        0,
        "no message was delivered twice in a group",
    )
}

// ------------------------------------------------------------------ duplicates

/// The system ids a message's acknowledged sends and deliveries carry: (id, op).
fn system_ids(ix: &Ix<'_>, m: &Msg) -> Vec<(String, usize)> {
    let mut v = Vec::new();
    for s in &m.sends {
        if ix.send_out(*s) == Out::Ok {
            if let Some(o) = &ix.send_item(*s).o {
                v.push((o.clone(), s.op));
            }
        }
    }
    for g in ix.groups.keys() {
        for &d in ix.deliveries(&m.m, g) {
            if let Some(o) = &ix.dlv_item(d).o {
                v.push((o.clone(), ix.dl[d].op));
            }
        }
    }
    v
}

pub fn duplicates(ix: &Ix<'_>) -> Claim {
    let (mut judged, mut und) = (0u64, 0u64);
    let mut candidates = 0u64;
    match ix.d.duplicates {
        Duplicates::Allowed => return held("duplicates", 0, 0, "duplicates allowed"),
        Duplicates::Forbidden => {
            for m in ix.msgs.values() {
                let ids = system_ids(ix, m);
                if ids.is_empty() {
                    und += 1;
                    continue;
                }
                judged += 1;
                if let Some(b) = ids.iter().find(|x| x.0 != ids[0].0) {
                    let mut w = Witness::default();
                    w.op(ix, ids[0].1);
                    w.op(ix, b.1);
                    return violated(
                        "duplicates",
                        judged,
                        und,
                        format!(
                            "{} is stored twice: it carries the system ids {} and {}",
                            m.m, ids[0].0, b.0
                        ),
                        w,
                    );
                }
            }
            if judged == 0 && und > 0 {
                return undecided(
                    "duplicates",
                    0,
                    und,
                    "no send acknowledgement or delivery carries a system id (o=): only the \
                     system's ids tell a second stored copy from a redelivery"
                        .into(),
                );
            }
        }
        Duplicates::Window { by, window_ms } => {
            // sends that may have reached the system, grouped by the id that deduplicates them
            let mut by_id: BTreeMap<String, Vec<(String, super::index::SendRef)>> = BTreeMap::new();
            for m in ix.msgs.values() {
                for s in &m.sends {
                    if ix.send_out(*s) == Out::Fail {
                        continue;
                    }
                    let it = ix.send_item(*s);
                    let id = match by {
                        DedupBy::Message => it.m.clone(),
                        DedupBy::DedupId => it.dedup().to_string(),
                    };
                    by_id.entry(id).or_default().push((m.m.clone(), *s));
                }
            }
            let within = |a: &super::index::SendRef, b: &super::index::SendRef| -> Option<bool> {
                if window_ms == 0 {
                    return Some(true);
                }
                let (ai, bi) = (ix.op(a.op), ix.op(b.op));
                // b reached the system by its completion (or never: an unknown outcome)
                let bret = bi.ret?;
                span(&ai.inv, &bret).map(|dt| dt < window_ms as i128 * MS)
            };
            for (id, sends) in &by_id {
                if sends.len() < 2 {
                    continue;
                }
                // pairs of acknowledged sends with system ids
                for (i, (ma, a)) in sends.iter().enumerate() {
                    for (mb, b) in &sends[i + 1..] {
                        candidates += 1;
                        let (oa, ob) = (&ix.send_item(*a).o, &ix.send_item(*b).o);
                        let (Some(oa), Some(ob)) = (oa, ob) else {
                            continue;
                        };
                        let (first, second) = if ix.op(a.op).inv.pos <= ix.op(b.op).inv.pos {
                            (a, b)
                        } else {
                            (b, a)
                        };
                        match within(first, second) {
                            None => und += 1,
                            Some(false) => judged += 1,
                            Some(true) => {
                                judged += 1;
                                if oa != ob {
                                    let mut w = Witness::default();
                                    w.op(ix, first.op);
                                    w.op(ix, second.op);
                                    return violated(
                                        "duplicates",
                                        judged,
                                        und,
                                        format!(
                                            "two sends with the {} {id} ({ma} and {mb}) within the deduplication window were stored twice, with the system ids {oa} and {ob}",
                                            if by == DedupBy::Message { "message id" } else { "deduplication id" }
                                        ),
                                        w,
                                    );
                                }
                            }
                        }
                    }
                }
                // the deliveries: every send within the window of the first, and two stored copies
                let first = sends
                    .iter()
                    .min_by_key(|x| ix.op(x.1.op).inv.pos)
                    .expect("two sends");
                let all_within: Option<bool> = sends
                    .iter()
                    .try_fold(true, |acc, s| within(&first.1, &s.1).map(|b| acc && b));
                if all_within != Some(true) {
                    continue;
                }
                let msgs: BTreeSet<&String> = sends.iter().map(|x| &x.0).collect();
                let mut ids: Vec<(String, usize)> = Vec::new();
                let mut delivered: Vec<(&String, usize)> = Vec::new();
                for mid in &msgs {
                    let m = &ix.msgs[*mid];
                    ids.extend(system_ids(ix, m));
                    if let Some(d) = ix.dl.iter().find(|x| &x.m == *mid) {
                        delivered.push((mid, d.op));
                    }
                }
                if by == DedupBy::DedupId && delivered.len() > 1 {
                    let mut w = Witness::default();
                    w.op(ix, first.1.op);
                    w.op(ix, delivered[0].1);
                    w.op(ix, delivered[1].1);
                    return violated(
                        "duplicates",
                        judged + 1,
                        und,
                        format!(
                            "{} and {}, sent with the deduplication id {id} within the window, were both delivered",
                            delivered[0].0, delivered[1].0
                        ),
                        w,
                    );
                }
                if let Some(b) = ids.iter().find(|x| x.0 != ids[0].0) {
                    let mut w = Witness::default();
                    w.op(ix, ids[0].1);
                    w.op(ix, b.1);
                    return violated(
                        "duplicates",
                        judged + 1,
                        und,
                        format!(
                            "{id}, sent {} times within the deduplication window, is stored twice: it carries the system ids {} and {}",
                            sends.len(),
                            ids[0].0,
                            b.0
                        ),
                        w,
                    );
                }
                if !ids.is_empty() {
                    judged += 1;
                }
            }
            if judged == 0 && candidates > 0 && und == 0 {
                return undecided(
                    "duplicates",
                    0,
                    candidates,
                    "sends with one id were not compared: no send acknowledgement or delivery \
                     carries a system id (o=)"
                        .into(),
                );
            }
        }
    }
    held("duplicates", judged, und, "no message was stored twice")
}

// ------------------------------------------------------------------ after-ack

pub fn after_ack(ix: &Ix<'_>) -> Claim {
    let (mut judged, mut und) = (0u64, 0u64);
    for ((m, g), acks) in &ix.acks {
        for a in acks {
            if a.kind != OpKind::Ack || a.out != Out::Ok {
                continue;
            }
            let aop = ix.op(a.op);
            let Some(aret) = aop.ret else { continue };
            judged += 1;
            let acked_o = a
                .dlv
                .and_then(|d| ix.dlv_item(d).o.clone())
                .or(a.item.o.clone());
            for &d in ix.deliveries(m, g) {
                let r = ix.dlv_op(d);
                if r.inv.pos <= aret.pos {
                    continue;
                }
                let same_stored = acked_o.is_some() && ix.dlv_item(d).o == acked_o;
                let resent = ix.msgs.get(m).is_some_and(|msg| {
                    msg.sends
                        .iter()
                        .any(|s| ix.send_out(*s) == Out::Info || ix.op(s.op).inv.pos > aop.inv.pos)
                });
                if resent && !same_stored {
                    und += 1;
                    continue;
                }
                let mut w = Witness::default();
                if let Some(d0) = a.dlv {
                    w.op(ix, ix.dl[d0].op);
                }
                w.op(ix, a.op);
                w.op(ix, ix.dl[d].op);
                return violated(
                    "after-ack",
                    judged,
                    und,
                    format!(
                        "{m} was delivered again to process {} in {} after process {}'s acknowledgement of it completed at {}",
                        r.proc_,
                        gname(g),
                        aop.proc_,
                        secs(aret.shown)
                    ),
                    w,
                );
            }
        }
    }
    held(
        "after-ack",
        judged,
        und,
        "no message was delivered again after its acknowledgement completed",
    )
}

// ------------------------------------------------------------------ processing (and members)

/// Two accepted acknowledgements of one message in a group, for different deliveries (by
/// different members when `members` is set). Returns (judged, the first such pair).
fn double_acks(ix: &Ix<'_>, members: bool) -> (u64, Option<(String, Gk, usize, usize)>) {
    let mut judged = 0;
    for ((m, g), acks) in &ix.acks {
        let ok: Vec<&super::index::AckRef> = acks
            .iter()
            .filter(|a| a.kind == OpKind::Ack && a.out == Out::Ok)
            .collect();
        if ok.is_empty() {
            continue;
        }
        judged += 1;
        for (i, a) in ok.iter().enumerate() {
            for b in &ok[i + 1..] {
                let distinct = match (a.dlv, b.dlv) {
                    (Some(x), Some(y)) => x != y,
                    _ => false,
                };
                let others = ix.op(a.op).proc_ != ix.op(b.op).proc_;
                if distinct && (!members || others) {
                    return (judged, Some((m.clone(), g.clone(), a.op, b.op)));
                }
            }
        }
    }
    (judged, None)
}

pub fn processing(ix: &Ix<'_>) -> Claim {
    let (judged, pair) = double_acks(ix, false);
    if let Some((m, g, a, b)) = pair {
        let mut w = Witness::default();
        for x in [a, b] {
            if let Some(d) = ix
                .acks_of(&m, &g)
                .iter()
                .find(|r| r.op == x)
                .and_then(|r| r.dlv)
            {
                w.op(ix, ix.dl[d].op);
            }
            w.op(ix, x);
        }
        return violated(
            "processing",
            judged,
            0,
            format!(
                "{m} was processed twice in {}: the system accepted the acknowledgements of two deliveries, by process {} and process {}",
                gname(&g),
                ix.op(a).proc_,
                ix.op(b).proc_
            ),
            w,
        );
    }
    // every delivered acknowledged message of a drained group has an acknowledgement
    let mut und = 0;
    let mut judged2 = judged;
    for msg in ix.msgs.values() {
        if !ix.acknowledged(msg) {
            continue;
        }
        for g in ix.must_groups(msg) {
            let ds = ix.deliveries(&msg.m, &g);
            if ds.is_empty() {
                continue;
            }
            if ix.groups[&g].drained.is_none() {
                und += 1;
                continue;
            }
            judged2 += 1;
            let acks = ix.acks_of(&msg.m, &g);
            let acked = acks
                .iter()
                .any(|a| a.kind == OpKind::Ack && a.out != Out::Fail);
            let rejected = acks.iter().any(|a| {
                a.kind == OpKind::Nack && a.out != Out::Fail && a.item.r.as_deref() == Some("0")
            });
            if !acked && !rejected && fate_dead_lettered(ix, msg) {
                continue;
            }
            if !acked && !rejected {
                let mut w = Witness::default();
                for &d in ds {
                    w.op(ix, ix.dl[d].op);
                }
                if let Some(dr) = ix.groups[&g].drained {
                    w.drained(ix, dr);
                }
                return violated(
                    "processing",
                    judged2,
                    und,
                    format!(
                        "{} was delivered {} time(s) in {} and never acknowledged, although the group was drained",
                        msg.m,
                        ds.len(),
                        gname(&g)
                    ),
                    w,
                );
            }
        }
    }
    held(
        "processing",
        judged2,
        und,
        "every message was processed once in each group",
    )
}

fn fate_dead_lettered(ix: &Ix<'_>, m: &Msg) -> bool {
    ix.d.dead_letter
        .as_ref()
        .and_then(|x| x.to.as_ref())
        .is_some_and(|to| ix.dl.iter().any(|x| &x.g.0 == to && x.m == m.m))
}

pub fn members(ix: &Ix<'_>) -> Claim {
    if ix.d.lease != Lease::None {
        let c = lease_claim(ix, "members", true);
        return c;
    }
    let (judged, pair) = double_acks(ix, true);
    if let Some((m, g, a, b)) = pair {
        let mut w = Witness::default();
        w.op(ix, a);
        w.op(ix, b);
        return violated(
            "members",
            judged,
            0,
            format!(
                "two members of {} both finished {m}: the system accepted process {}'s and process {}'s acknowledgements",
                gname(&g),
                ix.op(a).proc_,
                ix.op(b).proc_
            ),
            w,
        );
    }
    held(
        "members",
        judged,
        0,
        "no message was finished by two members of a group",
    )
}

// ------------------------------------------------------------------ lease

/// The lease a delivery got, in ns: the declared length, or the delivery's own `v=`.
fn lease_of(ix: &Ix<'_>, d: usize) -> Option<i128> {
    match ix.d.lease {
        Lease::Ms(x) => Some(x as i128 * MS),
        Lease::PerDelivery => ix.dlv_item(d).v.map(|v| v as i128 * MS),
        Lease::None => None,
    }
}

/// The time until which delivery `d`'s lease surely held, and the reason it ends there: the
/// lower bound of its end (raised by accepted extensions), cut at the first acknowledgement or
/// nack invoked that did not fail.
fn lease_end(ix: &Ix<'_>, d: usize, len: i128) -> Option<(i128, Option<usize>)> {
    let x: &Dlv = &ix.dl[d];
    let r1 = ix.dlv_op(d);
    let mut l = r1.inv.t? + len;
    let mut exts: Vec<&super::index::AckRef> = ix
        .acks_of(&x.m, &x.g)
        .iter()
        .filter(|a| a.kind == OpKind::Extend && a.dlv == Some(d) && a.out == Out::Ok)
        .collect();
    exts.sort_by_key(|a| ix.op(a.op).ret_pos());
    for e in exts {
        let eo = ix.op(e.op);
        let (Some(ret), Some(inv)) = (eo.ret, eo.inv.t) else {
            continue;
        };
        if ret.boot != r1.inv.boot {
            continue;
        }
        let v = e.item.v.map(|v| v as i128 * MS).unwrap_or(len);
        if ret.t.is_some_and(|t| t < l) {
            l = l.max(inv + v);
        }
    }
    let mut cut: Option<(i128, usize)> = None;
    for a in ix.acks_of(&x.m, &x.g) {
        if a.dlv != Some(d) || a.out == Out::Fail || a.kind == OpKind::Extend {
            continue;
        }
        let ao = ix.op(a.op);
        if ao.inv.boot != r1.inv.boot {
            continue;
        }
        if let Some(t) = ao.inv.t {
            if cut.is_none_or(|c| t < c.0) {
                cut = Some((t, a.op));
            }
        }
    }
    match cut {
        Some((t, op)) if t < l => Some((t, Some(op))),
        _ => Some((l, None)),
    }
}

fn lease_claim(ix: &Ix<'_>, name: &'static str, members_only: bool) -> Claim {
    let (mut judged, mut und) = (0u64, 0u64);
    let mut missing_v = 0u64;
    for ((m, g), ds) in &ix.by_mg {
        for (i, &d1) in ds.iter().enumerate() {
            for &d2 in &ds[i + 1..] {
                let (r1, r2) = (ix.dlv_op(d1), ix.dlv_op(d2));
                if members_only && r1.proc_ == r2.proc_ {
                    continue;
                }
                let (Some(len1), Some(len2)) = (lease_of(ix, d1), lease_of(ix, d2)) else {
                    missing_v += 1;
                    continue;
                };
                let (ret1, ret2) = (ix.dlv_ret(d1), ix.dlv_ret(d2));
                if r1.inv.boot != ret2.boot || r2.inv.boot != ret1.boot {
                    // across a power cut: the first boot's holder is gone
                    und += 1;
                    continue;
                }
                let lo = r1.inv.pos.min(r2.inv.pos);
                let hi = ret1.pos.max(ret2.pos);
                if ix.clock_between(lo, hi) {
                    und += 1;
                    continue;
                }
                let Some((end1, cut1)) = lease_end(ix, d1, len1) else {
                    und += 1;
                    continue;
                };
                let Some(t2) = ret2.t else {
                    und += 1;
                    continue;
                };
                judged += 1;
                let bad = if ret1.pos < r2.inv.pos {
                    t2 < end1
                } else {
                    // overlapping receives: neither order fits the leases
                    let Some((end2, _)) = lease_end(ix, d2, len2) else {
                        und += 1;
                        continue;
                    };
                    ret1.t.is_some_and(|t1| t2 < end1 && t1 < end2)
                };
                if bad {
                    let mut w = Witness::default();
                    w.op(ix, r1.i);
                    if let Some(c) = cut1 {
                        w.op(ix, c);
                    }
                    w.op(ix, r2.i);
                    let why = match cut1 {
                        Some(_) => "until its acknowledgement or nack was invoked".to_string(),
                        None => format!(
                            "until at least {} (lease {} ms, extensions counted, no acknowledgement or nack before)",
                            secs(Some(end1)),
                            len1 / MS
                        ),
                    };
                    return violated(
                        name,
                        judged,
                        und,
                        format!(
                            "{m} was delivered to process {} at {} while process {}'s lease from {} held {why}, in {}",
                            r2.proc_,
                            secs(ret2.shown),
                            r1.proc_,
                            secs(r1.inv.shown),
                            gname(g)
                        ),
                        w,
                    );
                }
            }
        }
    }
    if judged == 0 && missing_v > 0 && und == 0 {
        return undecided(
            name,
            0,
            missing_v,
            "lease: {ms: per-delivery} needs each delivery's lease (v=), and the deliveries \
             compared carry none"
                .into(),
        );
    }
    held(
        name,
        judged,
        und + missing_v,
        "no message was delivered again in its group while a lease on it held",
    )
}

pub fn lease(ix: &Ix<'_>) -> Claim {
    lease_claim(ix, "lease", false)
}

// ------------------------------------------------------------------ order

/// A message's first delivery in a group: (receive invocation, receive completion, receive op,
/// place).
#[derive(Clone, Copy, Debug)]
struct First {
    inv: usize,
    ret: usize,
    op: usize,
    idx: usize,
}

fn first_in(ix: &Ix<'_>, m: &str, g: &Gk) -> Option<First> {
    let d = *ix.deliveries(m, g).first()?;
    let op = ix.dlv_op(d);
    Some(First {
        inv: op.inv.pos,
        ret: op.ret_pos(),
        op: op.i,
        idx: ix.dl[d].idx,
    })
}

/// Whether `b` is surely delivered before `a` started: b's receive completed before a's was
/// invoked, or b comes earlier in the same receive.
fn before(b: &First, a: &First) -> bool {
    b.ret < a.inv || (b.op == a.op && b.idx < a.idx)
}

/// The scope a message's order is judged in.
fn scope_of(ix: &Ix<'_>, m: &Msg) -> String {
    match ix.d.order.scope {
        Scope::Key => m.k.clone(),
        Scope::Channel => String::new(),
    }
}

/// Precedence pairs m1 -> m2 inside one scope of one channel: (m1, m2, why). Real-time (a send of
/// m1 completed ok before m2's first live send was invoked) and, with `batch: ordered`, the order
/// of one batch's items.
pub fn precedence_witness(ix: &Ix<'_>, m1: &Msg, m2: &Msg) -> Option<String> {
    let a = ix.first_ok(m1)?;
    let b = ix.first_live(m2)?;
    if ix.op(a.op).ret_pos() < ix.op(b.op).inv.pos {
        return Some(format!(
            "the send of {} completed before the send of {} was invoked",
            m1.m, m2.m
        ));
    }
    None
}

pub fn order(ix: &Ix<'_>) -> Claim {
    match ix.d.order.kind {
        OrderKind::Fifo | OrderKind::FifoBlocking => fifo(ix),
        OrderKind::Log => log(ix),
        OrderKind::Causal => causal(ix),
        OrderKind::None => held("order", 0, 0, "no order claimed"),
    }
}

/// For each group: the messages of its channel by scope.
fn scopes<'a>(ix: &'a Ix<'_>, q: &str) -> BTreeMap<String, Vec<&'a Msg>> {
    let mut s: BTreeMap<String, Vec<&Msg>> = BTreeMap::new();
    for m in ix.msgs.values() {
        if m.q == q {
            s.entry(scope_of(ix, m)).or_default().push(m);
        }
    }
    s
}

fn fifo(ix: &Ix<'_>) -> Claim {
    let blocking = ix.d.order.kind == OrderKind::FifoBlocking;
    let (mut judged, mut und) = (0u64, 0u64);
    for (gk, gr) in &ix.groups {
        let first_info = gr.info_recvs.iter().min().copied();
        for msgs in scopes(ix, &gk.0).values() {
            // predecessors by their first ok completion, with a prefix maximum of the first
            // delivery's receive invocation (fifo) and of the release (blocking)
            let mut preds: Vec<(usize, &Msg)> = msgs
                .iter()
                .filter_map(|m| ix.first_ok(m).map(|s| (ix.op(s.op).ret_pos(), *m)))
                .collect();
            preds.sort_by_key(|x| x.0);
            let mut succs: Vec<(usize, &Msg)> = msgs
                .iter()
                .filter_map(|m| ix.first_live(m).map(|s| (ix.op(s.op).inv.pos, *m)))
                .collect();
            succs.sort_by_key(|x| x.0);
            let mut best: Option<(usize, &Msg)> = None; // max first-delivery inv
            let mut best_rel: Option<(usize, &Msg)> = None; // max release
            let mut pi = 0;
            for (sinv, m2) in &succs {
                while pi < preds.len() && preds[pi].0 < *sinv {
                    let m1 = preds[pi].1;
                    if let Some(f1) = first_in(ix, &m1.m, gk) {
                        if best.is_none_or(|b| f1.inv > b.0) {
                            best = Some((f1.inv, m1));
                        }
                        if blocking {
                            let rel = release(ix, m1, gk);
                            if best_rel.is_none_or(|b| rel > b.0) {
                                best_rel = Some((rel, m1));
                            }
                        }
                    }
                    pi += 1;
                }
                let Some(f2) = first_in(ix, &m2.m, gk) else {
                    continue;
                };
                if best.is_none() {
                    continue;
                }
                if first_info.is_some_and(|p| p < f2.ret) {
                    und += 1;
                    continue;
                }
                judged += 1;
                if let Some((inv1, m1)) = best {
                    if f2.ret < inv1 {
                        return order_violation(ix, gk, m1, m2, "first delivered", judged, und);
                    }
                }
                if let Some((rel, m1)) = best_rel {
                    if rel > f2.ret {
                        return order_violation(
                            ix,
                            gk,
                            m1,
                            m2,
                            "delivered before the earlier message was acknowledged",
                            judged,
                            und,
                        );
                    }
                }
            }
            // the same receive, and the order of one batch
            for m2 in msgs {
                let Some(f2) = first_in(ix, &m2.m, gk) else {
                    continue;
                };
                if first_info.is_some_and(|p| p < f2.ret) {
                    continue;
                }
                let r = ix.op(f2.op);
                for it in &r.items[f2.idx + 1..] {
                    // a later item of the same receive: m1 -> m2 would be violated
                    let Some(m1) = ix.msgs.get(&it.m) else {
                        continue;
                    };
                    if scope_of(ix, m1) != scope_of(ix, m2) || m1.q != m2.q {
                        continue;
                    }
                    let Some(f1) = first_in(ix, &m1.m, gk) else {
                        continue;
                    };
                    if f1.op != f2.op {
                        continue;
                    }
                    if precedence_witness(ix, m1, m2).is_some() {
                        judged += 1;
                        return order_violation(ix, gk, m1, m2, "first delivered", judged, und);
                    }
                }
            }
            if ix.d.order.batch_ordered {
                for op in
                    ix.h.ops
                        .iter()
                        .filter(|o| o.kind == OpKind::Send && o.q == gk.0)
                {
                    let live: Vec<&super::hist::Item> = op
                        .items
                        .iter()
                        .filter(|it| op.item_out(it) != Out::Fail)
                        .collect();
                    for (i, a) in live.iter().enumerate() {
                        for b in &live[i + 1..] {
                            let (Some(m1), Some(m2)) = (ix.msgs.get(&a.m), ix.msgs.get(&b.m))
                            else {
                                continue;
                            };
                            if scope_of(ix, m1) != scope_of(ix, m2) {
                                continue;
                            }
                            let (Some(f1), Some(f2)) =
                                (first_in(ix, &m1.m, gk), first_in(ix, &m2.m, gk))
                            else {
                                continue;
                            };
                            if first_info.is_some_and(|p| p < f2.ret) {
                                und += 1;
                                continue;
                            }
                            judged += 1;
                            if before(&f2, &f1) {
                                return order_violation(
                                    ix,
                                    gk,
                                    m1,
                                    m2,
                                    "first delivered (one ordered batch)",
                                    judged,
                                    und,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    held(
        "order",
        judged,
        und,
        if blocking {
            "first deliveries followed the order of the sends, and nothing later was delivered while an earlier message was out"
        } else {
            "first deliveries followed the order of the sends"
        },
    )
}

/// When an earlier message stops blocking its scope (fifo-blocking): the first acknowledgement
/// or reject invoked that did not fail, its last allowed delivery under a dead-letter limit,
/// its first delivery from the dead-letter channel; usize::MAX when never.
fn release(ix: &Ix<'_>, m: &Msg, g: &Gk) -> usize {
    let mut r = usize::MAX;
    for a in ix.acks_of(&m.m, g) {
        let rej = a.kind == OpKind::Nack && a.item.r.as_deref() == Some("0");
        if (a.kind == OpKind::Ack || rej) && a.out != Out::Fail {
            r = r.min(ix.op(a.op).inv.pos);
        }
    }
    if let Some(dlq) = &ix.d.dead_letter {
        let ds = ix.deliveries(&m.m, g);
        if ds.len() >= dlq.after as usize {
            r = r.min(ix.dlv_ret(ds[dlq.after as usize - 1]).pos);
        }
        if let Some(to) = &dlq.to {
            for (n, x) in ix.dl.iter().enumerate() {
                if &x.g.0 == to && x.m == m.m {
                    r = r.min(ix.dlv_ret(n).pos);
                }
            }
        }
    }
    r
}

fn order_violation(
    ix: &Ix<'_>,
    g: &Gk,
    m1: &Msg,
    m2: &Msg,
    what: &str,
    judged: u64,
    und: u64,
) -> Claim {
    let mut w = Witness::default();
    if let Some(s) = ix.first_ok(m1) {
        w.op(ix, s.op);
    }
    if let Some(s) = ix.first_live(m2) {
        w.op(ix, s.op);
    }
    for m in [m2, m1] {
        if let Some(f) = first_in(ix, &m.m, g) {
            w.op(ix, f.op);
        }
    }
    if what.contains("acknowledged") {
        if let Some(a) = ix.acks_of(&m1.m, g).first() {
            w.op(ix, a.op);
        }
    }
    let why = precedence_witness(ix, m1, m2)
        .unwrap_or_else(|| format!("{} comes before {} in one ordered batch", m1.m, m2.m));
    violated(
        "order",
        judged,
        und,
        format!(
            "{} was {what} in {} before {} although {why}",
            m2.m,
            gname(g),
            m1.m
        ),
        w,
    )
}

fn log(ix: &Ix<'_>) -> Claim {
    let (mut judged, und) = (0u64, 0u64);
    // offsets per scope: offset -> (message, op), message -> offsets
    let mut by_off: BTreeMap<(String, String, i64), (String, usize)> = BTreeMap::new();
    let mut of_msg: BTreeMap<String, (i64, usize)> = BTreeMap::new();
    let mut known: BTreeMap<(String, String), BTreeSet<i64>> = BTreeMap::new();
    let mut seen_any = false;
    let mut not_numbers = 0u64;
    let mut add = |m: &Msg, o: &Option<String>, op: usize| -> Option<Claim> {
        let o = o.as_ref()?;
        seen_any = true;
        let Ok(off) = o.parse::<i64>() else {
            not_numbers += 1;
            return None;
        };
        let sk = (m.q.clone(), scope_of(ix, m));
        known.entry(sk.clone()).or_default().insert(off);
        if let Some((other, op2)) = by_off.get(&(sk.0.clone(), sk.1.clone(), off)) {
            if other != &m.m {
                let mut w = Witness::default();
                w.op(ix, *op2);
                w.op(ix, op);
                return Some(violated(
                    "order",
                    0,
                    0,
                    format!(
                        "{other} and {} share offset {off} of key {} (inconsistent offsets)",
                        m.m, m.k
                    ),
                    w,
                ));
            }
        } else {
            by_off.insert((sk.0, sk.1, off), (m.m.clone(), op));
        }
        if let Some((o2, op2)) = of_msg.get(&m.m) {
            if *o2 != off {
                let mut w = Witness::default();
                w.op(ix, *op2);
                w.op(ix, op);
                return Some(violated(
                    "order",
                    0,
                    0,
                    format!("{} has two offsets, {o2} and {off} (stored twice)", m.m),
                    w,
                ));
            }
        } else {
            of_msg.insert(m.m.clone(), (off, op));
        }
        None
    };
    for m in ix.msgs.values() {
        for s in &m.sends {
            if ix.send_out(*s) == Out::Ok {
                if let Some(c) = add(m, &ix.send_item(*s).o, s.op) {
                    return c;
                }
            }
        }
        for g in ix.groups.keys() {
            for &d in ix.deliveries(&m.m, g) {
                if let Some(c) = add(m, &ix.dlv_item(d).o, ix.dl[d].op) {
                    return c;
                }
            }
        }
    }
    if !seen_any || not_numbers > 0 {
        let why = if seen_any {
            "order: log needs whole-number offsets (o=), and some are not".to_string()
        } else {
            "order: log needs offsets (o=) on the send acknowledgements or the deliveries, and \
             none is recorded"
                .to_string()
        };
        if ix.dl.is_empty() && !seen_any {
            return held("order", 0, 0, "nothing was delivered");
        }
        return undecided("order", 0, not_numbers, why);
    }
    // each consumer's reads of a key go up without skipping a known offset; a `sub` by the
    // consumer, or a new boot, starts its reads again
    let mut subs: BTreeMap<(Gk, String), Vec<usize>> = BTreeMap::new();
    for op in &ix.h.ops {
        if op.kind == OpKind::Sub {
            subs.entry(((op.q.clone(), op.g.clone()), op.proc_.clone()))
                .or_default()
                .push(op.inv.pos);
        }
    }
    // per (group, consumer, scope): (offset, receive op, its completion, its boot)
    type Reader = (Gk, String, String);
    let mut last: BTreeMap<Reader, (i64, usize, usize, usize)> = BTreeMap::new();
    for (n, x) in ix.dl.iter().enumerate() {
        let it = ix.dlv_item(n);
        let Some(off) = it.o.as_deref().and_then(|o| o.parse::<i64>().ok()) else {
            continue;
        };
        let Some(m) = ix.msgs.get(&x.m) else { continue };
        let r = ix.dlv_op(n);
        let key = (x.g.clone(), r.proc_.clone(), scope_of(ix, m));
        let ret = ix.dlv_ret(n);
        if let Some(&(prev, pop, ppos, pboot)) = last.get(&key) {
            let resub = subs
                .get(&(x.g.clone(), r.proc_.clone()))
                .is_some_and(|v| v.iter().any(|&p| p > ppos && p < r.inv.pos));
            if !resub && pboot == ret.boot {
                judged += 1;
                if off <= prev {
                    let mut w = Witness::default();
                    w.op(ix, pop);
                    w.op(ix, r.i);
                    return violated(
                        "order",
                        judged,
                        und,
                        format!(
                            "process {} read offset {off} of key {} in {} after offset {prev} (nonmonotonic)",
                            r.proc_,
                            m.k,
                            gname(&x.g)
                        ),
                        w,
                    );
                }
                let sk = (m.q.clone(), scope_of(ix, m));
                if let Some(skip) = known
                    .get(&sk)
                    .and_then(|s| s.range(prev + 1..off).next().copied())
                {
                    let mut w = Witness::default();
                    w.op(ix, pop);
                    w.op(ix, r.i);
                    if let Some((_, op)) = by_off.get(&(sk.0.clone(), sk.1.clone(), skip)) {
                        w.op(ix, *op);
                    }
                    return violated(
                        "order",
                        judged,
                        und,
                        format!(
                            "process {} read offset {off} of key {} in {} right after offset {prev}, skipping offset {skip}, which holds a message",
                            r.proc_,
                            m.k,
                            gname(&x.g)
                        ),
                        w,
                    );
                }
            }
        }
        last.insert(key, (off, r.i, ret.pos, ret.boot));
    }
    held(
        "order",
        judged,
        und,
        "offsets are one per message and one message per offset, and each consumer read them in order without gaps",
    )
}

fn causal(ix: &Ix<'_>) -> Claim {
    let (mut judged, mut und) = (0u64, 0u64);
    for (gk, gr) in &ix.groups {
        let first_info = gr.info_recvs.iter().min().copied();
        let msgs: Vec<&Msg> = ix.msgs.values().filter(|m| m.q == gk.0).collect();
        // events in position order: a send's first live invocation (compute its causal past),
        // the completion of an ok send (it joins the real-time past), the completion of a
        // receive (its messages join the receiving process's past)
        #[derive(Clone, Copy)]
        enum Ev<'m> {
            Inv(&'m Msg),
            Ret(&'m Msg),
            Recv(usize),
        }
        let mut evs: Vec<(usize, u8, Ev)> = Vec::new();
        for m in &msgs {
            if let Some(s) = ix.first_live(m) {
                evs.push((ix.op(s.op).inv.pos, 1, Ev::Inv(m)));
            }
            if let Some(s) = ix.first_ok(m) {
                evs.push((ix.op(s.op).ret_pos(), 0, Ev::Ret(m)));
            }
        }
        for (n, x) in ix.dl.iter().enumerate() {
            if x.g.0 == gk.0 {
                evs.push((ix.dlv_ret(n).pos, 0, Ev::Recv(n)));
            }
        }
        evs.sort_by_key(|e| (e.0, e.1));
        // val(m) = the latest first-delivery invocation in m's causal past or of m itself,
        // with the message it belongs to
        let mut val: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
        let mut past: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
        let mut global: Option<(usize, &str)> = None;
        let mut by_proc: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
        for (_, _, e) in &evs {
            match e {
                Ev::Inv(m) => {
                    // the sender's process past and the real-time past
                    let s = ix.first_live(m).expect("live");
                    let p = ix.op(s.op).proc_.as_str();
                    let mut best = global;
                    if let Some(x) = by_proc.get(p) {
                        if best.is_none_or(|b| x.0 > b.0) {
                            best = Some(*x);
                        }
                    }
                    if let Some(b) = best {
                        past.insert(m.m.as_str(), b);
                    }
                    let own = first_in(ix, &m.m, gk).map(|f| (f.inv, m.m.as_str()));
                    let v = match (best, own) {
                        (Some(b), Some(o)) => Some(if o.0 > b.0 { o } else { b }),
                        (b, o) => b.or(o),
                    };
                    if let Some(v) = v {
                        val.insert(m.m.as_str(), v);
                    }
                }
                Ev::Ret(m) => {
                    if let Some(v) = val.get(m.m.as_str()) {
                        if global.is_none_or(|g| v.0 > g.0) {
                            global = Some(*v);
                        }
                    }
                }
                Ev::Recv(n) => {
                    let x = &ix.dl[*n];
                    let p = ix.dlv_op(*n).proc_.as_str();
                    if let Some(v) = val.get(x.m.as_str()) {
                        let e = by_proc.entry(p).or_insert(*v);
                        if v.0 > e.0 {
                            *e = *v;
                        }
                    }
                }
            }
        }
        for m2 in &msgs {
            let Some(f2) = first_in(ix, &m2.m, gk) else {
                continue;
            };
            let Some(&(inv1, m1)) = past.get(m2.m.as_str()) else {
                continue;
            };
            if first_info.is_some_and(|p| p < f2.ret) {
                und += 1;
                continue;
            }
            judged += 1;
            if f2.ret < inv1 {
                let m1 = &ix.msgs[m1];
                let mut w = Witness::default();
                if let Some(s) = ix.first_live(m1) {
                    w.op(ix, s.op);
                }
                if let Some(s) = ix.first_live(m2) {
                    w.op(ix, s.op);
                }
                w.op(ix, f2.op);
                if let Some(f1) = first_in(ix, &m1.m, gk) {
                    w.op(ix, f1.op);
                }
                return violated(
                    "order",
                    judged,
                    und,
                    format!(
                        "{} was delivered in {} before {}, which causally precedes it (sent or received before it was sent, transitively)",
                        m2.m,
                        gname(gk),
                        m1.m
                    ),
                    w,
                );
            }
        }
    }
    held(
        "order",
        judged,
        und,
        "every group got each message after the messages that causally precede it",
    )
}

// ------------------------------------------------------------------ dead-letter

pub fn dead_letter(ix: &Ix<'_>) -> Claim {
    let Some(dlq) = ix.d.dead_letter.clone() else {
        return held("dead-letter", 0, 0, "no dead-letter claim");
    };
    let mut judged = 0;
    for ((m, g), ds) in &ix.by_mg {
        if dlq.to.as_ref() == Some(&g.0) {
            continue;
        }
        let Some(msg) = ix.msgs.get(m) else { continue };
        if msg.q != g.0 {
            continue;
        }
        judged += 1;
        let acks = ix.acks_of(m, g);
        // recorded attempt counts
        let mut prev: Option<i64> = None;
        for (k, &d) in ds.iter().enumerate() {
            if let Some(n) = ix.dlv_item(d).n {
                if n < (k as i64 + 1) || prev.is_some_and(|p| n < p) {
                    let mut w = Witness::default();
                    for &x in &ds[..=k] {
                        w.op(ix, ix.dl[x].op);
                    }
                    return violated(
                        "dead-letter",
                        judged,
                        0,
                        format!(
                            "{m}'s delivery {} in {} carries the attempt count {n}, below the {} deliveries observed{}",
                            k + 1,
                            gname(g),
                            k + 1,
                            if prev.is_some_and(|p| n < p) { " (the count went down)" } else { "" }
                        ),
                        w,
                    );
                }
                prev = Some(n);
            }
        }
        if ds.len() as i64 > dlq.after {
            let d = ds[dlq.after as usize];
            let ret = ix.dlv_ret(d).pos;
            let acked_before = acks
                .iter()
                .any(|a| a.kind == OpKind::Ack && a.out == Out::Ok && ix.op(a.op).inv.pos < ret);
            if !acked_before {
                let mut w = Witness::default();
                for &x in &ds[..=dlq.after as usize] {
                    w.op(ix, ix.dl[x].op);
                }
                return violated(
                    "dead-letter",
                    judged,
                    0,
                    format!(
                        "{m} was delivered {} times in {} without an acknowledgement, more than the limit of {}",
                        dlq.after + 1,
                        gname(g),
                        dlq.after
                    ),
                    w,
                );
            }
        }
        if let Some(to) = &dlq.to {
            let moved: Option<usize> = ix
                .dl
                .iter()
                .enumerate()
                .filter(|(_, x)| &x.g.0 == to && &x.m == m)
                .map(|(n, _)| n)
                .min_by_key(|n| ix.dlv_ret(*n).pos);
            if let Some(mv) = moved {
                // the dead-letter channel does not say which group moved the message: only a
                // group that had delivered it `after` times before it appeared there is the one
                // that moved it (or should have)
                let at = ix.dlv_ret(mv).pos;
                let before = ds.iter().filter(|&&d| ix.dlv_ret(d).pos < at).count() as i64;
                let again = ds.iter().find(|&&d| ix.dlv_op(d).inv.pos > at);
                if let (Some(&d), true) = (again, before >= dlq.after) {
                    let mut w = Witness::default();
                    w.op(ix, ix.dl[mv].op);
                    w.op(ix, ix.dl[d].op);
                    return violated(
                        "dead-letter",
                        judged,
                        0,
                        format!(
                            "{m} was delivered from {} after it appeared in the dead-letter channel {to}",
                            gname(g)
                        ),
                        w,
                    );
                }
            } else if ds.len() as i64 >= dlq.after
                && !acks
                    .iter()
                    .any(|a| a.out != Out::Fail && matches!(a.kind, OpKind::Ack))
                && ix
                    .groups
                    .iter()
                    .any(|(k, gr)| &k.0 == to && gr.drained.is_some())
            {
                let mut w = Witness::default();
                for &x in ds {
                    w.op(ix, ix.dl[x].op);
                }
                return violated(
                    "dead-letter",
                    judged,
                    0,
                    format!(
                        "{m} was delivered {} times in {} without an acknowledgement and never appeared in the dead-letter channel {to}, which was drained",
                        ds.len(),
                        gname(g)
                    ),
                    w,
                );
            }
        }
    }
    held(
        "dead-letter",
        judged,
        0,
        "no message was delivered again after its last allowed delivery",
    )
}

// ------------------------------------------------------------------ delay

pub fn delay(ix: &Ix<'_>) -> Claim {
    let (mut judged, mut und) = (0u64, 0u64);
    for (n, x) in ix.dl.iter().enumerate() {
        let Some(m) = ix.msgs.get(&x.m) else { continue };
        if m.q != x.g.0 {
            continue;
        }
        let ret = ix.dlv_ret(n);
        let mut bound: Option<(i128, usize)> = None;
        let mut unknown = false;
        for s in &m.sends {
            if ix.send_out(*s) == Out::Fail {
                continue;
            }
            let dms = match ix.d.delay {
                Delay::Ms(v) => v,
                Delay::PerMessage => ix.send_item(*s).d.unwrap_or(0),
                Delay::None => 0,
            };
            let inv = ix.op(s.op).inv;
            if inv.boot != ret.boot || inv.t.is_none() || ix.clock_between(inv.pos, ret.pos) {
                unknown = true;
                continue;
            }
            let b = inv.t.unwrap_or(0) + dms as i128 * MS;
            if bound.is_none_or(|x| b < x.0) {
                bound = Some((b, s.op));
            }
        }
        if unknown || ret.t.is_none() {
            und += 1;
            continue;
        }
        let Some((b, sop)) = bound else { continue };
        judged += 1;
        if ret.t.is_some_and(|t| t < b) {
            let mut w = Witness::default();
            w.op(ix, sop);
            w.op(ix, x.op);
            return violated(
                "delay",
                judged,
                und,
                format!(
                    "{} was delivered to process {} at {}, before its delay ended at {}",
                    x.m,
                    ix.dlv_op(n).proc_,
                    secs(ret.shown),
                    secs(Some(b))
                ),
                w,
            );
        }
    }
    held(
        "delay",
        judged,
        und,
        "no message was delivered before its delay ended",
    )
}

/// The `At` of an op's completion or its invocation (for the explanations).
pub fn when(a: Option<At>) -> String {
    secs(a.and_then(|x| x.shown))
}
