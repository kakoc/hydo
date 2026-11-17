//! The harness: a test module that hydo writes into a copy of the crate, right after the
//! function (or after the `impl` block of a method), so that it sees the function and its
//! types as the function's own module does. Its one test reads the cases of a file, calls the
//! function on each, and writes each answer: the result and the `&mut` parameters after the
//! call, or the panic's location and message. The module is one line, so every line of the file
//! keeps its number and a panic's location is the original file's.

use super::rust::{FTy, Pass, Receiver};
use std::fmt::Write as _;

/// A parameter as the harness passes it.
pub struct Param {
    pub name: String,
    pub pass: Pass,
    pub ty: FTy,
}

/// What the harness of one function needs.
pub struct Spec {
    /// The module's name, unique in the crate (`__hydo_fuzz_0`).
    pub module: String,
    /// The call's path: `super::apply`, or `<Ledger>::apply` for a method.
    pub call: String,
    pub receiver: Option<Receiver>,
    /// The parameters, the receiver first (as `self`).
    pub params: Vec<Param>,
    pub ret: FTy,
    /// Whether the result is a reference (the harness then encodes it as it is).
    pub ret_ref: bool,
    pub is_unsafe: bool,
    /// The lints that the crate forbids (`#![forbid(unsafe_code)]`): the module does not allow
    /// them, because an `allow` of a forbidden lint is an error (E0453, Milestone 289).
    pub forbidden: Vec<String>,
}

/// The lints the module allows, so that the crate's own `deny` settings do not stop it.
const ALLOWED: &[&str] = &[
    "warnings",
    "unused",
    "clippy::all",
    "clippy::pedantic",
    "clippy::nursery",
    "clippy::restriction",
];

/// The module's `allow` list: [`ALLOWED`] without the forbidden lints, and `unsafe_code` only
/// for an `unsafe` function, whose call the module makes in an `unsafe` block.
fn allow_list(s: &Spec) -> String {
    let mut l: Vec<&str> = ALLOWED.to_vec();
    if s.is_unsafe {
        l.insert(2, "unsafe_code");
    }
    l.retain(|x| !s.forbidden.iter().any(|f| f == x));
    l.join(", ")
}

/// The owned type as the harness writes it.
fn rty(t: &FTy) -> String {
    match t {
        FTy::Boxed(k, x) => {
            let p = match k.as_str() {
                "Rc" => "self::std::rc::Rc",
                "Arc" => "self::std::sync::Arc",
                _ => "self::std::boxed::Box",
            };
            format!("{p}<{}>", rty(x))
        }
        FTy::List(x) => format!("self::std::vec::Vec<{}>", rty(x)),
        FTy::Array(x, n) => format!("[{}; {n}]", rty(x)),
        FTy::Opt(x) => format!("self::std::option::Option<{}>", rty(x)),
        FTy::Res(a, b) => format!("self::std::result::Result<{}, {}>", rty(a), rty(b)),
        FTy::Tuple(v) if v.len() == 1 => format!("({},)", rty(&v[0])),
        FTy::Tuple(v) => format!("({})", v.iter().map(rty).collect::<Vec<_>>().join(", ")),
        FTy::Str => "self::std::string::String".into(),
        other => other.rust(),
    }
}

/// An expression that reads a value of type `t` from the reader `r`.
fn dec(t: &FTy) -> String {
    match t {
        FTy::Int { .. } => "r.int()".into(),
        FTy::Bool => "r.b()".into(),
        FTy::Float { bits: 32 } => "(r.f() as f32)".into(),
        FTy::Float { .. } => "r.f()".into(),
        FTy::Str => "r.s()".into(),
        FTy::List(x) => format!(
            "{{ let n = r.n(); let mut v = self::std::vec::Vec::with_capacity(n); for _ in 0..n {{ v.push({}); }} v }}",
            dec(x)
        ),
        FTy::Array(x, _) => format!(
            "{{ r.n(); self::std::array::from_fn(|_| {}) }}",
            dec(x)
        ),
        FTy::Opt(x) => format!(
            "if r.w() == \"S\" {{ self::std::option::Option::Some({}) }} else {{ self::std::option::Option::None }}",
            dec(x)
        ),
        FTy::Res(a, b) => format!(
            "if r.w() == \"O\" {{ self::std::result::Result::Ok({}) }} else {{ self::std::result::Result::Err({}) }}",
            dec(a),
            dec(b)
        ),
        FTy::Tuple(v) => {
            let p: Vec<String> = v.iter().map(dec).collect();
            if p.len() == 1 {
                format!("({},)", p[0])
            } else {
                format!("({})", p.join(", "))
            }
        }
        FTy::Boxed(_, x) => {
            let p = rty(t);
            let base = p.split('<').next().unwrap_or("self::std::boxed::Box");
            format!("{base}::new({})", dec(x))
        }
        FTy::Struct {
            path,
            tuple,
            fields,
        } => {
            if *tuple {
                let p: Vec<String> = fields.iter().map(|(_, x)| dec(x)).collect();
                if p.is_empty() {
                    path.clone()
                } else {
                    format!("{path}({})", p.join(", "))
                }
            } else {
                let p: Vec<String> = fields
                    .iter()
                    .map(|(n, x)| format!("{n}: {}", dec(x)))
                    .collect();
                format!("{path} {{ {} }}", p.join(", "))
            }
        }
        FTy::Opaque(_) | FTy::Hidden(_) => "unreachable!()".into(),
    }
}

