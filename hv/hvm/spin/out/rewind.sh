#!/bin/sh
# HVM_SPIN_REWIND=1: every variant at several kick timings (fingerprint must not depend on the timing)
cd "$(dirname "$0")/.."
for v in count spin load ldar yield samestore ldxr; do
  for ms in 1 2 5 10 20 50 100; do
    r=$(HVM_SPIN_REWIND=1 HVM_SPIN_MS=$ms HVM_SPIN_SECS=6 ./hvm-spin-count print guest/Image guest/initramfs-$v.cpio.gz "hydo-boot-marker: launching shell" 2>&1)
    fp=$(echo "$r" | sed -n 's/.*fp=\([0-9a-f]*\).*/\1/p' | head -1)
    st=$(echo "$r" | sed -n 's/.*stop=\([0-9]\).*/\1/p' | head -1)
    cyc=$(echo "$r" | sed -n 's/.*resolved \([0-9]*\),.*/\1/p')
    aff=$(echo "$r" | sed -n 's/.*counter loops proven [0-9]*, resolved \([0-9]*\).*/\1/p')
    wall=$(echo "$r" | sed -n 's/.*wall=\([0-9.]*\) s.*/\1/p' | head -1)
    echo "$v ms=$ms stop=$st fp=${fp:-none} cycles=$cyc counters=$aff wall=$wall invalid=$(echo "$r" | grep -c 'SPIN:\|abort')"
  done
done
