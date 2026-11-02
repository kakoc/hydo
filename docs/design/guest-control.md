# Guest control from the host (M116, design)

Status: design and feasibility experiments, 2026-10-31. Nothing in the product changed. The
protocol's authority will be its Rust types, the schema generated from them and the example
transcripts (see "Protocol"); this note explains the decisions and the plan, not the format.

## The problem

Every image carries `/sbin/init` = `hydo/onboard/hydo-init.sh` (448 lines). It starts services,
runs the workload in steps, applies the process-level faults (kill, pause, netem on a port or a
link, freeze at a packet, clock steps), runs the checks and prints the verdict. Eight optional
features attach by wrapping its shell functions: `hydo-build` copies a function under a new name
and splices an extension that redefines it (the `WRAP` lists in `hydo/crates/hydo-build/src/init.rs`:
liveness, clock, history, freeze, BUGGIFY, fakes, error logs, network namespaces). This has four
costs:

- The order of the wrappers is implicit (it is the order of the `if` blocks in `init_text`), and
  most combinations were never run (M86, M94 and M95 each list untested combinations).
- Every feature and every fix changes the image, so every fingerprint of that image moves.
- The guest decides everything from one command line typed at S1. Nothing can react to what a
  run shows while it runs.
- The host↔guest protocol is a handful of console markers and one input line per phase
  (`provision`, `run <tokens>`, `go`, `oracle <ids>`), with a 3,584-byte limit for the recovery
  command (M89).

Device-level faults (disk, power cut, virtual NIC, tick timing) already come from the
hypervisor. The goal: the host controls the guest, also at run time and in reaction to what it
observes, and a small agent in the guest only executes commands.

## Recommendation in one paragraph

Replace the shell init by one small static Rust program, `hydo-agent`, which is PID 1 of the
guest and executes primitive commands (start a process, signal a process tree, run a command and
report its exit status, write a file, set the clock, configure a qdisc through a pinned `tc`,
arm a packet trigger, stream a file). All decisions move into a host-side controller in Rust: the
schedule, the step sequence, the composer's rounds, liveness deadlines, the heal, the checks'
order and the verdict. Commands travel on the serial input, typed by the engine at an exact point
of virtual time (hvx: the instruction clock `icount`; hvm: the exit count); replies and events
come back as framed lines on the console. A later, opt-in module addition (a "host-call port"
whose write stops the run at that instruction) makes the host's observation time exact and the
channel synchronous; the design works without it. Every host decision is a pure function of the
schedule, the seed and the events seen so far, and every run records the concrete commands it
sent with their times (the control transcript), so replay, branching and shrinking work on
recorded facts. Today's schedule strings keep their meaning: the static schedule becomes one
controller among others.

## What stays in the guest, and what moves

**The agent (`hydo-agent`, one static binary per architecture, Rust, std only plus `libc`).**
It is PID 1: it mounts `/proc`, `/sys`, `/dev`, `/tmp`, seeds the CRNG (`hvx_seedrng`'s job; its
pinned bytes stay as they are, the agent calls it), reaps zombies, and then serves commands.

It does:

- process control: spawn with argv, env, cwd, uid, network namespace, stdout/stderr to a file;
  remember it under a handle; signal the whole process tree of a handle (what `svc_tree` does
  today with awk over `/proc`); wait for exit;
- `probe`: run a command repeatedly until it exits 0 or a guest-time deadline passes (readiness
  checks, `eventually` polls), and report the attempts;
- files and mounts: write, append, read (bounded), mount the data disk, `sync`;
- the realtime clock: `clock_settime` (`T=`, the start clock, `clock_after_cut`);
- network: run the image's pinned `tc`/`ip`/`iptables` with given arguments (the agent does not
  speak netlink itself), and report the netem queue length (the M71 drain);
- one in-guest trigger: hold a packet in an NFQUEUE and stop a process tree at the n-th match
  (the `hydo_fc` logic of M70). This is the only command that acts on its own between two host
  decisions, because the freeze must happen before the frozen service runs another instruction;
- streaming: forward new lines of a file (a service log, the history log, `world.log`) as events.

It does not: parse schedules, draw random numbers, decide when anything happens, read the clock
to decide, keep timers of its own (only the deadlines a command gives), or print verdicts. While
it waits for a command it is blocked in `read()`, so an idle agent costs no exit and no guest
time.

**The host controller (`hydo-control`, a Rust crate used by the engines' callers).** It owns
everything the init decides today: the order of service starts and readiness waits, provisioning,
the step sequence and each step's timeout, the fault timeline (which fault, before which step, for
how long, in guest time), the heal (lift netem, resume, restart, the workload's retry), invariant
scheduling (`every:`), liveness deadlines (measured in the guest's virtual time, which the host
knows exactly), the composer's rounds (the composition is already a pure function of `W=` that
the host computes in `hydo-composer`), the history `final:` read-backs, BUGGIFY's configuration,
the error-log scan (the host reads the streamed log; classification is already host-side in
`hydo-errors`), and the verdict. The power-cut recovery boot is a second conversation with the
agent of the recovery VM: the acknowledged ids, the carried fake world and the clock are sent as
commands, so the 3,584-byte recovery line disappears.

