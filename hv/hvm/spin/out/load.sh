#!/bin/sh
# counter variant + baseline under full host load (12 x yes), rewind mode
cd "$(dirname "$0")/.."
pids=""; for i in 1 2 3 4 5 6 7 8 9 10 11 12; do yes > /dev/null & pids="$pids $!"; done
for n in 1 2 3 4 5 6 7 8 9 10; do
  ms=$(( (n * 7) % 60 + 1 ))
  r=$(HVM_SPIN_REWIND=1 HVM_SPIN_MS=$ms HVM_SPIN_SECS=8 ./hvm-spin-count print guest/Image guest/initramfs-count.cpio.gz "hydo-boot-marker: launching shell" 2>&1)
  echo "count load ms=$ms $(echo "$r" | sed -n 's/.*\(fp=[0-9a-f]*\).*/\1/p' | head -1) $(echo "$r" | grep -c 'SPIN:\|abort') invalid"
done
for n in 1 2 3 4 5; do
  r=$(HVM_SPIN_REWIND=1 ./hvm-spin-count print guest/Image guest/initramfs.cpio.gz "hydo-boot-marker: launching shell" 2>&1)
  echo "baseline load $(echo "$r" | sed -n 's/.*\(fp=[0-9a-f]*\).*/\1/p' | head -1)"
done
r=$(HVM_SPIN_REWIND=1 HVM_SPIN_MS=13 ./hvm-spin-count branch guest/Image guest/initramfs-count.cpio.gz "hydo-spin: start" "hydo-boot-marker: launching shell" 2>&1)
echo "$r" | grep -E "fp=|branch:" | sed 's/(console.*//'
kill $pids
