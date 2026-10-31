#!/usr/bin/env bash
# Runs the agent's native tests on a Linux machine from the Mac (they need Linux: /proc, signalfd,
# inotify, netlink). Copies the sources the agent's workspace reads (this crate, the protocol crate,
# the schema folder, the toolchain pin) to <host>:<tree> and runs there, as the calling user:
#   hydo/crates/hydo-agent/bare-metal.sh [<host> [<tree>]] [-- <command>]
# (defaults: the bare-metal machine ($HYDO_BARE_METAL) and hydo-ga2; the command defaults to the tests: cargo test --locked).
# Rust on the bare-metal test machine is its private rustup: . ~/hydo-rust/env.sh.
set -euo pipefail
TOP="$(cd "$(dirname "$0")/../../.." && pwd)"
HOST=bare-metal
TREE=hydo-ga2
[ $# -gt 0 ] && [ "$1" != "--" ] && { HOST=$1; shift; }
[ $# -gt 0 ] && [ "$1" != "--" ] && { TREE=$1; shift; }
[ "${1:-}" = "--" ] && shift
CMD="${*:-cargo test --locked}"
ssh "$HOST" "mkdir -p ~/$TREE/hydo/crates ~/$TREE/hydo/schema ~/$TREE/.cargo"
rsync -a --delete --exclude target "$TOP/hydo/crates/hydo-agent" "$TOP/hydo/crates/hydo-agent-proto" "$HOST:$TREE/hydo/crates/"
rsync -a --delete "$TOP/hydo/schema/" "$HOST:$TREE/hydo/schema/"
rsync -a "$TOP/rust-toolchain.toml" "$TOP/Cargo.toml" "$HOST:$TREE/"
rsync -a "$TOP/.cargo/config.toml" "$HOST:$TREE/.cargo/"
ssh "$HOST" "cd ~/$TREE/hydo/crates/hydo-agent && . ~/hydo-rust/env.sh && $CMD"
