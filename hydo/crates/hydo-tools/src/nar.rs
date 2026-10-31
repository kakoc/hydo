//! Nix archives (NAR) read without Nix: Nix's base-32 hash text, an xz stream decoded in a
//! thread (pure Rust, lzma-rs), and the NAR reader that nar_extract.py and nix_cache_fetch.py
//! shared. Errors are the Python tools' `SystemExit` messages.

use sha2::{Digest, Sha256};
use std::io::{BufReader, Read};
use std::path::Path;
use std::thread::JoinHandle;

const NIX32: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Nix's base-32 text of a hash (nar_extract.py `nix32`).
pub fn nix32(h: &[u8]) -> String {
    let n = (h.len() * 8 - 1) / 5 + 1;
    let mut out = String::with_capacity(n);
    for k in (0..n).rev() {
        let b = k * 5;
        let (i, j) = (b / 8, b % 8);
        let mut c = (h[i] as u32) >> j;
        if i + 1 < h.len() {
            c |= (h[i + 1] as u32) << (8 - j);
        }
        out.push(NIX32[(c & 0x1f) as usize] as char);
    }
    out
}

/// The SHA-256 of a whole file.
pub fn sha256_file(p: &Path) -> Result<Vec<u8>, String> {
    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().to_vec())
}

/// An xz file decoded in a thread; reading gives the decoded bytes (Python `lzma.open(p, "rb")`).
/// A decoding error is reported where the stream ends.
pub struct Xz {
    pipe: std::io::PipeReader,
    worker: Option<JoinHandle<Result<(), String>>>,
}

impl Xz {
    pub fn open(p: &Path) -> Result<Xz, String> {
        let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let (r, mut w) = std::io::pipe().map_err(|e| e.to_string())?;
        let worker = std::thread::spawn(move || {
            let mut br = BufReader::with_capacity(1 << 20, f);
            match lzma_rs::xz_decompress(&mut br, &mut w) {
                Ok(()) => Ok(()),
                // the reader stopped reading (it had what it wanted, or it failed first)
                Err(lzma_rs::error::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::BrokenPipe =>
                {
                    Ok(())
                }
                Err(e) => Err(format!("xz: {e}")),
            }
        });
        Ok(Xz {
            pipe: r,
            worker: Some(worker),
        })
    }
}

impl Read for Xz {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.pipe.read(buf)?;
        if n == 0 {
            if let Some(w) = self.worker.take() {
                match w.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Err(std::io::Error::other(e)),
                    Err(_) => return Err(std::io::Error::other("xz: the decoder panicked")),
                }
            }
        }
        Ok(n)
    }
}

/// A reader that hashes what is read through it (nix_cache_fetch.py `Hashing`).
pub struct Hashing<R> {
    pub inner: R,
    pub h: Sha256,
}

impl<R: Read> Hashing<R> {
    pub fn new(inner: R) -> Hashing<R> {
        Hashing {
            inner,
            h: Sha256::new(),
        }
    }
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.h.update(&buf[..n]);
        Ok(n)
    }
}

/// The NAR reader (nar_extract.py `Nar`).
pub struct Nar<R> {
    pub f: R,
}

impl<R: Read> Nar<R> {
    pub fn new(f: R) -> Nar<R> {
        Nar { f }
    }

