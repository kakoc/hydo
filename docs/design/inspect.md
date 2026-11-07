# Why a run failed, and looking inside a run (Milestone 177)

The users of these tools are AI agents that drive the `hydo` command (the onboarding skill, an
agent fixing a project's `hydo/` folder), and people second. Three onboardings (Medusa, M174;
TigerBeetle, M176; libSQL, M173) lost most of their time the same way: a run ended with
`ERROR service start (medusa)` or `ERROR provision failed` and nothing in the run directory said
why, and the only way to look at a run's state was to change the image, which changes its timing
(on Medusa the failure then stopped happening). hydo's runs replay exactly, so hydo can do what a
non-deterministic tool cannot: bring a failed run back to any of its stops and look there.

## Decisions

1. **Nothing that is recorded moves.** The diagnosis is computed after `hydo run` has written its
   result, transcripts and consoles, and what is read in the guest is read by a *separate* replay
   of the run on a fresh engine. No controller, engine or agent changed; no transcript, `ctl=` or
   fingerprint can move. `hydo inspect` only reads the recorded run's directory; its engine writes
   into `$HYDO_HOME/inspect/` (the one exception: `hydo run` itself adds `agent-events.jsonl` to the
   run directory it is making, below).
2. **Replay, then take the stop.** A run on an agent image is a pure function of its image, its
   schedule and the host's commands, which the control transcripts record by stop. A fresh worker
   replays the boot, run and recovery conversations with the recorded transcripts
   (`ReplayController`, which refuses the first stop whose time or agent-line count differs), and at
   the chosen stop a wrapper (`Halt`) takes the stop *before* the recorded commands of that stop are
   typed. Everything the engines' lock-step protocol offers is enough: no protocol addition.
3. **Snapshot where the engines can, a live branch where they cannot.** At the stop the engine takes
   a snapshot (`SNAP`, with the commands still in the serial queue retyped on restore), and every
   call is a conversation resumed from it (`RESUME`): each call sees the same state, and a call
   costs a restore and a few stops. The engines snapshot only in the boot and the workload phase
   (not after the workload's end, not in a recovery boot, not under the timing tokens `tj`, `xt`,
   `pp`, `q`, `cf`, `pct`); at such a stop the session is *live*: the conversation stays at the stop
   and calls run one after another on one branch. Every answer says which (`state`).
4. **A session is a background process.** `hydo inspect start` starts `hydo inspect __serve <id>`
   detached; it replays once and holds the stop (the worker, or the live conversation) and serves
   one JSON request per connection on a Unix socket in `$HYDO_HOME/inspect/<id>/`. It ends by
   `stop` or after `--idle` seconds without a call (default 900). A call with a run directory instead
   of a session id is one-shot: replay, the call on the live branch, end (no session, a replay per
   call). On hvx a session holds the module lock shared, like a run.
5. **Calls are agent commands the agent already has.** A command runs with `run` (its stdout and
   stderr redirected into `/tmp/hydo-inspect/`, then read back with `read` in pieces of 3,000 bytes up
   to a byte limit), files are read with `read` (as hex when they are not UTF-8), `ls`, `ps` and
   `sockets` are small busybox scripts parsed on the host (`stat -c`, `/proc/<pid>/stat`,
   `/proc/net/{tcp,udp}{,6}`). `--service` runs in that service's network namespace
   (`network: bridge`). Each call has a guest-time limit (`--timeout`, default 10 s) and a stop limit.
6. **One JSON document per call, results format version 1** (additions only, checked by the schema
   test): `inspect` (`op` says which members are set), `inspect-error` (`code`, `message`, `try`: the
   exact commands that would work), `timeline`, `diagnosis`, and `Run.diagnosis` in `result.json`.
   Every list is bounded (`--limit`, `--max-bytes`, `truncated`, `next` for the following page);
   `--text` prints a view for people instead. The JSON is meant to be a thin MCP tool layer: one tool
   per op with the same arguments, the document as its result.
7. **The diagnosis has two layers.** The host layer reads only the run's own files: the verdict, the
   transcripts (which command the host typed for which service), and the agent's own events from the
   engine's consoles (each process's exit status or signal, each readiness check's end, each command's
   end, refusals), placed at stops by the transcripts' agent-line counts, plus the kernel's lines
   about memory and crashes. It names the failing service, process, signal and probe, and draws
   conclusions with their reason (a SIGKILL hydo did not send is the out-of-memory killer; 127 is a
   program not found). The evidence layer is one replay to the verdict's stop that reads, in the
   guest, every service's log tail, the setup and provisioning logs, the readiness check run once,
   `dmesg` (the memory and crash lines, then its tail), `free` and `ps`. `hydo run` collects the
   evidence for ERROR verdicts (`--no-diagnose` skips it); `hydo inspect diagnose --evidence` collects
   it for any run.
8. **hvx's host-call port.** On hvx the agent's lines travel over the host-call port (M138), so the
   consoles have none of them. The evidence replay records the agent events it sees before the stop,
   rebuilds the host layer from them, and `hydo run` keeps them in `agent-events.jsonl` in the run
   directory it is making, which `hydo inspect events` reads.

## The timeline

`hydo inspect events <run dir>` lists the run's events, each with `point` (`boot:172`, the form
`--at` takes), stop, guest time, `kind` (`command`, `exit`, `probe`, `done`, `refused`, `fault`,
`note`, `report`, `assertion`, `guest`, `kernel`, `snapshot`, `engine`, `end`, `verdict`), text,
service and the record behind it; filters `--kind`, `--grep`, `--conversation`, `--from`/`--to`;
pages `--limit`/`--offset` with `next`. It reuses `hydo diff`'s reading of a run (M165) and adds the
agent's events. It also lists each conversation's stops and where snapshots are possible.

## Shapes (abridged)

```json
{"v":1,"type":"inspect","op":"exec",
 "at":{"point":"run:812","conversation":"run","stop":812,"at":1043200},
 "exec":{"argv":["redis-cli","GET","k:11"],"status":0,"timed_out":false,
         "stdout":{"text":"v11\n","bytes":4,"truncated":false},"stderr":{"bytes":0,"truncated":false}},
 "cost":{"wall_ms":412,"stops":9}}
{"v":1,"type":"inspect-error","op":"logs","code":"usage",
 "message":"no service \"db\" in this image (its services: redis)",
 "try":["hydo inspect logs <session> redis"]}
```

## What each engine supports

The same code drives every engine through the lock-step protocol. hvx (the bare-metal machine):
the diagnosis with its evidence for a service that exits at start, a readiness check that never
passes and an out-of-memory kill in provisioning; snapshot sessions in the workload, live sessions
in a recovery boot, one-shot calls; the stack's pinned reproducer. hvm (the Mac): the evidence of
the same three failures, a snapshot session before a power cut and a live one in the recovery
boot. KVM serves the same `SNAP`/`RESUME` (M161) and was not run with `hydo inspect`. Stops that
cannot be snapshotted are live on every engine.

## Costs

`start` costs one replay up to the stop (the run's own time to get there, plus a worker boot);
each call in a snapshot session costs a restore (milliseconds) and the stops the agent needs to
answer (a few to a few dozen); a one-shot call costs a replay. Every answer carries `cost`
(`wall_ms`, `stops`).

## Left for later

A mid-run snapshot of the check phase and the recovery boot (an engine change); the data disk kept
with a run so a finished run's disk can be mounted without a replay; an MCP server; reading the
consoles' guest lines of the moment in the live state; KVM runs of `hydo inspect`.
