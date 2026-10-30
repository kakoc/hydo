//! `hydo-explore`: the explorer on its own, with explore.py's command line (the release's entry
//! point is `hydo explore`, which calls the same library).
//!
//! The root (the folder with `hydo/` and `hv/`): `$HYDO_ROOT`, else the first parent of this
//! program that has both, else the source tree it was built from.

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
        if p.join("hydo").join("explore").is_dir() && p.join("hv").is_dir() {
            return p.to_path_buf();
        }
        d = p.parent();
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let env = hydo_explore::Env {
        root: root(),
        cli: std::env::var("HYDO_CLI").is_ok_and(|v| !v.is_empty()),
    };
    std::process::exit(hydo_explore::main_with(&argv, &env));
}
