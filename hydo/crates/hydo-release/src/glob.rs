//! Release-path patterns: `*` and `?` within one path segment, `**` across segments.

/// Whether `path` matches `pat`.
pub fn matches(pat: &str, path: &str) -> bool {
    m(pat.as_bytes(), path.as_bytes())
}

fn m(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    if p.starts_with(b"**") {
        let rest = &p[2..];
        let rest = rest.strip_prefix(b"/").unwrap_or(rest);
        return (0..=s.len())
            .any(|i| (i == 0 || s[i - 1] == b'/' || rest.is_empty()) && m(rest, &s[i..]));
    }
    if p[0] == b'*' {
        for i in 0..=s.len() {
            if i > 0 && s[i - 1] == b'/' {
                break;
            }
            if m(&p[1..], &s[i..]) {
                return true;
            }
        }
        return false;
    }
    !s.is_empty() && ((p[0] == b'?' && s[0] != b'/') || p[0] == s[0]) && m(&p[1..], &s[1..])
}

/// The part of a pattern before its first wildcard.
pub fn literal_prefix(pat: &str) -> &str {
    let end = pat.find(['*', '?']).unwrap_or(pat.len());
    &pat[..end]
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn patterns() {
        assert!(matches("bin/hydo", "bin/hydo"));
        assert!(!matches("bin/hydo", "bin/hydo-build"));
        assert!(matches("bin/*", "bin/hydo-build"));
        assert!(!matches("bin/*", "bin/x/y"));
        assert!(matches(
            "guest/modules/**",
            "guest/modules/6.6.35/kernel/net/sched/sch_prio.ko.xz"
        ));
        assert!(matches("a/**/b", "a/b"));
        assert!(matches("a/**/b", "a/x/y/b"));
        assert!(!matches("a/**/b", "a/xb"));
        assert!(matches(
            "guest/tc/nix/store/*-glibc-2.39-52/**",
            "guest/tc/nix/store/8sz-glibc-2.39-52/lib/libc.so.6"
        ));
        assert!(matches("lib?.so", "liba.so"));
        assert!(!matches("lib?.so", "lib/.so"));
    }
}
