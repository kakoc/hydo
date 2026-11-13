#!/bin/sh
# The explorer's turn tokens on the KVM engine (docs/design/kvm-vcpus.md), on nats-server's flaky
# test (hydo/onboard/examples/nats-flaky-8363) with 2 vCPUs, on any x86_64 Linux machine where
# this user can open /dev/kvm (no root, no module):
#
#   1. `hydo explore --backend kvm --vcpus 2 --turns` over SEEDS: the drawn turn lengths reach both
#      verdicts, the explorer reproduces every failure, and `hydo repro` of the exploration
#      replays every failure with its fingerprint;
#   2. `hydo explore --backend kvm --vcpus 2 --sweep turns` with KVM's default range (1 to 4096
#      exits): it finds a boundary between a passing and a failing turn length, both lengths of
#      every boundary give one fingerprint twice, and `hydo repro` replays its failures.
#
#   HYDO=<hydo of this tree> REL=<unpacked Linux release> SMP_KERNEL=<bzImage-kvm-smp> \
#     [OUT=<dir>] [SEEDS=1-40] [WORKERS=3] [T=<seconds per command>] sh hv/kvm/smp/turns-check.sh
#
# Needs Go (build-test.sh fetches the module's toolchain) and python3. Each check prints PASS or
# FAIL, then a summary. Every wait has a time limit (`timeout`). It writes only under OUT.
set -u
HYDO=${HYDO:?HYDO=<hydo of this tree>}
REL=${REL:?REL=<unpacked Linux release>}
SMP_KERNEL=${SMP_KERNEL:?SMP_KERNEL=<bzImage-kvm-smp>}
OUT=${OUT:-$PWD/turns-check}
SEEDS=${SEEDS:-1-40}
WORKERS=${WORKERS:-3}
T=${T:-10800}
here=$(cd "$(dirname "$0")" && pwd)
ex=$(cd "$here/../../../hydo/onboard/examples/nats-flaky-8363" && pwd)
mkdir -p "$OUT"
cd "$OUT" || exit 2
LOG=$OUT/turns-check.log
: > "$LOG"
pass=0
fail=0
say() { echo "$*" | tee -a "$LOG"; }
ok() { pass=$((pass + 1)); say "PASS $*"; }
bad() { fail=$((fail + 1)); say "FAIL $*"; }

say "turns-check: $(date -u) on $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//'), Linux $(uname -r)"
cp -f "$HYDO" "$REL/bin/hydo-turns-check" || exit 2
H="$REL/bin/hydo-turns-check"
# the pinned SMP kernel where hydo finds it by sha256 (the release's guest/, a tree's
# hv/kvm/linux/out/: hydo_kvm::kernel::SMP_TWINS), not through HYDO_KVM_KERNEL
mkdir -p "$REL/guest" "$REL/hv/kvm/linux/out"
cp -f "$SMP_KERNEL" "$REL/guest/bzImage-kvm-smp"
cp -f "$SMP_KERNEL" "$REL/hv/kvm/linux/out/bzImage-kvm-smp"
unset HYDO_KVM_KERNEL
say "SMP kernel: $(sha256sum "$SMP_KERNEL" | cut -c1-16)..."

