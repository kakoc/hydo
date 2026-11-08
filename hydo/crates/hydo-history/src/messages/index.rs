//! What every claim and measure looks up: messages by id with their sends, deliveries by message
//! and group, acknowledgements matched to the deliveries they acknowledge, and the groups of
//! each channel with what decides whether they must receive a message.

use super::decl::Decl;
use super::hist::{At, Item, MHist, MOp, OpKind, Out};
use std::collections::BTreeMap;

/// A group: (channel, group name).
pub type Gk = (String, String);

/// One send item: the op and the item's index in it.
#[derive(Clone, Copy, Debug)]
pub struct SendRef {
    pub op: usize,
    pub item: usize,
}

/// One delivery: an item of a successful receive.
#[derive(Clone, Debug)]
pub struct Dlv {
    pub op: usize,
    /// The item's place in its receive.
    pub idx: usize,
    pub m: String,
    pub g: Gk,
}

/// One acknowledgement (an ack item, or a delivery a commit covers), nack or extension.
#[derive(Clone, Debug)]
pub struct AckRef {
    pub op: usize,
    pub kind: OpKind,
    /// The item (a commit's synthetic item carries the delivery's message id).
    pub item: Item,
    pub out: Out,
    /// The delivery it is about, when one matches.
    pub dlv: Option<usize>,
}

/// A message: its id, the channel and key of its first send, and every send item.
#[derive(Clone, Debug)]
pub struct Msg {
    pub m: String,
    pub q: String,
    pub k: String,
    pub sends: Vec<SendRef>,
}

/// A group's facts.
#[derive(Clone, Debug, Default)]
pub struct Group {
    /// The position of its first completed `sub` (`from=now` or no `from`) or `recv`.
    pub joined: Option<usize>,
    /// It exists from the start: the channel's one group `-`, or a `sub ... from=start`.
    pub from_start: bool,
    /// Its first `drained` record (an index into MHist::drained).
    pub drained: Option<usize>,
    /// The invocation positions of its receives with an unknown outcome.
    pub info_recvs: Vec<usize>,
}

pub struct Ix<'a> {
    pub h: &'a MHist,
    pub d: &'a Decl,
    pub msgs: BTreeMap<String, Msg>,
    pub dl: Vec<Dlv>,
    /// Deliveries per (message, group), in delivery order.
    pub by_mg: BTreeMap<(String, Gk), Vec<usize>>,
    pub groups: BTreeMap<Gk, Group>,
    /// Acks (and commits), nacks and extensions per (message, group).
    pub acks: BTreeMap<(String, Gk), Vec<AckRef>>,
}

