//! Values of a fuzzed function's parameters and results: the text the harness reads and writes
//! (the wire form), the form a report shows and `--input` reads back, generation from a seed,
//! and the simpler values a shrink tries.

use super::rust::FTy;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// A value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Int(i128),
    Bool(bool),
    /// A floating-point value as the bits of an `f64` (an `f32` widened exactly), so that the
    /// value keeps `Eq` (Milestone 289).
    Float(u64),
    Str(String),
    List(Vec<Val>),
    Opt(Option<Box<Val>>),
    Res(Result<Box<Val>, Box<Val>>),
    Tuple(Vec<Val>),
    /// A structure's fields by name (a tuple structure's are `0`, `1`, ...).
    Struct(Vec<(String, Val)>),
    /// A value of a type fuzzing does not read.
    Opaque,
}

/// The smallest and largest value of an integer type (a `u128` is limited to `i128::MAX`).
pub fn int_range(signed: bool, bits: u32) -> (i128, i128) {
    if bits >= 128 {
        return if signed {
            (i128::MIN, i128::MAX)
        } else {
            (0, i128::MAX)
        };
    }
    if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// The wire form: space-separated words in the order of the type. An integer is its decimal
/// digits, a boolean `t` or `f`, a string `s` and its bytes in hexadecimal, a list `L<n>` and its
/// elements, a floating-point value `F` and the hexadecimal bits of its `f64`, an option `N` or
/// `S` and the value, a result `O` or `E` and the value, a tuple or a
/// structure its fields in order, and an opaque value `X`.
pub fn encode(v: &Val, o: &mut String) {
    match v {
        Val::Int(x) => {
            let _ = write!(o, "{x} ");
        }
        Val::Bool(b) => o.push_str(if *b { "t " } else { "f " }),
        Val::Float(b) => {
            let _ = write!(o, "F{b:x} ");
        }
        Val::Str(s) => {
            o.push('s');
            for b in s.as_bytes() {
                let _ = write!(o, "{b:02x}");
            }
            o.push(' ');
        }
        Val::List(l) => {
            let _ = write!(o, "L{} ", l.len());
            for x in l {
                encode(x, o);
            }
        }
        Val::Opt(None) => o.push_str("N "),
        Val::Opt(Some(x)) => {
            o.push_str("S ");
            encode(x, o);
        }
        Val::Res(Ok(x)) => {
            o.push_str("O ");
            encode(x, o);
        }
        Val::Res(Err(x)) => {
            o.push_str("E ");
            encode(x, o);
        }
        Val::Tuple(v) => v.iter().for_each(|x| encode(x, o)),
        Val::Struct(f) => f.iter().for_each(|(_, x)| encode(x, o)),
        Val::Opaque => o.push_str("X "),
    }
}

/// Read one value of type `t` from wire words.
pub fn decode<'a>(t: &FTy, w: &mut impl Iterator<Item = &'a str>) -> Result<Val, String> {
    let mut word = || w.next().ok_or_else(|| "the answer ended early".to_string());
    Ok(match t {
        FTy::Int { .. } => {
            let x = word()?;
            if x == "X" {
                return Ok(Val::Opaque);
            }
            Val::Int(x.parse().map_err(|_| format!("`{x}` is not an integer"))?)
        }
        FTy::Bool => match word()? {
            "t" => Val::Bool(true),
            "f" => Val::Bool(false),
            "X" => Val::Opaque,
            x => return Err(format!("`{x}` is not a boolean")),
        },
        FTy::Float { .. } => {
            let x = word()?;
            if x == "X" {
                return Ok(Val::Opaque);
            }
            let h = x
                .strip_prefix('F')
                .ok_or_else(|| format!("`{x}` is not a floating-point value"))?;
            Val::Float(u64::from_str_radix(h, 16).map_err(|e| e.to_string())?)
        }
        FTy::Str => {
            let x = word()?;
            if x == "X" {
                return Ok(Val::Opaque);
            }
            let h = x
                .strip_prefix('s')
                .ok_or_else(|| format!("`{x}` is not a string"))?;
            let bytes: Result<Vec<u8>, _> = (0..h.len() / 2)
                .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16))
                .collect();
            Val::Str(String::from_utf8_lossy(&bytes.map_err(|e| e.to_string())?).into_owned())
        }
        FTy::List(x) | FTy::Array(x, _) => {
            let n = word()?;
            if n == "X" {
                return Ok(Val::Opaque);
            }
            let n: usize = n
                .strip_prefix('L')
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("`{n}` is not a list's length"))?;
            let mut l = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                l.push(decode(x, w)?);
            }
            Val::List(l)
        }
        FTy::Opt(x) => match word()? {
            "N" => Val::Opt(None),
            "S" => Val::Opt(Some(Box::new(decode(x, w)?))),
            "X" => Val::Opaque,
            o => return Err(format!("`{o}` is not an option")),
        },
        FTy::Res(a, b) => match word()? {
            "O" => Val::Res(Ok(Box::new(decode(a, w)?))),
            "E" => Val::Res(Err(Box::new(decode(b, w)?))),
            "X" => Val::Opaque,
            o => return Err(format!("`{o}` is not a result")),
        },
        FTy::Tuple(v) => Val::Tuple(v.iter().map(|x| decode(x, w)).collect::<Result<_, _>>()?),
        FTy::Boxed(_, x) => decode(x, w)?,
        FTy::Struct { fields, .. } => Val::Struct(
            fields
                .iter()
                .map(|(n, x)| Ok((n.clone(), decode(x, w)?)))
                .collect::<Result<_, String>>()?,
        ),
        FTy::Opaque(_) | FTy::Hidden(_) => {
            word()?;
            Val::Opaque
        }
    })
}

