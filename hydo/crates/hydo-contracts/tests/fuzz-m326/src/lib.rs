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
