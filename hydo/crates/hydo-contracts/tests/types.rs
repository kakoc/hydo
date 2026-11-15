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
        .args(["build", "-trimpath", "-buildvcs=false", "-o"])
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

/// The reads of a function, by name.
fn has(ls: &[Loc], want: &[&str], what: &str) {
    let n = names(ls);
    for w in want {
        assert!(n.contains(w), "{what}: {w} not in {n:?}");
    }
}

fn base<'a>(ls: &'a [Loc], name: &str) -> &'a Loc {
    ls.iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("{name} not in {:?}", names(ls)))
}

/// What every clause compiled to, as facts that hold on both architectures, and the registers
/// that each architecture's calling convention gives.
fn check_rust(p: &Program, arch: &str) {
    assert_eq!(p.functions.len(), 2, "{:?}", p.functions);
    // Rust 1.90 passes the address of a result in memory in x0 on arm64 (rdi on x86_64)
    let sret = if arch == "arm64" { 0 } else { 5 };
    // strings, vectors, slices: lengths at entry, a reference's vector at return, elements up
    // to the bound for v[k]; an Option in two registers
    let t = func(p, "types::text");
    assert_eq!(t.clauses.len(), 6);
    has(
        &t.at_entry,
        &[
            "len(s)",
            "s[0..5]",
            "len(owned)",
            "owned[0..3]",
            "len(v)",
            "v[0]",
            "v[7]",
            "k",
            "len(sl)",
            "sl[1]",
            "the variant of p",
            "some(p).x",
            "some(p).y",
        ],
        arch,
    );
    has(
        &t.at_return,
        &["the variant of result", "some(result)", "len(v)"],
        arch,
    );
    let s5 = base(&t.at_entry, "s[0..5]");
    assert_eq!((s5.size, s5.path.len()), (5, 1), "{s5:?}");
    assert_eq!(
        (
            base(&t.at_return, "the variant of result").base,
            base(&t.at_return, "some(result)").base
        ),
        (Base::Reg(0), Base::Reg(1)),
        "{arch}"
    );
    assert!(matches!(base(&t.at_return, "len(v)").base, Base::In(_)));
    // a Vec and a String by value (in memory), options and results by value, through a
    // reference and on the stack, and a Result returned in memory the caller gives
    let w = func(p, "types::owned");
    has(
        &w.at_entry,
        &[
            "len(v)",
            "v[2]",
            "len(s)",
            "s[0..2]",
            "the variant of r",
            "len(err(r))",
            "err(r)[0..4]",
            "the variant of o",
            "some(o)",
            "n",
            "address of result",
        ],
        arch,
    );
    assert_eq!(base(&w.at_entry, "address of result").base, Base::Reg(sret));
    // x86_64: n is the seventh word, on the stack: 8 above the stack pointer at entry, whatever
    // the prologue does to it (the frame base counts from after it); on arm64 a register
    let n = base(&w.at_entry, "n");
    if arch == "x86_64" {
        assert_eq!((n.base, n.path.clone()), (Base::Reg(7), vec![8]), "{n:?}");
        let k = base(&t.at_entry, "k");
        assert_eq!((k.base, k.path.clone()), (Base::Reg(7), vec![8]), "{k:?}");
        let p = base(&t.at_entry, "the variant of p");
        assert_eq!((p.base, p.path.clone()), (Base::Reg(7), vec![16]), "{p:?}");
    } else {
        assert!(n.path.is_empty(), "{n:?}");
    }
    assert!(w.at_return.iter().all(|l| matches!(l.base, Base::In(_))));
}

fn check_go(p: &Program, arch: &str) {
    assert_eq!(p.functions.len(), 2, "{:?}", p.functions);
    let t = func(p, "main.text");
    has(
        &t.at_entry,
        &[
            "len(s)", "s[0..5]", "s[4]", "len(v)", "v[0]", "v[7]", "k", "len(ps)", "ps[0].X",
            "ps[0].Y",
        ],
        arch,
    );
    // Go's results in the integer registers in order: arm64 R0, R1, ...; amd64 RAX, RBX, RCX,
    // RDI (DWARF 0, 3, 2, 5)
    let r = &t.at_return;
    let (second, third, fourth) = if arch == "arm64" {
        (Base::Reg(1), Base::Reg(2), Base::Reg(3))
    } else {
        (Base::Reg(3), Base::Reg(2), Base::Reg(5))
    };
    assert_eq!(base(r, "result.0").base, Base::Reg(0), "{arch}");
    assert_eq!(base(r, "result.1").base, second, "{arch}");
    // three results: a string in two registers, an int, an error in two registers
    let s = func(p, "main.split");
    let r = &s.at_return;
    assert_eq!(base(r, "len(result.0)").base, second, "{arch}");
    assert_eq!(base(r, "result.0[0..3]").base, Base::Reg(0), "{arch}");
    assert_eq!(base(r, "result.1").base, third, "{arch}");
    assert_eq!(base(r, "result.2").base, fourth, "{arch}");
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
