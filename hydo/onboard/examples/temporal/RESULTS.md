# Temporal: results

This onboarding puts Temporal 1.32.2 under hydo and hunts for a new bug. Temporal is a workflow
engine. A client starts a workflow, and a worker runs the workflow's code, which schedules
activities, waits on timers, and receives signals and updates. The server records every step as
an event in the workflow's history, in PostgreSQL. 1.32.2 is the latest release on 2026-11-14
(released 2026-11-02, image revision `6d085822`).

Most runs used the Mac engine on an M2 Pro with arm64 guests. The first exploration used hvx on
the bare-metal machine. hydo was built in this tree with the 0.3.2 release's guest files.

## The short answers

- **A new bug candidate.** After the clock has been stepped back, one crash of PostgreSQL takes
  the Temporal server down for as long as the step was (finding 2). The SQL handle limits its
  reconnects to one per second, and it measures that second on the wall clock. Every start and
  list then answers `Namespace default is not found.` It reproduces without hydo, in Docker in a
  virtual machine whose clock the script steps. No upstream issue or pull request describes the
  wall-clock throttle. Issue 11691 and pull request 11730 (both open) are about the same code,
  for another cause.
- **By design.** A backward clock step delays every new timer and timeout until the clock is
  back where it was (finding 1). The history service keeps its timer queue monotonic on purpose.
- **What held.** Without clock steps, no rule failed. These rules held:
  - one run per workflow ID;
  - signals delivered exactly once;
  - update IDs answered once, with the same answer when they came again;
  - at most one outcome per activity and timer, and no timer before its time;
  - history event IDs without gaps;
  - completed orders kept their results, and visibility agreed with the histories.

## The system under test

| file | what it is |
|---|---|
| `docker-compose.yaml` | Temporal 1.32.2, PostgreSQL 16, and the worker. It starts from temporalio/docker-compose's `docker-compose-postgres.yml` (commit `cd6d3b19`) |
| `setup/setup-schema.sh` | creates Temporal's two databases with `temporal-sql-tool` of the `admin-tools` image, as the `auto-setup` image did up to 1.29 |
| `dynamicconfig/development-sql.yaml` | the dynamic configuration of temporalio/docker-compose, unchanged |
| `tw/` | the worker, the client and the checks, one Go program on Temporal's Go SDK 1.51.0 |
| `build.sh` | builds `hydo/bin/tw` for the guest |
| `hydo/hydo.yaml` | from `hydo compose ../docker-compose.yaml --out hydo.yaml --images-dir images`; hand changes are marked `hand:`. `hydo.generated.yaml` is the raw import |

```sh
sh build.sh                      # hydo/bin/tw (Go 1.26 or later)
cd hydo && sh fetch-images.sh    # the four images by digest (network, once)
hydo build hydo.yaml
hydo run "disk=none | run"
hydo explore --seeds 1-20 --strategy process
```

The temporalio/docker-compose repository pins Temporal 1.29.1 and its `auto-setup` image. Docker
Hub has no `auto-setup` image after 1.29.7, so the Compose file of this example uses the
`server` image and a one-shot job of the `admin-tools` image. The server runs all its services
(frontend, history, matching and the internal worker) in one process with 4 history shards, as
the Compose file does.

## The workload

The business is an order. One workflow, `Order`, runs per order ID:

1. It runs the activity `Charge`, whose result names the attempt that completed.
2. It starts a timer of 2 to 15 s, drawn by the client.
3. Until the timer fires, it collects the signals `item` that arrive.
4. It runs the activity `Ship`, takes the signals that arrived meanwhile, and waits until every
   update handler has finished.
5. It returns its result: the charge, the signals in the order it received them, the updates it
   applied, and how long the timer took in workflow time.

The workflow also answers the update `reserve`: each update gets the next sequence number, and
the workflow keeps `<update ID>=<number>`.

The client is `tw`, called by the commands in `hydo/workload/`. Each command makes one request
and records what it saw: `ack` (the server answered yes), `failed` (the server refused, for
example `WorkflowExecutionAlreadyStarted`), or `unknown` (a timeout or an unavailable server).

