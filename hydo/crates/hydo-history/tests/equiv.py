#!/usr/bin/env python3
"""equiv.py -- prove hydo-history (Rust) gives exactly what hydo/history/hydohist.py gives.

    python3 equiv.py gen   <cases.jsonl> [--n N] [--seed S] [--large]
                                                               a seeded random corpus (--large:
                                                               long histories, for timing)
    python3 equiv.py units <cases.jsonl>                       the histories of test_hydohist.py
    python3 equiv.py consoles <cases.jsonl> <dir>...           every saved run under the dirs
                                                               (<tag>.log + <tag>.recovery.log)
    python3 equiv.py py    <cases.jsonl> <out.jsonl> [--jobs J] [--times F]
                                                               the reference's answers
Then compare with the Rust side:
    cargo run --release --example hist_batch < cases.jsonl > rs.jsonl ; cmp out.jsonl rs.jsonl

Each output line is one case's answer as json.dumps(sort_keys=True, separators=(",", ":")):
the records, the verdict, the report, the warnings and every check's status, summary,
explanation, details and warnings (or {"crash": true} when the reference raised).
"""
import json
import os
import random
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
HIST = os.path.normpath(os.path.join(HERE, "..", "..", "..", "history"))
sys.path.insert(0, HIST)
import hydohist as H   # noqa: E402


def dumps(x):
    return json.dumps(x, sort_keys=True, separators=(",", ":"))


# ------------------------------------------------------------------ the reference's answers
def answer(case):
    k = case["kind"]
    if k == "pdec":
        return [H.pdec(t) for t in case["toks"]]
    if k == "penc":
        return [H.penc(s, case["extra"]) for s in case["strs"]]
    if k == "decode":
        import tempfile
        fd, p = tempfile.mkstemp()
        os.write(fd, bytes.fromhex(case["hex"]))
        os.close(fd)
        try:
            return open(p, errors="replace").read()
        finally:
            os.unlink(p)
    srcs = [tuple(s) for s in case["srcs"]]
    try:
        r = H.run_checks(srcs, case.get("verdict"), budget=case.get("budget", H.DEFAULT_BUDGET),
                         specs=case.get("specs"))
    except Exception:          # an exception the reference does not catch
        return {"crash": True}
    res = [[i, {"status": x["status"], "summary": x["summary"], "explanation": x["explanation"],
                "details": x["details"], "warnings": x.get("warnings", [])}]
           for i, x in r["results"].items()]
    return {"records": r["records"], "verdict": r["verdict"], "report": r["report"],
            "warnings": r["warnings"], "results": res}


def _one(line):
    case = json.loads(line)
    t0 = time.perf_counter_ns()
    a = answer(case)
    return dumps(a), time.perf_counter_ns() - t0


def run_py(cases, out, jobs, times):
    lines = [x for x in open(cases) if x.strip()]
    if jobs > 1:
        import multiprocessing
        with multiprocessing.Pool(jobs) as p:
            res = p.map(_one, lines, chunksize=4)
    else:
        res = [_one(x) for x in lines]
    with open(out, "w") as f:
        for a, _ in res:
            f.write(a + "\n")
    if times:
        with open(times, "w") as f:
            for (_, t), x in zip(res, lines):
                if json.loads(x)["kind"] == "checks":
                    f.write("%d\n" % t)


