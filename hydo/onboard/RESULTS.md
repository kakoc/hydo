# Onboarding v1 — proof results (2026-10-18, bare-metal machine)

Images built by `hydo-build` on the build machine (byte-identical across rebuilds), run with
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

## Liveness invariants (`type: eventually`) on Redis (2026-10-21, bare-metal machine)

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

Builds (build machine, each built twice in separate build directories, byte-identical):

| image | sha256 |
|---|---|
| `explore-ob-redis-oci-disk.img` (x86_64, from a docker-archive tar) | `ca3ea2b0…` (ext4 root `b9e56872…`) |
| `explore-ob-redis-oci-arm64.cpio.gz` (`--arch arm64`, from an OCI layout) | `f575448a…` (disk `d4116ff4…`) |

The same amd64 image as an OCI layout directory gives an identical `/oci/redis` tree (content,
owners, modes; only `image.json` differs). Feeding the arm64 image to the x86_64 build, or the
amd64 image to `--arch arm64`, is refused. Existing manifests build byte-identically with the new
hydo-build: `examples/redis` `0dd509c2…` (same as main), arm64 `hydo/onboard/examples-mac/redis`
cpio `10f7858a…` / disk `9ddfc929…` (the values in `hv/hvm/explore/README.md`), and `examples/stack` (FRONT=fix) disk `37a2ff6a…` with both main's and the new hydo-build.

Runs on the bare-metal machine (`hvx_explore one`, results `/var/tmp/m59-oci`), each twice:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=1 \| run` | OK, acked=20 all present | `d6a18f52f6418cf5` (both) |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK, acked=15 | `4ffee7177898e58e` (both) |
| `disk=none dseed=1 \| run C=12 R=everysec` | VIOLATION `key k:1 lost` (Redis's documented fsync window) | `9c36c7d46bcb9ddf` (both) |

### The arm64 image booted on the Mac (2026-10-21, M2 Pro, `hvmx`)

The arm64 image was fetched again on the build machine (`fetch-image.sh`; the OCI layout is identical to
the earlier fetch) and built with `hydo-build --arch arm64`: `explore-ob-redis-oci-arm64.cpio.gz`
`f575448a…`, disk `d4116ff4…`, the same bytes as the M59 build. Kernel `Image-explore` `f80c5655…`
(the release kernel, HVC idle, with `tc`), `hvmx` built from `main` (`hv/hvm/explore/build.sh`).

Each schedule ran in two separate `hvmx serve` processes (restored from the S1 snapshot) and once
as `hvmx one` (a fresh boot), with `hv/hvm/explore/netfault/check.py` (deleted in M147). All three fingerprints are
equal for every schedule:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run` | OK, acked=20 all present | `df1b39fdaa4ecf79` |
| `disk=none \| run K=redis@5+2 P=redis@12+2` (kill, then pause) | OK, acked=16 | `e6b9a7eef5dc96e6` |
| `disk=none \| run K=redis@5+2` | OK, acked=18 | `6b9321d4b13e5d12` |
| `disk=none \| run P=redis@8+2` | OK, acked=18 | `cc7f5ce830f256fb` |
| `disk=cut \| run C=12` (power cut, `appendfsync always`) | OK, acked=12 | `9bfe88561695340d` |
| `disk=cut \| run C=12 R=everysec` (power cut) | VIOLATION `key k:1 lost` (Redis's documented fsync window) | `73b907b554f95f1a` |

The bare-metal machine's three schedules on the Mac (same check, all identical); the verdicts equal the x86_64
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

## Lifting a network fault no longer drops netem's queued packets (2026-10-23, bare-metal machine)

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
fingerprints. Results `/var/tmp/nq/b1` on the bare-metal machine.

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

The stack images with the Go binary rebuilt on the bare-metal machine equal the M88 images
byte for byte, so the Rust stack images differ from them only in `/bin/hydo-read`. The other
seven recorded stack violations and four OK schedules give Go's verdict lines too (HISTORY M112);
miniflux: `examples/miniflux/RESULTS.md`. Images in `~/hydo-prd/build/img` on the bare-metal machine.

## Milestone 136: agent images with hydo-agent 0.2.0

Since M136 every image is an agent image (the guest agent `hydo-agent` as the first process, the
host controller driving it; docs/HISTORY.md "Milestone 136"), so every fingerprint above this
section belongs to a shell-init image of an earlier milestone. Its values, each schedule
run twice with the same fingerprint (docs/REBASELINE.md section 8, which also says how each image
was made):

**x86_64 (hvx), redis**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `6875291e90072ae6` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `53f01e40711ccfdf` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `7b2f0cd241ca4d4c` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `beb90e457eed6cd7` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `e94d248dc0263db7` | yes |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK invariants=1 held (redis.acked_writes_survive: acked=15 all present) | `9161ea99985a7dfc` | yes |
| `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `1ab0adbd21b84609` | yes |

**x86_64 (hvx), redis-netem**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=7.99) | `ccbf6defbc79c73d` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | VIOLATION [net.no_retransmission] retrans=20 timeouts=16 uptime=14.08 | `0709180f00628487` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.08 | `bf326d61b307fe1b` | yes |

**x86_64 (hvx), redis-freeze**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `00e91cf282302822` | yes |
| `disk=none dseed=0 \| run F=redis<workload@5#1:150000` | OK invariants=1 held (redis.acked_writes_survive: acked=5 all present) | `84ccade7ac058525` | yes |
| `disk=none dseed=0 \| run F=redis<workload@12#0:1` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `635d755e11838cc8` | yes |

**x86_64 (hvx), redis-live**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `7dadf3d612873e3f` | yes |
| `disk=none dseed=1 \| run K=redis@5+3` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `c1532e566d404833` | yes |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | VIOLATION [redis.pending_resolved] not satisfied 10.14s after heal, deadline 10s (last check rc=1: pending=1) | `eb479d619d4f470b` | yes |

**x86_64 (hvx), redis-proxy**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `b32e2470daf79567` | yes |
| `disk=none \| run H=redisp@2-5:drop` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `8d758359f1f1cbb0` | yes |
| `disk=none \| run H=redisp@1-3:reset K=redis@4+2` | OK invariants=1 held (redis.acked_writes_survive: acked=16 all present) | `0b4d666bbd6c6873` | yes |

**x86_64 (hvx), redis-ram**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `66426149ddd86227` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `68c822c265af3c69` | yes |

**x86_64 (hvx), redis-composed**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `9699814e85527567` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | VIOLATION [acked_writes_survive] key k:21 lost | `ecc0f218e02c56a4` | yes |

**x86_64 (hvx), redis-composed-read**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `e993025ac922649e` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: no composed workload ran in this boot) | `40a281b8671f1da6` | yes |

