#!/bin/sh
# Build the self-contained Mac release of hydo (see hydo/dist/mac/README.md):
#     hydo/dist/mac/build-release.sh [OUTDIR]        -> OUTDIR/hydo-mac-<version>/ and its .tar.gz
#
# Runs on an Apple Silicon Mac. The guest pieces are arm64 Linux binaries; they are fetched once
# into hydo/dist/mac/assets/ (pinned by sha256 in assets.sha256), from:
#   - the arm64 kernel Image-explore: $HYDO_KERNEL, a file built by hv/hvm/explore/build-guest.sh
#     (with the HVC idle, the default);
#   - busybox, mke2fs and the Redis example's files: the aarch64-linux nixpkgs store paths below,
#     copied over ssh from $HYDO_BUILD_HOST (a Linux machine with nix; default the build machine).
# Once assets/ is filled, the release builds offline. Users of the release need no Linux machine.
set -e
D=$(cd "$(dirname "$0")" && pwd)
TOP=$(cd "$D/../../.." && pwd)
VERSION=${HYDO_VERSION:-0.1.0}
OUTDIR=${1:-$D/out}
A=$D/assets
HOST=${HYDO_BUILD_HOST:?}
BUSYBOX=/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1/bin/busybox
MKE2FS=/nix/store/gnavnl9lh7lwi3kzvf0rzlw3zzplg32i-e2fsprogs-static-aarch64-unknown-linux-musl-1.47.0-bin/bin/mke2fs
LISTER=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
REDIS_MANIFEST=$TOP/hydo/onboard/examples-mac/redis/hydo.yaml
TC=$(sed -n 's/^pub const TC_ARM64: &str = "\(.*\)";$/\1/p' "$TOP/hydo/crates/hydo-build/src/arm64.rs")   # the pinned aarch64 tc

mkdir -p "$A"
fetch() {   # fetch <remote path> <local name>
  [ -f "$A/$2" ] && return 0
  mkdir -p "$(dirname "$A/$2")"
  echo "fetching $2 from $HOST"
  scp -q "$HOST:$1" "$A/$2"
}

# --- 1. assets ---
if [ ! -f "$A/Image-explore" ]; then
  [ -n "$HYDO_KERNEL" ] || { echo "set HYDO_KERNEL to an Image-explore built by hv/hvm/explore/build-guest.sh" >&2; exit 1; }
  cp "$HYDO_KERNEL" "$A/Image-explore"
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
[ -f "$A/busybox-applets.txt" ] || ssh "$HOST" "$LISTER --list" > "$A/busybox-applets.txt"
if [ ! -d "$A/redis" ]; then   # tar keeps the libraries' symlinks, as hydo-build's copy does on Linux
  mkdir -p "$A/redis"
  srcs=$(sed -n 's/.*{src: \/\(nix\/store\/[^,]*\), dst:.*/\1/p' "$REDIS_MANIFEST" | tr '\n' ' ')
  echo "fetching the Redis example's files from $HOST"
  ssh "$HOST" "cd / && tar cf - $srcs" | ( cd "$A/redis" && tar xf - )
  find "$A/redis" -type d -exec chmod u+w {} +   # files keep the store's read-only modes, as on Linux
fi
if [ ! -d "$A/tc" ]; then   # tc and every file its loader opens (hydo-elfdeps, hydo/crates/hydo-tools, on $HOST)
  # a static x86_64 build (rust-lld, .cargo/config.toml), so $HOST needs nothing but /nix/store
  ( cd "$TOP" && cargo build --release --locked -p hydo-tools --no-default-features --bin hydo-elfdeps --target x86_64-unknown-linux-musl ) >&2
  scp -q "$TOP/target/x86_64-unknown-linux-musl/release/hydo-elfdeps" "$HOST:/dev/shm/hydo-elfdeps"
  ssh "$HOST" "chmod 755 /dev/shm/hydo-elfdeps && /dev/shm/hydo-elfdeps $TC" > "$A/tc-closure.txt"
  echo "$TC" >> "$A/tc-closure.txt"
  mkdir -p "$A/tc"
  ssh "$HOST" "cd / && tar cf - $(sed 's#^/##' "$A/tc-closure.txt" | tr '\n' ' ')" | ( cd "$A/tc" && tar xf - )
  find "$A/tc" -type d -exec chmod u+w {} +
fi
( cd "$A" && shasum -a 256 -c "$D/assets.sha256" >/dev/null ) || {
  echo "assets do not match assets.sha256 (pinned); remove hydo/dist/mac/assets and fetch again" >&2
  ( cd "$A" && shasum -a 256 -c "$D/assets.sha256" | grep -v ': OK$' ) >&2; exit 1; }

