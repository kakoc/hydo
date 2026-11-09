//! The case model: a program of SDK calls (`Prog`), the configurations it runs under (`Run`),
//! and how both are written to the case files that the other SDKs read (`sdk/cases/`).
//!
//! Values carry their Go type (`int` = int64, `uint` = uint64, `f64`, `f32`), because the
//! encoding depends on it. Two kinds of input exist in Go but not in Rust: a byte string
//! (`[]byte`, written as base64) and a string with invalid UTF-8. The Rust runner passes a
//! byte string as its base64 text, and each invalid byte of a string as a marker character
//! (U+10FF00 + byte), which `main.rs` turns back into what Go 1.26 writes for it (`�` in
//! JSON, the raw byte in a HYDO-HIST record).

use std::fmt::Write as _;

/// A piece of a string: text, or one byte (0x80..=0xff) that is not valid UTF-8 there.
#[derive(Clone, Debug)]
pub enum P {
    T(String),
    B(u8),
}

/// A string: plain text, or text with invalid UTF-8 bytes.
#[derive(Clone, Debug)]
pub enum S {
    T(String),
    R(Vec<P>),
}

impl From<&str> for S {
    fn from(s: &str) -> S {
        S::T(s.to_string())
    }
}
impl From<String> for S {
    fn from(s: String) -> S {
        S::T(s)
    }
}

/// A string written with `\xHH` escapes for invalid bytes: `x(r"a\xffb")`.
pub fn x(s: &str) -> S {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("\\x") {
        text.push_str(&rest[..i]);
        let b = u8::from_str_radix(&rest[i + 2..i + 4], 16).expect("\\xHH");
        assert!(b >= 0x80, "x(): \\x{:02x} is ASCII; write it as text", b);
        if !text.is_empty() {
            pieces.push(P::T(std::mem::take(&mut text)));
        }
        pieces.push(P::B(b));
        rest = &rest[i + 4..];
    }
    text.push_str(rest);
    if pieces.is_empty() {
        return S::T(text);
    }
    if !text.is_empty() {
        pieces.push(P::T(text));
    }
    let s = S::R(pieces);
    validate_raw(&s);
    s
}

const MARK: u32 = 0x10FF00;

impl S {
    /// The bytes Go sees.
    pub fn go_bytes(&self) -> Vec<u8> {
        match self {
            S::T(t) => t.as_bytes().to_vec(),
            S::R(ps) => {
                let mut v = Vec::new();
                for p in ps {
                    match p {
                        P::T(t) => v.extend_from_slice(t.as_bytes()),
                        P::B(b) => v.push(*b),
                    }
                }
                v
            }
        }
    }
    /// The string the Rust runner passes: invalid bytes as marker characters.
    pub fn rust(&self) -> String {
        match self {
            S::T(t) => t.clone(),
            S::R(ps) => {
                let mut s = String::new();
                for p in ps {
                    match p {
                        P::T(t) => s.push_str(t),
                        P::B(b) => s.push(char::from_u32(MARK + *b as u32).unwrap()),
                    }
                }
                s
            }
        }
    }
    /// The UTF-8 bytes of the string the JavaScript SDK sorts by (a lone surrogate is U+FFFD).
    fn js_bytes(&self) -> Vec<u8> {
        match self {
            S::T(t) => t.as_bytes().to_vec(),
            S::R(ps) => {
                let mut v = Vec::new();
                for p in ps {
                    match p {
                        P::T(t) => v.extend_from_slice(t.as_bytes()),
                        P::B(_) => v.extend_from_slice("\u{fffd}".as_bytes()),
                    }
                }
                v
            }
        }
    }
    pub fn is_raw(&self) -> bool {
        matches!(self, S::R(_))
    }
    pub fn json(&self) -> String {
        match self {
            S::T(t) => jstr(t),
            S::R(ps) => {
                let parts: Vec<String> = ps
                    .iter()
                    .map(|p| match p {
                        P::T(t) => jstr(t),
                        P::B(b) => b.to_string(),
                    })
                    .collect();
                format!("{{\"raw\":[{}]}}", parts.join(","))
            }
        }
    }
}

