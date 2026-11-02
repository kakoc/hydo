#!/usr/bin/env bash
# hydo/bench/agent-patch.sh -- make an agent image from a copy of an x86_64 shell image without
# rebuilding it (Milestone 129, docs/C2-BENCH.md). For images whose inputs only the build machine
# has (the four-service stack and the bug benchmark need Nix store paths).
#
#   agent-patch.sh <shell image> <agent image> [<hydo-agent binary>]
#
# What `hydo-build --control agent` does differently from a shell build (hydo/crates/hydo-build/
# src/agent.rs and x86.rs) is exactly two things, so the patch does exactly these to the copy's
# root file system (partition 1 at 1 MiB), with debugfs:
#   1. /sbin/init is the pinned hydo-agent (sha256 checked), mode 0755, owner 0:0;
#   2. /etc/hydo/control = "agent 1\n" (mode 0644, owner 0:0; /etc/hydo is created if missing).
# Everything else (the project's files, the manifest, manifest.sh, prelude.sh, the data disk)
# stays as the shell build staged it, as in an agent build. `--check` mode compares two images'
# root trees (contents, modes, owners, link targets): agent-patch.sh --check <a> <b>.
# `--swap` replaces one regular file of a copy, keeping its mode and owner (the benchmark's
# defects differ only in /bin/hydostack): agent-patch.sh --swap <image> <path> <file> <out image>.
set -euo pipefail
PIN=${HYDO_AGENT_PIN:-398f297e7425768a6175e58d27eca00d417fec25f0fb8b48842bcbbb53fdb8a6}   # hydo-agent 0.3.0 with the host-call port (docs/BUILD-PINS.md, M140; M127 b72c5caa…, M131 1f517e04…, 0.1.0 ff7652a1…)
OFF=1048576

tree_list() {   # <image> <dir>: rdump the root and list every entry with type, mode, owner, sha
  local img=$1 d=$2
  rm -rf "$d"; mkdir -p "$d"
  debugfs -R "rdump / $d" "$img?offset=$OFF" > /dev/null 2>&1
  # rdump keeps modes; owners come from debugfs itself (rdump as non-root gives our uid)
  ( cd "$d" && find . -mindepth 1 -printf '%y %m %P %l\n' | LC_ALL=C sort ) > "$d.list"
  ( cd "$d" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum ) > "$d.sha"
}

owners() {   # <image>: "<uid>:<gid> <path>" for every inode, by a recursive debugfs walk
  local img=$1
  python3 - "$img" "$OFF" <<'EOF' 2>/dev/null || echo "(owner walk needs python3)"
import subprocess, sys
img, off = sys.argv[1], sys.argv[2]
def ls(p):
    out = subprocess.run(["debugfs", "-R", f"ls -p {p}", f"{img}?offset={off}"],
                         capture_output=True, text=True).stdout
    for line in out.splitlines():
        f = line.strip().split("/")   # /<inode>/<mode>/<uid>/<gid>/<name>/<size>/
        if len(f) < 7 or f[5] in (".", ".."):
            continue
        yield f[5], f[2], f[3], f[4]
def walk(p):
    for name, mode, uid, gid in ls(p or "/"):
        q = f"{p}/{name}"
        print(f"{uid}:{gid} {mode} {q}")
        if mode.startswith("040"):
            walk(q)
walk("")
EOF
}

if [ "${1:-}" = --check ]; then
  a=$2; b=$3; w=${CHECK_DIR:-$(mktemp -d)}
  tree_list "$a" "$w/a"; tree_list "$b" "$w/b"
  owners "$a" | LC_ALL=C sort -k3 > "$w/a.own"; owners "$b" | LC_ALL=C sort -k3 > "$w/b.own"
  rc=0
  diff "$w/a.list" "$w/b.list" > "$w/list.diff" || rc=1
  diff "$w/a.sha" "$w/b.sha" > "$w/sha.diff" || rc=1
  diff "$w/a.own" "$w/b.own" > "$w/own.diff" || rc=1
  echo "entries: $(wc -l < "$w/a.list") vs $(wc -l < "$w/b.list"); files hashed: $(wc -l < "$w/a.sha") vs $(wc -l < "$w/b.sha")"
  for f in list sha own; do echo "== $f differences: $(wc -l < "$w/$f.diff") lines"; head -20 "$w/$f.diff"; done
  [ $rc = 0 ] && echo "SAME TREE" || echo "TREES DIFFER"
  chmod -R u+w "$w/a" "$w/b" 2>/dev/null || true   # rdump keeps the store's read-only modes
  rm -rf "$w/a" "$w/b"
  exit $rc
fi

