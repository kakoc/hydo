# Garnet under hydo: a cluster of three shards with one replica each

Garnet (github.com/microsoft/garnet) is a cache and key-value store from Microsoft Research,
written in C#, that speaks the Redis protocol. In cluster mode it splits the key space into
16,384 slots, each owned by one primary. A primary logs every write to its append-only file (the
AOF), ships the AOF to its replicas, and takes checkpoints on `SAVE` and `BGSAVE`. Garnet's
cluster is passive: it holds no election, and a control plane outside it fails shards over
(`CLUSTER FAILOVER`) and moves slots (`MIGRATE ... SLOTS`). This example runs three primaries with
one replica each from the official image in one guest of hvx on the bare-metal machine, with a
client that plays the control plane, and looks for bugs that nobody reported.

## The short answers

- **What was found?** Four new candidates and one finding that needs Garnet's default of a
  setting. The last three were traced later (section "The three failures traced"). Findings 1
  and 2 are reported as microsoft/garnet#2441 and #2440; nothing else was posted.
  1. **A deployment setting, with a status that hides it: a replica never replicates again after
     its primary restarts.** With Garnet's default `--cluster-replication-reestablishment-timeout
     0`, a kill of a primary and its start with `--recover` leave its replica without the AOF
     stream for good. The replica keeps reporting `master_link_status:up`, and it lacks every
     write that the primary acknowledged after the restart
     (`regressions/replica-stops-after-its-primary-restarts.schedule`). With the option at 5 s
     the same schedule passes.
  2. **New: a failover right after a slot migration leaves the migrated slot without an owner, for
     good.** When `CLUSTER FAILOVER` comes before gossip has told the replica about a slot that
     `MIGRATE ... SLOTS` just gave its primary, the new primary takes over only its primary's old
     slots. No node serves the migrated slot any more, and its acknowledged writes are
     unavailable: `CLUSTERDOWN Hash slot not served`. The bug needs no fault, only a failover that
     comes before the gossip; Docker shows it without hydo
     (`regressions/slot-without-owner-after-a-failover-after-a-migration.schedule`).
  3. **New: with `--aof-commit-wait`, a replica stops answering when its primary dies before a
     commit (A).** The replica keeps an AOF tail that nothing commits, and every reply that waits
     for the commit blocks its thread for good
     (`regressions/replica-hangs-after-a-kill-during-a-checkpoint.schedule`).
  4. **New: a failover of a migration's source gives the migrated slot two primaries (B).** The
     source's replica takes over the slot from its old slot map with the epoch of the target's
     claim, both nodes acknowledge writes of the slot, and the loser's writes are lost
     (`regressions/acked-writes-lost-after-failovers-on-both-sides-of-a-migration.schedule`).
     Finding 2's cause, on the other side of the migration.
  5. **New: a replica that attaches after a failover damages its AOF file below the new tail (C).**
     After a power cut it replays the damaged file, stops at the damage without an error, and
     reports its primary's offset without the writes after it
     (`regressions/replica-differs-at-the-same-offset-after-a-power-cut.schedule`).
- **What held?** With the option at 5 s and the device of #2281's workaround, every write that
  Garnet acknowledged stayed on the owner of its slot, through failovers, slot migrations and
  checkpoints, under kills, pauses, partitions, one node's power loss, power cuts and clock steps,
  but in findings 2 and B. Transactions and scripts applied whole, counters stayed within
  their `INCR`s, expiries kept their deadlines, and no read returned a value that no write sent.
- **What is the environment?** A damaged cluster file stops its node at every start. A damaged
  checkpoint or AOF segment lets the node start without the data, as Garnet's default
  `--fail-on-recovery-error false` allows, and its replica then differs from it.
- **What is known?** A power cut of the guest loses writes that Garnet acknowledged with
  `--aof-commit-wait`, with Garnet's default device: microsoft/garnet#2281 (open). With
  `--device-type FileStream`, which that issue names, the same cuts lost nothing.
- **What did hydo lack?** The section "What hydo lacked".

## The version

- Garnet 2.2.14, the newest release (2026-11-23), the tag `v2.2.14`. The official image
  `ghcr.io/microsoft/garnet:2.2.14`: index
  `sha256:9ea4c7f379e0366e5647c94ddbe17710d3aba434a92d974658dd90e4a3efc4ed`, linux/amd64
  `sha256:c5f73c1ec85f260e40bb35ed3e9f10f431f1506cd95286c6063fc1675fae1ea1`. It runs .NET 10.0.15.
- The causes were read at `v2.2.14` and on the default branch `main` at `ff769f86`
  (2026-11-24), where they are unchanged.
- hydo: the published 0.6.0 Linux release (`12fe3299…`) for the image and the directed runs on
  hvx; this branch's hydo (0.6.0 and Milestones 363 to 367) on GitHub's KVM runners.
- The released hydo 0.7.0 replayed findings 1 and 2 on hvx again, from a fresh clone at
  `67ccb6ee`. Each finding's paragraph "The replay with hydo 0.7.0" has the runs.
- It traced findings A, B and C later, from `git archive` of `main` at `bdb403f4`: the section
  "The three failures traced".

## The promises

The promises come from Garnet's documentation of the cluster (`website/docs/cluster/`), of its
commands, and of its settings (`libs/host/defaults.conf`). No owner of Garnet confirmed them; the
ones marked "unconfirmed by the owner" are read from the code and the Redis semantics only.

| promise | source |
|---|---|
| with `--aof --aof-commit-wait`, an acknowledged write survives a kill and a restart with `--recover` | `defaults.conf`: "Wait for AOF to flush the commit before returning results to client" |
| a replica is an exact copy of its primary, and catches up after a restart | `replication.md`: "This allows the replica to be exact copies of the primary", and its section on checkpoints and recovery |
| `CLUSTER FAILOVER` without an option promotes a replica that has caught up with the reachable primary | `commands/cluster.md`, `CLUSTER FAILOVER`, steps 1 to 5 |
| a slot migration moves every key of the slot and loses none | `cluster/key-migration.md` |
| a transaction and a Lua script apply all of their writes or none | Redis semantics; unconfirmed by the owner |
| an expiry does not delete a key before its deadline | Redis semantics; unconfirmed by the owner |
| every node names the same owner for a slot once the gossip has settled | `cluster/overview.md`, "Garnet Cluster Configuration" |

Replication is asynchronous, and Garnet has no `WAIT`. So a forced failover (`FORCE`,
`TAKEOVER`) can lose writes that the replica had not received, as `replication.md` says. The
client never forces a failover.

## The project

`docker-compose.yaml` runs six nodes, `g1` to `g6`, from the official image, each with a volume
for `/data` and a fixed address. `config/start.sh` starts each with `--cluster --aof --recover
--lua` and the durability settings from the environment, the build parameters of `hydo.yaml`:

| parameter | setting | example's value | Garnet's default |
|---|---|---|---|
| `wait` | `--aof-commit-wait` | `yes` | `no` |
| `freq` | `--aof-commit-freq` (ms) | `0` | `0` |
| `ckpt` | `--checkpoint-freq` (s) | `0` | `0` |
| `device` | `--device-type` | `Default` | `Default` |
| `reest` | `--cluster-replication-reestablishment-timeout` (s) | `5` | `0` (finding 1) |

The memory settings are small, so that six nodes fit in one guest of 3,072 MiB: a hash index of
1 MiB, a log of 64 MiB in pages of 1 MiB, and an AOF buffer of 8 MiB. The nodes run as root, so
that a fresh volume needs no change of owner; the image's user is 1654. `hydo compose` imported
the file (`hydo/hydo.yaml`), with these changes: each node's `/data` is a file system of its own
(`storage:`), the readiness checks ask `PING` with the test client, each node depends on the
nodes before it, so that the explorer draws network faults between every pair, and the client
`gnc` is in the image. The three groups `shard1` to `shard3` name a primary and its replica, and
their `primary:` command names the member whose `ROLE` is `master`.

## The workload

`client/` is the Go client `gnc` on go-redis v9.23.0, without its retries, so that one command is
sent once (`sh build.sh` builds it). It reads `CLUSTER SLOTS` before each write and sends the
write to the owner of its slot, and it follows a `MOVED` or an `ASK` reply itself, since such a
reply means that the node did nothing. Every key carries one of 16 hash tags, so that the writes
fall into 16 slots and a migration moves keys. Its writes, one command file each in
`hydo/workload/`:

- registers: `SET` of a new key, `SET ... PXAT` with a deadline 3 s to an hour ahead, `SET` or
  `HSET` over a known register, `DEL`, and `HSET` of a new hash field;
- `INCR` of one of 16 counters;
- members: `RPUSH`, `SADD` and `ZADD` of a new element;
- `MULTI`, `SET`, `HSET`, `SADD`, `RPUSH`, `EXEC` on four new keys of one slot, and `EVAL` of a
  script that `SET`s and `SADD`s two new keys of one slot;
- `serial_driver_failover`: in one call of two, `CLUSTER FAILOVER` on the replica of one shard,
  a wait until it is the primary, and one late `SET` to the old primary, as a client does that
  has not learned of the failover;
- `serial_driver_migrate`: `MIGRATE <target> 6379 "" 0 5000 SLOTS <slot>` of one tag's slot to
  another primary, and a wait until both name the target as the owner;
- `serial_driver_save`: `BGSAVE` on one primary.

A write is acknowledged when the owner of its slot replied OK; it is recorded in `$HYDO_ACKED`,
`$HYDO_OUTCOMES` and the history `writes`. A reply that means Garnet did nothing (a redirect that
was not followed, `CLUSTERDOWN`, `TRYAGAIN`, `EXECABORT`, a `DEL` that found nothing) is a failed
write; a timeout or no answer is unknown. `hydo rules scan --kind writes` does not apply: Garnet
is a server of the Redis protocol, and the workload covers the families above.

## The rules

| check | promise | file |
|---|---|---|
| `writes` (`type: messages`, `acked_outcome` with `effect: recorded`) | every acknowledged write is there at the end, read from the owner of its slot, unless a later write of its register replaced it or its deadline passed; no refused write took effect | `hydo.yaml`, `gnc readback` |
| `eventually_converge` | within 120 s of the heal every shard has a primary and a replica that follows it and holds what it holds, key for key, and no node holds a key of a slot that its shard does not own | `checks/eventually_converge` |
| `eventually_slots` | within 120 s of the heal every node names the same owner for every slot of the test, and that owner is a primary | `checks/eventually_slots` |
| `final_atomic` | on every node each transaction and script has all of its writes or none | `checks/final_atomic` |
| `final_counters` | each counter is at least its acknowledged `INCR`s and at most those and the unknown ones | `checks/final_counters` |
| no phantom read | a read of any node returns only values that some write sent | `anytime_read` |

## Without faults, and the regressions

The final images were built with the published 0.6.0 Linux release on the bare-metal machine,
with the client `bin/gnc-amd64` `f3951b59…` built from `client/main.go` by Go 1.27.3, at the
fingerprint epoch 6:

| image | build parameters | `explore-ob-garnet-disk.img` |
|---|---|---|
| default | the example's: `reest=5`, `device=Default`, `wait=yes` | `f9d61a2f…` |
| `r0` | `--set reest=0`, Garnet's default re-establishment | `c5bc4e25…` |
| `fs` | `--set device=FileStream`, #2281's workaround | `57aaef25…` |

Two runs of `disk=none | run` on the default image give one fingerprint, `96eee05052238c8b`. The
run has 91 commands, of which one failed for certain (a `CLUSTERDOWN` during a failover), two
failovers of shard 3, three slot migrations, and 71 acknowledged writes judged by
`acked_outcome`. A run takes 50 to 60 s on hvx and 200 to 350 s on GitHub's KVM runners.

The saved regressions, each with one fingerprint over two runs on hvx. `hydo repro
--regressions` on each of the three images replayed the regression of its build parameters as
expected, with the same fingerprint, and left the other two out, which need other parameters.

| file | finding | image | verdict | fingerprint |
|---|---|---|---|---|
| `replica-stops-after-its-primary-restarts.schedule` (`# params: reest=0`) | 1 | `r0` | `converge`: `g4` lacks 18 keys and holds 5 of moved slots | `982a16effdb11bfa` |
| `slot-without-owner-after-a-failover-after-a-migration.schedule` (`# params: device=FileStream reest=5`), seed 96 of e2 | 2 | `fs` | `converge` and `slots`: `CLUSTERDOWN Hash slot not served` | `5a4771d51585eeff` |
| `acked-writes-lost-at-a-power-cut-default-device.schedule` (`# params: device=Default reest=5`) | known, #2281 | default | `acked-outcome`: 5 of 20 acknowledged writes lost | `bb4299751e2f04c0` |

