#!/usr/bin/env bash
# build-reproducers.sh -- the two pinned reproducer images (Milestone 145): the stack example
# (hydo.yaml in this folder) as agent images, once with the order service of M38's explorer image
# (`old`: the M37 order service, the one explore-disk.img ran, whose failed cache write fails an
# order that was already published) and once with M39's fix (`fix`, the one explore-fix-disk.img
# ran). Both order services are built from the repository's own history with the pinned Go, so
# their bytes are the recorded ones:
#   old  git commit d26ed1d, hvx/stack (main.go, go.sum; that commit tracked no go.mod, so
#        0112b48's go.mod, which pins the same versions as the go.sum, is added)   sha256 3ddaf8ba...
#   fix  git commit 0112b48, hvx/stack                                             sha256 1f18f855...
# built as build-stack.sh 6.1 built them (CGO_ENABLED=0, -trimpath, -buildvcs=false,
# -ldflags "-s -w -buildid=", GOFLAGS=-mod=readonly, GOTOOLCHAIN=local).
#
#   build-reproducers.sh <out dir> [old|fix ...]     (default: old fix)
#     -> <out dir>/explore-ob-stack-old-disk.img, <out dir>/explore-ob-stack-fix-disk.img
#   TWICE=1   build each image a second time and compare the bytes (the second copy is deleted)
#
# Prerequisites (an x86_64 Linux machine; Milestone 140's setup of the bare-metal test machine):
#   - HYDO_PREFIX: an installed Linux release (hydo/dist/linux; its guest/, tools/, libexec/ are
#     used, its bin/hydo is replaced by this tree's target/release/hydo, built with
#     `cargo build --release -p hydo-cli`); default ~/hydo-rf-work/prefix-rf
#   - HYDO_AGENT_BIN: the pinned hydo-agent (docs/BUILD-PINS.md, x86_64 398f297e...;
#     hydo/crates/hydo-agent/build.sh gives these bytes)
#   - the manifest's Nix store paths in /nix/store (MySQL 8.4.10 aw6ljbiw..., OpenJDK 17.0.7
#     vjcrra86..., Kafka 3.5.0 prk07apd..., docs/BUILD-PINS.md "Nix on the bare-metal test
#     machine"); iproute2-6.8.0 (dx4fsbxs...) and iptables-1.8.10 (xnrrq638...) must NOT be in
#     /nix/store (the release bundles them; the script refuses to build when they are)
#   - HYDO_GO: the pinned Go 1.26.5 (/nix/store/pwj3x44p...-go-1.26.5/bin/go) and network access
#     to the Go module proxy, or a module cache with the go.sum's modules (HYDO_GOMODCACHE)
#   - HYDO_REDIS_DIR: a folder with the static redis-server (e66c3fd0...) and redis-cli
#     (7ecd56f4...) of docs/BUILD-PINS.md (the manifest names ~/hydo/build/redis-*; the script
#     gives the build a private HOME whose hydo/build holds them)
#   - the order-service sources: this repository's git history (git archive), or
#     HYDO_STACK_SRC=<dir> holding d26ed1d.tar and 0112b48.tar made with
#     `git archive -o <dir>/<commit>.tar <commit> hvx/stack`
#   - sudo -n setpriv, unless the caller already is uid 1000 gid 100: the images are built as
#     uid 1000, gid 100 with HYDO_KEEP_OWNERS=1, as the build machine's hydo-build made them (the
#     owners of the staged /nix/store files are part of the image's bytes)
# Image work takes ~/.hydo-imgbuild.lock (one image build at a time on a shared machine).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
TOP="$(cd "$HERE/../../../.." && pwd)"
[ $# -ge 1 ] || { sed -n '2,40p' "$0"; exit 2; }
mkdir -p "$1"; OUT="$(cd "$1" && pwd)"; shift
[ $# = 0 ] && set -- old fix
PREFIX_SRC="${HYDO_PREFIX:-$HOME/hydo-rf-work/prefix-rf}"
AGENT="${HYDO_AGENT_BIN:?set HYDO_AGENT_BIN to the pinned hydo-agent}"
GO="${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}"
REDIS="${HYDO_REDIS_DIR:?set HYDO_REDIS_DIR to the folder with the pinned redis-server and redis-cli}"
HYDO="$TOP/target/release/hydo"
declare -A COMMIT=([old]=d26ed1d [fix]=0112b48)
declare -A BIN_SHA=([old]=3ddaf8baf54c16bdaddcecf76d21e840d58d8b625d3c400c0c31ffc3c89b5f65
                    [fix]=1f18f855b7678115f29563640e77eeb34f9da16fb49d4c9239a00ca97b5993be)
sha() { sha256sum < "$1" | cut -d' ' -f1; }
need() { [ "$(sha "$1")" = "$2" ] || { echo "error: $1 is not ${2:0:16}... (docs/BUILD-PINS.md)" >&2; exit 1; }; }

[ -x "$HYDO" ] || { echo "error: no $HYDO (cargo build --release -p hydo-cli)" >&2; exit 1; }
[ -d "$PREFIX_SRC/guest" ] || { echo "error: $PREFIX_SRC is not an installed Linux release (HYDO_PREFIX)" >&2; exit 1; }
"$GO" version | grep -q 'go1.26.5 ' || { echo "error: $GO is not go1.26.5 (HYDO_GO)" >&2; exit 1; }
need "$AGENT" 398f297e7425768a6175e58d27eca00d417fec25f0fb8b48842bcbbb53fdb8a6
need "$REDIS/redis-server" e66c3fd025dfd94b0994c3515e6f6f83ec78b36eeabfad360561a01a14bbcf79
need "$REDIS/redis-cli" 7ecd56f4b9a90c9c75e0bbab48537dc6698572d5a2faea1a2e7bfc67b0c593ea
if ls -d /nix/store/dx4fsbxs* /nix/store/xnrrq638* > /dev/null 2>&1; then
  echo "error: iproute2 dx4fsbxs... or iptables xnrrq638... is in /nix/store; the release's bundled closures must be used" >&2; exit 1
fi

W="$OUT/.work"; mkdir -p "$W"
# --- the order services -------------------------------------------------------------------------
for v in "$@"; do
  c=${COMMIT[$v]:?"unknown variant $v (old or fix)"}
  s="$W/src-$v"; rm -rf "$s"; mkdir -p "$s"
  if [ -n "${HYDO_STACK_SRC:-}" ]; then tar -xf "$HYDO_STACK_SRC/$c.tar" -C "$s"
  else git -C "$TOP" archive "$c" hvx/stack | tar -x -C "$s"; fi
  if [ ! -f "$s/hvx/stack/go.mod" ]; then   # d26ed1d tracked no go.mod (added in 45528d1)
    if [ -n "${HYDO_STACK_SRC:-}" ]; then tar -xOf "$HYDO_STACK_SRC/0112b48.tar" hvx/stack/go.mod > "$s/hvx/stack/go.mod"
    else git -C "$TOP" show 0112b48:hvx/stack/go.mod > "$s/hvx/stack/go.mod"; fi
  fi
  ( cd "$s/hvx/stack" && GOPATH="$W/gopath" GOMODCACHE="${HYDO_GOMODCACHE:-$W/gomod}" GOCACHE="$W/gocache" \
      GOFLAGS=-mod=readonly GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
      "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$W/$v-hydostack" . )
  need "$W/$v-hydostack" "${BIN_SHA[$v]}"
  echo "order service $v (git $c): sha256 $(sha "$W/$v-hydostack" | cut -c1-16)"
done

# --- the build prefix: this tree's hydo with the release's guest files and tools ------------------
P="$W/prefix"; rm -rf "$P"; mkdir -p "$P/bin" "$P/libexec"
cp "$HYDO" "$P/bin/hydo"
for d in guest tools module; do ln -s "$PREFIX_SRC/$d" "$P/$d"; done
ln -s "$TOP/hv" "$P/hv"; ln -s "$TOP/hydo" "$P/hydo"
for d in bin faketime hvx-run; do [ -e "$PREFIX_SRC/libexec/$d" ] && ln -s "$PREFIX_SRC/libexec/$d" "$P/libexec/$d"; done
H="$W/home"; mkdir -p "$H/hydo/build"
install -m 0755 "$REDIS/redis-server" "$REDIS/redis-cli" "$H/hydo/build/"
chmod -R a+rX "$W"

asuser() {   # run as uid 1000 gid 100 with a clean environment
  local e=(env -i HOME="$H" PATH="$HOME/.nix-profile/bin:/usr/bin:/bin" LANG=C LC_ALL=C HYDO_KEEP_OWNERS=1 HYDO_AGENT_BIN="$AGENT")
  if [ "$(id -u)" = 1000 ] && [ "$(id -g)" = 100 ]; then "${e[@]}" "$@"
  else sudo -n setpriv --reuid=1000 --regid=100 --clear-groups "${e[@]}" "$@"; fi
}
build() {   # <variant> <dir>
  local v=$1 d=$2 ex="$W/ex-$1"
  rm -rf "$ex"; mkdir -p "$ex"; cp "$HERE/hydo.yaml" "$ex/"; install -m 0755 "$W/$v-hydostack" "$ex/front-door"
  rm -rf "$d"; mkdir -p "$d"; chmod 777 "$d" "$ex"
  asuser /usr/bin/flock "$HOME/.hydo-imgbuild.lock" /usr/bin/nice -n 10 /usr/bin/env -C "$ex" \
    "$P/bin/hydo" build hydo.yaml --out "$d" > "$d.log" 2>&1 || { tail -20 "$d.log" >&2; return 1; }
  mv "$d/explore-ob-stack-disk.img" "$d/img"
  sudo -n rm -rf "$d"/*-root "$d"/*-ext4.img "$d"/*-data.img "$d"/*.closures 2>/dev/null || true
}
for v in "$@"; do
  build "$v" "$W/b1-$v"
  mv "$W/b1-$v/img" "$OUT/explore-ob-stack-$v-disk.img"
  echo "image $v: explore-ob-stack-$v-disk.img sha256 $(sha "$OUT/explore-ob-stack-$v-disk.img" | cut -c1-16)"
  if [ "${TWICE:-0}" = 1 ]; then
    build "$v" "$W/b2-$v"
    if [ "$(sha "$W/b2-$v/img")" = "$(sha "$OUT/explore-ob-stack-$v-disk.img")" ]; then echo "image $v: two builds identical"
    else echo "image $v: BUILDS DIFFER (second copy kept in $W/b2-$v/img)"; exit 1; fi
    rm -f "$W/b2-$v/img"
  fi
done