if [ "${1:-}" = --swap ]; then
  img=$2; path=$3; file=$4; dst=$5
  st=$(debugfs -R "stat $path" "$img?offset=$OFF" 2>/dev/null)
  mode=$(printf '%s\n' "$st" | sed -n 's/.*Mode: *\([0-7]*\).*/\1/p' | head -1)
  uid=$(printf '%s\n' "$st" | sed -n 's/.*User: *\([0-9]*\).*/\1/p' | head -1)
  gid=$(printf '%s\n' "$st" | sed -n 's/.*Group: *\([0-9]*\).*/\1/p' | head -1)
  [ -n "$mode" ] && [ -n "$uid" ] && [ -n "$gid" ] || { echo "agent-patch: no regular file $path in $img" >&2; exit 1; }
  cp --sparse=always "$img" "$dst.tmp"
  # the replaced file keeps the image's own times (hydo-build: atime, ctime and crtime 0, mtime
  # the source's fixed one), and the superblock's write time is a fixed instant
  # (E2FSPROGS_FAKE_TIME), so the same swap gives the same bytes (M135; before, both carried the
  # wall clock)
  times=$(printf '%s\n' "$st" | sed -n 's/^ *\(ctime\|atime\|mtime\|crtime\): 0x\([0-9a-f]*\):.*/\1 \2/p')
  {
    printf 'rm %s\nwrite %s %s\nset_inode_field %s mode 0100%s\nset_inode_field %s uid %s\nset_inode_field %s gid %s\n' \
      "$path" "$file" "$path" "$path" "${mode: -3}" "$path" "$uid" "$path" "$gid"
    printf '%s\n' "$times" | while read -r f v; do [ -n "$f" ] && printf 'set_inode_field %s %s @%s\n' "$path" "$f" "$((16#$v))"; done
  } | E2FSPROGS_FAKE_TIME=${SWAP_FAKE_TIME:-1600000000} debugfs -w -f - "$dst.tmp?offset=$OFF" > /dev/null 2>&1
  e2fsck -fn "$dst.tmp?offset=$OFF" > /dev/null 2>&1 || { echo "agent-patch: e2fsck -fn found problems in $dst.tmp" >&2; exit 1; }
  debugfs -R "dump $path $dst.chk" "$dst.tmp?offset=$OFF" > /dev/null 2>&1
  cmp -s "$dst.chk" "$file" || { rm -f "$dst.chk"; echo "agent-patch: $path did not read back" >&2; exit 1; }
  rm -f "$dst.chk"; mv "$dst.tmp" "$dst"
  echo "agent-patch: $dst = $img with $path = $(basename "$file") ($(sha256sum < "$file" | cut -c1-16), mode ${mode: -3}, owner $uid:$gid)"
  exit 0
fi

src=$1; dst=$2; agent=${3:-${HYDO_AGENT_BIN:-$HOME/hydo-cw/build/hydo-agent}}
sha=$(sha256sum "$agent" | cut -d' ' -f1)
[ "$sha" = $PIN ] || { echo "agent-patch: $agent has sha256 $sha, not the pinned $PIN" >&2; exit 1; }
debugfs -R "cat /etc/hydo/control" "$src?offset=$OFF" 2>/dev/null | grep -q . && { echo "agent-patch: $src is already an agent image" >&2; exit 1; }
cp --sparse=always "$src" "$dst.tmp"
ctl=$(mktemp); printf 'agent 1\n' > "$ctl"
hasdir=$(debugfs -R "stat /etc/hydo" "$dst.tmp?offset=$OFF" 2>/dev/null | grep -c "^Inode:" || true)
{
  echo "rm /sbin/init"
  echo "write $agent /sbin/init"
  echo "set_inode_field /sbin/init mode 0100755"
  echo "set_inode_field /sbin/init uid 0"
  echo "set_inode_field /sbin/init gid 0"
  [ "$hasdir" = 1 ] || { echo "mkdir /etc/hydo"; echo "set_inode_field /etc/hydo mode 040755"; echo "set_inode_field /etc/hydo uid 0"; echo "set_inode_field /etc/hydo gid 0"; }
  echo "write $ctl /etc/hydo/control"
  echo "set_inode_field /etc/hydo/control mode 0100644"
  echo "set_inode_field /etc/hydo/control uid 0"
  echo "set_inode_field /etc/hydo/control gid 0"
} | debugfs -w -f - "$dst.tmp?offset=$OFF" > /dev/null 2>&1
rm -f "$ctl"
# the copy's file system must be clean, and the two files must read back
e2fsck -fn "$dst.tmp?offset=$OFF" > /dev/null 2>&1 || { echo "agent-patch: e2fsck -fn found problems in $dst.tmp" >&2; exit 1; }
got=$(debugfs -R "cat /etc/hydo/control" "$dst.tmp?offset=$OFF" 2>/dev/null)
[ "$got" = "agent 1" ] || { echo "agent-patch: /etc/hydo/control did not read back" >&2; exit 1; }
debugfs -R "dump /sbin/init $dst.init" "$dst.tmp?offset=$OFF" > /dev/null 2>&1
[ "$(sha256sum < "$dst.init" | cut -d' ' -f1)" = $PIN ] || { rm -f "$dst.init"; echo "agent-patch: /sbin/init did not read back as the agent" >&2; exit 1; }
rm -f "$dst.init"
mv "$dst.tmp" "$dst"
echo "agent-patch: $dst = $src with /sbin/init = hydo-agent (${PIN:0:16}) and /etc/hydo/control"
