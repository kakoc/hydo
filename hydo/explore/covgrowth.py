#!/usr/bin/env python3
"""covgrowth.py -- coverage-growth table for guided.py result directories (M44).

  python3 covgrowth.py /var/tmp/m44-cov/grow-random-1 /var/tmp/m44-cov/grow-guided-1 ...

For each directory: coverage features (edge + AFL hit-count bucket) and plain edges
(from,to) after runs 5/10/20/40 (or --at), failing runs per finding class with the run of
the first one, corpus size and throughput. Then the mean per mode (random / guided).
"""
import argparse, collections, csv, json, os
import explore as E


def load(d):
    growth = {}
    with open(os.path.join(d, "growth.csv")) as f:
        for row in csv.DictReader(f):
            growth[int(row["run"])] = (int(row["global_edges"]), int(row.get("global_plain_edges") or 0))
    rep = json.load(open(os.path.join(d, "report.json")))
    fails = collections.Counter()
    first = {}
    for line in open(os.path.join(d, "runs.jsonl")):
        r = json.loads(line)
        if E.verdict_kind(r["verdict"]) != "ok":
            fails[r["class"]] += 1
            first.setdefault(r["class"], r["run"])
    return growth, rep["summary"], fails, first


def at(growth, n):
    """Coverage after run n (the last recorded run <= n), or None if the run did not happen."""
    ks = [k for k in growth if k <= n]
    if not ks or max(growth) < n:
        return None
    return growth[max(ks)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dirs", nargs="+")
    ap.add_argument("--at", default="5,10,20,40")
    a = ap.parse_args()
    pts = [int(x) for x in a.at.split(",")]
    classes = ["durable-defaults", "unsafe-knob", "lying-disk", "io-errors"]
    rows = []
    print("| run dir | mode | runs | " + " | ".join("feat@%d / edges@%d" % (p, p) for p in pts)
          + " | corpus | " + " | ".join(classes) + " | runs/h |")
    print("|" + "---|" * (4 + len(pts) + len(classes)))
    for d in a.dirs:
        g, s, fails, first = load(d)
        cells = []
        for p in pts:
            v = at(g, p)
            cells.append("-" if v is None else "%d / %d" % v)
        fc = ["%d (first %d)" % (fails[c], first[c]) if fails[c] else "0" for c in classes]
        runs = "%d" % s["runs"]
        if s.get("pt_retries") or s.get("untraced_runs"):
            runs += " (%d re-run, %d untraced)" % (s.get("pt_retries", 0), s.get("untraced_runs", 0))
        print("| %s | %s | %s | %s | %d | %s | %.0f |" % (os.path.basename(d.rstrip("/")), s["mode"], runs,
              " | ".join(cells), s["corpus"], " | ".join(fc), s["runs_per_hour"]))
        rows.append((s["mode"], g, fails))
    print()
    for mode in ("random", "guided"):
        rs = [r for r in rows if r[0] == mode]
        if not rs:
            continue
        cells = []
        for p in pts:
            vs = [at(g, p) for _, g, _ in rs]
            vs = [v for v in vs if v]
            cells.append("-" if not vs else "%d / %d (n=%d)" % (sum(v[0] for v in vs) / len(vs),
                                                                sum(v[1] for v in vs) / len(vs), len(vs)))
        tot = collections.Counter()
        for _, _, f in rs:
            tot.update(f)
        print("mean %s: %s; failing runs %s" % (mode, " | ".join(cells),
              ", ".join("%s=%d" % (c, tot[c]) for c in classes)))


if __name__ == "__main__":
    main()
