"""fc_adaptive.py -- the adaptive freeze sweep (explore.py --sweep freeze --fc-adaptive).

M76's freeze sweep runs one schedule F=<svc><dir><peer>@<step>#<n>[:<match>]:<dur> per packet of
every swept stream. On two links over two steps that can be over a thousand runs; the
failing points sit in a few narrow bands. This sweep spends runs where failures are:

  stage 1  a cheap sweep: every packet of the manifest's freeze_match: streams (M76's "manifest"
           streams), or, for a manifest without freeze_match:, every k-th packet (--fc-stride) of
           the every-packet streams. Over every allowed pair and every --fc-steps step.
  stage 2  around every stage-1 failure (and, with --fc-centres props/state, around points whose
           outcome changed in a weaker way), every packet of the every-packet streams in a window
           of +-W packets (--fc-window): the failing point's own stream, the opposite direction of
           the same pair, and (--fc-links svc, the default) both directions of every other freeze
           pair of the frozen service. Then each window widens until W packets beyond its
           outermost failure pass on both sides, so a failing band is mapped whole.

Locating a stage-1 failure on the every-packet streams: the helper's "hydo-fc: FROZE ... at resp #15
(resp308, ...)" line gives the packet's ordinal among ALL payload packets in that direction since
the step began, i.e. its n on the every-packet stream of the same direction. The opposite direction
and the other links are found through the #0 trace runs of those streams at the same step (which
the sweep runs anyway for their packet counts): the trace lists each payload packet in order with
its guest uptime t=; the opposite direction's n is the last such packet before ours in the same
trace, another link's n the packet nearest in time. The console shows only the first 600 trace
lines, so a packet beyond them is placed by extrapolating from the last visible ones. The mapping is
only a centre: the window and the widening absorb its error.

Every point is an ordinary schedule; explore.py main re-runs each failure (twice by default) and
writes the report, exactly as for the full sweep. fc-adaptive.json records the centres, windows
and rounds.
"""
import json, os, re, sys, time

FROZE_RE = re.compile(r"hydo-fc: FROZE .* at (req|resp) #(\d+) \((req|resp)(\d+),")
TRACE_RE = re.compile(r"hydo-fc-trace: (req|resp)(\d+) len=\d+(?: #\d+)? t=(\d+)")
DIRWORD = {">": "req", "<": "resp"}


def log(msg):
    print("fc-adaptive: " + msg, file=sys.stderr)
    sys.stderr.flush()


def read(path):
    try:
        return open(path, errors="replace").read()
    except OSError:
        return ""


def trace_lines(text):
    """[(dirword, k, t)] of a #0 trace console, in packet order (the visible part)."""
    return [(m.group(1), int(m.group(2)), int(m.group(3))) for m in TRACE_RE.finditer(text)]


def props_sig(text):
    """The outcome of every non-`always` property in a console (sometimes / reachable / eventually
    / unreachable), without the fault.* records the sweep itself causes."""
    out = set()
    for m in re.finditer(r"HYDO-ASSERT (\{.*\})", text):
        try:
            r = json.loads(m.group(1))
        except ValueError:
            continue
        if r.get("t") in ("always", "always_or_unreachable") or str(r.get("id", "")).startswith("fault."):
            continue
        out.add((r.get("t"), r.get("id"), r.get("ok")))
    return frozenset(out)


