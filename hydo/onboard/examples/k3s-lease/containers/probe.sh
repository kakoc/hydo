#!/bin/sh
# probe.sh: does the guest kernel have what containers need? With k3s's own tools (runc, the
# iptables builds, ipset, busybox), without k3s: cgroup v2 and its controllers, a container that
# runc starts in its own namespaces with memory and pids limits, overlayfs, NAT through
# iptables-legacy and nftables, connection tracking, bridged traffic through iptables, ipsets.
# One line per check on the console ("probe: ok ..." or "probe: FAIL ..."), then a summary that
# probe.yaml's invariant reads.
set -u
H=$(cat /opt/k3s/data-hash)
B=/opt/k3s/data/$H/bin
export PATH=$B:$B/aux:$PATH
n=0; fails=0
say() { echo "probe: $*" > /dev/console; echo "probe: $*" >> /data/probe.log; }
check() {   # <name> <command...>
  n=$((n + 1)); name=$1; shift
  if out=$("$@" 2>&1); then say "ok $name"; else fails=$((fails + 1)); say "FAIL $name: $(echo "$out" | tail -2 | tr '\n' ' ')"; fi
}
: > /data/probe.log
mountpoint -q /sys/fs/cgroup || mount -t cgroup2 cgroup2 /sys/fs/cgroup
mountpoint -q /run || mount -t tmpfs -o mode=0755 tmpfs /run
say "uname $(uname -r -m), controllers: $(cat /sys/fs/cgroup/cgroup.controllers)"
for c in cpu memory pids; do check "cgroup controller $c" grep -qw $c /sys/fs/cgroup/cgroup.controllers; done

overlay() {
  mkdir -p /data/ov/l /data/ov/u /data/ov/w /data/ov/m && echo low > /data/ov/l/f &&
    mount -t overlay overlay -o lowerdir=/data/ov/l,upperdir=/data/ov/u,workdir=/data/ov/w /data/ov/m &&
    echo up >> /data/ov/m/f && grep -q up /data/ov/u/f && umount /data/ov/m
}
check "overlayfs" overlay

# a container: busybox's image unpacked as the root, runc's default spec with limits
R=/data/runc-b
unpack() {
  rm -rf "$R" && mkdir -p "$R/img" "$R/rootfs" && tar -xf /opt/k3s/images/busybox-*.tar -C "$R/img" &&
    for l in $(sed -n 's/.*"Layers": *\[\([^]]*\)\].*/\1/p' "$R/img/manifest.json" | tr -d '" ' | tr ',' ' '); do
      gzip -dc "$R/img/$l" | tar -xf - -C "$R/rootfs" || return 1
    done && test -e "$R/rootfs/bin/sh" || { ls -la "$R/img" "$R/rootfs" "$R/rootfs/bin" 2>&1 | head -12; return 1; }
}
check "unpack busybox" unpack
cat > /data/runc.sed <<'EOF'
s/"terminal": true/"terminal": false/
s/"resources": {/"resources": {"memory": {"limit": 67108864}, "pids": {"limit": 64},/
s|"sh"$|"sh", "-c", "echo in-container; hostname; cat /sys/fs/cgroup/memory.max /sys/fs/cgroup/pids.max; echo pid $$"|
EOF
spec() { cd "$R" && runc spec && sed -i -f /data/runc.sed config.json && grep -q 67108864 config.json; }
check "runc spec" spec
# runc moves the container's root with pivot_root, which the kernel refuses when the current root
# is the initramfs (the Mac engine's images); there runc must copy the root's mount instead
NOPIVOT=
case $(awk '$2 == "/" { print $3; exit }' /proc/mounts) in rootfs|tmpfs|ramfs) NOPIVOT=--no-pivot ;; esac
say "the root is $(awk '$2 == "/" { print $1, $3; exit }' /proc/mounts)${NOPIVOT:+: runc runs with --no-pivot}"
container() {
  cd "$R" && runc --root /run/runc run $NOPIVOT probe > "$R/out" 2>&1
  sed 's/^/probe: container: /' "$R/out" > /dev/console
  grep -q in-container "$R/out" && grep -qx 67108864 "$R/out" && grep -qx 64 "$R/out" && grep -qx 'pid 1' "$R/out" && grep -qx runc "$R/out"
}
check "runc run (namespaces, memory and pids limits)" container

check "iptables-legacy NAT" iptables-legacy -t nat -A POSTROUTING -s 10.42.0.0/16 -j MASQUERADE
check "iptables-legacy conntrack match" iptables-legacy -A FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
check "iptables-legacy comment, multiport, statistic, mark" iptables-legacy -t nat -A PREROUTING -p tcp -m comment \
  --comment probe -m multiport --dports 80,443 -m statistic --mode random --probability 0.5 -j MARK --set-xmark 0x4000/0x4000
check "iptables-nft NAT" iptables-nft -t nat -A POSTROUTING -s 10.42.0.0/16 -j MASQUERADE
check "ip6tables-legacy NAT" ip6tables-legacy -t nat -A POSTROUTING -j MASQUERADE
check "connection tracking" test -r /proc/sys/net/netfilter/nf_conntrack_max
bridged() { ip link add probe0 type bridge && test -r /proc/sys/net/bridge/bridge-nf-call-iptables && ip link del probe0; }
check "bridged traffic through iptables" bridged
ipsets() { ipset create probe hash:ip && ipset add probe 10.42.0.1 && ipset destroy probe; }
check "ipset" ipsets
vxlan() { ip link add probev type vxlan id 1 dstport 8472 dev lo && ip link del probev; }
check "vxlan" vxlan
say "summary $n checks, $fails failed"
