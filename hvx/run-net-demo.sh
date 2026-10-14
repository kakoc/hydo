#!/usr/bin/env bash
#
# M23 stretch runner (run ON THE HOST, after ./dev.sh sync + build-linux.sh +
# deploy-hvx.sh build). Proves REAL Linux uses the virtio-net device:
#   1. boots the NixOS 6.6.35 bzImage + a net-enabled initramfs with the virtio-net
#      PCI function attached; Linux binds virtio_net, creates eth0, and brings it up
#      with a static IP (10.0.0.2);
#   2. the loader pushes a scripted ARP request (who-has 10.0.0.2) via HVX_PUT_RX;
#      the guest replies and the reply is captured on the transmitq (HVX_GET_TX);
#   3. runs the with-net determinism harness (two boots bit-identical).
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

echo ">> staging bzImage + net initramfs into L1:/tmp/hvx-net (a dir we own)"
L1SSH 'mkdir -p /tmp/hvx-net'
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-net.cpio.gz" \
      dev@localhost:/tmp/hvx-net/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

NET_CMDLINE="console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys ipv6.disable=1"
echo ">> M23 real-Linux net boot: attach virtio-net, push an ARP request, capture the reply"
L1SSH "sudo timeout 220 env HVX_NET=1 \
       /tmp/hvx-src/hvx_linux /tmp/hvx-net/bzImage /tmp/hvx-net/initramfs-net.cpio.gz \
       \"$NET_CMDLINE\" >/tmp/hvx_net.out 2>/tmp/hvx_net.err; echo \"hvx_linux rc=\$?\""
echo ">> proof: guest bound virtio_net, brought up eth0, and replied to the ARP"
L1SSH "grep -aE '1af4:1041|virtio_net|hydo-net: eth0|ARP REPLY|DEMO PASS|captured' /tmp/hvx_net.err /tmp/hvx_net.out | tail -12"

echo ">> M23 with-net determinism (two boots identical)"
L1SSH "sudo timeout 480 /tmp/hvx-src/hvx_net_boot_verify /tmp/hvx-net/bzImage \
       /tmp/hvx-net/initramfs-net.cpio.gz 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|ARP' | tail -8"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
