//! Small pieces of Go's standard library whose exact behaviour shows in hydo-world's records and
//! answers: the record escaping, `%q`, header-key canonicalisation, status texts, the HTTP date,
//! query parsing (`url.ParseQuery`), path unescaping and the error texts of `os` and `net`.
//!
//! Values that come from a request (a ref, an idempotency key, a path) are byte strings, as in
//! Go: they are recorded byte for byte, whatever their encoding.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// `esc`: a value safe for one whitespace-separated, comma-separated field of a record or token.
pub fn esc(s: &[u8]) -> String {
    if s.is_empty() {
        return "-".into();
    }
    let mut b = String::with_capacity(s.len());
    for &c in s {
        if c.is_ascii_alphanumeric() || b"_.:@/+-".contains(&c) {
            b.push(c as char);
        } else {
            let _ = write!(b, "%{c:02X}");
        }
    }
    b
}

/// `unesc`: the inverse of [`esc`] (`-` is the empty value; a `%` not followed by two hex digits
/// stays as it is).
pub fn unesc(s: &[u8]) -> Vec<u8> {
    if s == b"-" {
        return Vec::new();
    }
    let mut b = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            if let (Some(h), Some(l)) = (hexval(s[i + 1]), hexval(s[i + 2])) {
                b.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        b.push(s[i]);
        i += 1;
    }
    b
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Go's `strconv.Quote` (what `%q` prints), for the error texts of rule parsing.
pub fn quote(s: &str) -> String {
    let mut b = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => b.push_str("\\\""),
            '\\' => b.push_str("\\\\"),
            '\u{7}' => b.push_str("\\a"),
            '\u{8}' => b.push_str("\\b"),
            '\u{c}' => b.push_str("\\f"),
            '\n' => b.push_str("\\n"),
            '\r' => b.push_str("\\r"),
            '\t' => b.push_str("\\t"),
            '\u{b}' => b.push_str("\\v"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(b, "\\x{:02x}", c as u32);
            }
            c if c.is_control() => {
                if (c as u32) < 0x10000 {
                    let _ = write!(b, "\\u{:04x}", c as u32);
                } else {
                    let _ = write!(b, "\\U{:08x}", c as u32);
                }
            }
            c => b.push(c),
        }
    }
    b.push('"');
    b
}

/// Go's `textproto.CanonicalMIMEHeaderKey`: "content-type" -> "Content-Type". A key with a byte
/// that is not a token character is returned unchanged.
pub fn canonical_key(k: &str) -> String {
    let ok = k.bytes().all(|c| {
        c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)
    });
    if !ok {
        return k.to_string();
    }
    let mut up = true;
    let mut out = String::with_capacity(k.len());
    for c in k.chars() {
        if up {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c.to_ascii_lowercase());
        }
        up = c == '-';
    }
    out
}

/// Go's `http.StatusText`.
pub fn status_text(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        101 => "Switching Protocols",
        102 => "Processing",
        103 => "Early Hints",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        208 => "Already Reported",
        226 => "IM Used",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        305 => "Use Proxy",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        407 => "Proxy Authentication Required",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Request Entity Too Large",
        414 => "Request URI Too Long",
        415 => "Unsupported Media Type",
        416 => "Requested Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        506 => "Variant Also Negotiates",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        510 => "Not Extended",
        511 => "Network Authentication Required",
        _ => "",
    }
}

/// Go's `bodyAllowedForStatus`.
pub fn body_allowed(code: u16) -> bool {
    !((100..200).contains(&code) || code == 204 || code == 304)
}

