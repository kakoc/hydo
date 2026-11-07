# The Mac release of hydo

`build-release.sh` turns the Mac backend into a self-contained tarball that a user installs with
one command and runs with no Linux machine, no root and no other software: since Milestone 107
nothing in the release uses Python, and since Milestone 150 nothing on the Mac needs a C compiler
(`hydo doctor` no longer asks for the Command Line Tools; `codesign` is part of macOS).
`hydo/dist/mac/QUICKSTART-mac.md` at the top of the repository is the user guide; it is shipped
inside the release.

## What is in a release

| Path | What |
|---|---|
| `bin/hydo`, `bin/hydo-build`, `bin/hydo-fetch-image` | the Rust programs (`aarch64-apple-darwin`, built by `build-release.sh` or taken from `$HYDO_RUST_BIN_DIR`): `hydo` is the one CLI (`hydo/crates/hydo-cli`; the same program as on Linux, with the same behaviour since Milestone 107), with the host controller that drives every image's guest agent (Milestone 136), `hydo-build` the image builder with its own command line (for scripts), `hydo-fetch-image` what the `fetch-images.sh` of `hydo compose` calls |
| `bin/hydo-hvm`, `hv/hvm/hv.entitlements` | the Mac engine (`hydo/crates/hydo-hvm`, Rust since Milestone 143; the C `hvm` and `hvmx` were deleted in Milestone 147), signed with the hypervisor entitlement on the user's Mac by `hydo setup` (by `install.sh` before Milestone 154); it runs every guest, `hydo build`'s helper VM included |
| `hv/hvm/explore/out/Image-explore` | the arm64 kernel (`hv/hvm/explore/build-guest.sh`, HVC idle) |
| `hv/hvm/explore/out/Image-explore-netns` | the second arm64 kernel (`hv/hvm/explore/build-guest-netns.sh`, `806807e7…`, docs/BUILD-PINS.md): `Image-explore` plus network namespaces, veth, bridge and NFQUEUE, for manifests with `network: bridge`, `addr:` or `faults: [freeze]` (Milestone 99; shipped since Milestone 150) |
| `guest/hydo-agent` | the guest agent (`hydo/crates/hydo-agent`, 0.3.0, aarch64 `099e5444…`), every image's first process; built from the tree by `hydo/crates/hydo-agent/build.sh`, and `hydo build` checks its pinned sha256 |
| `guest/hydo-world` | the in-guest program of `fakes:` and `proxies:` (`hydo/crates/hydo-world`, built from the tree, `fefcca66…`) |
| `guest/hydo-read` | `hydo-read` and, through a link in the image, `hydo-check` (`hydo/crates/hydo-read`, built from the tree; `build-release.sh` and `hydo setup` check the pinned Mac build `88576fa8…` of `guest-programs.sha256`; shipped since Milestone 150) |
| `guest/hydo_fc` | the freeze helper (`hydo/onboard/hydo_fc.c` built for aarch64 with the pinned Arm GNU toolchain, `499f654e…`), which `hydo build` stages for `faults: [freeze]` (an agent image freezes with the agent's own packet trigger; open item 30 in AGENTS.md; shipped since Milestone 150) |
| `guest/busybox`, `guest/mke2fs` | static arm64 busybox 1.36.1 and e2fsprogs 1.47.0 (aarch64 nixpkgs) |
| `guest/busybox-applets[.txt]` | the applet set to link, as the Linux build links it |
| `guest/tc/`, `guest/tc-closure.txt`, `guest/netns-closure.txt` | the pinned aarch64 `tc` and `ip` (iproute2 6.8.0), the iptables 1.8.10 files the freeze uses, and every file their loader opens, at their `/nix/store` paths; `guest/tc` is the release's `HYDO_ARM64_STORE` |
| `hydo/onboard/MANIFEST.md`, `hydo/history/hydo_hist.sh`, `hydo/VERSION` | the manifest format (`hydo guide manifest`), the history recorder `hydo build` stages for a manifest with `checks:`, and the version a plan's `hydo_version:` is checked against |
| `examples/redis/` | the Redis example, with its arm64 binaries under `files/` (works offline) |
| `examples/ledger-spin/` | the busy-wait example (`hydo/onboard/examples-mac/ledger-spin`), with `bin/spin_load` and `bin/spin_count` (static arm64, `hv/hvm/spin/spinvar.c`; `$HYDO_SPIN_BIN` when filling `assets/`) |
| `skill/hydo-onboard/` | the onboarding skill (Agent Skills format, any AI tool) that writes a project's `hydo/` folder; `hydo guide`, `hydo skill install` and `hydo init` read it (`bin/hydo` also carries a built-in copy) |
| `install.sh`, `QUICKSTART-mac.md`, `VERSION`, `SHA256SUMS` | `install.sh` copies the release into the prefix and runs `hydo setup` and `hydo doctor` there (Milestone 154); `hydo setup` checks every file of `SHA256SUMS` and the pins built into `hydo` (`assets.sha256` mapped to the release paths, and `guest-programs.sha256`), removes the quarantine attribute, signs `bin/hydo-hvm` and records the signed bytes in `.hydo-setup` |

The shell init and its extensions (`hydo-init.sh`, `hydo-init-*.sh`, the guest composer) are not
in a release any more: they were deleted in Milestone 136, when every image became an agent image.

In the Mac release mode, `hydo build` takes its guest tools from the release (each one can still
be replaced with its hydo-config variable, `hydo/crates/hydo-config/INVENTORY.md`): `guest/busybox`
(`HYDO_BUSYBOX_ARM64`), `guest/busybox-applets` (`HYDO_BUSYBOX`), `guest/mke2fs` (`HYDO_MKE2FS`),
`guest/hydo-agent` (`HYDO_AGENT_BIN_ARM64`), `guest/hydo-world` (`HYDO_WORLDBIN_ARM64`),
`guest/hydo-read` (`HYDO_READ_BIN_ARM64`), `guest/hydo_fc` (`HYDO_FCBIN_ARM64`) and the store tree
`guest/tc` (`HYDO_ARM64_STORE`).

## How `hydo build` works without Linux

`hydo-build --arch arm64` needs three things from a Linux build machine today: an arm64 busybox,
its applet list (asked from an x86 busybox, which cannot run on a Mac), and a pinned `mke2fs`
run under `libfaketime` for the data disk. The release provides the first two as files, and
`hydo build` makes the data disk differently (since Milestone 103 the Rust builder's Mac release
mode, `hydo_build::Release::Mac`):
- it starts a helper VM with `hydo-hvm hvm run`: the release kernel plus a two-file initramfs (busybox and
  the static arm64 `mke2fs`), with the empty data-disk file attached as the virtio disk;
- the guest runs `mke2fs` with exactly `hydo-build`'s options on `/dev/vda`, and the engine writes
  the disk back (`HVM_DISK_OUT`);
- `mke2fs` records whether `char` is signed on the machine that ran it (superblock `s_flags`:
  1 on x86, 2 on arm64); `hydo build` writes the x86 value, as a Linux build of the same
  manifest does. The filesystem is empty at that point, so nothing else depends on it.
No `faketime` is needed: the guest clock is deterministic, and `hydo-build` zeroes every time
field afterwards.

For network faults the arm64 builder copies the pinned aarch64 `tc` and the files its loader
opens (found with `hydo-elfdeps`, `hydo/crates/hydo-tools`) from the host's `/nix/store`. The release ships
those files under `guest/tc/`, and `hydo build` points the existence check and the copy at that
tree (the Rust builder's Mac release mode), so the image gets the
same paths, bytes and modes as a Linux build. The same tree holds `ip` and the iptables files
for service networks and the freeze (the builder reads them from `HYDO_ARM64_STORE`, which the
release sets to `guest/tc`).

**Checked:** for the Redis example the Mac-built data disk is byte-identical to the Linux-built
one (`9ddfc929…`), and every file of the root image, including the 23 files of the `tc`
closure, is identical to the Linux build except where the input differs: the embedded manifest
names `files/...` instead of `/nix/store/...`, and one library symlink is relative (see below).
Two Mac builds give the same bytes.

`root.nix:` (a Nix closure) still needs `nix-store`, so it is not supported on the Mac path; a
project lists its arm64 files under `root.files:` instead (hydo/dist/mac/QUICKSTART-mac.md, step 5).

## Building a release

```sh
HYDO_KERNEL=<Image-explore> HYDO_KERNEL_NETNS=<Image-explore-netns> HYDO_FC_BIN=<hydo_fc-arm64> \
  HYDO_SPIN_BIN=<dir> hydo/dist/mac/build-release.sh [OUTDIR]
```

The first run fills `hydo/dist/mac/assets/` (git-ignored), and every asset is checked against
`assets.sha256`:
- the two kernels from `$HYDO_KERNEL` and `$HYDO_KERNEL_NETNS` (`hv/hvm/explore/out/` on the
  Mac that built them; `Image-explore-netns` is built without Nix on any x86_64 Linux machine,
  docs/BUILD-PINS.md);
- the freeze helper from `$HYDO_FC_BIN`: `hydo/onboard/hydo_fc.c` compiled with the pinned Arm GNU
  Toolchain 13.3.Rel1 (`aarch64-none-linux-gnu-gcc -static -O2 -Wall`, docs/BUILD-PINS.md). The
  toolchain is an x86_64 Linux program; on a Mac it runs in an x86_64 Linux container (Milestone
  150: the toolchain archive checked by its sha256, unpacked inside the container, because the
  Mac's file system ignores case, and two compilations gave the pinned `499f654e…`);
- `ip` and the iptables files from the public Nix binary cache with `hydo-nix-fetch --closure`
  (built from the tree; it checks every archive's hash), then exactly the files `hydo-elfdeps
  --root` lists for them (`assets/netns/`, `assets/netns-closure.txt`); no Linux machine needed;
- busybox, `mke2fs`, the applet list, the `tc` closure and the Redis example's files from
  the build machine, which has these nixpkgs store paths (reached through the git-ignored
  `machines.local.sh`, see `machines.sh`).

After that the release builds offline in about a minute (most of it the Rust builds).
The Redis files are copied with `tar`, so the libraries' symlinks survive; the one absolute
symlink (`libgcc_s.so.1` into another store path) is made relative, because hydo-build checks
that each source file exists on the host. In the guest it resolves to the same file.

**Two runs give the same tarball.** The script sets every file's time to 0, clears extended
attributes, lists the files in sorted order (`gnutar` format, owner `root:wheel`, no extended
attributes, ACLs or file flags) and compresses with `gzip -n` (no name or time). The Rust Mac
programs are the same bytes when they are built in the same target directory; built in another
directory they differ only in the Mach-O `LC_UUID` (and the page hash of the ad-hoc signature
that covers it), because Apple's linker derives the UUID from the object files' paths.

Nothing is published by the script; the tarball stays in `OUTDIR`.

## The current tarball

`hydo-mac-0.1.0.tar.gz`, 29,925,257 bytes, sha256
`d6a274058ab6037d164c02ed3fe4ae73d8e5b960f85c079eab6628314329ef2e`, built twice on the Mac from
the final pre-release commit `ce6071c` (after M157 and the history pass; one tree path, clean
target directories) with the same bytes. Against the tarball built from `e744b8c` only
`SHA256SUMS`, `bin/hydo` (it carries MANIFEST.md) and `bin/hydo-build` (the compiled-in commit),
`hydo/onboard/MANIFEST.md` (M157's wording), and the Mach-O `LC_UUID` of the programs (another tree
path; `bin/hydo-fetch-image` and `bin/hydo-hvm` differ in only those bytes) differ. Installed into a
clean prefix it gave every Mac value of docs/REBASELINE.md 13.4, twice each (docs/HISTORY.md,
"Milestone 157", the addendum "Both tarballs rebuilt from the final commit").

## Built from `e744b8c`

`hydo-mac-0.1.0.tar.gz`, 29,926,313 bytes, sha256
`098c8773b110916f9b21a78a926ffc3c4ae9a47d3339120f38a467bb1b9d780e`, built twice on the Mac from
`main` = `e744b8c` (one tree path, clean target directories) with the same bytes. Against M155's
tarball only `SHA256SUMS`, the commit compiled into `bin/hydo` and `bin/hydo-build`, and the Mach-O
`LC_UUID` of `bin/hydo-fetch-image` and `bin/hydo-hvm` (another tree path) differ. Installed into a
clean prefix it gave every Mac value of docs/REBASELINE.md 13.4, twice each (docs/HISTORY.md,
"Milestone 156", the addendum "Both tarballs built from `e744b8c`").

## Milestone 155

`hydo-mac-0.1.0.tar.gz`, 29,926,376 bytes, sha256
`3ad2b456ed71473b1c153806d40e147a85487bdc83d4ce2f0aea97c567341a17`, built twice on the Mac from
M155's branch (`b662589`, one tree path, clean target directories) with the same bytes. Every image
it builds carries `/etc/hydo/image.json` (the guest kernel by sha256 and the fingerprint epoch) and
gets a build record `<image>.json` beside it; `hydo run` boots the kernel the image names. Installed
into a clean prefix: `setup` and `doctor` all checks passed; Redis `a3447614…` `disk=none | run`
`67e5d6469f1a90a5`, `disk=cut | run C=12 R=everysec` `a6870a92a1bbf0ae`, one-way loss
`ed3651af0bf0fb7e`, tick jitter `aad758f1a6d55892`; `hydo explore --seeds 1-20 --strategy swarm
--workers 2` 11 failures (minimal `0441476f5fd74d0b`, `d6120f56fdef3f28`), `repro hydo-out/explore`
13 of 13; ledger-spin `aa732142c0f5567f` and `3b4dd4910d2a6123`; each twice (fingerprint epoch 1;
docs/REBASELINE.md 13.4, docs/HISTORY.md "Milestone 155").

## Milestone 154: the tarball before

Built twice on this Mac from M154's branch (on `main` = `ba16a4b`) from clean target
directories, with the same bytes both times: `hydo-mac-0.1.0.tar.gz`, 29,829,740 bytes, sha256
`2cced36ce7657acbba805851b4636a6e71588ed96753b12585883d8da95b1072`. `install.sh` now copies the
release and runs `hydo setup` and `hydo doctor` (the checks and the signing are in `hydo`), and
`build-release.sh` compiles the four Mac programs with the tree and the Cargo home remapped to
`/hydo` and `/cargo` (both computed when it runs), so no program carries the building user's
home directory any more; `bin/hydo-build` also lost its compiled-in checkout path. Installed into
a clean prefix with the same PATH and stubs as below, it gave every M150 value of the release's
own examples, each schedule twice (docs/HISTORY.md, "Milestone 154").

## Milestone 153: the tarball before

Built twice on this Mac from `main` = `a4d9531` (after M151's wording changes) from clean target
directories, with the same bytes both times: `hydo-mac-0.1.0.tar.gz`, 29,786,366 bytes, sha256
`251e0bb3022b48bec0bcd3ecaef7e7bb2cbf9baf8515b44fd198350e71544539`. It differs from M150's
tarball in `hydo/onboard/MANIFEST.md` (M151's wording), in `bin/hydo` (which carries that file),
and in `bin/hydo-build` and `bin/hydo-hvm`, whose `LC_UUID` and source paths name the checkout
they were built in (see "Two runs give the same tarball"). Installed into a clean prefix with the
same PATH and traps as below, it gave every value of the M150 table: the bundled Redis and
ledger-spin examples, the exploration and the regressions bit for bit. The three examples built
from copies under the user's own directory (redis-composed, redis-bridge, the store reader) get
other root images and `fp=` values, with the same `ctl=`, because their `src:` paths name the
copy's directory and the manifest text is inside the image; built from copies at M150's directory
they give M150's images (`3a72431d…`, `37d0d5de…`, `4ec4923c…`) and every `fp=` of the table
(docs/HISTORY.md, "Milestone 153").

## Milestone 150: the release built and checked

Built from `main` at `85e3f4c` (with M149) plus this milestone's changes, twice from clean target
directories: the same tarball both times (`hydo-mac-0.1.0.tar.gz`; its size and sha256 are in
docs/HISTORY.md "Milestone 150"). Installed into a clean prefix and run with a PATH of the
prefix's `bin`, `/usr/bin`, `/bin`, `/usr/sbin` and `/sbin`, with traps for `python`,
`python3`, `pip`, `go`, `cargo`, `rustc`, `rustup`, `nix`, `cc`, `clang`, `gcc`, `make`,
`xcrun` and `docker` in front, which log every call and fail: nothing called them. `hydo doctor`: all checks
passed (0 failed, 0 warnings). Every schedule below ran twice with the same fingerprint (`fp=`)
and control-transcript hash (`ctl=`):

| example | schedule | verdict | fp | ctl |
|---|---|---|---|---|
| redis (image `e1e97c53…`, disk `9ddfc929…`) | `disk=none \| run` | OK, acked=20 all present | `76bce7dce084f41e` | `0b9e60d528cc2dc7` |
| redis | `disk=cut \| run C=12 R=everysec` | VIOLATION, key k:0 lost | `0f139f3fc3398595` | `c1c6baa6a88a32a5` |
| redis | `disk=none \| run O=redis@5-10:resp:loss,100%` | OK, acked=15 all present | `e0c65f9a0d99f99b` | `4f21d3b71f10701c` |
| redis | `disk=none tj=7 tjpm=500 tjmax=500 \| run` | OK, acked=20 (`tjk=9-20 tjn=5`) | `11f5e98285489afa` | `87ffd5e894a2c08e` |
| ledger-spin (image `c0746ab8…`) | `disk=none \| run` | OK, acked=30 each stored once (rewind mode) | `82eac6c01b9b1398` | `930f998155f77b29` |
| ledger-spin | `disk=none \| run P=front@10+2` | VIOLATION, entry 10 stored 2 times | `66cec8d81248181f` | `012f769bba1db408` |
| redis-composed (image `3a72431d…`) | `disk=none \| run W=0000002a,p4,q2,r100` | OK, 34 commands ok | `ea17ecba892046cb` | `64228edfdcc3de50` |
| redis-composed | `hydo repro --regressions` (`lost-update`) | PASS | `404f330457dca5cb` | |
| redis-bridge (image `37d0d5de…`, disk `26f4a507…`, `Image-explore-netns`) | `disk=none \| run` | OK, acked=16 | `61df76df4baf4b99` | `6a8e130bfbf9fef2` |
| redis-bridge | `disk=none \| run N=primary-replica@5-10:loss,100%` | OK, acked=16 (5 packets dropped each way) | `a1142c131ca4a1d0` | `bb902d00160d7286` |
| redis-bridge | `disk=none \| run F=replica<primary@5#3:2000` | OK, acked=16 (FROZE at resp #3, SET k:7) | `b8aa1f2c0cc32eee` | `db293b879f216430` |
| redis-bridge | `disk=cut \| run C=8` | OK, acked=8 | `6fd3c2db1a622c51` | `1ac708d19e5718f8` |
| redis, store reader (`examples-mac/redis/hydo-read.yaml`, image `4ec4923c…`) | `disk=none \| run` | OK, subset holds (20 of 20) | `2b92dd0393d345f1` | `8476030a9d80c61e` |
| redis, store reader | `disk=cut \| run C=12 R=everysec` | VIOLATION, 12 of 12 acknowledged items missing | `6e226296c6c172be` | `557e45d1698ad7cc` |

The Redis, ledger-spin and Redis power-cut values equal the agent-image values of Milestones 143
and 147. `hydo explore --seeds 1-20 --strategy swarm --workers 2` on Redis: 11 failures, all 11
reproduced (classes `lying-disk`, `unsafe-knob`, `io-errors`), minimal schedules
`disk=lying:3 dseed=eac0b527c5ac67d9 | run C=20` (`49ac9358094e92cb`) and
`disk=cut dseed=1f16055b6d41c6d9 | run C=9 R=no` (`bfd9875c8eb3c9ec`), each twice identical; a
second exploration gave the same 40 runs with the same fingerprints; `hydo repro` of the first
minimal schedule gave its fingerprint twice, and `hydo repro hydo-out/explore` reproduced 13 of
13. `hydo report`, `hydo repro <run dir>` (REPRODUCED), `hydo init`, `hydo guide`,
`hydo skill install --agent claude` and `hydo build hydo/hydo.yaml --check` worked as the
quickstart says. redis-composed, redis-bridge and the store reader are not shipped as examples:
they were built from copies of `hydo/onboard/examples-mac/` whose `src:` paths point at the
release's `examples/redis/files`.
