//! Helpers shared by the tests.
#![allow(dead_code)]

use hydo_read::check::check_main;
use std::path::PathBuf;

/// A temp dir with the given files.
pub fn write_files(files: &[(&str, &str)]) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "hydo-read-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for (n, c) in files {
        std::fs::write(d.join(n), c).unwrap();
    }
    d
}

/// A check in-process: its exit status and its last output line.
pub fn run(args: &[String]) -> (i32, String) {
    let args: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let rc = check_main(&args, &mut out, &mut err);
    let s = String::from_utf8_lossy(&out).into_owned();
    let last = s.trim().lines().last().unwrap_or("").to_string();
    (rc, last)
}
