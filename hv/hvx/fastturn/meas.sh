#!/bin/bash
# The before/after timing set of hv/hvx/FASTTURN.md (run inside quiet.sh). On the box.
# "before" = the main module /dev/hvx (the old code) or the side module with ft_opt=0;
# "after" = the side module /dev/hvxft with ft_opt=7 and the given turn-end margin.
set -u
H=~/hydo-ft/hvx/fastturn
O=/var/tmp/fastturn
B=~/hydo/build
P=/sys/module/hvxft/parameters/ft_opt
CMD="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"
cd $O
echo "== ft_cost A/B (opt 0 = old landing code, 7 = new), margin 256"
for k in 0 1 2; do $H/ab.sh 2 1000 5000000 $k 0 7; done
echo "== ft_cost opt 7, margins 160 and 128"
for k in 0 1 2; do for m in 160 128; do echo "kind=$k margin=$m: $($H/ab.sh 1 1000 5000000 $k 7 7 $m | head -1)"; done; done
echo "== 2-vCPU Linux boot (hvx_smp_boot_verify HVX_MODE=one), main vs side"
for d in /dev/hvx /dev/hvxft /dev/hvx /dev/hvxft; do
  echo "$d: $(timeout 120 ~/bin/hvx-run sudo env HVX_DEV=$d HVX_MODE=one ./smpboot $B/bzImage $B/initramfs.cpio.gz 2 2>&1 | grep -a 'boot #1:.*wall\|fp=' | tr '\n' ' ')"
done
stack() {	# <tag> <dev> [env...]
  local tag=$1 dev=$2; shift 2
  /usr/bin/time -f "wall %e s" timeout 1800 ~/bin/hvx-run sudo env HVX_DEV=$dev HVX_VCPUS=2 HVX_FTSTAT=1 HVX_QUANTUM=1000 \
    "HVX_QUANTUM_AFTER=hydo-stack: provisioning synced" HVX_SNAP_DIR=$O/snap "$@" \
    ./stackv $B/bzImage $B/initramfs-kafka.cpio.gz $B/stack-binlog-ret-disk.img "$CMD" 1024 sanity > stack-$tag.txt 2>&1
  echo "stack $tag: $(grep -a 'FTSTAT wall\|SANITY  client\|^wall\|cyc/step' stack-$tag.txt | sed 's/.*client=//' | tr '\n' ' ')"
}
echo "== 2-vCPU stack sanity, q=1000 from 'provisioning synced' to the end"
echo 7 | sudo tee $P > /dev/null
stack before-main /dev/hvx
stack after-m256 /dev/hvxft
stack after-m160 /dev/hvxft HVX_TURN_MARGIN=160
stack before-main2 /dev/hvx
stack after-m160b /dev/hvxft HVX_TURN_MARGIN=160
echo 7 | sudo tee $P > /dev/null
