# Function contracts (Milestones 241 and 255)

Status: a prototype on the Mac engine (Milestone 241) and the KVM engine (Milestone 255),
opt-in. On hvx it is designed, not built ([The hvx design](#the-hvx-design)). An image without
`contracts:` in its manifest gets no plan, and its runs are unchanged.

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

Since Milestone 255 an expression also has these:

| Form | Meaning |
|---|---|
| `len(s)` | The length of a string, a vector or a slice: Rust `String`, `&str`, `Vec<T>`, `&[T]`; Go `string`, `[]T`. |
| `v[i]` | An element. `i` is a number or an expression; a computed index reaches the first 8 elements. |
| `s == "text"`, `s != "text"` | A string compared with a literal of at most 64 bytes. |
| `is_some(o)`, `is_none(o)`, `is_ok(r)`, `is_err(r)` | The variant of a Rust `Option` or `Result`. |
| `some(o)`, `ok(r)`, `err(r)` | The value inside the variant; fields follow, as in `some(p).x`. |
| `result.1`, a result's name | A Go function's second result, or its result of that name. |
| `nil` | Go's nil: `err == nil` reads whether an `error` holds a value. |

These forms only read. An element past the length, and the value of the other variant, leave
the clause undecided for that call.

- A parameter in `ensures` is its value at entry. A field read through a pointer parameter is
  read at return, unless it is inside `old()`. So are the elements of a borrowed string or slice
  parameter (Rust `&str` and `&[T]`, Go strings and slices). A Rust `Vec` or `String` passed by
  value belongs to the function, which drops it, so its elements are read at entry.
- The operators are, loosest first: `||`, `&&`, the comparisons (`==`, `!=`, `<`, `<=`, `>`,
  `>=`), `+` and `-`, `*`, `/` and `%`, then `!` and unary `-`. Comparisons do not chain.
- Integers are exact: `old(l.balance) - amount` is never wrapped, so `u64` arithmetic in a
  contract cannot overflow.
- `||` and `&&` read their right side only when they must, so `p == 0 || p.x > 1` is decided
  when `p` is null.
- A call of the program's code, and a method call, are refused with the reason. `v.len()` is
  refused with the advice to write `len(v)`.

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
| Mac (hvm) | Built (M241): 5 breakpoint pairs, debug exceptions to the engine | The guest kernel has no uprobes (`allnoconfig`); a new kernel is a new fingerprint epoch |
| hvx | Designed, not built ([The hvx design](#the-hvx-design)): a side module with DR0 to DR3 and the `#DB` exit | The guest kernel (nixpkgs 6.6.35) has `UPROBE_EVENTS`; measured in the first table |
| KVM | Built (M255): `KVM_SET_GUEST_DEBUG` with DR0 to DR3, `KVM_EXIT_DEBUG`, no exit counted ([The KVM engine](#the-kvm-engine)) | The guest kernel has no uprobes (`allnoconfig`) |

The measurement favours (a). Its checks leave the run as it was, so a finding with contracts on
is a finding of the plain run, and its replay needs no contracts. (b) changes the fingerprint.
It also loses records unless a reader drains the buffer, and that reader is guest code that
changes the guest's scheduling. (a) is limited by the number of breakpoints. Four functions
with contracts fit on the Mac engine, with one pair left for return addresses. Three fit on
KVM, with one debug register left for return addresses.

## The KVM engine

The KVM engine (`hydo/crates/hydo-kvm/src/contracts.rs`) puts the breakpoints into DR0 to DR3
with `KVM_SET_GUEST_DEBUG` and `KVM_GUESTDBG_USE_HW_BP`. KVM keeps them apart from the guest's
own debug registers: `KVM_GET_DEBUGREGS`, and so a snapshot and a fingerprint, still give the
guest's values. A hit is a `KVM_EXIT_DEBUG`, which the engine handles inside its run loop:

1. It reads the registers and the guest's memory through the guest's page tables (CR3), and
   records the call or its return.
2. It sets RFLAGS.RF and runs the vCPU again. RF lets the instruction at the breakpoint run
   once; the CPU clears the flag when the instruction completes. A fault in that instruction
   pushes RF set in any case, so the guest's memory is the same as without the stop.
3. It counts no exit, so the guest's clock and every stop of the run are the same.

Without a plan the engine never calls `KVM_SET_GUEST_DEBUG` for contracts, and the vCPU's
debugging stays as it was before Milestone 255. The spin observer uses DR0 and DR1 for its own
steps. While it steps a vCPU, the contracts' breakpoints are out of the vCPU. A call that the
observer steps into, by a jump from a loop without exits, is not recorded. The observer steps
loads, compares, register arithmetic and jumps only, so only a jump into a contracted function
from such a loop is missed. The rewind mode goes back to a checkpoint and runs the same exits
again; the contracts go back with it, so no call is recorded twice.

A return is matched with its call by the return address, which the engine reads from the stack
at the entry, by CR3, and by the stack pointer (8 more at the return) or Go's goroutine
register (r14).

### The load address of a position-independent program

Rust's musl target links x86_64 programs as static position-independent executables. The guest's
kernel loads such a program at an address of its choice, and each process gets its own. The
plan marks such a program (`pie`) and keeps its first 64 bytes, the ELF header (`head`).

The engine finds the load address without the agent and without a guest change. At the first
exit in each address space (each CR3), it walks the user half of that address space's page
tables and looks for a 4 KiB page that starts with the program's header. The program's start-up
code reads its own header, so the page is mapped once the program runs. The header's address is
the load address. In an address space without the program, the walk runs again every 256 exits.
Exits are the engine's clock, so the walks and their results are the same in every run of a
schedule.

Each process of the program has its own load address, and the three entry breakpoints cover at
most three processes. The engine prefers the processes where calls were recorded, then the
address spaces with the latest exits. A call in a process outside those three is not recorded.
The engine's log names the processes found (`hydo-kvm: contracts:` in `engine.log`).

A Go program and an arm64 Rust program are linked at fixed addresses and need no search. The
Mac engine refuses a position-independent program with a message.

## The hvx design

This section is a design only: no module is built or loaded. By AGENTS.md's side-module rule,
the work starts on a side module with its own device, for example `/dev/hvxct`, built like
`hv/hvx/avx/build-hvxavx.sh`, and moves into the main module only after the side module passes
`deploy-metal.sh` and the pinned reproducers with the same values.

The module change has five parts:

1. **An opt-in ioctl.** `HVX_SET_GUEST_DEBUG` takes four addresses and an enable mask. A new
   bit of `HVX_GET_CAPS` says the module has it. Without the ioctl, nothing below happens: the
   exception bitmap, DR7 and the VM-execution controls stay as they are, so every recorded value
   stays the same.
2. **The debug registers.** VMX keeps the guest's DR7 in the VMCS (`GUEST_DR7`), and DR0 to DR3
   are the hardware's own. With breakpoints set, the module saves the guest's DR7 and loads the
   contracts' DR7 before VM entry, and loads DR0 to DR3 next to the guest's other state. "MOV-DR
   exiting" is on, so a guest that reads or writes a debug register gets its own values from the
   module. A snapshot and the fingerprint take the guest's DR7, never the contracts'.
3. **The `#DB` exit.** Bit 1 of the exception bitmap makes a breakpoint a VM exit. The exit
   qualification says which of B0 to B3 hit. The module returns to the run engine (`hydo-hvx`)
   with a new exit reason, `HVX_EXIT_DEBUG`, and the address. The module counts no exit for it,
   so the exit clock does not move. With the PMU branch clock, a breakpoint is a fault before its
   instruction runs, so no branch retires and the clock does not move either.
4. **Going on after the stop.** The engine makes the plan's reads from the registers (`HVX_GET_REGS`) and from
   guest RAM, which it maps already, through the guest's page tables. It then calls the run ioctl
   with a flag that sets RFLAGS.RF in `GUEST_RFLAGS` for the next entry, as on KVM.
5. **The load address.** The KVM engine's search for a position-independent program's header
   runs in `hydo-hvx` unchanged: it needs only CR3 at an exit and guest RAM.

After the change to `hvx_abi.h`, every userspace program is built again against it. The
hand-written test `hvx_contract_test` puts a breakpoint into a small guest image and checks the
records. The proof is the one of this page: each schedule of the contracts-ledger example gives
one fingerprint with the plan and without it, on the side module, and `deploy-metal.sh` against
the side module prints `ALL PASS` with the recorded values. Several vCPUs keep a debug state per
vCPU. A forced preemption (`pp=`, `xt=`) that falls on the instruction at a breakpoint is
delivered after the vCPU goes on, as an interrupt is after any exit.

## Reading values

The plan's compiler reads the ELF file and its DWARF with the `gimli` crate. Go's compressed
DWARF sections are inflated. It handles these:

- A parameter's location at the function's first address: a register, a register plus an
  offset (in memory), the frame base plus an offset, or pieces of these (DWARF's
  `DW_OP_piece`). Rust's frame base is the stack pointer; Go's is the canonical frame address,
  which at the first address is the stack pointer on arm64 and the stack pointer plus 8 on
  x86_64. Milestone 241 took the frame base as the canonical frame address for Rust too, which
  put a Rust parameter on the x86_64 stack 8 bytes off; no recorded plan had one.
- Integer, boolean, enumeration and pointer types, through typedefs and `const`.
- Fields of structures through any number of pointers, of a structure passed in memory, and of
  a structure in registers piece by piece (a Rust `&str` is a pointer and a length in two
  registers). Go's types are in other compilation units than its functions; the compiler
  follows those references.
- Since Milestone 255: strings, vectors and slices by their pointer and length. Rust's `Vec`
  keeps an untyped buffer pointer, so the element type is the `Vec`'s generic parameter `T`.
  Rust's `Option` and `Result` are DWARF variant parts: a discriminant, and per variant either
  its value or none (the variant that a niche keeps, such as `None` in a null pointer).
- The result. Rust returns one scalar in x0 (rax), two scalars in x0 and x1 (rax and rdx),
  another value of up to 8 bytes as the bytes of x0 (rax), and a larger value in memory whose
  address the caller passes in rdi on x86_64. On arm64 Rust 1.90 passes that address in x0, as
  a first argument before the parameters; the compiler takes x8, the C convention's register,
  when a parameter is in x0. Go puts its results' machine words into the integer
  registers in order: R0 to R15 on arm64, and RAX, RBX, RCX, RDI, RSI, R8, R9, R10 and R11 on
  amd64. A Go
  string is two words, a slice three, an `error` two.

A return is matched with its call by the return address (x30 at entry), the page tables
(TTBR0_EL1), and the stack pointer (Rust) or the goroutine (Go: x28). Go moves a goroutine's
stack when it grows, so the stack pointer does not identify a Go call.

The compiler does not read these:

- floating-point values;
- a result that Go returns on the stack (more words than its registers);
- inlined functions (the plan names them as not checked);
- locations that need DWARF's expression stack;
- a value the compiler folded into the code: an argument that is the same at every call
  (`DW_OP_stack_value`), which a program built with its real inputs does not have.

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

A run records its first 100,000 calls. Since Milestone 255 the engine counts the calls after
those, and the records' last line says how many (`{"end":{"recorded":…,"not_recorded":…}}`).
The report then says how many calls were not judged, and so does each finding
(`details.not_recorded`):

```text
hydo: function contracts: 100000 calls checked against 14 clauses, 6 broken
hydo: function contracts: 120104 calls in all; 20104 of them were not judged, because a run records its first 100000 calls only
```

A call whose return the run did not reach has its `ensures` clauses undecided, and the report
says how many such calls there were.

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

### Milestone 255 on the Mac engine

The images are of the fingerprint epoch 3, built with Milestone 255's programs over the 0.3.3
Mac release with the kernel `Image-explore-cg`. Each schedule ran with the plan and without it:

| Image, schedule | Contracts off: fp | Contracts on: fp | Calls, broken |
|---|---|---|---|
| Rust ledger, `disk=none \| run` | `b90439bd647be814` | `b90439bd647be814` | 12, none |
| Rust ledger, `disk=none \| run P=ledger@5+2` | `ccf20770efca87a8` | `ccf20770efca87a8` | 14, `ensures` at 2 calls |
| Go ledger, `disk=none \| run` | `7b4cf7f428b528f4` | `7b4cf7f428b528f4` | 12, none |
| Go ledger, `disk=none \| run P=ledger@5+2` | `4388a6af47647439` | `4388a6af47647439` | 14, `ensures` at 2 calls |
| Rust types, `disk=none \| run` | `34ae597a1864ee59` | `34ae597a1864ee59` (twice) | 120,104; 100,000 judged; 6 clauses broken at 1 call each |
| Go types, `disk=none \| run` | `6836ea1d65bfb460` | `6836ea1d65bfb460` (twice) | 120,078; 100,000 judged; 5 clauses broken at 1 call each |

The types programs (`hydo-types.yaml`, `hydo-types-go.yaml`) call each function once with values
that break a clause, then with values that keep every clause. Each clause that the first round
breaks broke at exactly that call, and every other clause held at all of its calls, so every
read of every type gave the program's values. The two runs of each types image wrote the same
records (`13ffdab5…` and `637df48a…`). Without a plan the Mac release's Redis example, built
with these programs, has its recorded bytes (`706c8b95…`) and gave its recorded fingerprints
`a62eee5332a5b9d2`, `474b2874e7ef0fa2` and `2e5f02d8ff33394f`.

### Milestone 255 on the KVM engine

Neither this Mac nor the bare-metal machine may run KVM. The one-off workflow
`tmp-contracts-kvm` (branch `tmp-contracts-kvm`) runs the same images on GitHub's runners. For
each schedule it compares the run with the plan, the run without it, and the run under main's
hydo, whose KVM engine has no contracts, and it runs the Redis example with this hydo and with
main's. Its results are not in yet.

## Next steps

- hvx: build the side module of [The hvx design](#the-hvx-design).
- KVM: stop in more than three processes of one position-independent program, for example by
  moving the entry breakpoints to the process that runs at each exit.
- Calls past the first 100,000 of a run: write the records as the run goes, so a run judges
  every call.
- More types: floating-point values, Go interfaces' values (not only nil or not), maps, and an
  element chosen by a computed index past the eighth.
- Runtime adapters for Python, Node and the JVM: a stop in the interpreter's call path, and the
  interpreter's own structures as the reads.
- Active property testing: from a snapshot at a function's entry, call the function with
  inputs drawn from its `input` domains, in branches that leave the run as it was. The same
  branches can run a pure function that a clause calls.
