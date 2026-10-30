//! Does Python's `re.compile` accept a pattern? hydo-build checks the `errors:` regular expressions
//! of a manifest with it, and prints its message. This is a port of the checks of Python's regex
//! parser (`re/_parser.py`, CPython 3.11 to 3.15): groups, escapes, character sets, repeats,
//! extensions and inline flags, with its messages and positions ("<msg> at position <n>").
//! Not checked here: `\N{name}` (any name is accepted), repeat counts above the engine's limit,
//! and the checks Python makes only when it compiles the parsed pattern (look-behind width).

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    At,
    Repeat,
    Other,
}

struct Src {
    s: Vec<char>,
    index: usize,
    next: Option<String>,
}

struct State {
    groups: usize,
    open: Vec<usize>,
    names: Vec<(String, usize)>,
    grouprefpos: Vec<(usize, usize)>,
    verbose: bool,
}

type E = String;

fn at(msg: &str, pos: usize) -> E {
    format!("{msg} at position {pos}")
}

impl Src {
    fn new(p: &str) -> Result<Src, E> {
        let mut s = Src { s: p.chars().collect(), index: 0, next: None };
        s.advance()?;
        Ok(s)
    }

    fn advance(&mut self) -> Result<(), E> {
        let mut i = self.index;
        let Some(&c) = self.s.get(i) else {
            self.next = None;
            return Ok(());
        };
        let mut t = c.to_string();
        if c == '\\' {
            i += 1;
            match self.s.get(i) {
                Some(&d) => t.push(d),
                None => return Err(at("bad escape (end of pattern)", self.s.len() - 1)),
            }
        }
        self.index = i + 1;
        self.next = Some(t);
        Ok(())
    }

