# Pinned nix store paths for the image builds (the build machine)

Written 2026-10-21. The image scripts copy tools and binaries out of exact `/nix/store` paths
so that a rebuild is byte-identical. When the build machine garbage-collected some of them (noticed at
Milestone 46), builds fell back to `HYDO_GCC_MUSL` (a wrapper that copied a prebuilt
`hvx_seedrng`) and `HYDO_REDIS_OUT` (prebuilt Redis binaries). This file lists every pin,
says what was restored and how, and records the proof that from-scratch builds work again.

## Inventory

Pins referenced by `build-linux.sh`, `build-stack.sh`, `build-kafka.sh`, `build-mysql.sh`,
`hydo/onboard/hydo-build` and `hydo/onboard/hydo_build_arm64.py` (store hash shortened to 8
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
| `qmhi9c99…-iproute2-6.8.0` (aarch64) | hydo_build_arm64.py | present | gc-rooted |
| `zbc5fx2y…-busybox-static-aarch64-unknown-linux-musl-1.36.1` | hydo_build_arm64.py | present | gc-rooted |
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
`hydo_build_arm64.NETNS_KERNEL_SHA256`), its `.config` `0aff83371194fdcf41108b6e2c4666bf827fbc735e8102906d047c65631e9b16`.
The default kernel `Image-explore` (`f80c5655…`, Nix cross gcc) is not rebuilt and not changed.

The same toolchain builds the freeze helper for aarch64:
`aarch64-none-linux-gnu-gcc -static -O2 -Wall hydo/onboard/hydo_fc.c -o hydo_fc-arm64` gives
`499f654e05b712601601c9286fa4c4836e28f31fd7e03e3443228f2fc5060c4e` (twice, in different
directories; glibc static, the toolchain's own sysroot).

The aarch64 user-space tools of these images come from the public Nix binary cache, taken with
`hydo/dist/linux/nix_cache_fetch.py --closure <dir> <path>` (checks each archive's FileHash and NarHash;
on a case-insensitive Mac file system nine iptables extensions whose names differ only in case
are read but not written, none of them used):

| store path (aarch64-linux) | NarHash | staged |
|---|---|---|
| `qmhi9c99dpgxyfg6cipf8xbmcvsr1vx2-iproute2-6.8.0` | `sha256:11gbr98yllcv8cwxdmkn2iwgrrzyd4c01az9hyhicf5jfxjhs472` | `bin/ip` (its libraries are a subset of `tc`'s) |
| `y6j6xf1bfshzhdw7hip3swpgd6fqfs3b-iptables-1.8.10` | `sha256:177d4lvj73k9r9xj5hjwfkc1z40yknnzh1dva8q8p9w42nxc38zw` | `bin/xtables-legacy-multi`, `lib/xtables/libxt_{NFQUEUE,tcp,standard}.so`, `lib/lib{ip4tc,ip6tc,xtables}.so*` |

The files each image gets are exactly those `hv/hvm/services/elfdeps.py --root <dir>` lists for
the binaries above (glibc 2.39-52, libmnl, zlib, xz, bzip2, zstd, elfutils from the same closure).

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

The SDK crates (`sdk/rust`, `sdk/rust-test`) and the Rust examples are not workspace members:
they keep their own workspaces and lock files, because their bytes are inside recorded images.
