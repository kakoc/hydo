//! What a fault acts on beyond one service (Milestone 194, docs/design/jepsen-gaps.md): several
//! services named in a list, a selector over a group of services that the manifest declares
//! (`groups:`), and the partition shapes of a network fault.
//!
//! - `K=`, `P=` and `X=` take a [`Target`]: `<svc>`, `<svc>,<svc>[,...]` (every service of the
//!   list at once), or `<group>.<selector>` with the selectors `one[.<i>]`, `minority[.<i>]`,
//!   `majority[.<i>]`, `all` and `primary`. `one`, `minority` and `majority` choose among the
//!   group's subsets of their size: the subsets of the members in the manifest's order, in
//!   lexicographic order of the members' positions, and `<i>` (0 when not written) is the
//!   subset's number, counted modulo the number of subsets. So the choice is a pure function of
//!   the token and the manifest. `primary` is the member the group's `primary:` probe command
//!   prints when the fault starts, so it follows the cluster's leader.
//! - `N=` takes a [`NetTarget`], which gained two forms: components `<a>[,<b>...]/<c>[,<d>...]`
//!   (every link between two services in different components is cut, both ways) and a shape over
//!   a group `<group>.<shape>`: `halves[.<i>]`, `isolate[.<i>]`, `ring`, `bridge[.<i>]` and
//!   `primary` ([`Shape`]). A shape is a set of directed links ([`Shape::links`]); the host
//!   controller applies them with the per-link machinery of `N=a-b` and lifts them at the
//!   window's end, draining the queues as every network fault does.
//!
//! Service and group names are `[a-z0-9_]+`, so `,`, `/` and `.` are free to separate them.

use crate::num::{dec, err, name};
use crate::ParseError;
use std::fmt;

/// A selector over a group's members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Selector {
    /// `one[.<i>]`: one member, the i-th (modulo the group's size).
    One(u64),
    /// `minority[.<i>]`: the i-th subset of floor(n/2) members (n = the group's size).
    Minority(u64),
    /// `majority[.<i>]`: the i-th subset of floor(n/2)+1 members.
    Majority(u64),
    /// `all`: every member.
    All,
    /// `primary`: the member the group's `primary:` probe prints when the fault starts.
    Primary,
}

impl Selector {
    fn parse(s: &str, what: &str) -> Result<Selector, ParseError> {
        let (word, idx) = match s.split_once('.') {
            Some((w, i)) => {
                let i = dec(i, what)?;
                if i == 0 {
                    return Err(err(format!(
                        "{what}: write no .0 (the first subset is chosen without it)"
                    )));
                }
                (w, Some(i))
            }
            None => (s, None),
        };
        let i = idx.unwrap_or(0);
        let sel = match word {
            "one" => Selector::One(i),
            "minority" => Selector::Minority(i),
            "majority" => Selector::Majority(i),
            "all" | "primary" if idx.is_some() => {
                return Err(err(format!("{what}: {word} takes no index")))
            }
            "all" => Selector::All,
            "primary" => Selector::Primary,
            _ => {
                return Err(err(format!(
                    "{what}: {word:?} is not a selector (one, minority, majority, all, primary)"
                )))
            }
        };
        Ok(sel)
    }

    /// How many members the selector picks from a group of `n` (None for `primary`: one).
    pub fn size(self, n: usize) -> usize {
        match self {
            Selector::One(_) | Selector::Primary => 1,
            Selector::Minority(_) => n / 2,
            Selector::Majority(_) => n / 2 + 1,
            Selector::All => n,
        }
    }

    /// The members picked from `members` (in the manifest's order), or None for `primary`
    /// (the probe decides) and for a group too small for the selector.
    pub fn pick(self, members: &[String]) -> Option<Vec<String>> {
        let n = members.len();
        let k = self.size(n);
        if n == 0 || k == 0 || k > n {
            return None;
        }
        match self {
            Selector::Primary => None,
            Selector::All => Some(members.to_vec()),
            Selector::One(i) | Selector::Minority(i) | Selector::Majority(i) => {
                let c = subset(n, k, i);
                Some(c.into_iter().map(|j| members[j].clone()).collect())
            }
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Selector::One(_) => "one",
            Selector::Minority(_) => "minority",
            Selector::Majority(_) => "majority",
            Selector::All => "all",
            Selector::Primary => "primary",
        }
    }

    fn index(self) -> u64 {
        match self {
            Selector::One(i) | Selector::Minority(i) | Selector::Majority(i) => i,
            Selector::All | Selector::Primary => 0,
        }
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())?;
        if self.index() > 0 {
            write!(f, ".{}", self.index())?;
        }
        Ok(())
    }
}

