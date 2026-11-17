# Function fuzzing from contracts (Milestone 275)

Status: a prototype for Rust, natively, on this Mac (Milestone 275, tentative number). Since
Milestone 292 (tentative number) the calls also run inside hydo's virtual machine under faults,
a prototype on hvx ([Inside the virtual machine](#inside-the-virtual-machine)).

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
- a parameter of a type it cannot generate: enumerations, references inside structures, a
  recursive structure, `impl Trait`. Floating-point values are generated since Milestone 289.

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

Status: a prototype since Milestone 292 (tentative number), run on hvx, and on the Mac engine
since Milestone 296 (tentative number). Milestone 296 also puts faults inside a call. The code is
the module `fuzz::vm` of `hydo-contracts`, and the command is `hydo fuzz --vm`. `hydo guide fuzz`
is the user's reference.

A function that writes files, syncs them, or reads the clock gives other results under faults.
`hydo fuzz --vm` makes the calls inside hydo's VM, where a power cut, a disk error, a clock step
or a pause can meet a call, and the run is deterministic. The generator, the harness, and
the judge of `ensures` are the native ones. Only the place of the calls changes.

### How a run goes

1. **The harness for the guest.** hydo builds the copy's test program for
   `x86_64-unknown-linux-musl` (hvx and KVM) or `aarch64-unknown-linux-musl` (the Mac engine),
   statically linked. It sets `CARGO_BUILD_TARGET` and the target's `RUSTFLAGS` and calls the
   native build.
2. **A project of the calls.** hydo generates the inputs as natively and writes a project:
   - `cases/<step>/calls` holds the inputs of one workload step, `--batch` calls (default 25);
   - `run.sh` is the workload: each call is one process of the harness, in the folder
     `/data/fuzz/<step>.<call>` on the data disk;
   - `dirs.sh` makes those folders at provisioning, which ends with a `sync`, so that no power
     cut takes away a folder that no function made;
   - `check.sh` is the one invariant, the second phase below.

   `hydo build` makes the image, which holds the program and the inputs.
3. **The schedules.** `hydo run` runs the image under each schedule. The first has no fault, and
   the others draw one kind each, in turn:
   - a power cut: `disk=cut` with `C=<step>`, with `cf=<k>` at the disk's k-th flush on hvx, or
     inside a call;
   - disk errors: `disk=ioerr:<p>`, half of them with a `C=`;
   - a clock step: `T=<step>:<seconds>`, or inside a call;
   - a pause: `P=workload@<step>~<ms>:<ms>`, or of the call's process or a thread it started,
     inside a call;
   - a kill of the call's process, inside a call.

   The next section says how a fault comes inside a call.

   `--schedule S` names the schedules instead.
4. **The answers.** A call's answer is an id `<step>.<call>=<answer>` in `$HYDO_ACKED`. The run
   reports that file at its end and at a power cut (`hydo-explore-acked:`) and carries it into
   the recovery boot. The console is a poor channel: the hvx module keeps the first 64 KiB of a
   VM's console (`HVX_CONSOLE_SZ`). An answer longer than 64 bytes goes to the console all the
   same, at most 32 KiB of them per boot. The reason is that a recovery boot on hvx with about
   90 KiB of carried ids did not start ("What is left").
5. **The judgment.** The host decodes each answer and judges it with the function's `ensures`.

### Faults inside a call

Milestone 292's clock steps and pauses came between workload steps, and a call ends inside its
step, so they never met a call. Its evaluation found that they tested nothing. A function that
measures a timeout or a lease with the wall clock, or waits for a thread it started, fails only
when the fault comes while it runs. Milestone 296 places such faults with hydo's reactions
(`A=`, Milestone 139), whose delay counts from an event that the guest reports:

1. The schedule names one call with the knob `E=<step>.<call>`. The project's manifest declares
   the knob, and its `set` command writes the value to `/tmp/fz.event` in the guest.
2. `run.sh` gives that call `HYDO_FUZZ_EVENT=1` and starts it as `/tmp/fuzz-call`, a link to the
   harness. Right before the call, the harness writes the name `hydo-call` to its thread's
   `/proc/thread-self/comm` and the record `HYDO-ASSERT {"t":"reachable","id":"fuzz.call",...}`
   to the console. The threads that the function starts inherit the name.
3. `A=fuzz.call+<ms>:<action>` acts `ms` milliseconds of guest time after the controller sees
   the record. Milestone 296 adds four actions to the reactions (`hydo_schedule::ReactAction`):
   - `clock+<s>` and `clock-<s>`: the clock step of `T=`, at that moment;
   - `stop-workload-<ms>`: a stop and a continue of the workload's tree with signals, as
     `stop-<svc>-<ms>` does for a service;
   - `stop-workload/<thread>[#<k>]-<ms>`: the cgroup freezer of `P=workload/<thread>`
     (Milestone 226), for the threads of that name;
   - `kill-workload/<name>[#<k>]`: a kill of the workload's processes of that name with
     `SIGKILL`, while the workload goes on.

   `cut` was there before.

The drawn actions are `cut`, a clock step, `stop-workload-<ms>`,
`stop-workload/hydo-call#2-<ms>` (the first thread that the function starts), and
`kill-workload/fuzz-call`. Only the named call reports its start, so the console gets one record
per run. On hvx a VM's console keeps 64 KiB, and a record per call would fill it. A kill that
comes after the call's end meets no other call, because only the named call runs as `fuzz-call`.
hydo does not judge the `ensures` lines of a killed call, and its durability check runs with
`None`.

The schedules hold the whole fault, so the shrink keeps it. `one_step_schedule` turns `E=` into
`E=0.0` for the input alone. A fault inside a call meets one call per run, so each smaller input
gets a run of its own on its chunk's image, with `E=0.<k>` naming it. A failure under a timed
fault of the whole run often passes alone. The shrink then tries the next two schedules under
which the call failed.

The reaction's delay is in guest time, but the controller sees the record only at its next stop.
A probe on hvx measured when a clock step reached a call that slept 1 ms at a time. With no delay
it came 1 ms after the call's start, with 20 ms 32 ms after it, and with 100 ms 113 ms after it.
The same in a busy loop gave 5, 29, and 109 ms.

Two scripts of the controller had to change for faults this early in a call:

- busybox's `timeout` leaves a watcher process that leads a session of its own and has the
  workload's environment. Milestone 226's freeze takes the first leader with `HYDO_FROM` in the
  order of `/proc`'s names. In a shrink run it took a watcher, found only `timeout`, and stopped
  nothing. The reaction's freeze and kill take the leader whose session has the most processes.
  The timer's freeze keeps its script, so that its runs send the same commands.
- A thread that ends between the freeze's listing and its move ended the Mac run in
  `ERROR P=workload/hydo-call#2: unexpected output "sh: write error: No such process"`. The
  reaction's freeze leaves such a thread out. The timer's freeze keeps the error (open item 80).

### The second phase: `durable`

`// hydo: durable <check>` names a property function of the crate. hydo calls it after the
run: in the recovery boot after a power cut, and at the run's end without one. It calls the
check once for each call that started, in that call's folder. The check's parameters are the
function's, then the call's result as an `Option`:

- `Some(result)` for a call that returned before the cut;
- `None` for a call that did not return: the cut came during it, or it panicked or was stopped.
  A result longer than 64 bytes on the wire also reaches the check as `None`.

The check returns `bool`, or `Option<bool>` with `None` for a call it cannot judge, and carries
`// hydo: ensures result`. Native `hydo fuzz` does not fuzz a check, and `--list` names it.

`run.sh` lists a call in `$HYDO_ACKED` when it starts and again with its answer when it
returns. A power cut reads the file before the recovery boot, so the check knows each call that
started and each result. `check.sh` writes the checks' answers to the console, about 2,000
bytes per line (`hydo-fuzz: checks`).

The check is Rust code, not a clause. The language of the clauses reads values and calls no
code of the program, and a durability property must read the state after the cut. The check
takes the call's own input, so it knows what the call promised.

### How a failure shrinks and replays

A failure is a pair: the input, and the schedule under which the call or its check failed. The
shrink first runs the input alone in an image of one step. A fault at or before the failing
call's step moves to step 0, and one after it to step 1, the end of the one step
(`one_step_schedule`). If the input alone fails the same way, the shrink tries simpler inputs,
`--batch` of them per image and run, and keeps the first that fails the same way. It stops
after `--shrink-runs` runs (default 16). If the input alone passes, the report names the whole
run as the replay and says so. A schedule with `cf=` is not shrunk: the flushes count from the
workload's start, so the call keeps its place in its run.

The image holds the inputs and the schedule holds the faults, so `hydo run <image>
'<schedule>'` repeats the failing run. hydo runs the replay once more and reports whether it
gave the same fingerprint.

### Proof

The example is `hydo/onboard/examples/fuzz-vm`, three functions of one crate with their checks:

- `commit_all` commits lists of keys as write transactions of redb 4.6.0, an embedded key-value
  store, with its default durability. Its check says three things. Every commit that returned
  `Ok` is in the database. The transaction after them is there whole or not at all. No later
  one is there.
- `save` saves a text with tempfile 3.27.0: it writes a temporary file and renames it over
  `saved.txt`, and syncs nothing.
- `save_synced` syncs the temporary file before the rename and the folder after it.

redb is a popular embedded store, and its commits promise durability with an `fsync`. The save
by rename is a common way to write a file in Rust, and its durability depends on two syncs that
the code must make itself.

The runs used the bare-metal machine's hvx, 200 inputs per function (600 calls in 24 steps),
and 24 schedules per seed. Natively, `hydo fuzz` held every clause of the three functions.

| Function | Seed 1 | Seed 2 |
|---|---|---|
| `commit_all` | held: 4,465 checks, 1,465 of them after a power cut | held: 4,569 checks, 1,169 of them after a power cut |
| `save` | broken on 750 calls in 4 runs, first under `disk=cut \| run C=14`; shrunk to `text = ""` | broken on 750 calls in 4 runs, first under `disk=cut \| run C=14`; shrunk to `text = ""` |
| `save_synced` | held: 3,675 checks, 675 of them after a power cut | held: 3,905 checks, 505 of them after a power cut |

Five `cf=` cuts came in the middle of a call: four in a commit of `commit_all` (`cf=692` and
`cf=236` of seed 1, `cf=1505` and `cf=913` of seed 2), and one in `save_synced` (`cf=3748`). Each
check of the call that did not return found the database's transactions whole and in order, and
the file whole or absent. Each seed took 452 seconds. This time includes the shrink and the replay. The replay of
`save`'s smallest input gave its fingerprint again in both seeds (`dd2260cccca16aa8` and
`11464c875d1a0163`).

On the way, hydo reported three findings that came from the checks and from hydo, not from
the crates. Each one is a lesson for a user:

- A check opened redb's database, started a read, and dropped the database before reading. Every
  call failed its check, also in the schedule without a fault. The report now says when a
  failure came without a fault: the function or its check is wrong before any fault meets it.
- Under `disk=ioerr`, ext4 aborted its journal at the first error. Each later open of the
  database answered `EIO`, also after the disk's faults were off for the checks, and the check
  counted that as a lost commit. A check that cannot read the state returns `None`.
- `check.sh` compared the ids `0.1` and `0.10` with awk as numbers, which are equal. Nine
  checks got another call's result. hydo's script compares them as text now.

### Proof of the faults inside a call, and of the Mac engine

Milestone 296 added four functions to the example, two with planted bugs and two twins, and two
functions that call crates from crates.io:

- `wait_reply_is_honest` waits for a worker's answer with a deadline read from `SystemTime`.
  Its twin `wait_reply_monotonic_is_honest` reads `Instant`. Both promise that an answer is the
  worker's, and that "timed out" comes only after the timeout passed on the monotonic clock.
- `count_twice` counts on a worker and on the caller under one lock, and waits 500 ms for the
  worker's "done" instead of joining it. Its twin `count_twice_joined` joins. Both promise a
  total of `2 * n`.
- `crossbeam_recv_is_honest` makes the same promise for crossbeam-channel 0.5.17's
  `recv_timeout`.
- `snowflake_ids_increase` promises that rs-snowflake 0.6.0's `real_time_generate` gives
  increasing ids, made 2 ms apart by one generator.

Natively all six held. With 40 inputs per function and seed 1 (the schedules are the same on
both engines):

| Functions, `--faults`, schedules | hvx | the Mac engine |
|---|---|---|
| the waits, `clock`, 16 | `SystemTime`: broken on 2 calls in 2 runs, first under `E=0.7 A=fuzz.call+5:clock+86400`; shrunk to `work_ms = 20, slack_ms = 200`. `Instant`: held on 640 calls | the same: broken on 2 calls in 2 runs, shrunk to `work_ms = 20, slack_ms = 200`. `Instant`: held on 640 calls |
| the counts, `pause`, 16 | `count_twice`: broken on 6 calls in 6 runs; shrunk under `E=1.19 A=fuzz.call+2:stop-workload-3000` to `n = 3`. `count_twice_joined`: held on 640 calls | `count_twice`: broken on 5 calls in 5 runs; shrunk under the same schedule to `n = 1`. `count_twice_joined`: held on 640 calls |
| the crates, `clock,pause`, 24 | rs-snowflake: broken on 1 call, under `E=3.15 A=fuzz.call+20:clock-30`; shrunk to `n = 12`. crossbeam-channel: held on 960 calls | rs-snowflake: broken on 2 calls in 2 runs, first under the same schedule; shrunk to `n = 12`. crossbeam-channel: held on 960 calls |

Each shrunk finding's replay gave its fingerprint again. rs-snowflake's id went back with the
clock: its code has a comment for a clock that moves backwards and no check for one. It is a
candidate, not posted.

On the Mac engine, M292's three functions gave M292's results. 200 inputs per function, 24
schedules per seed:

| Function | Seed 1 | Seed 2 |
|---|---|---|
| `commit_all` | held: 4,576 checks, 1,376 of them after a power cut | held: 4,800 checks, 1,400 of them after a power cut |
| `save` | broken on 373 calls in 3 runs, first under `disk=cut \| run C=14`; shrunk to `text = ""` | broken on 925 calls in 5 runs, first under `disk=cut \| run C=14`; shrunk to `text = ""` |
| `save_synced` | held: 3,300 checks, 100 of them after a power cut | held: 3,759 checks, 359 of them after a power cut |

The replays of `save`'s smallest input gave their fingerprints again (`a373bb16002967d6`, and
`ba19400d6860bca7`). A seed took 251 and 239 s.

