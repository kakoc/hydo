#!/bin/sh
# M116 experiment on the Mac: run gc_hvmx (build.sh) on the Redis OCI arm64 image with host input
# typed at exact exit counts and in reaction to a console line; print one line per run.
#   run-mac.sh <dir with Image-explore, explore-ob-redis-oci-arm64{.cpio.gz,-disk.img}> <out dir>
# Each configuration runs as a serve process with the same schedule twice (restored from S1) and
# as `one` (a fresh boot, no restore). Every value must repeat within a configuration.
set -u
IMG=$1; OUT=$2
B=$(cd "$(dirname "$0")" && pwd)/gc_hvmx
SCHED=${SCHED:-"disk=none | run K=redis@5+2"}
mkdir -p "$OUT"
export HVM_DISK="$IMG/explore-ob-redis-oci-arm64-disk.img" HVM_SNAP_DIR="$OUT/snap"
mkdir -p "$HVM_SNAP_DIR"
one() {   # <name> <env...>: serve twice + one
  name=$1; shift
  d="$OUT/$name"; mkdir -p "$d"
  printf 'RUN a %s\nRUN b %s\nQUIT\n' "$SCHED" "$SCHED" |
    env "$@" "$B" serve "$IMG/Image-explore" "$IMG/explore-ob-redis-oci-arm64.cpio.gz" "$d" > "$d/serve.out" 2> "$d/serve.err"
  env "$@" "$B" one "$IMG/Image-explore" "$IMG/explore-ob-redis-oci-arm64.cpio.gz" "$d" "$SCHED" > "$d/one.out" 2> "$d/one.err"
  fa=$(sed -n 's/^RESULT a fp=\([0-9a-f]*\).*/\1/p' "$d/serve.out")
  fb=$(sed -n 's/^RESULT b fp=\([0-9a-f]*\).*/\1/p' "$d/serve.out")
  fo=$(sed -n 's/^RESULT [^ ]* fp=\([0-9a-f]*\).*/\1/p' "$d/one.out")
  v=$(sed -n 's/^RESULT a .*verdict=\(.*\)/\1/p' "$d/serve.out" | cut -c1-60)
  puts=$(grep -c '^GC put' "$d/serve.err")
  same=DIFFER; [ -n "$fa" ] && [ "$fa" = "$fb" ] && [ "$fa" = "$fo" ] && same=SAME
  echo "$name serve-a=$fa serve-b=$fb one=$fo $same puts(serve)=$puts verdict=$v"
}
one base
one at500 GC_AT="500:hydo-agent-ping 1"
one at1500 GC_AT="1500:hydo-agent-ping 1"
one at3000 GC_AT="3000:hydo-agent-ping 1"
one at1500b GC_AT="1500:hydo-agent-ping 2"
one at1501 GC_AT="1501:hydo-agent-ping 1"
one two GC_AT="500:hydo-agent-ping 1;3000:hydo-agent-ping 2"
one react GC_REACT="KILLED redis@0:hydo-agent-ping 1"
one react200 GC_REACT="KILLED redis@200:hydo-agent-ping 1"
