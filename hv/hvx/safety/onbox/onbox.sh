#!/usr/bin/env bash
# The hvx safety net on the bare-metal machine itself (Milestone 140): a debug kernel built
# from the bare-metal machine's own running config, booted ONCE with kexec, so hvx.c runs under KASAN, kmemleak,
# UBSAN and lockdep on the real hardware (the PMU clock, several vCPUs and Intel PT included,
# which the nested VM of SAFETY.md cannot reach). Run ON THE BARE-METAL MACHINE, from a tree in your own
# directory. Booting the bare-metal machine into it needs the user's approval every time.
#
#   onbox.sh build          unpack the pinned 6.8.12 tarball, config, make bzImage modules (nice 19)
#   onbox.sh install        modules into /lib/modules/<rel>, bzImage and initramfs into $DBG/boot,
#                           then check the initramfs has md raid1, mdadm, ahci and igb
#   onbox.sh grub           GRUB_DEFAULT=saved and saved_entry = the 6.8.0-146 entry (prints checks)
#   onbox.sh load           kexec -l the debug kernel (nothing reboots yet)
#   onbox.sh go             checks, then the exclusive module lock, then systemctl kexec
#   onbox.sh module         (under the debug kernel) build $DBG/hv/hvx/hvx.ko for it and load it as hvx
#   onbox.sh scan [tag]     kmemleak scan (twice) and every KASAN/UBSAN/lockdep/kmemleak/BUG line of the boot's kernel journal
#
# Why kexec and not grub-reboot: /boot is on md RAID (md2), and GRUB cannot write its
# environment block there ("GRUB lacks write support for diskfilter"), so the `save_env
# next_entry` that makes a grub-reboot entry one-time fails at boot and the entry would be
# booted again on every reboot. A kexec'ed kernel is never in GRUB: any reboot (a plain one, a
# panic with panic=30, or a power cycle after a hang) goes through the firmware and GRUB's
# saved default. The debug kernel is not in /boot, so update-grub never lists it.
set -euo pipefail
DBG=${DBG:-$HOME/hydo-dbg}
HERE="$(cd "$(dirname "$0")" && pwd)"
KVER=6.8.12
KSHA=19b31956d229b5b9ca5671fa1c74320179682a3d8d00fc86794114b21da86039
TAR=${TAR:-$HOME/hydo-safety/dl/linux-$KVER.tar.xz}
BASE=${BASE:-6.8.0-146-generic}		# the normal kernel: its config, its GRUB entry
REL=$KVER-hydodbg
K=$DBG/kbuild/linux
JOBS=${JOBS:-6}
LOCK=$HOME/.hvx-module.lock
PENDING=$HOME/m140-reboot-pending
EXTRA="panic=30 softlockup_panic=1 kasan_multi_shot"	# a panic or a soft lockup reboots to GRUB's default

cmd=${1:-}
case $cmd in
build)
  mkdir -p "$DBG/kbuild"
  echo "$KSHA  $TAR" | sha256sum -c --quiet
  if [ ! -f "$K/.hydo-unpacked-$KVER" ]; then
    rm -rf "$K"; mkdir -p "$K"; tar -xJf "$TAR" -C "$K" --strip-components=1; touch "$K/.hydo-unpacked-$KVER"
  fi
  cd "$K"
  cp "/boot/config-$BASE" .config
  lsmod > "$DBG/kbuild/lsmod.txt"
  make -s olddefconfig
  (yes '' || true) | make -s LSMOD="$DBG/kbuild/lsmod.txt" localmodconfig >/dev/null
  scripts/kconfig/merge_config.sh -m -O . .config "$HERE/debug.config" >/dev/null
  make -s olddefconfig
  bad=0
  while IFS= read -r l; do
    case "$l" in CONFIG_*=n) o=${l%%=*}; grep -q "^$o=" .config && { echo "not off: $o"; bad=1; } ;;
                 CONFIG_*) grep -qxF "$l" .config || { echo "not set: $l"; bad=1; } ;; esac
  done < "$HERE/debug.config"
  for m in CONFIG_MD_RAID1 CONFIG_BLK_DEV_MD CONFIG_SATA_AHCI CONFIG_IGB CONFIG_EXT4_FS; do
    grep -qE "^$m=(y|m)" .config || { echo "driver missing: $m"; bad=1; }; done
  [ $bad = 0 ] || { echo "onbox: config not as asked" >&2; exit 1; }
  export KBUILD_BUILD_TIMESTAMP='Thu Jan  1 00:00:00 UTC 2026' KBUILD_BUILD_USER=hydo \
         KBUILD_BUILD_HOST=hydo-dbg KBUILD_BUILD_VERSION=1 SOURCE_DATE_EPOCH=1767225600
  nice -n 19 make -j"$JOBS" bzImage modules > "$DBG/kbuild/make.log" 2>&1 || { tail -30 "$DBG/kbuild/make.log"; exit 1; }
  echo "onbox: built $(make -s kernelrelease), bzImage $(sha256sum arch/x86/boot/bzImage | cut -c1-16)…, $(grep -c '=m$' .config) modules";;
