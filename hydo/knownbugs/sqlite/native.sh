#!/bin/sh
# native.sh <v> <word: reset|default> <first> <count> <cpus> <tag> [steps]
# One walrace process at a time, pinned to <cpus>; seed i = splitmix-ish of i; results one line per seed.
# Env: WALRACE_SYNC_OFF=1 (the box's /var/tmp is on a spinning disk), WALRACE_COUNT=1 for default.
# SEEDS="<hex> <hex>": cycle through these seeds instead; BIN / WALRACE_BINDIR: where walrace-<v>
# lives (default ~/hydo-walreset/bin, i.e. build.sh's output dir). Stop: touch /var/tmp/walreset/STOP-<tag>.
# Results: /var/tmp/walreset/native-<tag>.txt; a corrupt database is kept as n-<tag>/viol-<i>.db.
v=$1; word=$2; first=$3; count=$4; cpus=$5; tag=$6; steps=${7:-4}
B=${WALRACE_BINDIR:-$HOME/hydo-walreset}/${BIN:-bin}/walrace-$v
D=/var/tmp/walreset/n-$tag; mkdir -p $D
out=/var/tmp/walreset/native-$tag.txt; : > $out
export WALRACE_SEED_FILE=$D/seed
i=$first; end=$((first + count))
while [ $i -lt $end ]; do
  rm -f $D/t.db $D/t.db-wal $D/t.db-shm $D/a $D/a.r $D/o
  s=$(printf %x $(( (i * 2654435761 + 12345) % 4294967296 )))
  if [ -n "$SEEDS" ]; then set -- $SEEDS; n=$#; k=$(( i % n + 1 )); eval s=\${$k}; fi
  if [ "$word" = default ]; then echo "$s" > $D/seed; else echo "$s $word" > $D/seed; fi
  t0=$(date +%s.%N)
  timeout 300 taskset -c $cpus $B run $D/t.db $D/a $D/o 0 $steps > $D/run.out 2>&1; rc=$?
  t1=$(date +%s.%N)
  chk=$(taskset -c $cpus $B check $D/t.db $D/a 2>&1 | tail -1)
  done_line=$(grep 'walrace: done' $D/run.out | sed 's/walrace: done //')
  shape=$(grep 'walrace: seed' $D/run.out | sed 's/walrace: //')
  echo "$i rc=$rc secs=$(echo "$t1 - $t0" | bc) | $shape | $done_line | $chk" >> $out
  case "$chk" in *VIOLATION*) cp $D/t.db $D/viol-$i.db 2>/dev/null; cp $D/run.out $D/viol-$i.out;; esac
  [ -f /var/tmp/walreset/STOP ] && break
  [ -f /var/tmp/walreset/STOP-$tag ] && break
  i=$((i + 1))
done
echo DONE >> $out