/// The number of k-subsets of n things (saturating; groups are small).
pub fn binomial(n: usize, k: usize) -> u64 {
    if k > n {
        return 0;
    }
    let k = k.min(n - k);
    let mut c: u64 = 1;
    for j in 0..k {
        c = c.saturating_mul((n - j) as u64) / (j as u64 + 1);
    }
    c
}

/// The positions of the `i`-th k-subset of 0..n (modulo the number of subsets), the subsets in
/// lexicographic order of their sorted positions.
pub fn subset(n: usize, k: usize, i: u64) -> Vec<usize> {
    let total = binomial(n, k).max(1);
    let mut i = i % total;
    let mut out = Vec::with_capacity(k);
    let mut start = 0;
    for left in (1..=k).rev() {
        // the first position p (>= start) whose subsets, counted with p first, contain i
        let mut p = start;
        loop {
            let with_p = binomial(n - p - 1, left - 1);
            if i < with_p {
                break;
            }
            i -= with_p;
            p += 1;
        }
        out.push(p);
        start = p + 1;
    }
    out
}

/// What `K=`, `P=` and `X=` act on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// One service.
    Service(String),
    /// `a,b[,...]`: several services at once (at least two, each once).
    List(Vec<String>),
    /// `<group>.<selector>`: members of a group of the manifest's `groups:`.
    Group { group: String, sel: Selector },
}

impl Target {
    pub(crate) fn parse(s: &str, what: &str) -> Result<Target, ParseError> {
        if let Some((g, sel)) = s.split_once('.') {
            name(g, what)?;
            return Ok(Target::Group {
                group: g.to_string(),
                sel: Selector::parse(sel, what)?,
            });
        }
        if s.contains(',') {
            let l = names_list(s, what)?;
            if l.len() < 2 {
                return Err(err(format!("{what}: a list names at least two services")));
            }
            return Ok(Target::List(l));
        }
        name(s, what)?;
        Ok(Target::Service(s.to_string()))
    }

    /// The one service of a plain target (None for a list or a group selector).
    pub fn service(&self) -> Option<&str> {
        match self {
            Target::Service(s) => Some(s),
            _ => None,
        }
    }

    /// The services named in the target itself (a group selector names none).
    pub fn named(&self) -> Vec<&str> {
        match self {
            Target::Service(s) => vec![s.as_str()],
            Target::List(l) => l.iter().map(String::as_str).collect(),
            Target::Group { .. } => Vec::new(),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Service(s) => f.write_str(s),
            Target::List(l) => f.write_str(&l.join(",")),
            Target::Group { group, sel } => write!(f, "{group}.{sel}"),
        }
    }
}

/// `a,b,c`: service names separated by `,`, each once.
fn names_list(s: &str, what: &str) -> Result<Vec<String>, ParseError> {
    let mut out: Vec<String> = Vec::new();
    for p in s.split(',') {
        name(p, what)?;
        if out.iter().any(|x| x == p) {
            return Err(err(format!("{what}: {p} is named twice")));
        }
        out.push(p.to_string());
    }
    Ok(out)
}

/// A partition shape over a group's members (`N=<group>.<shape>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    /// `halves[.<i>]`: the i-th subset of floor(n/2) members against the rest.
    Halves(u64),
    /// `isolate[.<i>]`: the i-th member (modulo n) against the rest.
    Isolate(u64),
    /// `ring`: every member hears a majority (itself and the next floor(n/2) members in the
    /// manifest's order, around), and no two members hear the same majority. One way: a member
    /// drops the packets from the members it does not hear.
    Ring,
    /// `bridge[.<i>]`: the i-th member (modulo n) is the bridge; the others in the manifest's
    /// order are split into a first half (floor((n-1)/2) of them) and the rest; the halves cannot
    /// reach each other, the bridge reaches both.
    Bridge(u64),
    /// `primary`: the member the group's `primary:` probe prints, against the rest.
    Primary,
}

impl Shape {
    fn parse(s: &str, what: &str) -> Result<Shape, ParseError> {
        let (word, idx) = match s.split_once('.') {
            Some((w, i)) => {
                let i = dec(i, what)?;
                if i == 0 {
                    return Err(err(format!(
                        "{what}: write no .0 (the first choice is made without it)"
                    )));
                }
                (w, Some(i))
            }
            None => (s, None),
        };
        let i = idx.unwrap_or(0);
        Ok(match word {
            "halves" => Shape::Halves(i),
            "isolate" => Shape::Isolate(i),
            "bridge" => Shape::Bridge(i),
            "ring" | "primary" if idx.is_some() => {
                return Err(err(format!("{what}: {word} takes no index")))
            }
            "ring" => Shape::Ring,
            "primary" => Shape::Primary,
            _ => {
                return Err(err(format!(
                    "{what}: {word:?} is not a partition shape (halves, isolate, ring, bridge, \
                     primary)"
                )))
            }
        })
    }

