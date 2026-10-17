#!/usr/bin/env python3
"""compare.py -- tables for comparing exploration strategies (Milestone 42).

Reads explorer output directories (runs.jsonl, written by explore.py) and prints:
  * for a sweep: every point with its verdict, and the failing points by class;
  * for strategy runs: per repetition, the position (1-based, in seed order) of the first
    failure of each known kind, and the number of distinct failure signatures.

  python3 compare.py sweep /var/tmp/m42/sweep-kill-fix
  python3 compare.py strategies /var/tmp/m42 old   # dirs old-<strategy>-<rep>
  python3 compare.py strategies /var/tmp/m42 fix
  python3 compare.py portfolio /var/tmp/m44-port /var/tmp/m42 old   # portfolio.py vs swarm/random
"""
import json, os, re, sys, statistics

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from explore import parse, signature, verdict_kind  # noqa: E402


def load(d):
    recs = [json.loads(l) for l in open(os.path.join(d, "runs.jsonl"))]
    recs.sort(key=lambda r: r["seed"])
    # runs.jsonl is written before the reproduction step; report.json has its results
    rp = os.path.join(d, "report.json")
    if os.path.exists(rp):
        rep = {f["seed"]: f for f in json.load(open(rp))["failures"]}
        for r in recs:
            if r["seed"] in rep:
                r["reproduced"] = rep[r["seed"]].get("reproduced")
    return recs


def sig_key(r):
    return (r["class"], signature(r["verdict"]))      # numbers replaced by #


# Known kinds of failure, recognised from the schedule and the verdict.
def kinds(r):
    if verdict_kind(r["verdict"]) == "ok":
        return set()
    s = parse(r["schedule"])
    cut = s["disk"] == "cut" or "C" in s
    out = set()
    if r["class"] == "durable-defaults":
        out.add("durable-defaults")
    if r["class"] == "unsafe-knob" and cut and s.get("M", 1) != 1:
        out.add("mysql-trx_commit+cut")
    if r["class"] == "unsafe-knob" and cut and s.get("R", "always") != "always":
        out.add("redis-appendfsync+cut")
    if r["class"] == "lying-disk":
        out.add("lying-disk")
    if r["class"] == "io-errors":
        out.add("io-errors")
    return out


def cmd_sweep(d):
    recs = load(d)
    pts = {}
    p = os.path.join(d, "sweep-points.jsonl")
    if os.path.exists(p):
        pts = {json.loads(l)["seed"]: json.loads(l)["point"] for l in open(p)}
    fails = [r for r in recs if verdict_kind(r["verdict"]) != "ok"]
    print("sweep %s: %d points, %d failing" % (os.path.basename(d), len(recs), len(fails)))
    by = {}
    for r in fails:
        by.setdefault(sig_key(r), []).append(pts.get(r["seed"], r["seed"]))
    for (c, s), ps in sorted(by.items()):
        print("  %-16s %-70s %3d  %s" % (c, s[:70], len(ps), " ".join(map(str, ps[:12])) + (" ..." if len(ps) > 12 else "")))
    if not fails:
        print("  (no failures)")
    nr = sum(1 for r in fails if r.get("reproduced"))
    print("  reproduced bit-for-bit: %d/%d" % (nr, len(fails)))


def cmd_strategies(root, prefix):
    dirs = sorted(x for x in os.listdir(root) if x.startswith(prefix + "-") and os.path.isdir(os.path.join(root, x)))
    table = {}
    for x in dirs:
        _, st, rep = x.split("-")
        recs = load(os.path.join(root, x))
        first = {}
        sigs = set()
        for i, r in enumerate(recs, 1):
            for k in kinds(r):
                first.setdefault(k, i)
            if verdict_kind(r["verdict"]) != "ok":
                sigs.add(sig_key(r))
        fails = [r for r in recs if verdict_kind(r["verdict"]) != "ok"]
        table.setdefault(st, []).append({"rep": int(rep), "runs": len(recs), "first": first, "sigs": len(sigs),
                                          "fails": len(fails), "repro": sum(1 for r in fails if r.get("reproduced"))})
    allk = sorted({k for st in table for t in table[st] for k in t["first"]} |
                  ({"durable-defaults"} if prefix == "old" else {"mysql-trx_commit+cut", "redis-appendfsync+cut", "lying-disk"}))
    for st in sorted(table):
        print("== %s (%s image)" % (st, prefix))
        print("  rep runs fails repro sigs  " + "  ".join("%22s" % k for k in allk))
        for t in sorted(table[st], key=lambda t: t["rep"]):
            print("  %3d %4d %5d %5d %4d  " % (t["rep"], t["runs"], t["fails"], t["repro"], t["sigs"]) +
                  "  ".join("%22s" % (t["first"].get(k, "-")) for k in allk))
        for k in allk:
            v = [t["first"].get(k) for t in table[st]]
            found = [x for x in v if x is not None]
            print("  %-22s found in %d/%d reps; median first %s; values %s" % (
                k, len(found), len(v), statistics.median(found) if found else "-", v))
        print("  distinct signatures per rep: %s (median %s)" % (
            [t["sigs"] for t in sorted(table[st], key=lambda t: t["rep"])],
            statistics.median([t["sigs"] for t in table[st]])))


