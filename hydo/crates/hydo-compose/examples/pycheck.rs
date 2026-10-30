//! A probe for cross-checking the Python semantics of hydo-compose against Python itself
//! (tests/pycheck.py): reads JSON strings, one per line, and prints one JSON string per line:
//!
//!   pycheck yaml    repr(yaml.safe_load(text)), or "ERR"
//!   pycheck shlex   repr(shlex.split(text)), or "ERR: <message>"
//!   pycheck quote   shlex.quote(text)

use std::io::BufRead;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    for line in std::io::stdin().lock().lines() {
        let s: String = serde_json::from_str(&line.unwrap()).unwrap();
        let out = match mode.as_str() {
            "yaml" => match hydo_compose::yaml::safe_load(&s) {
                Ok(v) => hydo_compose::pyval::repr(&v),
                Err(_) => "ERR".into(),
            },
            "shlex" => match hydo_compose::shlex::split(&s) {
                Ok(v) => hydo_compose::pyval::repr(&hydo_compose::pyval::Y::Seq(
                    v.into_iter().map(hydo_compose::pyval::Y::Str).collect(),
                )),
                Err(e) => format!("ERR: {e}"),
            },
            _ => hydo_compose::shlex::quote(&s),
        };
        println!("{}", serde_json::to_string(&out).unwrap());
    }
}
