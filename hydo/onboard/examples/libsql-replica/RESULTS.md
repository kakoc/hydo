# libSQL server (sqld), a primary and a read replica: results

A third-party SQL system onboarded end to end the way an outside user would (after NATS
JetStream, `../nats-jetstream/RESULTS.md`): libSQL's server `sqld` (the Turso family's SQLite fork with a network server and
replication), one primary and one read replica, from a `docker-compose.yaml` written as a libSQL
user writes one, then only the onboarding skill's process, `hydo guide`, and the `hydo` command
built from this tree (`33355f5`, plus the two fixes below). Everything ran on **the build machine
with the KVM engine** (`--backend kvm`: stock Linux KVM, `/dev/kvm` as the login user, no kernel
module, no root), the path of an ordinary Linux user without hydo's module. The guest files came
from the next release's Linux tarball installed with `install.sh --no-module` into a private
prefix; this tree's `hydo`, `hydo-kvm`, `hydo-build` and `hydo-fetch-image` replaced the
tarball's programs there.

The central question of this run: how does a user test an SQL system's logic with hydo without
writing everything by hand? The answer here is a seeded SQL workload generator, `sqlgen`
(`client/`, about 800 lines of Rust), that the example had to bring itself; what hydo would have
to offer instead is in "What a ready-made SQL workload generator needs".

## Why sqld with a replica, and not `turso`

Both are from the Turso family. `sqld` is a server: clients speak HTTP (Hrana over HTTP,
`POST /v2/pipeline`), a replica serves reads from its own copy and forwards writes to the primary
over gRPC, and the project ships a Docker image, so a primary and a replica are a two-service
Compose file, which is what hydo is for (several services, faults between them, a power cut of the
whole system). `turso` (the Rust rewrite of SQLite) is an in-process library and a shell, with no
server and no replication to fault: its onboarding would be an embedded-database project
(`services: []`, the workload running the shell), which tests hydo far less. The image is
`ghcr.io/tursodatabase/libsql-server:v0.24.33` (the newest `v0.24` tag on 2026-11-07), pinned by
digest (`sha256:756fb4d3…` for amd64).

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | `primary` (`SQLD_NODE=primary`, HTTP 8080, gRPC 5001) and `replica` (`SQLD_NODE=replica`, `SQLD_PRIMARY_URL=http://primary:5001`), a named volume each on `/var/lib/sqld`, healthchecks, `restart: unless-stopped` |
| `client/` | `sqlgen`, the seeded SQL workload (static x86_64 musl Rust, `client/build.sh`; only dependency the repository's Rust SDK for the history records) |
| `hydo/hydo.yaml` | from `hydo compose`, hand changes marked `hand:` |
| `hydo/workload/first_schema` | the schema and 8 accounts of 100 on the primary; waits until the replica serves them |
| `hydo/workload/parallel_driver_transfer` | a bank transfer as one transaction sent as one Hrana batch (BEGIN; debit if the balance allows; credit only if the debit changed a row; COMMIT, else ROLLBACK), to the primary or the replica as the seed chooses |
| `hydo/workload/parallel_driver_itransfer` | the same transfer as an interactive transaction, the way a client library's `transaction()` runs one: one statement per HTTP request in one Hrana stream, the balances read, the new values computed by the client and written as values (a lost update changes the bank's total) |
| `hydo/workload/parallel_driver_txn`, `parallel_driver_itxn` | an Elle-style list-append transaction (1 to 4 appends of unique elements and whole-list reads over 6 keys, `lists(k, v)` with `v` a comma-separated list), as one batch or interactively, recorded as hydo history records |
| `hydo/checks/always_bank_total_live` | every 2 s while the workload runs: on the primary and on the replica, 8 accounts, the total 800, no negative balance |
| `hydo/checks/final_bank_total_after` | the same on the primary after the faults (and in the recovery boot after a power cut) |
| `hydo/checks/list_append_sql.la` | history `la`, every transaction on either node, checked by hydo's list-append checker as `serializable` (a replica may serve stale reads, so no real-time order is claimed) |
| `hydo/checks/list_append_sql.lap` | history `lap`, the transactions sent to the primary and only the appends of those sent to the replica, checked as `strict-serializable` (an acknowledged append missing from a later read is a lost write, also across a power cut) |
| `hydo/checks/eventually_replica_converges` | a differential check: within 120 s of guest time after the heal, the replica holds exactly the primary's rows (both tables dumped as JSON lines on each node, compared with `hydo-check equal`) |
| knob `D` (`hydo.yaml`) | sqld's `durability_mode` (`strong` = `PRAGMA synchronous=FULL`, the image's value; `relaxed` = NORMAL, sqld's default), set through sqld's admin API by `/etc/sqld-durability.sh`, which the primary's `ready:` runs after every start |

