#!/bin/sh
# A pause of threads on the Mac engine or the KVM engine (Milestone 234, docs/design/guest-cgroups.md):
# this example at the fingerprint epoch 3, whose guest kernels have cgroups. It builds the buggy,
# the fixed and a debug copy of the example, finds the moment the test reloads its data from the
# debug copy's output (each line carries the guest's uptime), runs a pause of the test's thread
# at every millisecond around it, and checks a failing start: it fails twice with one
# fingerprint, the same start passes on the fixed test and with the whole workload paused, and
# the run without a pause passes twice with one fingerprint.
#
#   HYDO=<hydo of this tree> REL=<unpacked release of this platform> BIN=<dir> \
#     [VCPUS=1] [DUR=500] [PRE=30] [POST=20] [JOBS=2] [OUT=<dir>] [T=<seconds per run>] \
#     sh hydo/onboard/examples/qdrant-flaky-10200/pause-check.sh
#
# BIN holds collection-buggy.test, collection-fixed.test and lib/ (the C library they load) for
# this platform's guest: build-test.sh's bin/ on x86_64 (KVM); on the Mac an arm64 build with
# ld-linux-aarch64.so.1, libc.so.6, libm.so.6 and libgcc_s.so.1. The release must have the
# guest kernels with cgroups (KVM: guest/bzImage-kvm-cg and, for VCPUS>1, bzImage-kvm-smp-cg;
# the Mac: hv/hvm/explore/out/Image-explore-cg). Every wait has a time limit (`timeout`, or the
# run's own). It writes only under OUT. Each check prints PASS or FAIL, then a summary.
set -u
HYDO=${HYDO:?HYDO=<hydo of this tree>}
REL=${REL:?REL=<unpacked release of this platform>}
BIN=${BIN:?BIN=<dir with collection-buggy.test, collection-fixed.test and lib/>}
VCPUS=${VCPUS:-1}
DUR=${DUR:-500}
PRE=${PRE:-30}
POST=${POST:-20}
JOBS=${JOBS:-2}
T=${T:-1800}
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$PWD/pause-check}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
LOG=$OUT/pause-check.log
: > "$LOG"
pass=0
fail=0
say() { echo "$*" | tee -a "$LOG"; }
ok() { pass=$((pass + 1)); say "PASS $*"; }
bad() { fail=$((fail + 1)); say "FAIL $*"; }
TO=timeout
command -v timeout > /dev/null 2>&1 || TO=gtimeout
command -v "$TO" > /dev/null 2>&1 || TO=
if [ "$(uname -s)" = Darwin ]; then
  ARCH=arm64; BACKEND=hvm; BK=
  cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null)
else
  ARCH=x86_64; BACKEND=kvm; BK="--backend kvm"
  cpu=$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')
fi
say "pause-check: $(date -u) on $cpu, $BACKEND, $VCPUS vCPU(s), pauses of $DUR ms"
cp -f "$HYDO" "$REL/bin/hydo-pause-check" || exit 2
H=$REL/bin/hydo-pause-check
VC=
[ "$VCPUS" = 1 ] || VC="--vcpus $VCPUS"

