# `hydo fuzz` tried by a new user, third round

This page records a third evaluation of `hydo fuzz`, after Milestone 307 added enumerations,
`matches()`, `with Self =`, `build ... from`, `read ... as`, and `sorted()`. `hydo fuzz` tests
one Rust function at a time from `// hydo:` comments that state what the function requires and
ensures. The evaluator played a Rust developer who had never used hydo, and learned the tool only
from `hydo guide fuzz` and `hydo fuzz --help`. The evaluator read the first two evaluations
([M280](function-fuzzing-evaluation.md) and [M288](function-fuzzing-evaluation-2.md)) only for
their method and their crates, and did not read hydo's source.

The runs used the `hydo` binary built with `cargo build --release -p hydo` from `main` at
`bc91ab75`, on the Mac, on 2026-11-18. No VM was used. Every run used the default 1,000 inputs
and seed 1, unless the text names other values.

## The verdict

A Rust developer would keep using `hydo fuzz` for property functions in a crate's test module,
and for contracts on simple functions over integers, strings, lists and the crate's own
enumerations. The evaluator fuzzed six mature crates for about 20 minutes. hydo reproduced 9 bugs
that other people had already reported upstream. Each came with a minimal input and a replay
command that worked. It found no new candidate.

The developer would still not write most contracts on the crate's API itself. Of the 73 library
functions that the evaluator contracted, hydo fuzzed 57. It refused 13 and could not build the
harness of 3. Of the
57, 10 gave no judgment: 8 because hydo does not read a trait's `Self::Owned` result, and 2
because a built value's fields are not read. One broken verdict was hydo's own error. What stops
the developer, worst first:

1. One clause gave a false `broken` verdict. An element of a list of built values compares as
   its constructor's arguments, so `result[0] == self[0]` broke on every call with a slice that was not empty.
2. Common shapes of a Rust API still cannot carry a contract:
   - const generic parameters, in every method of arrayvec;
   - a `&'static` parameter, in percent-encoding's encoders;
   - a parameter that takes a closure, in half of either's methods.
3. Two enumerations with one name in two modules make hydo read the wrong one. humantime's
   `parse_duration`, the crate's main function, could not be fuzzed at all.
4. The default strings hold only three characters outside ASCII (`é`, `€`, and an emoji, U+1F600). On heck, a
   case conversion library, a known idempotence bug held for 50,000 calls until the evaluator
   named other characters.
5. A `read` line fails on ordinary crates. In a `#![no_std]` crate `by to_string()` does not
   build, `by collect()` needs a type that the line cannot give, and a generic type name such as
   `ArrayVec<u8, 8>` is refused.

## The crates

The evaluator picked six popular crates of different kinds that the earlier evaluations did not
use. Each crate came from its latest release: a git clone at its release tag, or the crates.io
archive for heck, whose repository has no tags.

| Crate | Version | Kind | Library contracts | Fuzzed | Property functions |
|---|---|---|---|---|---|
| heck | 0.5.0 | text: case conversion | 8 | 8 | 12 |
| half | 2.7.1 | numeric: 16-bit floats | 19 | 19 | 14 |
| either | 1.19.0 | a generic enumeration with a trait-heavy API | 17 | 16 | 3 |
| humantime | 2.4.0 | parsing: durations and RFC 3339 times | 8 | 7 | 11 |
| percent-encoding | 2.3.2 | encoding, a member of the `rust-url` workspace | 9 | 7 | 8 |
| arrayvec | 0.7.8 | data structure with const generics | 12 | 0 | 6 |

A library contract is a set of comments on a public function or method of the crate. A property
function is a function in the crate's `#[cfg(test)]` module with `// hydo: ensures result`, as
the guide teaches for round trips and models.

## Time to the first useful run

A useful run is the first run that judged at least one function. The time counts from the first
look at the crate's source, and includes reading the documentation and writing the contracts.

