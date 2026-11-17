# Function fuzzing from contracts (Milestone 275)

Status: a prototype for Rust, natively, on this Mac (Milestone 275, tentative number). The run
inside hydo's virtual machine with faults is designed, not built
([Inside the virtual machine](#inside-the-virtual-machine)).

A function contract is a set of `// hydo:` comments right before a function
([contracts.md](contracts.md)). During a whole-system run, hydo checks the contract at every call.
Function fuzzing uses the same comments to test one function at a time:

- `requires` and the `input` lines describe the valid inputs, so hydo generates inputs that keep
  them;
- `ensures` judges the result of each call;
- a panic, a call that does not return, and a process that ends are findings too.

```rust
// hydo: requires amount > 0
// hydo: ensures req_id > old(l.last_req) || l.balance == old(l.balance)
// hydo: ensures result == l.balance
// hydo: input amount in 1..=10_000
#[inline(never)]
fn apply(l: &mut Ledger, c: &mut Conn, req_id: u64, amount: u64) -> u64
```

```text
$ hydo fuzz hydo/onboard/examples/contracts-ledger/rust
hydo: fuzz: ledger::apply (src/main.rs:40): 1000 inputs from seed 1, 0 generated inputs broke `requires`
hydo: contract broken: ledger::apply ensures req_id > old(l.last_req) || l.balance == old(l.balance) (src/main.rs:35) on 96 of 1000 inputs
hydo:   smallest input (60 shrink steps): l = Ledger { balance: 1, last_req: 1, applied: 0 }, c = Conn { last_req: 0 }, req_id = 1, amount = 1
hydo:   the call gave: l = Ledger { balance: 0, last_req: 1, applied: 1 }, c = Conn { last_req: 1 }, result = 0
hydo:   again: hydo fuzz hydo/onboard/examples/contracts-ledger/rust --fn apply --input 'l = Ledger { balance: 1, last_req: 1, applied: 0 }, c = Conn { last_req: 0 }, req_id = 1, amount = 1'
```

## Decisions

1. **The contract is the test.** A developer writes no generator and no assertion. The
   comments that a run checks also describe the inputs and judge the outputs. A property that
   needs a call, such as a round trip, is a function that returns `bool` with
   `// hydo: ensures result`. It can live in the crate's `#[cfg(test)]` module.
2. **hydo calls the function through a copy of the crate.** It never changes the project's
   files. It copies the package, writes a manifest for the copy, and adds a test module, the
   harness, right after the function. The harness is a child of the function's own module, so
   it reaches private functions, private fields, and the functions of a binary crate. No procedural
   macro and no change to the project's build is needed.
3. **The harness is one line.** hydo writes it on the line of the function's closing brace (or
   of the `impl` block's brace for a method). Every line of the file keeps its number, so a
   panic's location is the line of the project's own file.
4. **hydo generates, judges and shrinks; the harness only calls.** The harness reads each input
   as text, calls the function, and writes the result and the `&mut` parameters after the call.
   hydo evaluates the clauses on those values. The same driver can therefore run the harness
   natively or inside a VM.
5. **No solver.** hydo generates values from the types and keeps those that satisfy
   `requires`. The generator aims at the edges that the clauses name
   ([How inputs are generated](#how-inputs-are-generated)).
6. **One seed gives one run.** The generator is seeded, and the shrink is deterministic, so two
   runs with the same seed give the same report, byte for byte. An input also replays alone
   with `--input`.

## How a function is found and called

hydo reads the package with `cargo metadata` and every `.rs` file under the package's folder.
It reads the sources as text. Comments and the contents of literals are masked, and then
braces, parentheses and angle brackets are matched. The DWARF debug information is not used:
it describes the values of a running program, and the harness must build values, which needs
the type names the compiler accepts.

For each contract, hydo reads the function's signature:

- the parameters and how each is passed: by value, by `&`, or by `&mut`;
- the receiver of a method (`self`, `&self`, `&mut self`) and the type of its `impl` block;
- the result type.

It resolves each type to a type that it can generate. These are the integer types, `bool`,
`String` and `&str`, `Vec<T>`, `&[T]`, arrays of up to 64 elements, `Option`, `Result`, tuples,
`Box`, `Rc` and `Arc`. It also resolves the crate's structures with named or tuple fields and
the crate's type aliases. hydo refuses a function with one reason each:

- a generic function or a method of a generic `impl` block;
- an `async` function, a method of a trait, or a function inside another function;
- a parameter of a type it cannot generate: floating-point values, enumerations, references
  inside structures, a recursive structure, `impl Trait`.

`hydo fuzz --list` prints each contracted function and whether it can be fuzzed. A result or a
field of a type that hydo cannot read is shown as not read, and a clause that reads it is
undecided.

The copy's manifest comes from `cargo metadata`, so a workspace member's inherited keys and
relative paths resolve. hydo keeps the workspace's `Cargo.lock`, its `[profile.*]` and
`[patch.*]` sections, its `rust-toolchain` file and its `.cargo/config.toml`. It declares every
target of the package, because a build script can name them. It then runs
`cargo test --no-run --lib` (or `--bin`) and runs the test program with the harness's test
only. The harness builds in the test profile, so the crate's `#[cfg(test)]` code and debug
assertions are on, as in its own unit tests. `--release` builds it as the program ships.

## How inputs are generated

The first input is the smallest value of every parameter, and the second the largest. The rest
are random. Since Milestone 282, the longest list or string grows with the inputs. It is 4 for the first
100 inputs, 16 up to input 299, 64 up to input 599, and 256 after them. One input in eight after the
first 100 has an edge size, such as 0, 63, 64, 65, or 4,096. One list of eight elements or more
in four repeats one value with up to three others among it. A `usize` or `isize` value without
an `input` line stays between -2^20 and 2^20. One input holds at most 65,536 integers,
characters, and elements in all.

The seeded generator draws each integer in one of three ways:

- An edge, in 3 of 10 draws. An edge is a bound of the type or of the `input` domain, 0, 1, or
  -1. It is also a constant that a clause compares the value with, or the next value on either
  side of it.
- A small value from 0 to 16, in 3 of 10 draws.
- A value of a random bit width, in 4 of 10 draws, so that 5 and 2^40 are equally likely.

A path that a clause names, such as `l.balance` or `len(v)`, gets the clause's constants. A
pair of paths that a clause compares, such as `req_id` and `old(l.last_req)`, is made equal or
one apart in 1 of 4 inputs. This is how the ledger's input with `req_id == l.last_req` comes up
often, and it is the input that shows the planted bug.

An `input` line narrows a value: `input amount in 1..=10_000`. Since Milestone 275 it can
narrow a field too: `input l.balance in 0..=1_000_000`. Since Milestone 282 it takes a length
(`input v len 0..1000`), a string's characters (`input s chars ascii`), example strings that the
generator uses and changes (`input text like "1.2.3"`), and the elements of a list
(`input v[] in 1..=255`). A clause that compares a list's element with a value (`v[i] == x`)
makes the generator put the value into the list at a random place in one input of four. An input that breaks a `requires` clause,
or one that a clause cannot judge, is counted and not run. hydo generates at most 50 times the
wanted number of inputs.

## How `ensures` is judged

The meaning of an expression is the one of a run ([contracts.md](contracts.md), "The
language"). A parameter in `ensures` is its value at entry, a field read through a reference
parameter is read after the call unless it is inside `old()`, and integers are exact. hydo knows
the value at entry (it generated it), and the harness writes the `&mut` parameters after the
call. A value parameter and a `&` parameter keep their entry values. Two lists, two strings or
two structures compare by their contents.

A clause that cannot be read for a call, such as an element past the length, is undecided for
that call and not broken. The report counts the undecided calls of each clause.

## How a failure shrinks and replays

Each failure kind shrinks once, from the first input that showed it:

- a broken clause, by its line;
- a panic, by its location in the crate (a panic in the standard library is placed at the
  crate's own frame of the backtrace);
- a call that did not return within `--timeout`, or a process that ended during the call.

A shrink step tries simpler inputs, each one change away from the current one, and keeps the
first that keeps `requires` and fails the same way:

- an integer moves to its target (0, or the bound of its domain nearest to 0), then halfway, a
  quarter of the way, and so on, and by 1;
- a list or a string becomes empty, loses a half or one element, or has an element shrunk;
- an option becomes `None`, a boolean becomes `false`, a character of a string becomes `a`.

The candidates run in chunks of 64 in one process. A step ends at the first chunk with a
failing candidate, and the shrink ends when no candidate fails. The report then runs the
smallest input once more and says whether it failed the same way (`replayed` in `fuzz.json`).

## The report

`hydo fuzz` prints each broken clause, panic and other failure, with the number of inputs it
broke on, the smallest input, what the call gave, and the command that replays it. It prints
each clause that held, with the number of calls it judged. `--out DIR` (default
`hydo-out/fuzz`) holds the copy, cargo's log and `fuzz.json`. Since Milestone 282 each function
ends with one verdict: `held`, `broken`, `vacuous` (a clause `a || ... || z` whose `z` decided
fewer calls than `--min-reach`), or `too few inputs` (fewer calls than `--min-inputs`). The exit
status is 1 when a clause broke or a call failed, and 3 when none did but a function tested too
little. `hydo guide fuzz` is the user's reference.

## Proof

### The contracts-ledger example

`hydo fuzz hydo/onboard/examples/contracts-ledger/rust --toolchain 1.90.0` ran 1,000 inputs of
`apply` in 5.1 s, the build included. The example's files were not changed. Four failures were
found:

| Failure | Inputs | Smallest input | What it means |
|---|---|---|---|
| `ensures req_id > old(l.last_req) \|\| l.balance == old(l.balance)` | 96 | `l = Ledger { balance: 1, last_req: 1, applied: 0 }, c = Conn { last_req: 0 }, req_id = 1, amount = 1` | The planted bug: a request that the ledger applied before is applied again on a new connection. |
| `ensures req_id <= old(l.last_req) \|\| l.balance == old(l.balance) - amount` | 159 | `l = Ledger { balance: 0, last_req: 0, applied: 0 }, c = Conn { last_req: 1 }, req_id = 1, amount = 1` | A connection that saw a higher request id than the ledger. The server never makes one; the contract lacks `requires c.last_req <= l.last_req`. |
| a panic at `src/main.rs:46`, `attempt to subtract with overflow` | 180 | `balance: 0`, `amount = 1` | A debit larger than the balance. The server has no check, and the contract lacks `requires amount <= l.balance`. |
| a panic at `src/main.rs:48`, `attempt to add with overflow` | 13 | `applied: 18446744073709551615` | A counter at its largest value; no run reaches it. |

The third clause, `ensures result == l.balance`, held on the 807 calls that returned. A second
run with seed 1 wrote the same `fuzz.json`
(`16c3e7aa3b43882da574d275bbe29dac56058219114d45bc4597c51b9c1fb9a4`), and the `--input` of the
first row replayed the broken clause.

The whole-system run found the planted bug only under a pause or a lost answer
([contracts.md](contracts.md), "Proof on the example"). Fuzzing finds it from the contract
alone, and it also shows what the contract does not say.

### Qdrant's crate `common`

The contracts are in [function-fuzzing/qdrant-contracts.patch](function-fuzzing/qdrant-contracts.patch),
on Qdrant `222b2e44`, the commit of the example `hydo/onboard/examples/qdrant-flaky-10200`. They
cover 12 functions of `lib/common/common`: 10 of Qdrant's own and 2 round-trip properties in the
test module of `delta_pack.rs`. `hydo fuzz lib/common/common --toolchain 1.99.0` ran 1,000
inputs of each in 7.8 s with a built copy. Nine functions held every clause, among them both
round trips, `thread_count_for_hnsw`, `prefetch_windows` and the three functions of `U24`. Three
broke:

| Function | Failure | Smallest input | Weight |
|---|---|---|---|
| `default_cpu_budget_unallocated` | `ensures result <= 0` on 3 of 1,000 | `num_cpu = 9223372036854775808` gives `576460752303423488` | `num_cpu as isize` wraps. No machine has 2^63 CPUs. |
| `delta_pack` | a panic at `src/delta_pack.rs:22` on 454 of 1,000 | `data = [1, 0]` | Its comment says that the input must be sorted, and the contract language cannot say so. The round trip over sorted input held. |
| `decompress_sequence` | a panic at `src/bitpacking.rs:101` on 93 of 1,000 | `data = [57]` | A first byte whose 6 low bits are 57 or more asks for 65 to 71 bits per value. `compress_sequence` never writes such a header. |

The last row is the one finding of interest. `StoredSparseVector::decode_untrusted_bytes` in
`lib/segment` says that a malformed blob from outside the storage is a user error and never a
crash, and its path reaches `decompress_sequence`. In the test profile the decoder panics at a
debug assertion. With `--release` the same 1,000 inputs gave no panic: the decoder returns values
and no error. What the caller then does with those values was not checked, and nothing was
posted upstream.

### The tests

`cargo test -p hydo-contracts --test it fuzz::` runs the parts on their own and one run of the
fixture `hydo/crates/hydo-contracts/tests/fuzz-crate`. The fixture has five functions with
contracts, and four of them have a planted bug: a debit applied twice, a sum that overflows, a
wrong length bound, and `>` for `>=`. The test checks each smallest input, the same report
twice, and the replay of an input.

## Inside the virtual machine

A function that touches the disk, the network, the clock, or threads gives other results under
faults. The design runs the same harness inside hydo's VM, where the faults come from the
schedule and the run is deterministic. This part is not built (open item 139).

1. **The harness for the guest.** `hydo fuzz --target <triple>` builds the copy's test program
   for the guest: `aarch64-unknown-linux-musl` on the Mac engine and `x86_64-unknown-linux-musl`
   on hvx and KVM, statically linked as the contracts-ledger example's `build.sh` does.
2. **A runner instead of a process.** The driver hands each batch of inputs to a runner instead
   of a process. The runner writes an image whose workload runs the harness on the batch.
   The inputs file and the program go into `root.files`, and the folder that the function
   writes in goes on the data disk. It then runs the image under one schedule. The harness writes its answers to the
   console with a marker, and the runner reads them back from the run's console. Each shrink
   step is one run of the image.
3. **The faults.** The schedule names them as for any run: `disk=ioerr:<p>` for I/O errors,
   `T=<step>:<seconds>` for a clock step between batches, `P=workload@<step>~<ms>:<ms>` for a
   pause inside a call, and `pct=` or `tj=` for other thread interleavings. A finding is then a
   pair of an input and a schedule, and both replay it.
4. **What a call can observe.** A clause reads memory. A failed `fsync` that a function hides,
   or a write that a power cut loses, shows only after a recovery boot. For those, the harness
   needs a second phase that reads the state back in the recovery boot and judges a property
   function there. That phase is designed here and not built.

The first targets are the error paths that native runs never reach. Examples are a read error
taken for a missing file, a dropped `fsync` error, and a duration on the wall clock across a
pause.

## Limits

- Rust only: hydo reads only `.rs` files for fuzzing. A Go harness would be a `_test.go` file
  in the function's package, by the same design.
- The language has no quantifier, so `delta_pack`'s "the input is sorted" cannot be stated. A
  property function that sorts its input first is the way around it.
- No floating-point values and no enumerations. A generic function needs a `with T = <type>`
  line for each type parameter (Milestone 282), and a method of a trait is not fuzzed.
- A result that borrows from a `&mut` parameter was not tried.
- The reading of the sources is textual. A structure defined by a macro is not found, and a
  field type of a structure in another module is written as that module writes it.

## Sources

- K. Claessen and J. Hughes, QuickCheck: a lightweight tool for random testing of Haskell
  programs. ICFP 2000. Properties as functions, random generation from types, and shrinking.
- proptest, a Rust crate (https://github.com/proptest-rs/proptest): strategies and shrinking for
  Rust values.
- B. Meyer, Object-Oriented Software Construction, 1988, and the Eiffel language. Design by
  contract: preconditions and postconditions on routines.
- I. Ciupa, A. Leitner, M. Oriol, and B. Meyer, ARTOO: adaptive random testing for
  object-oriented software. ICSE 2008. It describes the Eiffel AutoTest tool. Contracts are the
  test oracle, and inputs that break a precondition are discarded.
