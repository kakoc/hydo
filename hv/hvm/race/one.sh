#!/bin/sh
# one.sh <variant> <strategy> <seed>: one boot (see cells.sh); prints "<seed> hit1=.. hit2=.. nvcsw=.. fp=.. [n made the timer due]"
cd "$(dirname "$0")/.."
s=$(printf %x "$3")
case $2 in
	jitter) e="HVM_TJ=$s,1000,1000";;
	pct*)   e="HVM_PCT=$s,${2#pct},14327,34800";;
	*)      e="X=1";;
esac
r=$(env HVM_SOFT_IRQ=1 $e ../../target/release/hydo-hvm hvm run explore/out/Image-explore-idle race/initramfs-race-$1.cpio.gz hydo-race-done 2>&1 |
    grep -a -o 'hit1=[0-9]* hit2=[0-9]* nvcsw=[0-9]*\|fp=[0-9a-f]*\|[0-9]* made the timer due' | head -3 | tr '\n' ' ')
echo "$3 $r"
