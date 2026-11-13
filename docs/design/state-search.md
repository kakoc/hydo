# State search: exploring from mid-run snapshots

Status: experimental (Milestone 212). The design was written before the prototype. The
prototype is opt-in (`hydo explore --search state`), and without the option nothing changes. It
was measured on the bug benchmark and on etcd 3.5.5, and it did not help (section
[The answer](#the-answer)).

## The problem

`hydo explore` draws each schedule from its seed and runs it from S1, the snapshot that every
worker takes at the end of the boot. A schedule is the text that names the faults and the timing
of one run, for example `disk=none dseed=1 | run K=kafka@12+3 C=20`. The explorer keeps no state
between runs: run 400 starts from the same S1 as run 1 and knows nothing of runs 1 to 399.

A state search keeps a corpus of mid-run states instead. A state is a snapshot that a run took
at one of its stops. A stop is a point where the engine waits for the host controller, the
program on the host that drives the guest (`docs/design/guest-control.md`). The search keeps a
state when the run had reached something rare by then. It then runs the rest of the run again
from that state, with new faults and new timing for the remaining steps. Such a run is a branch.

## What failed before

hydo tried to guide its exploration six times. Each attempt found nothing that fresh random
schedules did not find, at the same cost or less:

- Milestone 41 kept a schedule when it reached a new control-flow edge (Intel Processor Trace).
  The edges of the order service saturated within a few runs, and most were start-up code. The
  bug needed a timing, and its code was covered in the first run.
- Milestone 43 branched from a snapshot taken at a fixed console line (`KILLED`) and varied only
  the tick jitter after it. On a calibration race the hit rate per try was not higher (1 % from
  the snapshot, 1.7 % over whole runs). Each try was 2.8 times cheaper, so the hits per hour were
  1.7 times higher. On the four-service stack, 488 branch runs with durable settings gave no
  failure.
- Milestone 44 used coverage of the whole system and a bandit over strategies. Coverage grew
  alike with and without guidance, and the guided corpus filled with variants that differed only
  in hit counts.
- Milestone 45 is the closest earlier attempt. It ran a batch of fresh schedules, took the event
  assertion seen in the fewest runs, and branched that schedule at the event's first record with
  6 timing variants. The result was 53 features and 6 failure signatures against 59 and 11 for
  random schedules, in 150 runs each. The reasons were these. The variants changed only the tick
  jitter, so they mostly repeated each other. The budget was counted in runs, and the snapshot
  and restore work made a variant cost about as much as a fresh run (1,552 s against 1,052 s).
- Milestone 139 made a fault react to an event the system writes (`A=`). A fault anchored to an
  event fell in a narrower set of places than a seeded one. An early reaction also pre-empted the
  later fault that made the base schedule fail.
- Milestone 209 kept the schedules whose runs came closest to failing by a number the test
  reported. On the Qdrant test the failure sits next to passing schedules of any length, so the
  number did not predict the verdict (`docs/design/steered-search.md`).

The common lesson of these milestones is that where and in what combination faults fall matters
more than how the next schedule is chosen. A design that only chooses differently repeats them.

## What is different this time

The state search changes three things that the earlier attempts held fixed:

1. **A branch draws new faults for the rest of the run, not only new jitter.** Milestones 43 and
   45 kept the schedule and moved ticks. Here a branch keeps the prefix of the run and draws the
   faults of the remaining steps from a fresh seed. The new faults fall inside the remaining
   steps. So a branch from a state late in the run places its faults late, where a uniform draw
   over all steps rarely places them.
2. **The cost is counted in time, not in runs.** A branch skips the part of the run before its
   state. Milestone 43 measured 2.8 times cheaper tries, and Milestone 45 counted them as whole
   runs. Here both the plain exploration and the state search are charged the seconds that their
   worker spent. That time includes the snapshots and the restores.
3. **The plain exploration stays inside the search.** Half of the schedules stay fresh random
   schedules, in the plain exploration's order. A search whose states are useless then costs at
   most about twice the plain exploration's time to the same failure.

The earlier evidence also limits what to expect. If a failure depends only on a draw from the
start of the run, a state from mid-run cannot help. If the rare state is common in practice, the
corpus fills with ordinary states, as Milestone 44's corpus did. The measurement is set up so that
both cases show.

## The mechanism

### Snapshots during a run

A run of the state search goes as a plain run does. At every stop, the runner first acknowledges
the agent's replies. Then, before the controller decides its next commands, the worker looks at
the console lines that the stop brought (see [Signals](#signals)). When the stop shows a
feature that is rare in the runs so far, the worker sends the engine `SNAP <name>`. The engines
have this command since Milestone 118. It does not change the guest, so a run with snapshots has
the fingerprint of the same run without them. The search checks this property (test 1 below).

A run takes at most 2 snapshots. The engine refuses a snapshot while a typed command is not yet
acknowledged. The worker then tries again at a later stop.

### What the worker keeps

The worker records every stop of a run in memory: the stop's clock value, its console lines, and
every command the runner sent there. For each snapshot it keeps the record up to that stop. The
explorer keeps the rest of a corpus entry:

- the snapshot's name and the run's schedule;
- the stop's number and clock value;
- the step range in progress;
- the features seen up to the stop.

Snapshots live inside one engine process. A branch must therefore run on the worker that holds
its snapshot. The prototype runs the state search on one worker. More seed ranges run as more
processes, each with its own worker.

### Choosing a state

Each corpus entry has an energy. Let c be the number of runs that showed the entry's rarest
feature, and p the number of branches already taken from the entry. The energy is 1 / (c × (p +
1)). The search draws an entry with a probability proportional to its energy. A rare state
is chosen often at first, and a state that was chosen often gives way to the others.

The corpus has a fixed number of entries per engine: 4 on hvx and 32 on KVM and on the Mac. hvx
keeps 8 named snapshots per worker, and each one holds every non-zero page of guest RAM in host
memory. KVM and the Mac keep 64, and each one holds only the pages that differ from S1. When the
corpus is full, a new state replaces the entry with the lowest energy, and its snapshot name is
used again.

### A branch

A branch is one new schedule, B, built from the entry's schedule, A, and from the strategy's
schedule of a fresh seed, F:

- The host part is A's. On KVM and on the Mac, F's timing tokens are added after the branch
  point (see [Timing after the branch point](#timing-after-the-branch-point)).
- A fault of A whose first step is at or before the end of the step range in progress at the
  snapshot is kept. The section [Why a branch starts at a step range](#why-a-branch-starts-at-a-step-range)
  explains this rule.
- A fault of F of a kind that B does not have yet is moved into the remaining steps and added.
  A network window (`N=`) and a one-way window (`O=`) use the same netem queue, and the
  strategies never draw both. So B keeps A's window of the two and drops F's.
- The settings of the whole run, such as the durability settings and the concurrency, are A's.

The search then checks B against the record of A. It runs B's controller over A's recorded stops,
up to the snapshot's stop, without a VM. B is valid when the controller sends exactly the commands
that A's controller sent. An invalid B is counted and dropped, and the search draws another one.

For a valid B the worker sends `RESUME <tag> <snapshot>`. The runner feeds B's controller A's
recorded stops up to the snapshot's stop, with the recorded answers, and sends nothing to the
engine. From that stop on, every command goes to the engine. The guest therefore sees the run of
B from its first instruction: the prefix from the snapshot, the rest live.

The runner sends no resynchronisation at the branch point. After a restore of S1 the runner
types a cancel byte and a `sync` command, and a fresh boot does the same at S1, so that both
runs are equal. A fresh boot of B does nothing extra at the branch point, so the branch does not
either.

### Why a branch starts at a step range

The controller runs the workload in step ranges. A fault before step 12 splits the workload into
the steps 0 to 11 and the steps from 12 on. The workload program starts once per range, with the
range in its environment (`HYDO_FROM`, `HYDO_TO`). A fault at another step gives other ranges, so
other programs start, and the run differs from its first step.

So a branch keeps the range that runs at its snapshot. Let b be the step where that range ends.
B keeps every fault of A that starts at or before b, and F's faults go into the steps after b.
The fault that ends the range at b is one of the kept faults, so B ends its range at b too. On
KVM and on the Mac a timing branch changes the timing at the snapshot itself. A fault branch
from a snapshot inside a range runs that range as A did, and the first difference is at step b.

### Mixing branches and fresh runs

The search runs in rounds of 4 schedules. Once the corpus has an entry, each schedule is a branch
with probability `--state-mix`/1000 (default 500). Otherwise it is the strategy's schedule of the
next seed, exactly as the plain exploration draws it. With `--state-mix 0` the search runs the
plain exploration's schedules in seed order and takes no snapshots. The plain measurements use
that mode, so both columns have the same bookkeeping.

The corpus and the feature counts change only between rounds, and each round's results are read
in tag order. The search is therefore a pure function of its options and of the image.

## Signals

A feature is a string that a stop can show. The prototype reads two kinds from the guest's own
console lines, the lines that the guest's programs write to `/dev/console`:

- An SDK assertion record (`HYDO-ASSERT`) gives `assert:<id>:<ok>`. A `sometimes` event that few
  runs reach is the signal that Milestone 45 used.
- Any other line gives `line:<class>`. The class is the line with every run of digits and every
  hexadecimal word of 6 or more characters replaced by `#`, cut to 96 bytes.

A feature's count is the number of runs, fresh or branched, that showed it. A stop is a candidate
for a snapshot when it shows a feature for the first time in its run and the feature's count is at
most 2. While the corpus is empty, the first stop of each new feature in a run is a candidate.

The guest's console does not carry the workload's own output. The controller sends the workload's
standard output to a file (`workload.log`), so the console shows only what a program writes to
`/dev/console`, such as the SDK's records. A target without SDK records can show no feature at
all. For such a target the search takes its snapshots by clock. With k snapshots per run,
snapshot j is at the first stop past j/(k+1) of the median guest clock of the runs so far. The
search then runs the rest of a run again with new faults and timing, from a state that no
signal chose.

Three other signals were considered and not built:

- Coverage edges from Intel Processor Trace (Milestone 40). They exist on hvx only, the decode
  costs about 4 to 10 s per run, and Milestones 41 and 44 found that they saturate early.
- The state fingerprint `state_fp` (Milestone 199). On KVM it costs about 0.8 s per call, almost
  all of it the hash of the disk image. Two runs rarely have the same fingerprint at the same stop,
  so every state would be new.
- The fault combination reached so far. The controller's notes say which faults acted, but the
  notes are written after the stop. The schedule already says which faults act before the stop.

## Timing after the branch point

On KVM and on the Mac the timing tokens are tick jitter (`tj=`, delays counted in VM exits) and
forced preemption points (`pct=`, `pctn=`, `pctk=`). Before the prototype every engine refused a
mid-run snapshot of a schedule with timing tokens, because the timing state was not in the
snapshot. The prototype changes the KVM and the Mac engines in two places, both inert for every
existing run:

- `SNAP` is accepted with tick jitter and preemption points, and the snapshot keeps both
  structures. `RESUME` restores them. Before the change no run could take such a snapshot, and
  every snapshot taken today keeps the empty structures that `RESUME` finds today.
- A new host command `TIME <tokens>` at a stop of the workload replaces the timing from that stop
  on. The new preemption points are drawn among the next `pctk` exits after the stop.

The schedule says where the timing changes with a new host token, `br=<clock>`. The tokens after
it are the timing from the first stop whose clock value is at least `<clock>`. For example:

```text
disk=none dseed=c7ab56057c8da907 pct=ec699251106e8e0c pctn=40 pctk=3000 br=81234 pct=5be2f1a09c3d7e44 pctn=40 pctk=3000 | run
```

The engine never sees `br=`. The host controller removes it and the tokens after it from the
engine's schedule. Its runner sends `TIME` at that stop. A branch and a run from S1 do this
alike. A branch of a branch has two `br=` tokens. hvx refuses `br=` before any VM starts, with
the reason, as it refuses other host tokens it cannot apply (Milestone 208).

## The replayable schedule

A branch prints one schedule line, as every run does. `hydo run` replays it from a fresh boot:

- A fault branch needs no new token. B is an ordinary schedule. The prefix of its fresh run
  equals A's because its controller sends A's commands up to the snapshot's stop, and the search
  checked exactly that.
- A timing branch carries `br=`. A run from S1 or from a fresh boot reaches the stop of the
  snapshot with A's state, and its runner sends the same `TIME` there.

The explorer already runs every failure again from S1 and requires the same fingerprint. That
check covers branches too. The prototype adds two tests on real engines:

1. A run that takes snapshots has the fingerprint of the same run without them.
2. A branch whose B equals A, and a branch with new faults, each have the fingerprint of `hydo
   run` of the same schedule from a fresh boot.

## The engines

| engine | snapshots | fault branches | timing branches |
|---|---|---|---|
| hvx | in memory, every non-zero page, 8 names per worker | yes, no engine change | no: snapshots of tj/xt/pp/q/cf schedules stay refused |
| KVM | pages that differ from S1, 64 names | yes, no engine change | yes, with the engine change of the preceding section |
| the Mac | pages that differ from S1, 64 names | yes, no engine change | yes, with the same engine change as KVM |

A schedule that the engine cannot snapshot (on hvx, one with `cf=`, `pp=`, `xt=`, `q=` or `tj=`)
still runs, and it takes no snapshot. So on hvx the state search has no states for such runs.
The bug benchmark's defect d11 needs `cf=` (a power cut at the k-th disk flush), so the prototype
cannot branch towards it on hvx.

## Cost

Each job records two numbers:

- the worker's wall time for the job, from the request to the result, including the snapshots
  and the restores;
- the guest clock that the run advanced: the clock value of its last stop minus that of its first
  stop. A recovery boot adds its own difference. A branch starts at the snapshot's clock value.

The comparison uses the wall time. The guest clock shows how much of the saving comes from the
skipped prefix, and it is the same on any host.

## Measurement plan

Each target runs the plain exploration (`--state-mix 0`) and the state search with the same
strategy, options, and seed range, and with `--state-stop`: the search ends after the round of
its first failure. A range without a failure counts as its budget, and the table says how many
did. Every target uses at least three seed ranges. The report gives:

- per range, the worker seconds and the runs to the first failure, for both;
- the median of each;
- at equal worker seconds, the number of distinct failure classes, from runs without
  `--state-stop` on the same ranges.

The targets are the ones where plain search is slow:

| target | engine | what varies | the failure |
|---|---|---|---|
| Qdrant flaky test (`hydo/onboard/examples/qdrant-flaky-10200`), buggy image | KVM | timing only (`--timing --preempt`) | `length: 0` |
| the bug benchmark's slowest defects under `--strategy process --more-faults`: d7 and d10, and d4 and d6 as a control | hvx | faults (without `cf=`, see [The engines](#the-engines)) | the defect's signature, with the control image passing the same schedule |
| etcd issue 14685 (`hydo/onboard/examples/etcd`, 3.5.5) | hvx | faults on three members | the known history violation |

Before the long runs, tests 1 and 2 of [The replayable schedule](#the-replayable-schedule) run on
each engine used.

## Measurements

All measurements ran on hvx on the bare-metal machine. Each exploration had one worker, and two
explorations ran side by side. Each target ran 5 seed ranges: 1001 on, 2001 on, and so on to 5001.
The plain exploration ran with `--search state --state-mix 0`, so both columns have the same
bookkeeping. No exploration used `--state-stop`. Each one ran its whole budget, so the same runs
give the first detection and the failure classes. The budgets were 60 plain runs against 80
state-search runs on the benchmark, and 80 against 100 on etcd. These budgets cost about the
same worker seconds, because a branch is cheaper than a fresh run.

### The bug benchmark

The images are hydo-bench's agent images of each defect. The manifest is the benchmark's,
without the concurrency knob `Q`. The options were `--strategy process --more-faults --cf-range
1-309 --durable-only --no-shrink --state-corpus 3`. A run detects the defect when it fails, its
verdict matches the defect's signature (`hydo/crates/hydo-bench/src/defects.rs`), and its
schedule passes on the defect's control image in a run from a fresh boot. This is hydo-bench's
rule. The control image is `explore-ob-bench-nf` for d4, d6 and d7, and `explore-ob-bench-control`
for d10.

Each cell gives the run number and the worker seconds up to the first detection. "none" gives
the worker seconds of the whole budget. "branch" marks a detection by a branch.

| defect | seeds | plain | state search |
|---|---|---|---|
| d7 | 1001– | none, 489 s | none, 503 s |
| d7 | 2001– | run 2, 42 s | run 2, 28 s |
| d7 | 3001– | run 23, 246 s | run 49, 390 s |
| d7 | 4001– | run 49, 418 s | none, 524 s |
| d7 | 5001– | none, 495 s | none, 519 s |
| d10 | 1001– | none, 514 s | none, 570 s |
| d10 | 2001– | none, 513 s | none, 624 s |
| d10 | 3001– | run 13, 149 s | run 7, 95 s, branch |
| d10 | 4001– | run 4, 28 s | run 4, 30 s |
| d10 | 5001– | run 57, 474 s | none, 573 s |
| d4 | 1001– | run 57, 455 s | none, 496 s |
| d4 | 2001– | run 18, 148 s | run 22, 186 s |
| d4 | 3001– | run 2, 23 s | run 2, 24 s |
| d4 | 4001– | run 11, 98 s | run 15, 115 s |
| d4 | 5001– | run 1, 18 s | run 1, 18 s |
| d6 | 1001– | run 10, 32 s | run 6, 21 s, branch |
| d6 | 2001– | run 2, 27 s | run 2, 28 s |
| d6 | 3001– | run 2, 28 s | run 2, 29 s |
| d6 | 4001– | run 10, 88 s | run 6, 40 s, branch |
| d6 | 5001– | run 1, 19 s | run 1, 19 s |

The medians over the 5 ranges count a range without a detection as its budget:

| defect | ranges with a detection, plain | state search | median worker seconds, plain | state search |
|---|---|---|---|---|
| d7 | 3 of 5 | 2 of 5 | 418 s | 503 s (no detection) |
| d10 | 3 of 5 | 2 of 5 | 474 s | 570 s (no detection) |
| d4 | 5 of 5 | 4 of 5 | 98 s | 115 s |
| d6 | 5 of 5 | 5 of 5 | 28 s | 28 s |

The state search was earlier in 3 of the 20 ranges, each time by a branch. These were d10 from
3001, and d6 from 1001 and from 4001. d6 is one of the two easy control defects. The state search was later,
or found nothing, in 6 ranges. In the other 11 ranges both found the same schedule of the same
seed at the same run number, or neither found the defect. The plain exploration's schedules are
the state search's fresh schedules, and the first rounds come before the corpus has an entry.

At equal worker seconds (the smaller of the two totals of a range), the counts of distinct
failure classes are these. A class is a verdict with every number made `#`. The counts leave out
`ERROR tc setup failed`, a fault of the prototype (below).

| target | plain, per range | state search, per range | plain, sum | state search, sum |
|---|---|---|---|---|
| d7 | 0, 1, 1, 1, 1 | 0, 1, 1, 1, 0 | 4 | 3 |
| d10 | 0, 0, 1, 1, 1 | 0, 0, 1, 1, 0 | 3 | 2 |
| d4 | 1, 1, 2, 2, 2 | 0, 1, 2, 2, 1 | 8 | 6 |
| d6 | 2, 1, 1, 1, 1 | 1, 1, 1, 1, 1 | 6 | 5 |

The state search found no class that the plain exploration missed in the same range.

### The etcd issue 14685

The image is the etcd example's 3.5.5 image (`hydo/onboard/examples/etcd`), with `--strategy
process --durable-only --no-shrink --state-corpus 3`. The target was the second symptom of the
issue, `client.revisions_monotonic`, which the plain exploration finds rarely (open item 8). No
exploration found it: neither the 5 plain explorations (80 runs each, 816 to 879 s) nor the 5
state searches (100 runs each, 810 to 866 s). The first symptom, `etcd.members_agree`, came in
each of the 10 explorations at run 1, 2 or 6, always from a fresh schedule of the same seed in
both columns. The classes at equal worker seconds were 1, 2, 1, 1, 1 for both.

### What the branches did

| target | plain runs, median s, failed | fresh runs of the state search | branches |
|---|---|---|---|
| d7 | 300, 6.1 s, 3.0 % | 206, 6.7 s, 2.4 % | 194, 4.1 s, 7.7 % |
| d10 | 300, 6.1 s, 1.3 % | 206, 6.6 s, 1.5 % | 194, 4.8 s, 0.5 % |
| d4 | 300, 5.5 s, 6.7 % | 206, 6.0 s, 6.8 % | 194, 4.5 s, 2.1 % |
| d6 | 300, 6.1 s, 30.0 % | 206, 6.5 s, 30.6 % | 194, 4.0 s, 33.5 % |
| etcd | 400, 9.7 s, 15.5 % | 270, 9.9 s, 17.0 % | 230, 5.4 s, 4.3 % |

A branch cost about two thirds of a fresh run, and on etcd about half. It failed less often than
a fresh run on d10, d4, and etcd. On d7 branches failed more often, but these failures were
mostly repeats of a parent that fails on the control image too. In the d7 search of seeds
4001–4080, all 14 runs with d7's signature failed on the control image as well, so none was a
detection.

17 branches ended with `ERROR tc setup failed`. Each had both an `N=` window and an `O=` window,
which the strategies never draw together, and the two windows met. The branch rule now drops
the fresh seed's window of the two (section [A branch](#a-branch)). The measurements ran before
this fix. 1,006 branches ran. One more was drawn and dropped as invalid, because its controller
did not send its parent's commands.

## The answer

The state search did not help on the measured targets. On d7 and d10, the two slow defects, it
found the defect in 2 of 5 ranges, against 3 of 5 for the plain exploration. Its median was the
whole budget. On d4 it was later in 3 ranges and equal in 2. On etcd's second symptom neither
found anything. It was earlier in 3 of 20 ranges, two of them on the easy defect d6. The numbers
rest on 5 ranges per target with small budgets, so a small benefit cannot be ruled out. A large
one would have shown.

The branches were cheaper, as the design expected, but they did not fail more often. A branch
keeps its parent's faults up to the snapshot and adds the fresh seed's faults later. On these
targets the failure depends on where and with what the early faults fall, which a branch does
not change. The lesson of the earlier milestones holds here too.

Not measured: the Qdrant flaky test on KVM, the one target where only the timing varies. The
build machine, which runs KVM, was not reachable during the measurement.

## Limits of the prototype

- One worker per search.
- The results formats are unchanged. The search writes its own record, `state-search.jsonl`, in
  the exploration's folder: one line per run with its origin, its parent and stop, its cost, and
  its new features.
- No timing branches on hvx.
- The shrinker does not know `br=`. A shrink candidate of a timing branch loses its branch
  point, so the measurements run with `--no-shrink`.
