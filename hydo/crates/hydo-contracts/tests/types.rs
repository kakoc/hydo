//! The types of Milestone 255, each read from a small compiled program: Rust `String`, `&str`,
//! `Vec<T>`, slices, `Option` and `Result` (by value, through a reference, and as the result in
//! registers or in memory), and Go strings, slices, results after the first and `error`. The
//! programs are the contracts-ledger example's `types/rust` and `types/go`; the test builds them
//! for arm64 and x86_64 when the toolchains are here (Rust 1.90.0 with the musl targets, Go), and
//! otherwise says so and checks nothing else.

use hydo_contracts::compile::{compile, Source};
use hydo_contracts::plan::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard/examples/contracts-ledger/types")
}

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("contracts-types");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The Rust program for `arch` (`aarch64`, `x86_64`), built with Rust 1.90.0.
fn rust_program(arch: &str) -> Option<PathBuf> {
    let t = format!("{arch}-unknown-linux-musl");
    let target = out_dir().join("rust");
    let ok = Command::new("cargo")
        .args(["+1.90.0", "build", "--release", "--quiet", "--target", &t])
        .arg("--target-dir")
        .arg(&target)
        .env(
            "RUSTFLAGS",
            "-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static",
        )
        .current_dir(example().join("rust"))
        .status()
        .is_ok_and(|s| s.success());
    let b = target.join(&t).join("release/types");
    if !ok || !b.exists() {
        eprintln!(
            "NOT CHECKED: cargo +1.90.0 with the target {t} did not build {}",
            b.display()
        );
        return None;
    }
    Some(b)
}

/// The Go program for `goarch` (`arm64`, `amd64`).
fn go_program(goarch: &str) -> Option<PathBuf> {
    let b = out_dir().join(format!("types-go-{goarch}"));
    let ok = Command::new("go")
        .args(["build", "-trimpath", "-o"])
        .arg(&b)
        .arg(".")
        .env("CGO_ENABLED", "0")
        .env("GOOS", "linux")
        .env("GOARCH", goarch)
        .current_dir(example().join("go"))
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("NOT CHECKED: go did not build the program for {goarch}");
        return None;
    }
    Some(b)
}

fn plan(bin: PathBuf, src: &str) -> Program {
    let p = compile(&[Source {
        program: "/bin/types".into(),
        binary: bin,
        sources: vec![example().join(src)],
    }])
    .unwrap();
    // kept for a look: target/tmp/contracts-types/<program>.contracts.json
    let name = format!("{}-{}", src.replace('/', "-"), p.programs[0].arch);
    std::fs::write(
        out_dir().join(format!("{name}.contracts.json")),
        plan_text(&p),
    )
    .unwrap();
    let prog = p.programs.into_iter().next().unwrap();
    assert!(prog.unchecked.is_empty(), "{:#?}", prog.unchecked);
    prog
}

fn func<'a>(p: &'a Program, name: &str) -> &'a Function {
    p.functions
        .iter()
        .find(|f| f.name.ends_with(name))
        .unwrap_or_else(|| panic!("{name} not in {:?}", p.functions))
}

fn names(ls: &[Loc]) -> Vec<&str> {
    ls.iter().map(|l| l.name.as_str()).collect()
}

