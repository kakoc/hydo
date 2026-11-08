# TigerBeetle: results

The second third-party project onboarded end to end the way an outside user would (after
`nats-jetstream`): TigerBeetle, the financial transactions database (Viewstamped Replication
between replicas, io_uring, direct I/O with `O_DSYNC` on a preallocated data file, all memory
allocated at start). Starting point: the Docker Compose file of TigerBeetle's own documentation
(`docs/operating/deploying/docker.md`), then only the onboarding skill (`hydo guide`) and the
`hydo` command built from this tree (a Linux release tarball built from it with
`hydo/dist/linux/build-release.sh` and installed with `install.sh --no-module`), on the
bare-metal machine with the hvx engine, agent images only. TigerBeetle releases: 0.17.9 (the
newest, 2026-07-06), 0.16.45 and 0.16.29, the static x86_64 binaries of the GitHub releases.

## The questions, and the short answers

- **Can hydo run TigerBeetle at all?** Yes, one replica, unmodified, at the first attempt:
  io_uring, `O_DIRECT` with `O_DSYNC` on ext4 over hydo's virtio disk, `fallocate`, the
  multiversion binary's self-check, and its AES-NI and AVX2 code (with `guest.cpu: v3`) all work
  under hvx, and every schedule gives the same fingerprint twice. A run from a fresh boot takes
  23–56 s of wall time.
- **Can it run the three-replica cluster of the Compose file?** No. One replica of the release
  binary allocates and touches 2.1 GiB at start (1.35 GiB with `--development`, which also turns
  direct I/O off), whatever `--cache-grid` says (its minimum is 32 MiB); TigerBeetle's
  documentation asks for at least 6 GiB per replica. hydo's x86_64 guest has at most 3072 MiB. In
  the guest the kernel's out-of-memory killer ends two of the three replicas during provisioning
  ("Memory" below). Every cluster finding of TigerBeetle's issue tracker that hydo's faults could
  reach needs three replicas. **Since Milestone 179 it can:** with `guest.ram_mib: 8192` the
  three replicas provision and serve, every schedule twice with one fingerprint, and a short
  process exploration found nothing ("Three replicas in 8 GiB" at the end).
- **Can it find anything?** Not on one replica. No acknowledged transfer was lost and no
  balance went wrong in 50 explored schedules on an honest disk (process strategy on 0.17.9 and
  0.16.29: kills, pauses, clock steps of up to a year either way, power cuts by round and at the
  k-th flush) and in the hand-written ones on 0.17.9, 0.16.45 and 0.16.29. The only failures (10
  of 20 swarm schedules) need a disk fault, and they are TigerBeetle's documented
  behaviour for a replica with no peer to repair from (it stops instead of serving a damaged
  ledger). On the way hydo produced one convincing false finding ("every acknowledged transfer
  lost at every power cut", reproducible, twice the same fingerprint), caused by what a check
  sees in the recovery boot; it is explained below and documented now.

## What was hunted

Sources: Jepsen's analysis of TigerBeetle 0.16.11 (https://jepsen.io/analyses/tigerbeetle-0.16.11,
June 2025) and TigerBeetle's CHANGELOG up to 0.17.9. The findings, and whether hydo's fault kinds
(process kills, pauses, network faults, power cuts, clock steps, disk faults on one shared data
disk) reach them:

| finding | versions | trigger | reachable here? |
|---|---|---|---|
| #2739: replication along a ring turns one replica's pause or crash into 10–100 s request latencies | 0.16.11 to 0.16.42 (partly fixed in 0.16.30, fully in 0.16.43) | `SIGSTOP`/`SIGKILL` of one of 3 or 5 replicas | yes with three replicas (`P=`, `K=`, and a latency check); blocked by memory |
| #2544: queries with several filters omit results at their end | 0.16.13 to 0.16.16 | no fault: many objects spread over several LSM tables per level, so that a zig-zag merge join skips tables | not tried: a REPL-driven workload writes tens of transfers per run, far from several LSM tables per level |
| #3802: the object cache's stash overflows (a crash) | 0.17.6, 0.17.7 | no fault; a workload pattern with the object cache on | not tried: the trigger is not described beyond "some situations" |
| #2767: no safe way to rebuild a replica that lost its data file | 0.16.11 to 0.16.42 | one replica's disk lost, then reformatted | no: hydo has no "lose one service's files" fault (one data disk for the whole guest) |
| #2681, corruption on several replicas, superblock rollback | 0.16.20 to 0.16.25 | bit flips and misdirected writes inside one replica's file | no: hydo's disk faults act on the whole disk's requests, not on one file's zones |
| #2745, #2758, #2763 | 0.16.25 to 0.16.28 | rolling upgrades between releases | no: hydo has no upgrade fault |
| power loss | (not tested by Jepsen) | every replica loses what was not flushed, at once | yes: `C=`, `cf=` and `disk=cut`; the hunt below |

