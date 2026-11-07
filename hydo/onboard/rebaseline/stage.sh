#!/usr/bin/env bash
# stage.sh -- the staging and build steps of `hydo-bench rebaseline` (Milestone 164; docs/REBASELINE.md
# "How to re-baseline"). hydo-bench calls it; a person can too:
#
#   stage.sh prefix                     the build and run prefix $W/prefix: this tree's programs
#                                       (target/release) over a release's guest files and tools
#   stage.sh prepare <stage>            copy the examples of <stage> from the tree into $W/tree (at
#                                       their paths in the repository) and stage their inputs
#   stage.sh build normal|keep <dir> <manifest> <out> [hydo build args...]
#                                       `hydo build` of <dir>/<manifest> into <out> (keep: as uid 1000,
#                                       gid 100 with HYDO_KEEP_OWNERS=1, as images with Nix store
#                                       paths are built on the bare-metal machine, M140)
#
# Environment (hydo-bench rebaseline sets the first four):
#   HYDO_ROOT              the tree (the folder with hydo/ and hv/)
#   HYDO_REBASE_WORK (W)   the work folder: prefix/, tree/, home/, img/, runs/, cache/, logs/
#   HYDO_REBASE_INPUTS     inputs made or fetched once and checked by sha256 (default $W/in)
#   HYDO_REBASE_PLATFORM   hvx | kvm (x86_64 Linux) or hvm (the Mac)
#   HYDO_REBASE_RELEASE    an unpacked hydo release of this platform (its guest/, tools/, libexec/
#                          and examples/ are used; on Linux `hydo setup` must have run in it)
#   HYDO_GO                Go 1.26.5 (Linux: the pinned /nix/store/pwj3x44p...-go-1.26.5; the Mac:
#                          go.dev's darwin-arm64 1.26.5)
#   HYDO_HVX_RUN           Linux: the shared module lock wrapper (default ~/bin/hvx-run)
#   HYDO_NIX_MACHINE       the Mac: where node-kv's and python-kv's aarch64 store paths are fetched
#                          from (default bare-metal; machines.sh)
# Inputs that cannot be made from the tree alone (upstream programs and images) are fetched by
# digest or sha256 and checked; each stage says which below. Every image build takes
# ~/.hydo-imgbuild.lock (one image build at a time on a shared machine).
set -euo pipefail
T=${HYDO_ROOT:?HYDO_ROOT: the tree}
W=${HYDO_REBASE_WORK:?HYDO_REBASE_WORK: the work folder}
IN=${HYDO_REBASE_INPUTS:-$W/in}
PLAT=${HYDO_REBASE_PLATFORM:?HYDO_REBASE_PLATFORM: hvx, hvm or kvm}
REL=${HYDO_REBASE_RELEASE:-}
P=$W/prefix
ST=$W/tree
FH=$W/home                 # the builds' HOME on Linux: the manifests name ~/hydo/build/<file>
H=$FH/hydo/build
X=hydo/onboard/examples
XM=hydo/onboard/examples-mac
# rust-kv's program is built at this one path on every machine (its bytes depend on the path)
RKV_HOME=${HYDO_RKV_HOME:-/tmp/hydo-rebaseline-rkv}
mkdir -p "$W" "$IN" "$ST"

