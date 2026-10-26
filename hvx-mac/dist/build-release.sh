#!/bin/sh
# Build the self-contained Mac release of hydo (see hvx-mac/dist/README.md):
#     hvx-mac/dist/build-release.sh [OUTDIR]        -> OUTDIR/hydo-mac-<version>/ and its .tar.gz
#
# Runs on an Apple Silicon Mac. The guest pieces are arm64 Linux binaries; they are fetched once
# into hvx-mac/dist/assets/ (pinned by sha256 in assets.sha256), from:
#   - the arm64 kernel Image-explore: $HYDO_KERNEL, a file built by hvx-mac/explore/build-guest.sh
#     (with the HVC idle, the default);
#   - busybox, mke2fs and the Redis example's files: the aarch64-linux nixpkgs store paths below,
#     copied over ssh from $HYDO_BUILD_HOST (a Linux machine with nix; default the build machine).
# Once assets/ is filled, the release builds offline. Users of the release need no Linux machine.
set -e
D=$(cd "$(dirname "$0")" && pwd)
TOP=$(cd "$D/../.." && pwd)
VERSION=${HYDO_VERSION:-0.1.0}
OUTDIR=${1:-$D/out}
A=$D/assets
HOST=${HYDO_BUILD_HOST:?}
BUSYBOX=/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1/bin/busybox
MKE2FS=/nix/store/gnavnl9lh7lwi3kzvf0rzlw3zzplg32i-e2fsprogs-static-aarch64-unknown-linux-musl-1.47.0-bin/bin/mke2fs
LISTER=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
REDIS_MANIFEST=$TOP/hvx-mac/explore/examples/redis/hydo.yaml
TC=$(sed -n 's/^TC_ARM64 = "\(.*\)"$/\1/p' "$TOP/hvx/onboard/hydo_build_arm64.py")   # the pinned aarch64 tc

mkdir -p "$A"
fetch() {   # fetch <remote path> <local name>
  [ -f "$A/$2" ] && return 0
  mkdir -p "$(dirname "$A/$2")"
  echo "fetching $2 from $HOST"
  scp -q "$HOST:$1" "$A/$2"
}

# --- 1. assets ---
if [ ! -f "$A/Image-explore" ]; then
  [ -n "$HYDO_KERNEL" ] || { echo "set HYDO_KERNEL to an Image-explore built by hvx-mac/explore/build-guest.sh" >&2; exit 1; }
  cp "$HYDO_KERNEL" "$A/Image-explore"
fi
fetch "$BUSYBOX" busybox
# the busy-wait example's two static arm64 programs (hvx-mac/spin/spinvar.c, MODE=1 and MODE=5,
# built with the cross gcc in its header): $HYDO_SPIN_BIN is a directory holding both
if [ ! -f "$A/ledger-spin/spin_load" ]; then
  [ -n "$HYDO_SPIN_BIN" ] || { echo "set HYDO_SPIN_BIN to a directory with spin_load and spin_count (hvx-mac/spin/spinvar.c)" >&2; exit 1; }
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
if [ ! -d "$A/tc" ]; then   # tc and every file its loader opens (hvx-mac/services/elfdeps.py on $HOST)
  scp -q "$TOP/hvx-mac/services/elfdeps.py" "$HOST:/dev/shm/hydo-elfdeps.py"
  ssh "$HOST" "p=\$(ls -d /nix/store/*-python3-3.1*/bin/python3 | head -1); \$p /dev/shm/hydo-elfdeps.py $TC" > "$A/tc-closure.txt"
  echo "$TC" >> "$A/tc-closure.txt"
  mkdir -p "$A/tc"
  ssh "$HOST" "cd / && tar cf - $(sed 's#^/##' "$A/tc-closure.txt" | tr '\n' ' ')" | ( cd "$A/tc" && tar xf - )
  find "$A/tc" -type d -exec chmod u+w {} +
fi
( cd "$A" && shasum -a 256 -c "$D/assets.sha256" >/dev/null ) || {
  echo "assets do not match assets.sha256 (pinned); remove hvx-mac/dist/assets and fetch again" >&2
  ( cd "$A" && shasum -a 256 -c "$D/assets.sha256" | grep -v ': OK$' ) >&2; exit 1; }

# --- 2. the VMM binaries (signed again by install.sh on the user's Mac) ---
sh "$TOP/hvx-mac/build.sh" >/dev/null 2>&1
sh "$TOP/hvx-mac/explore/build.sh" >/dev/null 2>&1

# --- 3. the release tree ---
REL=$OUTDIR/hydo-mac-$VERSION
rm -rf "$REL"
mkdir -p "$REL/bin" "$REL/hvx/onboard" "$REL/hvx/explore" "$REL/hvx-mac/explore/out" "$REL/guest" \
         "$REL/examples" "$REL/skill"
