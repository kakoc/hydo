#!/bin/sh
# Build the self-contained Mac release of hydo (see hydo/dist/mac/README.md):
#     hydo/dist/mac/build-release.sh [OUTDIR]        -> OUTDIR/hydo-mac-<version>/ and its .tar.gz,
#                                                       and OUTDIR/hydo-mac-<version>-sources.tar.gz
#                                                       (the corresponding source, M158; docs/RELEASING.md)
#
# Runs on an Apple Silicon Mac. The guest pieces are arm64 Linux binaries. The ones that are not
# built here from this tree are fetched once into hydo/dist/mac/assets/ (pinned by sha256 in
# assets.sha256), from:
#   - the arm64 kernel Image-explore: $HYDO_KERNEL, a file built by hv/hvm/explore/build-guest.sh
#     (with the HVC idle, the default);
#   - the second arm64 kernel Image-explore-netns (for network: bridge, addr: and the freeze):
#     $HYDO_KERNEL_NETNS, a file built by hv/hvm/explore/build-guest-netns.sh (docs/BUILD-PINS.md);
#   - the freeze helper hydo_fc: $HYDO_FC_BIN, hydo/onboard/hydo_fc.c compiled for aarch64 with the
#     pinned Arm GNU toolchain (docs/BUILD-PINS.md; the toolchain is an x86_64 Linux program, so on
#     a Mac it runs in an x86_64 Linux container);
#   - the aarch64 ip and iptables files (service networks and the freeze): the pinned nixpkgs store
#     paths of hydo/crates/hydo-build/src/arm64.rs, from the public Nix binary cache with
#     hydo-nix-fetch (hydo/crates/hydo-tools; it checks every archive's hash), so no Linux machine
#     is needed for them;
#   - busybox, mke2fs, the applet list, the tc closure and the Redis example's files: the
#     aarch64-linux nixpkgs store paths below, copied from a Linux machine with nix: the build
#     machine, or with HYDO_NIX_MACHINE=bare-metal the bare-metal machine, reached through
#     machines.local.sh (machines.sh at the repository root).
# Built from this tree on every run, with the workspace's pinned Rust toolchain: bin/hydo,
# bin/hydo-build, bin/hydo-fetch-image and bin/hydo-hvm (aarch64-apple-darwin, or taken from
# $HYDO_RUST_BIN_DIR), guest/hydo-agent (hydo-build checks its pinned sha256), guest/hydo-world,
# guest/hydo-world-tls, guest/hydo-read and guest/hydo-sql (each checked here against its pin in
# guest-programs.sha256, docs/BUILD-PINS.md).
# Once assets/ is filled, the release builds offline. Users of the release need no Linux machine.
# Two runs give the same tarball: every file's time is set to 0, the archive lists the files in a
# sorted order with no owner, extended attributes or ACLs, and gzip writes no name or time.
set -e
umask 022
D=$(cd "$(dirname "$0")" && pwd)
TOP=$(cd "$D/../../.." && pwd)
VERSION=${HYDO_VERSION:-0.3.2}
OUTDIR=${1:-$D/out}
mkdir -p "$OUTDIR"
OUTDIR=$(cd "$OUTDIR" && pwd)
A=$D/assets
HYDO_REPO=$TOP; . "$TOP/machines.sh"   # the machine with the store paths (only to fetch assets)
BUSYBOX=/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1/bin/busybox
MKE2FS=/nix/store/gnavnl9lh7lwi3kzvf0rzlw3zzplg32i-e2fsprogs-static-aarch64-unknown-linux-musl-1.47.0-bin/bin/mke2fs
LISTER=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
REDIS_MANIFEST=$TOP/hydo/onboard/examples-mac/redis/hydo.yaml
ARM64_RS=$TOP/hydo/crates/hydo-build/src/arm64.rs
TC=$(sed -n 's/^pub const TC_ARM64: &str = "\(.*\)";$/\1/p' "$ARM64_RS")   # the pinned aarch64 tc
IP=$(sed -n 's/^pub const IP_ARM64: &str = "\(.*\)";$/\1/p' "$ARM64_RS")   # the pinned aarch64 ip
IPT=$(sed -n 's/^pub const IPTABLES_ARM64: &str = "\(.*\)";$/\1/p' "$ARM64_RS")   # the pinned aarch64 iptables
# the iptables files hydo-build stages for the freeze (IPT_FILES in arm64.rs)
IPT_FILES="bin/xtables-legacy-multi lib/xtables/libxt_NFQUEUE.so lib/xtables/libxt_tcp.so lib/xtables/libxt_standard.so"

