# The findings ledger: which techniques found what

An onboarding chooses its checks and faults from `hydo guide strategies` by judgement. This page
counts what every onboarding example found, how it found it, which check noticed it, and what it
cost. It then checks
five impressions of the lead against the numbers, and proposes a first hour for every
onboarding. Milestone 386 (tentative number) took the first hour and the default oracles into
`hydo guide workflow` ("The first hour of exploring"), `hydo/guide/strategies.md`, the playbook,
and the skill.

The ledger itself is the file `findings-ledger.csv` beside this page: one row per finding, 189
rows. A few rows hold several findings of one kind, so the rows hold 223 findings.

## How the ledger was made

Every row comes from the records only. The sources are the `RESULTS.md` page of each example under
`hydo/onboard/examples/`, the example's sections of `docs/HISTORY.md`, and, for function fuzzing,
the evaluation pages under `docs/design/`. The column `sources` of each row names its section.

- A value that the record does not give is an empty cell.
- A value that this page infers from the record carries the word `inferred`.
- An upstream answer comes from the lead's list of posted reports, last checked on 2026-11-27.
- A finding is one row when its record numbers it. A table of documented cases in a record
  becomes one row per kind, with the count in the column `n`.

The rows have three kinds, in the column `kind`:

- `system`: a finding of a real system that an onboarding ran. 47 examples, among them the
  private query engine of Milestone 330, which no tracked file names.
- `demo`: a planted bug of one of hydo's own demonstrations (`web-shop`, `checkout`,
  `openapi-payouts`, `disk-faults`, `shop`). The yields below leave these out.
- `function`: a candidate of function fuzzing (`hydo fuzz`). The yields below leave these out too.

The examples without findings of a system, such as `redis`, `compose-voting`, `python-kv` or
`service-clocks`, show a feature of hydo and have no row.

## The columns

| column | what it holds |
|---|---|
| `example`, `system` | the folder under `hydo/onboard/examples/` and the system's name and version |
| `finding`, `n` | the record's number of the finding and a short description; the count of findings in the row |
| `class` | one of the classes below |
| `novelty` | new as far as the search found, known (with the issue), by design, or documented |
| `found_how`, `exploration_or_schedule` | how the record says the finding came first, and the exploration, seed, or schedule |
| `techniques` | the entries of `hydo guide strategies` that the finding counts under |
| `missing_technique` | a technique that no entry names, where the record shows one was needed |
| `fault_kinds_as_recorded`, `fault_kinds_normalized` | the faults that the minimal schedule needs |
| `engine`, `first_found`, `cost` | the engine, the first failing schedule, and the time or schedules spent on the example |
| `upstream`, `answer` | the report and the maintainers' answer |
| `corrected` | what a later run or check changed in the record |
| `cause`, `operator_action` | the cause's kind (below), and `yes` when an operator's action in the workload was needed |
| `first_mode`, `also_blind` | the way the finding came first (below), and `yes` when a blind exploration found it later |
| `oracle`, `oracle_check` | the class of check that noticed the finding first, and the check |
| `generic_would_catch`, `generic_note` | `yes`, `no` or `unknown`: whether a check of the generic set would catch it, and why |
| `milestone`, `sources` | the onboarding's milestone and the record's sections |

The classes are those of the playbook:

- `new bug`: a bug of the system that no report names. "Cause not traced" marks a candidate that
  the record does not explain yet.
- `known bug`: a bug that an upstream issue or pull request names.
- `clock effect`: a failure that needs a step of the wall clock. The record labels it so,
  whether it breaks a promise or not.
- `documented trade-off`: a loss that a setting or a page of the system documents.
- `environment`: a failure of the environment, such as damage that the system detects or a disk
  that fails its reads.
- `mistake of the checks`: a false failure of the example's own client or checks.

Three rows are errors of hydo itself, and two rows are known bugs that the runs did not reproduce.

The causes are these letters:

- `W`: the wall clock measures a duration, a deadline, an order, or a version.
- `E`: the election, replication, or membership logic.
- `C`: a crash between two commits, or state that lives only in memory.
- `S`: an acknowledgement before the data is synced.
- `R`: a race of concurrent requests, without a fault.
- `D`: the handling of a damaged file.
- `L`: a client library or a dependency.
- `O`: another cause.

The first modes are these codes:

