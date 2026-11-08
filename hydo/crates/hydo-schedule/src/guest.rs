//! The guest part of a schedule: `run` and the tokens the guest's init applies (hydo-init.sh
//! explore_run and its extensions, and until M146 the stack's stack-explore.sh).
//!
//! The engines type the guest part on the guest's serial console and the init logs it, so the
//! console -- and the run's fingerprint -- contains the text exactly as written. A guest token
//! is therefore never rewritten: a token that is not in the canonical form explore.py prints
//! (leading zeros, upper-case hex, an empty field) is refused with a message, and every accepted
//! token prints back byte for byte.

use crate::compose::Compose;
use crate::num::{dec, err, hex, name, sdec};
use crate::storage::Corruption;
use crate::target::{component_links, parse_components, parse_shape, Shape, Target};
use crate::ParseError;
use std::fmt;

/// A network fault's netem argument (explore.py netem_args): `delay,<n>ms` or `<kind>,<n>%`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Netem {
    /// `delay,<ms>ms`
    Delay(u64),
    /// `loss,<n>%`
    Loss(u64),
    /// `duplicate,<n>%`
    Duplicate(u64),
    /// `reorder,<n>%` (explore.py puts `delay,20ms` before it when no delay is drawn)
    Reorder(u64),
    /// `corrupt,<n>%`
    Corrupt(u64),
}

impl Netem {
    fn parse_list(s: &str, what: &str) -> Result<Vec<Netem>, ParseError> {
        let t: Vec<&str> = s.split(',').collect();
        if !t.len().is_multiple_of(2) || t.is_empty() {
            return Err(err(format!(
                "{what}: netem arguments come in pairs <kind>,<value> (got {s:?})"
            )));
        }
        let mut out = Vec::new();
        for p in t.chunks(2) {
            let (k, v) = (p[0], p[1]);
            let pct = |v: &str| -> Result<u64, ParseError> {
                let n = v.strip_suffix('%').ok_or_else(|| {
                    err(format!("{what}: {k},{v}: the value is a percentage (<n>%)"))
                })?;
                dec(n, what)
            };
            out.push(match k {
                "delay" => {
                    let n = v
                        .strip_suffix("ms")
                        .ok_or_else(|| err(format!("{what}: delay,{v}: the delay is <n>ms")))?;
                    Netem::Delay(dec(n, what)?)
                }
                "loss" => Netem::Loss(pct(v)?),
                "duplicate" => Netem::Duplicate(pct(v)?),
                "reorder" => Netem::Reorder(pct(v)?),
                "corrupt" => Netem::Corrupt(pct(v)?),
                _ => {
                    return Err(err(format!(
                        "{what}: netem kind {k:?} (delay, loss, duplicate, reorder, corrupt)"
                    )))
                }
            });
        }
        Ok(out)
    }
}

impl fmt::Display for Netem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Netem::Delay(n) => write!(f, "delay,{n}ms"),
            Netem::Loss(n) => write!(f, "loss,{n}%"),
            Netem::Duplicate(n) => write!(f, "duplicate,{n}%"),
            Netem::Reorder(n) => write!(f, "reorder,{n}%"),
            Netem::Corrupt(n) => write!(f, "corrupt,{n}%"),
        }
    }
}

fn write_netem(f: &mut fmt::Formatter<'_>, l: &[Netem]) -> fmt::Result {
    for (i, n) in l.iter().enumerate() {
        if i > 0 {
            f.write_str(",")?;
        }
        write!(f, "{n}")?;
    }
    Ok(())
}

/// What a network fault acts on: a service's port, or a link between services with `addr:`
/// (`a-b`, or `a-*` = a against every other; manifest_space.py, hydo-init-links.sh), and since
/// Milestone 195 a partition (`a,b/c`: every link between two components) or a shape over a group
/// (`<group>.<shape>`, `target.rs`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NetTarget {
    Service(String),
    Link(String, String),
    LinkAll(String),
    /// `a[,b...]/c[,d...][/...]`: the components of a partition.
    Partition(Vec<Vec<String>>),
    /// `<group>.<shape>`: a partition shape over a group's members.
    Shape {
        group: String,
        shape: Shape,
    },
}

impl NetTarget {
    fn parse(s: &str, what: &str) -> Result<NetTarget, ParseError> {
        if s.contains('/') {
            return Ok(NetTarget::Partition(parse_components(s, what)?));
        }
        if s.contains('.') {
            let (group, shape) = parse_shape(s, what)?;
            return Ok(NetTarget::Shape { group, shape });
        }
        match s.split_once('-') {
            None => {
                name(s, what)?;
                Ok(NetTarget::Service(s.to_string()))
            }
            Some((a, "*")) => {
                name(a, what)?;
                Ok(NetTarget::LinkAll(a.to_string()))
            }
            Some((a, b)) => {
                name(a, what)?;
                name(b, what)?;
                Ok(NetTarget::Link(a.to_string(), b.to_string()))
            }
        }
    }

