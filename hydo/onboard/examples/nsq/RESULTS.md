# NSQ under hydo: one nsqd with every message on disk

NSQ (github.com/nsqio/nsq) is a message queue in Go. `nsqd` takes messages over HTTP
(`POST /pub`) and over its own TCP protocol, and hands each message of a topic to every channel
of the topic. A consumer of a channel acknowledges a message with `FIN`. `nsqlookupd` tells
consumers where the topics are. NSQ's documentation says that `--mem-queue-size=0` writes every
incoming message to disk, so that only a failure of the operating system or of the disk can
lose one. This example runs one `nsqd` with that setting and one `nsqlookupd` in one guest of
the Mac engine, and looks for bugs that nobody reported. It started as the project of a new
user's trial of hydo 0.6.0 (`docs/design/0.6.0-user-trial.md`).

## The short answers

- **What was found?** One new candidate and two known bugs. Nothing was posted.
  1. **New: a kill of `nsqd` in the first seconds of a new topic or channel, or a power cut a
     few seconds later, loses every message of it.** A disk queue of go-diskqueue writes its
     metadata file only at its first sync, up to 2 s after its first message, and renames it into
     place without a sync of the directory. Without the file, the restarted queue starts empty at
     file 0, position 0, although the data file holds the messages, and the next publish
     overwrites the first one (`regressions/topic-queue-restarts-empty-after-kill.schedule`,
     `regressions/channel-queue-restarts-empty-after-kill.schedule`,
     `regressions/topic-queue-restarts-empty-after-power-cut.schedule`).
  2. **Known, nsqio/nsq#1386: a kill of `nsqd` loses a message that a consumer holds in
     flight.** An in-flight message is only in `nsqd`'s memory, and a sync before the kill has
     already moved the channel's read position past it. A clean exit writes it back; a kill
     does not. A maintainer confirmed it in #1386 and #1142, and the guarantees page says that
     delivery at least once assumes that `nsqd` does not fail
     (`regressions/in-flight-message-lost-at-kill.schedule`).
  3. **Known, nsqio/nsq#1124, a clock effect: after a backward step of the wall clock, every
     publish to an existing topic waits until the clock passes the last message id's time.**
     The id generator returns `ErrTimeBackwards`, and `GenerateID` tries again every
     millisecond (`regressions/publish-waits-after-backward-clock-step.schedule`).
- **What held?** Without a kill of `nsqd` or a power cut, every message that `nsqd` answered
  `OK` reached a consumer that sent `FIN`, through pauses of `nsqd` and `nsqlookupd`, network
  loss and delays, one-way loss, and clock steps, and no consumer received a message that no
  client had sent. A kill after the queues' first syncs lost nothing but the message in flight:
  three blind kills of `nsqd` at steps 7, 8 and 15 lost nothing.
- **What is documented or the environment?** A power cut of the guest may lose what Linux had
  not written back, as the guarantees page says of a failure of the operating system. The power
  cut of finding 1 lost a message whose data file had been synced. Finding 2 is documented.
- **What did hydo lack?** The section "What hydo lacked".

## The version

- NSQ v1.3.0 (2023-12-26, the newest release), the official image `nsqio/nsq:v1.3.0`: index
  `sha256:1a369c146af71bc95c25d54b375a2b98452478c1eaf4e85f8fcb01da20f2c78a`, linux/arm64
  `sha256:7d09dcaf9f942d3c7440d75d8c84a6a4fc1289278c7bf2b6af7f8780c20e3ae6`. It is built with
  go-diskqueue v1.1.0.
- The causes were read at the tags `v1.3.0` of nsqio/nsq and `v1.1.0` of nsqio/go-diskqueue,
  and on both `master` branches: nsqio/nsq at `85cf10c0` (2026-08-11), which still requires
  go-diskqueue v1.1.0, and nsqio/go-diskqueue at `ff2a02e5` (2025-09-12). Every cause is
  unchanged there.
- hydo 0.6.0, the Mac engine, one worker of 1,024 MiB for the directed runs.

## The promises