impl<'a> Ix<'a> {
    pub fn op(&self, i: usize) -> &'a MOp {
        &self.h.ops[i]
    }

    pub fn send_item(&self, s: SendRef) -> &'a Item {
        &self.h.ops[s.op].items[s.item]
    }

    pub fn send_out(&self, s: SendRef) -> Out {
        let op = &self.h.ops[s.op];
        op.item_out(&op.items[s.item])
    }

    pub fn dlv_item(&self, d: usize) -> &'a Item {
        let x = &self.dl[d];
        &self.h.ops[x.op].items[x.idx]
    }

    pub fn dlv_op(&self, d: usize) -> &'a MOp {
        &self.h.ops[self.dl[d].op]
    }

    /// The receive's completion of a delivery.
    pub fn dlv_ret(&self, d: usize) -> At {
        self.dlv_op(d).ret.expect("a delivery's receive completed")
    }

    /// Delivery order: by the receive's completion, then by the place in it.
    pub fn dlv_key(&self, d: usize) -> (usize, usize) {
        (self.dlv_ret(d).pos, self.dl[d].idx)
    }

    /// Whether some send item of the message was acknowledged (`ok`).
    pub fn acknowledged(&self, m: &Msg) -> bool {
        m.sends.iter().any(|s| self.send_out(*s) == Out::Ok)
    }

    /// The first `ok` send item of the message (by completion).
    pub fn first_ok(&self, m: &Msg) -> Option<SendRef> {
        m.sends
            .iter()
            .filter(|s| self.send_out(**s) == Out::Ok)
            .min_by_key(|s| self.h.ops[s.op].ret_pos())
            .copied()
    }

    /// The earliest invocation of a send of the message that did not fail: the earliest moment
    /// the system may have accepted it.
    pub fn first_live(&self, m: &Msg) -> Option<SendRef> {
        m.sends
            .iter()
            .filter(|s| self.send_out(**s) != Out::Fail)
            .min_by_key(|s| self.h.ops[s.op].inv.pos)
            .copied()
    }

    /// Deliveries of `m` in group `g`.
    pub fn deliveries(&self, m: &str, g: &Gk) -> &[usize] {
        self.by_mg
            .get(&(m.to_string(), g.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn acks_of(&self, m: &str, g: &Gk) -> &[AckRef] {
        self.acks
            .get(&(m.to_string(), g.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The groups of the message's channel that must receive it: those that exist from the start,
    /// and those whose first completed sub or recv precedes the message's first live send.
    pub fn must_groups(&self, m: &Msg) -> Vec<Gk> {
        let first = self.first_live(m).map(|s| self.h.ops[s.op].inv.pos);
        self.groups
            .iter()
            .filter(|(gk, g)| {
                gk.0 == m.q
                    && (g.from_start || matches!((g.joined, first), (Some(j), Some(f)) if j < f))
            })
            .map(|(gk, _)| gk.clone())
            .collect()
    }

    /// Whether a clock step lies strictly between two positions (and the declaration leaves such
    /// pairs undecided).
    pub fn clock_between(&self, a: usize, b: usize) -> bool {
        self.d.clock_steps == super::decl::ClockSteps::Undecided
            && self.h.clock_steps.iter().any(|c| c.pos > a && c.pos <= b)
    }
}

/// Builds the index.
pub fn build<'a>(h: &'a MHist, d: &'a Decl) -> Ix<'a> {
    let mut ix = Ix {
        h,
        d,
        msgs: BTreeMap::new(),
        dl: Vec::new(),
        by_mg: BTreeMap::new(),
        groups: BTreeMap::new(),
        acks: BTreeMap::new(),
    };
    let mut ops: Vec<&MOp> = h.ops.iter().collect();
    // completion order for deliveries (a receive's items are delivered at its completion)
    ops.sort_by_key(|o| (o.ret_pos(), o.inv.pos));
    for op in &h.ops {
        if op.kind == OpKind::Send {
            for (j, it) in op.items.iter().enumerate() {
                ix.msgs
                    .entry(it.m.clone())
                    .or_insert_with(|| Msg {
                        m: it.m.clone(),
                        q: op.q.clone(),
                        k: it.key().to_string(),
                        sends: Vec::new(),
                    })
                    .sends
                    .push(SendRef { op: op.i, item: j });
            }
        }
        if op.kind != OpKind::Send {
            let g = ix.groups.entry((op.q.clone(), op.g.clone())).or_default();
            if op.g == "-" {
                g.from_start = true;
            }
            if op.kind == OpKind::Sub && op.from.as_deref() == Some("start") {
                g.from_start = true;
            }
            if matches!(op.kind, OpKind::Sub | OpKind::Recv) && op.out == Out::Ok {
                let p = op.ret_pos();
                if g.joined.is_none_or(|j| p < j) {
                    g.joined = Some(p);
                }
            }
            if op.kind == OpKind::Recv && op.out == Out::Info {
                g.info_recvs.push(op.inv.pos);
            }
        }
    }
    for (i, dr) in h.drained.iter().enumerate() {
        let g = ix.groups.entry((dr.q.clone(), dr.g.clone())).or_default();
        if dr.g == "-" {
            g.from_start = true;
        }
        if g.drained.is_none() {
            g.drained = Some(i);
        }
    }
    for op in &ops {
        if op.kind == OpKind::Recv && op.out == Out::Ok {
            for (j, it) in op.items.iter().enumerate() {
                let g = (op.q.clone(), op.g.clone());
                let n = ix.dl.len();
                ix.dl.push(Dlv {
                    op: op.i,
                    idx: j,
                    m: it.m.clone(),
                    g: g.clone(),
                });
                ix.by_mg.entry((it.m.clone(), g)).or_default().push(n);
            }
        }
    }
    // acks, nacks, extensions and commits, matched to deliveries
    for op in &ops {
        let g = (op.q.clone(), op.g.clone());
        match op.kind {
            OpKind::Ack | OpKind::Nack | OpKind::Extend => {
                for it in &op.items {
                    let dlv = matched(&ix, op, &it.m, it.h.as_deref(), &g);
                    let r = AckRef {
                        op: op.i,
                        kind: op.kind,
                        item: it.clone(),
                        out: op.item_out(it),
                        dlv,
                    };
                    ix.acks
                        .entry((it.m.clone(), g.clone()))
                        .or_default()
                        .push(r);
                }
            }
            OpKind::Commit => {
                // every delivery of the key up to the offset, received before the commit
                let mut add = Vec::new();
                for (key, off) in &op.commits {
                    let Ok(off) = off.parse::<i64>() else {
                        continue;
                    };
                    for (n, x) in ix.dl.iter().enumerate() {
                        if x.g != g || ix.h.ops[x.op].ret_pos() >= op.inv.pos {
                            continue;
                        }
                        let it = &ix.h.ops[x.op].items[x.idx];
                        let o = it.o.as_deref().and_then(|o| o.parse::<i64>().ok());
                        if it.key() == key && o.is_some_and(|o| o <= off) {
                            add.push((n, it.clone()));
                        }
                    }
                }
                for (n, it) in add {
                    let r = AckRef {
                        op: op.i,
                        kind: OpKind::Ack,
                        out: op.out,
                        item: Item {
                            m: it.m.clone(),
                            o: it.o.clone(),
                            ..Item::default()
                        },
                        dlv: Some(n),
                    };
                    ix.acks.entry((it.m, g.clone())).or_default().push(r);
                }
            }
            _ => {}
        }
    }
    ix
}

/// The delivery an acknowledgement (or nack, or extension) is about: the one with its handle,
/// else the latest delivery of the message in the group received before it (by the same
/// process when there is one).
fn matched(ix: &Ix<'_>, op: &MOp, m: &str, h: Option<&str>, g: &Gk) -> Option<usize> {
    let ds = ix.deliveries(m, g);
    let before: Vec<usize> = ds
        .iter()
        .copied()
        .filter(|&d| ix.dlv_ret(d).pos < op.inv.pos)
        .collect();
    if let Some(h) = h {
        if let Some(d) = before
            .iter()
            .rev()
            .find(|&&d| ix.dlv_item(d).h.as_deref() == Some(h))
        {
            return Some(*d);
        }
    }
    before
        .iter()
        .rev()
        .find(|&&d| ix.dlv_op(d).proc_ == op.proc_)
        .or(before.last())
        .copied()
}
