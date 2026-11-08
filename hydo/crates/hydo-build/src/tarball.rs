//! A tar reader that sees an archive the way Python's `tarfile` does (hydo_oci.py read image
//! archives and layers with it): the same member names (GNU long names, PAX `path`/`linkpath`,
//! the ustar prefix, a directory's trailing slashes removed), the same numbers (octal or
//! base-256; PAX `uid`/`gid`/`size`), global PAX headers applied to every later member, data
//! skipped only for regular (or unknown) members, and the same end: a zero block, or a header
//! that does not check out after the first member.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

pub const REGTYPE: u8 = b'0';
pub const AREGTYPE: u8 = 0;
pub const LNKTYPE: u8 = b'1';
pub const SYMTYPE: u8 = b'2';
pub const CHRTYPE: u8 = b'3';
pub const BLKTYPE: u8 = b'4';
pub const DIRTYPE: u8 = b'5';
pub const FIFOTYPE: u8 = b'6';
pub const CONTTYPE: u8 = b'7';
const GNU_LONGNAME: u8 = b'L';
const GNU_LONGLINK: u8 = b'K';
const GNU_SPARSE: u8 = b'S';
const XHDTYPE: u8 = b'x';
const XGLTYPE: u8 = b'g';
const SOLARIS_XHDTYPE: u8 = b'X';

const SUPPORTED: &[u8] = &[
    REGTYPE,
    AREGTYPE,
    LNKTYPE,
    SYMTYPE,
    DIRTYPE,
    FIFOTYPE,
    CONTTYPE,
    CHRTYPE,
    BLKTYPE,
    GNU_LONGNAME,
    GNU_LONGLINK,
    GNU_SPARSE,
];

#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub kind: u8,
    pub mode: i64,
    pub uid: i64,
    pub gid: i64,
    pub size: u64,
    pub linkname: String,
    pub offset_data: u64,
}

impl Member {
    pub fn isreg(&self) -> bool {
        matches!(self.kind, REGTYPE | AREGTYPE | CONTTYPE | GNU_SPARSE)
    }
    pub fn isdir(&self) -> bool {
        self.kind == DIRTYPE
    }
    pub fn issym(&self) -> bool {
        self.kind == SYMTYPE
    }
    pub fn islnk(&self) -> bool {
        self.kind == LNKTYPE
    }
    /// What `extractfile` returns a file object for (regular members and unknown types).
    pub fn has_data(&self) -> bool {
        self.isreg() || !SUPPORTED.contains(&self.kind)
    }
}

/// `nts`: up to the first NUL. Names that are not UTF-8 are refused (Python could not have written
/// them into the filesystem image either).
fn nts(b: &[u8]) -> Result<String, String> {
    let e = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8(b[..e].to_vec()).map_err(|_| "a member name that is not UTF-8".to_string())
}

/// `nti`: octal text, or GNU base-256.
fn nti(b: &[u8]) -> Result<i64, String> {
    if b[0] == 0o200 || b[0] == 0o377 {
        let mut n: i128 = 0;
        for &c in &b[1..] {
            n = (n << 8) + c as i128;
        }
        if b[0] == 0o377 {
            n = -((1i128 << (8 * (b.len() - 1))) - n);
        }
        return Ok(n as i64);
    }
    let e = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let s = std::str::from_utf8(&b[..e]).map_err(|_| "invalid header".to_string())?;
    let t = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if t.is_empty() {
        return Ok(0);
    }
    i64::from_str_radix(t, 8).map_err(|_| "invalid header".to_string())
}

fn chksum_ok(buf: &[u8]) -> Result<bool, String> {
    let want = nti(&buf[148..156])?;
    let mut u: i64 = 256;
    let mut s: i64 = 256;
    for (i, &c) in buf.iter().enumerate() {
        if (148..156).contains(&i) {
            continue;
        }
        u += c as i64;
        s += c as i8 as i64;
    }
    Ok(want == u || want == s)
}

enum Header {
    Eof,
    Bad(String),
    Ok(Member, Vec<u8>),
}

fn block(n: u64) -> u64 {
    n.div_ceil(512) * 512
}

