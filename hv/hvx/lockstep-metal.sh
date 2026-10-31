#!/usr/bin/env bash
#
# lockstep-metal.sh -- the lock-step control check of milestone 118 on the bare-metal test machine
# (run FROM THE MAC; docs/design/guest-control.md, hvx_explore.c's header "Lock-step control").
#
#   hv/hvx/lockstep-metal.sh check
#       the M116 feasibility experiments as a test: bytes typed into the serial input at exact
#       icounts, in reaction to a console line, before and after a mid-run snapshot; each twice
#       from S1, from a fresh boot and from the snapshot; every value must equal the recorded one
#       (hydo/crates/hydo-engine/examples/lockstep_check.rs). Prints "lockstep_check: ALL PASS".
#   hv/hvx/lockstep-metal.sh cost <image> "<schedule>" <rb>...
#       the cost of stopping: the schedule as a plain RUN and as a CTL run whose host answers only
#       CONT, once per cadence (rb=<n> added; 0 = none), on <image> in ~/hydo/build.
#
# It syncs the engine runner (hvx_explore.c and the userspace files it includes; never hvx.c or
# hvx_abi.h) and the Rust workspace to ~/$HVX_METAL_TREE on the box, builds hvx_explore there
# against the box tree's hvx_abi.h and the check program with the box's Rust
# (~/hydo-rust/env.sh), waits while /var/tmp/hydo-QUIET exists, and runs every VM through
# ~/bin/hvx-run (the shared module lock). No module is loaded or reloaded.
# Env: HVX_METAL (the bare-metal machine); HVX_METAL_TREE (default hydo; a session's own tree, for
# example hydo-ls, keeps the main tree's binaries untouched); OUT (default
# /var/tmp/lockstep-<mode>-<date>-<time>, a new directory per run).
set -euo pipefail

BOX="${HVX_METAL:?}"
TREE="${HVX_METAL_TREE:-hydo}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MODE="${1:-check}"
[ $# -gt 0 ] && shift
case "$MODE" in
  check) IMG=explore-ob-redis-nq-disk.img;;
  cost) [ $# -ge 3 ] || { echo "usage: $0 cost <image> \"<schedule>\" <rb>..." >&2; exit 2; }
        IMG=$1; shift;;
  *) echo "usage: $0 check | cost <image> \"<schedule>\" <rb>..." >&2; exit 2;;
esac
OUT="${OUT:-/var/tmp/lockstep-$MODE-$(date +%Y%m%d-%H%M%S)}"
ARGS=""
for a in "$@"; do ARGS="$ARGS $(printf '%q' "$a")"; done

echo ">> syncing the engine runner and the Rust workspace to $BOX:$TREE/"
ssh -o BatchMode=yes "$BOX" "mkdir -p $TREE/hv/hvx"
(cd "$ROOT" && rsync -azR --exclude target --exclude hv/hvx/hvx.c --exclude hv/hvx/hvx_abi.h \
  Cargo.toml Cargo.lock rust-toolchain.toml .cargo hydo/crates \
  hv/hvx/hvx_explore.c hv/hvx/hvx_stack_verify.c hv/hvx/hvx_boot.h hv/hvx/hvx_sched_util.h hv/hvx/ptedges.h \
  "$BOX:$TREE/")

ssh -o BatchMode=yes "$BOX" "TREE=$TREE MODE=$MODE IMG=$IMG OUT=$OUT bash -s -- $ARGS" <<'REMOTE'
set -euo pipefail
cd ~/"$TREE"
[ -f hv/hvx/hvx_abi.h ] || { echo "~/$TREE/hv/hvx/hvx_abi.h is missing (the box tree's module ABI)" >&2; exit 1; }
(cd hv/hvx && cc -O2 -Wall -o hvx_explore hvx_explore.c -lipt)
. ~/hydo-rust/env.sh
cargo build --release --locked -q -p hydo-engine --example lockstep_check
n=0
while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 1800 ]; do sleep 5; n=$((n + 5)); done
B=~/hydo/build
mkdir -p "$OUT"
echo ">> lockstep_check $MODE on $IMG, results in $OUT"
timeout 7200 target/release/examples/lockstep_check "$MODE" "$HOME/$TREE/hv/hvx/hvx_explore" ~/bin/hvx-run \
  "$B/bzImage" "$B/initramfs-kafka.cpio.gz" "$B/$IMG" "$OUT" "$@" 2>&1 | tee "$OUT/out.txt"
exit "${PIPESTATUS[0]}"
REMOTE
