#!/usr/bin/env python3
"""Per-run probability that a generated schedule triggers bench defect d7 (skip-last-record).

    python3 d7prob.py [--seeds N]

The trigger set comes from the tail sweep and its validation (`--sweep tail`, listrun.py with
tail-validate.txt; results in hvx/bench/README.md): Kafka must be unavailable from one of the
last orders until the end of the workload, so the order whose send is uncertain is the LAST
record in the topic when the client retries -- the record the defect's lookup skips -- with
an honest disk and no power cut (a cut ends the run before the retry). The predicate below
encodes exactly the placements measured to trigger; the script counts how often each
generator draws one. Pure function of the generators: no VM runs.
"""
import argparse, os, sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "explore"))
import explore as ex

END = ex.NORDERS


def trigger(s):
    """-> the matching fault's description, or None. Measured trigger set (see module doc)."""
    if s.get("disk") != "none":         # a cut, a lying disk or I/O errors: the run ends or
        return None                     # fails another way before the retry
    k = s.get("K")
    if k and k[0] == "kafka" and k[1] + k[2] >= END and k[1] >= TRIG["kill_from"]:
        return "K"
    p = s.get("P")
    if p and p[0] == "kafka":
        if p[2] == "+" and p[1] + p[3] >= END and p[1] >= TRIG["pause_from"]:
            return "P+"
        if p[2] == "~" and TRIG["timed"](p[1], p[3], p[4]):
            return "P~"
    o = s.get("O")
    if o and o[0] == "kafka" and o[2] >= END and TRIG["oneway"](o[1], o[3], o[4]):
        return "O"
    n = s.get("N")
    if n and TRIG["net"](n):
        return "N"
    return None


# the measured trigger set, filled from the validation runs (README "d7 trigger set")
TRIG = {
    # a kill reaching the end triggers only when it starts at order 27 or later: K=kafka@26+4
    # also duplicates an order on the CONTROL image (the M50 base bug), so it is not counted
    "kill_from": END - 3,
    "pause_from": 0,                                 # a "+" pause reaching the end, any start (20..29 measured)
    "timed": lambda at, off, dur: False,             # timed pauses: none of 36 (at 25-28, dur 10-30 s) triggered
    # reply loss on Kafka over the last order only: 90% and 100% trigger, 30% and 60% do not
    # (61-89% not measured: counted as not triggering)
    "oneway": lambda a, d, kinds: (a == END - 1 and d == "resp" and len(kinds) == 1
                                   and kinds[0][0] == "loss" and kinds[0][1] >= 90),
    "net": lambda n: (n[0] == "mysql" and n[1] == END - 3 and n[2] >= END and n[3] == [("loss", 60)]),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", type=int, default=200000)
    a = ap.parse_args()
    for strat in ("random", "swarm"):
        for nf in (False, True):
            ex.NEW_FAULTS = nf
            gen = ex.generate_swarm if strat == "swarm" else ex.generate
            hits, kinds = 0, {}
            for sd in range(1, a.seeds + 1):
                s = ex.add_new_faults(gen(sd), sd, strat == "swarm")
                t = trigger(s)
                if t:
                    hits += 1
                    kinds[t] = kinds.get(t, 0) + 1
            p = hits / a.seeds
            print("%-6s %-11s p=%.5f (1 in %s)  by kind %s" % (
                strat, "+new-faults" if nf else "", p, "%.0f" % (1 / p) if p else "inf", kinds))


if __name__ == "__main__":
    main()
