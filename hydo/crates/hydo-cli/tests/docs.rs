//! The repository's documents are checked like code (docs/STYLE.md, "How the standard is
//! checked"). The documents are the ones `ci/vale.sh` checks: its `DOCS` patterns without its
//! `EXCEPT` files, read from the script, so the two lists cannot drift apart.
//!
//! - Every relative link of those documents names a file or a directory of the repository, and a
//!   `#anchor` names a heading of the linked Markdown file (GitHub's anchor rules). External links
//!   are checked only by the nightly job of `.github/workflows/docs.yml`, so that this test needs
//!   no network.
//! - The pages a user reads first (README.md and the two quickstarts) state facts of the tree:
//!   every version they name as the current one is the workspace version (so a release that
//!   raises the version must update them, or this test fails); every 16-digit hex value they cite
//!   is in hydo/onboard/recorded.json or in `CITED_ELSEWHERE` with its source; every `hydo`
//!   command they show is in a help text, with only options that help text names.
//! - The README's commands that need no virtual machine and no network run as written, in a
//!   temporary directory (`the_readme_commands_that_need_no_vm_run_as_written`).

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

// ---------------------------------------------------------------- the facts of the user pages

/// The pages a user reads first. Their facts are checked against the tree.
const USER_PAGES: &[&str] = &[
    "README.md",
    "hydo/dist/linux/QUICKSTART-linux.md",
    "hydo/dist/mac/QUICKSTART-mac.md",
];

/// 16-digit hex values the user pages cite that are not in hydo/onboard/recorded.json, each with
/// where it is recorded. Add a value here only with its source.
const CITED_ELSEWHERE: &[(&str, &str)] = &[
    (
        "30e74366625f3474",
        "wfp of the Redis example's clean run on hvx, recorded with fp 5ec14b8b4bdcf367 \
         (docs/HISTORY.md, Milestone 204)",
    ),
    (
        "c24534f57aab6d42",
        "ctl of the same run; docs/HISTORY.md, Milestone 204",
    ),
    (
        "f4de09236076bc87",
        "wfp of the Redis power cut on hvx, recorded with fp 9f8f90a7d14566df (Milestone 204)",
    ),
    (
        "a98cc29af3d645f4",
        "ctl of the Redis power cut; docs/HISTORY.md, Milestone 204",
    ),
];

/// Options that the help text does not show because the command hands them to a program of its
/// own (`hydo build` passes them to the image builder, whose usage is `hydo_build::HELP`).
const DELEGATED: &[(&str, &str)] = &[("build", "--check")];

const MAIN: &str = include_str!("../src/main.rs");

/// The `hydo ...` commands of a page: shell-block lines (split at `&&`) and code spans.
fn commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = None::<String>;
    for line in text.lines() {
        if let Some(info) = line.trim_start().strip_prefix("```") {
            fenced = match fenced {
                None => Some(info.to_string()),
                Some(_) => None,
            };
            continue;
        }
        match &fenced {
            Some(info) if info == "sh" => {
                let code = line.split(" #").next().unwrap_or("");
                for part in code.split("&&") {
                    let part = part.trim();
                    if part.starts_with("hydo ") {
                        out.push(part.to_string());
                    }
                }
            }
            Some(_) => {}
            None => {
                for (i, span) in line.split('`').enumerate() {
                    if i % 2 == 1 && span.starts_with("hydo ") {
                        out.push(span.to_string());
                    }
                }
            }
        }
    }
    out
}

