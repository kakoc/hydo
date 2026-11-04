# Pinned nix store paths for the image builds (the build machine)

Written 2026-10-21. The image scripts copy tools and binaries out of exact `/nix/store` paths
so that a rebuild is byte-identical. When the build machine garbage-collected some of them (noticed at
Milestone 46), builds fell back to `HYDO_GCC_MUSL` (a wrapper that copied a prebuilt
`hvx_seedrng`) and `HYDO_REDIS_OUT` (prebuilt Redis binaries). This file lists every pin,
says what was restored and how, and records the proof that from-scratch builds work again.

## Inventory

Pins referenced by `build-linux.sh`, `build-stack.sh`, `build-kafka.sh`, `build-mysql.sh`,
and the image builder `hydo/crates/hydo-build` (`src/x86.rs`, `src/arm64.rs`; until M103 the Python
`hydo/onboard/hydo-build` and `hydo_build_arm64.py`) (store hash shortened to 8
characters; the full paths are in the scripts).

| store path | used by | state on 2026-10-21 | action |
|---|---|---|---|
| `aa1jsl19…-gcc-static-x86_64-unknown-linux-musl-13.2.0` | linux, stack, kafka, mysql (static C helpers) | **missing, not in cache** | rebuilt from its derivation |
| `ky1xq6xa…-redis-static-x86_64-unknown-linux-musl-7.2.4` | linux §2h, stack, onboard examples (via `build/redis-*`) | **missing, not in cache** | rebuilt from its derivation |
| `6c7pf5c2…-sqlite-static-x86_64-unknown-linux-musl-3.45.3-bin` | linux §2g | **missing** | substituted from cache.nixos.org |
| `0g011k1p…-openjdk-minimal-jre-17.0.7+7` | linux §2k | **missing** | substituted from cache.nixos.org |
| `0g011k1p…-openjdk-minimal-jre-17.0.7+9` | linux §2k (first choice) | does not exist anywhere | none: same hash as the `+7` path with a different name, so it can never be valid; the script falls back to `+7` |
| `14bkh67g…-python3-3.11.9-env` | linux, kafka, mysql, stack (`PY3`) | **missing, cannot be restored** | see "Not restorable" |
| `0nlqfwsn…-musl-static-x86_64-unknown-linux-musl-1.2.6` | static C helpers (`-B` crt/libc) | present | gc-rooted |
| `ahw5fkq0…-busybox-static-x86_64-unknown-linux-musl-1.37.0` | all images | present | gc-rooted |
| `24s0phlx…-e2fsprogs-1.47.0-bin` | all images (`mke2fs`) | present | gc-rooted |
| `fs0gp7zy…-libfaketime-0.9.10` | all images | present | gc-rooted |
| `aw6ljbiw…-mysql-8.4.10` | mysql, stack, onboard stack | present | gc-rooted |
| `vjcrra86…-openjdk-headless-17.0.7+7` | linux §2k/§2l, stack, onboard stack | present | gc-rooted |
| `prk07apd…-apache-kafka-2.13-3.5.0` | linux §2l, stack, onboard stack | present | gc-rooted |
| `pwj3x44p…-go-1.26.5`, `xrgn6m0c…-go-1.26.5` | stack, linux §2m/§2n | present | gc-rooted |
| `dx4fsbxs…-iproute2-6.8.0` | stack, hydo-build (`tc`) | present | gc-rooted |
| `qmhi9c99…-iproute2-6.8.0` (aarch64) | hydo-build (`--arch arm64`, `tc`) | present | gc-rooted |
| `zbc5fx2y…-busybox-static-aarch64-unknown-linux-musl-1.36.1` | hydo-build (`--arch arm64`) | present | gc-rooted |
| `f6c0sf4f…-nodejs-slim-22.2.0` (x86_64) | `hydo/onboard/examples/node-kv/hydo.yaml` (`root.nix`) | substituted from cache.nixos.org on 2026-10-24 | gc-rooted (`~/hydo-gcroots/nodejs-slim-22.2.0`) |
| `vh315js3…-nodejs-slim-22.2.0` (aarch64) | `hydo/onboard/examples/node-kv/fetch-node-arm64.sh` (Mac example) | substituted from cache.nixos.org on 2026-10-24 | gc-rooted (`~/hydo-gcroots/nodejs-slim-22.2.0-aarch64`) |

## How the two missing derivations were found and rebuilt

