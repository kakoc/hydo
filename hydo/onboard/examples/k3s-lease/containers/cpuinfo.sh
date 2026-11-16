#!/bin/sh
# The KVM engine's guest kernel does not know the CPU's clock rate, so its /proc/cpuinfo has no
# "cpu MHz" line, and the kubelet stops at start ("could not detect clock speed", Milestone 274).
# Give the kubelet a /proc/cpuinfo with a fixed rate: the file with the line after each
# "model name", bound over /proc/cpuinfo. Where the line is there (hvx), nothing changes.
grep -q '^cpu MHz' /proc/cpuinfo && exit 0
awk '{ print } /^model name/ { print "cpu MHz\t\t: 2000.000" }' /proc/cpuinfo > /tmp/hydo-cpuinfo
mount --bind /tmp/hydo-cpuinfo /proc/cpuinfo