### The evaluation: would a Rust developer keep it

The question was whether a developer would keep `hydo fuzz --vm` after the first runs, and for
what. The evaluator wrote the example crate as a user would, ran it, and read the reports.

**Time to the first useful run.** On hvx, about one minute from `hydo fuzz --vm` to the first
report. The harness for the guest built in 5 to 9 seconds (35 seconds on a loaded machine),
and the image in 4 to 5 seconds. A run of 600 calls took 10 to 16 seconds. 24 schedules with
their shrinks took about 8 minutes. The missing syncs of `save` showed in the second schedule.

**What it found.** One bug class, and the absence of another:

- the save without a sync lost its text at every power cut after the call;
- redb's commits and the synced save held under every power cut, also in the middle of a
  commit (`cf=`), and redb's transactions were whole after each cut.

Neither is new. Native fuzzing can find neither: its runs have no power cut.

**The friction.**

1. The check is a second function with a fixed signature. A wrong signature gets a message
   that prints the signature hydo wants, which was enough to fix it.
2. A function under test works in the current folder. A function that takes a path needs a
   wrapper that passes `.`.
3. A check is easy to get wrong in a way that looks like a durability bug. Two findings of the
   first runs were bugs of the example's checks. The note on a failure without a fault, and the
   shrink's note that the input passed alone, told them apart.
