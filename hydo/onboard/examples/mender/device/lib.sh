# Shared by the device's scripts (busybox sh). The device has one eMMC, laid out as Mender's
# U-Boot integration lays out its storage device (RESULTS.md, "The device"):
#   0x400000, 0x800000   U-Boot's redundant environment, two 128 KiB copies
#   16 MiB  (32 MiB)     root file system A, Mender's partition 2
#   48 MiB  (32 MiB)     root file system B, Mender's partition 3
#   80 MiB  (48 MiB)     the data partition (ext4): /data/mender (Mender's database, its
#                        configuration, the Update Module's work files), /data/app (records)
# Linux sees the eMMC as /dev/loop0 and its partitions A, B and data as /dev/loop2, /dev/loop3
# and /dev/loop1: loop devices with direct I/O, so that every write reaches the eMMC and every
# flush of a partition flushes the eMMC. The eMMC is (/etc/ota/storage):
#   devices (hydo-devices.yaml): /dev/vdb, a disk of its own with a volatile write cache
#                                (guest.disks; the Mac and KVM engines)
#   loop (hydo.yaml):            /data/flash/emmc.img on the guest's data disk (hvx)
# The booted slot is mounted read-only at /sysroot with the data partition at /sysroot/data,
# and the device's programs (mender-update and its Update Module) run in a chroot there, so the
# root file system Mender sees is the slot that U-Boot booted.
#   /tmp/ota/            what lives in RAM: lost at every boot
OTA=/opt/ota
ROOT=/sysroot
RUN=/tmp/ota
ARCH=$(uname -m)
IMAGE_BYTES=25165824   # the slot images: 24 MiB ext4
MAX_RESETS=12          # boots in a row without a slot that verifies: the device is bricked
EMMC_MIB=128

say() {   # a line of the device's log, also on the console (the run's boot<n>.log files)
  l="ota: [$(cut -d" " -f1 /proc/uptime)] $*"
  echo "$l"
  c=/dev/ttyS0; [ -c $c ] || c=/dev/console
  echo "$l" > $c
}

# an assertion record on the console: the event a reaction waits for (A=<id>@<step>+<ms>:cut
# cuts the power <ms> of guest time after the first such record from step <step> on)
event() {
  c=/dev/ttyS0; [ -c $c ] || c=/dev/console
  echo "HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"$1\",\"ok\":true,\"msg\":\"$2\",\"details\":{}}" > $c
}

STORAGE=$(cat /etc/ota/storage 2>/dev/null || echo loop)
# the Artifacts the operator installs: with state scripts (scripts, the default), or the same
# ones without (plain: v<N>-plain.mender)
ARTIFACT_SUFFIX=; [ "$(cat /etc/ota/artifacts 2>/dev/null)" = plain ] && ARTIFACT_SUFFIX=-plain
EMMC=/data/flash/emmc.img; [ "$STORAGE" = devices ] && EMMC=/dev/vdb
part_dev() { case $1 in 2) echo /dev/loop2 ;; 3) echo /dev/loop3 ;; esac; }
part_off() { case $1 in 2) echo 16777216 ;; 3) echo 50331648 ;; esac; }
other_part() { case $1 in 2) echo 3 ;; 3) echo 2 ;; esac; }
DATA_OFF=83886080
ENV_DEV=/dev/loop0

# the devices model needs the engine's added disk (guest.disks): a device node that is missing
# would become a file in /dev, in RAM, and the device would forget its eMMC at a reboot
need_disks() {
  [ "$STORAGE" != devices ] || [ -b /dev/vdb ] || { say "no eMMC /dev/vdb: the engine attached no added disk"; return 1; }
}

# version and health of the bytes in a partition: "<version> <health>", or "none" when its
# first 24 MiB are no complete OS image (a verified boot fails on such a slot)
slot_content() {
  h=$(head -c $IMAGE_BYTES "$(part_dev "$1")" | sha256sum | cut -d' ' -f1)
  awk -v h="$h" '$1 == h { print $3, $4; found = 1 } END { if (!found) print "none" }' $OTA/$ARCH/images/known
}

