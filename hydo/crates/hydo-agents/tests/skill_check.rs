//! The repository check of the skills in `skills/` (it replaced skills/check.py in M108): the
//! Agent Skills specification (https://agentskills.io/specification), the skill's version and its
//! Claude Code plugin entry, and the guide (hydo/guide/) the skill relies on (Milestone 170).
//!
//! 1. skills-ref (https://github.com/agentskills/agentskills/tree/main/skills-ref), when
//!    `$HYDO_SKILLS_REF` names it or `skills-ref` is on PATH: `skills-ref validate <skill>` must
//!    succeed. Without it the rules below are checked anyway.
//! 2. The specification's rules, checked here (skills-ref checks only the first group): name 1-64
//!    chars, lowercase letters, digits and single hyphens, equal to the directory name; description
//!    1-1024 chars; compatibility 1-500 chars; metadata a map of strings; only the allowed
//!    frontmatter keys; SKILL.md under 500 lines; every relative file it links exists and is one
//!    level deep (references/x.md, assets/x, scripts/x).
//! 3. The skill's own release: `metadata.version` and `metadata.hydo-min` are versions, the
//!    marketplace `.claude-plugin/marketplace.json` lists `./skills/<name>` as a plugin of the same
//!    version, and the folder holds nothing Claude Code would load as another plugin component.
//! 4. Every `hydo guide <topic>` that the skill or the guide names is a topic of hydo/guide/ (or
//!    manifest, schema, skill, examples/..., init/...), so the process never points at a fact the
//!    guide lacks.
//! 5. Copies stay in sync: hydo/guide/examples/example-redis-composed/hydo/ equals
//!    hydo/onboard/examples/redis-composed/hydo/ (bytes and executable bits), and the tool
//!    directories .claude/skills/<name> and .agents/skills/<name> are symlinks to skills/<name>.
//!
//! It is a test, not a `hydo` command: a release carries the skill inside the `hydo` binary
//! (hydo-agents' build.rs), so there is no skill folder or repository copy for a user to check;
//! this is the developers' check of the repository's folder. Run it with
//! `cargo test -p hydo-agents --test skill_check -- --nocapture` to see what it checked.
//! The negative tests below run it on a copy of the relevant files with one thing broken.

use hydo_manifest::py::repr_str;
use regex::Regex;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const ALLOWED: [&str; 6] = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
];

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("the repository root")
}

/// A frontmatter value: a string, or a map of strings (an empty value opens a map).
#[derive(Debug)]
enum V {
    S(String),
    M(BTreeMap<String, String>),
}

fn frontmatter(text: &str) -> Option<Vec<(String, V)>> {
    let m = Regex::new(r"(?s)\A---\n(.*?)\n---\n")
        .unwrap()
        .captures(text)?;
    let key_re = Regex::new(r"^\S[^:]*:").unwrap();
    let mut fm: Vec<(String, V)> = Vec::new();
    let mut key: Option<String> = None;
    for line in m[1].split('\n') {
        if key_re.is_match(line) {
            let (k, v) = line.split_once(':').unwrap();
            let (k, v) = (k.trim().to_string(), v.trim());
            let val = if v.is_empty() {
                V::M(BTreeMap::new())
            } else {
                V::S(v.to_string())
            };
            fm.retain(|(x, _)| *x != k);
            fm.push((k.clone(), val));
            key = Some(k);
        } else if line.starts_with("  ") {
            if let Some((_, V::M(map))) = fm.iter_mut().find(|(x, _)| Some(x) == key.as_ref()) {
                let (k, v) = line.trim().split_once(':').unwrap_or((line.trim(), ""));
                map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
            }
        }
    }
    Some(fm)
}

fn get<'a>(fm: &'a [(String, V)], k: &str) -> Option<&'a V> {
    fm.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

/// What the check found: problems, and what it checked.
#[derive(Default)]
struct Report {
    bad: Vec<String>,
    info: Vec<String>,
}

