//! The Mac release's signatures, apart from the programs (Milestone 172, docs/design/signing.md).
//!
//! The Mac tarball is built unsigned and reproducibly (`hydo/dist/mac/build-release.sh`: two builds
//! give the same bytes). Signing a program with a Developer ID cannot be reproduced by anyone else
//! (only the key's owner can sign, and the secure timestamp in the signature differs at every
//! signing), so the signatures are published on their own, as the release asset
//! `hydo-mac-<version>-signatures.tar.gz`, and attached to an unsigned build:
//!
//! - **What codesign changes.** A Mach-O program built by the linker already carries an ad-hoc
//!   signature: the load command `LC_CODE_SIGNATURE` names a blob at the end of the file, inside
//!   the `__LINKEDIT` segment. `codesign` replaces that blob with the new signature (the code
//!   directory with every page's hash, the requirements, the entitlements, the CMS signature with
//!   the certificate chain and the timestamp) at the same file offset, and updates three numbers
//!   in the header: the blob's size in `LC_CODE_SIGNATURE`, and the file size and the
//!   page-rounded memory size of `__LINKEDIT`. Every other byte stays.
//! - **The signature file** of a program is that blob, exactly as codesign wrote it
//!   ([`extract`]). [`extract`] checks that [`attach`] of it to the unsigned program gives the
//!   signed program byte for byte, so a signature file is only written when it reproduces.
//! - **Attaching** ([`attach`]) cuts the unsigned program at the blob's offset, appends the
//!   signature file and sets the three numbers. The code directory inside the blob hashes the
//!   header with the new numbers, so the result verifies exactly as the signed program does.
//!
//! The archive ([`write_archive`], [`read_archive`]) holds `SIGNATURES` (one line per program:
//! the unsigned program's sha256, the signed program's sha256 and its release path), the
//! signature files `<path>.sig` and a `README.md`, with fixed times, owners and order and gzip
//! without a time stamp. [`attach_tree`] attaches every signature of an archive to an unpacked
//! unsigned release and checks both sha256 values of every program.

use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_SEGMENT_64: u32 = 0x19;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;

/// Where the numbers codesign changes are in a Mach-O program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// file offset of `LC_CODE_SIGNATURE`'s `dataoff` (its `datasize` follows)
    pub sig_cmd: usize,
    /// the signature blob's offset and size
    pub dataoff: usize,
    pub datasize: usize,
    /// file offset of the `__LINKEDIT` segment command
    pub linkedit_cmd: usize,
    pub linkedit_fileoff: u64,
    /// the page size the segment's memory size is rounded to (16 KiB on arm64)
    pub page: u64,
}

fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes(s.try_into().expect("4 bytes")))
        .ok_or_else(|| format!("truncated at {o}"))
}

fn u64_at(b: &[u8], o: usize) -> Result<u64, String> {
    b.get(o..o + 8)
        .map(|s| u64::from_le_bytes(s.try_into().expect("8 bytes")))
        .ok_or_else(|| format!("truncated at {o}"))
}

/// Read the layout of a thin 64-bit little-endian Mach-O program with a code signature.
pub fn layout(b: &[u8]) -> Result<Layout, String> {
    if u32_at(b, 0)? != MH_MAGIC_64 {
        return Err("not a 64-bit Mach-O program (a universal file is not supported)".into());
    }
    let cputype = u32_at(b, 4)?;
    let ncmds = u32_at(b, 16)? as usize;
    let mut o = 32;
    let (mut sig, mut linkedit) = (None, None);
    for _ in 0..ncmds {
        let cmd = u32_at(b, o)?;
        let size = u32_at(b, o + 4)? as usize;
        if size < 8 {
            return Err(format!("load command at {o} has size {size}"));
        }
        if cmd == LC_CODE_SIGNATURE {
            sig = Some((
                o + 8,
                u32_at(b, o + 8)? as usize,
                u32_at(b, o + 12)? as usize,
            ));
        }
        if cmd == LC_SEGMENT_64
            && b.get(o + 8..o + 24)
                .is_some_and(|n| n.starts_with(b"__LINKEDIT\0"))
        {
            linkedit = Some((o, u64_at(b, o + 40)?));
        }
        o += size;
    }
    let (sig_cmd, dataoff, datasize) = sig.ok_or("no LC_CODE_SIGNATURE")?;
    let (linkedit_cmd, linkedit_fileoff) = linkedit.ok_or("no __LINKEDIT segment")?;
    if dataoff < o || (dataoff as u64) < linkedit_fileoff || dataoff > b.len() {
        return Err(format!("the signature's offset {dataoff} is out of place"));
    }
    Ok(Layout {
        sig_cmd,
        dataoff,
        datasize,
        linkedit_cmd,
        linkedit_fileoff,
        page: if cputype == CPU_TYPE_ARM64 {
            0x4000
        } else {
            0x1000
        },
    })
}