    /// Exactly n bytes, as Python's `f.read(n)` checked for length.
    pub fn raw(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut b = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match self.f.read(&mut b[got..]) {
                Ok(0) => return Err("nar_extract: truncated archive".into()),
                Ok(k) => got += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("nar_extract: {e}")),
            }
        }
        Ok(b)
    }

    pub fn num(&mut self) -> Result<u64, String> {
        let b = self.raw(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(&b);
        Ok(u64::from_le_bytes(a))
    }

    /// A length-prefixed, padded byte string, given to `sink` in pieces of at most 1 MiB.
    pub fn bytes_to(
        &mut self,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let n = self.num()?;
        let mut left = n;
        while left > 0 {
            let b = self.raw(left.min(1 << 20) as usize)?;
            left -= b.len() as u64;
            sink(&b)?;
        }
        let pad = ((8 - n % 8) % 8) as usize;
        if pad > 0 {
            self.raw(pad)?;
        }
        Ok(())
    }

    pub fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        self.bytes_to(&mut |b| {
            out.extend_from_slice(b);
            Ok(())
        })?;
        Ok(out)
    }

    /// A string token (Python `.decode()`: UTF-8).
    pub fn s(&mut self) -> Result<String, String> {
        let b = self.bytes()?;
        String::from_utf8(b).map_err(|_| "nar_extract: a token that is not UTF-8".to_string())
    }

    pub fn expect(&mut self, t: &str) -> Result<(), String> {
        let x = self.s()?;
        if x != t {
            return Err(format!(
                "nar_extract: expected {}, got {}",
                hydo_manifest::py::repr_str(t),
                hydo_manifest::py::repr_str(&x)
            ));
        }
        Ok(())
    }

    /// nar_extract.py `Nar.node`: walk one node; write each regular file whose archive path is in
    /// `want` under `out` (removing it from `want`) and print its sha256 and path.
    pub fn node(
        &mut self,
        path: &str,
        want: &mut std::collections::BTreeSet<String>,
        out: &str,
    ) -> Result<(), String> {
        self.expect("(")?;
        self.expect("type")?;
        let t = self.s()?;
        match t.as_str() {
            "regular" => {
                let mut x = self.s()?;
                if x == "executable" {
                    self.expect("")?;
                    x = self.s()?;
                }
                if x != "contents" {
                    return Err(format!(
                        "nar_extract: expected 'contents', got {}",
                        hydo_manifest::py::repr_str(&x)
                    ));
                }
                if want.contains(path) {
                    let mut h = Sha256::new();
                    let dst = crate::pyjoin(out, path);
                    let dir = hydo_manifest::py::dirname(&dst);
                    if !dir.is_empty() {
                        std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
                    }
                    let mut f = std::fs::File::create(&dst).map_err(|e| format!("{dst}: {e}"))?;
                    self.bytes_to(&mut |b| {
                        h.update(b);
                        std::io::Write::write_all(&mut f, b).map_err(|e| format!("{dst}: {e}"))
                    })?;
                    println!("{}  {}", crate::hex(&h.finalize()), path);
                    want.remove(path);
                } else {
                    self.bytes_to(&mut |_| Ok(()))?;
                }
                self.expect(")")?;
            }
            "symlink" => {
                self.expect("target")?;
                self.s()?;
                self.expect(")")?;
            }
            "directory" => loop {
                let x = self.s()?;
                if x == ")" {
                    break;
                }
                if x != "entry" {
                    return Err(format!(
                        "nar_extract: expected 'entry', got {}",
                        hydo_manifest::py::repr_str(&x)
                    ));
                }
                self.expect("(")?;
                self.expect("name")?;
                let name = self.s()?;
                self.expect("node")?;
                let sub = if path.is_empty() {
                    name
                } else {
                    format!("{path}/{name}")
                };
                self.node(&sub, want, out)?;
                self.expect(")")?;
            },
            _ => {
                return Err(format!(
                    "nar_extract: unknown node type {}",
                    hydo_manifest::py::repr_str(&t)
                ))
            }
        }
        Ok(())
    }
}

/// `os.path.lexists`.
pub fn lexists(p: &str) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn chmod(p: &str, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{p}: {e}"))
}

