#!/usr/bin/env python3
# M107: a research/measurement tool. It needs the Python explorer and builder, deleted in M107
# (replaced by the hydo-explore and hydo-build crates): restore them from commit 2a58c4b with
# git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history in a scratch checkout, and run it there.
"""depth1map.py -- trigger map of a defect from two `explore.py --sweep depth1` runs (M56).

    python3 depth1map.py <defect sweep dir> <control sweep dir> [--sig REGEX]

A point TRIGGERS the defect when the defect image's verdict is a VIOLATION and the control
image's verdict for the same point is OK. Prints the triggering points grouped by fault kind,
the points that fail on both images (the control's own failures: the M50 duplicate bug and
harness artefacts), and points that did not reproduce.
"""
import argparse, collections, json, os, re, sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "explore"))
import explore as ex


def load(d):
    pts = {json.loads(l)["seed"]: json.loads(l)["point"] for l in open(os.path.join(d, "sweep-points.jsonl"))}
    runs = {json.loads(l)["seed"]: json.loads(l) for l in open(os.path.join(d, "runs.jsonl"))}
    return pts, runs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("defect")
    ap.add_argument("control")
    ap.add_argument("--sig", default="")
    ap.add_argument("--json", help="write the map here")
    a = ap.parse_args()
    pts, dr = load(a.defect)
    _, cr = load(a.control)
    trig, both, ctl_only, nonrepro = [], [], [], []
    for sd in sorted(pts):
        dv, cv = dr[sd]["verdict"], cr[sd]["verdict"]
        dbad, cbad = ex.verdict_kind(dv) == "violation", ex.verdict_kind(cv) == "violation"
        if dr[sd].get("reproduced") is False:
            nonrepro.append(pts[sd])
        if dbad and not cbad:
            trig.append((pts[sd], dv))
        elif dbad and cbad:
            both.append((pts[sd], cv))
        elif cbad:
            ctl_only.append((pts[sd], cv))
    kinds = collections.Counter(p.split("=")[0] for p, _ in trig)
    print("points %d  triggers %d %s  fail-on-both %d  control-only %d  not-reproduced %s" % (
        len(pts), len(trig), dict(kinds), len(both), len(ctl_only), nonrepro))
    sig_ok = sum(1 for _, v in trig if re.search(a.sig, v)) if a.sig else None
    if a.sig:
        print("signature match %d of %d" % (sig_ok, len(trig)))
    for p, v in trig:
        print("  T", p, "|", v[:90])
    if a.json:
        json.dump({"triggers": trig, "both": both, "control_only": ctl_only, "nonrepro": nonrepro},
                  open(a.json, "w"), indent=1)


if __name__ == "__main__":
    main()