# ------------------------------------------------------------------ the unit tests' histories
def units(path):
    """Run test_hydohist.py's tests with the checker entry points wrapped, and keep every
    history a test checks (sources, spec, budget) as a case."""
    import unittest
    import test_hydohist as T
    cases = []
    last = {"srcs": None}
    inside = [0]
    orig_ps, orig_rc, orig_rcs = H.parse_sources, H.run_check, H.run_checks

    def ps(sources):
        last["srcs"] = [list(s) for s in sources]
        return orig_ps(sources)

    def rc(events, spec, budget=H.DEFAULT_BUDGET):
        if not inside[0]:
            cases.append({"kind": "checks", "srcs": last["srcs"], "verdict": None, "budget": budget,
                          "specs": [spec]})
        return orig_rc(events, spec, budget)

    def rcs(sources, guest_verdict=None, budget=H.DEFAULT_BUDGET, specs=None):
        cases.append({"kind": "checks", "srcs": [list(s) for s in sources], "verdict": guest_verdict,
                      "budget": budget, "specs": specs})
        inside[0] += 1
        try:
            return orig_rcs(sources, guest_verdict, budget, specs)
        finally:
            inside[0] -= 1

    H.parse_sources, H.run_check, H.run_checks = ps, rc, rcs
    try:
        suite = unittest.defaultTestLoader.loadTestsFromModule(T)
        r = unittest.TextTestRunner(verbosity=0, stream=open(os.devnull, "w")).run(suite)
    finally:
        H.parse_sources, H.run_check, H.run_checks = orig_ps, orig_rc, orig_rcs
    toks = ["a b%c@d,e", "%", "nil", "%6Eil", "v%201", "%41%4", "%%41", "%E2%82", "%ff%FE", "x"]
    cases.append({"kind": "pdec", "toks": toks + [H.penc(t, ",") for t in toks]})
    cases.append({"kind": "penc", "strs": toks + ["", "a\tb", "\x01", "é ü", "[1,2]", "k:v"], "extra": ",[]:"})
    with open(path, "w") as f:
        for c in cases:
            f.write(dumps(c) + "\n")
    print("units: %d tests run (%d failures, %d errors), %d cases" % (
        r.testsRun, len(r.failures), len(r.errors), len(cases)))


# ------------------------------------------------------------------ saved consoles
def consoles(path, dirs):
    """Every run whose console holds HYDO-HIST or HYDO-CHECK lines: its workload console and its
    recovery console, with the guest verdict of the consoles (as hydo-check takes it)."""
    import glob
    cases = []
    seen = set()
    for d in dirs:
        for p in sorted(glob.glob(os.path.join(d, "**", "*.log"), recursive=True)):
            if p.endswith(".recovery.log") or p.endswith(".checks.log"):
                continue
            if p in seen:
                continue
            seen.add(p)
            base = p[:-4]
            tag = os.path.basename(base)
            srcs = [(tag, open(p, errors="replace").read())]
            if os.path.exists(base + ".recovery.log"):
                srcs.append((tag + ".recovery", open(base + ".recovery.log", errors="replace").read()))
            if not any("HYDO-HIST " in t or "HYDO-CHECK" in t for _, t in srcs):
                continue
            v = None
            for _, text in srcs:
                for line in text.splitlines():
                    k = line.find("HYDO-ORACLE:")
                    if k >= 0:
                        v = line[k:].strip()
            cases.append({"kind": "checks", "srcs": [list(s) for s in srcs], "verdict": v,
                          "budget": H.DEFAULT_BUDGET, "specs": None, "path": p})
    with open(path, "w") as f:
        for c in cases:
            f.write(dumps(c) + "\n")
    print("consoles: %d runs" % len(cases))


# ------------------------------------------------------------------ the random corpus
VALS = ["0", "1", "2", "3", "v1", "v2", "a%20b", "%", "%6Eil", "x%40y", "%C3%A9", "%E2%82", "\u00e9", "1_0"]
JUNK = ["", "random console text", "[   1.234] kernel: something", "HYDO-HIST", "HYDO-HIST kv 0",
        "HYDO-HIST kv 0 bogus 1 write x 1", "HYDO-HIST k$v 0 invoke 1 write x 1",
        "HYDO-HIST kv 0 invoke 12x write x 1", "HYDO-ORACLE: OK invariants=1 held (a: b)",
        "HYDO-CHECK {not json", "HYDO-CHECK-FINAL {\"rc\":1}", "HYDO-CHECK-FINAL [1]x",
        "HYDO-CHECK [1,2]", "HYDO-CHECK {\"type\":\"linearizable\"}", "\x1c", "x\u2028y",
        "HYDO-HIST kv 9 invoke 1 txn a:k:1:1", "HYDO-HIST log 9 ok 1 txn r:x:[1,2]",
        "HYDO-CHECK-FINAL {\"id\": \"kv.lin\", \"rc\": \" 4 \"}"]
CRASH = ["HYDO-CHECK-FINAL [1]", "HYDO-CHECK {\"id\": [1], \"type\": \"list-append\"}",
         "HYDO-CHECK-FINAL {\"id\": \"kv.lin\", \"rc\": Infinity}"]


def ts(rnd, i):
    c = rnd.random()
    if c < 0.85:
        return str((i + 1) * 1000000 + rnd.randrange(1000))
    return rnd.choice(["-", "0", "125000000", "-5", "1_000", "١٢", "99999999999999999999999",
                       "375000000", "-0", "+7"])


