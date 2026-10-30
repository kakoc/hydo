# The Linux release of hydo

`build-release.sh` turns the bare-metal backend (the `hvx` kernel module, `hvx_explore`, and the
Rust programs `hydo`, `hydo-build` and `hydo-fetch-image`) into a self-contained tarball for x86_64 Linux machines with Intel
VT-x. A user installs it with one command and drives everything with one `hydo` command; no
Nix, no `~/hydo` tree and no hand-written lock scripts are needed. `QUICKSTART-linux.md` is the
user guide; it is shipped as the release's `README.md`. The Mac counterpart is
`hydo/dist/mac/` (same verbs, same release pattern).

## The `hydo` command

`bin/hydo` is the Rust CLI (`hydo/crates/hydo-cli`), a static `x86_64-unknown-linux-musl`
binary; `bin/hydo-build` (the image builder with its own command line, for scripts) and
`bin/hydo-fetch-image` (what the `fetch-images.sh` of `hydo compose` calls) come with it.
`build-release.sh` builds the three with the workspace's pinned toolchain (the TLS code of
`hydo-fetch-image` compiles some C: `musl-gcc` when present, else the host's `cc`), or takes
them from `$HYDO_RUST_BIN_DIR`. Since Milestone 107 the release contains no Python and
needs none; the Python CLI (`hydo/dist/linux/hydo`) and every Python module behind it are
deleted (commit `2a58c4b` has them). The Mac release's `hydo` is the same program and behaves
the same (Milestone 107); only `hydo module` is Linux-only.

