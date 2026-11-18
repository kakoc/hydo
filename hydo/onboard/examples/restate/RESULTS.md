# Restate under hydo: durable execution across crashes, power cuts and clock steps

Restate (github.com/restatedev/restate) is a durable execution engine in Rust. A Restate server
sits in front of a service, invokes its handlers, and keeps a journal of each invocation: every
step's result, every timer, every call. After a crash it replays the journal, so that a step
whose result was journaled does not run again. This example runs one Restate server from its
official image and one deployment of the example's own service on Restate's Rust SDK. It hunts
for bugs that nobody reported in what Restate's documentation promises.

## The short answers

- **What was found?** One new candidate, not posted upstream:
  1. **After a backward clock step, a restart of the server holds accepted invocations until the
     wall clock reaches the time they were created.** Restate's invocation scheduler starts an
     invocation only when its `run_at` is not later than the wall clock, and an invocation
     without a delay gets its record's creation time as `run_at`. A restart puts every
     invocation that was in flight back into the scheduler with that old time. After a step back
     of 5 minutes, a plain one-way send that Restate had acknowledged did not start for the
     whole 250 s that the checks wait (`regressions/held-after-a-clock-step-and-a-restart.schedule`).
     The same restart without the step, and the same step without the restart, pass.
- **What held?** Restate kept what it promises in every other run. No acknowledged invocation was
  lost, and no handler completed twice. No journaled step ran again, and an idempotency key gave
  one invocation. No two adds were inside one virtual object key at once, and its state had each
  acknowledged add once. Awakeables and workflow promises kept their first resolution. That held
  under kills of the server and of the deployment, pauses, network faults, and power cuts. The
  cuts came at steps, at the client's and the service's events, and at 49 disk flushes on hvx.
  Damaged files that Restate detects stop it at the start. A truncated RocksDB log loses the
  writes after the cut silently, as a damaged disk would. Every failure with disk I/O errors or a
  lying disk was the disk's loss.
- **What is documented?** Durable timers follow the wall clock (section "What was found",
  "Documented"): after a step back, a sleep that the timer service had armed ends on time by
  real time, and every later one waits for the wall clock. A delayed send waits the same way.
- **What did hydo lack?** Section "What hydo lacked". One gap is fixed here: the explorer read an
  image's manifest without its parameter values, so `guest.cpu: "{{cpu}}"` stopped it.

## The version

Restate v1.8.1-hotfix.2 (2026-11-02) was the newest published release when the onboarding
started (2026-11-17); GitHub's "Latest" flag names v1.8.1 (2026-10-21), and the hotfix adds
dependency updates and an escape hatch for orphaned queue markers to it. The server runs from
the image `docker.restate.dev/restatedev/restate:1.8.1-hotfix.2`: the index digest is
`sha256:e3a11e7c…`, amd64 `sha256:e239ab0c…`, arm64 `sha256:727dc7f0…`. The sources were read
at the tag `v1.8.1-hotfix.2` (commit `34918c22`).

The service uses `restate-sdk` 0.13.0 (2026-10-28), the newest release of the Rust SDK. It is
one static musl program, `rsv`, built for arm64 and x86_64 without a C toolchain (`build.sh`).

## The promises

Each rule of the section "The rules" states one of these, in the documentation's words:

- Invocations (Foundations, "Key concepts"): "Restate tracks each invocation through
  completion, ensuring it runs exactly once regardless of failures." A journaled step "won't
  repeat on retry".
- Idempotency keys (the same page): "If you add an idempotency key to your request headers,
  Restate will automatically ensure that requests are deduplicated. Duplicate requests will
  return the same result as the original request."
- Durability (Server, "Overview"): a single node "fsyncs committed data to disk before
  acknowledging network messages, providing the highest level of durability". "If Restate is
  deployed as a single node, it can still recover from crashes by recovering from persistent
  disk."
- Virtual objects (the same page as the first): "Single-writer guarantee: Only one handler can
  modify state at a time", and state "is never out of sync with the execution".
- Durable timers (Develop, "Scheduling & Timers"): a durable sleep and a delayed message wake
  the handler at the wake-up time. The page asks to keep the server's and the SDK's clocks in
  sync: "A mismatch can cause timers to fire earlier or later than expected."
- Workflows (Foundations, "Services"): "The `run` handler executes exactly once per workflow ID".
  An awakeable and a workflow's durable promise are resolved once: the first resolution is the
  value the handler gets.

## The project

