# Shared beginnings of an exploration's runs

Status: research (Milestone 337, tentative number). Both recommendations are built since: the
answers to repeated schedules (Milestone 339) and the shrinks from snapshots (Milestone 340,
`docs/design/shrink-snapshots.md`), the second opt-in. The note measures how much
of an exploration's runs repeats the same beginning. It says what keeps runs apart, and it
recommends what to build.

## The question

`hydo explore` draws each schedule from its seed and runs it from S1, the snapshot that every
worker takes at the end of the boot. A schedule is the text that names the faults of one run,
for example `disk=none dseed=4d5cb825dfaab05b | run K=redis@11+3 C=20`. Many schedules could be
the same up to their first fault. The explorer could then run that beginning once, take a
snapshot where the schedules part, and continue each schedule from the snapshot. A continued run
must give the same fingerprint, control transcript, and verdict as the same schedule run from S1.

This note calls the part of a run that another run already ran its shared beginning. It answers
five questions:

1. How much of each run is a shared beginning, with the explorer's draws of today and with other
   draws?
2. Which inputs reach the guest before the first fault and keep runs apart?
3. How do the engines take and continue a snapshot in the middle of a run today? What does it
   cost, and is a continued run exact?
4. How does a tree of snapshots fit the copy-on-write workers of the Mac engine?
5. What do published systems do for the same problem?

## How the measurement works

A run's inputs to the guest are of two kinds. The engine arms the host part of the schedule (the
tokens before `|`) when the run starts from S1. The host controller then sends commands to the
guest agent, each at a value of the guest clock. The run's control transcript is
`consoles/<tag>.control.jsonl` in the exploration's folder. It records every command as a `send`
line with its clock value (`at`) and its bytes.

The engine is deterministic. So two runs with the same host part and the same sends at the same
clock values are the same run up to their first send that differs. The guest ran identically
until that send's clock value.

The scripts in [`shared-prefix/`](shared-prefix/) read the transcripts:

- `prefix.py` builds a tree of all runs in the order of `events.jsonl`. For each run it finds the
  longest beginning that it shares with an earlier run. The sum over all runs is the guest clock
  that a tree of snapshots at every point where runs part would save.
