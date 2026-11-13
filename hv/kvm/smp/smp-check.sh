#!/bin/sh
# Several vCPUs on the KVM engine (docs/design/kvm-vcpus.md), checked end to end on any x86_64
# Linux machine where this user can open /dev/kvm (no root, no module): the release's Redis
# example on 2 and 4 vCPUs with the SMP guest kernel, and, with one vCPU, a timing branch of the
# state search (br=, Milestone 212) replayed from a fresh boot and from the worker's S1.
#
#   HYDO=<hydo of this tree> REL=<unpacked Linux release> [SMP_KERNEL=<bzImage-kvm-smp>] \
#     [OUT=<dir>] [T=<seconds per command>] sh hv/kvm/smp/smp-check.sh
#
# Without SMP_KERNEL only the br= part runs. Each check prints PASS or FAIL, then a summary. Every
# wait has a time limit (`timeout`). It writes only under OUT.
set -u
HYDO=${HYDO:?HYDO=<hydo of this tree>}
REL=${REL:?REL=<unpacked Linux release>}
OUT=${OUT:-$PWD/smp-check}
T=${T:-1200}
mkdir -p "$OUT"
cd "$OUT" || exit 2
LOG=$OUT/smp-check.log
: > "$LOG"
pass=0
fail=0
say() { echo "$*" | tee -a "$LOG"; }
ok() { pass=$((pass + 1)); say "PASS $*"; }
bad() { fail=$((fail + 1)); say "FAIL $*"; }

say "smp-check: $(date -u) on $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//'), Linux $(uname -r)"
cp -f "$HYDO" "$REL/bin/hydo-smp-check" || exit 2
H="$REL/bin/hydo-smp-check"

IMG=$OUT/redis/hydo-out/explore-ob-redis-disk.img
if [ ! -f "$IMG" ]; then
  mkdir -p "$OUT/redis" && cp -R "$REL/examples/redis/." "$OUT/redis/"
  (cd "$OUT/redis" && timeout "$T" "$REL/bin/hydo" build hydo.yaml >> "$LOG" 2>&1)
fi
say "image: $(sha256sum "$IMG" | cut -c1-16)..."

# one run from a fresh boot: prints "<fp> <verdict>"; the run's folder keeps the engine's
# statistics (HYDO_KVM_DIAG=1: <tag>.diag)
run() { # <dir> <schedule> [hydo run options...]
  d=$1; s=$2; shift 2
  rm -rf "$d"
  HYDO_KVM_DIAG=1 timeout "$T" "$H" run --backend kvm --image "$IMG" --out "$d" "$@" "$s" > "$d.txt" 2>&1
  r=$(grep -m1 '^RESULT' "$d.txt")
  echo "$r" >> "$LOG"
  fp=$(echo "$r" | sed -n 's/.* fp=\([0-9a-f]*\) .*/\1/p')
  echo "${fp:-none} $(echo "$r" | sed -n 's/.*verdict=//p')"
}

# the same schedule from the worker's S1 (the explorer's --schedule): prints its fingerprint
from_s1() { # <dir> <schedule> [hydo explore options...]
  d=$1; s=$2; shift 2
  rm -rf "$d"
  timeout "$T" "$H" explore --backend kvm --image "$IMG" --schedule "$s" --workers 1 --out "$d" "$@" > "$d.txt" 2>&1
  cat "$d.txt" >> "$LOG"
  grep -o '"fp": *"[0-9a-f]*"' "$d.txt" | head -1 | sed 's/.*"\([0-9a-f]*\)"$/\1/'
}

twice() { # <label> <schedule> [hydo run options...]
  label=$1; s=$2; shift 2
  a=$(run "$OUT/a" "$s" "$@"); b=$(run "$OUT/b" "$s" "$@")
  say "  $label: $s: $a"
  case "$a" in
    none*) bad "$label: no result ($s; $(tail -3 "$OUT/a.txt" | tr '\n' ' '))" ;;
    *) if [ "$a" = "$b" ]; then ok "$label twice the same: $s ($a)"; else bad "$label twice the same: $s ($a / $b)"; fi ;;
  esac
}

