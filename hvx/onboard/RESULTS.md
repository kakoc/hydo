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

## Services from a Docker/OCI image (2026-10-21)

`examples/redis-oci/hydo.yaml`: the Redis example with Redis taken from the official
`redis:7.2.4` image (unmodified; fetched with skopeo 1.15.1 by `fetch-image.sh`, pinned by the
linux/amd64 manifest digest `sha256:9341b654…` and the linux/arm64 one `sha256:4da694ba…`; image
id / config `sha256:9b38108e…` amd64, `sha256:0b4893f5…` arm64). The image's own entrypoint
(`docker-entrypoint.sh` → `gosu redis`) starts the server; only `CMD` is replaced.

Builds (build box, each built twice in separate build directories, byte-identical):

| image | sha256 |
|---|---|
| `explore-ob-redis-oci-disk.img` (x86_64, from a docker-archive tar) | `ca3ea2b0…` (ext4 root `b9e56872…`) |
| `explore-ob-redis-oci-arm64.cpio.gz` (`--arch arm64`, from an OCI layout) | `f575448a…` (disk `d4116ff4…`) |

The same amd64 image as an OCI layout directory gives an identical `/oci/redis` tree (content,
owners, modes; only `image.json` differs). Feeding the arm64 image to the x86_64 build, or the
amd64 image to `--arch arm64`, is refused. Existing manifests build byte-identically with the new
hydo-build: `examples/redis` `0dd509c2…` (same as main), arm64 `hvx-mac/explore/examples/redis`
cpio `10f7858a…` / disk `9ddfc929…` (the values in `hvx-mac/explore/README.md`), and `examples/stack` (FRONT=fix) disk `37a2ff6a…` with both main's and the new hydo-build.

Runs on the bare-metal test machine (`hvx_explore one`, results `/var/tmp/m59-oci`), each twice:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=1 \| run` | OK, acked=20 all present | `d6a18f52f6418cf5` (both) |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK, acked=15 | `4ffee7177898e58e` (both) |
| `disk=none dseed=1 \| run C=12 R=everysec` | VIOLATION `key k:1 lost` (Redis's documented fsync window) | `9c36c7d46bcb9ddf` (both) |