    /// Whether the target is a partition (components or a group's shape): a set of links cut
    /// together.
    pub fn is_partition(&self) -> bool {
        matches!(self, NetTarget::Partition(_) | NetTarget::Shape { .. })
    }

    /// The directed links a partition of components cuts (None for the other targets).
    pub fn component_links(&self) -> Option<Vec<(String, String)>> {
        match self {
            NetTarget::Partition(c) => Some(component_links(c)),
            _ => None,
        }
    }
}

impl fmt::Display for NetTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetTarget::Service(s) => f.write_str(s),
            NetTarget::Link(a, b) => write!(f, "{a}-{b}"),
            NetTarget::LinkAll(a) => write!(f, "{a}-*"),
            NetTarget::Partition(c) => {
                let t: Vec<String> = c.iter().map(|x| x.join(",")).collect();
                f.write_str(&t.join("/"))
            }
            NetTarget::Shape { group, shape } => write!(f, "{group}.{shape}"),
        }
    }
}

/// `P=`: a pause (SIGSTOP .. SIGCONT).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Pause {
    /// `P=<target>@<at>+<n>`: stopped before step at, continued before step at+n.
    Steps { target: Target, at: u64, steps: u64 },
    /// `P=<target>@<at>~<off>:<dur>`: a timer armed before step at stops it after off ms, for
    /// dur ms.
    Timer {
        target: Target,
        at: u64,
        off_ms: u64,
        dur_ms: u64,
    },
}

/// `O=`'s direction: `req` (packets to the service's port) or `resp` (packets from it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Req,
    Resp,
}

/// `F=`'s direction: `>` (the n-th packet the frozen service sends) or `<` (receives).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FreezeDir {
    Sends,
    Receives,
}

/// `F=<svc><dir><peer>@<step>[#<n>][:<match>]:<dur>` (hydo-init-freeze.sh; explore.py FC_RE).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Freeze {
    pub svc: String,
    pub dir: FreezeDir,
    /// a service, or `workload` (the workload's own client connections)
    pub peer: String,
    pub step: u64,
    /// `#<n>` as written; None = not written (the guest then counts to 1). `#0` = trace only.
    pub n: Option<u64>,
    /// the payload string to match (`%xx` escapes), None = every packet
    pub matching: Option<String>,
    pub dur_ms: u64,
}

impl Freeze {
    fn parse(v: &str) -> Result<Freeze, ParseError> {
        // FC_RE: ^([a-z0-9_]+)([<>])([a-z0-9_]+)@(\d+)(?:#(\d+))?(?::([^:]*))?:(\d+)$
        let what = format!("F={v}");
        let (head, rest) = v.split_once('@').ok_or_else(|| {
            err(format!(
                "{what}: no @ (F=<svc>[<>]<peer>@<step>[#n][:match]:<dur>)"
            ))
        })?;
        let (svc, dir, peer) = if let Some((a, b)) = head.split_once('>') {
            (a, FreezeDir::Sends, b)
        } else if let Some((a, b)) = head.split_once('<') {
            (a, FreezeDir::Receives, b)
        } else {
            return Err(err(format!(
                "{what}: no > or < between the service and its peer"
            )));
        };
        name(svc, &what)?;
        name(peer, &what)?;
        let (rest, dur) = rest
            .rsplit_once(':')
            .ok_or_else(|| err(format!("{what}: no :<dur>")))?;
        let dur_ms = dec(dur, &what)?;
        let (sn, matching) = match rest.split_once(':') {
            Some((a, m)) => {
                if m.is_empty() || m.contains(':') {
                    // FC_RE takes an empty match (explore.py then drops it when printing, the
                    // guest reads it as "every packet"): not canonical, refused
                    return Err(err(format!(
                        "{what}: the match string is not empty and has no ':'"
                    )));
                }
                (a, Some(m.to_string()))
            }
            None => (rest, None),
        };
        let (step, n) = match sn.split_once('#') {
            Some((s, n)) => (dec(s, &what)?, Some(dec(n, &what)?)),
            None => (dec(sn, &what)?, None),
        };
        Ok(Freeze {
            svc: svc.to_string(),
            dir,
            peer: peer.to_string(),
            step,
            n,
            matching,
            dur_ms,
        })
    }
}

