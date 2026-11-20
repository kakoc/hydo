//! The seventh fixture of `hydo fuzz` (Milestone 333): what the fourth evaluation found, each
//! with a contract. It denies warnings, as ordered-float does, so that a helper that only the
//! functions that --fn leaves out call would stop the copy's build without hydo's
//! `#![allow(dead_code)]`.
#![deny(warnings)]

// ---------------------------------------------------------------- values that hydo does not read

/// A list of values of a type that hydo does not read: a comparison of it is undecided, never
/// broken (smallvec's `into_vec` broke with `result = [<not read>]`).
// hydo: input v len 1..=8
// hydo: ensures result == old(v)
pub fn wrapped(v: Vec<u8>) -> Vec<std::num::Wrapping<u8>> {
    v.into_iter().map(std::num::Wrapping).collect()
}

/// The same inside an `Option`: `Some([<not read>])` is not `Some([0])`.
// hydo: input v len 1..=8
// hydo: ensures is_none(result) || some(result) == v
pub fn wrapped_some(v: Vec<u8>) -> Option<Vec<std::num::Wrapping<u8>>> {
    Some(v.into_iter().map(std::num::Wrapping).collect())
}

// ---------------------------------------------------------------- errors of private modules

/// An error as semver's: a `pub(crate)` field of a type that a private module defines and the
/// crate's root does not import.
mod kinds {
    #[derive(Debug)]
    pub(crate) enum Kind {
        Empty,
        NotDigit(usize),
    }
}

mod perr {
    use crate::kinds::Kind;

    #[derive(Debug)]
    pub struct Error {
        pub(crate) kind: Kind,
    }

    impl Error {
        pub(crate) fn new(kind: Kind) -> Error {
            Error { kind }
        }
    }
}

pub use perr::Error;

/// A parser whose error has such a field: its harness builds, and a clause reads the error.
// hydo: input text like "7", "42", "", "x1"
// hydo: ensures is_err(result) || text != ""
// hydo: ensures !matches(result, Err(_)) || text != "" || matches(err(result).kind, Empty)
pub fn digits(text: &str) -> Result<u64, Error> {
    if text.is_empty() {
        return Err(Error::new(kinds::Kind::Empty));
    }
    match text.bytes().position(|b| !b.is_ascii_digit()) {
        Some(i) => Err(Error::new(kinds::Kind::NotDigit(i))),
        None => text.parse().map_err(|_| Error::new(kinds::Kind::NotDigit(0))),
    }
}

// ---------------------------------------------------------------- constructors

/// Values with private fields, built by a constructor without arguments, by one that returns
/// a `Result`, and by one that returns an `Option`.
pub mod made {
    #[derive(Default)]
    pub struct Req {
        parts: Vec<u8>,
    }

    impl Req {
        pub fn len(&self) -> usize {
            self.parts.len()
        }

        pub fn is_empty(&self) -> bool {
            self.parts.is_empty()
        }
    }

    /// A pre-release text: ASCII letters and digits only, as semver's `Prerelease::new`.
    pub struct Pre(String);

    impl Pre {
        pub fn new(text: &str) -> Result<Pre, String> {
            if text.bytes().all(|b| b.is_ascii_alphanumeric()) {
                Ok(Pre(text.to_string()))
            } else {
                Err(format!("not alphanumeric: {text}"))
            }
        }

        pub fn as_str(&self) -> &str {
            &self.0
        }
    }

    /// A number below 100.
    pub struct Tiny(u8);

    pub fn tiny(n: u8) -> Option<Tiny> {
        (n < 100).then_some(Tiny(n))
    }

    impl Tiny {
        pub fn get(&self) -> u8 {
            self.0
        }
    }
}

// hydo: build made::Req from made::Req::default()
// hydo: ensures result == 0
pub fn req_len(r: &made::Req) -> usize {
    r.len()
}

// hydo: build made::Pre from made::Pre::new(text: &str)
// hydo: read made::Pre as String by as_str()
// hydo: ensures len(result) == len(p)
pub fn pre_text(p: &made::Pre) -> String {
    p.as_str().to_string()
}

// hydo: build made::Tiny from made::tiny(n: u8)
// hydo: read made::Tiny as u8 by get()
// hydo: ensures result < 100
pub fn tiny_of(s: &made::Tiny) -> u8 {
    s.get()
}

/// A type whose refusal proposes only lines that build: no `unsafe` function, a qualified
/// `From` beside another trait's `from`, and a constructor that returns an `Option`.
pub mod num {
    pub struct Num(u8);

    impl Num {
        /// # Safety
        /// None needed; it is `unsafe` as ordered-float's `NotNan::new_unchecked` is.
        pub unsafe fn new_unchecked(x: u8) -> Num {
            Num(x)
        }

