# Shared by the device's scripts (busybox sh). The device has one of two storage models
# (RESULTS.md, "The storage model"; /etc/ota/storage):
#   loop (hydo.yaml):
#     /data/flash/slot-a.img, slot-b.img the two root file system slots (16 MiB each), attached
#                                        as /dev/loop0 (A) and /dev/loop1 (B): block devices
#                                        whose writes are durable only after a flush (fsync)
#     /data/flash/uboot.env              the U-Boot environment, two redundant 16 KiB copies
#   devices (hydo-devices.yaml, guest.disks):
#     /dev/vdb                           the eMMC with its own volatile cache: slot A at 0,
#                                        slot B at 16 MiB, as /dev/loop0 and /dev/loop1 with
#                                        direct I/O (a partition of the eMMC)
#     /dev/vdc                           the SPI NOR flash: the environment, every write durable
#   /data/app/records                    the application's data: one synced line per update
#   /data/app/violations                 the client's wrong boots (synced)
#   /tmp/ota/                            what lives in RAM: lost at every boot
# The bootloader's state is U-Boot's usual one for SWUpdate's double copy: boot_slot (A or B),
# and U-Boot's boot counter: upgrade_available, bootcount, bootlimit.
OTA=/opt/ota
FLASH=/data/flash
APP=/data/app
RUN=/tmp/ota
IMAGE_BYTES=12582912
MAX_RESETS=12   # boots in a row without a slot that verifies: the device is bricked

say() { echo "ota: [$(cut -d" " -f1 /proc/uptime)] $*"; }

# an assertion record on the console: the event a reaction waits for (A=<id>@<step>+<ms>:cut
# cuts the power <ms> of guest time after the first such record from step <step> on)
event() {
  c=/dev/ttyS0; [ -c $c ] || c=/dev/console
  echo "HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"$1\",\"ok\":true,\"msg\":\"$2\",\"details\":{}}" > $c
}

STORAGE=$(cat /etc/ota/storage 2>/dev/null || echo loop)
ENV_DEV=/data/flash/uboot.env; [ "$STORAGE" = devices ] && ENV_DEV=/dev/vdc
slot_dev() { case $1 in A) echo /dev/loop0 ;; B) echo /dev/loop1 ;; esac; }
slot_off() { case $1 in A) echo 0 ;; B) echo 16777216 ;; esac; }
slot_file() { case $1 in A) echo $FLASH/slot-a.img ;; B) echo $FLASH/slot-b.img ;; esac; }
other_slot() { case $1 in A) echo B ;; B) echo A ;; esac; }

# version and health of the bytes in a slot: "<version> <health>", or "none" when the slot's
# first 12 MiB are no complete OS image (a verified boot fails on such a slot)
slot_content() {
  h=$(head -c $IMAGE_BYTES "$(slot_dev "$1")" | sha256sum | cut -d' ' -f1)
  awk -v h="$h" '$1 == h { print $3, $4; found = 1 } END { if (!found) print "none" }' $OTA/images/known
}

load_modules() {   # hvx's guest kernel has loop as a module; the Mac's has it built in
  [ -e /sys/module/loop ] || { [ -f $OTA/modules/loop.ko ] && insmod $OTA/modules/loop.ko; }
  return 0
}

# The slots' storage model (RESULTS.md, "The storage model"): with the build parameter dio=1 (/etc/ota/dio)
# the loop devices use direct I/O, so a written block reaches the disk under them when the
# loop device's page cache is written back, as on an eMMC that also holds the environment.
# Without it, a block also waits in the backing file's page cache until a flush of the slot,
# as on a slot device that the environment's flushes do not reach.
# the devices model needs the engine's added disks (guest.disks): a device node that is missing
# would become a file in /dev, in RAM, and the device would forget its slots at a reboot
need_disks() {
  [ "$STORAGE" != devices ] || { [ -b /dev/vdb ] && [ -b /dev/vdc ]; } || { say "no eMMC /dev/vdb or SPI NOR /dev/vdc: the engine attached no added disks"; return 1; }
}