impl fmt::Display for Freeze {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = match self.dir {
            FreezeDir::Sends => '>',
            FreezeDir::Receives => '<',
        };
        write!(f, "{}{d}{}@{}", self.svc, self.peer, self.step)?;
        if let Some(n) = self.n {
            write!(f, "#{n}")?;
        }
        if let Some(m) = &self.matching {
            write!(f, ":{m}")?;
        }
        write!(f, ":{}", self.dur_ms)
    }
}

/// `status=` of an `H=` rule: an HTTP code / gRPC code number, or a gRPC status name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Code(u64),
    Name(String),
}

/// One rule of an `H=` token (hydo-world's `Rule::parse` and `parse_ext`, hydo/crates/hydo-world/src/rules.rs).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    /// `status=<code|NAME>`
    Status(Status),
    /// `retry-after=<value>` (passed through as the header's value)
    RetryAfter(String),
    /// `delay=<ms>`
    Delay(u64),
    /// `late=<ms>` (a webhook comes late)
    Late(u64),
    /// `p=<1..100>`: the share of matching requests
    P(u64),
    /// `drop`: no answer
    Drop,
    /// `reset`, `reset=stream`, `reset=conn`
    Reset(Option<ResetKind>),
    /// `dup` (webhook)
    Dup,
    /// `reorder` (webhook)
    Reorder,
    /// `lost` (webhook)
    Lost,
    /// `effect=yes|no`: whether the outside world acted all the same
    Effect(bool),
    /// `rpc=<method>` (gRPC proxies)
    Rpc(String),
    /// `call=<n>` or `call=<a>-<b>` (gRPC proxies; counted from 1)
    Call(u64, Option<u64>),
    /// `cut=<n messages>` (gRPC)
    Cut(u64),
    /// `hold=<n messages>` (gRPC)
    Hold(u64),
    /// `dir=req|resp` (gRPC cut/hold side)
    Dir(Direction),
    /// `seed=<value>`: the seeded share's seed (any non-empty value)
    Seed(String),
}

/// `reset=`'s value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResetKind {
    Stream,
    Conn,
}

fn plain_value(v: &str, what: &str) -> Result<String, ParseError> {
    if v.is_empty() || v.contains(',') {
        return Err(err(format!("{what}: a value, without ','")));
    }
    Ok(v.to_string())
}

