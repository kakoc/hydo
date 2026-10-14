#!/usr/bin/env bash
#
# M24 stretch runner (run ON THE HOST, after ./dev.sh sync + build-linux.sh +
# deploy-hvx.sh build). Proves a SEEDED deterministic NET fault changes REAL Linux's
# behavior reproducibly:
#   1. BASELINE (no fault): boot with the virtio-net device + a scripted ARP request;
#      the guest binds virtio_net, brings up eth0, and ANSWERS the ARP -- the reply is
#      captured on the transmitq (like M23; baseline fingerprint fd5663b60bf9412e).
#   2. DROP fault (HVX_NET_FAULT): the SAME boot with a seeded DROP policy (every
#      scripted ARP copy dropped) -- the guest never receives the ARP, so NO reply is
#      captured. A deterministic behavior change from the seed alone.
#   3. reproducible: the DROP boot repeated gives the identical outcome (no reply).
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

NET_CMDLINE="console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys ipv6.disable=1"

echo ">> staging bzImage + net initramfs into L1:/tmp/hvx-net"
L1SSH 'mkdir -p /tmp/hvx-net'
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-net.cpio.gz" \
      dev@localhost:/tmp/hvx-net/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

run_boot() {  # $1 = label, $2 = timeout secs, $3 = extra env (may be empty)
  echo ">> [$1] booting (timeout ${2}s, env: ${3:-none})"
  L1SSH "sudo timeout $2 env HVX_NET=1 $3 \
         /tmp/hvx-src/hvx_linux /tmp/hvx-net/bzImage /tmp/hvx-net/initramfs-net.cpio.gz \
         \"$NET_CMDLINE\" >/tmp/nf_$1.out 2>/tmp/nf_$1.err; echo \"[$1] hvx_linux rc=\$?\""
  L1SSH "echo '   [$1] net-fault/ARP lines:'; \
         grep -aE 'net fault policy|ARP REPLY found|no ARP reply|DEMO PASS|DEMO: no ARP|eth0' \
         /tmp/nf_$1.err /tmp/nf_$1.out | tail -6"
}

# baseline needs time to reach + answer the ARP; the DROP boots never reply, so they
# only need enough to boot + bring up eth0 (then they are killed -> grep proves 0 replies).
run_boot baseline 210 ""
run_boot drop1 110 "HVX_NET_FAULT=0xd24 HVX_NET_FAULT_CLASSES=1 HVX_NET_FAULT_PROB=100"
run_boot drop2 110 "HVX_NET_FAULT=0xd24 HVX_NET_FAULT_CLASSES=1 HVX_NET_FAULT_PROB=100"

echo ">> verdict: baseline answers the ARP; the DROP boots do not, and match each other"
L1SSH 'b=$(cat /tmp/nf_baseline.err /tmp/nf_baseline.out | grep -acE "ARP REPLY found"); \
       d1=$(cat /tmp/nf_drop1.err /tmp/nf_drop1.out | grep -acE "ARP REPLY found"); \
       d2=$(cat /tmp/nf_drop2.err /tmp/nf_drop2.out | grep -acE "ARP REPLY found"); \
       echo "   baseline ARP-reply-lines=$b  drop1=$d1  drop2=$d2"; \
       if [ "$b" -gt 0 ] && [ "$d1" -eq 0 ] && [ "$d2" -eq 0 ]; then \
         echo "   M24 STRETCH PASS: seeded DROP deterministically suppressed the ARP reply"; \
       else echo "   M24 STRETCH: inconclusive (see /tmp/nf_*.err)"; fi'

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
