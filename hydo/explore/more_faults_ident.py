# M62 identity check for --more-faults. usage: python3 more_faults_ident.py <reference explore.py> <new explore.py>
# (1) without --more-faults the new explore.py gives byte-identical schedule strings, classes (both
#     --knob-rule values) and shrink candidates for all four generators, composed as explore.py main
#     does, with every other stream (new faults, timing, concurrency, preempt, buggify, durable-only);
# (2) with --more-faults every field other than X / cf / disk / C equals the schedule without it,
#     the strings round-trip, and cf= only appears with disk=cut and no C=.
import importlib.util, itertools, sys
def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec); sys.path.insert(0, path.rsplit("/", 1)[0]); spec.loader.exec_module(m); sys.path.pop(0); return m
A = load(sys.argv[1], "ex_ref"); B = load(sys.argv[2], "ex_new")
def compose(M, strat, sd, dur, mf):
    s = M.GENERATORS[strat](sd)
    if dur: s = M.durable_only(s)
    if strat not in ("swarm-pos", "process"): s = M.add_new_faults(s, sd, strat == "swarm")
    if mf: s = M.add_more_faults(s, sd, strat)
    return M.render(M.add_buggify(M.add_preempt(M.add_concurrency(M.add_timing(s, sd), sd), sd), sd))
n = 0
for strat in ("random", "swarm", "swarm-pos", "process"):
    for flags in itertools.product((False, True), repeat=6):
        nf, tm, cc, pe, bg, dur = flags
        for M in (A, B):
            M.NEW_FAULTS, M.TIMING, M.CONCURRENCY, M.PREEMPT, M.BUGGIFY = nf, tm, cc, pe, bg
        B.MORE_FAULTS = False
        seeds = range(1, 301)
        a = [compose(A, strat, sd, dur, False) for sd in seeds]
        b = [compose(B, strat, sd, dur, False) for sd in seeds]
        assert a == b, (strat, flags)
        for rule in ("cut", "any"):
            A.KNOB_RULE = B.KNOB_RULE = rule
            assert [A.finding_class(A.parse(x)) for x in a] == [B.finding_class(B.parse(x)) for x in b], (strat, flags, rule)
        A.KNOB_RULE = B.KNOB_RULE = "cut"
        assert [[A.render(c) for c in A.candidates(A.parse(x))] for x in a[:60]] == \
               [[B.render(c) for c in B.candidates(B.parse(x))] for x in b[:60]], (strat, flags)
        n += len(a)
        # (2) with --more-faults
        B.MORE_FAULTS, B.CF_RANGE = True, (1, 300)
        c = [compose(B, strat, sd, dur, True) for sd in seeds]
        for x, y in zip(b, c):
            p, q = B.parse(x), B.parse(y)
            assert B.render(q) == y, y
            assert "cf" not in q or (q["disk"] == "cut" and "C" not in q), y
            for k in set(p) | set(q):
                if k not in ("X", "cf", "disk", "C"):
                    assert p.get(k) == q.get(k), (x, y, k)
            if "cf" not in q:
                assert (p["disk"], p.get("C")) == (q["disk"], q.get("C")), (x, y)
            [B.render(cand) for cand in B.candidates(q)]
        B.MORE_FAULTS = False
print("identical without --more-faults:", n, "schedule strings (4 generators x 64 stream combinations; classes under both knob rules; shrink candidates)")