def emit(rnd, recs, hist, p, typ, f, args, i):
    pre = rnd.choice(["", "", "", "", "[  12.3] ", "junk ", "\t"])
    return "%sHYDO-HIST %s %s %s %s %s%s" % (pre, hist, p, typ, ts(rnd, i), f, "".join(" " + a for a in args))


def gen_register(rnd, hist, keys, nproc, nops, cas, versions, init_any):
    """A simulated register store; every operation takes effect at a random instant inside its
    interval (or never, for failures and some unknown outcomes)."""
    store = {} if not init_any else {k: (rnd.choice(VALS[:4]), None) for k in keys}
    rev = [rnd.randrange(1, 50)]
    lines = []
    open_ = {}
    n = 0
    while n < nops or open_:
        p = rnd.randrange(nproc)
        if p not in open_ and n < nops:
            k = rnd.choice(keys)
            c = rnd.random()
            if cas and c < 0.25:
                old = rnd.choice(VALS[:4] + ["nil"])
                new = rnd.choice(VALS[:6])
                open_[p] = ["cas", k, (old, new), False, None]
                lines.append((p, "invoke", "cas", [k, old, new]))
            elif c < 0.55:
                v = rnd.choice(VALS)
                open_[p] = ["write", k, v, False, None]
                lines.append((p, "invoke", "write", [k, v]))
            elif c < 0.62:
                open_[p] = ["readall", None, None, False, None]
                lines.append((p, "invoke", "read", ["*"]))
            else:
                open_[p] = ["read", k, None, False, None]
                lines.append((p, "invoke", "read", [k]))
            n += 1
            continue
        if p not in open_:
            continue
        f, k, v, done, res = open_[p]
        if not done:
            fate = rnd.random()
            if f == "write":
                if fate < 0.9:
                    rev[0] += rnd.randrange(1, 3)
                    store[k] = (v, rev[0])
                    res = rev[0]
                else:
                    res = "lost"
            elif f == "cas":
                old, new = v
                cur = store.get(k, (None, None))[0]
                if (cur is None and old == "nil") or cur == old:
                    rev[0] += 1
                    store[k] = (new, rev[0])
                    res = rev[0]
                else:
                    res = "nomatch"
            elif f == "read":
                res = store.get(k)
            else:
                res = {kk: store.get(kk) for kk in keys}
            open_[p] = [f, k, v, True, res]
            continue
        del open_[p]
        r = rnd.random()
        if r < 0.05:
            continue                                   # never completed: unknown outcome
        if f == "write":
            if res == "lost":
                lines.append((p, "info" if r < 0.5 else "fail", "write", [k, v]))
            elif r < 0.1:
                lines.append((p, "info", "write", [k, v]))
            else:
                lines.append((p, "ok", "write", [k, v + ("@%d" % res if versions else "")]))
        elif f == "cas":
            old, new = v
            if res == "nomatch":
                lines.append((p, "fail", "cas", [k, old, new]))
            else:
                lines.append((p, "ok", "cas", [k, old, new + ("@%d" % res if versions else "")]))
        elif f == "read":
            if r < 0.08:
                lines.append((p, "info", "read", [k]))
            else:
                lines.append((p, "ok", "read", [k, "nil" if res is None else res[0] + (
                    "@%d" % res[1] if versions and res[1] is not None else "")]))
        else:
            args = []
            for kk in sorted(res):
                x = res[kk]
                args += [kk, "nil" if x is None else x[0] + ("@%d" % x[1] if versions and x[1] is not None else "")]
            lines.append((p, "ok", "read", args))
    return lines


