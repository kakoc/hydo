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
    # Milestone 59 (build-bench-m59.sh; control explore-bench-m59-control-disk.img)
    "d8-no-order-lock": ["disk=none dseed=0 | run Q=2d"],
    "d9-redis-unlocked": ["disk=none dseed=0 | run Q=4"],
    # the -pin-kr image's timing moved d10's window: X=front@12~20 passes there, @9~20 fails
    "d10-outcome-in-memory": ["disk=none dseed=0 | run X=front@%d~20" % (9 if bench.M59_SUFFIX == "-pin-kr" else 12)],
    "d11-ack-unflushed-flag": ["disk=cut dseed=0 cf=238 | run"],
    "d12-shared-key-buffer": ["disk=none dseed=0 | run Q=2"],
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
    ap.add_argument("--vcpus", type=int, default=1, help="SMP stage 3: every VM with N vCPUs (explore.py --vcpus)")
    ap.add_argument("--base", action="store_true", help="also re-run the BASE schedules on the control")
    ap.add_argument("--bg-images", action="store_true", help="M56: the explore-bench-bg-* images and their control")
    a = ap.parse_args()
    bench.BG_IMAGES = a.bg_images
    ex.VCPUS = a.vcpus
    names = ([n for n in TRIGGERS if n not in bench.M59] if a.defects == "all" else
             list(bench.M59) if a.defects == "m59" else a.defects.split(","))
    os.makedirs(a.out, exist_ok=True)
    log = open(os.path.join(a.out, "confirm.jsonl"), "a")
    for name in names:
        image, sig = bench.image_of(name), bench.DEFECTS[name][1]
        ex.IMAGE = image
        pool = ex.Pool(a.workers, os.path.join(a.out, name))
        items = []
        for i, s in enumerate(TRIGGERS[name]):
            items += [("t%d-a" % i, s), ("t%d-b" % i, s)]
        res = pool.run_all(items)
        pool.close()
        for i, s in enumerate(TRIGGERS[name]):
            ra, rb = dict(res["t%d-a" % i][1]), dict(res["t%d-b" % i][1])
            for tg, rr in (("t%d-a" % i, ra), ("t%d-b" % i, rb)):
                rr["verdict"] = bench.crash_verdict(os.path.join(a.out, name), tg, rr["verdict"], name)
            rec = {"defect": name, "image": image, "schedule": s, "verdict": ra["verdict"],
                   "fp_a": ra["fp"], "fp_b": rb["fp"], "same": ra["fp"] == rb["fp"],
                   "violation": ex.verdict_kind(ra["verdict"]) == "violation",
                   "class": ex.finding_class(ex.parse(s)),
                   "signature_match": bool(bench.re.search(sig, ra["verdict"]))}
            print(json.dumps(rec), flush=True)
            log.write(json.dumps(rec) + "\n"); log.flush()
    controls = {}
    for n in names:
        controls.setdefault(bench.control_of(n), []).append(n)
    for control, cnames in ([] if a.no_control else sorted(controls.items())):
        ex.IMAGE = control
        cdir = os.path.join(a.out, "control" if control == bench.control_of("d1-kafka-first") else control.replace("-disk.img", ""))
        pool = ex.Pool(a.workers, cdir)
        items = [("%s-t%d" % (n, i), s) for n in cnames if n != "d0-old-front-door" for i, s in enumerate(TRIGGERS[n])]
        if "d0-old-front-door" in cnames:
            items.append(("d0-old-front-door-t0", TRIGGERS["d0-old-front-door"][0]))
        if a.base and control == bench.control_of("d1-kafka-first"):
            for i, s in enumerate(BASE):
                items += [("base%d-a" % i, s), ("base%d-b" % i, s)]
        res = pool.run_all(items)
        pool.close()
        for tag, (s, r) in sorted(res.items()):
            dn = tag.rsplit("-t", 1)[0]
            r = dict(r, verdict=bench.crash_verdict(cdir, tag, r["verdict"], dn))
            rec = {"control": control, "for": tag, "schedule": s, "verdict": r["verdict"], "fp": r["fp"],
                   "ok": ex.verdict_kind(r["verdict"]) == "ok"}
            print(json.dumps(rec), flush=True)
            log.write(json.dumps(rec) + "\n"); log.flush()


if __name__ == "__main__":
    main()
