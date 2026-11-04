#!/usr/bin/env bash
# M138: build this tree's hvx.c as the side module hvxgc.ko (device /dev/hvxgc) on the bare-metal
# machine in ~/hydo-mod/hvxgc, next to the shared hvx.ko, which is never touched.
#   build-hvxgc.sh            build only (prints the srcversion)
#   build-hvxgc.sh load       build, then (re)load hvxgc next to hvx.ko
#   build-hvxgc.sh unload     remove hvxgc
# A load or unload writes ~/m138-side-loading on the bare-metal machine (with the time) while it runs and
# removes it after, so other sessions can see that a side module is being (re)loaded. A reload
# waits (up to 10 minutes) until no process has /dev/hvxgc open.
set -euo pipefail
. "$(cd "$(dirname "$0")/../../.." && pwd)/machines.sh"; need_bare_metal; BOX="$BARE_METAL"
HERE="$(cd "$(dirname "$0")" && pwd)"
what="${1:-build}"
case "$what" in
  unload)
    on_bare_metal 'if lsmod | grep -q "^hvxgc "; then date > ~/m138-side-loading; sudo rmmod hvxgc; rm -f ~/m138-side-loading; fi; lsmod | grep "^hvx" || true'
    exit 0;;
  build|load) ;;
  *) echo "usage: build-hvxgc.sh [build|load|unload]" >&2; exit 2;;
esac
on_bare_metal 'mkdir -p ~/hydo-mod/hvxgc'
rsync -az "$HERE/../hvx.c" "$HERE/../hvx_abi.h" "$BOX:hydo-mod/hvxgc/"
on_bare_metal 'cd ~/hydo-mod/hvxgc && cp -f hvx.c hvxgc.c && printf "%s\n" \
  "obj-m := hvxgc.o" \
  "ccflags-y := -DHVX_DEVNAME=\\\"hvxgc\\\"" \
  "OBJECT_FILES_NON_STANDARD_hvxgc.o := y" \
  "KDIR ?= /lib/modules/\$(shell uname -r)/build" \
  "all:" \
  "	\$(MAKE) -C \$(KDIR) M=\$(CURDIR) modules" > Makefile
make 2>&1 | grep -E " error |warning: |error:" || true
test -f hvxgc.ko && echo "srcversion $(modinfo -F srcversion hvxgc.ko)"'
if [ "$what" = load ]; then
  on_bare_metal 'set -e; date > ~/m138-side-loading
    if lsmod | grep -q "^hvxgc "; then
      n=0; while sudo fuser -s /dev/hvxgc 2>/dev/null && [ $n -lt 120 ]; do sleep 5; n=$((n+1)); done
      sudo rmmod hvxgc
    fi
    sudo insmod ~/hydo-mod/hvxgc/hvxgc.ko && ls -la /dev/hvxgc && cat /sys/module/hvxgc/srcversion
    sudo dmesg | grep -a "/dev/hvxgc" | tail -1
    rm -f ~/m138-side-loading'
fi