fn normpath(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn check_skill(d: &Path, r: &mut Report) {
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read_to_string(d.join("SKILL.md")).expect("SKILL.md");
    let Some(fm) = frontmatter(&text) else {
        r.bad.push(format!("{name}: no YAML frontmatter"));
        return;
    };
    for (k, _) in &fm {
        if !ALLOWED.contains(&k.as_str()) {
            r.bad.push(format!(
                "{name}: frontmatter key {} not in the specification",
                repr_str(k)
            ));
        }
    }
    let n = match get(&fm, "name") {
        Some(V::S(s)) => s.clone(),
        _ => String::new(),
    };
    let name_re = Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").unwrap();
    if !((1..=64).contains(&n.chars().count()) && name_re.is_match(&n)) {
        r.bad.push(format!(
            "{name}: name {} breaks the naming rules",
            repr_str(&n)
        ));
    }
    if n != name {
        r.bad.push(format!(
            "{name}: name {} is not the directory name",
            repr_str(&n)
        ));
    }
    let dlen = match get(&fm, "description") {
        Some(V::S(s)) => s.chars().count() as i64,
        None => 0,
        Some(V::M(_)) => -1,
    };
    if !(1..=1024).contains(&dlen) {
        r.bad.push(format!(
            "{name}: description must be 1-1024 characters ({dlen})"
        ));
    }
    match get(&fm, "compatibility") {
        None => {}
        Some(V::S(s)) if (1..=500).contains(&s.chars().count()) => {}
        Some(_) => r
            .bad
            .push(format!("{name}: compatibility must be 1-500 characters")),
    }
    if let Some(V::S(_)) = get(&fm, "metadata") {
        r.bad
            .push(format!("{name}: metadata must be a map of strings"));
    }
    let lines = text.matches('\n').count();
    if lines >= 500 {
        r.bad.push(format!(
            "{name}: SKILL.md has {lines} lines (keep it under 500)"
        ));
    }
    let scheme = Regex::new(r"^[a-z]+:").unwrap();
    for c in Regex::new(r"\]\(([^)#]+)\)").unwrap().captures_iter(&text) {
        let link = &c[1];
        if scheme.is_match(link) {
            continue;
        }
        let p = normpath(&d.join(link));
        if !p.exists() {
            r.bad.push(format!(
                "{name}: SKILL.md links {link}, which does not exist"
            ));
        } else if link.matches('/').count() > 1 {
            r.bad.push(format!(
                "{name}: SKILL.md links {link}, deeper than one level"
            ));
        }
    }
    r.info.push(format!(
        "rules: {name}: name, description ({dlen} chars), compatibility, metadata, {lines} lines, links"
    ));
}

fn skills_ref_exe() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HYDO_SKILLS_REF").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("skills-ref"))
            .find(|p| p.is_file())
    })
}

fn skills_ref(d: &Path, exe: &Path, r: &mut Report) {
    match std::process::Command::new(exe)
        .arg("validate")
        .arg(d)
        .output()
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            r.info.push(format!("skills-ref: {}", s.trim()));
            if !o.status.success() {
                r.bad.push(format!("skills-ref rejected {}", d.display()));
            }
        }
        Err(e) => r.bad.push(format!("skills-ref ({}): {e}", exe.display())),
    }
}

fn names(d: &Path) -> BTreeMap<String, PathBuf> {
    std::fs::read_dir(d)
        .map(|it| {
            it.flatten()
                .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                .collect()
        })
        .unwrap_or_default()
}

/// The differences between two trees: names on one side only, files whose bytes or executable
/// bit differ, and names that are a file on one side and a directory on the other.
fn same_tree(a: &Path, b: &Path) -> Vec<String> {
    let (na, nb) = (names(a), names(b));
    let mut diff = Vec::new();
    for n in na.keys().chain(nb.keys().filter(|k| !na.contains_key(*k))) {
        let (Some(pa), Some(pb)) = (na.get(n), nb.get(n)) else {
            diff.push(n.clone());
            continue;
        };
        let (ma, mb) = (std::fs::metadata(pa), std::fs::metadata(pb));
        let (Ok(ma), Ok(mb)) = (ma, mb) else {
            diff.push(n.clone());
            continue;
        };
        if ma.is_dir() && mb.is_dir() {
            diff.extend(same_tree(pa, pb).into_iter().map(|x| format!("{n}/{x}")));
        } else if ma.is_file() && mb.is_file() {
            if std::fs::read(pa).ok() != std::fs::read(pb).ok() {
                diff.push(n.clone());
            } else if (ma.permissions().mode() & 0o100) != (mb.permissions().mode() & 0o100) {
                diff.push(format!("{n} (executable bit)"));
            }
        } else {
            diff.push(n.clone());
        }
    }
    diff
}

