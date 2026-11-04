#!/usr/bin/env bash
# Runs a command in this crate on the bare-metal machine from the Mac (the native tests need
# Linux: /proc, signalfd, inotify, netlink). Copies the repository's files (tracked and new, not
# ignored; nothing is deleted there, so files the remote run wrote stay until they are fetched) to
# <tree> in the machine's home and runs the command in <tree>/hydo/crates/hydo-agent as the
# calling user:
#   hydo/crates/hydo-agent/bare-metal.sh [<tree>] [-- <command>]
# (default tree hydo-ga2; the command defaults to the tests: cargo test --locked). The machine is
# reached through machines.local.sh (machines.sh at the repository root).
# Examples: `-- cargo test --locked --test native pid1 -- --ignored` (the PID-1 check; sudo -n),
# `-- ./build.sh all` (the static binaries). Rust there is its private rustup: . ~/hydo-rust/env.sh.
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../../.." && pwd)"
. "$TOP/machines.sh"
need_bare_metal
TREE=hydo-ga2
[ $# -gt 0 ] && [ "$1" != "--" ] && { TREE=$1; shift; }
[ "${1:-}" = "--" ] && shift
CMD="${*:-cargo test --locked}"
on_bare_metal "mkdir -p ~/$TREE"
(cd "$TOP" && git ls-files --cached --others --exclude-standard -z) |
  rsync -a --from0 --files-from=- "$TOP/" "$BARE_METAL:$TREE/"
on_bare_metal "cd ~/$TREE/hydo/crates/hydo-agent && . ~/hydo-rust/env.sh && $CMD"
