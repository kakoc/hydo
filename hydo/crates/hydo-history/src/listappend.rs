//! The Elle-style list-append checker: lists per key that clients append unique elements to and
//! read whole, in single operations or multi-key transactions. Direct checks (duplicates,
//! garbage and aborted reads, intermediate reads, internal consistency, incompatible orders, and
//! by consistency level lost appends, stale reads, monotonic reads, read-your-writes), then a
//! G0 / G1c / G-single / G2 cycle search over the dependency graph.
//!
//! Every loop runs in hydohist.py's order (sorted keys, insertion orders, the same BFS and Tarjan
//! traversal), so the anomalies, their order and their texts are the same. The one change is a
//! depth bound on the cycle search's BFS: it only stops searching paths that could not give a
//! strictly shorter cycle than the best one so far, which the reference would reject anyway, and
//! it leaves the BFS tree up to that depth unchanged.

use crate::events::{Events, Op, Status};
use crate::fx::{FxHashMap as HashMap, FxHashSet as HashSet};
use crate::py::{self, BigInt, Crash, PyVal};
use crate::register::pdec_cow as pdec;
use crate::{CheckErr, CheckResult, Outcome};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const LEVELS: [&str; 6] = [
    "strict-serializable",
    "strong-session-serializable",
    "serializable",
    "strong-snapshot-isolation",
    "strong-session-snapshot-isolation",
    "snapshot-isolation",
];

/// A cycle as its edges (from, to, edge type).
type Cycle = Vec<(usize, usize, u8)>;

const WW: u8 = 1;
const WR: u8 = 2;
const RW: u8 = 4;
const PROC: u8 = 8;
const RT: u8 = 16;

/// Interned keys and elements.
#[derive(Default)]
struct Strs {
    v: Vec<String>,
    ids: HashMap<String, u32>,
}

impl Strs {
    fn id(&mut self, s: std::borrow::Cow<'_, str>) -> u32 {
        if let Some(&i) = self.ids.get(s.as_ref()) {
            return i;
        }
        let i = self.v.len() as u32;
        let s = s.into_owned();
        self.v.push(s.clone());
        self.ids.insert(s, i);
        i
    }
    fn t(&self, i: u32) -> &str {
        &self.v[i as usize]
    }
    fn join(&self, l: &[u32]) -> String {
        l.iter().map(|&e| self.t(e)).collect::<Vec<_>>().join(",")
    }
}

#[derive(Debug, Clone)]
enum Mop {
    A(u32, u32),
    R(u32, Option<Vec<u32>>),
}

struct Txn {
    n: usize,
    op: usize,
    mops: Vec<Mop>,
}

fn parse_list(tok: &str, s: &mut Strs) -> Result<Vec<u32>, CheckErr> {
    if tok == "nil" || tok == "[]" {
        return Ok(Vec::new());
    }
    if !(tok.starts_with('[') && tok.ends_with(']')) || tok.len() < 2 {
        return Err(CheckErr::Hist(format!(
            "a list value is [e1,e2,...] (got {})",
            py::repr_str(tok)
        )));
    }
    let body = &tok[1..tok.len() - 1];
    if body.is_empty() {
        return Ok(Vec::new());
    }
    Ok(body.split(',').map(|x| s.id(pdec(x))).collect())
}