    fn matches(&mut self, c: &str) -> Result<bool, E> {
        if self.next.as_deref() == Some(c) {
            self.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn get(&mut self) -> Result<Option<String>, E> {
        let t = self.next.clone();
        self.advance()?;
        Ok(t)
    }

    fn tell(&self) -> usize {
        self.index - self.next.as_ref().map(|x| x.chars().count()).unwrap_or(0)
    }

    fn seek(&mut self, i: usize) -> Result<(), E> {
        self.index = i;
        self.advance()
    }

    fn error(&self, msg: &str, offset: usize) -> E {
        at(msg, self.tell().saturating_sub(offset))
    }

    fn getwhile(&mut self, n: usize, set: &str) -> Result<String, E> {
        let mut r = String::new();
        for _ in 0..n {
            match &self.next {
                Some(c) if c.chars().count() == 1 && set.contains(c.as_str()) => {
                    r.push_str(c);
                    self.advance()?;
                }
                _ => break,
            }
        }
        Ok(r)
    }

    fn getuntil(&mut self, term: &str, name: &str) -> Result<String, E> {
        let mut r = String::new();
        loop {
            let c = self.next.clone();
            self.advance()?;
            match c {
                None => {
                    if r.is_empty() {
                        return Err(self.error(&format!("missing {name}"), 0));
                    }
                    return Err(self.error(&format!("missing {term}, unterminated name"), r.chars().count()));
                }
                Some(c) if c == term => {
                    if r.is_empty() {
                        return Err(self.error(&format!("missing {name}"), 1));
                    }
                    break;
                }
                Some(c) => r.push_str(&c),
            }
        }
        Ok(r)
    }

    fn checkgroupname(&self, name: &str, offset: usize) -> Result<(), E> {
        let mut cs = name.chars();
        let ok = match cs.next() {
            Some(c) if c == '_' || c.is_alphabetic() => cs.all(|c| c == '_' || c.is_alphanumeric()),
            _ => false,
        };
        if !ok {
            return Err(self.error(
                &format!("bad character in group name {}", crate::py::repr_str(name)),
                name.chars().count() + offset,
            ));
        }
        Ok(())
    }
}

const HEX: &str = "0123456789abcdefABCDEF";
const OCT: &str = "01234567";
const DIGITS: &str = "0123456789";

fn is_ascii_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

enum Esc {
    Lit(u32),
    Class,
    At,
    Ref,
}

fn hexval(s: &str) -> u32 {
    u32::from_str_radix(s, 16).unwrap_or(0)
}

/// The \x \u \U \N escapes, shared by both contexts; None when `c` is not one of them.
fn num_escape(src: &mut Src, esc: &mut String, c: char) -> Result<Option<u32>, E> {
    let (n, total) = match c {
        'x' => (2, 4),
        'u' => (4, 6),
        'U' => (8, 10),
        'N' => {
            if !src.matches("{")? {
                return Err(src.error("missing {", 0));
            }
            src.getuntil("}", "character name")?;
            return Ok(Some(0));
        }
        _ => return Ok(None),
    };
    esc.push_str(&src.getwhile(n, HEX)?);
    if esc.chars().count() != total {
        return Err(src.error(&format!("incomplete escape {esc}"), esc.chars().count()));
    }
    let v = hexval(&esc[2..]);
    if c == 'U' && char::from_u32(v).is_none() && v > 0x10ffff {
        return Err(src.error(&format!("bad escape {esc}"), esc.chars().count()));
    }
    Ok(Some(v))
}

fn class_escape(src: &mut Src, escape: &str) -> Result<Esc, E> {
    let c = escape.chars().nth(1).unwrap_or('\\');
    if matches!(c, 'a' | 'b' | 'f' | 'n' | 'r' | 't' | 'v' | '\\') {
        return Ok(Esc::Lit(c as u32));
    }
    if matches!(c, 'd' | 'D' | 's' | 'S' | 'w' | 'W') {
        return Ok(Esc::Class);
    }
    let mut esc = escape.to_string();
    if let Some(v) = num_escape(src, &mut esc, c)? {
        return Ok(Esc::Lit(v));
    }
    if OCT.contains(c) {
        esc.push_str(&src.getwhile(2, OCT)?);
        let v = u32::from_str_radix(&esc[1..], 8).unwrap_or(0);
        if v > 0o377 {
            return Err(src.error(
                &format!("octal escape value {esc} outside of range 0-0o377"),
                esc.chars().count(),
            ));
        }
        return Ok(Esc::Lit(v));
    }
    if DIGITS.contains(c) || is_ascii_letter(c) {
        return Err(src.error(&format!("bad escape {esc}"), esc.chars().count()));
    }
    Ok(Esc::Lit(c as u32))
}

fn escape(src: &mut Src, escape: &str, st: &State) -> Result<Esc, E> {
    let c = escape.chars().nth(1).unwrap_or('\\');
    if matches!(c, 'A' | 'Z' | 'b' | 'B') {
        return Ok(Esc::At);
    }
    if matches!(c, 'd' | 'D' | 's' | 'S' | 'w' | 'W') {
        return Ok(Esc::Class);
    }
    if matches!(c, 'a' | 'f' | 'n' | 'r' | 't' | 'v' | '\\') {
        return Ok(Esc::Lit(c as u32));
    }
    let mut esc = escape.to_string();
    if let Some(v) = num_escape(src, &mut esc, c)? {
        return Ok(Esc::Lit(v));
    }
    if c == '0' {
        esc.push_str(&src.getwhile(2, OCT)?);
        return Ok(Esc::Lit(u32::from_str_radix(&esc[1..], 8).unwrap_or(0)));
    }
    if DIGITS.contains(c) {
        if src.next.as_deref().is_some_and(|n| n.len() == 1 && DIGITS.contains(n)) {
            esc.push_str(&src.get()?.unwrap_or_default());
            let cs: Vec<char> = esc.chars().collect();
            if OCT.contains(cs[1])
                && OCT.contains(cs[2])
                && src.next.as_deref().is_some_and(|n| n.len() == 1 && OCT.contains(n))
            {
                esc.push_str(&src.get()?.unwrap_or_default());
                let v = u32::from_str_radix(&esc[1..], 8).unwrap_or(0);
                if v > 0o377 {
                    return Err(src.error(
                        &format!("octal escape value {esc} outside of range 0-0o377"),
                        esc.chars().count(),
                    ));
                }
                return Ok(Esc::Lit(v));
            }
        }
        let group: usize = esc[1..].parse().unwrap_or(usize::MAX);
        if group < st.groups {
            if st.open.contains(&group) {
                return Err(src.error("cannot refer to an open group", esc.chars().count()));
            }
            return Ok(Esc::Ref);
        }
        return Err(src.error(&format!("invalid group reference {group}"), esc.chars().count() - 1));
    }
    if is_ascii_letter(c) {
        return Err(src.error(&format!("bad escape {esc}"), esc.chars().count()));
    }
    Ok(Esc::Lit(c as u32))
}

fn parse_sub(src: &mut Src, st: &mut State, nested: usize) -> Result<(), E> {
    let mut first_alt = true;
    loop {
        parse(src, st, nested + 1, nested == 0 && first_alt)?;
        first_alt = false;
        if !src.matches("|")? {
            break;
        }
    }
    Ok(())
}

const FLAGS: &str = "aiLmsux";

/// Returns Some(true) for global flags `(?x)` (the group is done), Some(false) for scoped flags.
fn parse_flags(src: &mut Src, st: &mut State, first: char) -> Result<(bool, bool), E> {
    let mut ch = first.to_string();
    let mut add = String::new();
    let mut del = String::new();
    if ch != "-" {
        loop {
            if ch == "L" {
                return Err(src.error("bad inline flags: cannot use 'L' flag with a str pattern", 0));
            }
            add.push_str(&ch);
            let types = add.chars().filter(|c| "auL".contains(*c)).collect::<std::collections::BTreeSet<_>>();
            if types.len() > 1 {
                return Err(src.error("bad inline flags: flags 'a', 'u' and 'L' are incompatible", 0));
            }
            let Some(c) = src.get()? else {
                return Err(src.error("missing -, : or )", 0));
            };
            ch = c;
            if ch == ")" || ch == "-" || ch == ":" {
                break;
            }
            if !(ch.chars().count() == 1 && FLAGS.contains(ch.as_str())) {
                let alpha = ch.chars().all(|c| c.is_alphabetic());
                return Err(src.error(if alpha { "unknown flag" } else { "missing -, : or )" }, ch.chars().count()));
            }
        }
    }
    if ch == ")" {
        if add.contains('x') {
            st.verbose = true;
        }
        return Ok((true, false));
    }
    if ch == "-" {
        let Some(c) = src.get()? else {
            return Err(src.error("missing flag", 0));
        };
        ch = c;
        if !(ch.chars().count() == 1 && FLAGS.contains(ch.as_str())) {
            let alpha = ch.chars().all(|c| c.is_alphabetic());
            return Err(src.error(if alpha { "unknown flag" } else { "missing flag" }, ch.chars().count()));
        }
        loop {
            if "auL".contains(ch.as_str()) {
                return Err(src.error("bad inline flags: cannot turn off flags 'a', 'u' and 'L'", 0));
            }
            del.push_str(&ch);
            let Some(c) = src.get()? else {
                return Err(src.error("missing :", 0));
            };
            ch = c;
            if ch == ":" {
                break;
            }
            if !(ch.chars().count() == 1 && FLAGS.contains(ch.as_str())) {
                let alpha = ch.chars().all(|c| c.is_alphabetic());
                return Err(src.error(if alpha { "unknown flag" } else { "missing :" }, ch.chars().count()));
            }
        }
    }
    if add.chars().any(|c| del.contains(c)) {
        return Err(src.error("bad inline flags: flag turned on and off", 1));
    }
    Ok((false, add.contains('x') && !del.contains('x')))
}

fn parse(src: &mut Src, st: &mut State, nested: usize, first: bool) -> Result<(), E> {
    let mut last: Option<Kind> = None;
    let mut any = false;
    loop {
        let Some(this) = src.next.clone() else { break };
        if this == "|" || this == ")" {
            break;
        }
        src.get()?;
        if st.verbose {
            if this.chars().count() == 1 && " \t\n\r\x0b\x0c".contains(this.as_str()) {
                continue;
            }
            if this == "#" {
                loop {
                    match src.get()? {
                        None => break,
                        Some(x) if x == "\n" => break,
                        _ => {}
                    }
                }
                continue;
            }
        }
        any = true;
        if this.starts_with('\\') {
            last = Some(match escape(src, &this, st)? {
                Esc::At => Kind::At,
                _ => Kind::Other,
            });
            continue;
        }
        let c = this.chars().next().unwrap_or(' ');
        if !".\\[{()*+?^$|".contains(c) {
            last = Some(Kind::Other);
            continue;
        }
        match c {
            '[' => {
                let here = src.tell() - 1;
                src.matches("^")?;
                let mut set_n = 0usize;
                loop {
                    let Some(this) = src.get()? else {
                        return Err(src.error("unterminated character set", src.tell() - here));
                    };
                    if this == "]" && set_n > 0 {
                        break;
                    }
                    let code1 = if this.starts_with('\\') {
                        class_escape(src, &this)?
                    } else {
                        Esc::Lit(this.chars().next().unwrap_or(' ') as u32)
                    };
                    if src.matches("-")? {
                        let Some(that) = src.get()? else {
                            return Err(src.error("unterminated character set", src.tell() - here));
                        };
                        if that == "]" {
                            let _ = set_n;
                            break;
                        }
                        let code2 = if that.starts_with('\\') {
                            class_escape(src, &that)?
                        } else {
                            Esc::Lit(that.chars().next().unwrap_or(' ') as u32)
                        };
                        let msg = format!("bad character range {this}-{that}");
                        let off = this.chars().count() + 1 + that.chars().count();
                        match (code1, code2) {
                            (Esc::Lit(lo), Esc::Lit(hi)) => {
                                if hi < lo {
                                    return Err(src.error(&msg, off));
                                }
                            }
                            _ => return Err(src.error(&msg, off)),
                        }
                        set_n += 1;
                    } else {
                        set_n += 1;
                    }
                }
                last = Some(Kind::Other);
            }
            '?' | '*' | '+' | '{' => {
                let here = src.tell();
                if c == '{' {
                    if src.next.as_deref() == Some("}") {
                        last = Some(Kind::Other);
                        continue;
                    }
                    let mut lo = String::new();
                    let mut hi = String::new();
                    while src.next.as_deref().is_some_and(|n| n.len() == 1 && DIGITS.contains(n)) {
                        lo.push_str(&src.get()?.unwrap_or_default());
                    }
                    if src.matches(",")? {
                        while src.next.as_deref().is_some_and(|n| n.len() == 1 && DIGITS.contains(n)) {
                            hi.push_str(&src.get()?.unwrap_or_default());
                        }
                    } else {
                        hi = lo.clone();
                    }
                    if !src.matches("}")? {
                        src.seek(here)?;
                        last = Some(Kind::Other);
                        continue;
                    }
                    let min: u128 = lo.parse().unwrap_or(0);
                    if !hi.is_empty() {
                        let max: u128 = hi.parse().unwrap_or(0);
                        if max < min {
                            return Err(src.error("min repeat greater than max repeat", src.tell() - here));
                        }
                    }
                }
                let back = src.tell() - here + this.chars().count();
                match last {
                    None | Some(Kind::At) => return Err(src.error("nothing to repeat", back)),
                    Some(Kind::Repeat) => return Err(src.error("multiple repeat", back)),
                    _ => {}
                }
                if !src.matches("?")? {
                    src.matches("+")?;
                }
                last = Some(Kind::Repeat);
            }
            '.' => last = Some(Kind::Other),
            '^' | '$' => last = Some(Kind::At),
            '(' => {
                let start = src.tell() - 1;
                let mut capture = true;
                let mut name: Option<String> = None;
                let mut sub_verbose = st.verbose;
                if src.matches("?")? {
                    let Some(ch) = src.get()? else {
                        return Err(src.error("unexpected end of pattern", 0));
                    };
                    match ch.as_str() {
                        "P" => {
                            if src.matches("<")? {
                                let n = src.getuntil(">", "group name")?;
                                src.checkgroupname(&n, 1)?;
                                name = Some(n);
                            } else if src.matches("=")? {
                                let n = src.getuntil(")", "group name")?;
                                src.checkgroupname(&n, 1)?;
                                let Some(gid) = st.names.iter().find(|(k, _)| *k == n).map(|(_, g)| *g) else {
                                    return Err(src.error(
                                        &format!("unknown group name {}", crate::py::repr_str(&n)),
                                        n.chars().count() + 1,
                                    ));
                                };
                                if st.open.contains(&gid) {
                                    return Err(src.error("cannot refer to an open group", n.chars().count() + 1));
                                }
                                last = Some(Kind::Other);
                                continue;
                            } else {
                                let Some(ch2) = src.get()? else {
                                    return Err(src.error("unexpected end of pattern", 0));
                                };
                                return Err(src.error(&format!("unknown extension ?P{ch2}"), ch2.chars().count() + 2));
                            }
                        }
                        ":" => capture = false,
                        "#" => {
                            loop {
                                if src.next.is_none() {
                                    return Err(src.error("missing ), unterminated comment", src.tell() - start));
                                }
                                if src.get()?.as_deref() == Some(")") {
                                    break;
                                }
                            }
                            continue;
                        }
                        "=" | "!" | "<" => {
                            if ch == "<" {
                                let Some(ch2) = src.get()? else {
                                    return Err(src.error("unexpected end of pattern", 0));
                                };
                                if ch2 != "=" && ch2 != "!" {
                                    return Err(src.error(&format!("unknown extension ?<{ch2}"), ch2.chars().count() + 2));
                                }
                            }
                            parse_sub(src, st, nested + 1)?;
                            if !src.matches(")")? {
                                return Err(src.error("missing ), unterminated subpattern", src.tell() - start));
                            }
                            last = Some(Kind::Other);
                            continue;
                        }
                        "(" => {
                            let cond = src.getuntil(")", "group name")?;
                            if !(cond.chars().all(|c| c.is_ascii_digit())) {
                                src.checkgroupname(&cond, 1)?;
                                if !st.names.iter().any(|(k, _)| *k == cond) {
                                    return Err(src.error(
                                        &format!("unknown group name {}", crate::py::repr_str(&cond)),
                                        cond.chars().count() + 1,
                                    ));
                                }
                            } else {
                                let g: u128 = cond.parse().unwrap_or(0);
                                if g == 0 {
                                    return Err(src.error("bad group number", cond.chars().count() + 1));
                                }
                                if g >= 1 << 30 {
                                    return Err(src.error(&format!("invalid group reference {g}"), cond.chars().count() + 1));
                                }
                                let pos = src.tell() - cond.chars().count() - 1;
                                if !st.grouprefpos.iter().any(|(k, _)| *k as u128 == g) {
                                    st.grouprefpos.push((g as usize, pos));
                                }
                            }
                            parse(src, st, nested + 1, false)?;
                            if src.matches("|")? {
                                parse(src, st, nested + 1, false)?;
                                if src.next.as_deref() == Some("|") {
                                    return Err(src.error("conditional backref with more than two branches", 0));
                                }
                            }
                            if !src.matches(")")? {
                                return Err(src.error("missing ), unterminated subpattern", src.tell() - start));
                            }
                            last = Some(Kind::Other);
                            continue;
                        }
                        ">" => capture = false,
                        x if (x.chars().count() == 1 && FLAGS.contains(x)) || x == "-" => {
                            let (global, verbose_on) = parse_flags(src, st, x.chars().next().unwrap_or('-'))?;
                            if global {
                                if !first || any_before(&last) {
                                    return Err(src.error(
                                        "global flags not at the start of the expression",
                                        src.tell() - start,
                                    ));
                                }
                                continue;
                            }
                            sub_verbose = sub_verbose || verbose_on;
                            capture = false;
                        }
                        other => {
                            return Err(src.error(&format!("unknown extension ?{other}"), other.chars().count() + 1));
                        }
                    }
                }
                let group = if capture {
                    let g = st.groups;
                    st.groups += 1;
                    if let Some(n) = &name {
                        if let Some((_, og)) = st.names.iter().find(|(k, _)| k == n) {
                            let msg = format!(
                                "redefinition of group name {} as group {}; was group {}",
                                crate::py::repr_str(n),
                                g,
                                og
                            );
                            return Err(src.error(&msg, n.chars().count() + 1));
                        }
                        st.names.push((n.clone(), g));
                    }
                    st.open.push(g);
                    Some(g)
                } else {
                    None
                };
                let saved = st.verbose;
                st.verbose = sub_verbose;
                parse_sub(src, st, nested + 1)?;
                st.verbose = saved;
                if !src.matches(")")? {
                    return Err(src.error("missing ), unterminated subpattern", src.tell() - start));
                }
                if let Some(g) = group {
                    st.open.retain(|x| *x != g);
                }
                last = Some(Kind::Other);
            }
            _ => last = Some(Kind::Other),
        }
    }
    let _ = any;
    Ok(())
}

fn any_before(last: &Option<Kind>) -> bool {
    last.is_some()
}

/// Ok when Python's `re.compile(p)` accepts p; else the text of its `re.error`.
pub fn check(p: &str) -> Result<(), String> {
    let mut src = Src::new(p)?;
    let mut st = State { groups: 1, open: Vec::new(), names: Vec::new(), grouprefpos: Vec::new(), verbose: false };
    parse_sub(&mut src, &mut st, 0)?;
    if src.next.is_some() {
        return Err(src.error("unbalanced parenthesis", 0));
    }
    for (g, pos) in &st.grouprefpos {
        if *g >= st.groups {
            return Err(at(&format!("invalid group reference {g}"), *pos));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn accepts() {
        for p in [
            "abc",
            r"^\d+:[XCSM] [^#]* # ",
            r"(a|b)*c+?",
            r"(?P<x>a)(?P=x)",
            r"(?i)abc",
            r"(?:a)*",
            r"[\]a-z-]",
            r"\(\)",
            r"a{2,3}",
            r"a{,3}",
            r"x{",
            r"(?=a)b",
            r"(?<!a)b",
            r"(a)\1",
            r"(?#c)a",
            r"(?i:a)",
        ] {
            assert!(check(p).is_ok(), "{p}: {:?}", check(p));
        }
    }

    #[test]
    fn refuses() {
        assert_eq!(check("(a").unwrap_err(), "missing ), unterminated subpattern at position 0");
        assert_eq!(check("a)").unwrap_err(), "unbalanced parenthesis at position 1");
        assert_eq!(check("*a").unwrap_err(), "nothing to repeat at position 0");
        assert_eq!(check("a**").unwrap_err(), "multiple repeat at position 2");
        assert_eq!(check("[a").unwrap_err(), "unterminated character set at position 0");
        assert_eq!(check(r"\q").unwrap_err(), r"bad escape \q at position 0");
        assert_eq!(check("[z-a]").unwrap_err(), "bad character range z-a at position 1");
        assert_eq!(check("(?<x>a)").unwrap_err(), "unknown extension ?<x at position 1");
        assert_eq!(check(r"\1").unwrap_err(), "invalid group reference 1 at position 1");
        assert_eq!(check("a\\").unwrap_err(), "bad escape (end of pattern) at position 1");
        assert_eq!(check("a{3,2}").unwrap_err(), "min repeat greater than max repeat at position 2");
        assert_eq!(check("^*").unwrap_err(), "nothing to repeat at position 1");
    }
}
