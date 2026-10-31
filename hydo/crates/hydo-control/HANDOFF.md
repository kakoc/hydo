# hydo-control — handoff (milestone M120, step B1 done; M122, four wave-3 seams done; what the rest of wave 3 and step C1 need)

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

## M122: liveness, clock, BUGGIFY and the recovery inputs (branch `ctl-liveness-clock`)

Four seams are driven (docs/HISTORY.md "Milestone 122"). Each feature is a child module of
`schedule` in `src/features/` (`#[path]`, so it uses schedule.rs's private helpers without any
visibility change), called from its `SEAM(<name>)` place with a one-line hook:

- `features/liveness.rs`: hydo-liveness.sh's four wrappers (after start_periodic: the `start`
  anchor; after step_range ending at the last step: `heal`; before start_all in the recovery
  boot: both; before finish: live_resolve). A monitor per eventually invariant is a flow thread
  (`live:i<k>`).
- `features/clock.rs`: hydo-init.sh's T= branch (it is core shell, not an extension), and
  hydo-init-clock.sh (the clock at the cut, the recovery boot's clock).
- `features/buggify.rs`: hydo-init-buggify.sh's set_knob branch.
- `features/recovery.rs`: `RecoveryInputs { acked, failed, clock, world }` (`world`: M125's records).
  `ScheduleController::recovery_inputs()` gives it after a run that ended with a power cut, and
  `ScheduleController::recovery_from(plan, &inputs, s0)` starts the recovery conversation with
  it (`recovery(plan, acked, failed, s0)` still exists and carries nothing). After the merge of
  M125, `recovery_from` passes `world` on with `with_world`, and the cut's acked line is the ids,
  then `clock=`, then the world's records (`tests/features.rs` checks that order on a shop
  variant with clock_after_cut).

Every hook does nothing for a plan without the feature, so the 1,986 runs of M120's corpus give
byte-identical transcripts (notes included). This was checked run by run against the M120
controller.

### Decisions where the shell is ambiguous (continued; each is marked `AMBIGUOUS:` or explained in the module)

18. **The liveness monitor is not the agent's `probe`.** A probe kills an attempt at the
    deadline, paces attempts from its own start, and reports neither the elapsed time nor the
    output, so it could not give the shell's `late` outcome, `elapsed_s` or details. The monitor
    runs the shell's loop instead: `timeout <within+1>` attempt, `clock_get`, compare,
    `sleep <poll>`.
19. **`UP` is the agent's CLOCK_MONOTONIC** (`clock_get.mono_ms`, shown in hundredths like
    /proc/uptime, which reads CLOCK_BOOTTIME; the two agree in a guest that never suspends). An
    unreadable clock reads as empty, which awk takes as 0, as in the shell.
20. **A monitor's environment is the init's environment at the anchor** (the shell forks the
    monitor then). A later start_svc does not change it. (Observation for the core: M120's
    periodic_loop builds its environment when each command is sent, so after a K= restart of a
    service other than the last one started, its HYDO_SVC/HYDO_LOG differ from the shell's
    subshell. The core model does not check the periodic job's environment. This was not
    changed here.)
21. **The monitors' files stay on the host** (`live.anchor.*`, `live.pid.*`, `live.res.*` are not
    written in the guest). The attempt's output is kept to 3,000 bytes (decision 7); the appended
    "satisfied ..." line follows the kept output, also when it does not end with a newline.
