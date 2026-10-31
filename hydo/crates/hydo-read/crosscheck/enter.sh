#!/bin/bash
# On bare-metal: run "$@" (as ubuntu) inside the chroot ~/hydo-prd/chroot, in a private mount
# namespace (nothing is mounted outside it, nothing on the host changes), with the stores of
# inside.sh started around it.
#   ~/hydo-prd/cc/enter.sh <command...>
set -euo pipefail
P=/home/user/hydo-prd
C=$P/chroot
mkdir -p "$P/cc/tmp" "$C$P"
chmod 1777 "$P/cc/tmp"
exec sudo unshare -m --propagation private /bin/bash -c '
set -e
P=/home/user/hydo-prd; C=$P/chroot
mount --bind /usr "$C/usr"
mount --bind /etc "$C/etc"
mount --bind /dev "$C/dev"
mount -t proc proc "$C/proc"
mount --bind "$P/cc/tmp" "$C/tmp"
mount --bind "$P" "$C$P"
exec chroot "$C" /usr/bin/setpriv --reuid=1000 --regid=1000 --init-groups /usr/bin/env -i HOME=/home/user PATH=/usr/bin:/bin LANG=C LC_ALL=C USER=ubuntu /bin/bash '"$P"'/cc/inside.sh "$@"
' enter "$@"
