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
- `// hydo: with T = i64`: the type that a generic function's type parameter takes.

An expression reads parameters, fields, `result`, integers, floating-point numbers (`0.5`),
strings, `old(e)` (the value at the call's entry), and the functions `len`, `is_some`, `is_none`,
`is_ok`, `is_err`, `some`, `ok` and `err`. The operators are `||`, `&&`, the comparisons,
`+ - * / %`, `!` and unary `-`. Integers are exact, with no overflow. A comparison of an integer
with an `f32` or `f64` value is exact too. Arithmetic on such a value is the program's own `f64`
arithmetic, and a NaN is unequal to everything, itself included. A call of the program's own code
is refused.

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
an enumeration. Such a value shows as `<not read>`. A clause that is decided on fewer than
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

- a method of a trait, and a method of a generic `impl` block;
- a generic function without a `with` line for each type parameter;
- a parameter of an enumeration, or of a type that holds a pointer;
- a parameter whose structure has a private field of another module. Fuzz its constructor, or a
  function that takes the parts.

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
- pauses of the calls (`P=workload@<step>~<ms>:<ms>`).

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
and KVM, or `aarch64-unknown-linux-musl` for the Mac engine. Since Milestone 292 it ran on hvx;
the Mac engine and KVM have not run it yet.

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
schedule, `--faults cut,disk,clock,pause`, `--cases N` (default 200) and `--batch N` (calls per
workload step, default 25).

## Long runs

A run prints a progress line on standard error every 5 seconds. A call that does not return
within `--timeout` (default 10 seconds) is a finding. After 3 such calls, the function's other
inputs are not run, and a shrink stops after 3 such calls or a minute. `--json` prints only the
JSON report on standard output.
