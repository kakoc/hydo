#!/bin/sh
# integrated hvm: every spin variant at several kick timings, plain (default) and rewind mode
cd "$(dirname "$0")/../.."
for mode in plain rewind; do
  for v in spin load ldar yield samestore ldxr count; do
    for ms in 1 5 20 50 100; do
      if [ $mode = rewind ]; then rw=1; else rw=0; fi
      r=$(HVM_SPIN_REWIND=$rw HVM_SPIN_MS=$ms HVM_SPIN_SECS=6 ./hvm print spin/guest/Image spin/guest/initramfs-$v.cpio.gz "hydo-boot-marker: launching shell" 2>&1)
      fp=$(echo "$r" | sed -n 's/.*fp=\([0-9a-f]*\).*/\1/p' | head -1)
      st=$(echo "$r" | sed -n 's/.*stop=\([0-9]\).*/\1/p' | head -1)
      wall=$(echo "$r" | sed -n 's/.*wall=\([0-9.]*\) s.*/\1/p' | head -1)
      echo "$mode $v ms=$ms stop=$st fp=${fp:-none} wall=$wall invalid=$(echo "$r" | grep -c 'SPIN:\|abort')"
    done
  done
done
