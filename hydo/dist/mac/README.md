# The Mac release of hydo

`build-release.sh` turns the Mac backend into a self-contained tarball that a user installs with
one command and runs with no Linux machine, no root and no other software beyond the macOS
Command Line Tools (for `cc`; since Milestone 107 nothing in the release uses Python). `hydo/dist/mac/QUICKSTART-mac.md` at the top of the repository is
the user guide; it is shipped inside the release.

## What is in a release

| Path | What |
|---|---|
| `bin/hydo`, `bin/hydo-build`, `bin/hydo-fetch-image` | the Rust programs (`aarch64-apple-darwin`, built by `build-release.sh` or taken from `$HYDO_RUST_BIN_DIR`): `hydo` is the one CLI (`hydo/crates/hydo-cli`; the same program as on Linux, with the same behaviour since Milestone 107), `hydo-build` the image builder with its own command line (for scripts), `hydo-fetch-image` what the `fetch-images.sh` of `hydo compose` calls. The Python CLI (`hydo/dist/mac/hydo`) and the Python behind it were deleted in Milestone 107 |
| `hv/hvm/hvm`, `hv/hvm/explore/hvmx` | the VMM and the explorer engine, re-signed by `install.sh` on the user's Mac |
| `hv/hvm/explore/out/Image-explore` | the arm64 kernel (`hv/hvm/explore/build-guest.sh`, HVC idle) |
| `guest/busybox`, `guest/mke2fs` | static arm64 busybox 1.36.1 and e2fsprogs 1.47.0 (aarch64 nixpkgs) |
| `guest/busybox-applets[.txt]` | the applet set to link, as the Linux build links it |
| `guest/tc/`, `guest/tc-closure.txt` | the pinned aarch64 `tc` (iproute2 6.8.0) and every file its loader opens, at their `/nix/store` paths, for network faults |
| `hydo/onboard/`, `hydo/history/hydo_hist.sh` | the guest-side files `hydo build` puts into images (the generic init and every extension, the test composer's guest runner, the history recorder) and the manifest format (`hydo guide manifest`) |
| `examples/redis/` | the Redis example, with its arm64 binaries under `files/` (works offline) |
| `examples/ledger-spin/` | the busy-wait example (`hydo/onboard/examples-mac/ledger-spin`), with `bin/spin_load` and `bin/spin_count` (static arm64, `hv/hvm/spin/spinvar.c`; `$HYDO_SPIN_BIN` when filling `assets/`) |
| `skill/hydo-onboard/` | the onboarding skill (Agent Skills format, any AI tool) that writes a project's `hydo/` folder; `hydo guide`, `hydo skill install` and `hydo init` read it (`bin/hydo` also carries a built-in copy) |
| `install.sh`, `hydo/dist/mac/QUICKSTART-mac.md`, `VERSION`, `SHA256SUMS` | |

## How `hydo build` works without Linux

`hydo-build --arch arm64` needs three things from a Linux build box today: an arm64 busybox,
its applet list (asked from an x86 busybox, which cannot run on a Mac), and a pinned `mke2fs`
run under `libfaketime` for the data disk. The release provides the first two as files, and
`hydo build` makes the data disk differently (since Milestone 103 the Rust builder's Mac release
mode, `hydo_build::Release::Mac`):
- it starts a helper VM with `hvm`: the release kernel plus a two-file initramfs (busybox and
  the static arm64 `mke2fs`), with the empty data-disk file attached as the virtio disk;
- the guest runs `mke2fs` with exactly `hydo-build`'s options on `/dev/vda`, and `hvm` writes
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
same paths, bytes and modes as a Linux build.

**Checked:** for the Redis example the Mac-built data disk is byte-identical to the Linux-built
one (`9ddfc929…`), and every file of the root image, including the 23 files of the `tc`
closure, is identical to the Linux build except where the input differs: the embedded manifest
names `files/...` instead of `/nix/store/...`, and one library symlink is relative (see below).
Two Mac builds give the same bytes.

`root.nix:` (a Nix closure) still needs `nix-store`, so it is not supported on the Mac path; a
project lists its arm64 files under `root.files:` instead (hydo/dist/mac/QUICKSTART-mac.md, step 5).

## Building a release

```sh
HYDO_KERNEL=<path to Image-explore> hydo/dist/mac/build-release.sh [OUTDIR]
```

The first run fills `hydo/dist/mac/assets/` (git-ignored): the kernel from `$HYDO_KERNEL`, and
busybox, `mke2fs`, the applet list and the Redis example's files over ssh from a Linux machine
with these nixpkgs store paths (`$HYDO_BUILD_HOST`, default the build machine). Every asset is
checked against `assets.sha256`. After that the release builds offline in a few seconds.
The Redis files are copied with `tar`, so the libraries' symlinks survive; the one absolute
symlink (`libgcc_s.so.1` into another store path) is made relative, because hydo-build checks
that each source file exists on the host. In the guest it resolves to the same file.

Nothing is published by the script; the tarball stays in `OUTDIR`.
