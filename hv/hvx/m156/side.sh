#!/usr/bin/env bash
# M156: build an hvx.c as a side module NAME.ko (device /dev/NAME) on the bare-metal machine in
# ~/hydo-m156/mod/NAME, next to the shared hvx.ko, which is never touched.
#   side.sh NAME SRC build     SRC = a git revision (its hv/hvx/hvx.c and hvx_abi.h) or a file
#   side.sh NAME SRC load      build, then (re)load NAME next to hvx.ko
#   side.sh NAME - unload      remove NAME
# A load or unload writes ~/m156-side-loading (with the time) while it runs and removes it after.
# A reload waits (up to 10 minutes) until no process has /dev/NAME open.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
. "$ROOT/machines.sh"; need_bare_metal; BOX="$BARE_METAL"
NAME=${1:?name}; SRC=${2:?src}; what=${3:-build}
case "$NAME" in hvx) echo "refusing: hvx is the shared main module" >&2; exit 2;; esac
case "$what" in
  unload)
    on_bare_metal "if lsmod | grep -q '^$NAME '; then date > ~/m156-side-loading; sudo rmmod $NAME; rm -f ~/m156-side-loading; fi; lsmod | grep '^hvx' || true"
    exit 0;;
  build|load) ;;
  *) echo "usage: side.sh NAME SRC [build|load|unload]" >&2; exit 2;;
esac
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
if [ -f "$SRC" ]; then
  cp "$SRC" "$TMP/hvx.c"; cp "$(dirname "$SRC")/hvx_abi.h" "$TMP/hvx_abi.h"
else
  git -C "$ROOT" show "$SRC:hv/hvx/hvx.c" > "$TMP/hvx.c"
  git -C "$ROOT" show "$SRC:hv/hvx/hvx_abi.h" > "$TMP/hvx_abi.h"
fi
on_bare_metal "mkdir -p ~/hydo-m156/mod/$NAME"
rsync -a "$TMP/hvx.c" "$TMP/hvx_abi.h" "$BOX:hydo-m156/mod/$NAME/"
on_bare_metal "cd ~/hydo-m156/mod/$NAME && cp -f hvx.c $NAME.c && printf '%s\n' \
  'obj-m := $NAME.o' \
  'ccflags-y := -DHVX_DEVNAME=\\\"$NAME\\\"' \
  'OBJECT_FILES_NON_STANDARD_$NAME.o := y' \
  'KDIR ?= /lib/modules/\$(shell uname -r)/build' \
  'all:' \
  '	\$(MAKE) -C \$(KDIR) M=\$(CURDIR) modules' > Makefile
make 2>&1 | grep -E ' error |warning: |error:' || true
test -f $NAME.ko && echo \"$NAME srcversion \$(modinfo -F srcversion $NAME.ko) sha256 \$(sha256sum < hvx.c | cut -c1-16)\""
if [ "$what" = load ]; then
  on_bare_metal "set -e; date > ~/m156-side-loading
    if lsmod | grep -q '^$NAME '; then
      n=0; while sudo fuser -s /dev/$NAME 2>/dev/null && [ \$n -lt 120 ]; do sleep 5; n=\$((n+1)); done
      sudo rmmod $NAME
    fi
    sudo insmod ~/hydo-m156/mod/$NAME/$NAME.ko && ls -la /dev/$NAME && cat /sys/module/$NAME/srcversion
    sudo dmesg | grep -a '/dev/$NAME' | tail -1
    rm -f ~/m156-side-loading"
fi
