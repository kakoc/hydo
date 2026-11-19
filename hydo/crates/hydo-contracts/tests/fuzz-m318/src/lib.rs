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