# 1. one vCPU: a timing branch (br=, the state search) and its control, each twice from a fresh
#    boot, and from the worker's S1 (open item 85: this path ran only in scripted tests)
for s in "disk=none dseed=1 pct=ec699251106e8e0c pctn=40 pctk=3000 | run" \
         "disk=none dseed=1 pct=ec699251106e8e0c pctn=40 pctk=3000 br=40000000 pct=5be2f1a09c3d7e44 pctn=40 pctk=3000 | run"; do
  twice "1 vCPU" "$s"
  w=$(from_s1 "$OUT/x" "$s")
  a=$(run "$OUT/a" "$s")
  if [ -n "$w" ] && [ "$w" = "${a%% *}" ]; then ok "1 vCPU from S1 equals the fresh boot: $s ($w)"; else bad "1 vCPU from S1 equals the fresh boot: $s (S1 ${w:-none}, fresh ${a%% *})"; fi
done

if [ -z "${SMP_KERNEL:-}" ]; then
  say "smp-check: $pass passed, $fail failed (no SMP_KERNEL: several vCPUs not checked)"
  [ "$fail" = 0 ]
  exit
fi
# the pinned SMP kernel where hydo finds it by sha256 (the release's guest/, a tree's
# hv/kvm/linux/out/: hydo_kvm::kernel::SMP_TWINS), not through HYDO_KVM_KERNEL
mkdir -p "$REL/guest" "$REL/hv/kvm/linux/out"
cp -f "$SMP_KERNEL" "$REL/guest/bzImage-kvm-smp"
cp -f "$SMP_KERNEL" "$REL/hv/kvm/linux/out/bzImage-kvm-smp"
unset HYDO_KVM_KERNEL
say "SMP kernel: $(sha256sum "$SMP_KERNEL" | cut -c1-16)..."

# 2. several vCPUs: the default turns, fixed and seeded turn lengths, a power cut, 4 vCPUs
twice "2 vCPUs" "disk=none dseed=1 | run" --vcpus 2
find "$OUT/a" -name '*.diag' -exec grep -h 'exits per vCPU' {} + 2>/dev/null | tail -2 | tee -a "$LOG"
if find "$OUT/a" -name '*.diag' -exec grep -h 'exits per vCPU' {} + 2>/dev/null | tail -1 | grep -q 'vCPU \[[0-9]*, [1-9]'; then
  ok "2 vCPUs: vCPU 1 made exits (Linux started it and ran on it)"
else
  bad "2 vCPUs: vCPU 1 made no exits"
fi
twice "2 vCPUs" "disk=none dseed=1 q=64 | run" --vcpus 2
twice "2 vCPUs" "disk=none dseed=1 q=7 qs=1f | run" --vcpus 2
twice "2 vCPUs" "disk=cut | run C=12 R=everysec" --vcpus 2
twice "4 vCPUs" "disk=none dseed=1 | run" --vcpus 4
w=$(from_s1 "$OUT/x" "disk=none dseed=1 | run" --vcpus 2)
a=$(run "$OUT/a" "disk=none dseed=1 | run" --vcpus 2)
if [ -n "$w" ] && [ "$w" = "${a%% *}" ]; then ok "2 vCPUs from S1 equals the fresh boot ($w)"; else bad "2 vCPUs from S1 equals the fresh boot (S1 ${w:-none}, fresh ${a%% *})"; fi

# 3. an exploration on 2 vCPUs (every failure is run again from S1 by the explorer) and its repro
rm -rf "$OUT/explore"
if timeout "$T" "$H" explore --backend kvm --image "$IMG" --vcpus 2 --seeds 1-8 --strategy swarm --workers 2 --out "$OUT/explore" >> "$LOG" 2>&1; then
  ok "2 vCPUs: explore --seeds 1-8 ($(grep -c '"type": *"run"' "$OUT/explore/events.jsonl" 2>/dev/null) runs)"
  rr=$(timeout "$T" "$H" repro --backend kvm "$OUT/explore" 2>&1 | tee -a "$LOG" | tail -1)
  if echo "$rr" | grep -q -E '([0-9]+) of \1'; then ok "2 vCPUs: repro of the exploration: $rr"; else bad "2 vCPUs: repro of the exploration: $rr"; fi
else
  bad "2 vCPUs: explore --seeds 1-8"
fi

say "smp-check: $pass passed, $fail failed ($(date -u))"
[ "$fail" = 0 ]