- `BP`: a blind exploration with the process strategy. Reactions may be part of it.
- `BS` and `BR`: a blind exploration with the swarm strategy: `BS` without reactions, `BR` with them.
- `DS`: a schedule written from the code, the documentation, or an upstream report.
- `AG`: an aimed reaction grid (`hydo explore --reactive-at`).
- `NF`: the first run without faults.
- `NP`: a native probe outside hydo.
- `EL`: an error line that the error-log scan saw for the first time.
- `WK`: the page walk.
- `TM`: a directed timing run (forced preemption points, turn lengths, a pause of a thread).
- `SE`: the same schedule on a second engine.
- `OT`: another way.

## The ledger by example

"Bugs" below means the three classes `new bug`, `known bug` and `clock effect`. The system rows
hold 105 of them, in 43 of the 47 examples. The last column counts each bug by its first mode.

| example | milestone | new | known | clock effect | trade-off | environment | mistake | bugs first found by |
|---|---|---|---|---|---|---|---|---|
| `miniflux` | M51 | 0 | 0 | 0 | 1 | 0 | 0 | no bug |
| `etcd` | M60 | 0 | 1 | 0 | 0 | 0 | 0 | blind, process strategy 1 |
| `nats-jetstream` | M141 | 1 | 4 | 0 | 1 | 0 | 0 | directed schedule 3, blind, process strategy 2 |
| `libsql-replica` | M173 | 1 | 0 | 0 | 1 | 1 | 1 | blind, process strategy 1 |
| `medusa` | M174 | 2 | 0 | 0 | 0 | 0 | 1 | blind, process strategy 1, directed schedule 1 |
| `tigerbeetle` | M176 | 0 | 1 | 0 | 0 | 1 | 1 | directed schedule 1 |
| `yugabyte` | M193 | 0 | 0 | 1 | 1 | 0 | 0 | blind, process strategy 1 |
| `ghost` | M200 | 0 | 1 | 0 | 1 | 0 | 1 | run without faults 1 |
| `nats-flaky-8363` | M205 | 0 | 1 | 0 | 0 | 0 | 0 | directed timing 1 |
| `qdrant-flaky-10200` | M208 | 0 | 1 | 0 | 0 | 0 | 0 | directed timing 1 |
| `etcd-flaky-22796` | M211 | 0 | 1 | 0 | 0 | 0 | 0 | directed schedule 1 |
| `formance-ledger` | M215 | 1 | 0 | 0 | 3 | 0 | 0 | new error kind 1 |
| `lago` | M218 | 2 | 2 | 2 | 1 | 1 | 1 | blind, process strategy 3, directed schedule 2, blind, swarm 1 |
| `blnk` | M224 | 2 | 0 | 1 | 1 | 0 | 4 | blind, process strategy 2, native probe 1 |
| `temporal` | M231 | 0 | 0 | 2 | 0 | 1 | 0 | blind, process strategy 2 |
| `valkey` | M232 | 0 | 1 | 1 | 0 | 1 | 1 | blind, process strategy 2 |
| `river` | M233 | 1 | 0 | 3 | 0 | 0 | 5 | directed schedule 2, blind, process strategy 2 |
| `etcd-upgrade` | M238 | 0 | 1 | 0 | 0 | 1 | 0 | directed schedule 1 |
| `k3s-lease` | M246 | 0 | 0 | 0 | 1 | 1 | 0 | no bug |
| `hatchet` | M250 | 1 | 2 | 0 | 0 | 0 | 0 | directed schedule 3 |
| `saleor` | M251 | 2 | 2 | 0 | 1 | 0 | 0 | blind, process strategy 2, native probe 2 |
| `memos` | M258 | 2 | 0 | 0 | 0 | 0 | 0 | page walk 2 |
| `rauc` | M259 | 0 | 0 | 0 | 0 | 1 | 0 | no bug |
| `swupdate` | M259 | 1 | 0 | 0 | 0 | 0 | 0 | blind, process strategy 1 |
| `mender` | M272 | 3 | 0 | 0 | 0 | 0 | 0 | blind, process strategy 2, other 1 |
| `vikunja` | M273 | 2 | 0 | 0 | 0 | 0 | 1 | page walk 2 |
| `surrealdb` | M277 | 1 | 0 | 2 | 0 | 1 | 0 | blind, process strategy 3 |
| `dragonfly` | M278 | 0 | 1 | 1 | 0 | 1 | 1 | blind, process strategy 2 |
| `redpanda` | M279 | 4 | 0 | 0 | 0 | 0 | 1 | blind, process strategy 3, second engine 1 |
| `tikv` | M287 | 0 | 1 | 1 | 0 | 1 | 1 | blind, process strategy 2 |
| `meilisearch` | M295 | 4 | 0 | 0 | 0 | 1 | 1 | directed schedule 3, aimed reaction grid 1 |
| `restate` | M303 | 0 | 0 | 1 | 1 | 1 | 4 | blind, process strategy 1 |
| `clickhouse` | M311 | 2 | 1 | 0 | 1 | 1 | 0 | blind, process strategy 2, other 1 |
| `victoriametrics` | M312 | 0 | 3 | 3 | 3 | 1 | 0 | blind, process strategy 5, directed schedule 1 |
| `iggy` | M314 | 0 | 0 | 1 | 1 | 2 | 1 | directed schedule 1 |
| `query-engine` | M330 | 2 | 0 | 0 | 0 | 0 | 0 | blind, process strategy 2 |
| `prometheus` | M342 | 1 | 0 | 2 | 3 | 1 | 0 | directed schedule 2, run without faults 1 |
| `greptimedb` | M343 | 1 | 0 | 0 | 0 | 1 | 1 | blind, process strategy 1 |
| `garage` | M349 | 2 | 1 | 0 | 1 | 1 | 0 | blind, process strategy 2, aimed reaction grid 1 |
| `kvrocks` | M351 | 1 | 0 | 1 | 1 | 1 | 1 | aimed reaction grid 1, blind, process strategy 1 |
| `pulsar` | M356 | 1 | 1 | 1 | 1 | 1 | 1 | blind, process strategy 2, run without faults 1 |
| `nsq` | M362 | 1 | 1 | 1 | 0 | 0 | 1 | blind, process strategy 2, directed schedule 1 |
| `seaweedfs` | M364 | 0 | 0 | 0 | 1 | 1 | 0 | no bug |
| `typesense` | M366 | 1 | 0 | 2 | 0 | 1 | 0 | blind, process strategy 3 |
| `rqlite` | M369 | 2 | 1 | 0 | 0 | 1 | 1 | blind, process strategy 1, directed schedule 1, aimed reaction grid 1 |
| `rabbitmq` | M370 | 1 | 0 | 1 | 1 | 1 | 0 | blind, process strategy 2 |
| `garnet` | M371 | 4 | 1 | 0 | 1 | 1 | 0 | blind, process strategy 4, blind, swarm with reactions 1 |

