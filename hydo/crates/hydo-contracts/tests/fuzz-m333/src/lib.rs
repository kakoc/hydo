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