`docker-compose.yaml` starts the server as the documentation does with Docker: the data in
`/restate-data` on a volume, ingress on 8080, the admin API on 9070, no telemetry, and 256 MiB
for RocksDB. `hydo compose` imported it. The changes from the import:

- The server's data directory is a file system of its own (`storage:`, the target of `G=` and
  `L=`), its readiness check asks both health endpoints, and its log has no color codes.
- A second service, `svc`, is the deployment: `rsv serve`, Restate's Rust SDK on port 9080.
  Its `depends_on: [restate]` gives the explorer the link between the two.
- The guest CPU is a build parameter, `cpu`: Restate's x86_64 build needs AVX2 (`v3`), and an
  arm64 build takes `--set cpu=v2`.
- One error kind is accepted (`known-errors.yaml`): the server's check for a newer release on
  GitHub at start, which fails without a network.

The deployment serves four handlers. Each journaled step (`ctx.run`) appends one line to an
effects file on the data disk and syncs it: the outside system that a step changes.

| handler | what it does |
|---|---|
| `Job/<key>/run` (virtual object) | 1 to 3 journaled steps, each an effect; a durable sleep of 3 s between two steps for the sleep jobs; then the state `runs` (+1) and `out` (the invocation id, each step's nonce and times) |
| `Acct/<key>/add` (virtual object) | an effect `in`, a durable sleep of 20 ms, the balance and the map of applied ops in the state, an effect `out` |
| `Wait/<key>/run` (virtual object) | an awakeable, its id published as an effect, the value it gets stored in the state |
| `Flow/<key>/run` (workflow) | waits for the durable promise `p` and stores its value; `signal` resolves it |

A step's closure appends `beg` when it starts and `job` with its result, both with one nonce. A
`beg` of step k means that step k-1's result was journaled, because the SDK waits for the
server's acknowledgement of a step before it goes on.

## The workload

`hydo/workload/` has one `first_` command, which registers the deployment, and seven drivers
that run 4 at a time in 40 steps (`test:`), about 110 operations a run:

| command | operation |
|---|---|
| `parallel_driver_call` | a request-response call of a job with 3 steps, with the idempotency key `j<cmd>`, tried up to 3 times |
| `parallel_driver_send` | a one-way send of a job with 2 steps, with an idempotency key |
| `parallel_driver_delay` | a one-way send of a job with 1 step and a delay of 2 s |
| `parallel_driver_sleep` | a one-way send of a job with a durable sleep of 3 s |
| `parallel_driver_add` | an add to one of 3 accounts, with the idempotency key `o<cmd>` |
| `parallel_driver_awk` | starts a waiter, resolves its awakeable with `v1`, then with `v2` |
| `parallel_driver_flow` | submits a workflow, signals its promise with `v1`, then `v2`, then submits it again |

An answer `200` (a call) or `202` (a send) is acknowledged: the client writes the operation to
`$HYDO_ACKED` and `ack` to `$HYDO_OUTCOMES`. Any other answer is failed, and no answer is
unknown. The calls and sends are also a messages history (`HYDO-HIST` records). Before a check
judges, it waits up to 100 s for every acknowledged operation's result.

## The rules

| rule | promise | where |
|---|---|---|
| `delivery` (a `type: messages` check) | every acknowledged call and send of a job completes (`no-loss`), and its handler completes once (`duplicates: forbidden`) | `hydo.yaml`, the drain `rsv drain` |
| `jobs` | every acknowledged job completed once (`runs` = 1) with all its steps; one invocation per idempotency key; no step's closure starts after a later step's started; no step runs again after its result was journaled | `checks/final_jobs` |
| `timers` | a sleep ends, and a delayed send runs, at or after its time, by the wall clock or, in one boot, by the boot's clock | `checks/final_timers` |
| `objects` | no add enters an account while another is inside; each acknowledged add is in the state once, and the balance is the sum of the applied adds | `checks/final_objects` |
| `signals` | an awakeable and a workflow promise keep their first acknowledged resolution, stored once, by one invocation | `checks/final_signals` |

Two cases are not violations, and the checks print them on a line of their own:

- An acknowledged job that waits for a timer (status `scheduled`, `suspended` or `backing-off`)
  whose last change, or whose scheduler entry's `next_at`, is later than the wall clock now: the
  clock was stepped back, and Restate's timers follow the wall clock, as its documentation says.
  A job that is `ready` waits for no timer, and it counts.
- A closure that ends in an attempt that Restate gave up on (a zombie) after the step's result
  of another attempt was journaled, when the zombie's closure had started before: that is the
  at-least-once of a step that was not journaled yet.

## Without faults, and the regressions

`disk=none | run` passes with one fingerprint per image: on the Mac engine (2048 MiB, arm64)
`5197fd2b05fab330` twice on the first image, and on hvx (x86_64, `guest.cpu: v3`)
`3be8abae33f86551` twice. A run takes about 6 s of engine time on the Mac and 25 s of wall time
on hvx. Each change of the client changed the image, so later images give other fingerprints.
The final arm64 image (`sha256(explore-ob-restate-arm64.cpio.gz)=f0b30e75…`) gives
`88849d3d1b7d25bd` twice.

The regression `hydo/regressions/held-after-a-clock-step-and-a-restart.schedule` is finding 1's
minimal schedule; it expects a violation. `hydo repro --regressions` on the final image gives it
(`PASS ... expect violation`, 5 jobs never completed). On the image built from `main` at
`1442a6d5` (the fingerprint epoch 4) `hydo run` of its schedule gives `fp=8b211024ba849447` on
every run; the images before gave other values. hydo warns that
its `W=` is in the seed form, so the composition comes from the current command list; a change
of the commands changes the run.

## What was found

### 1. New: after a backward clock step, a restart holds accepted invocations until the wall clock reaches their creation time

**Label: a bug of the project, new as far as the search found (a clock effect that breaks a
promise).**

Blind, seed 14 of exploration A failed `jobs` and `delivery.no-loss` on the Mac engine. The
explorer shrank it to

```text
disk=none dseed=d584008bda670834 | run K=restate@1+1 N=restate-svc@12-14:loss,85% P=svc@28+2 T=30:-300 W=726d723b,p1,q4,r600 L=restate@30+1
```

and the schedule gives the same failure on hvx. Three of its faults are needed (the table below):
the deployment is paused for steps 28 and 29, the wall clock goes back 5 minutes before step 30,
and the server loses its power for one step:

```text
disk=none dseed=d584008bda670834 | run P=svc@28+2 T=30:-300 W=726d723b,p1,q4,r600 L=restate@30+1
```

The one-way send `j65` was acknowledged (`202`) at step 29, while the deployment was paused.
Its invocation is `ready` with only its input in the journal, and it never starts in the 150 s
of the drain and the 100 s of the checks:

```text
j64: now 1794873671765 sys_invocation: 200 {"rows":[{"id":"inv_18ffM7t23ed838zEkfpJRxYV9erFTyItry","status":"ready","journal_size":1,"created_at":"2026-11-17T00:01:38.455Z",...
j65 (send): acknowledged, never completed (0 effects)
j65 was acknowledged to process 65 at 105.430 s and never delivered in channel jobs, which was drained
```

The invocations' `created_at` is later than the wall clock `now`: the clock went back after
they were created. Two more jobs of the run (`j64`, `j66`) have a sleep in their handler but
never started either, so no timer of theirs was armed. On hvx the scheduler's own table shows
why, 274 s after the step back:

```text
j65: now 1794873592877 sys_vqueue_entry_status: 200 {"rows":[{"stage":"inbox","status":"yielded","next_at":"2026-11-17T00:00:19Z","created_at":"2026-11-17T00:00:19.779Z","first_runnable_at":"2026-11-17T00:00:19.779Z","has_lock":true}]}
```

`now` is 23:59:52.9 by the stepped clock. The send was moved back to its vqueue's inbox
(`yielded`) at the restart, it may run next at its creation time (`next_at` 00:00:19), and it
holds its virtual object's lock, so the key's later invocations wait behind it.

Directed runs on hvx separate the faults:

| schedule | result |
|---|---|
| `T=15:-300 K=restate@16+1` | 7 jobs stay `ready` (`jobs`); each is a sleep job between its two steps |
| `T=15:-300 L=restate@16+1` | the same |
| `T=15:-120 K=restate@16+1` | 6 jobs stay `ready` or `running` with a retry |
| `T=15:-30 K=restate@16+1` | `OK`: the wait is shorter than the checks' |
| `K=restate@16+1` | `OK` |
| `T=15:-300` (no restart) | `OK`; two timers wait for the wall clock (documented, below) |
| `T=15:-300 K=svc@16+1` (a kill of the deployment, not of the server) | `OK` |
| `P=svc@28+2 T=30:-300 W=726d723b,p1,q4,r600 L=restate@30+1` | the send `j65` and three sleep jobs never start (`jobs`, `delivery.no-loss`) |
| the same with `K=restate@30+1` for the power loss | the same; `j65` ran its first step before the kill and then waits |
| the same without `T=` | `OK` |
| the same without `P=` | three sleep jobs wait; the send completes |

So the failure needs the step back and a restart of the server after it. The wait has the size
of the step: 30 s fits into the checks' wait, and 120 s and 300 s do not.

The cause, at v1.8.1-hotfix.2 (`34918c22`). VQueues, the invocation scheduler of 1.8, are on for
every cluster since v1.8.0 (`release-notes/v1.8.0.md`, "VQueues Enabled by Default"):

1. A new invocation without a delay enters its vqueue's inbox with `run_at` set to its record's
   creation time, "run ASAP" (`crates/vqueues/src/lib.rs:295`, `enqueue_new`; the caller passes
   the record's `created_at`, `crates/worker/src/partition/state_machine/mod.rs:1020`).
2. A failover of the partition processor, which every restart of a single node is, moves a
   running invocation back to the inbox and keeps its key, so `run_at` stays the creation time
   (`yield_entry`, `crates/vqueues/src/lib.rs:779` and `:811`; called from
   `crates/worker/src/partition/state_machine/mod.rs:5428`). Rescheduling an invocation without a
   new `run_at` takes the creation time too, with the comment "always eligible"
   (`crates/worker/src/partition/state_machine/mod.rs:5481`).
3. The scheduler takes an inbox head only when `run_at` is not later than its clock
   (`crates/vqueues/src/scheduler/vqueue_state.rs:357`), and that clock is the wall clock
   (`SchedulerClock`, `crates/vqueues/src/scheduler/clock.rs:21`). Otherwise the head is
   `Scheduled` and parked until then (`crates/vqueues/src/scheduler/eligible.rs:191`).
4. After a step back of X, the creation times of the invocations from before the step are up to
   X later than the wall clock. Each one that enters the scheduler again waits up to X.

The consequence: after an NTP or an operator's correction of a clock that was ahead, a crash or
a restart of the server freezes the in-flight invocations for the size of the correction, also
those that have no timer. The documentation's note on clocks is about timers ("A mismatch can
cause timers to fire earlier or later than expected"); an invocation without a timer is said to
run "exactly once regardless of failures". A run without the restart was not shown to stall,
because new records after the step get the new time; an invocation that waits in an inbox, such
as one queued behind a virtual object's lock, was not tested.

Native reproduction: with Docker, in `native/`. The server runs from the official image
`restate:1.8.1-hotfix.2`, with an `LD_PRELOAD` shim (`native/shiftback.c`) that makes
`clock_gettime(CLOCK_REALTIME)`, `gettimeofday` and `time` read `SHIFT_BACK_SECS` seconds earlier.
The monotonic clocks are unchanged. The shim sets the clock of one process, not of the Docker
host. libfaketime made `restate-server` hang at start. `native/Dockerfile` adds the shim and the
example's `rsv` (`bin/rsv-arm64` or `bin/rsv-amd64` of `build.sh`, copied as `rsv`) to the image.
The steps, run by hand with `docker` and `curl` on a Mac (Colima, arm64):

1. Start the deployment (`rsv serve`) and the server, with the server's environment of
   `docker-compose.yaml` and a volume for its data, and register the deployment.
2. `docker pause` the deployment, so that the next invocations stay running in the invoker.
3. Send `POST /Job/k1/run/send` twice and `POST /Job/k2/run/send` once; each answers `202`.
4. `docker kill` the server, start it again on the same volume with
   `LD_PRELOAD=/usr/local/lib/shiftback.so SHIFT_BACK_SECS=300`, and `docker unpause` the
   deployment.
5. Query the admin API with `native/q.json` (`POST :9070/query`).

50 s and 3 minutes after the restart, every invocation was `ready` or `pending` in the inbox,
`yielded` with `next_at` equal to its creation time and `has_lock: true`, and the server logged
no warning. They ran exactly 300 s after their creation. The second invocation of `k1` ran right
after the first, so it waited behind the lock for the same time. The same restart without the
shim ran all three 0.25 s after the server's start. Each was run once. The regression is
`hydo/regressions/held-after-a-clock-step-and-a-restart.schedule`.

### Documented: durable timers follow the wall clock

`T=15:-300` alone gives the jobs of a sleep or a delay two behaviours. The timer service arms
the next timer with a tokio sleep for the difference between the wake-up time and the wall clock
(`crates/timer/src/service/clock.rs:29`, `TokioClock::sleep_until`). A timer that was armed
before the step ends on time by real time, which is 297 s before its wake-up time by the wall
clock: `j20: a sleep of 3000 ms ended after -297641 ms by the wall clock`. Every timer that is
armed after the step waits for the wall clock to reach its wake-up time, 300 s more. A delayed
send whose time the ingress computed (`SystemTime::now() + delay`,
`crates/ingress-http/src/handler/service_handler.rs:321`) just before a step back of 30 minutes
waits 30 minutes for its delay of 2 s (seed 13 of exploration A: `next_at` 00:00:05 while the
clock reads 23:32). The SDK
documentation says that the wake-up time is wall-clock time and asks to keep the clocks in sync,
so the `timers` rule judges a sleep by the wall clock or by the boot's clock, and the `jobs` rule
lists a job that waits for a timer due later on the wall clock without counting it as lost. The
clock audit's first place, the invoker's `next_retry_at` (`crates/invoker-impl/src/lib.rs:1251`),
is only reported: the retry itself waits in a `DelayQueue` on the monotonic clock.

## The duplicate searches

`gh search issues --include-prs` over restatedev/restate, open and closed, and its discussions,
on 2026-11-17:

| phrase | found |
|---|---|
| wall clock, system clock, NTP, clock skew, clock jump, time jump, time skew, NTP step, clock change, system time | #5739, #5156, #4516, #247, #4108 (`SchedulerClock` to the wall clock), #5155, #898: none about a step back |
| clock backwards, clock goes back, HLC backward, time went backwards | nothing |
| vqueue clock, SchedulerClock, eligibility, run_at, RoughTimestamp, enqueue_new, created_at, created_at run_at | #5017 (a panic on a large send delay), #4948, #5505, #4573, #4572, #4516 (record timestamps for the inbox order), #5552: none about a step back |
| invocation stuck ready, stuck in ready, ready but not running, inbox stuck, parked | #5638 (a workflow stuck in `ready` after a child call's completion, another cause, closed), #5174 |
| invocations not running after restart, invocations stuck after restart, not starting after restart | nothing |
| timer fires early, sleep_until, TokioClock, timer delayed, sleep longer, monotonic | #245, #244 (TokioClock's cost), #5199: none about a step |
| discussions: clock | nothing |

The cited lines are the same on `main` at `06349bf0` (2026-11-17); the comment "always
eligible" is at line 5484 there.

## The explorations

Both on the Mac engine with 3 workers, guest RAM 2,048 MiB, `--new-faults --storage-faults`, and
the reactions `--reactive fault --react-events rs.step,rs.send_acked,rs.call_acked,rs.add_acked`.

| exploration | strategy, seeds | failures by cause |
|---|---|---|
| A | process, 1-20 | 11 of 20. 1 finding 1 (seed 14). 5 damaged files that Restate detects at the start (`restart failed (its files were corrupted)`, seeds 3, 5, 6, 17, 18). 1 truncated RocksDB log of the log server that loses the writes after the cut (seed 10, shrunk to `A=rs.call_acked:kill-restate G=restate@9+1:trunc`). 3 with a truncated file and a backward step (seeds 13, 15, 16). Seed 16 fails without its `G=` and passes without its `T=`: finding 1. Seed 13 fails the same without its `G=` and its cut: a delayed send whose time the ingress computed just before the step back (documented, below). Seed 15 was not separated. 1 delayed send behind a step back of 30 days (seed 7, documented) |
| B | swarm, 21-40 | 11 of 20, each with a disk fault or a damaged file: 5 with a lying disk, 4 with disk I/O errors (restarts that fail, a storage that does not mount), 2 damaged files that Restate detects. Stopped after the 20 first runs, before the shrink |

Exploration A ran its 20 schedules in 2,220 s (32 runs an hour) and shrank 5 signatures in 339
runs; every failure reproduced. The checks wait for each acknowledged operation, up to 250 s
after the faults, so every failing run is slow.

Directed runs before and between the explorations, on the Mac engine unless named:

| schedules | result |
|---|---|
| `disk=none \| run`, 2 runs per image | `OK`, one fingerprint per image |
| `K=restate@15+2`, `K=svc@15+2`, `P=svc@15+3`, `P=restate@15+3`, `C=20`, `L=restate@15+2` | `OK` |
| `A=<event>@<step>:cut` at `rs.step` (also `+3`), `rs.send_acked`, `rs.call_acked` (`+1`), `rs.add_acked`, `rs.awk_resolved`, `rs.flow_signalled` | `OK` |
| `A=rs.step@10:kill-restate`, `A=rs.send_acked@10+1:kill-restate` | `OK` |
| hvx: `disk=cut cf=<k> \| run` at 49 flushes (51 runs) from `cf=37` to `cf=4000` (the last few after the run's last flush) | `OK` after two fixes of the rules (below) |
| hvx: `A=rs.step@10:stop-svc-90000` (the deployment stopped inside a step, past Restate's inactivity timeout) | `OK` |
| hvx: `K=svc@15+10 T=17:-300` (retries of a dead deployment across a step back) | `OK`: the invoker's retry timer is monotonic |
| `T=15:-300` | `OK` after the rules learned the documented timers (below) |

The first failures were the rules' own mistakes, each fixed before the explorations:

- An add that was in flight at a power cut has no record, so `objects` called it "never sent"
  (`cf=450`, `cf=2000`). The rule now takes the amount from the op's number.
- Two processes wrote one console line each, and a write of the text and a write of the newline
  let another line in between. The messages check read a history record with an assertion
  inside (`cf=2000`). Every line is one `write` now.
- Durable timers wait for the wall clock after a step back, as documented. `jobs` counted such a
  job as lost, and the messages check counted a delayed send as lost (seed 7). The calls and
  sends are the messages history now, and `jobs` lists a job that waits for a timer due later on
  the wall clock.
- Each of the four checks waited 100 s on its own; the first one waits now, once per boot.

## What hydo lacked

1. **The explorer read an image's manifest without its parameter values.** `guest.cpu` must be
   `v3` for Restate's x86_64 build and cannot be `v3` on arm64, so the manifest takes it from a
   parameter. `hydo build` and `hydo run` applied the image's values, and `hydo explore` stopped
   with `guest.cpu: ... (got '{{cpu}}')`. So did its tests, which read the manifest file. Fixed
   here: the explorer applies the parameters to the `guest:` section only, the image's values
   from `/etc/hydo/params` (`Space::from_text_params`) and a manifest file's defaults
   (`hydo_manifest::for_explorer`). The rest of the manifest reads as before; the test is in
   `fault_space_line.rs`.
2. **A check's output is cut, and the verdict line then names a fragment.** The rules' lines of
   Restate's introspection made the output long. The verdict named a cut line
   (`VIOLATION [jobs] j25: sys_`), and the violations were left out of `result.json`. The rules
   now print the violations last and keep each detail line short. Open item 152.
3. **One `K=` per schedule.** A kill of the deployment and a kill of the server at other steps
   cannot be one schedule; a pause (`P=svc`) took the first's place.
4. **The clock audit does not follow a project's own clock.** It found 8 places that read
   `SystemTime::now`; Restate's scheduler reads the wall clock through `restate_clock::WallClock`
   (`SchedulerClock`), and the finding's comparison was not among them. Its first place, the
   invoker's `next_retry_at`, is a reported value only.
5. **The Linux release on the bare-metal machine needed `guest/store.tar` unpacked into
   `guest/store` by hand** before `hydo build` of a `network: bridge` manifest (`HYDO_IPROUTE2 not
   found`, as open item 98 says for `hydo setup`).
6. **A kill reaction and a stop reaction did not hold a plain send in flight across a restart.**
   Every command of a round runs in milliseconds, so only the explorer's pause of the deployment
   for two steps (`P=svc@28+2`) kept one acknowledged send from starting before the cut.

## Time

The onboarding started at 19:57 CET on 2026-11-17 and used this tree's `hydo` over the 0.3.4 Mac
release on the Mac engine, and the 0.3.4 Linux release with this tree's `hydo` on hvx (one VM at
a time, while the bare-metal machine ran a re-recording pass). The Mac VM lock was free at every
batch.

| phase | wall time |
|---|---|
| 1. Scouting: the documentation, the SDK, the server's storage and clocks, the clock audit | 40 minutes |
| 2. Setup to the first clean run, twice: the service in Rust, the import, the image | 20 minutes |
| 3. Directed runs, the rules' fixes, the hvx tree | 30 minutes |
| 3. Exploration A (Mac) and the `cf=` sweeps (hvx), at the same time | 40 minutes |
| 4. Judging finding 1: the code, 30 directed runs on hvx, exploration B | 60 minutes |
| 5. The duplicate searches and the write-up | 60 minutes |
