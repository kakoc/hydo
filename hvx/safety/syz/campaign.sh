#!/usr/bin/env bash
# Run a syzkaller campaign against /dev/hvx (Milestone 97). Runs ON THE BUILD MACHINE BUILD BOX, which
# has nested KVM; never on the bare-metal test machine. Inputs (from build-kernel/-debug/-syzkaller/
# -image, shipped by safety.sh push): $SAFETY/debug/{bzImage,hvx.ko}, $SAFETY/kbuild/vmlinux,
# $SAFETY/syzkaller/bin, $SAFETY/syz/{image.ext4,id_syz}.
#
#   campaign.sh smoke [MIN]     bounded smoke run (default 20 minutes, 2 VMs), then a summary;
#                               re-run it after every module change
#   campaign.sh start HOURS [N] long campaign in the background (N VMs, default 4), pid file
#   campaign.sh status          is it running, corpus/coverage/crash counts, crash titles
#   campaign.sh stop            stop it (by pid file only)
#
# Every VM has 2 vCPUs and VM_MEM MiB (default 2048). The host is shared: a campaign starts only
# when MemAvailable covers its VMs plus MEM_RESERVE (default 4096 MiB), waits up to WAIT_MIN
# minutes for that (default 0), and a guard stops it if MemAvailable falls below MEM_FLOOR
# (default 1536 MiB) while it runs, so someone else's job can have the memory back.
set -uo pipefail
SAFETY=${SAFETY:-$HOME/hydo-safety}
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../lib.sh"
C="$SAFETY/campaign"
PIDF="$C/manager.pid"
VM_MEM=${VM_MEM:-2048}
BUNDLE=${BUNDLE:-debug}	# debug-orig fuzzes the unfixed module of main
mkdir -p "$C" "$SAFETY/gcroots"

qemu_path() {
  local q
  q=$(nix-build "$SAFETY_NIXPKGS" -A qemu_kvm --no-out-link -o "$SAFETY/gcroots/qemu" 2>/dev/null | tail -1)
  [ -x "$q/bin/qemu-system-x86_64" ] || q=$(readlink -f "$SAFETY/gcroots/qemu")
  echo "$q/bin/qemu-system-x86_64"
}

config() {	# workdir nvms procs
  cat > "$1/hvx.cfg" <<EOF
{
	"target": "linux/amd64",
	"http": "127.0.0.1:${HTTP_PORT:-56741}",
	"workdir": "$1",
	"kernel_obj": "$SAFETY/kbuild",
	"kernel_src": "$SAFETY/kbuild/linux",
	"module_obj": ["$SAFETY/$BUNDLE/mod"],
	"image": "$SAFETY/syz/image-$BUNDLE.ext4",
	"sshkey": "$SAFETY/syz/id_syz",
	"syzkaller": "$SAFETY/syzkaller",
	"procs": $3,
	"type": "qemu",
	"sandbox": "none",
	"cover": true,
	"reproduce": true,
	"enable_syscalls": ["openat\$hvx", "mmap\$hvx", "syz_ext_hvx_guest", "ioctl\$HVX_*", "close", "munmap", "dup"],
	"vm": {
		"count": $2,
		"kernel": "$SAFETY/$BUNDLE/bzImage",
		"cpu": 2,
		"mem": $VM_MEM,
		"qemu": "$(qemu_path)",
		"qemu_args": "-enable-kvm -cpu host",
		"cmdline": "nokaslr kmemleak=off panic_on_warn=1"
	}
}
EOF
}

guard() {	# stop the manager if the host runs short of memory
  local pid=$1 floor=${MEM_FLOOR:-1536} a
  while kill -0 "$pid" 2>/dev/null; do
    a=$(awk '/^MemAvailable:/ {print int($2/1024)}' /proc/meminfo)
    if [ "$a" -lt "$floor" ]; then
      echo "$(date -Is) guard: MemAvailable ${a} MiB < ${floor}: stopping the campaign" >> "$C/guard.log"
      kill -INT "$pid"; sleep 30; kill -KILL "$pid" 2>/dev/null; break
    fi
    sleep 20
  done
}

launch() {	# workdir nvms seconds
  local w=$1 n=$2 secs=$3 need
  need=$(( n * (VM_MEM + 512) + ${MEM_RESERVE:-4096} ))
  echo "$(date -Is) waiting for ${need} MiB available (up to ${WAIT_MIN:-0} min)" | tee -a "$C/guard.log"
  wait_free_mem "$need" $(( ${WAIT_MIN:-0} * 60 )) || return 3
  echo "$(date -Is) start: $n VMs, $secs s, workdir $w" | tee -a "$C/guard.log"
  mkdir -p "$w"; config "$w" "$n" 4
  (cd "$w" && exec nice -n 10 timeout -s INT "$secs" "$SAFETY/syzkaller/bin/syz-manager" -config "$w/hvx.cfg" > "$w/manager.log" 2>&1) &
  echo $! > "$PIDF"
  guard "$(cat "$PIDF")" &
  echo $! > "$C/guard.pid"
}

summary() {	# workdir
  local w=$1
  echo "== $w"
  grep -a 'corpus\|coverage\|exec total' "$w/manager.log" | tail -2
  if [ -d "$w/crashes" ]; then
    for d in "$w"/crashes/*/; do
      [ -f "$d/description" ] && printf '  CRASH %s (%s logs%s)\n' "$(cat "$d/description")" \
        "$(ls "$d" | grep -c '^log')" "$([ -f "$d/repro.cprog" ] && echo ', C repro' || true)"
    done
  fi
  true
}

case "${1:-status}" in
smoke)
  w="$C/smoke-$BUNDLE-$(date +%Y%m%d-%H%M%S)"
  launch "$w" 2 $(( ${2:-20} * 60 )) || exit 3
  wait "$(cat "$PIDF")" 2>/dev/null
  while kill -0 "$(cat "$PIDF")" 2>/dev/null; do sleep 10; done
  summary "$w"
  ;;
start)
  [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null && { echo "already running (pid $(cat "$PIDF"))"; exit 1; }
  launch "$C/long" "${3:-4}" $(( ${2:-10} * 3600 )) || exit 3
  echo "campaign: pid $(cat "$PIDF"), log $C/long/manager.log, web UI 127.0.0.1:${HTTP_PORT:-56741} (ssh -L)"
  ;;
status)
  if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then echo "running (pid $(cat "$PIDF"))"; else echo "not running"; fi
  for w in "$C"/long "$C"/smoke-*; do [ -d "$w" ] && summary "$w"; done
  tail -3 "$C/guard.log" 2>/dev/null
  ;;
stop)
  for f in "$PIDF" "$C/guard.pid"; do [ -f "$f" ] && kill -INT "$(cat "$f")" 2>/dev/null; done
  echo "stopped"
  ;;
*) echo "usage: $0 smoke [MIN] | start HOURS [N] | status | stop" >&2; exit 2 ;;
esac
