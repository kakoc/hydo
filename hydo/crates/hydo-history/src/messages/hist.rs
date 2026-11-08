//! The records of a messages history (docs/design/messaging.md "The history") read into
//! operations: send, recv, ack, nack, extend, commit, sub, and the drain's `drained` records.
//!
//! Order is the records' position over the consoles (the workload console, then the recovery
//! console); time is the record's guest time, used only for durations between two records of
//! one boot. A completion's recorded time is raised by [`RESOLUTION_NS`] (the coarsest recorder
//! reads `/proc/uptime`, which rounds down to 10 ms), so every bound computed from it stays sound.

use crate::events::{Events, Kind};
use crate::register::pdec;
use std::collections::HashMap;

/// What a completion time may lag the true reply by (the shell and Rust recorders' 10 ms).
pub const RESOLUTION_NS: i128 = 10_000_000;

/// An operation's outcome (or a batch item's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    Ok,
    Fail,
    Info,
}

impl Out {
    pub fn word(self) -> &'static str {
        match self {
            Out::Ok => "ok",
            Out::Fail => "fail",
            Out::Info => "info",
        }
    }
}

/// Where and when a record was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    /// Position over all consoles (the order that counts).
    pub pos: usize,
    /// The console: 0 the workload's boot, 1 the recovery boot.
    pub boot: usize,
    /// Guest time in ns (a completion's raised by RESOLUTION_NS), None for `-`.
    pub t: Option<i128>,
    /// The time as recorded (for the explanations).
    pub shown: Option<i128>,
}

/// The duration from `a` to `b` in ns when both are of one boot and have times.
pub fn span(a: &At, b: &At) -> Option<i128> {
    if a.boot != b.boot {
        return None;
    }
    Some(b.t? - a.t?)
}

/// One item: a message id and its attributes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub m: String,
    pub k: Option<String>,
    pub x: Option<String>,
    pub d: Option<i64>,
    pub o: Option<String>,
    pub n: Option<i64>,
    pub h: Option<String>,
    pub v: Option<i64>,
    pub s: Option<Out>,
    /// `r=0` on a nack: do not requeue (a reject).
    pub r: Option<String>,
}

impl Item {
    /// The key, `-` when none.
    pub fn key(&self) -> &str {
        self.k.as_deref().unwrap_or("-")
    }

    /// The deduplication id, the message id when none.
    pub fn dedup(&self) -> &str {
        self.x.as_deref().unwrap_or(&self.m)
    }

    /// The token as the history wrote it (decoded values re-encoded).
    pub fn token(&self) -> String {
        let mut s = enc(&self.m);
        let mut kv = |k: &str, v: &Option<String>| {
            if let Some(v) = v {
                s.push_str(&format!(";{k}={}", enc(v)));
            }
        };
        kv("k", &self.k);
        kv("x", &self.x);
        kv("d", &self.d.map(|x| x.to_string()));
        kv("o", &self.o);
        kv("n", &self.n.map(|x| x.to_string()));
        kv("h", &self.h);
        kv("v", &self.v.map(|x| x.to_string()));
        kv("s", &self.s.map(|x| x.word().to_string()));
        kv("r", &self.r);
        s
    }

    /// Completion attributes over the invocation's.
    fn merge(&mut self, o: &Item) {
        macro_rules! take {
            ($($f:ident),*) => {$( if o.$f.is_some() { self.$f = o.$f.clone(); } )*};
        }
        take!(k, x, d, o, n, h, v, s, r);
    }
}

/// `enc` of the recorders, extended to `;` and `=`.
pub fn enc(s: &str) -> String {
    crate::register::penc(s, ",[]:;=")
}

fn num(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

/// Reads an item token. Err names what is wrong.
pub fn parse_item(tok: &str) -> Result<Item, String> {
    let mut parts = tok.split(';');
    let m = pdec(parts.next().unwrap_or(""));
    if m.is_empty() {
        return Err(format!("item {tok:?}: no message id"));
    }
    let mut it = Item {
        m,
        ..Item::default()
    };
    for p in parts {
        let Some((k, v)) = p.split_once('=') else {
            return Err(format!("item {tok:?}: attribute {p:?} without '='"));
        };
        let v = pdec(v);
        let bad = || format!("item {tok:?}: {k}={v}: a whole number");
        match k {
            "k" => it.k = Some(v),
            "x" => it.x = Some(v),
            "o" => it.o = Some(v),
            "h" => it.h = Some(v),
            "r" => it.r = Some(v),
            "d" => it.d = Some(num(&v).ok_or_else(bad)?),
            "n" => it.n = Some(num(&v).ok_or_else(bad)?),
            "v" => it.v = Some(num(&v).ok_or_else(bad)?),
            "s" => {
                it.s = Some(match v.as_str() {
                    "ok" => Out::Ok,
                    "fail" => Out::Fail,
                    "info" => Out::Info,
                    _ => return Err(format!("item {tok:?}: s={v}: ok, fail or info")),
                })
            }
            // an attribute this hydo does not know: a later recorder's; ignored
            _ => {}
        }
    }
    Ok(it)
}

/// The operations of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OpKind {
    Send,
    Recv,
    Ack,
    Nack,
    Extend,
    Commit,
    Sub,
}

