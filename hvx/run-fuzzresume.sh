#!/usr/bin/env bash
# Build the (clean) module + the minimal deep-snapshot-resume isolation test, load, run.
# Tiny guest (256 KiB, no Linux) -> cannot wedge L1. Run ON THE HOST after ./dev.sh sync.
set -uo pipefail
SRC="${HYDO_HVX_SRC:-$HOME/hydo/hvx}"
K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -n -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

L1SSH 'mkdir -p /tmp/hvx-src'
L1SCP "$SRC/hvx.c" "$SRC/hvx_abi.h" "$SRC/Makefile" "$SRC/hvx_fuzzresume_test.c" dev@localhost:/tmp/hvx-src/ >/dev/null
L1SSH 'cd /tmp/hvx-src && make >/tmp/mk.log 2>&1; echo "make=$?"; tail -2 /tmp/mk.log; \
       cc -O2 -Wall -o hvx_fuzzresume_test hvx_fuzzresume_test.c; echo "cc=$?"; \
       lsmod | grep -q hvx && sudo rmmod hvx; sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; sudo insmod hvx.ko; echo "insmod=$?"; \
       echo "=== TEST ==="; sudo timeout 60 ./hvx_fuzzresume_test; echo "test_rc=$?"; \
       echo "=== dmesg tail ==="; sudo dmesg | grep -aiE "hvx|bug|oops|stall|taint" | tail -12; \
       sudo rmmod hvx; echo "rmmod=$?"'
