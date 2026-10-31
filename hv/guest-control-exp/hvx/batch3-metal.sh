#!/bin/sh
# M116 experiment batch 3 on the bare-metal machine: the snapshot rule. The branch point and the reaction
# are the same stop (marker "KILLED"): the base run types the line right after the snapshot, so
# a variant restored from that snapshot has it only if the engine types it again (GC_RESTORE=ge).
set -u
OUT=${1:-/var/tmp/m116/b4}
R=$HOME/hydo-gc/hv/guest-control-exp/hvx/run-metal.sh
S="disk=none | run K=redis@5+2"
SB="disk=none rb=16 | run K=redis@5+2"
P="hydo-agent-ping 1"
mkdir -p "$OUT"; echo $$ > "$OUT/pid"
export GC_NONL=1
{
$R "$OUT" same-gt - "RUN a $SB\nBRANCH br 2 0 1 KILLED $S" GC_REACT="KILLED@0:$P"
$R "$OUT" same-ge - "RUN a $SB\nBRANCH br 2 0 1 KILLED $S" GC_REACT="KILLED@0:$P" GC_RESTORE=ge
} > "$OUT/summary.txt" 2>&1
echo done > "$OUT/done"
