#!/bin/bash
# M138, on the bare-metal test machine: `hydo run` of schedules on agent images with the engine on
# the side module (HVX_DEV, default /dev/hvxgc), over the serial line and over the host-call port
# (HYDO_HVX_HOSTCALL=1), each twice from a fresh boot, then `hydo repro --transcript` of the
# first port run's control transcript. Every VM goes through ~/bin/hvx-run; a stack-sized image
# runs only with >= 6 GiB available.
#   port-runs.sh <prefix> <list> <outdir>
# <list>: name|image|schedule lines. One line per run on stdout:
#   <name> <n> <serial|port> <r1|r2|replay>: <RESULT without secs>
set -u
P=$1; LIST=$2; OUT=$3
DEV=${HVX_DEV:-/dev/hvxgc}
mkdir -p "$OUT"
memwait() { local n=0; while [ "$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)" -lt "$1" ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done; }
quiet() { local n=0; while [ -e /var/tmp/hydo-QUIET ] && [ $n -lt 720 ]; do sleep 10; n=$((n+1)); done; }
res() { grep -a '^RESULT' "$1" | tail -1 | sed 's/ secs=[0-9.]*//'; }
declare -A seen
while IFS='|' read -r name img s; do
  case "$name" in ''|'#'*) continue;; esac
  seen[$name]=$(( ${seen[$name]:-0} + 1 )); n=${seen[$name]}
  for mode in serial port; do
    hc=0; [ $mode = port ] && hc=1
    for k in r1 r2; do
      d=$OUT/$name-$n-$mode-$k; sudo rm -rf "$d" "$d.out"
      quiet; memwait 6144
      (cd "$OUT" && timeout -s KILL 3600 ~/bin/hvx-run sudo env HOME="$HOME" HVX_DEV="$DEV" \
        HYDO_HVX_HOSTCALL=$hc "$P/bin/hydo" run --image "$img" "$s" --out "$d" > "$d.out" 2>&1)
      echo "$name $n $mode $k: $(res "$d.out")"
    done
    if [ $mode = port ]; then
      d=$OUT/$name-$n-$mode-replay; sudo rm -rf "$d" "$d.out"
      t=$OUT/$name-$n-port-r1/control.jsonl
      quiet; memwait 6144
      (cd "$OUT" && timeout -s KILL 3600 ~/bin/hvx-run sudo env HOME="$HOME" HVX_DEV="$DEV" \
        HYDO_HVX_HOSTCALL=1 "$P/bin/hydo" repro --image "$img" --transcript "$t" --out "$d" > "$d.out" 2>&1)
      echo "$name $n $mode replay: $(res "$d.out") $(grep -a -m1 -oE 'REPRODUCED|DIFFERENT|NOT REPRODUCED' "$d.out")"
    fi
  done
done < "$LIST"
echo PORT-RUNS-DONE
