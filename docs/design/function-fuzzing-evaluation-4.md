# `hydo fuzz` tried by a new user, fourth round

This page records a fourth evaluation of `hydo fuzz`, after Milestones 318 and 326 fixed what the
[third evaluation](function-fuzzing-evaluation-3.md) found. `hydo fuzz` tests one Rust function
at a time from `// hydo:` comments that state what the function requires and ensures. The
evaluator played a Rust developer who had never used hydo, and learned the tool only from `hydo
guide fuzz` and `hydo fuzz --help`. The evaluator read the three earlier evaluations only for
their method and their crates, and did not read hydo's source.

The runs used the `hydo` binary built with `cargo build --release -p hydo` from `main` at
`7b3d309b`, on the Mac, on 2026-11-19. No VM was used. Every run used seed 1 and 1,000 inputs,
unless the text names other values.

## The verdict

A Rust developer would keep using `hydo fuzz` for property functions in a crate's test module,
and for contracts on functions over integers, floating-point numbers, strings and the crate's
own enumerations. The evaluator fuzzed six popular crates that no earlier evaluation used. All
60 property functions ran, and each failure came with a minimal input and an `again:` command
that replayed it. hydo reproduced 3 bugs that other people had already reported upstream, and
found no new candidate.

The developer would still not write most contracts on three kinds of API. They are a generic
container, a parser whose error type has private fields, and a type that only a fallible
constructor builds. Of the 88 library functions that the evaluator contracted, hydo fuzzed 71. It
refused 8 and could not build the harness of 9. Of the 71, 9 gave no judgment because hydo did
not read a value that the clause names, and one `broken` verdict was hydo's own error. Four of
the six crates needed a helper constructor in the test module before their contracts ran. What
stops the developer, worst first:

1. One clause gave a false `broken` verdict. smallvec's `into_vec` returns `Vec<A::Item>`, whose
   elements hydo does not read, and `result == old(self)` compared them as unequal.
2. A parameter or a result of a type parameter's associated type (`A::Item`) is not read.
   smallvec's 18 contracts gave 9 refusals or harness failures and 3 functions not judged.
3. A parser's `Err` value stops the harness. semver's `Error` holds a private field of a type
   from a private module, and 5 of semver's 9 contracts did not build.
4. Constructors: a `build` line without arguments does not build, a constructor that returns a
   `Result` cannot be named, and the refusals proposed lines that do not build. They proposed an
   ambiguous `from`, an `unsafe` function, and a macro's `$src` placeholder.
5. The type parameters of an `impl` block are not put into a parameter's type
   (`&OrderedFloat<T>`), and `Self::Output` came from another `impl`'s `type Output`.

## The crates

The evaluator picked six popular crates of different kinds. Each came from its latest stable release
on crates.io, unpacked by cargo into its registry folder and copied to a scratch folder.

| Crate | Version | Kind | Library contracts | Fuzzed | Property functions |
|---|---|---|---|---|---|
| semver | 1.0.28 | a parser of Cargo's versions and requirements | 9 | 4 | 14 |
| base64 | 0.23.1 | encoding, with a trait for engines | 11 | 11 | 10 |
| smallvec | 1.16.5 | a generic data structure | 18 | 9 | 7 |
| time | 0.3.55 | dates, times, and durations | 23 | 23 | 10 |
| textwrap | 0.16.4 | text: wrapping, indenting, columns | 11 | 11 | 10 |
| ordered-float | 5.5.1 | floats with a total order, a trait-heavy API | 16 | 13 | 9 |

A library contract is a set of comments on a public function or method of the crate. A property
function is a function in the crate's `#[cfg(test)]` module with `// hydo: ensures result`, as
the guide teaches for round trips and models. The verdicts of the library contracts, after the
workarounds that each section names:

