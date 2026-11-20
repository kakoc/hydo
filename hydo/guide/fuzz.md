# Test one Rust function from its contract: `hydo fuzz`

A function contract is a set of `// hydo:` comments right before a Rust function. `hydo fuzz`
reads them and tests each such function on its own, natively, without an image. It generates
inputs that keep `requires` and the `input` lines, calls the function on each, and judges each
call by `ensures`. A broken clause, a panic, or a call that does not return is shrunk to a small
input that replays. The same comments are checked at every call of a whole-system run (`hydo
guide manifest`, section "Function contracts"). `hydo fuzz --vm` makes the calls inside hydo's
virtual machine under faults instead (section "Fuzz under faults, inside the VM").

## A worked example

```rust
/// Where `x` first is.
// hydo: input v len 0..=2000
// hydo: ensures is_none(result) || v[some(result)] == x
pub fn find(v: &[u8], x: u8) -> Option<usize> {
    v.iter().position(|&b| b == x)
}
```

```sh
hydo fuzz path/to/crate --fn find
```

Each function gets one header line, one line per clause, and one verdict line. The last line
counts the verdicts:

```text
hydo: fuzz: mycrate::find (src/lib.rs:4): 1000 calls from seed 1; 0 of 1000 generated inputs broke `requires` (0.0 %)
hydo:   held: ensures is_none(result) || v[some(result)] == x (line 3) on 1000 calls, its last part decided 678
hydo: fuzz: mycrate::find: every clause held
hydo: fuzz: the report is /.../hydo-out/fuzz/fuzz.json
hydo: fuzz: 1 function fuzzed: 1 held
```

A broken clause adds the smallest input, what the call gave, and an `again:` command that
replays it with the run's own options. `fuzz.json` in `--out DIR` (default `hydo-out/fuzz`)
holds the whole report.

## The comment lines

- `// hydo: requires <expr>`: the inputs that the function accepts. hydo throws away a generated
  input that breaks it.
- `// hydo: ensures <expr>`: what must hold after the call. `result` is the returned value.
- `// hydo: input <path> ...`: the values that hydo generates for a parameter, a field
  (`l.balance`), or the elements of a list (`v[]`).
- `// hydo: with T = i64`: the type that a type parameter takes. It names the parameters of a
  generic function, of a generic `impl` block, and of a trait. `with Self = i64` names the type
  that a trait's method is fuzzed on (section "Trait methods"). A const generic parameter
  takes its value the same way: `with N = 16`. A `with` line for a name that is no type
  parameter is refused with the names that are.
- `// hydo: build <Type> from <constructor>(<name>: <type>, ...)` and `// hydo: read <Type> as
  <type> by <method>()`: how hydo makes and reads a value of a type that it cannot generate
  field by field (section "Values built by a constructor").

An expression reads parameters, fields, `result`, integers, floating-point numbers (`0.5`),
strings, a string's bytes by index (`text[0] >= 48`), `old(e)` (the value at the call's entry), and the functions `len`, `is_some`, `is_none`,
`is_ok`, `is_err`, `some`, `ok` and `err`. `hydo fuzz` also reads `matches(e, <pattern>)` and
`sorted(v)` (sections "Enumerations" and "Sorted inputs"). The operators are `||`, `&&`, the
comparisons, the bit operators `|`, `^`, `&`, `<<` and `>>`, `+ - * / %`, `!` and unary `-`.
They bind as in Rust: `x & 0x3ff != 0` is `(x & 0x3ff) != 0`. `!` on an integer is Rust's
bitwise `!` of its type, and on a condition its negation. An integer may be written `0x7c00`,
`0b11111` or `0o17`. Integers are exact, with no overflow: a `<<` whose result does not fit in
128 bits is undecided. half's NaN test is
`(self.0 >> 10) & 0x1f == 0b11111 && (self.0 & 0x3ff) != 0`. A comparison
of an integer with an `f32` or `f64` value is exact too. Arithmetic on such a value is the
program's own `f64` arithmetic, and a NaN is unequal to everything, itself included. A call of
the program's own code is refused.