/// The names `hydo guide` answers: the guide's top-level topics, the built-in ones, and the
/// `examples/` and `init/` paths.
fn guide_names(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = names(&root.join("hydo/guide"))
        .into_keys()
        .filter_map(|n| n.strip_suffix(".md").map(str::to_string))
        .collect();
    v.extend(["manifest", "schema", "skill", "examples", "init"].map(String::from));
    v
}

/// Every `hydo guide <name>` written in the files below `dir` names something the guide has.
fn guide_mentions(root: &Path, dir: &Path, what: &str, r: &mut Report) {
    let known = guide_names(root);
    let re = Regex::new(r"hydo guide ([a-z][a-z0-9-]*)").unwrap();
    let mut stack = vec![dir.to_path_buf()];
    let mut n = 0;
    while let Some(d) = stack.pop() {
        for (name, p) in names(&d) {
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            for c in re.captures_iter(&text) {
                n += 1;
                if !known.iter().any(|k| *k == c[1]) {
                    r.bad.push(format!(
                        "{what}/{}: `hydo guide {}` is not a topic of hydo/guide/",
                        p.strip_prefix(dir).unwrap_or(Path::new(&name)).display(),
                        &c[1]
                    ));
                }
            }
        }
    }
    r.info.push(format!(
        "topics: {what}: {n} `hydo guide <topic>` mentions, all known"
    ));
}

/// The skill's version and the oldest hydo it supports (`metadata.version`, `metadata.hydo-min`)
/// are dotted numbers, and the Claude Code marketplace lists the skill folder as a plugin of the
/// same version, with nothing in the folder that Claude Code would load as another component.
fn check_plugin(root: &Path, d: &Path, r: &mut Report) {
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read_to_string(d.join("SKILL.md")).unwrap_or_default();
    let fm = frontmatter(&text).unwrap_or_default();
    let meta = match get(&fm, "metadata") {
        Some(V::M(m)) => m.clone(),
        _ => BTreeMap::new(),
    };
    let num = Regex::new(r"^[0-9]+\.[0-9]+\.[0-9]+$").unwrap();
    for k in ["version", "hydo-min"] {
        match meta.get(k) {
            Some(v) if num.is_match(v) => {}
            v => r.bad.push(format!(
                "{name}: metadata.{k} must be a version like 1.0.0 (got {v:?})"
            )),
        }
    }
    for c in [
        "skills",
        "commands",
        "agents",
        "hooks",
        "bin",
        ".claude-plugin",
        ".mcp.json",
    ] {
        if d.join(c).exists() {
            r.bad.push(format!(
                "{name}: {c} in the skill folder would be loaded by Claude Code as a plugin component"
            ));
        }
    }
    // The marketplace is `.claude-plugin/marketplace.json` (live since hydo 0.2.0; docs/RELEASING.md,
    // "Releasing the onboarding skill").
    let mp = root.join(".claude-plugin/marketplace.json");
    let Ok(m) = std::fs::read(&mp)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).map_err(|e| e.to_string()))
    else {
        r.bad
            .push(".claude-plugin/marketplace.json is missing or not JSON".into());
        return;
    };
    let src = format!("./skills/{name}");
    let entry = m["plugins"].as_array().and_then(|a| {
        a.iter()
            .find(|p| p["source"].as_str() == Some(src.as_str()))
    });
    match entry {
        None => r.bad.push(format!(
            ".claude-plugin/marketplace.json has no plugin with source {src}"
        )),
        Some(e) => {
            if e["version"].as_str() != meta.get("version").map(String::as_str) {
                r.bad.push(format!(
                    ".claude-plugin/marketplace.json gives {name} version {}, its SKILL.md {:?}",
                    e["version"],
                    meta.get("version")
                ));
            }
            r.info.push(format!(
                "plugin: {}@{} from {src}, version {}",
                e["name"].as_str().unwrap_or("?"),
                m["name"].as_str().unwrap_or("?"),
                e["version"]
            ));
        }
    }
}