## How each bug came first

| first mode | bugs first found | examples with such a bug | schedules spent, where the records count them | examples tried |
|---|---|---|---|---|
| blind exploration, process strategy (`BP`) | 60 | 30 | 4,318 | 40 |
| directed schedule (`DS`) | 23 | 14 | not counted in most records | |
| aimed reaction grid (`AG`) | 4 | 4 | 563 | 8 |
| page walk (`WK`) | 4 | 2 | 160 | 2 |
| run without faults (`NF`) | 3 | 3 | 1 run each | |
| native probe (`NP`) | 3 | 2 | | |
| directed timing (`TM`) | 2 | 2 | 508 | 4 |
| blind exploration, swarm strategy (`BS`, `BR`) | 2 | 2 | 620 | 19 |
| another way (`EL`, `SE`, `OT`) | 4 | 4 | | |
| settings explored (`hydo explore --configs`) | 0 | 0 | 245 | 8 |

The blind process exploration found 1.4 bugs per 100 schedules, and a first bug in 30 of the 40
examples where it ran. Of the 43 blind first finds whose position the record gives, 25 came
within the first 10 schedules of their exploration, and 35 within 20. The median position is 7.

The directed schedules found 23 bugs first, each after reading code, documents, or reports. Three
came from Jepsen's reports on NATS, three from a scan of kill steps on Hatchet, and three from
reactions written after reading Meilisearch's code.

The aimed grids found 4 bugs first: rqlite's removed node, Kvrocks's full synchronization,
Garage's lost peers and Meilisearch's index deletion. On RabbitMQ (24 cells), SeaweedFS (125)
and Pulsar (15) they found nothing.

The swarm strategy found 2 bugs first in 620 schedules. Most of its failures were the
environment: a lying disk or disk I/O errors, in Valkey, TigerBeetle, Restate, Meilisearch,
Iggy, libSQL, and ClickHouse.

