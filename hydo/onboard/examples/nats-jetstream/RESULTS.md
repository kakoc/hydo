# NATS JetStream: results (Milestone 141)

The first third-party project onboarded end to end the way an outside user would: a
`docker-compose.yaml` for a three-node JetStream cluster written as a NATS user writes one, then
only the onboarding skill (`hydo guide`) and the `hydo` command built from main
(`3a9986e`), on the Mac backend (hvm, Apple M2 Pro, `network: bridge` with the second guest
kernel `Image-explore-netns`), agent images only. The hunt targets are the findings of Jepsen's
analysis of NATS 2.12.1.

## What Jepsen reported (and what this example hunts)

Sources: https://jepsen.io/analyses/nats-2.12.1 (December 2025), nats-server issues 6888, 7549,
7556, 7564 and 7567, PRs 5700 and 5946, and Synadia's reply
(https://www.synadia.com/blog/jepsen-nats-2-12-1). Jepsen's harness: 3 or 5 nodes (most findings
on 5), one stream `jepsen-stream` replicated to every node, file storage, the Java client JNATS;
producers publish globally unique values; at the end every node is restored, faults healed, and
the harness reads the stream; a value that was acknowledged and never read is lost.

| # | finding | versions | trigger | fixed |
|---|---|---|---|---|
| 3.1 | the whole stream deleted ("No matching streams for subject", an empty stream list), for good | 2.10.20–2.10.22 | process crashes only (`kill -9` of several nodes; issue 6888, reproduced with `sync_interval` both `always` and default); PR 5946's reproducer: an R3 stream, all three servers hard-killed right after the stream was added, before the meta layer took a snapshot | 2.10.23 (PR 5700, "Don't send meta snapshot when becoming metaleader", backported with other Raft fixes) |
| 3.2 | lost writes after bit flips or truncation of `.blk` files on one or two of five nodes | 2.12.1 | file corruption, no power loss needed | open (issue 7549) |
| 3.3 | the whole stream deleted after snapshot-file corruption on a minority | 2.12.1 | file corruption | open (issue 7556) |
| 3.4 | acknowledged writes lost on power loss: NATS acknowledges before `fsync`, which by default runs every two minutes (`sync_interval`); also the stream deleted by single-node failures a few seconds apart within the first two minutes | 2.12.1 (and every release: it is the default) | LazyFS power loss (unflushed writes dropped), coordinated on all nodes | documented, not changed (issue 7564; Synadia: `sync_interval: always` for strict durability) |
| 3.5 | lost writes and a lasting split brain after one node's OS crash plus pauses or partitions | 2.12.1 (shown with `sync_interval: 10s`) | one node's power loss with process pauses or a minority partition | open (issue 7567) |

Hunted here: 3.1 with its version pair (2.10.22 against 2.10.23), and 3.4 as a configuration
finding (the default `sync_interval` against `sync_interval: always`, on 2.12.1 and 2.10.23). Not
hunted in Milestone 141: 3.2 and 3.3 (hydo had no file-corruption fault), 3.5 (hydo's power cut was
the whole guest's: one data disk, every node at once). Milestone 183 added both faults, and the
section "One node's storage faults (Milestone 235)" below hunts 3.2, 3.3 and 3.5 with them, and the
section "Several nodes' corrupted blocks, and the strict power loss (Milestone 245)" reaches 3.2.

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | three `nats:<version>-alpine` nodes, a shared `nats-server.conf`, a named volume on `/data` per node, Compose healthchecks on `/healthz?js-enabled-only=true`, `restart: unless-stopped` |
| `nats-server.conf`, `nats-server-sync-always.conf` | the server configuration (default fsync, and `sync_interval: always`) |
| `client/` | `natsjs`, the test client (Go, nats.go v1.37.0, static): `create` the stream (3 replicas, file storage), `pub <value>` with the JetStream acknowledgement, `read` every message (ordered consumer) |
| `hydo/hydo.yaml` | from `hydo compose`, hand changes marked `hand:`; `params:` choose the release (`nats`) and the configuration (`conf`) |
| `hydo/workload/parallel_driver_publish` | one publish of a unique value (`$HYDO_CMD_ID`); records ack/failed/unknown; writes an SDK-style `nats.publish_acked` record to the console |
| `hydo/checks/final_acked_readable` | every acknowledged value is readable after the faults; "stream not found" is a violation |
| `hydo/known-errors.yaml` | the route-connection errors every boot prints while the peers start |

Build and run (Mac):

```sh
sh hydo/fetch-images.sh                        # the six images by digest (2.10.22, 2.10.23, 2.12.1)
sh client/build.sh arm64; sh client/build.sh amd64
hydo build hydo/hydo.yaml                      # 2.10.22, default fsync
hydo build hydo/hydo.yaml --set nats=2.10.23
hydo build hydo/hydo.yaml --set nats=2.12.1 --set conf=nats-server-sync-always.conf
hydo run "disk=none | run"
hydo explore --seeds 1-50 --strategy process --new-faults
```

On a Mac without the netns helpers in the release, `HYDO_ARM64_STORE` must name a directory with
the aarch64 `iproute2-6.8.0` and `iptables-1.8.10` store paths (`hydo-nix-fetch --closure`).

## Images (Mac, arm64, agent images)

Built by `hydo build` from this folder (with the commits of Milestone 141); a second build of the
2.10.22 image gave the same bytes. The client was built with Go 1.27.3 (`client/build.sh`):
`natsjs-arm64` `bccebbe8…`, `natsjs-amd64` `21b477ab…`. Initramfs sha256 prefixes (the empty data
disk is `ee82a339…` for all):

| name | release | `sync_interval` | `--set` | initramfs |
|---|---|---|---|---|
| b22 | 2.10.22 | default (2 min) | (none) | `aa2d6f3c…` |
| b23 | 2.10.23 | default | `nats=2.10.23` | `95564015…` |
| b121 | 2.12.1 | default | `nats=2.12.1` | `96c7162a…` |
| b22a | 2.10.22 | `always` | `conf=nats-server-sync-always.conf` | `03d7cd25…` |
| b23a | 2.10.23 | `always` | `nats=2.10.23 conf=…` | `3f00ba2e…` |
| b121a | 2.12.1 | `always` | `nats=2.12.1 conf=…` | `747535a9…` |

Without faults, `disk=none | run` (46 publishes, all acknowledged and read back), twice each:
b22 OK `65534882d775088b` (ctl `552b25dcfdb807a8`), b23 OK `a5535399929a4c74`
(`4b2a1ab1e442b4f7`), b121a OK `fc126b0be53200af` (`be14dbe7d57efda3`).

The manifest also names the amd64 images and the amd64 client, but no x86_64 image was built
(open item 38).

## 3.1, found blind: the stream is deleted (2.10.22), not on 2.10.23

`hydo explore --seeds 1-50 --strategy process --new-faults` (6 workers; `--more-faults` is
x86_64 only), every failure re-run and the first of each signature shrunk and run twice:

| image | failures | signatures | first failing seed | minimal schedule | fingerprints |
|---|---|---|---|---|---|
| b22 (2.10.22) | 14 of 50 (seeds 1 5 6 8 12 17 20 21 26 32 37 40 47 49), all reproduced | 1: `stream lost: the cluster says stream jepsen does not exist` | 1 | `disk=cut dseed=4127cc1347fae6ba \| run C=30 W=:p1,r1000,cparallel_driver_publish,0.0.0.1390654497` (52 shrink runs) | `8c81413d2915448a` twice |
| b23 (2.10.23) | 14 of 50: 13 lose acknowledged messages (seeds 1 5 6 8 12 20 21 26 32 37 40 47 49; 3.4, below), 1 ERROR (seed 17: the read timed out); no stream lost | `lost # of # acknowledged messages` | 1 | `disk=cut dseed=4127cc1347fae6ba \| run C=30 W=:p1,r1000,cparallel_driver_publish,21.15.0.…` (the last ten publishes before the cut; 39 shrink runs) | `990ae554be672a9e` twice |
| b22a (2.10.22, `always`) | 16 of 50, all reproduced | 1: `stream lost` | 1 | the same as b22 | `cc7cca364d1f4fe4` twice |
| b23a (2.10.23, `always`) | 0 of 50 | | | | |

The minimal schedule is one publish, acknowledged, then a power cut 30 rounds (30 s of guest
time) later: on 2.10.22 the recovered cluster answers that the stream does not exist, with the
default fsync and with `sync_interval: always`; on 2.10.23 the message is there. A power cut in
hydo stops every node at once; with `sync_interval: always` every acknowledged write is on the
disk, so on b22a it is the same as Jepsen's (and PR 5946's) hard kill of every server.

