# Hatchet under hydo, a task queue's promises under faults

Hatchet (github.com/hatchet-dev/hatchet) is a task queue and workflow engine in Go on
PostgreSQL. A program triggers tasks, workflows of several tasks (a DAG), scheduled runs and cron
runs. Hatchet's engine queues them and dispatches them over gRPC to the program's workers. The
engine keeps every state in PostgreSQL, and a deployment without RabbitMQ also uses PostgreSQL as
its internal message queue between the engine's parts.

Hatchet is a server with an SDK, so this example has its own small Go program on the SDK, `hq`,
with three worker processes. They run beside two engines, the API, and one PostgreSQL. The
onboarding started as a new user would, with `hydo onboard` on a folder with a Compose file. It
then hunted for a new Hatchet bug with Hatchet's documented promises as the rules.

## The short answers

- **What was found?** One Hatchet bug that no report names, and two known bugs, one of them in a
  new path:
  1. **A Hatchet bug, new as far as the search found.** Hatchet's PostgreSQL message queue
     loses every message that an engine read and did not acknowledge before it crashed. Reading
     a batch sets each message to `ASSIGNED` in a committed statement, and no query ever sets an
     `ASSIGNED` message back to `PENDING`. After a crash, the batch's trigger messages are never
     handled. The scheduled runs they carry never run, although the API acknowledged them and
     Hatchet deleted their schedules. A `RunNoWait`
     that the engine acknowledged is lost the same way, and a run whose completion is lost runs
     again. On hvx, `disk=none | run K=hatchet_engine_1@6+3` loses the acknowledged runs `b10_4` and
     `b10_5` on every run. Natively, 4 of 8 crashes of an engine that read the queue alone lost 10 to 95 of
     150 scheduled runs, and no clean stop and no run without a stop lost one. The search of
     Hatchet's issues, pull requests, and discussions found no report of it. Nothing was posted.
  2. **A known bug in a new path.** After a crash of an engine, a cancelled run became
     `COMPLETED`: the worker never got the cancellation, and the engine took the worker's
     completion after the cancellation (issue #2655 reports that acceptance).
  3. **A known bug.** A crash of an engine ran a scheduled run twice (issue #4129).
- **What held?** No cron tick ran twice or before its time, and no scheduled run started before
  its time. No two runs of one concurrency key overlapped, the rate limit held, and no DAG task
  started before its parent completed. No task run was executed twice at the same time. Each held
  in every run, with and without faults.
- **What did hydo lack?** Section "What hydo lacked".

## The version

Hatchet v0.120.5 (tag of 2026-11-13, commit `0787e45d`) is the newest tag with published
images. The newest GitHub release is v0.114.2 (2026-10-26); the tags after it have images but no
release notes. The images are `ghcr.io/hatchet-dev/hatchet/hatchet-{migrate,admin,engine,api}`
at `v0.120.5`, pinned by digest in `hydo/hydo.yaml`. PostgreSQL is 16.16
(`postgres:16.16-alpine3.24`). The Go SDK is the module `github.com/hatchet-dev/hatchet` at
v0.120.5, built with Go 1.27.3.

The deployment follows Hatchet's `docker-compose.release.yml`: PostgreSQL is the store and the
message queue (`SERVER_MSGQUEUE_KIND: postgres`), which is also what Hatchet Lite uses by default.
Two engines run, as the high-availability guide suggests, so that the cron and scheduled-run
tickers, the partitions of the controllers and schedulers, and the workers move between them.

## The project

| file | what it is |
|---|---|
| `app/` | `hq`, about 1,100 lines of Go on Hatchet's Go SDK: the workers, the producer operations, the drain, and the rules |
| `build.sh` | builds `bin/hq-amd64` and `bin/hq-arm64`, static Linux programs |
| `Dockerfile`, `docker-compose.yaml` | the system as a user runs it: PostgreSQL, Hatchet's migration and setup jobs, two engines, the API, the application's migration, and three workers |
| `hydo/hydo.yaml` | the services as `hydo onboard` imported them from the Compose file (`hydo.generated.yaml` is the raw import), with the changes marked `hand:` |
| `hydo/workload/` | eight `parallel_driver_` commands, one per producer operation |
| `hydo/checks/` | ten `final_` rules, each one line that runs `hq check <rule>` |
| `hydo/rules.yaml` | the rules as `hydo rules check` validated them |
| `hydo/regressions/` | a crash of an engine that loses acknowledged scheduled runs (finding 1) |
| `native/` | finding 1 without hydo, with plain Docker |

```sh
sh build.sh
cd hydo && sh fetch-images.sh
hydo build hydo.yaml                                   # x86_64: hvx, KVM
hydo run "disk=none | run"
hydo explore --seeds 1-20 --strategy process --new-faults --more-faults --cluster-faults
```

The changes to what `hydo onboard` wrote:

- The workers and the application's migration run the static program from `root.files`. The
  Compose build's image holds only that program, and the commands and checks need it in the
  guest anyway. The bare-metal machine has no Docker, so the import could not build that image.
- The engines' port is the gRPC port 7070. The import took the image's `EXPOSE 8080`, and its
  readiness check waited for a port that the engine does not open.
- The import ran the one-shot jobs (migration, setup) in the `setup:` of each of the three
  services that wait for them, which can start at the same time. The second engine and the API
  now wait for the first engine, whose `setup:` runs the jobs once.
- Each engine, the API and each worker name `postgres` in `depends_on`, and each worker its
  engine and the API. The Compose file has these links only through the one-shot jobs, so the
  explorer had no network link from an engine to PostgreSQL.
- Each worker has a readiness check, `hq ready wN`: PostgreSQL answers and the worker started.
- The groups `engines` and `workers`, so that `K=`, `P=`, `X=` and `N=` can aim at either.
- `guest.clock_after_cut` and `guest.outcomes_after_cut`, and no `disk` faults.

## The program

Each worker (`hq worker wN`) is one Hatchet worker with 6 slots. `w1` and `w3` connect to engine
1, `w2` to engine 2. It registers these tasks:

| task | what it does | retries, timeout |
|---|---|---|
| `plain` | works 10 to 310 ms | 3 retries, 20 s |
| `sched` | the same; it is only triggered by scheduled runs | the same |
| `limited` | works 100 to 500 ms; `GROUP_ROUND_ROBIN` with one run at a time per key (`input.key`, one of three) | the same |
| `rated` | works 10 to 60 ms; a static rate limit of 2 per second | the same |
| `order` | a DAG: `a`, then `b`, then `c`, each 20 to 220 ms | the same, per task |
| `slow` | works 1 to 3 s and stops when it is cancelled | the same |
| `tick` | the cron `*/2 * * * * *`: one run every 2 s | the same |

Every execution of a task writes a row of `app_run` in the database `app` before its work and
finishes the row after it. The row has the message id, Hatchet's workflow run id and task run id,
the retry count, the worker, the guest's monotonic clock, and for `tick` the time Hatchet scheduled
the run for (`hatchet__cron_scheduled_at`). A failed write fails the execution, and Hatchet
retries it.

The producer operations, one per workload command:

| command | operation | its result, as the producer knows it |
|---|---|---|
| `plain`, `limited`, `rated`, `dag` | `RunNoWait` of the task or the DAG | acknowledged with a run id, or unknown after an error |
| `burst` | 6 runs of `plain` one after the other | the same, per run |
| `sched` | a scheduled run 0.5 to 4.5 s ahead (`Schedules().Create`, the API) | acknowledged, or unknown |
| `schedmany` | 10 scheduled runs for one instant 2.5 s ahead | the same, per run |
| `cancel` | a run of `slow`, cancelled through the API (`Runs().Cancel`) 0.2 to 1.5 s later; then the status the API reports | the status after the cancel |

The producer and the workers write hydo history records: a send for each acknowledged trigger
and scheduled run, with the scheduled run's delay, and a receive for each execution a worker
starts.

## The rules

Each rule is a promise of Hatchet's documentation. `hydo rules scan` over Hatchet's
documentation, schema and API listed 771 lines of evidence; the rules below come from the
documentation's promise words, since the schema's constraints repeat what PostgreSQL enforces.
`hydo rules check rules.yaml --mutate` judged them on runs without faults (section "Without
faults, and the regression").

| rule | Hatchet's words | how it is checked |
|---|---|---|
| no acknowledged run is lost | "Hatchet is at least once: tasks are not silently dropped" (architecture-and-guarantees) | `tasks.no-loss` (`type: messages`, `delivery: at-least-once`), and the generic rule `final_acked` |
| a scheduled run runs once | "A scheduled run that comes due while the engine is down still runs once" (scheduled-runs) | `final_scheduled` |
| a cron tick runs once | "If the engine is down when a cron is due, that occurrence is skipped ... It does not replay the ticks" (cron-runs) | `final_cron`, by `hatchet__cron_scheduled_at` |
| a scheduled or cron run does not start before its time | "The scheduled time is when Hatchet enqueues the task" | `tasks.delay` (`delay: per-message`), `final_early` |
| a concurrency key with max runs 1 runs one at a time | "If no slots are available, the new task or workflow is added to a queue for its key." (concurrency) | `final_concurrency` |
| a rate limit holds | "If a step run exceeds the rate limit, Hatchet re-queues the step run" (rate-limits) | `final_rate`: at most 4 starts in any second (two windows of 2) |
| a DAG task waits for its parents | "those parent tasks must complete successfully before the child task is allowed to run" (directed-acyclic-graphs) | `final_dag` |
| a cancelled run does not complete later | cancellation | `final_cancelled`: the API's status stays `CANCELLED`, and no execution finishes after it |
| one task run is not executed twice at once, nor again after it finished | at least once allows a rerun after a failure | `final_overlap`, `final_rerun` (each duplicate is classified below) |

Hatchet's documentation also says what is not a promise, and the rules leave it out:

- A task can run more than once: "a task can run more than once, so your task code should be
  idempotent".
- A timed-out task "does not guarantee that the task will be stopped immediately".
- A cron tick that comes due while the engine is down is skipped.

## Without faults, and the regression

The final image passed `disk=none | run` twice on hvx with one fingerprint. The run starts 38
commands, and every rule judged something: in `hydo rules check`'s three runs the generic rule
judged 109 to 143 triggers, the cron rule 9 ticks, the scheduled rule 69 to 86 scheduled runs,
the concurrency rule 4 to 6 runs and the DAG rule 10 to 12 tasks.

| run on hvx (this tree's hydo in a copy of the 0.3.3 Linux release) | image | verdict | fp |
|---|---|---|---|
| `disk=none \| run`, twice | `explore-ob-hatchet-disk.img`, sha256 `915d95db…` | OK | `2d17c0c77d838bbd` |
| the regression `lost-run-after-engine-crash`, twice, and once on the image before | the same | VIOLATION `[acked]`, 2 of 107 triggers | `a2ef47c5068b01ba` |

`hydo repro --regressions` gives 1 of 1 as expected, with `a2ef47c5068b01ba`. The regression
has no `W=`, so it composes from the current command list with the seed 0; a change of `hydo/workload/` changes its composition and its timing. Each
change of the image moved the kill points that lose a message (section "The explorations"),
and the regression is the kill point that lost one on the final image. That kill point was
found on an image whose regression file named another kill point, with a line of the same
length; `disk=none | run` gave `2d17c0c77d838bbd` on both images, so the line of the regression
does not move a run.

## What was found

A finding is a Hatchet bug only when its cause is in Hatchet's code and it breaks a promise of
Hatchet's documentation.

### 1. A crash of an engine loses the messages it read from the PostgreSQL queue (a Hatchet bug, new as far as the search found)

**How it was found.** The first directed kills of an engine broke the rules. On an earlier image of
this example, `disk=none | run K=hatchet_engine_2@7+3` broke the generic rule and `tasks.no-loss`:

```text
HYDO-ORACLE: VIOLATION [acked] 8 of 107 acknowledged or failed triggers without their result
tasks.no-loss: s0_1 was acknowledged to process 0 at 26.851 s and never delivered in channel tasks, which was drained
```

The 8 lost runs were scheduled runs of two `schedmany` commands (`s0_1`, `s0_2`, `s0_4`, `s0_5`,
`s1_0` to `s1_3`). The API had acknowledged each one, and Hatchet had deleted each schedule.
`K=hatchet_engine_1@13+3` lost 2 more (`s0_6`, `s1_5`). The same runs also broke `final_rerun`:
a `plain` run that a worker had finished started again as its next attempt.

**The cause.** Hatchet's PostgreSQL queue is `internal/msgqueue/postgres/msgqueue.go` with the
queries of `pkg/repository/sqlcv1/mq.sql`. A subscriber handles a queue in three steps:

1. `ReadMessages` selects up to 1,000 messages with `status = 'PENDING'` and sets them to
   `ASSIGNED` in one statement, which commits at once.
2. The engine handles each message (`preAck`).
3. `AckMessage` deletes the message (`DELETE ... WHERE id = ... AND status = 'ASSIGNED'`).

No query sets an `ASSIGNED` message back to `PENDING`, and there is no visibility timeout. A
message is read only while it is `PENDING`. When the engine that read a batch dies between step 1
and step 3, every message of the batch stays `ASSIGNED` until it expires 5 minutes after it was
sent (`expiresAt`), and then `CleanupMessageQueueItems` deletes it. The same holds when step 2
returns an error: `doTask` returns before it acknowledges.

The messages of `task_processing_queue_v2` carry the engine's own work. The ones this example saw
lost are:

- `task-trigger`: a run to create. A scheduled run's trigger is sent by the ticker after it
  deleted the schedule's row (`RunScheduledWorkflow` in `internal/services/ticker/schedule_workflow_v1.go`),
  so nothing triggers it again: the run is lost. A `RunNoWait` that cannot be scheduled at once
  on a worker of the engine that took it (the optimistic path) goes the same way, and so does
  every cron tick.
- `task-completed`: a worker's report that it finished a run. Hatchet does not learn of the
  completion and runs the task again as its next attempt (`final_rerun`).
- `olap_queue_v2` messages, the status events of the API and the dashboard: a run's status there
  can stay behind.

No clean stop (SIGTERM) of an engine lost a run in the native probes below.

**Separated on the final image.** The regression has one fault, the kill of engine 1 before
step 6 and its restart before step 9. Without it, the run passes (`disk=none | run`, the table
above). The trigger messages of `b10_4` and `b10_5`, two `RunNoWait` of one `burst` command, were
in the batch that engine 1 read; the same run also ran `b10_1` again after a worker had finished
it. Other kill points of the same image pass (section "The explorations"): a crash loses messages
only when it falls between an engine's read of a batch and its acknowledgement.

**Without hydo.** `native/` reproduces the loss on Hatchet v0.120.5 with plain Docker on this
Mac: the example's Compose file, 150 scheduled runs for one instant, and a stop of one engine
just after that instant (`native/README.md`). Every run that did not run had its trigger message
left `ASSIGNED`, and none of them ran in the next 6 minutes.

| probes (`native/measure.sh`) | SIGKILL of an engine | SIGTERM | no stop |
|---|---|---|---|
| the stopped engine alone (`ALONE=1`), 8 of each | 4 lost runs: 95, 44, 10 and 20 of 150 | 0 | 0 |
| both engines run, 19 of each, 150 or 600 runs | 2 lost runs: 28 and 2 of 150 | 0 | 0 |

With both engines running, the crash loses runs only when the crashed engine is the one that
read the batch, within a fraction of a second.

**Hatchet's documentation and earlier reports.** The architecture page says: "Hatchet is at
least once: tasks are not silently dropped, and failures retry according to your configuration."
The scheduled-runs page says: "A scheduled run that comes due while the engine is down still runs
once when the engine is back online, if it has not already triggered." The high-availability
page suggests several engines.

**Where the PostgreSQL queue is selected, and Hatchet's `main`.** The server's default is
RabbitMQ (`pkg/config/server/server.go:610`, `default:"rabbitmq"`; the configuration page
`frontend/docs/content/docs/self-hosting/configuration-options.mdx:335` lists
`SERVER_MSGQUEUE_KIND` with `rabbitmq` or `postgres`). The PostgreSQL queue is selected by:

- `docker-compose.release.yml`, `SERVER_MSGQUEUE_KIND: postgres` at line 18 (`hatchet-admin`),
  line 39 (`hatchet-engine`) and line 65 (`hatchet-api`), the same on v0.120.5 and on `main`;
- Hatchet Lite, `cmd/hatchet-lite/main.go:113-114`: when neither `SERVER_MSGQUEUE_KIND` nor a
  RabbitMQ URL is set, it sets `SERVER_MSGQUEUE_KIND` to `postgres`; the same on `main`.

On `main` at `ee6d34ec` (2026-11-14), `pkg/repository/sqlcv1/mq.sql` and
`internal/msgqueue/postgres/msgqueue.go` are byte for byte the files of v0.120.5:
`ReadMessages` (mq.sql:70-95) reads `status = 'PENDING'` and sets `ASSIGNED`,
`BulkAckMessages` (mq.sql:97-102) deletes only `ASSIGNED` rows, and `Subscribe`
(msgqueue.go:170) acknowledges a message (line 207) only after its handler returned without an
error.

The search covered Hatchet's open and closed issues, pull requests, and discussions, with
`gh search issues`, `gh search prs` and the discussions search of GitHub's API. Its phrases were
`MessageQueueItem`, `postgres message queue`, `msgqueue postgres`, `pgmq`, `ReadMessages`,
`AckMessage`, `ASSIGNED status`, `redeliver`, `visibility timeout`, `unacked`, `ack message`,
`lost messages`, `message lost`, `lost trigger`, `scheduled run lost`, `scheduled run never`,
`scheduled workflow not triggered`, `task never runs`, `engine restart lost`, `engine restart
task`, `engine crash`, `OOM engine`, `stuck running`, `postgres mq`, `expiresAt`, `messages
expire`, `missing runs`, `lost runs`, `runs disappear`, and `hatchet-lite restart`. It also read the CHANGELOG up to
v0.114.2, and the history of `mq.sql` and `msgqueue.go` on `main` up to `d7feb829` (2026-11-13):
neither file changed after v0.120.5. It found no report of this cause. These are close and are
not the same:

- Issue #4129 (closed as not planned, 2026-11-07): the scheduled-run trigger's three steps are
  not atomic, and its idempotency key expires 30 s after the run's time, so a scheduled run can
  be delayed or run twice. Its loss is a delay: the key expires and the schedule fires again.
  Here the schedule is gone and only the queue message carried the run.
- Pull request #4543 (open, 2026-07-27): an outbox for the OLAP messages, so that the engine
  publishes them at least once. It is about publishing to the queue, not about a message that
  an engine read and never acknowledged.
- Pull request #1145 and #1198 (merged, 2024-12 and 2025-01) fixed races in the same queue code
  (new tasks after the queue's cleanup, a shared `err`).
- Issue #552 (closed, 2024): a step executed twice after an engine restart, in the RabbitMQ era.

**The decision.** A Hatchet bug, new as far as this search found. Hatchet promises that tasks are
not silently dropped and that a scheduled run that comes due still runs once. With the
PostgreSQL queue, a crash of the engine that read a batch drops the batch: acknowledged scheduled
runs and triggers never run, and finished runs run again. The cause is that
the queue marks a message `ASSIGNED` when it is read and never gives it back. A queue with
leases (a visibility timeout on `ASSIGNED`, as `readAfter` already allows) or reading and
acknowledging in one transaction would keep the messages. RabbitMQ puts a message back in its
queue when the consumer's connection closes before the acknowledgement; this example did not run
Hatchet with RabbitMQ. Nothing was posted.

### 2. A cancelled run becomes COMPLETED after an engine crash (known class, issue #2655, in a new path)

**How it was found.** The directed kills `K=hatchet_engine_1@3+3`, `K=hatchet_engine_2@3+3` and
`K=hatchet_engine_1@11+3` broke `final_cancelled` on two images of this example, each with one
or two cancelled runs. On the second image `K=hatchet_engine_2@3+3` gave:

```text
cancelled x12 (52bcd145): the API reported CANCELLED, and now COMPLETED
```

**What Hatchet recorded.** `hydo inspect exec` at the end of that run read the tables
(`native/q-cancel.sql`). In guest time, the worker `w3` started `x12` at 00:00:22.946. Hatchet's
core table `v1_task_event` has, for the same attempt, `CANCELLED` at 00:00:23.768 and `COMPLETED`
at 00:00:24.511. The worker never saw the cancellation and finished at 00:00:24.374. The client
had read `CANCELLED` from the API at 00:00:24.695. The monitoring events then went `CANCELLED`,
`FINISHED`, and the run's final status is `COMPLETED`.

**The cause, as far as it was read.** Two things meet:

1. The engine accepts a completion of an attempt that it had already cancelled. Issue #2655
   (closed 2025-12-12) reports the same acceptance from the TypeScript SDK, which sent the
   completion of a cancelled task; the fix (#2865) made that SDK stop sending it. The engine
   still takes such a completion.
2. The worker did not get the cancellation. The engine sends it to the worker's dispatcher
   through the dispatcher's own queue (`<dispatcher id>_dispatcher_v1`). After the kills, this
   example's queue held messages of dispatchers that no longer existed, and each probe of
   finding 1 left such messages. Which message carried this cancellation was not traced.

Without a fault no cancelled run completed: every `cancel` command of the runs without faults
held, and natively 4 of 4 cancellations of a task that ignores its cancellation stayed
`CANCELLED` (`native/cancel.sh`). The run with the kill was not reproduced natively.

**The decision.** Not counted as new: the engine-side acceptance is the subject of issue #2655.
The new part is the path, a cancellation lost with an engine, through which a Go worker, whose
SDK does stop at a cancellation it receives, still completes a cancelled run.

### 3. A crash of an engine runs a scheduled run twice (known, issue #4129)

`K=hatchet_engine_2@6+3` on the second image broke `final_scheduled`: four runs of two `schedmany`
commands each ran twice, with two run ids. On an earlier image `K=hatchet_engine_1@13+3` ran
`s1_6` twice. Issue #4129 (closed as not planned) describes the cause: the ticker claims the
scheduled run's idempotency key until 30 s after the run's time, sends the trigger, and only then
deletes the schedule. A ticker that dies before the delete leaves the schedule, and another
ticker triggers it again once the key has expired.

### 4. A finished run runs again after an engine crash (finding 1, a consequence)

`final_rerun` broke in every run that lost a message of finding 1, and in some others: a worker
finished a run, and the same task run started again as attempt 1, on the same or another worker.
The worker's completion is a `task-completed` message of the PostgreSQL queue, and the engine that
read it died before it acknowledged it. Hatchet's documentation allows a
task to run more than once, so the rerun alone is not reported. Its cause is the lost message of
finding 1.

## The explorations

Every run used hvx on the bare-metal machine, with this tree's `hydo` inside a copy of the 0.3.3
Linux release, one VM at a time. Other sessions shared the machine at a load of 12 to 28 on 8
threads, so a run took 2 to 5 minutes. Four images of this example ran; each one differs from the
next in `hq` (the text of a rule, the case counts and changes for `hydo rules check`) or in the
regression file, so each moves the timing of the runs.

| runs | image | what broke |
|---|---|---|
| `K=hatchet_engine_1@N+3` and `K=hatchet_engine_2@N+3`, N = 3, 5, 7, 9, 11, 13 | the first | 5 of 12: `cancelled` (N = 3 on both engines, engine 1 at 11); `acked`, `rerun` (engine 2 at 7, 8 runs lost; engine 1 at 13, 2 lost, and `scheduled`) |
| `disk=none \| run`, twice | the second | nothing; one fingerprint |
| `K=hatchet_engine_2@7+3`, three times | the second | `rerun` each time, one fingerprint |
| `K=hatchet_engine_1@3+3` twice, `K=hatchet_engine_2@3+3`, `K=hatchet_engine_1@11+3` | the second | `cancelled` (engine 2 at 3; engine 1 at 11, with `scheduled`) |
| `K=hatchet_engine_2@N+3` (N = 5, 6, 8), `K=hatchet_engine_1@N+3` (N = 6, 7, 8, 12) | the second | `scheduled` (engine 2 at 6); `acked` and `rerun` (engine 1 at 8 and at 12, each twice with one fingerprint; at 12 also `scheduled`) |
| `P=hatchet_engine_1@3~0:15000`, a pause of 15 s | the second | nothing |
| `disk=none \| run` twice, `K=hatchet_engine_1@8+3` three times | the third, with the regression at engine 1, N = 8 | nothing; one fingerprint each |
| `hydo rules check rules.yaml --mutate`, 3 runs and 1 with the changes | the second | nothing; 10 rules hold, 5 changes caught |
| `disk=none \| run`, `K=hatchet_engine_1@N+3` (N = 4 to 8), `K=hatchet_engine_2@N+3` (N = 4 to 7) | the final, with an earlier regression line of the same length | `acked` and `rerun` at engine 1, N = 6; nothing at the other 6; engine 1 at 7 and engine 2 at 5 ended `ERROR harness boot vm` (no memory for the guest) |

Only four rules ever broke: `acked` (with `tasks.no-loss`), `rerun`, `scheduled`, and
`cancelled`. A blind exploration (`hydo explore --strategy process --new-faults --more-faults
--cluster-faults --no-shrink --time-limit 1800`, one worker) on the final image ran 3 seeds in
the 30 minutes, at 150 s a run, and the explorer ran each failure again with the same verdict:

| seed | schedule | verdict |
|---|---|---|
| 1 | `disk=cut dseed=4127cc1347fae6ba \| run K=worker_1@2+2 N=hatchet_engine_1-hatchet_engine_2@18-19:loss,72% C=20 P=hatchet_engine_2@5+4 T=13:-300 W=3e71cf9e,p5,q3,r800` | `VIOLATION service cannot recover (worker_1)` |
| 2 | `disk=none dseed=d7071eb1f9d436b9 \| run K=postgres@4+3 N=workers.bridge.6@0-1:loss,100% P=hatchet_engine_1@12+1 W=5764a73c,p3,q2,r400` | `VIOLATION [acked]`: the DAG `d4` started and never completed; `rerun`: `r6` ran again after it finished |
| 3 | `disk=none dseed=1717ad91294d20d2 \| run K=workers.one.3@4+3 N=hatchet_engine_1-hatchet_api@1-4:loss,61% P=worker_1@8+1 T=13:30 W=fc94c8ea,p2,q2,r800` | OK |

Seed 2 crashes PostgreSQL, not an engine. A handler that fails because the database is down also
leaves its message `ASSIGNED` (finding 1, the error path), which fits a DAG whose next task is
never created; its faults were not separated. In seed 1, `worker_1` exited with status 1 at every
start of the recovery boot after a step of the clock 5 minutes back and a power cut, until its
readiness limit; its log was not read, so the cause is open.

## What hydo lacked

In the order in which they cost time. None is fixed in this milestone; open item 121 lists them.

1. **A service without `image:` cannot have its own `env:`.** `hydo build --check` stopped with
   `services[6] (worker_1): env only with image:`. The three workers need different engines, so
   `hq` now picks its engine from its name. A plain process can only get variables through its
   `cmd:` (`env X=1 /bin/hq ...`) or the manifest's top-level `env:`, which every service shares.
2. **YAML anchors stop `hydo build` with a wrong message.** The hand-edited manifest used
   `env: &hatchet-env` and `env: *hatchet-env` for the five Hatchet services. `hydo build --check`
   answered `hydo.yaml:107: unexpected indentation`, which names neither the anchor nor the alias.
   The example writes each block out.
3. **The import's port and readiness check of a gRPC server came from the image's `EXPOSE`.**
   Hatchet's server images all `EXPOSE 8080`, and the engine listens on 7070 (gRPC) only. The
   import gave both engines `port: 8080` and `ready: nc -z ... 8080`, which never passes. A wrong
   `port:` also aims the explorer's network faults at a port without traffic.
4. **One-shot jobs ran in the `setup:` of each service that waits for them.** The migration and
   the setup job ran in the `setup:` of both engines and the API. These start together, so the
   jobs could run three times at once; the done-marker in `/tmp/hydo` does not lock. The example
   makes the second engine and the API wait for the first engine.
5. **No network link from a service to what its one-shot job waits for.** As open items 97 and
   111 say: the engines had no link to PostgreSQL, because the Compose file reaches it only
   through the migration job. The example names `postgres` by hand.
6. **`build:` services without Docker on the machine.** The bare-metal machine has no Docker, so
   `fetch-images.sh` stopped at the four images of the Compose build (`app-migrate` and the
   workers) on every run, after it had fetched the others. The example runs the program from
   `root.files` and deletes the build lines from the script.
7. **`hydo onboard` changes the project's own files.** It created `AGENTS.md` and `.gitignore`
   in the example's folder. The example does not keep them.
8. **The verdict names one failed rule.** A run that broke `acked`, `tasks.no-loss`, `rerun` and
   `scheduled` printed only `VIOLATION [acked] ...`; the others are in `result.json` and the
   console's `HYDO-CHECK-OUT` lines. A script that reads the verdict line misses them.
9. **`hydo rules scan` reports noise for an API.** Of its 31 `api` lines over Hatchet's OpenAPI
   files, most were `maxLength: 36`, the length of a UUID.

## Time

About 8 hours of one session. The program, the Compose file, `hydo onboard` and the first run
without faults took 1.5 hours; the first run passed after one fix of the folder (a cancelled
run's receive record). Reading Hatchet's queue and the native probes took 2 hours. The bare-metal
machine's load made each hvx run take 2 to 6 minutes and made some runs fail to get memory for
their guest, so the directed runs, the regression and the exploration took the rest.