| promise | source |
|---|---|
| with `--mem-queue-size=0` every incoming message is persisted to disk, and a node failure loses one only when the OS or the IO subsystem fails | the guarantees page, "messages are not durable (by default)" |
| a message is delivered at least once, while `nsqd` does not fail | the guarantees page, "messages are delivered at least once" |
| a message that a consumer received and did not finish comes back after `--msg-timeout` (60 s) | `nsqd`'s options; unconfirmed by the owner |
| a publish works while `nsqd` runs | unconfirmed by the owner |

## The project

`docker-compose.yaml` is the Compose file of nsq.io's Docker page, with `nsqd`'s data on a
volume and `--mem-queue-size=0`. NSQ itself has no Compose file. `hydo compose` imported it
(`hydo/hydo.yaml`), with these changes:

- `nsqd`'s data path is a file system of its own (`storage:`), the target of `L=` and `G=`.
  `nsqlookupd` keeps its state in memory only, so it has no data.
- The readiness checks ask `/ping` of each service over HTTP with the test client, not a TCP
  connect.
- `--mem-queue-size` and `--sync-every` are the build parameters `memq` and `sync_every`, with
  `0` and NSQ's default `2500`. `--sync-timeout` keeps its default of 2 s.
- The test client `nsqc` is in the image (`sh build.sh`).

`nsqlookupd` announces itself by the guest's host name `hvm`, so `nsqd`'s query for the
channels to make before a topic's first channel fails with a warning. Nothing in the workload
needs that query.

## The workload

`client/` is the Go client `nsqc`, about 460 lines, written for the trial. It speaks the HTTP
API and the TCP protocol V2 itself, without go-nsq, so that each publish and each `FIN` is sent
once. It records each operation with the Go SDK's history recorder `hydohist` in the history
`jobs`, and writes the events `nsq.pub_acked` and `nsq.msg_received` for reactions. The
commands in `hydo/workload/`:

- `parallel_driver_pub`: publishes one message `m<id>` to the topic `jobs` over `POST /pub`. An
  answer `OK` is an acknowledged send; a refused connection is a failed send; a timeout or
  another answer is unknown.
- `serial_driver_consume`: subscribes to the channel `work`, takes one message, holds it 2.5 s,
  sends `FIN`, and waits up to 5 s for the server's answer to a `CLS` after it. A `FIN` that the
  server took is recorded as the effect in a synced file of the data disk. A `FIN` without an
  answer in time is an unknown acknowledgement: `nsqd` may still take it, so the read-back
  leaves that message out unless a consumer finished it later.
- `eventually_publish`: after the faults, one probe publish must succeed within 60 s.

The drain (`nsqc drain`) first records `sub jobs work from=start`: `work` is the topic's only
channel, and `nsqd` hands a topic's earlier messages to its first channel, so the group exists
from the start. Then it takes every message left, with a quiet window of 70 s, longer than the
message timeout of 60 s, and finishes each with `FIN`. Then it writes one `status` record per
sent id: `s=ok` and `f=1` when a consumer sent `FIN` for the message, and `s=info` and `f=0`
when none did. NSQ reports no state per message, so "a consumer sent `FIN`" is the final state.

## The rules

| check | promise | file |
|---|---|---|
| `jobs` (`type: messages`): `delivery: at-least-once`, `lease` of 60 s, and `acked_outcome` with `effect: recorded` | every message that `nsqd` answered `OK` reaches the channel and ends finished by a consumer; no consumer receives a message that was not sent; a message is not delivered again while a consumer holds its lease | `hydo.yaml`, `nsqc drain` |
| `eventually_publish` | a publish works within 60 s after the faults | `workload/eventually_publish` |

## Without faults, and the regressions

The image `explore-ob-nsq-arm64.cpio.gz` `4fe61962…` was built with hydo 0.6.0 at the
fingerprint epoch 6. Two runs of `disk=none | run` give one fingerprint, `d29eb914ebcefac1`,
with 29 commands and every claim held (`no-phantom` judged 24 deliveries). Each schedule below
ran twice from fresh boots with one fingerprint, and `hydo repro --regressions` gave 5 of 5 as
expected, with the same fingerprints, in 56 s.

