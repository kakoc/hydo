#!/bin/sh
# Build the self-contained x86_64 Linux release of hydo (see hydo/dist/linux/README.md):
#     hydo/dist/linux/build-release.sh [OUTDIR]        -> OUTDIR/hydo-linux-<version>/ and its .tar.gz,
#                                                         and OUTDIR/hydo-linux-<version>-sources.tar.gz
#                                                         (the corresponding source, M158; docs/RELEASING.md)
#
# Runs on any machine with sh, rsync, tar, sha256sum/shasum and cargo (the Mac works, with
# HYDO_RUST_BIN_DIR or a C cross compiler for the musl target). The
# guest and host-tool pieces are fetched once into hydo/dist/linux/assets/ (pinned by sha256 in
# assets.sha256) from a Linux machine with the pinned nixpkgs store paths below (docs/BUILD-PINS.md):
# the build machine, or with HYDO_NIX_MACHINE=bare-metal the bare-metal machine, reached through
# machines.local.sh (machines.sh at the repository root):
#   guest/bzImage, guest/initramfs-kafka.cpio.gz   the guest kernel + its switch_root initramfs
#                                                  (~/hydo/build on the build machine, build-linux.sh)
#   guest/busybox                                  static busybox 1.37.0 (every image's /bin)
#   guest/hvx_seedrng, guest/hydo_fc               static helpers, compiled there with the pinned
#                                                  musl gcc from hv/hvx/images/hvx_seedrng.c, hydo/onboard/hydo_fc.c
#   guest/modules/6.6.35/...                       the guest kernel's netem/u32/netfilter modules
#   guest/store.tar, guest/store-closures.txt      the iproute2 + iptables closures (tc, iptables-legacy
#                                                  inside the guest; "<root> <member>" lines, nix-store -qR)
#   tools/bin/{mke2fs,debugfs}, tools/lib/*        e2fsprogs 1.47.0 + libfaketime 0.9.10 with their
#                                                  glibc, run on the user's machine by libexec/faketime
#   redis/{redis-server,redis-cli}                 the Redis example's static binaries
# HYDO_ASSETS_FROM=DIR copies these files from a local directory of the same layout instead of
# fetching them (for example a copy kept on the bare-metal machine; each file is still
# checked against assets.sha256). With HYDO_PIN=1 a missing assets.sha256 is written from the
# fetched files (once, by a maintainer). Once assets/ is filled the release builds offline.
# Users of the release need no Nix.
# Everything else is built here from this tree with the pinned Rust toolchain: the programs
# (bin/), the guest agent (guest/hydo-agent, checked against its pin by hydo-build), hydo-world,
# hydo-read and the gRPC client hydo-grpc. The tarball's bytes depend only on the inputs (fixed
# file times, owners and order; gzip without a time stamp).
set -e
umask 022
D=$(cd "$(dirname "$0")" && pwd)
TOP=$(cd "$D/../../.." && pwd)
VERSION=${HYDO_VERSION:-0.1.0}
OUTDIR=${1:-$D/out}
A=$D/assets
GB=$TOP/target/dist-linux     # the guest programs built from this tree (not assets: they are not pinned here)
HYDO_REPO=$TOP; . "$TOP/machines.sh"   # the machine with the store paths (only to fetch assets)
BUSYBOX=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
E2FS=/nix/store/24s0phlx9kly7x7qb8gal3mmn51ndmlx-e2fsprogs-1.47.0-bin
FAKETIME=/nix/store/fs0gp7zyfjnbvaq82yvwbcsvqh7876d1-libfaketime-0.9.10
IPROUTE2=/nix/store/dx4fsbxs5v82l9lzmjkpkhx5ygqcwm69-iproute2-6.8.0
IPTABLES=/nix/store/xnrrq638db6170pg7dvz84f8nr1q2wvi-iptables-1.8.10
KMODS=/nix/store/rp8637p734k6rshqjqlnfdm97b4jn8w9-linux-6.6.35/lib/modules/6.6.35
REDIS=/nix/store/ky1xq6xa88qhwwagnh4anm4sjl3c7vyk-redis-static-x86_64-unknown-linux-musl-7.2.4
GCC_MUSL=/nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc
MUSL_ROOT=/nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6
MODS="sch_prio sch_netem cls_u32 x_tables ip_tables iptable_filter xt_tcpudp xt_NFQUEUE nfnetlink nfnetlink_queue llc stp bridge veth"
if command -v sha256sum >/dev/null; then SHA="sha256sum"; else SHA="shasum -a 256"; fi

