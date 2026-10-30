#!/bin/sh
# start the host load right before each run (the one invalid run so far was the first after starting it)
cd "$(dirname "$0")/.."
for n in $(seq 1 12); do
  pids=""; for i in 1 2 3 4 5 6 7 8 9 10 11 12; do yes > /dev/null & pids="$pids $!"; done
  r=$(HVM_SPIN_REWIND=1 HVM_SPIN_MS=8 HVM_SPIN_SECS=8 ./hvm-spin-count print guest/Image guest/initramfs-count.cpio.gz "hydo-boot-marker: launching shell" 2>&1)
  kill $pids; wait 2>/dev/null
  fp=$(echo "$r" | sed -n 's/.*\(fp=[0-9a-f]*\).*/\1/p' | head -1)
  echo "n=$n $fp"; [ "$fp" = "fp=d8c2bb710b72b0db" ] || { echo "$r" | grep -v "^\[" | grep -v "^hydo" | tail -8; }
done
