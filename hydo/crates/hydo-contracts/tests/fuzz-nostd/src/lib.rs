//! The second fixture of hydo-contracts' tests/fuzz.rs (Milestone 282). The crate is
//! `#![no_std]`, and each function shows one problem that Milestone 280 met on real crates.

#![no_std]

extern crate alloc;

use alloc::vec;

mod ident {
    /// A structure with a private field, in another module than the functions that use it.
    pub struct Ident {
        head: u8,
        pub len: u8,
    }

    impl Ident {
        pub fn new(len: u8) -> Ident {
            Ident { head: 0, len }
        }

        pub fn head(&self) -> u8 {
            self.head
        }
    }
}

/// A version: its `pre` holds a private field that the harness cannot name.
pub struct Version {
    pub major: u64,
    pub pre: ident::Ident,
}

/// A result with a private field of another module: the harness reads the rest.
// hydo: ensures result.major == major && result.pre.len == 0
pub fn version(major: u64) -> Version {
    Version {
        major,
        pre: ident::Ident::new(0),
    }
}

/// A parameter with a private field of another module: refused, with the reason.
// hydo: ensures result == v.major
pub fn major_of(v: &Version) -> u64 {
    v.major + v.pre.head() as u64
}

mod a {
    pub struct P {
        pub x: u8,
    }
}

pub mod b {
    /// Another `P` than `a::P`: until Milestone 318 hydo's textual reading took the first one
    /// it met, so this function's harness did not build. Now the function's module decides.
    pub struct P {
        pub y: u16,
    }

    // hydo: ensures result == p.y
    pub fn get(p: P) -> u16 {
        p.y
    }
}

/// A `with` line that names no type: this function's harness does not build, and the other
/// functions still run.
// hydo: with T = NoSuchType
// hydo: ensures is_none(result)
pub fn nothing<T>() -> Option<T> {
    None
}

/// A `requires` that generated inputs almost never keep: `too few inputs`.
// hydo: requires s == "hydo"
// hydo: ensures result
pub fn is_name(s: &str) -> bool {
    s == "hydo"
}

/// A parser that the random strings never reach: its clause holds without being checked.
// hydo: ensures is_none(result) || some(result) < 10
pub fn digit_of(s: &str) -> Option<u8> {
    let d = s.strip_prefix("num:")?;
    d.parse::<u8>().ok().filter(|&d| d < 10)
}

/// The same parser, with examples of its input: the clause is checked.
// hydo: input s like "num:7", "num:0"
// hydo: ensures is_none(result) || some(result) < 10
pub fn digit_of_like(s: &str) -> Option<u8> {
    let d = s.strip_prefix("num:")?;
    d.parse::<u8>().ok().filter(|&d| d < 10)
}

/// A search with a planted bug past the 64th element, which only long lists reach.
// hydo: ensures is_none(result) || v[some(result)] == x
pub fn find_late(v: &[u8], x: u8) -> Option<usize> {
    let i = v.iter().position(|&b| b == x)?;
    Some(if i >= 100 { i + 1 } else { i })
}

/// A size without an `input` line: its values stay small, so no call waits for the timeout.
// hydo: ensures result == n
pub fn zeros(n: usize) -> usize {
    vec![0u8; n].len()
}

/// A generic function, fuzzed with the type that the contract names.
// hydo: with T = u8
// hydo: ensures result >= x
pub fn twice<T: Copy + core::ops::Add<Output = T>>(x: T) -> T {
    x + x
}

/// A value with `Display`, read by `to_string()`: the harness of a `#![no_std]` crate imports
/// `ToString` (Milestone 318).
pub struct Word(u8);

impl core::fmt::Display for Word {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for _ in 0..self.0 {
            f.write_str("a")?;
        }
        Ok(())
    }
}

// hydo: read Word as String by to_string()
// hydo: input n in 0..=40
// hydo: ensures len(result) == n
pub fn word(n: u8) -> Word {
    Word(n)
}