impl Rule {
    fn parse(kv: &str, what: &str) -> Result<Rule, ParseError> {
        let what = format!("{what}: rule {kv:?}");
        let (k, v) = match kv.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (kv, None),
        };
        fn need<'a>(v: Option<&'a str>, what: &str) -> Result<&'a str, ParseError> {
            v.ok_or_else(|| err(format!("{what}: needs =<value>")))
        }
        let none = |v: Option<&str>, r: Rule| match v {
            None => Ok(r),
            Some(_) => Err(err(format!("{what}: takes no value"))),
        };
        Ok(match k {
            "status" => {
                let v = need(v, &what)?;
                if v.bytes().all(|b| b.is_ascii_digit()) {
                    Rule::Status(Status::Code(dec(v, &what)?))
                } else if !v.is_empty() && v.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
                    Rule::Status(Status::Name(v.to_string()))
                } else {
                    return Err(err(format!(
                        "{what}: a status code or an upper-case gRPC status name"
                    )));
                }
            }
            "retry-after" => Rule::RetryAfter(plain_value(need(v, &what)?, &what)?),
            "delay" => Rule::Delay(dec(need(v, &what)?, &what)?),
            "late" => Rule::Late(dec(need(v, &what)?, &what)?),
            "p" => Rule::P(dec(need(v, &what)?, &what)?),
            "drop" => none(v, Rule::Drop)?,
            "dup" => none(v, Rule::Dup)?,
            "reorder" => none(v, Rule::Reorder)?,
            "lost" => none(v, Rule::Lost)?,
            "reset" => match v {
                None => Rule::Reset(None),
                Some("stream") => Rule::Reset(Some(ResetKind::Stream)),
                Some("conn") => Rule::Reset(Some(ResetKind::Conn)),
                Some(_) => return Err(err(format!("{what}: reset=stream or reset=conn"))),
            },
            "effect" => match need(v, &what)? {
                "yes" => Rule::Effect(true),
                "no" => Rule::Effect(false),
                _ => return Err(err(format!("{what}: effect=yes or effect=no"))),
            },
            "rpc" => Rule::Rpc(plain_value(need(v, &what)?, &what)?),
            "call" => {
                let v = need(v, &what)?;
                match v.split_once('-') {
                    Some((a, b)) => {
                        let (a, b) = (dec(a, &what)?, dec(b, &what)?);
                        if a < 1 || b < a {
                            return Err(err(format!("{what}: call=<a>-<b>, 1 <= a <= b")));
                        }
                        Rule::Call(a, Some(b))
                    }
                    None => {
                        let a = dec(v, &what)?;
                        if a < 1 {
                            return Err(err(format!("{what}: calls are counted from 1")));
                        }
                        Rule::Call(a, None)
                    }
                }
            }
            "cut" => Rule::Cut(dec(need(v, &what)?, &what)?),
            "hold" => Rule::Hold(dec(need(v, &what)?, &what)?),
            "dir" => match need(v, &what)? {
                "req" => Rule::Dir(Direction::Req),
                "resp" => Rule::Dir(Direction::Resp),
                _ => return Err(err(format!("{what}: dir=req or dir=resp"))),
            },
            "seed" => Rule::Seed(plain_value(need(v, &what)?, &what)?),
            _ => {
                // hydo-world refuses an unknown rule when the fault is switched on; an empty rule
                // (a stray ',') is accepted there and refused here
                return Err(err(format!("{what}: unknown rule")));
            }
        })
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rule::Status(Status::Code(n)) => write!(f, "status={n}"),
            Rule::Status(Status::Name(s)) => write!(f, "status={s}"),
            Rule::RetryAfter(v) => write!(f, "retry-after={v}"),
            Rule::Delay(n) => write!(f, "delay={n}"),
            Rule::Late(n) => write!(f, "late={n}"),
            Rule::P(n) => write!(f, "p={n}"),
            Rule::Drop => f.write_str("drop"),
            Rule::Reset(None) => f.write_str("reset"),
            Rule::Reset(Some(ResetKind::Stream)) => f.write_str("reset=stream"),
            Rule::Reset(Some(ResetKind::Conn)) => f.write_str("reset=conn"),
            Rule::Dup => f.write_str("dup"),
            Rule::Reorder => f.write_str("reorder"),
            Rule::Lost => f.write_str("lost"),
            Rule::Effect(true) => f.write_str("effect=yes"),
            Rule::Effect(false) => f.write_str("effect=no"),
            Rule::Rpc(m) => write!(f, "rpc={m}"),
            Rule::Call(a, None) => write!(f, "call={a}"),
            Rule::Call(a, Some(b)) => write!(f, "call={a}-{b}"),
            Rule::Cut(n) => write!(f, "cut={n}"),
            Rule::Hold(n) => write!(f, "hold={n}"),
            Rule::Dir(Direction::Req) => f.write_str("dir=req"),
            Rule::Dir(Direction::Resp) => f.write_str("dir=resp"),
            Rule::Seed(s) => write!(f, "seed={s}"),
        }
    }
}

/// `H=<target>@<from>-<to>:<rule>,...`: a protocol fault on a fake or proxy (hydo-init-fakes.sh).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProtocolFault {
    /// `<fake|proxy>` or `<fake|proxy>.<route>` (explore.py H_RE)
    pub target: String,
    pub from: u64,
    pub to: u64,
    pub rules: Vec<Rule>,
}

impl ProtocolFault {
    fn parse(v: &str) -> Result<ProtocolFault, ParseError> {
        // H_RE: ^([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)?)@(\d+)-(\d+):(\S+)$
        let what = format!("H={v}");
        let (target, rest) = v
            .split_once('@')
            .ok_or_else(|| err(format!("{what}: H=<target>@<from>-<to>:<rules>")))?;
        let part_ok = |p: &str| {
            p.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        let tok = match target.split_once('.') {
            Some((a, b)) => part_ok(a) && part_ok(b),
            None => part_ok(target),
        };
        if !tok {
            return Err(err(format!(
                "{what}: the target is <name> or <name>.<route> (lower case)"
            )));
        }
        let (rng, rules) = rest
            .split_once(':')
            .ok_or_else(|| err(format!("{what}: no :<rules>")))?;
        let (a, b) = rng
            .split_once('-')
            .ok_or_else(|| err(format!("{what}: the window is <from>-<to>")))?;
        let (from, to) = (dec(a, &what)?, dec(b, &what)?);
        if rules.is_empty() {
            return Err(err(format!("{what}: no rules")));
        }
        let rules = rules
            .split(',')
            .map(|r| Rule::parse(r, &what))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ProtocolFault {
            target: target.to_string(),
            from,
            to,
            rules,
        })
    }
}

impl fmt::Display for ProtocolFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}-{}:", self.target, self.from, self.to)?;
        for (i, r) in self.rules.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{r}")?;
        }
        Ok(())
    }
}

/// `B=<seed hex>[,<enable permille>[,<fire permille>]]` (hydo-init-buggify.sh, the SDKs' BUGGIFY).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Buggify {
    pub seed: u64,
    pub enable_permille: Option<u64>,
    pub fire_permille: Option<u64>,
}

