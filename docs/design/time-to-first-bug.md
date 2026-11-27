# Time to a first real run and to a first bug

A user of hydo meets two clocks. The first runs from "I have a repository with a Compose or
Kubernetes file" to a first real run: an exploration whose checks judge the system's real
promises. The second runs from the first exploration to the first real failure with a command
that replays it. This page measures both clocks from the records of the onboardings, says where
the time goes, and estimates what five changes would save. It ends with a target for each clock
and the first two things to build. Nothing changes until the lead decides.

## The records

Every number on this page comes from these records. A number that this page derives from a
record carries the word *inferred*.

- The `RESULTS.md` page of each example under `hydo/onboard/examples/`, mostly its sections
  `Time` and `Onboarding retro`. 26 examples give minutes per phase.
- The two trials by a fresh user: `docs/design/0.4.0-user-trial.md` (Milestone 297) and
  `docs/design/0.6.0-user-trial.md` (Milestone 359). A fresh user learned hydo from its pages
  only.
- The findings ledger of Milestone 380, `docs/design/findings-ledger.md` and its CSV. It holds
  105 bugs of 47 real systems, with how each came first and whether a generic check catches it.
- `docs/ONBOARDING-PLAYBOOK.md` (its phases and throughput), the quickstarts, and the
  milestones of `docs/HISTORY.md`, among them Milestone 304 for explorations on many runners.

An AI coding agent made each onboarding of the records. It followed the onboarding skill and the
playbook, which counts 0.4 to 1 million agent tokens per onboarding. The machines were shared with
other sessions, so many wall times hold waits. A wait is named where the record names it.

Two words recur. A *first run* is the run of one drawn schedule. A *later run* is a re-run of a
failure, or a shrink candidate. A shrink candidate is a variant of a failing schedule with fewer
or smaller faults. The explorer runs candidates to find the minimal schedule.

## Clock 1: from a repository to a first real run

### The stages

