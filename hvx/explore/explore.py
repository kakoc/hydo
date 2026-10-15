#!/usr/bin/env python3
"""explore.py -- the automatic fault explorer for the four-service stack (Milestone 38).

Runs on the bare-metal test machine. Generates a fault schedule from every seed (a pure function of
the seed: splitmix64), runs the schedules on a pool of `hvx_explore serve` workers (each
boots + provisions the guest ONCE and then starts every schedule from a snapshot), keeps
every run whose invariant fails, re-runs each failure to confirm it reproduces bit-for-bit,
shrinks it to a minimal failing schedule, and writes a report.

  python3 explore.py --seeds 1-200 --workers 6 --out /var/tmp/explore/run1
  python3 explore.py --schedule "disk=cut dseed=0 | run C=20 M=2" --out ...   # one schedule

Schedule format (see hvx_explore.c / stack-explore.sh):
  disk=<none|ioerr:P|cut|lying:P> dseed=<hex> | run [K=svc@at+down] [N=svc@from-to:netem,args]
                                                   [C=at] [M=0|1|2] [R=always|everysec|no]
Classes of findings:
  durable-defaults  every store configured durably, the disk honest -- the interesting ones
  unsafe-knob       needs a documented-unsafe setting (MySQL trx_commit != 1, Redis
                    appendfsync != always): expected
  lying-disk        the disk acknowledged writes it never stored: expected
  io-errors         the disk returned I/O errors (also on reads of program files)
"""
import argparse, json, os, queue, re, subprocess, sys, threading, time

M64 = (1 << 64) - 1
NORDERS = 30
HERE = os.path.dirname(os.path.abspath(__file__))
HVX = os.path.dirname(HERE)
BUILD = os.path.expanduser("~/hydo/build")
IMAGE = "explore-disk.img"


class SplitMix:
    def __init__(self, seed):
        self.s = seed & M64

    def next(self):
        self.s = (self.s + 0x9E3779B97F4A7C15) & M64
        z = self.s
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & M64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & M64
        return z ^ (z >> 31)

    def below(self, n):
        return self.next() % n

    def rng(self, lo, hi):          # inclusive
        return lo + self.below(hi - lo + 1)

    def pick(self, xs):
        return xs[self.below(len(xs))]


SVCS = ["mysql", "kafka", "redis"]


def generate(seed):
    """seed -> schedule dict. Pure function of the seed."""
    r = SplitMix(seed)
    s = {"disk": "none", "dseed": r.next()}
    if r.below(100) < 40:
        at = r.rng(0, NORDERS - 4)
        s["K"] = (r.pick(SVCS), at, r.rng(1, min(4, NORDERS - at)))
    if r.below(100) < 40:
        a = r.rng(0, NORDERS - 2)
        b = r.rng(a + 1, min(NORDERS, a + 15))
        kinds = []
        for _ in range(r.rng(1, 2)):
            k = r.pick(["delay", "loss", "duplicate", "reorder", "corrupt"])
            if k not in [x[0] for x in kinds]:
                kinds.append((k, {"delay": r.rng(10, 300), "loss": r.rng(5, 60), "duplicate": r.rng(5, 50),
                                  "reorder": r.rng(10, 50), "corrupt": r.rng(1, 10)}[k]))
        s["N"] = (r.pick(SVCS), a, b, kinds)
    x = r.below(100)
    if x < 45:
        s["disk"] = "none"
    elif x < 60:
        s["disk"] = "ioerr:%d" % r.rng(1, 3)
    elif x < 85:
        s["disk"] = "cut"
    else:
        s["disk"] = "lying:%d" % r.rng(2, 12)
    if s["disk"] in ("cut",) or s["disk"].startswith("lying"):
        s["C"] = r.rng(1, NORDERS)
    if r.below(100) < 20:
        k = r.pick([("M", 2), ("M", 0), ("R", "everysec"), ("R", "no")])
        s[k[0]] = k[1]
    return s


