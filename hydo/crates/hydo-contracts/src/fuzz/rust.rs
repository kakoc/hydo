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
    // a `;` inside brackets is an array type's (`impl<const N: usize> T for [u8; N]`), not an
    // item's end (Milestone 318)
    let mut depth = 0i32;
    while i < pos.min(m.len()) {
        match m[i] {
            b'{' => {
                stack.push((item_start, i));
                item_start = i + 1;
                depth = 0;
            }
            b'}' => {
                stack.pop();
                item_start = i + 1;
                depth = 0;
            }
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b';' if depth <= 0 => item_start = i + 1,
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
    /// The bounds of the type parameters, inline and in the `where` clause: the name and the
    /// bound's text (`F`, `FnOnce(L) -> M`; Milestone 318).
    pub bounds: Vec<(String, String)>,
    /// The parameters whose type is a `&'static` reference (Milestone 318).
    pub statics: Vec<String>,
}

/// The bounds of a generics list's or a `where` clause's parts: `F: FnOnce(L) -> M` gives
/// (`F`, `FnOnce(L) -> M`); lifetimes and parts without a bound are left out.
pub fn bounds_of(parts: &str) -> Vec<(String, String)> {
    split_top(parts, ',')
        .iter()
        .filter_map(|g| {
            let g = g.trim();
            if g.starts_with('\'') || g.starts_with("const ") {
                return None;
            }
            let c = top_colon(g)?;
            let name = g[..c].trim();
            name.chars()
                .all(|c| c.is_alphanumeric() || c == '_')
                .then(|| (name.to_string(), g[c + 1..].trim().to_string()))
        })
        .collect()
}

/// The closure signature in a bound (`FnOnce(L) -> M + Send`): the arguments' types and the
/// result's (Milestone 318); none for another bound.
pub fn fn_bound(bound: &str) -> Option<FnBound> {
    for part in split_top(bound, '+') {
        let p = part.trim();
        let p = p.strip_prefix("impl ").unwrap_or(p).trim();
        let p = p.strip_prefix("dyn ").unwrap_or(p).trim();
        let Some(k) = ["FnOnce(", "FnMut(", "Fn("]
            .iter()
            .find(|k| p.starts_with(**k))
        else {
            continue;
        };
        let once = *k == "FnOnce(";
        let inside = &p[k.len() - 1..];
        let e = matching(inside, 0, '(', ')')?;
        let args = split_top(&inside[1..e], ',')
            .iter()
            .map(|a| a.trim())
            .filter(|a| !a.is_empty())
            .map(parse_ty)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let rest = inside[e + 1..].trim();
        let ret = match rest.strip_prefix("->") {
            Some(t) => parse_ty(t.trim()).ok()?,
            None => TyAst::Tuple(vec![]),
        };
        return Some(FnBound { once, args, ret });
    }
    None
}

/// A closure's signature in a bound (Milestone 318).
#[derive(Clone, Debug, PartialEq)]
pub struct FnBound {
    /// `FnOnce`: the closure may move its captured value out.
    pub once: bool,
    pub args: Vec<TyAst>,
    pub ret: TyAst,
}

/// What a generated closure does with its arguments (Milestone 318; the shapes of two arguments
/// and of an `Option` result since Milestone 326).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `|_| k`: the generated value, whatever the arguments.
    Const,
    /// `|x| x`: the argument itself, when the result has its type.
    Same,
    /// `|x| try_from(x).unwrap_or(k)`: an integer argument as the integer result.
    Conv,
    /// `|x| try_from(x).unwrap_or(k).wrapping_add(1)`.
    Add,
    /// `|x| x % 2 == 0`: a predicate on an integer argument.
    Even,
    /// `|x| x > 0`.
    Positive,
    /// `|x, y| x + y`: two integer arguments added, as the integer result, wrapping.
    Sum,
    /// `|_, y| y`: the second argument, when the result has its type.
    Second,
    /// `|a, b| a.cmp(b)`: two arguments of one type compared, for a sort.
    Cmp,
    /// `|a, b| b.cmp(a)`: the reverse order.
    RevCmp,
    /// `|x, y| x < y` and `|x, y| x == y`: a predicate on two arguments of one type.
    Less,
    Equal,
    /// `|x| Some(x)`: the argument in an `Option`, when the result is an `Option` of its type.
    SomeSame,
    /// `|x| try_from(x).ok()`: an integer argument as an `Option` of an integer.
    SomeConv,
}

/// Every shape, in the order of its number in a closure's path.
const SHAPES: [Shape; 14] = [
    Shape::Const,
    Shape::Same,
    Shape::Conv,
    Shape::Add,
    Shape::Even,
    Shape::Positive,
    Shape::Sum,
    Shape::Second,
    Shape::Cmp,
    Shape::RevCmp,
    Shape::Less,
    Shape::Equal,
    Shape::SomeSame,
    Shape::SomeConv,
];

/// A closure that hydo generates for a parameter bound by `Fn`, `FnMut` or `FnOnce`
/// (Milestone 318): its argument and result types as the harness writes them, and the shapes
/// it takes. A generated closure is a shape's index and a generated value `k`.
#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    pub once: bool,
    pub args: Vec<String>,
    /// Whether the first argument is a reference, which the integer shapes read through.
    pub by_ref: bool,
    pub ret: String,
    pub shapes: Vec<Shape>,
    /// Whether the second argument is a reference (Milestone 326).
    pub by_ref2: bool,
    /// The type inside an `Option` result, for `|x| try_from(x).ok()` (Milestone 326).
    pub some: String,
}

/// The mark that starts a closure's `Ctor` path.
const CLOSURE: &str = "\u{1}closure";

impl Closure {
    /// The closure for a bound whose types are resolved: the shapes that fit its first
    /// argument and its result.
    pub fn new(b: &FnBound, first: Option<&FTy>, ret: &FTy) -> Closure {
        Closure::with_args(b, &first.into_iter().cloned().collect::<Vec<_>>(), ret)
    }

    /// The closure for a bound whose argument types are resolved as `args` (Milestone 326):
    /// the shapes that fit its arguments and its result. A comparison for a sort takes no
    /// constant, because a sort may panic on an order that is not total.
    pub fn with_args(b: &FnBound, args: &[FTy], ret: &FTy) -> Closure {
        let int = |t: &FTy| matches!(t, FTy::Int { .. });
        let text = |i: usize| b.args.get(i).map(TyAst::text);
        let same_args = args.len() == 2 && text(0) == text(1);
        let ordering = matches!(&b.ret, TyAst::Path(s, a) if a.is_empty()
            && s.last().is_some_and(|l| l == "Ordering")
            && !s.iter().any(|x| x == "atomic"));
        // `Option<T>`: the inside's text
        let some = match &b.ret {
            TyAst::Path(s, a) if s.last().is_some_and(|l| l == "Option") && a.len() == 1 => {
                Some(a[0].text())
            }
            _ => None,
        };
        let mut shapes = if ordering && same_args {
            vec![Shape::Cmp, Shape::RevCmp]
        } else {
            vec![Shape::Const]
        };
        match args.first() {
            Some(a) if int(a) && int(ret) => shapes.extend([Shape::Conv, Shape::Add]),
            Some(a) if int(a) && *ret == FTy::Bool => shapes.extend([Shape::Even, Shape::Positive]),
            Some(_) if text(0).as_deref() == Some(b.ret.text().as_str()) => {
                shapes.push(Shape::Same)
            }
            _ => {}
        }
        if let [a, c] = args {
            if int(a) && int(c) && int(ret) {
                shapes.push(Shape::Sum);
            }
            if text(1).as_deref() == Some(b.ret.text().as_str()) {
                shapes.push(Shape::Second);
            }
            if same_args && *ret == FTy::Bool {
                shapes.extend([Shape::Less, Shape::Equal]);
            }
        }
        if let (Some(inner), Some(a)) = (&some, args.first()) {
            if text(0).as_deref() == Some(inner.as_str()) {
                shapes.push(Shape::SomeSame);
            }
            if int(a) && matches!(ret, FTy::Opt(x) if int(x)) {
                shapes.push(Shape::SomeConv);
            }
        }
        Closure {
            once: b.once,
            args: b.args.iter().map(TyAst::text).collect(),
            by_ref: matches!(b.args.first(), Some(TyAst::Ref(..))),
            ret: b.ret.text(),
            shapes,
            by_ref2: matches!(b.args.get(1), Some(TyAst::Ref(..))),
            some: some.unwrap_or_default(),
        }
    }