fn check(root: &Path, exe: Option<&Path>) -> Report {
    let mut r = Report::default();
    let here = root.join("skills");
    let skills: Vec<PathBuf> = names(&here)
        .into_values()
        .filter(|p| p.join("SKILL.md").is_file())
        .collect();
    for d in &skills {
        match exe {
            Some(x) => skills_ref(d, x, &mut r),
            None => r.info.push(
                "skills-ref: not found (install it from https://github.com/agentskills/agentskills/tree/main/skills-ref, or set HYDO_SKILLS_REF); the rules below are checked anyway".into(),
            ),
        }
        check_skill(d, &mut r);
        check_plugin(root, d, &mut r);
        let n = d.file_name().unwrap().to_string_lossy().into_owned();
        guide_mentions(root, d, &format!("skills/{n}"), &mut r);
        for tool in [".claude/skills", ".agents/skills"] {
            let p = root.join(tool).join(&n);
            let link = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
            if !(link && p.canonicalize().ok() == d.canonicalize().ok()) {
                r.bad
                    .push(format!("{tool}/{n} is not a symlink to skills/{n}"));
            }
        }
    }
    guide_mentions(root, &root.join("hydo/guide"), "hydo/guide", &mut r);
    let ex = root.join("hydo/guide/examples/example-redis-composed/hydo");
    let src = root.join("hydo/onboard/examples/redis-composed/hydo");
    let d = same_tree(&src, &ex);
    if d.is_empty() {
        r.info.push(
            "sync: hydo/guide/examples/example-redis-composed/hydo equals hydo/onboard/examples/redis-composed/hydo".into(),
        );
    } else {
        r.bad.push(format!(
            "hydo/guide/examples/example-redis-composed/hydo differs from hydo/onboard/examples/redis-composed/hydo: {} (copy it again: rm -r {} && cp -Rp {} {})",
            d.join(", "),
            ex.display(),
            src.display(),
            ex.parent().unwrap().display()
        ));
    }
    r
}

#[test]
fn the_skills_hold_the_specification_and_their_copies() {
    let exe = skills_ref_exe();
    let r = check(&top(), exe.as_deref());
    for l in &r.info {
        println!("{l}");
    }
    for b in &r.bad {
        println!("FAIL {b}");
    }
    assert!(
        r.bad.is_empty(),
        "skill check: {} problems: {:?}",
        r.bad.len(),
        r.bad
    );
}

// ---------------------------------------------------------------- the check fails when it should

/// A scratch copy of what the check reads: skills/, the guide, the marketplace file, the two
/// tool symlinks (relative, so they point into the copy) and the example the guide copies.
fn copy_tree() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "hydo-skill-check-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    let t = top();
    std::fs::create_dir_all(d.join("hydo/onboard/examples/redis-composed")).unwrap();
    for (from, to) in [
        ("skills", "."),
        ("hydo/guide", "hydo"),
        (
            "hydo/onboard/examples/redis-composed/hydo",
            "hydo/onboard/examples/redis-composed",
        ),
    ] {
        let ok = std::process::Command::new("cp")
            .arg("-Rp")
            .arg(t.join(from))
            .arg(d.join(to))
            .status()
            .unwrap()
            .success();
        assert!(ok, "cp {from}");
    }
    // The copy has the marketplace at its live place, as the tree does.
    let mp = t.join(".claude-plugin/marketplace.json");
    assert!(mp.exists(), "the marketplace file");
    std::fs::create_dir_all(d.join(".claude-plugin")).unwrap();
    std::fs::copy(mp, d.join(".claude-plugin/marketplace.json")).unwrap();
    for tool in [".claude/skills", ".agents/skills"] {
        std::fs::create_dir_all(d.join(tool)).unwrap();
        std::os::unix::fs::symlink(
            "../../skills/hydo-onboard",
            d.join(tool).join("hydo-onboard"),
        )
        .unwrap();
    }
    d
}

fn bad_after(change: impl FnOnce(&Path)) -> Vec<String> {
    let d = copy_tree();
    assert!(check(&d, None).bad.is_empty(), "the unchanged copy passes");
    change(&d);
    let bad = check(&d, None).bad;
    let _ = std::fs::remove_dir_all(&d);
    bad
}

fn edit_skill(d: &Path, f: impl FnOnce(String) -> String) {
    let p = d.join("skills/hydo-onboard/SKILL.md");
    let t = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, f(t)).unwrap();
}

const EXAMPLE: &str = "hydo/guide/examples/example-redis-composed/hydo";

#[test]
fn a_broken_link_fails() {
    let bad = bad_after(|d| edit_skill(d, |t| t + "\nSee [gone](references/no-such-file.md).\n"));
    assert_eq!(
        bad,
        ["hydo-onboard: SKILL.md links references/no-such-file.md, which does not exist"]
    );
}

