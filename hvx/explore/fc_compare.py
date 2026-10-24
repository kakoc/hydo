#!/usr/bin/env python3
"""fc_compare.py <adaptive out> [<full-sweep out> ...] -- which failing points of a full freeze sweep
(M76, explore.py --sweep freeze) an adaptive sweep (--fc-adaptive) found, and at what cost.

A point is the F= label without its duration (`app<mysql@9#307`); the full sweep's failing points
are those whose run failed. Prints the adaptive run's cost (trace runs, points per stage, wall time
of the sweep and of the whole run) and, per full sweep, found / total and the missed points.
"""
import json, os, re, sys


def failing(d):
    pts = {json.loads(l)["seed"]: json.loads(l)["point"] for l in open(os.path.join(d, "sweep-points.jsonl"))}
    out = {}
    for l in open(os.path.join(d, "runs.jsonl")):
        r = json.loads(l)
        if not r["verdict"].startswith("HYDO-ORACLE: OK"):
            out[re.sub(r":\d+$", "", pts[r["seed"]])] = r["verdict"]
    return out


def main():
    ad = sys.argv[1]
    rep = json.load(open(os.path.join(ad, "report.json")))
    s = rep["summary"]
    fa = failing(ad)
    secs = sum(json.loads(l).get("secs") or 0 for l in open(os.path.join(ad, "runs.jsonl")))
    tp = os.path.join(ad, "fc-traces.jsonl")
    tsecs = sum(json.loads(l).get("secs") or 0 for l in open(tp)) if os.path.exists(tp) else 0
    print("run seconds: points %.0f s, traces %.0f s (with %d workers: %.0f s of wall time; quiet-window waits %s s)"
          % (secs, tsecs, s["workers"], (secs + tsecs) / s["workers"], s.get("quiet_wait_s", 0)))
    print("adaptive %s: %d trace runs + %d stage-1 + %d stage-2 points = %d runs; sweep %.0f s, whole run %.0f s; "
          "%d failing points, %d of %d failures reproduced"
          % (ad, s.get("trace_runs", 0), s.get("stage1_points", 0), s.get("stage2_points", 0),
             s.get("trace_runs", 0) + s["schedules"], s["sweep_wall_s"], s["total_wall_s"], len(fa),
             s["reproduced"], s["failures"]))
    for full in sys.argv[2:]:
        ff = failing(full)
        got = [p for p in sorted(ff) if p in fa]
        print("vs %s: %d / %d failing points found; missed: %s" % (full, len(got), len(ff),
              " ".join(p for p in sorted(ff) if p not in fa) or "none"))
        extra = [p for p in sorted(fa) if p not in ff and "@" in p and ":" not in p.split("#", 1)[1]]
        if extra:
            print("  failing in the adaptive run only (every-packet points): %s" % " ".join(extra))


if __name__ == "__main__":
    main()
