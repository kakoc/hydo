#!/bin/bash
# Stack A/B of the per-step cuts alone (run inside quiet.sh): the side module with ft_opt=0
# (old landing code) and ft_opt=7, margin 256, alternated, q=1000 after provisioning.
set -u
O=/var/tmp/fastturn
B=~/hydo/build
P=/sys/module/hvxft/parameters/ft_opt
CMD="root=/dev/vda1 ro rootfstype=ext4 console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys"
cd $O
for r in 1 2; do for o in 0 7; do
  echo $o | sudo tee $P > /dev/null
  /usr/bin/time -f "wall %e s" timeout 1800 ~/bin/hvx-run sudo env HVX_DEV=/dev/hvxft HVX_VCPUS=2 HVX_FTSTAT=1 HVX_QUANTUM=1000 \
    "HVX_QUANTUM_AFTER=hydo-stack: provisioning synced" HVX_SNAP_DIR=$O/snap \
    ./stackv $B/bzImage $B/initramfs-kafka.cpio.gz $B/stack-binlog-ret-disk.img "$CMD" 1024 sanity > stack-ab-$o-$r.txt 2>&1
  echo "opt=$o: $(grep -a 'FTSTAT wall\|fp=\|^wall\|cyc/step' stack-ab-$o-$r.txt | sed 's/.*client=//; s/turns=.*calls=[0-9]*//' | tr '\n' ' ')"
done; done
echo 7 | sudo tee $P > /dev/null
echo "== bursts on the default turn length (2^20): 10% of the windows hold a 20,000-branch burst of 100-branch turns"
for m in 256 160; do
  /usr/bin/time -f "wall %e s" timeout 1800 ~/bin/hvx-run sudo env HVX_DEV=/dev/hvxft HVX_VCPUS=2 HVX_FTSTAT=1 HVX_BURST=20000:100:100:5 HVX_TURN_MARGIN=$m \
    "HVX_QUANTUM_AFTER=hydo-stack: provisioning synced" HVX_SNAP_DIR=$O/snap \
    ./stackv $B/bzImage $B/initramfs-kafka.cpio.gz $B/stack-binlog-ret-disk.img "$CMD" 1024 sanity > stack-burst-$m.txt 2>&1
  echo "burst margin=$m: $(grep -a 'FTSTAT\|fp=\|^wall' stack-burst-$m.txt | sed 's/.*client=//' | tr '\n' ' ')"
done
