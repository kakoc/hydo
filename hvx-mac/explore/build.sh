#!/bin/sh
# Build and ad-hoc sign hvmx (the explorer engine for the Mac VMM; needs the hypervisor entitlement).
set -e
cd "$(dirname "$0")"
/usr/bin/clang -O2 -Wall -Wno-unused-function -arch arm64 -o hvmx serve.c ../virtio.c ../virtio_blk.c ../virtio_net.c -framework Hypervisor
codesign --force --entitlements ../hv.entitlements -s - hvmx
