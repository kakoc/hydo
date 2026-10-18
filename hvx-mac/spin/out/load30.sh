#!/bin/sh
cd "$(dirname "$0")/.."
pids=""; for i in 1 2 3 4 5 6 7 8 9 10 11 12; do yes > /dev/null & pids="$pids $!"; done
for n in $(seq 1 30); do
  ms=$(( (n * 7) % 60 + 1 ))
  r=$(HVM_SPIN_REWIND=1 HVM_SPIN_MS=$ms HVM_SPIN_SECS=8 HVM_SPLOG=out/l30-$n.log ./hvm-spin-count print guest/Image guest/initramfs-count.cpio.gz "hydo-boot-marker: launching shell" 2>&1)
  fp=$(echo "$r" | sed -n 's/.*\(fp=[0-9a-f]*\).*/\1/p' | head -1)
  echo "n=$n ms=$ms $fp"; [ "$fp" = "fp=d8c2bb710b72b0db" ] || { echo "$r" | grep -v "^\[" | tail -8; }
done
kill $pids