/// The value as a report shows it, close to a Rust literal.
pub fn show(v: &Val, t: &FTy) -> String {
    match (v, t) {
        (Val::Int(x), _) => x.to_string(),
        (Val::Bool(b), _) => b.to_string(),
        (Val::Float(b), FTy::Float { bits: 32 }) => format!("{:?}", f64::from_bits(*b) as f32),
        (Val::Float(b), _) => format!("{:?}", f64::from_bits(*b)),
        (Val::Str(s), _) => format!("{s:?}"),
        (Val::List(l), FTy::List(x) | FTy::Array(x, _))
            if is_byte(x) && l.iter().all(|v| matches!(v, Val::Int(0..=255))) =>
        {
            // bytes as a byte string, `b"GET /\r\n"` (Milestone 289)
            let mut s = String::from("b\"");
            for v in l {
                if let Val::Int(b) = v {
                    s.extend(std::ascii::escape_default(*b as u8).map(char::from));
                }
            }
            s.push('"');
            s
        }
        (Val::List(l), FTy::List(x) | FTy::Array(x, _)) => format!(
            "[{}]",
            l.iter().map(|e| show(e, x)).collect::<Vec<_>>().join(", ")
        ),
        (Val::Opt(None), _) => "None".into(),
        (Val::Opt(Some(x)), FTy::Opt(t)) => format!("Some({})", show(x, t)),
        (Val::Res(Ok(x)), FTy::Res(t, _)) => format!("Ok({})", show(x, t)),
        (Val::Res(Err(x)), FTy::Res(_, t)) => format!("Err({})", show(x, t)),
        (_, FTy::Boxed(_, t)) => show(v, t),
        (Val::Tuple(v), FTy::Tuple(ts)) => {
            let p: Vec<String> = v.iter().zip(ts).map(|(x, t)| show(x, t)).collect();
            if p.len() == 1 {
                format!("({},)", p[0])
            } else {
                format!("({})", p.join(", "))
            }
        }
        (
            Val::Struct(fv),
            FTy::Struct {
                path,
                tuple,
                fields,
            },
        ) => {
            let name = path.rsplit("::").next().unwrap_or(path);
            if *tuple {
                let p: Vec<String> = fv
                    .iter()
                    .zip(fields)
                    .map(|((_, x), (_, t))| show(x, t))
                    .collect();
                format!("{name}({})", p.join(", "))
            } else if fv.is_empty() {
                format!("{name} {{}}")
            } else {
                let p: Vec<String> = fv
                    .iter()
                    .zip(fields)
                    .map(|((n, x), (_, t))| format!("{n}: {}", show(x, t)))
                    .collect();
                format!("{name} {{ {} }}", p.join(", "))
            }
        }
        _ => "<not read>".into(),
    }
}

/// Read back the form [`show`] writes, for a value of type `t`.
pub fn parse(s: &str, t: &FTy) -> Result<Val, String> {
    let mut p = Reader { s, i: 0 };
    let v = p.val(t)?;
    p.ws();
    if p.i != s.len() {
        return Err(format!("unexpected `{}` in `{s}`", &s[p.i..]));
    }
    Ok(v)
}

/// Read several `name = value` pairs separated by commas (an `--input` text).
pub fn parse_args(s: &str, params: &[(String, FTy)]) -> Result<Vec<Val>, String> {
    let mut p = Reader { s, i: 0 };
    let mut got: BTreeMap<String, Val> = BTreeMap::new();
    loop {
        p.ws();
        if p.i >= s.len() {
            break;
        }
        let name = p.ident();
        let Some((_, t)) = params.iter().find(|(n, _)| *n == name) else {
            return Err(format!("`{name}` is not a parameter of the function"));
        };
        p.ws();
        p.expect("=")?;
        let v = p.val(t)?;
        got.insert(name, v);
        p.ws();
        if p.i < s.len() {
            p.expect(",")?;
        }
    }
    params
        .iter()
        .map(|(n, _)| {
            got.remove(n)
                .ok_or_else(|| format!("the input names no value for `{n}`"))
        })
        .collect()
}

struct Reader<'a> {
    s: &'a str,
    i: usize,
}

