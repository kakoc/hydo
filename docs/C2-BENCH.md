# C2: the bug benchmark on shell images and agent images (Milestone 129)

Step C2 of `docs/design/guest-control.md` asks whether a strategy finds each planted defect of the
bug benchmark (`hydo/bench`, defects d0–d12) as often on agent images as on shell images. This
file holds the answer for the bare-metal test machine (hvx). The criterion is a strategy's
detection rate, not single verdicts, because a change of guest timing changes which schedule
finds what.

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