attach_slots() {
  need_disks || return 1
  for s in A B; do
    d=$(slot_dev $s)
    [ -b "$d" ] || mknod "$d" b 7 "${d#/dev/loop}"
    losetup -d "$d" 2>/dev/null
    if [ "$STORAGE" = devices ]; then
      # a partition of the eMMC: the slot's 16 MiB of /dev/vdb, written through to the disk
      losetup -o "$(slot_off $s)" "$d" /dev/vdb || { say "cannot attach slot $s"; return 1; }
      loopdio "$d" on || say "cannot switch $d to direct I/O"
      continue
    fi
    losetup "$d" "$(slot_file $s)" || { say "cannot attach slot $s"; return 1; }
    if [ "$(cat /etc/ota/dio 2>/dev/null)" = 1 ]; then loopdio "$d" on || say "cannot switch $d to direct I/O"; fi
  done
}

# the factory: both slots get v1, the environment U-Boot's first one (runs once, at the first boot)
factory() {
  [ -f $FLASH/factory-done ] && return 0
  need_disks || return 1
  say "factory: writing v1 into both slots"
  mkdir -p $FLASH
  if [ "$STORAGE" = devices ]; then
    for s in A B; do
      dd if=$OTA/images/v1.ext4 of=/dev/vdb bs=1048576 seek=$(($(slot_off $s) / 1048576)) conv=notrunc,fsync 2>/dev/null
    done
    dd if=$OTA/images/uboot-default.env of=$ENV_DEV conv=notrunc,fsync 2>/dev/null
  else
  for s in A B; do
    dd if=/dev/zero of="$(slot_file $s)" bs=1048576 count=16 2>/dev/null
    dd if=$OTA/images/v1.ext4 of="$(slot_file $s)" bs=1048576 conv=notrunc 2>/dev/null
  done
  cp $OTA/images/uboot-default.env $FLASH/uboot.env
  fi
  mkdir -p $APP
  : > $APP/records
  sync
  : > $FLASH/factory-done
  sync
}

envvar() {   # a variable of the environment, or $2 when it is unset or empty
  v=$(fw_printenv -n "$1" 2>/dev/null) || v=
  if [ -n "$v" ]; then echo "$v"; else echo "$2"; fi
}

# U-Boot with its boot counter (CONFIG_BOOTCOUNT_ENV), as SWUpdate's documentation and
# meta-swupdate set it up: while upgrade_available=1, every boot counts bootcount up and saves
# the environment; once bootcount exceeds bootlimit, U-Boot runs altbootcmd instead, which
# switches boot_slot to the other slot and clears upgrade_available and bootcount. Prints the
# slot to boot. When neither copy of the environment is valid, U-Boot runs with its default
# environment, and its saveenv writes that: here, the default image.
uboot_choose() {
  if ! fw_printenv > /dev/null 2>&1; then
    say "uboot: no valid environment copy, using the default environment" >&2
    if [ "$STORAGE" = devices ]; then
      dd if=$OTA/images/uboot-default.env of=$ENV_DEV conv=notrunc,fsync 2>/dev/null
    else
      cp $OTA/images/uboot-default.env $FLASH/uboot.env && fsync $FLASH/uboot.env
    fi
  fi
  slot=$(envvar boot_slot A)
  ua=$(envvar upgrade_available 0)
  bc=$(envvar bootcount 0)
  bl=$(envvar bootlimit 3)
  if [ "$ua" = 1 ]; then
    bc=$((bc + 1))
    if [ $bc -gt "$bl" ]; then
      slot=$(other_slot "$slot")
      say "uboot: bootcount $bc above bootlimit $bl: altbootcmd boots slot $slot" >&2
      printf 'boot_slot=%s\nupgrade_available=0\nbootcount=0\n' "$slot" > /tmp/ota-uboot.env
    else
      printf 'bootcount=%s\n' $bc > /tmp/ota-uboot.env
    fi
    fw_setenv -s /tmp/ota-uboot.env >&2 || say "uboot: saveenv failed" >&2
  fi
  echo "$slot"
}