**x86_64 (hvx), redis-errors**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `014ee59eba36eadf` | yes |
| `disk=ioerr:1 dseed=281cc34161f446f4 \| run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,oanytime_read` | OK invariants=5 held (compose.commands: started=6 ok=4 failed=2 timeout=0 violation=0 killed=0 skipped=0) | `b35ee148adf7d89a` | yes |

**x86_64 (hvx), stack-fix**

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

**x86_64 (hvx), stack-old**

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

**x86_64 (hvx), stack-read-fix**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `84d920df46d6e1be` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `2b09df294c12b4c4` | yes |

**x86_64 (hvx), stack-read-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `c9d9b56b99037daf` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [order.told_failed_not_applied] disjoint violated: 1 item of told_failed is also in kafka: 11 | `ec83563bdd9838ef` | yes |

**x86_64 (hvx), stack-errors-fix**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `348cd1e351ee0583` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `e17626b8099af7e3` | yes |
| `disk=ioerr:5 dseed=1 \| run` | ERROR restart after the faults (mysql) | `6d8a523700f109e4` | yes |

**x86_64 (hvx), stack-errors-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `ecbdc4b8fed098c8` | yes |

**x86_64 (hvx), stack-grpc-cur**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `444c61285b66c236` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `9242ed976a23fb24` | yes |

**x86_64 (hvx), stack-grpc-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `c3ea21902906e620` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `1dfcf63387024add` | yes |

**x86_64 (hvx), stack-grpc-planted**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `fb676b97ed846766` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | VIOLATION [stack.exactly_once] kafka order=10 present 2 times (must be exactly once) | `441b8084ef4767db` | yes |

**x86_64 (hvx), stackgen**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `05f46c749f5f6cc5` | yes |
| `disk=none \| run K=front@10+2` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `d7c83fa446cd0313` | yes |
| `disk=cut \| run C=15` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `aef706013160bb3a` | yes |

**Mac (hvm), redis**

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

