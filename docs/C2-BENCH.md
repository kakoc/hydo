# C2: the bug benchmark on shell images and agent images (Milestone 129)

> Since Milestone 136 (step C3) the benchmark's manifest images are agent images only (hydo-bench
> `--kind shell` is refused), the order service's "a restart after the faults may fail" is the
> manifest key `restart_may_fail: true` instead of section 9's wrapper script, and the images were
> rebuilt with the fixed agent (M127) and the triggers confirmed again: `hydo/bench/README.md`
> "The manifest images after the switch (Milestone 136)".

Step C2 of `docs/design/guest-control.md` asks whether a strategy finds each planted defect of the
bug benchmark (`hydo/bench`, defects d0–d12) as often on agent images as on shell images. This
file holds the answer for the bare-metal test machine (hvx). The criterion is a strategy's
detection rate, not single verdicts, because a change of guest timing changes which schedule
finds what.

**Agent version.** Every agent image here carries hydo-agent 0.1.0 (the M117 pin, x86_64 sha256 `ff7652a1…`); M131's 0.2.0 (`1f517e04…`, an optional `netns` field in `freeze`, busybox's exact mount-error text) came later. No schedule here uses `F=` or a failed mount, so the comparison holds for 0.2.0 images too, but their fingerprints will differ. M131's D7 (a timed-out check prints `Terminated` only on shell images) did not show up: no console of this campaign has a `Terminated` line on either kind.

## 1. The benchmark images cannot be driven by the host controller

The benchmark images (`explore-bench-*`, `explore-nf`, `explore-disk`/`explore-fix-disk` of the
pinned reproducers, and every other `explore-*` image that `hv/hvx/images/build-stack.sh` builds)
run the four-service stack under the stack's own hand-written init: `build-stack.sh`'s
`/sbin/init` sources `/etc/stack-explore.sh`, which reads the explorer's commands on the console.
They carry no `/etc/hydo/hydo.yaml`. The host controller (`hydo-control`) drives manifests: its
`ScheduleController` does what `hydo/onboard/hydo-init.sh` does, from the plan of the image's
embedded manifest.

A copy of `explore-bench-m59-control-pin-kr-disk.img` made into an agent image with
`hydo/bench/agent-patch.sh` (`/sbin/init` = the pinned hydo-agent, `/etc/hydo/control` =
`agent 1`) shows what happens:

- `hydo-bench` (before this milestone) and `hydo-explore` without a manifest start the plain hvx
  engine, which refuses the image: `hvx_explore: the guest's first process is hydo-agent (an agent
  image, /etc/hydo/control): boot it with --control boot`. Every worker fails to start.
- `hydo-explore` in manifest mode (an `explore-ob-*` name) refuses it earlier:
  `manifest_space: no /etc/hydo/hydo.yaml in … (not a hydo-build image?)`, and
  `hydo_control::agent::plan_of_image` gives the same refusal (no embedded manifest).

What C3 needs before these images can run under the agent (one of the two):

1. **A controller for the stack's init**, written from `stack-explore.sh` and the functions of
   `build-stack.sh`'s init that it calls, as `ScheduleController` was written from
   `hydo-init.sh`: its own start and readiness polls for Redis, MySQL, Kafka and the order service
   (with the `/tmp/*.log` files and the `hydo-stack:` lines), the client batches with
   `HYDO_CONC`/`HYDO_DUP` (`Q=`), the timed kill `X=`, `M=` that also sets `sync_binlog=0`, the
   restart of a dead order service before the retry pass with its `hydo-explore: front door log:`
   lines (the M59 crash oracle reads them), the M67 path where the order service cannot come back
   after a `cf=` cut, `explore_oracle`'s restarts of dead stores, and the recovery boot's oracle; or
2. **the stack images rebuilt from a manifest**, which needs the build machine (Nix store paths).
   `hydo/bench/manifest/hydo.yaml` (this milestone) is such a manifest; section 2 shows that the
   controller drives it.

Until one of the two is done, the benchmark images, the pinned reproducers' images and the
stack's explorer images stay shell-only.

## 2. What can be driven: the benchmark guest as a manifest

`hydo/bench/manifest/hydo.yaml` describes the benchmark's guest as a manifest, so that one builder
gives a shell image and an agent image of each defect from the same files:

- **Files:** those of the `-pin-kr` control image (`explore-bench-m59-control-pin-kr-disk.img`),
  dumped with `debugfs rdump` on the bare-metal test machine: the 65 store paths of its
  `/nix/store` that are not in the pinned iproute2 closure (MySQL 8.4.10 and OpenJDK 17 with their
  closures; hydo-build stages the iproute2 closure itself, from the same pinned path), Kafka's
  libs and the pinned Redis. `front-door` is the defect's order service, `/bin/hydostack` of its
  benchmark image.
- **Configuration:** `build-stack.sh`'s: the same `my.cnf` (with `skip-log-bin`, as every
  benchmark image has it) plus the line the init appends (`innodb_flush_log_at_trx_commit=1`), the
  same Kafka properties with `log.retention.ms=-1` (the `-pin-kr` fix) and the appended
  `log.flush.interval.messages=1`, Redis with `appendfsync always`, 30 orders, the fixed clock.
- **Tokens:** `stack-explore.sh`'s tokens that the generic init lacks are knobs: `Q=<n>[d]`
  writes `HYDO_CONC`/`HYDO_DUP` for the client, `M=` also sets `sync_binlog=0`. `X=` (the stack's
  timed kill) has no knob: the shell init ignores it, the host controller gives it the stack's
  meaning (hydo-control `HANDOFF.md` decision 12). This is a designed difference; section 4 says
  what it does to d10.
- **Oracles:** the first invariant is `stack-explore.sh`'s (`hydostack oracle`); the second is the
  M59 crash oracle (the order service's log must have no `panic`/`fatal error` line, hydo-bench's
  `crash_verdict`), so d9's signature `front door crashed` is in the verdict on both kinds.
- **What stays different from the benchmark images:** the generic init's workload loop, restarts
  and markers instead of `stack-explore.sh`'s (no M67 "front door cannot restart" path, a
  restart of every dead service before the retry pass, other console lines), so the fingerprints
  and some timings differ from M108's. The comparison is between this manifest's shell and agent
  images; the original images' M108 results are given beside them.

**Building on the bare-metal test machine** (no Nix): `hydo/bench/build-bench-manifest.sh` runs this
tree's `hydo build` in a release layout (the Linux release's `guest/`, `tools/` and `libexec/`, this
tree's `bin/hydo` and `hydo/onboard`) twice per image, with `--control shell` and `--control agent`.
The control pair and d8 were built in full; every other defect's pair is a copy of the control
pair with `/bin/hydostack` replaced (`FROM=`, `agent-patch.sh --swap`), because the manifest and
every other file are the same.

**Proofs that the images are what they claim** (`agent-patch.sh --check`: every entry's type, mode,
link target, owner and file contents, by `debugfs rdump` and a `debugfs ls -p` walk; the data
partitions by their SHA-256):

| check | result |
|---|---|
| the Redis example: M121's shell image patched by `agent-patch.sh` vs M121's `hydo build --control agent` image | same tree (423 entries, 13 files); same data partition `2d52cd04…`; the unpatched shell image differs in exactly `/sbin/init` and `/etc/hydo/control` |
| the manifest's control: the shell build patched vs the agent build | same tree (14,425 entries, 9,966 files, every owner and mode) |
| d8: the control pair with d8's order service swapped in (`--swap`) vs d8 built in full | same tree for the shell pair (14,424 entries, 9,965 files) and for the agent pair (14,425 entries, 9,966 files) |

Images (sha256 prefixes; in `~/c2b-work/bm/{shell,agent}` on the box, under hydo-bench's names):
control shell `01fc889be9b8d807`, agent `50e6ee9a4903e85c`; d8 (built) shell `821d2f989e6cad69`,
agent `36ac86dd40f28bd9`.

**The controller drives the manifest's agent images.** On the control pair, `hydo-bench list`
(one worker, fresh pools) gives the same verdicts on both kinds:

| schedule | shell | agent |
|---|---|---|
| `disk=none dseed=0 \| run` | OK acked=30/30 `59d12b91b1de0233` | OK acked=30/30 `3daf8ea141b7c843` |
| `disk=none dseed=0 \| run Q=2d` | OK acked=30/30 `c30e6098f3d4cec8` | OK acked=30/30 `ef8bd4c3399d3289` |
| `disk=cut dseed=0 \| run C=15` | OK acked=15/30 `805420f1ae19f622` | OK acked=15/30 `709c4c2bc7440ddb` |

## 3. The triggers (`hydo-bench confirm`)

Each defect's hand-written trigger runs twice on its image (both runs gave the same fingerprint
in every row below) and once on the control of the same kind. "M108" is the original benchmark
image's result from M108's proof (`/var/tmp/m108/rs` on the box).

