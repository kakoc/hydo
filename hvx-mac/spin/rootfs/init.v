#!/bin/sh
# /init of the hvm test initramfs: prove userspace runs, the clock works, timers fire, then idle.
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev 2>/dev/null
echo "hydo-hvm: userspace alive"
uname -a
echo "hydo-hvm: uptime $(cat /proc/uptime)"
echo "hydo-hvm: clocksource $(cat /sys/devices/system/clocksource/clocksource0/current_clocksource)"
/bin/cntvct
echo "hydo-hvm: compute $(seq 1 3000 | md5sum)"
/bin/spin_ldxr
sleep 1
echo "hydo-hvm: after sleep 1, uptime $(cat /proc/uptime)"
echo "hydo-boot-marker: launching shell"
exec sh