load_modules() {   # hvx's guest kernel has loop as a module; the Mac's has it built in
  [ -e /sys/module/loop ] || { [ -f $OTA/modules/loop.ko ] && insmod $OTA/modules/loop.ko; }
  return 0
}

attach() {   # loop device, offset: a partition of the eMMC, written through to it
  [ -b "$1" ] || mknod "$1" b 7 "${1#/dev/loop}"
  losetup -d "$1" 2>/dev/null
  losetup -o "$2" "$1" "$EMMC" || { say "cannot attach $1"; return 1; }
  loopdio "$1" on || { say "cannot switch $1 to direct I/O"; return 1; }
}

attach_emmc() {
  need_disks || return 1
  attach /dev/loop0 0 && attach /dev/loop1 $DATA_OFF && attach /dev/loop2 16777216 && attach /dev/loop3 50331648
}

# the factory: both slots get v1, the data partition its first contents, the environment
# U-Boot's first one (runs once, at the first boot)
factory() {
  [ -f /data/flash/factory-done ] && return 0
  need_disks || return 1
  say "factory: writing v1 into both slots, the data partition and U-Boot's environment"
  mkdir -p /data/flash
  if [ "$STORAGE" = loop ]; then
    dd if=/dev/zero of=$EMMC bs=1048576 count=$EMMC_MIB 2>/dev/null
  fi
  for p in 2 3; do
    dd if=$OTA/$ARCH/images/v1.ext4 of=$EMMC bs=1048576 seek=$(($(part_off $p) / 1048576)) conv=notrunc 2>/dev/null
  done
  dd if=$OTA/$ARCH/images/data.ext4 of=$EMMC bs=1048576 seek=$((DATA_OFF / 1048576)) conv=notrunc 2>/dev/null
  dd if=$OTA/common/uboot-default.env of=$EMMC bs=131072 seek=32 conv=notrunc,fsync 2>/dev/null
  sync
  : > /data/flash/factory-done
  sync
}

envvar() {   # a variable of the environment, or $2 when it is unset or empty
  v=$(fw_printenv -n "$1" 2>/dev/null) || v=
  if [ -n "$v" ]; then echo "$v"; else echo "$2"; fi
}

# U-Boot with Mender's integration (meta-mender) and U-Boot's boot counter
# (CONFIG_BOOTCOUNT_ENV): mender_setup sets the save canary once; while upgrade_available=1,
# every boot counts bootcount up and saves the environment; once bootcount exceeds bootlimit
# (1), altbootcmd runs mender_altbootcmd, which switches mender_boot_part to the other
# partition, clears upgrade_available and saves. Prints the partition to boot. When neither
# copy of the environment is valid, U-Boot runs with its default environment and saves it.
uboot_choose() {
  if ! fw_printenv > /dev/null 2>&1; then
    say "uboot: no valid environment copy, using the default environment" >&2
    dd if=$OTA/common/uboot-default.env of=$ENV_DEV bs=131072 seek=32 conv=notrunc,fsync 2>/dev/null
  fi
  if [ "$(envvar mender_saveenv_canary 0)" != 1 ]; then
    fw_setenv mender_saveenv_canary 1 >&2 || say "uboot: saveenv failed" >&2
  fi
  part=$(envvar mender_boot_part 2)
  ua=$(envvar upgrade_available 0)
  bc=$(envvar bootcount 0)
  bl=$(envvar bootlimit 1)
  if [ "$ua" = 1 ]; then
    bc=$((bc + 1))
    fw_setenv bootcount $bc >&2 || say "uboot: saveenv failed" >&2
    if [ $bc -gt "$bl" ]; then
      part=$(other_part "$part")
      say "uboot: bootcount $bc above bootlimit $bl: mender_altbootcmd boots partition $part" >&2
      printf 'mender_boot_part=%s\nmender_boot_part_hex=%s\nupgrade_available=0\n' "$part" "$part" > /tmp/ota-uboot.env
      fw_setenv -s /tmp/ota-uboot.env >&2 || say "uboot: saveenv failed" >&2
    fi
  fi
  echo "$part"
}

