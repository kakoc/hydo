# C2 on the Mac: verdict equivalence of agent images on hvm (Milestone 128)

> Since Milestone 136 (step C3) hydo builds agent images only; the shell init and its extensions
> are deleted. This file is the record of the comparison that justified the switch; the current
> fingerprints of the Mac examples (all 19 images built twice with the same bytes, every recorded
> schedule run twice) are in docs/REBASELINE.md section 8.

Step C2 of `docs/design/guest-control.md` ("Migration", point 2, and "Implementation plan"), for the
arm64 examples on the Mac engine hvm. The x86_64 examples (M127) and the bug benchmark (M129) ran
on the bare-metal machine in parallel and are not in this file.

**Milestone 130** (branch `c2-mac-fixes`) closed two of M128's differences: D5 (busy-wait counter
loops) is resolved by hvm's rewind mode under lock-step control, and D6 (the voting app's
database) is explained: it is a timing difference around two real bugs of the voting app, not a
hydo bug. The voting app's seeds 1–100 ran on both images. The tables below carry M130's values
where they changed; the section "Milestone 130" at the end has the evidence.

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
| redis-bridge | yes (`F=` since M131) | the two freeze schedules were refused on the agent image (D4); since M131 they give the shell image's FROZE points and verdicts, and the freeze sweep is the same on both images except one word of its signature (D7); everything else the same |
| ledger | yes | 7 of 100 swarm seeds, both directions, 39 against 38 failures, the same minimal schedule (packet-loss timing: D1) |
| ledger-spin | yes (since M130) | nothing (swarm 100 of 100, 44 failures each, both classes shrunk); in M128 every agent run ended as SPIN (D5, resolved by the rewind mode under lock-step control) |
| redis-composed (atomic) | yes | 2 of 100 swarm seeds, both lying disk (D2) |
| redis-composed (racy) | yes | 4 of 100 swarm seeds: 2 lost-update races (D1), 2 lying disk (D2); the same classes found and shrunk |
| node-kv | yes | nothing (the SDK's HYDO-ASSERT properties included, after fix 1) |
| python-kv | yes | nothing |
| python-kv-hist | yes, after fix 1 | nothing after the fix |
| rust-kv | yes | nothing |
| shop (buggy and fixed) | yes | nothing; the same 4 failures and the same two minimal schedules as M89/M96 |
| gRPC demo (buggy and fixed) | yes, after fix 2 | nothing after the fix; before it `orderd` did not start on the agent image |
| voting app | yes, apart from timing | recorded schedules the same; exploration (M130: seeds 1–100, process `--new-faults`, with shrinking) 97 of 100, 60 failures on each image, the same two minimal schedules; the 3 differing seeds are M128's (seeds 7, 31, 41), all timing around the worker's death after a clock step (D6, explained in M130) |

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
| redis-bridge | `disk=none \| run F=replica<primary@5#3:2000` | OK `5ab120707c50bcb4` | M128: ERROR no result (refused); M131: OK `10923c154b50fe48`, the same packet frozen (resp #3, `SET k:7 v7`) | yes (M131) | yes (M131) |
| redis-bridge | `disk=none \| run F=primary<workload@5#2:1000` | OK `d51a611fa1a33838` | M128: ERROR no result (refused); M131: OK `f1b963a1d51fb2df`, the same packet frozen (resp #2, `INFO replication`) | yes (M131) | yes (M131) |
| redis-bridge | `disk=none \| run H=redisp@3-6:drop` | OK `1cdc1d26691e912c` | OK `3e381682b8072473` | yes | yes |
| ledger | `disk=none \| run` | OK `41b0e641957da1a6` | OK `9142e77626d9dde0` | yes | yes |
| ledger | `disk=none \| run O=front@5-8:resp:loss,100%` | OK `ce410b59d77057f9` | OK `226636df28deeab1` | yes | yes |
| ledger | `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION `ledger.acked_exactly_once` `631f0ae8508b9565` | VIOLATION `ledger.acked_exactly_once` `b037a0e5cf4b0ff0` | yes | yes |
| ledger | `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION `ledger.acked_exactly_once` `34f01afcf85b9322` | VIOLATION `ledger.acked_exactly_once` `6fb5073b7ad855b2` | yes | yes |
| ledger | `disk=none \| run N=front@0-30:loss,60%` | VIOLATION `ledger.acked_exactly_once` `854a3f0de47b74e5` | VIOLATION `ledger.acked_exactly_once` `e03863089782235a` | yes | yes |
| ledger | `disk=none \| run P=front@10+2` | VIOLATION `ledger.acked_exactly_once` `23cc35358531f39d` | VIOLATION `ledger.acked_exactly_once` `ead725b801c7a3b6` | yes | yes |
| ledger-spin | `disk=none \| run` | OK `12e6a03e6e22595d` | OK `9016421c181fc5ba` (M130, rewind mode; M128: ERROR workload: SPIN) | yes | yes |
| ledger-spin | `disk=none \| run P=front@10+2` | VIOLATION `ledger.no_duplicates` `85e7e22a2d0a0746` | VIOLATION `ledger.acked_exactly_once` `8ca0de67dafd69ac` (M130, rewind mode; M128: ERROR workload: SPIN) | yes | yes (both properties violated on both images; see D1 in M130) |
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
minutes a run with the Mac swapping; M130 ran seeds 1–100 with shrinking, two workers per image), and the deterministic sweeps that apply on the Mac. "Same meaning" counts the
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
| ledger-spin (M130) | `--strategy swarm --new-faults --seeds 1-100` | 100 | 100 | 44 (44 reproduced) | 44 (44 reproduced) | same: `ledger.acked_exactly_once`, `ledger.no_duplicates` | 8 of 8 |
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
| voting | `--strategy process --new-faults --no-shrink --seeds 1-50` (M128) | 50 | 47 | 31 (31 reproduced) | 31 (31 reproduced) | same: none | 8 of 8 |
| voting (M130) | `--strategy process --new-faults --seeds 1-100` | 100 | 97 | 60 (60 reproduced) | 60 (60 reproduced) | same: `votes.counted_soon` (`disk=cut … C=20`, `disk=cut … C=20 P=vote@6+1`); the agent image also shrank seed 7 to `T=7:-2592000 O=db-*@0-1:resp:loss,37%` | 8 of 8 |

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
- **D3, wording only (fixed in M131).** "data disk unmountable" named the mount error differently:
  busybox `mount` (shell init) prints "No error information", the agent's `mount` op reported
  "errno 117" (EUCLEAN). The property and the verdict class were the same; the explorer's
  signatures differed, so the two images listed this class under two signature strings. Since
  M131 the controller writes busybox's own text for the errno (`hydo-control/src/busybox.rs`, with
  the musl table of the image's busybox: the arm64 busybox's musl has no words for EUCLEAN, hence
  "No error information"; the x86_64 one says "Data consistency error"): on Redis
  `disk=lying:7 dseed=b74758aed7322936 | run C=18` both images now give `VIOLATION data disk
  unmountable (mount: mounting /dev/vda2 on /data failed: No error information)` and the
  explorer records one signature.
- **D4, refused by design (open in M124; fixed in M131).** `F=` on a manifest with `network:
  bridge` (redis-bridge's two freeze schedules): the agent's `freeze` had no namespace field, so
  the controller refused the schedule. M131 added the optional `netns` (feature `freeze-netns`,
  hydo-agent 0.2.0, aarch64 `585f0bdb…`): both schedules give the shell image's FROZE points and
  verdicts, each agent run twice identical (table above), and `--sweep freeze --fc-adaptive`
  gives the same 32 trace runs, 208 points and 628 failing runs (`replica.catches_up`) on both
  images, apart from D7.
- **D5, busy-wait counter loops: resolved in M130.** ledger-spin's `spin_count` is a counter
  loop that only hvm's rewind mode resolves (M57). In M128 lock-step control refused the rewind
  mode, so every agent run ended `ERROR workload: SPIN`. Since M130 `serve.c` runs a controlled
  schedule with `spin=rewind` in the rewind mode, and the host runs an agent image's schedule
  again with `spin=rewind` when its run on hvm ends as SPIN, as hvmx does for a shell image's RUN.
  The agent image now gives the shell image's verdicts (section "Milestone 130").
- **D6, the voting app's database after a pause and a clock jump: explained in M130, a timing
  difference (D1) around two real bugs of the voting app.** The check's "db not answering" is not
  PostgreSQL: `psql` answers, but the `votes` table does not exist, because the .NET worker creates
  it only after its first database connection, and `votes_counted` reports every failing `psql`
  as "db not answering". The worker exits on errors its retry loop does not catch: after the
  realtime clock is stepped back 30 days, Npgsql's connection timeout, computed from the wall
  clock, becomes more than 2^31 ms, and `Socket.ReceiveTimeout` throws
  `ArgumentOutOfRangeException`; after a step forward of a year, Npgsql's timeout expires at once,
  and Redis refuses writes (`MISCONF`: its RDB save fails, `/data` is in the read-only root), so
  even a restarted worker exits at its first `LPOP`. hydo restarts dead services once, in the
  heal's restart pass (hydo-init.sh's `restart_dead`, which the controller reproduces). Whether the
  worker dies just before that pass (restarted, OK) or just after it (never restarted, VIOLATION)
  depends on guest timing, which differs between the two images. Details and evidence in section
  "Milestone 130". **Both decisions taken in Milestone 134:** the check now says what it saw ("db
  not answering: <psql's error>", "no vote counted yet: the votes table does not exist", or "too
  few votes counted: <n> of <total> ..."; the exit statuses are unchanged), with the new Mac
  fingerprints in the example's RESULTS.md; and a service can now have a restart policy
  (`services[].restart`, Docker Compose's words, followed by agent images: MANIFEST.md "Restart
  policies"), while the default stays the heal's restart passes. The voting app's manifests keep
  the default, because its Compose file has no `restart:`; with `restart: always` on the worker
  an agent image would start the worker again after it dies after the heal.
- **D7, wording only (found in M131, fixed in M134).** A check that its `timeout` kills: busybox
  ash prints the signal's name ("Terminated") on its error output, which in hydo-liveness.sh's
  monitor is the check's redirected output file, so the shell image's signature ended `(last check
  rc=#: Terminated)` and the agent image's `(last check rc=#: )`. Seen on every point of
  redis-bridge's freeze sweep. Since M134 the controller writes busybox ash's line after the
  output of every check (and composed command) that a signal other than SIGINT or SIGPIPE ended
  (`hydo-control/src/busybox.rs`, `foreground_signal_line`: the redirection is the shell's own
  while it waits, and `waitone` prints `strsignal` and a newline). Proof on this Mac: `hydo
  explore --schedule "disk=none dseed=0 | run F=replica<primary@0#1:150000"` (the sweep's first
  failing point) on M131's redis-bridge images gives one signature on both, `VIOLATION
  [replica.catches_up] not satisfied #.#s after heal, deadline #s (last check rc=#: Terminated)`;
  the same schedule on M131's x86_64 redis-bridge images on the bare-metal machine (hvx, this
  branch's `hydo-explore`, one VM at a time) gives that signature on both kinds too.


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

- **`F=` with `network: bridge` (D4):** done in M131 (the `freeze` op's `netns` field, a new
  pinned agent for both architectures, the redis-bridge freeze schedules and a `--sweep freeze`
  on the Mac).
- **A check killed by its timeout (D7):** done in M134 (the shell's "Terminated" line in the
  check's output, written by the controller).
- **The voting app's check (D6):** done in M134 (the check names what it saw; restart policies
  for services that must come back after the heal).
- **The mount error's wording (D3):** done in M131 (busybox's own text from the agent's errno).
- **The re-baseline** of every Mac fingerprint (the agent images' values above are the first
  ones), the Mac release rebuilt with `guest/hydo-agent` and the second kernel, and its quickstart
  run on agent images.
- **Inputs:** node-kv's and python-kv's fetch scripts need the build machine; on a Mac
  `hydo-nix-fetch` gives the same files (checked against `files.sha256`), and their symlinks must
  be staged too. `hydo-elfdeps --root` does not follow an absolute symlink into another store path
  (python's `libgcc_s.so.1`), so a Mac-only fetch script would need that fixed or worked around.
- **Not run here:** `cf=`/`X=` (`--more-faults`; hvmx refuses `cf=`), BRANCH,
  several vCPUs and a schedule's own `rb=` under control (refused by the controller, M126), and the
  etcd example (no aarch64 etcd).

## Milestone 130: busy-wait loops on agent images, and the voting app's database

Branch `c2-mac-fixes`, on this Mac, with M128's images and a prefix with this branch's `hydo`,
`hydo-build`, `hydo-explore` and `hvmx`. Two workers per image for the voting app (3 GiB guests),
two for ledger-spin.

### Rewind under lock-step control (D5)

**How it works.** hvm's rewind mode (M57) takes a copy-on-write checkpoint of the vCPU, the
machine state and the pages written since the last exit, at the loop top of every exit. When the
spin observer proves an exit-free counter loop, the engine rewinds to that checkpoint and
re-executes to the loop's canonical point, where it delivers the pending interrupt. Under
lock-step control this keeps the stops pure, because a rewind never moves the exit count: the
checkpoint is taken after the stop hook of its exit, and the guest makes no exit between the
checkpoint and the rewind (the loop is exit-free), so the rewind restores the same exit count,
console and queued input, and the hook, which stops at most once per exit count, does not stop
again. M128's reason for refusing it ("it moves M.exits backwards") did not hold. The real
obstacle was the automatic re-run: hvmx runs a shell image's schedule plainly and runs it again
in the rewind mode only when it ends as SPIN, and it cannot do that under control, where the
host's conversation has already happened. So the choice moved to the host:
- `serve.c`: a CTL, a RESUME and `--control boot one` whose schedule has `spin=rewind` (or
  `HVM_SPIN_REWIND=1`) run the workload, check and recovery phases in the rewind mode (the boot of
  `--control boot one` stays plain, as a worker's boot does); a SNAP keeps the mode for its RESUME.
- `hydo-control` (`agent::rewind_again`): when an agent image's run on hvm ends as SPIN and its
  schedule has no `spin=rewind`, the worker (from S1) or `hydo run` (a new fresh boot) runs the
  schedule again with `spin=rewind` and reports only that run. `HVM_SPIN_REWIND=0` turns it off.
  The transcript's header names `spin=rewind`, so a transcript replay is in the rewind mode.

**Proofs.**

| check | result |
|---|---|
| ledger-spin agent `disk=none \| run`: fresh boot twice, from S1 twice (explorer worker) | OK `9016421c181fc5ba`, stops 617, `inh=9f8ab84c8416601a`, `ctl=f9c60fb418e129fa` all four times; the four transcripts byte-identical |
| ledger-spin agent `disk=none \| run P=front@10+2`: the same | VIOLATION `8ca0de67dafd69ac`, stops 819, `inh=f69ca8f184cea50c`, `ctl=2fb68165f0b33af8` all four times; transcripts byte-identical |
| `hydo repro --transcript` of both | REPRODUCED, the same commands at the same stops |
| a plain schedule in the rewind mode on agent images (fingerprint, stops, `inh=` as the plain run; `ctl=` differs only by the header) | Redis `disk=none` `251aa0074e7e7642` stops 388; Redis `disk=cut C=12 R=everysec` (with its recovery) `2ba69ae149ad2ed0` stops 341; ledger `disk=none` `9142e77626d9dde0` stops 335; ledger `P=front@10+2` `ead725b801c7a3b6` stops 558 |
| every rewind restores the exit count it was proven at (`HVM_SPLOG`, ledger-spin `P=front@10+2`) | 87 rewinds, 87 to a checkpoint at the same exit count |
| `lockstep-check.sh` with ledger-spin's shell image as fourth argument | every M116/M119 value, CTL = RUN; `spin=rewind` schedules CTL = RUN = fresh boot: `12e6a03e6e22595d`, `85e7e22a2d0a0746` (with M128's hvmx the CTL gives `fp=0`: the group fails) |
| M128's recorded values with this branch's programs | Redis plain and cut (shell and agent), ledger, node-kv's cut, the buggy shop's dropped answer, the racy composed plan, redis-bridge's link fault: the same fingerprint, stops, `inh=` and `ctl=`; ledger-spin's shell runs `12e6a03e6e22595d` and `85e7e22a2d0a0746`; the voting app's seeds 1–50 on both images and ledger-spin's shell seeds 1–100: the same fingerprint and `ctl=` for every seed |

**ledger-spin's verdicts.** Recorded schedules: `disk=none | run` OK on both images;
`P=front@10+2` VIOLATION on both, with both properties violated on both images (`ledger.no_duplicates`
and `ledger.acked_exactly_once`); the verdict line names the first one found, which differs (D1
timing): on the shell image the periodic `no_duplicates` check ran between the workload's retry
(which stores entry 10 again) and the workload's end, on the agent image it did not, so the final
`acked_exactly_once` check is the first to see the duplicate. Swarm `--new-faults` seeds 1–100:
100 of 100 with the same meaning, 44 failures on each image (44 reproduced each), the same two
failure classes shrunk on both (`P=front@…` pauses: shell `P=front@22~422:10963` and
`P=front@6~0:2417`, agent `P=front@22~0:2740` and `P=front@26+2`), the property table 8 of 8.

### The voting app's database (D6)

The three differing seeds are M128's (7, 31 and 41, with M128's fingerprints). What happens, from
runs with diagnostic checks (an agent image driven with an extended check, and a shell image built
from a copy of the manifest whose check also prints the worker's log; neither is committed):
- The check's "db not answering" is not PostgreSQL: `psql -c 'select 1'` answers, and the query
  fails with `relation "votes" does not exist`. The .NET worker creates that table only after its
  first database connection, and the manifest's `votes_counted` reports any failing `psql` as "db
  not answering".
- **Seed 7** (`P=db@18+2 T=7:-2592000 O=db-*@0-6:resp:loss,74%`): the worker's first connection
  is still being opened while the database's answers are lost; the clock is then stepped back 30
  days. Npgsql's timeout was computed from the wall clock, so the remaining time becomes 30 days,
  more than 2^31 ms, and `Socket.set_ReceiveTimeout` throws `System.ArgumentOutOfRangeException`,
  which the worker's retry loop (it catches `SocketException` and `DbException`) does not catch:
  the worker exits with status 1. Both images: the same exception in the worker's log. On the
  shell image it exits before the heal and the heal's restart pass restarts it (OK). On the agent
  image its pending read is answered only when the paused database continues, 11 ms of guest time
  after the restart pass had seen it alive, so it is never restarted and no vote reaches the
  database (VIOLATION). The agent image's exploration shrank this to `T=7:-2592000
  O=db-*@0-1:resp:loss,37%` (no pause needed there); from a fresh boot that schedule gives OK `e2807227ec1f84df` on
  the shell image and VIOLATION `votes.counted_soon` `525026bff2a7347b` on the agent image (twice,
  stops 9922, `ctl=adf022ccf023fb23`).
- **Seed 31** (`P=db@1+5 T=18:31536000`): after the clock is stepped forward a year, the first
  worker exits on an exception from Npgsql's timeout check (`NpgsqlTimeout.Check`), and the worker
  restarted in the heal's pass exits too: Redis refuses writes (`MISCONF … unable to persist to
  disk`, its RDB save fails because `/data` is in the read-only root), and the worker's `LPOP`
  throws. Both images: VIOLATION `votes.counted_soon`. The final `votes.counted` differs only in
  whether the restarted worker created the table before it died (shell: "voter0 acknowledged but
  not counted"; agent: "db not answering", which counts as "cannot decide").
- **Seed 41** (`K=db@8+1 P=redis@18+2 T=2:-30 O=redis-*@10-15:req:loss,61%`, shell VIOLATION,
  agent OK) differs in the other direction. It was not run with diagnostics; its faults (a
  database kill, a Redis pause, a 30 s clock step) point to the same kind of timing race around
  the worker's death and the heal's one restart pass.

So the difference is timing (D1) around two real bugs of the voting app (the worker dies on a
clock step and nothing restarts it after the heal, as under Compose without `restart:`; Redis
refuses writes after a forward clock step). The controller does what hydo-init.sh does; nothing
in hydo was changed. Two decisions are listed under "What C3 still needs".

### The voting app's seeds 1–100

`--strategy process --new-faults --seeds 1-100`, with shrinking: 97 of 100 seeds with the same
meaning, 60 failures on each image, every failure reproduced on both, the property table 8 of 8.
Seeds 1–50 gave M128's fingerprints on both images; seeds 51–100 all have the same meaning. Both
images shrank to the same two minimal schedules (`disk=cut dseed=4127cc1347fae6ba | run C=20` and
`disk=cut dseed=978180e49ee2ae24 | run C=20 P=vote@6+1`, both `votes.counted_soon`: the app
acknowledges votes held in a Redis list without persistence, M84's finding); the agent image also
shrank seed 7 (above).
