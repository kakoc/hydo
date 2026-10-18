#!/bin/sh
# Build and ad-hoc sign the HVF probes (needs the hypervisor entitlement).
set -e
cd "$(dirname "$0")"
/usr/bin/clang -O1 -Wall -arch arm64 -march=armv8.5-a+rng -o probe probe.c guest.s -framework Hypervisor
codesign --force --entitlements hv.entitlements -s - probe
/usr/bin/clang -O2 -Wall -arch arm64 -o hvm hvm.c -framework Hypervisor
codesign --force --entitlements hv.entitlements -s - hvm
