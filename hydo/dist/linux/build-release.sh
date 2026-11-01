#!/bin/sh
# Build the self-contained x86_64 Linux release of hydo (see hydo/dist/linux/README.md):
#     hydo/dist/linux/build-release.sh [OUTDIR]        -> OUTDIR/hydo-linux-<version>/ and its .tar.gz
#
# Runs on any machine with sh, ssh, tar, sha256sum/shasum and cargo (the Mac works, with
# HYDO_RUST_BIN_DIR or a C cross compiler for the musl target). The
# guest and host-tool pieces are fetched once into hydo/dist/linux/assets/ (pinned by sha256 in
# assets.sha256) from $HYDO_BUILD_HOST, a Linux machine with the pinned nixpkgs store paths
# below (docs/BUILD-PINS.md; default the build machine):
#   guest/bzImage, guest/initramfs-kafka.cpio.gz   the guest kernel + its switch_root initramfs
#                                                  (~/hydo/build on the build box, build-linux.sh)
#   guest/busybox                                  static busybox 1.37.0 (every image's /bin)
#   guest/hvx_seedrng, guest/hydo_fc               static helpers, compiled there with the pinned
#                                                  musl gcc from hv/hvx/images/hvx_seedrng.c, hydo/onboard/hydo_fc.c
#   guest/modules/6.6.35/...                       the guest kernel's netem/u32/netfilter modules
#   guest/store.tar, guest/store-closures.txt      the iproute2 + iptables closures (tc, iptables-legacy
#                                                  inside the guest; "<root> <member>" lines, nix-store -qR)
#   tools/bin/{mke2fs,debugfs}, tools/lib/*        e2fsprogs 1.47.0 + libfaketime 0.9.10 with their
#                                                  glibc, run on the user's machine by libexec/faketime
#   redis/{redis-server,redis-cli}                 the Redis example's static binaries
# With HYDO_PIN=1 a missing assets.sha256 is written from the fetched files (once, by a maintainer).
# Once assets/ is filled the release builds offline. Users of the release need no Nix.
set -e
D=$(cd "$(dirname "$0")" && pwd)
TOP=$(cd "$D/../../.." && pwd)
VERSION=${HYDO_VERSION:-0.1.0}
OUTDIR=${1:-$D/out}
A=$D/assets
HOST=${HYDO_BUILD_HOST:?}
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

# --- 1. assets (fetched once, in one ssh session; the build host keeps nothing afterwards) ---
if [ ! -f "$A/.complete" ]; then
  [ -d "$A" ] && chmod -R u+w "$A" && rm -rf "$A"; mkdir -p "$A"
  echo "fetching the release assets from $HOST"
  ( cd "$TOP" && tar --no-xattrs -cf - hv/hvx/images/hvx_seedrng.c hydo/onboard/hydo_fc.c ) | ssh "$HOST" "
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
    # them with cp -a, so the image gets the store's modes; install.sh unpacks it (GNU tar sets a
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
# so install.sh compiles nothing for it), bin/hydo-build (the image builder with its own
# command line, for scripts; `hydo build` is the same builder with the release's tools) and
# bin/hydo-fetch-image (what fetch-images.sh of `hydo compose` calls), static x86_64 musl
# binaries. Built here with the workspace's pinned toolchain (rust-toolchain.toml; rustup adds
# the musl target; the TLS code of hydo-fetch-image compiles some C, with musl-gcc when present,
# else the host's cc), or taken from $HYDO_RUST_BIN_DIR (a directory with the four) when they
# were built elsewhere.
BINS="hydo hydo-hvx hydo-build hydo-fetch-image"
if [ -n "$HYDO_RUST_BIN_DIR" ]; then
  BD=$HYDO_RUST_BIN_DIR
else
  if [ -z "$CC_x86_64_unknown_linux_musl" ] && [ "$(uname -s)" = Linux ]; then
    CC_x86_64_unknown_linux_musl=$(command -v musl-gcc || command -v cc); export CC_x86_64_unknown_linux_musl
  fi
  ( cd "$TOP" && { rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true; } &&
    cargo build --release --locked -p hydo-cli -p hydo-hvx -p hydo-build -p hydo-compose --target x86_64-unknown-linux-musl ) >&2
  BD=$TOP/target/x86_64-unknown-linux-musl/release
fi
for b in $BINS; do
  file -L "$BD/$b" 2>/dev/null | grep -q 'x86-64.*static' || { echo "$BD/$b is not a static x86_64 binary" >&2; exit 1; }
  cp "$BD/$b" "$REL/bin/$b"; chmod 755 "$REL/bin/$b"
done
cp "$D/libexec/hvx-run" "$D/libexec/faketime" "$REL/libexec/"
cp "$D/libexec/bin/debugfs" "$REL/libexec/bin/"
chmod 755 "$REL/libexec/hvx-run" "$REL/libexec/faketime" "$REL/libexec/bin/debugfs"
# the format reference (`hydo guide manifest`) and hydo_fc.c, the source of guest/hydo_fc (the
# history recorder follows); the shell init and its extensions are gone since M136 (every image's
# first process is guest/hydo-agent)
cp "$TOP/hydo/onboard/hydo_fc.c" "$TOP/hydo/onboard/MANIFEST.md" "$REL/hydo/onboard/"
echo "$VERSION" > "$REL/hydo/VERSION"     # what a plan's hydo_version: is checked against
mkdir -p "$REL/hydo/history"
cp "$TOP/hydo/history/hydo_hist.sh" "$REL/hydo/history/"
# the property-table tool the explorer compiles on the fly, and the assertion protocol it reads
# (the property table is made by the explorer itself since M115: no hydo/assert tool in the release)
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
"$TOP/hydo/crates/hydo-agent/build.sh" x86_64 "$A/agent" >&2
cp "$A/agent/hydo-agent" "$REL/guest/hydo-agent"; chmod 755 "$REL/guest/hydo-agent"
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
( cd "$REL" && find . -type f ! -name SHA256SUMS | LC_ALL=C sort | sed 's#^\./##' | xargs $SHA > SHA256SUMS )

# --- 3. the tarball (owner root, fixed order; the store's read-only modes are kept) ---
( cd "$OUTDIR" && COPYFILE_DISABLE=1 tar --no-xattrs --uid 0 --gid 0 --uname root --gname root -czf "hydo-linux-$VERSION.tar.gz" "hydo-linux-$VERSION" 2>/dev/null \
  || tar --owner=0 --group=0 --sort=name -czf "hydo-linux-$VERSION.tar.gz" "hydo-linux-$VERSION" )
ls -la "$OUTDIR/hydo-linux-$VERSION.tar.gz"
$SHA "$OUTDIR/hydo-linux-$VERSION.tar.gz"
