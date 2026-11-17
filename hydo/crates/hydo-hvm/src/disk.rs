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
//!
//! **The tree hash** (Milestone 290: the fingerprint of an image built at the fingerprint epoch 4
//! or later, [`Disk::tree_hash`]). The disk's part of a fingerprint is a hash of hashes. Each
//! 4 KiB block has a hash ([`leaf`], the word-wise mix of `crate::fnv::mix64`), each 2 MiB chunk a
//! hash over its blocks' hashes in order ([`node`]), and the root covers the disk's length and the
//! chunks' hashes in order. The root is a function of the disk's bytes alone, whatever the table
//! holds: a block that a run wrote is hashed from its bytes, and a block it did not write has the
//! base's hash. The base's hashes are made once per base, on a thread of their own while the guest
//! runs ([`Disk::hash_base_soon`]) or at the first fingerprint on every core, and every copy of the
//! disk shares them. So a fingerprint hashes again only the blocks written since the base. Each step is a bijection of the state when the other inputs are fixed (`h ^=
//! w`, a multiplication by the odd FNV prime, `h ^= h >> 29`, and FNV-1a's bytes), so a change of
//! any one byte of the disk always changes the root. An image of an older epoch keeps the
//! byte-wise FNV of every byte ([`Disk::for_each`]).

use crate::fnv::{fnv, fnv_u64, mix64, OFFSET, PRIME};
use std::sync::{Arc, Mutex, OnceLock};

/// The size of a block of the copy-on-write table, and of a leaf of the tree hash.
pub const BLOCK: usize = 4096;
/// Blocks per second-level chunk (2 MiB of disk per chunk), and per node of the tree hash.
const CHUNK: usize = 512;

type Block = Arc<[u8; BLOCK]>;
type Chunk = Arc<Vec<Option<Block>>>;

/// The bytes every copy of the disk shares.
enum Store {
    #[cfg(unix)]
    Mapped {
        p: *const u8,
        len: usize,
    },
    Bytes(Vec<u8>),
}

// SAFETY: the mapping is read-only and owned by the Store for its whole life
unsafe impl Send for Store {}
// SAFETY: as above: nothing writes the mapped bytes
unsafe impl Sync for Store {}

impl Store {
    fn bytes(&self) -> &[u8] {
        match self {
            #[cfg(unix)]
            // SAFETY: a read-only mapping of `len` bytes for this value's life
            Store::Mapped { p, len } => unsafe { std::slice::from_raw_parts(*p, *len) },
            Store::Bytes(b) => b,
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Store::Mapped { p, len } = self {
            // SAFETY: the mapping made in `Disk::open`; no slice of it outlives the Store
            unsafe {
                libc::munmap(*p as *mut libc::c_void, *len);
            }
        }
    }
}

/// The base's tree hashes: one per block and one per chunk.
struct Sums {
    block: Vec<u64>,
    chunk: Vec<u64>,
}

/// The base: its bytes, whether they are all zero (an added disk), and its tree hashes once made.
struct Base {
    store: Store,
    zero: bool,
    sums: OnceLock<Sums>,
}

impl Base {
    fn new(store: Store, zero: bool) -> Base {
        Base {
            store,
            zero,
            sums: OnceLock::new(),
        }
    }

    fn bytes(&self) -> &[u8] {
        self.store.bytes()
    }

    /// The base's tree hashes: made now on every core, unless another thread makes them (then
    /// this waits for them).
    fn sums(&self) -> &Sums {
        self.sums.get_or_init(|| self.make_sums(cores()))
    }

    /// Every block's and every chunk's hash of the base: for a zero base from one block's hash,
    /// for any other on `threads` threads, 64 MiB of the disk at a time.
    fn make_sums(&self, threads: usize) -> Sums {
        /// chunks per piece of work
        const RUN: usize = 32;
        let b = self.bytes();
        let len = b.len();
        let blocks = len.div_ceil(BLOCK);
        let chunks = len.div_ceil(BLOCK * CHUNK);
        let mut block = vec![0u64; blocks];
        let mut chunk = vec![0u64; chunks];
        if self.zero {
            block.fill(leaf(&[0u8; BLOCK]));
            if !len.is_multiple_of(BLOCK) {
                block[blocks - 1] = leaf(&b[(blocks - 1) * BLOCK..]);
            }
            for (c, h) in block.chunks(CHUNK).zip(chunk.iter_mut()) {
                *h = node(c.iter().copied());
            }
            return Sums { block, chunk };
        }
        let threads = threads.min(chunks.div_ceil(RUN)).max(1);
        let work = Mutex::new(
            block
                .chunks_mut(CHUNK * RUN)
                .zip(chunk.chunks_mut(RUN))
                .enumerate(),
        );
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let next = work.lock().unwrap_or_else(|e| e.into_inner()).next();
                    let Some((i, (bs, cs))) = next else { break };
                    let first = i * CHUNK * RUN;
                    for (j, h) in bs.iter_mut().enumerate() {
                        let at = (first + j) * BLOCK;
                        *h = leaf(&b[at..(at + BLOCK).min(len)]);
                    }
                    for (c, h) in bs.chunks(CHUNK).zip(cs.iter_mut()) {
                        *h = node(c.iter().copied());
                    }
                });
            }
        });
        Sums { block, chunk }
    }
}

