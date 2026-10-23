#!/bin/sh
# Install hydo for x86_64 Linux (Intel VT-x) from an unpacked release directory:
#     ./install.sh [--prefix DIR] [--add-to-path] [--no-module]
# Checks the release's SHA256SUMS, copies it into DIR (default ~/.hydo), compiles the run engine
# (hvx/hvx_explore) and the hvx kernel module for the running kernel, and runs `hydo doctor`.
# It needs no root and installs nothing outside DIR; it does NOT load the module (that needs
# sudo: `hydo module load`). Your shell startup file is changed only with --add-to-path.
set -e
SRC=$(cd "$(dirname "$0")" && pwd)
PREFIX="$HOME/.hydo"
ADD_PATH=0
MODULE=1
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) PREFIX=$2; shift 2 ;;
    --add-to-path) ADD_PATH=1; shift ;;
    --no-module) MODULE=0; shift ;;
    -h|--help) sed -n '2,7p' "$0"; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || { echo "hydo's Linux release needs x86_64 Linux (on an Apple Silicon Mac use the Mac release)." >&2; exit 1; }
command -v python3 >/dev/null || { echo "hydo needs python3 (3.9 or later)." >&2; exit 1; }
python3 -c 'import sys; sys.exit(sys.version_info < (3, 9))' || { echo "hydo needs Python 3.9 or later (found $(python3 --version))." >&2; exit 1; }
command -v cc >/dev/null || { echo "hydo needs a C compiler (cc) to build its run engine (Debian/Ubuntu: sudo apt install build-essential)." >&2; exit 1; }

( cd "$SRC" && sha256sum -c --quiet SHA256SUMS ) || { echo "the release does not match its SHA256SUMS (a damaged download?)" >&2; exit 1; }

if [ "$SRC" != "$(cd "$PREFIX" 2>/dev/null && pwd)" ]; then
  if [ -d "$PREFIX" ] && [ -n "$(ls -A "$PREFIX")" ]; then     # replace an earlier install, nothing else
    [ -f "$PREFIX/bin/hydo" ] && [ -f "$PREFIX/VERSION" ] || { echo "$PREFIX exists and is not a hydo installation; choose another --prefix" >&2; exit 1; }
    chmod -R u+w "$PREFIX"; rm -rf "$PREFIX"                    # (the guest store tree is read-only)
  fi
  mkdir -p "$PREFIX"
  ( cd "$SRC" && tar cf - . ) | ( cd "$PREFIX" && tar xpf - )
fi
P=$(cd "$PREFIX" && pwd)
# the guest-side Nix closures (tc, iptables) keep their read-only modes: unpack them here
if [ -f "$P/guest/store.tar" ]; then
  mkdir -p "$P/guest/store" && tar -C "$P/guest/store" -xpf "$P/guest/store.tar" && rm -f "$P/guest/store.tar"
fi

# the run engine, against this release's hvx_abi.h; libipt (coverage decoding) is optional
cd "$P/hvx"
if cc -O2 -Wall -o hvx_explore hvx_explore.c -lipt 2>/dev/null; then
  echo "built hvx/hvx_explore (with libipt: coverage decoding available)"
else
  cc -O2 -Wall -DHYDO_NO_LIBIPT -o hvx_explore hvx_explore.c
  echo "built hvx/hvx_explore (without libipt: coverage decoding off; install libipt-dev to get it)"
fi
cd "$P"
chmod 755 bin/hydo

if [ $MODULE = 1 ]; then
  if [ -d "/lib/modules/$(uname -r)/build" ]; then
    bin/hydo module build || echo "building the kernel module failed (see above); fix it, then: hydo module build"
  else
    echo "no kernel headers for $(uname -r): the module is not built (install them, then: hydo module build)"
  fi
fi

if [ $ADD_PATH = 1 ]; then
  rc="$HOME/.bashrc"; [ -n "$ZSH_VERSION" ] || [ "$(basename "${SHELL:-}")" = zsh ] && rc="$HOME/.zshrc"
  line="export PATH=\"$P/bin:\$PATH\""
  grep -qxF "$line" "$rc" 2>/dev/null || { echo "$line" >> "$rc"; echo "added $P/bin to PATH in $rc"; }
fi

echo
"$P/bin/hydo" doctor || true
echo
echo "hydo $(cat "$P/VERSION") is installed in $P."
echo "Next: export PATH=\"$P/bin:\$PATH\"; hydo module load (if doctor says so); then follow $P/README.md"
echo "(first step: cp -R \"$P/examples/redis\" ~/hydo-redis && cd ~/hydo-redis && hydo build hydo.yaml)"
