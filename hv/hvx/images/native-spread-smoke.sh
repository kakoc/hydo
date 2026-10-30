#!/usr/bin/env bash
# F0 native sanity (on L0/the box, NOT under hvx): start ONE spread fs-backend broker,
# poll /healthz with spread-drv, then run the single-broker `smoke` oracle against it.
# Confirms the driver's smoke subcommand + spread's fs backend end-to-end before the
# full rootfs build. Output goes to stdout.
set -u
GLIBC=/nix/store/dbcw19dshdwnxdv5q2g6wldj6syyvq7l-glibc-2.39-52
GCCLIB=/nix/store/p44qan69linp3ii0xrviypsw2j4qdcp2-gcc-13.2.0-lib
export LD_LIBRARY_PATH="$GLIBC/lib:$GCCLIB/lib"
DRV="$HOME/hydo/build/spread-drv"
SPREAD="$HOME/spread/target/release/spread"
DATA=/tmp/spread-smoke-data

pkill -f "release/spread --backend fs --data-dir $DATA" 2>/dev/null || true
sleep 1
rm -rf "$DATA" && mkdir -p "$DATA"
setsid "$SPREAD" --backend fs --data-dir "$DATA" --listen 127.0.0.1:8080 >/tmp/spread-smoke.log 2>&1 </dev/null &
SP=$!
echo "spread launched pid=$SP (LD_LIBRARY_PATH set)"
sleep 1

echo "=== health ==="
"$DRV" health 8080 </dev/null; echo "health rc=$?"
echo "=== smoke ==="
"$DRV" smoke 8080 </dev/null; echo "smoke rc=$?"
echo "=== spread.log tail ==="
tail -8 /tmp/spread-smoke.log

pkill -f "release/spread --backend fs --data-dir $DATA" 2>/dev/null || true
echo "=== done ==="
