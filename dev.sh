#!/usr/bin/env bash
# Local helper: sync the source tree to the build machine and run a command there.
# Usage: ./dev.sh "<remote shell command>"
#   e.g. ./dev.sh "bash hv/hvx/images/build-stack.sh"     (guest images; outputs stay in build/ there)
# The build machine is reached through machines.local.sh (machines.sh at the repository root).
# HYDO_REMOTE_DIR is the tree's directory in the build machine's home (default hydo).
set -euo pipefail

. "$(cd "$(dirname "$0")" && pwd)/machines.sh"
need_build_machine
DIR="${HYDO_REMOTE_DIR:-hydo}"

rsync -az --delete \
  --exclude target \
  --exclude build \
  --exclude '.git' \
  --exclude machines.local.sh \
  --exclude AGENTS.local.md \
  --exclude ROADMAP.local.md \
  ./ "$BUILD_MACHINE:$DIR/"

if [ "$#" -gt 0 ]; then
  on_build_machine "cd $DIR && source ~/.nix-profile/etc/profile.d/*.sh 2>/dev/null; export CC=clang; $*"
fi
