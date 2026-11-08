//! Messaging histories (docs/design/messaging.md "The history"): record what a client does with
//! a message-passing system -- send, receive, acknowledge, give back, extend a lease, commit an
//! offset, subscribe -- and the host checks exactly the claims the project declares (a
//! `type: messages` check) and measures the history. hydo knows nothing about the system: the
//! client says what happened, in these records.
//!
//! ```
//! use hydo_assert::history::messages::{Item, Messages};
//! # struct Delivery { id: String, count: u64, handle: String }
//! # fn send(_b: &[&str]) -> Result<Vec<u64>, bool> { Ok(vec![81, 82]) }
//! # fn receive() -> Vec<Delivery> { vec![Delivery { id: "m17".into(), count: 1, handle: "a91f".into() }] }
//! let q = Messages::new("q");
//! let op = q.send(3, "orders", &[Item::new("m17").key("g2"), Item::new("m18").key("g2")]);
//! match send(&["m17", "m18"]) {
//!     Ok(ids) => op.ok(&[Item::new("m17").key("g2").id(ids[0]), Item::new("m18").key("g2").id(ids[1])]),
//!     Err(true) => op.fail(),          // refused: definitely not stored
//!     Err(false) => op.info(),         // a timeout: it may be stored, later or never
//! }
//! let r = q.recv(5, "orders", "-");
//! let got = receive();
//! let items: Vec<Item> = got.iter().map(|d| Item::new(&d.id).attempt(d.count).handle(&d.handle)).collect();
//! r.ok(&items);
//! q.ack(5, "orders", "-", &[Item::new("m17").handle("a91f")]).ok_same();
//! q.drained(9, "orders", "-");          // the drain read everything at the end
//! ```
//!
//! The lines are `HYDO-HIST <history> <process> <type> <time> <op> <channel> [<group>] <item>...`
//! with an item `m17;k=g2;x=d17;d=5000;o=81;n=2;h=a91f;v=30000;s=ok` (message id, then key,
//! deduplication id, delay, system id or offset, attempt count, handle, lease, the item's own
//! outcome in a batch, and `r=0` for a reject), values encoded with [`enc`](super::enc) extended
//! to `;` and `=`. They are the generic records, so any client can write them with
//! [`History`](super::History) too.

use super::{enc, History, Op};
use std::collections::HashSet;
use std::sync::Mutex;

/// A value of an item: [`enc`](super::enc), and `;` and `=` encoded too.
pub fn item_enc(s: &str) -> String {
    enc(s).replace(';', "%3B").replace('=', "%3D")
}

/// The outcome of one item of a batch (`s=`), when the system answered per item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Accepted.
    Ok,
    /// Refused: it had no effect.
    Fail,
    /// Unknown.
    Info,
}

/// One message in a record: its id (unique per sending intent) and what the client knows about
/// it. The attributes are written in one fixed order, whatever order they were set in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    m: String,
    k: Option<String>,
    x: Option<String>,
    d: Option<u64>,
    o: Option<String>,
    n: Option<u64>,
    h: Option<String>,
    v: Option<u64>,
    s: Option<Outcome>,
    r: bool,
}

impl Item {
    /// The message id `m` the producer chose (put it into the message's body).
    pub fn new(m: &str) -> Item {
        Item { m: m.to_string(), ..Item::default() }
    }
    /// The ordering key (`k=`).
    pub fn key(mut self, k: &str) -> Item {
        self.k = Some(k.to_string());
        self
    }
    /// The deduplication id (`x=`), when it is not the message id.
    pub fn dedup(mut self, x: &str) -> Item {
        self.x = Some(x.to_string());
        self
    }
    /// The delay the send asked for, in milliseconds (`d=`).
    pub fn delay_ms(mut self, d: u64) -> Item {
        self.d = Some(d);
        self
    }
    /// The id or offset the system gave the stored message (`o=`).
    pub fn id(mut self, o: impl std::fmt::Display) -> Item {
        self.o = Some(o.to_string());
        self
    }
    /// The attempt count the system reported for a delivery (`n=`).
    pub fn attempt(mut self, n: u64) -> Item {
        self.n = Some(n);
        self
    }
    /// What the consumer presents to acknowledge this delivery (`h=`).
    pub fn handle(mut self, h: &str) -> Item {
        self.h = Some(h.to_string());
        self
    }
    /// The lease the system gave this delivery, or the new length of an extension, in
    /// milliseconds (`v=`).
    pub fn lease_ms(mut self, v: u64) -> Item {
        self.v = Some(v);
        self
    }
    /// The item's own outcome inside a batch whose request was answered (`s=`).
    pub fn outcome(mut self, s: Outcome) -> Item {
        self.s = Some(s);
        self
    }
    /// A nack that must not requeue (a reject, `r=0`).
    pub fn reject(mut self) -> Item {
        self.r = true;
        self
    }
    /// The item as one token.
    pub fn token(&self) -> String {
        let mut t = item_enc(&self.m);
        let mut kv = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                t.push(';');
                t.push_str(k);
                t.push('=');
                t.push_str(&item_enc(&v));
            }
        };
        kv("k", self.k.clone());
        kv("x", self.x.clone());
        kv("d", self.d.map(|x| x.to_string()));
        kv("o", self.o.clone());
        kv("n", self.n.map(|x| x.to_string()));
        kv("h", self.h.clone());
        kv("v", self.v.map(|x| x.to_string()));
        kv(
            "s",
            self.s.map(|s| {
                match s {
                    Outcome::Ok => "ok",
                    Outcome::Fail => "fail",
                    Outcome::Info => "info",
                }
                .to_string()
            }),
        );
        kv("r", if self.r { Some("0".to_string()) } else { None });
        t
    }
}

