#!/bin/sh
# fetch-modules.sh: fill out/modules/ with the hvx guest kernel's loop and squashfs modules,
# uncompressed (the device's boot loads them with busybox insmod). The Mac engine's guest
# kernel has loop built in and no squashfs (build/rauc-unsquashfs.patch); it needs none.
# Run it on Linux: the modules come from the guest kernel's build output, the nixpkgs store
# path below (nix-store -r fetches it from cache.nixos.org when Nix is on PATH).
set -eu
cd "$(dirname "$0")"
KSTORE=/nix/store/rp8637p734k6rshqjqlnfdm97b4jn8w9-linux-6.6.35
K=$KSTORE/lib/modules/6.6.35/kernel
[ -d "$K" ] || nix-store -r "$KSTORE" > /dev/null
umask 022
mkdir -p out/modules
xz -dc "$K/drivers/block/loop.ko.xz" > out/modules/loop.ko
xz -dc "$K/fs/squashfs/squashfs.ko.xz" > out/modules/squashfs.ko
ls -l out/modules