The runs that separate the causes, one run each unless two are named. "Client 2" is the client
before the final one (`5e2970e7…`), which printed fewer lines in its checks; "client 1" is the one
before that, which also printed go-redis's own log lines:

| schedule | image | verdict | fingerprint |
|---|---|---|---|
| `disk=none \| run K=g1@15+3` | `r0` | `converge` (two runs, and two with hydo 0.7.0) | `982a16effdb11bfa` |
| the same | `r0`, client 2 | the same (two runs) | `59e40847341696b9` |
| the same | default (`reest=5`), with hydo 0.7.0 | OK (two runs) | `179ac0194c742e8d` |
| the same | default, client 2 (`reest=5`) | OK | `8ccf4e5c0b279c83` |
| `disk=none \| run K=g4@15+3`, a kill of the replica | client 1, `reest=0` | OK | `caa84a69cc7b1e77` |
| `disk=cut \| run C=10` | default | `acked-outcome`, 5 of 20 lost (two runs) | `bb4299751e2f04c0` |
| the same | `fs`, client 2 | OK | `9e3c1c88ce016ff7` |
| the same | `r0`, client 2 | OK: the cut came at another moment among the writes | `8ade502b3f3f1618` |
| `disk=cut \| run C=30` | client 1, `device=Default` | `counters`: an acknowledged `INCR` lost | `b1cadec26fddd8bf` |
| the same | client 1, `device=FileStream` | OK | `c61ba559715f5773` |
| `disk=none \| run A=garnet.migrate_done:kill-now-g2`, the target killed when it serves the slot | default, client 2 | OK | `b3e7c1b70e0d53b5` |
| `disk=none \| run A=garnet.migrate_done+500:kill-g2` | the same | OK | `792097ed1ae6a126` |
| `disk=none \| run A=garnet.failover_done:kill-now-g6`, the new primary killed at once | the same | OK | `dcd677f939aee5b4` |
| `disk=none \| run A=garnet.migrate_started:kill-now-g1`, the source killed during the migration | client 1 | OK | `bc46eda8f7048cdd` |
| `disk=none \| run T=20:-3600` | client 1 | OK | `563e511518a6a354` |
| `disk=none \| run T=20:3600` | client 1 | OK | `bdfb017e84967692` |
| `disk=cut \| run A=garnet.bgsave+20:cut` | `fs`, client 2 | OK | `3bb8389fa91b232a` |
| `disk=cut \| run A=garnet.migrate_done:cut` | the same | OK | `d4c1c1af5ed986b7` |
| `disk=cut \| run A=garnet.failover_done:cut` | the same | OK | `d4c1c1af5ed986b7`: the cut came at the same stop as the one before |
| `disk=cut cf=300 \| run` | the same | OK | `beb19a39ed210ae8` |
| `disk=cut cf=600 \| run` | the same | OK | `c2abf6c4e69956c8` |
| seed 96 of e2: `disk=cut dseed=0 \| run K=g6@15+3 N=g1-g5@27-29:loss,65% C=40 P=g4@0+6 T=37:3600 W=9abb9e17,p3,q2,r250 L=g3@29+3` | `fs` | `converge` and `slots`: `CLUSTERDOWN Hash slot not served` (finding 2, two runs, and two with hydo 0.7.0) | `5a4771d51585eeff` |
| the same | `fs`, client 2 | the same | `8ef9c39df29e6745` |
| its composition and the power cut alone, `disk=cut dseed=0 \| run C=40 W=9abb9e17,p3,q2,r250` | `fs`, client 2 | the same | `8a2f907f37e1dbae` |
| seed 96 without its clock step `T=37:3600` | `fs`, client 2 | the same | `99972f8e8b388925` |
| seed 96 without its power cut, `disk=none` and no `C=` | `fs`, client 2 | the same | `870c0b0be3950847` |
| its composition and the power cut alone | `fs` | OK: the failover came after the gossip | `8b39a130cb9c906b` |
| its composition alone, `disk=none dseed=0 \| run W=9abb9e17,p3,q2,r250` | default | OK, the same (two runs) | `e7ca4e3b785a3a18` |

