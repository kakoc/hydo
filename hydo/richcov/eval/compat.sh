#!/bin/bash
# tracing off on the richcov-old image (fingerprint must equal the traced runs), then the two
# pinned reproducers with this tree's hvx_explore and no new env (backward compatibility)
cd /var/tmp/m44-rich
B=$HOME/hydo/build; X=$HOME/hydo-rich/hvx/hvx_explore
mkdir -p compat/a compat/b compat/c
echo -e "RUN t0 disk=none dseed=1 | run\nQUIT" | ~/bin/hvx-run sudo env HVX_SNAP_DIR=/var/tmp/hvx-snap HVX_EXPLORE_CMDLINE_EXTRA=norandmaps $X serve $B/bzImage $B/initramfs-kafka.cpio.gz $B/richcov-old-disk.img compat/a 2>compat/a.err
for img in explore-disk.img explore-fix-disk.img; do
  d=compat/${img%%-disk.img}
  echo -e "RUN p disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24\nQUIT" | ~/bin/hvx-run sudo env HVX_SNAP_DIR=/var/tmp/hvx-snap $X serve $B/bzImage $B/initramfs-kafka.cpio.gz $B/$img $d 2>$d.err
done
