#!/usr/bin/env bash
#
# Bring up the L1 nested-virtualization sandbox on the KVM host (NixOS).
# Run this ON THE HOST (the build machine), NOT on your build machine and NOT inside L1.
#
# It is idempotent: it downloads the Ubuntu cloud image and realizes qemu/xorriso
# from nixpkgs on first run, then (re)launches a throwaway Ubuntu 24.04 guest
# ("L1") under the host's KVM with VMX exposed via nested virt. Re-run it to
# restart L1 after a guest panic; the host is never touched.
#
# After it prints "L1 up", ssh in with:
#   ssh -p 2222 -i ~/l1/id_l1 -o StrictHostKeyChecking=no \
#       -o UserKnownHostsFile=/dev/null dev@localhost
#
set -euo pipefail

WORK="$HOME/l1"
IMG_URL="https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"
BASE="$WORK/noble-server-cloudimg-amd64.img"
OVL="$WORK/l1.qcow2"
SEED="$WORK/seed.iso"
NIX="nix --extra-experimental-features nix-command --extra-experimental-features flakes"

mkdir -p "$WORK"
cd "$WORK"

echo ">> realizing qemu + xorriso from nixpkgs (cached after first run)"
QEMU_DIR=$($NIX build nixpkgs#qemu --no-link --print-out-paths | grep -m1 . )/bin
# nixpkgs#xorriso now yields several outputs (e.g. a separate -man); pick the
# store path that actually contains bin/xorriso rather than blindly appending.
XORRISO=$(for p in $($NIX build nixpkgs#xorriso --no-link --print-out-paths); do
            [ -x "$p/bin/xorriso" ] && { echo "$p/bin/xorriso"; break; }
          done)
QEMU="$QEMU_DIR/qemu-system-x86_64"
QIMG="$QEMU_DIR/qemu-img"

if [ ! -f "$BASE" ]; then
  echo ">> downloading Ubuntu 24.04 cloud image"
  curl -fL --retry 3 -o "$BASE.part" "$IMG_URL"
  mv "$BASE.part" "$BASE"
fi

if [ ! -f "$WORK/id_l1" ]; then
  ssh-keygen -t ed25519 -N "" -f "$WORK/id_l1" -C l1key
fi

if [ ! -f "$OVL" ]; then
  echo ">> creating 20G overlay backed by the pristine cloud image"
  "$QIMG" create -f qcow2 -F qcow2 -b "$BASE" "$OVL" 20G
fi

echo ">> building cloud-init NoCloud seed"
PUB=$(cat "$WORK/id_l1.pub")
# user-data.tmpl / meta-data are shipped in the repo under hvx/cloud-init/.
TMPL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/cloud-init"
sed "s|@PUBKEY@|$PUB|" "$TMPL_DIR/user-data.tmpl" > "$WORK/user-data"
cp "$TMPL_DIR/meta-data" "$WORK/meta-data"
"$XORRISO" -as mkisofs -o "$SEED" -V CIDATA -J -r "$WORK/user-data" "$WORK/meta-data" 2>/dev/null

# stop a prior L1 if running
if [ -f "$WORK/l1.pid" ] && kill -0 "$(cat "$WORK/l1.pid")" 2>/dev/null; then
  echo ">> stopping previous L1"
  kill "$(cat "$WORK/l1.pid")"; sleep 1
fi

echo ">> launching L1 (daemonized, serial -> $WORK/l1-serial.log)"
: > "$WORK/l1-serial.log"
# NOTE (milestone 17 — PMU fast instruction clock): `pmu=on` asks KVM to expose an
# architectural PerfMon (CPUID.0AH / IA32_FIXED_CTR0 = INST_RETIRED.ANY) to L1, which
# the guest-only PMU fast clock needs. This is NECESSARY BUT NOT SUFFICIENT on the
# current L0: KVM's guest-PMU virtualization is gated by the host module parameter
# /sys/module/kvm/parameters/enable_pmu, which reads `N` here and is read-only (0444),
# so KVM still zeroes CPUID.0AH and #GPs the PMU MSRs inside L1 (verified: hvx_pmu_probe
# reports "SUPPORTED: NO"). Flipping it needs L0 root (reload the kvm module with
# enable_pmu=1, or kvm.enable_pmu=1 on the cmdline + reboot) — which we do NOT have, and
# L0's kvm is shared with other jobs. `pmu=on` is harmless while it stays off (KVM
# ignores it) and becomes live automatically once the host enables the vPMU. See the
# "Milestone 17 — BLOCKED" section in CLAUDE.md.
"$QEMU" \
  -name l1-hvx \
  -enable-kvm -cpu host,+vmx,pmu=on -smp 2 -m 4096 \
  -machine pc \
  -drive file="$OVL",if=virtio,format=qcow2 \
  -drive file="$SEED",if=virtio,format=raw,readonly=on \
  -netdev user,id=n0,hostfwd=tcp::2222-:22 \
  -device virtio-net,netdev=n0 \
  -display none \
  -serial file:"$WORK/l1-serial.log" \
  -pidfile "$WORK/l1.pid" \
  -daemonize

echo ">> L1 launched, pid=$(cat "$WORK/l1.pid"). Waiting for SSH on :2222 ..."
for i in $(seq 1 60); do
  if ssh -p 2222 -i "$WORK/id_l1" -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o BatchMode=yes \
        dev@localhost true 2>/dev/null; then
    echo ">> L1 up. On first boot, install the build toolchain inside L1:"
    echo "     sudo apt-get update && sudo apt-get install -y build-essential linux-headers-\$(uname -r)"
    exit 0
  fi
  sleep 2
done
echo "!! L1 did not answer SSH in time; check $WORK/l1-serial.log"
exit 1