| command | what it does |
|---|---|
| `parallel_driver_start` | starts an order with a new ID. One start in four reuses an ID already started, which the server must refuse: the ID reuse policy is `REJECT_DUPLICATE` and the conflict policy `FAIL` |
| `parallel_driver_signal` | sends the signal `item` with a new message ID to one of the last four orders started |
| `parallel_driver_update` | sends the update `reserve` and waits for its answer. One update in three reuses an update ID that was answered before, which must get the same answer |
| `anytime_observe` | reads one order. When the order is completed, it records the run with the hash of the result |
| `first_clock` | records the wall clock before any fault (finding 1) |

The worker is `tw worker`, an image service with the task queue `orders`. It registers the
namespace `default` itself at its start, because the server image does not.

## The rules

Each rule is a promise from Temporal's documentation, stated as what the client can check.

| rule | where | what it checks |
|---|---|---|
| a workflow ID runs once | `parallel_driver_start` (exit 100) and `final_history_well_formed` | a start of a used ID is refused; every order ID has one acknowledged run |
| an acknowledged signal is delivered exactly once | `checks: signals` (`type: messages`) | the drain `tw drain` reads every order's history; each acknowledged signal is in it once, and nothing is in it that was not sent |
| an update ID is applied once | `parallel_driver_update` (exit 100), `final_history_well_formed`, `eventually_acked_orders_complete` | a repeated update ID gets the first answer; the history has at most one acceptance and one completion per update ID; the result has every acknowledged update with its answer |
| a completed activity is completed once | `final_history_well_formed`, `eventually_acked_orders_complete` | at most one outcome per scheduled activity; the result's charge equals the history's |
| a timer fires once, and not before its time | `final_history_well_formed`, `eventually_acked_orders_complete` | at most one `TimerFired` per timer; its event time is at least the start's time plus the timeout; the workflow saw at least the timeout pass |
| event IDs increase without gaps | `final_history_well_formed` | the events of every acknowledged run are numbered 1 to N, the first is the start and a close event is last |
| completed stays completed | `eventually_acked_orders_complete` | every acknowledged order completes, with the result the client saw before |
| visibility agrees with the history | `eventually_visibility_agrees` | the list of executions has one execution per order ID. Its run and status are the history's |
| the server serves again after the faults | `eventually_server_answers` (added for finding 2) | within 30 s of the heal, the server lists the namespace's workflows |

The generic rule over the client's acknowledged operations is the second half of
`eventually_acked_orders_complete`: every acknowledged start and update is in an order's
result. The messages check holds every acknowledged signal against the histories. A signal
that arrives after the workflow took its last signals is in the history but not in the result,
so the result is checked only for duplicates. Error logs are findings too (`errors: true`).

## What was found

### 1. A backward clock step stops every new timer and timeout for the length of the step (by design)

The first exploration on hvx failed in seed 5 with `VIOLATION [acked_orders_complete] ... 2 of 3
acknowledged orders not completed`. Its schedule stepped the clock 30 days back before step 7
(`T=7:-2592000`). The step alone gives the same verdict:

```sh
hydo run "disk=none | run T=7:-2592000 W=3cc7df19,p1,q1,r1000"
```

All three orders stop at `WorkflowTaskScheduled`. The order `o15` started after the step: its
first workflow task and its activity completed, and its next workflow task was scheduled and
never started. `hydo inspect exec` ran `temporal workflow describe` of the `admin-tools` image,
which showed it so 2 minutes later, with the clock at 2026-10-15.

The cause is in the history service. A scheduled task gets a fire time no earlier than what the
shard's timer queue has read already. Timers, the timeouts of workflow tasks, and the fall-back
of a sticky workflow task to the normal queue are such tasks. `loadShardMetadata` in
`service/history/shard/context_impl.go` sets the earliest fire time to the persisted
`ExclusiveReaderHighWatermark` of the scheduled queues. After a backward step, every new scheduled
task waits until the clock is back at the time before the step: 30 days here. Temporal keeps its
timer queue monotonic on purpose, so this is not counted as a bug. No upstream issue or document
describes the effect. The searches of issues and pull requests were `clock backwards`, `time
went backwards`, `clock skew`, and `system clock timers`.