        pub fn try_new(x: u8) -> Option<Num> {
            (x < 200).then_some(Num(x))
        }
    }

    impl From<u8> for Num {
        fn from(x: u8) -> Num {
            Num(x / 2)
        }
    }

    pub trait Cast: Sized {
        fn from(x: i64) -> Option<Self>;
    }

    impl Cast for Num {
        fn from(x: i64) -> Option<Num> {
            u8::try_from(x).ok().map(Num)
        }
    }
}

// hydo: ensures result == 0
pub fn num_zero(n: &num::Num) -> u8 {
    let _ = n;
    0
}

// ---------------------------------------------------------------- generic `impl` blocks

/// A generic structure whose field is public, as ordered-float's `OrderedFloat<T>(pub T)`: with
/// `with T = f64` hydo generates it field by field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wrap<T>(pub T);

impl<T: Copy + PartialOrd> Wrap<T> {
    /// `&Wrap<T>` takes the `with` line's type.
    // hydo: with T = f64
    // hydo: ensures result == (self.0 < other.0)
    pub fn lt(&self, other: &Wrap<T>) -> bool {
        self.0 < other.0
    }
}

/// `Self::Output` of this `impl`, not of another type's `impl` of `Neg`.
impl<T: std::ops::Add<Output = T>> std::ops::Add<T> for Wrap<T> {
    type Output = Self;

    // hydo: with T = i64
    // hydo: input self.0 in -1000..=1000
    // hydo: input other in -1000..=1000
    // hydo: ensures result.0 == self.0 + other
    fn add(self, other: T) -> Self::Output {
        Wrap(self.0 + other)
    }
}

pub struct Other(pub u8);

impl std::ops::Neg for Other {
    type Output = u8;

    fn neg(self) -> u8 {
        0u8.wrapping_sub(self.0)
    }
}

/// `with T = &[u8]` passes a slice.
// hydo: with T = &[u8]
// hydo: ensures is_none(result) || some(result) == input[0]
pub fn first_of<T: AsRef<[u8]>>(input: T) -> Option<u8> {
    input.as_ref().first().copied()
}

/// A structure with a lifetime parameter, read field by field, as textwrap's `Options<'a>`.
pub struct Opts<'a> {
    pub width: usize,
    pub indent: &'a str,
}

// hydo: ensures result.width == width && result.indent == ""
pub fn opts(width: usize) -> Opts<'static> {
    Opts { width, indent: "" }
}

/// `Cow<str>` reads as its text, as textwrap's `wrap` returns `Vec<Cow<str>>`.
// hydo: ensures len(result) == 1 && result[0] == s
pub fn cowed(s: &str) -> Vec<std::borrow::Cow<'_, str>> {
    vec![std::borrow::Cow::Borrowed(s)]
}

/// A `Cow` parameter takes a generated value.
// hydo: ensures result == len(c)
pub fn cow_len(c: std::borrow::Cow<'_, [u8]>) -> usize {
    c.len()
}

/// A type that a `build` line builds, whose public fields a clause reads without a `read`
/// line, as semver's `Version`.
pub mod ver {
    pub struct Ver {
        pub major: u64,
        pub minor: u64,
        note: String,
    }

    impl Ver {
        pub fn new(major: u64, minor: u64) -> Ver {
            Ver {
                major,
                minor,
                note: String::new(),
            }
        }

        pub fn note(&self) -> &str {
            &self.note
        }
    }
}

// hydo: build ver::Ver from ver::Ver::new(major: u64, minor: u64)
// hydo: ensures result == v.major + v.minor
pub fn ver_sum(v: &ver::Ver) -> u128 {
    v.major as u128 + v.minor as u128 + v.note().len() as u128
}

// hydo: ensures result.major == major && result.minor == 0
pub fn ver_of(major: u64) -> ver::Ver {
    ver::Ver::new(major, 0)
}

// ---------------------------------------------------------------- associated types

/// A container generic over a trait with an associated type, as smallvec's `SmallVec<A:
/// Array>`: `A::Item` is `u8` with `with A = [u8; 4]`.
pub mod small {
    /// # Safety
    /// None needed; the trait is `unsafe` as smallvec's `Array` is.
    pub unsafe trait Store {
        type Item;
        fn size() -> usize;
    }

    unsafe impl<T, const N: usize> Store for [T; N] {
        type Item = T;
        fn size() -> usize {
            N
        }
    }

    pub struct Small<A: Store> {
        items: Vec<A::Item>,
    }

    /// An `IntoIterator` whose `type Item = A::Item;` is no alias of the crate.
    impl<A: Store> IntoIterator for Small<A> {
        type Item = A::Item;
        type IntoIter = std::vec::IntoIter<A::Item>;
        fn into_iter(self) -> Self::IntoIter {
            self.items.into_iter()
        }
    }

