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
# Milestone 59: harder defects, patched into hvx/bench/base/main-m59-control.go (the current
# order service with the M50 fixes, the BUGGIFY wrappers and a fix for the M58 flush-cut bug),
# built by build-bench-m59.sh. Their control is that file unpatched. d9's signature is the
# crash oracle below (the guest's "front door log: panic" line). Images: build-bench-m59.sh with
# BENCH_SUFFIX=-pin (built from the pinned toolchain, hvx/BUILD-PINS.md); HYDO_M59_SUFFIX=""
# selects the first build, which used the stand-in hvx_seedrng (superseded).
M59 = {
    "d8-no-order-lock":       r"present \d+ times|kafka_exactly_once",
    "d9-redis-unlocked":      r"front door crashed",
    "d10-outcome-in-memory":  r"present \d+ times|kafka_exactly_once",
    "d11-ack-unflushed-flag": r"not marked published|acked_marked_published",
    "d12-shared-key-buffer":  r"kafka order=\d+ (expected|present)|in_kafka|kafka_exactly_once",
}
M59_SUFFIX = os.environ.get("HYDO_M59_SUFFIX", "-pin")   # images rebuilt with the pinned tools
for _n, _sig in M59.items():
    DEFECTS[_n] = ("explore-bench-m59-%s%s-disk.img" % (_n, M59_SUFFIX), _sig)
M59_CONTROL = "explore-bench-m59-control%s-disk.img" % M59_SUFFIX
CONTROL = "explore-nf-disk.img"
CRASH_RE = re.compile(r"hydo-explore: front door log: ((?:panic|fatal error).*)")


def crash_verdict(outdir, tag, verdict, defect):
    """M59 crash oracle (the property "no process crashes", for the order
    service only): stack-explore.sh restarts a dead front door before the retry pass and prints
    its first panic/fatal-error lines. A SIGKILL (X=, K=) leaves no such line. Applied to the
    M59 defects and their control only, so earlier results keep their meaning."""
    if defect not in M59 or ex.verdict_kind(verdict) != "ok":
        return verdict
    for suffix in ("", ".recovery"):
        path = os.path.join(outdir, "consoles", "%s%s.log" % (tag, suffix))
        if os.path.exists(path):
            for line in open(path, errors="replace"):
                m = CRASH_RE.search(line)
                if m:
                    return "HYDO-ORACLE: VIOLATION front door crashed: " + m.group(1).strip()[:160]
    return verdict
BG_IMAGES = False   # --bg-images (M56): the images rebuilt with BUGGIFY fault points (build-bench-bg.sh)


def image_of(defect):
    if defect in M59:
        return DEFECTS[defect][0]
    return "explore-bench-bg-%s-disk.img" % defect if BG_IMAGES else DEFECTS[defect][0]


def control_of(defect):
    """The unpatched image in the same guest: d0 runs the pre-M46 guest, so its control is the
    fixed front door in that guest (explore-fix-disk.img). With --bg-images every defect,
    d0 included, is built in the current guest, so its control is the BUGGIFY control image."""
    if defect in M59:
        return M59_CONTROL
    if BG_IMAGES:
        return "explore-bench-bg-control-disk.img"
    return "explore-fix-disk.img" if defect.startswith("d0-") else CONTROL


class Control:
    """Attribution: a candidate detection counts only if the SAME schedule passes on the
    control image. (The fixed front door has a bug of its own -- see README -- so a failure in
    class durable-defaults is not automatically the planted defect's.)"""
    def __init__(self, a, defect):
        self.image, self.cache, self.defect = control_of(defect), {}, defect
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
            r = dict(self.pool.run_all([(tag, text)])[tag][1])
            r["verdict"] = crash_verdict(self.dout, tag, r["verdict"], self.defect)
            self.cache[text] = r
        return ex.verdict_kind(self.cache[text]["verdict"]) == "ok", self.cache[text]

    def close(self):
        self.pool.close()


def detects(rec):
    return rec["class"] == "durable-defaults" and ex.verdict_kind(rec["verdict"]) == "violation"


