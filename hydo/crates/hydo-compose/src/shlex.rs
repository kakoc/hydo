//! Python's `shlex.split` (POSIX mode, no comments) and `shlex.quote`, as the Compose importer
//! used them: Compose splits a string command into words the same way.

const WS: &str = " \t\r\n";

/// `shlex.split(s)`; the error is Python's ValueError text.
pub fn split(s: &str) -> Result<Vec<String>, String> {
    let cs: Vec<char> = s.chars().collect();
    let mut pos = 0;
    let mut out = Vec::new();
    // state: ' ' whitespace, 'a' word, a quote char, '\\' escape; None = end of input
    let mut state: Option<char> = Some(' ');
    loop {
        let mut token = String::new();
        let mut quoted = false;
        let mut escaped = ' ';
        loop {
            let next = cs.get(pos).copied();
            pos += 1;
            match state {
                None => {
                    token.clear();
                    break;
                }
                Some(' ') => match next {
                    None => {
                        state = None;
                        break;
                    }
                    Some(c) if WS.contains(c) => {
                        if !token.is_empty() || quoted {
                            break;
                        }
                    }
                    Some('\\') => {
                        escaped = 'a';
                        state = Some('\\');
                    }
                    Some(c) if c == '\'' || c == '"' => state = Some(c),
                    Some(c) => {
                        token.push(c);
                        state = Some('a');
                    }
                },
                Some(q) if q == '\'' || q == '"' => {
                    quoted = true;
                    match next {
                        None => return Err("No closing quotation".into()),
                        Some(c) if c == q => state = Some('a'),
                        Some('\\') if q == '"' => {
                            escaped = q;
                            state = Some('\\');
                        }
                        Some(c) => token.push(c),
                    }
                }
                Some('\\') => {
                    let Some(c) = next else {
                        return Err("No escaped character".into());
                    };
                    if (escaped == '\'' || escaped == '"') && c != '\\' && c != escaped {
                        token.push('\\');
                    }
                    token.push(c);
                    state = Some(escaped);
                }
                Some(_) => match next {
                    // 'a': inside a word
                    None => {
                        state = None;
                        break;
                    }
                    Some(c) if WS.contains(c) => {
                        state = Some(' ');
                        if !token.is_empty() || quoted {
                            break;
                        }
                    }
                    Some(c) if c == '\'' || c == '"' => state = Some(c),
                    Some('\\') => {
                        escaped = 'a';
                        state = Some('\\');
                    }
                    Some(c) => token.push(c),
                },
            }
        }
        if token.is_empty() && !quoted {
            if state.is_none() {
                return Ok(out);
            }
            continue;
        }
        out.push(token);
        if state.is_none() {
            // the next call returns the end
            return Ok(out);
        }
    }
}

/// `shlex.quote(s)`.
pub fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_python() {
        let v = |s: &str| split(s).unwrap();
        assert_eq!(
            v("redis-server --appendonly yes --save \"\""),
            ["redis-server", "--appendonly", "yes", "--save", ""]
        );
        assert_eq!(
            v("sh -c 'echo $HOME; exec app'"),
            ["sh", "-c", "echo $HOME; exec app"]
        );
        assert_eq!(v("a\\ b \"c\\\"d\\e\" ''"), ["a b", "c\"d\\e", ""]);
        assert_eq!(v("  "), Vec::<String>::new());
        assert_eq!(v("x'y'z"), ["xyz"]);
        assert_eq!(split("'a").unwrap_err(), "No closing quotation");
        assert_eq!(split("a\\").unwrap_err(), "No escaped character");
        assert_eq!(quote("a b'c"), "'a b'\"'\"'c'");
        assert_eq!(quote("/app/x"), "/app/x");
    }
}
