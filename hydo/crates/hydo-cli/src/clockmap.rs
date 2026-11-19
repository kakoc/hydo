//! Which code of a source tree the services of hydo.yaml run, for the clock audit (Milestone
//! 324, `hydo rules scan --kind clock`). A user's audit ranked places in a program that no
//! service of the manifest runs, and kept `<service>` in its schedules although hydo.yaml
//! existed.
//!
//! The audit reads text, so the map is made from the build files of two languages:
//!
//! - Rust: every Cargo package of the tree (`Cargo.toml` with `[package]`), its binaries
//!   (`[[bin]]`, `src/main.rs`, `src/bin/`), and the packages of the tree it depends on (by
//!   `path =` or by name; dev-dependencies are left out). A file belongs to the package of the
//!   nearest `Cargo.toml` above it.
//! - Go: every module (`go.mod`), its main packages (`package main`, a program named after its
//!   folder) and the packages of the module they import, followed through every import. A file
//!   belongs to its folder's package.
//!
//! A service runs a program when a command of its `entrypoint` and `cmd` (as Docker joins them,
//! with the image's from `hydo-compose.lock.json` beside the manifest), its `setup` or its `ready`
//! starts it, or, failing those, when its image is named after it (`server` of
//! `temporalio/server`). The workload runs a program that a command of `workload.run` or of a
//! file of the manifest's `workload/` folder starts. Code of other languages, and code that no
//! such program reaches, counts as run by none of them.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

/// A program that the tree builds.
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub name: String,
    /// `Rust` or `Go`.
    pub lang: &'static str,
    /// The folder it is built from (the Cargo package's, or the main package's).
    pub root: String,
    /// The folders of the code it is built from: Cargo packages or Go packages.
    pub dirs: BTreeSet<String>,
}

/// A program that the manifest runs, by whom, and how the audit knows.
#[derive(Clone, Debug, Default)]
pub struct Used {
    pub program: usize,
    /// The services that run it, and `the workload`.
    pub by: Vec<String>,
    /// How the audit found it: `its cmd`, `the image's entrypoint`, `the image's name`.
    pub how: String,
}

/// The map of the code that the manifest's services run.
#[derive(Clone, Debug, Default)]
pub struct Mapping {
    pub programs: Vec<Program>,
    pub used: Vec<Used>,
    /// The programs that services run and the tree does not build: (who, program).
    pub elsewhere: Vec<(String, String)>,
    /// The Cargo packages' folders, longest first, and the Go packages' folders.
    crates: Vec<String>,
    go_dirs: HashSet<String>,
}

/// The name of the workload in the map.
pub const WORKLOAD: &str = "the workload";

impl Mapping {
    /// Whether a program that the manifest runs is built from the tree.
    pub fn active(&self) -> bool {
        self.used
            .iter()
            .any(|u| !self.programs[u.program].dirs.is_empty())
    }

    /// The folder whose code a file is: its Cargo package, or its Go package.
    fn owner(&self, file: &str) -> Option<String> {
        let dir = parent(file);
        if file.ends_with(".go") || self.go_dirs.contains(&dir) {
            return Some(format!("go:{dir}"));
        }
        self.crates
            .iter()
            .find(|c| c.is_empty() || dir == **c || dir.starts_with(&format!("{c}/")))
            .map(|c| format!("rs:{c}"))
            .or_else(|| self.go_dirs.contains(&dir).then(|| format!("go:{dir}")))
    }

    /// Who runs the code of `file`, the services first: None when the map is not active, an
    /// empty list when none of them does.
    pub fn run_by(&self, file: &str) -> Option<Vec<String>> {
        if !self.active() {
            return None;
        }
        let Some(o) = self.owner(file) else {
            return Some(Vec::new());
        };
        let mut who: Vec<String> = Vec::new();
        for u in &self.used {
            if self.programs[u.program].dirs.contains(&o) {
                for b in &u.by {
                    if !who.contains(b) {
                        who.push(b.clone());
                    }
                }
            }
        }
        who.sort_by_key(|w| w == WORKLOAD);
        Some(who)
    }

