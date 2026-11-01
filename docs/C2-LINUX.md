# C2 on Linux: verdict equivalence of agent images and shell images (Milestone 127)

Step C2 of `docs/design/guest-control.md` for the x86_64 engine (hvx): for each example, an
image whose first process is `hydo-agent` (driven by the host controller, `hydo-control`) is
compared with the image whose first process is the shell init (`hydo-init.sh` with its spliced
extensions). Fingerprints cannot be equal (another `/sbin/init`, another guest execution), so the
comparison is at the level of meaning: for every explorer schedule the verdict class (OK,
VIOLATION, ERROR) and the violated properties (the failing findings' ids), and for every
exploration the classes of the shrunk failures. The Mac examples (M128) and the bug benchmark
(M129) are other milestones.

All runs: the bare-metal machine, the main `hvx.ko` (srcversion
`0799F1B8EE2A35600D94EB4`, not reloaded), `hvx_explore` and `hydo-explore` built from this branch
in the tree `~/hydo-c2l`, every VM through `~/bin/hvx-run`, at most three VMs at a time (an
exploration with two workers plus one other run), `HYDO_QUIET_FILE=/var/tmp/hydo-QUIET` and a
`HYDO_EXPLORE_CACHE` per exploration. Results are in `/var/tmp/m127` on the box (one directory per
exploration, `cmp/` with the comparisons).

## How the agent images were made

There are three kinds of examples, by what their images need.

1. **Built here, both kinds (tier A).** The Redis family and the Docker voting app need no Nix:
   both images were built by this branch's `hydo build` with the Linux release's tools (M103's
   recipe: busybox, `hvx_seedrng`, `hydo_fc`, mke2fs and debugfs under the release's faketime,
   the bundled guest store), once as a shell image and once with `--control agent` (the pinned
   agent `ff7652a1…`), from the same inputs. The shell images are the recorded bytes of M103
   (`redis` `c6a09428…`, `hydo-netem` `3d9ba33c…`, `hydo-freeze` `8aa6bf16…`, `hydo-hist`
   `2621d3ce…`, `hydo-live` `84b5a929…`, `hydo-proxy` `ad8c5ce8…`, `redis-composed-hist`
   `cfead7dd…`, `redis-errors` `ddb995ea…`, `redis-bridge` `ab6847df…`, `compose-voting`
   `e4bcbcf8…`), and the Redis agent image is M121's `53f09ccc…`.
2. **Built here in M112's chroot (tier B).** The four-service stack (`stack/hydo.yaml` and
   `stack/hydo-read.yaml`, old and fixed order service) and miniflux need the MySQL, JDK, Kafka and
   PostgreSQL closures. M112 made a release-shaped root on the bare-metal machine with those closures taken out
   of recorded images (`~/hydo-prd`); both kinds were built in that chroot with this branch's
   `hydo build` (as uid 1000, gid 100, `HYDO_KEEP_OWNERS=1`, as M112 did).
3. **Patched from the recorded shell image (tier C).** etcd, the shop, the gRPC stack, the
   generated-driver stack, the error-log stack, rust-kv, avx2, the OCI examples and the user's
   workflow engine need inputs only the build machine can stage. Their agent images were made
   from the recorded shell images by `hv/hvx/images/patch-agent.sh`.

### The patch, and its proof against a real build

`hydo-build --control agent` differs from a shell build in three files only (`x86.rs`,
`agent.rs`, `init.rs` `install_init`): `/sbin/init` is the pinned agent instead of the shell init,
`/etc/hydo/control` (`agent 1`) is added, and `/etc/hydo/init-links.sh` (images of manifests with
`links:`) is not staged. `patch-agent.sh` makes exactly these changes with debugfs on the root
partition (offset 1 MiB): the files with hydo-build's owners, modes and times (mtime
`FIXED_MTIME`, atime, ctime and crtime 0), under the release's faketime (hydo-build's frozen clock,
then one pass with the clock at 0, so that a removed inode's deletion time is not 0 and the
superblock's write time is 0 as in a built image). The data partition is not touched; the result
must pass `e2fsck -fn`; the agent's sha256 must be the pin.

Proof on the Redis example (shell `c6a09428…`, built agent image `53f09ccc…`):

- **Bytes.** Two patches of one shell image give the same bytes (each tier-C image was patched
  twice). The patched image is not the built image's bytes, for three reasons: the inode numbers
  (mke2fs gives `/etc/hydo/control` the inode after `/etc/hydo` in its walk order, 419, and every
  later entry one more; the patch gives the new file the next free inode, 433), the blocks the
  agent occupies, and the superblock's lifetime-writes counter (7,121 kB built, 7,141 kB patched).
- **Trees.** Both root partitions mounted read-only and compared entry by entry (M112's
  `cmptree.sh`): the same 424 entries with the same names, link counts, modes, owners, sizes,
  symlink targets and contents (`diff -r` empty); only the inode numbers above differ. The data
  partitions are the same bytes (`2d52cd04…`, as the shell image's).
- **Runs.** The swarm strategy, seeds 1–30, on the patched image against the built agent image:
  the same verdict text for all 56 runs (30 first runs, re-runs and shrinks), including the
  lying-disk and I/O-error schedules (so the disk requests are the same), and the same control
  hash (`ctl=`, the same commands at the same stops) for 45 of the 56; the fingerprints differ
  (the guest's RAM holds other inode and block numbers).

So a patched image stands for a built agent image at the level of meaning, which is the level of
this campaign; its fingerprints are its own and are not baselines (C3 builds the real images).

RESULTS-PLACEHOLDER
