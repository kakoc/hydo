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

- **What was found?** One new candidate, one finding that needs Garnet's default of a setting,
  and three candidates whose cause was not traced (section "Candidates whose cause was not
  traced"). Nothing was posted.
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
- **What held?** With the option at 5 s and the device of #2281's workaround, every write that
  Garnet acknowledged stayed on the owner of its slot, through failovers, slot migrations and
  checkpoints, under kills, pauses, partitions, one node's power loss, power cuts and clock steps,
  but in finding 2 and candidate B. Transactions and scripts applied whole, counters stayed within
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
| `disk=none \| run K=g1@15+3` | `r0` | `converge` (two runs) | `982a16effdb11bfa` |
| the same | `r0`, client 2 | the same (two runs) | `59e40847341696b9` |
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
| seed 96 of e2: `disk=cut dseed=0 \| run K=g6@15+3 N=g1-g5@27-29:loss,65% C=40 P=g4@0+6 T=37:3600 W=9abb9e17,p3,q2,r250 L=g3@29+3` | `fs` | `converge` and `slots`: `CLUSTERDOWN Hash slot not served` (finding 2, two runs) | `5a4771d51585eeff` |
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
microsoft/garnet#2087 reported for another trigger; that issue was closed with a fix of its
trigger only.

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
   calls (`libs/cluster/Session/RespClusterBasicCommands.cs`, line 465). It returns at once when
   `cluster-replication-reestablishment-timeout` is 0 (line 189, "Disabled"), the default of
   `libs/host/defaults.conf` (line 598).
3. The replica's `master_link_status` is the state of its gossip connection to the primary, not
   of the AOF stream (`Gossip.GetPrimaryLinkStatus`, `libs/cluster/Server/Gossip/Gossip.cs`,
   line 232). The gossip connection comes back after the restart, so the replica says `up`.

With `--cluster-replication-reestablishment-timeout 5` (the example's default `reest=5`) the
same schedule passes on hvx, and the replica holds every key. A kill of the replica `g4` instead
of the primary passes with either value: the replica attaches when it starts.

`native/replica-after-primary-restart.sh` reproduces it with Docker, without hydo, with a primary
and one replica: after a `SIGKILL` of the primary and its start with `--recover`, the replica
holds 0 of 10 keys written after the restart, 60 s later, with `master_link_status:up`, while the
primary reports `connected_slaves:0`. With `REEST=5` it holds all 10, and the primary lists it
online at the same offset. The cause is plain in the code after this run, so the script is the
only native run.

The documentation of the option is one line of `defaults.conf`, "Maximum frequency cluster
Replicas will attempt to re-establish replicating their Primaries, in seconds", and a row of
`website/docs/getting-started/configuration.md` without a description. Neither says that 0 turns
it off, and `website/docs/cluster/replication.md` says that "replicas can recover from their own
checkpoint and replay their AOF before connecting to their primary and resuming
synchronization". Garnet's cluster is passive by design (`cluster/overview.md`, "Control
Plane"), so a control plane could attach the replica again with `CLUSTER REPLICATE`. It would
need to know that it must: the replica's own status says that nothing is wrong. Nothing was
posted.

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

What happens, step by step:

1. `MIGRATE <g1> 6379 "" 0 5000 SLOTS 8943` on `g2` moves the slot's keys to `g1` and gives the
   slot to `g1` on `g1` and `g2`. The client waits until both name `g1` in `CLUSTER SLOTS`, the
   check that `website/docs/cluster/key-migration.md` names. The other nodes, among them `g1`'s
   replica `g4`, learn the new owner by gossip some seconds later.
2. The client sends `CLUSTER FAILOVER`, without an option, to `g4` at once. `g4` waits until it
   has replicated `g1`'s AOF, which holds the migrated keys, and takes over.
3. `g4` takes over the slots that its own copy of the cluster's configuration gives to `g1`
   (`ClusterConfig.TakeOverFromPrimary`, `libs/cluster/Server/ClusterConfig.cs` line 1393, which
   reads `GetLocalPrimarySlots()`; `ClusterManager.TryTakeOverForPrimary`,
   `libs/cluster/Server/ClusterManager.cs` line 389). That copy does not have slot 8943 yet.
4. `g1` becomes a replica of `g4` and serves no slot. No node claims slot 8943 any more, and
   gossip never gives it a new owner: `g2` still names `g1`, and every other node names none.
   This holds across a restart of every node and a power cut.

The keys of the slot are on `g4`, which replicated them, but no node serves them: a read through
`g4` answers `CLUSTERDOWN Hash slot not served`, and one through `g2` answers `MOVED` to `g1`,
which is a replica. Every acknowledged write of the slot is unavailable for good, and so is every
new write of its keys. The cause is the same at `v2.2.14` and on `main`.

`native/slot-lost-after-failover.sh` reproduces it with Docker, without hydo, with three
primaries and one replica: after the migration and the failover, `a` (the old primary), `c`
and `ra` (the new primary) name no owner of the slot, `b` names `a`, and a `GET` through `ra`
answers `CLUSTERDOWN Hash slot not served`, also 30 s after a restart of all four nodes. With
`MIGSLEEP=6`, a wait of 6 s between the migration and the failover, gossip reaches `ra` first,
`ra` takes the slot over, and every node names `ra`.

The documentation of `CLUSTER FAILOVER` asks a control plane to serialize failovers and to check
that the reachable nodes agree on the new owner after a failover. It says nothing about a
migration before it, and the control plane cannot repair the slot with a failover: every node but
`g2` names no owner. `CLUSTER SETSLOT 8943 NODE <g4>` on the nodes would. No issue or pull request
names it (section "The duplicate searches"). Nothing was posted.

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

### Candidates whose cause was not traced

The last parts of the explorations gave three more failures on the `fs` image, each with one
fingerprint over two runs on KVM and each failing again on hvx. Their causes were not traced
before the end of this round, so they carry no label yet. Each has a saved regression.

**A. A kill of a primary 10 ms after a `BGSAVE` leaves its replica without an answer.** Seed 144
of e3 shrank to `disk=none dseed=0 | run W=69274d70,p3,q1,r500,oparallel_driver_del
A=garnet.bgsave+10:kill-now-g1` (KVM `88ca1c0b695a75fb` twice). `kill-now-g1` stops `g1` 10 ms
after a `BGSAVE` of the client and keeps it down until the heal. 120 s after the heal the replica
`g4` still answered no command (`shard 1: the replica g4 does not answer: i/o timeout`), the nodes
named different owners of a slot, and on hvx the read-back lacked one acknowledged `RPUSH`
(`e12c7c93adf9d46c` twice). `hydo inspect diagnose --evidence` read the replica's log: it ends with `fail:
ReplicationManager[0] Error background recovering task has not completed [ReadRole]` after the
primary came back, and the primary's gossip to `g4` timed out from then on. The same message, with
`[ClusterFailover]`, is in #1153 (closed in 2025), after a forced failover. Garnet's open #2437 (a
transaction that straddles a checkpoint stops the AOF's recovery) may explain the lost write; it
does not explain a replica that stops answering.
Regression: `regressions/replica-hangs-after-a-kill-during-a-checkpoint.schedule`.

**B. A pause of a migration's target and failovers of both shards after the migration lose
acknowledged writes.** Seed 89 of e2 shrank to `disk=none dseed=0 | run P=g1@12+3
W=0a39fa3d,p1,q2,r500` (KVM `00af90d009683f21` twice). The client moved slot 13404 from `g3` to `g1`,
failed shard 3 over to `g6`, and then shard 1 over to `g4`. After the heal `g6` and `g3` still
held two keys of the slot, which shard 3 no longer owned, and the owner lacked both: the
read-back found 2 of 45 acknowledged writes missing (`674c2b16f5e9438b` twice). The same
composition without the pause passed (`4239689f3f3ff03e`). It is finding 2's race on both
sides of a migration, as far as the logs show: each new primary took over the slots that its own
copy of the configuration named. Regression:
`regressions/acked-writes-lost-after-failovers-on-both-sides-of-a-migration.schedule`.

**C. After a kill of a new primary and a power cut, its replica holds other data at the same
offset.** Seed 115 of e2 shrank to `disk=cut dseed=0 | run K=g4@22+1 C=40 W=a0075364,p1,q1,r500`
(KVM `8bb39f73a77bf0fb` twice). In the recovery boot the replica `g1` reported the primary's
offset, 2,460, and held other data: it lacked keys, had an older list, and held an empty string
for a counter that its primary holds as `1`: 8 keys differed (`012c520d5bb92ffc` twice). The same
schedule without the kill passed (`23cd71d660070fd1`). The read-back from the primaries found
every acknowledged write. A failover to `g1` would serve that data. Regression:
`regressions/replica-differs-at-the-same-offset-after-a-power-cut.schedule`.

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

`gh search issues --repo microsoft/garnet --include-prs`, open and closed, on 2026-11-27:

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
(finding 2). The aimed power cuts at the client's events (`A=garnet.bgsave+20:cut`,
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
| e2 | `fs`; the same options | 61-120 | 51 (17 parts) | finding 2 in seeds 96, 99 and 120; candidate B in seed 89; candidate C in seed 115; damaged files: cluster files that stop a node in seeds 63, 66, 75 and 83, and a checkpoint or AOF segment after which a node starts without its data in seeds 82, 100, 104 and 105; a group's `primary:` that named no member in seed 90; a guest loop without VM exits in seed 103's recovery boot (`SPIN`, KVM) |
| e3 | `fs`; `--strategy swarm --new-faults --cluster-faults --reactive fault --react-events` (the client's five events) `--kill-now` | 121-160 | 32 (16 parts) | candidate A in seed 144 |

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
8. **The diagnosis named a reaction's kill the out-of-memory killer.** In candidate A's run, `hydo
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