#[test]
fn a_link_two_levels_deep_fails() {
    let bad = bad_after(|d| {
        std::fs::create_dir_all(d.join("skills/hydo-onboard/references/deep")).unwrap();
        std::fs::write(d.join("skills/hydo-onboard/references/deep/x.md"), "x\n").unwrap();
        edit_skill(d, |t| t + "\nSee [deep](references/deep/x.md).\n")
    });
    assert_eq!(
        bad,
        ["hydo-onboard: SKILL.md links references/deep/x.md, deeper than one level"]
    );
}

#[test]
fn a_changed_example_byte_fails() {
    let bad = bad_after(|d| {
        let p = d.join(EXAMPLE).join("hydo.yaml");
        let mut b = std::fs::read(&p).unwrap();
        b[0] ^= 0x20;
        std::fs::write(&p, b).unwrap();
    });
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(
        bad[0].starts_with(&format!(
        "{EXAMPLE} differs from hydo/onboard/examples/redis-composed/hydo: hydo.yaml (copy it again"
    )),
        "{bad:?}"
    );
}

#[test]
fn a_lost_executable_bit_fails() {
    let bad = bad_after(|d| {
        let root = d.join(EXAMPLE);
        // the first executable file of the copy
        let mut stack = vec![root.clone()];
        while let Some(x) = stack.pop() {
            for e in std::fs::read_dir(&x).unwrap().flatten() {
                let p = e.path();
                let m = std::fs::metadata(&p).unwrap();
                if m.is_dir() {
                    stack.push(p);
                } else if m.permissions().mode() & 0o100 != 0 {
                    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
                    return;
                }
            }
        }
        panic!("no executable file in {}", root.display());
    });
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains(" (executable bit)"), "{bad:?}");
}

#[test]
fn a_frontmatter_key_outside_the_specification_fails() {
    let bad = bad_after(|d| edit_skill(d, |t| t.replacen("---\n", "---\nversion: 2\n", 1)));
    assert_eq!(
        bad,
        ["hydo-onboard: frontmatter key 'version' not in the specification"]
    );
}

#[test]
fn a_wrong_name_and_a_missing_symlink_fail() {
    let bad = bad_after(|d| {
        edit_skill(d, |t| {
            t.replacen("name: hydo-onboard", "name: Hydo_Onboard", 1)
        });
        std::fs::remove_file(d.join(".agents/skills/hydo-onboard")).unwrap();
    });
    assert_eq!(
        bad,
        [
            "hydo-onboard: name 'Hydo_Onboard' breaks the naming rules",
            "hydo-onboard: name 'Hydo_Onboard' is not the directory name",
            ".agents/skills/hydo-onboard is not a symlink to skills/hydo-onboard",
        ]
    );
}

#[test]
fn a_skill_version_the_plugin_does_not_have_fails() {
    let bad = bad_after(|d| {
        edit_skill(d, |t| {
            t.replacen("  version: \"1.0.0\"", "  version: \"1.0.1\"", 1)
        })
    });
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(
        bad[0].starts_with(".claude-plugin/marketplace.json gives hydo-onboard version \"1.0.0\""),
        "{bad:?}"
    );
    let bad = bad_after(|d| edit_skill(d, |t| t.replacen("  hydo-min: \"0.1.0\"\n", "", 1)));
    assert_eq!(
        bad,
        ["hydo-onboard: metadata.hydo-min must be a version like 1.0.0 (got None)"]
    );
}

#[test]
fn a_plugin_component_in_the_skill_folder_fails() {
    let bad = bad_after(|d| {
        std::fs::create_dir_all(d.join("skills/hydo-onboard/hooks")).unwrap();
    });
    assert_eq!(
        bad,
        ["hydo-onboard: hooks in the skill folder would be loaded by Claude Code as a plugin component"]
    );
}

#[test]
fn a_guide_topic_that_does_not_exist_fails() {
    let bad = bad_after(|d| edit_skill(d, |t| t + "\nRead `hydo guide no-such-topic`.\n"));
    assert_eq!(
        bad,
        ["skills/hydo-onboard/SKILL.md: `hydo guide no-such-topic` is not a topic of hydo/guide/"]
    );
    let bad = bad_after(|d| std::fs::remove_file(d.join("hydo/guide/services.md")).unwrap());
    assert!(
        !bad.is_empty() && bad.iter().all(|b| b.contains("`hydo guide services`")),
        "{bad:?}"
    );
}