    /// The programs that run `file`'s code.
    pub fn programs_of(&self, file: &str) -> Vec<String> {
        let Some(o) = self.owner(file) else {
            return Vec::new();
        };
        let mut v: Vec<String> = Vec::new();
        for u in &self.used {
            let p = &self.programs[u.program];
            if p.dirs.contains(&o) && !v.contains(&p.name) {
                v.push(p.name.clone());
            }
        }
        v
    }

    /// How the audit read hydo.yaml, in sentences, for the report's head.
    pub fn words(&self) -> String {
        let and = |v: &[String]| crate::clockscan::and_words(v);
        let mut parts: Vec<String> = Vec::new();
        for u in &self.used {
            let p = &self.programs[u.program];
            let other = p.dirs.len().saturating_sub(1);
            let what = if p.lang == "Rust" {
                "package"
            } else {
                "Go package"
            };
            parts.push(format!(
                "{} run{} {} ({}), built from {}{}",
                and(&u.by),
                if u.by.len() == 1 { "s" } else { "" },
                p.name,
                u.how,
                if p.root.is_empty() { "." } else { &p.root },
                match other {
                    0 => String::new(),
                    1 => format!(" and 1 {what} it uses"),
                    n => format!(" and {n} {what}s it uses"),
                }
            ));
        }
        let mut by_program: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (who, prog) in &self.elsewhere {
            by_program
                .entry(prog.clone())
                .or_default()
                .push(who.clone());
        }
        for (prog, who) in by_program {
            parts.push(format!(
                "{} run{} {prog}, which these sources do not build",
                and(&who),
                if who.len() == 1 { "s" } else { "" }
            ));
        }
        let built: Vec<String> = {
            let mut v: Vec<String> = self.programs.iter().map(|p| p.name.clone()).collect();
            v.sort();
            v.dedup();
            v
        };
        if !self.active() {
            let what = if built.is_empty() {
                "these sources build no Cargo binary and no Go main package, the programs the \
                 audit maps"
                    .to_string()
            } else {
                format!(
                    "these sources build {} program{} ({}{})",
                    built.len(),
                    if built.len() == 1 { "" } else { "s" },
                    built.iter().take(6).cloned().collect::<Vec<_>>().join(", "),
                    if built.len() > 6 { ", ..." } else { "" }
                )
            };
            parts.push(format!(
                "no service of hydo.yaml runs a program built from these sources ({what}), so the \
                 places are ranked by their words alone"
            ));
        } else {
            parts.push(
                "the places in the code that hydo.yaml runs come first, and the other places after \
                 them"
                    .into(),
            );
        }
        let mut s = parts.join("; ");
        if let Some(c) = s.get(..1) {
            s = format!("{}{}", c.to_uppercase(), &s[1..]);
        }
        s + "."
    }
}

fn parent(file: &str) -> String {
    file.rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default()
}