fn parse_mops(op: &Op, s: &mut Strs) -> Result<Vec<Mop>, CheckErr> {
    let a = op.args();
    let ok = op.status == Status::Ok;
    match op.f.as_str() {
        "append" => {
            if a.len() < 2 {
                return Err(CheckErr::Hist(format!(
                    "append by process {} needs <key> <element>",
                    op.proc_
                )));
            }
            let k = s.id(pdec(&a[0]));
            let e = s.id(pdec(&a[1]));
            Ok(vec![Mop::A(k, e)])
        }
        "read" => {
            if a.is_empty() {
                return Err(CheckErr::Hist(format!(
                    "read by process {} needs <key>",
                    op.proc_
                )));
            }
            let k = s.id(pdec(&a[0]));
            let l = if ok && a.len() > 1 {
                Some(parse_list(&a[1], s)?)
            } else {
                None
            };
            Ok(vec![Mop::R(k, l)])
        }
        "txn" => {
            let mut mops = Vec::new();
            for t in a {
                let parts: Vec<&str> = t.split(':').collect();
                if parts[0] == "a" && parts.len() == 3 {
                    let k = s.id(pdec(parts[1]));
                    let e = s.id(pdec(parts[2]));
                    mops.push(Mop::A(k, e));
                } else if parts[0] == "r" && (parts.len() == 2 || parts.len() == 3) {
                    let k = s.id(pdec(parts[1]));
                    let l = if ok && parts.len() == 3 {
                        Some(parse_list(parts[2], s)?)
                    } else {
                        None
                    };
                    mops.push(Mop::R(k, l));
                } else {
                    return Err(CheckErr::Hist(format!(
                        "txn micro-op {}: a:<key>:<elem>, r:<key> or r:<key>:[...]",
                        py::repr_str(t)
                    )));
                }
            }
            Ok(mops)
        }
        f => Err(CheckErr::Hist(format!(
            "operation {} is not part of the list-append model (append, read, txn)",
            py::repr_str(f)
        ))),
    }
}

struct Level {
    name: String,
    si: bool,
    rt: bool,
    proc_: bool,
}

fn level(spec: &PyVal) -> Result<Level, CheckErr> {
    let lv = spec
        .get("consistency")
        .cloned()
        .unwrap_or(PyVal::str("strict-serializable"));
    let Some(name) = lv.as_str().filter(|s| LEVELS.contains(s)) else {
        return Err(CheckErr::Hist(format!(
            "consistency {}: one of {}",
            lv.repr(),
            LEVELS.join(", ")
        )));
    };
    let rt = name.starts_with("strict") || name.starts_with("strong-snapshot");
    Ok(Level {
        name: name.to_string(),
        si: name.contains("snapshot-isolation"),
        rt,
        proc_: rt || name.starts_with("strong-session"),
    })
}

/// The dependency graph: adj[u] = {v: edge types}.
struct Graph {
    adj: Vec<BTreeMap<usize, u8>>,
}

fn pick(ts: u8) -> u8 {
    ts & ts.wrapping_neg()
}

/// Reused BFS buffers (stamp-marked, so a BFS costs only what it visits).
struct Bfs {
    stamp: u32,
    seen: Vec<u32>,
    prev: Vec<(usize, u8)>,
    depth: Vec<usize>,
}

impl Bfs {
    fn new(n: usize) -> Bfs {
        Bfs {
            stamp: 0,
            seen: vec![0; n],
            prev: vec![(0, 0); n],
            depth: vec![0; n],
        }
    }

    /// hydohist's bfs(src, dst, types) inside `nodes`, but only paths of at most `limit` edges
    /// (the reference would discard a longer one; the BFS tree up to `limit` is the same).
    fn path(
        &mut self,
        g: &Graph,
        nodes: &[bool],
        src: usize,
        dst: usize,
        types: u8,
        limit: usize,
    ) -> Option<Cycle> {
        if dst == src || limit == 0 {
            return None;
        }
        self.stamp += 1;
        let st = self.stamp;
        self.seen[src] = st;
        self.depth[src] = 0;
        let mut dq = VecDeque::new();
        dq.push_back(src);
        let mut found = false;
        'outer: while let Some(u) = dq.pop_front() {
            if self.depth[u] >= limit {
                continue;
            }
            for (&v, &t) in &g.adj[u] {
                if !nodes[v] || self.seen[v] == st {
                    continue;
                }
                let ts = t & types;
                if ts == 0 {
                    continue;
                }
                self.seen[v] = st;
                self.prev[v] = (u, pick(ts));
                self.depth[v] = self.depth[u] + 1;
                if v == dst {
                    found = true;
                    break 'outer;
                }
                dq.push_back(v);
            }
        }
        if !found {
            return None;
        }
        let mut path = Vec::new();
        let mut v = dst;
        while v != src {
            let (u, t) = self.prev[v];
            path.push((u, v, t));
            v = u;
        }
        path.reverse();
        Some(path)
    }
}

