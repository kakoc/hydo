# Function fuzzing: an evaluation on six crates (Milestone 280)

Status: an evaluation of the prototype of Milestone 275, `hydo fuzz`, done on this Mac on
2026-11-16 (Milestone 280, tentative number). No code of the feature changed.

The question was whether a Rust developer who has never used hydo would keep using `hydo fuzz`.
The evaluator learned the feature only from what a user gets: `hydo fuzz --help`, `hydo guide`,
and the design document [function-fuzzing.md](function-fuzzing.md), which the help names. The
evaluator then wrote contracts on six popular crates from their documentation and ran
`hydo fuzz` on each.

## The setup

- hydo built from `main` at `9294778e` with `cargo build --release -p hydo`; Rust 1.90.0 on
  an M2 Pro.
- Each crate is the archive from crates.io at one version, unpacked into a scratch folder:

| Crate | Version | Kind | Archive SHA-256 |
|---|---|---|---|
| semver | 1.0.28 | a parser | `8a7852d0…` |
| base64 | 0.23.1 | an encoding | `ac07cdec…` |
| fixedbitset | 0.5.9 | a data structure | `5905ec75…` |
| memchr | 2.8.3 | a byte search | `cf8baf1c…` |
| num-integer | 0.1.47 | numeric | `7ce2d95d…` |
| byteorder | 1.5.0 | I/O, through `Read` and `Write` | `1fd0f258…` |

