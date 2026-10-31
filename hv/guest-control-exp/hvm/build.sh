#!/bin/sh
# M116 experiment: build gc_hvmx (hvmx + the timed-input hook of gc_hvm.c) on the Mac.
set -e
cd "$(dirname "$0")"
X=../../hvm/explore
mkdir -p gen
sed -e 's/hvm_stop_hook = marker_hook;/hvm_stop_hook = gc_hook;/' \
    -e 's/^\thvm_put_input(line);$/\thvm_put_input(line); gc_arm(line);/' $X/serve.c > gen/serve_gc.c
grep -q 'gc_hook;' gen/serve_gc.c && grep -q 'gc_arm(line)' gen/serve_gc.c
/usr/bin/clang -O2 -Wall -Wno-unused-function -arch arm64 -I$X -o gc_hvmx gc_hvm.c ../../hvm/virtio.c ../../hvm/virtio_blk.c ../../hvm/virtio_net.c -framework Hypervisor -lz
codesign --force --entitlements ../../hvm/hv.entitlements -s - gc_hvmx
