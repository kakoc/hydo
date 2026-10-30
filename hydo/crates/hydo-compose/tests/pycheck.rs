//! hydo-compose's Python semantics (YAML read as PyYAML's `safe_load` reads it, `shlex.split`,
//! `shlex.quote`) against the frozen answers of Python itself, recorded in tests/golden/pycheck.txt
//! from tests/pycheck.py (`git show 2a58c4b:hydo/crates/hydo-compose/tests/pycheck.py`; Python
//! 3.15.0, PyYAML 6.0.3). The inputs are made here exactly as pycheck.py made them (CPython's
//! Mersenne Twister in tests/common/pyrandom.rs; the golden pins a hash of them):
//!
//! - `repr(yaml.safe_load(text))` of 22 edge cases (verbatim), of every Compose file the repository
//!   had then (33; by the SHA-256 of the file and of the answer: a file that has changed since is
//!   reported and skipped, except the crate's own fixtures, which must be unchanged) and of 400
//!   generated documents (verbatim);
//! - `shlex.split` and `shlex.quote` of 5,000 generated strings (chunked SHA-256).
//!
//! `HYDO_BLESS=1` rewrites the golden from the Rust answers (the crate is the reference from M107
//! on; bless only a deliberate change).

mod common;

use common::pyrandom::PyRandom;
use hydo_compose::pyval::{repr, Y};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const EDGE: [&str; 22] = [
    "a: yes\nb: No\nc: on\nd: OFF\ne: y\nf: n\n",
    "a: 0755\nb: 0o17\nc: 0x1F\nd: 0b101\ne: 1_000\nf: -0\ng: +12\n",
    "a: 1:30\nb: 22:22\nc: 8080:80\nd: 1:30:00\ne: 190:20:30.15\n",
    "a: 1.5\nb: 1.\nc: .5\nd: 1e5\ne: 1.0e+5\nf: .inf\ng: -.Inf\nh: .NaN\ni: 6.8523015e+5\n",
    "a: ~\nb: null\nc: Null\nd:\ne: NULL\nf: nULL\n",
    "a: 2024-01-02\nb: 2001-12-14t21:59:43.10-05:00\nc: 2001-12-14 21:59:43.10 -5\nd: 2002-12-14\ne: 2001-12-15T02:59:43.1Z\n",
    "a: '7'\nb: \"yes\"\nc: !!str 12\nd: !!int '12'\ne: !!float '1'\nf: ! 12\n",
    "x: &a {k: 1, m: 2}\ny: &b {k: 3, z: 4}\ns:\n  <<: [*a, *b]\n  m: 9\n",
    "x: &a\n  k: 1\ns:\n  <<: *a\n  k: 2\n  j: 3\n",
    "a: 1\na: 2\nb: 3\n",
    "- a\n- [b, c]\n- {d: e}\n",
    "a: |\n  x\n  y\nb: >\n  p\n  q\n\nc: |-\n  z\n",
    "a: \"\\u00e9\\t\\n\"\nb: 'it''s'\n",
    "? complex\n: value\n",
    "a: [1, 2,\n  3]\n",
    "test: [\n    \"CMD\",\n    # c\n    \"x\",\n  ]\n",
    "a: =\n",
    "{a: 1, b: [x, y]}",
    "",
    "# only a comment\n",
    "- 1\n- -1\n- 01\n- 09\n- 0.0\n- -0.0\n",
    "a: 1e\nb: 12e3\nc: 1.2.3\n",
];

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/pycheck.txt")
}

fn py_yaml(t: &str) -> String {
    match hydo_compose::yaml::safe_load(t) {
        Ok(v) => repr(&v),
        Err(_) => "ERR".into(),
    }
}

fn py_shlex(t: &str) -> String {
    match hydo_compose::shlex::split(t) {
        Ok(v) => repr(&Y::Seq(v.into_iter().map(Y::Str).collect())),
        Err(e) => format!("ERR: {e}"),
    }
}

