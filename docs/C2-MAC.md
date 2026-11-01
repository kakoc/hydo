# C2 on the Mac: verdict equivalence of agent images on hvm (Milestone 128)

Step C2 of `docs/design/guest-control.md` ("Migration", point 2, and "Implementation plan"), for the
arm64 examples on the Mac engine hvm. The x86_64 examples (M127) and the bug benchmark (M129) ran
on the bare-metal machine in parallel and are not in this file.

The question: does an image whose first process is `hydo-agent` (built with
`hydo build --control agent`, driven by the host controller `hydo-control`) give the same answers
as the shell-init image of the same manifest? An agent image cannot have the shell image's
fingerprints (another `/init`, another guest execution), so the comparison is at the level of
meaning: the same verdict class (OK, VIOLATION, ERROR) and the same violated property for every
schedule.

## How it ran

- **Where:** this Mac (M2 Pro, macOS 26.4.1), branch `c2-mac` (from `98f38dd`, M123 merged). The
  Mac was shared with another project's tests the whole time (load average 30 to 60); the engines
  are deterministic under load, so only wall times suffered.
- **Programs:** a release-like prefix in the session's scratch folder: M121's Mac release prefix
  (the release layout, busybox, mke2fs, `tc`, the Redis example's aarch64 files) with this
  branch's `hydo`, `hydo-build`, `hydo-explore`, `hvm`, `hvmx`, the guest files of
  `hydo/onboard/`, the arm64 agent built by `hydo/crates/hydo-agent/build.sh aarch64` (sha256
  `6a3ea9f8…`, the pinned value), and M99's arm64 helpers (`hydo-world` `7bc58006…`, `hydo-read`
  `bf368613…`, `hydo_fc` `499f654e…`, `ip`/`iptables` in `guest/tc`) and second kernel
  `Image-explore-netns` (`806807e7…`). Default kernel `Image-explore` `f80c5655…`.
- **Inputs without the build machine** (which this step must not use): node-kv's and python-kv's
  aarch64 store paths from cache.nixos.org with `hydo-nix-fetch` (checked against the examples'
  `files.sha256`; the symlinks the fetch scripts also stage were copied from the fetched store);
  `rkv` built with `build-rkv.sh aarch64`; the shop's two arm64 programs and the gRPC demo's
  `orderd-arm64` and generated client from earlier Mac builds of the unchanged sources; the voting
  app's arm64 images from M99's `fetch-images-mac.sh` run; redis-oci's arm64 manifest
  (`sha256:4da694ba…`, the digest of `fetch-image.sh`) fetched with `hydo-fetch-image` as a
  `docker save` archive, because the Mac has no skopeo (the copy's `image: arm64:` names the
  archive instead of the OCI layout; the voting app's Mac manifest does the same).
- **Images:** each manifest built once as a shell image and twice as an agent image with the same
  inputs; every pair of agent builds was byte-identical (initramfs and data disk).
- **Runs:** `hydo run --image` (a fresh boot) for the recorded schedules, the shell image once and
  the agent image twice; `hydo explore --image` with the same options on both images, three
  workers each, side by side, resumable through `HYDO_EXPLORE_CACHE`; a comparison of every
  seed's first run (the same schedule string on both images, then the verdict kind and the set of
  violated property ids of its findings, every kind: invariants, assertions, history checks).
  Agent runs use the default cadence `rb=64`.

## Which examples

Built and compared (19 images from 16 manifests):

