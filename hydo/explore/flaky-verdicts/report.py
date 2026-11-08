"""report.py <out dir>: the per-task table and the totals of docs/design/flaky-verdicts.md.

Reads every <out dir>/<id>/summary.json (collect.py) and prints Markdown. For each kind of run:
"res" is how many graded runs resolved the task (every FAIL_TO_PASS and PASS_TO_PASS test
passed), "flips" the FAIL_TO_PASS and PASS_TO_PASS tests whose verdict was not the same in every
run (with "+n" for other tests the log shows), "s" the median wall time of one run, and "logs"
the number of different test logs, byte for byte. The snapshot's logs are counted over calls 1
to 9: call 10 writes its output to a file named "10.out" instead of "<k>.out", one byte longer,
so its guest work differs by design (docs/design/flaky-verdicts.md).
"""
import json
import os
import statistics
import sys

out = sys.argv[1]
rows = []
for iid in sorted(os.listdir(out)):
    p = os.path.join(out, iid, "summary.json")
    if os.path.exists(p):
        rows.append(json.load(open(p)))

KINDS = [("native", "native"), ("native-load", "native, 1/4 CPU"), ("fresh", "hydo, fresh boot"),
         ("fresh-load", "hydo, loaded CPU"), ("snap", "hydo, from snapshot")]


def cell(m, snap=False):
    if not m:
        return "-"
    fl = len(m["flips"])
    other = len(m["other_flips"])
    flips = f"{fl}" + (f"+{other}" if other else "")
    shas = [x for x in m.get("log_shas", []) if x]
    if snap:
        shas = shas[:9]
    logs = len(set(shas)) if shas else m["distinct_logs"]
    ung = "" if m["graded"] == m["runs"] else f" ({m['runs'] - m['graded']} ungraded)"
    return f"{m['resolved']}/{m['graded']}{ung}, {flips}, {m['wall_median']:.1f} s, {logs}"


print("Each cell: resolved runs / graded runs, flipping tests, median seconds per run, different logs.")
print()
print("| task | tests | " + " | ".join(k[1] for k in KINDS) + " |")
print("|---|---|" + "---|" * len(KINDS))
tot = {"tasks": 0, "native_flip_tasks": 0, "native_any_flip_tasks": 0, "native_load_flip_tasks": 0,
       "hydo_flip_tasks": 0, "hydo_any_flip_tasks": 0, "changed_tasks": 0, "resolution_flip_tasks": 0,
       "runs": {k: 0 for k, _ in KINDS}, "native_log_differs_tasks": 0, "hydo_log_differs_tasks": 0,
       "fp_one_per_task": 0, "fp_load_same": 0, "fp_load_tasks": 0}
walls = {k: [] for k, _ in KINDS}
for s in rows:
    m = s["modes"]
    cells = [cell(m.get(k), k == "snap") for k, _ in KINDS]
    print(f"| {s['instance_id']} | {s['tests']} | " + " | ".join(cells) + " |")
    tot["tasks"] += 1
    for k, _ in KINDS:
        if m.get(k):
            tot["runs"][k] += m[k]["runs"]
            walls[k].append(m[k]["wall_median"])
    n = m.get("native")
    tot["native_flip_tasks"] += bool(n and n["flips"])
    tot["native_any_flip_tasks"] += bool(n and (n["flips"] or n["other_flips"]))
    nl = m.get("native-load")
    tot["native_load_flip_tasks"] += bool(nl and (nl["flips"] or nl["other_flips"]))
    hm = [m.get(k) for k in ("fresh", "fresh-load", "snap")]
    tot["hydo_flip_tasks"] += bool(any(x and x["flips"] for x in hm))
    tot["hydo_any_flip_tasks"] += bool(any(x and (x["flips"] or x["other_flips"]) for x in hm))
    tot["changed_tasks"] += bool(s["changed"])
    tot["resolution_flip_tasks"] += bool(any(x and 0 < x["resolved"] < x["graded"] for x in [n, nl] + hm))
    tot["native_log_differs_tasks"] += bool(n and n["distinct_logs"] > 1)
    f, fl, sn = m.get("fresh"), m.get("fresh-load"), m.get("snap")
    hydo_logs_differ = (f and f["distinct_logs"] > 1) or (fl and fl["distinct_logs"] > 1) or \
        (sn and len(set(x for x in sn.get("log_shas", [])[:9] if x)) > 1)
    tot["hydo_log_differs_tasks"] += bool(hydo_logs_differ)
    tot["fp_one_per_task"] += bool(f and len(f["fps"]) == 1)
    if f and fl:
        tot["fp_load_tasks"] += 1
        tot["fp_load_same"] += f["fps"] == fl["fps"]
print()
print("median of the per-task medians (s):",
      {k: round(statistics.median(v), 1) for k, v in walls.items() if v})
print(json.dumps(tot))
for s in rows:
    if s["changed"]:
        print(s["instance_id"], json.dumps(s["changed"]))
