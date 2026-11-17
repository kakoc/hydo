# Value history

Status: a first version (Milestone 285) on the Mac engine, opt-in; value properties since
Milestone 291. The KVM engine makes the same reads with
its breakpoints, and no KVM run was made. On hvx it is designed only, with the breakpoints of
[the contracts' hvx design](contracts.md#the-hvx-design).

A check that fails at the end of a run says that the state is wrong, not where it went wrong.
A value history records chosen values at chosen functions, at every call, while the whole
system runs under faults. It then shows each value in the order of the calls, grouped by a key
such as a transaction's reference, and marks each change. The user's question was how one field
of a structure travels through the program; the history answers it for the fields a developer
names.

```go
// hydo: key transaction.Reference
// hydo: record transaction.OverdraftLimit as overdraft_limit
// hydo: record transaction.MetaData["overdraft_limit"] as meta.overdraft_limit at return
func setTransactionMetadata(transaction *model.Transaction) {
```

## Decisions

1. **The declaration is a comment next to the function, as for a contract.** A `record` line
   names a value, and a `key` line names what the history follows. The lines go through the
   contracts' plan compiler, so a function can have clauses and records together. The other way
   was a `values:` list in `hydo.yaml`. It would name each function's file and line a second
   time, and the comment keeps the value next to the code that changes it.
2. **The engine only reads.** A record is a set of reads of the plan (`<image>.contracts.json`)
   at the breakpoints the contracts use. The engines need no change for it: they make every read
   the plan lists, and the host builds the values after the run. The one engine addition is the
   number of the boot in each record (`boot`), so that a call after a power cut is placed in the
   recovery boot. A run with the plan has the fingerprint of the run without it.
3. **A value has a name, and a name is one history across functions.** `as overdraft_limit` in
   three functions gives one line of values per call. A value without `as` is named by its
   expression.
4. **A key groups the calls.** A key is a string or a number read at the call's entry. A key that
   extends another key of the run after `_`, `-`, `.`, `:`, `/` or `#` is that key's copy and is
   in its history. Blnk's queue copy of the transaction `d17` has the reference `d17_q`.
5. **The view marks changes.** Between two stops of one key, a value whose text differs is a
   change: `hydo inspect values` prints it as `(CHANGED from 50.0)` and lists the two stops.
   An unreadable value is no change.

## The readers

The plan's compiler reads the program's DWARF debug information (contracts.md, "Reading
values"). For the history it reads four more kinds of values. Each is a fixed list of reads that
an engine makes at a stop:

- **A floating-point number.** A `float64` or `float32` in memory is read as its 8 or 4 bytes. The
  host shows it and compares it as an IEEE 754 number. Contract clauses can now compare floats
  too: `requires t.Limit > 0.5`. A float in a floating-point register is not read, because the
  engines read the integer registers only.
- **A string.** Its length and its first 32 bytes, in 4 reads of 8 bytes. A longer string is shown
  cut.
- **A Go interface's dynamic value.** An `interface{}` (`runtime.eface`) has its type's
  descriptor at word 0. An interface with methods, such as `error` (`runtime.iface`), has an
  itab there, whose type is at offset 8. The value is behind the data word. Go's linker writes
  each type's `DW_AT_go_runtime_type`, an offset in the section of type descriptors (`.go.type`
  since Go 1.26). The compiler adds that section's address, and the plan lists the descriptors
  of 11 types: `float64`, `float32`, `int`, `int64`, `int32`, `uint`, `uint64`, `uint32`,
  `uint8`, `bool` and `string`. The host compares the recorded type word with them. A test
  checks each address against the descriptor's size and kind in the program's file.
- **A Go map's value by a constant key.** Go's linker describes a map as `map<K,V>` with
  `dirPtr` and `dirLen`, the table type, and the group type (`noalg.map.group[K]V`), whose slots
  are an array of `{key, elem}` structures (or, with Go's split groups, the arrays `keys` and
  `elems`). A map of at most 8 entries is one group: `dirLen` is 0 and `dirPtr` points at the
  group. The plan reads the group's 8 control bytes, each slot's key, and each slot's value. The
  host finds the full slot (a control byte below 128) whose key is the constant. A lookup by
  the hash is not possible: the hash function takes a seed that the runtime draws for each map.
  A map of more than 8 entries is shown as unreadable, with the reason.

A record also reads the call's return address (x30 on arm64, the top of the stack on x86_64).
The compiler finds the direct calls of each recording function in the program's code (a `BL` on
arm64, a `CALL rel32` on x86_64) and names the function that holds each call. The history then
says which function made the call: `createQueueCopy, called from buildQueuedCoalescingBatchForScope`.

