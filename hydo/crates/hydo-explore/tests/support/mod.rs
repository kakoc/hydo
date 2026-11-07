//! Shared by the explorer's frozen-reference tests (identity.rs, corpus.rs, fake_engine.rs): the
//! golden files in `tests/golden/`, their pinned inputs, and chunked SHA-256 of long answer streams.
//!
//! A golden file is text: `#` comment lines (what recorded it), then `input\t<path>\t<sha256>`
//! lines (the files the stream depends on: a new, gone or reordered input fails with a clear
//! message; a changed one is named when the stream differs, and passes when it does not),
//! then `chunk\t<label>\t<index>\t<lines>\t<sha256>` lines (a stream cut into groups by label and
//! each group into chunks of a fixed number of lines; a chunk's hash is the SHA-256 of its lines,
//! each followed by "\n").
//!
//! `HYDO_BLESS=1` rewrites a golden from the Rust output (Rust is the reference since M107);
//! `HYDO_GOLDEN_FROM=<file>` records it from a stream saved in a file instead (how the goldens of
//! M107 were recorded from the Python reference's own output).
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository root")
}

pub fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

pub fn bless() -> bool {
    std::env::var_os("HYDO_BLESS").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// explore.py's `mix64`.
pub fn mix(x: u64) -> u64 {
    hydo_explore::rng::mix64(x)
}

/// zlib.crc32.
pub fn crc32(b: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(b);
    c.sum()
}

/// `int(hashlib.sha256(text).hexdigest()[:15], 16)`.
pub fn salt_of(text: &str) -> u64 {
    u64::from_str_radix(&sha256_hex(text.as_bytes())[..15], 16).unwrap()
}

fn files_matching(dir: &Path, pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !n.starts_with('.') && pred(&n) {
                out.push(e.path());
            }
        }
    }
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = files_matching(dir, |_| true)
        .into_iter()
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// The example manifests, as ident.py / corpus.py globbed them (relative to the repository root):
/// `examples/*/hydo*.yaml`, `examples/*/hydo/hydo.yaml`, `examples-mac/*/hydo*.yaml`, each
/// pattern sorted, in that order.
pub fn manifests() -> Vec<String> {
    let top = top();
    let onb = top.join("hydo/onboard");
    let rel = |p: &Path| p.strip_prefix(&top).unwrap().to_string_lossy().into_owned();
    let mut out = Vec::new();
    let mut a = Vec::new();
    for d in subdirs(&onb.join("examples")) {
        for f in files_matching(&d, |n| n.starts_with("hydo") && n.ends_with(".yaml")) {
            if f.is_file() {
                a.push(rel(&f));
            }
        }
    }
    a.sort();
    out.extend(a);
    let mut b = Vec::new();
    for d in subdirs(&onb.join("examples")) {
        let f = d.join("hydo/hydo.yaml");
        if f.is_file() {
            b.push(rel(&f));
        }
    }
    b.sort();
    out.extend(b);
    let mut c = Vec::new();
    for d in subdirs(&onb.join("examples-mac")) {
        for f in files_matching(&d, |n| n.starts_with("hydo") && n.ends_with(".yaml")) {
            if f.is_file() {
                c.push(rel(&f));
            }
        }
    }
    c.sort();
    out.extend(c);
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with('.') {
            continue; // .DS_Store, a local .hydo output folder: not inputs
        }
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The pinned inputs of a manifest: the file itself, and for a `hydo/hydo.yaml` every file of its
/// `hydo/` folder (the test composer's plan reads the folder).
pub fn manifest_inputs(m: &str) -> Vec<(String, String)> {
    let top = top();
    let p = top.join(m);
    let mut files = vec![p.clone()];
    if m.ends_with("/hydo/hydo.yaml") {
        let mut more = Vec::new();
        walk(p.parent().unwrap(), &mut more);
        more.retain(|f| f != &p);
        more.sort();
        files.extend(more);
    }
    files
        .into_iter()
        .map(|f| {
            let b = std::fs::read(&f).unwrap_or_default();
            (
                f.strip_prefix(&top).unwrap().to_string_lossy().into_owned(),
                sha256_hex(&b),
            )
        })
        .collect()
}

/// A stream of lines cut into labelled groups.
#[derive(Default)]
pub struct Stream {
    pub groups: Vec<(String, Vec<String>)>,
}

impl Stream {
    pub fn group(&mut self, label: &str) {
        self.groups.push((label.to_string(), Vec::new()));
    }
    pub fn line(&mut self, l: String) {
        if self.groups.is_empty() {
            self.group("-");
        }
        self.groups.last_mut().unwrap().1.push(l);
    }
    pub fn lines(&self) -> usize {
        self.groups.iter().map(|g| g.1.len()).sum()
    }
}

pub struct Chunk {
    pub label: String,
    pub index: usize,
    pub count: usize,
    pub hash: String,
}

pub fn chunks(s: &Stream, size: usize) -> Vec<(Chunk, Vec<String>)> {
    let mut out = Vec::new();
    for (label, lines) in &s.groups {
        for (i, ch) in lines.chunks(size).enumerate() {
            let mut b = Vec::new();
            for l in ch {
                b.extend_from_slice(l.as_bytes());
                b.push(b'\n');
            }
            out.push((
                Chunk {
                    label: label.clone(),
                    index: i,
                    count: ch.len(),
                    hash: sha256_hex(&b),
                },
                ch.to_vec(),
            ));
        }
    }
    out
}

pub struct Golden {
    pub comments: Vec<String>,
    pub inputs: Vec<(String, String)>,
    pub chunks: Vec<Chunk>,
}

pub fn read_golden(path: &Path) -> Golden {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (record it with HYDO_BLESS=1 and check the result)",
            path.display()
        )
    });
    let mut g = Golden {
        comments: Vec::new(),
        inputs: Vec::new(),
        chunks: Vec::new(),
    };
    for l in text.lines() {
        if l.starts_with('#') {
            g.comments.push(l.to_string());
            continue;
        }
        let f: Vec<&str> = l.split('\t').collect();
        match f[0] {
            "input" => g.inputs.push((f[1].to_string(), f[2].to_string())),
            "chunk" => g.chunks.push(Chunk {
                label: f[1].to_string(),
                index: f[2].parse().unwrap(),
                count: f[3].parse().unwrap(),
                hash: f[4].to_string(),
            }),
            _ => panic!("{}: bad golden line {l}", path.display()),
        }
    }
    g
}

