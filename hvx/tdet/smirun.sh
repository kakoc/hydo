#!/bin/bash
# smirun.sh <tag> <dev> <nruns> <hit-runs...>: serve session of one example's clean schedule on
# <dev>; during each listed run (1-based), ~15 s into it, read ONE EFI variable (a few SMIs).
T=$1; DEV=$2; N=$3; shift 3; HITS="$*"
O=${M65_OUT:-/var/tmp/m65-tdet}/$T; rm -rf $O; mkdir -p $O/dump
for i in $(seq 1 $N); do echo "RUN $T$i disk=none dseed=1 | run"; done > $O/runs
cd ~/hydo-tdet/hvx
( timeout 7200 ~/bin/hvx-run sudo env LD_PRELOAD=$PWD/tdet/devshim.so HVX_DEV=$DEV HVX_EXPLORE_RETLOG=1 HVX_EXPLORE_DUMP=$O/dump HVX_EXPLORE_DUMP_HASHONLY=1 ./hvx_explore --ram-mib 2048 serve ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz "${IMG:?set IMG to a guest image}" $O < $O/runs > $O/results.txt 2> $O/stderr; echo rc=$? > $O/done ) &
V=/sys/firmware/efi/efivars/BIOSBootIdentify-4efdd949-e871-4a21-96b6-b161b271b9bf
for h in $HITS; do
  k=$((h-1)); t=0
  until [ $(grep -c "^RESULT\|^READY" $O/results.txt 2>/dev/null || echo 0) -ge $((k+1)) ] || [ -f $O/done ] || [ $t -ge 3600 ]; do sleep 1; t=$((t+1)); done
  sleep 15
  s0=$(sudo python3 $(dirname "$0")/smicount.py | cut -d" " -f1); sudo cat $V > /dev/null; s1=$(sudo python3 $(dirname "$0")/smicount.py | cut -d" " -f1)
  echo "run $h: efivar read at $(date -u +%T) smi $s0 -> $s1" >> $O/hits.log
done
wait