## What was found

### 1. A replica never replicates again after its primary restarts, and reports its link up

**Label: a deployment setting, with a status that hides it (Garnet's default
`--cluster-replication-reestablishment-timeout 0`).** The symptom of the status is the one that
microsoft/garnet#2087 reported for another trigger. That trigger was a client's write on the
replica through `READWRITE`, and #2109 fixed it by a change of `READWRITE` only.

`disk=none | run K=g1@15+3` kills the primary `g1` of shard 1 before step 15 and starts it again
3 steps later. `g1` recovers its data from its AOF and serves its slots. Its replica `g4` was not
restarted. 120 s after the heal the check `eventually_converge` still fails:

```text
the replica g4 holds 5 keys of slots that its shard does not own: {t3}u19a (slot 685), {t3}u19c (slot 685), {t3}u35a (slot 685), {t3}u35c (slot 685), {t7}c7 (slot 553)
the replica g4 lacks {t14}l14, which its primary g1 holds as list [v88]
the replica g4 lacks {t15}t47a, which its primary g1 holds as string v47
the replica g4 lacks {t15}u63a, which its primary g1 holds as string v63
the replica g4 lacks {t6}t86a, which its primary g1 holds as string v86
shard 1: the primary g1 has master_repl_offset 5984 and replid 418b286fc650e9979294f0897bc697e235885143; the replica g4 has master_repl_offset 1668, slave_read_repl_offset 1668, replid 418b286fc650e9979294f0897bc697e235885143, link up
a replica differs from its primary in 19 keys
```

So `g4` holds none of the writes that `g1` acknowledged after its restart, and it still holds the
keys of the slots 685 and 553 that moved from `g1` to `g6` and `g2` after the restart. Its offset
stays at 1,668 while `g1`'s is at 5,984, and it reports `master_link_status:up`. A forced failover
of shard 1 (`CLUSTER FAILOVER FORCE`, the documented step when the primary is gone) would then
promote a replica without any write since the restart. If one of the moved slots came back to
shard 1, its stale keys would come back with it. A default `CLUSTER FAILOVER` does not promote
it: `native/failover-after-primary-restart.sh` showed it abort with `replication stream from the primary is not
active`.

The cause, at `v2.2.14` and unchanged on `main`:

1. The primary drives the AOF stream to a replica only after the replica attached
   (`CLUSTER REPLICATE`). A primary that restarts has no replica session, and it starts none.
2. A replica attaches again by itself only in `ReplicationManager.EnsureReplication`
   (`libs/cluster/Server/Replication/ReplicationManager.cs`, line 183), which each gossip message
   calls (`libs/cluster/Session/RespClusterBasicCommands.cs`, line 465). Lines 185 to 190 read
   the option and return at once when it is 0: `if (pollFrequency == 0) { // Disabled
   return; }`. 0 is the default of `libs/host/defaults.conf` (line 598,
   `"ClusterReplicationReestablishmentTimeout": 0`).
3. The replica's `master_link_status` is the state of its cluster connection to the primary, the
   connection of the gossip, not of the AOF stream (`Gossip.GetPrimaryLinkStatus`,
   `libs/cluster/Server/Gossip/Gossip.cs`, line 232). The cluster connection comes back after the
   restart, so the replica says `up`.

**The replay with hydo 0.7.0.** The released hydo 0.7.0, from a fresh clone of `main` at
`67ccb6ee`, built the three images again on hvx with the sha256 values of the section "Without faults,
and the regressions" (`f9d61a2f…`, `c5bc4e25…` and `57aaef25…`). `disk=none | run K=g1@15+3`
gave the same `converge` violation on the `r0` image with `982a16effdb11bfa` twice. On the default image, with
`reest=5`, it passed twice with `179ac0194c742e8d`: 91 commands, and 69 writes judged by
`acked_outcome`. A held run of the `r0` image (`hydo inspect start <run> --at end`) read `INFO
replication` of both nodes of shard 1:

```text
g1: role:master  connected_slaves:0  master_repl_offset:5984  sync_driver_count:0
g4: role:slave  master_host:10.89.0.11  master_link_status:up  master_last_io_seconds_ago:2
    master_repl_offset:1668  slave_read_repl_offset:1668
```

So the primary has no replica session, and the replica reports its link up at an offset that
stays 4,316 bytes behind.

