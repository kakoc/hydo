#!/usr/bin/env bash
# Cargo's rustc wrapper for this repository (.cargo/config.toml, build.rustc-wrapper; docs/CI.md
# "The compile cache"). Cargo runs it as `rustc-wrapper.sh <rustc> <arguments>...`.
#
# With sccache on PATH, a compile goes through sccache with hydo's own local cache: the git-ignored
# directory .sccache/ of the main checkout, shared by every git worktree of it (the worktrees
# under .claude/worktrees/ and any other), and a sccache server of its own (a port derived from
# that directory), so no other project's builds and no other sccache setting are touched. Nothing
# leaves the machine: no remote cache is configured here.
#
# Without sccache, or with HYDO_SCCACHE=0, it runs the compiler itself: nothing needs sccache.
#
# The release profile never uses the cache: release tarballs and the pinned guest programs are
# built with `--release` (directly or through their build scripts) and must not depend on what a
# cache holds. A release-profile compile is recognised by its output directory
# (<target>[/<triple>]/release/...), which Cargo passes as --out-dir or -o.
#
# By hand: `.cargo/rustc-wrapper.sh --show-stats` (or --zero-stats, --stop-server) talks to this
# repository's sccache server.
case ${1:-} in
  --show-stats | --zero-stats | --stop-server) admin=1 ;;
  *) admin=0 ;;
esac
if [ "${HYDO_SCCACHE:-1}" = 0 ] || ! command -v sccache >/dev/null 2>&1; then
  if [ $admin = 1 ]; then
    echo "rustc-wrapper.sh: sccache is not installed (or HYDO_SCCACHE=0)" >&2
    exit 1
  fi
  exec "$@"
fi
for a in "$@"; do
  case $a in
    */release/deps | */release/build/* | */release/deps/*) exec "$@" ;;
  esac
done

if [ -n "${HYDO_SCCACHE_DIR:-}" ]; then
  SCCACHE_DIR=$HYDO_SCCACHE_DIR
else
  # this checkout's root (the directory above .cargo/), then the main checkout's when it is a
  # git worktree: its .git is a file "gitdir: <main>/.git/worktrees/<name>"
  root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
  if [ -f "$root/.git" ]; then
    gitdir=$(sed -n 's/^gitdir: //p' "$root/.git")
    case $gitdir in
      /*/.git/worktrees/*) root=${gitdir%/.git/worktrees/*} ;;
      *)
        common=$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) &&
          root=${common%/.git}
        ;;
    esac
  fi
  SCCACHE_DIR=$root/.sccache
fi
export SCCACHE_DIR
# a server of this cache's own: a port in 20000..29999 derived from the cache's path
port=$(printf '%s' "$SCCACHE_DIR" | cksum | cut -d' ' -f1)
SCCACHE_SERVER_PORT=$((20000 + port % 10000))
export SCCACHE_SERVER_PORT
# the local disk cache only: no remote backend, and no configuration file (a user's own
# sccache configuration file could name one)
unset SCCACHE_BUCKET SCCACHE_GCS_BUCKET SCCACHE_AZURE_CONNECTION_STRING SCCACHE_REDIS \
  SCCACHE_REDIS_ENDPOINT SCCACHE_MEMCACHED SCCACHE_MEMCACHED_ENDPOINT SCCACHE_WEBDAV_ENDPOINT \
  SCCACHE_GHA_ENABLED SCCACHE_OSS_BUCKET SCCACHE_COS_BUCKET
SCCACHE_CONF=/dev/null
export SCCACHE_CONF
: "${SCCACHE_CACHE_SIZE:=${HYDO_SCCACHE_SIZE:-8G}}"
export SCCACHE_CACHE_SIZE
exec sccache "$@"
