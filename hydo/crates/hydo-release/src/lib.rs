//! The releases' license notices and corresponding-source archives (Milestone 158).
//!
//! Each release ships third-party software: the guest kernels, busybox, the iproute2 and iptables
//! closures, e2fsprogs and libfaketime with their glibc, the Redis example, the Rust standard
//! library and the Rust crates compiled into every program. What each shipped file is, under which
//! license, and where its source is, is written down once per platform in
//! `hydo/dist/<platform>/third-party.json` (the *component list*). From it this crate
//!
//! - checks that the list is complete ([`check`]; `cargo test -p hydo-release` runs it): every file
//!   of `assets.sha256` and `guest-programs.sha256`, and every literal destination of the
//!   platform's `build-release.sh`, belongs to a component; every component has a license, its
//!   full license texts (files under `hydo/dist/licenses/`) and a source; a copyleft component has
//!   its source archive pinned by sha256; every Rust crate compiled into a shipped program has a
//!   license and license texts in its registry source (or an override in the list);
//! - writes the release's `LICENSE`, `licenses/` and `THIRD-PARTY.md` ([`notices`]), after it has
//!   checked that every file of the release tree belongs to a component and every Nix store path
//!   in it to a component's `nix` entry;
//! - builds the sources archive `hydo-<platform>-<version>-sources.tar.gz` ([`sources`]): every
//!   pinned source archive (checked by sha256), the repository files the components name (our
//!   patches, kernel configurations and build recipes), the component list and the notices, as one
//!   tar with fixed times, owners and order and gzip without a name or time, so two builds from
//!   the same inputs give the same bytes on Linux and on a Mac.
//!
//! It also checks that the crates `cargo install hydo` builds can be published to crates.io and
//! build from there alone ([`publish`], Milestone 169; `cargo test -p hydo-release --test crates`).
//!
//! # The component list (format 1)
//!
//! ```text
//! {
//!   "format": 1,
//!   "platform": "linux" | "mac",
//!   "about": ["free text"],
//!   "nixpkgs": { "rev", "store_path", "archive", "sha256", "url" },   // the build recipes of every
//!                                                                     // component built by Nix
//!   "rust": [ { "files": [release paths], "manifest": "Cargo.toml",   // one entry per cargo build
//!               "packages": [...], "target": "x86_64-unknown-linux-musl" } ],
//!   "rust_overrides": [ { "name", "version", "license"?, "texts"?, "reason" } ],
//!   "components": [ {
//!       "id": "linux",                          // unique
//!       "name": "Linux kernel", "version": "6.6.35" ("@release": the release's version),
//!       "license": "GPL-2.0-only WITH Linux-syscall-note",   // an SPDX expression
//!       "files": ["guest/bzImage", "guest/modules/**"],      // release paths; * within one path
//!                                                            // segment, ** across segments
//!       "texts": ["hydo/dist/licenses/linux-6.6.35/COPYING"],   // repository paths, at least one
//!       "sources": [ { "file": "linux-6.6.35.tar.xz", "sha256": "…",   // included in the archive
//!                      "url": ["https://…"], "from": "nix:x86_64-linux:linux_6_6" } ],
//!                    // or { "url": [...] } alone: a source that is referenced, not included
//!       "repo": ["hv/hvm/linux/apply-cntpct.py"],           // our files that go into the archive
//!       "nix": [ { "system", "attr"?, "out": ["/nix/store/…"], "note"?,
//!                  "drv"?: "/nix/store/….drv",          // the derivation that built "out"
//!                  "nixpkgs"?: "nixpkgs-<rev>.tar.gz",  // an included tree other than the pinned
//!                                                       // one, in which "attr" built "out"
//!                  "recipe"?: "<name>.tar.gz" } ],      // an included derivation closure
//!            // a copyleft component built by Nix needs "attr" (in the pinned tree, or in the
//!            // tree "nixpkgs" names) or "recipe", so that its build recipe is in the archive
//!       "note": "how it was built, and anything a reader of THIRD-PARTY.md needs"
//!   } ]
//! }
//! ```
//!
//! `from` says how `hydo/dist/fetch-sources.sh` obtains an included source again on a Linux
//! machine with Nix and the pinned nixpkgs: `url` (downloaded), `nix:<system>:<attr>:<name>` (a
//! fixed-output input of that package's derivation with that name, a directory packed as a
//! deterministic `.tar.gz`), `nix-in:<tree store path>:<system>:<attr>:<name>` (the same in
//! another nixpkgs tree), `nix-cache:<store path>` (a file output read from cache.nixos.org),
//! `nix-pack:<store path>` (a store directory packed; a tree the store lacks is fetched from the
//! source's `url` with `builtins.fetchTarball`) or `nix-recipe:<tree store path>:<system>:<attr>`
//! (the derivation closure of that attribute, packed: `derivations.json`, `drv/`, `inputs/`;
//! Milestone 158 addendum). A sources cache is a directory with the
//! files by name; the release scripts take it from `$HYDO_SOURCES_FROM` or fill
//! `hydo/dist/<platform>/sources/` once, as they do with the assets.

pub mod check;
pub mod glob;
pub mod model;
pub mod notices;
pub mod publish;
pub mod rust;
pub mod sources;

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// The hex sha256 of a file.
pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Whether an SPDX expression names a copyleft license (any option of it): then the component's
/// source archive must be included in the sources archive.
pub fn copyleft(license: &str) -> bool {
    const WORDS: &[&str] = &[
        "GPL",
        "LGPL",
        "AGPL",
        "MPL",
        "Sleepycat",
        "EPL",
        "CDDL",
        "EUPL",
        "OSL",
        "copyleft",
    ];
    license
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        .any(|t| WORDS.iter().any(|w| t.starts_with(w)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn copyleft_words() {
        assert!(super::copyleft("GPL-2.0-only WITH Linux-syscall-note"));
        assert!(super::copyleft("BSD-3-Clause OR GPL-2.0-only"));
        assert!(super::copyleft("(LGPL-3.0-or-later OR GPL-2.0-or-later)"));
        assert!(super::copyleft("MPL-2.0"));
        assert!(super::copyleft("Sleepycat"));
        assert!(!super::copyleft("MIT OR Apache-2.0"));
        assert!(!super::copyleft("Apache-2.0 WITH LLVM-exception"));
        assert!(!super::copyleft("BSD-3-Clause AND Zlib"));
    }
}
