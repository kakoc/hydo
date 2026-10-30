#!/usr/bin/env bash
# Build the root disk of syzkaller's fuzzing VMs (Milestone 97): a small ext4 image with the
# pinned static busybox, an sshd (the build machine's OpenSSH and its shared libraries), the
# hvx.ko built for the debug kernel (loaded at boot), and root's authorized key. Runs on the
# bare-metal test machine (Ubuntu: OpenSSH, fakeroot, mke2fs) after build-debug.sh. No root needed:
# fakeroot makes every file owned by root inside the image. It only builds; the image boots
# on the build machine (campaign.sh). Output: $SAFETY/syz/{image-$BUNDLE.ext4,id_syz,id_syz.pub}.
set -euo pipefail
SAFETY=${SAFETY:-$HOME/hydo-safety}
HERE="$(cd "$(dirname "$0")" && pwd)"
O="$SAFETY/syz"
R="$O/root"
BB="$SAFETY/bin/busybox"
[ -x "$BB" ] || BB=/nix/store/ahw5fkq085k5ppdsylyd475xlxb18pz1-busybox-static-x86_64-unknown-linux-musl-1.37.0/bin/busybox
BUNDLE=${BUNDLE:-debug}	# debug = this tree's hvx.c, debug-orig = main's (to see the fuzzer find the fixed bugs)
KO="$SAFETY/$BUNDLE/hvx.ko"
[ -f "$KO" ] || { echo "build-image: build-debug.sh first ($KO)" >&2; exit 1; }
mkdir -p "$O"
[ -f "$O/id_syz" ] || ssh-keygen -q -t ed25519 -N '' -C hvx-syz -f "$O/id_syz"

rm -rf "$R"
mkdir -p "$R"/{bin,sbin,etc/ssh,etc/init.d,proc,sys,dev,tmp,run,root/.ssh,var/empty,lib,lib64,usr/lib/openssh,syzkaller}
cp "$BB" "$R/bin/busybox"
for a in $("$BB" --list); do [ "$a" = busybox ] || ln -sf /bin/busybox "$R/bin/$a"; done
ln -sf /bin/busybox "$R/sbin/init"

# sshd, scp (syzkaller copies with the legacy scp protocol, which runs scp on the target),
# sftp-server, ssh-keygen (host keys at first boot), and their libraries
for f in /usr/sbin/sshd /usr/bin/scp /usr/lib/openssh/sftp-server /usr/bin/ssh-keygen; do
  mkdir -p "$R$(dirname "$f")"; cp -L "$f" "$R$f"
  ldd "$f" | awk '/=>/ {print $3} /ld-linux/ {print $1}' | while read -r l; do
    [ -f "$l" ] && { mkdir -p "$R$(dirname "$l")"; cp -L "$l" "$R$l"; }
  done
done
cp "$KO" "$R/hvx.ko"
cp "$O/id_syz.pub" "$R/root/.ssh/authorized_keys"
chmod 700 "$R/root/.ssh"; chmod 600 "$R/root/.ssh/authorized_keys"
cat > "$R/etc/passwd" <<'EOF'
root:x:0:0:root:/root:/bin/sh
sshd:x:74:74:sshd:/var/empty:/bin/false
nobody:x:65534:65534:nobody:/:/bin/false
EOF
cat > "$R/etc/group" <<'EOF'
root:x:0:
sshd:x:74:
nogroup:x:65534:
EOF
echo 'root:*:19000:0:99999:7:::' > "$R/etc/shadow"; chmod 600 "$R/etc/shadow"
cat > "$R/etc/ssh/sshd_config" <<'EOF'
Port 22
PermitRootLogin prohibit-password
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
UseDNS no
Subsystem sftp /usr/lib/openssh/sftp-server
EOF
cat > "$R/etc/inittab" <<'EOF'
::sysinit:/etc/init.d/rcS
::respawn:/usr/sbin/sshd -D -e
ttyS0::respawn:/bin/sh
::ctrlaltdel:/bin/reboot -f
EOF
cat > "$R/etc/init.d/rcS" <<'EOF'
#!/bin/sh
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev 2>/dev/null
mkdir -p /dev/pts /dev/shm; mount -t devpts devpts /dev/pts; mount -t tmpfs shm /dev/shm
mount -t debugfs debugfs /sys/kernel/debug
mount -t tmpfs tmp /tmp; mount -t tmpfs run /run; mkdir -p /run/sshd; chmod 755 /run/sshd
mount -o remount,rw /
hostname hvx-syz
ifconfig lo 127.0.0.1 up
ifconfig eth0 10.0.2.15 netmask 255.255.255.0 up && route add default gw 10.0.2.2
[ -f /etc/ssh/ssh_host_ed25519_key ] || /usr/bin/ssh-keygen -A
insmod /hvx.ko && echo "hvx-syz: hvx.ko loaded" > /dev/kmsg
EOF
chmod 755 "$R/etc/init.d/rcS"
IMG="$O/image-$BUNDLE.ext4"
rm -f "$IMG"
fakeroot -- sh -c "chown -R 0:0 '$R' && mke2fs -q -t ext4 -L hvxsyz -d '$R' '$IMG' 1G"
echo "build-image: OK ($IMG, $(du -h "$IMG" | cut -f1) used)"
