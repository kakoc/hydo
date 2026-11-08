//! The repository's documents are checked like code (docs/STYLE.md, "How the standard is
//! checked"). The documents are the ones `ci/vale.sh` checks: its `DOCS` patterns without its
//! `EXCEPT` files, read from the script, so the two lists cannot drift apart.
//!
//! - Every relative link of those documents names a file or a directory of the repository, and a
//!   `#anchor` names a heading of the linked Markdown file (GitHub's anchor rules). External links
//!   are checked only by the nightly job of `.github/workflows/docs.yml`, so that this test needs
//!   no network.

use std::path::{Path, PathBuf};

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(top().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The value of `NAME="..."` in ci/vale.sh, which may span lines.
fn script_list(name: &str) -> Vec<String> {
    let s = read("ci/vale.sh");
    let start = s
        .find(&format!("\n{name}=\""))
        .unwrap_or_else(|| panic!("ci/vale.sh has no {name}=\"...\""));
    let rest = &s[start + name.len() + 3..];
    let end = rest.find('"').expect("closing quote");
    rest[..end].split_whitespace().map(String::from).collect()
}

/// `pattern` expanded in the repository: `*` matches one path segment, or a part of the last.
fn expand(pattern: &str) -> Vec<String> {
    if !pattern.contains('*') {
        return vec![pattern.to_string()];
    }
    let mut paths = vec![String::new()];
    for seg in pattern.split('/') {
        let mut next = Vec::new();
        for p in &paths {
            if seg.contains('*') {
                let (pre, post) = seg.split_once('*').unwrap();
                let dir = top().join(p);
                let mut names: Vec<String> = std::fs::read_dir(&dir)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                names.sort();
                for n in names {
                    if n.starts_with(pre) && n.ends_with(post) && n.len() >= pre.len() + post.len()
                    {
                        next.push(join(p, &n));
                    }
                }
            } else {
                next.push(join(p, seg));
            }
        }
        paths = next;
    }
    paths
        .into_iter()
        .filter(|p| top().join(p).exists())
        .collect()
}

fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}

/// The documents `ci/vale.sh` checks.
pub fn documents() -> Vec<String> {
    let except = script_list("EXCEPT");
    let mut out = Vec::new();
    for p in script_list("DOCS") {
        for f in expand(&p) {
            if !except.contains(&f) && !out.contains(&f) {
                out.push(f);
            }
        }
    }
    out
}

/// The lines of a Markdown text outside fenced code blocks, with their line numbers.
fn prose_lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut fenced = false;
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push((i + 1, line));
        }
    }
    out
}

/// A line without its code spans.
fn without_code(line: &str) -> String {
    line.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 0)
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join(" ")
}

/// GitHub's anchor of a heading: lower case, letters, digits, `-` and `_` kept, spaces turned
/// into `-`, everything else dropped.
fn anchor(heading: &str) -> String {
    let mut s = String::new();
    for c in heading.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            s.extend(c.to_lowercase());
        } else if c == ' ' {
            s.push('-');
        }
    }
    s
}

fn anchors_of(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (_, line) in prose_lines(text) {
        let t = line.trim_start();
        if t.starts_with('#') {
            let h = t.trim_start_matches('#');
            if h.starts_with(' ') {
                let a = anchor(h);
                let n = seen.iter().filter(|x| **x == a).count();
                out.push(if n == 0 {
                    a.clone()
                } else {
                    format!("{a}-{n}")
                });
                seen.push(a);
            }
        }
    }
    out
}

/// The targets of the inline links `[text](target)` of a line.
fn link_targets(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("](") {
        let after = &rest[i + 2..];
        let Some(end) = after.find(')') else { break };
        let target = after[..end].split_whitespace().next().unwrap_or("");
        out.push(target.to_string());
        rest = &after[end..];
    }
    out
}

#[test]
fn the_documents_are_the_ones_vale_checks() {
    let docs = documents();
    for want in [
        "README.md",
        "AGENTS.md",
        "hydo/dist/linux/QUICKSTART-linux.md",
        "hydo/guide/faults.md",
        "sdk/rust/README.md",
        "hydo/crates/hydo-cli/README.md",
    ] {
        assert!(
            docs.iter().any(|d| d == want),
            "{want} is not covered: {docs:?}"
        );
    }
    assert!(!docs.iter().any(|d| d == "hydo/guide/overview.md"));
}

#[test]
fn internal_links_and_anchors_resolve() {
    let mut problems = Vec::new();
    let mut checked = 0;
    for doc in documents() {
        let text = read(&doc);
        let dir = Path::new(&doc)
            .parent()
            .unwrap_or(Path::new(""))
            .to_path_buf();
        for (n, line) in prose_lines(&text) {
            for target in link_targets(&without_code(line)) {
                if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
                    continue;
                }
                checked += 1;
                let (path, frag) = match target.split_once('#') {
                    Some((p, f)) => (p, Some(f)),
                    None => (target.as_str(), None),
                };
                let file = if path.is_empty() {
                    top().join(&doc)
                } else {
                    top().join(&dir).join(path)
                };
                if !file.exists() {
                    problems.push(format!("{doc}:{n}: {target}: no such file or directory"));
                    continue;
                }
                if let Some(f) = frag {
                    if file.extension().is_some_and(|e| e == "md") {
                        let t = std::fs::read_to_string(&file).unwrap_or_default();
                        if !anchors_of(&t).iter().any(|a| a == f) {
                            problems
                                .push(format!("{doc}:{n}: {target}: no heading with that anchor"));
                        }
                    }
                }
            }
        }
    }
    assert!(checked >= 20, "too few relative links found ({checked})");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn anchors_follow_github() {
    assert_eq!(
        anchor(" 6. Run without the kernel module (KVM)"),
        "6-run-without-the-kernel-module-kvm"
    );
    assert_eq!(anchor(" `hydo diff`: two runs"), "hydo-diff-two-runs");
}
