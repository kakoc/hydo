# YugabyteDB, three nodes: results

The first onboarding of the second round (user decision, 2026-11-08): a distributed SQL database
onboarded end to end the way an outside user would, to measure whether the fixes of the first
round (M173 libSQL, M174 Medusa, M176 TigerBeetle) reduced the friction, and to find what is
still missing. The system is **YugabyteDB 2025.2.8.1**, the newest release of the 2025.2 stable
series on 2026-11-08. It runs as three nodes with replication factor 3: each node is one
`yb-master` and one `yb-tserver`, and every tserver serves YSQL, PostgreSQL's wire protocol. The
image is the official `yugabytedb/yugabyte:2025.2.8.1-b8`.

The starting point was a Compose file written the way the YugabyteDB documentation's manual
deployment runs a cluster. After that, the work used only the onboarding skill's process,
`hydo guide`, and the `hydo` command built from this tree: a Linux release tarball built with
`hydo/dist/linux/build-release.sh` and installed with `install.sh --no-module`. Everything ran
on **the bare-metal machine with the hvx engine** (the main module, which takes 8 GiB guests
since 2026-11-08), one VM of this work at a time.

**Why YugabyteDB and not CockroachDB.** The task named YugabyteDB first, and CockroachDB only if
YugabyteDB did not fit in 8 GiB of guest RAM. It fits easily. In these workloads the three
masters, the three tservers and their PostgreSQL processes use less than 0.5 GiB of the guest's
8 GiB (the guest's `free -m` at the end of a run, read with `hydo inspect`). The image tree is
0.9 GB, and `root.share_images` keeps one copy for all six services. Size was not the problem;
the section "Getting it to run" says what was.

## The short answers

- **Does it run?** Yes, after eight runs and two hours (one hour and 38 minutes of work, the
  rest waiting for the shared machine). Every schedule then gave the same fingerprint twice, and
  a fresh boot with the whole workload takes about 75 seconds of wall time.
- **What blocked it?** One hydo bug (the image builder silently dropped most of the image's
  base layer), one gap in hvx's virtual CPU (no cache sizes, which crashes glibc 2.28's
  `memcpy`), a readiness deadlock (hydo starts services one after the other), and two settings
  YugabyteDB needs in a small, freshly started guest (its free-disk floor, and a clock that does
  not start before the release was built). The diagnosis of M178 named the failing process and
  its status at once in four of the six failed runs; `hydo inspect` explained the other two
  without changing the image.
- **What was found?** (1) YugabyteDB's default `durable_wal_write=false` loses acknowledged
  writes at a power cut of the whole cluster: an increment, a set element, a register write and
  list appends, each lost at another cut point, each found by a different generated check; with
  `--durable_wal_write=true` the same schedules pass. This is the documented behaviour of the
  default ("recommended only for multi-AZ or multi-region deployments"); what makes it worth a
  line is that the default is what a single-site Compose file gets. (2) A backward step of the
  whole cluster's clock by 60 s stops the tservers ("Too big clock skew is detected"), and they
  cannot start again until the clock has caught up. (3) Two hydo bugs, both FIXED here: the
  image builder's tar reader (above) and the power loss of one service (`L=`), which failed for
  every image service with a false VIOLATION. No isolation anomaly and no lost write without a
  power cut was seen.
- **The exploration** (20 schedules of the process strategy with every fault kind, 96 minutes):
  finding 1 again in four schedules, shrunk to one cut at the 4th flush; finding 2 in eleven
  (every schedule with a backward clock step); two false findings that hydo caused (the recovery
  clock after a cut at the k-th flush, friction 19); no other failure.
- **The known bug by version pair** (TA-30772, a transaction of more than 100,000 records
  partly undone after a restart): not reproduced. A user-table version of its trigger passes on
  2025.2.2.2 (before the fix) and on 2025.2.3.0 alike (section "The known bug by version pair").

## What was hunted

Sources: Jepsen's analyses of YugabyteDB 1.1.9 (YCQL, 2019) and 1.3.1 (YSQL, 2019), YugabyteDB's
technical advisories (docs.yugabyte.com, "Technical advisories", read 2026-11-08) and its
documentation of the WAL settings. What hydo's faults reach (process kills and pauses, network
faults between nodes, a power cut of the whole cluster, a power loss or file corruption of one
node, clock steps of the whole guest; not clock skew between nodes, since every node reads the one
guest clock):

