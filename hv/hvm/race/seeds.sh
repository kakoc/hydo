#!/bin/sh
# seeds.sh <mem|clk> <permille> <first> <last>: one boot per seed with HVM_TJ=<seed>,<permille>,1000;
# prints "<seed> lost=<n> fp=<fingerprint>" per seed.
cd "$(dirname "$0")/.."
v=$1; pm=$2
seq $3 $4 | xargs -P 4 -I{} sh -c "r=\$(HVM_SOFT_IRQ=1 HVM_TJ=\$(printf %x {}),$pm,1000 ./hvm run explore/out/Image-explore-idle race/initramfs-race-$v.cpio.gz hydo-race-done 2>&1 | grep -a -o 'lost=[0-9-]*\|fp=[0-9a-f]*' | head -2 | tr '\n' ' '); echo \"{} \$r\""