def mutate_register(rnd, lines):
    """Make it (maybe) wrong: a stale or invented read value, a wrong version, a write whose
    acknowledgement was a lie, a swapped pair of records."""
    lines = list(lines)
    for _ in range(rnd.choice([0, 1, 1, 2, 3])):
        if not lines:
            break
        i = rnd.randrange(len(lines))
        p, ty, f, args = lines[i]
        c = rnd.random()
        if ty == "ok" and f == "read" and len(args) >= 2 and c < 0.5:
            j = 1 + 2 * rnd.randrange(len(args) // 2)
            a = list(args)
            if "@" in a[j] and rnd.random() < 0.5:
                v, ver = a[j].rsplit("@", 1)
                a[j] = "%s@%d" % (v, int(ver) + rnd.choice([-1, 1, 2]))
            else:
                a[j] = rnd.choice(VALS + ["nil", "zz"])
            lines[i] = (p, ty, f, a)
        elif ty == "ok" and f == "write" and c < 0.7:
            lines[i] = (p, rnd.choice(["fail", "info"]), f, args)
        elif c < 0.85 and i + 1 < len(lines):
            lines[i], lines[i + 1] = lines[i + 1], lines[i]
        else:
            del lines[i]
    return lines


def gen_lists(rnd, hist, keys, nproc, nops, txn, bug):
    """A simulated list store, serializable unless `bug` (stale snapshots, lost appends,
    duplicated or reordered elements)."""
    store = {}
    elem = [0]
    lines = []
    open_ = {}
    n = 0
    while n < nops or open_:
        p = rnd.randrange(nproc)
        if p not in open_ and n < nops:
            if txn and rnd.random() < 0.6:
                mops = []
                for _ in range(rnd.randrange(1, 5)):
                    k = rnd.choice(keys)
                    if rnd.random() < 0.5:
                        elem[0] += 1
                        mops.append(("a", k, str(elem[0])))
                    else:
                        mops.append(("r", k, None))
                open_[p] = ["txn", mops, False, None, {k: list(v) for k, v in store.items()}]
                lines.append((p, "invoke", "txn", lines_args("txn", mops, False)))
            elif rnd.random() < 0.5:
                elem[0] += 1
                k = rnd.choice(keys)
                open_[p] = ["append", [("a", k, str(elem[0]))], False, None, None]
                lines.append((p, "invoke", "append", [k, str(elem[0])]))
            else:
                k = rnd.choice(keys)
                open_[p] = ["read", [("r", k, None)], False, None, {kk: list(v) for kk, v in store.items()}]
                lines.append((p, "invoke", "read", [k]))
            n += 1
            continue
        if p not in open_:
            continue
        f, mops, done, res, snap = open_[p]
        if not done and bug == "step" and rnd.random() < 0.06:
            open_[p] = [f, mops, True, ("fail", mops), snap]
            continue
        if not done and bug == "step":
            # every micro-op takes effect on its own turn: other transactions see intermediate
            # states and can see each other's appends (G1b, G1c)
            out = list(res[1]) if res else []
            m = mops[len(out)]
            if m[0] == "a":
                store.setdefault(m[1], []).append(m[2])
                out.append(m)
            else:
                out.append(("r", m[1], list(store.get(m[1], []))))
            open_[p] = [f, mops, len(out) == len(mops), ("ok", out), snap]
            continue
        if not done:
            fate = rnd.random()
            out = []
            if fate < 0.06:
                res = ("fail", mops)
            else:
                # bug "si": the whole transaction reads its start snapshot (plus its own appends);
                # "lossy"/"dup"/"swap": rare lost appends, duplicated or swapped elements
                own = {}
                for m in mops:
                    if m[0] == "a":
                        if not (bug == "lossy" and rnd.random() < 0.05):
                            store.setdefault(m[1], []).append(m[2])
                        own.setdefault(m[1], []).append(m[2])
                        out.append(m)
                    else:
                        if bug == "si" and snap is not None:
                            l = list(snap.get(m[1], [])) + own.get(m[1], [])
                        else:
                            l = list(store.get(m[1], []))
                        if bug == "dup" and l and rnd.random() < 0.02:
                            l.append(l[0])
                        if bug == "swap" and len(l) > 1 and rnd.random() < 0.02:
                            l[0], l[1] = l[1], l[0]
                        out.append(("r", m[1], l))
                res = ("ok", out)
            open_[p] = [f, mops, True, res, snap]
            continue
        del open_[p]
        r = rnd.random()
        if r < 0.04:
            continue
        st, out = res
        if st == "fail":
            lines.append((p, "fail", f, lines_args(f, mops, False)))
        elif r < 0.09:
            lines.append((p, "info", f, lines_args(f, mops, False)))
        else:
            lines.append((p, "ok", f, lines_args(f, out, True)))
    return lines


def lines_args(f, mops, ok):
    def lst(l):
        return "[" + ",".join(l) + "]"
    if f == "append":
        return [H.penc(H.pdec(mops[0][1]), ":,[]"), mops[0][2]]
    if f == "read":
        m = mops[0]
        return [H.penc(H.pdec(m[1]), ":,[]")] + ([lst(m[2])] if ok and m[2] is not None else [])
    out = []
    for m in mops:
        k = H.penc(H.pdec(m[1]), ":,[]")
        if m[0] == "a":
            out.append("a:%s:%s" % (k, m[2]))
        elif ok and m[2] is not None:
            out.append("r:%s:%s" % (k, lst(m[2])))
        else:
            out.append("r:%s" % k)
    return out


def gen_case(rnd):
    hists = []
    specs = []
    nh = rnd.choice([1, 1, 1, 2])
    size = rnd.choice([8, 15, 25, 40, 60, 100] + [250] * (rnd.random() < 0.1))
    for h in range(nh):
        name = rnd.choice(["kv", "log", "h%d" % h])
        if any(name == x[0] for x in hists):
            name += str(h)
        nproc = rnd.choice([1, 2, 3, 5, 8])
        keys = rnd.sample(["x", "y", "z", "k:1", "k%202", "w"], rnd.choice([1, 1, 2, 3]))
        if rnd.random() < 0.55:
            cas = rnd.random() < 0.4
            versions = rnd.random() < 0.4
            init_any = rnd.random() < 0.15
            lines = gen_register(rnd, name, keys, nproc, size, cas, versions, init_any)
            if rnd.random() < 0.6:
                lines = mutate_register(rnd, lines)
            sp = {"id": "%s.lin" % name, "type": "linearizable", "history": name}
            if cas or rnd.random() < 0.3:
                sp["model"] = "cas" if cas or rnd.random() < 0.5 else "register"
            if init_any or rnd.random() < 0.05:
                sp["initial"] = "any"
        else:
            txn = rnd.random() < 0.6
            bug = rnd.choice([None, None, "si", "si", "si", "step", "step", "step", "lossy", "dup", "swap"])
            lines = gen_lists(rnd, name, keys, nproc, size, txn, bug)
            if rnd.random() < 0.1:
                lines = mutate_register(rnd, lines)
            sp = {"id": "%s.la" % name, "type": "list-append", "history": name}
            if rnd.random() < 0.85:
                sp["consistency"] = rnd.choice(H.LEVELS)
        c = rnd.random()
        if c < 0.03:
            sp["type"] = rnd.choice(["linear", None, 5])
        elif c < 0.05:
            sp["model"] = rnd.choice(["reg", None, [1, "a"]])
        elif c < 0.07:
            sp["consistency"] = rnd.choice(["linearizable", 3])
        if rnd.random() < 0.3:
            sp["msg"] = rnd.choice(["acked writes stay", "", "é is fine", 7, ["l"], {"b": 1, "a": 2.5}])
        if rnd.random() < 0.05:
            sp["history"] = rnd.choice([None, 1, "nohist"])
        if rnd.random() < 0.03:
            sp["id"] = rnd.choice([1, 2.0, True, "x y"])
        specs.append(sp)
        hists.append((name, lines))
    # interleave the histories' records (each keeps its own order)
    recs = []
    idx = [0] * len(hists)
    counter = 0
    while any(idx[i] < len(hists[i][1]) for i in range(len(hists))):
        i = rnd.choice([j for j in range(len(hists)) if idx[j] < len(hists[j][1])])
        p, ty, f, args = hists[i][1][idx[i]]
        idx[i] += 1
        recs.append(emit(rnd, recs, hists[i][0], p, ty, f, args, counter))
        counter += 1
        if rnd.random() < 0.02:
            recs.append(rnd.choice(JUNK))
    decl = ["HYDO-CHECK " + json.dumps(sp) for sp in specs]
    if rnd.random() < 0.05:
        decl.append(decl[0])                      # a repeated declaration (the first wins)
    pre = decl + ["some boot line"]
    if rnd.random() < 0.01:
        pre.append(rnd.choice(CRASH))             # inputs the reference does not survive
    finals = []
    if rnd.random() < 0.15:
        sp = rnd.choice(specs)
        finals.append("HYDO-CHECK-FINAL " + json.dumps({"id": sp["id"], "rc": rnd.choice([0, 2, "3", 1.5, None, "x"])}))
    cut = rnd.randrange(len(recs) + 1) if rnd.random() < 0.3 else len(recs)
    work = "\n".join(pre + recs[:cut]) + "\n"
    srcs = [["one", work]]
    if cut < len(recs) or rnd.random() < 0.1:
        srcs.append(["one.recovery", "\n".join(recs[cut:] + finals) + "\n"])
    else:
        srcs[0][1] += "\n".join(finals) + ("\n" if finals else "")
    verdict = rnd.choice([None, "HYDO-ORACLE: OK invariants=0 held ()", "HYDO-ORACLE: OK invariants=1 held (a: b)",
                          "HYDO-ORACLE: VIOLATION [g] guest first", "HYDO-ORACLE: ERROR boom",
                          "  HYDO-ORACLE: OK invariants=0 held ()  ", ""])
    budget = rnd.choice([H.DEFAULT_BUDGET] * 8 + [5, 50, 500, 5000, -1])
    given = None
    if rnd.random() < 0.05:
        given = specs[:]
    return {"kind": "checks", "srcs": srcs, "verdict": verdict, "budget": budget, "specs": given}


def gen_large(rnd):
    """One long history (the search's worst cases): a register or list-append history of
    500-3000 operations by 5-24 processes on 1-3 keys, violating in about half."""
    name = "big"
    nproc = rnd.choice([5, 10, 16, 24])
    keys = ["x", "y", "z"][:rnd.choice([1, 1, 2, 3])]
    size = rnd.choice([500, 1000, 2000, 3000])
    if rnd.random() < 0.6:
        cas = rnd.random() < 0.3
        lines = gen_register(rnd, name, keys, nproc, size, cas, rnd.random() < 0.5, False)
        if rnd.random() < 0.5:
            lines = mutate_register(rnd, lines)
        sp = {"id": "big.lin", "type": "linearizable", "history": name, "model": "cas" if cas else "register"}
    else:
        bug = rnd.choice([None, "si", "step", "lossy"])
        lines = gen_lists(rnd, name, keys, nproc, size, rnd.random() < 0.7, bug)
        sp = {"id": "big.la", "type": "list-append", "history": name, "consistency": rnd.choice(H.LEVELS)}
    recs = [emit(rnd, None, name, p, ty, f, args, i) for i, (p, ty, f, args) in enumerate(lines)]
    text = "HYDO-CHECK " + json.dumps(sp) + "\n" + "\n".join(recs) + "\n"
    return {"kind": "checks", "srcs": [["one", text]], "verdict": "HYDO-ORACLE: OK invariants=0 held ()",
            "budget": H.DEFAULT_BUDGET, "specs": None}


def gen(path, n, seed, large=False):
    rnd = random.Random(seed)
    with open(path, "w") as f:
        for i in range(n):
            if large:
                f.write(dumps(gen_large(rnd)) + "\n")
                continue
            f.write(dumps(gen_case(rnd)) + "\n")
            if i % 50 == 0:
                toks = ["".join(rnd.choice(["%", "4", "1", "e", "E", "f", "%C3", "%A9", "a", "@", "%2", "é"])
                                for _ in range(rnd.randrange(0, 8))) for _ in range(30)]
                f.write(dumps({"kind": "pdec", "toks": toks}) + "\n")
                strs = ["".join(rnd.choice(["a", " ", "%", "@", ",", "[", ":", "\t", "\x01", "é", "　", "nil", "\x1d"])
                                for _ in range(rnd.randrange(0, 6))) for _ in range(30)]
                f.write(dumps({"kind": "penc", "strs": strs, "extra": rnd.choice(["", ",", ",[]", ":"])}) + "\n")
                b = bytes(rnd.choice([0x41, 0x0a, 0x0d, 0xc3, 0xa9, 0xe2, 0x82, 0xac, 0xf0, 0x9f, 0x80, 0xff, 0xed, 0xa0, 0xc0])
                          for _ in range(rnd.randrange(0, 40)))
                f.write(dumps({"kind": "decode", "hex": b.hex()}) + "\n")


def main(argv):
    if not argv or argv[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    cmd = argv[0]
    if cmd == "gen":
        n, seed = 1000, 1
        if "--n" in argv:
            n = int(argv[argv.index("--n") + 1])
        if "--seed" in argv:
            seed = int(argv[argv.index("--seed") + 1])
        gen(argv[1], n, seed, "--large" in argv)
    elif cmd == "units":
        units(argv[1])
    elif cmd == "consoles":
        consoles(argv[1], argv[2:])
    elif cmd == "py":
        jobs = int(argv[argv.index("--jobs") + 1]) if "--jobs" in argv else 1
        times = argv[argv.index("--times") + 1] if "--times" in argv else None
        run_py(argv[1], argv[2], jobs, times)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