/// The host's cores.
fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// A block's hash in the tree (the last block of a disk may be shorter).
fn leaf(b: &[u8]) -> u64 {
    mix64(b, OFFSET)
}

/// A chunk's hash over its blocks' hashes, in order: per hash `h ^= w; h *= P; h ^= h >> 29`.
fn node(blocks: impl Iterator<Item = u64>) -> u64 {
    blocks.fold(OFFSET, |mut h, w| {
        h ^= w;
        h = h.wrapping_mul(PRIME);
        h ^ (h >> 29)
    })
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
                return Ok(Disk::with_base(Base::new(
                    Store::Mapped {
                        p: p as *const u8,
                        len,
                    },
                    false,
                )));
            }
        }
        drop(f);
        Ok(Disk::with_base(Base::new(
            Store::Bytes(std::fs::read(path)?),
            false,
        )))
    }

    /// A disk whose base is these bytes (tests, and images made in memory).
    pub fn from_vec(b: Vec<u8>) -> Disk {
        Disk::with_base(Base::new(Store::Bytes(b), false))
    }

    /// A disk of `len` zero bytes (an added disk, Milestone 263). Its base's tree hashes need no
    /// pass over its bytes.
    pub fn zeroed(len: usize) -> Disk {
        Disk::with_base(Base::new(Store::Bytes(vec![0u8; len]), true))
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

    /// Start making the base's tree hashes on a thread of their own, on half of the host's cores,
    /// while the guest runs. Reading a disk that is not in the page cache takes seconds (3.5 s for
    /// 3.4 GB after a run of web-shop on an M2 Pro). The hashes are a function of the base's bytes,
    /// so when they are made changes nothing; a fingerprint that comes first waits for them.
    pub fn hash_base_soon(&self) {
        if self.base.sums.get().is_some() {
            return;
        }
        let b = self.base.clone();
        let _ = std::thread::Builder::new()
            .name("disk-tree".into())
            .spawn(move || {
                b.sums.get_or_init(|| b.make_sums(cores().div_ceil(2)));
            });
    }

    /// The tree hash of the whole disk, continued from `h` (the module documentation): the base's
    /// hashes, made at the first call for this base, and the blocks written since the base hashed
    /// again from their bytes.
    pub fn tree_hash(&self, h: u64) -> u64 {
        let sums = self.base.sums();
        let blocks = self.len.div_ceil(BLOCK);
        let mut h = fnv(b"hydo-disk-tree-v1", h);
        h = fnv_u64(self.len as u64, h);
        for (ci, c) in self.top.iter().enumerate() {
            let c = match c {
                None => sums.chunk[ci],
                Some(c) => {
                    let first = ci * CHUNK;
                    let n = (blocks - first).min(CHUNK);
                    node((first..first + n).map(|k| match &c[k - first] {
                        None => sums.block[k],
                        Some(b) => leaf(&b[..((k + 1) * BLOCK).min(self.len) - k * BLOCK]),
                    }))
                }
            };
            h = fnv_u64(c, h);
        }
        h
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

    /// The tree hash from the flat bytes alone, as the module documentation defines it.
    fn tree_of(b: &[u8], h: u64) -> u64 {
        let mut h = fnv(b"hydo-disk-tree-v1", h);
        h = fnv_u64(b.len() as u64, h);
        for c in b.chunks(BLOCK * CHUNK) {
            h = fnv_u64(node(c.chunks(BLOCK).map(leaf)), h);
        }
        h
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn the_tree_hash_is_a_function_of_the_bytes() {
        // two chunks and a short last block
        let n = BLOCK * CHUNK * 2 + 1536;
        let base = pattern(n);
        let d0 = Disk::from_vec(base.clone());
        assert_eq!(d0.tree_hash(OFFSET), tree_of(&base, OFFSET));
        assert_eq!(d0.tree_hash(7), tree_of(&base, 7));
        // the format is pinned: a change of it moves every epoch-4 fingerprint
        assert_eq!(format!("{:016x}", d0.tree_hash(OFFSET)), "e37c386f14728064");
        let mut d = d0.clone();
        let mut flat = base.clone();
        for (off, len, v) in [
            (10usize, 5usize, 1u8),
            (BLOCK - 3, 9, 2),
            (BLOCK * CHUNK - 1, 3, 3),
            (n - 700, 700, 4),
        ] {
            d.write(off, &vec![v; len]);
            flat[off..off + len].fill(v);
        }
        // written blocks hashed again, the base's hashes for the rest, the same as the flat bytes
        assert_eq!(d.tree_hash(OFFSET), tree_of(&flat, OFFSET));
        assert_eq!(
            d.tree_hash(OFFSET),
            Disk::from_vec(flat.clone()).tree_hash(OFFSET)
        );
        assert_ne!(d.tree_hash(OFFSET), d0.tree_hash(OFFSET));
        // the copy taken before the writes keeps its hash
        assert_eq!(d0.tree_hash(OFFSET), tree_of(&base, OFFSET));
        // a block written back to the base's bytes: the base's hash again
        let mut r = d0.clone();
        r.write(BLOCK * 3 + 5, &[0xee; 10]);
        r.write(BLOCK * 3 + 5, &base[BLOCK * 3 + 5..BLOCK * 3 + 15]);
        assert_eq!(r.written(), 1);
        assert_eq!(r.tree_hash(OFFSET), d0.tree_hash(OFFSET));
    }

    #[test]
    fn a_changed_byte_changes_the_tree_hash() {
        let n = BLOCK * CHUNK + 3 * BLOCK + 100;
        let base = pattern(n);
        let d0 = Disk::from_vec(base.clone());
        let h0 = d0.tree_hash(OFFSET);
        for at in [0, 1, 7, 8, BLOCK - 1, BLOCK, BLOCK * CHUNK, n - 1] {
            for v in [base[at] ^ 1, base[at] ^ 0x80, base[at].wrapping_add(1)] {
                // in a written block
                let mut d = d0.clone();
                d.write(at, &[v]);
                assert_ne!(d.tree_hash(OFFSET), h0, "a write at {at}");
                // in the base
                let mut b = base.clone();
                b[at] = v;
                assert_ne!(Disk::from_vec(b).tree_hash(OFFSET), h0, "the base at {at}");
            }
        }
        // the length is in the hash: a disk one block longer of zeros differs
        let mut longer = base.clone();
        longer.extend_from_slice(&[0; BLOCK]);
        assert_ne!(Disk::from_vec(longer).tree_hash(OFFSET), h0);
    }

    #[test]
    fn the_base_hashes_are_made_on_every_core_as_on_one() {
        // more than one piece of work (64 MiB), with a short last chunk and block
        let n = BLOCK * CHUNK * 70 + 2 * BLOCK + 512;
        let base = pattern(n);
        let d = Disk::from_vec(base.clone());
        let (one, all) = (d.base.make_sums(1), d.base.make_sums(cores().max(3)));
        assert!(one.block == all.block && one.chunk == all.chunk);
        // made on their own thread first, or at the first fingerprint: the same
        d.hash_base_soon();
        assert_eq!(d.tree_hash(OFFSET), tree_of(&base, OFFSET));
        let fresh = Disk::from_vec(base.clone());
        assert_eq!(fresh.tree_hash(OFFSET), d.tree_hash(OFFSET));
        // a zero base takes its hashes from one block's
        for len in [BLOCK * CHUNK * 3, BLOCK * 5 + 512, 512] {
            assert_eq!(
                Disk::zeroed(len).tree_hash(OFFSET),
                Disk::from_vec(vec![0; len]).tree_hash(OFFSET),
                "{len} bytes"
            );
        }
        let mut z = Disk::zeroed(BLOCK * 4);
        z.write(BLOCK + 3, b"x");
        let mut flat = vec![0; BLOCK * 4];
        flat[BLOCK + 3] = b'x';
        assert_eq!(z.tree_hash(OFFSET), tree_of(&flat, OFFSET));
        assert_eq!(
            Disk::from_vec(Vec::new()).tree_hash(OFFSET),
            tree_of(&[], OFFSET)
        );
    }

    /// A test's folder, removed when the test ends, also when it fails (Milestone 271;
    /// `HYDO_KEEP_TMP=1` keeps it).
    struct Tmp(std::path::PathBuf);

    impl std::ops::Deref for Tmp {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            if std::env::var_os("HYDO_KEEP_TMP").is_some_and(|v| !v.is_empty() && v != "0") {
                return;
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn opens_a_file_and_writes_one() {
        let dir = std::env::temp_dir().join(format!("hydo-disk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = Tmp(dir);
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
        drop(dir);
    }
}