/// The HTTP date of now (Go's `http.TimeFormat`, "Mon, 02 Jan 2006 15:04:05 GMT").
pub fn http_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WD[days.rem_euclid(7) as usize],
        d,
        MO[(m - 1) as usize],
        y,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Now in milliseconds since 1970 (Go's `time.Now().UnixMilli()`).
pub fn unix_milli() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// `url.unescape` in query mode ('+' is a space) or path mode; `None` = a bad escape.
pub fn unescape(s: &[u8], query: bool) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'%' => {
                if i + 2 >= s.len() {
                    return None;
                }
                let h = hexval(s[i + 1])?;
                let l = hexval(s[i + 2])?;
                out.push(h << 4 | l);
                i += 3;
            }
            b'+' if query => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

/// `url.ParseQuery`: the values by key, in order, and whether any pair was bad (a ';' in it or a
/// bad escape; such a pair is skipped, as in Go).
pub fn parse_query(q: &[u8]) -> (Vec<(Vec<u8>, Vec<u8>)>, bool) {
    let mut out = Vec::new();
    let mut bad = false;
    for part in q.split(|&c| c == b'&') {
        if part.is_empty() {
            continue;
        }
        if part.contains(&b';') {
            bad = true;
            continue;
        }
        let (k, v) = match part.iter().position(|&c| c == b'=') {
            Some(p) => (&part[..p], &part[p + 1..]),
            None => (part, &b""[..]),
        };
        match (unescape(k, true), unescape(v, true)) {
            (Some(k), Some(v)) => out.push((k, v)),
            _ => bad = true,
        }
    }
    (out, bad)
}

/// The first value of key in a parsed query.
pub fn query_get<'a>(q: &'a [(Vec<u8>, Vec<u8>)], key: &[u8]) -> Option<&'a [u8]> {
    q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_slice())
}

/// The text of an OS error as Go prints it ("no such file or directory", "connection refused").
pub fn errno_text(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let s = unsafe { std::ffi::CStr::from_ptr(libc::strerror(n)) }
                .to_string_lossy()
                .into_owned();
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
                None => s,
            }
        }
        None => e.to_string(),
    }
}

/// Go's `*os.PathError` text: "open <path>: no such file or directory".
pub fn path_err(op: &str, path: &str, e: &std::io::Error) -> String {
    format!("{op} {path}: {}", errno_text(e))
}

/// Go's `JoinHostPort`.
pub fn join_host_port(host: &str, port: &str) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_round_trip() {
        assert_eq!(esc(b"a b,c%"), "a%20b%2Cc%25");
        assert_eq!(unesc(esc(b"a b,c%").as_bytes()), b"a b,c%");
        assert_eq!(unesc(esc(b"").as_bytes()), b"");
        assert_eq!(unesc(b"%4"), b"%4");
        assert_eq!(unesc(b"%41"), b"A");
        assert_eq!(unesc(b"%zz"), b"%zz");
        assert_eq!(esc(&[0xff, b'x']), "%FFx");
    }

    #[test]
    fn canonical() {
        assert_eq!(canonical_key("content-type"), "Content-Type");
        assert_eq!(canonical_key("IDEMPOTENCY-KEY"), "Idempotency-Key");
        assert_eq!(canonical_key("x y"), "x y");
        assert_eq!(canonical_key("grpc-status"), "Grpc-Status");
    }

    #[test]
    fn query() {
        let (q, bad) = parse_query(b"a=1&b=x+y&a=2&c");
        assert!(!bad);
        assert_eq!(query_get(&q, b"a"), Some(&b"1"[..]));
        assert_eq!(query_get(&q, b"b"), Some(&b"x y"[..]));
        assert_eq!(query_get(&q, b"c"), Some(&b""[..]));
        let (q, bad) = parse_query(b"a=%zz&b=1;2&c=3");
        assert!(bad);
        assert_eq!(query_get(&q, b"a"), None);
        assert_eq!(query_get(&q, b"c"), Some(&b"3"[..]));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("x"), "\"x\"");
        assert_eq!(quote("a\"b\\\n"), "\"a\\\"b\\\\\\n\"");
    }

    #[test]
    fn date() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20454), (2026, 1, 1));
        let d = http_date();
        assert!(d.ends_with(" GMT") && d.len() == 29, "{d}");
    }
}
