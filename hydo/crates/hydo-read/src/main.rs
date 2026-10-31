//! The hydo-read / hydo-check program (see the library for what it does).

use std::os::unix::ffi::OsStrExt;

fn main() {
    let mut argv = std::env::args_os();
    let argv0 = argv
        .next()
        .map(|a| a.as_bytes().to_vec())
        .unwrap_or_default();
    let args: Vec<Vec<u8>> = argv.map(|a| a.as_bytes().to_vec()).collect();
    let rc = std::panic::catch_unwind(|| {
        let mut out = std::io::stdout().lock();
        let mut err = std::io::stderr().lock();
        hydo_read::main_with(&argv0, &args, &mut out, &mut err)
    })
    .unwrap_or(2);
    std::process::exit(rc);
}
