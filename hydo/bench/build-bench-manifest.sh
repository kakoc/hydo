#!/usr/bin/env bash
# hydo/bench/build-bench-manifest.sh -- the benchmark's images: an agent image of every order
# service from hydo/bench/manifest/hydo.yaml (Milestone 129; the benchmark's default images since
# Milestone 135, the only kind since Milestone 136, README "Images"). Since Milestone 146 they are
# built from sources and pinned Nix store paths only, the way hydo/onboard/examples/stack/
# build-reproducers.sh builds the pinned reproducers: no file comes from a hand-made stack image.
#
#   build-bench-manifest.sh <work dir> <release dir> [<stem> ...]
#
# <release dir> is a Linux release laid out as hydo/dist/linux builds it (bin/hydo, guest/,
# tools/, libexec/, hydo/onboard). For every stem, $OUT (default <work dir>/build) gets
# explore-ob-bench-<stem>-agent-disk.img (`hydo build`), the name hydo-bench uses
# (`HYDO_BUILD=$OUT hydo-bench ...`). Stems (build-order-services.sh's): control, d8-no-order-lock
# .. d12-shared-key-buffer, nf (the d1-d7 control), d1-kafka-first .. d7-skip-last-record,
# d0-old-front-door and fix (d0's control); BG=1 adds bg-control and bg-d0..bg-d7. Default: all
# of them.
# The stems in $FULL (default: the first stem) are built in full; every other stem is a copy of the
# first full build with /bin/hydostack replaced (agent-patch.sh --swap), because the manifest and
# every other file are the same (proven against full builds with agent-patch.sh --check,
# docs/C2-BENCH.md). TWICE=1 builds every image a second time (the full builds and the swaps) and
# stops when the two differ. KEEP=1 keeps a stem whose image exists (to resume, or to add stems).
#
# Inputs (an x86_64 Linux machine with Nix; docs/BUILD-PINS.md "Nix on the bare-metal test
# machine"):
#   - the order services: ORDER_BIN=<dir> holding <stem>-hydostack, else they are built into
#     <work dir>/order by build-order-services.sh (pinned Go 1.26.5, HYDO_GO; franz-go v1.22.2)
#   - nix-store on PATH and the manifest's store paths in /nix/store: MySQL 8.4.10
#     aw6ljbiw..., OpenJDK 17.0.7 vjcrra86... (each with its closure) and Kafka 3.5.0 prk07apd...
#     (alone); iproute2-6.8.0 (dx4fsbxs...) and iptables-1.8.10 (xnrrq638...) must NOT be in
#     /nix/store (the release bundles them; the script refuses to build when they are)
#   - HYDO_REDIS_DIR: a folder with the pinned static redis-server (e66c3fd0...) and redis-cli
#     (7ecd56f4...)
#   - HYDO_AGENT_BIN: the pinned hydo-agent (x86_64 398f297e...)
#   - KERNEL_DIR (default ~/hydo/build): bzImage and initramfs-kafka.cpio.gz, linked into $OUT
# The images are built as the calling user with the root's owners normalised to 0:0 (no
# HYDO_KEEP_OWNERS), as every benchmark image since M129 was. Image work takes
# ~/.hydo-imgbuild.lock (one image build at a time on a shared machine).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
[ $# -ge 2 ] || { sed -n '2,40p' "$0"; exit 2; }
mkdir -p "$1"; work=$(cd "$1" && pwd); rel=$2; shift 2
OUT=${OUT:-$work/build}
KD=${KERNEL_DIR:-$HOME/hydo/build}
AGENT=${HYDO_AGENT_BIN:?set HYDO_AGENT_BIN to the pinned hydo-agent}
REDIS=${HYDO_REDIS_DIR:?set HYDO_REDIS_DIR to the folder with the pinned redis-server and redis-cli}
sha() { sha256sum < "$1" | cut -d' ' -f1; }
need() { [ "$(sha "$1")" = "$2" ] || { echo "error: $1 is not ${2:0:16}... (docs/BUILD-PINS.md)" >&2; exit 1; }; }
need "$AGENT" 398f297e7425768a6175e58d27eca00d417fec25f0fb8b48842bcbbb53fdb8a6
need "$REDIS/redis-server" e66c3fd025dfd94b0994c3515e6f6f83ec78b36eeabfad360561a01a14bbcf79
need "$REDIS/redis-cli" 7ecd56f4b9a90c9c75e0bbab48537dc6698572d5a2faea1a2e7bfc67b0c593ea
command -v nix-store > /dev/null || { echo "error: no nix-store on PATH (the manifest's root.nix: needs it)" >&2; exit 1; }
for p in aw6ljbiwgds7yw8x2jc3340dm46s7i2k-mysql-8.4.10 vjcrra860yal7fzk1m81ddirrafqrccz-openjdk-headless-17.0.7+7 \
         prk07apdgqqjcqyxr5fqww1y8bxpw7z9-apache-kafka-2.13-3.5.0; do
  [ -d "/nix/store/$p" ] || { echo "error: /nix/store/$p is missing (docs/BUILD-PINS.md)" >&2; exit 1; }
done
if ls -d /nix/store/dx4fsbxs* /nix/store/xnrrq638* > /dev/null 2>&1; then
  echo "error: iproute2 dx4fsbxs... or iptables xnrrq638... is in /nix/store; the release's bundled closures must be used" >&2; exit 1
fi
M59="d8-no-order-lock d9-redis-unlocked d10-outcome-in-memory d11-ack-unflushed-flag d12-shared-key-buffer"
OLD="d1-kafka-first d2-batched-publish-flag d3-blind-retry d4-produce-timeout-definite d5-myisam d6-client-deadline-definite d7-skip-last-record"
if [ $# = 0 ]; then
  set -- control $M59 nf $OLD d0-old-front-door fix
  [ "${BG:-0}" = 1 ] && for d in control d0-old-front-door $OLD; do set -- "$@" "bg-$d"; done
fi
mkdir -p "$OUT" "$work/src"
for f in bzImage initramfs-kafka.cpio.gz; do [ -e "$OUT/$f" ] || ln -s "$KD/$f" "$OUT/$f"; done
# the order services
if [ -n "${ORDER_BIN:-}" ]; then ob=$ORDER_BIN
else
  ob=$work/order; missing=()
  for s in "$@"; do [ -s "$ob/$s-hydostack" ] || missing+=("$s"); done
  [ ${#missing[@]} = 0 ] || "$here/build-order-services.sh" "$ob" "${missing[@]}"
fi
FULL=${FULL:-$1}
first=${FULL%% *}
rmtree() { [ -e "$1" ] || return 0; chmod -R u+w "$1" 2>/dev/null || true; rm -rf "$1"; }   # staged store files are read-only
name() { echo "explore-ob-bench-$1-agent-disk.img"; }
full() {   # <stem dir> <out image>: hydo build of the manifest in <stem dir>
  local d=$1 b=$1/b
  rmtree "$b"
  flock "$HOME/.hydo-imgbuild.lock" nice env HYDO_AGENT_BIN="$AGENT" "$rel/bin/hydo" build "$d/hydo.yaml" --out "$b" > "$d/build.log" 2>&1 || {
    echo "build failed:" >&2; tail -20 "$d/build.log" >&2; exit 1; }
  mv "$b/explore-ob-bench-disk.img" "$2"
  rmtree "$b"
}
same() {   # <image> <second build>: compare, delete the second copy
  if [ "$(sha "$1")" = "$(sha "$2")" ]; then echo "   twice: identical"; rm -f "$2"
  else echo "error: two builds of $(basename "$1") differ (second copy kept: $2)" >&2; exit 1; fi
}
# the full builds first, so that every swap has its source
order=$(for s in "$@"; do case " $FULL " in *" $s "*) echo "0 $s";; *) echo "1 $s";; esac; done | sort -s -k1,1 | cut -d' ' -f2-)
for stem in $order; do
  img=$OUT/$(name "$stem")
  if [ "${KEEP:-0}" = 1 ] && [ -s "$img" ]; then echo "== $stem: kept (KEEP=1)"; continue; fi
  d=$work/src/$stem
  rmtree "$d"; mkdir -p "$d"
  cp "$here/manifest/hydo.yaml" "$d/hydo.yaml"
  install -m 0755 "$REDIS/redis-server" "$REDIS/redis-cli" "$d/"
  install -m 0755 "$ob/$stem-hydostack" "$d/front-door" || { echo "no $ob/$stem-hydostack (build-order-services.sh)" >&2; exit 1; }
  echo "== $stem: order service $(sha "$d/front-door" | cut -c1-16)"
  case " $FULL " in
    *" $stem "*)
      full "$d" "$img"
      echo "   $(sha "$img" | cut -c1-16) (full build)"
      if [ "${TWICE:-0}" = 1 ]; then full "$d" "$img.2"; same "$img" "$img.2"; fi;;
    *)
      "$here/agent-patch.sh" --swap "$OUT/$(name "$first")" /bin/hydostack "$d/front-door" "$img" > /dev/null
      echo "   $(sha "$img" | cut -c1-16) (swapped into $first)"
      if [ "${TWICE:-0}" = 1 ]; then
        "$here/agent-patch.sh" --swap "$OUT/$(name "$first")" /bin/hydostack "$d/front-door" "$img.2" > /dev/null
        same "$img" "$img.2"
      fi;;
  esac
done
