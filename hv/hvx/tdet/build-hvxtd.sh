#!/usr/bin/env bash
# Build hvxtd.ko (a copy of this tree's hvx.c with device /dev/hvxtd) on the bare-metal test machine in
# ~/hydo-tdet/hvxtd, and with "load" (re)load it next to the main hvx.ko. Never touches hvx.ko.
# Insmod parameters (e.g. chaos_ptimer=2000 chaos_preempt=5) can be given in HVXTD_PARAMS.
set -euo pipefail
BOX="${HVX_METAL:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ssh -o BatchMode=yes "$BOX" 'mkdir -p ~/hydo-tdet/hvxtd'
rsync -az "$HERE/../hvx.c" "$HERE/../hvx_abi.h" "$BOX:hydo-tdet/hvxtd/"
rsync -az "$HERE/Makefile.hvxtd" "$BOX:hydo-tdet/hvxtd/Makefile"
ssh -o BatchMode=yes "$BOX" "cd ~/hydo-tdet/hvxtd && cp -f hvx.c hvxtd.c && make 2>&1 | grep -E ' error |warning: |LD \[M\]'"
if [ "${1:-}" = "load" ]; then
  ssh -o BatchMode=yes "$BOX" "lsmod | grep -q '^hvxtd ' && sudo rmmod hvxtd; sudo insmod ~/hydo-tdet/hvxtd/hvxtd.ko ${HVXTD_PARAMS:-} && ls -la /dev/hvxtd"
fi
