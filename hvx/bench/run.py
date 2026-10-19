#!/usr/bin/env python3
"""run.py -- run explorer strategies on the bug benchmark (hvx/bench/README.md).

For every defect image and repetition, runs one strategy's schedules in seed order and stops at
the first DETECTION: a run whose schedule has durable settings and an honest disk (explore.py
class "durable-defaults") and whose verdict is a VIOLATION. The control image
(explore-nf-disk.img, the same guest with the unpatched front door) never fails in that class
(0 in 484 schedules + sweeps, Milestone 46), so such a failure is attributable to the planted
defect; run.py also records whether the verdict matches the defect's own signature (README).
The detecting run is re-run once and its fingerprint compared.

  python3 run.py --strategy random|swarm [--new-faults] --defects all --reps 1-3 --budget 60 \\
                 --workers 3 --out /var/tmp/m46-bench/random
  python3 run.py --strategy portfolio --defects all --reps 1-3 --budget 60 --workers 3 --out ...
  python3 run.py --table /var/tmp/m46-bench/*      # per defect x strategy summary

Seeds: repetition r uses seeds r*1000+1 .. r*1000+budget (the M42/M44 repetitions). The
portfolio strategy runs hvx/explore/portfolio.py for the full budget (it has no early stop) and
reads its runs.jsonl in exploration order.

Uses hvx/explore/explore.py as a library (read-only): run from a tree whose hvx/hvx_explore is
built; images are read from ~/hydo/build.
"""
import argparse, glob, json, os, re, statistics, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
EXPLORE = os.path.join(os.path.dirname(HERE), "explore")
sys.path.insert(0, EXPLORE)
import explore as ex  # noqa: E402

# defect -> (image, signature regex). d0 is the pre-M39 front door (its own, older guest:
# it ignores the M46 tokens P= T= O=).
DEFECTS = {
    "d0-old-front-door":          ("explore-disk.img", r"never ACKed"),
    "d1-kafka-first":             ("explore-bench-d1-kafka-first-disk.img", r"present \d+ times|told it failed|kafka_exactly_once|told_failed"),
    "d2-batched-publish-flag":    ("explore-bench-d2-batched-publish-flag-disk.img", r"not marked published|acked_marked_published"),
    "d3-blind-retry":             ("explore-bench-d3-blind-retry-disk.img", r"present \d+ times|kafka_exactly_once"),
    "d4-produce-timeout-definite": ("explore-bench-d4-produce-timeout-definite-disk.img", r"told it failed|told_failed"),
    "d5-myisam":                  ("explore-bench-d5-myisam-disk.img", r"mysql|in_mysql|published"),
    "d6-client-deadline-definite": ("explore-bench-d6-client-deadline-definite-disk.img", r"told it failed|told_failed"),
    "d7-skip-last-record":        ("explore-bench-d7-skip-last-record-disk.img", r"present \d+ times|kafka_exactly_once"),
}
CONTROL = "explore-nf-disk.img"


def control_of(defect):
    """The unpatched image in the same guest: d0 runs the pre-M46 guest, so its control is the
    fixed front door in that guest (explore-fix-disk.img)."""
    return "explore-fix-disk.img" if defect.startswith("d0-") else CONTROL


class Control:
    """Attribution: a candidate detection counts only if the SAME schedule passes on the
    control image. (The fixed front door has a bug of its own -- see README -- so a failure in
    class durable-defaults is not automatically the planted defect's.)"""
    def __init__(self, a, defect):
        self.image, self.cache = control_of(defect), {}
        self.dout = os.path.join(a.out, defect, "control")
        os.makedirs(self.dout, exist_ok=True)
        saved = ex.IMAGE
        ex.IMAGE = self.image
        self.pool = ex.Pool(1, self.dout)
        # explore.Worker reads the global IMAGE when its thread starts: wait for the boot
        # before another pool may change it
        t0 = time.time()
        while self.pool.workers[0].boot_secs is None and self.pool.workers[0].is_alive() and time.time() - t0 < 900:
            time.sleep(1)
        ex.IMAGE = saved

    def passes(self, text):
        if text not in self.cache:
            tag = "c%d" % len(self.cache)
            r = self.pool.run_all([(tag, text)])[tag][1]
            self.cache[text] = r
        return ex.verdict_kind(self.cache[text]["verdict"]) == "ok", self.cache[text]

    def close(self):
        self.pool.close()


