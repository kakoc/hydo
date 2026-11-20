#!/usr/bin/env python3
"""What a shared beginning would save under three ways of drawing and running the schedules.

Usage: models.py <exploration dir>... [--all-roles] [--table]

For each run the script reads its control transcript and finds:
  S   the workload's steps (HYDO_STEPS of the first workload command),
  k   the first step boundary: the run's first range is 0..k-1 (k = S when no fault splits it),
  t   the guest clock from S1 to the end of that first range, as a share of the run's guest
      clock (its run conversation plus its recovery boot).

Models (each run is compared with the runs before it in the explorer's order):
  now     the measured longest common prefix with an earlier run (prefix.py, relaxed host key);
  same-k  the explorer draws the composition (W=), the walk (I=) and the setting tokens once per
          exploration, and the controller is unchanged: a run shares its first range with an
          earlier run of the same host timing tokens and the same k;
  any-k   as same-k, and the controller starts the workload once per step, so that a run shares
          the steps before min(k, the largest earlier k) with the earlier runs.
"""
import os, re, sys, json, statistics, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prefix

TIMING = ('tj', 'tjpm', 'tjmax', 'tjlo', 'tjhi', 'tjx', 'pp', 'q', 'qn', 'xt', 'xtn', 'xtk',
          'pct', 'pctn', 'pctk', 'cf', 'cfn', 'spin', 'br')

STEPS_RE = re.compile(r'workload steps (\d+)\.\.(\d+)')

def timing_key(sched):
    toks = prefix.host_parts(sched)
    return ' '.join(t for t in toks if t.split('=', 1)[0] in TIMING)

def scan(path):
    stops = {}
    first_range = None
    steps = None
    go_n = None
    a0 = a1 = None
    with open(path) as f:
        for line in f:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            at = e.get('at')
            if at is not None:
                a0 = at if a0 is None else a0
                a1 = at
            if e.get('t') == 'stop':
                stops[e['n']] = e['at']
            if e.get('t') == 'send' and steps is None and e.get('bytes') and 'HYDO_STEPS=' in e['bytes']:
                m = re.search(r'HYDO_STEPS=(\d+)', e['bytes'])
                steps = int(m.group(1))
            if e.get('t') == 'note':
                ln = e.get('line', '')
                m = STEPS_RE.search(ln)
                if m and first_range is None:
                    first_range = (int(m.group(1)), int(m.group(2)), e['n'])
                if go_n is None and ln.endswith('command: go'):
                    go_n = e['n']
    def at_of(n):
        if n is None:
            return None
        ks = [x for x in stops if x <= n]
        return stops[max(ks)] if ks else None
    return dict(a0=a0, a1=a1, steps=steps, first=first_range,
                first_end=at_of(first_range[2]) if first_range else None, go=at_of(go_n))

def runs_of(d, all_roles):
    base = prefix.load(d, all_roles)
    out = []
    for r in base:
        s = scan(os.path.join(d, 'consoles', f"{r['tag']}.control.jsonl"))
        tot = r['cost'] + r['rec']
        if not tot or s['first_end'] is None or s['steps'] is None:
            continue
        fp = prefix.first_point(r['sched'])
        if fp == 0:
            k, end = 0, (r['workload_start'] or r['a0'])
        else:
            k, end = s['first'][1] + 1, s['first_end']
        r.update(S=s['steps'], k=k, tot=tot, fp=fp,
                 t=(end - r['a0']) / tot,
                 wl=((s['go'] or r['a1']) - r['a0']) / tot)
        out.append(r)
    return out

def model(runs):
    now = {x['tag']: x['prev'] / x['tot'] for x in prefix.analyse(runs, 'relaxed') if x['tot']}
    res = collections.defaultdict(float)
    secs = sum(r['secs'] for r in runs)
    tot = sum(r['tot'] for r in runs)
    seen_k = collections.defaultdict(set)
    maxk = collections.defaultdict(lambda: -1)
    for r in runs:
        tk = timing_key(r['sched'])
        f_now = now.get(r['tag'], 0.0)
        f_same = r['t'] if r['k'] in seen_k[tk] and r['k'] > 0 else 0.0
        m = maxk[tk]
        if m >= r['k']:
            f_any = r['t']
        elif m > 0:
            f_any = r['t'] * m / r['k']
        else:
            f_any = 0.0
        f_any = max(f_any, f_same)
        for name, f in (('now', f_now), ('same-k', f_same), ('any-k', f_any)):
            res[name + '_clock'] += f * r['tot']
            res[name + '_secs'] += f * r['secs']
        seen_k[tk].add(r['k'])
        maxk[tk] = max(m, r['k'])
    out = dict(runs=len(runs), secs=secs, steps=statistics.median(r['S'] for r in runs),
               timing_groups=len({timing_key(r['sched']) for r in runs}),
               k_share_median=statistics.median(r['k'] / r['S'] for r in runs),
               t_median=statistics.median(r['t'] for r in runs),
               t_mean=statistics.mean(r['t'] for r in runs),
               wl_median=statistics.median(r['wl'] for r in runs),
               k0=sum(1 for r in runs if r['k'] == 0))
    for name in ('now', 'same-k', 'any-k'):
        out[name] = res[name + '_clock'] / tot
        out[name + '_s'] = res[name + '_secs']
    return out

if __name__ == '__main__':
    dirs = [a for a in sys.argv[1:] if not a.startswith('--')]
    allr = '--all-roles' in sys.argv
    print('| exploration | runs | run s | steps | first split k/S (median) | k = 0 | first range t (median, mean) | workload share | now | same-k | any-k | any-k s |')
    print('|---|---|---|---|---|---|---|---|---|---|---|---|')
    for d in dirs:
        runs = runs_of(d, allr)
        if not runs:
            print(f'| {os.path.basename(d)} | 0 |')
            continue
        m = model(runs)
        print(f"| {os.path.basename(d.rstrip('/'))} | {m['runs']} | {m['secs']:.0f} | {m['steps']:.0f} | "
              f"{m['k_share_median']:.2f} | {m['k0']} | {100*m['t_median']:.1f} %, {100*m['t_mean']:.1f} % | "
              f"{100*m['wl_median']:.0f} % | {100*m['now']:.1f} % | {100*m['same-k']:.1f} % | "
              f"{100*m['any-k']:.1f} % | {m['any-k_s']:.0f} |")
