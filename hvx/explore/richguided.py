#!/usr/bin/env python3
"""richguided.py -- exploration guided by a RICHER signal (Milestone 44; hvx/richcov/DESIGN.md).

Milestone 41's guidance used one feature kind, the front door's control-flow edges, and did not
beat random schedules: those edges saturate within a few runs and the bug that mattered is about
the order of events, not about reaching new code. This loop uses four feature kinds per run:

  E  whole-system edges: (program, from, to, AFL hit-count bucket) for the front door, Redis
     and MySQL, from one Intel PT trace with two IP ranges, decoded by hvx/richcov/ptmulti
     (needs a richcov-old / richcov-fix image and `norandmaps` on the guest command line);
  P  property firsts: a sometimes/reachable assertion satisfied (ok=true) for the first time;
  D  detail buckets: (assertion, outcome, bucketed low-cardinality details), emitted by the
     richcov images' distinct-details SDK mode;
  S  per-order state tuples from the oracle: "HYDO-STATE client=.. mysql=.. kafka=.. redis=..".

A schedule enters the corpus iff its run produced a feature never seen before. The next entry
to mutate is the one with the highest score  sum_f w(kind f) / (1 + later hits of f), divided
by (1 + times the entry was mutated); w(S) = w(P) = 4, w(D) = 2, w(E) = 1. Every 4th schedule
is a fresh random one (exploration floor). Mutations are guided.py's, plus SPLICE (the kill and
network fault of one entry with the power cut / disk class of another).

  python3 richguided.py --image richcov-old-disk.img --out /var/tmp/m44-rich/old-rich-1 \
      --mode rich --budget 60 --rng0 1 --stop-on durable-defaults

Modes: rich (all four kinds), richk (all four kinds, score per kind = weight x mean rarity,
edges of a truncated trace ignored -- added after the first results, see below), edges (E only,
same scheduler), random (fresh seeds, same budget;
every run is still traced, so feature growth is comparable). Everything stays deterministic: a
schedule renders to one run with one fingerprint; failures are re-run to check that.
"""
import argparse, json, os, random, re, subprocess, sys, time
import explore as E
import guided as G

HERE = os.path.dirname(os.path.abspath(__file__))
PROGS = ["hydostack", "redis", "mysqld"]
WEIGHT = {"E": 1.0, "P": 4.0, "D": 2.0, "S": 4.0}
FLOOR = 4               # every FLOOR-th schedule is a fresh random one
RICH_KEYS = ("store", "what", "attempt", "count", "kafka_count")   # = richdistinct.go
MYSQLD_BIAS = 0x555555554000


def exec_range(elf, bias=0):
    """[lo, hi) of the ELF's executable PT_LOAD segment(s), bias added."""
    out = subprocess.run(["readelf", "-lW", elf], stdout=subprocess.PIPE, text=True, check=True).stdout
    lo, hi = None, None
    for line in out.splitlines():
        f = line.split()
        if f and f[0] == "LOAD" and "E" in f[-2]:
            va, memsz = int(f[2], 16), int(f[5], 16)
            lo = va if lo is None else min(lo, va)
            hi = va + memsz if hi is None else max(hi, va + memsz)
    return lo + bias, hi + bias


def coverage_env(image):
    """Worker environment for whole-system tracing of a richcov image: two IP ranges (Redis up
    to the end of the front door; mysqld at the no-ASLR PIE base) and the ptmulti decoder."""
    syms = os.path.join(E.BUILD, image.replace("-disk.img", "-syms"))
    hs, rs, ms = (os.path.join(syms, n) for n in ("hydostack", "redis-server", "mysqld"))
    r_lo, _ = exec_range(rs)
    _, h_hi = exec_range(hs)
    m_lo, m_hi = exec_range(ms, MYSQLD_BIAS)
    ptm = os.path.join(E.HVX, "richcov", "ptmulti")
    if not os.path.exists(ptm):
        subprocess.run(["cc", "-O2", "-o", ptm, ptm + ".c", "-lipt"], check=True)
    return ["HVX_EXPLORE_CMDLINE_EXTRA=norandmaps",
            "HVX_EXPLORE_PT_RANGES=%x-%x,%x-%x" % (r_lo, h_hi, m_lo, m_hi),
            "HVX_EXPLORE_PT_MIB=256",
            "HVX_EXPLORE_PTMULTI=%s hydostack=%s redis=%s mysqld=%s@%#x" % (ptm, hs, rs, ms, MYSQLD_BIAS)]


