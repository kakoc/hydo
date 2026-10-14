#!/usr/bin/env bash
#
# M26 FLAGSHIP-ON-UNMODIFIED-SQLITE runner (run ON THE HOST, after ./dev.sh sync +
# build-linux.sh + deploy-hvx.sh build). Unlike M25's purpose-built hydokv, the SUT
# here is an UNMODIFIED, off-the-shelf static `sqlite3` CLI running on a REAL ext2
# filesystem on /dev/vda.
#
#   1. boot real Linux; /init mounts the ext2 fs, runs committed sqlite3 INSERTs
#      (synchronous=FULL), drops the page cache, and an oracle re-reads every
#      committed row. NO fault -> HYDO-ORACLE: OK (the DB is durable + correct);
#   2. boot again under a SEEDED virtio-blk DROP fault (a "LYING DISK" that acks a
#      write -- even one sqlite fsync'd -- but silently fails to persist it) ->
#      HYDO-ORACLE: VIOLATION (a genuinely-committed row is LOST);
#   3. run hvx_sqlite_verify: the four hydo-verify checks on this real bug --
#      F1 no-fault OK + deterministic (2 boots + branch identical),
#      F2 seeded DROP VIOLATION + replay + branch bit-for-bit,
#      F3 a different seed => a different reproducible failure,
#      CONTRAST a FLUSH-AWARE fault (faithful power-loss) does NOT lose the fsync'd
#      row -- isolating that F2's loss is the disk violating the FLUSH contract;
#   4. regression: the FAULT-FREE with-disk baseline (36583d533d7c989b) is preserved
#      (the SQL VM only attaches a bigger disk + a fault policy; both inert-by-default).
# Scoped kills only; never touches the host or unrelated qemu.
set -uo pipefail

K="-i $HOME/l1/id_l1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes"
L1SSH() { ssh -p 2222 $K dev@localhost "$@"; }
L1SCP() { scp -P 2222 $K "$@"; }

DISK_FP="36583d533d7c989b"      # fault-free with-disk baseline (must be preserved)
DROP=4                          # HVX_FAULT_DROP bit
DROP_UNFLUSHED=32               # HVX_FAULT_DROP_UNFLUSHED bit (1<<5)
SEED="0x5017E0"                 # matches hvx_sqlite_verify SEED_A
PROB=5                          # matches hvx_sqlite_verify FAULT_PROB

echo ">> staging bzImage + SQL initramfs + reproducible ext2 disk into L1:/tmp"
L1SCP "$HOME/hydo/build/bzImage" "$HOME/hydo/build/initramfs-sql.cpio.gz" \
      "$HOME/hydo/build/sql-disk.img" dev@localhost:/tmp/ >/dev/null

echo ">> (re)loading hvx module in L1"
L1SSH 'lsmod | grep -q hvx && sudo rmmod hvx; \
       sudo rmmod kvm_intel 2>/dev/null; sudo rmmod kvm 2>/dev/null; \
       sudo dmesg -C; \
       sudo insmod /tmp/hvx-src/hvx.ko; echo "insmod rc=$?"'

echo ">> 1. NO FAULT: boot real Linux running unmodified sqlite3 -- expect HYDO-ORACLE: OK"
L1SSH "sudo timeout 400 env HVX_DISK=/tmp/sql-disk.img \
       HVX_INPUT='echo hydo-sql-shell\n' HVX_EXPECT='hydo-sqlite-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-sql.cpio.gz \
       >/tmp/hvx_sql_ok.out 2>/tmp/hvx_sql_ok.err; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydo-sql:|HYDO-ORACLE:|hydo-sqlite-marker' /tmp/hvx_sql_ok.out | tail -10"

echo ">> 2. SEEDED LYING-DISK DROP (seed $SEED prob $PROB%): expect HYDO-ORACLE: VIOLATION (a committed row lost)"
L1SSH "sudo timeout 400 env HVX_DISK=/tmp/sql-disk.img HVX_FAULT=$SEED \
       HVX_FAULT_CLASSES=$DROP HVX_FAULT_PROB=$PROB \
       HVX_INPUT='echo hydo-sql-shell\n' HVX_EXPECT='hydo-sqlite-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-sql.cpio.gz \
       >/tmp/hvx_sql_bug.out 2>/tmp/hvx_sql_bug.err; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydo-sql:|HYDO-ORACLE:|hydo-sqlite-marker|fault policy' /tmp/hvx_sql_bug.out /tmp/hvx_sql_bug.err | tail -12"

echo ">> 2b. CONTRAST -- FLUSH-AWARE DROP (faithful power-loss, seed $SEED): expect HYDO-ORACLE: OK (fsync'd row survives)"
L1SSH "sudo timeout 400 env HVX_DISK=/tmp/sql-disk.img HVX_FAULT=$SEED \
       HVX_FAULT_CLASSES=$DROP_UNFLUSHED HVX_FAULT_PROB=$PROB \
       HVX_INPUT='echo hydo-sql-shell\n' HVX_EXPECT='hydo-sqlite-marker' \
       /tmp/hvx-src/hvx_linux /tmp/bzImage /tmp/initramfs-sql.cpio.gz \
       >/tmp/hvx_sql_flush.out 2>/tmp/hvx_sql_flush.err; echo \"hvx_linux rc=\$?\""
L1SSH "grep -aE 'hydo-sql:|HYDO-ORACLE:|hydo-sqlite-marker' /tmp/hvx_sql_flush.out | tail -8"

echo ">> 3. hvx_sqlite_verify: the four checks (F1 OK+deterministic, F2 VIOLATION+replay+branch, F3 seed-sensitive, CONTRAST flush-aware survives)"
L1SSH "sudo timeout 3000 /tmp/hvx-src/hvx_sqlite_verify /tmp/bzImage \
       /tmp/initramfs-sql.cpio.gz /tmp/sql-disk.img 2>&1 | \
       grep -aiE 'F1 |F2 |F3 |CONTRAST|verdict=|PASS|FAIL|MISMATCH' | tail -24"

echo ">> 4. regression: FAULT-FREE with-disk baseline preserved (expect $DISK_FP)"
L1SSH "scp -P 2222 $K $HOME/hydo/build/vblk-disk.img dev@localhost:/tmp/ >/dev/null 2>&1 || true; \
       scp -P 2222 $K $HOME/hydo/build/initramfs-disk.cpio.gz dev@localhost:/tmp/ >/dev/null 2>&1 || true"
L1SSH "sudo timeout 480 /tmp/hvx-src/hvx_disk_boot_verify /tmp/bzImage \
       /tmp/initramfs-disk.cpio.gz /tmp/vblk-disk.img 2>&1 | \
       grep -aiE 'whole-boot|branch |PASS|FAIL|MISMATCH|$DISK_FP' | tail -6"

echo ">> dmesg health + unload"
L1SSH 'sudo dmesg | grep -iE "bug|taint|warn|rcu|stall|oops" | tail -20; \
       echo "---- dmesg last 6 ----"; sudo dmesg | tail -6; \
       sudo rmmod hvx; echo "rmmod rc=$?"'
