//! A test's scratch folder, removed when the value is dropped: at the test's end, and also when
//! the test fails and unwinds (Milestone 271: the tests had left 10 GB of folders in the
//! temporary directory). `HYDO_KEEP_TMP=1` keeps every folder, to look at it after the test.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// A folder that is removed with its contents when this value is dropped.
pub struct TmpDir(PathBuf);

impl TmpDir {
    /// The empty folder `<the temporary directory>/<name>`.
    pub fn new(name: &str) -> TmpDir {
        TmpDir::fresh(std::env::temp_dir().join(name))
    }

    /// The folder at `p`, emptied and made.
    pub fn fresh(p: PathBuf) -> TmpDir {
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        TmpDir(p)
    }

    /// The folder at `p`, made by the caller, removed with this value.
    pub fn adopt(p: PathBuf) -> TmpDir {
        TmpDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TmpDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TmpDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for TmpDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        remove(&self.0);
    }
}

fn remove(p: &Path) {
    if std::env::var_os("HYDO_KEEP_TMP").is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("HYDO_KEEP_TMP: kept {}", p.display());
        return;
    }
    let _ = std::fs::remove_dir_all(p);
}

/// A folder that a `static` keeps for the whole test program, which is never dropped: it is
/// removed when the program exits (C's `atexit`, which the test harness's exit runs).
pub fn at_exit(p: PathBuf) -> PathBuf {
    use std::sync::{Mutex, Once};
    static DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    static HOOK: Once = Once::new();
    extern "C" fn remove_all() {
        let dirs = DIRS.lock().map(|d| d.clone()).unwrap_or_default();
        for d in dirs {
            remove(&d);
        }
    }
    extern "C" {
        fn atexit(f: extern "C" fn()) -> std::ffi::c_int;
    }
    // SAFETY: atexit only records the function, which runs at exit and touches only DIRS.
    HOOK.call_once(|| unsafe {
        atexit(remove_all);
    });
    DIRS.lock().unwrap().push(p.clone());
    p
}