- `models.py` computes the same for explorer draws that do not exist yet (section
  [If the explorer drew differently](#if-the-explorer-drew-differently)).
- `extra.py`, `roles.py`, `shrinks.py`, `dups.py`, `same.py`, and `wallclock.py` give the other
  tables of this note. `branches.py` lists the branches of a state search for a replay.

The unit is the engine's guest clock: on hvx the clock of its lock-step stops, on the Mac engine
the exit count. A run's clock is its run conversation from S1 to its end, plus its recovery boot
after a power cut. The wall seconds of a saving are each run's seconds times its saved share of
the clock.

That estimate holds where the clock predicts a run's seconds. `wallclock.py` gives the
correlation of the two over the first runs of each exploration. It is 0.93 to 1.00 for TiKV,
Redpanda, VictoriaMetrics `e2`, and Iggy's `e1` and `e4`. It is -0.03 for ClickHouse `e2` and
0.11 for Dragonfly. So the seconds in this note are estimates, and the shares of the clock are
exact.

The measurements use 15 explorations of 9 examples. They ran on hvx on the bare-metal machine
in the four days to 2026-11-19, and this note read their folders there without starting a VM.
The section [The Mac engine](#the-mac-engine) says why no new Mac exploration ran. Each script
runs as `python3 -I
hydo/explore/shared-prefix/<script> <exploration folder>...`.

| exploration | example | strategy and options | first runs | their seconds |
|---|---|---|---|---|
| `ch-e2` | ClickHouse (M311) | process, `--new-faults --cluster-faults` | 20 | 1,550 |
| `ch-e4` | ClickHouse | swarm, `--new-faults` | 12 | 609 |
| `df-ex2` | Dragonfly (M278) | process, `--new-faults --cluster-faults` | 60 | 386 |
| `iggy-e1` to `e4` | Iggy (M314) | process or swarm; storage faults or reactions | 20, 40, 40, 24 | 384, 1,210, 732, 438 |
| `rp-b3`, `rp-b6` | Redpanda (M279) | process, every fault family | 14, 13 | 1,821, 1,948 |
| `surreal-e1` | SurrealDB (M277) | process, `--more-faults --storage-faults` | 40 | 569 |
| `tikv-ex4` | TiKV (M287) | process, every fault family | 44 | 2,516 |
| `vm-e2`, `vm-e3` | VictoriaMetrics (M312) | process or swarm; storage faults and cluster faults | 40, 40 | 279, 208 |
| `pg7-e3` | seven PostgreSQL image services (M317's race) | random, the manifest's power cuts only | 300 | 848 |
| `webshop-wc` | web-shop's walk (M267) | random, `--protocol-faults` | 20 | 402 |

## What is shared today

With the explorer's draws of today, no two first runs of an exploration share a beginning that
counts. In every exploration but one, an earlier run already ran 0.0 % to 0.6 % of the clock.
The largest share of one run with its nearest run is 4.4 % (`vm-e3`).

These numbers compare the host part without `dseed=`, and they count `disk=cut` as `disk=none`.
No engine allows that today (section [Why so little](#why-so-little)). By the whole host part,
every run of every exploration shares nothing, because each seed draws its own `dseed=`.

| exploration | clock that an earlier run already ran | median share with the nearest run | largest |
|---|---|---|---|
| `ch-e2` | 0.0 % | 0.0 % | 0.2 % |
| `df-ex2` | 0.6 % | 0.7 % | 1.4 % |
| `iggy-e1` | 0.0 % | 0.1 % | 0.1 % |
| `rp-b6` | 0.0 % | 0.0 % | 0.3 % |
| `tikv-ex4` | 0.0 % | 0.0 % | 0.1 % |
| `vm-e3` | 0.4 % | 0.5 % | 4.4 % |
| `webshop-wc` | 0.0 % | 0.0 % | 0.0 % |
| `pg7-e3` | 57.7 % | 100 % | 100 % |

The other explorations give 0.0 % to 0.4 %. The 13 explorations of faults on databases and
queues have 12,590 s of first runs, and 0.07 % of it is shared.

`pg7-e3` is the exception, and a snapshot is not what it needs. Its manifest allows power cuts
only, over 12 steps. So the random strategy drew 13 distinct schedules for 300 seeds: 184 times
`disk=none | run` and 116 times `disk=cut | run C=<k>`. `same.py` shows that the 184 runs of
`disk=none | run` have the same transcript, line for line. Their fingerprints are 184 different
values, because the fingerprint covers the disk's fault seed.

## Why so little

Each input that reaches the guest before the first fault makes two runs differ from the start.
`diffpair.py` shows where two runs with the same first fault step part:

- **The host part.** Each seed draws its own `dseed=`. The engines keep the disk's fault seed,
  classes, and probability in the device's state, and the fingerprint covers that state. On hvx
  `fnv_snapshot` hashes `fault_seed`, `fault_classes`, `fault_probability`, `fault_req_seq`, and
  `fault_count` (`hydo-hvx/src/fnv.rs`). The Mac engine hashes the device's struct with its seed
  (`hydo-hvm/src/virtio.rs`, `hash`). So a run of `disk=none` with another `dseed=` ends with
  another fingerprint, although the guest saw nothing of the seed.
- **The disk's policy.** With `disk=cut`, every write waits in the engine's volatile cache until
  a flush, and the cache is in the fingerprint. `ioerr:P` and `lying:P` decide every disk request
  from the seed, from the workload's first request on.
- **The timing tokens.** `tj=`, `pp=`, `q=`, `xt=`, and `pct=` act from the workload's first
  instruction, and hvx's `cf=` counts flushes from the same moment.
- **The step ranges.** The controller runs the workload in step ranges split at every fault's
  step. The workload program of a range gets `HYDO_FROM` and `HYDO_TO` in its environment
  (`docs/design/state-search.md`, section "Why a branch starts at a step range"). A schedule with
  its first fault before step 6 starts its first program with `HYDO_TO=6`, and one with its first
  fault before step 9 with `HYDO_TO=9`. A composed workload also names the boundary in its first
  command, as the handle `compose-step:6` of ClickHouse's `ch-e2`. So only runs with the same
  first fault step can share their first range.
- **The composition.** Every schedule of a composed workload draws its own `W=`, and the
  composer's first choice already differs. In `iggy-e1` the runs of seeds 5 and 9 both have their
  first fault before step 4. They part at their twelfth send, at 0.1 % of the run, where one
  composer sleeps 0.49 s and the other 0.99 s.
- **The walk.** Every schedule of a walk through a web app draws its own `I=`, and the walk
  script differs from the first workload command (`webshop-wc`).
- **The tokens that act at the start.** A setting token such as `R=everysec`, when its value is
  not the manifest's default, runs its command before step 0. A strict power loss
  (`L=...:strict`) turns the guest's writeback off before step 0, whatever its own step. A window
  of `H=` acts at its first step and splits the ranges there, as a fault does.

Some inputs do not keep runs apart. The guest clock starts from S1's clock in every run, the
agent's conversation starts with the same `sync`, and the controller empties the same three
files. These first sends take 0.0 % to 0.7 % of a run: the median time from S1 to the first
workload command (`prefix.py`).

## If the explorer drew differently

`models.py` computes what two other ways of drawing would share. Both keep the fault draws of
today and the order of the runs.

- **Same first step.** The explorer draws the composition, the walk, and the setting tokens once
  per exploration, and the controller is unchanged. A run then shares its first range with an
  earlier run that has the same timing tokens and the same first fault step k.
- **Any first step.** The draws are those of the first way, and the controller starts the
  workload program once per step. The steps before min(k, the largest earlier k) are then the
  same in every run. This changes the workload's processes of every run, so it moves every
  recorded fingerprint and needs a new fingerprint epoch.

| exploration | steps | median first step k as a share of the steps | median share of a run before k | same first step | any first step |
|---|---|---|---|---|---|
| `ch-e2` | 40 | 0.14 | 6.5 % | 1.1 % | 5.0 % |
| `ch-e4` | 40 | 0.30 | 20.9 % | 0.3 % | 18.7 % |
| `df-ex2` | 30 | 0.13 | 7.4 % | 5.6 % | 8.9 % |
| `iggy-e1` | 60 | 0.11 | 4.3 % | 0.7 % | 6.7 % |
| `iggy-e2` | 60 | 0.17 | 10.7 % | 2.6 % | 15.0 % |
| `iggy-e3` | 60 | 0.20 | 17.9 % | 10.7 % | 24.1 % |
| `iggy-e4` | 60 | 0.28 | 33.6 % | 13.1 % | 33.7 % |
| `rp-b3` | 120 | 0.15 | 4.2 % | 0.4 % | 3.2 % |
| `rp-b6` | 120 | 0.20 | 5.9 % | 0.5 % | 4.8 % |
| `surreal-e1` | 80 | 0.20 | 15.4 % | 3.7 % | 14.8 % |
| `tikv-ex4` | 80 | 0.16 | 6.0 % | 2.4 % | 7.0 % |
| `vm-e2` | 60 | 0.20 | 12.7 % | 5.3 % | 11.4 % |
| `vm-e3` | 60 | 0.14 | 10.5 % | 5.5 % | 14.9 % |
| `webshop-wc` | 4 | 0.62 | 63.2 % | 52.1 % | 58.2 % |

Over the 13 explorations of databases and queues, the first way saves 3.3 % of the first runs'
12,590 s (an estimated 415 s). The second way saves 10.9 % (1,366 s). The first fault falls
early, at a median step of 0.11 to 0.30 of the workload's steps, so the shared range is short.

Both ways cost search. A composition drawn once per exploration runs one interleaving of the
workload's commands instead of one per seed. A walk drawn once walks one path through the pages.
These draws find the workload's own bugs, and the 3.3 % to 10.9 % saved does not pay for them.
web-shop's 52 % is the extreme case: its four steps leave little room for a fault, and its
search is the walk itself.

Drawing the first fault later shares more, and it removes the schedules that fail. The 11
explorations of databases and queues with failures have 106 first runs that failed with a
violation or an error. Of these, 73 (69 %) had their first fault in the first quarter of the
steps (`extra.py`). In 9 of the 11 explorations, the median first step of a failing run was 0.08
to 0.20 of the steps. The minimal schedules that the shrinker kept start at 0.03 of the steps
(TiKV), and at 0.27 and 0.40 (Dragonfly). A draw that leaves the first quarter free of faults
draws none of those 73 schedules.

## Shrinks and repeated schedules

The shrinker is the one place where many runs of an exploration share a beginning. A shrink
candidate is the failing schedule with a fault left out or made weaker. So it keeps the faults
before the one that it changes. `roles.py` and `shrinks.py` measure the shrink runs of the three
explorations that shrank:

| exploration | candidates | their seconds | shared with any earlier run | with the failing run | with the last failing candidate |
|---|---|---|---|---|---|
| `df-ex2` (shrinks and error shrinks) | 97 | 418 | 43 % | 5 % | 9 % |
| `tikv-ex4` | 18 | 1,124 | 28 % | 6 % | 12 % |
| `webshop-wc` | 184 | 2,988 | 10 % | 0 % | 0 % |

The shrink runs took 27 % to 83 % of these explorations' seconds:

- 1,124 s of 4,093 s on TiKV;
- 418 s of 922 s on Dragonfly;
- 2,988 s of 3,600 s on web-shop.

Most of a candidate's shared beginning is with other candidates, not with the failing run. A
later round drops a fault that an earlier candidate also lacked. So one snapshot path, the
failing run's, saves 5 % to 12 % of the shrinks' clock. A tree over all candidates saves 10 % to
43 %.

A part of that tree is plain repetition. `dups.py` counts the runs whose schedule repeats an
earlier run's exactly, after it drops the `dseed=` of `disk=none` and `disk=cut`:

| exploration | role | runs | repeats | their seconds |
|---|---|---|---|---|
| `df-ex2` | shrink and error shrink | 97 | 21 | 88 |
| `tikv-ex4` | shrink | 18 | 3 | 238 |
| `webshop-wc` | shrink | 184 | 19 | 300 |
| `pg7-e3` | first | 300 | 287 | 800 |

The shrinker tries a candidate again in a later round. TiKV's `shrink-2-0`, `shrink-3-0`,
`shrink-4-0`, and `shrink-5-0` are one schedule. A repeated schedule needs no VM: the engine is
deterministic, so its result is the earlier result. Two kinds of runs repeat on purpose and stay:
the reruns from S1 that confirm a failure, and the confirmations of a minimal schedule.

## The engines today

Every engine can take a snapshot at a stop of a run and continue a run from it. The state search
(Milestone 212, `docs/design/state-search.md`) and the `hydo inspect` branches (Milestone 199,
`docs/design/agent-environments.md`) use it:

- `SNAP <name>` at a stop keeps the guest's state, the engine's book of sends, and the run's
  state. It does not change the guest, so a run with snapshots has the fingerprint of the same
  run without them. An engine refuses it while a typed send is not acknowledged.
- `RESUME <tag> <name>` continues a run from the snapshot in a fresh VM, with the snapshot's host
  part. First the host controller replays the parent's recorded stops to the new run's
  controller without a VM (`agent::check_branch`). It refuses the branch when that controller
  does not send exactly the parent's commands up to the snapshot.
- On the Mac engine and on KVM a snapshot keeps the tick jitter and the forced preemption points.
  The command `TIME` changes the timing from a stop on (the host token `br=`). hvx refuses
  snapshots of schedules with `cf=`, `pp=`, `xt=`, `q=`, or `tj=`.

| engine | what a snapshot holds | snapshots per worker | the cost that was measured | exactness that was shown |
|---|---|---|---|---|
| hvx | every non-zero page of guest RAM, in host memory | 8 | a branch cost two thirds of a fresh run on the bug benchmark and half on etcd, its prefix skipped (M212) | 6 of 6 branches of the d7 image gave their fingerprints again under `hydo run` (M212) |
| the Mac | the pages that differ from S1, in a file | 64 | the section [The Mac engine](#the-mac-engine) | 9 of 9 fault branches and 9 of 9 timing branches of Redis (M212) |
| KVM | the pages that differ from S1 | 64 | a call on a branch 19 to 43 ms; 1 MB to 46 MB of memory per branch (M199) | the state search's KVM change ran only in the scripted tests (open item 85); `hydo inspect` branches gave equal fingerprints for equal actions (M199) |

A snapshot keeps the run's disk policy, its seed too, and `RESUME` takes it over. No command
changes the policy at a stop. So today a continued run must have its parent's whole host part,
with its `dseed=`. That is why the comparison by the whole host part finds nothing.

## The Mac engine

No new Mac exploration ran for this note. The plan was the Redis quickstart with the random,
swarm, and timing draws, the composed Redis example, and a state search on Redis. The state
search's branches were then to run again with `hydo run`, to measure a snapshot's cost and to
check exactness on the Mac engine of release 0.4.1. The shared Mac VM lock stayed with another
session for more than 40 minutes. In that time this Mac's free disk fell to 12 GB, with 17.7 GB
of swap in use, so the runs were stopped before they started.

What is known of the Mac engine comes from earlier milestones:

- A snapshot keeps its RAM in an unlinked file in `HVM_SNAP_DIR`, and it holds only the pages
  that differ from S1 (`hydo-hvm/src/serve.rs`, the module's header).
- 9 of 9 fault branches and 9 of 9 timing branches of the Redis example gave their fingerprints
  again under `hydo run` (M212).
- A workload run of an agent image from S1 took 0.3 s, after a boot of 1.0 s (Milestone 121,
  "Cost"). A shared beginning of a few percent of such a run saves milliseconds.

The cost of `SNAP` and `RESUME` on the Mac, in seconds and in disk, was not measured. It is the
first measurement of the second part of the design below.

## A tree of snapshots and copy-on-write workers

A tree of snapshots holds one node per point where runs part. Each run below a node continues
from it. On the Mac engine and on KVM a snapshot holds only the pages that differ from S1. So a
node deep in the tree holds the pages that its path wrote since S1. Milestone 335 (tentative
number) builds copy-on-write workers on the Mac engine. This note assumes only that such a worker
maps S1's RAM and disk copy-on-write and keeps its own written pages.

Three things decide whether a tree fits such workers:

- **Where a node lives.** Today a snapshot lives in one engine process, so a branch runs on the
  worker that holds its parent. The state search runs on one worker for that reason. The shrinker
  runs a round's candidates side by side on all workers (`hydo-explore/src/run.rs`). So a tree of
  shrink candidates needs its nodes in files that each worker maps read-only and copy-on-write,
  as it maps S1.
- **How deep a tree is.** A shrink's tree has one level per round, and TiKV's shrink had 5 rounds
  (`shrink-1` to `shrink-5`). A node holds the pages written since S1, not since its parent. So a
  deep node costs what a shallow one costs, plus its own pages.
- **When a node is dropped.** A snapshot cannot be dropped today (M199): a second `SNAP` with a
  name replaces the first. A shrink can name its nodes by round and use the names again.

## The design that the numbers support

A tree of snapshots for the first runs of an exploration saves nothing with today's draws. The
draws that make it save 3 % to 11 % cost search. The measured savings are in two places, and the
first needs no snapshot.

1. **A repeated schedule takes the earlier result.** Before a run, the explorer compares the
   schedule with the schedules that it already ran, without the `dseed=` of `disk=none` and
   `disk=cut`. A repeat takes the earlier run's verdict, findings, and class, without a VM. The
   reruns from S1 and the confirmations keep their VMs.
2. **A shrink continues from its own snapshots.** Each shrink candidate's run takes `SNAP` at the
   first stop of each of its step ranges. A later candidate continues from the deepest kept
   snapshot up to which its controller sends the same commands, by `agent::check_branch`, with
   `RESUME`. This is the state search's mechanism, with parents that the shrinker already ran.
   It fits the Mac engine and KVM, where a snapshot holds only the pages written since S1. hvx
   keeps 8 snapshots of every non-zero page and refuses them for `cf=` schedules, so its shrinks
   stay as they are.

A repeat's fingerprint is the earlier run's, and the fingerprint covers the disk's seed. So the
first part has two ways to report a repeat. It names the repeat by the earlier schedule's text.
Or the explorer stops drawing a `dseed=` for `disk=none`, and for `disk=cut` without `torn=`. The
second way changes the text of every drawn schedule and the explorer's identity goldens. It moves
no value of `hydo/onboard/recorded.json`, whose schedules are fixed text.

The smallest first version is the first part alone. It changes the explorer only, and it cannot
change a run. Its saving is measured: 10 % to 21 % of the shrinks' seconds in the three
explorations that shrank. In `pg7-e3`, whose fault space is small, it is 94 % of the first runs'
seconds.

## Expected saving

| what | Dragonfly `df-ex2` | TiKV `tikv-ex4` | web-shop `webshop-wc` |
|---|---|---|---|
| seconds of the exploration: first runs, reruns, and shrinks | 922 | 4,093 | 3,600 |
| repeated shrink candidates, no VM | 88 s | 238 s | 300 s |
| shrinks from their own snapshots: the clock already run by an earlier candidate | 43 % of 418 s, an estimated 180 s | 28 %, 315 s | 10 %, 299 s |

The second row's saving includes the first row's, because a repeated candidate shares its whole
run. The snapshots add their own cost, which was not measured (section
[The Mac engine](#the-mac-engine)). An exploration without failures shrinks nothing and saves nothing. Of the 13
explorations of databases and queues in this note, 11 had a failure.

## Risks

- **A continued run that is not exact.** The check that a candidate's controller sends the
  parent's commands up to the snapshot is the state search's `agent::check_branch`. Its branches
  gave their fingerprints again on hvx and on the Mac. KVM has no such proof on a real host. The
  explorer confirms every minimal schedule from S1, so a wrong continuation shows as a minimal
  schedule that does not reproduce.
- **The disk's seed in the fingerprint.** A repeat that takes an earlier result under another
  `dseed=` must not claim that fingerprint for its own text. The section
  [The design that the numbers support](#the-design-that-the-numbers-support) gives two ways.
- **Memory.** A Mac or KVM snapshot holds the pages written since S1. On KVM that was 1 MB after
  a trivial command and 46 MB after a test run (M199). The larger examples have guests of 4 GiB
  to 7.5 GiB and write more. A worker keeps at most 64 snapshots.
- **Workers.** A snapshot lives in one engine process. A shrink that uses snapshots then runs a
  candidate on the worker that holds its parent, and a round's candidates no longer spread over
  all workers. Snapshot files that every worker maps remove that limit.

## Recommendation

No-go for a tree of snapshots over the first runs of an exploration. With the explorer's draws
of today it saves 0.07 % of the first runs' time. The draws that make it save more remove the
compositions, the walks, and the early faults that find bugs.

Go for the two smaller parts, in this order:

1. A repeated schedule takes the earlier result, in the explorer. The reruns and the
   confirmations keep their VMs. Measure it on the next explorations that shrink.
2. Shrink candidates continue from their own snapshots on the Mac engine and KVM. First prove on
   each engine that a continued candidate gives the fingerprint of its run from S1.

## Sources

Published work on running a shared beginning once:

- Michał Zalewski, *Fuzzing random programs without execve()*, 2014,
  https://lcamtuf.blogspot.com/2014/10/fuzzing-binaries-without-execve.html. AFL's fork server
  starts the target once and forks it before each input, so the program's start-up runs once.
- Dokyung Song and others, *Agamotto: Accelerating Kernel Driver Fuzzing with Lightweight Virtual
  Machine Checkpoints*, USENIX Security 2020. Agamotto keeps a tree of virtual machine
  checkpoints of the prefixes of system-call sequences. It continues each test from the deepest
  checkpoint that matches its prefix.
- Sergej Schumilo and others, *Nyx: Greybox Hypervisor Fuzzing using Fast Snapshots and Affine
  Types*, USENIX Security 2021, and *Nyx-Net: Network Fuzzing with Incremental Snapshots*,
  EuroSys 2022. Nyx-Net takes an incremental snapshot after a prefix of a test's network packets.
  It runs the inputs that share the prefix from that snapshot.
- Elia Geretto and others, *Snappy: Efficient Fuzzing with Adaptive and Mutable Snapshots*,
  ACSAC 2022. Snappy moves the snapshot to the point where the program first reads the part of
  the input that the fuzzer changes.
- Patrice Godefroid, *Model Checking for Programming Languages using VeriSoft*, POPL 1997, and
  Madanlal Musuvathi and others, *Finding and Reproducing Heisenbugs in Concurrent Programs*,
  OSDI 2008 (CHESS). Both search by running the program again from its start for every path,
  without stored states.
- Willem Visser and others, *Model Checking Programs*, ASE 2000 (Java PathFinder). It stores the
  states of its search and goes back to them, instead of running again from the start.
- Robert O'Callahan and others, *Engineering Record and Replay for Deployability*, USENIX ATC
  2017. rr makes checkpoints of a replayed process with `fork`.