fn norm(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

/// The quoted value after `key =` on a TOML line (`name = "x"`), or inside an inline table.
fn toml_str(line: &str, key: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = line[from..].find(key) {
        let at = from + i;
        let before = line[..at].chars().next_back();
        let rest = line[at + key.len()..].trim_start();
        if before.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            && rest.starts_with('=')
        {
            let v = rest[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                return v[1..].split(q).next().map(String::from);
            }
        }
        from = at + key.len();
    }
    None
}

/// What one `Cargo.toml` says: its package, its binaries and its dependencies.
#[derive(Default)]
struct Cargo {
    name: Option<String>,
    bins: Vec<String>,
    /// (the dependency's package name, its path relative to the manifest's folder)
    deps: Vec<(String, Option<String>)>,
}

fn read_cargo(text: &str) -> Cargo {
    let mut c = Cargo::default();
    let mut section = String::new();
    let mut table_dep: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.split(" #").next().unwrap_or(raw).trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = line
                .trim_matches(|c| c == '[' || c == ']')
                .trim()
                .to_string();
            table_dep = None;
            // [dependencies.foo], [target.'cfg(...)'.dependencies.foo]
            for kind in ["dependencies.", "build-dependencies."] {
                if let Some(i) = section.find(kind) {
                    if section.starts_with(kind) || section.starts_with("target.") {
                        let name = section[i + kind.len()..].trim_matches('"').to_string();
                        c.deps.push((name, None));
                        table_dep = Some(c.deps.len() - 1);
                    }
                }
            }
            if section == "[bin]" || section == "bin" {
                c.bins.push(String::new());
            }
            continue;
        }
        let is_deps = section == "dependencies"
            || section == "build-dependencies"
            || (section.starts_with("target.")
                && (section.ends_with(".dependencies")
                    || section.ends_with(".build-dependencies")));
        if section == "package" {
            if let Some(n) = toml_str(line, "name") {
                if c.name.is_none() {
                    c.name = Some(n);
                }
            }
        } else if section == "bin" {
            if let (Some(n), Some(last)) = (toml_str(line, "name"), c.bins.last_mut()) {
                *last = n;
            }
        } else if is_deps {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().trim_matches('"');
            let key = key.strip_suffix(".workspace").unwrap_or(key);
            let key = key.split('.').next().unwrap_or(key).to_string();
            let name = toml_str(value, "package").unwrap_or(key);
            c.deps.push((name, toml_str(value, "path")));
        } else if let Some(d) = table_dep {
            if let Some(p) = toml_str(line, "path") {
                c.deps[d].1 = Some(p);
            }
            if let Some(n) = toml_str(line, "package") {
                c.deps[d].0 = n;
            }
        }
    }
    c.bins.retain(|b| !b.is_empty());
    c
}

/// The Go import paths of a file.
fn go_imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut block = false;
    for raw in text.lines() {
        let l = raw.trim();
        if block {
            if l.starts_with(')') {
                block = false;
                continue;
            }
            if let Some(p) = l.split('"').nth(1) {
                out.push(p.to_string());
            }
            continue;
        }
        if let Some(rest) = l.strip_prefix("import") {
            let rest = rest.trim_start();
            if rest.starts_with('(') {
                block = true;
            } else if let Some(p) = rest.split('"').nth(1) {
                out.push(p.to_string());
            }
        } else if l.starts_with("func ") || l.starts_with("type ") || l.starts_with("var ") {
            // the imports come before the declarations
            break;
        }
    }
    out
}