/// The lines of both help texts that belong to `hydo <sub>`: its lines and their continuations.
/// A line for several commands names them with `|` (`hydo run|repro|explore ...`).
fn help_lines(sub: &str) -> String {
    let mut text = String::new();
    let mut inside = false;
    for line in MAIN.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("hydo ") {
            let word = rest.split_whitespace().next().unwrap_or("");
            inside = word.split('|').any(|w| w == sub);
        } else if t.is_empty() || !line.starts_with(' ') {
            inside = false;
        }
        if inside {
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

/// What `hydo <sub> --help` prints, for the commands that have a help text of their own
/// (`hydo inspect`, `hydo sql-gen`, `hydo compose`, ...).
fn own_help(sub: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("hydo-docs-help-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_hydo"))
        .args([sub, "--help"])
        .current_dir(&tmp)
        .env("HYDO_HOME", &tmp)
        .env("HYDO_OFFLINE", "1")
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The versions a page names as the current one: release file names, download URLs, the
/// install cache's directory, and the README's "The current release is hydo X".
fn named_versions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in [
        "hydo-linux-",
        "hydo-mac-",
        "/download/v",
        "versions/",
        "current release is hydo ",
    ] {
        let mut rest = text;
        while let Some(i) = rest.find(marker) {
            let after = &rest[i + marker.len()..];
            let v: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let v = v.trim_end_matches('.').to_string();
            if v.split('.').count() == 3 && v.split('.').all(|p| !p.is_empty()) {
                out.push(v);
            }
            rest = after;
        }
    }
    out
}

/// The 16-digit lower-case hex words of a text (fingerprints, seeds, sha256 prefixes).
fn hex16(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_hexdigit() && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            let word_end = j == b.len() || !b[j].is_ascii_alphanumeric();
            let s = &text[i..j];
            if j - i == 16 && word_end && !s.chars().any(|c| c.is_ascii_uppercase()) {
                out.push(s.to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn the_user_pages_name_this_version() {
    let want = env!("CARGO_PKG_VERSION");
    let mut problems = Vec::new();
    for page in USER_PAGES {
        let named = named_versions(&read(page));
        assert!(!named.is_empty(), "{page} names no version");
        for v in named {
            let p = format!("{page} names {v} as the current version; the workspace is {want}");
            if v != want && !problems.contains(&p) {
                problems.push(p);
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_cited_fingerprint_is_recorded() {
    let recorded = read("hydo/onboard/recorded.json");
    let mut problems = Vec::new();
    let mut n = 0;
    for page in USER_PAGES {
        for v in hex16(&read(page)) {
            n += 1;
            if !recorded.contains(&v) && !CITED_ELSEWHERE.iter().any(|(x, _)| *x == v) {
                problems.push(format!(
                    "{page} cites {v}, which is neither in hydo/onboard/recorded.json nor in \
                     CITED_ELSEWHERE (tests/docs.rs) with its source"
                ));
            }
        }
    }
    assert!(n >= 10, "too few values found ({n})");
    for (v, source) in CITED_ELSEWHERE {
        assert!(
            USER_PAGES.iter().any(|p| read(p).contains(v)),
            "{v} ({source}) is cited by no page any more; remove it from CITED_ELSEWHERE"
        );
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_command_on_the_user_pages_is_one_hydo_has() {
    let mut problems = Vec::new();
    let mut n = 0;
    for page in USER_PAGES {
        for c in commands(&read(page)) {
            n += 1;
            let words: Vec<&str> = c.split_whitespace().collect();
            let sub = words[1];
            if sub.starts_with('-') || sub.starts_with('<') || sub.starts_with('"') {
                continue;
            }
            let mut help = help_lines(sub);
            let missing = words[2..]
                .iter()
                .any(|w| w.starts_with("--") && !help.contains(*w));
            if help.is_empty() || missing {
                let own = own_help(sub);
                if own.contains(&format!("hydo {sub}")) || own.contains(&format!("hydo-{sub}")) {
                    help.push_str(&own);
                }
            }
            // `hydo build` and `hydo explore` hand their options to the image builder and the
            // explorer, whose own usage texts list them
            if sub == "build" {
                help.push_str(hydo_build::HELP);
            }
            if sub == "explore" {
                help.push_str(hydo_explore::args::USAGE);
            }
            if help.is_empty() {
                problems.push(format!("{page}: {c}: `hydo {sub}` is in no help text"));
                continue;
            }
            for w in &words[2..] {
                let w = w.trim_end_matches(['.', ',', ';', ')']);
                let opt = w.split('=').next().unwrap_or(w);
                if opt.starts_with("--") && !help.contains(opt) && !DELEGATED.contains(&(sub, opt))
                {
                    problems.push(format!(
                        "{page}: {c}: the help of `hydo {sub}` has no {opt}"
                    ));
                }
            }
        }
    }
    assert!(n >= 20, "too few commands found ({n})");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_fact_checks_see_what_a_page_says() {
    let page = "Install `hydo-linux-9.9.9.tar.gz` from /download/v9.9.9/.\n\n\
                ```sh\nhydo run --no-such-option \"disk=none | run\"  # a comment\n```\n\n\
                It gave `0123456789abcdef` and fp=fedcba9876543210.\n";
    assert_eq!(named_versions(page), ["9.9.9", "9.9.9"]);
    assert_eq!(hex16(page), ["0123456789abcdef", "fedcba9876543210"]);
    let c = commands(page);
    assert_eq!(c, ["hydo run --no-such-option \"disk=none | run\""]);
    assert!(!help_lines("run").contains("--no-such-option"));
    assert!(help_lines("run").contains("--image"));
}

// ---------------------------------------------------------------- the README's commands run

/// Runs `hydo` in `dir`, offline, with an install cache of its own, and returns whether it
/// succeeded and what it printed.
fn run_hydo(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_hydo"))
        .args(args)
        .current_dir(dir)
        .env("HYDO_HOME", dir.join(".hydo-home"))
        .env("HYDO_OFFLINE", "1")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.success(), text)
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-docs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The commands of the README's "Test your own project" that need no virtual machine run as
/// written: `hydo init`, `hydo build hydo/hydo.yaml --check`, every `hydo guide <topic>` the
/// README names, and `hydo compose` on a small Compose file. The Compose test copies a recorded
/// lock file (tests/fixtures/docs-compose/) next to the output, so that the importer pins the
/// image from it and needs no registry. `sh hydo/fetch-images.sh`, `hydo run` and `hydo explore`
/// need the network or a virtual machine; their output is cited from recorded runs instead.
#[test]
fn the_readme_commands_that_need_no_vm_run_as_written() {
    let readme = read("README.md");
    let cmds = commands(&readme);
    for want in [
        "hydo init",
        "hydo build hydo/hydo.yaml --check",
        "hydo compose docker-compose.yaml --out hydo/hydo.yaml --images-dir images",
        "hydo guide workflow",
        "hydo guide faults",
        "hydo guide manifest",
        "hydo guide sdks",
        "hydo guide skill",
    ] {
        assert!(
            cmds.iter().any(|c| c == want),
            "README.md no longer shows `{want}`; update this test with the page"
        );
    }

    // hydo init, then the check of the skeleton it wrote
    let d = scratch("init");
    let (ok, out) = run_hydo(&d, &["init"]);
    assert!(ok, "hydo init: {out}");
    for f in [
        "hydo/hydo.yaml",
        "hydo/workload",
        "hydo/checks",
        "hydo/regressions",
        ".gitignore",
        "AGENTS.md",
    ] {
        assert!(d.join(f).exists(), "hydo init did not create {f}: {out}");
    }
    let (ok, out) = run_hydo(&d, &["build", "hydo/hydo.yaml", "--check"]);
    assert!(ok, "hydo build hydo/hydo.yaml --check: {out}");

    // every guide topic the README names
    let mut topics = 0;
    for c in &cmds {
        let words: Vec<&str> = c.split_whitespace().collect();
        if words.get(1) == Some(&"guide") {
            let (ok, out) = run_hydo(&d, &words[1..]);
            assert!(ok, "{c}: {out}");
            topics += 1;
        }
    }
    assert!(
        topics >= 5,
        "the README names fewer guide topics than expected"
    );
    let _ = std::fs::remove_dir_all(&d);

    // hydo compose, as written, after hydo init
    let d = scratch("compose");
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs-compose");
    std::fs::copy(
        fx.join("docker-compose.yaml"),
        d.join("docker-compose.yaml"),
    )
    .unwrap();
    let (ok, out) = run_hydo(&d, &["init"]);
    assert!(ok, "hydo init: {out}");
    std::fs::copy(
        fx.join("hydo-compose.lock.json"),
        d.join("hydo/hydo-compose.lock.json"),
    )
    .unwrap();
    let args = [
        "compose",
        "docker-compose.yaml",
        "--out",
        "hydo/hydo.yaml",
        "--images-dir",
        "images",
    ];
    let (ok, out) = run_hydo(&d, &args);
    assert!(ok, "hydo compose: {out}");
    assert!(
        d.join("hydo/fetch-images.sh").exists(),
        "hydo compose wrote no hydo/fetch-images.sh: {out}"
    );
    let yaml = std::fs::read_to_string(d.join("hydo/hydo.yaml")).unwrap();
    assert!(
        yaml.contains("cache"),
        "hydo/hydo.yaml lacks the service: {yaml}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

// ---------------------------------------------------------------- the release's docs review

/// docs/RELEASING.md has the docs review as a step before the tag, and the reviewer's prompt it
/// points to is in the hydo-docs skill, so every release gives the reader the same prompt.
#[test]
fn the_release_docs_review_is_a_release_step_with_one_prompt() {
    let releasing = read("docs/RELEASING.md");
    let review = releasing
        .find("## 6a. Docs review")
        .expect("docs/RELEASING.md has no step `## 6a. Docs review`");
    let publish = releasing
        .find("## 7. Publish")
        .expect("docs/RELEASING.md has no step 7");
    assert!(
        review < publish,
        "the docs review must come before the tag (step 7)"
    );
    let step = &releasing[review..publish];
    for want in [
        "skills/hydo-docs/SKILL.md",
        "The release docs review",
        "release notes",
        "docs::",
    ] {
        assert!(step.contains(want), "step 6a no longer names {want}");
    }
    let skill = read("skills/hydo-docs/SKILL.md");
    let section = skill
        .find("## The release docs review")
        .expect("skills/hydo-docs/SKILL.md has no section `The release docs review`");
    let rest = &skill[section..];
    let prompt = rest
        .split("```text\n")
        .nth(1)
        .and_then(|p| p.split("```").next())
        .expect("the section has no ```text block with the prompt");
    for want in ["README.md", "quickstart", "file and line"] {
        assert!(
            prompt.contains(want),
            "the reviewer's prompt no longer says {want}"
        );
    }
}

// ---------------------------------------------------------------- the pull-request form

/// The pull-request form asks, with one checkbox, whether the documentation is updated or why
/// no page needs a change, and CONTRIBUTING.md points to it.
#[test]
fn the_pull_request_form_asks_for_the_docs() {
    let form = read(".github/pull_request_template.md");
    let boxes: Vec<&str> = form.lines().filter(|l| l.starts_with("- [ ]")).collect();
    assert_eq!(
        boxes.len(),
        1,
        "the form must have exactly one checkbox: {boxes:?}"
    );
    assert!(
        boxes[0].contains("documentation") && boxes[0].contains("Reason"),
        "the checkbox must ask about the documentation and for a reason: {}",
        boxes[0]
    );
    assert!(
        read("CONTRIBUTING.md").contains(".github/pull_request_template.md"),
        "CONTRIBUTING.md does not mention the pull-request form"
    );
}

/// `hydo run --help` prints the run's usage and exits 0 (before 0.3.0 it looked for an image and
/// failed with "no image given"); the usage states the exit-status rule the quickstarts give.
#[test]
fn run_help_prints_the_usage() {
    let d = scratch("run-help");
    let (ok, out) = run_hydo(&d, &["run", "--help"]);
    assert!(ok, "{out}");
    assert!(out.starts_with("usage: hydo run "), "{out}");
    assert!(out.contains("verdict.kind"), "{out}");
    let _ = std::fs::remove_dir_all(&d);
}
