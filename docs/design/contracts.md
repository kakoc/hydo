# Function contracts (Milestone 241)

Status: a prototype on the Mac engine, opt-in. An image without `contracts:` in its manifest
gets no plan, and its runs are unchanged.

A function contract is a set of comments next to a function. The comments say what must hold
when the function is called (`requires`) and when it returns (`ensures`):

```rust
// hydo: requires amount > 0
// hydo: ensures req_id > old(l.last_req) || l.balance == old(l.balance)
// hydo: ensures result == l.balance
// hydo: input amount in 1..=10_000
#[inline(never)]
fn apply(l: &mut Ledger, c: &mut Conn, req_id: u64, amount: u64) -> u64
```

hydo checks every call of the function while the whole system runs under faults. The program's
code and build stay as they are; the program only keeps its debug information. A broken clause
is a finding of the run, and the run replays from its schedule with the same fingerprint.

## Decisions

1. **The comments are the specification.** A contract is `// hydo:` lines right before a
   function declaration. Comment lines, attributes and Go directives may stand between them. The
   function is the next `fn` (Rust) or `func` (Go) line. The debug information (DWARF) gives
   the function's file and declaration line, so hydo finds the function by its file, line and
   name. A name alone could match several functions.
2. **The engine stops at the function, and the guest sees nothing.** The engine puts hardware
   breakpoints at each function's first instruction and at the return address of each call in
   progress. It reads registers and memory there and never writes to the guest. The other way,
   uprobes in the guest kernel, changes the guest's state and needs guest kernel options
   ([Where the stops happen](#where-the-stops-happen)).
3. **The engine reads, the host judges.** The plan (`<image>.contracts.json`, written by `hydo
   build`) lists, per function, the entry address, the reads at entry, and the reads at return.
   The engine makes those reads and writes one record per call to `<tag>.contracts.jsonl`. The
   host evaluates the clauses after the run. An engine needs no expression evaluator, and a new
   engine needs only breakpoints and memory reads.
4. **No calls in expressions.** A call such as `balance(from)` would run the program's code at
   the stop, and a check must not change the run. A contract reads fields instead. A later
   version can run a pure function in a branch from a snapshot of the stop
   ([Next steps](#next-steps)).
5. **A broken clause is a finding of kind `contract`.** It turns an `OK` verdict into
   `VIOLATION [contract <function>] <clause> (<file>:<line>)`. The guest's own verdict is kept as
   `verdict.guest`. These are additions to the results format.

## The language

A contract line is one of these three:

| Line | Meaning |
|---|---|
| `// hydo: requires <expr>` | Must hold at the call's entry. |
| `// hydo: ensures <expr>` | Must hold at the call's return. |
| `// hydo: input <name> in <lo>..<hi>` or `..=<hi>` | The domain of a parameter, for active property testing later. The first version stores it in the plan and checks nothing with it. |

An expression names the function's parameters, `result` (in `ensures`), integers, `true` and
`false`. It reads fields with `.`; a field read goes through pointers and references. Rust tuple fields are `.0`,
`.1`. `old(e)` is the value of `e` at the call's entry.

- A parameter in `ensures` is its value at entry. A field read through a pointer parameter is
  read at return, unless it is inside `old()`.
- The operators are, loosest first: `||`, `&&`, the comparisons (`==`, `!=`, `<`, `<=`, `>`,
  `>=`), `+` and `-`, `*`, `/` and `%`, then `!` and unary `-`. Comparisons do not chain.
- Integers are exact: `old(l.balance) - amount` is never wrapped, so `u64` arithmetic in a
  contract cannot overflow.
- `||` and `&&` read their right side only when they must, so `p == 0 || p.x > 1` is decided
  when `p` is null.
- A call or a method call, and indexing, are refused with the reason.

A clause that cannot be read for a call is undecided for that call, not broken. Two examples are
an unmapped address and a call that had not returned when the run ended. The report counts
undecided calls.

## Where the stops happen

Two ways were measured on the example's `apply` function.

(a) Engine breakpoints. On the Mac engine these are the debug registers DBGBVR1 to DBGBVR5.
The spin observer keeps the first pair. Each breakpoint matches at EL0 (user mode) only.
Hypervisor.framework sends a hit to the engine as a debug exception. The engine makes the plan's
reads, steps over the instruction with a single step, and resumes. It counts neither the hit
nor the step as an exit, so the guest's clock does not move. The breakpoint enable bit
(MDSCR_EL1.MDE) is set only while the guest runs. A snapshot and a fingerprint take the guest's
own value.

(b) uprobes in the guest kernel. The guest kernel puts a trap instruction into the program's
code page. Its handler records the arguments into the kernel's trace buffer, and a guest
program reads them.

| | (a) engine breakpoints, Mac engine | (b) uprobes, hvx |
|---|---|---|
| Fingerprint with the check on | The same as without it: 4 of 4 schedules (below) | Another fingerprint: `8f868c6d…` without, `bec43bea…` with |
| The same schedule twice, check on | The same fingerprint (`hydo repro` printed `REPRODUCED`) | The same fingerprint (`bec43bea…` twice) |
| Wall time per stop | 4.8 µs (a hit and its single step): 300,000 stops added 1.45 s (1.75 s against 0.3 s) | 6.9 µs: 800,000 stops added 5.5 s (11.2 s against 5.7 s) |
| Guest time per stop | 0: the run's stops and control transcript are the same | 0 on hvx's exit clock; the handler runs in the guest |
| Lost records | None | 610,788 of 800,000 events lost: the 8 MiB trace buffer filled without a reader |
| What the guest needs | Nothing | A kernel with uprobes and tracefs, and a reader program |

On every engine a breakpoint needs a guest program's link address. A position-independent
program (Rust's musl target builds them on x86_64) also needs its load address. The engines
differ in what they offer:

| Engine | (a) engine breakpoints | (b) uprobes |
|---|---|---|
| Mac (hvm) | Built: 5 breakpoint pairs, debug exceptions to the engine | The guest kernel has no uprobes (`allnoconfig`); a new kernel is a new fingerprint epoch |
| hvx | Needs a module change, on a side module, with DR0 to DR3 (4 breakpoints) and the `#DB` exit | The guest kernel (nixpkgs 6.6.35) has `UPROBE_EVENTS`; measured in the first table |
| KVM | `KVM_SET_GUEST_DEBUG` with hardware breakpoints (4), `KVM_EXIT_DEBUG`; the exit clock must not count these exits | The guest kernel has no uprobes (`allnoconfig`) |

The measurement favours (a). Its checks leave the run as it was, so a finding with contracts on
is a finding of the plain run, and its replay needs no contracts. (b) changes the fingerprint.
It also loses records unless a reader drains the buffer, and that reader is guest code that
changes the guest's scheduling. (a) is limited by the number of breakpoints. Four functions
with contracts fit on the Mac engine, with one pair left for return addresses.

## Reading values

The plan's compiler reads the ELF file and its DWARF with the `gimli` crate. Go's compressed
DWARF sections are inflated. It handles these:

- A parameter's location at the function's first address: a register, a register plus an
  offset (in memory), or the frame base. At that address on arm64 the frame base is the stack
  pointer, and on x86_64 it is the stack pointer plus 8.
- Integer, boolean, enumeration and pointer types, through typedefs and `const`.
- Fields of structures through any number of pointers, and of a structure passed in memory.
  Go's types are in other compilation units than its functions; the compiler follows those
  references.
- The result: Rust's scalar return value in x0 (rax), and Go's first integer result in R0
  (rax). Go's register ABI puts results in the integer registers in order.

A return is matched with its call by the return address (x30 at entry), the page tables
(TTBR0_EL1), and the stack pointer (Rust) or the goroutine (Go: x28). Go moves a goroutine's
stack when it grows, so the stack pointer does not identify a Go call.

The first version does not read these:

- a structure passed in registers (DWARF pieces);
- Rust `String`, `Vec`, `Option` and `Result` beyond their scalar fields;
- Go strings and slices as values, and Go's results after the first;
- inlined functions (the plan names them as not checked);
- locations that need DWARF's expression stack.

## Reporting and replay

`hydo run` prints the calls checked and each broken clause with the first broken call's values:

```text
hydo: function contracts: 14 calls checked against 4 clauses, 1 broken
hydo: contract broken: ledger::apply ensures req_id > old(l.last_req) || l.balance == old(l.balance) (main.rs:35) at 2 of 14 calls
hydo:   first at call 7 (guest clock 18396): amount = 10, req_id = 6, old(l.last_req) = 6, old(l.balance) = 999940, l.balance = 999930, result = 999930
hydo: VERDICT VIOLATION [contract ledger::apply] ensures req_id > old(l.last_req) || l.balance == old(l.balance) (main.rs:35)
```

`result.json` has a finding of kind `contract` with the function, the file and line, the calls,
the violations, and the first broken call's values. `hydo explore` judges each run the same way,
so it groups, reproduces and shrinks a broken clause like any violation.

## Proof on the example

The example is `hydo/onboard/examples/contracts-ledger`: a Rust and a Go account server with the
same planted bug. The server remembers applied request ids per connection, so a client's retry
on a new connection debits the same request again. The end-to-end check passes.

| Program, schedule | Contracts off: fp | Contracts on: fp | Calls, broken |
|---|---|---|---|
| Rust, `disk=none \| run` | `2471cc244ce21feb` | `2471cc244ce21feb` | 12, none |
| Rust, `disk=none \| run P=ledger@5+2` | `2efb8d7751609270` | `2efb8d7751609270` | 14, `ensures` at 2 calls |
| Go, `disk=none \| run` | `6bd3c24d764ceb27` | `6bd3c24d764ceb27` | 12, none |
| Go, `disk=none \| run P=ledger@5+2` | `7fc63116e0286dde` | `7fc63116e0286dde` | 14, `ensures` at 2 calls |

`hydo repro` of the Rust pause run printed `REPRODUCED` with `2efb8d7751609270` and the
contract's `VIOLATION`. A blind exploration of the example's first build (`hydo explore --seeds
1-120`, stopped after 72 runs) found the broken clause in 2 runs, both with a network loss of
about 50% (`N=ledger@2-9:loss,50%`). There the lost answer made the client retry.

The cost was measured with `ledger local 200000`: 200,000 calls of `apply` in one guest process.
A run records at most 100,000 calls; the engine stops at the entry of the rest but sets no return
breakpoint for them. So the run had 300,000 stops. A stop is a breakpoint hit and its single step. Two
runs with the plan took 1.8 s and 1.7 s, and two without it 0.3 s each, with the same fingerprint
`1063af2ded364da9`. On a swapping host this run first stopped the engine: a kick of the spin
observer came during a contract's single step. The observer now leaves the guest alone then
(`Vmm::ct_busy`).

Without a plan nothing changes. The Redis quickstart of the 0.3.2 Mac release, built with this
tree, has the release's bytes and no plan. With this tree's Mac engine its three schedules gave
the recorded fingerprints `d817d69781986987`, `a2b8d71c0c467d15` and `7147e75676d15194`.

## Next steps

- Calls past the first 100,000 of a run are not judged; the report does not say so yet.
- hvx and KVM: engine breakpoints as in the table, and the load address of a
  position-independent program, read once from the guest's page tables.
- More types: Rust `Option`, `Result`, `String` and `Vec`, Go strings, slices and interfaces,
  `len()`, and Go's results after the first.
- Runtime adapters for Python, Node and the JVM: a stop in the interpreter's call path, and the
  interpreter's own structures as the reads.
- Active property testing: from a snapshot at a function's entry, call the function with
  inputs drawn from its `input` domains, in branches that leave the run as it was. The same
  branches can run a pure function that a clause calls.