`eventually_acked_orders_complete` now excuses orders that wait while the clock is behind the
latest time that the server or the client saw. Every command of the client records the clock it
sees (`clock/<ns>` in `$HYDO_ACKED`), and `first_clock` records it before any fault. The check
prints `excused: ...` when it uses the excuse.

### 2. After a backward clock step, one crash of PostgreSQL takes the server down for the length of the step (a Temporal bug; no upstream report found)

The first exploration on the Mac failed in seed 13 with
`K=postgresql@9+1 N=temporal@4-5:loss,23% P=postgresql@6+1 T=1:-3600`. For 120 s its visibility
check read `cannot read visibility of o6: Namespace default is not found.` The run ended `ERROR
check: exit budget exhausted`. The check `eventually_server_answers` was added for it: 30 s after
the heal, the server must list the namespace's workflows again. Two faults are enough:

| schedule (Mac engine) | verdict |
|---|---|
| `disk=none \| run K=postgresql@9+1 T=1:-300 W=6311ab23,p4,q2,r1000` | `VIOLATION [server_answers] not satisfied 31.19s after heal, deadline 30s (last check rc=1: tw ready: Namespace default is not found.)` |
| the same with `T=1:-3600` | the same violation |
| the same with `T=1:-20` | OK; the server answered 3.15 s after the heal |
| the same without `T=` (with `N=` and `P=` of seed 13) | OK |

The server's log has the cause. When PostgreSQL goes away, the SQL handle of each store drops
its connection pool and reconnects, at most once per second. The handle measures that second on
the wall clock: `common/persistence/sql/sqlplugin/db_handle.go` compares
`h.timeSource.Now()` with `h.lastRefresh`, and `clock.RealTimeSource.Now()` returns
`time.Now().UTC()`, which drops Go's monotonic clock reading. After a step back of D seconds,
the difference stays below one second for D seconds. Every reconnect is refused with `sql handle:
did not refresh database connection pool because the last refresh was too close`, and every
operation of that store fails with `no usable database connection found`. The namespace
registry cannot read its namespaces, so the frontend answers `Namespace default is not found.`
The history service cannot read its task queues either (`GetHistoryTasks`, `GetTimerTasks`),
and the membership heartbeat fails. The code is the same on `main` (`170fb83d`, 2026-11-14).

**Without hydo.** `native/repro-clock-db.sh` reproduces it with Docker in a Colima VM whose
clock the script steps. The clock went back one hour, and PostgreSQL's server got one `kill -9`.
In the 160 s after PostgreSQL was back, 0 of 8 orders started, and the server logged the throttle
line 8,119 times. The control, the same kill without the step, completed 6 of 6 orders. When the
clock was put forward again, a new order completed within 8 s. `native/README.md` has both
outputs.

**Is it known?** The searches covered issues and pull requests of temporalio/temporal, open and
closed, on 2026-11-14:

- `no usable database connection found`, `did not refresh database connection pool`, `last
  refresh was too close`, `sessionRefreshMinInternal`, `lastRefresh`, `database connection pool
  refresh`, `sql handle refresh`, `DatabaseHandle reconnect`, `reconnect throttle`;
- `monotonic`, `clock`, `clock skew`, `clock jump`, `clock step`, `clock backwards`, `time went
  backwards`, `NTP`;
- the release notes of 1.32.2, which name no change to the SQL handle.

None describes the wall-clock throttle. Three reports are about the same code:

- Issue 6514 (closed in September 2024): the throttle kept moving its own last-refresh time, so
  the handle never reconnected. Pull request 6538 fixed that; the throttle still reads the wall
  clock.
- Issue 11691 (open): a throttled forced refresh has already closed the pool, so the handle has
  no pool until the next refresh. It names no clock.
- Pull request 11730 (open): checks the throttle before the pool is closed, for issue 11691.
  Its test moves a fake clock forward only (`Advance`).

