# Explaining a failure: where two runs first differ (M165, design)

Status: design, with its first piece built (`hydo diff`, `hydo explain`, M165). The authority on
the output is the Rust type `hydo_results::Diff` (`diff.json`, results format version 1, an
addition) and the example under `hydo/schema/examples/results/v1/metal-stack-diff/`; this note
explains the decisions and what deeper levels would need.

## The problem

hydo finds a failing schedule and shrinks it to a minimal one, and every run reproduces bit for
bit. What it does not do yet is say *why* the schedule fails. A person (or an AI tool) reads the
consoles of the failing run and guesses. But hydo can do better than a guess: because runs are
deterministic, a failing run and a nearly identical passing run are the same run up to one exact
point, and everything before that point is shared. The first place where they part is the best
first hint of the cause, and the records around it are the context a person needs.

## What a divergence is, level by level

A run is recorded at several levels. Each level is deterministic, so two runs of one image and
two schedules that agree up to some point give equal records up to that point.

| Level | Where it is recorded | Position of a record | Available without engine changes |
|---|---|---|---|
| Commands the host typed, engine actions, the end of a conversation | the control transcripts `control.boot.jsonl`, `control.jsonl`, `control.recovery.jsonl` (`send`, `engine`, `snap`, `end`) | stop number `n` and guest time `at` (hvx: the instruction clock; hvm: the exit count) | yes |
| The host controller's notes and reports (`hydo-ob: KILLED redis before step 11`, the acknowledged and failed lists, `HYDO-ORACLE`) | the transcripts (`note`, `report`) and the merged console `<tag>.log` | the stop that produced them | yes |
| The guest's own console lines (services' prints, SDK assertion records `HYDO-ASSERT`, history records `HYDO-HIST`, `hydo-world:` summaries) | the merged console `<tag>.log` (each guest line is put before the reports of the stop that saw it), raw in `<tag>.agent.log` | the stop that saw the line (an upper bound on when it was printed) | yes |
| The agent's replies and events (exit statuses, file contents read back) | not recorded: a transcript keeps only `seen`, the number of agent lines read by each stop | the stop | no: needs the transcript to keep the replies (a host-side addition that does not change fingerprints, but changes the transcript files) |
| Service log files inside the guest | read on the host only with `errors:` in the manifest (`HYDO-ERRLOG` records at the end of each boot) | the end of a boot | only with `errors:` |
| Fake-service records (`/tmp/hydo/world.log` in the guest) | the console's `hydo-world:` summary before the verdict | the stop | the summary only |
| Findings, verdicts, history checks | `result.json` | none (the end of the run) | yes |
| Network packets | not recorded (virtio-net has a capture for scripted devices; the freeze sweep counts packets of one stream) | a guest time per frame | no: an engine change (a frame log with the guest time of each frame) |
| Disk requests | not recorded (the engine sees each virtio-blk request: its index, sectors, length, flush) | a guest time per request | no: an engine change (a request log) |
| Guest memory | the snapshot fingerprint of the whole run (`fp=`) only | the end of the run | no: an engine request that hashes RAM (and disk) at a given stop |
| Instructions | Intel PT coverage exists (M40), not per run | the instruction | no |

A *divergence* at a level is the first record one run has and the other does not, after
aligning the two sequences of records. Separately, a *guest-time divergence* is the first record
both runs have that happens at another stop or guest time: two runs can do the same things at
different times (a different image boots at another speed), and one run can do something
different at the same time.

## How two runs are aligned

Per conversation (boot, run, recovery, then the result document), the records of each run are
put in their recorded order and compared by a key that leaves out what only moves with time: a
command's bytes without its `id` and `nonce` (the nonce of a `sync` is the stop's time), a line's
text, an `end` record's content. Stop numbers and guest times are kept with every record but are
not part of the key. The two key sequences are aligned with Myers' O(ND) diff (common prefix and
suffix first, then the shortest edit script; beyond 2,000 edits the middle counts as replaced).

- **One image, two schedules.** Both runs restore the same S1 snapshot and are deterministic, so
  they share every record, at the same stop and guest time, up to the point where the schedule's
  difference first acts. The alignment and the guest-time divergence both find that point
  exactly.
