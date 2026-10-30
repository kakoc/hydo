#!/usr/bin/env python3
"""Test-only: the Python reference (hydo/onboard/hydo_composer.py) for hydo-composer's
equivalence test (tests/equivalence.rs).

    py_ref.py --work DIR [--n 3000]

Prints one JSON line per case: {"op": <name>, "args": {...}, "out": <what Python gives>} with
"out" = {"ok": <value>} or {"err": "ValueError" | "crash", "msg": <text>}. The Rust test runs the
same op on the same args and compares. The cases:

  1. the M98 corpora of hydo/onboard/tests/test_composer.py (the same random stream, seed 7):
     --n seed-form W values over 80 command sets (render, kept), and the resolved compositions
     replayed on a changed, shuffled command list (514 replays, 86 with a removed command when
     n = 3000), with their candidates;
  2. every plan and hydo/ folder in the repository (every tracked *.yaml, *.yml, *.json that
     hydo-build expands into a plan, and every hydo/ folder without a hydo.yaml, with a minimal
     manifest): expand, shell_lines, test_from_meta, stage, 40 drawn W values each with their
     resolved form, key, entries, description, candidates and contents, and regressions in every
     form (seed, index form with the same and another list, resolved, saved @key, edited key,
     missing compose, removed command, late round, changed content) with regression_status,
     regression_warning and saved_regression_text;
  3. the M98 folder variants of redis-composed under DIR (two commands added, then the transfer
     removed, then its file edited), as test_composer_ids.py builds them.
"""
import argparse, json, os, random, shutil, subprocess, sys, importlib.machinery, importlib.util

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.normpath(os.path.join(HERE, "..", "..", "..", ".."))
ONB = os.path.join(TOP, "hydo", "onboard")
sys.path.insert(0, ONB)
import hydo_composer as hc  # noqa: E402


def _load_build():
    loader = importlib.machinery.SourceFileLoader("hydo_build", os.path.join(ONB, "hydo-build"))
    spec = importlib.util.spec_from_loader("hydo_build", loader)
    m = importlib.util.module_from_spec(spec)
    loader.exec_module(m)
    return m


hb = _load_build()
E = hb.ManifestError
N_OPS = [0]


METAS = {}


def ref(args):
    """A plan meta is printed once ({"op": "meta", "args": {"id", "meta"}}) and then named by
    {"$meta": id} in the args of every case that uses it."""
    m = args.get("meta")
    if isinstance(m, dict):
        k = json.dumps(m, sort_keys=True)
        if k not in METAS:
            METAS[k] = len(METAS)
            print(json.dumps({"op": "meta", "args": {"id": METAS[k], "meta": m}, "out": {"ok": None}}))
        args = dict(args, meta={"$meta": METAS[k]})
    return args


def emit(op, args, fn):
    try:
        out = {"ok": fn()}
    except (ValueError, E) as e:
        out = {"err": "ValueError", "msg": str(e)}
    except Exception as e:      # the reference would stop with a traceback here
        out = {"err": "crash", "msg": "%s: %s" % (type(e).__name__, e)}
    print(json.dumps({"op": op, "args": ref(args), "out": out}, sort_keys=False))
    N_OPS[0] += 1
    return out.get("ok")


def tup(es):
    return [list(e) for e in es]


# ---------------------------------------------------------------- 1. the M98 corpora
def cmd_sets():
    k = lambda kind, n: [{"kind": kind, "id": "%s_%d" % (kind, i)} for i in range(n)]
    sets = []
    for npar in (0, 1, 3):
        for nser in (0, 1, 2):
            for nsg in (0, 1, 2):
                for nany in (0, 1, 2):
                    if npar + nser + nsg + nany == 0:
                        continue
                    cs = k("parallel_driver", npar) + k("serial_driver", nser) + k("singleton_driver", nsg) + \
                         k("anytime", nany) + k("first", 1) + k("eventually", 1)
                    cs.sort(key=lambda c: c["id"])
                    sets.append(cs)
    return sets