    pub fn word(self) -> &'static str {
        match self {
            Shape::Halves(_) => "halves",
            Shape::Isolate(_) => "isolate",
            Shape::Ring => "ring",
            Shape::Bridge(_) => "bridge",
            Shape::Primary => "primary",
        }
    }

    fn index(self) -> u64 {
        match self {
            Shape::Halves(i) | Shape::Isolate(i) | Shape::Bridge(i) => i,
            Shape::Ring | Shape::Primary => 0,
        }
    }

    /// The fewest members the shape needs.
    pub fn min_members(self) -> usize {
        match self {
            Shape::Halves(_) | Shape::Isolate(_) | Shape::Primary => 2,
            Shape::Ring | Shape::Bridge(_) => 3,
        }
    }

    /// The directed links the shape cuts, as (from, to): the packets from `from` to `to` are
    /// faulted. `primary` is the probe's answer for [`Shape::Primary`] (ignored otherwise).
    /// None when the group is too small or the primary is not a member.
    pub fn links(self, members: &[String], primary: Option<&str>) -> Option<Vec<(String, String)>> {
        let n = members.len();
        if n < self.min_members() {
            return None;
        }
        let split = |a: Vec<usize>| -> Vec<(String, String)> {
            let b: Vec<usize> = (0..n).filter(|j| !a.contains(j)).collect();
            cross(members, &[a, b])
        };
        Some(match self {
            Shape::Halves(i) => split(subset(n, n / 2, i)),
            Shape::Isolate(i) => split(vec![(i % n as u64) as usize]),
            Shape::Primary => {
                let p = members.iter().position(|m| Some(m.as_str()) == primary)?;
                split(vec![p])
            }
            Shape::Bridge(i) => {
                let b = (i % n as u64) as usize;
                let rest: Vec<usize> = (0..n).filter(|j| *j != b).collect();
                let h = rest.len() / 2;
                cross(members, &[rest[..h].to_vec(), rest[h..].to_vec()])
            }
            Shape::Ring => {
                let m = n / 2 + 1;
                let mut out = Vec::new();
                for to in 0..n {
                    let heard: Vec<usize> = (0..m).map(|d| (to + d) % n).collect();
                    for from in 0..n {
                        if !heard.contains(&from) {
                            out.push((members[from].clone(), members[to].clone()));
                        }
                    }
                }
                out
            }
        })
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())?;
        if self.index() > 0 {
            write!(f, ".{}", self.index())?;
        }
        Ok(())
    }
}

/// Every directed link between members of different components, in the order: component by
/// component, member by member, then the other end in the members' order.
fn cross(members: &[String], comps: &[Vec<usize>]) -> Vec<(String, String)> {
    let comp_of = |j: usize| comps.iter().position(|c| c.contains(&j));
    let mut out = Vec::new();
    for a in 0..members.len() {
        for b in 0..members.len() {
            if a != b && comp_of(a).is_some() && comp_of(b).is_some() && comp_of(a) != comp_of(b) {
                out.push((members[a].clone(), members[b].clone()));
            }
        }
    }
    out
}

/// Components `a,b/c,d`: every service once, at least two components.
pub(crate) fn parse_components(s: &str, what: &str) -> Result<Vec<Vec<String>>, ParseError> {
    let mut comps: Vec<Vec<String>> = Vec::new();
    for c in s.split('/') {
        let l = names_list(c, what)?;
        if comps.iter().flatten().any(|x| l.contains(x)) {
            return Err(err(format!(
                "{what}: a service is in one component of a partition only"
            )));
        }
        comps.push(l);
    }
    if comps.len() < 2 {
        return Err(err(format!(
            "{what}: a partition has two components or more"
        )));
    }
    Ok(comps)
}

/// The directed links cut between the components of a partition (both ways).
pub fn component_links(comps: &[Vec<String>]) -> Vec<(String, String)> {
    let members: Vec<String> = comps.iter().flatten().cloned().collect();
    let mut idx = Vec::new();
    let mut k = 0;
    for c in comps {
        idx.push((k..k + c.len()).collect::<Vec<usize>>());
        k += c.len();
    }
    cross(&members, &idx)
}

pub(crate) fn parse_shape(s: &str, what: &str) -> Result<(String, Shape), ParseError> {
    let (g, sh) = s
        .split_once('.')
        .ok_or_else(|| err(format!("{what}: <group>.<shape>")))?;
    name(g, what)?;
    Ok((g.to_string(), Shape::parse(sh, what)?))
}
