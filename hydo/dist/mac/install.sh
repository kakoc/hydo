#!/bin/sh
# Install hydo for Apple Silicon Macs from an unpacked release directory:
#     ./install.sh [--prefix DIR] [--add-to-path]
# Copies the release into DIR (default ~/.hydo). Everything else is the hydo program's:
#   hydo setup    checks this Mac (Apple Silicon, macOS 13 or later, Hypervisor.framework) and
#                 every file against SHA256SUMS and the sha256 pins built into hydo, removes the
#                 quarantine attribute of a download, and checks the signature of the engine
#                 bin/hydo-hvm (the release comes signed and notarized; an unsigned engine, one
#                 built from source, is signed ad hoc with the hypervisor entitlement)
#                 (--add-to-path: also puts DIR/bin on PATH in your shell startup file);
#   hydo doctor   checks this Mac and the installation.
# It installs nothing outside DIR and needs no sudo.
set -e
SRC=$(cd "$(dirname "$0")" && pwd)
PREFIX="$HOME/.hydo"
SETUP=
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX=$2; shift 2 ;;
    --add-to-path) SETUP=--add-to-path; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

if [ "$SRC" != "$(cd "$PREFIX" 2>/dev/null && pwd)" ]; then
  mkdir -p "$PREFIX"
  ( cd "$SRC" && tar cf - . ) | ( cd "$PREFIX" && tar xf - )
fi
# macOS refuses to start a downloaded program that still carries the quarantine attribute, so
# bin/hydo is cleared before it runs (hydo setup clears the other files)
xattr -d com.apple.quarantine "$PREFIX/bin/hydo" 2>/dev/null || true
"$PREFIX/bin/hydo" setup $SETUP || { echo "install.sh: hydo setup failed (see above; hydo needs an Apple Silicon Mac)" >&2; exit 1; }
"$PREFIX/bin/hydo" doctor
echo
echo "hydo $(cat "$PREFIX/VERSION") is installed in $PREFIX."
echo "Next: export PATH=\"$PREFIX/bin:\$PATH\", then follow $PREFIX/QUICKSTART-mac.md"
echo "(first step: cp -R \"$PREFIX/examples/redis\" ~/hydo-redis && cd ~/hydo-redis && hydo build hydo.yaml)"
