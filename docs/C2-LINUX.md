# C2 on Linux: verdict equivalence of agent images and shell images (Milestone 127)

Step C2 of `docs/design/guest-control.md` for the x86_64 engine (hvx): for each example, an
image whose first process is `hydo-agent` (driven by the host controller, `hydo-control`) is
compared with the image whose first process is the shell init (`hydo-init.sh` with its spliced
extensions). Fingerprints cannot be equal (another `/sbin/init`, another guest execution), so the
comparison is at the level of meaning: for every explorer schedule the verdict class (OK,
VIOLATION, ERROR) and the violated properties (the failing findings' ids), and for every
exploration the classes of the shrunk failures. The Mac examples (M128) and the bug benchmark
(M129) are other milestones.

All runs: the bare-metal machine, the main `hvx.ko` (srcversion
`0799F1B8EE2A35600D94EB4`, not reloaded), `hvx_explore` and `hydo-explore` built from this branch
in the tree `~/hydo-c2l`, every VM through `~/bin/hvx-run`, `HYDO_QUIET_FILE=/var/tmp/hydo-QUIET`
and a `HYDO_EXPLORE_CACHE` per exploration. Until the bare-metal machine ran out of memory on 2026-11-01 (another
session's seven stack-sized VMs) at most three VMs at a time; after it at most two, an image over
1 GiB with one worker and only with 6 GiB available. Results are in `/var/tmp/m127` on the bare-metal machine (one
directory per exploration; `cmp/` holds the comparisons, `cmp/table.txt` the rows below).

How a comparison is read (`cmp.py`, `table.py`, `ident.py`; measurement scripts, not in the
repository): the explorer's first runs of the same seed (or sweep point) on both images; the
schedules are the same strings on both images in every exploration below (the fault space is read
from the image's manifest, which both carry). **same** = the same verdict class and the same set of
failing properties. A difference is counted under **disk index** when the schedule has
`disk=lying:` or `disk=ioerr:`, and listed one by one otherwise. **Twice identical**: a second
exploration of the agent image, seeds 1–30, gives every run (first runs, re-runs, shrinks) the same
schedule, `fp`, `wfp`, `ctl` and verdict.

## How the agent images were made

There are three kinds of examples, by what their images need.

1. **Built here, both kinds (tier A).** The Redis family and the Docker voting app need no Nix:
   both images were built by this branch's `hydo build` with the Linux release's tools (M103's
   recipe: busybox, `hvx_seedrng`, `hydo_fc`, mke2fs and debugfs under the release's faketime,
   the bundled guest store), once as a shell image and once with `--control agent` (the pinned
   agent `ff7652a1…`), from the same inputs. The shell images are the recorded bytes of M103
   (`redis` `c6a09428…`, `hydo-netem` `3d9ba33c…`, `hydo-freeze` `8aa6bf16…`, `hydo-hist`
   `2621d3ce…`, `hydo-live` `84b5a929…`, `hydo-proxy` `ad8c5ce8…`, `redis-composed-hist`
   `cfead7dd…`, `redis-errors` `ddb995ea…`, `redis-bridge` `ab6847df…`, `compose-voting`
   `e4bcbcf8…`), and the Redis agent image is M121's `53f09ccc…`.
2. **Built here in M112's chroot (tier B).** The four-service stack (`stack/hydo.yaml` and
   `stack/hydo-read.yaml`, old and fixed order service) and miniflux need the MySQL, JDK, Kafka and
   PostgreSQL closures. M112 made a release-shaped root on the bare-metal machine with those closures taken out
   of recorded images (`~/hydo-prd`); both kinds were built in that chroot with this branch's
   `hydo build` (as uid 1000, gid 100, `HYDO_KEEP_OWNERS=1`, as M112 did).
3. **Patched from the recorded shell image (tier C).** etcd, the shop, the gRPC stack, the
   generated-driver stack, the error-log stack, rust-kv, avx2, the OCI examples and the user's
   workflow engine need inputs only the build machine can stage. Their agent images were made
   from the recorded shell images by `hv/hvx/images/patch-agent.sh`.

### The patch, and its proof against a real build

`hydo-build --control agent` differs from a shell build in three files only (`x86.rs`,
`agent.rs`, `init.rs` `install_init`): `/sbin/init` is the pinned agent instead of the shell init,
`/etc/hydo/control` (`agent 1`) is added, and `/etc/hydo/init-links.sh` (images of manifests with
`links:`) is not staged. `patch-agent.sh` makes exactly these changes with debugfs on the root
partition (offset 1 MiB): the files with hydo-build's owners, modes and times (mtime
`FIXED_MTIME`, atime, ctime and crtime 0), under the release's faketime (hydo-build's frozen clock,
then one pass with the clock at 0, so that a removed inode's deletion time is not 0 and the
superblock's write time is 0 as in a built image). The data partition is not touched; the result
must pass `e2fsck -fn`; the agent's sha256 must be the pin.

Proof on the Redis example (shell `c6a09428…`, built agent image `53f09ccc…`):

- **Bytes.** Two patches of one shell image give the same bytes (each tier-C image was patched
  twice). The patched image is not the built image's bytes, for three reasons: the inode numbers
  (mke2fs gives `/etc/hydo/control` the inode after `/etc/hydo` in its walk order, 419, and every
  later entry one more; the patch gives the new file the next free inode, 433), the blocks the
  agent occupies, and the superblock's lifetime-writes counter (7,121 kB built, 7,141 kB patched).
- **Trees.** Both root partitions mounted read-only and compared entry by entry (M112's
  `cmptree.sh`): the same 424 entries with the same names, link counts, modes, owners, sizes,
  symlink targets and contents (`diff -r` empty); only the inode numbers above differ. The data
  partitions are the same bytes (`2d52cd04…`, as the shell image's).
- **Runs.** The swarm strategy, seeds 1–30, on the patched image against the built agent image:
  the same verdict text for all 56 runs (30 first runs, re-runs and shrinks), including the
  lying-disk and I/O-error schedules (so the disk requests are the same), and the same control
  hash (`ctl=`, the same commands at the same stops) for 45 of the 56; the fingerprints differ
  (the guest's RAM holds other inode and block numbers).

So a patched image stands for a built agent image at the level of meaning, which is the level of
this campaign; its fingerprints are its own and are not baselines (C3 builds the real images).

## Which agent and controller the numbers are from

The campaign found four bugs on the way (section "Fixes"), so not every row ran with the same
build. Every row below ran with the fixes that could change it:

- Every agent image built or patched before the last fix carries the agent of its time: M117's
  0.1.0 (`ff7652a1…`) for the Redis family, the voting app, the stack, the shop, miniflux,
  rust-kv, avx2 and the OCI examples. Those images have the signal-mask bug (fix 3 below), which
  only a workload with a shell `wait` for a background job meets; of the examples only etcd's
  workload has one, and the etcd rows ran on images patched with the fixed agent (`b72c5caa…`).
- Rows with guest-program records (the stack, the shop, rust-kv, miniflux, etcd, the history
  examples) ran after M128's console fix was merged (2026-11-01 03:19 box time); the stack and
  shop agent explorations that ran before it were run again and their old results set aside
  (`/var/tmp/m127/old-controller*`).
- The composed history example's agent row ran after fix 4; etcd's agent rows after fix 3.
- The redis-bridge freeze rows ran on images with M131's agent (0.2.0, `1f517e04…`), and the
  Redis row "mount wording" on the same: these are the rows the lead marked "pending: netns
  freeze" and "pending: mount wording".

## Results, one table per example

Columns: schedules compared (first runs); verdicts shell and agent as ok/violation/error; **same**
(class and failing properties); differences on request-indexed disk faults; other differences
(each listed under the table); failures found and reproduced (shell, agent); shrink classes;
twice identical (agent seeds 1–30, every run).

### Redis (`examples/redis`, tier A)

| exploration | n | shell | agent | same | disk index | other | failures sh / ag | shrink classes | twice identical |
|---|---|---|---|---|---|---|---|---|---|
| swarm 1–100 | 100 | 62/25/13 | 60/24/16 | 78 | 22 | 0 | 38/38, 40/40 | lying-disk, unsafe-knob (both) | 163 of 163 runs |
| swarm 1–100, M131 agent | 100 | 62/25/13 | 60/24/16 | 79 | 21 | 0 | — | lying-disk, unsafe-knob (both) | — |
| process `--more-faults` 1–100 | 100 | 100/0/0 | 100/0/0 | 100 | 0 | 0 | 0, 0 | — | — |
| `--sweep cut` | 20 | 20/0/0 | 20/0/0 | 20 | 0 | 0 | 0, 0 | — | — |

- The minimal schedules: shell `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` (unsafe-knob)
  and three lying-disk minima; agent the same unsafe-knob minimum and three lying-disk minima at
  other seeds. M126's open question (the shell image's two lying-disk signatures, "data disk
  unmountable" and "service cannot recover", missing on the agent image in seeds 1–20) is
  answered: with seeds 1–100 the agent image finds both (seed 49 unmountable, seed 97 cannot
  recover), at other seeds than the shell image (4, 19 and 8).
- **Disk index (all 22 differences).** A lying or I/O-error disk decides per request by the
  request's index (`hvx_abi.h`: one roll per virtio-blk request). The two kinds of image do not
  issue the same request stream: with the engine's phase counters (`HVX_EXPLORE_PHASES=1`) the
  workload phase of seeds 1–20 made 0–111 requests on the shell image and 0–130 on the agent
  image, different for 15 of the 20 seeds (seed 5: 0 against 17). So a seed's lying or failing
  requests hit other writes; the detection rates match (violations 25/24, errors 13/16 of 100).
  The patched agent image (the same stream as the built one, see above) gave the built agent
  image's verdict on every lying-disk seed of 1–30, which confirms that the stream belongs to the
  guest program, not to chance. Classification: **request order** (like timing; recorded).
- **Mount wording (pending: mount wording).** With M131's busybox text, seed 49's verdict is
  "data disk unmountable (mount: mounting /dev/vda2 on /data failed: Data consistency error)" on
  the agent image, the shell image's words; before, "(mount /dev/vda2 on /data: errno 117)". One
  explorer signature for both kinds now. Between the agent with that fix and M127's earlier agent
  run, the only other verdict that changed is lying-disk seed 97 (request order again: the agent
  binary's size changes the root partition's reads).
- Transcript replay (`hydo repro --transcript`, a fresh boot, the recorded commands at the
  recorded stops) gave the explorer's fingerprint and "the same commands at the same stops" for
  seeds 1, 2, 5, 8, 9, 11 and 12 (lying disk with kill and cut, cut with `R=no`, `R=everysec`,
  I/O error with kill).

### Redis variants (tier A)

| example / exploration | n | shell | agent | same | disk index | other | failures | shrink classes sh / ag | twice identical |
|---|---|---|---|---|---|---|---|---|---|
| `hydo-netem` swarm 1–100 | 100 | 57/32/11 | 50/33/17 | 70 | 25 | 5 | 43/43, 50/50 | durable-defaults, io-errors, lying-disk, unsafe-knob (both) | 41 of 41 |
| `hydo-netem` process `--new-faults` 1–100 | 100 | 67/33/0 | 68/32/0 | 99 | 0 | 1 | 33/33, 32/32 | durable-defaults (both) | — |
| `hydo-freeze` swarm 1–100 | 100 | 61/26/13 | 59/25/16 | 77 | 23 | 0 | 39/39, 41/41 | + io-errors / lying-disk, unsafe-knob | 57 of 57 |
| `hydo-freeze` `--sweep freeze` | 40 | 40/0/0 | 40/0/0 | 40 | 0 | 0 | 0, 0 | — | — |
| `hydo-freeze` `--sweep freeze --fc-adaptive` | 40 | 40/0/0 | 40/0/0 | 40 | 0 | 0 | 0, 0 | — | — |
| `hydo-hist` swarm 1–100 | 100 | 50/31/19 | 52/29/19 | 84 | 16 | 0 | 50/50, 48/48 | lying-disk, unsafe-knob (both) | 65 of 65 |
| `hydo-live` swarm 1–100 | 100 | 56/31/13 | 58/26/16 | 75 | 24 | 1 | 44/44, 42/42 | durable-defaults, io-errors, lying-disk, unsafe-knob / io-errors, lying-disk, unsafe-knob | 56 of 56 |
| `hydo-proxy` process `--protocol-faults` 1–100 | 100 | 100/0/0 | 100/0/0 | 100 | 0 | 0 | 0, 0 | — | 30 of 30 |
| `hydo-proxy` `--sweep protocol` | 60 | 60/0/0 | 60/0/0 | 60 | 0 | 0 | 0, 0 | — | — |
| `redis-ram` swarm 1–100 | 100 | 62/25/13 | 59/25/16 | 77 | 23 | 0 | 38/38, 41/41 | lying-disk, unsafe-knob (both) | — |
| `redis-bridge` swarm 1–100 | 100 | 100/0/0 | 100/0/0 | 100 | 0 | 0 | 0, 0 | — | 30 of 30 |
| `redis-bridge` `--sweep freeze` (M131 agent; pending: netns freeze) | 18 common points | 18 violations | 18 violations | 18 | 0 | 0 | 25/25, 25/25 | durable-defaults: the same minimum `F=replica<primary@0#1:75000` | — |
| `redis-oci` swarm 1–50 | 50 | 26/14/10 | 26/13/11 | 45 | 5 | 0 | 24/24, 24/24 | lying-disk, unsafe-knob (both) | — |

Other differences, one by one:

- `hydo-netem` swarm seeds 31, 54, 85, 99 (`N=` with loss, duplicate or corrupt for one or two
  steps): the shell image saw no retransmission, the agent image one or two
  (`net.no_retransmission`). netem's loss is drawn from the guest kernel's random state, which
  differs between the two guests, and the fault window is one SET. Over all 53 `N=` seeds the
  property fails on 7 shell and 9 agent runs. **Timing (guest randomness).** Seed 9 (`K=` + `C=4`
  + `R=everysec`): shell "service cannot recover", agent "key k:1 lost": the cut lands at another
  point of the AOF write. **Timing.**
- `hydo-netem` process seed 84 (`N=…loss,24%` with a pause, a kill and a clock step): the
  reverse, shell 3 retransmissions, agent none. **Timing.**
- `hydo-live` seed 99 (`K=redis@3+3 R=everysec`): the shell image lost k:2, the agent image kept
  every write. With `everysec` a kill can land before or after the fsync. **Timing.**
- `hydo-freeze` sweeps: all 40 points fired on both images (40 consoles with a FROZE line each,
  the trace runs "never fired"), at the same packet with the same payload; the FROZE line names
  the handle (`redis`) where hydo_fc names a pid (M124 decision 8). The redis-bridge sweep's
  point lists differ in 7 of 25 points (the shell trace saw two packets at step 9, the agent
  trace one, and two at step 15): the replication stream's segmentation is timing; every point
  of both lists violates `replica.catches_up`, as the freeze of a replica for 150 s must.
- Transcript replay REPRODUCED: freeze sweep points 5 and 25, proxy seeds 2 and 4, netem re-runs
  31 and 54, redis-bridge freeze points 1 and 5 (M131 agent).

### Composed plans and error logs (tier A)

| example / exploration | n | shell | agent | same | disk index | other | failures | shrink classes sh / ag | twice identical |
|---|---|---|---|---|---|---|---|---|---|
| `redis-composed` swarm 1–100 | 100 | 53/24/23 | 52/26/22 | 94 | 5 | 1 | 47/47, 48/48 | io-errors, lying-disk / + unsafe-knob | 174 of 174 |
| `redis-composed` process 1–100 | 100 | 100/0/0 | 100/0/0 | 100 | 0 | 0 | 0, 0 | — | — |
| `redis-composed-hist` swarm 1–100 (after fix 4) | 100 | 57/31/12 | 59/27/14 | 72 | 27 | 1 | 43/43, 41/41 | lying-disk, unsafe-knob (both) | — |
| `redis-composed-read` swarm 1–100 | 100 | 53/25/22 | 53/24/23 | 93 | 6 | 1 | 47/47, 47/47 | io-errors, lying-disk / + unsafe-knob | — |
| `redis-errors` swarm 1–100 | 100 | 53/25/22 | 52/26/22 | 92 | 7 | 1 | 47/47, 48/48 | io-errors, lying-disk / + unsafe-knob | 499 of 499 |

- Seed 69 of the three composed plans (`C=1 R=everysec W=d0bd4237,p3,q2,r200`): on the shell
  image the cut at step 1 came before the composer's first round ("no composed workload ran"), on
  the agent image one SET ran and was lost (`acked_writes_survive`), which makes the agent's
  extra shrink class `unsafe-knob`. The host starts a round with one sleep where the shell's
  daemon polls every 10 ms (HANDOFF M123 decisions 2 and 11). **Timing, from a recorded controller
  decision.**
- `redis-composed-hist` seed 52: both VIOLATION, the shell image's first failing check is
  `redis.kv_linearizable`, the agent image's `redis.lists_append` (which ops the cut catches).
  **Timing.** Before fix 4 this example gave 31 history violations on the shell image and 6 on
  the agent image: the agent image ran no history check at all (fix 4).

### The four-service stack (tier B)

| example / exploration | n | shell | agent | same | disk index | other | failures | shrink classes | twice identical |
|---|---|---|---|---|---|---|---|---|---|
| `stack/hydo.yaml`, old order service, swarm 1–100 | 100 | 41/35/24 | 40/36/24 | 81 | 18 | 1 | 59/59, 60/60 | durable-defaults, lying-disk, unsafe-knob (both) | 120 of 120 |
| `stack/hydo.yaml`, fixed order service, swarm 1–100 | 100 | 46/30/24 | 45/31/24 | 73 | 23 | 4 | 54/54, 55/55 | lying-disk, unsafe-knob (both) | — |

- **The pinned reproducer** `disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24` (`hydo
  run`, a fresh boot): old order service, shell VIOLATION `[stack.exactly_once] order=11 is in
  kafka (1 times) but the client was never ACKed` (`4b41f4f833b19154`), agent the same verdict
  twice (`0b993f6a05821a85`, `ctl=7c7b997bd2a7d455`); fixed order service, shell OK
  (`f4eb8fdad0e2dd6a`), agent OK twice (`e094cc6c90fd5ac7`). With `stack/hydo-read.yaml` (the
  store readers): old shell VIOLATION `[order.told_failed_not_applied] disjoint violated: 1 item
  of told_failed is also in kafka: 11` (`d23bd7583ae420b8`), agent the same twice
  (`6189683ba3c41a50`); fixed shell OK 7 held (`e5f566ef90cadfa9`), agent OK twice
  (`c74816d17c7abe54`). The clean schedule `disk=none dseed=1 | run` is OK on all, each agent run
  twice identical. Transcript replay REPRODUCED for the old and fixed reproducer runs and the
  stack-read old one.
- Old seed 69 (`C=21 R=everysec`): shell "Redis cannot recover", agent OK. **Timing** (the cut
  against Redis's AOF write).
- Fixed seeds 5, 9, 38, 39 (a cut with MySQL's relaxed knob `M=0`/`M=2`): both images violate
  `stack.exactly_once`; the shell image loses order 0's "published" mark (and its oracle's
  assertions `order.acked_marked_published`, `order.in_mysql` fail), the agent image loses the
  last orders' rows. With `innodb_flush_log_at_trx_commit` 0 or 2 MySQL flushes its log once a
  second, so what a cut loses depends on how much guest time passed since the last flush. Same
  property, other rows. **Timing.**
- Before M128's console fix the agent image's runs had no assertion findings at all (the order
  service's oracle writes its `HYDO-ASSERT` records to the serial port itself); the rows above
  are from after it (shell 108, agent 101 runs with assertion findings in the old service's
  exploration).

### The shop and the gRPC stack (tier C, patched)

| example / exploration | n | shell | agent | same | disk index | other | failures | shrink classes | twice identical |
|---|---|---|---|---|---|---|---|---|---|
| shop, planted bug, process `--protocol-faults` 1–100 | 100 | 96/4/0 | 96/4/0 | 100 | 0 | 0 | 4/4, 4/4 | durable-defaults (both) | 30 of 30 |
| shop, fixed, process `--protocol-faults` 1–100 | 100 | 100/0/0 | 100/0/0 | 100 | 0 | 0 | 0, 0 | — | — |
| shop, planted bug, `--sweep protocol` | 228 | 180/48/0 | 180/48/0 | 228 | 0 | 0 | 48/48, 48/48 | durable-defaults (both) | — |
| shop, fixed, `--sweep protocol` | 228 | 228/0/0 | 228/0/0 | 228 | 0 | 0 | 0, 0 | — | — |
| `stack-grpc`, planted, process `--grpc-faults` 1–20 | 20 | 13/7/0 | 13/6/1 | 18 | 0 | 2 | 7/7, 7/7 | durable-defaults (both) | — |

- The shop matches M89's numbers on both kinds: the planted bug fails exactly the 48 "effect
  without answer" points of the protocol sweep.
- `stack-grpc` seed 14 (`H=orders@10-12:drop,rpc=Place,call=11` with a Kafka loss window): the
  shell image duplicates order 10 in Kafka (the planted bug), the agent image does not: `call=11`
  names the 11th Place call, and which order that is depends on the retries before it. The bug is
  found on both kinds in this exploration (7 failures each). **Timing.**
- `stack-grpc` seed 6 (a cut at the last step with a kill, a pause, a backward clock step and two
  `H=` rules): the agent run ended "TIMEOUT workload" (the engine's wall limit for the workload
  phase), the shell run OK. **Not explained** (no time left to look; listed in the report).

### etcd (tier C, patched; agent with fix 3)

| example / exploration | n | shell | agent | same | disk index | other | failures | shrink classes | twice identical |
|---|---|---|---|---|---|---|---|---|---|
| 3.5.5-nq, process `--new-faults --timing` 1–30 | 30 | 23/7/0 | 22/8/0 | 27 | 0 | 3 | 7/7, 8/8 | durable-defaults (both) | — |
| 3.5.6-nq, process `--new-faults --timing` 1–50 | 50 | 50/0/0 | 49/0/1 | 49 | 0 | 1 | 0/0, 1/1 | — | — |
| `hydo-hist` 3.5.5 (`-hc`), swarm 1–36 | 36 | 17/10/9 | 18/10/8 | 30 | 6 | 0 | 19/19, 18/18 | lying-disk (both) | — |

- **etcd issue 14685 is found on both kinds of 3.5.5** (`etcd.members_agree`: members disagree on
  revisions), 7 of 30 seeds on the shell image and 8 on the agent image, and on neither kind of
  3.5.6. Seeds 6 and 20 violate on the agent image only, seed 9 on the shell image only (a pause
  and a kill against the defrag): which schedules reach the bug is timing; the rate is the same.
- 3.5.6 seed 39 (tick jitter, a link loss window, a pause and a forward clock step of a day): the
  agent run's final range read failed ("invariant could not be evaluated"), the shell run OK.
  **Timing** (a read right after a day's clock step against a member still electing); not
  investigated further.
- Before fix 3 every agent workload step range of these images ran into its 600 s timeout (51 of
  66 consoles of the 3.5.6 exploration, 50 of 75 of the history one), so the history checks saw a
  few operations only (shell 10 history violations in 36, agent 3); those results are in
  `/var/tmp/m127/old-agent-wait`.

### Other examples

| example / exploration | n | shell | agent | same | disk index | other | failures | shrink classes | twice identical |
|---|---|---|---|---|---|---|---|---|---|
| rust-kv (`-v2`, tier C) swarm `--buggify` 1–100 | 100 | 66/28/6 | 67/29/4 | 86 | 13 | 1 | 34/34, 33/33 | lying-disk, unsafe-knob (both) | — |
| avx2 (tier C) swarm 1–50 | 50 | 50/0/0 | 50/0/0 | 50 | 0 | 0 | 0, 0 | — | — |
| miniflux (tier B) swarm `--new-faults` 1–30 | 30 | 16/5/9 | 14/6/10 | 23 | 5 | 2 | 14/14, 16/16 | lying-disk, unsafe-knob (both) | — |

- rust-kv seed 52 (`S=never` with a cut and a loss window): the shell image kept all 16
  acknowledged writes, the agent image lost them. With `S=never` nothing is synced, so what
  survives a cut is what the kernel wrote back by then. **Timing.**
- miniflux seed 9 (`S=off`, a cut at step 4): the agent image lost category c3, the shell image
  kept it. **Timing** (PostgreSQL's `synchronous_commit=off` window). Seed 11 (a kill of the
  front service at step 16 during a 97 % request loss window and a pause): the agent run ended
  "ERROR front restart failed", the shell run OK. The restart's readiness check runs during the
  loss window; the agent's readiness polls are timed by the host (HANDOFF decision 3).
  **Timing, from a recorded controller decision; not proven.**
  violation on the agent image twice (`eba88737fd17c6a8`, 342,273 stops, the agent's packet
  (agent twice `8ddcb88b45d54f93`). Transcript replay of the freeze run REPRODUCED.

## Not run

Stopped by the memory rule (2026-11-01: at most two VMs, a stack-sized one alone) and the time
left; each is listed with what would run it:

- `stack/hydo.yaml` fixed, process `--more-faults` (timed kills and k-th-flush cuts on the stack).
- `stack/hydo-errors.yaml` (error logs on the stack), `stack/hydo-grpcgen.yaml` (`stackgen-rs`),
  `stack/hydo-read.yaml` explorations (only its pinned reproducers ran), `stack-grpc` cur and old.
- The voting app (`compose-voting` hydo, generated, loopback): 27 shell seeds ran before the
  memory stop (`/var/tmp/m127/broken-nohvx/compose-voting_hydo-swarm-shell/cache.jsonl`), no
  agent seeds; a run takes 75 s with one worker.
- miniflux-read, restate-oci, etcd 3.5.6 history (`-hc`), etcd 3.6.x.
  schedule ran.
- The pinned reproducers' own images (`explore-disk.img`, `explore-fix-disk.img`) cannot be
  agent images: their init is `stack-explore.sh` and they carry no manifest (M129's finding); the
  reproducer's schedule ran on the onboarding stack's old and fixed images instead (above).
- D7 (a check killed by its timeout prints "Terminated" in the shell's output): not seen in these
  rows; M134's fix is on main and was not merged into this branch.

## Fixes made in this campaign

1. **The periodic checkers keep the environment of their start** (HANDOFF M122 decision 20; branch
   `c2-linux-fixes`, `db824eb`): `Part::Every` carries the init's exports at start_periodic, as
   the shell's forked subshell does. Test `conversations::a_periodic_check_keeps_the_environment_of_its_start`;
   the shell model checks every periodic command's environment; `corpus-quick.txt` re-blessed for
   the seven manifests with `every:` invariants only.
2. **Error logs together with fakes in the shell's order** (M123 decision 9; `1eb923f`): the
   fakes' summary lines are held back with the assertion records, as hydo-build's splicing order
   gives. Test `fakes::error_logs_with_fakes_print_in_the_shells_order` against
   `hydo-build --print-init` of `tests/manifests/redis-proxy-errors.yaml`.
3. **Every child of the agent starts with an empty signal mask** (`517c900`): the agent blocks
   SIGCHLD for its signalfd and its children inherited the mask, so a busybox shell's `wait` for
   a background job never returned (etcd's `client_op … & wait`). Native test
   `children_start_with_no_blocked_signals`; native transcripts replay byte for byte; PID 1 test
   passes on the bare-metal machine; new pins x86_64 `b72c5caa…`, aarch64 `340a1eec…` (the same bytes from
   the Mac and from Linux), version string still 0.2.0.
4. **History checks of a `hydo/` folder plan on agent images** (`788e573`): the embedded
   `hydo.yaml` does not carry the checks the build expanded from `checks/`; the plan now takes
   them from the image's `manifest.sh`. Test `agent::tests::history_checks_from_manifest_sh`.

Made here and superseded at the merge of main: the `netns` field of the agent's `freeze` and F=
with `network: bridge` (`ed10886`), busybox's mount-error words (`adf823e`) and the guest console
records in the report (`f9a78e1`): M131 and M128 did the same on main, and their versions were
kept (merges `8115694`, `30cb865`).

## What C3 still needs from x86_64

- The rows under "Not run", above all the stack's process strategy, the error-log stack, the
  generated-driver stack and the voting app.
- One explanation each: `stack-grpc` seed 6 ("TIMEOUT workload" on the agent image only) and
  miniflux seed 11 ("front restart failed" on the agent image only).
- Agent images built for real (not patched) for the tier-C examples, which needs the build
  machine; their fingerprints are C3's re-baseline.
- The benchmark's and the pinned reproducers' stack-explore images under the agent (a controller
  for `stack-explore.sh`, or those images rebuilt from a manifest; M129).
