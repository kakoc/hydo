# Finding 1 without hydo

These scripts show finding 1 of [../RESULTS.md](../RESULTS.md) on Hatchet v0.120.5 with plain
Docker. They run the example's own Compose file: PostgreSQL 16, Hatchet's migration and setup
jobs, two engines, the API, and three workers of `hq`. PostgreSQL is also Hatchet's message queue
(`SERVER_MSGQUEUE_KIND: postgres`), as in Hatchet's `docker-compose.release.yml`. Nothing in
Hatchet is changed or patched.

You need Docker with Compose, Go (the build fetches go1.27.3 once), Python 3, and about 2 GB of
memory for the containers.

```sh
sh stack.sh up                         # build hq and start the stack (project name hqnative)
bash probe.sh kill hatchet-engine-1    # one probe: a crash of engine 1
bash probe.sh none hatchet-engine-1    # the control: no stop
R=8 ALONE=1 OFFSETS="0.1 0.2 0.3 0.4 0.5" sh measure.sh   # 8 rounds of a crash, a clean stop
                                       # and a control, with the stopped engine alone
sh stack.sh down                       # remove the containers, the volumes and hq's image
```

`cancel.sh` and `q-cancel.sql` belong to finding 2: the first cancels runs of a task that does
not look at its cancellation, and the second reads what Hatchet recorded about one cancelled run.

## What one probe does

1. It schedules 150 runs of the task `sched` for one instant, 20 s ahead, through Hatchet's API
   (`hq op schedmany`, the Go SDK's `Schedules().Create`). The API acknowledges each one.
2. Just after that instant, while the engines turn the due runs into tasks, it stops one engine
   (with `alone-kill` or `alone-term`, the other engine is stopped first and started at the end):
   - `kill`: SIGKILL, as a crash or an out-of-memory kill does;
   - `term`: SIGTERM, a clean stop;
   - `none`: no stop, the control.
3. Ten seconds later it starts the engine again. Then it waits up to 120 s for every scheduled
   run to finish on a worker. Each worker records each run it starts in the table `app_run` of
   the database `app`.
4. It prints one line with its tag. `stuck=` counts the runs that did not run and whose trigger
   message is left in the state `ASSIGNED` in Hatchet's table `MessageQueueItem`; `late=` counts
   the other runs that did not run in time.

`measure.sh` runs the probes and then, 6 minutes after the last one, `verify.sh`, which counts
again for each probe the runs that never ran (`never=`).

A lost run stays lost. Hatchet deletes the scheduled run's row when it sends the trigger message,
and a message in the state `ASSIGNED` is never read again. It expires after 5 minutes, and then
Hatchet's cleanup deletes it.

## Why the runs are lost

Hatchet's PostgreSQL queue (`internal/msgqueue/postgres/msgqueue.go` and
`pkg/repository/sqlcv1/mq.sql`) works like this:

1. `ReadMessages` selects up to 1,000 `PENDING` messages and sets them to `ASSIGNED` in one
   committed statement.
2. The engine handles them.
3. Then `AckMessage` deletes each message.

No query ever sets an `ASSIGNED` message back to `PENDING`. When the engine that read a batch
dies before it acknowledges the batch, every message of the batch stays `ASSIGNED` until it
expires. The same happens when the handler returns an error. For a trigger message, the run is
never created. The scheduled run's row was deleted when the message was sent, so nothing triggers
it again.

## What it showed

On an M2 Pro Mac with Docker 28.4 (2 CPUs and 8 GB for Docker), 2026-11-14. Each line of the
table is one `measure.sh` series; `verify.sh` looked again at least 6 minutes after each series,
and every run that had not run by then had still not run. Every lost run of these series had its
trigger message left `ASSIGNED` (`stuck=` in the probe's line).

| series | probes of each kind | SIGKILL: probes that lost runs (runs lost) | SIGTERM | no stop |
|---|---|---|---|---|
| `R=6`, both engines run | 6 | 2 (28, 2) | 0 | 0 |
| `R=10 OFFSETS="0.3 0.5 0.7 0.4 0.6"`, both engines run, stopped after 7 rounds | 7 | 0 | 0 | 0 |
| `R=6 N=600 AT_MS=45000 OFFSETS="0.3 0.6 0.9 1.2 1.5"`, both engines run | 6 | 0 | 0 | 0 |
| `R=8 ALONE=1 OFFSETS="0.1 0.2 0.3 0.4 0.5"`, the stopped engine alone | 8 | 4 (95, 44, 10, 20) | 0 | 0 |

With two engines, the engine that reads a batch is the one that stops only in some probes, and
the batch is in flight for a fraction of a second. With `ALONE=1` the other engine is stopped
first, so the stopped engine reads every batch, and half of the crashes lost runs. No clean stop
and no control lost a run.

The first probes, with an earlier version of `probe.sh` that did not start both engines before it
scheduled, lost 2, 80 and 97 runs in 3 of 6 crashes. In one clean stop of those probes 39 runs did
not run, but the stopped engine had not come back before the next probe; it is not counted.