/// The program `unsigned` with the signature blob `sig` in place of its own.
pub fn attach(unsigned: &[u8], sig: &[u8]) -> Result<Vec<u8>, String> {
    let l = layout(unsigned)?;
    let mut out = Vec::with_capacity(l.dataoff + sig.len());
    out.extend_from_slice(&unsigned[..l.dataoff]);
    out.extend_from_slice(sig);
    let size = u32::try_from(sig.len()).map_err(|_| "signature too large")?;
    out[l.sig_cmd + 4..l.sig_cmd + 8].copy_from_slice(&size.to_le_bytes());
    let filesize = out.len() as u64 - l.linkedit_fileoff;
    let vmsize = filesize.div_ceil(l.page) * l.page;
    out[l.linkedit_cmd + 32..l.linkedit_cmd + 40].copy_from_slice(&vmsize.to_le_bytes());
    out[l.linkedit_cmd + 48..l.linkedit_cmd + 56].copy_from_slice(&filesize.to_le_bytes());
    Ok(out)
}

/// The signature blob of `signed`, checked to turn `unsigned` into `signed` by [`attach`].
pub fn extract(unsigned: &[u8], signed: &[u8]) -> Result<Vec<u8>, String> {
    let (u, s) = (layout(unsigned)?, layout(signed)?);
    if u.dataoff != s.dataoff {
        return Err(format!(
            "the signature moved (offset {} unsigned, {} signed): not a re-signing of this program",
            u.dataoff, s.dataoff
        ));
    }
    if s.dataoff + s.datasize != signed.len() {
        return Err(format!(
            "the signed program has {} bytes after its signature",
            signed.len() as i64 - (s.dataoff + s.datasize) as i64
        ));
    }
    let sig = signed[s.dataoff..].to_vec();
    if attach(unsigned, &sig)? != signed {
        return Err(
            "attaching the signature to the unsigned program does not give the signed program"
                .into(),
        );
    }
    Ok(sig)
}

/// The hex sha256 of bytes.
pub fn sha256(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// One signed program of a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// release path, e.g. `bin/hydo-hvm`
    pub path: String,
    pub unsigned_sha256: String,
    pub signed_sha256: String,
    pub sig: Vec<u8>,
}

/// The Mach-O programs of `bin/` in a release tree, by release path.
pub fn programs(tree: &Path) -> Result<Vec<String>, String> {
    let dir = tree.join("bin");
    let mut v = Vec::new();
    for e in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let mut head = [0u8; 4];
        let n = fs::File::open(&p)
            .and_then(|mut f| f.read(&mut head))
            .map_err(|e| format!("{}: {e}", p.display()))?;
        if n == 4 && u32::from_le_bytes(head) == MH_MAGIC_64 {
            v.push(format!("bin/{}", e.file_name().to_string_lossy()));
        }
    }
    v.sort();
    Ok(v)
}

