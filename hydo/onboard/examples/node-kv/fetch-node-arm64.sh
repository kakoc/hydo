#!/usr/bin/env bash
# Stage the aarch64-linux Node.js for hydo-mac.yaml into files/ (git-ignored): nodejs-slim 22.2.0
# from the pinned nixpkgs (NixOS 24.05, revision 89c4987; docs/BUILD-PINS.md), fetched from the
# nixpkgs binary cache on a Linux machine with nix (the build machine, or with
# HYDO_NIX_MACHINE=bare-metal the bare-metal machine; machines.sh at the repository root), and only the shared libraries its loader opens (found with hydo-elfdeps, hydo/crates/hydo-tools).
# The one absolute symlink (libgcc_s.so.1 into another store path) is made relative, as the
# Redis example of the Mac release does; in the guest it resolves to the same file.
# Usage: hydo/onboard/examples/node-kv/fetch-node-arm64.sh     (checks files.sha256 at the end)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$(cd "$HERE/../../../.." && pwd)/machines.sh"; need_nix_machine
NODE=/nix/store/vh315js3yiw2j58jfm5m934snawzzykw-nodejs-slim-22.2.0
FILES="
$NODE/bin/node
/nix/store/5nrc0aihwsmns1g8aa9b2d0k60cjax7n-zlib-1.3.1/lib/libz.so.1
/nix/store/5nrc0aihwsmns1g8aa9b2d0k60cjax7n-zlib-1.3.1/lib/libz.so.1.3.1
/nix/store/8sz64jcd47ihhd2y3yd40jya9v7lzjjy-glibc-2.39-52/lib/ld-linux-aarch64.so.1
/nix/store/8sz64jcd47ihhd2y3yd40jya9v7lzjjy-glibc-2.39-52/lib/libc.so.6
/nix/store/8sz64jcd47ihhd2y3yd40jya9v7lzjjy-glibc-2.39-52/lib/libdl.so.2
/nix/store/8sz64jcd47ihhd2y3yd40jya9v7lzjjy-glibc-2.39-52/lib/libm.so.6
/nix/store/8sz64jcd47ihhd2y3yd40jya9v7lzjjy-glibc-2.39-52/lib/libpthread.so.0
/nix/store/c6w5xa1bcyhqn2dpwi8lvhpp183k72xm-icu4c-73.2/lib/libicudata.so.73
/nix/store/c6w5xa1bcyhqn2dpwi8lvhpp183k72xm-icu4c-73.2/lib/libicudata.so.73.2
/nix/store/c6w5xa1bcyhqn2dpwi8lvhpp183k72xm-icu4c-73.2/lib/libicui18n.so.73
/nix/store/c6w5xa1bcyhqn2dpwi8lvhpp183k72xm-icu4c-73.2/lib/libicui18n.so.73.2
/nix/store/c6w5xa1bcyhqn2dpwi8lvhpp183k72xm-icu4c-73.2/lib/libicuuc.so.73
/nix/store/c6w5xa1bcyhqn2dpwi8lvhpp183k72xm-icu4c-73.2/lib/libicuuc.so.73.2
/nix/store/hallr4n38j24j5ca3wnig9dg4xnngya0-libuv-1.48.0/lib/libuv.so.1
/nix/store/hallr4n38j24j5ca3wnig9dg4xnngya0-libuv-1.48.0/lib/libuv.so.1.0.0
/nix/store/p27dxq20hdvm8xjsgj4n103cl02x4jr4-openssl-3.0.14/lib/libcrypto.so.3
/nix/store/p27dxq20hdvm8xjsgj4n103cl02x4jr4-openssl-3.0.14/lib/libssl.so.3
/nix/store/pp8jrab5a9qfpn3la57r37x8kw2hrcil-gcc-13.2.0-libgcc/lib/libgcc_s.so.1
/nix/store/x884b7ks41l0zvfvdw1m3xb0l3j1yjlc-gcc-13.2.0-lib/lib/libgcc_s.so.1
/nix/store/x884b7ks41l0zvfvdw1m3xb0l3j1yjlc-gcc-13.2.0-lib/lib/libstdc++.so.6
/nix/store/x884b7ks41l0zvfvdw1m3xb0l3j1yjlc-gcc-13.2.0-lib/lib/libstdc++.so.6.0.32
"
rel=$(for f in $FILES; do printf '%s ' "${f#/}"; done)
on_nix_machine "nix-store -r $NODE >/dev/null 2>&1 && cd / && tar cf - $rel" | (mkdir -p "$HERE/files" && cd "$HERE/files" && tar xf - && chmod -R u+w nix)
L="$HERE/files/nix/store/x884b7ks41l0zvfvdw1m3xb0l3j1yjlc-gcc-13.2.0-lib/lib/libgcc_s.so.1"
case $(readlink "$L") in
  /*) ln -sfn ../../pp8jrab5a9qfpn3la57r37x8kw2hrcil-gcc-13.2.0-libgcc/lib/libgcc_s.so.1 "$L" ;;
esac
cd "$HERE" && shasum -a 256 -c files.sha256
