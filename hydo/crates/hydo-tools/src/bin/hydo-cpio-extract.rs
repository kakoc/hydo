//! hydo-cpio-extract <image.cpio.gz> <out dir> -- extract a gzip'd newc cpio (as hydo-build writes
//! arm64 images) into a directory, without root: regular files, directories and symlinks only
//! (device nodes are skipped). The port of hydo/dist/mac/cpio_extract.py (M108).

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 2 {
        hydo_tools::die("usage: hydo-cpio-extract <image.cpio.gz> <out dir>");
    }
    if let Err(e) = hydo_tools::cpio::extract(&a[0], &a[1]) {
        hydo_tools::die(&e);
    }
}