| file | finding | fingerprint |
|---|---|---|
| `topic-queue-restarts-empty-after-kill.schedule` | 1, the topic's queue | `d39f53f08499f0e1` |
| `channel-queue-restarts-empty-after-kill.schedule` | 1, the topic's and the channel's queues | `2b20291d317536f9` |
| `in-flight-message-lost-at-kill.schedule` | 2 | `cc4f5ae54b811cdc` |
| `publish-waits-after-backward-clock-step.schedule` | 3 | `b5b2092116480f0d` |
| `topic-queue-restarts-empty-after-power-cut.schedule` | 1, after a power cut | `501a3f509a3aca0f` |

The directed runs that separate the causes, one run each:

| schedule | image | verdict |
|---|---|---|
| finding 1's composition without `K=` | default | OK, `a8ba9cfd63a22e6e` |
| finding 1 with the kill before step 14, about 2.8 s after the first publish | default | OK, `ebadab026c178e3f` |
| finding 1 | `--set sync_every=1` | OK, `54b3f45d23821f81` |
| `A=nsq.msg_received+1000:kill-nsqd` | default | 4 of 4 lost, as `+0`: finding 1, `84f0001522086327` |
| `A=nsq.msg_received+2200:kill-nsqd` | `--set sync_every=1` | 1 of 4 lost, the message in flight: finding 2, `cc492d67978cc258` |
| finding 3's composition without `T=` | default | OK, `b98ae2a76ab45b67` |

## What was found

### 1. New: a kill of `nsqd` in the first seconds of a new topic or channel, or a power cut soon after, loses every message of it

**The schedule.** `disk=none dseed=0 | run K=nsqd@4+1 W=:p1,r200,cparallel_driver_pub,...` (the
whole line is in the regression) ends `VIOLATION [jobs.no-loss] m0 was acknowledged to process 0
at 1.162 s and never delivered in group work of channel jobs, which was drained`, and
`acked-outcome` breaks too: `6 of 7 judged operations did not end as the system answered them`.
Six publishes answered `OK` before the kill, about 0.8 s after the topic's first message. The
seventh, after the restart, reached a consumer. The kill is a SIGKILL, so the page cache keeps
every write.