22. **T= reads the new time from the `clock_set` reply** (the agent reads the clock right after
    setting it). When the set is refused, a `clock_get` reads it, as the shell's `date +%s`.
    The clock is set to `now + delta` as an absolute second (the shell's `date -s @`), not
    stepped by delta, so the fraction of the second is dropped as in the shell.
23. **The clock at a power cut** is read with `clock_get` after failed_ids, as
    hydo-init-clock.sh does. It is not written into the guest's acked file (the VM is cut right
    after and nothing reads the file again). The report's acked line still shows
    `clock=<epoch>` byte for byte. `Ending::PowerCut.acked` and `RecoveryInputs.acked` hold ids
    only; the epoch is `RecoveryInputs.clock`. The recovery boot writes only the ids (the net
    effect of the shell's `sed -i '/^clock=/d'`), and its `command:` note shows the token after
    the acked ids. A missing or negative clock falls back to guest.clock_start, as the shell's
    `case` does.
24. **B=** is one `write` with mode 0644, not waited for (PID 1's umask is 022, so this equals
    the shell's `chmod 644`).

### Order against the other extensions (for the merge of wave 3)

hydo-build splices liveness first, then clock, history, freeze, buggify, fakes, error logs and
network namespaces; each later one wraps the earlier one's functions. So:
- in `finish`, history's `final:` read-backs come **before** live_resolve (hydo-init-history.sh
  wraps liveness's finish), and the final_verdict hooks of fakes and error logs come after it;
- in `start_periodic`, history's declarations come **after** the `start` anchor;
- in `start_all` of the recovery boot, fakes start the world **before** the anchors;
- in `step_range`, the `heal` anchor stays innermost: fakes split a range and call it per piece
  (only the piece ending at the last step stamps `heal`), error logs and freeze wrap outside;
- at the cut, fakes' `w:` lines come **after** `clock=` in the acked line (fakes wraps the clock
  extension's failed_ids).

### Proofs

- `tests/corpus.rs` (all manifests): T= and B= schedules now run (they were refused), and
  redis/hydo-live.yaml and compose-voting/hydo-loopback.yaml are core now. Its golden
  `corpus-quick.txt` was re-recorded. **Every wave-3 branch changes this golden, so after the
  merges it has to be re-recorded once (`HYDO_BLESS=1`) and the diff reviewed.**
- `tests/features.rs`: 2,611 runs on eleven plans (after the merge of M125; the eleventh is the shop with clock_after_cut) (the liveness and BUGGIFY manifests and variants
  with clock_after_cut, a second eventually invariant anchored at `start` or `heal`, and zero
  steps), every run and recovery checked against the shell model, both transcripts typed again
  by the ReplayController, 28 feature paths that must each be reached, golden
  `features-quick.txt`. Its second test makes 15 kinds of deliberate mistakes in the
  conversations and checks that the model refuses every one.
- `tests/support/model_features.rs`: the shell model of the four features, from the shell text.

## What step C1 needs (the engine side)

- **A controlled boot phase.** The boot conversation starts at the agent's hello, takes S0 and
  S1 with `SNAP`, and ends with `Ending::AtSnapshot("S1")`. The run conversation resumes from S1
  with `Start::Resume(AgentPoint)` and the `GuestState` the boot's `snapshots()` give. The
  recovery conversation resumes from S0. (The other wave-2 agent builds the boot phase.)
- **`EngineAction::DiskFaultsOff`.** This is the shell's `go`. `LockstepLink` refuses it until
  the engines have a request for it.
- **`Ending::PowerCut { acked, failed, world }`.** The engine must cut the power, keep the flushed
  data disk and start the recovery conversation. Its controller is
  `ScheduleController::recovery_from(plan, run_ctl.recovery_inputs(), s0)` (M122): the ids, the
  first boot's clock and the fake world (M125's `world`) go from controller to controller as
  `RecoveryInputs`, not through the engine or a console line. (`recovery(..)?.with_world(&world)`
  still works but carries no clock.) `LockstepLink` refuses the ending until the engines can do
  it.
- **The results files.** Write `control.jsonl` (the run) and `control.recovery.jsonl`, put
  `Transcript::to_results()` into `Run.control` and `ctl=<hash>` on the RESULT line, and add
  `hydo repro --transcript` (`ReplayController`).
- **The cadence.** The schedule's `rb=` (or the engine's default) is the cadence the transcript
  records. Snapshots and their continuations must use the same cadence (M43, M118).

## What wave 3 needs (one line per seam; `Seam::wave3()` has the details)

- **liveness:** done in M122 (a monitor thread, not a probe: decision 18).
- **clock:** done in M122 (decisions 22 and 23).
- **history:** `tail` the history file (or keep the console records), and run the `final:`
  read-backs before final_verdict.
- **freeze:** done in M124 (branch `ctl-net`), see "Network faults, the freeze and service
  networks" below.
- **buggify:** done in M122.
- **fakes:** done in M125 (branch `ctl-fakes`), see "The fakes controller" below.
- **error-logs:** `tail` or `read` the service logs, and classify them on the host with
  hydo-errors.
- **net-namespaces:** done in M124 (branch `ctl-net`).
- **composer:** `spawn` each composed command at its round (HYDO_SEED, HYDO_CMD_ID,
  HYDO_ROUND), with the drain and the kills.
- **net-faults:** done in M124 (branch `ctl-net`).
- **recovery-inputs:** done in M122 (`RecoveryInputs { acked, failed, clock, world }`, with
  M125's world); the engine's hand-over is C1's.

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
netns) are still refused by that feature's seam. (M124: netns is driven now; redis-bridge waits
only for liveness, and `tests/corpus_net.rs` runs it with its `eventually` check taken out.)

