# Re-baseline after the hvx_seedrng stand-in (2026-10-21/22)

## How to re-baseline (since Milestone 164)

The recorded values of every example run are in **`hydo/onboard/recorded.json`**, not in this
file: per image how it is built from the tree, its sha256, and per schedule the verdict, the
fingerprint and (where recorded) ctl and wfp, for the platforms `hvx` (x86_64, the bare-metal
machine), `hvm` (arm64, a Mac) and `kvm`. It was generated once from this file's latest tables
(13.1–13.4, 14, 15, 16, and docs/HISTORY.md Milestone 159 for KVM); the format is described in
`hydo/crates/hydo-bench/src/recorded.rs`. This file keeps the history of how each re-baseline was
made (sections 1–14 below); a new re-baseline adds a short section here only when it decides
something (a new epoch, an accepted verdict change).

One command per platform, on the machine of that platform (docs/HISTORY.md "Milestone 164"):

    # the bare-metal machine (. ~/hydo-rust/env.sh first), or a Mac:
    cargo build --release --locked -p hydo -p hydo-hvx -p hydo-build -p hydo-compose -p hydo-bench -p hydo-grpc-gen
    cargo build --release --locked -p hydo -p hydo-hvm -p hydo-build -p hydo-compose -p hydo-bench -p hydo-grpc-gen -p hydo-tools
    # then, from the tree:
    target/release/hydo-bench rebaseline --release <an unpacked hydo release of the platform> \
      --work <a work folder> [--jobs 2] --check        # or --write

It stages each image's inputs (`hydo/onboard/rebaseline/stage.sh`), builds each image twice from
the tree, runs every recorded schedule twice from fresh boots and writes `<work>/report.md`: first
the verdict changes, the runs whose two repetitions differ and the images that did not build twice
to the same bytes, then every run's recorded and new values. `--check` writes nothing and exits 3
when only values moved; `--write` writes the new values into the list (a run whose verdict changed
only with `--accept-verdicts`); exit status 1 always means a verdict change, a nondeterminism or a
failed step. `--only <image or stage>,...` limits it; `--changed-only` runs only the images whose
bytes changed (enough when only image inputs changed; after an engine, controller, agent or kernel
change run everything, and raise `hydo_image::EPOCH`); finished runs are cached in the work folder,
so an interrupted rebaseline resumes. After `--write`, review the list's diff and commit it with
the change that moved the values. What the stages need (Go 1.26.5 as `HYDO_GO`, the network once
for the downloads they check by sha256, on Linux the pinned Nix store paths and `sudo -n setpriv`
for the uid 1000 builds) is in `stage.sh`'s comments.

> Note (M107): `explore.py` and `hydo_build_arm64.py` named below were the Python explorer and
> arm64 builder of the time, replaced by the hydo-explore and hydo-build crates (`hydo explore`,
> `hydo-build`, the same options) and deleted in M107; the commands below are records of how the
> baselines were measured.
>
> Note (M108): `hydo/bench/run.py` and `confirm.py` are now `hydo-bench run` and `hydo-bench confirm`
> (`hydo/crates/hydo-bench`); `hydo/richcov/` (with `build-richcov.sh`) and the explorer's research
> strategies (`richguided.py`, `abranch.py`) were deleted in M108 and can be restored from commit `705cf98`.

From 2026-10-17 until 2026-10-21 the build machine had no pinned static musl gcc
(`aa1jsl19…-gcc-static-x86_64-unknown-linux-musl-13.2.0`). Image builds then used a stand-in
`hvx_seedrng` binary, sha256 `01adf1bd67cbc79f…` (36,280 bytes), copied from
`~/hydo/build/hvx_seedrng`. Nobody can rebuild that binary (`docs/BUILD-PINS.md`). The pinned
gcc is restored, and with it `hv/hvx/images/hvx_seedrng.c` gives `c2a440c5e1c3888f…` (40,272 bytes).
The decision was to re-baseline: rebuild every image that contains the stand-in with the
pinned tools only, and record the new image hashes and fingerprints. This file is that
record. CLAUDE.md is not changed here.

Contents: 1. Inventory. 2. How the images were rebuilt and verified. 3. Old → new image
hashes. 4. Old → new fingerprints, one table per image. 5. Verdict changes. 6. Paths.
7. Rebuild after the netem drain fix (M77, 2026-10-23): the `-nq` images. 8. The switch to agent
images (M136, 2026-11-01): every manifest image's new values, on both engines.

## 1. Inventory

Method: for every `*.img` and `*.cpio.gz` in `~/hydo/build` on the bare-metal machine
and on the build machine, the file `hvx_seedrng` was extracted from every
partition (`debugfs` dump of `/bin/hvx_seedrng`, `/usr/bin`, `/sbin`, `/oci/*/bin`) or from the
initramfs (cpio walk), and hashed. No image was judged by its date.

**Contain the stand-in (`01adf1bd…`), bare-metal machine `~/hydo/build`:**

| image | rebuilt here? | where it is used in the docs |
|---|---|---|
| `explore-nf-disk.img` | yes | CLAUDE.md M46, M54, M56, M58; `hydo/bench/README.md` (control image, `e5d8d6fb…`, base bug `f1c39a88…`); `hydo/cutflush/FINDING.md` (cf=204 `e592e6c6…`, 12 cut points) |
| `explore-bench-d1…d7-*-disk.img` (7) | yes | CLAUDE.md M54; `hydo/bench/README.md` defect table; `hydo/bench/run.py`, `confirm.py` |
| `explore-bench-bg-control`, `-bg-d0…d7-*` (9) | yes | CLAUDE.md M56; `hydo/bench/README.md` BUGGIFY section; `confirm.py --bg-images` |
| `explore-bench-bg-fixed-disk.img` (`7974498b…`) | yes | `hydo/bench/README.md` (BUGGIFY fixed front door); `hydo/cutflush/FINDING.md` (cf=242 `5c04f271…`, 13 cut points); CLAUDE.md M58 |
| `explore-ob-redis`, `-stack-fix`, `-stack-old` | yes | CLAUDE.md M47; `hydo/onboard/RESULTS.md` (M47 tables) |
| `explore-ob-redis-pt`, `-stack-fix-pt`, `-stack-old-pt` | yes | CLAUDE.md M52; `docs/BUILD-PINS.md` (`0dd509c2…`, `37a2ff6a…`) |
| `explore-ob-miniflux`, `-miniflux-pt` | yes | CLAUDE.md M52; `hydo/onboard/examples/miniflux/RESULTS.md` |
| `explore-ob-miniflux-tree` | yes (byte-identical to `-pt`, old and new) | not in the docs |
| `explore-ob-kb-sqlite-3510200`, `-3510300` | yes | CLAUDE.md M49, M52; `hydo/knownbugs/sqlite/README.md` (`05d75b28…`, `2ff12b40…`); `hv/hvx/race/PREEMPT.md` |
| `explore-ob-redis-live` | yes | CLAUDE.md M59; `hydo/onboard/RESULTS.md` (liveness, `558a256b…`) |
| `explore-ob-redis-oci` | yes | CLAUDE.md M59; `hydo/onboard/RESULTS.md` (OCI, `ca3ea2b0…`) |
| `richcov-fix-disk.img`, `richcov-old-disk.img` | yes | CLAUDE.md M44, M45 (`cfc71a50…`, `059021d7…`); `hydo/richcov/DESIGN.md`; `hydo/explore/richguided.py`, `abranch.py` |
| `explore-dx-disk.img` | no | not in the docs (see below) |
| `explore-cc0`, `explore-cc1`, `explore-cc` | no (other session) | CLAUDE.md M50 (see below) |
| `explore-ob-etcd-3.5.5/3.5.6/3.6.8/3.6.9/3.6.15` | no (etcd work in progress) | not yet in the docs |
| `explore-binlog-disk.img`, `explore-binlog-old-disk.img`, `stack-binlog-disk.img` | no: rebuilt by the other session | the other session's MySQL binlog fix |
| `explore-bench-m59-control`, `-m59-d8…d12` (6) | no: rebuilt by the bench-more agent | the benchmark extension |

**Build machine `~/hydo/build`:** `richcov-disk.img`, `richcov-fix-disk.img`, `richcov-old-disk.img`
contain the stand-in. The last two are the same bytes as the bare-metal copies above. The
build machine's `richcov-disk.img` is the copy that CLAUDE.md M44 says was rebuilt with a
substitute gcc ("rebuild the build machine copy before using it"); the bare-metal copy of
`richcov-disk.img` (`9d1e432c…`) contains the pinned binary and is not affected. Not rebuilt.

**Not affected (contain `c2a440c5…`):** `explore-disk.img` and `explore-fix-disk.img` (the two
pinned reproducer images, as expected), `explore-sdk`, `explore-ptfix`, `explore-ptold`,
`sdk`, `stack`, `pt-stack`, `richcov` (bare-metal copy), `kafka`, `kafka-disk-ext2`, `mysql`,
`jvm`, `spread*` (build machine), `initramfs-redis.cpio.gz`, `initramfs-system.cpio.gz`. All
other images and initramfs files contain no `hvx_seedrng` at all.

**Not rebuilt, and why:**
- `explore-cc0`, `explore-cc1`, `explore-cc` (M50). Agreed with the other session: their
  recorded results (frontkill 25/40 and 0/40, `hvx_stack_verify` passing) depend on the order
  service code, not on the bytes of `hvx_seedrng`. `explore-binlog-pin` replaces `explore-cc`
  for current measurements. If they are needed again, rebuild them from commit `029e4b3`
  (cc0) and `b9233d5` (cc1) with `hydo/stack` at those commits.
- `explore-dx-disk.img`: a diagnostic image of M50 (built 2026-10-18 09:22 UTC, used once in
  `/var/tmp/dx1`). No document refers to it and its source was never committed.
- `explore-ob-etcd-*`, the binlog images and the `explore-bench-m59-*` images belong to work
  in progress in other sessions, which rebuild them with the pinned tools themselves.
- arm64 (Mac) images: not in either `~/hydo/build`; `hydo-build --arch arm64` (then `hydo_build_arm64.py`) adds
  `hvx_seedrng` only when `HYDO_SEEDRNG_ARM64` is set. Not inventoried.

## 2. How the images were rebuilt and verified

Every image was rebuilt from the source it was originally built from, in a private directory
on the build machine (`~/hydo-rebase`, deleted afterwards), with `HYDO_GCC_MUSL`,
`HYDO_MUSL_ROOT`, `HYDO_REDIS_OUT` and `HYDO_SEEDRNG` unset. `hvx_seedrng` was compiled by
the pinned gcc (`c2a440c5…` in every build). Redis came from the pinned store path
`ky1xq6xa…-redis-static-…-7.2.4` (byte-identical to `~/hydo/build/redis-*`).

| images | source | build command |
|---|---|---|
| `explore-nf` | commit `15cc5a4` (identical to the tree `~/hydo-faults` it was built in) | `STACK_OUT=explore-nf bash hv/hvx/images/build-stack.sh` |
| `explore-bench-d1…d7` | commit `91312b2` (identical to `~/hydo-bench`, the M54 build tree, except for 3 abandoned patches) | `bash hydo/bench/build-bench.sh` |
| `explore-bench-bg-*` (9) | commit `3773607` | `bash hydo/bench/build-bench-bg.sh <name>` |
| `explore-bench-bg-fixed` | commit `3773607` (its `hydo/stack/main.go` already calls the BUGGIFY wrappers) | `STACK_OUT=explore-bench-bg-fixed bash hv/hvx/images/build-stack.sh` |
| `explore-ob-redis`, `-stack-fix`, `-stack-old` | commit `0360a16` | `hydo-build examples/redis/hydo.yaml --out explore-ob-redis`; `FRONT=fix|old bash hydo/onboard/examples/stack/build.sh` |
| `explore-ob-*-pt` (redis, stack-fix, stack-old) | commit `78f2ed4` | the same with `--out explore-ob-<name>-pt` |
| `explore-ob-miniflux` | commit `3063f23` | `hydo-build examples/miniflux/hydo.yaml` (miniflux 2.3.3 rebuilt from `~/mf-src/v2`: `dd8e2456…`, as recorded) |
| `explore-ob-miniflux-tree`, `-pt` | commits `f9e9e6c` and `7d46a1b` | the same, `--out …-tree` / `…-pt` |
| `explore-ob-kb-sqlite-3510200/3510300` | commit `fe97d2c`, walrace binaries from `~/hydo-sqlite/hvx/knownbugs/sqlite/bin` on the bare-metal machine | `hydo-build hydo/knownbugs/sqlite/hydo.yaml [--set v=3510300]` |
| `explore-ob-redis-live` | commit `af1c873` | `hydo-build examples/redis/hydo-live.yaml --out explore-ob-redis-live` |
| `explore-ob-redis-oci` | commit `130941f` | `fetch-image.sh` (with `SKOPEO=` set, because this commit has the skopeo-path bug fixed later in `b3e2f5a`), then `hydo-build examples/redis-oci/hydo.yaml` |
| `richcov-old`, `richcov-fix` | commit `1986481` | `RICHCOV_VARIANT=old|fix bash hydo/richcov/build-richcov.sh` (also rebuilds the debug Redis with the pinned gcc; it came out byte-identical) |

The manifests' `~/hydo/build/…` inputs (Redis, prebuilt front doors) were read from a copy
under a private `HOME`, as `hv/hvx/images/pins-proof.sh` does. No build script was changed.

**Verification that the source is right.** For every rebuilt image, the file tree of every
partition (path, type, mode, size, sha256 of every file; ext4 via `debugfs rdump`) was
compared with the old image. **In all 33 images the only difference is `/bin/hvx_seedrng`**
(`01adf1bd…`, 36,280 bytes → `c2a440c5…`, 40,272 bytes). The data partitions are identical.
So each image was rebuilt from exactly the source it was built from, and the only change is
the binary.

**New images** are on the bare-metal machine in `~/hydo/build` under the old name with `-pin`
inserted before `-disk.img` (for example `explore-nf-pin-disk.img`). The old images were not
changed.

## 3. Old → new image hashes (sha256)

| image | old sha256 | new image (`-pin`) sha256 |
|---|---|---|
| `explore-bench-bg-control` | `cc8546d83a51a77c1f860f871b22b980af0cb2e54bd99d5d8d37a5b3e848efe2` | `b0e09677c7e9254f4dc025466dd5d08fba7b2de00057da32e6171be0161cb22a` |
| `explore-bench-bg-d0-old-front-door` | `66271977129bfb214799f2051d8abfaa5f6ec6c6232969c2fa3c6c2e0317d6b7` | `5f4c8aa38356a27c3bb77db1df3738e34f8fdcc6a442a19221717d32d6315374` |
| `explore-bench-bg-d1-kafka-first` | `89f73c992209452d24dcd039abeef7d2639c7cce5a10ba5a8bfebd990badb0ae` | `4fb76ec9ef438a218ea08f579056554846e6fc4ba5e93bffa7805e3c4835b03a` |
| `explore-bench-bg-d2-batched-publish-flag` | `4f868a06bf4d5d88d6af70305e075349be8f082b1f82f27326423057c5c6e8c5` | `18a22d30055a2052771f763de71872ce65da421d7ba3b255e909b9a4cd991768` |
| `explore-bench-bg-d3-blind-retry` | `ce926666498b353c2a84e30f8ecf50e4d531faf6be68310d818c824c0203b6e8` | `19174f2b4ad943c9222ebd8214edc7755823dae908159e49e129fae04328deee` |
| `explore-bench-bg-d4-produce-timeout-definite` | `f90530a550314b43e341db912bebe2ae0e61266536b1bf0aeb4fe467bf536ae7` | `e75caf8b48fcf70dc5aad319f0dd88cf2eb54d15078783495c98519328eafbf2` |
| `explore-bench-bg-d5-myisam` | `673e6104e5b6253701c7b677d28bc89ed59094b0d0aa7e1e0caf4bc628ae1e3d` | `eebec1c9d1adb34c03b5f19c24e06215fd936e9f3f649f92c5e6c35c80ec44bd` |
| `explore-bench-bg-d6-client-deadline-definite` | `717e6601fcb1cc76545c8c2d088608f252cdb362096adbc7377da6bcafdbb6ae` | `aaa249c625c088d1ab1564630319d8c0f787cbf7e244fa5099f5e8797de5acb8` |
| `explore-bench-bg-d7-skip-last-record` | `b3065b5f9d6bc7240f492425e78469e84285ac990e4054ee4a9c92e01410c292` | `7d6814818b8a1c3cea722a130ab2efd48c49abc4dcde9f26a9870ca3f5e3e893` |
| `explore-bench-bg-fixed` | `7974498b8802ad4a8222600865a22c13d193db80a1bc1970633c87e60c4e2261` | `51980aadc9770eacd0c78defb959c992dbcf32e277f2b47cc61a14c1a5c42c95` |
| `explore-bench-d1-kafka-first` | `67f468af957ccc9bf38c36d183e0aee18619c3528fbe1cbacf58977beb9ec3f9` | `8461e70b137c2369a23e43feee0aad01aafb0ee9e8d49de6b1ea1da0e740a43a` |
| `explore-bench-d2-batched-publish-flag` | `73d2f5433fe9322a2d777d1abb0e2582351c5ce34e321fb94741537741091c74` | `3df67584cbe8d2a84fdb2d243ea24d3c41c7cf6b07dc8b20ade89f1d284bf582` |
| `explore-bench-d3-blind-retry` | `fc0449bbe13f1ed69244d0f19c8a410fcff935d10b01d4f38b76c1d371116ec7` | `e18d3b9d4c2869214d9452b580cddd4e425a7636bd52dc218fbf51185e61a222` |
| `explore-bench-d4-produce-timeout-definite` | `482b8ce0c4811f78628af8d04b3d74e97ac4ba683bf5c4c0d9e1dda7cd0c12fb` | `44ff3150a10985d474daeb049154415b591395824640b7c5497ba8091c10acad` |
| `explore-bench-d5-myisam` | `b16c3bb1f76997fbc6cd4b8cc21bd14d04c3856181eb03d141dafd21de08630f` | `661b1f39b8eb823b9563fff3a2308a5f65cfcb541b9b6c895f7f294bb0bdcfb3` |
| `explore-bench-d6-client-deadline-definite` | `586cbc2688818ee49e0514bdd1504de03cfb6483a63b79cc51335a0c5f5dbdb0` | `58f214783a4ad1c1a7ec1a3422f02ad603bc7bd270a2481f30b9f8dcc6ef0be9` |
| `explore-bench-d7-skip-last-record` | `cc8c191bbab804971486b6e96872ce73f7a6c4130641fef3aca65574149cab6c` | `a816f0265bf5dd29710f90283d0d13e046349bed98113a711cbb31095abe7cf2` |
| `explore-nf` | `e5d8d6fb1bc0302b53caedcaf6db50e6c8edb1db54d2ff82a19200da5842efd7` | `2ba9c1a1c150515684c700fb1538bea5ed3e7d2ae2c3619ff356324223e013c7` |
| `explore-ob-kb-sqlite-3510200` | `05d75b284058df7ca63081ed6dc0e332b2f2e88ee5627bad66db2f461624a2e0` | `a8d38894a8dd5c05b453abd795ebbb2e0c865f60642471707535acaafa4f729c` |
| `explore-ob-kb-sqlite-3510300` | `2ff12b4061601dd19a72b0341b4a1df097ad8605abe7c8ca8cf27725ee30536f` | `d3e2c47126e2e4f00f4f46122ba0ad3a2015b0e09479c6fbe7d473b434e996ca` |
| `explore-ob-miniflux` | `b3409055c3e8cb3979fd88fa0aac5f649c9ec960dbdec7851fc5f95b2b200d2b` | `5d6cfaf74e59679b9ba2459e2db4d2670d3096475ee9f787729a186bf1af6420` |
| `explore-ob-miniflux-pt` | `19518573474e36bf99650f9c66211ccb69cd568c1d4317c7aff0be6e5d280ddb` | `cc3e6534145663d92fa99d879412e26b2a309fe95d340738b5656285c70318ec` |
| `explore-ob-miniflux-tree` | `19518573474e36bf99650f9c66211ccb69cd568c1d4317c7aff0be6e5d280ddb` | `cc3e6534145663d92fa99d879412e26b2a309fe95d340738b5656285c70318ec` |
| `explore-ob-redis-live` | `558a256be8028999c58b30bef29ee5bc7398fdb193d2e29b55ff23dfc35b8614` | `dc466b43c6e72307f151fd57915731406fe84a85903d996b362746b6f7b5061a` |
| `explore-ob-redis` | `ad5d68d5c5130d6ceaa7ed5f9d3d4529028d1077dd3b48ed50edc9e31cb09bb4` | `a4b12789988fce1b5604723287f7776156fa127a4bf6f093584edb296790cfbe` |
| `explore-ob-redis-oci` | `ca3ea2b01a305b1bf626e823c4bec8ee48539c7fe7858344d72ace2cc38b4dec` | `4fefa3546ebf949982d1997accbbf4d1d98930e057e70190229e49e6d6459f45` |
| `explore-ob-redis-pt` | `0dd509c29fff89f190efdb76f434dd11662ed11344240706a9dc6c8e8399830a` | `4eda84fe75909c2147b384efd0da83963138ca0a42c0c3917941f14a2e78855b` |
| `explore-ob-stack-fix` | `8ae8288b76e5665870a992ed5759da10edf5cf6137c6b864528b57d3f975b037` | `9b57598e510fb00cadaded98d4b13c0e0d6e2c9a5e0b73697d2540966949ada0` |
| `explore-ob-stack-fix-pt` | `37a2ff6acd4778ec72d06a9a5c93f1cea935be1138585449e461868a32b09b19` | `ac5553ead6d49fef7d2154dc11c87f363082fd7cde7263ed6836acdf0c7220b2` |
| `explore-ob-stack-old` | `c1906d2d833f38699eab40ef4df70a5f3c90863eb738e1682a572fbae4a12fff` | `f8d65229bb98bc8b33c284c704fc6a1f24af5a3e51850b39359b697e260795ac` |
| `explore-ob-stack-old-pt` | `0b526e0246323b6a9ab8a3bc2a8880f5a8d5fcf27fc710fb2cb53763ced86e99` | `0920ec729f80755e43fadc61e12e2567e77ab449f17ac80d07a1d473c4f9632e` |
| `richcov-fix` | `cfc71a50e887d5d84ec243ec3f5585a725985914c7e89c5e42e600eaee2f1385` | `5c53fccf377647d0d45dcc06b5bf11810c66e6e4570d2ac2fc61f9196567fec8` |
| `richcov-old` | `059021d7c16ee6065184f14dc4ca4fbbdff7469da9a38a7d443f34f9777299f1` | `bcf1969b07b075e619b2ba357fc8c2ed9b3b5eb0680bdc03056bfa99c7a6691d` |

`explore-ob-miniflux-tree` and `explore-ob-miniflux-pt` were byte-identical before and are byte-identical after (old `19518573…`, new `cc3e6534…`), so the checks below were run on `-pt` only. Every new image was copied to the bare-metal machine and its sha256 checked there.


## 4. Old → new fingerprints

Checks run on the bare-metal machine with the current `main` explorer (`hvx_explore` built from
`26630ee` in `~/hydo-rebase`, serve mode, 1 worker, `~/bin/hvx-run`, module
srcversion `38E88060EEC5F955DFDFD03`, not reloaded), 2026-10-21 19:43 to 2026-10-22 03:54 UTC.
Each listed schedule ran at least twice on the new image, and all runs of a schedule gave the
same fingerprint and verdict (204 schedule/image pairs, 359 runs, no exception). The only
single runs are the flush-cut sweep points that passed and were not recorded before; they are
listed below each table in one line. "Old image re-run today" is a run of the OLD image
during this work: done where a verdict changed (it reproduced the recorded fingerprint every
time, so the change comes from the image, not from the module or the explorer) and where no
old fingerprint was recorded (richcov, kb-sqlite 3.51.3). Recorded fingerprints and where they
come from:

- `explore-nf` (control): `/var/tmp/m46-bench/confirm/confirm.jsonl` on the bare-metal machine (only the base
  bug `f1c39a88…` is in `hydo/bench/README.md`); flush-cut points: `hydo/cutflush/FINDING.md`
  (cf=204) and `/var/tmp/m57-cfcheck/nf-rerun/results.txt`.
- `explore-bench-d*`: `hydo/bench/README.md` defect table. `explore-bench-bg-d*`:
  `hydo/bench/README.md` BUGGIFY section; `-bg-control`: `/var/tmp/m56-bg/confirm/confirm.jsonl`.
- `explore-bench-bg-fixed`: FINDING.md (cf=242) and `/var/tmp/m57-cfcheck/fixed-rerun/results.txt`.
- `explore-ob-redis`, `-stack-fix`, `-stack-old`: `hydo/onboard/RESULTS.md` (M47); M42 schedules
  `/var/tmp/m47-onboard/m42.txt`. `-pt` images: CLAUDE.md M52 and `/var/tmp/m48-miniflux/batch3.txt`.