With `--cluster-replication-reestablishment-timeout 5` (the example's default `reest=5`) the
same schedule passes on hvx, and the replica holds every key. A kill of the replica `g4` instead
of the primary passes with either value: the replica attaches when it starts.

`native/replica-after-primary-restart.sh` reproduces it with Docker, without hydo, with a primary
and one replica: after a `SIGKILL` of the primary and its start with `--recover`, the replica
holds 0 of 10 keys written after the restart, 60 s later, with `master_link_status:up`, while the
primary reports `connected_slaves:0`. With `REEST=5` it holds all 10, and the primary lists it
online at the same offset. The cause is plain in the code after this run, so the script is the
only native run.

The documentation of the option is one comment line of `defaults.conf` (line 597), "Maximum
frequency cluster Replicas will attempt to re-establish replicating their Primaries, in
seconds". Its declaration in `libs/host/Configuration/Options.cs` (line 777) has no `HelpText`,
and its row of `website/docs/getting-started/configuration.md` has no description and no
default. None of them says that 0 turns it off, and `website/docs/cluster/replication.md` says that "replicas can recover from their own
checkpoint and replay their AOF before connecting to their primary and resuming
synchronization". Garnet's cluster is passive by design (`cluster/overview.md`, "Control
Plane"), so a control plane could attach the replica again with `CLUSTER REPLICATE`. It would
need to know that it must: the replica's own status says that nothing is wrong. It is reported
as microsoft/garnet#2441 (2026-11-27).

Regression: `regressions/replica-stops-after-its-primary-restarts.schedule`, on an image built
with `--set reest=0`.

### 2. New: a failover right after a slot migration leaves the migrated slot without an owner, for good

**Label: a bug of the project, new as far as the search found.**

The second exploration (e2, `device=FileStream`) failed seeds 96 and 99 on KVM in the recovery
boot after a power cut. Every node answered, but the slot of one hash tag had no owner, so the
read-back could not read its writes and the checks ended `CLUSTERDOWN Hash slot not served`.
Both seeds had moved a slot from `g2` to `g1` and failed shard 1 over to `g4` right after,
seed 96 the slot of `{t1}` and seed 99 that of `{t5}`:

```text
gnc: slot 8943 (tag t1) moved from g2 to g1
gnc: failover of shard 1 from g1 to g4
```

On hvx, seed 96's schedule failed the same way, and so did its composition with the power cut
alone, `disk=cut dseed=0 | run C=40 W=9abb9e17,p3,q2,r250` (`8a2f907f37e1dbae`), and seed 96
without its power cut. The power cut is not needed. The slot lost its owner at the failover: the
next writes of the tag failed with `slot 8943 has no owner`. With the final client, the check
`eventually_slots` prints each node's view after the heal of seed 96's schedule (the client read
the string reply of `CLUSTER SLOTSTATE` as a list, hence that field's error):

```text
g1 names - as the owner of slot 8943; slotstate redis: unexpected type=string for Slice, role slave, cluster_current_epoch:16 cluster_my_epoch:16
g2 names g1 as the owner of slot 8943; slotstate redis: unexpected type=string for Slice, role master, cluster_current_epoch:16 cluster_my_epoch:2
g3 names - as the owner of slot 8943; slotstate redis: unexpected type=string for Slice, role master, cluster_current_epoch:16 cluster_my_epoch:13
g4 names - as the owner of slot 8943; slotstate redis: unexpected type=string for Slice, role master, cluster_current_epoch:16 cluster_my_epoch:14
g5 names - as the owner of slot 8943; slotstate redis: unexpected type=string for Slice, role slave, cluster_current_epoch:16 cluster_my_epoch:7
g6 names - as the owner of slot 8943; slotstate redis: unexpected type=string for Slice, role slave, cluster_current_epoch:16 cluster_my_epoch:15
the nodes name different owners of slot 8943
```

The read-back judged 43 of the run's 44 acknowledged writes and left the one of slot 8943
undecided: no node serves it.

**The replay with hydo 0.7.0.** The released hydo 0.7.0, from a fresh clone of `main` at
`67ccb6ee`, built the `fs` image again on hvx with the same sha256 (`57aaef25…`). Seed 96's
schedule gave the `converge` and `slots` violations with `5a4771d51585eeff` twice, and the check
printed the same six views, line for line. A held run (`hydo inspect start <run> --at end`)
asked every node for its `CLUSTER NODES`, `CLUSTER SLOTS` and a `GET` of a key of the tag:

| node | role | the owner of slot 8943 in its `CLUSTER SLOTS` | `GET {t1}k` |
|---|---|---|---|
| `g1` | replica of `g4` | none | `CLUSTERDOWN Hash slot not served` |
| `g2` | primary | `g1` | `MOVED 8943 10.89.0.11:6379`, the address of `g1` |
| `g3` | primary | none | `CLUSTERDOWN Hash slot not served` |
| `g4` | primary of shard 1 | none | `CLUSTERDOWN Hash slot not served` |
| `g5` | replica of `g2` | none | `CLUSTERDOWN Hash slot not served` |
| `g6` | replica of `g3` | none | `CLUSTERDOWN Hash slot not served` |

`g2`'s `CLUSTER NODES` lists `g1` as `slave 8558ef5c… 16 connected 8943`: a replica with the
slot. No other node's `CLUSTER NODES` lists slot 8943 for any node.

What happens, step by step, at `v2.2.14` (the same code is on `main` at `ff769f86`):

1. `MIGRATE <g1> 6379 "" 0 5000 SLOTS 8943` on `g2` moves the slot's keys to `g1`. The migration
   driver gives the slot to `g1` on `g1` and on `g2` and then gossips with `g1` alone
   (`libs/cluster/Server/Migration/MigrationDriver.cs`, lines 201, 210 and 219). The client waits
   until both name `g1` in `CLUSTER SLOTS`, the check that
   `website/docs/cluster/key-migration.md` names. The other nodes, among them `g1`'s replica
   `g4`, learn of the change later, by gossip.
2. The client sends `CLUSTER FAILOVER`, without an option, to `g4` at once. `g4` waits until it
   has replicated `g1`'s AOF, which holds the migrated keys, and takes over
   (`TakeOverAsPrimaryAsync`, `libs/cluster/Server/Failover/ReplicaFailoverSession.cs`, line
   262, which calls `TryTakeOverForPrimary` at line 285).
3. `ClusterManager.TryTakeOverForPrimary` (`libs/cluster/Server/ClusterManager.cs`, line 380)
   calls `ClusterConfig.TakeOverFromPrimary` at line 389. That function
   (`libs/cluster/Server/ClusterConfig.cs`, line 1393) claims the slots of
   `GetLocalPrimarySlots()` (line 334), which reads this node's own slot map. `g4`'s slot map
   still gave slot 8943 to `g2`, so `g4` did not claim it.
4. `g1` becomes a replica of `g4` (`MakeReplicaOf`, line 1365).
5. Gossip never gives the slot an owner again. Every node merges each gossip message in
   `MergeSlotMap` (line 1217). When a primary's message does not claim a slot that this node
   gives to that primary, lines 1239 to 1258 set the slot `OFFLINE` with no owner. So `g2`'s
   messages, which no longer claim slot 8943, take it from `g2` on every node that had not yet
   learned of the migration. A replica's message cannot give an owner to such a slot: line 1277
   is `if (currentOwnerId == RESERVED_WORKER_ID) continue;`, with the comment "a replica must
   never introduce ownership" (from pull request #2089). Only a primary's claim could, and no
   primary claims the slot: `g4` did not take it, and `g1` is a replica.

This state holds across a restart of every node and a power cut. The keys of the slot are on
`g4`, which replicated them, but no node serves them. Every acknowledged write of the slot is
unavailable for good, and so is every new write of its keys.

`native/slot-lost-after-failover.sh` reproduces it with Docker, without hydo, with three
primaries `a`, `b` and `c`, and `ra`, the replica of `a`. It moves slot 8943 from `b` to `a` and
sends `CLUSTER FAILOVER` to `ra` at once. On this Mac three runs gave two end states, and in both
the slot stays unserved, also 30 s after a `SIGKILL` and a start of all four nodes:

- In two runs, as in hydo's run, `a` (the old primary), `c` and `ra` (the new primary) name no
  owner, and `b` names `a`. A `GET` through `b` answers `MOVED` to `a`, and through `ra`
  `CLUSTERDOWN Hash slot not served`.
- In one run, `a`, `b` and `c` name `ra`, and `ra` names none. `ra` had set the slot `OFFLINE`
  right after the migration, before the failover, when `b`'s gossip no longer claimed it (line
  1248). So `ra`'s takeover did not claim it, and line 1277 keeps the replica `a`'s messages
  from giving it to `ra`. A `GET` through `b` answers `MOVED 8943` to `ra`, and through `ra`
  `CLUSTERDOWN Hash slot not served`.

With `MIGSLEEP=6`, a wait of 6 s between the migration and the failover, gossip reaches `ra`
first, `ra` takes the slot over, every node names `ra`, and a `GET` through `ra` answers `v1`.

The documentation of `CLUSTER FAILOVER` asks a control plane to serialize failovers and to check
that the reachable nodes agree on the new owner after a failover. It says nothing about a
migration before it, and the control plane cannot repair the slot with a failover: every node but
`g2` names no owner. `CLUSTER SETSLOT 8943 NODE <g4>` on the nodes would. No issue or pull request
named it before (section "The duplicate searches"). It is reported as microsoft/garnet#2440
(2026-11-27).

Regression: `regressions/slot-without-owner-after-a-failover-after-a-migration.schedule`, seed
96's schedule on the `fs` image. The race depends on the moment of the failover: on the final
image the same composition without faults, and with the power cut alone, passed, because the
gossip came first there. The faults of seed 96 keep the order that fails.

### Known: a power cut loses acknowledged writes with the default device (microsoft/garnet#2281)

`disk=cut | run C=10` on the default image loses 5 of 20 acknowledged writes, all of them
acknowledged with `--aof --aof-commit-wait`, in the last seconds before the cut. The open issue
#2281 has the cause: the default device on Linux writes the AOF and its commit records with
`O_DIRECT` and never syncs, so the file system's metadata of the growing AOF segment is not on
the disk. Its workaround `--device-type FileStream` (the build parameter `device`) passed the same
cut, and `C=30` too. The explorations of the default image hit it with every power cut that came
seconds after a write (section "The explorations"). Regression:
`regressions/acked-writes-lost-at-a-power-cut-default-device.schedule`.

### The three failures traced

The last parts of the explorations gave three more failures on the `fs` image, each with one
fingerprint over two runs on KVM. A later round traced them on hvx with the released hydo 0.7.0,
from `git archive` of `main` at `bdb403f4`. It used the 0.7.0 Linux tarball, checked by its
`SHA256SUMS` and installed with `install.sh --no-module`, and the client `f3951b59…` built on
the Mac from the archive. It built the `fs` image again with the recorded sha256 (`57aaef25…`).
Each regression gave its recorded value twice. Held runs (`hydo inspect start <run> --at <stop>`)
read every node's `INFO`, `CLUSTER NODES`, keys, threads, and log at stops before and after each
failure.
All three are bugs of Garnet: A and C are new, and B has finding 2's cause.

The runs that separate the causes, on hvx with hydo 0.7.0, one run each unless two are named.
The `nw` image is the `fs` image built with `--set wait=no` (`af1e228c…`), and `tr2` is the copy
with Garnet's log level `Information` (`c2b9188f…`):

| schedule | image | verdict | fingerprint |
|---|---|---|---|
| A: `disk=none dseed=0 \| run W=69274d70,p3,q1,r500,oparallel_driver_del A=garnet.bgsave+10:kill-now-g1` | `fs` | `converge`: `g4` does not answer (two runs) | `e12c7c93adf9d46c` |
| A with the kill at the event, `+0` | `fs` | the same | `6efd2887a84c26ad` |
| A with the kill 2 s after the event, `+2000` | `fs` | OK | `66518ef67d425f02` |
| A without `--aof-commit-wait` | `nw` | OK | `8cf849360412507e` |
| A | `tr2` | `converge`: `g4` does not answer | `bb0d392e801a000b` |
| B: `disk=none dseed=0 \| run P=g1@12+3 W=0a39fa3d,p1,q2,r500` | `fs` | `converge` and `acked-outcome`: 2 of 45 lost (two runs) | `674c2b16f5e9438b` |
| B without the failovers, `W=0a39fa3d,p1,q2,r500,oserial_driver_failover` | `fs` | OK: 44 judged, with the same migration of slot 13404 | `4e7ef3a4bc87e9db` |
| C: `disk=cut dseed=0 \| run K=g4@22+1 C=40 W=a0075364,p1,q1,r500` | `fs` | `converge`: `g1` differs in 8 keys (two runs) | `012c520d5bb92ffc` |
| C without the power cut, `disk=none` and no `C=` | `fs` | OK | `057b2b492432c418` |
| C | `tr2` | the same as on `fs` | `3a8118d0f88895df` |
| C without the kill of `g4`, `disk=cut dseed=0 \| run C=40 W=a0075364,p1,q1,r500` | `tr2` | OK: the failover of shard 1 and the power cut came too | `1123bb3544856201` |

### A. New: with `--aof-commit-wait`, a replica stops answering when its primary dies before a commit

**Label: a bug of the project, new as far as the search found.** The run holds a second failure,
a lost `RPUSH`, which is finding B's.

Seed 144 of e3 shrank to `disk=none dseed=0 | run W=69274d70,p3,q1,r500,oparallel_driver_del
A=garnet.bgsave+10:kill-now-g1`
(`regressions/replica-hangs-after-a-kill-during-a-checkpoint.schedule`). The released hydo 0.7.0
replayed it twice on hvx with `e12c7c93adf9d46c`, the recorded value. `kill-now-g1` kills the
primary `g1` of shard 1 10 ms after the client's `BGSAVE`, and `g1` stays down until the heal.
120 s after the heal its replica `g4` still answered no command.

The replays' held runs, and the same schedule on an image with Garnet's log level `Information`
(a copy of the example with `--logger-level Information`, not committed), show this. The trace
image failed the same way (`bb0d392e801a000b`).

- `g4` received the checkpoint's end marker. At 12:00:25 of guest time it logged `Initiating
  checkpoint; full = True` and `Completed checkpoint`, and then `ReplicaReplayTask - terminating`
  when `g1` died. `g1` started again with its AOF at tail 404, and that checkpoint's `info.dat`
  was empty: `Skipping unreadable HybridLog checkpoint ... Invalid metadata length 0`. So `g1`
  died before it committed what `g4` had received.
- `g4` stopped answering long before `g1` came back. In the replays every other node logged
  `GOSSIP to remote node [g4] timeout` from 12:00:29 on, 4 s after `g1` died. At the stop
  `run:1700000`, with `g1` still down, every node but `g4` answered `PING`.
- `g4`'s process gathered threads that never ran. At `run:1700000` it had 31 threads, 19 of them
  .NET thread-pool workers waiting in `futex_wait_queue`. The other nodes had 18 or 19 threads in
  all. At the end of the trace run it had 93 threads. 80 of them were waiting workers, and 78 of
  those had used no CPU time. The other nodes had 15 to 17 threads.

What happens, at `v2.2.14` (unchanged on `main` at `ff769f86`):

1. A primary streams its AOF to its replica before the AOF is committed
   (`libs/cluster/Server/Replication/PrimaryOps/AofOperations/AofSyncTask.cs`, line 343,
   `scanUncommitted: true`). The replica appends each record without a commit
   (`ReplicaOps/AOFReplay/ReplicaReplaySession.cs`, line 104) and commits only when the primary's
   commit record arrives (`ReplicaReplayDriver.cs`, line 193). A primary that dies between a
   record and its commit leaves the replica's AOF tail past its commit point, and nothing commits
   it later.
2. With `--aof-commit-wait` a reply waits until the AOF has committed up to its tail. A client
   session waits for every command that is not in a short list (`libs/server/Resp/Parser/
   RespCommand.cs`, lines 468 to 523, which leaves out `CLUSTER`, `ROLE` and every data command),
   in `RespServerSession.Send` (`libs/server/Resp/RespServerSession.cs`, lines 1566 to 1571). A
   cluster session waits for every reply (`libs/cluster/Session/ClusterSession.cs`, lines 222 to
   226). Both block their thread, as the code's comment says: `Must block, we're on the network
   thread`.
3. On `g4` the commit never comes, so each gossip message from a peer blocks one thread for good.
   The peers connect again after each timeout, and the waiting workers grew: 19 at `run:1700000`
   of the replay, 80 at the end of the trace run. The checks' `INFO`, which does not wait for
   the AOF, got no answer either.

The same schedule passes without `--aof-commit-wait`, and it passes with the kill 2 s after the
`BGSAVE`. With the kill at the event itself it fails the same way. The table of the separating
runs has the values.

The 0.6.0 diagnosis and the replays at the example's level `Warning` also show one more line on
`g4`, at 12:03:17, after `g1` came back: `Error background recovering task has not completed
[ReadRole]`. A resync that `EnsureReplication` tried found another recovery in progress. Which
one was not traced. The example sets `--aof-commit-wait` (`wait=yes`); Garnet's default leaves it
off.

Milestone 383 read every guest thread's stack at the verdict's stop of the recorded hvx run with
`hydo inspect stacks` (6,315,089 stops replayed in 696 s, with no divergence). The replica `g4`
(pid 368, its service told apart by its `HYDO_SVC`) had 90 threads, against 16 to 19 in each
other node. 78 of them were `.NET TP Worker` threads of the thread pool, and 85 of its threads
slept in `futex`, each on its own address, so no lock is shared and no cycle shows. A thread pool
that keeps adding workers while every worker waits is what .NET does when its workers block. That
fits a replica whose background recovery task never completes and whose commands get no worker.
The frames are the runtime's and the JIT's code, which hydo does not name, so the place where the
workers wait is not traced yet.
So step 3 rests on the thread counts, these stacks and the code.

**The lost `RPUSH` is finding B.** The client moved slot 12874 from `g6` to `g5` and then failed
shard 3 over from `g6` to `g3`. `g3` took over the slot from its old map with a newer epoch, and
the slot went back to shard 3. At the end of the trace run every node named `g3` the owner of
slot 12874, and the list `{t4}l4`, which the migration had moved, stayed on `g5` and `g2`.

### B. New: a failover of a migration's source gives one slot two primaries, and the loser's writes are lost

**Label: a bug of the project, new as far as the search found. It has the cause of finding 2,
on the source's side of the migration.**

Seed 89 of e2 shrank to `disk=none dseed=0 | run P=g1@12+3 W=0a39fa3d,p1,q2,r500`
(`regressions/acked-writes-lost-after-failovers-on-both-sides-of-a-migration.schedule`). The
released hydo 0.7.0 replayed it twice on hvx with `674c2b16f5e9438b`, the recorded value. The
client moved slot 13404, the slot of `{t13}`, from `g3` to `g1`, and failed shard 3 over from
`g3` to `g6` right after. Two writes of the tag were acknowledged then: `zadd.q13` and
`rpush.l13`. Then it failed shard 1 over from `g1` to `g4`. After the heal `g6` and its replica
`g3` held both keys, which shard 3 no longer owned, and the owner lacked them:
`writes.acked-outcome` broke with 2 of 45 acknowledged writes.

Held runs (`hydo inspect start runs/B1 --at <stop>`) read every node's `CLUSTER NODES` at four
stops:

| stop | what the nodes said about slot 13404 |
|---|---|
| `run:2580000`, the migration done | `g1` and `g3` name `g1`, whose config epoch is 10. `g6`, the replica of the source, still gives `g3` the whole range 10923-16383 |
| `run:2710000`, after the failover of shard 3 and the two writes | `g1` claims 13404 with config epoch 10, and `g6` claims 10923-16383, 13404 among them, also with config epoch 10. `g2` and `g3` name `g1`. `g6` holds `{t13}l13` and `{t13}q13`, and `g1` holds the migrated keys `{t13}t29a` to `t29d` |
| `run:2805000`, after the failover of shard 1 | `g4` claims 13404 with config epoch 13, and `g6` still claims it, now also with 13. `g4` holds `{t13}z13`, which the client wrote at step 27 |
| the end | every node names shard 1. `g6` and `g3` hold `l13` and `q13`, keys of a slot that their shard does not own |

So two primaries served slot 13404 from the failover of shard 3 on, and both acknowledged
writes of it. What happens, at `v2.2.14` (unchanged on `main` at `ff769f86`):

1. The migration gives the slot to `g1` on `g1` and on `g3` and gossips with `g1` alone, as in
   finding 2 (`MigrationDriver.cs`, lines 201, 210 and 219).
2. `CLUSTER FAILOVER` on `g6` comes before gossip has told `g6` of the migration.
   `ClusterManager.TryTakeOverForPrimary` (`libs/cluster/Server/ClusterManager.cs`, line 389)
   calls `TakeOverFromPrimary`, which claims the slots that `g6`'s own slot map gives `g3`
   (`ClusterConfig.cs`, lines 1393 and 1400). That map still holds 13404. The same line bumps
   `g6`'s config epoch, here to 10, the epoch of `g1`'s claim.
3. `MergeSlotMap` (line 1217) lets a primary's claim replace the current owner only when the
   claim has a newer config epoch (lines 1262 to 1268). With equal epochs no node moves the slot
   to the other claimant, so `g1` and `g6` both keep serving it. Clients that read the slot map
   from `g6`, or from a node that still named shard 3, sent the slot's writes to `g6`.
4. Gossip settles the epoch collision by bumping one node's epoch (`HandleConfigEpochCollision`,
   line 1588). The failover of shard 1 bumps `g4`'s epoch past `g6`'s. `g4`'s map gives the slot
   to its old primary, so `g4` claims it. The other nodes take `g4`'s claim, and the writes
   that `g6` acknowledged stay on `g6` and `g3`.

The pause of `g1` before steps 12 to 15 only moves the moment of the failover: the same composition
without it passed (`4239689f3f3ff03e`), and so did the schedule without the failovers
(`4e7ef3a4bc87e9db`). The documentation of `CLUSTER FAILOVER` (`website/docs/commands/cluster.md`,
step 3 of the external coordination requirement) asks a control plane to verify the new primary's
slot ownership and the reachable nodes' agreement. The example's client checks the role only. Such
a check would have seen `g6` and the other nodes disagree at the stop `run:2710000`. But `g6` had
answered the writes with `OK` by then, and the documentation names no command that takes the slot
back from `g6`.

Nothing was posted. Finding 2 is reported as microsoft/garnet#2440 (2026-11-27, open), with the
target's side only. #2284 (closed) reports two promotions in one shard that lose the loser's
writes; the maintainers answered that a control plane serializes failovers. Here the failovers
are of two shards, one after the other.

### C. New: a replica that attaches after a failover damages its AOF file below the new tail, and after a power cut it reports its primary's offset without the data

**Label: a bug of the project, new as far as the search found.**

Seed 115 of e2 shrank to `disk=cut dseed=0 | run K=g4@22+1 C=40 W=a0075364,p1,q1,r500`
(`regressions/replica-differs-at-the-same-offset-after-a-power-cut.schedule`). The released
hydo 0.7.0 replayed it twice on hvx with `012c520d5bb92ffc`, the recorded value. `g4`, then the
replica of shard 1, was killed before step 22 and started again before step 23. At step 27 the
client failed shard 1 over from `g1` to `g4`, and `g1` became `g4`'s replica. The power cut came
before step 40. In the recovery boot `g1` reported `master_repl_offset:2460`, the offset of
`g4`, and held 6 keys where `g4` held 12. It lacked every write after the failover, and it held
an empty string for the counter `{t10}c10`, which `g4` holds as `1`.

Held runs read both nodes and their AOF files (`/data/garnet/AOF/aof.log.0` of each node):

- Before the power cut (`run:1290000`) `g1` and `g4` both held 12 keys, and both AOFs reported
  `CommittedUntilAddress:2460`. The bytes of the two files matched except for 1536 to 1595. There
  `g4` holds the end of the `INCR {t10}c10` record and a commit record. `g1` holds the 16 bytes
  that start at 1520, copied three times, and then zeros. 1536 is the start of the 512-byte
  sector that holds 1596, and 1596 is the tail at which `g1` began its AOF again at the failover:
  its log says `ReplicaRecover: replay local AOF from 1372 until 1596`.
- In the recovery boot `g1`'s log says `ReplicaRecover: replay local AOF from 1372 until 2460`,
  with no error, and the node holds the 6 keys and the empty counter.

What happens, at `v2.2.14` (unchanged on `main` at `ff769f86`):

1. After the failover the old primary attaches to the new primary as a replica. Its checkpoint
   has the new primary's history and version, so it recovers its own checkpoint, replays its own
   AOF up to 1596, and calls `Log.Initialize(beginAddress, 1596)`
   (`libs/cluster/Server/Replication/ReplicaOps/ReplicaDiskbasedSync.cs`, lines 334, 338 and
   342).
2. `TsavoriteLog.Initialize` calls `RestoreHybridLog(begin, 1596, 1596, 1596)`
   (`libs/storage/Tsavorite/cs/src/core/TsavoriteLog/TsavoriteLog.cs`, line 589). With the head,
   the start and the end at one address inside a page, the restore reads no page
   (`libs/storage/Tsavorite/cs/src/core/Index/Recovery/Recovery.cs`, lines 1643 to 1680). So the
   page in memory never holds the bytes below 1596.
3. The primary's stream is appended at 1596. The log writes whole sectors
   (`libs/storage/Tsavorite/cs/src/core/Allocator/AllocatorBase.cs`, lines 2369 to 2371), so its
   first flush writes the sector from 1536 with whatever the page buffer held below 1596. The
   node's memory is right; its file is not.
4. After the power cut the replica recovers its checkpoint and replays its file from 1372 to
   2460. The damaged record leaves `c10` empty, and the replay ends there without an error.
   `AofProcessor.Recover` returns the address it was asked to reach, not the last record it
   replayed (`libs/server/AOF/Recover/AofRecover.cs`, line 66). So the replica sets its offset to
   2460 and takes the stream from there, and the writes between 1596 and 2460 never reach it.

Without the power cut the schedule passes (`057b2b492432c418`): the damage lies in the file, and
only a recovery reads it. Without the kill of `g4` before the failover it passes too
(`1123bb3544856201`). That run had the same failover and the same power cut. Why the kill is needed
was not separated.

A failover to `g1` would serve that data. The read-back from the primaries found every
acknowledged write, so the example's `acked_outcome` held; the check `eventually_converge`
compares the replica key for key and failed. No issue names it. The closest are these:

- #2299 and pull request #2373 (merged 2026-11-17, in 2.2.14) clear the stale data past a re-used
  tail;
- #2219 (closed) is a scan that ends inside a record;
- #2437 (open) is a replay that stops on an exception.

Nothing was posted.

### Not bugs

- **Damaged cluster files.** A `G=` damage of `cluster/nodes.conf.0` or
  `cluster/replication.conf.0` stops the node at every start, and the run ends `ERROR g<n> restart
  failed (its files were corrupted)`. Garnet detects the damage and refuses to start, so these are
  the environment.
- **A damaged checkpoint of a primary.** Seed 82 of e2 shrank to `disk=none dseed=0 | run
  W=62464f13,p4,q2,r1000,oanytime_read G=g4@31+1:zero,len=65536,seed=907946e8`: `G=` stopped
  `g4`, by then the primary of shard 1, zeroed 512 bytes at the start of its checkpoint's
  `snapshot.dat.0`, and started it again. `g4` recovered without the keys of that checkpoint and
  served on: on hvx it lacked 6 acknowledged writes (`f757c1777c8d1b88`, two runs on the default
  image), and its replica `g1`
  still held them at the same replication offset, 2,716. Garnet's default
  `--fail-on-recovery-error false` lets a node start after an error of its recovery, as the
  option's help says, and #2150 (closed) is about that path. The damage is hydo's, so this is the
  environment with a documented setting. The same schedule with a kill of `g4` instead of the
  damage, `K=g4@31+1`, passed (`99d2c802c942851e`).
- **A failover requested while the replica is detached.** With `reest=0`, a `CLUSTER FAILOVER`
  after the primary's restart aborts (`last_failover_state:failover-aborted`) and the old primary
  stays the primary. A write during it got `MOVED` to the replica for a moment, which the client
  counts as failed.

## The duplicate searches

`gh search issues --repo microsoft/garnet --include-prs`, open and closed, on 2026-11-27 (the rows
of A, B and C later that day):

| phrase | found |
|---|---|
| `master_link_status` | #2087 (closed): a write through `READWRITE` on a replica stops replication, while the replica reports `master_link_status:up`; fixed by #2109, which changed `READWRITE` only |
| `AOF stream client disconnected`, `AofSyncDriver terminated` | #2087 |
| `connected_slaves`, `connected_slaves:0` | #2087, #2086, #2141, #2166 (Sentinel and stock-replica support), none about a restart |
| `ClusterReplicationReestablishmentTimeout`, `replication reestablishment`, `EnsureReplication`, `IsReplicating`, `auto resync` | #1828 (merged): a resync loop on an idle stream when the option is on; #1670, #1674 (failover restore); #1928, #1929 (gossip timeouts) |
| `replica restart primary`, `primary restart`, `primary restart replica`, `primary crash replica`, `replica after restart`, `replica not replicating`, `replica does not reattach`, `replica reattach`, `replica stays`, `replica sync after recover`, `primary recovery replica`, `replica missing writes`, `replica stale`, `replica diverge` | nothing about a primary's restart |
| `kill primary` | #385 (closed): a crash during a snapshot's flush |
| `replica offset`, `partial sync`, `resync` | #2220 (closed, a failover with a stale offset), #2252, #1730, #2141, #1807 |
| `AOF recover` | #2435 (open, a `FLUSHALL` in the AOF), #2437 (open, a transaction across a checkpoint), #2164, #2150, #2174, #2351 |
| the known power-cut loss: `aof-commit-wait power`, `O_DIRECT fsync` | #2281 (open) |
| finding 2: `failover migrate`, `failover migration slot`, `migrate slots failover`, `slot no owner`, `slot unassigned failover`, `slot ownership failover`, `failover slots lost`, `replica slots after migration`, `orphan slot`, `CLUSTERDOWN failover` | nothing |
| `Hash slot not served` | #1999 (a flush that hung), not about slots |
| `TakeOverAsPrimary` | #1670, #1671, #1674 (failover restore), #1694, #1080 (a cluster on Windows after a restart) |
| titles with "migrat" or "slot" (400 newest issues) | #1912 (closed, a destination that fails to apply a migrated key), #1655 (closed, a migration that fails silently on a timeout of `SetSlotRange`), #1560, #494 (`CLUSTER FORGET` of the importing node mid-migration), #354, #350; none with a failover |
| A: `background recovering task`, `recovering task has not completed`, `recoverLock`, `ReadRole` | #1153 (closed, the same message with `[ClusterFailover]` after a forced failover), #1319 (merged, a reset of the recovery state), #1670, #1671, #1674 (failover restore), #1305, #1080 |
| A: `WaitForCommit`, `CommittedUntilAddress`, `replica commit`, `uncommitted AOF replica`, `replica unresponsive`, `replica hangs`, `replica not responding`, `gossip hang`, `replica deadlock`, `thread pool starvation`, `kill primary checkpoint` | #2253 (merged, write admission during a failover), #2093, #2065, #1958, #1730; none about a replica that stops answering |
| titles with "replica" (all) | #226 (closed, 2024, a replica stuck in recovery after `CLUSTER REPLICATE` to a dead primary), #2284 (closed, two promotions in one shard), #2087, #2086, #2208, #2223, #931, #176, #1153, #2441 |
| B: `migration keys left source`, `keys remain source migration`, `migrate slots lost keys`, `migration data loss`, `MIGRATE SLOTS keys not migrated` | nothing; with the queries of finding 2, #2440 only |
| C: `sector`, `partial sector`, `tail sector`, `RestoreHybridLog`, `Initialize AOF replica`, `AOF corruption replica`, `torn`, `PrepareToAppend`, `aof-replay-clear-above-tail` | #2299 (closed) and pull request #2373 (merged): stale data above a re-used tail; #2303, #2239 (checkpoint sector alignment); #2219 (closed, a scan that ends inside a record); #1910 (a torn commit record); none below the tail |
| C: `same version checkpoint replica`, `skipLocalMainStoreCheckpoint`, `replica divergence`, `replica data mismatch`, `replica local checkpoint`, `divergent history` | #1819 (`RI.SET` ordering), #1301 (closed, an out-of-band `CommitAOF`) |

## What held, and why

With `reest=5`, the durable `wait=yes`, and `device=FileStream` for power cuts, every write that
Garnet acknowledged was there at the end on the owner of its slot, unless a later write of the
same register replaced it or its deadline passed. No refused write took effect. That held for
strings, expiring keys, hashes, counters, lists, sets, sorted sets, transactions and scripts,
through failovers by `CLUSTER FAILOVER`, slot migrations by `MIGRATE ... SLOTS` and checkpoints by
`BGSAVE`. It held under kills of a primary, of a replica, of a migration's source and target and
of a newly promoted primary, under pauses, partitions and one-way loss, one node's power loss,
power cuts, and clock steps of an hour each way. Every transaction and script had all of its
writes or none on every node, no counter went past its `INCR`s, and no read returned a value
that no write sent. After the heal every replica held what its primary held, and every node named
the same owner for every slot, but where a failover came right after a migration into its shard
(finding 2) or out of it (finding B), or where a replica attached after a failover had a power cut
(finding C) or lost its primary before a commit (finding A). The aimed power cuts at the client's events (`A=garnet.bgsave+20:cut`,
`A=garnet.migrate_done:cut`, `A=garnet.failover_done:cut`) and the cuts at the 300th and the
600th disk flush (`cf=`, hvx only) lost nothing with `device=FileStream`.

The one deviation from the documented settings is `reest=5`: with Garnet's default, every kill of
a primary ends in finding 1 and covers what comes after it.

## The explorations

Every exploration ran on GitHub's KVM runners with `explore.yml` (`ci/remote-explore.sh`), 20
parts of one worker each, from the branch's first version of the example (its client printed
go-redis's log lines). The runners were Intel Xeon 6975P-C, 6973P-C and Platinum 8370C hosts and
AMD EPYC 7763, 9V45 and 9V74 hosts. The parts' runs took 200 to 350 s each, and a part's shrinks
up to 3.5 hours. The last 3 parts of e2 and 4 of e3 were still shrinking when this round ended and
were cancelled, so no merged report exists; the table counts the parts that finished, whose
reports were read one by one.

| exploration | image and options | seeds | first runs read | failures by cause |
|---|---|---|---|---|
| e1 | default; `--strategy process --new-faults --cluster-faults --storage-faults` | 1-60 | 9 (3 parts, the run was cancelled) | #2281, seed 57, shrunk to `disk=cut dseed=0 \| run C=40 O=g3@27-29:req:loss,82% W=8a737e4e,p4,q1,r250` in 81 runs |
| e2 | `fs`; the same options | 61-120 | 51 (17 parts) | finding 2 in seeds 96, 99 and 120; finding B in seed 89; finding C in seed 115; damaged files: cluster files that stop a node in seeds 63, 66, 75 and 83, and a checkpoint or AOF segment after which a node starts without its data in seeds 82, 100, 104 and 105; a group's `primary:` that named no member in seed 90; a guest loop without VM exits in seed 103's recovery boot (`SPIN`, KVM) |
| e3 | `fs`; `--strategy swarm --new-faults --cluster-faults --reactive fault --react-events` (the client's five events) `--kill-now` | 121-160 | 32 (16 parts) | finding A in seed 144 |

Seed 120 shrank to `disk=none dseed=0 | run W=939b9c23,p2,q1,r500 G=g3@11:flip,n=32,seed=82b902c
L=g3@13+3`: its composition moved slot 4748 into `g2` and failed shard 2 over right after, which
is finding 2; the shrinker kept a `G=` and an `L=` of another node that the failure does not
need. Seeds 96 and 99 ended `ERROR recovery: exit budget exhausted` on KVM, because the recovery
boot's checks waited for the unserved slot (section "What hydo lacked"). Seed 90's `ERROR
shard2.primary: the group's primary: command named no member` came in a schedule that damaged a
file of `g2` (`G=g2@2+2:trunc`), killed shard 2's primary (`K=shard2.primary@4+4`) and paused `g2`
(`P=g2@19+7`): hydo asked the command while no member answered `ROLE` with `master`. It was not
judged further (section "What hydo lacked").

## What hydo lacked

In the order of the time each cost:

1. **A run took four times longer on GitHub's KVM runners than on hvx.** A first run took 200 to
   350 s on a runner, 50 to 60 s on hvx, and a shrink of a power-cut failure of the default image
   took 81 runs. The first exploration (e1) would have run past four hours, and it held the
   runners that the second needed, so it was cancelled after 9 of its 60 seeds. The explorations
   that followed used `device=FileStream`, so that the known power-cut loss was not shrunk again
   and again.
2. **On KVM finding 2 ended as an exhausted exit budget, under the class `durable-defaults`.** In
   seeds 96 and 99 the recovery boot's checks waited for the unserved slot, and the run ended
   `ERROR recovery: exit budget exhausted while the check counters ran`. On hvx the same schedule
   ended with the checks' own violations. The explorer named the class `durable-defaults`, because
   the schedule had a power cut, although the image had the durable device and the failure needs
   no power cut. It then shrank the error by its signature, and every candidate that kept it ran
   to the budget, so those parts ran for hours and gave no minimal schedule. The directed runs on
   hvx separated the faults instead.
3. **No exploration leaves out one fault kind.** The default image loses writes at every power
   cut that follows a write by seconds (#2281). To explore power cuts beyond it, the example has a
   build parameter for the device; an exploration without power cuts would need a copy of the
   manifest without `cut` (as open items 140 and 141 say).
4. **A cluster imported from Compose has no network faults between its nodes.** The nodes of
   Garnet's cluster gossip, replicate and migrate with each other, but Compose names no
   `depends_on` between them. With `network: bridge` the explorer draws links only between
   services that list each other in `depends_on` (`hydo guide strategies`, "Cluster faults and
   group selectors"), so the import drew none. The example names every earlier node in each
   node's `depends_on` by hand, which also makes the nodes start one after another.
5. **A Compose file's fixed address reads as ignored.** `hydo compose` printed `service g1:
   ipv4_address 10.89.0.11 ignored; the service is at 10.89.0.11` for each of the six nodes, as in
   the Typesense example. The addresses were the same.
6. **The aimed grid multiplies by the services.** `--reactive-at` tries each delay with each
   action for each service; six nodes make a grid of 918 cells for the default delays. The
   explorations used `--reactive fault` with the client's events instead.
7. **hvx's console of 64 KiB.** go-redis prints a line for each failed dial, and the commands'
   output notes carried them to the console: a run with a node down for 25 steps wrote 30 such
   notes. The client now turns go-redis's log off, and its history records are short.
8. **The diagnosis named a reaction's kill the out-of-memory killer.** In finding A's run, `hydo
   inspect diagnose --evidence` said `g1 was killed by SIGKILL, which hydo did not send: in a
   guest with 3072 MiB of RAM that is almost always the kernel's out-of-memory killer`. hydo had
   sent it: the reaction `A=garnet.bgsave+10:kill-now-g1` kills from a relay in the guest.
9. **The shrinker kept faults of another node.** Seed 120's minimal schedule kept `G=g3@11:flip`
   and `L=g3@13+3`, which finding 2 in shard 2 does not need, and the class named the damage
   (`corrupted-files`), as in the Typesense example.
10. **A group's `primary:` must name a member.** In seed 90 no member of shard 2 answered `ROLE`
    with `master` while its primary was killed and its other node paused, and the run ended in
    `ERROR shard2.primary: ... named no member`, so its checks never ran.

## Time

| phase | wall time |
|---|---|
| scouting: the release, the image, the documentation of the cluster and the durability settings | 25 minutes |
| setup to a first clean run: the import, the client, two rebuilds for `--logdir` and `depends_on` | 1 hour 10 minutes, about 30 minutes of it waiting for builds on the loaded machine |
| directed runs on hvx, among them the separation of finding 2 and the final images | about 6 hours, most of it waiting for runs of 1 to 10 minutes on the loaded machine |
| explorations on KVM | about 5 hours, beside the directed runs; the last parts of e2 and e3 ran past this round |
| judging, the native runs with Docker, and the write-up | about 2 hours |
| tracing findings A, B and C on hvx with hydo 0.7.0, and the write-up | about 6 hours, most of it waiting for runs and held runs on the loaded machine |
