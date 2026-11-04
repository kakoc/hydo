#!/usr/bin/env bash
# native-test.sh on the bare-metal machine, which has no /nix: the stores' closures are taken
# out of the recorded example images (MySQL 8.4.10, the JDK and the Kafka 3.5.0 libs and the
# static Redis from explore-ob-stack-read-fix, PostgreSQL 15.6 from explore-ob-miniflux-read),
# etcd 3.5.6 from its release tarball (sha256 pinned), and native-test.sh runs in a chroot that
# has them at their /nix/store paths, in a private mount namespace (sudo, nothing on the host
# changes; it runs as the calling user):
#   hydo/crates/hydo-read/bare-metal.sh <work dir> <command...>
# for example the store tests, cross-built anywhere (`cargo test --release --no-run --target
# x86_64-unknown-linux-musl`, see build.sh for the flags) and run as `.../stores --test-threads=1`.
# The work dir (for example ~/hydo-prd) holds x/ (the images' root trees) and chroot/; the images
# come from $HYDO_READ_IMAGES (default ~/hydo-read/build). The repository and the work dir must
# be under /home. Ports: HYDO_TEST_PORTBASE (default 47000 here).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
[ $# -ge 2 ] || { sed -n '2,13p' "$0" >&2; exit 2; }
P="$(mkdir -p "$1" && cd "$1" && pwd)"; shift
IMG="${HYDO_READ_IMAGES:-$HOME/hydo-read/build}"
X=$P/x
C=$P/chroot
mkdir -p "$X"
for img in stack-read-fix miniflux-read; do
  [ -d "$X/$img-root/nix" ] && continue
  src="$IMG/explore-ob-$img-disk.img"
  read -r s n < <(sfdisk -d "$src" | awk '/start=/{gsub(/[=,]/," "); print $4, $6; exit}')
  dd if="$src" of="$X/$img-p1.img" bs=512 skip="$s" count="$n" status=none
  rm -rf "$X/$img-root"; mkdir "$X/$img-root"
  debugfs -R "rdump / $X/$img-root" "$X/$img-p1.img" 2>/dev/null
  rm -f "$X/$img-p1.img"
done
mkdir -p "$C/nix/store" "$C/opt/kafka" "$C/opt/redis" "$C/opt/etcd" "$C/usr" "$C/etc" "$C/proc" "$C/dev" "$C/tmp" "$C/home"
for d in bin lib lib64 sbin; do [ -e "$C/$d" ] || [ -L "$C/$d" ] || ln -s "usr/$d" "$C/$d"; done
for r in stack-read-fix-root miniflux-read-root; do
  for e in "$X/$r/nix/store/"*; do
    [ -e "$C/nix/store/$(basename "$e")" ] || cp -al "$e" "$C/nix/store/"
  done
done
[ -e "$C/opt/kafka/libs" ] || cp -al "$X/stack-read-fix-root/opt/kafka/libs" "$C/opt/kafka/libs"
cp -f "$X/stack-read-fix-root/bin/redis-server" "$X/stack-read-fix-root/bin/redis-cli" "$C/opt/redis/"
V=3.5.6
SHA=4db32e3bc06dd0999e2171f76a87c1cffed8369475ec7aa7abee9023635670fb
T=$P/dl/etcd-v$V-linux-amd64.tar.gz
mkdir -p "$P/dl" "$P/tmp"
[ -f "$T" ] || curl -sSfL --max-time 300 -o "$T" "https://github.com/etcd-io/etcd/releases/download/v$V/etcd-v$V-linux-amd64.tar.gz"
echo "$SHA  $T" | sha256sum -c - >/dev/null
[ -x "$C/opt/etcd/etcd" ] || tar -xzf "$T" -C "$C/opt/etcd" --strip-components=1
chmod 1777 "$P/tmp"
U=$(id -u); G=$(id -g)
exec sudo unshare -m --propagation private /bin/bash -c '
set -e
C=$1; P=$2; U=$3; G=$4; H=$5; PB=$6; shift 6
mount --bind /usr "$C/usr"; mount --bind /etc "$C/etc"; mount --bind /dev "$C/dev"
mount -t proc proc "$C/proc"; mount --bind "$P/tmp" "$C/tmp"; mount --bind /home "$C/home"
exec chroot "$C" /usr/bin/setpriv --reuid="$U" --regid="$G" --init-groups /usr/bin/env -i \
  HOME="$H" PATH=/usr/bin:/bin LANG=C LC_ALL=C USER="$(id -un "$U")" HYDO_TEST_PORTBASE="$PB" \
  HYDO_KAFKA_LIBS=/opt/kafka/libs HYDO_REDIS_SERVER=/opt/redis/redis-server \
  HYDO_REDIS_CLI=/opt/redis/redis-cli HYDO_ETCD=/opt/etcd/etcd "$@"
' metal "$C" "$P" "$U" "$G" "$HOME" "${HYDO_TEST_PORTBASE:-47000}" /bin/bash "$HERE/native-test.sh" "$@"
