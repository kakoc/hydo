# Dragonfly with replicas under hydo

Dragonfly is an in-memory key-value store in C++ that speaks the Redis protocol. This example
runs Dragonfly 2.1.0 as one primary and two replicas and hunts for bugs in its snapshots, its
replication and failover, its transactions, and its key expiry.

## The short answers

- **What was found?**
  1. **A new candidate: after a backward step of the wall clock, a restart loads an older
     snapshot.** Dragonfly chooses the snapshot to load at start by the files' modification
     time. After the clock steps back, every new snapshot has an older time than the snapshots
     from before the step. A restart then loads a snapshot from before the step, also after a
     clean `SHUTDOWN` that saves first. Every write since that snapshot is gone, although a
     `SAVE` answered `OK` after it. hydo found it blind in 25 of 120 explored schedules, and
     the shrinker reduced it to one step of 18 s back (`T=8:-18`). No report names it.
  2. **A known bug: a power cut loses snapshots that `SAVE` acknowledged.** Dragonfly renames
     its snapshot files into place and answers `OK` without an `fsync`. After a power cut the
     newest snapshots are gone. This is
     [dragonflydb/dragonfly#8646](https://github.com/dragonflydb/dragonfly/issues/8646) (open).
  3. **A known requirement: Dragonfly's Lua interpreter needs AVX.** On a CPU without AVX the
     first `EVAL` ends Dragonfly with "An attempt to execute an instruction failed". The
     maintainers require a Sandy Bridge CPU or newer
     ([#163](https://github.com/dragonflydb/dragonfly/issues/163)).
- **What held?** `REPLTAKEOVER` kept its promise in 80 schedules with kills, pauses and
  partitions: after every `OK`, the new primary held every key of the old one. No run failed
  the rule atomic (transactions whole on every server) or the rule converge (the replicas hold
  the primary's data). The rule expiry failed only through finding 1 and in its first version,
  which was too strict.
- **What did hydo lack?** Five things, listed in the section about what the onboarding needed
  from hydo. The first was a start blocker: Dragonfly exits when `/proc/self/cgroup` is empty,
  and in hvx's guest it is empty until a cgroup2 file system is mounted.

## The version

Dragonfly 2.1.0 (2026-11-04) is the newest release. The image is the official
`ghcr.io/dragonflydb/dragonfly:v2.1.0`, pinned by digest in `hydo/fetch-images.sh` and in
`hydo/hydo.yaml`. The code of finding 1 is the same on Dragonfly's `main` branch on 2026-11-16
(`FileSnapshotStorage::LoadPath` in `src/server/detail/snapshot_storage.cc`).

## The project

Dragonfly's repository has no Compose file for replicas, so this example has its own:
[docker-compose.yaml](docker-compose.yaml). It has a primary and two replicas. Each server runs
two threads (`--proactor_threads=2`), so that its key space has two shards and one transaction
can span both. Snapshots go to `/data` with the default file name `dump-{timestamp}`. Dragonfly
writes a snapshot on `SAVE`, on `BGSAVE` and on shutdown, and loads the newest one when it
starts.

| file | what it is |
|---|---|
| `docker-compose.yaml` | the three servers, the input of `hydo compose` |
| `build.sh` | builds `bin/dfc-amd64` and `bin/dfc-arm64` from `client/`, static Linux programs |
| `client/main.go` | `dfc`, the test client: every operation, the wait for the replicas, and the rules (standard library only) |
| `hydo/hydo.yaml` | the importer's three services, with the client's readiness check, each server's role in a file, error logs as findings, and the parameter `takeover` |
| `hydo/known-errors.yaml` | the expected error-log kinds, each with its reason |
| `hydo/workload/` | three parallel drivers, two serial drivers, one anytime read, and the wait for the replicas before and after the faults |
| `hydo/checks/` | the four final rules |
| `hydo/regressions/` | the schedules of findings 1 and 2, and the passing neighbours of finding 1 |
| `native/` | the snapshot choice of finding 1 without hydo, in Docker |

```sh
sh build.sh                     # needs Go 1.27.3
cd hydo && sh fetch-images.sh   # the Dragonfly image by digest, network once
hydo build hydo.yaml            # on Linux (hvx or KVM)
hydo run "disk=none | run"
hydo explore --seeds 1-60 --strategy process --new-faults --cluster-faults
hydo build hydo.yaml --set takeover=yes   # the failover variant
```

The example is for x86_64 guests only (hvx and KVM), because it needs `guest.cpu: v3` (the next
section). Its runs used hvx only.

The import was changed in three places:

- The readiness check is the client's `dfc ready`. It requires an answer to a read, so a server
  that still loads its snapshot (`LOADING`) is not ready.
- Each server reads its `--replicaof` flag from `/data/replicaof`. Each service's `setup` writes
  the file once with the Compose file's value, and a takeover changes it. A server that restarts
  after a takeover then comes back in its new role.
- Each service's `setup` mounts the cgroup2 file system. Dragonfly reads `/proc/self/cgroup` at
  start and exits when the file is empty (`Failed to parse cgroupv2 format`). The guest's file is
  empty until a cgroup2 file system is mounted.

## The CPU level

The guest CPU is `v3` (`guest.cpu: v3`). On the default x86-64-v2 guest, Dragonfly stopped at the first `EVAL` with `An attempt to execute an instruction failed.The
root cause might be an old hardware. Exiting...`. Dragonfly builds its own code for Core 2 with
SSE4.1 (`-march=core2 -msse4.1 -mpopcnt`), but it builds its Lua interpreter from its fork
`dragonflydb/lua` (tag `Dragonfly-5.4.6a`), whose makefile sets `-march=sandybridge` on x86_64.
In the 2.1.0 binary, `lua_newstate`, `luaV_execute` and about 60 other Lua functions contain AVX
instructions such as `vextractf128`, with no check of the CPU first. hydo's v2 CPU offers no AVX,
XSAVE or OSXSAVE in CPUID, so the instruction faults as it does on a real CPU without AVX. This
is a known requirement. In a run on a v2 image, the primary stopped this way 6 times: no Lua
transaction was acknowledged and 7 failed, while 5 `MULTI` transactions were acknowledged. In issue
[#163](https://github.com/dragonflydb/dragonfly/issues/163) a maintainer wrote "we require
sandybridge or equivalent", and the closed issues
[#3166](https://github.com/dragonflydb/dragonfly/issues/3166) and
[#5197](https://github.com/dragonflydb/dragonfly/issues/5197) report this message on virtual
CPUs of the x86-64-v2 level.

## The workload

Each composer entry owns its keys, so no other operation changes them. Every write records the
primary's process id, read on the same connection right after the acknowledgement. A write and
a save with the same process id ran in one process of the primary, with no restart between
them.

| command | operation |
|---|---|
| `parallel_driver_set` | `SET s:<id>` to a value of 0 to 2,000 bytes on the primary, then `WAIT 2 500` |
| `parallel_driver_tx` | sets `t:<id>:a`, `t:<id>:b` and `t:<id>:c` in one `MULTI`/`EXEC`, or in one Lua script (`EVAL`) |
| `parallel_driver_px` | `x:<id>` with an expiry of 100 ms to 10 minutes: `SET` with `PX`, `EX` or `PXAT`, or `SET` and `PEXPIRE`; then it reads the value and the absolute expiry back from the primary |
| `serial_driver_save` | a snapshot of the primary. One time in two it is `SAVE`, one time in four `BGSAVE` and a wait until it ends, and one time in four `SHUTDOWN` (which saves) and the wait for the restart |
| `serial_driver_takeover` | with `takeover=yes`: `REPLTAKEOVER 10` on a replica in stable sync, then the check of the promise below |
| `anytime_read` | reads 8 acknowledged items from one server and applies the expiry rule at once |
| `first_settle`, `eventually_settle` | wait until both replicas are in stable sync with no lag: 80 s before the test, 100 s after the faults heal |

## The rules

| rule | file | what must hold |
|---|---|---|
| durable | `checks/final_durable` | a write acknowledged before a `SAVE`, a `BGSAVE` or a `SHUTDOWN` that completed in the same process of the primary is on the primary at the end |
| atomic | `checks/final_atomic` | on every server, the three keys of a transaction are all there or none is |
| converge | `checks/final_converge` | each replica holds exactly the primary's keys, values, and expiries |
| expiry | `checks/final_expiry` | no server returns a key after its expiry; the primary keeps a saved key whose expiry has not come |
| takeover | `workload/serial_driver_takeover` | after `REPLTAKEOVER` answers `OK`, the new primary holds every key the old primary held just before |
| liveness | `workload/eventually_settle` | after the faults heal, both replicas are in stable sync within 100 s |

The rule durable follows from Dragonfly's documentation. Without `--aof`, a snapshot is
Dragonfly's only persistence, so a kill may lose the writes since the last snapshot. A write
that a completed snapshot holds must survive, because every start loads the newest snapshot.
The rule takeover quotes the documentation of `REPLTAKEOVER`: "The master first stops serving
regular clients and waits until the replica has applied all of the master's writes. Only then
does the replica become master."

Two rules were too strict at first, and the runs showed it:

- The rule durable first counted every write before a completed snapshot. A kill of the primary
  between the write and the snapshot loses the write by design. The rule now requires the same
  process id for the write and the snapshot.
- The rule expiry first required every item whose expiry was later than the client's last clock
  reading. A clock step back that comes a few rounds after that reading makes an item that has
  expired look live again. The rule now requires 10 s more than the last reading.

## Without faults, and the regressions

The final image, `explore-ob-dragonfly-disk.img` with sha256 `7cb07d1a…`, gave
`fp=aba00945721586e9` twice for `disk=none | run` on hvx. The run starts 71 commands, and all
of them end with exit status 0. The error logs of a run without faults have three kinds, one per
server, all of the one pattern that `hydo/known-errors.yaml` lists.

`hydo repro --regressions` gave 5 of 5 as expected, twice, with the same fingerprints:

| regression | expects | fp |
|---|---|---|
| `snapshot-older-after-clock-step` | violation (finding 1) | `4e00723d70706c54` |
| `snapshot-after-shutdown-without-step` | OK, the same composition without the step | `31d217a68cbef3d0` |
| `snapshot-older-after-kill-and-clock-step` | violation (finding 1) | `ec06181c11e9c87d` |
| `snapshot-after-kill-without-step` | OK, the same kill without the step | `d8e3cca2145b739f` |
| `known-8646-snapshot-lost-at-power-cut` | violation (finding 2) | `4a6542fbfeb2dfdf` |

## What was found

### 1. After a backward clock step, a restart loads an older snapshot (new candidate)

**The schedules.** The explorations ex1 and ex2 (section "Runs") explored 120 schedules on the
image without the takeover. Of
them, 25 failed the rule durable or the rule expiry with a backward clock step, and 16 of those
had no power cut. The first was the fourth schedule of ex1 (seed 4). The shrinker reduced
seed 4 of ex1 to `T=10:-81000` and its composition, and seed 64 of ex2 to one step of 18 s back
(`T=8:-18`). The saved regressions replay the ex2 shrinks on the final image, and each passes
without its clock step:

| regression | schedule | verdict | fp |
|---|---|---|---|
| `snapshot-older-after-clock-step` | `disk=none dseed=9836c546baaf0b5 \| run T=8:-18 W=48c2cbb5,p5,q2,r300,oanytime_read` | `VIOLATION [durable] the primary lost 17 of 21 writes acknowledged before a completed snapshot` | `4e00723d70706c54` |
| `snapshot-after-shutdown-without-step` | the same without `T=8:-18` | OK | `31d217a68cbef3d0` |
| `snapshot-older-after-kill-and-clock-step` | `disk=none dseed=ff6a37cd7620524e \| run K=df1@21+9 T=12:-150 W=c854bf22,p4,q2,r600` | `VIOLATION [expiry] 1 items break the expiry rule` | `ec06181c11e9c87d` |
| `snapshot-after-kill-without-step` | the same without `T=12:-150` | OK | `d8e3cca2145b739f` |

The first pair loses the writes at a clean `SHUTDOWN` of the composition's save driver. The
second pair loses them at a kill (SIGKILL) of the primary: a key with a 10-minute expiry, which a
later `SAVE` held, is gone before its expiry.

A look inside ex1's shrunk run (`T=10:-81000`, `ad89151ae61e0fff`; without the step OK,
`11068df5e881aef6`) with `hydo inspect exec` shows the sequence. Before the step, the primary
saved `dump-2026-11-16T00:00:04` and `dump-2026-11-16T00:00:05`. The clock then went back 22.5
hours. The client acknowledged `SET s:14`, a `SAVE` answered `OK` (`dump-2026-11-15T01:30:06`),
and a `SHUTDOWN` saved `dump-2026-11-15T01:30:07` and stopped the primary. The restarted primary
logged:

```text
I20261115 01:30:08.386636   401 server_family.cc:1775] Loading /data/dump-2026-11-16T00:00:05-summary.dfs
```

`s:14`, the one write between the step and the shutdown, was gone.

**The cause.** `FileSnapshotStorage::LoadPath` in `src/server/detail/snapshot_storage.cc` chooses
the file to load among the files that match `--dbfilename`:

```cpp
rng::sort(*short_vec, [](const io::StatShort& l, const io::StatShort& r) {
  return std::difftime(l.last_modified, r.last_modified) < 0;
});
auto it = std::find_if(short_vec->rbegin(), short_vec->rend(), ...);
```

It takes the summary file with the latest modification time. The kernel sets that time from the
wall clock. After a step of D seconds back, every snapshot written in the next D seconds has an
older time than the last snapshot from before the step. So every start in that time loads the
old snapshot. The `{timestamp}` in the file name comes from the same clock, so an order by name
would fail the same way. On `main` (2026-11-16) the code still keeps the file with the largest
`last_modified`.

**Without hydo.** [native/](native/README.md) shows the choice with the official image in Docker.
It writes two snapshots, gives the first one's files a time one hour ahead with `touch`, and
restarts with SIGTERM. Dragonfly loads the first snapshot, and the newer value is gone. The
clock step itself is not reproduced natively, because the cause is plain in the code.

**The decision: a Dragonfly bug.** Dragonfly's documentation ("Saving Backups") makes two
promises. The first is "Dragonfly will create a backup upon shutdown". The second is that at
start it "will try to find a snapshot file in its configured `dir` path and will load it
automatically". For the default `dump-{timestamp}`, the AOF page says it loads "the newest
one". After a
backward step it loads an older one. It throws away writes that a `SAVE` acknowledged and that a
clean shutdown saved. A backward step is a normal event. For example, ntpd steps a clock that is
more than 128 ms off, and a virtual machine's clock can step back after a migration. The
documentation names no dependence on the wall clock. With a fixed `--dbfilename`, such as `dump`,
Dragonfly overwrites one file and loads it by its exact name, so the default name is the setting
that loses data.

**The search for an earlier report.** Nothing reports it. Each phrasing was searched in
dragonflydb/dragonfly's issues and pull requests, open and closed, and in its discussions, on
2026-11-16:

- issues and pull requests: `snapshot mtime`, `load latest snapshot`, `wrong snapshot loaded`,
  `clock backwards`, `time jump`, `snapshot timestamp`, `older snapshot`, `last_modified`,
  `newest snapshot`, `LoadPath`, `stale snapshot`, `old snapshot loaded`, `data lost after
  restart`, `lost data restart snapshot`, `system time`, `ntp`, `clock`, `modification time`,
  `dbfilename timestamp`, `snapshot selection`, `latest snapshot`, `dump-{timestamp}`, `time
  went backwards`, `time change`, `date change`, `loads wrong snapshot`, `restart loses data`,
  `snapshot not loaded`, `shutdown save lost`, `FindMatchingFile`, `StatFiles`;
- discussions: `snapshot clock`, `snapshot restart lost`, `newest snapshot`, `wrong snapshot`,
  `system time`, `snapshot loaded old`, `time`, `clock backwards`, `dump timestamp`, `restart
  data lost`.

The nearest reports are about other code:

- Issue [#2824](https://github.com/dragonflydb/dragonfly/issues/2824) (open) asks for a limit on
  the number of kept snapshots.
- Pull request [#1028](https://github.com/dragonflydb/dragonfly/pull/1028) (merged, 2023) made
  `dump-{timestamp}` the default name.
- Pull request [#8779](https://github.com/dragonflydb/dragonfly/pull/8779) (merged 2026-11-13)
  reworked `SnapshotStorage` for the cloud stores and kept the choice by modification time.
- Issue [#8406](https://github.com/dragonflydb/dragonfly/issues/8406) (open) is about AOF replay
  of commands with absolute deadlines.

### 2. A power cut loses snapshots that SAVE acknowledged (known)

**The schedules.** Six failures of the first exploration had a power cut at the end (`C=30`) and
no backward clock step: seeds 20, 21, 40, 52, 57, and 58. Seed 58's composition with the cut alone
fails, and the same schedule without the cut passes (two runs each):

| schedule | verdict | fp |
|---|---|---|
| `disk=cut dseed=9f14bde427152010 \| run C=30 W=07713d5d,p1,q2,r150` | `VIOLATION [durable] the primary lost 9 of 14 writes acknowledged before a completed snapshot` | `dc90bf5a49ea5252` |
| `disk=none dseed=9f14bde427152010 \| run N=nodes.isolate.4@0-2:loss,100% P=df3@3+8 W=07713d5d,p1,q2,r150` | OK | `7db895511e57586c` |

In the recovery boot, `/data` held the snapshots up to `dump-2026-11-16T00:00:07` and the files
of the next one as `dump-2026-11-16T00:00:08-*.dfs.tmp`, the summary file empty. The later
snapshots, whose `SAVE` had answered `OK`, were gone. Dragonfly loaded `dump-2026-11-16T00:00:07`,
and 9 writes that later snapshots held were lost. ex2 had 6 more such failures. The saved
regression `known-8646-snapshot-lost-at-power-cut` is seed 99 of ex2 with only the cut kept.

**The decision: known.** [dragonflydb/dragonfly#8646](https://github.com/dragonflydb/dragonfly/issues/8646)
(open, 2026-10-22) reports exactly this. `SAVE` writes the files as `.dfs.tmp`, renames them and
answers `OK` without an `fsync` of the files or of the directory, so the renames can be lost at a
power cut. Dragonfly's AOF design document says "Snapshot files are not fsynced today".

### 3. REPLTAKEOVER kept its promise

With `--set takeover=yes`, `serial_driver_takeover` moved the primary during the faults. The
exploration ex3b drew kills, pauses, partitions and one-way faults, but no clock steps and no
power cuts, which have the two findings above. `REPLTAKEOVER` answered `OK` at least once in 73 of
its 80 schedules, and after every `OK` the new primary held every key the old primary held just
before. No schedule failed.

An earlier exploration of the same variant (ex3) failed `eventually_settle` in 4 of 59
schedules. In them, the node that the client recorded as the primary reported itself a
replica, or the primary had one replica instead of two. The client did not do the operator's
part of a takeover. When a pause or a partition takes the answer of `REPLTAKEOVER` away, the
takeover can still happen, and the third node must be pointed at the new primary. `dfc settle`
now does both, as an orchestrator would, and ex3b, with the same seeds, gave no such failure.

## Runs

Every run used hvx on the bare-metal machine with the 0.3.4 Linux release, one VM at a time.

| exploration | image | seeds | failures |
|---|---|---|---|
| ex1 | the first image, before the takeover variant | 1-60 | 26, every one reproduced. 18 failed the rule durable with a backward clock step (finding 1), and 6 with a power cut (finding 2). 2 failed the rule expiry, which was too strict |
| ex3 | `takeover=yes`, no clock steps or power cuts | 1-59, stopped | 4 in `eventually_settle`, from the client's bookkeeping after a takeover |
| ex3b | the same, with the fixed client | 1-80 | 0 |
| ex2 | the final checks, with error logs | 61-120 | 13, every one reproduced: 7 with a backward clock step (finding 1, 3 of them without a power cut), 6 durable with a power cut (finding 2) |

Each exploration used `hydo explore --strategy process --new-faults --cluster-faults --workers 1`.
ex1 ran 299 schedules an hour, ex2 286, and ex3b 588. ex2's error logs had 4 new kinds, all of
them expected after a restart or a fault; `hydo/known-errors.yaml` lists them.

## What was not covered

- Dragonfly's experimental AOF (`--aof`). Its design document already lists the crash cases
  that are not solved yet, among them transactions that are not atomic across a crash.
- Disk faults beyond the explorer's draws, and the AOF's own fsync. The example has no AOF.
- Clock steps between servers: the three servers share the guest's one clock.
- Cluster mode, and `REPLTAKEOVER` together with clock steps or power cuts.
- The Mac engine and KVM: the example needs `guest.cpu: v3`, so it is for x86_64 only, and it
  ran on hvx only.

## What the onboarding needed from hydo

1. Dragonfly exits at start when `/proc/self/cgroup` is empty, and in hvx's guest that file is
   empty until a cgroup2 file system is mounted. Each service's `setup` mounts one. A container
   runtime always has one, so hydo could mount cgroup2 when it boots the guest, or the importer
   could warn.
2. A restarted service's earlier log (`/tmp/hydo/df1.log.old`) stays in the guest. The run
   directory's `logs/df1.log` held only the last start, and the reason of the first crash
   (the SIGILL) needed `hydo inspect`.
3. `guest.cpu: "{{cpu}}"` passes `hydo build --check`, but the explorer's corpus test refuses
   it, so the CPU level cannot be a parameter.
4. An exploration cannot leave out a fault kind. The two known causes (clock steps, power cuts)
   failed most schedules, so the takeover search needed a copy of the manifest with a shorter
   `faults:` list.
5. A check cannot know the faults' clock steps or restarts. The client records the primary's
   process id with every write and its own clock marks, as the Valkey example did.

## Onboarding retro

- **Time to the first clean run:** 29 minutes from the start, at 19:22 UTC. The import, the
  client and the folder took about 15 minutes; two start problems took the rest (see what took
  the most time).
- **Schedules run before the first finding:** 3. The fourth schedule of the first exploration
  (seed 4) failed with finding 1, about 35 minutes after the start.
- **The findings:** one new candidate (finding 1), one known bug (finding 2, #8646), and one
  known hardware requirement (the CPU level). The promise of `REPLTAKEOVER` held.
- **What took the most time:**
  - The first runs on hvx: Dragonfly exited because `/proc/self/cgroup` was empty. Then it
    stopped at its first `EVAL` with SIGILL on the v2 guest, and the reason was in the log of an
    earlier start, which only `hydo inspect` could read.
  - Rules that were too strict: a kill between a write and a snapshot, and an expiry just
    before a clock step back. The process id of the primary in every token fixed the first.
  - The takeover variant: the client's bookkeeping after a `REPLTAKEOVER` whose answer was
    lost gave 4 false liveness failures, and a shrink of each took minutes, because every run
    waits out `eventually_settle`.
  - The known causes (clock steps, power cuts) failed most schedules, so the takeover search
    needed a manifest without them.
- **What hydo lacked:** the five points of the previous section. The two that cost the most
  were the cgroup2 file system and the earlier start's log.
- **What the playbook should say:**
  - Mount a cgroup2 file system in `setup` for a program that reads its container limits at
    start and stops when `/proc/self/cgroup` is empty, as Dragonfly does, until hydo mounts it.
  - On hvx, try `guest.cpu: v3` early when a C or C++ program stops with SIGILL. Check whether
    a bundled library is built for a newer CPU than the program itself.
  - Give every write the server's process id, read on the same connection, so that a
    durability rule can tell a loss by design (a kill before a snapshot) from a bug.
  - In a failover test, the client must do the orchestrator's part: after a takeover whose
    answer is lost, find the real primary and point every replica at it. Without it, the
    liveness check reports the client's state, not the system's.
  - When one cause fails most schedules, explore a variant without its fault kind to look for
    the next cause.
