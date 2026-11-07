# hydo-test-macros

The `#[driver(..)]` and `#[check(..)]` attributes of [`hydo-test`](https://crates.io/crates/hydo-test),
the Rust test SDK of hydo, a deterministic hypervisor (https://github.com/kakoc/hydo). The
crate has no dependencies (the attribute is parsed by hand). Do not depend on it directly: use
it through `hydo-test` (`use hydo_test::prelude::*;`), whose README describes both attributes.

License: Apache-2.0.