def netem_args(kinds):
    out = []
    has_delay = any(k == "delay" for k, _ in kinds)
    for k, v in kinds:
        if k == "delay":
            out += ["delay", "%dms" % v]
        elif k == "reorder":
            if not has_delay:
                out += ["delay", "20ms"]
            out += ["reorder", "%d%%" % v]
        else:
            out += [k, "%d%%" % v]
    return ",".join(out)


def render(s):
    g = ["run"]
    if "K" in s:
        g.append("K=%s@%d+%d" % s["K"])
    if "N" in s:
        svc, a, b, kinds = s["N"]
        g.append("N=%s@%d-%d:%s" % (svc, a, b, netem_args(kinds)))
    if "C" in s:
        g.append("C=%d" % s["C"])
    if "M" in s:
        g.append("M=%d" % s["M"])
    if "R" in s:
        g.append("R=%s" % s["R"])
    return "disk=%s dseed=%x | %s" % (s["disk"], s["dseed"], " ".join(g))


def parse(text):
    host, guest = text.split("|", 1)
    s = {"disk": re.search(r"disk=(\S+)", host).group(1), "dseed": int(re.search(r"dseed=([0-9a-fA-F]+)", host).group(1), 16)}
    for t in guest.split()[1:]:
        k, v = t.split("=", 1)
        if k == "K":
            svc, rest = v.split("@"); at, down = rest.split("+"); s["K"] = (svc, int(at), int(down))
        elif k == "N":
            svc, rest = v.split("@", 1); rng, args = rest.split(":", 1); a, b = rng.split("-")
            toks = args.split(","); kinds = []; i = 0
            while i < len(toks):
                kinds.append((toks[i], int(re.sub(r"\D", "", toks[i + 1])))); i += 2
            s["N"] = (svc, int(a), int(b), kinds)
        elif k == "C":
            s["C"] = int(v)
        elif k == "M":
            s["M"] = int(v)
        elif k == "R":
            s["R"] = v
    return s


def finding_class(s):
    """The reason a failure is expected, most fundamental first: a lying or failing disk
    explains more than a relaxed durability setting does."""
    if s["disk"].startswith("lying"):
        return "lying-disk"
    if s["disk"].startswith("ioerr"):
        return "io-errors"
    if s.get("M", 1) != 1 or s.get("R", "always") != "always":
        return "unsafe-knob"
    return "durable-defaults"


# ---------------------------------------------------------------- assertions (hvx/ASSERTIONS.md)
ASSERT_RE = re.compile(r"HYDO-ASSERT (\{.*\})")


def assert_failures(outdir, tag):
    """Failed property evaluations in one run's console(s): an always / always_or_unreachable
    with ok=false, or any unreachable that was emitted. Returns sorted assertion ids."""
    bad = set()
    for suffix in ("", ".recovery"):
        path = os.path.join(outdir, "consoles", "%s%s.log" % (tag, suffix))
        if not os.path.exists(path):
            continue
        for line in open(path, errors="replace"):
            m = ASSERT_RE.search(line)
            if not m:
                continue
            try:
                rec = json.loads(m.group(1))
            except ValueError:
                continue
            t, ok = rec.get("t"), rec.get("ok")
            if (t in ("always", "always_or_unreachable") and ok is False) or t == "unreachable":
                bad.add(rec.get("id", "?"))
    return sorted(bad)