| stage | what hydo does | what the user or the agent does | minutes in the records | what failed and was done again | who |
|---|---|---|---|---|---|
| 0. Scouting | `hydo rules scan` lists the API, the schema, and the clock readings in under 1 s (NSQ) | reads the documentation, the issues, the code of the writes, and the deployment manifests; writes down the promises | median 28 (12 to 40), 16 phase tables | nothing | the agent, by hand |
| 1. Install | `hydo setup`, `hydo doctor`; the quickstart says about ten minutes | runs the install, or builds a tree | 6 (libSQL), 12 (Ghost, built from the tree) | a stale `hydo-world` pin (Ghost, YugabyteDB); `hydo setup --get` refused a pinned kernel and the tester made a release tree by hand (0.4.0 trial) | the agent, with hydo's commands |
| 2. `hydo onboard` and `hydo compose` | writes `hydo.yaml` with every service, image digest, address, start order, readiness check, and volume; writes `fetch-images.sh` and placeholders | one command | under 1 s (NSQ) | most examples edited the import by hand. The causes were ports from `EXPOSE`, missing links through one-shot jobs, and an unused data folder. Others were a placeholder against the wrong service, read-only roots, and too little guest RAM (the playbook's section 10) | hydo, then the agent |
| 3. Fetch and build images | `fetch-images.sh`, `hydo build --check`, `hydo build` | runs them after every change | fetch 3 to 17 s; one build 5 s (libSQL), 23 s (Medusa), 28 s (NSQ), 147 s (Ghost), about 4 minutes (ClickHouse), about 25 minutes (Saleor, a loaded machine) | 7 rebuilds in Medusa's setup, 5 in ClickHouse's, 2 in Garnet's; a disk over 4 GiB (YugabyteDB, ClickHouse) | hydo; the waits are the machine's |
| 4. The client and the workload | composes the commands into runs | writes a client program and the workload commands | inside setup; 13 minutes for libSQL with `hydo sql-gen`, 15 for Dragonfly's import, client, and folder | a client fix means a new image and new timing for every schedule (Redpanda, NSQ, RabbitMQ) | the agent, by hand |
| 5. Checks and the read-back | ready-made checks: `type: messages` with `acked_outcome`, `hydo sql-gen`'s checks, the error-log scan | writes the read-back (`final:` drain), the rules, and `eventually_` checks | 10 minutes for Medusa's five checks; otherwise inside setup | rules too strict or reading too early (below) | the agent, by hand |
| 6. The first clean run | `hydo run "disk=none \| run"` twice, the diagnosis, `hydo inspect` | reads the diagnosis and fixes the folder | one run 4 s (libSQL on KVM), 9.5 s (NSQ on the Mac), 50 to 60 s (Garnet on hvx), 3 to 14 minutes (Redpanda on hvx) | start failures: cgroup2 (Dragonfly, Redpanda), `SIGILL` without `guest.cpu: v3` (Dragonfly, ClickHouse), the FIPS entropy panic (SeaweedFS), the console's limit (Typesense, VictoriaMetrics), a readiness deadlock and a missing `tini` (YugabyteDB) | the agent, with hydo's diagnosis |
| 7. The checks' own mistakes | shows each failure with its schedule and replay | reads each failure, fixes the client or the check, rebuilds, explores again | 17 (Ghost, plus a new image and a new exploration), 20 (the 0.6.0 trial, a false ERROR), 57 (TigerBeetle), 90 (NSQ, two client fixes), about 120 (RabbitMQ) | 30 false findings in 47 examples; business rules made 21 of them | the agent, by hand |

Stages 2, 3, and 6 are hydo's work and take seconds to minutes each. Stages 0, 4, 5, and 7 are
written or judged by hand in every onboarding. The only exceptions are `hydo sql-gen` for SQL
systems and the page walk for web apps (`browser.explore`), which need no client.

### What the hand-written part holds

The 17 onboardings from Milestone 287 to Milestone 371 with a client program give these sizes.
The counts come from each example's folder: the client's source, the files under `workload/`
and `checks/`, and the final `hydo.yaml`.

| part | median | range |
|---|---|---|
| lines of the client program, mostly Go | 1,229 | 464 (NSQ) to 1,998 (Garnet) |
| workload commands, small shell files that call the client | 7 | 3 to 16 |
| check files, besides the checks declared in `hydo.yaml` | 3 | 0 to 7 |
| lines of the final `hydo.yaml` (the import with hand edits) | 167 | 95 to 333 |

Earlier examples wrote fewer lines. Medusa's checks are 136 lines of shell, SQL, and awk, its
client 117 lines of JavaScript, and its seed 128 lines. The libSQL and YugabyteDB examples took
their workload and checks from `hydo sql-gen`.

### The totals

| records | first clean run | median |
|---|---|---|
| 9 records that time it from the start (Ghost, Medusa, libSQL, Dragonfly, SurrealDB, Redpanda, YugabyteDB, TigerBeetle, and the 0.6.0 trial) | 14 to 119 minutes | 23 minutes |
| 16 phase tables, scouting and setup, Milestones 287 to 371 | 35 to 110 minutes | 68 minutes |

The later onboardings ran clusters on a loaded bare-metal machine, so they took longer. A first
clean run is not yet a first real run. Stage 7 comes after it, and its fixes took 17 minutes to
2 hours where the records time them. So clock 1 runs from about 1 hour to about 3 hours, and an
agent writes about 1,200 lines in it.

## Clock 2: from the first exploration to a first bug

### The records that time it

| example | engine | first clean run | first failure that is a bug, from the start | how | schedules before it |
|---|---|---|---|---|---|
| Ghost | KVM | 21 minutes | 30 minutes; found blind at 79 minutes | the first run with donations; blind at seed 20 of the second exploration | the first exploration's 20 schedules were spent on two mistakes of the checks |
| Medusa | KVM | 23 minutes | 36 minutes by hand; about 45 blind | blind at seed 2, 6 minutes into the exploration | 1 |
| Dragonfly | hvx | 29 minutes | about 35 minutes | blind, seed 4 | 3 |
| SurrealDB | the Mac, hvx | 18 minutes | 44 minutes | blind, seed 3 on hvx | about 10 |
| Pulsar | hvx | about 70 minutes (inferred from the phases) | about 90 minutes (inferred) | the first run without faults | 0 |
| Redpanda | hvx | 35 minutes | about 1 to 2 hours after the start (inferred: 3 to 14 minutes a run, one worker) | blind, seed 6 | 5 |
| the 0.6.0 trial, NSQ | the Mac | about 14 minutes | about 40 minutes (inferred: inside the first blind round of 9 minutes) | blind; seed 2 in the example's first exploration | 1 |
| TigerBeetle | hvx | 85 minutes | none; the first violation was a mistake of the check, at 93 minutes | | |

The ledger gives the position of the first blind find for 43 bugs. The median is the seventh
schedule; 25 came within 10 schedules and 35 within 20. So a bug comes early in the seed order.
The minutes that follow come from how long a run takes, how many workers run, and what else the
exploration does before it reports.

### Where an exploration's time goes

`hydo explore` writes `events.jsonl` as it runs. It writes `exploration.json` and `report.md`,
which `hydo report` reads, only when every re-run and every shrink has ended
(`hydo/crates/hydo-explore/src/run.rs`). A user sees a problem's words and its replay command
only at that end.

| measure | value | record |
|---|---|---|
| later runs per first run | median 6.9, from 1.9 (Ghost) to 16.9 (Restate), over 10 explorations | the ledger, impression 4 |
| Lago's first exploration | 20 first runs, 12 re-runs and 306 shrink runs in 66 minutes; first runs 6 % of the runs | the playbook, section 9 |
| the 0.6.0 trial's blind round | 89 % of the workers' busy time in re-runs and shrinks; the aimed round 85 % | the trial, section 4 |
| the Redis quickstart, seeds 1 to 60 | 39 % of the busy time in re-runs and shrinks | Milestone 336 |
| Garnet on KVM runners | a run 200 to 350 s; a part's shrinks up to 3.5 hours; 7 parts were still shrinking when the round ended | `garnet/RESULTS.md` |
| Redpanda on 20 KVM runners | the part of seeds 27 and 28 took 9,469 s, most of it shrinks | Milestone 304, open item 153 |
| Dragonfly's shrink of seed 4 | 81 runs | Milestone 304 |

Judging a failure took as long as exploring, or longer:

- Restate: 40 minutes of explorations and 60 of judging.
- VictoriaMetrics: 50 minutes of directed runs and the first exploration, and 100 of judging.
- Meilisearch: 27 minutes of explorations, and 85 of directed runs and the native reproduction.
- Redpanda: 2 hours for finding 2's evidence alone.

In 13 onboardings from Milestone 287 on, the explorations and the judging took a median of 58 %
of the wall time. 11 examples ran explorations with `--no-shrink`, and in 5 more an exploration
was stopped during its shrinks.

### Workers and engines

- **hvx, the bare-metal machine.** It is shared by several agents and has 32 GB. Explorations ran
  with one or two workers. A run of a cluster took 2 to 14 minutes (Redpanda, Pulsar, Hatchet).
- **The Mac engine.** Three to five workers; Valkey ran 1,400 runs an hour with three. The shared
  VM lock took 28 of Meilisearch's 32 setup minutes and 70 of its 85 minutes of directed runs.
- **KVM on GitHub's runners (Milestone 304).** Up to 20 parts at once, each on a runner with 4
  CPUs and 15 GB. Garnet's explorations all ran there.

| exploration | one runner | 20 runners | what set the time |
|---|---|---|---|
| Redis, seeds 1 to 40 | 9 minutes | 9.4 minutes on 8 | the build job, 6 minutes |
| Dragonfly, seeds 1 to 60 | 75 minutes | 31 minutes | the slowest part: 1,281 s without a failure; the part with the shrink 454 s |
| Redpanda, seeds 1 to 40 | not run: 16.2 hours of exploration in all | 2 hours 53 minutes | the part of seeds 27 and 28, 9,469 s, mostly shrinks |

Sharding cut Dragonfly's time by a factor of 2.4, not 20. The 6-minute build and the slowest
part's shrinks set the time.

## Where the time goes

- **Clock 1 is hand work.** Every onboarding wrote a client of about 1,200 lines, 7 workload
  commands, and its checks. hydo's own steps, the import, the fetch, the build, and the run, take
  seconds to a few minutes.
- **The checks' own mistakes are the longest part of clock 1.** They took 17 minutes to 2 hours,
  and each needed a new image and a new exploration. Business rules made 21 of the 30 false
  findings. The generic read-back made 2.
- **A rebuild follows every change of the client.** The client is inside the image. So a fix
  costs a build of 23 s to 4 minutes, and on a loaded machine more. It also moves the timing of
  every saved schedule (Redpanda's item 7).
- **Waits for machines.** 20 of Garage's 50 setup minutes, 30 of Garnet's 70, 28 of
  Meilisearch's 32, 59 minutes of TigerBeetle's 85, and 21 minutes of YugabyteDB's 119.
- **Clock 2 is mostly later runs.** The first failure comes at a median of 7 schedules. Re-runs
  and shrinks then take 85 % to 94 % of an exploration's runs or busy time before the report
  appears. The records give 6.9 later runs per first run, Lago's 318 of 338, and the trial's 85 %
  and 89 %.
- **Judging is the second half of clock 2.** It took as long as the explorations, or longer, in
  every record that separates the two.

Stages 4, 5, and 7 repeat in every onboarding: the client, the read-back, and the fixes of the
checks. So does the wait for the shrinks before a report.

## Five changes, estimated

### Change A: onboarding without code

hydo would generate the client from the Compose file and the project's API. The API comes from
the OpenAPI file when the project has one, or from the write-route list of `hydo rules scan
--kind writes`. A service of a known protocol gets a driver chosen by its image. hydo would add
the generic checks of the ledger without hand-written code:

- a read-back of every acknowledged write with its final outcome (`acked_outcome`);
- every replica compared with its leader after the heal, for the groups of the Compose file;
- an `eventually_` check that the system serves writes after the heal;
- the error-log scan.

The ledger says that a generic check catches 63 of the 105 bugs (61 rows). A generated client
reaches only the bugs whose workload it can make. This page sorts the 61 rows by the workload
each needed.

| workload the bug needed | bugs | the rows |
|---|---|---|
| any workload, or none: the service does not start again or stops serving | 5 | Redpanda 1, TiKV 2, YugabyteDB 2, GreptimeDB 1, SurrealDB 3 |
| plain writes through an HTTP API, read back by id | 13 | Formance 1, Meilisearch 2 and 4, Blnk 2 and 4, rqlite 1, Typesense 1, VictoriaMetrics 1, 3, and k1 to k3, Vikunja 1 (the walk) |
| plain writes through Redis | 5 | Dragonfly 1 and 2, Garnet 3 and A, Valkey 1 |
| plain writes through S3 | 1 | Garage 1 |
| plain messages through NATS | 5 | NATS 3.1, 3.2, 3.3, 3.5, and c1 |
| plain messages through AMQP streams | 2 | RabbitMQ 1 and 3 |
| plain messages through NSQ | 3 | NSQ 1 to 3 |
| plain writes through SQL or SurrealQL | 2 | ClickHouse 1, SurrealDB 1 |
| plain writes through etcd | 1 | etcd 1 |
| a domain sequence, or a worker of the project's SDK | 11 | Lago 1, 6, and 8, Hatchet 1, Restate 1, Saleor 1, Temporal 1 and 2, TigerBeetle 1, the query engine 2, ClickHouse 4 |
| transactions | 5 | Pulsar 1 to 3, Redpanda 2, TiKV 1 |
| an operator's action: a failover, a slot migration, a node's removal, a graceful stop, an upgrade | 10 | Garnet 2, B and C, Kvrocks 1, rqlite 2 and 3, Garage 3, etcd's upgrade, libSQL 3, Redpanda 3 |

Four rows of the protocol classes need an administration command beside the writes. They are
Dragonfly's `SAVE`, Garnet's `BGSAVE`, etcd's defragmentation, and ClickHouse's replicated tables.

- **With HTTP drivers only**, a generated setup reaches 18 of the 105 bugs (17 %).
- **With drivers of the common protocols too**, it reaches 37 (35 %), or 33 (31 %) without the
  administration commands.
- **With a library of operator actions** for clusters (failover, node removal, graceful stop,
  upgrade), it reaches 47 (45 %).
- The other 16 bugs of the generic set need a client that knows the domain. 33 bugs need a check
  of the project or another check outside the set, and for 9 the record does not say.

The scan of write routes found no route in rqlite, Typesense, and SeaweedFS, so 2 of the 13 HTTP
rows need a better scan or the project's OpenAPI file. The same setup removes the client's 1,200
lines and most of stage 7, because the generic read-back made 2 of the 30 false findings. With a
protocol driver, clock 1 becomes the import, the fetch, the build, and two clean runs. By the
records that is 5 to 15 minutes (inferred from the stage times).

### Change B: report first, shrink later

The explorer would write each failure into the report after its first run and one re-run, with
the replay command of the first run. It would shrink a failure only when the user asks, or after
the round. A first run's schedule replays as it is, because every run is deterministic.

- A round today runs 7.9 runs per first run: the first run and 6.9 later runs. Reporting first
  needs the first runs and one re-run per failing signature. That is a cut by a factor of 4 to 6
  (inferred).
- Lago's first exploration would report after about 4 minutes of its 66: its 20 first runs were
  6 % of the runs (inferred).
- The 0.6.0 trial's blind round would report after about 1 minute of its 9 minutes 26 s (inferred
  from 89 % in later runs).
- On runners, Garnet's parts would report after their first runs, about 15 minutes for 3 seeds of
  300 s, not after shrinks of up to 3.5 hours (inferred).

The playbook already says to judge each failure by directed runs and to shrink only the one you
report. RabbitMQ, Typesense, and Pulsar worked this way, and 11 examples used `--no-shrink`.

### Change C: first seeds on many runners by default

`hydo explore --shard` and `explore.yml` would be the default for the first round of an x86_64
image, 20 parts of 1 to 3 seeds.

- With shrinks, Dragonfly's 60 seeds took 31 minutes on 20 runners against 75 on one: a factor
  of 2.4.
- A round on runners ends with its slowest part. Dragonfly's slowest part had no failure and ran
  1,281 s of first runs, so without shrinks its round still takes about 30 minutes. With one seed
  per runner, that part would take one run, about 7 minutes (inferred from 1,281 s for three).
- Redpanda's slowest part, seeds 27 and 28, took 9,469 s with its shrinks. Its two first runs
  take 6 to 28 minutes, so with change B the round takes about 40 minutes, not 2 hours 53 minutes
  (inferred).
- The build job and the transfers set a lower bound of about 9 minutes (Redis on one runner and
  on eight).

The limits: only x86_64 images built on Linux run there, the branch must be pushed, and a private
repository pays for the minutes. An Intel and an AMD runner give different fingerprints, so each
part replays its own failures. A KVM run of Garnet took 200 to 350 s against 50 to 60 s on hvx.
So a runner is slower per run, and the gain comes from the count of runners alone. With change B,
change C cuts clock 2 most for systems whose run takes minutes, such as clusters.

### Change D: the measured first hour

The ledger proposes a first hour for every onboarding. Run the system in production's shape with
its durable settings, and make the first clean run judge something. Then explore 20 seeds with
`--strategy process --cluster-faults --no-shrink`, and judge each failure by a directed run.

- The process strategy found 60 of the 105 bugs first, and a first bug in 30 of the 40 examples
  where it ran.
- 35 of 43 positioned blind finds came within 20 schedules.
- So 20 seeds give a first bug in about 6 of 10 examples (inferred: 30 of 40 times 35 of 43).
- 3 of the 105 bugs came in the first run without faults (Pulsar, Prometheus, Ghost).

The hour fits only when a run is short. 20 runs of 10 s on 3 workers take about a minute. 20 runs
of 5 minutes on one worker take 100 minutes, and need change C. With change B the hour holds the
judging too.

### Change E: faster engines and builds

- **A client change without a rebuild.** If the workload's programs came from a folder beside the
  image, a client fix would cost no build. Medusa's 7 rebuilds of 90 s were 10 of its 23 minutes
  to the first clean run. ClickHouse's 5 builds of 4 minutes were 20 of its 50 setup minutes. NSQ
  spent 90 minutes and RabbitMQ about 2 hours on client fixes, each with new images.
- **Waits for machines.** Waits took 20 to 59 minutes in 5 records. A user on an own machine has
  none, so this cost is the records' cost more than a user's.
- **Faster runs.** A faster engine cuts clock 2 in proportion only where the runs dominate. Today
  the later runs dominate, so change B comes first. A cluster's run of 2 to 14 minutes on hvx
  sets the wall time of every exploration of that cluster.

### The changes ranked

| change | clock 1 | clock 2 | what the estimate rests on |
|---|---|---|---|
| A: onboarding without code | from 1 to 3 hours to 5 to 15 minutes, for 35 % of the bugs | none directly; removes the rules that made 21 of the 30 false findings | the stage times, the 61 generic rows sorted by workload |
| B: report first, shrink later | none | a cut by a factor of 4 to 6 of the time to a report | 6.9 later runs per first run; Lago 6 %; the trial 89 % |
| C: runners by default | none | a factor of 2.4 for Dragonfly; with change B, Redpanda's 2 hours 53 minutes become about 40 minutes | Milestone 304's three explorations |
| D: the measured first hour | none | a first bug in about 6 of 10 examples within 20 seeds | the ledger's first modes and positions |
| E: faster engines and builds | 10 to 20 minutes of rebuilds, and the client fixes' images | in proportion to run time, after change B | Medusa's and ClickHouse's rebuilds |

## Targets

- **Clock 1: a first real run in 3 commands and 15 minutes, with no file written by hand.** It
  holds for a repository with a Compose file whose services speak HTTP with an OpenAPI file or a
  route list, SQL, Redis, S3, Kafka, NATS, or AMQP. The commands are `hydo onboard`, `sh
  hydo/fetch-images.sh`, and `hydo run`. Today the records give medians of 23 and 68 minutes to a
  first clean run, about 1,200 hand-written lines, and 17 minutes to 2 hours of fixes after it.
- **Clock 2: a first reported failure with its replay command within 20 minutes of the first
  exploration's start.** It holds for a run of under 1 minute on local workers, and for a run of
  under 5 minutes on 20 runners. The report comes before any shrink. Today the report comes only
  after the shrinks. From an exploration's start to its report the records give 9 minutes (the
  0.6.0 trial) to 66 minutes (Lago) for small systems, and hours for clusters.

