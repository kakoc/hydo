//! Milestone 313 (docs/design/block-faults.md): `G=<svc>@<at>[+<down>]:bad|rot,...`, a latent
//! sector error under a file of a service's storage. The token names a file and a range of its
//! bytes, as the other `G=` kinds do; the disk knows only sectors. So at the fault's stop the
//! controller finds the sectors itself, with the agent's existing commands and the image's
//! busybox, and no new agent (a new agent would move the agent pin and every image's bytes):
//!
//! 1. `sync`, so that the file's blocks are allocated and its inode is current;
//! 2. busybox `stat` gives the file's inode and device, and `/sys/dev/block/<maj>:<min>` the
//!    partition's name and its first sector;
//! 3. the agent's `read` of the partition's device gives the ext4 superblock, the group
//!    descriptor, the inode and its extent tree ([`map`], a pure function over a reader). The
//!    reads go through the guest's page cache of the device, where ext4 keeps its metadata, so
//!    they see the current tree also before the journal is written back;
//! 4. the engine's `DISK lse=<sector>+<count>,...[:rot]` makes those sectors bad from this
//!    stop on;
//! 5. `drop_caches` drops the clean pages of every file, so the next read of the range goes to
//!    the disk, as after a reboot.
//!
//! What it cannot map it logs and leaves alone: a file system other than ext4, a file without
//! extents or with its data inline, a range in a hole or in an unwritten extent (Linux reads no
//! sector there), and a file on another disk than the data disk (`/dev/vda`). The schedule is
//! refused on hvx before any VM (`hydo_engine::refusal`).

use super::{bb, call, log, run, sh, xassert, Ctx, F};
use crate::flow::Step;
use hydo_agent_proto::msg::{Command, Read, Reply};

