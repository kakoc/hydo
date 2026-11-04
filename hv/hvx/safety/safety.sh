#!/usr/bin/env bash
# The hvx safety net from the Mac (Milestone 97; how-to in hv/hvx/SAFETY.md).
#
#   safety.sh build       on the BUILD machine (default the bare-metal machine): debug kernel,
#                         hvx.ko for it + tests + reproducers (bundle "debug"), the unfixed
#                         module of main (bundle "debug-orig"), syzkaller, the fuzzing image
#   safety.sh push        copy the build outputs to the RUN machine (the build machine)
#   safety.sh debug [t,t] on the RUN machine: boot the debug kernel, run tests, report splats
#   safety.sh smoke [MIN] on the RUN machine: bounded syzkaller smoke run (default 20 min)
#   safety.sh campaign HOURS [N] | status | stop      the long syzkaller campaign
#   safety.sh side        prove a module change inert on the bare-metal machine (side-check.sh)
#
# The build machine only builds (it never boots a VM: on the bare-metal machine KVM would turn VMX
# on next to the shared hvx module); the run machine needs nested KVM. Both use ~/hydo-safety.
# The build step runs on the bare-metal machine, the run steps on the build machine; both are
# reached through machines.local.sh (machines.sh at the repository root).
set -euo pipefail
. "$(cd "$(dirname "$0")/../../.." && pwd)/machines.sh"
HERE="$(cd "$(dirname "$0")" && pwd)"
HVX="$(cd "$HERE/.." && pwd)"
ROOT="$(cd "$HVX/.." && pwd)"
STAGE=${SAFETY_STAGE:-/tmp/hydo-safety-stage}

on_build() { need_bare_metal; on_bare_metal "$@"; }
on_run() { need_build_machine; on_build_machine "$@"; }
sync_src() {	# build | run
  if [ "$1" = build ]; then need_bare_metal; on_bare_metal 'mkdir -p ~/hydo-safety/src'; H=$BARE_METAL
  else need_build_machine; on_build_machine 'mkdir -p ~/hydo-safety/src'; H=$BUILD_MACHINE; fi
  rsync -az --exclude 'dist/out' --exclude 'dist/assets' --exclude '*.img' "$HVX/" "$H:hydo-safety/src/hvx/"
}

cmd=${1:-}; shift || true
case "$cmd" in
build)
  sync_src build
  git -C "$ROOT" show main:hv/hvx/hvx.c > "$STAGE.hvx-main.c"
  rsync -az "$STAGE.hvx-main.c" "$BARE_METAL:hydo-safety/src/hvx-main.c"
  on_build 'set -e; cd ~/hydo-safety/src/hvx/safety
    bash build-kernel.sh
    bash build-debug.sh
    DEBUG_OUT=debug-orig HVX_C=$HOME/hydo-safety/src/hvx-main.c bash build-debug.sh
    bash syz/build-syzkaller.sh
    bash syz/build-image.sh'
  ;;
push)
  need_bare_metal; need_build_machine
  mkdir -p "$STAGE"
  rsync -azS --delete --include='/debug/***' --include='/debug-orig/***' --include='/kbuild/' \
    --include='/kbuild/vmlinux' --include='/kbuild/bzImage' --include='/kbuild/.config' \
    --include='/syz/' --include='/syz/image.ext4' --include='/syz/id_syz*' \
    --include='/syzkaller/' --include='/syzkaller/bin/***' --exclude='*' \
    "$BARE_METAL:hydo-safety/" "$STAGE/"
  sync_src run
  rsync -azS "$STAGE/" "$BUILD_MACHINE:hydo-safety/"
  ;;
debug)    sync_src run; on_run "bash ~/hydo-safety/src/hvx/safety/run-debug.sh ${1:-}" ;;
smoke)    sync_src run; on_run "bash ~/hydo-safety/src/hvx/safety/syz/campaign.sh smoke ${1:-20}" ;;
campaign) sync_src run; on_run "bash ~/hydo-safety/src/hvx/safety/syz/campaign.sh start ${1:-10} ${2:-4}" ;;
status|stop) on_run "bash ~/hydo-safety/src/hvx/safety/syz/campaign.sh $cmd" ;;
side)     "$HERE/side-check.sh" "${1:-all}" ;;
*) sed -n '2,16p' "$0"; exit 2 ;;
esac