What C1 needs from this: pass `Ending::PowerCut::world` to `with_world`. Nothing else.

## Network faults, the freeze and service networks (M124, branch `ctl-net`; docs/HISTORY.md "Milestone 124")

Three modules beside `schedule/fakes.rs`, each called from its `SEAM` comment in `schedule.rs`:

- `src/schedule/netfault.rs` (NET-FAULTS): explore_run's `N=`/`O=` lines (read in the shell's
  order, their points, "healed before step", "healed at the end"), hydo-init.sh's `net_on` and
  `net_heal`, hydo-init-links.sh's `link_ends`/`known_svc`/`net_on` (a manifest with `addr:`), and
  hydo-init-netns.sh's `net_on`/`ns_fault_dev`/`ns_heal`/`ns_stats` (`network: bridge`).
- `src/schedule/freeze.rs` (FREEZE): hydo-init-freeze.sh's `fc_parse` (in set_knob), the
  declaration and `fc_arm` (in step_range), `fc_heal` (in restart_dead), `fc_kill` (before
  stop_periodic: in die_oracle, at a cut, after the retry), and the thread `freeze` that reports
  hydo_fc's console lines from the agent's `froze`/`resumed`/`warn` events.
- `src/schedule/netns.rs` (NET-NAMESPACES): `ns_setup` and its log line between the phase line and
  declare_all (before S0, so every conversation resumed from S0 or S1 has the network), and the
  namespace of start_svc's four commands.