/// Statements that write the value `v` (an expression of a reference to a value of type `t`)
/// to the answer `o`. `d` keeps the bindings of nested values apart.
fn enc(t: &FTy, v: &str, d: usize) -> String {
    let x = format!("x{d}");
    match t {
        FTy::Int { .. } => format!("ei({v}, o);"),
        FTy::Bool => format!("eb({v}, o);"),
        FTy::Float { .. } => {
            format!("ef(<f64 as self::std::convert::From<_>>::from(*{v}), o);")
        }
        FTy::Str => format!("es({v}, o);"),
        FTy::List(e) | FTy::Array(e, _) => format!(
            "{{ let _ = self::std::write!(o, \"L{{}} \", ({v}).len()); for {x} in ({v}).iter() {{ {} }} }}",
            enc(e, &x, d + 1)
        ),
        FTy::Opt(e) => format!(
            "match {v} {{ self::std::option::Option::None => o.push_str(\"N \"), self::std::option::Option::Some({x}) => {{ o.push_str(\"S \"); {} }} }}",
            enc(e, &x, d + 1)
        ),
        FTy::Res(a, b) => format!(
            "match {v} {{ self::std::result::Result::Ok({x}) => {{ o.push_str(\"O \"); {} }} self::std::result::Result::Err({x}) => {{ o.push_str(\"E \"); {} }} }}",
            enc(a, &x, d + 1),
            enc(b, &x, d + 1)
        ),
        FTy::Tuple(ts) => ts
            .iter()
            .enumerate()
            .map(|(i, e)| enc(e, &format!("&({v}).{i}"), d + 1))
            .collect::<Vec<_>>()
            .join(" "),
        FTy::Boxed(k, e) => {
            let f = match k.as_str() {
                "Rc" => "unrc",
                "Arc" => "unarc",
                _ => "unbox",
            };
            format!("{{ let {x} = {f}({v}); {} }}", enc(e, &x, d + 1))
        }
        // a field not read is not named: the harness's module may not see it
        FTy::Struct { fields, .. } => fields
            .iter()
            .map(|(n, e)| match e {
                FTy::Opaque(_) | FTy::Hidden(_) => "o.push_str(\"X \");".to_string(),
                _ => enc(e, &format!("&({v}).{n}"), d + 1),
            })
            .collect::<Vec<_>>()
            .join(" "),
        FTy::Opaque(_) | FTy::Hidden(_) => format!("{{ let _ = {v}; o.push_str(\"X \"); }}"),
    }
}

