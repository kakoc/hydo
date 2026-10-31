//! hydo-elfdeps [--root DIR] FILE... -- print the shared-library closure of some ELF files, as
//! absolute paths (files are dynamically linked nix-store binaries/libraries).
//!
//! Reads PT_INTERP, DT_NEEDED, DT_RUNPATH/DT_RPATH straight from the ELF (no readelf, so it works
//! for foreign architectures), resolves each needed library through the RUNPATH of the object that
//! needs it, and recurses. Prints every resolved file (and every symlink on the way to it) once,
//! sorted, so an image builder can copy exactly what the dynamic loader will open. A library that
//! cannot be resolved is an error.
//!
//! --root DIR: the files live under DIR (DIR/nix/store/..., e.g. a tree unpacked by
//! hydo-nix-fetch on a Mac without /nix): FILE arguments and the printed paths are the guest's
//! absolute paths, every read and symlink hop is made under DIR.
//!
//! A command line around hydo-build's port (hydo_build::elfdeps), which replaced
//! hv/hvm/services/elfdeps.py (M108): the same command line and output.

fn main() {
    let mut a: Vec<String> = std::env::args().skip(1).collect();
    let mut root = String::new();
    if a.first().map(String::as_str) == Some("--root") {
        let Some(d) = a.get(1) else {
            hydo_tools::die("usage: hydo-elfdeps [--root DIR] FILE...");
        };
        root = hydo_manifest::py::abspath(d)
            .trim_end_matches('/')
            .to_string();
        a.drain(..2);
    }
    match hydo_build::elfdeps::Elfdeps::new(&root).closure(&a) {
        Ok(v) => {
            for o in v {
                println!("{o}");
            }
        }
        Err(e) => hydo_tools::die(&e),
    }
}
