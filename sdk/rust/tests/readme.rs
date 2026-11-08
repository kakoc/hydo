//! The README's complete program is `examples/kv_store.rs`, and running it writes exactly the
//! lines the README shows after it. (The README is also the crate documentation, so its other
//! code blocks run as doctests: `cargo test --doc`.)
//!
//! This file is its own test program with one test: the SDK's state (the catalog, which outcomes
//! were written, where records go) belongs to the process.

mod kv_store {
    include!("../examples/kv_store.rs");

    pub fn run() {
        main()
    }
}

const README: &str = include_str!("../README.md");
const PROGRAM: &str = include_str!("../examples/kv_store.rs");

/// The text of the first fenced block with the info string `info` after the heading `heading`.
fn block_after(heading: &str, info: &str) -> String {
    let start = README
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("README.md has no heading {heading:?}"));
    let rest = &README[start..];
    let open = format!("\n```{info}\n");
    let i = rest
        .find(&open)
        .unwrap_or_else(|| panic!("no ```{info} block after {heading:?}"))
        + open.len();
    let end = rest[i..].find("\n```\n").unwrap();
    rest[i..i + end + 1].to_string()
}

#[test]
fn the_complete_program_writes_what_the_readme_shows() {
    assert_eq!(
        block_after("## A complete program", "rust"),
        PROGRAM,
        "the README's complete program and examples/kv_store.rs must be the same text"
    );
    let dir = std::env::temp_dir().join(format!("hydo-assert-readme-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("console");
    std::fs::write(&out, "").unwrap();
    std::env::set_var("HYDO_ASSERT_OUT", &out);
    std::env::remove_var("HYDO_BUGGIFY");
    std::env::set_var("HYDO_BUGGIFY_FILE", dir.join("no-such-file"));
    kv_store::run();
    let got = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let want = block_after("## A complete program", "text");
    if cfg!(feature = "noop") {
        assert_eq!(got, "", "with the feature noop the program writes nothing");
    } else {
        assert_eq!(got, want, "what the program wrote differs from the README's lines");
    }
}
