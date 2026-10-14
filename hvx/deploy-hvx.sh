#!/usr/bin/env bash
#
# Copy the hvx module + userspace test into the running L1 sandbox, build both
# against L1's kernel headers, free VMX-root (rmmod kvm_intel), load the module,
# run the userspace driver against /dev/hvx, then unload. Run this ON THE HOST
# after run-l1.sh reports "L1 up" and after the tree has been synced to ~/hydo
# (via ./dev.sh from the build machine).
#
# Usage:
#   ./deploy-hvx.sh          # copy + build only
#   ./deploy-hvx.sh load     # copy + build + rmmod kvm_intel + insmod + run test + dmesg
#
set -euo pipefail

SRC="${HYDO_HVX_SRC:-$HOME/hydo/hvx}"
K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }

echo ">> copying $SRC -> L1:/tmp/hvx-src"
L1SSH 'rm -rf /tmp/hvx-src && mkdir -p /tmp/hvx-src'
scp -P 2222 $K \
  "$SRC/hvx.c" "$SRC/hvx_abi.h" "$SRC/hvx_test.c" "$SRC/hvx_rdtsc_test.c" \
  "$SRC/hvx_step_test.c" "$SRC/hvx_tscclock_test.c" "$SRC/hvx_snapshot_test.c" \
  "$SRC/hvx_intr_test.c" "$SRC/hvx_verify.c" "$SRC/hvx_console_test.c" \
  "$SRC/hvx_bigmem_test.c" "$SRC/hvx_scale_test.c" "$SRC/hvx_cpuid_test.c" "$SRC/hvx_sregs_test.c" \
  "$SRC/hvx_input_test.c" "$SRC/hvx_smp_test.c" \
  "$SRC/hvx_virtio_iso_test.c" "$SRC/hvx_virtio_pci_test.c" \
  "$SRC/hvx_fault_test.c" "$SRC/hvx_net_test.c" "$SRC/hvx_net_fault_test.c" \
  "$SRC/hvx_bug_f0_test.c" "$SRC/hvx_bug_verify.c" "$SRC/hydokv.c" \
  "$SRC/hvx_fuzzresume_test.c" "$SRC/hvx_pmu_test.c" \
  "$SRC/hvx_sqlite_verify.c" "$SRC/hvx_redis_verify.c" "$SRC/hvx_rootfs_verify.c" \
  "$SRC/hvx_system_verify.c" "$SRC/hvx_jvm_verify.c" "$SRC/hvx_kafka_verify.c" \
  "$SRC/hvx_spread_verify.c" "$SRC/hvx_spread_search.c" \
  "$SRC/hvx_linux.c" "$SRC/hvx_boot.h" "$SRC/hvx_boot_verify.c" \
  "$SRC/hvx_disk_boot_verify.c" "$SRC/hvx_net_boot_verify.c" \
  "$SRC/Makefile" \
  dev@localhost:/tmp/hvx-src/ >/dev/null

