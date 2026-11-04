#!/bin/bash
# A/B timing of the turn-end cost cuts on the side module: alternate the ft_opt module
# parameter between runs of ft_cost (same binary, same load window). On the bare-metal machine:
#   ab.sh <reps> <q> <n> <kind> <optA> <optB> [FT_MARGIN]
set -u
P=/sys/module/hvxft/parameters/ft_opt
reps=$1 q=$2 n=$3 kind=$4 A=$5 B=$6 M=${7:-}
cd /var/tmp/fastturn
for r in $(seq "$reps"); do
  for o in "$A" "$B"; do
    echo "$o" | sudo tee $P > /dev/null
    out=$(timeout 300 ~/bin/hvx-run sudo env HVX_DEV=/dev/hvxft ${M:+FT_MARGIN=$M} ./ft_cost "$q" "$n" "$kind")
    echo "opt=$o $(grep -o 'us/turn)\|[0-9.]* us/turn\|turnhash=[0-9a-f]*\|overshoots=[0-9]*' <<< "$out" | tr '\n' ' ') $(grep -o 'per step [0-9]* cyc (VMRESUME..exit [0-9]*)' <<< "$out")"
  done
done
echo 7 | sudo tee $P > /dev/null