The outage needs both defects: 11691's closed pool, and the wall-clock throttle that keeps it
closed for the length of the step. With pull request 11730, a throttled refresh keeps the old
pool; this onboarding did not run the pull request. The wall-clock comparison itself is not
reported. `hydo/regressions/clock-back-db-crash.schedule` keeps the schedule (`# expect:
violation`).

### Not found

Without clock steps, no rule failed in 20 schedules of kills, pauses, network faults, power cuts,
and disk faults. Those runs judged 57 acknowledged starts, 33 signals, and 42 updates, 5 of
them a repeated update ID. 16 starts of a used workflow ID were refused as they must be. The
client saw 11 orders completed, and each kept its result.

### Changes to the workload and its checks

- `eventually_acked_orders_complete` waited for orders behind a backward clock step (finding
  1). It now excuses them, with the client's clock records.
- The drain `tw drain` retried each order for up to 10 minutes and was killed at the `final:`
  command's limit of 60 s. It now stops after 45 s in all.
- `eventually_server_answers` was added for finding 2, and the other two `eventually_` checks
  wait 60 s instead of 120 s. Long waits ran into the Mac engine's exit budget.
- `guest.ram_mib` went from 3072 to 2048 MiB, the smallest size that was tried and passed.

## The explorations

| exploration | engine | strategy | schedules | failures |
|---|---|---|---|---|
| hvx-1 | hvx | `process --new-faults`, stopped | 5 | 1, finding 1 |
| mac-1 | Mac | `process --new-faults`, stopped while it shrank | 18 | 5: findings 1 and 2, and the `duplicates` error that hydo now avoids |
| mac-2 | Mac | the same, stopped | 10 | 2, both finding 2 |
| no-clock | Mac | `process --new-faults` without `clock` | 20 | 1 `ERROR workload: exit budget exhausted`; its replay passed |
| swarm | Mac | `swarm --new-faults`, one worker, stopped | 9 | 3, each with a lying disk or I/O errors: one data disk that did not mount after a power cut, and two `exit budget exhausted` |

The hvx exploration stopped because the bare-metal machine was loaded, and its two workers held
14 GB. The Mac explorations stopped when their failures were understood, so that the shrinker did
not spend hours on clock steps. The swarm exploration stopped when the Mac ran short of memory;
its disk faults are the environment, not Temporal. The final image gives `fp=6f64c437f639b8ed` twice for `disk=none
| run`, and `hydo repro --regressions` gives 1 of 1 as expected.

## What hydo lacked

1. `hydo onboard` in temporalio/docker-compose imported `docker-compose.yml`, the setup with
   Elasticsearch, and did not say that seven other Compose files exist. Fixed: it lists them, and
   `--compose FILE` imports one of them.
2. A messages check with `duplicates: forbidden` ended a run in ERROR when no signal was
   acknowledged or delivered. Fixed: such messages are not judged.
3. Temporal's entrypoint ended with status 1 and no output in the guest. It runs `getent hosts
   $(hostname)`, and Docker puts every container's hostname into `/etc/hosts`; the importer's
   `/etc/hosts` has no line for the guest's hostname. The diagnosis showed an empty log. The
   example sets `TEMPORAL_BROADCAST_ADDRESS`.
4. A check cannot tell whether its run had a clock step. The client records the clock it sees
   instead.
5. A run whose `eventually_` checks wait their whole deadline, while the server logs thousands of
   errors, ends `ERROR ... exit budget exhausted`. The verdict hides the check's violation;
   `result.json` keeps it.
6. `hydo build --check` stops at the first missing image, so it cannot check the workload and
   the checks before `fetch-images.sh` ran.
7. On the Mac, `hydo explore --image <...>-disk.img` answers that `debugfs` is not on `PATH`;
   the Mac image to name is the `.cpio.gz`.
8. The importer guessed 1536 MiB of guest RAM; the four image roots live in guest RAM on the
   Mac.
9. The build leaves its root tree and data image in `.hydo` (about 2 GB on the Mac) after the
   disk image is made.

## Time

The onboarding took about 5 hours on 2026-11-14, with long waits: the Mac's disk was below the
safe line for an hour, and the bare-metal machine was loaded.
