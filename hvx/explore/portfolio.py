#!/usr/bin/env python3
"""portfolio.py -- a portfolio scheduler for the fault explorer (Milestone 44).

Splits one run budget across several schedule generators ("arms") with a bandit, instead of
committing to one strategy up front:

  random   explore.generate(seed)                       (the M38 generator)
  swarm    explore.generate_swarm(seed)                 (M42 swarm testing)
  timing   the random generator with seeded tick jitter (M43, as `explore.py --timing`)
  branch   a swarm schedule with a service kill, then mid-run branching at KILLED: the base run
           plus --variants timing variants from a snapshot (M43, as `explore.py --branch`)
  sweep    the M42 exhaustive spaces (kill, cut, net, killcut) in a seeded order; a FINITE arm,
           dropped when it is used up

Reward. A run's "features" are: (class, signature) of a failure; every `sometimes`/`reachable`
property that held (`ok=true`); every `always`/`unreachable` property that failed. A run is a
success when it produced at least one feature no earlier run had. Each arm keeps a Beta(1+s, 1+f)
posterior over its per-run success rate (Thompson sampling); a branch pull costs variants+1 runs
and adds that many outcomes.

Determinism. The scheduler is a pure function of (--rep, the run results): pulls are chosen in
batches of 2*workers from a seeded RNG, run, and the posteriors are updated in pull order -- never
in completion order. Every run is still a plain schedule string (a branch variant is reported as
its standalone `... tj=<v> tjlo=<k> rb=16 | ...` form), so it can be re-run with
`explore.py --schedule` or `hvx_explore one`, and every failure is re-run once and compared.

  python3 portfolio.py --image explore-fix-disk.img --budget 60 --rep 1 --workers 2 --out /var/tmp/m44-port/fix-portfolio-1

Seeds: arm seeds start at rep*1000+1 (random, swarm -- the same schedules as M42's repetitions),
rep*1000+301 (timing), rep*1000+601 (branch). runs.jsonl uses the global run number as "seed",
so compare.py reads it in exploration order.
"""
import argparse, json, os, random, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import explore as ex  # noqa: E402

ARMS = ["random", "swarm", "timing", "branch", "sweep"]
SWEEPS = ["kill", "cut", "net", "killcut"]
BRANCH_MARKER = "KILLED"


# ---------------------------------------------------------------- features (the reward)
def run_features(outdir, tag, rec):
    """The features of one run: failure signature + property outcomes from its console(s)."""
    feats = set()
    if ex.verdict_kind(rec["verdict"]) != "ok":
        feats.add(("sig", rec["class"], ex.signature(rec["verdict"])))
    for suffix in ("", ".recovery"):
        p = os.path.join(outdir, "consoles", "%s%s.log" % (tag, suffix))
        if not os.path.exists(p):
            continue
        for line in open(p, errors="replace"):
            m = ex.ASSERT_RE.search(line)
            if not m:
                continue
            try:
                r = json.loads(m.group(1))
            except ValueError:
                continue
            t, ok, pid = r.get("t"), r.get("ok"), r.get("id", "?")
            if t in ("sometimes", "reachable") and ok is True:
                feats.add(("prop", pid))
            elif (t in ("always", "always_or_unreachable") and ok is False) or t == "unreachable":
                feats.add(("propfail", pid))
    return feats


def fmt_feat(f):
    return ":".join(f[:1] + tuple(str(x) for x in f[1:]))


