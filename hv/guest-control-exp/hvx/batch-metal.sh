#!/bin/sh
# M116 experiment batch on the bare-metal machine (see run-metal.sh, gc_hvx.c). Results: $OUT/summary.txt.
set -u
OUT=${1:-/var/tmp/m116/b1}
R=$HOME/hydo-gc/hv/guest-control-exp/hvx/run-metal.sh
S="disk=none | run K=redis@5+2"
SB="disk=none rb=16 | run K=redis@5+2"
P="hydo-agent-ping 1"
mkdir -p "$OUT"; echo $$ > "$OUT/pid"
export GC_NONL=1
{
BIN=$HOME/hydo-gc/hv/hvx/hvx_explore $R "$OUT" control "$S" "RUN a $S\nRUN b $S"
$R "$OUT" inert "$S" "RUN a $S\nRUN b $S"
$R "$OUT" at20000 "$S" "RUN a $S\nRUN b $S" GC_AT="20000:$P"
$R "$OUT" at20001 "$S" "RUN a $S\nRUN b $S" GC_AT="20001:$P"
$R "$OUT" at20000t2 "$S" "RUN a $S\nRUN b $S" GC_AT="20000:hydo-agent-ping 2"
$R "$OUT" at60000 "$S" "RUN a $S\nRUN b $S" GC_AT="60000:$P"
$R "$OUT" two "$S" "RUN a $S\nRUN b $S" GC_AT="20000:$P;60000:hydo-agent-ping 2"
$R "$OUT" react0 "$S" "RUN a $S\nRUN b $S" GC_REACT="KILLED redis@0:$P"
$R "$OUT" react5000 "$S" "RUN a $S\nRUN b $S" GC_REACT="KILLED redis@5000:$P"
$R "$OUT" rb16at20000 "$SB" "RUN a $SB\nRUN b $SB" GC_AT="20000:$P"
$R "$OUT" br-after "$SB" "RUN a $SB\nBRANCH br 2 0 1 @3 $S" GC_AT="60000:$P"
$R "$OUT" br-before "$SB" "RUN a $SB\nBRANCH br 2 0 1 @3 $S" GC_AT="5000:$P"
$R "$OUT" br-react-gt - "RUN a $SB\nBRANCH br 2 0 1 KILLED $S" GC_REACT="KILLED redis@0:$P"
$R "$OUT" br-react-ge - "RUN a $SB\nBRANCH br 2 0 1 KILLED $S" GC_REACT="KILLED redis@0:$P" GC_RESTORE=ge
} > "$OUT/summary.txt" 2>&1
echo done > "$OUT/done"
