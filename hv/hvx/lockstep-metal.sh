#!/usr/bin/env bash
#
# lockstep-metal.sh -- the lock-step control check of milestone 118 on the bare-metal test machine
# (run FROM THE MAC; docs/design/guest-control.md "The channel"; the protocol is described in
# hydo/crates/hydo-engine/src/lockstep.rs and implemented by the Rust engine, hydo/crates/hydo-hvx).
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
# It syncs the Rust workspace (never hvx.c or hvx_abi.h) to ~/$HVX_METAL_TREE on the box, builds
# the check program and the engine program hydo-hvx there with the box's Rust
# (~/hydo-rust/env.sh), waits while /var/tmp/hydo-QUIET exists, and runs the check program as a
# whole under ~/bin/hvx-run sudo (the shared module lock), so the engine runs in its process
# (M137; ENGINE=process runs the hydo-hvx program per VM instead). No module is loaded or reloaded.
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

echo ">> syncing the Rust workspace to $BOX:$TREE/"
ssh -o BatchMode=yes "$BOX" "mkdir -p $TREE"
(cd "$ROOT" && rsync -azR --exclude target Cargo.toml Cargo.lock rust-toolchain.toml .cargo hydo/crates "$BOX:$TREE/")

ssh -o BatchMode=yes "$BOX" "TREE=$TREE MODE=$MODE IMG=$IMG OUT=$OUT ENGINE=${ENGINE:-inproc} bash -s -- $ARGS" <<'REMOTE'
set -euo pipefail
cd ~/"$TREE"
. ~/hydo-rust/env.sh
cargo build --release --locked -q -p hydo-hvx -p hydo-engine --bin hydo-hvx --example lockstep_check
n=0
while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 1800 ]; do sleep 5; n=$((n + 5)); done
B=~/hydo/build
mkdir -p "$OUT"
echo ">> lockstep_check $MODE on $IMG, results in $OUT"
if [ "$ENGINE" = process ]; then RUN=(env HYDO_ENGINE=process); else RUN=(~/bin/hvx-run sudo env HOME="$HOME" HYDO_ENGINE=inproc); fi
timeout 7200 "${RUN[@]}" target/release/examples/lockstep_check "$MODE" "$HOME/$TREE/target/release/hydo-hvx" ~/bin/hvx-run \
  "$B/bzImage" "$B/initramfs-kafka.cpio.gz" "$B/$IMG" "$OUT" "$@" 2>&1 | tee "$OUT/out.txt"
exit "${PIPESTATUS[0]}"
REMOTE
