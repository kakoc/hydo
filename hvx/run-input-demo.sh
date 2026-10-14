#!/usr/bin/env bash
#
# M18 end-to-end demo runner (run ON THE HOST, after ./dev.sh sync + a build).
# Stages the bzImage+initramfs into L1, loads hvx, boots real Linux with hvx_linux
# feeding a SCRIPTED command at the "~ #" prompt, and shows the shell consumed it.
# Also re-checks the M16 boot-determinism fingerprint. Scoped kills only; never
# touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

echo ">> staging bzImage + initramfs into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs.cpio.gz" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> M18 scripted-input demo: feed \"echo hydo-interactive\\n\" at the shell prompt"
L1SSH "sudo timeout 240 env HVX_INPUT='echo hydo-interactive\\n' HVX_EXPECT='hydo-interactive' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs.cpio.gz 2>/tmp/hvx_demo.err; \
       echo \"hvx_linux rc=\$?\"; \
       echo '==== tail of console (stdout) already above; key stderr lines: ===='; \
       grep -aE 'fed .*scripted|DEMO PASS|DEMO:|hydo-interactive|~ #|launching shell' /tmp/hvx_demo.err | tail -20"

echo ">> grep the captured console for the fed command + its output"
L1SSH "grep -aE 'hydo-interactive' /tmp/hvx_demo.err | tail -5 || true"

echo ">> M16 regression: whole-boot determinism fingerprint (expect 640ece2f900fe5b3)"
# NOTE: the reproducible-initramfs baseline is 640ece2f900fe5b3 (build-linux.sh is now
# byte-reproducible; the pre-M21 value 1bdf266a0db52bf7 was tied to a one-off, non-
# reproducible initramfs blob -- see the M21 CLAUDE.md section). The device stays inert
# with no disk, so this value is unchanged by the M21 MSI-X/virtio additions.
L1SSH 'sudo /tmp/hvx-src/hvx_boot_verify /tmp/bzImage /tmp/initramfs.cpio.gz 2>&1 | \
       grep -aiE "identical|PASS|FAIL|640ece2f900fe5b3|mismatch" | tail -20'

echo ">> dmesg tail + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 8 ----"; sudo dmesg | tail -8; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
