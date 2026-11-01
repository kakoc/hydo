//! With the cargo feature `pt` (Linux): compile the coverage decoder `pt/ptedges.c` and link
//! libipt. Without it nothing is built here.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "pt")]
    {
        println!("cargo:rerun-if-changed=pt/ptedges.c");
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
            cc::Build::new()
                .file("pt/ptedges.c")
                .opt_level(2)
                .warnings(true)
                .compile("hydo_ptedges");
            println!("cargo:rustc-link-lib=ipt");
        }
    }
}
