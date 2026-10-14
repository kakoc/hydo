#!/usr/bin/env bash
# F0 native sanity (on L0/the box, NOT under hvx): TWO spread fs-backend brokers sharing ONE
# data-dir, driven by the FULL mini-Jepsen op-program runner (spread-drv, no args -> runProgram).
# Confirms fs multi-broker sharing + the consistency oracle end-to-end before the VM build. The
# crash op X is OMITTED here (it needs root-owned /b<i>.pid, exercised in-guest where /init is
# root); this program covers sends/receives/nacks across both brokers + the metric-gated drain.
set -u
GLIBC=/nix/store/dbcw19dshdwnxdv5q2g6wldj6syyvq7l-glibc-2.39-52
GCCLIB=/nix/store/p44qan69linp3ii0xrviypsw2j4qdcp2-gcc-13.2.0-lib
export LD_LIBRARY_PATH="$GLIBC/lib:$GCCLIB/lib"
export TOKIO_WORKER_THREADS=1
DRV="$HOME/hydo/build/spread-drv"
SPREAD="$HOME/spread/target/release/spread"
DATA=/tmp/spread2-data

pkill -f "release/spread --backend fs --data-dir $DATA" 2>/dev/null || true
sleep 1
rm -rf "$DATA" && mkdir -p "$DATA"
setsid "$SPREAD" --backend fs --data-dir "$DATA" --listen 127.0.0.1:8080 >/tmp/spread2-b1.log 2>&1 </dev/null &
B1=$!
setsid "$SPREAD" --backend fs --data-dir "$DATA" --listen 127.0.0.1:8081 >/tmp/spread2-b2.log 2>&1 </dev/null &
B2=$!
echo "brokers launched B1=$B1 (8080) B2=$B2 (8081) sharing $DATA"

"$DRV" health 8080 </dev/null; echo "b1 health rc=$?"
"$DRV" health 8081 </dev/null; echo "b2 health rc=$?"

# runProgram assumes queue "q" already exists (there is no create-queue op) — create it first.
"$DRV" create 8080 q </dev/null

echo "=== op-program ==="
"$DRV" <<'PROG'
S 1 g0 m0
S 2 g1 m1
R 1
S 1 g0 m2
N 2
R 2
S 1 g0 m3
R 1
PROG
echo "driver rc=$?"

pkill -f "release/spread --backend fs --data-dir $DATA" 2>/dev/null || true
echo "=== done ==="
