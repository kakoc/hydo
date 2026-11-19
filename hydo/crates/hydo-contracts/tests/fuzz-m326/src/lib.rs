//! The sixth fixture of `hydo fuzz` (Milestone 326): what the third evaluation left after
//! Milestone 318, each with a contract.

// ---------------------------------------------------------------- harnesses that do not build

/// A value with a private field, read by a method that does not exist: the harness does not
/// build, and the report names the `read` line.
pub mod text {
    #[derive(Clone)]
    pub struct Text(String);

    impl Text {
        pub fn new(s: String) -> Text {
            Text(s)
        }

        pub fn len(&self) -> usize {
            self.0.len()
        }

        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
    }
}

// hydo: build text::Text from text::Text::new(s: String)
// hydo: read text::Text as String by as_text()
// hydo: ensures len(result) == len(t)
pub fn echo(t: text::Text) -> text::Text {
    t
}

/// A `build` line whose argument has the wrong type: `Code::new` takes a `u8`.
pub mod code {
    pub struct Code(u8);

    impl Code {
        pub fn new(x: u8) -> Code {
            Code(x)
        }

        pub fn get(&self) -> u8 {
            self.0
        }
    }
}

// hydo: build code::Code from code::Code::new(x: u16)
// hydo: read code::Code as u8 by get()
// hydo: ensures result == c
pub fn code_of(c: code::Code) -> u8 {
    c.get()
}

/// A `with` line whose type does not fit the bound.
// hydo: with T = String
// hydo: ensures result == x
pub fn same<T: Copy>(x: T) -> T {
    x
}

// ---------------------------------------------------------------- a private field

/// Values whose fields the harness cannot set: the refusal names the `build` line with the
/// crate's public constructors, and a `read` line.
pub mod bits {
    #[derive(Clone, Copy)]
    pub struct B(u16);

    impl B {
        pub const fn from_bits(bits: u16) -> B {
            B(bits)
        }

        pub fn new(x: u8) -> Self {
            B(x as u16)
        }

        pub const fn to_bits(self) -> u16 {
            self.0
        }

        fn secret() -> B {
            B(0)
        }
    }

    impl From<u8> for B {
        fn from(x: u8) -> B {
            B(x as u16 + B::secret().0)
        }
    }

    #[derive(Clone, Default)]
    pub struct D {
        n: u8,
    }

    impl D {
        pub fn n(&self) -> u8 {
            self.n
        }
    }

    pub struct Bare {
        n: u8,
    }

    impl Bare {
        pub fn n(&self) -> u8 {
            self.n
        }
    }
}

// hydo: ensures len(result) == len(v)
pub fn widths(v: &[bits::B]) -> Vec<u16> {
    v.iter().map(|b| b.to_bits()).collect()
}

// hydo: ensures result == 0 || result > 0
pub fn d_of(d: bits::D) -> u8 {
    d.n()
}

// hydo: ensures result == 0 || result > 0
pub fn bare_of(b: &bits::Bare) -> u8 {
    b.n()
}

// ---------------------------------------------------------------- the bit operators

/// half's NaN test with the bit operators, against an implementation that compares.
// hydo: ensures result == ((x >> 10) & 0x1f == 0b11111 && (x & 0x3ff) != 0)
pub fn is_nan(x: u16) -> bool {
    x & 0x7fff > 0x7c00
}

/// `!` on an integer is Rust's bitwise `!` of its type.
// hydo: ensures result == !x && result ^ x == 0xff && (result | x) == 255 && (result & x) == 0
pub fn flip(x: u8) -> u8 {
    !x
}

// hydo: ensures result == !x && result == -x - 1
pub fn flip_signed(x: i8) -> i8 {
    !x
}

/// `<<` is exact: no bit is lost.
// hydo: ensures result == x << 2 && result >> 2 == x
pub fn times_four(x: u8) -> u16 {
    (x as u16) << 2
}

/// `&` on two conditions.
// hydo: ensures result == (a & b) && !(result ^ (a && b))
pub fn both(a: bool, b: bool) -> bool {
    a && b
}

// ---------------------------------------------------------------- patterns of named fields

/// As humantime's `parse_duration`: a variant with named fields.
pub mod units {
    #[derive(Debug, PartialEq)]
    pub enum Error {
        Empty,
        NumberExpected(usize),
        UnknownUnit { start: usize, end: usize },
    }