die() { echo "stage.sh: $*" >&2; exit 1; }
say() { echo "stage.sh: $*"; }
sha() { if command -v sha256sum > /dev/null; then sha256sum < "$1" | cut -d' ' -f1; else shasum -a 256 < "$1" | cut -d' ' -f1; fi; }
need() {   # <file> <sha256>
  [ -f "$1" ] || die "missing $1"
  [ "$(sha "$1")" = "$2" ] || die "$1 is $(sha "$1" | cut -c1-16)..., not the pinned ${2:0:16}..."
}
have() { [ -f "$1" ] && [ "$(sha "$1")" = "$2" ]; }
need_release() { [ -n "$REL" ] && [ -d "$REL/guest" ] || die "HYDO_REBASE_RELEASE must name an unpacked hydo release of this platform (hydo-bench rebaseline --release DIR)"; }
# copy <path in the repository> into the staged tree, replacing what was there
copy() {
  for p in "$@"; do
    [ -e "$T/$p" ] || die "no $T/$p"
    rm -rf "${ST:?}/$p"; mkdir -p "$(dirname "$ST/$p")"
    cp -R "$T/$p" "$ST/$p"
  done
}
GO=${HYDO_GO:-/nix/store/pwj3x44pp735yb038fnmqckyyra9k29x-go-1.26.5/bin/go}
need_go() { [ -x "$GO" ] && "$GO" version | grep -q 'go1.26.5 ' || die "HYDO_GO must be Go 1.26.5 (it is ${GO})"; }
gobuild() {   # <src dir> <out> [GOARCH] [go build args]: build-stack.sh 6.1's byte-reproducible recipe
  local src=$1 out=$2 arch=${3:-amd64}; shift 3 || shift $#
  need_go
  ( cd "$src" && GOPATH=$W/go GOMODCACHE=$IN/gomod GOCACHE=$W/go-cache GOFLAGS=-mod=readonly \
      GOTOOLCHAIN=local CGO_ENABLED=0 GOOS=linux GOARCH=$arch \
      "$GO" build "$@" -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$out" . )
  say "built $(basename "$out") sha256 $(sha "$out" | cut -c1-16)"
}

# ------------------------------------------------------------------------------------- prefix
prefix() {
  need_release
  rm -rf "$P"; mkdir -p "$P/bin" "$P/libexec"
  local progs
  case $PLAT in
    hvm) progs="hydo hydo-hvm hydo-build hydo-fetch-image";;
    *) progs="hydo hydo-hvx hydo-build hydo-fetch-image";;
  esac
  for b in $progs; do
    [ -x "$T/target/release/$b" ] || die "no $T/target/release/$b: cargo build --release -p hydo -p hydo-bench $( [ $PLAT = hvm ] && echo -p hydo-hvm || echo -p hydo-hvx) -p hydo-build -p hydo-compose"
    cp "$T/target/release/$b" "$P/bin/$b"
  done
  for d in guest tools module examples skill; do [ -e "$REL/$d" ] && ln -s "$REL/$d" "$P/$d"; done
  # the tree's hydo/ (the guest-side files of hydo/onboard go into images); on the Mac the
  # release's hv/ (its guest kernels are in hv/hvm/explore/out, git-ignored in a tree)
  ln -s "$T/hydo" "$P/hydo"
  if [ "$PLAT" = hvm ]; then ln -s "$REL/hv" "$P/hv"; else ln -s "$T/hv" "$P/hv"; fi
  if [ "$PLAT" = hvm ]; then
    codesign -s - --entitlements "$T/hv/hvm/hv.entitlements" --force "$P/bin/hydo-hvm" > /dev/null 2>&1 \
      || die "codesign of $P/bin/hydo-hvm failed"
  else
    for d in bin faketime; do [ -e "$REL/libexec/$d" ] && ln -s "$REL/libexec/$d" "$P/libexec/$d"; done
    local run=${HYDO_HVX_RUN:-$HOME/bin/hvx-run}
    if [ -x "$run" ]; then ln -s "$run" "$P/libexec/hvx-run"; else ln -s "$REL/libexec/hvx-run" "$P/libexec/hvx-run"; fi
    [ -d "$REL/guest/store" ] || die "$REL: run its bin/hydo setup first (guest/store is not unpacked)"
  fi
  say "prefix $P: $(cd "$P/bin" && for b in *; do printf '%s %s  ' "$b" "$(sha "$b" | cut -c1-12)"; done)"
}

