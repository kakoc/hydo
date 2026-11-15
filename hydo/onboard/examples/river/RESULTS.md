# River under hydo, a job queue's promises under faults

River (github.com/riverqueue/river) is a job queue for Go on PostgreSQL. A program inserts jobs
into a PostgreSQL table, often inside its own business transaction, and River's clients work
them. One client is elected leader, and the leader runs the maintenance services: it moves
scheduled jobs to the queue, rescues jobs whose worker died, and inserts the periodic jobs.

River is a library, so this example has its own small Go program on it, `riverq`, with three
worker processes on one PostgreSQL. The onboarding started as a new user would, with `hydo
onboard` on a folder with a Compose file. It then hunted for a new River bug with River's own
promises as the rules.

## The short answers

- **What was found?** One River bug that no report names, and three clock effects that River's
  design explains:
  1. **A River bug, new as far as the search found.** After a crash of PostgreSQL, two clients
     act as leader at once for a few seconds, and both insert the same run of a periodic job.
     River keeps its leader in an unlogged table, and PostgreSQL empties such a table when it
     recovers from a crash. So another client wins the election while the old leader still
     trusts its term. Natively, 5 of 30 crashes of PostgreSQL gave a periodic run inserted by
     two clients, and 0 of 30 clean restarts did. On hvx, `disk=none | run K=postgres@6+1
     W=44c763a6,p3,q2,r400` breaks the rule on every run, and holds on every run with the table
     made `LOGGED`. The search of River's issues, pull requests, and discussions found no report
     of it. Nothing was posted.
  2. **A clock effect.** A forward clock step longer than River's 15 s lease also gives two
     leaders, with either table. The lease ends by PostgreSQL's clock. The leader trusts its
     term by its own monotonic clock.
  3. **A clock effect.** A forward clock step makes the leader insert every run of a periodic
     job that the step skipped, at once.
  4. **A clock effect.** After a backward clock step, a scheduled job waits for its time on the
     clock that went back, and a dead leader's lease lasts longer by the step.
- **What held?** No acknowledged job was lost. No job inserted in a rolled-back transaction ran.
  No unique job was stored twice in its period, and no job that completed in its transaction
  ran again. No cancelled job completed, and no two runs of one job overlapped.
- **What did hydo lack?** Section "What hydo lacked": four gaps of the messages check across
  clock steps and power cuts and one step of `hydo onboard` are fixed here with tests. Six gaps
  stay open.

## The version

River v0.51.2 (2026-11-09) was the newest release when the onboarding started. It ran with its
PostgreSQL driver `riverpgxv5` and pgx v5.11.0, built with Go 1.27.3. PostgreSQL is 16.16
(`postgres:16.16-alpine3.24`). The CHANGELOG from v0.44.0 on was read before the search. It
fixes many faults of the same kinds: the job rescuer, the completer, cancellations lost with a
notification, and periodic jobs after a failed insert.

## The project

| file | what it is |
|---|---|
| `app/` | `riverq`, about 1,000 lines of Go: the workers, the producer operations, the drain, and the rules |
| `build.sh` | builds `bin/riverq-amd64` and `bin/riverq-arm64`, static Linux programs |
| `Dockerfile`, `docker-compose.yaml` | the system as a user runs it: PostgreSQL, a one-shot migration, and three workers |
| `hydo/hydo.yaml` | the services as `hydo onboard` imported them from the Compose file, with the changes below |
| `hydo/workload/` | five `parallel_driver_` commands, one per producer operation |
| `hydo/checks/` | seven `final_` rules, each one line that runs `riverq check <rule>` |
| `hydo/regressions/` | a kill of PostgreSQL that breaks the one-leader rule (finding 1) |
| `native/` | finding 1 without hydo: PostgreSQL in Docker and the workers on this machine |

```sh
sh build.sh
cd hydo && sh fetch-images.sh
hydo build hydo.yaml                                   # x86_64: hvx, KVM
hydo build hydo.yaml --arch arm64 --set arch=arm64     # the Mac engine
hydo run "disk=none | run"
hydo explore --seeds 1-40 --strategy process --new-faults --more-faults --cluster-faults
```

