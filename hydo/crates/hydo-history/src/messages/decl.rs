//! The declaration of a `type: messages` check (docs/design/messaging.md "The declaration"):
//! what the project says its system promises. Every default claims nothing. The HYDO-CHECK line
//! carries the declaration with only the keys the project gave (includes already merged by
//! `hydo build`); this module reads it, refuses keys it does not know (so a newer console's
//! claim is an ERROR, never a pass) and names contradictory combinations.

use crate::py::PyVal;

/// How consumers acknowledge deliveries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ack {
    Explicit,
    OnReceive,
    None,
}

/// Whether time-dependent claims judge pairs whose interval contains a `T=` clock step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockSteps {
    Undecided,
    Strict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    None,
    AtMostOnce,
    AtLeastOnce,
    ExactlyOnce,
}

/// What a deduplication window compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedupBy {
    DedupId,
    Message,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplicates {
    Allowed,
    Forbidden,
    /// `window_ms: 0` means "always".
    Window {
        by: DedupBy,
        window_ms: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lease {
    None,
    Ms(i64),
    PerDelivery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderKind {
    None,
    Fifo,
    FifoBlocking,
    Log,
    Causal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Key,
    Channel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order {
    pub kind: OrderKind,
    pub scope: Scope,
    /// `batch: ordered`: the items of one send are in order.
    pub batch_ordered: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeadLetter {
    pub after: i64,
    pub to: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delay {
    None,
    PerMessage,
    Ms(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Linearizable {
    None,
    Fifo,
    Unordered,
}

/// What the measures cover.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasureOpts {
    pub windows: bool,
    pub quantiles: Vec<f64>,
    /// None = all channels.
    pub channels: Option<Vec<String>>,
}

impl Default for MeasureOpts {
    fn default() -> Self {
        MeasureOpts {
            windows: true,
            quantiles: vec![0.5, 0.95, 0.99, 1.0],
            channels: None,
        }
    }
}

/// A resolved declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Decl {
    pub ack: Ack,
    pub retention_ms: Option<i64>,
    pub clock_steps: ClockSteps,
    pub delivery: Delivery,
    pub duplicates: Duplicates,
    /// `after_ack: no-redelivery`.
    pub after_ack: bool,
    /// `processing: once`.
    pub processing_once: bool,
    pub lease: Lease,
    pub order: Order,
    pub dead_letter: Option<DeadLetter>,
    pub delay: Delay,
    /// `fan_out: every-group`.
    pub fan_out: bool,
    /// `members: exclusive`.
    pub members_exclusive: bool,
    pub linearizable: Linearizable,
    pub measures: MeasureOpts,
}

impl Default for Decl {
    fn default() -> Self {
        Decl {
            ack: Ack::Explicit,
            retention_ms: None,
            clock_steps: ClockSteps::Undecided,
            delivery: Delivery::None,
            duplicates: Duplicates::Allowed,
            after_ack: false,
            processing_once: false,
            lease: Lease::None,
            order: Order {
                kind: OrderKind::None,
                scope: Scope::Key,
                batch_ordered: false,
            },
            dead_letter: None,
            delay: Delay::None,
            fan_out: false,
            members_exclusive: false,
            linearizable: Linearizable::None,
            measures: MeasureOpts::default(),
        }
    }
}

/// The check type.
pub const TYPE: &str = "messages";

/// The declaration's own keys (beside a check's `id`, `type`, `history`, `msg`, `final`,
/// `include` and `description`).
pub const DECL_KEYS: [&str; 5] = [
    "acknowledgement",
    "retention_ms",
    "clock_steps",
    "claims",
    "measures",
];
/// The claim keys this hydo knows.
pub const CLAIM_KEYS: [&str; 11] = [
    "delivery",
    "duplicates",
    "after_ack",
    "processing",
    "lease",
    "order",
    "dead_letter",
    "delay",
    "fan_out",
    "members",
    "linearizable",
];
/// The measure option keys this hydo knows.
pub const MEASURE_KEYS: [&str; 3] = ["windows", "quantiles", "channels"];
/// The keys a HYDO-CHECK spec of a messages check may carry.
const SPEC_KEYS: [&str; 5] = ["id", "type", "history", "msg", "_final_rc"];

fn int_of(v: &PyVal, wh: &str) -> Result<i64, String> {
    match v {
        PyVal::Int(i) => {
            let n = i.saturating_i64();
            if n < 0 {
                Err(format!("{wh}: a number of milliseconds, 0 or more"))
            } else {
                Ok(n)
            }
        }
        _ => Err(format!("{wh}: a number, not {}", v.repr())),
    }
}

fn word<'a>(v: &'a PyVal, wh: &str, allowed: &[&str]) -> Result<&'a str, String> {
    match v.as_str() {
        Some(s) if allowed.contains(&s) => Ok(s),
        _ => Err(format!(
            "{wh}: one of {}, not {}",
            allowed.join(", "),
            v.repr()
        )),
    }
}

fn keys_of<'a>(v: &'a PyVal, wh: &str, known: &[&str]) -> Result<&'a [(String, PyVal)], String> {
    let PyVal::Dict(m) = v else {
        return Err(format!("{wh}: a mapping, not {}", v.repr()));
    };
    for (k, _) in m {
        if !known.contains(&k.as_str()) {
            return Err(format!(
                "{wh}: the key {k} is not known to this hydo (known: {})",
                known.join(", ")
            ));
        }
    }
    Ok(m)
}

/// Reads a HYDO-CHECK spec of `type: messages` (or any mapping with the declaration's keys
/// beside the spec's own). Err names the first key or value this hydo cannot read.
pub fn from_spec(spec: &PyVal) -> Result<Decl, String> {
    let mut known: Vec<&str> = SPEC_KEYS.to_vec();
    known.extend(DECL_KEYS);
    let m = keys_of(spec, "the declaration", &known)?;
    let mut d = Decl::default();
    for (k, v) in m {
        match k.as_str() {
            "acknowledgement" => {
                d.ack = match word(v, k, &["explicit", "on-receive", "none"])? {
                    "explicit" => Ack::Explicit,
                    "on-receive" => Ack::OnReceive,
                    _ => Ack::None,
                }
            }
            "retention_ms" => d.retention_ms = Some(int_of(v, k)?),
            "clock_steps" => {
                d.clock_steps = match word(v, k, &["undecided", "strict"])? {
                    "strict" => ClockSteps::Strict,
                    _ => ClockSteps::Undecided,
                }
            }
            "claims" => claims(&mut d, v)?,
            "measures" => measures(&mut d.measures, v)?,
            _ => {}
        }
    }
    Ok(d)
}

fn claims(d: &mut Decl, v: &PyVal) -> Result<(), String> {
    for (k, v) in keys_of(v, "claims", &CLAIM_KEYS)? {
        let wh = format!("claims.{k}");
        match k.as_str() {
            "delivery" => {
                d.delivery = match word(
                    v,
                    &wh,
                    &["none", "at-most-once", "at-least-once", "exactly-once"],
                )? {
                    "at-most-once" => Delivery::AtMostOnce,
                    "at-least-once" => Delivery::AtLeastOnce,
                    "exactly-once" => Delivery::ExactlyOnce,
                    _ => Delivery::None,
                }
            }
            "duplicates" => {
                d.duplicates = match v {
                    PyVal::Str(_) => match word(v, &wh, &["allowed", "forbidden"])? {
                        "forbidden" => Duplicates::Forbidden,
                        _ => Duplicates::Allowed,
                    },
                    _ => {
                        let m = keys_of(v, &wh, &["by", "window_ms"])?;
                        let by = m
                            .iter()
                            .find(|(k, _)| k == "by")
                            .ok_or(format!("{wh}: by: is required (dedup-id or message)"))?;
                        let by = match word(&by.1, &format!("{wh}.by"), &["dedup-id", "message"])? {
                            "dedup-id" => DedupBy::DedupId,
                            _ => DedupBy::Message,
                        };
                        let w = m
                            .iter()
                            .find(|(k, _)| k == "window_ms")
                            .ok_or(format!("{wh}: window_ms: is required (0 means always)"))?;
                        Duplicates::Window {
                            by,
                            window_ms: int_of(&w.1, &format!("{wh}.window_ms"))?,
                        }
                    }
                }
            }
            "after_ack" => {
                d.after_ack = word(v, &wh, &["allowed", "no-redelivery"])? == "no-redelivery"
            }
            "processing" => d.processing_once = word(v, &wh, &["any", "once"])? == "once",
            "lease" => {
                d.lease = match v {
                    PyVal::Str(_) => {
                        word(v, &wh, &["none"])?;
                        Lease::None
                    }
                    _ => {
                        let m = keys_of(v, &wh, &["ms"])?;
                        let ms = m.first().ok_or(format!("{wh}: ms: is required"))?.1.clone();
                        if ms.eq_str("per-delivery") {
                            Lease::PerDelivery
                        } else {
                            let n = int_of(&ms, &format!("{wh}.ms"))?;
                            if n == 0 {
                                return Err(format!("{wh}.ms: more than 0"));
                            }
                            Lease::Ms(n)
                        }
                    }
                }
            }
            "order" => {
                const KINDS: [&str; 5] = ["none", "fifo", "fifo-blocking", "log", "causal"];
                let kind_of = |s: &str| match s {
                    "fifo" => OrderKind::Fifo,
                    "fifo-blocking" => OrderKind::FifoBlocking,
                    "log" => OrderKind::Log,
                    "causal" => OrderKind::Causal,
                    _ => OrderKind::None,
                };
                d.order = match v {
                    PyVal::Str(_) => Order {
                        kind: kind_of(word(v, &wh, &KINDS)?),
                        scope: Scope::Key,
                        batch_ordered: false,
                    },
                    _ => {
                        let m = keys_of(v, &wh, &["kind", "scope", "batch"])?;
                        let mut o = Order {
                            kind: OrderKind::None,
                            scope: Scope::Key,
                            batch_ordered: false,
                        };
                        let mut has_kind = false;
                        for (k2, v2) in m {
                            let w2 = format!("{wh}.{k2}");
                            match k2.as_str() {
                                "kind" => {
                                    has_kind = true;
                                    o.kind = kind_of(word(v2, &w2, &KINDS)?)
                                }
                                "scope" => {
                                    o.scope = match word(v2, &w2, &["key", "channel"])? {
                                        "channel" => Scope::Channel,
                                        _ => Scope::Key,
                                    }
                                }
                                _ => {
                                    o.batch_ordered =
                                        word(v2, &w2, &["ordered", "unordered"])? == "ordered"
                                }
                            }
                        }
                        if !has_kind {
                            return Err(format!("{wh}: kind: is required"));
                        }
                        o
                    }
                }
            }
            "dead_letter" => {
                d.dead_letter = match v {
                    PyVal::Str(_) => {
                        word(v, &wh, &["none"])?;
                        None
                    }
                    _ => {
                        let m = keys_of(v, &wh, &["after", "to"])?;
                        let after = m
                            .iter()
                            .find(|(k, _)| k == "after")
                            .ok_or(format!("{wh}: after: is required"))?;
                        let after = int_of(&after.1, &format!("{wh}.after"))?;
                        if after < 1 {
                            return Err(format!("{wh}.after: 1 or more"));
                        }
                        let to = match m.iter().find(|(k, _)| k == "to") {
                            None => None,
                            Some((_, PyVal::Str(s))) if !s.is_empty() => Some(s.clone()),
                            Some((_, x)) => {
                                return Err(format!("{wh}.to: a channel name, not {}", x.repr()))
                            }
                        };
                        Some(DeadLetter { after, to })
                    }
                }
            }
            "delay" => {
                d.delay = match v {
                    PyVal::Str(_) => match word(v, &wh, &["none", "per-message"])? {
                        "per-message" => Delay::PerMessage,
                        _ => Delay::None,
                    },
                    _ => {
                        let m = keys_of(v, &wh, &["ms"])?;
                        let ms = &m.first().ok_or(format!("{wh}: ms: is required"))?.1;
                        Delay::Ms(int_of(ms, &format!("{wh}.ms"))?)
                    }
                }
            }
            "fan_out" => d.fan_out = word(v, &wh, &["none", "every-group"])? == "every-group",
            "members" => d.members_exclusive = word(v, &wh, &["any", "exclusive"])? == "exclusive",
            "linearizable" => {
                d.linearizable = match word(v, &wh, &["none", "fifo", "unordered"])? {
                    "fifo" => Linearizable::Fifo,
                    "unordered" => Linearizable::Unordered,
                    _ => Linearizable::None,
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn measures(o: &mut MeasureOpts, v: &PyVal) -> Result<(), String> {
    for (k, v) in keys_of(v, "measures", &MEASURE_KEYS)? {
        let wh = format!("measures.{k}");
        match k.as_str() {
            "windows" => match v {
                PyVal::Bool(b) => o.windows = *b,
                _ => return Err(format!("{wh}: true or false")),
            },
            "quantiles" => {
                let PyVal::List(a) = v else {
                    return Err(format!("{wh}: a list of numbers from 0 to 1"));
                };
                let mut q = Vec::new();
                for x in a {
                    let f = match x {
                        PyVal::Int(i) => i.to_f64().unwrap_or(f64::NAN),
                        PyVal::Float(f) => *f,
                        PyVal::Str(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
                        _ => f64::NAN,
                    };
                    if !(0.0..=1.0).contains(&f) {
                        return Err(format!("{wh}: a list of numbers from 0 to 1"));
                    }
                    q.push(f);
                }
                o.quantiles = q;
            }
            _ => match v {
                PyVal::Str(s) if s == "all" => o.channels = None,
                PyVal::List(a) => {
                    let mut c = Vec::new();
                    for x in a {
                        match x.as_str() {
                            Some(s) => c.push(s.to_string()),
                            None => return Err(format!("{wh}: all, or a list of channel names")),
                        }
                    }
                    o.channels = Some(c);
                }
                _ => return Err(format!("{wh}: all, or a list of channel names")),
            },
        }
    }
    Ok(())
}

/// The claims a declaration makes, in the order they are reported (`no-phantom` always first).
pub fn claim_names(d: &Decl) -> Vec<&'static str> {
    let mut v = vec!["no-phantom"];
    if matches!(d.delivery, Delivery::AtLeastOnce | Delivery::ExactlyOnce) {
        v.push("no-loss");
    }
    if matches!(d.delivery, Delivery::AtMostOnce | Delivery::ExactlyOnce) {
        v.push("at-most-once");
    }
    if d.duplicates != Duplicates::Allowed {
        v.push("duplicates");
    }
    if d.after_ack {
        v.push("after-ack");
    }
    if d.processing_once {
        v.push("processing");
    }
    if d.lease != Lease::None {
        v.push("lease");
    }
    if d.order.kind != OrderKind::None {
        v.push("order");
    }
    if d.dead_letter.is_some() {
        v.push("dead-letter");
    }
    if d.delay != Delay::None {
        v.push("delay");
    }
    if d.fan_out {
        v.push("fan-out");
    }
    if d.members_exclusive {
        v.push("members");
    }
    if d.linearizable != Linearizable::None {
        v.push("linearizable");
    }
    v
}

/// Whether a claim reads the end of the history (needs a drain: the check's `final:`).
pub fn needs_drain(d: &Decl) -> Vec<&'static str> {
    let mut v = Vec::new();
    if matches!(d.delivery, Delivery::AtLeastOnce | Delivery::ExactlyOnce) {
        v.push("claims.delivery");
    }
    if d.fan_out {
        v.push("claims.fan_out");
    }
    if d.processing_once {
        v.push("claims.processing");
    }
    if d.dead_letter.as_ref().is_some_and(|x| x.to.is_some()) {
        v.push("claims.dead_letter (with to:)");
    }
    v
}

/// The combinations refused before any run (docs/design/messaging.md "Combinations refused
/// before any run"), each naming both keys. `has_final`: None when the caller cannot know (the
/// checker reads a spec, which never carries `final:`).
pub fn conflicts(d: &Decl, has_final: Option<bool>) -> Vec<String> {
    let mut out = Vec::new();
    if d.linearizable != Linearizable::None && d.ack != Ack::OnReceive {
        out.push(
            "claims.linearizable needs acknowledgement: on-receive (a queue with \
             acknowledgements redelivers, so it is not one linearizable queue)"
                .to_string(),
        );
    }
    if d.ack != Ack::Explicit {
        let ack = if d.ack == Ack::OnReceive {
            "on-receive"
        } else {
            "none"
        };
        let mut about = Vec::new();
        if d.processing_once {
            about.push("claims.processing");
        }
        if d.after_ack {
            about.push("claims.after_ack");
        }
        if d.lease != Lease::None {
            about.push("claims.lease");
        }
        if d.dead_letter.is_some() {
            about.push("claims.dead_letter");
        }
        if d.order.kind == OrderKind::FifoBlocking {
            about.push("claims.order: fifo-blocking");
        }
        for a in about {
            out.push(format!(
                "{a} is about acknowledgements, and acknowledgement: {ack} has none"
            ));
        }
    }
    if matches!(d.delivery, Delivery::AtMostOnce | Delivery::ExactlyOnce) && d.lease != Lease::None
    {
        out.push(
            "claims.delivery: at-most-once (or exactly-once) with claims.lease: a lease exists to \
             allow redelivery, which at-most-once forbids"
                .to_string(),
        );
    }
    if has_final == Some(false) {
        for k in needs_drain(d) {
            out.push(format!(
                "{k} reads the end of the history, and the check has no final: (the drain that \
                 reads every message at the end)"
            ));
        }
    }
    out
}

/// The declaration as the HYDO-CHECK line and the results show it: every key with its value,
/// defaults included (a JSON object).
pub fn to_pyval(d: &Decl) -> PyVal {
    let s = PyVal::str;
    let n = |x: i64| PyVal::Int(crate::py::BigInt::from_i64(x));
    let dict = |v: Vec<(&str, PyVal)>| {
        PyVal::Dict(v.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    };
    let mut claims = Vec::new();
    claims.push((
        "delivery",
        s(match d.delivery {
            Delivery::None => "none",
            Delivery::AtMostOnce => "at-most-once",
            Delivery::AtLeastOnce => "at-least-once",
            Delivery::ExactlyOnce => "exactly-once",
        }),
    ));
    claims.push((
        "duplicates",
        match d.duplicates {
            Duplicates::Allowed => s("allowed"),
            Duplicates::Forbidden => s("forbidden"),
            Duplicates::Window { by, window_ms } => dict(vec![
                (
                    "by",
                    s(if by == DedupBy::DedupId {
                        "dedup-id"
                    } else {
                        "message"
                    }),
                ),
                ("window_ms", n(window_ms)),
            ]),
        },
    ));
    claims.push((
        "after_ack",
        s(if d.after_ack {
            "no-redelivery"
        } else {
            "allowed"
        }),
    ));
    claims.push((
        "processing",
        s(if d.processing_once { "once" } else { "any" }),
    ));
    claims.push((
        "lease",
        match d.lease {
            Lease::None => s("none"),
            Lease::Ms(x) => dict(vec![("ms", n(x))]),
            Lease::PerDelivery => dict(vec![("ms", s("per-delivery"))]),
        },
    ));
    let kind = match d.order.kind {
        OrderKind::None => "none",
        OrderKind::Fifo => "fifo",
        OrderKind::FifoBlocking => "fifo-blocking",
        OrderKind::Log => "log",
        OrderKind::Causal => "causal",
    };
    claims.push((
        "order",
        dict(vec![
            ("kind", s(kind)),
            (
                "scope",
                s(if d.order.scope == Scope::Key {
                    "key"
                } else {
                    "channel"
                }),
            ),
            (
                "batch",
                s(if d.order.batch_ordered {
                    "ordered"
                } else {
                    "unordered"
                }),
            ),
        ]),
    ));
    claims.push((
        "dead_letter",
        match &d.dead_letter {
            None => s("none"),
            Some(x) => {
                let mut v = vec![("after", n(x.after))];
                if let Some(t) = &x.to {
                    v.push(("to", s(t)));
                }
                dict(v)
            }
        },
    ));
    claims.push((
        "delay",
        match d.delay {
            Delay::None => s("none"),
            Delay::PerMessage => s("per-message"),
            Delay::Ms(x) => dict(vec![("ms", n(x))]),
        },
    ));
    claims.push(("fan_out", s(if d.fan_out { "every-group" } else { "none" })));
    claims.push((
        "members",
        s(if d.members_exclusive {
            "exclusive"
        } else {
            "any"
        }),
    ));
    claims.push((
        "linearizable",
        s(match d.linearizable {
            Linearizable::None => "none",
            Linearizable::Fifo => "fifo",
            Linearizable::Unordered => "unordered",
        }),
    ));
    let mut top = vec![
        (
            "acknowledgement",
            s(match d.ack {
                Ack::Explicit => "explicit",
                Ack::OnReceive => "on-receive",
                Ack::None => "none",
            }),
        ),
        (
            "clock_steps",
            s(if d.clock_steps == ClockSteps::Strict {
                "strict"
            } else {
                "undecided"
            }),
        ),
        ("claims", dict(claims)),
        (
            "measures",
            dict(vec![
                ("windows", PyVal::Bool(d.measures.windows)),
                (
                    "quantiles",
                    PyVal::List(d.measures.quantiles.iter().map(|q| quant(*q)).collect()),
                ),
                (
                    "channels",
                    match &d.measures.channels {
                        None => s("all"),
                        Some(c) => PyVal::List(c.iter().map(|x| s(x)).collect()),
                    },
                ),
            ]),
        ),
    ];
    if let Some(r) = d.retention_ms {
        top.push(("retention_ms", n(r)));
    }
    dict(top)
}

/// A quantile as JSON shows it: 1 rather than 1.0.
pub(crate) fn quant(q: f64) -> PyVal {
    if q.fract() == 0.0 {
        PyVal::Int(crate::py::BigInt::from_i64(q as i64))
    } else {
        PyVal::Float(q)
    }
}