# ---------------------------------------------------------------- arms
class Arm:
    def __init__(self, name, rep, variants):
        self.name, self.rep, self.variants = name, rep, variants
        self.i = 0                  # next index into this arm's stream
        self.s = self.f = 0         # Beta posterior counts (successes / failures, per run)
        self.pulls = self.runs = self.new = self.fails = 0
        self.exhausted = False
        if name == "sweep":
            pts = []
            for w in SWEEPS:
                pts += [(w + ":" + lab, sch) for lab, sch in ex.sweep(w)]
            random.Random(0x5EE9 + rep).shuffle(pts)
            self.points = pts

    def cost(self):
        return self.variants + 1 if self.name == "branch" else 1

    def next(self):
        """-> (label, job text) for the next pull of this arm."""
        base = self.rep * 1000
        if self.name == "random":
            sd = base + 1 + self.i; self.i += 1
            return "seed %d" % sd, ex.render(ex.generate(sd))
        if self.name == "swarm":
            sd = base + 1 + self.i; self.i += 1
            return "seed %d" % sd, ex.render(ex.generate_swarm(sd))
        if self.name == "timing":       # the seeds `explore.py --timing` gives a jitter to
            ex.TIMING = True
            while True:
                sd = base + 301 + self.i; self.i += 1
                s = ex.add_timing(ex.generate(sd), sd)
                if "tj" in s:
                    ex.TIMING = False
                    return "seed %d" % sd, ex.render(s)
        if self.name == "branch":       # a kill (the marker) and no power cut (not snapshottable)
            while True:
                sd = base + 601 + self.i; self.i += 1
                s = ex.generate_swarm(sd)
                if "K" in s and "C" not in s and s["disk"] != "cut":
                    pm = ex.TJ_PM[len(ex.TJ_PM) // 2]
                    return "seed %d" % sd, "BRANCH %d %d %d %s %s" % (self.variants, pm, ex.TJ_MAX, BRANCH_MARKER, ex.render(s))
        if self.name == "sweep":
            lab, sch = self.points[self.i]; self.i += 1
            if self.i >= len(self.points):
                self.exhausted = True
            return lab, ex.render(sch)
        raise ValueError(self.name)


# ---------------------------------------------------------------- the scheduler
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="explore-fix-disk.img")
    ap.add_argument("--budget", type=int, default=60, help="runs (a branch pull counts variants+1)")
    ap.add_argument("--rep", type=int, default=1, help="repetition: picks the seed ranges and the scheduler's RNG")
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--arms", default=",".join(ARMS))
    ap.add_argument("--variants", type=int, default=3, help="timing variants per branch pull")
    ap.add_argument("--batch", type=int, default=0, help="pulls per scheduling round (default 2*workers)")
    ap.add_argument("--shrink", action="store_true", help="shrink one failure per signature (off: like the M42 runs)")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    ex.IMAGE = a.image
    os.makedirs(a.out, exist_ok=True)
    arms = [Arm(n, a.rep, a.variants) for n in a.arms.split(",")]
    rng = random.Random(0xB4D17 * 1000003 + a.rep)
    batch = a.batch or 2 * a.workers
    t0 = time.time()
    pool = ex.Pool(a.workers, a.out)

    seen = {}                       # feature -> global run number that first produced it
    runs, timeline, used, npull = [], [], 0, 0
    runs_f = open(os.path.join(a.out, "runs.jsonl"), "w")
    warm = [x for x in arms]        # one pull per arm first, in a fixed order

    while used < a.budget:
        # ---- choose a batch of pulls
        plan, planned = [], 0
        while len(plan) < batch and used + planned < a.budget:
            live = [x for x in arms if not x.exhausted and used + planned + x.cost() <= a.budget]
            if not live:
                break
            if warm:
                arm = warm.pop(0)
                if arm not in live:
                    continue
                why = "warm-up"
            else:
                draws = [(rng.betavariate(1 + x.s, 1 + x.f), x) for x in live]
                arm = max(draws, key=lambda d: d[0])[1]
                why = "thompson %.3f" % max(d[0] for d in draws)
            npull += 1
            label, job = arm.next()
            plan.append((npull, arm, label, job, why))
            planned += arm.cost()
        if not plan:
            break
        res = pool.run_all([("p%d" % pn, job) for pn, _, _, job, _ in plan])
        # ---- update in pull order
        for pn, arm, label, job, why in plan:
            text, r = res["p%d" % pn]
            if job.startswith("BRANCH "):
                subs = [(sub, st, rr) for sub, st, rr in r.get("runs", [])]
                if not subs:        # worker error: count it as one failed run
                    subs = [("p%d" % pn, job, {"fp": "0", "secs": 0, "verdict": "ERROR branch: no runs"})]
            else:
                subs = [("p%d" % pn, text, r)]
            arm.pulls += 1
            for sub, st, rr in subs:
                used += 1
                s = ex.parse(st) if not st.startswith("BRANCH") else {"disk": "none"}
                rec = {"seed": used, "arm": arm.name, "pull": pn, "label": label, "tag": sub, "schedule": st,
                       "class": ex.finding_class(s), **rr}
                af = ex.assert_failures(a.out, sub)
                rec["assert_failures"] = af
                if af and ex.verdict_kind(rr["verdict"]) == "ok":
                    rec["verdict"] = "VIOLATION assertion " + ",".join(af)
                if job.startswith("BRANCH "):
                    rec["branch_tjlo"] = r.get("tjlo")
                feats = run_features(a.out, sub, rec)
                new = sorted(fmt_feat(f) for f in feats if f not in seen)
                for f in feats:
                    seen.setdefault(f, used)
                rec["new_features"] = new
                arm.runs += 1
                arm.new += len(new)
                if new:
                    arm.s += 1
                else:
                    arm.f += 1
                if ex.verdict_kind(rec["verdict"]) != "ok":
                    arm.fails += 1
                runs.append(rec)
                runs_f.write(json.dumps(rec) + "\n")
                runs_f.flush()
            timeline.append({"pull": pn, "arm": arm.name, "why": why, "label": label, "runs_used": used,
                             "posterior": {x.name: [x.s, x.f] for x in arms}})
    runs_f.close()
    t_explore = time.time() - t0

    # ---- reproduce every failure from its plain schedule string
    fails = [r for r in runs if ex.verdict_kind(r["verdict"]) != "ok"]
    rep = pool.run_all([("repro-%d" % r["seed"], r["schedule"]) for r in fails])
    for r in fails:
        rr = rep["repro-%d" % r["seed"]][1]
        r["repro_fp"], r["repro_verdict"] = rr["fp"], rr["verdict"]
        r["reproduced"] = rr["fp"] == r["fp"]

    shrunk = {}
    if a.shrink:
        slog = open(os.path.join(a.out, "shrink.jsonl"), "w")
        reps_ = {}
        for f in fails:
            if ex.verdict_kind(f["verdict"]) == "violation":
                reps_.setdefault((f["class"], ex.signature(f["verdict"]).split(" order=")[0]), f)
        for key, f in reps_.items():
            m, mv, n = ex.shrink(pool, ex.parse(f["schedule"]), f["verdict"], slog, tjk=f.get("tjk"))
            mt = ex.render(m)
            conf = pool.run_all([("min-a", mt), ("min-b", mt)])
            shrunk[key] = {"from_seed": f["seed"], "minimal": mt, "verdict": mv, "tries": n,
                           "min_class": ex.finding_class(m), "fp_a": conf["min-a"][1]["fp"],
                           "fp_b": conf["min-b"][1]["fp"], "min_verdict": conf["min-a"][1]["verdict"]}
        slog.close()
    pool.close()

    summary = {"strategy": "portfolio", "image": a.image, "rep": a.rep, "budget": a.budget, "runs": used,
               "pulls": npull, "workers": a.workers, "batch": batch, "variants": a.variants,
               "explore_wall_s": round(t_explore, 1), "runs_per_hour": round(used / max(t_explore, 1) * 3600, 1),
               "total_wall_s": round(time.time() - t0, 1), "failures": len(fails),
               "reproduced": sum(1 for r in fails if r["reproduced"]),
               "features": len(seen), "signatures": sum(1 for f in seen if f[0] == "sig")}
    arm_rows = [{"arm": x.name, "pulls": x.pulls, "runs": x.runs, "success_runs": x.s, "new_features": x.new,
                 "failing_runs": x.fails, "posterior_mean": round((1 + x.s) / (2 + x.s + x.f), 3),
                 "exhausted": x.exhausted} for x in arms]
    firsts = sorted(((n, fmt_feat(f)) for f, n in seen.items()))
    json.dump({"summary": summary, "arms": arm_rows, "timeline": timeline, "failures": fails,
               "first_seen": firsts, "shrunk": [{"class": k[0], "signature": k[1], **v} for k, v in shrunk.items()]},
              open(os.path.join(a.out, "report.json"), "w"), indent=1)
    write_report(a.out, summary, arm_rows, timeline, runs, firsts, shrunk)
    table = ex.property_table(a.out, [r["tag"] for r in runs])
    if table is not None:
        open(os.path.join(a.out, "properties.txt"), "w").write(table)
    print(json.dumps(summary))