/// Go's decoding: at each position one rune, or (invalid) one byte. The marked bytes must be
/// exactly the invalid ones, so that every SDK writes one `�` (or one raw byte) per mark.
fn validate_raw(s: &S) {
    let S::R(ps) = s else { return };
    let mut marked = Vec::new();
    let mut bytes = Vec::new();
    for p in ps {
        match p {
            P::T(t) => {
                for b in t.bytes() {
                    bytes.push(b);
                    marked.push(false);
                }
            }
            P::B(b) => {
                bytes.push(*b);
                marked.push(true);
            }
        }
    }
    let mut i = 0;
    while i < bytes.len() {
        let n = match bytes[i] {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 0,
        };
        let valid = n > 0 && i + n <= bytes.len() && std::str::from_utf8(&bytes[i..i + n]).is_ok();
        if valid {
            assert!(!marked[i..i + n].iter().any(|m| *m), "x(): a marked byte forms valid UTF-8 in {:?}", s);
            i += n;
        } else {
            assert!(marked[i], "x(): an unmarked invalid byte in {:?}", s);
            i += 1;
        }
    }
}

/// A JSON string as Go writes it (valid JSON for every reader).
pub fn jstr(s: &str) -> String {
    hydo_assert::Value::from(s).to_json().unwrap()
}

/// A details value with its Go type.
#[derive(Clone, Debug)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    F64(f64),
    F32(f32),
    Str(S),
    Bytes(Vec<u8>),
    List(Vec<V>),
    Map(Vec<(S, V)>),
}

impl From<bool> for V {
    fn from(b: bool) -> V {
        V::Bool(b)
    }
}
impl From<i32> for V {
    fn from(i: i32) -> V {
        V::Int(i as i64)
    }
}
impl From<i64> for V {
    fn from(i: i64) -> V {
        V::Int(i)
    }
}
impl From<u64> for V {
    fn from(u: u64) -> V {
        V::Uint(u)
    }
}
impl From<f64> for V {
    fn from(f: f64) -> V {
        V::F64(f)
    }
}
impl From<f32> for V {
    fn from(f: f32) -> V {
        V::F32(f)
    }
}
impl From<&str> for V {
    fn from(s: &str) -> V {
        V::Str(s.into())
    }
}
impl From<String> for V {
    fn from(s: String) -> V {
        V::Str(s.into())
    }
}
impl From<S> for V {
    fn from(s: S) -> V {
        V::Str(s)
    }
}
impl From<()> for V {
    fn from(_: ()) -> V {
        V::Null
    }
}

/// `d! {"k" => v, ...}` or `d! {"k": v, ...}`: a map value (keys `&str` or `S`).
macro_rules! d {
    ($($k:literal : $v:expr),* $(,)?) => { $crate::model::V::Map(vec![$(($crate::model::S::from($k), $crate::model::V::from($v))),*]) };
    ($($k:expr => $v:expr),* $(,)?) => { $crate::model::V::Map(vec![$(($crate::model::S::from($k), $crate::model::V::from($v))),*]) };
}
/// `l![a, b, ...]`: a list value.
macro_rules! l {
    ($($v:expr),* $(,)?) => { $crate::model::V::List(vec![$($crate::model::V::from($v)),*]) };
}

pub fn bytes(b: &[u8]) -> V {
    V::Bytes(b.to_vec())
}

fn float_text(f: f64, debug: String) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f == f64::INFINITY {
        "+Inf".into()
    } else if f == f64::NEG_INFINITY {
        "-Inf".into()
    } else {
        debug
    }
}

impl V {
    pub fn json(&self) -> String {
        match self {
            V::Null => "null".into(),
            V::Bool(b) => b.to_string(),
            V::Int(i) => format!("{{\"int\":\"{}\"}}", i),
            V::Uint(u) => format!("{{\"uint\":\"{}\"}}", u),
            V::F64(f) => format!("{{\"f64\":\"{}\"}}", float_text(*f, format!("{:?}", f))),
            V::F32(f) => format!("{{\"f32\":\"{}\"}}", float_text(*f as f64, format!("{:?}", f))),
            V::Str(s) => s.json(),
            V::Bytes(b) => {
                let mut h = String::new();
                for x in b {
                    let _ = write!(h, "{:02x}", x);
                }
                format!("{{\"bytes\":\"{}\"}}", h)
            }
            V::List(l) => format!("[{}]", l.iter().map(V::json).collect::<Vec<_>>().join(",")),
            V::Map(m) => format!(
                "{{\"map\":[{}]}}",
                m.iter().map(|(k, v)| format!("[{},{}]", k.json(), v.json())).collect::<Vec<_>>().join(",")
            ),
        }
    }

