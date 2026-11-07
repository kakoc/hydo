#!/bin/sh
# The KVM engine's end-to-end check (docs/design/kvm-engine.md, "The check on a KVM machine"),
# on any x86_64 Linux machine where /dev/kvm can be opened by this user (no root, no module).
#
#   HYDO=<hydo built from this tree> REL=<unpacked Linux release> [OUT=<dir>] [CPUS="0 16"] \
#     [REPEAT=10] [T=<seconds per command>] sh hv/kvm/kvm-check.sh
#
# REL is the Linux release (its guest/ has initramfs-kafka.cpio.gz and must also have
# bzImage-kvm, from hv/kvm/linux/build-kernel.sh); HYDO is this tree's `hydo` (cargo build
# --release -p hydo-cli -p hydo-kvm -p hydo-explore), copied into REL/bin so that it finds the
# release's files. The Redis example's image is built with the release's own hydo when it is
# not in OUT yet (the quickstart's bytes, d255fb2b...).
#
# What it checks, each line PASS or FAIL, then a summary:
#   1. hydo doctor --backend kvm passes;
#   2. two boots from scratch of the same schedule give the same fingerprint (hydo run);
#   3. the quickstart's two schedules, each twice: the same fingerprint each time;
#   4. a run from the worker's snapshot S1 (the explorer's --schedule) gives the fingerprint of
#      the fresh boot (branch from a snapshot);
#   5. pinned to each CPU of $CPUS (a performance and an efficiency core on a hybrid CPU),
#      $REPEAT runs each: one fingerprint;
#   6. an exploration (20 seeds, swarm, 2 workers) and `hydo repro` of it: every failure
#      reproduced;
#   7. a kill, a pause, a clock step, an I/O error, tick jitter and forced preemption points,
#      each twice the same;
#   8. a network fault (the release's hydo-netem.yaml), twice the same.
# Every wait has a time limit (`timeout`). It writes only under OUT.
set -u
HYDO=${HYDO:?HYDO=<hydo of this tree>}
REL=${REL:?REL=<unpacked Linux release>}
OUT=${OUT:-$PWD/kvm-check}
CPUS=${CPUS:-"0 1"}
T=${T:-1800}
mkdir -p "$OUT"
cd "$OUT" || exit 2
LOG=$OUT/kvm-check.log
: > "$LOG"
pass=0
fail=0
say() { echo "$*" | tee -a "$LOG"; }
ok() { pass=$((pass + 1)); say "PASS $*"; }
bad() { fail=$((fail + 1)); say "FAIL $*"; }

say "kvm-check: $(date -u) on $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//'), Linux $(uname -r)"
cp -f "$HYDO" "$REL/bin/hydo-kvm-check" || exit 2
H="$REL/bin/hydo-kvm-check"

# 1. the doctor
if timeout 120 "$H" doctor --backend kvm >> "$LOG" 2>&1; then ok "doctor --backend kvm"; else bad "doctor --backend kvm"; fi

# the image
IMG=$OUT/redis/hydo-out/explore-ob-redis-disk.img
if [ ! -f "$IMG" ]; then
  mkdir -p "$OUT/redis" && cp -R "$REL/examples/redis/." "$OUT/redis/"
  (cd "$OUT/redis" && timeout "$T" "$REL/bin/hydo" build hydo.yaml >> "$LOG" 2>&1)
fi
say "image: $(sha256sum "$IMG" | cut -c1-16)..."

# one run: prints "<fp> <verdict>"
run() { # <dir> <schedule> [cpu]
  d=$1; s=$2; c=${3:-}
  rm -rf "$d"
  if [ -n "$c" ]; then
    timeout "$T" taskset -c "$c" "$H" run --backend kvm --image "$IMG" --out "$d" "$s" > "$d.txt" 2>&1
  else
    timeout "$T" "$H" run --backend kvm --image "$IMG" --out "$d" "$s" > "$d.txt" 2>&1
  fi
  r=$(grep -m1 '^RESULT' "$d.txt")
  echo "$r" >> "$LOG"
  fp=$(echo "$r" | sed -n 's/.* fp=\([0-9a-f]*\) .*/\1/p')
  echo "${fp:-none} $(echo "$r" | sed -n 's/.*verdict=//p')"
}

# 2 and 3. fresh boots, each schedule twice
for s in "disk=none | run" "disk=cut | run C=12 R=everysec"; do
  a=$(run "$OUT/r-a" "$s"); b=$(run "$OUT/r-b" "$s")
  say "  $s: $a"
  if [ "${a%% *}" != none ] && [ "$a" = "$b" ]; then ok "twice the same: $s ($a)"; else bad "twice the same: $s ($a / $b)"; fi
  eval "FP_$(echo "$s" | tr -c 'a-z0-9' _)=\${a%% *}"
