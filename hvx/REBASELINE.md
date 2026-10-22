# Re-baseline after the hvx_seedrng stand-in (2026-10-21/22)

From 2026-10-17 until 2026-10-21 the build machine had no pinned static musl gcc
(`aa1jsl19…-gcc-static-x86_64-unknown-linux-musl-13.2.0`). Image builds then used a stand-in
`hvx_seedrng` binary, sha256 `01adf1bd67cbc79f…` (36,280 bytes), copied from
`~/hydo/build/hvx_seedrng`. Nobody can rebuild that binary (`hvx/BUILD-PINS.md`). The pinned
gcc is restored, and with it `hvx/hvx_seedrng.c` gives `c2a440c5e1c3888f…` (40,272 bytes).
The decision was to re-baseline: rebuild every image that contains the stand-in with the
pinned tools only, and record the new image hashes and fingerprints. This file is that
record. CLAUDE.md is not changed here.

Contents: 1. Inventory. 2. How the images were rebuilt and verified. 3. Old → new image
hashes. 4. Old → new fingerprints, one table per image. 5. Verdict changes. 6. Paths.

## 1. Inventory

Method: for every `*.img` and `*.cpio.gz` in `~/hydo/build` on the bare-metal machine
and on the build machine, the file `hvx_seedrng` was extracted from every
partition (`debugfs` dump of `/bin/hvx_seedrng`, `/usr/bin`, `/sbin`, `/oci/*/bin`) or from the
initramfs (cpio walk), and hashed. No image was judged by its date.

**Contain the stand-in (`01adf1bd…`), bare-metal machine `~/hydo/build`:**

| image | rebuilt here? | where it is used in the docs |
|---|---|---|
| `explore-nf-disk.img` | yes | CLAUDE.md M46, M54, M56, M58; `hvx/bench/README.md` (control image, `e5d8d6fb…`, base bug `f1c39a88…`); `hvx/cutflush/FINDING.md` (cf=204 `e592e6c6…`, 12 cut points) |
| `explore-bench-d1…d7-*-disk.img` (7) | yes | CLAUDE.md M54; `hvx/bench/README.md` defect table; `hvx/bench/run.py`, `confirm.py` |
| `explore-bench-bg-control`, `-bg-d0…d7-*` (9) | yes | CLAUDE.md M56; `hvx/bench/README.md` BUGGIFY section; `confirm.py --bg-images` |
| `explore-bench-bg-fixed-disk.img` (`7974498b…`) | yes | `hvx/bench/README.md` (BUGGIFY fixed front door); `hvx/cutflush/FINDING.md` (cf=242 `5c04f271…`, 13 cut points); CLAUDE.md M58 |
| `explore-ob-redis`, `-stack-fix`, `-stack-old` | yes | CLAUDE.md M47; `hvx/onboard/RESULTS.md` (M47 tables) |
| `explore-ob-redis-pt`, `-stack-fix-pt`, `-stack-old-pt` | yes | CLAUDE.md M52; `hvx/BUILD-PINS.md` (`0dd509c2…`, `37a2ff6a…`) |
| `explore-ob-miniflux`, `-miniflux-pt` | yes | CLAUDE.md M52; `hvx/onboard/examples/miniflux/RESULTS.md` |
| `explore-ob-miniflux-tree` | yes (byte-identical to `-pt`, old and new) | not in the docs |
| `explore-ob-kb-sqlite-3510200`, `-3510300` | yes | CLAUDE.md M49, M52; `hvx/knownbugs/sqlite/README.md` (`05d75b28…`, `2ff12b40…`); `hvx/race/PREEMPT.md` |
| `explore-ob-redis-live` | yes | CLAUDE.md M59; `hvx/onboard/RESULTS.md` (liveness, `558a256b…`) |
| `explore-ob-redis-oci` | yes | CLAUDE.md M59; `hvx/onboard/RESULTS.md` (OCI, `ca3ea2b0…`) |
| `richcov-fix-disk.img`, `richcov-old-disk.img` | yes | CLAUDE.md M44, M45 (`cfc71a50…`, `059021d7…`); `hvx/richcov/DESIGN.md`; `hvx/explore/richguided.py`, `abranch.py` |
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
  (cc0) and `b9233d5` (cc1) with `hvx/stack` at those commits.
- `explore-dx-disk.img`: a diagnostic image of M50 (built 2026-10-18 09:22 UTC, used once in
  `/var/tmp/dx1`). No document refers to it and its source was never committed.
- `explore-ob-etcd-*`, the binlog images and the `explore-bench-m59-*` images belong to work
  in progress in other sessions, which rebuild them with the pinned tools themselves.
- arm64 (Mac) images: not in either `~/hydo/build`; `hydo_build_arm64.py` adds
  `hvx_seedrng` only when `HYDO_SEEDRNG_ARM64` is set. Not inventoried.