**What the files show.** `hydo inspect` at the stop right after the restart (`run:1263`, before
the next publish) lists `jobs.diskqueue.000000.dat` of 192 bytes with `m0` to `m5`, and
`nsqd.dat` with the topic, but no `jobs.diskqueue.meta.dat`. In the second regression, a kill at
the moment the first consumer takes `m0`, both `jobs.diskqueue.000000.dat` and
`jobs:work.diskqueue.000000.dat` hold `m0` to `m3`, and neither queue has a metadata file. Later
in the first regression's run the restarted queue logs `negative depth at tail (-1), metadata
corruption, resetting 0...` and `readPos > writePos (96 > 86), corruption, skipping to next
writeFileNum and resetting 0...`.

**The cause** (go-diskqueue v1.1.0 `diskqueue.go`; on `master` the lines are 5 to 14 later):

1. `nsqd` answers `OK` once `writeOne` has written the message into the current data file
   with a plain `Write` (line 419; `ioLoop` returns its result at line 696). The file is
   `<name>.diskqueue.000000.dat` (`fileName`, line 536), opened with `os.O_RDWR|os.O_CREATE`
   (line 390).
2. Only `sync()` (line 433) writes the metadata, through `persistMetaData()` (line 503).
   `ioLoop` syncs after `syncEvery` reads and writes (line 656) or at a tick of `syncTimeout` once there
   was activity (lines 697 to 702). `New` writes nothing, so a new queue has no metadata file
   until its first sync.
3. At a start, `New` calls `retrieveMetaData()` (line 131), which returns the error of
   `os.OpenFile` for the missing file (line 458). `New` ignores an `os.IsNotExist` error
   (line 132). The queue starts with every position at 0 and `depth` 0.
4. The check of metadata that is out of sync with the data file (lines 476 to 497, from
   go-diskqueue#31 for go-diskqueue#28) runs only after the metadata file was read.
5. With `writePos` 0, `writeOne` writes the next message at position 0 of file 0 without a seek
   (lines 388 to 405), over the first old message.
6. `persistMetaData` syncs a temporary file and renames it to `<name>.diskqueue.meta.dat`
   (lines 524 to 528), and syncs no directory. So a power cut after the first sync can still
   leave no metadata file.

The topic's and each channel's queue come from `diskqueue.New` in `nsqd/topic.go` line 67 and
`nsqd/channel.go` line 109 (124 on `master`), with `--sync-every` 2500 and `--sync-timeout` 2 s
(`nsqd/options.go` lines 119 and 120). So every new topic and every new channel has a window of
up to 2 s or 2,500 messages, and so has a channel after `/channel/empty`, which deletes the
queue's files. A crash of `nsqd` in the window (SIGKILL, the out-of-memory killer, a panic)
loses the window's messages. A kill after the first sync lost none, and neither did the same
kill with `--sync-every=1`, which syncs after each message.

**After a power cut.** `disk=cut dseed=0 | run C=20 W=:p1,r400,cparallel_driver_pub,...` publishes
one message, `m13`, in round 10 and cuts the guest's power at the end of the workload, about 3 s
after the topic queue's first sync. It ends `VIOLATION [jobs.no-loss] m13 was acknowledged to
process 13 at 5.257 s and never delivered`. At the last stop before the cut the data path holds
`jobs.diskqueue.000000.dat` (33 bytes) and `jobs.diskqueue.meta.dat` (`1`, `0,0`, `0,33`). In the
recovery boot it holds the data file with `m13` and `jobs.diskqueue.meta.dat.<n>.tmp`, and no
`jobs.diskqueue.meta.dat`: the rename did not reach the disk. The blind exploration e1 had 6
failures with a power cut, and the explorer shrank seed 1's to this schedule; the other five have
the same verdict and were not looked at one by one.

**The judgment.** A bug of go-diskqueue that breaks NSQ's documented promise of
`--mem-queue-size=0`: a process crash is no failure of the operating system or of the disk. The
maintainers' answer in #1142, "a reduction from thousands of queued messages being lost, to
potentially only a couple", is about in-flight and deferred messages, which this loss is not.

**Duplicate search.** No issue or pull request names a missing metadata file at a start (the
section "The duplicate searches"). go-diskqueue#28 and its fix #31 handle metadata that is
older than the data file, not a missing one.

**No native run.** The cause is plain in the code, and hydo's files after the restart show it.

### 2. Known, nsqio/nsq#1386: a kill of `nsqd` loses a message that a consumer holds in flight

**The schedule.** `disk=none dseed=0 | run A=nsq.msg_received+2200:kill-nsqd` kills `nsqd`
2.2 s after a consumer took `m0`, before its `FIN` at 2.5 s, and ends `VIOLATION
[jobs.acked-outcome] 1 of 4 judged operations did not end as the system answered them: m0 was
acknowledged and has no final state`. `no-loss` leaves `m0` undecided: the consumer's `FIN` to
the killed `nsqd` is recorded as unknown (open item 183). After the restart
`jobs:work.diskqueue.meta.dat` says `3`, `0,32`, `0,128`: the channel's read position is past
`m0`, whose 32 bytes are still in the data file. The other three messages came to the drain.

**The cause** (nsqd v1.3.0). The consumer's `messagePump` takes `m0` from the channel's disk
queue (`nsqd/protocol_v2.go` line 303), which moves the queue's read position, and
`StartInFlightTimeout` (line 315, `nsqd/channel.go` line 447) keeps it in memory. The queue's
next sync, at most 2 s later, writes the read position. `Channel.exit` writes the in-flight and
deferred messages back to the queue at a clean exit (`nsqd/channel.go` lines 187 and 214 to
250), and a kill skips it.

**The judgment.** Known and documented. In #1386 (2021, `--mem-queue-size=0`) a maintainer
answered "Messages in-flight are only in memory. nsqd must exit gracefully for them to be saved
to disk", and in #1142 "in-flight and deferred messages will be in memory and not in the valid
part of the disk queue, so those can still be lost". The guarantees page says that delivery at
least once assumes that `nsqd` does not fail.

**What the trial's schedule was.** The trial's `A=nsq.msg_received:kill-nsqd` kills at the
moment of the receipt, before either queue's first sync, so it shows finding 1, not finding 2:
4 of 4 messages are lost, and so with `+1000`. Only `+2200` comes after the channel queue's
sync.

### 3. Known, nsqio/nsq#1124, a clock effect: after a backward clock step every publish waits

**The schedule.** `disk=none dseed=0 | run T=7:-1265 W=:p1,r100,...` ends `VIOLATION
[eventually_publish] eventually_publish exited rc=1: probe: no publish succeeded within 60 s
after the faults`. Every probe's publish timed out after 5 s. Without `T=` the run passes.

**The cause** (nsqd v1.3.0, unchanged on `master`). `NewGUID` (`nsqd/guid.go` lines 51 to 88)
reads `time.Now()` and returns `ErrTimeBackwards` while the clock is before the last id's time
(line 57). `Topic.GenerateID` (`nsqd/topic.go` lines 491 to 503, 500 to 512 on `master`) tries
again every millisecond and logs every 10,000th try. The HTTP and TCP publish paths call it
(`nsqd/http.go` line 249, `nsqd/protocol_v2.go` lines 804, 920 and 996), so a step back of
21 minutes holds every publish to a topic that had a message for 21 minutes. A restart of
`nsqd` ends it, because each process makes new id generators.

**The judgment.** Known: #1124 ("nsqd: stops sending messages after moving time backward",
open, labeled `bug`), #1249 (a change to ids that do not depend on the clock, closed unmerged),
and #1283 (a pull request that logs the error, closed). A clock effect.

## The duplicate searches

`gh search issues --include-prs`, open and closed, on 2026-11-26:

| repository | phrase | what it found |
|---|---|---|
| nsqio/nsq | `diskqueue metadata`, `meta.dat`, `retrieveMetaData`, `metadata file`, `meta file` | #58, #203, #231, #730, #1032, #1187, #1275, #755: other metadata bugs, fixed, and questions; none about a missing file at a start |
| nsqio/nsq | `lost messages crash`, `messages lost after restart`, `topic messages lost restart`, `depth 0 after restart`, `new topic lost` | nothing |
| nsqio/nsq | `kill -9`, `SIGKILL`, `kill nsqd`, `nsqd crash` | #625 (a WAL per topic, open), #1354 (a duplicate of #510), #1386, #1391, #755 |
| nsqio/nsq | `mem-queue-size=0`, `unclean shutdown`, `data loss`, `power`, `diskqueue` | #1142, #1386, #1391 (in-flight and memory messages), #855 (in-flight messages under power cuts), #510 (resilience, open), #1459 (the default memory queue) |
| nsqio/nsq | `inflight`, `in-flight lost` | #1386, #144, #160 |
| nsqio/nsq | `time has gone backwards`, `ErrTimeBackwards`, `failed to create guid` | nothing |
| nsqio/nsq | `guid`, `clock`, `system time`, `NTP`, `clock backwards` | #1124 (open), #1249, #1283, #741 |
| nsqio/nsq | `sync-timeout` | #855, #1507 |
| nsqio/go-diskqueue | `metadata`, `meta`, `retrieveMetaData`, `sync`, `overwrite`, `restart`, `lost`, `crash`, `kill`, `empty`, `position 0` | #28 and #31 (metadata older than the data file), #29, #44 (`Close()`), #3, #1, #45, #21, #23; none about a missing metadata file |

The ChangeLog of `master` has no entry about the disk queue's metadata since v1.3.0.

## What held, and why

- A queue whose metadata file exists starts again from its last synced positions. When the data
  file is longer than the metadata says, `retrieveMetaData` moves writes to a new file and
  reads the rest of the old one, so a kill after the first sync lost nothing that was not in
  flight. Messages may come again; the claims allow duplicates.
- Pauses of `nsqd` and network faults between the client and `nsqd` made publishes unknown or
  failed, and every acknowledged one arrived.

## The explorations

| name | options | seeds | schedules | failures |
|---|---|---|---|---|
| the trial, blind | the trial's image; `--strategy process --storage-faults` | 1-20 | 20 | findings 1 and 3, and power losses of `nsqd` (documented) |
| the trial, aimed | `--reactive-at nsq.msg_received@2 --react-delays 0..2500/500 --react-actions kill,powerloss` | 1-36 | 36 | 15 runs lost messages after a kill, shrunk to `A=nsq.msg_received:kill-nsqd`, which is finding 1 |
| e1 | the final image; `--strategy process` | 1-20 | 20 | 14: finding 1 after a power cut (6, shrunk to `C=20` alone), finding 1 after a kill of `nsqd` (seeds 2, 10 and 11; seed 11's kill came at the stop where a consumer took its first message), and finding 3 (5, shrunk to `T=7:-1265`) |

The trial's rounds are in `docs/design/0.6.0-user-trial.md`, section 4. e1 ran on 3 workers in
713 s, 105 runs an hour. A first version of e1, on an image whose client recorded a `FIN`
without an answer as no final state, also failed seed 13: a pause of `nsqd` held the `FIN` past
the client's 5 s, `nsqd` took it after the pause, and the read-back called the message lost.
That was a mistake of the rules. The client now leaves such a message out of the read-back, and
seed 13 passes (`P=nsqd@10+1 W=6311ab23,p1,q2,r200` gives OK).

## What hydo lacked

In the order of the time each cost:

1. **The trial's aimed schedule named the wrong cause.** The grid of `--reactive-at
   nsq.msg_received` found losses after a kill at the receipt and shrank them to `+0`, where
   finding 1 hides finding 2. The trial took them for the in-flight loss. Only directed kills
   at `+1000` and `+2200`, and the files after the restart, told the two apart. A shrink
   moves a reaction's delay towards 0, so it moves towards the window of finding 1.
2. **`hydo inspect exec --service` does not see a service's own root.** `--service nsqd` runs
   in the service's network namespace, and `/data/vol/nsqd-data` there was the empty mount
   point. The files were in the service's root, through `hydo-oci nsqd`, as `hydo guide
   inspect` says in a later line.
3. **No service log for a failed check.** A run whose check failed and whose services did not
   exit has no `logs/` folder, so `nsqd`'s log and its log before the restart
   (`/tmp/hydo/nsqd.log.old`) needed a `hydo inspect` session.
4. **`no-loss` did not judge the messages that finding 1 lost.** They were sent before any
   receive, so the group had not joined yet (open item 183). Only `acked_outcome` caught them
   until the drain recorded `sub jobs work from=start`. The same record from a `first_` command
   did nothing: a `first_` command runs in provisioning, and its records are not in the run.
5. **The verdict's words for a lost message.** `m0 was acknowledged and has no final state (no
   error code)` (open item 183).

Milestone 367 changed hydo for items 2 to 4. `hydo inspect exec --service` of an image service
runs inside the service's root. A failed check's run keeps every service's log, and the log
before its last restart, in `logs/`. `hydo guide messages` says that a `sub` record from a
`first_` command has no effect. Item 1 is left (open item 184). Milestone 363 changed item 5.

## Time

| phase | wall time |
|---|---|
| the trial (2026-11-25), from the guide to the regressions | about 1 hour |
| the example: the client, the manifest, the build and the first clean run | 20 minutes |
| the directed runs, the files after the restarts, and the code | 1 hour |
| two fixes of the client (the drain's `sub`, the unknown `FIN`) and every run again | 1 hour 30 minutes |
| the explorations, the duplicate search and the write-up | 1 hour 30 minutes |
