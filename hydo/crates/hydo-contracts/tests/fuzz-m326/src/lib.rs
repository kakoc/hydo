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
