#!/usr/bin/env python3
"""Shared beginnings of an exploration's runs, measured from their control transcripts.

Usage: prefix.py <exploration dir> [--all-roles] [--json]

A run's inputs to the guest are its engine's host part (armed when the run starts from S1) and
the commands the host controller sends, each at a clock value (`at`). Two runs with the same host
part and the same sends at the same clock values are the same run up to their first differing
send: the guest ran identically until that clock value. So the time a run shares with an earlier
run ends at the clock value of its first send that differs from every earlier run's.

The script builds a prefix tree of all runs, in their tag order (the explorer's order), and
reports for each host-part policy:
  - share: the part of each run's guest clock that an earlier run already ran (the saving
    with a snapshot at every point where runs part);
  - nearest: the part of each run's guest clock that it shares with the run nearest to it.
"""
import json, os, re, sys, statistics, collections

def host_parts(sched):
    host = sched.split('|')[0].split()
    return host

def host_key(sched, policy):
    """The part of the host tokens that two runs must share to share a beginning.
    strict: every host token but the cadence rb=; drop-dseed: without the dseed= of disk=none
    and disk=cut; relaxed: also disk=cut counted as disk=none (an engine that switched the
    disk's policy at a branch, which no engine does today)."""
    toks = [t for t in host_parts(sched) if not t.startswith('rb=')]
    if policy == 'strict':
        return ' '.join(toks)
    # relaxed: before the first disk request that the policy decides, disk=none and disk=cut
    # look the same to the guest, and dseed= decides nothing for them (no torn=)
    kv = dict(t.split('=', 1) for t in toks if '=' in t)
    disk = kv.get('disk', 'none')
    block = any(k in kv for k in ('torn', 'mis', 'lse', 'hang'))
    out = []
    for t in toks:
        k = t.split('=', 1)[0]
        if k == 'disk' and disk == 'cut' and policy == 'relaxed':
            out.append('disk=none')
            continue
        if k == 'dseed' and disk in ('none', 'cut') and not block:
            continue
        out.append(t)
    return ' '.join(out)

STEP_RE = re.compile(r'@(\d+)')

def first_point(sched):
    """The smallest step a guest token names, the step boundary where the run's ranges first
    split (C= counts too); None when no token names a step."""
    guest = sched.split('|', 1)[1].split()[1:] if '|' in sched else []
    pts = []
    for t in guest:
        k, _, v = t.partition('=')
        if k == 'C':
            try:
                pts.append(int(v))
            except ValueError:
                pass
        elif k == 'T':
            try:
                pts.append(int(v.split(':')[0]))
            except ValueError:
                pass
        elif k in ('W', 'R', 'I', 'E'):
            continue
        else:
            m = STEP_RE.search(v)
            if m:
                pts.append(int(m.group(1)))
    return min(pts) if pts else None

def read_transcript(path):
    events = []
    first_at = last_at = None
    sched = None
    with open(path) as f:
        for line in f:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            if 'schedule' in e and sched is None:
                sched = e['schedule']
            at = e.get('at')
            if at is not None:
                if first_at is None:
                    first_at = at
                last_at = at
            if e.get('t') == 'send':
                events.append((at, e.get('by'), e.get('bytes')))
    return sched, first_at, last_at, events

def load(d, all_roles):
    runs = {}
    with open(os.path.join(d, 'events.jsonl')) as f:
        for line in f:
            e = json.loads(line)
            if e.get('type') != 'run':
                continue
            if not all_roles and e.get('role') != 'first':
                continue
            runs[e['tag']] = e
    out = []
    for tag, e in runs.items():
        p = os.path.join(d, 'consoles', f'{tag}.control.jsonl')
        if not os.path.exists(p):
            continue
        sched, a0, a1, ev = read_transcript(p)
        if a0 is None:
            continue
        cost = a1 - a0
        rp = os.path.join(d, 'consoles', f'{tag}.control.recovery.jsonl')
        rec = 0
        if os.path.exists(rp):
            _, r0, r1, _ = read_transcript(rp)
            if r0 is not None:
                rec = r1 - r0
        out.append(dict(tag=tag, seed=e.get('seed'), sched=e['schedule'], a0=a0, a1=a1,
                        cost=cost, rec=rec, events=ev, secs=e.get('secs') or 0.0,
                        workload_start=next((x[0] for x in ev if x[2] and '"HYDO_FROM=' in x[2]), None)))
    def order(r):
        m = re.search(r'(\d+)$', r['tag'])
        return (r['tag'].rstrip('0123456789'), int(m.group(1)) if m else 0, r['tag'])
    # the order of events.jsonl, the order in which the explorer recorded the results
    rank = {t: i for i, t in enumerate(runs)}
    out.sort(key=lambda r: rank[r['tag']])
    return out