/// The signatures of every program of `signed` (a signed copy of the release tree `unsigned`).
pub fn extract_tree(unsigned: &Path, signed: &Path) -> Result<Vec<Signature>, String> {
    let paths = programs(signed)?;
    if paths != programs(unsigned)? {
        return Err("the two trees have different programs in bin/".into());
    }
    let mut v = Vec::new();
    for path in paths {
        let read = |t: &Path| fs::read(t.join(&path)).map_err(|e| format!("{path}: {e}"));
        let (u, s) = (read(unsigned)?, read(signed)?);
        if u == s {
            return Err(format!("{path} is not signed in {}", signed.display()));
        }
        let sig = extract(&u, &s).map_err(|e| format!("{path}: {e}"))?;
        v.push(Signature {
            path,
            unsigned_sha256: sha256(&u),
            signed_sha256: sha256(&s),
            sig,
        });
    }
    Ok(v)
}

/// Attach every signature to the unsigned release tree `tree`, in place. Each program must be the
/// unsigned program the signature was made for, and the result the signed program, by sha256.
pub fn attach_tree(tree: &Path, sigs: &[Signature]) -> Result<(), String> {
    for s in sigs {
        let p = tree.join(&s.path);
        let u = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let h = sha256(&u);
        if h == s.signed_sha256 {
            continue; // already attached
        }
        if h != s.unsigned_sha256 {
            return Err(format!(
                "{}: sha256 {h}, but the signature was made for the unsigned program {} (another build?)",
                s.path, s.unsigned_sha256
            ));
        }
        let out = attach(&u, &s.sig).map_err(|e| format!("{}: {e}", s.path))?;
        let h = sha256(&out);
        if h != s.signed_sha256 {
            return Err(format!(
                "{}: attaching gives sha256 {h}, not the signed program's {}",
                s.path, s.signed_sha256
            ));
        }
        let mode = fs::metadata(&p)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        let tmp = p.with_extension("sig-attach");
        fs::write(&tmp, &out).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

fn readme(top: &str) -> String {
    format!(
        "# {top}\n\n\
         The Developer ID signatures of the programs of the Mac release, apart from the programs\n\
         (docs/design/signing.md in hydo's repository). The release tarball is built unsigned and\n\
         reproducibly by `hydo/dist/mac/build-release.sh`; the published tarball is that build with\n\
         these signatures attached. To check it from source:\n\n\
         ```\n\
         sh hydo/dist/mac/build-release.sh <out>      # the unsigned tarball\n\
         sh hydo/dist/mac/attach-signatures.sh <out>/<the unsigned tarball> {top}.tar.gz <signed out>\n\
         ```\n\n\
         The second command writes the signed tarball; it has the published tarball's sha256.\n\n\
         `SIGNATURES` lists, for each program, the sha256 of the unsigned program, the sha256 of the\n\
         signed program and its path in the release; `<path>.sig` is the program's signature as\n\
         codesign wrote it (the data `LC_CODE_SIGNATURE` names).\n"
    )
}

/// Write the signatures archive `out` (its top directory is its file name without `.tar.gz`).
/// Returns its sha256.
pub fn write_archive(sigs: &[Signature], out: &Path) -> Result<String, String> {
    let name = out
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".tar.gz"))
        .ok_or("the archive's name must end in .tar.gz")?
        .to_string();
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    files.insert(format!("{name}/README.md"), readme(&name).into_bytes());
    let mut list = String::new();
    for s in sigs {
        list.push_str(&format!(
            "{}  {}  {}\n",
            s.unsigned_sha256, s.signed_sha256, s.path
        ));
        files.insert(format!("{name}/{}.sig", s.path), s.sig.clone());
    }
    files.insert(format!("{name}/SIGNATURES"), list.into_bytes());
    let mut dirs = std::collections::BTreeSet::new();
    for k in files.keys() {
        let mut d = k.as_str();
        while let Some(i) = d.rfind('/') {
            d = &d[..i];
            dirs.insert(format!("{d}/"));
        }
    }
    let mut all: Vec<(String, Option<&Vec<u8>>)> = dirs.into_iter().map(|d| (d, None)).collect();
    all.extend(files.iter().map(|(k, v)| (k.clone(), Some(v))));
    all.sort_by(|a, b| a.0.cmp(&b.0));
    let mut buf = Vec::new();
    {
        let gz = GzBuilder::new()
            .mtime(0)
            .write(&mut buf, Compression::new(9));
        let mut tb = tar::Builder::new(gz);
        for (n, data) in all {
            let mut h = tar::Header::new_gnu();
            h.set_uid(0);
            h.set_gid(0);
            h.set_mtime(crate::sources::MTIME);
            let _ = h.set_username("root");
            let _ = h.set_groupname("root");
            match data {
                None => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(0o755);
                    h.set_size(0);
                    tb.append_data(&mut h, &n, std::io::empty())
                }
                Some(d) => {
                    h.set_entry_type(tar::EntryType::Regular);
                    h.set_mode(0o644);
                    h.set_size(d.len() as u64);
                    tb.append_data(&mut h, &n, d.as_slice())
                }
            }
            .map_err(|e| format!("{n}: {e}"))?;
        }
        let gz = tb.into_inner().map_err(|e| format!("tar: {e}"))?;
        gz.finish().map_err(|e| format!("gzip: {e}"))?;
    }
    let mut f = fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    f.write_all(&buf)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(sha256(&buf))
}

