#!/usr/bin/env python3
"""How well a run's guest clock predicts its wall seconds: the fit secs = a + b * clock over the
first runs of an exploration, with the correlation and the intercept's share of a mean run."""
import os, sys, statistics
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix

for d in sys.argv[1:]:
    runs = [r for r in prefix.load(d, False) if r['secs']]
    xs = [r['cost'] + r['rec'] for r in runs]
    ys = [r['secs'] for r in runs]
    if len(xs) < 3:
        continue
    mx, my = statistics.mean(xs), statistics.mean(ys)
    sxx = sum((x - mx) ** 2 for x in xs)
    sxy = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    b = sxy / sxx if sxx else 0
    a = my - b * mx
    r = statistics.correlation(xs, ys) if sxx else float('nan')
    print(f"{os.path.basename(d)}: n={len(xs)} mean run {my:.1f} s, r={r:.2f}, fixed part a={a:.1f} s ({100 * a / my:.0f} % of a mean run)")
