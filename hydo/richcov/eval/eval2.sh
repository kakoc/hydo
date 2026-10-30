#!/bin/bash
# rerun old-random-1 (its python was killed externally at run 40, likely a pkill -f guided.py
# from another workstream) once eval.sh is done
cd ~/hydo-rich/hvx/explore; O=/var/tmp/m44-rich
until [ -f $O/eval.done ]; do sleep 60; done
python3 richguided.py --image richcov-old-disk.img --mode random --rng0 1 --budget 60 --stop-on durable-defaults \
  --out $O/old-random-1 > $O/old-random-1.out 2> $O/old-random-1.err < /dev/null
echo done > $O/eval2.done