impl Reader<'_> {
    fn ws(&mut self) {
        while self.s[self.i..].starts_with(char::is_whitespace) {
            self.i += self.s[self.i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(t) {
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &str) -> Result<(), String> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!("expected `{t}` at `{}`", &self.s[self.i..]))
        }
    }
    fn ident(&mut self) -> String {
        self.ws();
        let st = self.i;
        while self.s[self.i..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            self.i += 1;
        }
        self.s[st..self.i].to_string()
    }
    fn val(&mut self, t: &FTy) -> Result<Val, String> {
        self.ws();
        match t {
            FTy::Int { signed, bits, .. } => {
                let st = self.i;
                if self.s[self.i..].starts_with('-') {
                    self.i += 1;
                }
                while self.s[self.i..].starts_with(|c: char| c.is_ascii_digit() || c == '_') {
                    self.i += 1;
                }
                let x: i128 = self.s[st..self.i]
                    .replace('_', "")
                    .parse()
                    .map_err(|_| format!("expected an integer at `{}`", &self.s[st..]))?;
                let (lo, hi) = int_range(*signed, *bits);
                if x < lo || x > hi {
                    return Err(format!("{x} is outside the type's range"));
                }
                Ok(Val::Int(x))
            }
            FTy::Float { bits } => {
                let st = self.i;
                while self.s[self.i..].starts_with(|c: char| {
                    c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.' | '_')
                }) {
                    self.i += 1;
                }
                let w = self.s[st..self.i].replace('_', "");
                let x: f64 = match w.as_str() {
                    "NaN" | "nan" => f64::NAN,
                    "inf" | "+inf" => f64::INFINITY,
                    "-inf" => f64::NEG_INFINITY,
                    _ => w
                        .parse()
                        .map_err(|_| format!("expected a floating-point value at `{w}`"))?,
                };
                Ok(Val::Float(float_bits(x, *bits)))
            }
            FTy::Bool => match self.ident().as_str() {
                "true" => Ok(Val::Bool(true)),
                "false" => Ok(Val::Bool(false)),
                o => Err(format!("`{o}` is not a boolean")),
            },
            FTy::Str => {
                // a string in Rust's escaped form
                self.expect("\"")?;
                let mut out = String::new();
                let mut it = self.s[self.i..].char_indices();
                loop {
                    let Some((k, c)) = it.next() else {
                        return Err("a string without its closing `\"`".into());
                    };
                    match c {
                        '"' => {
                            self.i += k + 1;
                            break;
                        }
                        '\\' => match it.next().map(|x| x.1) {
                            Some('n') => out.push('\n'),
                            Some('t') => out.push('\t'),
                            Some('r') => out.push('\r'),
                            Some('0') => out.push('\0'),
                            Some('\\') => out.push('\\'),
                            Some('"') => out.push('"'),
                            Some('\'') => out.push('\''),
                            Some('u') => {
                                let rest: String =
                                    it.by_ref().map(|x| x.1).take_while(|&c| c != '}').collect();
                                let h = rest.trim_start_matches('{');
                                let c = u32::from_str_radix(h, 16)
                                    .ok()
                                    .and_then(char::from_u32)
                                    .ok_or("a bad \\u escape")?;
                                out.push(c);
                            }
                            _ => return Err("an unknown escape in a string".into()),
                        },
                        c => out.push(c),
                    }
                }
                Ok(Val::Str(out))
            }
            FTy::List(x) | FTy::Array(x, _)
                if is_byte(x) && {
                    self.ws();
                    self.s[self.i..].starts_with("b\"")
                } =>
            {
                self.i += 2;
                let mut l = Vec::new();
                let b = self.s.as_bytes();
                loop {
                    let Some(&c) = b.get(self.i) else {
                        return Err("a byte string without its closing `\"`".into());
                    };
                    self.i += 1;
                    match c {
                        b'"' => break,
                        b'\\' => {
                            let e = b.get(self.i).copied().unwrap_or(0);
                            self.i += 1;
                            l.push(match e {
                                b'n' => b'\n',
                                b't' => b'\t',
                                b'r' => b'\r',
                                b'0' => 0,
                                b'x' => {
                                    let h = self.s.get(self.i..self.i + 2).unwrap_or("");
                                    self.i += 2;
                                    u8::from_str_radix(h, 16)
                                        .map_err(|_| format!("a bad `\\x{h}` escape"))?
                                }
                                o => o,
                            });
                        }
                        o => l.push(o),
                    }
                }
                if let FTy::Array(_, n) = t {
                    if l.len() != *n {
                        return Err(format!("an array of {n} needs {n} elements"));
                    }
                }
                Ok(Val::List(
                    l.into_iter().map(|b| Val::Int(b as i128)).collect(),
                ))
            }
            FTy::List(x) | FTy::Array(x, _) => {
                self.eat("vec!");
                self.eat("&");
                self.expect("[")?;
                let mut l = Vec::new();
                if self.eat("]") {
                    return Ok(Val::List(l));
                }
                loop {
                    l.push(self.val(x)?);
                    if self.eat("]") {
                        break;
                    }
                    self.expect(",")?;
                    if self.eat("]") {
                        break;
                    }
                }
                if let FTy::Array(_, n) = t {
                    if l.len() != *n {
                        return Err(format!("an array of {n} needs {n} elements"));
                    }
                }
                Ok(Val::List(l))
            }
            FTy::Opt(x) => {
                if self.eat("None") {
                    return Ok(Val::Opt(None));
                }
                self.expect("Some(")?;
                let v = self.val(x)?;
                self.expect(")")?;
                Ok(Val::Opt(Some(Box::new(v))))
            }
            FTy::Res(a, b) => {
                if self.eat("Ok(") {
                    let v = self.val(a)?;
                    self.expect(")")?;
                    return Ok(Val::Res(Ok(Box::new(v))));
                }
                self.expect("Err(")?;
                let v = self.val(b)?;
                self.expect(")")?;
                Ok(Val::Res(Err(Box::new(v))))
            }
            FTy::Boxed(_, x) => self.val(x),
            FTy::Tuple(ts) => {
                self.expect("(")?;
                let mut v = Vec::new();
                for (k, x) in ts.iter().enumerate() {
                    if k > 0 {
                        self.expect(",")?;
                    }
                    v.push(self.val(x)?);
                }
                self.eat(",");
                self.expect(")")?;
                Ok(Val::Tuple(v))
            }
            FTy::Struct {
                path,
                tuple,
                fields,
            } => {
                let name = self.ident();
                let want = path.rsplit("::").next().unwrap_or(path);
                if name != want {
                    return Err(format!("expected `{want}`, found `{name}`"));
                }
                let mut fv = Vec::new();
                if *tuple {
                    if fields.is_empty() && !self.s[self.i..].trim_start().starts_with('(') {
                        return Ok(Val::Struct(fv));
                    }
                    self.expect("(")?;
                    for (k, (n, x)) in fields.iter().enumerate() {
                        if k > 0 {
                            self.expect(",")?;
                        }
                        fv.push((n.clone(), self.val(x)?));
                    }
                    self.expect(")")?;
                } else {
                    self.expect("{")?;
                    for (k, (n, x)) in fields.iter().enumerate() {
                        if k > 0 {
                            self.expect(",")?;
                        }
                        let got = self.ident();
                        if got != *n {
                            return Err(format!("expected the field `{n}`, found `{got}`"));
                        }
                        self.expect(":")?;
                        fv.push((n.clone(), self.val(x)?));
                    }
                    self.eat(",");
                    self.expect("}")?;
                }
                Ok(Val::Struct(fv))
            }
            FTy::Opaque(o) => Err(format!("a value of `{o}` cannot be given")),
            FTy::Hidden(why) => Err(format!("{why}: its value cannot be given")),
        }
    }
}