def write_report(out, summary, arm_rows, timeline, runs, firsts, shrunk):
    L = ["# Portfolio explorer report", "", "| | |", "|---|---|"]
    L += ["| %s | %s |" % kv for kv in summary.items()]
    L += ["", "## Arms", "", "| arm | pulls | runs | runs with a new feature | new features | failing runs | posterior mean |",
          "|---|---|---|---|---|---|---|"]
    for r in arm_rows:
        L.append("| %s%s | %d | %d | %d | %d | %d | %.3f |" % (r["arm"], " (used up)" if r["exhausted"] else "", r["pulls"],
                                                             r["runs"], r["success_runs"], r["new_features"],
                                                             r["failing_runs"], r["posterior_mean"]))
    # allocation over time: runs per arm in each tenth of the budget
    names = [r["arm"] for r in arm_rows]
    step = max(1, summary["runs"] // 6)
    L += ["", "## Allocation over time (runs per arm, by run number)", "",
          "| runs | " + " | ".join(names) + " | new features |", "|---" * (len(names) + 2) + "|"]
    for lo in range(0, summary["runs"], step):
        chunk = [r for r in runs if lo < r["seed"] <= lo + step]
        L.append("| %d-%d | %s | %d |" % (lo + 1, min(lo + step, summary["runs"]),
                                          " | ".join(str(sum(1 for r in chunk if r["arm"] == n)) for n in names),
                                          sum(len(r["new_features"]) for r in chunk)))
    L += ["", "## First time each feature was seen", "", "| run | arm | feature |", "|---|---|---|"]
    arm_of = {r["seed"]: r["arm"] for r in runs}
    for n, f in firsts:
        L.append("| %d | %s | `%s` |" % (n, arm_of.get(n, "?"), f[:110]))
    fails = [r for r in runs if ex.verdict_kind(r["verdict"]) != "ok"]
    L += ["", "## Failures", "", "| run | arm | class | verdict | reproduced |", "|---|---|---|---|---|"]
    for r in fails:
        L.append("| %d | %s | %s | `%s` | %s |" % (r["seed"], r["arm"], r["class"], r["verdict"][:90],
                                                  "yes" if r.get("reproduced") else "NO"))
    if shrunk:
        L += ["", "## Minimal reproducers", ""]
        for (c, sig), v in shrunk.items():
            L += ["- %s `%s`: `%s` (fp %s / %s)" % (v["min_class"], sig, v["minimal"], v["fp_a"], v["fp_b"])]
    L += ["", "By hand: `~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz "
          "~/hydo/build/%s <outdir> \"<schedule>\"`" % summary["image"], ""]
    open(os.path.join(out, "report.md"), "w").write("\n".join(L) + "\n")


if __name__ == "__main__":
    main()