| finding | versions | trigger | reachable here? |
|---|---|---|---|
| `durable_wal_write=false` (the default): WAL synced once a second or every MB, "recommended only for multi-AZ or multi-region deployments ... For single AZ deployments, this flag should be set to true" | every release | a power loss of every replica at once | yes: `C=` (the whole guest), the hunt below |
| the same setting, a power loss of one node: what it acknowledged in its last second is gone from its log | every release | `L=` of one node, plus a second failure before the others replicate | yes, `L=` with a kill or a pause; hunted below |
| TA-30772: a transaction of more than 100,000 storage records, a restart before its rows are flushed, a later update, a flush and a compaction: the later update is undone | before 2025.2.3.0 (2024.2.9.0, 2025.1.4.0) | a restart of the nodes holding the rows; no other fault | yes: a power cut restarts every node; hunted by version pair (2025.2.2.2 against 2025.2.3.0) |
| Jepsen #2125: G2-item cycles in YSQL | 1.3.1, fixed in a later build of 1.3.1 | all masters crashing at once, or master pauses; about once in 100 hours of testing | in principle (reactions kill several services at once), not hunted: the releases are seven years old and the rate is too low |
| Jepsen #2182: counter reads below acknowledged increments | 1.3.1 | clock skew between nodes beyond `max_clock_skew_usec` | no: one guest clock, no skew between nodes |
| Jepsen #1992: recovery from partitions takes 100+ s | 1.3.1 | partitions | partly: `N=` on links; recovery time is not checked here |
| TA-31688: missing or spurious rows from multi-key lookups longer than 64 bytes | 2025.2.0.0 to 2025.2.3.0 | a query shape (an IN list with a key longer than 64 bytes after shorter ones), no fault | a workload could, but it needs no fault; not hunted |
| TA-32220, TA-30653, TA-30104: index inconsistencies | 2024.2 to 2025.2.4 | DML shapes, no fault | not hunted (no fault involved) |

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | three `yb-master` and three `yb-tserver` services of `yugabytedb/yugabyte:2025.2.8.1-b8`, written as the documentation's manual deployment starts a cluster: every master lists all three, RF 3, YSQL on every tserver, a named volume each, the healthchecks a user would write (`curl` of a master's web page, `pg_isready` of a tserver), `restart: unless-stopped` |
| `hydo/hydo.yaml` | from `hydo compose`; the hand changes are marked `hand:` and listed at its top (one image tree for six services, the masters' port, 8 GiB and `cpu: v3`, readiness that does not wait for the cluster, `GLIBC_TUNABLES`, YugabyteDB's free-disk floor, the guest clock's start and its continuation after a power cut, a file system of its own per service for `G=` and `L=`); `params: wal` is `--durable_wal_write` (false, the default; a restart is needed to change it, so it is a build parameter and not a knob) |
| `hydo/fetch-images.sh`, `hydo/hydo-compose.lock.json` | from `hydo compose`: the image by digest |
| `hydo/workload/`, `hydo/checks/` | the output of `hydo sql-gen --dialect postgres --workloads bank,list-append,register,set,counter --endpoint "n1=postgres://yugabyte@10.89.0.14:5433/yugabyte?sslmode=disable" --endpoint "n2=...10.89.0.15..." --endpoint "n3=...10.89.0.16..."` (three primaries; every command picks its node from the seed); by hand only `workload/first_00_wait_ysql` (waits until all three tservers answer YSQL) and the retry of the two final read-backs in `checks/list_append_sql.la` and `checks/linearizable_sql.rg` |
| `bigtxn/hydo/` | the large-transaction test of issue 30772 (below): the same cluster with the release as a build parameter (`yb`), one first command and one check |

```sh
sh hydo/fetch-images.sh                       # 0.4 GB, by digest (network, once)
hydo build hydo/hydo.yaml                     # durable_wal_write=false
hydo build hydo/hydo.yaml --set wal=true --out .hydo-wal
hydo run "disk=none dseed=1 | run"
hydo explore --seeds 1-20 --strategy process --more-faults --storage-faults --workers 1
```

`hydo build` stages the guest program `hydo-sql` from the release's `guest/hydo-sql`, which the
Linux release does not ship yet (friction 2): build it with `hydo/crates/hydo-sql/build.sh
x86_64 <dir>` and set `HYDO_SQL_BIN=<dir>/hydo-sql`.

The images built from this folder: `hydo build hydo/hydo.yaml` sha256 `1afe802f…` (3.8 GB
disk image: a 1.4 GiB root, a 256 MiB data disk and six file systems of 192 to 512 MiB), with
`--set wal=true` `0f4d61e0…`.

## Getting it to run: eight runs, and what explained each failure

| run | verdict | what hydo said | what it was | how it was found | fix |
|---|---|---|---|---|---|
| 0 | (no run) | `hydo: hydo: No such file or directory (os error 2)` | `hydo run` reads the image's files with `debugfs` from PATH; this PATH had no /usr/sbin | `strace` of `hydo` | /usr/sbin on PATH; FIXED: the message names debugfs |
| 1 | ERROR service start (yb_master_1) | diagnosis: exit status 127, log `/sbin/tini: not found` | the dynamic loader `/lib64/ld-linux-x86-64.so.2` was missing from the image tree: hydo-build's tar reader stopped the image's base layer at a PAX header with a binary value (a file capability) and left out 8,615 of its 9,188 entries, silently | `hydo inspect exec` at the end of the run (38 s), then the layer listings and `strace` of `hydo build` | FIXED in hydo-build |
| 2 | ERROR service start (yb_master_1) | diagnosis: exit status 139; the kernel's lines: `segfault ... in libc-2.28.so` for yb-master and curl, with the faulting AVX instructions | hvx's virtual CPU reports no cache sizes (CPUID leaf 4 empty); the image's glibc 2.28 (AlmaLinux 8) then uses a non-temporal copy threshold of 0, and its memcpy fails | the diagnosis's kernel lines; `hydo inspect exec`: no `/sys/devices/system/cpu/cpu0/cache` | `GLIBC_TUNABLES` with ordinary cache sizes in every service (not fixed in hydo) |
| 3 | ERROR service start (yb_tserver_1) | diagnosis: the readiness check (a YSQL query, written by hand) did not pass in 1200 s | hydo starts the services one after the other and waits for each one's readiness; YSQL needs the transaction table, which needs two live tservers, so the first tserver waits for the second, which is never started | an `hydo inspect` session at the end: the master's log "Not enough live tablet servers ... Need at least 2" | readiness back to a local check |
| 4 | ERROR service start (yb_tserver_1) | diagnosis: `pg_isready` (the Compose healthcheck) answers "no response" for 120 s | the same deadlock: a tserver's PostgreSQL accepts connections only once YSQL works | the diagnosis's one run of the check | readiness = the tserver's web server; the wait for YSQL moved into the first command |
| 5 | ERROR provision failed | diagnosis: the provisioning command failed (status 1); the waiting command was ended at its 1500 s | "Not enough disk space for bootstrap": YugabyteDB keeps 3 GB free by default and refuses writes and replica copies with less; each tserver's file system is 512 MiB | an `hydo inspect` session: the tserver's own log files under its data directory (the service's stdout has only tini's warning) | `--reject_writes_min_disk_space_mb=32` |
| 6 | ERROR provision failed | the same | the masters could not read the catalog versions ("Could not get YSQL db catalog versions", 4,663 times): the cluster starts its catalog from a snapshot made when the release was built (2026-11-02), whose hybrid times are in the future of hydo's default guest clock (2026-01-01) | an `hydo inspect` session: the masters' logs; the cause inferred from the release date | `guest.clock_start` 2026-11-08 |
| 7 | OK | | | | |
| (cut) | VIOLATION service cannot recover (yb_tserver_3) | diagnosis: "a check failed ... No process failed" | the recovery boot's clock starts at `clock_start` again, a minute behind the hybrid times on disk: "Too big clock skew is detected: 58.980s" (FATAL) | `hydo inspect events` (`yb_tserver_3 DIED`), a live session in the recovery boot: the FATAL log | `guest.clock_after_cut: 60` |

## Runs and verdicts

Every schedule ran twice from a fresh boot (`hydo run`), and both runs gave the same fingerprint
(`fp`) and control transcript (`ctl`). The verdict is the run's whole verdict (`hydo: VERDICT`,
the guest's checks and the host's history checks). A run takes 1.2 to 2.5 minutes of wall time
on the shared machine (the engine's own time, `secs=` in the result line, 12 to 18 s), and the
host process of an 8 GiB guest holds 15.4 GiB (its maximum resident set).

### `durable_wal_write=false` (YugabyteDB's default), image `1afe802f…`

| schedule | verdict | fp | ctl |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK: 81 commands, 77 ok, 4 failed (compare-and-sets whose expected value was not there, definite); `sql.la` 25 operations strict-serializable; `sql.rg` linearizable | `8f7618f3e08a3485` | `aea94b061a69d2f1` |
| `disk=cut dseed=1 \| run C=8` (power cut of the whole cluster) | **VIOLATION** [sql_set] n1: 1 of 4 acknowledged elements missing; also `sql.la` lost-append: T5 appended 1300 and was acknowledged, but T6, which began after that, read [100,301] without it | `086f686c474bb4f8` | `263d79c7ef53269e` |
| `disk=cut dseed=1 \| run C=12` | **VIOLATION** [sql_counter] n1: counter 1 < 2 acknowledged increments; also `sql.la` lost-append: T7 appended 2101 ... read [100,301,1300,1801] without it | `6620196fd74d07c6` | `7e40b6752ce807b8` |
| `disk=cut dseed=1 \| run C=16` | **VIOLATION** [sql.rg] key 5: an acknowledged write is lost or a read is stale: p29 write 5=2 (ok, t=74.54-74.59s); p9997 read 5 -> nil (ok, recovery boot) | `b75a03c991330339` | `81e69135e6189221` |
| `disk=cut dseed=1 \| run C=20` | OK (nothing acknowledged in the last second before the cut) | `a61eae303cfe451e` | `7455a67fec928897` |
| `disk=none dseed=1 \| run K=yb_tserver_1@10+5` (kill a tserver) | OK, 69 ok, 12 failed while its node was down | `1aced59cd60e092d` | `3432f4b13f9c0416` |
| `disk=none dseed=1 \| run K=yb_master_1@10+5` (kill a master) | OK | `094188b20ef27497` | `ba2517563b4504e8` |
| `disk=none dseed=1 \| run P=yb_tserver_2@10+6` (pause a tserver) | OK, 67 ok, 14 failed | `2eda74f7c503546a` | `814c6b33410e4a41` |
| `disk=none dseed=1 \| run N=yb_tserver_1-yb_tserver_2@10-20:loss,100%` (cut a link) | OK, 64 ok, 17 failed | `d9f968a4cc542ef0` | `3b30f46625232579` |
| `disk=none dseed=1 \| run T=10:-60` (the clock 60 s back) | ERROR restart after the faults (yb_tserver_1): yb_tserver_1 and yb_tserver_3 stop with "Too big clock skew is detected: 45.467s, while max allowed is: 0.500s" and cannot start until the clock has caught up | `5287ef9b01dd36ac` | `9bd2d738f53bbf5e` |
| `disk=none dseed=1 \| run T=10:3600` (the clock an hour forward) | OK | `5fcc3ec145f81936` | `784eb6de01e36351` |
| `disk=none dseed=1 \| run L=yb_tserver_1@10+5` (power loss of one tserver), hydo as merged | VIOLATION yb_tserver_1 storage unmountable after its power loss (mount: ... Resource busy): a hydo bug, FIXED | `c3acbe8fea2a7289` | `433edc373f71a9db` |
| the same, hydo with the fix | OK, 69 ok, 12 failed | `085115c573bc8566` | `6a9d888353fa97fd` |
| `disk=none dseed=1 \| run P=yb_tserver_2@8+10 L=yb_tserver_1@10+2` (a tserver paused, then another loses power) | OK, 67 ok, 14 failed | `67f2feb96626d537` | `a4b92022936c09f1` |
| `disk=none dseed=1 \| run G=yb_tserver_1@10+5:flip,n=8` (8 bit flips in a tablet's consensus metadata of a stopped tserver) | OK | `e5b5f4613ca63672` | `91cd451a5cb67fc8` |

### `durable_wal_write=true` (`--set wal=true`), image `0f4d61e0…`

| schedule | verdict | fp | ctl |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK | `717c891c3421a4b2` | `5cd0153496d6be5d` |
| `disk=cut dseed=1 \| run C=8` | OK | `03ba9085871ad89a` | `6f0b35b6bbdccf58` |
| `disk=cut dseed=1 \| run C=12` | OK | `ef225e3ff1b26026` | `80d6f559bf5c1ee3` |
| `disk=cut dseed=1 \| run C=16` | OK | `6408ec59e6beee29` | `d2b7ee273accd20e` |
| `disk=cut dseed=1 \| run C=20` | OK | `bec78dfd8567b506` | `3f4019f699cfaee2` |

## What was found

### 1. The default WAL setting loses acknowledged writes at a power cut of the whole cluster

With `durable_wal_write=false`, YugabyteDB's default, a tserver syncs its write-ahead log once a
second or once a megabyte, and a write is acknowledged when a majority of replicas has it, not
necessarily on disk. A power cut of every node at once (hydo's `C=`: the whole guest) loses what
was acknowledged in the last second. Three of four cut points lost acknowledged data (the table
above), each caught by a different check that `hydo sql-gen` wrote: the set read-back, the
counter read-back and the register history; the list-append history named a lost append in two
of them. With `--durable_wal_write=true` the same four schedules pass. YugabyteDB documents
this: the default "is recommended only for multi-AZ or multi-region deployments where the
availability zones (AZs) or regions are independent failure domains and there is not a risk of
correlated power loss. For single AZ deployments, this flag should be set to true." A Compose
file on one host, or three nodes in one rack, is such a single failure domain and gets the
default. As with NATS's `sync_interval`, sqld's `durability_mode` and Redis's `appendfsync
everysec`, the finding is the default value, not a bug.

The power loss of one node (`L=`: its own file system loses what it had not written back) lost
nothing, alone or with a second tserver paused around it: the other replicas still had every
acknowledged write.

### 2. A backward clock step stops the tservers until the clock catches up

`T=10:-60` steps the guest's clock 60 s back (every node's: there is one guest clock). The
tservers that then receive a hybrid time from before the step stop with a FATAL ("Too big clock
skew is detected: 45.467s, while max allowed is: 0.500s"), and their restart fails the same way
until the clock has passed the last hybrid time they saw, so the run ends `ERROR restart after
the faults (yb_tserver_1)`. No data was lost and no check failed. YugabyteDB chooses to stop
rather than serve what it cannot order (its `max_clock_skew_usec` and
`clock_skew_force_crash_bound_usec` flags); an NTP step backwards of a whole site's clocks is
where that shows. A step of an hour forward passes. In the exploration below every schedule with
a backward step (from 30 s to 30 days) ended in such an ERROR, except one that also had a power
cut and ended in the lost write of finding 1.

### 3. Not found

No isolation anomaly in any run (strict-serializable list-append transactions over three
primaries, batch and interactive; linearizable registers), no negative balance and no change of
the bank's total, under kills of a tserver or a master, a pause of a tserver, a cut link between
two tservers, clock steps, bit flips in a stopped tserver's tablet metadata, and the power loss
of one tserver. The only lost writes were those of finding 1.

## The known bug by version pair: issue 30772 (not reproduced)

Technical advisory TA-30772 (fixed in 2025.2.3.0, 2025.1.4.0 and 2024.2.9.0): a transaction of
more than 100,000 storage records is applied in batches of 100,000. When a node restarts before
the rows are flushed, its replay of the log applies only the first batch and leaves the rest as
intents; a later update of those rows, then a flush and a compaction, can make the old intents
the newest version again, so the later update is undone. The advisory saw it in the system
catalog after a full-database `ANALYZE`; nothing in it limits it to the catalog.

`bigtxn/hydo/` builds the trigger for a user table: a first command inserts 120,000 rows, flushes
the table (so they are packed rows on disk) and updates every row in one transaction (120,000
records); the schedule's power cut (`C=6`) restarts every node before anything flushes those
rows; the check, in the recovery boot, updates 100 rows to a newer value, flushes and compacts
the table with `yb-admin`, and requires the newer value on all 100. Each run twice, the same
fingerprint:

| release | schedule | verdict | fp | ctl |
|---|---|---|---|---|
| 2025.2.2.2 (before the fix) | `disk=none dseed=1 \| run` | OK | `9352a740e9ead170` | `5998443a8b0125ff` |
| 2025.2.2.2 | `disk=cut dseed=1 \| run C=6` | OK: the newer update kept on 100 of 100 rows | `c77bb3cabd8aaef7` | `380a980c1e17feb9` |
| 2025.2.3.0 (the fix) | `disk=none dseed=1 \| run` | OK | `fa0584c46ede6606` | `e91ec47a22d01866` |
| 2025.2.3.0 | `disk=cut dseed=1 \| run C=6` | OK | `1370dd00152806d7` | `a91abfbb93421c6e` |

The release before the fix passes too, so this is not a reproduction. Why the trigger did not
fire was not established. The candidates: the update may be stored as whole packed rows rather
than per-column updates (the advisory's merge needs per-column updates of packed rows); the
transaction's rows may have been applied and flushed before the cut (then the restart replays
nothing of it); or the replay of a user table differs from the catalog's. (The table is
hash-sharded, so the rows a replay would leave behind are not the last ids; but with one in six
of the rows left behind, all 100 checked rows escaping is improbable.) The next step: read the
tserver's log in the recovery boot with `hydo inspect` for the apply of the large transaction
during bootstrap, and check all 120,000 rows instead of 100. Hunting this bug needs no fault
but a restart at the right moment, which is what hydo makes repeatable; the version pair is
here for whoever takes it further.

## Explorations

`hydo explore --image .hydo/explore-ob-yugabyte-disk.img --seeds 1-20 --strategy process
--more-faults --storage-faults --workers 1` on the image of `durable_wal_write=false`, 10:48 to
12:24 (96 minutes; one 8 GiB worker, which booted in 47 s; a run from its snapshot took 23 s on
average, 12.8 runs an hour). The explorer warned that one worker may need 33 GB and this machine
had 29.5 GB available; it ran. The process strategy drew kills, pauses, link and one-way faults
between masters and tservers, clock steps, power cuts by round (`C=`) and at the k-th flush
(`cf=`, measured range 1 to 569), file corruptions (`G=`) and power losses of one service (`L=`).
17 of the 20 schedules failed, all 17 reproduced with the same fingerprint, and the explorer
shrank every violation (56 to 86 shrink runs each):

| seeds | verdict | what it is |
|---|---|---|
| 2, 9, 11 | OK | no backward clock step and no power cut |
| 1, 12, 17, 20 | VIOLATION: an acknowledged list append or increment lost (`sql.la`, `sql_counter`) | finding 1: a power cut with the default WAL setting. Minimal schedules: `disk=cut dseed=72b4b958fb4479ee cf=4 \| run W=86f364de,p2,q2,r250` (the 4th flush; "counter 0 < 1 acknowledged increments", `003f6141ef9fa37d` twice) and `disk=cut dseed=a8a48df0db54e975 \| run C=40 W=219dc827,p3,q3,r1000,oparallel_driver_sql_itransfer` (`66ea33ea4d53b260` twice); seed 1's shrank to a schedule that still has a link fault, a pause, a clock step forward and a power loss beside its `C=40` (`fdc49b63b59819b9` twice) |
| 3, 4, 6, 7, 10, 13, 14, 15, 16, 18, 19 | ERROR: a tserver or master does not restart (`restart failed`, `restart after the faults`, `restart failed (after its power loss)`, `(its files were corrupted)`) | every one of these schedules steps the clock back (by 30 s to 30 days): finding 2, the tservers stop on the clock-skew check, and any restart before the clock has caught up fails, whatever else the schedule did. The explorer does not shrink an ERROR |
| 5, 8 | VIOLATION service cannot recover (committed data unrecoverable) | false findings: power cuts at the k-th flush, whose recovery boot set the clock to `clock_start` + 60 s (friction 19). Both shrank to a flush cut alone: `disk=cut dseed=629bfb2777cb6c08 cf=139 \| run W=340bcd65,p1,q3,r1000,oparallel_driver_sql_counter_add+parallel_driver_sql_itransfer+parallel_driver_sql_itxn+parallel_driver_sql_register` (`1cbf96fcd6bb5986`, the same from a fresh boot with `hydo run`, where an `hydo inspect` call in its recovery boot read the tserver's FATAL: "Too big clock skew is detected: 1.109s, while max allowed is: 0.500s") and `disk=cut dseed=978180e49ee2ae24 cf=270 \| run W=3cc7df19,p2,q2,r500,oparallel_driver_sql_counter_add+parallel_driver_sql_itransfer` (`730d1682887f93d9`) |

Every schedule with a backward clock step ended in an ERROR, except seed 12 (`T=36:-30`, then a
cut at the 64th flush), which ended in the lost increment of finding 1; no schedule without a
backward step ended in an ERROR. Seeds 13, 15 and 18 also changed a file of a stopped service
(seed 13: `CORRUPTED yb_tserver_2's .../tablet-33fc2f40.../CURRENT before step 10: cut to 10
bytes`); with a clock step in the same schedule, whether the service refused to start because of
the file or because of the clock was not separated. No schedule showed an isolation anomaly, and
no schedule lost a write without a power cut. The report names the class of a failure after its
schedule's faults (friction 20): 10 failures are `corrupted-files` (two of them, seeds 5 and 17,
shrank to schedules without a file change), the other 7 `durable-defaults`.

## Time

Wall time on 2026-11-08 (the bare-metal machine's clock, UTC), from the first look at the
first round's results:

| phase | from | to | took |
|---|---|---|---|
| read the first round's results, the skill, `hydo guide`; YugabyteDB's documentation, technical advisories and Jepsen's analyses; chose the release | 06:43 | 06:50 | 7 min |
| a release from this tree (the stale hydo-world pin, then a second build), installed with `--no-module`; the Compose file, `hydo compose`, the image fetched (7 s), `hydo sql-gen`, `hydo build --check` OK | 06:50 | 06:56 | 6 min |
| the first `hydo build` refused (the disk over 4 GiB), the second built (1.5 min) | 06:56 | 06:58 | 2 min |
| waiting: the release work held the machine quiet | 06:59 | 07:20 | 21 min |
| run 0 (debugfs), run 1 (`/sbin/tini: not found`), the image builder's tar bug found, fixed with a test, programs rebuilt | 07:20 | 07:42 | 22 min |
| run 2 (glibc and CPUID), runs 3 and 4 (the readiness deadlock), the debugfs message fixed | 07:44 | 08:03 | 19 min |
| runs 5 and 6 (11 and 7 minutes each, with their diagnosis replays), two `hydo inspect` sessions (5 and 3 minutes to start): the disk floor and the clock start | 08:05 | 08:41 | 36 min |
| **first green run** (run 7, OK; twice the same fingerprint at 08:58) | | 08:42 | **119 min from the start, 98 min of work** |
| waiting: quiet again | 08:43 | 08:57 | 14 min |
| the first power cut: a false finding (the recovery boot's clock), explained in a live `hydo inspect` session in the recovery boot, `clock_after_cut` | 08:58 | 09:08 | 10 min |
| **first finding** (`C=12`: an acknowledged increment lost, twice the same fingerprint) | | 09:16 | **153 min from the start, 118 min of work** |
| the read-backs after a power cut, the final folder; every schedule twice on both images (from 09:26 to 09:40 the machine was quiet again); the power-loss bug of `L=` found, fixed with a test, verified | 09:16 | 10:45 | 89 min |
| the version-pair runs of issue 30772 | 10:26 | 10:43 | 17 min |
| the exploration (20 schedules, 17 failures reproduced, five violations shrunk), and one of its false findings confirmed with `hydo run` and `hydo inspect` | 10:48 | 12:35 | 107 min |

## Friction log: what an outside user needed and where they looked

Each item says what was needed, where it was looked for, and what was done. Items marked FIXED
were changed in hydo on this branch, each in its own commit with a test.

1. **A release from this tree.** hydo 0.2.0 is not published, so `hydo setup --get` has nothing
   to fetch, and the way in was again `hydo/dist/linux/build-release.sh` plus `install.sh
   --no-module`, as for TigerBeetle. The script now stopped earlier: its check of the guest
   programs against their pins failed, because `guest/hydo-world` built from `main` is
   `cd417796…` while `hydo/dist/linux/guest-programs.sha256` and docs/BUILD-PINS.md still pin
   `bdafadb3…` (hydo-world's sources changed in Milestones 180 and 181, and the pin did not move).
   The pin was changed in the copy on the bare-metal machine only, and the release built. Not
   fixed: moving the pin is a release decision (images with fakes get new bytes), and the
   release work of 0.3.0 was changing the same files at the same time. The last step, the
   sources archive, still stops for want of the git-ignored machines file, after the tarball is
   written (TigerBeetle's item 1).
2. **`hydo sql-gen` needs a program the release does not ship.** The generated commands call
   `hydo-sql`, and `hydo build` stages it from `guest/hydo-sql` of the release, which the Linux
   release does not contain: "HYDO_SQL_BIN not found at <prefix>/guest/hydo-sql (set
   HYDO_SQL_BIN)". It was built from source (`hydo/crates/hydo-sql/build.sh`) and named with
   `HYDO_SQL_BIN`. A user of a release could not do that without Rust. Neither `hydo guide
   workloads` nor `hydo guide capabilities` mentions it.
3. **A run that could not read its image.** `hydo run` ended at once with `hydo: hydo: No such
   file or directory (os error 2)`. `strace` showed that `hydo` reads `/etc/hydo/control` and the
   manifest out of the image with `debugfs` from PATH, and this PATH had no `/usr/sbin`; the
   release has its own `debugfs` in `libexec/bin`, which `hydo explore` puts on PATH and `hydo
   run` does not. FIXED (message): the error now says that debugfs (e2fsprogs) is not on PATH and
   where it usually is.
4. **A Docker image with a file capability.** The first run ended `ERROR service start
   (yb_master_1)`; the diagnosis said at once what happened (exit status 127, `/sbin/tini: not
   found`) although `/sbin/tini` was in the image. `hydo inspect exec` at the end of the run
   showed that the dynamic loader was missing, and the image tree on disk lacked 8,615 of the
   9,188 entries of the image's base layer. Cause: hydo-build's tar reader failed on a PAX
   extended header whose value is not UTF-8 (Docker writes `SCHILY.xattr.security.capability`
   for programs such as `newuidmap`), and the reader ends an archive silently at the first bad
   member, so the rest of the layer was left out without a word. FIXED (commit "hydo-build: a PAX
   value that is not UTF-8 no longer ends an image layer"): decoded as Python's tarfile does,
   with a unit test; none of the 13 image files of the repository's examples on the bare-metal
   machine has such a value, so no recorded image changes. This is the "silent pass" kind of bug
   M184 hunted: the build said OK.
5. **CPUID without cache sizes.** The next run ended with `yb-master` and `curl` killed by
   SIGSEGV. The diagnosis's kernel lines named the place (`segfault ... in libc-2.28.so`, with the
   faulting AVX instructions); `hydo inspect exec` showed no `/sys/devices/system/cpu/cpu0/cache`.
   hvx's virtual CPU answers CPUID leaf 4 (the cache description) with nothing, and glibc 2.28
   (AlmaLinux 8, the base of YugabyteDB's image; also Red Hat UBI 8 and Rocky 8) then computes a
   non-temporal copy threshold of 0 and its `memcpy` fails. Worked around with `GLIBC_TUNABLES`
   giving ordinary cache sizes in every service. Not fixed: answering leaf 4 changes the
   module's CPUID and so possibly the recorded fingerprints; it is open item material.
6. **`guest.cpu: v3` by disassembly.** Whether the image needs AVX was decided by counting `ymm`
   registers in `objdump -d` of `yb-server` (20,780); the image's documentation says nothing, and
   a run at v2 would have cost a boot to learn it (TigerBeetle's item 6, unchanged).
7. **Readiness that waits for the cluster.** hydo starts services one after the other and starts
   the next only when the previous one is ready; Docker Compose starts them all and only reports
   their health (unless a `depends_on` asks for `service_healthy`). The Compose healthcheck of a
   tserver (`pg_isready`), imported as its readiness, therefore deadlocked: YSQL answers only
   once the transaction table has replicas on two tservers, and the second tserver is started
   only after the first is ready. Two runs went to it (one with a YSQL query as readiness, one
   with the imported `pg_isready`); the diagnosis said what the check printed ("no response")
   and an `hydo inspect` session showed the master's "Not enough live tablet servers ... Need at
   least 2". The fix was a local readiness (the tserver's web server) and a first command that
   waits for YSQL on all three. Neither the importer nor the services guide says that readiness
   gates the next service's start, which a clustered system's healthcheck often cannot pass.
8. **A provisioning failure without a cause.** Two runs ended `ERROR provision failed` after the
   waiting command's 1500 s. The diagnosis had the provisioning command's status but no cause:
   the evidence was each service's `/tmp/hydo/<svc>.log`, which holds only tini's warning,
   because YugabyteDB writes its own logs into its data directory (`/mnt/tserver/yb-data/.../
   logs/yb-tserver.INFO`). `hydo inspect` sessions found both causes without an image change
   (items 9 and 10), each session costing one replay of the run (about 3 to 5 minutes).
9. **YugabyteDB's free-space floor.** "Not enough disk space for bootstrap": YugabyteDB refuses
   writes and replica copies when less than 3 GB is free (`--reject_writes_min_disk_space_mb`),
   and each tserver's own file system here is 512 MiB (hydo's whole disk image stays under 4 GiB).
   Set to 32 MB. A system property, but hydo's 4 GiB disk makes it likely for any storage engine
   that reserves space.
10. **A guest clock before the release.** The masters could not read the catalog versions
    ("Could not get YSQL db catalog versions for heartbeat response", 4,663 times). The release
    starts its catalog from a snapshot made when it was built (2026-11-02), and hydo's default
    guest clock starts at 2026-01-01, ten months earlier, so the snapshot's hybrid times are in
    the guest's future. `guest.clock_start` set to 2026-11-08. The cause was inferred (the logs
    say only that the versions cannot be read); the guide's hazards page lists clock jumps, not a
    start date that precedes the software.
11. **The recovery boot's clock goes back.** The first power cut ended `VIOLATION service cannot
    recover (committed data unrecoverable) (yb_tserver_3)`, twice with one fingerprint, a
    convincing false finding. The diagnosis said "a check failed ... No process failed", which
    was wrong: `hydo inspect events` showed `yb_tserver_3 DIED` at its start in the recovery boot,
    and a live session in the recovery boot read its FATAL log: "Too big clock skew is detected:
    58.980s, while max allowed is: 0.500s". The recovery boot starts the clock at
    `guest.clock_start` again, a minute behind the hybrid times written before the cut.
    `guest.clock_after_cut: 60` fixed it. The guide mentions the setting for checks that compare
    times; a database that stores hybrid timestamps needs it too, and a real power loss never
    turns a machine's clock back.
12. **The final read-back right after a power cut.** On the first image, the power cut at round
    12 lost an acknowledged increment (finding 1), and the run's list-append and register
    histories were reported as errors: "its final read-back exited with status 2, so the history
    is incomplete". The generated read-back (`hydo-sql list-append read-all`) tries once, and
    right after the recovery boot the cluster is still electing tablet leaders. hydo did not
    pass it silently (M184's rule held). The two check files now retry the read-back up to ten
    times, three seconds apart (a failed attempt is recorded as a failed read, which the
    checkers ignore), and `test.command_timeout` is 90 s. Not fixed in `hydo-sql`: its bytes are
    pinned and inside recorded images.
13. **The generated comment.** With three endpoints that are all primaries, sql-gen's
    `list_append_sql.la` says "one primary serves them all"; the claim it makes
    (strict-serializable) is right for three primaries too. The comment was corrected by hand.
14. **The imported Compose file.** `hydo compose` gave each master the image's first exposed port
    (5433, YSQL's) as `port:`, kept the image's two declared volumes `/mnt/disk0` and
    `/mnt/disk1` for all six services (twelve unused directories), still names the deleted
    Python tools in its header ("hvx/onboard/hydo-compose", "HYDO_SEEDRNG=... hvx/onboard/
    hydo-build"; open item 38 d), and did not set `root.share_images` for six services of one
    image: without it the image tree would have been unpacked six times, beyond the disk's 4 GiB.
    `root.share_images` is in no guide topic; it was known from AGENTS.md. `fetch-images.sh`
    still fetches the arm64 image on Linux (libSQL's item 3).
15. **The disk's size.** The first build refused the image: "the disk with the services' own file
    systems is 6663 MiB, more than the 4096 MiB an engine maps". `storage: {size_mib}` comes on top
    of `data.size_mib`, which MANIFEST.md says and the services guide does not; the message was
    clear, and one rebuild (1.5 minutes) fixed it.
16. **`hydo run --help`** still prints "no image given ..." (TigerBeetle's item 15).
17. **The power loss of one image service.** `L=yb_tserver_1@10+5` ended `VIOLATION yb_tserver_1
   storage unmountable after its power loss (mount: mounting /dev/vda8 on /data/vol/tserver-1
   failed: Resource busy)`, twice with one fingerprint: a false finding about the system. The
   power loss unmounted the service's directory, but an image service also has the directory
   bound into its root (`mounts:`), and that bind kept the shut-down file system alive. M183 had
   proved `L=` on a service without an image (TigerBeetle from `root.files`). FIXED (commit
   "hydo-control: the power loss of an image service (L=) unmounts its binds too"): every mount
   of the device goes and the launcher binds the new one at the restart; a test on the scripted
   agent, and the same schedule passes on the bare-metal machine.
18. **The links the explorer cuts.** The exploration's fault space lists the links between each
   tserver and each master (the tservers `depends_on` the masters) and none between two tservers
   or two masters, although the tservers replicate every write to each other and the masters
   form their own Raft group: the explorer draws link faults only between services that list
   each other in `depends_on`, and peers of one cluster cannot list each other (it would be a
   cycle). Such links can be cut by hand (`N=yb_tserver_1-yb_tserver_2@10-20:loss,100%` above);
   the explorer never draws them, and nothing says so.
19. **`clock_after_cut` after a cut at the k-th flush.** With `guest.clock_after_cut`, the
    recovery boot sets its clock to the first boot's clock at the cut plus the given seconds. For a
    power cut of the schedule (`C=`) the controller reads the guest's clock at the cut; for a cut
    the engine makes at the k-th disk flush (`cf=`, which `--more-faults` draws) it never does, and
    the recovery boot falls back to `clock_start` ("recovery boot: realtime clock set to
    2026-11-08T00:01:00Z (first boot at the cut: 1794096000, +60s)"), minutes behind the hybrid
    times on disk. A tserver then stops with the clock-skew FATAL of friction 11, and the run ends
    `VIOLATION service cannot recover (committed data unrecoverable) (yb_tserver_3)`: a false
    finding, in two of the exploration's first nine schedules. Not fixed: the controller would have
    to carry the guest's clock (or compute it from the guest time of the last stop and the clock
    steps it made), which changes what every transcript with `cf=` and `clock_after_cut` holds.
20. **The class of a failure.** The explorer names a failure's class after the faults its schedule
    contains: every failure of a schedule with a `G=` was `corrupted-files`, also when the cause
    was a clock step back (seeds 3, 4 and 6: `T=28:-86400`, `T=16:-2592000`, `T=23:-30`), and
    every failure of a schedule with a power cut and no knob was `durable-defaults`.
21. **The shared machine.** The release work of 0.3.0 held `/var/tmp/hydo-QUIET` three times
    while this work ran: 06:55 to 07:20, 08:43 to 08:57 and from 09:26; every run of this work
    waited for it (`run.sh` waits at most 90 minutes, then goes on).

## What hydo lacked, ranked by how much it blocked

1. **An image builder that never drops files silently.** The worst failure of the onboarding was
   a build that said OK and left most of the image's base layer out (friction 4, FIXED). M184
   removed silent passes from schedules and checks; the builder had one left. A count of the
   entries each layer added, compared with the layer's own list, would have caught it.
2. **A virtual CPU that describes its caches.** hvx answers CPUID leaf 4 with nothing, which
   breaks glibc 2.28's `memcpy` (friction 5): every image based on AlmaLinux 8, Rocky 8 or Red Hat
   UBI 8 (glibc 2.28 is still common in enterprise images) needs a `GLIBC_TUNABLES` line nobody
   would guess. It cost one run here only because the diagnosis printed the kernel's segfault
   lines; without them it would have looked like a broken image.
3. **Readiness that does not deadlock a cluster.** hydo waits for each service's readiness
   before it starts the next (friction 7), which Docker Compose does not; a clustered system's
   healthcheck often needs its peers. Two runs went to it. Either the importer should not turn a
   healthcheck into a gate when services depend on each other's start, or hydo should be able to
   start services without waiting (and wait for all of them together).
4. **Service logs that are not on stdout.** The diagnosis reads `/tmp/hydo/<svc>.log`, the
   service's output; databases write their real logs into their data directories (YugabyteDB's
   `yb-data/*/logs`, PostgreSQL's `log/`, MySQL's error log). Two provisioning failures had no
   cause in the diagnosis and cost an `hydo inspect` session each (friction 8). A per-service
   `logs:` list (files the diagnosis and `hydo inspect logs` read) would have put the causes in
   the first answer.
5. **A recovery boot whose clock continues.** The default restarts the clock at `clock_start`
   after a power cut (friction 11), which no real machine does, and a database with hybrid
   clocks refuses to start: a convincing, reproducible false finding. With
   `guest.clock_after_cut` it still happens after a cut at the k-th flush (`cf=`), whose clock the
   controller never reads (friction 19): two more false findings in the exploration. `guest.clock_after_cut`
   exists; it is opt-in only because the default would move recorded values (as M184 did for
   `outcomes_after_cut`). And the diagnosis of a service that does not come back in the recovery
   boot said "No process failed".
6. **The release itself:** `hydo-sql` not shipped (friction 2), a stale pin that stops the
   release build (friction 1), `hydo run` without the release's own `debugfs` (friction 3).
7. **A guest clock that starts after the software was built** (friction 10): `clock_start`'s
   default, 2026-01-01, now precedes software released since; a default derived from the image
   (the newest file time of its layers, for example) would avoid it, or the hazards page could
   say so.
8. **The explorer's links between peers** (friction 18): replication traffic between the nodes of
   one cluster is never cut by the explorer, only by hand; a `peers:` list (or links drawn
   between services of one image) would put it in the fault space.
9. **Room on the disk.** One 4 GiB disk for the root, every service's data and every own file
   system: YugabyteDB's 3 GB free-space floor had to be lowered (friction 9), and the image with
   `wal=true` came within 260 MB of the limit.

## Comparison with the first round

**Earlier friction that no longer happened:**

| first round | what it was | here |
|---|---|---|
| TigerBeetle 7, item 1 of "What hydo lacked" | three replicas did not fit in 3 GiB | gone (M179): 8 GiB, YugabyteDB uses under 0.5 GiB of it |
| TigerBeetle 8, Medusa 2 and 12, item 1 of Medusa's "What hydo lacked" | "ERROR provision failed" / "service start" with no cause; each cause cost a rebuild with a debug wrapper | mostly gone (M178): the diagnosis named the process, its status and its last log lines in four of six failures (127, a segfault with the kernel's lines, a readiness check's own output twice); the other two were found with `hydo inspect` sessions, **with no image change made to find a cause** |
| Medusa 12, item 3 of Medusa's "What hydo lacked" | the state behind a verdict needed a debug image, whose other timing lost the reproduction | gone (M178): a live `hydo inspect` session in the recovery boot read the FATAL log behind a false VIOLATION |
| libSQL 10, item 3 of libSQL's "What hydo lacked" | 868 lines of hand-written SQL client | gone (M182): `hydo sql-gen` wrote 16 files; a user wrote one 15-line wait command and a retry loop around two read-backs |
| libSQL 11 | which consistency to claim; the first power cuts passed because `serializable` cannot see a lost write | gone (M182): the generator claimed strict-serializable for three primaries, and the power cuts that lost writes were caught by four different generated checks |
| libSQL 6 and 7, item 1 of libSQL's "What hydo lacked" | an exploration of another system, unknown service names passing silently | not hit: every schedule here named the image's services, and the explorer read the image's own fault space |
| libSQL 8 | the `RESULT` line omits the host's history checks | gone (M184): `hydo: VERDICT` carries the whole verdict |
| libSQL 12 | which read saw the loss | improved: the register checker's witness says "(recovery boot)" |
| libSQL 3 (half) | an image unpacked twice for two services | gone with `root.share_images` (M181), which six services of one image needed; but nothing told a user about it (friction 14) |
| TigerBeetle 9 | `$HYDO_OUTCOMES` empty after a power cut, a false "everything lost" | not hit: the generated checks read `$HYDO_ACKED`, which is carried |
| TigerBeetle 4 | a client inside an image service's namespace only | not hit: `hydo-sql` runs from the host side of the bridge |

**Earlier friction that happened again:** a release had to be built from the tree (TigerBeetle 1;
now also stopped by a stale pin), `hydo compose` still fetches the arm64 image on Linux and
still names the deleted Python tools (libSQL 3, NATS's open item), `hydo run --help` (TigerBeetle
15), deciding `guest.cpu: v3` by disassembly (TigerBeetle 6), and the shared machine's quiet
periods (TigerBeetle 16: 35 minutes before the first finding here, and 14 more later).

**New friction:**

- the image builder's dropped layer (a hydo bug, FIXED);
- the CPUID cache leaves (hvx);
- the readiness deadlock, and service logs outside stdout;
- the recovery boot's clock as a false finding, with the default and again after `cf=` cuts;
- the guest clock's start before the release;
- `hydo-sql` missing from the release;
- the power loss of an image service (a hydo bug, FIXED);
- the explorer's link space without peer links, and failure classes named after the tokens.

**Times.** To the first green run: libSQL 16 minutes (two services, the KVM engine on the build
machine), Medusa 23 minutes, TigerBeetle 85 minutes (25 of work, one replica), YugabyteDB 119
minutes (98 of work, six services). To the first finding: libSQL 35 minutes (the durability
finding at 50), Medusa 36 minutes by hand and 45 blind, TigerBeetle none (a false one at 93
minutes), YugabyteDB 153 minutes (118 of work; a false one at 139). The time went to things the
first round's fixes do not touch: a builder bug, hvx's CPUID, and four properties of a
distributed database starting cold in a small guest (cluster formation behind readiness gates,
its free-disk floor, its catalog snapshot's timestamps, its hybrid clock after a power cut).
What the fixes did change shows in the cost per failure: every failed run was explained from
the run itself (the diagnosis, then `hydo inspect`), never by adding a debug command to the
image, and the workload and its checks took one command instead of a client.