`Plan` gained `freeze`, `bridge`, `net_extra` and each service's `addr`; `Layout` gained the pinned
tools `tc`, `ip`, `iptables` (manifest.sh's HYDO_TC, HYDO_IP, HYDO_IPT). The tool paths are image
facts: `Plan::from_manifest` fills hydo-build's `--print-shell` placeholders
(`/nix/store/...-iproute2/bin/tc`, ...) when the manifest's fault kinds need the tool, and leaves
"" otherwise (the shell's "no tc" paths). **C1 must set the image's real paths** (they are in its
`/etc/hydo/manifest.sh`), or the agent runs a program that does not exist. `Facts::events` keeps
every other agent event in order (froze, resumed, warn, and line for the tail users).

Decisions where the shell is ambiguous (marked `AMBIGUOUS:` in the module headers; the shell
model in `tests/support/model/net.rs` checks each):

1. **`$BB nsenter -n<file> <cmd>`** is the agent's `netns` field on the command (tc in a service's
   namespace, the per-service `sh -c` of ns_setup, start_svc's `.log.old` copy, setup, command
   and readiness probe).
2. **The netem queue length.** In the guest's own namespace, the agent's `qlen` (`tc -s qdisc
   show` and the backlog of handle 40:). In a service's namespace, where `qlen` cannot go, a `run`
   of `tc -s qdisc show dev eth0` there (3,000 bytes kept) and the shell's awk on the host.
   `ns_stats` is the same `run` with its awk on the host. `$tc` is one word.
3. **Every `tc`, `insmod`, `sleep 0.01` and `iptables`** is a `run` that is waited for, also where
   the shell ignores the status: the shell runs them one after another (M120's decision 9).
4. **netns.log** stays on the host (the outputs of the commands that wrote to it, then the
   `<svc>=<addr>@vh<i>` lines); the file is not written. On a failed setup its last 8 lines are
   notes (`tail -8`). The insmods' output is not added (they print nothing on stdout).
   `ns_v6off`'s builtins run as one `sh -c` with the same text; `: > $f` is a `write`; `mkdir -p
   $NSD` is the agent's `mkdir`, not waited for (as M120's mkdirs).
5. **hydo_fc is the agent's `freeze`.** Its arguments become the command's fields: `sa` →
   `sip`, a target port → `port`, `s<port>` (the workload peer) → `own_port`, `-` → absent, the
   match `-` → no `pattern`, n and dur by C's atol; the log is `$H/fc.log`, the queue 7. The marker
   files are the reply (`fc.bound`) and the events (`fc.fired`, `fc.resumed`), so their `rm -f` is
   not sent, and "freeze helper did not start" is the agent's refusal of `freeze`.
6. **A refused freeze is not armed**, so `fc_kill` (die_oracle) sends no `unfreeze`. The shell
   would kill the helper it had started. (The model found this difference; it is the controller's
   choice, because the agent has nothing to stop and would refuse the `unfreeze`.)
7. **A pair with neither an address nor a port** (a service without `addr:` and a peer without
   `port:`): hydo_fc started with `- -` and counted nothing; the agent refuses such a freeze, so
   the controller writes the helper's empty log instead and the rules give 3 ("skipped").
8. **hydo_fc's console lines** are report lines of the thread `freeze`, at the stop where their
   event is seen: `hydo-fc: FROZE <handle> (...)` (the handle where hydo_fc printed `pid <n>`; the
   host has no pids; the explorer's `froze_re` still matches), the `fault.freeze_call` record,
   `hydo-fc: RESUMED after <dur> ms (<n> processes)`, and `hydo-fc: <msg>` for the trigger's
   warnings. An event seen at the stop where the conversation ends is not reported (the main
   part ends the conversation first).
9. **fired/resumed** in fc_heal's first test and its wait are the events seen so far; after the
   trigger is stopped they are the `unfreeze` reply's (the markers' final state). "freeze helper
   had exited early" is never logged: the trigger is part of the agent.
10. **The "never fired" count** is the shell's `grep -c ' #'` in the guest (`sh -c` with
    `2>/dev/null`); the `#0` trace is fc.log read on the host and the shell's awk ported there.
11. **`cat $H/fc.ipt`** after failed rules: the rules' captured output, as notes.
12. **XTABLES_LOCKFILE** is exported from the arming on (the shell's `export` in fc_arm), into
    every later command of the main part. A background job of the shell keeps the environment of
    its fork; the controller's threads build each command when they send it (M120's approach), so
    a periodic check after the arming has the variable too.
13. **F= with `network: bridge`** is refused (seam freeze): the helper runs in the frozen
    service's namespace, and the agent's `freeze` has no namespace field. Doing it needs a
    protocol addition (`netns` on `freeze`, with a feature name) in hydo-agent-proto and the agent.

The order against the other extensions (hydo-build's `init_text`; a wrapper spliced later runs
first): `step_range`: the fakes split, then freeze's (declaration, arming), then hydo-init.sh's;
`set_knob`: the fakes `H`, then freeze's `F`; `restart_dead`: the fakes heal, then `fc_heal`;
`stop_periodic`: `fc_kill` first; netns's setup runs where /sbin/init reaches it (before main),
its start_svc wraps the whole of start_svc, and its `net_on`/`net_off` replace the links' and the
plain ones. With buggify, liveness, history or error logs merged, check their places against this.

The proofs (on the Mac, no VM):

- `tests/corpus.rs` now drives `redis/hydo-freeze.yaml` and `compose-voting/hydo.generated.yaml`
  (refused before), and every N=/O= schedule of the identity corpus (refused before); every run is
  checked against the shell model with its new network part, and its replay against the policy
  run. Boot transcripts of every other manifest are unchanged.
- `tests/corpus_net.rs`: schedules written for every path of the three features on redis, the
  netem example, etcd (links), the voting app (bridge), redis-bridge with fakes and bridge (its
  `eventually` check taken out; both architectures' copies) and the freeze example; plan variants
  for paths no example reaches (no tc; a freeze pair without a port; a pair without addresses or
  ports; a pair with two addresses; a failing network setup; F= with bridge refused); solo mode;
  the freeze sweep's point lists (M76: a `#0` trace per stream and step, the counts read from the
  runs' own "freeze never fired" lines, then every packet of every step; M92: every 8th packet,
  then a window of 16 around each failing point). Every path in its `PATHS` list (41) is reached.
  Ten kinds of mistakes made in recorded conversations are each caught in every run they apply to.
  Four mistakes made in the controller's code, one at a time, were caught and reverted (a failed
  `qdisc change` that does not stop the drain, a heal that waits one second longer, no `unfreeze`
  at a power cut, IPv6 left on hydo0).
- Goldens: `tests/golden/corpus-net.txt`, readable transcripts in `tests/golden/examples-net/`.

What C1 needs from this: the image's tool paths in `Layout`. Nothing else: the freeze, the netem
faults and the namespaces are all agent commands.

## Left open

- **Agent images and real guests.** No agent image exists yet (step B3), so the controller has
  only met the scripted agent. The first real-guest run should compare verdict classes with the
  shell images (design step C2).
- **Branching.** A mid-run branch (cloning the controller at a stop and continuing two engines)
  is supported by the types (`ScheduleController: Clone`), but no test checks it.
- **The full corpus.** `HYDO_CONTROL_FULL=1` (300 seeds) was not recorded as a golden.
