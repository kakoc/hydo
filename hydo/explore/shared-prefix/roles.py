#!/usr/bin/env python3
"""The clock that an earlier run already ran, by the role of the later run (first, rerun,
shrink, error-shrink, minimal), over all runs of an exploration in tag order."""
import os, sys, json, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix

for d in sys.argv[1:]:
    role = {}
    with open(os.path.join(d, 'events.jsonl')) as f:
        for line in f:
            e = json.loads(line)
            if e.get('type') == 'run':
                role[e['tag']] = e.get('role')
    runs = prefix.load(d, True)
    # the explorer's order: first runs by seed, then reruns, shrinks and the rest by tag
    rows = prefix.analyse(runs, 'relaxed')
    tot = collections.Counter()
    sav = collections.Counter()
    secs = collections.Counter()
    for r in rows:
        k = role.get(r['tag'], '?')
        tot[k] += r['tot']
        sav[k] += r['prev']
        secs[k] += r['secs']
    parts = [f"{k} {100 * sav[k] / tot[k]:.0f} % of {secs[k]:.0f} s" for k in tot if tot[k]]
    print(f"{os.path.basename(d)}: " + '; '.join(parts))
