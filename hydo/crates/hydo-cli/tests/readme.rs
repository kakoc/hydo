//! The crates.io page of `hydo` (README.md) is tested like code (docs/PUBLISHING.md, "The
//! crates' pages").
//!
//! Every `hydo` command the page shows, in a shell block or in a code span, names a command of
//! `hydo --help` (the Linux or the Mac help text in src/main.rs) and only options that the
//! command's lines of the help text name. The commands that need no installed release and no
//! virtual machine run for real: `hydo --version`, `hydo --help`, every `hydo guide <topic>`,
//! and, in an empty directory, `hydo init`, `hydo build hydo/hydo.yaml --check` and
//! `hydo skill install --agent claude`. The version in the example's path is this crate's
//! version, and every link is absolute.

use std::path::Path;
use std::process::Command;

const README: &str = include_str!("../README.md");

/// Options that the help text does not show because the command hands them to a program of its
/// own: `hydo build` passes its options to the image builder, whose usage lists `--check`
/// (the test runs `hydo build hydo/hydo.yaml --check`, below).
const DELEGATED: &[(&str, &str)] = &[("build", "--check")];
const MAIN: &str = include_str!("../src/main.rs");

/// The `hydo ...` commands of the page: the lines of its shell blocks and its code spans.
fn commands() -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = None::<String>;
    for line in README.lines() {
        if let Some(info) = line.strip_prefix("```") {
            fenced = match fenced {
                None => Some(info.to_string()),
                Some(_) => None,
            };
            continue;
        }
        match &fenced {
            Some(info) if info == "sh" => {
                for part in line.split("&&") {
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

/// The help text's lines for `hydo <sub>`: the line that starts with it and the lines that
/// continue it, in both platforms' help texts.
fn help_lines(sub: &str) -> String {
    let mut text = String::new();
    let mut inside = false;
    for line in MAIN.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("hydo ") {
            let word = rest.split_whitespace().next().unwrap_or("");
            inside = word == sub || (sub.starts_with('-') && rest.contains(sub));
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

fn hydo(dir: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
        .args(args)
        .current_dir(dir)
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

#[test]
fn every_command_on_the_page_is_one_hydo_has() {
    let cmds = commands();
    assert!(cmds.len() >= 10, "too few commands found: {cmds:?}");
    let mut problems = Vec::new();
    for c in &cmds {
        let words: Vec<&str> = c.split_whitespace().collect();
        let sub = words[1];
        if sub == "--help" {
            continue; // prints the help text itself (run below)
        }
        let help = help_lines(sub);
        if help.is_empty() {
            problems.push(format!("{c}: `hydo {sub}` is not in hydo's help text"));
            continue;
        }
        for w in &words[2..] {
            if w.starts_with("--") && !help.contains(w) && !DELEGATED.contains(&(sub, *w)) {
                problems.push(format!("{c}: the help text of `hydo {sub}` has no {w}"));
            }
        }
    }
    assert!(problems.is_empty(), "README.md:\n{}", problems.join("\n"));
}

#[test]
fn the_commands_that_need_no_release_run() {
    let tmp = std::env::temp_dir().join(format!("hydo-readme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let (ok, out) = hydo(&tmp, &["--version"]);
    let want = format!("hydo {}", env!("CARGO_PKG_VERSION"));
    assert!(ok && out.starts_with(&want), "hydo --version: {out}");
    let (ok, out) = hydo(&tmp, &["--help"]);
    assert!(ok, "hydo --help: {out}");
    let mut ran = 0;
    for c in commands() {
        let words: Vec<&str> = c.split_whitespace().collect();
        if words[1] == "guide" {
            let (ok, out) = hydo(&tmp, &words[1..]);
            assert!(ok, "{c}: {out}");
            ran += 1;
        }
    }
    assert!(ran >= 3, "the page names fewer guide topics than expected");
    for c in [
        "hydo init",
        "hydo build hydo/hydo.yaml --check",
        "hydo skill install --agent claude",
    ] {
        assert!(
            commands().iter().any(|x| x == c),
            "the page no longer shows `{c}`"
        );
        let words: Vec<&str> = c.split_whitespace().collect();
        let (ok, out) = hydo(&tmp, &words[1..]);
        assert!(ok, "{c}: {out}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn the_example_path_names_this_version_and_links_are_absolute() {
    let want = format!(
        "~/.hydo/versions/{}/examples/redis",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        README.contains(&want),
        "README.md must copy the example from {want} (the version of this crate)"
    );
    for line in README.lines() {
        let mut rest = line;
        while let Some(i) = rest.find("](") {
            let after = &rest[i + 2..];
            let end = after.find(')').unwrap_or(after.len());
            let target = &after[..end];
            assert!(
                target.starts_with("https://"),
                "README.md links to {target:?}: crates.io shows the page outside the repository"
            );
            rest = &after[end..];
        }
    }
}
