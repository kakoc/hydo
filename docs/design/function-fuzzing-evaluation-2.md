# `hydo fuzz` tried by a new user, second round

This page records a second evaluation of `hydo fuzz` before the 0.4.0 release. `hydo fuzz` tests
one Rust function at a time from `// hydo:` comments that state what the function requires and
ensures. The evaluator played a Rust developer who had never used hydo. The evaluator learned the
tool only from `hydo guide fuzz`, `hydo fuzz --help`, and `hydo guide manifest`, and did not read
the earlier evaluation or the design notes.

The runs used the `hydo` binary built with `cargo build --release -p hydo` from `main` at
`0038380d`, on the Mac, on 2026-11-17. No VM was used.

## The crates

The evaluator picked six popular crates from crates.io, one of each kind that the task named. The
six crates of the first evaluation were left out. `cargo fetch` resolved the newest versions.

| Crate | Version | Kind | Contracts on library functions | Property functions |
|---|---|---|---|---|
| strsim | 0.11.1 | string handling | 5 | 8 |
| hex | 0.4.3 | encoding | 5 | 3 |
| httparse | 1.10.1 | parser | 1 | 4 |
| bytes | 1.12.1 | data structure | 2 (both refused) | 6 |
| num-traits | 0.2.19 | numeric code with a trait-heavy API | 7 | 6 |
| chrono | 0.4.45 | calendar arithmetic and parsers | 6 | 10 |

A contract on a library function is a set of comments right before the function. A property
function is a function in the crate's `#[cfg(test)]` module with `// hydo: ensures result`, as
the guide teaches for round trips. A wrapper is a property function that calls one trait method,
so that the trait method gets a contract of its own.

In all, the evaluator wrote 26 contracts on library functions, and hydo fuzzed 24 of them. The
evaluator also wrote 37 property functions. Every run used the default 1,000 inputs and seed 1,
unless a table says otherwise.

## Time to the first useful run

A useful run is the first run that judged at least one function. The time counts from the copy of
the crate to that run, and it includes the time to read the crate and write the contracts.

| Crate | Time | Runs before the first useful run |
|---|---|---|
| strsim | about 2 minutes | 1: the harness did not build (friction 2) |
| hex | about 2 minutes | 0 |
| httparse | about 4 minutes | 2: `like` refused for a byte slice, then the harness did not build (frictions 5 and 6) |
| bytes | about 3 minutes | 0 |
| num-traits | about 2 minutes | 0 |
| chrono | about 3 minutes | 0 |

The evaluator is a coding agent, so a person writes the contracts more slowly. The builds and runs
themselves took 1 to 15 seconds each.

## The results per crate

### The crate `strsim` 0.11.1

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `hamming` | `is_ok(result) \|\| len(a) != len(b)` and a bound | held | the bound's last part decided 37 calls |
| `levenshtein` | upper and lower bounds by length, ASCII inputs | held | |
| `osa_distance` | the same bounds, without `input` lines | broken, then held | the contract was wrong: see below |
| `damerau_levenshtein` | the same bounds, letters `a-d` | held | |
| `jaro` | `ensures result <= 1` | held | decided on 0 of 1,000 calls (friction 3) |
| `tests::osa_le_levenshtein` | OSA distance at most the Levenshtein distance | held | |
| `tests::damerau_le_osa` | Damerau-Levenshtein at most the OSA distance | held | |
| `tests::damerau_triangle`, `tests::osa_triangle` | the triangle inequality | held | |
| `tests::similarities_in_range` | five scores between 0 and 1 | held | |
| `tests::jaro_winkler_ge_jaro` | Jaro-Winkler at least Jaro | held | |
| `tests::sorensen_dice_symmetric` | the score is symmetric | held | |
| `tests::scores_ignore_letter_names` | renaming letters keeps three scores | broken | a real candidate: finding 1 |

The `osa_distance` contract compared a character count with `len()`, which counts bytes. hydo's
default strings hold characters outside ASCII, so it shrank the failure to `a = "é", b = ""`.
That is the evaluator's mistake, and the report made it plain in one line.