#[derive(Clone, Copy)]
enum Must {
    None,
    Rw1,
    RwPlus,
}

/// hydohist's _shortest_cycle: the shortest cycle within `comp` using edge types in `allowed`;
/// with Rw1 exactly one rw edge (G-single), with RwPlus at least one (G2).
fn shortest_cycle(
    g: &Graph,
    bfs: &mut Bfs,
    nodes: &[bool],
    comp: &[usize],
    allowed: u8,
    must: Must,
) -> Option<Cycle> {
    let mut best: Option<Cycle> = None;
    let rest = match must {
        Must::None => allowed,
        Must::Rw1 => allowed & !RW,
        Must::RwPlus => allowed | RW,
    };
    for &u in comp {
        for (&v, &t) in &g.adj[u] {
            if !nodes[v] || v == u {
                continue;
            }
            let first = match must {
                Must::None => {
                    let ts = t & allowed;
                    if ts == 0 {
                        continue;
                    }
                    pick(ts)
                }
                _ => {
                    if t & RW == 0 {
                        continue;
                    }
                    RW
                }
            };
            // a cycle is accepted only if strictly shorter than the best: back paths of at most
            // len(best) - 2 edges
            let limit = best
                .as_ref()
                .map_or(usize::MAX, |b| b.len().saturating_sub(2));
            if let Some(back) = bfs.path(g, nodes, v, u, rest, limit) {
                let mut cyc = vec![(u, v, first)];
                cyc.extend(back);
                if best.as_ref().is_none_or(|b| cyc.len() < b.len()) {
                    best = Some(cyc);
                }
            }
            if best.as_ref().is_some_and(|b| b.len() == 2) {
                return best; // no cycle is shorter than two edges
            }
        }
    }
    best
}

