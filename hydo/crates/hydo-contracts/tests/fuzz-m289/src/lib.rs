//! The third fixture of hydo-contracts' tests/fuzz.rs (Milestone 289). Each function shows one
//! problem that the second evaluation of `hydo fuzz` met on real crates.

// as strsim: the harness must not allow `unsafe_code`
#![forbid(unsafe_code)]
// as httparse: a lost `[lints]` table turns the `cfg` warning below into an error
#![cfg_attr(test, deny(warnings))]

#[cfg(fixture_simd)]
pub fn simd() {}

/// A score between 0 and 1, an `f64` result as strsim's `jaro`.
// hydo: requires b > 0 && a <= b
// hydo: ensures result >= 0 && result <= 1
pub fn ratio(a: u32, b: u32) -> f64 {
    a as f64 / b as f64
}

/// A planted bug: the score passes 1 from `x = 200` on.
// hydo: ensures result < 1
pub fn score(x: u8) -> f64 {
    x as f64 / 200.0
}

/// A floating-point parameter.
// hydo: input x in -2..=2
// hydo: ensures x != x || (result >= 0 && result <= 1)
pub fn clamp_unit(x: f64) -> f64 {
    if x < 0.0 {
        0.0
    } else if x > 1.0 {
        1.0
    } else {
        x
    }
}

/// An enumeration hydo does not read.
pub enum Kind {
    Small,
    Large,
}

/// A clause on a result that hydo cannot read: it is never decided, so it is not judged.
// hydo: ensures result != 0
pub fn kind(x: u8) -> Kind {
    if x < 128 {
        Kind::Small
    } else {
        Kind::Large
    }
}

/// As num-traits' `abs`: the one input that reaches the clause's last part panics in the test
/// profile.
// hydo: ensures result >= 0 || (v == -2147483648 && result == v)
pub fn abs32(v: i32) -> i32 {
    v.abs()
}

/// A parser of bytes, as httparse's: the size of a chunk before `\r\n`, in hexadecimal.
// hydo: input buf like b"4\r\n", b"1a\r\n", b"ff;ext\r\n"
// hydo: ensures is_none(result) || len(buf) >= 3
pub fn chunk_len(buf: &[u8]) -> Option<u64> {
    let end = buf.windows(2).position(|w| w == b"\r\n")?;
    let digits = buf[..end].split(|&b| b == b';').next()?;
    if digits.is_empty() {
        return None;
    }
    let mut n: u64 = 0;
    for &d in digits {
        n = n.checked_mul(16)? + (d as char).to_digit(16)? as u64;
    }
    Some(n)
}

/// A trait method: refused, so a run that names it does not test every contract.
pub trait Shape {
    // hydo: ensures result >= 0
    fn area(&self) -> i64 {
        0
    }
}