def property_table(outdir, tags):
    """The exploration-wide property table from hvx/hydo_assert_report (built on the fly),
    over the consoles of the given runs. Returns its text, or None if no run emitted records."""
    # One console file per SCHEDULE: a power cut's recovery boot is part of the same run,
    # so its console is appended to the workload's (the report tool counts files as runs).
    files = []
    merged_dir = os.path.join(outdir, "consoles-merged")
    os.makedirs(merged_dir, exist_ok=True)
    for t in tags:
        text = ""
        for suffix in ("", ".recovery"):
            p = os.path.join(outdir, "consoles", "%s%s.log" % (t, suffix))
            if os.path.exists(p):
                text += open(p, errors="replace").read()
        if "HYDO-ASSERT" in text:
            m = os.path.join(merged_dir, "%s.log" % t)
            open(m, "w").write(text)
            files.append(m)
    if not files:
        return None
    tool = os.path.join(outdir, "hydo_assert_report")
    subprocess.run(["cc", "-O2", "-o", tool, os.path.join(HVX, "hydo_assert_report.c")], check=True)
    r = subprocess.run([tool] + files, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    return r.stdout


def verdict_kind(v):
    if "HYDO-ORACLE: OK" in v:
        return "ok"
    if "VIOLATION" in v:
        return "violation"
    return "error"            # harness / guest setup problems, timeouts


def signature(v):
    v = v.replace("HYDO-ORACLE: ", "")
    v = re.sub(r"\d+", "#", v)
    return v


# ---------------------------------------------------------------- the worker pool
class Worker(threading.Thread):
    def __init__(self, idx, jobs, results, outdir):
        super().__init__(daemon=True)
        self.idx, self.jobs, self.results, self.outdir = idx, jobs, results, outdir
        self.boot_secs = None

    def run(self):
        cmd = [os.path.expanduser("~/bin/hvx-run"), "sudo", "env", "HVX_SNAP_DIR=/var/tmp/hvx-snap",
               os.path.join(HVX, "hvx_explore"), "serve", os.path.join(BUILD, "bzImage"),
               os.path.join(BUILD, "initramfs-kafka.cpio.gz"), os.path.join(BUILD, IMAGE),
               os.path.join(self.outdir, "consoles")]
        err = open(os.path.join(self.outdir, "worker-%d.err" % self.idx), "w")
        p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, bufsize=1)
        line = p.stdout.readline()
        if not line.startswith("READY"):
            print("worker %d failed to start: %r" % (self.idx, line), file=sys.stderr)
            return
        self.boot_secs = float(line.split("boot_secs=")[1])
        while True:
            job = self.jobs.get()
            if job is None:
                break
            tag, text = job
            p.stdin.write("RUN %s %s\n" % (tag, text))
            p.stdin.flush()
            out = p.stdout.readline()
            m = re.match(r"RESULT (\S+) fp=(\S+) wfp=(\S+) secs=(\S+) verdict=(.*)", out.strip())
            if not m:
                self.results.put((tag, text, {"fp": "0", "secs": 0, "verdict": "ERROR worker: %r" % out, "worker": self.idx}))
                continue
            self.results.put((tag, text, {"fp": m.group(2), "wfp": m.group(3), "secs": float(m.group(4)),
                                          "verdict": m.group(5), "worker": self.idx}))
        p.stdin.write("QUIT\n")
        p.stdin.flush()
        p.wait()


class Pool:
    def __init__(self, n, outdir):
        self.jobs, self.results = queue.Queue(), queue.Queue()
        os.makedirs(os.path.join(outdir, "consoles"), exist_ok=True)
        self.workers = [Worker(i, self.jobs, self.results, outdir) for i in range(n)]
        for w in self.workers:
            w.start()

    def run_all(self, items):
        """items: list of (tag, schedule text) -> dict tag -> (text, result)"""
        for it in items:
            self.jobs.put(it)
        out = {}
        while len(out) < len(items):
            tag, text, res = self.results.get()
            out[tag] = (text, res)
        return out

    def close(self):
        for _ in self.workers:
            self.jobs.put(None)
        for w in self.workers:
            w.join()