| example | manifest | kernel | shell image | agent image (two builds identical) | data disk |
|---|---|---|---|---|---|
| redis | `examples-mac/redis/hydo.yaml` | default | `c592cb1f…` | `12e381d2…` | `9ddfc929…` |
| redis-hist | `examples-mac/redis/hydo-hist.yaml` | default | `208e3875…` | `830ba466…` | `ab08f785…` |
| redis-read | `examples-mac/redis/hydo-read.yaml` | default | `eddeb1db…` | `5f69fb9f…` | `2f51b31d…` |
| redis-ram | `examples-mac/redis-ram/hydo.yaml` | default | `f9c8fc1a…` | `d68a7096…` | `9ddfc929…` |
| redis-oci | `examples/redis-oci/hydo.yaml` (archive, see above) | default | `d6097fe7…` | `2d892fab…` | `d4116ff4…` |
| redis-bridge | `examples-mac/redis-bridge/hydo.yaml` | netns | `c94bb380…` | `9f678f73…` | `26f4a507…` |
| ledger | `examples-mac/ledger/hydo.yaml` | default | `35d2d16d…` | `0fc28183…` | `ed04ad2a…` |
| ledger-spin | `examples-mac/ledger-spin/hydo.yaml` | default | `d722915a…` | `06fc3165…` | `0c26a375…` |
| redis-composed (atomic) | `examples-mac/redis-composed/hydo/hydo.yaml` | default | `f62e098b…` | `068141ae…` | `56e07f19…` |
| redis-composed (racy) | the same with `TRANSFER: racy` | default | `28461e50…` | `0f0e8b90…` | `56e07f19…` |
| node-kv | `examples/node-kv/hydo-mac.yaml` | default | `d63e68be…` | `63a41749…` | `d031f382…` |
| python-kv | `examples/python-kv/hydo-mac.yaml` | default | `bd5e67cc…` | `16502bc1…` | `93c5d2ca…` |
| python-kv-hist | `examples/python-kv/hydo-mac-hist.yaml` | default | `016d54ff…` | `c87f17fb…` | `03d395df…` |
| rust-kv | `examples/rust-kv/hydo.yaml` | default | `daab72fa…` | `9161b1b7…` | `e19be506…` |
| shop (buggy) | `examples/shop/hydo.yaml --set variant=buggy` | default | `dfcd8c62…` | `1f9338e3…` | `359c73c4…` |
| shop (fixed) | `examples/shop/hydo.yaml --set variant=fixed` | default | `796cf92f…` | `3f9a9203…` | `359c73c4…` |
| gRPC demo (buggy) | `hydo/grpcgen/demo/hydo-mac.yaml --set mode=buggy` | default | `849e2e1e…` | `5e7e2085…` | `3ba736d5…` |
| gRPC demo (fixed) | `hydo/grpcgen/demo/hydo-mac.yaml --set mode=fixed` | default | `b195ddcc…` | `c67f868b…` | `3ba736d5…` |
| voting app | `examples/compose-voting/hydo-mac.yaml` | netns | `f9038b23…` | `e0647a8c…` | `120e7088…` |

