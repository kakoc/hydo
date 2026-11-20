#!/usr/bin/env python3
"""The branches of a state search with their fingerprints: tag, fp, schedule (tab-separated),
at most N of them. Usage: branches.py <state search dir> [N]"""
import json, os, sys

d = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 6
fp = {}
with open(os.path.join(d, 'events.jsonl')) as f:
    for line in f:
        e = json.loads(line)
        if e.get('type') == 'run':
            fp[e['tag']] = (e.get('fingerprints') or {}).get('fp')
out = []
with open(os.path.join(d, 'state-search.jsonl')) as f:
    for line in f:
        e = json.loads(line)
        if e.get('summary') or not e.get('from') or e.get('invalid'):
            continue
        if fp.get(e['tag']):
            out.append((e['tag'], fp[e['tag']], e['schedule']))
for t in out[:n]:
    print('\t'.join(t))