def make_rec(outdir, sd, text, r, defect=None):
    s = ex.parse(text)
    rec = {"seed": sd, "schedule": text, "class": ex.finding_class(s), **r}
    rec["verdict"] = crash_verdict(outdir, "seed-%d" % sd, rec["verdict"], defect)
    rec["assert_failures"] = ex.assert_failures(outdir, "seed-%d" % sd)
    if rec["assert_failures"] and ex.verdict_kind(r["verdict"]) == "ok":
        rec["verdict"] = "VIOLATION assertion " + ",".join(rec["assert_failures"])
    return rec


def schedules(strategy, seeds, new_faults, buggify=False, concurrency=False, turns=False):
    gen = ex.GENERATORS[strategy]
    ex.NEW_FAULTS, ex.BUGGIFY, ex.CONCURRENCY, ex.TURNS = new_faults, buggify, concurrency, turns
    # the same composition order as explore.py: turns(buggify(concurrency(new_faults(gen)))).
    # add_turns returns the schedule unchanged without --turns, so the strings are unchanged.
    if strategy in ("swarm-pos", "process"):      # M56 generators draw every fault kind themselves
        return [(sd, ex.render(ex.add_turns(ex.add_buggify(ex.add_concurrency(gen(sd), sd), sd), sd))) for sd in seeds]
    return [(sd, ex.render(ex.add_turns(ex.add_buggify(ex.add_concurrency(ex.add_new_faults(gen(sd), sd, strategy == "swarm"), sd), sd), sd)))
            for sd in seeds]


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
        items = schedules(a.strategy, seeds, a.new_faults, a.buggify, a.concurrency, a.turns)
        done, nxt, outstanding, found, base = {}, 0, 0, None, []
        t0 = time.time()
        while True:
            while nxt < len(items) and outstanding < (a.window or 2 * a.workers) and found is None:
                sd, text = items[nxt]
                pool.jobs.put(("seed-%d" % sd, text)); nxt += 1; outstanding += 1
            if outstanding == 0:
                break
            tag, text, r = pool.results.get(); outstanding -= 1
            sd = int(tag.split("-")[1])
            done[sd] = make_rec(dout, sd, text, r, defect)
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
        if a.vcpus != 1:
            res["vcpus"] = a.vcpus
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