/// Whether a list's elements are bytes (`u8`).
pub fn is_byte(t: &FTy) -> bool {
    matches!(
        t,
        FTy::Int {
            signed: false,
            bits: 8,
            ..
        }
    )
}

/// The bits of `x` as a value of a floating-point type of `bits` (an `f32` is rounded to one).
pub fn float_bits(x: f64, bits: u32) -> u64 {
    if bits == 32 {
        (x as f32 as f64).to_bits()
    } else {
        x.to_bits()
    }
}

/// A deterministic generator (SplitMix64).
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }
    pub fn word(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// A number in `0..n` (`n` > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.word() % n.max(1)
    }
    /// A number in `lo..=hi`.
    pub fn within(&mut self, lo: i128, hi: i128) -> i128 {
        if hi <= lo {
            return lo;
        }
        let span = (hi - lo) as u128;
        let r = ((self.word() as u128) << 64 | self.word() as u128) % span.saturating_add(1).max(1);
        lo + r as i128
    }
}

/// What the clauses and the `input` lines say about the values at a path (`amount`,
/// `l.balance`, `v[]` for the elements of `v`, `len(data)` as `data#len`).
#[derive(Clone, Debug, Default)]
pub struct Hints {
    /// `input <path> in lo..=hi`, and `input <path> len lo..=hi` as `<path>#len`.
    pub domains: BTreeMap<String, (i128, i128)>,
    /// `input <path> chars <set>`: a string's characters.
    pub chars: BTreeMap<String, Vec<char>>,
    /// `input <path> like "<text>", ...`: example strings that the generator uses and changes.
    pub like: BTreeMap<String, Vec<String>>,
    /// `input <path> like b"<bytes>", ...` for a list of bytes (Milestone 289).
    pub like_bytes: BTreeMap<String, Vec<Vec<u8>>>,
    /// Constants a clause compares the path with: the generator tries each, one below and one
    /// above.
    pub consts: BTreeMap<String, Vec<i128>>,
    /// Pairs of paths that a clause relates (`req_id > old(l.last_req)`): the generator sometimes
    /// makes them equal or one apart.
    pub pairs: Vec<(String, String)>,
}

/// How a generated value grows: lists and strings are short in the first cases and longer later.
pub struct Gen<'a> {
    pub rng: &'a mut Rng,
    pub hints: &'a Hints,
    /// 0 for the smallest values, 1 for the largest, 2 for random ones.
    pub mode: u8,
    /// The longest list or string of a random length; an edge size can be longer.
    pub max_len: usize,
    /// The elements, characters and integers that one input may still have, so that nested
    /// lists stay small ([`BUDGET`] at the start of an input).
    pub budget: usize,
}

/// The elements, characters and integers of one generated input, at most.
pub const BUDGET: usize = 1 << 16;

/// The longest list or string without a `len` part.
pub const MAX_LEN: usize = 4096;

/// The largest `usize` or `isize` value without an `input` line (and the smallest is its
/// negative): a size that large allocates a mebibyte or more, while terabytes would not return
/// before the timeout (Milestone 282). An `input` line gives the whole range.
pub const SIZE_BOUND: i128 = 1 << 20;

/// The lengths that a loop or a buffer often has at its edges: the generator picks among them
/// in some inputs.
pub const EDGE_LENS: &[usize] = &[
    0, 1, 2, 3, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511,
    512, 513, 1023, 1024, 1025, 4095, 4096,
];

/// The printable ASCII characters, the second default set of a string.
fn ascii() -> Vec<char> {
    (' '..='~').collect()
}

/// The longest random length for the `k`-th input: short first, then longer.
pub fn max_len_for(k: usize) -> usize {
    match k {
        0..=99 => 4,
        100..=299 => 16,
        300..=599 => 64,
        _ => 256,
    }
}