## The view

`hydo inspect values <run dir>` reads the run's records (`one.contracts.jsonl`) and the plan
next to the image. It prints one JSON document of the results type `values`, or with `--text` a
view per key:

- every stop of a recording function, in the run's order (by boot, then by the engine's clock);
- the point of each stop (`run:812`, the form `hydo inspect start --at` takes) and the workload's
  steps then, from the transcript's `workload steps a..z` notes;
- the caller, and the values, each change marked.

`hydo inspect events` lists the same stops as events of the kind `value`. `hydo report --html`
adds the section "How the recorded values changed" with each key that changed.

## Value properties

Since Milestone 291 a history can fail a run. A `keep` line next to a recording function says
what must hold for a recorded value along each key's history:

```go
// hydo: keep overdraft_limit from blnk.setTransactionMetadata to model.canProcessTransaction
```

The line's grammar is `keep <name> [once set] [from <function>] [to <function>] [across boots]`.
Each stop of `to` (every stop without it) that recorded the value is judged against a reference
stop of the same key:

- Without `once set`, the judged stop records the reference's value. The reference is the last
  earlier stop of `from` that recorded the value, or without `from` the key's first such stop.
- With `once set`, the reference is the last earlier stop where the value was present, and the
  judged stop breaks the property when its value is `absent` or `nil`.
- The reference is in the judged stop's boot, unless the line says `across boots`.
- An unreadable value is neither judged nor a reference.

The decisions:

1. **The property is a comment line, as the records are.** The other way was a `values:` list in
   `hydo.yaml`. A property names values by the `as` names of the `record` lines and functions
   by their names, so it belongs to the same sources. The plan's compiler checks both names.
   `hydo build` lists a property as not checked when no `record` line records its value, or
   when a function it names records no such value. The line can stand next to any recording function. `to` and
   `from` say which functions take part, so the line reads the same wherever it stands.
2. **The engine does not change.** The properties are in the plan (`Program.keeps`), which only
   the host reads. They are judged on the host after the run, from the same records, so a run
   with them has the fingerprint of the run without the plan.
3. **A broken property is a finding of the kind `values`.** It turns an `OK` verdict into
   `VIOLATION [values <the line as written>] the value changed` (or `the value was absent after
   it was set`). The verdict names no key and no value. A key is a random reference such as
   `c27`, and two runs that break the property at two keys must give one signature, or the
   shrinker treats them as two failures. The key, the values, the judged stop, the reference and
   the first stop whose value broke the property are in the finding's `details`. Its `output`
   is the first broken key's history up to the break, which `hydo report` shows like a check's
   output.
4. **The first changing call is named.** The judged stop is where the property breaks; the call
   where the value first left the reference's value is often earlier. For Blnk the judged stop is
   the decision, and the first changing call is the worker's coalesced copy.
5. **The class stays the faults' class.** The explorer's classes describe the faults of a
   schedule (`lying-disk`, `durable-defaults`). A values finding without faults is
   `durable-defaults`, as a business rule's is. The advice knows the kind: its title is
   "<system> broke a value property you declared", and it points at the history.

## The proof on a real service

Blnk, a ledger in Go, ignores a draw's overdraft limit when its worker coalesces the draw with
others (`hydo/onboard/examples/blnk`, RESULTS.md, finding 1). The rule over Blnk's tables saw
only the end state, and the contract of Milestone 260 named the call but not the cause: the
limit is a `float64`, and Blnk saves its flags in a `map[string]interface{}`. The example's
`values.patch` records the limit, the flag, and both of their metadata entries at three
functions: `setTransactionMetadata` in the API, `createQueueCopy` in the API and in the worker,
and the overdraft decision `canProcessTransaction`.