/// The shared part of every harness module: the reader, the leaf writers, the panic hook.
const COMMON: &str = concat!(
    // `extern crate std` and `self::std` paths: a `#![no_std]` crate has no `std` and the core
    // prelude only, and a crate of the 2015 edition reads `use` paths from its root
    "extern crate std; use super::*; use self::std::fmt::Write as _; ",
    "struct R<'a> { w: self::std::str::SplitAsciiWhitespace<'a> } ",
    "impl<'a> R<'a> { ",
    "fn w(&mut self) -> &'a str { self.w.next().unwrap_or(\"\") } ",
    "fn int<T: self::std::convert::TryFrom<i128>>(&mut self) -> T { match self.w().parse::<i128>().ok().and_then(|x| T::try_from(x).ok()) { self::std::option::Option::Some(v) => v, self::std::option::Option::None => self::std::process::abort() } } ",
    "fn b(&mut self) -> bool { self.w() == \"t\" } ",
    "fn f(&mut self) -> f64 { f64::from_bits(u64::from_str_radix(self.w().get(1..).unwrap_or(\"\"), 16).unwrap_or(0)) } ",
    "fn n(&mut self) -> usize { self.w().trim_start_matches('L').parse().unwrap_or(0) } ",
    "fn s(&mut self) -> self::std::string::String { let h = self.w(); let h = h.get(1..).unwrap_or(\"\"); let b: self::std::vec::Vec<u8> = (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap_or(0)).collect(); self::std::string::String::from_utf8(b).unwrap_or_default() } ",
    "} ",
    "fn ei<T: self::std::fmt::Display>(v: T, o: &mut self::std::string::String) { let _ = self::std::write!(o, \"{} \", v); } ",
    "fn ef(v: f64, o: &mut self::std::string::String) { let _ = self::std::write!(o, \"F{:x} \", v.to_bits()); } ",
    "fn eb(v: &bool, o: &mut self::std::string::String) { o.push_str(if *v { \"t \" } else { \"f \" }); } ",
    "fn es(v: &str, o: &mut self::std::string::String) { o.push('s'); for b in v.as_bytes() { let _ = self::std::write!(o, \"{:02x}\", b); } o.push(' '); } ",
    "fn hx(v: &str, o: &mut self::std::string::String) { o.push('s'); for b in v.as_bytes() { let _ = self::std::write!(o, \"{:02x}\", b); } o.push(' '); } ",
    "fn unbox<T: ?Sized>(b: &self::std::boxed::Box<T>) -> &T { b } ",
    "fn unrc<T: ?Sized>(b: &self::std::rc::Rc<T>) -> &T { b } ",
    "fn unarc<T: ?Sized>(b: &self::std::sync::Arc<T>) -> &T { b } ",
    "const EVENT: &[u8] = b\"HYDO-ASSERT {\\\"t\\\":\\\"reachable\\\",\\\"id\\\":\\\"fuzz.call\\\",\\\"ok\\\":true,\\\"msg\\\":\\\"a call of hydo fuzz --vm started\\\"}\\n\"; ",
    "static LAST: self::std::sync::Mutex<self::std::option::Option<(self::std::string::String, self::std::string::String)>> = self::std::sync::Mutex::new(self::std::option::Option::None); ",
    "#[test] fn run() { ",
    "let cases = self::std::env::var(\"HYDO_FUZZ_CASES\").unwrap_or_default(); if cases.is_empty() { return; } ",
    "let text = self::std::fs::read_to_string(&cases).unwrap_or_default(); ",
    "let mut out = self::std::fs::OpenOptions::new().create(true).append(true).open(self::std::env::var(\"HYDO_FUZZ_OUT\").unwrap_or_default()).expect(\"HYDO_FUZZ_OUT\"); ",
    "self::std::panic::set_hook(self::std::boxed::Box::new(|i| { let mut loc = i.location().map(|l| self::std::format!(\"{}:{}\", l.file(), l.line())).unwrap_or_default(); ",
    // a panic inside the standard library or a dependency is placed at the crate's own frame
    "fn foreign(p: &str) -> bool { p.starts_with(\"/rustc/\") || p.contains(\"/.cargo/\") || p.contains(\"/rustlib/\") } ",
    "if foreign(&loc) { let bt = self::std::backtrace::Backtrace::force_capture(); let bt = self::std::string::ToString::to_string(&bt); ",
    "if let self::std::option::Option::Some(at) = bt.lines().filter_map(|l| l.trim().strip_prefix(\"at \")).find(|p| !foreign(p)) { ",
    "let at = at.trim_start_matches(\"./\"); let mut parts = at.rsplitn(3, ':'); let _ = parts.next(); let line = parts.next().unwrap_or(\"\"); let file = parts.next().unwrap_or(at); loc = self::std::format!(\"{}:{}\", file, line); } } ",
    "let p = i.payload(); let msg = if let self::std::option::Option::Some(s) = p.downcast_ref::<&str>() { self::std::string::ToString::to_string(*s) } else if let self::std::option::Option::Some(s) = p.downcast_ref::<self::std::string::String>() { s.clone() } else { self::std::string::String::new() }; ",
    "let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner()); if g.is_none() { *g = self::std::option::Option::Some((loc, msg)); } })); ",
    // Milestone 296: natively each call works in a folder of its own (HYDO_FUZZ_DIRS/<k>); in the
    // VM, the call that the schedule names (HYDO_FUZZ_EVENT) names its thread hydo-call, which the
    // threads it starts inherit, and reports its start on the console for a reaction (A=fuzz.call)
    "let dirs = self::std::env::var(\"HYDO_FUZZ_DIRS\").unwrap_or_default(); let event = !self::std::env::var(\"HYDO_FUZZ_EVENT\").unwrap_or_default().is_empty(); ",
    "for line in text.lines() { let (k, rest) = line.split_once(' ').unwrap_or((line, \"\")); let mut o = self::std::string::String::new(); o.push_str(k); o.push(' '); ",
    "if !dirs.is_empty() { let d = self::std::format!(\"{}/{}\", dirs, k); let _ = self::std::fs::create_dir_all(&d); let _ = self::std::env::set_current_dir(&d); } ",
    "if event { let _ = self::std::fs::write(\"/proc/thread-self/comm\", \"hydo-call\"); if let self::std::result::Result::Ok(mut c) = self::std::fs::OpenOptions::new().write(true).open(\"/dev/console\") { use self::std::io::Write as _; let _ = c.write_all(EVENT); } } ",
    "one(rest, &mut o); o.push('\\n'); ",
    "use self::std::io::Write as _; let _ = out.write_all(o.as_bytes()); let _ = out.flush(); } } ",
);

