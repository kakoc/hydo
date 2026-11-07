//! The crates of `cargo install hydo` can be published (Milestone 169, docs/PUBLISHING.md
//! "Publishing the hydo command"): the package `hydo` and every workspace crate it depends on
//! have their metadata and version, publish their sources only, read no file outside their
//! directory at compile time, and are listed in docs/PUBLISHING.md in an order that publishes
//! each after its dependencies. A failure says what to change and where.

use hydo_release::publish::{self, Piece};

fn repo() -> std::path::PathBuf {
    hydo_release::model::repo_root()
}

#[test]
fn every_crate_of_hydo_is_publishable() {
    let errs = publish::check(&repo()).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        errs.is_empty(),
        "the hydo package cannot be published as it is ({} problems):\n{}",
        errs.len(),
        errs.join("\n")
    );
}

#[test]
fn the_closure_is_the_product_crates_without_the_guest_programs() {
    let m = publish::members(&repo()).unwrap();
    let set = publish::closure(&m);
    assert!(set.contains("hydo") && set.contains("hydo-config") && set.contains("hydo-hvm"));
    for g in publish::GUEST_PROGRAMS {
        assert!(!set.contains(*g), "{g}");
    }
    // the computed order is one the documentation may use
    let order = publish::order(&m);
    assert_eq!(order.len(), set.len(), "a dependency cycle: {order:?}");
    assert_eq!(order.last().map(String::as_str), Some("hydo"));
}

#[test]
fn includes_and_parent_paths_are_found_in_code_only() {
    let src = r#"
        // include_str!("../../not/this.txt") is a comment
        /* nor "../this" */
        const A: &str = include_str!("../pins/a.sha256");
        const B: &[u8] = include_bytes!( "x.bin");
        include!(concat!(env!("OUT_DIR"), "/gen.rs"));
        let q = '"'; let e = "a \" ../escaped"; let l: &'static str = "../../COMMIT";
    "#;
    assert_eq!(
        publish::included_literals(src),
        vec!["../pins/a.sha256".to_string(), "x.bin".to_string()]
    );
    assert_eq!(
        publish::parent_literals(src),
        vec![
            "../pins/a.sha256".to_string(),
            "a \\\" ../escaped".to_string(),
            "../../COMMIT".to_string()
        ]
    );
    assert!(publish::pieces("'a'")
        .iter()
        .all(|p| matches!(p, Piece::Code(_))));
    let lex = |p: &str| {
        publish::lexical(std::path::Path::new(p))
            .display()
            .to_string()
    };
    assert_eq!(lex("src/../pins/a"), "pins/a");
    assert_eq!(lex("src/../../../VERSION"), "../../VERSION");
    assert_eq!(lex("src/./x/../y"), "src/y");
}

#[test]
fn the_documented_order_is_read_from_the_publish_lines() {
    let doc = "text\n\n```sh\ncargo publish --registry crates-io -p hydo-config\n  cargo publish --registry crates-io --dry-run -p hydo\n```\ncargo publish -p other\n";
    assert_eq!(
        publish::documented_order(doc),
        vec!["hydo-config".to_string(), "hydo".to_string()]
    );
}
