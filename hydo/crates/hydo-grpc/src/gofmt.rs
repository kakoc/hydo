//! Go's number formatting and parsing (strconv, encoding/json, protojson), so the Rust programs
//! write the same text as the Go generator and client did: plan.json, request bodies, pool values.

/// The shortest decimal digits of `x` (no sign) and the decimal point position: x = 0.d1d2… × 10^dp.
/// `bits` = 32 formats `x` as a float32. Zero gives no digits.
fn shortest(x: f64, bits: u32) -> (Vec<u8>, i32) {
    let a = x.abs();
    if a == 0.0 {
        return (Vec::new(), 0);
    }
    // Rust's `{:e}` writes the shortest digits that read back to the same value, as Go's
    // shortest formatting does (both pick the closest such string).
    let s = if bits == 32 { format!("{:e}", a as f32) } else { format!("{:e}", a) };
    let (m, e) = s.split_once('e').expect("exponent");
    let e: i32 = e.parse().expect("exponent number");
    let digits: Vec<u8> = m.bytes().filter(|c| c.is_ascii_digit()).collect();
    let mut d = digits;
    while d.len() > 1 && d.last() == Some(&b'0') {
        d.pop();
    }
    (d, e + 1)
}

fn fmt_e(out: &mut String, d: &[u8], dp: i32, prec: usize) {
    out.push(if d.is_empty() { '0' } else { d[0] as char });
    if prec > 0 {
        out.push('.');
        for i in 1..=prec {
            out.push(*d.get(i).unwrap_or(&b'0') as char);
        }
    }
    out.push('e');
    let exp = if d.is_empty() { 0 } else { dp - 1 };
    out.push(if exp < 0 { '-' } else { '+' });
    let a = exp.unsigned_abs();
    if a < 10 {
        out.push('0');
    }
    out.push_str(&a.to_string());
}

fn fmt_f(out: &mut String, d: &[u8], dp: i32, prec: usize) {
    if dp > 0 {
        let m = (dp as usize).min(d.len());
        for &c in &d[..m] {
            out.push(c as char);
        }
        for _ in m..dp as usize {
            out.push('0');
        }
    } else {
        out.push('0');
    }
    if prec > 0 {
        out.push('.');
        for i in 1..=prec as i32 {
            let j = dp + i - 1;
            let c = if j < 0 || j as usize >= d.len() { b'0' } else { d[j as usize] };
            out.push(c as char);
        }
    }
}

fn special(x: f64) -> Option<&'static str> {
    if x.is_nan() {
        Some("NaN")
    } else if x == f64::INFINITY {
        Some("+Inf")
    } else if x == f64::NEG_INFINITY {
        Some("-Inf")
    } else {
        None
    }
}

/// `strconv.FormatFloat(x, 'g', -1, bits)`.
pub fn format_g(x: f64, bits: u32) -> String {
    if let Some(s) = special(x) {
        return s.into();
    }
    let (d, dp) = shortest(x, bits);
    let mut out = String::new();
    if x.is_sign_negative() {
        out.push('-');
    }
    let nd = d.len() as i32;
    let exp = dp - 1;
    if !d.is_empty() && (exp < -4 || exp >= 6) {
        fmt_e(&mut out, &d, dp, (nd - 1).max(0) as usize);
    } else {
        // prec = nd; "if prec > dp { prec = nd }"; then max(prec - dp, 0) fraction digits
        fmt_f(&mut out, &d, dp, (nd - dp).max(0) as usize);
    }
    out
}

/// `strconv.FormatFloat(x, 'f', -1, bits)`.
pub fn format_f(x: f64, bits: u32) -> String {
    if let Some(s) = special(x) {
        return s.into();
    }
    let (d, dp) = shortest(x, bits);
    let mut out = String::new();
    if x.is_sign_negative() {
        out.push('-');
    }
    fmt_f(&mut out, &d, dp, (d.len() as i32 - dp).max(0) as usize);
    out
}

/// `strconv.FormatFloat(x, 'e', -1, bits)`.
pub fn format_e(x: f64, bits: u32) -> String {
    if let Some(s) = special(x) {
        return s.into();
    }
    let (d, dp) = shortest(x, bits);
    let mut out = String::new();
    if x.is_sign_negative() {
        out.push('-');
    }
    fmt_e(&mut out, &d, dp, (d.len() as i32 - 1).max(0) as usize);
    out
}

/// A finite float as encoding/json and protojson write it: %f, or %e for very small or large
/// magnitudes, with "e-07" shortened to "e-7".
pub fn json_float(x: f64, bits: u32) -> String {
    let a = x.abs();
    let use_e = a != 0.0
        && if bits == 64 {
            !(1e-6..1e21).contains(&a)
        } else {
            let f = a as f32;
            !(1e-6f32..1e21f32).contains(&f)
        };
    if !use_e {
        return format_f(x, bits);
    }
    let mut s = format_e(x, bits).into_bytes();
    let n = s.len();
    if n >= 4 && s[n - 4] == b'e' && s[n - 3] == b'-' && s[n - 2] == b'0' {
        s[n - 2] = s[n - 1];
        s.pop();
    }
    String::from_utf8(s).expect("ascii")
}

