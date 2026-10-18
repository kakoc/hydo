# Onboarding v1 — proof results (2026-10-18, bare-metal test machine)

Images built by `hydo-build` on the build box (byte-identical across rebuilds), run with
`hvx_explore one` / `explore.py` from the merged main. Results under `/var/tmp/m47-onboard`.

| image | sha256 (disk) |
|---|---|
| `explore-ob-redis-disk.img` (examples/redis) | `ad5d68d5…` |
| `explore-ob-stack-fix-disk.img` (examples/stack, fixed front door) | `8ae8288b…` |
| `explore-ob-stack-old-disk.img` (examples/stack, pre-fix front door) | `c1906d2d…` |

Every schedule below was run twice; both runs gave the same fingerprint.

**Stack (four services) — the four proof checks**

| check | schedule | verdict | fingerprint |
|---|---|---|---|
| (a) sanity, fixed | `disk=none dseed=1 \| run` | OK, 3 invariants held | `fd5d47a611683780` |
| (b) faults, fixed | `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK | `f50893ec3f010153` |
| (c) old bug, old image | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION `[stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed` | `2eca7c7d0619e84d` |
| (d) same, fixed image | same | OK | `798ebf5e43758154` |

The 7 schedules with which Milestone 42's swarm runs found the old bug on the hand-built
image (seeds 1017 1039 1049 1055 2019 3038 3046): 7/7 VIOLATION on the manifest old image,
each naming the same order as in M42; 7/7 OK on the manifest fixed image.

`explore.py --strategy swarm --seeds 1-40 --workers 1`: old image 23 failures, fixed image
25 failures, every one re-run with the same fingerprint (48/48); all in the expected classes
(lying disk, I/O errors, relaxed knob). Seeds 1-40 happen to contain no durable-settings
schedule of the bug's shape (M42's runs used seeds 1001-3060), so this sample shows
determinism and the generator working on manifest images, not the bug.

**Redis (one service)**

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=1 \| run` | OK, acked=20 all present | `712bcab7167a52fe` |
| `disk=none dseed=1 \| run C=12 R=everysec` | VIOLATION `[redis.acked_writes_survive] key k:3 lost` (Redis's documented fsync window) | `e4f65d61d3320ada` |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK, acked=15 (kill window + two 2 s client timeouts) | `eb3cc8787ec1e261` |
