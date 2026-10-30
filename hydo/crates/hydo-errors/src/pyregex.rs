//! A known-list entry's `match:` is a Python regular expression (`re.search` on a template). Its
//! syntax is checked with Python's own rules and messages (`hydo_manifest::rx`), then the pattern
//! is rewritten where Python and fancy-regex read the same text differently, and compiled with
//! fancy-regex (backtracking, look-around and back-references like Python's `re`):
//!
//! - `\Z` (end of text) is `\z`;
//! - `{` and `}` that do not form a repeat are literal in Python, so they are escaped; `{,n}` is
//!   `{0,n}`;
//! - inside a character set, `[`, `&&`, `--` and `~~` are literal in Python (set operations in the
//!   regex crate), so `[`, `&`, `~` and a second `-` are escaped;
//! - `(?#...)` comments are dropped.

use fancy_regex::Regex;

fn repeat_at(cs: &[char], i: usize) -> Option<(String, usize)> {
    // cs[i] == '{'; Python: digits*, optionally ',' digits*, then '}' (but `{}` is literal)
    let mut j = i + 1;
    if cs.get(j) == Some(&'}') {
        return None;
    }
    let mut lo = String::new();
    while j < cs.len() && cs[j].is_ascii_digit() {
        lo.push(cs[j]);
        j += 1;
    }
    let mut hi: Option<String> = None;
    if cs.get(j) == Some(&',') {
        j += 1;
        let mut h = String::new();
        while j < cs.len() && cs[j].is_ascii_digit() {
            h.push(cs[j]);
            j += 1;
        }
        hi = Some(h);
    }
    if cs.get(j) != Some(&'}') {
        return None;
    }
    let lo = if lo.is_empty() { "0".to_string() } else { lo };
    let body = match hi {
        None => format!("{{{lo}}}"),
        Some(h) => format!("{{{lo},{h}}}"),
    };
    Some((body, j + 1))
}

/// The pattern as fancy-regex should read it to match what Python's `re` matches.
pub fn translate(p: &str) -> String {
    let cs: Vec<char> = p.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut in_class = false;
    let mut class_first = false;
    while i < cs.len() {
        let c = cs[i];
        if c == '\\' {
            match cs.get(i + 1) {
                Some('Z') if !in_class => out.push_str(r"\z"),
                Some(&d) => {
                    out.push('\\');
                    out.push(d);
                }
                None => out.push('\\'),
            }
            i += 2;
            class_first = false;
            continue;
        }
        if in_class {
            match c {
                ']' if !class_first => {
                    in_class = false;
                    out.push(']');
                }
                '[' | '&' | '~' => {
                    out.push('\\');
                    out.push(c);
                }
                '-' if i > 0 && cs[i - 1] == '-' => out.push_str("\\-"),
                _ => out.push(c),
            }
            class_first = false;
            i += 1;
            continue;
        }
        match c {
            '[' => {
                in_class = true;
                class_first = true;
                out.push('[');
                if cs.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
            }
            '(' if cs.get(i + 1) == Some(&'?') && cs.get(i + 2) == Some(&'#') => {
                let mut j = i + 3;
                while j < cs.len() && cs[j] != ')' {
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            '{' => match repeat_at(&cs, i) {
                Some((body, next)) => {
                    out.push_str(&body);
                    i = next;
                    continue;
                }
                None => out.push_str("\\{"),
            },
            '}' => out.push_str("\\}"),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// Compile a `match:` pattern: Err(Python's message) when Python's `re.compile` refuses it, or
/// Err(a message of our own) when Python accepts it and fancy-regex cannot run it.
pub fn compile(p: &str) -> Result<Regex, (bool, String)> {
    hydo_manifest::rx::check(p).map_err(|e| (true, e))?;
    Regex::new(&translate(p)).map_err(|e| (false, e.to_string()))
}

/// `pattern.search(s) is not None` (a pattern that runs out of its backtracking budget matches
/// nothing).
pub fn search(re: &Regex, s: &str) -> bool {
    re.is_match(s).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_readings() {
        assert_eq!(translate(r"a{2}b{,3}c{d}e\Z"), r"a{2}b{0,3}c\{d\}e\z");
        assert_eq!(translate(r"[[&~a--]x"), r"[\[\&\~a-\-]x");
        assert_eq!(translate(r"[]a](?#note)b"), r"[]a]b");
        for (p, s, want) in [
            (
                r"Connection to node # \(",
                "ERROR Connection to node # (x",
                true,
            ),
            (r"^ERROR.*refused$", "ERROR Connection refused", true),
            (r"(?P<w>ab)(?P=w)", "xxabab", true),
            (r"(?<!x)y", "xy", false),
            (r"a{,2}z\Z", "aaz", true),
            (r"x{y}", "x{y}", true),
            (r"(?i)fatal", "FATAL error", true),
        ] {
            let re = compile(p).unwrap();
            assert_eq!(search(&re, s), want, "{p} on {s}");
        }
        assert_eq!(
            compile("(").err().unwrap(),
            (
                true,
                "missing ), unterminated subpattern at position 0".to_string()
            )
        );
    }
}
