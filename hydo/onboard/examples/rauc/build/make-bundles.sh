#!/bin/sh
# Runs inside the tools image (build.sh starts it): the signing keys, the device's OS images and
# the update bundles, into the directory $1:
#   pki/ca.cert.pem                 the keyring the device trusts (the signing key stays here)
#   images/v1.ext4                  the factory image, written into both slots at the first boot
#   images/known                    "<sha256> <size> <version> <health>" of every OS image
#   bundles/v2.raucb ... v5.raucb   signed RAUC bundles, one image each (slot class rootfs)
#   images/uboot-default.env        U-Boot's redundant environment as U-Boot's first saveenv
#                                   writes it (copy 0 valid, copy 1 erased): BOOT_ORDER=A B and
#                                   3 attempts each. fw_setenv cannot start from an erased
#                                   environment, so the factory writes this image.
# Every OS image is a 12 MiB ext4 file system with /etc/os-release and 6 MiB of firmware data
# made from its version, so that a slot that holds half of one image and half of another has a
# hash that no image has. v4 is a broken release: its /etc/os-release says HEALTH=bad, the
# device's health check fails after it boots, and the boot counter rolls the device back.
set -eu
out=$1
umask 022
mkdir -p "$out/pki" "$out/images" "$out/bundles"
export PATH=/out:$PATH SOURCE_DATE_EPOCH=1767225600 E2FSPROGS_FAKE_TIME=1767225600

cd "$out/pki"
if [ ! -f signing.cert.pem ]; then
  # valid from 2020 to 2099, so that the guest's clock (and its clock steps) stays inside
  openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key.pem -out ca.cert.pem \
    -subj "/CN=hydo-ota example CA" -not_before 20200101000000Z -not_after 20991231235959Z \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign
  openssl req -newkey rsa:2048 -nodes -keyout signing.key.pem -out signing.csr \
    -subj "/CN=hydo-ota example signing"
  printf 'basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\n' > signing.ext
  openssl x509 -req -in signing.csr -CA ca.cert.pem -CAkey ca.key.pem -set_serial 2 \
    -not_before 20200101000000Z -not_after 20991231235959Z -extfile signing.ext -out signing.cert.pem
  rm -f signing.csr signing.ext
fi

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
printf 'BOOT_ORDER=A B\000BOOT_A_LEFT=3\000BOOT_B_LEFT=3\000\000' > envdata
head -c $((16379 - $(stat -c %s envdata))) /dev/zero >> envdata
{ gzip -c envdata | tail -c 8 | head -c 4; printf '\001'; cat envdata; } > uboot-default.env
head -c 16384 /dev/zero | tr '\0' '\377' >> uboot-default.env
rm -f envdata
[ "$(stat -c %s uboot-default.env)" = 32768 ]

cd "$out/bundles"
for v in 2 3 4 5; do
  [ -f "v$v.raucb" ] && continue   # a bundle's signature has a time: keep the bytes once made
  rm -rf "dir-v$v"
  mkdir "dir-v$v"
  cp "$out/images/v$v.ext4" "dir-v$v/rootfs.ext4"
  printf '[update]\ncompatible=hydo-ota-device\nversion=%s\n\n[bundle]\nformat=plain\n\n[image.rootfs]\nfilename=rootfs.ext4\n' \
    "$v" > "dir-v$v/manifest.raucm"
  rauc bundle --cert="$out/pki/signing.cert.pem" --key="$out/pki/signing.key.pem" \
    --keyring="$out/pki/ca.cert.pem" \
    "dir-v$v" "v$v.raucb"
  rm -rf "dir-v$v"
done
printf '%s 0x0000 0x4000\n%s 0x4000 0x4000\n' "$out/images/env.test" "$out/images/env.test" > /tmp/fw_env.config
cp "$out/images/uboot-default.env" "$out/images/env.test"
fw_printenv -c /tmp/fw_env.config
rm -f "$out/images/env.test"
ls -l "$out/images" "$out/bundles"
cat "$out/images/known"
