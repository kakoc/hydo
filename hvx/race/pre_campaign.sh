#!/bin/bash
# all race boots: W in 50 10 2, arms tj pp xt, seeds 1-200; 2 at a time. Hits are replayed.
cd /var/tmp/m49-preempt
one() { r=$(./pre_race1.sh "$@"); lost=$(echo "$r" | grep -o "lost=[-0-9]*" | cut -d= -f2)
  if [ -n "$lost" ] && [ "$lost" != 0 ]; then
    fp=$(echo "$r" | grep -o " fp=[0-9a-f]*"); r2=$(./pre_race1.sh "$@"); fp2=$(echo "$r2" | grep -o " fp=[0-9a-f]*")
    [ "$fp" = "$fp2" ] && r="$r replay=IDENTICAL" || r="$r replay=MISMATCH"
  fi; echo "$r"; }
export -f one
for W in 50 10 2; do ./pre_race1.sh $W base 0; done > base.txt
for W in 50 10 2; do for a in tj pp xt; do for s in $(seq 1 200); do echo "$W $a $s"; done; done; done |
  xargs -P 1 -L 1 bash -c "one \$0 \$1 \$2" > camp.txt 2>&1
echo DONE >> camp.txt
