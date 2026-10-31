//! `hydo-bench`: the bug benchmark runner (hydo/bench/README.md). The root (the folder with
//! `hydo/` and `hv/`): `$HYDO_ROOT`, else the first parent of this program that has both, else
//! the source tree it was built from.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    if let Some(r) = std::env::var_os("HYDO_ROOT").filter(|v| !v.is_empty()) {
        return PathBuf::from(r);
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
    std::process::exit(hydo_bench::main_with(&argv, &root()));
}