# --- 1. assets (fetched once, in one remote session; that machine keeps nothing afterwards) ---
if [ ! -f "$A/.complete" ] && [ -n "$HYDO_ASSETS_FROM" ]; then
  [ -d "$A" ] && chmod -R u+w "$A" && rm -rf "$A"; mkdir -p "$A"
  echo "copying the release assets from $HYDO_ASSETS_FROM"
  ( cd "$HYDO_ASSETS_FROM" && tar cf - guest tools redis ) | ( cd "$A" && tar xpf - )
  touch "$A/.complete"
fi
if [ ! -f "$A/.complete" ]; then
  [ -d "$A" ] && chmod -R u+w "$A" && rm -rf "$A"; mkdir -p "$A"
  need_nix_machine
  echo "fetching the release assets from the machine with the Nix store paths"
  ( cd "$TOP" && tar --no-xattrs -cf - hv/hvx/images/hvx_seedrng.c hydo/onboard/hydo_fc.c ) | on_nix_machine "
    set -e
    W=\$(mktemp -d /dev/shm/hydo-linux-release.XXXXXX); trap 'chmod -R u+w \$W; rm -rf \$W' EXIT
    cd \$W && tar xf - >&2
    mkdir -p out/guest/modules out/tools/bin out/tools/lib out/redis
    cp ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz out/guest/
    cp $BUSYBOX out/guest/busybox
    $GCC_MUSL -static -O2 -Wall -B $MUSL_ROOT/lib hv/hvx/images/hvx_seedrng.c -o out/guest/hvx_seedrng >&2
    $GCC_MUSL -static -O2 -Wall -B $MUSL_ROOT/lib hydo/onboard/hydo_fc.c -o out/guest/hydo_fc >&2
    ( cd $KMODS/.. && for m in $MODS; do find 6.6.35 -name \$m.ko.xz; done | sort | tar cf - -T - ) | ( cd out/guest/modules && tar xf - )
    # the guest-side closures as ONE GNU tar with their exact (read-only) modes: hydo-build copies
    # them with cp -a, so the image gets the store's modes; `hydo setup` unpacks it (GNU tar sets a
    # read-only directory's mode only after its contents, which a re-packed tree would not survive)
    for p in $IPROUTE2 $IPTABLES; do nix-store -qR \$p | sort > \$(basename \$p).closure; done
    sort -u *.closure | sed 's#^/##' | ( cd / && tar --sort=name --owner=0 --group=0 --numeric-owner -cf - -T - ) > out/guest/store.tar
    for p in $IPROUTE2 $IPTABLES; do sed \"s#^#\$p #\" \$(basename \$p).closure; done > out/guest/store-closures.txt
    # host tools: the ELF files + every library they load, dereferenced into one directory
    cp $E2FS/bin/mke2fs $E2FS/bin/debugfs out/tools/bin/
    cp $FAKETIME/lib/libfaketime.so.1 out/tools/lib/
    for b in $E2FS/bin/mke2fs $E2FS/bin/debugfs $FAKETIME/lib/libfaketime.so.1; do
      ldd \$b | sed -n 's#.*=> \(/nix/store/[^ ]*\) .*#\1#p; s#^\s*\(/nix/store/[^ ]*ld-linux-x86-64.so.2\) .*#\1#p'
    done | sort -u | while read l; do cp -L \$l out/tools/lib/\$(basename \$l); done
    cp $REDIS/bin/redis-server $REDIS/bin/redis-cli out/redis/
    chmod -R u+w out/tools out/redis out/guest/bzImage out/guest/initramfs-kafka.cpio.gz out/guest/busybox
    cd out && tar cf - .
  " | ( cd "$A" && tar xpf - )
  touch "$A/.complete"
