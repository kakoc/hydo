# hydo-control — handoff (milestone M120, step B1 done; what wave 3 and step C1 need)

Step B1 of `docs/design/guest-control.md` is done on branch `control-core` (docs/HISTORY.md
"Milestone 120"). Nothing in the product calls this crate yet, and no product behaviour changed.

## What exists

- `src/control.rs`: the `Controller` trait and what it sees and does at a numbered stop.
- `src/schedule.rs`: `ScheduleController`, which does what `hydo/onboard/hydo-init.sh` does, in
  its order, through hydo-agent's commands. One Rust function per shell function, with the same
  name. The background jobs of the shell are threads of `src/flow.rs`, a small runtime that runs
  each thread's straight-line code again over its recorded effects until it finishes (the replay
  technique of workflow engines), so the code reads like the shell and stays a pure function of
  what was seen.
- `src/plan.rs`: the core project model, read from a validated manifest with manifest.sh's
  defaults; `Layout` holds the guest paths that agent images must keep (step B3).
- `src/seams.rs`: one seam per feature for wave 3; `Seam::wave3()` says what the shell does today
  and what the controller has to do. A manifest or schedule that needs a seam is refused with
  `Unsupported`, never run with the feature missing. `SEAM(<name>)` comments in `schedule.rs` mark
  where each feature's code goes.
- `src/transcript.rs`: the control transcript (format 1) and its hash; `src/replay.rs`:
  `ReplayController`; `src/runner.rs`: one conversation over an engine (`Link`), with the agent
  session, the acknowledgements and the transcript; `src/lockstep_link.rs`: `Link` over
  `hydo_engine::lockstep::Lockstep`.
- hydo-results: `Run.control` (a list of `Control`, one per conversation), additive; the schema
  `hydo/schema/results-v1.json` was regenerated and still passes the additions-only check.

## The proofs (all on the Mac, no VM)

- `tests/corpus.rs`: every example manifest of the explorer's identity test is read into a plan;
  22 are core and 20 name their seam. Each core manifest boots on the scripted agent and engine
  (`tests/support/fake.rs`). Its schedules are the identity corpus's (40 seeds; 300 with
  `HYDO_CONTROL_FULL=1`) plus 11–14 written for the paths the explorer never draws for a
  manifest. 1,986 runs (525 with a power cut and a recovery boot) are each checked against the
  shell model (`tests/support/model.rs`), typed again from their transcript, and recorded as
  goldens (`tests/golden/corpus-quick.txt`, six readable transcripts in `tests/golden/examples/`).
- The shell model was shown to catch five deliberate mistakes in the controller (a wrong order at
  one point, a call not waited for, a wrong text escape, a cancelled checker that dropped seen
  answers, the last violated invariant instead of the first).
- `tests/conversations.rs`: solo mode against the shell model (88 runs), two runs give the same
  transcript, the transcript file reads back to the same records and hash, the results field, and
  the lock-step adapter over a scripted engine stream.

## Decisions where the shell is ambiguous

Each one is also marked `AMBIGUOUS:` in `src/schedule.rs`, and the shell model checks the
controller's choice.

1. **The environment of every command.** It contains HOME=/ and TERM=linux (what the kernel gives
   init), the init's exports (HYDO_STEPS, HYDO_ACKED, HYDO_OUTCOMES, HYDO_FAILED), the
   command's own variables (HYDO_FROM/HYDO_TO, HYDO_VALUE), and HYDO_SVC/HYDO_LOG of the last
   service started. The shell exports those two inside start_svc, so they reach every later
   command, and the controller keeps that. The entries are sorted by name. The kernel's unknown
   command-line words (`hydo_phase=serve`) are not reproduced. PATH is not set, as under the
   shell init, so busybox sh uses its built-in default.
2. **Standard input.** It is /dev/null for every command, because the agent owns the console.
   The shell's children inherited /dev/console.