- **Two images, one schedule.** The guest times differ from the first boot step on (other
  binaries run at other speeds), so stop numbers and times cannot align the runs; the keys can,
  because both runs receive the same commands from the same controller until the systems behave
  differently. The guest-time divergence is then reported once and is expected.

A diff then reports:

1. **The first difference:** the first place where one run has records the other does not, with
   the records both share right before it and what differs inside records of the same kind (the
   tokens or the JSON fields: `env: only in A: HYDO_TO=11; only in B: HYDO_TO=24`,
   `hydo-explore-acked: only in B: 11`). Records of a kind only one run ever writes are left out
   here and listed on their own (below).
2. **Guest time:** up to which shared record the stops and times are equal, and where the next
   shared record is in each run.
3. **The first difference in what the checks judge:** the first place with a judged value that
   differs: the acknowledged and failed lists, a verdict, an `end` record, a history,
   fake-world or error-log record, a failing assertion, or an assertion both runs write there
   with other content. An extra *passing* evaluation of a check that one run happened to make at
   another time does not count (it is timing, and it was the first "outcome" difference before
   this rule, see M165's first report).
4. **Records of a kind only one run writes** (an assertion id the other run never reports), each
   at its first occurrence and under the controller note it came after. With two images this is
   often code only one image has; with two schedules, a fault only one schedule has.
5. **All differences**, counted by conversation and level, and the verdicts.

## Which pairs are useful

- **The minimal failing schedule against its neighbours.** The shrinker's minimal schedule is
  minimal because every simpler variant passes; those variants are the natural partners: the
  schedule with one guest token removed, with one fault moved one step earlier or later, without
  its disk faults. The neighbour that stays the same for longest (its first difference comes
  latest) and passes is the most informative. `hydo explain <image> "<schedule>"` runs them and
  picks it.
- **The failing image against the fixed image on the same schedule** (`--with-image`, or two
  `hydo run`s and `hydo diff`): the first difference in what the checks judge points at the
  behaviour the fix changed, and the records only the fixed code writes say what it did there.
- Later: the explorer could keep its last round's passing shrink candidates, so that a minimal
  schedule comes with its passing neighbours already run.

## What a person sees

`hydo diff <run dir A> <run dir B>` prints the report and writes `diff.json` (the results format)
and `diff.txt` (the same text, a view like `report.md`). `hydo explain` prints a table of the
neighbours (verdict, how each was made, where it first differs) and the report of the closest
passing one, writes each neighbour's run directory with its `diff.json` and `diff.txt`, and
`explain.txt` (the view). Real reports are in docs/HISTORY.md, Milestone 165.

## What deeper levels need later

- **Agent replies in the transcript.** The exit status of every command and every file read
  back (the outcomes file, the acknowledged ids) would let the diff show what the workload's
  client saw at each step, not only the controller's summary. A host-side addition: the
  transcript gains a record type (the transcript hash `ctl=` would then have to leave it out to
  keep recorded `ctl=` values).
- **Service logs at every stop.** Today they are read at the end of a boot and only with
  `errors:`; a controller option to tail them (the agent's `tail`, mentioned for reactions in
  open item 30 h) would put each service's lines on the timeline.
- **Packets and disk requests.** An engine log of virtio-net frames and virtio-blk requests, each
  with its guest time, written beside the run and outside the fingerprint. Disk requests matter
  most for durability bugs: the Redis report of M165 shows that without them the diff can only
  say "the knob was sent, and the recovery lost a key", not which writes were not flushed.
- **Guest memory at a stop.** An engine request that replays a run to stop N (the transcript
  already allows an exact replay, `hydo repro --transcript`) and returns a hash of RAM and disk
  there. For two runs of one image a binary search over stops then finds the first stop at which
  the guest's state differs, which can come before any visible record differs; page hashes would
  then name the memory that differs (a process's heap, the page cache of a file).
- **Instructions.** Between two stops with equal state and the next with different state, the
  PMU branch clock and Intel PT could narrow the divergence to the instruction, as a record and
  replay debugger would; this needs the single-step and tracing paths of the module per run.