### The crate `hex` 0.4.3

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `encode`, `encode_upper` | `with T = Vec<u8>`, `len(result) == 2 * len(data)` | held | |
| `decode` | `with T = String`, hex inputs, the length and the odd-length error | held | |
| `decode_to_slice` | `with T = String`, the length on success | held | the last part decided 25 calls |
| `encode_to_slice` | `is_ok(result) == (len(output) == 2 * len(input))` | held | |
| `test::roundtrip` | `decode(encode(x)) == x`, also for upper case | held | |
| `test::slice_matches_vec` | `decode_to_slice` agrees with `decode` | held | |
| `test::error_names_the_character` | the error's character is at its index | broken | finding 5 |

### The crate `httparse` 1.10.1

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `parse_chunk_size` | bytes from 9 to 70, `is_ok(result) \|\| len(buf) > 0` | held | its `Status` result is not read (friction 7) |
| `tests::request_prefixes_are_partial` | each prefix of a complete request is partial | held | 436 complete requests |
| `tests::response_prefixes_are_partial` | the same for a response | held | 424 complete responses |
| `tests::header_names_are_tokens` | every parsed header name is an HTTP token | held | |
| `tests::chunk_size_line` | a complete chunk size line starts with a hex digit | broken | finding 2 |

The evaluator first gave `"\r\n"` as a `like` example, so the first failure was planted. Without
that example, hydo reached `"\r\n"` from the other examples on 32 of 1,000 calls.

### The crate `bytes` 1.12.1

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `BytesMut::split_to` | `requires at <= len(self)` | not fuzzed | `hydo cannot generate a value of NonNull<u8>` |
| `Buf::get_uint` | `requires nbytes <= 8` | not fuzzed | `a method of a trait` |
| `tests::bytes_split_model` | `split_to` and `split_off` agree with a `Vec` | held | |
| `tests::bytes_slice_model` | `slice` and a slice of a slice agree with a `Vec` | held | |
| `tests::bytes_mut_ops_model` | 8 `BytesMut` operations in sequence agree with a `Vec` | held | also 50,000 calls with seed 7 |
| `tests::uint_roundtrip`, `tests::int_roundtrip` | `put_uint` then `get_uint`, 1 to 8 bytes | held | |
| `tests::get_uint_short_input` | `get_uint` advances by `nbytes` | held | |

### The crate `num-traits` 0.2.19

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `clamp` | `with T = i32`, the result in range, equal to the input inside it | held | 45 % of the inputs broke `requires` |
| `pow` | `with T = i64`, exponents 0, 1, and 3 | held | |
| `checked_pow` | `with T = i64`, exponent 2 with exact arithmetic | held | |
| `cast` | `with T = i64`, `with U = u8`, `Some` exactly for 0 to 255 | held | |
| `sign::abs` | `with T = i32`, the documented `MIN` case | broken in the test profile, held with `--release` | finding 4 |
| `sign::abs_sub` | `with T = i64`, `x <= y \|\| result == x - y` | broken in both profiles | finding 3 |
| `sign::signum` | `with T = i8`, -1, 0, or 1 | held | |
| `hydo_props::checked_add_i64` | a wrapper of `CheckedAdd`, with exact arithmetic | held | |
| `hydo_props::saturating_sub_u32` | a wrapper of `SaturatingSub` | held | |
| `hydo_props::to_i8_from_i128`, `u16_from_u64` | wrappers of `ToPrimitive` and `FromPrimitive` | held | |
| `hydo_props::float_to_i64_cast`, `float_to_u8_cast` | `cast` from `f64`, built from integer inputs | held | |

The crate has no `#[cfg(test)]` module. The evaluator added one, `hydo_props`, and hydo fuzzed its
functions.

### The crate `chrono` 0.4.45

