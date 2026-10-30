#!/usr/bin/env python3
"""run_list.py -- run a list of hand-written schedules on one image, each from the S1 snapshot
of ONE boot (explore.py's worker pool; no generation, no shrinking). Each line of the list
file is "<tag> <schedule>"; a schedule may be listed several times under different tags to
check that it reproduces.

    python3 run_list.py --image explore-ob-redis-disk.img --out /var/tmp/x list.txt

Writes <out>/results.jsonl (tag, schedule, fp, wfp, verdict) and prints one line per run;
the consoles are in <out>/consoles/<tag>.log.
"""
import argparse, json, os, sys

import explore as ex


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("list")
    a = ap.parse_args()
    ex.IMAGE = a.image
    items = []
    for line in open(a.list):
        line = line.strip()
        if line and not line.startswith("#"):
            tag, sched = line.split(None, 1)
            items.append((tag, sched))
    os.makedirs(a.out, exist_ok=True)
    pool = ex.Pool(a.workers, a.out)
    res = pool.run_all(items)
    pool.close()
    with open(os.path.join(a.out, "results.jsonl"), "w") as f:
        for tag, sched in items:
            r = res[tag][1]
            f.write(json.dumps({"tag": tag, "schedule": sched, **r}) + "\n")
            print("%-10s fp=%s wfp=%s %s | %s" % (tag, r["fp"], r.get("wfp", "-"), r["verdict"][:150], sched))
            sys.stdout.flush()


if __name__ == "__main__":
    main()