/// The sectors of the data disk under bytes [off, off+len) of the file with inode `ino` on an
/// ext4 file system, relative to the start of its partition, as (first sector, count) runs in
/// the file's order, merged where they touch. `read(offset, len)` reads the partition's bytes
/// (None: it could not). Err(why) when the file cannot be mapped.
pub(crate) fn map<E>(
    read: &mut dyn FnMut(u64, usize) -> Result<Option<Vec<u8>>, E>,
    ino: u64,
    off: u64,
    len: u64,
) -> Result<Result<Vec<(u64, u64)>, String>, E> {
    macro_rules! rd {
        ($o:expr, $n:expr, $what:expr) => {
            match read($o, $n)? {
                Some(b) if b.len() >= $n => b,
                _ => return Ok(Err(format!("cannot read the {}", $what))),
            }
        };
    }
    let u16_at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u64;
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as u64;
    if ino == 0 || len == 0 {
        return Ok(Err("no inode or an empty range".into()));
    }
    let sb = rd!(1024, 1024, "superblock");
    if u16_at(&sb, 0x38) != 0xEF53 {
        return Ok(Err("not an ext4 file system".into()));
    }
    let log_bs = u32_at(&sb, 0x18);
    if log_bs > 6 {
        return Ok(Err("a block size beyond 64 KiB".into()));
    }
    let bs = 1024u64 << log_bs;
    let first_data = u32_at(&sb, 0x14);
    let ipg = u32_at(&sb, 0x28);
    let rev = u32_at(&sb, 0x4C);
    let isize = if rev >= 1 { u16_at(&sb, 0x58) } else { 128 };
    let incompat = u32_at(&sb, 0x60);
    let wide = incompat & 0x80 != 0;
    let dsize = if wide { u16_at(&sb, 0xFE).max(32) } else { 32 };
    if ipg == 0 || isize < 128 {
        return Ok(Err("a superblock without inodes".into()));
    }
    let (group, index) = ((ino - 1) / ipg, (ino - 1) % ipg);
    let desc = rd!(
        (first_data + 1) * bs + group * dsize,
        dsize as usize,
        "group descriptor"
    );
    let mut table = u32_at(&desc, 0x08);
    if wide && dsize >= 64 {
        table |= u32_at(&desc, 0x28) << 32;
    }
    let inode = rd!(table * bs + index * isize, 128, "inode");
    let flags = u32_at(&inode, 0x20);
    if flags & 0x1000_0000 != 0 {
        return Ok(Err("the file's data is inline in its inode".into()));
    }
    if flags & 0x8_0000 == 0 {
        return Ok(Err(
            "the file has no extents (an ext2 or ext3 block map)".into()
        ));
    }
    let root = inode[0x28..0x28 + 60].to_vec();
    let mut out: Vec<(u64, u64)> = Vec::new();
    let spb = bs / 512;
    for lblk in off / bs..(off + len).div_ceil(bs) {
        // walk from the root to the leaf that holds lblk
        let mut node = root.clone();
        let phys = loop {
            if node.len() < 12 || u16_at(&node, 0) != 0xF30A {
                return Ok(Err("a damaged extent tree".into()));
            }
            let (n, depth) = (u16_at(&node, 2) as usize, u16_at(&node, 6));
            if 12 + n * 12 > node.len() {
                return Ok(Err("a damaged extent tree".into()));
            }
            if depth == 0 {
                let mut hit = None;
                for i in 0..n {
                    let e = 12 + i * 12;
                    let first = u32_at(&node, e);
                    let raw = u16_at(&node, e + 4);
                    let (count, unwritten) = if raw > 32768 {
                        (raw - 32768, true)
                    } else {
                        (raw, false)
                    };
                    let start = u32_at(&node, e + 8) | (u16_at(&node, e + 6) << 32);
                    if lblk >= first && lblk < first + count {
                        hit = Some((start + (lblk - first), unwritten));
                        break;
                    }
                }
                break hit;
            }
            let mut child = None;
            for i in 0..n {
                let e = 12 + i * 12;
                if u32_at(&node, e) <= lblk {
                    child = Some(u32_at(&node, e + 4) | (u16_at(&node, e + 8) << 32));
                }
            }
            let Some(c) = child else {
                break None;
            };
            node = rd!(c * bs, bs as usize, "extent block");
        };
        let Some((pblk, unwritten)) = phys else {
            return Ok(Err(format!(
                "block {lblk} of the file is a hole (Linux reads no sector there)"
            )));
        };
        if unwritten {
            return Ok(Err(format!(
                "block {lblk} of the file is not written yet (Linux reads no sector there)"
            )));
        }
        // the part of this block inside [off, off+len), in whole sectors
        let a = off.max(lblk * bs) - lblk * bs;
        let b = (off + len).min((lblk + 1) * bs) - lblk * bs;
        let first = pblk * spb + a / 512;
        let count = b.div_ceil(512) - a / 512;
        match out.last_mut() {
            Some(l) if l.0 + l.1 == first => l.1 += count,
            _ => out.push((first, count)),
        }
    }
    Ok(Ok(out))
}

/// The `lse=` token of sector runs (relative to the partition at `start`), with runs longer
/// than `hydo_schedule::LSE_MAX_COUNT` split; None when it would pass the token's 16 ranges.
pub(crate) fn token(runs: &[(u64, u64)], start: u64, rot: bool) -> Option<String> {
    let mut parts = Vec::new();
    for (a, n) in runs {
        let (mut a, mut n) = (start + a, *n);
        while n > 0 {
            let k = n.min(hydo_schedule::LSE_MAX_COUNT);
            parts.push(format!("{a}+{k}"));
            a += k;
            n -= k;
        }
    }
    if parts.is_empty() || parts.len() > hydo_schedule::LSE_MAX_RANGES {
        return None;
    }
    Some(format!(
        "lse={}{}",
        parts.join(","),
        if rot { ":rot" } else { "" }
    ))
}

fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `len` bytes of the device at `off`, in reads of at most 2,048 bytes; None when one fails.
fn read_dev(fx: &mut F<'_>, dev: &str, off: u64, len: usize) -> Step<Option<Vec<u8>>> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let n = (len - out.len()).min(2048);
        let r = call(
            fx,
            Command::Read(Read {
                path: dev.into(),
                offset: Some(off + out.len() as u64),
                max: n as u32,
                hex: Some(true),
            }),
        )?;
        let Ok(Reply::Read(r)) = r else {
            return Ok(None);
        };
        let d = r.data.trim();
        if d.len() != 2 * n {
            return Ok(None);
        }
        for i in 0..n {
            let Ok(b) = u8::from_str_radix(&d[2 * i..2 * i + 2], 16) else {
                return Ok(None);
            };
            out.push(b);
        }
    }
    Ok(Some(out))
}

/// The bad sectors under bytes [off, off+len) of `path` (of `svc`'s storage), from this stop
/// on: the steps of the module's documentation. Logs what it did, or why it did nothing.
pub(super) fn arm(
    fx: &mut F<'_>,
    ctx: &Ctx,
    svc: &str,
    path: &str,
    (off, len, rot): (u64, u64, bool),
) -> Step<String> {
    let b = bb(ctx);
    let probe = format!(
        "{b} sync; i=$({b} stat -c %i {p}) && d=$({b} stat -c %d {p}) && \
         M=$(( (d >> 8) & 4095 )) && m=$(( (d & 255) | ((d >> 12) & 1048320) )) && \
         n=$({b} sed -n 's/^DEVNAME=//p' /sys/dev/block/$M:$m/uevent) && \
         s=$({b} cat /sys/dev/block/$M:$m/start 2>/dev/null || {b} printf 0) && \
         {b} printf '%s %s %s\\n' \"$i\" \"$n\" \"$s\"",
        p = q(path)
    );
    let r = run(fx, sh(ctx, probe), Vec::new(), Some(256))?;
    let words: Vec<&str> = r.out.split_whitespace().collect();
    let parsed = match words.as_slice() {
        [i, n, s] if r.status == 0 => match (i.parse::<u64>(), s.parse::<u64>()) {
            (Ok(i), Ok(s)) => Some((i, n.to_string(), s)),
            _ => None,
        },
        _ => None,
    };
    let Some((ino, name, start)) = parsed else {
        return Ok(format!(
            "cannot find the file's inode and device ({})",
            r.out.trim()
        ));
    };
    if !name.starts_with("vda") {
        return Ok(format!(
            "the file is on /dev/{name}, and the block faults act on the data disk /dev/vda only"
        ));
    }
    let dev = format!("/dev/{name}");
    let mut rd = |o: u64, n: usize| read_dev(fx, &dev, o, n);
    let runs = match map(&mut rd, ino, off, len)? {
        Ok(r) => r,
        Err(why) => return Ok(why),
    };
    let Some(tok) = token(&runs, start, rot) else {
        return Ok(format!(
            "the range spans more than {} pieces of the disk",
            hydo_schedule::LSE_MAX_RANGES
        ));
    };
    fx.engine(crate::control::EngineAction::BadSectors(tok.clone()));
    // the next read of the range goes to the disk
    run(
        fx,
        sh(
            ctx,
            format!("{b} sync; {b} printf 1 > /proc/sys/vm/drop_caches"),
        ),
        Vec::new(),
        None,
    )?;
    log(
        fx,
        format!(
            "BAD SECTORS under {svc}'s {path} bytes {off}..{}: {tok} ({dev}, inode {ino}, \
             partition from sector {start}){}",
            off + len,
            if rot {
                ", reads return damaged bytes"
            } else {
                ""
            }
        ),
    );
    let (t, id, msg) = super::storage::CORRUPT_DECL;
    xassert(fx, t, id, msg);
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny ext4 image by hand: 4 KiB blocks, one group, inode 12 with two extents (blocks 0
    /// and 1 at 100, block 2 at 300) and an unwritten one (block 3 at 400).
    fn image() -> Vec<u8> {
        let bs = 4096usize;
        let mut d = vec![0u8; 512 * bs];
        let put16 =
            |d: &mut Vec<u8>, o: usize, v: u16| d[o..o + 2].copy_from_slice(&v.to_le_bytes());
        let put32 =
            |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        // superblock at 1024
        put32(&mut d, 1024 + 0x14, 0); // first data block (4 KiB blocks)
        put32(&mut d, 1024 + 0x18, 2); // log block size: 4096
        put32(&mut d, 1024 + 0x28, 64); // inodes per group
        put16(&mut d, 1024 + 0x38, 0xEF53);
        put32(&mut d, 1024 + 0x4C, 1);
        put16(&mut d, 1024 + 0x58, 256);
        // group descriptor in block 1: inode table at block 10
        put32(&mut d, bs + 0x08, 10);
        // inode 12: index 11 of the table
        let ino = 10 * bs + 11 * 256;
        put32(&mut d, ino + 0x20, 0x8_0000);
        let r = ino + 0x28;
        put16(&mut d, r, 0xF30A);
        put16(&mut d, r + 2, 3);
        put16(&mut d, r + 4, 4);
        put16(&mut d, r + 6, 0);
        for (i, (first, count, start)) in [(0u32, 2u16, 100u32), (2, 1, 300), (3, 32769, 400)]
            .iter()
            .enumerate()
        {
            let e = r + 12 + i * 12;
            put32(&mut d, e, *first);
            put16(&mut d, e + 4, *count);
            put32(&mut d, e + 8, *start);
        }
        d
    }

    fn map_img(d: &[u8], ino: u64, off: u64, len: u64) -> Result<Vec<(u64, u64)>, String> {
        let mut rd = |o: u64, n: usize| -> Result<Option<Vec<u8>>, ()> {
            Ok(d.get(o as usize..o as usize + n).map(|b| b.to_vec()))
        };
        map(&mut rd, ino, off, len).unwrap()
    }

    #[test]
    fn a_file_range_maps_to_sectors() {
        let d = image();
        // one 4 KiB block at the start: block 100, sectors 800..808
        assert_eq!(map_img(&d, 12, 0, 4096), Ok(vec![(800, 8)]));
        // two blocks that touch on the disk merge
        assert_eq!(map_img(&d, 12, 0, 8192), Ok(vec![(800, 16)]));
        // a part of a block: whole sectors around it
        assert_eq!(map_img(&d, 12, 4096 + 100, 600), Ok(vec![(808, 2)]));
        // across two extents
        assert_eq!(map_img(&d, 12, 4096, 8192), Ok(vec![(808, 8), (2400, 8)]));
        assert!(map_img(&d, 12, 3 * 4096, 10)
            .unwrap_err()
            .contains("not written"));
        assert!(map_img(&d, 12, 9 * 4096, 10).unwrap_err().contains("hole"));
        assert!(map_img(&d, 13, 0, 10).unwrap_err().contains("no extents"));
        let mut bad = d.clone();
        bad[1024 + 0x38] = 0;
        assert!(map_img(&bad, 12, 0, 10)
            .unwrap_err()
            .contains("not an ext4"));
    }

    #[test]
    fn the_token_names_whole_disk_sectors() {
        assert_eq!(
            token(&[(800, 8), (2400, 8)], 2048, false).as_deref(),
            Some("lse=2848+8,4448+8")
        );
        assert_eq!(
            token(&[(0, 5000)], 0, true).as_deref(),
            Some("lse=0+2048,2048+2048,4096+904:rot")
        );
        let many: Vec<(u64, u64)> = (0..17).map(|i| (i * 10, 1)).collect();
        assert_eq!(token(&many, 0, false), None);
        assert!(hydo_schedule::Schedule::parse(&format!(
            "disk=none {} | run",
            token(&[(800, 8), (2400, 8)], 2048, true).unwrap()
        ))
        .is_ok());
    }
}