/// nix_cache_fetch.py `unpack`: unpack one NAR node at `dst` (None = read only). On a
/// case-insensitive file system two names that differ only in case are one file: the later one
/// is read (the hashes still cover it) but not written, and its path is added to `skipped`.
pub fn unpack<R: Read>(
    n: &mut Nar<R>,
    dst: Option<String>,
    skipped: &mut Vec<String>,
) -> Result<(), String> {
    let mut dst = dst;
    if let Some(d) = &dst {
        if lexists(d) {
            skipped.push(d.clone());
            dst = None;
        }
    }
    n.expect("(")?;
    n.expect("type")?;
    let t = n.s()?;
    match t.as_str() {
        "regular" => {
            let mut x = n.s()?;
            let ex = x == "executable";
            if ex {
                n.expect("")?;
                x = n.s()?;
            }
            if x != "contents" {
                return Err(format!(
                    "nix_cache_fetch: expected 'contents', got {}",
                    hydo_manifest::py::repr_str(&x)
                ));
            }
            match &dst {
                None => n.bytes_to(&mut |_| Ok(()))?,
                Some(d) => {
                    let mut f = std::fs::File::create(d).map_err(|e| format!("{d}: {e}"))?;
                    n.bytes_to(&mut |b| {
                        std::io::Write::write_all(&mut f, b).map_err(|e| format!("{d}: {e}"))
                    })?;
                    drop(f);
                    chmod(d, if ex { 0o555 } else { 0o444 })?;
                }
            }
            n.expect(")")?;
        }
        "symlink" => {
            n.expect("target")?;
            let tgt = n.s()?;
            if let Some(d) = &dst {
                std::os::unix::fs::symlink(&tgt, d).map_err(|e| format!("{d}: {e}"))?;
            }
            n.expect(")")?;
        }
        "directory" => {
            if let Some(d) = &dst {
                std::fs::create_dir(d).map_err(|e| format!("{d}: {e}"))?;
            }
            loop {
                let x = n.s()?;
                if x == ")" {
                    break;
                }
                if x != "entry" {
                    return Err(format!(
                        "nix_cache_fetch: expected 'entry', got {}",
                        hydo_manifest::py::repr_str(&x)
                    ));
                }
                n.expect("(")?;
                n.expect("name")?;
                let name = n.s()?;
                n.expect("node")?;
                if name.contains('/') || name.is_empty() || name == "." || name == ".." {
                    return Err(format!(
                        "nix_cache_fetch: bad entry name {}",
                        hydo_manifest::py::repr_str(&name)
                    ));
                }
                let sub = dst.as_ref().map(|d| crate::pyjoin(d, &name));
                unpack(n, sub, skipped)?;
                n.expect(")")?;
            }
            if let Some(d) = &dst {
                chmod(d, 0o555)?;
            }
        }
        _ => {
            return Err(format!(
                "nix_cache_fetch: unknown node type {}",
                hydo_manifest::py::repr_str(&t)
            ))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(&(b.len() as u64).to_le_bytes());
        out.extend_from_slice(b);
        out.resize(out.len() + (8 - b.len() % 8) % 8, 0);
    }

    #[test]
    fn nix32_of_known_hashes() {
        // `nix hash to-base32 sha256-<empty>` and of "abc"
        assert_eq!(
            nix32(&Sha256::digest(b"")),
            "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73"
        );
        assert_eq!(
            nix32(&Sha256::digest(b"abc")),
            "1b8m03r63zqhnjf7l5wnldhh7c134ap5vpj0850ymkq1iyzicy5s"
        );
    }

    #[test]
    fn reads_a_small_archive() {
        let mut a = Vec::new();
        for t in [
            &b"nix-archive-1"[..],
            b"(",
            b"type",
            b"directory",
            b"entry",
            b"(",
            b"name",
            b"f",
            b"node",
            b"(",
            b"type",
            b"regular",
            b"executable",
            b"",
            b"contents",
            b"hello",
            b")",
            b")",
            b"entry",
            b"(",
            b"name",
            b"l",
            b"node",
            b"(",
            b"type",
            b"symlink",
            b"target",
            b"f",
            b")",
            b")",
            b")",
        ] {
            tok(&mut a, t);
        }
        let mut n = Nar::new(&a[..]);
        n.expect("nix-archive-1").unwrap();
        let mut want: std::collections::BTreeSet<String> = ["f".to_string()].into();
        let d = std::env::temp_dir().join(format!("hydo-tools-nar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        n.node("", &mut want, &d.to_string_lossy()).unwrap();
        assert!(want.is_empty());
        assert_eq!(std::fs::read(d.join("f")).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&d);
    }
}