Build and run (the build machine, KVM engine):

```sh
sh client/build.sh                          # Rust 1.90.0 with the x86_64 musl target
sh hydo/fetch-images.sh                     # the image by digest (amd64 and arm64)
hydo build hydo/hydo.yaml                   # .hydo/explore-ob-libsql-replica-disk.img
hydo run --backend kvm "disk=none | run"
hydo explore --backend kvm --seeds 1-100 --strategy process --new-faults --workers 2
```

The image (the final folder, built on the build machine with this tree's `hydo`): sha256
`f0a83d03…`. Every schedule below ran twice from a fresh boot on the KVM engine with the same
fingerprint (`fp`) and control transcript (`ctl`) both times; the verdict is the run's whole
verdict from `result.json` (the guest's checks and the host's history checks).

## What was found

### 1. sqld's default durability loses acknowledged transactions at a power cut

sqld runs each connection with `PRAGMA synchronous` set from its per-database `durability_mode`,
whose default is `relaxed` (`NORMAL`; libsql-server `src/connection/config.rs` at
`libsql-server-v0.24.33`). In WAL mode `NORMAL` does not sync a commit; the WAL reaches the disk
at a checkpoint (sqld's default checkpoint interval is an hour). So every transaction since the
last checkpoint is acknowledged and then lost at a power cut. hydo saw it once the history check
on the primary claimed real-time order (friction 11): a single list-append transaction, a power
cut 57 rounds (8.6 s of guest time) later, and the recovered primary reads every list empty.

| schedule | `D` | verdict | fp | ctl |
|---|---|---|---|---|
| `disk=none \| run` | strong | OK (117 commands; `la` and `lap` no anomaly) | `ed38f60a208c07e1` | `3065a70d6f7aa337` |
| `disk=cut \| run C=12` | strong | OK | `407e8763e1a87215` | `2a2f47cf40ac5287` |
| `disk=cut \| run C=12 D=relaxed` | relaxed | VIOLATION [sql.lap] lost-append: 0: T7 appended 81 and was acknowledged, but T15, which began after that, read [] without it | `34da9bd30009d4fe` | `fa9d4aabf95f7e35` |
| `disk=cut dseed=72b4b958fb4479ee \| run C=60 W=:p1,r150,cparallel_driver_txn,5.3.0.1001779105` (one transaction) | strong | OK (3 operations) | `3ff11e8d0c5b10fd` | `a8b155fd60d7a919` |

The last schedule is the explorer's minimal one from an exploration of an image that left
sqld's default in place and claimed `strict-serializable` for every transaction (60 process
schedules, 16 failures, all reproduced: 16 of the 17 schedules with a power cut, none of the 43
without one): there it was `VIOLATION [sql.la]
lost-append: 2: T0 appended 52 and was acknowledged, but T1, which began after that, read []
without it` (`24c3555ac7844875` twice). As for NATS's `sync_interval` and Redis's `appendfsync
everysec`, this is the documented behaviour of a setting (SQLite documents that a WAL commit with
`synchronous=NORMAL` may roll back after a power loss); what makes it worth a line is that it is
sqld's default and that sqld's documentation of `durability_mode` is a field in the admin API.

### 2. No isolation anomaly, and no stale read was seen

Checked as `serializable`, every transaction on both nodes, including interactive ones that read
on the replica and write through it, passed in every exploration: no aborted read, no duplicated
element, no dependency cycle, and the bank's total never changed on either node, also not in the
interactive transfers that write computed values (a lost update would change it). Even claimed
as `strict-serializable` for every transaction (the variant of finding 1), none of the 43
schedules without a power cut failed, among them pauses of the replica and lossy or one-way
links to it: the replica's reads, which libSQL documents as possibly stale, were never seen
stale in these workloads.

### 3. sqld forgets its durability setting when it restarts

With `durability_mode` set to `strong` once (through the admin API, before the workload), the
process strategy still found acknowledged transactions lost at a power cut: 4 of 100 schedules,
3 signatures, all reproduced, every minimal schedule a kill of the primary followed by a power
cut (from seed 49: `disk=cut dseed=894a9113a6ef9318 | run K=primary@24+1 C=60 W=:p1,r150,...`,
`VIOLATION [sql.lap] lost-append: 0: T12 appended 263 and was acknowledged, but T14, which began
after that, read [10,140,222] without it`, `c0b2e4ada71292ca` twice). A check printing the admin
API's answer at the end of the run showed why: after the kill and restart the primary's
configuration is the default again (`"txn_timeout_s":5,"durability_mode":"relaxed"`), without a
kill it is `strong`. The same happens outside hydo: in Docker on the build machine, sqld
v0.24.33 with `SQLD_ADMIN_LISTEN_ADDR`, `POST /v1/namespaces/default/config` with
`"durability_mode":"strong"` reads back `strong` (also 5 s later), then after `docker kill -s
KILL` and `docker start` reads `relaxed`, and after a clean `docker stop` and `start` too. So a
durability setting made through the admin API lasts until the next restart of the server, for
the default namespace (namespaces were not enabled); whether that is intended we could not tell
from the documentation.

The example works around it: the primary's `ready:` command sets the mode after every start.

| schedule | image | verdict | fp | ctl |
|---|---|---|---|---|
| `disk=cut \| run K=primary@10+2 C=20` | mode set once (before the workaround) | VIOLATION [sql.lap] lost-append: 0: T18 appended 362 and was acknowledged, but T20, which began after that, read [121] without it | `c2542f6a19cc2bb8` | `92dfdaaccae82b0a` |
| the same | set after every start (final) | OK (21 operations) | `14de44ec5a815d97` | `81335c5bf7eb504e` |
| seed 49's minimal schedule (above) | set once | VIOLATION … T12 appended 263 … read [10,140,222] without it | `00a2f1a0b7df7119` | `fb64900980bfbaaf` |
| the same | final | OK (18 operations) | `aaec348b0b4dd3cf` | `5b36b883195f9368` |

(On the first image, `hydo run` from a fresh boot gives `00a2f1a0b7df7119` and the explorer's
worker, which starts every run from its snapshot S1, gives `c0b2e4ada71292ca`, each every time:
friction 19.)

The image "set once" is the final folder with two differences: `first_schema` set the mode
(`wget --post-data` to the admin API before `sqlgen init`), and the primary's `ready:` was only
the health check.

### 4. The replica's catch-up after a lossy link takes longer than 30 s

`disk=none dseed=8a06efbe04aca436 | run N=primary-replica@40-60:loss,90% P=replica@5+23
W=3cab7453,p1,q2,r150,oparallel_driver_txn` (shrunk from seed 23 of 100; 90% loss on the link
for 6 s, a paused replica): with a 30 s deadline `VIOLATION [replica_converges] not satisfied
31.36s after heal` (`92f52b95e1f62a20` twice, on an earlier image), with 600 s satisfied after
47.36 s. Probably TCP's retransmission backoff on the replication stream, not a sqld fault; the
deadline is 120 s now (OK, `1bd30e5aa605c142` twice).

### The explorations (final image, KVM engine, 2 workers)

| exploration | runs | failures, by the explorer's class | wall time |
|---|---|---|---|
| `--seeds 1-100 --strategy process --new-faults` | 100 | none | 210 s |
| `--seeds 1-50 --strategy swarm --new-faults` (also disk faults and the knob) | 50 | 32, all reproduced: unsafe-knob 5 (`D=relaxed` and a power cut: lost appends, finding 1; minimal `disk=cut dseed=1f16055b6d41c6d9 \| run C=29 D=relaxed W=:p1,r300,cparallel_driver_txn,18.12.0.343071598`, one transaction, `97dced6251b8e34d` twice in the explorer and from a fresh boot); lying-disk 13 (the primary cannot recover committed data, or the data disk does not mount); io-errors 14 (a node's restart fails, the knob cannot be set, the primary answers "disk I/O error") | 571 s |

Before the workaround of finding 3, the same process exploration found that finding (4 of 100);
with sqld's default left in place, every power-cut schedule but one lost acknowledged appends
(finding 1).

### The fault table (final image, each twice)

| schedule | verdict | fp |
|---|---|---|
| `disk=none \| run K=primary@10+4` | OK | `212221ec4b83cb9b` |
| `disk=none \| run K=replica@10+4` | OK | `3ff757f0aad0c9e5` |
| `disk=none \| run P=primary@8+6` | OK | `5787b096f928e3a3` |
| `disk=none \| run P=replica@8+6` | OK | `9b810fa7fb24e693` |
| `disk=none \| run N=primary-replica@5-15:loss,100%` | OK | `6bba1c0add7d20c2` |
| `disk=none \| run N=primary-replica@5-15:delay,2000ms` | OK | `01c87950141e2e35` |
| `disk=none \| run O=primary-replica@5-20:resp:loss,100%` | OK | `88e4520378d59d21` |
| `disk=none \| run T=10:-60` | OK | `a65ea7f5dc6954be` |
| `disk=lying:3 dseed=7 \| run C=30` | VIOLATION service cannot recover (committed data unrecoverable) (primary): a disk that drops acknowledged writes | `6435c8f5c90c60ec` |
| `disk=ioerr:20 dseed=7 \| run` | VIOLATION [replica_converges] … primary: HTTP 500 "disk I/O error" (the disk keeps failing) | `f60f0e280d2f97bb` |

## How long each phase took (wall time on the build machine, 2026-11-07, UTC)

| phase | from the start |
|---|---|
| this tree's `hydo` built, the release installed, `hydo doctor --backend kvm` all checks passed | 6 min |
| Compose file written, `hydo compose`, the image fetched (12 s), `sqlgen` written and tried against two sqld containers in Docker | 13 min |
| first image (`hydo build`, 5 s), first run on the KVM engine (4 s) | 14 min |
| first green run, twice with one fingerprint | 16 min |
| first fault table (kills, pauses, link faults, power cuts) | 26 min |
| first finding of the explorer (the replica's catch-up after a lossy link) | 35 min, after the explorer's wrong fault space was found and fixed (below) |
| the durability finding (sqld's default loses acknowledged transactions at a power cut) | 50 min (seen in a strict-serializable variant at 46 min, confirmed on the primary at 52 min, the knob at 54 min): the first power-cut runs, at 17 min, passed because the history check claimed `serializable`, which cannot see a lost write (friction 11) |

A run of this two-node system takes 4 to 6 s of wall time on the KVM engine (60 rounds, 15 to
35 s of guest time, about 120 SQL transactions); an exploration of 100 schedules with 2 workers
takes about 4 minutes.

## Friction log: what an outside user needed and where they looked

Each item: what was needed, where it was looked for, and what was done. Items marked FIXED were
changed in hydo in this work (each in its own commit, with a test or a proof).

1. **The KVM path.** `hydo doctor --backend kvm` passed every check at once, and the release's
   files plus this tree's programs were enough. But `hydo guide capabilities`, the table the skill
   sends a user to first, knows two backends (hvx and the Mac) and no KVM column; it is dated
   2026-10-30 and still says the Linux release does not bundle `hydo-read`. What the KVM engine
   cannot do was learned from refusals: `--more-faults` is refused with a message that names
   deleted programs ("explore.py: --more-faults needs --backend hvx (X= and cf= are
   stack/hvx_explore tokens)"), and `disk=cut cf=20` ends `UNSUPPORTED on kvm: host token cf=20
   needs hvx`. faults.md recommends `--more-faults` and calls it "x86_64 only", which a KVM user,
   on x86_64, reads as available. `network: bridge` and link faults work on the KVM engine
   (nothing said so; it was tried).
2. **The importer's healthcheck.** The Compose healthcheck (`curl -fsS http://localhost:8080/health`)
   became `ready: timeout 2 hydo-oci primary curl ...`, and `hydo build --check` said OK, but the
   image has no `curl` (the Compose file was wrong too: Docker would only have marked the
   container unhealthy). hydo would have failed every run at readiness after 120 s. The build
   unpacks the image, so it could check that a `ready:` command's program exists in it. Changed by
   hand to busybox `wget` from the guest.
3. **The image twice.** Both services use the same image; the build unpacks it twice into the
   root (576 MiB root, 1.1 GB image for a 63 MB Docker image), and each build leaves a staged root
   with read-only Nix store directories that `rm -rf` cannot delete without `chmod -R u+w`, plus
   the ext4 and data intermediates (another 1.1 GB). On a shared machine with 13 to 22 GB free this
   decided how many image variants could be kept. `fetch-images.sh` also fetches the arm64 image
   on Linux.
4. **Check names.** `final_bank_total` beside `always_bank_total` is refused ("duplicate check
   name 'bank_total' (the name is the property id)"); checks.md does not say that the name after
   the kind prefix must be unique across kinds.
5. **hydo-check on JSON lines.** `hydo-check equal` with `jsonl` sources refused records of
   several fields ("records have 3 fields; say which one with --field"); comparing whole rows
   needed the `file` source (a line is an item). On that usage error hydo-check printed its whole
   usage text, whose `--oracle` line contains the words `"HYDO-ORACLE: OK|VIOLATION|ERROR
   <explanation>" line`, and the host controller took that fragment for the check's verdict:
   `VIOLATION [replica_converges] |VIOLATION|ERROR <explanation>" line`. FIXED (commit
   `1fac4337`, hydo-control): only a line that starts with `HYDO-ORACLE:` is a check's own
   verdict line; every recorded transcript is unchanged, and a unit test covers the usage text.
6. **The explorer explored another system.** `hydo explore --image <file>` read the image's own
   fault space only when the file name starts with `explore-ob-`. The copied image
   (`img-v2.img`) got the built-in fault space of hydo's stack example: 50 schedules of
   `K=kafka@4+2`, `P=front@8+5`, `N=mysql@...`, 30 steps, no `W=`, all OK in 101 s. FIXED
   (commit `1800e162`, hydo-explore): the explorer reads `/etc/hydo/hydo.yaml` from an image of
   any name and keeps the built-in space only for an image without one; the explorer's tests and
   corpora are unchanged, and the next exploration named `primary` and `replica`.
7. **Unknown services in a schedule pass silently.** The host controller ignores a fault token
   for a service the image does not have: `disk=none | run K=kafka@4+2 P=front@3+2` on this image
   is OK with the clean run's fingerprint (`55608f074e54672d`). A typo in a hand-written schedule
   (`K=primry@5+2`) tests nothing and says OK. Not fixed (a refusal changes what recorded
   transcripts accept); it is what made item 6 invisible.
8. **The verdict line is not the verdict.** `hydo run` prints `RESULT ... verdict=HYDO-ORACLE:
   OK ...` (the guest's checks) and, on another line, `hydo: history checks: ... VIOLATION
   [sql.lap] ...`; the run's real verdict is in `result.json`. A script that takes the `RESULT`
   line (as this work's first scripts did) reports a lost
   write as OK.
9. **No SQL in hydo's readers.** `hydo-read` reads MySQL, PostgreSQL, Kafka, Redis and etcd, not
   SQLite or libSQL (neither a file nor Hrana over HTTP), so the differential check needed
   `sqlgen dump`; there is no generic SQL source for `hydo-check` (a query against two endpoints).
10. **The workload.** Everything SQL-specific was written by hand (`client/`, about 800 lines of
   Rust): an HTTP/1.1 client, a JSON parser, Hrana batches and interactive streams, seeded
   transfers and list-append transactions, the outcome rules, the history records, the bank
   check, the dump. "What a ready-made SQL workload generator needs" below lists what of it hydo
   could offer.
11. **Which consistency to claim, and what it hides.** Reads at the replica may be stale by
   design, so the list-append history over both nodes can only claim `serializable`. Under
   `serializable` the checker has no real-time order, so an acknowledged append missing from the
   final read after a power cut is not an anomaly: the first power-cut runs passed while sqld lost
   every acknowledged append. Nothing in checks.md or HISTORIES.md says that the levels without
   real time cannot see a lost write. A second history over the same transactions was needed
   (`lap`: what ran on the primary, plus only the appends of the transactions sent to the
   replica), claimed `strict-serializable`; recording replica transactions whole in it produced
   false `garbage-read` anomalies first. The recorder records one operation into one history; a
   projection (the same operation into two histories, one without its reads) is the client's
   job.
12. **Which read saw the loss.** A `lost-append` names transactions by their index (`T0`,
   `T1`); whether the reader was the final read-back after a power cut, a client on the primary
   or one on the replica had to be found in the console (process 9998/9999 are the read-backs,
   the route is in the command's own output). A stale replica read and a write lost at a power
   cut get the same words.
13. **The console budget.** Two histories double the records; at about 120 transactions a run
   uses 30 KB of the 64 KiB console. An SQL workload of thousands of transactions does not fit.
14. **Picking a liveness deadline.** The replica-convergence check first had `within=30`; after a
   lossy link (90% loss for 6 s) and a paused replica, the replica caught up after 47 s of guest
   time, so 1 of 100 explored schedules failed. The elapsed time of a satisfied `eventually` check
   is in its `HYDO-ASSERT` record, but no report shows the largest one over an exploration, so the
   deadline was found by rebuilding with `within=600`.
15. **Service logs.** To see why the replica lagged, the check prints the replica's last log lines
   (`/tmp/hydo/replica.log`) on an earlier output line; service logs reach the host only with
   `errors:`.
16. **The durability knob.** sqld's durability is a per-database setting (`durability_mode`:
   relaxed, the default, = `PRAGMA synchronous=NORMAL`; strong = FULL), changed through its admin
   API, which the Compose file does not enable; the knob needed `SQLD_ADMIN_LISTEN_ADDR` and a
   `wget --post-data` with nested quotes in YAML. It works as a knob because sqld applies it to
   new connections.
17. **The corpora.** As for the NATS example (its item 13), the new directory enters hydo-control's
   identity corpora (`corpus-quick.txt` and others); the goldens were re-recorded on purpose.
18. **A lone KVM fingerprint difference.** On one image, two simultaneous runs of
   `disk=none | run K=primary@10+4` gave different fingerprints (`2403237cc7a0473b` and
   `fb4341ae9ce0c5fc`) with the same control transcript (`a7245f9c1b8b4cdb`), the same console and
   the same verdict; `hydo diff` found no difference at any level it records. On the next image
   the same schedule gave one fingerprint in 14 of 14 runs (4 pinned to a performance core, 4 to
   an efficiency core, 6 unpinned), and on the final image in 2 of 2. Not explained. (Both were
   fresh boots without a power cut, so friction 19's cause, a restored snapshot, does not apply.
   Later, on the final image rebuilt from this folder with the same bytes, three pairs of
   simultaneous runs gave `212221ec4b83cb9b` six times.)
19. **A snapshot run that differs from a fresh boot (KVM engine).** On the image "set once" of
   finding 3, every schedule tried gives one fingerprint from a fresh boot (`hydo run`) and
   another from the explorer's worker, which restores its snapshot S1 for every run; each is the
   same every time, and the verdicts agree. `disk=none dseed=1 | run`: S1 `d4e72f0a8f19ef4e`,
   fresh `fdb1c55dab281097`; seed 49's minimal schedule: S1 `c0b2e4ada71292ca`, fresh
   `00a2f1a0b7df7119`. The control transcripts first differ early in the workload, at a cadence
   stop (guest time 62848 against 62912, line 57 of 1064). On the final image the two agree on
   five schedules (clean, a kill, a cut, a kill then a cut, seed 49's minimal schedule). The two
   images differ only in where the admin API call that sets the durability mode runs: in the last
   provisioning command, just before S1 is taken (it differs), or in the primary's readiness
   command, earlier (it agrees). So something in flight in the guest when S1 is taken (an armed
   timer, a packet on the bridge) is probably not restored exactly. `hv/kvm/kvm-check.sh`'s S1
   check uses one image and one schedule, so it would not see it. **Explained and fixed after
   this onboarding** (`docs/design/kvm-engine.md`, "A run from S1 against a fresh boot"): when an
   interrupt was pending at the stop where the host took S1, the run that took it injected the
   interrupt at once, while a run restored from S1 into a fresh vCPU first asked for the
   interrupt window (KVM's `ready_for_interrupt_injection` is not part of the state a snapshot
   loaded, and a fresh vCPU says 0), one exit more; from there the restored run was one exit
   behind. A snapshot now keeps those flags. The image "set once" rebuilt from this folder does
   not land S1 on such a stop (its S1 and fresh-boot runs agree before the fix too); on the
   release's Redis image, moving where S1 lands with `HYDO_KVM_STEP`, 8 of 21 steps differed
   before the fix and none after.

## What a ready-made SQL workload generator needs

What `sqlgen` does, split into what is specific to this system and what every SQL system needs.
Only the first part should be left to a user.

**Specific to libSQL (a user would still write, or pick from a list):** the wire protocol
(Hrana over HTTP here; PostgreSQL's and MySQL's wire protocols for most others, which `hydo-read`
already speaks for reading), the node addresses, and which node a statement may go to.

**What hydo could offer, from what had to be built here:**

1. **Workloads as commands, generated like the gRPC drivers.** A `hydo sql-gen` (in the manner of
   `hydo-grpc-gen`) that writes `parallel_driver_*` commands and their checks for a chosen
   dialect and endpoint list: one static client, every choice from `HYDO_SEED`. The classic
   transactional workloads, each with its checker already in hydo:
   - **bank**: N accounts, transfers that keep the total, the total and "no negative balance" as
     an `always_` and a `final_` check (here: `transfer`, `itransfer`, `total`);
   - **list-append**: Elle's workload over a `(k, v)` table, recorded for the list-append checker
     (here: `txn`, `itxn`, `read-all`);
   - **registers**: read, write and compare-and-set over a `(k, v)` table for the linearizability
     checker (not built here; the same plumbing);
   - **unique inserts / sets** and **monotonic counters**, checked as sets (`hydo-check`).
2. **Both transaction shapes.** One request per transaction (a batch with BEGIN and COMMIT) and
   interactive transactions (one statement per round trip, decisions in the client between them,
   as ORMs and client libraries run them). They behave differently: the interactive transactions met
   SQLite's `database is locked` on the lock upgrade (6 of the 7 failed commands of 117 in a clean
   run; one batch met it too); and only interactive transactions let a replica see reads before a write.
3. **The outcome rules, written once per protocol.** A connection refused, an error answered
   before COMMIT was sent, and an error that rolled back are definite failures; a missing answer
   to COMMIT (a timeout, a reset, a 5xx from a node that forwarded the write) is unknown. Busy and
   serialization errors (`SQLITE_BUSY`, PostgreSQL 40001) are failures, retryable. This is where a
   hand-written client is most often wrong, and the checkers' verdicts depend on it.
4. **Routing and the history it implies.** Per operation, which endpoint (primary, replica,
   either), and from that which histories an operation enters and with which claim: everything
   at the level the system promises across nodes (`serializable` here), the operations whose
   reads come from the primary at `strict-serializable`, and the writes alone wherever they were
   sent (friction 11). A generator knows the routes, so it can write both histories and both
   checks; by hand this took three attempts.
5. **Readers for the differential check.** A replica that must converge to its primary is the
   most common property of a replicated SQL system; `hydo-check equal` over two `hydo-read` SQL
   sources (one query, two endpoints) would replace `sqlgen dump` and the shell around it, and an
   SQLite file source would cover embedded databases (`turso`, plain SQLite).
6. **SQL invariants without a program.** A check kind whose body is a query that must return no
   rows (`SELECT id FROM accounts WHERE bal < 0`, `SELECT SUM(bal) != 800`), run on each named
   node, would express most business rules of a schema directly; here each needed a subcommand.
7. **Business operations from templates.** Beyond the classic workloads, a user's own operations
   as parameterized SQL (`UPDATE orders SET state = 'paid' WHERE id = :id AND state = 'new'`)
   with seeded parameters drawn from what earlier operations created (`:id` from an acknowledged
   insert), the same idea as `hydo-grpc-gen`'s `from: <Rpc>.<field>` hints.
8. **Durability as a knob.** Most SQL systems have the same knob (`synchronous`, `fsync`,
   `innodb_flush_log_at_trx_commit`, sqld's `durability_mode`); a generator could ask for the
   setting and write the `knobs:` entry, since the finding here was exactly the default value.
9. **Room in the console.** Histories of thousands of transactions (friction 13).

## What hydo lacked, ranked by how much it blocked this work

1. **A check of what a schedule names** (friction 7, and 6 before its fix). An exploration of
   another system's services, and a hand-written token for a service that does not exist, both
   end OK with no word. This cost a whole exploration and would have hidden every finding.
2. **Guidance on what a history check can see** (friction 11, 12). The durability finding was
   missed for 30 minutes because `serializable` cannot see a lost write. A sentence in
   checks.md, a warning when a run has a power cut and every history check lacks real-time
   order, and a `lost-append` report that says whether the reader was the read-back after the
   power cut would each have helped.
3. **An SQL workload generator** (friction 10; the section above). It is most of the work of
   onboarding an SQL system, and the part where mistakes change verdicts.
4. **One verdict where people look** (friction 8). The `RESULT` line omits the host's history
   checks.
5. **The KVM backend in the guide** (friction 1). What works on `--backend kvm` was learned from
   refusals with stale messages.
6. **SQL in `hydo-read`** (friction 9): SQLite/libSQL, and a generic query source for
   differential checks.
7. **Image staging that shares a Docker image between services, and cleans up after itself**
   (friction 3), on a disk shared with other work.
8. **Readiness commands checked against the image** (friction 2).
9. **The largest elapsed time of `eventually` checks in a report** (friction 14).
10. **Console room for long histories** (friction 13).

Not a lack of a feature, but found on the way and worth the KVM engine's attention: a snapshot
run that differs from a fresh boot on one image (friction 19, since explained and fixed), and one
unexplained fingerprint difference between two simultaneous runs (friction 18). Neither changed a
verdict.