impl Gen<'_> {
    pub fn val(&mut self, t: &FTy, path: &str) -> Val {
        match t {
            FTy::Int { name, signed, bits } => Val::Int(self.int(name, *signed, *bits, path)),
            FTy::Bool => Val::Bool(match self.mode {
                0 => false,
                1 => true,
                _ => self.rng.below(2) == 1,
            }),
            FTy::Float { bits } => Val::Float(self.float(*bits, path)),
            FTy::Str => Val::Str(self.string(path)),
            FTy::List(x)
                if matches!(
                    **x,
                    FTy::Int {
                        signed: false,
                        bits: 8,
                        ..
                    }
                ) && self
                    .hints
                    .like_bytes
                    .get(path)
                    .is_some_and(|e| !e.is_empty()) =>
            {
                Val::List(
                    self.bytes_like(path)
                        .into_iter()
                        .map(|b| Val::Int(b as i128))
                        .collect(),
                )
            }
            FTy::List(x) => {
                let n = self.len(path);
                let ep = format!("{path}[]");
                if n >= 8 && self.mode == 2 && self.rng.below(4) == 0 {
                    // one value repeated, with a few others among it: a search or a scan then
                    // runs far before it meets them
                    let fill = self.val(x, &ep);
                    let mut l = vec![fill; n];
                    for _ in 0..1 + self.rng.below(3) {
                        let at = self.rng.below(n as u64) as usize;
                        l[at] = self.val(x, &ep);
                    }
                    return Val::List(l);
                }
                Val::List((0..n).map(|_| self.val(x, &ep)).collect())
            }
            FTy::Array(x, n) => {
                let ep = format!("{path}[]");
                Val::List((0..*n).map(|_| self.val(x, &ep)).collect())
            }
            FTy::Opt(x) => {
                let none = match self.mode {
                    0 => true,
                    1 => false,
                    _ => self.rng.below(4) == 0,
                };
                if none {
                    Val::Opt(None)
                } else {
                    Val::Opt(Some(Box::new(self.val(x, path))))
                }
            }
            FTy::Res(a, b) => {
                if self.rng.below(4) == 0 {
                    Val::Res(Err(Box::new(self.val(b, path))))
                } else {
                    Val::Res(Ok(Box::new(self.val(a, path))))
                }
            }
            FTy::Tuple(v) => Val::Tuple(
                v.iter()
                    .enumerate()
                    .map(|(i, x)| self.val(x, &format!("{path}.{i}")))
                    .collect(),
            ),
            FTy::Boxed(_, x) => self.val(x, path),
            FTy::Struct { fields, .. } => Val::Struct(
                fields
                    .iter()
                    .map(|(n, x)| (n.clone(), self.val(x, &format!("{path}.{n}"))))
                    .collect(),
            ),
            FTy::Opaque(_) | FTy::Hidden(_) => Val::Opaque,
        }
    }

    /// Take `n` from the budget, as far as it goes.
    fn take(&mut self, n: usize) -> usize {
        let n = n.min(self.budget);
        self.budget -= n;
        n
    }

    /// The length of the list or string at `path`.
    fn len(&mut self, path: &str) -> usize {
        let key = format!("{path}#len");
        let n = self.len_unbudgeted(&key);
        self.take(n)
    }

    fn len_unbudgeted(&mut self, key: &str) -> usize {
        if let Some(&(lo, hi)) = self.hints.domains.get(key) {
            let (lo, hi) = (lo.max(0) as usize, hi.max(0) as usize);
            return match self.mode {
                0 => lo,
                1 => hi.min(MAX_LEN.max(lo)),
                _ => match self.rng.below(8) {
                    // an edge of the domain, or an edge size inside it
                    0 => [lo, hi, lo + 1, hi.saturating_sub(1)][self.rng.below(4) as usize]
                        .clamp(lo, hi),
                    1 | 2 => {
                        let e: Vec<usize> = EDGE_LENS
                            .iter()
                            .copied()
                            .filter(|n| (lo..=hi).contains(n))
                            .collect();
                        if e.is_empty() {
                            self.rng.within(lo as i128, hi as i128) as usize
                        } else {
                            e[self.rng.below(e.len() as u64) as usize]
                        }
                    }
                    3 => {
                        // short, as most inputs of a short domain
                        let top = hi.min(lo + 16);
                        self.rng.within(lo as i128, top as i128) as usize
                    }
                    _ => self.rng.within(lo as i128, hi as i128) as usize,
                },
            };
        }
        match self.mode {
            0 => 0,
            1 => self.max_len,
            _ => {
                if let Some(c) = self.hints.consts.get(key).filter(|c| !c.is_empty()) {
                    if self.rng.below(4) == 0 {
                        let k = c[self.rng.below(c.len() as u64) as usize];
                        let d = self.rng.below(3) as i128 - 1;
                        return (k + d).clamp(0, MAX_LEN as i128) as usize;
                    }
                }
                if self.max_len > 16 && self.rng.below(8) == 0 {
                    // an edge size, past the lengths of the first inputs
                    let e: Vec<usize> = EDGE_LENS
                        .iter()
                        .copied()
                        .filter(|&n| n <= MAX_LEN)
                        .collect();
                    return e[self.rng.below(e.len() as u64) as usize];
                }
                // short lists more often than long ones
                let cap = 1 + self.rng.below(self.max_len as u64 + 1);
                self.rng.below(cap + 1) as usize
            }
        }
    }

    /// A character of `set`, or the default mix when there is none.
    fn pick(&mut self, set: &[char]) -> char {
        set[self.rng.below(set.len() as u64) as usize]
    }

    /// A string at `path`: of a length in bytes (as `len()` counts), from the path's characters,
    /// or like one of its examples.
    fn string(&mut self, path: &str) -> String {
        let key = format!("{path}#len");
        let set: Vec<char> = match self.hints.chars.get(path) {
            Some(s) => s.clone(),
            None if self.mode == 2 && self.rng.below(2) == 0 => ascii(),
            None => crate::lang::DEFAULT_CHARS.to_vec(),
        };
        if let Some(ex) = self.hints.like.get(path).filter(|e| !e.is_empty()).cloned() {
            let s = self.like(&ex, &set);
            let s = match self.hints.domains.get(&key) {
                Some(&(lo, hi)) => fit(s, lo.max(0) as usize, hi.max(0) as usize, &set),
                None => s,
            };
            self.take(s.len());
            return s;
        }
        let n = self.len(path);
        let mut s = String::new();
        let small: Vec<char> = set.iter().copied().filter(|c| c.len_utf8() == 1).collect();
        while s.len() < n {
            let mut c = self.pick(&set);
            if c.len_utf8() > n - s.len() {
                if small.is_empty() {
                    break;
                }
                c = self.pick(&small);
            }
            s.push(c);
        }
        s
    }

    /// Bytes like one of the examples at `path`, changed as [`Gen::like`] changes a string, and
    /// fitted to the path's `len` part.
    fn bytes_like(&mut self, path: &str) -> Vec<u8> {
        let ex: Vec<String> = self.hints.like_bytes[path]
            .iter()
            .map(|b| b.iter().map(|&c| c as char).collect())
            .collect();
        let set: Vec<char> = (0u8..=255).map(|c| c as char).collect();
        let mut b: Vec<u8> = self
            .like(&ex, &set)
            .chars()
            .map(|c| c as u32 as u8)
            .collect();
        if let Some(&(lo, hi)) = self.hints.domains.get(&format!("{path}#len")) {
            let (lo, hi) = (lo.max(0) as usize, hi.max(0) as usize);
            b.truncate(hi);
            let fill = ex
                .iter()
                .find_map(|e| e.chars().next())
                .map_or(b'a', |c| c as u8);
            b.resize(b.len().max(lo), fill);
        }
        self.take(b.len());
        b
    }

    /// A string like one of the examples: the example itself, or the example with a few
    /// changes (a character removed, added or replaced, a part repeated, the end cut, or the
    /// end of another example).
    fn like(&mut self, ex: &[String], set: &[char]) -> String {
        match self.mode {
            0 => {
                return ex
                    .iter()
                    .min_by_key(|e| e.len())
                    .cloned()
                    .unwrap_or_default()
            }
            1 => {
                return ex
                    .iter()
                    .max_by_key(|e| e.len())
                    .cloned()
                    .unwrap_or_default()
            }
            _ => {}
        }
        let mut c: Vec<char> = ex[self.rng.below(ex.len() as u64) as usize]
            .chars()
            .collect();
        if self.rng.below(3) == 0 {
            return c.into_iter().collect();
        }
        // the characters a change adds: the examples' own, or the set's
        let own: Vec<char> = ex.iter().flat_map(|e| e.chars()).collect();
        for _ in 0..1 + self.rng.below(4) {
            let add = if own.is_empty() || self.rng.below(4) == 0 {
                self.pick(set)
            } else {
                self.pick(&own)
            };
            let at = self.rng.below(c.len() as u64 + 1) as usize;
            match self.rng.below(6) {
                0 if !c.is_empty() => {
                    c.remove(at.min(c.len() - 1));
                }
                1 => c.insert(at, add),
                2 if !c.is_empty() => {
                    let i = at.min(c.len() - 1);
                    c[i] = add;
                }
                3 if !c.is_empty() => {
                    let from = self.rng.below(c.len() as u64) as usize;
                    let to = from + 1 + self.rng.below((c.len() - from) as u64) as usize;
                    let part: Vec<char> = c[from..to].to_vec();
                    c.splice(at..at, part);
                }
                4 => c.truncate(at),
                _ => {
                    let other: Vec<char> = ex[self.rng.below(ex.len() as u64) as usize]
                        .chars()
                        .collect();
                    let from = self.rng.below(other.len() as u64 + 1) as usize;
                    c.truncate(at);
                    c.extend_from_slice(&other[from..]);
                }
            }
        }
        c.into_iter().collect()
    }

    /// A floating-point value: an edge (0, -0, 1, -1, the type's smallest and largest, the
    /// infinities, NaN, a clause's constant), a small integer, a value of the domain of an
    /// `in` part, or any bits (Milestone 289).
    fn float(&mut self, bits: u32, path: &str) -> u64 {
        let dom = self.hints.domains.get(path).copied();
        let (lo, hi) = match dom {
            Some((a, b)) => (a as f64, b as f64),
            None if bits == 32 => (f32::MIN as f64, f32::MAX as f64),
            None => (f64::MIN, f64::MAX),
        };
        let x = match self.mode {
            0 => 0f64.clamp(lo, hi),
            1 => hi,
            _ => {
                self.take(1);
                let r = self.rng.below(10);
                if r < 3 {
                    let tiny = if bits == 32 {
                        f32::MIN_POSITIVE as f64
                    } else {
                        f64::MIN_POSITIVE
                    };
                    let eps = if bits == 32 {
                        f32::EPSILON as f64
                    } else {
                        f64::EPSILON
                    };
                    let mut e = vec![0.0, -0.0, 1.0, -1.0, 0.5, -0.5, lo, hi, tiny, -tiny, eps];
                    if dom.is_none() {
                        e.extend([f64::INFINITY, f64::NEG_INFINITY, f64::NAN]);
                    }
                    if let Some(c) = self.hints.consts.get(path) {
                        for &k in c {
                            let k = k as f64;
                            e.extend([k, k - 0.5, k + 0.5, k - eps * k.abs(), k + eps * k.abs()]);
                        }
                    }
                    e.retain(|x| x.is_nan() || (lo..=hi).contains(x));
                    e[self.rng.below(e.len() as u64) as usize]
                } else if r < 5 {
                    // a small integer, or a half or a tenth of one
                    let n = self.rng.below(33) as f64 - 16.0;
                    let d = [1.0, 2.0, 10.0][self.rng.below(3) as usize];
                    (n / d).clamp(lo, hi)
                } else if r < 8 || dom.is_some() {
                    // uniform in the domain
                    let u = self.rng.word() as f64 / u64::MAX as f64;
                    let v = lo + (hi - lo) * u;
                    if v.is_finite() {
                        v
                    } else {
                        lo / 2.0 + hi / 2.0 * u
                    }
                } else if bits == 32 {
                    f32::from_bits(self.rng.word() as u32) as f64
                } else {
                    f64::from_bits(self.rng.word())
                }
            }
        };
        float_bits(x, bits)
    }

    fn int(&mut self, name: &str, signed: bool, bits: u32, path: &str) -> i128 {
        let (tlo, thi) = int_range(signed, bits);
        let (lo, hi) = match self.hints.domains.get(path) {
            Some(&(a, b)) => (a.max(tlo), b.min(thi)),
            // a size without an `input` line stays within SIZE_BOUND, its edges too
            None if matches!(name, "usize" | "isize") => {
                (tlo.max(-SIZE_BOUND), thi.min(SIZE_BOUND))
            }
            None => (tlo, thi),
        };
        match self.mode {
            0 => return 0i128.clamp(lo, hi),
            1 => return hi,
            _ => {}
        }
        self.take(1);
        let (rlo, rhi) = (lo, hi);
        let r = self.rng.below(10);
        if r < 3 {
            // an edge: a bound of the domain or the type, 0, 1, -1, or a clause's constant
            let mut e = vec![lo, hi, lo.saturating_add(1), hi.saturating_sub(1), 0, 1, -1];
            if let Some(c) = self.hints.consts.get(path) {
                for &k in c {
                    e.extend([k.saturating_sub(1), k, k.saturating_add(1)]);
                }
            }
            e.retain(|x| (lo..=hi).contains(x));
            if !e.is_empty() {
                return e[self.rng.below(e.len() as u64) as usize];
            }
        }
        if r < 6 {
            // a small value
            let base = 0i128.clamp(lo, hi);
            let v = base
                + self.rng.below(17) as i128
                    * if signed && self.rng.below(2) == 0 {
                        -1
                    } else {
                        1
                    };
            return v.clamp(lo, hi);
        }
        // a value of a random magnitude: each bit width as likely as the others
        let width = (rhi - rlo).max(0) as u128;
        let wbits = 128 - width.leading_zeros();
        let k = self.rng.below(wbits as u64 + 1) as u32;
        let m: u128 = if k >= 128 {
            u128::MAX
        } else {
            (1u128 << k).wrapping_sub(1)
        };
        let raw = ((self.rng.word() as u128) << 64 | self.rng.word() as u128) & m;
        let off = if width == u128::MAX {
            raw
        } else {
            raw % (width + 1)
        };
        if signed && rlo < 0 && rhi > 0 && self.rng.below(2) == 0 {
            // around 0 on both sides
            let o = (off as i128).min(rhi);
            return if self.rng.below(2) == 0 {
                o
            } else {
                (-o).max(rlo)
            };
        }
        rlo.saturating_add(off as i128).min(rhi)
    }
}

