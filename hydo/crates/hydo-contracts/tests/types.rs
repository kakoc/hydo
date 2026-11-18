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
        error_paths: false,
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

/// A named Go slice type (`type words []uint`, and `math/big.nat` inside a `*big.Int`) is read
/// as a slice: `len()` and elements (Milestone 260, from Blnk's balances).
#[test]
fn go_named_slices_on_arm64_and_x86_64() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/go-named-slice");
    for (goarch, plan_arch) in [("arm64", "arm64"), ("amd64", "x86_64")] {
        let b = out_dir().join(format!("named-slice-{goarch}"));
        let ok = Command::new("go")
            .args(["build", "-trimpath", "-buildvcs=false", "-o"])
            .arg(&b)
            .arg(".")
            .env("CGO_ENABLED", "0")
            .env("GOOS", "linux")
            .env("GOARCH", goarch)
            .current_dir(&dir)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("NOT CHECKED: go did not build the named-slice program for {goarch}");
            continue;
        }
        let p = compile(&[Source {
            program: "/bin/named".into(),
            binary: b,
            sources: vec![dir.clone()],
            error_paths: false,
        }])
        .unwrap();
        std::fs::write(
            out_dir().join(format!("named-slice-{plan_arch}.contracts.json")),
            plan_text(&p),
        )
        .unwrap();
        let prog = &p.programs[0];
        assert_eq!(prog.arch, plan_arch);
        assert!(prog.unchecked.is_empty(), "{:#?}", prog.unchecked);
        assert_eq!(prog.functions.len(), 2, "{:?}", prog.functions);
        // a slice parameter's elements are borrowed: read at return through the entry's pointer
        let f = func(prog, "main.first");
        has(&f.at_entry, &["len(w)"], plan_arch);
        has(&f.at_return, &["w[0]"], plan_arch);
        has(
            &func(prog, "main.floor").at_return,
            &["b.neg", "len(b.abs)", "b.abs[0]"],
            plan_arch,
        );
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

/// The bytes of an ELF file at a virtual address (its allocated sections only).
fn elf_bytes(data: &[u8], addr: u64, n: usize) -> Option<&[u8]> {
    let rd =
        |o: usize, k: usize| -> u64 { (0..k).fold(0, |a, i| a | (data[o + i] as u64) << (8 * i)) };
    let (shoff, shnum) = (rd(40, 8) as usize, rd(60, 2) as usize);
    for i in 0..shnum {
        let o = shoff + i * 64;
        let (ty, flags, a, off, size) = (
            rd(o + 4, 4),
            rd(o + 8, 8),
            rd(o + 16, 8),
            rd(o + 24, 8),
            rd(o + 32, 8),
        );
        if ty != 8 && flags & 2 != 0 && a <= addr && addr + n as u64 <= a + size {
            let f = (off + addr - a) as usize;
            return data.get(f..f + n);
        }
    }
    None
}

/// Milestone 285: a value history's reads of a Go program. The float64 field, a map's value by
/// a string key and by an integer key, an interface's dynamic value (with the runtime type
/// descriptors' addresses), an `error`, and a string key; the return address and the direct
/// call of the function.
#[test]
fn go_values_on_arm64_and_x86_64() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/go-values");
    for (goarch, plan_arch) in [("arm64", "arm64"), ("amd64", "x86_64")] {
        let b = out_dir().join(format!("values-{goarch}"));
        let ok = Command::new("go")
            .args(["build", "-trimpath", "-buildvcs=false", "-o"])
            .arg(&b)
            .arg(".")
            .env("CGO_ENABLED", "0")
            .env("GOOS", "linux")
            .env("GOARCH", goarch)
            .current_dir(&dir)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("NOT CHECKED: go did not build the values program for {goarch}");
            continue;
        }
        let p = compile(&[Source {
            program: "/bin/values".into(),
            binary: b.clone(),
            sources: vec![dir.clone()],
            error_paths: false,
        }])
        .unwrap();
        std::fs::write(
            out_dir().join(format!("values-{plan_arch}.contracts.json")),
            plan_text(&p),
        )
        .unwrap();
        let prog = &p.programs[0];
        assert!(prog.unchecked.is_empty(), "{:#?}", prog.unchecked);
        let f = func(prog, "main.mark");
        let v = |n: &str| {
            f.values
                .iter()
                .find(|v| v.name == n)
                .unwrap_or_else(|| panic!("{n} not in {:?}", f.values))
        };
        assert!(v("t.Ref").key && v("t.Ref").at_return.is_none());
        assert!(matches!(&v("t.Ref").entry, Some(Show::Text { chunks, .. }) if chunks.len() == 4));
        // a float64 at entry and at return, through the pointer as it was at entry
        assert!(matches!(
            &v("limit").entry,
            Some(Show::Val(Expr::In(_, Ty::F(8))))
        ));
        assert!(matches!(
            &v("limit").at_return,
            Some(Show::Val(Expr::Out(_, Ty::F(8))))
        ));
        // one group of 8 slots, each with its control byte, its key and an interface's value
        let Some(Show::Lookup { slots, .. }) = &v("meta.limit").at_return else {
            panic!("meta.limit: {:?}", v("meta.limit"))
        };
        assert_eq!(slots.len(), 8);
        assert!(v("meta.limit").entry.is_none());
        let Show::Dyn { cases, .. } = &slots[3].val else {
            panic!("a slot's value: {:?}", slots[3].val)
        };
        let data = std::fs::read(&b).unwrap();
        for (name, size, kind) in [("float64", 8, 14), ("bool", 1, 1), ("string", 16, 24)] {
            let c = cases.iter().find(|c| c.name == name).unwrap_or_else(|| {
                panic!(
                    "{name} not among {:?}",
                    cases.iter().map(|c| &c.name).collect::<Vec<_>>()
                )
            });
            // the descriptor at the address: internal/abi.Type's Size_ and Kind_
            let d =
                elf_bytes(&data, c.addr, 24).unwrap_or_else(|| panic!("{name} at {:#x}", c.addr));
            assert_eq!(
                u64::from_le_bytes(d[0..8].try_into().unwrap()),
                size,
                "{name}"
            );
            assert_eq!(d[23] & 0x1f, kind, "{name}'s kind");
        }
        assert!(matches!(&v("any").entry, Some(Show::Dyn { .. })));
        assert!(matches!(&v("err").at_return, Some(Show::Dyn { .. })));
        assert!(matches!(&v("count.7").entry, Some(Show::Lookup { .. })));
        // the caller: the return address at entry, and main.main's call of mark
        let ra = f.ret_addr.expect("a return address read") as usize;
        assert_eq!(f.at_entry[ra].name, "the return address");
        assert!(
            prog.calls
                .iter()
                .any(|c| c.callee == f.id && c.caller == "main.main"),
            "{:?}",
            prog.calls
        );
        // a clause compares a float64 with a floating-point literal
        let c = func(prog, "main.check");
        has(&c.at_entry, &["t.Limit"], plan_arch);
        assert!(c.values.is_empty() && c.ret_addr.is_none());
    }
}