impl OpKind {
    pub fn name(self) -> &'static str {
        match self {
            OpKind::Send => "send",
            OpKind::Recv => "recv",
            OpKind::Ack => "ack",
            OpKind::Nack => "nack",
            OpKind::Extend => "extend",
            OpKind::Commit => "commit",
            OpKind::Sub => "sub",
        }
    }
    fn parse(f: &str) -> Option<OpKind> {
        Some(match f {
            "send" => OpKind::Send,
            "recv" => OpKind::Recv,
            "ack" => OpKind::Ack,
            "nack" => OpKind::Nack,
            "extend" => OpKind::Extend,
            "commit" => OpKind::Commit,
            "sub" => OpKind::Sub,
            _ => return None,
        })
    }
}

/// One operation: an invocation and maybe its completion.
#[derive(Clone, Debug)]
pub struct MOp {
    pub i: usize,
    pub proc_: String,
    pub kind: OpKind,
    pub q: String,
    /// The group (`-` for a send).
    pub g: String,
    pub out: Out,
    pub inv: At,
    /// The completion of an `ok` or `fail` operation; None for an unknown outcome.
    pub ret: Option<At>,
    /// The items (a send's, an ack's, ...; a recv's deliveries), the completion's attributes
    /// merged over the invocation's.
    pub items: Vec<Item>,
    /// A commit's `<key>@<offset>` pairs.
    pub commits: Vec<(String, String)>,
    /// A sub's `from=` (`start` or `now`).
    pub from: Option<String>,
}

impl MOp {
    /// The completion position, or usize::MAX (never).
    pub fn ret_pos(&self) -> usize {
        self.ret.map(|r| r.pos).unwrap_or(usize::MAX)
    }

    /// An item's own outcome.
    pub fn item_out(&self, it: &Item) -> Out {
        match self.out {
            Out::Ok => it.s.unwrap_or(Out::Ok),
            o => o,
        }
    }
}

/// A `drained <q> <g>` record.
#[derive(Clone, Debug)]
pub struct Drained {
    pub proc_: String,
    pub q: String,
    pub g: String,
    pub at: At,
}

/// A `T=` clock step: the position of the controller's `fault.clock_back|fwd` record.
#[derive(Clone, Copy, Debug)]
pub struct ClockStep {
    pub pos: usize,
}

/// A history read for the messages checker.
#[derive(Clone, Debug, Default)]
pub struct MHist {
    pub ops: Vec<MOp>,
    pub drained: Vec<Drained>,
    pub clock_steps: Vec<ClockStep>,
    /// Records this checker cannot read (ignored, reported).
    pub warnings: Vec<String>,
    /// The console labels (for the explanations).
    pub labels: Vec<String>,
    /// Records of the history.
    pub records: usize,
}

fn time_of(ev: &Events, i: usize, done: bool) -> Option<i128> {
    let t = ev.events[i].time.as_ref()?;
    let v: i128 = t.text().parse().ok()?;
    Some(if done { v + RESOLUTION_NS } else { v })
}

fn at_of(ev: &Events, i: usize, done: bool) -> At {
    At {
        pos: ev.events[i].idx,
        boot: ev.events[i].src,
        t: time_of(ev, i, done),
        shown: time_of(ev, i, false),
    }
}

fn group_of(args: &[String], w: &str) -> Result<(String, String), String> {
    match args {
        [q, g, ..] => Ok((pdec(q), pdec(g))),
        _ => Err(format!("{w}: needs a channel and a group")),
    }
}

