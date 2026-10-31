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

**Current stack example images (2026-10-23, M77):** `explore-ob-stack-fix-nq-disk.img`
(`ecdcb4fa…`) and `explore-ob-stack-old-nq-disk.img` (`58de1559…`), built with the pinned tools and
the M71 netem drain; only `/sbin/init` differs from the `-pt-pin` images. Every recorded schedule
gives the same verdict: (a) sanity OK `8ad89b9f2565f88b`, (b) OK `76b4917b3761288e`, (c) on old
VIOLATION order 11 `402736c78bbf9a6d`, (d) OK `96229e59c4d498ec`; the 7 M42 schedules fail on old
naming the same orders and pass on fix. Tables: `docs/REBASELINE.md` section 7.1.

**Redis (one service)**

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=1 \| run` | OK, acked=20 all present | `712bcab7167a52fe` |
| `disk=none dseed=1 \| run C=12 R=everysec` | VIOLATION `[redis.acked_writes_survive] key k:3 lost` (Redis's documented fsync window) | `e4f65d61d3320ada` (on the re-baselined `explore-ob-redis-pin` image it loses `k:2`; see docs/REBASELINE.md) |
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
hydo-build: `examples/redis` `0dd509c2…` (same as main), arm64 `hydo/onboard/examples-mac/redis`
cpio `10f7858a…` / disk `9ddfc929…` (the values in `hv/hvm/explore/README.md`), and `examples/stack` (FRONT=fix) disk `37a2ff6a…` with both main's and the new hydo-build.

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
(the release kernel, HVC idle, with `tc`), `hvmx` built from `main` (`hv/hvm/explore/build.sh`).

Each schedule ran in two separate `hvmx serve` processes (restored from the S1 snapshot) and once
as `hvmx one` (a fresh boot), with `hv/hvm/explore/netfault/check.py`. All three fingerprints are
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

## Lifting a network fault no longer drops netem's queued packets (2026-10-23, bare-metal test machine)

**Cause.** Every network fault (`N=`, `O=`, link faults) is a netem qdisc on `lo`, and lifting it
ran `tc qdisc del dev lo root`. In the guest kernel (6.6.35) deleting the qdisc runs
`netem_reset` -> `tfifo_reset`, which frees every packet still waiting in netem's delay queue.
TCP then retransmitted those packets after a timeout the schedule never asked for.

**Fix** (`net_heal`, in `hydo/onboard/hydo-init.sh` and, as a copy, in the stack's `/sbin/init`
in `build-stack.sh`; used by `hydo-init.sh`'s `net_off`, the link faults of
`hydo-init-links.sh`, `hydo/explore/stack-explore.sh` and the stack's `hydo_net=` mode):
`tc qdisc change` the netem to rate 100gbit and no delay/loss/duplication/corruption
(`netem_change` keeps the queue and every queued packet's send time; in rate mode a new packet
is scheduled after the last queued one, so the order is kept), wait in guest time until the
queue is empty (at most delay + 4 x jitter + 500 ms), remove the filters, wait until the queue
is empty again (at most 50 ms), then delete the root qdisc. The console line
`network fault lifted: queued=<n> drained_ms=<ms> left=<n>` records it. Schedules without a
network fault never call it.

**Demonstration** (`examples/redis/hydo-netem.yaml`: the Redis example plus the invariant
`net.no_retransmission`, Tcp RetransSegs from `/proc/net/snmp` must be 0). Old init = main's
`hydo-init.sh` (`explore-ob-redis-netem-main`, `74c45491…`); new init (`explore-ob-redis-netem-nq`,
`c7bc532a…`, two builds identical). New-image runs were each run twice with identical
fingerprints. Results `/var/tmp/nq/b1` on the box.

| schedule | old init | new init |
|---|---|---|
| `disk=none dseed=1 \| run` | OK retrans=0 | OK retrans=0 (`33acb90ead2afd38`) |
| `... run N=redis@3-6:delay,300ms` | VIOLATION retrans=2 (`c568d85e8e4f70ba`, twice) | OK retrans=0, queued=2 at the heal (`5b226954f8df4479`) |
| `... run O=redis@3-6:resp:delay,300ms` | VIOLATION retrans=2 | OK retrans=0 (`dd440bce5c239ea2`) |
| `... run N=redis@5-20:delay,300ms` (heal at the end) | VIOLATION retrans=2 | OK retrans=0 (`3ed5964ccdd8e5f4`) |
| `... run N=redis@3-6:delay,1s` | OK retrans=0 | OK retrans=0, queued=2 drained in 720 ms (`157b4a39899d5a90`) |
| `... run O=redis@3-6:req:delay,2000ms` | VIOLATION retrans=5 | VIOLATION retrans=3 (`acb2046ab94d0227`): a 2 s delay exceeds the 1 s SYN timeout, so 3 retransmissions are caused by the delay itself |
| `... run N=redis@3-6:delay,100ms,loss,30%` | VIOLATION retrans=5 | VIOLATION retrans=5 (`c16d8d18e625a0b1`): the loss asks for them |

Link faults on etcd 3.6.15 (`explore-ob-etcd-3.6.15-nq`, `021d350a…`, two builds identical;
old `explore-ob-etcd-3.6.15-main`, `a3842320…`): at the heal 34 (`N=etcd1-etcd2@5-10:delay,300ms`),
37 (`N=etcd1-*@10-20:delay,200ms`) and 6 (`O=etcd2-etcd3@5-12:resp:delay,500ms`) packets were in
the queue, all delivered (`left=0`); the old init freed them. All verdicts OK on both images;
new-image runs twice identical (`309c17a298f4cc14`, `46c0a710bf71e0fc`, `6ffff35b4266fd7f`).

**Schedules without a network fault keep their verdicts** (fingerprints move: the init's bytes
changed). Plain Redis example (`explore-ob-redis-nq`, `03beff6c…`, vs main's init `192f1f0a…`;
this manifest has no `network` fault kind, so no tc in the image): `run` OK acked=20;
`K=redis@5+3 P=redis@12+2 T=8:-86400` OK acked=15; `N=...` skipped (no tc) OK. `C=12 R=everysec`
is VIOLATION on both but loses `k:4` with main's init (twice `209c0bf7129cf00e`) and `k:2` with the
new one (twice `68da556788406ec3`): which key falls in Redis's 1 s fsync window depends on guest
timing, which the longer init script shifts (the same key moved at M52 and M61).

Stack explorer image `explore-binlog-ret-xr-nq-disk.img` (`4a6a8493…`, two builds identical; only
`/sbin/init` and `/etc/stack-explore.sh` differ from `explore-binlog-ret-xr`, checked with a
read-only mount and `diff -r`). Results `/var/tmp/nq/b2`, `/var/tmp/nq/swarm-*`.
- Without network faults, old vs new: `run`, `K=kafka@10+3`, `disk=cut C=20`,
  `P=mysql@15~100:400 T=5:86400` all OK 30/30 (20/30 with the cut) on both;
  `disk=cut C=20 M=2` VIOLATION on both (the relaxed-MySQL window), naming order 0 ("acked but
  not marked published") on the old image and order 9 ("MISSING, kafka has it") on the new one.
- With network faults (new image twice, identical; all OK 30/30 on both images):
  `N=kafka@5-10:delay,300ms` `84a353b7ebc93fdf`, `O=mysql@5-10:resp:delay,1000ms`
  `e6a879d3570488dc`, `N=redis@10-30:delay,200ms` `5e75794a7d1b3b5a`,
  `N=mysql@3-8:delay,50ms,loss,20%` `4dc5f28c0b68f608` (queued=1), 
  `O=kafka@12-18:req:delay,2000ms Q=4` `78c36fd2efabf063` (queued=5, drained in 1650 ms).
- `explore.py --strategy swarm --new-faults --durable-only --seeds 1-40`: 0 failures on the new
  image and 0 on the old one, same 40 schedule strings; 39 of 40 verdict lines identical, the
  40th (seed 8, a cut schedule) OK on both with one more un-ACKed order applied on the new image.
  30 schedules had a network fault; 21 heals ran, 9 with packets in the queue (12 packets),
  `left=0` in every one.

## The store readers in Rust (2026-10-31, M112)

`hydo-read`/`hydo-check` are a Rust program since M112 (`hydo/crates/hydo-read`; the Go program
of M88 was deleted after the comparison in `docs/HISTORY.md` Milestone 112). Images contain the
binary, so the reader examples have new fingerprints; every verdict and explanation line is the
one the Go images print, each schedule below twice with the same fingerprint (bare-metal test
machine, `hvx_explore` `38455ace…`; Rust hydo-read `64452374…` x86_64, `88576fa8…` arm64):

| example, image | schedule | verdict | Go | Rust |
|---|---|---|---|---|
| `stack/hydo-read.yaml` old order service (`ce559e51…`; Go `558b8248…`) | `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION `disjoint violated: 1 item of told_failed is also in kafka: 11` | `3c15d7ff5d519483` | `8f963d4f266998c9` |
| the same | `disk=none dseed=1 \| run` | OK, 7 held | `93fcc6553f5583c3` | `c8ddb1ac217db4f9` |
| `stack/hydo-read.yaml` fixed order service (`cf7901b5…`; Go `4a26328a…`) | the pinned reproducer | OK, 7 held | `d94e97cb016a49ca` | `640514fdd9b15f87` |
| the same | `disk=none dseed=1 \| run` | OK, 7 held | `c447d11b257cfcef` | `c26e8cb75971ea11` |
| `redis-composed-read` (`ae115f6f…`; Go rebuilt `cd91ca43…`) | `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK, 34 commands | `d8a292e551541f01` (rebuilt `e92fe548d3f2f9a8`) | `765d5a882c554ecb` |
| the same | `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | VIOLATION `subset violated: 1 item of 6 in acked not in redis: k:21=v21` | `da42b7722985605c` (rebuilt `d74802881611fa1c`) | `1d0f56120f6c4b38` |
| the same | `... R=always ...` | OK | `c8507284879bdaca` (rebuilt `b332ca6fdc9ddd02`) | `631b3aa767cba394` |
| `examples-mac/redis/hydo-read.yaml`, Mac (cpio `5592645f…`; Go `50d13de5…`) | `disk=none \| run` | OK `subset holds: all 20 items of acked are in redis (redis has 20 items)` | `d5577a4f26cffe1b` | `9bfed6578c82ffd0` |
| the same | `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION `subset violated: 12 items of 12 in acked not in redis: k:0=v0, ...` | `52eea0ecf13fb256` | `6b01ba56f975e9ce` |

The stack images with the Go binary rebuilt on the bare-metal test machine equal the M88 images
byte for byte, so the Rust stack images differ from them only in `/bin/hydo-read`. The other
seven recorded stack violations and four OK schedules give Go's verdict lines too (HISTORY M112);
miniflux: `examples/miniflux/RESULTS.md`. Images in `~/hydo-prd/build/img` on the box.
