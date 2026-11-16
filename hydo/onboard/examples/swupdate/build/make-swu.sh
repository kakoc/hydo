#!/bin/sh
# Runs inside the tools image (build.sh starts it): the device's OS images and the update
# bundles, into the directory $1:
#   images/v1.ext4             the factory image, written into both slots at the first boot
#   images/known               "<sha256> <size> <version> <health>" of every OS image
#   images/uboot-default.env   U-Boot's redundant environment as U-Boot's first saveenv writes
#                              it (copy 0 valid, copy 1 erased): boot_slot=A, the boot counter
#                              off (upgrade_available=0, bootcount=0, bootlimit=3)
#   bundles/v2.swu ... v5.swu  SWUpdate images, one root file system image each; the selection
#                              stable,copy1 writes slot A (/dev/loop0) and sets boot_slot=A,
#                              stable,copy2 writes slot B (/dev/loop1) and sets boot_slot=B; both
#                              set upgrade_available=1 and bootcount=0
# The OS images are the RAUC example's (../../rauc/build/make-bundles.sh): 12 MiB ext4 file
# systems with 6 MiB of firmware data made from the version; v4 fails its health check.
set -eu
out=$1
umask 022
mkdir -p "$out/images" "$out/bundles"
export SOURCE_DATE_EPOCH=1767225600 E2FSPROGS_FAKE_TIME=1767225600

cd "$out/images"
: > known.tmp
for v in 1 2 3 4 5; do
  health=ok; [ "$v" = 4 ] && health=bad
  rm -rf tree && mkdir -p tree/etc tree/usr/lib/firmware
  printf 'NAME=hydo-ota-device\nVERSION_ID=%s\nHEALTH=%s\n' "$v" "$health" > tree/etc/os-release
  key=$(printf '%032x' "$v")
  openssl enc -aes-128-ctr -K "$key" -iv 00000000000000000000000000000000 -nosalt -in /dev/zero 2>/dev/null \
    | head -c 6291456 > tree/usr/lib/firmware/blob.bin
  (cd tree && sha256sum etc/os-release usr/lib/firmware/blob.bin > etc/files.sha256)
  find tree -exec touch -h -d @1767225600 {} +
  rm -f "v$v.ext4"
  mke2fs -q -t ext4 -b 4096 -L "rootfs-v$v" -U "0000000$v-0000-4000-8000-000000000000" \
    -E root_owner=0:0,hash_seed=0000000$v-0000-4000-8000-000000000000 -d tree "v$v.ext4" 12M
  echo "$(sha256sum "v$v.ext4" | cut -d' ' -f1) $(stat -c %s "v$v.ext4") $v $health" >> known.tmp
  rm -rf tree
done
mv known.tmp known

# the environment: 16 KiB per copy = CRC32 (little endian) + flags byte + 16379 bytes of
# "name=value\0" entries, an empty entry, zeros. gzip's trailer carries the CRC32 of its input.
printf 'boot_slot=A\000upgrade_available=0\000bootcount=0\000bootlimit=3\000\000' > envdata
head -c $((16379 - $(stat -c %s envdata))) /dev/zero >> envdata
{ gzip -c envdata | tail -c 8 | head -c 4; printf '\001'; cat envdata; } > uboot-default.env
head -c 16384 /dev/zero | tr '\0' '\377' >> uboot-default.env
rm -f envdata
[ "$(stat -c %s uboot-default.env)" = 32768 ]

cd "$out/bundles"
for v in 2 3 4 5; do
  rm -rf "dir-v$v" "v$v.swu"
  mkdir "dir-v$v"
  cp "$out/images/v$v.ext4" "dir-v$v/rootfs.ext4"
  sha=$(sha256sum "dir-v$v/rootfs.ext4" | cut -d' ' -f1)
  copy() {   # name device slot
    printf '\t\t%s = {\n' "$1"
    printf '\t\t\timages: (\n\t\t\t\t{\n\t\t\t\t\tfilename = "rootfs.ext4";\n\t\t\t\t\tdevice = "%s";\n' "$2"
    printf '\t\t\t\t\ttype = "raw";\n\t\t\t\t\tsha256 = "%s";\n\t\t\t\t}\n\t\t\t);\n' "$sha"
    printf '\t\t\tbootenv: (\n\t\t\t\t{ name = "boot_slot"; value = "%s"; },\n' "$3"
    printf '\t\t\t\t{ name = "upgrade_available"; value = "1"; },\n'
    printf '\t\t\t\t{ name = "bootcount"; value = "0"; }\n\t\t\t);\n\t\t};\n'
  }
  {
    printf 'software =\n{\n\tversion = "%s.0.0";\n\tdescription = "hydo-ota-device v%s";\n\n\tstable = {\n' "$v" "$v"
    copy copy1 /dev/loop0 A
    copy copy2 /dev/loop1 B
    printf '\t};\n}\n'
  } > "dir-v$v/sw-description"
  # cpio's headers carry each file's time, and --reproducible fixes only the inode and device
  # numbers: without this, every build gave other bundles, so another image (Milestone 266)
  touch -h -d "@$SOURCE_DATE_EPOCH" "dir-v$v/sw-description" "dir-v$v/rootfs.ext4"
  (cd "dir-v$v" && printf 'sw-description\nrootfs.ext4\n' | cpio -o -H crc --reproducible --quiet > "../v$v.swu")
  rm -rf "dir-v$v"
done
ls -l "$out/images" "$out/bundles"
cat "$out/images/known"
