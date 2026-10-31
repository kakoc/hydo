//! Build-machine and release-asset helpers, ported from the Python tools they replace (M108):
//!
//! - [`nar`]: Nix binary-cache archives without Nix (`hydo-nar-extract`, was
//!   hydo/dist/linux/nar_extract.py; `hydo-nix-fetch`, was hydo/dist/linux/nix_cache_fetch.py);
//! - [`cpio`]: a gzip'd newc cpio unpacked without root (`hydo-cpio-extract`, was
//!   hydo/dist/mac/cpio_extract.py);
//! - `hydo-elfdeps` (was hv/hvm/services/elfdeps.py) is a command line around hydo-build's port
//!   ([`hydo_build::elfdeps`]).
//!
//! Each program has the Python tool's command line, prints the same lines and writes the same
//! files (docs/HISTORY.md, Milestone 108). None of them is shipped in a release: they fetch and
//! unpack the release's pinned assets, once, on a maintainer's machine.

pub mod cpio;
pub mod elfcomment;
pub mod nar;

/// Python's `os.path.join(a, b)` for two POSIX path parts.
pub fn pyjoin(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `raise SystemExit(msg)`: the message on stderr, exit status 1.
pub fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

/// The hex digest of a SHA-256.
pub fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_like_python() {
        assert_eq!(pyjoin("out", "a/b"), "out/a/b");
        assert_eq!(pyjoin("out/", "a"), "out/a");
        assert_eq!(pyjoin("", "a"), "a");
        assert_eq!(pyjoin("out", "/abs"), "/abs");
    }
}