fi
if [ ! -f "$D/assets.sha256" ] && [ "${HYDO_PIN:-0}" = 1 ]; then
  ( cd "$A" && find . -type f ! -name .complete | LC_ALL=C sort | sed 's#^\./##' | xargs $SHA ) > "$D/assets.sha256"
  echo "wrote $D/assets.sha256 ($(wc -l < "$D/assets.sha256") files)"
fi
[ -f "$D/assets.sha256" ] || { echo "no assets.sha256 (a maintainer pins the assets once with HYDO_PIN=1)" >&2; exit 1; }
nf=$(cd "$A" && find . -type f ! -name .complete | wc -l | tr -d ' ')
[ "$nf" = "$(wc -l < "$D/assets.sha256" | tr -d ' ')" ] || { echo "assets/ has $nf files, assets.sha256 lists $(wc -l < "$D/assets.sha256")" >&2; exit 1; }
( cd "$A" && $SHA -c "$D/assets.sha256" >/dev/null ) || {
  echo "assets do not match assets.sha256 (pinned); remove hydo/dist/linux/assets and fetch again" >&2
  ( cd "$A" && $SHA -c "$D/assets.sha256" 2>/dev/null | grep -v ': OK$' | head ) >&2; exit 1; }

# --- 2. the release tree ---
REL=$OUTDIR/hydo-linux-$VERSION
[ -d "$REL" ] && chmod -R u+w "$REL" && rm -rf "$REL"
# the release tree mirrors the repository: hydo/ (the product) and module/ (the kernel module)
mkdir -p "$REL/bin" "$REL/libexec/bin" "$REL/hydo/onboard" \
         "$REL/module" "$REL/examples/redis/files" "$REL/skill"
# the programs, all Rust (M107: no Python anywhere in the release): bin/hydo (the one CLI,
# hydo/crates/hydo-cli; the run engine runs inside it when the user can open /dev/hvx, M137),
# bin/hydo-hvx (the same run engine as a program, hydo/crates/hydo-hvx, which hydo starts under
# libexec/hvx-run with sudo when the user cannot open the device; M137: it replaces hvx_explore,
# so nothing is compiled for it on the user's machine), bin/hydo-build (the image builder with its own
# command line, for scripts; `hydo build` is the same builder with the release's tools) and
# bin/hydo-fetch-image (what fetch-images.sh of `hydo compose` calls), and the gRPC driver
# generator (libexec/hydo-grpc-gen, started by the wrapper bin/hydo-grpc-gen, which points its
# --client at the release's guest/hydo-grpc; MANIFEST.md "gRPC services"), static x86_64 musl
# binaries. Built here with the workspace's pinned toolchain (rust-toolchain.toml; rustup adds
# the musl target; the TLS code of hydo-fetch-image compiles some C, with musl-gcc when present,
# else the host's cc), or taken from $HYDO_RUST_BIN_DIR (a directory with the five) when they
# were built elsewhere.
BINS="hydo hydo-hvx hydo-build hydo-fetch-image"
if [ -n "$HYDO_RUST_BIN_DIR" ]; then
  BD=$HYDO_RUST_BIN_DIR
