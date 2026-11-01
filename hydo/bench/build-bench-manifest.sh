#!/usr/bin/env bash
# hydo/bench/build-bench-manifest.sh -- the benchmark's images: an agent image of every order
# service from hydo/bench/manifest/hydo.yaml (Milestone 129; the benchmark's default images since
# Milestone 135, the only manifest kind since Milestone 136, README "Images"; until M136 it also
# built a shell image of each). Runs on the bare-metal test machine (no Nix needed): the
# stores, Kafka's libs and Redis come from a root tree dumped from the -pin-kr control image, each
# order service from its stack benchmark image.
#
#   build-bench-manifest.sh <work dir> <release dir> [<stem>=<stack image> ...]
#
# <release dir> is a Linux release laid out as hydo/dist/linux builds it (bin/hydo, guest/,
# tools/, libexec/, hydo/onboard). For every stem, $OUT (default <work dir>/build) gets
# explore-ob-bench-<stem>-agent-disk.img (`hydo build`): the name hydo-bench uses
# (`HYDO_BUILD=$OUT hydo-bench ...`; the "-agent" keeps the name apart from M135's shell images,
# explore-ob-bench-<stem>-disk.img, in older directories). Without stems, the default set is built from
# $STACK_BUILD (default ~/hydo/build): control, d8-d12 (the -pin-kr m59 images), nf (the d1-d7
# control, explore-nf), d1-d7, d0-old-front-door (explore-disk) and fix (explore-fix-disk); with
# BG=1 also bg-control and bg-d0..bg-d7 (explore-bench-bg-*).
# The stems in $FULL (default: the first stem) are built in full; every other stem is a copy of the
# first full build with /bin/hydostack replaced (agent-patch.sh --swap), because the manifest and
# every other file are the same (proven against full builds with agent-patch.sh --check,
# docs/C2-BENCH.md). ORDER_BIN=<dir> takes each order service from <dir>/<stem>-hydostack
# (build-order-services.sh: franz-go v1.22.2 since M135, the benchmark's images) instead of the
# stack image's /bin/hydostack (franz-go v1.17.1, M129's images). KEEP=1 keeps a stem whose image exists (to resume, or to add stems to a
# set). $HYDO_AGENT_BIN is the pinned hydo-agent. <work dir>/benchroot is the debugfs
# rdump of the -pin-kr control image's root; bzImage and initramfs-kafka.cpio.gz are linked into
# $OUT from $STACK_BUILD.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
work=$1; rel=$2; shift 2
OFF=1048576
OUT=${OUT:-$work/build}
SB=${STACK_BUILD:-$HOME/hydo/build}
: "${HYDO_AGENT_BIN:?set HYDO_AGENT_BIN to the pinned hydo-agent}"
[ -d "$work/benchroot/nix/store" ] || { echo "no $work/benchroot (debugfs rdump of the -pin-kr control image's root)" >&2; exit 1; }
mkdir -p "$OUT" "$work/src"
for f in bzImage initramfs-kafka.cpio.gz; do [ -e "$OUT/$f" ] || ln -s "$SB/$f" "$OUT/$f"; done
if [ $# = 0 ]; then
  set -- control="$SB/explore-bench-m59-control-pin-kr-disk.img"
  for d in d8-no-order-lock d9-redis-unlocked d10-outcome-in-memory d11-ack-unflushed-flag d12-shared-key-buffer; do
    set -- "$@" "$d=$SB/explore-bench-m59-$d-pin-kr-disk.img"
  done
  set -- "$@" nf="$SB/explore-nf-disk.img"
  for d in d1-kafka-first d2-batched-publish-flag d3-blind-retry d4-produce-timeout-definite d5-myisam d6-client-deadline-definite d7-skip-last-record; do
    set -- "$@" "$d=$SB/explore-bench-$d-disk.img"
  done
  set -- "$@" d0-old-front-door="$SB/explore-disk.img" fix="$SB/explore-fix-disk.img"
  if [ "${BG:-0}" = 1 ]; then
    for d in control d0-old-front-door d1-kafka-first d2-batched-publish-flag d3-blind-retry d4-produce-timeout-definite d5-myisam d6-client-deadline-definite d7-skip-last-record; do
      set -- "$@" "bg-$d=$SB/explore-bench-bg-$d-disk.img"
    done
  fi
fi
FULL=${FULL:-${1%%=*}}
first=${FULL%% *}
rmtree() { [ -e "$1" ] || return 0; chmod -R u+w "$1" 2>/dev/null || true; rm -rf "$1"; }   # staged store files are read-only
name() { echo "explore-ob-bench-$1-agent-disk.img"; }
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
# the full builds first, so that every swap has its source
order=$(for pair in "$@"; do s=${pair%%=*}; case " $FULL " in *" $s "*) echo "0 $pair";; *) echo "1 $pair";; esac; done | sort -s -k1,1 | cut -d' ' -f2-)
for pair in $order; do
  stem=${pair%%=*}; img=${pair#*=}
  if [ "${KEEP:-0}" = 1 ] && [ -s "$OUT/$(name "$stem" agent)" ]; then
    echo "== $stem: kept (KEEP=1)"; continue
  fi
  d=$work/src/$stem
  rmtree "$d"; mkdir -p "$d"
  cp "$here/manifest/hydo.yaml" "$d/hydo.yaml"
  ln -s "$work/benchroot" "$d/root"
  ln -s "$work/benchstore" "$d/store"
  if [ -n "${ORDER_BIN:-}" ]; then
    img=$ORDER_BIN/$stem-hydostack
    cp "$img" "$d/front-door" || { echo "no $img (build-order-services.sh)" >&2; exit 1; }
  else
    debugfs -R "dump /bin/hydostack $d/front-door" "$img?offset=$OFF" > /dev/null 2>&1
    [ -s "$d/front-door" ] || { echo "no /bin/hydostack in $img" >&2; exit 1; }
  fi
  echo "== $stem: order service $(sha256sum < "$d/front-door" | cut -c1-16) from $(basename "$img")"
  case " $FULL " in
    *" $stem "*) ;;
    *)
      for kind in agent; do
        "$here/agent-patch.sh" --swap "$OUT/$(name "$first" $kind)" /bin/hydostack "$d/front-door" "$OUT/$(name "$stem" $kind)" > /dev/null
        echo "   $kind $(sha256sum < "$OUT/$(name "$stem" $kind)" | cut -c1-16) (swapped into $first)"
      done
      continue;;
  esac
  for kind in agent; do
    rmtree "$d/b-$kind"
    flock "$HOME/.hydo-imgbuild.lock" nice "$rel/bin/hydo" build "$d/hydo.yaml" --out "$d/b-$kind" > "$d/build-$kind.log" 2>&1 || {
      echo "build $kind failed:" >&2; tail -20 "$d/build-$kind.log" >&2; exit 1; }
    mv "$d/b-$kind/explore-ob-bench-disk.img" "$OUT/$(name "$stem" $kind)"
    echo "   $kind $(sha256sum < "$OUT/$(name "$stem" $kind)" | cut -c1-16)"
    rmtree "$d/b-$kind"
  done
done