class Adaptive:
    def __init__(self, ex, pool, a):
        self.ex, self.pool, self.a = ex, pool, a
        self.base = ex.fc_sweep_with(a.sweep_with)
        self.durs = [int(v) for v in a.fc_durs.split(",")]
        self.W = a.fc_window
        self.traces = {}            # (svc, peer, d, m, st) -> trace record (count, console path, ...)
        self.trecs = []
        self.ntrace = 0
        self.points = []            # [(label, schedule dict)] in seed order
        self.items = []             # [(tag, text)]
        self.res = {}               # tag -> (text, result)
        self.by_key = {}            # point key (svc, d, peer, st, n, m, dur) -> seed index
        self.stage = {}             # seed -> 1 or 2
        known = {}
        for path in (a.fc_traces.split(",") if a.fc_traces else []):
            for line in open(path):
                rec = json.loads(line)
                if rec.get("count") is not None:
                    known[rec["schedule"]] = rec
        self.known = known

    # ------------------------------------------------------------ traces
    def need_traces(self, keys):
        """Run the #0 trace of every (svc, peer, d, m, st) not traced yet, in one batch."""
        ex = self.ex
        todo, tags = [], {}
        for key in keys:
            if key in self.traces or key in tags.values():
                continue
            svc, peer, d, m, st = key
            t = dict(self.base)
            t["F"] = (svc, d, peer, st, 0, m, 1)
            text = ex.render(t)
            if text in self.known and self.known[text].get("console") and os.path.exists(self.known[text]["console"]):
                rec = dict(self.known[text], reused=True)
                self.traces[key] = rec
                self.trecs.append(rec)
                continue
            tag = "fctrace-%d" % self.ntrace
            self.ntrace += 1
            tags[tag] = key
            todo.append((tag, text))
        if not todo:
            return
        res = self.pool.run_all(todo)
        for tag, (text, r) in sorted(res.items()):
            svc, peer, d, m, st = key = tags[tag]
            path = os.path.join(self.a.out, "consoles", tag + ".log")
            n = None
            for mm in ex.FC_TRACE_RE.finditer(read(path)):
                if int(mm.group(2)) == st:
                    n = int(mm.group(1))
            if n is None:
                raise SystemExit("explore.py: --fc-adaptive: no packet count in the console of %s (%s)" % (tag, text))
            rec = {"tag": tag, "schedule": text, "pair": "%s>%s" % (svc, peer), "dir": d, "match": m, "step": st,
                   "count": n, "verdict": r["verdict"], "fp": r["fp"], "console": path}
            self.traces[key] = rec
            self.trecs.append(rec)

    def count(self, key):
        return self.traces[key]["count"]

    def lines(self, key):
        rec = self.traces[key]
        if "_lines" not in rec:
            rec["_lines"] = trace_lines(read(rec.get("console", "")))
        return rec["_lines"]

    # ------------------------------------------------------------ points
    def add_point(self, svc, d, peer, st, n, m, dur, stage):
        key = (svc, d, peer, st, n, m, dur)
        if key in self.by_key:
            return None
        ex = self.ex
        i = len(self.points) + 1
        s = dict(self.base)
        s["F"] = key
        self.points.append((ex.render_fc(s["F"])[2:], s))
        self.by_key[key] = i
        self.stage[i] = stage
        text = ex.render(ex.add_turns(ex.add_buggify(ex.add_preempt(ex.add_concurrency(ex.add_timing(s, i), i), i), i), i))
        self.items.append(("seed-%d" % i, text))
        return ("seed-%d" % i, text)

    def run(self, items):
        if items:
            self.res.update(self.pool.run_all(items))

    def failed(self, i):
        tag = "seed-%d" % i
        text, r = self.res[tag]
        return self.ex.verdict_kind(r["verdict"]) != "ok" or bool(self.ex.assert_failures(self.a.out, tag))

    def console(self, i):
        return read(os.path.join(self.a.out, "consoles", "seed-%d.log" % i))

    # ------------------------------------------------------------ mapping a point onto other streams
    def own_t(self, tkey, dw, k):
        """Guest uptime of the k-th `dw` packet in trace tkey (extrapolated past the visible lines)."""
        L = [(kk, t) for w, kk, t in self.lines(tkey) if w == dw]
        for kk, t in L:
            if kk == k:
                return t
        if not L:
            return None
        if k < L[0][0]:
            return L[0][1]
        tail = L[-20:]
        rate = (tail[-1][1] - tail[0][1]) / max(1, tail[-1][0] - tail[0][0])
        return tail[-1][1] + (k - tail[-1][0]) * rate

    def opposite(self, tkey, dw, k, ow):
        """The opposite direction's n just before the k-th `dw` packet of trace tkey."""
        L = self.lines(tkey)
        last = {"req": 0, "resp": 0}
        for w, kk, t in L:
            if w == dw and kk == k:
                return max(1, last[ow])
            last[w] = kk
        return max(1, k + last[ow] - last[dw]) if L else k      # past the visible lines: keep the offset

    def at_time(self, tkey, dw, t, n_max):
        """The n of the `dw` packet of trace tkey nearest guest uptime t."""
        L = [(kk, tt) for w, kk, tt in self.lines(tkey) if w == dw]
        if not L or t is None:
            return None
        if t > L[-1][1] and len(L) >= 2:              # later than every visible packet: extrapolate
            tail = L[-20:]
            rate = (tail[-1][0] - tail[0][0]) / max(1.0, tail[-1][1] - tail[0][1])
            return max(1, min(n_max, int(round(tail[-1][0] + (t - tail[-1][1]) * rate))))
        return min(L, key=lambda x: (abs(x[1] - t), x[0]))[0]

    def centre_windows(self, i, why):
        """Stage-2 windows (stream key -> centre n) around stage-1 point i."""
        a, ex = self.a, self.ex
        label, s = self.points[i - 1]
        svc, d, peer, st, n, m, dur = s["F"]
        mm = FROZE_RE.search(self.console(i))
        if not mm:
            log("point %s: the freeze never fired, no centre" % label)
            return {}, None
        k = int(mm.group(4))                          # ordinal among ALL payload packets of that direction
        dw = mm.group(3)
        d = ">" if dw == "req" else "<"
        pairs = [(svc, peer)]
        if a.fc_links == "svc":
            pairs += [p for p in self.allowed_pairs() if p[0] == svc and p != (svc, peer)]
        dirs = [x for x in "<>" if x in self.dirs()]
        keys = [(p[0], p[1], x, None, st) for p in pairs for x in dirs]
        self.need_traces(keys + [(svc, peer, d, None, st)])
        own = (svc, peer, d, None, st)
        t = self.own_t(own, dw, k)
        out = {}
        for key in keys:
            q, qd, ow = key[1], key[2], DIRWORD[key[2]]
            N = self.count(key)
            if N == 0:
                continue
            if q == peer and qd == d:
                c = k
            elif q == peer:
                c = self.opposite(own, dw, k, ow)
            else:
                c = self.at_time(key, ow, t, N)
                if c is None:                         # no visible trace: by proportion
                    c = max(1, int(round(k * N / max(1, self.count(own)))))
            out[key] = max(1, min(N, c))
        return out, {"point": label, "why": why, "froze": "%s%d" % (dw, k), "t": t,
                     "centres": {"%s%s%s@%d" % (kk[0], kk[2], kk[1], kk[4]): c for kk, c in out.items()}}

    def allowed_pairs(self):
        pairs = list(self.ex.FC_PAIRS)
        if self.a.fc_pairs:
            want = [tuple(re.split(r"[>:]", p.strip(), 1)) for p in self.a.fc_pairs.split(",") if p.strip()]
            pairs = [p for p in pairs if p in want]
        return pairs

    def dirs(self):
        return [d for d in "<>" if d in self.a.fc_dirs.replace("resp", "<").replace("req", ">")]

    # ------------------------------------------------------------ the two stages
    def stage1(self):
        a, ex = self.a, self.ex
        steps = sorted({v for part in a.fc_steps.split(",") for v in
                        (range(int(part.split("-")[0]), int(part.split("-")[1]) + 1) if "-" in part else [int(part)])}) \
            if a.fc_steps else list(range(ex.NORDERS))
        if not steps or steps[0] < 0 or steps[-1] >= ex.NORDERS:
            raise SystemExit("explore.py: --fc-steps must lie in 0..%d" % (ex.NORDERS - 1))
        modes = a.fc_stage1.split(",")
        if modes == ["auto"]:
            modes = ["manifest"] if ex.FC_MATCH else ["stride"]
        dirs = self.dirs()
        streams = []                                   # (svc, peer, d, m, every)
        for svc, peer in self.allowed_pairs():
            for mode in modes:
                if mode == "manifest":
                    for m, md in ex.FC_MATCH:
                        streams += [(svc, peer, x, m, 1) for x in ([md] if md else dirs) if x in dirs]
                elif mode == "stride":
                    streams += [(svc, peer, x, None, a.fc_stride) for x in dirs]
                else:
                    raise SystemExit("explore.py: --fc-stage1 %r: expected auto, manifest, stride" % mode)
        if not streams:
            raise SystemExit("explore.py: --fc-adaptive: no stage-1 streams (check --fc-pairs, --fc-dirs, "
                             "--fc-stage1; stage 1 'manifest' needs freeze_match: in the manifest)")
        tsteps = sorted(set(steps) | {s + 1 for s in steps if s + 1 < ex.NORDERS})
        keys = [(svc, peer, d, m, st) for svc, peer, d, m, _ in streams for st in tsteps]
        log("stage 1 (%s): %d streams x %d steps, %d trace runs" % (",".join(modes), len(streams), len(tsteps), len(keys)))
        self.need_traces(keys)
        items = []
        for svc, peer, d, m, every in streams:
            for st in steps:
                key = (svc, peer, d, m, st)
                n_here = self.count(key)
                if st + 1 < ex.NORDERS:
                    n_here = max(0, n_here - self.count((svc, peer, d, m, st + 1))) + (0 if st + 1 in steps else a.fc_margin)
                    n_here = min(n_here, self.count(key))
                for n in range(1, n_here + 1, every):
                    for dur in self.durs:
                        it = self.add_point(svc, d, peer, st, n, m, dur, 1)
                        if it:
                            items.append(it)
        log("stage 1: %d points" % len(items))
        self.run(items)
        return [int(t[5:]) for t, _ in items]

    def centres_of(self, seeds):
        """Stage-1 points that seed a window: failures, and (--fc-centres) weaker outcome changes."""
        want = set(self.a.fc_centres.split(","))
        out = []
        for i in seeds:
            if self.failed(i):
                out.append((i, "fail"))
                continue
            if not want & {"props", "state"}:
                continue
            _, s = self.points[i - 1]
            svc, d, peer, st, n, m, dur = s["F"]
            tr = self.traces.get((svc, peer, d, m, st))
            if not tr:
                continue
            base = read(tr.get("console", ""))
            mine = self.console(i)
            if "props" in want and props_sig(mine) != props_sig(base):
                out.append((i, "props"))
            elif "state" in want and self.res["seed-%d" % i][1]["verdict"] != tr["verdict"]:
                out.append((i, "state"))
        return out

    def stage2(self, seeds1):
        cents = self.centres_of(seeds1)
        log("stage 2: %d centres (%s)" % (len(cents), ", ".join("%s[%s]" % (self.points[i - 1][0], w) for i, w in cents)))
        windows = {}                                   # stream key -> list of [lo, hi]
        self.centre_log = []
        for i, why in cents:
            cs, info = self.centre_windows(i, why)
            if info:
                self.centre_log.append(info)
            for key, c in cs.items():
                N = self.count(key)
                windows.setdefault(key, []).append([max(1, c - self.W), min(N, c + self.W)])
        self.rounds = []
        rnd = 0
        while True:
            for key in windows:
                windows[key] = merge(windows[key])
            items = []
            for key, ivs in sorted(windows.items(), key=lambda kv: str(kv[0])):
                svc, peer, d, m, st = key
                for lo, hi in ivs:
                    for n in range(lo, hi + 1):
                        for dur in self.durs:
                            it = self.add_point(svc, d, peer, st, n, None, dur, 2)
                            if it:
                                items.append(it)
            if not items:
                break
            rnd += 1
            t0 = time.time()
            self.run(items)
            nf = sum(1 for t, _ in items if self.failed(int(t[5:])))
            self.rounds.append({"round": rnd, "points": len(items), "failures": nf, "secs": round(time.time() - t0, 1)})
            log("stage 2 round %d: %d points, %d failures" % (rnd, len(items), nf))
            changed = False                            # widen: W passing packets beyond each band's ends
            for key, ivs in windows.items():
                svc, peer, d, m, st = key
                N = self.count(key)
                for iv in ivs:
                    fails = [n for n in range(iv[0], iv[1] + 1)
                             if any(self.failed(self.by_key[(svc, d, peer, st, n, None, dur)]) for dur in self.durs)]
                    if not fails:
                        continue
                    lo, hi = max(1, fails[0] - self.W), min(N, fails[-1] + self.W)
                    if lo < iv[0] or hi > iv[1]:
                        iv[0], iv[1] = min(lo, iv[0]), max(hi, iv[1])
                        changed = True
            if not changed:
                break
        self.windows = {"%s%s%s@%d" % (k[0], k[2], k[1], k[4]): v for k, v in windows.items()}