Hunted on one replica: the durability of acknowledged transfers and the conservation of balances
under kills, pauses, clock steps, power cuts and disk faults, on 0.17.9, with the same schedules
on 0.16.45 and 0.16.29.

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | TigerBeetle's documented three-replica Compose file, the release pinned (`TB_VERSION`, default 0.17.9) |
| `fetch-tigerbeetle.sh` | downloads the static x86_64 binary of each release from GitHub into `bin/<version>/tigerbeetle`, checked by sha256 |
| `hydo/hydo.yaml` | three replicas (from `hydo compose`; hand changes marked `hand:`), `network: bridge`, `params: tb` chooses the release, `guest.ram_mib: 8192` since Milestone 179 (with 3072 MiB it ran out of memory; "Three replicas in 8 GiB" below) |
| `hydo/hydo-single.yaml` | one replica (a cluster of one), the same workload and checks: what fits |
| `hydo/workload/helper_tb.sh` | the client: TigerBeetle's own REPL (`tigerbeetle repl --command=...`, built into the server binary, so there is no client to build), every call under `timeout`; parses both REPL answer formats (0.16 and 0.17 differ) |
| `hydo/workload/first_accounts` | accounts 1..8 on ledger 700, before any fault |
| `hydo/workload/parallel_driver_transfer` | one transfer between two accounts chosen from `HYDO_SEED`; the transfer id encodes accounts and amount; an attempt without an answer in 10 s is unknown and is sent again with the same id (TigerBeetle answers `exists` if the first one was applied) |
| `hydo/checks/final_ledger` | every acknowledged transfer is in the ledger (`query_transfers`), with what its id says; no transfer that no client sent; every account's posted debits and credits equal the sum of its transfers |
| `hydo/checks/eventually_serves` | after the faults, the cluster answers a lookup within 60 s |

Build and run (Linux, x86_64):

```sh
sh fetch-tigerbeetle.sh                                   # 0.17.9, 0.16.45, 0.16.29
hydo build hydo/hydo-single.yaml                          # one replica, 0.17.9
hydo build hydo/hydo-single.yaml --set tb=0.16.29 --out img-0.16.29
hydo run "disk=none dseed=1 | run"
hydo explore --seeds 1-30 --strategy process --more-faults
hydo build hydo/hydo.yaml                                 # three replicas, 8 GiB (Milestone 179: needs the module with the split RAM)
```

## Memory

Measured natively on the bare-metal machine (the release binary, `start` with one replica's
data file; resident set after start):

| mode | flags | resident |
|---|---|---|
| production | `--cache-grid=32MiB` (the minimum) | 2.05 GiB ("Allocated 2126MiB during replica init" in the guest) |
| development | `--development --cache-grid=64MiB` | 1.47 GiB |
| development | `--development --experimental --limit-pipeline-requests=0 --limit-request=32KiB --memory-lsm-manifest=8MiB --memory-lsm-compaction=11MiB --cache-grid=32MiB` (every smaller limit) | 1.32 GiB |

