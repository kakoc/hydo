#!/usr/bin/env python3
"""For runs with the same first split point, print where two of them first differ."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix

d = sys.argv[1]
limit = int(sys.argv[2]) if len(sys.argv) > 2 else 3
runs = prefix.load(d, '--all-roles' in sys.argv)
by = {}
for r in runs:
    by.setdefault(prefix.first_point(r['sched']), []).append(r)
shown = 0
for fp, rs in sorted(by.items(), key=lambda x: (x[0] is None, x[0] or 0)):
    if len(rs) < 2:
        continue
    a, b = rs[0], rs[1]
    print(f"first point {fp}: {a['tag']} | {a['sched']}\n               {b['tag']} | {b['sched']}")
    for i, (x, y) in enumerate(zip(a['events'], b['events'])):
        if x != y:
            print(f"   differ at send {i}, clock +{x[0]-a['a0']} of {a['cost']} / +{y[0]-b['a0']}:")
            print(f"     A {x[1]} {x[2][:300]!r}")
            print(f"     B {y[1]} {y[2][:300]!r}")
            break
    shown += 1
    if shown >= limit:
        break
