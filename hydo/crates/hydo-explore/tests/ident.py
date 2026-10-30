#!/usr/bin/env python3
"""The reference side of tests/identity.rs: explore.py itself (and manifest_space.py, grpc_faults.py,
hvm_backend.py, hydo_composer.py) answering a deterministic stream of questions, one per line:

    G <json>                       a group: the fault space (manifest or the stack) and the engine
    F <json>                       the option flags for the following lines
    S <strategy> <seed> <durable>\t<schedule>                   main()'s composed, rendered schedule
    P <tjk>\t<schedule>\t<render(parse)>\t<class cut>\t<class any>\t<candidates...>
    W <sweep> <timing>\t<label>\x1f<schedule>\t...              every point of a sweep
    Z\t<schedule>\t<stable_w>\t<describe>\t<contents>\t<key>\t<fits>\t<candidates of the resolved form...>
    X <svc> <dir> <peer> <step> <n> <match> <dur>\t<schedule>   a freeze point rendered from its dict
    N <nmax>\t<fc_n for below(1000) = 0..999>
    E\t<message>                   the group could not be set up (explore.py stops)

The Rust side reads the questions, answers them with hydo-explore and compares every answer.

One deliberate difference is applied here to the reference: on --backend hvm a schedule never
carries cf= (hvmx cannot apply a power cut at the k-th FLUSH; explore.py printed it, so Mac
reports claimed cuts that never happened). Everything else is explore.py as it is.

usage: python3 ident.py [--quick]
"""
import glob
import hashlib
import importlib.util
import json
import os
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
TOP = os.path.abspath(os.path.join(HERE, "..", "..", "..", ".."))
EXP = os.path.join(TOP, "hydo", "explore")
ONB = os.path.join(TOP, "hydo", "onboard")
sys.path.insert(0, EXP)
sys.path.insert(0, ONB)
import grpc_faults          # noqa: E402
import hydo_composer        # noqa: E402
import manifest_space       # noqa: E402

QUICK = "--quick" in sys.argv
SEEDS = 40 if QUICK else 300
CAND_EVERY = 10
US = "\x1f"
out = sys.stdout
_n = [0]


