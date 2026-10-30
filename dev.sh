#!/usr/bin/env bash
# Local helper: sync the source tree to the build machine and run a command there.
# Usage: ./dev.sh "<remote shell command>"
#   e.g. ./dev.sh "bash hv/hvx/images/build-stack.sh"     (guest images; outputs stay in build/ there)
set -euo pipefail

REMOTE="${HYDO_REMOTE:?}"
DIR="${HYDO_REMOTE_DIR:-hydo}"

rsync -az --delete \
  --exclude target \
  --exclude build \
  --exclude '.git' \
  ./ "$REMOTE:$DIR/"

if [ "$#" -gt 0 ]; then
  ssh "$REMOTE" "cd $DIR && source ~/.nix-profile/etc/profile.d/*.sh 2>/dev/null; export CC=clang; $*"
fi
