//! The fifth fixture of `hydo fuzz` (Milestone 318): the shapes of a Rust API that the third
//! evaluation found refused or judged wrong, each with a contract.

// ---------------------------------------------------------------- built values in a list

/// A 16-bit value with a private field, as half's `f16`: built by its constructor.
pub mod h {
    #[derive(Clone, Copy)]
    pub struct H(u16);

    impl H {
        pub const fn from_bits(bits: u16) -> H {
            H(bits)
        }

        pub const fn to_bits(self) -> u16 {
            self.0
        }
    }

    /// The same without a `read` line.
    pub struct G(u16);

    impl G {
        pub const fn new(x: u16) -> G {
            G(x)
        }

        pub const fn get(&self) -> u16 {
            self.0
        }
    }
}

/// Each value's bits, as half's `reinterpret_cast`: an element of a list of built values
/// compares as the value its `read` line reads, never as its constructor's arguments.
// hydo: build h::H from h::H::from_bits(bits: u16)
// hydo: read h::H as u16 by to_bits()
// hydo: ensures len(v) == 0 || result[0] == v[0]
// hydo: ensures len(v) == 0 || old(v[0]) == result[0]
// hydo: ensures len(result) == len(v)
pub fn bits(v: &[h::H]) -> Vec<u16> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// A built value without a `read` line: a clause that compares it is not judged, with why.
// hydo: build h::G from h::G::new(x: u16)
// hydo: requires len(v) > 0
// hydo: ensures result == v[0]
pub fn first(v: &[h::G]) -> u16 {
    v.first().map_or(0, h::G::get)
}

// ---------------------------------------------------------------- one name in two modules

/// humantime has two enumerations named `Error`, in `date.rs` and in `duration.rs`.
pub mod date {
    #[derive(Debug, PartialEq)]
    pub enum Error {
        OutOfRange,
        InvalidDigit,
        InvalidFormat,
    }
}

pub mod duration {
    #[derive(Debug, PartialEq)]
    pub enum Error {
        Empty,
        NumberExpected(usize),
        UnknownUnit { start: usize, end: usize },
    }

    /// As humantime's `parse_duration`: the module's own `Error`.
    // hydo: ensures !matches(result, Err(Error::Empty)) || s == ""
    // hydo: ensures s != "" || matches(result, Err(Empty))
    pub fn parse(s: &str) -> Result<u64, Error> {
        if s.is_empty() {
            return Err(Error::Empty);
        }
        s.parse::<u64>().map_err(|_| Error::NumberExpected(0))
    }
}

/// A module that imports one of the two.
pub mod user {
    use crate::duration::Error;

    // hydo: ensures matches(result, Error::Empty) == (n == 0)
    pub fn check(n: u8) -> Error {
        if n == 0 {
            Error::Empty
        } else {
            Error::UnknownUnit {
                start: 0,
                end: n as usize,
            }
        }
    }
}

/// A full path from the crate's root.
// hydo: ensures !matches(result, Err(crate::duration::Error::Empty)) || len(s) == 0
pub fn parse_full(s: &str) -> Result<u64, crate::duration::Error> {
    duration::parse(s)
}

// ---------------------------------------------------------------- const generic parameters

/// The first `N` bytes, as a const generic function.
// hydo: with N = 4
// hydo: ensures is_none(result) || len(some(result)) == 4
// hydo: ensures is_some(result) == (len(v) >= 4)
pub fn head<const N: usize>(v: &[u8]) -> Option<[u8; N]> {
    v.get(..N).map(|s| {
        let mut a = [0; N];
        a.copy_from_slice(s);
        a
    })
}

/// A trait implemented for arrays of every length, as a const generic `impl` block.
pub trait Total {
    fn total(&self) -> u32;
}

impl<const N: usize> Total for [u8; N] {
    // hydo: with N = 3
    // hydo: ensures result <= 765
    fn total(&self) -> u32 {
        self.iter().map(|&b| b as u32).sum()
    }
}

// ---------------------------------------------------------------- `&'static` parameters

/// A set of bytes, as percent-encoding's `AsciiSet`, which its encoders take as `&'static`.
pub struct Set {
    pub lo: u64,
}

/// The bytes of `input` below 64 that the set holds.
// hydo: ensures len(result) <= len(input)
pub fn keep(input: &[u8], set: &'static Set) -> Vec<u8> {
    input
        .iter()
        .copied()
        .filter(|&b| b < 64 && set.lo & (1 << b) != 0)
        .collect()
}

/// A name that must live as long as the program.
// hydo: ensures result == len(name)
pub fn tag(name: &'static str) -> usize {
    name.len()
}

// ---------------------------------------------------------------- closures

/// either's `Either`, whose methods take closures.
pub enum Either<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> Either<L, R> {
    // hydo: with L = i32
    // hydo: with R = u8
    // hydo: with M = i64
    // hydo: ensures matches(result, Left(_)) == matches(self, Left(_))
    pub fn map_left<F, M>(self, f: F) -> Either<M, R>
    where
        F: FnOnce(L) -> M,
    {
        match self {
            Either::Left(l) => Either::Left(f(l)),
            Either::Right(r) => Either::Right(r),
        }
    }