| Crate | Time | Runs before the first useful run |
|---|---|---|
| heck | about 2 minutes | 0 |
| half | about 1.5 minutes | 0 |
| either | about 1 minute | 1: `old(self).Left.0` refused, the message said what to write |
| humantime | about 2.5 minutes | 2: `Error::UnknownUnit { .. }` refused, then the wrong `Error` (friction 3) |
| percent-encoding | about 1.5 minutes | 2: the evaluator's own property code did not build |
| arrayvec | about 1 minute | 1: `build ArrayVec<u8, 8>` refused (friction 5) |

The evaluator is a coding agent, so a person writes the contracts more slowly. Each build and run
took less than a minute.

## The results per crate

### The crate `heck` 0.5.0

heck converts text between cases: snake case, title case, and six others. Its conversions are methods
of traits implemented for `str`, so every contract used `with Self = str`.

| Function | Contract | Verdict | Note |
|---|---|---|---|
| 8 trait methods, `to_snake_case` to `to_train_case` | `self != "" \|\| result == ""` | vacuous | the result `Self::Owned` is not read (friction 2) |
| `hydo_props::*_is_idempotent`, 7 cases | a conversion of a converted text changes nothing | 1 held, 6 broken | see below |
| `hydo_props::snake_has_no_empty_word` | no leading, trailing or doubled `_` | held | |
| `hydo_props::kebab_is_snake_with_dashes` | kebab case is snake case with `-` | held | |
| `hydo_props::snake_of_kebab` | snake case of the kebab case is the snake case | broken | heck issue 71 |
| `hydo_props::title_words_are_snake_words` | title case has the words of snake case | broken | heck issue 70 |
| `hydo_props::display_matches_method` | `AsSnakeCase` writes what `to_snake_case` returns | held | |

With the default strings, only the two camel case properties broke, at `s = "a_a"`:
`"a_a"` becomes `"AA"`, and `"AA"` becomes `"Aa"`. heck documents that consecutive capitals are
one word, and issue 36 asks for the other choice. The evaluator then gave the properties
`input s len 0..=12 chars "a-zA-Z ßΣσİǅﬄ́_-"`, and six more broke:

- `"İ"` becomes `"i\u{307}"` in snake case, and the second pass drops the combining dot. That is
  issue 71: a combining mark is not alphanumeric, so heck treats it as a word boundary.
- `"ﬄ"` becomes `"FFL"` in title case and train case, and then `"Ffl"`. That is issue 70, which
  reports the same for `"ß"`.

Before the `chars` line, `title_is_idempotent` held on 50,000 calls. `--show-inputs 12` showed
why: the default strings mix ASCII with `é`, `€`, and the emoji U+1F600, which have no other case.

### The crate `half` 2.7.1

half's `f16` is `pub struct f16(u16)`. The harness sits in the struct's own module, so hydo
generated the private field, and clauses read it as `self.0`. The language has no bit operators,
so a NaN test is `(self.0 % 32768) / 1024 == 31 && self.0 % 1024 != 0`, written again in each
clause that needs it.

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `from_bits`, `to_le_bytes`, `from_be_bytes` | the bits and the bytes | held | |
| `from_f32`, `from_f64` | NaN stays NaN, at least 65,520 gives an infinity | held | |
| `to_f32`, `to_f64` | a NaN only from NaN bits, the infinity for bits `0x7C00` | held | |
| `is_nan`, `is_infinite`, `is_finite`, `is_normal` | each result from the exponent and the mantissa | held | |
| `signum`, `copysign` | the documented sign rules, also for `-0` | held | |
| `max`, `min` | "If one of the arguments is NaN, then the other argument is returned" | broken, then held | the contract was wrong: see below |
| `total_cmp` | `matches(result, Equal)` exactly for equal bits, a negative sign first | held | |
| `<[f16] as HalfFloatSliceExt>::reinterpret_cast` | each `u16` is the bits of its `f16` | broken | hydo's error (friction 1) |
| `<[f16] as HalfFloatSliceExt>::convert_from_f32_slice` | `requires len(self) == len(src)` | held | 99.2 % of the inputs broke `requires` |
| `<[f16] as HalfFloatSliceExt>::convert_to_f32_slice` | no `requires` | broken | the documented panic for slices of other lengths |
| 14 property functions | round trips, the `const` twins, nearest rounding, `+` in `f32`, `clamp`, the slice conversions against the scalar ones | held | |