install)
  cd "$K"
  [ "$(make -s kernelrelease)" = "$REL" ] || { echo "onbox: tree is not $REL"; exit 1; }
  sudo make -s INSTALL_MOD_STRIP=1 modules_install
  mkdir -p "$DBG/boot"
  cp arch/x86/boot/bzImage "$DBG/boot/vmlinuz-$REL"; cp .config "$DBG/boot/config-$REL"; cp System.map "$DBG/boot/System.map-$REL"
  sudo mkinitramfs -o "$DBG/boot/initrd.img-$REL" "$REL"
  sudo chown "$(id -u):$(id -g)" "$DBG/boot/initrd.img-$REL"
  l=$(lsinitramfs "$DBG/boot/initrd.img-$REL")
  bad=0
  for w in raid1 md-mod igb ahci "sbin/mdadm" "mdadm.conf" ext4; do
    if grep -q "$w" <<< "$l"; then echo "  initramfs has $w"; else
      # built in is as good as a module
      case $w in raid1) c=CONFIG_MD_RAID1;; md-mod) c=CONFIG_BLK_DEV_MD;; igb) c=CONFIG_IGB;; ahci) c=CONFIG_SATA_AHCI;; ext4) c=CONFIG_EXT4_FS;; *) c=;; esac
      if [ -n "$c" ] && grep -q "^$c=y" .config; then echo "  $w is built in"; else echo "  MISSING $w"; bad=1; fi
    fi; done
  [ $bad = 0 ] || exit 1
  ls -la "$DBG/boot";;
grub)
  sudo cp -n /etc/default/grub "$DBG/grub.default.orig"
  uuid=$(sudo grub-probe --target=fs_uuid /)
  id="gnulinux-advanced-$uuid>gnulinux-$BASE-advanced-$uuid"
  sudo grep -q "'gnulinux-$BASE-advanced-$uuid'" /boot/grub/grub.cfg || { echo "onbox: no GRUB entry for $BASE"; exit 1; }
  if ! grep -qx 'GRUB_DEFAULT=saved' /etc/default/grub; then
    sudo sed -i 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT=saved/' /etc/default/grub
    sudo update-grub 2>&1 | tail -3
  fi
  sudo grub-set-default "$id"
  echo "--- /etc/default/grub:"; grep '^GRUB_DEFAULT' /etc/default/grub
  echo "--- grub-editenv list:"; sudo grub-editenv list
  echo "--- grub.cfg default selection:"; sudo grep -n 'set default=' /boot/grub/grub.cfg
  echo "--- grub.cfg entry the saved default names:"; sudo grep -n "'gnulinux-$BASE-advanced-$uuid'" /boot/grub/grub.cfg | cut -c1-140
  echo "--- grub.cfg entries for $REL (must be none):"; sudo grep -c "$REL" /boot/grub/grub.cfg || true;;
load)
  [ -f "$DBG/boot/vmlinuz-$REL" ] || { echo "onbox: not installed"; exit 1; }
  cl="$(sed 's/^BOOT_IMAGE=[^ ]* //' /proc/cmdline) $EXTRA"
  sudo kexec -l "$DBG/boot/vmlinuz-$REL" --initrd="$DBG/boot/initrd.img-$REL" --command-line="$cl"
  echo "kexec_loaded=$(cat /sys/kernel/kexec_loaded) cmdline: $cl";;
