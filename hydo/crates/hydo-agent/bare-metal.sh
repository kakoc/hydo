#!/usr/bin/env bash
# Runs a command in this crate on a Linux machine from the Mac (the native tests need Linux:
# /proc, signalfd, inotify, netlink). Copies the repository's files (tracked and new, not ignored;
# nothing is deleted there, so files the remote run wrote stay until they are fetched) to
# <host>:<tree> and runs the command in <tree>/hydo/crates/hydo-agent as the calling user:
#   hydo/crates/hydo-agent/bare-metal.sh [<host> [<tree>]] [-- <command>]
# (defaults: the bare-metal machine ($HYDO_BARE_METAL) and hydo-ga2; the command defaults to the tests: cargo test --locked).
# Examples: `-- cargo test --locked --test native pid1 -- --ignored` (the PID-1 check; sudo -n),
# `-- ./build.sh all` (the static binaries). Rust there is its private rustup: . ~/hydo-rust/env.sh.
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../../.." && pwd)"
HOST=bare-metal
TREE=hydo-ga2
[ $# -gt 0 ] && [ "$1" != "--" ] && { HOST=$1; shift; }
[ $# -gt 0 ] && [ "$1" != "--" ] && { TREE=$1; shift; }
[ "${1:-}" = "--" ] && shift
CMD="${*:-cargo test --locked}"
ssh "$HOST" "mkdir -p ~/$TREE"
(cd "$TOP" && git ls-files --cached --others --exclude-standard -z) |
  rsync -a --from0 --files-from=- "$TOP/" "$HOST:$TREE/"
ssh "$HOST" "cd ~/$TREE/hydo/crates/hydo-agent && . ~/hydo-rust/env.sh && $CMD"
