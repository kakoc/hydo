#!/usr/bin/env python3
"""richreport.py <result dir> -- tables for the M44 richer-signal comparison.

Reads <dir>/{old,fix}-<mode>-<rep>/{report.json,runs.jsonl} written by hydo/explore/richguided.py
and prints (1) runs until the first durable-defaults failure on the old front door, (2) feature
growth and failures per class on the fixed front door, (3) tracing cost."""
import glob, json, os, statistics, sys

D = sys.argv[1] if len(sys.argv) > 1 else "/var/tmp/m44-rich"


def load(d):
    rj = os.path.join(d, "report.json")
    runs = [json.loads(l) for l in open(os.path.join(d, "runs.jsonl"))] if os.path.exists(os.path.join(d, "runs.jsonl")) else []
    return (json.load(open(rj)) if os.path.exists(rj) else None), runs


def at(growth, k, key):
    g = [x for x in growth if x["run"] <= k]
    return g[-1][key] if g else "-"


print("## Old front door: runs until the first durable-defaults failure (cap 60)\n")
print("| mode | start | first durable failure | runs | reproduced | distinct signatures |")
print("|---|---|---|---|---|---|")
for d in sorted(glob.glob(os.path.join(D, "old-*-*"))):
    if not os.path.isdir(d) or "aborted" in d:
        continue
    rep, runs = load(d)
    _, mode, r = os.path.basename(d).split("-")
    if rep:
        s = rep["summary"]
        print("| %s | %s | %s | %d | %d/%d | %d |" % (mode, r, s["first_failure_run_by_class"].get("durable-defaults", "-"),
              s["runs"], s["reproduced"], s["failures"], len(s["signatures"])))
    else:
        print("| %s | %s | (running, %d runs) | | | |" % (mode, r, len(runs)))

print("\n## Fixed front door: feature growth (cumulative distinct features at run k)\n")
print("| mode | k | edges hydostack | edges redis | edges mysqld | property firsts | detail buckets | state tuples |")
print("|---|---|---|---|---|---|---|---|")
for d in sorted(x for x in glob.glob(os.path.join(D, "fix-*-*")) if os.path.isdir(x)):
    rep, runs = load(d)
    mode = os.path.basename(d).split("-")[1]
    g = rep["growth"] if rep else [{"run": x["run"], **{"tot_" + k: v for k, v in x["tot"].items()},
                                     **{"E_" + p: v for p, v in x["E_by_prog"].items()}} for x in runs]
    for k in (5, 10, 20, 40):
        print("| %s | %d | %s | %s | %s | %s | %s | %s |" % (mode, k, at(g, k, "E_hydostack"), at(g, k, "E_redis"),
              at(g, k, "E_mysqld"), at(g, k, "tot_P"), at(g, k, "tot_D"), at(g, k, "tot_S")))

print("\n## Fixed front door: failures by class (40 runs)\n")
print("| mode | failing runs | durable-defaults | unsafe-knob | lying-disk | io-errors | distinct signatures |")
print("|---|---|---|---|---|---|---|")
for d in sorted(x for x in glob.glob(os.path.join(D, "fix-*-*")) if os.path.isdir(x)):
    rep, runs = load(d)
    if not rep:
        continue
    mode = os.path.basename(d).split("-")[1]
    cnt = {}
    for f in rep["failures"]:
        cnt[f["class"]] = cnt.get(f["class"], 0) + 1
    print("| %s | %d | %s | %s | %s | %s | %d |" % (mode, rep["summary"]["failures"], cnt.get("durable-defaults", 0),
          cnt.get("unsafe-knob", 0), cnt.get("lying-disk", 0), cnt.get("io-errors", 0), len(rep["summary"]["signatures"])))

print("\n## Tracing cost per run (all runs)\n")
tb, ds, sec, trunc = [], [], [], 0
for d in glob.glob(os.path.join(D, "*-*-*")):
    if not os.path.isdir(d):
        continue
    for x in load(d)[1]:
        c = x.get("cov", {})
        if "trace_bytes" in c:
            tb.append(c["trace_bytes"]); ds.append(c["decode_secs"]); trunc += c.get("truncated", 0)
            sec.append(x["secs"])
if tb:
    print("runs=%d  trace MB median %.1f max %.1f  decode s median %.1f max %.1f  run s median %.1f  truncated=%d" % (
        len(tb), statistics.median(tb) / 1e6, max(tb) / 1e6, statistics.median(ds), max(ds), statistics.median(sec), trunc))
