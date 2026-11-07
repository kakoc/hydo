#!/bin/sh
# List every instruction of the KVM guest kernel that reads the host's time or randomness or a
# performance counter (RDTSC, RDTSCP, RDPID, RDRAND, RDSEED, RDPMC), with its function, from the
# vmlinux that hv/kvm/linux/kernel.nix leaves next to bzImage-kvm. docs/design/kvm-engine.md says
# why each site that remains is never reached in this engine's guest.
#
#   sh hv/kvm/linux/audit-rdtsc.sh <vmlinux> [objdump]
set -e
V=${1:?usage: audit-rdtsc.sh <vmlinux> [objdump]}
OBJDUMP=${2:-objdump}
"$OBJDUMP" -d --no-show-raw-insn "$V" |
  awk '/^[0-9a-f]+ <.*>:/ { f = $2 }
       /\t(rdtsc|rdtscp|rdpid|rdrand|rdseed|rdpmc)( |$)/ { print f, $0; n++ }
       END { print (n + 0) " sites" }'