# A cold boot: what the boot ROM, U-Boot and the kernel do. The kernel's verified boot is a
# hash of the whole slot (as dm-verity would refuse a root file system with a changed block):
# a slot that does not verify panics, and the watchdog resets the device into U-Boot again.
cold_boot() {
  mkdir -p $RUN/req $RUN/res
  # libubootenv's lock file is /run/fw_printenv.lock; /run is a tmpfs, as on a device
  grep -q ' /run ' /proc/mounts || mount -t tmpfs -o size=4m ota-run /run
  load_modules
  factory || return 1
  attach_slots || return 1
  gen=$(cat $RUN/bootgen 2>/dev/null || echo 0)
  resets=0
  while [ $resets -lt $MAX_RESETS ]; do
    gen=$((gen + 1)); echo $gen > $RUN/bootgen
    s=$(uboot_choose)
    set -- $(slot_content "$s")
    if [ "$1" = none ]; then
      say "boot $gen: slot $s does not verify: kernel panic, watchdog reset"
      echo "$gen $s unverified" >> $RUN/boots
      resets=$((resets + 1))
      continue
    fi
    say "boot $gen: slot $s, version $1 ($2)"
    echo "$gen $s v$1 $2" >> $RUN/boots
    printf 'slot=%s\nversion=%s\nhealth=%s\ngen=%s\n' "$s" "$1" "$2" "$gen" > $RUN/booted
    return 0
  done
  say "bricked: no slot verified in $MAX_RESETS boots"
  echo bricked > $RUN/bricked
  printf 'slot=none\nversion=none\nhealth=none\ngen=%s\n' "$gen" > $RUN/booted
  return 0
}

booted() { sed -n "s/^$1=//p" $RUN/booted 2>/dev/null; }

# The client's model: the versions the device may boot at its next boot, from $HYDO_OUTCOMES.
# Update k is install i<k> (bundle v<2 + k mod 4>; v4 is broken) and then reboot r<k>; the
# reboot's outcome names the version it saw boot (id r<k>v<version>) or is unknown (id r<k>).
# The agent installs into the slot that is not booted, so an install that was never booted can
# be overwritten by the next one. Two sets: cur (may be booted now) and nxt (may boot next):
#   install acknowledged, good image  -> nxt = {that version}
#   install acknowledged, v4          -> nxt = cur (the boot counter rolls the device back)
#   install failed                    -> nxt = nxt + cur
#   install unknown                   -> nxt = nxt + cur (+ that version if it is good)
#   reboot saw version x              -> cur = nxt = {x}
#   reboot unknown                    -> cur = cur + nxt (the next boot is still nxt)
# allowed_versions [UPTO]: nxt after install UPTO and before its reboot; without UPTO, after all.
allowed_versions() {
  awk -v upto="${1:--1}" '
    function union(a, b,   x) { for (x in b) a[x] = 1 }
    $1 ~ /^i[0-9]+$/ { k = substr($1, 2) + 0; inst[k] = $2; if (k > max) max = k }
    $1 ~ /^r[0-9]+v[0-9a-z]+$/ { split(substr($1, 2), p, "v"); rb[p[1] + 0] = p[2]; if (p[1] + 0 > max) max = p[1] + 0 }
    $1 ~ /^r[0-9]+$/ { k = substr($1, 2) + 0; if (!(k in rb)) rb[k] = "?"; if (k > max) max = k }
    END {
      cur[1] = 1; nxt[1] = 1
      if (upto >= 0) max = upto
      for (k = 0; k <= max; k++) {
        if (k in inst) {
          v = 2 + k % 4; good = (v != 4)
          if (inst[k] == "ack") { delete nxt; if (good) nxt[v] = 1; else union(nxt, cur) }
          else { union(nxt, cur); if (inst[k] == "unknown" && good) nxt[v] = 1 }
        }
        if (k == upto || !(k in rb)) continue
        if (rb[k] == "?") union(cur, nxt)
        else { delete cur; delete nxt; cur[rb[k]] = 1; nxt[rb[k]] = 1 }
      }
      for (x in nxt) printf "%s ", x; print ""
    }' "$HYDO_OUTCOMES"
}

bundle_of() { echo $((2 + $1 % 4)); }
