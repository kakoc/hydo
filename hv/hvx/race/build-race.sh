#!/bin/bash
# Build the calibration-race guest on the bare-metal machine: a static hydorace, and an
# initramfs = the stock busybox initramfs + a second cpio that replaces /init and
# adds /bin/hydorace (the kernel extracts concatenated archives in order).
set -euo pipefail
cd "$(dirname "$0")"
B=${B:-$HOME/hydo/build}
gcc -O1 -static -pthread -o hydorace hydorace.c
rm -rf ovl && mkdir -p ovl/bin
cp init ovl/init && chmod 755 ovl/init
cp hydorace ovl/bin/hydorace
touch -d @0 ovl ovl/bin ovl/init ovl/bin/hydorace
(cd ovl && printf 'init\nbin\nbin/hydorace\n' | cpio -o -H newc --reproducible 2>/dev/null | gzip -n) > ovl.cpio.gz
OUTIMG=${OUTIMG:-initramfs-race.cpio.gz}
cat "$B/initramfs.cpio.gz" ovl.cpio.gz > "$B/$OUTIMG"
sha256sum hydorace "$B/$OUTIMG"
