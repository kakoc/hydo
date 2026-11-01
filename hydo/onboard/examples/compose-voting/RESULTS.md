# compose-voting: Docker's example voting app, imported from its Compose file (Milestone 84)

Source: github.com/dockersamples/example-voting-app at commit `63e9150ca17af4ed05880d4245e486481f73fcb4`.
`docker-compose.yml` here is its `docker-compose.images.yml` (the prebuilt images), and
`healthchecks/` is its `healthchecks/` directory, both copied unmodified.

## Steps

1. Import, on the build machine, with skopeo from the nix store:
   `SKOPEO=<skopeo> hydo/onboard/hydo-compose docker-compose.yml`. The importer resolved the five
   image tags to digests (comments in the manifest; `hydo-compose.lock.json`), and it wrote
   `hydo.generated.yaml` (kept unchanged here) and `fetch-images.sh`. It printed 5 warnings:
   - The worker has no healthcheck and no port.
   - Two bind mounts of `./healthchecks` are copied into the read-only root.
   - The `front-tier` and `back-tier` networks are not modelled.
   - `vote` and `result` both use container port 80.
2. `sh fetch-images.sh` copied the 10 images (amd64 and arm64) by digest, 1.3 GB in total.
3. Hand edits, each marked `hand-written:` in `hydo.yaml`:
   - the workload: step i is one vote by `voter<i>` through the vote form, and HTTP 200 means acknowledged;
   - two invariants:
     - `votes.counted_soon` (eventually, 60 s after heal): every acknowledged vote is in PostgreSQL's `votes` table with the right choice;
     - `votes.counted`: the same check at the end;
   - `guest.ram_mib` 1536 -> 2048;
   - `result` moved to port 81 with `env: PORT: "81"`, because of the port collision the importer reported.
4. Build with the pinned toolchain, with `hvx_seedrng` compiled by the pinned musl gcc (`c2a440c5…`).
   The image is `explore-ob-compose-voting-disk.img`, sha256 `5d49ad99d00828ae7995b50deb6c9fad356d9271b0246ec9b19082ff80501de6`.
   Three builds gave identical bytes, one of them from the tree rebased onto main `7c9bf46`.

## Runs (bare-metal machine, main `hvx.ko` srcversion `CA5BF8BF2A0D66B312A68EF`)

Every run was a fresh boot:
`hvx_explore --ram-mib 2048 one … "<schedule>"`, with `hvx_explore` built from this branch.

| schedule | verdict | fingerprint | runs |
|---|---|---|---|
| `disk=none dseed=1 \| run` | OK: all 20 acknowledged votes counted (liveness after 3.15 s of guest time) | `bf06c27f2ef25df6` | 2, identical |
| `disk=none dseed=1 \| run K=worker@5+5` | OK: the worker is killed before step 5 and restarted before step 10, the votes wait in Redis, and all 20 are counted | `ccb3d9b71cb43d7f` | 2, identical (a third run was killed by the host's OOM killer while other sessions' VMs ran) |
| `disk=none dseed=1 \| run C=12` | VIOLATION `votes.counted_soon`: voter0 was acknowledged but never counted | `afaddce3a39c713e` | 2, identical |

The power cut result is the app's own design and not a hydo artefact. The vote app acknowledges
a vote as soon as the vote is in a Redis list, and this Redis has no persistence (no AOF). The 12
votes acknowledged before the cut were still in that list, because the worker polls only every
100 ms, so all 12 were lost at the cut. A team would read this as "the vote endpoint is
at-most-once".

## Getting it to run (what the first boots found)

- The first two boots (image `0f6d97c0…`) stopped at provisioning: PostgreSQL's `initdb` reads
  `/dev/fd/63` (bash process substitution), and the guest's `/dev` has no `/dev/fd`. Now the
  launcher of an image service with `mounts:` creates `/dev/fd` and `/dev/std{in,out,err}`, the
  links Docker puts in every container. This is commit "hydo_oci: /dev/fd ...". Image services
  without `mounts:` are unchanged.