    impl<A: Store> Small<A> {
        pub fn from_vec(v: Vec<A::Item>) -> Self {
            Small { items: v }
        }

        // hydo: with A = [u8; 4]
        // hydo: build Small<[u8; 4]> from Small::<[u8; 4]>::from_vec(v: Vec<u8>)
        // hydo: read Small<[u8; 4]> as Vec<u8> by to_vec()
        // hydo: ensures len(self) == old(len(self)) + 1 && self[len(self) - 1] == value
        pub fn push(&mut self, value: A::Item) {
            self.items.push(value)
        }

        // hydo: with A = [u8; 4]
        // hydo: ensures is_none(result) || some(result) == old(self[len(self) - 1])
        pub fn pop(&mut self) -> Option<<A as Store>::Item> {
            self.items.pop()
        }

        // hydo: with A = [u8; 4]
        // hydo: ensures result == old(self)
        pub fn into_vec(self) -> Vec<A::Item> {
            self.items
        }

        // hydo: with A = [u8; 4]
        // hydo: ensures len(self) <= old(len(self))
        pub fn retain<F: FnMut(&mut A::Item) -> bool>(&mut self, mut f: F) {
            self.items.retain_mut(|x| f(x))
        }

        pub fn len(&self) -> usize {
            self.items.len()
        }

        pub fn is_empty(&self) -> bool {
            self.items.is_empty()
        }
    }

    impl<A: Store> Small<A>
    where
        A::Item: Clone,
    {
        pub fn to_vec(&self) -> Vec<A::Item> {
            self.items.clone()
        }
    }
}

// ---------------------------------------------------------------- the smaller items

/// A `with` line for a name that is no type parameter is refused.
// hydo: with Item = u8
// hydo: ensures result == x
pub fn same_u8(x: u8) -> u8 {
    x
}

/// A nested `input` path through a built value names its constructor's arguments.
// hydo: input v.note like "a"
// hydo: ensures result >= v.major
pub fn ver_major(v: &ver::Ver) -> u128 {
    v.major as u128
}

/// A string's byte by its index.
// hydo: ensures is_none(result) || some(result) == s[0]
pub fn first_byte(s: &str) -> Option<u8> {
    s.bytes().next()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    North,
    East,
    South,
    West,
}

/// Two values of one enumeration compare with `==`.
// hydo: ensures result == d
pub fn turn_four(d: Dir) -> Dir {
    let t = |d: Dir| match d {
        Dir::North => Dir::East,
        Dir::East => Dir::South,
        Dir::South => Dir::West,
        Dir::West => Dir::North,
    };
    t(t(t(t(d))))
}

/// Two floats that are both NaN: the contract is wrong exactly then.
// hydo: ensures !result
pub fn both_nan(a: f64, b: f64) -> bool {
    a.is_nan() && b.is_nan()
}

/// An index that `requires` keeps inside its list is generated there.
// hydo: requires index < len(v)
// hydo: ensures result == v[index]
pub fn at(v: Vec<u8>, index: usize) -> u8 {
    v[index]
}

/// A helper that only `uses_helper` calls: `--fn first_byte` leaves it unused.
fn helper(x: u8) -> u8 {
    x.wrapping_add(1)
}

// hydo: ensures result == x || x == 255
pub fn uses_helper(x: u8) -> u8 {
    helper(x).wrapping_sub(1)
}

/// Whether the feature `extra` is on: the contract holds with `--features extra` only.
// hydo: ensures result
pub fn extra_on(x: u8) -> bool {
    let _ = x;
    cfg!(feature = "extra")
}

// ---------------------------------------------------------------- build lines across modules

/// A type that the module of its `build` line's function does not import, as base64's
/// `Alphabet` in its `engine` module.
pub mod deep {
    pub struct Inner(u8);

    impl Inner {
        pub fn new(x: u8) -> Inner {
            Inner(x)
        }

        pub fn get(&self) -> u8 {
            self.0
        }
    }
}

pub mod user {
    // hydo: build Inner from Inner::new(x: u8)
    // hydo: read Inner as u8 by get()
    // hydo: ensures result == i.x
    pub fn get(i: &crate::deep::Inner) -> u8 {
        i.get()
    }
}

/// A generic constructor of a generic `impl` block, named for one type, as ordered-float's
/// `NotNan::new(val: T)` for `NotNan<f64>`.
#[derive(Clone, Copy)]
pub struct Nn<T>(T);

impl<T: PartialEq + Copy> Nn<T> {
    #[allow(clippy::eq_op)]
    pub fn new(val: T) -> Option<Nn<T>> {
        (val == val).then_some(Nn(val))
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl Nn<f64> {
    // hydo: build Nn from Nn::new(val: T)
    // hydo: read Nn as f64 by into_inner()
    // hydo: ensures result == self
    pub fn value(self) -> f64 {
        self.0
    }
}