/// `s` cut to at most `hi` bytes at a character's boundary, and filled up to `lo` bytes with
/// one-byte characters of `set` (or `a`).
fn fit(mut s: String, lo: usize, hi: usize, set: &[char]) -> String {
    while s.len() > hi {
        s.pop();
    }
    let fill = set
        .iter()
        .copied()
        .find(|c| c.len_utf8() == 1)
        .unwrap_or('a');
    while s.len() < lo {
        s.push(fill);
    }
    s
}

/// The value at a path (`l.balance`, `p.0`), when the path names one.
pub fn at<'a>(v: &'a Val, path: &[&str]) -> Option<&'a Val> {
    let Some((first, rest)) = path.split_first() else {
        return Some(v);
    };
    match v {
        Val::Struct(f) => f
            .iter()
            .find(|(n, _)| n == first)
            .and_then(|(_, x)| at(x, rest)),
        Val::Tuple(t) => first
            .parse::<usize>()
            .ok()
            .and_then(|i| t.get(i))
            .and_then(|x| at(x, rest)),
        _ => None,
    }
}

/// The value at a path, to change.
pub fn at_mut<'a>(v: &'a mut Val, path: &[&str]) -> Option<&'a mut Val> {
    let Some((first, rest)) = path.split_first() else {
        return Some(v);
    };
    match v {
        Val::Struct(f) => f
            .iter_mut()
            .find(|(n, _)| n == first)
            .and_then(|(_, x)| at_mut(x, rest)),
        Val::Tuple(t) => first
            .parse::<usize>()
            .ok()
            .and_then(|i| t.get_mut(i))
            .and_then(|x| at_mut(x, rest)),
        _ => None,
    }
}

