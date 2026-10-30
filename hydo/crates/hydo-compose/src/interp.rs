//! Compose variable interpolation and env files (hydo_compose.py's `Interp` and
//! `parse_env_file`).

use crate::pyval::{pstr, Y};
use crate::{ComposeError, Warn};
use hydo_manifest::py;

/// Python's `str.splitlines()`.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((pos, c)) = it.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            out.push(&s[start..pos]);
            let mut next = pos + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = it.peek() {
                    it.next();
                    next += 1;
                }
            }
            start = next;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Read a text file as Python's `open(p, encoding="utf-8").read()` (universal newlines).
pub fn read_text(p: &str) -> Result<String, ComposeError> {
    let b = std::fs::read(p).map_err(|e| ComposeError::os(e, p))?;
    let s = String::from_utf8(b).map_err(|_| {
        ComposeError(format!(
            "{p}: 'utf-8' codec can't decode the file (not UTF-8 text)"
        ))
    })?;
    Ok(if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s
    })
}

/// A Docker/Compose env file: KEY=VALUE lines, # comments, optional `export `, quotes stripped.
/// Returns the values in order and the keys that have no `=`.
#[allow(clippy::type_complexity)]
pub fn parse_env_file(
    path: &str,
    required: bool,
) -> Result<(Vec<(String, String)>, Vec<String>), ComposeError> {
    if !std::path::Path::new(path).is_file() {
        if required {
            return Err(ComposeError(format!("env file {path} does not exist")));
        }
        return Ok((Vec::new(), Vec::new()));
    }
    let text = read_text(path)?;
    let mut out: Vec<(String, String)> = Vec::new();
    let mut bare = Vec::new();
    for ln in splitlines(&text) {
        let mut s = py::strip(ln);
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        if let Some(r) = s.strip_prefix("export ") {
            s = py::lstrip(r);
        }
        let (k, v) = match s.split_once('=') {
            Some((k, v)) => (py::strip(k).to_string(), v),
            None => {
                bare.push(py::strip(s).to_string());
                continue;
            }
        };
        let mut v = py::strip(v).to_string();
        let cs: Vec<char> = v.chars().collect();
        if cs.len() >= 2 && cs[0] == cs[cs.len() - 1] && (cs[0] == '\'' || cs[0] == '"') {
            v = cs[1..cs.len() - 1].iter().collect();
            // the original line's value part decides (as the Python did)
            let orig = py::strip(ln);
            let after = orig.split_once('=').map(|x| x.1).unwrap_or("");
            if py::strip(after).starts_with('"') {
                v = v.replace("\\n", "\n").replace("\\\"", "\"");
            }
        } else if let Some(i) = v.find(" #") {
            v = py::rstrip(&v[..i]).to_string();
        }
        if let Some(e) = out.iter_mut().find(|(x, _)| *x == k) {
            e.1 = v;
        } else {
            out.push((k, v));
        }
    }
    Ok((out, bare))
}

/// Compose variable interpolation: $VAR ${VAR} ${VAR:-d} ${VAR-d} ${VAR:?e} ${VAR?e} ${VAR:+r}
/// ${VAR+r}; $$ is a literal $ (kept as $$ here, unescaped where Compose unescapes).
pub struct Interp {
    pub env: Vec<(String, String)>,
    pub shell: Vec<(String, String)>,
    pub use_env: bool,
    used_shell: Vec<String>,
}

fn lookup<'a>(v: &'a [(String, String)], k: &str) -> Option<&'a String> {
    v.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

impl Interp {
    pub fn new(env: Vec<(String, String)>, shell: Vec<(String, String)>, use_env: bool) -> Interp {
        Interp {
            env,
            shell,
            use_env,
            used_shell: Vec::new(),
        }
    }

    pub fn get(&mut self, k: &str, w: &mut Warn) -> Option<String> {
        if self.use_env {
            if let Some(v) = lookup(&self.shell, k) {
                return Some(v.clone());
            }
        }
        if let Some(v) = lookup(&self.env, k).cloned() {
            if let Some(sv) = lookup(&self.shell, k) {
                if *sv != v && !self.used_shell.iter().any(|x| x == k) {
                    self.used_shell.push(k.to_string());
                    w.warn("interpolation", format!("${{{k}}} is also set in your shell environment; Compose would use the shell's value, hydo-compose used the .env value (pass --use-env to follow Compose)"));
                }
            }
            return Some(v);
        }
        if lookup(&self.shell, k).is_some() && !self.used_shell.iter().any(|x| x == k) {
            self.used_shell.push(k.to_string());
            w.warn("interpolation", format!("${{{k}}} is set only in your shell environment; it was NOT used (the manifest must not depend on who runs the importer). Put it in .env or pass --use-env"));
        }
        None
    }