/// What a reaction does (`A=`'s action, M139).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ReactAction {
    /// `cut`: a power cut at the reaction point (the workload and every service frozen, then the
    /// recovery boot), as `C=` does between two steps.
    Cut,
    /// `kill-<svc>`: SIGKILL the service's process tree; it stays down until the heal's restart
    /// pass, as after `X=`.
    Kill(String),
    /// `stop-<svc>-<ms>`: SIGSTOP the service's tree for ms of guest time.
    Stop(String, u64),
    /// `cont-<svc>`: SIGCONT the service (a `P=` pause healed at the reaction point instead of
    /// at its step).
    Cont(String),
}

impl ReactAction {
    fn parse(v: &str, what: &str) -> Result<ReactAction, ParseError> {
        if v == "cut" {
            return Ok(ReactAction::Cut);
        }
        let bad = || {
            err(format!(
                "{what}: the action is cut, kill-<svc>, stop-<svc>-<ms> or cont-<svc>"
            ))
        };
        let (kind, rest) = v.split_once('-').ok_or_else(bad)?;
        Ok(match kind {
            "kill" => {
                name(rest, what)?;
                ReactAction::Kill(rest.to_string())
            }
            "cont" => {
                name(rest, what)?;
                ReactAction::Cont(rest.to_string())
            }
            "stop" => {
                let (svc, ms) = rest.rsplit_once('-').ok_or_else(bad)?;
                name(svc, what)?;
                let ms = dec(ms, what)?;
                if ms == 0 {
                    return Err(err(format!("{what}: a stop lasts at least 1 ms")));
                }
                ReactAction::Stop(svc.to_string(), ms)
            }
            _ => return Err(bad()),
        })
    }

    /// The service the action acts on (none for a cut).
    pub fn svc(&self) -> Option<&str> {
        match self {
            ReactAction::Cut => None,
            ReactAction::Kill(s) | ReactAction::Stop(s, _) | ReactAction::Cont(s) => Some(s),
        }
    }
}

impl fmt::Display for ReactAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReactAction::Cut => f.write_str("cut"),
            ReactAction::Kill(s) => write!(f, "kill-{s}"),
            ReactAction::Stop(s, ms) => write!(f, "stop-{s}-{ms}"),
            ReactAction::Cont(s) => write!(f, "cont-{s}"),
        }
    }
}

/// `A=<event>[@<step>][+<ms>]:<action>` (M139, reactive exploration): from before workload step
/// `step` (0 when not written) the host controller watches the guest's console for the first
/// assertion record of `event` whose outcome is the event's (`ok:true` for a `sometimes` or
/// `reachable` assertion, `ok:false` for the others), waits `ms` of guest time and does the
/// action. The schedule string is the whole run: the controller's decision is a pure function of
/// it and of the guest's console, so the run reproduces from the string and from its control
/// transcript. `@0` and `+0` are not written (the canonical form).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct React {
    pub event: String,
    pub at: u64,
    pub delay_ms: u64,
    pub action: ReactAction,
}

impl React {
    fn parse(v: &str) -> Result<React, ParseError> {
        let what = format!("A={v}");
        let (head, act) = v
            .split_once(':')
            .ok_or_else(|| err(format!("{what}: A=<event>[@<step>][+<ms>]:<action>")))?;
        let (head, delay_ms) = match head.rsplit_once('+') {
            Some((h, ms)) => {
                let ms = dec(ms, &what)?;
                if ms == 0 {
                    return Err(err(format!(
                        "{what}: write no +0 (the delay is 0 without it)"
                    )));
                }
                (h, ms)
            }
            None => (head, 0),
        };
        let (event, at) = match head.split_once('@') {
            Some((e, a)) => {
                let a = dec(a, &what)?;
                if a == 0 {
                    return Err(err(format!(
                        "{what}: write no @0 (the reaction is armed at step 0 without it)"
                    )));
                }
                (e, a)
            }
            None => (head, 0),
        };
        if event.is_empty()
            || !event
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
        {
            return Err(err(format!(
                "{what}: {event:?} is not an assertion id ([A-Za-z0-9_.-])"
            )));
        }
        Ok(React {
            event: event.to_string(),
            at,
            delay_ms,
            action: ReactAction::parse(act, &what)?,
        })
    }
}

impl fmt::Display for React {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.event)?;
        if self.at > 0 {
            write!(f, "@{}", self.at)?;
        }
        if self.delay_ms > 0 {
            write!(f, "+{}", self.delay_ms)?;
        }
        write!(f, ":{}", self.action)
    }
}