# the three copies of the example: the manifest (for arm64 with the aarch64 library paths) and
# BIN as its bin/
copy() { # <dir> <debug: 0|1>
  rm -rf "$1"; mkdir -p "$1"
  ln -s "$BIN" "$1/bin"
  if [ "$ARCH" = arm64 ]; then
    sed -e 's|{src: bin/lib/ld-linux-x86-64.so.2, dst: /lib64/ld-linux-x86-64.so.2|{src: bin/lib/ld-linux-aarch64.so.1, dst: /lib/ld-linux-aarch64.so.1|' \
        -e 's|/lib/x86_64-linux-gnu/|/lib/aarch64-linux-gnu/|g' \
        -e 's|LD_LIBRARY_PATH=/lib/x86_64-linux-gnu|LD_LIBRARY_PATH=/lib/aarch64-linux-gnu|' \
        "$HERE/hydo.yaml" > "$1/hydo.yaml"
  else
    cp "$HERE/hydo.yaml" "$1/hydo.yaml"
  fi
  if [ "$2" = 1 ]; then
    # each line of the test's output carries the guest's uptime; t0 is the workload's start
    # (the guest's shell expands these, not this one)
    # shellcheck disable=SC2016
    sed -i.orig \
      -e 's|mkdir -p /data/t/tmp \&\& cd /data/t|read t0 x < /proc/uptime; mkdir -p /data/t/tmp \&\& cd /data/t|' \
      -e 's#--test-threads 1 --nocapture > out 2>&1#--test-threads 1 --nocapture 2>\&1 | while IFS= read -r l; do read u x < /proc/uptime; echo "$u $l"; done > out; echo "t0=$t0" >> out#' \
      "$1/hydo.yaml"
    rm -f "$1/hydo.yaml.orig"
  fi
}
build() { # <dir> [hydo build args]
  d=$1; shift
  (cd "$d" && umask 022 && $TO ${TO:+$T} "$H" build hydo.yaml "$@" >> "$LOG" 2>&1) || { bad "build $d"; return 1; }
  if [ "$ARCH" = arm64 ]; then
    set -- "$d"/hydo-out/explore-ob-*-arm64.cpio.gz
  else
    set -- "$d"/hydo-out/explore-ob-*-disk.img
  fi
  img=$1
  say "image $d: $(basename "$img") $( (sha256sum "$img" 2>/dev/null || shasum -a 256 "$img") | cut -c1-16)..."
  echo "$img" > "$d/image"
}
copy "$OUT/buggy" 0; copy "$OUT/fixed" 0; copy "$OUT/dbg" 1
build "$OUT/buggy" || exit 1
build "$OUT/fixed" --set variant=fixed || exit 1
build "$OUT/dbg" || exit 1
IB=$(cat "$OUT/buggy/image"); IF=$(cat "$OUT/fixed/image"); ID=$(cat "$OUT/dbg/image")

