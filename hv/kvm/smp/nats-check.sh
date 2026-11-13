#!/bin/sh
# Stage 4's usefulness test of docs/design/kvm-vcpus.md: nats-server's flaky test
# (hydo/onboard/examples/nats-flaky-8363) on the KVM engine with 2 vCPUs, over a grid of turn
# lengths. On one vCPU it fails on every schedule (the example's RESULTS.md); on hvx with 2 vCPUs
# the verdict depends on the turn length. The question: does some turn length in exits pass?
#
#   HYDO=<hydo of this tree> REL=<unpacked Linux release> SMP_KERNEL=<bzImage-kvm-smp> \
#     [OUT=<dir>] [T=<seconds per command>] [QS="1 2 4 ..."] [SEEDS="1 2 ..."] [REPEAT=1] \
#     [KEEP=1] sh hv/kvm/smp/nats-check.sh
#
# REPEAT runs each turn length, and each default-turn run, that many times; KEEP=1 keeps every
# turn length's run folder as OUT/r-<turns>-<k> (with the engine's diagnostics, HYDO_KVM_DIAG:
# which part of the end state a fingerprint differs in).
#
# Needs Go (build-test.sh fetches the module's toolchain). Each run prints one line: the turn
# length, the fingerprint and the verdict. Every wait has a time limit. It writes only under OUT.
set -u
HYDO=${HYDO:?HYDO=<hydo of this tree>}
REL=${REL:?REL=<unpacked Linux release>}
SMP_KERNEL=${SMP_KERNEL:?SMP_KERNEL=<bzImage-kvm-smp>}
OUT=${OUT:-$PWD/nats-check}
T=${T:-900}
QS=${QS:-"1 2 3 4 6 8 12 16 24 32 48 64 96 128 192 256 384 512 768 1024 2048 4096"}
SEEDS=${SEEDS-"1 2 3 4 5 6"}
REPEAT=${REPEAT:-1}
KEEP=${KEEP:-0}
here=$(cd "$(dirname "$0")" && pwd)
ex=$(cd "$here/../../../hydo/onboard/examples/nats-flaky-8363" && pwd)
mkdir -p "$OUT"
cd "$OUT" || exit 2
LOG=$OUT/nats-check.log
: > "$LOG"
say() { echo "$*" | tee -a "$LOG"; }
say "nats-check: $(date -u) on $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
cp -f "$HYDO" "$REL/bin/hydo-nats-check" || exit 2
H="$REL/bin/hydo-nats-check"

# the test programs (pinned by sha256) and the two images, buggy and fixed
mkdir -p "$OUT/ex" && cp -R "$ex/." "$OUT/ex/"
(cd "$OUT/ex" && timeout "$T" sh build-test.sh >> "$LOG" 2>&1) || { say "nats-check: build-test.sh failed"; exit 1; }
for v in buggy fixed; do
  mkdir -p "$OUT/$v" && cp -R "$OUT/ex/." "$OUT/$v/"
  (cd "$OUT/$v" && timeout "$T" "$H" build hydo.yaml --set variant=$v >> "$LOG" 2>&1)
done
img() { ls "$OUT/$1"/hydo-out/*-disk.img 2>/dev/null | head -1; }
say "images: buggy $(sha256sum "$(img buggy)" | cut -c1-16)..., fixed $(sha256sum "$(img fixed)" | cut -c1-16)..."

run() { # <image> <dir> <schedule> [options]
  i=$1; d=$2; s=$3; shift 3
  rm -rf "$d"
  HYDO_KVM_DIAG=1 timeout "$T" "$H" run --backend kvm --image "$i" --out "$d" "$@" "$s" > "$d.txt" 2>&1
  r=$(grep -m1 '^RESULT' "$d.txt")
  echo "$r" >> "$LOG"
  fp=$(echo "$r" | sed -n 's/.* fp=\([0-9a-f]*\) .*/\1/p')
  cpus=$(grep -h -o 'hydo-ats: cpus=[0-9]*' "$d"/*.log "$d.txt" 2>/dev/null | head -1 | sed 's/.*=//')
  echo "${fp:-none} cpus=${cpus:-?} $(echo "$r" | sed -n 's/.*verdict=//p' | cut -c1-90)"
}

# each of the default-turn runs REPEAT times too (one fingerprint each, below)
k=1
while [ "$k" -le "$REPEAT" ]; do
  say "1 vCPU, buggy: $(run "$(img buggy)" "$OUT/r" "disk=none | run")"
  k=$((k + 1))
done
# the pinned SMP kernel where hydo finds it by sha256 (the release's guest/, a tree's
# hv/kvm/linux/out/: hydo_kvm::kernel::SMP_TWINS), not through HYDO_KVM_KERNEL
mkdir -p "$REL/guest" "$REL/hv/kvm/linux/out"
cp -f "$SMP_KERNEL" "$REL/guest/bzImage-kvm-smp"
cp -f "$SMP_KERNEL" "$REL/hv/kvm/linux/out/bzImage-kvm-smp"
unset HYDO_KVM_KERNEL
k=1
while [ "$k" -le "$REPEAT" ]; do
  say "2 vCPUs, fixed, default turns: $(run "$(img fixed)" "$OUT/r" "disk=none | run" --vcpus 2)"
  say "2 vCPUs, buggy, default turns: $(run "$(img buggy)" "$OUT/r" "disk=none | run" --vcpus 2)"
  k=$((k + 1))
done
dir() { # <label> <k>: the run's folder
  if [ "$KEEP" = 1 ]; then echo "$OUT/r-$1-$2"; else echo "$OUT/r"; fi
}
pass=0; failn=0
for q in $QS; do
  k=1
  while [ "$k" -le "$REPEAT" ]; do
    r=$(run "$(img buggy)" "$(dir "q$q" "$k")" "disk=none q=$q | run" --vcpus 2)
    say "2 vCPUs, buggy, q=$q: $r"
    case "$r" in *"HYDO-ORACLE: OK"*) pass=$((pass + 1));; *VIOLATION*) failn=$((failn + 1));; esac
    k=$((k + 1))
  done
done
for seed in $SEEDS; do
  k=1
  while [ "$k" -le "$REPEAT" ]; do
    r=$(run "$(img buggy)" "$(dir "qs$seed" "$k")" "disk=none q=64 qs=$seed | run" --vcpus 2)
    say "2 vCPUs, buggy, q=64 qs=$seed: $r"
    case "$r" in *"HYDO-ORACLE: OK"*) pass=$((pass + 1));; *VIOLATION*) failn=$((failn + 1));; esac
    k=$((k + 1))
  done
done
# REPEAT > 1: one fingerprint per setting
if [ "$REPEAT" -gt 1 ]; then
  grep -E '^(1 vCPU|2 vCPUs), ' "$LOG" | sed 's/: \([0-9a-f]*\) .*/ \1/' | sort | uniq | awk '{k=$0; sub(/ [0-9a-f]+$/, "", k); n[k]++} END {for (k in n) print (n[k] == 1 ? "ONE " : "DIFFER ") n[k] " fingerprint(s): " k}' | sort | tee -a "$LOG"
fi
say "nats-check: buggy on 2 vCPUs: $pass passed, $failn failed ($(date -u))"