/// What every clause compiled to, as a few facts that do not depend on the architecture.
fn check_rust(p: &Program, arch: &str) {
    // Rust 1.90 passes the address of a result in memory in x0 on arm64 (rdi on x86_64)
    let sret = if arch == "arm64" { 0 } else { 5 };
    // strings, vectors, slices: lengths at entry; a reference's vector at return; elements up
    // to the bound for v[k]
    let t = func(p, "types::text");
    assert_eq!(t.clauses.len(), 5);
    let n = names(&t.at_entry);
    for want in [
        "len(s)",
        "s[0..5]",
        "len(owned)",
        "owned[0..3]",
        "len(sl)",
        "sl[1]",
        "k",
    ] {
        assert!(n.contains(&want), "{arch}: {want} not in {n:?}");
    }
    let r = names(&t.at_return);
    assert!(
        r.contains(&"len(v)") && r.contains(&"result"),
        "{arch}: {r:?}"
    );
    assert!(
        matches!(&t.clauses[2].expr, Expr::Bin(..)),
        "{:?}",
        t.clauses[2].expr
    );
    let s5 = t.at_entry.iter().find(|l| l.name == "s[0..5]").unwrap();
    assert_eq!((s5.size, s5.path.len()), (5, 1), "{s5:?}");
    // options and results: the discriminant and the value in two registers, a niche pointer in
    // one, a Result passed in memory
    let o = func(p, "types::opt");
    let n = names(&o.at_entry);
    for want in [
        "the variant of o",
        "some(o)",
        "the variant of r",
        "err(r)",
        "some(p).x",
    ] {
        assert!(n.contains(&want), "{arch}: {want} not in {n:?}");
    }
    let res = names(&o.at_return);
    assert!(
        res.contains(&"the variant of result") && res.contains(&"some(result)"),
        "{arch}: {res:?}"
    );
    let disc = o
        .at_return
        .iter()
        .find(|l| l.name == "the variant of result")
        .unwrap();
    let val = o
        .at_return
        .iter()
        .find(|l| l.name == "some(result)")
        .unwrap();
    assert_eq!(
        (disc.base, val.base),
        (Base::Reg(0), Base::Reg(1)),
        "{arch}"
    );
    // a Vec and a String by value (in memory), a Result returned in memory the caller gave
    let w = func(p, "types::owned");
    let n = names(&w.at_entry);
    for want in ["len(v)", "v[2]", "len(s)", "s[0..2]", "address of result"] {
        assert!(n.contains(&want), "{arch}: {want} not in {n:?}");
    }
    let a = w
        .at_entry
        .iter()
        .find(|l| l.name == "address of result")
        .unwrap();
    assert_eq!(a.base, Base::Reg(sret), "{arch}");
    assert!(w.at_return.iter().all(|l| matches!(l.base, Base::In(_))));
    // a String result in memory, and a Result read through a reference
    let c = func(p, "types::copy");
    assert!(
        names(&c.at_entry).contains(&"len(err(r))"),
        "{:?}",
        names(&c.at_entry)
    );
    assert!(names(&c.at_return).contains(&"len(result)"));
}

fn check_go(p: &Program, arch: &str) {
    let t = func(p, "main.text");
    let n = names(&t.at_entry);
    for want in [
        "len(s)", "s[0..5]", "s[4]", "len(v)", "v[0]", "k", "ps[0].X", "ps[0].Y",
    ] {
        assert!(n.contains(&want), "{arch}: {want} not in {n:?}");
    }
    // v[k]: the elements 0 to 7, each guarded by the length
    assert!(n.contains(&"v[7]"), "{arch}: {n:?}");
    // three results: a string in two registers, an int, an error in two registers
    let s = func(p, "main.split");
    let r = &s.at_return;
    let reg = |name: &str| r.iter().find(|l| l.name == name).map(|l| l.base);
    let (r2, r3) = if arch == "arm64" {
        (Base::Reg(2), Base::Reg(3))
    } else {
        // RAX, RBX, RCX, RDI: DWARF 0, 3, 2, 5
        (Base::Reg(2), Base::Reg(5))
    };
    assert_eq!(
        reg("len(result.0)"),
        Some(match arch {
            "arm64" => Base::Reg(1),
            _ => Base::Reg(3),
        })
    );
    assert_eq!(reg("result.1"), Some(r2), "{arch}: {r:?}");
    assert_eq!(reg("result.2"), Some(r3), "{arch}: {r:?}");
    let f = func(p, "main.fail");
    let r = &f.at_return;
    assert_eq!(
        r.iter().find(|l| l.name == "result.1").map(|l| l.base),
        Some(if arch == "arm64" {
            Base::Reg(1)
        } else {
            Base::Reg(3)
        })
    );
}

#[test]
fn rust_types_on_arm64_and_x86_64() {
    for (arch, plan_arch) in [("aarch64", "arm64"), ("x86_64", "x86_64")] {
        if let Some(b) = rust_program(arch) {
            let p = plan(b, "rust/src");
            assert_eq!(p.arch, plan_arch);
            check_rust(&p, plan_arch);
        }
    }
}

#[test]
fn go_types_on_arm64_and_x86_64() {
    for (goarch, plan_arch) in [("arm64", "arm64"), ("amd64", "x86_64")] {
        if let Some(b) = go_program(goarch) {
            let p = plan(b, "go");
            assert_eq!(p.arch, plan_arch);
            check_go(&p, plan_arch);
        }
    }
}