| Function | Contract | Verdict | Note |
|---|---|---|---|
| `NaiveDate::from_ymd_opt` | `Some` exactly for real dates, leap years written out | held | |
| `NaiveDate::from_yo_opt` | `Some` exactly for real ordinals | held | |
| `NaiveTime::from_hms_opt`, `from_hms_milli_opt` | `Some` exactly for real times and leap seconds | held | |
| `NaiveTime::from_num_seconds_from_midnight_opt` | the same | held | |
| `DateTime<Utc>::from_timestamp` | a leap second only at second 59 | vacuous, then held | the `to fix:` line worked |
| 7 date properties in `naive::date::tests` | day numbers, days, months, ISO weeks, two text forms | held | |
| `tests::rfc3339_roundtrip` | parse, write, and parse again | held | 20,000 calls |
| `tests::rfc2822_roundtrip` | the same for RFC 2822 | broken, then held | the property was wrong: see below |
| `tests::timestamp_roundtrip` | seconds and milliseconds back | held | |

`rfc2822_roundtrip` panicked on `"1 Jan 20000 00:00 +0000"`, in `to_rfc2822`. Its documentation
says that it panics for a year with more than 4 digits, so the property was wrong. With such years
left out, 50,000 calls held.

## The friction log

The frictions are in the order the evaluator met them.

1. `README.md` does not name `hydo fuzz`. A developer who reads the front page does not learn that
   the command exists.
2. strsim starts with `#![forbid(unsafe_code)]`, as many crates do. hydo's test module carries
   `#[allow(warnings, unused, unsafe_code, ...)]`, so every function failed with `error[E0453]:
   allow(unsafe_code) incompatible with previous forbid`. The message names the compiler error and
   no `to fix:` line. The evaluator changed `forbid` to `deny` in the copy.
3. `ensures result <= 1` on `jaro`, whose result is an `f64`, printed `held ... on 0 calls,
   undecided on 1000` and then `every clause held`. The summary counted it as held, and the exit
   status was 0. The guide's table says that `held` means that the run tested enough.
4. The `osa_distance` failure from a wrong contract was clear: `a = "é", b = ""` and the result 1.
5. httparse's functions take `&[u8]`, and `like` is for strings only: `chars and like are for a
   string, and buf is a list`. The message is clear. Every parser property had to take `&str` and
   call `as_bytes()`.
6. httparse then stopped with `the crate does not build in the test profile without hydo's
   harness`. That is false: `cargo test --lib --no-run` builds the original crate. hydo's copy of
   `Cargo.toml` leaves out `[lints.rust.unexpected_cfgs]` with its `check-cfg` list. The crate's
   `#![cfg_attr(test, deny(warnings))]` then turns the `httparse_simd` cfg warnings into errors.
   The evaluator removed `deny(warnings)` from the copy.
7. The replay of `parse_chunk_size` printed `result = Ok(<not read>)`. hydo does not read the
   `Status` enumeration inside the result, so a contract on the parser itself can say nothing
   about a complete parse. Every httparse property needed a function in the test module.
8. In bytes, every method of `Bytes` and `BytesMut` is refused, because the types hold a
   `NonNull<u8>`. `Buf` and `BufMut` are traits, and the refusal says `fuzz a function of a type
   that implements it`. The methods of `impl Buf for &[u8]` are trait methods too, so that advice
   leads nowhere. The run exited 0 although 2 contracted functions were not fuzzed.
9. A run shows only counts. The evaluator could not see which operation sequences the
   `BytesMut` model got, so a pass after 50,000 calls says nothing about whether `unsplit` ever
   followed a `reserve`. `fuzz.json` holds the same counts.
10. Each trait method of num-traits needed a wrapper function. The wrappers worked, and the exact
    integer arithmetic in the clauses made them short.
11. On `abs`, the only input that reaches `value == -2147483648` panicked. The report then also
    called the clause `vacuous` and printed `to fix: give the inputs the shape that reaches the
    clause's last part`. The inputs already had that shape, so the advice was wrong.
12. A panic inside a macro-generated implementation is reported at the macro's call, so `abs` and
    `abs_sub` both point to `src/sign.rs:75`.
