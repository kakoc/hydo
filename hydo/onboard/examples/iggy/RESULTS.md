# Apache Iggy under hydo: acknowledged messages, stored offsets and message expiry

Apache Iggy (github.com/apache/iggy) is a message streaming platform in Rust: streams hold topics,
topics hold partitions, and each partition is an append-only log that consumers read by offset,
alone or in consumer groups that store their offsets on the server. This example runs one Iggy
server from its official image and a test client on Iggy's Go SDK. It hunts for bugs that nobody
reported in what Iggy's documentation promises about acknowledged messages, stored consumer
offsets, and a topic's message expiry.

## The short answers

- **What was found?** No new bug. Two behaviours that a reader should know, both by design:
  1. **A clock effect of the message expiry: after a backward clock step, retention of a
     partition stops for the step's length.** Iggy deletes "the oldest sealed segments", by the
     newest message's timestamp, which is the wall clock at the append. After a step back, the
     oldest segment's newest timestamp lies ahead of the clock, and the cleaner stops at it. Every
     later segment then stays, also one whose own messages expired long ago, until the wall clock
     passes that timestamp plus the expiry. A step back of a day kept 4 sealed segments in place
     to the run's end (`disk=none | run T=15:-86400`). The section "What was found" has the code.
  2. **Damaged files that Iggy detects fence the partition.** A flipped, zeroed or truncated
     segment, WAL or frontier file of a partition stops that partition at the next start, and a
     single node cannot repair it, as the server's `config.toml` says ("Recovery behavior"). No
     damage gave a silent loss.
- **What held?** With the durable setting, `durability: persisted` on both topics, Iggy kept every
  promise under test in 112 blind schedules and 30 directed runs. No acknowledged message was
  lost, stored twice or read out of its partition's order. Group `g` never received a message
  again after an acknowledged offset store, and it never skipped one. No acknowledged message of
  the expiring topic was gone before its expiry, and no expired leading segment stayed. That held
  under kills of the server, pauses, whole-guest power cuts, the server's own power loss, and
  clock steps of 30 s to 30 days in both directions. It also held under power cuts at 9 disk
  flushes from the 50th to the 1,500th, and under kills and power cuts at the moment of a send's
  or an offset store's acknowledgement.
- **What does the setting change?** With Iggy's default, `durability: replicated`, 7 of 12
  schedules of kills and power cuts lost acknowledged messages. A kill of the server alone does it
  (`K=iggy@15+1`), and so does a power cut alone (`C=30`). With `persisted` all 12 passed.
  `hydo explore --configs` labels the loss the setting's documented trade-off: the README says
  that `replicated` "acknowledges a write before that copy reaches disk", and issue #4631 shows the
  same loss after a `kill -9` of a single node. Section "The setting `durability`".
- **What did hydo lack?** Section "What hydo lacked". Nothing blocked the onboarding.

## The version

Iggy server 0.10.0 (2026-11-07) was the newest release (tag `server-0.10.0`, commit `91e0206`).
The server runs from the image `apache/iggy:0.10.0`: the index digest is `sha256:176125a0…`,
amd64 `sha256:df58f48f…`, arm64 `sha256:79f12228…`. The client uses Iggy's Go SDK
`github.com/apache/iggy/foreign/go` v0.10.0, tagged on the same commit. It is one static program,
`iggyc`, built with Go 1.27.3 (`client/build.sh`). The Rust SDK needs Rust 1.95, and the
bare-metal machine has 1.90, so the Go SDK was used.

## The promises

Each check of the section "The checks" states one of these, in the documentation's words:

- Durability (the server's README, "Replication factor"): "A factor of 1 keeps one copy, and the
  default `replicated` durability acknowledges a write before that copy reaches disk; set
  `durability=persisted` on such a topic to acknowledge only written data." The same README:
  "Crash-safe send retries require `Persisted`; crash-safe explicit offset retries require
  `Persisted` and `Quorum`. Weaker policies can lose data and receipts on a crash."
- The server's `config.toml`, `[partition]`: "`persisted` also requires recoverable stable-storage
  copies at the replication quorum before explicit success. A singleton quorum is one replica."
- Offsets (the Go SDK's README): "`iggcon.ManualCommit()` (the default) stores nothing, and the
  application stores a checkpoint", and "A later poll from `Next` starts after what the server
  stored." In `config.toml`, `consumer_offset_durability` "controls explicit offset stores and
  deletes".
- Offsets per partition (`config.toml`, `offset_reservation_lease`): a single replica claims
  offsets ahead "so a crash-restarted replica resumes above every offset it confirmed to a client
  instead of re-minting it for a different message". Offsets may skip after a crash, and no offset
  holds two messages.
- Expiry (`config.toml`, `[partition]`): "`message_expiry` - delete sealed segments older than
  this ... The active segment is never touched." The cleaner "deletes the oldest sealed segments
  of topics with a finite message_expiry or max_topic_size, per partition, best-effort", every
  `data_maintenance.messages.interval`, and never past a committed consumer offset.

## The project

`docker-compose.yaml` starts the server as the repository's `docker-compose.yml` does, with the
released image instead of a local build. `hydo compose` imported it. The settings that differ from
the server's defaults are these:

- Only the TCP transport is on, on 127.0.0.1:8090; QUIC, HTTP and WebSocket are off.
- One shard without pinning (`IGGY_SHARDING_CPU_ALLOCATION=1`), because the guest has one vCPU,
  and a memory pool of 512 MiB.
- Logs go to stdout only, without colors.
- The segment cleaner runs every 5 s instead of every minute, so that a short run sees the expiry.

The changes from the import:

- The server's data directory is a file system of its own (`storage:`, the target of `G=` and
  `L=`). Iggy refuses a data directory that holds anything it did not write, and a new file
  system holds `lost+found`. So `IGGY_PATH` points at a folder inside it, `/app/local_data/data`.
- The released image's working directory is `/app`, so the server's `local_data` is
  `/app/local_data`. The repository's compose file builds from its own `Dockerfile`, which has no
  working directory, and mounts `/local_data`. This example mounts `/app/local_data`.
- No bridge network: one service, and the client in the guest's own network namespace.
- The client's readiness check, a login and a stream list.

## The workload

`provision:` creates the stream `s`, the topic `t` with 3 partitions, the topic `x` with 1
partition, a message expiry of 20 s and 1 MiB segments, and the consumer group `g` on `t`. Both
topics take the build parameter `durability` for `durability` and `consumer_offset_durability`.
`hydo/workload/` has four commands, run up to 3 at a time in 60 steps of 500 ms:

| command | operation |
|---|---|
| `parallel_driver_produce` | one batch of 1 to 3 messages to the partition of `t` that the seed picks, each with a unique message id and its name as the payload |
| `serial_driver_consume` | one member of group `g`: it joins, reads up to 10..49 messages from the stored offsets with a partition reader and manual commits, stores the offset of the last message it read of each partition, and leaves |
| `parallel_driver_expire` | one batch of 16 messages of 8 KiB to `x`; it records each message with the wall clock before the send and after the answer |
| `anytime_xprobe` | reads `x` during the run and fails the run when an acknowledged message is gone before its expiry |

A send or a store that the server answered with success is acknowledged. Any error, a lost
connection included, is unknown, because the SDK may have stored the batch on a retry. The client
writes the history `ig` of the messages check (`HYDO-HIST` records) and the console events
`ig.send_acked` and `ig.commit_acked` for reactions.

## The checks

| check | promise | where |
|---|---|---|
| `ig` (a `type: messages` check) | every acknowledged message is read back by a full read and by group `g` (`no-loss`, `fan_out: every-group`); a message is stored once (`duplicates: forbidden`); one message per offset, reads go up without skipping one (`order: log`); after an acknowledged store, `g` does not receive the messages again (`after_ack: no-redelivery`) | `hydo.yaml`, the drain `iggyc drain` |
| `expiry` | no acknowledged message of `x` is gone before its expiry, and no sealed segment of the leading run of expired segments stays 30 s past its expiry | `checks/final_expiry` (`iggyc check-expiry`) |
| `compose.commands` | the probe of `x` saw no message gone before its expiry during the run | `workload/anytime_xprobe` |

The drain reads every partition of `t` from offset 0 with a standalone consumer that stores
nothing, until the partitions' current offsets hold still for 8 s. Then it joins group `g`, reads
from the stored offsets, stores what it reads, and ends when it holds all 3 partitions and nothing
arrives for 10 s. Every error is "not yet", up to 230 s.

The expiry check judges by Iggy's own rule (`core/partitions/src/segment.rs`, `is_expired`): a
sealed segment goes once its newest message's timestamp plus the expiry is not after the wall
clock. A message's timestamp is not before the wall clock at its send. So a message sent at `t0`
may be gone only once the wall clock reached `t0` plus the expiry. The clock at a deletion is not
known, so the check takes the latest clock the run can show: the uptime plus the largest offset
of the wall clock from the uptime that any command recorded in this boot, and an earlier boot's
latest recorded clock. A step back therefore never makes a deletion look early. A send whose
answer came at an earlier wall clock than its request is not judged.

## Without faults, and the regressions

`disk=none | run` passes with one fingerprint per image on hvx: `f604edd59f0dc533` twice on the
first image, `5c6775224fea8d88` twice on the image with the probe, and `a23c4e632bba0742` twice
on the final image with the console events (`sha256(explore-ob-iggy-disk.img)=7b8966eb…`). A run
takes about 15 s of engine time and 1 minute of wall time. The run reads back 72 to 79 messages of
`t`, and the cleaner deletes 384 to 512 of the 512 to 544 messages of `x`.

There is no new finding, so there is no saved regression. The documented loss of `replicated`
needs another image (`hydo-configs.yaml`), and its minimal schedules are in the section "The
setting `durability`".

## What was found

### 1. A clock effect: after a backward clock step, retention of a partition stops for the step's length

The cleaner (`core/server/src/segment_cleaner.rs`) asks each partition to remove its expired
segments. `leading_expired_end` (`core/partitions/src/iggy_partition.rs`, near line 14179) walks
the sealed segments from the oldest and stops at the first one that has not expired. A segment
has expired when its `max_timestamp` plus the expiry is not after `IggyTimestamp::now()`
(`core/partitions/src/segment.rs:73`), and `IggyTimestamp::now()` reads `SystemTime`, the wall
clock (`core/common/src/utils/timestamp.rs:50`). A message's timestamp is the wall clock at its
append.

After the wall clock steps back by S, the segments written before the step have timestamps up to
S ahead of the clock. The oldest of them stops the walk, so the later segments stay even when
their own messages are older than the expiry. `disk=none | run T=15:-86400` on hvx: segments
`[0 128 256 384 512]` all stay, and the 4 sealed ones hold 512 acknowledged messages. The first
ones were sent before the step, so the whole partition keeps its data until the clock comes back
to them, a day later. The same schedule without the step deletes 512 of 544 messages.

- **The label:** a clock effect, by design. The code and the documentation delete the oldest
  sealed segments by timestamp; nothing promises a deletion by elapsed time.
- **The consequence:** after an NTP step back, a topic with an expiry grows by the step's length
  of data. The same holds for a forward step that came first and was corrected: the segments
  written during it stay ahead.
- **Not reported.** It breaks no stated promise. The duplicate search found no issue about clock
  steps and retention (section "The duplicate searches"). Issue #4650 is about an idle
  partition's active segment, which never seals and so never expires; that is another cause.

### 2. Documented: detected damage fences a partition

In the first exploration (`--storage-faults`), 7 of 20 schedules ended in ERROR, each with a
damaged file of Iggy's:

| seed | damaged file | what followed |
|---|---|---|
| 2 | `t`'s partition 1, `prepares-1/segment-0-0.log`, 4 flipped bits, then a power loss of the server | partition 1 is not served; the drain cannot finish |
| 13, 15 | `t`'s partition 2, `prepares-1/frontier`, cut | partition 2 is not served |
| 16 | `t`'s partition 2, `prepares-1/segment-0-0.log`, cut | partition 2 is not served |
| 18 | `t`'s partition 2, `00000000000000000000.log`, flipped bits | partition 2 is not served |
| 4 | `x`'s segment `128.log`, cut in its middle | the expiry check could not read `x` within its time |
| 10 | `t`'s partition 2, `offsets/groups/0` (group `g`'s offset), cut to 1 byte | the expiry check timed out (the check's own mistake, fixed) |

The server's `config.toml` says ("Recovery behavior"): "Interior damage or valid records after a
damaged range require refusal or repair, never blind truncation", and "A singleton cannot fetch a
missing copy from another replica." Each damaged partition was refused, and no read returned wrong data. In seed 10 the
group's offset file of one partition was cut to 1 byte and the server was killed later; the drain
of `g` read nothing more, and no claim broke. Explorations after the first one leave out storage
faults.

### 3. Documented: with `durability: replicated`, a kill or a power cut loses acknowledged messages

This is the result of the section "The setting `durability`". With Iggy's default, the server's
own restart after a kill loses the messages acknowledged since the partition's last published WAL
frontier. Seed 2 shows the shape: `m1_0`, acknowledged at offset 0 of its partition, is gone after
`K=iggy@1+3`, and the next message of the partition got offset 65537. That is the next block of
the offset reservation lease (`offset_reservation_lease = 65536`), so no offset holds two
messages. The README documents the loss for `replicated` with one copy, and issue #4631
(closed) has the cause, the truncation to the WAL frontier, and a comment that measured it on a
single node. Not reported: known and documented.

The expiry probe reports the loss of acknowledged messages of `x` as messages "gone before their
expiry" (seed 9). Its rule cannot tell a lost message from one deleted early; with `persisted`,
which loses nothing, a report of the probe is an early deletion.


## The setting `durability`

`hydo/hydo-configs.yaml` is `hydo.yaml` with the parameter `durability` at Iggy's default,
`replicated`, a `configs:` entry over `[replicated, persisted]` with the README's note as the
default's `tradeoff:`, the fault kinds `kill` and `cut`, and 30 steps. `hydo explore --configs
hydo/hydo-configs.yaml --seeds 1-12 --strategy process --workers 2` built one image per value and
explored each with the same seeds:

```text
  combination            schedules  failed  failures found (minimal schedule)
  durability=replicated         12       7  durable-defaults: disk=cut dseed=4127cc1347fae6ba | run C=30 W=:p1,r1000,cparallel_driver_produce,0.0.0.91903114
                                            durable-defaults: disk=none dseed=a16e4eaefdb58ac0 | run K=iggy@15+1 W=:p1,r1000,canytime_xprobe+parallel_driver_expire,...
  durability=persisted          12       0  none
```

Both failures were "found only with durability=replicated; not with durability=persisted", and
the table labeled each "by design: the setting's documented trade-off, not a bug". The first is a
`no-loss` violation of `t` after a power cut. The second is the probe's report after a kill: 48 of
80 acknowledged messages of `x` were gone. Of the 7 failing schedules, 4 had a kill and no power
cut (seeds 2, 3, 9 and 10). So with `replicated` a crash of the process, not only a power loss,
loses acknowledged messages, as issue #4631 measured.

## The duplicate searches

`gh search issues --repo apache/iggy --include-prs` (open and closed issues and pull requests)
with each phrase, on 2026-11-18. The release notes of `server-0.10.0` and the commit history of
`core/server/src/segment_cleaner.rs` and `core/partitions/src/segment.rs` on the default branch
were read too: their latest commits are about tiered storage (#4857 to #4888), segment hashes
(#4806) and resource options (#4615), none about clocks.

| phrase | what it found |
|---|---|
| `clock skew retention`, `backward clock`, `future timestamp`, `expiry clock`, `clock jump` | nothing |
| `system clock` | #4978, a metadata checkpoint; unrelated |
| `NTP`, `wall clock` | the Kafka gateway, benchmarks; unrelated |
| `segment cleaner`, `max_timestamp` | #4650 (an idle partition's active segment never seals, so its data never expires), #4601 (an expiry near `u64::MAX` wraps), #4781 (roll the active segment before retention); none is about clock steps |
| `message_expiry` | #4853 (a retention mode that may delete unconsumed messages), #4826 (finer retention by time); unrelated |
| `power loss`, `SIGKILL` | #4631 (closed): with `replicated`, a whole-cluster SIGKILL discards acknowledged messages that were already in every node's segment files, because recovery truncates to the last published WAL frontier; its single-node comment saw 40,000 acknowledged messages missing after one `kill -9` with `replicated` and none with `persisted` |
| `retention clock`, `consumer offset lost`, `offset after restart`, `persisted durability kill`, `local_data volume` | nothing |
| `lost+found`, `storage format` | no issue about a data directory that holds `lost+found`; #4003 tests the on-disk format, unrelated |

The release notes of `server-0.10.0` name no change to the cleaner, to message timestamps, or to
clocks. They name fixes of recovered consumer offsets (#4737, #4283) and receipts (#4387), which
the runs here exercised without a failure.

## The explorations

Every exploration ran on hvx with 2 workers, the process strategy and 60 steps of 500 ms.

| exploration | image | options | seeds | schedules with each fault | failures by cause |
|---|---|---|---|---|---|
| 1 | first | `--new-faults --storage-faults` | 1-20 | 14 kills, 7 power cuts, 7 power losses of the server, 12 damaged files, 17 pauses, 16 clock steps | 7 ERRORs, each a damaged file that Iggy detects (section 2); 3 of them also the expiry check's own wait, fixed |
| 2 | with the probe | `--new-faults` | 1-40 | 24 kills, 12 power cuts, 34 pauses, 31 clock steps | none |
| 3 | with the events | `--new-faults --more-faults --reactive fault --react-events ig.send_acked,ig.commit_acked --kill-now` | 41-80 | 25 kills, 21 power cuts (3 at a disk flush), 25 reactions (15 power cuts and 10 kills at the moment of an event), 20 pauses, 26 clock steps | none |
| 4 (swarm) | with the events | `--strategy swarm --new-faults` | 81-104 of 81-120 | 14 kills, 5 power cuts, 5 with disk I/O errors, 6 with a lying disk, 10 pauses, 10 clock steps | 11, each with disk I/O errors (a restart that fails) or a lying disk (Iggy refuses committed data it lost, or the file system does not mount, or a message is lost); the environment |
| configurations, `replicated` | `hydo-configs.yaml` | `--configs`, kills and power cuts, 30 steps | 1-12 | 8 kills, 5 power cuts, 1 without a fault | 7 losses of acknowledged messages: the documented trade-off (section 3) |
| configurations, `persisted` | `hydo-configs.yaml` | the same | 1-12 | the same | none |

Exploration 1 ran 20 schedules in 6 minutes, 2 in 14 minutes, and 3 in 9 minutes. The
configuration exploration took 14 minutes with its two builds and shrinks. Exploration 4 stopped
after 24 of its 40 schedules and before its report: the host's out-of-memory killer ended one of
its engines, while the repository's tests compiled beside it and two other onboardings ran. Every
one of its 11 schedules with a disk fault failed, and every other passed. A lying disk loses
synced writes and a disk with I/O errors fails the server's own reads, so these are the disk's
loss (the playbook's label "the environment"). Iggy refused to start on committed data that it
had lost ("service cannot recover (committed data unrecoverable)") in 4 of them.

The directed runs on hvx, each once:

| schedule | what it tests | verdict |
|---|---|---|
| `T=20:-300`, `T=15:-86400` | the expiry after a step back | OK; nothing of `x` deleted to the end, by design (section 1) |
| `T=20:3600`, `T=30:7200 K=iggy@32+2` | the expiry after a step forward, with a restart | OK; every sealed segment deleted |
| `T=40:-3600 K=iggy@42+3`, `T=15:-86400 K=iggy@30+3` | a restart after a step back | OK |
| `K=iggy@30+3` | the segments' timestamps after a restart | OK |
| `disk=cut \| run C=40`, `disk=cut \| run T=20:-600 C=45` | the recovery boot's expiry and offsets | OK |
| `disk=cut cf=<k> \| run` for k = 50, 150, 300, 500, 700, 900, 1100, 1300, 1500 (the run makes about 1,600 flushes) | a power cut at the k-th disk flush, with every message, offset and expiry write in flight | OK, each |
| `A=ig.commit_acked@10[+1,+3]:kill-now-iggy`, `A=ig.send_acked@10[+1,+3]:kill-now-iggy` | a kill 0 to 3 ms after an acknowledged offset store or send, before any thread of the server runs on | OK, each; the server stays down until the heal, so about 70 later commands fail |

## What hydo lacked

1. **A fresh `storage:` file system holds `lost+found`**, and Iggy refuses a data directory that
   holds anything it did not write ("storage at local_data does not match this build's storage
   format"). The first run stopped there; `IGGY_PATH` names a folder inside the mount. The import
   cannot know it. The playbook's table of setup problems has the row now.
2. **A check that judges a clock rule needs the clock of the moment that something happened.** The
   expiry check knows only when it reads, so a message deleted on time by a clock that a later step
   moved back looks early. The client records the wall clock's offset from the uptime at every
   send and probe, and the check takes the latest clock that the run showed. hydo has no record of
   the guest's clock steps that a check can read.
3. **The released Linux tree's `guest/store.tar`** was unpacked by hand before the first build, as
   in the Restate example (open item 98).

## Time

The onboarding started at 18:55 UTC on 2026-11-18 on the bare-metal machine, with this tree's
`hydo` and `hydo-hvx` over the 0.4.0 Linux release's guest files. Two other onboardings shared the
machine, and every exploration ran with two workers.

| phase | wall time |
|---|---|
| 1. Scouting: the server's README and `config.toml`, the Go SDK, the expiry and recovery code | 20 minutes |
| 2. Setup to the first clean run: the client, the import, the data directory | 15 minutes |
| 3. Explorations, directed clock and crash runs, the expiry check's two revisions | 1 hour 20 minutes |
| 4. Judging: the clock effect, the damaged files, the configurations | inside phase 3 |
| 5. Duplicate search and write-up, beside the last runs | 45 minutes |