/// Read a signatures archive written by [`write_archive`].
pub fn read_archive(path: &Path) -> Result<Vec<Signature>, String> {
    let f = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut ar = tar::Archive::new(GzDecoder::new(f));
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for e in ar
        .entries()
        .map_err(|e| format!("{}: {e}", path.display()))?
    {
        let mut e = e.map_err(|e| format!("{}: {e}", path.display()))?;
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let name = e
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        let rel = name
            .split_once('/')
            .map(|(_, r)| r.to_string())
            .unwrap_or(name);
        let mut d = Vec::new();
        e.read_to_end(&mut d).map_err(|e| e.to_string())?;
        files.insert(rel, d);
    }
    let list = files
        .get("SIGNATURES")
        .ok_or("no SIGNATURES in the archive")?;
    let mut v = Vec::new();
    for line in String::from_utf8_lossy(list).lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [u, s, p] = f.as_slice() else {
            return Err(format!("SIGNATURES: cannot read the line {line:?}"));
        };
        if p.contains("..") || p.starts_with('/') {
            return Err(format!("SIGNATURES: {p} is not a release path"));
        }
        let sig = files
            .get(&format!("{p}.sig"))
            .ok_or_else(|| format!("no {p}.sig in the archive"))?
            .clone();
        v.push(Signature {
            path: p.to_string(),
            unsigned_sha256: u.to_string(),
            signed_sha256: s.to_string(),
            sig,
        });
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal arm64 Mach-O: the header, a __LINKEDIT segment command and LC_CODE_SIGNATURE,
    /// a body, and a signature blob of `sig` bytes at the end.
    fn program(body: usize, sig: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(&MH_MAGIC_64.to_le_bytes());
        b[4..8].copy_from_slice(&CPU_TYPE_ARM64.to_le_bytes());
        b[16..20].copy_from_slice(&2u32.to_le_bytes());
        b[20..24].copy_from_slice(&(72u32 + 16).to_le_bytes());
        let mut seg = vec![0u8; 72];
        seg[0..4].copy_from_slice(&LC_SEGMENT_64.to_le_bytes());
        seg[4..8].copy_from_slice(&72u32.to_le_bytes());
        seg[8..18].copy_from_slice(b"__LINKEDIT");
        let fileoff = 128u64;
        seg[40..48].copy_from_slice(&fileoff.to_le_bytes());
        b.extend_from_slice(&seg);
        let dataoff = 128 + body;
        let mut cs = vec![0u8; 16];
        cs[0..4].copy_from_slice(&LC_CODE_SIGNATURE.to_le_bytes());
        cs[4..8].copy_from_slice(&16u32.to_le_bytes());
        cs[8..12].copy_from_slice(&(dataoff as u32).to_le_bytes());
        b.extend_from_slice(&cs);
        b.resize(dataoff, 0xab);
        b.extend_from_slice(sig);
        // what the linker or codesign writes into the three numbers
        let n = b.len();
        b[32 + 72 + 12..32 + 72 + 16].copy_from_slice(&(sig.len() as u32).to_le_bytes());
        let filesize = n as u64 - fileoff;
        b[32 + 32..32 + 40].copy_from_slice(&(filesize.div_ceil(0x4000) * 0x4000).to_le_bytes());
        b[32 + 48..32 + 56].copy_from_slice(&filesize.to_le_bytes());
        b
    }

    #[test]
    fn a_signature_moves_between_builds_byte_for_byte() {
        let unsigned = program(1000, &[1u8; 300]);
        let signed = program(1000, &[7u8; 20_000]);
        let sig = extract(&unsigned, &signed).unwrap();
        assert_eq!(sig, vec![7u8; 20_000]);
        assert_eq!(attach(&unsigned, &sig).unwrap(), signed);
        // a smaller signature shrinks the segment again
        let small = program(1000, &[9u8; 40]);
        assert_eq!(attach(&signed, &[9u8; 40]).unwrap(), small);
    }

    #[test]
    fn a_signature_of_another_program_is_refused() {
        let unsigned = program(1000, &[1u8; 300]);
        let other = program(2000, &[7u8; 2000]);
        assert!(extract(&unsigned, &other).unwrap_err().contains("moved"));
        let mut tail = program(1000, &[7u8; 2000]);
        tail.push(0);
        assert!(extract(&unsigned, &tail)
            .unwrap_err()
            .contains("after its signature"));
        assert!(layout(b"\x7fELF....").is_err());
    }

    #[test]
    fn the_archive_round_trips_and_attaches_to_a_tree() {
        let d = std::env::temp_dir().join(format!("hydo-sigs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for t in ["u/bin", "s/bin", "x/bin"] {
            fs::create_dir_all(d.join(t)).unwrap();
        }
        for (name, body) in [("hydo", 500), ("hydo-hvm", 900)] {
            fs::write(d.join("u/bin").join(name), program(body, &[1u8; 64])).unwrap();
            fs::write(d.join("x/bin").join(name), program(body, &[1u8; 64])).unwrap();
            fs::write(
                d.join("s/bin").join(name),
                program(body, &[name.len() as u8; 9000]),
            )
            .unwrap();
        }
        fs::write(d.join("u/bin/hydo-grpc-gen"), b"#!/bin/sh\n").unwrap();
        fs::write(d.join("s/bin/hydo-grpc-gen"), b"#!/bin/sh\n").unwrap();
        fs::write(d.join("x/bin/hydo-grpc-gen"), b"#!/bin/sh\n").unwrap();
        let sigs = extract_tree(&d.join("u"), &d.join("s")).unwrap();
        assert_eq!(sigs.len(), 2);
        let a = d.join("hydo-mac-9.9.9-signatures.tar.gz");
        let h1 = write_archive(&sigs, &a).unwrap();
        assert_eq!(
            h1,
            write_archive(&sigs, &a).unwrap(),
            "the same bytes twice"
        );
        let back = read_archive(&a).unwrap();
        assert_eq!(back, sigs);
        attach_tree(&d.join("x"), &back).unwrap();
        for name in ["hydo", "hydo-hvm"] {
            assert_eq!(
                fs::read(d.join("x/bin").join(name)).unwrap(),
                fs::read(d.join("s/bin").join(name)).unwrap()
            );
        }
        // attached already: nothing to do; another build: refused
        attach_tree(&d.join("x"), &back).unwrap();
        fs::write(d.join("x/bin/hydo"), program(501, &[1u8; 64])).unwrap();
        assert!(attach_tree(&d.join("x"), &back)
            .unwrap_err()
            .contains("another build"));
        let _ = fs::remove_dir_all(&d);
    }
}