go)
  [ -f "$PENDING" ] || { echo "onbox: write $PENDING first and give the other sessions 15 minutes"; exit 1; }
  age=$(( $(date +%s) - $(stat -c %Y "$PENDING") ))
  [ "$age" -ge 900 ] || [ "${FORCE:-}" = 1 ] || { echo "onbox: $PENDING is only ${age}s old (15 min wanted)"; exit 1; }
  [ "$(cat /sys/kernel/kexec_loaded)" = 1 ] || { echo "onbox: nothing loaded (onbox.sh load)"; exit 1; }
  echo "taking the exclusive module lock (up to 30 min)…"
  exec flock -x -w 1800 "$LOCK" bash -c '
    if sudo fuser /dev/hvx* 2>/dev/null | grep -q "[0-9]"; then echo "onbox: /dev/hvx is open:"; sudo fuser -v /dev/hvx* ; exit 1; fi
    ps -eo pid,user,etime,args | grep -E "hydo|hvx" | grep -vE "grep|onbox.sh|flock -x" || true
    [ "${GO:-}" = 1 ] || { echo "onbox: lock held, /dev/hvx closed; set GO=1 to kexec now"; exit 0; }
    sync; sudo systemctl kexec'
  ;;
module)
  [ "$(uname -r)" = "$REL" ] || { echo "onbox: not running $REL"; exit 1; }
  cd "$DBG/hv/hvx"
  make 2>&1 | grep -E " error |error:|warning:" || true
  echo "built: srcversion $(modinfo -F srcversion hvx.ko), vermagic $(modinfo -F vermagic hvx.ko)"
  flock -x -w 1800 "$LOCK" bash -c 'sudo rmmod hvx 2>/dev/null; sudo insmod hvx.ko && echo LOADED $(cat /sys/module/hvx/srcversion)';;
scan)
  tag=${2:-scan}
  out=/var/tmp/m140a/dmesg-$tag.txt; mkdir -p /var/tmp/m140a
  if [ -e /sys/kernel/debug/kmemleak ] || sudo test -e /sys/kernel/debug/kmemleak; then
    echo scan | sudo tee /sys/kernel/debug/kmemleak >/dev/null; sleep 6
    echo scan | sudo tee /sys/kernel/debug/kmemleak >/dev/null
    sudo cat /sys/kernel/debug/kmemleak > /var/tmp/m140a/kmemleak-$tag.txt
    echo "kmemleak: $(grep -c '^unreferenced object' /var/tmp/m140a/kmemleak-$tag.txt) unreferenced objects ($(grep -c hvx /var/tmp/m140a/kmemleak-$tag.txt) lines naming hvx)"
  fi
  # the whole boot's kernel log: hvx's exit histograms rotate the dmesg ring buffer within minutes
  sudo journalctl -k -b --no-pager > "$out"
  grep -aE 'BUG:|KASAN|UBSAN|WARNING:|possible (circular|recursive)|inconsistent lock|lockdep|INFO: task|soft lockup|rcu.*stall|Oops|general protection|kmemleak:|sleeping function|scheduling while atomic|DEBUG_LOCKS_WARN|list_(add|del) corruption' "$out" \
    | grep -vE 'kasan_multi_shot|RCU lockdep checking is enabled|kmemleak: Kernel memory leak detector initialized|KernelAddressSanitizer initialized' \
    > "/var/tmp/m140a/splats-$tag.txt" || true
  dl=$(sudo awk '/debug_locks/{print ($2 == 1 ? "armed" : "OFF (it reported)")}' /proc/lockdep_stats)
  echo "kernel log: $(wc -l < "$out") lines, lockdep $dl, $(wc -l < "/var/tmp/m140a/splats-$tag.txt") report lines -> /var/tmp/m140a/splats-$tag.txt"
  head -40 "/var/tmp/m140a/splats-$tag.txt";;
*) sed -n '2,20p' "$0"; exit 2;;
esac