/// `json.dumps(s)` (ASCII only, as Python's default).
fn json_str(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn sha(lines: &[String]) -> String {
    let mut h = Sha256::new();
    for l in lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn chunks(tag: &str, lines: &[String]) -> Vec<String> {
    lines
        .chunks(256)
        .enumerate()
        .map(|(i, c)| format!("{tag}\t{i}\t{}\t{}", c.len(), sha(c)))
        .collect()
}

#[test]
fn python_semantics_as_recorded() {
    let text = std::fs::read_to_string(golden()).expect("tests/golden/pycheck.txt");
    let head: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    let want: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    let mut got: Vec<String> = Vec::new();
    let (mut skipped, mut nfiles) = (Vec::new(), 0);
    for (i, d) in EDGE.iter().enumerate() {
        got.push(format!("yaml-edge\t{i}\t{}", json_str(&py_yaml(d))));
    }
    // the repository's Compose files, as recorded (path, sha256 of the text, sha256 of the answer)
    for w in want.iter().filter(|l| l.starts_with("yaml-file\t")) {
        let f: Vec<&str> = w.split('\t').collect();
        let text = std::fs::read_to_string(top().join(f[1])).unwrap_or_default();
        let th: String = Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if th != f[2] {
            assert!(
                !f[1].starts_with("hydo/crates/hydo-compose/tests/fixtures/"),
                "{}: the crate's own fixture changed (bless deliberately)",
                f[1]
            );
            skipped.push(f[1].to_string());
            got.push(w.to_string());
            continue;
        }
        nfiles += 1;
        got.push(format!(
            "yaml-file\t{}\t{th}\t{}",
            f[1],
            sha(&[py_yaml(&text)])
        ));
    }
    let mut rnd = PyRandom::new(7);
    let atoms = [
        "yes", "no", "on", "0755", "1:30", "1.5", "~", "", "'x'", "\"y\"", "2024-1-2", "0x1f",
        "1e5", "-.inf", "abc", "<<", "=", "12_3", "+1", "0b11", "3:60", "10:59:59", "1_0.5",
        "Null", "TRUE", ".NaN",
    ];
    let mut docs = Vec::new();
    for _ in 0..400 {
        let n = rnd.randint(1, 6);
        let mut d = String::new();
        for i in 0..n {
            let _ = writeln!(d, "k{i}: {}", rnd.choice(&atoms));
        }
        docs.push(d);
    }
    let inputs: Vec<String> = docs.iter().map(|d| json_str(d)).collect();
    got.extend(chunks("yaml-gen-input", &inputs));
    for (i, d) in docs.iter().enumerate() {
        got.push(format!("yaml-gen\t{i}\t{}", json_str(&py_yaml(d))));
    }
    let mut rnd = PyRandom::new(11);
    let alpha = [
        "a", "b", " ", "'", "\"", "\\", "$", "\t", "\n", "\u{e9}", "#", ";", "=", "-",
    ];
    let strs: Vec<String> = (0..5000)
        .map(|_| {
            let n = rnd.randint(0, 12);
            (0..n).map(|_| *rnd.choice(&alpha)).collect()
        })
        .collect();
    let col = |f: &dyn Fn(&str) -> String| strs.iter().map(|s| json_str(&f(s))).collect::<Vec<_>>();
    got.extend(chunks("shlex-input", &col(&|s| s.to_string())));
    got.extend(chunks("shlex-split", &col(&py_shlex)));
    got.extend(chunks("shlex-quote", &col(&hydo_compose::shlex::quote)));
    if std::env::var_os("HYDO_BLESS").is_some() {
        std::fs::write(golden(), head + &got.join("\n") + "\n").unwrap();
        return;
    }
    let bad: Vec<String> = want
        .iter()
        .zip(&got)
        .filter(|(w, g)| *w != *g)
        .map(|(w, g)| format!("  recorded: {w}\n  rust:     {g}"))
        .collect();
    assert_eq!(want.len(), got.len(), "the golden has other rows");
    assert!(
        bad.is_empty(),
        "{} of {} rows differ from Python's recorded answers:\n{}",
        bad.len(),
        want.len(),
        bad.join("\n")
    );
    eprintln!(
        "pycheck: {} edge cases, {nfiles} Compose files ({} changed since the recording, skipped: \
         {skipped:?}), {} generated documents and {} strings split and quoted, identical to Python",
        EDGE.len(),
        skipped.len(),
        docs.len(),
        strs.len()
    );
}
