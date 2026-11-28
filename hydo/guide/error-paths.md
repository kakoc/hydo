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
- A program that reads its own file while it runs, as a Go program with protobuf does at its
  start, reads the pages that hold the breakpoints. On macOS 26 such a read puts that
  page's instructions back, and the guest reads its own bytes. The places of that page that had
  not run yet are not watched for the rest of the boot.
- Rust's `?` on an `Option`, and the closure of `map_err`, are no places.
- The runs record from the workload's start. The services' start-up before it is not watched.

## 6. Watch every function: the live map

`all_functions: true` in the same `contracts:` entry records every function of the program that
a run reached, besides its error paths. A function is one with code of its own in the debug
information: the program's own, its dependencies' and the language's runtime. The engine puts a
one-shot breakpoint at its first instruction, as it does at an error path, so a run keeps its
fingerprint.

```yaml
contracts:
  - program: /bin/blnk
    sources: ["../work/blnk-paths"]
    error_paths: true
    all_functions: true
```

`hydo build` prints how many functions it watches. A function whose file is in `sources:` is
the project's own:

```text
hydo-build: functions of /bin/blnk: 39038 functions with code of their own (1039 in the sources, 14792 KiB of code), on 3458 pages of the program's file
```

`hydo run` prints one line. `result.json` has the functions under `functions`: each function's
first run, the workload's step range it first ran in, and whether that came before the
schedule's first fault.

```text
hydo: functions: 2359 of 39038 functions ran, 2359 of them before the first fault (`hydo report --live <run dir>` shows them on a map)
```

`hydo explore --live` serves a page on `http://127.0.0.1:8394/` while the exploration runs
(`--port N` names another port). `hydo report --live <dir>` serves it for a finished exploration
or a run directory, until Ctrl-C. The page loads nothing from outside. It shows these parts:

- A map of the program's code. Boxes are modules (a Go package, a Rust module), then files, and
  each tile is a function, sized by its code. A tile stays dark until a run reaches it. Then it
  is green when a run reached it before its first fault, and orange when only runs under faults
  reached it. A red dot marks a function with error-handling places, and a ring marks one whose
  error handling ran.
- The runs in seed order, with their verdicts. Selecting a run shows only what it reached.
- The curve of the functions reached so far: by any run, and before any fault.
- Selecting a function names the first run that reached it, with its schedule, its step range
  and the `hydo run` command that replays it.

A schedule's part before its first fault counts as a run without faults. So the green tiles are
what the workload reaches by itself, and the orange tiles are code that only the faults reached.
The dark tiles are code that no run tested.

The live map's limits are those of section 5, and four more:

- On macOS 27 a program that reads its own file can read the breakpoints. A Go program with
  protobuf does at every start, so a run that starts it again gives another fingerprint with the
  watch than without it. On macOS 26 the engine makes each page with breakpoints execute-only
  and puts its instructions back before such a read. On macOS 27 that is off, because the
  kernel's panic of `HVM_RAM_SHARE` names a change of a page's permissions; `HVM_BRK_PROTECT=1`
  turns it on, at that risk.

- A function that the compiler copied into every caller has no first instruction of its own, and
  it is no tile.
- The first run on each engine reads all of guest RAM to find the program's pages: about half a
  second for 1.5 GiB. Later runs from the same snapshot check the pages at the addresses found.
- The step range comes from the control transcript. A function that first ran in a recovery boot
  or after the workload has no step range.

### A page to show: `--showcase`

`--showcase` serves another page on the same address, for people who have never seen hydo, for
example in a screen recording. It is dark and sized for a screen of 1920 by 1080 or 1200 by 675:

```sh
hydo explore --live --showcase --live-routes ../work/blnk-paths
hydo report --live hydo-out/explore --showcase --routes ../work/blnk-paths
```

The page shows the runs one after another, at most one per 1.1 seconds, so a finished
exploration plays back in its order. It has three views of the project's own code.

- The code map. Each tile lights up with a short glow when a run first reaches its function.
- The call graph. Every function runs from left to right from the entry points, as a flame
  graph on its side, so the functions that no run reached show too. The edges are the direct
  calls in the program's machine code and the first callers that the runs recorded. When a run
  first reaches a function, its path from the entry point flashes like a bolt and stays lit.
  Each entry point shows how many of the functions under it a run reached. A function that no
  run reached and no direct call reaches is in the last group, a small treemap of dark boxes.
- The first calls. The entry points are at the top. Each function hangs under the function of the
  project that called it first. A hover names the other callers that came first in other runs.

`--routes DIR` names an entry point by its route, such as `POST /transactions`, from `hydo rules
scan --kind writes DIR`. For `hydo explore` the option is `--live-routes DIR`. The engine reads
the return address at each function's one-shot stop. So the tree holds only the calls that a run
made: the first caller of each function in each run.

A track of the runs shows each fault as a pulse with its name. When a run breaks a check for
the first time, a banner names the check's promise in its own words, from its `hydo-msg:` line.
The banner also says what hydo did in the run, and gives the `hydo run` command that replays
it. The page's address takes `#pace=SECONDS`, `#hold=SECONDS`, `#start=SECONDS`, `#view=flow`
and `#title=NAME`.
