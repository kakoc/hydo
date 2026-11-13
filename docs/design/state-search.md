# State search: exploring from mid-run snapshots

Status: design, written before the prototype. The prototype is opt-in
(`hydo explore --search state`). Without the option nothing changes. The section
[Measurement plan](#measurement-plan) says how the benefit is measured. The results go into
`docs/HISTORY.md`.

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

- The host part is A's. On KVM, F's timing tokens are added after the branch point (see
  [Timing after the branch point](#timing-after-the-branch-point)).
- A fault of A whose first step is at or before the end of the step range in progress at the
  snapshot is kept. The section [Why a branch starts at a step range](#why-a-branch-starts-at-a-step-range)
  explains this rule.
- A fault of F of a kind that B does not have yet is moved into the remaining steps and added.
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
KVM the timing changes at the snapshot itself. On hvx and on the Mac a fault branch from a
snapshot inside a range runs that range as A did, and the first difference is at step b.

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
forced preemption points (`pct=`, `pctn=`, `pctk=`). Today every engine refuses a mid-run snapshot
of a schedule with timing tokens, because the timing state is not in the snapshot. The prototype
changes the KVM engine in two places, both inert for every existing run:

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
alike. A branch of a branch has two `br=` tokens. hvx and the Mac refuse `br=` before any VM
starts, with the reason, as they refuse other host tokens they cannot apply (Milestone 208).

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
| the Mac | pages that differ from S1, 64 names | yes, no engine change | no: `TIME` is not built |

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

## Limits of the prototype

- One worker per search.
- The results formats are unchanged. The search writes its own record, `state-search.jsonl`, in
  the exploration's folder: one line per run with its origin, its parent and stop, its cost, and
  its new features.
- No timing branches on hvx and on the Mac.
- The shrinker does not know `br=`. A shrink candidate of a timing branch loses its branch
  point, so the measurements run with `--no-shrink`.