def detects(rec):
    return rec["class"] == "durable-defaults" and ex.verdict_kind(rec["verdict"]) == "violation"


def make_rec(outdir, sd, text, r):
    s = ex.parse(text)
    rec = {"seed": sd, "schedule": text, "class": ex.finding_class(s), **r}
    rec["assert_failures"] = ex.assert_failures(outdir, "seed-%d" % sd)
    if rec["assert_failures"] and ex.verdict_kind(r["verdict"]) == "ok":
        rec["verdict"] = "VIOLATION assertion " + ",".join(rec["assert_failures"])
    return rec


def schedules(strategy, seeds, new_faults):
    gen = ex.generate_swarm if strategy == "swarm" else ex.generate
    ex.NEW_FAULTS = new_faults
    return [(sd, ex.render(ex.add_new_faults(gen(sd), sd, strategy == "swarm"))) for sd in seeds]


def run_seeded(a, defect, image, sig):
    """random / swarm with early stop: a sliding window of 2*workers schedules in seed order."""
    dout = os.path.join(a.out, defect)
    os.makedirs(dout, exist_ok=True)
    ctl = Control(a, defect)          # first: it sets the global IMAGE while its worker starts
    ex.IMAGE = image
    pool = ex.Pool(a.workers, dout)
    results = []
    for rep in a.reps:
        seeds = list(range(rep * 1000 + 1, rep * 1000 + a.budget + 1))
        items = schedules(a.strategy, seeds, a.new_faults)
        done, nxt, outstanding, found, base = {}, 0, 0, None, []
        t0 = time.time()
        while True:
            while nxt < len(items) and outstanding < 2 * a.workers and found is None:
                sd, text = items[nxt]
                pool.jobs.put(("seed-%d" % sd, text)); nxt += 1; outstanding += 1
            if outstanding == 0:
                break
            tag, text, r = pool.results.get(); outstanding -= 1
            sd = int(tag.split("-")[1])
            done[sd] = make_rec(dout, sd, text, r)
            if found is None:       # first detection = lowest detecting seed with all lower seeds done
                for i, (s2, _) in enumerate(items):
                    if s2 not in done:
                        break
                    if detects(done[s2]) and not done[s2].get("base_bug"):
                        ok, cr = ctl.passes(done[s2]["schedule"])
                        if ok:
                            found = (i + 1, done[s2])
                            break
                        done[s2]["base_bug"] = True     # the control fails too: not this defect
                        done[s2]["control_verdict"] = cr["verdict"]
                        base.append({"run": i + 1, "seed": s2, "schedule": done[s2]["schedule"],
                                     "verdict": done[s2]["verdict"], "control_verdict": cr["verdict"]})
        runs = open(os.path.join(dout, "runs-rep%d.jsonl" % rep), "w")
        for sd in sorted(done):
            runs.write(json.dumps(done[sd]) + "\n")
        runs.close()
        res = {"defect": defect, "strategy": a.label, "rep": rep, "budget": a.budget, "image": image,
               "runs_done": len(done), "wall_s": round(time.time() - t0, 1), "base_bug_hits": base}
        if found:
            n, rec = found
            rr = pool.run_all([("repro-%d" % rec["seed"], rec["schedule"])])["repro-%d" % rec["seed"]][1]
            res.update({"detected_at": n, "seed": rec["seed"], "schedule": rec["schedule"],
                        "verdict": rec["verdict"], "fp": rec["fp"], "repro_fp": rr["fp"],
                        "reproduced": rr["fp"] == rec["fp"],
                        "signature_match": bool(re.search(sig, rec["verdict"]))})
        else:
            res["detected_at"] = None
        results.append(res)
        print(json.dumps(res), flush=True)
    pool.close()
    ctl.close()
    return results