    pub fn sub(&mut self, s: &str, wh: &str, w: &mut Warn) -> Result<String, ComposeError> {
        let cs: Vec<char> = s.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < cs.len() {
            if cs[i] != '$' {
                out.push(cs[i]);
                i += 1;
                continue;
            }
            match cs.get(i + 1) {
                Some('$') => {
                    out.push_str("$$");
                    i += 2;
                }
                Some('{') if cs[i + 2..].contains(&'}') => {
                    let end = i + 2 + cs[i + 2..].iter().position(|&c| c == '}').unwrap();
                    let body: String = cs[i + 2..end].iter().collect();
                    out.push_str(&self.one(&body, wh, w)?);
                    i = end + 1;
                }
                Some(&c) if c.is_ascii_alphabetic() || c == '_' => {
                    let mut j = i + 1;
                    while j < cs.len() && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
                        j += 1;
                    }
                    let name: String = cs[i + 1..j].iter().collect();
                    out.push_str(&self.one(&name, wh, w)?);
                    i = j;
                }
                _ => {
                    out.push('$');
                    i += 1;
                }
            }
        }
        Ok(out)
    }

    fn one(&mut self, body: &str, wh: &str, w: &mut Warn) -> Result<String, ComposeError> {
        // ^([A-Za-z_][A-Za-z0-9_]*)(?:(:?[-?+])(.*))?$  (re.S; $ also before a final newline)
        let cs: Vec<char> = body.chars().collect();
        let ok_first = cs
            .first()
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_');
        let mut j = 0;
        if ok_first {
            j = 1;
            while j < cs.len() && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
                j += 1;
            }
        }
        let k: String = cs[..j].iter().collect();
        let rest: String = cs[j..].iter().collect();
        let (op, arg) = if !ok_first {
            (None, String::new())
        } else if rest.is_empty() || rest == "\n" {
            (Some(""), String::new())
        } else if let Some(a) = rest.strip_prefix(":-") {
            (Some(":-"), a.to_string())
        } else if let Some(a) = rest.strip_prefix(":?") {
            (Some(":?"), a.to_string())
        } else if let Some(a) = rest.strip_prefix(":+") {
            (Some(":+"), a.to_string())
        } else if let Some(a) = rest.strip_prefix('-') {
            (Some("-"), a.to_string())
        } else if let Some(a) = rest.strip_prefix('?') {
            (Some("?"), a.to_string())
        } else if let Some(a) = rest.strip_prefix('+') {
            (Some("+"), a.to_string())
        } else {
            (None, String::new())
        };
        let Some(op) = op else {
            return Err(ComposeError(format!(
                "{wh}: invalid interpolation ${{{body}}}"
            )));
        };
        let v = self.get(&k, w);
        if op.is_empty() {
            return Ok(match v {
                None => {
                    w.warn(
                        "interpolation",
                        format!(
                            "{wh}: ${{{k}}} is not set; it became an empty string (as in Compose)"
                        ),
                    );
                    String::new()
                }
                Some(v) => v,
            });
        }
        let unset = match &v {
            None => true,
            Some(s) => op.starts_with(':') && s.is_empty(),
        };
        match op.chars().last() {
            Some('-') => {
                if unset {
                    self.sub(&arg, wh, w)
                } else {
                    Ok(v.unwrap_or_default())
                }
            }
            Some('?') => {
                if unset {
                    Err(ComposeError(format!(
                        "{wh}: required variable {k} is not set: {}",
                        if arg.is_empty() { "missing" } else { &arg }
                    )))
                } else {
                    Ok(v.unwrap_or_default())
                }
            }
            _ => {
                if unset {
                    Ok(String::new())
                } else {
                    self.sub(&arg, wh, w)
                }
            }
        }
    }

    /// Interpolate every string of the document (keys are kept as they are).
    pub fn walk(&mut self, x: &Y, wh: &str, w: &mut Warn) -> Result<Y, ComposeError> {
        Ok(match x {
            Y::Str(s) => Y::Str(self.sub(s, wh, w)?),
            Y::Seq(v) => Y::Seq(
                v.iter()
                    .map(|y| self.walk(y, wh, w))
                    .collect::<Result<_, _>>()?,
            ),
            Y::Map(m) => {
                let mut out = Y::Map(Vec::new());
                for (k, v) in m {
                    let nv = self.walk(v, &format!("{wh}.{}", pstr(k)), w)?;
                    out.set(k.clone(), nv);
                }
                out
            }
            other => other.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates() {
        let mut w = Warn::default();
        let mut i = Interp::new(
            vec![("A".into(), "1".into()), ("E".into(), String::new())],
            vec![],
            false,
        );
        let mut s = |x: &str| i.sub(x, "w", &mut w);
        assert_eq!(
            s("$A-${A}-$$-${B:-d$A}-${E:-x}-${E-x}-${A:+r}-${B+r}").unwrap(),
            "1-1-$$-d1-x--r-"
        );
        assert_eq!(s("${A").unwrap(), "${A");
        assert_eq!(s("$ 1").unwrap(), "$ 1");
        assert!(s("${B:?need}")
            .unwrap_err()
            .0
            .contains("required variable B is not set: need"));
        assert!(s("${1x}").is_err());
    }
}