Settings explored with `--configs` found no bug first. They showed the documented loss of a
relaxed durability setting in 7 examples and labeled it in 5. They found RabbitMQ's finding 1
again.

The 26 onboardings whose records give a time per phase took 138 hours of wall time and found 63
bugs: 0.46 bugs per hour. SeaweedFS found none in 5.6 hours. Garnet took the longest, 14.6 hours
for 5.

## Techniques

Each finding counts under every entry of `hydo guide strategies` that it needed, so a row adds to
several lines. The last column counts the examples with at least one bug under the entry.

| technique | new | known | clock effect | trade-off | environment | mistake | examples with a bug |
|---|---|---|---|---|---|---|---|
| Clock steps | 1 | 2 | 27 | 8 | 2 | 0 | 19 |
| Kills of the process that holds acknowledged work | 10 | 11 | 5 | 6 | 0 | 0 | 20 |
| Power cuts of the whole guest | 14 | 6 | 0 | 6 | 2 | 0 | 15 |
| Domain rules | 10 | 7 | 2 | 1 | 0 | 5 | 8 |
| The messages check's claims | 9 | 2 | 2 | 2 | 0 | 1 | 8 |
| One node's storage faults | 5 | 6 | 1 | 3 | 16 | 0 | 7 |
| Reactions at console events | 11 | 1 | 0 | 0 | 0 | 0 | 9 |
| The generic rule | 5 | 3 | 4 | 4 | 0 | 4 | 9 |
| Every replica compared after a restart | 5 | 2 | 1 | 1 | 0 | 0 | 5 |
| A scan of a reaction's delay | 4 | 1 | 0 | 0 | 0 | 0 | 4 |
| Concurrent clients without faults | 3 | 1 | 0 | 1 | 0 | 0 | 4 |
| Failovers by the client | 3 | 0 | 1 | 0 | 0 | 0 | 2 |
| The page walk | 4 | 0 | 0 | 0 | 0 | 0 | 2 |
| A snapshot or a backup read back | 2 | 0 | 1 | 0 | 0 | 0 | 3 |
| History checks | 1 | 1 | 1 | 2 | 0 | 0 | 3 |
| Pauses past the failure detector | 2 | 0 | 1 | 0 | 0 | 0 | 3 |
| Power cuts at the k-th flush | 2 | 1 | 0 | 1 | 0 | 0 | 3 |
| Protocol faults | 2 | 1 | 0 | 2 | 0 | 1 | 3 |
| The features behind a flag | 3 | 0 | 0 | 0 | 0 | 0 | 3 |
| Version pairs | 0 | 3 | 0 | 0 | 0 | 0 | 3 |
| Cluster faults and group selectors | 1 | 1 | 0 | 0 | 0 | 0 | 2 |
| Directed workloads | 1 | 1 | 0 | 0 | 0 | 0 | 2 |
| Error logs as findings | 2 | 0 | 0 | 0 | 0 | 0 | 2 |
| How each unknown outcome ends | 2 | 0 | 0 | 0 | 0 | 0 | 1 |
| Pauses of the workload or a thread | 0 | 2 | 0 | 0 | 0 | 0 | 2 |
| Production's topology | 2 | 0 | 0 | 0 | 0 | 0 | 1 |
| Reboots and added disks | 2 | 0 | 0 | 0 | 0 | 0 | 2 |
| Turn lengths and preemption points | 0 | 2 | 0 | 0 | 0 | 0 | 2 |
| A graceful stop | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| A listing compared with a read | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| A second engine's timing | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| An effects file | 0 | 0 | 1 | 0 | 0 | 0 | 1 |
| Upgrades | 0 | 1 | 0 | 0 | 0 | 0 | 1 |
| Value properties | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| Settings that trade durability | 0 | 0 | 0 | 6 | 0 | 0 | 0 |

## Fault kinds

The fault kinds are those that the minimal schedule needs. "None" means that the bug needs no
fault: a concurrent workload, a graceful stop or a run of the composition alone. The columns of the
environment and the mistakes are rough, because those records seldom give a minimal schedule.

