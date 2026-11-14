//! `hydo onboard [DIR] [--agent NAME ...] [--no-skill] [--no-resolve]` (Milestone 228): the one
//! command that starts onboarding a repository. It says what it found (a Compose file and its
//! services, Dockerfiles, the main language by files, an existing hydo/ folder), starts the
//! folder (`hydo compose` when there is a Compose file, else `hydo init`; an existing hydo/ folder
//! is never touched), installs the onboarding skill for the AI tools it finds, and prints the one
//! next step. Without an AI tool the next step is the three parts a person fills in.
//!
//! An AI tool is found by its folder in the repository (`.claude/`, `.codex/`, `.cursor/`,
//! `.gemini/`, `.github/copilot-instructions.md`) or its command on PATH (`claude`, `codex`,
//! `cursor-agent` or `cursor`, `gemini`, `copilot`). `--agent` names the tools instead, and
//! `--no-skill` installs nothing. The skill goes where `hydo skill install --agent <tool>` puts
//! it, relative to DIR's repository: Claude Code gets the plugin folder `.claude/skills/hydo`
//! (the command `/hydo:onboard`), the other tools the skill folder `.agents/skills/onboard`.

use crate::ctx::Ctx;
use hydo_agents::{folder_for, Out, Skill, AGENTS, CLAUDE_COMMAND};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const HELP: &str =
    "usage: hydo onboard [DIR] [--agent claude|codex|cursor|copilot|gemini|all ...] [--no-skill]
                    [--no-resolve]
  starts onboarding the repository at DIR (default: this directory) and prints the next step:
  1. says what it found: a Compose file and its services, Dockerfiles, the main language by
     files, and an existing hydo/ folder;
  2. starts hydo/: with a Compose file, `hydo compose <file> --out hydo/hydo.yaml --images-dir
     images` (the services, ready to fill in); without one, `hydo init`. An existing hydo/ folder
     is never changed;
  3. installs the onboarding skill for the AI tools it finds (a .claude/, .codex/, .cursor/ or
     .gemini/ folder, or the claude, codex, cursor-agent, gemini or copilot command on PATH):
     Claude Code gets .claude/skills/hydo, whose command is /hydo:onboard, the other tools
     .agents/skills/onboard;
  4. prints the next step: open the AI tool and run the skill, or, without one, the three parts
     to fill in.
  --agent NAME: install for these tools instead of the ones found (repeatable, or comma-separated)
  --no-skill: install no skill
  --no-resolve: pass --no-resolve to hydo compose (no registry lookups; the images stay tags)";