**Mac (hvm), redis-read**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: subset holds: all 20 items of acked are in redis (redis h... | `6ae40167e8288b87` | `f3ba88cf863b6426` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] subset violated: 12 items of 12 in acked not in redis: k:0=v0, k:1=v... | `cdc8de057c26cbb3` | `71466bb198edf9f9` | yes |

**Mac (hvm), redis-ram**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `067dd2c6d54e2ca6` | `9d2a1edace89eb0f` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `ca5031a62951d171` | `533ee15b0e8b4e30` | yes |

**Mac (hvm), redis-composed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `5efdd2f240a987e7` | `5b8c3c98d0ddb1b4` | yes |

**Mac (hvm), redis-composed-racy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | VIOLATION [total_conserved] balances sum to 495, not 500 | `0d5d1e702e3ac25e` | `1a0a10b88c498849` | yes |

**Mac (hvm), ledger**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `0d47c63d0101b506` | `f68317656d50efba` | yes |
| `disk=none \| run O=front@5-8:resp:loss,100%` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `946266e1c874aa0b` | `de5826d41aa3ceef` | yes |
| `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION [ledger.acked_exactly_once] entry 1 stored 2 times | `5af3b8a3cab4723b` | `11127ff78b0d4810` | yes |
| `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `61113c627c33395f` | `162cc6d22b65be38` | yes |
| `disk=none \| run N=front@0-30:loss,60%` | VIOLATION [ledger.acked_exactly_once] entry 9 stored 2 times | `d1536c0e926261c8` | `fbff9fa6de9b9cf0` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `fda105a8fa77341b` | `27229cb79acbbb1b` | yes |

**Mac (hvm), ledger-spin**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `e0518577f10b88c3` | `8898c0caab66f7ff` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `2dcb3c0bf16ed0c0` | `24f5e0f77c0e0778` | yes |

**Mac (hvm), node-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `f15f18b0c9841e28` | `541802f17cca1c77` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `46e0b639fcbcdf95` | `1e967c83932acc4b` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `001f3efd81700a81` | `ac19dfcca9ed9f5e` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `258ef05237ca123e` | `a907a15d495d8deb` | yes |

**Mac (hvm), grpc-buggy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | VIOLATION [orders.consistent] VIOLATION request_id r1 created 5 orders: o11 o1 o2 o3 o4 | `1c3ee0d25016e91b` | `449b66182014be39` | yes |

**Mac (hvm), grpc-fixed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | OK invariants=2 held (compose.commands: started=2 ok=1 failed=1 timeout=0 violation=0 killed=0 skipped=0) | `00d2c1058ea96480` | `dab029d6add95a35` | yes |

## Milestone 140: hydo-agent 0.3.0 and the host-call port

Since M140 `hydo-build` stages hydo-agent 0.3.0 and the hvx engine offers the host-call port by
default (docs/HISTORY.md "Milestone 140"), so every image has new bytes and every fingerprint of
the M136 section above is superseded. The current values, each schedule run twice with the same
fingerprint (docs/REBASELINE.md section 9, which also says how each image was made; on x86_64 the
agent's lines travel over the host-call port, on the Mac over the serial line):

**x86_64 (hvx), redis**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `e7de012271d99d0a` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `a678f41e98c1ca76` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `784ad4a2ab462de1` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `3c57c1db1c05c410` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `dd4791c21b813908` | yes |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK invariants=1 held (redis.acked_writes_survive: acked=15 all present) | `025eecdf01c72e8c` | yes |
| `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `4c6dbfaae010725b` | yes |

**x86_64 (hvx), redis-netem**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=8.05) | `7800b04875b8c265` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | VIOLATION [net.no_retransmission] retrans=12 timeouts=9 uptime=16.34 | `2f689cdd7faf7278` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.16 | `d20e0c833ff5c16b` | yes |

**x86_64 (hvx), redis-freeze**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `24624254f42ab5de` | yes |
| `disk=none dseed=0 \| run F=redis<workload@5#1:150000` | OK invariants=1 held (redis.acked_writes_survive: acked=5 all present) | `b5b4a5367a680c36` | yes |
| `disk=none dseed=0 \| run F=redis<workload@12#0:1` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `72c7301945727c36` | yes |

**x86_64 (hvx), redis-live**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `3a4c6f99afe4ccb3` | yes |
| `disk=none dseed=1 \| run K=redis@5+3` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `b71a52f6e2e7e8c0` | yes |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | VIOLATION [redis.pending_resolved] not satisfied 10.16s after heal, deadline 10s (last check rc=1: pending=1) | `daf98f52e714d103` | yes |

**x86_64 (hvx), redis-proxy**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `1b5ac9bc529d2b73` | yes |
| `disk=none \| run H=redisp@2-5:drop` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `e8a48ac08bbcd6fe` | yes |
| `disk=none \| run H=redisp@1-3:reset K=redis@4+2` | OK invariants=1 held (redis.acked_writes_survive: acked=16 all present) | `0e5462ab0bc7fd75` | yes |

**x86_64 (hvx), redis-ram**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `c781388975e573af` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `fd3e4a85d7ed5448` | yes |

**x86_64 (hvx), redis-composed**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `aa64393559c1ad8c` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | VIOLATION [acked_writes_survive] key k:21 lost | `bec487fb247ea7de` | yes |

**x86_64 (hvx), redis-composed-read**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `ab1689254694d5cd` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: no composed workload ran in this boot) | `01a664be89cc4e0e` | yes |