/// One guest token (after `run`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GuestToken {
    /// `K=<target>@<at>+<down>`: SIGKILL before step at, restart before step at+down. The
    /// target is a service, a list or a group selector (`target.rs`).
    Kill { target: Target, at: u64, down: u64 },
    /// `P=`: a pause.
    Pause(Pause),
    /// `N=<target>@<from>-<to>:<netem>`: netem on a service's port or a link for steps [from, to).
    Net {
        target: NetTarget,
        from: u64,
        to: u64,
        netem: Vec<Netem>,
    },
    /// `O=<target>@<from>-<to>:<req|resp>:<netem>`: one direction only.
    OneWay {
        target: NetTarget,
        from: u64,
        to: u64,
        dir: Direction,
        netem: Vec<Netem>,
    },
    /// `C=<at>`: a power cut before step at (the recovery boot follows).
    Cut(u64),
    /// `T=<at>:<delta>`: the realtime clock stepped by delta seconds (signed) before step at.
    Clock { at: u64, delta: i64 },
    /// `Q=<n>[d]`: n concurrent requests; `d` = every request sent twice (the stack's client).
    Concurrency { n: u64, double: bool },
    /// `X=<target>@<at>~<off>`: a timer armed before step at kills the target after off ms.
    TimedKill {
        target: Target,
        at: u64,
        off_ms: u64,
    },
    /// `B=`: the BUGGIFY configuration.
    Buggify(Buggify),
    /// `H=`: a protocol fault.
    Protocol(ProtocolFault),
    /// `F=`: freeze a service at the n-th packet of a conversation.
    Freeze(Freeze),
    /// `W=`: the test composer's composition.
    Compose(Compose),
    /// `A=`: a reaction to an assertion record (M139).
    React(React),
    /// `G=<svc>@<at>[+<down>]:<corruption>`: change bytes of one file in the service's storage
    /// directory before step at; with `+down` while the service is down (killed before, started
    /// again before step at+down). See `storage.rs`.
    Corrupt {
        svc: String,
        at: u64,
        down: Option<u64>,
        how: Corruption,
    },
    /// `L=<svc>@<at>+<down>`: a power loss of one service (its own file system shut down,
    /// unwritten data lost) before step at; it starts again before step at+down.
    PowerLoss { svc: String, at: u64, down: u64 },
    /// Any other single upper-case letter: a manifest knob (`M=`, `R=`, `S=`, ...), whose value
    /// the guest passes to the knob's `set:` command.
    Knob { letter: char, value: String },
}

/// The letters with a fixed meaning (manifest_space.RESERVED_TOKENS plus the stack's M/R knobs,
/// which stay knobs).
pub const RESERVED: &str = "KPNOCTQXBFHWAGL";

fn at_plus(v: &str, what: &str) -> Result<(String, u64, u64), ParseError> {
    let (svc, r) = v
        .split_once('@')
        .ok_or_else(|| err(format!("{what}: <svc>@<at>+<n>")))?;
    name(svc, what)?;
    let (a, n) = r
        .split_once('+')
        .ok_or_else(|| err(format!("{what}: <svc>@<at>+<n>")))?;
    Ok((svc.to_string(), dec(a, what)?, dec(n, what)?))
}

/// `<target>@<at>+<n>` (K=, P=).
fn target_at_plus(v: &str, what: &str) -> Result<(Target, u64, u64), ParseError> {
    let (t, r) = v
        .split_once('@')
        .ok_or_else(|| err(format!("{what}: <svc>@<at>+<n>")))?;
    let target = Target::parse(t, what)?;
    let (a, n) = r
        .split_once('+')
        .ok_or_else(|| err(format!("{what}: <svc>@<at>+<n>")))?;
    Ok((target, dec(a, what)?, dec(n, what)?))
}

fn window(r: &str, what: &str) -> Result<(u64, u64), ParseError> {
    let (a, b) = r
        .split_once('-')
        .ok_or_else(|| err(format!("{what}: the window is <from>-<to>")))?;
    Ok((dec(a, what)?, dec(b, what)?))
}

impl GuestToken {
    /// The token's letter.
    pub fn letter(&self) -> char {
        match self {
            GuestToken::Kill { .. } => 'K',
            GuestToken::Pause(_) => 'P',
            GuestToken::Net { .. } => 'N',
            GuestToken::OneWay { .. } => 'O',
            GuestToken::Cut(_) => 'C',
            GuestToken::Clock { .. } => 'T',
            GuestToken::Concurrency { .. } => 'Q',
            GuestToken::TimedKill { .. } => 'X',
            GuestToken::Buggify(_) => 'B',
            GuestToken::Protocol(_) => 'H',
            GuestToken::Freeze(_) => 'F',
            GuestToken::Compose(_) => 'W',
            GuestToken::React(_) => 'A',
            GuestToken::Corrupt { .. } => 'G',
            GuestToken::PowerLoss { .. } => 'L',
            GuestToken::Knob { letter, .. } => *letter,
        }
    }

