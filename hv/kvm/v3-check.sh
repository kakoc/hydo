#!/bin/sh
# The KVM engine at x86-64-v3 (guest.cpu: v3; docs/design/kvm-engine.md, "x86-64-v3"), on any
# x86_64 Linux machine where /dev/kvm can be opened by this user (no root, no module), Intel or AMD.
#
#   HYDO=<hydo built from this tree> REL=<unpacked or installed Linux release with guest/bzImage-kvm> \
#     [HYDOAVX=<static hydoavx>] [OUT=<dir>] [T=<seconds per command>] sh hv/kvm/v3-check.sh
#
# HYDOAVX is hydo/onboard/examples/avx2/hydoavx.c compiled statically (`cc -static -O2 -pthread
# hydoavx.c -o hydoavx -lm`); without it the script compiles it with $CC (default cc). The image is
# hv/kvm/v3/hydo.yaml built with the release's own hydo (kept in OUT).
#
# What it checks, each line PASS or FAIL, then a summary:
#   1. hydo doctor --backend kvm says that guest.cpu: v3 can run here;
#   2. two boots from scratch of `disk=none | run` give one fingerprint and OK (AVX2 and FMA results
#      equal the scalar ones; the guest kernel uses XSAVE with x87|SSE|AVX);
#   3. a run from the explorer's worker snapshot S1 gives the fresh boot's fingerprint;
#   4. a clock step (`T=4:3600`) twice: one fingerprint, OK;
#   5. the control: the same image on a v2 guest CPU (`hydo explore --cpu v2`) fails its check.
# It prints the guest's CPU flags and the host line (which CPUs the vCPU runs on). Every wait has a
# time limit (`timeout`). It writes only under OUT.
set -u
HYDO=${HYDO:?HYDO=<hydo of this tree>}
REL=${REL:?REL=<Linux release>}
OUT=${OUT:-$PWD/kvm-v3-check}
T=${T:-1800}
here=$(cd "$(dirname "$0")" && pwd)
top=$(cd "$here/../.." && pwd)
mkdir -p "$OUT"
cd "$OUT" || exit 2
LOG=$OUT/v3-check.log
: > "$LOG"
pass=0
fail=0
say() { echo "$*" | tee -a "$LOG"; }
ok() { pass=$((pass + 1)); say "PASS $*"; }
bad() { fail=$((fail + 1)); say "FAIL $*"; }

say "v3-check: $(date -u) on $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//'), Linux $(uname -r)"
cp -f "$HYDO" "$REL/bin/hydo-v3-check" || exit 2
H="$REL/bin/hydo-v3-check"

# 1. the doctor
timeout 120 "$H" doctor --backend kvm > "$OUT/doctor.txt" 2>&1
grep 'guest.cpu: v3' "$OUT/doctor.txt" | tee -a "$LOG"
if grep -q 'guest.cpu: v3 (x86-64-v3, AVX2) can run here' "$OUT/doctor.txt"; then ok "doctor: v3 can run here"; else bad "doctor: v3 cannot run here"; fi

# the image
IMG=$OUT/v3/hydo-out/explore-ob-kvm-v3-disk.img
if [ ! -f "$IMG" ]; then
  mkdir -p "$OUT/v3"
  cp "$here/v3/hydo.yaml" "$OUT/v3/"
  if [ -n "${HYDOAVX:-}" ]; then
    cp "$HYDOAVX" "$OUT/v3/hydoavx"
  else
    ${CC:-cc} -static -O2 -pthread "$top/hydo/onboard/examples/avx2/hydoavx.c" -o "$OUT/v3/hydoavx" -lm >> "$LOG" 2>&1
  fi
  (cd "$OUT/v3" && timeout "$T" "$REL/bin/hydo" build hydo.yaml >> "$LOG" 2>&1)
fi
[ -f "$IMG" ] || { bad "the v3 image was not built"; say "v3-check: $pass passed, $fail failed"; exit 1; }
say "image: $(sha256sum "$IMG" | cut -c1-16)..."

# one fresh-boot run: prints "<fp> <verdict>"
run() { # <dir> <schedule>
  d=$1; s=$2
  rm -rf "$d"
  timeout "$T" "$H" run --backend kvm --image "$IMG" --out "$d" "$s" > "$d.txt" 2>&1
  r=$(grep -m1 '^RESULT' "$d.txt")
  echo "$r" >> "$LOG"
  fp=$(echo "$r" | sed -n 's/.* fp=\([0-9a-f]*\) .*/\1/p')
  echo "${fp:-none} $(echo "$r" | sed -n 's/.*verdict=//p')"
}
# one run from the explorer's worker S1: prints "<fp> <verdict>"
runs1() { # <dir> <schedule> [explore options]
  d=$1; s=$2; shift 2
  rm -rf "$d"
  timeout "$T" "$H" explore --backend kvm --image "$IMG" --schedule "$s" --workers 1 --out "$d" "$@" > "$d.txt" 2>&1
  j=$(grep -m1 '^{"fp"' "$d.txt")
  echo "$j" >> "$LOG"
  fp=$(echo "$j" | sed -n 's/.*"fp": *"\([0-9a-f]*\)".*/\1/p')
  echo "${fp:-none} $(echo "$j" | sed -n 's/.*"verdict": *"\([^"]*\)".*/\1/p')"
}

# 2. two fresh boots
s="disk=none | run"
a=$(run "$OUT/r-a" "$s"); b=$(run "$OUT/r-b" "$s")
say "  $s: $a"
grep -ah 'hydo-kvm: /dev/kvm' "$OUT/r-a"/*.err "$OUT/r-a/engine.log" 2>/dev/null | head -1 | tee -a "$LOG"
grep -ah 'hydo-avx-cpu' "$OUT/r-a"/*.log 2>/dev/null | sort -u | tee -a "$LOG"
case "$a" in
  *"HYDO-ORACLE: OK"*) if [ "$a" = "$b" ]; then ok "two fresh boots: one fingerprint, OK"; else bad "two fresh boots differ ($b)"; fi ;;
  *) bad "fresh boot: $a" ;;
esac

# 3. a run from S1
c=$(runs1 "$OUT/x-s1" "$s")
say "  S1: $c"
if [ "${c%% *}" = "${a%% *}" ]; then ok "a run from S1 gives the fresh boot's fingerprint"; else bad "S1 ${c%% *} against the fresh boot ${a%% *}"; fi

# 4. a clock step, twice
s="disk=none | run T=4:3600"
a=$(run "$OUT/r-t1" "$s"); b=$(run "$OUT/r-t2" "$s")
say "  $s: $a"
case "$a" in
  *"HYDO-ORACLE: OK"*) if [ "$a" = "$b" ]; then ok "clock step twice the same"; else bad "clock step differs ($b)"; fi ;;
  *) bad "clock step: $a" ;;
esac

# 5. the control: a v2 guest CPU
c=$(runs1 "$OUT/x-v2" "disk=none | run" --cpu v2)
say "  as v2: $c"
case "$c" in
  *VIOLATION*) ok "the control: on a v2 guest CPU the AVX2 check fails" ;;
  *) bad "the control on v2 did not fail: $c" ;;
esac

say "v3-check: $pass passed, $fail failed ($(date -u))"
[ "$fail" = 0 ]