    /// The hydo_assert value the Rust runner passes.
    pub fn rust(&self) -> hydo_assert::Value {
        use hydo_assert::Value as R;
        match self {
            V::Null => R::Null,
            V::Bool(b) => R::Bool(*b),
            V::Int(i) => R::Int(*i),
            V::Uint(u) => R::Uint(*u),
            V::F64(f) => R::Float(*f),
            V::F32(f) => R::Float32(*f),
            V::Str(s) => R::Str(s.rust()),
            V::Bytes(b) => R::Str(base64(b)),
            V::List(l) => R::List(l.iter().map(V::rust).collect()),
            V::Map(m) => R::Map(m.iter().map(|(k, v)| (k.rust(), v.rust())).collect()),
        }
    }

    fn walk(&self, f: &mut impl FnMut(&V)) {
        f(self);
        match self {
            V::List(l) => l.iter().for_each(|v| v.walk(f)),
            V::Map(m) => m.iter().for_each(|(_, v)| v.walk(f)),
            _ => {}
        }
    }
}

/// Go's base64.StdEncoding (how `[]byte` is written).
fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

/// A history record argument: a literal token or one of the recorders' helpers.
#[derive(Clone, Debug)]
pub enum Arg {
    Tok(S),
    Enc(S),
    Ver(S, u64),
    Von(Option<S>),
    List(Vec<S>),
    AppendMop(S, S),
    ReadMop(S),
    ReadResultMop(S, Vec<S>),
    Nil,
}

impl From<&str> for Arg {
    fn from(s: &str) -> Arg {
        Arg::Tok(s.into())
    }
}
impl From<S> for Arg {
    fn from(s: S) -> Arg {
        Arg::Tok(s)
    }
}

pub fn enc(s: impl Into<S>) -> Arg {
    Arg::Enc(s.into())
}
pub fn ver(s: impl Into<S>, n: u64) -> Arg {
    Arg::Ver(s.into(), n)
}
pub fn von(s: Option<&str>) -> Arg {
    Arg::Von(s.map(S::from))
}
pub fn hlist(l: &[&str]) -> Arg {
    Arg::List(l.iter().map(|e| S::from(*e)).collect())
}
pub fn amop(k: impl Into<S>, e: impl Into<S>) -> Arg {
    Arg::AppendMop(k.into(), e.into())
}
pub fn rmop(k: impl Into<S>) -> Arg {
    Arg::ReadMop(k.into())
}
pub fn rrmop(k: impl Into<S>, l: &[&str]) -> Arg {
    Arg::ReadResultMop(k.into(), l.iter().map(|e| S::from(*e)).collect())
}

impl Arg {
    pub fn json(&self) -> String {
        let list = |l: &[S]| format!("[{}]", l.iter().map(S::json).collect::<Vec<_>>().join(","));
        match self {
            Arg::Tok(s) => s.json(),
            Arg::Enc(s) => format!("{{\"enc\":{}}}", s.json()),
            Arg::Ver(s, n) => format!("{{\"ver\":[{},\"{}\"]}}", s.json(), n),
            Arg::Von(None) => "{\"value_or_nil\":null}".into(),
            Arg::Von(Some(s)) => format!("{{\"value_or_nil\":{}}}", s.json()),
            Arg::List(l) => format!("{{\"list\":{}}}", list(l)),
            Arg::AppendMop(k, e) => format!("{{\"append_mop\":[{},{}]}}", k.json(), e.json()),
            Arg::ReadMop(k) => format!("{{\"read_mop\":{}}}", k.json()),
            Arg::ReadResultMop(k, l) => format!("{{\"read_result_mop\":[{},{}]}}", k.json(), list(l)),
            Arg::Nil => "{\"nil\":true}".into(),
        }
    }

