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
  that a trait's method is fuzzed on (section "Trait methods").
- `// hydo: build <Type> from <constructor>(<name>: <type>, ...)` and `// hydo: read <Type> as
  <type> by <method>()`: how hydo makes and reads a value of a type that it cannot generate
  field by field (section "Values built by a constructor").

An expression reads parameters, fields, `result`, integers, floating-point numbers (`0.5`),
strings, `old(e)` (the value at the call's entry), and the functions `len`, `is_some`, `is_none`,
`is_ok`, `is_err`, `some`, `ok` and `err`. `hydo fuzz` also reads `matches(e, <pattern>)` and
`sorted(v)` (sections "Enumerations" and "Sorted inputs"). The operators are `||`, `&&`, the
comparisons, `+ - * / %`, `!` and unary `-`. Integers are exact, with no overflow. A comparison
of an integer with an `f32` or `f64` value is exact too. Arithmetic on such a value is the
program's own `f64` arithmetic, and a NaN is unequal to everything, itself included. A call of
the program's own code is refused.

A type that a `with` line names is written as the function's module writes it. byteorder's
`io.rs` does not import `BigEndian`, so its contracts write `with T = crate::BigEndian`.

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

A variant that the type does not have is refused before the run, with the variants it has. A
variant is never compared with `==`: write `matches()`. An `input` line names a variant's field
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

A constructor that panics on an input ends the call with a panic at the constructor. Narrow its
arguments with `input` lines: chrono's `NaiveDate::from_ymd(year: i32, month: u32, day: u32)`
needs `input self.month in 1..=12` and `input self.day in 1..=28`. A type without a `build` line
is refused with the line to write.

## Sorted inputs

`sorted(v)` is true when each element of a list is at most the next. Integers and
floating-point numbers compare as numbers, and strings by their bytes. In a `requires` clause,
hydo generates the list in order, so no input is thrown away for it:

```rust
// hydo: requires sorted(v)
// hydo: ensures is_none(result) || v[some(result)] == x
pub fn search(v: &[u32], x: u32) -> Option<usize> {
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
- Some lists repeat one value with a few others among it.
- A floating-point number is 0, -0, 1, -1, a half, the type's smallest or largest value, an
  infinity, NaN, a small fraction, a value of the `in` range, or any bits.
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
a structure of another crate without a `read` line. Such a value shows as `<not read>`. A clause that is decided on fewer than
`--min-reach` calls (default 1 % of `--cases`) gives `not judged`, with the reason.

A clause `a || ... || z` holds without checking `z` when a part before it is true. The report
counts the calls on which `z` decided. Fewer than `--min-reach` such calls make the run
`vacuous`. When calls panicked, the clause line says `not reached` instead: the calls that
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
  each type parameter;
- a parameter of a type that holds a pointer, or whose structure has a private field of another
  module, without a `build` line;
- a parameter of a generic structure, a `char`, or a map.

A result of such a type is read in part, and a clause that reads the rest is undecided. A
function whose test module does not build is reported with the compiler's first error, and the
other functions run. hydo builds the test module in a `#![no_std]` crate too. The copy keeps the
crate's `[lints]` tables, and the test module allows no lint that the crate forbids.

When the copy does not build without any test module, hydo builds the crate with its own
`Cargo.toml` to find the cause. If the crate builds, the message says that hydo's copy is the
cause and names the first error.

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
hydo: durability check broken: fuzz-vm::save: fuzz-vm::save_survives: ensures result (line 161) on 750 calls in 4 runs, first under `disk=cut dseed=b6a3ee203ddfdea3 | run C=14`
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