mkdir -p "$A"
fetch() {   # fetch <remote path> <local name>
  [ -f "$A/$2" ] && return 0
  mkdir -p "$(dirname "$A/$2")"
  need_nix_machine
  echo "fetching $2 from the machine with the Nix store paths"
  rsync -q -L "$NIX_MACHINE:$1" "$A/$2"
}

# --- 1. assets ---
if [ ! -f "$A/Image-explore" ]; then
  [ -n "$HYDO_KERNEL" ] || { echo "set HYDO_KERNEL to an Image-explore built by hv/hvm/explore/build-guest.sh" >&2; exit 1; }
  cp "$HYDO_KERNEL" "$A/Image-explore"
fi
if [ ! -f "$A/Image-explore-netns" ]; then
  [ -n "$HYDO_KERNEL_NETNS" ] || { echo "set HYDO_KERNEL_NETNS to an Image-explore-netns built by hv/hvm/explore/build-guest-netns.sh" >&2; exit 1; }
  cp "$HYDO_KERNEL_NETNS" "$A/Image-explore-netns"
fi
if [ ! -f "$A/hydo_fc" ]; then
  [ -n "$HYDO_FC_BIN" ] || { echo "set HYDO_FC_BIN to hydo/onboard/hydo_fc.c built for aarch64 with the pinned toolchain (docs/BUILD-PINS.md)" >&2; exit 1; }
  cp "$HYDO_FC_BIN" "$A/hydo_fc"
fi
fetch "$BUSYBOX" busybox
# the busy-wait example's two static arm64 programs (hv/hvm/spin/spinvar.c, MODE=1 and MODE=5,
# built with the cross gcc in its header): $HYDO_SPIN_BIN is a directory holding both
if [ ! -f "$A/ledger-spin/spin_load" ]; then
  [ -n "$HYDO_SPIN_BIN" ] || { echo "set HYDO_SPIN_BIN to a directory with spin_load and spin_count (hv/hvm/spin/spinvar.c)" >&2; exit 1; }
  mkdir -p "$A/ledger-spin" && cp "$HYDO_SPIN_BIN/spin_load" "$HYDO_SPIN_BIN/spin_count" "$A/ledger-spin/"
fi
fetch "$MKE2FS" mke2fs
# the applet set the Linux build of hydo-build --arch arm64 links (the x86 build of busybox's --list)
[ -f "$A/busybox-applets.txt" ] || { need_nix_machine; on_nix_machine "$LISTER --list"; } > "$A/busybox-applets.txt"
if [ ! -d "$A/redis" ]; then   # tar keeps the libraries' symlinks, as hydo-build's copy does on Linux
  mkdir -p "$A/redis"
  srcs=$(sed -n 's/.*{src: \/\(nix\/store\/[^,]*\), dst:.*/\1/p' "$REDIS_MANIFEST" | tr '\n' ' ')
  need_nix_machine
  echo "fetching the Redis example's files from the machine with the Nix store paths"
  on_nix_machine "cd / && tar cf - $srcs" | ( cd "$A/redis" && tar xf - )
  find "$A/redis" -type d -exec chmod u+w {} +   # files keep the store's read-only modes, as on Linux
fi
if [ ! -d "$A/tc" ]; then   # tc and every file its loader opens (hydo-elfdeps, hydo/crates/hydo-tools, on that machine)
  # a static x86_64 build (rust-lld, .cargo/config.toml), so that machine needs nothing but /nix/store
  ( cd "$TOP" && cargo build --release --locked -p hydo-tools --no-default-features --bin hydo-elfdeps --target x86_64-unknown-linux-musl ) >&2
  need_nix_machine
  rsync -q "$TOP/target/x86_64-unknown-linux-musl/release/hydo-elfdeps" "$NIX_MACHINE:/dev/shm/hydo-elfdeps"
  on_nix_machine "chmod 755 /dev/shm/hydo-elfdeps && /dev/shm/hydo-elfdeps $TC" > "$A/tc-closure.txt"
  echo "$TC" >> "$A/tc-closure.txt"
  mkdir -p "$A/tc"
  on_nix_machine "cd / && tar cf - $(sed 's#^/##' "$A/tc-closure.txt" | tr '\n' ' ')" | ( cd "$A/tc" && tar xf - )
  find "$A/tc" -type d -exec chmod u+w {} +