def run_portfolio(a, defect, image, sig):
    ctl = Control(a, defect)
    results = []
    for rep in a.reps:
        dout = os.path.join(a.out, defect, "rep%d" % rep)
        t0 = time.time()
        subprocess.run([sys.executable, os.path.join(EXPLORE, "portfolio.py"), "--image", image,
                        "--budget", str(a.budget), "--rep", str(rep), "--workers", str(a.workers),
                        "--out", dout], check=False, stdout=subprocess.DEVNULL)
        recs = [json.loads(l) for l in open(os.path.join(dout, "runs.jsonl"))] if os.path.exists(os.path.join(dout, "runs.jsonl")) else []
        recs.sort(key=lambda r: r["seed"])
        repro = {}      # portfolio.py re-runs every failure; the result is in report.json
        rp = os.path.join(dout, "report.json")
        if os.path.exists(rp):
            for f in json.load(open(rp)).get("failures", []):
                repro[f["schedule"]] = f.get("reproduced")
        res = {"defect": defect, "strategy": a.label, "rep": rep, "budget": a.budget, "image": image,
               "runs_done": len(recs), "wall_s": round(time.time() - t0, 1), "detected_at": None,
               "base_bug_hits": []}
        for i, rec in enumerate(recs):
            if detects(rec):
                ok, cr = ctl.passes(rec["schedule"])
                if not ok:
                    res["base_bug_hits"].append({"run": i + 1, "schedule": rec["schedule"], "verdict": rec["verdict"],
                                                 "control_verdict": cr["verdict"]})
                    continue
                res.update({"detected_at": i + 1, "schedule": rec["schedule"], "verdict": rec["verdict"],
                            "fp": rec.get("fp"), "arm": rec.get("arm"),
                            "reproduced": repro.get(rec["schedule"], rec.get("reproduced")),
                            "signature_match": bool(re.search(sig, rec["verdict"]))})
                break
        results.append(res)
        print(json.dumps(res), flush=True)
    ctl.close()
    return results


def table(dirs):
    rows = {}
    for d in dirs:
        p = os.path.join(d, "results.jsonl")
        if not os.path.exists(p):
            continue
        for l in open(p):
            r = json.loads(l)
            rows.setdefault(r["defect"], {}).setdefault(r["strategy"], []).append(r)
    strats = sorted({s for v in rows.values() for s in v})
    print("| defect | " + " | ".join(strats) + " |")
    print("|---|" + "---|" * len(strats))
    found = {s: 0 for s in strats}
    for dfc in sorted(rows):
        cells = []
        for s in strats:
            rs = sorted(rows[dfc].get(s, []), key=lambda r: r["rep"])
            if not rs:
                cells.append("-"); continue
            hits = [r["detected_at"] for r in rs if r["detected_at"]]
            if hits:
                found[s] += 1
            runs = ", ".join(str(r["detected_at"]) if r["detected_at"] else "-" for r in rs)
            med = statistics.median(hits) if hits else None
            cells.append("%d/%d (%s)%s" % (len(hits), len(rs), runs, " med %g" % med if med else ""))
        print("| %s | %s |" % (dfc, " | ".join(cells)))
    print("| defects found (any rep) | " + " | ".join(str(found[s]) for s in strats) + " |")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--strategy", choices=["random", "swarm", "portfolio"])
    ap.add_argument("--new-faults", action="store_true")
    ap.add_argument("--defects", default="all")
    ap.add_argument("--reps", default="1-3")
    ap.add_argument("--budget", type=int, default=60)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--out")
    ap.add_argument("--table", nargs="*")
    a = ap.parse_args()
    if a.table is not None:
        table(a.table)
        return
    lo, hi = [int(x) for x in a.reps.split("-")] if "-" in a.reps else (int(a.reps), int(a.reps))
    a.reps = list(range(lo, hi + 1))
    a.label = a.strategy + ("+nf" if a.new_faults else "")
    names = list(DEFECTS) if a.defects == "all" else a.defects.split(",")
    os.makedirs(a.out, exist_ok=True)
    out = open(os.path.join(a.out, "results.jsonl"), "a")
    for name in names:
        image, sig = DEFECTS[name]
        rs = run_portfolio(a, name, image, sig) if a.strategy == "portfolio" else run_seeded(a, name, image, sig)
        for r in rs:
            out.write(json.dumps(r) + "\n")
        out.flush()


if __name__ == "__main__":
    main()
