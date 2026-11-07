//! No machine access details in tracked files (AGENTS.md, "Safety and coordination rules";
//! Milestone 151). Every file `git ls-files` lists is read.
//!
//! Generic rules (this file names no concrete value), checked on every line except in recorded
//! data (test fixtures, recorded answers, example documents: other tests or images compare their
//! bytes) and in the few files listed in `ALLOWED`. A line fails when it holds
//!
//! 1. a remote-shell or remote-copy command word followed by a blank (the scripts reach a machine
//!    only through `machines.sh`, whose details live in the git-ignored `machines.local.sh`);
//! 2. an IPv4 address outside the ranges that name no machine: 0.0.0.0/8, 10.0.0.0/8 (the
//!    guests' own networks), 127.0.0.0/8, the netmasks 255.x.x.x and the documentation ranges of
//!    RFC 5737;
//! 3. the words for the company that rents a machine out.
//!
//! Concrete values: the git-ignored `AGENTS.local.md` (at the repository root, or in a git
//! worktree without its own copy the main checkout's) lists the real aliases, addresses, logins
//! and names between the lines `<!-- public-content-forbidden: begin -->` and
//! `<!-- public-content-forbidden: end -->`, one `` - `value` `` per line. When the file exists,
//! no tracked file at all (recorded data included) may contain one of them as a word
//! (case-insensitive). Without the file (a fresh clone, a release tarball) that part is skipped.
//! Run from a tree that is not a git checkout, the test only says so.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path prefixes the generic rules skip: recorded data whose bytes other tests compare with.
const SKIP_PREFIXES: &[&str] = &["hydo/schema/examples/", "sdk/cases/"];
/// Path parts the generic rules skip (anywhere in the path).
const SKIP_PARTS: &[&str] = &["/tests/fixtures/", "/tests/golden/", "/tests/golden."];

/// Files that keep a line matching a generic rule on purpose, with the reason.
const ALLOWED: &[(&str, &str)] = &[
    (
        "hv/hvx/safety/syz/build-image.sh",
        "the syzkaller test VM's own sshd and its keys (syzkaller reaches its QEMU VMs that way)",
    ),
    (
        "hydo/crates/hydo-control/tests/m123.rs",
        "the composer's resolved W=: entries (dotted round, command and seed numbers) look like addresses",
    ),
    (
        "hydo/dist/linux/third-party.json",
        "the name and URL of nixpkgs's bzip2 patch carry its four-part version number, which looks like an address",
    ),
    (
        "hydo/dist/mac/third-party.json",
        "the name and URL of nixpkgs's bzip2 patch carry its four-part version number, which looks like an address",
    ),
];

const BEGIN: &str = "<!-- public-content-forbidden: begin -->";
const END: &str = "<!-- public-content-forbidden: end -->";

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root")
}