The changes to what `hydo onboard` wrote:

- The workers and the migration run the static program from `root.files`. The Compose build's
  image holds only that program, and the commands and checks need it in the guest anyway.
- Each worker names `postgres` in `depends_on`. The import gave the workers only the one-shot
  job `migrate`, so the explorer had no network link from a worker to PostgreSQL.
- Each worker has a readiness check, `riverq ready wN`: PostgreSQL answers and the worker started.
- The group `workers` with `primary: /bin/riverq leader`, so that `K=`, `P=` and `X=` can aim at
  River's elected leader (`workers.primary`).

## The program

Each worker (`riverq worker wN`) is one River client with the queue `default` (4 jobs at a
time), leader election, and one periodic job, `tick`, every 2 seconds. A tick's arguments name
the client that inserted it. Two settings are shorter than River's defaults, so that a run of a
minute sees them. The job timeout is 4 s, and a running job counts as stuck after 10 s
(`RescueStuckJobsAfter`). A failed job is retried after 1 s per attempt, at most 5 s.

The producer operations, one per workload command:

| command | operation | its result, as the producer knows it |
|---|---|---|
| `plain` | insert a job that sleeps 10 to 160 ms | inserted, or unknown after an error |
| `sched` | insert a job scheduled 0.3 to 3.3 s ahead | the same |
| `order` | insert an order row and its job with `InsertTx` in one transaction; one in three is rolled back | committed, rolled back, or unknown |
| `uniq` | insert a job unique by its key (one of five) and the hour (`ByArgs`, `ByPeriod`); an error is retried | inserted or skipped as a duplicate, or unknown |
| `cancel` | insert a slow job (0.5 to 2 s), wait up to 1.5 s, and cancel it with `JobCancel` | what `JobCancel` answered |

The jobs `order` and `uniq` and the slow jobs complete in their own transaction with
`JobCompleteTx`. That transaction also writes the job's effect, a row of `app_effect`, so a
committed effect is a committed completion. Every run of every job writes a row of `app_run`
before its work. The row has the client, the attempt, and the guest's monotonic clock, and the
run finishes it afterwards.

## The rules

River's documentation states each rule. The messages check (`type: messages`) states the rules
about delivery over the history that the producer and the workers write. The history has a
send per insert, a receive per run, and an acknowledgement when River completed the job. The
other rules are queries over the tables, in `app/check.go`.

| rule | River's words | how it is checked |
|---|---|---|
| no job is lost after its insert committed | the reliable workers page: an inserted job is worked with at-least-once semantics | `jobs.no-loss` (`delivery: at-least-once`), and the generic rule |
| a job inserted in a transaction that rolled back never runs | `InsertTx` | `final_rolled_back`, and the generic rule |
| a unique job has one row and one committed run per key and hour | the unique jobs page | `jobs.duplicates` (`by: message`, the message id is the key and the hour), `final_unique` |
| a job that completed in its transaction never runs again | the same page: a job completed in a transaction that commits never runs again | `jobs.after-ack`, `jobs.processing`, `final_rerun` |
| two runs of one job do not overlap | at-least-once, unless a job runs longer than `RescueStuckJobsAfter` | `final_concurrent` |
| a scheduled job does not start before its time | "The job won't be fetched before its `scheduled_at` time." | `jobs.delay` |
| one leader at a time: a periodic job is inserted once per run | "Leader election ensures that only one worker is managing periodic jobs at any time." | `final_periodic` |
| a job whose cancellation answered `cancelled` never completes | `JobCancel` | `final_cancelled` |
| every acknowledged operation has its result, and every failed one has none | the generic rule over the producer's records | `final_acked` |

River's documentation also says what is not a promise, and the rules leave it out:

- A job can run twice: "Jobs should be written so that they can still succeed even if run
  multiple times."
- A job that finishes but is not marked completed is rescued and run again.
- A unique job is inserted once but "can still execute more than once."
- A periodic run can be skipped when the leader changes.

## Without faults, and the regressions