| defect | trigger | M108 (original image) | shell (manifest) | agent (manifest) |
|---|---|---|---|---|
| d1-kafka-first | `disk=none dseed=0 \| run K=mysql@10+1` | VIOLATION order=10 twice in Kafka (M108's d0/d3 run; d1 confirmed in M54) | VIOLATION order=10 present 2 times `c39344804028e7a8`; control OK | VIOLATION order=10 present 2 times `5e89855d88b1c00d`; control OK |
| d5-myisam | `disk=cut dseed=0 \| run C=15` | (M54) | VIOLATION mysql order=0 MISSING `e683fa1df14af57e`; control OK | VIOLATION mysql order=0 MISSING `c15e9c5879d9a0b1`; control OK |
| d8-no-order-lock | `disk=none dseed=0 \| run Q=2d` | VIOLATION order=0 present 2 times `6a56326d6de88bba` | VIOLATION order=0 present 2 times `b5db689787b3fa39`; control OK | VIOLATION order=0 present 2 times `2d9c12461b7c35d4`; control OK |
| d9-redis-unlocked | `disk=none dseed=0 \| run Q=4` | VIOLATION front door crashed: panic: slice bounds out of range `6a5f06b675687ac4` | **OK** (no panic) `9d1fa0421dcd233a` | VIOLATION [front.no_crash] front door crashed: panic: slice bounds out of range `7d4f2e7e31e03f27`; control OK |
| d10-outcome-in-memory | `disk=none dseed=0 \| run X=front@9~20` | VIOLATION order=10 present 2 times `5d03c640c24f667d` | **OK**: the shell init ignores `X=` (`knob X not in the manifest, ignored`) `acf36daf59e7b196` | VIOLATION order=10 present 2 times `51e4d29967f025b1`; control OK |
| d11-ack-unflushed-flag | `disk=cut dseed=0 cf=238 \| run` | VIOLATION order=22 acked but not marked published `adadedf9698291bd` | **OK** acked=23/30 `9ddbc9ce8d7e842f` | **OK** acked=23/30 `bfce19ce39f712d8` |
| d12-shared-key-buffer | `disk=none dseed=0 \| run Q=2` | VIOLATION order=0 present 2 times `d89a283a66ac19a9` | VIOLATION order=0 present 2 times `7e3d75367b1f9ee6`; control OK | VIOLATION order=0 present 2 times `a4f7227c1371b44c`; control OK |

The other defects' triggers (`hydo-bench confirm --base`, wide plan step `confirm-rest`): on both
kinds every trigger fails twice with the same fingerprint, with the original image's verdict, and
every control passes it.

| defect | trigger | shell (manifest) | agent (manifest) |
|---|---|---|---|
| d0-old-front-door (the pinned reproducer's schedule; control = the M39 fix) | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION order=11 in Kafka but never acknowledged `d7a739a52d1a7bd2`; the fix: OK acked=24/30 `240f9dedeab39b54` | VIOLATION order=11 … `7064cd947dbbe114`; the fix: OK acked=24/30 `81d6117b6f11a016` |
| d2-batched-publish-flag | `disk=cut dseed=0 \| run C=12` | VIOLATION order=10 acked but not marked published `5c3794dc4bc8ed0b` | the same `5fa55f7273f52666` |
| d3-blind-retry | `P=kafka@5+2`; `O=kafka@5-10:resp:loss,100%` | order=5 twice in Kafka, both | the same, both |
| d4-produce-timeout-definite | `O=kafka@5-10:resp:loss,100% C=12`; `P=kafka@5+2 C=8` | order=5 in Kafka but told failed, both | the same, both |
| d6-client-deadline-definite | `P=kafka@5+2` | order=5 in Kafka but told failed | the same |
| d7-skip-last-record | `P=kafka@28+2` | order=29 twice in Kafka | the same |
| the control's known bug (README "A bug in the fixed front door") | `O=kafka@25-30:resp:loss,100%` | VIOLATION order=26 twice in Kafka, twice `7dd0d0fe188a6f46` (M108's original image: order 26 too) | the same, twice `af463a05cc06b797` |

## 4. M108's cells (the same cells, seeds and budgets as M108's proof)

`hydo-bench run` with M108's arguments (process with `--more-faults --cf-range 1-309` on d10/d11,
budget 8; swarm with `--concurrency` on d8/d12, budget 6; random on d1/d5, budget 4; repetition 1,
seeds 1001…; 2 workers), first on the shell images, then on the agent images. A cell stops at its
first detection; a detection counts only if the control passes the same schedule and a re-run gives
the same fingerprint. `n/m (k)` = found in n of m repetitions, at the k-th schedule.

