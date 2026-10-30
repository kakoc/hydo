#!/usr/bin/env bash
# Build hvxpt.ko (a copy of hvx.ko with device /dev/hvxpt) on the bare-metal test machine, in
# its own directory ~/hydo/hvxpt, and optionally (re)load it. Never touches hvx.ko.
#   ./build-hvxpt.sh          build only
#   ./build-hvxpt.sh load     build, then rmmod hvxpt (if loaded) and insmod it
# Extra insmod parameters (e.g. pt_force=2 pt_force_lo0=... ) can be given in HVXPT_PARAMS.
set -euo pipefail
BOX="${HVX_METAL:?}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ssh -o BatchMode=yes "$BOX" 'mkdir -p ~/hydo/hvxpt'
rsync -az "$HERE/../hvx.c" "$HERE/../hvx_abi.h" "$BOX:hydo/hvxpt/"
rsync -az "$HERE/Makefile.hvxpt" "$BOX:hydo/hvxpt/Makefile"
ssh -o BatchMode=yes "$BOX" "cd ~/hydo/hvxpt && cp -f hvx.c hvxpt.c && make 2>&1 | grep -E ' error |warning: |LD \[M\]'"
if [ "${1:-}" = "load" ]; then
  ssh -o BatchMode=yes "$BOX" "lsmod | grep -q '^hvxpt ' && sudo rmmod hvxpt; sudo insmod ~/hydo/hvxpt/hvxpt.ko ${HVXPT_PARAMS:-} && echo 'hvxpt loaded' && ls -la /dev/hvxpt && sudo dmesg | grep -a 'Intel PT guest tracing' | tail -1"
fi