pub fn write_golden(path: &Path, comments: &[String], inputs: &[(String, String)], ch: &[Chunk]) {
    let mut t = String::new();
    for c in comments {
        t.push_str(c);
        t.push('\n');
    }
    for (p, h) in inputs {
        t.push_str(&format!("input\t{p}\t{h}\n"));
    }
    for c in ch {
        t.push_str(&format!(
            "chunk\t{}\t{}\t{}\t{}\n",
            c.label, c.index, c.count, c.hash
        ));
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, t).unwrap();
}

/// Compare the pinned inputs with the recording. A new, gone or reordered input is another input
/// set and fails here. An input whose bytes changed is returned (and printed) but decides nothing:
/// the answers do (Milestone 171), so an edit that changes no answer passes, and a failing
/// comparison of the answers names the inputs that changed.
pub fn check_inputs(path: &Path, g: &Golden, now: &[(String, String)]) -> Vec<String> {
    if g.inputs == now {
        return Vec::new();
    }
    let mut msg = Vec::new();
    let mut changed = Vec::new();
    for (p, h) in now {
        match g.inputs.iter().find(|x| &x.0 == p) {
            None => msg.push(format!("  new input: {p}")),
            Some((_, h0)) if h0 != h => changed.push(p.clone()),
            _ => {}
        }
    }
    for (p, _) in &g.inputs {
        if !now.iter().any(|x| &x.0 == p) {
            msg.push(format!("  gone: {p}"));
        }
    }
    let names = |v: &[(String, String)]| v.iter().map(|x| x.0.clone()).collect::<Vec<_>>();
    if msg.is_empty() && names(&g.inputs) != names(now) {
        msg.push("  the order of the inputs changed".into());
    }
    assert!(
        msg.is_empty(),
        "{}: the inputs this golden was recorded from changed since:\n{}\nIf the change is \
         intended, check the explorer's answers on them and re-record with HYDO_BLESS=1.",
        path.display(),
        msg.join("\n")
    );
    eprintln!(
        "{}: inputs whose bytes changed since the recording (the answers decide): {changed:?}",
        path.display()
    );
    changed
}

/// Compare a stream against its golden, or write the golden (HYDO_BLESS, HYDO_GOLDEN_FROM).
pub fn verify(
    path: &Path,
    comments: &[String],
    inputs: &[(String, String)],
    stream: &Stream,
    size: usize,
) {
    let ch = chunks(stream, size);
    if bless() || std::env::var_os("HYDO_GOLDEN_FROM").is_some() {
        let only: Vec<Chunk> = ch.into_iter().map(|c| c.0).collect();
        write_golden(path, comments, inputs, &only);
        eprintln!(
            "wrote {} ({} lines, {} chunks)",
            path.display(),
            stream.lines(),
            only.len()
        );
        return;
    }
    let g = read_golden(path);
    let changed = check_inputs(path, &g, inputs);
    let mut bad = Vec::new();
    let mut shown = false;
    let n = ch.len().max(g.chunks.len());
    for i in 0..n {
        match (ch.get(i), g.chunks.get(i)) {
            (Some((c, lines)), Some(w)) => {
                if c.label != w.label
                    || c.index != w.index
                    || c.count != w.count
                    || c.hash != w.hash
                {
                    let mut m = format!(
                        "chunk {i}: label {} #{} ({} lines) differs from the recorded {} #{} ({} lines)",
                        c.label, c.index, c.count, w.label, w.index, w.count
                    );
                    if !shown {
                        shown = true;
                        m.push_str("\n  the Rust lines of this chunk:");
                        for l in lines {
                            let l: String = l.chars().take(400).collect();
                            m.push_str(&format!("\n    {l}"));
                        }
                    }
                    bad.push(m);
                }
            }
            (Some((c, _)), None) => bad.push(format!(
                "chunk {i}: label {} #{}: more output than recorded",
                c.label, c.index
            )),
            (None, Some(w)) => bad.push(format!(
                "chunk {i}: label {} #{}: recorded, but the Rust stream ended",
                w.label, w.index
            )),
            (None, None) => {}
        }
        if bad.len() >= 12 {
            break;
        }
    }
    assert!(
        bad.is_empty(),
        "{}: {} lines in {} chunks; differences:\n{}{}",
        path.display(),
        stream.lines(),
        ch.len(),
        bad.join("\n"),
        if changed.is_empty() {
            String::new()
        } else {
            format!(
                "\ninputs whose bytes changed since the recording: {changed:?}; if the change is \
                 intended, check the answers and re-record with HYDO_BLESS=1"
            )
        }
    );
}

/// The lines of a stream saved in HYDO_GOLDEN_FROM, if set.
pub fn recorded_from() -> Option<Vec<String>> {
    let p = std::env::var_os("HYDO_GOLDEN_FROM")?;
    let t = std::fs::read(&p).expect("HYDO_GOLDEN_FROM");
    let t = String::from_utf8(t).expect("utf-8 stream");
    let mut v: Vec<String> = t.split('\n').map(str::to_string).collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    Some(v)
}