3. **Readiness polls.** The shell starts each poll one second after the previous poll ended and
   makes at most `ready_timeout` polls. The agent's probe starts attempt k at k seconds after the
   start (or when attempt k-1 ended, if later) and gives up after `ready_timeout` seconds. Both
   make the same number of polls when a poll is fast. The log line "ready after N polls" omits
   the shell's uptime, because the host has no guest uptime.
4. **`kill -0` on a service that was just killed.** The shell may still see the process (a
   zombie that is not reaped yet). The controller decided that a killed service is down once its
   exit is reported: restart_dead, and start_svc for a restart, first wait for that exit. The
   agent refuses to start a handle that is still busy, so it would also refuse an early restart.
5. **A failed setup or provisioning step.** The shell also prints the last 8 lines of the log.
   This is not reproduced (it would need a `read`).
6. **A failed mount.** The text in parentheses is the agent's refusal message instead of busybox
   mount's error text. The verdict class is the same.
7. **Invariant output.** The controller keeps the output on the host and cuts it to its first
   3,000 bytes. The shell's file keeps all of it, so for a longer output the last HYDO-ORACLE line
   that inv_detail reads may be lost.
8. **A check that is still running when the checkers stop.** In the shell the check's child is
   not killed, so its output still reaches the file, and a later inv_detail of that invariant can
   read it. The controller takes in every answer seen up to the stop at which it stopped the
   checkers, and ignores answers that come later.
9. **Commands whose answer the shell ignores** (mkdir, file writes, signals) are not waited for.
   The agent carries out commands in order, so the next command still comes after them. Every
   command whose result the shell uses or waits for is waited for: runs, calls with an output file,
   probes, mount, the clock, sync and reads.
10. **The power cut's freeze.** It sends SIGSTOP to every service tree without waiting until
    every thread has stopped. The shell does not wait either; the agent's `wait_stopped` could.
11. **The workload's timeout** stays busybox `timeout` inside the command (SIGTERM, exit status
    143), not the agent's `kill_after_ms` (which kills the tree with SIGKILL).
12. **X= on onboarded images.** hydo-init.sh has no X=: it reads the token as an unknown knob and
    ignores it, and the explorer never draws X= for a manifest. The controller gives X= the
    stack's `kill_timer` meaning (hydo/explore/stack-explore.sh), as the task asked:
    - the timer is armed before step `at`, after a K= restart and before a K= kill;
    - it waits `%.3f` seconds, runs `pgrep`, kills the service's tree with SIGKILL, and writes a
      KILLED line and a `fault.killed_timer` record (declared only in schedules with X=);
    - restart_dead brings the service back at the end, after the timer was joined;
    - at a power cut the timer is stopped.
13. **Several tokens of one letter.** The last one wins, as in the shell's `case` assignments.
    Knobs are applied in token order.
14. **`sort -n` with equal numbers.** The ties are put in byte order of the whole line (GNU sort's
    last-resort rule). Busybox's rule was not checked; the ids are numbers in practice.