def lcp_clock(a, b):
    """The clock value up to which two runs (same host key, same start) are the same: the
    clock of the first differing send, or the end of the shorter run."""
    ea, eb = a['events'], b['events']
    if a['a0'] != b['a0']:
        return a['a0']
    for x, y in zip(ea, eb):
        if x != y:
            return min(x[0], y[0])
    if len(ea) == len(eb):
        return min(a['a1'], b['a1'])
    return min(ea[len(eb)][0] if len(ea) > len(eb) else a['a1'],
               eb[len(ea)][0] if len(eb) > len(ea) else b['a1'])

def analyse(runs, policy):
    rows = []
    for i, r in enumerate(runs):
        hk = host_key(r['sched'], policy)
        best_prev, best_any = r['a0'], r['a0']
        for j, o in enumerate(runs):
            if j == i or host_key(o['sched'], policy) != hk:
                continue
            c = lcp_clock(r, o)
            best_any = max(best_any, c)
            if j < i:
                best_prev = max(best_prev, c)
        tot = r['cost'] + r['rec']
        rows.append(dict(tag=r['tag'], prev=best_prev - r['a0'], near=best_any - r['a0'], tot=tot,
                         cost=r['cost'], secs=r['secs'], fp=first_point(r['sched']),
                         ws=(r['workload_start'] - r['a0']) if r['workload_start'] else None))
    return rows

def pct(x):
    return f'{100*x:.1f} %'

def summary(name, runs):
    res = {'name': name, 'runs': len(runs)}
    if not runs:
        return res
    tot = sum(r['cost'] + r['rec'] for r in runs)
    secs = sum(r['secs'] for r in runs)
    res['secs'] = secs
    res['setup_share'] = statistics.median([(r['workload_start'] - r['a0']) / (r['cost'] + r['rec'])
                                            for r in runs if r['workload_start']] or [0])
    for pol in ('strict', 'relaxed'):
        rows = analyse(runs, pol)
        saved = sum(x['prev'] for x in rows)
        wall = sum(x['secs'] * x['prev'] / x['tot'] for x in rows if x['tot'])
        near = [x['near'] / x['tot'] for x in rows if x['tot']]
        hk = collections.Counter(host_key(r['sched'], pol) for r in runs)
        res[pol] = dict(saved_share=saved / tot if tot else 0, wall_saved=wall,
                        near_median=statistics.median(near), near_max=max(near),
                        near_mean=statistics.mean(near),
                        hosts=len(hk), biggest_host_group=hk.most_common(1)[0][1],
                        near_dist=sorted(round(v, 3) for v in near))
    fps = [first_point(r['sched']) for r in runs]
    res['first_points'] = collections.Counter(fps).most_common()
    return res

if __name__ == '__main__':
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    allr = '--all-roles' in sys.argv
    for d in args:
        runs = load(d, allr)
        s = summary(os.path.basename(d.rstrip('/')), runs)
        if '--json' in sys.argv:
            print(json.dumps(s))
            continue
        print(f"== {s['name']}: {s['runs']} runs, {s.get('secs', 0):.0f} s of runs")
        if not runs:
            continue
        print(f"   setup after S1 (to the first workload command): median {pct(s['setup_share'])} of a run")
        for pol in ('strict', 'relaxed'):
            p = s[pol]
            print(f"   {pol:8}: host groups {p['hosts']} (largest {p['biggest_host_group']}); "
                  f"clock already run by an earlier run {pct(p['saved_share'])}, ~{p['wall_saved']:.0f} s; "
                  f"shared with the nearest run: median {pct(p['near_median'])}, mean {pct(p['near_mean'])}, max {pct(p['near_max'])}")
        print(f"   first split points: {s['first_points'][:12]}")