- miniflux: `hydo/onboard/examples/miniflux/RESULTS.md`. Liveness and OCI: `hydo/onboard/RESULTS.md`.
- kb-sqlite 3.51.2: `/var/tmp/m48-sqlite/hunt4/runs.jsonl` (run 1 is the README's `e75e8480…`).

The column "old image re-run today" shows `-` where it was not re-run.

#### `explore-nf`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `baab065f6561ffc6` | - | b7584694892dccbd / b7584694892dccbd | OK acked=24/30 exactly-once |
| `disk=none dseed=0 \| run K=mysql@10+1` | `ca32d175302d966e` | - | 394f2663713e15fd / 394f2663713e15fd | OK acked=30/30 exactly-once |
| `disk=cut dseed=0 \| run C=12` | `7be5c70f34a90b1c` | - | 850f66ebd981e7fe / 850f66ebd981e7fe | OK acked=12/30 exactly-once |
| `disk=none dseed=0 \| run P=kafka@5+2` | `808fdee0591b8e8d` | - | 925b7892ba922ff8 / 925b7892ba922ff8 | OK acked=30/30 exactly-once |
| `disk=none dseed=0 \| run O=kafka@5-10:resp:loss,100%` | `a7fed0d6f18c036f` | - | b358b399659f1965 / b358b399659f1965 | OK acked=30/30 exactly-once |
| `disk=cut dseed=0 \| run O=kafka@5-10:resp:loss,100% C=12` | `373c12e642c16a9f` | - | 4a1f84e80f484822 / 4a1f84e80f484822 | OK acked=7/30 exactly-once |
| `disk=cut dseed=0 \| run P=kafka@5+2 C=8` | `d6fe5546eeb64f8a` | - | aca6ee5ea5a0813b / aca6ee5ea5a0813b | OK acked=6/30 exactly-once |
| `disk=cut dseed=0 \| run C=15` | `f7280df83bd7230e` | - | b2182f88d334f043 / b2182f88d334f043 | OK acked=15/30 exactly-once |
| `disk=none dseed=0 \| run P=kafka@28+2` | `fd0957f2021ced32` | - | b187d6e58558ec26 / b187d6e58558ec26 | OK acked=30/30 exactly-once |
| `disk=none dseed=0 \| run O=kafka@25-30:resp:loss,100%` | `f1c39a886803a8fa` | - | 1834ffcb722d048c / 1834ffcb722d048c | VIOLATION kafka order=26 present 2 times (must be exactly once |
| `disk=cut dseed=0 cf=204 \| run` | `e592e6c6fb6e5f3a` | e592e6c6fb6e5f3a (= recorded) | a408a0f1ca1d8316 / a408a0f1ca1d8316 (3 runs, all same) | OK acked=22/30 exactly-once |
| `disk=cut dseed=0 cf=205 \| run` | `e43ea8d9dd0fa934` | - | f6e6ddaf354a49f3 / f6e6ddaf354a49f3 (3 runs, all same) | OK acked=22/30 exactly-once |
| `disk=cut dseed=0 cf=213 \| run` | `212ee656ad19689e` | 212ee656ad19689e (= recorded) | ae5936d2542330a6 / ae5936d2542330a6 (3 runs, all same) | OK acked=23/30 exactly-once |
| `disk=cut dseed=0 cf=221 \| run` | `1664796c93062473` | - | b5e546888d360f7e / b5e546888d360f7e (3 runs, all same) | OK acked=24/30 exactly-once |
| `disk=cut dseed=0 cf=229 \| run` | `3369ad69ed21f413` | - | 3cc1a727e178c204 / 3cc1a727e178c204 | OK acked=25/30 exactly-once |
| `disk=cut dseed=0 cf=237 \| run` | `ae535ea6be326142` | - | f055871e8a1af78f / f055871e8a1af78f | OK acked=26/30 exactly-once |
| `disk=cut dseed=0 cf=238 \| run` | `54ac8ca6b102ed00` | - | cab2ef41fc06fe21 / cab2ef41fc06fe21 | OK acked=26/30 exactly-once |
| `disk=cut dseed=0 cf=246 \| run` | `3d59fa0231ab2062` | - | 9b291c7ad82f18ee / 9b291c7ad82f18ee | OK acked=27/30 exactly-once |
| `disk=cut dseed=0 cf=254 \| run` | `78f8fabf7f5174d3` | - | 1c0e0c9f738526c0 / 1c0e0c9f738526c0 | OK acked=28/30 exactly-once |
| `disk=cut dseed=0 cf=255 \| run` | `ac22be7a40850c74` | - | 9eb69f6049c96dee / 9eb69f6049c96dee | OK acked=28/30 exactly-once |
| `disk=cut dseed=0 cf=263 \| run` | `014cf35e125c632e` | - | 61a5b7ce6ab45e22 / 61a5b7ce6ab45e22 | OK acked=29/30 exactly-once |
| `disk=cut dseed=0 cf=264 \| run` | `b0458e9584d45913` | - | fc9550cbba3ed89b / fc9550cbba3ed89b | OK acked=29/30 exactly-once |
| `disk=cut dseed=0 cf=209 \| run` | not recorded | - | 8666c33298537074 / 8666c33298537074 | VIOLATION mysql order=22 acked but not marked published |
| `disk=cut dseed=0 cf=210 \| run` | not recorded | - | e9437e42c0c06bf0 / e9437e42c0c06bf0 | VIOLATION mysql order=22 acked but not marked published |
| `disk=cut dseed=0 cf=218 \| run` | not recorded | - | 920d4383a865a20e / 920d4383a865a20e | VIOLATION mysql order=23 acked but not marked published |

Sweep points run once on the new image, all OK, not recorded before: cf=196,197,198,199,200,201,202,203,206,207,208,211,212,214,215,216,217,219,220,222,223,224,225.

#### `explore-bench-d1-kafka-first`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run K=mysql@10+1` | `57b21c9a63e66f4a` | - | 46a50564b962e60c / 46a50564b962e60c | VIOLATION kafka order=10 present 2 times (must be exactly once |

#### `explore-bench-d2-batched-publish-flag`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=cut dseed=0 \| run C=12` | `0982ff7f24617983` | - | 1a891c35eacdc620 / 1a891c35eacdc620 | VIOLATION mysql order=10 acked but not marked published |

#### `explore-bench-d3-blind-retry`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run P=kafka@5+2` | `1e7ee78e0da23d6c` | - | 070e9914eba809af / 070e9914eba809af | VIOLATION kafka order=5 present 2 times (must be exactly once |
| `disk=none dseed=0 \| run O=kafka@5-10:resp:loss,100%` | `4f2bb4f141b1b67b` | - | e71fb9d9db9b9799 / e71fb9d9db9b9799 | VIOLATION kafka order=5 present 2 times (must be exactly once |

#### `explore-bench-d4-produce-timeout-definite`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=cut dseed=0 \| run O=kafka@5-10:resp:loss,100% C=12` | `f2180513dc7ce87f` | - | f56e00be7aa0425f / f56e00be7aa0425f | VIOLATION order=5 is in kafka (1 times) but the client was told it fai |
| `disk=cut dseed=0 \| run P=kafka@5+2 C=8` | `8f9916c7a09e19e3` | - | 0e9a013f902af124 / 0e9a013f902af124 | VIOLATION order=5 is in kafka (1 times) but the client was told it fai |

#### `explore-bench-d5-myisam`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=cut dseed=0 \| run C=15` | `c7e097222b66cc96` | - | 0a5f8bee3d493d84 / 0a5f8bee3d493d84 | VIOLATION mysql order=0 expected=100000 got=MISSING (kafka has it: tru |

#### `explore-bench-d6-client-deadline-definite`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run P=kafka@5+2` | `d69d7035542ca1c4` | - | 62d7ab6367bde666 / 62d7ab6367bde666 | VIOLATION order=5 is in kafka (1 times) but the client was told it fai |

#### `explore-bench-d7-skip-last-record`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run P=kafka@28+2` | `e3607a4b79305277` | - | 078d3d4ab756d9f5 / 078d3d4ab756d9f5 | VIOLATION kafka order=29 present 2 times (must be exactly once |

#### `explore-bench-bg-control`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `d1b7a597436cb337` | - | 7ec7d4d8eefc2821 / 7ec7d4d8eefc2821 | OK acked=24/30 exactly-once |
| `disk=none dseed=0 \| run K=mysql@10+1` | `70870db1695e1417` | - | bc5f756bea8e6dd3 / bc5f756bea8e6dd3 | OK acked=30/30 exactly-once |
| `disk=cut dseed=0 \| run C=12` | `97b09671ca211588` | - | 3f11c95974ce3ed9 / 3f11c95974ce3ed9 | OK acked=12/30 exactly-once |
| `disk=none dseed=0 \| run P=kafka@5+2` | `43f176513865374f` | - | b0d526b8cf4ade6e / b0d526b8cf4ade6e | OK acked=30/30 exactly-once |
| `disk=none dseed=0 \| run O=kafka@5-10:resp:loss,100%` | `508dda2fc0148196` | - | 5427bc15156a1568 / 5427bc15156a1568 | OK acked=30/30 exactly-once |
| `disk=cut dseed=0 \| run O=kafka@5-10:resp:loss,100% C=12` | `b1d39636283d8a91` | - | c89ff61ecb7f6db6 / c89ff61ecb7f6db6 | OK acked=7/30 exactly-once |
| `disk=cut dseed=0 \| run P=kafka@5+2 C=8` | `c292544ea615e291` | - | 9c218e907da16a28 / 9c218e907da16a28 | OK acked=6/30 exactly-once |
| `disk=cut dseed=0 \| run C=15` | `98452d96321b1634` | - | 60e22155d4db98ee / 60e22155d4db98ee | OK acked=15/30 exactly-once |
| `disk=none dseed=0 \| run P=kafka@28+2` | `f07baba269ae2d47` | - | 7308945173a487d5 / 7308945173a487d5 | OK acked=30/30 exactly-once |

#### `explore-bench-bg-d0-old-front-door`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `80fb3138c43b1350` | - | 19ba9e8b32f35f9f / 19ba9e8b32f35f9f | VIOLATION order=11 is in kafka (1 times) but the client was never ACKe |

#### `explore-bench-bg-d1-kafka-first`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run K=mysql@10+1` | `5548f222e4ee1381` | - | 9dd3d60699cd5071 / 9dd3d60699cd5071 | VIOLATION kafka order=10 present 2 times (must be exactly once |

#### `explore-bench-bg-d2-batched-publish-flag`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=cut dseed=0 \| run C=12` | `e45437152fdaa5c0` | - | 939d19fec1321e96 / 939d19fec1321e96 | VIOLATION mysql order=10 acked but not marked published |

#### `explore-bench-bg-d3-blind-retry`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run P=kafka@5+2` | `886965f79516e715` | - | 39d940b2c1106335 / 39d940b2c1106335 | VIOLATION kafka order=5 present 2 times (must be exactly once |
| `disk=none dseed=0 \| run O=kafka@5-10:resp:loss,100%` | `df28d56d30c88d37` | - | 2470547346c1c534 / 2470547346c1c534 | VIOLATION kafka order=5 present 2 times (must be exactly once |

#### `explore-bench-bg-d4-produce-timeout-definite`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=cut dseed=0 \| run O=kafka@5-10:resp:loss,100% C=12` | `21c6259650708ced` | - | 621a961a3df5596c / 621a961a3df5596c | VIOLATION order=5 is in kafka (1 times) but the client was told it fai |
| `disk=cut dseed=0 \| run P=kafka@5+2 C=8` | `e0e6a1fab1e57ed9` | - | 4e4c728c40e8fa9b / 4e4c728c40e8fa9b | VIOLATION order=5 is in kafka (1 times) but the client was told it fai |

#### `explore-bench-bg-d5-myisam`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=cut dseed=0 \| run C=15` | `7965496592b4bd1c` | - | 6a984022395ab649 / 6a984022395ab649 | VIOLATION mysql order=0 expected=100000 got=MISSING (kafka has it: tru |

#### `explore-bench-bg-d6-client-deadline-definite`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run P=kafka@5+2` | `928f9c0f03289352` | - | 4965b4156c274e17 / 4965b4156c274e17 | VIOLATION order=5 is in kafka (1 times) but the client was told it fai |

#### `explore-bench-bg-d7-skip-last-record`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run P=kafka@28+2` | `52d633f175ecc3a4` | - | 05eabdae77e044b3 / 05eabdae77e044b3 | VIOLATION kafka order=29 present 2 times (must be exactly once |

#### `explore-bench-bg-fixed`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=cut dseed=0 cf=242 \| run` | `5c04f27153945d7b` | 5c04f27153945d7b (= recorded) | 368731d6ef0e33d6 / 368731d6ef0e33d6 (4 runs, all same) | VIOLATION mysql order=23 acked but not marked published |
| `disk=cut dseed=0 cf=243 \| run` | `374a4289102fb83d` | - | 204bcad982cd455c / 204bcad982cd455c | OK acked=24/30 exactly-once |
| `disk=cut dseed=0 cf=252 \| run` | `cb1bdca92ca68fb9` | - | 1410ab9eebfc60a8 / 1410ab9eebfc60a8 | OK acked=24/30 exactly-once |
| `disk=cut dseed=0 cf=253 \| run` | not recorded | - | e8369ce00e9ddbde / e8369ce00e9ddbde | VIOLATION mysql order=24 acked but not marked published |
| `disk=cut dseed=0 cf=261 \| run` | `52d369e6762aa0e8` | - | aa2466f3efdf84bb / aa2466f3efdf84bb | OK acked=25/30 exactly-once |
| `disk=cut dseed=0 cf=262 \| run` | `c91d08d16caa0c9d` | - | 70da9935c98c430a / 70da9935c98c430a | VIOLATION mysql order=25 acked but not marked published |
| `disk=cut dseed=0 cf=271 \| run` | `02b6489f130bd67a` | - | 182f294b5b54ec49 / 182f294b5b54ec49 | VIOLATION mysql order=26 acked but not marked published |
| `disk=cut dseed=0 cf=281 \| run` | `d199ccde526afc20` | - | 6d3259bfb3cee942 / 6d3259bfb3cee942 | OK acked=28/30 exactly-once |
| `disk=cut dseed=0 cf=290 \| run` | `6164017faccb85b2` | - | 0b6620fd6046c88f / 0b6620fd6046c88f | OK acked=28/30 exactly-once |
| `disk=cut dseed=0 cf=300 \| run` | `63b49d9e540ea192` | - | 9db21e7d851a34db / 9db21e7d851a34db | OK acked=29/30 exactly-once |
| `disk=cut dseed=0 cf=301 \| run` | `56cdea468af000f0` | - | 60789fe5e618dbea / 60789fe5e618dbea | OK acked=29/30 exactly-once |
| `disk=cut dseed=0 cf=302 \| run` | `a856e46385d4de1d` | - | 5557074251aa5457 / 5557074251aa5457 | OK acked=29/30 exactly-once |
| `disk=cut dseed=0 cf=303 \| run` | `b3a22a2ae424ea79` | - | a2f34530888c6e6e / a2f34530888c6e6e | VIOLATION mysql order=29 acked but not marked published |
| `disk=cut dseed=0 cf=314 \| run` | `cfb2e4dc72fff75e` | - | 4ac667597e933fd0 / 4ac667597e933fd0 | OK acked=30/30 exactly-once |

Sweep points run once on the new image, all OK, not recorded before: cf=234,235,236,237,238,239,240,241,244,245,246,247,248,249,250,251,254,255,256,257,258,259,260,263.

#### `explore-ob-redis`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `712bcab7167a52fe` | - | e3a37fbc1df45f2e / e3a37fbc1df45f2e | OK redis.acked_writes_survive: acked=20 all present |
| `disk=none dseed=1 \| run C=12 R=everysec` | `e4f65d61d3320ada` | e4f65d61d3320ada (= recorded) | 3818f0815e4e7afc / 3818f0815e4e7afc | VIOLATION [redis.acked_writes_survive] key k:2 lost |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | `eb3cc8787ec1e261` | - | e927650b0213ff04 / e927650b0213ff04 | OK redis.acked_writes_survive: acked=15 all present |

#### `explore-ob-stack-fix`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `fd5d47a611683780` | - | 7261eedf0e9aa9f4 / 7261eedf0e9aa9f4 | OK invariants=3 held |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `798ebf5e43758154` | - | 940d4b2729ef5a87 / 940d4b2729ef5a87 | OK invariants=3 held |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | `f50893ec3f010153` | - | 0918ac1aceb6c481 / 0918ac1aceb6c481 | OK invariants=3 held |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | `59a476dbb5f125cd` | - | bc2b434ced066d0b / bc2b434ced066d0b | OK invariants=3 held |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | `97d42b3f305e3009` | - | f024c5ff8db8401c / f024c5ff8db8401c | OK invariants=3 held |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | `d5439a9d436f5e3e` | - | 5906a950409e608c / 5906a950409e608c | OK invariants=3 held |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | `04a106eebd22eabe` | - | e59c568c7d713fb6 / e59c568c7d713fb6 | OK invariants=3 held |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | `104f7e9eebe0ddd1` | - | ff0a73477306e5ac / ff0a73477306e5ac | OK invariants=3 held |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | `b799eee66ca35c8e` | - | 0face87c38cc39ab / 0face87c38cc39ab | OK invariants=3 held |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | `600c43eb2f7832f0` | - | 7797e98df5ffded8 / 7797e98df5ffded8 | OK invariants=3 held |

#### `explore-ob-stack-old`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | not recorded | - | 5bfabc0071121286 / 5bfabc0071121286 | OK invariants=3 held |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `2eca7c7d0619e84d` | - | 0db375c372c33514 / 0db375c372c33514 | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the  |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | not recorded | - | 78ec1909a38d1fa1 / 78ec1909a38d1fa1 | OK invariants=3 held |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | `e0d3c3e6f775c868` | - | 889e3ee07734a954 / 889e3ee07734a954 | VIOLATION [stack.exactly_once] order=6 is in kafka (1 times) but the c |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | `0ce1c094670ac86b` | - | 57b817ce1527958e / 57b817ce1527958e | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the c |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | `9e0c88f58310a309` | - | 079c0c2c0d664772 / 079c0c2c0d664772 | VIOLATION [stack.exactly_once] order=5 is in kafka (1 times) but the c |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | `23d515ebd98aa646` | - | bfea8bea8f5a9a0b / bfea8bea8f5a9a0b | VIOLATION [stack.exactly_once] order=8 is in kafka (1 times) but the c |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | `1b267c460163dbb5` | - | 69e5ec946890061f / 69e5ec946890061f | VIOLATION [stack.exactly_once] order=12 is in kafka (1 times) but the  |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | `e9b1808136282c11` | - | cc469897fe33482a / cc469897fe33482a | VIOLATION [stack.exactly_once] order=17 is in kafka (1 times) but the  |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | `18199864df5dda95` | - | 62d82978510db767 / 62d82978510db767 | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the c |

#### `explore-ob-redis-pt`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `892c21e720aa888f` | - | 4e29fa9c4bf6416f / 4e29fa9c4bf6416f | OK redis.acked_writes_survive: acked=20 all present |
| `disk=none dseed=1 \| run C=12 R=everysec` | `d67dc6a6292054d7` | - | 00b2adacab25fa73 / 00b2adacab25fa73 | VIOLATION [redis.acked_writes_survive] key k:4 lost |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | `8dd4959290a4aad3` | - | f454e6eb607ed126 / f454e6eb607ed126 | OK redis.acked_writes_survive: acked=15 all present |

#### `explore-ob-stack-fix-pt`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `004b53662fdb8c5c` | - | 1e143793fbd1178a / 1e143793fbd1178a | OK invariants=3 held |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `64955f025db1d0cf` | - | b1710b70113809cd / b1710b70113809cd | OK invariants=3 held |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | `420aaba038efb689` | - | 5e443166ef59747b / 5e443166ef59747b | OK invariants=3 held |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | `2a6a5470ebf76179` | - | f34ad5b6d7485755 / f34ad5b6d7485755 | OK invariants=3 held |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | `1f31f3e3b6e0ec7d` | - | 195747446cfe1976 / 195747446cfe1976 | OK invariants=3 held |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | `7d2fd5ae7c83fc64` | - | 6d21b656d9edbb20 / 6d21b656d9edbb20 | OK invariants=3 held |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | `3448b3b5a64f8b96` | - | ba4fcd16652818c1 / ba4fcd16652818c1 | OK invariants=3 held |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | `f8dac7f2f32c98d8` | - | 00691d330beb913a / 00691d330beb913a | OK invariants=3 held |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | `b9ddd9097674160b` | - | 74fea7e8b174048e / 74fea7e8b174048e | OK invariants=3 held |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | `a801224223a89e85` | - | 0949615d3aee5d10 / 0949615d3aee5d10 | OK invariants=3 held |

#### `explore-ob-stack-old-pt`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `c326a384771a8444` | - | 99daf546e5d66fc2 / 99daf546e5d66fc2 | OK invariants=3 held |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `58f2f804b86f7e82` | - | a9314b08247ff193 / a9314b08247ff193 | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the  |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | `8b3da81f96edc0a6` | - | 9e6dfbb3698fa9ef / 9e6dfbb3698fa9ef | OK invariants=3 held |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | `cd82459de5d39a00` | - | a428601b64a74455 / a428601b64a74455 | VIOLATION [stack.exactly_once] order=6 is in kafka (1 times) but the c |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | `c7ed38880f1df3ae` | - | d25ccf1387445bc0 / d25ccf1387445bc0 | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the c |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | `1f7a19eded6ddb24` | - | ff2130f956ec89c6 / ff2130f956ec89c6 | VIOLATION [stack.exactly_once] order=5 is in kafka (1 times) but the c |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | `25cd27b25ac61c96` | - | a9a1857e2794eebb / a9a1857e2794eebb | VIOLATION [stack.exactly_once] order=8 is in kafka (1 times) but the c |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | `b34fd66f916dc30c` | - | 2a832da7c211a517 / 2a832da7c211a517 | VIOLATION [stack.exactly_once] order=12 is in kafka (1 times) but the  |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | `4fa3de863e122469` | - | a98f44d8e8ea7db4 / a98f44d8e8ea7db4 | VIOLATION [stack.exactly_once] order=17 is in kafka (1 times) but the  |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | `52f44fdc69fc430d` | - | 80d9f550aba6c54e / 80d9f550aba6c54e | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the c |

#### `explore-ob-miniflux`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `909117d9438d2f99` | - | 963a2d1839153db3 / 963a2d1839153db3 | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run K=postgres@10+3` | `58774edb2e7b0ee8` | - | c01116f8c2c336fe / c01116f8c2c336fe | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run K=front@10+3` | `839ee5276586b0f8` | - | e64ac743bcd5118d / e64ac743bcd5118d | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run P=postgres@10+3` | `a3d333f0b73b7674` | - | 2f24987c39e53a3e / 2f24987c39e53a3e | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run P=postgres@10~50:4000` | `12ceadac472f2323` | - | dd7fa45b33461433 / dd7fa45b33461433 | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run O=postgres@10-15:resp:loss,100%` | `1d4b053678d17a90` | - | 022c874e1ecde28c / 022c874e1ecde28c | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run O=front@10-15:resp:loss,100%` | `7fc1a4e946526c7c` | - | 84840b6c8f9481d3 / 84840b6c8f9481d3 | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run O=front@10-15:req:loss,100%` | `19cc5fcdc5f2fa81` | - | d272d83270344be1 / d272d83270344be1 | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run C=20` | `9528d6f8aad27135` | - | 858a1c8ef2700865 / 858a1c8ef2700865 | OK categories.acked_exactly_once: acked=20 exactly once |
| `disk=none dseed=1 \| run C=20 S=off` | `35d19164e2645b63` | - | 15d0e7075d0eff62 / 15d0e7075d0eff62 | OK categories.acked_exactly_once: acked=20 exactly once |

#### `explore-ob-miniflux-pt`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `4d456ac1c7de578f` | - | bf87f3ce800d26d1 / bf87f3ce800d26d1 | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run P=postgres@10+3` | `3eb1ee824394c338` | - | ecc0e1d1df2081a0 / ecc0e1d1df2081a0 | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run P=postgres@10~50:4000` | `167462a5fd57d5c1` | - | 03c17b73a10c407c / 03c17b73a10c407c | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run K=postgres@10+3` | `c5c6c73e2d99cbb2` | - | fdf37ca6d00e1cce / fdf37ca6d00e1cce | OK categories.acked_exactly_once: acked=30 exactly once |
| `disk=none dseed=1 \| run C=20` | `9b029d7c21d34a8b` | - | edcfe5f2eaebcf2c / edcfe5f2eaebcf2c | OK categories.acked_exactly_once: acked=20 exactly once |
| `disk=none dseed=1 \| run C=20 S=off` | `8f7290a59489aaac` | - | 46c0b42d001f9068 / 46c0b42d001f9068 | VIOLATION [categories.acked_exactly_once] category c4 stored 0 times |

#### `explore-ob-kb-sqlite-3510200`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run S=ae6f10cfefb4ae24` | `94234a0aed2afbe7` | 94234a0aed2afbe7 (= recorded) | 8d5d436ebbbd0f7c / 8d5d436ebbbd0f7c (4 runs, all same) | OK sqlite.committed_durable_and_intact: committed=536 transactions all |
| `disk=none dseed=0 tj=7404eae8487954f tjpm=500 tjmax=50000 \| run S=816665c37e16a3d3` | `e75e8480c7583361` | e75e8480c7583361 (= recorded) | 7ed8258c193cff08 / 7ed8258c193cff08 (4 runs, all same) | OK sqlite.committed_durable_and_intact: committed=1740 transactions al |

#### `explore-ob-kb-sqlite-3510300`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run S=ae6f10cfefb4ae24` | not recorded | 076eab25f410440b | e07ae92b29f7ca95 / e07ae92b29f7ca95 | OK sqlite.committed_durable_and_intact: committed=536 transactions all |
| `disk=none dseed=0 tj=7404eae8487954f tjpm=500 tjmax=50000 \| run S=816665c37e16a3d3` | not recorded | 8aa2bfe3ecc95594 | aabedeaad2c8936a / aabedeaad2c8936a | OK sqlite.committed_durable_and_intact: committed=1740 transactions al |

#### `explore-ob-redis-live`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `a98ba5035745de58` | - | 4d3309d0b8101b75 / 4d3309d0b8101b75 | OK redis.acked_writes_survive: acked=20 all present |
| `disk=none dseed=1 \| run K=redis@5+3` | `98923d5657a64cbf` | - | e757335c1962bf72 / e757335c1962bf72 | OK redis.acked_writes_survive: acked=20 all present |
| `disk=none dseed=1 \| run P=redis@5+3` | `68728f9760f2d079` | - | 98533bbdf92314d3 / 98533bbdf92314d3 | OK redis.acked_writes_survive: acked=20 all present |
| `disk=none dseed=1 \| run C=12` | `7465c2902bd68b65` | - | c31d5d33c51b665d / c31d5d33c51b665d | OK redis.acked_writes_survive: acked=12 all present |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | `a8df6a3e9d90c802` | - | 2a79f2cac682cc31 / 2a79f2cac682cc31 | VIOLATION [redis.pending_resolved] not satisfied 10.08s after heal, de |

#### `explore-ob-redis-oci`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | `d6a18f52f6418cf5` | - | 53e62adb7b981b63 / 53e62adb7b981b63 | OK redis.acked_writes_survive: acked=20 all present |
| `disk=none dseed=1 \| run C=12 R=everysec` | `9c36c7d46bcb9ddf` | - | 6b7dcbcd7f53f15b / 6b7dcbcd7f53f15b | VIOLATION [redis.acked_writes_survive] key k:1 lost |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | `4ffee7177898e58e` | - | bd48499e2c355cfa / bd48499e2c355cfa | OK redis.acked_writes_survive: acked=15 all present |

#### `richcov-old`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run` | not recorded | ee80306cc945a8f5 | 1742ac972b497a43 / 1742ac972b497a43 | OK acked=30/30 exactly-once |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | not recorded | 976d468db3e5a8df | 172496b98479d3b1 / 172496b98479d3b1 | VIOLATION order=11 is in kafka (1 times) but the client was never ACKe |

#### `richcov-fix`

| schedule | old fingerprint (recorded) | old image re-run today | new fingerprint (run 1 / run 2) | new verdict |
|---|---|---|---|---|
| `disk=none dseed=0 \| run` | not recorded | ceed30816296744c | e82c0b048e6fa8bb / e82c0b048e6fa8bb | OK acked=30/30 exactly-once |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | not recorded | ae08a197f1594f62 | b5c6439d5042fb67 / b5c6439d5042fb67 | OK acked=24/30 exactly-once |

## 5. Verdict changes

Every schedule gave the same verdict as recorded, except at the flush-cut points and in one
Redis key name. All of them were checked on the OLD image today as well: the old image still
gives the recorded fingerprint and verdict, so each change below comes from the new image (the
new `hvx_seedrng` is a different program, so it runs a different number of instructions and
branches early in the boot, and every later step of the run lands at a slightly different
virtual time).

1. **`explore-nf` flush-cut points (`hydo/cutflush/FINDING.md`, CLAUDE.md M58).** The 12
   recorded points (cf=204, 205, 213, 221, 229, 237, 238, 246, 254, 255, 263, 264) were all
   `VIOLATION mysql order=N acked but not marked published`. On the new image all 12 are
   `OK` (each twice, same fingerprint). The finding itself is still there, a few FLUSHes
   later: a sweep of cf=196–225 on the new image fails at **cf=209 and cf=210 (order 22) and
   cf=218 (order 23)** with the same verdict (each twice, same fingerprint), i.e. 5 FLUSHes
   after the old points 204/205/213. **New minimal reproducer:**
   `disk=cut dseed=0 cf=209 | run` on `explore-nf-pin-disk.img`, fingerprint
   `8666c33298537074` (old: `cf=204`, `e592e6c6fb6e5f3a`, which the old image still gives).
2. **`explore-bench-bg-fixed` flush-cut points (FINDING.md, CLAUDE.md M58).** Recorded: 13
   VIOLATION points (242, 243, 252, 261, 262, 271, 281, 290, 300, 301, 302, 303, 314; orders
   22–29). New image: **cf=242 still fails, but names order 23 instead of 22**
   (`368731d6ef0e33d6`; the old image still gives `5c04f27153945d7b`, order 22). Of the other
   12, cf=262 (order 25, was 24), cf=271 (order 26, was 25) and cf=303 (order 29, was 28)
   still fail; cf=243, 252, 261, 281, 290, 300, 301, 302 and 314 are now `OK`. The sweep
   cf=234–263 finds one new failing point, cf=253 (order 24). Every point ran twice with the
   same fingerprint. The minimal reproducer `disk=cut dseed=0 cf=242 | run` from FINDING.md
   still reproduces the bug on the new image.
3. **`explore-ob-redis` (M47, `hydo/onboard/RESULTS.md`).** `disk=none dseed=1 | run C=12
   R=everysec` loses key `k:2` instead of `k:3`. It is still a VIOLATION of the same kind
   (Redis's documented `everysec` window). The old image still gives `k:3`
   (`e4f65d61d3320ada`). The `-pt` image still loses `k:4`, as in M52.

Unchanged verdicts worth naming: every benchmark trigger (9 on the M54 images, 10 on
the M56 images) fails on its defect image with the same oracle line and pass on both controls; the control's base
bug (`O=kafka@25-30:resp:loss,100%`, order 26 twice) is unchanged; the pinned reproducer
fails on every old-front-door image (stack-old, stack-old-pt, richcov-old, bg-d0) and passes
on every fixed one; the 7 M42 schedules fail on both stack-old images naming the same orders
as M42 and pass on both stack-fix images; miniflux `C=20 S=off` is OK on the first-PID init
image and loses category `c4` on the process-tree image, as recorded; the liveness and OCI
schedules give the recorded verdicts.

## 6. Paths

- New images: bare-metal machine `~/hydo/build/*-pin-disk.img`.
- Raw results: bare-metal machine `/var/tmp/m60-rebase/res/results.jsonl` (one line per run),
  consoles under `/var/tmp/m60-rebase/res/<image>/consoles/`, schedule lists in
  `/var/tmp/m60-rebase/lists/`, queue log `/var/tmp/m60-rebase/queue.log`.
- The build directory on the build machine was deleted after the images were copied.

## 7. Rebuild after the netem drain fix (M77, 2026-10-23)

Milestone 71 changed how a network fault is lifted: the netem qdisc is first switched to pass-through
and drained, then deleted (`net_heal`), so the packets waiting in its delay queue are delivered instead
of dropped. Images built before M71 still drop them. This section records the rebuild of the
onboarding examples that allow `network`/`oneway` faults or use `addr:` link faults, and of the
default stack image (whose `/sbin/init` `hydo_net=` mode got the same change), with new image
hashes and fingerprints. The `-pin` values in sections 3–4 stay valid for the `-pin` images.

**Build.** Commit `2e0e3fe` (main), on the build machine in a private tree (`~/hydo-rb`, deleted
afterwards) with the pinned tools only: `hvx_seedrng` compiled by the pinned gcc (`c2a440c5…`; the
copy in `~/hydo/build/hvx_seedrng` is the stand-in `01adf1bd…` and was not used), miniflux 2.3.3
rebuilt from `~/mf-src/v2` (`dd8e2456…`, as recorded), the stack example's prebuilt order services
`~/hydo/build/explore-fix-hydostack` (`1f18f855…`) and `explore-hydostack` (`3ddaf8ba…`), and the
etcd release tarballs checked against the sha256s pinned in `examples/etcd/build.sh`. Commands:
`hydo-build <manifest> --out <name>` (etcd with `--set version=<v>`) and
`STACK_OUT=stack-nq bash hv/hvx/images/build-stack.sh`. Every image was built twice; both builds were
byte-identical. New names end in `-nq`; no existing image was changed.

| new image | sha256 | previous image (sha256) | files that differ (read-only mount, `diff -r`) |
|---|---|---|---|
| `explore-ob-stack-fix-nq` | `ecdcb4fab04253c4d28d32bd55ab6a7ff06bf6242e532a2c858df45706693e77` | `explore-ob-stack-fix-pt-pin` (`ac5553ea…`) | `/sbin/init` |
| `explore-ob-stack-old-nq` | `58de1559bbb52787684ff268b3a9f5dde89ec4cae4ca0acd0366f0a8147339a7` | `explore-ob-stack-old-pt-pin` (`0920ec72…`) | `/sbin/init` |
| `explore-ob-miniflux-nq` | `7b694c9fe1061aa73dc5b94fe7c2b3b9a4acb974ea5a569554a3f2e51b1f10fb` | `explore-ob-miniflux-pt-pin` (`cc3e6534…`) | `/sbin/init` |
| `explore-ob-etcd-3.5.5-nq` | `2d4d9301342a4199f061f107037ed368f1110cd545b8d63c57caf17ade05c144` | `explore-ob-etcd-3.5.5` (`bb5f8b79…`) | `/sbin/init`, `/etc/hydo/init-links.sh` |
| `explore-ob-etcd-3.5.6-nq` | `1df90f1696cc3a8a4c6ddcce47ab5896c9ca21b1e273af84f39a13562097e9ce` | `explore-ob-etcd-3.5.6` (`7f63c284…`) | the same two |
| `explore-ob-etcd-3.6.8-nq` | `bf213ea507eecf8d90d2ff56824c9ab8834c1615453a57715a3297a98f8b7da4` | `explore-ob-etcd-3.6.8` (`5b32d9e3…`) | the same two |
| `explore-ob-etcd-3.6.9-nq` | `fd2e9c585e5e8c124b5eb777b742049bdb17f93b0a274c3ef0a8982959ac2f47` | `explore-ob-etcd-3.6.9` (`a4a5c641…`) | the same two |
| `stack-nq` (default stack) | `161996bbf9f511ac58115b2386407175cde560289a53bce33fc114597cb54eed` | `stack-binlog-ret` (`b7eb93ee…`, M63) | `/sbin/init` |

The data partitions are identical in every pair. `explore-ob-etcd-3.6.15-nq` (`021d350a…`) was
built in M71 and was not rebuilt. The other
onboarding examples (redis, redis-ram, redis-oci, avx2, restate-oci) allow no network fault.

**Runs.** Bare-metal machine, `hvx_explore one` (a fresh boot per run) and `hvx_stack_verify`, both
compiled from `2e0e3fe` in `~/hydo-rb`, module srcversion `CA5BF8BF2A0D66B312A68EF` (not
reloaded), at most two VMs at a time, 2026-10-23 18:53–21:37 UTC. Each schedule ran twice on the
new image and once on the old image the same day. Every new-image pair gave the same fingerprint
and verdict (95 schedule/image pairs, 190 runs, no exception). Every old-image run whose
fingerprint was recorded earlier (sections 4 and `examples/etcd/RESULTS.md`, 32 values) gave the
recorded value, so the module and harness changes since then did not move them and every change
below comes from the new images. Schedules: the ones already recorded for each image in sections 4
and `examples/*/RESULTS.md`, plus network-heal schedules (the three M71 link-fault schedules and
the three 3.6.15 link-fault schedules on every etcd version, three network faults on stack-fix)
and, on etcd 3.5.5/3.5.6, the hand-written check of M60 (`K=etcd<n>@<5..25>+2`, 15 schedules).
Raw results: `/var/tmp/rb-nq/results.txt` on the bare-metal machine, consoles under `/var/tmp/rb-nq/runs/<id>/`.

**Packets delivered at the heal.** Every network or link fault lifted on a new image ended with
`left=0` (nothing dropped). In 14 of the 20 schedules whose fault was lifted before the end of the
workload, packets were waiting in the queue and were delivered (both runs gave the same counts):
4–41 packets on the twelve etcd link-delay schedules, 1 on stack-fix `O=mysql@5-10:resp:delay,1000ms`
and 3 on stack-fix `N=mysql@3-8:delay,50ms,loss,20%`; also 2 on the default stack's
`hydo_net=mysql` scenario (7.2). The old images dropped such packets.

### 7.1 Onboarding examples, old image vs new image

#### `explore-ob-stack-fix-nq` (old image `explore-ob-stack-fix-pt-pin`)

| schedule | old image, run today | new image, run 1 / run 2 | new verdict | verdict vs old |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | 1e143793fbd1178a OK | 8ad89b9f2565f88b / 8ad89b9f2565f88b | OK (3 held) | same |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | b1710b70113809cd OK | 96229e59c4d498ec / 96229e59c4d498ec | OK (3 held) | same |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | 5e443166ef59747b OK | 76b4917b3761288e / 76b4917b3761288e | OK (3 held) | same |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | f34ad5b6d7485755 OK | 1840ff6a385e0466 / 1840ff6a385e0466 | OK (3 held) | same |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | 195747446cfe1976 OK | 478afac2dbde8e89 / 478afac2dbde8e89 | OK (3 held) | same |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | 6d21b656d9edbb20 OK | b65235b9260d8875 / b65235b9260d8875 | OK (3 held) | same |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | ba4fcd16652818c1 OK | 7e38aa35149cc8c0 / 7e38aa35149cc8c0 | OK (3 held) | same |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | 00691d330beb913a OK | dad9ea70a2c5c9b1 / dad9ea70a2c5c9b1 | OK (3 held) | same |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | 74fea7e8b174048e OK | 5509af1f4f73a6d8 / 5509af1f4f73a6d8 | OK (3 held) | same |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | 0949615d3aee5d10 OK | 88a345f565f777c9 / 88a345f565f777c9 | OK (3 held) | same |
| `disk=none dseed=1 \| run N=kafka@5-10:delay,300ms` | 29eb2fbacf618373 OK | 17037311b8e11b73 / 17037311b8e11b73 | OK (3 held) | same |
| `disk=none dseed=1 \| run O=mysql@5-10:resp:delay,1000ms` | 2c419971f737b5c4 OK | 4523171f2e23e589 / 4523171f2e23e589 | OK (3 held) | same |
| `disk=none dseed=1 \| run N=mysql@3-8:delay,50ms,loss,20%` | 2e0c7f43af1e0eb4 OK | 1a06e2d8a0b97acd / 1a06e2d8a0b97acd | OK (3 held) | same |

#### `explore-ob-stack-old-nq` (old image `explore-ob-stack-old-pt-pin`)

| schedule | old image, run today | new image, run 1 / run 2 | new verdict | verdict vs old |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | 99daf546e5d66fc2 OK | 3eb4fc3d8f986ffa / 3eb4fc3d8f986ffa | OK (3 held) | same |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | a9314b08247ff193 VIOLATION | 402736c78bbf9a6d / 402736c78bbf9a6d | VIOLATION exactly_once order=11 in Kafka, never ACKed | same |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | 9e6dfbb3698fa9ef OK | 3a35b21568b5832b / 3a35b21568b5832b | OK (3 held) | same |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | a428601b64a74455 VIOLATION | 2a682dbf4ccfb074 / 2a682dbf4ccfb074 | VIOLATION exactly_once order=6 in Kafka, never ACKed | same |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | d25ccf1387445bc0 VIOLATION | 24c92e0c9238a2e1 / 24c92e0c9238a2e1 | VIOLATION exactly_once order=1 in Kafka, never ACKed | same |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | ff2130f956ec89c6 VIOLATION | c5d0b0d0ded187f6 / c5d0b0d0ded187f6 | VIOLATION exactly_once order=5 in Kafka, never ACKed | same |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | a9a1857e2794eebb VIOLATION | 92b720e1cc111ba3 / 92b720e1cc111ba3 | VIOLATION exactly_once order=8 in Kafka, never ACKed | same |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | 2a832da7c211a517 VIOLATION | 2285a081ad33e198 / 2285a081ad33e198 | VIOLATION exactly_once order=12 in Kafka, never ACKed | same |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | a98f44d8e8ea7db4 VIOLATION | 5a49cdd6c0ec7a3b / 5a49cdd6c0ec7a3b | VIOLATION exactly_once order=17 in Kafka, never ACKed | same |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | 80d9f550aba6c54e VIOLATION | 96849badad99db66 / 96849badad99db66 | VIOLATION exactly_once order=1 in Kafka, never ACKed | same |

#### `explore-ob-miniflux-nq` (old image `explore-ob-miniflux-pt-pin`)

| schedule | old image, run today | new image, run 1 / run 2 | new verdict | verdict vs old |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | bf87f3ce800d26d1 OK | 092d1ebdf7cfd714 / 092d1ebdf7cfd714 | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run K=postgres@10+3` | fdf37ca6d00e1cce OK | 3b562f2f1bd2ff85 / 3b562f2f1bd2ff85 | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run K=front@10+3` | 650e9ab225ae8b95 OK | 9eef5de1f30c56c7 / 9eef5de1f30c56c7 | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run P=postgres@10+3` | ecc0e1d1df2081a0 OK | 208623b82f6e8a5f / 208623b82f6e8a5f | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run P=postgres@10~50:4000` | 03c17b73a10c407c OK | 5955a8a7ff603dc6 / 5955a8a7ff603dc6 | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run O=postgres@10-15:resp:loss,100%` | 1d98911ae0470028 OK | 9523c079a75e578b / 9523c079a75e578b | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run O=front@10-15:resp:loss,100%` | 678539bc6bcf3447 OK | 18f0a36e175cb607 / 18f0a36e175cb607 | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run O=front@10-15:req:loss,100%` | f92a2195a7093a94 OK | c93698afd51f8531 / c93698afd51f8531 | OK (acked=30 exactly once) | same |
| `disk=none dseed=1 \| run C=20` | edcfe5f2eaebcf2c OK | 58a3b9ff6e46d29f / 58a3b9ff6e46d29f | OK (acked=20 exactly once) | same |
| `disk=none dseed=1 \| run C=20 S=off` | 46c0b42d001f9068 VIOLATION | b883bfaac88baf61 / b883bfaac88baf61 | VIOLATION category c4 stored 0 times | same |

#### `explore-ob-etcd-3.5.5-nq` (old image `explore-ob-etcd-3.5.5`)

| schedule | old image, run today | new image, run 1 / run 2 | new verdict | verdict vs old |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | 56f01a9f4404c4b5 OK | 4baa570bd70d0ae7 / 4baa570bd70d0ae7 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-etcd2@5-10:delay,300ms` | ed9ff0665ca936b4 OK | defd0cba5a705cf8 / defd0cba5a705cf8 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-*@10-20:delay,200ms` | cbc8b97a46cda7f3 OK | dd365613ab7b7e0a / dd365613ab7b7e0a | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run O=etcd2-etcd3@5-12:resp:delay,500ms` | 1b80ecebf8550e0b OK | 0a0bddda0eb17ab7 / 0a0bddda0eb17ab7 | OK (acked=90 all readable) | same |
| `disk=cut dseed=1 \| run C=30` | 5fb1c4c3aa64efb7 VIOLATION members_agree, revisions 99 99 98 | debadb6e1a4d46b8 / debadb6e1a4d46b8 | VIOLATION members_agree, revisions 99 98 98 | same kind, details differ |
| `disk=cut dseed=1 \| run N=etcd1-*@0-30:loss,100% C=30` | 949421928ad26017 VIOLATION members_agree, revisions 65 66 66 | 1e2946920c51737c / 1e2946920c51737c | OK (acked=58 all readable) | **CHANGED** |
| `disk=cut dseed=1 \| run N=etcd1-etcd2@0-30:loss,100% C=30` | 088fecfac3bd619a VIOLATION members_agree, revisions 69 68 69 | 7d2553e55364fad5 / 7d2553e55364fad5 | VIOLATION members_agree, revisions 67 66 67 | same kind, details differ |
| `disk=cut dseed=4127cc1347fae6ba \| run C=30` | b802065d149ebda2 VIOLATION members_agree, revisions 99 99 98 | 2bd06b8446559fe9 / 2bd06b8446559fe9 | VIOLATION members_agree, revisions 99 98 98 | same kind, details differ |
| `disk=none dseed=890abaea1c4d92d3 \| run K=etcd1@10+1 P=etcd3@26+1 T=2:-30` | 20ec47337eaf03f8 VIOLATION revisions_monotonic (client revision went back) | 73546ff456569eba / 73546ff456569eba | OK (acked=90 all readable) | **CHANGED** |
| `disk=none dseed=1 \| run K=etcd1@5+2` | c26a912a430b1612 OK | 21d281f9332eecf0 / 21d281f9332eecf0 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@10+2` | b5f21c2168f171fc OK | 90ec7193f3f66764 / 90ec7193f3f66764 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@15+2` | 06ab626d2e73c15e VIOLATION members_agree, revisions 99 98 98 | 68a4292b8e72e30b / 68a4292b8e72e30b | OK (acked=90 all readable) | **CHANGED** |
| `disk=none dseed=1 \| run K=etcd1@20+2` | 8558f8ed07255f97 OK | afe0410e80b5a21c / afe0410e80b5a21c | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@25+2` | 9fe1f5fd1acb3158 VIOLATION | 2c591d5d1fb5c3ea / 2c591d5d1fb5c3ea | VIOLATION members_agree, revisions 99 98 98 | same |
| `disk=none dseed=1 \| run K=etcd2@5+2` | 275efecc599c70b8 OK | a9318707c8213185 / a9318707c8213185 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@10+2` | d13a4b2d7236b8a9 OK | f7cb239260dec1fd / f7cb239260dec1fd | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@15+2` | 5f6bba205443f476 OK | 4f5f89b4d45e0c0e / 4f5f89b4d45e0c0e | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@20+2` | d760adcc6e185431 VIOLATION members_agree, revisions 98 99 98 | b9e41e70c21ebe84 / b9e41e70c21ebe84 | OK (acked=90 all readable) | **CHANGED** |
| `disk=none dseed=1 \| run K=etcd2@25+2` | 242320f8fb1810e5 OK (acked=90 all readable) | 971d74bcb0433c81 / 971d74bcb0433c81 | VIOLATION members_agree, revisions 98 99 98 | **CHANGED** |
| `disk=none dseed=1 \| run K=etcd3@5+2` | c6fb175d0fde5688 OK | 62aa978444d1887f / 62aa978444d1887f | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@10+2` | 52388ce96c5a8633 OK | ab123877d6e99a6c / ab123877d6e99a6c | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@15+2` | f44f9c538d8bc1d3 VIOLATION members_agree, revisions 98 98 99 | 3f7eae9042c7ce81 / 3f7eae9042c7ce81 | OK (acked=90 all readable) | **CHANGED** |
| `disk=none dseed=1 \| run K=etcd3@20+2` | 27b1d91d212d6e0e OK | 154b332a2055ff11 / 154b332a2055ff11 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@25+2` | 2da0f55484a21122 VIOLATION | be4081c525b935df / be4081c525b935df | VIOLATION members_agree, revisions 98 98 99 | same |

#### `explore-ob-etcd-3.5.6-nq` (old image `explore-ob-etcd-3.5.6`)

| schedule | old image, run today | new image, run 1 / run 2 | new verdict | verdict vs old |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | 86970013d37351c1 OK | abface2f2b610673 / abface2f2b610673 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-etcd2@5-10:delay,300ms` | 0cb718bb2e346318 OK | ebfe884fb6284035 / ebfe884fb6284035 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-*@10-20:delay,200ms` | d435b300e16634e2 OK | 5e1babe260a2f01d / 5e1babe260a2f01d | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run O=etcd2-etcd3@5-12:resp:delay,500ms` | 686fa6e356207f11 OK | ff1195f0ac35aa4c / ff1195f0ac35aa4c | OK (acked=90 all readable) | same |
| `disk=cut dseed=1 \| run C=30` | a5784f4674e22485 OK | e44f529fcb8ce746 / e44f529fcb8ce746 | OK (acked=90 all readable) | same |
| `disk=cut dseed=1 \| run N=etcd1-*@0-30:loss,100% C=30` | 93f72c58083f1a87 OK | 1693d9693b17750e / 1693d9693b17750e | OK (acked=58 all readable) | same |
| `disk=cut dseed=1 \| run N=etcd1-etcd2@0-30:loss,100% C=30` | 130b5b420c74963c OK | 88efbdc5baefe220 / 88efbdc5baefe220 | OK (acked=60 all readable) | same |
| `disk=cut dseed=4127cc1347fae6ba \| run C=30` | b3ae9098c0c3da18 OK | 5c5ff264b8046e6b / 5c5ff264b8046e6b | OK (acked=90 all readable) | same |
| `disk=none dseed=890abaea1c4d92d3 \| run K=etcd1@10+1 P=etcd3@26+1 T=2:-30` | 2b4952671683ea70 OK | 12c8375b4ba5b955 / 12c8375b4ba5b955 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@5+2` | b09ba87e80770f3e OK | 051736ab9a2592c3 / 051736ab9a2592c3 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@10+2` | aefa2bf3e56fba7b OK | a99cc97375a076d0 / a99cc97375a076d0 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@15+2` | d45de0e447034852 OK | 3b80002570382241 / 3b80002570382241 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@20+2` | 7fb40a3fceaf0552 OK | 6d658f9d67fbb33a / 6d658f9d67fbb33a | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd1@25+2` | 6b33bd3bfaedc6ce OK | 6abe9d82470d9df7 / 6abe9d82470d9df7 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@5+2` | 369420f4b718bdf5 OK | bd6e94a0b7892147 / bd6e94a0b7892147 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@10+2` | 8e3cf143afd1f1df OK | fa140681bb24349a / fa140681bb24349a | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@15+2` | a5830713d01dc365 OK | ec67109c3c4dca6d / ec67109c3c4dca6d | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@20+2` | 199279032efb92e9 OK | 7bd74a5b473a367d / 7bd74a5b473a367d | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd2@25+2` | 7d42440b4346578c OK | a184a8224a13a572 / a184a8224a13a572 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@5+2` | 6afe7a0d59541be8 OK | ead71eb78395945f / ead71eb78395945f | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@10+2` | d888ec98bd504889 OK | fdf70451066078f0 / fdf70451066078f0 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@15+2` | 892b27ebfc3c9528 OK | 02701a4f5f71e8d4 / 02701a4f5f71e8d4 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@20+2` | 7d8ca8060df95fdf OK | 9311ecf133817d97 / 9311ecf133817d97 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run K=etcd3@25+2` | a28938bbd069c900 OK | f3cb832cd0cc0f3b / f3cb832cd0cc0f3b | OK (acked=90 all readable) | same |

#### `explore-ob-etcd-3.6.8-nq` (old image `explore-ob-etcd-3.6.8`)

| schedule | old image, run today | new image, run 1 / run 2 | new verdict | verdict vs old |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | e6409067a40cc2d5 OK | 875de1a790776cff / 875de1a790776cff | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-etcd2@5-10:delay,300ms` | 58f5670a04ccc89e OK | e471ab31445a1b28 / e471ab31445a1b28 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-*@10-20:delay,200ms` | a26242cf01dc0084 OK | 476bb0bc2995096e / 476bb0bc2995096e | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run O=etcd2-etcd3@5-12:resp:delay,500ms` | 44c7154a6fb1c15a OK | 2f4dbb6e5da80e11 / 2f4dbb6e5da80e11 | OK (acked=90 all readable) | same |
| `disk=cut dseed=1 \| run C=30` | 58d815a2231170a5 OK | 4f4eca318b655aeb / 4f4eca318b655aeb | OK (acked=90 all readable) | same |
| `disk=cut dseed=1 \| run N=etcd1-*@0-30:loss,100% C=30` | ce82a58a2a1cf70c OK | c45fda05552f97b5 / c45fda05552f97b5 | OK (acked=58 all readable) | same |
| `disk=cut dseed=1 \| run N=etcd1-etcd2@0-30:loss,100% C=30` | a6cf5defd0d0e5f8 OK | 18126372e8346143 / 18126372e8346143 | OK (acked=60 all readable) | same |

#### `explore-ob-etcd-3.6.9-nq` (old image `explore-ob-etcd-3.6.9`)

| schedule | old image, run today | new image, run 1 / run 2 | new verdict | verdict vs old |
|---|---|---|---|---|
| `disk=none dseed=1 \| run` | 084054bfa1889a31 OK | ec25ed73287912f2 / ec25ed73287912f2 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-etcd2@5-10:delay,300ms` | 6025dfd1fdd6e89d OK | e88e74bde3b8f915 / e88e74bde3b8f915 | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run N=etcd1-*@10-20:delay,200ms` | bf973c623fcf8e8b OK | d52f5a1bce0e07ed / d52f5a1bce0e07ed | OK (acked=90 all readable) | same |
| `disk=none dseed=1 \| run O=etcd2-etcd3@5-12:resp:delay,500ms` | 18f2f317a6950e4b OK | 23a6b8acb5ac2768 / 23a6b8acb5ac2768 | OK (acked=90 all readable) | same |
| `disk=cut dseed=1 \| run C=30` | 642746a482fbb046 OK | 9b113e89611a4571 / 9b113e89611a4571 | OK (acked=90 all readable) | same |
| `disk=cut dseed=1 \| run N=etcd1-*@0-30:loss,100% C=30` | 0b9912014730194b OK | 101b2754b5163dcb / 101b2754b5163dcb | OK (acked=58 all readable) | same |
| `disk=cut dseed=1 \| run N=etcd1-etcd2@0-30:loss,100% C=30` | 32af834004aacca8 OK | 098c379001ca8add / 098c379001ca8add | OK (acked=60 all readable) | same |

### 7.2 The default stack (`stack-nq`), superseding the M63 baselines

`hvx_stack_verify <bzImage> <initramfs-kafka> <image> "<cmdline>" 1024 <mode>`, cmdline
`root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys`
(the M39 invocation). The old image `stack-binlog-ret` ran once the same day: `mode=all` gave
exactly the M63 values (SANITY `ceee1ce465e5424b`, F1 `3ddc9bd9aa95cf99`, F2 `00af090e634fefb5`,
durable `df945fb47a53e280`, F3 `88fa2915c90b452f` / `e9f60d6301f75013`).

`mode=all` on `stack-nq`, run twice, both runs identical, "MILESTONE 35 PASS" both times:

| check | M63 (`stack-binlog-ret`) | new (`stack-nq`) | verdict |
|---|---|---|---|
| SANITY | `ceee1ce465e5424b` | `2235f9d76a592a07` | OK, 30 orders consistent |
| F1 (all durable + power cut) | `3ddc9bd9aa95cf99` | `815cc64346e728fc` | OK, replay identical |
| F2 (MySQL relaxed + power cut) | `00af090e634fefb5`, order 9, durable `df945fb47a53e280` | `32024d0b245aff3c`, durable `7323951e90d21b4f` | VIOLATION `mysql order=8 expected=100008 got=MISSING (kafka has it: true)` (was order 9; MySQL's documented relaxed window, expected); replay and branch identical |
| F3 seed A / seed B | `88fa2915c90b452f` / `e9f60d6301f75013` | `96e2e7ddb963ee66` / `e8c16382540eba2e` | VIOLATION, Redis / MySQL cannot recover (same as M63) |

The eight per-fault scenarios of M36/M39 (`HVX_STACK_FAULT`). The first seven are `mode=fault`
(two fresh runs plus a branch from the pre-fault snapshot, all four fingerprints identical); the
eighth is `mode=sanity` (MySQL relaxed) run twice, identical. The old image ran each once the same
day (these old values were never recorded before; the M39 values belong to the M39 image).

| scenario (`HVX_STACK_FAULT`) | old `stack-binlog-ret`: fp, verdict | new `stack-nq`: fp | new verdict |
|---|---|---|---|
| `hydo_kill=mysql@10` | `31cbbb6682dd163d`, OK 30/30 | `959e90dd32e720f4` | OK acked=30/30 exactly-once |
| `hydo_kill=kafka@10` | `dff8953006949951`, OK 30/30 | `e00b67e3c5fb1898` | OK 30/30 |
| `hydo_kill=kafka@20` | `5abf5c66b75c4d1e`, OK 30/30 | `1ba55098f467b928` | OK 30/30 |
| `hydo_kill=redis@10` | `ea79982dd62c9b1c`, OK 30/30, 27 cached | `f235adac001ad733` | OK 30/30, 27 cached in Redis |
| `hydo_net=mysql:delay,100ms,loss,40%` | `87da43e4d3a6bcbb`, OK 30/30 (first pass 30/30) | `6a944fa762d93cc7` | OK 30/30 (first pass 29/30, 2 queued packets delivered at the heal) |
| `hydo_net=redis:duplicate,50%,reorder,50%,delay,20ms` | `42f89dc4c7c8146d`, OK 30/30 | `e92c9e6b38852983` | OK 30/30 |
| `hydo_net=kafka:delay,1s,loss,60%` (all durable) | `cd2c976727758c05`, OK 30/30 (first pass 28/30) | `b6c82b0ca917e0f2` | OK 30/30 (first pass 28/30) |
| same, MySQL relaxed (`mode=sanity`, twice) | `ed6470789675816b`, OK 30/30 (first pass 24/30) | `349b826362a275ec` | OK 30/30 (first pass 24/30) |

Every verdict is the same kind as on the old image. Two details moved: F2 names order 8 instead of
9, and the `hydo_net=mysql` scenario ACKs 29 of 30 orders on the first pass instead of 30 (the
retry then ACKs the last one; told-failed 0, unknown-but-applied 0).

### 7.3 Verdict changes

Stack example (fix and old order service), miniflux, etcd 3.5.6, 3.6.8 and 3.6.9: **no verdict
changed** (every schedule has the same verdict text on the old and the new image). The
default stack: same verdict kinds; F2 names order 8 instead of 9 (7.2).

**etcd 3.5.5 (etcd issue 14685) — six verdicts changed.** The bug is still there, but which
schedules hit its window moved:

| schedule | old image | new image |
|---|---|---|
| `disk=cut dseed=1 \| run N=etcd1-*@0-30:loss,100% C=30` | VIOLATION members disagree (65 66 66) | OK (acked=58) |
| `disk=none dseed=890abaea1c4d92d3 \| run K=etcd1@10+1 P=etcd3@26+1 T=2:-30` (M60's second minimal reproducer) | VIOLATION client revision went back | OK |
| `K=etcd1@15+2` | VIOLATION | OK |
| `K=etcd2@20+2` | VIOLATION | OK |
| `K=etcd3@15+2` | VIOLATION | OK |
| `K=etcd2@25+2` | OK | VIOLATION members disagree (98 99 98) |

The hand-written check of M60 (each member killed right after each defrag, `K=etcd<n>@<5,10,15,20,25>+2`,
`disk=none dseed=1`) now finds the bug in 3 of 15 schedules (all at step 25: etcd1, etcd2, etcd3)
instead of 5 of 15; on 3.5.6 it is 0 of 15 on both images. M60's first minimal reproducer
(`disk=cut dseed=4127cc1347fae6ba | run C=30`) still fails: members disagree 99 98 98 (was 99 99 98),
`2bd06b8446559fe9` on the new image.

**Why they changed.** None of the changed kill schedules has a network fault, so the new heal code
never runs in them; the one changed schedule with a link fault (`N=etcd1-*@0-30:loss,100% C=30`)
keeps its fault until the power cut and logs no heal either. The only file differences are `/sbin/init` (20,525 → 22,854 bytes: the heal
function, a `NET_ARGS` assignment, and six-digit instead of three-digit precision in the timed-pause
sleep) and `/etc/hydo/init-links.sh` (two `NET_ARGS` assignments), and the old image still gives its
recorded fingerprints today. The longer init script shifts guest timing (busybox reads and parses
more script before the services start), and 14685 needs a member to crash inside a short window
after an online defrag, so whether a kill at a fixed step lands in that window depends on that
timing. The same kind of shift moved the Redis lost key and the stack's F2 order in M52, M61 and M71.
3.5.6 (the fixed release) passes every schedule on both images.

### 7.4 etcd explorer re-run (the M60 campaign on the new 3.5.5 and 3.5.6 images)

`explore.py --strategy process --new-faults --timing --seeds 1-100 --workers 1` (the M60
command), on `explore-ob-etcd-3.5.5-nq` and `explore-ob-etcd-3.5.6-nq`, one worker each. The 100
schedule strings are identical to M60's (checked seed by seed). Results
`/var/tmp/rb-nq/ex/etcd-<v>-nq/report.md` on the bare-metal machine.

| version | M60 (old image) | new image |
|---|---|---|
| 3.5.5 | 19 failures, 19 reproduced: 18 × `etcd.members_agree`, 1 × `client.revisions_monotonic` | 17 failures, 17 reproduced, all `etcd.members_agree` |
| 3.5.6 | 0 failures | 0 failures |

On 3.5.5, 10 failing seeds are common to both runs (1 3 8 17 37 40 47 62 63 99), 9 failed only on the
old image (6 12 19 20 49 52 78 83 94) and 7 only on the new one (5 9 26 32 57 81 96): the same timing
shift as in 7.3. The shrinker reduced the new failures to the same minimal schedule as M60,
`disk=cut dseed=4127cc1347fae6ba | run C=30` (members disagree 99 98 98, `2bd06b8446559fe9` twice).
The second symptom of 14685 (a client sees its revision go back after switching members) was not
found in these 100 schedules on the new image, and M60's reproducer for it now passes (7.3); a new
reproducer for that symptom on the new image is not known.

### 7.5 Paths

- New images: bare-metal machine `~/hydo/build/*-nq-disk.img` (names in the table at the top of this section).
- Raw results: `/var/tmp/rb-nq/results.txt` (one line per run; job list `/var/tmp/rb-nq/queue.txt`),
  `hvx_explore` output under `/var/tmp/rb-nq/runs/<id>/`, `hvx_stack_verify` logs and consoles under
  `/var/tmp/rb-nq/sv/`, explorer runs under `/var/tmp/rb-nq/ex/`. Harnesses compiled in `~/hydo-rb/hvx`.

### 7.6 The default stack on 2 vCPUs (`stack-nq`), superseding the M69 2-vCPU baselines (M79, 2026-10-24)

The 2-vCPU stack baselines of M69 were measured on `stack-binlog-ret`. This subsection re-measures
them on `stack-nq` (`161996bb…`, section 7: only `/sbin/init` differs). Command, as in M69 and 7.2
plus the vCPU count: `HVX_VCPUS=2 hvx_stack_verify <bzImage> <initramfs-kafka> <image> "<cmdline>" 1024 <mode>`,
the 7.2 command line (no `tsc=reliable`), default PAUSE setting (4 consecutive PAUSEs end a turn),
`HVX_VCPU_PARK` unset (Linux starts vCPU 1; every boot logs "smp: Brought up 1 node, 2 CPUs").
Harness compiled from main `19fb8f6` in `~/hydo-smpb/hvx`; module srcversion
`CA5BF8BF2A0D66B312A68EF` (the main `hvx.ko`, not reloaded; M69 ran on the side module
`/dev/hvxsmp2` before it was merged). Bare-metal machine, 2026-10-24 07:48–08:08 UTC, at most two of
these VMs at a time, beside another session's explorer (one or two VMs).

**Old image first.** `mode=all` on `stack-binlog-ret` with 2 vCPUs, once, gave exactly the M69
values (SANITY `05915e49a5506c58`, F1 `1e4fedf03f81f941`, F2 `c5e01886e97b32b8` order 2, durable
`c9b1cfeb274d4e3d`, F3 `2c522171088a9da5` / `2a01253d0d5a4eb8`, MILESTONE 35 PASS). So the module
merge and the harness changes since M69 did not move them, and every change below comes from the
new image.

`mode=all` on `stack-nq` with 2 vCPUs, run twice, both runs identical in every value, "MILESTONE 35
PASS" both times (430 s and 532 s; the second shared the bare-metal machine with two explorer VMs):

| check | M69 (`stack-binlog-ret`, 2 vCPUs) | new (`stack-nq`, 2 vCPUs) | verdict |
|---|---|---|---|
| SANITY | `05915e49a5506c58` | `d1a5ee3ccea87dac` | OK, 30 orders consistent (MySQL, Kafka and Redis 30 each) |
| F1 (all durable + power cut) | `1e4fedf03f81f941` | `7fb7b85c69257f2b` | OK, replay identical |
| F2 (MySQL relaxed + power cut) | `c5e01886e97b32b8`, order 2, durable `c9b1cfeb274d4e3d` | `809c8292d95b638a`, durable `233e0afedb35fe4e` | VIOLATION `mysql order=8 expected=100008 got=MISSING (kafka has it: true)` (was order 2; MySQL's documented relaxed window, expected); replay and branch identical |
| F3 seed A / seed B | `2c522171088a9da5` / `2a01253d0d5a4eb8` | `3a074084d6fbeaa5` / `c008a3f9a3d961e3` | VIOLATION, MySQL cannot recover (both seeds, as in M69) |

The durable-disk hashes printed by `produce_durable` (the flushed data partition after each workload
boot) also changed and were identical within each run and across the two runs: flush on
`888d632b69733e2b` → `d471b222baf70917`, flush off `7477a4d55bdf6d0b` → `dc77182c93caa2cf`, lying-disk
seed A `ca4cb4235e9d8fd9` → `5bf79d4d529bb8d9`, seed B `577b077177234f87` → `6186c52bb8d402e9`.

**One fault scenario on 2 vCPUs.** M69's only 2-vCPU fault check (`HVX_STACK_FAULT=hydo_kill=kafka@10`,
`mode=fault`, `hydo/explore/TURNS.md`, `15be998ebba5667e`) ran with vCPU 1 parked on the explorer image
`explore-binlog-ret`, so it is not comparable. It was run here with Linux using both vCPUs:

| scenario | old `stack-binlog-ret`, 2 vCPUs (once) | new `stack-nq`, 2 vCPUs (twice) | verdict (both images) |
|---|---|---|---|
| `hydo_kill=kafka@10` | `29501fbc69f0b5f6` | `33611b4de52bd93d` (both runs) | OK acked=30/30 exactly-once (told-failed 0, unknown-but-applied 0, 30 cached in Redis); two runs + branch identical |

**The other seven scenarios on 2 vCPUs** (M81, 2026-10-24 08:17–08:41 UTC). The same command
with `HVX_STACK_FAULT=<scenario>`: the six `mode=fault` runs once each (two fresh runs plus a branch
from the pre-fault snapshot, all four fingerprints identical), the MySQL-relaxed case `mode=sanity`
twice (identical). Harness compiled from main `230ad15` in `~/hydo-pf/hvx` (byte-identical to the
`~/hydo-smpb` binary above), main `hvx.ko` srcversion `CA5BF8BF2A0D66B312A68EF` (not reloaded), at
most two of these VMs at a time beside other sessions' explorers; every console logs "smp: Brought
up 1 node, 2 CPUs". New values only (the old image was not run on 2 vCPUs for these):

| scenario (`HVX_STACK_FAULT`) | 1 vCPU, `stack-nq` (7.2) | 2 vCPUs, `stack-nq` | verdict on 2 vCPUs |
|---|---|---|---|
| `hydo_kill=mysql@10` | `959e90dd32e720f4` | `5787e8c2ab91c8dd` | OK acked=30/30 exactly-once (told-failed 0, unknown-but-applied 0, 30 cached in Redis) |
| `hydo_kill=kafka@20` | `1ba55098f467b928` | `70f2262407cef26e` | OK 30/30, 30 cached |
| `hydo_kill=redis@10` | `f235adac001ad733` | `124cc3dad8b93dfb` | OK 30/30, 27 cached in Redis (as on 1 vCPU) |
| `hydo_net=mysql:delay,100ms,loss,40%` | `6a944fa762d93cc7` | `c07c4bbbcde6d0d9` | OK 30/30 (first pass 29/30, as on 1 vCPU; the heal delivered 1 queued packet, 0 left) |
| `hydo_net=redis:duplicate,50%,reorder,50%,delay,20ms` | `e92c9e6b38852983` | `962604a54396c17c` | OK 30/30 (first pass 30/30; the heal delivered 1 queued packet, 0 left) |
| `hydo_net=kafka:delay,1s,loss,60%` (all durable) | `b6c82b0ca917e0f2` | `cbdbd77370547093` | OK 30/30 (first pass 28/30, as on 1 vCPU; nothing queued at the heal) |
| same, MySQL relaxed (`mode=sanity`, twice) | `349b826362a275ec` | `51d0cedb269b4693` (both runs) | OK 30/30 (first pass 22/30; 24/30 on 1 vCPU) |

With the Kafka kill at order 10 above, all eight scenarios of 7.2 now have 2-vCPU values. No verdict
differs in kind from 1 vCPU; the only detail that moved is the MySQL-relaxed first pass (22 instead of
24 orders acknowledged before the retry). Raw results: `/var/tmp/pf-smp/<tag>.{log,console,done}` on the
bare-metal machine (tags `k-mysql10`, `k-kafka20`, `k-redis10`, `n-mysql`, `n-redis`, `n-kafka`, `relaxed1`, `relaxed2`),
runner `~/hydo-pf/pf-smp.sh`.

**Verdict changes.** None in kind. F2 names order 8 instead of order 2, the same kind of shift as the
one-vCPU F2 (order 9 → 8 in 7.2): the longer `/sbin/init` changes guest timing, and with it which
not-yet-flushed MySQL commit the power cut catches. With the new image, F2 names order 8 on both one and
two vCPUs. Nothing in these checks lifts a network fault, so the netem drain itself never runs here.

**Not re-measured.** M69's explorer cell on 2 vCPUs (`explore.py --vcpus 2 --turns --concurrency
--strategy swarm`, seeds 1–40) ran on the explorer image `explore-binlog-ret`, which section 7 did
not rebuild, so its values still stand. M69 did not run the stack on 3 or 4 vCPUs.

Raw results: `/var/tmp/smpb-nq/<tag>.log` (tags `old-all`, `new-all-1`, `new-all-2`, `old-fault-kafka10`,
`new-fault-kafka10-1`, `new-fault-kafka10-2`), `<tag>.console` (consoles), `<tag>.done` (exit code, seconds);
runner `~/hydo-smpb/smpb-run.sh`.

## 8. The switch to agent images (M136, 2026-11-01)

Step C3 of `docs/design/guest-control.md`: since Milestone 136 every image `hydo build` makes is
an agent image (the guest agent `hydo-agent` as the first process, the host controller driving
it), and the shell init with its spliced extensions is deleted. Every manifest image therefore
has new bytes and new fingerprints; this section is the one re-recording of them (the user's
decision, open item 30). The hand-written images (the stack-explore images, the flagships, the
`deploy-metal.sh` tests, the pinned reproducers) are not manifest images and keep every value
of sections 1–7.

**How each value was measured.** Every recorded schedule of an example ran twice and both runs
gave the same fingerprint (the "twice" column). On the Mac: `hydo run --image` (a fresh boot)
twice, from a release-like prefix with this branch's programs (as M128). On the bare-metal machine:
`hydo-bench list --repeat 2 --workers 1` (one boot under lock-step control, then every schedule
twice from S1; an agent image's run from S1 gives the fingerprint of `hydo run` from a fresh boot,
M121, and the six x86_64 Redis schedules that also ran with `hydo run` from a fresh boot twice gave the same
values); the history examples' history verdicts with `hydo run`. The verdict column is the
invariants' verdict line (and, on the Mac, the history checks' verdict where the plan has them).

**The images.**
- **Mac (arm64), all built here, two builds byte-identical each** (initramfs and data disk), by
  this branch's `hydo build` from M128's staged example folders and prefix (busybox, mke2fs, tc,
  both guest kernels) with the pinned agent `340a1eec…`, the Rust `hydo-world` `fefcca66…` and the
  Rust `hydo-read` `88576fa8…` (M128's images carried M99's Go `hydo-read`). Initramfs sha256
  prefixes: redis `38364f57…`, redis-hist `05dfbbd5…`, redis-read `dbc071f3…`, redis-ram `583fd9f8…`, redis-bridge `a54c2248…` (second kernel), ledger `648814ff…`, ledger-spin `5062cc20…`, redis-composed atomic `4c8f2530…` and racy `9a3a9e30…`, node-kv `e1f75034…`, python-kv `b231c9f6…`, python-kv-hist `38f0e7cc…`, rust-kv `84b12d3e…`, shop buggy `400c87c0…` and fixed `69cbf111…`, gRPC demo buggy `a535cd40…` and fixed `fde16ebc…`, the voting app `60981390…` (second kernel), redis-oci `be44cc22…`.
- **x86_64, three tiers as in M127 (docs/C2-LINUX.md):** tier A (the Redis family and the voting
  app) built twice on the bare-metal machine with the Linux release's tools, two builds byte-identical each;
  tier B (the stack, `stack/hydo-read.yaml`, `stack/hydo-errors.yaml` with the old and the fixed
  order service, miniflux and miniflux-read) built in M112's no-Nix chroot (`stack-old` twice,
  byte-identical); tier C (etcd and its history images, the shop, the gRPC stack, the
  generated-driver stack, rust-kv, avx2, redis-oci, and restate-oci)
  patched from the recorded shell images by `hv/hvx/images/patch-agent.sh` (the pinned agent
  `b72c5caa…`, `/etc/hydo/control`, `init-links.sh` and, new, `/etc/hydo/compose.sh` removed),
  two patches byte-identical each. Disk image sha256 prefixes: tier A: redis `e39cb02a…`, redis-netem `b4bc8aba…`, redis-freeze `3cd41615…`, redis-hist `3dff5850…`, redis-live `80527451…`, redis-proxy `60d21cf9…`, redis-ram `14685a2a…`, redis-composed `e9c9e93e…`, redis-composed-hist `95c1500c…`, redis-composed-read `07d5e8ac…`, redis-errors `d430360d…`, redis-bridge `1661b504…`, the voting app `48dc9b22…` (generated manifest `38374949…`, loopback `63d176d9…`); tier B: stack old `000448fc…`, fixed `c0273506…`, stack-read old `18f8a2df…`, fixed `433cfab8…`, stack-errors fixed `e6bcc4f9…`, old `242de999…`, miniflux `07a7a90e…`, miniflux-read `6da84738…`; tier C (source → patched): etcd 3.5.5 `2d4d9301…` → `bf800b87…`, 3.5.6 `1df90f16…` → `5802a2a9…`, etcd-hc 3.5.5 `4f8aa317…` → `2e7e2d92…`, 3.5.6 `2a4a5d4d…` → `8a1e7da9…`, shop buggy `f331c6be…` → `82ebd785…`, fixed `6c669320…` → `016483b6…`, stack-grpc cur `b2206ebf…` → `6fb57d56…`, old `224b42f4…` → `df47fe94…`, planted `47ab04a7…` → `260a046a…`, stackgen `b70caeab…` → `b389dd8b…`, rust-kv `118c9f2c…` → `e8e44df0…`, avx2 `68267cf4…` → `5eeec022…`, restate-oci `2842986a…` → `13e5df59…`, redis-oci `ca3ea2b0…` → `eca096f1…`.
- **What only a Nix build can make** (the tier-C images are patched stand-ins whose fingerprints
  are their own; a real build needs the build machine): etcd 3.5.5/3.5.6 and their `-hc` images
  (etcd and its tools from Nix), the shop (its two programs built with Nix's Go), the gRPC stack
  (the order service with franz-go v1.22.2, which `stack-grpc/build.sh` takes from `hydo/stack`
  since M135, and the stack's closures), the generated-driver stack (the stack's closures and the
  generated folder), rust-kv's x86_64 build, avx2 (`hydoavx` from Nix's gcc), redis-oci
  and restate-oci (their OCI layouts; `hydo-fetch-image` could fetch them, not tried).

### 8.1 Mac (hvm), every recorded schedule twice

67 schedules on 19 images, all twice identical. The verdict classes are M128's agent images'
(and, where M130, M131 and M134 changed them, theirs): ledger-spin runs under the rewind mode,
the redis-bridge freezes run in the namespace, python-kv starts, the gRPC demo violates on the
buggy build and passes on the fixed one.


#### redis

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `193b67145318d6cf` | `3a017c99868bdde2` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `7d89169baf52ecc2` | `e363b83c51fe1b6a` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `1f61a9f4c6743764` | `2d7e8790ccc7ae8c` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `fb9c6839e1f6f4f7` | `af04504002d1ee7a` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `431723382cd6f829` | `17a9c9c812890c76` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `1655543a974efcbe` | `e786d086c906ce14` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `72ba6bc7c6de2503` | `b49246907b6b6431` | yes |
| `disk=none \| run T=10:3600` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `ee1f3c01eb04616f` | `3eb0d887243ec326` | yes |

#### redis-hist

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut dseed=1 \| run C=12` | OK invariants=0 held (); history OK | `4957be0a6dcb2035` | `f5977b5f85d127d8` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | OK invariants=0 held (); history VIOLATION [redis.kv_linearizable] | `957cb0ad3bb0b259` | `90144220037ae4ec` | yes |

#### redis-read

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: subset holds: all 20 items of acked are in redis (redis h... | `6ae40167e8288b87` | `f3ba88cf863b6426` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] subset violated: 12 items of 12 in acked not in redis: k:0=v0, k:1=v... | `cdc8de057c26cbb3` | `71466bb198edf9f9` | yes |

#### redis-ram

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `067dd2c6d54e2ca6` | `9d2a1edace89eb0f` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `ca5031a62951d171` | `533ee15b0e8b4e30` | yes |

#### redis-bridge

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `8fd9e1576dee25c4` | `14af985fafa6a0ca` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c42fb828cb40ea69` | `b9b191afe8ff347f` | yes |
| `disk=none \| run O=primary-replica@5-10:req:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `022f2184ab03958f` | `4f0e58064c0510be` | yes |
| `disk=none \| run O=primary@5-8:resp:delay,300ms` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c2bc92c9a0b26ea4` | `72f5aa1bb200a63b` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `2f0c2e49fb70d37a` | `6a8c64c25c180c74` | yes |
| `disk=cut \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `bd3d67ef875fb289` | `0f774df0f3a9ecb2` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c5468c557f44330f` | `1993b9669f47ad4e` | yes |
| `disk=none \| run F=primary<workload@5#2:1000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `e2d9ddbf12753896` | `db19e485a83060b1` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `9da67ba88882b5d5` | `01634ac3903ea950` | yes |

#### ledger

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `0d47c63d0101b506` | `f68317656d50efba` | yes |
| `disk=none \| run O=front@5-8:resp:loss,100%` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `946266e1c874aa0b` | `de5826d41aa3ceef` | yes |
| `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION [ledger.acked_exactly_once] entry 1 stored 2 times | `5af3b8a3cab4723b` | `11127ff78b0d4810` | yes |
| `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `61113c627c33395f` | `162cc6d22b65be38` | yes |
| `disk=none \| run N=front@0-30:loss,60%` | VIOLATION [ledger.acked_exactly_once] entry 9 stored 2 times | `d1536c0e926261c8` | `fbff9fa6de9b9cf0` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `fda105a8fa77341b` | `27229cb79acbbb1b` | yes |

#### ledger-spin

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `e0518577f10b88c3` | `8898c0caab66f7ff` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `2dcb3c0bf16ed0c0` | `24f5e0f77c0e0778` | yes |

#### redis-composed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `5efdd2f240a987e7` | `5b8c3c98d0ddb1b4` | yes |

#### redis-composed-racy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | VIOLATION [total_conserved] balances sum to 495, not 500 | `0d5d1e702e3ac25e` | `1a0a10b88c498849` | yes |

#### node-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `f15f18b0c9841e28` | `541802f17cca1c77` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `46e0b639fcbcdf95` | `1e967c83932acc4b` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `001f3efd81700a81` | `ac19dfcca9ed9f5e` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `258ef05237ca123e` | `a907a15d495d8deb` | yes |

#### python-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `26d1a7ff9f69fc6e` | `b7a4875e60aad8cf` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `2bf54a9ce250ed03` | `ac6ca0286e71a319` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `9860f80fb0e1625b` | `8c230ac87c46cb19` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `d3709c242b3f967a` | `6e977dae9aa72cba` | yes |

#### python-kv-hist

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `a7a64977623a9b0e` | `59f9219faa984ef8` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `88d948c63401bb25` | `f9cb1876c15427a0` | yes |

#### rust-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `5777c136b33b2224` | `41f2ebf5d53abd98` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `74c66bb13973fd34` | `1dbfd8f3d1e8d604` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `72da87f9b318654b` | `2e45ad554dc8876c` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `d94833c3a18f8546` | `0dd8982209f7390b` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `28e10d55aef58de9` | `c6f3b015807efab0` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `548606cca90e0e00` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `050f2a8526860056` | `b2a49ac46a82c64a` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `a853e49fb584b0fc` | `5dfdccf8b8dd97c5` | yes |

#### shop-buggy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `287ec924a7c24266` | `cb5e8cab8f6ad1e8` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `3ca443aabec98486` | `8c5be03f3908e8e4` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `9496d19fc5de2a84` | `4e904702332fb368` | yes |
| `disk=none \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 24,26) | `36de7a870a2f929b` | `88ac58547d058752` | yes |

#### shop-fixed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `4837e65705116cdb` | `42df466b523f61a8` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `61a13ebe6297ead4` | `c22306de0f9f6947` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `a6999e49d410101a` | `e1634bd4c544b877` | yes |

#### grpc-buggy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | VIOLATION [orders.consistent] VIOLATION request_id r1 created 5 orders: o11 o1 o2 o3 o4 | `1c3ee0d25016e91b` | `449b66182014be39` | yes |

#### grpc-fixed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | OK invariants=2 held (compose.commands: started=2 ok=1 failed=1 timeout=0 violation=0 killed=0 skipped=0) | `00d2c1058ea96480` | `dab029d6add95a35` | yes |

#### voting

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `3cf409bead783e0e` | `1b9e5fbb98634f9c` | yes |
| `disk=none \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `e5ea05f9170541b5` | `e39f6fb3fb98c8d0` | yes |
| `disk=cut \| run C=12` | VIOLATION [votes.counted_soon] not satisfied 60.83s after heal, deadline 60s (last check rc=1: too few vote... | `ff4dc7a30d3a8c98` | `64b7f90f3b5cd36e` | yes |
| `disk=none \| run N=vote-redis@5-12:loss,100%` | OK invariants=1 held (votes.counted: all 13 acknowledged votes counted) | `22b4be3c7a922d35` | `aa587576ffe76bf2` | yes |

#### redis-oci

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `f5368b1e6404ab4c` | `a9c56f64c3140e36` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `1103323601bea190` | `ba162b683a58bea2` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `e19719312392a8f1` | `1596f40e650cc3f4` | yes |


### 8.2 x86_64 (hvx), every recorded schedule twice

126 schedules on 37 images (`hydo-bench list --repeat 2 --workers 1`; a pool boots each image with
its manifest's `guest.ram_mib` and `guest.cpu`, which hydo-bench learned in M136), all twice
identical. Results on the bare-metal machine: `/var/tmp/m136/list/<name>/` (`<image>.jsonl` with `fp` and `fp2`,
consoles and control transcripts). The verdicts are the shell images' of sections 4 and 7 and of
the examples' RESULTS.md at the level of meaning: the stack's pinned-reproducer schedule
violates order 11 on the old order service and passes on the fixed one, every 7.1 schedule gives
7.1's verdict and order, the shop's planted bug and the gRPC stack's planted duplicate are found,
etcd 3.5.5 shows 14685's revision split at `C=30` and its history check flags the second
symptom, 3.5.6 passes both; `compose-voting/hydo.generated.yaml` ends in ERROR by design (its
invariants are the importer's TODOs).


#### redis (`redis_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `6875291e90072ae6` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `53f01e40711ccfdf` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `7b2f0cd241ca4d4c` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `beb90e457eed6cd7` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `e94d248dc0263db7` | yes |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK invariants=1 held (redis.acked_writes_survive: acked=15 all present) | `9161ea99985a7dfc` | yes |
| `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `1ab0adbd21b84609` | yes |

#### redis-netem (`redis_hydo-netem`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=7.99) | `ccbf6defbc79c73d` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | VIOLATION [net.no_retransmission] retrans=20 timeouts=16 uptime=14.08 | `0709180f00628487` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.08 | `bf326d61b307fe1b` | yes |

#### redis-freeze (`redis_hydo-freeze`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `00e91cf282302822` | yes |
| `disk=none dseed=0 \| run F=redis<workload@5#1:150000` | OK invariants=1 held (redis.acked_writes_survive: acked=5 all present) | `84ccade7ac058525` | yes |
| `disk=none dseed=0 \| run F=redis<workload@12#0:1` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `635d755e11838cc8` | yes |

#### redis-hist (`redis_hydo-hist`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=cut dseed=1 \| run C=12` | OK checks=2 held (redis.kv_linearizable: 32 operations on 20 keys linearizable) | `92ff124ba40094ce` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.kv_linearizable] key k:1: an acknowledged write is lost or a read is stale: a read that be... | `c1e60506e16ab573` | yes |

#### redis-live (`redis_hydo-live`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `7dadf3d612873e3f` | yes |
| `disk=none dseed=1 \| run K=redis@5+3` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `c1532e566d404833` | yes |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | VIOLATION [redis.pending_resolved] not satisfied 10.14s after heal, deadline 10s (last check rc=1: pending=1) | `eb479d619d4f470b` | yes |

#### redis-proxy (`redis_hydo-proxy`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `b32e2470daf79567` | yes |
| `disk=none \| run H=redisp@2-5:drop` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `8d758359f1f1cbb0` | yes |
| `disk=none \| run H=redisp@1-3:reset K=redis@4+2` | OK invariants=1 held (redis.acked_writes_survive: acked=16 all present) | `0b4d666bbd6c6873` | yes |

#### redis-ram (`redis-ram_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `66426149ddd86227` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `68c822c265af3c69` | yes |

#### redis-composed (`redis-composed_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `9699814e85527567` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | VIOLATION [acked_writes_survive] key k:21 lost | `ecc0f218e02c56a4` | yes |

#### redis-composed-hist (`redis-composed-hist_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=cut dseed=1 \| run C=12 W=0000002a,p4,q2,r100` | OK invariants=1 held (compose.commands: no composed workload ran in this boot) \| checks=2 held (redis.kv_li... | `eb9c4ad0a14e5f6e` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | VIOLATION [redis.kv_linearizable] key k:12: an acknowledged write is lost or a read is stale: a read that b... | `87d270f8f5466dff` | yes |

#### redis-composed-read (`redis-composed-read_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `e993025ac922649e` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: no composed workload ran in this boot) | `40a281b8671f1da6` | yes |

#### redis-errors (`redis-errors_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `014ee59eba36eadf` | yes |
| `disk=ioerr:1 dseed=281cc34161f446f4 \| run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,oanytime_read` | OK invariants=5 held (compose.commands: started=6 ok=4 failed=2 timeout=0 violation=0 killed=0 skipped=0) | `b35ee148adf7d89a` | yes |

#### redis-bridge (`redis-bridge_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `db3984d26b70e52b` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `282f100fa2ebf1bd` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `526a9673d7fff440` | yes |
| `disk=cut dseed=1 \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `f237f2cc19c52b43` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `cf3f34ef035a7f4e` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `1fb5b228073c9443` | yes |

#### rust-kv (`rust-kv-v2`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `389bc482e51e2194` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `d4604820a762223b` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `1591a9e914967b33` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `d4d56cb09417de82` | yes |

#### avx2 (`avx2`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (avx.results_match_scalar: steps=8 all AVX2 results equal the scalar ones) | `334f56db42148c38` | yes |

#### redis-oci (`redis-oci`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `6147e0344f24a878` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `231146340448942c` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `a86ca371b62d709b` | yes |

#### restate-oci (`restate-oci`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (restate.answers: answers; steps answered=6 of 6) | `01b32189a1ec35e8` | yes |
| `disk=none \| run K=restate@2+2` | OK invariants=1 held (restate.answers: answers; steps answered=4 of 6) | `6127ac2b0ce527bc` | yes |
| `disk=cut \| run C=4` | OK invariants=1 held (restate.answers: answers; steps answered=4 of 6) | `4cde851a6dfcaa06` | yes |

#### shop-buggy (`shop2-buggy`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `b9568c33cf16930e` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `c0b64b0991adc479` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 25,26) | `fbf9e64b1e159b9d` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `5a8193e66693ed9a` | yes |

#### shop-fixed (`shop2-fixed`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `a38984a542210bd8` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `92e91caf0502fe49` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `a9b788c96aa2ccf3` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `5c628c155c3f0b77` | yes |

#### etcd-3.5.5 (`etcd-3.5.5`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `ed50aa191ffd0a39` | yes |
| `disk=cut dseed=1 \| run C=30` | VIOLATION [etcd.members_agree] members disagree: revisions 98 99 98; k89 mod_revisions 97 97 97 | `200822da59c2ec72` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `3b47960fb89dd908` | yes |

#### etcd-3.5.6 (`etcd-3.5.6`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `fe7fa0444ca89f70` | yes |
| `disk=cut dseed=1 \| run C=30` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `b95213ade20090b2` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `220a997535814ade` | yes |

#### etcd-hc-3.5.5 (`etcd-hc-3.5.5`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `edec19943cb68c42` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | VIOLATION [etcd.kv_linearizable] key k31: these 4 operations cannot be ordered consistently with real time ... | `2367445439ec5bc8` | yes |

#### etcd-hc-3.5.6 (`etcd-hc-3.5.6`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `f9142657ac4708ac` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable) | `55e6b1dda9a9b14e` | yes |

#### voting (`compose-voting_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `5178e8c08ecf93cc` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `473a8fea22bcde0f` | yes |
| `disk=none dseed=1 \| run C=12` | VIOLATION [votes.counted_soon] not satisfied 60.79s after heal, deadline 60s (last check rc=1: too few vote... | `3e0c121b964fd925` | yes |
| `disk=none dseed=1 \| run N=vote-redis@5-12:loss,100%` | OK invariants=1 held (votes.counted: all 13 acknowledged votes counted) | `6d6fa005c6971bbc` | yes |

#### voting-generated (`compose-voting_hydo.generated`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | ERROR invariant todo.acked_operations_survive could not be evaluated: TODO: write the invariants (hydo.yaml) | `7eed916a16de1b82` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | ERROR invariant todo.acked_operations_survive could not be evaluated: TODO: write the invariants (hydo.yaml) | `b43a721c180f16ae` | yes |

#### voting-loopback (`compose-voting_hydo-loopback`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `32a8fda173a1552c` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `76991ff091db8c00` | yes |

#### stack-fix (`stack-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `af02273eb613d4f1` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `952f376397b32f0d` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `7c442cdf89fbafe4` | yes |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `7c836e665dc86a6a` | yes |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `9c1a09eb84c10492` | yes |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `134db765072a2a4b` | yes |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `392607638b9a2814` | yes |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `c1b2c134dfa2408d` | yes |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `464325fad244757b` | yes |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `3bae7cbcc42c41d2` | yes |
| `disk=none dseed=1 \| run N=kafka@5-10:delay,300ms` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `4a3c93815639896d` | yes |
| `disk=none dseed=1 \| run O=mysql@5-10:resp:delay,1000ms` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `738c3ce88bc57adf` | yes |
| `disk=none dseed=1 \| run N=mysql@3-8:delay,50ms,loss,20%` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `a1d1231abc0fa65e` | yes |

#### stack-old (`stack-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `0dde9af6eff8b2e7` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `b9e6fc324fa8ed76` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `b2c3e23999830920` | yes |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | VIOLATION [stack.exactly_once] order=6 is in kafka (1 times) but the client was never ACKed | `07bc12a5783716de` | yes |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the client was never ACKed | `30d5c3e2073cfac1` | yes |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | VIOLATION [stack.exactly_once] order=5 is in kafka (1 times) but the client was never ACKed | `3681cf66a8a666f6` | yes |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | VIOLATION [stack.exactly_once] order=8 is in kafka (1 times) but the client was never ACKed | `064aac00f5becfe0` | yes |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | VIOLATION [stack.exactly_once] order=12 is in kafka (1 times) but the client was never ACKed | `5ef55431f88e353e` | yes |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | VIOLATION [stack.exactly_once] order=17 is in kafka (1 times) but the client was never ACKed | `1d415e4720037456` | yes |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the client was never ACKed | `7f95d1abc26d0d83` | yes |

#### stack-read-fix (`stack-read-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `84d920df46d6e1be` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `2b09df294c12b4c4` | yes |

#### stack-read-old (`stack-read-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `c9d9b56b99037daf` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [order.told_failed_not_applied] disjoint violated: 1 item of told_failed is also in kafka: 11 | `ec83563bdd9838ef` | yes |

#### stack-errors-fix (`stack-errors-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `348cd1e351ee0583` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `e17626b8099af7e3` | yes |
| `disk=ioerr:5 dseed=1 \| run` | ERROR restart after the faults (mysql) | `6d8a523700f109e4` | yes |

#### stack-errors-old (`stack-errors-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `ecbdc4b8fed098c8` | yes |

#### miniflux (`miniflux`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `bf92f72fb2ee7743` | yes |
| `disk=none dseed=1 \| run K=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `94f7dff586a28ece` | yes |
| `disk=none dseed=1 \| run K=front@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `fe35c945ecc011c3` | yes |
| `disk=none dseed=1 \| run P=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `f2a0a493e00ee1fa` | yes |
| `disk=none dseed=1 \| run P=postgres@10~50:4000` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `17ee484de861e07a` | yes |
| `disk=none dseed=1 \| run O=postgres@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `22222bf0ae764cf7` | yes |
| `disk=none dseed=1 \| run O=front@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `2165a6d333a70bb5` | yes |
| `disk=none dseed=1 \| run O=front@10-15:req:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `632a3ecf4a94866f` | yes |
| `disk=none dseed=1 \| run C=20` | OK invariants=2 held (categories.acked_exactly_once: acked=20 exactly once) | `de5339c8ae51f04a` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] category c18 stored 0 times | `5cd2fcd9fc424b6b` | yes |

#### miniflux-read (`miniflux-read`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: exactly-once holds: each of the 30 items of acked occu... | `d9945a951b2253fd` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] exactly-once violated: acked c18 is in categories 0 times (of 20 ... | `aa403b06c9e5391d` | yes |

#### stack-grpc-cur (`stack-grpc-cur`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `444c61285b66c236` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `9242ed976a23fb24` | yes |

#### stack-grpc-old (`stack-grpc-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `c3ea21902906e620` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `1dfcf63387024add` | yes |

#### stack-grpc-planted (`stack-grpc-planted`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `fb676b97ed846766` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | VIOLATION [stack.exactly_once] kafka order=10 present 2 times (must be exactly once) | `441b8084ef4767db` | yes |

#### stackgen (`stackgen-rs`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `05f46c749f5f6cc5` | yes |
| `disk=none \| run K=front@10+2` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `d7c83fa446cd0313` | yes |
| `disk=cut \| run C=15` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `aef706013160bb3a` | yes |

### 8.3 The benchmark

The benchmark's manifest images (agent images, `restart_may_fail` on the order service) and their
triggers on the M136 images: `hydo/bench/README.md` "The manifest images after the switch
(Milestone 136)".

## 9. hydo-agent 0.3.0 and the host-call port (M140, 2026-11-02)

Milestone 140 pinned hydo-agent 0.3.0 (x86_64 `398f297e…`, aarch64 `099e5444…`, docs/BUILD-PINS.md)
in `hydo-build` and made the host-call port the hvx engine's default (`HYDO_HVX_HOSTCALL=0` turns it
off). Every agent image therefore has new bytes, and on hvx with a module that has the port (the
main module since Milestone 140 part A) the agent's lines and the host's lines travel over the port,
whose records are part of the fingerprint. This section is the one re-recording of every example
fingerprint after that change, measured as section 8 was (every recorded schedule twice, both runs
one fingerprint). Section 8's values are the 0.2.0 images' and stay as the record of M136. The
hand-written images (stack-explore images, flagships, `deploy-metal.sh`, the pinned reproducers)
are not agent images and keep every value of sections 1–7.

**The x86_64 images are now all real builds.** The tier-C examples that
M136 patched from recorded shell images (etcd and its `-hc` images, the shop, the gRPC stack, the
generated-driver stack, rust-kv, avx2, redis-oci, restate-oci) were built for real on the bare-metal machine
with Nix installed there (docs/BUILD-PINS.md "Nix on the bare-metal machine").

### 9.1 Mac (hvm), every recorded schedule twice

67 schedules on 19 images, 67 twice identical. Measured again in Milestone 140 on images with hydo-agent 0.3.0 (aarch64 `099e5444…`): every image has new bytes, so every fingerprint and ctl value is new. 64 of 67 verdict lines are word for word the M136 ones; 3 differ (listed at the end). The verdict classes are M128's agent images' (and, where M130, M131 and M134 changed them, theirs): ledger-spin runs under the rewind mode, the redis-bridge freezes run in the namespace, python-kv starts, the gRPC demo violates on the buggy build and passes on the fixed one.

How: M136's release-like prefix and staged example folders (M128's inputs, unchanged) with this
branch's `hydo`, `hydo-build`, `hydo-explore`, `hvm`, `hvmx` and the pinned arm64 agent; every
image built twice by `hydo build`, then every schedule run twice with `hydo run --image` (a fresh
boot each). The Mac engine has no host-call port, so its runs stay on the serial line; the
fingerprints moved only because the images' bytes did.

Initramfs sha256 prefixes (two builds byte-identical each, initramfs and data disk): redis `3287ec68…`, redis-hist `a02207fa…`, redis-read `44074ee6…`, redis-ram `de9299e9…`, redis-bridge `ec23e76d…`, ledger `60e5c1c4…`, ledger-spin `9fcc4781…`, redis-composed atomic `cb18a467…`, racy `a8ccf05b…`, node-kv `0120fa41…`, python-kv `1e54249a…`, python-kv-hist `c29c0b90…`, rust-kv `7da906b0…`, shop buggy `cffb7543…`, fixed `978a0b96…`, gRPC demo buggy `97945ae1…`, fixed `68503918…`, the voting app `e3cb6df4…`, redis-oci `1a4f2757…`.


#### redis

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `f1ed8926f43acd84` | `0b9e60d528cc2dc7` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `0f1d8968c551592e` | `ff4caa09a583ed70` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `3e16adbd26b169f6` | `a30845edf992e360` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `f6d694ceb65cd0a1` | `c1c6baa6a88a32a5` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `9b3b41c2d9d6921d` | `b4f7adea42cc337b` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `32520901637d3efb` | `99bc0f20aeb15106` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `c7fab9ec8cc7d26c` | `1ba0f7a39eb8e843` | yes |
| `disk=none \| run T=10:3600` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `2d3bf39ec370aa12` | `12b5294b6b4cadc3` | yes |


#### redis-hist

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut dseed=1 \| run C=12` | OK invariants=0 held (); history OK | `b141a93ed493b3d3` | `625922e1528c1e3c` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | OK invariants=0 held (); history VIOLATION [redis.kv_linearizable] | `3934af9273c205b5` | `037a11f0d411212e` | yes |


#### redis-read

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: subset holds: all 20 items of acked are in redis (redis h... | `f06758ca0247dbd8` | `8476030a9d80c61e` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] subset violated: 12 items of 12 in acked not in redis: k:0=v0, k:1=v... | `95d7e1ca3d487825` | `19138a12b5ab46af` | yes |


#### redis-ram

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `7a59995ac88ef7ce` | `23d53d97e4d64efa` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `5ac947f4a7487097` | `93ebd8cf695051da` | yes |


#### redis-bridge

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `dddc2303d3d42ccf` | `6a8e130bfbf9fef2` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `79d0a7f4e0e3f363` | `bb902d00160d7286` | yes |
| `disk=none \| run O=primary-replica@5-10:req:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `8efe5abed105a3a9` | `f2d609514474f032` | yes |
| `disk=none \| run O=primary@5-8:resp:delay,300ms` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `763b91f2b44aae1f` | `ab4e21b1c8e8ce83` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `501b262abcf67878` | `38e15c68d9963cda` | yes |
| `disk=cut \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `aa1c35b41a917b4e` | `1ac708d19e5718f8` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c5d5ec021d56f506` | `db293b879f216430` | yes |
| `disk=none \| run F=primary<workload@5#2:1000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `80ce5234857338c7` | `4ff9110839ed6dfd` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `94e11ee5fff0c8ce` | `a31807d7688f461c` | yes |


#### ledger

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `c862785a330bfab8` | `c9fd7b0f480180cd` | yes |
| `disk=none \| run O=front@5-8:resp:loss,100%` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `7efd0546c264d279` | `c15ce94773db279a` | yes |
| `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION [ledger.acked_exactly_once] entry 1 stored 2 times | `a0717cd470073ba2` | `aea4258f9a112981` | yes |
| `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `9a65359fb397b0a3` | `8552ef571c61fb8f` | yes |
| `disk=none \| run N=front@0-30:loss,60%` | VIOLATION [ledger.acked_exactly_once] entry 9 stored 2 times | `03ab518387ad9e81` | `e50f44fc62acc4da` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `3a9697e31d03494f` | `382b70a8955072c9` | yes |


#### ledger-spin

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `25f1123366ea96c6` | `930f998155f77b29` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `8dd9c5537321c0e2` | `012f769bba1db408` | yes |


#### redis-composed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `8e6df1a1d61c89a5` | `64228edfdcc3de50` | yes |


#### redis-composed-racy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | VIOLATION [total_conserved] balances sum to 503, not 500 | `e521936cacc3758f` | `cc25282fa6ee57cf` | yes |


#### node-kv

Superseded by section 12 (M152: the package file's license field changed the image bytes).

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `ab38514eec59f829` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `376432e7f641b27f` | `8de6e3d2ba8ff5d8` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `0ccaca40efede16b` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `41e70d32728c4aa4` | `39129eaa9b7e2227` | yes |


#### python-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `9d00f01d83b06ac4` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `128d1d3b48e64d1b` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `849c3e7fdff7ff7d` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `8b0fb5dad694116f` | `91e0931a5e07100c` | yes |


#### python-kv-hist

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `591de92b60287448` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `9f14f5e97552d859` | `4a799f3170dcb41b` | yes |


#### rust-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `61439e850cbf3f0b` | `bc6fe53352fa8359` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `340ee6be11b15054` | `ded68d8cdfcf34f0` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `5c01cc7f873600a8` | `22168eb0b60f5ac1` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `a3ccc7e44aba8a85` | `0ad74bffbfd87b1f` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `45f4beefdcc451e5` | `d20fa8483538cd42` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `30a2e13e3513dcd6` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `9becaca62b9a064d` | `31ca45bf4f0d1dfe` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `c79b1f2bf4cf5659` | `53e69be30dff5ad1` | yes |


#### shop-buggy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `b21d94a70f547510` | `0927ec66fcfd9b81` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `4918bf71ad909f1f` | `a95a5e731424d505` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `c9ebb0c89f804f43` | `66806064cf1f0f7a` | yes |
| `disk=none \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 23,26) | `065c010bd255c768` | `c9bf99c5c985c965` | yes |


#### shop-fixed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `6f9ef8fe3c5b9aa5` | `0707ac0014b93d7a` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `884e05caed8206ce` | `95eb07beb9a9a85b` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `d30f33a552194f9b` | `ec7c5b8d2145b6f9` | yes |


#### grpc-buggy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | VIOLATION [orders.consistent] VIOLATION request_id r1 created 5 orders: o11 o1 o2 o3 o4 | `87140952dd83ad1d` | `e6766eac8f5475fc` | yes |


#### grpc-fixed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | OK invariants=2 held (compose.commands: started=2 ok=1 failed=1 timeout=0 violation=0 killed=0 skipped=0) | `01b82adb6d3f4ae7` | `9fcb1c71aa47fc8b` | yes |


#### voting

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `d9203d76dbb2365e` | `176d7acdf4d05c6e` | yes |
| `disk=none \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `267c21b397767be1` | `73f458b22605fd8d` | yes |
| `disk=cut \| run C=12` | VIOLATION [votes.counted_soon] not satisfied 60.64s after heal, deadline 60s (last check rc=1: too few vote... | `de894df127e2d53c` | `ce1382c7c8092626` | yes |
| `disk=none \| run N=vote-redis@5-12:loss,100%` | OK invariants=1 held (votes.counted: all 13 acknowledged votes counted) | `b082b693ab94dcf5` | `880937d1be914793` | yes |


#### redis-oci

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `a1e5e9487c32ff5d` | `f7161ddb8479f702` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `6b71834cda791fa5` | `a8dee7e04969f117` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `009d7f67bb41f009` | `cd6f8435f894b551` | yes |


#### M136 → M140, fingerprint by fingerprint (Mac)

| example | schedule | M136 fingerprint | M140 fingerprint |
|---|---|---|---|
| redis | `disk=none \| run` | `193b67145318d6cf` | `f1ed8926f43acd84` |
| redis | `disk=none \| run K=redis@5+2` | `7d89169baf52ecc2` | `0f1d8968c551592e` |
| redis | `disk=none \| run P=redis@5+2` | `1f61a9f4c6743764` | `3e16adbd26b169f6` |
| redis | `disk=cut \| run C=12 R=everysec` | `fb9c6839e1f6f4f7` | `f6d694ceb65cd0a1` |
| redis | `disk=cut \| run C=12 R=always` | `431723382cd6f829` | `9b3b41c2d9d6921d` |
| redis | `disk=none \| run O=redis@5-8:resp:loss,100%` | `1655543a974efcbe` | `32520901637d3efb` |
| redis | `disk=none \| run N=redis@5-10:loss,60%` | `72ba6bc7c6de2503` | `c7fab9ec8cc7d26c` |
| redis | `disk=none \| run T=10:3600` | `ee1f3c01eb04616f` | `2d3bf39ec370aa12` |
| redis-hist | `disk=cut dseed=1 \| run C=12` | `4957be0a6dcb2035` | `b141a93ed493b3d3` |
| redis-hist | `disk=cut dseed=1 \| run C=12 R=everysec` | `957cb0ad3bb0b259` | `3934af9273c205b5` |
| redis-read | `disk=none \| run` | `6ae40167e8288b87` | `f06758ca0247dbd8` |
| redis-read | `disk=cut dseed=1 \| run C=12 R=everysec` | `cdc8de057c26cbb3` | `95d7e1ca3d487825` |
| redis-ram | `disk=none \| run` | `067dd2c6d54e2ca6` | `7a59995ac88ef7ce` |
| redis-ram | `disk=cut \| run C=12 R=everysec` | `ca5031a62951d171` | `5ac947f4a7487097` |
| redis-bridge | `disk=none \| run` | `8fd9e1576dee25c4` | `dddc2303d3d42ccf` |
| redis-bridge | `disk=none \| run N=primary-replica@5-10:loss,100%` | `c42fb828cb40ea69` | `79d0a7f4e0e3f363` |
| redis-bridge | `disk=none \| run O=primary-replica@5-10:req:loss,100%` | `022f2184ab03958f` | `8efe5abed105a3a9` |
| redis-bridge | `disk=none \| run O=primary@5-8:resp:delay,300ms` | `c2bc92c9a0b26ea4` | `763b91f2b44aae1f` |
| redis-bridge | `disk=none \| run K=replica@5+3` | `2f0c2e49fb70d37a` | `501b262abcf67878` |
| redis-bridge | `disk=cut \| run C=8` | `bd3d67ef875fb289` | `aa1c35b41a917b4e` |
| redis-bridge | `disk=none \| run F=replica<primary@5#3:2000` | `c5468c557f44330f` | `c5d5ec021d56f506` |
| redis-bridge | `disk=none \| run F=primary<workload@5#2:1000` | `e2d9ddbf12753896` | `80ce5234857338c7` |
| redis-bridge | `disk=none \| run H=redisp@3-6:drop` | `9da67ba88882b5d5` | `94e11ee5fff0c8ce` |
| ledger | `disk=none \| run` | `0d47c63d0101b506` | `c862785a330bfab8` |
| ledger | `disk=none \| run O=front@5-8:resp:loss,100%` | `946266e1c874aa0b` | `7efd0546c264d279` |
| ledger | `disk=none \| run O=front@0-30:resp:loss,70%` | `5af3b8a3cab4723b` | `a0717cd470073ba2` |
| ledger | `disk=none \| run O=front@0-30:resp:loss,50%` | `61113c627c33395f` | `9a65359fb397b0a3` |
| ledger | `disk=none \| run N=front@0-30:loss,60%` | `d1536c0e926261c8` | `03ab518387ad9e81` |
| ledger | `disk=none \| run P=front@10+2` | `fda105a8fa77341b` | `3a9697e31d03494f` |
| ledger-spin | `disk=none \| run` | `e0518577f10b88c3` | `25f1123366ea96c6` |
| ledger-spin | `disk=none \| run P=front@10+2` | `2dcb3c0bf16ed0c0` | `8dd9c5537321c0e2` |
| redis-composed | `disk=none \| run W=0000002a,p4,q2,r100` | `5efdd2f240a987e7` | `8e6df1a1d61c89a5` |
| redis-composed-racy | `disk=none \| run W=0000002a,p4,q2,r100` | `0d5d1e702e3ac25e` | `e521936cacc3758f` |
| node-kv | `disk=none \| run` | `f15f18b0c9841e28` | `ab38514eec59f829` |
| node-kv | `disk=cut \| run C=12 S=never` | `46e0b639fcbcdf95` | `376432e7f641b27f` |
| node-kv | `disk=cut \| run C=12` | `001f3efd81700a81` | `0ccaca40efede16b` |
| node-kv | `disk=none \| run K=nkv@8+2 B=1234` | `258ef05237ca123e` | `41e70d32728c4aa4` |
| python-kv | `disk=none \| run` | `26d1a7ff9f69fc6e` | `9d00f01d83b06ac4` |
| python-kv | `disk=none \| run K=pykv@8+2` | `2bf54a9ce250ed03` | `128d1d3b48e64d1b` |
| python-kv | `disk=cut \| run C=12 S=never` | `9860f80fb0e1625b` | `849c3e7fdff7ff7d` |
| python-kv | `disk=none \| run B=1234` | `d3709c242b3f967a` | `8b0fb5dad694116f` |
| python-kv-hist | `disk=cut \| run C=12` | `a7a64977623a9b0e` | `591de92b60287448` |
| python-kv-hist | `disk=cut \| run C=12 S=never` | `88d948c63401bb25` | `9f14f5e97552d859` |
| rust-kv | `disk=none \| run` | `5777c136b33b2224` | `61439e850cbf3f0b` |
| rust-kv | `disk=cut \| run C=12` | `74c66bb13973fd34` | `340ee6be11b15054` |
| rust-kv | `disk=cut \| run C=12 S=never` | `72da87f9b318654b` | `5c01cc7f873600a8` |
| rust-kv | `disk=none \| run K=rkv@8+2` | `d94833c3a18f8546` | `a3ccc7e44aba8a85` |
| rust-kv | `disk=none \| run B=5abc,1000,500` | `28e10d55aef58de9` | `45f4beefdcc451e5` |
| rust-kv | `disk=none \| run B=1234` | `548606cca90e0e00` | `30a2e13e3513dcd6` |
| rust-kv | `disk=none \| run K=rkv@8+2 B=1234` | `050f2a8526860056` | `9becaca62b9a064d` |
| rust-kv | `disk=none \| run P=rkv@5+3` | `a853e49fb584b0fc` | `c79b1f2bf4cf5659` |
| shop-buggy | `disk=none \| run` | `287ec924a7c24266` | `b21d94a70f547510` |
| shop-fixed | `disk=none \| run` | `4837e65705116cdb` | `6f9ef8fe3c5b9aa5` |
| shop-buggy | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | `3ca443aabec98486` | `4918bf71ad909f1f` |
| shop-fixed | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | `61a13ebe6297ead4` | `884e05caed8206ce` |
| shop-buggy | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | `9496d19fc5de2a84` | `c9ebb0c89f804f43` |
| shop-fixed | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | `a6999e49d410101a` | `d30f33a552194f9b` |
| shop-buggy | `disk=none \| run H=payments.charge@6-9:reset,p=50` | `36de7a870a2f929b` | `065c010bd255c768` |
| grpc-buggy | `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | `1c3ee0d25016e91b` | `87140952dd83ad1d` |
| grpc-fixed | `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | `00d2c1058ea96480` | `01b82adb6d3f4ae7` |
| voting | `disk=none \| run` | `3cf409bead783e0e` | `d9203d76dbb2365e` |
| voting | `disk=none \| run K=worker@5+5` | `e5ea05f9170541b5` | `267c21b397767be1` |
| voting | `disk=cut \| run C=12` | `ff4dc7a30d3a8c98` | `de894df127e2d53c` |
| voting | `disk=none \| run N=vote-redis@5-12:loss,100%` | `22b4be3c7a922d35` | `b082b693ab94dcf5` |
| redis-oci | `disk=none \| run` | `f5368b1e6404ab4c` | `a1e5e9487c32ff5d` |
| redis-oci | `disk=cut \| run C=12 R=everysec` | `1103323601bea190` | `6b71834cda791fa5` |
| redis-oci | `disk=none \| run K=redis@5+2` | `e19719312392a8f1` | `009d7f67bb41f009` |

#### Verdict lines that differ from M136 (Mac)

- redis-composed-racy `disk=none | run W=0000002a,p4,q2,r100`
  - M136: HYDO-ORACLE: VIOLATION [total_conserved] balances sum to 495, not 500
  - M140: HYDO-ORACLE: VIOLATION [total_conserved] balances sum to 503, not 500
- shop-buggy `disk=none | run H=payments.charge@6-9:reset,p=50`
  - M136: HYDO-ORACLE: VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 24,26)
  - M140: HYDO-ORACLE: VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 23,26)
- voting `disk=cut | run C=12`
  - M136: HYDO-ORACLE: VIOLATION [votes.counted_soon] not satisfied 60.83s after heal, deadline 60s (last check rc=1: too few votes counted: 0 of 12 acknowledged votes are in PostgreSQL; voter0 (vote a) is not)
  - M140: HYDO-ORACLE: VIOLATION [votes.counted_soon] not satisfied 60.64s after heal, deadline 60s (last check rc=1: too few votes counted: 0 of 12 acknowledged votes are in PostgreSQL; voter0 (vote a) is not)

### 9.2 x86_64 (hvx), every recorded schedule twice

126 schedules on 37 images (`hydo-bench list --repeat 2` on the bare-metal machine: one boot per image under
lock-step control, every schedule twice from S1; the host-call port on, the engine's default since
M140; the main module `B5E97B37504F1A5F508BA30`), 126 twice identical. Redis's `disk=none | run`
from a fresh boot with `hydo run` gives the S1 value `e7de012271d99d0a`. Every image was built for
real with hydo-agent 0.3.0 (x86_64 `398f297e…`), each twice with the same bytes (docs/HISTORY.md
"Milestone 140, part B" lists the hashes and how each compares with M136's image). 113 of 126
verdict lines are word for word the M136 ones; 13 differ (listed at the end): 11 in timing words only (retransmission counts, times, revisions, which
service's restart failed first under I/O errors, which acknowledged ids a cut lost), and 2 in
their verdict class, each also with the port off (`HYDO_HVX_HOSTCALL=0`, so the new agent's timing
moved them, not the port): etcd-hc-3.5.5's `K=etcd1@10+1 P=etcd3@26+1` (VIOLATION in M136, OK now;
M78's schedule is known to be timing-fragile on the `-hc` images, open item 20), and miniflux's `P=postgres@10~50:4000` (OK in
M136, VIOLATION `api.reads_acked_categories` now).


#### redis (`redis_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `e7de012271d99d0a` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `a678f41e98c1ca76` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `784ad4a2ab462de1` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `3c57c1db1c05c410` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `dd4791c21b813908` | yes |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK invariants=1 held (redis.acked_writes_survive: acked=15 all present) | `025eecdf01c72e8c` | yes |
| `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `4c6dbfaae010725b` | yes |

#### redis-netem (`redis_hydo-netem`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=8.05) | `7800b04875b8c265` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | VIOLATION [net.no_retransmission] retrans=12 timeouts=9 uptime=16.34 | `2f689cdd7faf7278` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.16 | `d20e0c833ff5c16b` | yes |

#### redis-freeze (`redis_hydo-freeze`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `24624254f42ab5de` | yes |
| `disk=none dseed=0 \| run F=redis<workload@5#1:150000` | OK invariants=1 held (redis.acked_writes_survive: acked=5 all present) | `b5b4a5367a680c36` | yes |
| `disk=none dseed=0 \| run F=redis<workload@12#0:1` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `72c7301945727c36` | yes |

#### redis-hist (`redis_hydo-hist`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=cut dseed=1 \| run C=12` | OK checks=2 held (redis.kv_linearizable: 32 operations on 20 keys linearizable) | `d663fa479b7f3112` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.kv_linearizable] key k:1: an acknowledged write is lost or a read is stale: a read that be... | `d66c54b6730f3c9b` | yes |

#### redis-live (`redis_hydo-live`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `3a4c6f99afe4ccb3` | yes |
| `disk=none dseed=1 \| run K=redis@5+3` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `b71a52f6e2e7e8c0` | yes |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | VIOLATION [redis.pending_resolved] not satisfied 10.16s after heal, deadline 10s (last check rc=1: pending=1) | `daf98f52e714d103` | yes |

#### redis-proxy (`redis_hydo-proxy`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `1b5ac9bc529d2b73` | yes |
| `disk=none \| run H=redisp@2-5:drop` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `e8a48ac08bbcd6fe` | yes |
| `disk=none \| run H=redisp@1-3:reset K=redis@4+2` | OK invariants=1 held (redis.acked_writes_survive: acked=16 all present) | `0e5462ab0bc7fd75` | yes |

#### redis-ram (`redis-ram_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `c781388975e573af` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `fd3e4a85d7ed5448` | yes |

#### redis-composed (`redis-composed_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `aa64393559c1ad8c` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | VIOLATION [acked_writes_survive] key k:21 lost | `bec487fb247ea7de` | yes |

#### redis-composed-hist (`redis-composed-hist_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=cut dseed=1 \| run C=12 W=0000002a,p4,q2,r100` | OK invariants=1 held (compose.commands: no composed workload ran in this boot) \| checks=2 held (redis.kv_li... | `a1216567acc59fdd` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | VIOLATION [redis.kv_linearizable] key k:12: an acknowledged write is lost or a read is stale: a read that b... | `2ea4d1ea752f384d` | yes |

#### redis-composed-read (`redis-composed-read_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `ab1689254694d5cd` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: no composed workload ran in this boot) | `01a664be89cc4e0e` | yes |

#### redis-errors (`redis-errors_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `65bcc8329249d3a9` | yes |
| `disk=ioerr:1 dseed=281cc34161f446f4 \| run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,oanytime_read` | OK invariants=5 held (compose.commands: started=6 ok=4 failed=2 timeout=0 violation=0 killed=0 skipped=0) | `2a9a315d66a895cf` | yes |

#### redis-bridge (`redis-bridge_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `f1d54ce6976d42f8` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `64337cd16d534b92` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `d37667b70deb571d` | yes |
| `disk=cut dseed=1 \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `f66d365cf05528c3` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `0c6ce203fc4ad381` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `ba20935e807e328a` | yes |

#### rust-kv (`rust-kv-v2`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `cc837d9a09a2e708` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `bc268af1845c6e27` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `816d17daf4541f55` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `cb256779528160cd` | yes |

#### avx2 (`avx2`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (avx.results_match_scalar: steps=8 all AVX2 results equal the scalar ones) | `53342df87d6fb2a3` | yes |

#### redis-oci (`redis-oci`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `e677188840a31f83` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `d69cd5ef4915a5ad` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `ef47e3b95cc52618` | yes |

#### restate-oci (`restate-oci`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (restate.answers: answers; steps answered=6 of 6) | `e079c291660ca4bf` | yes |
| `disk=none \| run K=restate@2+2` | OK invariants=1 held (restate.answers: answers; steps answered=4 of 6) | `9ec68bf557979229` | yes |
| `disk=cut \| run C=4` | OK invariants=1 held (restate.answers: answers; steps answered=4 of 6) | `e51219e946666bc4` | yes |

#### shop-buggy (`shop2-buggy`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `4bb0a1fe876cdb85` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `8fc62dc461277c3f` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 25,26) | `9671f2b17a161583` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `9d12c1710fb3caf3` | yes |

#### shop-fixed (`shop2-fixed`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `b53ecc59b80008fc` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `ea6404c9dd299dde` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `ae0a30fbdbf638ed` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `944f214df3babac2` | yes |

#### etcd-3.5.5 (`etcd-3.5.5`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `c2f71429accf734c` | yes |
| `disk=cut dseed=1 \| run C=30` | VIOLATION [etcd.members_agree] members disagree: revisions 99 99 98; k89 mod_revisions 96 96 96 | `97fd98642e8f2614` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `5d0dde7808bdb377` | yes |

#### etcd-3.5.6 (`etcd-3.5.6`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `0428c99be308463e` | yes |
| `disk=cut dseed=1 \| run C=30` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `347369b1bc71618b` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `0c3cae8d4892f38f` | yes |

#### etcd-hc-3.5.5 (`etcd-hc-3.5.5`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `145703f206f2a797` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable) | `e91c242f589bb74c` | yes |

#### etcd-hc-3.5.6 (`etcd-hc-3.5.6`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `fe88a223de851b5a` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 189 operations on 90 keys linearizable) | `3c036a3f62a06536` | yes |

#### voting (`compose-voting_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `be6cc370a553eddd` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `baf15f205fd6f4e0` | yes |
| `disk=none dseed=1 \| run C=12` | VIOLATION [votes.counted_soon] not satisfied 60.69s after heal, deadline 60s (last check rc=1: too few vote... | `378cb9bf32c7a22c` | yes |
| `disk=none dseed=1 \| run N=vote-redis@5-12:loss,100%` | OK invariants=1 held (votes.counted: all 13 acknowledged votes counted) | `62a1314567295e9b` | yes |

#### voting-generated (`compose-voting_hydo.generated`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | ERROR invariant todo.acked_operations_survive could not be evaluated: TODO: write the invariants (hydo.yaml) | `8195138a6ac6e761` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | ERROR invariant todo.acked_operations_survive could not be evaluated: TODO: write the invariants (hydo.yaml) | `cb6dab11caa9613f` | yes |

#### voting-loopback (`compose-voting_hydo-loopback`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `c414c6cea52839bd` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `d5ce7e93d5e95ecd` | yes |

#### stack-fix (`stack-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `e43499921a8ec703` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `aad6a563e2046361` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ac2f1606e253ca3e` | yes |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `4d94660c4856fb70` | yes |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `67e6b3e6375df28c` | yes |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `fcf450f3f4722992` | yes |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `43a27aa3950d1c0d` | yes |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `6107e823320da4c7` | yes |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `683e16b5dff9c452` | yes |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `97b8a3f31c8cb87a` | yes |
| `disk=none dseed=1 \| run N=kafka@5-10:delay,300ms` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `b51e800a0d7a31c3` | yes |
| `disk=none dseed=1 \| run O=mysql@5-10:resp:delay,1000ms` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `a0ecb6f21e977306` | yes |
| `disk=none dseed=1 \| run N=mysql@3-8:delay,50ms,loss,20%` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `64361e680ffa7d23` | yes |

#### stack-old (`stack-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `cf581cbbbf91e2fa` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `8fc461db23ef6642` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `17996aa138ca25da` | yes |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | VIOLATION [stack.exactly_once] order=6 is in kafka (1 times) but the client was never ACKed | `ea237a38611e2dab` | yes |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the client was never ACKed | `78a2445f1ca68973` | yes |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | VIOLATION [stack.exactly_once] order=5 is in kafka (1 times) but the client was never ACKed | `b39f27cdbb31ebd3` | yes |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | VIOLATION [stack.exactly_once] order=8 is in kafka (1 times) but the client was never ACKed | `ea2cdfdec04ca0e8` | yes |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | VIOLATION [stack.exactly_once] order=12 is in kafka (1 times) but the client was never ACKed | `bb2ec2417c340bb2` | yes |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | VIOLATION [stack.exactly_once] order=17 is in kafka (1 times) but the client was never ACKed | `3ebb3c4d243bc953` | yes |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the client was never ACKed | `766607a5e83edd4a` | yes |

#### stack-read-fix (`stack-read-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `31747d42da9db86a` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `c1baad113f4123b0` | yes |

#### stack-read-old (`stack-read-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `999585f7452513a4` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [order.told_failed_not_applied] disjoint violated: 1 item of told_failed is also in kafka: 11 | `931d0c941ab83a14` | yes |

#### stack-errors-fix (`stack-errors-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `cc19dbda476a4b57` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `fb32f0301654d355` | yes |
| `disk=ioerr:5 dseed=1 \| run` | ERROR restart after the faults (redis) | `47701c32e88789f2` | yes |

#### stack-errors-old (`stack-errors-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `209fb1b264129ef7` | yes |

#### miniflux (`miniflux`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `b3d028a49ff721f0` | yes |
| `disk=none dseed=1 \| run K=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `edb99b547d0c089a` | yes |
| `disk=none dseed=1 \| run K=front@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `3b5894a161f6fb95` | yes |
| `disk=none dseed=1 \| run P=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `ae55876e3d32a5b9` | yes |
| `disk=none dseed=1 \| run P=postgres@10~50:4000` | VIOLATION [api.reads_acked_categories] acked c14 missing from the API | `c03554dad20bf266` | yes |
| `disk=none dseed=1 \| run O=postgres@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `a876920f14d5d738` | yes |
| `disk=none dseed=1 \| run O=front@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `ab345741ae40ab0d` | yes |
| `disk=none dseed=1 \| run O=front@10-15:req:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `ca153b23aee1c129` | yes |
| `disk=none dseed=1 \| run C=20` | OK invariants=2 held (categories.acked_exactly_once: acked=20 exactly once) | `c4eb6b2ae72ab9ba` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] category c18 stored 0 times | `730fdec8df1d2116` | yes |

#### miniflux-read (`miniflux-read`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: exactly-once holds: each of the 30 items of acked occu... | `ca1b716bce71cae0` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] exactly-once violated: acked c17 is in categories 0 times (of 20 ... | `562f9946eec79ab9` | yes |

#### stack-grpc-cur (`stack-grpc-cur`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `dd59dc534dad98d2` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `9442c7562964fa21` | yes |

#### stack-grpc-old (`stack-grpc-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `5525e06e8c7cc7d5` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ad76d475e916b7d8` | yes |

#### stack-grpc-planted (`stack-grpc-planted`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `4a70ccf46a07d57f` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | VIOLATION [stack.exactly_once] kafka order=10 present 2 times (must be exactly once) | `6e9f9d545da56de1` | yes |

#### stackgen (`stackgen-rs`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `7069a01e93b6190d` | yes |
| `disk=none \| run K=front@10+2` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `d047b98066d388b2` | yes |
| `disk=cut \| run C=15` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `a33bed27be75792b` | yes |

#### M136 → M140, fingerprint by fingerprint (x86_64)

| example | schedule | M136 fingerprint | M140 fingerprint |
|---|---|---|---|
| redis | `disk=none \| run` | `6875291e90072ae6` | `e7de012271d99d0a` |
| redis | `disk=none \| run K=redis@5+2` | `53f01e40711ccfdf` | `a678f41e98c1ca76` |
| redis | `disk=none \| run P=redis@5+2` | `7b2f0cd241ca4d4c` | `784ad4a2ab462de1` |
| redis | `disk=cut \| run C=12 R=everysec` | `beb90e457eed6cd7` | `3c57c1db1c05c410` |
| redis | `disk=cut \| run C=12 R=always` | `e94d248dc0263db7` | `dd4791c21b813908` |
| redis | `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | `9161ea99985a7dfc` | `025eecdf01c72e8c` |
| redis | `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` | `1ab0adbd21b84609` | `4c6dbfaae010725b` |
| redis-netem | `disk=none \| run` | `ccbf6defbc79c73d` | `7800b04875b8c265` |
| redis-netem | `disk=none \| run N=redis@5-10:loss,60%` | `0709180f00628487` | `2f689cdd7faf7278` |
| redis-netem | `disk=none \| run O=redis@5-8:resp:loss,100%` | `bf326d61b307fe1b` | `d20e0c833ff5c16b` |
| redis-freeze | `disk=none \| run` | `00e91cf282302822` | `24624254f42ab5de` |
| redis-freeze | `disk=none dseed=0 \| run F=redis<workload@5#1:150000` | `84ccade7ac058525` | `b5b4a5367a680c36` |
| redis-freeze | `disk=none dseed=0 \| run F=redis<workload@12#0:1` | `635d755e11838cc8` | `72c7301945727c36` |
| redis-hist | `disk=cut dseed=1 \| run C=12` | `92ff124ba40094ce` | `d663fa479b7f3112` |
| redis-hist | `disk=cut dseed=1 \| run C=12 R=everysec` | `c1e60506e16ab573` | `d66c54b6730f3c9b` |
| redis-live | `disk=none dseed=1 \| run` | `7dadf3d612873e3f` | `3a4c6f99afe4ccb3` |
| redis-live | `disk=none dseed=1 \| run K=redis@5+3` | `c1532e566d404833` | `b71a52f6e2e7e8c0` |
| redis-live | `disk=none dseed=1 \| run P=redis@18~0:20000` | `eb479d619d4f470b` | `daf98f52e714d103` |
| redis-proxy | `disk=none \| run` | `b32e2470daf79567` | `1b5ac9bc529d2b73` |
| redis-proxy | `disk=none \| run H=redisp@2-5:drop` | `8d758359f1f1cbb0` | `e8a48ac08bbcd6fe` |
| redis-proxy | `disk=none \| run H=redisp@1-3:reset K=redis@4+2` | `0b4d666bbd6c6873` | `0e5462ab0bc7fd75` |
| redis-ram | `disk=none \| run` | `66426149ddd86227` | `c781388975e573af` |
| redis-ram | `disk=cut \| run C=12 R=everysec` | `68c822c265af3c69` | `fd3e4a85d7ed5448` |
| redis-composed | `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | `9699814e85527567` | `aa64393559c1ad8c` |
| redis-composed | `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | `ecc0f218e02c56a4` | `bec487fb247ea7de` |
| redis-composed-hist | `disk=cut dseed=1 \| run C=12 W=0000002a,p4,q2,r100` | `eb9c4ad0a14e5f6e` | `a1216567acc59fdd` |
| redis-composed-hist | `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | `87d270f8f5466dff` | `2ea4d1ea752f384d` |
| redis-composed-read | `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | `e993025ac922649e` | `ab1689254694d5cd` |
| redis-composed-read | `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | `40a281b8671f1da6` | `01a664be89cc4e0e` |
| redis-errors | `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | `014ee59eba36eadf` | `65bcc8329249d3a9` |
| redis-errors | `disk=ioerr:1 dseed=281cc34161f446f4 \| run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,oanytime_read` | `b35ee148adf7d89a` | `2a9a315d66a895cf` |
| redis-bridge | `disk=none \| run` | `db3984d26b70e52b` | `f1d54ce6976d42f8` |
| redis-bridge | `disk=none \| run N=primary-replica@5-10:loss,100%` | `282f100fa2ebf1bd` | `64337cd16d534b92` |
| redis-bridge | `disk=none \| run K=replica@5+3` | `526a9673d7fff440` | `d37667b70deb571d` |
| redis-bridge | `disk=cut dseed=1 \| run C=8` | `f237f2cc19c52b43` | `f66d365cf05528c3` |
| redis-bridge | `disk=none \| run F=replica<primary@5#3:2000` | `cf3f34ef035a7f4e` | `0c6ce203fc4ad381` |
| redis-bridge | `disk=none \| run H=redisp@3-6:drop` | `1fb5b228073c9443` | `ba20935e807e328a` |
| rust-kv | `disk=none \| run` | `389bc482e51e2194` | `cc837d9a09a2e708` |
| rust-kv | `disk=cut \| run C=12` | `d4604820a762223b` | `bc268af1845c6e27` |
| rust-kv | `disk=cut \| run C=12 S=never` | `1591a9e914967b33` | `816d17daf4541f55` |
| rust-kv | `disk=none \| run K=rkv@8+2 B=1234` | `d4d56cb09417de82` | `cb256779528160cd` |
| avx2 | `disk=none \| run` | `334f56db42148c38` | `53342df87d6fb2a3` |
| redis-oci | `disk=none \| run` | `6147e0344f24a878` | `e677188840a31f83` |
| redis-oci | `disk=cut \| run C=12 R=everysec` | `231146340448942c` | `d69cd5ef4915a5ad` |
| redis-oci | `disk=none \| run K=redis@5+2` | `a86ca371b62d709b` | `ef47e3b95cc52618` |
| restate-oci | `disk=none \| run` | `01b32189a1ec35e8` | `e079c291660ca4bf` |
| restate-oci | `disk=none \| run K=restate@2+2` | `6127ac2b0ce527bc` | `9ec68bf557979229` |
| restate-oci | `disk=cut \| run C=4` | `4cde851a6dfcaa06` | `e51219e946666bc4` |
| shop-buggy | `disk=none dseed=1 \| run` | `b9568c33cf16930e` | `4bb0a1fe876cdb85` |
| shop-buggy | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | `c0b64b0991adc479` | `8fc62dc461277c3f` |
| shop-buggy | `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | `fbf9e64b1e159b9d` | `9671f2b17a161583` |
| shop-buggy | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | `5a8193e66693ed9a` | `9d12c1710fb3caf3` |
| shop-fixed | `disk=none dseed=1 \| run` | `a38984a542210bd8` | `b53ecc59b80008fc` |
| shop-fixed | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | `92e91caf0502fe49` | `ea6404c9dd299dde` |
| shop-fixed | `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | `a9b788c96aa2ccf3` | `ae0a30fbdbf638ed` |
| shop-fixed | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | `5c628c155c3f0b77` | `944f214df3babac2` |
| etcd-3.5.5 | `disk=none dseed=1 \| run` | `ed50aa191ffd0a39` | `c2f71429accf734c` |
| etcd-3.5.5 | `disk=cut dseed=1 \| run C=30` | `200822da59c2ec72` | `97fd98642e8f2614` |
| etcd-3.5.5 | `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | `3b47960fb89dd908` | `5d0dde7808bdb377` |
| etcd-3.5.6 | `disk=none dseed=1 \| run` | `fe7fa0444ca89f70` | `0428c99be308463e` |
| etcd-3.5.6 | `disk=cut dseed=1 \| run C=30` | `b95213ade20090b2` | `347369b1bc71618b` |
| etcd-3.5.6 | `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | `220a997535814ade` | `0c3cae8d4892f38f` |
| etcd-hc-3.5.5 | `disk=none dseed=1 \| run` | `edec19943cb68c42` | `145703f206f2a797` |
| etcd-hc-3.5.5 | `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | `2367445439ec5bc8` | `e91c242f589bb74c` |
| etcd-hc-3.5.6 | `disk=none dseed=1 \| run` | `f9142657ac4708ac` | `fe88a223de851b5a` |
| etcd-hc-3.5.6 | `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | `55e6b1dda9a9b14e` | `3c036a3f62a06536` |
| voting | `disk=none dseed=1 \| run` | `5178e8c08ecf93cc` | `be6cc370a553eddd` |
| voting | `disk=none dseed=1 \| run K=worker@5+5` | `473a8fea22bcde0f` | `baf15f205fd6f4e0` |
| voting | `disk=none dseed=1 \| run C=12` | `3e0c121b964fd925` | `378cb9bf32c7a22c` |
| voting | `disk=none dseed=1 \| run N=vote-redis@5-12:loss,100%` | `6d6fa005c6971bbc` | `62a1314567295e9b` |
| voting-generated | `disk=none dseed=1 \| run` | `7eed916a16de1b82` | `8195138a6ac6e761` |
| voting-generated | `disk=none dseed=1 \| run K=worker@5+5` | `b43a721c180f16ae` | `cb6dab11caa9613f` |
| voting-loopback | `disk=none dseed=1 \| run` | `32a8fda173a1552c` | `c414c6cea52839bd` |
| voting-loopback | `disk=none dseed=1 \| run K=worker@5+5` | `76991ff091db8c00` | `d5ce7e93d5e95ecd` |
| stack-fix | `disk=none dseed=1 \| run` | `af02273eb613d4f1` | `e43499921a8ec703` |
| stack-fix | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `952f376397b32f0d` | `aad6a563e2046361` |
| stack-fix | `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | `7c442cdf89fbafe4` | `ac2f1606e253ca3e` |
| stack-fix | `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | `7c836e665dc86a6a` | `4d94660c4856fb70` |
| stack-fix | `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | `9c1a09eb84c10492` | `67e6b3e6375df28c` |
| stack-fix | `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | `134db765072a2a4b` | `fcf450f3f4722992` |
| stack-fix | `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | `392607638b9a2814` | `43a27aa3950d1c0d` |
| stack-fix | `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | `c1b2c134dfa2408d` | `6107e823320da4c7` |
| stack-fix | `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | `464325fad244757b` | `683e16b5dff9c452` |
| stack-fix | `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | `3bae7cbcc42c41d2` | `97b8a3f31c8cb87a` |
| stack-fix | `disk=none dseed=1 \| run N=kafka@5-10:delay,300ms` | `4a3c93815639896d` | `b51e800a0d7a31c3` |
| stack-fix | `disk=none dseed=1 \| run O=mysql@5-10:resp:delay,1000ms` | `738c3ce88bc57adf` | `a0ecb6f21e977306` |
| stack-fix | `disk=none dseed=1 \| run N=mysql@3-8:delay,50ms,loss,20%` | `a1d1231abc0fa65e` | `64361e680ffa7d23` |
| stack-old | `disk=none dseed=1 \| run` | `0dde9af6eff8b2e7` | `cf581cbbbf91e2fa` |
| stack-old | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `b9e6fc324fa8ed76` | `8fc461db23ef6642` |
| stack-old | `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | `b2c3e23999830920` | `17996aa138ca25da` |
| stack-old | `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | `07bc12a5783716de` | `ea237a38611e2dab` |
| stack-old | `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | `30d5c3e2073cfac1` | `78a2445f1ca68973` |
| stack-old | `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | `3681cf66a8a666f6` | `b39f27cdbb31ebd3` |
| stack-old | `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | `064aac00f5becfe0` | `ea2cdfdec04ca0e8` |
| stack-old | `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | `5ef55431f88e353e` | `bb2ec2417c340bb2` |
| stack-old | `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | `1d415e4720037456` | `3ebb3c4d243bc953` |
| stack-old | `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | `7f95d1abc26d0d83` | `766607a5e83edd4a` |
| stack-read-fix | `disk=none dseed=1 \| run` | `84d920df46d6e1be` | `31747d42da9db86a` |
| stack-read-fix | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `2b09df294c12b4c4` | `c1baad113f4123b0` |
| stack-read-old | `disk=none dseed=1 \| run` | `c9d9b56b99037daf` | `999585f7452513a4` |
| stack-read-old | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `ec83563bdd9838ef` | `931d0c941ab83a14` |
| stack-errors-fix | `disk=none dseed=1 \| run` | `348cd1e351ee0583` | `cc19dbda476a4b57` |
| stack-errors-fix | `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | `e17626b8099af7e3` | `fb32f0301654d355` |
| stack-errors-fix | `disk=ioerr:5 dseed=1 \| run` | `6d8a523700f109e4` | `47701c32e88789f2` |
| stack-errors-old | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | `ecbdc4b8fed098c8` | `209fb1b264129ef7` |
| miniflux | `disk=none dseed=1 \| run` | `bf92f72fb2ee7743` | `b3d028a49ff721f0` |
| miniflux | `disk=none dseed=1 \| run K=postgres@10+3` | `94f7dff586a28ece` | `edb99b547d0c089a` |
| miniflux | `disk=none dseed=1 \| run K=front@10+3` | `fe35c945ecc011c3` | `3b5894a161f6fb95` |
| miniflux | `disk=none dseed=1 \| run P=postgres@10+3` | `f2a0a493e00ee1fa` | `ae55876e3d32a5b9` |
| miniflux | `disk=none dseed=1 \| run P=postgres@10~50:4000` | `17ee484de861e07a` | `c03554dad20bf266` |
| miniflux | `disk=none dseed=1 \| run O=postgres@10-15:resp:loss,100%` | `22222bf0ae764cf7` | `a876920f14d5d738` |
| miniflux | `disk=none dseed=1 \| run O=front@10-15:resp:loss,100%` | `2165a6d333a70bb5` | `ab345741ae40ab0d` |
| miniflux | `disk=none dseed=1 \| run O=front@10-15:req:loss,100%` | `632a3ecf4a94866f` | `ca153b23aee1c129` |
| miniflux | `disk=none dseed=1 \| run C=20` | `de5339c8ae51f04a` | `c4eb6b2ae72ab9ba` |
| miniflux | `disk=none dseed=1 \| run C=20 S=off` | `5cd2fcd9fc424b6b` | `730fdec8df1d2116` |
| miniflux-read | `disk=none dseed=1 \| run` | `d9945a951b2253fd` | `ca1b716bce71cae0` |
| miniflux-read | `disk=none dseed=1 \| run C=20 S=off` | `aa403b06c9e5391d` | `562f9946eec79ab9` |
| stack-grpc-cur | `disk=none dseed=1 \| run` | `444c61285b66c236` | `dd59dc534dad98d2` |
| stack-grpc-cur | `disk=none dseed=1 \| run H=orders.place@10-13:drop` | `9242ed976a23fb24` | `9442c7562964fa21` |
| stack-grpc-old | `disk=none dseed=1 \| run` | `c3ea21902906e620` | `5525e06e8c7cc7d5` |
| stack-grpc-old | `disk=none dseed=1 \| run H=orders.place@10-13:drop` | `1dfcf63387024add` | `ad76d475e916b7d8` |
| stack-grpc-planted | `disk=none dseed=1 \| run` | `fb676b97ed846766` | `4a70ccf46a07d57f` |
| stack-grpc-planted | `disk=none dseed=1 \| run H=orders.place@10-13:drop` | `441b8084ef4767db` | `6e9f9d545da56de1` |
| stackgen | `disk=none \| run` | `05f46c749f5f6cc5` | `7069a01e93b6190d` |
| stackgen | `disk=none \| run K=front@10+2` | `d7c83fa446cd0313` | `d047b98066d388b2` |
| stackgen | `disk=cut \| run C=15` | `aef706013160bb3a` | `a33bed27be75792b` |

#### Verdict lines that differ from M136 (x86_64)

- redis-netem `disk=none | run`
  - M136: HYDO-ORACLE: OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=7.99)
  - M140: HYDO-ORACLE: OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=8.05)
- redis-netem `disk=none | run N=redis@5-10:loss,60%`
  - M136: HYDO-ORACLE: VIOLATION [net.no_retransmission] retrans=20 timeouts=16 uptime=14.08
  - M140: HYDO-ORACLE: VIOLATION [net.no_retransmission] retrans=12 timeouts=9 uptime=16.34
- redis-netem `disk=none | run O=redis@5-8:resp:loss,100%`
  - M136: HYDO-ORACLE: VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.08
  - M140: HYDO-ORACLE: VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.16
- redis-hist `disk=cut dseed=1 | run C=12 R=everysec`
  - M136: HYDO-ORACLE: VIOLATION [redis.kv_linearizable] key k:1: an acknowledged write is lost or a read is stale: a read that began after the write of v1 completed returned nil: p0 write k:1=v1 (ok, t=7.76-7.78s); p9 read k:1 -> nil (ok, t=7.73-7.74s (recovery boot)) (11 keys not linearizable)
  - M140: HYDO-ORACLE: VIOLATION [redis.kv_linearizable] key k:1: an acknowledged write is lost or a read is stale: a read that began after the write of v1 completed returned nil: p0 write k:1=v1 (ok, t=7.80-7.82s); p9 read k:1 -> nil (ok, t=7.76-7.77s (recovery boot)) (11 keys not linearizable)
- redis-live `disk=none dseed=1 | run P=redis@18~0:20000`
  - M136: HYDO-ORACLE: VIOLATION [redis.pending_resolved] not satisfied 10.14s after heal, deadline 10s (last check rc=1: pending=1)
  - M140: HYDO-ORACLE: VIOLATION [redis.pending_resolved] not satisfied 10.16s after heal, deadline 10s (last check rc=1: pending=1)
- redis-composed-hist `disk=cut dseed=1 | run C=12 R=everysec W=0000002a,p4,q2,r100`
  - M136: HYDO-ORACLE: VIOLATION [redis.kv_linearizable] key k:12: an acknowledged write is lost or a read is stale: a read that began after the write of v12 completed returned nil: p12 write k:12=v12 (ok, t=8.72-8.75s); p900 read k:12 -> nil (ok, t=7.79-7.80s (recovery boot)) (2 keys not linearizable)
  - M140: HYDO-ORACLE: VIOLATION [redis.kv_linearizable] key k:12: an acknowledged write is lost or a read is stale: a read that began after the write of v12 completed returned nil: p12 write k:12=v12 (ok, t=8.83-8.85s); p900 read k:12 -> nil (ok, t=7.83-7.84s (recovery boot)) (2 keys not linearizable)
- etcd-3.5.5 `disk=cut dseed=1 | run C=30`
  - M136: HYDO-ORACLE: VIOLATION [etcd.members_agree] members disagree: revisions 98 99 98; k89 mod_revisions 97 97 97
  - M140: HYDO-ORACLE: VIOLATION [etcd.members_agree] members disagree: revisions 99 99 98; k89 mod_revisions 96 96 96
- etcd-hc-3.5.5 `disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1`
  - M136: HYDO-ORACLE: VIOLATION [etcd.kv_linearizable] key k31: these 4 operations cannot be ordered consistently with real time and a register: p1 write k31=v31 (unknown outcome, t=18.52-20.77s); p10 write k31=v31@96 (ok, t=35.96-36.01s); p20 read k31 -> v31@97 (ok, t=36.28-36.32s); p21 read k31 -> v31@96 (ok, t=36.32-36.37s) (59 keys not linearizable)
  - M140: HYDO-ORACLE: OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable)
- etcd-hc-3.5.6 `disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1`
  - M136: HYDO-ORACLE: OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable)
  - M140: HYDO-ORACLE: OK checks=1 held (etcd.kv_linearizable: 189 operations on 90 keys linearizable)
- voting `disk=none dseed=1 | run C=12`
  - M136: HYDO-ORACLE: VIOLATION [votes.counted_soon] not satisfied 60.79s after heal, deadline 60s (last check rc=1: too few votes counted: 0 of 12 acknowledged votes are in PostgreSQL; voter0 (vote a) is not)
  - M140: HYDO-ORACLE: VIOLATION [votes.counted_soon] not satisfied 60.69s after heal, deadline 60s (last check rc=1: too few votes counted: 0 of 12 acknowledged votes are in PostgreSQL; voter0 (vote a) is not)
- stack-errors-fix `disk=ioerr:5 dseed=1 | run`
  - M136: HYDO-ORACLE: ERROR restart after the faults (mysql)
  - M140: HYDO-ORACLE: ERROR restart after the faults (redis)
- miniflux `disk=none dseed=1 | run P=postgres@10~50:4000`
  - M136: HYDO-ORACLE: OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once)
  - M140: HYDO-ORACLE: VIOLATION [api.reads_acked_categories] acked c14 missing from the API
- miniflux-read `disk=none dseed=1 | run C=20 S=off`
  - M136: HYDO-ORACLE: VIOLATION [categories.acked_exactly_once] exactly-once violated: acked c18 is in categories 0 times (of 20 in acked); missing 2: c18, c19
  - M140: HYDO-ORACLE: VIOLATION [categories.acked_exactly_once] exactly-once violated: acked c17 is in categories 0 times (of 20 in acked); missing 3: c17, c18, c19

## 10. The pinned reproducers as agent images (M145, 2026-11-02)

Milestone 145 removed the engines' shell-image path, so the two hand-made shell images that were
the pinned reproducers since M38/M39 can no longer run. The reproducers are now the stack example
(`hydo/onboard/examples/stack/hydo.yaml`) built as agent images with the same two order services,
which `hydo/onboard/examples/stack/build-reproducers.sh` builds from the repository's history with
the pinned Go 1.26.5 (byte for byte the binaries the old images carried) and then builds into
images with the bare-metal machine's Nix (M140 part B's tier-B recipe). Each image was built
twice with identical bytes, and both are identical to M140 part B's images of the same case.

| Image | Order service | Image sha256 | `hydo run` from a fresh boot, twice | Fingerprint |
|---|---|---|---|---|
| `explore-ob-stack-old-disk.img` | M38's (git `d26ed1d`, `3ddaf8ba…`) | `e053a633cfd7e757…` | `VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed` | `8fc461db23ef6642` (wfp `88f321c464d7b948`, ctl `86227d78c3d85ba7`) |
| `explore-ob-stack-fix-disk.img` | M39's fix (git `0112b48`, `1f18f855…`) | `0afcbed997dc8293…` | `OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0)` | `aad6a563e2046361` (wfp `6d8fe1825192ece3`, ctl `aa8efc5a7f8698ee`) |

The schedule is the old one, `disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24`, and the
violated order is the old one, 11. The fresh-boot values equal M140 part B's S1 values for the same
images (section 9.2, `stack-old`, `stack-fix`). They replace `2403499c9a3f8419` (`explore-disk.img`)
and `f87130453609a5d6` (`explore-fix-disk.img`) in `hv/hvx/deploy-metal.sh`,
`hv/hvx/gc/side-suite.sh` and `hv/hvx/safety/side-check.sh`. On the bare-metal machine the images are in
`~/hydo/build/m145/`.

## 11. The Linux release (M149, 2026-11-04)

The Linux release tarball built from M149's branch (`hydo-linux-0.1.0.tar.gz`, sha256
`3083f40f9abe43ae…`, twice with the same bytes), installed with `install.sh --no-module` into
clean prefixes on the bare-metal machine (main module `B5E97B37504F1A5F508BA30`, the host-call port on),
each schedule run twice with `hydo run` from a fresh boot, as the user (the engine as
`bin/hydo-hvx` under sudo) and, for the first two rows, also as root (the engine inside `hydo`);
every pair identical, and both ways equal. The release's Redis example has its own manifest text
(`src: files/redis-…`), so its image and values are new; the other rows are repository examples
built with the installed release, whose images have M140's bytes and whose values are section 9.2's.

| image (sha256) | schedule | verdict | fingerprint |
|---|---|---|---|
| release `examples/redis` (`afd27220c09d9745…`) | `disk=none \| run` | OK (acked=20 all present) | `9a42e3d59c3b3a53` |
| release `examples/redis` | `disk=cut \| run C=12 R=everysec` | VIOLATION key k:1 lost | `63736100f744c3ff` |
| release `examples/redis` | `disk=lying:3 dseed=eac0b527c5ac67d9 \| run C=20` (minimal, from the exploration) | VIOLATION key k:3 lost | `29910066ab8efc9d` |
| release `examples/redis` | `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` (minimal, from the exploration) | VIOLATION key k:0 lost | `9b99edc5bee28261` |
| release `examples/redis/hydo-netem.yaml` (`2f62fa5c5001c1b9…`) | `disk=none \| run N=redis@3-6:delay,300ms` | OK (retrans=0) | `45d3cf4cd815fb53` |
| repository `examples/redis` (`bde714a7…`) | `disk=none \| run` | OK | `e7de012271d99d0a` (9.2) |
| `redis-bridge` (`490d7240…`) | the six schedules of 9.2 | as 9.2 | as 9.2 |
| `redis-composed` (`39d9e74f…`) | the two schedules of 9.2, and `repro --regressions` | as 9.2 (PASS `aa64393559c1ad8c`) | as 9.2 |
| `redis-composed-read` (`ecb7f1e0…`) | `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK | `ab1689254694d5cd` (9.2) |

## 12. The node-kv example with the Apache-2.0 package file (M152, 2026-11-05)

Milestone 152 set the JavaScript SDK's `package.json` license field to `Apache-2.0` and removed its
`conformance` script (it named the Go-binary harnesses deleted in M110). The node-kv example copies
that file into its images (`/app/node_modules/hydo-assert/package.json`), so the node-kv images have
new bytes and new fingerprints; no other image contains the file. Only the Mac image is recorded
(section 9.1); there is no recorded x86_64 node-kv image. These values supersede section 9.1's
node-kv table.

How: a release prefix built from this branch with `hydo/dist/mac/build-release.sh` (the cached
`assets/`, hydo-agent `099e5444…`) and installed with `install.sh --prefix`; the aarch64 Node.js and
its libraries fetched from the public Nix cache with `hydo-nix-fetch --closure` and staged as
`fetch-node-arm64.sh` stages them (its file list, `chmod -R u+w`, the one absolute symlink made
relative; every file as in `files.sha256`); `hydo build hydo-mac.yaml` twice into two directories,
then every schedule twice with `hydo run --image` (a fresh boot each), one VM at a time.

Images (two builds byte-identical each): before the change (the tree at `a4d9531`) initramfs
`ce00049267a22a84…`, after it `2ac9dc0cdc32c57b…`; the root disk `d031f382…` and the data disk
`9dba2707…` are the same before and after. Unpacked, the two initramfs trees differ only in
`app/node_modules/hydo-assert/package.json` (the license line and the removed script line); every
other path has the same bytes, mode and owner.

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `3def1ce155691292` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `59bd74f26a0e3d97` | `8de6e3d2ba8ff5d8` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `33c406be62f5df36` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `ae1db81f2458fe15` | `39129eaa9b7e2227` | yes |

The verdict lines and the ctl values are section 9.1's word for word: the guest behaves the same,
only the image bytes moved. Fingerprint by fingerprint:

| schedule | section 9.1 (M140) | the tree before M152 (= M143's runs) | M152 |
|---|---|---|---|
| `disk=none \| run` | `ab38514eec59f829` | `0ce0c2a34c0509d0` | `3def1ce155691292` |
| `disk=cut \| run C=12 S=never` | `376432e7f641b27f` | `5df3ab5506474f11` | `59bd74f26a0e3d97` |
| `disk=cut \| run C=12` | `0ccaca40efede16b` | `14932bc02067c1dd` | `33c406be62f5df36` |
| `disk=none \| run K=nkv@8+2 B=1234` | `41e70d32728c4aa4` | `400119ed741b8151` | `ae1db81f2458fe15` |

**Section 9.1's node-kv values are not reproduced from the tree.** The image built from `a4d9531`
before the change (`ce000492…`) is not section 9.1's (`0120fa41…`) and gives the middle column, each
schedule twice identical, with 9.1's ctl values; Milestone 143 built the same `ce000492…` image and
measured the same four fingerprints with both Mac engines. M140 built its images from M128's staged
folders, which were not kept, so the difference was not traced to a file. The image's bytes do
depend on the staged files' modes: the same staging with every Node.js file read-only (as in the Nix
store) gives a third initramfs, `87a25da8…`. The values above are what the repository's own staging
gives.

## 13. The image record, fingerprint epoch 1 (M155, 2026-11-05)

Milestone 155 put `/etc/hydo/image.json` into every image (the guest kernel the image was built
for, by sha256, and the fingerprint epoch; crate `hydo/crates/hydo-image`) and a build record
`<image file>.json` next to it. Every image therefore has new bytes and every fingerprint moved
once; the agent (0.3.0), both engines, the controller and the module are M140's, so the verdicts
are M140's except three x86_64 schedules whose guest timing moved (13.2). This is the one re-recording the user accepted (2026-11-05), and these values are
**fingerprint epoch 1**: a later change that moves a recorded fingerprint must raise
`hydo_image::EPOCH` and add a section here. Section 9's values (and section 12's node-kv values)
stay as the record of the images without the record; images built before M155 still run with them
(checked: M140's Redis image gives `e7de012271d99d0a` with the one-line warning on the bare-metal
machine, M154's Mac release Redis image `76bce7dce084f41e` and `0f139f3fc3398595` on the Mac).

How: on the bare-metal machine M140 part B's `img2.sh` and `list.sh` with this branch's tree
(`~/hydo-m155`, work directory `~/hydo-m155-work`, results `/var/tmp/m155`): every x86_64 example
image built twice from the same inputs (the inputs M146 had deleted made again: the Redis binaries
from the release assets, the two order services, miniflux and `hydo-read` dumped from M140's own
images, the voting app's images fetched by digest), then every recorded schedule twice with
`hydo-bench list --repeat 2`. On the Mac a release prefix built from the branch, every example
staged from the tree without the build machine (docs/C2-MAC.md's inputs, M152's node-kv recipe),
every image built twice by `hydo build`, every schedule twice with `hydo run --image` from a fresh
boot. Every pair of builds was byte-identical and every schedule gave one fingerprint both times.

### 13.1 Mac (hvm), every recorded schedule twice

67 schedules on 19 images (section 9.1's, node-kv's from section 12), 67 twice identical; every
verdict line is word for word section 9.1's (section 12's for node-kv). 55 of the 67 ctl values are
section 9.1's; the 12 below changed with the same verdicts (not investigated; the shop's programs
were built here with go.dev's Go 1.26.5, and M140's staged inputs of these examples were not kept,
so they cannot be compared file by file, as M152 found for node-kv).

Initramfs sha256 prefixes (each built twice; "identical" = initramfs, root disk, data disk and build record of the two builds byte-identical): redis `da37b9ad9b9a76ae…` (identical), redis-hist `ad5c41a6737aca55…` (identical), redis-read `22b42bca27a28109…` (identical), redis-ram `d4a9bdc87d401810…` (identical), redis-bridge `bec8a25f149afbf4…` (identical), ledger `c4ddd14aacc10fbe…` (identical), ledger-spin `f5d34550e7a94e31…` (identical), redis-composed `bca39f97dcf6138c…` (identical), redis-composed-racy `e7630a982d125490…` (identical), node-kv `51d67eb22c8a1956…` (identical), python-kv `4bf7513bebe38613…` (identical), python-kv-hist `4f4a31973168823e…` (identical), rust-kv `23b338645b762cd6…` (identical), shop-buggy `85a735afd7c08029…` (identical), shop-fixed `a64ef81490fd1a53…` (identical), grpc-buggy `a2418d0953443beb…` (identical), grpc-fixed `fc1e3e19ef5bd30d…` (identical), voting `9d97d9622e8db395…` (identical), redis-oci `8364b670defa9a51…` (identical).

#### redis

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `25b1201e4067fa47` | `0b9e60d528cc2dc7` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `2ab0f6beb9b1bd6b` | `ff4caa09a583ed70` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `75579009064a763d` | `a30845edf992e360` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `793ebdf0717899ec` | `c1c6baa6a88a32a5` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `18ed9ff59e7ad7fa` | `b4f7adea42cc337b` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `61c5de2f05839067` | `99bc0f20aeb15106` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `8b3a42bb0df08d7e` | `1ba0f7a39eb8e843` | yes |
| `disk=none \| run T=10:3600` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `933427bb2819f822` | `12b5294b6b4cadc3` | yes |

#### redis-hist

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut dseed=1 \| run C=12` | OK invariants=0 held (); history OK | `0b3af533ea748e5f` | `625922e1528c1e3c` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | OK invariants=0 held (); history VIOLATION [redis.kv_linearizable] | `a33fd1d16cdc3728` | `037a11f0d411212e` | yes |

#### redis-read

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: subset holds: all 20 items of acked are in redis (redis h... | `22331a2dab9a2dfe` | `8476030a9d80c61e` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] subset violated: 12 items of 12 in acked not in redis: k:0=v0, k:1=v... | `f95fb7be44801a83` | `19138a12b5ab46af` | yes |

#### redis-ram

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `d9a41c337d7b63bf` | `23d53d97e4d64efa` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `01dcdc20e141301b` | `93ebd8cf695051da` | yes |

#### redis-bridge

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `2370cbe14fdd9697` | `6a8e130bfbf9fef2` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `3d41fb4244f6c723` | `bb902d00160d7286` | yes |
| `disk=none \| run O=primary-replica@5-10:req:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `a716b817bd8af377` | `f2d609514474f032` | yes |
| `disk=none \| run O=primary@5-8:resp:delay,300ms` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `029ef58ab82151a6` | `ab4e21b1c8e8ce83` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `5959be48d244f62b` | `38e15c68d9963cda` | yes |
| `disk=cut \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `432a95ba85834fbb` | `1ac708d19e5718f8` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `ccf16aceb5c5066f` | `db293b879f216430` | yes |
| `disk=none \| run F=primary<workload@5#2:1000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `578bf6fe0f4f4bac` | `4ff9110839ed6dfd` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `44da508e705c7aca` | `a31807d7688f461c` | yes |

#### ledger

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `de32ee8c5ee6f69a` | `c9fd7b0f480180cd` | yes |
| `disk=none \| run O=front@5-8:resp:loss,100%` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `6f14d9d934c84dc0` | `c15ce94773db279a` | yes |
| `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION [ledger.acked_exactly_once] entry 1 stored 2 times | `a897f325702de127` | `aea4258f9a112981` | yes |
| `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `6da86067e774a48d` | `8552ef571c61fb8f` | yes |
| `disk=none \| run N=front@0-30:loss,60%` | VIOLATION [ledger.acked_exactly_once] entry 9 stored 2 times | `a18e3e452a41ba3e` | `e50f44fc62acc4da` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `6abc98fcb198db88` | `382b70a8955072c9` | yes |

#### ledger-spin

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `e32475acef7341b4` | `930f998155f77b29` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `0caf9beb9125978d` | `012f769bba1db408` | yes |

#### redis-composed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `adfdd1128b47b2e1` | `64228edfdcc3de50` | yes |

#### redis-composed-racy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | VIOLATION [total_conserved] balances sum to 503, not 500 | `9bcde106d244c7c8` | `cc25282fa6ee57cf` | yes |

#### node-kv (superseded by section 14, M157)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `eb0811dba0a01354` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `846b02b08b161419` | `8de6e3d2ba8ff5d8` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `f9847e1a9e687a15` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `9e6672f7f7f52ff7` | `39129eaa9b7e2227` | yes |

#### python-kv (superseded by section 14, M157)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `a7f4b2de5ba82687` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `5b58d933c00349b6` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `fc139f93a791ec46` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `ed2d03a6970eb0bd` | `ece160c3964f7f03` | yes |

#### python-kv-hist (superseded by section 14, M157)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `ee666af6229fc2e5` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `458abf5f089819cb` | `4a799f3170dcb41b` | yes |

#### rust-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `547f7b419affd943` | `bc6fe53352fa8359` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `6d32a2e7ac9aecce` | `ded68d8cdfcf34f0` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `c221cba5d3298571` | `22168eb0b60f5ac1` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `3d188ff780a5d993` | `0ad74bffbfd87b1f` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `439f45f7fe19dff7` | `d20fa8483538cd42` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `dbc272cd3cc600ec` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `0178e70f40b959a5` | `31ca45bf4f0d1dfe` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `6e6269b2af51beab` | `53e69be30dff5ad1` | yes |

#### shop-buggy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `61ffe63cbc129405` | `826e4ec0225b2c54` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `9d1af8f92957f335` | `5dc99b20820664bb` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `1091377a63d61bd8` | `f013583309d54686` | yes |
| `disk=none \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 23,26) | `bc0b730de9eca400` | `f5397144a2781746` | yes |

#### shop-fixed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `4fe68ccdf8a48174` | `597c4a89c39ff4f7` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `7863ce11fc7b8b77` | `fed12be8a59f8f63` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `bd59301989bf12b0` | `b86e3f5a800e5cc0` | yes |

#### grpc-buggy

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | VIOLATION [orders.consistent] VIOLATION request_id r1 created 5 orders: o11 o1 o2 o3 o4 | `28417fb1c34bcfac` | `e6766eac8f5475fc` | yes |

#### grpc-fixed

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | OK invariants=2 held (compose.commands: started=2 ok=1 failed=1 timeout=0 violation=0 killed=0 skipped=0) | `ec450ee463da3f5d` | `9fcb1c71aa47fc8b` | yes |

#### voting

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `43733e843f702aac` | `f4a6471774a0d569` | yes |
| `disk=none \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `51104aebf127569b` | `f530e788c6cc833f` | yes |
| `disk=cut \| run C=12` | VIOLATION [votes.counted_soon] not satisfied 60.64s after heal, deadline 60s (last check rc=1: too few vote... | `0933dc3789d8653a` | `36baf4229229cd1c` | yes |
| `disk=none \| run N=vote-redis@5-12:loss,100%` | OK invariants=1 held (votes.counted: all 13 acknowledged votes counted) | `d4b302ed2c44ceca` | `720f005b311db7de` | yes |

#### redis-oci

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `fbbf1515e8aaf063` | `f7161ddb8479f702` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `7b97be9a18a42f3c` | `a8dee7e04969f117` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `fed9f1895e37e2f6` | `cd6f8435f894b551` | yes |

#### ctl values that changed (verdict unchanged)

| example | schedule | M140 ctl | M155 ctl |
|---|---|---|---|
| python-kv | `disk=none \| run B=1234` | `91e0931a5e07100c` | `ece160c3964f7f03` |
| shop-buggy | `disk=none \| run` | `0927ec66fcfd9b81` | `826e4ec0225b2c54` |
| shop-buggy | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | `a95a5e731424d505` | `5dc99b20820664bb` |
| shop-buggy | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | `66806064cf1f0f7a` | `f013583309d54686` |
| shop-buggy | `disk=none \| run H=payments.charge@6-9:reset,p=50` | `c9bf99c5c985c965` | `f5397144a2781746` |
| shop-fixed | `disk=none \| run` | `0707ac0014b93d7a` | `597c4a89c39ff4f7` |
| shop-fixed | `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | `95eb07beb9a9a85b` | `fed12be8a59f8f63` |
| shop-fixed | `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | `ec7c5b8d2145b6f9` | `b86e3f5a800e5cc0` |
| voting | `disk=none \| run` | `176d7acdf4d05c6e` | `f4a6471774a0d569` |
| voting | `disk=none \| run K=worker@5+5` | `73f458b22605fd8d` | `f530e788c6cc833f` |
| voting | `disk=cut \| run C=12` | `ce1382c7c8092626` | `36baf4229229cd1c` |
| voting | `disk=none \| run N=vote-redis@5-12:loss,100%` | `880937d1be914793` | `720f005b311db7de` |

### 13.2 x86_64 (hvx), every recorded schedule twice

126 schedules on 37 images (section 9.2's without the outside project's image, which M151
removed), 126 twice identical (`hydo-bench list --repeat 2`, one boot per image, the host-call port
on, the loaded main module `B5E97B37504F1A5F508BA30`). Every image was built twice with the same
bytes. 116 verdict lines are word for word section 9.2's, 7 differ in timing words only, and 3
changed: etcd 3.5.5's `K=etcd1@10+1 P=etcd3@26+1` is a VIOLATION again (a client's revision goes
back, the second symptom of etcd issue 14685 that M78 recorded and M140 lost), miniflux's
`P=postgres@10~50:4000` is OK again (M136's class), and the error-log stack's `disk=ioerr:5` ends
`ERROR restart after the faults` for MySQL instead of Redis. The images' root file systems gained a
file, so the guest's timing moved (as M140's agent change moved two classes the other way); on the
Mac, whose root is the initramfs in RAM, no verdict moved.

Image sha256 prefixes: avx2 `2dcb4305…`, compose-voting_hydo-loopback `2adc16fa…`, compose-voting_hydo.generated `6d6fe82d…`, compose-voting_hydo `2ac9257b…`, etcd-3.5.5 `24df9e71…`, etcd-3.5.6 `da477dd1…`, etcd-hc-3.5.5 `93d39cd0…`, etcd-hc-3.5.6 `3afb6e73…`, miniflux-read `4bac2fe3…`, miniflux `a8b5f3e3…`, redis-bridge_hydo `8ecaee35…`, redis-composed-hist_hydo_hydo `777366e0…`, redis-composed-read_hydo_hydo `c059e152…`, redis-composed_hydo_hydo `e419d321…`, redis-errors_hydo_hydo `7a2ba166…`, redis-oci `7943f914…`, redis-ram_hydo `34d77414…`, redis_hydo-freeze `f109500a…`, redis_hydo-hist `8cad29c9…`, redis_hydo-live `6d70f805…`, redis_hydo-netem `a7df2d6a…`, redis_hydo-proxy `8ae51a08…`, redis_hydo `eb8a9cb1…`, restate-oci `77978db5…`, rust-kv-v2 `20fb50e4…`, shop2-buggy `0d627229…`, shop2-fixed `6624e195…`, stack-errors-fix `005b7a8a…`, stack-errors-old `168ad0d2…`, stack-fix `1de40b6a…`, stack-grpc-cur `b47fdf01…`, stack-grpc-old `7e5159f3…`, stack-grpc-planted `6d3e4cfc…`, stack-old `dc8f8a89…`, stack-read-fix `2932045a…`, stack-read-old `11f9e419…`, stackgen-rs `3ca09cc8…`.

#### redis (`redis_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `37fcf7394c6d8099` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `002ee5dae3a491cf` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `726798145db9e2f9` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `5c245487b5f1b4f8` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `238e54a7507a11d2` | yes |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK invariants=1 held (redis.acked_writes_survive: acked=15 all present) | `068a688045997d87` | yes |
| `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `88473d78441a2ef0` | yes |

#### redis-netem (`redis_hydo-netem`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=8.05) | `0c1ab03e91f01184` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | VIOLATION [net.no_retransmission] retrans=12 timeouts=9 uptime=16.34 | `12bef4fa21a9cca9` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.15 | `63e8785c9e26a9cc` | yes |

#### redis-freeze (`redis_hydo-freeze`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `c54a1256c7219eef` | yes |
| `disk=none dseed=0 \| run F=redis<workload@5#1:150000` | OK invariants=1 held (redis.acked_writes_survive: acked=5 all present) | `722243ef48894ba4` | yes |
| `disk=none dseed=0 \| run F=redis<workload@12#0:1` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `594ee228b16f2ca2` | yes |

#### redis-hist (`redis_hydo-hist`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=cut dseed=1 \| run C=12` | OK checks=2 held (redis.kv_linearizable: 32 operations on 20 keys linearizable) | `e8ad171a302c2cb8` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.kv_linearizable] key k:1: an acknowledged write is lost or a read is stale: a read that be... | `1f6af4c6d0049ca5` | yes |

#### redis-live (`redis_hydo-live`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `ebdc9f149aaa9f90` | yes |
| `disk=none dseed=1 \| run K=redis@5+3` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `b4388cf37bd1ca81` | yes |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | VIOLATION [redis.pending_resolved] not satisfied 10.16s after heal, deadline 10s (last check rc=1: pending=1) | `d97ccc5bfc664c68` | yes |

#### redis-proxy (`redis_hydo-proxy`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `ca9f41a4ba161da6` | yes |
| `disk=none \| run H=redisp@2-5:drop` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `d849b15699b028f4` | yes |
| `disk=none \| run H=redisp@1-3:reset K=redis@4+2` | OK invariants=1 held (redis.acked_writes_survive: acked=16 all present) | `348442b1011e7081` | yes |

#### redis-ram (`redis-ram_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `a0835b988223f147` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `b41c79d047e98358` | yes |

#### redis-composed (`redis-composed_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `96023abe8675ca7b` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | VIOLATION [acked_writes_survive] key k:21 lost | `c0daeb01f242ebb7` | yes |

#### redis-composed-hist (`redis-composed-hist_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=cut dseed=1 \| run C=12 W=0000002a,p4,q2,r100` | OK invariants=1 held (compose.commands: no composed workload ran in this boot) \| checks=2 held (redis.kv_li... | `1c36820149f28af3` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | VIOLATION [redis.kv_linearizable] key k:12: an acknowledged write is lost or a read is stale: a read that b... | `9acfad5318e6d3b8` | yes |

#### redis-composed-read (`redis-composed-read_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `6f32919d10ab40a5` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: no composed workload ran in this boot) | `f10eda1b5db0ea3a` | yes |

#### redis-errors (`redis-errors_hydo_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `21bbdabe5ad64801` | yes |
| `disk=ioerr:1 dseed=281cc34161f446f4 \| run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,oanytime_read` | OK invariants=5 held (compose.commands: started=6 ok=4 failed=2 timeout=0 violation=0 killed=0 skipped=0) | `7fc3174b9b541522` | yes |

#### redis-bridge (`redis-bridge_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `c69c6175a12c2b34` | yes |
| `disk=none \| run N=primary-replica@5-10:loss,100%` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `b88ae0563c9149f8` | yes |
| `disk=none \| run K=replica@5+3` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `07f944e0703ecd6a` | yes |
| `disk=cut dseed=1 \| run C=8` | OK invariants=1 held (primary.acked_writes_survive: acked=8 all on the primary) | `3e52d47fb9193f55` | yes |
| `disk=none \| run F=replica<primary@5#3:2000` | OK invariants=1 held (primary.acked_writes_survive: acked=16 all on the primary) | `2c2b25bc68bb109b` | yes |
| `disk=none \| run H=redisp@3-6:drop` | OK invariants=1 held (primary.acked_writes_survive: acked=13 all on the primary) | `a085504c6559a1d1` | yes |

#### rust-kv (`rust-kv-v2`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `2db6fdcc217982c3` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `9391d8a94083dd30` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `4ce661597e641723` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `faa3c2c7c56207c8` | yes |

#### avx2 (`avx2`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (avx.results_match_scalar: steps=8 all AVX2 results equal the scalar ones) | `7f10bb98d687cd55` | yes |

#### redis-oci (`redis-oci`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `06ab4f8912337f8d` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `1aa183891c2f6193` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `e441d37bf94ee8c7` | yes |

#### restate-oci (`restate-oci`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (restate.answers: answers; steps answered=6 of 6) | `2a48fbc111b3c0f0` | yes |
| `disk=none \| run K=restate@2+2` | OK invariants=1 held (restate.answers: answers; steps answered=4 of 6) | `9e67b3758a478c4c` | yes |
| `disk=cut \| run C=4` | OK invariants=1 held (restate.answers: answers; steps answered=4 of 6) | `ce7d75cfa286f1b2` | yes |

#### shop-buggy (`shop2-buggy`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `09ae7938ecf3bea1` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `9ef09b5fe0d3a244` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o7 took effect 2 times (records 25,26) | `62a2cd4d5c3ed5c3` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | VIOLATION [payments.charged_at_most_once] payments.charge: ref o0 took effect 3 times (records 1,2,3) | `c9ae5f3ea2da0df3` | yes |

#### shop-fixed (`shop2-fixed`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `d779faa3dbb1199c` | yes |
| `disk=none dseed=efb4cfbc49052179 \| run H=payments.charge@0-1:drop` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `f159969cab5825c6` | yes |
| `disk=none dseed=fbb60485ae17b0ba \| run H=payments.charge@6-9:reset,p=50` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 12 effects, every ref at most once) | `cc26d265f48957ca` | yes |
| `disk=cut dseed=1 \| run H=payments.charge@0-1:drop C=6` | OK invariants=5 held (payments.charged_at_most_once: payments.charge: 6 effects, every ref at most once) | `309a10a5cc83c3b5` | yes |

#### etcd-3.5.5 (`etcd-3.5.5`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `2a17ced62ad6a7db` | yes |
| `disk=cut dseed=1 \| run C=30` | VIOLATION [etcd.members_agree] members disagree: revisions 99 98 98; k89 mod_revisions 97 97 97 | `6ffb0aa93ceef86e` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | VIOLATION [client.revisions_monotonic] /tmp/hydo/rev.2: revision 82 (get-k80@127.0.0.12:12379) after 83 (ge... | `0967af85386e3b6f` | yes |

#### etcd-3.5.6 (`etcd-3.5.6`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `e0e0a2756b78e8fa` | yes |
| `disk=cut dseed=1 \| run C=30` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `151706a66875664b` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable) | `942bb76fd89ceda3` | yes |

#### etcd-hc-3.5.5 (`etcd-hc-3.5.5`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `784287e70cae4395` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 189 operations on 90 keys linearizable) | `d47eb5a57df4dc44` | yes |

#### etcd-hc-3.5.6 (`etcd-hc-3.5.6`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK checks=1 held (etcd.kv_linearizable: 183 operations on 90 keys linearizable) | `06f59cfe18040d1d` | yes |
| `disk=none dseed=1 \| run K=etcd1@10+1 P=etcd3@26+1` | OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable) | `7cf68773b8ba60da` | yes |

#### voting (`compose-voting_hydo`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `8b1cab74b5df78ee` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `35353f1cca523a17` | yes |
| `disk=none dseed=1 \| run C=12` | VIOLATION [votes.counted_soon] not satisfied 60.70s after heal, deadline 60s (last check rc=1: too few vote... | `5af8f850f95131e7` | yes |
| `disk=none dseed=1 \| run N=vote-redis@5-12:loss,100%` | OK invariants=1 held (votes.counted: all 13 acknowledged votes counted) | `b5d3770c329759f3` | yes |

#### voting-generated (`compose-voting_hydo.generated`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | ERROR invariant todo.acked_operations_survive could not be evaluated: TODO: write the invariants (hydo.yaml) | `3dc653254503ef08` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | ERROR invariant todo.acked_operations_survive could not be evaluated: TODO: write the invariants (hydo.yaml) | `6912170e3ec0c396` | yes |

#### voting-loopback (`compose-voting_hydo-loopback`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `14cb5f53eff7257c` | yes |
| `disk=none dseed=1 \| run K=worker@5+5` | OK invariants=1 held (votes.counted: all 20 acknowledged votes counted) | `483eb5d5d556df17` | yes |

#### stack-fix (`stack-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `97ce3e6d46f5ce0e` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ba0aa0612502a1df` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `85f7864ee2cb0498` | yes |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `3c5a25d843be6824` | yes |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `5425fcdfa4c733d4` | yes |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `db59edc566b6d68d` | yes |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ed665f5ba4bc103c` | yes |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ff4d2334d4f78c19` | yes |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `a2832854d50437f6` | yes |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `b68c443ab45f1fbb` | yes |
| `disk=none dseed=1 \| run N=kafka@5-10:delay,300ms` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `b6f42bbe48756cf3` | yes |
| `disk=none dseed=1 \| run O=mysql@5-10:resp:delay,1000ms` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `572c1c379f7cb926` | yes |
| `disk=none dseed=1 \| run N=mysql@3-8:delay,50ms,loss,20%` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `6889998299a21eca` | yes |

#### stack-old (`stack-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `8cb4443bf2524d35` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `1dc3677fb94053cf` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `41e3226fcc52d1df` | yes |
| `disk=cut dseed=45e6d4ac0682d6a9 \| run K=redis@6+4 N=mysql@20-26:delay,48ms C=13` | VIOLATION [stack.exactly_once] order=6 is in kafka (1 times) but the client was never ACKed | `f50afc416d3acbe2` | yes |
| `disk=cut dseed=676bcff7dd7d00ca \| run K=kafka@1+2 C=23` | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the client was never ACKed | `8bc7bcc2b42956eb` | yes |
| `disk=cut dseed=cf949e8cc47e94d2 \| run K=kafka@5+3 N=redis@19-30:delay,80ms C=28` | VIOLATION [stack.exactly_once] order=5 is in kafka (1 times) but the client was never ACKed | `4f1e6500e20b772b` | yes |
| `disk=cut dseed=787cfeea930dd7c3 \| run K=redis@8+1 N=kafka@25-30:delay,20ms,reorder,45% C=24` | VIOLATION [stack.exactly_once] order=8 is in kafka (1 times) but the client was never ACKed | `7dd5adec02004a39` | yes |
| `disk=cut dseed=3e46da0b0d53091a \| run K=kafka@12+1 N=mysql@0-13:delay,20ms,reorder,39%,corrupt,3% C=18` | VIOLATION [stack.exactly_once] order=12 is in kafka (1 times) but the client was never ACKed | `27ddc2209f0f6895` | yes |
| `disk=cut dseed=4631de84f54a4a77 \| run K=kafka@17+2 N=kafka@27-29:corrupt,4% C=21` | VIOLATION [stack.exactly_once] order=17 is in kafka (1 times) but the client was never ACKed | `1d644aaf66d60da8` | yes |
| `disk=cut dseed=31aad0ceab8015ae \| run K=redis@1+3 N=mysql@20-29:delay,20ms,reorder,43%,duplicate,46% C=11` | VIOLATION [stack.exactly_once] order=1 is in kafka (1 times) but the client was never ACKed | `bb80cd2d1a09be2b` | yes |

#### stack-read-fix (`stack-read-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `2b44d170d8b45013` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `bbe5b38ca4cbd2b8` | yes |

#### stack-read-old (`stack-read-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `73634cec66706c6e` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [order.told_failed_not_applied] disjoint violated: 1 item of told_failed is also in kafka: 11 | `0e848bdfb5567c10` | yes |

#### stack-errors-fix (`stack-errors-fix`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `1a56a26c0fcc566e` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `21e99ebd08442e3f` | yes |
| `disk=ioerr:5 dseed=1 \| run` | ERROR restart after the faults (mysql) | `3dee52a138056f08` | yes |

#### stack-errors-old (`stack-errors-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `d4ee59df9b431dbb` | yes |

#### miniflux (`miniflux`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `7301f204913837cb` | yes |
| `disk=none dseed=1 \| run K=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `c804c5804ce61e8d` | yes |
| `disk=none dseed=1 \| run K=front@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `97e2f8fc21e1070c` | yes |
| `disk=none dseed=1 \| run P=postgres@10+3` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `85fbb1faee5d700f` | yes |
| `disk=none dseed=1 \| run P=postgres@10~50:4000` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `9b40d31136f91705` | yes |
| `disk=none dseed=1 \| run O=postgres@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `81be562fa8a81dd1` | yes |
| `disk=none dseed=1 \| run O=front@10-15:resp:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `2ef7ada1e02f09e0` | yes |
| `disk=none dseed=1 \| run O=front@10-15:req:loss,100%` | OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once) | `05d52161ce0f0dcd` | yes |
| `disk=none dseed=1 \| run C=20` | OK invariants=2 held (categories.acked_exactly_once: acked=20 exactly once) | `f1921822ccac9894` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] category c17 stored 0 times | `b01b60dc7e763c6f` | yes |

#### miniflux-read (`miniflux-read`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=2 held (categories.acked_exactly_once: exactly-once holds: each of the 30 items of acked occu... | `96d6e0b2584542bd` | yes |
| `disk=none dseed=1 \| run C=20 S=off` | VIOLATION [categories.acked_exactly_once] exactly-once violated: acked c17 is in categories 0 times (of 20 ... | `d35d0fb81ca11ed4` | yes |

#### stack-grpc-cur (`stack-grpc-cur`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `1727a83353d9d2be` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ad2d115c82989fda` | yes |

#### stack-grpc-old (`stack-grpc-old`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `d0f18e1652e0e731` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `b77a87a60dc14f6c` | yes |

#### stack-grpc-planted (`stack-grpc-planted`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `47055f125f26994f` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | VIOLATION [stack.exactly_once] kafka order=10 present 2 times (must be exactly once) | `8786ec4211bfbecd` | yes |

#### stackgen (`stackgen-rs`)

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `42c34b875d91ad13` | yes |
| `disk=none \| run K=front@10+2` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `34b626826e68fe5f` | yes |
| `disk=cut \| run C=15` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `4103289e58dd5177` | yes |

#### Verdict lines that differ from section 9.2 (x86_64)

- redis-netem `disk=none | run O=redis@5-8:resp:loss,100%`
  - M140: HYDO-ORACLE: VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.16
  - M155: HYDO-ORACLE: VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.15
- redis-hist `disk=cut dseed=1 | run C=12 R=everysec`
  - M140: HYDO-ORACLE: VIOLATION [redis.kv_linearizable] key k:1: an acknowledged write is lost or a read is stale: a read that began after the write of v1 completed returned nil: p0 write k:1=v1 (ok, t=7.80-7.82s); p9 read k:1 -> nil (ok, t=7.76-7.77s (recovery boot)) (11 keys not linearizable)
  - M155: HYDO-ORACLE: VIOLATION [redis.kv_linearizable] key k:1: an acknowledged write is lost or a read is stale: a read that began after the write of v1 completed returned nil: p0 write k:1=v1 (ok, t=7.81-7.82s); p9 read k:1 -> nil (ok, t=7.76-7.77s (recovery boot)) (11 keys not linearizable)
- etcd-3.5.5 `disk=cut dseed=1 | run C=30`
  - M140: HYDO-ORACLE: VIOLATION [etcd.members_agree] members disagree: revisions 99 99 98; k89 mod_revisions 96 96 96
  - M155: HYDO-ORACLE: VIOLATION [etcd.members_agree] members disagree: revisions 99 98 98; k89 mod_revisions 97 97 97
- etcd-3.5.5 `disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1`
  - M140: HYDO-ORACLE: OK invariants=3 held (etcd.acked_puts_readable: acked=90 all readable)
  - M155: HYDO-ORACLE: VIOLATION [client.revisions_monotonic] /tmp/hydo/rev.2: revision 82 (get-k80@127.0.0.12:12379) after 83 (get-k77@127.0.0.11:2379)
- etcd-hc-3.5.5 `disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1`
  - M140: HYDO-ORACLE: OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable)
  - M155: HYDO-ORACLE: OK checks=1 held (etcd.kv_linearizable: 189 operations on 90 keys linearizable)
- etcd-hc-3.5.6 `disk=none dseed=1 | run K=etcd1@10+1 P=etcd3@26+1`
  - M140: HYDO-ORACLE: OK checks=1 held (etcd.kv_linearizable: 189 operations on 90 keys linearizable)
  - M155: HYDO-ORACLE: OK checks=1 held (etcd.kv_linearizable: 187 operations on 90 keys linearizable)
- voting `disk=none dseed=1 | run C=12`
  - M140: HYDO-ORACLE: VIOLATION [votes.counted_soon] not satisfied 60.69s after heal, deadline 60s (last check rc=1: too few votes counted: 0 of 12 acknowledged votes are in PostgreSQL; voter0 (vote a) is not)
  - M155: HYDO-ORACLE: VIOLATION [votes.counted_soon] not satisfied 60.70s after heal, deadline 60s (last check rc=1: too few votes counted: 0 of 12 acknowledged votes are in PostgreSQL; voter0 (vote a) is not)
- stack-errors-fix `disk=ioerr:5 dseed=1 | run`
  - M140: HYDO-ORACLE: ERROR restart after the faults (redis)
  - M155: HYDO-ORACLE: ERROR restart after the faults (mysql)
- miniflux `disk=none dseed=1 | run P=postgres@10~50:4000`
  - M140: HYDO-ORACLE: VIOLATION [api.reads_acked_categories] acked c14 missing from the API
  - M155: HYDO-ORACLE: OK invariants=2 held (categories.acked_exactly_once: acked=30 exactly once)
- miniflux `disk=none dseed=1 | run C=20 S=off`
  - M140: HYDO-ORACLE: VIOLATION [categories.acked_exactly_once] category c18 stored 0 times
  - M155: HYDO-ORACLE: VIOLATION [categories.acked_exactly_once] category c17 stored 0 times

### 13.3 The pinned reproducers

The stack example's two agent images (section 10) built again with the record, each twice with the
same bytes, `hydo run` from a fresh boot twice each, one fingerprint each time; in
`~/hydo/build/m155/` on the bare-metal machine with their build records (`hv/hvx/deploy-metal.sh`,
`hv/hvx/gc/side-suite.sh` and `hv/hvx/safety/side-check.sh` read them there).

| Image | Image sha256 | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | Fingerprint |
|---|---|---|---|
| `explore-ob-stack-old-disk.img` | `dc8f8a89e9a758a8…` | `VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed` | `1dc3677fb94053cf` (wfp `dc9321d7bad3e2a5`, ctl `e873020651036931`) |
| `explore-ob-stack-fix-disk.img` | `1de40b6a2681530d…` | `OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0)` | `ba0aa0612502a1df` (wfp `30130050db038e08`, ctl `07c598044a68e5be`) |

They replace section 10's `8fc461db23ef6642` and `aad6a563e2046361`, which M145's images (without
the record, in `~/hydo/build/m145/`) still give.

### 13.4 The releases' examples

Both tarballs of M155 (`hydo-linux-0.1.0.tar.gz` `d557c341…`, `hydo-mac-0.1.0.tar.gz`
`3ad2b456…`; docs/HISTORY.md "Milestone 155"), installed into clean prefixes, each schedule twice.
The Linux tarball rebuilt with M156's module fix (`98168977…`, docs/HISTORY.md "Milestone 156")
gave every Linux value of this table again, twice each, as the user and as root. Both tarballs
built from `e744b8c` (`e9538ed9…`, `098c8773…`; docs/HISTORY.md "Milestone 156", the addendum
"Both tarballs built from `e744b8c`") gave every value of this table, twice each, and so did both
tarballs built from the final commit `ce6071c` (`114e4160…`, `d6a27405…`; docs/HISTORY.md
"Milestone 157", the addendum "Both tarballs rebuilt from the final commit"):

| release | image (sha256) | schedule | verdict | fingerprint |
|---|---|---|---|---|
| Linux | `examples/redis` (`d255fb2bff5209bc…`) | `disk=none \| run` | OK (acked=20 all present) | `1893d8c55d7704d9` |
| Linux | `examples/redis` | `disk=cut \| run C=12 R=everysec` | VIOLATION key k:1 lost | `3520af95868b4e8f` |
| Linux | `examples/redis` | `disk=lying:3 dseed=eac0b527c5ac67d9 \| run C=20` (minimal) | VIOLATION | `747a6fbfbceecaa3` |
| Linux | `examples/redis` | `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` (minimal) | VIOLATION | `89105a398516a2ea` |
| Linux | `examples/redis/hydo-netem.yaml` (`bd4c979f8d6a448d…`) | `disk=none \| run N=redis@3-6:delay,300ms` | OK (retrans=0) | `0a706d74e43f2f4b` |
| Mac | `examples/redis` (`a34476147cda279a…`, data disk `9ddfc929…`) | `disk=none \| run` | OK (acked=20 all present) | `67e5d6469f1a90a5` |
| Mac | `examples/redis` | `disk=cut \| run C=12 R=everysec` | VIOLATION key k:0 lost | `a6870a92a1bbf0ae` |
| Mac | `examples/redis` | `disk=none \| run O=redis@5-10:resp:loss,100%` | OK (acked=15) | `ed3651af0bf0fb7e` |
| Mac | `examples/redis` | `disk=none tj=7 tjpm=500 tjmax=500 \| run` | OK (acked=20) | `aad758f1a6d55892` |
| Mac | `examples/redis` | `disk=lying:3 dseed=eac0b527c5ac67d9 \| run C=20` (minimal) | VIOLATION key k:6 lost | `0441476f5fd74d0b` |
| Mac | `examples/redis` | `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` (minimal) | VIOLATION | `d6120f56fdef3f28` |
| Mac | `examples/ledger-spin` (`c2840c669e605eee…`) | `disk=none \| run` | OK (acked=30) | `aa732142c0f5567f` |
| Mac | `examples/ledger-spin` | `disk=none \| run P=front@10+2` | VIOLATION entry 10 stored 2 times | `3b4dd4910d2a6123` |

## 14. node-kv and python-kv with the SDK comments reworded (M157, 2026-11-07)

Milestone 157 reworded one comment in `sdk/js/index.js` and one in
`sdk/python/src/hydo_assert/_core.py` (the comparison helpers' comment; the code is unchanged).
Both files are copied into images: `index.js` into node-kv's
(`/app/node_modules/hydo-assert/index.js`), `_core.py` into python-kv's and python-kv-hist's
(`/opt/pykv/hydo_assert/_core.py`). These three images therefore have new bytes and new
fingerprints; no other image contains either file. The engines, the controller, the agent and the
guest kernels did not change, so the fingerprint epoch stays 1 (an image built before M157 still
gives its section 13 values). These values supersede section 13.1's node-kv, python-kv and
python-kv-hist tables.

How: M155's Mac staging (section 13: its release prefix, which has the same image builder as
`main` = `3aa43c6`, and its staged Node.js, CPython and SDK inputs), copied into a new directory
twice: once as it was ("before") and once with the two files of this milestone ("after").
`hydo build` of the three manifests, once before and twice after; then every recorded schedule twice
with `hydo run --image` from a fresh boot, one VM at a time, on both.

Images: the before builds are byte for byte section 13.1's (initramfs node-kv `51d67eb22c8a1956…`,
python-kv `4bf7513bebe38613…`, python-kv-hist `4f4a31973168823e…`) and give section 13.1's ten
fingerprints and ctl values again, each twice. The after builds (two builds byte-identical each):
node-kv initramfs `ae2bbb9b231f295c…`, python-kv `3d7c2130b76cdd8e…`, python-kv-hist
`1c03cd40f532ac95…`; the root disks (`d031f382…`, `93c5d2ca…`, `03d395df…`) and the data disks are
the same before and after. Unpacked, each after initramfs has the same paths, modes and owners as
its before initramfs, and only the reworded file differs.

The verdict lines and the ctl values are section 13.1's word for word; only the fingerprints moved.

#### node-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `5f47c9a775cd40a8` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `3146b75677356b19` | `8de6e3d2ba8ff5d8` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `1cbb888242587805` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `772d0ca8f633d3be` | `39129eaa9b7e2227` | yes |

#### python-kv

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `f37f7b2513c61d0c` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `0dc1f942d30494c2` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `bd9cb77e5587ec31` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `5bf4fc7e7881d4d9` | `ece160c3964f7f03` | yes |

#### python-kv-hist

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `618b9d4430b2089e` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `78c99aebf1097a15` | `4a799f3170dcb41b` | yes |

Fingerprint by fingerprint (section 13.1, M155 → M157): node-kv `eb0811dba0a01354` →
`5f47c9a775cd40a8`, `846b02b08b161419` → `3146b75677356b19`, `f9847e1a9e687a15` →
`1cbb888242587805`, `9e6672f7f7f52ff7` → `772d0ca8f633d3be`; python-kv `a7f4b2de5ba82687` →
`f37f7b2513c61d0c`, `5b58d933c00349b6` → `0dc1f942d30494c2`, `fc139f93a791ec46` →
`bd9cb77e5587ec31`, `ed2d03a6970eb0bd` → `5bf4fc7e7881d4d9`; python-kv-hist `ee666af6229fc2e5` →
`618b9d4430b2089e`, `458abf5f089819cb` → `78c99aebf1097a15`.

No other recorded value moved: the other source files M157 changed are compiled into recorded
programs or modules, and each gives the same bytes before and after (docs/HISTORY.md
"Milestone 157").

## 15. node-kv with the publishable JavaScript package file (M160, 2026-11-07)

Milestone 160 prepared the SDKs for their registries (docs/PUBLISHING.md). npm refuses a package
marked `"private": true`, so `sdk/js/package.json` lost that field and gained `repository`,
`homepage`, `bugs` and `keywords`, and its description now says what hydo is. The node-kv example
copies that file into its images (`/app/node_modules/hydo-assert/package.json`), so the node-kv
images have new bytes and new fingerprints; no other image contains the file. Every other SDK
change of M160 is outside the images or byte-neutral (docs/HISTORY.md "Milestone 160"). The
engines, the controller, the agent and the guest kernels did not change, so the fingerprint epoch
stays 1 (an image built before M160 still gives its section 14 values). These values supersede
section 14's node-kv table.

How: M157's staged node-kv inputs and `sdk/js` (the release prefix of M155, whose image builder is
`main`'s, and the staged Node.js), checked against `main` (`dbb1df8`: the same `sdk/js` and
node-kv files apart from the README, which no image contains). `hydo build hydo-mac.yaml` once
with `main`'s `package.json` (initramfs `ae2bbb9b231f295c…`, section 14's, byte for byte) and twice
with this milestone's (two builds byte-identical); then every recorded schedule twice with
`hydo run --image` from a fresh boot, one VM at a time.