    // hydo: input s like "5s", "12m", "5x", "", "x5", "7"
    // hydo: ensures s != "" || matches(result, Err(Error::Empty { .. }))
    // hydo: ensures !matches(result, Err(Error::UnknownUnit { .. })) || len(s) >= 1
    // hydo: ensures !matches(result, Err(Error::UnknownUnit { start: 0, .. }))
    // hydo: ensures !matches(result, Err(Error::UnknownUnit { start, end })) || is_err(result)
    pub fn unit(s: &str) -> Result<u64, Error> {
        if s.is_empty() {
            return Err(Error::Empty);
        }
        let digits = s.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(Error::NumberExpected(0));
        }
        let n: u64 = s[..digits].parse().unwrap_or(u64::MAX);
        match &s[digits..] {
            "s" => Ok(n),
            "m" => Ok(n.saturating_mul(60)),
            _ => Err(Error::UnknownUnit {
                start: digits,
                end: s.len(),
            }),
        }
    }

    /// A field that the variant does not have is refused before the run.
    // hydo: ensures !matches(result, Err(Error::UnknownUnit { begin: 0, .. }))
    pub fn unit_misspelt(s: &str) -> Result<u64, Error> {
        unit(s)
    }
}

// ---------------------------------------------------------------- `&'static mut` parameters

/// A `&'static mut` parameter, read after the call.
// hydo: ensures result == old(len(buf)) + 1 && len(buf) == result && buf[result - 1] == x
pub fn push_static(buf: &'static mut Vec<u8>, x: u8) -> usize {
    buf.push(x);
    buf.len()
}

/// A result that keeps the `&'static mut` reference.
// hydo: ensures len(result) == old(len(s)) + 1 && len(s) == len(result)
pub fn bang(s: &'static mut String) -> &'static mut String {
    s.push('!');
    s
}

// ---------------------------------------------------------------- glob imports and re-exports

/// A second `Mode`, defined first, so that only the `use` lines tell the two apart.
pub mod decoy {
    #[derive(Debug, Clone, Copy)]
    pub enum Mode {
        A,
        B,
    }
}

pub mod deep {
    pub mod inner {
        #[derive(Debug, Clone, Copy)]
        pub enum Mode {
            Fast,
            Slow(u8),
        }

        pub struct Point {
            pub x: u8,
            pub y: u8,
        }
    }
}

/// A renamed re-export at the root.
pub use deep::inner::Mode as Speed;

pub mod api {
    use crate::deep::inner::*;

    /// `Mode` through a glob import.
    // hydo: ensures matches(m, Slow(_)) == is_some(result)
    pub fn slow_of(m: Mode) -> Option<u8> {
        match m {
            Mode::Slow(x) => Some(x),
            Mode::Fast => None,
        }
    }

    // hydo: ensures result == p.x + p.y
    pub fn sum(p: Point) -> u16 {
        p.x as u16 + p.y as u16
    }
}

/// `Speed`, a renamed re-export.
// hydo: ensures result == matches(s, Slow(_))
pub fn is_slow(s: Speed) -> bool {
    matches!(s, Speed::Slow(_))
}

pub mod far {
    /// The re-export through its path.
    // hydo: ensures result == matches(s, Fast)
    pub fn is_fast(s: crate::Speed) -> bool {
        matches!(s, crate::Speed::Fast)
    }
}

// ---------------------------------------------------------------- two lists of one length

/// As half's `convert_from_f32_slice`: the lists must have one length.
// hydo: requires len(dst) == len(src)
// hydo: ensures len(dst) == len(src)
// hydo: ensures len(src) == 0 || dst[len(src) - 1] == src[len(src) - 1] / 2
pub fn halve_into(dst: &mut [u16], src: &[u16]) {
    assert_eq!(dst.len(), src.len());
    for (d, s) in dst.iter_mut().zip(src) {
        *d = s / 2;
    }
}

/// The same with a planted bug from the third element on.
// hydo: requires len(dst) == len(src)
// hydo: ensures len(src) == 0 || dst[len(src) - 1] == src[len(src) - 1] / 2
pub fn halve_into_bad(dst: &mut [u16], src: &[u16]) {
    assert_eq!(dst.len(), src.len());
    for (i, (d, s)) in dst.iter_mut().zip(src).enumerate() {
        *d = if i >= 2 && *s > 1 { 0 } else { s / 2 };
    }
}

/// A planted mask that drops bits 8 to 11.
// hydo: ensures result == x & 0x0fff
pub fn low_bits(x: u16) -> u16 {
    x & 0x00ff
}