# one run from a fresh boot: prints "<fp> <verdict word> <the queue's length>"
run() { # <dir> <image> <schedule>
  d=$1; i=$2; s=$3
  rm -rf "$d"; mkdir -p "$(dirname "$d")"
  # shellcheck disable=SC2086
  $TO ${TO:+$T} "$H" run $BK --image "$i" --out "$d" $VC "$s" > "$d.txt" 2>&1
  r=$(grep -m1 '^RESULT' "$d.txt")
  fp=$(echo "$r" | sed -n 's/.* fp=\([0-9a-f]*\) .*/\1/p')
  v=$(echo "$r" | sed -n 's/.*verdict=\(HYDO-ORACLE: \)\{0,1\}\([A-Z]*\).*/\2/p')
  q=$(cat "$d"/*.log 2>/dev/null | grep -a -o 'length: [0-9]*, op_num: Some([0-9]*)' | head -1 | sed 's/, op_num: /\//; s/length: //')
  echo "${fp:-none} ${v:-none} ${q:-none}"
}

# 1. without a pause: passes, twice the same
a=$(run "$OUT/r/plain1" "$IB" "disk=none | run"); b=$(run "$OUT/r/plain2" "$IB" "disk=none | run")
say "  no pause: $a / $b"
case "$a" in
  *" OK "*) if [ "$a" = "$b" ]; then ok "no pause: passes twice with one fingerprint ($a)"; else bad "no pause: twice the same ($a / $b)"; fi ;;
  *) bad "no pause: passes ($a)" ;;
esac

# 2. where the test reloads: the debug copy's uptime lines
run "$OUT/r/dbg" "$ID" "disk=none | run" > /dev/null
L=$(cat "$OUT/r/dbg"/*.log 2>/dev/null | grep -a 'hydo-qdrant: ' | sed 's/.*hydo-qdrant: //')
t0=$(echo "$L" | sed -n 's/^t0=//p' | head -1)
t1=$(echo "$L" | grep -m1 'Applied seq path' | cut -d' ' -f1)
t2=$(echo "$L" | grep -m1 'Post-load update queue info' | cut -d' ' -f1)
if [ -z "$t0" ] || [ -z "$t1" ] || [ -z "$t2" ]; then
  bad "the debug copy's output names the reload (t0=$t0 load=$t1 post-load=$t2)"
  say "pause-check: $pass passed, $fail failed"; exit 1
fi
lo=$(awk -v a="$t1" -v z="$t0" -v p="$PRE" 'BEGIN { x = int((a - z) * 1000) - p; if (x < 0) x = 0; print x }')
hi=$(awk -v a="$t2" -v z="$t0" -v p="$POST" 'BEGIN { print int((a - z) * 1000) + 10 + p }')
say "  the reload: t0=$t0 s, load $t1 s, post-load $t2 s: pauses from $lo to $hi ms"

# 3. a pause of the test's thread at every millisecond of the window, JOBS runs at a time
mkdir -p "$OUT/w"
o=$lo
while [ "$o" -le "$hi" ]; do
  n=0
  while [ "$n" -lt "$JOBS" ] && [ "$o" -le "$hi" ]; do
    ( r=$(run "$OUT/w/$o" "$IB" "disk=none | run P=workload/tests::wal_reco@0~$o:$DUR"); echo "$o $r" > "$OUT/w/$o.res" ) &
    o=$((o + 1)); n=$((n + 1))
  done
  wait
done
cat "$OUT"/w/*.res | sort -n > "$OUT/window.txt"
tee -a "$LOG" < "$OUT/window.txt" > /dev/null
nv=$(grep -c ' VIOLATION ' "$OUT/window.txt")
nok=$(grep -c ' OK ' "$OUT/window.txt")
say "  the window: $nv of $((hi - lo + 1)) starts fail, $nok pass: $(grep ' VIOLATION ' "$OUT/window.txt" | cut -d' ' -f1 | tr '\n' ' ')"
S=$(grep -m1 ' VIOLATION ' "$OUT/window.txt" | cut -d' ' -f1)
if [ -z "$S" ]; then
  bad "a pause of the test's thread fails the test at some start from $lo to $hi ms"
  say "pause-check: $pass passed, $fail failed ($(date -u))"; exit 1
fi
ok "a pause of the test's thread fails the test: $nv starts of $((hi - lo + 1)) (the first at $S ms)"
F="disk=none | run P=workload/tests::wal_reco@0~$S:$DUR"

# 4. the failing schedule: twice the same; the fixed test and a pause of the whole workload pass
a=$(run "$OUT/r/f1" "$IB" "$F"); b=$(run "$OUT/r/f2" "$IB" "$F")
say "  $F: $a / $b"
case "$a" in
  *" VIOLATION "*) if [ "$a" = "$b" ]; then ok "fails twice with one fingerprint: $F ($a)"; else bad "twice the same: $F ($a / $b)"; fi ;;
  *) bad "fails again: $F ($a)" ;;
esac
grep -a -h -o 'PAUSED workload/[^"]*' "$OUT/r/f1/control.jsonl" 2>/dev/null | head -1 | tee -a "$LOG"
a=$(run "$OUT/r/fx" "$IF" "$F")
case "$a" in *" OK "*) ok "the fixed test passes: $F ($a)" ;; *) bad "the fixed test passes: $F ($a)" ;; esac
W="disk=none | run P=workload@0~$S:$DUR"
a=$(run "$OUT/r/w" "$IB" "$W")
case "$a" in *" OK "*) ok "a pause of the whole workload passes: $W ($a)" ;; *) bad "a pause of the whole workload passes: $W ($a)" ;; esac

say "pause-check: $pass passed, $fail failed ($(date -u))"
[ "$fail" = 0 ]