else
  if [ -z "$CC_x86_64_unknown_linux_musl" ] && [ "$(uname -s)" = Linux ]; then
    CC_x86_64_unknown_linux_musl=$(command -v musl-gcc || command -v cc); export CC_x86_64_unknown_linux_musl
  fi
  # the tree and the Cargo home are remapped to /hydo and /cargo (as the guest programs' builds
  # do), so the programs' panic messages name no directory of the building machine (M153: they
  # named the builder's home); RUSTFLAGS replaces the target's rustflags of .cargo/config.toml,
  # so +crt-static is repeated here
  CH=$(cd "${CARGO_HOME:-$HOME/.cargo}" 2>/dev/null && pwd || echo "${CARGO_HOME:-$HOME/.cargo}")
  ( cd "$TOP" && { rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true; } &&
    RUSTFLAGS="-C target-feature=+crt-static --remap-path-prefix=$TOP=/hydo --remap-path-prefix=$CH=/cargo" \
    cargo build --release --locked -p hydo-cli -p hydo-hvx -p hydo-build -p hydo-compose -p hydo-grpc-gen \
      --target x86_64-unknown-linux-musl ) >&2
  BD=$TOP/target/x86_64-unknown-linux-musl/release
fi
for b in $BINS hydo-grpc-gen; do
  file -L "$BD/$b" 2>/dev/null | grep -q 'x86-64.*static' || { echo "$BD/$b is not a static x86_64 binary" >&2; exit 1; }
done
for b in $BINS; do cp "$BD/$b" "$REL/bin/$b"; chmod 755 "$REL/bin/$b"; done
cp "$BD/hydo-grpc-gen" "$REL/libexec/hydo-grpc-gen"
cp "$D/libexec/hvx-run" "$D/libexec/faketime" "$REL/libexec/"
cp "$D/libexec/bin/debugfs" "$REL/libexec/bin/"
cp "$D/bin/hydo-grpc-gen" "$REL/bin/hydo-grpc-gen"
chmod 755 "$REL/libexec/hvx-run" "$REL/libexec/faketime" "$REL/libexec/bin/debugfs" \
          "$REL/libexec/hydo-grpc-gen" "$REL/bin/hydo-grpc-gen"
# the format reference (`hydo guide manifest`) and hydo_fc.c, the source of guest/hydo_fc;
# the shell init and its extensions are gone since M136 (every image's first process is
# guest/hydo-agent, driven by the host controller inside bin/hydo)
cp "$TOP/hydo/onboard/hydo_fc.c" "$TOP/hydo/onboard/MANIFEST.md" "$REL/hydo/onboard/"
echo "$VERSION" > "$REL/hydo/VERSION"     # what a plan's hydo_version: is checked against
# the history recorder for shell workloads (MANIFEST.md "History checks")
mkdir -p "$REL/hydo/history"
cp "$TOP/hydo/history/hydo_hist.sh" "$REL/hydo/history/"
# the assertion protocol the SDKs write and the explorer's property table reads
cp "$TOP/hydo/ASSERTIONS.md" "$REL/hydo/"
# the kernel module's sources (hydo module build)
cp "$TOP/hv/hvx/hvx.c" "$TOP/hv/hvx/hvx_abi.h" "$TOP/hv/hvx/Makefile" "$REL/module/"
mkdir -p "$REL/guest" "$REL/tools"
( cd "$A/guest" && tar cf - . ) | ( cd "$REL/guest" && tar xpf - )
( cd "$A/tools" && tar cf - . ) | ( cd "$REL/tools" && tar xpf - )
chmod 755 "$REL/guest/busybox" "$REL/guest/hvx_seedrng" "$REL/guest/hydo_fc" "$REL/tools/bin/"*
# the guest agent of agent images (hydo build --control agent, M121; driven by the host controller,
# M126): built here byte-reproducibly (hydo/crates/hydo-agent/build.sh, no C toolchain); hydo-build
# checks its sha256 against the pin in docs/BUILD-PINS.md when it stages it
"$TOP/hydo/crates/hydo-agent/build.sh" x86_64 "$GB/agent" >&2
cp "$GB/agent/hydo-agent" "$REL/guest/hydo-agent"; chmod 755 "$REL/guest/hydo-agent"
# guest/hydo-world: the in-guest program of fakes: and proxies: (hydo/crates/hydo-world, Rust since
# M111), a static x86_64 musl build that needs no build machine; `hydo build` stages it
# ($HYDO_RUST_BIN_DIR/hydo-world when the binaries were built elsewhere)
if [ -n "$HYDO_RUST_BIN_DIR" ] && [ -f "$HYDO_RUST_BIN_DIR/hydo-world" ]; then
  cp "$HYDO_RUST_BIN_DIR/hydo-world" "$REL/guest/hydo-world"