## The channel

### Phase 1: no module change

- **Host → guest.** The engine types a command line into the UART receive queue
  (`HVX_PUT_INPUT` on hvx, `hvm_put_input` on hvm). The guest's 8250 driver takes it through the
  instruction-timed injection path (M18; on the Mac at an exit where the guest has interrupts
  unmasked, M48). The agent reads it from `/dev/ttyS0` in raw mode with echo off.
- **Guest → host.** The agent writes one framed line per reply or event to the same UART:
  `HYDO-AG <len> <crc32> <json>`. The engine reads it at its next stop (`HVX_GET_CONSOLE`; hvm
  reads its console buffer at every exit).
- **The exact point.** The engine delivers a command at an exact point of virtual time:
  - hvx: the instruction clock `icount` (struct `hvx_run_ctl.icount`: +1 per retired
    deterministic exit, +2048 per PMU tick). The engine caps each `HVX_RUNC` event budget so that
    no call runs past the target, and types the command at the first stop with `icount >= target`.
    With the PMU clock, ticks land on exact branches (M33), so these points are branch-exact; with
    the exit-count clock they are exit-exact. Several vCPUs: `HVX_RUNC` reports the scheduler's
    shared clock (`vm->sched.icount`) and stops only at clean turn boundaries, so the point is the
    first clean boundary at or after the target.
  - hvm: the exit count `M.exits`; the stop hook runs at every exit, so the point is exact to the
    exit.
