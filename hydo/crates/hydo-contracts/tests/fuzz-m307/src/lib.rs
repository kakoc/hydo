//! The fourth fixture of hydo-contracts' tests/fuzz.rs (Milestone 307). Each part of a Rust API
//! that the evaluations of `hydo fuzz` found refused has a function here, some with a planted
//! bug.

#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::ptr::NonNull;

// ---------------------------------------------------------------- enumerations

/// A parse that may need more input, as httparse's `Status`.
#[derive(Debug, PartialEq)]
pub enum Status<T> {
    Complete(T),
    Partial,
}

/// A chunk size line that does not parse.
#[derive(Debug, PartialEq)]
pub struct Bad;

/// A planted bug, httparse's: a line without a hex digit parses as size 0. The contract states
/// the rule `chunk-size = 1*HEXDIG` on the parser itself.
// hydo: input buf like b"4\r\n", b"1a;x\r\n", b"ff \r\n", b"0\r\n"
// hydo: ensures !matches(result, Ok(Complete(_))) || (buf[0] >= 48 && buf[0] <= 57) || (buf[0] >= 97 && buf[0] <= 102) || (buf[0] >= 65 && buf[0] <= 70)
// hydo: ensures !matches(result, Ok(Status::Complete(_))) || ok(result).Complete.0.0 <= len(buf)
pub fn chunk_size(buf: &[u8]) -> Result<Status<(usize, u64)>, Bad> {
    let mut size: u64 = 0;
    for (i, &b) in buf.iter().enumerate() {
        match b {
            b'0'..=b'9' => size = size.checked_mul(16).ok_or(Bad)? + (b - b'0') as u64,
            b'a'..=b'f' => size = size.checked_mul(16).ok_or(Bad)? + (b - b'a' + 10) as u64,
            b'A'..=b'F' => size = size.checked_mul(16).ok_or(Bad)? + (b - b'A' + 10) as u64,
            b'\r' => {
                return match buf.get(i + 1) {
                    Some(b'\n') => Ok(Status::Complete((i + 2, size))),
                    Some(_) => Err(Bad),
                    None => Ok(Status::Partial),
                }
            }
            b' ' | b';' => {}
            _ => return Err(Bad),
        }
    }
    Ok(Status::Partial)
}

/// An operation with fields of each kind.
#[derive(Debug)]
pub enum Op {
    Neg,
    Add(i32),
    Clamp { lo: i32, hi: i32 },
}

/// An enumeration as a parameter. A planted bug: `Clamp` with `lo > hi` gives a value above
/// `hi`.
// hydo: ensures !matches(op, Neg) || result == -x
// hydo: ensures !matches(op, Add(_)) || result == x + op.Add.0
// hydo: ensures !matches(op, Clamp) || result <= op.Clamp.hi
pub fn apply(op: Op, x: i32) -> i64 {
    match op {
        Op::Neg => -(x as i64),
        Op::Add(k) => x as i64 + k as i64,
        Op::Clamp { lo, hi } => (x.max(lo).min(hi.max(lo))) as i64,
    }
}

/// The standard library's `Ordering` as a result.
// hydo: ensures matches(result, Less) == (a < b)
// hydo: ensures matches(result, Equal) == (a == b)
pub fn compare(a: u16, b: u16) -> Ordering {
    a.cmp(&b)
}

/// A misspelt variant: refused before the run, with the variants it could be.
// hydo: ensures matches(result, Greatr) || a <= b
pub fn compare_typo(a: u8, b: u8) -> Ordering {
    a.cmp(&b)
}

// ---------------------------------------------------------------- trait methods

/// A trait whose methods are implemented by a macro, as num-traits' are.
pub trait Halve: Sized {
    /// Half the value, rounded towards zero. The contract holds for unsigned types only: a
    /// negative odd value breaks it.
    // hydo: with Self = u8
    // hydo: with Self = i64
    // hydo: ensures result * 2 <= self
    fn halve(&self) -> Self;
}

macro_rules! halve_impl {
    ($($t:ty)*) => {$(
        impl Halve for $t {
            fn halve(&self) -> Self {
                *self / 2
            }
        }
    )*};
}
halve_impl!(u8 i64);

