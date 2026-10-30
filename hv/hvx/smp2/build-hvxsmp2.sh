#!/usr/bin/env bash
# Build hvxsmp2.ko (a copy of hvx.ko with device /dev/hvxsmp2) on the bare-metal test machine, in its
# own tree ~/hydo-smp2 (userspace sources in ~/hydo-smp2/hvx, the module in
# ~/hydo-smp2/hvxsmp2). Never touches hvx.ko or ~/hydo.
#   ./build-hvxsmp2.sh          sync + build only
#   ./build-hvxsmp2.sh load     also rmmod hvxsmp2 (if loaded) and insmod it
#   ./build-hvxsmp2.sh unload   rmmod hvxsmp2 only
set -euo pipefail
BOX="${HVX_METAL:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ "${1:-}" = "unload" ]; then
  ssh -o BatchMode=yes "$BOX" "if lsmod | grep -q '^hvxsmp2 '; then sudo rmmod hvxsmp2; fi; lsmod | grep '^hvxsmp2 ' || echo 'hvxsmp2 not loaded'"
  exit 0
fi
ssh -o BatchMode=yes "$BOX" 'mkdir -p ~/hydo-smp2/hvx ~/hydo-smp2/hvxsmp2'
rsync -az --exclude '*.o' --exclude '*.ko' --exclude '.*.cmd' "$HERE/../" "$BOX:hydo-smp2/hvx/"
ssh -o BatchMode=yes "$BOX" "cd ~/hydo-smp2/hvxsmp2 && cp -f ../hvx/hvx.c hvxsmp2.c && cp -f ../hvx/hvx_abi.h . && cp -f ../hvx/smp2/Makefile.hvxsmp2 Makefile && make 2>&1 | grep -E 'error|warning: |LD \[M\]'"
if [ "${1:-}" = "load" ]; then
  ssh -o BatchMode=yes "$BOX" "if lsmod | grep -q '^hvxsmp2 '; then sudo rmmod hvxsmp2; fi; sudo insmod ~/hydo-smp2/hvxsmp2/hvxsmp2.ko && echo 'hvxsmp2 loaded' && ls -la /dev/hvxsmp2"
fi