fn tracked(root: &Path) -> Option<Vec<String>> {
    Some(
        git(root, &["ls-files", "-z"])?
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// The git-ignored AGENTS.local.md: this checkout's, else the main checkout's (a worktree).
fn local_file(root: &Path) -> Option<PathBuf> {
    let here = root.join("AGENTS.local.md");
    if here.is_file() {
        return Some(here);
    }
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let main = Path::new(common.trim()).parent()?.join("AGENTS.local.md");
    main.is_file().then_some(main)
}

/// The values between the markers, one `- `value`` per line.
fn forbidden_values(text: &str) -> Vec<String> {
    let Some(start) = text.find(BEGIN) else {
        return Vec::new();
    };
    let rest = &text[start + BEGIN.len()..];
    let block = &rest[..rest.find(END).unwrap_or(rest.len())];
    block
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- `")?.split('`').next())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

fn skipped(path: &str) -> bool {
    SKIP_PREFIXES.iter().any(|p| path.starts_with(p))
        || SKIP_PARTS.iter().any(|p| format!("/{path}").contains(p))
        || ALLOWED.iter().any(|(p, _)| *p == path)
}

/// True when the dotted quad names no machine (see the module comment).
fn harmless_ipv4(o: [u32; 4]) -> bool {
    matches!(o[0], 0 | 10 | 127 | 255)
        || (o[0], o[1], o[2]) == (192, 0, 2)
        || (o[0], o[1], o[2]) == (198, 51, 100)
        || (o[0], o[1], o[2]) == (203, 0, 113)
}

struct Rules {
    command: Regex,
    ipv4: Regex,
    provider: Regex,
}

impl Rules {
    fn new() -> Rules {
        // spelled in pieces, so that this file does not match itself
        let (s, c) = ("ss", "sc");
        Rules {
            command: Regex::new(&format!(r"(^|[^A-Za-z0-9_./-])({s}h|{c}p)[ \t]")).unwrap(),
            ipv4: Regex::new(r"(^|[^0-9A-Za-z.])([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})($|[^0-9.])").unwrap(),
            provider: Regex::new(&format!(r"(?i)\bhosting\s+{}", "provider")).unwrap(),
        }
    }

    /// What is wrong with one line, if anything.
    fn check(&self, line: &str) -> Option<&'static str> {
        if self.command.is_match(line) {
            return Some("a remote-shell or remote-copy command");
        }
        if self.provider.is_match(line) {
            return Some("the name for the company that hosts a machine");
        }
        for c in self.ipv4.captures_iter(line) {
            let o = [2, 3, 4, 5].map(|i| c[i].parse::<u32>().unwrap_or(999));
            if o.iter().all(|&x| x <= 255) && !harmless_ipv4(o) {
                return Some("an IPv4 address");
            }
        }
        None
    }
}

/// One case-insensitive whole-word pattern per concrete value.
fn value_patterns(values: &[String]) -> Vec<(String, Regex)> {
    values
        .iter()
        .map(|v| {
            let re = format!(r"(?i)(^|[^A-Za-z0-9]){}($|[^A-Za-z0-9])", regex::escape(v));
            (v.clone(), Regex::new(&re).unwrap())
        })
        .collect()
}

#[test]
fn rules_catch_what_they_should() {
    let r = Rules::new();
    let rsh = format!("{}h", "ss");
    for bad in [
        format!("{rsh} -o BatchMode=yes \"$BOX\" uptime"),
        format!("  ({rsh} box 'ls')"),
        format!("{}p -q a b:c", "sc"),
        format!("copy it to 198.18.{}.7 first", 4),
        format!("someone@172.31.{}.9", 9),
        format!("the hosting {} panel", "provider"),
    ] {
        assert!(r.check(&bad).is_some(), "not caught: {bad}");
    }
    for good in [
        "listen on 127.0.0.1:6379 and 0.0.0.0",
        "eth0 = 10.89.0.3/16, host 10.0.0.1",
        "netmask 255.255.255.0",
        "example 192.0.2.7",
        "version 1.2.3 and v6.10.20.29",
        "/etc/ssh/sshd_config and ~/.ssh/config are files",
        "on_bare_metal \"uptime\"",
        "the bare-metal machine and the build machine",
        "a payment provider answers 500",
    ] {
        assert!(r.check(good).is_none(), "wrongly caught: {good}");
    }
    let text = format!(
        "x\n{BEGIN}\n- `alpha-one`\n- `beta@two`  (a login)\nnot a value\n{END}\n- `gamma`\n"
    );
    let values = forbidden_values(&text);
    assert_eq!(values, ["alpha-one", "beta@two"]);
    let pats = value_patterns(&values);
    assert!(pats[0].1.is_match("rsync f ALPHA-ONE:dir/"));
    assert!(!pats[0].1.is_match("alpha-ones"));
    assert!(pats[1].1.is_match("as beta@two."));
}

#[test]
fn tracked_files_have_no_machine_access_details() {
    let root = repo();
    let Some(files) = tracked(&root) else {
        eprintln!("public_content: not a git checkout (or no git); nothing checked");
        return;
    };
    let values = match local_file(&root) {
        Some(p) => {
            let v = forbidden_values(&std::fs::read_to_string(&p).unwrap_or_default());
            eprintln!(
                "public_content: {} concrete values from {}",
                v.len(),
                p.display()
            );
            v
        }
        None => {
            eprintln!("public_content: no AGENTS.local.md; only the generic rules are checked");
            Vec::new()
        }
    };
    let pats = value_patterns(&values);
    let r = Rules::new();
    let mut problems = Vec::new();
    let mut checked = 0;
    for path in &files {
        let Ok(bytes) = std::fs::read(root.join(path)) else {
            continue; // deleted in the work tree, or a broken symlink
        };
        let text = String::from_utf8_lossy(&bytes);
        let generic = !skipped(path) && !bytes.iter().take(8192).any(|&b| b == 0);
        if generic {
            checked += 1;
        }
        for (n, line) in text.lines().enumerate() {
            let shown = || -> String { line.trim().chars().take(160).collect() };
            if generic {
                if let Some(why) = r.check(line) {
                    problems.push(format!("{path}:{}: {why}: {}", n + 1, shown()));
                }
            }
            for (_, re) in &pats {
                if re.is_match(line) {
                    problems.push(format!(
                        "{path}:{}: a value listed in AGENTS.local.md: {}",
                        n + 1,
                        shown()
                    ));
                }
            }
        }
    }
    assert!(
        checked > 500,
        "only {checked} tracked text files were checked"
    );
    assert!(
        problems.is_empty(),
        "{} line(s) in tracked files name how to reach a machine (AGENTS.md, \"Machines in \
         tracked files\"); move the detail to AGENTS.local.md or machines.local.sh:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
