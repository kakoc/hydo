# Shrinks from snapshots

Status: built (Milestone 340, tentative number). It is on by default with the KVM engine and off
on the Mac engine and hvx (section [The proofs](#the-proofs)). `hydo explore
--shrink-from-snapshots` and `--shrink-from-s1` choose, and `--check-continued` proves each
continued run against its run from S1.

## The problem

The shrinker makes a failing schedule smaller. A schedule is the text that names the faults of
one run, for example `disk=none dseed=0 | run K=redis@11+3 C=20`. Each round of the shrinker runs
candidates: the current schedule with one fault left out or made weaker. Every candidate runs from
S1, the snapshot that each worker takes at the end of the boot.

A candidate keeps the faults before the one that it changes. So it runs what an earlier run of the
shrink already ran, up to the point where the two differ. Milestone 337 measured this on hvx
(`hydo/explore/RESEARCH-shared-prefix.md`). Shrinks took 27 % to 83 % of the seconds of the
explorations that shrank. A candidate's run shares 10 % to 43 % of its guest clock with earlier
runs of the same shrink.

## Where two runs part

The host controller runs the workload in step ranges. A fault at step k splits the workload at k,
and the workload program of a range gets `HYDO_FROM` and `HYDO_TO` in its environment. A candidate
that leaves out the fault at step 9 of `K=redis@5+2 P=redis@9+2` runs the ranges 0 to 4, 5 to 6,
and 7 to the end. The parent ran 0 to 4, 5 to 6, 7 to 8, 9 to 10, and 11 to the end. The two runs
send the same commands up to the start of the range at step 7. There the candidate's command has
another `HYDO_TO`.

So the points where runs part are the starts of the step ranges. A snapshot there holds the run up
to the range's first command. A composed workload (`W=`) has points of its own. Its
candidates often leave out rounds of the composition (`W=...,x12-14`), and such a candidate parts
from its parent where the first left-out round starts. So the start of each round is a point too.

## The mechanism

**Snapshots at the points.** The host controller counts the step ranges and the rounds that it
started (`Controller::ranges_started`), in its host state only, so no command changes. The runner
compares the count before and after each step. When it grew, the link takes `SNAP`
(`Link::at_range_start`) before any of the step's commands goes to the engine. So the snapshot
holds the guest as it was before the controller decided at that stop. The first point comes
right after S1 and gets no snapshot. A composed run spreads its few snapshots over its rounds: at
most one in `steps / (per run + 1)` points. A `SNAP`
changes nothing in the guest, so the run's fingerprint and transcript stay the same. An engine
that refuses the snapshot, for example while a typed command is not acknowledged, leaves that
point out.

**Which runs take them.** The reruns of a failure (`repro-`) and the shrink candidates (`shrink`,
`errshrink-`) take snapshots. First runs and the confirmations of a minimal schedule (`min-`) take
none.

**Which candidates continue.** A candidate continues from a snapshot of its worker when two things
hold:

- the engine's schedule is the snapshot's own: the same host part, with its `dseed=`, timing tokens
  and cadence;
- the candidate's controller sends exactly the commands of the snapshot's run up to the snapshot.
  The state search's offline check (`agent::check_branch`) runs the candidate's controller over the
  parent's recorded stops without a VM.

The worker takes the deepest such snapshot by guest clock and sends `RESUME`. The runner replays
the parent's recorded stops to the candidate's controller and goes live at the snapshot. This is
the branch of the state search (`docs/design/state-search.md`) with parents that the shrinker
already ran. The answers of the offline check are kept per snapshot.

**Where the snapshots live.** A snapshot lives in its engine process, so a candidate continues
only on the worker that took its parent. The pool lets a free worker take, among the jobs that
would start now (one per worker), the one that it continues from the deepest snapshot. It never
waits for a job that another worker could take, so no worker stays idle. Snapshots that every
worker maps, as Milestone 335 maps S1 on the Mac, would remove that limit. They need a snapshot
format that another engine process can load: the vCPU, the devices, and the engine's book of
sends besides the RAM base.

A first version asked a copy of the controller before each step whether the step would start a
range. The copy of the controller's whole state at every stop cost more than the snapshots found.

**How many are kept.** A worker keeps 8 snapshots and a run takes 4 at most, for a guest under
2 GiB. A larger guest keeps 4 and takes 2. hvx keeps 2. Its 8 names include S0 and S1, and each of
its snapshots holds every page of guest RAM that is not zero, in host memory. A new snapshot takes
a free slot, else the least recently used one, never the one that the run continues from.

**The fallback.** A candidate that reaches no snapshot runs from S1. A continuation that fails
drops its snapshot and runs the candidate from S1 again. A run that ends in `SPIN` on the Mac runs
again from S1 in the rewind mode, as before.

## Exactness

A continued candidate must give the RESULT line, the consoles, and the control transcripts of the
same candidate from S1. `--check-continued` checks this for every run that took or used a snapshot.
The worker runs the schedule again from S1 without snapshots and compares:

- the RESULT line without its wall seconds: the fingerprints `fp` and `wfp`, the control
  transcript's hash `ctl`, the engine's counts, and the verdict;
- every file of the run under its tag: the console report `<tag>.log`, the control transcripts,
  and a recovery boot's files.

The engine's own console (`<tag>.agent.log`) is left out, because a continued run has only the part
after its snapshot. The result is the run from S1's. Each difference is printed and listed in
`shrink-snapshots.json`.

## The Mac engine's snapshot

A Mac snapshot is a RAM base (Milestone 335): a file of the guest RAM's pages that are not zero,
which guest RAM is then mapped from. Its making reads the whole guest RAM. Until this milestone
every snapshot was also named by its RAM tree hash, so that another engine with the same bytes
finds its file. That hash read the RAM a second time. Only the host's S0 and S1 are named now. A
run's own snapshots get a file of their own, which is unlinked at once.

## The proofs

Every run that took or used a snapshot and was compared with its run from S1 matched:

- on the Mac engine, 22 to 24 runs of each Redis quickstart exploration and 98 to 105 of each
  `redis-composed` exploration;
- on KVM, 41 runs of each of four GitHub runners (three Xeon 6975P-C, one EPYC 7763), with the
  Redis example's random and swarm strategies.

On KVM a snapshot holds the pages written since S1, in the engine's memory. The 16 to 30
snapshots of an exploration took 0.0 to 0.1 s together. The shrinks and the reruns took 1 % to
7 % less time. On the Mac they took longer, because each snapshot reads the whole guest RAM. The
numbers are in `docs/HISTORY.md` Milestone 340.

## What is left

The list is in open item 170 of `AGENTS.md`.
