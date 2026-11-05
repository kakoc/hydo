# The Linux release of hydo

`build-release.sh` turns the bare-metal backend (the `hvx` kernel module and the Rust programs
`hydo`, `hydo-hvx` (the run engine as a program; since M137 it replaces `hvx_explore`), `hydo-build`,
`hydo-fetch-image` and `hydo-grpc-gen`) into a self-contained tarball for x86_64 Linux machines
with Intel VT-x. Every image it builds is an agent image (M136): the guest's first process is
`guest/hydo-agent` (the pinned 0.3.0, M140), and the host controller inside `hydo` drives it;
on hvx the agent and the host talk over the host-call port by default (`HYDO_HVX_HOSTCALL=0`
turns it off, M140). The configuration variables are listed in
`hydo/crates/hydo-config/INVENTORY.md` (M144). A user installs it with one command and drives everything with one `hydo` command; no
Nix, no `~/hydo` tree and no hand-written lock scripts are needed. `QUICKSTART-linux.md` is the
user guide; it is shipped as the release's `README.md`. The Mac counterpart is
`hydo/dist/mac/` (same verbs, same release pattern).

## The `hydo` command

`bin/hydo` is the Rust CLI (`hydo/crates/hydo-cli`), a static `x86_64-unknown-linux-musl`
binary; `bin/hydo-hvx` (the run engine as a program), `bin/hydo-build` (the image builder with
its own command line, for scripts), `bin/hydo-fetch-image` (what the `fetch-images.sh` of `hydo
compose` calls) and `bin/hydo-grpc-gen` (a shell wrapper that runs `libexec/hydo-grpc-gen`, the
gRPC driver generator, with `--client` defaulting to `guest/`, where the static client
`guest/hydo-grpc` is) come with it. `build-release.sh` builds them with the workspace's pinned
toolchain (the TLS code of `hydo-fetch-image` compiles some C: `musl-gcc` when present, else the
host's `cc`), or takes them from `$HYDO_RUST_BIN_DIR`. It also builds the guest programs from
this tree: `guest/hydo-agent` (`hydo/crates/hydo-agent/build.sh`, checked against its pin by
`hydo-build`), `guest/hydo-world`, `guest/hydo-read` (`hydo/crates/hydo-read/build.sh`; its
bytes depend on the kind of host that builds it, docs/BUILD-PINS.md) and `guest/hydo-grpc`
(`hydo/grpcgen/build-client.sh`, pinned `3926d137…`). Since Milestone 107 the release contains no Python and
needs none; the Python CLI (`hydo/dist/linux/hydo`) and every Python module behind it are
deleted (commit `2a58c4b` has them). The Mac release's `hydo` is the same program and behaves
the same (Milestone 107); only `hydo module` is Linux-only.

| verb | what it runs underneath |
|---|---|
| `hydo setup [--check \| --verify] [--add-to-path]` | (M154; `install.sh` ran it as shell before) checks every file of `SHA256SUMS` and the pins built into `hydo` (`assets.sha256` mapped to the release paths, and `guest-programs.sha256`: `guest/hydo-agent`, `hydo-world`, `hydo-read`, `hydo-grpc`), unpacks `guest/store.tar` into `guest/store` with `tar -xpf` and deletes the tar, makes `bin/*` and `libexec/*` executable, and writes `.hydo-setup`, the record of the unpacked tree (every entry's kind, mode, sha256 or link target), against which later checks compare it. A damaged file is refused, never repaired. `--check` only reports (0 nothing to do, 1 damaged, 3 steps left); `--verify` checks the files only (`install.sh` runs it on the unpacked release before it replaces an earlier installation); `--add-to-path` adds `export PATH=…` to `~/.zshrc` for zsh, else `~/.bashrc` |
| `hydo doctor` | checks: Intel CPU, `vmx` flag (VT-x on in firmware), bare metal, a usable PMU (non-hybrid, `arch_perfmon`; otherwise it says the exit-count clock is used), `cc`/`make`/`flock`/`xz`/`sudo`/`modinfo`, password-less sudo, kernel headers, lockdown, the module (built, loaded, same `srcversion` as this release), `kvm_intel` not loaded, release files, the run engine, the bundled `mke2fs`, what `hydo setup --check` would fix (M154), disk space |
| `hydo module install\|uninstall [--name N] [--dry-run]` | (M154) `install`: `build` when the release's build is missing, built from other sources or for another kernel (`modinfo -F vermagic`), then `load`; nothing when this release's build is loaded. `uninstall`: `unload`, then delete `module/build/N` (a udev rule stays). `install.sh` runs `hydo module install` unless `--no-module` |
| `hydo module build [--name N]` | `make -C /lib/modules/$(uname -r)/build` on `module/hvx.c`; with `--name` a side copy `N.ko` with device `/dev/N` (`-DHVX_DEVNAME`, the `hv/hvx/tdet/build-hvxtd.sh` pattern) |
| `hydo module load\|unload [--name N] [--dry-run] [--param K=V]` | under `flock -x` on the module lock: `rmmod hvx`, `rmmod kvm_intel kvm` if loaded, `insmod`. A side module gets its own lock file (`<lock>.N`) and never touches `hvx` or KVM |
| `hydo module udev install\|remove\|status [--name N] [--group G] [--dry-run]` | optional: with sudo, writes `/etc/udev/rules.d/70-hydo-N.rules` (`KERNEL=="N", SUBSYSTEM=="misc", GROUP="G", MODE="0660"`), creates the group if missing, adds the user, reloads udev and applies the rule to a loaded device; `remove` deletes the rule and sets the device back to root:root 0600. `install.sh --udev [--udev-group G] [--udev-name N]` runs `install` |
| `hydo module status [--name N]` | loaded or not, `srcversion` of the loaded and the release-built module, device mode, KVM, how many processes hold the lock (read from `/proc/locks`, no lock taken) |
| `hydo build [hydo.yaml]` | the Rust image builder (`hydo/crates/hydo-build`, since Milestone 103) with the bundled tools, into `./hydo-out` (`./.hydo` for a project's `hydo/hydo.yaml`, the default manifest) |
| `hydo run [IMG] "<schedule>"` | the host controller drives the agent image (`/etc/hydo/control`) through the run engine (`hydo/crates/hydo-hvx`): on a thread of `hydo`, holding the module lock shared, when the user can open `/dev/hvx`; otherwise as `libexec/hvx-run sudo env … bin/hydo-hvx [--ram-mib/--cpu from the image's manifest] one --control boot guest/bzImage guest/initramfs-kafka.cpio.gz IMG <dir> "<schedule>"` (M137); writes `result.json` and the control transcripts |
| `hydo repro <run dir \| explore dir \| "<schedule>">` | re-runs a `result.json`, or every failure and minimal schedule of an exploration's `exploration.json`, from a fresh boot, and compares fingerprints |
| `hydo explore [IMG] [explorer options]` | the explorer (`hydo/crates/hydo-explore`) in the same process (default `--seeds 1-20 --out hydo-out/explore`; workers sized from the machine) |
| `hydo report [DIR]` | prints `report.md` |

**sudo only when needed.** `hydo run` and `hydo explore` call `libexec/hvx-run sudo env …`;
`hvx-run` drops the `sudo` when the user can read and write the device (`$HVX_DEV`, default
`/dev/hvx`), which the optional udev rule above allows, and passes `HVX_DEV` through `sudo`
when it keeps it. `HYDO_SUDO=1` keeps sudo. `hydo doctor` reports which case applies. The
engine keeps its snapshots in memory (no snapshot directory since M144). The rule's meaning:
anyone in the group can run guests through the kernel module, which was not hardened against
hostile users. As root (or with the udev rule) the engine runs inside `hydo` and takes the
module lock itself; both ways give the same fingerprints (checked in M149).

**Side modules.** With `HVX_DEV=/dev/N`, runs use the side module `N` (built and loaded with
`--name N`) and hold its lock file (`<lock>.N`) instead of the main one.

**The module lock** is the bare-metal machine's convention: `~/bin/hvx-run` = `flock -s ~/.hvx-module.lock`,
`~/bin/hvx-reload` = `flock -x` on the same file. The release's `libexec/hvx-run` takes
`$HYDO_LOCK` (default `~/.hvx-module.lock`), so on the bare-metal machine release runs and the
existing tools share one lock.

**Changes to existing tools** (defaults unchanged, so other users of the tree see no difference):
the explorer reads its build directory and lock wrapper from `HYDO_BUILD` and `HYDO_HVX_RUN`
(defaults `~/hydo/build`, `~/bin/hvx-run`; every variable is listed in
`hydo/crates/hydo-config/INVENTORY.md`), and `hydo explore` gives it the release's paths. The
release's engine is built without coverage decoding (the cargo feature `pt` and libipt are not
used), so Intel PT traces are not decoded by it.

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
  unpacked by `hydo setup`) and `guest/store-closures.txt` (`nix-store -qR` of each root). The
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

- M149, agent images, the release built from `main` after M148 and installed into clean prefixes on
  the bare-metal machine (no Nix used, owners normalised): the repository's own
  `examples/redis/hydo.yaml` (with its `~/hydo/build` paths given by a fake home) gives
  `bde714a7…`, `redis-bridge` `490d7240…`, `redis-composed` `39d9e74f…` and
  `redis-composed-read` (which stages `guest/hydo-read`) `ecb7f1e0…`, each byte for byte the
  image M140 built with the tree's `hydo` and the M107 release's tools.

`root.nix:` still needs `nix-store` on the build machine (the closure is resolved there).

## Building a release

```sh
hydo/dist/linux/build-release.sh [OUTDIR]          # -> OUTDIR/hydo-linux-<version>/ and .tar.gz (default hydo/dist/linux/out)
```

The first run fills `hydo/dist/linux/assets/` (git-ignored) from the build machine
(reached through the git-ignored `machines.local.sh`, see `machines.sh`): the kernel and initramfs from
`~/hydo/build`, every Nix store path named in the script (all gc-rooted there, see
docs/BUILD-PINS.md), and `hvx_seedrng`/`hydo_fc` compiled there with the pinned musl gcc
(`hvx_seedrng` comes out as the pinned `c2a440c5…`). `HYDO_ASSETS_FROM=DIR` copies the same
files from a local directory instead (the bare-metal machine keeps a matching copy in
`~/hydo-layout-assets`; M149 built from it, since the build machine is off limits). Every asset
is checked against `assets.sha256`; `HYDO_PIN=1` writes that file when it is missing (a
maintainer, once). After that the release builds offline (about 4 minutes on the bare-metal
test machine from clean Rust targets, most of it compiling). The tarball is about 59 MB.
Nothing is published by the script.

The tarball is reproducible with GNU tar: file times are set to 1600000000, entries are sorted,
owners are root:0 and gzip writes no name or time stamp, and the umask is 022. M149 built it
twice on the bare-metal machine from clean Rust targets (`target/`, the agent's and
hydo-read's own target directories removed in between): both tarballs had the same sha256. On
a Mac, BSD tar keeps the owner but not the file times, so a Mac-built tarball is not
reproducible (and its `hydo-read` bytes differ: they depend on the build host).

Since M153 the five host programs are compiled with the tree and the Cargo home remapped to
`/hydo` and `/cargo` (`--remap-path-prefix`, as the guest programs' builds already did), so
their panic messages name no directory of the machine that built them; before, they carried
the builder's home directory. This changes only those programs' bytes, no image.

**The current tarball (M154)**, built twice on the bare-metal machine from M154's branch (on
`main` = `ba16a4b`), from clean Rust targets, with the same bytes both times:
`hydo-linux-0.1.0.tar.gz`, 59,463,651 bytes, sha256
`71cfd64678346de8b14a7d4fcd371d7afcec5ed28b3395c3f36b8abc7e13bec0`. Installed with
`install.sh --no-module` into a clean prefix, it gave every value of M149 and M153, as the user
and as root (docs/HISTORY.md, "Milestone 154"). It differs from M153's tarball in `install.sh`
(it only copies and runs `hydo`), the release's README (the install section), `bin/hydo`
(`hydo setup`, the doctor's installation check, `hydo module install|uninstall`) and
`bin/hydo-build`, which no longer carries the building checkout's path (`hydo-manifest`'s
compiled-in `CARGO_MANIFEST_DIR`, missed by M153's scan).

**The tarball of M153**, built twice on the bare-metal machine from `main` = `a4d9531`
plus M153's script change, from clean Rust targets, with the same bytes both times:
`hydo-linux-0.1.0.tar.gz`, 59,414,879 bytes, sha256
`a257f54b4e02fdf514ff084f0db2baaece6a6e96ab32ca6b44baa7cffd2fa6c9` (96 files). Installed with
`install.sh --no-module` into a clean prefix, it gave every value of M149 (docs/HISTORY.md,
"Milestone 153"). It differs from M149's tarball in the release's README, MANIFEST.md, the
comments of `libexec/faketime` and `libexec/hvx-run` (M151's wording), and in `bin/hydo`
(which carries MANIFEST.md), `bin/hydo-build`, `bin/hydo-fetch-image` and
`libexec/hydo-grpc-gen` (the remapped paths).

Only the kernel module is compiled on the user's machine, by `hydo module install` (it must
match the running kernel), which `install.sh` runs unless `--no-module`. Since M154 `install.sh`
only copies the unpacked release into the prefix and runs `hydo` commands (`setup --verify` on
the unpacked release, then `setup`, `module install`, `module udev install` with `--udev`, and
`doctor`); every check and step it did in shell before is in `hydo/crates/hydo-cli/src/setup.rs`
and `src/module.rs`, so other ways of installing (a package manager, a download into a cache)
can run the same steps.

## Not done

- One release covers x86_64 + Intel only; AMD (SVM) is not supported by hvx.
- The guest kernel is the one 6.6.35 bzImage the bare-metal machine uses; a different guest kernel needs its
  own modules in `guest/modules/`.