# ------------------------------------------------------------------------------- Linux stages
# the fake home's build folder: the Redis binaries the manifests name as ~/hydo/build/redis-*
linux_home() {
  need_release
  mkdir -p "$H"
  for f in redis-server:e66c3fd025dfd94b0994c3515e6f6f83ec78b36eeabfad360561a01a14bbcf79 \
           redis-cli:7ecd56f4b9a90c9c75e0bbab48537dc6698572d5a2faea1a2e7bfc67b0c593ea; do
    n=${f%%:*}; s=${f#*:}
    have "$H/$n" "$s" || { install -m 0755 "$REL/examples/redis/files/$n" "$H/$n"; need "$H/$n" "$s"; }
  done
}
# the stack's two order services (M145's pinned reproducers; hydo/onboard/examples/stack/
# build-reproducers.sh): old = git d26ed1d hvx/stack (with 0112b48's go.mod), fix = 0112b48;
# from the repository's history, or from HYDO_STACK_SRC/<commit>.tar
order_service() {   # old|fix -> $IN/stack/<v>-hydostack
  local v=$1 c s out=$IN/stack/$1-hydostack
  case $v in old) c=d26ed1d; s=3ddaf8baf54c16bdaddcecf76d21e840d58d8b625d3c400c0c31ffc3c89b5f65;;
             fix) c=0112b48; s=1f18f855b7678115f29563640e77eeb34f9da16fb49d4c9239a00ca97b5993be;; esac
  have "$out" "$s" && return 0
  mkdir -p "$IN/stack"; local d=$IN/stack/src-$v; rm -rf "$d"; mkdir -p "$d"
  if [ -n "${HYDO_STACK_SRC:-}" ]; then tar -xf "$HYDO_STACK_SRC/$c.tar" -C "$d"
  else git -C "$T" archive "$c" hvx/stack | tar -x -C "$d"; fi
  if [ ! -f "$d/hvx/stack/go.mod" ]; then
    if [ -n "${HYDO_STACK_SRC:-}" ]; then tar -xOf "$HYDO_STACK_SRC/0112b48.tar" hvx/stack/go.mod > "$d/hvx/stack/go.mod"
    else git -C "$T" show 0112b48:hvx/stack/go.mod > "$d/hvx/stack/go.mod"; fi
  fi
  gobuild "$d/hvx/stack" "$out" amd64
  need "$out" "$s"
}
linux_prepare() {
  case $1 in
    redis)
      linux_home
      copy $X/redis $X/redis-ram $X/redis-composed $X/redis-composed-hist $X/redis-composed-read $X/redis-errors $X/redis-bridge
      ln -sfn "$H" "$ST/$X/redis-bridge/files";;
    voting)   # the voting app's images by digest (fetch-images.sh, network once)
      copy $X/compose-voting
      if [ ! -e "$IN/voting/.done" ]; then
        rm -rf "$IN/voting"; mkdir -p "$IN/voting"; cp "$T/$X/compose-voting/fetch-images.sh" "$IN/voting/"
        ( cd "$IN/voting" && HYDO_FETCH_IMAGE=$P/bin/hydo-fetch-image sh fetch-images.sh ) && touch "$IN/voting/.done"
      fi
      rm -rf "$ST/$X/compose-voting/images"; ln -s "$IN/voting/images" "$ST/$X/compose-voting/images";;
    stack-old|stack-fix)
      linux_home; order_service "${1#stack-}"
      copy $X/stack; install -m 0755 "$IN/stack/${1#stack-}-hydostack" "$ST/$X/stack/front-door";;
    miniflux)   # miniflux v2.3.3 (dd8e2456...), hydo/onboard/examples/miniflux/build.sh step 1
      local m=$IN/miniflux/miniflux s=dd8e24562eaf9e1dc43e1718a61257482aa735c4e6dcd414452c28d152fc74e4
      if ! have "$m" "$s"; then
        mkdir -p "$IN/miniflux"; rm -rf "$IN/miniflux/v2"
        git clone -q --depth 1 --branch v2.3.3 https://github.com/miniflux/v2.git "$IN/miniflux/v2"
        need_go
        ( cd "$IN/miniflux/v2" && GOPATH=$W/go GOMODCACHE=$IN/gomod GOCACHE=$W/go-cache CGO_ENABLED=0 \
            GOTOOLCHAIN=local GOFLAGS=-mod=readonly "$GO" build -trimpath -buildvcs=false \
            -ldflags "-s -w -buildid= -X miniflux.app/v2/internal/version.Version=2.3.3" -o "$m" . )
        need "$m" "$s"
      fi
      copy $X/miniflux; install -m 0755 "$m" "$ST/$X/miniflux/miniflux";;
    etcd)   # the release archives of etcd-io/etcd, checked against etcd/build.sh's sha256 table
      copy $X/etcd; mkdir -p "$IN/etcd"
      for V in 3.5.5 3.5.6; do
        local t=etcd-v$V-linux-amd64.tar.gz s
        s=$(grep -o "\[$V\]=[0-9a-f]*" "$T/$X/etcd/build.sh" | cut -d= -f2)
        have "$IN/etcd/$t" "$s" || curl -sSfL -o "$IN/etcd/$t" "https://github.com/etcd-io/etcd/releases/download/v$V/$t"
        need "$IN/etcd/$t" "$s"
        local x; x=$(mktemp -d); tar -xzf "$IN/etcd/$t" -C "$x"; mkdir -p "$ST/$X/etcd/etcd-v$V"
        install -m 0755 "$x/etcd-v$V-linux-amd64/etcd" "$x/etcd-v$V-linux-amd64/etcdctl" "$ST/$X/etcd/etcd-v$V/"; rm -rf "$x"
      done;;
    shop)   # the two shop programs from this tree's sources; the image names their folder
      # ({inputs}/shop, in /etc/hydo/params), so a program is written there only when it differs
      copy $X/shop; mkdir -p "$IN/shop" "$W/shop-new"
      gobuild "$ST/$X/shop/src" "$W/shop-new/shop-buggy" amd64
      gobuild "$ST/$X/shop/src" "$W/shop-new/shop-fixed" amd64 -tags=fixed
      for v in buggy fixed; do
        cmp -s "$W/shop-new/shop-$v" "$IN/shop/shop-$v" || install -m 0755 "$W/shop-new/shop-$v" "$IN/shop/shop-$v"
      done;;
    stack-grpc-cur|stack-grpc-old|stack-grpc-planted)   # stack-grpc/build.sh's sources, without its image step
      copy $X/stack-grpc hydo/stack hydo/bench/base
      local v=${1#stack-grpc-}
      ( cd "$ST" && eval "$(sed -n '/^src() {/,/^src cur;/p' "$T/$X/stack-grpc/build.sh" | sed '$d')"
        for k in cur "$v"; do src "$k"; done )
      mkdir -p "$IN/stack-grpc"
      for k in cur "$v"; do gobuild "$ST/build/stack-grpc-src/$k" "$IN/stack-grpc/$k-hydostack" amd64; done
      install -m 0755 "$IN/stack-grpc/$v-hydostack" "$ST/$X/stack-grpc/front-door"
      install -m 0755 "$IN/stack-grpc/cur-hydostack" "$ST/$X/stack-grpc/client";;
    stackgen)   # the stack with generated gRPC drivers (stack/hydo-grpcgen.yaml): the fix order
      # service with go.mod.baseline (franz-go v1.17.1), the pinned client, hydo-grpc-gen's output
      linux_home; copy $X/stack
      rm -rf "$ST/$X/stackgen"; cp -R "$ST/$X/stack" "$ST/$X/stackgen"
      mkdir -p "$IN/orders"
      HYDO_GO=$GO HYDO_GOMODCACHE=$IN/gomod-baseline STACK_GOMOD=baseline HYDO_BENCH_BIN=$T/target/release/hydo-bench \
        bash "$T/hydo/bench/build-order-services.sh" "$IN/orders" fix
      install -m 0755 "$IN/orders/fix-hydostack" "$ST/$X/stackgen/front-door"
      mkdir -p "$IN/grpc-client"
      have "$IN/grpc-client/hydo-grpc" 3926d13712772cceb3d93c5be9ba5884e6bee4e49f29fca04b8d065c47a4228d \
        || "$T/hydo/grpcgen/build-client.sh" --out "$IN/grpc-client" amd64
      ( cd "$T" && HYDO_GRPC_CLIENT=$IN/grpc-client "$T/target/release/hydo-grpc-gen" hydo/stack/proto \
          --hints hydo/stack/grpc-hints.yaml --out "$ST/$X/stackgen/grpcgen" );;
    rust-kv)   # rkv with build-rkv.sh's flags from a checkout at <home>/hydo, <home> = $RKV_HOME
      # (rkv's bytes depend on where it is built, even with the remaps: Cargo hashes a path
      # dependency's absolute path into the symbols; AGENTS.md open item 24), so always one path
      local fh=$RKV_HOME
      rm -rf "$fh/hydo"; mkdir -p "$fh/hydo/$X"
      cp -R "$T/$X/rust-kv" "$fh/hydo/$X/"; cp -R "$T/sdk" "$fh/hydo/sdk"; rm -rf "$fh"/hydo/sdk/*/target
      rm -rf "$fh/hydo/$X/rust-kv/target" "$fh/hydo/$X/rust-kv/bin"
      ( export RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}; export CARGO_HOME=$fh/.cargo HOME=$fh; mkdir -p "$CARGO_HOME"
        cd "$fh/hydo/$X/rust-kv" &&
        RUSTFLAGS="-C linker=rust-lld -C link-self-contained=yes -C target-feature=+crt-static --remap-path-prefix=$fh/hydo=/hydo --remap-path-prefix=$fh=/home" \
          cargo build --release --locked --target x86_64-unknown-linux-musl ) > "$IN/rkv-build.log" 2>&1 \
        || { tail "$IN/rkv-build.log" >&2; die "rkv build failed"; }
      copy $X/rust-kv; mkdir -p "$ST/$X/rust-kv/bin"
      cp "$fh/hydo/$X/rust-kv/target/x86_64-unknown-linux-musl/release/rkv" "$ST/$X/rust-kv/bin/rkv"
      say "rkv sha256 $(sha "$ST/$X/rust-kv/bin/rkv" | cut -c1-16)";;
    avx2)   # the pinned static musl gcc with nixpkgs 24.05's binutils 2.41 first on PATH
      copy $X/avx2
      PATH=/nix/store/7v7g86ml0ri171gfcrs1d442px5bi1p3-binutils-2.41/bin:$PATH \
        /nix/store/aa1jsl19ywwfpqp0438vqbncc7sndnyw-gcc-static-x86_64-unknown-linux-musl-13.2.0/bin/gcc \
        -static -O2 -Wall -pthread -B /nix/store/0nlqfwsn4qpx1453gb4zvx6dvn3121l4-musl-static-x86_64-unknown-linux-musl-1.2.6/lib \
        "$ST/$X/avx2/hydoavx.c" -o "$ST/$X/avx2/hydoavx"
      say "hydoavx sha256 $(sha "$ST/$X/avx2/hydoavx" | cut -c1-16)";;
    redis-oci)
      copy $X/redis-oci
      [ -f "$IN/oci/redis-7.2.4-amd64.docker.tar" ] || { mkdir -p "$IN/oci"; "$P/bin/hydo-fetch-image" docker.io/library/redis \
        sha256:9341b6548cc35b64a6de0085555264336e2f570e17ecff20190bf62222f2bd64 redis:7.2.4 "$IN/oci/redis-7.2.4-amd64.docker.tar"; }
      ln -sfn "$IN/oci/redis-7.2.4-amd64.docker.tar" "$ST/$X/redis-oci/redis-7.2.4-amd64.docker.tar";;
    restate-oci)
      copy $X/restate-oci
      [ -f "$IN/oci/restate-1.7.13-amd64.docker.tar" ] || { mkdir -p "$IN/oci"; "$P/bin/hydo-fetch-image" docker.io/restatedev/restate \
        sha256:d25e52b505f1b21c382282c9aa3339a79ff3eaab0c9b01c9516a1f349a895d07 restatedev/restate:1.7.13 "$IN/oci/restate-1.7.13-amd64.docker.tar"; }
      ln -sfn "$IN/oci/restate-1.7.13-amd64.docker.tar" "$ST/$X/restate-oci/restate-1.7.13-amd64.docker.tar";;
    release) release_examples;;
    *) die "unknown stage $1 for $PLAT";;
  esac
}

# --------------------------------------------------------------------------------- Mac stages
# the release's examples/redis/files (the aarch64 Redis 7.2.4 store paths, absolute symlinks made
# relative) beside a manifest whose /nix/store sources are made relative to it, as the Mac
# release's own Redis example is; a composed plan's files are beside its hydo/ folder (M155's
# staging: `{src: ../files/nix/store/...}` in hydo/hydo.yaml)
mac_redis_files() {   # <manifest dir in the staged tree> [<files dir relative to it>], every *.yaml in it
  need_release
  local up=${2:-.} pre=
  [ "$up" = . ] || pre=$up/
  rm -rf "${ST:?}/$1/$up/files"; cp -R "$REL/examples/redis/files" "$ST/$1/$up/files"
  for y in "$ST/$1"/*.yaml; do
    sed "s#{src: /nix/store/#{src: ${pre}files/nix/store/#" "$y" > "$y.new" && mv "$y.new" "$y"
  done
}
# an aarch64 store path's closure from the public Nix cache (hydo-nix-fetch, no Nix needed) under
# $IN/nix-arm64, its absolute /nix/store symlinks made relative (they resolve the same in the guest,
# and hydo-elfdeps --root follows only relative ones, open item 42 d); prints the root
relpath() {   # <target> <from dir>: both absolute and normalised (hydo/dist/mac/build-release.sh's)
  local t=$1 c=$2 up=
  while [ "$c" != / ] && [ "${t#"$c"/}" = "$t" ]; do c=$(dirname "$c"); up="../$up"; done
  echo "$up${t#"$c"/}"
}
nix_arm64() {   # <store path>
  local R=$IN/nix-arm64 m; m=$IN/nix-arm64.$(basename "$1").done
  if [ ! -e "$m" ]; then
    mkdir -p "$R"
    "$T/target/release/hydo-nix-fetch" --closure "$R" "$1" >&2 || die "hydo-nix-fetch $1 (cargo build --release -p hydo-tools)"
    find "$R/nix/store" -type d -exec chmod u+w {} +   # (the files keep the store's modes)
    find "$R/nix/store" -type l | while read -r p; do
      local t; t=$(readlink "$p")
      case $t in /nix/store/*)
        rm "$p"; ln -s "$(relpath "$R$t" "$(dirname "$p")")" "$p"
        printf '%s\t%s\n' "${p#"$R"/}" "$t" >> "$IN/nix-arm64.absolute";;
      esac
    done
    touch "$m"
  fi
  echo "$R"
}
# the absolute symlinks nix_arm64 made relative, absolute again in a staged copy (as a copy from a
# real /nix/store has them)
absolute_again() {   # <folder that holds nix/store>
  [ -f "$IN/nix-arm64.absolute" ] || return 0
  local p t
  while IFS="$(printf '\t')" read -r p t; do
    if [ -L "$1/$p" ]; then ln -sfn "$t" "$1/$p"; fi
  done < "$IN/nix-arm64.absolute"
}
mac_prepare() {
  case $1 in
    mac-redis)
      copy $XM/redis $XM/redis-ram $XM/redis-bridge $XM/redis-composed $X/redis-composed
      for d in $XM/redis $XM/redis-ram $XM/redis-bridge; do mac_redis_files "$d"; done
      mac_redis_files $XM/redis-composed/hydo ..;;
    mac-redis-racy)   # the composed plan's racy transfer (arm64 builds take no params:)
      copy $X/redis-composed
      rm -rf "${ST:?}/$XM/redis-composed-racy"; cp -R "$T/$XM/redis-composed" "$ST/$XM/redis-composed-racy"
      sed 's/^  TRANSFER: atomic /  TRANSFER: racy   /' "$ST/$XM/redis-composed-racy/hydo/hydo.yaml" > "$ST/x.yaml"
      mv "$ST/x.yaml" "$ST/$XM/redis-composed-racy/hydo/hydo.yaml"
      mac_redis_files $XM/redis-composed-racy/hydo ..;;
    mac-ledger)
      need_release; copy $XM/ledger $XM/ledger-spin
      mkdir -p "$ST/$XM/ledger-spin/bin"; cp "$REL/examples/ledger-spin/bin/spin_load" "$REL/examples/ledger-spin/bin/spin_count" "$ST/$XM/ledger-spin/bin/";;
    mac-node-kv)   # fetch-node-arm64.sh's file list and steps over the closure fetched here
      copy $X/node-kv sdk/js
      local NODE=/nix/store/vh315js3yiw2j58jfm5m934snawzzykw-nodejs-slim-22.2.0 R
      R=$(nix_arm64 $NODE)
      local rel; rel=$(sed -n '/^FILES="$/,/^"$/p' "$T/$X/node-kv/fetch-node-arm64.sh" | sed "s#^\$NODE/#$NODE/#" \
        | grep '^/nix/store/' | sed 's#^/##' | tr '\n' ' ')
      local F=$ST/$X/node-kv/files; rm -rf "$F"; mkdir -p "$F"
      # shellcheck disable=SC2086
      (cd "$R" && tar cf - $rel) | (cd "$F" && tar xf - && chmod -R u+w nix)
      local L=$F/nix/store/x884b7ks41l0zvfvdw1m3xb0l3j1yjlc-gcc-13.2.0-lib/lib/libgcc_s.so.1
      case $(readlink "$L") in /*) ln -sfn ../../pp8jrab5a9qfpn3la57r37x8kw2hrcil-gcc-13.2.0-libgcc/lib/libgcc_s.so.1 "$L";; esac
      (cd "$ST/$X/node-kv" && shasum -a 256 -c --quiet files.sha256) || die "node-kv: files/ is not files.sha256";;
    mac-python-kv)   # fetch-python-arm64.sh's selection over the closure fetched here
      copy $X/python-kv sdk/python
      local PY=/nix/store/ic0l2ykss28lr7r4by14inb3pqw87yza-python3-3.11.9 R
      R=$(nix_arm64 $PY)
      local L=${PY#/}/lib/python3.11 F=$ST/$X/python-kv/files lst=$IN/python-list
      "$T/target/release/hydo-elfdeps" --root "$R" "$PY/bin/python3.11" \
        $(cd "$R" && ls "$L"/lib-dynload/*.so | grep -v _tkinter | sed 's#^#/#') > "$IN/python-deps" \
        || die "hydo-elfdeps failed (cargo build --release -p hydo-tools)"
      { (cd "$R" && find "$L" -mindepth 1 \( -path '*/test' -o -path '*/idlelib' -o -path '*/tkinter' -o -path '*/turtledemo' \
            -o -path '*/ensurepip' -o -path '*/lib2to3' -o -path '*/pydoc_data' -o -path '*/config-3.11-*' \
            -o -path '*/site-packages' -o -name '_tkinter*' \) -prune -o \( -name '*.opt-1.pyc' -o -name '*.opt-2.pyc' \) -prune -o -print)
        echo "${PY#/}/bin/python3.11"; sed 's#^/##' "$IN/python-deps"; } | LC_ALL=C sort -u > "$lst"
      rm -rf "$F"; mkdir -p "$F"
      (cd "$R" && tar -c -n -f - -T "$lst") | (cd "$F" && tar xf -)
      absolute_again "$F"
      mkdir -p "$F/opt/pykv"
      (cd "$T/sdk/python/src" && find hydo_assert -name '*.py' -o -name py.typed | LC_ALL=C sort | tar cf - -T -) | (cd "$F/opt/pykv" && tar xf -)
      find "$F" -type d -exec chmod u+w {} +
      (cd "$ST/$X/python-kv" && shasum -a 256 -c --quiet files.sha256) || die "python-kv: files/ is not files.sha256";;
    mac-rust-kv)   # build-rkv.sh aarch64 in a copy at the one path $RKV_HOME/hydo (see rust-kv)
      local fh=$RKV_HOME
      rm -rf "$fh/hydo"; mkdir -p "$fh/hydo/$X" "$fh/hydo/sdk"
      cp -R "$T/$X/rust-kv" "$fh/hydo/$X/"; cp -R "$T/sdk/rust" "$fh/hydo/sdk/"
      rm -rf "$fh/hydo/$X/rust-kv/target" "$fh/hydo/$X/rust-kv/bin" "$fh/hydo/sdk/rust/target"
      ( cd "$fh/hydo/$X/rust-kv" && bash ./build-rkv.sh aarch64 ) > "$IN/rkv-arm64.log" 2>&1 || { tail "$IN/rkv-arm64.log" >&2; die "rkv failed"; }
      copy $X/rust-kv; mkdir -p "$ST/$X/rust-kv/bin"; cp "$fh/hydo/$X/rust-kv/bin/rkv" "$ST/$X/rust-kv/bin/rkv"
      say "rkv sha256 $(sha "$ST/$X/rust-kv/bin/rkv" | cut -c1-16)";;
    mac-shop)   # shop-{buggy,fixed} for arm64 in build/ (the manifest's default bin)
      copy $X/shop; mkdir -p "$ST/build"
      gobuild "$ST/$X/shop/src" "$ST/build/shop-buggy" arm64
      gobuild "$ST/$X/shop/src" "$ST/build/shop-fixed" arm64 -tags=fixed;;
    mac-grpc)   # orderd for arm64, the generated drivers with the arm64 client (pinned fd762fa0...)
      copy hydo/grpcgen; mac_redis_files hydo/grpcgen/demo
      mkdir -p "$ST/hydo/grpcgen/demo/build"
      ( cd "$ST/hydo/grpcgen" && GOPATH=$W/go GOMODCACHE=$IN/gomod GOCACHE=$W/go-cache GOFLAGS=-mod=readonly GOTOOLCHAIN=local \
          CGO_ENABLED=0 GOOS=linux GOARCH=arm64 "$GO" build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" \
          -o demo/build/orderd-arm64 ./demo/orderd )
      say "orderd-arm64 sha256 $(sha "$ST/hydo/grpcgen/demo/build/orderd-arm64" | cut -c1-16)"
      mkdir -p "$IN/grpc-client"
      [ -f "$IN/grpc-client/hydo-grpc.arm64" ] && [ "$(sha "$IN/grpc-client/hydo-grpc.arm64" | cut -c1-8)" = fd762fa0 ] \
        || "$T/hydo/grpcgen/build-client.sh" --out "$IN/grpc-client" amd64 arm64
      [ "$(sha "$IN/grpc-client/hydo-grpc.arm64" | cut -c1-8)" = fd762fa0 ] || die "the arm64 gRPC client is not the pinned fd762fa0..."
      ( cd "$ST" && HYDO_GRPC_CLIENT=$IN/grpc-client "$T/target/release/hydo-grpc-gen" hydo/grpcgen/demo/proto \
          --hints hydo/grpcgen/demo/hints.yaml --out hydo/grpcgen/demo/build/gen --arch amd64,arm64 );;
    mac-voting)   # the arm64 images as docker save archives (fetch-images-mac.sh, network once)
      copy $X/compose-voting
      mkdir -p "$IN/voting-mac"
      if [ ! -e "$IN/voting-mac/.done" ]; then
        cp "$T/$X/compose-voting/fetch-images-mac.sh" "$IN/voting-mac/"
        ( cd "$IN/voting-mac" && HYDO_FETCH_IMAGE=$P/bin/hydo-fetch-image sh fetch-images-mac.sh ) && touch "$IN/voting-mac/.done"
      fi
      rm -rf "$ST/$X/compose-voting/images"; ln -s "$IN/voting-mac/images" "$ST/$X/compose-voting/images";;
    mac-redis-oci)   # the arm64 manifest by digest as a docker save archive (a Mac has no skopeo);
      # the staged manifest names the archive instead of the OCI layout (docs/C2-MAC.md)
      copy $X/redis-oci; mkdir -p "$IN/oci"
      [ -f "$IN/oci/redis-7.2.4-arm64.docker.tar" ] || "$P/bin/hydo-fetch-image" docker.io/library/redis \
        sha256:4da694ba727afad9a027c075a3e32356affea4db13482d489822107dcf09d4ed redis:7.2.4 "$IN/oci/redis-7.2.4-arm64.docker.tar"
      ln -sfn "$IN/oci/redis-7.2.4-arm64.docker.tar" "$ST/$X/redis-oci/redis-7.2.4-arm64.docker.tar"
      sed 's#arm64: redis-7.2.4-arm64-oci#arm64: redis-7.2.4-arm64.docker.tar#' "$ST/$X/redis-oci/hydo.yaml" > "$ST/x.yaml"
      mv "$ST/x.yaml" "$ST/$X/redis-oci/hydo.yaml";;
    release) release_examples;;
    *) die "unknown stage $1 for $PLAT";;
  esac
}
release_examples() {   # the release's own examples, as a user builds them
  need_release
  rm -rf "$ST/release"; mkdir -p "$ST/release"; cp -R "$REL/examples" "$ST/release/examples"
}

