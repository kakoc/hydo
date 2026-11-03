#!/bin/sh
# Build and ad-hoc sign the HVF capability probes of PROBE.md (needs the hypervisor entitlement).
# The Mac engine is the Rust program hydo-hvm (hydo/crates/hydo-hvm; the C hvm.c was deleted in M147).
set -e
cd "$(dirname "$0")"
/usr/bin/clang -O1 -Wall -arch arm64 -march=armv8.5-a+rng -o probe probe.c guest.s -framework Hypervisor
codesign --force --entitlements hv.entitlements -s - probe