    /// The token the Rust recorder's helpers make.
    pub fn rust(&self) -> String {
        use hydo_assert::history as h;
        let v = |l: &[S]| l.iter().map(S::rust).collect::<Vec<_>>();
        match self {
            Arg::Tok(s) => s.rust(),
            Arg::Enc(s) => h::enc(&s.rust()),
            Arg::Ver(s, n) => h::ver(&s.rust(), *n),
            Arg::Von(o) => h::value_or_nil(o.as_ref().map(S::rust).as_deref()),
            Arg::List(l) => {
                let e = v(l);
                h::list(&e.iter().map(String::as_str).collect::<Vec<_>>())
            }
            Arg::AppendMop(k, e) => h::append_mop(&k.rust(), &e.rust()),
            Arg::ReadMop(k) => h::read_mop(&k.rust()),
            Arg::ReadResultMop(k, l) => {
                let e = v(l);
                h::read_result_mop(&k.rust(), &e.iter().map(String::as_str).collect::<Vec<_>>())
            }
            Arg::Nil => "nil".into(),
        }
    }

    fn strings(&self) -> Vec<&S> {
        match self {
            Arg::Tok(s) | Arg::Enc(s) | Arg::Ver(s, _) | Arg::ReadMop(s) => vec![s],
            Arg::Von(o) => o.iter().collect(),
            Arg::List(l) => l.iter().collect(),
            Arg::AppendMop(k, e) => vec![k, e],
            Arg::ReadResultMop(k, l) => std::iter::once(k).chain(l.iter()).collect(),
            Arg::Nil => vec![],
        }
    }
}

/// One SDK call. Handles (`a`, `w`, `h`, `o`) number the declared objects, the watches, the
/// histories and the invoked operations, each from 0 in the order they were made.
#[derive(Clone, Debug)]
pub enum Step {
    Decl {
        kind: &'static str,
        id: S,
        msg: S,
    },
    Eventually {
        id: S,
        msg: S,
        within_ms: u64,
    },
    Buggify {
        id: S,
        msg: S,
    },
    /// A steered value (`minimize` or `maximize`, Milestone 209).
    Steer {
        goal: &'static str,
        id: S,
        msg: S,
    },
    /// A steered value reported.
    Value {
        a: usize,
        v: i64,
    },
    Check {
        a: usize,
        cond: bool,
        d: Option<V>,
    },
    Hit {
        a: usize,
        d: Option<V>,
    },
    Cmp {
        a: usize,
        left: i64,
        op: &'static str,
        right: i64,
        d: Option<V>,
    },
    Begin {
        a: usize,
    },
    Done {
        w: usize,
        d: Option<V>,
    },
    Expire {
        w: usize,
        d: Option<V>,
    },
    Await {
        a: usize,
        d: Option<V>,
        asyn: bool,
    },
    Fire {
        a: usize,
        d: Option<V>,
    },
    Catalog,
    SetupComplete,
    History {
        name: &'static str,
    },
    Invoke {
        h: usize,
        proc_: i64,
        f: &'static str,
        args: Vec<Arg>,
    },
    Ok {
        o: usize,
        args: Vec<Arg>,
    },
    OkSame {
        o: usize,
    },
    Fail {
        o: usize,
    },
    Info {
        o: usize,
    },
    Record {
        h: usize,
        proc_: i64,
        typ: &'static str,
        f: &'static str,
        args: Vec<Arg>,
    },
}

/// A program of SDK calls, built in order. The builder checks that every handle exists and
/// has the right kind.
#[derive(Clone, Debug, Default)]
pub struct Prog {
    pub steps: Vec<Step>,
    kinds: Vec<&'static str>,
    watches: usize,
    hists: usize,
    op_nargs: Vec<usize>,
}

