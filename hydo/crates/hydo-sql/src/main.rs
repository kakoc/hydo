//! The hydo-sql program (see the library for what it does).

use std::os::unix::ffi::OsStrExt;

fn main() {
    let args: Vec<Vec<u8>> = std::env::args_os()
        .skip(1)
        .map(|a| a.as_bytes().to_vec())
        .collect();
    let rc = std::panic::catch_unwind(|| {
        let mut out = std::io::stdout().lock();
        let mut err = std::io::stderr().lock();
        hydo_sql::main_with(&args, &mut out, &mut err)
    })
    .unwrap_or(2);
    std::process::exit(rc);
}