| fault kind | new | known | clock effect | trade-off | environment | mistake | examples with a bug |
|---|---|---|---|---|---|---|---|
| kill | 17 | 15 | 8 | 6 | 0 | 1 | 25 |
| clock step | 4 | 2 | 27 | 8 | 2 | 12 | 21 |
| power cut of the guest | 17 | 7 | 0 | 12 | 2 | 5 | 17 |
| pause | 8 | 6 | 4 | 0 | 0 | 2 | 12 |
| none | 7 | 4 | 0 | 2 | 1 | 12 | 11 |
| one node's power loss | 6 | 4 | 1 | 6 | 0 | 0 | 8 |
| network | 4 | 3 | 1 | 7 | 0 | 2 | 7 |
| damaged file | 4 | 2 | 0 | 0 | 16 | 0 | 4 |
| protocol fault | 2 | 1 | 0 | 1 | 0 | 0 | 3 |
| timing | 0 | 3 | 0 | 0 | 0 | 0 | 3 |
| disk errors | 2 | 0 | 0 | 2 | 10 | 1 | 2 |
| power cut at a flush | 1 | 1 | 0 | 0 | 0 | 0 | 2 |
| upgrade | 0 | 1 | 0 | 0 | 0 | 0 | 1 |

Clock steps, kills and power cuts of the whole guest each found bugs in 17 to 25 examples. One
node's damaged file found 6 bugs and 16 environment failures: the system detected the damage and
refused to start.

## The five impressions, checked

### 1. Half of the recent findings are clock effects that blind explorations found and the clock audit missed

Partly. The recent onboardings are Milestones 311 to 371, 14 examples with 40 bugs.

- A clock step was needed in 13 of the 40 (33 %), not half. Over all 105 bugs it was needed in
  32 (30 %).
- Blind explorations found 9 of the 13 first. Directed schedules found the other 4. They are
  Prometheus's two known clock bugs, VictoriaMetrics's finding 3, and Iggy's retention.
- The clock audit found none. Its outcome is recorded for 12 examples. It missed the clock bugs
  of VictoriaMetrics, Typesense, Kvrocks, Garage, Restate, and TiKV. It named no place at all in
  Redpanda, GreptimeDB, rqlite, and RabbitMQ.
- The audit named the function of Prometheus's finding 2 second, a known bug. Its three
  schedules on ClickHouse passed.

### 2. A third are acknowledgements before a sync, with one node's power loss

No. An acknowledgement before a sync caused 6 of the 40 recent bugs (15 %), and 14 of all 105
(13 %). One node's power loss was needed in 2 of the 6, RabbitMQ's finding 1 and Kvrocks's
finding 1. With the documented trade-offs counted, the share of the recent findings rises to 15 of 54
(28 %), mostly power cuts under a relaxed setting. Over all bugs the causes are these:

| cause | bugs | share |
|---|---|---|
| the wall clock (`W`) | 29 | 28 % |
| election, replication, or membership (`E`, also with `S`) | 22 | 21 % |
| a crash between two commits, or state in memory (`C`) | 17 | 16 % |
| an acknowledgement before a sync (`S`, also with `E`) | 14 | 13 % |
| another cause (`O`) | 11 | 10 % |
| a race without a fault (`R`) | 8 | 8 % |
| a damaged file (`D`) | 5 | 5 % |
| a library or a dependency (`L`) | 2 | 2 % |

The rows with `S` and `E` count in both lines, so the shares add to more than 100 %.

### 3. The rest is an operator's action next to a fault

No. An operator's action in the workload was needed in 8 of the 40 recent bugs (20 %) and in 14
of all 105 (13 %). The actions were these:

- failovers and slot migrations in Garnet;
- a node's removal and a graceful stop in rqlite;
- a failover that forced a full synchronization in Kvrocks, and `garage node connect`;
- in earlier examples, installs and commits of an update (Mender, SWUpdate), an upgrade (etcd),
  and a second producer's `InitProducerId` (Redpanda).

The larger part of the rest is the election and replication logic under plain kills, pauses, and
power cuts, and crash windows between two commits.

### 4. The cost went to shrinks and long explorations

Yes. Ten explorations record both their first runs and their later runs, the re-runs and shrink
candidates. They ran a median of 6.9 later runs per first run, from 1.9 (Ghost) to 16.9 (Restate).
Lago's process exploration ran 20 schedules and 318 later runs. Explorations in 11 examples ran
with `--no-shrink`. Explorations in 5 more were stopped during their shrinks: those of
Typesense, TiKV, Garnet, Temporal, and Saleor. In 13 onboardings from Milestone 287 on, the
explorations and the judging took a median of 58 % of the wall time of the phase tables. A run
of a system of several nodes took 2 to 14 minutes (Redpanda, Pulsar, Hatchet, Garnet on KVM).