**x86_64 (hvx), redis-errors**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `65bcc8329249d3a9` | yes |
| `disk=ioerr:1 dseed=281cc34161f446f4 \| run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,oanytime_read` | OK invariants=5 held (compose.commands: started=6 ok=4 failed=2 timeout=0 violation=0 killed=0 skipped=0) | `2a9a315d66a895cf` | yes |

**x86_64 (hvx), stack-fix**

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

**x86_64 (hvx), stack-old**

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

**x86_64 (hvx), stack-read-fix**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `31747d42da9db86a` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `c1baad113f4123b0` | yes |

**x86_64 (hvx), stack-read-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `999585f7452513a4` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [order.told_failed_not_applied] disjoint violated: 1 item of told_failed is also in kafka: 11 | `931d0c941ab83a14` | yes |

**x86_64 (hvx), stack-errors-fix**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `cc19dbda476a4b57` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `fb32f0301654d355` | yes |
| `disk=ioerr:5 dseed=1 \| run` | ERROR restart after the faults (redis) | `47701c32e88789f2` | yes |

**x86_64 (hvx), stack-errors-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `209fb1b264129ef7` | yes |

**x86_64 (hvx), stack-grpc-cur**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `dd59dc534dad98d2` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `9442c7562964fa21` | yes |

**x86_64 (hvx), stack-grpc-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `5525e06e8c7cc7d5` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ad76d475e916b7d8` | yes |

**x86_64 (hvx), stack-grpc-planted**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `4a70ccf46a07d57f` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | VIOLATION [stack.exactly_once] kafka order=10 present 2 times (must be exactly once) | `6e9f9d545da56de1` | yes |

**x86_64 (hvx), stackgen**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `7069a01e93b6190d` | yes |
| `disk=none \| run K=front@10+2` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `d047b98066d388b2` | yes |
| `disk=cut \| run C=15` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `a33bed27be75792b` | yes |

**Mac (hvm), redis**

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

**Mac (hvm), redis-read**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: subset holds: all 20 items of acked are in redis (redis h... | `f06758ca0247dbd8` | `8476030a9d80c61e` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] subset violated: 12 items of 12 in acked not in redis: k:0=v0, k:1=v... | `95d7e1ca3d487825` | `19138a12b5ab46af` | yes |

**Mac (hvm), redis-ram**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `7a59995ac88ef7ce` | `23d53d97e4d64efa` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `5ac947f4a7487097` | `93ebd8cf695051da` | yes |

**Mac (hvm), redis-composed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `8e6df1a1d61c89a5` | `64228edfdcc3de50` | yes |

**Mac (hvm), redis-composed-racy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | VIOLATION [total_conserved] balances sum to 503, not 500 | `e521936cacc3758f` | `cc25282fa6ee57cf` | yes |

**Mac (hvm), ledger**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `c862785a330bfab8` | `c9fd7b0f480180cd` | yes |
| `disk=none \| run O=front@5-8:resp:loss,100%` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `7efd0546c264d279` | `c15ce94773db279a` | yes |
| `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION [ledger.acked_exactly_once] entry 1 stored 2 times | `a0717cd470073ba2` | `aea4258f9a112981` | yes |
| `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `9a65359fb397b0a3` | `8552ef571c61fb8f` | yes |
| `disk=none \| run N=front@0-30:loss,60%` | VIOLATION [ledger.acked_exactly_once] entry 9 stored 2 times | `03ab518387ad9e81` | `e50f44fc62acc4da` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `3a9697e31d03494f` | `382b70a8955072c9` | yes |

**Mac (hvm), ledger-spin**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `25f1123366ea96c6` | `930f998155f77b29` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `8dd9c5537321c0e2` | `012f769bba1db408` | yes |

**Mac (hvm), node-kv** (the values of Milestone 152, image `2ac9dc0c…`: the SDK's `package.json`
inside the image now says `Apache-2.0`; docs/REBASELINE.md section 12; the M140 values are in
docs/HISTORY.md "Milestone 152")

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `3def1ce155691292` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `59bd74f26a0e3d97` | `8de6e3d2ba8ff5d8` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `33c406be62f5df36` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `ae1db81f2458fe15` | `39129eaa9b7e2227` | yes |

**Mac (hvm), grpc-buggy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | VIOLATION [orders.consistent] VIOLATION request_id r1 created 5 orders: o11 o1 o2 o3 o4 | `87140952dd83ad1d` | `e6766eac8f5475fc` | yes |

**Mac (hvm), grpc-fixed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | OK invariants=2 held (compose.commands: started=2 ok=1 failed=1 timeout=0 violation=0 killed=0 skipped=0) | `01b82adb6d3f4ae7` | `9fcb1c71aa47fc8b` | yes |

## Milestone 155: the image record (the current values)

Since M155 every image carries `/etc/hydo/image.json` (the guest kernel it was built for, by
sha256, and the fingerprint epoch; docs/HISTORY.md "Milestone 155"), so every image has new bytes
and every fingerprint of the M140 section above is superseded; the agent, the engines and the
controller are M140's, so the verdicts are M140's (docs/REBASELINE.md section 13 lists the lines
that differ). These are fingerprint epoch 1. The current values, each schedule run twice with the
same fingerprint (docs/REBASELINE.md section 13, which also says how each image was made):

**x86_64 (hvx), redis**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `37fcf7394c6d8099` | yes |
| `disk=none \| run K=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `002ee5dae3a491cf` | yes |
| `disk=none \| run P=redis@5+2` | OK invariants=1 held (redis.acked_writes_survive: acked=18 all present) | `726798145db9e2f9` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `5c245487b5f1b4f8` | yes |
| `disk=cut \| run C=12 R=always` | OK invariants=1 held (redis.acked_writes_survive: acked=12 all present) | `238e54a7507a11d2` | yes |
| `disk=none dseed=1 \| run K=redis@5+3 P=redis@12+2 T=8:-86400` | OK invariants=1 held (redis.acked_writes_survive: acked=15 all present) | `068a688045997d87` | yes |
| `disk=cut dseed=1f16055b6d41c6d9 \| run C=9 R=no` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `88473d78441a2ef0` | yes |

