#!/bin/bash
# rerun the two rich arms that had HVX_SET_PT failures (possible foreign traces), with the
# no-trace guard, after eval2 (old-random-1 rerun) is done
cd ~/hydo-rich/hvx/explore; O=/var/tmp/m44-rich
until [ -f $O/eval2.done ]; do sleep 60; done
for r in 1 2; do
  python3 richguided.py --image richcov-old-disk.img --mode rich --rng0 $r --budget 60 --stop-on durable-defaults \
    --out $O/old-rich-$r > $O/old-rich-$r.out 2> $O/old-rich-$r.err < /dev/null
done
echo done > $O/eval3.done