# the kernel and the init system: the slot is the root file system, mounted read-only (as a
# dm-verity or squashfs root would be), the data partition read-write at /data
mount_root() {   # partition
  grep -q " $ROOT " /proc/mounts && umount -l $ROOT 2>/dev/null
  mkdir -p $ROOT
  mount -t ext4 -o ro,noload "$(part_dev "$1")" $ROOT || { say "cannot mount partition $1"; return 1; }
  mount -t ext4 /dev/loop1 $ROOT/data || { say "cannot mount the data partition"; return 1; }
  mount --bind /dev $ROOT/dev && mount -t proc proc $ROOT/proc && mount -t sysfs sys $ROOT/sys
  mount -t tmpfs -o size=8m run $ROOT/run && mount -t tmpfs -o size=16m tmp $ROOT/tmp
  mount --bind $OTA/$ARCH/artifacts $ROOT/mnt/updates
  mkdir -p $ROOT/data/app
}

# A cold boot: what the boot ROM, U-Boot and the kernel do. The kernel's verified boot is a
# hash of the whole slot (as dm-verity would refuse a root file system with a changed block):
# a slot that does not verify panics, and the watchdog resets the device into U-Boot again.
cold_boot() {
  mkdir -p $RUN/req $RUN/res
  grep -q ' /run ' /proc/mounts || mount -t tmpfs -o size=4m ota-run /run   # fw_printenv's lock
  load_modules
  factory || return 1
  attach_emmc || return 1
  gen=$(cat $RUN/bootgen 2>/dev/null || echo 0)
  resets=0
  while [ $resets -lt $MAX_RESETS ]; do
    gen=$((gen + 1)); echo $gen > $RUN/bootgen
    p=$(uboot_choose)
    set -- $(slot_content "$p")
    if [ "$1" = none ]; then
      say "boot $gen: partition $p does not verify: kernel panic, watchdog reset"
      echo "$gen $p unverified" >> $RUN/boots
      resets=$((resets + 1))
      continue
    fi
    mount_root "$p" || return 1
    say "boot $gen: partition $p, version $1 ($2)"
    echo "$gen $p v$1 $2" >> $RUN/boots
    printf 'part=%s\nversion=%s\nhealth=%s\ngen=%s\n' "$p" "$1" "$2" "$gen" > $RUN/booted
    return 0
  done
  say "bricked: no slot verified in $MAX_RESETS boots"
  echo bricked > $RUN/bricked
  printf 'part=none\nversion=none\nhealth=none\ngen=%s\n' "$gen" > $RUN/booted
  return 0
}

booted() { sed -n "s/^$1=//p" $RUN/booted 2>/dev/null; }

# a program of the booted slot, run as the device runs it (its root is the slot)
on_device() { chroot $ROOT /bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME=/ "$@"; }

# mender-update on the device; prints "<exit code> <its last line>", its whole output goes to
# stderr (the service's log)
mender() {
  out=$(on_device mender-update "$@" 2>&1); rc=$?
  echo "$out" | sed 's/^/  mender-update: /' >&2
  line=$(echo "$out" | grep -v '^record_id=\|level=' | sed 's/^ *//' | grep . | tail -n 1)
  echo "$rc ${line:-(no output)}"
}

# the bootloader's next boot: "<partition> <upgrade_available> <bootcount>"
env_now() {
  echo "$(envvar mender_boot_part 2) $(envvar upgrade_available 0) $(envvar bootcount 0)"
}