Images: initramfs `a15ec5d60932ec2b…` (`a15ec5d60932ec2be98d748e64a43522dbd31025c33ab1c5f5a14cc5060bf799`);
the root disk `d031f382…` and the data disk `9dba2707…` are the same as before. Unpacked, the
before and after initramfs have the same 492 paths, modes and owners, and only
`app/node_modules/hydo-assert/package.json` differs.

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `d5ecbd4819dd31ba` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `453377f5c45e05b8` | `bfdec020a46f3dcf` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `426f966e55917bef` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `cfeb973e2598ce99` | `39129eaa9b7e2227` | yes |

The verdict lines are section 14's word for word. Three ctl values are section 14's; the
`C=12 S=never` run's moved (`8de6e3d2ba8ff5d8` → `bfdec020a46f3dcf`): its boot transcript
(`control.boot.jsonl`, 52 lines) and its run's (`control.jsonl`, 48 lines) are M157's line for
line, and in the recovery transcript (`control.recovery.jsonl`, 36 lines) only the last four lines
differ: the oracle's two report lines and the end come two stops later (`n` 761 at the cadence
stop `at=19392` instead of 759 at `19264`). The likely reason, not traced further: the oracle
(`nkv.cjs check`) loads the SDK, Node.js reads and parses the SDK's `package.json` to resolve its
`exports`, and the file grew from 1,141 to 1,511 bytes, which moves the check's end across a
cadence stop. M152's change of the same file (one shorter line) moved no ctl value.

