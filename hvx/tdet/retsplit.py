#!/usr/bin/env python3
"""retsplit.py <serve stderr>: split HVX_EXPLORE_RETLOG lines per run (serve mode; a run
starts at each 'hvx_boot: ... cmdline="console=ttyS0"' line) and report, per run, the SUBFP
hashes and the first RET line that differs from run 1 (fields compared: total, tsc, rdtsc,
ulen; ticks/steps/hops/land are statistics that do not ride in the snapshot)."""
import re, sys
runs, cur = [], None
for line in open(sys.argv[1], errors="replace"):
    if line.startswith("hvx_boot:") and 'cmdline="console=ttyS0"' in line:
        if cur is not None and cur.get("tag"): pass
        cur = {"ret": [], "sub": [], "tag": None}
        runs.append(cur)
        continue
    if cur is None: continue
    if line.startswith("RET "):
        d = dict(kv.split("=", 1) for kv in line.split()[1:])
        cur["ret"].append(d)
    elif line.startswith("SUBFP"):
        p = line.split(); cur["tag"] = p[1]; cur["sub"].append(" ".join(p[2:]))
keys = ["ulen", "total", "tsc", "rdtsc", "reason"]
base = runs[0]
for r in runs:
    first = None
    n = min(len(r["ret"]), len(base["ret"]))
    for i in range(n):
        if any(r["ret"][i].get(k) != base["ret"][i].get(k) for k in keys):
            first = i; break
    if first is None and len(r["ret"]) != len(base["ret"]): first = n
    smi = (r["ret"][0].get("smi"), r["ret"][-1].get("smi")) if r["ret"] else None
    print(r["tag"], "rets=%d" % len(r["ret"]), "smi=%s..%s" % smi if smi else "", "first_diff_ret=%s" % first)
    for s in r["sub"]: print("   ", s)
    if first is not None and first < len(r["ret"]):
        for j in range(max(0, first - 1), min(first + 2, len(r["ret"]))):
            a = base["ret"][j] if j < len(base["ret"]) else {}
            b = r["ret"][j]
            print("    ret %d base: %s" % (j, " ".join("%s=%s" % (k, a.get(k)) for k in keys + ["ticks", "land"])))
            print("    ret %d this: %s" % (j, " ".join("%s=%s" % (k, b.get(k)) for k in keys + ["ticks", "land"])))