Both are absent from cache.nixos.org (the gcc is a statically linked native musl compiler;
the Redis has an `overrideAttrs` that skips its tests), and no `.drv` for them was left in the
store. Both come from the nixpkgs the build machine's system flake points at: NixOS 24.05,
revision `89c4987` (`/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source`). Evaluating there
gives the exact pinned output paths:

```
N=/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source
nix-instantiate --eval -E "with import $N {}; pkgsStatic.gcc.cc.outPath"
  # -> /nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0
nix-instantiate --eval -E "with import $N {}; (pkgsStatic.redis.overrideAttrs (o: { doCheck = false; doInstallCheck = false; })).outPath"
  # -> /nix/store/ky1xq6xa88qhwwagnh4anm4sjl3c7vyk-redis-static-x86_64-unknown-linux-musl-7.2.4
```

Derivations: gcc `/nix/store/5v8gvjag59vfvvhvh59vrn12zz3v7zsw-gcc-static-x86_64-unknown-linux-musl-13.2.0.drv`,
Redis `/nix/store/l7p6dxcrcjcc48yc0n7fjj660k6srhk1-redis-static-x86_64-unknown-linux-musl-7.2.4.drv`.
They were built as the normal user through the nix daemon (no root, no configuration change),
a few minutes each on 24 cores; the build dependencies came from the binary cache:

```
nix-store --add-root ~/hydo-gcroots/redis-static-7.2.4 --indirect -r <redis .drv>
nix-store --add-root ~/hydo-gcroots/gcc-static-musl-13.2.0 --indirect -r <gcc .drv>
```

A rebuilt input-addressed path has the right name but is only proven to have the right bytes
by its outputs. The checks below show it does: the rebuilt Redis binaries are identical to the
August copies in `~/hydo/build`, and the rebuilt gcc produces `hydokv`, `hydoapp` and
`hvx_seedrng` byte-identical to the August builds.

The two cache paths: `nix-store --add-root ~/hydo-gcroots/<name> --indirect -r <path>`.

## Not restorable: `python3-3.11.9-env` (`14bkh67g…`)

No derivation on the build machine produces it, and the obvious `python3.withPackages` expressions in
the 24.05 nixpkgs give other hashes, so it was an ad-hoc environment. It only runs stdlib
scripts that zero timestamps in the ext2/ext4 images, so the interpreter does not affect the
image bytes; every script already falls back to the first `/nix/store/*python3*/bin/python3`.
All images in the proof below were built with that fallback and matched.
**Proposed pin update (not made):** point `PY3` at the plain interpreter of the same nixpkgs,
`/nix/store/4rf5qybw37b4lh1g0xczlv14sqdbmnpm-python3-3.11.9/bin/python3` (in the system
closure, and in the cache), in `build-linux.sh`, `build-kafka.sh`, `build-mysql.sh` and
`build-stack.sh` (the last is being edited in another session, so it was left alone).

## Garbage-collection roots

All pins that exist are now indirect gc roots in **`~/hydo-gcroots/`** on the build machine (one
symlink per path, registered under `/nix/var/nix/gcroots/auto`; `nix-store --gc --print-roots`
lists 22). Also rooted there: the gcc and Redis `.drv` files (`drv-*`, so the recipe survives)
and the nixpkgs source they come from (`nixpkgs-24.05-89c4987-source`). To add a new pin:
`nix-store --add-root ~/hydo-gcroots/<name> --indirect -r <store path>`. Deleting a symlink
there releases the path for the next GC.

## Proof

Everything was built in `~/hydo-pins-proof/hydo` (a copy of this tree) with `HYDO_GCC_MUSL`,
`HYDO_REDIS_OUT`, `HYDO_MUSL_ROOT` unset, and `HOME` set to `~/hydo-pins-proof`, so the
manifests' `~/hydo/build/...` sources resolved inside the proof directory. Steps:
`head -n 1258 build-linux.sh` (everything up to and including §2j, the unmodified script) run
in the proof tree, then `hv/hvx/images/pins-proof.sh` (the Redis and stack examples through `hydo-build`).

**`build-linux.sh` up to §2j: every artifact identical to the recorded/August builds**