fi
if [ ! -d "$A/netns" ]; then   # ip, the iptables files and every file their loader opens, from the public cache
  ( cd "$TOP" && cargo build --release --locked -p hydo-tools --target aarch64-apple-darwin --bin hydo-nix-fetch --bin hydo-elfdeps ) >&2
  T=$TOP/target/aarch64-apple-darwin/release
  rm -rf "$A/netns-store"
  "$T/hydo-nix-fetch" --closure "$A/netns-store" "${IP%/bin/ip}" "$IPT" >&2
  paths=$IP
  for f in $IPT_FILES; do paths="$paths $IPT/$f"; done
  { "$T/hydo-elfdeps" --root "$A/netns-store" $paths; for p in $paths; do echo "$p"; done; } \
    | LC_ALL=C sort -u > "$A/netns-closure.txt"
  mkdir -p "$A/netns"
  ( cd "$A/netns-store" && tar cf - $(sed 's#^/##' "$A/netns-closure.txt" | tr '\n' ' ') ) | ( cd "$A/netns" && tar xf - )
  find "$A/netns" -type d -exec chmod u+w {} +
  chmod -R u+w "$A/netns-store" && rm -rf "$A/netns-store"
fi
( cd "$A" && shasum -a 256 -c "$D/assets.sha256" >/dev/null ) || {
  echo "assets do not match assets.sha256 (pinned); remove hydo/dist/mac/assets and fetch again" >&2
  ( cd "$A" && shasum -a 256 -c "$D/assets.sha256" | grep -v ': OK$' ) >&2; exit 1; }

# --- 2. the release tree ---
REL=$OUTDIR/hydo-mac-$VERSION
rm -rf "$REL"
# the release tree mirrors the repository: hydo/ (the product) and hv/hvm/ (the Mac engine)
mkdir -p "$REL/bin" "$REL/hydo/onboard" "$REL/hv/hvm/explore/out" "$REL/guest" \
         "$REL/examples" "$REL/skill"
# the programs, all Rust (no Python anywhere in the release since M107): bin/hydo (the one CLI,
# hydo/crates/hydo-cli), bin/hydo-build (the image builder with its own command line, for
# scripts; `hydo build` is the same builder with the release's tools), bin/hydo-fetch-image
# (what fetch-images.sh of `hydo compose` calls) and bin/hydo-hvm (the Mac engine, M143; the only
# one since M147; unsigned here; the published release carries
# its Developer ID signature, attached by sign-release.sh or attach-signatures.sh, M172, and `hydo setup` signs
# an unsigned one ad hoc on the user's Mac), for aarch64-apple-darwin, built with the workspace's
# pinned toolchain (rust-toolchain.toml), or taken from $HYDO_RUST_BIN_DIR
BINS="hydo hydo-build hydo-fetch-image hydo-hvm"
if [ -n "$HYDO_RUST_BIN_DIR" ]; then
  BD=$HYDO_RUST_BIN_DIR
else
  # the tree and the Cargo home are remapped to /hydo and /cargo (as the Linux release's programs
  # and the guest programs are built), so the programs' panic messages name no directory of the
  # building Mac (M154; until M153 they named the builder's home); both are found at run time
  CH=$(cd "${CARGO_HOME:-$HOME/.cargo}" 2>/dev/null && pwd || echo "${CARGO_HOME:-$HOME/.cargo}")
  ( cd "$TOP" && RUSTFLAGS="--remap-path-prefix=$TOP=/hydo --remap-path-prefix=$CH=/cargo" \
    cargo build --release --locked -p hydo -p hydo-build -p hydo-compose -p hydo-hvm --target aarch64-apple-darwin ) >&2
  BD=$TOP/target/aarch64-apple-darwin/release