# --- 2. the VMM binaries (signed again by install.sh on the user's Mac) ---
sh "$TOP/hv/hvm/build.sh" >/dev/null 2>&1
sh "$TOP/hv/hvm/explore/build.sh" >/dev/null 2>&1

# --- 3. the release tree ---
REL=$OUTDIR/hydo-mac-$VERSION
rm -rf "$REL"
# the release tree mirrors the repository: hydo/ (the product) and hv/hvm/ (the Mac engine)
mkdir -p "$REL/bin" "$REL/hydo/onboard" "$REL/hydo/assert" "$REL/hv/hvm/explore/out" "$REL/guest" \
         "$REL/examples" "$REL/skill"
# the programs, all Rust (M107: no Python anywhere in the release): bin/hydo (the one CLI,
# hydo/crates/hydo-cli), bin/hydo-build (the image builder with its own command line, for
# scripts; `hydo build` is the same builder with the release's tools) and bin/hydo-fetch-image
# (what fetch-images.sh of `hydo compose` calls), for aarch64-apple-darwin, built with the
# workspace's pinned toolchain (rust-toolchain.toml), or taken from $HYDO_RUST_BIN_DIR
BINS="hydo hydo-build hydo-fetch-image"
if [ -n "$HYDO_RUST_BIN_DIR" ]; then
  BD=$HYDO_RUST_BIN_DIR
else
  ( cd "$TOP" && cargo build --release --locked -p hydo-cli -p hydo-build -p hydo-compose --target aarch64-apple-darwin ) >&2
  BD=$TOP/target/aarch64-apple-darwin/release
fi
for b in $BINS; do
  file "$BD/$b" | grep -q 'Mach-O 64-bit executable arm64' || { echo "$BD/$b is not an arm64 Mac binary" >&2; exit 1; }
  cp "$BD/$b" "$REL/bin/$b"; chmod 755 "$REL/bin/$b"
done
# the guest-side files hydo-build puts into images: the generic init and every extension it
# splices in by manifest, the test composer's guest runner, the history recorder (for a
# manifest with checks:), and the format reference (`hydo guide manifest`)
cp "$TOP"/hydo/onboard/hydo-init*.sh "$TOP/hydo/onboard/hydo-liveness.sh" "$TOP/hydo/onboard/MANIFEST.md" \
   "$TOP/hydo/onboard/hydo-composer.sh" "$REL/hydo/onboard/"
echo "$VERSION" > "$REL/hydo/VERSION"     # what a plan's hydo_version: is checked against
mkdir -p "$REL/hydo/history"
cp "$TOP/hydo/history/hydo_hist.sh" "$REL/hydo/history/"
cp "$TOP/hydo/assert/hydo_assert_report.c" "$TOP/hydo/assert/hydo_assert_table.h" "$REL/hydo/assert/"   # the explorer compiles the property table tool
cp "$TOP/hv/hvm/hvm" "$REL/hv/hvm/hvm"
cp "$TOP/hv/hvm/explore/hvmx" "$REL/hv/hvm/explore/hvmx"
cp "$TOP/hv/hvm/hv.entitlements" "$REL/hv/hvm/"
cp "$A/Image-explore" "$REL/hv/hvm/explore/out/Image-explore"
cp "$A/busybox" "$A/mke2fs" "$REL/guest/"; chmod 755 "$REL/guest/busybox" "$REL/guest/mke2fs"
# guest/hydo-world: the in-guest program of fakes: and proxies: (hydo/crates/hydo-world, Rust since
# M111), a static aarch64 musl build made here on the Mac; `hydo build` stages it
HYDO_WORLD_ARCH=arm64 bash "$TOP/hydo/crates/hydo-world/build.sh" "$REL/guest/hydo-world" >&2
chmod 755 "$REL/guest/hydo-world"
cp "$A/busybox-applets.txt" "$A/tc-closure.txt" "$REL/guest/"
cp -R "$A/tc" "$REL/guest/tc"
printf '#!/bin/sh\n# stands in for `busybox --list` (hydo-build asks it for the applet set)\ncat "$(dirname "$0")/busybox-applets.txt"\n' > "$REL/guest/busybox-applets"
chmod 755 "$REL/guest/busybox-applets"
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
( cd "$REL" && find . -type f ! -name SHA256SUMS | LC_ALL=C sort | xargs shasum -a 256 > SHA256SUMS )

# --- 4. the tarball ---
( cd "$OUTDIR" && COPYFILE_DISABLE=1 tar --uid 0 --gid 0 --uname root --gname wheel -czf "hydo-mac-$VERSION.tar.gz" "hydo-mac-$VERSION" )
ls -la "$OUTDIR/hydo-mac-$VERSION.tar.gz"
shasum -a 256 "$OUTDIR/hydo-mac-$VERSION.tar.gz"
