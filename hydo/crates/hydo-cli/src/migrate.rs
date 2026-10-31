//! `hydo migrate [FILE] [--check] [--out FILE | --stdout]`: rewrite a project file in the
//! canonical spelling of the project format this hydo writes (M114, hydo-manifest's migrate.rs):
//! `format: 1` added when the file has none, retired format-1 spellings replaced. The rewrite is
//! checked before it is written: the new file must mean exactly the same (the same validated data
//! and plan). With no FILE: ./hydo/hydo.yaml. `--check` writes nothing and exits 1 when the file
//! would change.

use std::path::Path;

use crate::ctx::{die, die_rc, take};

const USAGE: &str =
    "usage: hydo migrate [hydo.yaml] [--check] [--out FILE | --stdout]   (no file: ./hydo/hydo.yaml)";

pub fn cmd_migrate(argv: &[String]) -> i32 {
    let mut args: Vec<String> = argv.to_vec();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return 0;
    }
    let out = take(&mut args, "--out");
    let check = remove_flag(&mut args, "--check");
    let stdout = remove_flag(&mut args, "--stdout");
    if let Some(bad) = args.iter().find(|a| a.starts_with('-')) {
        die_rc(&format!("migrate: unknown option {bad}\n{USAGE}"), 2);
    }
    if args.is_empty() && Path::new("hydo/hydo.yaml").is_file() {
        args.push("hydo/hydo.yaml".into());
    }
    if args.len() != 1 {
        die_rc(USAGE, 2);
    }
    let file = Path::new(&args[0]);
    let text = match std::fs::read(file) {
        Ok(b) => String::from_utf8_lossy(&b)
            .replace("\r\n", "\n")
            .replace('\r', "\n"),
        Err(e) => die(&format!("migrate: {}: {e}", file.display())),
    };
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let m = match hydo_manifest::migrate::migrate_text(&text, &name, file.parent()) {
        Ok(m) => m,
        Err(e) => die(&format!("migrate: {}: {}", file.display(), e.message)),
    };
    let shown = file.display();
    if !m.changed {
        eprintln!(
            "hydo migrate: {shown}: already project format {} in its canonical spelling",
            hydo_manifest::model::FORMAT
        );
        if stdout {
            print!("{}", m.text);
        }
        return 0;
    }
    for n in &m.notes {
        eprintln!("hydo migrate: {shown}: {n}");
    }
    if check {
        eprintln!("hydo migrate: {shown}: would be rewritten (run without --check to write it)");
        return 1;
    }
    if stdout {
        print!("{}", m.text);
        return 0;
    }
    let dest = out
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| file.to_path_buf());
    if let Err(e) = std::fs::write(&dest, &m.text) {
        die(&format!("migrate: {}: {e}", dest.display()));
    }
    eprintln!("hydo migrate: wrote {}", dest.display());
    0
}

fn remove_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let n = args.len();
    args.retain(|a| a != flag);
    args.len() != n
}