- **The observation point.** The host learns of a guest line at the first stop after it was
  written. With a fixed stop cadence (`rb=<n>` on hvx: every run call returns after at most n
  events; hvm: every exit) that stop is a pure function of the run, so a decision taken "at the
  stop where event E was first seen" is deterministic. Its time is recorded as the stop's
  `icount`/exit count, so replay does not depend on the cadence. The cadence decides how late
  the host sees a line: with plain run calls (a return every 2^16 retired exits) the experiment's
  host saw a mid-workload line only at the end of the Redis workload. Control mode therefore uses
  a small fixed cadence (`rb=1` stops at every event; its cost is in the experiments), and the
  cadence is part of the recorded run like every other host parameter. The end-of-run
  fingerprint is taken at a recorded event too (the agent's final acknowledgement), not at "the
  first stop after a marker".

Problems with Phase 1, and how they are handled:

- The kernel's console messages share the UART. The agent's lines are framed (prefix, length,
  CRC) and the guest kernel runs with `quiet loglevel=1` after boot; a line that fails its CRC is
  an ERROR of the run (it is the same in every replay, so it cannot be flaky, only wrong).
- Today's init also reads the console (`read -r CMD < /dev/console`). The experiment below shows
  what sharing does: a line typed mid-run was echoed by the tty and later consumed by the init's
  next `read` in place of `go`. The agent owns the tty: it sets raw mode and no echo before it
  announces itself, and nothing else reads it.
- Bytes still in the UART receive queue are not in any snapshot (hvx: transient host input, M18;
  hvm: `rxq` is outside `struct mach`). Rule for the engines: the engine keeps the list of
  commands it typed with their icount; a snapshot carries the commands the agent has not yet
  acknowledged (its reply proves the bytes left the queue), and a restore types them again at the
  snapshot's point. A command whose bytes were partly read before the snapshot is safe because
  the restore first sends the resynchronisation line (below), which makes the agent drop a
  partial line, and the agent ignores a command id it has already executed. (Saving the receive
  queue in the module's snapshot is the cleaner fix; it belongs to Phase 2.) The experiment shows
  what breaks without the rule: a line typed at the snapshot's own stop was lost in every
  continuation.
- The receive ring holds 4,096 bytes (hvx and hvm); a command must fit. Large payloads (a fake
  world, a list of acknowledged ids) go as several commands to a file, then one command uses it.

### Phase 2: a host-call port (opt-in module and hvm additions, proposed)

A synchronous exchange point between guest and host: the agent writes a request with
one `rep outsb` to a new I/O port (hvx) or MMIO page (hvm); the engine's run call returns at that
exact instruction (`HVX_EXIT_HOSTCALL`, the bytes in a per-VM buffer); the host decides; its
answer is read by the agent with one `rep insb` from the port (the module's synchronous
fuzz-input port 0x510/0x511, `HVX_SET_FUZZ_INPUT` in `hvx_abi.h`, already has this shape: each
read is an exit, the bytes are a pure function of a staged buffer, and it is inert when empty;
it is not in the snapshot, which the new port must be). Effects:
observation time = the instruction that wrote the record (no cadence, no console scraping, no 64
KiB console copies per stop), the host can answer before the guest runs one more instruction, and
the agent's records no longer mix with the kernel log. Host-pushed commands (a fault at time T
while the agent sleeps) still need a doorbell: one byte on the serial input wakes the agent, which
then fetches the command through the port. Requirements: inert unless the VM opts in (a new
`hvx_config.flags` bit), the staged answer and the doorbell state in the snapshot (unlike today's
transient input), developed on a side module (`/dev/hvxgc`) and merged only after the full suite
and the pinned reproducers.

**Built on the side module (M138, hvx only; docs/HISTORY.md "Milestone 138").** What differs from
the proposal above: the agent is a user-space program, and a `rep outsb` from user space would make
the module walk the guest's page tables, so a record is written as 4-byte OUTs to a data port
(0x5f0) and committed by one OUT of its length to a control port (0x5f4); the run call returns at
that commit (`HVX_EXIT_HOSTCALL`, a `why=hostcall` stop). The host's lines are staged with
`HVX_HC_PUT` and read by the agent with 4-byte INs (0x5f8 gives the count), after one doorbell
byte on the serial input. The port's buffers (a partial record, staged bytes not yet read) are
saved with the engine's snapshots (`HVX_HC_GET_STATE`/`HVX_HC_SET_STATE`), not inside struct
hvx_snapshot, so every older binary keeps its ABI. The negotiation: the engine offers the port
(by default since M140, `HYDO_HVX_HOSTCALL=0` turns it off: the VM flag `HVX_VM_HOSTCALL` and,
when the module has the port, `hydo.hc=1` on the kernel command line),
and the agent takes it only when the port answers its magic (hydo-agent 0.3.0, hello feature
`hostcall`); the engine switches the host's lines to the port when the agent's first record
arrives there, so an older agent, an older engine or the main module stay on the serial line.

virtio-serial and vsock were considered and not chosen: they carry connection state that
snapshots break (Firecracker resets vsock connections at a snapshot), they need a third virtio
device model in the C module with its own deterministic queues and interrupts, and the messages
here are small and rare.

## The protocol

- **Source of truth:** a crate `hydo-agent-proto` (no dependencies besides serde) with the
  command, reply and event types. A test generates the JSON schema from the types and compares it
  with the committed `schema/agent-v<N>.json`; another test replays every file in
  `tests/transcripts/` (host lines and agent lines of real runs, never edited after they are
  committed) through the agent's command handler on a scratch root and through the host parser.
  This follows the project and results formats: a version number, additions only, a generated
  schema checked by a test, and permanent example transcripts.
- **Messages:** one canonical JSON object per line (sorted keys, integers only, no host times,
  no floats). Host: `{"v":1,"id":7,"op":"signal","h":"redis","sig":"KILL","tree":true}`. Agent:
  `HYDO-AG … {"v":1,"re":7,"ok":true}` or `{"v":1,"re":7,"ok":false,"err":"no-such-handle"}`.
  Events: `{"v":1,"ev":"exit","h":"redis","status":137}`, `{"v":1,"ev":"line","h":"redis.log",
  "n":42,"text":"…"}`, `{"v":1,"ev":"probe","id":9,"ok":true,"tries":3}`. Every event has a
  sequence number, so a lost line is detected.
- **Hello and version skew.** The agent's first line is `{"ev":"hello","proto":1,
  "agent":"0.1.0","ops":[…],"features":[…]}`. The agent is inside the image, so its version is
  fixed by the image and by every fingerprint; the host is normally the newer side. The host
  supports every protocol version ever released (with additions only this is one code path with
  feature checks), sends only ops and fields the hello lists, and refuses an image whose agent
  speaks a newer protocol with a message naming both versions ("this image was built by hydo
  0.3; this hydo knows agent protocol 1..2; upgrade hydo"). The agent rejects unknown fields
  (they would otherwise be silently ignored and change the meaning of a run) and answers an
  unknown op with `unknown-op`, never by stopping. Kata's 1.x/2.x skew ("unexpected EOF") is the
  failure this rules out.
- **Resynchronisation.** Each run starts at a snapshot point where the agent is idle; the first
  host line after a restore is a `sync` with a nonce the agent echoes (qemu-ga's `guest-sync`), so
  a partial line left from an earlier state is detected.

## Determinism, replay, snapshots

- **Rule.** A host decision is a pure function of the schedule string, the seeds in it and the
  events observed so far, each with its virtual time. The controller never reads wall time,
  thread timing, the worker's identity or the pool size. It is written in Jepsen's later
  generator style: `(state, event) -> (state, actions)`, with time as an explicit input.
- **The control transcript.** Every run writes `control.jsonl`: each line is
  `{"at":<icount or exits>,"send":"<exact bytes>"}` or `{"at":…,"seen":<seq>}`. Its hash goes into
  the RESULT line (`ctl=<hash16>`), next to `fp=`/`wfp=`. `hydo repro` replays the schedule (the
  policy runs again) by default; `hydo repro --transcript <file>` types the recorded bytes at the
  recorded times without running the policy. Both must give the same fingerprint; a test checks it
  on every example image. Replaying a log rather than recomputing decisions is what rr and QEMU's
  record/replay do, and it keeps a saved failure valid after the controller's code changes.
- **Anchors, not absolute times.** A resolved action is written relative to a guest event: "at
  the stop where step 5's start was acknowledged, +0", "2,000 events after the first
  `KILLED redis`". Absolute `icount` values are only the replay coordinates. The shrinker works on
  anchored actions, so removing an earlier fault does not leave later faults at meaningless times
  (the schedule language's steps are already such anchors).
- **Today's schedules.** The static schedule becomes `ScheduleController`, which issues the same
  primitives the init issues today at the same anchors (before step k, after n ms of guest time,
  at the n-th packet). Every schedule string the explorer can generate keeps its meaning; its
  fingerprints change once (the agent is not the shell), which is the one re-baseline below.
- **Snapshots and branches.** Host state at a snapshot is (schedule, seeds, events so far), which
  a branch copies; a branch's continuation sends the same commands at the same anchors as the
  straight run unless the explorer changes them. The engine snapshots only with an empty receive
  queue (above). One more rule from M43: snapshots and their continuations use the same run-call
  cadence (`rb=`), because the end-of-run fingerprint is taken at a stop.
- **Checks that keep it honest.** For every new op: one (fresh boot) equals serve (restored S1);
  a branch continued without changes equals the straight run; two runs give the same transcript
  hash. madsim and FoundationDB run such a determinism check on every test, not on demand;
  `hydo explore` already re-runs every failure and should also compare transcript hashes.

## What dynamic control enables

- **Faults at interesting moments.** React to an assertion (`sometimes` first satisfied, an
  `always` about to be checked), a log line, a history record, a held write in the disk's cache
  (`@held` already exists), or a packet pattern (the freeze is the in-guest version). Example
  rules: "pause the leader for 300 ms the first time a client times out", "power-cut 2,000 events
  after the first `fsync` of the WAL".
- **Branching there.** Snapshot at the reaction point and explore several continuations. M43 and
  M45 found, on the stack, that variety of fresh schedules beat exploring around one moment; this
  is a capability to measure on the bug benchmark (`hydo-bench`), not a promise.
- **Liveness and budgets.** The host heals when the system has stopped making progress or after
  a no-progress budget (TigerBeetle's VOPR resets its request budget on every reply) instead of
  at a fixed step, and stops a run early once a violation is certain.
- **Adaptive workloads.** The next command can depend on earlier outcomes (Jepsen's generators),
  for example retrying exactly the operations whose outcome was unknown.
- **Explorer API.** `hydo-engine` gains one method and one trait; `run()` stays as it is:

  ```rust
  pub trait Controller {
      /// Called at every stop with the events read since the last one. Returns what to send and
      /// when (at once, or at a later virtual time), and whether to snapshot or stop here.
      fn step(&mut self, now: VTime, events: &[AgentEvent]) -> Vec<Action>;
  }
  pub enum Action { Send { at: VTime, cmd: AgentCommand }, Snapshot(String), Stop }
  trait Worker {
      fn run_controlled(&mut self, tag: &str, ctl: &mut dyn Controller) -> Result<RunResult, EngineError>;
      fn resume(&mut self, snap: &str, ctl: &mut dyn Controller) -> Result<RunResult, EngineError>;
      // run(tag, schedule) == run_controlled(tag, &mut ScheduleController::new(schedule))
  }
  ```

  The process engines (`hvx_explore`, `hvmx`) carry it over their stdio protocol in lock step:
  `EVENT <tag> at=<icount> <line>` and `STOP <tag> at=<icount>` from the engine, `SEND at=<icount>
  <bytes>`, `SNAP <name>` and `CONT` from the host; the engine waits at each stop for `CONT`. The
  planned in-process engine (crate documentation of `hydo-engine`) calls the controller directly.
  Strategies in `hydo-explore` then produce controllers instead of strings, and a run's resolved
  actions are printed back as schedule tokens where the language has them.

## How each current feature maps

| Feature (today) | Host controller | Agent needs |
|---|---|---|
| S0/S1, provisioning (`provision`, markers) | sends mount, the start sequence, `first` commands, `sync`; takes S0/S1 at the agent's idle acknowledgements | `mount`, `spawn`, `probe`, `sync` |
| Services, readiness, crash restart (`start_svc`, `restart_dead`) | start order, timeouts, restart decisions | `spawn` (with netns), `probe`, `exit` events |
| Workload steps (`step_range`, timeout, retry) | step sequence and per-step timeout in guest time | `spawn` with `HYDO_FROM`/`HYDO_TO` env, `signal` on timeout |
| `K=` kill, `P=` pause (both forms), `X=` timed kill | when (anchor + guest-time offset), restart/resume | `signal` on a tree (`KILL`, `STOP`, `CONT`); a "wait until every thread stopped" flag |
| `N=`/`O=` netem, links, heal and drain (M71) | arguments, windows, when to heal | run pinned `tc`; `qlen` query for the drain loop |
| `F=` freeze at a packet (M70, M76, M92) | which pair, n, match, duration | the NFQUEUE trigger (in-guest, by necessity); `trace` mode streams the packet trace |
| `T=` clock jump, `guest.clock_start`, `clock_after_cut` | the value; the recovery clock from the first boot's last value | `clock_set`, `clock_get` |
| `B=` BUGGIFY (M56, M80) | the seed and rates | `write` `/tmp/hydo-buggify` |
| Knobs `M=`, `R=`, … | the knob's `set` command with `HYDO_VALUE` | `spawn` |
| Invariants `every:`/`final:`, `HYDO-ASSERT` records | scheduling, deduplication, the verdict | `spawn`, exit status, output capture |
| `eventually` liveness (M59) | anchors (`start`, `heal`), deadlines in virtual time | `probe` with a deadline |
| History recording `checks:` (M87) | declares checks, runs `final:` read-backs, checks on the host as today | `spawn`; `tail` of the history file (or console records as today) |
| Fake services and `H=` (M89, M90) | rule windows; at a cut, the world is carried as commands into the recovery VM | `spawn` `hydo-world`; its `ctl` via `spawn`; `read` of the world file |
| Error-log scan `errors:` (M94) | reads the streamed logs; signatures already host-side | `tail` |
| Composer `W=` rounds (M86, M98) | the composition (host already computes it), each command's start at its round time, drain, kill | `spawn` with `HYDO_SEED`, `HYDO_CMD_ID`, `HYDO_ROUND`; `exit` events |
| `network: bridge` (M95, M99) | the namespace plan | run pinned `ip`; `spawn` inside a namespace |
| Power cut and recovery boot (`C=`, `cf=`) | freeze all services (no `sync`), then a second conversation with the recovery VM's agent: acknowledged ids, failed ids, world, clock | `signal` `STOP` to every tree; nothing else changes |
| Stack-only tokens `Q=`, `X=` (the hand-written stack image) | unchanged until the stack image is rebuilt on the agent | — |

## Performance

Measured costs (docs/PERF-RUN-COST.md): 1.0–1.5 µs of host time per VM exit; a stack run
19.7 s on average, a Redis run 2.5–5.7 s, of which the guest is 0.13–0.16 s for Redis. Against
that:

- A command of 100 bytes costs about 100 port reads (one exit each), well under 1 ms of host
  time; a run sends tens of commands. The agent replaces busybox processes per primitive
  (`svc_tree` alone runs `cat` and `awk` over `/proc` for every signal), so guest phases should
  get shorter, not longer. This is an estimate until step 6 below measures it.
- Exact delivery on hvx costs single-event run calls only near a target (at most 2,048 of them,
  each a short ioctl).
- Phase 1 observation needs stops at a fixed cadence. Each stop copies the 64 KiB console
  (`HVX_GET_CONSOLE`); at `rb=16` the Redis workload makes a few hundred stops, which is
  negligible, but a dense cadence on the stack's 3.9 million-exit workload is not. Phase 2's
  host-call port removes both the cadence and the copies.
- Several vCPUs (M66, M69): the UART interrupt goes to vCPU 0; the agent may run on any vCPU;
  the engine stops only at clean turn boundaries and reports the shared clock, so delivery points
  stay deterministic. Each extra stop costs a turn end (about 0.5–0.8 ms, M74/M85), so the
  cadence matters more there.
- The Mac has no exact instruction counter (M48): its clock is the exit count, commands land
  exactly at an exit, and stretches of guest work without exits cannot be interrupted (open item
  13). Process-level control does not need more.

## Feasibility experiments (code in `hv/guest-control-exp/`)

Both experiments type a line (`hydo-agent-ping N`) into the serial input of an unchanged
onboarded Redis image while its workload runs (`disk=none | run K=redis@5+2`), with the engines'
sources unchanged: on hvx `gc_hvx.c` compiles `hvx_explore.c` with every `ioctl` routed through a
small interposer, and on the Mac `build.sh` makes a copy of `serve.c` with two one-line
substitutions that call a hook. Nothing reads the line during the workload; the guest's tty
echoes it to the console at the point it arrives (and the init's later `read` consumes it), so
the fingerprint shows exactly when and whether it arrived. Each configuration ran as a serve
process with the same schedule twice (restored from S1) and as `one` (a fresh boot).

**hvx on the bare-metal machine** (main module `0799F1B8EE2A35600D94EB4`, image `explore-ob-redis-nq-disk.img`,
PMU clock, one VM at a time through `~/bin/hvx-run`; results in `/var/tmp/m116/b2`–`b4` on the bare-metal machine).
The workload runs from icount 949,127 (S1) for about 46,000–98,000 icount. "a", "b" = serve,
restored from S1; "one" = fresh boot.

| configuration | a | b | one |
|---|---|---|---|
| plain `hvx_explore` (control) | `fd4b848eb8969419` | same | same |
| the interposer with nothing to type (inert) | `fd4b848eb8969419` | same | same |
| typed at icount +20,000 | `146dec7ffae541c1` | same | same |
| typed at +20,001 | `46a292f1a33788cc` | same | same |
| other text at +20,000 | `50a11f2b77861b05` | same | same |
| at +60,000 (landed at +62,019, the first stop at or after it: a tick adds 2,048) | `023bcafa7233eaf2` | same | same |
| two lines, +20,000 and +60,000 | `2309a3b53b2db7d0` | same | same |
| reaction to `KILLED redis`, plain run calls | `7258219e8a25e337` | same | same |
| the same reaction with `rb=16` | `b83bf67d682b4609` | same | same |
| reaction +5,000 with `rb=16` (landed at +17,013) | `f6d7670d74596283` | same | same |
| `rb=16`, nothing typed / `rb=1`, nothing typed | `a6e72fcdff5f17ed` / `93d199cae9ab642d` | same | same |

Branches (`BRANCH br 2 0 1 <point>`: a base run to the point, a snapshot there, two
continuations from the snapshot with zero jitter): typed at +60,000 after a branch point at tick 3
(+11,569): base, both continuations, the plain `rb=16` run and the fresh boot all
`456001c6ec2263cb`; typed at +5,000, before the branch point: all `8f87ca0fcdddf60a`. The snapshot
rule: with the branch point and the reaction at the same stop (marker `KILLED`, +11,825), the base
run types the line just after taking the snapshot. If the engine treats a command at the
snapshot's own icount as already contained in it, both continuations give `a6e72fcdff5f17ed`, the
fingerprint of the run with nothing typed: the line is lost. If it types it again after the
restore, both give the base's `b83bf67d682b4609`.

**hvm on this Mac** (M2 Pro, `explore-ob-redis-oci-arm64`, load average about 125 from other
sessions during the runs; scratchpad results): the workload starts at exit 19,899.

| configuration | a | b | one |
|---|---|---|---|
| nothing typed | `6b9321d4b13e5d12` | same | same |
| typed at exit +500 / +1,500 / +3,000 | `dae633ebb392b3d8` / `7b01d5c261a5334a` / `ec004616904a49f0` | same | same |
| other text at +1,500 | `5406af0d7002e705` | same | same |
| at +1,501 | `7b01d5c261a5334a` (= +1,500) | same | same |
| two lines, +500 and +3,000 | `d36401c377728af4` | same | same |
| reaction to `KILLED redis` (seen at +3,542), at once / +200 exits | `c32331d43a6b3f37` / `762b3637033677ed` | same | same |

What this shows:

1. A command typed on the serial input at an exact point of virtual time is reproducible on both
   engines: twice from a restored S1, from a fresh boot, and (hvx) from a mid-run snapshot, in
   every configuration. Different times and different bytes give different fingerprints, so the
   input really reaches the guest at that point. On hvx one event matters (+20,000 and +20,001
   differ); on the Mac two times one exit apart can give the same state, because the guest takes
   the interrupt only at its next exit with interrupts unmasked (M48).
2. The interposer typing nothing gives the control's fingerprint, so converting the run calls to
   capped `HVX_RUNC` changes nothing by itself.
3. The stop cadence is part of a run's definition today: `rb=16`, `rb=1` and plain run calls give
   three different fingerprints with nothing typed, because the host types `go` and takes the
   final fingerprint at "the first stop after a marker". With plain run calls (a return every
   2^16 retired exits) the host saw the `KILLED redis` line only at the end of the Redis workload
   (+89,352 instead of +11,841), so a reaction came far too late. The design therefore records
   every host action by its own icount/exit count, and the engines in control mode stop at every
   event near the points that matter (exact delivery) and observe with a fixed small cadence.
   Measured cost on this workload: `rb=1` runs took 2.6 s against 2.5 s for `rb=16` and plain
   calls (the engine's own `secs`, which also contains about 2.4 s of fingerprinting); against a
   Redis guest phase of about 0.13–0.16 s (PERF-RUN-COST.md) this may double the guest phase, so
   it must be measured on the stack before it becomes a default.
4. Snapshots must carry the host input that is not yet in the guest. The engine records what it
   typed and at which icount; a restore types again every command at or after the snapshot's
   icount that the snapshot cannot contain. (The general form: the receive queue is host state;
   either the module saves it with the snapshot, or the engine snapshots only when the agent has
   acknowledged every command, or the engine re-types what the guest had not acknowledged.)
5. A stray line on a tty that someone else reads is taken as that reader's input. With a
   newline, the line typed mid-run was echoed and then read by the init's next `read` in place
   of `go`; the guest ran on to its verdict without waiting, and on hvx (which looks only at
   stops) the run then hung until it was stopped by hand: as far as the console shows, the
   guest printed its final marker before the stop at which the host typed `go` and started
   looking for it, so the host looked only after it. On the Mac
   (which looks at every exit) the same run finished. The hvx batch was repeated with the line
   sent without its newline (only echoed). The agent must own its channel exclusively.

## Migration

1. **Side by side.** Images built with the agent carry `/etc/hydo/control` = `agent <proto>`;
   images without it are shell-init images. Engines and the explorer pick the protocol from the
   image, so every existing image, schedule and recorded fingerprint keeps working, and both
   kinds can be explored by the same `hydo`. (M121: the host reads the file,
   `hydo_engine::image::control`, and asks the engine for its controlled boot, `--control boot`;
   the C engines do not read the image themselves.)
2. **Verdict equivalence before fingerprint baselines.** An agent image cannot have the shell
   image's fingerprints (different `/sbin/init`, different guest execution). The proof is at the
   level of meaning: on every example manifest (42) and both architectures, the same verdict
   class and the same violated property for the explorer's schedules (seeds 1–100 per example,
   the sweeps, the pinned reproducers' schedules: the old order service still violates order 11,
   the fixed one passes), and the same per-seed detections on the bug benchmark (`hydo-bench`).
   Differences are investigated one by one; a difference caused by timing alone is recorded, not
   forced away.
3. **Re-baseline once.** When the agent is the default, every example's fingerprints are
   recorded again in one place (`docs/REBASELINE.md`, a new section), with two runs each, as M61
   did for the pinned toolchain. The engine's own baselines (`deploy-metal.sh`: boots,
   hand-written tests) are not affected, because no module change is needed for Phase 1.
4. **Retire the shell path.** `hydo-build` stops splicing (`init.rs` goes) once no supported
   release builds shell-init images; the shell files stay in git history. Old images keep running
   for as many releases as the release notes promise, because the engines keep the old protocol.
5. **Phase 2 port** after the agent is the default, on a side module first; agent images learn to
   use it through a hello feature, so Phase 1 images keep working.

What can be done incrementally: the protocol crate, the agent and its native tests need no VM;
the engines' lock-step control protocol is inert for schedules that do not use it (provable by
the existing corpora and the pinned reproducers); the controller can first drive a "minimal"
manifest (services, steps, kills) and grow feature by feature, each feature with its own
equivalence proof.

Risks:

- **Timing shifts change which bugs a schedule finds.** A schedule's verdict depends on guest
  timing (M71 moved Redis `C=12 R=everysec` from `k:4` to `k:2` by making the init longer). The
  benchmark comparison measures this; a strategy's detection rate, not single verdicts, is the
  criterion.
- **Reactive decisions make shrinking harder.** A shrunk schedule may no longer trigger a
  reaction. Shrinking therefore works on the resolved, anchored actions of the failing run, as
  the composer's resolved `W=:` form does (M98).
- **The console stays shared in Phase 1.** Framing makes corruption visible but not impossible;
  `quiet` reduces it. Phase 2 removes it.
- **A bigger guest program to keep deterministic.** The agent must not use threads, timers or
  randomness of its own; its tests check that two runs over a transcript give byte-identical
  output, and its binary is pinned by sha256 like `hvx_seedrng`.
- **Version skew in saved artefacts.** Saved regressions name an image; the image names its
  agent protocol; the host keeps every protocol version. A release note must say when a protocol
  version stops being supported.

## Implementation plan

Steps that can run in parallel are marked with the same letter. Each step lists its proof.

- **A1. `hydo-agent-proto` crate** (types, canonical JSON, schema generation, the transcript
  test harness). Proof: the schema test, round-trip of every type, transcript files parse.
- **A2. `hydo-agent` (Rust, static musl x86_64 and aarch64, std + libc).** PID 1 duties and the
  primitives above, except the packet trigger. Proof: native tests on Linux (the agent runs as a
  normal process against a pipe in a scratch root; process trees, signals, probes with
  deadlines, files), two runs over the same transcript give byte-identical output, the binary
  builds byte-identically twice (pinned toolchain, M82's recipe).
- **A3. Packet trigger in the agent** (port of `hydo_fc.c`). Proof: the M70/M76 freeze points
  of the Redis freeze example give the same FROZE lines as `hydo_fc` (verdict
  equivalence; the fingerprints differ).
- **A4. Engine lock-step control, hvx** (`hvx_explore.c` or the in-process engine: `EVENT`,
  `STOP`, `SEND at=`, `SNAP`, `CONT`; exact delivery by capped `HVX_RUNC` as in `gc_hvx.c`; the
  snapshot-only-when-acknowledged rule). Proof: inert when unused (the explorer's identity
  corpora, `deploy-metal.sh` ALL PASS, both pinned reproducers), and the experiments of this note
  as permanent tests.
- **A5. Engine lock-step control, hvm** (the same over `serve.c`'s stop hook). Proof: the Mac
  baselines (M101/M107 quickstart values) unchanged, the experiment as a test.
- **B1. `hydo-control` crate: `Controller`, `ScheduleController`** for the core manifest
  (services, readiness, provisioning, steps, kills, pauses, knobs, invariants, the verdict).
  Proof: a scripted-agent test (like `fake_engine.rs`) that checks the command sequence for every
  schedule of the explorer's identity corpus on the core manifests.
- **B2. Feature controllers, one per former extension** (liveness, clock, history, freeze,
  BUGGIFY, fakes and the carried world, error logs, network namespaces, composer rounds). Can be
  split among agents. Proof per feature: its example's verdicts on agent images equal the
  shell images' (both architectures where the feature exists), plus the scripted-agent test.
- **B3. `hydo-build --control agent`** (stages the agent and `/etc/hydo/control`; no `init.rs`
  splicing for such images). Proof: byte-identical images in two builds; shell images unchanged
  (the existing 15 + 11 image hashes). *Done in M121, with the controlled boot phase both engines
  need for such an image (`--control boot`: lock-step control from the first instruction, S0 and
  S1 taken by the host with `SNAP S0`/`SNAP S1`; `Engine::start_controlled`/`one_controlled`);
  docs/HISTORY.md "Milestone 121".*
- **C1. `hydo-engine` `run_controlled`/`resume` and the explorer's strategies on controllers;
  `ctl=` in RESULT; `control.jsonl` in runs; `hydo repro --transcript`.** Proof: the 18
  scripted-engine scenarios unchanged for shell images; transcript replay equals policy replay on
  every example. *Done in M126 (the engines' `DISKOFF`/`CUT`, `Worker::run_controlled`/`resume`
  with a `Driver`, `hydo_control::agent::AgentEngine`; proven on the Redis example on hvx and hvm;
  the other examples are C2's); docs/HISTORY.md "Milestone 126".*
- **C2. Equivalence campaign** (after A–C1): every example, both architectures, seeds 1–100,
  the sweeps, the benchmark cells of M108. Proof: the verdict tables, every difference explained.
- **C3. Re-baseline and switch the default; then retire the splicing.** Proof: REBASELINE.md
  section with two runs per value; releases rebuilt and their quickstarts run. *Done in M136,
  except the release rebuild (the user's decision, at the end of the project's work): every
  image is an agent image, `init.rs`, the shell init and its extensions and the guest composer
  are deleted, `services[].restart_may_fail` replaces the benchmark's wrapper, and
  docs/REBASELINE.md section 8 has the new values (every recorded schedule twice on both
  engines); docs/HISTORY.md "Milestone 136".*
- **D1 (later, independent). Host-call port** on a side module and in hvm. Proof: inert when
  off (full suite, pinned reproducers), exact observation points (a record's stop equals the
  instruction that wrote it, checked against the PMU branch count), branch equality with a
  staged answer pending at the snapshot.
- **D2 (later). Reactive strategies** (fault on assertion, heal on no progress, branch at a
  reaction) measured on `hydo-bench` against the process strategy.

## Sources

FoundationDB: SIGMOD'21 paper §4 (https://www.foundationdb.org/files/fdb-paper.pdf),
testing (https://apple.github.io/foundationdb/testing.html), determinism checks by "unseed"
(https://github.com/apple/foundationdb/wiki/Debugging-in-FDB). TigerBeetle VOPR: liveness mode
and no-progress budgets (https://github.com/tigerbeetle/tigerbeetle/blob/main/src/vopr.zig,
https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md), simulator false
positives (https://tigerbeetle.com/newsletters/2025-03-05-february-in-tigerland). Jepsen: control
node and nemeses (https://github.com/jepsen-io/jepsen), pure generators with explicit time
(https://jepsen-io.github.io/jepsen/jepsen.generator.html), `:info` outcomes
(https://github.com/jepsen-io/jepsen/blob/main/doc/tutorial/03-client.md), Elle
(https://arxiv.org/abs/2003.10554). madsim: determinism check
(https://github.com/madsim-rs/madsim, `madsim/src/sim/runtime/builder.rs`); turmoil
(https://docs.rs/turmoil/latest/turmoil/); S2's experience (https://s2.dev/blog/dst). Shadow:
shim and shared-memory IPC, time only advancing in syscalls
(https://shadow.github.io/docs/guide/design_2x.html,
https://shadow.github.io/docs/guide/limitations.html). rr: branch counter plus registers, late
overflow interrupts, SMM counting (https://arxiv.org/abs/1705.05937). QEMU record/replay: icount,
input events logged with instruction counts, versioned logs, fragility
(https://www.qemu.org/docs/master/devel/replay.html,
https://www.qemu.org/docs/master/system/replay.html). qemu-ga `guest-sync`/`guest-info`
(https://www.qemu.org/docs/master/interop/qemu-ga-ref.html). Firecracker vsock and its reset at
snapshot (https://github.com/firecracker-microvm/firecracker/blob/main/docs/vsock.md,
https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md).
Kata 1.x/2.x agent skew
(https://lists.katacontainers.io/archives/list/kata-dev@lists.katacontainers.io/message/PRVGNQEK7NBKW7Z2XRCTJTTJFMDUMKZA).
Not verified from a primary source: what Will Wilson's 2014 talk says, and anything Kyle
Kingsbury said about the control node's clock.