    /// The closure as a `Ctor`'s path.
    pub fn path(&self) -> String {
        let shapes: Vec<String> = self
            .shapes
            .iter()
            .map(|s| format!("{}", *s as u8))
            .collect();
        format!(
            "{CLOSURE}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.once,
            self.by_ref,
            self.args.join("\u{1e}"),
            self.ret,
            shapes.join(","),
            self.by_ref2,
            self.some
        )
    }

    /// The closure a `Ctor`'s path holds, if it holds one.
    pub fn of(path: &str) -> Option<Closure> {
        let r = path.strip_prefix(CLOSURE)?.strip_prefix('\u{1f}')?;
        let p: Vec<&str> = r.split('\u{1f}').collect();
        let [once, by_ref, args, ret, shapes, by_ref2, some] = p.as_slice() else {
            return None;
        };
        Some(Closure {
            once: *once == "true",
            by_ref: *by_ref == "true",
            args: args
                .split('\u{1e}')
                .filter(|a| !a.is_empty())
                .map(str::to_string)
                .collect(),
            ret: ret.to_string(),
            shapes: shapes
                .split(',')
                .filter_map(|s| SHAPES.get(s.parse::<usize>().ok()?).copied())
                .collect(),
            by_ref2: *by_ref2 == "true",
            some: some.to_string(),
        })
    }

    /// The shape that a generated index stands for.
    pub fn shape(&self, i: i128) -> Shape {
        let n = self.shapes.len().max(1) as i128;
        self.shapes
            .get(i.rem_euclid(n) as usize)
            .copied()
            .unwrap_or(Shape::Const)
    }

    /// The closure as a report shows it, with `k` shown.
    pub fn show(&self, i: i128, k: &str) -> String {
        let rest = if self.args.len() > 1 {
            ", _".repeat(self.args.len() - 1)
        } else {
            String::new()
        };
        let x = if self.args.is_empty() { "" } else { "x" };
        // the shapes of two arguments name both
        let more = if self.args.len() > 2 {
            ", _".repeat(self.args.len() - 2)
        } else {
            String::new()
        };
        match self.shape(i) {
            Shape::Const => format!("|{}{rest}| {k}", if x.is_empty() { "" } else { "_" }),
            Shape::Same => format!("|{x}{rest}| x"),
            Shape::Conv => format!("|{x}{rest}| try_from(x).unwrap_or({k})"),
            Shape::Add => format!("|{x}{rest}| try_from(x).unwrap_or({k}).wrapping_add(1)"),
            Shape::Even => format!("|{x}{rest}| x % 2 == 0"),
            Shape::Positive => format!("|{x}{rest}| x > 0"),
            Shape::Sum => format!("|x, y{more}| x + y"),
            Shape::Second => format!("|_, y{more}| y"),
            Shape::Cmp => format!("|a, b{more}| a.cmp(b)"),
            Shape::RevCmp => format!("|a, b{more}| b.cmp(a)"),
            Shape::Less => format!("|x, y{more}| x < y"),
            Shape::Equal => format!("|x, y{more}| x == y"),
            Shape::SomeSame => format!("|{x}{rest}| Some(x)"),
            Shape::SomeConv => format!("|{x}{rest}| try_from(x).ok()"),
        }
    }
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
    let mut bounds = bounds_of(&generics.join(", "));
    if let Some(w) = find_word(after, "where") {
        bounds.extend(bounds_of(&after[w + 5..]));
    }
    let mut statics = Vec::new();
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
        let ty = p[colon + 1..].trim();
        if ty.replace(' ', "").starts_with("&'static") {
            statics.push(pat.to_string());
        }
        params.push((pat.to_string(), parse_ty(ty)?));
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
        bounds,
        statics,
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
    // `<A as Array>::Item`, a type parameter's associated type with its trait named
    // (Milestone 333): read as `A::Item`
    if s.starts_with('<') {
        if let Some(e) = matching(s, 0, '<', '>') {
            let (inner, rest) = (&s[1..e], s[e + 1..].trim());
            if let (Some((base, _)), Some(r)) = (inner.split_once(" as "), rest.strip_prefix("::"))
            {
                if let Ok(TyAst::Path(mut segs, a)) = parse_ty(base) {
                    let more: Vec<String> = r.split("::").map(|x| x.trim().to_string()).collect();
                    if a.is_empty()
                        && segs.len() == 1
                        && more.iter().all(|x| {
                            !x.is_empty() && x.chars().all(|c| c.is_alphanumeric() || c == '_')
                        })
                    {
                        segs.extend(more);
                        return Ok(TyAst::Path(segs, vec![]));
                    }
                }
            }
        }
        return Ok(TyAst::Other(s.into()));
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
    /// The type parameters (`T` of `OrderedFloat<T>`), lifetimes left out (Milestone 333).
    pub params: Vec<String>,
    /// Whether the structure has a lifetime parameter (`Word<'a>`; Milestone 333).
    pub lifetimes: bool,
    pub tuple: bool,
    pub fields: Vec<(String, TyAst)>,
    /// Whether each field is declared with a `pub` visibility (`pub`, `pub(crate)`, ...).
    pub public: Vec<bool>,
    /// The module that defines the structure (`["identifier"]`; empty for the crate's root).
    pub module: Vec<String>,
    /// Whether the structure is declared with a `pub` visibility (Milestone 333).
    pub is_pub: bool,
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
    /// Whether the enumeration is declared with a `pub` visibility (Milestone 333).
    pub is_pub: bool,
}

/// A `build` line (Milestone 307): the constructor's path and its arguments.
#[derive(Clone, Debug)]
pub struct BuildDef {
    pub ctor: String,
    pub args: Vec<(String, TyAst)>,
    /// The constructor returns a `Result` or an `Option` (Milestone 333).
    pub fallible: bool,
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
    /// The `impl` blocks of the crate, for the associated types a trait's method returns
    /// (Milestone 318).
    pub impls: Vec<ImplDef>,
    /// Every enumeration and structure of a name, when two modules define one (humantime's two
    /// `Error`s; Milestone 318). `enums` and `structs` keep the first.
    pub all_enums: BTreeMap<String, Vec<EnumDef>>,
    pub all_structs: BTreeMap<String, Vec<StructDef>>,
    /// The `use` lines of each module's file: by the module, each imported name with the paths
    /// it may stand for, as modules and the item's name (Milestone 318). The name `*` holds the
    /// modules of the file's glob imports (Milestone 326).
    pub uses: BTreeMap<Vec<String>, BTreeMap<String, Vec<Vec<String>>>>,
    /// The structures with `#[derive(Default)]` (Milestone 326).
    pub defaults: std::collections::BTreeSet<String>,
    /// The `use` lines of each module, an inline module apart from its file's (Milestone 333).
    pub uses_exact: BTreeMap<Vec<String>, BTreeMap<String, Vec<Vec<String>>>>,
    /// The result types of the functions outside `impl` blocks and traits, by name, for a
    /// `build` line's constructor that returns a `Result` or an `Option` (Milestone 333).
    pub free_fns: BTreeMap<String, Vec<Option<TyAst>>>,
}

/// A structure's name from its path as the harness writes it: the last segment, without the
/// type arguments of a generic one (`OrderedFloat::<f64>` gives `OrderedFloat`; Milestone 333).
pub fn struct_name(path: &str) -> &str {
    let p = match path.find("::<") {
        Some(i) if path.ends_with('>') => &path[..i],
        _ => path,
    };
    p.rsplit("::").next().unwrap_or(p)
}

/// A path split at its `::` outside angle brackets (`SmallVec::<[u8; 4]>::from_vec` gives
/// `SmallVec`, `<[u8; 4]>` and `from_vec`).
fn path_segments(p: &str) -> Vec<String> {
    let (mut out, mut cur, mut d) = (Vec::new(), String::new(), 0i32);
    let b: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '<' => d += 1,
            '>' => d -= 1,
            ':' if d == 0 && b.get(i + 1) == Some(&':') => {
                out.push(std::mem::take(&mut cur).trim().to_string());
                i += 2;
                continue;
            }
            _ => {}
        }
        cur.push(b[i]);
        i += 1;
    }
    out.push(cur.trim().to_string());
    out.retain(|x| !x.is_empty());
    out
}

