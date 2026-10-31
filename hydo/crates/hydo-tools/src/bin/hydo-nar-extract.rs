//! hydo-nar-extract -- take files out of a Nix binary-cache archive (.nar.xz) without Nix.
//!
//!     hydo-nar-extract <file.nar.xz> <FileHash nix32> <out dir> <path in the archive>...
//!
//! Checks the compressed file against the narinfo's FileHash (sha256, Nix base-32), then streams
//! the NAR and writes each named regular file (e.g. lib/modules/6.6.35/kernel/drivers/net/veth.ko.xz)
//! under <out dir> with the same relative path, and prints its sha256. Used in Milestone 95 to add
//! the guest kernel's llc/stp/bridge/veth modules (network: bridge) to a release tree on a machine
//! without Nix, from https://cache.nixos.org/<hash>.narinfo of the guest kernel's store path.
//! The port of hydo/dist/linux/nar_extract.py (M108): the same command line and output.

use hydo_tools::die;
use hydo_tools::nar::{nix32, sha256_file, Nar, Xz};
use std::collections::BTreeSet;
use std::path::Path;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        die("usage: hydo-nar-extract <file.nar.xz> <FileHash nix32> <out dir> <path in the archive>...");
    }
    let (src, filehash, out) = (&a[0], &a[1], &a[2]);
    let mut want: BTreeSet<String> = a[3..].iter().cloned().collect();
    let got = nix32(&sha256_file(Path::new(src)).unwrap_or_else(|e| die(&e)));
    if got != filehash.rsplit(':').next().unwrap_or("") {
        die(&format!(
            "nar_extract: FileHash mismatch: {got}, narinfo says {filehash}"
        ));
    }
    println!("FileHash ok: sha256:{got}");
    let x = Xz::open(Path::new(src)).unwrap_or_else(|e| die(&e));
    let mut n = Nar::new(x);
    let r = n
        .expect("nix-archive-1")
        .and_then(|_| n.node("", &mut want, out));
    if let Err(e) = r {
        die(&e);
    }
    if !want.is_empty() {
        die(&format!(
            "nar_extract: not in the archive: {}",
            want.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
}