/// The members of a tar file, as `tarfile.open(path, "r:").getmembers()` lists them.
pub fn members(f: &mut File) -> Result<Vec<Member>, String> {
    let mut out = Vec::new();
    let mut offset: u64 = 0;
    let mut global: HashMap<String, String> = HashMap::new();
    loop {
        let first = out.is_empty();
        match read_member(f, &mut offset, &mut global) {
            Ok(Some(m)) => out.push(m),
            Ok(None) => break,
            Err(e) => {
                if first {
                    return Err(e);
                }
                break;
            }
        }
    }
    Ok(out)
}

fn read_header(f: &mut File, offset: u64) -> Result<Header, String> {
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 512];
    let mut got = 0;
    while got < 512 {
        let n = f.read(&mut buf[got..]).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        got += n;
    }
    if got == 0 {
        return Ok(Header::Eof);
    }
    if got < 512 {
        return Ok(Header::Bad("truncated header".into()));
    }
    if buf.iter().all(|&c| c == 0) {
        return Ok(Header::Eof);
    }
    match chksum_ok(&buf) {
        Ok(true) => {}
        Ok(false) => return Ok(Header::Bad("bad checksum".into())),
        Err(e) => return Ok(Header::Bad(e)),
    }
    let name = nts(&buf[0..100])?;
    let mode = nti(&buf[100..108]);
    let uid = nti(&buf[108..116]);
    let gid = nti(&buf[116..124]);
    let size = nti(&buf[124..136]);
    let (mode, uid, gid, size) = match (mode, uid, gid, size) {
        (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
        _ => return Ok(Header::Bad("invalid header".into())),
    };
    let mut kind = buf[156];
    let linkname = nts(&buf[157..257])?;
    let prefix = nts(&buf[345..500]).unwrap_or_default();
    let mut name = name;
    if kind == AREGTYPE && name.ends_with('/') {
        kind = DIRTYPE;
    }
    if kind == DIRTYPE {
        name = name.trim_end_matches('/').to_string();
    }
    if !prefix.is_empty() && ![GNU_LONGNAME, GNU_LONGLINK, GNU_SPARSE].contains(&kind) {
        name = format!("{prefix}/{name}");
    }
    Ok(Header::Ok(
        Member {
            name,
            kind,
            mode,
            uid,
            gid,
            size: size.max(0) as u64,
            linkname,
            offset_data: offset + 512,
        },
        buf,
    ))
}

fn read_data(f: &mut File, off: u64, n: u64) -> Result<Vec<u8>, String> {
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    let mut v = vec![0u8; n as usize];
    f.read_exact(&mut v).map_err(|e| e.to_string())?;
    Ok(v)
}

fn parse_pax(b: &[u8]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < b.len() {
        if b[pos] == 0 {
            break;
        }
        let sp = b[pos..]
            .iter()
            .position(|&c| c == b' ')
            .ok_or("invalid header")?;
        let len: usize = std::str::from_utf8(&b[pos..pos + sp])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or("invalid header")?;
        if len < 5 || pos + len > b.len() {
            return Err("invalid header".into());
        }
        let rec = &b[pos + sp + 1..pos + len - 1];
        let eq = rec
            .iter()
            .position(|&c| c == b'=')
            .ok_or("invalid header")?;
        let k = String::from_utf8_lossy(&rec[..eq]).into_owned();
        // Python's tarfile decodes a value that is not UTF-8 with its fallback encoding instead
        // of failing: a binary value (an extended attribute such as SCHILY.xattr.security.capability,
        // which Docker writes for setcap programs like newuidmap) is normal. An error here used
        // to end the layer silently at that member (members() ends an archive at the first bad
        // member after the first), so every later file of the layer was missing from the image.
        let v = String::from_utf8_lossy(&rec[eq + 1..]).into_owned();
        out.push((k, v));
        pos += len;
    }
    Ok(out)
}

fn apply_pax(m: &mut Member, pax: &HashMap<String, String>) {
    let num = |v: &str| -> i64 {
        v.parse::<i64>()
            .ok()
            .or_else(|| v.parse::<f64>().ok().map(|f| f as i64))
            .unwrap_or(0)
    };
    for (k, v) in pax {
        match k.as_str() {
            "path" => m.name = v.trim_end_matches('/').to_string(),
            "linkpath" => m.linkname = v.clone(),
            "size" => m.size = num(v).max(0) as u64,
            "uid" => m.uid = num(v),
            "gid" => m.gid = num(v),
            _ => {}
        }
    }
}

