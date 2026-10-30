#!/usr/bin/env bash
# Boot the debug test kernel in a QEMU/KVM VM with nested VMX, load hvx.ko, run the module
# tests and short real-Linux boots under hvx, and report every KASAN / UBSAN / lockdep /
# kmemleak / BUG / WARNING splat (Milestone 97). Runs ON THE BUILD MACHINE BUILD BOX after
# build-kernel.sh and build-debug.sh. Never on the bare-metal test machine (KVM there would turn VMX
# on next to the shared hvx module).
#   run-debug.sh [test,test,...]     default: every test in the initramfs
# Env: VM_MEM (MiB, default 6144: hvx_bigram_test needs a 3 GiB guest), VM_CPUS (2),
#      TEST_TIMEOUT (s per test, 900), OUT (log directory).
# Exit status: 0 = every test passed (or was skipped for lack of a PMU) and no splat.
set -uo pipefail
SAFETY=${SAFETY:-$HOME/hydo-safety}
NIXPKGS=/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source
D="$SAFETY/debug"
OUT=${OUT:-$SAFETY/runs/debug-$(date +%Y%m%d-%H%M%S)}
mkdir -p "$OUT" "$SAFETY/gcroots"
ONLY=${1:-}
QEMU=$(nix-build "$NIXPKGS" -A qemu_kvm --no-out-link -o "$SAFETY/gcroots/qemu" 2>/dev/null | tail -1)/bin/qemu-system-x86_64
[ -x "$QEMU" ] || QEMU=$(readlink -f "$SAFETY/gcroots/qemu")/bin/qemu-system-x86_64
[ -r /dev/kvm ] && [ -w /dev/kvm ] || { echo "run-debug: /dev/kvm is not usable by $(id -un)" >&2; exit 2; }

APPEND="console=ttyS0 nokaslr rdinit=/init panic_on_warn=0 oops=panic panic=-1 kmemleak=on hvxto=${TEST_TIMEOUT:-900}"
[ -n "$ONLY" ] && APPEND="$APPEND hvxtests=$ONLY"
LOG="$OUT/serial.log"
echo "run-debug: log $LOG"
# -cpu host passes VMX through (kvm_intel nested=Y on this host); the VM is capped in
# memory and CPUs and runs at low priority, because the host is shared.
nice -n 10 timeout "${VM_TIMEOUT:-14400}" "$QEMU" -name hvx-safety-debug \
  -enable-kvm -cpu host -smp "${VM_CPUS:-2}" -m "${VM_MEM:-6144}" \
  -kernel "$SAFETY/kbuild/bzImage" -initrd "$D/initramfs.cpio.gz" -append "$APPEND" \
  -display none -serial "file:$LOG" -no-reboot -nic none \
  -pidfile "$OUT/qemu.pid"
qrc=$?
[ $qrc = 124 ] && echo "run-debug: VM TIMEOUT (${VM_TIMEOUT:-14400}s)"

# --- report ---
SPLAT='BUG:|KASAN:|UBSAN:|WARNING:|general protection fault|Oops|Kernel panic|kernel BUG|soft lockup|hard LOCKUP|rcu: INFO|rcu_sched self-detected|blocked for more than|possible circular locking|possible recursive locking|inconsistent lock state|suspicious RCU usage|sleeping function called from invalid context|scheduling while atomic|list_add corruption|list_del corruption|refcount_t:|Bad page|bad page state|stack-protector|stack guard page|unreferenced object|DEBUG_LOCKS_WARN_ON|hardened usercopy|detected buffer overflow'
awk -v splat="$SPLAT" '
  { gsub(/\r/, "") }
  /SAFETY START /  { cur = $NF; next }
  /SAFETY END /    { cur = "(between tests)"; next }
  /SAFETY RESULT / { sub(/^.*SAFETY /, "SAFETY "); print; next }
  /SAFETY BEGIN|SAFETY insmod ok/ { cur = "(module load)"; next }
  /SAFETY rmmod/   { cur = "(module unload)"; next }
  /SAFETY KMEMLEAK|SAFETY DONE|SAFETY FATAL/ { cur = "(after rmmod)"; next }
  /SAFETY\| /      { next }
  $0 ~ splat { print "SPLAT [" (cur == "" ? "(boot)" : cur) "] " $0 }
' "$LOG" > "$OUT/report.txt"
grep -a 'SAFETY| ' "$LOG" | sed 's/^.*SAFETY| //' > "$OUT/tests.txt"

pass=0; fail=0; skip=0; splats=$(grep -c '^SPLAT' "$OUT/report.txt")
while read -r _ _ name rcf _; do
  rc=${rcf#rc=}
  if [ "$rc" = 0 ]; then pass=$((pass+1)); st=PASS
  elif grep -q "^$name: .*\(No such device\|ENODEV\|PMU\|pmu\)" "$OUT/tests.txt"; then skip=$((skip+1)); st="SKIP (needs the PMU branch clock; not available nested)"
  else fail=$((fail+1)); st="FAIL rc=$rc"; fi
  printf '  %-24s %s\n' "$name" "$st"
done < <(grep -a '^SAFETY RESULT' "$OUT/report.txt")
grep '^SPLAT' "$OUT/report.txt" | cut -c1-240 | head -n 60
grep -a 'SAFETY KMEMLEAK' "$LOG" | sed 's/^.*SAFETY /  /'
grep -q 'SAFETY DONE' "$LOG" || { echo "run-debug: the VM did not finish (crash or timeout); see $LOG"; fail=$((fail+1)); }
echo "run-debug: $pass passed, $skip skipped, $fail failed, $splats splat lines ($OUT)"
[ $fail = 0 ] && [ "$splats" = 0 ]