### 5. Records were often corrected at verification

Partly. Seven findings were checked again with a released hydo from a fresh clone. They are
Typesense 1 to 3, RabbitMQ 1 and 3, and Garnet 1 and 2. Two of the seven were corrected in
substance: the first records of RabbitMQ's findings 1 and 3 described the trigger wrongly. The other five held,
and two of them gained detail. Other records changed in these ways:

- A label changed. TiKV's finding 1 became the known pingcap/tidb#44822, and ClickHouse's
  Keeper loop was first read as hvx's slow guest clock.
- A saved schedule stopped failing when the image or the engine changed. This happened to
  Meilisearch's swap (the delay `+14`) and Saleor's gift card (`cf=270` became `cf=271`). It
  also happened to Blnk's overdraft, to Mender's commit (7 ms on the Mac, 69 to 87 ms on hvx),
  and to etcd's issue 14685 at four epochs.
- An upstream answer changed the class. River's maintainers keep the unlogged leader table by
  design, and Redpanda closed finding 1 because Redpanda does not tolerate damaged data.

## Techniques that found nothing over many tries

| technique | tries | what it found |
|---|---|---|
| the clock audit (`hydo rules scan --kind clock`) | 12 examples with a recorded outcome | no bug; its 3 schedules on ClickHouse passed |
| the scan of write routes (`--kind writes`) | 6 examples with a recorded outcome | no bug; no route in rqlite, Typesense, or SeaweedFS; in Kvrocks and Garage only after their fixes |
| the swarm strategy | 620 schedules, 19 examples | 2 bugs first; mostly disk faults |
| timing and forced preemption points on services | 90 schedules, 3 examples | nothing first; they found Blnk's finding 1 again |
| settings explored (`--configs`) | 245 schedules, 8 examples | no bug; a documented loss in 7, labeled in 5 |
| aimed reaction grids, in 3 of 8 examples | 164 cells | nothing in RabbitMQ, SeaweedFS, and Pulsar |
| function contracts in a whole-system run | 24 schedules, Blnk | nothing new |
| version pairs aimed at a known bug | 2 bugs | neither reproduced (YugabyteDB TA-30772, etcd 20418) |
| the steered and the state search | the strategies page | no gain over the plain exploration |

Function fuzzing, measured apart, found 8 candidates in 25 crates and reproduced 12 known
bugs. The page walk found 4 small bugs of the pages in Memos and Vikunja in 160 schedules.

## Proposal: the first hour of an onboarding

The order follows the yields of the preceding sections. Each step names when it applies and why.

1. **Run the system in production's shape, with its durable settings.** Applies to every system
   with replicas or durability settings. Under a relaxed setting every power cut gives the
   documented loss, as in Kvrocks, Garage, SeaweedFS, Iggy, and ClickHouse.
2. **Make the first run without faults judge something.** Write the read-back checks first: the
   generic rule or the messages check with `acked_outcome`, every replica against its leader,
   and a backup read back. 11 bugs needed no fault, and 3 showed in the first run (Pulsar,
   Prometheus, Ghost).
3. **Explore 20 seeds with `--strategy process`, `--cluster-faults` and `--no-shrink`.** Keep
   clock steps, kills and power cuts in `faults:`. Add `--storage-faults` for a system whose
   nodes have storage of their own. This mode found 60 of 105 bugs first. 35 of the 43 blind
   first finds with a position came within 20 schedules.
4. **Judge each failure by a directed run, and shrink only the one you report.** Shrinks took a
   median of 6.9 runs per first run. RabbitMQ, Typesense, and Pulsar judged their failures by
   directed runs instead.
5. **Put the operator's actions into the workload.** Applies to a system with failovers,
   migrations, node removal, graceful stops, or upgrades. 14 bugs needed such an action.
6. **Then aim a reaction grid at the acknowledgement events.** Applies when a crash window
   between an answer and a commit or a sync is plausible. It found 4 bugs first in 8 examples.

Move these later, after the first hour:

- The clock audit and the scan of write routes: they found no bug. Read their lists for the
  workload, not for schedules.
- `--configs`: run it to label a documented loss, not to find bugs.
- The swarm strategy: 2 bugs first in 620 schedules, and most failures were disk faults.
- The timing explorations, contracts, version pairs, and the steered and state searches: none
  found a bug first.
