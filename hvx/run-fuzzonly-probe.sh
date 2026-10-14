#!/usr/bin/env bash
# Build clean module + fuzzonly probe, stage spread3 assets, run the probe (bounded,
# host-side RIP/tuple probing -> cannot wedge). Run ON THE HOST after ./dev.sh sync.
set -uo pipefail
SRC="${HYDO_HVX_SRC:-$HOME/hydo/hvx}"; BUILD="${HYDO_BUILD:-$HOME/hydo/build}"
K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -n -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

L1SSH 'mkdir -p /tmp/hvx-src'
L1SCP "$SRC/hvx.c" "$SRC/hvx_abi.h" "$SRC/hvx_boot.h" "$SRC/hvx_fuzzonly_probe.c" "$SRC/Makefile" dev@localhost:/tmp/hvx-src/ >/dev/null
L1SSH 'cd /tmp/hvx-src && make >/tmp/mk.log 2>&1; echo "make=$?"; tail -1 /tmp/mk.log; \
       cc -O2 -Wall -o hvx_fuzzonly_probe hvx_fuzzonly_probe.c; echo "cc=$?"'
echo ">> staging spread3 assets (once)"
L1SSH 'test -f /tmp/bzImage && test -f /tmp/initramfs-spread3.cpio.gz && test -f /tmp/spread3-disk.img && echo present' \
  || L1SCP "$BUILD/bzImage" "$BUILD/initramfs-spread3.cpio.gz" "$BUILD/spread3-disk.img" dev@localhost:/tmp/ >/dev/null
echo ">> load module + run probe"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod=$?"; \
       sudo timeout 240 /tmp/hvx-src/hvx_fuzzonly_probe /tmp/bzImage /tmp/initramfs-spread3.cpio.gz /tmp/spread3-disk.img 2>&1 | tail -40; \
       echo "probe_rc=$?"; \
       echo "--- hvx DBG ---"; sudo dmesg | grep -aE "hvx DBG" | tail -20; \
       echo "--- dmesg health ---"; sudo dmesg | grep -aiE "bug|oops|stall|rcu|taint" | tail -6; \
       sudo rmmod hvx; echo "rmmod=$?"'