/// Whether a `build` line's constructor returns a `Result` or an `Option` (Milestone 333), as
/// the crate's sources write it: the function of the type's `impl` blocks (`Prerelease::new`),
/// or a function outside them (`hydo_props::date`); `TryFrom::try_from` and
/// `FromStr::from_str` named with their trait (`<X as TryFrom<u8>>::try_from`) too.
pub fn ctor_fallible(items: &Items, ctor: &str) -> bool {
    let segs = path_segments(ctor);
    let Some(name) = segs.last() else {
        return false;
    };
    let fallible = |r: &Option<TyAst>| matches!(r, Some(TyAst::Path(s, _)) if s.last().is_some_and(|l| l == "Result" || l == "Option"));
    if segs.first().is_some_and(|f| f.starts_with('<')) {
        return matches!(name.as_str(), "try_from" | "from_str");
    }
    let types: Vec<&String> = segs[..segs.len() - 1]
        .iter()
        .filter(|x| !x.starts_with('<'))
        .collect();
    if let Some(ty) = types.last() {
        let key = type_key(ty);
        for b in &items.impls {
            if type_key(&b.ty.text()) != key {
                continue;
            }
            if let Some((_, r)) = b.rets.iter().find(|(n, _)| n == name) {
                return fallible(r);
            }
        }
    }
    items
        .free_fns
        .get(name.as_str())
        .is_some_and(|rs| !rs.is_empty() && rs.iter().all(fallible))
}

/// Add the names a `use` tree imports (`a::b::{C, d::E as F, self}`) to `out`: each name with
/// the paths it may stand for. `module` is the module of the file; a path that starts with
/// `crate`, `self` or `super` is made absolute, and any other one is read both from the
/// module (2018's paths) and from the crate's root (Milestone 318).
fn use_tree(
    prefix: &[String],
    tree: &str,
    module: &[String],
    out: &mut BTreeMap<String, Vec<Vec<String>>>,
) {
    let tree = tree.trim();
    if let Some(open) = tree.find('{') {
        let head: Vec<String> = tree[..open]
            .trim()
            .trim_end_matches("::")
            .split("::")
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().to_string())
            .collect();
        let mut p = prefix.to_vec();
        p.extend(head);
        let Some(close) = tree.rfind('}') else { return };
        for part in split_top(&tree[open + 1..close], ',') {
            use_tree(&p, &part, module, out);
        }
        return;
    }
    let (path, alias) = match tree.split_once(" as ") {
        Some((p, a)) => (p.trim(), Some(a.trim().to_string())),
        None => (tree, None),
    };
    let mut segs = prefix.to_vec();
    segs.extend(
        path.trim_start_matches("::")
            .split("::")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    );
    if segs.last().is_some_and(|s| s == "self") {
        segs.pop();
    }
    let Some(name) = alias.or_else(|| segs.last().cloned()) else {
        return;
    };
    // a glob import (`use a::*`) is kept under the name `*` with the module it names
    // (Milestone 326)
    if name == "*" {
        segs.pop();
    }
    if name == "_" || segs.is_empty() {
        return;
    }
    let mut paths = Vec::new();
    match segs[0].as_str() {
        "crate" => paths.push(segs[1..].to_vec()),
        "self" => paths.push(
            module
                .iter()
                .cloned()
                .chain(segs[1..].iter().cloned())
                .collect(),
        ),
        "super" => {
            let mut m = module.to_vec();
            let mut i = 0;
            while segs.get(i).is_some_and(|s| s == "super") {
                m.pop();
                i += 1;
            }
            m.extend(segs[i..].iter().cloned());
            paths.push(m);
        }
        _ => {
            paths.push(module.iter().cloned().chain(segs.iter().cloned()).collect());
            paths.push(segs.clone());
        }
    }
    out.entry(name).or_default().extend(paths);
}

/// An `impl` block of the crate as the resolution of an associated type needs it (Milestone
/// 318): the type, the trait, the associated types it names (`type Owned = String;`), and the
/// result type of each method.
#[derive(Clone, Debug)]
pub struct ImplDef {
    pub ty: TyAst,
    pub tr: Option<TyAst>,
    /// The block's type parameters (`T` and `N` of `impl<T, const N: usize>`), for an
    /// associated type of a type parameter's type (Milestone 333).
    pub generics: Vec<String>,
    pub assoc: Vec<(String, TyAst)>,
    pub rets: Vec<(String, Option<TyAst>)>,
    /// The block's functions without a receiver, each with whether it is `pub`, for the
    /// constructor that a refusal proposes (Milestone 326).
    pub fns: Vec<(bool, Sig)>,
    /// The block's `pub` methods that take `&self` and nothing else, with their results, for
    /// the `read` line that a refusal proposes.
    pub getters: Vec<(String, TyAst)>,
}

/// Whether two types are written alike, spaces aside.
fn same_ty(a: &TyAst, b: &TyAst) -> bool {
    a.text().replace(' ', "") == b.text().replace(' ', "")
}

/// The last segment of a trait's path, without type arguments.
pub fn trait_name(t: &TyAst) -> String {
    match t {
        TyAst::Path(s, _) => s.last().cloned().unwrap_or_default(),
        o => o.text(),
    }
}

/// What `Self::<name>` stands for in the method `method` of the trait `tr` (of an inherent
/// `impl` when `tr` is none) on the type `self_ty` (Milestone 318), in this order:
///
/// - `type <name> = <type>;` in the crate's `impl` of the trait for the type, else in another
///   `impl` for the type;
/// - for a result (`ret`), the result type that the method of that `impl` writes, when it
///   names no `Self`;
/// - the standard library's `ToOwned::Owned`: `String` for `str`, `Vec<T>` for `[T]`, else the
///   type itself.
pub fn assoc_type(
    items: &Items,
    self_ty: &TyAst,
    tr: Option<&str>,
    method: &str,
    name: &str,
    ret: bool,
) -> Option<TyAst> {
    // the block's type parameters bound to the parts of the type (`impl<T> Add<T> for
    // NotNan<T>` for `NotNan<f64>`; Milestone 333)
    let fits = |i: &ImplDef| -> Option<BTreeMap<String, TyAst>> {
        let mut map = BTreeMap::new();
        unify(&i.ty, self_ty, &i.generics, &mut map).then_some(map)
    };
    let own_trait = |i: &ImplDef| match (tr, &i.tr) {
        (Some(t), Some(it)) => trait_name(it) == t,
        (None, None) => true,
        _ => false,
    };
    let blocks: Vec<(&ImplDef, BTreeMap<String, TyAst>)> = items
        .impls
        .iter()
        .filter(|i| own_trait(i))
        .filter_map(|i| fits(i).map(|m| (i, m)))
        .collect();
    let others = items.impls.iter().filter_map(|i| fits(i).map(|m| (i, m)));
    let bound =
        |t: &TyAst, map: &BTreeMap<String, TyAst>| subst_param_assoc(&subst_ty(t, map), map, items);
    for (b, map) in blocks.iter().cloned().chain(others) {
        if let Some((_, t)) = b.assoc.iter().find(|(n, _)| n == name) {
            return Some(bound(t, &map));
        }
    }
    if ret {
        for (b, map) in &blocks {
            if let Some((_, Some(r))) = b.rets.iter().find(|(n, _)| n == method) {
                if !r.text().contains("Self") {
                    return Some(bound(r, map));
                }
            }
        }
    }
    if name == "Owned" {
        return Some(match self_ty {
            TyAst::Path(s, a) if a.is_empty() && s.last().map(String::as_str) == Some("str") => {
                TyAst::Path(vec!["String".into()], vec![])
            }
            TyAst::Slice(x) => TyAst::Path(vec!["Vec".into()], vec![(**x).clone()]),
            o => o.clone(),
        });
    }
    None
}