/// Tarjan's SCCs (hydohist's iterative traversal order): components of more than one node, each
/// sorted, in the order Tarjan completes them.
fn sccs(g: &Graph, nodes: &[usize], allowed: u8) -> Vec<Vec<usize>> {
    let n = g.adj.len();
    let mut index: Vec<Option<usize>> = vec![None; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut st: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    let mut counter = 0;
    let nbrs = |s: usize| -> Vec<usize> {
        g.adj[s]
            .iter()
            .filter(|(_, &t)| t & allowed != 0)
            .map(|(&v, _)| v)
            .collect()
    };
    for &s in nodes {
        if index[s].is_some() {
            continue;
        }
        let mut work: Vec<(usize, Vec<usize>, usize)> = vec![(s, nbrs(s), 0)];
        index[s] = Some(counter);
        low[s] = counter;
        counter += 1;
        st.push(s);
        on[s] = true;
        while let Some(top) = work.last_mut() {
            let u = top.0;
            let mut pushed = None;
            while top.2 < top.1.len() {
                let v = top.1[top.2];
                top.2 += 1;
                match index[v] {
                    None => {
                        pushed = Some(v);
                        break;
                    }
                    Some(iv) if on[v] => low[u] = low[u].min(iv),
                    _ => {}
                }
            }
            if let Some(v) = pushed {
                index[v] = Some(counter);
                low[v] = counter;
                counter += 1;
                st.push(v);
                on[v] = true;
                work.push((v, nbrs(v), 0));
                continue;
            }
            work.pop();
            if let Some(p) = work.last() {
                low[p.0] = low[p.0].min(low[u]);
            }
            if Some(low[u]) == index[u] {
                let mut comp = Vec::new();
                while let Some(w) = st.pop() {
                    on[w] = false;
                    comp.push(w);
                    if w == u {
                        break;
                    }
                }
                if comp.len() > 1 {
                    comp.sort_unstable();
                    out.push(comp);
                }
            }
        }
    }
    out
}

fn prefix_of(long: &[u32], short: &[u32]) -> bool {
    short.len() <= long.len() && long[..short.len()] == *short
}

/// `hydohist.check_list_append`.
pub fn check_list_append(ops: &[Op], ev: &Events, spec: &PyVal) -> Result<CheckResult, CheckErr> {
    let lv = level(spec)?;
    let mut s = Strs::default();
    let mut txns: Vec<Txn> = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        let mops = parse_mops(op, &mut s)?;
        txns.push(Txn {
            n: txns.len(),
            op: i,
            mops,
        });
    }
    let st = |t: &Txn| ops[t.op].status;
    let call = |t: usize| ops[txns[t].op].call;
    let ret = |t: usize| ops[txns[t].op].ret;
    let mut anomalies: Vec<(&'static str, String, Vec<usize>)> = Vec::new();

    // who appended what (insertion order kept); elements must be unique per key
    let mut appender: HashMap<(u32, u32), usize> = HashMap::default();
    let mut app_order: Vec<(u32, u32)> = Vec::new();
    for t in &txns {
        for m in &t.mops {
            if let Mop::A(k, e) = *m {
                match appender.get(&(k, e)) {
                    Some(&w) if w != t.n => {
                        return Err(CheckErr::Hist(format!(
                            "element {} appended to {} twice (T{w} and T{}): elements must be unique per key",
                            s.t(e),
                            s.t(k),
                            t.n
                        )))
                    }
                    Some(_) => {}
                    None => {
                        appender.insert((k, e), t.n);
                        app_order.push((k, e));
                    }
                }
            }
        }
    }
    let oks: Vec<usize> = txns
        .iter()
        .filter(|t| st(t) == Status::Ok)
        .map(|t| t.n)
        .collect();
    let mut reads: HashMap<u32, Vec<(usize, &[u32])>> = HashMap::default();
    for &t in &oks {
        for m in &txns[t].mops {
            if let Mop::R(k, Some(l)) = m {
                reads.entry(*k).or_default().push((t, l));
            }
        }
    }
    let mut rkeys: Vec<u32> = reads.keys().copied().collect();
    rkeys.sort_by(|a, b| s.t(*a).cmp(s.t(*b)));

    // per-read checks; mark[e] == stamp: element e is in the current read
    let mut mark = vec![0u32; s.v.len()];
    let mut stamp = 0u32;
    for &k in &rkeys {
        for &(t, l) in &reads[&k] {
            stamp += 1;
            let mut dup: Option<&str> = None;
            for &e in l {
                if mark[e as usize] == stamp {
                    let t = s.t(e);
                    if dup.is_none_or(|d| t < d) {
                        dup = Some(t);
                    }
                }
                mark[e as usize] = stamp;
            }
            if let Some(dup) = dup {
                anomalies.push((
                    "duplicate-elements",
                    format!(
                        "T{t} read {}=[{}]: element {dup} appears more than once",
                        s.t(k),
                        s.join(l)
                    ),
                    vec![t],
                ));
            }
            for &e in l {
                match appender.get(&(k, e)) {
                    None => anomalies.push((
                        "garbage-read",
                        format!(
                            "T{t} read element {} of {} that no operation appended",
                            s.t(e),
                            s.t(k)
                        ),
                        vec![t],
                    )),
                    Some(&w) if st(&txns[w]) == Status::Fail => anomalies.push((
                        "aborted-read",
                        format!(
                            "T{t} read element {} of {}, appended by a failed operation T{w} (G1a)",
                            s.t(e),
                            s.t(k)
                        ),
                        vec![t, w],
                    )),
                    _ => {}
                }
            }
            if let Some(&last) = l.last() {
                if let Some(&w) = appender.get(&(k, last)) {
                    if w != t {
                        let own: Vec<u32> = txns[w]
                            .mops
                            .iter()
                            .filter_map(|m| match *m {
                                Mop::A(kk, e) if kk == k => Some(e),
                                _ => None,
                            })
                            .collect();
                        if own.last().is_some_and(|&x| x != last) {
                            let pos = own.iter().position(|&x| x == last).unwrap_or(0);
                            anomalies.push((
                                "intermediate-read",
                                format!(
                                    "T{t} read {}=[...,{}], an intermediate state of T{w}, which then appended {} (G1b)",
                                    s.t(k),
                                    s.t(last),
                                    s.t(own[pos + 1])
                                ),
                                vec![t, w],
                            ));
                        }
                    }
                }
            }
        }
    }
    // internal consistency: a transaction sees its own earlier appends
    for &t in &oks {
        let mut mine: HashMap<u32, Vec<u32>> = HashMap::default();
        let mut last: HashMap<u32, Vec<u32>> = HashMap::default();
        for m in &txns[t].mops {
            match m {
                Mop::A(k, e) => {
                    mine.entry(*k).or_default().push(*e);
                    if let Some(l) = last.get_mut(k) {
                        l.push(*e);
                    }
                }
                Mop::R(_, None) => {}
                Mop::R(k, Some(l)) => {
                    if let Some(want) = last.get(k) {
                        if want != l {
                            anomalies.push((
                                "internal",
                                format!(
                                    "T{t} read {}=[{}] but its own earlier operations imply [{}]",
                                    s.t(*k),
                                    s.join(l),
                                    s.join(want)
                                ),
                                vec![t],
                            ));
                        }
                    } else if let Some(mn) = mine.get(k) {
                        let tail = &l[l.len().saturating_sub(mn.len())..];
                        if tail != mn.as_slice() {
                            anomalies.push((
                                "internal",
                                format!(
                                    "T{t} read {}=[{}] without its own earlier appends {}",
                                    s.t(*k),
                                    s.join(l),
                                    s.join(mn)
                                ),
                                vec![t],
                            ));
                        }
                    }
                    last.insert(*k, l.clone());
                }
            }
        }
    }
    // version order per key: every read must be a prefix of the longest one
    let mut vorder: Vec<(u32, &[u32])> = Vec::new();
    for &k in &rkeys {
        let mut ls: Vec<(usize, &[u32])> = reads[&k].clone();
        ls.sort_by_key(|&(t, l)| (std::cmp::Reverse(l.len()), t));
        let (lt, longest) = ls[0];
        let mut ok = true;
        for &(t, l) in &ls[1..] {
            if !prefix_of(longest, l) {
                anomalies.push((
                    "incompatible-order",
                    format!(
                        "{}: T{lt} read [{}] and T{t} read [{}]: neither is a prefix of the other",
                        s.t(k),
                        s.join(longest),
                        s.join(l)
                    ),
                    vec![lt, t],
                ));
                ok = false;
                break;
            }
        }
        if ok {
            vorder.push((k, longest));
        }
    }
    // real time / process order: lost appends, stale reads, monotonic reads, read-your-writes
    if lv.rt {
        let mut app_by_key: HashMap<u32, Vec<(u32, usize)>> = HashMap::default();
        for &(kk, e) in &app_order {
            let w = appender[&(kk, e)];
            if st(&txns[w]) == Status::Ok {
                app_by_key.entry(kk).or_default().push((e, w));
            }
        }
        for &k in &rkeys {
            let rk = &reads[&k];
            for &(t, l) in rk {
                stamp += 1;
                for &e in l {
                    mark[e as usize] = stamp;
                }
                for &(e, w) in app_by_key.get(&k).map(Vec::as_slice).unwrap_or(&[]) {
                    if mark[e as usize] == stamp || w == t {
                        continue;
                    }
                    if ret(w).unwrap_or(usize::MAX) < call(t) {
                        anomalies.push((
                            "lost-append",
                            format!(
                                "{}: T{w} appended {} and was acknowledged, but T{t}, which began after that, read [{}] without it",
                                s.t(k),
                                s.t(e),
                                s.join(l)
                            ),
                            vec![w, t],
                        ));
                        break;
                    }
                }
            }
            let mut prev: Vec<(usize, &[u32])> = rk.clone();
            prev.sort_by_key(|&(t, _)| call(t));
            'stale: for &(t1, l1) in &prev {
                for &(t2, l2) in &prev {
                    if ret(t1).unwrap_or(usize::MAX) < call(t2)
                        && l2.len() < l1.len()
                        && prefix_of(l1, l2)
                    {
                        anomalies.push((
                            "stale-read",
                            format!(
                                "{}: T{t1} read [{}], then T{t2}, which began after T{t1} completed, read the older [{}]",
                                s.t(k),
                                s.join(l1),
                                s.join(l2)
                            ),
                            vec![t1, t2],
                        ));
                        break 'stale;
                    }
                }
            }
        }
    }
    let mut by_call: Vec<usize> = (0..txns.len()).collect();
    by_call.sort_by_key(|&t| call(t));
    if lv.proc_ {
        let mut byp: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for &t in &by_call {
            byp.entry(ops[txns[t].op].proc_.as_str())
                .or_default()
                .push(t);
        }
        for (p, ts) in &byp {
            let mut seen: HashMap<u32, (usize, &[u32])> = HashMap::default();
            let mut wrote: HashMap<u32, Vec<(usize, u32)>> = HashMap::default();
            for &t in ts {
                if st(&txns[t]) != Status::Ok {
                    continue;
                }
                for m in &txns[t].mops {
                    match m {
                        Mop::A(k, e) => wrote.entry(*k).or_default().push((t, *e)),
                        Mop::R(_, None) => {}
                        Mop::R(k, Some(l)) => {
                            if let Some(&(t0, l0)) = seen.get(k) {
                                if t0 != t && l.len() < l0.len() && prefix_of(l0, l) {
                                    anomalies.push((
                                        "monotonic-read",
                                        format!(
                                            "process {p} read {}=[{}] (T{t0}), then the older {}=[{}] (T{t}): its reads went back in time",
                                            s.t(*k),
                                            s.join(l0),
                                            s.t(*k),
                                            s.join(l)
                                        ),
                                        vec![t0, t],
                                    ));
                                }
                            }
                            let own = wrote.get(k).map(Vec::as_slice).unwrap_or(&[]);
                            if !own.is_empty() {
                                stamp += 1;
                                for &e in l.iter() {
                                    mark[e as usize] = stamp;
                                }
                            }
                            for &(tw, e) in own {
                                if tw != t && mark[e as usize] != stamp {
                                    anomalies.push((
                                        "read-your-writes",
                                        format!(
                                            "process {p} appended {} to {} (T{tw}, acknowledged), then read {}=[{}] without it (T{t})",
                                            s.t(e),
                                            s.t(*k),
                                            s.t(*k),
                                            s.join(l)
                                        ),
                                        vec![tw, t],
                                    ));
                                    break;
                                }
                            }
                            seen.insert(*k, (t, l));
                        }
                    }
                }
            }
        }
    }
    // dependency graph: ww, wr, rw from the version orders; rt and process edges by level
    let nt = txns.len();
    let mut committed = vec![false; nt];
    for &t in &oks {
        committed[t] = true;
    }
    let vsets: HashMap<u32, HashSet<u32>> = vorder
        .iter()
        .map(|&(k, vo)| (k, vo.iter().copied().collect()))
        .collect();
    for &(k, e) in &app_order {
        let w = appender[&(k, e)];
        if st(&txns[w]) == Status::Info && vsets.get(&k).is_some_and(|vs| vs.contains(&e)) {
            committed[w] = true; // an unknown-outcome append that was observed committed
        }
    }
    let mut g = Graph {
        adj: vec![BTreeMap::new(); nt],
    };
    let mut why: HashMap<(usize, usize, u8), String> = HashMap::default();
    let mut edge = |g: &mut Graph, u: usize, v: usize, typ: u8, reason: &dyn Fn() -> String| {
        if u == v || !committed[u] || !committed[v] {
            return;
        }
        *g.adj[u].entry(v).or_insert(0) |= typ;
        why.entry((u, v, typ)).or_insert_with(reason);
    };
    for &(k, vo) in &vorder {
        for p in vo.windows(2) {
            let (a, b) = (p[0], p[1]);
            if let (Some(&wa), Some(&wb)) = (appender.get(&(k, a)), appender.get(&(k, b))) {
                edge(&mut g, wa, wb, WW, &|| {
                    format!(
                        "T{wa} appended {} to {} before T{wb} appended {}",
                        s.t(a),
                        s.t(k),
                        s.t(b)
                    )
                });
            }
        }
        for &(t, l) in &reads[&k] {
            if let Some(&last) = l.last() {
                if let Some(&w) = appender.get(&(k, last)) {
                    edge(&mut g, w, t, WR, &|| {
                        format!(
                            "T{t} read {}=[...,{}], which T{w} appended",
                            s.t(k),
                            s.t(last)
                        )
                    });
                }
            }
            if l.len() < vo.len() && prefix_of(vo, l) {
                let nx = vo[l.len()];
                if let Some(&w) = appender.get(&(k, nx)) {
                    edge(&mut g, t, w, RW, &|| {
                        format!(
                            "T{t} read {}=[{}], missing {}, which T{w} appended next",
                            s.t(k),
                            s.join(l),
                            s.t(nx)
                        )
                    });
                }
            }
        }
    }
    let mut extra = 0u8;
    if lv.rt {
        extra |= RT;
        let mut evs: Vec<(usize, u8, usize)> = Vec::new();
        for (t, &c) in committed.iter().enumerate() {
            if c {
                evs.push((call(t), 0, t));
                if let Some(r) = ret(t) {
                    evs.push((r, 1, t));
                }
            }
        }
        evs.sort_unstable();
        let mut frontier: BTreeSet<usize> = BTreeSet::new();
        let mut preds: Vec<(usize, Vec<usize>)> = Vec::new();
        let mut pred_of: HashMap<usize, usize> = HashMap::default();
        for &(_, kind, n) in &evs {
            if kind == 0 {
                pred_of.insert(n, preds.len());
                preds.push((n, frontier.iter().copied().collect()));
            } else {
                if let Some(&i) = pred_of.get(&n) {
                    for u in &preds[i].1 {
                        frontier.remove(u);
                    }
                }
                frontier.insert(n);
            }
        }
        for (n, ps) in &preds {
            for &u in ps {
                let n = *n;
                edge(&mut g, u, n, RT, &|| {
                    format!("T{u} completed before T{n} began")
                });
            }
        }
    }
    if lv.proc_ {
        extra |= PROC;
        let mut order: Vec<&str> = Vec::new();
        let mut byp: HashMap<&str, Vec<usize>> = HashMap::default();
        for &t in &by_call {
            if committed[t] {
                let p = ops[txns[t].op].proc_.as_str();
                if !byp.contains_key(p) {
                    order.push(p);
                }
                byp.entry(p).or_default().push(t);
            }
        }
        for p in order {
            for w in byp[p].windows(2) {
                let (a, b) = (w[0], w[1]);
                edge(&mut g, a, b, PROC, &|| {
                    format!("process {p} ran T{a} before T{b}")
                });
            }
        }
    }
    let allowed_all = WW | WR | RW | extra;
    let nodes_all: Vec<usize> = (0..nt).filter(|&t| committed[t]).collect();
    let mut classes: Vec<(&str, u8, Must)> = vec![
        ("G0", WW, Must::None),
        ("G1c", WW | WR, Must::None),
        ("G-single", WW | WR, Must::Rw1),
    ];
    if !lv.si {
        classes.push(("G2", WW | WR, Must::RwPlus));
    }
    let mut found: Vec<(String, Cycle)> = Vec::new();
    let mut bfs = Bfs::new(nt);
    let mut in_comp = vec![false; nt];
    for comp in sccs(&g, &nodes_all, allowed_all) {
        for &c in &comp {
            in_comp[c] = true;
        }
        for &(name, base, must) in &classes {
            let mut cyc = shortest_cycle(&g, &mut bfs, &in_comp, &comp, base, must);
            let mut suffix = "";
            if cyc.is_none() && extra != 0 {
                cyc = shortest_cycle(&g, &mut bfs, &in_comp, &comp, base | extra, must);
                if let Some(c) = &cyc {
                    let used = c.iter().fold(0u8, |a, &(_, _, t)| a | t);
                    suffix = if used & RT != 0 {
                        "-realtime"
                    } else if used & PROC != 0 {
                        "-process"
                    } else {
                        ""
                    };
                }
            }
            if let Some(c) = cyc {
                found.push((format!("{name}{suffix}"), c));
                break;
            }
        }
        for &c in &comp {
            in_comp[c] = false;
        }
    }
    let mut cyc_anoms: Vec<(String, String, Vec<usize>)> = Vec::new();
    for (name, cyc) in found {
        let text = format!(
            "cycle of {} transactions: {}",
            cyc.len(),
            cyc.iter()
                .map(|e| why[e].as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
        cyc_anoms.push((name, text, cyc.iter().map(|&(u, _, _)| u).collect()));
    }
    let mut all: Vec<(String, String, Vec<usize>)> = anomalies
        .into_iter()
        .map(|(k, t, n)| (k.to_string(), t, n))
        .collect();
    all.extend(cyc_anoms);

    let nops = txns.iter().filter(|t| st(t) != Status::Fail).count();
    let mut keyset: HashSet<u32> = app_order.iter().map(|&(k, _)| k).collect();
    keyset.extend(rkeys.iter().copied());
    let int = |x: usize| PyVal::Int(BigInt::from_i64(x as i64));
    let mut det: Vec<(String, PyVal)> = vec![
        ("ops".into(), int(nops)),
        ("keys".into(), int(keyset.len())),
        ("consistency".into(), PyVal::str(&lv.name)),
    ];
    if all.is_empty() {
        return Ok(CheckResult {
            status: Outcome::Ok,
            summary: format!(
                "{nops} operations on {} keys: no anomaly ({})",
                keyset.len(),
                lv.name
            ),
            details: det,
            explanation: String::new(),
            warnings: Vec::new(),
        });
    }
    let mut kinds: Vec<&str> = Vec::new();
    for a in &all {
        if !kinds.contains(&a.0.as_str()) {
            kinds.push(&a.0);
        }
    }
    det.push(("anomaly".into(), PyVal::str(&all[0].0)));
    det.push(("anomalies".into(), PyVal::str(&kinds.join(","))));
    let mut lines = vec![format!("anomalies ({}): {}", lv.name, kinds.join(", "))];
    for (kind, text, ns) in all.iter().take(20) {
        lines.push(format!("{kind}: {text}"));
        for &n in ns {
            lines.push(format!("    {}", label(&txns[n], ops, ev, &s)?));
        }
    }
    Ok(CheckResult {
        status: Outcome::Violation,
        summary: format!("{}: {}", all[0].0, all[0].1),
        details: det,
        explanation: lines.join("\n"),
        warnings: Vec::new(),
    })
}

fn label(t: &Txn, ops: &[Op], ev: &Events, s: &Strs) -> Result<String, Crash> {
    let op = &ops[t.op];
    let mo: Vec<String> = t
        .mops
        .iter()
        .map(|m| match m {
            Mop::A(k, e) => format!("append {} {}", s.t(*k), s.t(*e)),
            Mop::R(k, None) => format!("read {} ?", s.t(*k)),
            Mop::R(k, Some(l)) => format!("read {} [{}]", s.t(*k), s.join(l)),
        })
        .collect();
    Ok(format!(
        "T{} p{} {} ({}, {})",
        t.n,
        op.proc_,
        mo.join("; "),
        op.status.words(),
        op.where_(ev)?
    ))
}
