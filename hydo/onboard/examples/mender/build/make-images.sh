#!/bin/sh
# Runs inside the native tools image (build.sh starts it): the device's slot images, its data
# partition, U-Boot's environment and the Mender Artifacts, into the directory $1, for each
# architecture whose tools are in $1/<arch>/bin (aarch64, x86_64):
#   <arch>/images/v1.ext4 ... v5.ext4  root file systems of the device (24 MiB ext4): busybox,
#                                    mender-update, mender-flash, fw_printenv/fw_setenv,
#                                    Mender's rootfs-image Update Module, a Download state
#                                    script, /etc/os-release with the version and its health
#                                    (v4 fails its health check)
#   <arch>/images/known              "<sha256> <size> <version> <health>" of every slot image
#   <arch>/images/data.ext4          the data partition as the factory writes it: Mender's data
#                                    store with mender.conf (RootfsPartA/B), device_type and the
#                                    bootstrap Artifact of v1; the application's directory
#   <arch>/artifacts/v2.mender ...   Mender Artifacts of type rootfs-image, one per version, with
#                                    the state scripts that print the console events, and
#                                    v2-plain.mender ... without state scripts
#   common/uboot-default.env         U-Boot's redundant environment, two 128 KiB copies as
#                                    Mender's U-Boot integration keeps them (copy 0 valid)
# Every file time is SOURCE_DATE_EPOCH, and the Artifacts' outer tar entries too (retime.go),
# so every build gives the same bytes.
set -eu
out=$1
umask 022
export SOURCE_DATE_EPOCH=1767225600 E2FSPROGS_FAKE_TIME=1767225600
B=/build
go build -trimpath -o /tmp/retime $B/retime.go

# U-Boot's environment: Mender's variables (meta-mender's U-Boot integration): mender_boot_part
# 2 (slot A) or 3 (slot B), upgrade_available, bootcount, bootlimit=1, and the canary that
# U-Boot sets at its first boot. 128 KiB per copy = CRC32 (little endian) + flags byte +
# "name=value\0" entries, an empty entry, zeros. gzip's trailer carries the CRC32 of its input.
mkdir -p "$out/common"
cd "$out/common"
printf '%s\000' \
  'altbootcmd=run mender_altbootcmd; run bootcmd' \
  'bootcount=0' \
  'bootlimit=1' \
  'mender_boot_part=2' \
  'mender_boot_part_hex=2' \
  'mender_check_saveenv_canary=1' \
  'upgrade_available=0' > envdata
printf '\000' >> envdata
head -c $((131072 - 5 - $(stat -c %s envdata))) /dev/zero >> envdata
{ gzip -c envdata | tail -c 8 | head -c 4; printf '\001'; cat envdata; } > uboot-default.env
head -c 131072 /dev/zero | tr '\0' '\377' >> uboot-default.env
rm -f envdata
[ "$(stat -c %s uboot-default.env)" = 262144 ]

