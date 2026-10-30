#!/usr/bin/env python3
"""cftable.py <sweepdir>...: per sweep, runs / failures / distinct failure kinds / distinct
end states, and every failing schedule's verdict (from cfsweep.py's results.txt)."""
import re, sys, collections
for d in sys.argv[1:]:
    res, fc = {}, {}
    for l in open(d + "/results.txt"):
        m = re.match(r"RESULT (\S+) fp=(\S+) wfp=\S+ secs=(\S+)\S* .*?verdict=(.*)", l.strip()) or \
            re.match(r"RESULT (\S+) fp=(\S+) wfp=\S+ secs=(\S+) verdict=(.*)", l.strip())
        if m: res[m.group(1)] = (m.group(2), float(m.group(3)), m.group(4))
        m = re.match(r"FLUSHCUT (\S+) target=(\d+) flushes=(\d+) off=(\d) hung=(\d+)", l)
        if m: fc[m.group(1)] = (int(m.group(3)), int(m.group(4)))
    bad = {t: v for t, v in res.items() if "VIOLATION" in v[2] or "ERROR" in v[2] or "TIMEOUT" in v[2]}
    kind = lambda v: re.sub(r"\d+", "N", v.split("(")[0])[:90]
    kinds = collections.Counter(kind(v[2]) for v in bad.values())
    secs = sorted(v[1] for v in res.values())
    print("%s: runs=%d failures=%d kinds=%d end-states=%d median=%.0fs" % (d, len(res), len(bad), len(kinds),
          len(set(v[0] for v in res.values())), secs[len(secs) // 2] if secs else 0))
    for k, n in kinds.most_common():
        tags = [t for t, v in sorted(bad.items()) if kind(v[2]) == k]
        print("   %3d x %s  [%s]" % (n, k, " ".join(tags[:12]) + (" ..." if len(tags) > 12 else "")))
