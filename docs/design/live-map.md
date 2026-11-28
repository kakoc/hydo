# The live map of the code an exploration reaches

Status: a first version (Milestone 394) on the Mac engine, opt-in. The KVM engine and hvx do
not watch the functions yet ([Next steps](#next-steps)).

During an exploration a team asks which code of its program the runs reached so far. It also
asks what only the faults reached, and what no run tested at all. Milestone 306 answered that
for the error-handling places (docs/design/error-paths.md). This page extends the same watch to
every function of the program, and shows the answer as a page that updates while the exploration
runs.

## What it shows

`hydo explore --live` serves a page on `http://127.0.0.1:8394/`. `hydo report --live <dir>`
serves the same page for a finished exploration or a run directory. The page is one file with its
style and its script, and it loads nothing from outside. It shows these parts:

- A map of the program's code. Boxes are modules: a Go package, or a Rust path without its last
  part. Inside them are files, and each tile is a function, sized by its code. A tile is dark
  until a run reaches it. It is green when a run reached it before the schedule's first fault,
  and orange when only runs under faults reached it. A red dot marks a function with
  error-handling places, and a ring marks one whose error handling ran.
- The first runs in seed order, with their verdicts. A click on a run shows only what it reached.
- The curve of the functions reached so far, with and without faults.
- A click on a function names the first run that reached it, its step range, and the `hydo run`
  command that replays the run.

The map shows the project's own code by default, which is every function whose file is in the
`sources:` of the `contracts:` entry. A switch shows every function, with the dependencies and
the language's runtime.

## Decisions

1. **A function is a place at its first instruction.** `all_functions: true` in a `contracts:`
   entry (a format-1 addition) makes `hydo build` list every function of the program with code of
   its own. The list comes from the debug information (DWARF): each subprogram with a low and a
   high address. Two names at one address are one place. A function that the compiler copied
   into every caller has no first instruction of its own and is no place. The places go into the
   plan's new list `fn_places`, with the code's size, the module and whether the file is one of the
   sources. Their ids are their own, apart from the error paths' ids.
2. **The engine watches them as it watches error paths.** The Mac engine writes `BRK #0x4859`
   over each function's first instruction in guest RAM. At the first hit it puts the instruction
   back and lets the guest go on. It counts no exit, and it puts the guest's instructions back at
   every stop, so the guest, the snapshots and the fingerprints see the guest's own memory. A
   recovery function's error path and the function's own place share one address. The engine
   keeps one breakpoint there, and a hit records both places.
3. **A page that holds breakpoints is execute-only while the guest runs.** A program can read
   its own file, and the guest's kernel serves the read from the same pages of RAM that the
   program runs from. Blnk does at every start: the protobuf package hashes eight samples of 64
   bytes of the binary (`detrand.binaryHash`). The first proof found it. With every function
   watched, 15 of 30 schedules gave another fingerprint than without the watch, the same
   verdicts and the same control transcripts. In each of them a Blnk process started during the
   run, and 8 bytes of its memory differed. So the engine maps each 16 KiB page that holds a
   breakpoint without read and write permission. A read or a write of it is a data abort that
   comes to the engine. The engine puts back every instruction of the page for the rest of the
   boot, maps the page as before, and lets the guest do the access again. It counts no exit. A
   place that had not run yet is listed as dropped (`dropped` in the records line). The rewind
   mode tracks writes with the same permissions, so it keeps the old behaviour. On macOS 27 the
   kernel panicked in the Mac engine with a violation of a page's permissions (Milestone 395).
   Whether `hv_vm_protect` takes part in it is not known, so `HVM_BRK_PROTECT` turns the
   execute-only pages on, and they are off there by default.
4. **The functions have a records line of their own.** The engine writes `{"functions":...}`
   beside `{"paths":...}` in `<tag>.contracts.jsonl`, in the same shape. So the reports of error
   paths do not change, and a plan without `all_functions` writes what it wrote before.
5. **The host places each first run at a workload step range.** The control transcript has a
   stop for each clock value and a note for each step range (`workload steps 0..13`). The first
   stop at or after a function's clock gives its range. A function ran before the faults when its
   range ends at or before the schedule's first fault, in the first boot. `result.json` and each
   run record of `events.jsonl` get `functions`, a results addition: the functions' count, each
   first run with its clock, boot and range, the ranges, and the first fault's step.
6. **The page reads only what the exploration writes.** At its start the explorer writes
   `function-map.json` from the plan, beside `events.jsonl`. The page's server reads the stream
   from where it stopped the time before. It gives the page each first run in a short form: the
   seed, the schedule, the verdict, and the functions with their range and their place before
   the faults. The page asks for the runs after the ones it has every 1.5 seconds.
7. **A baseline is one option.** `hydo report --live A --against B` and `hydo explore --live
   --live-against B` count what B's runs reached as reached without a fault. B is an exploration
   made with `hydo explore --no-faults`.

## The proof on the Blnk example

Blnk is a ledger in Go (`hydo/onboard/examples/blnk`). Its manifest `hydo/hydo-map.yaml` is
`hydo-paths.yaml` of Milestone 306 with `all_functions: true`. `hydo build` printed:

```text
hydo-build: error paths of /bin/blnk: 1326 places at 1334 addresses; 40 more places of the sources have no code in the program
hydo-build: functions of /bin/blnk: 39038 functions with code of their own (1039 in the sources, 14792 KiB of code), on 3458 pages of the program's file
```

The plan is 19.6 MB, against 570 KB with the error paths alone. The image was built at the
fingerprint epoch 7 with `guest.proc_io: false`, and every run used the Mac engine on macOS 27.2.1
with `HVM_RAM_SHARE=0`, so with the execute-only pages off. Four explorations of the seeds 1-30
with the process strategy ran: one without faults and one with faults, each with the watch and
without it.

- The 30 runs without a fault gave the same 30 fingerprints with the watch and without it.
- Of the 30 runs with faults, 15 gave the same fingerprint. Each of the other 15 starts a Blnk
  process during the run, which reads its own file (decision 3). Their verdicts and control
  transcripts were the same. On macOS 26, with the execute-only pages, the one such schedule
  that ran gave the fingerprint of the run without the watch.
- Of the 1,039 functions of Blnk's sources, 236 ran without a fault, 103 only under faults, and
  700 never. Of all 39,038 functions, 2,447 ran without a fault, 2,301 only under faults, and
  34,290 never. The runs without faults are the baseline (`--live-against`).
- The account server in Rust of `contracts-ledger` (`hydo-map.yaml`, 806 functions) gave one
  fingerprint with the watch and without it for each of three schedules, a kill among them.

## The cost

- A run of Blnk reached 2,148 to 4,641 functions, and the engine stopped once at each.
- The search of guest RAM took 18 to 27 ms for 1.5 GiB. With `HVM_RAM_SHARE` on it took 0.4 to
  0.5 s on a first run, because every page of guest RAM was then read from its RAM base file for
  the first time; later runs from the same snapshot check the pages at the addresses found.
- The engine's seconds of the 30 first runs with faults were 174.6 with the watch and 169.3
  without it, 3 % more; without faults 49.2 and 49.0.
- The stream `events.jsonl` of the exploration with faults was 8.3 MB. Only the first runs carry
  their functions; when the shrink runs carried them too, it was 44 MB.

## The showcase page and the flow of calls

Status: Milestone 397 (a tentative number), opt-in, on the Mac engine.

`--showcase`, a flag of `hydo explore --live` and of `hydo report --live`, serves another page
on the same address. It is for people who have never seen hydo: a dark page sized for a screen
of 1920 by 1080 or for 1200 by 675, with large type and no internal terms. Without the flag the
server serves the page of Milestone 394, so nothing changes for it. The page reads the same
three answers of the server, and every number on it comes from the runs:

- The headline names the program, and four counters give the runs shown, the faults that the
  runs injected, the share of the project's own functions reached, and the bugs found. A bug is
  a check that broke; the counter counts each check once.
- The hero is one of two views of the project's own code. The code map is Milestone 394's map,
  where a tile lights up with a short glow when a run first reaches its function. The call flow
  is described below.
- A track of the runs shows each run's faults as small dots at their workload steps. While a
  run plays, a line moves along it, and each fault pulses with its name in words: crash, power
  cut, clock jump, network cut, slow network, pause, or disk errors.
- When a run breaks a promise for the first time, a banner names the promise in the check's own
  words (its `hydo-msg:` line), says what hydo did in the run with the schedule's sentences
  (`hydo_schedule::describe`), and shows the `hydo run` command that replays it.

The banner says "<program> broke a promise" only for a bug (Milestone 397's third follow-up). A
failure that the explorer's advice expects, from a setting that trades safety for speed or a
fault that no system survives, reads "A documented trade-off, shown". The notes of a recording
(below) can name a failure as a documented limit or a known bug, with its issue.

The page shows the runs one after another, at most one per 1.1 seconds, and pauses 3.6 seconds
after a bug. So a finished exploration plays back in its order with `hydo report --live
--showcase`, and a live one shows each run as it ends. The page's address takes `#pace=`,
`#hold=`, `#view=flow` and `#title=`. The chip under the name says `Live` or `Recorded
exploration, played back`. The command on the page names the image by its file name only, so
a screen recording shows no folder of the machine.

### The notes of a recording, and several explorations

Status: Milestone 397's third follow-up, from the NSQ recording.

The runs do not say whether a failure is a new bug, a bug that someone reported before, or a
behaviour that the project documents. `--notes FILE` of `hydo report --live --showcase` names
it. The file is a list `notes:`. A note names a check of the verdict (`check:`), a text of the
schedule (`schedule:`), a seed (`seed:`), or several of them, and the first note that matches a
run applies:

- `kind: new | reported | known | documented`, `issue: owner/repo#N` and optional `words:`.
  The banner of a new or reported bug says "<program> broke a promise" and names the issue
  ("Reported upstream by hydo"). A known bug reads "A known bug of <program>, found again", and
  a documented limit "A documented limit, shown", each with its issue. The run track labels
  each failed run `BUG`, `KNOWN BUG` or `DOCUMENTED`, and the header counts no documented limit
  or trade-off as a bug.
- `leave_out: true` with `why:` leaves a run out of the playback, for example a failure whose
  cause is not traced. The server counts the runs left out (`left_out`).

`hydo report --live A B ...` plays the runs of several explorations or run directories of one
image one after another, as one recording. The map is the first folder's, each run's command
names its own image, and the chip says "Recorded explorations, played back". A run's number on
the page is then its place in the playback, not its seed, and so it is when a note leaves runs
out. The example is the NSQ recording (`hydo/onboard/examples/nsq/RESULTS.md`, "The recording
of the live map").

### The flow of calls

The call flow draws the project's own code as a tree that grows from the top. It is the
first-caller tree of the program's call graph:

- At each function's one-shot breakpoint stop the Mac engine reads the guest's link register
  (x30 on arm64), the return address into the function that called it. The engine writes the
  addresses on the functions' records line (`lr`, parallel to `hits`), and the host names the
  caller as the function of the plan whose code holds the call (`caller`, a results addition of
  each first run). Each function stops once per run, so a run names one caller per function:
  the first.
- A function hangs under its nearest caller of the project's own code. The page walks the first
  callers up and skips a library's code in between, such as a web framework's middleware chain
  or a wrapper that the compiler made.
- The roots are the entry points. `--routes DIR` (`hydo explore --live-routes DIR`) reads the
  sources in DIR with `hydo rules scan --kind writes`, and a function whose definition a route's
  handler names is a root labeled with the route, such as `POST /transactions`. `main.main` is
  the root `main`. A function that no function of the project called first is a root under the
  library that called it (`called by <library>`), or under `background work` when the
  language's runtime started it (a goroutine). Roots of less than 3 % of the tree share one box,
  `other entry points`. On Blnk the 339 functions of its own code that the 30 runs reached hung
  under six routes, `main` (59), `background work` (166, in 21 goroutines' functions), and
  three other entry points.
- A cell is green when a run reached the function before its first fault, and orange when only
  runs under faults did, as on the map. A red dot marks error-handling code, a ring the code
  that ran. A cell's width is the number of functions under it, and a new branch grows down from
  its parent when a run first takes that call, so a fault shows as orange branches under an
  entry point.
- A hover names the first caller and the other callers that came first in other runs.

What the tree does not show:

- Calls that no run made. Without a static call graph of the program the tree knows only the
  calls that ran, so the branches that were never reached are not drawn.
- A second caller in the same run: a one-shot breakpoint stops once.
- Inlined functions. A function that the compiler copied into its callers has no first
  instruction of its own, so it has no node, and its callees hang under the function it was
  copied into.
- A call through a tail call: the link register then names the caller's caller.

### The call graph, left to right

The third view of the showcase page draws every function of the project's own code, also the
ones that no run reached, as a partition from left to right, as a flame graph on its side:

- `hydo build` reads the static call graph of the project's own code from the program's machine
  code. For each function of `fn_places` whose file is one of the sources, it reads the
  function's bytes and takes each arm64 `bl` and `b` with a 26-bit immediate whose target is the
  first instruction of another such function. The plan keeps the targets in the function's
  `calls` (ids), and the map gives them to the page. On Blnk 598 of its 1,039 functions call
  another one of them directly, with 1,347 edges.
- The roots are on the left: the routes that `--routes DIR` names, `main.main`, and the
  background work. A function of background work calls no function of the project's own code
  before it, and the runs' first callers show that the language's runtime started it (a
  goroutine). Small routes share one box.
- A breadth-first search over the direct calls from every root at once places each function
  once, at its shortest path; a tie goes to the root above. Then a function that a run reached,
  and that no direct call from a root reaches, goes under the nearest function of the project
  that called it first in a run (the return address of Milestone 397), and the direct calls go
  on from it. A reached function with no function of the project above it is an entry point of
  its own, under `background work` or `called by libraries`. On Blnk the direct calls alone
  left 609 functions unplaced; the runs' first callers placed 194 of them, and 415 stay in the
  last group.
- The last group, `not reached from an entry point by a direct call`, holds the functions that
  no run reached and no direct call reaches. They have no path to show, so the group is a
  treemap of dark boxes in an eighth of the height.
- A box's height is its subtree's share of its parent's box, so each column fills its parent's
  height. A subtree weighs the square root of each function's code, and a function that no run
  of the exploration reached weighs a quarter of that, so the code that the runs reach has the
  room. A box is at least 3 pixels high where that fits. A function that calls none of the
  placed ones reaches to the right edge, so its name fits. A box at least 11 pixels high shows
  the function's name without its package and receiver, and a hover shows the full name.
- Each root shows how many of the functions placed under it a run reached (`POST /transactions
  108 of 154`). The header keeps the share of the whole project.
- When a run first reaches a function, a bolt runs along the boxes of its path from the root,
  the boxes on the way light up, and the function stays lit, green or orange as on the map,
  with a red mark for error-handling code. A run that reaches a function again flashes a
  fainter bolt.

What the call graph does not show:

- A call through an interface, a closure or a function value has no direct edge. Such a function
  is placed by its first caller in the runs when a run reached it, else it is in the last group.
- A function that the compiler copied into its callers, as in the first-caller tree.
- The machine code of x86_64: the plan reads arm64 only, so an x86_64 program has no edges.
- The layout uses the first callers of every run that the page has, so a live exploration's
  layout can change when a run reaches a function for the first time. A played-back exploration
  has all its runs from the start.
- A partition gives a column at most its height. On a screen of 1080 lines, 28 boxes of Blnk's
  call graph carry a name, against the 8 module names of the code map, and 542 boxes are at
  least 3 pixels in both directions, against 1,029 tiles of the code map. The deep columns of a
  large subtree are lines thinner than 3 pixels.

The read of a register at a stop that already happens changes nothing that the guest can see.
The proof is in `docs/HISTORY.md`, Milestone 397: the same 30 schedules with faults gave the
same fingerprints with the engine of Milestone 394 and with the engine that reads the register.

## Next steps

- KVM and hvx: the same breakpoints, as docs/design/error-paths.md says for error paths.
- `stats.json` has no measure of the functions reached, and `exploration.json` has no list of
  them. The page draws its own curve from the run records.
- A function that ran only during the services' start, before the snapshot that every run starts
  from, is never reached in a run. A watch of the boot to S1 would show it.
- A Go function inlined into every caller is no tile. Its inlined copies in the line table could
  count as the function.