/// Every program that the tree under `root` builds, with the folders of its code.
fn programs(root: &Path) -> (Vec<Program>, Vec<String>, HashSet<String>) {
    let mut paths = Vec::new();
    crate::rules::walk(root, &mut paths);
    let rel = |p: &Path| -> String {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let files: Vec<String> = paths.iter().map(|p| rel(p)).collect();
    let mut out: Vec<Program> = Vec::new();
    // Rust: the Cargo packages by folder and by name
    let mut pkgs: BTreeMap<String, Cargo> = BTreeMap::new();
    for f in files
        .iter()
        .filter(|f| f.rsplit('/').next() == Some("Cargo.toml"))
    {
        if let Ok(t) = std::fs::read_to_string(root.join(f)) {
            let c = read_cargo(&t);
            if c.name.is_some() {
                pkgs.insert(parent(f), c);
            }
        }
    }
    let by_name: HashMap<String, String> = pkgs
        .iter()
        .filter_map(|(d, c)| c.name.clone().map(|n| (n, d.clone())))
        .collect();
    let closure = |start: &str| -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut todo = vec![start.to_string()];
        while let Some(d) = todo.pop() {
            if !seen.insert(d.clone()) {
                continue;
            }
            if let Some(c) = pkgs.get(&d) {
                for (name, path) in &c.deps {
                    let target = match path {
                        Some(p) => Some(norm(&format!("{d}/{p}"))),
                        None => by_name.get(name).cloned(),
                    };
                    if let Some(t) = target.filter(|t| pkgs.contains_key(t)) {
                        todo.push(t);
                    }
                }
            }
        }
        seen.into_iter().map(|d| format!("rs:{d}")).collect()
    };
    for (d, c) in &pkgs {
        let mut bins = c.bins.clone();
        let pre = if d.is_empty() {
            String::new()
        } else {
            format!("{d}/")
        };
        if files.contains(&format!("{pre}src/main.rs")) {
            bins.push(c.name.clone().unwrap_or_default());
        }
        for f in files
            .iter()
            .filter_map(|f| f.strip_prefix(&format!("{pre}src/bin/")))
        {
            let name = match f.split_once('/') {
                Some((dir, "main.rs")) => dir.to_string(),
                Some(_) => continue,
                None => match f.strip_suffix(".rs") {
                    Some(s) => s.to_string(),
                    None => continue,
                },
            };
            bins.push(name);
        }
        bins.sort();
        bins.dedup();
        if bins.is_empty() {
            continue;
        }
        let dirs = closure(d);
        for b in bins {
            out.push(Program {
                name: b,
                lang: "Rust",
                root: d.clone(),
                dirs: dirs.clone(),
            });
        }
    }
    let mut crates: Vec<String> = pkgs.keys().cloned().collect();
    crates.sort_by_key(|c| std::cmp::Reverse(c.len()));
    // Go: the modules, the packages' imports, and the main packages
    let mut modules: Vec<(String, String)> = Vec::new(); // (module path, folder)
    for f in files
        .iter()
        .filter(|f| f.rsplit('/').next() == Some("go.mod"))
    {
        if let Ok(t) = std::fs::read_to_string(root.join(f)) {
            if let Some(m) = t
                .lines()
                .find_map(|l| l.trim().strip_prefix("module "))
                .map(|m| m.trim().trim_matches('"').to_string())
            {
                modules.push((m, parent(f)));
            }
        }
    }
    modules.sort_by_key(|(m, _)| std::cmp::Reverse(m.len()));
    let mut imports: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut mains: BTreeSet<String> = BTreeSet::new();
    let mut go_dirs: HashSet<String> = HashSet::new();
    if !modules.is_empty() {
        for f in files
            .iter()
            .filter(|f| f.ends_with(".go") && !f.ends_with("_test.go"))
        {
            let Ok(t) = std::fs::read_to_string(root.join(f)) else {
                continue;
            };
            let d = parent(f);
            go_dirs.insert(d.clone());
            let main = t.lines().any(|l| l.trim() == "package main");
            if main {
                mains.insert(d.clone());
            }
            let e = imports.entry(d).or_default();
            for imp in go_imports(&t) {
                if let Some((m, md)) = modules
                    .iter()
                    .find(|(m, _)| imp == *m || imp.starts_with(&format!("{m}/")))
                {
                    let sub = imp[m.len()..].trim_start_matches('/');
                    e.insert(norm(&format!("{md}/{sub}")));
                }
            }
        }
    }
    for m in &mains {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut todo = vec![m.clone()];
        while let Some(d) = todo.pop() {
            if !seen.insert(d.clone()) {
                continue;
            }
            for i in imports.get(&d).into_iter().flatten() {
                todo.push(i.clone());
            }
        }
        let name = match m.rsplit('/').next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => modules
                .iter()
                .find(|(_, md)| md.is_empty())
                .and_then(|(p, _)| p.rsplit('/').next().map(String::from))
                .unwrap_or_default(),
        };
        out.push(Program {
            name,
            lang: "Go",
            root: m.clone(),
            dirs: seen.into_iter().map(|d| format!("go:{d}")).collect(),
        });
    }
    (out, crates, go_dirs)
}

