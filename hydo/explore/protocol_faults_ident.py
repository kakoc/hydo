# Identity check for --protocol-faults (H=, fake outside services). usage:
#   python3 protocol_faults_ident.py <reference explore.py> <new explore.py> <hydo.yaml with fakes> <same without fakes>
# (1) no manifest: without --protocol-faults the new explore.py gives byte-identical schedule
#     strings, classes (both knob rules) and shrink candidates for all four generators, composed as
#     explore.py main does, with every other random stream on or off (new faults, timing,
#     concurrency, preempt, buggify, durable-only, more faults, freeze-call);
# (2) the same on the manifest: the new explorer on the manifest WITH fakes: and the reference on
#     the same manifest WITHOUT them give identical strings when --protocol-faults is off;
# (3) with --protocol-faults every field other than H equals the schedule without it, the strings
#     round-trip, every H names a target manifest_space.py lists with a rule of that target's kinds,
#     and the shrink candidates render and stay parseable.
import importlib.util, itertools, sys


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec); sys.path.insert(0, path.rsplit("/", 1)[0]); spec.loader.exec_module(m)
    sys.path.pop(0)
    return m


A = load(sys.argv[1], "ex_ref"); B = load(sys.argv[2], "ex_new")
MS_A = load(sys.argv[1].rsplit("/", 1)[0] + "/manifest_space.py", "ms_ref")
MS_B = load(sys.argv[2].rsplit("/", 1)[0] + "/manifest_space.py", "ms_new")
GLOBALS = ("SVCS", "NET_SVCS", "PAUSE_SVCS", "NORDERS", "KNOBS", "KNOB_TOKENS", "SPACE", "FC_PAIRS", "FC_MATCH", "PF_TARGETS")
SAVED = {M: {k: getattr(M, k) for k in GLOBALS if hasattr(M, k)} for M in (A, B)}


def reset(M):
    for k, v in SAVED[M].items():
        setattr(M, k, v)


def compose(M, strat, sd, dur, pf):
    s = M.GENERATORS[strat](sd)
    if dur: s = M.durable_only(s)
    if strat not in ("swarm-pos", "process"): s = M.add_new_faults(s, sd, strat == "swarm")
    s = M.add_freeze_call(M.add_more_faults(s, sd, strat), sd)
    if pf: s = M.add_protocol_faults(s, sd)
    return M.render(M.add_buggify(M.add_preempt(M.add_concurrency(M.add_timing(s, sd), sd), sd), sd))


def run(label, strats=("random", "swarm", "swarm-pos", "process"), seeds=range(1, 151)):
    n = nh = 0
    for strat in strats:
        for flags in itertools.product((False, True), repeat=8):
            nf, tm, cc, pe, bg, dur, mf, fc = flags
            for M in (A, B):
                M.NEW_FAULTS, M.TIMING, M.CONCURRENCY, M.PREEMPT, M.BUGGIFY, M.MORE_FAULTS = nf, tm, cc, pe, bg, mf
                M.FREEZE_CALL = fc
                M.CF_RANGE = (1, 300)
            B.PROTOCOL_FAULTS = False
            a = [compose(A, strat, sd, dur, False) for sd in seeds]
            b = [compose(B, strat, sd, dur, False) for sd in seeds]
            assert a == b, (label, strat, flags)
            for rule in ("cut", "any"):
                A.KNOB_RULE = B.KNOB_RULE = rule
                assert [A.finding_class(A.parse(x)) for x in a] == [B.finding_class(B.parse(x)) for x in b], (label, strat, flags, rule)
            A.KNOB_RULE = B.KNOB_RULE = "cut"
            assert [[A.render(c) for c in A.candidates(A.parse(x))] for x in a[:30]] == \
                   [[B.render(c) for c in B.candidates(B.parse(x))] for x in b[:30]], (label, strat, flags)
            n += len(a)
            if B.PF_TARGETS:                                  # (3) with --protocol-faults
                B.PROTOCOL_FAULTS = True
                c = [compose(B, strat, sd, dur, True) for sd in seeds]
                kinds = {t: rk + hk for t, rk, hk, _ in B.PF_TARGETS}
                cnt = 0
                for x, y in zip(b, c):
                    p, q = B.parse(x), B.parse(y)
                    assert B.render(q) == y, y
                    assert all(p.get(k) == q.get(k) for k in set(p) | set(q) if k != "H"), (x, y)
                    for h in q.get("H", []):
                        cnt += 1
                        assert h[0] in kinds and 0 <= h[1] < h[2] <= B.NORDERS, y
                        k0 = h[3][0][0]
                        assert k0 in kinds[h[0]], (k0, y)
                    for cand in B.candidates(q):
                        z = B.render(cand)
                        assert B.render(B.parse(z)) == z, z
                assert 0 < cnt, (label, strat)
                nh += cnt
                B.PROTOCOL_FAULTS = False
    return n, nh


n1, _ = run("no manifest")
reset(A); reset(B)
MS_A.load(manifest=sys.argv[4]).apply(vars(A))
MS_B.load(manifest=sys.argv[3]).apply(vars(B))
assert B.PF_TARGETS, "the manifest with fakes gives no protocol fault targets"
# swarm-pos needs at least 15 steps (pos_start's "late" range is empty below that, in main too)
strats = ("random", "swarm", "swarm-pos", "process") if B.NORDERS >= 15 else ("random", "swarm", "process")
n2, nh = run("manifest", strats)
print("identical without --protocol-faults: %d schedule strings without a manifest, %d on %s "
      "(%d generators x 256 stream combinations; classes under both knob rules; shrink candidates); "
      "with --protocol-faults only H= differs (%d H= tokens drawn); targets %s"
      % (n1, n2, sys.argv[3], len(strats), nh, [t for t, _, _, _ in B.PF_TARGETS]))
