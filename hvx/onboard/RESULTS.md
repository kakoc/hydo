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
| `disk=none dseed=1 \| run C=12 R=everysec` | VIOLATION `[redis.acked_writes_survive] key k:3 lost` (Redis's documented fsync window) | `e4f65d61d3320ada` (on the re-baselined `explore-ob-redis-pin` image it loses `k:2`; see hvx/REBASELINE.md) |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK, acked=15 (kill window + two 2 s client timeouts) | `eb3cc8787ec1e261` |

## Liveness invariants (`type: eventually`) on Redis (2026-10-21, bare-metal test machine)

Manifest `examples/redis/hydo-live.yaml` (the Redis example + a `retry:` that re-sends every
SET not yet acknowledged + `redis.pending_resolved`: every SET whose outcome was unknown is
acknowledged within 10 s of guest time after fault injection ends). Image
`~/hydo/build/explore-ob-redis-live-disk.img`, sha256 `558a256b…` (two builds identical; the
plain `explore-ob-redis` image built with the new `hydo-build` is byte-identical to main's,
`0dd509c2…`). Each schedule run twice with `hvx_explore one`; results under
`/var/tmp/m59-live`.

| schedule | liveness episode | verdict | fingerprint (both runs) |
|---|---|---|---|
| `disk=none dseed=1 \| run` | ok, 0.02 s after heal | OK | `a98ba5035745de58` |
| `disk=none dseed=1 \| run K=redis@5+3` | ok, 1.04 s (the retry acks 5..7) | OK | `98923d5657a64cbf` |
| `disk=none dseed=1 \| run P=redis@5+3` | ok, 1.03 s | OK | `68728f9760f2d079` |
| `disk=none dseed=1 \| run C=12` | ok, 0.03 s after the recovery boot starts | OK, acked=12 | `7465c2902bd68b65` |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | missed: Redis stays paused ~18 s after the last step | VIOLATION `[redis.pending_resolved] not satisfied 10.08s after heal, deadline 10s (last check rc=1: pending=1)` | `a8df6a3e9d90c802` |

`hydo_assert_report` over the ten consoles: `redis.pending_resolved` 8 ok / 2 fail,
`FAIL (deadline missed)`, slowest in-time episode 1.04 s,
`liveness: 1 eventually properties, 1 missed a deadline`.

`explore.py --image explore-ob-redis-live-disk.img --seeds 1-12 --strategy swarm --workers 1`
(`/var/tmp/m59-live/explore-swarm`): 6 failures, all reproduced, all in the expected classes
(lying disk, power cut with `R=no`/`everysec`, I/O errors); no liveness miss. The report's new
Liveness table: `redis.pending_resolved` held in time in 9 schedules (slowest 1.03 s), missed 0;
the other 3 ended before the final checks (unmountable disk, unrecoverable AOF, failed restart).

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

### The arm64 image booted on the Mac (2026-10-21, M2 Pro, `hvmx`)

The arm64 image was fetched again on the build box (`fetch-image.sh`; the OCI layout is identical to
the earlier fetch) and built with `hydo-build --arch arm64`: `explore-ob-redis-oci-arm64.cpio.gz`
`f575448a…`, disk `d4116ff4…`, the same bytes as the M59 build. Kernel `Image-explore` `f80c5655…`
(the release kernel, HVC idle, with `tc`), `hvmx` built from `main` (`hvx-mac/explore/build.sh`).

Each schedule ran in two separate `hvmx serve` processes (restored from the S1 snapshot) and once
as `hvmx one` (a fresh boot), with `hvx-mac/explore/netfault/check.py`. All three fingerprints are
equal for every schedule:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run` | OK, acked=20 all present | `df1b39fdaa4ecf79` |
| `disk=none \| run K=redis@5+2 P=redis@12+2` (kill, then pause) | OK, acked=16 | `e6b9a7eef5dc96e6` |
| `disk=none \| run K=redis@5+2` | OK, acked=18 | `6b9321d4b13e5d12` |
| `disk=none \| run P=redis@8+2` | OK, acked=18 | `cc7f5ce830f256fb` |
| `disk=cut \| run C=12` (power cut, `appendfsync always`) | OK, acked=12 | `9bfe88561695340d` |
| `disk=cut \| run C=12 R=everysec` (power cut) | VIOLATION `key k:1 lost` (Redis's documented fsync window) | `73b907b554f95f1a` |

The box's three schedules on the Mac (same check, all identical); the verdicts equal the x86_64
ones above:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=1 \| run` | OK, acked=20 | `99c3ca773c56f8d8` |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK, acked=15 | `2f4a5da3dedce46a` |
| `disk=none dseed=1 \| run C=12 R=everysec` | VIOLATION `key k:1 lost` | `93dc6075a535c340` |
| `disk=none dseed=1 \| run C=12` | OK, acked=12 | `fae6d944b70426fc` |

**Owners and modes.** On arm64 they come from the newc headers of the initramfs. Two checks:
- *In the archive.* An independent flatten of the eight OCI layers (Python `tarfile`, whiteouts and
  hard links applied) against the cpio headers under `oci/redis/`: 4,254 layer entries, every
  type, permission (incl. setuid/setgid/sticky), uid, gid and symlink target equal. The only
  extra cpio entries are hydo's `/.hydo`, `/.hydo/busybox` and `/etc/hosts`.
- *At run time.* `examples/redis-oci/hydo-metacheck.yaml` (the same manifest plus one final
  invariant, `oci.meta_at_runtime`) reads them inside the running guest. Image
  `explore-ob-redis-oci-meta-arm64.cpio.gz` `ae757bea…` (disk `354bdc02…`), byte-identical in two
  builds. In the guest: `/usr/bin/{su,passwd,mount}` 4755 root:root, `/usr/bin/chage` and
  `/usr/sbin/unix_chkpwd` 2755 root:shadow(42), `/etc/shadow` 640 root:shadow, `/tmp` 1777,
  `/var/mail` 2775 root:mail, `/var/cache/apt/archives/partial` 700 uid 42; `redis-server` runs
  with uid/gid 999 (real, effective, saved, fs), also after a kill and restart and in the recovery
  boot after a power cut; every file under `/data` (the data disk; the entrypoint chowns it) is
  owned by 999, the AOF files are 999:999 0600; as the redis user `cat /etc/shadow` is denied and
  the setgid `chage -l redis` reads it. Fingerprints (serve, serve, one; all equal):
  `disk=none | run` `83c5d4085fa80182`; `K=redis@5+2 P=redis@12+2` `88fedf841db84f20`;
  `disk=cut | run C=12 R=everysec` `80188561d5d1d447` (VIOLATION `k:1`, as without the check).

**Fixed:** `fetch-image.sh` took `nix build --print-out-paths nixpkgs#skopeo` as one path, but it
prints two (`skopeo-1.15.1-man` first), so without `skopeo` on `PATH` the script failed. It now
uses the output that has `bin/skopeo`; re-fetched with it, the OCI layout is identical.
