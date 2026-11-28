//! The error paths of Milestone 306 (docs/design/error-paths.md): the places the sources show,
//! the declarations that add and remove places, the hits an engine writes, and the plan of a
//! small Go and Rust program when the toolchains are here.

use hydo_contracts::compile::{compile, Source};
use hydo_contracts::paths::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn lines(p: &SrcPaths) -> Vec<(u32, &str)> {
    p.places.iter().map(|x| (x.line, x.kind)).collect()
}

#[test]
fn go_err_checks_are_the_first_line_of_their_block() {
    let src = r#"package x

func f() error {
	v, err := g()
	if err != nil {
		return fmt.Errorf("g: %w", err) // the place
	}
	if err := h(); err != nil {

		log.Print("h")
	}
	if txErr != nil && !errors.Is(txErr, io.EOF) {
		return txErr
	}
	if err != nil {
	}
	s := "if err != nil {"
	if v != nil {
		return nil
	}
	return nil
}
"#;
    let p = scan(src, SrcLang::Go);
    assert_eq!(
        lines(&p),
        vec![
            (6, kind::ERR_CHECK),
            (10, kind::ERR_CHECK),
            (13, kind::ERR_CHECK)
        ]
    );
}

#[test]
fn rust_err_returns_and_arms() {
    let src = r#"fn f(x: u32) -> Result<u32, E> {
    match g(x) {
        Ok(v) => v,
        Err(e) => return Err(e.into()),
    };
    match h() {
        Err(e) => {
            log(e);
        }
        Ok(_) => {}
    }
    if let Err(e) = k() {
        warn(e);
    }
    let s = "return Err(";
    if x == 0 {
        return Err(E::Zero);
    }
    Err(E::Other)
}
"#;
    let p = scan(src, SrcLang::Rust);
    assert_eq!(
        lines(&p),
        vec![
            (4, kind::ERR_ARM),
            (8, kind::ERR_ARM),
            (13, kind::ERR_ARM),
            (17, kind::ERR_RETURN),
            (19, kind::ERR_RETURN)
        ]
    );
}

#[test]
fn declarations_add_and_remove_places() {
    let src = r#"package x

// hydo: no error path
func quiet() {
	if err != nil {
		return
	}
}

func loud() {
	// hydo: error path
	fallback()
	// hydo: no error path
	if err != nil {
		return
	}
	if err != nil {
		return
	}
}
"#;
    let p = scan(src, SrcLang::Go);
    let kept: Vec<u32> = p
        .places
        .iter()
        .filter(|x| !p.is_removed(x.line))
        .map(|x| x.line)
        .collect();
    assert_eq!(kept, vec![12, 18]);
    assert_eq!(p.places[1].kind, kind::DECLARED);
    // a contract's scanner reads the declarations as comments
    let (contracts, errs) = hydo_contracts::lang::scan(src);
    assert!(contracts.is_empty() && errs.is_empty(), "{errs:?}");
}

#[test]
fn recovery_names_and_short_names() {
    assert!(is_recovery_name(
        "github.com/blnkfinance/blnk.(*Blnk).RetryQueuedTransactions"
    ));
    assert!(is_recovery_name("svc::store::rollback_tx"));
    assert!(!is_recovery_name("main.retire"));
    assert_eq!(
        short_function("github.com/blnkfinance/blnk/database.(*Datasource).GetBalance"),
        "database.(*Datasource).GetBalance"
    );
    assert_eq!(short_function("ledger::apply"), "ledger::apply");
}

#[test]
fn hits_read_back_and_records_skip_them() {
    let d = crate::tmpdir::TmpDir::new("paths-hits");
    let p = d.path().join("seed-1.contracts.jsonl");
    let h = Hits {
        hits: vec![(3, 120, 0), (7, 4000, 1)],
        unwatched: vec![9],
        ..Hits::default()
    };
    let text = format!(
        "{}{}",
        hits_line(&h),
        hydo_contracts::plan::records_text(&[], 0)
    );
    std::fs::write(&p, text).unwrap();
    assert_eq!(read_hits(&p).unwrap(), Some(h.clone()));
    let (recs, end) = hydo_contracts::plan::read_run(&p).unwrap();
    assert!(recs.is_empty());
    assert_eq!(end.recorded, 0);
    assert_eq!(h.list()[1].boot, 1);
    assert_eq!(read_hits(&d.path().join("none")).unwrap(), None);
    // the breakpoint the Mac engine writes
    assert_eq!(brk_word(BRK_IMM), 0xd429_0b20);
}

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("contracts-paths");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The Go program of tests/go-paths for arm64, built here when Go is.
fn go_program() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/go-paths");
    let b = out_dir().join("paths-go-arm64");
    let ok = Command::new("go")
        .args(["build", "-trimpath", "-buildvcs=false", "-o"])
        .arg(&b)
        .arg(".")
        .env("CGO_ENABLED", "0")
        .env("GOOS", "linux")
        .env("GOARCH", "arm64")
        .current_dir(&dir)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("NOT CHECKED: go did not build {}", dir.display());
        return None;
    }
    Some(b)
}

