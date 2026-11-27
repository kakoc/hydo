//! `hydo-explore`: the explorer on its own, with explore.py's command line (the release's entry
//! point is `hydo explore`, which calls the same library).
//!
//! The root (the folder with `hydo/` and `hv/`): `$HYDO_ROOT`, else the first parent of this
//! program that has both, else the source tree it was built from. The configuration is read
//! here, once (`hydo_config`, M144), and passed down.

use std::path::{Path, PathBuf};

fn root(paths: &hydo_config::run::Paths) -> PathBuf {
    if let Some(r) = &paths.root {
        return r.clone();
    }
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .unwrap_or_default();
    let mut d = exe.parent();
    while let Some(p) = d {
        if p.join("hydo").join("explore").is_dir() && p.join("hv").is_dir() {
            return p.to_path_buf();
        }
        d = p.parent();
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let config = hydo_config::Config::process();
    let env = hydo_explore::Env {
        root: root(&config.paths),
        cli: false,
        plain_end: false,
        config,
        memory_hint: None,
        first_report: None,
    };
    // Milestone 387: `hydo-explore shrink <explore dir> [--seed N]... [--errors]` shrinks what the
    // exploration did not shrink (everything without --seed and --errors); `hydo shrink` is its
    // form for people
    if argv.first().map(String::as_str) == Some("shrink") {
        std::process::exit(shrink(&argv[1..], &env));
    }
    std::process::exit(hydo_explore::main_with(&argv, &env));
}

fn shrink(argv: &[String], env: &hydo_explore::Env) -> i32 {
    let mut dir = None;
    let mut seeds: Vec<i64> = Vec::new();
    let mut errors = false;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--errors" => errors = true,
            "--seed" => match it.next().and_then(|v| v.parse().ok()) {
                Some(n) => seeds.push(n),
                None => {
                    eprintln!("hydo-explore shrink: --seed takes a number");
                    return 2;
                }
            },
            d if dir.is_none() => dir = Some(PathBuf::from(d)),
            _ => {
                eprintln!("usage: hydo-explore shrink <explore dir> [--seed N]... [--errors]");
                return 2;
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: hydo-explore shrink <explore dir> [--seed N]... [--errors]");
        return 2;
    };
    if seeds.is_empty() && !errors {
        match hydo_explore::run::shrinks::unshrunk_groups(&dir) {
            Ok(g) => seeds = g.into_iter().map(|x| x.1).collect(),
            Err(e) => {
                eprintln!("hydo-explore shrink: {e}");
                return 2;
            }
        }
        errors = true;
    }
    let what = hydo_explore::run::shrinks::ShrinkWhat { seeds, errors };
    match hydo_explore::shrink_main(&dir, &what, env) {
        Ok(o) => {
            for (seed, minimal, tries) in o.shrunk {
                println!("seed {seed}: {minimal} ({tries} shrink runs)");
            }
            for (sig, minimal, tries) in o.errors {
                println!("error kind {sig}: {minimal} ({tries} shrink runs)");
            }
            0
        }
        Err(e) => {
            eprintln!("hydo-explore shrink: {e}");
            2
        }
    }
}
