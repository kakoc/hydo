#!/usr/bin/env python3
"""Where an explorer campaign spends its runs and its wall time (PERF-RUN-COST.md, item 5).

Reads explore.py output folders (report.json, shrink.jsonl) and prints, per campaign and in
total: first runs (one per schedule), confirmation re-runs of failures, shrink candidates, and
the two confirmation runs of each minimal schedule; and the wall time of the first sweep against
the whole campaign (the rest is worker boot, re-runs, shrinking and the report).

  campaign_mix.py <out dir or glob> ...       (default: every report.json under /var/tmp)
"""
import glob, json, os, sys


def one(d):
    rj = os.path.join(d, "report.json")
    try:
        r = json.load(open(rj))
    except Exception:
        return None
    s = r.get("summary", {})
    if not s.get("schedules") or "sweep_wall_s" not in s:
        return None
    first = s["schedules"]
    nrep = s.get("repro_runs", 1)
    fails = s.get("failures", 0)
    repro = fails * nrep
    shrink = 0
    sp = os.path.join(d, "shrink.jsonl")
    if os.path.exists(sp):
        for line in open(sp):
            try:
                x = json.loads(line)
            except Exception:
                continue
            if "shrink" in x:
                shrink += 1
    conf = 2 * len(r.get("shrunk", {}) or {})
    return {"dir": d, "image": os.path.basename(str(s.get("image"))), "strategy": s.get("strategy"),
            "workers": s.get("workers"), "first": first, "repro": repro, "shrink": shrink, "confirm": conf,
            "fails": fails, "sweep_s": s.get("sweep_wall_s", 0), "total_s": s.get("total_wall_s", 0),
            "boot_s": s.get("worker_boot_s", 0), "mean_run_s": s.get("mean_run_s", 0)}


def main():
    args = sys.argv[1:] or ["/var/tmp/**/report.json"]
    dirs = []
    for a in args:
        if a.endswith("report.json"):
            dirs += [os.path.dirname(p) for p in glob.glob(a, recursive=True)]
        else:
            dirs += glob.glob(a)
    rows = [x for x in (one(d) for d in sorted(set(dirs))) if x]
    tot = {k: 0 for k in ("first", "repro", "shrink", "confirm", "sweep_s", "total_s")}
    print("%-48s %-34s %-16s %3s %5s %5s %6s %5s %8s %8s" % ("campaign", "image", "strategy", "w", "first",
          "repro", "shrink", "conf", "sweep_s", "total_s"))
    for x in rows:
        print("%-48s %-34s %-16s %3s %5d %5d %6d %5d %8.0f %8.0f" % (x["dir"][-48:], x["image"][:34],
              str(x["strategy"])[:16], x["workers"], x["first"], x["repro"], x["shrink"], x["confirm"],
              x["sweep_s"], x["total_s"]))
        for k in tot:
            tot[k] += x[k]
    runs = tot["first"] + tot["repro"] + tot["shrink"] + tot["confirm"]
    if runs:
        print("\nTOTAL %d campaigns, %d runs: first %.1f%%, repro %.1f%%, shrink %.1f%%, confirm %.1f%%" % (
            len(rows), runs, 100 * tot["first"] / runs, 100 * tot["repro"] / runs, 100 * tot["shrink"] / runs,
            100 * tot["confirm"] / runs))
        print("wall: first sweeps %.0f s of %.0f s total (%.1f%%)" % (tot["sweep_s"], tot["total_s"],
              100 * tot["sweep_s"] / max(1, tot["total_s"])))
    with_f = [x for x in rows if x["fails"]]
    if with_f:
        r2 = sum(x["first"] + x["repro"] + x["shrink"] + x["confirm"] for x in with_f)
        print("campaigns with failures (%d): first runs are %.1f%% of their runs; first sweeps %.1f%% of their wall" % (
            len(with_f), 100 * sum(x["first"] for x in with_f) / r2,
            100 * sum(x["sweep_s"] for x in with_f) / max(1, sum(x["total_s"] for x in with_f))))


if __name__ == "__main__":
    main()