/// What `hydo onboard` was asked.
struct Args {
    dir: PathBuf,
    agents: Option<Vec<String>>,
    skill: bool,
    no_resolve: bool,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut dir: Option<PathBuf> = None;
    let (mut agents, mut skill, mut no_resolve) = (None::<Vec<String>>, true, false);
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        let mut add = |v: &str| {
            agents
                .get_or_insert_with(Vec::new)
                .extend(v.split(',').filter(|x| !x.is_empty()).map(str::to_string))
        };
        if a == "--agent" {
            let v = argv.get(i + 1).ok_or(
                "--agent needs a tool name (claude, codex, cursor, copilot, gemini or all)",
            )?;
            add(v);
            i += 1;
        } else if let Some(v) = a.strip_prefix("--agent=") {
            add(v);
        } else if a == "--no-skill" {
            skill = false;
        } else if a == "--no-resolve" {
            no_resolve = true;
        } else if a.starts_with('-') {
            return Err(format!("unknown option {a} (hydo onboard --help)"));
        } else if dir.is_none() {
            dir = Some(PathBuf::from(a));
        } else {
            return Err(format!(
                "one directory only, got {a} too (hydo onboard --help)"
            ));
        }
        i += 1;
    }
    if let Some(v) = &mut agents {
        if v.iter().any(|x| x == "all") {
            *v = AGENTS.iter().map(|(k, _, _)| k.to_string()).collect();
        }
        if let Some(bad) = v.iter().find(|x| !AGENTS.iter().any(|(k, _, _)| k == x)) {
            return Err(format!(
                "unknown agent {bad} (known: {}, all)",
                AGENTS
                    .iter()
                    .map(|(k, _, _)| *k)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        v.dedup();
    }
    let dir = dir.unwrap_or_else(|| PathBuf::from("."));
    let dir = std::path::absolute(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !dir.is_dir() {
        return Err(format!("no such directory: {}", dir.display()));
    }
    Ok(Args {
        dir,
        agents,
        skill,
        no_resolve,
    })
}

const COMPOSE_FILES: [&str; 4] = [
    "docker-compose.yaml",
    "docker-compose.yml",
    "compose.yaml",
    "compose.yml",
];

/// The names under the top-level `services:` key of a Compose file (its first indentation level).
pub fn compose_services(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut indent: Option<usize> = None;
    for line in text.lines() {
        let t = line.trim_end();
        if t.is_empty() || t.trim_start().starts_with('#') {
            continue;
        }
        let lead = t.len() - t.trim_start().len();
        if lead == 0 {
            inside = t.starts_with("services:");
            indent = None;
            continue;
        }
        if !inside {
            continue;
        }
        let ind = *indent.get_or_insert(lead);
        if lead != ind {
            continue;
        }
        let s = t.trim_start();
        if let Some((k, rest)) = s.split_once(':') {
            let k = k.trim().trim_matches(|c| c == '"' || c == '\'');
            if !k.is_empty() && !k.starts_with('-') && (rest.is_empty() || rest.starts_with(' ')) {
                out.push(k.to_string());
            }
        }
    }
    out
}

/// The language of a file name's extension, for the files that name one.
fn language(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1;
    Some(match ext {
        "rs" => "Rust",
        "go" => "Go",
        "py" => "Python",
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "ts" | "tsx" | "mts" => "TypeScript",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" => "Scala",
        "rb" => "Ruby",
        "php" => "PHP",
        "cs" => "C#",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "C++",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "swift" => "Swift",
        "zig" => "Zig",
        "clj" => "Clojure",
        "hs" => "Haskell",
        "ml" => "OCaml",
        "dart" => "Dart",
        _ => return None,
    })
}

/// Folders a look at the repository skips: hidden ones, dependencies and build outputs.
fn skipped_dir(n: &str) -> bool {
    n.starts_with('.')
        || matches!(
            n,
            "node_modules" | "target" | "vendor" | "dist" | "build" | "__pycache__" | "hydo-out"
        )
}

/// What the look at the repository found.
#[derive(Default)]
struct Found {
    files: usize,
    languages: BTreeMap<&'static str, usize>,
    dockerfiles: Vec<String>,
}

/// Walk `dir` (at most 6 levels and 50,000 files): the languages by file count and the
/// Dockerfiles.
fn look(top: &Path) -> Found {
    let mut f = Found::default();
    let mut stack = vec![(top.to_path_buf(), 0usize)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut names: Vec<(String, bool)> = rd
            .filter_map(Result::ok)
            .map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (e.file_name().to_string_lossy().into_owned(), is_dir)
            })
            .collect();
        names.sort();
        for (n, is_dir) in names {
            if is_dir {
                if depth < 6 && !skipped_dir(&n) && !(depth == 0 && n == "hydo") {
                    stack.push((d.join(&n), depth + 1));
                }
                continue;
            }
            f.files += 1;
            if f.files > 50_000 {
                return f;
            }
            if n == "Dockerfile" || n.starts_with("Dockerfile.") || n.ends_with(".Dockerfile") {
                let p = d.join(&n);
                let rel = p.strip_prefix(top).unwrap_or(&p).display().to_string();
                f.dockerfiles.push(rel);
            }
            if let Some(l) = language(&n) {
                *f.languages.entry(l).or_default() += 1;
            }
        }
    }
    f.dockerfiles.sort();
    f
}

/// Whether `cmd` is a program on PATH.
fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|d| {
            let f = d.join(cmd);
            f.is_file()
                && std::fs::metadata(&f).is_ok_and(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                })
        })
    })
}

/// The AI tools found for the repository at `top`, each with what showed it.
fn tools(top: &Path) -> Vec<(&'static str, String)> {
    let mut v = Vec::new();
    let probes: [(&str, &[&str], &[&str]); 5] = [
        ("claude", &[".claude"], &["claude"]),
        ("codex", &[".codex"], &["codex"]),
        ("cursor", &[".cursor"], &["cursor-agent", "cursor"]),
        (
            "copilot",
            &[".github/copilot-instructions.md"],
            &["copilot"],
        ),
        ("gemini", &[".gemini"], &["gemini"]),
    ];
    for (k, dirs, cmds) in probes {
        if let Some(d) = dirs.iter().find(|d| top.join(d).exists()) {
            v.push((k, format!("{d} in the repository")));
        } else if let Some(c) = cmds.iter().find(|c| on_path(c)) {
            v.push((k, format!("the {c} command")));
        }
    }
    v
}