4. Disk errors made the file system read-only at the first error, so each later call of that
   run met the same error path. They tested how a function reports an error, and little else.
5. Clock steps and pauses came between steps, never inside a call. No function of the example
   read the clock, so they tested nothing here. Milestone 296 puts them inside a call.
6. On hvx a long answer, such as an error message with a path, is not read past 32 KiB per
   boot. 212 of 600 answers of a run with disk errors were not read.

**The verdict.** Keep it for one job: a function that writes files and promises durability or
atomicity, such as a save routine, a log, or a commit to an embedded store. It finds the missing
sync that native fuzzing cannot find, and each finding comes with a small input and a replay.

After Milestone 296 it is worth it for time and threads too, with `--fn` and `--faults`. The
planted wall-clock deadline and the missing wait broke within 16 schedules on both engines. Their
twins held. It found a crate's ids that go back with the clock. A blind run over all of a
crate's functions gives each function few faults inside its calls, because one call per run
gets one.

### What is left

The run with faults is a prototype (open item 139):

- It ran on hvx and on the Mac engine (Milestone 296). KVM did not run it.
- A fault inside a call comes 1 to 13 ms after its drawn delay, at the controller's next stop.
  One call per run gets it. Several vCPUs were not tried.
- A schedule with `cf=` is not shrunk, and a failure whose input passes alone under its first
  three schedules is not shrunk.
- The answers' channels are small on hvx: 64 KiB of console per VM, and a recovery boot with
  about 90 KiB of carried ids hung with `HVX_HC_PUT: No space left on device` (open item
  120). Long answers are dropped past 32 KiB per boot, and their checks get `None`.
- The schedules come from `hydo fuzz --vm`'s own draw, not from `hydo explore`, and their
  `cf=` values are drawn without the run's count of flushes.
- The rs-snowflake candidate is not posted.

## Limits

- Rust only: hydo reads only `.rs` files for fuzzing. A Go harness would be a `_test.go` file
  in the function's package, by the same design.
- The language has no quantifier, so `delta_pack`'s "the input is sorted" cannot be stated. A
  property function that sorts its input first is the way around it.
- No enumerations; floating-point values are fuzzed since Milestone 289. A generic function
  needs a `with T = <type>` line for each type parameter (Milestone 282), and a method of a
  trait is not fuzzed.
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
