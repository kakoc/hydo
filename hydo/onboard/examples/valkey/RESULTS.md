# Valkey with replicas under hydo

Valkey is the open source key-value store that continues Redis. This example runs Valkey 9.1.3
as one primary and two replicas and hunts for bugs beyond the documented trade-offs. Such a
trade-off is, for example, the fsync window of `appendfsync everysec`, which the Redis examples
already show.

## The short answers

- **What was found?**
  1. **A new bug, reported upstream as [valkey-io/valkey#5406](https://github.com/valkey-io/valkey/issues/5406) (2026-11-14).** A backward step of the primary's wall clock
     delays a replica's full synchronization by the size of the step. The primary waits
     `repl-diskless-sync-delay` (5 seconds by default) before it starts the transfer, and it
     measures that wait with the wall clock. After a step of an hour back, the replica waits
     an hour and holds none of the primary's data. hydo found it blind in 4 of 237 explored schedules.
     Natively, with Valkey's default settings, the sync took 6 s without a step. A step of 60
     seconds back made it take 65 s, and a step of 120 seconds back 124 s. The same wall-clock lag
     also breaks `min-replicas-to-write`: after a step of 120 seconds back, a primary whose
     only replica had stopped acknowledged writes for 133 seconds instead of about 10.
  2. **A known bug, found blind.** `HSETEX ... KEEPTTL` over the last field of a hash, when the
     field has expired but is not reclaimed yet, loses the hash's key-level expiry after an AOF
     reload. A power cut and the restart are enough. Upstream issue
     [valkey-io/valkey#5354](https://github.com/valkey-io/valkey/issues/5354) (open) reports
     it for replicas and for AOF replay.
- **What held?** Every write acknowledged with `appendfsync always` survived every kill, pause,
  partition, power cut, and clock step of the explored schedules. Only a disk that drops
  acknowledged writes (`disk=lying`) lost some. The replicas converged with the primary after
  every fault except the two findings. No server returned a key or a hash field after the expiry
  it held.
- **What did hydo lack?** Six things, listed in the section about what the onboarding needed
  from hydo. One was a hydo bug, fixed with a test: `hydo save` from inside the `hydo/` folder wrote
  into a second, nested `hydo/regressions`.

## The version

Valkey 9.1.3 (2026-10-26) is the newest release. 9.2.0-rc2 is a release candidate. The
image is the official `valkey/valkey:9.1.3-alpine`, pinned by digest in `hydo/fetch-images.sh`
and in `hydo/hydo.yaml`. The code of finding 1 is the same in Valkey's `unstable` branch and in
Redis's `unstable` branch on 2026-11-14.

## The project

Valkey's repository has no Compose file, so this example has its own:
[docker-compose.yaml](docker-compose.yaml). It has a primary and two replicas. Each server runs
with an append-only file synced before every reply (`appendfsync always`), no RDB snapshots,
dual-channel replication, and a replication backlog of 64 KB. The small backlog makes a replica
that falls behind need a full synchronization, the path that finding 1 is on.

| file | what it is |
|---|---|
| `docker-compose.yaml` | the three servers, the input of `hydo onboard` |
| `build.sh` | builds `bin/vkc-amd64` and `bin/vkc-arm64` from `client/`, static Linux programs |
| `client/main.go` | `vkc`, the test client: every operation, the wait for the replicas, and the rules (standard library only) |
| `hydo/hydo.yaml` | the importer's three services, with the client's readiness check, a group of the three servers for cluster faults, and the parameters `dual_channel` and `backlog` |
| `hydo/workload/` | four parallel drivers, one serial driver, one anytime read, and the wait for the replicas before and after the faults |
| `hydo/checks/` | the three final rules |
| `hydo/regressions/` | the schedules of findings 1 and 2, and the passing neighbour of finding 1 |
| `native/` | finding 1 without hydo (its README) |

```sh
sh build.sh                     # needs Go 1.27.3
cd hydo && sh fetch-images.sh   # the Valkey image by digest, network once
hydo build hydo.yaml --arch arm64 --set arch=arm64     # on a Mac; on Linux: hydo build hydo.yaml
hydo run "disk=none | run"
hydo explore --seeds 1-40 --strategy process --new-faults --cluster-faults
```

The one change to the importer's services is the readiness check. The import took Compose's
`valkey-cli ping`. The example uses `vkc ready`, which requires the answer `PONG`, so a server
that is still loading its data is not ready.

## The workload

Each composer entry owns its keys, so no other operation changes them. Every operation ends by
reading its item back from the primary in one transaction: the value and the absolute expiry in
milliseconds. That reading goes to `$HYDO_ACKED` as the item's token
(`i<id>~<key>|<field>|<value>|<expiry>|<low>|<high>`). The low and high bounds are the
expiry that the client computed from its own clock around the command that set it.

| command | operation |
|---|---|
| `parallel_driver_set` | `SET s:<id>` to a value of 0 to 2,000 bytes, then `WAIT 2 500` |
| `parallel_driver_px` | a string with an expiry: `SET` with `PX`, `PXAT` or `EX`, or `PEXPIRE`; then sometimes `GETEX PX` or `PERSIST` |
| `parallel_driver_hsx` | a field of one of 4 shared hashes with an expiry: `HSETEX` with `PX`, `PXAT` or `FNX EX`, or `HSET` and `HPEXPIRE` |
| `parallel_driver_hmut` | one field through 2 to 5 seeded hash commands, for example `HINCRBY`, `HGETEX`, `HSETEX KEEPTTL` and a Lua script. Pauses between them let expiries of 30 ms to 60 s pass. One time in three the hash is the op's own, with a key-level expiry of 10 minutes |
| `serial_driver_rewrite` | `BGREWRITEAOF` on the primary, or on a replica one time in three |
| `anytime_read` | reads 8 acknowledged items from one server and applies the expiry rule at once |
| `first_settle`, `eventually_settle` | wait until both replicas are online and hold the primary's whole stream: 80 s before the test, 100 s after the faults heal |

## The rules

| rule | file | what must hold |
|---|---|---|
| durable | `checks/final_durable` | every acknowledged `SET` and item is on the primary as the client last saw it, unless its expiry has passed |
| expiry | `checks/final_expiry` | no server returns an item after its expiry; the primary keeps the expiry that the client set and saw |
| converge | `checks/final_converge` | each replica holds exactly the primary's keys, values and expiries, the key-level expiry of a hash included |
| liveness | `workload/eventually_settle` | after the faults heal, both replicas are online and caught up within 100 s |
| at once | `workload/anytime_read` and the operations | an item gone at the end of its operation stays gone; no item is readable after its expiry; the expiry is within the client's bounds |

The checks compare times with the guest's one clock, with 2 ms of tolerance. Items that expire
within 50 ms of a dump are not compared between servers.

Three rules were too strict at first, and the explorations showed it:

- A backward clock step. An item whose expiry passed before a step back is gone, although its
  expiry is now in the future again. Each operation writes the wall clock to `$HYDO_ACKED`
  (`t~<ms>`). A check requires an item only when its expiry is later than the latest of
  these marks.
- A lagging replica. During a partition or after a restart from its own AOF, a replica serves
  an older value or lacks an expiry it has not received yet. This is Valkey's documented
  asynchronous replication. The read on a replica now checks only that the replica returns no
  item after the expiry it holds itself.
- `PERSIST` on a key that had already expired. The client counted the key as persistent and
  missing; it now counts the bounds as unknown.

## Without faults, and the regressions

The final image, `explore-ob-valkey-arm64.cpio.gz` with sha256 `2853f04f…`, gave
`fp=d9c2a73b193da94c` twice for `disk=none | run` on the Mac engine with this tree's hydo. The
run starts 71 commands, and all of them end with exit status 0.

`hydo repro --regressions` gave 3 of 3 as expected:

| regression | expects | fp |
|---|---|---|
| `fullsync-delayed-by-clock-step` | violation (finding 1) | `d3c8c0fed4a3dbd8` |
| `fullsync-after-kill` | OK, the same kill without the step | `a47cc5f097d7cfad` |
| `known-5354-keepttl-ttl-lost-on-aof-reload` | violation (finding 2) | `ca9c575efc5bc795` |

## What was found

### 1. A backward clock step delays a full synchronization by its size (new candidate)

**The schedules.** hydo found it in exploration ex1 (seed 16), ex2 (seeds 16 and 24) and ex3
(seed 122). Each failing run kills a replica, or two, and steps the clock back while the
primary waits to start the replica's full sync. The shrinker reduced seed 122 to
`K=valkey3@3+3 T=18:-112`. The regression is `disk=none | run K=valkey3@2+3 T=10:-300`:

```text
VIOLATION [eventually_settle] eventually_settle exited rc=1: not settled after 1m40s :
offset 29992, replicas map[slave0:ip=valkey2,...,state=online,... slave1:ip=valkey3,...,state=wait_bgsave,offset=0,...
```

`final_converge` then finds valkey3 without any of the primary's keys. The same kill without the
step passes, and so does the step without the kill.

**The cause.** `shouldStartChildReplication` in `src/replication.c` decides when to start the
transfer for replicas in `WAIT_BGSAVE_START`:

```c
idle = server.unixtime - replica->last_interaction;
if (idle > max_idle) max_idle = idle;
...
max_idle >= server.repl_diskless_sync_delay
```

`server.unixtime` and `last_interaction` are wall-clock seconds. A replica that waits sends
nothing, so `last_interaction` stays at the time it asked for the sync. After a step of D
seconds back, `idle` is negative until the clock passes that time again. The transfer starts
D + 5 seconds after the request instead of 5. Valkey has a monotonic clock (`monotonic.h`) for
such intervals, and this code does not use it.

`refreshGoodReplicasCount` computes the lag of `min-replicas-max-lag` the same way
(`server.unixtime - repl_ack_time`). After a step back, a replica that stopped acknowledging
still counts as good for the size of the step. The replication timeout (`repl-timeout`)
compares the same two values. This second effect was not explored with hydo: the example does not
set `min-replicas-to-write`. It was checked natively only.

**Without hydo.** [native/](native/README.md) runs the official `valkey/valkey:9.1.3` image
with its default settings. Only the primary runs under libfaketime, a test tool that moves the
wall clock of one process and leaves its monotonic clock alone:

| primary's clock step | time until the replica is in sync |
|---|---|
| none | 6 s |
| 60 s back | 65 s |
| 120 s back | 124 s |

With `min-replicas-to-write 1` and `min-replicas-max-lag 10`, and the only replica frozen, the
primary refused writes after 11 s without a step. After a step of 120 seconds back, it
acknowledged 114 writes over 133 s, none of which reached a replica.

**The decision: a Valkey bug, inherited from Redis.** A backward step is a normal event: ntpd
steps a clock that was more than 128 ms off, for example after a virtual machine resumes. The
documentation of `repl-diskless-sync-delay` and of `min-replicas-max-lag` speaks of seconds of
waiting and of lag, and neither mentions the wall clock. The documented exception for the
wall clock is the expiry of keys, which stores absolute times by design. Dual-channel
replication does not matter: hydo ran with it, and the native run without it.

**The search for an earlier report.** Nothing reports it. Each phrasing was searched in issues and
pull requests, open and closed, on 2026-11-14:

- valkey-io/valkey: `repl-diskless-sync-delay clock`, `diskless sync clock`, `replication
  clock backwards`, `clock jumps backwards`, `system time backwards`, `wait_bgsave`,
  `last_interaction`, `unixtime monotonic replication`, `NTP`, `clock skew replica`, `time
  jump`, `time jumped`, `clock change`, `wall clock`, `monotonic`, `repl_ack_time`, `full sync
  delayed`, `replica never syncs`, `diskless sync delay`, `min-replicas-max-lag`,
  `min-replicas-to-write`, `NOREPLICAS`, `good replicas lag`.
- redis/redis: `time goes backwards`, `clock backwards`, `clock jump`, `diskless sync delay`,
  `repl-diskless-sync-delay`, `wait_bgsave_start`, `system time changed`, `ntp`, `monotonic
  clock replication`, `min-replicas clock`, `min-replicas-to-write clock`, `min-slaves-to-write
  time`, `min-replicas-max-lag`, `NOREPLICAS`, `repl-timeout clock`.
- The release notes of the last 40 Valkey releases, for `clock`, `wall`, `time jump` and
  `diskless-sync-delay`.

The nearest reports are about other code:

- Valkey issue 2651 (open): key expiry follows the wall clock.
- Valkey issue 5202 and pull request 5203 (open): the event loop's timers after a backward step
  of the monotonic processor clock, when a checkpoint is restored on another host.
- Redis issue 6416 (open): cluster state after a time jump, in the cluster bus.
- Redis pull request 7644 (merged): the monotonic clock for the event loop.

### 2. HSETEX KEEPTTL over the last expired field loses the key's expiry (known)

**The schedule.** Exploration ex3, seed 124, shrunk to a power cut at the end and 14 commands
of the composition: `disk=cut | run C=30` with `parallel_driver_hmut` and
`serial_driver_rewrite` (the regression has the full line). After the recovery boot:

```text
primary: hp:34 m34 has expiry -1, the primary reported 1794528621059 when its operation ended
```

**The cause.** A look inside the recovery boot with `hydo inspect exec` read the primary's
AOF:

```text
HSETEX hp:34 PXAT 1794528023953 FIELDS 1 m34 1
PEXPIREAT hp:34 1794528621059
HSET hp:34 m34 10
HPEXPIREAT hp:34 1794528021275 FIELDS 1 m34
HDEL hp:34 m34
HSETEX hp:34 KEEPTTL FIELDS 1 m34 20
```

`HSETEX KEEPTTL` over the field that had expired was written as `HDEL` and `HSETEX`. On replay,
the `HDEL` empties the hash and deletes the key with its expiry, and `HSETEX` makes the key
again without one. Before the cut the primary had the key with its 10-minute expiry.

**The decision: known.** [valkey-io/valkey#5354](https://github.com/valkey-io/valkey/issues/5354)
(open, 2026-11-11) describes this path for replicas and says the same happens on AOF replay.
Direct probes against Docker gave the same result for `HSETEX FNX KEEPTTL` and for
`HSETEX KEEPTTL` in a Lua script. The other 44 probed commands on such a field did not diverge,
for example `HINCRBY`, `HPERSIST`, `HGETDEL` and `COPY`. Issue
[#4732](https://github.com/valkey-io/valkey/issues/4732) (closed, fixed in 9.1.3) was the same
divergence for `HDEL`, `HGETDEL`, `HPERSIST` and `HEXPIRE`.

## Runs

237 explored schedules on the Mac engine with this tree's hydo, and about 20 directed runs:

| exploration | strategy | seeds | failures |
|---|---|---|---|
| ex1 | `process --new-faults --cluster-faults` | 1-17 of 1-40, stopped | 10: each broke a rule that was too strict, and seed 16 also had finding 1 |
| ex2 | the same, after the first two fixes of the rules | 1-40 | 4: finding 1 twice, the lagging replica twice |
| ex3 | the same, with every fix of the rules | 41-160 | 2: finding 1, finding 2 |
| ex4 | `swarm --new-faults --cluster-faults` | 201-260 | 28, each with a disk fault: 18 with I/O errors, 10 with a lying disk |

ex3 ran 120 schedules at about 1,400 an hour with three workers.

The failures of ex4 are the environment, not Valkey:

- With `disk=ioerr`, the disk keeps failing reads after the restart. Valkey then stops at
  `Fatal error: can't open the AOF manifest file appendonlydir/appendonly.aof.manifest for
  reading: I/O error`, and the run ends with `ERROR valkey1 restart failed`.
- With `disk=lying`, the disk drops writes it acknowledged. A power cut then loses writes that
  Valkey had synced, and in 3 runs the data disk did not mount.

## What was not covered

- Failover: the example has no Sentinel and no cluster mode, so `WAIT` before a failover and
  two primaries were not tested.
- hvx: finding 1 needs no timing, and the example ran on the Mac engine only.
- The second effect of finding 1, on `min-replicas-to-write`, ran natively only.

## What the onboarding needed from hydo

1. `hydo save` run inside the `hydo/` folder wrote into `hydo/hydo/regressions`, which no
   build reads, while `hydo run` and `hydo explore` work there. Fixed: it writes into
   `regressions/` there (a unit test in `hydo/crates/hydo-cli/src/runs.rs`).
2. A rule about expiry cannot know about the faults' clock steps. hydo knows each `T=` step,
   and the messages checker reads them from the console, but a check gets no such record. The
   client had to write its own clock marks into `$HYDO_ACKED`.
3. `hydo report` on a passing run shows `PASS` for each check, without the check's last line,
   such as `all 22 acknowledged SETs and 38 items as acknowledged`. Whether a check judged
   anything needed a look into the run directory.
4. The property table marks the project's own checks `[not in catalog]`, which reads as an
   error.
5. The lock file of the Compose import says `"generated_by": "hvx/onboard/hydo-compose"`, a path
   that no longer exists.
6. `hydo setup --archive` from a checkout's build points `~/.hydo/bin/hydo` at the new version,
   also for other sessions of the same user.