Fingerprint by fingerprint (section 14, M157 → M160): `5f47c9a775cd40a8` → `d5ecbd4819dd31ba`,
`3146b75677356b19` → `453377f5c45e05b8`, `1cbb888242587805` → `426f966e55917bef`,
`772d0ca8f633d3be` → `cfeb973e2598ce99`.

## 16. The assertion SDKs' API cleanup (M163, 2026-11-07)

Milestone 163 removed and hid parts of the four assertion SDKs before their first publication
(docs/HISTORY.md "Milestone 163"): the named comparison helpers, `emit_line`, the exported
assertion-type constants and a few internals. Every record the remaining calls write is
unchanged (the shared cases' output files did not change), but SDK sources are inside four
recorded images, and three examples' programs call `cmp` now instead of a removed helper:

- **node-kv** (Mac): `sdk/js/index.js` and `index.mjs` (`/app/node_modules/hydo-assert/`) and
  the example's own `nkv-server.mjs`;
- **python-kv** and **python-kv-hist** (Mac): `sdk/python/src/hydo_assert/*.py`
  (`/opt/pykv/hydo_assert/`) and `pykv.py`;
- **rust-kv** (Mac `explore-ob-rust-kv-arm64`, x86_64 `explore-ob-rust-kv-v2`): `/bin/rkv`, which
  compiles the Rust SDK and the example's `src/main.rs`.

No other recorded image contains an SDK file or a program built from one: the redis-composed-rs
guest binary (hydo-test with the Rust SDK; no recorded image since M86) was built before and
after at one path from clean target directories and has the same bytes (x86_64 `40959716…`,
aarch64 `fc5bbb31…`), and the order service uses the frozen Go copy in `hydo/stack`, which did
not change. The engines, the controller, the agent and the guest kernels did not change, so the
fingerprint epoch stays 1 (an image built before M163 still gives its section 13–15 values).
These values supersede section 15's node-kv table, section 14's python-kv and python-kv-hist
tables, and section 13's rust-kv tables (13.1 and 13.2).

**How, on the Mac.** M158's Mac release (`hydo-mac-0.1.0.tar.gz` `5c02e7f3…`, whose image builder
is `main`'s) installed into a scratch prefix with its `install.sh`; Node.js 22.2.0 and CPython
3.11.9 for aarch64 fetched from the public Nix cache with `hydo-nix-fetch --closure` and staged
with the file lists and steps of `fetch-node-arm64.sh` and `fetch-python-arm64.sh` (CPython's
list made with `hydo-elfdeps --root` on a copy of the closure in which the one absolute
`libgcc_s.so.1` link is relative, open item 42 d; every file as in the examples'
`files.sha256`); `rkv` built with `build-rkv.sh aarch64`. Before: the sources of `d0c2999` (the
packaging branch, Milestone 160). After: this milestone's. `hydo build` once before and twice
after. The before builds of node-kv, python-kv and python-kv-hist are byte for byte sections 15
and 14 (initramfs `a15ec5d6…`, `3d7c2130…`, `1c03cd40…`), so the staging is the recorded one.
The before build of rust-kv is not section 13.1's image (`11204bc3…`, not `23b33864…`): `rkv`'s
bytes depend on the absolute path of the checkout it is built in (Cargo's `-C metadata` of a path
dependency includes the path; open item 24), and the path of M155's build was not recorded; its
root and data disks are 13.1's. Every after pair is byte-identical. Unpacked, each after
initramfs has the same paths, modes and owners as its before initramfs (node-kv 492 entries,
python-kv 1,897, python-kv-hist 1,898, rust-kv 459), and only the files listed above differ; the
root and data disks are the same before and after. Then every recorded schedule twice with
`hydo run --image` from a fresh boot, one VM at a time.

**How, on the bare-metal machine.** M155's recipe for the case `rust-kv-v2` (its `img.sh`: tier C,
built as uid 1000 gid 100 with `HYDO_KEEP_OWNERS=1`, M155's image builder, the pinned agent
`398f297e…`, hydo-world `082592ae…`), in this milestone's own tree and work directory. Before:
M155's staged example, `rkv` included, gives M155's disk `20fb50e4…` byte for byte, and its four
schedules with `hydo run` from a fresh boot give section 13.2's four fingerprints, each twice.
After: `rkv` built from this milestone's sources with M155's fake-home recipe at this
milestone's own path, twice with the same bytes (`615fec22…`); the image twice with the same bytes
(`ef1b4c5d079a19a6…`). The before and after root file systems have the same 2,519 entries,
modes and owners, and only `/bin/rkv` differs. Every recorded schedule twice with `hydo run
--image` from a fresh boot through `~/bin/hvx-run` and the loaded main module, one VM at a time.

