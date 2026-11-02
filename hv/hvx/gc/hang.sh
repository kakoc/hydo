#!/bin/bash
# M138, on the bare-metal test machine: open item 30 (d), the lock-step hang of M127's gRPC stack
# seed 6 (M136: TIMEOUT workload, the agent's last console frame cut in the middle), on a copy of
# M136's planted gRPC stack image with hydo-agent 0.3.0, over the serial line and over the
# host-call port, each once (a hang takes the engine's 900 s wall limit). Alone, with >= 6 GiB.
#   hang.sh <prefix> <image>    -> /var/tmp/m138/hang/
set -u
P=$1; IMG=$2
O=/var/tmp/m138/hang
mkdir -p $O
S='disk=cut dseed=581cc68c71772e2c | run K=front@27+2 C=30 P=front@2+4 T=17:-30 H=orders@8-11:status=UNAVAILABLE,rpc=Place H=orders.place@3-30:drop,p=50,seed=194'
for mode in port serial; do
  hc=0; [ $mode = port ] && hc=1
  d=$O/$mode; sudo rm -rf $d $d.out
  n=0; while [ "$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)" -lt 6144 ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done
  t0=$(date +%s)
  (cd $O && timeout -s KILL 3600 ~/bin/hvx-run sudo env HOME="$HOME" HVX_DEV=${HVX_DEV:-/dev/hvxgc} \
    HYDO_HVX_HOSTCALL=$hc "$P/bin/hydo" run --image "$IMG" "$S" --out $d > $d.out 2>&1)
  echo "$mode ($(( $(date +%s) - t0 )) s): $(grep -a '^RESULT' $d.out | tail -1 | cut -c1-260)"
done
echo HANG-END
