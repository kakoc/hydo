#!/bin/sh
# /init of the debug test VM (Milestone 97). Runs inside QEMU under the debug kernel: loads
# hvx.ko, runs the hand-written module tests and short real-Linux guest boots under hvx,
# unloads the module, asks kmemleak for leaks, and powers off. run-debug.sh reads the serial
# log; every line this script prints for the host starts with "SAFETY ".
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev
mount -t debugfs debugfs /sys/kernel/debug
mount -t tmpfs tmp /tmp
echo 8 > /proc/sys/kernel/printk
grep -o 'hvxtests=[^ ]*' /proc/cmdline | cut -d= -f2 | tr , ' ' > /tmp/only
T=$(cat /tmp/only)
TO=$(grep -o 'hvxto=[0-9]*' /proc/cmdline | cut -d= -f2); TO=${TO:-900}

# Leaks from boot are not ours: start the leak record from here.
echo clear > /sys/kernel/debug/kmemleak 2>/dev/null

echo "SAFETY BEGIN $(uname -r) cpus=$(grep -c ^processor /proc/cpuinfo)"
insmod /hvx/hvx.ko || { echo "SAFETY FATAL insmod rc=$?"; poweroff -f; }
echo "SAFETY insmod ok"

run() {	# name timeout cmd... ; prints the command's output prefixed, then its rc
	name=$1; to=$2; shift 2
	s=$(cut -d' ' -f1 /proc/uptime)
	timeout "$to" "$@" > /tmp/out 2>&1
	rc=$?
	e=$(cut -d' ' -f1 /proc/uptime)
	sed "s/^/SAFETY| $name: /" /tmp/out | tail -n 40
	echo "SAFETY RESULT $name rc=$rc secs=$(echo "$s $e" | awk '{printf "%d", $2-$1}') fps=$(grep -oE '\b[0-9a-f]{16}\b' /tmp/out | sort -u | tr '\n' ' ')"
}

for t in ${T:-$(cat /hvx/tests)}; do
	echo "SAFETY START $t"
	case $t in
	hvx_boot_verify)      run $t "$TO" /hvx/$t /guest/bzImage /guest/initramfs.cpio.gz ;;
	hvx_disk_boot_verify) run $t "$TO" /hvx/$t /guest/bzImage /guest/initramfs-disk.cpio.gz /guest/vblk-disk.img ;;
	hvx_linux)            run $t "$TO" env HVX_INPUT='echo hydo-interactive\n' /hvx/hvx_linux /guest/bzImage /guest/initramfs.cpio.gz ;;
	hvx_sched_sweep_test) run $t "$TO" /hvx/$t 2000 ;;
	*)                    run $t "$TO" /hvx/$t ;;
	esac
	# a splat during this test is attributed to it by run-debug.sh through this marker
	echo "SAFETY END $t"
done

rmmod hvx && echo "SAFETY rmmod ok" || echo "SAFETY rmmod FAILED"
# kmemleak needs two scans some seconds apart before it reports an object as leaked.
echo scan > /sys/kernel/debug/kmemleak; sleep 6; echo scan > /sys/kernel/debug/kmemleak
n=$(grep -c '^unreferenced object' /sys/kernel/debug/kmemleak)
echo "SAFETY KMEMLEAK objects=$n"
sed 's/^/SAFETY| kmemleak: /' /sys/kernel/debug/kmemleak | head -n 200
echo "SAFETY DONE"
poweroff -f