A type that a `with` line names is written as the function's module writes it. byteorder's
`io.rs` does not import `BigEndian`, so its contracts write `with T = crate::BigEndian`. hydo
finds a type's definition through the module's `use` lines, a glob import (`use a::*`), and a
renamed re-export (`pub use a::B as C`). The test module writes a type of the crate that the
function's module does not import by its path from the crate's root. semver's `Error` has a
field of `ErrorKind`, which its private module `error` defines, so a parser's `Err` value
builds and reads as `crate::error::ErrorKind`, and `matches(err(result).kind, Empty)` reads
it. A private type of a module that is neither the function's module nor one of its parents
is not read.

## Enumerations

hydo generates and reads the values of an enumeration of the crate, and of the standard
library's `Ordering`. A variant's fields are generated like a structure's. `matches(e,
<pattern>)` is true when the value has the pattern's shape:

| Pattern | Matches |
|---|---|
| `_` | any value |
| `0`, `-1`, `"text"`, `true` | that value |
| `Complete`, `Status::Complete` | the variant, whatever its fields are |
| `Complete(_)`, `Ok(Complete((_, 0)))` | the variant, with a pattern for each field |
| `UnknownUnit { .. }`, `UnknownUnit { start: 0, .. }` | a variant or a structure with named fields: each named field has its pattern, and the others any value |
| `(_, 0)` | a tuple |

`Some`, `None`, `Ok` and `Err` are variants too. A variant's field reads like a field:
`ok(result).Complete.0` is the field of `Complete` inside `Ok`. On a call where the value is
another variant, the read is undecided, as `some(o)` is on `None`. httparse's
`parse_chunk_size` returns `Result<Status<(usize, u64)>, InvalidChunkSize>`, and this contract
states the rule `chunk-size = 1*HEXDIG` of RFC 9112:

```rust
// hydo: input buf like b"4\r\nRust\r\n", b"0\r\n\r\n", b"1a;ext=1\r\n", b"ff \r\n"
// hydo: ensures !matches(result, Ok(Complete(_))) || (buf[0] >= 48 && buf[0] <= 57) || (buf[0] >= 65 && buf[0] <= 70) || (buf[0] >= 97 && buf[0] <= 102)
pub fn parse_chunk_size(buf: &[u8]) -> result::Result<Status<(usize, u64)>, InvalidChunkSize> {
```

```text
hydo: contract broken: httparse::parse_chunk_size ensures !matches(result, Ok(Complete(_))) || ... on 28 of 1000 calls
hydo:   smallest input (0 shrink steps): buf = b"\r\n"
hydo:   the call gave: result = Ok(Complete((2, 0)))
```

A variant or a field that the type does not have is refused before the run. The refusal names
the variants or the fields that the type has. Two
values of one enumeration compare with `==` (`result == self`), and a variant's name is
written with `matches()`. An `input` line names a variant's field
as `op.Clamp.lo`.

## Trait methods

A contract on a trait's method names the type to fuzz it on with `with Self = <type>`. The
method can be required or have a default body. Each `with Self` line gives one run of the
method, named `<i64 as CheckedAdd>::checked_add`:

```rust
pub trait CheckedAdd: Sized + Add<Self, Output = Self> {
    // hydo: with Self = i64
    // hydo: with Self = u8
    // hydo: ensures is_none(result) || some(result) == self + v
    fn checked_add(&self, v: &Self) -> Option<Self>;
}
```

`Self` can be a reference. byteorder's `read_uint` is a default method of `ReadBytesExt`, and
with `with Self = &[u8]` its `&mut self` is a slice that the call advances.
`len(self)` is the length after the call, and `old(len(self))` before it:

```rust
// hydo: with Self = &[u8]
// hydo: with T = crate::BigEndian
// hydo: input nbytes in 0..=10
// hydo: ensures is_err(result) || len(self) == old(len(self)) - nbytes
fn read_uint<T: ByteOrder>(&mut self, nbytes: usize) -> Result<u64> {
```

```text
hydo: byteorder::io::<&[u8] as ReadBytesExt>::read_uint panicked at src/lib.rs:1966 on 290 of 1000 calls
hydo:   smallest input (0 shrink steps): self = b"", nbytes = 0
```

A method of `impl Trait for Type` is called as `<Type as Trait>::method`, and a method of a
generic `impl` block takes `with` lines for the block's type parameters. A trait method without
a `with Self` line is refused with what to write. A parameter of the form `&mut &[u8]` is
generated as a slice too.

A result of an associated type, such as heck's `fn to_snake_case(&self) -> Self::Owned`, is
read as the type it stands for. hydo takes it from `type Owned = String;` in the crate's
`impl` of the trait for the `Self` type, else from the result type that the `impl`'s method
writes, else from the standard library's `ToOwned`: `String` for `str`, `Vec<T>` for `[T]`.

```rust
pub trait ToSnakeCase: ToOwned {
    // hydo: with Self = str
    // hydo: ensures self != "" || result == ""
    fn to_snake_case(&self) -> Self::Owned;
}
```

An associated type of a type parameter, such as smallvec's `A::Item` or `<A as Array>::Item`,
is read through the type that the parameter's `with` line names. hydo takes it from the
crate's `impl` of the trait for that type: smallvec's `impl<T, const N: usize> Array for [T;
N] { type Item = T; }` makes `A::Item` a `u8` with `with A = [u8; 4]`. Without such an `impl`,
`Item` of a list or an array is its element. Such a type works in a parameter, in a result,
and in a generated closure's arguments:

```rust
// hydo: with A = [u8; 4]
// hydo: build SmallVec<[u8; 4]> from SmallVec::<[u8; 4]>::from_vec(v: Vec<u8>)
// hydo: read SmallVec<[u8; 4]> as Vec<u8> by to_vec()
// hydo: ensures len(self) == old(len(self)) + 1 && self[len(self) - 1] == value
pub fn push(&mut self, value: A::Item) {
```

A crate can define one name in two modules, as humantime does with `Error`. hydo then takes the
type of the function's own module, else the one that a `use` line of its file imports. A path
names the other one: `crate::duration::Error`.

## Generic structures and lifetimes

hydo generates a generic structure field by field when each of its fields is `pub`, with the
types of the `with` lines. ordered-float's `OrderedFloat<T>(pub T)` with `with T = f64` is an
`f64`, and a clause reads it as `self.0`. A `with` line's type also stands in the parameters'
types, so `fn eq(&self, other: &OrderedFloat<T>)` takes an `OrderedFloat<f64>`. With `with T
= &[u8]`, a parameter `input: T` takes a slice. A generic structure with a private field needs
a `build` line, so that its constructor keeps what it checks (ordered-float's `NotNan` holds
no NaN).

A structure with a lifetime parameter or a reference field, such as textwrap's `Options<'a>`,
is read field by field as a result: `result.width == width`. As a parameter it needs a `build`
line. `Cow<str>` and `Cow<[T]>` read as the text or the list they hold.

## Values built by a constructor

A type with a pointer or with private fields of another module cannot be generated field by
field. A `build` line names a function that builds it: hydo generates the function's arguments
and passes the value it returns. A `read` line names a method that turns the value into one
that hydo reads. The two lines hold for the whole crate, so write them once, on any contracted
function:

```rust
// hydo: build Bytes from Bytes::copy_from_slice(v: &[u8])
// hydo: read Bytes as Vec<u8> by to_vec()
// hydo: requires at <= len(self.v)
// hydo: ensures len(result) == at && len(self) == old(len(self)) - at
pub fn split_to(&mut self, at: usize) -> Self {
```

- A clause names the constructor's arguments as fields: `self.v` is the `v` that built `self`.
  An `input` line narrows them the same way (`input self.v len 0..=300`).
- `requires` reads only the arguments, because hydo judges it before the call.
- `ensures` reads the value through the `read` line: at the entry with `old(self)`, and after
  the call with `self` for a `&mut` parameter or with `result`.
- A report shows a built input as its constructor's call, `self = Bytes::copy_from_slice(b"ab")`.

- An element of a list of built values, or a field of one, is read through the `read` line
  too: `len(self) == 0 || result[0] == self[0]` compares half's `f16` as its `u16` bits.
- Without a `read` line, a clause reads the structure's fields that the test module can name.
  semver's `Version` has public `major`, `minor` and `patch`, so `result.major == major` reads
  them. A built value without a `read` line and without such fields is not compared: the
  clause is `not judged`, with the `read` line to write.

A constructor may take no argument (`build VersionReq from VersionReq::default()`). It may
also return a `Result` or an `Option` of the type, which parsers and date types do. hydo then passes
the `Ok` or `Some` value, and throws away an input for which the constructor gives `Err` or
`None`. The report counts those inputs apart from `requires`:

```rust
// hydo: build Prerelease from Prerelease::new(text: &str)
// hydo: read Prerelease as String by to_string()
// hydo: input self.text like "alpha", "rc.1", "x.7.z.92"
```

```text
hydo:   212 of 1000 inputs were thrown away: a `build` line's constructor gave `Err` or `None`
```

A line's type and constructor may be of another module than the function's. When the
function's module does not import them, the test module names them by their path from the
crate's root: base64's `engine` module does not import `Alphabet`. A constructor of a generic
`impl` block takes the type's arguments: `build NotNan from NotNan::new(val: T)` of `impl<T>
NotNan<T>` passes an `f64` for `NotNan<f64>`, also in `impl NotNan<f64>`, which has no `T`.

hydo reads the constructor's result type from the crate's sources. For a function that it does
not find, such as one of another crate, end the line with `?`: `build Date from
time_helpers::date(jd: i32)?`.

A constructor that panics on an input ends the call with a panic at the constructor. Narrow its
arguments with `input` lines: chrono's `NaiveDate::from_ymd(year: i32, month: u32, day: u32)`
needs `input self.month in 1..=12` and `input self.day in 1..=28`. A type without a `build` line
is refused with the line to write. For a structure with a private field, the refusal names
the crate's public constructor with its parameters and a method for the `read` line. The
constructors come in this order: `new`, `from_*`, `with_*`, the others, `From`, those that
return a `Result` or an `Option`, and `Default`. It names no `unsafe` function and no line of a macro. When another trait
of the type has a function `from` too, it names the trait: `<OrderedFloat<_> as
From<f64>>::from(v: f64)`. For half's `f16`:

```text
the field `0` of `f16` is private to the module `binary16`, so the harness cannot build the value: write `// hydo: build f16 from f16::from_bits(bits: u16)` (a public constructor of the crate; the others are `f16::from_f32(value: f32)`, `f16::from_f64(value: f64)`), and `// hydo: read f16 as u16 by to_bits()` for a clause that reads the value
```

A type can have a `build` line already, with a constructor's argument of a type that hydo
cannot generate. The refusal then names a line for the argument's type: base64's
`GeneralPurpose::new(alphabet: &Alphabet, ...)` gets a line for `Alphabet`.

When the crate has no constructor that hydo can call, the refusal says so: add one, or write
a property function.

The type of a `build` or a `read` line may have type arguments, and so may the constructor:

```rust
// hydo: with T = u8
// hydo: with CAP = 8
// hydo: build ArrayVec<u8, 8> from ArrayVec::<u8, 8>::from_iter(v: Vec<u8>)
// hydo: read ArrayVec<u8, 8> as Vec<u8> by to_vec()
// hydo: input self.v len 0..=8
// hydo: ensures is_ok(result) == (old(len(self)) < 8)
pub fn try_push(&mut self, element: T) -> Result<(), CapacityError<T>> {
```

A `read` line's method may be `to_string()`, also in a `#![no_std]` crate with `alloc`, and
`collect()`, which collects into the line's type. A turbofish works too:
`read PercentDecode as Vec<u8> by collect::<Vec<u8>>()`. A method that takes its value, such
as `collect()` or `into_vec()`, reads a clone, so the type must implement `Clone`.

## Closures and `&'static` parameters

A parameter whose type parameter is bound by `Fn`, `FnMut` or `FnOnce`, or whose type is `impl
Fn(...)`, takes a generated closure. hydo names the other type parameters with `with` lines and
chooses each closure from a few shapes:

- every closure: `|_| k`, with a generated `k` of the result's type;
- an argument of the result's type: `|x| x`;
- an integer argument and an integer result: `|x| try_from(x).unwrap_or(k)`, and the same plus
  one;
- an integer argument and a `bool` result: `|x| x % 2 == 0` and `|x| x > 0`;
- two integer arguments and an integer result: `|x, y| x + y`, which wraps;
- a second argument of the result's type: `|_, y| y`;
- two arguments of one type and a `bool` result: `|x, y| x < y` and `|x, y| x == y`;
- two arguments of one type and an `Ordering` result, for a sort: `|a, b| a.cmp(b)` and
  `|a, b| b.cmp(a)`, and no constant, because a sort may panic on an order that is not total;
- an `Option` result: `|x| Some(x)` for an argument of the inside's type, and
  `|x| try_from(x).ok()` for an integer argument and an `Option` of an integer.

```rust
// hydo: with L = i32
// hydo: with R = u8
// hydo: with M = i64
// hydo: ensures matches(result, Left(_)) == matches(self, Left(_))
pub fn map_left<F, M>(self, f: F) -> Either<M, R>
where
    F: FnOnce(L) -> M,
```

A report shows a closure in that form, and `--input` reads it back: `f = |x| x > 0`. A closure
whose result hydo cannot generate is refused. Test such a function with a property function
that calls it with closures of your own.

A `&'static` parameter, such as percent-encoding's `ascii_set: &'static AsciiSet`, gets a
generated value that lives as long as the test program: hydo leaks it after it reads the values
at entry. A `&'static str` and a `&'static [T]` work the same way. A `&'static mut` parameter
gets a leaked value too, and a clause reads it after the call. The test module makes that
reference in an `unsafe` block, so in a crate that forbids `unsafe_code` such a function is
refused.

## Sorted inputs

`sorted(v)` is true when each element of a list is at most the next. Integers and
floating-point numbers compare as numbers, and strings by their bytes. In a `requires` clause,
hydo generates the list in order, so no input is thrown away for it:

```rust
// hydo: requires sorted(v)
// hydo: ensures is_none(result) || v[some(result)] == x
pub fn search(v: &[u32], x: u32) -> Option<usize> {
```

## An index inside its list

`requires index < len(v)` (or `<=`) makes hydo generate the index inside the list, so that
few inputs are thrown away for it. An index into an empty list still breaks such a clause.

## Lists of one length

`requires len(a) == len(b)` makes hydo generate `b` with the length of `a`, so that no input is
thrown away for it. A shrink shortens both lists together. For half's `convert_from_f32_slice`
this contract threw away 99 % of the generated inputs before hydo read it so; now none:

```rust
// hydo: with Self = [f16]
// hydo: requires len(self) == len(src)
// hydo: ensures len(self) == len(src)
fn convert_from_f32_slice(&mut self, src: &[f32]);
```

## The `input` line

An `input` line has a path and one or more parts:

| Part | For | Example |
|---|---|---|
| `in <lo>..<hi>` or `..=<hi>` | an integer, or a floating-point number between integers | `input amount in 1..=10_000` |
| `len <lo>..<hi>` or one number | a string's bytes or a list's elements | `input v len 0..1000` |
| `chars <set>` | a string's characters | `input s len 0..200 chars ascii` |
| `like "<text>", ...` | strings like these examples | `input text like "1.2.3", "0.1.0-rc.1"` |
| `like b"<bytes>", ...` | a `&[u8]` or `Vec<u8>` like these examples | `input buf like b"GET / HTTP/1.1\r\n"` |

A set is `ascii`, `alnum`, `alpha`, `digits`, `hex`, `lower`, `upper`, `any`, or a quoted set in
which `a-z` is a range, such as `"0-9.+-"`. A `like` string is used as it is in about a third of
the inputs. In the others, a few characters are removed, added or replaced, a part is repeated,
or the end comes from another example. Give a parser a few valid inputs with `like`: random text
almost never parses. A byte string takes Rust's escapes, `\r`, `\n` and `\xff` among them. A list
of bytes takes text examples too, as their bytes, and a report shows it as a byte string.

## How values are generated without an `input` line

- The first input is the smallest value of each parameter, and the second the largest.
- An integer is a bound, a constant that a clause names or a value next to it, a small value,
  or a value of a random bit width.
- A `usize` or `isize` value stays between -1,048,576 and 1,048,576, so that a size never asks
  for terabytes. An `input` line gives the whole range.
- A list or a string is short in the first inputs and up to 256 long later. Some lists are edge
  sizes up to 4,096, such as 63, 64, and 65.
- A string's characters come from one of three sets, each in about a third of the random
  inputs. The first set is printable ASCII. The second is the set of `chars any`. The third is
  letters, a space, `_` and `-`, with characters whose case mapping changes their length or
  meaning. These are `ß`, `İ`, `ı`, `ﬄ`, `ǅ`, `Σ`, `σ`, `ς`, `ŉ`, `é`, the Kelvin sign, and two
  combining marks (U+0301 and U+0307). The capital U+10400, outside the Basic Multilingual
  Plane, is among them too. A `chars` part of an `input` line replaces the three sets.
- Some lists repeat one value with a few others among it.
- A floating-point number is 0, -0, 1, -1, a half, the type's smallest or largest value, an
  infinity, NaN, a small fraction, a value of the `in` range, or any bits. In about one input
  in six, two values of one width are made equal, or both NaN with other bits, as a comparison
  of two floats needs.
- A value that a clause compares with a list's element is put at a random place of the list.

## The verdicts

| Verdict | Meaning | Exit status |
|---|---|---|
| `held` | Every clause held, and the run tested enough. | 0 |
| `broken` | A clause broke, or a call panicked, ended the process or did not return. | 1 |
| `too few inputs` | Fewer calls than `--min-inputs` (default a tenth of `--cases`). | 3 |
| `not judged` | hydo decided a clause on fewer calls than `--min-reach`. | 3 |
| `vacuous` | A clause held without being checked: see below. | 3 |

A clause is undecided on a call when it reads a value of a type that hydo does not read, such as
a structure of another crate without a `read` line. Such a value shows as `<not read>`. A
comparison of a list, an option or a structure that holds such a value is undecided too, and
the reason names the type that hydo does not read. A clause that is decided on fewer than
`--min-reach` calls (default 1 % of `--cases`) gives `not judged`, with the reason.

A clause `a || ... || z` holds without checking `z` when a part before it is true. The report
counts the calls on which `z` decided. Fewer than `--min-reach` such calls make the run
`vacuous`. When the inputs reached `z` and hydo could not decide it, the verdict is `not
judged` instead. The report then names the value that hydo does not read, with its type, and
the inputs need no other shape. When calls panicked, the clause line says `not reached` instead: the calls that
panicked may be the ones that reach `z`, so the verdict is the panic. Each `too few inputs`,
`not judged` and `vacuous` verdict ends with a `to fix:` line. `--min-inputs 0` and
`--min-reach 0` turn the checks off.

A contracted function that hydo does not fuzz also makes the exit status 3, when nothing broke.
Such a function is refused (see "What is not fuzzed"), or its test module did not build.
`--allow-skipped` keeps the exit status of the functions that ran.

## Write a property function

A property that needs a call, such as a round trip, is a function in the crate's `#[cfg(test)]`
module with `// hydo: ensures result`. Return `Option<bool>`, and `None` for an input that the
property cannot use. hydo then counts the `Some` answers like the last part of a clause.

```rust
// hydo: input text like "1.2.3", "0.0.0-alpha.1+build.5"
// hydo: ensures result
pub fn display_roundtrip(text: &str) -> Option<bool> {
    let v = Version::parse(text).ok()?;
    Some(Version::parse(&v.to_string()).ok() == Some(v))
}
```

## What is not fuzzed

`hydo fuzz --list` prints each contracted function and why it is not fuzzed. These are refused:

- a method of a trait without a `with Self` line;
- a generic function, or a method of a generic `impl` block or trait, without a `with` line for
  each type parameter and each const generic parameter. A type parameter bound by `Fn`,
  `FnMut` or `FnOnce` needs none;
- a parameter of a type that holds a pointer, or whose structure has a private field of another
  module, without a `build` line;
- a closure whose result hydo cannot generate, and a closure of a generic `impl` block's type
  parameter;
- a parameter of a generic structure with a private field or a lifetime parameter without a
  `build` line, a `char`, or a map.

A result of such a type is read in part, and a clause that reads the rest is undecided. hydo
builds the test module in a `#![no_std]` crate too. The copy keeps the crate's `[lints]`
tables, and the test module allows no lint that the crate forbids.

## When a test module does not build

A function whose test module does not build is reported, and the other functions run. The
test module is one line that hydo writes into its copy of the crate, so the compiler's line
and column there mean nothing in your files. The report names instead the line of your
contract or the part of the function that caused the error. It adds the compiler's labels at
the test module's own text, and the compiler's `help:` and `note:` lines:

```text
hydo: fuzz: mycrate::code_of (src/lib.rs:52) is not fuzzed: its harness did not build: error[E0308]: mismatched types
    caused by the line `// hydo: build code::Code from code::Code::new(x: u16)` (src/lib.rs:49): the harness builds the value by calling `code::Code::new(...)` with the arguments that the line names
    at `c.0`: expected `u8`, found `u16`
    help: you can convert a `u16` to a `u8` and panic if the converted value doesn't fit: `.try_into().unwrap()`
    the compiler's whole output: hydo-out/fuzz/harness/code_of.log (--show-harness prints it)
```

A cause is a `build`, `read` or `with` line, a parameter and how the test module passes it,
the call, or a generated closure. `DIR/harness/<function>.log` keeps the whole output of the
failed build, and `DIR/harness/<function>.rs` the test module's text. `--show-harness` prints
the log. `DIR/build.log` keeps every build of the run, each under a numbered heading.

hydo's copy of the crate allows dead code, with `#![allow(dead_code)]` on the first line of
its root file. A helper that only the functions that `--fn` leaves out call then does not stop
a crate that denies warnings. `--features F` builds the crate with `--features F`, such as
`--features parsing,formatting` for time's parsers.

When the copy does not build without any test module, hydo builds the crate with its own
`Cargo.toml` to find the cause. If the crate builds, the message says that hydo's copy is the
cause and names the first error. The message gives the first error's place in your files. For
a member of a workspace without a `Cargo.lock`, hydo cannot build the crate apart. The message
then names the member and the workspace's folder. It says that an error at that place is in
the crate's own code, such as its test module, and gives the command that tells:
`cargo test -p <member> --no-run` in the workspace's folder.

## Choose the functions

`--fn NAME` takes a function's name or the end of its path (`hex::encode`). With `::` after it,
it takes a module, a type or a trait: `--fn codec::` fuzzes every contracted function of the
module `codec`, `--fn Ledger::` every method of `Ledger`, and `--fn Engine::` every method of
`impl Engine for ...` and of the trait `Engine`. A function's name starts with the crate's
name as Rust paths write it, `ordered_float::`, not the package's. A name that names no function but a
module, such as `--fn hydo_props`, selects that module's functions, and a line says so.

## See the inputs

`--show-inputs N` prints N of each function's inputs, spread from the first to the last, and
keeps them in `fuzz.json` as `samples`:

```text
hydo:   input 1 of 3: buf = b"0\r\n\r\n"
hydo:   input 2 of 3: buf = b"1a;ex\n"
hydo:   input 3 of 3: buf = b"ff \r\n"
```

## Fuzz under faults, inside the VM

A function that writes files, syncs them, or reads the clock can fail only under faults.
`hydo fuzz --vm` makes each call inside hydo's virtual machine while hydo injects faults:

- power cuts, with `disk=cut` and `C=<step>`, or `cf=<k>` at the disk's k-th flush on hvx;
- disk errors (`disk=ioerr:<p>`);
- clock steps (`T=<step>:<seconds>`);
- pauses of the calls (`P=workload@<step>~<ms>:<ms>`);
- a power cut, a clock step, a pause, or a kill inside one call (section "Faults inside a
  call").

```sh
hydo fuzz --vm path/to/crate
```

hydo builds the harness for the guest as a static musl program, writes a project whose workload
makes the calls, builds its image, and runs it under each schedule. The first schedule has no
fault; the others draw one fault each. Each call runs in an empty folder of its own on the data
disk, so the function opens its files relative to the current folder. Its `ensures` lines judge
it as natively. A failure that came also under the schedule without a fault gets a note: the
function or its check is wrong before any fault meets it.

The harness's target must be installed: `rustup target add x86_64-unknown-linux-musl` for hvx
and KVM, or `aarch64-unknown-linux-musl` for the Mac engine. It ran on hvx and on the Mac
engine; KVM has not run it yet. Only hvx has `cf=`, so on the Mac engine a power cut comes before
a step (`C=`) or inside a call.

### What survives a power cut: `durable`

`// hydo: durable <check>` names a property function that runs after the run. It runs in the
recovery boot after a power cut, and at the end of a run without one. hydo calls it once for
each call that started, in that call's folder:

- its parameters are the function's parameters, then the call's result as an `Option`;
- `None` means that the call did not return: the cut came in the middle of it, or it panicked;
- it returns `bool`, or `Option<bool>` with `None` for a call that it cannot judge;
- it carries `// hydo: ensures result`, and native `hydo fuzz` does not fuzz it on its own.

```rust
// hydo: input text len 1..=300
// hydo: durable save_survives
pub fn save(text: String) -> Result<(), String> { ... }

/// After the run: a save that returned `Ok` left the whole text.
// hydo: ensures result
pub fn save_survives(text: String, done: Option<Result<(), String>>) -> Option<bool> {
    match done {
        Some(Ok(())) => Some(std::fs::read("saved.txt").is_ok_and(|b| b == text.as_bytes())),
        _ => None,
    }
}
```

### Faults inside a call

A clock step or a pause between two workload steps never meets a call, because each call ends
inside its step. A function that measures a timeout with the wall clock, or that waits for a
thread it started, fails only when the fault comes while the call runs. Most drawn schedules put
the fault inside one call:

```text
disk=none | run E=2.3 A=fuzz.call+20:stop-workload/hydo-call#2-1000
```

- `E=2.3` names call 3 of workload step 2. That call names its thread `hydo-call` and writes the
  event `fuzz.call` to the console when it starts. The threads that the function starts inherit
  the name.
- `A=fuzz.call+20:<action>` is a reaction (`hydo guide faults`): 20 ms of guest time after the
  event, hydo does the action.

The actions are these:

| Action | What it does to the call |
|---|---|
| `cut` | cuts the power |
| `clock+<s>`, `clock-<s>` | steps the clock forward or back by `s` seconds |
| `stop-workload-<ms>` | stops the call's process for `ms` milliseconds |
| `stop-workload/hydo-call#2-<ms>` | stops the first thread that the function started; the caller runs on |
| `kill-workload/fuzz-call` | kills the call's process, which runs as `fuzz-call` |

A killed call has no answer, so hydo does not judge its `ensures` lines. Its durability check
runs, with `None` for the result. The run's line says whether the fault came inside the call:
`the fault came inside call 2.3`. The fault comes 1 to 13 ms later than the delay says. On hvx,
a clock step drawn 20 ms after the start came 29 to 32 ms after it. One drawn 100 ms after the
start came 109 to 113 ms after it.

The shrink keeps the fault inside the call. It runs each smaller input in a run of its own, with
`E=` naming that input's call.

To test a function that depends on time or threads, name it and the kinds of fault:

```sh
hydo fuzz --vm path/to/crate --fn count_twice --faults pause
```

### The report

Each failure names the schedule it came under, the input, and what the call or the check gave.
hydo then shrinks the input in a run of one step under the same faults and prints the command
that replays the smallest one:

```text
hydo: durability check broken: fuzz_vm::save: fuzz_vm::save_survives: ensures result (line 161) on 750 calls in 4 runs, first under `disk=cut dseed=b6a3ee203ddfdea3 | run C=14`
hydo:   input: text = "A"
hydo:   gave: result = Some(false)
hydo:   smallest input (1 shrink step): text = ""
hydo:   again: hydo run .../shrink1-2/hydo-out/explore-ob-fuzz-vm-shrink1-2-disk.img 'disk=cut dseed=b6a3ee203ddfdea3 | run C=1'   (step 0 call 0; gave the same fingerprint, dd2260cccca16aa8)
```

The image holds the inputs, and the schedule holds the faults, so `hydo run` of the two repeats
the run exactly. `vm.json` in `--out DIR` (default `hydo-out/fuzz-vm`) holds the whole report.
The options are in `hydo fuzz --vm --help`: `--schedules N` (default 12), `--schedule S` for one
schedule, `--faults cut,disk,clock,pause,kill`, `--cases N` (default 200) and `--batch N` (calls
per workload step, default 25). `--run-timeout S` stops a `hydo run` that takes longer than `S`
seconds on this machine, and the fuzzing goes on without its answers. The default is 600
seconds plus 2 per call of the image.

## Long runs

Natively, each call runs in an empty folder of its own, which hydo removes after the batch. A
function that writes files therefore leaves none in the current folder.

A run prints a progress line on standard error every 5 seconds. A call that does not return
within `--timeout` (default 10 seconds) is a finding. After 3 such calls, the function's other
inputs are not run, and a shrink stops after 3 such calls or a minute. `--json` prints only the
JSON report on standard output.