Every verdict line is the earlier one word for word, and every schedule gave one fingerprint both
times.

#### node-kv (Mac), initramfs `39d38e81…`

`39d38e81ed7c16c393b852429f1a663fd97a7a37ce8c9f28ab4e725590bd8537`; root disk `d031f382…`, data disk
`9dba2707…` (section 15's).

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `9a4166f95ce564f1` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `eae9b3e9d9df7262` | `bfdec020a46f3dcf` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `db0ced2ee18e2182` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `258c878ca6f94d50` | `0c5914e7cd7b1a58` | yes |

Section 15 → M163: `d5ecbd4819dd31ba` → `9a4166f95ce564f1`, `453377f5c45e05b8` →
`eae9b3e9d9df7262`, `426f966e55917bef` → `db0ced2ee18e2182`, `cfeb973e2598ce99` →
`258c878ca6f94d50`. Three ctl values are section 15's; the `K=nkv@8+2 B=1234` run's moved
(`39129eaa9b7e2227` → `0c5914e7cd7b1a58`) with the same verdict (not traced further: the server's
and the SDK's code is shorter, which moves guest time).

#### python-kv (Mac), initramfs `0796b216…`

`0796b2166f663d8be289517d28cf817ee45dba0ab677335a48d4cf2ae3842420`; root disk `93c5d2ca…`, data
disk `cefe8bbd…` (section 14's).

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (pykv.oracle: acked=20 all present) | `77662a4dc66660b4` | `99d3a6c9bcab4d75` | yes |
| `disk=none \| run K=pykv@8+2` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `d218c9200955852c` | `9dac63ed9843b10c` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [pykv.oracle] key k:0 lost (got 'NONE') | `67c1be61543262ec` | `3b8233343cb49963` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (pykv.oracle: acked=18 all present) | `9de124f239acb2d2` | `ece160c3964f7f03` | yes |

#### python-kv-hist (Mac), initramfs `08d77aa7…`

`08d77aa7933e01f2b22f7a5aa34ded346809aec2fd53e707477194e18b55854d`; root disk `03d395df…`, data
disk `1cb55573…` (section 14's).

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=cut \| run C=12` | OK invariants=0 held (); history OK | `c784a2d6d224eb47` | `133c9c994223dbe4` | yes |
| `disk=cut \| run C=12 S=never` | OK invariants=0 held (); history VIOLATION [kv.linearizable] | `4e0ea385f449db81` | `4a799f3170dcb41b` | yes |

Section 14 → M163: python-kv `f37f7b2513c61d0c` → `77662a4dc66660b4`, `0dc1f942d30494c2` →
`d218c9200955852c`, `bd9cb77e5587ec31` → `67c1be61543262ec`, `5bf4fc7e7881d4d9` →
`9de124f239acb2d2`; python-kv-hist `618b9d4430b2089e` → `c784a2d6d224eb47`, `78c99aebf1097a15` →
`4e0ea385f449db81`. Every ctl value is section 14's.

#### rust-kv (Mac), initramfs `c450aacb…`

`c450aacb054240740d4a14dcbe314fa3b0c04e7a68db5c99c7391ddca4b4bace` (`rkv` aarch64 `735b28de…`); root
disk `e19be506…`, data disk `99dbc1be…` (section 13.1's).

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `5e1d844bdf412e73` | `bc6fe53352fa8359` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `ac2bfa6ddc729e9f` | `ded68d8cdfcf34f0` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `3a4be66e14c4c051` | `22168eb0b60f5ac1` | yes |
| `disk=none \| run K=rkv@8+2` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `bf97e6e506a17a14` | `0ad74bffbfd87b1f` | yes |
| `disk=none \| run B=5abc,1000,500` | OK invariants=1 held (rkv.oracle: rkv check: acked=2 all present) | `839345b237fb6bef` | `d20fa8483538cd42` | yes |
| `disk=none \| run B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `1f691ae7e24d1f34` | `243f271a5911baf1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `aa0b8c437f8b33f4` | `31ca45bf4f0d1dfe` | yes |
| `disk=none \| run P=rkv@5+3` | OK invariants=1 held (rkv.oracle: rkv check: acked=17 all present) | `a55085a1fe310ad9` | `53e69be30dff5ad1` | yes |

Section 13.1 → M163: `547f7b419affd943` → `5e1d844bdf412e73`, `6d32a2e7ac9aecce` →
`ac2bfa6ddc729e9f`, `c221cba5d3298571` → `3a4be66e14c4c051`, `3d188ff780a5d993` →
`bf97e6e506a17a14`, `439f45f7fe19dff7` → `839345b237fb6bef`, `dbc272cd3cc600ec` →
`1f691ae7e24d1f34`, `0178e70f40b959a5` → `aa0b8c437f8b33f4`, `6e6269b2af51beab` →
`a55085a1fe310ad9`. Every ctl value is section 13.1's.

#### rust-kv (x86_64, `explore-ob-rust-kv-v2`), disk `ef1b4c5d…`

`ef1b4c5d079a19a6321d53fc9d29a56dced928c4cc4a9fba1fc7cf97a8994882` (`rkv` x86_64
`615fec228c11f5b4…`).

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (rkv.oracle: rkv check: acked=20 all present) | `d184b981aeb40784` | `f8463ecaf1560f12` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (rkv.oracle: rkv check: acked=12 all present) | `a9cdd32d8953d7f2` | `9003b2c60348a405` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [rkv.oracle] rkv check: acked=12 lost=12 | `9e590e418c25edae` | `50d7001af4588bc1` | yes |
| `disk=none \| run K=rkv@8+2 B=1234` | OK invariants=1 held (rkv.oracle: rkv check: acked=18 all present) | `eb37594cfd033c85` | `20741d9f13ac3585` | yes |

Section 13.2 → M163: `2db6fdcc217982c3` → `d184b981aeb40784`, `9391d8a94083dd30` →
`a9cdd32d8953d7f2`, `4ce661597e641723` → `9e590e418c25edae`, `faa3c2c7c56207c8` →
`eb37594cfd033c85`. Section 13.2 recorded no ctl values; the before image gave `ca38cb4162fbb2b6`,
`9003b2c60348a405`, `50d7001af4588bc1` and `20741d9f13ac3585` with `hydo run`, so three are
unchanged; in the `disk=none | run` transcript (47 lines both times) only the last four lines
differ: the oracle's report and the end come 13 stops earlier (`n` 23,353 at `at=852874`
instead of 23,366 at `853080`), because the new `rkv` runs different code.