/// The type at a path.
pub fn ty_at<'a>(t: &'a FTy, path: &[&str]) -> Option<&'a FTy> {
    let Some((first, rest)) = path.split_first() else {
        return Some(t);
    };
    match t {
        FTy::Struct { fields, .. } => fields
            .iter()
            .find(|(n, _)| n == first)
            .and_then(|(_, x)| ty_at(x, rest)),
        FTy::Tuple(v) => first
            .parse::<usize>()
            .ok()
            .and_then(|i| v.get(i))
            .and_then(|x| ty_at(x, rest)),
        FTy::Boxed(_, x) => ty_at(x, path),
        _ => None,
    }
}

/// Each element of a list made simpler, one at a time: first every element's simplest
/// candidate, then the others, so that a long list's last elements shrink too.
fn elements_simpler(l: &[Val], x: &FTy, ep: &str, hints: &Hints, out: &mut Vec<Val>) {
    let per: Vec<Vec<Val>> = l
        .iter()
        .take(256)
        .map(|e| simpler(e, x, ep, hints))
        .collect();
    let most = per.iter().map(Vec::len).max().unwrap_or(0);
    for k in 0..most {
        for (i, c) in per.iter().enumerate() {
            if let Some(s) = c.get(k) {
                let mut d = l.to_vec();
                d[i] = s.clone();
                out.push(Val::List(d));
            }
        }
    }
}

