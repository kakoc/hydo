# Error paths: which error handling the faults reached

Use this page after an exploration that found nothing, to see whether its faults reached the
program's error handling at all. An error path is the code that runs when an operation fails.
hydo records which error paths of a program ran in each run, in which schedule each first ran,
and which ran only in runs with faults. The run is not changed: a run with the record has the
fingerprint of the same run without it.

The Mac engine records them. hvx and KVM do not yet, and a run there says so. The program keeps
its debug information, and its sources sit next to the manifest, as for function contracts
(`hydo guide manifest`, section "Function contracts").

## 1. Ask for them

Add `error_paths: true` to the program's `contracts:` entry in `hydo.yaml`:

```yaml
contracts:
  - program: /bin/blnk                  # a dst of root.files, built with its debug information
    sources: ["../work/blnk-paths"]     # the sources the program was built from
    error_paths: true
```

`hydo build` finds the places and prints how many:

```text
hydo-build: error paths of /bin/blnk: 1326 places at 1334 addresses; 40 more places of the sources have no code in the program
```

A place is the first instruction of one piece of error handling:

- Go: the first line of the block of each `if err != nil` (any error variable compared with
  `nil`).
- Rust: `return Err(...)`, an `Err(...)` given back, a `match` arm on `Err`, the body of `if let
  Err`, the error branch of each `?` on a `Result`, the failure of `unwrap()` and `expect()`, and
  `panic!`, `unreachable!` and a failed `assert!`.
- Both: the first instruction of each function whose name has `retry`, `recover` or `rollback`.

A place of the sources whose line has no code in the program is counted apart. The compiler
removed that code, or the program does not use the function.

## 2. Add and remove places

Two comment lines change the list:

```go
// hydo: error path
fallback()          // this line is a place too

// hydo: no error path
func parseOptional(s string) (int, error) {   // no place in this function
```

`// hydo: no error path` before a line removes that line. Before a line that opens a block, such
as an `if`, it removes the block, and before a function declaration it removes the function.

## 3. See what a run reached

`hydo run` prints one line:

```text
hydo: error paths: 27 of 1326 places ran (`hydo report --paths <run dir>` lists them)
```

`hydo report --paths <run dir>` lists the places that ran and the places never reached, by
function. `result.json` has them under `paths`, by the place's id in the plan
(`<image>.contracts.json`).

## 4. See what an exploration reached

Explore with faults, and explore the same seeds without them. `--no-faults` runs each seed's
workload, its composition and walk, without a fault:

```sh
hydo explore --seeds 1-12 --strategy process --out hydo-out/faults
hydo explore --seeds 1-12 --no-faults --out hydo-out/clean
hydo report --paths hydo-out/faults --against hydo-out/clean
```

The report has three lists:

- Ran only with faults: the places that no run without a fault reached. These are the error
  paths that the faults exercised.
- Ran also without faults: the workload reaches them by itself, for example a lookup that finds
  no row.
- Never reached, by function: the error handling that no run tested. Add a fault that reaches it,
  or a workload step that does.

On the Blnk example 73 places ran in 12 schedules with faults, and 27 in the same 12 without
them. The 46 that only faults reached were the database errors under kills of PostgreSQL, the
queue's errors under kills of Redis, and the recovery of queued transactions after power cuts.

Each place that ran names the first run that reached it and its schedule. `hydo run "<that
schedule>"` replays it, and the place runs at the same point of the run again.
`exploration.json` has the lists under `paths`. `hydo report --html` adds the section "Which
error paths ran" to the page.

## 5. What it does not see

- A line without an instruction of its own. In an optimized build, a Rust `?` whose error needs
  no conversion, and a `return Err` that shares its code with another return, have none. The
  second kind is counted among the places without code.
- A function named for a recovery that runs at every start, such as a handler of panics, runs in
  every run that restarts the program.
- A program that reads its own file while it runs reads the engine's breakpoint instructions.
  Remove such a program's places with `// hydo: no error path`.
- Rust's `?` on an `Option`, and the closure of `map_err`, are no places.
- The runs record from the workload's start. The services' start-up before it is not watched.