def bucket(c):
    return (0 if c == 1 else 1 if c == 2 else 2 if c == 3 else 3 if c < 8 else
            4 if c < 16 else 5 if c < 32 else 6 if c < 128 else 7)


def det_bucket(v):
    if isinstance(v, bool) or not isinstance(v, int):
        return str(v)
    b = 1
    while b < v:
        b <<= 1
    return "<=%d" % b


STATE_RE = re.compile(r"HYDO-STATE (.*) n=\d+")


def features(outdir, tag):
    """All features of one run, as a set of tuples whose first element is the kind."""
    fs = set()
    cov = os.path.join(outdir, "cov", tag)
    for p in PROGS:
        path = os.path.join(cov, p + ".edges")
        if os.path.exists(path):
            for line in open(path):
                a, b, c = line.split()
                fs.add(("E", p, a, b, bucket(int(c))))
    for suffix in ("", ".recovery"):
        path = os.path.join(outdir, "consoles", "%s%s.log" % (tag, suffix))
        if not os.path.exists(path):
            continue
        for line in open(path, errors="replace"):
            m = STATE_RE.search(line)
            if m:
                fs.add(("S", m.group(1).strip()))
                continue
            m = E.ASSERT_RE.search(line)
            if not m:
                continue
            try:
                rec = json.loads(m.group(1))
            except ValueError:
                continue
            t, ok, pid = rec.get("t"), rec.get("ok"), rec.get("id", "?")
            if t in ("sometimes", "reachable") and ok:
                fs.add(("P", pid))
            d = rec.get("details") or {}
            key = tuple(sorted((k, det_bucket(d[k])) for k in RICH_KEYS if k in d))
            fs.add(("D", pid, bool(ok), key))
    return fs


def cov_stats(outdir, tag):
    """trace bytes, decode seconds and per-program edge counts of one run (from the stats files)."""
    st = {}
    p = os.path.join(outdir, "cov", tag + ".stats")
    if os.path.exists(p):
        for kv in open(p).read().split():
            k, v = kv.split("=")
            st[k] = float(v) if "." in v else int(v)
    for prog in PROGS:
        e = os.path.join(outdir, "cov", tag, prog + ".edges")
        st["edges_" + prog] = sum(1 for _ in open(e)) if os.path.exists(e) else 0
    return st