fn tokens(items: &[Item]) -> Vec<String> {
    items.iter().map(Item::token).collect()
}

/// The recorder of one messaging history (a check's `history:`).
#[derive(Debug)]
pub struct Messages {
    h: History,
    sent: Mutex<HashSet<String>>,
}

/// An operation waiting for its completion. Its completion writes the channel (and group) again,
/// then the items given.
#[derive(Debug)]
pub struct MsgOp {
    op: Op,
    head: Vec<String>,
}

impl MsgOp {
    fn done(self, typ: &str, items: Option<&[Item]>) {
        let mut a = self.head.clone();
        match items {
            Some(i) => a.extend(tokens(i)),
            None => a.extend(self.op.args[self.head.len()..].iter().cloned()),
        }
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        self.op.h.record_raw(&self.op.proc_, typ, &self.op.f, &refs);
    }
    /// It succeeded; `items` are what the reply said (a send's system ids, a receive's
    /// deliveries: none for an empty receive).
    pub fn ok(self, items: &[Item]) {
        self.done("ok", Some(items));
    }
    /// It succeeded, with the invocation's items (an acknowledgement, a commit, a subscription).
    pub fn ok_same(self) {
        self.done("ok", None);
    }
    /// It definitely had no effect.
    pub fn fail(self) {
        self.done("fail", None);
    }
    /// The outcome is unknown: it may have taken effect at any later time, or never.
    pub fn info(self) {
        self.done("info", None);
    }
}

impl Messages {
    /// The recorder for history `name` (`[A-Za-z0-9_.-]+`).
    pub fn new(name: &str) -> Messages {
        Messages { h: History::new(name), sent: Mutex::new(HashSet::new()) }
    }

    fn invoke(&self, proc_: &dyn std::fmt::Display, f: &str, head: Vec<String>, rest: Vec<String>) -> MsgOp {
        let mut a = head.clone();
        a.extend(rest);
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        MsgOp { op: self.h.invoke(proc_, f, &refs), head }
    }

    /// Records a send of new messages (a new sending intent each). Panics when a message id was
    /// already sent by this recorder: a retry of the same intent is [`Messages::resend`].
    pub fn send(&self, proc_: impl std::fmt::Display, channel: &str, items: &[Item]) -> MsgOp {
        if let Ok(mut s) = self.sent.lock() {
            for i in items {
                assert!(
                    s.insert(i.m.clone()),
                    "message id {} was already sent for another intent: give each intent its own id, and record a retry of the same intent with resend",
                    i.m
                );
            }
        }
        self.resend(proc_, channel, items)
    }

    /// Records a send that retries an intent sent before, with the same message ids.
    pub fn resend(&self, proc_: impl std::fmt::Display, channel: &str, items: &[Item]) -> MsgOp {
        if let Ok(mut s) = self.sent.lock() {
            for i in items {
                s.insert(i.m.clone());
            }
        }
        self.invoke(&proc_, "send", vec![item_enc(channel)], tokens(items))
    }

    /// Records a receive (a poll or a fetch) of a group (`-`: the channel's one group). Push
    /// delivery writes a receive whose completion follows at once.
    pub fn recv(&self, proc_: impl std::fmt::Display, channel: &str, group: &str) -> MsgOp {
        self.invoke(&proc_, "recv", vec![item_enc(channel), item_enc(group)], vec![])
    }

    /// Records an acknowledgement of deliveries (by message id, and handle where the system has
    /// them).
    pub fn ack(&self, proc_: impl std::fmt::Display, channel: &str, group: &str, items: &[Item]) -> MsgOp {
        self.invoke(&proc_, "ack", vec![item_enc(channel), item_enc(group)], tokens(items))
    }

    /// Records giving deliveries back before their lease ends ([`Item::reject`]: do not requeue).
    pub fn nack(&self, proc_: impl std::fmt::Display, channel: &str, group: &str, items: &[Item]) -> MsgOp {
        self.invoke(&proc_, "nack", vec![item_enc(channel), item_enc(group)], tokens(items))
    }

    /// Records extending the leases of deliveries ([`Item::lease_ms`]: the new length).
    pub fn extend(&self, proc_: impl std::fmt::Display, channel: &str, group: &str, items: &[Item]) -> MsgOp {
        self.invoke(&proc_, "extend", vec![item_enc(channel), item_enc(group)], tokens(items))
    }

    /// Records committing offsets: every delivery of each key up to its offset is acknowledged.
    pub fn commit(&self, proc_: impl std::fmt::Display, channel: &str, group: &str, offsets: &[(&str, u64)]) -> MsgOp {
        let o: Vec<String> = offsets.iter().map(|(k, n)| format!("{}@{}", item_enc(k), n)).collect();
        self.invoke(&proc_, "commit", vec![item_enc(channel), item_enc(group)], o)
    }

    /// Records joining a group or creating a subscription; `from_start`: it receives the
    /// channel's messages from the start, else from now.
    pub fn sub(&self, proc_: impl std::fmt::Display, channel: &str, group: &str, from_start: bool) -> MsgOp {
        let from = if from_start { "from=start" } else { "from=now" };
        self.invoke(&proc_, "sub", vec![item_enc(channel), item_enc(group)], vec![from.to_string()])
    }

    /// Records that the drain has seen the end of the group's messages (one completion record,
    /// no invocation).
    pub fn drained(&self, proc_: impl std::fmt::Display, channel: &str, group: &str) {
        self.h.record_raw(proc_, "ok", "drained", &[&item_enc(channel), &item_enc(group)]);
    }
}
