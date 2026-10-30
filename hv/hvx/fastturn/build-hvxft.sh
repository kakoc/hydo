#!/usr/bin/env bash
# Build hvxft.ko (a copy of hvx.ko with device /dev/hvxft) on the bare-metal test machine, in its
# own tree ~/hydo-ft (userspace sources in ~/hydo-ft/hvx, the module in
# ~/hydo-ft/hvxft). Never touches hvx.ko or ~/hydo.
#   ./build-hvxft.sh          sync + build only
#   ./build-hvxft.sh load     also rmmod hvxft (if loaded) and insmod it
#   ./build-hvxft.sh unload   rmmod hvxft only
set -euo pipefail
BOX="${HVX_METAL:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ "${1:-}" = "unload" ]; then
  ssh -o BatchMode=yes "$BOX" "if lsmod | grep -q '^hvxft '; then sudo rmmod hvxft; fi; lsmod | grep '^hvxft ' || echo 'hvxft not loaded'"
  exit 0
fi
ssh -o BatchMode=yes "$BOX" 'mkdir -p ~/hydo-ft/hvx ~/hydo-ft/hvxft'
rsync -az --exclude '*.o' --exclude '*.ko' --exclude '.*.cmd' "$HERE/../" "$BOX:hydo-ft/hvx/"
ssh -o BatchMode=yes "$BOX" "cd ~/hydo-ft/hvxft && cp -f ../hvx/hvx.c hvxft.c && cp -f ../hvx/hvx_abi.h . && cp -f ../hvx/fastturn/Makefile.hvxft Makefile && make 2>&1 | grep -E 'error|warning: |LD \[M\]'"
if [ "${1:-}" = "load" ]; then
  ssh -o BatchMode=yes "$BOX" "if lsmod | grep -q '^hvxft '; then sudo rmmod hvxft; fi; sudo insmod ~/hydo-ft/hvxft/hvxft.ko && echo 'hvxft loaded' && ls -la /dev/hvxft"
fi