| defect | strategy | M108 (original images) | shell (manifest) | agent (manifest) |
|---|---|---|---|---|
| d1-kafka-first | random | 1/1 (2) | 1/1 (2) | 1/1 (2) |
| d5-myisam | random | 1/1 (3) | 1/1 (3) | 1/1 (3) |
| d8-no-order-lock | swarm+cc | 1/1 (5) | 1/1 (5) | 1/1 (5) |
| d12-shared-key-buffer | swarm+cc | 1/1 (5) | 1/1 (5) | 1/1 (5) |
| d10-outcome-in-memory | process+xcf | 1/1 (7) | **0/1** | 1/1 (7) |
| d11-ack-unflushed-flag | process+xcf | 0/1 | 0/1 | 0/1 |

Every detection is the same schedule on every kind: d1 seed 1002 `K=kafka@14+1`; d5 seed 1003
`disk=cut … C=10`; d8 and d12 seed 1005 `K=redis@23+1 N=redis@28-30:corrupt,8%,loss,8% Q=4d`; d10
seed 1007 `K=mysql@20+8 N=kafka@10-11:loss,49% T=8:-30 X=front@8~160`. **Per seed**, the shell and
agent images give the same verdict class (OK, or a violation with or without the defect's
signature) on every seed both ran (4 + 4 + 6 + 6 + 8 + 8 = 36 seeds), except d10's seed 1007: the
shell image ignores its `X=front@8~160` and passes, the agent image kills the order service 160 ms
after order 8 and finds the defect (order 11 twice in Kafka; M108's original image: order 10).

## 5. Three repetitions of M108's cells

Repetitions 2 and 3 of the same cells (seeds 2001… and 3001…), merged with repetition 1 by
`hydo-bench table`:

| defect | strategy | shell (manifest) | agent (manifest) |
|---|---|---|---|
| d1-kafka-first | random, budget 4 | 2/3 (2, 3, -) | 2/3 (2, 3, -) |
| d5-myisam | random, budget 4 | 2/3 (3, 4, -) | 2/3 (3, 4, -) |
| d8-no-order-lock | swarm+cc, budget 6 | 2/3 (5, 2, -) | 2/3 (5, 2, -) |
| d12-shared-key-buffer | swarm+cc, budget 6 | 3/3 (5, 2, 1) | 3/3 (5, 2, 1) |
| d10-outcome-in-memory | process+xcf, budget 8 | 0/3 (-, -, -) | 2/3 (7, -, 4) |
| d11-ack-unflushed-flag | process+xcf, budget 8 | 1/3 (-, -, 7) | 0/3 (-, -, -) |

d1, d5, d8 and d12 are found in the same repetitions, at the same schedule, on both kinds.
Per seed, 96 of the 103 seeds both kinds ran in these 18 repetition cells give the same verdict
class; the seven that differ are explained in section 6.

The process strategy with `--more-faults --cf-range 1-309` on d8, d9 and d12 (repetition 1,
budget 8; cut from three repetitions for time): none of the three is found on either kind, and all
24 seeds give the same verdict class on both kinds (the process strategy draws no `Q=`, which these
defects need).

## 6. Every difference and its classification

1. **`X=` (the stack's timed kill): a designed difference in the shell init, not an agent fault.**
   The generic shell init (`hydo-init.sh`) has no `X=`; it logs `knob X not in the manifest,
   ignored`. The host controller gives `X=` the meaning it has in `stack-explore.sh`
   (hydo-control `HANDOFF.md` decision 12). Everything that `X=` decides differs, and on the agent
   images it decides as on the original benchmark images:
   - d10's trigger `X=front@9~20` fails on the agent image (order 10 twice in Kafka, as on the
     original image) and passes on the shell image;
   - process+xcf repetition 1, seed 1007 (`… X=front@8~160`): the agent image finds d10 at the same
     seed as the original image (M108), the shell image passes; in repetition 3 the agent image
     finds d10 again (seed 3004, item 4 below), the shell image does not;
   - process+xcf repetition 3, seed 3007 (`… X=front@0~25`, a `cf=257` cut): the shell image ignores
     the kill and finds d11 (order 16 acked but not marked published); on the agent image the order
     service is killed 25 ms into order 0, comes back only at the end, and the run takes another
     path (24 orders acked, no d11);
   - process+xcf repetition 3, seed 3003 on both d10 and d11 (`cf=147 … X=front@20~151`): on the
     agent image the killed order service cannot start again after the `cf=` power cut (MySQL's
     sessions hang on the dead disk), and the generic init's restart gives `ERROR restart after
     the faults (front)`; `stack-explore.sh` has a path for this (M67: the unacknowledged orders
     stay unacknowledged and the recovery boot decides), the generic init and the controller do
     not. An error, not a detection, on one kind only.
2. **d9's race: timing.** The trigger `Q=4` crashes the order service on the agent image (the
   original image's panic `slice bounds out of range`) and not on the shell image. Over six `Q=`
   values and three disk seeds (`hydo/bench/c2-plans/d9q.txt`; the disk seed changes nothing,
   every run is deterministic) the crash happens on the shell image for `Q=3` and `Q=4d` (6 of 18
   runs) and on the agent image for `Q=2`, `Q=3`, `Q=4`, `Q=3d` and `Q=4d` (15 of 18). The order
   service, its environment (the controller's spawn gives it the shell's environment: `HOME`,
   `TERM`, the init's exports, `HYDO_SVC`/`HYDO_LOG`, the manifest's `env:` through the prelude)
   and the client are the same; the guests differ in the kernel's console output (`quiet
   loglevel=1` for agent images, M121), in the init's own work, and in where the engine's run
   calls return (`rb=16`, M118). The race is reachable on both kinds; the agent kind reaches it
   with more client settings.
3. **d11's trigger `cf=238`: timing.** It passes on both manifest kinds (the k-th flush is another
   write than on the original image, whose init differs); section 7 maps d11's window on both.
4. **An acknowledged order missing from Kafka after a long Kafka pause, on agent images only: a
   real bug of franz-go v1.17.1 that the agent image's timing reaches (explained in M133,
   below).** d10, repetition 3, seed 3004 (`K=redis@10+1
   P=kafka@16+14`, no `X=`): the agent image gives `VIOLATION kafka order=17 expected=100017
   got=MISSING (mysql has it: true)` (not d10's signature; it counted as a detection because the
   control passes that schedule), the shell image passes. Ten variants of the schedule
   (`d10kp.txt`: the pause moved from order 14 to 18, with and without the Redis kill) on d10 and
   on the **control** (the fixed order service, no planted defect): the shell kind passes all 20
   runs; the agent kind fails two, d10 with seed 3004's schedule again and **the control with
   `P=kafka@14+16`** (`kafka order=15 … MISSING`, `9f3e843a38fc41b7`). In both, the missing order
   is the second order placed after Kafka was stopped, every order is acknowledged, Kafka was
   resumed before the retry pass, and the order service logged a Kafka re-check and a duplicate
   hit for the first order after the pause (`server.kafka_recheck`, `server.kafka_dedup_hit`,
   order 14 in the control's run). A candidate whose schedule fails on the control is not
   counted, so this lowers the agent kind's detections when such a schedule is drawn.

   **Answer (Milestone 133): a real bug in the order service's Kafka client, not a controller
   difference.** The order service uses franz-go v1.17.1 (`hydo/stack/go.mod`; `go version -m`
   of the control image's `/bin/hydostack` says the same), whose idempotent producer can
   acknowledge a record it never wrote. franz-go fixed it in v1.22.2 (issue 1484): "an idempotent
   producer could ack a record without writing it, if a context, `RecordDeliveryTimeout`, or
   `RecordRetries` failed a batch the broker had already appended and the next batch had the
   same number of records". The agent image's timing reaches that window; the shell image's
   timing does not.

   *The evidence.* A diagnostic copy of each control image (`hydo/bench/kafka-diag.sh`: one more
   final invariant that always passes and prints Kafka's record batches with their producer id,
   epoch and sequence numbers, MySQL's `published` flags, the client's outcomes and the workload
   log) gives, for `P=kafka@14+16`:

   | | shell image (OK) | agent image (order 15 MISSING) |
   |---|---|---|
   | client outcomes | 14–27 `unknown` (20 s deadline), every order acked by the retry pass, 16 retry attempts | the same, 17 retry attempts; the first retry of order 14 failed with `kafka check: failed to dial: failed to open connection to :0` (the partition had no leader just after Kafka was continued) |
   | MySQL | every order `published=1` | every order `published=1` |
   | Kafka log, producer 0, epoch 0 | sequence 14 = order 14, 15 = 15, 16 = 16, 17 = 17, 18–28 = orders 18–28 (one batch), 29 = 29; the records of 14–28 carry the create times of their first attempts (made during the pause, appended when Kafka was continued) | sequence 14 = order 14 (its first attempt's record, created at the pause), then **15 = order 16**, 16 = 17, …, 28 = 29; orders 16–29 are the retry pass's records (created after the continue); **no record of order 15** |

   The mechanism, in franz-go 1.17.1 (`pkg/kgo/sink.go`): order 14's batch (sequence 14) was sent
   before the pause and could not be answered. When Kafka was continued, it appended that batch,
   but the answer never reached the producer (its connection had timed out), and for a moment the
   broker answered produce requests with a retriable error (the partition had no leader, which the
   retry's lookup saw). A response for a batch makes it failable again
   (`canFailFromLoadErrs = true`); the retriable error sends it to `handleRetryBatches` with
   `canFail`, and `maybeFailErr` fails it because its records are older than
   `RecordDeliveryTimeout` (10 s): `failAllRecords` fails order 14's batch and every batch behind
   it and rewinds the partition's sequence to `batch0Seq` = 14, with the same producer id and
   epoch. The retry pass then works through the orders in id order: order 14's retry finds its
   record (`server.kafka_dedup_hit`) and produces nothing; order 15's retry finds no record
   (correctly) and produces one record, which gets sequence 14 again. The broker sees producer 0,
   epoch 0, sequence 14, the sequence of the batch it already has, treats the new record as a
   re-send of that batch, answers success with the old batch's offset, and stores nothing. The
   order service marks order 15 published and acknowledges it. Every later record is one
   sequence number behind, which is why the log shows 16 under sequence 15. The order service's
   own logic is right at every step (the lookup before re-producing, `published`, the per-order
   lock); it trusts the producer's success, and the producer's success is false.

   *The checks.* The same control order service rebuilt from `hydo/bench/base/main-m59-control.go`
   with franz-go v1.17.1 (Go 1.27.3, not the image's Go 1.26.5) and swapped into a copy of the
   agent image fails the same way (order 15 MISSING, `c5efd57df3c63b99`); rebuilt with franz-go
   **v1.22.2** (the only change in `go.mod`), it passes (`OK acked=30/30`, `e2a8db82c5229e5d`; the
   retry pass again hits `server.kafka_recheck` and `server.kafka_dedup_hit` for order 14).
   The controller sends the pause and the continue where the shell init does (transcript:
   `signal STOP` before the step range 14..29, which times out after 300 s on both kinds, then
   `signal CONT`, then the retry; `restart_dead` restarts nothing on either kind).

   *The minimal schedule* is `disk=none dseed=0 | run P=kafka@14+16` on the agent control image
   (`c4a120cca428aa48`, order 15 MISSING; the disk seed plays no part). The same pause for 1, 2,
   4 and 8 orders, and `P=kafka@28+2`, pass (`hydo/bench/c2-plans/kp-min.txt`): the window needs
   the step range to end in the workload's 300 s timeout. *On shell images* the window was not
   reached with a timing token: `P=kafka@14+16` with tick jitter (`tj=1`…`8`, `tjpm=100`/`200`,
   `kp-tj.txt`) passes 8 of 8, and so do a shorter tick period (`pp=524288`, `pp=262144`), inserted ticks (`xt=1 xtr=4 xtpm=100`, `xt=2 xtr=4 xtpm=200`) and jitter with the pause moved to orders 13 and 12 (`kp-pp.txt`, 6 of 6 OK). The original stack-explore control image
   (`explore-bench-m59-control-pin-kr`, `stack-explore.sh`'s init, which has no 300 s step-range timeout): OK acked=30/30 `fdf5277af2820c13`.

   *What to do* (a user decision, AGENTS.md open item 35): upgrade franz-go to v1.22.2 or later in
   `hydo/stack/go.mod`. That changes the order service's bytes, and so every stack and benchmark
   image and their fingerprints, and the images are built on the build machine. Until then, a
   schedule that pauses Kafka across a whole timed-out step range can fail on the control of
   agent images, and hydo-bench rejects it as a control failure, as it does for README's
   "A bug in the fixed front door".
5. **Lying disks: different losses, not detections.** d12 swarm repetition 2 seed 2003
   (`disk=lying:5 … C=4 R=no`: shell OK, agent `kafka order=1 …`) and d5 random repetition 3 seed
   3001 (`disk=lying:5 … C=5 M=2`: shell `mysql order=0 … MISSING`, agent `service cannot recover`).
   A lying disk drops requests by index, and the two kinds' guests make different disk requests
   (as M126 saw on Redis). Both schedules are of the `lying-disk` class, which hydo-bench does not
   count as a detection.

The fix found on the way: `hydo-explore` gave every worker an empty start error for an agent image
explored without a manifest space; it now drives such an image through the controller or refuses
it with the controller's message (section 1). After M128's fixes were merged (guest programs'
`HYDO-ASSERT`/`HYDO-HIST` lines now reach an agent run's report), every agent run of this
campaign was checked for assertion records with `ok:false` that its report lacked: 44 runs had
such records, every one of them with a VIOLATION verdict already, so no verdict or detection here
changes (hydo-bench turns an assertion failure into a violation only when the verdict is OK). M108's process cell (16 runs) and the m59 confirmations (10 records) were run again on the agent
kind with the merged binaries: every fingerprint and verdict is the one recorded before.

## 7. The triggers that moved, measured on both kinds (`hydo-bench list`)

| list | shell (manifest) | agent (manifest) |
|---|---|---|
| d9, `Q=2,3,4,2d,3d,4d` × disk seeds 0–2 (`d9q.txt`, 18 runs) | crash (`front door crashed: panic …`) for `Q=3` and `Q=4d`: 6 of 18 | crash for `Q=2,3,4,3d,4d`: 15 of 18 |
| d11 and the control, `disk=cut dseed=0 cf=150…400` step 5 (`d11cuts.txt`, M72's sweep, 51 points) | d11 fails at 3 points (k=255 order 24, k=290 order 27, k=300 order 28); the control passes all 51 | d11 passes all 51; the control passes all 51 |
| d11, `cf=245…305` step 1 (`d11fine.txt`, 61 points) | fails at 8 points: 255 (order 24), 268, 269 (25), 278, 279 (26), 290 (27), 300, 301 (28) | fails at 9 points: 246, 247 (order 23), 257 (24), 267 (25), 279 (26), 288, 289 (27), 302, 303 (28) |
| d10 and the control, seed 3004's schedule with the Kafka pause moved (`d10kp.txt`, 10 schedules) | all 20 runs OK | d10 fails `K=redis@10+1 P=kafka@16+14` (order 17 MISSING in Kafka); the **control** fails `P=kafka@14+16` (order 15 MISSING); the other 18 OK |

d11's window has the same size on both kinds (about one cut point in seven between orders 23 and
28: the cut must fall between an order's acknowledgement and the flush of its `published` flag),
but its cut points are a few flushes apart, so a sweep with step 5 hit it on the shell kind only.
The flush counts themselves agree: on the control, at each of the 51 cut points both kinds had
acknowledged the same number of orders within one (41 points equal; 24 at k=250, 30 from k=325 on).

## 8. What C3 still needs for the benchmark

1. **A way to run the stack's own images under the agent** (section 1): a controller for
   `stack-explore.sh`'s init, or the benchmark, the pinned reproducers' images and the stack's
   explorer images rebuilt from a manifest on the build machine (this milestone's manifest is a
   start; it lacks `stack-explore.sh`'s M67 path, item 3). Until then hydo-bench, the pinned
   reproducers (`explore-disk.img`, `explore-fix-disk.img`) and `deploy-metal.sh`'s stack checks
   stay on shell images.
2. **The agent-only lost order after a long Kafka pause** (section 6, item 4): explained in
   M133 as franz-go v1.17.1's issue 1484 (an idempotent producer acknowledges a record it never
   wrote after failing a batch the broker had appended), fixed in franz-go v1.22.2. What is
   left is the user's decision to upgrade the order service's franz-go, which moves every stack
   and benchmark fingerprint (AGENTS.md open item 34).
3. **`X=` on the manifest form**: the controller gives it the stack's meaning, the shell init
   ignores it; when the shell path is retired that difference disappears, but the generic init's
   restart after a `cf=` cut (M67's case) still ends such a run with an error instead of the
   recovery boot's verdict.
4. **Re-baselining**: the benchmark's recorded triggers moved on the manifest images (d9 needs
   `Q=3` or `Q=4d` on the shell kind; d11's `cf=238` passes on both, its window is at k≈246–303;
   d10's `X=` trigger works on the agent kind only), so the trigger table of `hydo-bench confirm`
   needs new values for agent images, recorded twice each, as M72 did for `-pin-kr`.

Status after Milestone 135 (section 9): item 1 is done for hydo-bench (the manifest images are
its default; the pinned reproducers and `deploy-metal.sh` stay on the stack images as the engine's
baselines); item 2 was M133's (franz-go issue 1484), and the order services of the manifest images
now use franz-go v1.22.2; item 3's M67 path is in the manifest; item 4 is done for the agent kind.

## 9. Milestone 135: the manifest images are the benchmark's images

**What changed.** hydo-bench runs the manifest-built agent images by default (`--kind agent`),
the shell images with `--kind shell`, the stack images with `--kind stack`; the images live in one
directory as `explore-ob-bench-<stem>[-agent]-disk.img` (`hydo/bench/README.md` "The manifest
images (Milestone 135)"). Every order service is built from source with franz-go v1.22.2
(`hydo/bench/build-order-services.sh`); the stack images keep v1.17.1 (`hydo/stack/go.mod.baseline`).
The images carry hydo-agent 0.2.0. A swapped image is now byte-reproducible
(`agent-patch.sh --swap` keeps the image's inode times and fixes the superblock's write time; two
swaps of the same order service gave the same bytes, and `nf` and `fix`, the same binary, give the
same images).

**The M67 path.** In the manifest (`/etc/hydo-bench/front.sh`, `ready:`, `retry:` and the
invariant `front.restarted`; the README has the details). M129's ERROR schedule, process seed 3003
`disk=cut dseed=806555b3e1262cc5 cf=147 | run P=redis@10+3 T=26:-3600 X=front@20~151`, on the
agent control: the killed order service exits on its restart (status 1), the script stays up and
says so, the retry pass is skipped, and the recovery boot gives `OK invariants=3 held (… acked=11/30
…)`, twice `91ebd4a2e0731975` (M129: `ERROR restart after the faults (front)`).

**The franz-go upgrade (user decision, 2026-11-01).** `disk=none dseed=0 | run P=kafka@14+16`,
which lost order 15 from Kafka on M129's agent control (section 6, item 4), passes on the new agent
control, twice `a41a29598b2738ea` (`acked=31/30`: order 29 is in the acknowledged list twice; the
step range 14..29 hit the workload's 300 s timeout, and both that range's client, left running,
and the retry pass recorded order 29; the oracle counts lines, so `not-acked` shows -1; the
verdict is OK because every order is in MySQL and Kafka exactly once).

**The triggers re-recorded on the agent images** (`hydo-bench confirm --kind agent`, all thirteen
defects with `--base`; the table is in `hydo/bench/README.md` "The triggers on the manifest
images"): all 16 triggers fail twice with one fingerprint and the defect's signature, every control
passes them, and the control's known bug (order 26 twice) remains. The moved ones: d9 crashes for
`Q=3` and `Q=4d` (not `Q=4`) on the agent kind, so its trigger is `Q=3`; d10's `X=front@9~20`
still fails; d11's window is 9 of 61 cut points in `cf=245..305` (249, 250, 260, 271, 272, 282,
292, 302, 303), and its trigger is `cf=260` (order 24).


On the shell kind (`--kind shell`) every trigger but d10's is confirmed the same way (d9 `Q=3`,
d11 `cf=255`; the shell kind's d11 window is 8 of 61 points: 245, 255, 256, 266, 277, 287, 297,
298); d10's `X=` trigger passes there because the generic shell init ignores `X=`. M108's cells
were not run again on these images.
