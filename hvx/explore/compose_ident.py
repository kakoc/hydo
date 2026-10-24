# Test composer identity check. usage: python3 compose_ident.py <reference explore.py> <new explore.py>
# (1) no manifest, and a manifest without a test plan (examples/redis): the new explore.py gives
#     byte-identical schedule strings, classes and shrink candidates to the reference, for every
#     generator with the other random streams on or off;
# (2) on a plan with test commands (examples/redis-composed): every schedule carries W=, and with W
#     removed it equals the reference's schedule on the same manifest without the test plan
#     (W comes from its own random stream); --no-compose gives exactly those strings; W values
#     differ between seeds; strings round-trip; the W shrink candidates render and parse.
import importlib.util, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    sys.path.insert(0, os.path.dirname(path)); spec.loader.exec_module(m); sys.path.pop(0)
    return m


A = load(sys.argv[1], "ex_ref"); B = load(sys.argv[2], "ex_new")
ONB = os.path.join(os.path.dirname(HERE), "onboard", "examples")
PLAIN = os.path.join(ONB, "redis", "hydo.yaml")
COMPOSED = os.path.join(ONB, "redis-composed", "hydo", "hydo.yaml")


def space(M, manifest):
    import manifest_space
    for k, v in (("SVCS", ["mysql", "kafka", "redis"]), ("NORDERS", 30), ("SPACE", None)):
        M.__dict__[k] = v
    if manifest:
        sp = manifest_space.load(manifest)
        sp.apply(M.__dict__)


def gen(M, strat, opts, seeds=range(1, 401)):
    M.TIMING, M.NEW_FAULTS, M.CONCURRENCY, M.BUGGIFY = opts
    g = M.GENERATORS[strat]
    out = []
    for sd in seeds:
        s = g(sd)
        if strat not in ("swarm-pos", "process"):
            s = M.add_new_faults(s, sd, strat == "swarm")
        s = M.add_buggify(M.add_concurrency(M.add_timing(s, sd), sd), sd)
        if hasattr(M, "add_compose"):
            s = M.add_compose(s, sd)
        out.append(M.render(s))
    return out


def main():
    n = 0
    combos = [(t, nf, c, b) for t in (False, True) for nf in (False, True) for c in (False, True) for b in (False, True)]
    for manifest in (None, PLAIN):
        space(A, manifest); space(B, manifest)
        for strat in sorted(A.GENERATORS):
            for opts in combos[::3]:
                a, b = gen(A, strat, opts), gen(B, strat, opts)
                assert a == b, (manifest, strat, opts)
                ka = [[A.render(c) for c in A.candidates(A.parse(x))] for x in a[:60]]
                kb = [[B.render(c) for c in B.candidates(B.parse(x))] for x in b[:60]]
                assert ka == kb, (manifest, strat, opts)
                assert [A.finding_class(A.parse(x)) for x in a] == [B.finding_class(B.parse(x)) for x in b]
                n += len(a)
    print("(1) identical: %d schedule strings without a manifest and on examples/redis (no test plan)" % n)
    space(A, PLAIN); space(B, COMPOSED)
    assert B.SPACE.composed()
    m, ws = 0, set()
    for strat in sorted(A.GENERATORS):
        for opts in combos[::5]:
            a, b = gen(A, strat, opts), gen(B, strat, opts)
            B.COMPOSE = False
            b0 = gen(B, strat, opts)
            B.COMPOSE = True
            for x, y, y0 in zip(a, b, b0):
                assert " W=" in y and " W=" not in y0, y
                assert y.split(" W=")[0] == x == y0, (x, y)
                assert B.render(B.parse(y)) == y
                ws.add(y.split(" W=")[1].split()[0])
                for c in B.candidates(B.parse(y))[:40]:
                    B.render(c)
                m += 1
    assert len(ws) > m // 20, len(ws)
    print("(2) %d schedules on examples/redis-composed: W= on every one, the rest identical to the plan without "
          "a test section, %d distinct W values; round trip and shrink candidates OK" % (m, len(ws)))


main()
