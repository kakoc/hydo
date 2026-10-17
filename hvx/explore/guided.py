#!/usr/bin/env python3
"""guided.py -- coverage-GUIDED fault exploration (Milestone 41).

Builds on explore.py's run engine (the hvx_explore serve workers, the schedule format, the
failure/shrink logic) but replaces the fixed seed sweep with an AFL-style loop:

  * a CORPUS of schedules, seeded from a few random ones;
  * each run is traced with Intel PT, filtered to the front door's own text (cheap: a few
    MB, ~4 s to decode), and reduced to a set of (from,to) control-flow EDGES with bucketed
    hit counts -- exactly AFL's coverage notion;
  * a schedule is ADDED to the corpus iff it covered an edge/bucket not seen before;
  * the next schedule is a MUTATION of a corpus entry, preferring entries that have been
    mutated least (round-robin "energy"), mutating one or more schedule fields.

Everything stays deterministic: a schedule string renders to one guest run with one
fingerprint, failures are re-run + shrunk by explore.py's code, and the coverage of a given
schedule is itself reproducible (two decodes of two traces of one schedule are byte-equal).

  python3 guided.py --image explore-ptfix-disk.img \
      --elf ~/hydo/build/explore-ptfix-hydostack --out /var/tmp/g/fix --budget 200

Modes:
  --mode guided   (default) the loop above
  --mode random   the same budget, schedules from fresh random seeds (the baseline to beat)
Both record coverage, so the two are directly comparable. --repeat N runs N independent
starts (different --rng0) and reports the spread. --measure-cost times a few traced vs
untraced runs. See the evaluation harness evaluate.sh.
"""
import argparse, json, os, random, subprocess, sys, time
import explore as E

HERE = os.path.dirname(os.path.abspath(__file__))


def edges_of(outdir, tag):
    """The (from,to,bucket) coverage features of one run's edge file, AFL buckets."""
    path = os.path.join(outdir, "cov", "%s.edges" % tag)
    feats = set()
    if not os.path.exists(path):
        return feats
    for line in open(path):
        a, b, c = line.split()
        c = int(c)
        bucket = (0 if c == 1 else 1 if c == 2 else 2 if c == 3 else 3 if c < 8 else
                  4 if c < 16 else 5 if c < 32 else 6 if c < 128 else 7)
        feats.add((a, b, bucket))
    return feats


# ---------------------------------------------------------------- schedule mutation
SVCS = E.SVCS
NET_KINDS = ["delay", "loss", "duplicate", "reorder", "corrupt"]


def mutate(s, rng):
    """One random edit of schedule dict s. Durability knobs (M/R) and the disk class are
    kept SEPARABLE so a finding's class stays meaningful; a mutation may add or drop one."""
    t = dict(s)
    op = rng.randrange(9)
    if op == 0:                                   # kill: add / move / drop
        if "K" in t and rng.random() < 0.3:
            t.pop("K")
        else:
            at = rng.randrange(0, E.NORDERS - 3)
            t["K"] = (rng.choice(SVCS), at, rng.randint(1, min(4, E.NORDERS - at)))
    elif op == 1:                                 # netem: add / move / drop
        if "N" in t and rng.random() < 0.3:
            t.pop("N")
        else:
            a = rng.randrange(0, E.NORDERS - 1)
            b = rng.randint(a + 1, min(E.NORDERS, a + 15))
            k = rng.choice(NET_KINDS)
            v = {"delay": rng.randint(10, 300), "loss": rng.randint(5, 60), "duplicate": rng.randint(5, 50),
                 "reorder": rng.randint(10, 50), "corrupt": rng.randint(1, 10)}[k]
            t["N"] = (rng.choice(SVCS), a, b, [(k, v)])
    elif op == 2:                                 # power-cut point
        t["C"] = rng.randint(1, E.NORDERS)
        if t["disk"] == "none" and rng.random() < 0.5:
            t["disk"] = "cut"
    elif op == 3:                                 # disk class
        t["disk"] = rng.choice(["none", "none", "cut", "ioerr:1", "ioerr:2", "lying:%d" % rng.randint(2, 12)])
        if t["disk"] in ("cut",) or t["disk"].startswith("lying"):
            t.setdefault("C", rng.randint(1, E.NORDERS))
    elif op == 4:
        t["dseed"] = rng.getrandbits(64)
    elif op == 5:                                 # MySQL durability knob
        if "M" in t and rng.random() < 0.5:
            t.pop("M")
        else:
            t["M"] = rng.choice([0, 2])
    elif op == 6:                                 # Redis durability knob
        if "R" in t and rng.random() < 0.5:
            t.pop("R")
        else:
            t["R"] = rng.choice(["everysec", "no"])
    elif op == 7 and "K" in t:                    # tweak kill timing
        svc, at, down = t["K"]
        t["K"] = (svc, max(0, min(E.NORDERS - 1, at + rng.randint(-3, 3))), down)
    elif op == 8 and "N" in t:                    # tweak netem params
        svc, a, b, kinds = t["N"]
        kinds = [(k, max(1, v + rng.randint(-20, 20))) for k, v in kinds]
        t["N"] = (svc, a, b, kinds)
    if t["disk"] == "none" and "C" not in s and "C" in t and rng.random() < 0.5:
        pass
    return t


