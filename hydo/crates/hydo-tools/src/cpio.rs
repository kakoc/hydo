//! A gzip'd newc cpio (as hydo-build writes arm64 images) unpacked into a directory without root
//! (cpio_extract.py): regular files, directories and symlinks; device nodes are skipped.

use std::io::Read;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;

/// One archive member: name, mode, body.
pub type Entry = (String, u32, Vec<u8>);

fn hexfield(b: &[u8]) -> Result<u32, String> {
    let s =
        std::str::from_utf8(b).map_err(|_| "cpio: a header field that is not hex".to_string())?;
    u32::from_str_radix(s, 16).map_err(|_| format!("cpio: bad header field {s:?}"))
}

/// The members of a gzip'd newc archive up to its trailer (cpio_extract.py `entries`).
pub fn entries(path: &str) -> Result<Vec<Entry>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut data = Vec::new();
    flate2::read::MultiGzDecoder::new(f)
        .read_to_end(&mut data)
        .map_err(|e| format!("{path}: {e}"))?;
    let mut out = Vec::new();
    let mut i = 0usize;
    while data.get(i..i + 6) == Some(&b"070701"[..]) {
        let mut fl = [0u32; 13];
        for (k, x) in fl.iter_mut().enumerate() {
            let o = i + 6 + 8 * k;
            *x = hexfield(data.get(o..o + 8).ok_or("cpio: truncated header")?)?;
        }
        let (mode, fsz, nsz) = (fl[1], fl[6] as usize, fl[11] as usize);
        let name_b = data
            .get(i + 110..i + 110 + nsz.saturating_sub(1))
            .ok_or("cpio: truncated name")?;
        let name = String::from_utf8(name_b.to_vec())
            .map_err(|_| "cpio: a name that is not UTF-8".to_string())?;
        let o = (i + 110 + nsz + 3) & !3;
        if name == "TRAILER!!!" {
            return Ok(out);
        }
        let body = data
            .get(o..(o + fsz).min(data.len()))
            .unwrap_or(&[])
            .to_vec();
        out.push((name, mode, body));
        i = (o + fsz + 3) & !3;
    }
    Ok(out)
}

/// cpio_extract.py `extract`.
pub fn extract(path: &str, out: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    for (name, mode, body) in entries(path)? {
        let dst = crate::pyjoin(out, &name);
        let parent = hydo_manifest::py::dirname(&dst);
        let mkparent = || -> Result<(), String> {
            if !parent.is_empty() {
                std::fs::create_dir_all(&parent).map_err(|e| format!("{parent}: {e}"))?;
            }
            Ok(())
        };
        match mode & S_IFMT {
            S_IFDIR => std::fs::create_dir_all(&dst).map_err(|e| format!("{dst}: {e}"))?,
            S_IFLNK => {
                mkparent()?;
                if crate::nar::lexists(&dst) {
                    std::fs::remove_file(&dst).map_err(|e| format!("{dst}: {e}"))?;
                }
                let t = String::from_utf8(body)
                    .map_err(|_| format!("{dst}: a link target that is not UTF-8"))?;
                std::os::unix::fs::symlink(t, &dst).map_err(|e| format!("{dst}: {e}"))?;
            }
            S_IFREG => {
                mkparent()?;
                std::fs::write(&dst, &body).map_err(|e| format!("{dst}: {e}"))?;
                std::fs::set_permissions(
                    &dst,
                    std::fs::Permissions::from_mode(mode & 0o7777 | 0o200),
                )
                .map_err(|e| format!("{dst}: {e}"))?;
            }
            _ => {}
        }
    }
    Ok(())
}