def corpora(n):
    """The random stream of test_composer.py main(), case for case."""
    rnd = random.Random(7)
    sets = cmd_sets()
    params = dict(hc.DEFAULTS)
    nw = 0
    for i in range(n):
        cmds = sets[i % len(sets)]
        steps = rnd.choice([1, 4, 20, 57])
        d = {"seed": rnd.getrandbits(32) if i % 5 else i, "p": 1 + rnd.randrange(4), "q": 1 + rnd.randrange(3),
             "r": 100, "n": rnd.choice([0, 0, 3, 10]), "t": 0, "x": set(), "o": set()}
        full = hc.plan(d["seed"], d["q"], steps, cmds)
        if full and i % 3 == 0:
            d["x"] = set(rnd.sample(range(len(full)), min(len(full), rnd.randrange(1, 6))))
        if i % 4 == 0:
            d["o"] = {rnd.randrange(len(cmds))}
        elif i % 4 == 1:
            d["o"] = set(rnd.sample([c["id"] for c in cmds], min(len(cmds), rnd.randrange(1, 3))))
        w = hc.render_w(d)
        meta = {"params": params, "commands": cmds, "steps": steps}
        emit("plan", {"seed": d["seed"], "q": d["q"], "steps": steps, "cmds": cmds},
             lambda: tup(hc.plan(d["seed"], d["q"], steps, cmds)))
        emit("render_w", {"w": w, "meta": meta}, lambda: hc.render_w(hc.parse_w(w, params)))
        emit("kept", {"w": w, "meta": meta, "steps": steps},
             lambda: tup(hc.kept(hc.parse_w(w, params), steps, cmds)))
        if i % 10 == 0:
            emit("candidates", {"w": w, "meta": meta, "steps": None}, lambda: hc.candidates(w, meta))
        nw += 1
    meta = {"params": params, "commands": sets[-1], "steps": 20}
    for seed in range(50):
        w = hc.render_w({"seed": seed * 7919, "p": 4, "q": 2, "r": 100, "n": 0, "t": 0, "x": set(), "o": set()})
        emit("candidates", {"w": w, "meta": meta, "steps": None}, lambda: hc.candidates(w, meta))
    nres = nerr = 0
    for i in range(max(200, n // 5)):
        cmds = sets[i % len(sets)]
        steps = rnd.choice([4, 20, 57])
        w = hc.render_w({"seed": rnd.getrandbits(32), "p": 1 + rnd.randrange(4), "q": 1 + rnd.randrange(3),
                         "r": 100, "n": rnd.choice([0, 3]), "t": rnd.choice([0, 0, 500]), "x": set(), "o": set()})
        meta = {"params": params, "commands": cmds, "steps": steps}
        if not hc.kept(hc.parse_w(w, params), steps, cmds):
            continue
        res = emit("stable_w", {"w": w, "meta": meta, "steps": None, "saved": None}, lambda: hc.stable_w(w, meta))
        emit("key_of", {"x": res}, lambda: hc.key_of(res))
        emit("render_resolved", {"v": res}, lambda: hc.render_resolved(hc.parse_resolved(res)))
        now = [dict(c) for c in cmds] + [{"kind": "parallel_driver", "id": "parallel_driver_new%d" % i},
                                          {"kind": "anytime", "id": "anytime_0new"}]
        drop = None
        if i % 7 == 3:
            drop = rnd.choice(sorted({cid for _, _, cid, _ in hc.parse_resolved(res)["entries"]}))
            now = [c for c in now if c["id"] != drop]
        now.sort(key=lambda c: c["id"])
        if i % 2:
            rnd.shuffle(now)
        meta2 = {"params": params, "commands": now, "steps": steps}
        out = emit("entries_of", {"w": res, "meta": meta2, "steps": None, "saved": None},
                   lambda: tup(hc.entries_of(res, meta2)))
        if drop:
            nerr += 1
            continue
        emit("candidates", {"w": res, "meta": meta2, "steps": None}, lambda: hc.candidates(res, meta2))
        emit("describe", {"w": res, "meta": meta2, "steps": None}, lambda: hc.describe(res, meta2))
        nres += 1
    return nw, nres, nerr


# ---------------------------------------------------------------- 2. plans and folders
def tracked(*pats):
    """The repository's files (git ls-files), or, in a copy without a git index, every file under
    the top directory except build outputs."""
    out = subprocess.run(["git", "ls-files"] + list(pats), cwd=TOP, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True).stdout.split()
    if out:
        return [os.path.join(TOP, p) for p in out]
    import fnmatch
    files = []
    for d, dirs, fs in os.walk(TOP):
        dirs[:] = sorted(x for x in dirs if x not in ("target", ".git", "node_modules"))
        for f in sorted(fs):
            if not pats or any(fnmatch.fnmatch(f, p) for p in pats):
                files.append(os.path.join(d, f))
    return files


def raw_manifest(path):
    text = open(path).read()
    m = json.loads(text) if path.endswith(".json") else hb.parse_yaml(text, os.path.basename(path))
    if not isinstance(m, dict):
        return None
    hb.apply_params(m, [])
    return m


def snapshot(R):
    out = {}
    for d, dirs, files in os.walk(R):
        dirs.sort()
        for n in sorted(dirs + files):
            p = os.path.join(d, n)
            rel = os.path.relpath(p, R)
            st = os.lstat(p)
            if os.path.islink(p):
                out[rel] = {"link": os.readlink(p)}
            elif os.path.isdir(p):
                out[rel] = {"dir": oct(st.st_mode & 0o7777)}
            else:
                out[rel] = {"mode": oct(st.st_mode & 0o7777), "text": open(p, "rb").read().decode("utf-8", "replace")}
    return out


def plan_cases(label, mdir, m, work, rnd):
    """Every op on one plan: m is the manifest before expand, mdir the directory of hydo.yaml."""
    m0 = json.loads(json.dumps(m))
    warns = []
    folder_box = {}

    def do_expand():
        folder = hc.scan_folder(mdir, E)
        folder_box["f"] = folder
        mm = json.loads(json.dumps(m0))
        meta = hc.expand(mm, E, folder, warn=warns.append)
        return {"m": mm, "meta": meta, "warnings": warns}
    out = emit("expand", {"m": m0, "dir": mdir}, do_expand)
    if not out or not out["meta"]:
        return 0
    meta = out["meta"]
    emit("shell_lines", {"meta": meta}, lambda: hc.shell_lines(meta))
    emit("test_from_meta", {"meta": meta}, lambda: hc.test_from_meta(meta))
    R = os.path.join(work, "stage-%d" % N_OPS[0])

    def do_stage():
        hc.stage(meta, R, ONB, folder_box["f"])
        return snapshot(R)
    emit("stage", {"meta": meta, "dir": mdir, "root": R}, do_stage)
    shutil.rmtree(R, ignore_errors=True)
    for r in meta["regressions"]:
        emit("regression_status", {"r": r, "meta": meta, "steps": None}, lambda: hc.regression_status(r, meta))
        emit("regression_warning", {"r": r, "meta": meta}, lambda: hc.regression_warning(r, meta))
    if not meta.get("composed"):
        return 1
    ids = [c["id"] for c in meta["commands"]]
    for k in range(40):
        seq = []

        def nxt():
            v = rnd.getrandbits(64)
            seq.append(v)
            return v
        w = hc.draw(nxt, meta)
        emit("draw", {"seq": seq, "meta": meta}, lambda: hc.draw(iter(seq).__next__, meta))
        if k % 3 == 1:      # what the shrinker writes: entries dropped, commands by id
            cs = hc.candidates(w, meta)
            if cs:
                w = cs[k % len(cs)]
        steps = None if k % 4 else rnd.choice([3, 20, 57])
        emit("describe", {"w": w, "meta": meta, "steps": steps}, lambda: hc.describe(w, meta, steps))
        emit("candidates", {"w": w, "meta": meta, "steps": steps}, lambda: hc.candidates(w, meta, steps))
        res = emit("stable_w", {"w": w, "meta": meta, "steps": steps, "saved": None},
                   lambda: hc.stable_w(w, meta, steps))
        emit("contents_of", {"w": w, "meta": meta, "steps": steps, "saved": None},
             lambda: hc.contents_of(w, meta, steps))
        try:
            hc.parse_resolved(res)
        except (TypeError, ValueError):
            continue        # nothing composed (an empty plan for these steps)
        key = hc.key_of(res)
        emit("key_of", {"x": res}, lambda: hc.key_of(res))
        saved = {key: res}
        for v in (res, "@" + key):
            emit("entries_of", {"w": v, "meta": meta, "steps": steps, "saved": saved},
                 lambda: tup(hc.entries_of(v, meta, steps, saved)))
            emit("describe", {"w": v, "meta": meta, "steps": steps}, lambda: hc.describe(v, meta, steps))
            emit("candidates", {"w": v, "meta": meta, "steps": steps}, lambda: hc.candidates(v, meta, steps))
        emit("contents_of", {"w": "@" + key, "meta": meta, "steps": None, "saved": saved},
             lambda: hc.contents_of("@" + key, meta, None, saved))
        sched = "disk=none dseed=%x | run K=redis@3+1 W=%s" % (k, w)
        emit("with_w", {"schedule": sched, "w": res}, lambda: [hc.with_w(sched, res), hc.fits(hc.with_w(sched, res))])
        emit("w_of", {"schedule": sched}, lambda: hc.w_of(sched))
        contents = hc.contents_of(w, meta, steps)
        other = list(ids)
        rnd.shuffle(other)
        regs = [
            {"name": "seed", "schedule": sched, "expect": "ok"},
            {"name": "same-list", "schedule": sched, "expect": "violation", "commands": list(ids)},
            {"name": "other-list", "schedule": sched, "expect": "ok", "commands": other + ["parallel_driver_zz"]},
            {"name": "bad-list", "schedule": sched, "expect": "ok", "commands": ["bogus"] + ids},
            {"name": "resolved", "schedule": hc.with_w(sched, res), "expect": "ok", "contents": contents},
            {"name": "saved", "schedule": hc.with_w(sched, "@" + key), "expect": "ok", "compose": res},
            {"name": "edited-key", "schedule": hc.with_w(sched, "@" + key[::-1]), "expect": "ok", "compose": res},
            {"name": "no-compose", "schedule": hc.with_w(sched, "@" + key), "expect": "ok"},
            {"name": "changed", "schedule": hc.with_w(sched, res), "expect": "ok",
             "contents": {c: "0" * 16 for c in contents}},
            {"name": "garbage", "schedule": hc.with_w(sched, ":p0,zz"), "expect": "ok"},
        ]
        used = sorted({cid for _, _, cid, _ in hc.parse_resolved(res)["entries"]})
        smaller = dict(meta, commands=[c for c in meta["commands"] if c["id"] != used[0]])
        late = dict(meta, steps=2)
        for r in regs:
            for mt, st in ((meta, steps), (smaller, None), (late, None)):
                emit("regression_status", {"r": r, "meta": mt, "steps": st}, lambda: hc.regression_status(r, mt, st))
            emit("regression_warning", {"r": r, "meta": meta}, lambda: hc.regression_warning(r, meta))
        # hydo save for this failure, in each of the report's shapes
        base = {"from_seed": k, "class": "violation", "signature": "x", "min_verdict": "VIOLATION [p] " + "x" * 300,
                "minimal": sched}
        if k % 2:
            base["composition"] = hc.describe(w, meta, steps) + " " * 400
        xs = [dict(base),
              dict(base, commands=list(ids)),
              dict(base, stable=res, stable_schedule=hc.with_w(sched, res), contents=contents),
              dict(base, stable=res, stable_schedule=hc.with_w(sched, "@" + key), contents=contents, stable_note="n"),
              dict(base, stable=res, stable_note="did not fail the same way", commands=list(ids), contents=contents),
              dict(base, stable=res, stable_note="x", commands=list(ids), minimal=hc.with_w(sched, res))]
        for x in xs:
            emit("saved_regression_text", {"x": x}, lambda: hc.saved_regression_text(x))
    return 1


def folder_variants(work):
    """test_composer_ids.py's redis-composed variants: v1 as is, v2 with two commands added (one
    sorts first), v3 = v2 without the transfer, v4 = v1 with the transfer's file edited."""
    src = os.path.join(ONB, "examples", "redis-composed", "hydo")
    out = []
    for v in ("v1", "v2", "v3", "v4"):
        d = os.path.join(work, "variants", v, "hydo")
        shutil.copytree(src, d, symlinks=False)
        if v in ("v2", "v3"):
            for name in ("anytime_aaa_ping", "parallel_driver_incr"):
                p = os.path.join(d, "workload", name)
                open(p, "w").write("#!/bin/sh\ntrue\n")
                os.chmod(p, 0o755)
        if v == "v3":
            os.remove(os.path.join(d, "workload", "parallel_driver_transfer"))
        if v == "v4":
            open(os.path.join(d, "workload", "parallel_driver_transfer"), "a").write("# edited\n")
        out.append(d)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--n", type=int, default=3000)
    a = ap.parse_args()
    nw, nres, nerr = corpora(a.n)
    rnd = random.Random(101)
    nplans = 0
    seen = set()
    cands = [p for p in tracked("*.yaml", "*.yml", "*.json")]
    for p in cands:
        try:
            m = raw_manifest(p)
        except Exception:
            continue
        if not m or not ("test" in m or any(os.path.isdir(os.path.join(os.path.dirname(p), s))
                                            for s in ("workload", "checks", "regressions"))):
            continue
        seen.add(os.path.dirname(p))
        nplans += plan_cases(os.path.relpath(p, TOP), os.path.dirname(p), m, a.work, rnd)
    folders = sorted({os.path.dirname(os.path.dirname(p)) for p in tracked()
                      if os.path.basename(os.path.dirname(p)) in ("workload", "checks", "regressions")})
    for d in folders + folder_variants(a.work):
        if d in seen:
            continue
        m = {"name": "t", "workload": {"steps": 20}, "test": {"parallel": 4, "per_round": 2, "round_ms": 100}}
        nplans += plan_cases(os.path.relpath(d, TOP), d, m, a.work, rnd)
    print(json.dumps({"op": "summary", "args": {}, "out": {"ok": {
        "w_values": nw, "resolved": nres, "removed": nerr, "plans": nplans, "ops": N_OPS[0]}}}))


if __name__ == "__main__":
    main()
