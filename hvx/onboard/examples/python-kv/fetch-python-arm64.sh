#!/usr/bin/env bash
# Stage the pinned aarch64-linux CPython 3.11.9 (nixpkgs binary cache) under files/ for the Mac
# build (hydo build / hydo-build --arch arm64), as the Redis example stages Redis: the
# interpreter, its standard library without the parts a server never imports (test, idlelib,
# tkinter, turtledemo, ensurepip, lib2to3, pydoc_data, the config dir, the -O/-OO .pyc files),
# its extension modules, and exactly the shared libraries the loader opens for them
# (hvx-mac/services/elfdeps.py), at their /nix/store paths.
# Needs ssh to a Linux machine with nix that has (or can substitute) the store path:
#     nix build --max-jobs 0 --system aarch64-linux nixpkgs#python3   (prints the path below)
# Also copies the SDK (sdk/python/src/hydo_assert, without __pycache__) to files/opt/pykv/.
# Usage: fetch-python-arm64.sh   (checks the python files against files.sha256; FETCH_WRITE_SUMS=1 rewrites it)
#   [HYDO_BUILD_HOST=build-machine]
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../../../.." && pwd)
HOST=${HYDO_BUILD_HOST:?}
PY=/nix/store/ic0l2ykss28lr7r4by14inb3pqw87yza-python3-3.11.9     # aarch64-linux, nixpkgs#python3
HOSTPY=/nix/store/4rf5qybw37b4lh1g0xczlv14sqdbmnpm-python3-3.11.9/bin/python3   # x86_64, runs elfdeps

ssh "$HOST" "mkdir -p /tmp/hydo-pyfetch && cat > /tmp/hydo-pyfetch/elfdeps.py" < "$REPO/hvx-mac/services/elfdeps.py"
rm -rf "$HERE/files"; mkdir -p "$HERE/files"
ssh "$HOST" "set -e; P=$PY; L=\$P/lib/python3.11
  [ -e \$P/bin/python3.11 ] || { echo 'missing \$P: nix build --max-jobs 0 --system aarch64-linux nixpkgs#python3' >&2; exit 1; }
  cd /
  { find \${L#/} -mindepth 1 \\( -path '*/test' -o -path '*/idlelib' -o -path '*/tkinter' -o -path '*/turtledemo' \
        -o -path '*/ensurepip' -o -path '*/lib2to3' -o -path '*/pydoc_data' -o -path '*/config-3.11-*' \
        -o -path '*/site-packages' -o -name '_tkinter*' \\) -prune -o \\( -name '*.opt-1.pyc' -o -name '*.opt-2.pyc' \\) -prune -o -print
    echo \${P#/}/bin/python3.11
    $HOSTPY /tmp/hydo-pyfetch/elfdeps.py \$P/bin/python3.11 \$(ls \$L/lib-dynload/*.so | grep -v _tkinter) | sed 's#^/##'
  } | LC_ALL=C sort -u > /tmp/hydo-pyfetch/list
  tar cf - --no-recursion --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -T /tmp/hydo-pyfetch/list
  rm -rf /tmp/hydo-pyfetch" | (cd "$HERE/files" && tar xf -)
mkdir -p "$HERE/files/opt/pykv"
(cd "$REPO/sdk/python/src" && find hydo_assert -name '*.py' -o -name py.typed | LC_ALL=C sort | tar cf - -T -) | (cd "$HERE/files/opt/pykv" && tar xf -)
# store files are read-only; let a later fetch replace them
find "$HERE/files" -type d -exec chmod u+w {} +
cd "$HERE"
if [ "${FETCH_WRITE_SUMS:-}" ]; then
  find files/nix -type f | LC_ALL=C sort | xargs shasum -a 256 > files.sha256
else
  shasum -a 256 -c --quiet files.sha256
fi
echo "fetch-python-arm64: $(find "$HERE/files" -type f | wc -l | tr -d ' ') files, $(du -sh "$HERE/files" | cut -f1)"
