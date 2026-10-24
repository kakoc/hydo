# compose-voting: Docker's example voting app, imported from its Compose file (Milestone 84)

Source: github.com/dockersamples/example-voting-app at commit `63e9150ca17af4ed05880d4245e486481f73fcb4`.
`docker-compose.yml` here is its `docker-compose.images.yml` (the prebuilt images), and
`healthchecks/` is its `healthchecks/` directory, both copied unmodified.

## Steps

1. Import, on the build machine, with skopeo from the nix store:
   `SKOPEO=<skopeo> hvx/onboard/hydo-compose docker-compose.yml`. The importer resolved the five
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
