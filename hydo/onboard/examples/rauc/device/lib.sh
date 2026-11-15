# Shared by the device's scripts (busybox sh). The device:
#   /data/flash/slot-a.img, slot-b.img   the two root file system slots (16 MiB each), attached
#                                        as /dev/loop0 (A) and /dev/loop1 (B): block devices
#                                        whose writes are durable only after a flush (fsync)
#   /data/flash/uboot.env                the U-Boot environment, two redundant 16 KiB copies
#   /data/part                           the update tool's partition, a file system of its own
#                                        (services[].storage): RAUC's data-directory, the
#                                        target of the storage faults G= and L=
#   /data/app/records                    the application's data: one synced line per update
#   /tmp/ota/                            what lives in RAM: lost at every boot
OTA=/opt/ota
FLASH=/data/flash
PART=/data/part
APP=/data/app
RUN=/tmp/ota
SLOT_BYTES=16777216
IMAGE_BYTES=12582912
MAX_RESETS=12   # boots in a row without a slot that verifies: the device is bricked

say() { echo "ota: [$(cut -d" " -f1 /proc/uptime)] $*"; }

# an assertion record on the console: the event a reaction waits for (A=<id>@<step>+<ms>:cut
# cuts the power <ms> of guest time after the first such record from step <step> on)
event() {
  c=/dev/ttyS0; [ -c $c ] || c=/dev/console
  echo "HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"$1\",\"ok\":true,\"msg\":\"$2\",\"details\":{}}" > $c
}

slot_dev() { case $1 in A) echo /dev/loop0 ;; B) echo /dev/loop1 ;; esac; }
slot_file() { case $1 in A) echo $FLASH/slot-a.img ;; B) echo $FLASH/slot-b.img ;; esac; }
other_slot() { case $1 in A) echo B ;; B) echo A ;; esac; }

# version and health of the bytes in a slot: "<version> <health>", or "none" when the slot's
# first 12 MiB are no complete OS image (a verified boot fails on such a slot)
slot_content() {
  h=$(head -c $IMAGE_BYTES "$(slot_dev "$1")" | sha256sum | cut -d' ' -f1)
  awk -v h="$h" '$1 == h { print $3, $4; found = 1 } END { if (!found) print "none" }' $OTA/images/known
}

load_modules() {   # hvx's guest kernel has loop and squashfs as modules; the Mac's has loop built in
  for m in loop squashfs; do
    [ -e /sys/module/$m ] && continue
    [ -f $OTA/modules/$m.ko ] && insmod $OTA/modules/$m.ko
  done
  return 0
}

attach_slots() {
  for s in A B; do
    d=$(slot_dev $s)
    [ -b "$d" ] || mknod "$d" b 7 "${d#/dev/loop}"
    losetup -d "$d" 2>/dev/null
    losetup "$d" "$(slot_file $s)" || { say "cannot attach slot $s"; return 1; }
  done
}

# The kernel command line a booted device has: RAUC reads /proc/cmdline to find the booted slot
# (rauc.slot=<slot>), and hydo's guest kernel line cannot carry it, so every cold boot binds a
# file with the guest's line and rauc.slot=<the booted slot> over /proc/cmdline. hydo's guest
# agent reads the line only when it starts, and the file keeps its words.
set_cmdline() {
  umount /proc/cmdline 2>/dev/null
  echo "$(cat /proc/cmdline) rauc.slot=$1" > $RUN/cmdline
  mount --bind $RUN/cmdline /proc/cmdline
}
rauc_dev() { rauc "$@"; }

# the factory: both slots get v1, the environment U-Boot's first one (runs once, at the first boot)
factory() {
  [ -f $FLASH/factory-done ] && return 0
  say "factory: writing v1 into both slots"
  mkdir -p $FLASH
  for s in A B; do
    dd if=/dev/zero of="$(slot_file $s)" bs=1048576 count=16 2>/dev/null
    dd if=$OTA/images/v1.ext4 of="$(slot_file $s)" bs=1048576 conv=notrunc 2>/dev/null
  done
  cp $OTA/images/uboot-default.env $FLASH/uboot.env
  mkdir -p $PART/rauc $APP
  : > $APP/records
  sync
  : > $FLASH/factory-done
  sync
}

envvar() {   # a variable of the environment, or $2 when it is unset or empty (U-Boot's `test -n`)
  v=$(fw_printenv -n "$1" 2>/dev/null) || v=
  if [ -n "$v" ]; then echo "$v"; else echo "$2"; fi
}

# U-Boot's boot script for RAUC (the one in RAUC's documentation, "Integration", U-Boot): take the
# first slot of BOOT_ORDER whose BOOT_<slot>_LEFT is above 0, count one attempt down, saveenv,
# boot it. No such slot: set both counters to 3, saveenv, reset. Prints the chosen slot. When
# neither copy of the environment is valid, U-Boot runs with its default environment, and its
# saveenv writes that: here, the default image.
uboot_choose() {
  if ! fw_printenv > /dev/null 2>&1; then
    say "uboot: no valid environment copy, using the default environment" >&2
    cp $OTA/images/uboot-default.env $FLASH/uboot.env && fsync $FLASH/uboot.env
  fi
  order=$(envvar BOOT_ORDER "A B")
  left_a=$(envvar BOOT_A_LEFT 3)
  left_b=$(envvar BOOT_B_LEFT 3)
  for s in $order; do
    case $s in A) l=$left_a ;; B) l=$left_b ;; *) continue ;; esac
    l=$((0x$l))
    if [ "$l" -gt 0 ]; then
      printf 'BOOT_ORDER=%s\nBOOT_A_LEFT=%s\nBOOT_B_LEFT=%s\nBOOT_%s_LEFT=%x\n' \
        "$order" "$left_a" "$left_b" "$s" $((l - 1)) > /tmp/ota-uboot.env
      fw_setenv -s /tmp/ota-uboot.env >&2 || say "uboot: saveenv failed" >&2
      echo "$s"
      return 0
    fi
  done
  say "uboot: no valid slot found, resetting tries to 3" >&2
  printf 'BOOT_ORDER=%s\nBOOT_A_LEFT=3\nBOOT_B_LEFT=3\n' "$order" > /tmp/ota-uboot.env
  fw_setenv -s /tmp/ota-uboot.env >&2 || say "uboot: saveenv failed" >&2
  return 1
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
  # the kernel's boot ID, which RAUC's service reads to tell a reboot from its own restart: a
  # power cut's recovery boot has a new one, the agent's reboots keep the running kernel's
  say "kernel boot_id $(cat /proc/sys/kernel/random/boot_id)"
  resets=0
  while [ $resets -lt $MAX_RESETS ]; do
    gen=$((gen + 1)); echo $gen > $RUN/bootgen
    s=$(uboot_choose) || { resets=$((resets + 1)); continue; }
    set -- $(slot_content "$s")
    if [ "$1" = none ]; then
      say "boot $gen: slot $s does not verify: kernel panic, watchdog reset"
      echo "$gen $s unverified" >> $RUN/boots
      resets=$((resets + 1))
      continue
    fi
    say "boot $gen: slot $s, version $1 ($2)"
    echo "$gen $s v$1 $2" >> $RUN/boots
    set_cmdline "$s"
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
# RAUC writes the slot that is not booted, so an install that was never booted can be overwritten
# by the next one. Two sets: cur (may be booted now) and nxt (may boot at the next boot):
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
