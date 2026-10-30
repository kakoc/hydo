#!/bin/sh
# Install hydo for Apple Silicon Macs from an unpacked release directory:
#     ./install.sh [--prefix DIR] [--add-to-path]
# Copies the release into DIR (default ~/.hydo), signs the two VMM binaries on this Mac with the
# hypervisor entitlement, and runs `hydo doctor`. It installs nothing outside DIR and needs no
# sudo. Your shell startup file is changed only with --add-to-path.
set -e
SRC=$(cd "$(dirname "$0")" && pwd)
PREFIX="$HOME/.hydo"
ADD_PATH=0
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX=$2; shift 2 ;;
    --add-to-path) ADD_PATH=1; shift ;;
    -h|--help) sed -n '2,7p' "$0"; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || { echo "hydo needs an Apple Silicon Mac (M1 or later)." >&2; exit 1; }
major=$(sw_vers -productVersion | cut -d. -f1)
[ "$major" -ge 13 ] || { echo "hydo needs macOS 13 or later (found $(sw_vers -productVersion))." >&2; exit 1; }
[ "$(sysctl -n kern.hv_support 2>/dev/null)" = 1 ] || { echo "This Mac does not offer Hypervisor.framework (kern.hv_support is not 1)." >&2; exit 1; }
command -v python3 >/dev/null || { echo "hydo needs python3 (macOS offers it with the Command Line Tools: xcode-select --install)." >&2; exit 1; }

if [ "$SRC" != "$(cd "$PREFIX" 2>/dev/null && pwd)" ]; then
  mkdir -p "$PREFIX"
  ( cd "$SRC" && tar cf - . ) | ( cd "$PREFIX" && tar xf - )
fi
# a downloaded archive carries the quarantine attribute; the VMM binaries must be signed on
# this Mac with the hypervisor entitlement (an ad-hoc signature is enough)
xattr -dr com.apple.quarantine "$PREFIX" 2>/dev/null || true
for b in "$PREFIX/hvx-mac/hvm" "$PREFIX/hvx-mac/explore/hvmx"; do
  codesign --force --entitlements "$PREFIX/hvx-mac/hv.entitlements" -s - "$b" 2>/dev/null
done
chmod 755 "$PREFIX/bin/hydo"

if [ $ADD_PATH = 1 ]; then
  rc="$HOME/.zshrc"; [ -n "$BASH_VERSION" ] && rc="$HOME/.bash_profile"
  line="export PATH=\"$PREFIX/bin:\$PATH\""
  grep -qxF "$line" "$rc" 2>/dev/null || { echo "$line" >> "$rc"; echo "added $PREFIX/bin to PATH in $rc"; }
fi

"$PREFIX/bin/hydo" doctor
echo
echo "hydo $(cat "$PREFIX/VERSION") is installed in $PREFIX."
echo "Next: export PATH=\"$PREFIX/bin:\$PATH\", then follow $PREFIX/QUICKSTART-mac.md"
echo "(first step: cd \"$PREFIX/examples/redis\" && hydo build hydo.yaml)"