/// `strconv.ParseBool`.
pub fn parse_bool(s: &str) -> Result<bool, String> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        _ => Err(format!("strconv.ParseBool: parsing {}: invalid syntax", quote(s))),
    }
}

fn int_err(fname: &str, s: &str, range: bool) -> String {
    format!(
        "strconv.{}: parsing {}: {}",
        fname,
        quote(s),
        if range { "value out of range" } else { "invalid syntax" }
    )
}

/// `strconv.ParseInt(s, 10, bits)`.
pub fn parse_int(s: &str, bits: u32) -> Result<i64, String> {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    if body.is_empty() || !body.bytes().all(|c| c.is_ascii_digit()) {
        return Err(int_err("ParseInt", s, false));
    }
    let v: i128 = match s.parse::<i128>() {
        Ok(v) => v,
        Err(_) => return Err(int_err("ParseInt", s, true)),
    };
    let (lo, hi) = (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1);
    if v < lo || v > hi {
        return Err(int_err("ParseInt", s, true));
    }
    Ok(v as i64)
}

/// `strconv.ParseUint(s, 10, bits)`.
pub fn parse_uint(s: &str, bits: u32) -> Result<u64, String> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return Err(int_err("ParseUint", s, false));
    }
    let v: u128 = match s.parse::<u128>() {
        Ok(v) => v,
        Err(_) => return Err(int_err("ParseUint", s, true)),
    };
    if bits < 128 && v >= (1u128 << bits) {
        return Err(int_err("ParseUint", s, true));
    }
    Ok(v as u64)
}

/// `strconv.ParseFloat(s, bits)` for decimal input, "inf"/"infinity"/"nan" (any case, signed).
/// Out-of-range values are errors, as in Go. (Go's hexadecimal and underscore forms are not
/// read; nothing the generator writes uses them.)
pub fn parse_float(s: &str, bits: u32) -> Result<f64, String> {
    let bad = || format!("strconv.ParseFloat: parsing {}: invalid syntax", quote(s));
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let lower = body.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" || lower == "nan" {
        let v = if lower == "nan" {
            f64::NAN
        } else if s.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        return Ok(v);
    }
    // Go's decimal syntax: digits, an optional '.', digits, an optional exponent
    let b = body.as_bytes();
    let mut i = 0;
    let mut nd = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        nd += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            nd += 1;
        }
    }
    if nd == 0 {
        return Err(bad());
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let st = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == st {
            return Err(bad());
        }
    }
    if i != b.len() {
        return Err(bad());
    }
    let v = if bits == 32 {
        s.parse::<f32>().map(f64::from).map_err(|_| bad())?
    } else {
        s.parse::<f64>().map_err(|_| bad())?
    };
    if v.is_infinite() {
        return Err(format!("strconv.ParseFloat: parsing {}: value out of range", quote(s)));
    }
    Ok(v)
}

/// `strconv.Quote` for the strings in error messages (printable ASCII and valid UTF-8 as is).
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_like_go() {
        for (x, bits, want) in [
            (0.0, 64, "0"),
            (-0.0, 64, "-0"),
            (1.0, 64, "1"),
            (1.5, 64, "1.5"),
            (100000.0, 64, "100000"),
            (1e6, 64, "1e+06"),
            (1234567.5, 64, "1.2345675e+06"),
            (123456789012.0, 64, "1.23456789012e+11"),
            (1e10, 32, "1e+10"),
            (1e-6, 32, "1e-06"),
            (0.0001, 64, "0.0001"),
            (0.00001, 64, "1e-05"),
            (0.1, 32, "0.1"),
            (-0.5, 32, "-0.5"),
            (1e21, 64, "1e+21"),
            (5e-324, 64, "5e-324"),
            (1.7976931348623157e308, 64, "1.7976931348623157e+308"),
        ] {
            assert_eq!(format_g(x, bits), want, "{x} {bits}");
        }
    }

    #[test]
    fn json_like_go() {
        for (x, bits, want) in [
            (0.85, 64, "0.85"),
            (0.0, 64, "0"),
            (1e20, 64, "100000000000000000000"),
            (1e21, 64, "1e+21"),
            (1e-7, 64, "1e-7"),
            (1.5e-7, 64, "1.5e-7"),
            (123456.78, 64, "123456.78"),
            (-999999.9876, 64, "-999999.9876"),
            (3.4028235e38, 32, "3.4028235e+38"),
            (0.1, 32, "0.1"),
        ] {
            assert_eq!(json_float(x, bits), want, "{x} {bits}");
        }
    }

    #[test]
    fn parse_like_go() {
        assert_eq!(parse_int("-7", 32), Ok(-7));
        assert!(parse_int("2147483648", 32).is_err());
        assert!(parse_int("1.0", 64).is_err());
        assert!(parse_int("", 64).is_err());
        assert_eq!(parse_uint("18446744073709551615", 64), Ok(u64::MAX));
        assert!(parse_uint("-1", 64).is_err());
        assert_eq!(parse_float("1e10", 64), Ok(1e10));
        assert!(parse_float("1e400", 64).is_err());
        assert!(parse_float("abc", 64).is_err());
        assert!(parse_float("-Inf", 64).unwrap().is_infinite());
        assert_eq!(parse_bool("T"), Ok(true));
        assert!(parse_bool("yes").is_err());
    }
}
