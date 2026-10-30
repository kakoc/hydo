#!/usr/bin/env bash
# Build hvxavx.ko (this tree's hvx.c with device /dev/hvxavx) on the bare-metal test machine in
# ~/hydo-avx/hvxavx; with "load" (re)load it next to the main hvx.ko; with "unload" remove it.
# Never touches hvx.ko or any other side module.
set -euo pipefail
BOX="${HVX_METAL:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
if [ "${1:-}" = "unload" ]; then
  ssh -o BatchMode=yes "$BOX" "lsmod | grep -q '^hvxavx ' && sudo rmmod hvxavx; lsmod | grep '^hvx' || true"
  exit 0
fi
ssh -o BatchMode=yes "$BOX" 'mkdir -p ~/hydo-avx/hvxavx'
rsync -az "$HERE/../hvx.c" "$HERE/../hvx_abi.h" "$BOX:hydo-avx/hvxavx/"
rsync -az "$HERE/Makefile.hvxavx" "$BOX:hydo-avx/hvxavx/Makefile"
ssh -o BatchMode=yes "$BOX" "cd ~/hydo-avx/hvxavx && cp -f hvx.c hvxavx.c && make 2>&1 | grep -E ' error |warning: |LD \[M\]'"
if [ "${1:-}" = "load" ]; then
  ssh -o BatchMode=yes "$BOX" "lsmod | grep -q '^hvxavx ' && sudo rmmod hvxavx; sudo insmod ~/hydo-avx/hvxavx/hvxavx.ko && ls -la /dev/hvxavx"
fi
