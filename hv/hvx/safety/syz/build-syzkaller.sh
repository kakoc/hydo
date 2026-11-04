#!/usr/bin/env bash
# Build syzkaller with the /dev/hvx description (Milestone 97). Runs on the bare-metal machine or
# the build machine (it only builds). Output: $SAFETY/syzkaller/bin/{syz-manager,
# linux_amd64/syz-executor,...}.
# Pins: syzkaller commit SYZ_COMMIT, Go GO_VERSION (official tarball by sha256, unpacked in
# $SAFETY/go, not system-wide). Steps: copy dev_hvx.txt, the generated dev_hvx.txt.const and
# the executor extension common_ext.h into the syzkaller tree, `make descriptions`, check every
# ioctl argument size against hvx_abi.h (check-sizes.go), then `make` for linux/amd64.
set -euo pipefail
SAFETY=${SAFETY:-$HOME/hydo-safety}
HERE="$(cd "$(dirname "$0")" && pwd)"
HVX="$(cd "$HERE/../.." && pwd)"
. "$HERE/../lib.sh"
SYZ_COMMIT=b498adee94342d7bea3b7374b41f30408604a247	# 2026-10-29
GO_VERSION=1.26.5
GO_SHA=5c2c3b16caefa1d968a94c1daca04a7ca301a496d9b086e17ad77bb81393f053
JOBS=${JOBS:-6}

mkdir -p "$SAFETY/dl"
if [ ! -x "$SAFETY/go/bin/go" ]; then
  T="$SAFETY/dl/go$GO_VERSION.linux-amd64.tar.gz"
  [ -f "$T" ] || curl -sfL -o "$T" "https://go.dev/dl/go$GO_VERSION.linux-amd64.tar.gz"
  echo "$GO_SHA  $T" | sha256sum -c --quiet
  rm -rf "$SAFETY/go"; tar -xzf "$T" -C "$SAFETY"
fi
S="$SAFETY/syzkaller"
[ -d "$S/.git" ] || git clone -q https://github.com/google/syzkaller.git "$S"
git -C "$S" fetch -q origin "$SYZ_COMMIT" 2>/dev/null || true
git -C "$S" checkout -q -f "$SYZ_COMMIT"
git -C "$S" clean -qfd sys/linux tools executor

# the description, its constants (from the real header), the executor extension
cp "$HERE/dev_hvx.txt" "$S/sys/linux/dev_hvx.txt"
cc -I"$HVX" -o "$SAFETY/dl/gen-const" "$HERE/gen-const.c"
"$SAFETY/dl/gen-const" > "$S/sys/linux/dev_hvx.txt.const"
cp "$HERE/common_ext.h" "$S/executor/common_ext.h"
mkdir -p "$S/tools/hvx-check-sizes"
cp "$HERE/check-sizes.go" "$S/tools/hvx-check-sizes/main.go"

cat > "$S/hydo-build.sh" <<EOF
set -euo pipefail
export PATH="$SAFETY/go/bin:\$PATH" GOPATH="$SAFETY/gopath" GOCACHE="$SAFETY/gocache" GOMAXPROCS=$JOBS
cd '$S'
make descriptions
go run ./tools/hvx-check-sizes
make -j$JOBS TARGETOS=linux TARGETARCH=amd64
EOF
safety_build "$S/hydo-build.sh" "$SAFETY/syzbuild.log" gnumake gcc glibc.static git
grep -a 'hvx-check-sizes' "$SAFETY/syzbuild.log"
ls -la "$S/bin/syz-manager" "$S/bin/linux_amd64/syz-executor"
echo "build-syzkaller: OK ($SYZ_COMMIT, go $GO_VERSION)"