/// Whether the item whose keyword comes right after `before` (the text up to it) is declared
/// with a `pub` visibility (Milestone 333): the words since the item before, a block's `{` or a
/// `;`, attributes left out.
fn item_is_pub(before: &str) -> bool {
    let tail = &before[before.rfind([';', '}', '{']).map_or(0, |i| i + 1)..];
    strip_attrs(tail)
        .split_whitespace()
        .any(|w| w.starts_with("pub"))
}

/// Whether the item that starts after `before` (the text since its block's `{`) is declared
/// `pub`: also `pub(crate)`, `pub const` and `pub unsafe` (Milestone 326).
fn is_pub_before(before: &str) -> bool {
    let tail = &before[before.rfind([';', '}']).map_or(0, |i| i + 1)..];
    strip_attrs(tail)
        .split_whitespace()
        .any(|w| w.starts_with("pub"))
}

/// What to write for a structure whose fields the harness cannot set (Milestone 326): the
/// `build` line with a public constructor of the crate and its parameters (`new` first, then
/// `from_*`, `with_*`, the others, `From` and `Default`), and a `read` line with a public
/// method that reads the value. `path` is the type as the function writes it.
pub fn build_advice(items: &Items, path: &str, name: &str) -> String {
    let is_it = |t: &TyAst| matches!(t, TyAst::Path(s, _) if s.last().is_some_and(|l| l == name));
    let is_self = |t: &TyAst| matches!(t, TyAst::Path(s, _) if s.last().is_some_and(|l| l == "Self" || l == name));
    // the type itself, or (Milestone 333) a `Result` or an `Option` of it, whose `Err` or
    // `None` throws the input away: ranked after the constructors that always give one
    let returns_it = |r: &Option<TyAst>| -> Option<u8> {
        match r {
            Some(t) if is_self(t) => Some(0),
            Some(TyAst::Path(s, a))
                if s.last().is_some_and(|l| l == "Result" || l == "Option")
                    && a.first().is_some_and(is_self) =>
            {
                Some(10)
            }
            _ => None,
        }
    };
    // a macro's metavariable (`$src`) is no type, and a line that names one does not build
    // (Milestone 333)
    let usable = |t: &TyAst| {
        let x = t.text();
        !(x.contains("Self")
            || x.contains("impl ")
            || x.contains("dyn ")
            || x.contains("&mut")
            || x.contains('$'))
    };
    // another trait of the type with a function `from` (num-traits' `NumCast` beside `From`)
    // makes `Type::from` ambiguous: the line then names the trait (Milestone 333)
    let other_from = items.impls.iter().any(|b| {
        is_it(&b.ty)
            && b.tr.as_ref().is_some_and(|t| trait_name(t) != "From")
            && b.rets.iter().any(|(n, _)| n == "from")
    });
    let mut ctors: Vec<(u8, String)> = Vec::new();
    for b in items.impls.iter().filter(|b| is_it(&b.ty)) {
        match &b.tr {
            None => {
                for (public, sig) in &b.fns {
                    let generic = sig.generics.iter().any(|g| g != "where");
                    // an `unsafe` function is no constructor that a line can name
                    // (Milestone 333)
                    let Some(fallible) = returns_it(&sig.ret) else {
                        continue;
                    };
                    if !*public
                        || generic
                        || sig.is_unsafe
                        || !sig.params.iter().all(|(_, t)| usable(t))
                    {
                        continue;
                    }
                    let rank = fallible
                        + match sig.name.as_str() {
                            "new" => 0,
                            n if n.starts_with("from_") => 1,
                            n if n.starts_with("with_") => 2,
                            _ => 3,
                        };
                    let args: Vec<String> = sig
                        .params
                        .iter()
                        .map(|(n, t)| format!("{n}: {}", t.text()))
                        .collect();
                    ctors.push((rank, format!("{path}::{}({})", sig.name, args.join(", "))));
                }
            }
            Some(tr) if trait_name(tr) == "From" => {
                if let TyAst::Path(_, a) = tr {
                    // a type parameter of the block (`impl<T> From<T> for OrderedFloat<T>`)
                    // is no type that a line can name
                    if let Some(x) = a
                        .first()
                        .filter(|x| usable(x) && !b.generics.iter().any(|g| g == &x.text()))
                    {
                        let line = if other_from {
                            let holes = match &b.ty {
                                TyAst::Path(_, ta) if !ta.is_empty() => {
                                    format!("<{}>", vec!["_"; ta.len()].join(", "))
                                }
                                _ => String::new(),
                            };
                            format!("<{path}{holes} as From<{0}>>::from(v: {0})", x.text())
                        } else {
                            format!("{path}::from(v: {})", x.text())
                        };
                        // a conversion from the same type of other arguments
                        // (`OrderedFloat<f32>`) needs that one built first
                        let rank = if x.text().contains(name) { 12 } else { 4 };
                        ctors.push((rank, line));
                    }
                }
            }
            Some(tr) if trait_name(tr) == "Default" => {
                ctors.push((20, format!("{path}::default()")));
            }
            _ => {}
        }
    }
    if items.defaults.contains(name) {
        ctors.push((20, format!("{path}::default()")));
    }
    ctors.sort();
    ctors.dedup_by(|a, b| a.1 == b.1);
    // a method that reads the value as a type hydo reads
    let readable = |t: &TyAst| -> Option<String> {
        let t = match t {
            TyAst::Ref(false, x) => match &**x {
                TyAst::Path(s, a) if a.is_empty() && s.last().is_some_and(|l| l == "str") => {
                    return Some("String".into())
                }
                TyAst::Slice(e) => return Some(format!("Vec<{}>", e.text())),
                x => x,
            },
            t => t,
        };
        let x = t.text();
        let x = [
            "alloc::vec::",
            "std::vec::",
            "alloc::string::",
            "std::string::",
        ]
        .iter()
        .find_map(|p| x.strip_prefix(p))
        .map_or(x.clone(), str::to_string);
        let plain = [
            "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
            "bool", "f32", "f64", "String",
        ];
        (plain.contains(&x.as_str()) || x.starts_with("Vec<")).then_some(x)
    };
    let mut reads: Vec<(u8, String)> = Vec::new();
    for b in items
        .impls
        .iter()
        .filter(|b| is_it(&b.ty) && b.tr.is_none())
    {
        for (m, r) in &b.getters {
            if let Some(v) = readable(r) {
                let rank = match m.as_str() {
                    n if n.starts_with("to_") => 0,
                    n if n.starts_with("as_") => 1,
                    "get" => 2,
                    _ => 3,
                };
                reads.push((rank, format!("`// hydo: read {name} as {v} by {m}()`")));
            }
        }
    }
    reads.sort();
    if items.reads.contains_key(name) {
        reads.clear();
    }
    let read = match reads.first() {
        Some((_, r)) => format!(", and {r} for a clause that reads the value"),
        None => String::new(),
    };
    match ctors.as_slice() {
        [] => format!(
            "write `// hydo: build {name} from <constructor>(<name>: <type>, ...)` with a public \
             function of the crate that returns a `{name}`; the crate has none that hydo can \
             call, so add one that takes the fields' values, or write the contract on a \
             property function in the crate's test module{read}"
        ),
        [(_, c)] => {
            format!("write `// hydo: build {name} from {c}` (the crate's public constructor){read}")
        }
        [(_, c), rest @ ..] => format!(
            "write `// hydo: build {name} from {c}` (a public constructor of the crate; the \
             others are {}){read}",
            rest.iter()
                .take(3)
                .map(|(_, c)| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `t` with each `Self::<name>` that `f` resolves replaced (Milestone 318).
pub fn subst_assoc(t: &TyAst, f: &dyn Fn(&str) -> Option<TyAst>) -> TyAst {
    match t {
        TyAst::Path(segs, args) if segs.len() == 2 && segs[0] == "Self" && args.is_empty() => {
            f(&segs[1]).unwrap_or_else(|| t.clone())
        }
        TyAst::Path(segs, args) => TyAst::Path(
            segs.clone(),
            args.iter().map(|a| subst_assoc(a, f)).collect(),
        ),
        TyAst::Ref(m, x) => TyAst::Ref(*m, Box::new(subst_assoc(x, f))),
        TyAst::Slice(x) => TyAst::Slice(Box::new(subst_assoc(x, f))),
        TyAst::Array(x, n) => TyAst::Array(Box::new(subst_assoc(x, f)), n.clone()),
        TyAst::Tuple(v) => TyAst::Tuple(v.iter().map(|x| subst_assoc(x, f)).collect()),
        TyAst::Other(o) => TyAst::Other(o.clone()),
    }
}

/// Whether the type `pat` of an `impl` block, whose type parameters are `gens`, matches the
/// type `t` (Milestone 333): each parameter is bound to the part of `t` it stands for, in
/// `map`. `[T; N]` matches `[u8; 4]` with `T = u8` and `N = 4`; a macro's `$size` matches any
/// length.
pub fn unify(pat: &TyAst, t: &TyAst, gens: &[String], map: &mut BTreeMap<String, TyAst>) -> bool {
    match (pat, t) {
        (TyAst::Path(s, a), _) if s.len() == 1 && a.is_empty() && gens.contains(&s[0]) => {
            match map.get(&s[0]) {
                Some(b) => same_ty(b, t),
                None => {
                    map.insert(s[0].clone(), t.clone());
                    true
                }
            }
        }
        (TyAst::Path(s, a), TyAst::Path(u, b)) => {
            s.last() == u.last()
                && a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| unify(x, y, gens, map))
        }
        (TyAst::Ref(m, x), TyAst::Ref(n, y)) => m == n && unify(x, y, gens, map),
        (TyAst::Slice(x), TyAst::Slice(y)) => unify(x, y, gens, map),
        (TyAst::Array(x, n), TyAst::Array(y, k)) => {
            let n = n.trim();
            let len_ok = if gens.iter().any(|g| g == n) {
                map.insert(
                    n.to_string(),
                    TyAst::Path(vec![k.trim().to_string()], vec![]),
                );
                true
            } else {
                n.starts_with('$') || n.replace('_', "") == k.trim().replace('_', "")
            };
            len_ok && unify(x, y, gens, map)
        }
        (TyAst::Tuple(x), TyAst::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| unify(p, q, gens, map))
        }
        (p, t) => same_ty(p, t),
    }
}

/// What the associated type `name` of the type `t` stands for (Milestone 333): `type <name> =
/// <type>;` in an `impl` block of the crate whose type matches `t` (smallvec's `impl<T, const
/// N: usize> Array for [T; N] { type Item = T; }` gives `u8` for `[u8; 4]`), else the standard
/// library's `Item` of a list (`Vec<T>`, `[T; N]`, `[T]`), `Owned` of `str` and of a slice,
/// and `Output` of a number.
pub fn assoc_of(items: &Items, t: &TyAst, name: &str) -> Option<TyAst> {
    assoc_depth(items, t, name, 0)
}

fn assoc_depth(items: &Items, t: &TyAst, name: &str, depth: usize) -> Option<TyAst> {
    if depth > 6 {
        return None;
    }
    let t = match t {
        TyAst::Ref(_, x) => x,
        o => o,
    };
    for b in &items.impls {
        let Some((_, a)) = b.assoc.iter().find(|(n, _)| n == name) else {
            continue;
        };
        let mut map = BTreeMap::new();
        if unify(&b.ty, t, &b.generics, &mut map) {
            let r = subst_ty(a, &map);
            // `type Item = A::Item;` of `impl<A: Array> ... for SmallVec<A>`
            return Some(param_assoc_depth(&r, &map, items, depth + 1));
        }
    }
    let elem = |x: &TyAst| Some(x.clone());
    match (name, t) {
        ("Item", TyAst::Array(x, _) | TyAst::Slice(x)) => elem(x),
        ("Item", TyAst::Path(s, a))
            if a.len() == 1
                && matches!(
                    s.last().map(String::as_str),
                    Some("Vec" | "VecDeque" | "Option")
                ) =>
        {
            elem(&a[0])
        }
        ("Owned", TyAst::Path(s, a)) if a.is_empty() && s.last().is_some_and(|l| l == "str") => {
            Some(TyAst::Path(vec!["String".into()], vec![]))
        }
        ("Owned", TyAst::Slice(x)) => Some(TyAst::Path(vec!["Vec".into()], vec![(**x).clone()])),
        ("Output", TyAst::Path(s, a))
            if a.is_empty()
                && s.len() == 1
                && matches!(
                    s[0].as_str(),
                    "u8" | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "i128"
                        | "isize"
                        | "f32"
                        | "f64"
                ) =>
        {
            Some(t.clone())
        }
        _ => None,
    }
}

/// `t` with each associated type of a type parameter that `s` names replaced by what it stands
/// for (Milestone 333): `A::Item` with `with A = [u8; 4]` is `u8`. One that hydo cannot
/// resolve stays as written.
pub fn subst_param_assoc(t: &TyAst, s: &BTreeMap<String, TyAst>, items: &Items) -> TyAst {
    param_assoc_depth(t, s, items, 0)
}

fn param_assoc_depth(t: &TyAst, s: &BTreeMap<String, TyAst>, items: &Items, d: usize) -> TyAst {
    let r = |x: &TyAst| param_assoc_depth(x, s, items, d);
    match t {
        TyAst::Path(segs, args)
            if segs.len() == 2 && args.is_empty() && s.contains_key(&segs[0]) =>
        {
            assoc_depth(items, &s[&segs[0]], &segs[1], d).unwrap_or_else(|| t.clone())
        }
        TyAst::Path(segs, args) => TyAst::Path(segs.clone(), args.iter().map(r).collect()),
        TyAst::Ref(m, x) => TyAst::Ref(*m, Box::new(r(x))),
        TyAst::Slice(x) => TyAst::Slice(Box::new(r(x))),
        TyAst::Array(x, n) => TyAst::Array(Box::new(r(x)), n.clone()),
        TyAst::Tuple(v) => TyAst::Tuple(v.iter().map(r).collect()),
        TyAst::Other(o) => TyAst::Other(o.clone()),
    }
}

/// Add the `impl` blocks of a source (its text and masked text), with their associated types
/// and their methods' result types (Milestone 318).
fn read_impls(text: &str, m: &[u8], items: &mut Items) {
    let ms = String::from_utf8_lossy(m).to_string();
    let b = ms.as_bytes();
    let id = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut from = 0;
    while let Some(p) = ms[from..].find("impl") {
        let at = from + p;
        from = at + 4;
        if (at > 0 && id(b[at - 1])) || !matches!(b.get(at + 4), Some(b' ' | b'<' | b'\n')) {
            continue;
        }
        // the header up to the block's `{`, outside parentheses and brackets
        let (mut d, mut j) = (0i32, at);
        let open = loop {
            match b.get(j) {
                None | Some(b';') => break None,
                Some(b'(' | b'[') => d += 1,
                Some(b')' | b']') => d -= 1,
                Some(b'{') if d == 0 => break Some(j),
                _ => {}
            }
            j += 1;
        };
        let Some(open) = open else { continue };
        let header = ms[at..open]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let (Some((ty, tr, generics)), Some(close)) = (impl_parts(&header), close_of(m, open))
        else {
            continue;
        };
        let generics = generics
            .iter()
            .map(|g| g.strip_prefix("const ").unwrap_or(g).trim().to_string())
            .collect();
        let mut def = ImplDef {
            ty,
            tr,
            generics,
            assoc: vec![],
            rets: vec![],
            fns: vec![],
            getters: vec![],
        };
        // the items at the block's own depth
        let mut depth = 0;
        for k in open + 1..close {
            match b[k] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ if depth == 0 && !id(b[k - 1]) => {
                    let rest = &ms[k..close];
                    if let Some(r) = rest.strip_prefix("type ") {
                        let (Some(eq), Some(semi)) = (r.find('='), r.find(';')) else {
                            continue;
                        };
                        let name = r[..eq].trim();
                        if eq < semi && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                            let t = r[eq + 1..semi].split_whitespace().collect::<Vec<_>>();
                            if let Ok(t) = parse_ty(&t.join(" ")) {
                                def.assoc.push((name.to_string(), t));
                            }
                        }
                    } else if rest.starts_with("fn ") {
                        if let Ok(mut sig) = signature(text, m, k) {
                            // `pub unsafe fn`: the words before `fn` (Milestone 333)
                            let before = &ms[open + 1..k];
                            let tail = &before[before.rfind([';', '}']).map_or(0, |i| i + 1)..];
                            sig.is_unsafe |=
                                strip_attrs(tail).split_whitespace().any(|w| w == "unsafe");
                            def.rets.push((sig.name.clone(), sig.ret.clone()));
                            let public = is_pub_before(&ms[open + 1..k]);
                            match (sig.receiver, &sig.ret) {
                                (None, _) => def.fns.push((public, sig)),
                                // `&self`, or `self` of a `Copy` type, as half's `to_bits`
                                (Some(Receiver::Ref | Receiver::Value), Some(r))
                                    if public && sig.params.is_empty() =>
                                {
                                    def.getters.push((sig.name.clone(), r.clone()))
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        items.impls.push(def);
    }
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
        // a variant that a feature may leave out (`#[cfg(feature = "hyphenation")]`) is never
        // generated; a value of it reads as not read (Milestone 333)
        if v.contains("#[cfg(") {
            continue;
        }
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
        let def = EnumDef {
            name: name.clone(),
            params,
            variants,
            module,
            is_pub: item_is_pub(&ms[..at]),
        };
        items
            .all_enums
            .entry(name.clone())
            .or_default()
            .push(def.clone());
        items.enums.entry(name).or_insert(def);
    }
    read_impls(text, &m, items);
    // the functions outside `impl` blocks and traits, for their result types (Milestone 333)
    {
        let mut from = 0;
        while let Some(p) = ms[from..].find("fn ") {
            let at = from + p;
            from = at + 3;
            if at > 0 && id(b[at - 1]) {
                continue;
            }
            if enclosing(&m, at)
                .iter()
                .any(|b| impl_parts(&b.header).is_some() || trait_parts(&b.header).is_some())
            {
                continue;
            }
            if let Ok(sig) = signature(text, &m, at) {
                if sig.receiver.is_none() {
                    items
                        .free_fns
                        .entry(sig.name.clone())
                        .or_default()
                        .push(sig.ret);
                }
            }
        }
    }
    // `use crate::duration::Error;` and the like, for the names of the file's module
    {
        let mut from = 0;
        let mut names = BTreeMap::new();
        while let Some(p) = ms[from..].find("use ") {
            let at = from + p;
            from = at + 4;
            if at > 0 && id(b[at - 1]) {
                continue;
            }
            let Some(semi) = ms[at..].find(';') else {
                break;
            };
            let tree = ms[at + 4..at + semi].split_whitespace().collect::<Vec<_>>();
            let mut here = module.to_vec();
            here.extend(inline_modules(&m, at));
            use_tree(&[], &tree.join(" "), &here, &mut names);
            // the same by the inline module the line stands in (Milestone 333)
            use_tree(
                &[],
                &tree.join(" "),
                &here,
                items.uses_exact.entry(here.clone()).or_default(),
            );
        }
        if !names.is_empty() {
            let e = items.uses.entry(module.to_vec()).or_default();
            for (k, v) in names {
                e.entry(k).or_default().extend(v);
            }
        }
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
                // an associated type of an `impl` block or a trait is no alias of the crate:
                // smallvec's `type Item = T::Item;` made `A::Item` read as `T::Item`
                // (Milestone 333)
                if enclosing(&m, at)
                    .iter()
                    .any(|b| impl_parts(&b.header).is_some() || trait_parts(&b.header).is_some())
                {
                    continue;
                }
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
            let mut params = Vec::new();
            let mut lifetimes = false;
            let after = if after.starts_with('<') {
                match matching(after, 0, '<', '>') {
                    Some(e) => {
                        params = generic_names(&after[1..e]);
                        lifetimes = split_top(&after[1..e], ',')
                            .iter()
                            .any(|g| g.trim().starts_with('\''));
                        after[e + 1..].trim_start()
                    }
                    None => continue,
                }
            } else {
                after
            };
            // a structure with lifetime parameters only is no generic one (Milestone 333)
            let generic = !params.is_empty();
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
            // `#[derive(Default)]` before the structure (Milestone 326)
            let attrs = &ms[ms[..at].rfind([';', '}', '{']).map_or(0, |i| i + 1)..at];
            if attrs.contains("derive(") && find_word(attrs, "Default").is_some() {
                items.defaults.insert(name.clone());
            }
            if let Some((fields, public)) = parse_fields(body, tuple) {
                let mut module = module.to_vec();
                module.extend(inline_modules(&m, at));
                let def = StructDef {
                    name: name.clone(),
                    generic,
                    params,
                    lifetimes,
                    tuple,
                    fields,
                    public,
                    module,
                    is_pub: item_is_pub(&ms[..at]),
                };
                items
                    .all_structs
                    .entry(name.clone())
                    .or_default()
                    .push(def.clone());
                items.structs.entry(name).or_insert(def);
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
            FTy::Hidden(why) => Err(why.clone()),
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
                // the refusal adds the `build` line to write (Milestone 326)
                None => Err(format!("hydo cannot generate a value of `{ty}`")),
            },
            _ => Ok(()),
        }
    }

    /// The first part of the type that hydo does not read, as its text (Milestone 333): an
    /// opaque type, a field it cannot name, or a built value without a `read` line, also inside
    /// a list, an option, a tuple or a structure.
    pub fn unread_part(&self) -> Option<String> {
        match self {
            FTy::Opaque(t) => Some(t.clone()),
            FTy::Hidden(_) => Some("a private field".into()),
            FTy::Built { ty, read: None, .. } => Some(ty.clone()),
            FTy::Built {
                read: Some((t, _)), ..
            } => t.unread_part(),
            FTy::List(x) | FTy::Array(x, _) | FTy::Opt(x) | FTy::Boxed(_, x) => x.unread_part(),
            FTy::Res(a, b) => a.unread_part().or_else(|| b.unread_part()),
            FTy::Tuple(v) => v.iter().find_map(FTy::unread_part),
            FTy::Struct { fields, .. } => fields.iter().find_map(|(_, t)| t.unread_part()),
            FTy::Enum { variants, .. } => variants
                .iter()
                .find_map(|v| v.fields.iter().find_map(|(_, t)| t.unread_part())),
            _ => None,
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
    /// The constructor returns a `Result` or an `Option`: the harness takes its `Ok` or `Some`
    /// value, and throws the input away on `Err` or `None` (Milestone 333).
    pub fallible: bool,
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
        // a const generic parameter's value (`[T; CAP]` with `with CAP = 16`; Milestone 318)
        TyAst::Array(x, n) => TyAst::Array(
            Box::new(subst_ty(x, s)),
            s.get(n.trim()).map_or_else(|| n.clone(), TyAst::text),
        ),
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
                .or_else(|| {
                    let v = self.subst.get(n.trim())?.text();
                    v.replace('_', "").parse().ok()
                })
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
            // a type parameter's associated type (`A::Item`; Milestone 333)
            TyAst::Path(segs, args)
                if segs.len() == 2 && args.is_empty() && self.subst.contains_key(&segs[0]) =>
            {
                match assoc_of(self.items, &self.subst[&segs[0]], &segs[1]) {
                    Some(a) if stack.len() < 12 => {
                        stack.push(t.text());
                        let r = self.res(&a, stack);
                        stack.pop();
                        r
                    }
                    _ => FTy::Opaque(t.text()),
                }
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
                    // `Cow<str>` and `Cow<[T]>`: the value they hold (Milestone 333)
                    ("Cow", 1) => FTy::Boxed("Cow".into(), Box::new(arg(0, stack))),
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
                        // two definitions of the name: the one that the path or the function's
                        // module names (Milestone 318)
                        // a name that no item has, or that several have, may reach its item
                        // through a renaming `use`, a re-export or a glob import (Milestone 326)
                        let defs = self.items.all_structs.get(name).map_or(0, Vec::len)
                            + self.items.all_enums.get(name).map_or(0, Vec::len);
                        let picked = self
                            .pick(name, segs)
                            .or_else(|| (defs != 1).then(|| self.through_uses(segs)).flatten());
                        if let Some(real) = picked.as_ref().map(|d| match d {
                            Def::S(s) => s.name.as_str(),
                            Def::E(e) => e.name.as_str(),
                        }) {
                            if real != name
                                && (self.items.builds.contains_key(real)
                                    || self.items.reads.contains_key(real))
                            {
                                return self.built(real, t, stack);
                            }
                        }
                        let (s_def, e_def) = match picked {
                            Some(Def::S(s)) => (Some(s), None),
                            Some(Def::E(e)) => (None, Some(e)),
                            None => (self.items.structs.get(name), self.items.enums.get(name)),
                        };
                        let Some(d) = s_def.filter(|d| n == 0 || d.params.len() == n) else {
                            if let Some(e) = e_def {
                                return self.enumeration(e, segs, args, t, stack);
                            }
                            if name == "Ordering" && n == 0 && !segs.iter().any(|s| s == "atomic") {
                                return ordering();
                            }
                            return FTy::Opaque(t.text());
                        };
                        let view = self.structure(d, segs, args, t, stack);
                        // a structure with a lifetime parameter or a reference field (textwrap's
                        // `Options<'a>`): read field by field, never built so, since a generated
                        // field owns its value (Milestone 333)
                        let refs = d.fields.iter().any(|(_, f)| matches!(f, TyAst::Ref(..)));
                        if matches!(view, FTy::Struct { .. }) && (d.lifetimes || refs) {
                            return FTy::Built {
                                ty: subst_ty(t, self.subst).text(),
                                build: None,
                                read: Some((Box::new(view), String::new())),
                            };
                        }
                        view
                    }
                }
            }
        }
    }

    /// A structure of the crate field by field: each field that the harness's module can name
    /// with its type, and the others hidden. A generic structure's type parameters take the
    /// type's arguments when every field is `pub` (ordered-float's `OrderedFloat<T>(pub T)`
    /// with `with T = f64`; Milestone 333), and the path names them (`OrderedFloat::<f64>`);
    /// one with a private field stays unread, so that its constructor keeps its invariant.
    fn structure(
        &self,
        d: &StructDef,
        segs: &[String],
        args: &[TyAst],
        t: &TyAst,
        stack: &mut Vec<String>,
    ) -> FTy {
        let name = d.name.as_str();
        if stack.iter().any(|s| s == name) {
            return FTy::Opaque(t.text());
        }
        if d.generic
            && (d.params.len() != args.len()
                || d.params.iter().any(|p| p.starts_with("const "))
                || !d.public.iter().all(|p| *p))
        {
            return FTy::Opaque(t.text());
        }
        let Some(path) = self.written(segs, &d.module, name, d.is_pub) else {
            return FTy::Opaque(t.text());
        };
        let targs: Vec<TyAst> = args.iter().map(|a| subst_ty(a, self.subst)).collect();
        let map: BTreeMap<String, TyAst> = d.params.iter().cloned().zip(targs.clone()).collect();
        let inner = Resolver {
            items: self.items,
            self_ty: self.self_ty,
            module: self.module,
            subst: &map,
        };
        let fres: &Resolver = if d.generic { &inner } else { self };
        let path = if d.generic {
            format!(
                "{path}::<{}>",
                targs.iter().map(TyAst::text).collect::<Vec<_>>().join(", ")
            )
        } else {
            path
        };
        stack.push(name.into());
        let seen = self.module.starts_with(&d.module);
        let fields = d
            .fields
            .iter()
            .zip(d.public.iter().chain(std::iter::repeat(&true)))
            .map(|((n, ft), &public)| {
                if public || seen {
                    return (n.clone(), fres.res(ft, stack));
                }
                let m = if d.module.is_empty() {
                    "the crate's root".to_string()
                } else {
                    format!("`{}`", d.module.join("::"))
                };
                let why = format!(
                    "the field `{n}` of `{name}` is private to the module {m}, so the harness \
                     cannot build the value: {}",
                    build_advice(self.items, &segs.join("::"), name)
                );
                (n.clone(), FTy::Hidden(why))
            })
            .collect();
        stack.pop();
        FTy::Struct {
            path,
            tuple: d.tuple,
            fields,
        }
    }

    /// How the harness writes a type of the crate that is written as `segs` and defined in
    /// `module` (Milestone 333): as written when the function's module names it so, else by its
    /// path from the crate's root. semver's `Error` has a field of `ErrorKind`, which the
    /// private module `error` defines and the root does not import, so the harness writes
    /// `crate::error::ErrorKind`. None when the harness cannot name it at all: a private item of
    /// a module that is not the function's or one of its ancestors.
    fn written(
        &self,
        segs: &[String],
        module: &[String],
        name: &str,
        is_pub: bool,
    ) -> Option<String> {
        if !is_pub && !self.module.starts_with(module) {
            return None;
        }
        if segs.len() > 1 || module == self.module {
            return Some(segs.join("::"));
        }
        let imported = self.items.uses_exact.get(self.module).is_some_and(|u| {
            // a `use` of the item itself, or a glob import of its module
            u.get(name).is_some_and(|ps| {
                ps.iter().any(|p| {
                    p.split_last()
                        .is_some_and(|(l, m)| l == name && m == module)
                })
            }) || u.get("*").is_some_and(|gs| gs.iter().any(|g| g == module))
        });
        if imported {
            return Some(segs.join("::"));
        }
        Some(
            ["crate".to_string()]
                .into_iter()
                .chain(module.iter().cloned())
                .chain([name.to_string()])
                .collect::<Vec<_>>()
                .join("::"),
        )
    }

    /// The modules where a type written as `segs` may be defined, the likeliest first
    /// (Milestone 318): for `a::b::Name` the path from `crate`, `self`, `super`, the function's
    /// module, a `use` line of its file, or the crate's root; for `Name` the function's module,
    /// then the `use` lines of its file.
    pub fn modules_of(&self, segs: &[String]) -> Vec<Vec<String>> {
        let cur = self.module.to_vec();
        // the `use` lines of the function's module, else of the nearest enclosing one
        let uses = (0..=cur.len())
            .rev()
            .find_map(|k| self.items.uses.get(&cur[..k]));
        let imported = |n: &str| -> Vec<Vec<String>> {
            uses.and_then(|u| u.get(n)).cloned().unwrap_or_default()
        };
        let mut out: Vec<Vec<String>> = Vec::new();
        let n = segs.len();
        if n <= 1 {
            out.push(cur.clone());
            for p in imported(segs.first().map_or("", String::as_str)) {
                out.push(p[..p.len().saturating_sub(1)].to_vec());
            }
            return out;
        }
        let prefix = &segs[..n - 1];
        match prefix[0].as_str() {
            "crate" => out.push(prefix[1..].to_vec()),
            "self" => out.push(cur.iter().chain(&prefix[1..]).cloned().collect()),
            "super" => {
                let mut m = cur.clone();
                let mut i = 0;
                while prefix.get(i).is_some_and(|s| s == "super") {
                    m.pop();
                    i += 1;
                }
                m.extend(prefix[i..].iter().cloned());
                out.push(m);
            }
            first => {
                out.push(cur.iter().chain(prefix).cloned().collect());
                for p in imported(first) {
                    out.push(p.into_iter().chain(prefix[1..].iter().cloned()).collect());
                }
                out.push(prefix.to_vec());
            }
        }
        out
    }

    /// The definition that a type's name stands for when the crate defines the name more than
    /// once (Milestone 318); none when it defines it once, or when no module fits.
    fn pick(&self, name: &str, segs: &[String]) -> Option<Def<'_>> {
        let ss = self.items.all_structs.get(name).map_or(&[][..], |v| &v[..]);
        let es = self.items.all_enums.get(name).map_or(&[][..], |v| &v[..]);
        if ss.len() + es.len() < 2 {
            return None;
        }
        for m in self.modules_of(segs) {
            if let Some(s) = ss.iter().find(|d| d.module == m) {
                return Some(Def::S(s));
            }
            if let Some(e) = es.iter().find(|d| d.module == m) {
                return Some(Def::E(e));
            }
        }
        None
    }

    /// The definition that a type's name reaches through the crate's `use` lines (Milestone
    /// 326): a renaming import or re-export (`use a::B as C`, `pub use a::B as C`), a
    /// re-export that a path names (`m::C` where `m` has `pub use a::B as C`), and a glob
    /// import (`use a::*`), followed a few steps from the modules where the name may be.
    fn through_uses(&self, segs: &[String]) -> Option<Def<'_>> {
        let name = segs.last()?;
        let mut todo: std::collections::VecDeque<(Vec<String>, String)> = self
            .modules_of(segs)
            .into_iter()
            .map(|m| (m, name.clone()))
            .collect();
        let mut seen: Vec<(Vec<String>, String)> = Vec::new();
        while let Some((m, n)) = todo.pop_front() {
            if seen.contains(&(m.clone(), n.clone())) || seen.len() > 64 {
                continue;
            }
            seen.push((m.clone(), n.clone()));
            let s = self.items.all_structs.get(&n).into_iter().flatten();
            if let Some(d) = s.into_iter().find(|d| d.module == m) {
                return Some(Def::S(d));
            }
            let e = self.items.all_enums.get(&n).into_iter().flatten();
            if let Some(d) = e.into_iter().find(|d| d.module == m) {
                return Some(Def::E(d));
            }
            // the `use` lines of the module, else of the nearest module that encloses it
            let Some(u) = (0..=m.len())
                .rev()
                .find_map(|k| self.items.uses.get(&m[..k]))
            else {
                continue;
            };
            for p in u.get(&n).into_iter().flatten() {
                if let Some((last, module)) = p.split_last() {
                    todo.push_back((module.to_vec(), last.clone()));
                }
            }
            for g in u.get("*").into_iter().flatten() {
                todo.push_back((g.clone(), n.clone()));
            }
        }
        None
    }

    /// A type that a `build` or `read` line names (Milestone 307).
    fn built(&self, name: &str, t: &TyAst, stack: &mut Vec<String>) -> FTy {
        if stack.iter().any(|s| s == name) {
            return FTy::Opaque(t.text());
        }
        stack.push(name.into());
        let concrete = subst_ty(
            match t {
                TyAst::Ref(_, x) => x,
                o => o,
            },
            self.subst,
        );
        // the path that the harness's module names the type by: base64's `engine` module
        // does not import `Alphabet`, which `crate::alphabet::Alphabet` names (Milestone 333)
        let abs = self.items.structs.get(name).and_then(|d| {
            self.written(&[name.to_string()], &d.module, name, d.is_pub)
                .filter(|a| a != name)
                .map(|a| (name.to_string(), a))
        });
        let build = self.items.builds.get(name).map(|b| {
            // the constructor's `impl` block's type parameters bound to the type's arguments:
            // `NotNan::new(val: T)` of `impl<T> NotNan<T>` takes an `f64` for `NotNan<f64>`
            let mut map = BTreeMap::new();
            let fname = path_segments(&b.ctor).last().cloned().unwrap_or_default();
            for i in &self.items.impls {
                if type_key(&i.ty.text()) == name
                    && i.fns.iter().any(|(_, f)| f.name == fname)
                    && unify(&i.ty, &concrete, &i.generics, &mut map)
                {
                    break;
                }
                map.clear();
            }
            let path = match &abs {
                Some((w, a)) if b.ctor.starts_with(&format!("{w}::")) => {
                    format!("{a}{}", &b.ctor[w.len()..])
                }
                _ => b.ctor.clone(),
            };
            Ctor {
                fallible: b.fallible,
                path,
                args: b
                    .args
                    .iter()
                    .map(|(n, a)| {
                        let a = subst_ty(a, &map);
                        (n.clone(), self.res(&a, stack), pass_of(&a))
                    })
                    .collect(),
            }
        });
        let read = self
            .items
            .reads
            .get(name)
            .map(|(v, m)| (Box::new(self.res(v, stack)), m.clone()));
        stack.pop();
        // without a `read` line, the fields that the harness's module can name (semver's
        // `Version { major, minor, patch, .. }`; Milestone 333)
        let read = read.or_else(|| {
            let d = self.items.structs.get(name)?;
            let (segs, args) = match t {
                TyAst::Path(s, a) => (s.clone(), a.clone()),
                TyAst::Ref(_, x) => match &**x {
                    TyAst::Path(s, a) => (s.clone(), a.clone()),
                    _ => return None,
                },
                _ => return None,
            };
            let v = self.structure(d, &segs, &args, t, stack);
            match &v {
                FTy::Struct { fields, .. }
                    if fields
                        .iter()
                        .any(|(_, f)| !matches!(f, FTy::Hidden(_) | FTy::Opaque(_))) =>
                {
                    Some((Box::new(v), String::new()))
                }
                _ => None,
            }
        });
        // the `with` lines' types in the type's text (`OrderedFloat<T>` with `with T = f64`;
        // Milestone 333)
        let ty = concrete.text();
        let ty = match &abs {
            Some((w, a)) if ty == *w || ty.starts_with(&format!("{w}<")) => {
                format!("{a}{}", &ty[w.len()..])
            }
            _ => ty,
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
        let Some(path) = self.written(segs, &e.module, &e.name, e.is_pub) else {
            return FTy::Opaque(t.text());
        };
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
            path,
            args,
            variants,
        }
    }
}

/// A definition of a name: a structure or an enumeration (Milestone 318).
enum Def<'a> {
    S(&'a StructDef),
    E(&'a EnumDef),
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