`max` broke at `self = f16(31745), other = f16(31746)`, two NaNs. The contract said that the
result is `self` when `other` is a NaN, which is wrong when both are. The report showed the two
values in one line.

The slice methods were first refused: `the field 0 of f16 is private to the module binary16, so
the harness cannot build the value (fuzz a function that takes its parts and builds it, such as
its constructor)`. The advice did not name the `build` line. With `build f16 from
f16::from_bits(bits: u16)` and `read f16 as u16 by to_bits()`, all three ran.

### The crate `either` 1.19.0

`Either<L, R>` is a generic enumeration, and most of its methods are in `impl<L, R> Either<L,
R>`. Each contract named `with L = i32` and `with R = u8`. Blocks such as `impl<L, R>
Either<Option<L>, Option<R>>` and `impl<T> Either<T, T>` took the same lines.

| Function | Contract | Verdict |
|---|---|---|
| `is_left`, `is_right` | `result == matches(self, Left(_))` | held |
| `left`, `right` | `Some` exactly for the side, with its value | held |
| `flip` | the side changes and the value stays | held |
| `left_or`, `right_or`, `left_or_default` | the side's value, else the other value | held |
| `unwrap_left` | `requires matches(self, Left(_))` | held, 49.5 % of the inputs turned away |
| `left_and` | `with S = i64`, the documented rules | held |
| `factor_none`, `factor_err` | `None` or `Err` exactly for `Left(None)`, `Right(Err(_))` | held |
| `into_inner`, `either_into` | the value of either side | held |
| `From<Result<R, L>>::from` | `Right` exactly for `Ok` | held |
| `<i32 as IntoEither>::into_either` | `Left` exactly when `into_left` | held |
| `map_left` | takes a closure | refused |
| 3 property functions | `map_left` with three functions, `Left` before `Right`, `Result` round trip | held |

The first `--list` refused `old(self).Left.0` with `a field is read from a name (l.balance,
some(o).x), not from an expression`. `old(self.Left.0)` worked. `map_left` was refused with
`name the type to fuzz it with, for example // hydo: with F = i64`, which cannot work for a
closure. `with F = fn(i32) -> i32` gave `hydo cannot generate a value of fn(i32) -> i32; a //
hydo: build F from ... line names a function that builds one`, and the `build` line refuses a
type with spaces in its name.

### The crate `humantime` 2.4.0

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `parse_duration` | the empty and the `"0"` inputs, the error offsets inside the text | not fuzzed | friction 3 |
| `format_duration` | `build Duration from Duration::new(...)`, read by `to_string()` | held | |
| `parse_rfc3339`, `parse_rfc3339_weak` | the length, `matches(result, Err(Error::InvalidFormat))` | held | |
| `format_rfc3339`, `_seconds`, `_millis`, `_nanos` | the text's length, for a time that a helper builds | held | |
| `strict_accepts_only_its_format` | `parse_rfc3339` accepts only the documented grammar | broken | humantime issue 78 |
| `weak_refuses_other_offsets` | "Only UTC is supported": another offset is refused | broken | humantime issue 67 |
| `smart_format_any_time` | any time after the epoch can be formatted | broken | humantime issue 68 |
| `number_needs_unit` | `"1 2s"` is not one span of 12 seconds | broken | humantime issue 69 |
| `fraction_of_hour` | `"0.0001h"` parses as 360 ms | broken | humantime issue 82 |
| 6 other property functions | round trips of both formats, the wrapper type, spans that add | held | after one wrong property was fixed |

humantime has two enumerations named `Error`, one in `date.rs` and one in `duration.rs`. A
clause of `parse_duration` that named `Error::Empty` was refused with `Error has no variant
Empty; its variants are OutOfRange, InvalidDigit, InvalidFormat`, which are the other module's
variants. A full path, `crate::duration::Error::Empty`, gave the same message. With every
variant removed from the clauses, the harness still did not build: `no variant or associated
item named OutOfRange found for enum duration::Error`.