| artifact | sha256 (proof) | recorded / existing copy |
|---|---|---|
| `redis-server`, `redis-cli` | `e66c3fd0…`, `7ecd56f4…` | identical to `~/hydo/build` (2026-08-15) |
| `hydokv` | `91477ce1…` | identical |
| `hydoapp` | `63850b54…` | identical; M30 records `63850b54…` |
| `sqlite3` | `eefc7151…` | identical |
| `initramfs-redis.cpio.gz` | `619f0a63…` | identical |
| `redis-disk.img` | `64c7e3f5…` | identical; recorded `64c7e3f5…` |
| `initramfs-system.cpio.gz` | `dd6ce66e…` | identical; M30 records `dd6ce66e…` |
| `system-disk.img` | `b5b949f6…` | identical; M30 records `b5b949f6…` |
| `initramfs-kv.cpio.gz`, `initramfs-sql.cpio.gz`, `sql-disk.img` | `8540cd7c…`, `1d91e4f6…`, `b3b2b9c0…` | identical (`sql-disk.img` recorded `b3b2b9c0…`) |
| `rootfs-ext4.img`, `rootfs-disk.img` | `6df9f82c…`, `a4aefaf1…` | recorded at M29 |
| `hvx_seedrng` | `c2a440c5…` | identical to the August copy inside `build/initramfs-redis/bin/`; **differs** from `build/hvx_seedrng` (`01adf1bd…`, 2026-10-17), see below |

`initramfs-redis` and `initramfs-system` contain `hvx_seedrng`, the static Redis and `hydoapp`,
so the restored gcc and Redis reproduce the multi-service images byte for byte.

**The onboarding examples, and why their recorded values do not match**

| image | pinned tools | recorded | with the 2026-10-17 `hvx_seedrng` (`01adf1bd…`) |
|---|---|---|---|
| `examples/redis` disk | `4eda84fe75909c21…` | `0dd509c2…` | `0dd509c29fff89f190efdb76f434dd11662ed11344240706a9dc6c8e8399830a` = recorded |
| `examples/stack` (FRONT=fix) disk | `ac5553ead6d49fef…` | `37a2ff6a…` | `37a2ff6acd4778ec72d06a9a5c93f1cea935be1138585449e461868a32b09b19` = recorded |

The only difference is `hvx_seedrng`. The copy in `~/hydo/build/hvx_seedrng` (`01adf1bd…`,
built 2026-10-17 11:14, the day the pins were noticed missing) was NOT made by the pinned
toolchain: with the pinned gcc and musl 1.2.6 the source gives `c2a440c5…` (text 24705
bytes); `01adf1bd…` has 18252 bytes of text, which matches linking against musl 1.2.3, but
no gcc/musl combination on the build machine reproduces it, so its toolchain is unknown. Every
`hydo-build` image and every image built with the `HYDO_GCC_MUSL` stand-in since then carries
it; that presumably includes the M47/M52/M56 onboarding and explorer images (not checked one by one). A copy is kept at
`~/hydo-pins-proof/hvx_seedrng.standin-01adf1bd`.

**Decision needed.** Either (a) re-baseline: rebuild the post-2026-10-17 images with the
pinned tools and re-record their sha256 and fingerprints (the Redis example becomes
`4eda84fe…`, the stack example `ac5553ea…`), or (b) keep the old values by pinning the
stand-in binary itself (commit it, or point `HYDO_SEEDRNG` / the scripts at a saved copy).
(a) restores "built from pinned tools"; (b) keeps the existing baselines but leaves one
binary whose build cannot be reproduced.

## Nix on the bare-metal machine (Milestone 140)

The build machine is off limits since 2026-10-30, so in Milestone 140 (user's approval of
2026-11-02) the bare-metal machine got Nix and became the build machine for the images that need the pinned
store paths. Everything below is in the session's own directory `~/hydo-nixb-work` on the bare-metal machine;
nothing is in a shell profile.

- **Nix:** 2.18.9, single-user, `/nix` owned by `ubuntu` (made with `sudo`), installed with the
  official installer (`releases.nixos.org/nix/nix-2.18.9/install`, sha256 `5e887496…`) and
  `--no-daemon --no-modify-profile --no-channel-add`. To use it:
  `. ~/hydo-nixb-work/nixenv.sh` (puts `~/.nix-profile/bin` first on PATH, sets
  `NIX_CONFIG` with cache.nixos.org as the only substituter, and `NIXPKGS`). Builds run without
  the sandbox: Ubuntu 24.04 does not let a user create the namespaces it needs
  (`kernel.apparmor_restrict_unprivileged_userns = 1`), and Nix falls back. The proofs below show
  that the outputs are the build machine's anyway.