fn read_member(
    f: &mut File,
    offset: &mut u64,
    global: &mut HashMap<String, String>,
) -> Result<Option<Member>, String> {
    let (mut m, _) = match read_header(f, *offset)? {
        Header::Eof => return Ok(None),
        Header::Bad(e) => return Err(e),
        Header::Ok(m, b) => (m, b),
    };
    match m.kind {
        GNU_LONGNAME | GNU_LONGLINK => {
            let buf = read_data(f, m.offset_data, m.size)?;
            *offset = m.offset_data + block(m.size);
            let mut next = match read_member(f, offset, global)? {
                Some(n) => n,
                None => return Err("missing member after a GNU long name".into()),
            };
            if m.kind == GNU_LONGNAME {
                next.name = nts(&buf)?;
            } else {
                next.linkname = nts(&buf)?;
            }
            if next.isdir() {
                next.name = next.name.trim_end_matches('/').to_string();
            }
            Ok(Some(next))
        }
        XHDTYPE | XGLTYPE | SOLARIS_XHDTYPE => {
            let buf = read_data(f, m.offset_data, m.size)?;
            let recs = parse_pax(&buf)?;
            *offset = m.offset_data + block(m.size);
            let local: HashMap<String, String> = if m.kind == XGLTYPE {
                for (k, v) in recs {
                    global.insert(k, v);
                }
                global.clone()
            } else {
                let mut h = global.clone();
                for (k, v) in recs {
                    h.insert(k, v);
                }
                h
            };
            let hdr_start = *offset;
            let mut next = match read_member(f, offset, global)? {
                Some(n) => n,
                None => return Err("missing member after a PAX header".into()),
            };
            if m.kind != XGLTYPE {
                apply_pax(&mut next, &local);
                if local.contains_key("size") {
                    let _ = hdr_start;
                    let mut o = next.offset_data;
                    if next.has_data() {
                        o += block(next.size);
                    }
                    *offset = o;
                }
            }
            Ok(Some(next))
        }
        _ => {
            if m.kind == GNU_SPARSE {
                return Err("GNU sparse members are not supported".into());
            }
            *offset = m.offset_data;
            if m.has_data() {
                *offset += block(m.size);
            }
            if !global.is_empty() {
                apply_pax(&mut m, global);
            }
            Ok(Some(m))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// One 512-byte ustar header with a correct checksum.
    fn header(name: &str, kind: u8, size: usize) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        h
    }

    fn padded(mut v: Vec<u8>) -> Vec<u8> {
        v.resize(v.len().div_ceil(512) * 512, 0);
        v
    }

    /// A PAX header whose value is not UTF-8 (a file capability, as Docker writes for setcap
    /// programs) must not end the archive: every later member is still read.
    #[test]
    fn binary_pax_value_does_not_end_the_archive() {
        let mut rec = b"SCHILY.xattr.security.capability=".to_vec();
        rec.extend_from_slice(&[0x01, 0x00, 0x00, 0x02, 0xc0, 0xff, 0x00, 0x00, b'\n']);
        let len = rec.len() + 3; // the "NN " prefix: two digits and a space
        let mut pax = format!("{len} ").into_bytes();
        pax.extend_from_slice(&rec);
        assert_eq!(pax.len(), len);
        let mut tar = Vec::new();
        tar.extend(header("PaxHeaders/newuidmap", XHDTYPE, pax.len()));
        tar.extend(padded(pax));
        tar.extend(header("usr/bin/newuidmap", REGTYPE, 3));
        tar.extend(padded(b"abc".to_vec()));
        tar.extend(header("usr/lib64/ld-2.28.so", REGTYPE, 2));
        tar.extend(padded(b"ld".to_vec()));
        tar.extend(vec![0u8; 1024]);
        let path =
            std::env::temp_dir().join(format!("hydo-tarball-pax-{}.tar", std::process::id()));
        File::create(&path).unwrap().write_all(&tar).unwrap();
        let names: Vec<String> = members(&mut File::open(&path).unwrap())
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(names, ["usr/bin/newuidmap", "usr/lib64/ld-2.28.so"]);
    }
}
