#!/bin/sh
# M116 experiment batch 2 on the bare-metal machine: reactions with a fixed stop cadence (rb=16), so the
# host sees the guest's line soon after it is written, and the cost of that cadence.
set -u
OUT=${1:-/var/tmp/m116/b3}
R=$HOME/hydo-gc/hv/guest-control-exp/hvx/run-metal.sh
S="disk=none | run K=redis@5+2"
SB="disk=none rb=16 | run K=redis@5+2"
SB1="disk=none rb=1 | run K=redis@5+2"
P="hydo-agent-ping 1"
mkdir -p "$OUT"; echo $$ > "$OUT/pid"
export GC_NONL=1
{
$R "$OUT" rb16-none "$SB" "RUN a $SB\nRUN b $SB"
$R "$OUT" rb16-react0 "$SB" "RUN a $SB\nRUN b $SB" GC_REACT="KILLED redis@0:$P"
$R "$OUT" rb16-react5000 "$SB" "RUN a $SB\nRUN b $SB" GC_REACT="KILLED redis@5000:$P"
$R "$OUT" rb1-none "$SB1" "RUN a $SB1\nRUN b $SB1"
$R "$OUT" rb1-react0 "$SB1" "RUN a $SB1\nRUN b $SB1" GC_REACT="KILLED redis@0:$P"
} > "$OUT/summary.txt" 2>&1
echo done > "$OUT/done"
