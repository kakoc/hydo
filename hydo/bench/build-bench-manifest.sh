#!/usr/bin/env bash
# hydo/bench/build-bench-manifest.sh -- shell and agent images of the benchmark's defects from
# hydo/bench/manifest/hydo.yaml (Milestone 129, docs/C2-BENCH.md). Runs on the bare-metal test
# machine (no Nix needed): the stores, Kafka's libs and Redis come from a root tree dumped from the
# -pin-kr control image, each defect's order service from its benchmark image.
#
#   build-bench-manifest.sh <work dir> <release dir> <out name>=<benchmark image> ...
#
# <release dir> is a Linux release laid out as hydo/dist/linux builds it (bin/hydo, guest/,
# tools/, libexec/, hydo/onboard). For every pair, <work dir>/shell/<out name> and
# <work dir>/agent/<out name> are written: `hydo build` and `hydo build --control agent` of the
# same manifest with the same files, so the two kinds differ only in what --control agent changes.
# $HYDO_AGENT_BIN is the pinned hydo-agent. The out names are hydo-bench's image names (for example
# explore-bench-m59-d8-no-order-lock-pin-kr-disk.img), so `HYDO_BUILD=<work dir>/shell` and
# `HYDO_BUILD=<work dir>/agent` give hydo-bench the two sets.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
work=$1; rel=$2; shift 2
OFF=1048576
: "${HYDO_AGENT_BIN:?set HYDO_AGENT_BIN to the pinned hydo-agent}"
[ -d "$work/benchroot/nix/store" ] || { echo "no $work/benchroot (debugfs rdump of the -pin-kr control image's root)" >&2; exit 1; }
mkdir -p "$work/shell" "$work/agent" "$work/src"
rmtree() { [ -e "$1" ] || return 0; chmod -R u+w "$1" 2>/dev/null || true; rm -rf "$1"; }   # staged store files are read-only
# the image's /nix/store minus the pinned iproute2 closure (hydo-build stages that one itself, from
# the release's bundled store, read-only, so a second copy over it fails): hard links, built once
if [ ! -f "$work/benchstore.done" ]; then
  rm -rf "$work/benchstore"; mkdir -p "$work/benchstore"
  awk '$1 ~ /-iproute2-6.8.0$/ {sub("/nix/store/", "", $2); print $2}' "$rel/guest/store-closures.txt" | sort > "$work/benchstore.skip"
  for p in "$work/benchroot/nix/store"/*; do
    b=$(basename "$p"); grep -qxF "$b" "$work/benchstore.skip" && continue
    cp -al "$p" "$work/benchstore/$b"
  done
  echo "benchstore: $(ls "$work/benchstore" | wc -l) store paths ($(wc -l < "$work/benchstore.skip") left to hydo-build)"
  touch "$work/benchstore.done"
fi
for pair in "$@"; do
  out=${pair%%=*}; img=${pair#*=}
  d=$work/src/${out%-disk.img}
  rmtree "$d"; mkdir -p "$d"
  cp "$here/manifest/hydo.yaml" "$d/hydo.yaml"
  ln -s "$work/benchroot" "$d/root"
  ln -s "$work/benchstore" "$d/store"
  debugfs -R "dump /bin/hydostack $d/front-door" "$img?offset=$OFF" > /dev/null 2>&1
  [ -s "$d/front-door" ] || { echo "no /bin/hydostack in $img" >&2; exit 1; }
  echo "== $out: front door $(sha256sum < "$d/front-door" | cut -c1-16) from $(basename "$img")"
  if [ -n "${FROM:-}" ]; then
    # FROM=<out name already built>: the same manifest with another front door differs only in
    # /bin/hydostack, so copy that build and replace the one file (proven against a full build
    # with agent-patch.sh --check, docs/C2-BENCH.md)
    for kind in shell agent; do
      "$here/agent-patch.sh" --swap "$work/$kind/$FROM" /bin/hydostack "$d/front-door" "$work/$kind/$out" > /dev/null
      echo "   $kind $(sha256sum < "$work/$kind/$out" | cut -c1-16) (swapped into $FROM)"
    done
    continue
  fi
  for kind in shell agent; do
    rmtree "$d/b-$kind"
    flock "$HOME/.hydo-imgbuild.lock" nice "$rel/bin/hydo" build "$d/hydo.yaml" --out "$d/b-$kind" --control $kind > "$d/build-$kind.log" 2>&1 || {
      echo "build $kind failed:" >&2; tail -20 "$d/build-$kind.log" >&2; exit 1; }
    sfx=; [ $kind = agent ] && sfx=-agent
    mv "$d/b-$kind/explore-ob-bench$sfx-disk.img" "$work/$kind/$out"
    echo "   $kind $(sha256sum < "$work/$kind/$out" | cut -c1-16)"
    rmtree "$d/b-$kind"
  done
done
