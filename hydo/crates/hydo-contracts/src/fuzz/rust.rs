//! Reading Rust sources for function fuzzing: where a function's body and its enclosing blocks
//! are, its signature, the structures and type aliases of the crate, and the types of its
//! parameters and result as a fuzzing type ([`FTy`]). The reading is textual (no compiler): the
//! comments and the contents of literals are masked, then braces, parentheses and angle brackets
//! are matched.

use std::collections::BTreeMap;

/// `text` with every comment and the contents of every string and character literal replaced by
/// spaces. Newlines stay, so byte offsets and line numbers are the same as in `text`.
pub fn mask(text: &str) -> Vec<u8> {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in out.iter_mut().take(to).skip(from) {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            let e = b[i..]
                .iter()
                .position(|&x| x == b'\n')
                .map_or(b.len(), |p| i + p);
            blank(&mut out, i, e);
            i = e;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let (mut d, mut j) = (1, i + 2);
            while j < b.len() && d > 0 {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                    d += 1;
                    j += 2;
                } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                    d -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if (c == b'r' || (c == b'b' && b.get(i + 1) == Some(&b'r')))
            && (i == 0 || !ident(b[i - 1]))
            && {
                let k = if c == b'b' { i + 2 } else { i + 1 };
                let h = b[k..].iter().take_while(|&&x| x == b'#').count();
                b.get(k + h) == Some(&b'"')
            }
        {
            // a raw string: r"..", r#".."#, br".."
            let k = if c == b'b' { i + 2 } else { i + 1 };
            let h = b[k..].iter().take_while(|&&x| x == b'#').count();
            let start = k + h + 1;
            let mut j = start;
            let end = loop {
                if j >= b.len() {
                    break b.len();
                }
                if b[j] == b'"' && b[j + 1..].iter().take(h).filter(|&&x| x == b'#').count() == h {
                    break j;
                }
                j += 1;
            };
            blank(&mut out, start, end);
            i = (end + 1 + h).min(b.len());
        } else if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            blank(&mut out, i + 1, j.min(b.len()));
            i = j + 1;
        } else if c == b'\'' {
            // a character literal ('x', '\n', '\u{..}', 'é') or a lifetime ('a)
            if b.get(i + 1) == Some(&b'\\') {
                let mut j = i + 2;
                while j < b.len() && b[j] != b'\'' {
                    j += 1;
                }
                blank(&mut out, i + 1, j);
                i = j + 1;
            } else {
                let ch_len = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                if b.get(i + 1 + ch_len) == Some(&b'\'') {
                    blank(&mut out, i + 1, i + 1 + ch_len);
                    i += 2 + ch_len;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The byte offset where line `line` (1-based) starts.
pub fn line_start(text: &str, line: u32) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut n = 1;
    for (i, c) in text.bytes().enumerate() {
        if c == b'\n' {
            n += 1;
            if n == line {
                return i + 1;
            }
        }
    }
    text.len()
}

/// The offset of the `}` that closes the `{` at `open` (masked text).
pub fn close_of(m: &[u8], open: usize) -> Option<usize> {
    let mut d = 0usize;
    for (i, &c) in m.iter().enumerate().skip(open) {
        match c {
            b'{' => d += 1,
            b'}' => {
                d -= 1;
                if d == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A block that encloses a position: its header (the text from the previous item boundary to
/// its `{`) and the offsets of its braces.
#[derive(Clone, Debug)]
pub struct Block {
    pub header: String,
    pub open: usize,
    pub close: usize,
}

/// The blocks that enclose `pos`, outermost first.
pub fn enclosing(m: &[u8], pos: usize) -> Vec<Block> {
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (header start, open)
    let mut item_start = 0;
    let mut i = 0;
    while i < pos.min(m.len()) {
        match m[i] {
            b'{' => {
                stack.push((item_start, i));
                item_start = i + 1;
            }
            b'}' => {
                stack.pop();
                item_start = i + 1;
            }
            b';' => item_start = i + 1,
            _ => {}
        }
        i += 1;
    }
    stack
        .into_iter()
        .filter_map(|(h, o)| {
            let header = String::from_utf8_lossy(&m[h..o]).trim().to_string();
            Some(Block {
                header: strip_attrs(&header),
                open: o,
                close: close_of(m, o)?,
            })
        })
        .collect()
}

/// A header without its leading attributes (`#[...]`).
fn strip_attrs(h: &str) -> String {
    let mut s = h.trim_start();
    while s.starts_with("#[") || s.starts_with("#![") {
        let mut d = 0;
        let mut end = s.len();
        for (i, c) in s.char_indices() {
            match c {
                '[' => d += 1,
                ']' => {
                    d -= 1;
                    if d == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        s = s[end..].trim_start();
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A function's signature as read from its declaration.
#[derive(Clone, Debug)]
pub struct Sig {
    pub name: String,
    pub generics: Vec<String>,
    pub is_async: bool,
    pub is_unsafe: bool,
    /// `self`, `&self`, `&mut self` (or none).
    pub receiver: Option<Receiver>,
    pub params: Vec<(String, TyAst)>,
    pub ret: Option<TyAst>,
    /// The `{` of the body.
    pub body_open: usize,
    /// Whether the function has a body (a trait's required method has none; `body_open` is
    /// then its `;`).
    pub has_body: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receiver {
    Value,
    Ref,
    RefMut,
}

/// Read the signature of the function declared at `decl` (the declaration line's offset).
pub fn signature(text: &str, m: &[u8], decl: usize) -> Result<Sig, String> {
    // the body's `{`: the first `{` outside parentheses and brackets
    let (mut p, mut j) = (0i32, decl);
    let body = loop {
        let Some(&c) = m.get(j) else {
            return Err("the function has no body".into());
        };
        match c {
            b'(' | b'[' => p += 1,
            b')' | b']' => p -= 1,
            b'{' if p == 0 => break j,
            // a declaration without a body: a trait's required method (Milestone 307)
            b';' if p == 0 => break j,
            _ => {}
        }
        j += 1;
    };
    let head = String::from_utf8_lossy(&m[decl..body]).to_string();
    let head = head.split_whitespace().collect::<Vec<_>>().join(" ");
    let fn_at = find_word(&head, "fn").ok_or("no `fn` in the declaration")?;
    let quals = &head[..fn_at];
    let rest = head[fn_at + 2..].trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let mut rest = rest[name.len()..].trim_start();
    let mut generics = Vec::new();
    if rest.starts_with('<') {
        let e = matching(rest, 0, '<', '>').ok_or("unbalanced `<` in the generics")?;
        for g in split_top(&rest[1..e], ',') {
            let g = g.trim();
            if !g.is_empty() && !g.starts_with('\'') {
                generics.push(g.to_string());
            }
        }
        rest = rest[e + 1..].trim_start();
    }
    if !rest.starts_with('(') {
        return Err("no parameter list".into());
    }
    let e = matching(rest, 0, '(', ')').ok_or("unbalanced `(`")?;
    let plist = &rest[1..e];
    let after = rest[e + 1..].trim();
    let ret_text = after.strip_prefix("->").map(|r| {
        let r = r.trim();
        match find_word(r, "where") {
            Some(w) => r[..w].trim().to_string(),
            None => r.to_string(),
        }
    });
    if after.contains("where") {
        generics.push("where".into());
    }
    let mut receiver = None;
    let mut params = Vec::new();
    for (k, p) in split_top(plist, ',').into_iter().enumerate() {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let p = strip_attrs(p);
        if k == 0 {
            let r = p.replace(' ', "");
            let rv = match r.as_str() {
                "self" | "mutself" => Some(Receiver::Value),
                "&self" | "self:&Self" => Some(Receiver::Ref),
                "&mutself" | "self:&mutSelf" => Some(Receiver::RefMut),
                _ if r.starts_with("&'") && r.ends_with("self") => Some(if r.contains("mut") {
                    Receiver::RefMut
                } else {
                    Receiver::Ref
                }),
                _ => None,
            };
            if rv.is_some() {
                receiver = rv;
                continue;
            }
        }
        let Some(colon) = top_colon(&p) else {
            return Err(format!("the parameter `{p}` has no type"));
        };
        let pat = p[..colon].trim().trim_start_matches("mut ").trim();
        if !pat.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(format!(
                "the parameter `{pat}` is a pattern; only named parameters are fuzzed"
            ));
        }
        params.push((pat.to_string(), parse_ty(p[colon + 1..].trim())?));
    }
    let _ = text;
    Ok(Sig {
        name,
        generics,
        is_async: find_word(quals, "async").is_some(),
        is_unsafe: find_word(quals, "unsafe").is_some(),
        receiver,
        params,
        ret: match ret_text {
            Some(t) if !t.is_empty() => Some(parse_ty(&t)?),
            _ => None,
        },
        body_open: body,
        has_body: m[body] == b'{',
    })
}

/// The offset of `w` as a whole word in `s`.
fn find_word(s: &str, w: &str) -> Option<usize> {
    let b = s.as_bytes();
    let id = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut from = 0;
    while let Some(p) = s[from..].find(w) {
        let at = from + p;
        let before = at == 0 || !id(b[at - 1]);
        let after = b.get(at + w.len()).is_none_or(|&c| !id(c));
        if before && after {
            return Some(at);
        }
        from = at + w.len();
    }
    None
}

/// The offset of the bracket that closes the one at `at`.
fn matching(s: &str, at: usize, open: char, close: char) -> Option<usize> {
    let mut d = 0;
    let b = s.as_bytes();
    for (i, c) in s.char_indices().skip(at) {
        // `->` is not a closing angle bracket
        if c == '>' && i > 0 && b[i - 1] == b'-' {
            continue;
        }
        if c == open {
            d += 1;
        } else if c == close {
            d -= 1;
            if d == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// `s` split at `sep` outside brackets of every kind.
pub fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut d = 0i32;
    let mut cur = String::new();
    let b = s.as_bytes();
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => d += 1,
            ')' | ']' | '}' => d -= 1,
            '>' if !(i > 0 && b[i - 1] == b'-') => d -= 1,
            _ => {}
        }
        if c == sep && d == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// The first `:` outside brackets that is not part of `::`.
fn top_colon(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut d = 0i32;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'<' => d += 1,
            b')' | b']' | b'>' => d -= 1,
            b':' if d == 0 => {
                let dbl = b.get(i + 1) == Some(&b':') || (i > 0 && b[i - 1] == b':');
                if !dbl {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A type as written.
#[derive(Clone, Debug, PartialEq)]
pub enum TyAst {
    /// `a::b::C<T, U>`: the segments and the last segment's type arguments.
    Path(Vec<String>, Vec<TyAst>),
    Ref(bool, Box<TyAst>),
    Slice(Box<TyAst>),
    Array(Box<TyAst>, String),
    Tuple(Vec<TyAst>),
    /// Anything else (`impl Trait`, `dyn Trait`, a raw pointer, a function pointer).
    Other(String),
}

impl TyAst {
    /// The type written back as text.
    pub fn text(&self) -> String {
        match self {
            TyAst::Path(s, a) if a.is_empty() => s.join("::"),
            TyAst::Path(s, a) => format!(
                "{}<{}>",
                s.join("::"),
                a.iter().map(TyAst::text).collect::<Vec<_>>().join(", ")
            ),
            TyAst::Ref(m, x) => format!("&{}{}", if *m { "mut " } else { "" }, x.text()),
            TyAst::Slice(x) => format!("[{}]", x.text()),
            TyAst::Array(x, n) => format!("[{}; {n}]", x.text()),
            TyAst::Tuple(v) if v.len() == 1 => format!("({},)", v[0].text()),
            TyAst::Tuple(v) => format!(
                "({})",
                v.iter().map(TyAst::text).collect::<Vec<_>>().join(", ")
            ),
            TyAst::Other(s) => s.clone(),
        }
    }
}

/// Parse a type's text.
pub fn parse_ty(s: &str) -> Result<TyAst, String> {
    let s = s.trim();
    if let Some(r) = s.strip_prefix('&') {
        let mut r = r.trim_start();
        if r.starts_with('\'') {
            let n = r[1..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .map_or(r.len(), |p| p + 1);
            r = r[n..].trim_start();
        }
        let (m, r) = match r.strip_prefix("mut ") {
            Some(x) => (true, x),
            None => (false, r),
        };
        return Ok(TyAst::Ref(m, Box::new(parse_ty(r)?)));
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        let parts = split_top(inner, ';');
        return Ok(match parts.as_slice() {
            [t] => TyAst::Slice(Box::new(parse_ty(t)?)),
            [t, n] => TyAst::Array(Box::new(parse_ty(t)?), n.trim().to_string()),
            _ => TyAst::Other(s.into()),
        });
    }
    if s.starts_with('(') && s.ends_with(')') {
        let inner = s[1..s.len() - 1].trim();
        if inner.is_empty() {
            return Ok(TyAst::Tuple(vec![]));
        }
        let parts = split_top(inner, ',');
        let trailing = inner.ends_with(',');
        let parts: Vec<&String> = parts.iter().filter(|p| !p.trim().is_empty()).collect();
        if parts.len() == 1 && !trailing {
            return parse_ty(parts[0]);
        }
        return Ok(TyAst::Tuple(
            parts
                .into_iter()
                .map(|p| parse_ty(p))
                .collect::<Result<_, _>>()?,
        ));
    }
    let first = s.split_whitespace().next().unwrap_or("");
    if matches!(first, "impl" | "dyn") || s.starts_with('*') || s.starts_with("fn(") {
        return Ok(TyAst::Other(s.into()));
    }
    // a path, with type arguments on its last segment
    let (path, args) = match s.find('<') {
        Some(lt) if s.ends_with('>') => (&s[..lt], Some(&s[lt + 1..s.len() - 1])),
        Some(_) => return Ok(TyAst::Other(s.into())),
        None => (s, None),
    };
    let path = path.trim().trim_start_matches("::");
    if path.is_empty()
        || !path
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
    {
        return Ok(TyAst::Other(s.into()));
    }
    let segs = path.split("::").map(str::to_string).collect();
    let mut targs = Vec::new();
    if let Some(a) = args {
        for t in split_top(a, ',') {
            let t = t.trim();
            if t.is_empty() || t.starts_with('\'') {
                continue;
            }
            if top_eq(t) {
                targs.push(TyAst::Other(t.into()));
            } else {
                targs.push(parse_ty(t)?);
            }
        }
    }
    Ok(TyAst::Path(segs, targs))
}

/// Whether `t` is an associated-type binding (`Item = u8`).
fn top_eq(t: &str) -> bool {
    split_top(t, '=').len() > 1
}

/// A structure of the crate.
#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub generic: bool,
    pub tuple: bool,
    pub fields: Vec<(String, TyAst)>,
    /// Whether each field is declared with a `pub` visibility (`pub`, `pub(crate)`, ...).
    pub public: Vec<bool>,
    /// The module that defines the structure (`["identifier"]`; empty for the crate's root).
    pub module: Vec<String>,
}

/// How a variant of an enumeration is written: `A`, `A(u8, u8)` or `A { x: u8 }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VKind {
    Unit,
    Tuple,
    Named,
}

/// A variant of an enumeration of the crate, as written.
#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    pub kind: VKind,
    /// The fields: `0`, `1`, ... for a tuple variant.
    pub fields: Vec<(String, TyAst)>,
}

/// An enumeration of the crate (Milestone 307).
#[derive(Clone, Debug)]
pub struct EnumDef {
    pub name: String,
    /// The type parameters (`T` of `Status<T>`), without lifetimes.
    pub params: Vec<String>,
    pub variants: Vec<VariantDef>,
    pub module: Vec<String>,
}

/// A `build` line (Milestone 307): the constructor's path and its arguments.
#[derive(Clone, Debug)]
pub struct BuildDef {
    pub ctor: String,
    pub args: Vec<(String, TyAst)>,
}

/// The structures and type aliases of a crate's sources.
#[derive(Clone, Debug, Default)]
pub struct Items {
    pub structs: BTreeMap<String, StructDef>,
    pub aliases: BTreeMap<String, TyAst>,
    /// `const N: usize = 64;`: the crate's integer constants, for array lengths.
    pub consts: BTreeMap<String, usize>,
    /// The enumerations, by name (Milestone 307).
    pub enums: BTreeMap<String, EnumDef>,
    /// The type aliases with type parameters (`type Result<T> = ...`), by name: the parameters
    /// and the type (Milestone 307).
    pub generic_aliases: BTreeMap<String, (Vec<String>, TyAst)>,
    /// The `build` lines of the crate's contracts, by the type's name (Milestone 307).
    pub builds: BTreeMap<String, BuildDef>,
    /// The `read` lines of the crate's contracts, by the type's name: the type to read a value
    /// as, and the method that gives it (Milestone 307).
    pub reads: BTreeMap<String, (TyAst, String)>,
}

/// The name a `build` or `read` line keys its type by: the last segment of its path, without
/// type arguments (`Bytes` for `bytes::Bytes`).
pub fn type_key(t: &str) -> String {
    let t = t.split('<').next().unwrap_or(t).trim();
    t.rsplit("::").next().unwrap_or(t).trim().to_string()
}

/// The names of the type parameters in a generics list's inside (`T: Copy, 'a, U`), lifetimes
/// left out; a const parameter keeps its `const`.
pub fn generic_names(inner: &str) -> Vec<String> {
    split_top(inner, ',')
        .iter()
        .map(|g| g.trim())
        .filter(|g| !g.is_empty() && !g.starts_with('\''))
        .map(|g| {
            if g.starts_with("const ") {
                return g.split(':').next().unwrap_or(g).trim().to_string();
            }
            g.split([':', '=']).next().unwrap_or(g).trim().to_string()
        })
        .collect()
}

/// The fields of a structure's or a variant's body, with whether each is `pub`; `None` when a
/// field does not parse.
#[allow(clippy::type_complexity)]
fn parse_fields(body: &str, tuple: bool) -> Option<(Vec<(String, TyAst)>, Vec<bool>)> {
    let mut fields = Vec::new();
    let mut public = Vec::new();
    for (k, f) in split_top(body, ',').into_iter().enumerate() {
        let f = strip_attrs(f.trim());
        if f.is_empty() {
            continue;
        }
        let is_pub = (f.starts_with("pub ") || f.starts_with("pub("))
            && !f.replace(' ', "").starts_with("pub(self)");
        let f = strip_vis(&f);
        let (fname, fty) = if tuple {
            (k.to_string(), f.as_str())
        } else {
            let c = top_colon(&f)?;
            (f[..c].trim().to_string(), f[c + 1..].trim())
        };
        fields.push((fname, parse_ty(fty).ok()?));
        public.push(is_pub);
    }
    Some((fields, public))
}

/// The variants of an enumeration's body.
fn parse_variants(body: &str) -> Option<Vec<VariantDef>> {
    let mut out = Vec::new();
    for v in split_top(body, ',') {
        let v = strip_attrs(v.trim());
        if v.is_empty() {
            continue;
        }
        let name: String = v
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            return None;
        }
        let rest = v[name.len()..].trim();
        let (kind, fields) = if rest.is_empty() || rest.starts_with('=') {
            (VKind::Unit, vec![])
        } else if rest.starts_with('(') && rest.ends_with(')') {
            (
                VKind::Tuple,
                parse_fields(&rest[1..rest.len() - 1], true)?.0,
            )
        } else if rest.starts_with('{') && rest.ends_with('}') {
            (
                VKind::Named,
                parse_fields(&rest[1..rest.len() - 1], false)?.0,
            )
        } else {
            return None;
        };
        out.push(VariantDef { name, kind, fields });
    }
    Some(out)
}

/// Add the structures and type aliases of one source file of the crate's root module.
pub fn read_items(text: &str, items: &mut Items) {
    read_items_in(text, &[], items)
}

/// The module of a source file, from its path under the package: `src/lib.rs` and
/// `src/main.rs` are the root, `src/a/mod.rs` and `src/a.rs` the module `a`, `src/a/b.rs` the
/// module `a::b`. A file of `src/bin/` is the root of its own program.
pub fn file_module(rel: &std::path::Path) -> Vec<String> {
    let mut parts: Vec<String> = rel
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    if parts.first().is_some_and(|p| p == "src") {
        parts.remove(0);
    }
    if parts.first().is_some_and(|p| p == "bin") {
        return vec![];
    }
    let Some(last) = parts.pop() else {
        return vec![];
    };
    let stem = last.strip_suffix(".rs").unwrap_or(&last).to_string();
    if parts.is_empty() && matches!(stem.as_str(), "lib" | "main") {
        return vec![];
    }
    if stem != "mod" {
        parts.push(stem);
    }
    parts
}

/// The inline modules (`mod a { ... }`) that enclose `pos` (masked text), outermost first.
pub fn inline_modules(m: &[u8], pos: usize) -> Vec<String> {
    enclosing(m, pos)
        .iter()
        .filter_map(|b| {
            let h = strip_vis(&b.header);
            let r = h.strip_prefix("mod ")?;
            let name: String = r
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// Add the structures and type aliases of one source file whose module is `module`.
pub fn read_items_in(text: &str, module: &[String], items: &mut Items) {
    let m = mask(text);
    let ms = String::from_utf8_lossy(&m).to_string();
    let b = ms.as_bytes();
    let id = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    // `const NAME: <type> = <number>;`
    {
        let mut from = 0;
        while let Some(p) = ms[from..].find("const ") {
            let at = from + p;
            from = at + 6;
            if at > 0 && id(b[at - 1]) {
                continue;
            }
            let rest = &ms[at + 6..];
            let Some(semi) = rest.find(';') else { continue };
            let decl = &rest[..semi];
            let (Some(colon), Some(eq)) = (decl.find(':'), decl.find('=')) else {
                continue;
            };
            if colon > eq {
                continue;
            }
            let name = decl[..colon].trim();
            let val = decl[eq + 1..].trim().replace('_', "");
            if let Ok(v) = val.parse::<usize>() {
                if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    items.consts.entry(name.to_string()).or_insert(v);
                }
            }
        }
    }
    // `enum Name<T> { A, B(u8), C { x: u8 } }` (Milestone 307)
    let mut from = 0;
    while let Some(p) = ms[from..].find("enum ") {
        let at = from + p;
        from = at + 5;
        if at > 0 && id(b[at - 1]) {
            continue;
        }
        let rest = ms[at + 5..].trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let mut after = rest[name.len()..].trim_start();
        let mut params = Vec::new();
        if after.starts_with('<') {
            let Some(e) = matching(after, 0, '<', '>') else {
                continue;
            };
            params = generic_names(&after[1..e]);
            after = after[e + 1..].trim_start();
        }
        let Some(o) = after.find('{') else { continue };
        if !after[..o].trim().is_empty() && find_word(after, "where") != Some(0) {
            continue;
        }
        let after = &after[o..];
        let Some(e) = matching(after, 0, '{', '}') else {
            continue;
        };
        let Some(variants) = parse_variants(&after[1..e]) else {
            continue;
        };
        let mut module = module.to_vec();
        module.extend(inline_modules(&m, at));
        items.enums.entry(name.clone()).or_insert(EnumDef {
            name,
            params,
            variants,
            module,
        });
    }
    for kw in ["struct", "type"] {
        let mut from = 0;
        while let Some(p) = ms[from..].find(kw) {
            let at = from + p;
            from = at + kw.len();
            if (at > 0 && id(b[at - 1])) || b.get(at + kw.len()).is_none_or(|&c| c != b' ') {
                continue;
            }
            let rest = &ms[at + kw.len()..];
            let name: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let after = rest.trim_start()[name.len()..].trim_start();
            if kw == "type" {
                // `type Result<T> = result::Result<Status<T>, Error>;` (Milestone 307)
                if after.starts_with('<') {
                    let Some(e) = matching(after, 0, '<', '>') else {
                        continue;
                    };
                    let params = generic_names(&after[1..e]);
                    let rest = after[e + 1..].trim_start();
                    if let (Some(r), true) = (rest.strip_prefix('='), !params.is_empty()) {
                        if let Some(e) = r.find(';') {
                            let t = r[..e].split_whitespace().collect::<Vec<_>>().join(" ");
                            if let Ok(t) = parse_ty(&t) {
                                items.generic_aliases.entry(name).or_insert((params, t));
                            }
                        }
                    }
                    continue;
                }
                // `type X = T;` (not an associated type without `=`)
                if let Some(r) = after.strip_prefix('=') {
                    if let Some(e) = r.find(';') {
                        if let Ok(t) =
                            parse_ty(&r[..e].split_whitespace().collect::<Vec<_>>().join(" "))
                        {
                            items.aliases.entry(name).or_insert(t);
                        }
                    }
                }
                continue;
            }
            let generic = after.starts_with('<');
            let after = if generic {
                match matching(after, 0, '<', '>') {
                    Some(e) => after[e + 1..].trim_start(),
                    None => continue,
                }
            } else {
                after
            };
            let after = match find_word(after, "where") {
                Some(0) => match after.find('{') {
                    Some(o) => &after[o..],
                    None => continue,
                },
                _ => after,
            };
            let (tuple, body) = if after.starts_with('{') {
                match matching(after, 0, '{', '}') {
                    Some(e) => (false, &after[1..e]),
                    None => continue,
                }
            } else if after.starts_with('(') {
                match matching(after, 0, '(', ')') {
                    Some(e) => (true, &after[1..e]),
                    None => continue,
                }
            } else if after.starts_with(';') {
                (true, "")
            } else {
                continue;
            };
            if let Some((fields, public)) = parse_fields(body, tuple) {
                let mut module = module.to_vec();
                module.extend(inline_modules(&m, at));
                items.structs.entry(name.clone()).or_insert(StructDef {
                    name,
                    generic,
                    tuple,
                    fields,
                    public,
                    module,
                });
            }
        }
    }
}

fn strip_vis(f: &str) -> String {
    let f = f.trim_start();
    if let Some(r) = f.strip_prefix("pub(") {
        if let Some(e) = r.find(')') {
            return r[e + 1..].trim_start().to_string();
        }
    }
    f.strip_prefix("pub ").unwrap_or(f).trim_start().to_string()
}

/// A type that fuzzing can generate, check and show.
#[derive(Clone, Debug, PartialEq)]
pub enum FTy {
    Int {
        name: String,
        signed: bool,
        bits: u32,
    },
    Bool,
    /// `f32` or `f64` (Milestone 289): `bits` is 32 or 64.
    Float {
        bits: u32,
    },
    /// `String` (or `str` behind a reference).
    Str,
    /// `Vec<T>` (or `[T]` behind a reference).
    List(Box<FTy>),
    Array(Box<FTy>, usize),
    Opt(Box<FTy>),
    Res(Box<FTy>, Box<FTy>),
    Tuple(Vec<FTy>),
    /// `Box<T>`, `Rc<T>`, `Arc<T>`: the value inside, built with `<kind>::new`.
    Boxed(String, Box<FTy>),
    Struct {
        /// The name as the function's module writes it.
        path: String,
        tuple: bool,
        fields: Vec<(String, FTy)>,
    },
    /// An enumeration (Milestone 307).
    Enum {
        /// The name as the function's module writes it (`Status`, `self::std::cmp::Ordering`).
        path: String,
        /// The type arguments (`(usize, u64)` of `Status<(usize, u64)>`).
        args: Vec<FTy>,
        variants: Vec<Variant>,
    },
    /// A type that a `build` line builds, a `read` line reads, or both (Milestone 307).
    Built {
        /// The type as the harness writes it.
        ty: String,
        build: Option<Ctor>,
        /// The type a value is read as, and the method that gives it.
        read: Option<(Box<FTy>, String)>,
    },
    /// A type fuzzing cannot generate: its text.
    Opaque(String),
    /// A field that the harness's module cannot name (a private field of a structure of
    /// another module): why. The harness neither builds nor reads it.
    Hidden(String),
}

impl FTy {
    /// Whether a value of the type can be generated (no opaque part), else why not.
    pub fn generable(&self) -> Result<(), String> {
        match self {
            FTy::Opaque(t) => Err(format!("hydo cannot generate a value of `{t}`")),
            FTy::Hidden(why) => Err(format!(
                "{why}, so the harness cannot build the value (fuzz a function that takes its \
                 parts and builds it, such as its constructor)"
            )),
            FTy::List(x) | FTy::Array(x, _) | FTy::Opt(x) | FTy::Boxed(_, x) => x.generable(),
            FTy::Res(a, b) => a.generable().and(b.generable()),
            FTy::Tuple(v) => v.iter().try_for_each(FTy::generable),
            FTy::Struct { fields, .. } => fields.iter().try_for_each(|(_, t)| t.generable()),
            FTy::Enum { path, variants, .. } => {
                let mut why = None;
                for v in variants {
                    match v.fields.iter().try_for_each(|(_, t)| t.generable()) {
                        Ok(()) => return Ok(()),
                        Err(e) => {
                            why.get_or_insert(e);
                        }
                    }
                }
                Err(why.unwrap_or_else(|| format!("`{path}` has no variant")))
            }
            FTy::Built { ty, build, .. } => match build {
                Some(c) => c.args.iter().try_for_each(|(_, t, _)| t.generable()),
                None => Err(format!(
                    "hydo cannot generate a value of `{ty}`: a `// hydo: build {ty} from \
                     <constructor>(<name>: <type>, ...)` line names how to build one"
                )),
            },
            _ => Ok(()),
        }
    }

    /// The owned Rust type, as the harness writes it.
    pub fn rust(&self) -> String {
        match self {
            FTy::Int { name, .. } => name.clone(),
            FTy::Bool => "bool".into(),
            FTy::Float { bits } => format!("f{bits}"),
            FTy::Str => "String".into(),
            FTy::List(x) => format!("Vec<{}>", x.rust()),
            FTy::Array(x, n) => format!("[{}; {n}]", x.rust()),
            FTy::Opt(x) => format!("Option<{}>", x.rust()),
            FTy::Res(a, b) => format!("Result<{}, {}>", a.rust(), b.rust()),
            FTy::Tuple(v) if v.len() == 1 => format!("({},)", v[0].rust()),
            FTy::Tuple(v) => format!(
                "({})",
                v.iter().map(FTy::rust).collect::<Vec<_>>().join(", ")
            ),
            FTy::Boxed(k, x) => format!("{k}<{}>", x.rust()),
            FTy::Struct { path, .. } => path.clone(),
            FTy::Enum { path, args, .. } if args.is_empty() => path.clone(),
            FTy::Enum { path, args, .. } => format!(
                "{path}<{}>",
                args.iter().map(FTy::rust).collect::<Vec<_>>().join(", ")
            ),
            FTy::Built { ty, .. } => ty.clone(),
            FTy::Opaque(t) => t.clone(),
            FTy::Hidden(_) => "_".into(),
        }
    }
}

/// A variant of an enumeration as fuzzing sees it (Milestone 307).
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: String,
    pub kind: VKind,
    pub fields: Vec<(String, FTy)>,
}

/// How a `build` line builds a value (Milestone 307): the constructor's path as written, and its
/// arguments with the way each is passed.
#[derive(Clone, Debug, PartialEq)]
pub struct Ctor {
    pub path: String,
    pub args: Vec<(String, FTy, Pass)>,
}

/// `t` with each type parameter that `s` names replaced by its type.
pub fn subst_ty(t: &TyAst, s: &BTreeMap<String, TyAst>) -> TyAst {
    match t {
        TyAst::Path(segs, args)
            if segs.len() == 1 && args.is_empty() && s.contains_key(&segs[0]) =>
        {
            s[&segs[0]].clone()
        }
        TyAst::Path(segs, args) => {
            TyAst::Path(segs.clone(), args.iter().map(|a| subst_ty(a, s)).collect())
        }
        TyAst::Ref(m, x) => TyAst::Ref(*m, Box::new(subst_ty(x, s))),
        TyAst::Slice(x) => TyAst::Slice(Box::new(subst_ty(x, s))),
        TyAst::Array(x, n) => TyAst::Array(Box::new(subst_ty(x, s)), n.clone()),
        TyAst::Tuple(v) => TyAst::Tuple(v.iter().map(|x| subst_ty(x, s)).collect()),
        TyAst::Other(o) => TyAst::Other(o.clone()),
    }
}

/// What resolving a type needs: the crate's items, the type `Self` stands for, and the module
/// of the function. Its harness is a child of that module, so it sees the private fields of the
/// structures of that module and of its ancestors, and no others.
pub struct Resolver<'a> {
    pub items: &'a Items,
    pub self_ty: Option<&'a TyAst>,
    pub module: &'a [String],
    /// The types that `with` lines give a generic function's type parameters.
    pub subst: &'a BTreeMap<String, TyAst>,
}

impl Resolver<'_> {
    /// The fuzzing type of a type as written.
    pub fn resolve(&self, t: &TyAst) -> FTy {
        self.res(t, &mut Vec::new())
    }

    fn res(&self, t: &TyAst, stack: &mut Vec<String>) -> FTy {
        if stack.len() > 12 {
            return FTy::Opaque(t.text());
        }
        match t {
            TyAst::Ref(_, x) => self.res(x, stack),
            TyAst::Slice(x) => FTy::List(Box::new(self.res(x, stack))),
            TyAst::Array(x, n) => match n
                .replace('_', "")
                .parse::<usize>()
                .ok()
                .or_else(|| self.items.consts.get(n.trim()).copied())
                .ok_or(())
            {
                Ok(n) if n <= 64 => FTy::Array(Box::new(self.res(x, stack)), n),
                _ => FTy::Opaque(t.text()),
            },
            TyAst::Tuple(v) => FTy::Tuple(v.iter().map(|x| self.res(x, stack)).collect()),
            TyAst::Other(s) => FTy::Opaque(s.clone()),
            TyAst::Path(segs, args)
                if segs.len() == 1 && args.is_empty() && self.subst.contains_key(&segs[0]) =>
            {
                self.res(&self.subst[&segs[0]], stack)
            }
            TyAst::Path(segs, args) => {
                let last = segs.last().map(String::as_str).unwrap_or("");
                let int = |name: &str, signed: bool, bits: u32| FTy::Int {
                    name: name.into(),
                    signed,
                    bits,
                };
                let arg = |i: usize, stack: &mut Vec<String>| match args.get(i) {
                    Some(a) => self.res(a, stack),
                    None => FTy::Opaque("_".into()),
                };
                // the crate's own alias with type parameters, also over a standard name
                // (`Result<usize>` as httparse writes it; Milestone 307)
                let own = segs.len() == 1 || matches!(segs[0].as_str(), "crate" | "self" | "super");
                if let Some((params, a)) = self
                    .items
                    .generic_aliases
                    .get(last)
                    .filter(|(p, _)| own && p.len() == args.len())
                {
                    if stack.iter().any(|s| s == last) {
                        return FTy::Opaque(t.text());
                    }
                    let map: BTreeMap<String, TyAst> = params
                        .iter()
                        .cloned()
                        .zip(args.iter().map(|x| subst_ty(x, self.subst)))
                        .collect();
                    stack.push(last.to_string());
                    let r = self.res(&subst_ty(a, &map), stack);
                    stack.pop();
                    return r;
                }
                match (last, args.len()) {
                    ("u8", 0) => int("u8", false, 8),
                    ("u16", 0) => int("u16", false, 16),
                    ("u32", 0) => int("u32", false, 32),
                    ("u64", 0) => int("u64", false, 64),
                    ("u128", 0) => int("u128", false, 128),
                    ("usize", 0) => int("usize", false, 64),
                    ("i8", 0) => int("i8", true, 8),
                    ("i16", 0) => int("i16", true, 16),
                    ("i32", 0) => int("i32", true, 32),
                    ("i64", 0) => int("i64", true, 64),
                    ("i128", 0) => int("i128", true, 128),
                    ("isize", 0) => int("isize", true, 64),
                    ("bool", 0) => FTy::Bool,
                    ("f32", 0) => FTy::Float { bits: 32 },
                    ("f64", 0) => FTy::Float { bits: 64 },
                    ("String" | "str", 0) => FTy::Str,
                    ("Vec", 1) => FTy::List(Box::new(arg(0, stack))),
                    ("Option", 1) => FTy::Opt(Box::new(arg(0, stack))),
                    ("Result", 1) => FTy::Res(
                        Box::new(arg(0, stack)),
                        Box::new(FTy::Opaque("the error".into())),
                    ),
                    ("Result", 2) => FTy::Res(Box::new(arg(0, stack)), Box::new(arg(1, stack))),
                    ("Box" | "Rc" | "Arc", 1) => FTy::Boxed(last.into(), Box::new(arg(0, stack))),
                    ("Self", 0) => match self.self_ty {
                        Some(s) if !matches!(s, TyAst::Path(p, _) if p.last().map(String::as_str) == Some("Self")) => {
                            self.res(s, stack)
                        }
                        _ => FTy::Opaque("Self".into()),
                    },
                    (name, n) => {
                        // a `build` or `read` line names how to make or read the type, also
                        // one whose fields hydo could read (Milestone 307)
                        if self.items.builds.contains_key(name)
                            || self.items.reads.contains_key(name)
                        {
                            return self.built(name, t, stack);
                        }
                        if let Some(a) = self.items.aliases.get(name).filter(|_| n == 0) {
                            if stack.iter().any(|s| s == name) {
                                return FTy::Opaque(t.text());
                            }
                            stack.push(name.into());
                            let r = self.res(a, stack);
                            stack.pop();
                            return r;
                        }
                        let Some(d) = self.items.structs.get(name).filter(|_| n == 0) else {
                            if let Some(e) = self.items.enums.get(name) {
                                return self.enumeration(e, segs, args, t, stack);
                            }
                            if name == "Ordering" && n == 0 && !segs.iter().any(|s| s == "atomic") {
                                return ordering();
                            }
                            return FTy::Opaque(t.text());
                        };
                        if d.generic || stack.iter().any(|s| s == name) {
                            return FTy::Opaque(t.text());
                        }
                        stack.push(name.into());
                        let seen = self.module.starts_with(&d.module);
                        let fields = d
                            .fields
                            .iter()
                            .zip(d.public.iter().chain(std::iter::repeat(&true)))
                            .map(|((n, ft), &public)| {
                                if public || seen {
                                    return (n.clone(), self.res(ft, stack));
                                }
                                let m = if d.module.is_empty() {
                                    "the crate's root".to_string()
                                } else {
                                    format!("`{}`", d.module.join("::"))
                                };
                                let why = format!(
                                    "the field `{n}` of `{name}` is private to the module {m}"
                                );
                                (n.clone(), FTy::Hidden(why))
                            })
                            .collect();
                        stack.pop();
                        FTy::Struct {
                            path: segs.join("::"),
                            tuple: d.tuple,
                            fields,
                        }
                    }
                }
            }
        }
    }

    /// A type that a `build` or `read` line names (Milestone 307).
    fn built(&self, name: &str, t: &TyAst, stack: &mut Vec<String>) -> FTy {
        if stack.iter().any(|s| s == name) {
            return FTy::Opaque(t.text());
        }
        stack.push(name.into());
        let build = self.items.builds.get(name).map(|b| Ctor {
            path: b.ctor.clone(),
            args: b
                .args
                .iter()
                .map(|(n, a)| (n.clone(), self.res(a, stack), pass_of(a)))
                .collect(),
        });
        let read = self
            .items
            .reads
            .get(name)
            .map(|(v, m)| (Box::new(self.res(v, stack)), m.clone()));
        stack.pop();
        let ty = match t {
            TyAst::Ref(_, x) => x.text(),
            o => o.text(),
        };
        FTy::Built { ty, build, read }
    }

    /// An enumeration of the crate, with its type parameters given (Milestone 307).
    fn enumeration(
        &self,
        e: &EnumDef,
        segs: &[String],
        args: &[TyAst],
        t: &TyAst,
        stack: &mut Vec<String>,
    ) -> FTy {
        if stack.contains(&e.name) || e.params.iter().any(|p| p.starts_with("const ")) {
            return FTy::Opaque(t.text());
        }
        let targs: Vec<TyAst> = args.iter().map(|a| subst_ty(a, self.subst)).collect();
        let map: BTreeMap<String, TyAst> = e
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let a = targs.get(i).cloned();
                (p.clone(), a.unwrap_or_else(|| TyAst::Other(p.clone())))
            })
            .collect();
        let inner = Resolver {
            items: self.items,
            self_ty: self.self_ty,
            module: self.module,
            subst: &map,
        };
        stack.push(e.name.clone());
        let variants = e
            .variants
            .iter()
            .map(|v| Variant {
                name: v.name.clone(),
                kind: v.kind,
                fields: v
                    .fields
                    .iter()
                    .map(|(n, ft)| (n.clone(), inner.res(ft, stack)))
                    .collect(),
            })
            .collect();
        let args = targs.iter().map(|a| self.res(a, stack)).collect();
        stack.pop();
        FTy::Enum {
            path: segs.join("::"),
            args,
            variants,
        }
    }
}

/// The standard library's `Ordering`, which comparisons return (Milestone 307).
fn ordering() -> FTy {
    let unit = |n: &str| Variant {
        name: n.into(),
        kind: VKind::Unit,
        fields: vec![],
    };
    FTy::Enum {
        path: "self::std::cmp::Ordering".into(),
        args: vec![],
        variants: vec![unit("Less"), unit("Equal"), unit("Greater")],
    }
}

/// How a parameter is passed: by value, by `&` or by `&mut`, and whether the reference is to an
/// unsized_ref type (`str`, `[T]`), which the harness passes as `&local[..]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Value,
    Ref { unsized_ref: bool },
    RefMut { unsized_ref: bool },
}

/// How a parameter of type `t` is passed.
pub fn pass_of(t: &TyAst) -> Pass {
    match t {
        TyAst::Ref(m, x) => {
            let unsized_ref = matches!(**x, TyAst::Slice(_))
                || matches!(&**x, TyAst::Path(s, a) if a.is_empty() && s.last().map(String::as_str) == Some("str"));
            if *m {
                Pass::RefMut { unsized_ref }
            } else {
                Pass::Ref { unsized_ref }
            }
        }
        _ => Pass::Value,
    }
}

/// An `impl` block's header read in full (Milestone 307): the type, the trait of `impl Trait
/// for Type`, and the names of the block's type parameters.
pub fn impl_parts(header: &str) -> Option<(TyAst, Option<TyAst>, Vec<String>)> {
    let h = header.strip_prefix("unsafe ").unwrap_or(header);
    let h = h.strip_prefix("impl")?;
    if !h.starts_with([' ', '<']) {
        return None;
    }
    let h = h.trim_start();
    let (params, h) = if h.starts_with('<') {
        let e = matching(h, 0, '<', '>')?;
        (generic_names(&h[1..e]), h[e + 1..].trim_start())
    } else {
        (vec![], h)
    };
    let h = match find_word(h, "where") {
        Some(w) => h[..w].trim(),
        None => h.trim(),
    };
    let (tr, ty) = match find_word(h, "for") {
        Some(f) => (Some(h[..f].trim()), h[f + 3..].trim()),
        None => (None, h),
    };
    let tr = match tr {
        Some(t) => Some(parse_ty(t.trim_start_matches('!')).ok()?),
        None => None,
    };
    Some((parse_ty(ty).ok()?, tr, params))
}

/// A trait's header read (Milestone 307): its name and the names of its type parameters
/// (`pub trait ReadBytesExt: io::Read`, `trait Foo<T>`).
pub fn trait_parts(header: &str) -> Option<(String, Vec<String>)> {
    let mut h = strip_vis(header);
    for q in ["unsafe ", "auto "] {
        if let Some(r) = h.strip_prefix(q) {
            h = r.trim_start().to_string();
        }
    }
    let r = h.strip_prefix("trait ")?.trim_start();
    let name: String = r
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    let after = r[name.len()..].trim_start();
    let params = if after.starts_with('<') {
        let e = matching(after, 0, '<', '>')?;
        generic_names(&after[1..e])
    } else {
        vec![]
    };
    Some((name, params))
}

/// The type of an `impl` block's header (`impl<T> Foo<T>`, `impl Trait for Foo`).
pub fn impl_type(header: &str) -> Option<(TyAst, bool)> {
    let h = header.strip_prefix("impl")?.trim_start();
    let mut generic = false;
    let h = if h.starts_with('<') {
        let e = matching(h, 0, '<', '>')?;
        generic = h[1..e]
            .split(',')
            .any(|g| !g.trim().starts_with('\'') && !g.trim().is_empty());
        h[e + 1..].trim_start()
    } else {
        h
    };
    let h = match find_word(h, "where") {
        Some(w) => h[..w].trim(),
        None => h.trim(),
    };
    let ty = match find_word(h, "for") {
        Some(f) => h[f + 3..].trim(),
        None => h,
    };
    Some((parse_ty(ty).ok()?, generic))
}