    /// One guest word (`L=value`).
    pub fn parse(word: &str) -> Result<GuestToken, ParseError> {
        let (k, v) = word
            .split_once('=')
            .ok_or_else(|| err(format!("guest token {word:?}: expected <LETTER>=<value>")))?;
        let mut kc = k.chars();
        let letter = match (kc.next(), kc.next()) {
            (Some(c), None) if c.is_ascii_uppercase() => c,
            _ => {
                // the init reads only single upper-case letters (hydo-init.sh `?=*`) and ignores
                // every other word; refused here
                return Err(err(format!(
                    "guest token {word:?}: the key is one upper-case letter"
                )));
            }
        };
        let what = word;
        Ok(match letter {
            'K' => {
                let (target, at, down) = target_at_plus(v, what)?;
                GuestToken::Kill { target, at, down }
            }
            'P' => {
                if v.contains('~') {
                    let (t, r) = v
                        .split_once('@')
                        .ok_or_else(|| err(format!("{what}: <svc>@<at>~<off>:<dur>")))?;
                    let target = Target::parse(t, what)?;
                    let (a, r) = r
                        .split_once('~')
                        .ok_or_else(|| err(format!("{what}: <svc>@<at>~<off>:<dur>")))?;
                    let (o, d) = r
                        .split_once(':')
                        .ok_or_else(|| err(format!("{what}: <svc>@<at>~<off>:<dur>")))?;
                    GuestToken::Pause(Pause::Timer {
                        target,
                        at: dec(a, what)?,
                        off_ms: dec(o, what)?,
                        dur_ms: dec(d, what)?,
                    })
                } else {
                    let (target, at, steps) = target_at_plus(v, what)?;
                    GuestToken::Pause(Pause::Steps { target, at, steps })
                }
            }
            'N' => {
                let (t, r) = v
                    .split_once('@')
                    .ok_or_else(|| err(format!("{what}: <target>@<from>-<to>:<netem>")))?;
                let (rng, args) = r
                    .split_once(':')
                    .ok_or_else(|| err(format!("{what}: <target>@<from>-<to>:<netem>")))?;
                let (from, to) = window(rng, what)?;
                GuestToken::Net {
                    target: NetTarget::parse(t, what)?,
                    from,
                    to,
                    netem: Netem::parse_list(args, what)?,
                }
            }
            'O' => {
                let (t, r) = v.split_once('@').ok_or_else(|| {
                    err(format!("{what}: <target>@<from>-<to>:<req|resp>:<netem>"))
                })?;
                let mut it = r.splitn(3, ':');
                let (rng, d, args) = match (it.next(), it.next(), it.next()) {
                    (Some(a), Some(b), Some(c)) => (a, b, c),
                    _ => {
                        return Err(err(format!(
                            "{what}: <target>@<from>-<to>:<req|resp>:<netem>"
                        )))
                    }
                };
                let (from, to) = window(rng, what)?;
                let dir = match d {
                    "req" => Direction::Req,
                    "resp" => Direction::Resp,
                    // hydo-init.sh: die_oracle "bad one-way direction"
                    _ => return Err(err(format!("{what}: the direction is req or resp"))),
                };
                let target = NetTarget::parse(t, what)?;
                if target.is_partition() {
                    return Err(err(format!(
                        "{what}: a partition is cut both ways; write it as N= (a ring shape \
                         cuts one way already)"
                    )));
                }
                GuestToken::OneWay {
                    target,
                    from,
                    to,
                    dir,
                    netem: Netem::parse_list(args, what)?,
                }
            }
            'C' => GuestToken::Cut(dec(v, what)?),
            'T' => {
                let (a, d) = v
                    .split_once(':')
                    .ok_or_else(|| err(format!("{what}: T=<at>:<delta seconds>")))?;
                GuestToken::Clock {
                    at: dec(a, what)?,
                    delta: sdec(d, what)?,
                }
            }
            'Q' => {
                let (n, double) = match v.strip_suffix('d') {
                    Some(n) => (n, true),
                    None => (v, false),
                };
                GuestToken::Concurrency {
                    n: dec(n, what)?,
                    double,
                }
            }
            'X' => {
                let (t, r) = v
                    .split_once('@')
                    .ok_or_else(|| err(format!("{what}: <svc>@<at>~<ms>")))?;
                let target = Target::parse(t, what)?;
                let (a, o) = r
                    .split_once('~')
                    .ok_or_else(|| err(format!("{what}: <svc>@<at>~<ms>")))?;
                GuestToken::TimedKill {
                    target,
                    at: dec(a, what)?,
                    off_ms: dec(o, what)?,
                }
            }
            'B' => {
                let f: Vec<&str> = v.split(',').collect();
                if f.len() > 3 {
                    return Err(err(format!(
                        "{what}: B=<seed hex>[,<enable permille>[,<fire permille>]]"
                    )));
                }
                GuestToken::Buggify(Buggify {
                    seed: hex(f[0], what)?,
                    enable_permille: f.get(1).map(|x| dec(x, what)).transpose()?,
                    fire_permille: f.get(2).map(|x| dec(x, what)).transpose()?,
                })
            }
            'H' => GuestToken::Protocol(ProtocolFault::parse(v)?),
            'F' => GuestToken::Freeze(Freeze::parse(v)?),
            'W' => GuestToken::Compose(Compose::parse(v)?),
            'A' => GuestToken::React(React::parse(v)?),
            'L' => {
                let (svc, at, down) = at_plus(v, what)?;
                GuestToken::PowerLoss { svc, at, down }
            }
            'G' => {
                let form =
                    "<svc>@<at>[+<down>]:<flip|zero|trunc>[,n=][,len=][,range=][,file=][,seed=]";
                let (svc, r) = v
                    .split_once('@')
                    .ok_or_else(|| err(format!("{what}: {form}")))?;
                name(svc, what)?;
                let (when, how) = r
                    .split_once(':')
                    .ok_or_else(|| err(format!("{what}: {form}")))?;
                let (at, down) = match when.split_once('+') {
                    Some((a, d)) => (dec(a, what)?, Some(dec(d, what)?)),
                    None => (dec(when, what)?, None),
                };
                GuestToken::Corrupt {
                    svc: svc.to_string(),
                    at,
                    down,
                    how: Corruption::parse(how, what)?,
                }
            }
            _ => {
                if v.is_empty() {
                    return Err(err(format!("{what}: a knob needs a value")));
                }
                GuestToken::Knob {
                    letter,
                    value: v.to_string(),
                }
            }
        })
    }
}