| Crate | Held | Broken | Not judged | Vacuous | Refused | Harness did not build |
|---|---|---|---|---|---|---|
| semver | 2 | 0 | 1 | 1 | 0 | 5 |
| base64 | 11 | 0 | 0 | 0 | 0 | 0 |
| smallvec | 5 | 1 (hydo's error) | 3 | 0 | 8 | 1 |
| time | 20 | 0 | 2 | 1 | 0 | 0 |
| textwrap | 7 | 1 (known) | 3 | 0 | 0 | 0 |
| ordered-float | 12 | 1 (known) | 0 | 0 | 0 | 3 |

## Time to the first useful run

A useful run is the first run that judged at least one function. The time counts from the first
look at the crate's source, and includes reading the documentation and writing the contracts.

| Crate | Time | What came before the first useful run |
|---|---|---|
| semver | about 6 minutes | none; the property functions ran, 7 library contracts did not build |
| base64 | about 8 minutes | the refusal's advice for the engine repeated itself (friction 4) |
| smallvec | about 6 minutes | none; 8 contracts were refused for `A::Item` |
| time | about 10 minutes | none |
| textwrap | about 7 minutes | none |
| ordered-float | about 8 minutes | one property of the evaluator's did not build |

The evaluator is a coding agent, so a person writes the contracts more slowly. Each build and run
took less than 40 seconds, and a run of 20,000 inputs per function took at most 90 seconds.

## The results per crate

### The crate `semver` 1.0.28

semver parses versions such as `1.0.0-rc.1` and requirements such as `>=1.2.3, <1.8`, and
matches one against the other.

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `Version::new` | `result.major == major`, and the other two numbers | not judged | hydo's error: see below |
| `Version::cmp_precedence` | the order of the three numbers | held | |
| `VersionReq::matches` | `*` matches exactly the versions without a pre-release | held | `Version` and `Comparator` built by helpers |
| `Comparator::matches` | the documented rules of `=`, `>=`, `^` and `~` | vacuous at 1,000 and at 5,000 | one clause's case is rare; two wrong contracts first |
| `Version::parse`, `VersionReq::parse`, `Comparator::parse`, `Prerelease::new`, `BuildMetadata::new` | the grammar | harness did not build | friction 3 |
| 14 property functions | the grammar, round trips of both formats, the operators' rules, the orders of pre-releases and build metadata | held | also on 20,000 inputs |

Every function that returns `Result<_, semver::Error>` failed to build with `error[E0433]:
failed to resolve: use of undeclared type ErrorKind` and `caused by hydo's harness of
semver::Version::parse (src/lib.rs:427), at ErrorKind: a mistake of hydo's`. semver's `Error` is
`pub struct Error { pub(crate) kind: ErrorKind }`, and `ErrorKind` lives in the private module
`error`. The harness prints the error's fields by their types and does not import them. The
clauses do not read the error at all. The evaluator moved the five contracts into property
functions, which held.

`VersionReq::matches` needed a `Version` and a `Comparator`, whose `pre` field is a `Prerelease`
with a pointer inside. The refusal proposed `build VersionReq from VersionReq::default()`. A
`build` line without arguments does not build: the harness wrote `let mut c: (,) = (,);
Prerelease::default()`, and the report said `expected expression, found ,` with `a mistake of
hydo's`. `Prerelease::new` returns a `Result`, so no `build` line could name it. The evaluator
wrote helpers in the test module, `hydo_props::version(major, minor, patch, pre: &str)` and
`hydo_props::comparator(...)`. hydo accepted them in `build` lines, and both `matches` ran.

`Comparator::matches` broke twice before it held, each time on a value that the type's comments
forbid: `hydo_props::comparator(Exact, 0, None, Some(0), "")` has a patch without a minor, and
`(Exact, 2, None, None, "a")` a pre-release without a patch. Both shrinks took 2 to 4 steps, and
a `requires` line fixed the contract.

`Version::new` was not judged, with the reason ``result is a Version, which a build line builds
and no // hydo: read Version as <type> by <method>() line reads``. `Version` has public fields
`major`, `minor` and `patch` of type `u64`. The `build` line for `Version` on another function
made hydo stop reading them, and the advice said to compare an integer, which the clause does.
An `input version.pre.text like ...` line, which names a built field inside a generated
argument, was refused with `no argument pre`.

### The crate `base64` 0.23.1

base64's engines implement the trait `Engine`, whose methods have default bodies and a type
parameter `T: AsRef<[u8]>`. Each contract named `with Self = GeneralPurpose` and `with T =
Vec<u8>`.

| Function | Contract | Verdict |
|---|---|---|
| `encode`, `encode_string` | the length with and without padding | held |
| `encode_slice` | `Ok` exactly when the output is large enough, with the length written | held |
| `decode`, `decode_vec` | at most three bytes for four symbols; an `InvalidByte` and an `InvalidLastSymbol` name a byte of the input at their offset | held |
| `decode_slice`, `decode_slice_unchecked` | at most the output's length; `OutputSliceTooSmall` only below the documented estimate | held |
| `encoded_len` | the length, and `None` exactly when it does not fit in `usize` | held |
| `decoded_len_estimate` | `(n + 3) / 4 * 3` | held |
| `Alphabet::new`, `Symbol::new` | 64 symbols, printable ASCII | held |
| 10 property functions | round trips of every configuration, one text per strict encoding, `decode_slice` writes nothing past the decoded bytes, the display type and the streaming reader and writers agree with `encode` | held, also on 20,000 inputs |

The clauses read exact integers: `is_some(result) || (bytes_len + 2) / 3 * 4 >
18446744073709551615` states `encoded_len`'s overflow rule without an overflow of its own. A
field of a tuple variant reads as `err(result).InvalidByte.0`, and a struct variant matches with
`DecodeError::InvalidLastSymbol { .. }`.

Three things took retries. `GeneralPurpose` has private fields, and the refusal proposed its
constructor, `GeneralPurpose::new(alphabet: &Alphabet, config: GeneralPurposeConfig)`. With that
line the refusal said `hydo cannot generate a value of Alphabet` and proposed the same line
again. `Alphabet::new` returns a `Result`, and `GeneralPurposeConfig::new()` takes no argument,
so a helper `hydo_props::engine(url: bool, pad: bool, trailing: bool, mode: u8)` built it.
`with T = &[u8]` failed to build: the harness passed `a generated Vec<u8>` where the method takes
`&[u8]`. `with T = Vec<u8>` worked. `--fn Engine::` found no function, because the methods are
named `<GeneralPurpose as Engine>::encode`; `--fn engine::` selected them.

### The crate `smallvec` 1.16.5

`SmallVec<A: Array>` keeps up to `A`'s size inline. Each contract named `with A = [u8; 4]`,
with `build SmallVec<[u8; 4]> from SmallVec::<[u8; 4]>::from_vec(v: Vec<u8>)` and `read
SmallVec<[u8; 4]> as Vec<u8> by to_vec()`.

| Function | Contract | Verdict |
|---|---|---|
| `inline_size`, `grow`, `truncate`, `insert_many`, `dedup` | the documented lengths and contents | held |
| `pop`, `remove`, `swap_remove` | the removed element is the one at the index | not judged: `T::Item` is not read |
| `into_vec` | `result == old(self)` | broken, hydo's error |
| `retain` | the length does not grow | harness did not build |
| `from_vec`, `push`, `insert`, `from_slice`, `insert_from_slice`, `extend_from_slice`, `resize`, `from_elem` | the documented lengths and contents | refused |
| 7 property functions | a model of 16 operations against a `Vec`, the constructors, `into_inner`, `append`, the order, `dedup_by_key` and `resize_with`, `from_buf_and_len` | held |

Each method with a parameter of type `A::Item` was refused with `the parameter value: hydo cannot
generate a value of T::Item; a // hydo: build Item from <constructor>(<name>: <type>, ...) line
names a function that builds one`. The code says `A::Item`, and `Item` is a `u8` here, which no
constructor builds. `with A::Item = u8` was refused with `write with <type parameter> = <type>`.
`with Item = u8` was accepted without a message and changed nothing.

`into_vec` broke on 862 of 1,000 calls with `smallest input (4 shrink steps): self =
SmallVec::<[u8; 4]>::from_vec(b"\x00")` and `the call gave: result = [<not read>]`. The function
returned the right elements; hydo compared values it did not read. The same reading gave `not
judged` for `pop`, so the two verdicts disagree about one type of value. `retain` did not build
with ``use of undeclared type A`` in the closure that hydo generated for `FnMut(&mut A::Item) ->
bool`.

With a bug planted in `swap_remove` that skips the swap when the length is 6 and the index 1,
the contract `index == len(self) || self[index] == old(self[len(self) - 1])` broke on 25 of 1,000
calls. The shrink gave `self = SmallVec::<[u8; 4]>::from_vec(b"\x00\x00\x00\x00\x00\x01"),
index = 1` in 9 steps. The model property broke on 5 of 1,000 calls and shrank to 8 operations.

### The crate `time` 0.3.55

| Function | Contract | Verdict |
|---|---|---|
| `Date::from_calendar_date`, `from_ordinal_date`, `from_julian_day` | the documented ranges, leap years, the days of each month | held, except one clause vacuous |
| `Date::next_day`, `previous_day`, `nth_next_occurrence`, `replace_day` | the Julian day moves by one, by whole weeks; `None` only at the ends | held |
| `SignedDuration::new`, `abs`, `seconds_f64`, `whole_minutes`, `subsec_milliseconds`, `checked_add`, `checked_mul`, `saturating_add` | the nanoseconds as an `i128`, `None` exactly at an overflow | held, after one wrong contract |
| `Time::from_hms`, `UtcOffset::from_hms`, `util::days_in_month` | the documented ranges | held |
| `Month::next`, `Weekday::number_from_monday`, `number_days_from_sunday` | `December` to `January`, numbers 1 to 7 | held |
| `Month::nth_next`, `Weekday::nth_next` | `nth_next(12)` and `nth_next(7)` give the same value | not judged |
| 10 property functions | the three date forms, the next day's weekday and month, `next_occurrence`, `checked_add` of whole days, the `replace_*` methods, Unix timestamps, offsets, `std::time::Duration` | held, after two wrong properties |

`Date` was built by a helper, `hydo_props::date(jd: i32)`, because `Date::from_julian_day` returns
a `Result`, and read by `to_julian_day()`. `SignedDuration` was built by its constructor
`SignedDuration::new(seconds: i64, nanoseconds: i32)` and read as `i128` by `whole_nanoseconds()`.
With these, `ensures is_none(result) || some(result) == self + 1` states `next_day` in one line.

`SignedDuration::abs` broke at `self = SignedDuration::new(-9223372036854775808, 0)` with
`result = 9223372036854775807999999999`. The documentation says that the method saturates, so the
contract was wrong. Two properties of the evaluator's were wrong too: `checked_add` takes only
whole days, so `-1` day plus one hour is no move, and `prev_occurrence` near `Date::MIN` panics,
as documented. Each shrink was exact (`jd = 0, days = -1, hours = 1`).

Two values of one enumeration cannot be compared. `result == self` on `Month` gave `not judged`
with `a variant is compared with matches(<value>, <pattern>)`. `matches()` takes a pattern, not
a second value, so the rule needs one clause per variant. The `February` and day-29 clause was
`vacuous` with 3 of 1,000 calls; an `input` line would fix it. hydo has no way to turn on a
crate's features, so time's parsing and formatting, which are behind features, were not tested.

### The crate `textwrap` 0.16.4

| Function | Contract | Verdict |
|---|---|---|
| `fill`, `refill`, `unfill`, `indent`, `dedent`, `fill_inplace` | lengths; `fill_inplace` keeps the length, as it only replaces spaces | held |
| `core::display_width` | at most the text's bytes | held |
| `wrap_columns` | at least one line | broken: a panic, known upstream |
| `wrap` | the empty text gives one empty line | not judged: `Cow<str>` is not read |
| `Word::from`, `Options::new` | the public fields of the result | not judged: `Word<'a>` and `Options<'a>` are not read |
| 10 property functions | lines fit the width, the words stay, `fill_inplace` is `fill` with its documented options, `unfill` and `refill` undo `fill`, `indent` and `dedent`, columns of one width, indents, ANSI codes | 8 held, 2 broken |

`wrap_columns` panicked at `text = "\u{1f600}", columns = 4, total_width_or_options = 10, left_gap
= "", middle_gap = "a", right_gap = ""` with `attempt to subtract with overflow`. The padding is
`column_width - display_width(line)`, which underflows when a line is wider than its column. In a
release build the same input panics with `capacity overflow`. A word longer than the column with
`break_words(false)` panics too. The documentation names a panic only for zero columns. Pull
request 647 fixed it on 2026-09-17, after the release of 0.16.4 on 2026-09-13.

`dedent_undoes_indent` broke at `s = "a\n\t"`. A line of whitespace only that does not begin with
the other lines' indent stops the dedent: `dedent("    a\n\t\n    b\n")` keeps the four spaces. The
open pull request 654 makes `dedent` ignore such lines.

`fill_lines_fit` broke first at `text = "\u{1f600}", width = 1`, which cannot fit, and then at a
line of 41 columns for the width 40. The default wrapping algorithm lets a line overflow by one
column when that costs less than a new line. The documentation of `Penalties::overflow_penalty`
says so, so the property was too strict, and `width + 1` held on 20,000 inputs. Two other broken
properties were the evaluator's mistakes: a whitespace-only text, and a prefix that held a newline.

### The crate `ordered-float` 5.5.1

`OrderedFloat<T>(pub T)` orders every NaN after every number, and `NotNan<T>` refuses NaN.

| Function | Contract | Verdict |
|---|---|---|
| `lt`, `ge`, `cmp`, `PartialEq<T>::eq` of `OrderedFloat` | NaN is the largest value and equal to itself | held at 1,000; `PartialEq<T>::eq` broke at 20,000, known upstream |
| `abs`, `signum`, `neg`, `is_zero`, `from_str` of `OrderedFloat` | as the inner float | held |
| `NotNan::new`, `as_f32`, `neg`, `is_zero` | `Ok` exactly for a number, the value kept | held |
| `PartialEq::eq` of two `OrderedFloat`s | all NaNs equal | harness did not build |
| `NotNan`'s `add` and `div` with a float | the inner float's result | harness did not build |
| 9 property functions | equal values hash equal (`-0.0`, NaN payloads, also `f32` and `NotNan`), a total order, sorting, `NotNan`'s order, `+` panics only for NaN, the display round trip, `Sum`, conversions | held |

The refusal for `OrderedFloat<f64>` proposed `// hydo: build OrderedFloat from
OrderedFloat::from(v: $src)`, with the placeholder of the crate's `impl_ordered_float_from!`
macro. The line with `v: f64` failed with `multiple applicable items in scope`, because `From`
and `num_traits::NumCast` both have a `from`. The report named the `build` line as the cause.
`<OrderedFloat<f64> as From<f64>>::from(v: f64)` worked. For `NotNan` the refusal proposed
`NotNan::new_unchecked(val: T)`, an `unsafe` function, and the harness failed with `call to
unsafe function`. A helper `hydo_props::nn(val: f64)` worked.

Three harnesses did not build for hydo's own reasons. `fn eq(&self, other: &OrderedFloat<T>)`
failed with `cannot find type T`: the `with T = f64` line was not put into the parameter's type.
`NotNan`'s `add` returns `Self::Output`, which is `T`. hydo read it through the `read` line of
`OrderedFloat<f64>`, and the build failed with `no method named into_inner found for type f64`.

`OrderedFloat(NaN) == NaN` is `false`, while `OrderedFloat(NaN) == OrderedFloat(NaN)` is `true`.
The contract broke on 13 of 20,000 calls and held on 1,000, because two generated floats are
both NaN about once in 1,500 calls. The reports name the crate `ordered-float::`, the package's
name, where Rust paths say `ordered_float`. When `--fn eq` left out the functions that use the
helper `nn`, the crate's `#![deny(warnings)]` stopped the build on the unused helper. The message
said what to add: `#[allow(dead_code)]`.

## The frictions, ranked by how much they cost a user

1. A false `broken` verdict. smallvec's `into_vec` broke with `result = [<not read>]`: a list
   whose elements hydo does not read compared as unequal to a list of bytes. The same unread value
   gives `not judged` elsewhere. A user who trusts the verdict files a bug against the crate.
2. Associated types of a type parameter's trait. `A::Item` as a parameter is refused, as a result
   it is not read, and in a closure's argument the harness does not build. The refusal names
   `T::Item` and proposes a `build Item` line. 12 of smallvec's 18 contracts gave nothing.
3. A result whose error type has a field of a private module. semver's five parsers did not build,
   although no clause read the error. Most parsers return such an error.
4. Constructors. Four of six crates needed a helper in the test module:
   - a `build` line without arguments writes `(,)` and does not build, and the refusal proposes
     such lines (`VersionReq::default()`);
   - a constructor that returns a `Result` or an `Option` cannot be named, and parsers and date
     types have only such constructors;
   - the refusal proposed an ambiguous `from`, an `unsafe` function, a macro's `$src`
     placeholder, and for base64 the line already written.
5. Type parameters of an `impl` block in a parameter's type (`&OrderedFloat<T>`) are not
   substituted, and `Self::Output` came from another `impl`.
6. Values that hydo does not read: structures with a lifetime parameter (`Word<'a>`,
   `Options<'a>`), `Cow<str>`, and the public fields of a type that a `build` line builds
   elsewhere. Two values of one enumeration cannot be compared.
7. `with T = &[u8]` passes a `Vec<u8>`.
8. Rare combinations. Two floats are both NaN once in about 1,500 calls, so
   ordered-float's known bug held on 1,000 calls. `requires index < len(self.v)` threw away 64 %
   of the inputs, because the index is not generated inside the list.
9. Smaller ones:
   - a `with` line that names no type parameter (`with Item = u8`) is accepted without a message;
   - an `input` path through a generated argument to a built field is refused with `no argument
     pre`;
   - `--fn Engine::` does not select a trait's methods;
   - `hydo fuzz` cannot turn on a crate's features;
   - a report names the crate by its package name (`ordered-float::`);
   - `--fn` can leave a helper unused, and a crate with `#![deny(warnings)]` then does not build.

What worked well: property functions, including a 16-operation model of smallvec that caught a
planted bug; the exact shrinks; the replay lines. Helpers of the test module worked in `build`
lines (`crate::hydo_props::engine(...)`), as did a qualified constructor
(`<OrderedFloat<f64> as From<f64>>::from`). Exact integer arithmetic stated overflow rules,
`matches()` read tuple and struct variants, and floats compared exactly. Since Milestone 326 each
harness that did not build named its cause, and four of those causes were the evaluator's lines.

## The candidates

None of the findings is new, and nothing was posted. Each one matches a report or a pull request
that other people made upstream before this evaluation:

| Crate | What hydo found | Smallest input | Upstream |
|---|---|---|---|
| textwrap | `wrap_columns` panics when a line is wider than its column | `text = "\u{1f600}", columns = 4, total_width_or_options = 10, middle_gap = "a"` | pull request 647, merged after 0.16.4 |
| textwrap | a whitespace-only line stops `dedent` | `s = "a\n\t"` | pull request 654, open |
| ordered-float | `OrderedFloat(NaN) == NaN` is `false` | `self = NaN, other = NaN` | issue 143, open |

Two more were documented behavior, so the contracts were wrong: textwrap's one-column overflow of
the default algorithm (`Penalties::overflow_penalty`), and the saturation of time's
`SignedDuration::abs`.

## The fixes this evaluation calls for

In the order of the frictions:

1. Make a comparison with a value that hydo does not read undecided, so `not judged`, everywhere.
2. Resolve an associated type of a type parameter's trait (`A::Item`) through the `with` line's
   type and the trait's `impl` for it, in parameters, results, and closures.
3. Print an `Err` value that no clause reads without naming its fields' types, or import them by
   their paths.
4. Accept a `build` line without arguments. Accept a constructor that returns a `Result` or an
   `Option`, and use its `Ok` and `Some` values. Propose only lines that build: a qualified path
   for an ambiguous name, no `unsafe` function, no macro metavariable. For a nested type, name
   the type to build.
5. Put the `with` lines' types into the parameters' types of an `impl` block, and resolve
   `Self::Output` from the method's own `impl`.
6. Read structures with lifetime parameters, `Cow<str>`, and public fields of a built type, and
   compare two values of one enumeration with `==`.
7. Pass a slice for `with T = &[u8]`.
8. Generate special float values together for two parameters of one type, and an index inside
   the list that `requires` compares it with.
9. The smaller ones:
   - refuse a `with` line for a name that is not a type parameter;
   - explain a refused nested `input` path;
   - select trait methods with `--fn <Trait>::`, and take `--features`;
   - name the crate by its library name, and allow `dead_code` in hydo's copy.
