#!/bin/sh
# Runs before every start of the k3s service (also after a kill and in a recovery boot), as root.
# It makes the guest look like a small Linux host to k3s: kernel modules, cgroup v2, writable
# state directories on the data disk, a network interface with a default route, and the
# pre-extracted k3s files. Every step is idempotent.
set -u
H=$(cat /opt/k3s/data-hash)
export PATH=/opt/k3s/data/$H/bin:/opt/k3s/data/$H/bin/aux:$PATH
say() { echo "hydo-k3s: $*" > /dev/console; }
say "setup at uptime $(cut -d' ' -f1 /proc/uptime)"

# 1. kernel modules, dependencies first (the list comes from modules.dep at build time)
if [ ! -e /sys/module/br_netfilter ]; then
  while read -r m; do
    [ -e "/sys/module/$m" ] || insmod "/lib/modules/k3s/$m.ko" 2>/dev/null || say "insmod $m failed"
  done < /lib/modules/k3s/load-order
fi

# 2. cgroup v2 on /sys/fs/cgroup
mountpoint -q /sys/fs/cgroup || mount -t cgroup2 cgroup2 /sys/fs/cgroup

# 3. writable state: the root is read-only, so each directory k3s writes is the data disk's
for d in var/lib/rancher var/lib/kubelet var/lib/cni var/log var/tmp etc/rancher; do
  src=/data/k3s-state/$(echo "$d" | tr / _)
  mkdir -p "$src"
  mountpoint -q "/$d" || mount --bind "$src" "/$d"
done
mountpoint -q /run || mount -t tmpfs -o mode=0755 tmpfs /run
mount --make-rshared / 2>/dev/null

# 4. the pre-extracted k3s files: the data directory's entry links to the read-only copy
mkdir -p /var/lib/rancher/k3s/data /var/lib/rancher/k3s/agent/images
# k3s writes into it (iptables-detect.sh makes links in bin/aux), so it is an overlay: the
# read-only copy below, the changes on the data disk
D=/var/lib/rancher/k3s/data/$H
mkdir -p "$D" /data/k3s-state/data-upper /data/k3s-state/data-work
mountpoint -q "$D" || mount -t overlay overlay \
  -o "lowerdir=/opt/k3s/data/$H,upperdir=/data/k3s-state/data-upper,workdir=/data/k3s-state/data-work" "$D"
for f in /opt/k3s/images/*.tar; do
  [ -e "/var/lib/rancher/k3s/agent/images/${f##*/}" ] || ln -s "$f" /var/lib/rancher/k3s/agent/images/
done
mkdir -p /var/lib/rancher/k3s/server/manifests
for f in /opt/k3s/manifests/*.yaml; do
  [ -e "$f" ] || continue
  [ -e "/var/lib/rancher/k3s/server/manifests/${f##*/}" ] || cp "$f" /var/lib/rancher/k3s/server/manifests/
done

# 5. a network interface with an address and a default route (the guest has only lo)
if ! ip link show k3s0 > /dev/null 2>&1; then
  ip link add k3s0 type dummy
  ip addr add 10.77.0.2/24 dev k3s0
  ip link set k3s0 up
  ip route add default via 10.77.0.1 dev k3s0 onlink
fi
hostname hydo-k3s
grep -q hydo-k3s /etc/hosts 2>/dev/null || say "note: /etc/hosts has no hydo-k3s"
say "setup done at uptime $(cut -d' ' -f1 /proc/uptime)"