# the test programs (pinned by sha256) and the buggy image
mkdir -p "$OUT/buggy" && cp -R "$ex/." "$OUT/buggy/"
(cd "$OUT/buggy" && timeout 1800 sh build-test.sh >> "$LOG" 2>&1) || { say "turns-check: build-test.sh failed"; exit 1; }
(cd "$OUT/buggy" && timeout 1800 "$H" build hydo.yaml --set variant=buggy >> "$LOG" 2>&1)
IMG=$(ls "$OUT"/buggy/hydo-out/*-disk.img 2>/dev/null | head -1)
[ -f "$IMG" ] || { say "turns-check: hydo build failed"; exit 1; }
say "image: $(sha256sum "$IMG" | cut -c1-16)..."

# the first run of every schedule: "<tag> <kind> <q or -> <qs or -> <fp> <schedule>"
firsts() { # <exploration dir>
  python3 -I - "$1/events.jsonl" <<'EOF'
import json, re, sys
for line in open(sys.argv[1]):
    r = json.loads(line)
    if r.get("type") != "run" or r.get("role") != "first":
        continue
    s = r.get("schedule", "")
    host = s.split("|")[0]
    q = re.search(r"(?:^|\s)q=(\d+)", host)
    qs = re.search(r"(?:^|\s)qs=([0-9a-f]+)", host)
    v = r.get("verdict") or {}
    print(r.get("tag"), v.get("kind", "none"), q.group(1) if q else "-",
          qs.group(1) if qs else "-", (r.get("fingerprints") or {}).get("fp") or "none", s)
EOF
}

# 1. drawn turn lengths
rm -rf "$OUT/explore"
if timeout "$T" "$H" explore --backend kvm --image "$IMG" --vcpus 2 --turns --seeds "$SEEDS" \
    --workers "$WORKERS" --out "$OUT/explore" > "$OUT/explore.txt" 2>&1; then
  tail -5 "$OUT/explore.txt" | tee -a "$LOG"
  firsts "$OUT/explore" > "$OUT/explore-firsts.txt"
  cat "$OUT/explore-firsts.txt" >> "$LOG"
  n=$(wc -l < "$OUT/explore-firsts.txt" | tr -d ' ')
  nok=$(awk '$2 == "ok"' "$OUT/explore-firsts.txt" | wc -l | tr -d ' ')
  nbad=$(awk '$2 == "violation"' "$OUT/explore-firsts.txt" | wc -l | tr -d ' ')
  nq=$(awk '$3 != "-"' "$OUT/explore-firsts.txt" | wc -l | tr -d ' ')
  out=$(awk '$3 != "-" && ($3 < 1 || $3 > 512)' "$OUT/explore-firsts.txt" | wc -l | tr -d ' ')
  say "explore: $n schedules, $nq with a drawn turn length, $nok passed, $nbad failed"
  if [ "$nq" -gt 0 ] && [ "$out" = 0 ]; then ok "explore: every drawn q= is 1 to 512 exits ($nq schedules)"; else bad "explore: drawn q= ($nq schedules, $out outside 1 to 512)"; fi
  if [ "$nok" -gt 0 ] && [ "$nbad" -gt 0 ]; then ok "explore: both verdicts ($nok passed, $nbad failed)"; else bad "explore: both verdicts ($nok passed, $nbad failed)"; fi
  awk '$2 == "ok" {t = ($3 == "-") ? "default turns" : ("q=" $3 (($4 == "-") ? "" : " qs=" $4)); print "  passed:", t}' "$OUT/explore-firsts.txt" | sort | uniq -c | tee -a "$LOG"
  rep=$(python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1]))["summary"]; print(s.get("failures"), s.get("reproduced"), s.get("refused") or 0)' "$OUT/explore/exploration.json")
  set -- $rep
  if [ "$1" = "$2" ] && [ "$3" = 0 ]; then ok "explore: the explorer reproduced $2 of $1 failures, none refused"; else bad "explore: the explorer reproduced $2 of $1 failures, $3 refused"; fi
  rr=$(timeout "$T" "$H" repro --backend kvm "$OUT/explore" 2>&1 | tee "$OUT/explore-repro.txt" | tail -1)
  cat "$OUT/explore-repro.txt" >> "$LOG"
  if echo "$rr" | grep -q -E '([0-9]+) of \1 reproduced with the recorded fingerprint'; then ok "explore: hydo repro of the exploration: $rr"; else bad "explore: hydo repro of the exploration: $rr"; fi
else
  tail -20 "$OUT/explore.txt" | tee -a "$LOG"
  bad "explore --vcpus 2 --turns --seeds $SEEDS"
fi

# 2. the turn-length search with KVM's defaults
rm -rf "$OUT/sweep"
if timeout "$T" "$H" explore --backend kvm --image "$IMG" --vcpus 2 --sweep turns \
    --workers "$WORKERS" --out "$OUT/sweep" > "$OUT/sweep.txt" 2>&1; then
  grep -E '^turn sweep|^  `hydo run' "$OUT/sweep.txt" | tee -a "$LOG"
  firsts "$OUT/sweep" | awk '{print "  q=" $3, $2, $5}' | sort -t= -k2 -n | tee -a "$LOG"
  nb=$(grep -c '^turn sweep: boundary' "$OUT/sweep.txt")
  nr=$(grep -c '^turn sweep: boundary.*, both reproduced' "$OUT/sweep.txt")
  if [ "$nb" -gt 0 ]; then ok "sweep: $nb boundaries between turn lengths"; else bad "sweep: no boundary"; fi
  if [ "$nb" -gt 0 ] && [ "$nb" = "$nr" ]; then ok "sweep: both lengths of every boundary gave one fingerprint twice ($nr of $nb)"; else bad "sweep: reproduced boundaries $nr of $nb"; fi
  rr=$(timeout "$T" "$H" repro --backend kvm "$OUT/sweep" 2>&1 | tee "$OUT/sweep-repro.txt" | tail -1)
  cat "$OUT/sweep-repro.txt" >> "$LOG"
  if echo "$rr" | grep -q -E '([0-9]+) of \1 reproduced with the recorded fingerprint'; then ok "sweep: hydo repro of the sweep: $rr"; else bad "sweep: hydo repro of the sweep: $rr"; fi
else
  tail -20 "$OUT/sweep.txt" | tee -a "$LOG"
  bad "explore --vcpus 2 --sweep turns"
fi

say "turns-check: $pass passed, $fail failed ($(date -u))"
[ "$fail" = 0 ]
