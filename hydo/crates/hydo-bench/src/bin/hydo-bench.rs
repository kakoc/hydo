//! `hydo-bench`: the bug benchmark runner (hydo/bench/README.md). The root (the folder with
//! `hydo/` and `hv/`): `$HYDO_ROOT`, else the first parent of this program that has both, else
//! the source tree it was built from. The configuration is read here, once (`hydo_config`,
//! M144), and passed down.

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
        if p.join("hydo").join("bench").is_dir() && p.join("hv").is_dir() {
            return p.to_path_buf();
        }
        d = p.parent();
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let config = hydo_config::Config::process();
    std::process::exit(hydo_bench::main_with(&argv, &root(&config.paths), &config));
}