Without any power cut, too: on b22a two of the 16 failing schedules have no cut and an honest
disk, only one kill among pauses, one-way delays and a clock step (seed 9: `disk=none
dseed=a16e4eaefdb58ac0 | run K=n1@15+1 P=n1@1+1 O=n2-n3@20-30:resp:delay,2282ms
W=5372f125,p3,q1,r1000`; seed 19: `disk=none dseed=890abaea1c4d92d3 | run K=n1@10+2 P=n3@26+4
T=2:-30 W=f4973749,p4,q1,r500`), so process crashes and pauses alone delete the stream, as
Jepsen found. The explorer shrinks one failure per signature (here seed 1's power cut); these two
were shrunk separately below.

Shrunk by hand (one fault dropped at a time, one run each on b22a; `hydo explore --schedule`
runs a schedule once and does not shrink it): seed 9 needs the kill and the earlier pause of the
same node, seed 19 the kill and the clock step; dropping either passes. Twice each from a fresh
boot on every image:

| schedule | b22 | b23 | b22a | b23a | b121 | b121a |
|---|---|---|---|---|---|---|
| `disk=none dseed=a16e4eaefdb58ac0 \| run K=n1@15+1 P=n1@1+1 W=5372f125,p3,q1,r1000` (n1 paused for one round, killed before round 15) | OK `34b99a46207dec86` | OK `ccb210ad86c14176` | **stream lost** (acked=15) `04d81d362011c8c5` | OK `c8694be8c09a2895` | OK `2887782b8750cffe` | OK `701b7d57aae8bac4` |
| `disk=none dseed=890abaea1c4d92d3 \| run K=n1@10+2 T=2:-30 W=f4973749,p4,q1,r500` (the clock stepped back 30 s, then n1 killed) | OK `8463076ed7981677` | OK `86683f0f5ba9ca24` | **stream lost** (acked=10) `eb73e05b4017a2f2` | OK `5b16db813556c6f1` | OK `ad96d39ae0627f61` | OK `a3496588da3195fd` |

Every run twice identical (fp and ctl). On b22 (2.10.22 with the default fsync) the same two
schedules pass: the window depends on timing, and the fsync setting changes it; the explorer
found 2.10.22's stream loss on b22 through power cuts instead.

The process strategy kills one node at a time (one `K=` per schedule). Killing every node at
once, as in PR 5946's reproducer, was aimed by hand: a reaction
kills all three nodes at the first acknowledged publish (`A=` acts on the record
`nats.publish_acked` that `parallel_driver_publish` writes to the console); they stay down until
the heal restarts them. Each schedule twice from a fresh boot (`hydo run`):

`disk=none | run A=nats.publish_acked:kill-n1 A=nats.publish_acked:kill-n2 A=nats.publish_acked:kill-n3`

| image | verdict | fp | ctl | twice |
|---|---|---|---|---|
| b22 | VIOLATION [acked_readable] stream lost: the cluster says stream jepsen does not exist (acked=1) | `2fb2fcece2249f37` | `517ade98245fc6d4` | yes |
| b23 | OK (acked=2 all present, read 2) | `18cb25822d438e34` | `15cd8ceaf6a53b30` | yes |
| b22a | VIOLATION … stream jepsen does not exist (acked=1) | `c04c0b1080a18b26` | `d32aa4765d7452d3` | yes |
| b23a | OK (acked=1 all present) | `7c46e0bb22451db9` | `74b85cd356426f53` | yes |
| b121 | OK (acked=2 all present) | `2613a070f4303a40` | `b231631b40b2c07c` | yes |
| b121a | OK (acked=2 all present) | `a09b4e7649385fd0` | `07e68f881530c4ea` | yes |

The blind minimal schedule on every image, twice each:

`disk=cut dseed=4127cc1347fae6ba | run C=30 W=:p1,r1000,cparallel_driver_publish,0.0.0.1390654497`

| image | verdict | fp | ctl | twice |
|---|---|---|---|---|
| b22 | VIOLATION … stream jepsen does not exist (acked=1) | `8c81413d2915448a` | `404e1692efa4e163` | yes |
| b23 | OK (acked=1 all present) | `ea8fc1bc39f8c8f0` | `302cf22196331bdc` | yes |
| b22a | VIOLATION … stream jepsen does not exist (acked=1) | `cc7cca364d1f4fe4` | `44f503de96248512` | yes |
| b23a | OK (acked=1 all present) | `1e95862002722d15` | `bff5ca3e537ec171` | yes |
| b121 | OK (acked=1 all present) | `b03d41877150bb65` | `9b1f30b72cc524d8` | yes |
| b121a | OK (acked=1 all present) | `3d2bb0280b663e5c` | `c468e35d3d5dfe7c` | yes |

## 3.4, a configuration finding: the default `sync_interval` loses acknowledged writes

A power cut 12 rounds (6 s of guest time) into the workload, `disk=cut | run C=12`, twice each:

| image | verdict | fp | ctl | twice |
|---|---|---|---|---|
| b22 | VIOLATION … stream jepsen does not exist (acked=20) | `37a597b5fb196ed9` | `317df22854839a47` | yes |
| b23 | VIOLATION [acked_readable] lost 20 of 20 acknowledged messages (read 0; msgs=0 first=0 last=0 leader=n2) | `0bc447f697db9622` | `7e552c449179396a` | yes |
| b22a | VIOLATION … stream jepsen does not exist (acked=20) | `cbff1227fd6d2068` | `df0f6239e6607d5a` | yes |
| b23a | OK (acked=20 all present, read 20) | `3df83c4ee073946d` | `d8357f59d0eb2392` | yes |
| b121 | VIOLATION [acked_readable] lost 20 of 20 acknowledged messages (read 0; msgs=0 first=0 last=0 leader=n2) | `bdc72936b4501190` | `d1291f21740a5af6` | yes |
| b121a | OK (acked=20 all present, read 20) | `f43c79941cec2bbd` | `d19a20d3e6ac8b7b` | yes |

With the default every acknowledged message of the last seconds is gone after the cut (the
stream itself survives on 2.10.23 and 2.12.1); with `sync_interval: always` none is. As for
Redis's `appendfsync everysec`, this is the documented behaviour of a setting, not a bug: NATS
documents the two-minute default and keeps it (issue 7564).

### Every configuration, explored (Milestone 299)

`hydo/hydo-configs.yaml` makes `conf` a setting to explore (`configs:`), with a `tradeoff:` note
for the default and for 10 s, 12 workload steps, and the fault kinds `kill` and `cut`. On the Mac
engine, `hydo explore --configs hydo/hydo-configs.yaml --set nats=2.12.1 --seeds 1-20` built one
image per configuration and explored each with the same seeds, in 113 s with the images cached:

| `conf` | schedules failed | minimal schedule | fp, twice |
|---|---|---|---|
| `nats-server.conf` (default) | 5 of 20, the 5 with a power cut | `disk=cut dseed=63cbe1e459320dd7 \| run C=5 W=:p1,r500,cparallel_driver_publish,0.0.0.697350162` | `1a6cee6459096728` |
| `nats-server-sync-10s.conf` | 5 of 20 | the same | `4638f825f8c84f26` |
| `nats-server-sync-always.conf` | 0 of 20 | | |

The table labels the lost messages the setting's documented trade-off, found only with the
default and 10 s. `docs/HISTORY.md`, Milestone 299, has the details.

## Reactions drawn by the explorer

`hydo explore --seeds 1-50 --strategy process --reactive fault --react-events nats.publish_acked`
on the `always` images (the same process schedules plus, on 60% of them, one reaction at the
first acknowledged publish: a cut, a kill or a stop of one node):

| image | failures | signatures |
|---|---|---|
| b22a (2.10.22) | 23 of 50 (16 without reactions), all reproduced; the minimal schedule is seed 1's power cut again (`cc7cca364d1f4fe4` twice) | 1: stream lost |
| b23a (2.10.23) | 0 of 50 | |

## Swarm: the same finding among disk faults

`hydo explore --seeds 1-50 --strategy swarm --new-faults`; the explorer classes a failure by the
faults it needed (`durable-defaults` = no knob relaxed and an honest disk; `lying-disk` and
`io-errors` = a disk fault was on):

| image | failures | by class and signature |
|---|---|---|
| b22 (2.10.22) | 37 of 50, all reproduced | durable-defaults: 13 stream lost; lying-disk: 11 stream lost, 1 lost messages, 2 "service cannot recover (committed data unrecoverable)"; io-errors: 1 stream lost, 4 "n# restart failed", 5 the read timed out |
| b23 (2.10.23) | 37 of 50, all reproduced | durable-defaults: 12 lost messages (3.4), 1 the read timed out, no stream lost; lying-disk: 10 lost messages, 1 stream lost (seed 19, a disk that drops acknowledged writes, then a cut), 2 "data disk unmountable", 1 "service cannot recover"; io-errors: 7 "n# restart failed", 2 the read timed out, 1 lost messages |

With an honest disk, 2.10.22 loses the stream and 2.10.23 does not. With a lying disk or I/O
errors both releases fail in several ways; those classes say the disk broke its promise, which
NATS cannot be expected to survive. The swarm's honest-disk minimal schedule (from seed 2) and an
I/O-error one (from seed 17, one kill and no power cut), twice each from a fresh boot:

| schedule | image | verdict | fp | ctl | twice |
|---|---|---|---|---|---|
| `disk=cut dseed=dd8d1ecadadb959b \| run C=19 W=:p3,r500,cparallel_driver_publish,0.0.0.1979818538,1.0.0.313146279,2.1.0.1550708684` | b22 | VIOLATION … stream jepsen does not exist (acked=3) | `b820143548c71135` | `69c1260c460e20e5` | yes |
| | b23 | VIOLATION … lost 3 of 3 acknowledged messages (read 0; msgs=0 first=0 last=0 leader=n3) | `9ddcc6f24b87a3ff` | `d28df616e57d7d18` | yes |
| `disk=ioerr:2 dseed=de2648859907e743 \| run K=n1@11+1 W=:p1,r1000,…` (15 publishes, one each round) | b22 | VIOLATION … stream jepsen does not exist (acked=11) | `4a5534c4ae162715` | `a77f5ab59330a5c8` | yes |
| | b23 | ERROR n1 restart failed | `b794b2dc25d7b6bf` | `adc36a4c898d354e` | yes |

## Every power-cut point

`hydo explore --sweep cut` runs one schedule per cut point, `C=1` to `C=30`, each with its own
composition, every failure re-run and the first shrunk:

| image | failing points | signature | minimal (shrunk from `C=1`), fp twice |
|---|---|---|---|
| b121 (2.12.1, default fsync) | 30 of 30 | lost # of # acknowledged messages | `disk=cut dseed=0 \| run C=1 W=:p1,r1000,cparallel_driver_publish,0.0.0.1390654497` `d487e61201a82467` |
| b121a (2.12.1, `always`) | 0 of 30 | | |
| b23 (2.10.23, default fsync) | 30 of 30 | lost # of # acknowledged messages | the same schedule, `d855e2ad3c433927` |
| b23a (2.10.23, `always`) | 0 of 30 | | |
| b22a (2.10.22, `always`) | 30 of 30 | stream lost | the same schedule, `d35cfe0c2cebcd78` |

So, within this workload's 15 to 30 seconds: with the default `sync_interval` every power cut
loses the acknowledged messages written since the stream was created (Jepsen 3.4), on 2.10.23 as
on 2.12.1; with `always` none is lost, unless the release is 2.10.22, where every power cut
deletes the stream (3.1).

## Replay

`hydo repro <the b22 exploration>` ran every failure and the minimal schedule again from fresh
boots: 15 of 15 reproduced with the recorded fingerprint (the minimal schedule
`8c81413d2915448a`). Every schedule in the tables above ran twice from a fresh boot with the same
fp and ctl.

## The nodes as a group (Milestone 195)

`hydo/hydo-groups.yaml` is `hydo/hydo.yaml` with the three nodes declared as the group `nodes`.
Its `primary:` command prints the JetStream meta leader's name from any node's `/jsz`. The
fault tokens then aim at the cluster: `K=nodes.all@1+2` kills every node at once, which needed
three reactions above. Images built from Milestone 195's tree: 2.10.22 `46d0108f…`, 2.10.23
(`--set nats=2.10.23`) `53b8fc64…`. Each schedule ran twice from a fresh boot with one fingerprint:

| schedule | 2.10.22 | 2.10.23 |
|---|---|---|
| `disk=none \| run K=nodes.all@1+2` | stream lost (acked=2) `10afd1c5d6170bd2` | OK `25a3d38b0c8dc3d2` |
| `disk=none \| run K=nodes.primary@5+1` | stream lost (acked=19) `e97d8040917cb5db` | OK `4b9dc76af77a1164` |
| `disk=none \| run N=nodes.halves@3-9:loss,100%` | lost 9 of 38 acknowledged messages `956df32ad9956f56` | OK `05c5a0496fb0aea0` |
| `disk=none \| run N=nodes.ring@3-9:loss,100%` | OK `d9a6f7a30e7eb49a` | OK `f20f9d1d0b87dd8d` |

`hydo explore --seeds 1-30 --strategy process --cluster-faults` on 2.10.22 found 11 failures,
against 9 without the option (all with a power cut). Two of the 11 have no power cut: seed 18,
whose kill of every node alone (`disk=none dseed=f92a9eae7103378b | run K=nodes.all@19+3
W=06dd08ca,p2,q1,r500`) loses the stream, and seed 22, shrunk to `disk=none dseed=d9f3d4e02b8520f9
| run N=nodes.isolate@2-10:loss,100% T=11:-18 W=e195e771,p4,q2,r500` (lost 5 of 34
acknowledged messages, `e6b474bf9a8a5c14`; 2.10.23 passes it). docs/HISTORY.md "Milestone 195"
has every value.

## What the explorer lacked

- **One kill per schedule.** Jepsen's harness kills several nodes; PR 5946's reproducer kills all
  three at once. The process strategy draws one `K=` (and a second `K=` is now refused), so it
  reached 3.1 through power cuts and, on b22a, through one kill combined with a pause or a clock
  step. Several kills at once had to be written by hand as reactions (until Milestone 195:
  `K=nodes.all@...` and `--cluster-faults`, above).
- **Shrinking a chosen failure.** The explorer shrinks the first failure of each signature; the
  crash-only failures shared the power cut's signature and were shrunk by hand.
  `hydo explore --schedule` runs a schedule once without shrinking it.
- **Fault kinds for 3.2, 3.3 and 3.5.** No file-corruption fault (bit flips, truncation of one
  node's files) and no power cut of one service (one data disk for the whole guest). Milestone
  183 added both (`G=` and `L=`); Milestone 235 used them (the last section).

## Friction log: what an outside user needed and where they looked

Each item: what was needed, where it was looked for, and what was done. Items marked FIXED were
changed in hydo in this milestone (its own commit, with a test).

1. **Installing on a Mac.** A checkout has no engine binaries or guest kernels (`hydo doctor`:
   7 failures), so the Mac release was built from main (`hydo/dist/mac/build-release.sh` with its
   cached assets) and installed into a prefix. That release has neither the second guest kernel
   nor the aarch64 `ip`/`iptables` that `network: bridge` needs (doctor: WARN; open item 27). The
   kernel came from the main checkout; the tools were fetched with `hydo-nix-fetch --closure`
   and named with `HYDO_ARM64_STORE`, which only hydo-build's error message mentions. On macOS's
   case-insensitive disk `hydo-nix-fetch` skips `libxt_MARK.so` next to `libxt_mark.so` (only
   the freeze needs them).
2. **The skill's text.** SKILL.md said the skill needs Python 3.9 (not since M107) and that the
   arm64 build refuses `addr:`, `fakes:` and `params:` (only `guest.cpu: v3`); faults.md said to
   prefer `--more-faults`, which the Mac explorer refuses ("explore.py: --more-faults needs
   --backend hvx", an old program name too). FIXED in the skill.
3. **Three nodes, one store.** `hydo compose` put each node's named volume on `/data` aside
   ("the image's /data is already the data disk's root (shared by every service)"), so the three
   NATS nodes would have shared one JetStream store; a hand mount `{src: /data/vol/n1-data, dst:
   /data}` was refused by `hydo build`. FIXED: hydo-build accepts a data-disk directory on
   `/data`, and `hydo compose` writes it when several services each have their own volume on
   `/data` (MANIFEST.md, "From Docker Compose" and "services[].mounts").
4. **Several releases of one image.** Importing the Compose file again with another
   `NATS_VERSION` (`--env-file`) overwrote the first import's `fetch-images.sh`; a combined script
   was written by hand. `params:` with `{{nats}}` in the image paths works, but no example of the
   skill shows it.
5. **The test client.** There is no `hydo-read` store or history checker for a queue or stream
   (Jepsen used a queue workload), so the example brings a Go client and a shell check;
   `root.files` has no per-architecture source (an `image:` has `{x86_64:, arm64:}`), so both
   binaries are staged and chosen with `$(uname -m)`.
6. **The fsync setting.** `sync_interval` needs a restart, so it is not a knob (faults.md says
   so); it is a build parameter choosing the configuration file.
7. **A check's time limit.** The first power-cut runs ended `ERROR ... check acked_readable timed
   out after 20s` while the recovered cluster elected its leaders: a check runs under
   `test.command_timeout`, which checks.md did not say. FIXED in the skill.
8. **busybox grep.** The first check computed "acknowledged but not read" with
   `grep -v -x -F -f <read file>`; with an empty read file busybox matches every line, so a run
   that read 0 of 20 acknowledged messages passed. The check now uses `awk`. FIXED: a hazard in
   hazards.md.
9. **Killing every node.** `K=n1@3+4 K=n2@3+4 K=n3@3+4` killed only n3, without a word (the last
   token of a letter won). FIXED: a second `K=`, `P=` or `X=` is refused, naming reactions as the
   way to fault several services at once.
10. **Reactions and composer commands.** `A=<event>:kill-<svc>` never fired on a record that the
   publish command printed: a command's output reaches the run's report, not the console the
   reaction watches. The command now writes its record to `/dev/console`, as the SDKs do. FIXED
   in composer.md (the controller could hand such records to reactions: open item 38).
11. **Signatures.** The first check listed the lost ids in its last line, so each count of lost
   ids was its own signature and was shrunk separately (ten signatures for one finding). The ids
   moved to an earlier line. FIXED: checks.md says so.
12. **Error kinds.** NATS's start-up message "Error trying to connect to route" (a peer not
   listening yet) is three kinds, one per node; `hydo errors accept` without `--from` reads
   `hydo-out/explore` while the explorations were elsewhere.
13. **The corpora.** A new directory under `hydo/onboard/examples/` enters the explorer's and the
   controller's identity corpora; the controller's shell model has no restart policies and failed
   on the imported `restart: unless-stopped`. FIXED in the test support (such a plan is left to
   `restart.rs`), goldens re-recorded.
14. **Stale text.** The imported file's header names `hvx/onboard/hydo-compose` and
   `HYDO_SEEDRNG=... hvx/onboard/hydo-build` (Python-era paths; open item 38).
15. **Error-kind shrinking.** In the swarm explorations with disk faults, shrinking every new
   error kind (NATS's RAFT "Critical write error" lines under I/O errors and lying disks, one kind
   per node) took longer than the 50 schedules (`--errors-shrink 0` turns it off).
16. **Memory.** A 1 GiB hvm worker takes about 1.8 GB of the Mac's memory; six of them beside
   other sessions' VMs filled the swap, so a second exploration beside the first was stopped.
17. **Shrinking a chosen run.** Two crash-only failures shared the power cut's signature, so the
   explorer did not shrink them, and `hydo explore --schedule` only runs a schedule; they were
   shrunk by hand.

## One node's storage faults (Milestone 235)

The question: can hydo reach Jepsen's findings 3.2, 3.3 and 3.5 in one virtual machine, with the
two faults of Milestone 183? `G=` changes bytes of one service's files, and `L=` is the power loss
of one service while the others run on. If it can, these findings need no second virtual machine.

### The setup

`storage/hydo.yaml` is the cluster of `hydo/hydo.yaml` with these changes:

- Each node keeps its store on a 128 MiB ext4 file system of its own (`storage: {dir:
  vol/n1-data, size_mib: 128}`). `L=` then takes away only that node's unwritten pages.
- The default release is 2.12.1, the release Jepsen tested. `--set conf=` picks the
  configuration: the default `sync_interval` (two minutes), `nats-server-sync-10s.conf` (10 s,
  the setting of Jepsen's 3.5) or `nats-server-sync-always.conf`.
- A second command, `workload/parallel_driver_read`, reads the whole stream while the faults run,
  as Jepsen's readers do.
- A second check, `checks/final_replicas_agree`, asks each node's monitoring port for its own
  replica's state (`/jsz`). It fails when the nodes still disagree 60 s after the heal: Jepsen's
  split brain.
- The workload has 300 rounds of 500 ms (150 s of guest time) in place of 30 rounds.

Every run used hvx on the bare-metal machine, with the `hydo` of the 0.3.2 Linux release and the
x86_64 images built there: the first x86_64 run of this example. The images (`hydo build
hydo.yaml`, with `--set conf=`) are `aaf38a3b…` (the default), `d3a83762…` (10 s) and
`fb88948c…` (`always`). The schedules marked "twice" ran twice from a fresh boot and gave one
fingerprint and one `ctl` value both times. Without faults, `disk=none | run` passes on all
three images: 457 commands, every acknowledged message read back, and the replicas agree
(default `448731c9f6526288` twice, 10 s `dba919e07b472b82`, `always` `eac7bd60c4dbbae9`). The
previous build (`871f182b…`), whose only difference was the text of a regression file's
comment, gave every schedule in these tables the same verdict and `ctl` value, and other
fingerprints.

### 3.5, reached: one node's power loss loses acknowledged messages

The schedule `disk=none | run P=n1@2+10 L=n2@10+2 K=n3@12+6`
(`regressions/one-node-power-loss.schedule`) does three things:

1. n1 is paused before step 2 for 10 steps. n2, the stream leader at the start, and n3
   acknowledge each message without n1.
2. Before step 10, n2 loses its power. Its own file system drops the pages that Linux had not
   written back, and NATS had synced none of them.
3. Before step 12, n3 is killed. n1 and n2 elect a leader, and neither has the messages
   acknowledged while n1 was paused. n3 starts again at step 18 and follows that leader.

| configuration | verdict | fp | ctl |
|---|---|---|---|
| default `sync_interval` (2 min) | VIOLATION [acked_readable] lost 4 of 207 acknowledged messages (ids 7, 9, 10, 12), twice | `de74a8ee1b353f37` | `7d823eb14365a818` |
| `sync_interval: 10s` | the same, twice | `46628f4bd1bd5cc3` | `5ab9496a14cd87ad` |
| `sync_interval: always` | OK, twice | `77f862048f5bda9f` | `49e3f997eeb05da3` |

After the loss the three replicas agree: the messages are gone from every node, so the check
`replicas_agree` passes. Jepsen's run also left the nodes divergent; that part was not reproduced
here. Each fault alone, and a kill of n2 in place of its power loss, passes (default image, one run
each):

| schedule | verdict | fp |
|---|---|---|
| `P=n1@2+10 K=n3@12+6` (no power loss) | OK | `37248ce30d277726` |
| `P=n1@2+10 L=n2@10+2` (n3 keeps the messages and votes) | OK | `196a22df5d76f317` |
| `L=n2@10+2 K=n3@12+6` (n1 votes and has the messages) | OK | `db94fd69ec478fa5` |
| `P=n1@2+10 K=n2@10+2 X=n3@12~0` (n2 killed: its page cache stays) | OK | `b33f8b12e5df6aa4` |
| `P=n1@2+10 L=n2@10+2 X=n3@12~0` (the same with the power loss) | VIOLATION, lost 4 of 200 | `1da471f74c1323be` |

So the loss needs one node's power loss, not a kill. This is NATS's documented trade-off of
`sync_interval` (issue 7567), as for 3.4, but here only one of the three nodes lost its power.

### 3.3, reached: one node's corrupted metadata snapshot deletes the stream

The schedule `disk=none | run G=n2@150+2:zero,len=4096,file=_meta_/snapshots P=n1@154+6
K=n3@154+120` (`regressions/meta-snapshot-corrupted.schedule`):

1. Before step 150, n2 is killed, 4 KiB of its newest metadata snapshot are zeroed at a seeded
   offset, and n2 starts again before step 152. Its log: `RAFT [44jzkV9D - _meta_] Snapshot
   corrupt, checksums did not match`. NATS deletes the snapshot and replays only the log entries after
   it, so n2 no longer knows the stream.
2. 30 s after its start, n2 checks for orphans and deletes its own replica: `Detected orphaned
   stream '$G > jepsen', will cleanup`.
3. n1 is paused and n3 is killed before step 154, so n2 becomes the metadata leader. It then
   answers that the stream does not exist, while n1 and n3 keep the stream's files. n1 logs a
   warning that the stream has no quorum, as Jepsen's n4 did.

The quoted lines come from `hydo inspect logs` of runs on a build from before the regression
files were written.

| schedule | image | verdict | fp | ctl |
|---|---|---|---|---|
| `G=n2@150+2:zero,len=4096,file=_meta_/snapshots` | default | VIOLATION [replicas_agree] replicas diverge (n2: no stream; n1 and n3: every message), twice | `47abbcc9f408cb54` | `228280111090a841` |
| the regression | default | VIOLATION [acked_readable] stream lost: the cluster says stream jepsen does not exist (acked=109), twice | `5032361f7b335d66` | `714d727a52247ef5` |
| the regression | `always` | the same, twice | `0d5837bd34a54d16` | `9defedee0cd86a99` |
| `P=n1@154+6 K=n3@154+120` (no corruption) | default | OK | `4eadb2c2984fd50e` | `dc3acbd00d216b96` |

Three conditions are needed, and each was checked on copies of the manifest:

- **A metadata snapshot that holds the stream.** At step 10 the newest snapshot is `snap.2.4`
  (log index 4, taken while the cluster formed), and corrupting it, with zeros, a flip or a cut,
  passed. The metadata layer takes a snapshot every minute; at step 150 (75 s) the newest one is
  `snap.2.166`.
- **Metadata entries after the snapshot.** Without the readers, nothing adds entries after the
  stream is created, and the node gets the whole state from the leader again. The same schedule
  on a copy without `parallel_driver_read` passed.
- **The corrupted node as the metadata leader**, for the loss of the whole stream. Without it,
  the cluster keeps the stream on two nodes and only the replicas diverge. Six variants of the
  pause and the kill ran on a build of this manifest from before its regression files were
  written. They started at step 154, 156 or 158 and paused n1 for 3 or 6 steps. Five lost the
  stream, and one diverged. The image's bytes move NATS's elections, so such a variant can change its verdict
  between builds; the divergence of the first row does not depend on the election.

On that build, `flip` and `trunc` of the snapshot lost the stream too. The fsync setting does not
matter: the `always` image loses it the same way.

### The explorer with these faults

`hydo explore --seeds 1-16 --strategy process --storage-faults --cluster-faults --workers 2
--no-shrink --errors-shrink 0` ran on the previous build of the default image (`871f182b…`)
and took 584 s. Of its 16 schedules, 10 had a `G=`, 7 an `L=` (12 at least one of them) and 5 the whole guest's power cut. It found 4 failures,
each reproduced:

| seeds | class | verdict |
|---|---|---|
| 5, 6 | `corrupted-files` | lost 1 of 171 and lost 6 of 214 acknowledged messages |
| 12 | `durable-defaults` | lost 8 of 149 acknowledged messages (ids 284 to 299) |
| 1 | `durable-defaults` | the replicas diverge: n1 holds 224 messages, n2 and n3 hold 221; every acknowledged message is there |

All four schedules end with the whole guest's power cut `C=300`, the last step. Seeds 1, 5, and 6
ran again without `C=300`, and each passed, and so did seed 1 without its `L=` too. So the losses
are 3.4: the default `sync_interval` and a power cut of every node. Seed 1 is new for this
example: after a power cut of every node, one node keeps three unacknowledged messages that the
others lack, for good. Its schedule is `disk=cut dseed=4127cc1347fae6ba | run K=n2@52+20
N=n2-*@294-299:loss,72% C=300 P=n1@145+4 T=191:30 W=3e71cf9e,p1,q2,r1000 L=n1@76+3`, and it
was not investigated further. The exploration did not draw the schedules of 3.5 or 3.3; both need
faults in a narrow order on chosen nodes, written by hand in the preceding sections. The explorer was run without
shrinking, because each run is 150 s of guest time.

### 3.2 with a stream of one block: not reached

NATS 2.12.1 repaired every corruption of one node's `.blk` files that was tried. These runs used
copies of the manifest from before the readers were added. The copies had 30 or 600 rounds. The 600 rounds
(300 s) matter because a stream group takes its first snapshot two to three minutes after it
starts. Only then does it drop the older entries of its log, the copy a node could repair its
store from. At step 560 that snapshot existed (`S-R3F-upVsQPWE/snapshots/snap.1.506`). Every run
passed both checks, with all acknowledged messages present on every node:

| fault (all on the stream's `msgs/1.blk`) | and then |
|---|---|
| `G=n1@10+2:flip`, `zero,len=4096`, `trunc`, `flip,n=32` (30 rounds, before any stream snapshot) | `K=n3@14+4` (the stream leader) |
| `G=n3@10+2:zero,len=4096` (the stream leader itself) | nothing |
| `G=n1@10+2:zero,len=4096,file=upVsQPWE/msgs` (the stream group's own log) | `K=n3@14+4` |
| `G=n1@560+2:zero,len=4096`, `flip,n=4`, `trunc` (after the stream snapshot) | `K=n3@563+4` |
| `G=n2@560+2:zero,len=4096`, `flip,n=4` | `K=n3@563+4` |
| `G=n1@560+2:zero,len=4096`, `flip,n=4`; `G=n1@560+1:trunc` | `P=n2@562+4 K=n3@562+4` (n1 the only node to vote for) |

The corrupted node's log shows the repair (`Stream state outdated, last block has additional
entries, will rebuild`), and after the heal its replica agrees with the others. Two differences
from Jepsen's runs explain why the loss did not happen here:

- **Two corrupted nodes.** Jepsen's runs corrupted one or two of five nodes, and the split brain
  of 3.2 had corruption on n1 and n3. A hydo schedule corrupts one node: a second `G=` is refused
  (`G= appears more than once in the schedule; one G= token per schedule`).
- **Many blocks.** Jepsen's stream held about 1.4 million messages. NATS cuts a block at its first
  bad message, and a gap in the middle of the stream is not filled from the peers (a maintainer's
  comment on issue 7549). This stream holds about 900 messages, all in its first block, so a
  corrupted block is always the last one, and the normal catch-up refills it.

### One virtual machine or several

| what the finding needs | in one VM today | the change inside one VM |
|---|---|---|
| one node loses what it had not synced, the others run on (3.5) | yes: `L=` on the node's own file system | none |
| one node's files corrupted (3.2, 3.3) | yes: `G=` | none |
| two nodes' files corrupted in one run (3.2, and 3.3 on five nodes) | yes since Milestone 245: `G=n1,n3@...` or `G=nodes.majority@...` | built: the list and the group's selector in `hydo-schedule`, one corruption per service in the controller, `--corrupt-several` in the explorer |
| more than one block of stream data per node (3.2's gap in the middle) | yes: `blocks/` publishes 24,000 messages of 1 KB before the snapshot | none in hydo |
| five nodes (most of Jepsen's runs) | a manifest with five services | guest RAM: the three nodes ran in 1 GiB |
| writes the kernel wrote back without a sync are lost at the power loss (open item 61) | yes since Milestone 245: `L=<svc>@<at>+<down>:strict` | built: the guest's periodic writeback off from the run's start (docs/design/storage-faults.md) |
| the peers see the node go silent | yes since Milestone 245, with `:strict` and `network: bridge` | built: the node's bridge port down while it is down |

Separate virtual machines are needed only for a separate kernel or a separate clock per node:
one node's kernel crash, one node's memory pressure, or clocks that drift apart. Linux keeps one
realtime clock per kernel; its time namespaces move only the monotonic and boot clocks. None of
3.2, 3.3 and 3.5 needs them.

## Several nodes' corrupted blocks, and the strict power loss (Milestone 245)

Milestone 245 built the parts that the preceding table names. `G=` takes a list of services or a
group's selector (`G=n1,n3@...`, `G=nodes.majority@...`), and `L=<svc>@<at>+<down>:strict` is a
harsher power loss of one service (`hydo guide faults`). Every run used hvx on the bare-metal
machine, with this milestone's `hydo` inside a copy of the 0.3.2 Linux release, and images built
there. "Twice" means two runs from a fresh boot that gave one fingerprint and one `ctl` value.

### The setup: `blocks/`

`blocks/hydo.yaml` is the cluster of `storage/hydo.yaml` with these changes:

- The first command `workload/first_fill` publishes 24,000 messages of 1 KB before the snapshot
  that every run starts from. It uses `natsbulk`, a client of its own (`client/bulk/`), so that
  `natsjs` and the other manifests' images keep their bytes. With `test.first_records: true` the
  24,000 values stay among the acknowledged ones, so the checks look for each of them.
- Each node's stream then has three blocks: `jepsen/msgs/1.blk` and `2.blk` hold 8 MB each, and
  `3.blk` is the last one. A cut of 1 MB leaves `1.blk` at 7,339,502 bytes.
- The check `final_acked_readable` reads the stream with `natsbulk read`, one message at a time by
  its sequence, in at most 150 s. `natsjs read` gives the whole read 20 s, and with 24,000
  messages its ordered consumer got `no responders` on every attempt.
- Each node's file system has 384 MiB, the guest 2 GiB, and there is no reader during the faults.

The image is `071160781d5d4d5d…` (default `sync_interval`) and `c273173c32e9fea0…` (`always`).
Without faults, `disk=none | run` passes: all 24,457 acknowledged messages are read back, and the
replicas agree (fp `524ae0f8c706a3c1`). A first version published 150,000 messages of 100 bytes.
Its runs ended in `TIMEOUT workload`, because the host reads the 1.2 MB list of acknowledged
values from the guest in pieces of 3,000 bytes.

### Corrupted blocks on two nodes: 3.2, reached

| schedule | verdict | the replicas after the heal | fp | ctl |
|---|---|---|---|---|
| `G=n1,n3@10+2:flip,n=32,file=jepsen/msgs/1.blk` | VIOLATION [replicas_agree], twice | n1 17,216 (7,241 deleted), n2 24,457, n3 16,384 (8,073 deleted) | `575135a56fdc857f` | `d23017be675ff418` |
| `G=n1,n3@10+2:zero,len=65536,file=jepsen/msgs/1.blk K=n2@16+6` | VIOLATION [acked_readable] lost 2,456 of 24,446 acknowledged messages (leader n3), twice | n1 19,237 (5,209 deleted), n2 24,446, n3 21,990 (2,456 deleted) | `2336929ebcaa0ec4` | `b23c952824d09669` |
| `G=n1,n3@10+2:flip,n=32,file=jepsen/msgs/1.blk K=n2@16+6` | VIOLATION, lost 8,073 of 24,447 (leader n3) | n1 7,241 and n3 8,073 deleted | `992959583f4fb7a9` | `6c7827406d4f3fb9` |
| the second row on the `always` image | VIOLATION, lost 5,209 of 24,441 (leader n1) | n1 5,209 and n3 2,456 deleted | `e153b692d250382c` | `e7c36e1b68f97fdb` |

Each fault apart, on the default image, one run each:

| schedule | verdict | fp |
|---|---|---|
| `K=n2@16+6` (no corruption) | OK | `d5c3a90189ae46eb` |
| `G=n1@10+2:flip,n=32,file=jepsen/msgs/1.blk` (one node) | VIOLATION [replicas_agree]: n1 has 7,241 deleted, n2 and n3 hold every message | `e3490b5fb6819c5e` |
| `G=n1,n3@10+2:flip,n=32,file=jepsen/msgs/3.blk` (the last block at step 10) | OK | `04619d5fb5f45caa` |
| `G=n1,n3@10+2:flip,n=32,file=jepsen/msgs/1.blk` on the `storage/` image (one block) | OK | `48c4d8c6d2aafa1e` |

What happens is Jepsen's "Swiss cheese". A node whose block in the middle of the stream has a bad
message drops that block's messages from the first bad one to the block's end. The stream state
counts them as `deleted`, and the node never gets them back from its peers. Its log says nothing
about the bad block: `hydo inspect logs` of the first row shows on n1 and n3 only the warnings of
any restart (`Stream state outdated, last block has additional entries, will rebuild`).

The same flip in the last block, or in a stream of one block, is repaired by the normal catch-up.
So the many blocks decide the divergence, and one corrupted node is enough for it. Acknowledged
messages are lost when a corrupted node leads the stream. With two of three nodes corrupted, the
kill of the intact node n2 leaves a majority of corrupted nodes, and one of them leads. An fsync
before every acknowledgement does not help: the `always` image loses 5,209 messages.

The first round, on a build of `blocks/` with an earlier check, found the same in more forms:
zeroed ranges, a 1 MB cut (1,011 deleted on each corrupted node), the corruption under the
running nodes followed by `K=n1,n3@14+4`, `G=nodes.majority@10+2:...,file=jepsen/msgs/2.blk`, and
`G=nodes.all@...`.

The first two rows are saved as `blocks/regressions/two-nodes-corrupted-blocks-diverge.schedule`
and `two-nodes-corrupted-blocks-lose-acked.schedule`.

`hydo explore --seeds 1-8 --strategy process --storage-faults --corrupt-several --cluster-faults
--workers 2 --no-shrink` on the default image finished 5 seeds. Then the bare-metal machine ran
short of memory, shared with other sessions' VMs, and the kernel ended the exploration. The draws
included `G=nodes.majority@140:trunc,seed=7c0ecfff`, which cut the metadata's `peers.idx` of n1
and n2 (OK), and `G=nodes.all@237+1:zero,...` with a whole-guest cut. A drawn `G=` names no
`file=`, so it hits any file of the node. In seed 2 the 4 flips of `G=n3@167:flip,n=4,...` hit
`jepsen/msgs/2.blk`, a middle block, under the running n3, which led the stream at the end: 4
acknowledged messages were lost, and the replicas agree. That case was not investigated further.
Seeds 1 and 5 end with `C=300`, and their recovery boots ran out of time (`TIMEOUT recovery`): a
cut of every node does not fit this image's time limits.

### 3.5 with the strict power loss

The images of `storage/` built with this milestone's `hydo` give the values of the section "One
node's storage faults" for the plain `L=`. The schedule of 3.5 gave `de74a8ee1b353f37` (default)
and `46628f4bd1bd5cc3` (10 s), with the `ctl` values of Milestone 235. So the plain `L=` and a
`G=` on one service did not change. The strict loss, each row twice unless it says once:

| schedule | image | plain `L=` | `L=...:strict` | strict fp |
|---|---|---|---|---|
| `P=n1@2+10 L=n2@10+2 K=n3@12+6` | default | VIOLATION, lost 4 of 207 (once) | OK, 13 publishes failed | `1c5614b58207ded3` |
| the same | 10 s | VIOLATION, lost 4 of 207 (once) | VIOLATION, lost 4 of 207 | `1175284d776d200c` |
| the same | `always` | OK (Milestone 235) | OK, 6 publishes failed | `8cded8a4bec51f4a` |
| `P=n1@60+92 L=n2@150+2 K=n3@152+6` | default | VIOLATION, lost 3 of 219 (once) | VIOLATION, lost 10 of 203 | `7758d99f0cf02392` |
| the same | 10 s | VIOLATION, lost 1 of 219 (once) | OK (once) | `0809b4f138a589b8` |
| `P=n1@2+10 L=n2@10+2:strict` | default | | OK (once) | `098a0468fa7aab13` |
| `L=n2@10+2:strict K=n3@12+6` | default | | OK (once) | `0fb18880bb542c26` |

What reached n2's disk, from the strict run's log lines and, for the plain run, from `hydo inspect
exec` at the run's start and just before step 150:

| schedule | image | sectors written to n2's partition since the run's start | dirty pages at the loss |
|---|---|---|---|
| `P=n1@60+92 L=n2@150+2 K=n3@152+6`, plain | default | 3,072 | 68 kB |
| the same, strict | default | 2,296 | 336 kB |
| `P=n1@2+10 L=n2@10+2:strict K=n3@12+6` | default | 0 | 32 kB |
| the same | 10 s | 496 | 96 kB |
| the same | `always` | 904 | 76 kB |

The second schedule shows the writeback. n1 is paused for 46 s, and n2 loses its power 75 s into
the workload. With the plain `L=`, Linux has written back what n2 wrote more than 30 s before the
loss, and 3 messages are lost. With `:strict` only what NATS synced reached the disk, 776 sectors
less, and 10 messages are lost. In the first schedule the loss comes 5 s after the start, before
any writeback, so the disks are the same in both modes. There the silent links change the
elections instead. With the plain `L=`, n1 leads the stream after the restarts and lacks the 4
messages. With `:strict`, the stream reports `NO quorum, stalled` until n3 starts again, and n3
still has them. The verdict of a schedule can thus go either way, so a hunt for 3.5 should try both
forms.

The Mac engine did not run these schedules. The strict form changes no engine: it is a setting of
the guest and a bridge port, the same on every engine.
