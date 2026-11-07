//! The block device's disk image, copy-on-write: the image file (mapped read-only, or its bytes)
//! is the base that every copy shares, and only the 4 KiB blocks a run writes are held in memory,
//! in a two-level table of reference-counted blocks. A copy of the disk (a snapshot, the durable
//! image of a power cut, a checkpoint of the KVM engine's rewind mode) is the table's top level
//! with its references: a few thousand pointers for a disk of gigabytes, not its bytes. So memory
//! grows with what runs write, not with the image's size, and every copy reads the same bytes as
//! a full copy would (the hash and `HVM_DISK_OUT` see exactly the bytes the `Vec` of Milestone 147
//! held).
//!
//! The base: on Unix the image file is mapped read-only and private (its pages are the page cache,
//! shared with every other process that reads the image, and dropped by the kernel under memory
//! pressure); where the mapping fails, its bytes are read into memory once. The file must not be
//! rewritten while an engine uses it (an image is rebuilt with `hydo build` only between runs, as
//! before: a rewrite during a run was never supported, since the devices read the image once per
//! boot).

use std::sync::Arc;

/// The size of a block of the copy-on-write table.
pub const BLOCK: usize = 4096;
/// Blocks per second-level chunk (2 MiB of disk per chunk).
const CHUNK: usize = 512;

type Block = Arc<[u8; BLOCK]>;
type Chunk = Arc<Vec<Option<Block>>>;

/// The bytes every copy of the disk shares.
enum Base {
    #[cfg(unix)]
    Mapped {
        p: *const u8,
        len: usize,
    },
    Bytes(Vec<u8>),
}

// SAFETY: the mapping is read-only and owned by the Base for its whole life
unsafe impl Send for Base {}
// SAFETY: as above: nothing writes the mapped bytes
unsafe impl Sync for Base {}

impl Base {
    fn bytes(&self) -> &[u8] {
        match self {
            #[cfg(unix)]
            // SAFETY: a read-only mapping of `len` bytes for this value's life
            Base::Mapped { p, len } => unsafe { std::slice::from_raw_parts(*p, *len) },
            Base::Bytes(b) => b,
        }
    }
}

impl Drop for Base {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Base::Mapped { p, len } = self {
            // SAFETY: the mapping made in `Disk::open`; no slice of it outlives the Base
            unsafe {
                libc::munmap(*p as *mut libc::c_void, *len);
            }
        }
    }
}

/// A disk image with copy-on-write blocks (see the module documentation). `Clone` is cheap.
#[derive(Clone)]
pub struct Disk {
    base: Arc<Base>,
    len: usize,
    top: Vec<Option<Chunk>>,
}

impl std::fmt::Debug for Disk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Disk({} bytes, {} blocks written)",
            self.len,
            self.written()
        )
    }
}

impl Disk {
    fn with_base(base: Base) -> Disk {
        let len = base.bytes().len();
        let chunks = len.div_ceil(BLOCK * CHUNK);
        Disk {
            base: Arc::new(base),
            len,
            top: vec![None; chunks],
        }
    }