def cmd_portfolio(port_root, base_root, prefix, base_strats=("swarm", "random")):
    """Portfolio runs (<port_root>/<prefix>-portfolio-<rep>, from portfolio.py) against single-
    strategy runs (<base_root>/<prefix>-<strategy>-<rep>, from explore.py), same run budget:
    failing runs, distinct signatures, first run of each known kind, and distinct features
    (portfolio.py's reward: signatures + property outcomes) after 20/40/60 runs."""
    from portfolio import run_features
    rows = {}
    for root, st in [(port_root, "portfolio")] + [(base_root, s) for s in base_strats]:
        if not os.path.isdir(root):
            continue
        for x in sorted(os.listdir(root)):
            if not (x.startswith("%s-%s-" % (prefix, st)) and os.path.isdir(os.path.join(root, x))):
                continue
            d = os.path.join(root, x)
            recs = load(d)
            first, sigs, seen, curve = {}, set(), set(), {}
            for i, r in enumerate(recs, 1):
                for k in kinds(r):
                    first.setdefault(k, i)
                if verdict_kind(r["verdict"]) != "ok":
                    sigs.add(sig_key(r))
                seen |= run_features(d, r.get("tag", "seed-%d" % r["seed"]), r)
                if i in (20, 40, 60):
                    curve[i] = len(seen)
            fails = [r for r in recs if verdict_kind(r["verdict"]) != "ok"]
            arms = {}
            for r in recs:
                if "arm" in r:
                    arms[r["arm"]] = arms.get(r["arm"], 0) + 1
            rows.setdefault(st, []).append({"rep": int(x.rsplit("-", 1)[1]), "runs": len(recs), "first": first,
                                             "sigs": len(sigs), "fails": len(fails), "feats": curve, "arms": arms,
                                             "repro": sum(1 for r in fails if r.get("reproduced"))})
    allk = sorted({k for st in rows for t in rows[st] for k in t["first"]} |
                  ({"durable-defaults"} if prefix == "old" else {"mysql-trx_commit+cut", "redis-appendfsync+cut", "lying-disk"}))
    for st in rows:
        print("== %s (%s image)" % (st, prefix))
        print("  rep runs fails repro sigs feats@20/40/60  " + "  ".join("%22s" % k for k in allk))
        for t in sorted(rows[st], key=lambda t: t["rep"]):
            f = "/".join(str(t["feats"].get(n, "-")) for n in (20, 40, 60))
            print("  %3d %4d %5d %5d %4d %14s  " % (t["rep"], t["runs"], t["fails"], t["repro"], t["sigs"], f) +
                  "  ".join("%22s" % t["first"].get(k, "-") for k in allk))
            if t["arms"]:
                print("      runs per arm: " + ", ".join("%s %d" % kv for kv in sorted(t["arms"].items())))
        for k in allk:
            v = [t["first"].get(k) for t in sorted(rows[st], key=lambda t: t["rep"])]
            found = [x for x in v if x is not None]
            print("  %-22s found in %d/%d reps; median first %s; values %s" % (
                k, len(found), len(v), statistics.median(found) if found else "-", v))
        print("  distinct signatures per rep: %s" % [t["sigs"] for t in sorted(rows[st], key=lambda t: t["rep"])])


if __name__ == "__main__":
    if sys.argv[1] == "sweep":
        for d in sys.argv[2:]:
            cmd_sweep(d)
    elif sys.argv[1] == "portfolio":
        cmd_portfolio(sys.argv[2], sys.argv[3], sys.argv[4])
    else:
        cmd_strategies(sys.argv[2], sys.argv[3])