13. `from_timestamp` was `vacuous` with a general `to fix:` line. Two `input` lines with ranges
    near the leap-second edge fixed it on the next run.

## The candidates found

None of these was posted upstream, and none was checked against the crates' issue trackers.

1. strsim 0.11.1, `sorensen_dice`, scores text outside ASCII lower than the same text in ASCII.
   It divides by byte lengths, `(a.len() + b.len() - 2)`, but counts bigrams of characters.
   - Input: `sorensen_dice("ab", "aba")` gives `0.6666666666666666`, and `sorensen_dice("éü",
     "éüé")` gives `0.25`. `sorensen_dice("eee", "ee")` gives `0.666...`, and `sorensen_dice("ééé",
     "éé")` gives `0.25`.
   - Documentation: the function documents the Sørensen-Dice coefficient of bigrams, which does
     not depend on how many bytes a character has. The CHANGELOG lists `Fix metrics when given
     strings with multibyte characters` for the other metrics, and `jaro` and
     `normalized_levenshtein` give equal scores for both forms.
2. httparse 1.10.1, `parse_chunk_size`, accepts a chunk size line without a hex digit as size 0.
   - Input: `b"\r\n"` gives `Ok(Complete((2, 0)))`, `b" \r\n"` gives `Ok(Complete((3, 0)))`, and
     `b";ext\r\n"` gives `Ok(Complete((6, 0)))`.
   - Documentation: the function says only `Parse a buffer of bytes as a chunk size`. RFC 9112,
     section 7.1, defines `chunk-size = 1*HEXDIG`. A size of 0 means the last chunk, so this loose
     parse ends a body where a stricter parser reports an error.
3. num-traits 0.2.19, `abs_sub`, returns a negative number when the difference overflows.
   - Input: `abs_sub(1i64, -9223372036854775807)`. The test profile panics with `attempt to
     subtract with overflow`. With `--release` the result is `-9223372036854775808`.
   - Documentation: `The positive difference of two numbers. Returns zero if x is less than or
     equal to y, otherwise the difference between x and y is returned.` It names no overflow.
4. num-traits 0.2.19, `abs` on `i32::MIN` panics in a debug build.
   - Input: `abs(-2147483648i32)` panics with `attempt to negate with overflow` in the test
     profile, and returns `-2147483648` with `--release`.
   - Documentation: `For signed integers, ::MIN will be returned if the number is ::MIN.` That
     holds only without overflow checks.
5. hex 0.4.3, `decode`, names a character that is not in the input.
   - Input: `decode("é")` gives `InvalidHexCharacter { c: 'Ã', index: 0 }`, and the error text is
     `Invalid character 'Ã' at position 0`.
   - Documentation: `An invalid character was found.` The error turns the first byte of `é` into a
     character of its own.

Each minimal input in this list is hydo's own shrink. Each value is from a program that called the
published crate directly, outside hydo.

## The verdict

The evaluator would keep using `hydo fuzz` for pure functions with integer and string parameters, and for
round-trip and model properties in the test module. In about one hour on six mature crates, it
found three candidates (findings 1 to 3) and two smaller gaps in documentation (findings 4 and
5). Every finding came with a minimal input and a replay command that worked. The exact integer
arithmetic in clauses, the `with` lines for generic functions, and the 1 to 15 second runs made it
pleasant to use. What stops the evaluator, worst first:

1. A clause that hydo cannot read passes. A clause on an `f64` result was decided on 0 of 1,000
   calls, and the function still counted as `held` with exit status 0.
2. The harness fails on ordinary crates. It fails with `#![forbid(unsafe_code)]`, and it drops the
   `[lints]` table of `Cargo.toml`. The second failure comes with a message that blames the crate.
3. Most of a real API cannot carry a contract. Methods of types that hold pointers, trait methods,
   and enumeration results are refused or not read. In bytes and httparse every test was a
   hand-written function in the test module. For that work, hydo competes with the property tests
   a developer already writes.