fi
for b in $BINS; do
  file "$BD/$b" | grep -q 'Mach-O 64-bit executable arm64' || { echo "$BD/$b is not an arm64 Mac binary" >&2; exit 1; }
  cp "$BD/$b" "$REL/bin/$b"; chmod 755 "$REL/bin/$b"
done
# the format reference (`hydo guide manifest`) and the history recorder that hydo-build stages for
# a manifest with checks:; the shell init and its extensions are gone since M136 (every image's
# first process is guest/hydo-agent, driven by the host controller inside bin/hydo)
cp "$TOP/hydo/onboard/MANIFEST.md" "$REL/hydo/onboard/"
echo "$VERSION" > "$REL/hydo/VERSION"     # what a plan's hydo_version: is checked against
mkdir -p "$REL/hydo/history"
cp "$TOP/hydo/history/hydo_hist.sh" "$REL/hydo/history/"
# (the property table is made by the explorer itself since M115: no hydo/assert tool in the release)
cp "$TOP/hv/hvm/hv.entitlements" "$REL/hv/hvm/"
# both arm64 kernels: hydo-build records Image-explore-netns in an image that needs it
# (/etc/hydo/kernel) and the engine boots the kernel the image names
cp "$A/Image-explore" "$A/Image-explore-netns" "$REL/hv/hvm/explore/out/"
cp "$A/busybox" "$A/mke2fs" "$REL/guest/"; chmod 755 "$REL/guest/busybox" "$REL/guest/mke2fs"
# guest/hydo-world: the in-guest program of fakes: and proxies: (hydo/crates/hydo-world, Rust since
# M111), a static aarch64 musl build made here on the Mac; `hydo build` stages it
HYDO_WORLD_ARCH=arm64 bash "$TOP/hydo/crates/hydo-world/build.sh" "$REL/guest/hydo-world" >&2
chmod 755 "$REL/guest/hydo-world"
# guest/hydo-world-tls (since 0.3.0): hydo-world with the TLS front of `tls:` fakes and proxies
# (the cargo feature `tls`, M180), staged by `hydo build` instead of hydo-world only for manifests
# with `tls:`
HYDO_WORLD_TLS=1 HYDO_WORLD_ARCH=arm64 bash "$TOP/hydo/crates/hydo-world/build.sh" "$REL/guest/hydo-world-tls" >&2
chmod 755 "$REL/guest/hydo-world-tls"
cp "$A/busybox-applets.txt" "$A/tc-closure.txt" "$A/netns-closure.txt" "$REL/guest/"
cp -R "$A/tc" "$REL/guest/tc"
# ip and the iptables files go into the same tree: guest/tc is the Mac release's HYDO_ARM64_STORE,
# where hydo-build looks for the pinned aarch64 store paths (the files both closures share, glibc,
# libmnl, libxtables and the compression libraries, are the same files)
( cd "$A/netns" && tar cf - . ) | ( cd "$REL/guest/tc" && tar xf - )
printf '#!/bin/sh\n# stands in for `busybox --list` (hydo-build asks it for the applet set)\ncat "$(dirname "$0")/busybox-applets.txt"\n' > "$REL/guest/busybox-applets"
chmod 755 "$REL/guest/busybox-applets"
# the freeze helper hydo-build stages for `faults: [freeze]` (agent images freeze with the agent's
# own packet trigger, but the builder still stages it, AGENTS.md open item 30)
cp "$A/hydo_fc" "$REL/guest/hydo_fc"; chmod 755 "$REL/guest/hydo_fc"
# guest/hydo-read (hydo-read and, through a link, hydo-check, for manifests whose commands call
# them; M112): hydo/crates/hydo-read/build.sh, pure Rust, the pinned bytes on any Mac
bash "$TOP/hydo/crates/hydo-read/build.sh" aarch64 "$OUTDIR/.hydo-read" >&2
cp "$OUTDIR/.hydo-read/hydo-read-arm64" "$REL/guest/hydo-read"; chmod 755 "$REL/guest/hydo-read"
rm -rf "$OUTDIR/.hydo-read"
# guest/hydo-sql (since 0.3.0): the ready-made SQL workloads and checks of `hydo sql-gen`
# (hydo/crates/hydo-sql, its own Cargo workspace, M182), staged by `hydo build` only for
# manifests whose commands call it; pure Rust, the pinned bytes on any Mac
bash "$TOP/hydo/crates/hydo-sql/build.sh" aarch64 "$OUTDIR/.hydo-sql" >&2
cp "$OUTDIR/.hydo-sql/hydo-sql-arm64" "$REL/guest/hydo-sql"; chmod 755 "$REL/guest/hydo-sql"
rm -rf "$OUTDIR/.hydo-sql"
# the guest agent, every image's first process (M136; driven by the host controller, M126): the
# aarch64 build of hydo/crates/hydo-agent/build.sh (byte-reproducible, no C toolchain), as
# guest/hydo-agent (the CLI's default HYDO_AGENT_BIN_ARM64); hydo-build checks its pinned sha256
"$TOP/hydo/crates/hydo-agent/build.sh" aarch64 "$OUTDIR/.hydo-agent" >&2
cp "$OUTDIR/.hydo-agent/hydo-agent-arm64" "$REL/guest/hydo-agent"; chmod 755 "$REL/guest/hydo-agent"
rm -rf "$OUTDIR/.hydo-agent"
# the guest programs built here must be the pinned builds of guest-programs.sha256, which bin/hydo
# carries and `hydo setup` checks (M154; until then this script checked hydo-read's pin alone)
( cd "$REL" && shasum -a 256 -c "$D/guest-programs.sha256" >/dev/null ) || {
  echo "a guest program built here is not its pinned build (guest-programs.sha256, docs/BUILD-PINS.md)" >&2
  ( cd "$REL" && shasum -a 256 -c "$D/guest-programs.sha256" 2>/dev/null | grep -v ': OK$' ) >&2; exit 1; }