def splice(a, b, rng):
    """The kill and network fault of a, the cut point, disk class and durability knobs of b."""
    t = {k: v for k, v in b.items() if k not in ("K", "N")}
    for k in ("K", "N"):
        if k in a:
            t[k] = a[k]
    if "C" not in t and t["disk"] == "none" and rng.random() < 0.5:
        t["C"] = rng.randint(1, E.NORDERS)
        t["disk"] = "cut"
    return t


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="richcov-old-disk.img")
    ap.add_argument("--out", required=True)
    ap.add_argument("--budget", type=int, default=60)
    ap.add_argument("--workers", type=int, default=1)
    ap.add_argument("--mode", choices=["rich", "richk", "edges", "random"], default="rich")
    ap.add_argument("--rng0", type=int, default=1)
    ap.add_argument("--stop-on", default=None, help="stop when a failure of this class appears")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    E.IMAGE = a.image
    E.EXTRA_ENV[:] = coverage_env(a.image)
    kinds = {"rich": "EPDS", "richk": "EPDS", "edges": "E", "random": "EPDS"}[a.mode]
    rng = random.Random(a.rng0)
    pool = E.Pool(a.workers, a.out)

    corpus = []                  # {"s": schedule dict, "feats": [new features it brought], "energy": n}
    hits = {}                    # feature -> number of runs that produced it
    runs = open(os.path.join(a.out, "runs.jsonl"), "w")
    growth = []
    fails, first_class = [], {}
    t0, n = time.time(), 0

    def score(c):
        if a.mode == "richk":     # per kind: weight x MEAN rarity, so thousands of new edges
            tot = 0.0             # cannot drown one new state tuple (see the M44 notes)
            for k in "EPDS":
                fk = [f for f in c["feats"] if f[0] == k]
                if fk:
                    tot += WEIGHT[k] * sum(1.0 / hits[f] for f in fk) / len(fk)
            return tot / (1 + c["energy"])
        return sum(WEIGHT[f[0]] / hits[f] for f in c["feats"]) / (1 + c["energy"])

    def consider(tag, d, r, fs):
        nonlocal n
        n += 1
        fs = {f for f in fs if f[0] in "EPDS"}
        st = cov_stats(a.out, tag)
        if st.get("no_trace") or (a.mode == "richk" and st.get("truncated")):
            fs = {f for f in fs if f[0] != "E"}      # no trace, or (richk) a truncated one:
                                                     # its edge set depends on where it stopped
        new = [f for f in fs if f not in hits]
        for f in fs:
            hits[f] = hits.get(f, 0) + 1
        newk = {k: sum(1 for f in new if f[0] == k) for k in "EPDS"}
        tot = {k: sum(1 for f in hits if f[0] == k) for k in "EPDS"}
        totp = {p: sum(1 for f in hits if f[0] == "E" and f[1] == p) for p in PROGS}
        growth.append({"run": n, **{"tot_" + k: v for k, v in tot.items()}, **{"E_" + p: v for p, v in totp.items()}})
        cls = E.finding_class(d)
        vk = E.verdict_kind(r["verdict"])
        af = E.assert_failures(a.out, tag)
        if af and vk == "ok":
            r["verdict"] = "VIOLATION assertion " + ",".join(af)
            vk = "violation"
        guiding = [f for f in new if f[0] in kinds]
        rec = {"run": n, "tag": tag, "schedule": E.render(d), "class": cls, "new": newk,
               "corpus": len(corpus), "tot": tot, "E_by_prog": totp, "cov": cov_stats(a.out, tag),
               **r, "assert_failures": af}
        runs.write(json.dumps(rec) + "\n"); runs.flush()
        if vk == "violation":
            fails.append(rec)
            first_class.setdefault(cls, n)
        if a.mode != "random" and guiding:
            corpus.append({"s": d, "feats": guiding, "energy": 0})

    seen = set()                 # schedules already run: a mutation that repeats one is redrawn

    def next_schedule(i):
        if a.mode == "random" or not corpus or (n + i) % FLOOR == FLOOR - 1:
            return "r", E.generate(rng.getrandbits(64))
        c = max(corpus, key=score)
        c["energy"] += 1
        for _ in range(20):
            if len(corpus) > 1 and rng.random() < 0.2:
                other = rng.choice([x for x in corpus if x is not c])
                kind, d = "x", splice(c["s"], other["s"], rng)
            else:
                kind, d = "m", G.mutate(c["s"], rng)
            if E.render(d) not in seen:
                return kind, d
        return "r", E.generate(rng.getrandbits(64))

    while n < a.budget:
        batch = []
        for i in range(min(a.workers, a.budget - n)):
            kind, d = next_schedule(i)
            batch.append(("%s-%d-%d" % (kind, n, i), d))
        seen.update(E.render(d) for _, d in batch)
        res = pool.run_all([(t, E.render(d)) for t, d in batch])
        # a run whose PT buffer could not be allocated has no trace (hvx_explore writes
        # no_trace=1): never count it as coverage; compact memory and re-run it (<= 4 times)
        for i, (tag, d) in enumerate(batch):
            for attempt in range(1, 5):
                if not cov_stats(a.out, tag).get("no_trace"):
                    break
                subprocess.run(["sudo", "sh", "-c", "echo 1 > /proc/sys/vm/compact_memory"])
                tag2 = "%s-rt%d" % (tag, attempt)
                res[tag2] = pool.run_all([(tag2, E.render(d))])[tag2]
                batch[i] = (tag2, d)
                tag = tag2
        for tag, d in batch:
            consider(tag, d, res[tag][1], features(a.out, tag))
        if a.stop_on and a.stop_on in first_class:
            break
    runs.close()

    # every failure must replay bit-for-bit
    rep = pool.run_all([("repro-%d" % f["run"], f["schedule"]) for f in fails])
    for f in fails:
        f["reproduced"] = rep["repro-%d" % f["run"]][1]["fp"] == f["fp"]
    pool.close()
    summary = {"mode": a.mode, "image": a.image, "rng0": a.rng0, "budget": a.budget, "runs": n,
               "wall_s": round(time.time() - t0, 1), "runs_per_hour": round(n / (time.time() - t0) * 3600, 1),
               "corpus": len(corpus), "features": growth[-1] if growth else {},
               "failures": len(fails), "reproduced": sum(1 for f in fails if f.get("reproduced")),
               "first_failure_run_by_class": first_class,
               "signatures": sorted({(f["class"], E.signature(f["verdict"])) for f in fails})}
    json.dump({"summary": summary, "growth": growth,
               "failures": [{k: f[k] for k in ("run", "schedule", "class", "verdict", "fp", "reproduced")} for f in fails]},
              open(os.path.join(a.out, "report.json"), "w"), indent=1)
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
