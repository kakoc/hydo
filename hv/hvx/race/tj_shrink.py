#!/usr/bin/env python3
"""Shrink a failing tick-jitter seed of the calibration race to the fewest delayed ticks.

1. narrow the jittered tick window [lo, hi) while the race still fails;
2. turn the delays inside the window into an explicit (tick, delay) list (same
   formula as hvx_tj_delay in hvx.c);
3. drop list entries one at a time while it still fails.
Usage: tj_shrink.py W SEED [MAX] [PERMILLE]
"""
import os, re, subprocess, sys

W, SEED = int(sys.argv[1]), int(sys.argv[2], 0)
MAX = int(sys.argv[3]) if len(sys.argv) > 3 else 200000
PM = int(sys.argv[4]) if len(sys.argv) > 4 else 1000
B = os.path.expanduser("~/hydo/build")
CL = f"console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off x2apic_phys hydo_race={W},50000,5000"
M64 = (1 << 64) - 1
runs = 0

def mix(x):
    x = (x + 0x9e3779b97f4a7c15) & M64
    x = ((x ^ (x >> 30)) * 0xbf58476d1ce4e5b9) & M64
    x = ((x ^ (x >> 27)) * 0x94d049bb133111eb) & M64
    return x ^ (x >> 31)

def delay(k):
    if k <= 1:
        return 0
    h = mix((SEED + k * 0x9e3779b97f4a7c15) & M64)
    return 0 if h % 1000 >= PM else 1 + (h >> 16) % MAX

def run(extra):
    global runs
    runs += 1
    env = ["HVX_DEV=/dev/hvxtj", "HVX_PMU_CLOCK=1"] + [f"{k}={v}" for k, v in extra.items()]
    out = subprocess.run([os.path.expanduser("~/bin/hvx-run"), "sudo", "env"] + env +
                         ["./hvx_race_probe", f"{B}/bzImage", f"{B}/initramfs-race.cpio.gz", CL],
                         capture_output=True, text=True,
                         cwd=os.path.expanduser("~/hydo/hvx/race")).stdout
    m = re.search(r"lost=(-?\d+).*fp=([0-9a-f]+) ticks=(\d+)", out)
    return (int(m.group(1)), m.group(2), int(m.group(3))) if m else (None, None, None)

def seeded(lo, hi):
    return run({"HVX_TJ_SEED": SEED, "HVX_TJ_MAX": MAX, "HVX_TJ_PERMILLE": PM,
                "HVX_TJ_LO": lo, "HVX_TJ_HI": hi})

def explicit(pairs):
    return run({"HVX_TJ_EXPLICIT": ",".join(f"{k}:{d}" for k, d in pairs)})

lost, fp, ticks = seeded(0, 0)
print(f"seed {SEED}: lost={lost} ticks={ticks} fp={fp}")
assert lost and lost > 0, "seed does not fail"
# 1a. smallest hi (with lo = 0) that still fails
lo_h, hi_h = 2, ticks + 2
while lo_h < hi_h:
    mid = (lo_h + hi_h) // 2
    l = seeded(0, mid)[0]
    if l and l > 0: hi_h = mid
    else: lo_h = mid + 1
hi = lo_h
# 1b. largest lo that still fails with that hi
a, b = 0, hi - 1
while a < b:
    mid = (a + b + 1) // 2
    l = seeded(mid, hi)[0]
    if l and l > 0: a = mid
    else: b = mid - 1
lo = a
pairs = [(k, delay(k)) for k in range(max(lo, 2), hi) if delay(k)]
print(f"window [{lo},{hi}): {len(pairs)} delayed ticks; seeded lost={seeded(lo, hi)[0]}")
if len(pairs) > 32:
    print("too many delayed ticks for an explicit list; stopping at the window")
    sys.exit(0)
l = explicit(pairs)[0]
print(f"explicit list of {len(pairs)}: lost={l}")
# 2. drop entries one at a time
i = 0
while i < len(pairs):
    trial = pairs[:i] + pairs[i + 1:]
    l = explicit(trial)[0] if trial else 0
    if l and l > 0: pairs = trial
    else: i += 1
r1, r2 = explicit(pairs), explicit(pairs)
print(f"MINIMAL: {len(pairs)} delayed tick(s): {pairs}")
print(f"  run 1: lost={r1[0]} fp={r1[1]}  run 2: lost={r2[0]} fp={r2[1]}  -> {'IDENTICAL' if r1[1]==r2[1] else 'MISMATCH'}")
print(f"  with no jitter at all: lost={run({})[0]}   ({runs} boots used)")
