//! The part of Go's net/url the readers used: Parse for scheme://user:pw@host:port/path?query,
//! with the same percent-decoding and the same error texts for the common mistakes.

use crate::gofmt::quote;

#[derive(Debug, Default, Clone)]
pub struct Url {
    pub scheme: String,
    pub user: Option<(String, Option<String>)>,
    pub host: String,
    pub path: String,
    pub raw_query: String,
}

fn unhex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// url.PathUnescape / QueryUnescape (`plus`: '+' is a space).
pub fn unescape(s: &str, plus: bool) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = b.get(i + 1).and_then(|&c| unhex(c));
                let l = b.get(i + 2).and_then(|&c| unhex(c));
                match (h, l) {
                    (Some(h), Some(l)) => {
                        out.push(h << 4 | l);
                        i += 3;
                    }
                    _ => {
                        let end = (i + 3).min(b.len());
                        return Err(format!("invalid URL escape {}", quote(&b[i..end])));
                    }
                }
            }
            b'+' if plus => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// `url.Parse(raw)`; the error is Go's `parse "<raw>": <reason>`.
pub fn parse(raw: &str) -> Result<Url, String> {
    let wrap = |e: String| format!("parse {}: {e}", quote(raw.as_bytes()));
    let mut u = Url::default();
    let (rest, _frag) = raw.split_once('#').unwrap_or((raw, ""));
    let Some(colon) = rest.find(':') else {
        return Err(wrap("missing protocol scheme".into()));
    };
    let scheme = &rest[..colon];
    if scheme.is_empty() {
        return Err(wrap("missing protocol scheme".into()));
    }
    u.scheme = scheme.to_ascii_lowercase();
    let mut rest = &rest[colon + 1..];
    if let Some((r, qy)) = rest.split_once('?') {
        u.raw_query = qy.to_string();
        rest = r;
    }
    if let Some(r) = rest.strip_prefix("//") {
        let (auth, path) = match r.find('/') {
            Some(i) => (&r[..i], &r[i..]),
            None => (r, ""),
        };
        let host = match auth.rfind('@') {
            Some(i) => {
                let ui = &auth[..i];
                u.user = Some(match ui.split_once(':') {
                    Some((n, p)) => (
                        unescape(n, false).map_err(wrap)?,
                        Some(unescape(p, false).map_err(wrap)?),
                    ),
                    None => (unescape(ui, false).map_err(wrap)?, None),
                });
                &auth[i + 1..]
            }
            None => auth,
        };
        // a port must be digits
        let port_part = if host.starts_with('[') {
            host.find(']').map(|i| &host[i + 1..]).unwrap_or("")
        } else {
            host.rfind(':').map(|i| &host[i..]).unwrap_or("")
        };
        if let Some(p) = port_part.strip_prefix(':') {
            if !p.bytes().all(|c| c.is_ascii_digit()) {
                return Err(wrap(format!(
                    "invalid port {} after host",
                    quote(port_part.as_bytes())
                )));
            }
        }
        u.host = unescape(host, false).map_err(wrap)?;
        u.path = unescape(path, false).map_err(wrap)?;
    } else {
        u.path = unescape(rest, false).map_err(wrap)?;
    }
    Ok(u)
}

/// `u.Query()`: the first value of each key, in the keys' order of first appearance (Go's map
/// order is random; callers that care sort).
pub fn query(raw: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for part in raw.split('&') {
        if part.is_empty() || part.contains(';') {
            continue;
        }
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        let (Ok(k), Ok(v)) = (unescape(k, true), unescape(v, true)) else {
            continue;
        };
        if !out.iter().any(|(x, _)| *x == k) {
            out.push((k, v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let u = parse("postgres://miniflux@127.0.0.1/miniflux?sslmode=disable").unwrap();
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.user, Some(("miniflux".into(), None)));
        assert_eq!(u.path, "/miniflux");
        assert_eq!(
            query(&u.raw_query),
            vec![("sslmode".into(), "disable".into())]
        );
        let u = parse("redis://:p%40ss@h:1/2").unwrap();
        assert_eq!(u.user, Some(("".into(), Some("p@ss".into()))));
        assert!(parse("mysql://h:x/")
            .unwrap_err()
            .contains("invalid port \":x\" after host"));
    }
}
