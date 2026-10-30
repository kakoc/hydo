# Known-bug targets for hydo

The purpose: run a release that is known to have a bug and the release that fixed it, then
let the explorer search WITHOUT being told the trigger. For each bug we learn whether hydo
finds it, after how many runs, and which fault, timing or strategy it needed. The fixed
release must pass the same schedules. Every candidate below was checked against its public
source (fetched 2026-10-18); nothing is from memory. Building starts after the `hydo.yaml`
manifest work (branch `onboard`) lands.

Ranked by chance of reproduction with today's faults, weighed against effort.

| # | Target | Buggy → fixed | Nodes | Trigger | Chance | Effort |
|---|---|---|---|---|---|---|
| 1 | SQLite WAL-reset corruption | 3.51.2 → 3.51.3 | 1 process, 2+ connections | thread interleaving only (no fault) | medium-high | low |
| 2 | etcd revision divergence after crash during defrag | 3.5.5 → 3.5.6 | 1 (maybe) or 3 | kill during/just after online defrag | high | low-medium |
| 3 | etcd stale reads caused by process pausing | 3.6.8 → 3.6.9 (also 3.5.27 → 3.5.28) | 3 | pause / slow network of a member | medium | medium |
| 4 | PostgreSQL fsync retry after a write-back error | 10.6 → 10.7 (also 11.1 → 11.2) | 1 | disk write error on write-back | medium | medium-high |
| 5 | etcd inconsistent revision after crash under high load | 3.5.2 → 3.5.3 | 3 | kill under high load | low | medium |

Not recommended now: Kafka KIP-101 (KAFKA-1211, fixed in 0.11.0.0) needs ZooKeeper plus
three JVM brokers in one guest and very old releases; PostgreSQL 12.3 serializable G2-item
(Jepsen) needs a full transaction-history checker such as Elle.

## 1. SQLite WAL-reset bug (recommended first target)

- **Source:** https://www.sqlite.org/wal.html#walresetbug (section 11), news
  https://www.sqlite.org/news.html; how Antithesis found it:
  https://antithesis.com/blog/2026/wal-reset-bug/
- **Quotes:** "The bug is likely present in all version of SQLite from 3.7.0 (2010-07-21)
  through 3.51.2 (2026-01-09). It is fixed in version 3.51.3 (2026-03-13) and later." —
  "The bug only affects databases in WAL mode when there are two or more database
  connections open on the same file, in separate threads or processes, and when those two
  connections attempt to write or checkpoint at the same instant." — Antithesis: "Notably,
  this is a completely generic workload." / "On my first run, Antithesis caught the bug in
  15 mins."
- **Mechanism (wal.html 11.1):** a complete checkpoint; a second checkpoint starts; while it
  starts, another connection commits a transaction that resets the WAL; the second
  checkpoint misses the reset and marks part of the WAL as already checkpointed; a later
  checkpoint skips part of the committed transaction, so the database file goes corrupt.
- **Invariant:** no lost committed writes, and `PRAGMA integrity_check` returns `ok`. As
  manifest commands: `final: sqlite3 /data/db.sqlite 'PRAGMA integrity_check' | grep -qx ok`
  and a check that every id in `$HYDO_ACKED` is present with its value.
