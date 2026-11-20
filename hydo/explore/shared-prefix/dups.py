#!/usr/bin/env python3
"""Runs whose schedule repeats an earlier run of the same exploration exactly (after dropping
the dseed= that decides nothing for disk=none and disk=cut), by role, with their seconds.
Reruns and confirmations of minimal schedules repeat on purpose and are listed apart."""
import os, sys, json, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix

def norm(s):
    host = prefix.host_key(s, 'drop-dseed')
    guest = s.split('|', 1)[1].strip() if '|' in s else ''
    return host + ' | ' + guest

print('| exploration | role | runs | repeats of an earlier schedule | their s |')
print('|---|---|---|---|---|')
for d in sys.argv[1:]:
    seen = set()
    n = collections.Counter()
    rep = collections.Counter()
    secs = collections.Counter()
    with open(os.path.join(d, 'events.jsonl')) as f:
        for line in f:
            e = json.loads(line)
            if e.get('type') != 'run':
                continue
            k = norm(e['schedule'])
            r = e.get('role')
            n[r] += 1
            if k in seen:
                rep[r] += 1
                secs[r] += e.get('secs') or 0
            seen.add(k)
    for r in n:
        if r in ('rerun', 'minimal', 'error-minimal', 'probe'):
            continue
        print(f"| {os.path.basename(d)} | {r} | {n[r]} | {rep[r]} | {secs[r]:.0f} |")