    /// The image `path`, mapped read-only (or read when it cannot be mapped).
    pub fn open(path: &str) -> std::io::Result<Disk> {
        let f = std::fs::File::open(path)?;
        let len = f.metadata()?.len() as usize;
        #[cfg(unix)]
        if len > 0 {
            use std::os::unix::io::AsRawFd;
            // SAFETY: a fresh read-only private mapping of the whole file, owned by the Base
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    f.as_raw_fd(),
                    0,
                )
            };
            if p != libc::MAP_FAILED {
                return Ok(Disk::with_base(Base::Mapped {
                    p: p as *const u8,
                    len,
                }));
            }
        }
        drop(f);
        Ok(Disk::with_base(Base::Bytes(std::fs::read(path)?)))
    }

    /// A disk whose base is these bytes (tests, and images made in memory).
    pub fn from_vec(b: Vec<u8>) -> Disk {
        Disk::with_base(Base::Bytes(b))
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of blocks held in memory (written since the base).
    pub fn written(&self) -> usize {
        self.top
            .iter()
            .flatten()
            .map(|c| c.iter().filter(|b| b.is_some()).count())
            .sum()
    }

    /// The bytes of block `k` (the last one may be shorter).
    fn block(&self, k: usize) -> &[u8] {
        let end = ((k + 1) * BLOCK).min(self.len);
        let n = end - k * BLOCK;
        match self.top[k / CHUNK]
            .as_ref()
            .and_then(|c| c[k % CHUNK].as_ref())
        {
            Some(b) => &b[..n],
            None => &self.base.bytes()[k * BLOCK..end],
        }
    }

    /// Block `k` for writing: copied from the base, or out of a shared copy, first.
    fn block_mut(&mut self, k: usize) -> &mut [u8; BLOCK] {
        let (ci, bi) = (k / CHUNK, k % CHUNK);
        let base = &self.base;
        let len = self.len;
        let chunk = self.top[ci].get_or_insert_with(|| Arc::new(vec![None; CHUNK]));
        let chunk = Arc::make_mut(chunk);
        let b = chunk[bi].get_or_insert_with(|| {
            let mut a = [0u8; BLOCK];
            let end = ((k + 1) * BLOCK).min(len);
            a[..end - k * BLOCK].copy_from_slice(&base.bytes()[k * BLOCK..end]);
            Arc::new(a)
        });
        Arc::make_mut(b)
    }

    /// Copy `buf.len()` bytes from offset `off` (the range must be inside the disk).
    pub fn read(&self, off: usize, buf: &mut [u8]) {
        assert!(off + buf.len() <= self.len, "a read past the disk's end");
        let mut done = 0;
        while done < buf.len() {
            let at = off + done;
            let (k, o) = (at / BLOCK, at % BLOCK);
            let b = self.block(k);
            let n = (b.len() - o).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&b[o..o + n]);
            done += n;
        }
    }

    /// Write `data` at offset `off` (the range must be inside the disk).
    pub fn write(&mut self, off: usize, data: &[u8]) {
        assert!(off + data.len() <= self.len, "a write past the disk's end");
        let mut done = 0;
        while done < data.len() {
            let at = off + done;
            let (k, o) = (at / BLOCK, at % BLOCK);
            let lim = ((k + 1) * BLOCK).min(self.len) - k * BLOCK;
            let n = (lim - o).min(data.len() - done);
            self.block_mut(k)[o..o + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
    }

    /// Feed the first `n` bytes to `f` in order, a block at a time (the hash, the disk's file).
    pub fn for_each(&self, n: usize, mut f: impl FnMut(&[u8])) {
        let n = n.min(self.len);
        let mut k = 0;
        while k * BLOCK < n {
            let b = self.block(k);
            let m = (n - k * BLOCK).min(b.len());
            f(&b[..m]);
            k += 1;
        }
    }

    /// Whether the two disks hold the same bytes.
    pub fn same_bytes(&self, o: &Disk) -> bool {
        if self.len != o.len {
            return false;
        }
        let blocks = self.len.div_ceil(BLOCK);
        (0..blocks).all(|k| self.block(k) == o.block(k))
    }

    /// Write the first `n` bytes to the file `path`: into a new file next to it that then takes
    /// its name, so that a path the base is mapped from is not rewritten under the mapping.
    pub fn write_file(&self, path: &str, n: usize) -> std::io::Result<()> {
        use std::io::Write;
        let tmp = format!("{path}.tmp-{}", std::process::id());
        let r = (|| {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            let mut err = Ok(());
            self.for_each(n, |b| {
                if err.is_ok() {
                    err = f.write_all(b);
                }
            });
            err?;
            f.flush()?;
            drop(f);
            std::fs::rename(&tmp, path)
        })();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    /// The whole disk's bytes (tests).
    pub fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len);
        self.for_each(self.len, |b| v.extend_from_slice(b));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_share_until_written() {
        let n = BLOCK * CHUNK * 2 + 1536; // two chunks and a short last block
        let mut base = vec![0u8; n];
        for (i, b) in base.iter_mut().enumerate() {
            *b = (i * 7 % 251) as u8;
        }
        let mut d = Disk::from_vec(base.clone());
        let mut flat = base.clone();
        let s0 = d.clone();
        for (off, len, v) in [
            (10usize, 5usize, 1u8),
            (BLOCK - 3, 9, 2),
            (BLOCK * CHUNK - 1, 3, 3),
            (n - 700, 700, 4),
        ] {
            let data = vec![v; len];
            d.write(off, &data);
            flat[off..off + len].copy_from_slice(&data);
        }
        assert_eq!(d.to_vec(), flat);
        assert_eq!(
            s0.to_vec(),
            base,
            "the copy taken before the writes is unchanged"
        );
        assert_eq!(d.written(), 5);
        let s1 = d.clone();
        d.write(0, &[9; 4]);
        assert_eq!(s1.to_vec(), flat);
        let mut buf = vec![0u8; 20];
        d.read(BLOCK - 10, &mut buf);
        assert_eq!(
            &buf[..],
            &{
                let mut f = flat.clone();
                f[..4].copy_from_slice(&[9; 4]);
                f
            }[BLOCK - 10..BLOCK + 10]
        );
        assert!(s1.same_bytes(&Disk::from_vec(flat.clone())));
        assert!(!d.same_bytes(&s1));
        // the hash stream: block by block, the same bytes as the flat image
        let mut fed = Vec::new();
        s1.for_each(n - 100, |b| fed.extend_from_slice(b));
        assert_eq!(fed, flat[..n - 100]);
    }

    #[test]
    fn opens_a_file_and_writes_one() {
        let dir = std::env::temp_dir().join(format!("hydo-disk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("d.img");
        let bytes: Vec<u8> = (0..3 * BLOCK + 512).map(|i| (i % 13) as u8).collect();
        std::fs::write(&p, &bytes).unwrap();
        let ps = p.to_str().unwrap();
        let mut d = Disk::open(ps).unwrap();
        assert_eq!(d.to_vec(), bytes);
        d.write(BLOCK + 1, b"hello");
        // the same path the base is mapped from: the mapping keeps the old bytes
        d.write_file(ps, d.len()).unwrap();
        let mut want = bytes.clone();
        want[BLOCK + 1..BLOCK + 6].copy_from_slice(b"hello");
        assert_eq!(std::fs::read(&p).unwrap(), want);
        assert_eq!(d.to_vec(), want);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
