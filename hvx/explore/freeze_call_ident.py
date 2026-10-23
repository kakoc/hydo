# M70 identity check for --freeze-call. usage:
#   python3 freeze_call_ident.py <reference explore.py> <new explore.py> <hydo.yaml with freeze> <same without freeze>
# (1) no manifest: without --freeze-call the new explore.py gives byte-identical schedule strings,
#     classes and shrink candidates for all four generators, composed as explore.py main does, with
#     every other random stream on or off (new faults, more faults, timing, concurrency, preempt,
#     buggify, durable-only);
# (2) the same on the manifest: the new explorer on the manifest WITH `freeze` in faults: and the
#     reference on the same manifest WITHOUT it give identical strings when --freeze-call is off;
# (3) with --freeze-call every field other than F equals the schedule without it, the strings
#     round-trip, F names a pair manifest_space.py lists, and the shrink candidates render.
import importlib.util, itertools, sys


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec); sys.path.insert(0, path.rsplit("/", 1)[0]); spec.loader.exec_module(m)
    sys.path.pop(0)
    return m


A = load(sys.argv[1], "ex_ref"); B = load(sys.argv[2], "ex_new")
MS_A = load(sys.argv[1].rsplit("/", 1)[0] + "/manifest_space.py", "ms_ref")
MS_B = load(sys.argv[2].rsplit("/", 1)[0] + "/manifest_space.py", "ms_new")
GLOBALS = ("SVCS", "NET_SVCS", "PAUSE_SVCS", "NORDERS", "KNOBS", "KNOB_TOKENS", "SPACE", "FC_PAIRS", "FC_MATCH")
SAVED = {M: {k: getattr(M, k) for k in GLOBALS if hasattr(M, k)} for M in (A, B)}


def reset(M):
    for k, v in SAVED[M].items():
        setattr(M, k, v)


def compose(M, strat, sd, dur, mf, fc):
    s = M.GENERATORS[strat](sd)
    if dur: s = M.durable_only(s)
    if strat not in ("swarm-pos", "process"): s = M.add_new_faults(s, sd, strat == "swarm")
    s = M.add_more_faults(s, sd, strat)
    if fc: s = M.add_freeze_call(s, sd)
    return M.render(M.add_buggify(M.add_preempt(M.add_concurrency(M.add_timing(s, sd), sd), sd), sd))


def run(label, strats=("random", "swarm", "swarm-pos", "process"), seeds=range(1, 201)):
    n = 0
    for strat in strats:
        for flags in itertools.product((False, True), repeat=7):
            nf, tm, cc, pe, bg, dur, mf = flags
            for M in (A, B):
                M.NEW_FAULTS, M.TIMING, M.CONCURRENCY, M.PREEMPT, M.BUGGIFY, M.MORE_FAULTS = nf, tm, cc, pe, bg, mf
                M.CF_RANGE = (1, 300)
            B.FREEZE_CALL = False
            a = [compose(A, strat, sd, dur, mf, False) for sd in seeds]
            b = [compose(B, strat, sd, dur, mf, False) for sd in seeds]
            assert a == b, (label, strat, flags)
            for rule in ("cut", "any"):
                A.KNOB_RULE = B.KNOB_RULE = rule
                assert [A.finding_class(A.parse(x)) for x in a] == [B.finding_class(B.parse(x)) for x in b], (label, strat, flags, rule)
            A.KNOB_RULE = B.KNOB_RULE = "cut"
            assert [[A.render(c) for c in A.candidates(A.parse(x))] for x in a[:40]] == \
                   [[B.render(c) for c in B.candidates(B.parse(x))] for x in b[:40]], (label, strat, flags)
            n += len(a)
            if B.FC_PAIRS:                                  # (3) with --freeze-call
                B.FREEZE_CALL = True
                c = [compose(B, strat, sd, dur, mf, True) for sd in seeds]
                nfc = 0
                for x, y in zip(b, c):
                    p, q = B.parse(x), B.parse(y)
                    assert B.render(q) == y, y
                    assert set(p) | {"F"} >= set(q) and all(p.get(k) == q.get(k) for k in set(p) | set(q) if k != "F"), (x, y)
                    if "F" in q:
                        nfc += 1
                        assert (q["F"][0], q["F"][2]) in B.FC_PAIRS and 1 <= q["F"][4] <= B.FC_NMAX, y
                        mt = dict((x, dr) for x, dr in B.FC_MATCH)
                        assert q["F"][5] is None or (q["F"][5] in mt and mt[q["F"][5]] in (None, q["F"][1])), y
                    [B.render(cand) for cand in B.candidates(q)]
                assert 0 < nfc < len(c), (label, strat, nfc)
                B.FREEZE_CALL = False
    return n


n1 = run("no manifest")
reset(A); reset(B)
MS_A.load(manifest=sys.argv[4]).apply(vars(A))
MS_B.load(manifest=sys.argv[3]).apply(vars(B))
assert B.FC_PAIRS, "the manifest with freeze gives no pairs"
# swarm-pos needs at least 15 steps (pos_start's "late" range is empty below that, in main too)
n2 = run("manifest", ("random", "swarm", "process"))
print("identical without --freeze-call: %d schedule strings without a manifest, %d on %s "
      "(4 generators [3 on the manifest] x 128 stream combinations; classes under both knob rules; shrink candidates); "
      "with --freeze-call only F= differs; pairs %s" % (n1, n2, sys.argv[3], B.FC_PAIRS))