The final image passed `disk=none | run` twice on hvx with one fingerprint. The run starts 38
commands, and every rule judged something: the messages check 22 sends, 13 resent unique jobs,
and 44 acknowledgements in the first run of the onboarding.

| run on hvx (this tree's hydo in the 0.3.2 Linux release) | image | verdict | fp |
|---|---|---|---|
| `disk=none \| run`, twice | `explore-ob-river-disk.img`, sha256 `c4dfcb4f…` | OK | `6a6b3984349980e0` |
| the regression `two-leaders-after-postgres-crash`, three times | the same | VIOLATION `[periodic]` | `d89fba89c6a12363` |

`hydo repro --regressions` gives 1 of 1 as expected. The regression's `W=` is in the seed form,
so it composes from the current command list; a change of `hydo/workload/` changes its
composition and its timing. The separation runs in finding 1 used the image before the
regression file changed, which differs from this one only in that file.

## What was found

A finding is a River bug only when its cause is in River's code and it breaks a promise of
River's documentation.

### 1. Two leaders after a PostgreSQL crash insert one periodic run twice (reported as [riverqueue/river#1602](https://github.com/riverqueue/river/issues/1602); closed as not planned on 2026-11-14: the maintainers keep `river_leader` unlogged by design, say that regular periodic jobs can be duplicated, and recommend unique jobs)

**How it was found.** The first directed run with a fault broke the periodic rule. The run was
`disk=none | run K=postgres@5+1` on hvx, and the run of 00:00:14 was inserted twice, by `w1`
and by `w2`, each at 00:00:14.000. The blind exploration `x3` found it again in seed 14:
`disk=none dseed=d584008bda670834 | run K=postgres@3+1 N=postgres-*@7-10:loss,85%
P=worker_3@12+1 T=17:30 W=726d723b,p3,q3,r800` inserted the run of 00:00:14 by `w1` and `w3`.
That run's second double insert, at 00:00:56, came from its forward clock step (finding 2).

**The cause.** River's migration creates the leader table `river_leader` as `UNLOGGED`
(`riverdriver/riverpgxv5/migration/main/002_initial_schema.up.sql`). PostgreSQL empties every
unlogged table when it recovers from a crash, and a standby that is promoted has them empty too.
River's leader works with two clocks:

1. The table says until when the term holds: `expires_at`, 15 s after the last reelection by
   PostgreSQL's clock (`river_leader.sql`).
2. The leader trusts its term locally for 14 s after its last reelection attempt, by its own
   monotonic clock (`internal/leadership/elector.go`, `leaderLocalDeadlineSafetyMargin`). It
   tries to reelect every 5 s.

After a crash the row is gone. The next election attempt of any other client inserts its own
row at once (`LeaderAttemptElect`, `ON CONFLICT (name) DO NOTHING`), without waiting for the old
term to expire. The old leader learns that it lost only at its next reelection attempt, which
finds no row. Until then both clients run the maintenance services. The periodic job enqueuer
checks no leadership when it inserts (`internal/maintenance/periodic_job_enqueuer.go`). So both
insert the runs that fall into the overlap. The overlap lasts up to one reelection interval, and
longer when the old leader backs off after the errors of the outage.

A crash gives two leaders only when another client's election attempt comes before the old
leader's next reelection attempt, and each image times the attempts in its own way. On the
final image, 0 of 18 plain kills (`K=postgres@N+1` for N = 3 to 14, `K=postgres@N+2` for N = 3
to 8) broke the rule. A kill with another composition of the workload did: `disk=none | run
K=postgres@6+1 W=44c763a6,p3,q2,r400`, one of 16 kills with drawn compositions. It breaks
the rule on every run of the final image, with the run of 00:00:14 by `w3` (the old leader) and
`w1`, and it is the regression in `hydo/regressions/`.

**Why it is the table.** The same schedules ran on an image whose migration added `ALTER TABLE
river_leader SET LOGGED` (`hydo build --set leader_table=logged`, an experiment, not River's
schema):

| leader table | schedules on hvx | runs that broke the periodic rule |
|---|---|---|
| `UNLOGGED`, River's schema | `disk=none \| run K=postgres@<N>+1`, N = 3 to 14 | 1 of 12: N = 7, the run of 00:00:14 by `w3` and `w1` |
| `LOGGED` | the same 12 | 0 of 12 |

Both images were built from one earlier state of `riverq`, and every one of the 24 runs held
every other rule. A kill of PostgreSQL breaks the rule only when another client's election
attempt comes before the old leader's next reelection attempt. So most kill points pass.

**Separated on the final image.** The schedules below ran on hvx through `hvx-run`. Seed 14's
schedule and its first five variants ran twice each, and each pair gave one fingerprint and one
verdict. The kill with the drawn composition ran three times, twice with `LOGGED`, and the other
rows ran once. "Seed 14" is `x3`'s schedule
`disk=none dseed=d584008bda670834 | run K=postgres@3+1 N=postgres-*@7-10:loss,85%
P=worker_3@12+1 T=17:30 W=726d723b,p3,q3,r800`; the other rows leave faults out of it.

| schedule | leader table | periodic rule | runs inserted twice |
|---|---|---|---|
| seed 14 as saved | unlogged | broken | 00:00:56, 00:00:58 (`w3`, `w2`) |
| seed 14 without `K=postgres@3+1` | unlogged | broken | 00:00:54, 00:00:56 (`w2`, `w1`) |
| seed 14 without `N=postgres-*@7-10:loss,85%` | unlogged | held | |
| seed 14 without `T=17:30` | unlogged | held | |
| seed 14 without `P=worker_3@12+1` | unlogged | held | |
| seed 14 as saved | `LOGGED` | held | |
| only `N=` and `T=17:30` | unlogged | broken | 00:00:54, 00:00:56 (`w2`, `w1`) |
| only `N=` | unlogged | held | |
| only `N=` and `P=` | unlogged | held | |
| only `T=17:30` | unlogged | held | |
| `N=`, `P=` and `T=17:30` | `LOGGED` | broken | 00:00:56 (`w2`, `w1`) |
| `K=postgres@6+1 W=44c763a6,p3,q2,r400` | unlogged | broken, 3 of 3 runs | 00:00:14 (`w3`, `w1`) |
| the same | `LOGGED` | held | |

So seed 14's double inserts on the final image need its forward clock step and its packet loss.
The kill is not one of their causes. The packet loss only moves the step's timing: the step
alone at step 17 passes. Forward steps alone break the rule at other steps, with either table:

| schedule (`disk=none dseed=d584008bda670834 \| run ... W=726d723b,p3,q3,r800`) | unlogged | `LOGGED` |
|---|---|---|
| `T=5:30` | held | |
| `T=7:30` | held | |
| `T=9:30` | held | held |
| `T=11:30` | broken: 00:00:48 (`w1`, `w2`) | |
| `T=13:30` | held | broken: 00:00:50 (`w1`, `w3`) |
| `T=15:30` | broken: 00:00:54 (`w2`, `w1`) | |
| `T=17:30` | held | broken: 00:00:54 (`w1`, `w2`) |

Two causes give two leaders, then. A crash of PostgreSQL empties the unlogged table (this
finding). A forward step of PostgreSQL's clock longer than the 15 s lease expires the row at
once (finding 2). Then the old leader still trusts its term by its monotonic clock. Lost
answers alone did not give two leaders. Packet loss of 85% on every link to PostgreSQL for
1.6 s held, also together with a pause of a worker.

**Without hydo.** `native/` reproduces it on River v0.51.2 with PostgreSQL in Docker and three
workers on the Mac. It kills PostgreSQL's container with SIGKILL and starts it again. A clean
stop with SIGTERM is the control: PostgreSQL then writes the unlogged table at its shutdown
checkpoint and keeps it.

| restarts of PostgreSQL (`native/`) | the leader row gone | another client leader | a run inserted by two clients |
|---|---|---|---|
| SIGKILL, down 1 s (`N=30 DOWN=1 sh measure.sh`) | 18 of 30 | 21 of 30 | 5 of 30 |
| SIGTERM, down 1 s (the same script) | 0 of 30 | 0 of 30 | 0 of 30 |
| SIGKILL, down 3 s (`N=10 DOWN=3`) | 6 of 10 | 7 of 10 | 1 of 10 |
| SIGTERM, down 3 s | 0 of 10 | 0 of 10 | 0 of 10 |
| SIGKILL with the leader stopped for 6 s (`measure-pause.sh`) | 6 of 10 | 10 of 10, during its term | 0 of 10 |
| SIGTERM with the leader stopped for 6 s | 0 of 10 | 0 of 10 | 0 of 10 |
| only the leader stopped for 6 s | 0 of 10 | 0 of 10 | 0 of 10 |

The probe samples the leader row every 0.5 s, so the column "the leader row gone" misses short
gaps. The stopped leader shows the cause without the race. After a crash another client takes
the leadership within 3 s, while the stopped leader's 15 s term still holds. After a clean
restart, or without a restart, nobody does. `native/README.md` has the steps.

**River's documentation and earlier reports.** The periodic jobs page says: "Leader election
ensures that only one worker is managing periodic jobs at any time." It also warns that a
periodic run can be skipped when the leadership changes, and suggests unique jobs for
`RunOnStart`. Nothing in the documentation says that a crash of PostgreSQL ends a leader's term
early or gives two leaders.

The search covered River's open and closed issues, pull requests, and discussions. Its phrases
were `unlogged`, `river_leader`, `periodic duplicate`, `periodic job twice`, `duplicate
periodic`, `leader failover`, `postgres restart leader`, `postgres crash`, `multiple leaders`,
`split brain`, `failover`, `leadership`, `elector`, `LeaderAttemptElect`, `crash`, and `twice`.
It also read every entry of the CHANGELOG, up to v0.51.2, that names a leader, an election, or
an unlogged table. It found no report of this cause. Four are close and are not the same:

- Discussion #343 (2024): a maintainer names "potential gaps or duplicates that occur if a leader
  change happens right around the time of a periodic job being inserted." He calls it a
  shortcoming of the periodic design. Here the leader change is not around an insert: two
  clients hold the leadership at once for seconds, and every run in that time is inserted twice.
- Pull request #1600 (open, 2026-11-14), titled "Publish the loss of leadership before resigning." A
  leader whose reelection deadline passes while the database is unreachable keeps its
  maintenance running until its resignation finishes. That gap needs the old term to expire.
  Here the term has not expired, because the crash deletes it. The pull request does not change
  the unlogged table or the election.
- Pull request #1213 (merged, 2026-04): leadership terms and the local trust window. It made the
  window shorter than the lease, so that the old leader stops before the lease expires. That
  holds only while the row exists.
- Issue #283 (closed, 2024), "Periodic Jobs are emitted twice than expected": an early insert
  margin of one client, fixed by pull request #284. It has no leader change.

**The decision.** A River bug, new as far as this search found. River promises one leader at a
time, and a crash or a failover of PostgreSQL gives two for seconds. The cause is that the
leader's row lives in an unlogged table and that a new election does not wait for the old term.
The effect a user sees is a periodic job run twice. A unique periodic job (`UniqueOpts.ByPeriod`)
hides it. The other maintenance services also run twice in the overlap; the rules here did not
see a harm from that. Nothing was posted.

### 2. A forward clock step longer than the lease gives two leaders too (a clock effect)

River's lease ends at `expires_at` by PostgreSQL's clock, 15 s after the last reelection. The
leader trusts its term for 14 s by its own monotonic clock, which a step of the realtime clock
does not move. After a forward step of PostgreSQL's clock by more than 15 s, the row is expired
at once. The next election attempt of another client deletes it and wins. Both clients insert
the runs until the old leader's next reelection attempt finds no row. A step of 30 s broke the
one-leader rule at 2 of 7 steps with River's unlogged table. It broke it at 2 of 3 steps with
the table `LOGGED` (the tables in finding 1). The same leader logic gives this effect for any
table, so it is a clock effect. A deployment whose database host steps its clock forward by
more than 15 s gets two leaders for a few seconds. River's documentation does not mention it.

### 3. A forward clock step inserts every skipped periodic run at once (a clock effect)

A `T=` token steps the guest's realtime clock, as an NTP step does. PostgreSQL's `now()` and
River's clients read the same clock. The leader's local trust window and Go's timers use the
monotonic clock, which the step does not move.

- **Every skipped run at once.** River's enqueuer computes a schedule's next run from the run
  before it, not from the present, and inserts each run that is due. Its comment says it does so
  "to stay as true as possible to the original schedule" (`periodic_job_enqueuer.go`). After a
  forward step it inserts every run that the step skipped, one after the other. The
  exploration `x1` showed it in seed 7, with a step to 1 s before the next full hour
  (`T=7:@:00-1s`). The leader inserted 293 runs of the 2 s job within one 2 s window.
- **The same on the final image.** After `disk=none | run T=10:3600`, the leader `w1` inserted
  about 300 skipped runs, scheduled from 00:00:12 to 00:10:22, within one second. Then it lost
  its lease, because the step had ended the lease by PostgreSQL's clock. `w3` took over at
  01:00:14 and went on from the present, so the runs from 00:10:24 to 01:00:12 were never
  inserted. No run was inserted twice (`hydo inspect exec` with `psql` at the end of the run).
  With a job every minute and a step of a day, the same code would insert up to 1,440 jobs at
  once.
The catch-up follows from River's design and from a clock that jumps; it is not reported as a
bug here.

### 4. After a backward clock step, River waits for the clock (a clock effect)

River's scheduler makes a job available when `scheduled_at` has passed by PostgreSQL's clock.
The rescuer takes a stuck job when `RescueStuckJobsAfter` has passed since its attempt. A step
back moves both times away:

- After `T=13:-300` a job scheduled 0.5 s ahead waited 5 minutes, longer than the drain (seed 1
  of `x1` and `x2`).
- After `T=18:-2592000` (30 days back, then a power cut) a job that was running at the cut was
  still `running` at the end (seeds 5 and 12 of `x3`). Its attempt had started 30 days "later"
  than the clock now said, so the rescuer did not count it as stuck.

A step back also lengthens a dead leader's lease. The lease ends at `expires_at` by PostgreSQL's
clock. After a step back of 5 minutes the row of a killed leader stays valid for 5 minutes
more, and no other client can win the election. Without a leader no scheduled job becomes
available. Two runs show it: `x4` seed 24 (`K=workers.primary@1+1 ... T=15:-300`) and seed 36
(`K=workers.all@8+3 ... T=12:-3600`). Both ended with due scheduled jobs still `scheduled` after
the drain's 150 s. At
the end of seed 24's run on the final image, `river_leader` still held the killed client
`w1-260`. Its term ran from 00:00:05 to 00:00:20, and PostgreSQL's clock read 23:57:46.
The restarted worker had become `w1-332`.

River's documentation says that a scheduled job is not fetched before its time, and these waits
follow from that promise and from a lease kept by the database's clock. The generic rule
reports such jobs as waiting, and the messages check counts a delayed job as undecided across a
step. On the final image the schedules of seeds 1, 5 and 12 held every rule, and `disk=none |
run T=10:-300` passed too.

### Mistakes in this workload and its checks

Five kinds of failure were the rules' own mistakes. Each is fixed, and the schedules that
showed them ran again on a later image and held every rule (section "The explorations"):

- **The periodic rule's period came from the clock at the insert.** River's enqueuer inserts
  every missed run of a schedule in turn, for example after a pause of the leader. So two runs
  inserted within one 2 s window got one period and looked like a double insert by one leader.
  River sets a periodic job's `scheduled_at` to its run's time, and the rule now groups by it.
- **The unique rule counted committed runs per key only.** River's `ByPeriod` counts the hour of
  the job's time, and a clock step over the hour makes a second job of a key legal. The rule now
  counts per key and hour, and the history's message id of a unique job carries its hour.
- **The generic rule called a waiting job lost.** After a step back, a scheduled job waits for
  its time. A job that was running at a power cut is rescued only when `RescueStuckJobsAfter`
  has passed on the clock. The rule now reports such jobs as waiting (finding 4).
- **The periodic rule compared retried jobs.** A job that fails gets its retry's time as its new
  `scheduled_at`. After a forward step of an hour, ten periodic jobs of one catch-up each failed
  once and then carried one retry time. So the rule saw one run inserted ten times (`x4` seed
  21; `hydo inspect exec` showed `attempt` 2 and one error on each). The rule now compares only
  jobs without an error.
- **River's cleaner deleted the evidence.** A forward step of a year made every completed job
  older than River's 24 h retention, and the leader's cleaner deleted the rows that the generic
  rule reads. The workers now keep every finished job (`CompletedJobRetentionPeriod: -1` and the
  same for cancelled and discarded jobs).

One more was the program's own: `riverq migrate` failed in one boot of an image, because the
readiness check from the Compose file passed too early (item 11 of "What hydo lacked"). The
migration now tries again for up to a minute.

## The explorations

Every exploration ran on hvx with `--strategy process --new-faults --more-faults
--cluster-faults` and 2 workers. Other sessions shared the bare-metal machine, at a load of 30
to 40 on 8 threads, so the wall times say little. Each first run of a schedule is counted below.
The explorer ran every failure again and began to shrink it.

| name | rules and hydo | seeds | first runs | failures, by cause |
|---|---|---|---|---|
| `x1` | the first rules | 1-40, stopped after seed 11 | 11 | 8. A scheduled job waiting after a step back: seeds 1 and 5. A unique job over the hour: 6, 8, and 11. The period from the insert time after a forward step: 7 and 10. The duplicates ERROR of the checker: 4. |
| `x2` | the period from the run's time | 1-40, stopped after seed 6 | 6 | 4: the waiting scheduled job (1 and 5), the unique job over the hour (6), the duplicates ERROR (4) |
| `x3` | the period, the hour, the waiting scheduled job, and two checker fixes | 1-40, stopped after seed 17 while it shrank | 17 | 6. Finding 1: seed 14. A running job that waited after a step back of 30 days: 5 and 12. The cleaner after a step of a year: 16. The duplicates ERROR of the checker: 4. A restart that failed: 17. |
| `x4` | every fix but two: the retried periodic jobs, and the `processing` claim across a cut | 18-40, stopped while it shrank | 23 | 5. Retried periodic jobs: seed 21. A dead leader's lease after a step back: 24 and 36. An acknowledgement record lost at a power cut: 37. A restart that failed: 39. |

`x1` and `x2` were stopped as soon as their failures showed the rules' mistakes. The six
schedules that failed by those mistakes ran again on a later image and held every rule. They
are seeds 1 and 6 of `x1` and `x2`, and seeds 4, 5, 12 and 16 of `x3`. Seed 14 of `x3` ran
again too. On an earlier image it doubled the run of 00:00:14 only. On the final image it
doubled the runs of 00:00:56 and 00:00:58, and finding 1 separates its faults.

Seed 17 of `x3` (`disk=cut dseed=a8a48df0db54e975 | run K=workers.all@16+1
N=postgres-worker_1@16-19:loss,84% C=20 P=worker_1@4+3 W=219dc827,p6,q3,r800`) ends in `ERROR
worker_1 restart failed`, also on a later image. The kill restarts `worker_1` while its link to
PostgreSQL loses 84% of the packets. River's `Client.Start` cannot connect and returns an error,
and `riverq worker` exits with status 1. Under Docker Compose, `restart: unless-stopped` starts
such a worker again. hydo applies a restart policy while it starts the services and while the
workload runs, but not to the restart of a `K=` fault (item 10 of "What hydo lacked"). It is
not a River finding.

## What hydo lacked

In the order in which they cost time. Items 1 to 5 are fixed in this milestone, each with a
test; the others stay open.

1. **The messages check judged a deduplication window across a clock step.** `hydo guide
   messages` says that a `T=` step inside a pair's interval leaves a window, a lease, or a delay
   undecided. But the `duplicates` window compared the two sends anyway. A unique job sent
   before and after a step back over the hour is stored twice by River's design, and the check
   called it a violation. Fixed: such a pair is undecided, and `clock_steps: strict` judges it
   (`hydo-history`, `messages::duplicates`).
2. **The messages check called a delayed message lost when a clock step back kept it waiting.**
   A job scheduled 0.5 s ahead waits 5 minutes after a step back of 5 minutes, so the drain ends
   before it runs. Fixed: an undelivered message with a delay is undecided when a `T=` step lies
   between its send and the drain (`messages::no_loss_and_its_excuses`).
3. **The messages check gave an ERROR for a history whose resent messages all had one send with
   an unknown outcome.** Its message said that no send or delivery carries a system id. That was
   false: every other send carried one. Unique jobs sent while PostgreSQL was down caused it
   (`x3` seed 4). Fixed: the claim is undecided only for a client that records no system id at
   all; otherwise those pairs are counted as undecided (`messages::duplicates`).
4. **The messages check called a delivered message unacknowledged across a power cut.** The
   `processing` claim wants an acknowledgement for every delivery of a drained group. A plain
   job's completion committed just before a power cut. Its acknowledgement record, which the
   worker writes after River reports the completion, was lost with the cut (`x4` seed 37).
   Fixed: a message delivered only before a cut and drained after it, without an
   acknowledgement record, is undecided (`messages::processing`).
5. **`hydo onboard`'s next step did not work as printed.** It said `sh hydo/fetch-images.sh`, and
   the script looks for `hydo-fetch-image` on `PATH`. A hydo from `cargo install` or `hydo setup
   --get` keeps it in its release directory. So the script stopped with "put hydo's bin
   directory on PATH" without naming the directory. Fixed: when the tool is not on `PATH`, the
   step names it with `HYDO_FETCH_IMAGE=<release>/bin/hydo-fetch-image`
   (`onboard::the_fetch_step_names_the_release_fetcher_when_it_is_not_on_path`).
6. **One image per Compose service with the same build.** The migration and the three workers
   share one `build: .`, and `hydo onboard` built and saved four identical images, four times
   the disk. An image of a larger program would cost gigabytes.
7. **Network links from a one-shot job's dependencies are lost.** The workers wait for the job
   `migrate`, which waits for PostgreSQL. The import gave the workers `depends_on: [migrate]`
   only, so the explorer had no link from a worker to PostgreSQL. This is open item 97 in
   another form; the example names `postgres` by hand.
8. **A group's `primary:` that asks the database fails while the database is down.**
   `P=workers.primary@4+12 K=postgres@4+1` ended in `ERROR workers.primary: the group's primary:
   command named no member in 10 tries`. The kill came first, and River's leader lives in
   PostgreSQL. Putting the kill one step later works.
9. **A failing run prints no `hydo: VERDICT` line.** On hvx a passing `hydo run` ends with
   `hydo: VERDICT ...`, and a failing one with `hydo: history checks: HYDO-ORACLE: VIOLATION
   ...` only. So a script that looks for the verdict line misses the failures.
10. **A `K=` restart ignores the service's restart policy.** A worker that `K=` restarts while
    its link to PostgreSQL is lossy exits during its readiness check, because River's
    `Client.Start` cannot connect. Docker Compose with `restart: unless-stopped` starts it again;
    hydo ends the run with `ERROR worker_1 restart failed` (`x3` seed 17). `hydo guide services`
    says that the policy holds from the service's start; the restart of a `K=` fault is not
    covered.
11. **The imported readiness check of PostgreSQL passes too early.** The Compose file's
    healthcheck, `pg_isready -U postgres`, asks the Unix socket. The image's entrypoint first
    runs a temporary server on that socket only, to initialize the database, and then restarts
    it. The import copied the check, so a migration started during the restart, and one boot
    ended in `ERROR service start (worker_1)`. Asking `-h 127.0.0.1` waits for the real server.

The Mac had too little free disk for the Mac engine during this onboarding. So every run used
hvx on the bare-metal machine, with this tree's `hydo` inside a copy of the 0.3.2 Linux release.

## Time

About 4.5 hours of one session. Writing the program and the Compose file, and `hydo onboard`,
took 40 minutes. The first run without faults passed at once. The bare-metal machine's load and
its shared cargo lock made each build and run several times slower than on an idle machine.
The four explorations, the directed runs, and the native measurements took the rest.
