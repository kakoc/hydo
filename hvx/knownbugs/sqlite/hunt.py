#!/usr/bin/env python3
"""hunt.py -- blind exploration of a no-service manifest image (the SQLite WAL-reset target).

explore.py's generators place kills/cuts/disk faults by the stack's service names and know only
the M/R knobs; this target has no service, needs no fault, and its only levers are the
interleaving (seeded tick jitter, hvx M43) and the workload's own seed (the manifest's S knob).
So this driver reuses explore.py's worker pool and tick-jitter formulas, and generates:

    arm "timing":   disk=none dseed=0 tj=<seed> tjpm=<pm> tjmax=<max> | run          (S fixed 0)
    arm "workload": disk=none dseed=0 | run S=<seed>                                  (no jitter)
    arm "both":     disk=none dseed=0 tj=... | run S=<seed>
    arm "pp":       disk=none dseed=0 tj=... pp=<16384|4096> | run S=<seed>   (M49: shorter period)
    arm "xt":       disk=none dseed=0 xt=<seed> xtr=<8|32> xtpm=500 | run S=<seed>  (M49: extra ticks)

Run i uses arm i % 3 and a pure function of (i, --salt), so a run is reproducible from its
schedule string alone. Nothing is aimed at the bug: the pm/max ranges are the generic ones.
Every failure is re-run (fingerprints must match), then shrunk: drop the jitter; drop S;
narrow the delayed-tick window (tjlo/tjhi, same seed); once <= 32 delays remain, turn it into
an explicit list and remove delays one at a time. The shrunk schedule is then run on --fixed.

    python3 hunt.py --image explore-ob-kb-sqlite-3510200-disk.img \\
        --fixed explore-ob-kb-sqlite-3510300-disk.img --runs 300 --workers 1 --out DIR
"""
import argparse, glob, json, os, re, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "..", "explore"))
import explore  # noqa: E402  (read-only use: Pool, SplitMix, tj_delays, mix64)

PMS = [5, 20, 50, 100, 200, 500, 1000]          # delayed ticks per 1000
MAXS = [2000, 50000, 500000]                    # max delay in guest branches (< the 2^20 period)


ARMS = ["timing", "workload", "both"]
PPS = [1 << 14, 1 << 12]                        # M49 arm "pp": PMU tick period during the workload
XTRS = [8, 32]                                  # M49 arm "xt": extra-tick slots per period


def gen(i, salt):
    arm = ARMS[i % len(ARMS)]
    r = explore.SplitMix((i * 0x9E3779B97F4A7C15 ^ salt) & explore.M64)
    s = {"arm": arm, "S": 0, "tj": None}
    if arm in ("workload", "both"):
        s["S"] = r.next()
    if arm in ("timing", "both"):
        s["tj"] = ("seed", r.next(), r.pick(PMS), r.pick(MAXS), 0, 0)
    if arm in ("pp", "xt"):
        s["S"] = r.next()
    if arm == "pp":
        s["pp"] = r.pick(PPS)
        s["tj"] = ("seed", r.next(), r.pick(PMS), min(r.pick(MAXS), s["pp"] - 1), 0, 0)
    if arm == "xt":
        s["xt"] = (r.next(), r.pick(XTRS), 500)
    return s


def render(s):
    tj = explore.render_tj(s["tj"]) if s.get("tj") else ""
    tj += explore.render_preempt({k: v for k, v in s.items() if k in ("pp", "xt") and v})
    g = "run" + (" S=%x" % s["S"] if s.get("S") else "")
    return "disk=none dseed=0%s | %s" % (tj, g)


def stats(outdir, tag):
    """The workload's interleaving counters from the run's console (walrace prints them)."""
    out = {}
    for p in glob.glob(os.path.join(outdir, "consoles", "*%s*" % tag)):
        try:
            t = open(p, errors="replace").read()
        except OSError:
            continue
        m = re.search(r"walrace: done steps=\S+ checkpoints=(\d+) busy=(\d+) wal_empty_after=(\d+) "
                      r"csw_voluntary=(\d+) csw_involuntary=(\d+)", t)
        if m:
            out = dict(zip(["ckpt", "ckpt_busy", "wal_empty", "csw_vol", "csw_invol"], map(int, m.groups())))
        m = re.search(r"walrace: seed=\S+ (writers=.*) steps", t)
        if m:
            out["shape"] = m.group(1)
    return out


def failed(v):
    return "VIOLATION" in v or "ERROR" in v


