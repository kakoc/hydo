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

## Next steps

- KVM and hvx: the same breakpoints, as docs/design/error-paths.md says for error paths.
- `stats.json` has no measure of the functions reached, and `exploration.json` has no list of
  them. The page draws its own curve from the run records.
- A function that ran only during the services' start, before the snapshot that every run starts
  from, is never reached in a run. A watch of the boot to S1 would show it.
- A Go function inlined into every caller is no tile. Its inlined copies in the line table could
  count as the function.