cp "$D/hydo" "$REL/bin/hydo"; chmod 755 "$REL/bin/hydo"
# hydo-build splices the init extensions (liveness, recovery clock, links, freeze) in by manifest
cp "$TOP/hvx/onboard/hydo-build" "$TOP/hvx/onboard/hydo-compose" "$TOP/hvx/onboard/hydo_compose.py" "$TOP/hvx/onboard/hydo_oci.py" "$TOP/hvx/onboard/hydo_build_arm64.py" "$TOP"/hvx/onboard/hydo-init*.sh \
   "$TOP/hvx/onboard/hydo-liveness.sh" "$TOP/hvx/onboard/MANIFEST.md" "$TOP/hvx/onboard/hydo_composer.py" \
   "$TOP/hvx/onboard/hydo-composer.sh" "$REL/hvx/onboard/"
echo "$VERSION" > "$REL/hvx/VERSION"     # hydo_composer.engine_version: what a plan's hydo_version is checked against
cp "$TOP/hvx/explore/explore.py" "$TOP/hvx/explore/hvm_backend.py" "$TOP/hvx/explore/manifest_space.py" "$REL/hvx/explore/"
cp "$TOP/hvx/hydo_assert_report.c" "$TOP/hvx/hydo_assert_table.h" "$REL/hvx/"   # explore.py compiles the property table tool
cp "$TOP/hvx-mac/hvm" "$REL/hvx-mac/hvm"
cp "$TOP/hvx-mac/explore/hvmx" "$REL/hvx-mac/explore/hvmx"
cp "$TOP/hvx-mac/hv.entitlements" "$REL/hvx-mac/"
cp "$A/Image-explore" "$REL/hvx-mac/explore/out/Image-explore"
cp "$A/busybox" "$A/mke2fs" "$REL/guest/"; chmod 755 "$REL/guest/busybox" "$REL/guest/mke2fs"
cp "$A/busybox-applets.txt" "$A/tc-closure.txt" "$REL/guest/"
cp -R "$A/tc" "$REL/guest/tc"
printf '#!/bin/sh\n# stands in for `busybox --list` (hydo-build asks it for the applet set)\ncat "$(dirname "$0")/busybox-applets.txt"\n' > "$REL/guest/busybox-applets"
chmod 755 "$REL/guest/busybox-applets"
# the Redis example: the Mac manifest with every /nix/store src made relative to files/
mkdir -p "$REL/examples/redis"
cp -R "$A/redis" "$REL/examples/redis/files"
# an absolute /nix/store symlink would dangle on the Mac (hydo-build checks each src exists):
# point it at the same file inside files/, relatively, which resolves the same in the guest
python3 - "$REL/examples/redis/files" <<'EOF'
import os, sys
top = sys.argv[1]
for d, dirs, files in os.walk(top):
    for n in dirs + files:
        p = os.path.join(d, n)
        t = os.readlink(p) if os.path.islink(p) else ""
        if t.startswith("/nix/store/"):
            os.unlink(p)
            os.symlink(os.path.relpath(os.path.join(top, t.lstrip("/")), d), p)
EOF
sed 's#{src: /nix/store/#{src: files/nix/store/#' "$REDIS_MANIFEST" \
  | sed 's#^\# Build on the Linux box:.*#\# Build:  hydo build hydo.yaml#' > "$REL/examples/redis/hydo.yaml"
# the busy-wait example (the ledger with a user-space spin in every request)
cp -R "$TOP/hvx-mac/explore/examples/ledger-spin" "$REL/examples/ledger-spin"
mkdir -p "$REL/examples/ledger-spin/bin" && cp "$A/ledger-spin/spin_load" "$A/ledger-spin/spin_count" "$REL/examples/ledger-spin/bin/"
chmod 755 "$REL/examples/ledger-spin/bin/"*
sed 's#^\#   hvx/onboard/hydo-build .*#\#   hydo build hydo.yaml#' "$REL/examples/ledger-spin/hydo.yaml" > "$REL/examples/ledger-spin/hydo.yaml.new"
mv "$REL/examples/ledger-spin/hydo.yaml.new" "$REL/examples/ledger-spin/hydo.yaml"
# the onboarding skill (an Agent Skills folder, agentskills.io): `hydo guide`, `hydo skill install`
# and `hydo init` read it from skill/hydo-onboard (hvx/onboard/hydo_agents.py)
cp -R "$TOP/skills/hydo-onboard" "$REL/skill/"
cp "$TOP/hvx/onboard/hydo_agents.py" "$REL/hvx/onboard/"
cp "$D/install.sh" "$REL/install.sh"; chmod 755 "$REL/install.sh"
cp "$TOP/QUICKSTART-mac.md" "$REL/QUICKSTART-mac.md"
echo "$VERSION" > "$REL/VERSION"
( cd "$REL" && find . -type f ! -name SHA256SUMS | LC_ALL=C sort | xargs shasum -a 256 > SHA256SUMS )

# --- 4. the tarball ---
( cd "$OUTDIR" && COPYFILE_DISABLE=1 tar --uid 0 --gid 0 --uname root --gname wheel -czf "hydo-mac-$VERSION.tar.gz" "hydo-mac-$VERSION" )
ls -la "$OUTDIR/hydo-mac-$VERSION.tar.gz"
shasum -a 256 "$OUTDIR/hydo-mac-$VERSION.tar.gz"
