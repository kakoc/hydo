#!/bin/sh
# fetch-k3s.sh: fill k3s/ next to this hydo.yaml for the image with `guest.kernel: containers`,
# every file checked by sha256:
#   k3s/k3s                 the k3s binary of the release below, for $ARCH
#   k3s/data/<hash>/        the files k3s unpacks from itself at its first start (containerd,
#                           runc, the CNI plugins, iptables, kubectl), unpacked here once
#   k3s/data-hash           <hash>, which k3s-setup.sh and workload.sh read
#   k3s/bin                 a link to /opt/k3s/data/<hash>/bin, which the pods mount (elect.yaml)
#   k3s/images/*.tar        the pause image and busybox for $ARCH, pinned by digest
#   k3s/var-run             a link to /run, which becomes /var/run in the image
# No kernel modules: the variant's guest kernel has every option built in.
# ARCH is arm64 (the Mac engine) or amd64 (the KVM engine); the default is this machine's. The
# k3s binary unpacks its files: on Linux of that architecture it runs here, elsewhere in a Docker
# container of that architecture (docker run --platform linux/$ARCH; nothing is started but k3s's
# own unpacking). Needs: curl, sha256sum or shasum, and hydo-fetch-image (in the release's bin/).
set -eu
cd "$(dirname "$0")"
V=v1.37.2+k3s1
case "${ARCH:-$(uname -m)}" in
  arm64|aarch64) ARCH=arm64 ;;
  amd64|x86_64) ARCH=amd64 ;;
  *) echo "ARCH: arm64 or amd64" >&2; exit 1 ;;
esac
if [ "$ARCH" = arm64 ]; then
  BIN=k3s-arm64
  K3S_SHA256=a779ccf1e5e2e33a62f5a29c2f1c0bbf99431e100579d4c10f0aa0742a6aa84b
  HASH=10bae91d040ed73349935c6d1ce72fe9eedd59996e8758da3b1177613eded6f7
  PAUSE=sha256:a433214620b407678934ebd69690a9ed6066247231eacfba181fab20dbc3dbf1
  PAUSE_SHA=55a15451674c6321e96d3c1f5e6183cc5b9ca29092541b3bcc3c2fcc70332cd9
  BUSYBOX=sha256:fa8dc70514f29471fe118446e3f84040b19791531ec197836a4f43baf13d744b
  BUSYBOX_SHA=e80fb1d47ffebde1666287bc3a1d3b42b7d43ad64eb3f377a3b3b2216e6a10b2
else
  BIN=k3s
  K3S_SHA256=265b3ee6a3198ef14033c63ef46cbb32370d83795199fb6d1b01f21575eddbda
  HASH=342d21a06834327c49d125902586b5c58038e4decca01ba829aa819ce4d1b586
  PAUSE=sha256:412c4a7219cb8a299a37337f3d87810c5340095322e15594a1637785adad0f17
  PAUSE_SHA=5f7ab07a624422ff75a80046be3d2fed09e66f08e9f16b82d10a04e8a0a8c162
  BUSYBOX=sha256:ad9fa4d07136a83e69a54ef00102f579d04eba431932de3b0f098cc5d5948f9f
  BUSYBOX_SHA=cece7a7818d684ae701ba6a6f4d748054348a205ee788374ddd74e2bb320da86
fi
sha() { if command -v sha256sum > /dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
umask 022
# another architecture's files go first, so that the image never mixes two
if [ -f k3s/arch ] && [ "$(cat k3s/arch)" != "$ARCH" ]; then rm -rf k3s; fi
mkdir -p k3s/images k3s/data
echo "$ARCH" > k3s/arch

# 1. the binary
if [ ! -f k3s/k3s ] || [ "$(sha k3s/k3s)" != "$K3S_SHA256" ]; then
  curl -sSfL -o k3s/k3s.part "https://github.com/k3s-io/k3s/releases/download/$(echo $V | sed 's/+/%2B/')/$BIN"
  [ "$(sha k3s/k3s.part)" = "$K3S_SHA256" ] || { echo "k3s: wrong sha256" >&2; exit 1; }
  chmod 755 k3s/k3s.part && mv k3s/k3s.part k3s/k3s
fi

# 2. its files, unpacked once (k3s checks them against its own list of sha256 values)
if [ ! -x "k3s/data/$HASH/bin/k3s" ]; then
  rm -rf k3s/unpack
  here=$(uname -s)-$(uname -m)
  case "$ARCH:$here" in
    arm64:Linux-aarch64|amd64:Linux-x86_64)
      ./k3s/k3s --data-dir "$PWD/k3s/unpack" kubectl version --client > /dev/null ;;
    *)
      # in a container, onto its own file system (a Mac's is not case-sensitive), then out as tar
      cid=$(docker create --platform "linux/$ARCH" --entrypoint /bin/sh \
        "docker.io/rancher/mirrored-library-busybox@$BUSYBOX" -c \
        'chmod 755 /k3s && /k3s --data-dir /u kubectl version --client > /dev/null 2>&1 && tar cf /u.tar -C /u data')
      docker cp k3s/k3s "$cid:/k3s"
      docker start -a "$cid" > /dev/null
      mkdir -p k3s/unpack && docker cp "$cid:/u.tar" - | tar xf - -O | tar xf - -C k3s/unpack
      docker rm "$cid" > /dev/null ;;
  esac
  [ -d "k3s/unpack/data/$HASH" ] || { echo "k3s unpacked another hash than $HASH" >&2; exit 1; }
  rm -rf "k3s/data/$HASH" && mv "k3s/unpack/data/$HASH" k3s/data/ && rm -rf k3s/unpack
fi
(cd "k3s/data/$HASH/bin" && while read -r s f; do [ "$(sha "$f")" = "$s" ] || { echo "$f: wrong sha256" >&2; exit 1; }; done < .sha256sums)
echo "$HASH" > k3s/data-hash
ln -sfn "/opt/k3s/data/$HASH/bin" k3s/bin

# 3. the images, by digest
fetch() {   # repository digest tag file sha256-of-file
  if [ ! -f "k3s/images/$4" ] || [ "$(sha "k3s/images/$4")" != "$5" ]; then
    hydo-fetch-image "$1" "$2" "$3" "k3s/images/$4"
    [ "$(sha "k3s/images/$4")" = "$5" ] || { echo "$4: wrong sha256" >&2; exit 1; }
  fi
}
fetch docker.io/rancher/mirrored-pause "$PAUSE" docker.io/rancher/mirrored-pause:3.10.2 "pause-$ARCH.tar" "$PAUSE_SHA"
fetch docker.io/rancher/mirrored-library-busybox "$BUSYBOX" docker.io/rancher/mirrored-library-busybox:1.37.0 \
  "busybox-$ARCH.tar" "$BUSYBOX_SHA"

# 4. /var/run
ln -sfn /run k3s/var-run
echo "k3s/: k3s $V for $ARCH, $(ls k3s/images | wc -l | tr -d ' ') images"
