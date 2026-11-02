#!/bin/bash
# M138, on the bare-metal test machine: the cost of a run with fresh VMs (the default: the next VM
# made on a helper thread) and with in-place reuse (HYDO_HVX_REUSE=1, no check), the engine on the
# side module. One `hydo-hvx serve` per (image, schedule, mode) with N runs and
# HVX_EXPLORE_PHASES=1; prints each run's RESULT fingerprint and its PHASES total and parts.
#   timing.sh [N]      -> /var/tmp/m138/timing/
set -u
N=${1:-5}
T=$(cd "$(dirname "$0")/../../.." && pwd)
B=~/hydo/build
O=/var/tmp/m138/timing
mkdir -p $O
memwait() { local n=0; while [ "$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)" -lt "$1" ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done; }
one() {  # <name> <image> <schedule> <mem MiB> <mode> [NAME=value...]
  local name=$1 img=$2 s=$3 mem=$4 mode=$5; shift 5
  local d=$O/$name-$mode; sudo rm -rf "$d"; mkdir -p "$d"
  memwait "$mem"
  local req=""; for i in $(seq 1 "$N"); do req="${req}RUN t$i $s
"; done
  printf '%sQUIT\n' "$req" | timeout 7200 ~/bin/hvx-run sudo env HVX_DEV=${HVX_DEV:-/dev/hvxgc} \
    HVX_EXPLORE_PHASES=1 "$@" "$T/target/release/hydo-hvx" serve $B/bzImage $B/initramfs-kafka.cpio.gz \
    "$B/$img" "$d" > "$d/out.txt" 2> "$d/err.txt"
  echo "== $name $mode: $(grep -c '^RESULT' "$d/out.txt") runs, fps: $(grep -o 'fp=[0-9a-f]*' "$d/out.txt" | sort | uniq -c | tr '\n' ' ')"
  grep '^PHASES' "$d/err.txt" | awk '{o=$2; for(i=3;i<=NF;i++){split($i,a,"="); if(a[1]=="total"||a[1]=="fresh"||a[1]=="r_ram"||a[1]=="r_disk"||a[1]=="rec_fresh"||a[1]=="rec_restore"||a[1]=="teardown"||a[1]=="rec_teardown"||a[1]=="reuse_ram"||a[1]=="reuse_disk") o=o" "$i}; print "   "o}'
}
for mode in fresh reuse; do
  env=(); [ $mode = reuse ] && env=(HYDO_HVX_REUSE=1)
  one redis-clean explore-ob-redis-nq-disk.img "disk=none | run" 3072 $mode "${env[@]}"
  one redis-cut explore-ob-redis-nq-disk.img "disk=cut | run C=12 R=everysec" 3072 $mode "${env[@]}"
  one stack-cut explore-disk.img "disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24" 10240 $mode "${env[@]}"
done
echo TIMING-END