**x86_64 (hvx), redis-netem**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (net.no_retransmission: retrans=0 timeouts=0 uptime=8.05) | `0c1ab03e91f01184` | yes |
| `disk=none \| run N=redis@5-10:loss,60%` | VIOLATION [net.no_retransmission] retrans=12 timeouts=9 uptime=16.34 | `12bef4fa21a9cca9` | yes |
| `disk=none \| run O=redis@5-8:resp:loss,100%` | VIOLATION [net.no_retransmission] retrans=11 timeouts=8 uptime=14.15 | `63e8785c9e26a9cc` | yes |

**x86_64 (hvx), redis-freeze**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `c54a1256c7219eef` | yes |
| `disk=none dseed=0 \| run F=redis<workload@5#1:150000` | OK invariants=1 held (redis.acked_writes_survive: acked=5 all present) | `722243ef48894ba4` | yes |
| `disk=none dseed=0 \| run F=redis<workload@12#0:1` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `594ee228b16f2ca2` | yes |

**x86_64 (hvx), redis-live**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `ebdc9f149aaa9f90` | yes |
| `disk=none dseed=1 \| run K=redis@5+3` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `b4388cf37bd1ca81` | yes |
| `disk=none dseed=1 \| run P=redis@18~0:20000` | VIOLATION [redis.pending_resolved] not satisfied 10.16s after heal, deadline 10s (last check rc=1: pending=1) | `d97ccc5bfc664c68` | yes |