The voting app's shell image is M99's recorded `f9038b23…`, and its four recorded schedules gave
M99's fingerprints again on the shell image. The other shell images differ from older recorded
hashes because their manifests or inputs changed since (comments, the Rust `rkv` built in another
directory, python-kv's extra library links); the data disks are the recorded ones.

Not built: the etcd example (no aarch64 etcd is pinned, M99), and every x86_64-only example (the
stack, miniflux, restate-oci, avx2, redis-errors, the x86_64 composed plans), which
have no arm64 manifest.

## Verdict per example

| example | equivalent? | what differs (classification below) |
|---|---|---|
| redis | yes | nothing (swarm 100 of 100 once D3's wording is set aside, process 100 of 100, kill, cut and net sweeps all the same) |
| redis-hist | yes, after fix 1 | nothing after the fix; before it the history checks saw no records on the agent image |
| redis-read | yes | nothing |
| redis-ram | yes | 1 of 100 swarm seeds (an injected I/O error hits another disk request: D2) |
| redis-oci | yes | nothing (swarm 100 of 100) |
| redis-bridge | yes, except `F=` | the two freeze schedules are refused on the agent image (D4); everything else the same |
| ledger | yes | 7 of 100 swarm seeds, both directions, 39 against 38 failures, the same minimal schedule (packet-loss timing: D1) |
| ledger-spin | **no** | every agent run ends as SPIN: lock-step control does not run the rewind mode a counter loop needs (D5) |
| redis-composed (atomic) | yes | 2 of 100 swarm seeds, both lying disk (D2) |
| redis-composed (racy) | yes | 4 of 100 swarm seeds: 2 lost-update races (D1), 2 lying disk (D2); the same classes found and shrunk |
| node-kv | yes | nothing (the SDK's HYDO-ASSERT properties included, after fix 1) |
| python-kv | yes | nothing |
| python-kv-hist | yes, after fix 1 | nothing after the fix |
| rust-kv | yes | nothing |
| shop (buggy and fixed) | yes | nothing; the same 4 failures and the same two minimal schedules as M89/M96 |
| gRPC demo (buggy and fixed) | yes, after fix 2 | nothing after the fix; before it `orderd` did not start on the agent image |
| voting app | mostly | recorded schedules the same; exploration (seeds 1–50, process `--new-faults`, no shrinking) 47 of 50, 31 failures on each image: 1 seed timing (D1), 2 seeds where the agent image's checks find the database not answering (D6, not explained) |

## Recorded schedules

Each example's recorded schedules (RESULTS.md, docs/HISTORY.md M51–M57, M80–M83, M96, M99, M107,
M126), `hydo run` from a fresh boot: the shell image once, the agent image twice. "Agent twice
identical" means the same fingerprint, stop count and `ctl=`. "Same meaning" compares the verdict
class and the violated property, and for manifests with `checks:` also the host's history-check
verdict. Fingerprints of the two images differ by design.

| example | schedule | shell | agent (two runs) | agent twice identical | same meaning |
|---|---|---|---|---|---|
| redis | `disk=none \| run` | OK `bcd2ab406e1fcf1a` | OK `251aa0074e7e7642` | yes | yes |
| redis | `disk=none \| run K=redis@5+2` | OK `f5aa2f7d06d6e8b0` | OK `f77d2154ba8644b1` | yes | yes |
| redis | `disk=none \| run P=redis@5+2` | OK `0c17aafeb741e1a2` | OK `6748fc480df3a9da` | yes | yes |
| redis | `disk=cut \| run C=12 R=everysec` | VIOLATION `redis.acked_writes_survive` `d934c415a0816beb` | VIOLATION `redis.acked_writes_survive` `2ba69ae149ad2ed0` | yes | yes |
| redis | `disk=cut \| run C=12 R=always` | OK `6c1c24c66e236884` | OK `441c2f3af0235fb3` | yes | yes |
| redis | `disk=none \| run O=redis@5-8:resp:loss,100%` | OK `df5689f22687f693` | OK `6b3cbc7ba475a1af` | yes | yes |
| redis | `disk=none \| run N=redis@5-10:loss,60%` | OK `dbff167c8f304c9e` | OK `24cd46ed8f5c9bea` | yes | yes |
| redis | `disk=none \| run T=10:3600` | OK `bdaf6a87a70e8814` | OK `6f73c9d6735b2141` | yes | yes |
| redis-hist | `disk=cut dseed=1 \| run C=12` | OK; history OK `f1b13e25522180a9` | OK; history OK `e6817a499b54463a` | yes | yes |
| redis-hist | `disk=cut dseed=1 \| run C=12 R=everysec` | OK; history VIOLATION `redis.kv_linearizable` `2041ccef08d97fd9` | OK; history VIOLATION `redis.kv_linearizable` `9834aaa7d88ff5d1` | yes | yes |
| redis-read | `disk=none \| run` | OK `73570cb0029a52af` | OK `6f6a890e47df7b3f` | yes | yes |
| redis-read | `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION `redis.acked_writes_survive` `c8042106fdb452db` | VIOLATION `redis.acked_writes_survive` `c66d186c0d867dc2` | yes | yes |
| redis-ram | `disk=none \| run` | OK `56f023a6f38e1159` | OK `d2914b6a9bd02710` | yes | yes |
| redis-ram | `disk=cut \| run C=12 R=everysec` | VIOLATION `redis.acked_writes_survive` `9cdde0c414682532` | VIOLATION `redis.acked_writes_survive` `f218b8dd169737d1` | yes | yes |
| redis-bridge | `disk=none \| run` | OK `d91dc6963a0404a3` | OK `ba1e455ee5a6eae0` | yes | yes |
| redis-bridge | `disk=none \| run N=primary-replica@5-10:loss,100%` | OK `b50d2ec5d15dc833` | OK `c70376a12a7afc57` | yes | yes |
| redis-bridge | `disk=none \| run O=primary-replica@5-10:req:loss,100%` | OK `fb87ea4621911564` | OK `c01b608dc0bdd515` | yes | yes |
| redis-bridge | `disk=none \| run O=primary@5-8:resp:delay,300ms` | OK `d703e61bdbdb51fb` | OK `ff600aa415e3ab79` | yes | yes |
| redis-bridge | `disk=none \| run K=replica@5+3` | OK `16407a89bd0e0f3a` | OK `1bcbdeb825bf2497` | yes | yes |
| redis-bridge | `disk=cut \| run C=8` | OK `bd53c4eb42a31e21` | OK `d85122936a927ee9` | yes | yes |
| redis-bridge | `disk=none \| run F=replica<primary@5#3:2000` | OK `5ab120707c50bcb4` | ERROR no result `` | NO | **no** |
| redis-bridge | `disk=none \| run F=primary<workload@5#2:1000` | OK `d51a611fa1a33838` | ERROR no result `` | NO | **no** |
| redis-bridge | `disk=none \| run H=redisp@3-6:drop` | OK `1cdc1d26691e912c` | OK `3e381682b8072473` | yes | yes |
| ledger | `disk=none \| run` | OK `41b0e641957da1a6` | OK `9142e77626d9dde0` | yes | yes |
| ledger | `disk=none \| run O=front@5-8:resp:loss,100%` | OK `ce410b59d77057f9` | OK `226636df28deeab1` | yes | yes |
| ledger | `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION `ledger.acked_exactly_once` `631f0ae8508b9565` | VIOLATION `ledger.acked_exactly_once` `b037a0e5cf4b0ff0` | yes | yes |
| ledger | `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION `ledger.acked_exactly_once` `34f01afcf85b9322` | VIOLATION `ledger.acked_exactly_once` `6fb5073b7ad855b2` | yes | yes |
| ledger | `disk=none \| run N=front@0-30:loss,60%` | VIOLATION `ledger.acked_exactly_once` `854a3f0de47b74e5` | VIOLATION `ledger.acked_exactly_once` `e03863089782235a` | yes | yes |
| ledger | `disk=none \| run P=front@10+2` | VIOLATION `ledger.acked_exactly_once` `23cc35358531f39d` | VIOLATION `ledger.acked_exactly_once` `ead725b801c7a3b6` | yes | yes |
| ledger-spin | `disk=none \| run` | OK `12e6a03e6e22595d` | ERROR ERROR workload: SPIN (guest loops without exits; run invalid) `0000000000000000` | yes | **no** |
| ledger-spin | `disk=none \| run P=front@10+2` | VIOLATION `ledger.no_duplicates` `85e7e22a2d0a0746` | ERROR ERROR workload: SPIN (guest loops without exits; run invalid) `0000000000000000` | yes | **no** |
| redis-composed | `disk=none \| run W=0000002a,p4,q2,r100` | OK `524d3a03ac1276d4` | OK `fcbbeeec95b3d53a` | yes | yes |
| redis-composed-racy | `disk=none \| run W=0000002a,p4,q2,r100` | VIOLATION `total_conserved` `96f97912783caa87` | VIOLATION `total_conserved` `00dab157eee8e18f` | yes | yes |
| node-kv | `disk=none \| run` | OK `40065ecb80f4905e` | OK `bc9803591bc4dca4` | yes | yes |
| node-kv | `disk=cut \| run C=12 S=never` | VIOLATION `nkv.oracle` `2ce1128b0945e4a1` | VIOLATION `nkv.oracle` `986f3ee811825252` | yes | yes |
| node-kv | `disk=cut \| run C=12` | OK `2c97b278b4a34ece` | OK `4fd801201c1cb249` | yes | yes |
| node-kv | `disk=none \| run K=nkv@8+2 B=1234` | OK `cae82eda6ea1f308` | OK `d1252c0c54934427` | yes | yes |
| python-kv | `disk=none \| run` | OK `9ed003266f16c942` | OK `73a31b4ec3b66e31` | yes | yes |
| python-kv | `disk=none \| run K=pykv@8+2` | OK `fe11ae8864fe60f2` | OK `36dddb742f693e9d` | yes | yes |
| python-kv | `disk=cut \| run C=12 S=never` | VIOLATION `pykv.oracle` `bc7d9c88c425d435` | VIOLATION `pykv.oracle` `2ab906d5d7355f27` | yes | yes |
| python-kv | `disk=none \| run B=1234` | OK `356363e7db601f46` | OK `3e481a8f5b0926ce` | yes | yes |
| python-kv-hist | `disk=cut \| run C=12` | OK; history OK `1fb2b3eea7892383` | OK; history OK `63436b339df15265` | yes | yes |
| python-kv-hist | `disk=cut \| run C=12 S=never` | OK; history VIOLATION `kv.linearizable` `25c5d49f85583ba9` | OK; history VIOLATION `kv.linearizable` `2adf599f4632a659` | yes | yes |
| rust-kv | `disk=none \| run` | OK `871454dd27a429f4` | OK `03d4d2086f5039a5` | yes | yes |
| rust-kv | `disk=cut \| run C=12` | OK `cdf07fbde6a3e7a1` | OK `daaa504aa099c38e` | yes | yes |
| rust-kv | `disk=cut \| run C=12 S=never` | VIOLATION `rkv.oracle` `30fe75d75200e5ab` | VIOLATION `rkv.oracle` `2a34bbf1193cbf17` | yes | yes |
| rust-kv | `disk=none \| run K=rkv@8+2` | OK `7d55b4f31ee151da` | OK `d80ee7a4904cca36` | yes | yes |
| rust-kv | `disk=none \| run B=5abc,1000,500` | OK `d6ea5d3a41438acc` | OK `ffec0a4ffacf5458` | yes | yes |
| rust-kv | `disk=none \| run B=1234` | OK `5bea41187b684f8d` | OK `f3664cd39c21374b` | yes | yes |
| rust-kv | `disk=none \| run K=rkv@8+2 B=1234` | OK `90605683566bcc2c` | OK `4d0e3a825800754b` | yes | yes |
| rust-kv | `disk=none \| run P=rkv@5+3` | OK `f40f557c33fc0951` | OK `47ec216ccc43e33f` | yes | yes |
| shop-buggy | `disk=none \| run` | OK `f52e1501d39a52f9` | OK `ac3cb15f351b3138` | yes | yes |
| shop-fixed | `disk=none \| run` | OK `55197616ec33e025` | OK `0b44dd7007383a70` | yes | yes |
| shop-buggy | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION `payments.charged_at_most_once` `8ba0b6498b8fc91f` | VIOLATION `payments.charged_at_most_once` `c81934672e083181` | yes | yes |
| shop-fixed | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK `3f0633f83f715590` | OK `f38d82df98ef5373` | yes | yes |
| shop-buggy | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION `payments.charged_at_most_once` `251e4dd50d1707b1` | VIOLATION `payments.charged_at_most_once` `a4fa72dcc00d1b2e` | yes | yes |
| shop-fixed | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK `30dc1bb422e2b691` | OK `d522b69bb5f13f2d` | yes | yes |
| shop-buggy | `disk=none \| run H=payments.charge@6-9:reset,p=50` | VIOLATION `payments.charged_at_most_once` `af2c5f1994fe7ff0` | VIOLATION `payments.charged_at_most_once` `583a19a68939de08` | yes | yes |
| grpc-buggy | `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | VIOLATION `orders.consistent` `86739a642f05ead8` | VIOLATION `orders.consistent` `fa4a6c8053a3d362` | yes | yes |
| grpc-fixed | `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | OK `6120f4634c6216a0` | OK `a8fbcd437fc956ee` | yes | yes |
| voting | `disk=none \| run` | OK `bd065a3e8dc7eebc` | OK `746784dfd5d12d4f` | yes | yes |
| voting | `disk=none \| run K=worker@5+5` | OK `642d89607d21aa2e` | OK `63a319ba1e8d08b9` | yes | yes |
| voting | `disk=cut \| run C=12` | VIOLATION `votes.counted_soon` `1058b9d6fc1854f7` | VIOLATION `votes.counted_soon` `1a07d2c495c04928` | yes | yes |
| voting | `disk=none \| run N=vote-redis@5-12:loss,100%` | OK `88b7fe56b6025545` | OK `5800506e49c8a657` | yes | yes |
| redis-oci | `disk=none \| run` | OK `3eeec97c87347717` | OK `73096027d9cf97b1` | yes | yes |
| redis-oci | `disk=cut \| run C=12 R=everysec` | VIOLATION `redis.acked_writes_survive` `cec50ef2a922963e` | VIOLATION `redis.acked_writes_survive` `c8f42e74f0ad9269` | yes | yes |
| redis-oci | `disk=none \| run K=redis@5+2` | OK `7714b631aeb5c25a` | OK `990b372b6c847e88` | yes | yes |

**Transcript replay equals policy replay** on a sample of eleven agent runs (`hydo repro
--transcript <run>/control.jsonl`, which also types `control.recovery.jsonl` after a cut): redis
`K=redis@5+2` and the `R=everysec` cut, redis-bridge's link fault on the bridge and its cut, ledger
`O=front@0-30:resp:loss,70%`, the buggy shop's cut with a carried fake world, the racy composed
plan, rust-kv's BUGGIFY schedule, node-kv's cut, redis-hist's cut and the voting app's cut (an
`eventually` invariant across a power cut): every one gave the recorded fingerprint with the same
commands at the same stops (`REPRODUCED`). The replay's own `ctl=` differs only because its header
names the replay controller (M126).

## Explorations

The explorer's schedules for seeds 1–100 with the strategy and options each example is explored
with, on both images (the voting app seeds 1–50 without shrinking: its 3 GiB guests took about two
minutes a run with the Mac swapping), and the deterministic sweeps that apply on the Mac. "Same meaning" counts the
seeds whose first runs have the same verdict class and the same violated properties (the explorer
generated the same schedule string on both images for every seed of every exploration). Every
failure was re-run by the explorer and reproduced on both images. "Shrunk classes" are the
properties of the minimal schedules the shrinker produced; "properties" compares the exploration's
property table (assertions and invariants: passed, failed, not exercised).

| image | options | schedules | same meaning | shell failures | agent failures | shrunk classes | properties with the same status |
|---|---|---|---|---|---|---|---|
| grpc-buggy | `--strategy swarm --seeds 1-100` | 100 | 100 | 47 (47 reproduced) | 47 (47 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `VIOLATION service cannot recover after the fault`, `orders.consistent` | 24 of 24 |
| grpc-fixed | `--strategy swarm --seeds 1-100` | 100 | 100 | 40 (40 reproduced) | 40 (40 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `VIOLATION service cannot recover after the fault`, `orders.consistent` | 24 of 24 |
| ledger | `--strategy swarm --new-faults --seeds 1-100` | 100 | 93 | 39 (39 reproduced) | 38 (38 reproduced) | same: `ledger.acked_exactly_once` | 8 of 8 |
| ledger | `--sweep pause` | 12 | 12 | 6 (6 reproduced) | 6 (6 reproduced) | same: `ledger.acked_exactly_once` | 8 of 8 |
| ledger-spin | `--strategy swarm --new-faults --seeds 1-100` | 100 | 0 | 44 (44 reproduced) | 100 (100 reproduced) | differ: none | 5 of 8 |
| node-kv | `--strategy swarm --buggify --seeds 1-100` | 100 | 100 | 32 (32 reproduced) | 32 (32 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `nkv.oracle` | 17 of 17 |
| python-kv | `--strategy swarm --buggify --seeds 1-100` | 100 | 100 | 31 (31 reproduced) | 31 (31 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `pykv.oracle` | 17 of 17 |
| python-kv-hist | `--strategy swarm --seeds 1-100` | 100 | 100 | 32 (32 reproduced) | 32 (32 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `kv.linearizable` | 10 of 10 |
| redis | `--strategy process --new-faults --seeds 1-100` | 100 | 100 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 7 of 7 |
| redis | `--strategy swarm --new-faults --seeds 1-100` | 100 | 100 | 36 (36 reproduced) | 36 (36 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `redis.acked_writes_survive` | 7 of 7 |
| redis | `--sweep cut` | 20 | 20 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 7 of 7 |
| redis | `--sweep kill` | 20 | 20 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 7 of 7 |
| redis | `--sweep net` | 5 | 5 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 7 of 7 |
| redis-bridge | `--strategy process --new-faults --protocol-faults --seeds 1-100` | 100 | 100 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 10 of 10 |
| redis-bridge | `--sweep net` | 15 | 15 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 9 of 9 |
| redis-bridge | `--sweep protocol` | 48 | 48 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 10 of 10 |
| redis-composed | `--strategy swarm --seeds 1-100` | 100 | 98 | 49 (49 reproduced) | 47 (47 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `VIOLATION service cannot recover (committed data`, `acked_writes_survive`, `answers` | 20 of 20 |
| redis-composed-racy | `--strategy swarm --seeds 1-100` | 100 | 96 | 64 (64 reproduced) | 62 (62 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `VIOLATION service cannot recover (committed data`, `acked_writes_survive`, `answers`, `total_conserved` | 20 of 20 |
| redis-hist | `--strategy swarm --new-faults --seeds 1-100` | 100 | 100 | 45 (45 reproduced) | 45 (45 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `redis.kv_linearizable`, `redis.lists_append` | 8 of 8 |
| redis-oci | `--strategy swarm --new-faults --seeds 1-100` | 100 | 100 | 36 (36 reproduced) | 36 (36 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `redis.acked_writes_survive` | 7 of 7 |
| redis-ram | `--strategy swarm --new-faults --seeds 1-100` | 100 | 99 | 37 (37 reproduced) | 36 (36 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `redis.acked_writes_survive` | 8 of 8 |
| redis-read | `--strategy swarm --new-faults --seeds 1-100` | 100 | 100 | 36 (36 reproduced) | 36 (36 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `redis.acked_writes_survive` | 7 of 7 |
| rust-kv | `--strategy swarm --buggify --seeds 1-100` | 100 | 100 | 32 (32 reproduced) | 32 (32 reproduced) | same: `VIOLATION data disk unmountable (mount ...)`, `rkv.oracle` | 19 of 19 |
| shop-buggy | `--strategy process --protocol-faults --seeds 1-100` | 100 | 100 | 4 (4 reproduced) | 4 (4 reproduced) | same: `payments.charged_at_most_once` | 13 of 13 |
| shop-buggy | `--sweep protocol` | 228 | 228 | 48 (48 reproduced) | 48 (48 reproduced) | same: `payments.charged_at_most_once` | 13 of 13 |
| shop-fixed | `--strategy process --protocol-faults --seeds 1-100` | 100 | 100 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 13 of 13 |
| shop-fixed | `--sweep protocol` | 228 | 228 | 0 (0 reproduced) | 0 (0 reproduced) | same: none | 13 of 13 |
| voting | `--strategy process --new-faults --no-shrink --seeds 1-50` | 50 | 47 | 31 (31 reproduced) | 31 (31 reproduced) | same: none | 8 of 8 |

## Differences and their classification

- **D1, timing only (recorded, not forced away).** ledger (7 seeds), the racy composed plan (2
  seeds): random packet loss (`N=`/`O=` with a loss rate) and overlapping composer commands land
  differently because the agent image's guest runs a different program as PID 1 and is driven by
  host commands at stops instead of a shell loop. The detection counts are the same within one
  (ledger 39 against 38, racy plan 64 against 62), every class is found on both images, and the
  minimal schedules are the same or equivalent. This is the risk the design names ("timing shifts
  change which bugs a schedule finds"); the criterion is the detection rate, which holds. The
  verdict *texts* of a few recorded schedules differ the same way (redis `N=redis@5-10:loss,60%`
  acknowledged 18 against 17, the racy plan's sum 495 against 502, the gRPC duplicate's order list
  order), with the same verdict class and property.
- **D2, disk-request order (recorded).** redis-ram (1 seed, `disk=ioerr:3`), the composed plans (2
  and 2 seeds, `disk=lying:N`): the disk faults are a function of the request index, and the agent
  image's guest makes other disk requests (no shell init, no shell bookkeeping files, the agent's
  `syncfs` at S1), so the same index hits another request (on redis-ram the shell image's I/O error
  aborted the ext4 journal, the agent image's did not). M126 saw the same on hvx. The lying-disk
  and I/O-error classes are found and shrunk on both images.
- **D3, wording only.** "data disk unmountable" names the mount error differently: busybox `mount`
  (shell init) prints "No error information", the agent's `mount` op reports "errno 117" (EUCLEAN).
  The property and the verdict class are the same; the explorer's signatures differ, so the two
  images list this class under two signature strings. The comparison treats them as one property.
  A decision for C3: keep the agent's errno (more precise) and accept the new signature, or make
  the controller print busybox's text.
- **D4, refused by design (open in M124).** `F=` on a manifest with `network: bridge`
  (redis-bridge's two freeze schedules): the agent's `freeze` has no namespace field yet, so the
  controller refuses the schedule ("hydo-agent-proto's freeze has no netns"). Without
  `--freeze-call` the explorer draws no `F=`, so the redis-bridge exploration is unaffected. Needs
  the protocol addition (a new agent build, which changes the pinned agent and every agent image).
- **D5, an engine limit that needs a decision: busy-wait counter loops.** ledger-spin's
  `spin_count` is a counter loop that only hvm's rewind mode resolves (M57). The shell image's run
  ends as SPIN, hvmx runs it again with rewind and reports that run; under lock-step control
  `serve.c` refuses the rewind mode (it moves the exit count backwards, so stops would not be a
  pure function of the run), and the agent run ends `ERROR workload: SPIN` at its first counter
  loop. All 100 agent runs of the swarm and both recorded schedules fail this way, so the agent
  image cannot test this example at all. Exact-repeat loops (`spin_load`) are resolved under
  control as before.
- **D6, not explained: the voting app's database after a pause and a clock jump.** Two of 50
  seeds: `P=db@18+2 T=7:-2592000 O=db-*@0-6:resp:loss,74%` (shell OK, agent VIOLATION
  `votes.counted_soon`) and `P=db@1+5 T=18:31536000` (both VIOLATION `votes.counted_soon`, but the
  shell image's final `votes.counted` also fails while the agent image's cannot decide): on the
  agent image the checks' `psql` gets no answer from PostgreSQL ("db not answering") for the 60 s
  after the heal. In both agent runs the .NET worker exited after the heal's restart pass (the
  shell image's worker died before it and was restarted), so part of it is timing, but the
  database itself answers in the shell runs. Each schedule without one of its faults (without
  `T=`, without `O=`, without `T=` for the second) gives OK on both images. Not investigated
  further; a case for C3 (the controller's `T=` and pause, or the worker's reconnect storm against
  a database whose clock moved). The third differing seed (`K=db@8+1 P=redis@18+2 T=2:-30
  O=redis-*@10-15:req:loss,61%`: shell VIOLATION, agent OK) looks like the same timing in the other
  direction.

## Fixes (each with a test)

1. **The guest's own console lines reach `<tag>.log`** (`hydo-control`: `runner.rs`, `agent.rs`).
   The records a guest's programs print on `/dev/console` themselves (the history recorders'
   `HYDO-HIST` lines, the SDKs' `HYDO-ASSERT` records) were only in `<tag>.agent.log`, the agent's
   raw console, while every reader (the host's history checkers, the property table) reads
   `<tag>.log`, which an agent run fills with the controller's report lines. Found on redis-hist:
   `disk=cut dseed=1 | run C=12 R=everysec` gave the history VIOLATION `redis.kv_linearizable` on
   the shell image and "0 operations on 0 keys linearizable" on the agent image. Now the runner
   keeps each non-agent console line with the stop that reported it (`Finished::console`; not part
   of the transcript, so no `ctl=` changes) and `agent::console_text` puts them before the report
   lines of that stop, where the shell console has them. Test:
   `agent::tests::console_text_keeps_the_guests_own_lines`. After it, redis-hist, python-kv-hist
   and the SDK examples' property tables are the shell image's.
2. **`params:` applied to an agent image's plan** (`hydo-manifest`: `from_embedded_params`,
   `parse_params_file`; `hydo-control`: `agent::plan_of_embedded_params`, used by
   `plan_of_image`). The controller read the image's `/etc/hydo/hydo.yaml` without its `params:`,
   so the gRPC demo's service `/bin/orderd serve --mode {{mode}}` was started with the literal
   `{{mode}}` and exited ("ERROR service start (orderd)" on every agent run). The image's
   `/etc/hydo/params` (written by hydo-build) now gives the values, applied as hydo-build applies
   them. Test: `agent::tests::params_are_the_images` (the built value, the default, and a manifest
   without `params:` unchanged). The x86_64 stack-grpc and shop examples use `params:` too, so this
   matters for M127's images as well.
3. **`hydo build --control agent` printed that `hydo run` and `hydo explore` cannot drive an agent
   image** (stale since M126); it now prints the same next steps as for a shell image.

`cargo test --locked` (the whole workspace), `cargo clippy --workspace --all-targets -D warnings`
and `cargo fmt --check` pass. The identity corpora and the scripted-engine scenarios are unchanged
(the fixes touch neither the transcripts nor `plan_of_embedded`).

## What C3 still needs on the Mac

- **Busy-wait counter loops under control (D5).** Either a rewind mode that keeps the stop
  numbering pure (stops counted outside the rewound exits), or a documented limit that agent images
  do not run counter-loop busy-waits; until then ledger-spin can only be tested with a shell image,
  and switching the default would lose it.
- **`F=` with `network: bridge` (D4):** the `freeze` op's namespace field (an addition to protocol
  version 1 with a feature name), a new pinned agent build for both architectures, and the
  redis-bridge freeze schedules and a `--sweep freeze` on the Mac then.
- **The voting app's database (D6):** find why the agent image's checks get no answer from
  PostgreSQL after a pause combined with a clock jump; and its seeds 51–100.
- **The mount error's wording (D3):** a decision, because it changes the explorer's signatures of
  the "data disk unmountable" class.
- **The re-baseline** of every Mac fingerprint (the agent images' values above are the first
  ones), the Mac release rebuilt with `guest/hydo-agent` and the second kernel, and its quickstart
  run on agent images.
- **Inputs:** node-kv's and python-kv's fetch scripts need the build machine; on a Mac
  `hydo-nix-fetch` gives the same files (checked against `files.sha256`), and their symlinks must
  be staged too. `hydo-elfdeps --root` does not follow an absolute symlink into another store path
  (python's `libgcc_s.so.1`), so a Mac-only fetch script would need that fixed or worked around.
- **Not run here:** `--sweep freeze` (D4), `cf=`/`X=` (`--more-faults`; hvmx refuses `cf=`), BRANCH,
  several vCPUs and a schedule's own `rb=` under control (refused by the controller, M126), and the
  etcd example (no aarch64 etcd).
