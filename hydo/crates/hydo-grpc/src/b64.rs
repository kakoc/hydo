//! Base64 with padding as Go's `base64.StdEncoding` reads and writes it: decoding ignores '\r'
//! and '\n', needs the padding, and accepts non-zero trailing bits.

const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(ALPHA[(n >> 18) as usize & 63] as char);
        out.push(ALPHA[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            ALPHA[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            ALPHA[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn val(c: u8) -> Option<u32> {
    ALPHA.iter().position(|&a| a == c).map(|p| p as u32)
}

/// `base64.StdEncoding.DecodeString`; None where Go returns an error.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let src: Vec<u8> = s.bytes().filter(|&c| c != b'\r' && c != b'\n').collect();
    if !src.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(src.len() / 4 * 3);
    let n = src.len();
    for (qi, q) in src.chunks(4).enumerate() {
        let last = (qi + 1) * 4 == n;
        let pads = q.iter().rev().take_while(|&&c| c == b'=').count();
        if pads > 2 || (pads > 0 && !last) {
            return None;
        }
        let mut v = 0u32;
        for &c in &q[..4 - pads] {
            v = (v << 6) | val(c)?;
        }
        match pads {
            0 => out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]),
            1 => {
                v <<= 6;
                out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8]);
            }
            _ => {
                v <<= 12;
                out.push((v >> 16) as u8);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn like_go() {
        assert_eq!(super::encode(b""), "");
        assert_eq!(super::encode(b"\x01\xffabc"), "Af9hYmM=");
        assert_eq!(super::decode("Af9hYmM=").unwrap(), b"\x01\xffabc");
        assert_eq!(super::decode("abcd").unwrap(), [0x69, 0xb7, 0x1d]);
        assert_eq!(super::decode("").unwrap(), b"");
        assert!(super::decode("abc").is_none());
        assert!(super::decode("a===").is_none());
        assert!(super::decode("ab==cdef").is_none());
        assert!(super::decode("hello world").is_none());
        assert_eq!(super::decode("YQ==\n").unwrap(), b"a");
        assert_eq!(super::decode("YR==").unwrap(), b"a"); // trailing bits accepted
    }
}