impl Prog {
    pub fn new() -> Prog {
        Prog::default()
    }
    fn push(&mut self, s: Step) {
        self.steps.push(s);
    }
    fn decl(&mut self, kind: &'static str, id: impl Into<S>, msg: impl Into<S>) -> usize {
        self.push(Step::Decl {
            kind,
            id: id.into(),
            msg: msg.into(),
        });
        self.kinds.push(kind);
        self.kinds.len() - 1
    }
    pub fn always(&mut self, id: impl Into<S>, msg: impl Into<S>) -> usize {
        self.decl("always", id, msg)
    }
    pub fn always_or_unreachable(&mut self, id: impl Into<S>, msg: impl Into<S>) -> usize {
        self.decl("always_or_unreachable", id, msg)
    }
    pub fn sometimes(&mut self, id: impl Into<S>, msg: impl Into<S>) -> usize {
        self.decl("sometimes", id, msg)
    }
    pub fn reachable(&mut self, id: impl Into<S>, msg: impl Into<S>) -> usize {
        self.decl("reachable", id, msg)
    }
    pub fn unreachable(&mut self, id: impl Into<S>, msg: impl Into<S>) -> usize {
        self.decl("unreachable", id, msg)
    }
    pub fn eventually(&mut self, id: impl Into<S>, msg: impl Into<S>, within_ms: u64) -> usize {
        self.push(Step::Eventually {
            id: id.into(),
            msg: msg.into(),
            within_ms,
        });
        self.kinds.push("eventually");
        self.kinds.len() - 1
    }
    pub fn buggify(&mut self, id: &str, msg: impl Into<S>) -> usize {
        self.push(Step::Buggify {
            id: id.into(),
            msg: msg.into(),
        });
        self.kinds.push("buggify");
        self.kinds.len() - 1
    }
    pub fn steer(&mut self, goal: &'static str, id: impl Into<S>, msg: impl Into<S>) -> usize {
        assert!(matches!(goal, "min" | "max"));
        self.push(Step::Steer { goal, id: id.into(), msg: msg.into() });
        self.kinds.push("steer");
        self.kinds.len() - 1
    }
    pub fn value(&mut self, a: usize, v: i64) {
        assert_eq!(self.kind(a), "steer", "value on {}", self.kind(a));
        self.push(Step::Value { a, v });
    }
    fn kind(&self, a: usize) -> &'static str {
        self.kinds.get(a).copied().unwrap_or_else(|| panic!("no declared object {}", a))
    }
    pub fn check(&mut self, a: usize, cond: bool, d: Option<V>) {
        assert!(
            matches!(self.kind(a), "always" | "always_or_unreachable" | "sometimes"),
            "check on {}",
            self.kind(a)
        );
        self.push(Step::Check { a, cond, d });
    }
    pub fn hit(&mut self, a: usize, d: Option<V>) {
        assert!(matches!(self.kind(a), "reachable" | "unreachable"), "hit on {}", self.kind(a));
        self.push(Step::Hit { a, d });
    }
    pub fn cmp(&mut self, a: usize, left: i64, op: &'static str, right: i64, d: Option<V>) {
        assert!(matches!(self.kind(a), "always" | "always_or_unreachable" | "sometimes"));
        self.push(Step::Cmp {
            a,
            left,
            op,
            right,
            d,
        });
    }
    pub fn begin(&mut self, a: usize) -> usize {
        assert_eq!(self.kind(a), "eventually");
        self.push(Step::Begin { a });
        self.watches += 1;
        self.watches - 1
    }
    pub fn done(&mut self, w: usize, d: Option<V>) {
        assert!(w < self.watches);
        self.push(Step::Done { w, d });
    }
    pub fn expire(&mut self, w: usize, d: Option<V>) {
        assert!(w < self.watches);
        self.push(Step::Expire { w, d });
    }
    /// The condition is true at its first evaluation.
    pub fn await_(&mut self, a: usize, d: Option<V>, asyn: bool) {
        assert_eq!(self.kind(a), "eventually");
        self.push(Step::Await { a, d, asyn });
    }
    pub fn fire(&mut self, a: usize, d: Option<V>) {
        assert_eq!(self.kind(a), "buggify");
        self.push(Step::Fire { a, d });
    }
    pub fn catalog(&mut self) {
        self.push(Step::Catalog);
    }
    pub fn setup_complete(&mut self) {
        self.push(Step::SetupComplete);
    }
    pub fn history(&mut self, name: &'static str) -> usize {
        self.push(Step::History { name });
        self.hists += 1;
        self.hists - 1
    }
    pub fn invoke(&mut self, h: usize, proc_: i64, f: &'static str, args: Vec<Arg>) -> usize {
        assert!(h < self.hists);
        self.op_nargs.push(args.len());
        self.push(Step::Invoke { h, proc_, f, args });
        self.op_nargs.len() - 1
    }
    /// A completion with these results. With no results the invocation must have had no
    /// arguments either (Go, JavaScript and Python repeat the invocation's arguments then; use
    /// `ok_same` for that).
    pub fn ok(&mut self, o: usize, args: Vec<Arg>) {
        assert!(
            !args.is_empty() || self.op_nargs[o] == 0,
            "ok() without results on an op with arguments: use ok_same"
        );
        self.push(Step::Ok { o, args });
    }
    pub fn ok_same(&mut self, o: usize) {
        assert!(o < self.op_nargs.len());
        self.push(Step::OkSame { o });
    }
    pub fn fail(&mut self, o: usize) {
        assert!(o < self.op_nargs.len());
        self.push(Step::Fail { o });
    }
    pub fn info(&mut self, o: usize) {
        assert!(o < self.op_nargs.len());
        self.push(Step::Info { o });
    }
    pub fn record(&mut self, h: usize, proc_: i64, typ: &'static str, f: &'static str, args: Vec<Arg>) {
        assert!(h < self.hists);
        self.push(Step::Record { h, proc_, typ, f, args });
    }

    /// The SDK features a reader needs to run this program (a reader skips a case that needs
    /// something its language cannot express, and says so).
    pub fn needs(&self) -> Vec<&'static str> {
        let mut n = std::collections::BTreeSet::new();
        let val = |v: &V, n: &mut std::collections::BTreeSet<&'static str>| {
            v.walk(&mut |x| match x {
                V::F32(_) => {
                    n.insert("f32");
                }
                V::Bytes(_) => {
                    n.insert("bytes");
                }
                V::Str(s) if s.is_raw() => {
                    n.insert("invalid_utf8");
                }
                V::Map(m) if m.iter().any(|(k, _)| k.is_raw()) => {
                    n.insert("invalid_utf8");
                }
                _ => {}
            });
            check_key_order(v);
        };
        for s in &self.steps {
            match s {
                Step::Decl { id, msg, .. } | Step::Eventually { id, msg, .. } | Step::Buggify { id, msg } => {
                    if id.is_raw() || msg.is_raw() {
                        n.insert("invalid_utf8");
                    }
                }
                // the Rust SDK has steered values first (Milestone 209); the other SDKs skip
                // these cases until they have them
                Step::Steer { .. } | Step::Value { .. } => {
                    n.insert("steer");
                }
                Step::Check { d: Some(v), .. }
                | Step::Hit { d: Some(v), .. }
                | Step::Cmp { d: Some(v), .. }
                | Step::Done { d: Some(v), .. }
                | Step::Expire { d: Some(v), .. }
                | Step::Await { d: Some(v), .. }
                | Step::Fire { d: Some(v), .. } => val(v, &mut n),
                Step::Invoke { args, .. } | Step::Ok { args, .. } | Step::Record { args, .. } => {
                    if args.iter().any(|a| a.strings().iter().any(|s| s.is_raw())) {
                        n.insert("raw_hist");
                    }
                }
                _ => {}
            }
        }
        n.into_iter().collect()
    }

    pub fn steps_json(&self) -> Vec<String> {
        self.steps.iter().map(step_json).collect()
    }
}