The rest is sized at compile time (the message pool and the LSM tables follow `message_size_max`
and the batch size), and `--development` also turns direct I/O off. Three replicas need 4 to
6.2 GiB; hydo's x86_64 guest has at most 3072 MiB (`guest.ram_mib`). With `hydo/hydo.yaml`
(0.17.9, 3072 MiB) the three replicas start and pass their readiness checks, then the guest
kernel's out-of-memory killer ends replicas 0 and 1 (`Out of memory: Killed process ...
(tigerbeetle) anon-rss:2195228kB`), replica 2 logs `no agreement on cluster time (partitioned
or too many clock faults)`, and the run ends `ERROR provision failed` (`disk=none dseed=1 | run`, twice `da2a2b47273222ec`; image `22356f8c…`). The data
files fit: each is 1.06 GiB, preallocated with `fallocate` at format time, so three need 3.2 GiB
of hydo's disk, whose whole image must stay under 4 GiB (`data.size_mib: 3584`).

A TigerBeetle built from source with a smaller compile-time configuration (TigerBeetle's own
simulator uses such configurations) would fit, but it is not the program users run, and
building it was out of scope here.

## One replica: runs and verdicts

Every schedule twice from a fresh boot (`hydo run`), the same fingerprint (fp and ctl) both
times. One replica (`hydo/hydo-single.yaml`), `guest.cpu: v3`, 3072 MiB.

Images: `hydo build hydo/hydo-single.yaml [--set tb=<release>]`, disk image sha256 0.17.9
`063c9c34…`, 0.16.45 `5078ecbe…`, 0.16.29 `28a7d73a…` (with the folder of this commit).

| release | schedule | verdict | fp | ctl |
|---|---|---|---|---|
| 0.17.9 | `disk=none dseed=1 \| run` | OK, 33 acknowledged, 33 in the ledger | `a00435e961ccd16b` | `f09629cf91bc4597` |
| 0.17.9 | `disk=cut dseed=1 \| run C=10` (power cut, unflushed writes lost) | OK, 15 acknowledged, all there after recovery | `85508211b75c8055` | `bdf344ea8adf561b` |
| 0.17.9 | `disk=cut dseed=1 \| run C=20` | OK, 33 of 33 | `55fc3cd6479f8b9e` | `10e363e15cd6c1e4` |
| 0.17.9 | `disk=none dseed=1 \| run K=tigerbeetle_0@5+3` (kill, restart 3 rounds later) | OK, 30 acknowledged, 3 unknown | `05dd3bdc81a5d3e8` | `225833cc5b5a340c` |
| 0.17.9 | `disk=none dseed=1 \| run P=tigerbeetle_0@5+5` (pause) | OK, 29 acknowledged, 4 unknown | `65b9351af94b0bcc` | `55714d12229d6edc` |
| 0.17.9 | `disk=none dseed=1 \| run T=5:-30` (clock 30 s back) | OK, 33 of 33 | `287b5566b6770d57` | `fbb583a769c60f24` |
| 0.17.9 | `disk=none dseed=1 \| run T=8:-60 K=tigerbeetle_0@10+2` (clock back, then a restart) | OK, 30 of 30 | `f1255268c2844f51` | `cf721ac9060ed2d9` |
| 0.17.9 | `disk=lying:3 dseed=1 \| run C=12` | OK, 18 of 18 | `9b27dfbe4ff5ce88` | `db3564a76a827f03` |
| 0.17.9 | `disk=ioerr:2 dseed=1 \| run` | ERROR restart after the faults: `panic: latent sector error: no spare sectors to reallocate` | `f90390b2abb345de` | `a785e31939962fff` |
| 0.16.45 | `disk=none dseed=1 \| run` | OK, 33 of 33 | `a5ccd89aaa5abf9b` | `b21c37e0d505091b` |
| 0.16.45 | `disk=cut dseed=1 \| run C=10` | OK, 15 of 15 | `a5f2d065dceec5f7` | `21d2a2ea2a2109e7` |
| 0.16.29 | `disk=none dseed=1 \| run` | OK, 33 of 33 | `0a4b656e8cb2f648` | `535b48176a5e4992` |
| 0.16.29 | `disk=cut dseed=1 \| run C=10` | OK, 15 of 15 | `62aa827ef1a2288b` | `d8229dbbfa2d63c0` |

A one-replica run takes 23–56 s of wall time from a fresh boot (the shared machine was busier
for the 0.16 runs), about 7–14 s of guest time; inside an exploration, from snapshots in memory,
8–13 s.

## Explorations

One worker (one VM at a time on the shared machine), `HYDO_QUIET_FILE=/var/tmp/hydo-QUIET`.

| release | command | runs | failures | classes and signatures | wall |
|---|---|---|---|---|---|
| 0.17.9 | `hydo explore --seeds 1-30 --strategy process --more-faults` | 30 (and the `cf=` probe: flushes 1–211) | 0 | | 446 s |
| 0.17.9 | `hydo explore --seeds 1-20 --strategy swarm --new-faults` | 20 | 10, all reproduced | io-errors: `ERROR restart after the faults (tigerbeetle_#)` 2, `ERROR tigerbeetle_# restart failed` 3; lying-disk: `VIOLATION service cannot recover (committed data unrecoverable)` 4, `ERROR ... restart failed` 1; honest disk: none | 615 s |
| 0.16.29 | `hydo explore --seeds 1-20 --strategy process --more-faults` | 20 (and the `cf=` probe) | 0 | | 328 s (and 540 s waiting for another session's quiet window) |

The process strategy drew kills, pauses, clock steps from 30 s up to a year in both directions,
power cuts by round (`C=`) and at the k-th disk flush (`cf=`), all on an honest disk: every
acknowledged transfer survived and every balance matched. The swarm strategy's failures all
need a disk fault: with I/O errors TigerBeetle stops on purpose (a replica that reads an error
treats the sector as lost and, alone, has no peer to repair it from), and with a lying disk
(acknowledged writes dropped) plus a power cut it refuses to start rather than serve a ledger
with holes. The swarm's minimal reproducer of the latter, twice `822923ad2777dfd3`:
`disk=lying:4 dseed=17144527b62d669c | run C=17` with twenty single transfers (the composition
shrinks no further: the lying disk needs enough writes to drop an acknowledged one). These are
TigerBeetle's documented behaviour for a single replica, not findings; on three replicas they
are where a bug in repair would show.

## A false finding on the way: every acknowledged transfer "lost" at every power cut

The first power-cut schedule, `disk=cut dseed=1 | run C=10`, ended `VIOLATION [ledger] lost 15 of
15 acknowledged transfers (found 0)`, twice with one fingerprint, and so did `disk=none ... C=10`
(an honest disk) and `C=20`. It took about 50 minutes to show that TigerBeetle was right and the
check was wrong:

1. The durable disk image after the cut (`HVX_EXPLORE_DURABLE_OUT`, with the engine run as root,
   because `sudo` drops the variable) holds every transfer: after an ext4 journal replay
   (`e2fsck -fy` on a copy) the data file contains the acknowledged transfer ids.
2. TigerBeetle 0.17.9 started natively on that file answers with every transfer and the right
   balances.
3. In the guest, the same file (same md5 before the replica starts in the recovery boot) and
   TigerBeetle's debug log (`--experimental --log-debug`) show the journal recovered slot by slot
   (`recover_slots: dirty=0 faulty=0`) and every `create_transfers` operation executed again.
4. The check looked up the ids listed in `$HYDO_OUTCOMES`, and in the recovery boot after a power
   cut that file is empty: hydo carries only the acknowledged and the failed ids into the
   recovery boot. The check looked up nothing, found nothing, and reported every acknowledged id
   lost.

The check now reads the whole ledger with `query_transfers`, and hydo's guide says what the
recovery boot's checks get (friction item 9, FIXED in the guide). With it every power-cut
schedule passes.

## Time

Wall time on 2026-11-07 (the bare-metal machine's clock, UTC), from the first look at
TigerBeetle's documentation:

| phase | from | to | took |
|---|---|---|---|
| read the docs, Jepsen's analysis, CHANGELOG; native memory measurements; build and install a release tarball from this tree; `hydo compose`; write the folder; `hydo build --check` OK; first image | 15:27 | 15:51 | 24 min |
| wait: another session had asked for quiet on the shared machine (`/var/tmp/hydo-QUIET`) | 15:52 | 16:51 | 59 min |
| first green run (`disk=none dseed=1 \| run`, OK, twice with one fingerprint) | | 16:52 | **85 min from the start, 25 min of work** |
| three replicas booted once: out of memory, diagnosed | 16:53 | 17:00 | 7 min |
| the false finding (above): found, chased to the check, fixed | 17:00 | 17:57 | 57 min |
| one-replica schedules on three releases, twice each; three explorations (one waited 9 min for another quiet window); the three-replica run twice | 17:57 | 18:50 | 53 min |

No real finding was made, so there is no "time to first finding"; the time to the first
(false) violation, the first power cut, was about 93 minutes (16:59).

## Friction log: what an outside user needed and where they looked

Each item: what was needed, where it was looked for, and what was done. Items marked FIXED were
changed in hydo on this branch (their own commit, with a test or a re-recorded golden).

1. **Installing hydo from this tree.** `cargo build -p hydo` in a checkout gives a `hydo` that
   takes `target/` for a release: `hydo doctor` reports 10 failures (no guest kernel, no
   engine program, no mke2fs), and `hydo build` refuses every plan with `hydo_version: "0.1"`
   ("this hydo is 0.0.0"). The way through was `hydo/dist/linux/build-release.sh` (with the
   pinned assets copied from a local directory) and `install.sh --no-module` into a prefix,
   about 10 minutes; the script's last step, the sources archive, stops for want of the
   git-ignored machines file, after the tarball is written.
2. **`network_mode: host`.** TigerBeetle's documented Compose file passes
   `--addresses=0.0.0.0:3001,0.0.0.0:3002,0.0.0.0:3003` and runs every replica on the host's
   network. `hydo compose` gave each service its own namespace and said only "network_mode is
   ignored", so every replica would have dialled itself. FIXED: the warning for `network_mode:
   host` now says that peers named as `0.0.0.0:<port>` or `localhost:<port>` must become their
   `addr:` (or import with `--network loopback`); the Compose golden re-recorded. The addresses
   were changed by hand to the services' `addr:` values.
3. **No format step.** The documentation formats each data file once with `docker run ... format`
   before `docker compose up`; the Compose file has no such step, so the import has none. The
   services guide's `setup:` (idempotent, before every start) took it:
   `[ -f FILE ] || tigerbeetle format ...`.
4. **The client next to an image service.** The image `ghcr.io/tigerbeetle/tigerbeetle` holds the
   only client the project ships without a language runtime (`tigerbeetle repl`). `hydo-oci
   <service> <command>` runs a command inside that service's network namespace, so the workload
   could only talk to the cluster from inside one replica, where a link fault on that replica
   would also cut the client. The services were switched to the release binary in `root.files`
   (the documentation itself says Docker is not recommended), and the workload runs the same
   binary from the host side of the bridge.
5. **`cmd:` as a list.** `cmd: [/opt/tb/tigerbeetle, start, ...]` was refused ("field 'cmd' must
   be a str") for a service without an image, while the importer writes lists for image
   services. Rewritten as a string.
6. **CPU features.** The release binary is built for x86-64-v3 with AES-NI. `guest.cpu: v3` was
   needed (the hazards guide says so for AVX2). hvx's CPUID advertises no AES-NI even at v3;
   TigerBeetle does not check CPUID and the instructions run, so it works, but a program that
   checks the bit would take another path or refuse to start.
7. **Memory.** The blocker for the three-replica cluster ("Memory" above).
8. **Seeing why provisioning failed.** The three-replica run ended `ERROR provision failed` and
   nothing more: the run directory has no service log and no line saying that two services had
   died. The command's own diagnostics (`free`, `dmesg | grep oom`, the services' log tails,
   written to `/dev/console`) appear in `one.log`, after the boot messages; that is how the
   out-of-memory kills were seen.
9. **`HYDO_OUTCOMES` after a power cut.** The check read the transfer ids from `$HYDO_OUTCOMES`,
   which is empty in the recovery boot; every power cut then looked like the loss of every
   acknowledged transfer (the false finding above, 57 minutes). The composer guide said
   `HYDO_ACKED` is carried across a power cut and said nothing about the outcomes file. FIXED
   in the guide (composer and check-pattern topics); the data itself (the unknown ids) is still
   not carried, so a check must read the store in full.
10. **Debugging a check costs an image build.** Checks are inside the image, so every change to
   a diagnostic meant `hydo build` (about 40 s) and a fresh run (25–40 s). There is no way to run
   the checks again against the state a run ended in, or to look at a recovery boot's files
   after the run.
11. **The durable disk.** `HVX_EXPLORE_DURABLE_OUT` (a developer variable, found in
   `hydo-config/INVENTORY.md`) writes the disk a recovery boot starts from, but only when the
   engine runs inside `hydo`: under `hvx-run sudo` the variable is dropped, so the run had to be
   started as root (`hvx-run sudo env ... hydo run`).
12. **TigerBeetle's client never gives up** (Jepsen's #206): every REPL call is wrapped in
   `timeout`, and a call that hits it is an unknown outcome, sent again with the same id.
13. **The REPL's answers changed in 0.17**: 0.17 prints a status object per event, 0.16 prints
   nothing on success and `Failed to create ...` lines otherwise, and an empty query is exit
   status 1 before 0.17. The helper parses both (not hydo's concern; it took one failed
   provisioning on 0.16.29 to see).
14. **I/O errors and lying disks on one replica.** With `disk=ioerr:<p>` TigerBeetle stops with
   `panic: latent sector error: no spare sectors to reallocate` and does not come back
   (`ERROR restart after the faults`); with a lying disk and a power cut it refuses to start
   (`service cannot recover`). Both are TigerBeetle's design for a replica that cannot repair
   from a peer; hydo classes them correctly, and they only become interesting with three
   replicas.
15. **`hydo run --help`** prints "no image given ..." instead of help.
16. **The shared machine.** 59 minutes waiting for another session's quiet request before the
   first VM.
17. **Corpora.** The new directory entered the test corpora, as NATS's did: the controller's
   identity corpus (`hydo-control --test corpus`), the explorer's schedule corpus and identity
   stream (`hydo-explore --test corpus --test identity`) were re-recorded with `HYDO_BLESS=1`.
   The diffs only add this example's inputs and rows, except the explorer's quick identity
   stream, which samples manifests by position: adding one moved its sample from three
   manifests to three others (`ledger-spin`, `redis-bridge`, `redis/hydo-read` out; `ledger`,
   `redis/hydo-hist`, `redis/hydo.yaml` in), each still byte-identical to what the explorer
   produces.

## What hydo lacked, ranked by how much it blocked

1. **More guest memory, or one guest per service.** (Done in Milestone 179: guests of up to
   8 GiB, "Three replicas in 8 GiB" below.) The whole point of the Compose file, a
   three-replica cluster, could not run: TigerBeetle allocates 2.1 GiB per replica at start
   (1.35 GiB in development mode) and hydo's x86_64 guest stops at 3072 MiB, so the out-of-memory
   killer ended two of three replicas. Every cluster finding a hydo fault could reach (#2739's
   latency cascade after one replica's pause or crash; view changes, repair from a peer, I/O
   errors and lying disks that a replica can repair) needs three replicas.
2. **What the recovery boot's checks get.** `$HYDO_OUTCOMES` is empty there and the unknown ids
   are not carried, which produced a convincing, reproducible, false "every acknowledged write
   lost" finding and cost an hour (now documented; the data is still not carried).
3. **A view of a failed provisioning, and of a run's end state.** "ERROR provision failed" with
   no service log and no word that services died; no way to look at the guest's files after a
   run or to run a check again without rebuilding the image.
4. **Faults on one service's storage.** One data disk for the whole guest: no file corruption in
   one replica's file, no loss of one replica's disk, no power cut of one service (Jepsen's
   #2767, #2681 and the corruption findings).
5. **Upgrade faults.** No fault that replaces a running service's binary with another release
   (Jepsen's #2745, #2758, #2763).
6. **Running an image's tool from the workload's side.** `hydo-oci` runs only inside the
   service's namespace.
7. **A usable `hydo` straight from a checkout** (friction 1).

## Three replicas in 8 GiB (Milestone 179)

The user chose a bigger single guest over one guest per service (2026-11-07). Since Milestone 179
`guest.ram_mib` goes up to 8192 on every engine (on x86_64 the first 3 GiB below the 4th-GiB hole
and the rest from 4 GiB), and `hydo/hydo.yaml` asks for 8192 MiB. Run on the bare-metal machine
with hvx's side module `hvxbig` (this tree's `hvx.c`, `hv/hvx/m156/side.sh hvxbig <file> load`;
the shared main module takes at most 3 GiB until it is reloaded with the change), the engine in
`hydo`'s process as root (`HVX_DEV=/dev/hvxbig`, `HYDO_HVX_PREPARE=0` so that only one 8 GiB VM
of this session existed at a time), one VM at a time. Image: `hydo build hydo/hydo.yaml`
(0.17.9), disk image sha256 `4dfaec44…`.

Every schedule twice from a fresh boot, the same fp and ctl both times; all three replicas
provision and serve (no out-of-memory kill):

| schedule | verdict | fp | ctl |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK, 33 transfers started, 33 ok | `03a1dc26eb38c91a` | `d7fab49379f54a52` |
| `disk=cut dseed=1 \| run C=10` (power cut) | OK (recovery boot: every acknowledged transfer there) | `a250b2bea41e9672` | `8297acacb1bdd379` |
| `disk=none dseed=1 \| run K=tigerbeetle_0@5+3` (kill one replica) | OK, 33 of 33 | `8e4f8468f12a7813` | `9508489d89c3f476` |
| `disk=none dseed=1 \| run P=tigerbeetle_1@5+5` (pause one replica) | OK, 33 of 33 | `dd23cdddd0be2b78` | `d3ef0399ac20a801` |
| `disk=none dseed=1 \| run T=5:-30` (clock 30 s back) | OK, 33 of 33 | `7b9185132f875605` | `addfa62b08c31696` |

A run from the explorer's worker snapshot S1 (`hydo explore --schedule "disk=none dseed=1 | run"`)
gives the fresh boot's `03a1dc26eb38c91a`. A fresh-boot run takes 56–160 s of wall time on the
shared machine (18–30 s of guest time); the worker boots once in 80 s and a run from S1 takes
21–29 s.

Exploration: `hydo explore --seeds 1-10 --strategy process --more-faults --workers 1` (the
`cf=` probe measured flushes 1–509): 10 schedules with kills, pauses, network loss and delay
between replicas, one-way faults, clock steps from 30 s to 30 days back, power cuts by round and
at the k-th flush: **0 failures**, every property held (`ledger` 10 of 10, `serves` within
4.7 s). 349 s of sweep, 460 s in all. The cluster findings listed above (#2739's latency cascade
needs a latency check; repair from a peer needs a fault on one replica's storage, which hydo does
not have) were not hunted further.

Memory on the host: the explorer's process held about 19 GB (the 8 GiB VM, the 3.5 GiB data
disk mapped, and S0 and S1 in memory: the hvx engine keeps both there), so one such worker
needs a machine with about 20 GB free; `hydo explore` sizes its pool from the guest's RAM and the
machine's available memory, and `hydo run` refuses a guest that does not fit.

## On the KVM engine (Milestone 187)

Since Milestone 187 the KVM engine runs `guest.cpu: v3` images, so the three-replica image above
(`4dfaec44…`) was tried there (the build machine, `hydo run --backend kvm`, 8 GiB). It does not
run: every schedule ends `ERROR boot: SPIN (guest loops without exits; run invalid)` during the
first replica's `setup:`. `tigerbeetle format` starts, its AVX2 code runs, and it then waits for
its disk writes by polling io_uring's completion queue from user space, with no system call and
so no VM exit. The KVM engine delivers interrupts only at VM exits, so the disk's completion never
reaches the guest; with `HYDO_KVM_SPIN_SECS=600` it spun until stopped. On hvx the PMU clock
interrupts such a loop, which is why the cluster runs there. This is a limit of the KVM engine
for any program that busy-polls without system calls (AGENTS.md open item 45 (c)), not of the v3
CPU; TigerBeetle stays an hvx example (docs/design/kvm-engine.md, "x86-64-v3").
