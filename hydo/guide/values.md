# Value history: follow one value through a run

Use this page when a check fails at the end of a run and you need to know where a value went
wrong on its way. A value history records chosen values at chosen functions, at every call,
while the whole system runs under faults. `hydo inspect values` then shows each value in the
order the run made the calls, grouped by a key such as a transaction's reference, and marks
each change.

The record reads the program's memory at hardware breakpoints and never writes to it. A run
with the record plan has the fingerprint of the same run without it. The Mac engine and the KVM
engine make the reads; hvx has no breakpoints yet. The program keeps its debug information, and
its sources sit next to the manifest.

## 1. Declare the values next to a function

Write `// hydo:` comments right before the function, as for a function contract:

```go
// hydo: key transaction.Reference
// hydo: record transaction.OverdraftLimit as overdraft_limit
// hydo: record transaction.MetaData["overdraft_limit"] as meta.overdraft_limit at return
func setTransactionMetadata(transaction *model.Transaction) {
```

| Line | Meaning |
|---|---|
| `// hydo: key <expr>` | What the history follows, read at the call's entry: a reference, an id. One per function. |
| `// hydo: record <expr>` | A value read at the call's entry and again at its return. |
| `... as <name>` | The value's name. Values of one name in several functions are one history. |
| `... at entry`, `... at return` | Read the value only there. |

An expression is the one of a contract clause: parameters, `result`, fields through pointers,
`len(s)`, `v[0]`, `some(o)`, and arithmetic. A record line also reads these:

- a floating-point number (`float64`, `float32`, Rust `f64`);
- a string, shown up to its first 32 bytes;
- a Go interface (`any`, `interface{}`, `error`) by its dynamic value: `float64`, `float32`,
  `int`, `int64`, `int32`, `uint`, `uint64`, `uint32`, `uint8`, `bool` and `string` show their
  value, other types show their type's address;
- a Go map's value by a constant key, `m["key"]` or `m[7]`, in a map of at most 8 entries. A key
  that is not in the map shows `absent`. A larger map shows `unreadable`.

A field read through a pointer parameter at return is its value at return. That is how a record
shows what the function changed.

## 2. Name the program and its sources in the manifest

The manifest's `contracts:` entry names the program in the guest and its source folders:

```yaml
contracts:
  - program: /bin/blnk
    sources: ["../work/blnk-values"]
```

`hydo build` compiles the plan, `<image>.contracts.json`, and says how many values it records:

```text
hydo-build: .hydo/explore-ob-blnk-values-arm64.cpio.gz.contracts.json (function contracts: 3 functions, 0 clauses, 10 recorded values)
```

A function the compiler copied into its callers has no breakpoint of its own and is listed as
not checked. Record its caller or its callee instead. The Mac engine stops at 4 functions with
values or contracts in one program, with one breakpoint left for returns.

## 3. Run, then look at the history

`hydo run` writes the records into the run directory. `hydo inspect values` reads them with the
plan, without a VM:

```sh
hydo inspect values hydo-out/run --changes --text
hydo inspect values hydo-out/run --key d17 --text
```

| Option | What it does |
|---|---|
| `--key K` | Only the history of key `K` (or of its copy). |
| `--name N` | Only keys that record the value `N`. |
| `--changes` | Only keys where a value changed. |
| `--plan FILE` | The plan, when the image is not where `result.json` says. |
| `--limit N`, `--offset N` | Pages of keys (50 by default). |

Each line is one stop: the run's point (`run:812`, the form `hydo inspect start --at` takes),
the function, `entry` or `return`, the caller, and each value. A key that extends another key
after `_`, `-`, `.`, `:`, `/` or `#` belongs to its history: the queue copy `d17_q` of the
transaction `d17` is in the history of `d17`. A changed value reads `(CHANGED from <before>)`,
and a `change:` line names the two stops between which it changed.

Without `--text` the answer is one JSON document of the type `values` (results format 1). `hydo
inspect events` lists the same stops as events of the kind `value`. `hydo report --html` adds
the section "How the recorded values changed" with the keys that changed.

## What a record cannot read yet

- A map of more than 8 entries, a map whose key is neither a string nor an integer, and a value
  inside a struct inside a map.
- An interface's value of a type outside the list in section 1, and a pointer stored in an interface.
- A function the compiler inlined everywhere.
- Calls past the first 100,000 of a run, as for contracts.
