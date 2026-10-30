#!/usr/bin/env python3
"""One row per scaling point: workers, steady-state runs/min, speed-up, mean run, and the mean of
the phases that slow down under load (PERF-RUN-COST.md, section 7).

  scale_table.py <scale dir> ...      (each dir holds worker-*.err, vmstat.txt, explore.out)
"""
import glob, json, os, re, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import phases  # noqa: E402

KEYS = ["fresh", "r_ram", "r_disk", "workload", "wfp", "ffp", "durable", "rec_restore", "recovery", "rfp"]


def row(d):
    files = sorted(glob.glob(os.path.join(d, "worker-*.err")))
    runs, boots = phases.parse(files)
    if not runs:
        return None
    ends = [r["t0"] + r["total"] for r in runs]
    firsts = {}
    for r in runs:
        firsts[r["file"]] = min(firsts.get(r["file"], 1e18), r["t0"])
    lo, hi = max(firsts.values()), max(ends)
    n = sum(1 for e in ends if lo <= e <= hi)
    rpm = 60 * n / (hi - lo) if hi > lo else 0
    mean = {k: sum(r.get(k, 0) for r in runs) / len(runs) for k in KEYS + ["total", "dirtyscan"]}
    boot = [b.get("snap_s0", 0) + b.get("snap_s1", 0) for b in boots]
    cpu = None
    vp = os.path.join(d, "vmstat.txt")
    if os.path.exists(vp):
        rows = [l.split() for l in open(vp) if re.match(r"\s*\d", l)]
        if rows:
            col = lambda i: sum(float(x[i]) for x in rows) / len(rows)
            cpu = (col(12), col(13), col(14), col(15))     # us sy id wa
    summ = {}
    try:
        for line in open(os.path.join(d, "explore.out")):
            if line.startswith("{"):
                summ = json.loads(line)
    except OSError:
        pass
    return {"dir": os.path.basename(d.rstrip("/")), "workers": len(files), "rpm": rpm, "mean": mean,
            "boot_snap": sum(boot) / len(boot) if boot else 0, "cpu": cpu, "boot_s": summ.get("worker_boot_s")}


def main():
    rows = [r for r in (row(d) for d in sys.argv[1:]) if r]
    base = rows[0]["rpm"] if rows else 1
    print("%-12s %2s %8s %6s %7s %7s | %s | %s" % ("point", "N", "runs/min", "x", "run_s", "boot_s",
          " ".join("%7s" % k[:7] for k in KEYS), "cpu us/sy/id/wa"))
    for r in rows:
        m = r["mean"]
        print("%-12s %2d %8.2f %6.2f %7.2f %7s | %s | %s" % (r["dir"], r["workers"], r["rpm"], r["rpm"] / base,
              m["total"] - m["dirtyscan"], r["boot_s"], " ".join("%7.2f" % m[k] for k in KEYS),
              "/".join("%.0f" % x for x in r["cpu"]) if r["cpu"] else "-"))


if __name__ == "__main__":
    main()