else
  bash "$TOP/hydo/crates/hydo-world/build.sh" "$REL/guest/hydo-world" >&2
fi
file -L "$REL/guest/hydo-world" | grep -q 'x86-64.*static' || { echo "guest/hydo-world is not a static x86_64 binary" >&2; exit 1; }
chmod 755 "$REL/guest/hydo-world"
# guest/hydo-read (also hydo-check): the ready-made store readers (hydo/crates/hydo-read, its own
# Cargo workspace; MANIFEST.md "Store readers"), staged by `hydo build` only for manifests that
# call them; its bytes depend on the kind of host that builds it (docs/BUILD-PINS.md "hydo-read")
"$TOP/hydo/crates/hydo-read/build.sh" x86_64 "$GB/read" >&2
cp "$GB/read/hydo-read" "$REL/guest/hydo-read"; chmod 755 "$REL/guest/hydo-read"
# guest/hydo-grpc: the generated drivers' static client (hydo/crates/hydo-grpc; the same bytes on a
# Mac and on Linux, docs/BUILD-PINS.md "The gRPC client"), copied into a project by hydo-grpc-gen
"$TOP/hydo/grpcgen/build-client.sh" --out "$GB/grpc" amd64 >&2
cp "$GB/grpc/hydo-grpc" "$REL/guest/hydo-grpc"; chmod 755 "$REL/guest/hydo-grpc"
# the guest programs built here must be the pinned builds of guest-programs.sha256, which bin/hydo
# carries and `hydo setup` checks (M154; hydo-read's bytes depend on the kind of building host)
( cd "$REL" && $SHA -c "$D/guest-programs.sha256" >/dev/null ) || {
  echo "a guest program built here is not its pinned build (guest-programs.sha256, docs/BUILD-PINS.md)" >&2
  ( cd "$REL" && $SHA -c "$D/guest-programs.sha256" 2>/dev/null | grep -v ': OK$' ) >&2; exit 1; }
# (assets.sha256, the asset pins, is carried by bin/hydo too: `hydo setup` checks every asset at
# its place in the release)
# the Redis example (hydo/onboard/examples/redis) with its binaries under files/
cp "$A/redis/redis-server" "$A/redis/redis-cli" "$REL/examples/redis/files/"
chmod 755 "$REL/examples/redis/files/"*
for y in hydo.yaml hydo-netem.yaml; do
  sed -e 's#{src: ~/hydo/build/redis-#{src: files/redis-#' \
      -e 's#Build on the build box:#Build (in this directory):#' \
      -e "s#^\\#   HYDO_SEEDRNG=.*#\\#   hydo build $y#" \
      "$TOP/hydo/onboard/examples/redis/$y" > "$REL/examples/redis/$y"
done
# the onboarding skill (an Agent Skills folder, agentskills.io): `hydo guide`, `hydo skill install`
# and `hydo init` read it from skill/hydo-onboard (bin/hydo also carries a built-in copy)
cp -R "$TOP/skills/hydo-onboard" "$REL/skill/"
cp "$D/install.sh" "$REL/install.sh"; chmod 755 "$REL/install.sh"
cp "$D/QUICKSTART-linux.md" "$REL/README.md"
echo "$VERSION" > "$REL/VERSION"
# the license notices (M158): LICENSE (hydo's Apache-2.0), licenses/ (the kernel module's GPL-2.0,
# the full license texts of every third-party component, from hydo/dist/licenses/, and of every
# Rust crate compiled into a program, from its registry source) and THIRD-PARTY.md, generated from
# third-party.json by hydo-release (hydo/crates/hydo-release; built here for this machine). It
# checks the list first, and after writing that every file of the release tree and every Nix
# store path in it belongs to a component; anything missing fails the build (docs/RELEASING.md)
( cd "$TOP" && cargo build --release --locked -q -p hydo-release ) >&2
HR=$TOP/target/release/hydo-release
CARGO=$(command -v cargo) "$HR" notices linux --repo "$TOP" --version "$VERSION" "$REL"
( cd "$REL" && find . -type f ! -name SHA256SUMS | LC_ALL=C sort | sed 's#^\./##' | xargs $SHA > SHA256SUMS )