def shrink(pool, s, verdict, tjk, log, budget=60):
    tries = 0

    def attempt(c, why):
        nonlocal tries
        tries += 1
        tag = "shrink-%d" % tries
        res = pool.run_all([(tag, render(c))])[tag][1]
        log.write(json.dumps({"try": why, "schedule": render(c), "verdict": res["verdict"], "fp": res["fp"]}) + "\n")
        log.flush()
        return failed(res["verdict"]), res

    cur = dict(s)
    for k in ("xt", "pp"):                      # M49 levers first: is the finer preemption needed?
        if cur.get(k):
            c = dict(cur, **{k: None})
            ok, _ = attempt(c, "drop " + k)
            if ok:
                cur = c
    if cur.get("tj"):
        c = dict(cur, tj=None)
        ok, _ = attempt(c, "drop jitter")
        if ok:
            cur = c
    if cur.get("S"):
        c = dict(cur, S=0)
        ok, _ = attempt(c, "drop workload seed")
        if ok:
            cur = c
    if cur.get("tj") and cur["tj"][0] == "seed" and tjk:
        lo, hi = tjk[0], tjk[1] + 1
        while tries < budget and len(explore.tj_delays(cur["tj"][:4] + (lo, hi), lo, hi - 1)) > explore.TJ_EXPLICIT_MAX:
            mid = (lo + hi) // 2
            moved = False
            for a, b in ((lo, mid), (mid, hi)):
                c = dict(cur, tj=cur["tj"][:4] + (a, b))
                ok, _ = attempt(c, "window %d-%d" % (a, b))
                if ok:
                    cur, lo, hi, moved = c, a, b, True
                    break
            if not moved:
                break
        kd = explore.tj_delays(cur["tj"][:4] + (lo, hi), lo, hi - 1)
        if len(kd) <= explore.TJ_EXPLICIT_MAX:
            c = dict(cur, tj=("x", kd))
            ok, _ = attempt(c, "explicit list (%d delays)" % len(kd))
            if ok:
                cur = c
    if cur.get("tj") and cur["tj"][0] == "x":
        i = 0
        while i < len(cur["tj"][1]) and tries < budget:
            kd = cur["tj"][1][:i] + cur["tj"][1][i + 1:]
            c = dict(cur, tj=("x", kd) if kd else None)
            ok, _ = attempt(c, "remove delay %s" % (cur["tj"][1][i],))
            if ok:
                cur = c
            else:
                i += 1
    return cur, tries


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", required=True)
    ap.add_argument("--fixed", required=True)
    ap.add_argument("--runs", type=int, default=300)
    ap.add_argument("--first", type=int, default=0)
    ap.add_argument("--salt", type=lambda x: int(x, 0), default=0x5A17E)
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--out", required=True)
    ap.add_argument("--arms", default="timing,workload,both", help="arms to cycle through")
    ap.add_argument("--dev", help="hvx device, e.g. /dev/hvxpre for a side-by-side module")
    ap.add_argument("--stop-after", type=int, default=3, help="stop after this many distinct failures")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    ARMS[:] = a.arms.split(",")
    explore.IMAGE = a.image
    if a.dev:
        explore.EXTRA_ENV.append("HVX_DEV=" + a.dev)
    t0 = time.time()
    pool = explore.Pool(a.workers, a.out)
    runs = open(os.path.join(a.out, "runs.jsonl"), "a")
    shr = open(os.path.join(a.out, "shrink.jsonl"), "a")
    found = []
    batch = max(1, a.workers)
    i = a.first
    while i < a.first + a.runs and len(found) < a.stop_after:
        items, specs = [], {}
        for j in range(i, min(i + batch, a.first + a.runs)):
            s = gen(j, a.salt)
            tag = "r%05d" % j
            items.append((tag, render(s)))
            specs[tag] = (j, s)
        res = pool.run_all(items)
        for tag, text in items:
            j, s = specs[tag]
            r = res[tag][1]
            rec = {"run": j, "arm": s["arm"], "schedule": text, "fp": r["fp"], "secs": r["secs"],
                   "verdict": r["verdict"], "tjk": r.get("tjk"), "tjn": r.get("tjn"), "xtn": r.get("xtn"),
                   "wall": round(time.time() - t0, 1)}
            rec.update(stats(a.out, tag))
            runs.write(json.dumps(rec) + "\n")
            runs.flush()
            print("%5d %-8s %-60s %s" % (j, s["arm"], text[:60], r["verdict"][:90]), flush=True)
            if failed(r["verdict"]):
                again = pool.run_all([(tag + "-rerun", text)])[tag + "-rerun"][1]
                same = again["fp"] == r["fp"] and again["verdict"] == r["verdict"]
                cur, tries = shrink(pool, s, r["verdict"], r.get("tjk"), shr)
                txt = render(cur)
                mine = pool.run_all([(tag + "-min", txt)])[tag + "-min"][1]
                found.append({"run": j, "schedule": text, "verdict": r["verdict"], "fp": r["fp"],
                              "rerun_same": same, "minimal": txt, "min_verdict": mine["verdict"],
                              "min_fp": mine["fp"], "shrink_tries": tries, "wall": round(time.time() - t0, 1)})
                print("FOUND run %d rerun_same=%s minimal: %s -> %s" % (j, same, txt, mine["verdict"][:100]), flush=True)
        i += batch
    pool.close()
    # the minimal schedules on the fixed version, each twice
    if found:
        explore.IMAGE = a.fixed
        fpool = explore.Pool(1, os.path.join(a.out, "fixed"))
        for k, f in enumerate(found):
            r1 = fpool.run_all([("fx%d-a" % k, f["minimal"])])["fx%d-a" % k][1]
            r2 = fpool.run_all([("fx%d-b" % k, f["minimal"])])["fx%d-b" % k][1]
            f["fixed"] = {"verdict": r1["verdict"], "fp": r1["fp"], "same": r1["fp"] == r2["fp"]}
        fpool.close()
    summary = {"image": a.image, "runs_done": i - a.first, "found": found, "wall_s": round(time.time() - t0, 1)}
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