**x86_64 (hvx), redis-proxy**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: acked=20 all present) | `ca9f41a4ba161da6` | yes |
| `disk=none \| run H=redisp@2-5:drop` | OK invariants=1 held (redis.acked_writes_survive: acked=17 all present) | `d849b15699b028f4` | yes |
| `disk=none \| run H=redisp@1-3:reset K=redis@4+2` | OK invariants=1 held (redis.acked_writes_survive: acked=16 all present) | `348442b1011e7081` | yes |

**x86_64 (hvx), redis-ram**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `a0835b988223f147` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:1 lost | `b41c79d047e98358` | yes |

**x86_64 (hvx), redis-composed**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `96023abe8675ca7b` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=85ebca77,p4,q2,r100` | VIOLATION [acked_writes_survive] key k:21 lost | `c0daeb01f242ebb7` | yes |

**x86_64 (hvx), redis-composed-read**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `6f32919d10ab40a5` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec W=0000002a,p4,q2,r100` | OK invariants=6 held (compose.commands: no composed workload ran in this boot) | `f10eda1b5db0ea3a` | yes |

**x86_64 (hvx), redis-errors**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `21bbdabe5ad64801` | yes |
| `disk=ioerr:1 dseed=281cc34161f446f4 \| run K=redis@1+1 W=6311ab23,p1,q2,r100,n6,oanytime_read` | OK invariants=5 held (compose.commands: started=6 ok=4 failed=2 timeout=0 violation=0 killed=0 skipped=0) | `7fc3174b9b541522` | yes |

**x86_64 (hvx), stack-fix**

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

**x86_64 (hvx), stack-old**

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

**x86_64 (hvx), stack-read-fix**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `2b44d170d8b45013` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `bbe5b38ca4cbd2b8` | yes |

**x86_64 (hvx), stack-read-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=7 held (mysql.amounts_valid: count holds: wrong_amount has 0 records (want == 0)) | `73634cec66706c6e` | yes |
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [order.told_failed_not_applied] disjoint violated: 1 item of told_failed is also in kafka: 11 | `0e848bdfb5567c10` | yes |

**x86_64 (hvx), stack-errors-fix**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `1a56a26c0fcc566e` | yes |
| `disk=none dseed=1 \| run K=kafka@10+3 P=mysql@15~100:400` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `21e99ebd08442e3f` | yes |
| `disk=ioerr:5 dseed=1 \| run` | ERROR restart after the faults (mysql) | `3dee52a138056f08` | yes |

**x86_64 (hvx), stack-errors-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=4d5cb825dfaab05b \| run K=redis@11+1 C=24` | VIOLATION [stack.exactly_once] order=11 is in kafka (1 times) but the client was never ACKed | `d4ee59df9b431dbb` | yes |

**x86_64 (hvx), stack-grpc-cur**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `1727a83353d9d2be` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `ad2d115c82989fda` | yes |

**x86_64 (hvx), stack-grpc-old**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `d0f18e1652e0e731` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `b77a87a60dc14f6c` | yes |

**x86_64 (hvx), stack-grpc-planted**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK invariants=4 held (mysql.amounts_valid: rows with a wrong amount: 0) | `47055f125f26994f` | yes |
| `disk=none dseed=1 \| run H=orders.place@10-13:drop` | VIOLATION [stack.exactly_once] kafka order=10 present 2 times (must be exactly once) | `8786ec4211bfbecd` | yes |

**x86_64 (hvx), stackgen**

| schedule | verdict | fingerprint | twice |
|---|---|---|---|
| `disk=none \| run` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `42c34b875d91ad13` | yes |
| `disk=none \| run K=front@10+2` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `34b626826e68fe5f` | yes |
| `disk=cut \| run C=15` | OK invariants=3 held (mysql.amounts_valid: rows with a wrong amount: 0) | `4103289e58dd5177` | yes |

**Mac (hvm), redis**

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

**Mac (hvm), redis-read**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (redis.acked_writes_survive: subset holds: all 20 items of acked are in redis (redis h... | `22331a2dab9a2dfe` | `8476030a9d80c61e` | yes |
| `disk=cut dseed=1 \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] subset violated: 12 items of 12 in acked not in redis: k:0=v0, k:1=v... | `f95fb7be44801a83` | `19138a12b5ab46af` | yes |

