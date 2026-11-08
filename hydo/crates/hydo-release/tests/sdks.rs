//! The SDKs as packages for their registries (Milestone 160; docs/PUBLISHING.md).
//!
//! 1. The public Go module `sdk/go` and the frozen copy in `hydo/stack` (whose compiled bytes are
//!    inside recorded images) hold the same code: they may differ only in the documented
//!    differences below (`sdk/README.md` lists them too): the renamed identifiers of
//!    `GO_RENAMES` (Milestone 163 made the assertion type unexported in `sdk/go` only) and the
//!    lines of `GO_DIFFERENCES`.
//! 2. Every published SDK package (the four assertion SDKs: crates.io, PyPI and npm
//!    `hydo-assert`, the Go module `sdk/go`) has the release's version, the license Apache-2.0,
//!    the repository's URL and nothing that blocks publishing, and carries the repository's
//!    `LICENSE` text; the Rust test SDK's crates (`hydo-test`, `hydo-test-macros`,
//!    `cargo-hydo`) keep `publish = false` until the user decides about code-first tests.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const REPOSITORY: &str = "https://github.com/kakoc/hydo";
const GO_MODULE: &str = "github.com/kakoc/hydo/sdk/go";

/// The only differences between `sdk/go/<file>` and `hydo/stack/<file>`: (file, text in sdk/go,
/// text in hydo/stack). Each text occurs exactly once in its file.
const GO_DIFFERENCES: &[(&str, &str, &str)] = &[
    // the module path: `hydostack` is the order service's module, kept for its recorded bytes
    (
        "hydoassert/cases_test.go",
        "\t\"github.com/kakoc/hydo/sdk/go/hydoassert\"\n",
        "\t\"hydostack/hydoassert\"\n",
    ),
    (
        "hydoassert/cases_test.go",
        "\t\"github.com/kakoc/hydo/sdk/go/hydohist\"\n",
        "\t\"hydostack/hydohist\"\n",
    ),
    // where the shared cases are, seen from the package directory
    (
        "hydoassert/cases_test.go",
        "const casesDir = \"../../cases\"\n",
        "const casesDir = \"../../../sdk/cases\"\n",
    ),
    // the comment that says where to run the tests
    (
        "hydoassert/cases_test.go",
        "(in sdk/go)\n",
        "(in hydo/stack/hydoassert)\n",
    ),
];

/// Identifiers that `sdk/go` spells differently from `hydo/stack`: (name in sdk/go, name in
/// hydo/stack). Milestone 163 made the assertion type and its constants unexported in the
/// public module (no call of the API takes or returns them); the frozen copy keeps the exported
/// names because its bytes are inside recorded images. The test maps every whole-word occurrence
/// in `sdk/go` back before it compares, and it fails when a `hydo/stack` name is still a word in
/// `sdk/go` (the rename must be complete) or an `sdk/go` name is a word in `hydo/stack` (the
/// mapping must be one to one).
const GO_RENAMES: &[(&str, &str)] = &[
    ("kind", "Type"),
    ("tAlways", "TAlways"),
    ("tAlwaysOrUnreachable", "TAlwaysOrUnreachable"),
    ("tSometimes", "TSometimes"),
    ("tReachable", "TReachable"),
    ("tUnreachable", "TUnreachable"),
    ("tEventually", "TEventually"),
];

/// The package whose files `GO_RENAMES` applies to.
const GO_RENAMED_PACKAGE: &str = "hydoassert";

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Byte offsets of `word` in `text` where it is a whole identifier.
fn word_positions(text: &str, word: &str) -> Vec<usize> {
    text.match_indices(word)
        .map(|(i, _)| i)
        .filter(|&i| {
            let before = text[..i].chars().next_back();
            let after = text[i + word.len()..].chars().next();
            !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
        })
        .collect()
}

fn replace_word(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for i in word_positions(text, from) {
        out.push_str(&text[last..i]);
        out.push_str(to);
        last = i + from.len();
    }
    out.push_str(&text[last..]);
    out
}

/// Files that only the frozen copy has (the shell SDK is not part of the Go module).
const GO_FROZEN_ONLY: &[&str] = &["hydoassert/hydo_assert.sh"];

/// Files that only the public module has: the examples of its package documentation and the
/// test of its README. They are test files, so no program is compiled from them, and the frozen
/// copy needs no documentation.
const GO_PUBLIC_ONLY: &[&str] = &["hydoassert/example_test.go", "hydohist/example_test.go"];

/// The Go packages of the module.
const GO_PACKAGES: &[&str] = &["hydoassert", "hydohist"];

fn repo() -> PathBuf {
    hydo_release::model::repo_root()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn files_in(dir: &Path, pkg: &str) -> BTreeSet<String> {
    std::fs::read_dir(dir.join(pkg))
        .unwrap_or_else(|e| panic!("{}: {e}", dir.join(pkg).display()))
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| format!("{pkg}/{}", e.file_name().to_string_lossy()))
        .collect()
}