## 2. How the images were rebuilt and verified

Every image was rebuilt from the source it was originally built from, in a private directory
on the build machine (`~/hydo-rebase`, deleted afterwards), with `HYDO_GCC_MUSL`,
`HYDO_MUSL_ROOT`, `HYDO_REDIS_OUT` and `HYDO_SEEDRNG` unset. `hvx_seedrng` was compiled by
the pinned gcc (`c2a440c5…` in every build). Redis came from the pinned store path
`ky1xq6xa…-redis-static-…-7.2.4` (byte-identical to `~/hydo/build/redis-*`).

| images | source | build command |
|---|---|---|
| `explore-nf` | commit `15cc5a4` (identical to the tree `~/hydo-faults` it was built in) | `STACK_OUT=explore-nf bash ./build-stack.sh` |
| `explore-bench-d1…d7` | commit `91312b2` (identical to `~/hydo-bench`, the M54 build tree, except for 3 abandoned patches) | `bash hvx/bench/build-bench.sh` |
| `explore-bench-bg-*` (9) | commit `3773607` | `bash hvx/bench/build-bench-bg.sh <name>` |
| `explore-bench-bg-fixed` | commit `3773607` (its `hvx/stack/main.go` already calls the BUGGIFY wrappers) | `STACK_OUT=explore-bench-bg-fixed bash ./build-stack.sh` |
| `explore-ob-redis`, `-stack-fix`, `-stack-old` | commit `0360a16` | `hydo-build examples/redis/hydo.yaml --out explore-ob-redis`; `FRONT=fix|old bash hvx/onboard/examples/stack/build.sh` |
| `explore-ob-*-pt` (redis, stack-fix, stack-old) | commit `78f2ed4` | the same with `--out explore-ob-<name>-pt` |
| `explore-ob-miniflux` | commit `3063f23` | `hydo-build examples/miniflux/hydo.yaml` (miniflux 2.3.3 rebuilt from `~/mf-src/v2`: `dd8e2456…`, as recorded) |
| `explore-ob-miniflux-tree`, `-pt` | commits `f9e9e6c` and `7d46a1b` | the same, `--out …-tree` / `…-pt` |
| `explore-ob-kb-sqlite-3510200/3510300` | commit `fe97d2c`, walrace binaries from `~/hydo-sqlite/hvx/knownbugs/sqlite/bin` on the box | `hydo-build hvx/knownbugs/sqlite/hydo.yaml [--set v=3510300]` |
| `explore-ob-redis-live` | commit `af1c873` | `hydo-build examples/redis/hydo-live.yaml --out explore-ob-redis-live` |
| `explore-ob-redis-oci` | commit `130941f` | `fetch-image.sh` (with `SKOPEO=` set, because this commit has the skopeo-path bug fixed later in `b3e2f5a`), then `hydo-build examples/redis-oci/hydo.yaml` |
| `richcov-old`, `richcov-fix` | commit `1986481` | `RICHCOV_VARIANT=old|fix bash hvx/richcov/build-richcov.sh` (also rebuilds the debug Redis with the pinned gcc; it came out byte-identical) |

The manifests' `~/hydo/build/…` inputs (Redis, prebuilt front doors) were read from a copy
under a private `HOME`, as `hvx/pins-proof.sh` does. No build script was changed.

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
  bug `f1c39a88…` is in `hvx/bench/README.md`); flush-cut points: `hvx/cutflush/FINDING.md`
  (cf=204) and `/var/tmp/m57-cfcheck/nf-rerun/results.txt`.
- `explore-bench-d*`: `hvx/bench/README.md` defect table. `explore-bench-bg-d*`:
  `hvx/bench/README.md` BUGGIFY section; `-bg-control`: `/var/tmp/m56-bg/confirm/confirm.jsonl`.
- `explore-bench-bg-fixed`: FINDING.md (cf=242) and `/var/tmp/m57-cfcheck/fixed-rerun/results.txt`.
- `explore-ob-redis`, `-stack-fix`, `-stack-old`: `hvx/onboard/RESULTS.md` (M47); M42 schedules
  `/var/tmp/m47-onboard/m42.txt`. `-pt` images: CLAUDE.md M52 and `/var/tmp/m48-miniflux/batch3.txt`.
- miniflux: `hvx/onboard/examples/miniflux/RESULTS.md`. Liveness and OCI: `hvx/onboard/RESULTS.md`.
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

1. **`explore-nf` flush-cut points (`hvx/cutflush/FINDING.md`, CLAUDE.md M58).** The 12
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
3. **`explore-ob-redis` (M47, `hvx/onboard/RESULTS.md`).** `disk=none dseed=1 | run C=12
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