def rescore(a):
    """Re-score finished runs (runs-rep*.jsonl of random/swarm, rep*/runs.jsonl of portfolio) under
    ex.KNOB_RULE. Under the M56 rule only MORE runs are durable-defaults, so a new first detection can
    only be earlier than the old one: every run up to the old detection (or the whole budget) was
    executed and is re-classified here. A new candidate is attributed only if it passes on the control
    image (run on a 1-worker pool, as run.py does). Writes <out>/results.jsonl in the run.py format."""
    os.makedirs(a.out, exist_ok=True)
    out = open(os.path.join(a.out, "results.jsonl"), "w")
    for d in a.rescore:
        label = os.path.basename(d.rstrip("/"))
        old = {(r["defect"], r["rep"]): r for r in map(json.loads, open(os.path.join(d, "results.jsonl")))}
        for (defect, rep), r0 in sorted(old.items()):
            image, sig = DEFECTS[defect]
            p = os.path.join(d, defect, "runs-rep%d.jsonl" % rep)
            recs = [json.loads(l) for l in open(p)] if os.path.exists(p) else []
            if not recs:            # portfolio
                q = os.path.join(d, defect, "rep%d" % rep, "runs.jsonl")
                recs = sorted((json.loads(l) for l in open(q)), key=lambda x: x["seed"]) if os.path.exists(q) else []
            res = dict(r0, strategy=r0["strategy"] + "/" + a.knob_rule)
            res.update({"detected_at": None, "rescored_from": r0.get("detected_at")})
            for i, rec in enumerate(recs):
                rec["class"] = ex.finding_class(ex.parse(rec["schedule"]))
                if rec["class"] != "durable-defaults" or ex.verdict_kind(rec["verdict"]) != "violation":
                    continue
                if r0.get("detected_at") and rec["schedule"] == r0.get("schedule"):
                    ok = True                                   # already attributed in M54
                else:
                    ctl = Control(a, defect)
                    ok, cr = ctl.passes(rec["schedule"])
                    ctl.close()
                    if not ok:
                        res["base_bug_hits"] = res.get("base_bug_hits", []) + [
                            {"run": i + 1, "schedule": rec["schedule"], "control_verdict": cr["verdict"]}]
                if ok:
                    res.update({"detected_at": i + 1, "schedule": rec["schedule"], "verdict": rec["verdict"],
                                "fp": rec.get("fp"), "signature_match": bool(re.search(sig, rec["verdict"]))})
                    break
            print(json.dumps(res), flush=True)
            out.write(json.dumps(res) + "\n")
            out.flush()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--strategy", choices=["random", "swarm", "swarm-pos", "process", "portfolio"])
    ap.add_argument("--knob-rule", choices=["cut", "any"], default="cut",
                    help="cut (M56 default): a relaxed knob is unsafe only with a power cut or lying disk; "
                         "any: the M54 rule")
    ap.add_argument("--rescore", nargs="*", metavar="DIR",
                    help="re-score finished strategy dirs under --knob-rule (classes only; schedules unchanged); "
                         "candidates new to the durable class are checked on the control image")
    ap.add_argument("--new-faults", action="store_true")
    ap.add_argument("--buggify", action="store_true", help="M56: every schedule carries a BUGGIFY seed (B=)")
    ap.add_argument("--bg-images", action="store_true", help="M56: use the explore-bench-bg-* images")
    ap.add_argument("--concurrency", action="store_true",
                    help="M59: explore.py --concurrency (Q= token on 60%% of schedules, from its own stream)")
    ap.add_argument("--vcpus", type=int, default=1,
                    help="SMP stage 3: the engine runs every VM (defect and control) with N vCPUs "
                         "(explore.py --vcpus; the engine gets --vcpus only when N > 1)")
    ap.add_argument("--turns", action="store_true",
                    help="with --vcpus N > 1: explore.py --turns (q= [qs=] on half of the schedules, own stream)")
    ap.add_argument("--window", type=int, default=0,
                    help="schedules in flight (default 2*workers); 1 = no run overlaps a control check")
    ap.add_argument("--defects", default="all")
    ap.add_argument("--reps", default="1-3")
    ap.add_argument("--budget", type=int, default=60)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--out")
    ap.add_argument("--table", nargs="*")
    a = ap.parse_args()
    ex.KNOB_RULE = a.knob_rule
    if not 1 <= a.vcpus <= 8:
        raise SystemExit("run.py: --vcpus must be 1..8")
    if a.turns and a.vcpus < 2:
        raise SystemExit("run.py: --turns needs --vcpus N > 1")
    if a.vcpus > 1 and a.strategy == "portfolio":
        raise SystemExit("run.py: --vcpus is not passed to portfolio.py")
    ex.VCPUS = a.vcpus
    if a.table is not None:
        table(a.table)
        return
    if a.rescore is not None:
        rescore(a)
        return
    lo, hi = [int(x) for x in a.reps.split("-")] if "-" in a.reps else (int(a.reps), int(a.reps))
    a.reps = list(range(lo, hi + 1))
    global BG_IMAGES
    BG_IMAGES = a.bg_images
    a.label = (a.strategy + ("+nf" if a.new_faults and a.strategy in ("random", "swarm") else "")
               + ("+bg" if a.buggify else "") + ("+cc" if a.concurrency else "")
               + ("+v%d" % a.vcpus if a.vcpus != 1 else "") + ("+turns" if a.turns else ""))
    names = ([n for n in DEFECTS if n not in M59] if a.defects == "all" else
             list(M59) if a.defects == "m59" else a.defects.split(","))
    os.makedirs(a.out, exist_ok=True)
    out = open(os.path.join(a.out, "results.jsonl"), "a")
    for name in names:
        image, sig = image_of(name), DEFECTS[name][1]
        rs = run_portfolio(a, name, image, sig) if a.strategy == "portfolio" else run_seeded(a, name, image, sig)
        for r in rs:
            out.write(json.dumps(r) + "\n")
        out.flush()


if __name__ == "__main__":
    main()