fn first_difference(a: &str, b: &str) -> String {
    for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
        if x != y {
            return format!(
                "line {}:\n    sdk/go:     {x:?}\n    hydo/stack: {y:?}",
                i + 1
            );
        }
    }
    format!(
        "the files have {} and {} lines",
        a.lines().count(),
        b.lines().count()
    )
}

#[test]
fn go_sdk_equals_the_frozen_copy_in_hydo_stack() {
    let r = repo();
    let (public, frozen) = (r.join("sdk/go"), r.join("hydo/stack"));
    let mut problems = Vec::new();
    for pkg in GO_PACKAGES {
        let (p, f) = (files_in(&public, pkg), files_in(&frozen, pkg));
        for only in p.difference(&f) {
            if GO_PUBLIC_ONLY.contains(&only.as_str()) {
                continue;
            }
            problems.push(format!("sdk/go/{only} has no counterpart in hydo/stack/ (add it there too, or document why not here)"));
        }
        for only in f.difference(&p) {
            if !GO_FROZEN_ONLY.contains(&only.as_str()) {
                problems.push(format!("hydo/stack/{only} has no counterpart in sdk/go/ (copy it, or list it in GO_FROZEN_ONLY with the reason)"));
            }
        }
        for file in p.intersection(&f) {
            let mut a = read(&public.join(file));
            let b = read(&frozen.join(file));
            if file.starts_with(&format!("{GO_RENAMED_PACKAGE}/")) {
                for (ours, theirs) in GO_RENAMES {
                    if !word_positions(&a, theirs).is_empty() {
                        problems.push(format!("sdk/go/{file}: {theirs} is still a word here; sdk/go spells it {ours} (GO_RENAMES)"));
                    }
                    if !word_positions(&b, ours).is_empty() {
                        problems.push(format!("hydo/stack/{file}: {ours} is a word here, so the rename {ours} -> {theirs} is not one to one (GO_RENAMES)"));
                    }
                    a = replace_word(&a, ours, theirs);
                }
            }
            for (_, ours, theirs) in GO_DIFFERENCES.iter().filter(|d| d.0 == file) {
                let (na, nb) = (a.matches(ours).count(), b.matches(theirs).count());
                if na != 1 || nb != 1 {
                    problems.push(format!(
                        "{file}: the documented difference {ours:?} / {theirs:?} occurs {na} times in sdk/go and {nb} times in hydo/stack (once each expected)"
                    ));
                    continue;
                }
                a = a.replacen(ours, theirs, 1);
            }
            if a != b {
                problems.push(format!(
                    "sdk/go/{file} and hydo/stack/{file} differ beyond the documented differences, {}",
                    first_difference(&a, &b)
                ));
            }
        }
    }
    let gomod = read(&public.join("go.mod"));
    if gomod.lines().next() != Some(format!("module {GO_MODULE}").as_str()) {
        problems.push(format!(
            "sdk/go/go.mod must start with `module {GO_MODULE}`"
        ));
    }
    assert!(
        problems.is_empty(),
        "the public Go module sdk/go and the frozen copy in hydo/stack drifted apart (sdk/README.md, \"The Go module and the frozen copy\"; a real change goes into both, and a change to hydo/stack moves recorded fingerprints):\n{}",
        problems.join("\n")
    );
}

/// `key = "value"` of one TOML table (`[package]`, `[project]`), for these simple manifests.
fn toml_field(text: &str, table: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            inside = l == format!("[{table}]");
            continue;
        }
        if !inside {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            if k.trim() == key {
                let v = v.trim();
                return Some(
                    v.strip_prefix('"')
                        .and_then(|v| v.split('"').next())
                        .unwrap_or(v)
                        .to_string(),
                );
            }
        }
    }
    None
}