/// A one-line module that only names the function, so that it is in use in the copy's tests
/// when the run fuzzes other functions (Milestone 289): a property function that only hydo
/// calls is otherwise dead code, which a crate that denies warnings in its tests refuses.
pub fn keep(s: &Spec) -> String {
    let b = format!(
        " #[cfg(test)] #[allow({})] mod {} {{ use super::*; const _: () = {{ let _ = {}; }}; }}",
        allow_list(s),
        s.module,
        s.call
    );
    debug_assert!(!b.contains('\n'));
    b
}

/// The module's text: one line, with no line comment in it.
pub fn module(s: &Spec) -> String {
    let mut b = String::new();
    let _ = write!(
        b,
        " #[cfg(test)] #[allow({})] mod {} {{ {COMMON}",
        allow_list(s),
        s.module
    );
    b.push_str("fn one(line: &str, o: &mut self::std::string::String) { let mut r = R { w: line.split_ascii_whitespace() }; ");
    let mut args = Vec::new();
    for (i, p) in s.params.iter().enumerate() {
        let _ = write!(b, "let mut a{i}: {} = {}; ", rty(&p.ty), dec(&p.ty));
        args.push(match p.pass {
            Pass::Value => format!("a{i}"),
            Pass::Ref { unsized_ref: false } => format!("&a{i}"),
            Pass::Ref { unsized_ref: true } => format!("&a{i}[..]"),
            Pass::RefMut { unsized_ref: false } => format!("&mut a{i}"),
            Pass::RefMut { unsized_ref: true } => format!("&mut a{i}[..]"),
        });
    }
    let call = format!("{}({})", s.call, args.join(", "));
    let call = if s.is_unsafe {
        format!("unsafe {{ {call} }}")
    } else {
        call
    };
    b.push_str(
        "*LAST.lock().unwrap_or_else(|e| e.into_inner()) = self::std::option::Option::None; ",
    );
    let _ = write!(
        b,
        "let res = self::std::panic::catch_unwind(self::std::panic::AssertUnwindSafe(|| {call})); "
    );
    b.push_str("match res { self::std::result::Result::Ok(v) => { o.push_str(\"R \"); ");
    let v = if s.ret_ref { "v" } else { "&v" };
    b.push_str(&enc(&s.ret, v, 0));
    b.push_str(" let _ = v; ");
    for (i, p) in s.params.iter().enumerate() {
        if matches!(p.pass, Pass::RefMut { .. }) {
            b.push(' ');
            b.push_str(&enc(&p.ty, &format!("&a{i}"), 0));
        }
    }
    b.push_str(" } self::std::result::Result::Err(_) => { let (loc, msg) = LAST.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default(); o.push_str(\"P \"); hx(&loc, o); hx(&msg, o); } } } }");
    debug_assert!(!b.contains('\n'));
    b
}