The shrinks were exact. `smart_format_any_time` shrank to `secs = 253402300800`, the first second
of the year 10000, where `to_string()` panics with `a Display implementation returned an error`.
`strict_accepts_only_its_format` shrank to `s = "2018-02-14T00:20:07ZZ"`. A helper of the
evaluator's that adds seconds to the epoch also panicked, at `secs = 9223372036854775808`; the
report listed it as a second failure.

The first `spans_add` broke at `a = "7m", b = "0"`. `parse_duration("0")` is a special case, and
`"7m 0"` is an error. The documentation says that each span is a number and a unit, so the
property was wrong.

### The crate `percent-encoding` 2.3.2

percent-encoding is a member of the `rust-url` workspace, and `hydo fuzz` ran in its folder. The
crate is `#![no_std]` with `alloc`.

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `AsciiSet::add`, `AsciiSet::remove` | each chunk of the mask grows or shrinks | broken | a panic at `byte = 128`: `rust-url` issue 1132 |
| `AsciiSet::union`, `AsciiSet::complement` | the chunks of the mask | not judged | a built value's fields are not read |
| `percent_encode_byte` | `len(result) == 3` | held | |
| `percent_encode`, `utf8_percent_encode` | the output between the input's length and three times it | not fuzzed | the harness did not build |
| `percent_decode`, `percent_decode_str` | the output between a third of the input and the input | held | read through a helper method |
| 8 property functions | decoding the encoding, the escapes exactly, the three output forms, UTF-8, `size_hint`, `add` and `remove`, `!` | held | |

The encoders take `ascii_set: &'static AsciiSet`. The harness built the set from a `build` line,
and failed with `error[E0597]: a1 does not live long enough ... argument requires that a1 is
borrowed for 'static`.

The `read` lines needed three tries each:

- `read PercentEncode as String by to_string()` failed with `no method named to_string found`,
  because the harness of a `#![no_std]` crate does not import `ToString`.
- `read PercentDecode as Vec<u8> by collect()` failed with `error[E0282]: type annotations
  needed`, and `by collect::<Vec<u8>>()` is refused by the line's syntax.
- A method that the evaluator added to the crate for hydo, `hydo_bytes()`, worked for both.

When the first harness failed with `error[E0308]: mismatched types`, only that line reached the
report. `build.log` held the output of the later build that succeeded, so the evaluator could
not see which types mismatched.

### The crate `arrayvec` 0.7.8

| Function | Contract | Verdict |
|---|---|---|
| 12 methods of `ArrayVec<T, CAP>`: `push`, `try_push`, `insert`, `try_insert`, `pop`, `remove`, `swap_remove`, `pop_at`, `truncate`, `try_extend_from_slice`, `len`, `remaining_capacity` | the documented results and lengths | refused: `a const generic parameter (const CAP): not fuzzed yet` |
| `hydo_props::model` | 40 operations on an `ArrayVec<u8, 8>` and on a `Vec` agree | held |
| 5 other property functions | `into_inner`, `TryFrom<&[T]>`, and three of `ArrayString<16>` | held |

The section "What is not fuzzed" of `hydo guide fuzz` does not name const generic parameters. With a
bug planted in `swap_pop` that skips the swap when the length is 7, `model` broke on 4 of 1,000
calls. The shrink gave 11 operations: `ops = b"\x00\x00\x00\x00\x02\x02\x00\x00\x00\x02\x0e"`.

## The frictions, ranked by how much they cost a user

1. A false `broken` verdict. In half, `ensures len(self) == 0 || result[0] == self[0]` broke at
   `self = [f16::from_bits(7)]` with `result = [7]`. An element of a list of built values reads
   as its constructor's arguments, so `self[0].bits` held and `old(self[0]) == result[0]` broke.
   The comparison of such a record with an integer gives `false` without a message. A user who
   trusts the verdict files a bug against the crate.
2. Results that hydo does not read give the wrong verdict and the wrong advice. heck's eight trait
   methods return `Self::Owned`, which shows as `<not read>`. Each was `vacuous` with `to fix:
   give the inputs the shape that reaches the clause's last part`, although the empty input came
   82 times; the result was the problem.
3. A name used by two enumerations. humantime's `duration::Error` resolved to `date::Error`. The
   refusal named the other type's variants, a full path did not help, and the harness of
   `parse_duration` failed to build even without a variant in its clauses.