    // hydo: with L = i32
    // hydo: with R = u8
    // hydo: ensures !matches(self, Left(_)) || result == self.Left.0
    pub fn left_or_else<F>(self, f: F) -> L
    where
        F: FnOnce(R) -> L,
    {
        match self {
            Either::Left(l) => l,
            Either::Right(r) => f(r),
        }
    }
}

/// A predicate given as `impl Fn`.
// hydo: ensures result <= len(v)
pub fn count_if(v: &[u8], pred: impl Fn(&u8) -> bool) -> usize {
    v.iter().filter(|b| pred(b)).count()
}

/// A closure whose result hydo cannot generate: refused with advice that works.
// hydo: ensures result == 0
pub fn tally<F: FnMut(u8) -> std::collections::HashMap<u8, u8>>(mut f: F) -> usize {
    f(0).len()
}

// ---------------------------------------------------------------- characters that change case

/// Each word with its first character in upper case and the rest in lower case.
pub fn title(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// heck's issue 70: a title case of a title case changes it, for `ﬄ` (`FFL`, then `Ffl`) and
/// `ß` (`SS`, then `Ss`). The default strings hold such characters since Milestone 318.
// hydo: ensures result
pub fn title_is_idempotent(s: &str) -> bool {
    let t = title(s);
    title(&t) == t
}

// ---------------------------------------------------------------- `read` and `build` lines

/// An iterator over bytes, as percent-encoding's `PercentDecode`: read by `collect()`.
#[derive(Clone)]
pub struct Iter<'a> {
    s: &'a [u8],
    i: usize,
}

impl Iterator for Iter<'_> {
    type Item = u8;
    fn next(&mut self) -> Option<u8> {
        let b = self.s.get(self.i).copied();
        self.i += 1;
        b
    }
}

// hydo: read Iter as Vec<u8> by collect()
// hydo: ensures len(result) == len(v)
pub fn iter_of(v: &[u8]) -> Iter<'_> {
    Iter { s: v, i: 0 }
}

/// The same, read with a turbofish.
#[derive(Clone)]
pub struct Iter2<'a>(Iter<'a>);

impl Iterator for Iter2<'_> {
    type Item = u8;
    fn next(&mut self) -> Option<u8> {
        self.0.next()
    }
}

// hydo: read Iter2 as Vec<u8> by collect::<Vec<u8>>()
// hydo: ensures len(result) == len(v)
pub fn iter2_of(v: &[u8]) -> Iter2<'_> {
    Iter2(iter_of(v))
}

/// A stack of a fixed capacity, as arrayvec's `ArrayVec<T, CAP>`: a generic type in a `build`
/// line.
pub struct Ring<T, const CAP: usize> {
    items: Vec<T>,
}

impl<T: Copy, const CAP: usize> Ring<T, CAP> {
    pub fn from_vec(v: Vec<T>) -> Self {
        assert!(v.len() <= CAP);
        Ring { items: v }
    }

    pub fn to_vec(&self) -> Vec<T> {
        self.items.clone()
    }

    // hydo: with T = u8
    // hydo: with CAP = 4
    // hydo: build Ring<u8, 4> from Ring::<u8, 4>::from_vec(v: Vec<u8>)
    // hydo: read Ring<u8, 4> as Vec<u8> by to_vec()
    // hydo: input self.v len 0..=4
    // hydo: ensures is_err(result) || len(self) == old(len(self)) + 1
    // hydo: ensures is_ok(result) == (old(len(self)) < 4)
    pub fn try_push(&mut self, x: T) -> Result<(), T> {
        if self.items.len() < CAP {
            self.items.push(x);
            Ok(())
        } else {
            Err(x)
        }
    }
}

// ---------------------------------------------------------------- associated result types

/// heck's conversions return `Self::Owned`, the standard library's `ToOwned::Owned`.
pub trait Shout: ToOwned {
    // hydo: with Self = str
    // hydo: ensures self != "" || result == ""
    fn shout(&self) -> Self::Owned;
}

impl Shout for str {
    fn shout(&self) -> Self::Owned {
        self.to_uppercase()
    }
}

/// An associated type that the crate's `impl` names.
pub trait Measure {
    type Out;
    // hydo: with Self = u8
    // hydo: ensures result == self + 1
    fn measure(&self) -> Self::Out;
}

impl Measure for u8 {
    type Out = u16;
    fn measure(&self) -> Self::Out {
        *self as u16 + 1
    }
}

/// An associated type of a type that hydo does not read: not judged, and the report names the
/// type instead of blaming the inputs.
pub trait Index {
    type Out;
    // hydo: with Self = u8
    // hydo: ensures self != 0 || len(result) == 0
    fn index(&self) -> Self::Out;
}

impl Index for u8 {
    type Out = std::collections::HashMap<u8, u8>;
    fn index(&self) -> Self::Out {
        std::collections::HashMap::new()
    }
}
