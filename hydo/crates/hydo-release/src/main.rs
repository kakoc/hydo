//! hydo-release: the release scripts' license steps (crate documentation; docs/RELEASING.md).
//!
//! ```text
//! hydo-release check <linux|mac> [--repo DIR]
//! hydo-release notices <linux|mac> --version V [--repo DIR] <release tree>
//! hydo-release sources <linux|mac> --version V --cache DIR --out FILE [--repo DIR]
//! hydo-release fetch-list <linux|mac> [--cache DIR] [--repo DIR]   (only what DIR lacks)
//! hydo-release signatures extract <unsigned tree> <signed tree> <hydo-mac-V-signatures.tar.gz>
//! hydo-release signatures attach <unsigned tree> <hydo-mac-V-signatures.tar.gz>
//! ```

use hydo_release::model::{List, Platform};
use hydo_release::{check, notices, rust, sources};
use std::path::PathBuf;
use std::process::exit;

const USAGE: &str = "usage: hydo-release check|notices|sources|fetch-list <linux|mac> [--repo DIR] [--version V] [--cache DIR] [--out FILE] [RELEASE_TREE]";

fn fail(msgs: &[String]) -> ! {
    for m in msgs {
        eprintln!("hydo-release: {m}");
    }
    eprintln!(
        "hydo-release: {} problem(s); the release is not built",
        msgs.len()
    );
    exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("signatures") {
        exit(signatures(&args[1..]));
    }
    if args.len() < 2 {
        eprintln!("{USAGE}");
        exit(2);
    }
    let cmd = args[0].as_str();
    let Some(p) = Platform::parse(&args[1]) else {
        eprintln!("{USAGE}");
        exit(2)
    };
    let (mut repo, mut version, mut cache, mut out, mut rest) = (
        hydo_release::model::repo_root(),
        None,
        None,
        None,
        Vec::new(),
    );
    let mut i = 2;
    while i < args.len() {
        let val = |i: usize| {
            args.get(i + 1).cloned().unwrap_or_else(|| {
                eprintln!("{USAGE}");
                exit(2)
            })
        };
        match args[i].as_str() {
            "--repo" => {
                repo = PathBuf::from(val(i));
                i += 1;
            }
            "--version" => {
                version = Some(val(i));
                i += 1;
            }
            "--cache" => {
                cache = Some(PathBuf::from(val(i)));
                i += 1;
            }
            "--out" => {
                out = Some(PathBuf::from(val(i)));
                i += 1;
            }
            a => rest.push(a.to_string()),
        }
        i += 1;
    }
    let list = List::load(&repo, p).unwrap_or_else(|e| fail(&[e]));
    match cmd {
        "check" => {
            let e = check::all(&repo, p).unwrap_or_else(|e| fail(&[e]));
            if !e.is_empty() {
                fail(&e);
            }
            println!(
                "{}: complete ({} components)",
                p.list_path(),
                list.components.len()
            );
        }
        "fetch-list" => print!("{}", sources::fetch_list(&list, cache.as_deref())),
        "notices" => {
            let (Some(v), [rel]) = (version, rest.as_slice()) else {
                eprintln!("{USAGE}");
                exit(2)
            };
            let rel = PathBuf::from(rel);
            let mut e = check::all(&repo, p).unwrap_or_else(|e| fail(&[e]));
            let crates = rust::crates(&repo, &list).unwrap_or_else(|e| fail(&[e]));
            if e.is_empty() {
                match notices::write(&repo, p, &list, &crates, &rel, &v) {
                    Ok(n) => println!(
                        "wrote LICENSE, licenses/ and THIRD-PARTY.md ({n} files, {} Rust crates)",
                        crates.len()
                    ),
                    Err(x) => e.push(x),
                }
                e.extend(notices::check_tree(p, &list, &rel));
            }
            if !e.is_empty() {
                fail(&e);
            }
        }
        "sources" => {
            let (Some(v), Some(c), Some(o)) = (version, cache, out) else {
                eprintln!("{USAGE}");
                exit(2)
            };
            let crates = rust::crates(&repo, &list).unwrap_or_else(|e| fail(&[e]));
            match sources::write(&repo, p, &list, &crates, &c, &v, &o) {
                Ok(h) => println!("{h}  {}", o.display()),
                Err(e) => fail(&[e]),
            }
        }
        _ => {
            eprintln!("{USAGE}");
            exit(2)
        }
    }
}

/// `hydo-release signatures extract|attach` (Milestone 172, docs/design/signing.md).
fn signatures(args: &[String]) -> i32 {
    use hydo_release::signatures as s;
    let res = match args {
        [c, unsigned, signed, out] if c == "extract" => {
            s::extract_tree(unsigned.as_ref(), signed.as_ref()).and_then(|sigs| {
                let h = s::write_archive(&sigs, out.as_ref())?;
                for x in &sigs {
                    println!("{}  {}  {}", x.unsigned_sha256, x.signed_sha256, x.path);
                }
                println!("{h}  {out}");
                Ok(())
            })
        }
        [c, tree, archive] if c == "attach" => s::read_archive(archive.as_ref()).and_then(|sigs| {
            s::attach_tree(tree.as_ref(), &sigs)?;
            for x in &sigs {
                println!("attached {} ({})", x.path, x.signed_sha256);
            }
            Ok(())
        }),
        _ => {
            eprintln!(
                "usage: hydo-release signatures extract <unsigned tree> <signed tree> <archive.tar.gz>\n       hydo-release signatures attach <unsigned tree> <archive.tar.gz>"
            );
            return 2;
        }
    };
    match res {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("hydo-release signatures: {e}");
            1
        }
    }
}