/// The string leaves of a value.
fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// The words of a command line that name a program: the first word of each command, past
/// variable assignments and wrappers (`exec`, `timeout 60`, `chpst -u user`, `hydo-oci <svc>`,
/// busybox, init programs), each as its file's name.
fn command_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for seg in text.split(['\n', ';', '|', '&', '(', '`']) {
        let mut ws = seg.split_whitespace().peekable();
        while let Some(w) = ws.next() {
            let w = w.trim_matches(|c| matches!(c, '"' | '\'' | '{' | '}' | '[' | ']' | ','));
            if w.is_empty() || (w.contains('=') && !w.starts_with('-')) {
                continue;
            }
            let b = w.rsplit('/').next().unwrap_or(w);
            match b {
                "exec" | "env" | "nice" | "nohup" | "sudo" | "tini" | "dumb-init" | "gosu"
                | "su-exec" | "then" | "do" | "else" | "if" | "while" | "!" | "time" | "--" => {
                    continue
                }
                "timeout" => {
                    while ws.peek().is_some_and(|n| {
                        n.starts_with('-') || n.starts_with(|c: char| c.is_ascii_digit())
                    }) {
                        ws.next();
                    }
                    continue;
                }
                "chpst" => {
                    while ws.peek().is_some_and(|n| n.starts_with('-')) {
                        ws.next();
                        ws.next();
                    }
                    continue;
                }
                "hydo-oci" => {
                    ws.next();
                    continue;
                }
                _ if b.contains("busybox") || b.ends_with(".sh") => continue,
                _ => {}
            }
            if !w.starts_with('-') && !b.is_empty() {
                out.push(b.to_string());
            }
            break;
        }
    }
    out
}

/// A value as one command line: a list's words joined.
fn line_of(v: Option<&Value>) -> Option<String> {
    let v = v?;
    let mut s = Vec::new();
    strings(v, &mut s);
    (!s.is_empty()).then(|| s.join(" "))
}

/// The image's entrypoint and command, by image reference, from the lock file of `hydo
/// compose` beside the manifest.
fn lock_commands(mdir: &Path) -> HashMap<String, (Option<String>, Option<String>)> {
    let mut out = HashMap::new();
    let Ok(t) = std::fs::read_to_string(mdir.join("hydo-compose.lock.json")) else {
        return out;
    };
    let Ok(v) = serde_json::from_str::<Value>(&t) else {
        return out;
    };
    for (r, e) in v
        .get("images")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let c = &e["platforms"]["amd64"]["config"];
        let c = if c.is_object() {
            c
        } else {
            &e["platforms"]["arm64"]["config"]
        };
        out.insert(
            r.clone(),
            (line_of(c.get("Entrypoint")), line_of(c.get("Cmd"))),
        );
    }
    out
}

/// One runner of programs: a service or the workload.
struct Runner {
    who: String,
    /// The programs its commands name, and whether its own command line names them (else the
    /// image's entrypoint does).
    words: Vec<(String, bool)>,
    /// The program its command line starts, for the report when the tree does not build it.
    main: Option<String>,
    /// The image's name (`tikv` of `pingcap/tikv:v8.5.8`).
    image: String,
}