def run_batch(pool, items, outdir):
    """items: list of (tag, dict). Runs them, returns tag -> (dict, result, feats)."""
    res = pool.run_all([(t, E.render(d)) for t, d in items])
    out = {}
    for tag, d in items:
        text, r = res[tag]
        out[tag] = (d, r, edges_of(outdir, tag))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="explore-ptfix-disk.img")
    ap.add_argument("--elf", required=True, help="symbol-enabled hydostack of that image (PT filter + decode)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--budget", type=int, default=200, help="number of guest runs")
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--mode", choices=["guided", "random"], default="guided")
    ap.add_argument("--rng0", type=int, default=1)
    ap.add_argument("--stop-on", default=None, help="stop early when a finding of this class appears")
    ap.add_argument("--no-shrink", action="store_true", help="re-run failures but do not shrink them")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    E.IMAGE = a.image
    E.EXTRA_ENV[:] = ["HVX_EXPLORE_PT_ELF=" + os.path.expanduser(a.elf)]
    rng = random.Random(a.rng0)
    pool = E.Pool(a.workers, a.out)

    corpus = []                 # list of dicts kept for having added coverage
    energy = []                 # times each corpus entry has been mutated
    global_feats = set()
    global_plain = set()        # the same edges without hit-count buckets
    runs = open(os.path.join(a.out, "runs.jsonl"), "w")
    growth = []                 # (run index, |global_feats|)
    fails = []
    first_class = {}            # class -> run index of its first failure
    t0 = time.time()
    n = 0
    SEEDS0 = 6                  # initial random schedules

    def consider(tag, d, r, feats):
        nonlocal n
        n += 1
        new = feats - global_feats
        global_feats.update(feats)
        global_plain.update((x, y) for x, y, _ in feats)
        growth.append((n, len(global_feats), len(global_plain)))
        cls = E.finding_class(d)
        vk = E.verdict_kind(r["verdict"])
        af = E.assert_failures(a.out, tag)
        if af and vk == "ok":
            r["verdict"] = "VIOLATION assertion " + ",".join(af)
            vk = "violation"
        rec = {"run": n, "tag": tag, "schedule": E.render(d), "class": cls, "new_edges": len(new),
               "run_edges": len(feats), "corpus": len(corpus), "global_edges": len(global_feats),
               "global_plain_edges": len(global_plain), **r, "assert_failures": af}
        runs.write(json.dumps(rec) + "\n"); runs.flush()
        if vk != "ok":
            fails.append(rec)
            first_class.setdefault(cls, n)
        if a.mode == "guided" and new:
            corpus.append(d); energy.append(0)
        return rec

    # seed the corpus
    seeds = [("seed-%d" % i, E.generate(rng.getrandbits(64))) for i in range(SEEDS0)]
    for tag, (d, r, feats) in run_batch(pool, seeds, a.out).items():
        consider(tag, dict(seeds and next(x for t, x in seeds if t == tag)), r, feats)

    while n < a.budget:
        batch = []
        for i in range(a.workers):
            if a.mode == "random" or not corpus:
                batch.append(("r-%d-%d" % (n, i), E.generate(rng.getrandbits(64))))
            else:
                j = min(range(len(corpus)), key=lambda k: energy[k])
                energy[j] += 1
                batch.append(("m-%d-%d" % (n, i), mutate(corpus[j], rng)))
        for tag, (d, r, feats) in run_batch(pool, batch, a.out).items():
            consider(tag, dict(next(x for t, x in batch if t == tag)), r, feats)
        if a.stop_on and a.stop_on in first_class:
            break
    runs.close()

    # reproduce + shrink the failures explore.py-style
    rep = pool.run_all([("repro-%d" % f["run"], f["schedule"]) for f in fails])
    for f in fails:
        rr = rep["repro-%d" % f["run"]][1]
        f["reproduced"] = (rr["fp"] == f["fp"])
    shrunk = {}
    slog = open(os.path.join(a.out, "shrink.jsonl"), "w")
    reps = {}
    for f in fails:
        if E.verdict_kind(f["verdict"]) == "violation":
            key = (f["class"], E.signature(f["verdict"]).split(" order=")[0])
            reps.setdefault(key, f)
    for key, f in ([] if a.no_shrink else reps.items()):
        m, mv, t = E.shrink(pool, E.parse(f["schedule"]), f["verdict"], slog)
        mt = E.render(m)
        conf = pool.run_all([("min-a", mt), ("min-b", mt)])
        shrunk[key] = {"from_run": f["run"], "minimal": mt, "verdict": mv, "min_class": E.finding_class(m),
                       "fp_a": conf["min-a"][1]["fp"], "fp_b": conf["min-b"][1]["fp"]}
    slog.close()
    pool.close()

    summary = {"mode": a.mode, "budget": a.budget, "runs": n, "workers": a.workers,
               "wall_s": round(time.time() - t0, 1), "runs_per_hour": round(n / (time.time() - t0) * 3600, 1),
               "corpus": len(corpus), "global_edges": len(global_feats),
               "global_plain_edges": len(global_plain),
               "failures": len(fails), "reproduced": sum(1 for f in fails if f.get("reproduced")),
               "first_failure_run_by_class": first_class}
    json.dump({"summary": summary, "growth": growth,
               "shrunk": [{"class": k[0], "signature": k[1], **v} for k, v in shrunk.items()]},
              open(os.path.join(a.out, "report.json"), "w"), indent=1)
    with open(os.path.join(a.out, "growth.csv"), "w") as g:
        g.write("run,global_edges,global_plain_edges\n")
        for r, e, pl in growth:
            g.write("%d,%d,%d\n" % (r, e, pl))
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
