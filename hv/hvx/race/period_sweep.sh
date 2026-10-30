#!/bin/bash
# For one race window W: try 200 PMU tick periods (tick TSC scaled so the guest
# clock rate stays the same); replay every setting that hits the bug once more.
W=$1; OUT=50000; SEATS=5000
cd ~/hydo/hvx/race; B=~/hydo/build
CL="console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys hydo_race=$W,$OUT,$SEATS"
one() { ~/bin/hvx-run sudo env HVX_PMU_CLOCK=1 HVX_PMU_PERIOD=$1 HVX_PMU_TICK=$2 ./hvx_race_probe $B/bzImage $B/initramfs-race.cpio.gz "$CL" 2>/dev/null; }
for k in $(seq 0 199); do
  p=$(( 262144 + k * 7919 )); t=$(( 3200000 * p / 1048576 ))
  r=$(one $p $t)
  lost=$(echo "$r" | grep -o 'lost=[-0-9]*' | cut -d= -f2)
  fp=$(echo "$r" | grep -o 'fp=[0-9a-f]*')
  rep=""
  if [ -n "$lost" ] && [ "$lost" != 0 ]; then
    r2=$(one $p $t); fp2=$(echo "$r2" | grep -o 'fp=[0-9a-f]*')
    [ "$fp" = "$fp2" ] && rep="replay=IDENTICAL" || rep="replay=MISMATCH($fp2)"
  fi
  echo "W=$W k=$k period=$p lost=${lost:-ERR} $fp $rep"
done
