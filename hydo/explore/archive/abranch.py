#!/usr/bin/env python3
"""abranch.py -- branch points from assertions (after Milestone 44).

Mid-run branching (Milestone 43) snapshots a run at a console marker and tries timing variants
from there. A fixed marker (e.g. KILLED) only finds races near that line. Here the branch point
is chosen from what the run itself did: the front door and the client emit an "event"
assertion the first time they enter an error path (server.store_error, client.outcome_unknown,
...). The strategy

  1. runs a batch of fresh schedules plainly (rb=16, so a later branch of the same schedule
     reproduces them up to the branch point);
  2. for each run, takes the event assertions it hit, in console order, and picks the one seen
     in the fewest runs so far (the rarest moment);
  3. branches the same schedule at the first occurrence of that event, with N seeded timing
     variants (BRANCH with -N: the base run is not repeated).

It is compared, with the same budget of runs, against
  random   fresh schedules, no timing;
  timing   fresh schedules, each with whole-run tick jitter;
  kill     branching every schedule at its KILLED line (Milestone 43), N variants.
Measure: distinct features per run budget -- P (sometimes/reachable assertion satisfied), D
(assertion outcome + bucketed details) and S (per-order HYDO-STATE tuples from the oracle), as
in richguided.py (no edge features: no tracing here) -- and distinct failure signatures. Every
run is a standalone-reproducible schedule; failures are re-run to check that.

  python3 abranch.py --image richcov-fix-disk.img --out /var/tmp/m45/fix --budget 200 --variants 6
"""
import argparse, collections, json, os, re, time
import explore as E
import richguided as R

EVENT_IDS = ("server.store_error", "server.kafka_recheck", "server.kafka_dedup_hit",
             "server.cache_write_skipped", "server.not_applied", "server.outcome_unknown",
             "server.redis_redialed", "client.place_failed", "client.definite_failure",
             "client.outcome_unknown", "client.retry_recovered", "client.retry_exhausted")
STRATEGIES = ("random", "timing", "kill", "assert")


def events(outdir, tag):
    """Event assertions satisfied in the WORKLOAD console of a run, first occurrence order."""
    out = []
    path = os.path.join(outdir, "consoles", "%s.log" % tag)
    if not os.path.exists(path):
        return out
    for line in open(path, errors="replace"):
        m = E.ASSERT_RE.search(line)
        if not m:
            continue
        try:
            rec = json.loads(m.group(1))
        except ValueError:
            continue
        if rec.get("id") in EVENT_IDS and rec.get("ok") and rec["id"] not in out:
            out.append(rec["id"])
    return out


class Tally:
    """Distinct features and failure signatures after each run of one strategy."""
    def __init__(self, outdir):
        self.outdir, self.seen, self.sigs, self.curve, self.fails = outdir, set(), set(), [], []

    def add(self, tag, text, res):
        fs = {f for f in R.features(self.outdir, tag) if f[0] != "E"}
        self.seen |= fs
        if E.verdict_kind(res["verdict"]) != "ok":
            self.sigs.add((E.finding_class(E.parse(text)), E.signature(res["verdict"]).split(" order=")[0]))
            self.fails.append({"tag": tag, "schedule": text, **res})
        self.curve.append((len(self.seen), len(self.sigs)))


def run_strategy(name, a, gen):
    out = os.path.join(a.out, name)
    os.makedirs(out, exist_ok=True)
    pool = E.Pool(a.workers, out)
    T = Tally(out)
    t0 = time.time()
    sd = a.seed0
    count = collections.Counter()           # (assert) runs in which each event fired
    nbranch = 0
    while len(T.curve) < a.budget:
        batch = []
        for _ in range(a.workers * 2):
            s = gen(sd)
            if name == "timing":
                s["tj"] = ("seed", E.SplitMix(sd ^ 0x7417D1CE7417D1CE).next(), 100, E.TJ_MAX, 0, 0)
            if name == "assert":
                s["rb"] = E.BRANCH_BUDGET
            if name == "kill":
                batch.append(("s%d" % sd, "BRANCH %d 100 %d KILLED %s" % (a.variants, E.TJ_MAX, E.render(s))))
            else:
                batch.append(("s%d" % sd, E.render(s)))
            sd += 1
        res = pool.run_all(batch)
        follow = []
        for tag, _ in batch:
            text, r = res[tag]
            if "runs" in r:                 # a BRANCH job
                for sub, st, rr in r["runs"]:
                    T.add(sub, st, rr)
                continue
            T.add(tag, text, r)
            if name == "assert":
                ev = events(out, tag)
                for e in ev:
                    count[e] += 1
                if ev:
                    rare = min(ev, key=lambda e: (count[e], ev.index(e)))
                    plain = E.parse(text)
                    plain.pop("rb", None)
                    follow.append(("b%s-%s" % (tag, rare), "BRANCH -%d 100 %d \"id\":\"%s\",\"ok\":true %s" % (
                        a.variants, E.TJ_MAX, rare, E.render(plain))))
        if follow:
            nbranch += len(follow)
            fres = pool.run_all(follow)
            for tag, _ in follow:
                for sub, st, rr in fres[tag][1].get("runs", []):
                    T.add(sub, st, rr)
    wall = time.time() - t0
    # every failure once more, standalone
    rep = pool.run_all([("repro-%d" % i, f["schedule"]) for i, f in enumerate(T.fails)])
    pool.close()
    for i, f in enumerate(T.fails):
        f["reproduced"] = rep["repro-%d" % i][1]["fp"] == f["fp"]
    return {"strategy": name, "runs": len(T.curve), "wall_s": round(wall), "branches": nbranch,
            "features_at": {n: T.curve[min(n, len(T.curve)) - 1][0] for n in (25, 50, 100, 150, a.budget)},
            "signatures_at": {n: T.curve[min(n, len(T.curve)) - 1][1] for n in (25, 50, 100, 150, a.budget)},
            "features": len(T.seen), "signatures": sorted("%s: %s" % s for s in T.sigs),
            "failures": len(T.fails), "reproduced": sum(1 for f in T.fails if f["reproduced"]),
            "event_counts": dict(count)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="richcov-fix-disk.img")
    ap.add_argument("--out", required=True)
    ap.add_argument("--budget", type=int, default=200, help="runs per strategy")
    ap.add_argument("--variants", type=int, default=6)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--seed0", type=int, default=1)
    ap.add_argument("--strategy", choices=["random", "swarm"], default="random", help="schedule generator")
    ap.add_argument("--only", default=",".join(STRATEGIES))
    a = ap.parse_args()
    E.IMAGE = a.image
    gen = E.generate_swarm if a.strategy == "swarm" else E.generate
    os.makedirs(a.out, exist_ok=True)
    results = []
    for name in a.only.split(","):
        r = run_strategy(name, a, gen)
        results.append(r)
        print(json.dumps(r), flush=True)
        json.dump(results, open(os.path.join(a.out, "results.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
