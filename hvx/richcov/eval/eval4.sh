#!/bin/bash
# richk arms on the old front door, after eval3
cd ~/hydo-rich/hvx/explore; O=/var/tmp/m44-rich
until [ -f $O/eval3.done ]; do sleep 60; done
for r in 1 2 3; do
  python3 richguided.py --image richcov-old-disk.img --mode richk --rng0 $r --budget 60 --stop-on durable-defaults \
    --out $O/old-richk-$r > $O/old-richk-$r.out 2> $O/old-richk-$r.err < /dev/null
done
echo done > $O/eval4.done
