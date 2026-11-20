//! The seventh fixture of `hydo fuzz` (Milestone 333): what the fourth evaluation found, each
//! with a contract.

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