- **nixpkgs:** the same as the build machine's, NixOS 24.05 at `89c49874fb15f4124bf71ca5f42a04f2ee5825fd`
  (`/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source`, substituted from the cache).
  `pkgsStatic.gcc.cc` evaluates to the pinned `aa1jsl19…-gcc-static-x86_64-unknown-linux-musl-13.2.0`
  (derivation `5v8gvjag…`) and the Redis expression of "How the two missing derivations were found"
  to `ky1xq6xa…`, as on the build machine.
- **What came from where:** of the 51 store paths the build scripts and manifests name, 48 are in
  cache.nixos.org (checked with each path's `.narinfo`); the three that are not are the two
  derivations of this file (`aa1jsl19…` gcc, `ky1xq6xa…` Redis) and the unrestorable Python
  environment. The static gcc was built from its derivation on the bare-metal machine (about half an hour on its 8
  threads; its build inputs from the cache). MySQL, the JDK, Go 1.26.5 (`pwj3x44p…`), glibc 2.39-52 (`dbcw19ds…`),
  PostgreSQL 15.6, curl, musl 1.2.6 (`0nlqfwsn…`) and binutils 2.41 (`7v7g86ml…`) were substituted
  with their closures; Kafka (`prk07apd…`) was put there alone with `hydo-nix-fetch` (no
  `--closure`), because its closure holds `iproute2-6.8.0` (`dx4fsbxs…`), and that path and
  `iptables-1.8.10` (`xnrrq638…`) must never be in the host's `/nix/store`: they are roots of the
  Linux release's bundled closures (`guest/store-closures.txt`), and once present on the host a
  release build would look them up with `nix-store`, which other sessions do not have on PATH.
  GC roots: `~/hydo-nixb-work/gcroots/`. Redis was not rebuilt: the stack images use the
  `redis-server`/`redis-cli` binaries already proven identical to a rebuild (`e66c3fd0…`,
  `7ecd56f4…`, "Proof" above).
- **The pinned gcc needs nixpkgs' binutils 2.41 on PATH.** The gcc derivation has no assembler or
  linker of its own; it runs `as` and `ld` from PATH. With Ubuntu's binutils 2.42 the pinned gcc
  gives `hvx_seedrng` `e1a84724…` (the same `.text`, but no `.note.gnu.property` section); with
  nixpkgs 24.05's `binutils-2.41` (`7v7g86ml…`, unwrapped, or its wrapper `bl7gf4cr…`) first on
  PATH it gives the pinned `c2a440c5…`, and `hydoavx` of the avx2 example equals the one inside the
  recorded image (`a0e5af37…`). So the build machine's PATH had nixpkgs' binutils 2.41, and every
  use of the pinned gcc outside it needs `PATH=/nix/store/7v7g86ml…-binutils-2.41/bin:$PATH`.
- **How the images are built on the bare-metal machine:** the Linux release's tools (M136's prefix: this tree's
  `hydo`, the release's guest files, tools and faketime) with `nix-store` on PATH, so the
  manifests' `nix:` closures resolve with `nix-store -qR` as on the build machine; as uid 1000,
  gid 100 with `HYDO_KEEP_OWNERS=1` (`sudo setpriv --reuid=1000 --regid=100 --clear-groups`),
  which gives the build machine's owners (its plain `hydo-build` keeps the builder's IDs, and the
  recorded images have 1000:100). The script is M140's `img.sh` (docs/HISTORY.md Milestone 140).

## Mac guest kernel for service networks (Milestone 99, no Nix)

The second arm64 guest kernel, `Image-explore-netns` (`hv/hvm/explore/build-guest-netns.sh`), is
built without Nix, on any x86_64 Linux machine (built on the bare-metal machine in `~/hydo-macns`;
nothing was installed system-wide: the bare-metal machine already had make, flex, bison, bc, perl, python3 and
gcc 13.2.0). The script downloads its two inputs into `$DL` and checks them by sha256:

| input | where from | sha256 |
|---|---|---|
| `linux-6.6.35.tar.xz` | `https://cdn.kernel.org/pub/linux/kernel/v6.x/` (matches kernel.org's `sha256sums.asc`) | `fce3ee728712ed063aa8c14a8756c8ff8c7a46ba3827f61d2b04a73c7cf5dd9e` |
| `arm-gnu-toolchain-13.3.rel1-x86_64-aarch64-none-linux-gnu.tar.xz` (GCC 13.3.1 20240614, Arm build arm-13.24) | `https://developer.arm.com/-/media/Files/downloads/gnu/13.3.rel1/binrel/` (matches Arm's `.sha256asc`) | `322f0b4482fc0d9fa0bb468134841f08d8c554c54ff5aa29a13a7a24bf7e1eb5` |

Outputs (two builds in different directories, byte-identical): `Image-explore-netns`
`806807e7be5f55dae342f796ccc6e20c35514b3684a8c0d4633901a8ac0aa609` (recorded in
`NETNS_KERNEL_SHA256` in `hydo/crates/hydo-build/src/arm64.rs`; before M103 in `hydo_build_arm64.py`), its `.config` `0aff83371194fdcf41108b6e2c4666bf827fbc735e8102906d047c65631e9b16`.
The default kernel `Image-explore` (`f80c5655…`, Nix cross gcc) is not rebuilt and not changed.

The same toolchain builds the freeze helper for aarch64:
`aarch64-none-linux-gnu-gcc -static -O2 -Wall hydo/onboard/hydo_fc.c -o hydo_fc-arm64` gives
`499f654e05b712601601c9286fa4c4836e28f31fd7e03e3443228f2fc5060c4e` (twice, in different
directories; glibc static, the toolchain's own sysroot). Milestone 150 gave the same bytes twice
on the Mac, with the toolchain archive (checked by its sha256) unpacked and run inside an x86_64
Linux container (unpacked inside the container because the Mac's file system ignores case); the
Mac release ships that file as `guest/hydo_fc`.

The aarch64 user-space tools of these images come from the public Nix binary cache, taken with
`hydo-nix-fetch --closure <dir> <path>` (`hydo/crates/hydo-tools`; until M108 `hydo/dist/linux/nix_cache_fetch.py`, which gives the same tree; it checks each archive's FileHash and NarHash;
on a case-insensitive Mac file system nine iptables extensions whose names differ only in case
are read but not written, none of them used):

| store path (aarch64-linux) | NarHash | staged |
|---|---|---|
| `qmhi9c99dpgxyfg6cipf8xbmcvsr1vx2-iproute2-6.8.0` | `sha256:11gbr98yllcv8cwxdmkn2iwgrrzyd4c01az9hyhicf5jfxjhs472` | `bin/ip` (its libraries are a subset of `tc`'s) |
| `y6j6xf1bfshzhdw7hip3swpgd6fqfs3b-iptables-1.8.10` | `sha256:177d4lvj73k9r9xj5hjwfkc1z40yknnzh1dva8q8p9w42nxc38zw` | `bin/xtables-legacy-multi`, `lib/xtables/libxt_{NFQUEUE,tcp,standard}.so`, `lib/lib{ip4tc,ip6tc,xtables}.so*` |

The files each image gets are exactly those `hydo-elfdeps --root <dir>` (`hydo/crates/hydo-tools`) lists for
the binaries above (glibc 2.39-52, libmnl, zlib, xz, bzip2, zstd, elfutils from the same closure).
Since Milestone 150 the Mac release ships those files: `hydo/dist/mac/build-release.sh` fetches
the two store paths with `hydo-nix-fetch --closure` into `assets/netns-store`, keeps the 28 paths
`hydo-elfdeps --root` lists for `bin/ip` and the four iptables files (`assets/netns-closure.txt`,
each file pinned in `hydo/dist/mac/assets.sha256`), and puts them into the release's
`guest/tc` tree.

## Rust: the workspace toolchain and the release binaries (Milestone 101)

The Cargo workspace at the repository root (`Cargo.toml`, crates under `hydo/crates/`) builds
the `hydo` command of both releases. The pins:

- **Compiler:** `rust-toolchain.toml` pins `channel = "1.90.0"` (rustc 1.90.0, commit
  `1159e78c4747b02ef996e55082b704c09b970588`, 2025-09-14) with `clippy` and `rustfmt`. rustup
  installs it on first use. The same version already builds `sdk/rust-test` and the Rust
  examples (`hydo/onboard/examples/redis-composed-rs/rust-toolchain.toml`).
- **Targets:** `x86_64-unknown-linux-musl` for the Linux release (static; `.cargo/config.toml`
  links it with `rust-lld` and `+crt-static`, so a Mac can cross-build it as well as Linux) and
  `aarch64-apple-darwin` for the Mac release.
- **Crates:** `Cargo.lock` is committed and the release scripts build with `--locked`
  (serde 1.0.229, serde_json 1.0.154 with `preserve_order`, flate2 1.1.10 with the pure-Rust
  backend, sha2 0.10.9, regex 1.13.1, libc 0.2.191).
- **Where it was installed:** this Mac (rustup, the user's own toolchains); the bare-metal test
  machine in a private directory (`~/hydo-rust`, `RUSTUP_HOME` and `CARGO_HOME` there, nothing
  system-wide; `~/hydo-rust/env.sh` sets them), from `rustup-init` of static.rust-lang.org.
- **Results:** the Linux `hydo` is an `ELF 64-bit LSB pie executable, x86-64, static-pie linked`
  of about 1.2 MB; the Mac `hydo` a `Mach-O 64-bit executable arm64` that the linker signs ad hoc.

- **hydo-world (Milestone 111):** the in-guest program of `fakes:`/`proxies:` is the workspace crate
  `hydo/crates/hydo-world`, built by `hydo/crates/hydo-world/build.sh` for
  `x86_64-unknown-linux-musl` and (`HYDO_WORLD_ARCH=arm64`) `aarch64-unknown-linux-musl`, both
  linked by `rust-lld` with `+crt-static` (`.cargo/config.toml`), `--locked`, the release
  profile (stripped). Three things keep its bytes independent of where and on what it is built:
  the checkout and `$CARGO_HOME` are remapped out of the binary (`--remap-path-prefix`); the
  linker's `.comment` section is dropped with the toolchain's own `rust-objcopy` (the Mac's and
  Linux's `rust-lld` of the same LLVM commit write different identification strings there); and
  Cargo is given `rustc-host.sh` as `RUSTC`, which answers `rustc -vV` with one fixed `host:`
  line (Cargo mixes that line into the identity of every build script, every crate depending on
  one inherits it, and rustc's symbol hashes and type ids come from those identities, so a Mac
  and a Linux machine otherwise compile different code). Proof (M111 sources): this Mac and the
  bare-metal machine built the same files, x86_64 `082592ae…` (2,285,944 bytes, static-pie)
  and arm64 `fefcca66…` (1,902,328 bytes); two checkouts at different paths gave the same files.
  Extra crates (Cargo.lock): tokio 1.54.1 (current-thread runtime), h2 0.4.20, http 1.5.0,
  bytes 1.12.1, httparse 1.10.1. It replaces the Go program of `hydo/onboard/world/` (Go 1.26.5,
  `2da400f3…` x86_64), whose bytes are in the images built before M111.

The SDK crates (`sdk/rust`, `sdk/rust-test`) and the Rust examples are not workspace members:
they keep their own workspaces and lock files, because their bytes are inside recorded images.

## The gRPC client and generator (Milestone 113)

`hydo-grpc`, the static client that generated gRPC drivers put in a guest image, and the
generator `hydo-grpc-gen` are Rust (`hydo/crates/hydo-grpc`, `hydo/crates/hydo-grpc-gen`); the Go
programs of Milestone 91 were deleted. The demo's service `orderd` stays Go (it is the system
under test, not hydo's code): `hydo/grpcgen/go.mod` is kept unchanged for it, and with Go 1.26.5
from go.dev it still builds `orderd-arm64` `a87ca7fa…` (M96).

- **Toolchain:** the workspace's Rust 1.90.0 with the targets `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl`, both linked by `rust-lld` with `+crt-static` (`.cargo/config.toml`).
- **Crates (Cargo.lock):** prost 0.14.4, prost-reflect 0.16.5 (with `serde`), tokio 1.54.1
  (`rt`, `net`, `time`, `sync`, `macros`; a current-thread runtime), h2 0.4.20, http 1.5.0,
  bytes 1.12.1, ryu 1.0.23 for the client; protox 0.9.1 and libyaml-safer 0.3.0 for the generator.
  All pure Rust.
- **Build:** `hydo/grpcgen/build-client.sh [--out DIR] amd64 arm64` (called by
  `hydo/onboard/hydo-grpc-gen`). rustc runs through `hydo/grpcgen/rustc-stable`, which replaces
  Cargo's `-C metadata` hash (it covers the host-built build scripts, so it differs between a Mac
  and a Linux host and moves every function in the binary) by a hash of the crate's name, version,
  type, target and cfgs, and remaps the tree and `$CARGO_HOME` to `/hydo` and `/cargo`;
  `hydo-elf-comment` (`hydo/crates/hydo-tools`) then sets the `.comment` linker line to
  `Linker: LLD` (the Linux and the Mac rust-lld name different build paths there).
- **Results (this tree, M113):** `hydo-grpc` (x86_64, `ELF 64-bit LSB pie executable, static-pie
  linked`, 3.1 MB) `3926d13712772cce…`, the same bytes from this Mac and from the bare-metal test
  machine; `hydo-grpc.arm64` (`ELF 64-bit LSB executable, ARM aarch64, statically linked`, 2.6 MB)
  `fd762fa06b352d0e…` (Mac; the bare-metal machine's private rustup has no aarch64 musl
  target). The Go clients were 10.6 MB (`c772698c…`, nixpkgs Go) and `686b9e57…` (arm64, go.dev Go).
- **Well-known imports:** `hydo/crates/hydo-grpc-gen/src/wkt.pb` holds the descriptors of
  `google/protobuf/*.proto` exactly as protocompile v0.14.1 with protobuf-go v1.35.2 produced them
  for the Go generator (sha256 `f9696902ad5a824e…`), so a schema that imports them is the same
  `desc.pb` as before.

## hydo-read: the in-guest store reader in Rust (Milestone 112)

`hydo/crates/hydo-read` (the program that runs as `hydo-read` and, through a symlink the image
builder makes, as `hydo-check`) is staged into guest images, so its bytes are part of their
fingerprints. It keeps its own Cargo workspace and `Cargo.lock` (it is not a member of the
repository's workspace), so no dependency update elsewhere changes it. The pins:

- **Compiler:** Rust 1.90.0 (`cargo +1.90.0`, the workspace's toolchain) with the targets
  `x86_64-unknown-linux-musl` (hvx guests) and `aarch64-unknown-linux-musl` (Mac guests), linked
  by Rust's own `rust-lld` with the targets' self-contained musl and `+crt-static`; no C compiler.
- **Crates** (all pure Rust, pinned by the crate's `Cargo.lock`, built with `--locked`): flate2
  1.1.10 with miniz_oxide 0.9.1 (gzip), snap 1.1.2 (snappy), lz4_flex 0.11.6 (LZ4 frames), ruzstd
  0.7.3 (zstd), sha1 0.10.7, sha2 0.10.9, md-5 0.10.6 and hmac 0.12.1 (MySQL and PostgreSQL
  authentication), rsa 0.9.10 with rand_core 0.6.4 (the RSA path of MySQL's
  caching_sha2_password). The store protocols themselves are written in the crate.
- **Build:** `hydo/crates/hydo-read/build.sh [x86_64|aarch64|all] [OUTDIR]` writes
  `build/hydo-read` and `build/hydo-read-arm64`. It remaps the checkout path and `$CARGO_HOME`,
  so the bytes do not depend on where the tree or the crate cache lives (checked: two
  directories, two cargo homes, the same bytes).
- **The build host matters.** Cargo puts the output of `rustc -vV`, which names the host
  triple, into every crate's `-C metadata`, so the symbol hashes, and with them the bytes, depend
  on the kind of host that compiles. The values for commit `c93657c`:

  | target | built on | size | sha256 |
  |---|---|---|---|
  | x86_64 | x86_64-unknown-linux-gnu (the bare-metal machine, `~/hydo-rust`) | 1,389,016 | `6445237458633ef4df3622d3fcbaaa77cb15455cae53bddec06aa73f704734eb` |
  | x86_64 | aarch64-apple-darwin (the Mac) | 1,389,024 | `2b4d6a62805ac8fce66c92dc66b417e1a0a02e24877f878cc126b7b9ed47fd4d` |
  | aarch64 | aarch64-apple-darwin (the Mac) | 1,159,720 | `88576fa899570cc7f826a7e05f701f0aa3245cdd3f8be4f4adea8367d9b8842e` |

  The x86_64 images of Milestone 112 carry the first, the Mac images the third. The Go program
  it replaced was 10.3 MB, and nixpkgs Go and upstream Go built different bytes of it (M96).

## hydo-agent: the guest agent (Milestone 117)

`hydo/crates/hydo-agent` is the program that will run as the guest's first process under host
control (docs/design/guest-control.md). Its bytes will be inside guest images, so they are pinned
here. It keeps its own Cargo workspace and `Cargo.lock` (not a member of the repository's
workspace) and depends only on `libc` 0.2.191 and the protocol crate `hydo-agent-proto`, which
has no dependencies.

- **Compiler:** Rust 1.90.0 (the repository's `rust-toolchain.toml`) with the targets
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, linked by `rust-lld` with
  `+crt-static` (`.cargo/config.toml`); no C compiler. Profile: `opt-level = "s"`, LTO, one
  codegen unit, symbols stripped, `panic = "abort"`.
- **Build:** `hydo/crates/hydo-agent/build.sh [x86_64|aarch64|all] [OUTDIR]` writes
  `hydo-agent` and `hydo-agent-arm64`. It uses Milestone 113's recipe: rustc runs through
  `hydo/grpcgen/rustc-stable` (a host-independent `-C metadata`; the tree and `$CARGO_HOME`
  remapped to `/hydo` and `/cargo`), and `hydo-elf-comment` sets the `.comment` linker line to
  `Linker: LLD`. So the bytes do not depend on the build host, the tree's path or the crate cache.
- **Pinned (Milestone 140; hydo-agent 0.3.0 with the host-call port, hello feature `hostcall`,
  built in Milestone 138):** the same bytes from this Mac and from the bare-metal machine
  (M138 in `~/hydo-mod`; M140 again on this Mac and in `~/hydo-nixb` on the bare-metal machine, each with the
  pinned Rust 1.90.0):

  | target | size | sha256 |
  |---|---|---|
  | x86_64 (`ELF 64-bit LSB pie executable, static-pie linked`) | 668,896 | `398f297e7425768a6175e58d27eca00d417fec25f0fb8b48842bcbbb53fdb8a6` |
  | aarch64 (`ELF 64-bit LSB executable, statically linked`) | 566,800 | `099e54440e04a89798efc5755a73a181c87aa0731f10129bc146f53de9432e87` |

  The aarch64 agent never takes the port (the Mac engine has none). Every agent image built since
  M140 carries these bytes; an image built before (0.2.0, below) still runs, and stays on the
  serial line because its agent does not know the port.

  Before (Milestone 127, hydo-agent 0.2.0 with every child started with an empty signal mask;
  the version string is unchanged, so the recorded native transcripts still replayed; the same
  bytes from this Mac and from the bare-metal machine): x86_64 668,032 bytes
  `b72c5caa38484202541d2a2272726e06ffea674991df7a968fd270894f12239d`, aarch64 566,528 bytes
  `340a1eec749909a65f8fb8990629c36108b9bc8d91b997e6a000d601c21013d9`; every image built by
  Milestone 136 carries these.

  Before (Milestone 131, hydo-agent 0.2.0 with the freeze's `netns` field, feature
  `freeze-netns`; children inherited the agent's blocked SIGCHLD, so a shell's `wait` for a
  background job never returned): x86_64 667,904 bytes
  `1f517e04f837658a0f0fb85b0077d29b491d10eca75c2529ba0e67944ad24422`, aarch64 566,448 bytes
  `585f0bdb8cf1d03a3e95d0c8844e85617515939f42d1fe76d02fe8d2c7d72d30`.

  Before (M117, hydo-agent 0.1.0, the same from the Mac and from Linux): x86_64 665,632 bytes
  `ff7652a171f40c80f83c2c6ee871331f046b7c2953faf25e40505bbb12f2a16c`, aarch64 564,768 bytes
  `6a3ea9f8c80e972abda55a58e05fee9de0b7355b3a27f5ae7a8bb560d2f6fac1`; every agent image built
  before M131 carries these, and the host refuses an F= with `network: bridge` on such an image
  with a message (its hello has no `freeze-netns`).

  The C freeze helper it can stand in for (`hydo_fc`, pinned musl gcc) is 68,504 bytes.
- **In images (Milestone 121; every image since Milestone 136):** `hydo-build` stages exactly
  these bytes as the image's first process and refuses another build of the agent (`HYDO_AGENT_UNPINNED=1` stages it
  with a warning, for work on the agent). The two sha256 values are also in
  `hydo/crates/hydo-build/src/agent.rs` (`AGENT_SHA256_X86_64`, `AGENT_SHA256_ARM64`); a new agent
  changes this table and those constants together, and every agent image's bytes with them.
