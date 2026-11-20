#!/usr/bin/env python3
"""Per exploration: the runs by role, where the failing runs' first fault falls, the host-part
tokens that differ from the start, and the shared clock among all runs (shrinks included).

Usage: extra.py <exploration dir>...
"""
import os, sys, json, collections, statistics
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix, models

def steps_of(d):
    for name in sorted(os.listdir(os.path.join(d, 'consoles'))):
        if name.endswith('.control.jsonl'):
            s = models.scan(os.path.join(d, 'consoles', name))
            if s['steps']:
                return s['steps']
    return None

print('| exploration | roles | failing first runs | their first point k/S (median, min) | failing with k/S < 0.25 | minimal schedules k/S | host tokens beyond disk/dseed | all runs: clock already run |')
print('|---|---|---|---|---|---|---|---|')
for d in sys.argv[1:]:
    roles = collections.Counter()
    fails = []
    hosttok = collections.Counter()
    S = steps_of(d)
    with open(os.path.join(d, 'events.jsonl')) as f:
        for line in f:
            e = json.loads(line)
            if e.get('type') != 'run':
                continue
            roles[e.get('role')] += 1
            for t in prefix.host_parts(e['schedule']):
                k = t.split('=', 1)[0]
                if k not in ('disk', 'dseed', 'rb'):
                    hosttok[k] += 1
            if e.get('role') == 'first' and e.get('verdict', {}).get('kind') not in ('ok', None):
                fp = prefix.first_point(e['schedule'])
                fails.append(S if fp is None else fp)
    mins = []
    ex = os.path.join(d, 'exploration.json')
    if os.path.exists(ex):
        x = json.load(open(ex))
        for s in x.get('shrinks') or []:
            sch = s.get('minimal') or s.get('schedule') or ''
            if '|' in sch:
                fp = prefix.first_point(sch)
                mins.append(S if fp is None else fp)
    runs = prefix.load(d, True)
    rows = prefix.analyse(runs, 'relaxed')
    tot = sum(r['tot'] for r in rows) or 1
    saved = sum(r['prev'] for r in rows)
    fr = [f / S for f in fails] if S else []
    print(f"| {os.path.basename(d)} | {dict(roles)} | {len(fails)} | "
          f"{(statistics.median(fr) if fr else float('nan')):.2f}, {(min(fr) if fr else float('nan')):.2f} | "
          f"{sum(1 for v in fr if v < 0.25)} | {[round(m / S, 2) for m in mins][:8] if S else mins} | "
          f"{dict(hosttok) or '-'} | {100 * saved / tot:.1f} % of {len(runs)} |")