slot_tree() {   # dir arch version health
  t=$1; bin=$out/$2/bin
  rm -rf "$t" && mkdir -p "$t"
  cd "$t"
  mkdir -p bin usr/bin usr/share/mender/modules/v3 etc/mender/scripts var/lib data dev proc sys run tmp mnt/updates
  install -m 0755 "$bin/busybox" bin/busybox
  for a in sh ash cat sed grep cut stat readlink findfs sync which mv cp rm mkdir echo printf test \
           [ ls sleep head tail sha256sum awk mount umount dd tr wc env true false basename \
           dirname ln touch kill uname cmp sort expr; do
    ln -s busybox "bin/$a"
  done
  install -m 0755 "$bin/mender-update" usr/bin/mender-update
  install -m 0755 "$bin/mender-flash" usr/bin/mender-flash
  install -m 0755 "$bin/fw_printenv" usr/bin/fw_printenv
  ln -s fw_printenv usr/bin/fw_setenv
  install -m 0755 "$bin/rootfs-image" usr/share/mender/modules/v3/rootfs-image
  ln -s /data/mender var/lib/mender
  # U-Boot's environment: two copies in the eMMC's boot area, before the first partition
  # (meta-mender's offsets); /dev/loop0 is the whole eMMC (device/lib.sh)
  printf '/dev/loop0 0x400000 0x20000\n/dev/loop0 0x800000 0x20000\n' > etc/fw_env.config
  printf '{\n  "UpdatePollIntervalSeconds": 1800\n}\n' > etc/mender/mender.conf
  echo 3 > etc/mender/scripts/version
  install -m 0755 $B/state-script.sh etc/mender/scripts/Download_Enter_50_hydo
  printf 'NAME=hydo-mender-device\nVERSION_ID=%s\nHEALTH=%s\n' "$3" "$4" > etc/os-release
  find "$t" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
}

scripts=/tmp/scripts
rm -rf $scripts && mkdir -p $scripts
for s in ArtifactInstall_Enter_50_hydo ArtifactCommit_Enter_50_hydo ArtifactCommit_Leave_50_hydo \
         ArtifactRollback_Enter_50_hydo; do
  install -m 0755 $B/state-script.sh "$scripts/$s"
done
touch -h -d "@$SOURCE_DATE_EPOCH" $scripts/*

for arch in aarch64 x86_64; do
  [ -d "$out/$arch/bin" ] || continue
  img=$out/$arch/images art=$out/$arch/artifacts
  rm -rf "$img" "$art" && mkdir -p "$img" "$art"
  : > "$img/known.tmp"
  for v in 1 2 3 4 5; do
    health=ok; [ "$v" = 4 ] && health=bad
    slot_tree /tmp/tree "$arch" "$v" "$health"
    cd "$img"
    mke2fs -q -t ext4 -b 4096 -L "rootfs-v$v" -U "0000000$v-0000-4000-8000-00000000000${v}" \
      -E root_owner=0:0,hash_seed=0000000$v-0000-4000-8000-000000000000 -d /tmp/tree "v$v.ext4" 24M
    touch -d "@$SOURCE_DATE_EPOCH" "v$v.ext4"
    echo "$(sha256sum "v$v.ext4" | cut -d' ' -f1) $(stat -c %s "v$v.ext4") $v $health" >> known.tmp
    if [ "$v" != 1 ]; then
      cd "$art"
      set --
      for s in $scripts/*; do set -- "$@" -s "$s"; done
      mender-artifact write rootfs-image -f "$img/v$v.ext4" -n "v$v" -t hydo-device \
        --no-progress -o "v$v.tmp" "$@"
      /tmp/retime "v$v.tmp" "v$v.mender" && rm -f "v$v.tmp"
      # the same Artifact without state scripts (build parameter artifacts=plain)
      mender-artifact write rootfs-image -f "$img/v$v.ext4" -n "v$v" -t hydo-device \
        --no-progress -o "v$v-plain.tmp"
      /tmp/retime "v$v-plain.tmp" "v$v-plain.mender" && rm -f "v$v-plain.tmp"
    fi
  done
  mv "$img/known.tmp" "$img/known"
  # the data partition as the factory writes it (meta-mender keeps RootfsPartA/B and the device
  # type in the data partition's mender.conf and device_type)
  rm -rf /tmp/data && mkdir -p /tmp/data/mender /tmp/data/app
  printf '{\n  "RootfsPartA": "/dev/loop2",\n  "RootfsPartB": "/dev/loop3"\n}\n' > /tmp/data/mender/mender.conf
  echo device_type=hydo-device > /tmp/data/mender/device_type
  mender-artifact write bootstrap-artifact -n v1 -t hydo-device --no-progress \
    --provides rootfs-image.version:v1 -o /tmp/bootstrap.tmp
  /tmp/retime /tmp/bootstrap.tmp /tmp/data/mender/bootstrap.mender && rm -f /tmp/bootstrap.tmp
  : > /tmp/data/app/records
  find /tmp/data -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
  cd "$img"
  mke2fs -q -t ext4 -b 4096 -L data -U 00000009-0000-4000-8000-000000000009 \
    -E root_owner=0:0,hash_seed=00000009-0000-4000-8000-000000000000 -d /tmp/data data.ext4 48M
  touch -d "@$SOURCE_DATE_EPOCH" data.ext4 "$art"/*
  rm -rf /tmp/tree /tmp/data
done
ls -l "$out"/*/images "$out"/*/artifacts "$out/common"
cat "$out"/*/images/known
sha256sum "$out"/*/artifacts/*
