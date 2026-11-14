//! The plan of the contracts-ledger example, from its Rust and Go programs' debug information.
//! The programs are built by the example's build.sh (they are not in the repository); without
//! them these tests say so and check nothing else.

use hydo_contracts::compile::{compile, Source};
use hydo_contracts::plan::*;
use std::path::{Path, PathBuf};

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard/examples/contracts-ledger")
}

fn plan_of(bin: &str, src: &str) -> Option<Plan> {
    let b = example().join("bin").join(bin);
    if !b.exists() {
        eprintln!(
            "NOT CHECKED: {} is missing; run {}/build.sh first",
            b.display(),
            example().display()
        );
        return None;
    }
    Some(
        compile(&[Source {
            program: "/bin/ledger".into(),
            binary: b,
            sources: vec![example().join(src)],
        }])
        .unwrap(),
    )
}

fn check(plan: &Plan, lang: Lang, frame: Frame) {
    let p = &plan.programs[0];
    assert!(p.unchecked.is_empty(), "{:?}", p.unchecked);
    assert_eq!(p.functions.len(), 1);
    let f = &p.functions[0];
    assert_eq!((f.lang, f.frame), (lang, frame));
    assert_eq!(f.clauses.len(), 4);
    assert_eq!(f.inputs.len(), 1);
    assert_eq!(f.code_bytes().len(), 16);
    // arguments in x0 (the ledger), x2 (the request id) and x3 (the amount) at entry; the
    // ledger's fields read at entry and, through its pointer, again at return
    let names: Vec<&str> = f.at_entry.iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"amount"), "{names:?}");
    let amount = f.at_entry.iter().find(|l| l.name == "amount").unwrap();
    assert_eq!(
        (amount.base, amount.path.len(), amount.size),
        (Base::Reg(3), 0, 8)
    );
    let ret: Vec<&str> = f.at_return.iter().map(|l| l.name.as_str()).collect();
    assert!(
        ret.contains(&"result") && ret.contains(&"l.balance"),
        "{ret:?}"
    );
    let res = f.at_return.iter().find(|l| l.name == "result").unwrap();
    assert_eq!(res.base, Base::Reg(0));
    let bal = f.at_return.iter().find(|l| l.name == "l.balance").unwrap();
    assert!(matches!(bal.base, Base::In(_)), "{bal:?}");
}

#[test]
fn the_rust_ledger() {
    if let Some(p) = plan_of("ledger-rs", "rust") {
        check(&p, Lang::Rust, Frame::Sp);
        assert_eq!(p.programs[0].functions[0].name, "ledger::apply");
        assert_eq!(p.programs[0].functions[0].file, "src/main.rs");
    }
}

#[test]
fn the_go_ledger() {
    if let Some(p) = plan_of("ledger-go", "go") {
        check(&p, Lang::Go, Frame::G);
        assert_eq!(p.programs[0].functions[0].name, "main.apply");
    }
}