15. **The recovery boot's `command:` log line** is written as `oracle <acked ids> failed <failed
    ids>`. The engine's exact oracle line is the engine's business.
16. **Report lines belong to their conversation.** The assertion declarations come in the boot
    conversation. Whether a run's report includes lines from before S1 depends on the engine's
    console today, so step C1 decides how a run's report joins its conversations.
17. **die_oracle and a running pause timer.** In the shell, die_oracle does not stop a running
    pause timer. Here the timer also runs on until the conversation ends.

## What step C1 needs (the engine side)

- **A controlled boot phase.** The boot conversation starts at the agent's hello, takes S0 and
  S1 with `SNAP`, and ends with `Ending::AtSnapshot("S1")`. The run conversation resumes from S1
  with `Start::Resume(AgentPoint)` and the `GuestState` the boot's `snapshots()` give. The
  recovery conversation resumes from S0. (The other wave-2 agent builds the boot phase.)
- **`EngineAction::DiskFaultsOff`.** This is the shell's `go`. `LockstepLink` refuses it until
  the engines have a request for it.
- **`Ending::PowerCut { acked, failed, world }`.** The engine must cut the power, keep the flushed
  data disk and start the recovery conversation with these ids. `LockstepLink` refuses it until
  then. `world` (M125) is the fake world the first boot carried; the caller passes it on with
  `ScheduleController::recovery(..)?.with_world(&world)` (see "The fakes controller" below).
- **The results files.** Write `control.jsonl` (the run) and `control.recovery.jsonl`, put
  `Transcript::to_results()` into `Run.control` and `ctl=<hash>` on the RESULT line, and add
  `hydo repro --transcript` (`ReplayController`).
- **The cadence.** The schedule's `rb=` (or the engine's default) is the cadence the transcript
  records. Snapshots and their continuations must use the same cadence (M43, M118).

## What wave 3 needs (one line per seam; `Seam::wave3()` has the details)

- **liveness:** `eventually` invariants as one `probe` per anchor, with every_ms = poll and
  deadline_ms = within, and the records from the probe event.
- **clock:** `T=` as `clock_set {step}` with the 5-second check done on the host; and for
  clock_after_cut, a `clock_get` at the cut whose value is carried into the recovery
  conversation.
- **history:** `tail` the history file (or keep the console records), and run the `final:`
  read-backs before final_verdict.
- **freeze:** the pinned iptables through `run`, then `freeze`/`unfreeze`, the `froze`/`resumed`
  events and the fault.freeze_call records.
- **buggify:** one `write` of `/tmp/hydo-buggify` (mode 644) at the knob's place.
- **fakes:** done in M125 (branch `ctl-fakes`), see "The fakes controller" below.
- **error-logs:** `tail` or `read` the service logs, and classify them on the host with
  hydo-errors.
- **net-namespaces:** the pinned `ip` through `run` for the namespace plan, then `spawn` with
  `netns`.
- **composer:** `spawn` each composed command at its round (HYDO_SEED, HYDO_CMD_ID,
  HYDO_ROUND), with the drain and the kills.
- **net-faults:** the tc lines through `run`, `qlen` polls for the M71 drain, and the heal points
  (nto, oto, and at the end).
- **recovery-inputs:** the engine's hand-over (C1), the first boot's clock (clock) and the
  carried world (fakes, done in M125). The ids are already written by the controller.

## The fakes controller (M125, branch `ctl-fakes`; docs/HISTORY.md "Milestone 125")

`src/schedule/fakes.rs` is `hydo/onboard/hydo-init-fakes.sh` driven through the agent: one
function per shell function, called at the start of `start_all`, in `set_knob` for `H`, around
`step_range`, at the start of `restart_dead`, right after `failed_ids` at a cut, at the start of
`final_verdict`, and in the recovery's id writes. `Plan::world` (`PlanWorld`) holds manifest.sh's
HYDO_WORLD and HYDO_WORLD_TARGETS. `hydo-world` itself is unchanged (its Rust port is on branch
`port-world`); the controller only starts it and runs `hydo-world ctl ping|on|off|heal|dump|summary`.

The world is carried through the host: the cut's dump (`ctl dump` into `$H/world.dump`, then a
`read`) becomes `Ending::PowerCut::world`; the recovery is given it with `with_world`, writes it
into `$H/world.carry` with `write` commands (in pieces of at most 3,000 bytes) and starts the
world with `--carry`. The carried content is the shell's (the `w:` words of the acked line, in
its order). The 3,584-byte limit of the recovery command line no longer applies. The 3,000-byte
limit of the records is hydo-world's own (`carryMax`; a longer world is dumped as `w:trunc` and
every check answers "cannot decide"); the controller adds no limit, so a hydo-world without
`carryMax` needs no controller change, and shell images keep the old behaviour because nothing
in them changed.

Decisions where the shell is ambiguous (marked `AMBIGUOUS:` in `src/schedule/fakes.rs`; the
shell model checks each):

1. **The world's readiness.** The shell pings up to 300 times, sleeping 0.1 s after each failed
   ping, and stops when the world's process is gone. The controller sends one `probe` (every
   100 ms, deadline 30 s, `alive` = the world's handle). Both make the same number of pings when
   a ping is fast.
2. **`grep -c`, `tail -5`, `head -1`, `wc`.** They work on a `read` of the file on the host. The
   uptime in "fake world up" is left out, as in every log line. The 5 lines of `tail` are notes.
3. **`ctl on`'s error text.** The agent's output holds stdout and stderr together; the shell's
   `$(...)` keeps stdout only. When ctl cannot reach the world, the ERROR verdict therefore names
   the reason (the dial error). The verdict class is the same.
4. **`ctl dump` and `ctl summary` go through files** (`$H/world.dump`, as in the shell, and
   `$H/world.summary`, new) and a `read`, so no output limit applies (a `run` keeps at most
   3,000 bytes). The summary's stderr goes to /dev/null, as `2> /dev/null` does.
5. **At a cut, the dump stays on the host.** The shell appends it to `$HYDO_ACKED` and writes
   `w:trunc` into `$H/world.dump` after a failed dump; nothing in the guest reads either file
   after the cut (the power cut discards /tmp). The `hydo-explore-acked:` report line still
   carries the dump exactly as the shell's line did.
6. **The hand-over.** `Ending::PowerCut` gives the acked line's `w:` words as `world` and the
   other words as `acked`. Without a fake world every word stays an id, as before. The transcript
   writes `world` only when it is not empty, so every earlier transcript reads back the same.
7. **The recovery's id files.** The shell writes every word of the oracle line into the id files
   and `start_all` moves the `w:` lines into `$H/world.carry` with `grep` and `sed`, before
   anything reads the files. The controller never writes a `w:` word into an id file. The carry
   is the `w:` words among the acknowledged ids, then the carried world, then those among the
   failed ids: the shell's order, where the world followed the acknowledged ids.
8. **The recovery's `command:` log line** puts the carried records after the acknowledged ids
   (where the shell's line had them).
9. **The summary lines are report lines** (in the transcript hash), not notes: they are the
   outside world's record of the run.
10. **Handles.** The world runs under `fakes:world`; the dump and the summary under `fakes:dump`
    and `fakes:summary`. A service may be called `world`, so no bare name is used.
11. **The environment** is M120's decision 1 for these commands too (the prelude's exports are
    not there). hydo-world reads only HYDO_WORLD_SOCK and HYDO_WORLD_LOG, which no example sets.
12. **`ctl off` and `ctl heal` are waited for** (M120's decision 9: the shell waits for them).

The order against the other extensions (for whoever merges another seam): hydo-build splices
`hydo-init-fakes.sh` after the liveness, clock, history, freeze and BUGGIFY extensions and before
the error-log and network-namespace ones (`init_text`), and a wrapper spliced later runs its part
first. So:
- `step_range`: the error-log wrapper, then the fakes split, then freeze's, then liveness's;
- `set_knob`: the fakes `H` branch first, then BUGGIFY's and freeze's;
- `restart_dead`: the fakes heal before freeze's;
- `failed_ids`: the clock extension's part (its `clock=` word at a cut) before the fakes dump, so
  with both the shell's acked line is ids, then `clock=…`, then the `w:` records;
- `start_all`: the world starts before liveness's part;
- `final_verdict` is wrapped by the fakes only; liveness and history wrap `finish`, so their
  parts before the verdict come before the summary. netns wraps `start_svc` only.

The manifests with fakes and another feature (shop and redis-bridge: liveness; redis-bridge:
netns) are still refused by that feature's seam.

What C1 needs from this: pass `Ending::PowerCut::world` to `with_world`. Nothing else.

## Left open

- **Agent images and real guests.** No agent image exists yet (step B3), so the controller has
  only met the scripted agent. The first real-guest run should compare verdict classes with the
  shell images (design step C2).
- **Branching.** A mid-run branch (cloning the controller at a stop and continuing two engines)
  is supported by the types (`ScheduleController: Clone`), but no test checks it.
- **The full corpus.** `HYDO_CONTROL_FULL=1` (300 seeds) was not recorded as a golden.