done

# 4. from the worker's S1 (the explorer runs one schedule in a worker; its schedules carry dseed=)
S4="disk=none dseed=1 | run"
rm -rf "$OUT/x1"
timeout "$T" "$H" explore --backend kvm --image "$IMG" --schedule "$S4" --workers 1 --out "$OUT/x1" > "$OUT/x1.txt" 2>&1
cat "$OUT/x1.txt" >> "$LOG"
w=$(grep -o '"fp": *"[0-9a-f]*"' "$OUT/x1.txt" | head -1 | sed 's/.*"\([0-9a-f]*\)"$/\1/')
f=$(run "$OUT/r-c" "$S4")
if [ -n "$w" ] && [ "$w" = "${f%% *}" ]; then ok "a run from S1 equals the fresh boot ($w)"; else bad "a run from S1 equals the fresh boot (worker ${w:-none}, fresh ${f%% *})"; fi

# 5. pinned to each CPU, $REPEAT times each (a rare difference shows only in many runs: the
#    spurious page faults of docs/design/kvm-engine.md came in 3 of 60 runs on efficiency cores)
first=
for c in $CPUS; do
  i=0; same=0
  while [ $i -lt "${REPEAT:-10}" ]; do
    r=$(run "$OUT/r-cpu$c" "disk=cut | run C=12 R=everysec" "$c")
    [ -z "$first" ] && first=$r
    [ "$r" = "$first" ] && same=$((same + 1))
    [ "$r" = "$first" ] || say "  CPU $c run $i differs: $r"
    i=$((i + 1))
  done
  say "  CPU $c: $same of ${REPEAT:-10} give $first"
  if [ "$same" = "${REPEAT:-10}" ]; then ok "CPU $c: ${REPEAT:-10} runs, one result"; else bad "CPU $c: $same of ${REPEAT:-10} runs give the first result"; fi
done

# 6. an exploration and its reproduction
rm -rf "$OUT/explore"
if timeout "$T" "$H" explore --backend kvm --image "$IMG" --seeds 1-20 --strategy swarm --workers 2 --out "$OUT/explore" >> "$LOG" 2>&1; then
  ok "explore --seeds 1-20 ($(grep -c '"type": *"run"' "$OUT/explore/events.jsonl" 2>/dev/null) runs)"
  rr=$(timeout "$T" "$H" repro --backend kvm "$OUT/explore" 2>&1 | tee -a "$LOG" | tail -1)
  case "$rr" in *"of"*) say "  repro: $rr";; esac
  if echo "$rr" | grep -q -E '([0-9]+) of \1'; then ok "repro of the exploration: $rr"; else bad "repro of the exploration: $rr"; fi
else
  bad "explore --seeds 1-20"
fi

# 7. the other fault kinds and timing tokens, each twice: a kill, a pause, a clock step, an I/O
#    error, tick jitter, forced preemption points
for s in "disk=none | run K=redis@5+2" "disk=none | run P=redis@4+3" "disk=none | run T=6:3600" \
         "disk=ioerr:20 dseed=7 | run" "disk=none tj=1234 tjpm=300 | run" "disk=none pct=99 pctn=4 | run"; do
  a=$(run "$OUT/r-a" "$s"); b=$(run "$OUT/r-b" "$s")
  say "  $s: $a"
  if [ "${a%% *}" != none ] && [ "$a" = "$b" ]; then ok "twice the same: $s"; else bad "twice the same: $s ($a / $b)"; fi
done

# 8. network faults (netem in the guest): the release's hydo-netem.yaml
NIMG=$OUT/redis/hydo-out/explore-ob-redis-netem-disk.img
if [ ! -f "$NIMG" ]; then
  (cd "$OUT/redis" && timeout "$T" "$REL/bin/hydo" build hydo-netem.yaml >> "$LOG" 2>&1)
fi
if [ -f "$NIMG" ]; then
  IMG0=$IMG; IMG=$NIMG
  s="disk=none | run N=redis@3-6:delay,300ms"
  a=$(run "$OUT/r-a" "$s"); b=$(run "$OUT/r-b" "$s")
  say "  netem: $s: $a"
  if [ "${a%% *}" != none ] && [ "$a" = "$b" ]; then ok "twice the same: $s"; else bad "twice the same: $s ($a / $b)"; fi
  IMG=$IMG0
else
  bad "the netem image was not built"
fi

say "kvm-check: $pass passed, $fail failed ($(date -u))"
[ "$fail" = 0 ]
