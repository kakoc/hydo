#!/bin/sh
# fetch-modules.sh: fill out/modules/ with the hvx guest kernel's loop module, uncompressed
# (the device's boot loads it with busybox insmod). The Mac engine's guest kernel has loop
# built in; it needs none.
# Run it on Linux: the module comes from the guest kernel's build output, the nixpkgs store
# path below (nix-store -r fetches it from cache.nixos.org when Nix is on PATH).
set -eu
cd "$(dirname "$0")"
KSTORE=/nix/store/rp8637p734k6rshqjqlnfdm97b4jn8w9-linux-6.6.35
K=$KSTORE/lib/modules/6.6.35/kernel
[ -d "$K" ] || nix-store -r "$KSTORE" > /dev/null
umask 022
mkdir -p out/modules
xz -dc "$K/drivers/block/loop.ko.xz" > out/modules/loop.ko
ls -l out/modules