def merge(ivs):
    ivs = sorted(ivs)
    out = []
    for lo, hi in ivs:
        if out and lo <= out[-1][1] + 1:
            out[-1][1] = max(out[-1][1], hi)
        else:
            out.append([lo, hi])
    return out


def run(ex, pool, a):
    """--sweep freeze --fc-adaptive. Returns (points, items, res, extra summary fields)."""
    if not ex.FC_PAIRS:
        raise SystemExit("explore.py: --sweep freeze needs an image whose manifest lists `freeze` in faults: "
                         "(manifest_space.py gives the pairs)")
    if a.sweep_dry:
        raise SystemExit("explore.py: --fc-adaptive runs its points as it goes; --sweep-dry is for the full sweep")
    A = Adaptive(ex, pool, a)
    t0 = time.time()
    s1 = A.stage1()
    t1 = time.time()
    A.stage2(s1)
    t2 = time.time()
    with open(os.path.join(a.out, "fc-traces.jsonl"), "w") as f:
        for rec in A.trecs:
            f.write(json.dumps({k: v for k, v in rec.items() if not k.startswith("_")}) + "\n")
    n1 = sum(1 for v in A.stage.values() if v == 1)
    info = {"stage1_points": n1, "stage2_points": len(A.points) - n1, "trace_runs": A.ntrace,
            "stage1_failures": sum(1 for i, v in A.stage.items() if v == 1 and A.failed(i)),
            "stage2_failures": sum(1 for i, v in A.stage.items() if v == 2 and A.failed(i)),
            "stage1_wall_s": round(t1 - t0, 1), "stage2_wall_s": round(t2 - t1, 1),
            "fc_window": A.W, "fc_links": a.fc_links, "fc_centres": a.fc_centres}
    json.dump({"summary": info, "centres": A.centre_log, "windows": A.windows, "rounds": A.rounds,
               "stage": {"seed-%d" % i: v for i, v in sorted(A.stage.items())}},
              open(os.path.join(a.out, "fc-adaptive.json"), "w"), indent=1)
    log("done: %s" % json.dumps(info))
    return A.points, A.items, A.res, info