| verb | what it runs underneath |
|---|---|
| `hydo doctor` | checks: Intel CPU, `vmx` flag (VT-x on in firmware), bare metal, a usable PMU (non-hybrid, `arch_perfmon`; otherwise it says the exit-count clock is used), `cc`/`make`/`flock`/`xz`/`sudo`/`modinfo`, password-less sudo, kernel headers, lockdown, the module (built, loaded, same `srcversion` as this release), `kvm_intel` not loaded, release files, the run engine, the bundled `mke2fs`, disk space |
| `hydo module build [--name N]` | `make -C /lib/modules/$(uname -r)/build` on `module/hvx.c`; with `--name` a side copy `N.ko` with device `/dev/N` (`-DHVX_DEVNAME`, the `hv/hvx/tdet/build-hvxtd.sh` pattern) |
| `hydo module load\|unload [--name N] [--dry-run] [--param K=V]` | under `flock -x` on the module lock: `rmmod hvx`, `rmmod kvm_intel kvm` if loaded, `insmod`. A side module gets its own lock file (`<lock>.N`) and never touches `hvx` or KVM |
| `hydo module udev install\|remove\|status [--name N] [--group G] [--dry-run]` | optional: with sudo, writes `/etc/udev/rules.d/70-hydo-N.rules` (`KERNEL=="N", SUBSYSTEM=="misc", GROUP="G", MODE="0660"`), creates the group if missing, adds the user, reloads udev and applies the rule to a loaded device; `remove` deletes the rule and sets the device back to root:root 0600. `install.sh --udev [--udev-group G] [--udev-name N]` runs `install` |
| `hydo module status [--name N]` | loaded or not, `srcversion` of the loaded and the release-built module, device mode, KVM, how many processes hold the lock (read from `/proc/locks`, no lock taken) |
| `hydo build [hydo.yaml]` | the Rust image builder (`hydo/crates/hydo-build`, since Milestone 103) with the bundled tools, into `./hydo-out` (`./.hydo` for a project's `hydo/hydo.yaml`, the default manifest) |
| `hydo run [IMG] "<schedule>"` | `libexec/hvx-run sudo env HVX_SNAP_DIR=… hv/hvx/hvx_explore [--ram-mib/--cpu from the image's manifest] one guest/bzImage guest/initramfs-kafka.cpio.gz IMG <dir> "<schedule>"`; writes `run.json` |
| `hydo repro <run dir \| explore dir \| "<schedule>">` | re-runs a `run.json`, or every failure and minimal schedule of an exploration's `report.json`, from a fresh boot, and compares fingerprints |
| `hydo explore [IMG] [explorer options]` | the explorer (`hydo/crates/hydo-explore`) in the same process (default `--seeds 1-20 --out hydo-out/explore`; workers sized from the machine) |
| `hydo report [DIR]` | prints `report.md` |

**sudo only when needed.** `hydo run` and `hydo explore` call `libexec/hvx-run sudo env …`;
`hvx-run` drops the `sudo` when the user can read and write the device (`$HVX_DEV`, default
`/dev/hvx`), which the optional udev rule above allows, and passes `HVX_DEV` through `sudo`
when it keeps it. `HYDO_SUDO=1` keeps sudo. `hydo doctor` reports which case applies. A run
without sudo uses `/var/tmp/hvx-snap-<uid>` when `/var/tmp/hvx-snap` exists but is not
writable (earlier sudo runs made it root's). The rule's meaning: anyone in the group can run
guests through the kernel module, which was not hardened against hostile users.

**Side modules.** With `HVX_DEV=/dev/N`, runs use the side module `N` (built and loaded with
`--name N`) and hold its lock file (`<lock>.N`) instead of the main one.

**The module lock** is the bare-metal machine's convention: `~/bin/hvx-run` = `flock -s ~/.hvx-module.lock`,
`~/bin/hvx-reload` = `flock -x` on the same file. The release's `libexec/hvx-run` takes
`$HYDO_LOCK` (default `~/.hvx-module.lock`), so on the bare-metal machine release runs and the
existing tools share one lock.

**Changes to existing tools** (defaults unchanged, so other users of the tree see no difference):
the explorer reads its build directory, lock wrapper and snapshot directory from `HYDO_BUILD`,
`HYDO_HVX_RUN`, `HYDO_SNAP_DIR` (defaults `~/hydo/build`, `~/bin/hvx-run`, `/var/tmp/hvx-snap`),
and with `HYDO_CLI` set the report's hand-reproduction line is a `hydo run` command;
`explore/ptedges.h` compiles without libipt under `-DHYDO_NO_LIBIPT` (coverage decoding off),
so the run engine builds on a machine without `libipt-dev`.

## How `hydo build` works without Nix

`hydo-build` (x86_64) needs, from the Nix build machine: a static busybox, `hvx_seedrng`, the pinned
e2fsprogs `mke2fs` run under the pinned `libfaketime`, for network faults the iproute2 closure
(`tc` inside the guest) and the guest kernel's `sch_prio`/`sch_netem`/`cls_u32` modules, and
for the freeze fault the iptables closure, `hydo_fc` and the netfilter modules. The release
ships all of them (`guest/`, `tools/`), and `hydo build` points `hydo-build` at them:
- the `HYDO_*` tool variables name the bundled files (`HYDO_FAKETIME` = `libexec/faketime`);
- `libexec/faketime` runs the bundled Nix-built `mke2fs`/`debugfs` with the bundled glibc loader
  (`ld-linux-x86-64.so.2 --library-path tools/lib --preload libfaketime.so.1`), so no
  `/nix/store` is needed. The clock is frozen at 1600000000 (`FAKETIME` without `@`, and
  `TZ=UTC-4`: hydo-build's `FAKE_TS` string is that instant only in the build machine's zone,
  UTC+4; hydo-build itself also sets this zone, its `FAKE_TZ`, on every faketime call,
  so a plain `hydo-build` no longer depends on the machine's zone either), and `MKE2FS_CONFIG` names no file, so `mke2fs` uses its built-in defaults as on
  the build machine instead of the machine's `/etc/mke2fs.conf`;
- the guest-side closures are `guest/store.tar` (a GNU tar with the store's read-only modes,
  unpacked by `install.sh`) and `guest/store-closures.txt` (`nix-store -qR` of each root). The
  builder's Linux release mode (`hydo_build::Release::Linux`) resolves the pinned `/nix/store`
  paths into that tree, only when the machine does not have them;
- the umask is 022, and after the root image is made every inode's owner is set to 0:0 with the
  bundled `debugfs` (same frozen clock), so the image does not depend on the building user's
  uid/gid (`HYDO_KEEP_OWNERS=1` skips this).

**Checked** (Redis example, `hydo/onboard/examples/redis/hydo.yaml` and `hydo-netem.yaml`):
- with `HYDO_KEEP_OWNERS=1`, the release on the NixOS build machine builds images byte-identical to
  a plain `hydo-build` with the pinned tools (root `bcea6b1d…`/data `2d52cd04…`; netem root
  `6d768c1c…`/data `6b1dab4d…`);
- with owners normalised, the release on the Ubuntu bare-metal machine (no Nix, UTC, umask 002,
  Ubuntu's `/etc/mke2fs.conf`) builds the same bytes as on the NixOS build machine (UTC+4, no
  `mke2fs.conf`): root `aec24305…`, netem root `b0fe5081…`. Three things had to be pinned
  for that: the time zone of the frozen clock, `MKE2FS_CONFIG`, and the umask.

`root.nix:` still needs `nix-store` on the build machine (the closure is resolved there).

## Building a release

```sh
hydo/dist/linux/build-release.sh [OUTDIR]          # -> OUTDIR/hydo-linux-<version>/ and .tar.gz (default hydo/dist/linux/out)
```

The first run fills `hydo/dist/linux/assets/` (git-ignored) from the build machine
`$HYDO_BUILD_HOST` (default the build machine): the kernel and initramfs from
`~/hydo/build`, every Nix store path named in the script (all gc-rooted there, see
docs/BUILD-PINS.md), and `hvx_seedrng`/`hydo_fc` compiled there with the pinned musl gcc
(`hvx_seedrng` comes out as the pinned `c2a440c5…`). Every asset is checked against
`assets.sha256`; `HYDO_PIN=1` writes that file when it is missing (a maintainer, once). After
that the release builds offline in seconds on the Mac or any Linux machine. The tarball is
about 45 MB. Nothing is published by the script.

The kernel module and the run engine are compiled on the user's machine by `install.sh` (they
must match the running kernel and the release's `hvx_abi.h`).

## Not done

- One release covers x86_64 + Intel only; AMD (SVM) is not supported by hvx.
- The guest kernel is the one 6.6.35 bzImage the bare-metal machine uses; a different guest kernel needs its
  own modules in `guest/modules/`.