#[test]
fn sdk_packages_share_version_license_and_repository() {
    let r = repo();
    let version = read(&r.join("hydo/VERSION")).trim().to_string();
    let workspace = toml_field(&read(&r.join("Cargo.toml")), "workspace.package", "version");
    let mut problems = Vec::new();
    if workspace.as_deref() != Some(version.as_str()) {
        problems.push(format!(
            "hydo/VERSION is {version} but the workspace version is {workspace:?}"
        ));
    }
    // The published crate. The Rust test SDK's crates stay unpublished (below).
    let crates = [("sdk/rust", "hydo-assert")];
    for (dir, name) in crates {
        let path = format!("{dir}/Cargo.toml");
        let t = read(&r.join(&path));
        let want = [
            ("name", name.to_string()),
            ("version", version.clone()),
            ("license", "Apache-2.0".into()),
            ("repository", REPOSITORY.into()),
            ("readme", "README.md".into()),
        ];
        for (k, v) in want {
            let got = toml_field(&t, "package", k);
            if got.as_deref() != Some(v.as_str()) {
                problems.push(format!("{path}: [package] {k} is {got:?}, expected {v:?}"));
            }
        }
        for k in ["description", "include"] {
            if toml_field(&t, "package", k).is_none() {
                problems.push(format!("{path}: [package] has no {k}"));
            }
        }
        if toml_field(&t, "package", "publish").is_some() {
            problems.push(format!(
                "{path}: [package] publish must not be set (it blocks `cargo publish`)"
            ));
        }
        if !t.contains("\"LICENSE\"") || !r.join(dir).join("README.md").is_file() {
            problems.push(format!(
                "{path}: the package must include LICENSE and have a README.md"
            ));
        }
    }
    for dir in [
        "sdk/rust-test",
        "sdk/rust-test/macros",
        "sdk/rust-test/cargo-hydo",
    ] {
        let path = format!("{dir}/Cargo.toml");
        if toml_field(&read(&r.join(&path)), "package", "publish").as_deref() != Some("false") {
            problems.push(format!(
                "{path}: [package] publish = false is required: the Rust test SDK is not published until the user decides about code-first tests (docs/PUBLISHING.md)"
            ));
        }
    }

    let py = read(&r.join("sdk/python/pyproject.toml"));
    for (k, v) in [
        ("name", "hydo-assert"),
        ("version", version.as_str()),
        ("license", "Apache-2.0"),
    ] {
        let got = toml_field(&py, "project", k);
        if got.as_deref() != Some(v) {
            problems.push(format!(
                "sdk/python/pyproject.toml: [project] {k} is {got:?}, expected {v:?}"
            ));
        }
    }
    if !py.contains(&format!("Repository = \"{REPOSITORY}\"")) || py.contains("Private ::") {
        problems.push("sdk/python/pyproject.toml: needs Repository in [project.urls] and no `Private ::` classifier".into());
    }
    let init = read(&r.join("sdk/python/src/hydo_assert/__init__.py"));
    if !init.contains(&format!("__version__ = \"{version}\"")) {
        problems.push(format!(
            "sdk/python/src/hydo_assert/__init__.py: __version__ must be \"{version}\""
        ));
    }

    let js: serde_json::Value =
        serde_json::from_str(&read(&r.join("sdk/js/package.json"))).unwrap();
    for (k, v) in [
        ("name", "hydo-assert"),
        ("version", version.as_str()),
        ("license", "Apache-2.0"),
    ] {
        if js[k] != v {
            problems.push(format!(
                "sdk/js/package.json: {k} is {}, expected {v:?}",
                js[k]
            ));
        }
    }
    if js.get("private").is_some() || js.get("publishConfig").is_some() {
        problems.push("sdk/js/package.json: no `private` (it blocks `npm publish`) and no `publishConfig` (name the registry on the command line)".into());
    }
    if js["repository"]["url"] != format!("git+{REPOSITORY}.git")
        || js["repository"]["directory"] != "sdk/js"
    {
        problems.push(format!(
            "sdk/js/package.json: repository must be git+{REPOSITORY}.git, directory sdk/js"
        ));
    }

    let license = read(&r.join("LICENSE"));
    for dir in ["sdk/rust", "sdk/python", "sdk/js", "sdk/go"] {
        if std::fs::read_to_string(r.join(dir).join("LICENSE"))
            .ok()
            .as_deref()
            != Some(license.as_str())
        {
            problems.push(format!(
                "{dir}/LICENSE must be a copy of the repository's LICENSE"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "the SDK packages are not ready for their registries (docs/PUBLISHING.md; every SDK has the release's version, so a release bumps them together):\n{}",
        problems.join("\n")
    );
}

/// The four SDKs' pages (their README.md, shown by crates.io, PyPI, npm and pkg.go.dev) have
/// the same parts in the same order, and their complete programs write the same lines: the same
/// program in each language (sdk/README.md, "The pages are tested"; each SDK's own test runs
/// its program and compares the lines).
#[test]
fn the_four_sdk_pages_are_parallel() {
    let r = repo();
    let pages = ["sdk/rust", "sdk/python", "sdk/js", "sdk/go"];
    let mut outlines = Vec::new();
    let mut outputs = Vec::new();
    for p in pages {
        let text = read(&r.join(p).join("README.md"));
        // the level-2 headings, and the level-3 headings of "How to" but its last (each
        // language's own "build or run a program for a hydo guest")
        let mut h2 = Vec::new();
        let mut how = Vec::new();
        let mut in_how = false;
        for l in text.lines() {
            if let Some(h) = l.strip_prefix("## ") {
                in_how = h == "How to";
                h2.push(h.to_string());
            } else if let Some(h) = l.strip_prefix("### ") {
                if in_how {
                    how.push(h.to_string());
                }
            }
        }
        how.pop();
        outlines.push((p, h2, how));
        let start = text
            .find("\n## A complete program\n")
            .unwrap_or_else(|| panic!("{p}/README.md has no \"A complete program\""));
        let rest = &text[start..];
        let i = rest.find("\n```text\n").expect("the program's lines") + 9;
        let end = rest[i..].find("\n```\n").unwrap();
        outputs.push((p, rest[i..i + end].to_string()));
    }
    for (p, h2, how) in &outlines[1..] {
        assert_eq!(
            (h2, how),
            (&outlines[0].1, &outlines[0].2),
            "{p}/README.md has other parts than sdk/rust/README.md"
        );
    }
    for (p, out) in &outputs[1..] {
        assert_eq!(
            out, &outputs[0].1,
            "{p}/README.md: its complete program writes other lines than sdk/rust/README.md's"
        );
    }
}