# --------------------------------------------------------------------------------------- build
rmtree() {   # a build's outputs, with their read-only store folders (Linux: owned by another uid too)
  if [ "$PLAT" = hvm ]; then chmod -R u+w "$@" 2> /dev/null || true; rm -rf "$@"
  else sudo -n rm -rf "$@"; fi
}
build() {   # normal|keep <dir> <manifest> <out> [args]
  local owners=$1 dir=$2 man=$3 out=$4; shift 4
  [ -x "$P/bin/hydo" ] || die "no prefix: stage.sh prefix"
  [ -d "$dir" ] || die "no $dir (stage.sh prepare)"
  [ -d "$out" ] && rmtree "$out"
  mkdir -p "$out"
  local lock=(); command -v flock > /dev/null && lock=(flock "$HOME/.hydo-imgbuild.lock")
  if [ "$PLAT" = hvm ]; then
    ( cd "$dir" && "$P/bin/hydo" build "$man" --out "$out" "$@" )
  elif [ "$owners" = keep ]; then
    chmod 777 "$out"
    sudo -n timeout -s KILL 3600 setpriv --reuid=1000 --regid=100 --clear-groups \
      env -i HOME="$FH" PATH="${HYDO_NIX_BIN:-$HOME/.nix-profile/bin}:/usr/bin:/bin" LANG=C LC_ALL=C HYDO_KEEP_OWNERS=1 \
        "${lock[@]}" /usr/bin/nice -n 10 /usr/bin/env -C "$dir" "$P/bin/hydo" build "$man" --out "$out" "$@"
  else
    ( cd "$dir" && env HOME="$FH" "${lock[@]}" nice timeout -s KILL 3600 "$P/bin/hydo" build "$man" --out "$out" "$@" )
  fi
  # the intermediates (root trees, file systems before they are joined)
  # (Linux only: on the Mac every output file is part of the image)
  if [ "$PLAT" != hvm ]; then
    rmtree "$out"/*-root "$out"/*-ext4.img "$out"/*-data.img "$out"/*-oci-tmp "$out"/*-root.closures
  else
    rmtree "$out"/*-root
  fi
}

cmd=${1:-}; shift || true
case $cmd in
  prefix) prefix;;
  prepare)
    [ $# = 1 ] || die "prepare <stage>"
    case $PLAT in hvm) mac_prepare "$1";; *) linux_prepare "$1";; esac;;
  build) [ $# -ge 4 ] || die "build normal|keep <dir> <manifest> <out> [args]"; build "$@";;
  *) sed -n '2,32p' "$0"; exit 2;;
esac
