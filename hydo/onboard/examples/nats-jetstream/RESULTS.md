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
hunted: 3.2 and 3.3 (hydo has no file-corruption fault), 3.5 (hydo's power cut is the whole
guest's: one data disk, every node at once; there is no power cut of one service).

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
  node's files) and no power cut of one service (one data disk for the whole guest).

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
