#!/bin/bash
# On bare-metal, as ubuntu: a chroot tree with the stores the examples stage into their guests
# (MySQL 8.4.10 and the JDK + Kafka 3.5.0 libs from the stack image, PostgreSQL 15.6 from the
# miniflux image, the static Redis 7.2.4 of the stack image) and etcd 3.5.6 from its release
# tarball (sha256 pinned as in hydo/readers/native-test.sh). Hard links, no copies.
set -euo pipefail
P=$HOME/hydo-prd
X=$P/x
C=$P/chroot
mkdir -p "$C/nix/store" "$C/opt/kafka" "$C/opt/redis" "$C/opt/etcd" "$C/usr" "$C/etc" "$C/proc" "$C/dev" "$C/tmp" "$C/home" "$C/run" "$C/sys" "$C/var"
for d in bin lib lib64 sbin; do [ -e "$C/$d" ] || ln -s "usr/$d" "$C/$d"; done
for r in stack-read-fix-root miniflux-read-root; do
  for e in "$X/$r/nix/store/"*; do
    n=$(basename "$e")
    [ -e "$C/nix/store/$n" ] || cp -al "$e" "$C/nix/store/$n"
  done
done
[ -e "$C/opt/kafka/libs" ] || cp -al "$X/stack-read-fix-root/opt/kafka/libs" "$C/opt/kafka/libs"
cp -f "$X/stack-read-fix-root/bin/redis-server" "$X/stack-read-fix-root/bin/redis-cli" "$C/opt/redis/" 2>/dev/null || cp -f "$X/stack-read-fix-root/bin/redis-server" "$C/opt/redis/"
V=3.5.6
SHA=4db32e3bc06dd0999e2171f76a87c1cffed8369475ec7aa7abee9023635670fb
T=$P/dl/etcd-v$V-linux-amd64.tar.gz
mkdir -p "$P/dl"
[ -f "$T" ] || curl -sSfL --max-time 300 -o "$T" "https://github.com/etcd-io/etcd/releases/download/v$V/etcd-v$V-linux-amd64.tar.gz"
echo "$SHA  $T" | sha256sum -c - >/dev/null
[ -x "$C/opt/etcd/etcd" ] || tar -xzf "$T" -C "$C/opt/etcd" --strip-components=1
ls "$C/nix/store" | wc -l
echo setup-ok