# the Redis example: the Mac manifest with every /nix/store src made relative to files/
mkdir -p "$REL/examples/redis"
cp -R "$A/redis" "$REL/examples/redis/files"
# an absolute /nix/store symlink would dangle on the Mac (hydo-build checks each src exists):
# point it at the same file inside files/, relatively, which resolves the same in the guest
# (the relative path is os.path.relpath's: up to the common directory, then down)
relpath() {   # relpath <target> <from dir>: both absolute and normalised
  t=$1; c=$2; up=
  while [ "$c" != / ] && [ "${t#"$c"/}" = "$t" ]; do c=$(dirname "$c"); up="../$up"; done
  echo "$up${t#"$c"/}"
}
top=$REL/examples/redis/files
find "$top" -type l | while read -r p; do
  t=$(readlink "$p")
  case "$t" in
    /nix/store/*) rm "$p"; ln -s "$(relpath "$top/${t#/}" "$(dirname "$p")")" "$p" ;;
  esac
done
sed 's#{src: /nix/store/#{src: files/nix/store/#' "$REDIS_MANIFEST" \
  | sed 's#^\# Build on the Linux box:.*#\# Build:  hydo build hydo.yaml#' > "$REL/examples/redis/hydo.yaml"
# the busy-wait example (the ledger with a user-space spin in every request)
cp -R "$TOP/hydo/onboard/examples-mac/ledger-spin" "$REL/examples/ledger-spin"
mkdir -p "$REL/examples/ledger-spin/bin" && cp "$A/ledger-spin/spin_load" "$A/ledger-spin/spin_count" "$REL/examples/ledger-spin/bin/"
chmod 755 "$REL/examples/ledger-spin/bin/"*
sed 's#^\#   hvx/onboard/hydo-build .*#\#   hydo build hydo.yaml#' "$REL/examples/ledger-spin/hydo.yaml" > "$REL/examples/ledger-spin/hydo.yaml.new"
mv "$REL/examples/ledger-spin/hydo.yaml.new" "$REL/examples/ledger-spin/hydo.yaml"
# the onboarding skill (an Agent Skills folder, agentskills.io): `hydo guide`, `hydo skill install`
# and `hydo init` read it from skill/hydo-onboard (bin/hydo also carries a built-in copy)
cp -R "$TOP/skills/hydo-onboard" "$REL/skill/"
cp "$D/install.sh" "$REL/install.sh"; chmod 755 "$REL/install.sh"
cp "$D/QUICKSTART-mac.md" "$REL/QUICKSTART-mac.md"
echo "$VERSION" > "$REL/VERSION"
# the license notices (M158): LICENSE (hydo's Apache-2.0), licenses/ (the full license texts of
# every third-party component, from hydo/dist/licenses/, and of every Rust crate compiled into a
# program, from its registry source) and THIRD-PARTY.md, generated from third-party.json by
# hydo-release (hydo/crates/hydo-release; built here for this Mac). It checks the list first, and
# after writing that every file of the release tree and every Nix store path in it belongs to a
# component; anything missing fails the build (docs/RELEASING.md)
( cd "$TOP" && cargo build --release --locked -q -p hydo-release ) >&2
HR=$TOP/target/release/hydo-release
CARGO=$(command -v cargo) "$HR" notices mac --repo "$TOP" --version "$VERSION" "$REL"

# --- 3. the tarball (the same bytes from the same inputs): SHA256SUMS, no extended attributes,
# every time 0, sorted, no owners, gzip without a name or time (pack-release.sh, which
# attach-signatures.sh also uses to pack the signed release; docs/design/signing.md). The programs
# are unsigned here (the linker's ad-hoc signature): the published tarball is this one with the
# Developer ID signatures attached (sign-release.sh, Milestone 172)
sh "$D/pack-release.sh" "$REL"

# --- 4. the sources archive that accompanies the tarball (M158): the exact upstream source archives
# of every third-party component (pinned by sha256 in third-party.json; among them the Arm GNU
# Toolchain's source snapshot, whose glibc and libgcc are in guest/hydo_fc), our kernel patches,
# configurations and build recipes, the component list and the notices. The archives come from a
# sources cache: $HYDO_SOURCES_FROM (a directory with the files by name), else
# hydo/dist/mac/sources/, filled once from the machine with the Nix store paths by
# hydo/dist/fetch-sources.sh. hydo-release writes it with fixed times, owners and order and gzip
# without a time stamp, so two builds give the same bytes (and the same as on Linux).
SRC=${HYDO_SOURCES_FROM:-$D/sources}
CARGO=$(command -v cargo) "$HR" fetch-list mac --repo "$TOP" --cache "$SRC" > "$OUTDIR/.sources-missing.tsv"
if [ -s "$OUTDIR/.sources-missing.tsv" ]; then
  if [ -n "$HYDO_SOURCES_FROM" ]; then
    echo "$HYDO_SOURCES_FROM lacks these sources (or has other bytes):" >&2; cut -f1 "$OUTDIR/.sources-missing.tsv" >&2; exit 1
  fi
  mkdir -p "$SRC"; need_nix_machine
  echo "fetching $(wc -l < "$OUTDIR/.sources-missing.tsv" | tr -d ' ') source archives on the machine with the Nix store paths"
  ( cd "$OUTDIR" && cp "$TOP/hydo/dist/fetch-sources.sh" .fetch-sources.sh && COPYFILE_DISABLE=1 tar --no-xattrs -cf - .fetch-sources.sh .sources-missing.tsv && rm .fetch-sources.sh ) | on_nix_machine "
    set -e
    command -v nix-store >/dev/null || . ~/hydo-nixb-work/nixenv.sh
    W=\$(mktemp -d /var/tmp/hydo-sources.XXXXXX); trap 'chmod -R u+w \$W; rm -rf \$W' EXIT
    cd \$W && tar xf - >&2
    sh .fetch-sources.sh .sources-missing.tsv out >&2
    cd out && tar cf - .
  " | ( cd "$SRC" && tar xf - )
fi
rm -f "$OUTDIR/.sources-missing.tsv"
CARGO=$(command -v cargo) "$HR" sources mac --repo "$TOP" --version "$VERSION" --cache "$SRC" \
  --out "$OUTDIR/hydo-mac-$VERSION-sources.tar.gz"
ls -la "$OUTDIR/hydo-mac-$VERSION-sources.tar.gz"
