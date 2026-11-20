#!/usr/bin/env python3
"""The models of models.py summed over several explorations, in wall seconds."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import models

T = {'secs': 0.0, 'now_s': 0.0, 'same-k_s': 0.0, 'any-k_s': 0.0}
for d in sys.argv[1:]:
    m = models.model(models.runs_of(d, False))
    print(f"{os.path.basename(d)}: now {m['now_s']:.0f} s, same-k {m['same-k_s']:.0f} s, any-k {m['any-k_s']:.0f} s of {m['secs']:.0f} s")
    for k in T:
        T[k] += m[k]
print(f"all: {T['secs']:.0f} s of first runs; now {100 * T['now_s'] / T['secs']:.2f} %, "
      f"same-k {100 * T['same-k_s'] / T['secs']:.1f} % ({T['same-k_s']:.0f} s), "
      f"any-k {100 * T['any-k_s'] / T['secs']:.1f} % ({T['any-k_s']:.0f} s)")
