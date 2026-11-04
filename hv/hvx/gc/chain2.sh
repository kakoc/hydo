#!/bin/bash
# M138, on the bare-metal machine, after chain.sh (waits for its CHAIN-END, at most 12 h):
# the timing runs, then the gRPC stack's lock-step hang over the serial line and the port.
G=$(cd "$(dirname "$0")" && pwd)
M=/var/tmp/m138
n=0; while ! grep -q CHAIN-END $M/chain.log 2>/dev/null && [ $n -lt 4320 ]; do sleep 10; n=$((n+1)); done
bash "$G/timing.sh" 5 > $M/timing.txt 2>&1
bash "$G/hang.sh" $M/pfx $M/img/grpc-planted-a3-disk.img > $M/hang.txt 2>&1
echo CHAIN2-END >> $M/chain.log
