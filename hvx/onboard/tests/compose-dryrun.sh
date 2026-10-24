#!/bin/sh
# Dry run of the guest composer (hydo-composer.sh) on a Linux host with busybox, no VM: the
# daemon's rounds, gates, overlap, drain and console lines, driven the way hydo-init.sh drives
# it (provision -> step ranges with a "fault point" between them -> final invariants).
#   BB=/path/to/busybox sh hvx/onboard/tests/compose-dryrun.sh [W]
# Host timing is not deterministic, so this checks the mechanics, not fingerprints.
set -e
BB=${BB:-busybox}
HERE=$(cd "$(dirname "$0")/.." && pwd)
T=$(mktemp -d)
E=$T/etc; mkdir -p $E $T/tmp/hydo
cp $HERE/hydo-composer.sh $E/compose.sh
cat > $E/manifest.sh <<'EOF'
HYDO_STEPS=6
HYDO_COMPOSE=1
HYDO_COMPOSE_PARALLEL=3
HYDO_COMPOSE_PER_ROUND=2
HYDO_COMPOSE_ROUND_MS=100
HYDO_COMPOSE_COMMAND_TIMEOUT=5
HYDO_COMPOSE_DRAIN_TIMEOUT=3
HYDO_COMPOSE_MAX_COMMANDS=0
HYDO_COMPOSE_BUDGET_MS=0
EOF
echo "export PATH=$(dirname $(command -v $BB 2>/dev/null || echo $BB)):\$PATH" > $E/prelude.sh
cat > $E/compose.list <<EOF
0 anytime 5 anytime_x exec $BB true
1 eventually 5 eventually_e echo recovered
2 finally 5 finally_f echo final
3 first 5 first_init echo init
4 parallel_driver 5 parallel_driver_a exec $BB sleep 0.25
5 parallel_driver 5 parallel_driver_b exit 3
6 serial_driver 5 serial_driver_s exec $BB sleep 0.1
7 parallel_driver 5 parallel_driver_v exit 100
EOF
cat > $E/compose.checks <<EOF
0 final 5 c_ok echo fine
EOF
echo "${1:-0000002a,p3,q2,r100}" > $T/tmp/hydo/compose.W
export BB HYDO_ETC=$E HYDO_TMP=$T/tmp/hydo HYDO_CONSOLE=$T/console
CS="$BB sh $E/compose.sh"
$CS first
$CS step 0 3; echo "== fault point at step 3 (the daemon waits; commands keep running)" >> $T/console
$BB sleep 0.3
$CS step 3 6
for i in 1 2; do $CS end $i >> $T/console || echo "end $i rc=$?" >> $T/console; done
$CS check 0 >> $T/console
$CS verdict >> $T/console || echo "verdict rc=$?" >> $T/console
echo "== plan:"; $CS plan
echo "== console:"; cat $T/console
grep -q "HYDO-COMPOSE done" $T/console
grep -q "parallel_driver_v (entry 1) reported a violation" $T/console
# 2. a composition longer than workload.timeout: step stops it (every command killed, recorded)
: > $T/console; rm -rf $T/tmp/hydo/compose
echo HYDO_WORKLOAD_TIMEOUT=3 >> $E/manifest.sh
cat > $E/compose.list <<EOF2
0 parallel_driver 60 parallel_driver_long exec $BB sleep 30
EOF2
echo 00000001,p2,q2,r100 > $T/tmp/hydo/compose.W
t0=$(cut -d. -f1 /proc/uptime)
$CS step 0 6 || echo "step rc=$?" >> $T/console
$CS verdict >> $T/console
echo "== stopped after $(( $(cut -d. -f1 /proc/uptime) - t0 )) s:"; grep -v DECL $T/console
grep -q "status=killed (workload.timeout)" $T/console
[ -z "$(ps -o args | grep -v grep | grep 'compose.sh run1')" ] || { echo "a command is still running"; exit 1; }
rm -rf $T
echo "compose-dryrun: OK"