On the Mac engine, the saved regression `overdraft-limit-concurrent-draws` gave the same run
with the plan and without it, twice each: `f3cbeb9c85945f5c`, 10,288 stops, and `VIOLATION
[no_overdraft]`. The two runs with the plan wrote the same records. Of the run's 22 keys, two
changed, and they are the two draws the rule found past the limit:

```text
key "c27" (and c27_q), first seen in workload steps 0..29
  run:8007    blnk.setTransactionMetadata return, called from blnk.(*Blnk).QueueTransaction:  overdraft_limit = 50.0  allow_overdraft = true  meta.overdraft_limit = absent  meta.allow_overdraft = true
  run:8007    blnk.createQueueCopy entry, called from blnk.(*Blnk).processSingleTransaction:  overdraft_limit = 50.0  allow_overdraft = true  meta.overdraft_limit = absent  meta.allow_overdraft = true
  run:9103    blnk.createQueueCopy entry, called from blnk.(*Blnk).buildQueuedCoalescingBatchForScope:  overdraft_limit = 0.0 (CHANGED from 50.0)  allow_overdraft = true  meta.overdraft_limit = absent  meta.allow_overdraft = true
  run:9103    model.canProcessTransaction entry [c27_q], called from model.UpdateBalances:  overdraft_limit = 0.0  allow_overdraft = true
```

The limit enters the transaction at 50.0, never enters the metadata, and is 0.0 in the copy that
the worker's coalescing makes from the row in PostgreSQL. The flag reaches the decision, and the
limit does not. On the build with the fix no key changed: the metadata holds `overdraft_limit =
50.0`, and the coalesced copies reach the decision with 50.0. That run gave `31feeab9f802ef32`
and OK, twice with the plan and twice without it.

These runs used images of the fingerprint epoch 3. On the image of the epoch 4 (Milestones 269
and 283) the regression gave `ddb6f1730f6854ea` twice with the plan and twice without it. Four
keys lost the limit in their coalesced copies, and the two whose decision saw 0.0 are the rule's
two rows.

The history found the lost limit in a passing run too. In `disk=none | run` on the buggy build
(`e688c9f3241219f1` with the plan and without it, OK), 3 of 65 keys changed: their coalesced
copies had the limit 0.0. One of them reached the decision with 0.0, and its credit line stayed
above the limit by chance. No rule over the end state can see that run as wrong.

The cost: the regression stopped at 98 entries and 44 returns, 142 breakpoint stops with their
single steps. A stop makes at most 115 reads. The engine's time was 5.5 s and 5.1 s with the
plan, and 5.2 s and 5.5 s without it, so the cost is below the spread of the runs.

The property of Milestone 291 is one line next to `canProcessTransaction`: `keep overdraft_limit
from blnk.setTransactionMetadata to model.canProcessTransaction`. At the fingerprint epoch 4 it
broke the regression for `c27` and `c29`, and it failed `disk=none | run`, which every rule
passed, for 3 of 66 keys. Each schedule gave one fingerprint with the plan and without it, twice
each (`ddb6f1730f6854ea` and `f52b32d3577045db`), and the fixed build held. In a blind
exploration of 30 schedules the property broke in 9, first at seed 4, and the overdraft rule in
1, at seed 21. Seed 4 shrank to a schedule without a fault. The Blnk example's RESULTS.md has
the runs ("The limit as a property that a run can break").

## Next steps

- Properties over two names (a value against its copy in another field), a comparison with a
  constant, and an order of calls.

- KVM: the plan's 3 functions with one return need 4 debug registers, and the KVM engine has 3.
  A record without return reads needs no return breakpoint; a plan of 3 functions at entry only
  fits.
- hvx: the side module of the contracts' hvx design.
- Inlined functions: stop at each inlined copy's first address, with the parameters' locations of
  the inlined instance.
- Larger maps: follow the directory and the tables, up to a bound, as the elements of a vector.
- More dynamic types: name any type by its descriptor from the debug information.