fn name_of(agent: &str) -> &'static str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "cursor" => "Cursor",
        "copilot" => "GitHub Copilot",
        "gemini" => "Gemini CLI",
        _ => "your AI tool",
    }
}

pub fn cmd_onboard(ctx: &Ctx, argv: &[String]) -> i32 {
    let a = match parse(argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hydo: {e}");
            return 2;
        }
    };
    let mut rc = 0;
    let dir = &a.dir;
    let shown = dir.display().to_string();
    println!("hydo: looking at {shown}");

    // 1. what is there
    let compose = COMPOSE_FILES.into_iter().find(|f| dir.join(f).is_file());
    let services = compose
        .map(|c| compose_services(&std::fs::read_to_string(dir.join(c)).unwrap_or_default()))
        .unwrap_or_default();
    match compose {
        Some(c) if services.is_empty() => println!("  Compose file: {c} (no services found in it)"),
        Some(c) => println!(
            "  Compose file: {c}, {} service{}: {}",
            services.len(),
            if services.len() == 1 { "" } else { "s" },
            services.join(", ")
        ),
        None => println!("  Compose file: none"),
    }
    let found = look(dir);
    match found.dockerfiles.len() {
        0 => println!("  Dockerfile: none"),
        n if n <= 5 => println!("  Dockerfile: {}", found.dockerfiles.join(", ")),
        n => println!(
            "  Dockerfile: {} and {} more",
            found.dockerfiles[..5].join(", "),
            n - 5
        ),
    }
    let mut langs: Vec<(&str, usize)> = found.languages.iter().map(|(k, v)| (*k, *v)).collect();
    langs.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(y.0)));
    match langs.first() {
        None => println!("  main language: none found ({} files)", found.files),
        Some((l, n)) => {
            let others: Vec<String> = langs[1..]
                .iter()
                .take(2)
                .map(|(l, n)| format!("{l} {n}"))
                .collect();
            println!(
                "  main language: {l} ({n} file{}{})",
                if *n == 1 { "" } else { "s" },
                if others.is_empty() {
                    String::new()
                } else {
                    format!("; then {}", others.join(", "))
                }
            );
        }
    }
    let had_hydo = dir.join("hydo").exists();
    if had_hydo {
        let m = if dir.join("hydo/hydo.yaml").is_file() {
            "hydo/ with hydo/hydo.yaml"
        } else {
            "hydo/ (without hydo/hydo.yaml)"
        };
        println!("  hydo folder: {m}");
    } else {
        println!("  hydo folder: none yet");
    }

    // 2. start the folder
    let clock = ctx.config.cli.init_clock_start;
    if had_hydo {
        println!(
            "hydo: hydo/ exists, so it is left as it is (hydo onboard never changes it); \
             `hydo build hydo/hydo.yaml --check` checks it"
        );
    } else if let Some(c) = compose {
        let out = dir.join("hydo/hydo.yaml").display().to_string();
        let mut args = vec![
            dir.join(c).display().to_string(),
            "--out".into(),
            out,
            "--images-dir".into(),
            "images".into(),
        ];
        if a.no_resolve {
            args.push("--no-resolve".into());
        }
        println!("hydo: starting hydo/ from {c}: hydo compose {c} --out hydo/hydo.yaml --images-dir images");
        let r = hydo_compose::main_clock(&args, clock);
        if r != 0 {
            rc = 1;
            println!(
                "hydo: hydo compose stopped (above). Fix what it says and run it again in {shown}:\n\
                 hydo:   hydo compose {c} --out hydo/hydo.yaml --images-dir images\n\
                 hydo: (--no-resolve skips the registry when there is no network)"
            );
        } else {
            let mut so = std::io::stdout().lock();
            let mut out = Out::new(&mut so);
            if let Ok(lines) = hydo_agents::update_gitignore(
                &dir.join(".gitignore"),
                &[".hydo/", "hydo/images/", "hydo-out/"],
            ) {
                for l in lines {
                    let _ = out.put(&format!("hydo: added {l} to .gitignore\n"));
                }
            }
            if let Ok(b) = hydo_agents::agents_block() {
                if let Ok(r) = hydo_agents::update_agents_md(&dir.join("AGENTS.md"), &b) {
                    let _ = out.put(&format!("hydo: AGENTS.md: hydo section {r}\n"));
                }
            }
            println!(
                "hydo: before the first build, copy the images in once (network): sh hydo/fetch-images.sh"
            );
        }
    } else {
        println!("hydo: starting hydo/ with hydo init");
        let args = vec!["--dir".to_string(), shown.clone()];
        let r = hydo_agents::run_with("init-quiet", &args, &ctx.root, clock);
        if r != 0 {
            return r;
        }
    }

    // 3. the skill
    let top = hydo_agents::repo_root(dir);
    let chosen: Vec<(String, String)> = match &a.agents {
        Some(v) => v
            .iter()
            .map(|k| (k.clone(), "--agent".to_string()))
            .collect(),
        None => tools(&top)
            .into_iter()
            .map(|(k, why)| (k.to_string(), why))
            .collect(),
    };
    let mut installed: Vec<String> = Vec::new();
    if !a.skill {
        println!("hydo: no skill installed (--no-skill)");
    } else if chosen.is_empty() {
        println!("hydo: no AI coding tool found (no .claude/, .codex/, .cursor/ or .gemini/ folder, and no claude, codex, cursor-agent, gemini or copilot command)");
    } else {
        let sk = Skill::find(&ctx.root);
        let mut done: Vec<PathBuf> = Vec::new();
        for (k, why) in &chosen {
            let Some((_, d, _)) = AGENTS.iter().find(|(x, _, _)| x == k) else {
                continue;
            };
            let t = top.join(d).join(folder_for(d));
            println!("hydo: {} ({why})", name_of(k));
            if !done.contains(&t) {
                let mut so = std::io::stdout().lock();
                let mut out = Out::new(&mut so);
                if let Err(e) = hydo_agents::copy_skill(&sk, &t, false, &mut out) {
                    println!("hydo: the skill was not installed: {e}");
                    continue;
                }
                done.push(t);
            }
            installed.push(k.clone());
        }
        if !done.is_empty() {
            println!("hydo: commit the copied folder so that everyone working in the repository gets the skill");
        }
    }

    // 4. the next step
    println!();
    let here = if *dir == std::env::current_dir().unwrap_or_default() {
        "here".to_string()
    } else {
        format!("in {shown}")
    };
    if let Some(k) = installed.first() {
        if k == "claude" {
            println!("Next: open Claude Code {here} and run {CLAUDE_COMMAND}");
            println!("      (accept the folder's trust prompt: the plugin loads after it). It asks about your");
            println!("      system and fills in hydo/ with you.");
        } else {
            println!(
                "Next: open {} {here} and ask it to use the onboard skill to set up hydo for this project.",
                name_of(k)
            );
            println!("      It asks about your system and fills in hydo/ with you.");
        }
        for k in &installed[1..] {
            if k == "claude" {
                println!("      In Claude Code the command is {CLAUDE_COMMAND}.");
            } else {
                println!("      In {}, ask it to use the onboard skill.", name_of(k));
            }
        }
    } else {
        println!("Next: fill in three things in hydo/ (`hydo guide workflow` walks through them):");
        println!("  1. how to start your system: one service per process in hydo/hydo.yaml (hydo guide services)");
        println!("  2. a client that uses it: commands in hydo/workload/ that record what the system confirmed");
        println!("     (hydo guide composer)");
        println!(
            "  3. the rules that must hold: checks in hydo/checks/, or a `type: messages` check in"
        );
        println!(
            "     hydo/hydo.yaml for a queue or a log (hydo guide checks, hydo guide messages)"
        );
        println!("Check the folder after each change: hydo build hydo/hydo.yaml --check");
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_of_a_compose_file() {
        let t = "version: '3'\nservices:\n  web:\n    image: nginx\n    ports: [\"80:80\"]\n  \"db\":\n    image: postgres\n  # a comment\n  worker:\n    build: .\nvolumes:\n  data:\n";
        assert_eq!(compose_services(t), ["web", "db", "worker"]);
        assert!(compose_services("x: 1\n").is_empty());
    }

    #[test]
    fn languages_by_extension() {
        assert_eq!(language("main.go"), Some("Go"));
        assert_eq!(language("a.tsx"), Some("TypeScript"));
        assert_eq!(language("README.md"), None);
        assert_eq!(language("Makefile"), None);
    }
}
