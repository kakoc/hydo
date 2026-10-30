//! Recorded answer streams (goldens) for the equivalence tests, without the Python reference.
//!
//! A stream is a list of labelled sections of lines. The golden keeps, per section, the SHA-256
//! of each chunk of `CHUNK` lines: `<label>\t<chunk>\t<count>\t<sha256>` (lines starting with
//! `#` are comments). A test produces the same stream itself and compares; on a mismatch it names
//! the section and chunk and prints the produced lines of that chunk. `HYDO_BLESS=1` rewrites the
//! golden from the produced stream (Rust is the reference since the Python was deleted).

#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub type Sections = Vec<(String, Vec<String>)>;
/// A golden row: section, chunk index, line count, sha256.
pub type Row = (String, usize, usize, String);

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// (label, chunk index) -> (line count, sha256 of the chunk's lines, each ending in '\n').
pub fn digest(sections: &Sections, chunk: usize) -> Vec<Row> {
    let mut out = Vec::new();
    for (label, lines) in sections {
        if lines.is_empty() {
            out.push((label.clone(), 0, 0, sha256_hex(b"")));
        }
        for (i, c) in lines.chunks(chunk).enumerate() {
            let mut h = Sha256::new();
            for l in c {
                h.update(l.as_bytes());
                h.update(b"\n");
            }
            let s: String = h.finalize().iter().map(|x| format!("{x:02x}")).collect();
            out.push((label.clone(), i, c.len(), s));
        }
    }
    out
}

pub fn read(path: &Path) -> (Vec<String>, Vec<Row>) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e} (HYDO_BLESS=1 writes it)", path.display()));
    let mut header = Vec::new();
    let mut rows = Vec::new();
    for l in text.lines() {
        if l.starts_with('#') || l.is_empty() {
            header.push(l.to_string());
            continue;
        }
        let f: Vec<&str> = l.split('\t').collect();
        assert_eq!(f.len(), 4, "bad golden line: {l}");
        rows.push((
            f[0].to_string(),
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].to_string(),
        ));
    }
    (header, rows)
}

pub fn write(path: &Path, header: &[String], rows: &[Row]) {
    let mut s = String::new();
    for h in header {
        s.push_str(h);
        s.push('\n');
    }
    for (l, i, n, h) in rows {
        s.push_str(&format!("{l}\t{i}\t{n}\t{h}\n"));
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, s).unwrap();
}

pub fn bless() -> bool {
    std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1")
}

/// Compare the produced sections whose labels pass `want` with the golden at `path`; panics with
/// the differing chunks. With HYDO_BLESS=1 the golden's rows for those labels are replaced (rows
/// of other labels are kept) and the comparison is skipped.
pub fn check(
    path: &Path,
    default_header: &[String],
    sections: &Sections,
    chunk: usize,
    want: &dyn Fn(&str) -> bool,
) {
    let got = digest(sections, chunk);
    if bless() {
        let (mut header, old) = if path.exists() {
            read(path)
        } else {
            (default_header.to_vec(), Vec::new())
        };
        if header.is_empty() {
            header = default_header.to_vec();
        }
        let mut rows: Vec<_> = old.into_iter().filter(|r| !want(&r.0)).collect();
        rows.extend(got.into_iter().filter(|r| want(&r.0)));
        // keep the stream's own order for the new rows, the old ones first
        write(path, &header, &rows);
        eprintln!("BLESSED {}", path.display());
        return;
    }
    let (_, rows) = read(path);
    let rows: Vec<_> = rows.into_iter().filter(|r| want(&r.0)).collect();
    let mut by: BTreeMap<(String, usize), (usize, String)> = BTreeMap::new();
    for (l, i, n, h) in &rows {
        by.insert((l.clone(), *i), (*n, h.clone()));
    }
    let mut bad = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let lines_of = |label: &str, i: usize| -> Vec<String> {
        sections
            .iter()
            .find(|s| s.0 == label)
            .map(|s| s.1.chunks(chunk).nth(i).unwrap_or(&[]).to_vec())
            .unwrap_or_default()
    };
    for (l, i, n, h) in got.iter().filter(|r| want(&r.0)) {
        seen.insert((l.clone(), *i));
        match by.get(&(l.clone(), *i)) {
            Some((wn, wh)) if wn == n && wh == h => {}
            Some((wn, _)) => bad.push(format!(
                "section {l} chunk {i}: {n} lines (recorded {wn}) differ from the recording; the produced lines:\n{}",
                lines_of(l, *i)
                    .iter()
                    .map(|x| format!("    {}", x.chars().take(2000).collect::<String>()))
                    .collect::<Vec<_>>()
                    .join("\n")
            )),
            None => bad.push(format!("section {l} chunk {i}: not in the recording")),
        }
    }
    for (l, i, _, _) in &rows {
        if !seen.contains(&(l.clone(), *i)) {
            bad.push(format!("section {l} chunk {i}: recorded but not produced"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} chunks differ from {} (HYDO_BLESS=1 re-records after a deliberate change):\n{}",
        bad.len(),
        rows.len(),
        path.display(),
        bad.iter().take(3).cloned().collect::<Vec<_>>().join("\n")
    );
}