/// A map with invalid-UTF-8 keys must sort the same in every SDK: by Go's raw bytes, by the
/// Rust runner's marker characters and by JavaScript's U+FFFD.
fn check_key_order(v: &V) {
    v.walk(&mut |x| {
        if let V::Map(m) = x {
            if m.iter().any(|(k, _)| k.is_raw()) {
                let order = |f: &dyn Fn(&S) -> Vec<u8>| {
                    let mut ix: Vec<usize> = (0..m.len()).collect();
                    ix.sort_by(|a, b| f(&m[*a].0).cmp(&f(&m[*b].0)));
                    ix
                };
                let go = order(&|s: &S| s.go_bytes());
                assert_eq!(go, order(&|s: &S| s.rust().into_bytes()), "key order differs with the Rust markers: {:?}", m);
                assert_eq!(go, order(&|s: &S| s.js_bytes()), "key order differs with JavaScript's U+FFFD: {:?}", m);
            }
        }
    });
}

fn obj(fields: &[(&str, String)]) -> String {
    let f: Vec<String> = fields.iter().map(|(k, v)| format!("{}:{}", jstr(k), v)).collect();
    format!("{{{}}}", f.join(","))
}

fn det(d: &Option<V>) -> Option<(&'static str, String)> {
    d.as_ref().map(|v| ("details", v.json()))
}