/// The Rust program of tests/rust-paths for arm64, built here with Rust 1.90.0 and the musl
/// target when they are.
fn rust_program() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rust-paths");
    let t = "aarch64-unknown-linux-musl";
    let target = out_dir().join("rust");
    let ok = Command::new("cargo")
        .args(["+1.90.0", "build", "--release", "--quiet", "--target", t])
        .arg("--target-dir")
        .arg(&target)
        .env(
            "RUSTFLAGS",
            "-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static",
        )
        .current_dir(&dir)
        .status()
        .is_ok_and(|s| s.success());
    let b = target.join(t).join("release/rust-paths");
    if !ok || !b.exists() {
        eprintln!(
            "NOT CHECKED: cargo +1.90.0 with the target {t} did not build {}",
            b.display()
        );
        return None;
    }
    Some(b)
}

#[test]
fn a_rust_programs_try_unwrap_and_panic_places() {
    let Some(b) = rust_program() else { return };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rust-paths");
    let plan = compile(&[Source {
        program: "/bin/paths".into(),
        binary: b,
        sources: vec![dir],
        error_paths: true,
        all_functions: false,
    }])
    .unwrap();
    let got: Vec<(u32, &str)> = plan.programs[0]
        .paths
        .iter()
        .map(|x| (x.line, x.kind.as_str()))
        .collect();
    // line 18's `?` needs no conversion and line 20's `return Err` shares its code with another
    // return: in the optimized build neither has an instruction of its own (the first is no
    // place, the second counts among the places without code)
    assert_eq!(
        got,
        vec![
            (27, kind::TRY),
            (31, kind::ERR_ARM),
            (32, kind::ERR_RETURN),
            (38, kind::RECOVERY),
            (44, kind::UNWRAP),
            (46, kind::PANIC)
        ],
        "{:#?}",
        plan.programs[0].paths
    );
    assert_eq!(plan.programs[0].paths_without_code, 1);
    let f: Vec<&str> = plan.programs[0]
        .paths
        .iter()
        .map(|x| x.function.as_str())
        .collect();
    assert_eq!(f[0], "rust_paths::load");
    assert_eq!(f[5], "rust_paths::retry_load");
}

#[test]
fn a_go_programs_places_have_their_addresses_and_pages() {
    let Some(b) = go_program() else { return };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/go-paths");
    let plan = compile(&[Source {
        program: "/bin/paths".into(),
        binary: b.clone(),
        sources: vec![dir],
        error_paths: true,
        all_functions: false,
    }])
    .unwrap();
    let p = &plan.programs[0];
    let data = std::fs::read(&b).unwrap();
    let got: Vec<(u32, &str, &str)> = p
        .paths
        .iter()
        .map(|x| (x.line, x.kind.as_str(), short_function(&x.function)))
        .collect();
    assert_eq!(
        got,
        vec![
            (14, kind::ERR_CHECK, "main.load"),
            (20, kind::RECOVERY, "main.retryLoad"),
            (24, kind::ERR_CHECK, "main.retryLoad"),
            (30, kind::DECLARED, "main.retryLoad"),
        ],
        "{:#?}",
        p.paths
    );
    // every site's instruction is in its page of the file, and the page's hash is the file's
    for pl in &p.paths {
        assert!(!pl.sites.is_empty());
        for s in &pl.sites {
            let pg = &p.path_pages[s.page as usize];
            let start = pg.index as usize * PAGE;
            let o = start + s.off as usize;
            assert_eq!(
                u32::from_le_bytes(data[o..o + 4].try_into().unwrap()),
                s.word
            );
            let mut page = vec![0u8; PAGE];
            let end = (start + PAGE).min(data.len());
            page[..end - start].copy_from_slice(&data[start..end]);
            assert_eq!(page_hash(&page), pg.hash);
            assert_eq!(page_probe(&page), Some((pg.probe_off, pg.probe)));
        }
    }
    // the plan reads back
    let text = hydo_contracts::plan::plan_text(&plan);
    let back: hydo_contracts::plan::Plan = serde_json::from_str(&text).unwrap();
    assert_eq!(back, plan);
}