/// The map of the code that the services and the workload of the manifest at `manifest`
/// (its text `text`) run, in the tree under `root`.
pub fn map(root: &Path, manifest: &Path, text: &str) -> Mapping {
    let (progs, crates, go_dirs) = programs(root);
    let mut m = Mapping {
        programs: progs,
        crates,
        go_dirs,
        ..Mapping::default()
    };
    let Ok(v) = hydo_manifest::yaml::parse_yaml(text, &manifest.to_string_lossy()) else {
        return m;
    };
    let mdir = manifest.parent().unwrap_or(Path::new("."));
    let locked = lock_commands(mdir);
    let mut runners: Vec<Runner> = Vec::new();
    for s in v
        .get("services")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(n) = s.get("name").and_then(Value::as_str) else {
            continue;
        };
        let image_ref = s.get("image_ref").and_then(Value::as_str).unwrap_or("");
        let (ie, ic) = locked.get(image_ref).cloned().unwrap_or_default();
        // as Docker runs it: the entrypoint, then the command; the manifest's replace the image's
        let own_entry = line_of(s.get("entrypoint"));
        let own_cmd = line_of(s.get("cmd"));
        let entry = own_entry
            .clone()
            .or(if s.get("image").is_some() { ie } else { None });
        let cmd = own_cmd.clone().or(ic);
        let full = [entry.clone(), cmd.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let own = [own_entry, own_cmd]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let own_words = command_words(&own);
        let mut words: Vec<(String, bool)> = Vec::new();
        for w in command_words(&full) {
            let mine = own_words.contains(&w);
            words.push((w, mine));
        }
        for k in ["setup", "ready"] {
            for w in line_of(s.get(k))
                .map(|l| command_words(&l))
                .unwrap_or_default()
            {
                words.push((w, true));
            }
        }
        let repo = image_ref.split('@').next().unwrap_or("");
        let repo = repo.rsplit('/').next().unwrap_or("");
        runners.push(Runner {
            who: n.to_string(),
            main: command_words(&full).into_iter().next(),
            words,
            image: repo.split(':').next().unwrap_or("").to_string(),
        });
    }
    let mut wl = Vec::new();
    if let Some(r) = v.get("workload").and_then(|w| w.get("run")) {
        strings(r, &mut wl);
    }
    if let Ok(rd) = std::fs::read_dir(mdir.join("workload")) {
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files {
            if let Ok(t) = std::fs::read_to_string(f) {
                wl.push(t.chars().take(262_144).collect());
            }
        }
    }
    runners.push(Runner {
        who: WORKLOAD.into(),
        words: wl
            .iter()
            .flat_map(|t| command_words(t))
            .map(|w| (w, true))
            .collect(),
        main: None,
        image: String::new(),
    });
    let mut used: Vec<Used> = Vec::new();
    let add = |used: &mut Vec<Used>, p: usize, who: &str, how: &str| match used
        .iter_mut()
        .find(|u| u.program == p)
    {
        Some(u) => {
            if !u.by.iter().any(|b| b == who) {
                u.by.push(who.to_string());
            }
        }
        None => used.push(Used {
            program: p,
            by: vec![who.to_string()],
            how: how.to_string(),
        }),
    };
    let mut elsewhere: Vec<(String, String)> = Vec::new();
    for r in &runners {
        let mut found = false;
        for (i, p) in m.programs.iter().enumerate() {
            if let Some((_, mine)) = r.words.iter().find(|(w, _)| *w == p.name) {
                let how = match (*mine, r.who == WORKLOAD) {
                    (_, true) => "its commands",
                    (true, false) => "its command",
                    (false, false) => "the image's entrypoint",
                };
                add(&mut used, i, &r.who, how);
                found = true;
            }
        }
        if !found && !r.image.is_empty() {
            for (i, p) in m.programs.iter().enumerate() {
                if p.name == r.image {
                    add(&mut used, i, &r.who, "the image's name");
                    found = true;
                }
            }
        }
        if !found {
            if let Some(f) = r.main.clone().filter(|f| f.len() < 40) {
                elsewhere.push((r.who.clone(), f));
            }
        }
    }
    m.used = used;
    m.elsewhere = elsewhere;
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hydo-clockmap-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (rel, text) in files {
            let p = d.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        d
    }

    fn map_of(d: &Path, manifest: &str) -> Mapping {
        let mpath = d.join(manifest);
        let text = std::fs::read_to_string(&mpath).unwrap();
        map(d, &mpath, &text)
    }

    #[test]
    fn a_cargo_workspace_maps_each_binary_to_its_packages() {
        let d = tree(
            "cargo",
            &[
                ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nstore = { path = \"crates/store\" }\n"),
                ("crates/scheduler/Cargo.toml", "[package]\nname = \"scheduler\"\n\n[dependencies]\nstore.workspace = true\nserde = \"1\"\n\n[dev-dependencies]\ntestkit = { path = \"../testkit\" }\n"),
                ("crates/scheduler/src/main.rs", "fn main() {}\n"),
                ("crates/store/Cargo.toml", "[package]\nname = \"store\"\n"),
                ("crates/store/src/lib.rs", "pub fn lease() {}\n"),
                ("crates/migrate/Cargo.toml", "[package]\nname = \"migrate\"\n\n[[bin]]\nname = \"migrate-tool\"\npath = \"src/run.rs\"\n"),
                ("crates/migrate/src/run.rs", "fn main() {}\n"),
                ("crates/testkit/Cargo.toml", "[package]\nname = \"testkit\"\n"),
                ("crates/testkit/src/lib.rs", "pub fn t() {}\n"),
                ("hydo/hydo.yaml", "format: 1\nname: p\nservices:\n  - name: scheduler\n    cmd: \"/bin/scheduler --port 7000\"\n  - name: executor_1\n    cmd: [/bin/scheduler, executor]\n  - name: db\n    image_ref: postgres:16\nworkload:\n  steps: 4\n  run: \"true\"\n"),
            ],
        );
        let m = map_of(&d, "hydo/hydo.yaml");
        assert!(m.active());
        assert_eq!(
            m.run_by("crates/store/src/lib.rs"),
            Some(vec!["scheduler".to_string(), "executor_1".to_string()])
        );
        assert_eq!(m.run_by("crates/migrate/src/run.rs"), Some(vec![]));
        assert_eq!(m.run_by("crates/testkit/src/lib.rs"), Some(vec![]));
        assert_eq!(m.programs_of("crates/store/src/lib.rs"), vec!["scheduler"]);
        let w = m.words();
        assert!(
            w.starts_with(
                "Scheduler and executor_1 run scheduler (its command), built from \
                 crates/scheduler and 1 package it uses"
            ),
            "{w}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_go_main_package_maps_its_imports() {
        let d = tree(
            "go",
            &[
                ("go.mod", "module example.com/kv\n\ngo 1.22\n"),
                ("cmd/server/main.go", "package main\n\nimport (\n\t\"fmt\"\n\tkv \"example.com/kv/store\"\n)\n\nfunc main() {}\n"),
                ("cmd/tool/main.go", "package main\n\nimport \"example.com/kv/admin\"\n\nfunc main() {}\n"),
                ("store/lease.go", "package store\n\nimport \"example.com/kv/clock\"\n"),
                ("clock/clock.go", "package clock\n"),
                ("admin/admin.go", "package admin\n"),
                ("hydo.yaml", "format: 1\nname: kv\nservices:\n  - name: kv\n    image_ref: example/server:1\n"),
            ],
        );
        let m = map_of(&d, "hydo.yaml");
        assert_eq!(m.run_by("clock/clock.go"), Some(vec!["kv".to_string()]));
        assert_eq!(m.run_by("admin/admin.go"), Some(vec![]));
        assert!(
            m.words().contains("Kv runs server (the image's name)"),
            "{}",
            m.words()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn without_a_program_of_the_tree_the_map_is_not_active() {
        let d = tree(
            "c",
            &[
                ("src/server.c", "int main(void) { return 0; }\n"),
                ("hydo.yaml", "format: 1\nname: v\nservices:\n  - name: v1\n    cmd: [valkey-server, --port, \"7000\"]\n"),
            ],
        );
        let m = map_of(&d, "hydo.yaml");
        assert!(!m.active());
        assert_eq!(m.run_by("src/server.c"), None);
        let w = m.words();
        assert!(
            w.contains("V1 runs valkey-server, which these sources do not build"),
            "{w}"
        );
        assert!(w.contains("ranked by their words alone"), "{w}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
