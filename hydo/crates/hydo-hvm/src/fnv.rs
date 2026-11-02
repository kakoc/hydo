//! The hashes of the C engine: FNV-1a over bytes (`fnv`, hvm.c and virtio.c) and the word-wise
//! FNV-style mix over guest RAM (`mix64`, serve.c).

pub const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
pub const PRIME: u64 = 0x0000_0100_0000_01b3;

/// `fnv(p, n, h)`: FNV-1a continued from `h`.
pub fn fnv(b: &[u8], mut h: u64) -> u64 {
    for &c in b {
        h ^= c as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// `fnv` of a little-endian u64.
pub fn fnv_u64(v: u64, h: u64) -> u64 {
    fnv(&v.to_le_bytes(), h)
}

/// `mix64(p, n, h)`: per 64-bit word `h ^= w; h *= P; h ^= h >> 29`, then `fnv` of the tail.
pub fn mix64(b: &[u8], mut h: u64) -> u64 {
    let k = b.len() / 8;
    let (words, tail) = b.split_at(8 * k);
    for w in words.chunks_exact(8) {
        h ^= u64::from_le_bytes(w.try_into().unwrap());
        h = h.wrapping_mul(PRIME);
        h ^= h >> 29;
    }
    fnv(tail, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_values() {
        assert_eq!(fnv(b"", OFFSET), OFFSET);
        assert_eq!(fnv(b"a", OFFSET), 0xaf63_dc4c_8601_ec8c);
        // a tail of fewer than 8 bytes is plain fnv
        assert_eq!(mix64(b"abc", 7), fnv(b"abc", 7));
    }
}
