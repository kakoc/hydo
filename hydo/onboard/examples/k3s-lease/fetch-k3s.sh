#!/bin/sh
# fetch-k3s.sh: fill k3s/ next to hydo.yaml with what the image needs, every file checked:
#   k3s/k3s                 the k3s binary of the release below (sha256 pinned)
#   k3s/data/<hash>/        the files k3s unpacks from itself at its first start (containerd,
#                           runc, the CNI plugins, iptables, kubectl), unpacked here once, so
#                           that the guest does not unpack 270 MB in every run
#   k3s/images/*.tar        the pause image and busybox, pinned by digest (docker save format;
#                           k3s imports them at its start, the guest has no network)
#   k3s/modules/            the guest kernel's modules that k3s needs, with their dependencies,
#                           and load-order (k3s-setup.sh loads them in that order)
#   k3s/var-run             a link to /run, which becomes /var/run in the image
# Run it on a Linux x86_64 machine: it runs the k3s binary once to unpack its files (as the
# calling user, into a directory here; nothing is started). The modules come from the guest
# kernel's build output, the nixpkgs store path below: set KMODS to its lib/modules/6.6.35
# directory, or have Nix on PATH (nix-store -r fetches it from cache.nixos.org).
# Needs: curl, sha256sum, xz, and hydo-fetch-image (in the hydo release's bin/).
set -eu
cd "$(dirname "$0")"
V=v1.37.2+k3s1
K3S_SHA256=265b3ee6a3198ef14033c63ef46cbb32370d83795199fb6d1b01f21575eddbda
HASH=342d21a06834327c49d125902586b5c58038e4decca01ba829aa819ce4d1b586
KSTORE=/nix/store/rp8637p734k6rshqjqlnfdm97b4jn8w9-linux-6.6.35
MODULES="dummy overlay br_netfilter bridge veth vxlan nf_conntrack nf_nat xt_conntrack
xt_addrtype xt_comment xt_multiport xt_statistic xt_mark xt_MASQUERADE xt_nat xt_tcpudp
xt_physdev xt_set xt_limit xt_connmark xt_REDIRECT iptable_nat iptable_filter iptable_mangle
iptable_raw ipt_REJECT ip6_tables ip6table_filter ip6table_nat ip6table_mangle ip6t_REJECT ip_set
ip_set_hash_ip ip_set_hash_net ip_set_hash_ipport ip_set_bitmap_port nf_tables nft_compat
nft_chain_nat nft_ct nft_masq nft_nat nft_reject_inet nfnetlink"
umask 022
mkdir -p k3s/images k3s/data

# 1. the binary
if [ ! -f k3s/k3s ] || [ "$(sha256sum k3s/k3s | cut -d' ' -f1)" != "$K3S_SHA256" ]; then
  curl -sSfL -o k3s/k3s.part "https://github.com/k3s-io/k3s/releases/download/$(echo $V | sed 's/+/%2B/')/k3s"
  [ "$(sha256sum k3s/k3s.part | cut -d' ' -f1)" = "$K3S_SHA256" ] || { echo "k3s: wrong sha256" >&2; exit 1; }
  chmod 755 k3s/k3s.part && mv k3s/k3s.part k3s/k3s
fi

# 2. its files, unpacked once (k3s checks them against its own list of sha256 values)
if [ ! -x "k3s/data/$HASH/bin/k3s" ]; then
  rm -rf k3s/unpack
  ./k3s/k3s --data-dir "$PWD/k3s/unpack" kubectl version --client > /dev/null
  [ -d "k3s/unpack/data/$HASH" ] || { echo "k3s unpacked another hash than $HASH" >&2; exit 1; }
  rm -rf "k3s/data/$HASH" && mv "k3s/unpack/data/$HASH" k3s/data/ && rm -rf k3s/unpack
fi
(cd "k3s/data/$HASH/bin" && sha256sum -c --quiet .sha256sums)

# 3. the images, by digest
fetch() {   # repository digest tag file sha256-of-file
  if [ ! -f "k3s/images/$4" ] || [ "$(sha256sum "k3s/images/$4" | cut -d' ' -f1)" != "$5" ]; then
    hydo-fetch-image "$1" "$2" "$3" "k3s/images/$4"
    [ "$(sha256sum "k3s/images/$4" | cut -d' ' -f1)" = "$5" ] || { echo "$4: wrong sha256" >&2; exit 1; }
  fi
}
fetch docker.io/rancher/mirrored-pause \
  sha256:412c4a7219cb8a299a37337f3d87810c5340095322e15594a1637785adad0f17 \
  docker.io/rancher/mirrored-pause:3.10.2 pause-amd64.tar \
  5f7ab07a624422ff75a80046be3d2fed09e66f08e9f16b82d10a04e8a0a8c162
fetch docker.io/rancher/mirrored-library-busybox \
  sha256:ad9fa4d07136a83e69a54ef00102f579d04eba431932de3b0f098cc5d5948f9f \
  docker.io/rancher/mirrored-library-busybox:1.37.0 busybox-amd64.tar \
  cece7a7818d684ae701ba6a6f4d748054348a205ee788374ddd74e2bb320da86

# 4. the kernel modules, with their dependencies from modules.dep, dependencies first
if [ -z "${KMODS:-}" ]; then
  [ -d "$KSTORE" ] || nix-store -r "$KSTORE" > /dev/null
  KMODS=$KSTORE/lib/modules/6.6.35
fi
rm -rf k3s/modules && mkdir -p k3s/modules
seen=" "
visit() {   # one module and, first, what it depends on ($2: its line of modules.dep)
  case "$seen" in *" $1 "*) return ;; esac
  seen="$seen$1 "
  set -- "$1" "$(grep -E "/$1\.ko\.xz:" "$KMODS/modules.dep")"
  [ -n "$2" ] || { echo "no module $1 in $KMODS" >&2; exit 1; }
  for d in ${2#*:}; do visit "$(basename "$d" .ko.xz)"; done
  xz -dc "$KMODS/${2%%:*}" > "k3s/modules/$1.ko"
  echo "$1" >> k3s/modules/load-order
}
for m in $MODULES; do visit "$m"; done

# 5. /var/run
ln -sfn /run k3s/var-run
echo "k3s/: $(ls k3s/modules | grep -c '\.ko$') modules, $(ls k3s/images | wc -l) images, k3s $V"
