#!/usr/bin/env python3
"""The shrink candidates' shared beginnings: with any earlier run (an upper bound, a tree of
snapshots over every run), and with the failing run they come from (the run of the seed whose
dseed= the candidate keeps: one snapshot path, the failing run's)."""
import os, sys, json, re
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix

SHRINK = ('shrink', 'error-shrink')
print('| exploration | candidates | candidate s | with any earlier run | with the failing run | its s | with the last failing candidate |')
print('|---|---|---|---|---|---|')
for d in sys.argv[1:]:
    role = {}
    with open(os.path.join(d, 'events.jsonl')) as f:
        for line in f:
            e = json.loads(line)
            if e.get('type') == 'run':
                role[e['tag']] = e.get('role')
    runs = prefix.load(d, True)
    rows = {x['tag']: x for x in prefix.analyse(runs, 'relaxed')}
    def dseed(r):
        m = re.search(r'dseed=([0-9a-f]+)', r['sched'])
        return m.group(1) if m else None
    origin = {}
    for r in runs:
        if role.get(r['tag']) in ('first', 'rerun') and dseed(r):
            origin.setdefault(dseed(r), r)
    verdict = {}
    with open(os.path.join(d, 'events.jsonl')) as f:
        for line in f:
            e = json.loads(line)
            if e.get('type') == 'run':
                verdict[e['tag']] = (e.get('verdict') or {}).get('kind')
    n = tot = any_ = par = secs = par_s = kept = 0
    best = {}
    for r in runs:
        if role.get(r['tag']) in ('first', 'rerun') and verdict.get(r['tag']) != 'ok':
            best.setdefault(dseed(r), r)
        if role.get(r['tag']) not in SHRINK:
            continue
        b = best.get(dseed(r))
        if b is not None and prefix.host_key(b['sched'], 'relaxed') == prefix.host_key(r['sched'], 'relaxed'):
            kept += prefix.lcp_clock(r, b) - r['a0']
        if verdict.get(r['tag']) != 'ok':
            best[dseed(r)] = r
        x = rows[r['tag']]
        n += 1
        tot += x['tot']
        any_ += x['prev']
        secs += r['secs']
        o = origin.get(dseed(r))
        if o is not None and prefix.host_key(o['sched'], 'relaxed') == prefix.host_key(r['sched'], 'relaxed'):
            c = prefix.lcp_clock(r, o) - r['a0']
            par += c
            par_s += r['secs'] * c / x['tot'] if x['tot'] else 0
    if n:
        print(f"| {os.path.basename(d)} | {n} | {secs:.0f} | {100 * any_ / tot:.0f} % | {100 * par / tot:.0f} % | ~{par_s:.0f} s | {100 * kept / tot:.0f} % |")