fn args_json(a: &[Arg]) -> String {
    format!("[{}]", a.iter().map(Arg::json).collect::<Vec<_>>().join(","))
}

fn step_json(s: &Step) -> String {
    let mut f: Vec<(&str, String)> = Vec::new();
    match s {
        Step::Decl { kind, id, msg } => {
            f.push(("do", jstr(kind)));
            f.push(("id", id.json()));
            f.push(("msg", msg.json()));
        }
        Step::Eventually { id, msg, within_ms } => {
            f.push(("do", jstr("eventually")));
            f.push(("id", id.json()));
            f.push(("msg", msg.json()));
            f.push(("within_ms", within_ms.to_string()));
        }
        Step::Buggify { id, msg } => {
            f.push(("do", jstr("buggify")));
            f.push(("id", id.json()));
            f.push(("msg", msg.json()));
        }
        Step::Steer { goal, id, msg } => {
            f.push(("do", jstr(if *goal == "min" { "minimize" } else { "maximize" })));
            f.push(("id", id.json()));
            f.push(("msg", msg.json()));
        }
        Step::Value { a, v } => {
            f.push(("do", jstr("value")));
            f.push(("a", a.to_string()));
            f.push(("v", V::Int(*v).json()));
        }
        Step::Check { a, cond, d } => {
            f.push(("do", jstr("check")));
            f.push(("a", a.to_string()));
            f.push(("cond", cond.to_string()));
            f.extend(det(d));
        }
        Step::Hit { a, d } => {
            f.push(("do", jstr("hit")));
            f.push(("a", a.to_string()));
            f.extend(det(d));
        }
        Step::Cmp { a, left, op, right, d } => {
            f.push(("do", jstr("cmp")));
            f.push(("a", a.to_string()));
            f.push(("left", V::Int(*left).json()));
            f.push(("op", jstr(op)));
            f.push(("right", V::Int(*right).json()));
            f.extend(det(d));
        }
        Step::Begin { a } => {
            f.push(("do", jstr("begin")));
            f.push(("a", a.to_string()));
        }
        Step::Done { w, d } => {
            f.push(("do", jstr("done")));
            f.push(("w", w.to_string()));
            f.extend(det(d));
        }
        Step::Expire { w, d } => {
            f.push(("do", jstr("expire")));
            f.push(("w", w.to_string()));
            f.extend(det(d));
        }
        Step::Await { a, d, asyn } => {
            f.push(("do", jstr("await")));
            f.push(("a", a.to_string()));
            if *asyn {
                f.push(("async", "true".into()));
            }
            f.extend(det(d));
        }
        Step::Fire { a, d } => {
            f.push(("do", jstr("fire")));
            f.push(("a", a.to_string()));
            f.extend(det(d));
        }
        Step::Catalog => f.push(("do", jstr("catalog"))),
        Step::SetupComplete => f.push(("do", jstr("setup_complete"))),
        Step::History { name } => {
            f.push(("do", jstr("history")));
            f.push(("name", jstr(name)));
        }
        Step::Invoke { h, proc_, f: func, args } => {
            f.push(("do", jstr("invoke")));
            f.push(("h", h.to_string()));
            f.push(("proc", V::Int(*proc_).json()));
            f.push(("f", jstr(func)));
            f.push(("args", args_json(args)));
        }
        Step::Ok { o, args } => {
            f.push(("do", jstr("ok")));
            f.push(("o", o.to_string()));
            f.push(("args", args_json(args)));
        }
        Step::OkSame { o } => {
            f.push(("do", jstr("ok_same")));
            f.push(("o", o.to_string()));
        }
        Step::Fail { o } => {
            f.push(("do", jstr("fail")));
            f.push(("o", o.to_string()));
        }
        Step::Info { o } => {
            f.push(("do", jstr("info")));
            f.push(("o", o.to_string()));
        }
        Step::Record { h, proc_, typ, f: func, args } => {
            f.push(("do", jstr("record")));
            f.push(("h", h.to_string()));
            f.push(("proc", V::Int(*proc_).json()));
            f.push(("type", jstr(typ)));
            f.push(("f", jstr(func)));
            f.push(("args", args_json(args)));
        }
    }
    obj(&f)
}