def fresh():
    _n[0] += 1
    spec = importlib.util.spec_from_file_location("ex_%d" % _n[0], os.path.join(EXP, "explore.py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    orig = m.render

    def render(s):          # the deliberate fix: no cf= on hvm (see the docstring)
        if m.BACKEND == "hvm" and "cf" in s:
            s = dict(s)
            s.pop("cf")
        return orig(s)
    m.render = render
    return m


def mix(x):
    x = (x + 0x9E3779B97F4A7C15) & (2 ** 64 - 1)
    x = ((x ^ (x >> 30)) * 0xBF58476D1CE4E5B9) & (2 ** 64 - 1)
    x = ((x ^ (x >> 27)) * 0x94D049BB133111EB) & (2 ** 64 - 1)
    return x ^ (x >> 31)


FLAGS = ["TIMING", "NEW_FAULTS", "DURABLE", "MORE_FAULTS", "PREEMPT", "TURNS", "BURSTS", "BUGGIFY",
         "CONCURRENCY", "FREEZE_CALL", "PROTOCOL_FAULTS", "GRPC", "COMPOSE", "KNOB_ANY"]


def flags_for(sd, salt):
    if sd == 1:
        return {f: False for f in FLAGS}
    if sd == 2:
        return {f: True for f in FLAGS}
    h = mix(sd * 0x1000193 ^ salt)
    return {f: bool((h >> i) & 1) for i, f in enumerate(FLAGS)}


def setflags(E, fl):
    E.TIMING, E.NEW_FAULTS, E.MORE_FAULTS, E.PREEMPT = fl["TIMING"], fl["NEW_FAULTS"], fl["MORE_FAULTS"], fl["PREEMPT"]
    E.TURNS, E.BURSTS, E.BUGGIFY, E.CONCURRENCY = fl["TURNS"], fl["BURSTS"], fl["BUGGIFY"], fl["CONCURRENCY"]
    E.FREEZE_CALL, E.PROTOCOL_FAULTS, E.COMPOSE = fl["FREEZE_CALL"], fl["PROTOCOL_FAULTS"], fl["COMPOSE"]
    E.KNOB_RULE = "any" if fl["KNOB_ANY"] else "cut"
    grpc_faults.ON = fl["GRPC"]
    out.write("F %s\n" % json.dumps(fl, sort_keys=True))


def compose(E, strat, sd, durable):
    s = E.GENERATORS[strat](sd)
    if durable:
        s = E.durable_only(s)
    if strat not in ("swarm-pos", "process"):
        s = E.add_new_faults(s, sd, strat == "swarm")
    s = E.add_grpc_faults(E.add_protocol_faults(E.add_freeze_call(E.add_more_faults(s, sd, strat), sd), sd), sd)
    return E.add_compose(E.add_turns(E.add_buggify(E.add_preempt(E.add_concurrency(E.add_timing(s, sd), sd), sd), sd), sd), sd)


def p_line(E, text, tjk):
    s = E.parse(text)
    E.KNOB_RULE, ca = "cut", None
    cc = E.finding_class(s)
    E.KNOB_RULE = "any"
    ca = E.finding_class(s)
    cands = [E.render(c) for c in E.candidates(s, tjk)]
    out.write("P %s\t%s\t%s\t%s\t%s\t%s\n" % ("-" if tjk is None else "%d-%d" % tuple(tjk), text, E.render(s), cc, ca,
                                              US.join(cands)))
    return cands


def composer_forms(E, text):
    meta = E.SPACE.compose
    w = hydo_composer.w_of(text)
    if not w:
        return
    try:
        r = hydo_composer.stable_w(w, meta)
        desc = hydo_composer.describe(w, meta)
        cont = json.dumps(hydo_composer.contents_of(r, meta), sort_keys=True)
    except ValueError:
        return
    rc = hydo_composer.candidates(r, meta)[:8]
    st = hydo_composer.with_w(text, r)
    out.write("Z\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" % (text, r, desc, cont, hydo_composer.key_of(r),
                                                    "1" if hydo_composer.fits(st) else "0", US.join(rc)))


def group(E, cfg, strats):
    out.write("G %s\n" % json.dumps(cfg, sort_keys=True))
    E.BACKEND = cfg["backend"]
    E.CF_RANGE = (1, 300)
    salt = int(hashlib.sha256(cfg["label"].encode()).hexdigest()[:15], 16)
    for strat in strats:
        for sd in range(1, SEEDS + 1):
            fl = flags_for(sd, salt ^ zlib.crc32(strat.encode()))
            setflags(E, fl)
            text = E.render(compose(E, strat, sd, fl["DURABLE"]))
            out.write("S %s %d %d\t%s\n" % (strat, sd, 1 if fl["DURABLE"] else 0, text))
            if sd % CAND_EVERY == 0 or sd <= 3:
                tjk = (2 + sd % 7, 40 + (sd * 13) % 300)
                for tk in (None, tjk):
                    for c in p_line(E, text, tk):
                        if "tjx=" in c:          # one step further: single tjx entries
                            p_line(E, c, tk)
                            break
            if E.SPACE is not None and E.SPACE.composed() and (sd % 5 == 0 or sd <= 3):
                composer_forms(E, text)
    for timing in (False, True):
        fl = {f: False for f in FLAGS}
        fl["TIMING"], fl["COMPOSE"] = timing, True
        setflags(E, fl)
        for sw in ("kill", "cut", "net", "killcut", "pause", "clock", "oneway", "frontkill", "tail", "depth1",
                   "depth1-old", "protocol", "bogus"):
            if E.SPACE is not None:
                try:
                    E.SPACE.check_sweep(sw)
                except SystemExit as e:
                    out.write("W %s %d\t!%s\n" % (sw, timing, e))
                    continue
            try:
                points = E.sweep(sw)
            except SystemExit as e:
                out.write("W %s %d\t!%s\n" % (sw, timing, e))
                continue
            items = []
            for i, (lab, sch) in enumerate(points, 1):
                items.append(lab + US + E.render(E.add_compose(E.add_turns(E.add_buggify(E.add_preempt(
                    E.add_concurrency(E.add_timing(dict(sch), i), i), i), i), i), i)))
            out.write("W %s %d\t%s\n" % (sw, timing, "\t".join(items)))
    for svc, peer in E.FC_PAIRS:
        for d in "<>":
            for match in [None] + [m for m, _ in E.FC_MATCH]:
                for st in range(0, E.NORDERS, 7):
                    for n in (0, 1, 5, 64):
                        t = E.render({"disk": "none", "dseed": 0, "F": (svc, d, peer, st, n, match, 1000)})
                        out.write("X %s %s %s %d %d %s %d\t%s\n" % (svc, d, peer, st, n, match or "-", 1000, t))


def fc_table(E):
    import math
    for nmax in (64, 32, 16, 100, 7):
        vals = []
        for k in range(1000):
            e = k / 1000.0 * math.log(nmax)
            vals.append(str(max(1, min(nmax, int(math.exp(e))))))
        out.write("N %d\t%s\n" % (nmax, " ".join(vals)))


def manifests():
    res = []
    for pat in ("examples/*/hydo*.yaml", "examples/*/hydo/hydo.yaml", "examples-mac/*/hydo*.yaml"):
        res += sorted(glob.glob(os.path.join(ONB, pat)))
    return [os.path.relpath(p, TOP) for p in res]


def main():
    strats = ["random", "swarm", "swarm-pos", "process"]
    plan = [(None, "hvx"), (None, "hvm")] + [(m, b) for m in manifests() for b in ("hvx", "hvm")]
    if QUICK:
        plan = plan[:4] + plan[4::5]
    fc_table(None)
    for manifest, backend in plan:
        E = fresh()
        E.SPACE = None
        if manifest:
            try:
                sp = manifest_space.load(manifest=os.path.join(TOP, manifest))
            except (SystemExit, Exception) as e:      # noqa: B902 (hydo-build's ManifestError too)
                out.write("G %s\n" % json.dumps({"label": manifest, "manifest": manifest, "backend": backend,
                                                  "disk": False, "tc": False}, sort_keys=True))
                out.write("E\t%s\n" % e)
                continue
            sp.apply(vars(E))
            grpc_faults.TARGETS = grpc_faults.targets_of(sp.m)
        else:
            grpc_faults.TARGETS = []
        st = strats if E.NORDERS >= 15 else ["random", "swarm", "process"]
        if backend == "hvm":
            for disk, tc in ((True, True), (False, False)):
                E.hvm_backend.HAS_DISK, E.hvm_backend.HAS_TC = disk, tc
                group(E, {"label": "%s/hvm/%s%s" % (manifest, disk, tc), "manifest": manifest, "backend": backend,
                          "disk": disk, "tc": tc}, st)
        else:
            group(E, {"label": "%s/hvx" % manifest, "manifest": manifest, "backend": backend, "disk": False,
                      "tc": False}, st)


main()