# ---------------------------------------------------------------- shrinking
def candidates(s):
    """Simpler variants of schedule s, most aggressive first."""
    out = []
    def var(**kw):
        t = dict(s)
        for k, v in kw.items():
            if v is None:
                t.pop(k, None)
            else:
                t[k] = v
        out.append(t)
    if "K" in s:
        var(K=None)
    if "N" in s:
        var(N=None)
    for k in ("M", "R"):
        if k in s:
            var(**{k: None})
    if s["disk"] != "none":
        var(disk="none")
    if "C" in s and s["disk"] == "none":
        var(C=None)
    if "N" in s:
        svc, a, b, kinds = s["N"]
        if len(kinds) > 1:
            for i in range(len(kinds)):
                var(N=(svc, a, b, kinds[:i] + kinds[i + 1:]))
        for i, (k, v) in enumerate(kinds):
            if v > 1:
                var(N=(svc, a, b, kinds[:i] + [(k, max(1, v // 2))] + kinds[i + 1:]))
        if b - a > 1:
            var(N=(svc, a, a + max(1, (b - a) // 2), kinds))
    if "K" in s and s["K"][2] > 1:
        var(K=(s["K"][0], s["K"][1], 1))
    if s["disk"].startswith(("lying", "ioerr")):
        p = int(s["disk"].split(":")[1])
        if p > 1:
            var(disk="%s:%d" % (s["disk"].split(":")[0], max(1, p // 2)))
    return out


def same_failure(v_ref, v):
    """A variant counts as 'still failing the same way' when it is a violation of the same
    kind in the same service (numbers ignored)."""
    if verdict_kind(v) != "violation":
        return False
    return signature(v).split(" order=")[0] == signature(v_ref).split(" order=")[0]


def shrink(pool, s, verdict, log, budget=40):
    cur, cur_v, tries, rnd = s, verdict, 0, 0
    while tries < budget:
        cands = candidates(cur)
        if not cands:
            break
        rnd += 1
        items = [("shrink-%d-%d" % (rnd, i), render(c)) for i, c in enumerate(cands)]
        res = pool.run_all(items)
        tries += len(items)
        progressed = False
        for (tag, text), c in zip(items, cands):
            v = res[tag][1]["verdict"]
            log.write(json.dumps({"shrink": text, "verdict": v}) + "\n")
            log.flush()
            if same_failure(cur_v, v):
                cur, cur_v, progressed = c, v, True
                break
        if not progressed:
            break
    return cur, cur_v, tries


# ---------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", default="1-200")
    ap.add_argument("--workers", type=int, default=3)   # the box has 4 cores: 3 is fastest (M38)
    ap.add_argument("--image", default="explore-disk.img", help="guest disk image under ~/hydo/build")
    ap.add_argument("--out", required=True)
    ap.add_argument("--schedule", help="run just this schedule (no generation)")
    ap.add_argument("--no-shrink", action="store_true")
    a = ap.parse_args()
    global IMAGE
    IMAGE = a.image
    os.makedirs(a.out, exist_ok=True)
    t0 = time.time()
    pool = Pool(a.workers, a.out)
    if a.schedule:
        res = pool.run_all([("manual", a.schedule)])
        print(json.dumps(res["manual"][1]))
        pool.close()
        return
    lo, hi = [int(x) for x in a.seeds.split("-")]
    seeds = list(range(lo, hi + 1))
    items = [("seed-%d" % sd, render(generate(sd))) for sd in seeds]
    t_sweep = time.time()
    res = pool.run_all(items)
    t_sweep = time.time() - t_sweep
    runs = open(os.path.join(a.out, "runs.jsonl"), "w")
    fails = []
    for sd in seeds:
        tag = "seed-%d" % sd
        text, r = res[tag]
        s = parse(text)
        rec = {"seed": sd, "schedule": text, "class": finding_class(s), **r}
        rec["assert_failures"] = assert_failures(a.out, tag)
        if rec["assert_failures"] and verdict_kind(r["verdict"]) == "ok":
            # the HYDO-ORACLE line said OK but a property failed: the property wins
            rec["verdict"] = r["verdict"] = "VIOLATION assertion " + ",".join(rec["assert_failures"])
        runs.write(json.dumps(rec) + "\n")
        if verdict_kind(r["verdict"]) != "ok":
            fails.append(rec)
    runs.close()
    # reproduce every failure once (on whichever worker is free -- usually a different one)
    rep = pool.run_all([("repro-%d" % f["seed"], f["schedule"]) for f in fails])
    for f in fails:
        rr = rep["repro-%d" % f["seed"]][1]
        f["repro_fp"], f["repro_verdict"] = rr["fp"], rr["verdict"]
        f["reproduced"] = (rr["fp"] == f["fp"])
    # shrink one representative per (class, signature)
    reps = {}
    for f in fails:
        key = (f["class"], signature(f["verdict"]).split(" order=")[0] if verdict_kind(f["verdict"]) == "violation" else signature(f["verdict"]))
        reps.setdefault(key, f)
    slog = open(os.path.join(a.out, "shrink.jsonl"), "w")
    shrunk = {}
    if not a.no_shrink:
        for key, f in reps.items():
            if verdict_kind(f["verdict"]) != "violation":
                continue
            m, mv, n = shrink(pool, parse(f["schedule"]), f["verdict"], slog)
            mt = render(m)
            conf = pool.run_all([("min-a", mt), ("min-b", mt)])
            shrunk[key] = {"from_seed": f["seed"], "minimal": mt, "verdict": mv, "tries": n,
                           "min_class": finding_class(m),
                           "fp_a": conf["min-a"][1]["fp"], "fp_b": conf["min-b"][1]["fp"],
                           "min_verdict": conf["min-a"][1]["verdict"]}
    slog.close()
    boot = [w.boot_secs for w in pool.workers if w.boot_secs]
    pool.close()
    total = time.time() - t0
    secs = [res["seed-%d" % sd][1]["secs"] for sd in seeds]
    summary = {"schedules": len(seeds), "workers": a.workers, "sweep_wall_s": round(t_sweep, 1),
               "runs_per_hour": round(len(seeds) / t_sweep * 3600, 1), "mean_run_s": round(sum(secs) / len(secs), 1),
               "worker_boot_s": round(sum(boot) / max(1, len(boot)), 1), "total_wall_s": round(total, 1),
               "failures": len(fails), "reproduced": sum(1 for f in fails if f["reproduced"])}
    json.dump({"summary": summary, "failures": fails,
               "shrunk": [{"class": k[0], "signature": k[1], **v} for k, v in shrunk.items()]},
              open(os.path.join(a.out, "report.json"), "w"), indent=1)
    table = property_table(a.out, ["seed-%d" % sd for sd in seeds])
    if table is not None:
        open(os.path.join(a.out, "properties.txt"), "w").write(table)
    write_report(a.out, summary, fails, shrunk, table)
    print(json.dumps(summary))


def write_report(out, summary, fails, shrunk, table=None):
    L = ["# Explorer report", "", "| | |", "|---|---|"]
    for k, v in summary.items():
        L.append("| %s | %s |" % (k, v))
    by = {}
    for f in fails:
        k = (f["class"], signature(f["verdict"]).split(" order=")[0] if verdict_kind(f["verdict"]) == "violation" else signature(f["verdict"]))
        by.setdefault(k, []).append(f)
    L += ["", "## Failures by class and signature", "", "| class | signature | runs | seeds | reproduced |", "|---|---|---|---|---|"]
    for (c, sig), fs in sorted(by.items()):
        L.append("| %s | `%s` | %d | %s | %d/%d |" % (c, sig, len(fs), " ".join(str(f["seed"]) for f in fs[:8]),
                                                     sum(1 for f in fs if f["reproduced"]), len(fs)))
    L += ["", "## Minimal reproducers", ""]
    for (c, sig), v in shrunk.items():
        mc = v.get("min_class", c)
        head = mc if mc == c else "%s (found as %s; the minimal schedule needs only %s)" % (mc, c, mc)
        L += ["### %s: `%s`" % (head, sig), "",
              "- from seed %d, %d shrink runs" % (v["from_seed"], v["tries"]),
              "- minimal schedule: `%s`" % v["minimal"],
              "- verdict: `%s`" % v["min_verdict"],
              "- fingerprint, two runs: `%s` / `%s` (%s)" % (v["fp_a"], v["fp_b"], "identical" if v["fp_a"] == v["fp_b"] else "DIFFERENT"),
              "- by hand: `~/bin/hvx-run sudo ./hvx_explore one ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz ~/hydo/build/%s <outdir> \"%s\"`" % (IMAGE, v["minimal"]), ""]
    L += ["## Property table over every schedule (hvx/ASSERTIONS.md)", ""]
    if table is None:
        L += ["The guest emitted no HYDO-ASSERT records (an image built without the assertion SDK).", ""]
    else:
        L += ["```", table.rstrip(), "```", ""]
    open(os.path.join(out, "report.md"), "w").write("\n".join(L) + "\n")


if __name__ == "__main__":
    main()
