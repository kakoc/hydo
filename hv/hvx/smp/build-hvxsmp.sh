#!/usr/bin/env bash
# Build hvxsmp.ko (a copy of hvx.ko with device /dev/hvxsmp) on the bare-metal machine, in its
# own tree ~/hydo-smp (userspace sources in ~/hydo-smp/hvx, the module in
# ~/hydo-smp/hvxsmp). Never touches hvx.ko or ~/hydo.
#   ./build-hvxsmp.sh          sync + build only
#   ./build-hvxsmp.sh load     also rmmod hvxsmp (if loaded) and insmod it
#   ./build-hvxsmp.sh unload   rmmod hvxsmp only
set -euo pipefail
. "$(cd "$(dirname "$0")/../../.." && pwd)/machines.sh"; need_bare_metal; BOX="$BARE_METAL"
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ "${1:-}" = "unload" ]; then
  on_bare_metal "if lsmod | grep -q '^hvxsmp '; then sudo rmmod hvxsmp; fi; lsmod | grep '^hvxsmp ' || echo 'hvxsmp not loaded'"
  exit 0
fi
on_bare_metal 'mkdir -p ~/hydo-smp/hvx ~/hydo-smp/hvxsmp'
rsync -az --exclude '*.o' --exclude '*.ko' --exclude '.*.cmd' "$HERE/../" "$BOX:hydo-smp/hvx/"
on_bare_metal "cd ~/hydo-smp/hvxsmp && cp -f ../hvx/hvx.c hvxsmp.c && cp -f ../hvx/hvx_abi.h . && cp -f ../hvx/smp/Makefile.hvxsmp Makefile && make 2>&1 | grep -E 'error|warning: |LD \[M\]'"
if [ "${1:-}" = "load" ]; then
  on_bare_metal "if lsmod | grep -q '^hvxsmp '; then sudo rmmod hvxsmp; fi; sudo insmod ~/hydo-smp/hvxsmp/hvxsmp.ko && echo 'hvxsmp loaded' && ls -la /dev/hvxsmp"
fi
