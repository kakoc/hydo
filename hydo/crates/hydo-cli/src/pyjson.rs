//! Python's `%s` of a JSON value (`str()`) and its string slices, used in printed lines (the
//! lines stay the Python CLI's). Since M115 the CLI writes its files in the results format
//! (hydo-results), not as Python's `json.dump`.

use serde_json::Value;

fn number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    float_repr(n.as_f64().unwrap_or(0.0))
}

/// Python's `repr(float)` for the values hydo writes (finite, not tiny).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if f.fract() == 0.0 && f.abs() < 1e16 {
        return format!("{f:.1}");
    }
    let a = f.abs();
    if !(1e-4..1e16).contains(&a) {
        // Python switches to exponent notation here: 1e-05, 1e+16
        let s = format!("{f:e}");
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    format!("{f}")
}

/// Python's `str(x)` / `"%s" % x` of a value read from JSON.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => number(n),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python's `s[:n]` (characters, not bytes).
pub fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's `s[-n:]` (characters).
pub fn tail(s: &str, n: usize) -> String {
    let len = s.chars().count();
    s.chars().skip(len.saturating_sub(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn like_python() {
        assert_eq!(float_repr(12.3), "12.3");
        assert_eq!(float_repr(0.0), "0.0");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(py_str(&json!(7)), "7");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(tail("abcdef", 2), "ef");
    }
}
