# Identity check for --grpc-faults (gRPC-level faults, grpc_faults.py). usage:
#   python3 grpc_faults_ident.py <reference explore.py> <new explore.py> <hydo.yaml with a gRPC proxy>
# The reference is an explore.py without grpc_faults (the fakes-protocol-faults branch, which has
# --protocol-faults; e.g. `git archive fakes-protocol-faults hvx | tar -x -C ref`).
# (1) no manifest, and (2) on the manifest: without --grpc-faults the new explore.py gives
#     byte-identical schedule strings, classes (both knob rules) and shrink candidates for every
#     generator, composed as explore.py main does, with every other random stream on or off
#     (new faults, timing, concurrency, preempt, buggify, durable-only, more faults, freeze-call,
#     protocol faults);
# (3) with --grpc-faults every field other than H equals the schedule without it, the H= tokens
#     without it are a prefix of the ones with it, the strings round-trip, every added token names
#     a gRPC target of the manifest, and the shrink candidates render and stay parseable.
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


def compose(M, strat, sd, dur, pf, gf):
    s = M.GENERATORS[strat](sd)
    if dur: s = M.durable_only(s)
    if strat not in ("swarm-pos", "process"): s = M.add_new_faults(s, sd, strat == "swarm")
    s = M.add_freeze_call(M.add_more_faults(s, sd, strat), sd)
    s = M.add_protocol_faults(s, sd)
    if gf: s = M.add_grpc_faults(s, sd)
    return M.render(M.add_buggify(M.add_preempt(M.add_concurrency(M.add_timing(s, sd), sd), sd), sd))


def run(label, strats=("random", "swarm", "swarm-pos", "process"), seeds=range(1, 61)):
    n = ng = 0
    names = {t for t, _ in B.grpc_faults.TARGETS}
    for strat in strats:
        for flags in itertools.product((False, True), repeat=9):
            nf, tm, cc, pe, bg, dur, mf, fc, pf = flags
            for M in (A, B):
                M.NEW_FAULTS, M.TIMING, M.CONCURRENCY, M.PREEMPT, M.BUGGIFY, M.MORE_FAULTS = nf, tm, cc, pe, bg, mf
                M.FREEZE_CALL, M.PROTOCOL_FAULTS = fc, pf and bool(M.PF_TARGETS)
                M.CF_RANGE = (1, 300)
            B.grpc_faults.ON = False
            a = [compose(A, strat, sd, dur, pf, False) for sd in seeds]
            b = [compose(B, strat, sd, dur, pf, True) for sd in seeds]     # the hook runs, inert
            assert a == b, (label, strat, flags)
            for rule in ("cut", "any"):
                A.KNOB_RULE = B.KNOB_RULE = rule
                assert [A.finding_class(A.parse(x)) for x in a] == [B.finding_class(B.parse(x)) for x in b], (label, strat, flags, rule)
            A.KNOB_RULE = B.KNOB_RULE = "cut"
            assert [[A.render(c) for c in A.candidates(A.parse(x))] for x in a[:25]] == \
                   [[B.render(c) for c in B.candidates(B.parse(x))] for x in b[:25]], (label, strat, flags)
            n += len(a)
            if names:                                        # (3) with --grpc-faults
                B.grpc_faults.ON = True
                c = [compose(B, strat, sd, dur, pf, True) for sd in seeds]
                B.grpc_faults.ON = False
                cnt = 0
                for x, y in zip(b, c):
                    p, q = B.parse(x), B.parse(y)
                    assert B.render(q) == y, y
                    assert all(p.get(k) == q.get(k) for k in set(p) | set(q) if k != "H"), (x, y)
                    hp, hq = p.get("H", []), q.get("H", [])
                    assert hq[:len(hp)] == hp, (x, y)
                    for h in hq[len(hp):]:
                        cnt += 1
                        assert h[0] in names and 0 <= h[1] < h[2] <= B.NORDERS, y
                    for cand in B.candidates(q):
                        z = B.render(cand)
                        assert B.render(B.parse(z)) == z, z
                assert 0 < cnt, (label, strat)
                ng += cnt
    return n, ng


n1, _ = run("no manifest", seeds=range(1, 41))
reset(A); reset(B)
MS_A.load(manifest=sys.argv[3]).apply(vars(A))
MS_B.load(manifest=sys.argv[3]).apply(vars(B))
B.grpc_faults.TARGETS = B.grpc_faults.targets_of(B.SPACE.m)
assert B.grpc_faults.TARGETS, "the manifest gives no gRPC targets"
strats = ("random", "swarm", "swarm-pos", "process") if B.NORDERS >= 15 else ("random", "swarm", "process")
n2, ng = run("manifest", strats)
print("identical without --grpc-faults: %d schedule strings without a manifest, %d on %s "
      "(%d generators x 512 stream combinations, --protocol-faults on and off; classes under both knob "
      "rules; shrink candidates); with --grpc-faults only H= differs, by appended tokens (%d drawn); targets %s"
      % (n1, n2, sys.argv[3], len(strats), ng, [t for t, _ in B.grpc_faults.TARGETS]))