4. `like` works only for strings, so a parser over bytes needs a wrapper that takes `&str`.
5. A run does not show the inputs it generated, so a pass gives no evidence of what it reached.

Open item 139 in `AGENTS.md` points to this page.

## After the fixes of M289

Milestone 289 fixed problems 1, 2, 4 and 5 of the verdict. Of the friction log it fixed 2, 3, 5,
6, 9 and 11, and the exit status of 8. The runs below used copies of the evaluation's crates with the evaluator's
contracts. In these copies, strsim's `#![forbid(unsafe_code)]` and httparse's
`#![cfg_attr(test, deny(warnings))]` were put back as the crates publish them. Each run used seed 1
and 1,000 inputs, on the Mac, with `hydo` built from this milestone's tree. "Before" is `main` at
`0038380d`.

| Crate and run | Before | After |
|---|---|---|
| strsim, `--fn jaro` | the harness does not build (E0453), exit 2; with `deny` in place of `forbid`, `held ... on 0 calls, undecided on 1000`, exit 0 | `held: ensures result <= 1 (line 190) on 1000 calls`, exit 0 |
| strsim, all 13 functions | none fuzzed (E0453), exit 2 | 12 held, 1 broken: finding 1 at `a = "ab", b = "aba"`, exit 1 |
| httparse, all functions | `the crate does not build in the test profile without hydo's harness`, exit 2 | 4 held, 1 broken: finding 2 at `text = "\r\n"`, exit 1 |
| num-traits, `--fn abs` | the clause `vacuous`, and `to fix: give the inputs the shape ...` | the clause `not reached`, the 81 calls that panicked named, no `to fix:` line; exit 1 for the panic |
| bytes, all functions | `6 held; 2 not fuzzed`, exit 0 | the same counts, a line that says the run did not test every contract, exit 3 |

On strsim's `jaro`, hydo now reads the `f64` result. `--input 'a = "abc", b = "abd"'` prints
`result = 0.7777777777777777`. With the clause changed to `ensures result < 1`, the run breaks on
15 of 1,000 calls and shrinks to `a = "", b = ""` with `result = 1.0`.

httparse's chunk size property now takes the bytes themselves. An added property
`chunk_size_bytes(buf: &[u8])` with `input buf like b"4\r\nRust\r\n", b"0\r\n\r\n", b"1a;ext=1\r\n", b"ff \r\n"` breaks on 25
of 1,000 calls at `buf = b"\r\n"`, which is finding 2 without a wrapper that takes `&str`.
`--show-inputs 5` printed inputs such as `buf = b"1a;ex\n"` and `buf = b"0\xa4\r\n\r\r\n"`.
Its `again:` line replays alone. The crate denies warnings in its tests, and a property function
that only hydo calls stays in use when `--fn` names another one.

The new message for a copy that does not build was checked with a test build of hydo that left
out the `[lints]` tables. On httparse it printed this, shortened:

```text
hydo fuzz: hydo's copy of the crate does not build, though the crate itself builds with `cargo
test --no-run`: the cause is hydo's copy (its Cargo.toml, which hydo writes from `cargo
metadata`, or its harness), not the crate. The first error: error[unexpected_cfgs]: unexpected
`cfg` condition name: `httparse_simd`.
```

Still open: frictions 1, 7, 10 and 12, and problem 3 of the verdict. Methods of types
that hold pointers, trait methods and enumeration results are still refused or not read.

## After Milestone 307

Milestone 307 built the three parts of problem 3: enumerations, trait methods, and values built
by a constructor. It also added `sorted(v)`. The runs below used fresh copies of the six crates,
unpacked from their crates.io archives, and byteorder 1.5.0 of the first evaluation. Each run
used seed 1 and 1,000 inputs, on the Mac, without a change to any crate's code.

The contracts were written for this milestone, on the parts that the evaluations found refused.
"Before" is `main` at `d0159ad0`, given the same contracts without the lines its language
lacks: `build`, `read`, `with Self`, and the clauses with `matches()` or `sorted()`. A refused
function is one that `hydo fuzz --list` reports as not fuzzed.