# --- 3. the tarball (owner root, fixed order and file times, gzip without a name or time stamp,
# so two builds from the same inputs give the same bytes; the store's read-only modes are kept).
# GNU tar (Linux); BSD tar (a Mac) only keeps the owner and order, its file times are the build's.
TGZ=hydo-linux-$VERSION.tar.gz
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
  ( cd "$OUTDIR" && tar --format=gnu --sort=name --mtime=@1600000000 --owner=root:0 --group=root:0 \
      -cf - "hydo-linux-$VERSION" | gzip -n -9 > "$TGZ" )
else
  ( cd "$OUTDIR" && COPYFILE_DISABLE=1 tar --no-xattrs --uid 0 --gid 0 --uname root --gname root -czf "$TGZ" "hydo-linux-$VERSION" )
fi
ls -la "$OUTDIR/hydo-linux-$VERSION.tar.gz"
$SHA "$OUTDIR/hydo-linux-$VERSION.tar.gz"

# --- 4. the sources archive that accompanies the tarball (M158): the exact upstream source archives
# of every third-party component (pinned by sha256 in third-party.json), our patches,
# configurations and build recipes, the component list and the notices. The archives come from a
# sources cache: $HYDO_SOURCES_FROM (a directory with the files by name, for example a copy kept
# on the bare-metal machine), else hydo/dist/linux/sources/, filled once from the machine with
# the Nix store paths by hydo/dist/fetch-sources.sh. hydo-release writes it with fixed times,
# owners and order and gzip without a time stamp, so two builds give the same bytes.
SRC=${HYDO_SOURCES_FROM:-$D/sources}
CARGO=$(command -v cargo) "$HR" fetch-list linux --repo "$TOP" --cache "$SRC" > "$OUTDIR/.sources-missing.tsv"
if [ -s "$OUTDIR/.sources-missing.tsv" ]; then
  if [ -n "$HYDO_SOURCES_FROM" ]; then
    echo "$HYDO_SOURCES_FROM lacks these sources (or has other bytes):" >&2; cut -f1 "$OUTDIR/.sources-missing.tsv" >&2; exit 1
  fi
  mkdir -p "$SRC"; need_nix_machine
  echo "fetching $(wc -l < "$OUTDIR/.sources-missing.tsv" | tr -d ' ') source archives on the machine with the Nix store paths"
  ( cd "$OUTDIR" && cp "$TOP/hydo/dist/fetch-sources.sh" .fetch-sources.sh && tar cf - .fetch-sources.sh .sources-missing.tsv && rm .fetch-sources.sh ) | on_nix_machine "
    set -e
    command -v nix-store >/dev/null || . ~/hydo-nixb-work/nixenv.sh
    W=\$(mktemp -d /var/tmp/hydo-sources.XXXXXX); trap 'chmod -R u+w \$W; rm -rf \$W' EXIT
    cd \$W && tar xf - >&2
    sh .fetch-sources.sh .sources-missing.tsv out >&2
    cd out && tar cf - .
  " | ( cd "$SRC" && tar xf - )
fi
rm -f "$OUTDIR/.sources-missing.tsv"
CARGO=$(command -v cargo) "$HR" sources linux --repo "$TOP" --version "$VERSION" --cache "$SRC" \
  --out "$OUTDIR/hydo-linux-$VERSION-sources.tar.gz"
ls -la "$OUTDIR/hydo-linux-$VERSION-sources.tar.gz"
