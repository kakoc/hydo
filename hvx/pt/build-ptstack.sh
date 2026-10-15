#!/usr/bin/env bash
# Build a coverage-ready copy of the stack guest image: the same image as the
# assertion-enabled stack (build/sdk-disk.img on the box), with /bin/hydostack replaced by
# a build of the COMMITTED hvx/stack sources that keeps its symbols and DWARF and links its
# text at a unique address (0x2000001000), so an Intel PT trace filtered to that address
# range contains this program only (busybox and other static binaries also live at 0x400000).
#   output on the box: ~/hydo/build/pt-stack-disk.img and ~/hydo/hvxpt/pt/hydostack-pt
# Run from the Mac. Uses the build machine only for the pinned Go toolchain.
set -euo pipefail
BOX="${HVX_METAL:?}"
BUILD="${HYDO_REMOTE:?}"
REV="${PT_STACK_REV:-HEAD}"
TEXT="${PT_STACK_TEXT:-0x2000001000}"
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

git -C "$TOP" archive "$REV" hvx/stack | tar -x -C "$TMP"
ssh "$BUILD" 'rm -rf /tmp/ptstack-src && mkdir -p /tmp/ptstack-src'
rsync -az "$TMP/hvx/stack/" "$BUILD:/tmp/ptstack-src/"
ssh "$BUILD" "cd /tmp/ptstack-src && GO=/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go; \
  [ -x \$GO ] || GO=/nix/store/xrgn6m0c5vbz06467q06zdwnk6qqdl7z-go-1.26.5/bin/go; \
  GOPATH=\$HOME/hydo/build/stack-gopath GOMODCACHE=\$HOME/hydo/build/stack-gomod \
  GOCACHE=\$HOME/hydo/build/stack-gocache GOFLAGS=-mod=readonly GOTOOLCHAIN=local \
  CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
  \$GO build -trimpath -buildvcs=false -ldflags='-T $TEXT -buildid=' -o /tmp/hydostack-pt . && ls -la /tmp/hydostack-pt"
ssh -A "$BUILD" "rsync -az -e 'ssh -o BatchMode=yes' /tmp/hydostack-pt $BOX:hydo/hvxpt/pt/hydostack-pt"

# patch the image on the box: replace /bin/hydostack inside partition 1 (ext4, no
# metadata checksums, so debugfs can edit it in place)
ssh "$BOX" 'set -e; cd ~/hydo/build; cp --sparse=always sdk-disk.img pt-stack-disk.img
read P1 N1 < <(python3 -c "
import struct;m=open(\"pt-stack-disk.img\",\"rb\").read(512)
print(*struct.unpack_from(\"<II\",m,0x1BE+8))")
dd if=pt-stack-disk.img of=/tmp/pt-p1.img bs=512 skip=$P1 count=$N1 status=none
debugfs -w -R "rm /bin/hydostack" /tmp/pt-p1.img
debugfs -w -R "write /home/user/hydo/hvxpt/pt/hydostack-pt /bin/hydostack" /tmp/pt-p1.img
debugfs -w -R "set_inode_field /bin/hydostack mode 0100755" /tmp/pt-p1.img
debugfs -w -R "set_inode_field /bin/hydostack uid 0" /tmp/pt-p1.img
debugfs -w -R "set_inode_field /bin/hydostack gid 0" /tmp/pt-p1.img
e2fsck -fn /tmp/pt-p1.img | tail -1
dd if=/tmp/pt-p1.img of=pt-stack-disk.img bs=512 seek=$P1 conv=notrunc status=none
rm -f /tmp/pt-p1.img
readelf -lW ~/hydo/hvxpt/pt/hydostack-pt | grep -E "LOAD.*R E"
echo "built ~/hydo/build/pt-stack-disk.img"'