- Damaged files (`G=`): 16 of their 22 failures were the environment. Draw them after the first
  blind round, without shrinks.

## The oracles: which check noticed each finding

A finding needs a trigger, the faults of the preceding sections, and an oracle, the check that
noticed it. The column `oracle` of the CSV names the class of check that failed first in the
finding's record, and `oracle_check` names the check itself. The column `generic_would_catch`
says whether a check of the generic set would have caught the finding too: `yes`, `no`, or
`unknown`, with the reason in `generic_note`. The generic set is these checks:

- a read-back of every acknowledged write with its final outcome (`acked_outcome`, or the
  generic rule over the client's records);
- every replica compared with the leader after the heal;
- a check that the system serves writes after the heal;
- the error-log scan;
- a history checker where the workload has transactions.

hydo's own verdicts count with the generic set: a service that does not start again, a
`TIMEOUT`, and an `ERROR` need no check of the project.

| oracle | new | known | clock effect | trade-off | environment | false findings | examples that declare it, at least | false share |
|---|---|---|---|---|---|---|---|---|
| business rule over stored state | 14 | 8 | 10 | 4 | 1 | 21 | 22 | 36 % |
| acknowledged writes read back (the generic rule) | 7 | 7 | 5 | 11 | 1 | 2 | 32 | 6 % |
| liveness: an `eventually_` check or serves writes | 4 | 0 | 6 | 0 | 1 | 1 | 20 | 8 % |
| replicas compared with the leader | 5 | 4 | 1 | 1 | 0 | 1 | 11 | 8 % |
| hydo's verdict without a check | 4 | 3 | 2 | 0 | 23 | 1 | every run | 3 % |
| messages check, `no-loss` | 4 | 0 | 2 | 3 | 0 | 0 | 19 (any claim) | 0 % |
| messages check, `acked-outcome` | 2 | 2 | 0 | 3 | 0 | 2 | 19 (any claim) | 22 % |
| messages check, `duplicates` or another claim | 1 | 0 | 0 | 1 | 0 | 1 | 19 (any claim) | 33 % |
| history checker | 1 | 1 | 1 | 2 | 0 | 0 | 7 | 0 % |
| page invariant | 3 | 0 | 0 | 0 | 0 | 0 | 2 | 0 % |
| the test's own assertion (flaky tests) | 0 | 3 | 0 | 0 | 0 | 0 | 3 | 0 % |
| a backup or snapshot read back | 2 | 0 | 0 | 0 | 1 | 0 | 2 | 0 % |
| error-log scan | 1 | 0 | 0 | 0 | 0 | 0 | 15 | 0 % |
| the workload's own assertion | 0 | 1 | 0 | 0 | 0 | 1 | 2 | 50 % |
| none: reading or a native probe | 1 | 0 | 0 | 2 | 0 | 0 | | |
| value property (`keep`) | 0 | 0 | 0 | 0 | 0 | 0 | 1 (Blnk) | 0 % |
| contract | 0 | 0 | 0 | 0 | 0 | 0 | 1 (Blnk) | |

The column of declarations comes from a search of each example's folder for the check's marks,
such as `type: messages`, `errors: true` or a file `eventually_*`. A rule inside a client program
does not show there, so the counts are lower bounds; the business rules count the examples with a
row instead. The false share is the false findings divided by all rows of the oracle.

### What the generic set would have caught

- Of the 105 bugs, a generic check would have caught 63 (60 %), and missed 33. For 9 the record
  does not say.
- Of the 40 recent bugs (Milestones 311 to 371), it would have caught 29 (73 %), and missed 9.
- By class it would have caught 29 of 49 new bugs, 19 of 29 known bugs, and 15 of 27 clock
  effects.

Only a project-specific oracle, a business rule, a value property or a contract, caught 19 bugs
in 13 examples: 8 new bugs, 6 known bugs, and 5 clock effects. For 8 more the record does not say
whether a generic check would have seen them. The 19 break promises that are not "an
acknowledged write stays":

- at most once, or once per period, in River's periodic run, Hatchet's scheduled run, Ghost's
  donation, Saleor's two refunds, and Medusa's two refunds;
- a limit or a book that must match, in Blnk's overdraft, Saleor's single-use code, and Medusa's
  capture;
- a device that boots an allowed version, in Mender's two findings and SWUpdate's;
- a listing, an expiry, a retention, or a version order. These are Garage's listing, Valkey's
  expiry, Iggy's retention, and SurrealDB's changefeed. They are also Prometheus's scrape times,
  River's two leaders and skipped runs, and Hatchet's cancelled run.

14 more bugs needed another check outside the generic set and the project's rules:

- availability during the faults (Typesense, twice), `WAIT` (Kvrocks), and a query that never
  ends (the query engine);
- two snapshot read-backs (Prometheus, Meilisearch) and three page invariants (Memos, Vikunja);
- three flaky tests' own assertions;
- the Keeper loop that only CPU use showed, and two builds of mender-artifact that differ.

### The false findings

The examples recorded 30 false findings. Business rules made 21 of them (70 %), in Blnk, Lago,
River, Restate, Valkey, Dragonfly, Medusa, Ghost, Iggy, GreptimeDB, and Meilisearch. By the records, most were a
wait that did not cover the system's own recovery, or a rule stricter than the system's promise
after a clock step. The read-back of acknowledged writes made 2, and the messages check's claims
3.

### Blnk: rules, contracts, and value properties side by side

Blnk ran every class of oracle against one system (Milestones 224, 260, 285 and 291).

| oracle | finding 1, overdraft | finding 2, atomic bulk | finding 3, lost commit | finding 4, clock | runs and false findings |
|---|---|---|---|---|---|
| seven domain rules (1.1 to 1.7) | rule 1.6 alone; blind in 2 of 45 hvx schedules | rule 1.7 | no | no | 85 explored schedules; 4 false findings of the rules and the wait |
| the generic rule (rule 2) | no: it cannot know a draw should be refused | yes | yes, alone | yes, alone | the same runs |
| the error-log scan | no | its lines, beside the rules | no | no | the same runs |
| contracts on three functions | the overdraft contract broke in the same 3 of 24 schedules as the periodic rule; the end-state rule saw 1 | the bulk contract broke on the fixed build too, so no oracle | no | no | 24 schedules; nothing new |
| a value property (`keep`) | fails the run without faults; broke in 9 of 30 blind schedules, first at seed 4 | no | no | no | the overdraft rule broke in 1 of the same 30, at seed 21; the fixed build holds |

The value history of Milestone 285 named the cause of finding 1, the limit lost in the worker's
copy, but it is not an oracle. The value property found finding 1 nine times as often as the
rule, and with no fault. The contracts found nothing that the rules missed, and no contract could
state finding 2 or the cause of finding 1.

### Proposal: the default oracles

Make these the default of every onboarding, before any project-specific rule:

1. **A read-back of every acknowledged write with its final outcome** (`acked_outcome`, or the
   generic rule). It caught 29 bugs first, with a false share of 6 to 22 %.
2. **Every replica compared with the leader after the heal**, for a system with replicas. It
   caught 10 bugs first, with 1 false finding.
3. **An `eventually_` check that the system serves writes after the heal.** The liveness checks
   caught 10 bugs first, 6 of them clock effects; 4 of the 10 needed a narrower check.
4. **The error-log scan.** It caught Formance's bug alone, and it costs one line of `hydo.yaml`.
5. **A history checker where the workload has transactions.** It caught 3 bugs first, with no
   false finding.

Together with hydo's own verdicts, these would have caught 63 of 105 bugs, and 29 of the 40
recent ones.

Write project-specific oracles when they pay:

- **Business rules** pay when the promise is at most once, once per period, a money limit, a
  device's version, or a listing. They caught 19 bugs that no generic check would see. They also
  made 70 % of the false findings, so review each rule's wait and its behaviour under a clock
  step before an exploration.
- **A value property** pays when a value must reach a decision unchanged across functions or
  processes, as Blnk's limit. It found the bug with no fault, nine times as often as the rule.
- **Contracts in a whole-system run** did not pay on Blnk. Keep contracts for function fuzzing,
  which found 8 candidates in 25 crates.
- **A page invariant** pays for a web app: it caught 3 bugs that no server-side check saw.

## What the ledger cannot say

- The records count schedules, not machine hours. The yields per hour use wall time from the
  records' phase tables, which other sessions on the same machine stretched.
- The directed schedules have no count in most records, so their yield per schedule is unknown.
- The techniques of a finding are this page's reading of its record. The column `techniques`
  shows each one, so a reader can check it.
- A finding found blind once may need a narrow window. A first mode of `BP` does not say that a
  second exploration finds it again.
