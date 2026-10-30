#!/usr/bin/env bash
# F0 native sanity for the Phase-3 driver change: two brokers on shared /data, queue q created,
# then `spread-drv looprun` reads ONE op-program from stdin until a lone "RUN" line and drives it.
set -u
GLIBC=/nix/store/dbcw19dshdwnxdv5q2g6wldj6syyvq7l-glibc-2.39-52
GCCLIB=/nix/store/p44qan69linp3ii0xrviypsw2j4qdcp2-gcc-13.2.0-lib
export LD_LIBRARY_PATH="$GLIBC/lib:$GCCLIB/lib"; export TOKIO_WORKER_THREADS=1
DRV="$HOME/hydo/build/spread-drv"; SPREAD="$HOME/spread/target/release/spread"; DATA=/tmp/spread3-data
pkill -f "release/spread --backend fs --data-dir $DATA" 2>/dev/null || true; sleep 1
rm -rf "$DATA" && mkdir -p "$DATA"
setsid "$SPREAD" --backend fs --data-dir "$DATA" --listen 127.0.0.1:8080 >/tmp/s3-b1.log 2>&1 </dev/null & echo $! >/tmp/b1.pid
setsid "$SPREAD" --backend fs --data-dir "$DATA" --listen 127.0.0.1:8081 >/tmp/s3-b2.log 2>&1 </dev/null & echo $! >/tmp/b2.pid
"$DRV" health 8080 </dev/null; "$DRV" health 8081 </dev/null; "$DRV" create 8080 q </dev/null
echo "=== looprun (program fed on stdin, terminated by RUN) ==="
printf 'S 1 g0 m0\nS 2 g1 m1\nR 1\nN 2\nR 2\nP g0 g3 m5a m5b\nRUN\n' | "$DRV" looprun
echo "looprun rc=$?"
pkill -f "release/spread --backend fs --data-dir $DATA" 2>/dev/null || true
echo "=== done ==="