## The first two things to build

### 1. Report first, shrink later

It is the smaller change, and it shortens every exploration, in onboardings and in a team's CI.

- `hydo explore` writes `exploration.json` and `report.md` after the first runs and one re-run
  per failing signature, and updates them as shrinks end. A problem without a shrink shows its
  first run's schedule as the replay command, and says that it is not shrunk.
- A command shrinks one problem on demand, for example `hydo shrink <explore dir> --problem N`.
  It takes the explorer's candidates and the answers of repeated schedules (Milestone 339).
- `hydo explore --merge`, `hydo ci run` and `hydo ci accept` work with problems that are not
  shrunk. The accept key needs the failure's signature only, not its minimal schedule.
- The proof: run the ten explorations of the ledger that record their later runs again, and
  compare the time to the report and the problems found. Run each with shrinks and without them.

### 2. A generated client and the generic checks from the import

It is the change that cuts clock 1, and it gives every onboarding the generic checks.

- A guest program of drivers, as `hydo-sql` is for SQL. Each driver writes one type of item,
  records its id with `acked_outcome`, and reads it back at the end. The first drivers are HTTP
  with JSON, Redis, and S3, with `hydo sql-gen` for SQL. They reach 21 of the 32 bugs of the HTTP
  and protocol classes, and any of them reaches the 5 bugs that need any workload. NATS, AMQP,
  NSQ, and Kafka come next.
- The HTTP driver takes its operations from the project's OpenAPI file, through the reader of
  `hydo fake from-openapi`, or from `hydo rules scan --kind writes`. It reads an object back by
  the id of the write's answer, as the walk records `<METHOD>:<path>=<id>` (Milestone 273).
- `hydo onboard` chooses a driver per service from its image and ports. It writes the workload
  and the messages check with `acked_outcome`. It also writes an `eventually_` check that the
  system serves writes, the replica comparison for each group, and `errors: true`.
- The proof: run it on the Compose files of the 14 onboardings from Milestone 311 to Milestone
  371. Count which of the ledger's bugs it reaches. Compare its minutes with the phase tables.

## What the numbers cannot say

- The records time phases, not stages. The minutes of the client, the checks, and the first clean
  run are one setup phase in most records.
- The onboardings were made by agents on shared machines. No record times a person who writes
  the client, and a person's own machine has no waits from other sessions.
- The sorting of the 61 rows by workload is this page's reading of each finding's record. Some
  rows also needed faults that the first round does not draw, such as one node's power loss or
  damaged files.
- The estimates of changes B and C assume runs of equal length. A shrink candidate is often
  shorter than its first run, because it holds fewer faults, so the cut of change B is an upper
  bound.
