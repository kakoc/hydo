#!/usr/bin/env bash
# Build the hvx debug test kernel (Milestone 97). Runs ON THE BUILD MACHINE BUILD BOX (the build machine),
# never on the bare-metal test machine. Output: $SAFETY/kbuild/{bzImage,vmlinux,.config} and the kernel
# tree the module is built against ($SAFETY/kbuild/linux).
#
# Inputs are pinned: the kernel tarball by sha256 (kernel.org), the toolchain by the nixpkgs
# revision the other image builds use (hvx/BUILD-PINS.md), and the build identity variables, so
# a rebuild gives the same bzImage. Needs about 9 GB of disk while it builds (KASAN + debug info).
set -euo pipefail
SAFETY=${SAFETY:-$HOME/hydo-safety}
HERE="$(cd "$(dirname "$0")" && pwd)"
KVER=6.8.12
KSHA=19b31956d229b5b9ca5671fa1c74320179682a3d8d00fc86794114b21da86039
NIXPKGS=/nix/store/f0ddmw6s86y567yg06h5019z72szbzch-source	# NixOS 24.05 @ 89c4987
JOBS=${JOBS:-12}

avail=$(df -BG --output=avail "$HOME" | tail -1 | tr -dc 0-9)
[ "$avail" -ge 15 ] || { echo "build-kernel: only ${avail} GB free in \$HOME; need 15" >&2; exit 1; }

mkdir -p "$SAFETY/dl" "$SAFETY/kbuild"
TAR="$SAFETY/dl/linux-$KVER.tar.xz"
[ -f "$TAR" ] || curl -sfL -o "$TAR" "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KVER.tar.xz"
echo "$KSHA  $TAR" | sha256sum -c --quiet

K="$SAFETY/kbuild/linux"
if [ ! -f "$K/.hydo-unpacked-$KVER" ]; then
  rm -rf "$K"; mkdir -p "$K"
  tar -xJf "$TAR" -C "$K" --strip-components=1
  touch "$K/.hydo-unpacked-$KVER"
fi
cp "$HERE/kernel/hvx-debug.config" "$K/kernel/configs/hvx-debug.config"

# The nixpkgs cc-wrapper adds userspace hardening flags (-fPIC, -fstack-protector-strong,
# _FORTIFY_SOURCE) to every compile; the kernel chooses its own, so they are switched off.
nix-shell -I nixpkgs="$NIXPKGS" -p gnumake flex bison bc elfutils openssl perl python3 cpio zstd xz \
  --run "set -euo pipefail
  export NIX_HARDENING_ENABLE=''
  export KBUILD_BUILD_TIMESTAMP='Thu Jan  1 00:00:00 UTC 2026' KBUILD_BUILD_USER=hydo \
         KBUILD_BUILD_HOST=hydo-safety KBUILD_BUILD_VERSION=1 SOURCE_DATE_EPOCH=1767225600
  cd '$K'
  make -s defconfig kvm_guest.config hvx-debug.config
  # every option of the fragment must have survived olddefconfig's dependency resolution
  bad=0
  while IFS= read -r l; do
    case \"\$l\" in CONFIG_*=n) o=\${l%%=*}; grep -q \"^\$o=\" .config && { echo \"not off: \$o\"; bad=1; } ;;
                   CONFIG_*) grep -qxF \"\$l\" .config || { echo \"not set: \$l\"; bad=1; } ;; esac
  done < kernel/configs/hvx-debug.config
  [ \$bad = 0 ] || { echo 'build-kernel: config fragment not applied' >&2; exit 1; }
  nice -n 10 make -j$JOBS bzImage modules_prepare
  make -s headers_install INSTALL_HDR_PATH=usr >/dev/null
"
cp "$K/arch/x86/boot/bzImage" "$K/vmlinux" "$K/.config" "$SAFETY/kbuild/"
sha256sum "$SAFETY/kbuild/bzImage" | tee "$SAFETY/kbuild/bzImage.sha256"
echo "build-kernel: OK ($KVER, $(du -sh "$K" | cut -f1) tree)"
