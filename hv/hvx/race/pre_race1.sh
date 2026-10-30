#!/bin/bash
# one race boot: race1.sh W ARM SEED   (ARM: base | tj | pp | ppx | xt)
W=$1; ARM=$2; S=$3; OUT=50000; SEATS=5000
cd ~/hydo-preempt/hvx/race; B=~/hydo/build
CL="console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys hydo_race=$W,$OUT,$SEATS"
case $ARM in
  base) E="";;
  tj)   E="HVX_TJ_SEED=$S HVX_TJ_MAX=200000 HVX_TJ_PERMILLE=1000";;
  pp)   E="HVX_PMU_PERIOD=65536 HVX_TJ_SEED=$S HVX_TJ_MAX=60000 HVX_TJ_PERMILLE=1000";;
  pp14) E="HVX_PMU_PERIOD=16384 HVX_TJ_SEED=$S HVX_TJ_MAX=15000 HVX_TJ_PERMILLE=1000";;
  xt)   E="HVX_XT_SEED=$S HVX_XT_RATE=16 HVX_XT_PERMILLE=500";;
  xt4)  E="HVX_XT_SEED=$S HVX_XT_RATE=4 HVX_XT_PERMILLE=500";;
esac
t0=$(date +%s.%N)
r=$(~/bin/hvx-run sudo env HVX_DEV=/dev/hvxpre HVX_PMU_CLOCK=1 $E ./hvx_race_probe $B/bzImage $B/initramfs-race.cpio.gz "$CL" 2>/dev/null)
t1=$(date +%s.%N)
echo "W=$W arm=$ARM seed=$S secs=$(echo "$t1 - $t0" | bc) $r"
