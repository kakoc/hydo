#!/bin/bash
# M127 (docs/C2-LINUX.md, "Agent images made from shell images"): make an agent image from an
# x86_64 shell-init image, for examples whose inputs only the Nix build machine can stage (the
# stack with MySQL, Kafka and the JDK, etcd, the shop, the gRPC stack, ...). It changes exactly
# what `hydo build --control agent` changes (hydo-build: x86.rs, agent.rs, init.rs install_init):
#   /sbin/init              the pinned hydo-agent (mode 0755, owner 0:0) instead of the shell init
#   /etc/hydo/control       "agent 1\n" (mode 0644, owner 0:0), new
#   /etc/hydo/init-links.sh removed (shell images of manifests with links only)
# with hydo-build's timestamps (mtime FIXED_MTIME, atime, ctime and crtime 0) on the two files and
# their directories. The data partition is not touched. The result passes `e2fsck -fn`.
# Compared with a real `--control agent` build of the same inputs, the file trees are the same
# (names, links, modes, owners, sizes, contents); the inode numbers of /etc/hydo/control and of
# the entries after it in mke2fs's walk order, the blocks the new files occupy and the
# superblock's lifetime-writes counter differ.
#
#   patch-agent.sh <shell disk.img> <agent disk.img>
# Environment: HYDO_RELEASE (an installed Linux release: its libexec/faketime, tools/bin/debugfs
# and libexec/bin/debugfs are used), HYDO_AGENT_BIN (default build/hydo-agent; must be the pinned
# agent, checked against hydo-build's AGENT_SHA256_X86_64 unless HYDO_AGENT_UNPINNED=1), E2FSCK
# (default e2fsck). The changes are written under the release's faketime at hydo-build's frozen
# clock, then one pass with the clock at 0, so the
# superblock's write time stays 0 as in a built image, and two patches of one image give the same
# bytes.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
SRC=$1; DST=$2
AG=${HYDO_AGENT_BIN:-$ROOT/build/hydo-agent}
REL=${HYDO_RELEASE:?set HYDO_RELEASE to an installed Linux release}
DBG=$REL/libexec/bin/debugfs
FSCK=${E2FSCK:-e2fsck}
OFF=1048576   # hydo_engine::image::ROOT_OFFSET: the root partition starts at sector 2048
MT=$(awk '/FIXED_MTIME: i64 =/{gsub(/[;_]/,"",$NF); print $NF}' "$ROOT/hydo/crates/hydo-build/src/fsx.rs")
PIN=$(awk -F'"' '/AGENT_SHA256_X86_64: &str =/{getline; print $2}' "$ROOT/hydo/crates/hydo-build/src/agent.rs")
[ -n "$MT" ] && [ -n "$PIN" ] || { echo "patch-agent: cannot read FIXED_MTIME or the agent pin" >&2; exit 1; }
SHA=$(sha256sum "$AG" | cut -d' ' -f1)
if [ "$SHA" != "$PIN" ] && [ "${HYDO_AGENT_UNPINNED:-}" != 1 ]; then
  echo "patch-agent: $AG has sha256 $SHA, the pinned agent is $PIN (HYDO_AGENT_UNPINNED=1 stages it anyway)" >&2
  exit 1
fi
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
printf 'agent 1\n' > "$TMP/control"
cp --sparse=always "$SRC" "$DST"
FS="$DST?offset=$OFF"
links=$("$DBG" -R "stat /etc/hydo/init-links.sh" "$FS" 2>&1 | grep -c "^Inode:" || true)
{
  echo "rm /sbin/init"
  echo "write $AG /sbin/init"
  echo "sif /sbin/init mode 0100755"
  echo "write $TMP/control /etc/hydo/control"
  echo "sif /etc/hydo/control mode 0100644"
  for f in /sbin/init /etc/hydo/control; do
    echo "sif $f uid 0"; echo "sif $f gid 0"; echo "sif $f mtime $MT"
    for t in atime ctime crtime atime_extra mtime_extra ctime_extra crtime_extra; do echo "sif $f $t 0"; done
  done
  [ "$links" = 1 ] && echo "rm /etc/hydo/init-links.sh"
  for d in /sbin /etc/hydo; do echo "sif $d mtime $MT"; echo "sif $d atime 0"; echo "sif $d ctime 0"; done
} > "$TMP/cmds"
# the changes under hydo-build's frozen clock (a removed inode's dtime must not be 0, or e2fsck
# reports it), then one more write pass with the clock at 0, so the superblock's write time is 0
TZ=UTC-4 "$REL/libexec/faketime" "2020-09-13 16:26:40" "$REL/tools/bin/debugfs" -w -f "$TMP/cmds" "$FS" > "$TMP/log" 2>&1 \
  || { cat "$TMP/log" >&2; exit 1; }
echo "ssv mtime 0" > "$TMP/cmds0"
TZ=UTC-4 "$REL/libexec/faketime" "1970-01-01 04:00:00" "$REL/tools/bin/debugfs" -w -f "$TMP/cmds0" "$FS" >> "$TMP/log" 2>&1 \
  || { cat "$TMP/log" >&2; exit 1; }
if grep -v '^debugfs' "$TMP/log" | grep -q -i -E "error|not found|cannot"; then cat "$TMP/log" >&2; exit 1; fi
size=$("$DBG" -R stats "$FS" 2>/dev/null | awk '/^Block count:/{c=$3} /^Block size:/{s=$3} END{print c*s}')
dd if="$DST" of="$TMP/root.img" bs=1M skip=1 count=$((size / 1048576)) status=none
"$FSCK" -fn "$TMP/root.img" > "$TMP/fsck" 2>&1 || { tail -5 "$TMP/fsck" >&2; exit 1; }
echo "patch-agent: $DST: /sbin/init = hydo-agent ${SHA:0:16}, /etc/hydo/control = $("$DBG" -R 'cat /etc/hydo/control' "$FS" 2>/dev/null), init-links.sh removed: $links, e2fsck clean"