#[test]
fn every_function_of_a_go_program_is_a_place_at_its_first_instruction() {
    let Some(b) = go_program() else { return };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/go-paths");
    let plan = compile(&[Source {
        program: "/bin/paths".into(),
        binary: b.clone(),
        sources: vec![dir],
        error_paths: true,
        all_functions: true,
    }])
    .unwrap();
    let p = &plan.programs[0];
    let data = std::fs::read(&b).unwrap();
    // the error paths stay as they were, and the functions are a list of their own: the
    // program's own four, the runtime's and the packages' (each with code of its own)
    assert_eq!(p.paths.len(), 4);
    let own: Vec<&str> = p
        .fn_places
        .iter()
        .filter(|f| f.own)
        .map(|f| short_function(&f.function))
        .collect();
    assert_eq!(
        own,
        vec!["main.load", "main.retryLoad", "main.parse", "main.main"]
    );
    assert!(p.fn_places.len() > 500, "{}", p.fn_places.len());
    let mut addrs = std::collections::HashSet::new();
    for (i, f) in p.fn_places.iter().enumerate() {
        assert_eq!(f.id, i as u32);
        assert_eq!(f.kind, kind::FUNCTION);
        assert!(f.size > 0);
        assert_eq!(f.sites.len(), 1);
        assert!(addrs.insert(f.sites[0].addr), "two places at one address");
        // each function's first instruction is in its page of the file
        let s = &f.sites[0];
        let pg = &p.path_pages[s.page as usize];
        let o = pg.index as usize * PAGE + s.off as usize;
        assert_eq!(
            u32::from_le_bytes(data[o..o + 4].try_into().unwrap()),
            s.word
        );
    }
    let m = |n: &str| p.fn_places.iter().find(|f| f.function == n).unwrap();
    assert_eq!(m("main.load").module, "main");
    assert_eq!(m("main.load").file, "main.go");
    assert_eq!(m("runtime.main").module, "runtime");
    assert!(!m("runtime.main").own);
    // the recovery function's error path and its own place share one address
    let rl = p.paths.iter().find(|x| x.kind == kind::RECOVERY).unwrap();
    assert_eq!(rl.sites[0].addr, m("main.retryLoad").sites[0].addr);
    // Milestone 397's follow-up: the direct calls of the project's own code, by id; a function
    // of the runtime or a package keeps no edges
    let id = |n: &str| m(n).id;
    assert_eq!(m("main.main").calls, vec![id("main.retryLoad")]);
    assert_eq!(m("main.retryLoad").calls, vec![id("main.load")]);
    assert_eq!(m("main.load").calls, vec![id("main.parse")]);
    assert!(m("main.parse").calls.is_empty());
    assert!(m("runtime.main").calls.is_empty());
}

#[test]
fn a_functions_module_is_its_package_or_its_rust_path() {
    assert_eq!(
        module_of("github.com/blnkfinance/blnk/internal/metrics.Init", true),
        "github.com/blnkfinance/blnk/internal/metrics"
    );
    assert_eq!(
        module_of("github.com/blnkfinance/blnk.(*Blnk).validateTxn", true),
        "github.com/blnkfinance/blnk"
    );
    assert_eq!(module_of("runtime.main", true), "runtime");
    assert_eq!(module_of("slices.Sort[go.shape.string]", true), "slices");
    assert_eq!(
        module_of("redb::tree_store::btree::Btree::get", false),
        "redb::tree_store::btree::Btree"
    );
    assert_eq!(
        module_of("<alloc::vec::Vec<T> as core::ops::Drop>::drop", false),
        "<alloc::vec::Vec<T> as core::ops::Drop>"
    );
    assert_eq!(module_of("main", false), "main");
}

/// Milestone 397's follow-up: the direct calls of arm64 code, `bl` and `b` with a 26-bit
/// immediate, forwards and backwards; other instructions are no call.
#[test]
fn arm64_direct_calls_are_read_from_bl_and_b() {
    let words: [u32; 4] = [
        0x9400_0002, // bl +8
        0xd503_201f, // nop
        0x17ff_ffff, // b -4
        0x9600_0000, // bl -0x8000000
    ];
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    assert_eq!(
        hydo_contracts::paths::arm64_branch_targets(&code, 0x1000_0000),
        vec![0x1000_0008, 0x1000_0004, 0x1000_000c - 0x800_0000]
    );
}
