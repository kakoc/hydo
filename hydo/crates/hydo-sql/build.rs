//! Copies hydo-read's PostgreSQL, MySQL and hydo-check modules into OUT_DIR without their
//! module documentation (`//!` lines cannot be `include!`d), so this crate compiles them as its
//! own modules and adds session code beside their private items (src/lib.rs). hydo-read's
//! sources are not changed: the hydo-read inside recorded images keeps its bytes.

use std::path::PathBuf;

fn main() {
    let here = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let src = here.join("../hydo-read/src");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    for f in ["pg.rs", "mysql.rs", "check.rs"] {
        let p = src.join(f);
        println!("cargo:rerun-if-changed={}", p.display());
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let body: String = text
            .lines()
            .map(|l| if l.starts_with("//!") { "" } else { l })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(out.join(f), body + "\n").expect("write");
    }
}
