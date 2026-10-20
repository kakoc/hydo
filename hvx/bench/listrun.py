#!/usr/bin/env python3
"""Run a fixed list of schedules on one or more images through explore.py's serve pool.

    python3 listrun.py --images IMG[,IMG...] --list FILE --workers 3 --out DIR

FILE: one schedule per line ("disk=none dseed=0 | run P=kafka@28+2"); blank lines and # comments
are skipped. Writes DIR/<image>.jsonl (one record per schedule: schedule, verdict, fp) and
prints a table, one line per schedule, with each image's verdict.
"""
import argparse, json, os, sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "explore"))
import explore as ex


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--images", required=True)
    ap.add_argument("--list", required=True)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    items = [l.strip() for l in open(a.list) if l.strip() and not l.lstrip().startswith("#")]
    os.makedirs(a.out, exist_ok=True)
    table = {}
    for img in a.images.split(","):
        ex.IMAGE = img
        d = os.path.join(a.out, img.replace("-disk.img", ""))
        os.makedirs(d, exist_ok=True)
        pool = ex.Pool(a.workers, d)
        res = pool.run_all([("s%d" % i, t) for i, t in enumerate(items)])
        pool.close()
        with open(os.path.join(a.out, img.replace("-disk.img", "") + ".jsonl"), "w") as f:
            for i, t in enumerate(items):
                r = res["s%d" % i][1]
                rec = {"schedule": t, "verdict": r.get("verdict", "?"), "fp": r.get("fp")}
                f.write(json.dumps(rec) + "\n")
                table.setdefault(t, []).append(rec)
    for t in items:
        print("%-62s %s" % (t, " | ".join("%s %s" % (r["verdict"].replace("HYDO-ORACLE: ", "")[:44], r["fp"])
                                         for r in table[t])))


if __name__ == "__main__":
    main()
