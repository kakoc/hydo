#!/bin/bash
# M44 workstream B evaluation, 1 worker, sequential.
cd ~/hydo-rich/hvx/explore
O=/var/tmp/m44-rich
# 0. the old bug is present on richcov-old (pinned reproducer, traced)
# (a) time to the old durable-defaults bug, cap 60, 3 starts each
for r in 1 2 3; do
  for m in rich random; do
    python3 richguided.py --image richcov-old-disk.img --mode $m --rng0 $r --budget 60 --stop-on durable-defaults \
      --out $O/old-$m-$r > $O/old-$m-$r.out 2> $O/old-$m-$r.err < /dev/null
  done
done
# (b) feature growth + failures by class on the fixed front door, 40 runs each
for m in rich random edges; do
  python3 richguided.py --image richcov-fix-disk.img --mode $m --rng0 1 --budget 40 \
    --out $O/fix-$m-1 > $O/fix-$m-1.out 2> $O/fix-$m-1.err < /dev/null
done
echo EVAL-DONE > $O/eval.done