**Mac (hvm), redis-ram**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (redis.acked_writes_survive: acked=20 all present) | `d9a41c337d7b63bf` | `23d53d97e4d64efa` | yes |
| `disk=cut \| run C=12 R=everysec` | VIOLATION [redis.acked_writes_survive] key k:0 lost | `01dcdc20e141301b` | `93ebd8cf695051da` | yes |

**Mac (hvm), redis-composed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | OK invariants=5 held (compose.commands: started=34 ok=34 failed=0 timeout=0 violation=0 killed=0 skipped=0) | `adfdd1128b47b2e1` | `64228edfdcc3de50` | yes |

**Mac (hvm), redis-composed-racy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run W=0000002a,p4,q2,r100` | VIOLATION [total_conserved] balances sum to 503, not 500 | `9bcde106d244c7c8` | `cc25282fa6ee57cf` | yes |

**Mac (hvm), ledger**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `de32ee8c5ee6f69a` | `c9fd7b0f480180cd` | yes |
| `disk=none \| run O=front@5-8:resp:loss,100%` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `6f14d9d934c84dc0` | `c15ce94773db279a` | yes |
| `disk=none \| run O=front@0-30:resp:loss,70%` | VIOLATION [ledger.acked_exactly_once] entry 1 stored 2 times | `a897f325702de127` | `aea4258f9a112981` | yes |
| `disk=none \| run O=front@0-30:resp:loss,50%` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `6da86067e774a48d` | `8552ef571c61fb8f` | yes |
| `disk=none \| run N=front@0-30:loss,60%` | VIOLATION [ledger.acked_exactly_once] entry 9 stored 2 times | `a18e3e452a41ba3e` | `e50f44fc62acc4da` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `6abc98fcb198db88` | `382b70a8955072c9` | yes |

**Mac (hvm), ledger-spin**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=2 held (ledger.acked_exactly_once: acked=30 each stored once) | `e32475acef7341b4` | `930f998155f77b29` | yes |
| `disk=none \| run P=front@10+2` | VIOLATION [ledger.acked_exactly_once] entry 10 stored 2 times | `0caf9beb9125978d` | `012f769bba1db408` | yes |

**Mac (hvm), node-kv** (superseded by "Milestone 157" at the end of this file)

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `eb0811dba0a01354` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `846b02b08b161419` | `8de6e3d2ba8ff5d8` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `f9847e1a9e687a15` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `9e6672f7f7f52ff7` | `39129eaa9b7e2227` | yes |

**Mac (hvm), grpc-buggy**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | VIOLATION [orders.consistent] VIOLATION request_id r1 created 5 orders: o11 o1 o2 o3 o4 | `28417fb1c34bcfac` | `e6766eac8f5475fc` | yes |

**Mac (hvm), grpc-fixed**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none dseed=d7071eb1f9d436b9 \| run P=redis@16+11 W=5764a73c,p1,q2,r200,n3,x0.2.6.13.26-27.34.46-47,o0.2-3` | OK invariants=2 held (compose.commands: started=2 ok=1 failed=1 timeout=0 violation=0 killed=0 skipped=0) | `ec450ee463da3f5d` | `9fcb1c71aa47fc8b` | yes |

## Milestone 157: node-kv with the SDK comment reworded (the current values)

Milestone 157 reworded a comment in `sdk/js/index.js`, which node-kv copies into its image, so
the node-kv image has new bytes (initramfs `ae2bbb9b231f295c…`) and new fingerprints; the verdict
lines and the ctl values are Milestone 155's. Each schedule ran twice with the same fingerprint
(docs/REBASELINE.md section 14). Fingerprint epoch 1.

**Mac (hvm), node-kv**

| schedule | verdict | fingerprint | ctl | twice |
|---|---|---|---|---|
| `disk=none \| run` | OK invariants=1 held (nkv.oracle: acked=20 all present) | `5f47c9a775cd40a8` | `0141194b466e3bd9` | yes |
| `disk=cut \| run C=12 S=never` | VIOLATION [nkv.oracle] key k:0 lost (got null) | `3146b75677356b19` | `8de6e3d2ba8ff5d8` | yes |
| `disk=cut \| run C=12` | OK invariants=1 held (nkv.oracle: acked=12 all present) | `1cbb888242587805` | `202dcc1810826541` | yes |
| `disk=none \| run K=nkv@8+2 B=1234` | OK invariants=1 held (nkv.oracle: acked=18 all present) | `772d0ca8f633d3be` | `39129eaa9b7e2227` | yes |
