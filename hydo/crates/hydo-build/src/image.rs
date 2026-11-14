//! The filesystem images: mke2fs under the frozen clock (or, on the Mac release, inside a helper
//! VM), every time field zeroed afterwards, fixed UUIDs and hash seeds from the project name, and
//! the hand-made MBR. Port of hydo-build's `mkfs`, `zero_times`, `write_mbr`, `uuid_from`, plus
//! the two release patches that were in hydo_bridge.py (`own_by_root`, `mac_mkfs`).

use crate::fsx::{self, io_fail};
use crate::{Ctx, Fail, Io, Release};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};

pub const FAKE_TS: &str = "2020-09-13 16:26:40";
/// libfaketime reads FAKE_TS in the local time zone; every recorded image was built in UTC+4
/// (the Nix build machine's zone), where FAKE_TS is exactly FIXED_MTIME. TZ=UTC-4 is POSIX notation
/// for UTC+4, set on every faketime'd call, so a build in any zone gets the same bytes.
pub const FAKE_TZ: &str = "UTC-4";

/// `uuid_from(name, salt)`. The prefix `hydo-onboard:` is a fixed salt of the hash, not the
/// skill's name (the skill is `onboard` since Milestone 228): the UUIDs are inside every image,
/// so changing it would move every recorded fingerprint.
pub fn uuid_from(name: &str, salt: &str) -> String {
    let h = fsx::hex(&Sha256::digest(
        format!("hydo-onboard:{name}:{salt}").as_bytes(),
    ));
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The host tools mkfs needs.
pub struct MkfsTools {
    pub mke2fs: String,
    pub faketime: String,
}

fn run_checked(cmd: &mut Command, what: &str) -> Result<(), Fail> {
    let st = cmd
        .status()
        .map_err(|e| Fail::Exit(format!("hydo-build: cannot run {what}: {e}")))?;
    if !st.success() {
        return Err(Fail::Exit(format!(
            "hydo-build: {what} failed ({})",
            exit_text(&st)
        )));
    }
    Ok(())
}

pub fn exit_text(st: &std::process::ExitStatus) -> String {
    match st.code() {
        Some(c) => format!("exit status {c}"),
        None => "killed by a signal".into(),
    }
}

fn mke2fs_args(mib_out: &str, uuid: &str, seed: &str, journal: bool) -> Vec<String> {
    let feats = format!(
        "^metadata_csum,^64bit,^flex_bg,^resize_inode{}",
        if journal { ",has_journal" } else { "" }
    );
    let mut a: Vec<String> = ["-q", "-F", "-t", "ext4", "-b", "4096", "-O"]
        .iter()
        .map(|x| x.to_string())
        .collect();
    a.push(feats);
    if journal {
        a.push("-J".into());
        a.push("size=16".into());
    }
    a.push("-U".into());
    a.push(uuid.into());
    a.push("-E".into());
    a.push(format!(
        "hash_seed={seed},lazy_itable_init=0,lazy_journal_init=0"
    ));
    let _ = mib_out;
    a
}

fn truncate_new(out: &Path, mib: u64) -> Result<(), Fail> {
    let f = fs::File::create(out).map_err(|e| io_fail("cannot write", out, e))?;
    f.set_len(mib * 1024 * 1024)
        .map_err(|e| io_fail("cannot size", out, e))
}

/// `mkfs(T, out, mib, uuid, seed, src, journal)` with the release patches: on the Mac release the
/// filesystem is made inside a helper VM (no src allowed), on the Linux release every inode of a
/// populated root is owned by 0:0 afterwards (unless HYDO_KEEP_OWNERS).
#[allow(clippy::too_many_arguments)]
pub fn mkfs(
    ctx: &Ctx,
    t: &MkfsTools,
    out: &Path,
    mib: u64,
    uuid: &str,
    seed: &str,
    src: Option<&Path>,
    journal: bool,
) -> Result<(), Fail> {
    if let Release::Mac { root, engine } = &ctx.release {
        return mac_mkfs(ctx, root, engine, out, mib, uuid, seed, src, journal);
    }
    truncate_new(out, mib)?;
    let mut cmd = Command::new(&t.faketime);
    cmd.arg(FAKE_TS).arg(&t.mke2fs);
    cmd.args(mke2fs_args("", uuid, seed, journal));
    if let Some(s) = src {
        cmd.arg("-d").arg(s);
    }
    cmd.arg(out);
    cmd.env("TZ", FAKE_TZ)
        .env("SOURCE_DATE_EPOCH", "1600000000");
    run_checked(&mut cmd, "mke2fs")?;
    if let (Release::Linux { .. }, Some(s)) = (&ctx.release, src) {
        if !ctx.inputs.keep_owners {
            own_by_root(t, out, s)?;
        }
    }
    Ok(())
}

/// The Linux release's `own_by_root`: mke2fs -d records each source file's owner (the building
/// user's uid/gid); set every inode's owner to 0:0 with the bundled debugfs, under the frozen clock.
fn own_by_root(t: &MkfsTools, out: &Path, src: &Path) -> Result<(), Fail> {
    let mut paths = vec![src.to_path_buf()];
    for w in fsx::walk(src)? {
        let mut names: Vec<&std::ffi::OsString> = w.dirs.iter().chain(w.files.iter()).collect();
        names.sort();
        paths.extend(names.into_iter().map(|n| w.dir.join(n)));
    }
    let mut lines = Vec::new();
    for p in &paths {
        let st = fs::symlink_metadata(p).map_err(|e| io_fail("cannot stat", p, e))?;
        if st.uid() == 0 && st.gid() == 0 {
            continue;
        }
        let g = if p == src {
            "/".to_string()
        } else {
            format!("/{}", p.strip_prefix(src).unwrap_or(p).to_string_lossy())
        };
        if g.contains('"') || g.contains('\n') {
            return Err(Fail::Manifest(format!(
                "root: a file name with a quote or newline ({}) cannot be re-owned",
                hydo_manifest::py::repr_str(&g)
            )));
        }
        lines.push(format!("sif \"{g}\" uid 0"));
        lines.push(format!("sif \"{g}\" gid 0"));
    }
    if lines.is_empty() {
        return Ok(());
    }
    let dbg = Path::new(&t.mke2fs)
        .parent()
        .unwrap_or(Path::new(""))
        .join("debugfs");
    let cmdf = format!("{}.owners", out.display());
    fsx::write_text(Path::new(&cmdf), &(lines.join("\n") + "\n"))?;
    let r = Command::new(&t.faketime)
        .arg(FAKE_TS)
        .arg(&dbg)
        .arg("-w")
        .arg("-f")
        .arg(&cmdf)
        .arg(out)
        .env("TZ", FAKE_TZ)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    let _ = fs::remove_file(&cmdf);
    let r = r.map_err(|e| {
        Fail::Die(format!(
            "setting the root image's owners to root failed:\n  {e}"
        ))
    })?;
    let errs: Vec<String> = String::from_utf8_lossy(&r.stderr)
        .lines()
        .filter(|x| !x.trim().is_empty() && !x.starts_with("debugfs "))
        .map(str::to_string)
        .collect();
    if !r.status.success() || !errs.is_empty() {
        return Err(Fail::Die(format!(
            "setting the root image's owners to root failed:\n  {}",
            errs.iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n  ")
        )));
    }
    Ok(())
}

/// The Mac release's mkfs: the same mke2fs options, run by the static arm64 mke2fs inside a
/// helper VM (`<engine> hvm run`, the Mac engine's boot probe) on the raw file, instead of the
/// host's mke2fs under libfaketime. hydo-build zeroes every time field afterwards, so the image is
/// a pure function of the options.
#[allow(clippy::too_many_arguments)]
fn mac_mkfs(
    _ctx: &Ctx,
    rel: &Path,
    engine: &Path,
    out: &Path,
    mib: u64,
    uuid: &str,
    seed: &str,
    src: Option<&Path>,
    journal: bool,
) -> Result<(), Fail> {
    if src.is_some() {
        return Err(Fail::Die(
            "internal: populating a filesystem (mke2fs -d) is not supported on the Mac path".into(),
        ));
    }
    truncate_new(out, mib)?;
    let mut args = vec!["/bin/mke2fs".to_string()];
    args.extend(mke2fs_args("", uuid, seed, journal));
    args.push("/dev/vda".into());
    let guest = rel.join("guest");
    let kernel = rel.join("hv/hvm/explore/out/Image-explore");
    let tmp = crate::tempdir("hydo-mkfs-")?;
    let res = (|| -> Result<(), Fail> {
        let root = tmp.join("root");
        for d in ["bin", "dev", "proc", "sys", "etc"] {
            fs::create_dir_all(root.join(d)).map_err(|e| io_fail("cannot create", &root, e))?;
        }
        fsx::copy_file(&guest.join("busybox"), &root.join("bin/busybox"))?;
        fsx::copy_file(&guest.join("mke2fs"), &root.join("bin/mke2fs"))?;
        std::os::unix::fs::symlink("busybox", root.join("bin/sh"))
            .map_err(|e| io_fail("cannot link", &root, e))?;
        let quoted: Vec<String> = args.iter().map(|x| format!("'{x}'")).collect();
        let init = format!(
            "#!/bin/busybox sh\n\
             /bin/busybox mount -t devtmpfs none /dev\n\
             /bin/busybox mount -t proc none /proc\n\
             n=0; while [ ! -b /dev/vda ] && [ $n -lt 300 ]; do /bin/busybox usleep 10000; n=$((n+1)); done\n\
             if {}; then /bin/busybox sync; echo HYDO-MKFS-OK; else echo HYDO-MKFS-FAIL; fi\n\
             echo HYDO-MKFS-DONE\n\
             while :; do /bin/busybox sleep 3600; done\n",
            quoted.join(" ")
        );
        fsx::write_text(&root.join("init"), &init)?;
        for p in ["init", "bin/busybox", "bin/mke2fs"] {
            fsx::chmod(&root.join(p), 0o755)?;
        }
        let initrd = tmp.join("mkfs.cpio.gz");
        crate::cpio::write_cpio_gz(&root, &initrd, fsx::FIXED_MTIME as u64, None, false)?;
        let abs = crate::abspath(out);
        let p = Command::new(engine)
            .arg("hvm")
            .arg("run")
            .arg(&kernel)
            .arg(&initrd)
            .arg("HYDO-MKFS-DONE")
            .env("HVM_DISK", &abs)
            .env("HVM_DISK_OUT", &abs)
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| Fail::Die(format!("cannot run {}: {e}", engine.display())))?;
        let mut con = p.stdout.clone();
        con.extend_from_slice(&p.stderr);
        let con = String::from_utf8_lossy(&con).into_owned();
        if !con.contains("HYDO-MKFS-OK") {
            let tail: String = {
                let c: Vec<char> = con.chars().collect();
                c[c.len().saturating_sub(3000)..].iter().collect()
            };
            eprint!("{tail}");
            return Err(Fail::Die(
                "formatting the data disk in the helper VM failed (output above)".into(),
            ));
        }
        // mke2fs records whether `char` is signed on the machine that ran it (s_flags: 1 =
        // signed directory hash, x86; 2 = unsigned, arm64). Record the x86 value, as a Linux
        // build of the same manifest does; the filesystem is still empty, so nothing else changes.
        let mut f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(out)
            .map_err(|e| io_fail("cannot open", out, e))?;
        let mut b = [0u8; 4];
        f.seek(SeekFrom::Start(1024 + 0x160))
            .and_then(|_| f.read_exact(&mut b))
            .map_err(|e| io_fail("cannot read", out, e))?;
        let fl = (u32::from_le_bytes(b) & !0x3) | 0x1;
        f.seek(SeekFrom::Start(1024 + 0x160))
            .and_then(|_| f.write_all(&fl.to_le_bytes()))
            .map_err(|e| io_fail("cannot write", out, e))?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&tmp);
    res
}

fn u16_at(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// `zero_times(path, all_inode_times)`: zero every superblock time (primary and backups) and the
/// inode times mke2fs stamped (root: atime, ctime, dtime; data disk: all four), plus the extra
/// time fields of large inodes.
pub fn zero_times(path: &Path, all_inode_times: bool) -> Result<(), Fail> {
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| io_fail("cannot open", path, e))?;
    let len = f
        .metadata()
        .map_err(|e| io_fail("cannot stat", path, e))?
        .len() as usize;
    let mut sbuf = vec![0u8; 2048];
    f.read_exact(&mut sbuf)
        .map_err(|e| io_fail("cannot read", path, e))?;
    let sb = 1024usize;
    let inodes = u32_at(&sbuf, sb) as usize;
    let bs = 1024usize << u32_at(&sbuf, sb + 0x18);
    let ipg = u32_at(&sbuf, sb + 0x28) as usize;
    let isize = match u16_at(&sbuf, sb + 0x58) {
        0 => 128,
        x => x as usize,
    };
    let bpg = u32_at(&sbuf, sb + 0x20) as usize;
    let gdtb = if bs == 1024 { 2 } else { 1 };
    let ng = inodes.div_ceil(ipg);
    let put0 = |f: &mut fs::File, o: usize| -> Result<(), Fail> {
        f.seek(SeekFrom::Start(o as u64))
            .and_then(|_| f.write_all(&[0, 0, 0, 0]))
            .map_err(|e| io_fail("cannot write", path, e))
    };
    let read_at = |f: &mut fs::File, o: usize, n: usize| -> Result<Vec<u8>, Fail> {
        let mut b = vec![0u8; n];
        f.seek(SeekFrom::Start(o as u64))
            .and_then(|_| f.read_exact(&mut b))
            .map_err(|e| io_fail("cannot read", path, e))?;
        Ok(b)
    };
    for g in 0..ng {
        let o = if g == 0 {
            sb
        } else {
            g * bpg * bs + if bs == 1024 { 1024 } else { 0 }
        };
        if o + 0x3A <= len && u16_at(&read_at(&mut f, o + 0x38, 2)?, 0) == 0xEF53 {
            for t in [0x2C, 0x30, 0x40, 0x108] {
                put0(&mut f, o + t)?;
            }
        }
    }
    let fields: &[usize] = if all_inode_times {
        &[0x08, 0x0C, 0x10, 0x14]
    } else {
        &[0x08, 0x0C, 0x14]
    };
    for g in 0..ng {
        let itbl = u32_at(&read_at(&mut f, gdtb * bs + g * 32 + 0x08, 4)?, 0) as usize;
        let n = ipg.min(inodes.saturating_sub(g * ipg));
        if n == 0 {
            continue;
        }
        // the group's inode table in one read and one write (the same bytes as one write per field)
        let base = itbl * bs;
        let mut tbl = read_at(&mut f, base, n * isize)?;
        for i in 0..n {
            let io = i * isize;
            for o in fields {
                tbl[io + o..io + o + 4].fill(0);
            }
            if isize > 128 {
                for o in [0x84, 0x88, 0x8C, 0x90, 0x94] {
                    tbl[io + o..io + o + 4].fill(0);
                }
            }
        }
        f.seek(SeekFrom::Start(base as u64))
            .and_then(|_| f.write_all(&tbl))
            .map_err(|e| io_fail("cannot write", path, e))?;
    }
    f.flush().map_err(|e| io_fail("cannot write", path, e))
}

/// `write_mbr(img, parts)`.
pub fn write_mbr(img: &Path, parts: &[(u32, u32)]) -> Result<(), Fail> {
    let mut mbr = [0u8; 512];
    mbr[0x1B8..0x1BC].copy_from_slice(b"\x0b\xd0\xea\x1d");
    for (k, (lba, n)) in parts.iter().enumerate() {
        let off = 0x1BE + 16 * k;
        mbr[off] = 0;
        mbr[off + 1..off + 4].copy_from_slice(b"\xfe\xff\xff");
        mbr[off + 4] = 0x83;
        mbr[off + 5..off + 8].copy_from_slice(b"\xfe\xff\xff");
        mbr[off + 8..off + 12].copy_from_slice(&lba.to_le_bytes());
        mbr[off + 12..off + 16].copy_from_slice(&n.to_le_bytes());
    }
    mbr[0x1FE] = 0x55;
    mbr[0x1FF] = 0xAA;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .open(img)
        .map_err(|e| io_fail("cannot open", img, e))?;
    f.write_all(&mbr)
        .map_err(|e| io_fail("cannot write", img, e))
}

/// A disk of `total_sectors` with the MBR and each (image, start lba) copied in.
pub fn assemble_disk(
    disk: &Path,
    total_sectors: u64,
    parts: &[(u32, u32)],
    images: &[(&Path, u64)],
) -> Result<(), Fail> {
    {
        let f = fs::File::create(disk).map_err(|e| io_fail("cannot write", disk, e))?;
        f.set_len(total_sectors * 512)
            .map_err(|e| io_fail("cannot size", disk, e))?;
    }
    write_mbr(disk, parts)?;
    let mut fo = fs::OpenOptions::new()
        .write(true)
        .open(disk)
        .map_err(|e| io_fail("cannot open", disk, e))?;
    for (img, lba) in images {
        fo.seek(SeekFrom::Start(lba * 512))
            .map_err(|e| io_fail("cannot seek", disk, e))?;
        let mut fi = fs::File::open(img).map_err(|e| io_fail("cannot read", img, e))?;
        std::io::copy(&mut fi, &mut fo).map_err(|e| io_fail("cannot copy into", disk, e))?;
    }
    Ok(())
}

/// The largest disk an engine maps (hvx's page-array disk; the other engines follow it).
pub const DISK_MAX_BYTES: u64 = 4 << 30;

/// The services' own file systems (services[].storage.size_mib; docs/design/storage-faults.md):
/// the disk of [`assemble_disk`] with the root partition p1 as before, the data file system at
/// the start of p2, and an extended partition p3 after it that holds one logical partition per
/// file system, in the order given (Linux names them p5, p6, ...). Each logical partition is
/// preceded by its extended boot record and starts 1 MiB after it.
///
/// p2's entry spans the extended partition too (the data file system is smaller than p2, which
/// ext4 allows): the engines treat p2 as the data partition (hvx snapshots, restores,
/// fingerprints it and carries it into the recovery boot after a power cut), so the services'
/// own file systems must lie inside it to travel with it. Linux reads overlapping MBR entries
/// as they are written.
pub fn assemble_disk_volumes(
    disk: &Path,
    parts: [(u32, u32); 2],
    images: &[(&Path, u64)],
    vols: &[(&Path, u64)],
) -> Result<u64, Fail> {
    let ext_start = parts[1].0 as u64 + parts[1].1 as u64;
    let mut ebrs: Vec<(u64, u64)> = Vec::new(); // (EBR lba, logical partition sectors)
    let mut at = ext_start;
    for (_, mib) in vols {
        let n = mib * 2048;
        ebrs.push((at, n));
        at += 2048 + n;
    }
    let total = at;
    if total * 512 > DISK_MAX_BYTES {
        return Err(Fail::Exit(format!(
            "hydo-build: the disk with the services' own file systems is {} MiB, more than the {} MiB an engine maps; make data.size_mib or the storage size_mib values smaller",
            total / 2048,
            DISK_MAX_BYTES >> 20
        )));
    }
    let u32_of = |v: u64| -> Result<u32, Fail> {
        u32::try_from(v).map_err(|_| Fail::Exit("hydo-build: the disk is too large".into()))
    };
    {
        let f = fs::File::create(disk).map_err(|e| io_fail("cannot write", disk, e))?;
        f.set_len(total * 512)
            .map_err(|e| io_fail("cannot size", disk, e))?;
    }
    let ext = (u32_of(ext_start)?, u32_of(total - ext_start)?);
    let mut fo = fs::OpenOptions::new()
        .write(true)
        .open(disk)
        .map_err(|e| io_fail("cannot open", disk, e))?;
    let entry = |rec: &mut [u8; 512], k: usize, kind: u8, lba: u32, n: u32| {
        let off = 0x1BE + 16 * k;
        rec[off] = 0;
        rec[off + 1..off + 4].copy_from_slice(b"\xfe\xff\xff");
        rec[off + 4] = kind;
        rec[off + 5..off + 8].copy_from_slice(b"\xfe\xff\xff");
        rec[off + 8..off + 12].copy_from_slice(&lba.to_le_bytes());
        rec[off + 12..off + 16].copy_from_slice(&n.to_le_bytes());
    };
    let mut mbr = [0u8; 512];
    mbr[0x1B8..0x1BC].copy_from_slice(b"\x0b\xd0\xea\x1d");
    entry(&mut mbr, 0, 0x83, parts[0].0, parts[0].1);
    // p2 runs to the end of the disk, over the extended partition (the doc comment)
    entry(
        &mut mbr,
        1,
        0x83,
        parts[1].0,
        u32_of(total - parts[1].0 as u64)?,
    );
    entry(&mut mbr, 2, 0x05, ext.0, ext.1);
    mbr[0x1FE] = 0x55;
    mbr[0x1FF] = 0xAA;
    let mut put = |lba: u64, bytes: &[u8]| -> Result<(), Fail> {
        fo.seek(SeekFrom::Start(lba * 512))
            .map_err(|e| io_fail("cannot seek", disk, e))?;
        fo.write_all(bytes)
            .map_err(|e| io_fail("cannot write", disk, e))
    };
    put(0, &mbr)?;
    for (k, (lba, n)) in ebrs.iter().enumerate() {
        let mut ebr = [0u8; 512];
        // entry 1: the logical partition, relative to this record
        entry(&mut ebr, 0, 0x83, 2048, u32_of(*n)?);
        // entry 2: the next record, relative to the extended partition's start
        if let Some((next, nn)) = ebrs.get(k + 1) {
            entry(
                &mut ebr,
                1,
                0x05,
                u32_of(next - ext_start)?,
                u32_of(2048 + nn)?,
            );
        }
        ebr[0x1FE] = 0x55;
        ebr[0x1FF] = 0xAA;
        put(*lba, &ebr)?;
    }
    let mut copy = |img: &Path, lba: u64| -> Result<(), Fail> {
        fo.seek(SeekFrom::Start(lba * 512))
            .map_err(|e| io_fail("cannot seek", disk, e))?;
        let mut fi = fs::File::open(img).map_err(|e| io_fail("cannot read", img, e))?;
        std::io::copy(&mut fi, &mut fo)
            .map(|_| ())
            .map_err(|e| io_fail("cannot copy into", disk, e))
    };
    for (img, lba) in images {
        copy(img, *lba)?;
    }
    for ((img, _), (lba, _)) in vols.iter().zip(&ebrs) {
        copy(img, lba + 2048)?;
    }
    Ok(total)
}

/// The `sha256(<name>)=<hex>` lines a build ends with.
pub fn print_shas(io: &mut Io, paths: &[&Path]) -> Result<(), Fail> {
    for p in paths {
        let h = fsx::sha(p)?;
        crate::outln(io, &format!("  sha256({})={}", fsx::basename_str(p), h));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout of a disk with two own file systems: p1, p2 over everything after it, the
    /// extended p3 inside p2, and the chain of extended boot records with each file system's
    /// bytes 1 MiB after its record.
    #[test]
    fn volumes_lie_inside_p2() {
        let dir = std::env::temp_dir().join(format!("hydo-build-vol-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mk = |n: &str, mib: u64, b: u8| {
            let p = dir.join(n);
            fs::write(&p, vec![b; (mib << 20) as usize]).unwrap();
            p
        };
        let (root, data) = (mk("r", 1, 1), mk("d", 2, 2));
        let (v1, v2) = (mk("v1", 1, 3), mk("v2", 2, 4));
        let disk = dir.join("disk.img");
        let total = assemble_disk_volumes(
            &disk,
            [(2048, 2048), (4096, 4096)],
            &[(&root, 2048), (&data, 4096)],
            &[(&v1, 1), (&v2, 2)],
        )
        .unwrap();
        let img = fs::read(&disk).unwrap();
        assert_eq!(total, 8192 + 2048 + 2048 + 2048 + 4096);
        assert_eq!(img.len() as u64, total * 512);
        let le = |o: usize| u32::from_le_bytes(img[o..o + 4].try_into().unwrap()) as u64;
        let ent = |base: usize, k: usize| {
            let o = base + 0x1BE + 16 * k;
            (img[o + 4], le(o + 8), le(o + 12))
        };
        assert_eq!(ent(0, 0), (0x83, 2048, 2048));
        assert_eq!(ent(0, 1), (0x83, 4096, total - 4096), "p2 runs to the end");
        assert_eq!(ent(0, 2), (0x05, 8192, total - 8192), "p3 inside p2");
        assert_eq!(ent(0, 3), (0, 0, 0));
        // the first record (at the extended partition's start) and the next one
        let e1 = 8192 * 512;
        assert_eq!(&img[e1 + 0x1FE..e1 + 0x200], &[0x55, 0xAA]);
        assert_eq!(ent(e1, 0), (0x83, 2048, 2048));
        assert_eq!(ent(e1, 1), (0x05, 2048 + 2048, 2048 + 4096));
        let e2 = (8192 + 4096) * 512;
        assert_eq!(ent(e2, 0), (0x83, 2048, 4096));
        assert_eq!(ent(e2, 1), (0, 0, 0));
        assert_eq!(img[(8192 + 2048) * 512], 3);
        assert_eq!(img[(8192 + 4096 + 2048) * 512], 4);
        assert_eq!(img[4096 * 512], 2);
        let _ = fs::remove_dir_all(&dir);
    }
}
