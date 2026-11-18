# Error paths

Status: a first version (Milestone 306) on the Mac engine, opt-in. The KVM engine and hvx do
not watch error paths yet ([Next steps](#next-steps)).

After an exploration that found nothing, a team asks whether the faults reached its error
handling at all. An error path is the code that runs when an operation fails. Examples are the
body of a Go `if err != nil`, a Rust `Err` that a function gives back, and a retry function. hydo
records which error paths of a program ran in each run, and in which
schedule each first ran. On the Blnk example the answer starts like this (schedules cut):

```text
73 of 1326 error paths ran in 12 runs (0 of them without a fault).

Ran only with faults (46): no run without a fault here or in the other exploration reached them.
  queue.go:318 blnk.(*Queue).Enqueue (the body of an `if err != nil`): 3 runs; first in seed-1 "disk=cut ..."
  transaction_execution.go:279 blnk.(*Blnk).validateTxn (the body of an `if err != nil`): 1 run; first in seed-4 "..."

Never reached (1253 in 606 functions), by function:
  metrics.Init (18 places): internal/metrics/metrics.go:94, internal/metrics/metrics.go:102, ...
```

A static rule says that an error path exists. A run shows that it executes under real faults.

## Decisions

1. **A place is the first instruction of an error branch, found without the developer's help.**
   The plan's compiler (`hydo build`, the module `paths` of `hydo-contracts`) reads the program's
   sources and its debug information (DWARF):

   | Kind | Language | Where the place is |
   |---|---|---|
   | `err-check` | Go | The first line of code in the block of an `if` whose condition compares an error with `nil` (`err != nil`, `txErr != nil`). |
   | `err-return` | Rust | A line with `return Err(`, `=> Err(`, or an `Err(...)` that a function gives back as its value. |
   | `err-arm` | Rust | A `match` arm on `Err(..)`, or the body of an `if let Err(..)`. |
   | `try` | Rust | The error branch of a `?` on a `Result`: the code of `<Result as FromResidual>::from_residual` that the compiler copied into the function. |
   | `unwrap` | Rust | A call of `unwrap_failed` or `expect_failed`: the failure of `unwrap()` or `expect()`. |
   | `panic` | Rust | A call of a panic function: `panic!`, `unreachable!`, a failed `assert!`. Bounds checks and overflow checks are left out. |
   | `recovery` | both | The first instruction of a function whose name has `retry`, `recover` or `rollback`. |
   | `declared` | both | The line after `// hydo: error path`. |

   The line table of the debug information gives each line's addresses. A function that the
   compiler copied into its callers has a copy of the line in each. So a place has an address
   per copy, and it ran when any of them ran.
2. **Two comment lines change the list.** `// hydo: error path` adds the next line of code.
   `// hydo: no error path` removes the next line of code, the block that it opens, or the
   function that it declares. The other way was a list in `hydo.yaml`, which would name each
   file and line a second time.
3. **The engine watches each place once per run, with a breakpoint instruction.** Blnk has
   1,326 places at 1,334 addresses, and the Mac engine has 5 breakpoint registers for contracts
   (KVM has 3). The engine writes a `BRK #0x4859` instruction over each place's first
   instruction in guest memory. At the first hit it puts the instruction back at every address of
   the place and lets the guest run it. "Ran at least once" is the question, so one stop per place
   and run is enough, and a place that ran costs nothing afterwards.
4. **The guest sees nothing, so the run keeps its fingerprint.** The breakpoints are in memory
   while the guest runs a phase of the run. The engine puts the guest's instructions back before
   it reads guest memory. It does so when a run call ends, so before every fingerprint, and
   before a snapshot that the host takes at a stop. A breakpoint's stop is a debug exception that
   Hypervisor.framework sends to the engine. The engine counts no exit for it, so the guest's
   clock does not move. A `BRK` with another immediate is the guest's own, and the engine
   delivers it to the guest's kernel as the CPU would have.
5. **The engine finds the program's code by its pages.** The Mac engine's root is in guest
   RAM. So the guest's kernel keeps each page of the program's file in a page of RAM, and every
   process of the program runs from those pages. The plan lists each 4 KiB page of the file that
   holds a place: its first nonzero 8 bytes and the hash of its 4,096 bytes. At each run's start
   the engine reads 8 bytes of every page of guest RAM and hashes the pages whose bytes match. A
   place whose code the engine did not find is listed as not watched, never as not reached.
6. **The host keeps the first hit, and the verdict does not change.** The engine writes one line
   per run into the run's records (`<tag>.contracts.jsonl`): each place that ran, the engine's
   clock then, and the boot. `result.json` gets `paths`, and `exploration.json` gets `paths`,
   each place with the runs that reached it, the runs without a fault among them, and the first
   run, whose schedule replays it. These are results additions.
7. **A baseline without faults is one option.** `hydo explore --no-faults` runs each seed's
   workload, its composition and walk, without a fault. `hydo report --paths A --against B`
   leaves out of the list "Ran only with faults" every place that B reached.

## The report

`hydo run` prints one line, and `hydo report --paths` prints the lists:

```text
hydo: error paths: 27 of 1326 places ran (`hydo report --paths <run dir>` lists them)
```

`hydo report --paths [DIR] [--against DIR]` reads a run or an exploration. It prints the
places that only runs with faults reached, the places that runs without a fault reached too,
and the places never reached, by function. `hydo report --html` adds the section "Which error
paths ran" to a run's page and to an exploration's page.

## The proof on the Blnk example

Blnk is a ledger in Go (`hydo/onboard/examples/blnk`). Its manifest `hydo/hydo-paths.yaml` runs
the example's services, workload and rules with Blnk built from the same commit with its debug
information. The plan has 1,326 places at 1,334 addresses on 210 pages of the program's file:
1,311 bodies of `if err != nil` and 15 recovery functions. 40 more places of the sources have no
code in the program. All runs used the Mac engine and images of the fingerprint epoch 4.

Each schedule ran twice with the plan and twice without it:

| Schedule | Fingerprint, all four runs | Places that ran |
|---|---|---|
| `disk=none \| run` | `a5028e3b9aed3bf3`, OK | 27 |
| `K=redis@21+1 N=worker-*@14-20:delay,100ms` (the regression `commit-lost-with-redis`) | `75eddb844b2c11b9`, VIOLATION | 32 |
| `disk=cut dseed=5 \| run C=10` | `06683fc030390c31`, VIOLATION | 37, 10 of them in the recovery boot |

The two runs with the plan named the same places at the same clock. No place was unwatched: the
engine found all 210 pages in guest memory in every run.

Then two explorations of the same seeds 1 to 12 ran with the plan, one with faults
(`--strategy process --new-faults`) and one with `--no-faults`. The exploration with faults ran
again without the plan, and its 12 first runs gave the same 12 fingerprints.

- Without faults, 27 places ran. They are the lookups that find no row (`GetTransactionByRef`,
  `GetBalanceByIndicator`), a lock that is busy, and the rollbacks of the atomic bulks that the
  workload makes fail on purpose.
- With faults, 73 places ran, the same 27 among them. `hydo report --paths x-faults --against
  x-clean` lists the 46 that only faults reached:
  - The kills of PostgreSQL (seeds 4 and 9) reached 17 places that no other run reached, for
    example `getSourceAndDestination`, `validateTxn`, `CreateBalance` and the inflight lookups.
  - The kills of Redis (seeds 2 and 10) and seed 1's loss of Redis's packets reached the errors of
    the queue: `(*Queue).Enqueue`, four branches of `persistAndEnqueue`, and
    `enqueueTransactions`.
  - The power cuts (seeds 1, 6 and 12) and the kills of Redis reached the recovery of queued
    transactions: `RecoverQueuedTransactions`, `recoverWithThreshold` and
    `processStuckTransaction`.
  - Two recovery functions, `main.recoverPanic` and `api.logrusRecovery`, run at every start of
    Blnk, so a run that restarts Blnk reaches them without an error.
- 1,253 places in 606 functions never ran. Among them are the 18 of `metrics.Init`, which runs once
  at the start, and the API that the workload does not use, such as accounts, API keys and
  balance monitors.

A first run's schedule replays its places. Seed 4's first run reached `validateTxn`'s branch at
the engine's clock 590,557, and `hydo run` of its schedule reached it at 590,557 again, with the
same fingerprint `061c17092b845ffe`.

The cost:

- `hydo build` writes a plan of 570 KB.
- At each run's start the engine searches the 1,536 MiB of guest RAM in 10 to 11 ms (the 16 runs
  of the exploration).
- A run stopped 24 to 54 times, once per place that ran.
- The engine's time of a run was within the spread of the runs:

  | Schedule | With the plan | Without it |
  |---|---|---|
  | `disk=none \| run` | 1.7 s, 1.8 s | 1.7 s, 1.6 s |
  | the power cut | 12.5 s, 10.9 s | 12.0 s, 15.8 s |
  | the exploration with faults, one worker | 122 s | 152 s |

A third run of the first two schedules with the final engine, which also takes the breakpoints
out for a snapshot at a stop, gave the same fingerprints.

## Next steps

- KVM: the same instructions (`INT3`), after the guest's kernel reads a page of the program from
  the disk. The root is on a disk there, so the pages are not in RAM at the run's start. The
  engine would search at each system call, as the contracts' search for a program's header does.
- hvx: the side module of the contracts' hvx design, with `#BP` exits.
- A program that reads its own file (a checksum of `/proc/self/exe`) reads the breakpoints while
  the guest runs. No example does. Such a program needs `// hydo: no error path` on its whole
  sources, or the KVM design of the first item.
- A declaration that a place must run in at least one run of an exploration, as a property that
  fails the exploration.
- Rust's `?` on an `Option`, and `map_err` closures, are not places.
