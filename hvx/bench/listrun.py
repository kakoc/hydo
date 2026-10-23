#!/usr/bin/env python3
"""Run a fixed list of schedules on one or more images through explore.py's serve pool.

    python3 listrun.py --images IMG[,IMG...] --list FILE --workers 3 --out DIR [--vcpus N] [--repeat 2]

FILE: one schedule per line ("disk=none dseed=0 | run P=kafka@28+2"); blank lines and # comments
are skipped. Writes DIR/<image>.jsonl (one record per schedule: schedule, verdict, fp) and
prints a table, one line per schedule, with each image's verdict.
--vcpus N runs every VM with N vCPUs (explore.py --vcpus; host tokens q=/qs= need N > 1).
--repeat 2 runs every schedule twice; the record gets "fp2" and "same" (fingerprints equal).
Each record also carries "qn" (vCPU turns in the workload) and "crash": the order service's first
panic/fatal-error line from the run's consoles, the line run.py's crash oracle uses (M59 images).
"""
import argparse, json, os, re, sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "explore"))
import explore as ex

CRASH_RE = re.compile(r"hydo-explore: front door log: ((?:panic|fatal error).*)")


def crash_line(d, tag):
    for suffix in ("", ".recovery"):
        cp = os.path.join(d, "consoles", "%s%s.log" % (tag, suffix))
        if os.path.exists(cp):
            m = CRASH_RE.search(open(cp, errors="replace").read())
            if m:
                return m.group(1).strip()[:120]
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--images", required=True)
    ap.add_argument("--list", required=True)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--out", required=True)
    ap.add_argument("--vcpus", type=int, default=1)
    ap.add_argument("--repeat", type=int, choices=[1, 2], default=1)
    a = ap.parse_args()
    ex.VCPUS = a.vcpus
    items = [l.strip() for l in open(a.list) if l.strip() and not l.lstrip().startswith("#")]
    os.makedirs(a.out, exist_ok=True)
    table = {}
    for img in a.images.split(","):
        ex.IMAGE = img
        d = os.path.join(a.out, img.replace("-disk.img", ""))
        os.makedirs(d, exist_ok=True)
        pool = ex.Pool(a.workers, d)
        jobs = [("s%d" % i, t) for i, t in enumerate(items)]
        if a.repeat == 2:
            jobs += [("s%db" % i, t) for i, t in enumerate(items)]
        res = pool.run_all(jobs)
        pool.close()
        with open(os.path.join(a.out, img.replace("-disk.img", "") + ".jsonl"), "w") as f:
            for i, t in enumerate(items):
                r = res["s%d" % i][1]
                rec = {"schedule": t, "verdict": r.get("verdict", "?"), "fp": r.get("fp"), "qn": r.get("qn"),
                       "crash": crash_line(d, "s%d" % i)}
                if a.repeat == 2:
                    r2 = res["s%db" % i][1]
                    rec.update({"fp2": r2.get("fp"), "same": r2.get("fp") == r.get("fp"),
                                "crash2": crash_line(d, "s%db" % i)})
                f.write(json.dumps(rec) + "\n")
                table.setdefault(t, []).append(rec)
    for t in items:
        print("%-62s %s" % (t, " | ".join("%s %s%s%s" % (r["verdict"].replace("HYDO-ORACLE: ", "")[:44], r["fp"],
                                                       " CRASH" if r.get("crash") else "",
                                                       "" if r.get("same", True) else " DIFFERS")
                                         for r in table[t])))


if __name__ == "__main__":
    main()