| Crate | Contracts | Refused before | Refused after | Functions fuzzed after | Verdicts after |
|---|---|---|---|---|---|
| strsim 0.11.1 | 2 | 0 | 0 | 2 | 2 held |
| hex 0.4.3 | 4 | 3 | 0 | 5 | 5 held |
| httparse 1.10.1 | 7 | 6 | 2 | 5 | 4 held, 1 broken |
| bytes 1.12.1 | 23 | 20 | 0 | 23 | 23 held |
| num-traits 0.2.19 | 15 | 15 | 0 | 19 | 17 held, 2 broken |
| chrono 0.4.45 | 11 | 11 | 0 | 11 | 11 held |
| byteorder 1.5.0 | 4 | 4 | 0 | 5 | 4 held, 1 broken |

A trait's method with two `with Self` lines is fuzzed twice, so a crate can fuzz more functions
than it has contracts. What each crate shows:

- strsim: `hamming`'s error is the variant `DifferentLengthArgs`, which hydo reads now.
  `matches(result, Err(StrSimError::DifferentLengthArgs)) == (len(a) != len(b))` held on 1,000
  calls of ASCII text. Before, `err(result)` was not read.
- hex: `FromHex::from_hex` ran on `Vec<u8>` and `[u8; 4]`, and `ToHex::encode_hex` on
  `Vec<u8>`. `decode`'s clause `err(result).InvalidHexCharacter.index < len(data)` decided 275
  calls. The variant's `c` is a `char`, which hydo does not read, so finding 5 still needs a
  property function.
- httparse: `parse_chunk_size`'s result reads as `Ok(Complete((2, 0)))`, where it read
  `Ok(<not read>)`. The contract on the parser states the RFC's `chunk-size = 1*HEXDIG` and
  breaks on 28 of 1,000 calls at `buf = b"\r\n"`, which is finding 2 without a property
  function. `Error::description_str` takes an enumeration, and three methods of `impl<T>
  Status<T>` ran with `with T` lines. `Request::parse` and `parse_headers` stay refused, because
  a `Header` borrows the buffer it describes.
- bytes: with `build Bytes from Bytes::copy_from_slice(v: &[u8])`, `build BytesMut from
  BytesMut::from(v: &[u8])` and a `read` line for each, every contracted method of `Bytes` and
  `BytesMut` ran. `Buf::get_u8`, `get_u16` and `get_uint` ran on `&[u8]`, and `BufMut::put_uint`
  and `put_slice` on `Vec<u8>`. Every clause held.
- num-traits: 12 traits' methods ran on the types that `with Self` lines name, with no wrapper.
  `Signed::abs` on `i32` panics at `self = -2147483648`, and `Signed::abs_sub` on `i64` at
  `self = 0, other = -9223372036854775808`, which are findings 4 and 3. With `--release`,
  `abs_sub` breaks its contract with `result = -9223372036854775808`.
- chrono: `Weekday` and `Month` values are generated, and their methods' contracts name variants.
  `NaiveDate` is built by `NaiveDate::from_ymd(year: i32, month: u32, day: u32)` and read by
  `num_days_from_ce()`. `succ_opt`, `checked_add_days`, `leap_year` and `Datelike::weekday` held,
  among them `matches(result, Mon) == ((self + 700000000) % 7 == 1)`.
- byteorder: `ReadBytesExt::read_uint::<BigEndian>` on `&[u8]` panics at `nbytes = 0` and at
  `nbytes = 9`, the candidate of the first evaluation, now on the trait method itself.
  `ByteOrder::read_uint` held on `BigEndian` and `LittleEndian`.

Nothing new was found: each broken contract is a finding of an earlier evaluation. Nothing was
posted upstream. Problem 3 of the verdict is fixed for these crates, and friction 7 and 10 are
fixed. Still open: friction 1 and 12, a `char`, a map, a generic structure such as httparse's
`Header`, and a constructor that returns an `Option`, such as `NaiveDate::from_ymd_opt`.