Contracts are `// hydo:` comments before a function ([contracts.md](contracts.md), "The
language"). Where a public function could not be fuzzed, the evaluator wrote a property
function, as the guide advises: a function that returns `bool`, with `// hydo: ensures
result`, in a `#[cfg(test)]` module of the crate. Every run used the default 1,000 inputs per
function and seed 1, unless the text names other values.

## What the user documentation gives

- `hydo fuzz --help` gives the options and names `hydo guide manifest`, section "Function
  contracts", and the design document.
- `hydo guide` has no topic for fuzzing or contracts. `hydo guide fuzz` and `hydo guide
  contracts` answer `no guide topic`. The guide's whole text on fuzzing is one paragraph of six
  sentences in the manifest's section on contracts.
- The design document is in the repository, so a user of `cargo install hydo` or of a release
  tarball does not have it.
- The comment language (operators, `len()`, `is_some()`, `ok()`) is only in
  [contracts.md](contracts.md), which the guide's section names once.

The evaluator had to read the design documents for three facts. These are the types that hydo
can generate, the limit of 16 characters for a string, and the refusal of generic functions and
trait methods. Each is a gap in the user documentation.

## Results per crate

The time to a first useful run counts from the first `hydo fuzz` command, after the contracts
were written. Writing the contracts took the evaluator 5 to 10 minutes per crate.

| Crate | Contracted | Refused or unusable | Run time | First useful run | Findings |
|---|---|---|---|---|---|
| semver | 5 functions, 9 properties | all 5 functions (harness build error) | 8 s with the build, then 1 to 3 s | 3 min | 1, the evaluator's own error |
| base64 | 4 functions, 7 properties | none | 26 s with the build | 1 min | none |
| fixedbitset | 3 functions, 10 properties | 2 functions (a pointer field) | 23 min, stopped by hand; 2 s with an `input` line | 3 min | 1 run that does not end |
| memchr | 5 functions, 8 properties | none | 9 s; 20,000 inputs each in 4 s | 1 min | none; a planted bug only with a hand-built input |
| num-integer | 2 functions, 11 properties | both functions (generic) | 4 s | 1 min | 1 candidate, 1 known bug, 1 the evaluator's error |
| byteorder | 2 functions, 6 properties | 1 function (a trait method) | 9 s | 1 min | 1 candidate (a documentation gap) |

### The semver parser

The contracts on `Version::new`, `Version::parse`, `VersionReq::parse`, `Comparator::parse` and
`Prerelease::new` say what the documentation says, for example `ensures !is_ok(result) ||
len(text) >= 5`. `hydo fuzz --list` printed `can be fuzzed` for all five. The harness then did
not build, for any of them and also with `--fn` for one:

```text
error[E0616]: field `head` of struct `Identifier` is private
   --> src/lib.rs:478:4007
478 | ... ei(&(x0).patch, o); { let _ = &(&(&(x0).pre).identifier).head; o.push_str("X "); } ...
```

Each function returns a `Version` or holds a `Prerelease`, whose field `identifier` is a
structure of the module `identifier` with private fields. The harness lives in another module,
and it names every field even when it writes `X` (not read) for it. One such function stops the
build of all 14, so the evaluator ran the nine properties alone with `--fn`.

Eight properties held. `^M.m.p matches the next patch release` broke at once on `major = 0,
minor = 0, patch = 0`. The documentation says that `^0.0.p` means `=0.0.p`, so the property was
wrong, and hydo's first input, the smallest value of each parameter, showed it.

Four of the eight properties that held tested nothing. They take a `&str` and check only text
that parses, and the generated strings never parse: of 2,000 generated strings, 0 were valid
versions. The generator draws characters from a set of 20 (`\0`, newline, space, `-./019AZ_abcxyz`,
`é`, `€`, and an emoji) and at most 16 of them. The report still says `held: ensures result on
1000 inputs` and `every clause held`. The property that builds the text from its parts
(`major.minor.patch-pre+build`) reached the full parse on 31 of 1,000 inputs. semver's own tests
are example tests, without generated inputs.

### The base64 encoding

Four functions are not generic, and all four were fuzzed. They are the free functions
`encoded_len` and `decoded_len_estimate` and the constructors `Alphabet::new` and
`Symbol::new`. The engine API (`STANDARD.encode`, `decode`, `decode_slice`) is a trait. Seven
properties reached it, among them:

- round trips for two engines;
- the length of `encode` against `encoded_len`;
- `decode_slice` into a buffer of `decoded_len_estimate` bytes;
- a pad-indifferent engine on padded and unpadded text.

Every clause held. Two runs showed the limits of generation:

- `Alphabet::new` needs a string of 64 bytes. `input len(alphabet) in 64..=64` is refused
  (`write input <name> in <lo>..<hi>`). With `requires len(alphabet) == 64` instead, the report
  said `2 inputs from seed 1, 49998 generated inputs broke requires` and `every clause held`,
  with exit status 0. The two inputs were strings of 16 four-byte characters.
- 180 of the 1,000 generated strings for the decode property were valid base64, mostly the
  empty string.

base64's own tests already check these round trips with random inputs from the `rand` crate.

### The fixedbitset data structure

`FixedBitSet` holds a `NonNull` pointer, so `insert` and `grow` were refused with a clear
reason: `` `self`: hydo cannot generate a value of `NonNull<MaybeUninit<SimdBlock>>` ``.
The ten properties build sets from a `Vec<u16>` and compare them with a `BTreeSet`.

The first run did not build, because the crate is `#![no_std]`. The harness uses `::std`
paths and the standard prelude (`to_string`). A `#[cfg(test)] extern crate std;` in the crate
was not enough, because the prelude stays the core one. The change that worked was
`#![cfg_attr(not(test), no_std)]`. The errors named lines of generated code, not the cause.

The ten properties held within seconds of the build. Then `with_capacity(bits)`, with
`ensures result.length == bits` and no `input` line, ran for 23 minutes:

- The generator draws values of every bit width, so many calls asked for gigabytes or
  terabytes of memory. Each such call used 1 to 2 GB and ended at the 10 s `--timeout`.
- The 1,000 inputs took about 15 minutes. The shrink then started from 562,949,953,421,311 bits
  and moved by small steps, because every smaller candidate near it also timed out.
- hydo printed nothing after the tenth property. The evaluator stopped it by its process id.
  The harness process lived on with 1.3 GB until it was stopped too.

The function's documentation names no limit, so the finding is noise. With `input bits in
0..=1_000_000` the same 1,000 inputs held in 2 s.

### The memchr byte search

The contracts on `memchr`, `memrchr`, `memchr2`, `memmem::find` and `memmem::rfind` read the
result, for example `ensures is_none(result) || haystack[some(result)] == needle`. Seven
properties compare the searches with the standard library's naive search. All held, and 20,000
inputs per function took 4 s.

The evaluator then planted a bug in the vector loop of memchr's generic search, to see what
the contracts catch. A match in the third 16-byte vector of a 64-byte step returned its index
plus one.

- The five direct contracts and the seven properties held. A list has at most 16 elements, so
  the loop, which needs more than 64 bytes before the match, never ran. `memmem::find` with a
  haystack shorter than 64 bytes never leaves its Rabin-Karp path.
- A property that places the generated bytes between two runs of up to 599 filler bytes caught
  it on 5 of 1,000 inputs, shrunk to `needle = 0, pad = 47, fill = 1, mid = [1, 0], pad2 = 31`.
- memchr's own quickcheck tests (`cargo test --lib`) caught the bug in three tests.

### The num-integer numeric functions

Every public function of num-integer is generic over `T: Integer`, or a method of a trait.
`gcd` and `div_floor` were refused with `a generic function (T: Integer): write the contract on
a function of concrete types that calls it`. Eleven properties over `i64`, `u64`, and `i32`
state the documented rules:

- gcd is non-negative and divides both values;
- the rules of `div_mod_floor`, `div_ceil`, and the multiples;
- the Bézout identity of `extended_gcd`;
- the bounds of `sqrt`, `cbrt`, `nth_root`, and `average_floor`;
- `binomial` up to the documented n of 67.

Three failed:

- `prop_gcd_i64` panicked on `a = 0, b = -9223372036854775808`, `attempt to negate with
  overflow`. With `--release` the clause broke instead: the gcd is negative. This is known
  upstream as rust-num/num-integer#55, "`Integer::gcd()` returns negative value".
- `prop_nth_root_negative` panicked on `x = -9223372036854775808, n = 1` at
  `src/roots.rs:166`, `attempt to negate with overflow`. This is a candidate bug, below.
- `prop_nth_root_i64` panicked in the evaluator's own code, `r + 1` for `x = i64::MAX, n = 1`.

The report placed the gcd panic at `hydo-out/fuzz/crate/src/hydo_props.rs:16`: the path of the
copy, and the line of the property's closing brace, where the harness sits. The panic happens
in `abs`, called from code that a macro generates.

### The byteorder I/O traits

`BigEndian::read_uint`, a method of a trait's implementation for a concrete type, was fuzzed
with `requires nbytes >= 1 && nbytes <= 8 && nbytes <= len(buf)`; 4,823 of 5,823 generated
inputs broke `requires`, and its clause held. `ReadBytesExt::read_uint`, a method with a default
body in the trait, was refused. Six properties used `std::io::Cursor`, `Vec<u8>` as a writer,
and a writer that fails after a given number of bytes.

`prop_reader_read_uint` checks the documented errors. The documentation says that `read_uint`
on a reader returns the errors of `Read::read_exact`. It panicked in two ways, each shrunk and
reported apart:

- `data = [], nbytes = 9`: `range end index 9 out of range for slice of length 8`, at
  `src/io.rs:430`, on 623 of 1,000 inputs;
- `data = [], nbytes = 0`: `assertion failed: 1 <= nbytes && nbytes <= out.len() && nbytes <=
  buf.len()`, at `src/lib.rs:1966`, on 122 of 1,000 inputs.

The `--input` lines replayed both. Two runs with seed 1 wrote the same `fuzz.json`
(`e3c8ac34…`).

## Bug candidates

Each candidate was searched for in the crate's open and closed GitHub issues. Nothing was
posted.

1. **num-integer 0.1.47: `nth_root(1)` of a signed type's minimum panics in a debug build.**
   `i64::MIN.nth_root(1)` ends with `attempt to negate with overflow`. The documentation says
   that for negative `x` and odd `n` the root satisfies `(r-1)ⁿ < x ≤ rⁿ`, and it names only
   `n == 0` and an even `n` with a negative `x` as panics. The answer, `i64::MIN`, exists. The
   macro `signed_roots!` computes `-((x.wrapping_neg() as u64).nth_root(n) as i64)`, and the
   negation overflows only when the root is 2⁶³, that is for `n == 1`. A release build wraps
   and returns the right value. The crate's tests check negative roots for `n` from 2 and the
   minimum only for `n` equal to the bit width. The searches `nth_root`, `root`, `cbrt`,
   `negate`, `panic` and `MIN overflow` found the Roots trait's addition (#9) and the
   overflow issues of `lcm` (#82), `is_multiple_of` (#81, #83) and `gcd` (#55), and no report
   of this one.
2. **byteorder 1.5.0: `ReadBytesExt::read_uint` panics for `nbytes` of 0 or more than 8.**
   The same holds for `read_int` (8) and `read_uint128` and `read_int128` (16), by their code.
   Their documentation has an "Errors" section and no "Panics" section, while
   `ByteOrder::read_uint` documents the same panic. It is a documentation gap more than a bug:
   a caller who passes 9 bytes made a mistake. The searches `read_uint`, `nbytes panic` and
   `panics` found #230 (the `write_uint` panic documentation) and #130 (the extension traits'
   documentation in general), and no report of this one.

The known gcd bug (num-integer#55) was found too. The other failures were the evaluator's
errors: the caret rule of semver and an overflow in a property's own code.

## The problems, ranked by how much they hurt

1. **The harness does not build on common crates, and one function stops all.** Three of six
   crates are `#![no_std]` (fixedbitset, memchr, num-integer), and the harness needs `std`.
   semver's five functions failed on a private field of a structure in another module, after
   `--list` said `can be fuzzed`. The error shows generated lines of 8,000 columns. Fix: write
   the harness with `extern crate std;` and full paths (`::std::string::ToString::to_string`),
   and leave out a field that the harness's module cannot see, as not read. Build each
   function's harness on its own when the build fails, report the function that failed with
   one line, and fuzz the rest.
2. **`every clause held` hides runs that tested nothing.** The semver properties held on 2,000
   strings of which none was a version. `Alphabet::new` held on 2 inputs after 49,998
   rejections, with exit status 0. Fix: print a warning, and exit with status 1 under a new
   `--strict`, when fewer than 10 % of the wanted inputs were run. Add a clause
   `// hydo: reaches <expr>` that counts the calls where it holds and warns at 0, so that a
   property can say which branch matters.
3. **Generated values are too short and too random to reach real code.** Strings have at most
   16 characters from a set of 20, and lists at most 16 elements. memchr's vector loop and
   memmem's main path need more than 64 bytes, and semver's parser needs a version's shape.
   `input` takes only a range of a number. Fix: accept `input len(v) in <lo>..=<hi>` for strings
   and lists, `input s chars "<set>"` for a string's characters, and let the length grow with
   the number of inputs, up to 1,024 after the first 1,000.
4. **A run with a large size parameter looks hung.** `FixedBitSet::with_capacity` ran for 23
   minutes, at up to 2 GB per call, with no output, and its harness outlived `hydo fuzz`. Fix:
   print a progress line per function every 10 seconds. After the first call that does not
   return, record the finding, and stop that function's inputs and shrink at a time budget.
   Name the parameter in the report, with the advice to add an `input` line. Run the harness
   in its own process group and end the group when hydo ends.
5. **Most of a crate's public API cannot be fuzzed.** hydo refuses generic functions, methods
   of a trait, enumerations, and types with a pointer field (`FixedBitSet`).
   By their signatures this is all of num-integer, hex, smallvec, and arrayvec, and most of
   base64. Every finding came from a property function that the evaluator wrote by hand, so
   `hydo fuzz` acted as a property-test runner without generators. Fix: `// hydo: instantiate
   T = i64, u8` for a generic function, enumerations
   whose variants hold generated types, and values built by calling a constructor that the
   contract names (`// hydo: build FixedBitSet with with_capacity(n), insert(i)`).
6. **The replay line drops `--release`.** In the release profile `prop_gcd_i64` broke its
   clause, and the printed `again:` command, without `--release`, panics instead. Fix: carry
   `--release`, `--toolchain`, `--target-dir` and `--timeout` into the `again:` line.
7. **`--json` does not print only JSON.** Its first line on standard output is `hydo: fuzz:
   building the harness of 1 function (...)`, so `json.load` fails. Fix: write the progress
   lines to standard error.
8. **A panic in code that a macro generates is placed at the harness.** num-integer's gcd panic
   was reported at the property's closing brace, with the path of the copy. Fix: map a path in
   `hydo-out/fuzz/crate` back to the project's file, and take the deepest frame in the crate
   that is not the harness's line.
9. **The documentation is a paragraph.** There is no `hydo guide` topic; `--list` on a crate
   without contracts prints nothing and exits 0, while `hydo fuzz` says that `--list` says why.
   Function names lose their module or type (`memchr::find` for `memchr::memmem::find`,
   `fixedbitset::insert` for `FixedBitSet::insert`). The default `--out` is relative to the
   current folder, not to `CRATE_DIR`. Fix: a guide topic `hydo guide fuzz` with the comment
   language, the types, the limits and one worked example; full paths in names; and a line from
   `--list` when no function has a contract.

## Missing features

- Types: enumerations, floating-point values, `char`, `HashMap` and `BTreeMap`, `&mut [T]`,
  references inside structures, and the crate's types that hold pointers.
- Functions: generic functions, trait methods with default bodies, `async` functions.
- The language: a quantifier (`forall i in 0..len(v): v[i] <= v[i+1]`), an implication
  (`a ==> b`, written today as `!a || b`), and a call of the program's own pure functions in
  `ensures`, such as `ensures parse(display(result)) == result`.
- Generation: lengths and character sets for strings and lists, values from a list of
  examples (`input s in ["1.2.3", "0.0.0-alpha"]`), and a corpus of failing inputs kept between
  runs.
- Reporting: coverage of the crate's lines per function, and the share of inputs that reached
  a branch.

## The verdict

A Rust developer would not keep using `hydo fuzz` in its present form. On the six crates the
direct contracts ran on 11 of 21 functions, and they found nothing but noise. Every real
finding came from a property function, which the developer writes by hand and which proptest
or quickcheck already runs with better generators. The two candidates are edge cases at a
type's bounds: num-integer's `nth_root(1)` of the minimum, and byteorder's `nbytes` of 0 or 9.
They show that the generator's bounds-first order is useful.

Three changes would change the verdict. The first is a harness that builds on `no_std` crates
and private fields, function by function (problem 1). The second is a warning when a run tests
nothing (problem 2). The third is generators for lengths and characters (problem 3). With these,
`hydo fuzz` is a quick way to turn a function's documented rules into tests without writing a
generator. The run inside the VM with faults (open item 139) would then give what proptest
cannot. It would check the same contracts under I/O errors, clock steps, and pauses.
