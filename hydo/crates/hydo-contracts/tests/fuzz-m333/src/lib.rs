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
