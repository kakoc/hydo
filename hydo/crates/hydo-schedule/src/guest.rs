//! The guest part of a schedule: `run` and the tokens the guest's init applies (hydo-init.sh
//! explore_run and its extensions, the stack's stack-explore.sh).
//!
//! The engines type the guest part on the guest's serial console and the init logs it, so the
//! console -- and the run's fingerprint -- contains the text exactly as written. A guest token
//! is therefore never rewritten: a token that is not in the canonical form explore.py prints
//! (leading zeros, upper-case hex, an empty field) is refused with a message, and every accepted
//! token prints back byte for byte.

use crate::compose::Compose;
use crate::num::{dec, err, hex, name, sdec};
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
        if t.len() % 2 != 0 || t.is_empty() {
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
/// (`a-b`, or `a-*` = a against every other; manifest_space.py, hydo-init-links.sh).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NetTarget {
    Service(String),
    Link(String, String),
    LinkAll(String),
}

impl NetTarget {
    fn parse(s: &str, what: &str) -> Result<NetTarget, ParseError> {
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
}

impl fmt::Display for NetTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetTarget::Service(s) => f.write_str(s),
            NetTarget::Link(a, b) => write!(f, "{a}-{b}"),
            NetTarget::LinkAll(a) => write!(f, "{a}-*"),
        }
    }
}

/// `P=`: a pause (SIGSTOP .. SIGCONT).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Pause {
    /// `P=<svc>@<at>+<n>`: stopped before step at, continued before step at+n.
    Steps { svc: String, at: u64, steps: u64 },
    /// `P=<svc>@<at>~<off>:<dur>`: a timer armed before step at stops it after off ms, for dur ms.
    Timer {
        svc: String,
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

/// One rule of an `H=` token (hydo-world rules.go parseRule and grpc.go parseRuleExt).
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

/// One guest token (after `run`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GuestToken {
    /// `K=<svc>@<at>+<down>`: SIGKILL before step at, restart before step at+down.
    Kill { svc: String, at: u64, down: u64 },
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
    /// `X=<svc>@<at>~<off>`: a timer armed before step at kills svc after off ms.
    TimedKill { svc: String, at: u64, off_ms: u64 },
    /// `B=`: the BUGGIFY configuration.
    Buggify(Buggify),
    /// `H=`: a protocol fault.
    Protocol(ProtocolFault),
    /// `F=`: freeze a service at the n-th packet of a conversation.
    Freeze(Freeze),
    /// `W=`: the test composer's composition.
    Compose(Compose),
    /// Any other single upper-case letter: a manifest knob (`M=`, `R=`, `S=`, ...), whose value
    /// the guest passes to the knob's `set:` command.
    Knob { letter: char, value: String },
}

/// The letters with a fixed meaning (manifest_space.RESERVED_TOKENS plus the stack's M/R knobs,
/// which stay knobs).
pub const RESERVED: &str = "KPNOCTQXBFHW";

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
                let (svc, at, down) = at_plus(v, what)?;
                GuestToken::Kill { svc, at, down }
            }
            'P' => {
                if v.contains('~') {
                    let (svc, r) = v
                        .split_once('@')
                        .ok_or_else(|| err(format!("{what}: <svc>@<at>~<off>:<dur>")))?;
                    name(svc, what)?;
                    let (a, r) = r
                        .split_once('~')
                        .ok_or_else(|| err(format!("{what}: <svc>@<at>~<off>:<dur>")))?;
                    let (o, d) = r
                        .split_once(':')
                        .ok_or_else(|| err(format!("{what}: <svc>@<at>~<off>:<dur>")))?;
                    GuestToken::Pause(Pause::Timer {
                        svc: svc.to_string(),
                        at: dec(a, what)?,
                        off_ms: dec(o, what)?,
                        dur_ms: dec(d, what)?,
                    })
                } else {
                    let (svc, at, steps) = at_plus(v, what)?;
                    GuestToken::Pause(Pause::Steps { svc, at, steps })
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
                GuestToken::OneWay {
                    target: NetTarget::parse(t, what)?,
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
                let (svc, r) = v
                    .split_once('@')
                    .ok_or_else(|| err(format!("{what}: <svc>@<at>~<ms>")))?;
                name(svc, what)?;
                let (a, o) = r
                    .split_once('~')
                    .ok_or_else(|| err(format!("{what}: <svc>@<at>~<ms>")))?;
                GuestToken::TimedKill {
                    svc: svc.to_string(),
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
            GuestToken::Kill { svc, at, down } => write!(f, "K={svc}@{at}+{down}"),
            GuestToken::Pause(Pause::Steps { svc, at, steps }) => write!(f, "P={svc}@{at}+{steps}"),
            GuestToken::Pause(Pause::Timer {
                svc,
                at,
                off_ms,
                dur_ms,
            }) => write!(f, "P={svc}@{at}~{off_ms}:{dur_ms}"),
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
            GuestToken::TimedKill { svc, at, off_ms } => write!(f, "X={svc}@{at}~{off_ms}"),
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
            GuestToken::Knob { letter, value } => write!(f, "{letter}={value}"),
        }
    }
}