4. Const generics, `&'static` parameters and closures. Every method of arrayvec was refused, the
   main functions of percent-encoding failed to build, and either's methods with closures were
   refused with advice (`with F = i64`) that cannot work.
5. The default strings. Text crates need characters that change length or meaning with their case
   (`ß`, `İ`, `ﬄ`, combining marks). With them, hydo found heck's issues 70 and 71 within 1,000
   calls. Without them, 50,000 calls held.
6. The `read` line. `to_string()` fails in a `#![no_std]` crate, `collect()` needs a type, a
   turbofish is refused, and a generic type name such as `ArrayVec<u8, 8>` is refused in a
   `build` line. Each needed a helper method in the crate.
7. A harness that does not build shows only the compiler's first line, and `build.log` can hold
   another build's output.
8. The refusal for a private field (`fuzz a function that takes its parts and builds it`) does not
   name the `build` line that the guide offers for exactly that case.
9. The language has no bit operators and no named predicates, so half's NaN test is written out
   in 10 clauses.
10. Two independent lists rarely have one length: `requires len(self) == len(src)` turned away
    99.2 % of the inputs. The run still passed `--min-inputs` with 404 calls.
11. Smaller ones:
    - the pattern `Error::UnknownUnit { .. }` is refused with `unexpected {`, although
      `Error::UnknownUnit` works;
    - `--fn hydo_props` does not select a module's functions;
    - an error in the user's own test module stops every function, with a message about hydo's
      copy (`hydo could not build the crate apart`), because the crate is a workspace member.

What worked well: `with Self = str` and `with Self = [f16]`, `with` lines on generic `impl`
blocks, `matches()` on the crate's enumerations and on `Ordering`, `build` and `read` lines for
the standard library's `Duration` and `SystemTime`, the exact shrinks, and the `again:` lines.
Every replay gave its run's verdict.

## The candidates

None of the findings is new, and nothing was posted. Each one matches an issue that other people
reported upstream before this evaluation:

| Crate | What hydo found | Smallest input | Upstream |
|---|---|---|---|
| heck | camel case of a camel case text changes it | `s = "a_a"` | issue 36, open (documented word rule) |
| heck | title and train case of `ﬄ` are not idempotent | `s = "ﬄ"` | issue 70, open |
| heck | snake and kebab case drop a combining mark on the second pass | `s = "İ"` | issue 71, open |
| humantime | `parse_rfc3339` accepts bytes after `Z` | `s = "2018-02-14T00:20:07ZZ"` | issue 78, closed as not planned |
| humantime | `parse_rfc3339_weak` reads any `+` suffix after a fraction as UTC | `s = "4018-02-14 00:28:07.+aaaaa"` | issue 67, open |
| humantime | formatting a time in the year 10000 panics in `to_string()` | `secs = 253402300800` | issue 68, open |
| humantime | `"1 2s"` parses as 12 seconds | `n = 1, m = 0` | issue 69, open |
| humantime | `"0.0001h"` is refused, although it is 360 ms | `k = 1` | issue 82, open |
| percent-encoding | `AsciiSet::add` and `remove` panic for a byte of 128 or more | `byte = 128` | `rust-url` issue 1132, open |

## The fixes this evaluation calls for

In the order of the frictions:

1. Compare a built value in a list through its `read` line, or refuse the comparison; never give
   `false` for a record against a number.
2. Read a trait method's `Self::Owned` and other associated results through the `with Self`
   type. Say `not judged` with the unread value as the reason when a clause cannot be decided.
3. Resolve a type name by the module that the function's file imports, and accept a full path.
4. Generate const generic parameters from a `with CAP = 8` line, leak a built value for a
   `&'static` parameter, and generate a closure from a small set of functions (`build F from`).
5. Add characters that change under case mapping to the default strings, and say in the guide
   which characters they hold.
6. Import `alloc::string::ToString` in a `#![no_std]` harness, accept a turbofish in a `read`
   line, and accept a generic type in a `build` line.
7. Keep each failed harness's whole compiler output in `build.log`.
8. Name the `build` line in the refusal for a private field.
