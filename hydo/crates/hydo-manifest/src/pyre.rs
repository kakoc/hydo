//! The few Python `re` behaviours the reference relies on: `re.fullmatch`, `re.match` of a pattern
//! that ends in `$` (which also matches before one trailing newline), and `re.search`.

use regex::Regex;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn compiled(pat: &str) -> Regex {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    c.entry(pat.to_string())
        .or_insert_with(|| Regex::new(pat).unwrap_or_else(|e| panic!("bad pattern {pat}: {e}")))
        .clone()
}

/// `re.fullmatch(pat, s)`.
pub fn fullmatch(pat: &str, s: &str) -> bool {
    compiled(&format!("^(?:{pat})$")).is_match(s)
}

/// `re.match("^<pat>$", s)`: Python's `$` also matches before a final newline.
pub fn match_dollar(pat: &str, s: &str) -> Option<Vec<Option<String>>> {
    let re = compiled(&format!("^(?:{pat})$"));
    let caps = re
        .captures(s)
        .or_else(|| s.strip_suffix('\n').and_then(|t| re.captures(t)));
    caps.map(|c| {
        c.iter()
            .skip(1)
            .map(|m| m.map(|x| x.as_str().to_string()))
            .collect()
    })
}

/// `re.match(pat, s)` (anchored at the start only).
pub fn match_start(pat: &str, s: &str) -> Option<Vec<Option<String>>> {
    compiled(&format!("^(?:{pat})")).captures(s).map(|c| {
        c.iter()
            .skip(1)
            .map(|m| m.map(|x| x.as_str().to_string()))
            .collect()
    })
}

/// `bool(re.search(pat, s))`.
pub fn search(pat: &str, s: &str) -> bool {
    compiled(pat).is_match(s)
}

/// `re.findall(pat, s)` of a pattern with exactly one group.
pub fn findall1(pat: &str, s: &str) -> Vec<String> {
    compiled(pat)
        .captures_iter(s)
        .map(|c| c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default())
        .collect()
}

/// `re.findall(r"\d+", s)`.
pub fn digit_runs(s: &str) -> Vec<String> {
    compiled(r"\d+")
        .find_iter(s)
        .map(|m| m.as_str().to_string())
        .collect()
}
