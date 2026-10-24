//! `#[driver(kind, ...)]` and `#[check(kind, ...)]` for hydo-test. Each keeps the function as it
//! is and registers it (inventory) under its name, so the test binary can list it in the plan
//! (`<bin> plan`) and run it in the guest (`<bin> driver <name>` / `<bin> check <name>`).
//!
//! The attribute is parsed by hand (no syn): `kind` is one identifier (`final` is a keyword in
//! Rust, which proc-macro tokens carry fine), then `key = literal` pairs.
use proc_macro::{Delimiter, TokenStream, TokenTree};
use std::str::FromStr;

const DRIVER_KINDS: &[(&str, &str)] = &[
    ("first", "first"),
    ("parallel", "parallel_driver"),
    ("serial", "serial_driver"),
    ("singleton", "singleton_driver"),
    ("anytime", "anytime"),
    ("eventually", "eventually"),
    ("finally", "finally"),
];
const CHECK_KINDS: &[(&str, &str)] = &[("final", "final"), ("always", "always"), ("eventually", "eventually")];

fn err(msg: &str) -> TokenStream {
    TokenStream::from_str(&format!("compile_error!({:?});", msg)).unwrap()
}

struct Args {
    kind: String,
    timeout: u32,
    every: u32,
    within: u32,
    poll: u32,
    after: String,
    msg: String,
    name: String,
}

fn parse_args(attr: TokenStream, role: &str, kinds: &[(&str, &str)]) -> Result<Args, String> {
    let toks: Vec<TokenTree> = attr.into_iter().collect();
    let mut a = Args { kind: String::new(), timeout: 0, every: 0, within: 0, poll: 0, after: String::new(), msg: String::new(), name: String::new() };
    let mut i;
    let names: Vec<&str> = kinds.iter().map(|k| k.0).collect();
    match toks.first() {
        Some(TokenTree::Ident(id)) => {
            let k = id.to_string();
            a.kind = kinds.iter().find(|x| x.0 == k).map(|x| x.1.to_string())
                .ok_or_else(|| format!("#[{}({})]: the kind is one of {}", role, k, names.join(", ")))?;
            i = 1;
        }
        _ => return Err(format!("#[{}(...)]: the first argument is the kind: {}", role, names.join(", "))),
    }
    while i < toks.len() {
        match &toks[i] {
            TokenTree::Punct(p) if p.as_char() == ',' => { i += 1; continue; }
            TokenTree::Ident(key) => {
                let key = key.to_string();
                let eq = matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '=');
                let val = match (eq, toks.get(i + 2)) {
                    (true, Some(TokenTree::Literal(l))) => l.to_string(),
                    _ => return Err(format!("#[{}]: write {} = <value>", role, key)),
                };
                let num = || val.parse::<u32>().map_err(|_| format!("#[{}]: {} is a whole number of seconds", role, key));
                let text = || -> Result<String, String> {
                    if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
                        Ok(val[1..val.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\"))
                    } else {
                        Err(format!("#[{}]: {} is a string", role, key))
                    }
                };
                match key.as_str() {
                    "timeout" => a.timeout = num()?,
                    "every" if role == "check" => a.every = num()?,
                    "within" if role == "check" => a.within = num()?,
                    "poll" if role == "check" => a.poll = num()?,
                    "after" if role == "check" => a.after = text()?,
                    "msg" => a.msg = text()?,
                    "name" => a.name = text()?,
                    _ => return Err(format!("#[{}]: unknown argument {} (driver: timeout, msg, name; check: also every, within, poll, after)", role, key)),
                }
                i += 3;
            }
            t => return Err(format!("#[{}]: unexpected {}", role, t)),
        }
    }
    Ok(a)
}

fn fn_name(item: &TokenStream) -> Option<String> {
    let mut it = item.clone().into_iter().peekable();
    while let Some(t) = it.next() {
        if let TokenTree::Ident(id) = &t {
            if id.to_string() == "fn" {
                if let Some(TokenTree::Ident(n)) = it.next() {
                    return Some(n.to_string());
                }
            }
        }
        if let TokenTree::Group(g) = &t {
            if g.delimiter() == Delimiter::Brace {
                return None;
            }
        }
    }
    None
}

fn register(role: &str, kinds: &[(&str, &str)], attr: TokenStream, item: TokenStream) -> TokenStream {
    let a = match parse_args(attr, role, kinds) {
        Ok(a) => a,
        Err(e) => return err(&e),
    };
    let f = match fn_name(&item) {
        Some(f) => f,
        None => return err(&format!("#[{}] goes on a function `fn name(ctx: &hydo_test::Ctx) -> hydo_test::Result<()>`", role)),
    };
    let name = if a.name.is_empty() { f.clone() } else { a.name.clone() };
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
        return err("hydo_test: a command or check name uses letters, digits, '.', '_' and '-'");
    }
    let reg = format!(
        "::hydo_test::inventory::submit! {{ ::hydo_test::Registration {{ role: {:?}, kind: {:?}, name: {:?}, \
         func: {} as fn(&::hydo_test::Ctx) -> ::hydo_test::Result<()>, timeout: {}, every: {}, within: {}, poll: {}, \
         after: {:?}, msg: {:?} }} }}",
        role, a.kind, name, f, a.timeout, a.every, a.within, a.poll, a.after, a.msg
    );
    let mut out = item;
    out.extend(TokenStream::from_str(&reg).unwrap());
    out
}

/// `#[driver(parallel)]`, `#[driver(serial, timeout = 120)]`, `#[driver(first)]`,
/// `#[driver(eventually)]` ... : a command of the test composer (hvx/onboard/MANIFEST.md).
#[proc_macro_attribute]
pub fn driver(attr: TokenStream, item: TokenStream) -> TokenStream {
    register("driver", DRIVER_KINDS, attr, item)
}

/// `#[check(final)]`, `#[check(always, every = 1)]`, `#[check(eventually, within = 20)]`:
/// a property (Ok = holds, Err(violation) = violated, Err(failed) = could not be evaluated).
#[proc_macro_attribute]
pub fn check(attr: TokenStream, item: TokenStream) -> TokenStream {
    register("check", CHECK_KINDS, attr, item)
}
