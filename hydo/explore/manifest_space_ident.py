# Byte-identity check: explore.py with the manifest hook vs a reference explore.py (e.g. main's),
# no --manifest. usage: python3 manifest_space_ident.py <reference explore.py> <new explore.py>
import importlib.util, sys
def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec); sys.path.insert(0, path.rsplit("/",1)[0]); spec.loader.exec_module(m); sys.path.pop(0); return m
A = load(sys.argv[1], "ex_main"); B = load(sys.argv[2], "ex_new")
def gen(M, strat, timing, nf, dur):
    M.TIMING, M.NEW_FAULTS = timing, nf
    g = M.generate_swarm if strat == "swarm" else M.generate
    d = M.durable_only if dur else (lambda s: s)
    out = []
    for sd in range(1, 1001):
        txt = M.render(M.add_timing(M.add_new_faults(d(g(sd)), sd, strat == "swarm"), sd))
        out.append(txt)
        assert M.render(M.parse(txt)) == txt, txt     # round trip
        M.finding_class(M.parse(txt)); M.candidates(M.parse(txt))
    return out
n = 0
for strat in ("random", "swarm"):
    for timing in (False, True):
        for nf in (False, True):
            for dur in (False, True):
                a, b = gen(A, strat, timing, nf, dur), gen(B, strat, timing, nf, dur)
                assert a == b, (strat, timing, nf, dur)
                ca = [A.finding_class(A.parse(x)) for x in a]; cb = [B.finding_class(B.parse(x)) for x in b]
                assert ca == cb
                ka = [[A.render(c) for c in A.candidates(A.parse(x))] for x in a[:200]]
                kb = [[B.render(c) for c in B.candidates(B.parse(x))] for x in b[:200]]
                assert ka == kb
                n += len(a)
for sw in ("kill", "cut", "net", "killcut", "pause", "clock", "oneway"):
    for timing in (False, True):
        A.TIMING = B.TIMING = timing
        pa = [A.render(A.add_timing(s, i)) for i, (_, s) in enumerate(A.sweep(sw), 1)]
        pb = [B.render(B.add_timing(s, i)) for i, (_, s) in enumerate(B.sweep(sw), 1)]
        assert pa == pb, sw; n += len(pa)
print("identical:", n, "schedule strings (+ parse/render round trip, classes, shrink candidates)")