/// One configuration a program runs under. `out` is where the assertion records go
/// (`HYDO_ASSERT_OUT`): `file` (an empty file), `append` (a file that already holds one line),
/// `stderr`, `missing` (a path that does not exist: the SDK falls back to standard error) or
/// `off`. `hist_out` is the same for history records (`HYDO_HIST_OUT`), with `create` (a file
/// that does not exist yet: the recorder creates it) and `fallback` (a path in a directory
/// that does not exist: standard error) instead of `missing`. The case's output file holds
/// what the one sink that is not `off` received.
#[derive(Clone, Debug)]
pub struct Run {
    pub name: &'static str,
    pub out: &'static str,
    pub hist_out: &'static str,
    /// `HYDO_BUGGIFY` (None: unset)
    pub buggify_env: Option<String>,
    /// the content of the file `HYDO_BUGGIFY_FILE` names (None: it names a missing file)
    pub buggify_file: Option<String>,
}

impl Run {
    pub fn assert(name: &'static str) -> Run {
        Run {
            name,
            out: "file",
            hist_out: "off",
            buggify_env: None,
            buggify_file: None,
        }
    }
    pub fn hist(name: &'static str) -> Run {
        Run {
            name,
            out: "off",
            hist_out: "file",
            buggify_env: None,
            buggify_file: None,
        }
    }
    pub fn out(mut self, o: &'static str) -> Run {
        self.out = o;
        self
    }
    pub fn hist_out(mut self, o: &'static str) -> Run {
        self.hist_out = o;
        self
    }
    pub fn bg_env(mut self, s: &str) -> Run {
        self.buggify_env = Some(s.into());
        self
    }
    pub fn bg_file(mut self, s: &str) -> Run {
        self.buggify_file = Some(s.into());
        self
    }
}

pub struct Case {
    pub area: &'static str,
    pub name: &'static str,
    pub about: &'static str,
    pub prog: Prog,
    pub runs: Vec<Run>,
}

impl Case {
    pub fn id(&self) -> String {
        format!("{}/{}", self.area, self.name)
    }
    /// The output file of a run: `<case>.out` for a case with one unnamed run, else
    /// `<case>.<run>.out`.
    pub fn out_file(&self, run: &Run) -> String {
        if run.name.is_empty() {
            format!("{}.out", self.name)
        } else {
            format!("{}.{}.out", self.name, run.name)
        }
    }
    pub fn json(&self, returns: &[String]) -> String {
        let opt = |o: &Option<String>| o.as_ref().map_or("null".to_string(), |s| jstr(s));
        let mut s = String::new();
        s.push_str("{\n");
        let _ = writeln!(s, "  \"about\": {},", jstr(self.about));
        let needs: Vec<String> = self.prog.needs().iter().map(|n| jstr(n)).collect();
        let _ = writeln!(s, "  \"needs\": [{}],", needs.join(", "));
        s.push_str("  \"steps\": [\n");
        let steps = self.prog.steps_json();
        for (i, st) in steps.iter().enumerate() {
            let _ = writeln!(s, "    {}{}", st, if i + 1 < steps.len() { "," } else { "" });
        }
        s.push_str("  ],\n  \"runs\": [\n");
        for (i, (r, ret)) in self.runs.iter().zip(returns).enumerate() {
            let _ = writeln!(
                s,
                "    {{\"name\": {}, \"out\": {}, \"hist_out\": {}, \"buggify_env\": {}, \"buggify_file\": {}, \"output\": {}, \"returns\": {}}}{}",
                jstr(r.name),
                jstr(r.out),
                jstr(r.hist_out),
                opt(&r.buggify_env),
                opt(&r.buggify_file),
                jstr(&self.out_file(r)),
                jstr(ret),
                if i + 1 < self.runs.len() { "," } else { "" }
            );
        }
        s.push_str("  ]\n}\n");
        s
    }
}
