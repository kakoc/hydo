#!/usr/bin/env python3
"""Runs of one exploration whose schedules differ only in dseed=: are their transcripts the
same (every send, stop and acknowledgement), and are their fingerprints the same?"""
import os, sys, json, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix

d = sys.argv[1]
groups = collections.defaultdict(list)
with open(os.path.join(d, 'events.jsonl')) as f:
    for line in f:
        e = json.loads(line)
        if e.get('type') == 'run' and e.get('role') == 'first':
            key = prefix.host_key(e['schedule'], 'drop-dseed') + ' | ' + e['schedule'].split('|', 1)[1].strip()
            groups[key].append(e)

def body(tag):
    out = []
    with open(os.path.join(d, 'consoles', f'{tag}.control.jsonl')) as f:
        for i, line in enumerate(f):
            if i == 0:
                continue  # the header names the schedule
            out.append(line)
    return out

for key, es in sorted(groups.items(), key=lambda kv: -len(kv[1]))[:4]:
    if len(es) < 2:
        continue
    b0 = body(es[0]['tag'])
    same = sum(1 for e in es[1:] if body(e['tag']) == b0)
    fps = {e['fingerprints']['fp'] for e in es}
    wfps = {e['fingerprints'].get('wfp') for e in es}
    print(f"{key}: {len(es)} runs; transcripts equal to the first run's: {same} of {len(es) - 1}; "
          f"distinct fp {len(fps)}, distinct wfp {len(wfps)}")