- Provisioning a fresh boot takes 270–320 s of wall time, which is 20 s of guest time (five
  services, PostgreSQL's initdb). A workload run takes 12–13 s, and a run with a power cut takes
  126–197 s including the recovery boot.

## Milestone 95: each service in its own network namespace (`network: bridge`)

`hydo.yaml` now has `network: bridge` (MANIFEST.md, "Service networks"): every service runs in its
own network namespace at its `addr:` (10.89.0.11 to .15) on one bridge, as under Compose. The hand
edit that moved `result` to port 81 is gone: `vote` and `result` both listen on 0.0.0.0:80 again.
`hydo-loopback.yaml` is the Milestone 84 manifest, kept byte for byte. `hydo.generated.yaml` is the
new import (`network: bridge`, bridge addresses, no port-collision warning).

**Build.** On the bare-metal machine with the Linux release's `hydo build` (no Nix; the build
box was unavailable). The five amd64 images were fetched again by the digests in
`fetch-images.sh` with `hydo/onboard/fetch_image.py` (no skopeo on the bare-metal machine or the Mac).
- `hydo-loopback.yaml` -> `4f6be9f3…`/`87555754…` instead of M84's `5d49ad99…`. The only
  difference is file owners: the release sets every staged file to 0:0, while M84's plain build machine
  build kept the build user's 1000:100. `owners_replay.py` applies the release's owner step to a
  copy of M84's root partition; after it the two roots differ only in ext4's "Lifetime writes"
  counter (1413 MB against 1624 MB), and the data partition and the partition table are identical.
  So the re-fetched images and the bare-metal machine's tools reproduce M84's content.
- `hydo.yaml` (bridge) -> `explore-ob-compose-voting-disk.img` sha256 `d36a4a00568733236a999427ff3aef9e3bc264671b12a416cd8b2897d0df4ef4`
  (root `6df90161…`, data `13aaddb1…`), in `~/hydo-netns/build/br` on the bare-metal machine.

**Runs** (fresh boots, `hvx_explore --ram-mib 2048 one`, main `hvx.ko` `CA5BF8BF2A0D66B312A68EF`;
every schedule twice, both fingerprints equal):

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none dseed=1 \| run` | OK: all 20 votes counted, 3.10 s after the heal | `565fcfb8318327d6` |
| `disk=none dseed=1 \| run K=worker@5+5` | OK: the worker is killed before step 5 and restarted in its namespace before step 10; all 20 counted 3.11 s after the heal | `5f808fb08bb905de` |
| `disk=none dseed=1 \| run C=12` | VIOLATION `votes.counted_soon`: voter0 acknowledged but not counted (the M84 verdict: Redis has no AOF); the recovery boot brought all five services up in their namespaces | `ccb88b47eb6ef008` |
| `disk=none dseed=1 \| run N=worker-redis@5-12:loss,100%` | OK; the fault matched no packet (`vh1:sent=0,dropped=0 vh5:sent=0,dropped=0`): steps 5..11 last about 0.1 s of guest time and the .NET worker polls Redis only every 100 ms | `200379290aa090d0` |
| `disk=none dseed=1 \| run N=worker-db@5-12:loss,100%` | OK; no packet in the window either (`vh3:sent=0,dropped=0 vh5:sent=0,dropped=0`), for the same reason | `412d038b91edd6f5` |
| `disk=none dseed=1 \| run N=vote-redis@5-12:loss,100%` | OK with 13 of 20 votes acknowledged: the fault dropped 35 packets from vote to Redis (`vh1:sent=0,dropped=35`; none came back on `vh2`), so the 7 votes of steps 5..11 got no answer within the client's 5 s deadline (outcome unknown, which the invariant allows); the workload ran 35 s longer; the 13 acknowledged votes were counted 1.03 s after the heal | `da098498b325c557` |

**Before and after.** The same `N=worker-redis@5-12:loss,100%` on the Milestone 84 image
(`5d49ad99…`, one loopback network) passes with all votes counted 3.18 s after the heal and no
packet queued: there the fault matches nothing, because the worker's connections leave from
127.0.0.1. With `network: bridge` the fault sits on the bridge ports of the two services and
matches every packet between their addresses. Which links carry traffic during the workload is up
to the app: in this one the worker counts every vote about 3 s after the workload ends (each run
reports 3.10 s), so faults on the worker's links during the 0.25 s workload act on no packet, while
the vote app talks to Redis on every request and loses 7 acknowledgements to the same fault.

## On the Mac backend (Milestone 99)

`hydo-mac.yaml` = `hydo.yaml` with the arm64 images as `docker save` archives
(`fetch-images-mac.sh`, `fetch_image.py`, the lock's arm64 digests) and `guest.ram_mib: 3072` (on
the Mac the 1.1 GB root is the initramfs in guest RAM; 2048 MiB cannot unpack it). `network:
bridge` builds it for the second Mac guest kernel `Image-explore-netns`. The Mac `hydo compose`
now writes the same manifest as Linux. Image `f9038b23…`, disk `120e7088…` (two builds identical).
Every schedule twice from a fresh boot (`hydo run` on the M2 Pro), identical:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run` | OK: all 20 votes counted 3.0 s after the heal | `bd065a3e8dc7eebc` |
| `disk=none \| run K=worker@5+5` | OK: worker killed before step 5, restarted before step 10, all 20 counted | `642d89607d21aa2e` |
| `disk=cut \| run C=12` | VIOLATION `votes.counted_soon`: voter0 acknowledged but not counted (as on x86_64) | `1058b9d6fc1854f7` |
| `disk=none \| run N=vote-redis@5-12:loss,100%` | OK with 13 of 20 acknowledged; `vh1:sent=0,dropped=35`, as on x86_64 | `88b7fe56b6025545` |