echo ">> building module + tests in L1"
L1SSH 'cd /tmp/hvx-src && make && \
       cc -O2 -Wall -o hvx_test hvx_test.c && \
       cc -O2 -Wall -o hvx_rdtsc_test hvx_rdtsc_test.c && \
       cc -O2 -Wall -o hvx_step_test hvx_step_test.c && \
       cc -O2 -Wall -o hvx_tscclock_test hvx_tscclock_test.c && \
       cc -O2 -Wall -o hvx_snapshot_test hvx_snapshot_test.c && \
       cc -O2 -Wall -o hvx_intr_test hvx_intr_test.c && \
       cc -O2 -Wall -o hvx_verify hvx_verify.c && \
       cc -O2 -Wall -o hvx_console_test hvx_console_test.c && \
       cc -O2 -Wall -o hvx_bigmem_test hvx_bigmem_test.c && \
       cc -O2 -Wall -o hvx_scale_test hvx_scale_test.c && \
       cc -O2 -Wall -o hvx_cpuid_test hvx_cpuid_test.c && \
       cc -O2 -Wall -o hvx_sregs_test hvx_sregs_test.c && \
       cc -O2 -Wall -o hvx_input_test hvx_input_test.c && \
       cc -O2 -Wall -o hvx_smp_test hvx_smp_test.c && \
       cc -O2 -Wall -o hvx_virtio_iso_test hvx_virtio_iso_test.c && \
       cc -O2 -Wall -o hvx_virtio_pci_test hvx_virtio_pci_test.c && \
       cc -O2 -Wall -o hvx_fault_test hvx_fault_test.c && \
       cc -O2 -Wall -o hvx_net_test hvx_net_test.c && \
       cc -O2 -Wall -o hvx_net_fault_test hvx_net_fault_test.c && \
       cc -O2 -Wall -o hvx_bug_f0_test hvx_bug_f0_test.c && \
       cc -O2 -Wall -o hvx_fuzzresume_test hvx_fuzzresume_test.c && \
       cc -O2 -Wall -o hvx_pmu_test hvx_pmu_test.c && \
       cc -O2 -Wall -o hvx_bug_verify hvx_bug_verify.c && \
       cc -O2 -Wall -o hvx_sqlite_verify hvx_sqlite_verify.c && \
       cc -O2 -Wall -o hvx_redis_verify hvx_redis_verify.c && \
       cc -O2 -Wall -o hvx_rootfs_verify hvx_rootfs_verify.c && \
       cc -O2 -Wall -o hvx_system_verify hvx_system_verify.c && \
       cc -O2 -Wall -o hvx_jvm_verify hvx_jvm_verify.c && \
       cc -O2 -Wall -o hvx_kafka_verify hvx_kafka_verify.c && \
       cc -O2 -Wall -o hvx_spread_verify hvx_spread_verify.c && \
       cc -O2 -Wall -o hvx_spread_search hvx_spread_search.c && \
       cc -O2 -Wall -o hvx_linux hvx_linux.c && \
       cc -O2 -Wall -o hvx_boot_verify hvx_boot_verify.c && \
       cc -O2 -Wall -o hvx_disk_boot_verify hvx_disk_boot_verify.c && \
       cc -O2 -Wall -o hvx_net_boot_verify hvx_net_boot_verify.c'

if [ "${1:-}" = "load" ]; then
  echo ">> freeing VMX-root (rmmod kvm_intel), loading hvx, running the tests"
  L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
         sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
         sudo dmesg -C; \
         sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"; \
         sudo /tmp/hvx-src/hvx_test; echo "hvx_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_rdtsc_test; echo "hvx_rdtsc_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_step_test; echo "hvx_step_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_tscclock_test; echo "hvx_tscclock_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_snapshot_test; echo "hvx_snapshot_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_intr_test; echo "hvx_intr_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_verify; echo "hvx_verify rc=$?"; \
         sudo /tmp/hvx-src/hvx_console_test; echo "hvx_console_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_bigmem_test; echo "hvx_bigmem_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_scale_test; echo "hvx_scale_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_cpuid_test; echo "hvx_cpuid_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_sregs_test; echo "hvx_sregs_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_input_test; echo "hvx_input_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_smp_test; echo "hvx_smp_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_virtio_iso_test; echo "hvx_virtio_iso_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_virtio_pci_test; echo "hvx_virtio_pci_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_fault_test; echo "hvx_fault_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_net_test; echo "hvx_net_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_net_fault_test; echo "hvx_net_fault_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_bug_f0_test; echo "hvx_bug_f0_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_fuzzresume_test; echo "hvx_fuzzresume_test rc=$?"; \
         sudo /tmp/hvx-src/hvx_pmu_test; echo "hvx_pmu_test rc=$?"; \
         echo "---- dmesg (tail) ----"; sudo dmesg | tail -40; \
         sudo rmmod hvx; echo "rmmod rc=$?"'
fi
