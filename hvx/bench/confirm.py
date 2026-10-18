#!/usr/bin/env python3
"""confirm.py -- confirm each benchmark defect with its hand-written trigger schedule(s).

For every defect: run each trigger schedule twice on the defect's image (both must be a
VIOLATION with the same fingerprint), and once on the control image (explore-nf-disk.img,
the unpatched front door in the same guest), which must pass (HYDO-ORACLE: OK).

  python3 confirm.py --out /var/tmp/m46-bench/confirm [--defects d1-kafka-first,...] [--workers 3]
"""
import argparse, json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import run as bench  # noqa: E402
ex = bench.ex

# Trigger schedules: every store durable, honest disk (no M=/R= knobs, disk none or cut).
TRIGGERS = {
    "d0-old-front-door": ["disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24"],
    "d1-kafka-first": ["disk=none dseed=0 | run K=mysql@10+1"],
    "d2-batched-publish-flag": ["disk=cut dseed=0 | run C=12"],
    "d3-blind-retry": ["disk=none dseed=0 | run P=kafka@5+2",
                       "disk=none dseed=0 | run O=kafka@5-10:resp:loss,100%"],
    "d4-produce-timeout-definite": ["disk=cut dseed=0 | run O=kafka@5-10:resp:loss,100% C=12",
                                "disk=cut dseed=0 | run P=kafka@5+2 C=8"],
    "d5-myisam": ["disk=cut dseed=0 | run C=15"],
    "d6-client-deadline-definite": ["disk=none dseed=0 | run P=kafka@5+2"],
    "d7-skip-last-record": ["disk=none dseed=0 | run P=kafka@28+2"],
}

# Schedules that fail on the CONTROL itself (bugs of the fixed front door, README "A bug in
# the fixed front door"): run twice on the control, both must be the same VIOLATION.
BASE = ["disk=none dseed=0 | run O=kafka@25-30:resp:loss,100%"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--defects", default="all")
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--no-control", action="store_true")
    ap.add_argument("--base", action="store_true", help="also re-run the BASE schedules on the control")
    a = ap.parse_args()
    names = list(TRIGGERS) if a.defects == "all" else a.defects.split(",")
    os.makedirs(a.out, exist_ok=True)
    log = open(os.path.join(a.out, "confirm.jsonl"), "a")
    for name in names:
        image, sig = bench.DEFECTS[name]
        ex.IMAGE = image
        pool = ex.Pool(a.workers, os.path.join(a.out, name))
        items = []
        for i, s in enumerate(TRIGGERS[name]):
            items += [("t%d-a" % i, s), ("t%d-b" % i, s)]
        res = pool.run_all(items)
        pool.close()
        for i, s in enumerate(TRIGGERS[name]):
            ra, rb = res["t%d-a" % i][1], res["t%d-b" % i][1]
            rec = {"defect": name, "image": image, "schedule": s, "verdict": ra["verdict"],
                   "fp_a": ra["fp"], "fp_b": rb["fp"], "same": ra["fp"] == rb["fp"],
                   "violation": ex.verdict_kind(ra["verdict"]) == "violation",
                   "class": ex.finding_class(ex.parse(s)),
                   "signature_match": bool(bench.re.search(sig, ra["verdict"]))}
            print(json.dumps(rec), flush=True)
            log.write(json.dumps(rec) + "\n"); log.flush()
    if not a.no_control:
        ex.IMAGE = bench.CONTROL
        pool = ex.Pool(a.workers, os.path.join(a.out, "control"))
        items = [("%s-t%d" % (n, i), s) for n in names if n != "d0-old-front-door" for i, s in enumerate(TRIGGERS[n])]
        if "d0-old-front-door" in names:
            items.append(("d0-old-front-door-t0", TRIGGERS["d0-old-front-door"][0]))
        if a.base:
            for i, s in enumerate(BASE):
                items += [("base%d-a" % i, s), ("base%d-b" % i, s)]
        res = pool.run_all(items)
        pool.close()
        for tag, (s, r) in sorted(res.items()):
            rec = {"control": bench.CONTROL, "for": tag, "schedule": s, "verdict": r["verdict"], "fp": r["fp"],
                   "ok": ex.verdict_kind(r["verdict"]) == "ok"}
            print(json.dumps(rec), flush=True)
            log.write(json.dumps(rec) + "\n"); log.flush()


if __name__ == "__main__":
    main()