## Milestone 134: the check says what it saw

`votes_counted` (in `hydo.yaml` and `hydo-mac.yaml`; `hydo-loopback.yaml` stays the Milestone 84
manifest byte for byte) used to report every failing `psql` as "db not answering", although the
usual cause is that no vote has been counted yet: the .NET worker creates the `votes` table only
after its first database connection (Milestone 130). The check now keeps `psql`'s error output
and says which of three things it saw:

| what the check saw | message | exit status |
|---|---|---|
| `psql` failed, and its error is not about the table | `db not answering: <psql's first error line>` | 2 (cannot decide) |
| the `votes` table does not exist | `no vote counted yet: the votes table does not exist (the worker creates it at its first database connection)` | 2 (cannot decide) |
| some acknowledged votes are missing | `too few votes counted: <n> of <total> acknowledged votes are in PostgreSQL; voter<i> (vote <v>) is not` | 1 (violated) |
| every acknowledged vote is there | `all <n> acknowledged votes counted` (unchanged) | 0 |

The exit statuses are the ones the check had before, so no verdict changes its kind; only the
texts of a failing check change (and the image, which holds the check).

**On this Mac, the Mac manifest** (`hydo-mac.yaml`, M2 Pro, this branch's `hydo build` with the
release-like prefix of docs/C2-MAC.md and the pinned agent 0.2.0, aarch64 `585f0bdb…`): shell image
`f7159341…`, agent image `c595a65d…` (two agent builds byte-identical), data disk `120e7088…`
(unchanged). Every recorded schedule ran twice from a fresh boot on each image (`hydo run`), and
both runs of each pair were identical:

| schedule | shell image | agent image |
|---|---|---|
| `disk=none \| run` | OK, all 20 votes counted, `35a6b85db4b735ba` | OK, `dd74bc1b87cc294d`, stops 4729, `ctl=1b9e5fbb98634f9c` |
| `disk=none \| run K=worker@5+5` | OK, all 20 counted, `96baab51fafc8d64` | OK, `2a717f95fb338b41`, stops 4786, `ctl=e39f6fb3fb98c8d0` |
| `disk=cut \| run C=12` | VIOLATION `votes.counted_soon`: `not satisfied 60.09s after heal, deadline 60s (last check rc=1: too few votes counted: 0 of 12 acknowledged votes are in PostgreSQL; voter0 (vote a) is not)`, `e388cf8a9b3240bd` | the same VIOLATION (60.83 s), `cfb222e3f27fb154`, stops 20522, `ctl=64b7f90f3b5cd36e` |
| `disk=none \| run N=vote-redis@5-12:loss,100%` | OK, 13 of 20 acknowledged and counted, `ad66bdd583205494` | OK, 13 counted, `e50a13e3ca70e1a9`, stops 12879, `ctl=aa587576ffe76bf2` |

The recorded runs used programs built before this milestone's controller changes; the agent
image's `disk=none | run` and `disk=cut | run C=12` were run again with the final programs and gave
the same fingerprints, stops, `inh=` and `ctl=` (no check of these schedules is ended by a signal,
and the manifest has no restart policy). These replace the Mac values of Milestone 99 (shell) and Milestone 128 (agent) for the current
manifest; the verdicts are the same. The x86_64 images were not rebuilt (no VM budget was spent on
them on the bare-metal machine); the re-baseline at the switch to agent images records them.