impl fmt::Display for GuestToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuestToken::Kill { target, at, down } => write!(f, "K={target}@{at}+{down}"),
            GuestToken::Pause(Pause::Steps { target, at, steps }) => {
                write!(f, "P={target}@{at}+{steps}")
            }
            GuestToken::Pause(Pause::Timer {
                target,
                at,
                off_ms,
                dur_ms,
            }) => write!(f, "P={target}@{at}~{off_ms}:{dur_ms}"),
            GuestToken::Net {
                target,
                from,
                to,
                netem,
            } => {
                write!(f, "N={target}@{from}-{to}:")?;
                write_netem(f, netem)
            }
            GuestToken::OneWay {
                target,
                from,
                to,
                dir,
                netem,
            } => {
                let d = match dir {
                    Direction::Req => "req",
                    Direction::Resp => "resp",
                };
                write!(f, "O={target}@{from}-{to}:{d}:")?;
                write_netem(f, netem)
            }
            GuestToken::Cut(c) => write!(f, "C={c}"),
            GuestToken::Clock { at, delta } => write!(f, "T={at}:{delta}"),
            GuestToken::Concurrency { n, double } => {
                write!(f, "Q={n}{}", if *double { "d" } else { "" })
            }
            GuestToken::TimedKill { target, at, off_ms } => {
                write!(f, "X={target}@{at}~{off_ms}")
            }
            GuestToken::Buggify(b) => {
                write!(f, "B={:x}", b.seed)?;
                if let Some(e) = b.enable_permille {
                    write!(f, ",{e}")?;
                }
                if let Some(x) = b.fire_permille {
                    write!(f, ",{x}")?;
                }
                Ok(())
            }
            GuestToken::Protocol(h) => write!(f, "H={h}"),
            GuestToken::Freeze(x) => write!(f, "F={x}"),
            GuestToken::Compose(w) => write!(f, "W={w}"),
            GuestToken::React(r) => write!(f, "A={r}"),
            GuestToken::Corrupt { svc, at, down, how } => {
                write!(f, "G={svc}@{at}")?;
                if let Some(d) = down {
                    write!(f, "+{d}")?;
                }
                write!(f, ":{how}")
            }
            GuestToken::PowerLoss { svc, at, down } => write!(f, "L={svc}@{at}+{down}"),
            GuestToken::Knob { letter, value } => write!(f, "{letter}={value}"),
        }
    }
}