/// A byte order, as byteorder's `ByteOrder`.
pub trait Endian {
    const BIG: bool;
}

/// The big-endian order: a type without values, as byteorder's `BigEndian`.
pub enum Big {}

impl Endian for Big {
    const BIG: bool = true;
}

/// A reader over bytes, as byteorder's `ReadBytesExt`: a generic default method of a trait,
/// fuzzed on `&[u8]`, whose `&mut self` advances the slice.
pub trait Take {
    fn next_byte(&mut self) -> Option<u8>;

    /// The first `n` bytes as a number. A planted bug: `n` of 0 takes a byte.
    // hydo: with Self = &[u8]
    // hydo: with E = Big
    // hydo: input n in 0..=8
    // hydo: ensures is_none(result) || len(self) == old(len(self)) - n
    fn take_uint<E: Endian>(&mut self, n: usize) -> Option<u64> {
        let mut v = 0u64;
        for k in 0..n.max(1) {
            let b = self.next_byte()? as u64;
            v = if E::BIG { v << 8 | b } else { v | b << (8 * k) };
        }
        Some(v)
    }
}

impl Take for &[u8] {
    fn next_byte(&mut self) -> Option<u8> {
        let (&f, rest) = self.split_first()?;
        *self = rest;
        Some(f)
    }
}

/// A trait method without `with Self`: refused with what to write.
pub trait Size {
    // hydo: ensures result >= 0
    fn size(&self) -> i64;
}

impl Size for u8 {
    fn size(&self) -> i64 {
        *self as i64
    }
}

// ---------------------------------------------------------------- values built by a constructor

/// A buffer with a pointer, as bytes' `Bytes`: hydo cannot build it field by field.
pub struct Blob {
    data: Vec<u8>,
    start: usize,
    _ptr: Option<NonNull<u8>>,
}

impl Blob {
    // hydo: build Blob from Blob::from_vec(v: Vec<u8>)
    // hydo: read Blob as Vec<u8> by to_vec()
    // hydo: ensures len(result) == len(v)
    pub fn from_vec(v: Vec<u8>) -> Blob {
        Blob {
            data: v,
            start: 0,
            _ptr: None,
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.data[self.start..].to_vec()
    }

    /// The first `at` bytes; `self` keeps the rest.
    // hydo: requires at <= len(self.v)
    // hydo: ensures len(result) == at && len(self) == old(len(self)) - at
    pub fn split_to(&mut self, at: usize) -> Blob {
        let head = self.data[self.start..self.start + at].to_vec();
        self.start += at;
        Blob::from_vec(head)
    }

    /// A planted bug: a length of 3 more than a multiple of 7 keeps one byte too many.
    // hydo: ensures len(self) == old(len(self)) || len(self) == len
    pub fn truncate(&mut self, len: usize) {
        if len < self.data.len() - self.start {
            self.data.truncate(self.start + len + (len % 7 == 3) as usize);
        }
    }
}

/// A buffer without a `build` line: refused with what to write.
pub struct Raw {
    _ptr: NonNull<u8>,
}

impl Raw {
    // hydo: ensures result >= 0
    pub fn len(&self) -> i64 {
        0
    }
}

// ---------------------------------------------------------------- sorted inputs

/// A binary search over a sorted list, as Qdrant's `delta_pack` needs: without `sorted` almost
/// every random list breaks `requires`.
// hydo: requires sorted(v)
// hydo: ensures is_none(result) || v[some(result)] == x
pub fn search(v: &[u32], x: u32) -> Option<usize> {
    v.binary_search(&x).ok()
}

/// The deltas of a sorted list and back.
// hydo: requires sorted(v)
// hydo: ensures sorted(result) && len(result) == len(v)
pub fn roundtrip(v: Vec<u16>) -> Vec<u16> {
    let mut d = Vec::new();
    let mut prev = 0u16;
    for &x in &v {
        d.push(x - prev);
        prev = x;
    }
    let mut out = Vec::new();
    let mut acc = 0u16;
    for x in d {
        acc += x;
        out.push(acc);
    }
    out
}