/// Simpler values of `v` (one change each), for the shrink: each integer towards 0 (or the
/// domain's bound nearest to it), lists and strings shorter, an option `None`, a boolean
/// `false`. `path` is the value's path, for the domains.
pub fn simpler(v: &Val, t: &FTy, path: &str, hints: &Hints) -> Vec<Val> {
    let mut out = Vec::new();
    match (v, t) {
        (Val::Int(x), FTy::Int { signed, bits, .. }) => {
            let (tlo, thi) = int_range(*signed, *bits);
            let (lo, hi) = match hints.domains.get(path) {
                Some(&(a, b)) => (a.max(tlo), b.min(thi)),
                None => (tlo, thi),
            };
            let target = 0i128.clamp(lo, hi);
            if *x != target {
                // the target, then x moved towards it by half the distance, a quarter, ...,
                // and by 1: a value that must stay near another one still shrinks in few steps
                let mut d = *x - target;
                let mut cs = vec![target];
                while d != 0 {
                    d /= 2;
                    cs.push(*x - d);
                }
                cs.push(*x - (*x - target).signum());
                for c in cs {
                    if c != *x && (lo..=hi).contains(&c) && !out.contains(&Val::Int(c)) {
                        out.push(Val::Int(c));
                    }
                }
            }
        }
        (Val::Float(b), FTy::Float { bits }) => {
            // towards 0: 0, then the value without its fraction, then halves
            let x = f64::from_bits(*b);
            let (lo, hi) = match hints.domains.get(path) {
                Some(&(a, b)) => (a as f64, b as f64),
                None => (f64::MIN, f64::MAX),
            };
            let mut cs = vec![0f64.clamp(lo, hi)];
            if !x.is_finite() {
                cs.push(1.0);
            } else {
                cs.push(x.trunc());
                let mut d = x;
                for _ in 0..8 {
                    d /= 2.0;
                    cs.push(d);
                }
            }
            for c in cs {
                let k = float_bits(c, *bits);
                let c = Val::Float(k);
                if c != *v && (lo..=hi).contains(&f64::from_bits(k)) && !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        (Val::Bool(true), _) => out.push(Val::Bool(false)),
        (Val::Str(s), _) => {
            let c: Vec<char> = s.chars().collect();
            if !c.is_empty() {
                out.push(Val::Str(String::new()));
                if c.len() > 1 {
                    out.push(Val::Str(c[..c.len() / 2].iter().collect()));
                    out.push(Val::Str(c[c.len() / 2..].iter().collect()));
                }
                for i in 0..c.len().min(64) {
                    let mut d = c.clone();
                    d.remove(i);
                    out.push(Val::Str(d.into_iter().collect()));
                }
                for i in 0..c.len().min(64) {
                    if c[i] != 'a' {
                        let mut d = c.clone();
                        d[i] = 'a';
                        out.push(Val::Str(d.into_iter().collect()));
                    }
                }
            }
        }
        (Val::List(l), FTy::List(x)) => {
            if !l.is_empty() {
                out.push(Val::List(vec![]));
                if l.len() > 1 {
                    out.push(Val::List(l[..l.len() / 2].to_vec()));
                    out.push(Val::List(l[l.len() / 2..].to_vec()));
                }
                for i in 0..l.len().min(256) {
                    let mut d = l.clone();
                    d.remove(i);
                    out.push(Val::List(d));
                }
            }
            let ep = format!("{path}[]");
            elements_simpler(l, x, &ep, hints, &mut out);
        }
        (Val::List(l), FTy::Array(x, _)) => {
            let ep = format!("{path}[]");
            elements_simpler(l, x, &ep, hints, &mut out);
        }
        (Val::Opt(Some(x)), FTy::Opt(t)) => {
            out.push(Val::Opt(None));
            for s in simpler(x, t, path, hints) {
                out.push(Val::Opt(Some(Box::new(s))));
            }
        }
        (Val::Res(Ok(x)), FTy::Res(t, _)) => {
            for s in simpler(x, t, path, hints) {
                out.push(Val::Res(Ok(Box::new(s))));
            }
        }
        (Val::Res(Err(x)), FTy::Res(_, t)) => {
            for s in simpler(x, t, path, hints) {
                out.push(Val::Res(Err(Box::new(s))));
            }
        }
        (_, FTy::Boxed(_, t)) => return simpler(v, t, path, hints),
        (Val::Tuple(vs), FTy::Tuple(ts)) => {
            for (i, (x, t)) in vs.iter().zip(ts).enumerate() {
                for s in simpler(x, t, &format!("{path}.{i}"), hints) {
                    let mut d = vs.clone();
                    d[i] = s;
                    out.push(Val::Tuple(d));
                }
            }
        }
        (Val::Struct(fv), FTy::Struct { fields, .. }) => {
            for (i, ((n, x), (_, t))) in fv.iter().zip(fields).enumerate() {
                for s in simpler(x, t, &format!("{path}.{n}"), hints) {
                    let mut d = fv.clone();
                    d[i].1 = s;
                    out.push(Val::Struct(d));
                }
            }
        }
        _ => {}
    }
    out
}
