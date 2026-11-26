# SeaweedFS under hydo: three masters, three volume servers, a filer and the S3 gateway

SeaweedFS (github.com/seaweedfs/seaweedfs) is a distributed file and object store written in Go.
Its masters keep the cluster's map of volumes with Raft and hand out file ids. Its volume servers
store the file contents as needles appended to volume files, each volume on as many servers as
its replication says. Its filer keeps the names and the chunks of every file in a metadata store,
and its S3 gateway speaks S3 over the filer. This example runs that whole shape in one guest of
hvx, on the bare-metal machine, with hydo 0.6.0, and looks for bugs that nobody reported.

## The short answers

- **What was found?** No new bug of SeaweedFS, in 342 schedules of eight explorations and 11
  directed runs. Nothing was posted. Every failure was a server that refused its own damaged
  files: a volume server stops at a damaged superblock on purpose (pull request #8047), a master
  stops at a damaged Raft state, and the second copy of a chunk that hydo damaged stays damaged,
  as the wiki says ("no automatic repair").
- **What held?** With the filer's path rule `fsync` on `/buckets/`, every write that SeaweedFS
  acknowledged, through S3 or through the filer, stayed as acknowledged. No deleted key came
  back, and every listing and upload list agreed with the keys. Unless hydo damaged a file, both
  copies of every chunk held the same bytes. That held through kills, pauses past the masters'
  election timeout and the volume servers' heartbeats, partitions and one-way loss, clock steps
  back by up to 30 days and forward by up to a year, one server's power loss, and power cuts of
  the guest. It also held through 60 power cuts and power losses aimed 0 to 40 ms after
  acknowledgements and compactions, 65 kills inside writes of 4 MiB or more, and volumes of 8
  MB that filled during the runs.
- **What is documented?** Without the rule, SeaweedFS's default, 5 of 20 power cuts lost
  acknowledged writes; `hydo explore --configs` labeled it the documented trade-off, and the
  regression in `regressions/` carries `# params: fsync=false`.
- **What hydo lacked?** Section "What hydo lacked". The worst was that a Go program in FIPS mode
  cannot start in hydo's guest, whose clock has no jitter, and SeaweedFS's image turns the mode
  on. The example runs `/usr/bin/weed` without the image's entrypoint.

## The version

SeaweedFS 4.55 (2026-11-23, tag commit `a98c9e27`), the newest release when the onboarding
started (2026-11-26). The sources were read at the tag.

| image | index digest | amd64 | arm64 |
|---|---|---|---|
| `chrislusf/seaweedfs:4.55` | `sha256:7ca46aea…` | `sha256:340bc0cb…` | `sha256:1d857dc0…` |

`hydo/fetch-images.sh` fetches the image by digest. The image holds the Go program
`/usr/bin/weed`, which is every server, and the Rust volume server `weed-volume`, which this
example does not run.

## The promises

From the wiki pages "Replication", "Path Specific Configuration", "Filer Server API",
"Volume Server API" and "Amazon S3 API", and from the code:

1. A write completes only when every replica has it: "When data is written, the write operation
   does not complete until replication is confirmed on all volumes", and "In SeaweedFS, `W = N`
   and `R = 1`" (wiki "Replication").
2. Data stays readable while one replica is: "Data remains readable as long as at least one
   replica is accessible" (the same page). With the replication `001` every volume has two
   copies, on two servers of one rack, so an acknowledged write survives the loss of one volume
   server.
3. With the path rule `fsync`, an acknowledged write is durable: "if true, the write will incur
   an fsync operation" (wiki "Volume Server API" and "Filer Server API"). Pull request #11943
   (2026-10-22) made the rule sync the filer's metadata of a write too, and #12736 and #12759
   (2026-11-18 and 19) the deletions; 4.55 has all three.
4. A deleted object stays deleted, a listing shows what exists, and a completed multipart upload
   is whole, as S3 says.

The promises 2 to 4 for the S3 gateway, and every promise across a Raft failover of the masters,
are read from the code and the S3 semantics that SeaweedFS implements. They are unconfirmed by
the owner.

## The project

`docker-compose.yaml` runs the shape of the wiki pages "Failover Master Server" and
"Replication" on one host: three masters with Raft (`-peers`), three volume servers, one filer on
its default store, leveldb2 (the image's `/etc/seaweedfs/filer.toml`), and the S3 gateway with one
identity (`config/s3.json`). The masters give every new volume the replication `001` and a size
limit of 64 MB, and each volume server holds up to 16 volumes. The Helm chart's default is one
master, one volume server and one filer; the task of this onboarding was the cluster with
replication.

`hydo compose` imported the file. The changes from the import (the header of
`hydo/hydo.yaml`):

- Each server's `/data` is a file system of its own (`storage:`), the target of `L=` and `G=`:
  64 MiB for a master, 512 MiB for a volume server, 192 MiB for the filer.
- The ports and the readiness checks are each server's own (`tcp:9333`, `tcp:8080`,
  `tcp:8888`, `tcp:8333`). The import took the image's first `EXPOSE`, 7333 (WebDAV), for every
  server.
- The masters start together, since a Raft quorum needs two, and so do the volume servers.
- Every server runs `/usr/bin/weed -logtostderr=true` itself, instead of the image's
  `/entrypoint.sh`. The entrypoint sets `GODEBUG=fips140=on`, and in the guest Go's FIPS mode
  cannot gather its first entropy: every server stopped at its start with `panic: fips140/drbg:
  failed to obtain initial entropy` (section "What hydo lacked"). The entrypoint also drops to
  the user `seaweed`; here the servers run as root.
- The group `masters` names the Raft leader as its primary (`K=masters.primary`), and the group
  `volumes` holds the volume servers, for the cluster faults.
- The guest has 2,048 MiB. The eight servers peak at 107 to 186 MiB each, 1,076 MiB together,
  without faults (`hydo run --memory`).
- The first command, `first_init`, waits for a Raft leader and the three volume servers, writes
  the filer's path rule `{"locationPrefix":"/buckets/","fsync":true}` into
  `/etc/seaweedfs/filer.conf` (what `fs.configure -locationPrefix=/buckets/ -fsync` writes), and
  makes the bucket `hydo`. The build parameter `fsync` chooses `true` (the default of this
  example) or `false` (SeaweedFS's default).

## The workload

The client `swc` (`client/main.go`, the AWS SDK for Go v2 1.41.5 with its S3 client 1.97.3, and
Go's standard library for the filer, the masters and the volume servers) writes every key into
the bucket `hydo`. A key is written through S3 or through the filer's HTTP API, whose folder
`/buckets/hydo` is the bucket. The client sends no retries, and each request has a time limit of
30 s.

| command | operation |
|---|---|
| `first_init` | waits for the cluster, sets the path rule, makes the bucket, and writes once through each API |
| `parallel_driver_put` | S3 PutObject of a new key |
| `parallel_driver_over` | S3 PutObject over a known key |
| `parallel_driver_del` | S3 DeleteObject of a known key |
| `parallel_driver_mdel` | S3 DeleteObjects of two known keys in one request |
| `parallel_driver_mpu` | an S3 multipart upload of 2 or 3 parts, 5 MiB each but the last, to a new or a known key, and its completion |
| `parallel_driver_abort` | an S3 multipart upload of one part to a new key, and its abort |
| `parallel_driver_copy` | S3 CopyObject of a known key to a new or a known key |
| `parallel_driver_fput` | a filer PUT of a new file `/buckets/hydo/<key>` |
| `parallel_driver_fover` | a filer PUT over a known key |
| `parallel_driver_fdel` | a filer DELETE of a known key |
| `anytime_read` | reads of up to 5 known keys through S3 or the filer: each body must be whole |

The objects are 64 bytes to 2 KiB, 8 to 128 KiB, 1.1 to 1.4 MiB, or 4.3 to 4.6 MiB (two chunks
of the filer's 4 MiB), and a multipart upload writes 5 to 10.2 MiB. Each body starts with a line
that names its tag, its length and the sha256 of the rest, so a read knows the write it returns
and whether it is whole.

One command at a time works on a key. An answer 2xx is acknowledged, a 4xx answer but 408 and
429 failed, and any other answer, or none, unknown. A key whose operation has an unknown outcome
takes no other operation. After each acknowledged write the command reads the key again, two
times in three through the other API, and exits 100 when a certain answer is not the write
(read-after-write). Each acknowledgement writes a console event, `sw.<kind>_acked`, the event of
the reactions. 40 rounds of 1 s give about 95 operations a run.

## The rules

| rule | promise | where |
|---|---|---|
| `objects` (a `type: messages` check with `acked_outcome` and `effect: recorded`) | every acknowledged write is the key's value at the end unless a later write of the key replaced it; no refused write took effect | `hydo.yaml`, the read-back `swc readback` |
| `compose.commands` | read-after-write through either API; no body read is torn | the drivers, `anytime_read` |
| `list` | ListObjectsV2 lists every key that GET finds, and no key that GET does not | `checks/final_list` |
| `uploads` | no upload whose abort SeaweedFS acknowledged is still listed | `checks/final_uploads` |
| `replicas` | every chunk of every object that S3 reads has two copies registered with the masters, and both answer it with the same bytes | `checks/final_replicas` |
| `available` (eventually, 120 s) | S3, the filer and every volume server answer after the heal and in the recovery boot | `checks/eventually_available` |

Each operation is a send of the history `objects`, with its id `<kind>.<key>.<seq>.<value>`.
The read-back reads every key through S3 until each has a certain answer, then writes the final
state of each operation: `s=ok` for acknowledged, `s=fail` for refused, `s=info` for unknown. It
writes `f=1` or `f=0` for the last acknowledged write of each key, and for every write of
unknown outcome, by whether the key holds the write's value now (hydo guide messages, "Read
back the final states"). The rule `replicas` reads each chunk's file id from the filer
(`?metadata=true`), the volume's locations from the masters (`/dir/lookup`), and the chunk from
each volume server directly.

### Two parts of the folder that are not SeaweedFS's own

- **Compactions during the workload.** The masters compact a volume that holds deleted needles
  every 14 to 16 minutes (`-vacuumIntervalSeconds 840` and up to 2 minutes more,
  `garbageThreshold 0.3`), longer than a run. The command `anytime_vacuum` orders one compaction
  through the masters' `/vol/vacuum`, so compactions run beside the writes and meet the faults.
  It writes the console events `sw.vacuum_start` and `sw.vacuum_done`, the events of the
  reactions.
- **A compaction before the read-back.** The filer reads chunks with `readDeleted=true`
  (`RetriedFetchChunkData`), so a chunk that SeaweedFS deleted while an entry still names it
  stays readable until its volume is compacted. With the build parameter `vacuum=yes` (the
  default), the read-back compacts every volume first, so such a chunk would read as lost. The
  first explorations (e1 and e2) ran on the image before this and before `anytime_vacuum`.
- **Volumes that fill.** The build parameters `vlimit` and `vmax` set the masters'
  `-volumeSizeLimitMB` and each volume server's `-max`. With `vlimit=8` and `vmax=48` (e7) a run
  writes about 120 MB into volumes of 8 MB, so the masters must mark volumes full and grow new
  ones during the run; the run's volumes were not counted.

## Without faults, and the regressions

`disk=none | run` passes with one fingerprint on the final image
(`sha256(explore-ob-seaweedfs-disk.img)=f6e18b43…`): `353f5b2b1913660a` twice. A run takes about
28 s in the engine and a minute of wall time. About 95 commands run; 1 to 3 of them end with
status 1, each an `anytime_vacuum` whose request every master answered with an error within 10
to 30 ms. The client does not record the answer.

No finding of SeaweedFS has a regression. `regressions/` keeps the documented trade-off of
section "Settings that trade durability":

| regression | build | verdict |
|---|---|---|
| `acknowledged-write-lost-at-a-power-cut-without-fsync.schedule` (`disk=cut \| run C=1`) | `--set fsync=false` (its `# params:` line) | `VIOLATION [objects.acked-outcome]`: an acknowledged filer PUT is not there after the power cut |

**On the KVM engine (Milestone 373).** GitHub's runners built the default image with the
example's steps and the 0.6.0 release's guest files, and got hvx's bytes (`f6e18b43…`). The image
with `--set fsync=false` is `c56d8263…`. Each schedule ran twice from fresh boots with one
fingerprint per host, and each verdict is hvx's. `hydo repro --regressions` left the regression out
on the default image by its `# params:` line, and passed it on the `fsync=false` image. The
fingerprints are the KVM engine's own, one for the AMD hosts and one for the Intel hosts:

| schedule | image | verdict | AMD EPYC 9V74 and three 7763 | two Xeon 6975P-C |
|---|---|---|---|---|
| `disk=none \| run` | default | OK, 92 acknowledged writes judged | `bbf6243b4dc5d2b1` | `611bba1f54237e68` |
| the regression | `fsync=false` | `VIOLATION [objects.acked-outcome]`: `fover.k0.0.v0x0` is not there | `909e6e48aa11058c` | `08fdc426d9c908e2` |

## What was found

No new bug of SeaweedFS. Every failure of the explorations was a server that refused its own
damaged files, and each refusal is on purpose or a limit that the documentation names.

### By design and documented

| failure | label | where |
|---|---|---|
| a volume server stops at every start after hydo zeroed bytes of one of its volume files: `F volume_loading.go:241 Unsupported volume 1 version 0` | by design: pull request #8047 (2026-01-17) made a volume server "stop immediately rather than attempting to operate on incompatible data" when a superblock names an unknown version, and its test "simulates loading a corrupted volume". The other two volume servers kept serving every object | e1 seed 3, e3 seed 111 |
| a master stops at every start after hydo flipped, zeroed or cut bytes of its Raft state: `F master.go:288 please verify /data is writable, see .../issues/717: raft: Initialization error: invalid…` | the environment: the Raft log is damaged, and the two other masters keep the quorum. The message names the folder's permissions, not the damaged log (issue #7449 met the same message for a folder that was not writable) | e3 seeds 106, 109, 129, 131, 155 |
| the rule `replicas` finds a chunk whose second copy answers 404 after hydo damaged a volume server's files | documented: "If any replica is missing, there is no automatic repair" and "fixing replication is done through the weed shell (see `volume.fix.replication`)" (wiki "Replication"). Reads take the other copy: a 404 of one location is retried on the next (`ReadUrlAsStream`, `weed/util/http/http_global_client_util.go`), and every read-back of those runs held | e2 seed 35, e3 seed 147 |

### Settings that trade durability

`hydo explore --configs --seeds 1-20 --durable-only` (e5) explored both values of the filer's
path rule `fsync` with the same seeds:

| combination | schedules | failed | failures found |
|---|---|---|---|
| `fsync=true` | 20 | 0 | none |
| `fsync=false` (SeaweedFS's default) | 20 | 5 | acknowledged writes lost at a power cut, minimal `disk=cut \| run C=1` |

Every one of the five was a power cut of the guest: the read-back found 1 to 18 acknowledged
writes without their effect, filer PUTs, copies and multipart uploads among them. The table
labeled it the documented trade-off: the wiki says that a write without the rule incurs no
fsync, and the rule is off by default. The page "Filer Server API" still says that
with `fsync=true` "the file metadata will still be separate", which pull request #11943 changed.

## The duplicate searches

The searches ran on 2026-11-26 over SeaweedFS's issues and pull requests, open and closed, with
`gh search issues --repo seaweedfs/seaweedfs --include-prs`.

| subject | phrases | what they found |
|---|---|---|
| durability of acknowledged writes | `power loss`, `data loss`, `fsync`, `acknowledged`, `lost after crash`, `Jepsen` | a series of power-loss reports by one user since October, all fixed in 4.55: #11943 (writes under the fsync rule), #12735 and #12736 (needle deletes), #12758 and #12759 (chunk deletes of the filer), #12780 (a leveldb table damaged by a power loss). #6735 (open, 2025): torn writes to `.dat` files need a manual recovery. #1056 (open, 2019): a Jepsen suite |
| a volume server that refuses a damaged file | `Unsupported volume`, `superblock corrupted`, `volume server fails to start corrupted` | #8047 (the fail-fast on unknown versions, on purpose). #3463 (open, 2022): a volume server that crashed during a vacuum does not start; no run of this example reproduced it, also with power cuts and kills aimed at the end of a vacuum (e4) |
| a master that refuses a damaged Raft state | `Initialization error`, `please verify is writable`, `raft log corrupted`, `master fails to start after power`, `717` | #7449 (closed): the same message for a folder that was not writable after an upgrade. No report of a damaged Raft log |
| listings, replicas and failovers | `replica inconsistent`, `deleted object still listed`, `master failover`, `multipart complete`, `S3 PutObject lost` | #4557 (open, 2023, 3.52): a write of 200 MB that a stop of the master leader interrupted was acknowledged, and the file was 0 B (section "Kills of the master leader inside a large write"). #12701 (closed): a gateway's cached volume locations after a new master. No report that the example's checks would have met |
| the image's FIPS mode | `fips140`, `failed to obtain initial entropy`, `FIPS` | #12998 (closed 2026-11-25): the entrypoint appends `fips140=on` after the caller's `GODEBUG`, so the caller cannot turn it off. #12839 (closed): `fips140=only` panics at the start. No report of the entropy panic, which needs a machine whose clock has no jitter |

## What hydo lacked

In the order of the time each cost.

1. **A Go program in FIPS mode cannot start in hydo's guest.** The image's `/entrypoint.sh`
   sets `GODEBUG=fips140=on` (since SeaweedFS's Go 1.24 builds), and every server stopped at its
   start with `panic: fips140/drbg: failed to obtain initial entropy`. Go's FIPS entropy source
   (`crypto/internal/entropy/v1.0.0`, `Samples`) times 1,024 rounds of memory accesses with the
   high-precision clock and keeps the low byte of each difference. In hvx's guest with the exact
   tick a clock read advances the virtual TSC by the same 2 µs every time, so the low byte
   repeats, and the repetition count test (41 repeats) fails on each of the 100 tries. A user
   cannot turn the mode off with `GODEBUG`, because the entrypoint appends its own setting last
   (SeaweedFS issue #12998). The example runs `/usr/bin/weed` itself (`entrypoint:`), so its
   servers run without FIPS mode and as root. `hydo guide hazards` names no such limit. About 20
   minutes.
2. **The services' own file systems come on top of `data.size_mib`, and `--check` did not
   say so.** The first manifest gave the data disk 1,792 MiB for the servers' file systems, as
   the Garage example's comment says. `hydo build --check` printed `OK`, and the build stopped
   after the images were unpacked with `the disk with the services' own file systems is 4488 MiB,
   more than the 4096 MiB an engine maps`. The data disk needs only the run's own files: 256
   MiB. About 10 minutes.
3. **The import took the image's first `EXPOSE` for every service.** All eight servers got the
   port 7333 (WebDAV, which no server runs) and a TCP readiness check on it. The ports and
   checks were written by hand (as in the Saleor and Hatchet examples).
4. **Stopping an exploration's process group left its engine running.** `kill -- -<pgid>` of
   `hydo explore` ended the explorer, but the engine `hydo-hvx`, which hydo starts under `sudo`,
   kept running with 14 GB of the host until it was stopped by its pid. hydo prints no pid of
   its engines, so a user without the process table cannot find them.
5. **A violation in a run with damaged files is shrunk.** e2's first failure was the rule
   `replicas` after `G=` (class `corrupted-files`). Since Milestone 339 the detected damage's
   errors are shrunk only with `--shrink-detected-damage`, but a violation of the same class is
   shrunk always, at a minute or more a candidate. e2 was stopped after 17 schedules, and the
   later explorations ran with `--no-shrink`.
6. **`hydo rules scan --kind writes` found no S3 route and no filer write.** It listed 231 write
   routes: the gRPC services and the administrator's UI. SeaweedFS's S3 routes are a gorilla/mux
   router whose paths are a variable (`bucket.Methods(http.MethodPut).Path(objectPath)`,
   `weed/s3api/s3api_server.go` line 1123), and the filer's HTTP API is one handler on `/` that
   switches on the method (`weed/server/filer_server_handlers.go`). The workload's operations
   came from the code and the wiki by hand.
7. **The verdict line names a claim that judged nothing.** Every passing run ends
   `checks=2 held (objects.no-phantom: every delivered message was sent before it was delivered
   (0 judged, 0 undecided))`. The claim that judged the run, `objects.acked-outcome` (81 to 95
   writes a run), is only in `result.json`.

Milestone 367 changed hydo for items 1 to 4, 6 and 7. `hydo guide hazards` names the FIPS limit.
`hydo build --check` counts the services' own file systems. The import warns when services of
one image share its first `EXPOSE`. A stop of `hydo explore` stops its engines and
`engines.txt` names them. The write scan reads the S3 router's chains and the filer's switch on
the method. The verdict names a claim that judged something. Item 5 is left (open item 186).

## What held, and why

- **A write waits for every copy.** The volume server that takes a write asks a master for the
  volume's locations, refuses the write when fewer are writable than the replication's copy
  count, writes its own copy and the others at once, and answers only after all of them
  (`ReplicatedWrite` and `getRemoteReplications`, `weed/topology/store_replicate.go` lines 35
  to 140 and 378 to 420). No run acknowledged a write that one copy lacked.
- **A read takes any copy that has it.** The filer reads a chunk from the volume's locations in
  turn, and a 404 or no answer moves it to the next one (`RetriedFetchChunkData`,
  `weed/util/http/http_global_client_util.go` line 693). Every read-back after a kill, a pause or
  a power loss of one volume server found every acknowledged write.
- **The fsync rule syncs the whole write path.** With `fsync` on `/buckets/`, the filer passes
  `?fsync=true` to the volume servers and syncs its leveldb2 writes (`syncWriteContext`,
  `weed/filer/filer.go` line 519; `writeOptions`, `weed/filer/leveldb2/leveldb2_store.go` line
  140), for writes and, since 4.55's pull requests #12736 and #12759, for deletions. No power
  cut of the guest and no power loss of one server lost an acknowledged write or brought back a
  deleted key.
- **A restarted filer loads its rules before it serves.** `LoadFilerConf` runs in
  `NewFilerServer` before the server listens (`weed/server/filer_server.go` line 431), so the
  writes right after a recovery boot are synced too.
- **The masters' file ids follow the volume servers.** Every heartbeat raises the sequencer to
  the volume server's largest file key (`SetMax`, `weed/server/master_grpc_server.go` line 299),
  so a new Raft leader hands out no id that a volume holds. Kills and pauses of the leader did
  not lose a write.

## The explorations

All on hvx on the bare-metal machine, with the released hydo 0.6.0, two workers of 2,048 MiB (one
when two explorations ran together). The engine process held up to 14 GB of the host a worker.
A first run took 20 to 30 s on average.

| exploration | image | options | schedules | failures by cause |
|---|---|---|---|---|
| e1 | the first | `--seeds 1-20 --strategy process --cluster-faults --storage-faults --more-faults` | 20 | 1: a volume server's damaged superblock |
| e2 | the first | the options of e1, `--seeds 21-100` | 17, then stopped in its first shrink (section "What hydo lacked") | 1: `replicas` after a damaged volume server |
| e3 | with the compactions | the options of e1, `--seeds 101-160`, `--reactive fault --kill-now --react-events sw.vacuum_start,sw.vacuum_done,sw.put_acked,sw.del_acked,sw.fput_acked,sw.mpu_acked --no-shrink` | 60 | 7: a master's damaged Raft state in 5, a volume server's damaged superblock in 1, `replicas` after a damaged volume server in 1 |
| e4 | with the compactions | `--reactive-at sw.put_acked@10,sw.fput_acked@10,sw.del_acked@15,sw.vacuum_done@10 --react-delays 0..40/10 --react-actions cut,powerloss-filer,powerloss-volume1 --no-shrink` | 60, every reaction fired | none |
| e5 | both values of `fsync` | `--configs --seeds 1-20 --durable-only` | 20 and 20 | `fsync=true`: none. `fsync=false`: 5 power cuts lost acknowledged writes, the documented trade-off |
| e6 | the final | `--reactive-at sw.big_start@3 --react-delays 0..3000/250 --react-actions kill-now-master1,kill-now-master2,kill-now-master3,kill-now-volume1,kill-now-filer --workers 1 --no-shrink` | 65, every reaction fired | none |
| e7 | the final with `vlimit=8 vmax=48` | the options of e3 with `--react-events sw.vacuum_start,sw.vacuum_done,sw.big_start,sw.put_acked`, `--seeds 201-240 --workers 1` | 40 | 5: a master's damaged Raft state in 4, a volume server's damaged superblock in 1 |
| e8 | the final | `--seeds 301-340 --strategy swarm --new-faults --cluster-faults --storage-faults --reactive fault --kill-now --react-events sw.vacuum_done,sw.big_start,sw.del_acked,sw.fput_acked --workers 1 --no-shrink` | 40 | 3: a master's damaged Raft state, a volume server's damaged superblock, `replicas` after a damaged volume server |

The blind explorations (e1, e2, e3, e7 and e8) drew kills, pauses, network faults and one-way
loss of single servers and of the groups, clock steps of 30 s to 30 days back and up to a year
forward, one server's power loss, damaged files of every server, and power cuts. The process
strategy placed its `C=` cuts at the workload's end (step 40); the cuts inside the workload
came from `cf=` draws (the k-th disk flush), from the reactions, and from e4. `hydo stats e3`
judged the curve still growing (a new verdict and property outcome at run 47 of 60): the
damaged files of each server kept adding verdicts, and no run with working disks failed.
During e8 a replay job built the default image again into the same folder, with the same bytes
(`f6e18b43…`); e8's three failures reproduced on their re-runs.

The directed runs, on the final image:

| what | schedules | result |
|---|---|---|
| no fault, twice | `disk=none \| run` | OK, `353f5b2b1913660a` both times |
| the Raft leader paused past the election timeout of 10 s | `P=masters.primary@10+15` | OK |
| a volume server paused 30 s, two of them 20 s | `P=volume1@8+30`, `P=volumes.majority@10+20` | OK; with two paused, 35 of 95 commands failed or got no answer, and every acknowledged write stayed |
| the filer's own power loss | `disk=cut \| run L=filer@15+2` | OK |
| power cuts inside the workload | `disk=cut \| run C=20`, `disk=cut \| run L=volume2@12+3 C=25` | OK; after the cut at step 20 the read-back judged 46 acknowledged writes |
| clock steps back by an hour, with a kill of the leader | `T=10:-3600`, `K=masters.primary@10+2 T=20:-3600` | OK |
| one master cut off from the others | `N=masters.isolate.1@8-20:loss,100%` | OK |

## Time

The onboarding started at 06:25 UTC on 2026-11-26. The bare-metal machine was otherwise idle,
so the explorations ran two workers.

| phase | wall time |
|---|---|
| 1. Scouting: the release, the wiki, the write and read paths, the clock audit, the issues | 30 minutes |
| 2. Setup to the first clean run: the import, the client, the FIPS entropy panic, the disk's size | 35 minutes |
| 3. Explorations and directed runs | 3 hours 30 minutes, with e5's shrinks 40 minutes of it |
| 4. Judging: the damaged files, the replicas rule, the configurations | inside phase 3 |
| 5. The duplicate searches and the write-up | 1 hour |