# what Mender's database holds, read with LMDB's mdb_dump without changing it: the value of a
# key, the artifact name (what `mender-update show-artifact` prints) and the standalone state of
# an update in progress ("<InState> <ArtifactName>", "none" without one)
mender_db() {
  mdb_dump -n -p $ROOT/data/mender/mender-store 2>/dev/null \
    | awk -v k=" $1" 'f { print substr($0, 2); exit } $0 == k { f = 1 }'
}
mender_artifact() { a=$(mender_db artifact-name); echo "${a:-unknown}"; }
mender_state() {
  s=$(mender_db standalone-state)
  if [ -z "$s" ]; then echo none; return; fi
  st=$(echo "$s" | sed -n 's/.*"InState":"\([^"]*\)".*/\1/p')
  name=$(echo "$s" | sed -n 's/.*"ArtifactName":"\([^"]*\)".*/\1/p')
  echo "${st:-unknown} ${name:-unknown}"
}

# The client's model: the versions the device may boot at its next boot, from $HYDO_OUTCOMES.
# Update k is install i<k> (Artifact v<2 + k mod 4>; v4 fails its health check), then reboot
# r<k>, then the commit c<k> that the device runs after the reboot. The reboot's outcome names
# the version the client saw boot (id r<k>v<version>) or is unknown (id r<k>); the commit's is
# ack (exit 0), failed, none (no update in progress) or unknown. Three sets: cur (may be booted
# now), nxt (may boot next) and perm (may boot after the next reboot without an install):
#   install acknowledged, good image  -> nxt = {that version}
#   install acknowledged, v4          -> nxt = cur (the device rolls v4 back)
#   install failed                    -> nxt = nxt + cur + perm
#   install unknown                   -> nxt = nxt + cur + perm (+ that version if it is good)
#   reboot saw version x              -> cur = nxt = {x}
#   reboot unknown                    -> cur = cur + nxt
#   commit acknowledged or none       -> perm = cur
#   commit failed or unknown          -> perm = perm + cur
# Without an install after a commit, nxt is perm. allowed_versions [UPTO]: nxt after install UPTO
# and before its reboot; without UPTO, after all.
allowed_versions() {
  awk -v upto="${1:--1}" '
    function union(a, b,   x) { for (x in b) a[x] = 1 }
    function copy(a, b,   x) { delete a; for (x in b) a[x] = 1 }
    $1 ~ /^i[0-9]+$/ { k = substr($1, 2) + 0; inst[k] = $2; if (k > max) max = k }
    $1 ~ /^c[0-9]+$/ { k = substr($1, 2) + 0; com[k] = $2; if (k > max) max = k }
    $1 ~ /^r[0-9]+v[0-9a-z]+$/ { split(substr($1, 2), p, "v"); rb[p[1] + 0] = p[2]; if (p[1] + 0 > max) max = p[1] + 0 }
    $1 ~ /^r[0-9]+$/ { k = substr($1, 2) + 0; if (!(k in rb)) rb[k] = "?"; if (k > max) max = k }
    END {
      cur[1] = 1; nxt[1] = 1; perm[1] = 1
      if (upto >= 0) max = upto
      for (k = 0; k <= max; k++) {
        if (k in inst) {
          v = 2 + k % 4; good = (v != 4)
          if (inst[k] == "ack") { delete nxt; if (good) nxt[v] = 1; else union(nxt, cur) }
          else { union(nxt, cur); union(nxt, perm); if (inst[k] == "unknown" && good) nxt[v] = 1 }
        }
        if (k == upto || !(k in rb)) continue
        if (rb[k] == "?") union(cur, nxt)
        else { delete cur; delete nxt; cur[rb[k]] = 1; nxt[rb[k]] = 1 }
        if (k in com) {
          if (com[k] == "ack" || com[k] == "none") copy(perm, cur)
          else union(perm, cur)
          if (!((k + 1) in inst)) copy(nxt, perm)
        }
      }
      for (x in nxt) printf "%s ", x; print ""
    }' "$HYDO_OUTCOMES"
}

bundle_of() { echo $((2 + $1 % 4)); }
