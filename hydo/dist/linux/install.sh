#!/bin/sh
# Install hydo for x86_64 Linux (Intel VT-x) from an unpacked release directory:
#     ./install.sh [--prefix DIR] [--add-to-path] [--no-module] [--udev [--udev-group G] [--udev-name N]]
# Copies the release into DIR (default ~/.hydo; an earlier hydo installation there is replaced,
# any other non-empty directory refused). Everything else is the hydo program's (M154):
#   hydo setup            checks every file against SHA256SUMS and the sha256 pins built into
#                         hydo, unpacks the guest store tree, makes the programs executable
#                         (--add-to-path: also puts DIR/bin on PATH in your shell startup file);
#   hydo module install   builds the hvx kernel module for the running kernel and loads it (sudo;
#                         this unloads kvm_intel and kvm); --no-module skips it;
#   hydo module udev install   only with --udev (asks for sudo once): a udev rule gives /dev/hvx
#                         to group G (default hydo, created if missing; you are added to it), mode
#                         0660, so runs need no sudo. Anyone in that group can run guests through
#                         the kernel module: give it only to people you would give sudo.
#                         --udev-name N does the same for a side module's device /dev/N;
#   hydo doctor           checks this machine and the installation (with --no-module on a machine
#                         where only /dev/kvm is usable: hydo doctor --backend kvm).
set -e
SRC=$(cd "$(dirname "$0")" && pwd)
PREFIX="$HOME/.hydo"
SETUP=
MODULE=1
UDEV=0
UDEV_GROUP=hydo
UDEV_NAME=hvx
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX=$2; shift 2 ;;
    --add-to-path) SETUP=--add-to-path; shift ;;
    --no-module) MODULE=0; shift ;;
    --udev) UDEV=1; shift ;;
    --udev-group) UDEV_GROUP=$2; shift 2 ;;
    --udev-name) UDEV_NAME=$2; shift 2 ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

# the unpacked release is checked before it replaces anything (this also fails on a machine that
# cannot run its x86_64 Linux programs)
"$SRC/bin/hydo" setup --verify || { echo "install.sh: the release cannot be installed (see above)" >&2; exit 1; }
if [ "$SRC" != "$(cd "$PREFIX" 2>/dev/null && pwd)" ]; then
  if [ -d "$PREFIX" ] && [ -n "$(ls -A "$PREFIX")" ]; then     # replace an earlier install, nothing else
    [ -f "$PREFIX/bin/hydo" ] && [ -f "$PREFIX/VERSION" ] || { echo "$PREFIX exists and is not a hydo installation; choose another --prefix" >&2; exit 1; }
    chmod -R u+w "$PREFIX"; rm -rf "$PREFIX"                    # (the guest store tree is read-only)
  fi
  mkdir -p "$PREFIX"
  ( cd "$SRC" && tar cf - . ) | ( cd "$PREFIX" && tar xpf - )
fi
P=$(cd "$PREFIX" && pwd)

"$P/bin/hydo" setup $SETUP
[ $MODULE = 0 ] || "$P/bin/hydo" module install || echo "the kernel module was not installed (see above); fix it, then: hydo module install"
[ $UDEV = 0 ] || "$P/bin/hydo" module udev install --name "$UDEV_NAME" --group "$UDEV_GROUP" || echo "the udev rule was not installed (see above); runs keep using sudo"
echo
# with --no-module on a machine where only KVM is usable (no /dev/hvx, /dev/kvm open to this user),
# check for the KVM engine, the one this machine can run (since 0.3.0)
if [ $MODULE = 0 ] && [ ! -e /dev/hvx ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
  "$P/bin/hydo" doctor --backend kvm || true
  KVM_ONLY=1
else
  "$P/bin/hydo" doctor || true
  KVM_ONLY=0
fi
echo
echo "hydo $(cat "$P/VERSION") is installed in $P."
echo "Next: export PATH=\"$P/bin:\$PATH\"; then follow $P/README.md"
echo "(first step: cp -R \"$P/examples/redis\" ~/hydo-redis && cd ~/hydo-redis && hydo build hydo.yaml)"
[ $KVM_ONLY = 0 ] || echo "(this machine runs the KVM engine: add --backend kvm to hydo run, repro and explore, or export HYDO_BACKEND=kvm)"
