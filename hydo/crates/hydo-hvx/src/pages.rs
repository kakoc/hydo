//! A saved copy of guest RAM or of the data partition, kept in memory as its non-zero 4 KiB
//! blocks only. A fresh VM's RAM and disk pages are zero (the module allocates them zeroed), so
//! restoring into one writes only these blocks: the same bytes as the C engine's full copy back,
//! with less memory and less copying.

pub const BLOCK: usize = 4096;

#[derive(Clone, Default)]
pub struct Pages {
    len: usize,
    idx: Vec<u32>,
    data: Vec<u8>,
}

impl Pages {
    /// Save `src` (any length; the last block may be short).
    pub fn store(src: &[u8]) -> Pages {
        let mut p = Pages {
            len: src.len(),
            idx: Vec::new(),
            data: Vec::new(),
        };
        for (i, b) in src.chunks(BLOCK).enumerate() {
            if !crate::fnv::is_zero(b) {
                p.idx.push(i as u32);
                p.data.extend_from_slice(b);
            }
        }
        p
    }

    /// The saved length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// How many blocks are not zero.
    pub fn nonzero_blocks(&self) -> usize {
        self.idx.len()
    }

    /// Write the saved bytes into `dst`, which must be all zero (a fresh VM's RAM or disk).
    pub fn load_into_zero(&self, dst: &mut [u8]) {
        assert_eq!(dst.len(), self.len, "restore size");
        let mut off = 0;
        for &i in &self.idx {
            let at = i as usize * BLOCK;
            let n = BLOCK.min(self.len - at);
            dst[at..at + n].copy_from_slice(&self.data[off..off + n]);
            off += n;
        }
    }

    /// Write the saved bytes into `dst` whatever it holds (zero blocks are written as zeros).
    pub fn load_into(&self, dst: &mut [u8]) {
        assert_eq!(dst.len(), self.len, "restore size");
        let mut next = self.idx.iter().peekable();
        let mut off = 0;
        for (i, d) in dst.chunks_mut(BLOCK).enumerate() {
            if next.peek() == Some(&&(i as u32)) {
                next.next();
                d.copy_from_slice(&self.data[off..off + d.len()]);
                off += d.len();
            } else {
                d.fill(0);
            }
        }
    }

    /// HVX_EXPLORE_DIRTY: compare `cur` with the saved bytes 4 KiB block by block: (blocks,
    /// blocks that differ, saved blocks that are not zero, blocks of `cur` that are not zero).
    pub fn diff(&self, cur: &[u8]) -> (u64, u64, u64, u64) {
        let mut next = self.idx.iter().peekable();
        let mut off = 0;
        let (mut n, mut diff, mut nz, mut curnz) = (0u64, 0u64, 0u64, 0u64);
        for (i, c) in cur.chunks(BLOCK).enumerate() {
            n += 1;
            let saved: Option<&[u8]> = if next.peek() == Some(&&(i as u32)) {
                next.next();
                let l = c.len().min(self.len.saturating_sub(i * BLOCK));
                let s = &self.data[off..off + l];
                off += l;
                Some(s)
            } else {
                None
            };
            let cz = crate::fnv::is_zero(c);
            if !cz {
                curnz += 1;
            }
            match saved {
                Some(s) => {
                    nz += 1;
                    if s != c {
                        diff += 1;
                    }
                }
                None => {
                    if !cz {
                        diff += 1;
                    }
                }
            }
        }
        (n, diff, nz, curnz)
    }

    /// The whole saved bytes (a diagnostic: HVX_EXPLORE_DURABLE_OUT).
    pub fn to_vec(&self) -> Vec<u8> {
        let mut v = vec![0u8; self.len];
        self.load_into_zero(&mut v);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut src = vec![0u8; 3 * BLOCK + 700];
        src[5] = 1;
        src[2 * BLOCK + 9] = 2;
        src[3 * BLOCK + 699] = 3;
        let p = Pages::store(&src);
        assert_eq!(p.nonzero_blocks(), 3);
        let mut z = vec![0u8; src.len()];
        p.load_into_zero(&mut z);
        assert_eq!(z, src);
        let mut d = vec![7u8; src.len()];
        p.load_into(&mut d);
        assert_eq!(d, src);
        assert_eq!(p.to_vec(), src);
        let mut cur = src.clone();
        assert_eq!(p.diff(&cur), (4, 0, 3, 3));
        cur[5] = 0; // block 0 now zero
        cur[BLOCK + 1] = 9; // block 1 now non-zero
        assert_eq!(p.diff(&cur), (4, 2, 3, 3));
    }
}