/// Reads the events of history `name`; `clock` holds the positions of the clock-step records
/// (see [`clock_steps`]).
pub fn read(ev: &Events, name: &str, clock: &[usize]) -> MHist {
    let mut h = MHist {
        labels: ev.labels.clone(),
        clock_steps: clock.iter().map(|&pos| ClockStep { pos }).collect(),
        ..MHist::default()
    };
    let mut open: HashMap<String, usize> = HashMap::new();
    // the invocation's raw arguments of each open op
    let mut inv_args: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, e) in ev.events.iter().enumerate() {
        if e.hist != name {
            continue;
        }
        h.records += 1;
        let wh = format!("{}:{}", ev.labels[e.src], e.line);
        if e.f == "drained" {
            if e.kind == Kind::Invoke {
                // the drain writes a completion only; an invocation of it is tolerated
                continue;
            }
            match group_of(&e.args, &wh) {
                Ok((q, g)) => h.drained.push(Drained {
                    proc_: e.proc_.clone(),
                    q,
                    g,
                    at: at_of(ev, i, false),
                }),
                Err(w) => h.warnings.push(format!("drained {w}")),
            }
            continue;
        }
        let Some(kind) = OpKind::parse(&e.f) else {
            h.warnings.push(format!(
                "{wh}: operation {} is not one of send, recv, ack, nack, extend, commit, sub, drained (ignored)",
                e.f
            ));
            continue;
        };
        if e.kind == Kind::Invoke {
            open.remove(&e.proc_);
            let parsed = parse_op(kind, &e.args, &wh);
            let (q, g, items, commits, from) = match parsed {
                Ok(x) => x,
                Err(w) => {
                    h.warnings.push(format!("{w} (ignored)"));
                    continue;
                }
            };
            let n = h.ops.len();
            h.ops.push(MOp {
                i: n,
                proc_: e.proc_.clone(),
                kind,
                q,
                g,
                out: Out::Info,
                inv: at_of(ev, i, false),
                ret: None,
                items,
                commits,
                from,
            });
            inv_args.insert(n, e.args.clone());
            open.insert(e.proc_.clone(), n);
            continue;
        }
        let Some(n) = open.remove(&e.proc_) else {
            h.warnings.push(format!(
                "{wh}: {} {} by process {} without an open invocation (ignored)",
                e.kind.name(),
                e.f,
                e.proc_
            ));
            continue;
        };
        if h.ops[n].kind != kind {
            h.warnings.push(format!(
                "{wh}: completion {} does not match the invocation {} (ignored)",
                e.f,
                h.ops[n].kind.name()
            ));
            open.insert(e.proc_.clone(), n);
            continue;
        }
        let out = match e.kind {
            Kind::Ok => Out::Ok,
            Kind::Fail => Out::Fail,
            _ => Out::Info,
        };
        let op = &mut h.ops[n];
        op.out = out;
        if out != Out::Info {
            op.ret = Some(at_of(ev, i, true));
        }
        if out == Out::Ok {
            match parse_op(kind, &e.args, &wh) {
                Ok((_, _, items, commits, from)) => {
                    if kind == OpKind::Recv {
                        op.items = items;
                    } else if !items.is_empty() {
                        for c in &items {
                            match op.items.iter_mut().find(|x| x.m == c.m) {
                                Some(x) => x.merge(c),
                                None => op.items.push(c.clone()),
                            }
                        }
                    }
                    if !commits.is_empty() {
                        op.commits = commits;
                    }
                    if from.is_some() {
                        op.from = from;
                    }
                }
                Err(w) => h
                    .warnings
                    .push(format!("{w} (the completion's items ignored)")),
            }
        }
        // a recv that did not complete ok returned nothing the client saw
        if kind == OpKind::Recv && out != Out::Ok {
            op.items.clear();
        }
    }
    h
}

type Parsed = (
    String,
    String,
    Vec<Item>,
    Vec<(String, String)>,
    Option<String>,
);

fn parse_op(kind: OpKind, args: &[String], wh: &str) -> Result<Parsed, String> {
    let w = format!("{wh}: {}", kind.name());
    let items = |a: &[String]| -> Result<Vec<Item>, String> {
        a.iter()
            .map(|t| parse_item(t).map_err(|e| format!("{w}: {e}")))
            .collect()
    };
    match kind {
        OpKind::Send => {
            let q = args
                .first()
                .ok_or_else(|| format!("{w}: needs a channel"))?;
            Ok((pdec(q), "-".into(), items(&args[1..])?, vec![], None))
        }
        OpKind::Commit => {
            let (q, g) = group_of(args, &w)?;
            let mut c = Vec::new();
            for t in &args[2..] {
                let Some((k, o)) = t.rsplit_once('@') else {
                    return Err(format!("{w}: {t:?}: <key>@<offset>"));
                };
                c.push((pdec(k), pdec(o)));
            }
            Ok((q, g, vec![], c, None))
        }
        OpKind::Sub => {
            let (q, g) = group_of(args, &w)?;
            let from = args[2..]
                .iter()
                .find_map(|t| t.strip_prefix("from="))
                .map(str::to_string);
            Ok((q, g, vec![], vec![], from))
        }
        _ => {
            let (q, g) = group_of(args, &w)?;
            Ok((q, g, items(&args[2..])?, vec![], None))
        }
    }
}

/// The clock-step records of the consoles: the controller's `HYDO-ASSERT` of `fault.clock_back`
/// or `fault.clock_fwd` (printed right after a `T=` step), as (console index, line number from
/// 1, the numbering of [`crate::events::parse_sources`]).
pub fn clock_lines<S: AsRef<str>, T: AsRef<str>>(sources: &[(S, T)]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (si, (_, text)) in sources.iter().enumerate() {
        for (ln0, line) in crate::py::splitlines(text.as_ref()).into_iter().enumerate() {
            if line.contains("HYDO-ASSERT ")
                && (line.contains("\"id\":\"fault.clock_back\"")
                    || line.contains("\"id\":\"fault.clock_fwd\""))
                && line.contains("\"ok\":true")
            {
                out.push((si, ln0 + 1));
            }
        }
    }
    out
}

/// A clock-step line's position in the order of the records: the number of HYDO-HIST records
/// before it.
pub fn clock_positions(ev: &Events, lines: &[(usize, usize)]) -> Vec<usize> {
    lines
        .iter()
        .map(|&(s, l)| ev.events.partition_point(|e| (e.src, e.line) < (s, l)))
        .collect()
}