- **Trigger:** no fault. Threads (or processes) writing and checkpointing at the same
  instant. On hydo's single vCPU this is a question of where the guest kernel switches
  threads, which is exactly what seeded tick jitter (Milestone 43) varies. SQLite's own
  developers could not reproduce it without a test hook; a later public reproducer (two
  threads, three connections: one checkpointing, one writing, one reading; "within
  seconds") is described at https://theconsensus.dev/p/2026/08/23/another-look-at-sqlite-wal-reset.html
  (that page returned 403 to the fetcher; details are from a search excerpt).
- **Fits one guest:** yes, trivially (one process or three processes, no network).
- **Builds:** the SQLite amalgamation tarballs for 3.51.2 and 3.51.3 from sqlite.org
  (fixed URLs, record sha3/sha256), compiled statically with the pinned musl toolchain into
  a small C workload (in the style of `hydokv.c`) plus the `sqlite3` CLI for the checks.
- **Why first:** single node, no faults, cheap builds, and a published result: the write-up
  above reports it found in 15 minutes with a generic workload. It tests the one
  technique the stack never needed — interleaving exploration — on a bug where it matters.
- **Risk:** the race window may need a preemption at a very precise instruction; if tick
  jitter cannot land there, that is itself a result (and points at finer-grained preemption,
  "inserted ticks", listed as open in M43).

## 2. etcd: inconsistent revision after a crash during defragmentation

- **Source:** https://github.com/etcd-io/etcd/pull/14685 (analysis), fix PR #14730, 3.5
  backport #14733, CHANGELOG-3.5 v3.5.6 (2022-11-21); listed as reproducible by etcd's own
  robustness tests (`make test-robustness-issue14685`,
  https://github.com/etcd-io/etcd/blob/main/tests/robustness/README.md).
- **Quotes:** "When users perform the defragmentation operation, etcd doesn't update the
  consistent index." — "All the existing etcd 3.5 releases are affected, including 3.5.0 ~
  3.5.5." — changelog v3.5.6: "revision might be inconsistency between members when etcd
  crashes during processing defragmentation operation".
- **Mechanism:** the defrag commit path does not advance the consistent index; after a
  crash, etcd replays WAL entries it had already applied, which bumps the member's revision
  out of step. etcd 3.4 and offline defrag (`etcdutl`) are not affected.
- **Invariant:** all members report the same revision and the same key/value set
  (`etcdctl endpoint status -w json` per member, compare `revision`; `etcdctl get --prefix`
  hashes). For one node, a likely detectable form: a key's `mod_revision` read after
  restart differs from the revision returned when it was written (to be confirmed).
- **Trigger:** `etcdctl defrag` on a member, then SIGKILL of that member during or just
  after defrag, before the next consistent-index update; restart. Needs a kill timed inside
  an operation, not between workload steps.
- **Fits one guest:** yes; three `etcd` processes on distinct client/peer ports and data
  directories on the data disk.
- **Builds:** official static Go release tarballs `etcd-v3.5.5-linux-amd64.tar.gz` and
  `etcd-v3.5.6-linux-amd64.tar.gz` from GitHub releases (record sha256); nixpkgs also has
  etcd_3_5 at various revisions.
- **Why second:** the trigger is a plain process kill (a fault hydo already has), the bug is
  deterministic once the kill lands in the window, and etcd's own harness reproduces it —
  a good test of whether the explorer finds a narrow kill point.

## 3. etcd: stale reads caused by process pausing

- **Source:** https://github.com/etcd-io/etcd/issues/20418 (opened 2025-07-29, closed
  2026-03-04; found by Antithesis per the robustness README); CHANGELOG-3.6 v3.6.9
  (2026-03-20) "Stale reads caused by process pausing" (#21417) and "Race between read index
  and leader change" (#21378); CHANGELOG-3.5 v3.5.28 (2026-03-20), same two entries
  (#21421, #21387).
- **Quotes:** "Looks like one member revision has not progressed for long time, but still it
  accepted reads and writes." — "What I observed was that etcd1 was getting lower readIndex
  than other nodes."
- **Invariant:** linearizable reads never go back in time: per client, the revision in a
  read response is never lower than a revision that client (or any client, after the
  response) has already observed. A simple monotonic-revision check per client catches the
  reported symptom without a full linearizability checker.
- **Trigger:** three members; one member stalled (the thread points at slow disk/network
  with partitions; etcd later added a `PauseLeader` failpoint). hydo's pause fault
  (`P=svc@at~off:dur`, M46) on the leader plus netem delay is the natural search space.
- **Fits one guest:** yes, three processes as in #2.
- **Builds:** official release tarballs v3.6.8 / v3.6.9 (or v3.5.27 / v3.5.28).
- **Note:** exact affected releases are the ones before the changelog entries; the issue
  itself names no released version.

## 4. PostgreSQL: retrying fsync after a write-back error loses data ("fsyncgate")

- **Source:** release notes 10.7 (2019-02-14) https://www.postgresql.org/docs/release/10.7/,
  announcement https://www.postgresql.org/about/news/1920, background
  https://wiki.postgresql.org/wiki/Fsync_Errors. Same fix in 11.2, 9.6.12, 9.5.16, 9.4.21.
- **Quote (10.7 notes):** "Some popular operating systems discard kernel data buffers when
  unable to write them out, reporting this as fsync() failure. If we reissue the fsync()
  request it will succeed, but in fact the data has been lost, so continuing risks database
  corruption."
- **Invariant:** every committed row is present after a checkpoint and a restart (and
  `amcheck`/a full table scan shows no corruption).
- **Trigger:** a disk WRITE error during checkpoint write-back, then PostgreSQL retries
  fsync and continues. Needs a disk fault that fails some writes but not reads: hvx's
  `ioerr` class fails reads too, which breaks the services before the bug can show (seen in
  M38/M46). A write-only error class is a small module change.
- **Fits one guest:** yes, one server.
- **Builds:** PostgreSQL 10.6 and 10.7 source tarballs from ftp.postgresql.org (or an old
  nixpkgs revision), built statically or with the pinned glibc closure. Old releases may
  need older toolchains — the main cost.
- **Behaviour of the guest kernel matters:** the bug needs a kernel that marks pages clean
  after a failed write-back (Linux does; the guest is 6.6). Worth confirming first with a
  tiny C test (write, fail the write-back via the disk fault, fsync twice, read back).

## 5. etcd: inconsistent revision after a crash under high load

- **Source:** https://github.com/etcd-io/etcd/issues/13766, etcd-dev announcement
  https://groups.google.com/g/etcd-dev/c/sad9tgmKU7Y/m/vhArFVevBgAJ, CHANGELOG-3.5 v3.5.3
  (2022-04-13) #13908, #13933.
- **Quotes:** "In our testing we have found that if the etcd process is killed under high
  load, occasionally some committed transactions are not reflected" — "The problem affects
  versions v3.5.0, v3.5.1, v3.5.2."
- **Invariant/trigger:** as #2 (member revisions/keys agree), with SIGKILL under heavy
  write load. etcd's robustness README marks its own reproduction as "Load not high
  enough", so even etcd's harness does not reliably trigger it now — a stretch target that
  is interesting precisely because it is hard.

## What the manifest (branch `onboard`, `hydo/onboard/MANIFEST.md`) needs for these

Checked against the current schema:

- **A project with no long-running service (SQLite).** `services:` is required (at least
  one). Either allow zero services, or document using the workload alone with a no-op
  service.
- **Several instances of one binary (etcd x3).** Already expressible as three service
  entries (`etcd0`, `etcd1`, `etcd2`) with their own `cmd`, `port`, data dir. A template
  form (`instances: 3` with `$HYDO_INSTANCE` in cmd/port) would make it shorter but is not
  required.
- **A peer port per member.** `port:` is one port, used by `N=`/`O=`. Partitions between
  members need the peer port too, or a list of ports per service.
- **Partitions between specific nodes.** `N=`/`O=` act on all traffic to or from one port;
  that isolates one member from everyone, which covers the usual etcd case. A partition
  between two named members needs a source-and-destination filter (a new token form).
- **A kill timed inside an operation (#2, #5).** Only the pause has a timer form
  (`P=svc@at~off:dur`); kills happen between workload steps. A `K=svc@at~off` form is
  needed.
- **Building the same manifest for two versions.** A parameter (for example
  `hydo-build --set version=3.5.5`) so the buggy and fixed images come from one file.
- **A write-only disk error (#4).** Not a manifest change: a new hvx fault class.
- **Concurrency in the workload (#1).** Nothing new: the workload is a program; it starts
  its own threads.

Sources: [SQLite WAL-reset bug](https://www.sqlite.org/wal.html), [SQLite news](https://www.sqlite.org/news.html),
[Antithesis on the WAL-reset bug](https://antithesis.com/blog/2026/wal-reset-bug/),
[etcd PR 14685](https://github.com/etcd-io/etcd/pull/14685),
[etcd robustness README](https://github.com/etcd-io/etcd/blob/main/tests/robustness/README.md),
[etcd CHANGELOG-3.5](https://github.com/etcd-io/etcd/blob/main/CHANGELOG/CHANGELOG-3.5.md),
[etcd CHANGELOG-3.6](https://github.com/etcd-io/etcd/blob/main/CHANGELOG/CHANGELOG-3.6.md),
[etcd issue 20418](https://github.com/etcd-io/etcd/issues/20418),
[etcd issue 13766](https://github.com/etcd-io/etcd/issues/13766),
[etcd-dev announcement](https://groups.google.com/g/etcd-dev/c/sad9tgmKU7Y/m/vhArFVevBgAJ),
[PostgreSQL 10.7 release notes](https://www.postgresql.org/docs/release/10.7/),
[PostgreSQL news 1920](https://www.postgresql.org/about/news/1920),
[PostgreSQL Fsync Errors wiki](https://wiki.postgresql.org/wiki/Fsync_Errors),
[KIP-101](https://cwiki.apache.org/confluence/display/KAFKA/KIP-101+-+Alter+Replication+Protocol+to+use+Leader+Epoch+rather+than+High+Watermark+for+Truncation),
[KAFKA-1211](https://issues.apache.org/jira/browse/KAFKA-1211),
[Jepsen PostgreSQL 12.3](https://jepsen.io/analyses/postgresql-12.3).
