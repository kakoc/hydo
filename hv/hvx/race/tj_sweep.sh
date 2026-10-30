#!/bin/bash
# For one race window W: 200 tick-jitter seeds at the default PMU period; replay hits.
W=$1; OUT=50000; SEATS=5000; MAX=${MAX:-200000}; PM=${PM:-1000}
cd ~/hydo/hv/hvx/race; B=~/hydo/build
CL="console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys hydo_race=$W,$OUT,$SEATS"
one() { ~/bin/hvx-run sudo env HVX_DEV=/dev/hvxtj HVX_PMU_CLOCK=1 HVX_TJ_SEED=$1 HVX_TJ_MAX=$MAX HVX_TJ_PERMILLE=$PM ./hvx_race_probe $B/bzImage $B/initramfs-race.cpio.gz "$CL" 2>/dev/null; }
for s in $(seq 1 200); do
  r=$(one $s)
  lost=$(echo "$r" | grep -o 'lost=[-0-9]*' | cut -d= -f2)
  fp=$(echo "$r" | grep -o 'fp=[0-9a-f]*')
  rep=""
  if [ -n "$lost" ] && [ "$lost" != 0 ]; then
    r2=$(one $s); fp2=$(echo "$r2" | grep -o 'fp=[0-9a-f]*')
    [ "$fp" = "$fp2" ] && rep="replay=IDENTICAL" || rep="replay=MISMATCH($fp2)"
  fi
  echo "W=$W seed=$s lost=${lost:-ERR} $fp $rep"
done
