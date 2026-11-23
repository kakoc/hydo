# Shrinks from snapshots

Status: built and opt-in (Milestone 340, tentative number). `hydo explore --shrink-from-snapshots`
turns it on, and `--check-continued` proves each continued run against its run from S1. No engine
has it on by default yet (section [The proofs](#the-proofs)).

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
to the range's first command.

## The mechanism

**Snapshots at the range starts.** The host controller is a pure function of what it has seen.
At each stop, before the controller decides, the runner asks a copy of the controller whether its
step starts a step range (`Controller::starts_range`). When it does, the link takes `SNAP` there
(`Link::at_range_start`). The first range starts right after S1 and gets no snapshot. A `SNAP`
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

The numbers of this section are in `docs/HISTORY.md` Milestone 340.

## What is left

The list is in open item 170 of `AGENTS.md`.
